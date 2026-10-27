//! Secret redaction used by every error, `Debug` and diagnostic in this crate.
//!
//! Redaction is conservative by construction: whenever the scrubber cannot be
//! sure where a secret ends it hides more, never less. A false positive hides
//! a harmless word; a false negative leaks a credential. [`redact_text`]
//! recognizes
//!
//! - AWS access-key IDs (`AKIA...`, `ASIA...` and the other 20-character IAM
//!   prefixes) and long base64 session tokens (`IQoJ...`, `FwoG...`);
//! - `key = value` / `key: value` / JSON `"key": "value"` pairs whose key
//!   contains a secret label (`aws_secret_access_key`, `session_token`,
//!   `authorization`, `x-amz-signature`, `password`, ...), unquoted, single or
//!   double quoted, plain or JSON-escaped;
//! - credentials after an authentication scheme (`Bearer`, `Basic`, `Digest`,
//!   `AWS4-HMAC-SHA256`, `token`, ...) whatever whitespace separates them;
//! - userinfo in every embedded `scheme://` URL.
//!
//! A bare 40-character secret key with no label cannot be told apart from an
//! identifier and is *not* recognized; the crate therefore never formats a
//! secret value in the first place (see [`Secret`]).

use std::fmt;

/// Replacement text for every redacted value.
pub const REDACTED: &str = "[REDACTED]";

/// A string whose value must never be printed or logged.
///
/// `Debug` shows a fixed marker. There is no `Display`, no serialization and
/// no accessor other than the explicit [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value; call only when handing it to a signer or SDK.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the wrapped value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// Labels (lowercase) whose value is a secret. A label matches anywhere in a
/// key (`my_token=`, `nextToken:`, `x-amz-security-token:` all count) as long
/// as a `=`, `:` or `=>` separator follows it.
const SECRET_KEYS: &[&str] = &[
    "aws_secret_access_key",
    "aws_session_token",
    "secret_access_key",
    "secretaccesskey",
    "session_token",
    "sessiontoken",
    "security_token",
    "x-amz-security-token",
    "x-amz-signature",
    "x-amz-credential",
    "access_token",
    "accesstoken",
    "refresh_token",
    "refreshtoken",
    "id_token",
    "client_secret",
    "clientsecret",
    "private_key",
    "privatekey",
    "passphrase",
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "x-api-key",
    "credential",
    "signature",
];

/// Labels (lowercase) whose value is a whole header line (may contain spaces).
const LINE_KEYS: &[&str] = &[
    "proxy-authorization",
    "authorization",
    "set-cookie",
    "cookie",
];

/// An authentication scheme whose credential follows after whitespace.
struct AuthScheme {
    /// Lowercase scheme name, matched case-insensitively as a whole word.
    name: &'static str,
    /// The credential is a parameter list (`k="v", ...`): hide to end of line.
    params: bool,
    /// The name is also an English word: a following short lowercase word
    /// (`token included`, `digest mismatch`) is prose, not a credential.
    prose_word: bool,
}

const fn scheme(name: &'static str, params: bool, prose_word: bool) -> AuthScheme {
    AuthScheme {
        name,
        params,
        prose_word,
    }
}

/// Authentication schemes (RFC 9110 registry plus AWS and common token forms).
const AUTH_SCHEMES: &[AuthScheme] = &[
    scheme("bearer", false, false),
    scheme("basic", false, false),
    scheme("ntlm", false, false),
    scheme("dpop", false, false),
    scheme("gnap", false, false),
    scheme("negotiate", false, true),
    scheme("token", false, true),
    scheme("aws4-hmac-sha256", true, false),
    scheme("aws4-ecdsa-p256-sha256", true, false),
    scheme("privatetoken", true, false),
    scheme("scram-sha-256", true, false),
    scheme("scram-sha-1", true, false),
    scheme("vapid", true, false),
    scheme("hoba", true, false),
    scheme("digest", true, true),
    scheme("oauth", true, true),
];

/// Four-character prefixes of 20-character IAM access-key and unique IDs.
const KEY_ID_PREFIXES: &[&str] = &[
    "AKIA", "ASIA", "AGPA", "AIDA", "AIPA", "ANPA", "ANVA", "AROA", "ABIA", "ACCA",
];

/// Base64 prefixes of STS session tokens.
const TOKEN_PREFIXES: &[&str] = &["IQoJ", "FwoG"];

/// Minimum length of a run recognized as an STS session token.
const MIN_SESSION_TOKEN_LEN: usize = 100;

/// Passes after which a still-changing input is hidden entirely.
const MAX_PASSES: usize = 8;

/// The one diagnostic scrubber: replace credential-looking content in `input`
/// with [`REDACTED`].
///
/// Every error `Display`, `Debug`, conversion and rendered failure in this
/// crate goes through this function. One pass collects every span that any
/// recognizer wants hidden (URL userinfo, labelled values, scheme credentials,
/// key IDs and session tokens), all on the same input, and replaces their
/// union. Passes repeat until the text no longer changes, so the result is
/// idempotent by construction: `redact_text(&redact_text(x)) == redact_text(x)`.
/// When the end of a value is uncertain the redaction extends to the end of
/// the input.
#[must_use]
pub fn redact_text(input: &str) -> String {
    let mut current = scrub_once(input);
    for _ in 0..MAX_PASSES {
        let next = scrub_once(&current);
        if next == current {
            return current;
        }
        current = next;
    }
    REDACTED.to_string()
}

/// One pass: hide the union of every recognizer's spans.
fn scrub_once(input: &str) -> String {
    let mut spans = url_userinfo_spans(input);
    spans.extend(labelled_spans(input));
    spans.sort_unstable();
    let mut out = String::with_capacity(input.len());
    let mut copied = 0;
    let mut index = 0;
    while index < spans.len() {
        let (from, mut to) = spans[index];
        index += 1;
        // Merge overlapping and touching spans into one marker.
        while index < spans.len() && spans[index].0 <= to {
            to = to.max(spans[index].1);
            index += 1;
        }
        let from = from.max(copied);
        if to > from {
            out.push_str(&input[copied..from]);
            out.push_str(REDACTED);
            copied = to;
        }
    }
    out.push_str(&input[copied..]);
    out
}

/// Userinfo spans of every `scheme://` URL in `input`, plain or with escaped
/// slashes at any depth (`:\/\/` in JSON, `:\\/\\/` once more escaped).
///
/// The authority runs to the first `/`, `?`, `#` or whitespace. Everything
/// before its *last* `@` is userinfo, whatever it contains (quotes,
/// apostrophes, backslashes, more `@`). An `@` in the path, query or fragment
/// is not userinfo.
fn url_userinfo_spans(input: &str) -> Vec<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut spans = Vec::new();
    for (colon, _) in input.match_indices(':') {
        let Some(authority) = after_double_slash(bytes, colon + 1) else {
            continue;
        };
        let tail = &input[authority..];
        let authority_len = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
            .unwrap_or(tail.len());
        if let Some(at) = tail[..authority_len].rfind('@') {
            if at > 0 {
                spans.push((authority, authority + at));
            }
        }
    }
    spans
}

/// Index after `//` at `p`, each slash optionally preceded by escaping
/// backslashes.
fn after_double_slash(bytes: &[u8], mut p: usize) -> Option<usize> {
    for _ in 0..2 {
        let (_, after) = skip_backslashes(bytes, p);
        if bytes.get(after) != Some(&b'/') {
            return None;
        }
        p = after + 1;
    }
    Some(p)
}

/// Spans of labelled values, scheme credentials, key IDs and session tokens.
fn labelled_spans(input: &str) -> Vec<(usize, usize)> {
    let bytes = input.as_bytes();
    let lower = input.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match find_span(input, lower, i) {
            Some((from, to)) => {
                spans.push((from, to));
                i = to.max(i + 1);
            }
            None => i += 1,
        }
    }
    spans
}

/// Redact a URL. Kept for readability at URL call sites; it is exactly
/// [`redact_text`], which already strips userinfo and secret query values.
#[must_use]
pub fn redact_url(url: &str) -> String {
    redact_text(url)
}

/// Redact, flatten control characters and truncate to `max_chars` characters.
/// Used for echoing untrusted values (a bad region, a response body) in errors.
///
/// The value is scrubbed before it is flattened and truncated, and the result
/// is scrubbed again, so neither step can expose or half-cut a secret and the
/// output is stable under [`redact_text`].
#[must_use]
pub fn snippet(value: &str, max_chars: usize) -> String {
    let redacted = redact_text(value);
    let mut out: String = redacted
        .chars()
        .take(max_chars)
        .map(|c| match c {
            '\t' | '\n' | '\r' | '\u{b}' | '\u{c}' => ' ',
            c if c.is_control() => '?',
            c => c,
        })
        .collect();
    if redacted.chars().count() > max_chars {
        out.push('…');
    }
    redact_text(&out)
}

/// Replace id-like path segments so a request path is safe to show: strips the
/// query and fragment and turns every run of 16 or more `[A-Za-z0-9_-]`
/// characters (UUIDs, resource ids) into `<id>`.
#[must_use]
pub fn sanitize_path(path: &str) -> String {
    fn flush(out: &mut String, run: &mut String) {
        if run.len() >= 16 {
            out.push_str("<id>");
        } else {
            out.push_str(run);
        }
        run.clear();
    }

    let base = path.split(['?', '#']).next().unwrap_or("");
    let mut out = String::with_capacity(base.len());
    let mut run = String::new();
    for c in base.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            run.push(c);
        } else {
            flush(&mut out, &mut run);
            out.push(c);
        }
    }
    flush(&mut out, &mut run);
    out
}

/// Whether an HTTP header (any case) carries a secret.
#[must_use]
pub fn is_sensitive_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "authorization"
            | "proxy-authorization"
            | "x-amz-security-token"
            | "x-amz-signature"
            | "x-amz-credential"
            | "x-api-key"
            | "cookie"
            | "set-cookie"
    )
}

/// Whether a parameter or field *name* denotes a secret value (the value must
/// then never be shown, whatever it looks like).
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "api-key",
        "signature",
        "authorization",
        "cookie",
        "credential",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Whether the *value* of an HTTP header must be hidden entirely, judging by
/// its name: the exact sensitive set of [`is_sensitive_header`], any
/// [`is_secret_name`] marker, and the broader markers `key`, `auth`, `session`,
/// `pass`, `bearer`, `private` and `cred`. Hiding a harmless header such as
/// `x-amz-server-side-encryption-aws-kms-key-id` is the accepted cost.
#[must_use]
pub fn hides_header_value(name: &str) -> bool {
    if is_sensitive_header(name) || is_secret_name(name) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    [
        "key", "auth", "session", "pass", "bearer", "private", "cred",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Formats a string through [`redact_text`] in both `Display` and `Debug`
/// (the latter quoted like a `str`). Used by every hand-written `Debug` that
/// carries a raw string.
pub(crate) struct Scrubbed<'a>(pub(crate) &'a str);

impl fmt::Debug for Scrubbed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&redact_text(self.0), f)
    }
}

impl fmt::Display for Scrubbed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_text(self.0))
    }
}

/// Adapts a formatting closure to `Display` and `Debug`.
struct FmtFn<F>(F);

impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for FmtFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}

impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Debug for FmtFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}

/// Render a `Debug` body (honouring `{:#?}`) and write the *whole* printed
/// text through [`redact_text`]. Field-level scrubbing happens before Rust's
/// escaping; this second layer scrubs what is actually printed, so escaping
/// cannot turn a scrubbed field into a leak and the output is idempotent.
pub(crate) fn write_scrubbed_debug(
    f: &mut fmt::Formatter<'_>,
    body: impl Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
) -> fmt::Result {
    let body = FmtFn(body);
    let text = if f.alternate() {
        format!("{body:#?}")
    } else {
        format!("{body:?}")
    };
    f.write_str(&redact_text(&text))
}

/// Render a `Display` body and write the whole text through [`redact_text`].
pub(crate) fn write_scrubbed_display(
    f: &mut fmt::Formatter<'_>,
    body: impl Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
) -> fmt::Result {
    f.write_str(&redact_text(&FmtFn(body).to_string()))
}

fn is_word_boundary(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    // A JSON escape such as `\n` directly before the word ends the previous word.
    if i >= 2 && bytes[i - 2] == b'\\' && matches!(bytes[i - 1], b'n' | b'r' | b't') {
        return true;
    }
    !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' || bytes[i - 1] == b'-')
}

/// Bytes that end an unquoted labelled value or a scheme credential.
fn is_value_end(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'&' | b',' | b';' | b'"' | b'\'' | b')' | b'}')
}

/// Bytes that may begin a scheme credential (token68 and common token forms).
fn is_credential_start(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
}

fn is_base64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'=')
}

/// Span `[from, to)` of `input` to redact when a secret starts at `i`.
fn find_span(input: &str, lower: &[u8], i: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    if !bytes[i].is_ascii_alphanumeric() {
        return None;
    }
    if let Some(span) = identifier_span(bytes, i) {
        return Some(span);
    }
    for key in LINE_KEYS {
        if lower[i..].starts_with(key.as_bytes()) {
            if let Some(span) = value_span(input, i + key.len(), true) {
                return Some(span);
            }
        }
    }
    for key in SECRET_KEYS {
        if lower[i..].starts_with(key.as_bytes()) {
            if let Some(span) = value_span(input, i + key.len(), false) {
                return Some(span);
            }
        }
    }
    scheme_span(input, lower, i)
}

/// Credential after an authentication scheme starting at `i`.
///
/// Any run of whitespace (Unicode, newlines and `\n`/`\t` escapes included)
/// separates the scheme from its credential. A token credential runs to the
/// next delimiter; a parameter-list credential runs to the end of the line; a
/// quoted credential follows the quoted-value rules.
fn scheme_span(input: &str, lower: &[u8], i: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    if !is_word_boundary(bytes, i) {
        return None;
    }
    for auth in AUTH_SCHEMES {
        if !lower[i..].starts_with(auth.name.as_bytes()) {
            continue;
        }
        let after = i + auth.name.len();
        let start = skip_ws(input, after);
        if start == after || start >= bytes.len() {
            continue;
        }
        if let Some((depth, open)) = quote_at(bytes, start) {
            let from = open + 1;
            let end = quoted_value_end(input, from, bytes[open], depth);
            if end > from {
                return Some((from, end));
            }
            continue;
        }
        if !is_credential_start(bytes[start]) {
            continue;
        }
        if auth.prose_word && is_prose_word(bytes, start) {
            continue;
        }
        let end = if auth.params {
            scan_until(bytes, start, |b| b == b'\n' || b == b'\r')
        } else {
            scan_until(bytes, start, is_value_end)
        };
        return Some((start, end));
    }
    None
}

/// A short all-lowercase word followed by whitespace, punctuation or the end.
fn is_prose_word(bytes: &[u8], start: usize) -> bool {
    let end = scan_until(bytes, start, |b| !b.is_ascii_lowercase());
    end > start
        && end - start < 16
        && (end == bytes.len()
            || bytes[end].is_ascii_whitespace()
            || matches!(bytes[end], b'.' | b',' | b')' | b'!' | b'?'))
}

/// Access-key IDs and STS session tokens, recognized by shape alone.
fn identifier_span(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    let alnum_end = scan_until(bytes, i, |b| !b.is_ascii_alphanumeric());
    let run = &bytes[i..alnum_end];
    if run.len() == 20
        && run
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && KEY_ID_PREFIXES
            .iter()
            .any(|prefix| run.starts_with(prefix.as_bytes()))
    {
        return Some((i, alnum_end));
    }
    if TOKEN_PREFIXES
        .iter()
        .any(|prefix| bytes[i..].starts_with(prefix.as_bytes()))
    {
        let end = scan_until(bytes, i, |b| !is_base64(b));
        if end - i >= MIN_SESSION_TOKEN_LEN {
            return Some((i, end));
        }
    }
    None
}

/// Skip whitespace (Unicode, newlines included) and escaped whitespace at any
/// escape depth: `\n`, `\r`, `\t`, JSON ` ` and Rust `\u{2003}` forms,
/// also with doubled backslashes (`\\t`). `p` must be a char boundary.
fn skip_ws(input: &str, mut p: usize) -> usize {
    let bytes = input.as_bytes();
    loop {
        if bytes.get(p) == Some(&b'\\') {
            let (_, after) = skip_backslashes(bytes, p);
            let Some(next) = escaped_whitespace_end(input, after) else {
                return p;
            };
            p = next;
            continue;
        }
        match input[p..].chars().next() {
            Some(c) if c.is_whitespace() => p += c.len_utf8(),
            _ => return p,
        }
    }
}

/// End of an escape body at `p` (just after its backslashes) that encodes
/// whitespace: `n`, `r`, `t`, `uXXXX` or `u{X..}` naming a whitespace char.
fn escaped_whitespace_end(input: &str, p: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    match *bytes.get(p)? {
        b'n' | b'r' | b't' => Some(p + 1),
        b'u' => {
            let (digits, end) = if bytes.get(p + 1) == Some(&b'{') {
                let close = p + 2 + input.get(p + 2..)?.find('}')?;
                (input.get(p + 2..close)?, close + 1)
            } else {
                (input.get(p + 1..p + 5)?, p + 5)
            };
            if digits.is_empty()
                || digits.len() > 6
                || !digits.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return None;
            }
            let code = u32::from_str_radix(digits, 16).ok()?;
            char::from_u32(code)
                .filter(|c| c.is_whitespace())
                .map(|_| end)
        }
        _ => None,
    }
}

/// Number of consecutive backslashes at `p`, and the index after them.
fn skip_backslashes(bytes: &[u8], p: usize) -> (usize, usize) {
    let mut q = p;
    while q < bytes.len() && bytes[q] == b'\\' {
        q += 1;
    }
    (q - p, q)
}

fn is_quote(byte: u8) -> bool {
    matches!(byte, b'"' | b'\'')
}

/// If a quote, possibly written as an escaped quote (`\"`), starts at `at`:
/// its escape depth (number of backslashes) and the index of the quote itself.
fn quote_at(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let (depth, index) = skip_backslashes(bytes, at);
    (index < bytes.len() && is_quote(bytes[index])).then_some((depth, index))
}

/// Value span after a key that ended at `after_key`.
///
/// Accepts `key=value`, `key: value`, `"key": "value"`, `key => value` and the
/// same inside escaped JSON (`\"key\":\"value\"`), with any whitespace
/// (including newlines) around the separator. Once a label and separator are
/// found, a value that is not cleanly terminated is hidden to the end of the
/// input.
fn value_span(input: &str, after_key: usize, whole_line: bool) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut pos = skip_ws(input, after_key);
    if let Some((_, key_quote)) = quote_at(bytes, pos) {
        pos = skip_ws(input, key_quote + 1);
    }
    if pos >= bytes.len() || !matches!(bytes[pos], b'=' | b':') {
        return None;
    }
    pos += 1;
    if bytes.get(pos) == Some(&b'>') {
        pos += 1;
    }
    let mut value_at = skip_ws(input, pos);
    if whole_line && quote_at(bytes, value_at).is_none() {
        // An empty header value must not swallow the next header line; only a
        // quoted value may start on a later line.
        value_at = pos;
        while bytes
            .get(value_at)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            value_at += 1;
        }
    }
    if value_at >= bytes.len() {
        return None;
    }
    if let Some((depth, open)) = quote_at(bytes, value_at) {
        let start = open + 1;
        let end = quoted_value_end(input, start, bytes[open], depth);
        return (end > start).then_some((start, end));
    }
    if whole_line {
        let end = scan_until(bytes, value_at, |b| b == b'\n' || b == b'\r');
        return (end > value_at).then_some((value_at, end));
    }
    unquoted_value_span(input, value_at)
}

/// An unquoted value: a token ending at whitespace, `&`, `,`, `;`, `)`, `}` or
/// the end (not `]`, which ends the `[REDACTED]` marker itself and would stop
/// redaction converging). A value cut short by a quote (plain or escaped) is trusted
/// only when that quote closes cleanly; otherwise it is hidden to the end.
fn unquoted_value_span(input: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut end = start;
    while end < bytes.len() {
        if is_value_end(bytes[end]) {
            break;
        }
        if bytes[end] == b'\\' && quote_at(bytes, end).is_some() {
            break;
        }
        end += 1;
    }
    if end == start {
        return None;
    }
    if let Some((depth, quote_index)) = quote_at(bytes, end) {
        if !closes_cleanly(input, quote_index + 1, bytes[quote_index], depth) {
            return Some((start, bytes.len()));
        }
    }
    Some((start, end))
}

/// End (exclusive) of a quoted value that starts at `start`.
///
/// `escape_depth` is the number of backslashes before the opening quote: 0 for
/// plain JSON, 1 for JSON embedded in a string (`\"`), and so on. In plain
/// text a quote preceded by an odd number of backslashes is escaped; in an
/// escaped context only a quote preceded by exactly `escape_depth` backslashes
/// closes the value. An unterminated value, or a closing quote that is not
/// followed by a structural delimiter ([`closes_cleanly`]), makes the end
/// uncertain, so the span then runs to the end of the input.
fn quoted_value_end(input: &str, start: usize, quote: u8, escape_depth: usize) -> usize {
    let bytes = input.as_bytes();
    let mut p = start;
    while p < bytes.len() {
        if bytes[p] == b'\\' {
            let run_start = p;
            let (run, after_run) = skip_backslashes(bytes, p);
            p = after_run;
            if p < bytes.len() && bytes[p] == quote {
                let closes = if escape_depth == 0 {
                    run % 2 == 0
                } else {
                    run == escape_depth
                };
                if closes {
                    let end = if escape_depth == 0 { p } else { run_start };
                    return if closes_cleanly(input, p + 1, quote, escape_depth) {
                        end
                    } else {
                        bytes.len()
                    };
                }
                p += 1;
            }
        } else if bytes[p] == quote && escape_depth == 0 {
            return if closes_cleanly(input, p + 1, quote, 0) {
                p
            } else {
                bytes.len()
            };
        } else {
            p += 1;
        }
    }
    bytes.len()
}

/// Whether a value whose closing `quote` (at `escape_depth`) ends just before
/// `p` is cleanly terminated: after optional whitespace comes `,`, `}`, `]`,
/// `;`, `)` (as in a `Debug` `Some("...")`) or the end of the input, possibly
/// after the quotes that close enclosing strings (an escaped quote of lower
/// depth, or at depth 0 a quote of the other kind). Anything else is glued
/// text and makes the end uncertain.
fn closes_cleanly(input: &str, mut p: usize, quote: u8, escape_depth: usize) -> bool {
    let bytes = input.as_bytes();
    let mut depth = escape_depth;
    let mut quote = quote;
    loop {
        p = skip_ws(input, p);
        if p >= bytes.len() || matches!(bytes[p], b',' | b'}' | b']' | b';' | b')') {
            return true;
        }
        let (run, after) = skip_backslashes(bytes, p);
        if after < bytes.len() && is_quote(bytes[after]) {
            let encloses = if depth > 0 {
                run < depth
            } else {
                run == 0 && bytes[after] != quote
            };
            if encloses {
                depth = run;
                quote = bytes[after];
                p = after + 1;
                continue;
            }
        }
        return false;
    }
}

fn scan_until(bytes: &[u8], start: usize, stop: impl Fn(u8) -> bool) -> usize {
    let mut p = start;
    while p < bytes.len() && !stop(bytes[p]) {
        p += 1;
    }
    p
}

/// Adversarial diagnostics shared by the wrapper tests of every module (all
/// secrets synthetic): `(input, exact redact_text output, substrings that
/// must never survive any diagnostic path)`.
#[cfg(test)]
pub(crate) const ADVERSARIAL: &[(&str, &str, &[&str])] = &[
    // Review #4467 finding 1: an apostrophe in URL userinfo, plain and in
    // the JSON-escaped `:\/\/` form.
    (
        "dial https://FAKEUSR1:pa'FAKEPW1@logs.amazonaws.com/ failed",
        "dial https://[REDACTED]@logs.amazonaws.com/ failed",
        &["FAKEUSR1", "FAKEPW1"],
    ),
    (
        r#"{"url":"https:\/\/FAKEUSR2:pa'FAKEPW2@logs.amazonaws.com\/x"}"#,
        r#"{"url":"https:\/\/[REDACTED]@logs.amazonaws.com\/x"}"#,
        &["FAKEUSR2", "FAKEPW2"],
    ),
    // Finding 2: glued text after the closing quote, then a whitespace
    // continuation.
    (
        r#"{"password":"aa"tail" LEAK-REST"}"#,
        r#"{"password":"[REDACTED]"#,
        &["\"tail", "LEAK-REST"],
    ),
    // Finding 3 (#4494): several spaces after Bearer and Basic.
    (
        "Bearer  FAKE-BEARER-TWO-SPACES",
        "Bearer  [REDACTED]",
        &["FAKE-BEARER-TWO-SPACES"],
    ),
    (
        "Basic  RkFLRTpNVUxUSVNQQUNF",
        "Basic  [REDACTED]",
        &["RkFLRTpNVUxUSVNQQUNF"],
    ),
    // Quotes, backslashes and several `@` in userinfo.
    (
        r#"https://FAKEUSR5:"FAKEPW5"@h.example/p"#,
        "https://[REDACTED]@h.example/p",
        &["FAKEUSR5", "FAKEPW5"],
    ),
    (
        r#"https://FAKEUSR6:a\b\"FAKEPW6@h.example"#,
        "https://[REDACTED]@h.example",
        &["FAKEUSR6", "FAKEPW6"],
    ),
    (
        "ftp://FAKEUSR7@mid:FAKEPW7@h.example:21/",
        "ftp://[REDACTED]@h.example:21/",
        &["FAKEUSR7", "FAKEPW7"],
    ),
    (
        "see 'https://FAKEUSR8:FAKEPW8@h.example' now",
        "see 'https://[REDACTED]@h.example' now",
        &["FAKEUSR8", "FAKEPW8"],
    ),
    // No `/` ends the authority, so everything up to the last `@` goes.
    (
        r#"{"u":"https://h.example","mail":"a@b.example"}"#,
        r#"{"u":"https://[REDACTED]@b.example"}"#,
        &[],
    ),
    // Schemes: any case, tabs, mixed runs, newlines, `\t` escapes, Unicode
    // spaces and quoted credentials.
    (
        "sent bearer\t\tFAKEBEAR9, done",
        "sent bearer\t\t[REDACTED], done",
        &["FAKEBEAR9"],
    ),
    (
        "BASIC   FAKEBASIC10",
        "BASIC   [REDACTED]",
        &["FAKEBASIC10"],
    ),
    (
        "Bearer \t  FAKEBEAR35",
        "Bearer \t  [REDACTED]",
        &["FAKEBEAR35"],
    ),
    (
        "Bearer\n  FAKEBEAR14",
        "Bearer\n  [REDACTED]",
        &["FAKEBEAR14"],
    ),
    (
        r"Bearer\tFAKEBEAR15",
        r"Bearer\t[REDACTED]",
        &["FAKEBEAR15"],
    ),
    (
        "Bearer \"FAKEBEAR16\"",
        "Bearer \"[REDACTED]\"",
        &["FAKEBEAR16"],
    ),
    (
        "Token \u{2003}ghp_FAKETOKEN13 used",
        "Token \u{2003}[REDACTED] used",
        &["FAKETOKEN13"],
    ),
    (
        "Digest username=\"bob\", response=\"FAKEDIGEST11\"\nnext: 1",
        "Digest [REDACTED]\nnext: 1",
        &["FAKEDIGEST11"],
    ),
    (
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/us-east-1/s3/aws4_request, \
         SignedHeaders=host, Signature=FAKESIG12",
        "AWS4-HMAC-SHA256 [REDACTED]",
        &["AKIAIOSFODNN7EXAMPLE", "FAKESIG12"],
    ),
    (
        r#"{"msg":"auth failed: Basic  RkFLRTpGQUtFMzY="}"#,
        r#"{"msg":"auth failed: Basic  [REDACTED]"}"#,
        &["RkFLRTpGQUtFMzY="],
    ),
    // Glued tails with comma, semicolon, whitespace and newline continuations.
    (
        r#"{"password":"aa"FAKEG17 , FAKEG17B"}"#,
        r#"{"password":"[REDACTED]"#,
        &["FAKEG17"],
    ),
    (
        r#"{"password":"aa"FAKEG28; FAKEG28B"}"#,
        r#"{"password":"[REDACTED]"#,
        &["FAKEG28"],
    ),
    (
        r#"password="aa" FAKEG18; rest"#,
        r#"password="[REDACTED]"#,
        &["FAKEG18"],
    ),
    (
        "{\"password\":\"aa\"\nFAKEG25\"}",
        "{\"password\":\"[REDACTED]",
        &["FAKEG25"],
    ),
    (
        r#"token=abc"FAKEG21 tail"#,
        "token=[REDACTED]",
        &["FAKEG21"],
    ),
    ("token=abc'FAKEG22", "token=[REDACTED]", &["FAKEG22"]),
    (
        r#"{\"password\":\"aa\"FAKEG23\"}"#,
        r#"{\"password\":\"[REDACTED]"#,
        &["FAKEG23"],
    ),
    (
        r#"{\"token\":\"aa\" FAKEG24\"}"#,
        r#"{\"token\":\"[REDACTED]"#,
        &["FAKEG24"],
    ),
    (
        "password: it's FAKEAPOS29",
        "password: [REDACTED]",
        &["FAKEAPOS29"],
    ),
    // Cleanly terminated values keep the text around them.
    (
        "{\"password\": \"FAKEV26\" ,\n \"user\": \"bob\"}",
        "{\"password\": \"[REDACTED]\" ,\n \"user\": \"bob\"}",
        &["FAKEV26"],
    ),
    (
        "token='FAKEV33',next=1",
        "token='[REDACTED]',next=1",
        &["FAKEV33"],
    ),
    (
        "token=FAKEM31&password=FAKEM32",
        "token=[REDACTED]&password=[REDACTED]",
        &["FAKEM31", "FAKEM32"],
    ),
    (
        r"password=FAKEBS30\\x rest",
        "password=[REDACTED] rest",
        &["FAKEBS30"],
    ),
    (
        "x-amz-security-token:\tFAKESTS34",
        "x-amz-security-token:\t[REDACTED]",
        &["FAKESTS34"],
    ),
    (
        "retry nextToken=FAKENEXT36 later",
        "retry nextToken=[REDACTED] later",
        &["FAKENEXT36"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    #[test]
    fn secret_debug_never_shows_the_value() {
        let secret = Secret::new(SECRET);
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        assert_eq!(secret.expose(), SECRET);
        assert!(!secret.is_empty());
        assert!(Secret::new("").is_empty());
    }

    #[test]
    fn access_key_ids_are_redacted_anywhere() {
        let text = format!("User {KEY_ID} is not authorized; also ASIAIOSFODNN7EXAMPLE.");
        let out = redact_text(&text);
        assert_eq!(out, "User [REDACTED] is not authorized; also [REDACTED].");
    }

    #[test]
    fn near_misses_are_left_alone() {
        // 19 characters, wrong prefix, lowercase, and embedded in a longer word.
        for text in [
            "AKIAIOSFODNN7EXAMPL",
            "XKIAIOSFODNN7EXAMPLE",
            "akiaiosfodnn7example",
            "AKIAIOSFODNN7EXAMPLEX",
            "prefixAKIAIOSFODNN7EXAMPLE",
        ] {
            assert_eq!(redact_text(text), text, "{text}");
        }
    }

    #[test]
    fn labelled_secrets_are_redacted_in_every_syntax() {
        let cases = [
            (
                format!("aws_secret_access_key={SECRET} rest"),
                "aws_secret_access_key=[REDACTED] rest",
            ),
            (
                format!("AWS_SECRET_ACCESS_KEY: {SECRET}"),
                "AWS_SECRET_ACCESS_KEY: [REDACTED]",
            ),
            (
                format!("{{\"SecretAccessKey\": \"{SECRET}\", \"Expiration\": 1}}"),
                "{\"SecretAccessKey\": \"[REDACTED]\", \"Expiration\": 1}",
            ),
            (
                "session_token='abc123def' done".to_string(),
                "session_token='[REDACTED]",
            ),
            (
                "https://h/p?X-Amz-Signature=deadbeef&X-Amz-Expires=60".to_string(),
                "https://h/p?X-Amz-Signature=[REDACTED]&X-Amz-Expires=60",
            ),
            (
                "password = hunter2, user=bob".to_string(),
                "password = [REDACTED], user=bob",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_text(&input), expected);
        }
    }

    #[test]
    fn header_lines_and_bearer_tokens_are_redacted() {
        let header = "Authorization: AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/us-east-1/s3/aws4_request, Signature=abc\nHost: example.com";
        assert_eq!(
            redact_text(header),
            "Authorization: [REDACTED]\nHost: example.com"
        );
        assert_eq!(
            redact_text("sent Bearer eyJhbGciOi.payload.sig to server"),
            "sent Bearer [REDACTED] to server"
        );
        assert_eq!(
            redact_text("cookie: a=b; c=d\r\nx: y"),
            "cookie: [REDACTED]\r\nx: y"
        );
    }

    #[test]
    fn long_session_tokens_are_redacted_by_shape() {
        let token = format!("IQoJb3JpZ2lu{}", "A".repeat(120));
        assert_eq!(redact_text(&format!("got {token} ok")), "got [REDACTED] ok");
        let short = "IQoJb3JpZ2luABC";
        assert_eq!(redact_text(short), short);
    }

    #[test]
    fn plain_messages_and_non_ascii_text_pass_through() {
        for text in [
            "",
            "Unable to locate credentials",
            "Rate exceeded",
            "naïve ünïcode — fine",
            "token",
            "password:",
            "key=value&other=thing",
        ] {
            assert_eq!(redact_text(text), text, "{text:?}");
        }
        assert_eq!(redact_text("ünï token=sécret ✓"), "ünï token=[REDACTED] ✓");
    }

    #[test]
    fn urls_lose_userinfo_and_secret_query_values() {
        assert_eq!(
            redact_url("https://user:pw@example.amazonaws.com/path?token=abc&x=1"),
            "https://[REDACTED]@example.amazonaws.com/path?token=[REDACTED]&x=1"
        );
        assert_eq!(
            redact_url("https://example.amazonaws.com/a@b"),
            "https://example.amazonaws.com/a@b"
        );
        assert_eq!(redact_url("not a url"), "not a url");
    }

    #[test]
    fn snippet_truncates_flattens_and_redacts() {
        assert_eq!(snippet("abc", 10), "abc");
        assert_eq!(snippet("abcdef", 3), "abc…");
        assert_eq!(snippet("a\nb\u{0}c", 10), "a b?c");
        assert_eq!(snippet("token=zzz", 40), "token=[REDACTED]");
    }

    #[test]
    fn sanitize_path_strips_query_and_id_runs() {
        assert_eq!(
            sanitize_path("/api/launches/3f2504e0-4f89-41d3-9a0c-0305e82c3301?x=1#f"),
            "/api/launches/<id>"
        );
        assert_eq!(
            sanitize_path("/api/uploads/abcdefghijklmnop/short"),
            "/api/uploads/<id>/short"
        );
        assert_eq!(sanitize_path("/api/uploads"), "/api/uploads");
        assert_eq!(sanitize_path("/a/0123456789abcde"), "/a/0123456789abcde");
        assert_eq!(sanitize_path(""), "");
    }

    #[test]
    fn sensitive_header_names_are_case_insensitive() {
        for name in [
            "Authorization",
            "X-Amz-Security-Token",
            "COOKIE",
            "x-api-key",
        ] {
            assert!(is_sensitive_header(name), "{name}");
        }
        for name in ["content-type", "x-amz-request-id", "host"] {
            assert!(!is_sensitive_header(name), "{name}");
        }
    }

    #[test]
    fn secret_names_are_recognized_by_marker() {
        for name in [
            "password",
            "AWS_SECRET_ACCESS_KEY",
            "sessionToken",
            "X-Api-Key",
            "Credential",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["service", "region", "launch_id", "next_page"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact_text(&format!("a {KEY_ID} token={SECRET}"));
        assert_eq!(redact_text(&once), once);
    }

    /// Hostile and malformed inputs (synthetic secrets only) with the marker
    /// that must not survive and the exact expected output.
    fn hostile_cases() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            // Escaped quote inside a JSON string: the tail must not leak.
            (
                r#"{"password":"aa\"tail-leak"}"#,
                "tail-leak",
                r#"{"password":"[REDACTED]"}"#,
            ),
            // Escaped backslash before the closing quote really closes.
            (
                r#"{"password":"aa\\","next":"keep"}"#,
                "aa",
                r#"{"password":"[REDACTED]","next":"keep"}"#,
            ),
            // Newline after the colon, before the colon, and CRLF.
            (
                "{\"password\":\n\"FAKE-JSON-PASSWORD\"}",
                "FAKE-JSON-PASSWORD",
                "{\"password\":\n\"[REDACTED]\"}",
            ),
            (
                "{\"password\"\n  :\r\n  \"FAKE-JSON-PASSWORD\"}",
                "FAKE-JSON-PASSWORD",
                "{\"password\"\n  :\r\n  \"[REDACTED]\"}",
            ),
            // Whitespace written as JSON escapes inside an embedded document.
            (
                r#"{"password":\n"FAKE-ESCAPED-WS"}"#,
                "FAKE-ESCAPED-WS",
                r#"{"password":\n"[REDACTED]"}"#,
            ),
            // JSON embedded as an escaped string, escaped quote inside.
            (
                r#"{"body":"{\"token\":\"aa\\\"tail-leak\",\"ok\":1}"}"#,
                "tail-leak",
                r#"{"body":"{\"token\":\"[REDACTED]\",\"ok\":1}"}"#,
            ),
            // Single-quoted value with an escaped single quote.
            (
                r"{'secret': 'aa\'tail-leak'}",
                "tail-leak",
                r"{'secret': '[REDACTED]'}",
            ),
            // Unterminated string: hide to the end of the input.
            (
                r#"{"password":"never-closed, more: stuff"#,
                "never-closed",
                r#"{"password":"[REDACTED]"#,
            ),
            // Text glued to the closing quote: the end is uncertain, hide to the end.
            (
                r#"{"password":"aa"tail-leak"}"#,
                "tail-leak",
                r#"{"password":"[REDACTED]"#,
            ),
            // `=>` separator and a header on a later line.
            (
                "token => FAKE-ARROW-VALUE rest",
                "FAKE-ARROW-VALUE",
                "token => [REDACTED] rest",
            ),
            (
                "{\"authorization\":\n\"Bearer FAKE-HEADER-VALUE\"}",
                "FAKE-HEADER-VALUE",
                "{\"authorization\":\n\"[REDACTED]\"}",
            ),
            // A key right after a literal `\n` escape.
            (
                r"line one\npassword: FAKE-LINE-VALUE",
                "FAKE-LINE-VALUE",
                r"line one\npassword: [REDACTED]",
            ),
        ]
    }

    #[test]
    fn hostile_json_and_text_inputs_do_not_leak() {
        for (input, leak, expected) in hostile_cases() {
            let out = redact_text(input);
            assert!(!out.contains(leak), "{input:?} leaked {leak:?}: {out:?}");
            assert_eq!(out, expected, "{input:?}");
            assert_eq!(redact_text(&out), out, "not idempotent: {input:?}");
            assert!(
                !snippet(input, 200).contains(leak),
                "snippet leaked for {input:?}"
            );
        }
    }

    #[test]
    fn empty_header_value_does_not_swallow_the_next_line() {
        assert_eq!(
            redact_text("Authorization:\nHost: example.com"),
            "Authorization:\nHost: example.com"
        );
    }

    #[test]
    fn embedded_urls_lose_userinfo_everywhere() {
        let cases = [
            (
                "dial https://FAKE-USER:FAKE-URL-PASSWORD@logs.amazonaws.com failed",
                "dial https://[REDACTED]@logs.amazonaws.com failed",
            ),
            (
                "proxy=http://u:p%40ss@10.0.0.1:3128/x, retry",
                "proxy=http://[REDACTED]@10.0.0.1:3128/x, retry",
            ),
            // Several URLs, a literal `@` in the password, userinfo only.
            (
                "a https://u:p@one.example/x then ftp://v:w@@two.example/y",
                "a https://[REDACTED]@one.example/x then ftp://[REDACTED]@two.example/y",
            ),
            (
                "(https://tok@host.example)",
                "(https://[REDACTED]@host.example)",
            ),
            (
                r#"{"url":"https://u:FAKE-PW@host.example/p"}"#,
                r#"{"url":"https://[REDACTED]@host.example/p"}"#,
            ),
            (
                r#"{"url":"https:\/\/u:FAKE-PW@host.example\/p"}"#,
                r#"{"url":"https:\/\/[REDACTED]@host.example\/p"}"#,
            ),
            (
                "https://u:FAKE-PW@[2600:1f18::1]:443/p",
                "https://[REDACTED]@[2600:1f18::1]:443/p",
            ),
        ];
        for (input, expected) in cases {
            let out = redact_text(input);
            assert!(!out.contains("FAKE-"), "{input:?} -> {out:?}");
            assert!(!out.contains("p%40ss"), "{input:?} -> {out:?}");
            assert_eq!(out, expected, "{input:?}");
            assert_eq!(redact_text(&out), out, "not idempotent: {input:?}");
        }
        // An `@` in the path or query is not userinfo.
        for keep in [
            "https://host.example/a@b",
            "https://host.example/?mail=a@b",
            "see https://host.example#x@y",
            "no scheme user:pw@host stays",
        ] {
            assert_eq!(redact_text(keep), keep);
        }
    }

    #[test]
    fn every_formatting_wrapper_uses_the_common_scrubber() {
        let text = "x https://u:FAKE-PW@h.example/ token=FAKE-TOKEN";
        for out in [
            redact_url(text),
            snippet(text, 100),
            Scrubbed(text).to_string(),
            format!("{:?}", Scrubbed(text)),
        ] {
            assert!(!out.contains("FAKE-"), "{out}");
        }
    }

    #[test]
    fn header_value_hiding_covers_secret_names() {
        for name in [
            "authorization",
            "x-extra-token",
            "x-access-token",
            "x-amz-security-token",
            "x-session-id",
            "x-api-key",
            "x-client-secret",
            "cookie",
            "x-signature",
            "x-private-thing",
            "proxy-authorization",
            "x-bearer",
            "x-credential-scope",
            "x-pass-through",
        ] {
            assert!(hides_header_value(name), "{name}");
        }
        for name in ["content-type", "host", "x-amz-request-id", "content-length"] {
            assert!(!hides_header_value(name), "{name}");
        }
    }

    #[test]
    fn adversarial_inputs_redact_exactly_and_stably() {
        for &(input, expected, leaks) in ADVERSARIAL {
            let out = redact_text(input);
            assert_eq!(out, expected, "{input:?}");
            let stable = [out, snippet(input, 400), Scrubbed(input).to_string()];
            for shown in &stable {
                for leak in leaks {
                    assert!(
                        !shown.contains(leak),
                        "{input:?} leaked {leak:?}: {shown:?}"
                    );
                }
                assert_eq!(&redact_text(shown), shown, "not idempotent: {input:?}");
            }
            let debug = format!("{:?}", Scrubbed(input));
            for leak in leaks {
                assert!(!debug.contains(leak), "{input:?} leaked {leak:?}: {debug}");
            }
        }
    }

    #[test]
    fn scheme_credentials_need_a_credential_after_the_whitespace() {
        for text in [
            "Bearer",
            "Bearer   ",
            "Bearer ,next",
            "Bearer <token>",
            "basic\t",
            "Bearer [REDACTED]",
            "Digest [REDACTED]",
        ] {
            assert_eq!(redact_text(text), text, "{text:?}");
        }
        // Any case, any whitespace run, one redaction per credential.
        for name in [
            "Bearer",
            "bearer",
            "BEARER",
            "Basic",
            "bAsIc",
            "NTLM",
            "Negotiate",
        ] {
            for gap in [" ", "  ", "\t", " \t ", "\n", "\u{a0}"] {
                let text = format!("{name}{gap}RkFLRS1DUkVE more");
                let out = redact_text(&text);
                assert_eq!(out, format!("{name}{gap}{REDACTED} more"), "{text:?}");
                assert_eq!(redact_text(&out), out);
            }
        }
    }

    #[test]
    fn prose_and_harmless_text_is_kept() {
        for text in [
            "The security token included in the request is invalid",
            "the service returned a pagination token it had already returned",
            "pagination failed: nextToken must be a string, got integer",
            "Verify AWS credentials: aws configure list",
            "userinfo (credentials) in the URL is not allowed",
            "https://host.example/?mail=a@b",
            "message digest mismatch",
        ] {
            assert_eq!(redact_text(text), text, "{text:?}");
        }
    }

    #[test]
    fn ambiguous_text_is_over_redacted_rather_than_leaked() {
        for (input, expected) in [
            ("a basic check failed", "a basic [REDACTED] failed"),
            ("failed to negotiate TLS", "failed to negotiate [REDACTED]"),
            (
                "session_token='abc123def' done",
                "session_token='[REDACTED]",
            ),
            ("password: it's", "password: [REDACTED]"),
            ("x mytoken=abc y", "x mytoken=[REDACTED] y"),
        ] {
            let out = redact_text(input);
            assert_eq!(out, expected, "{input:?}");
            assert_eq!(redact_text(&out), out);
        }
    }

    #[test]
    fn whole_output_scrubbing_wrappers_are_stable() {
        struct Leaky<'a>(&'a str);
        impl fmt::Debug for Leaky<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_scrubbed_debug(f, |f| {
                    f.debug_struct("Leaky").field("raw", &self.0).finish()
                })
            }
        }
        impl fmt::Display for Leaky<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_scrubbed_display(f, |f| f.write_str(self.0))
            }
        }
        for &(input, _, leaks) in ADVERSARIAL {
            // The raw field is formatted unscrubbed: only the whole-output
            // pass stands between it and the printed text.
            let leaky = Leaky(input);
            for shown in [
                format!("{leaky:?}"),
                format!("{leaky:#?}"),
                leaky.to_string(),
            ] {
                for leak in leaks {
                    assert!(!shown.contains(leak), "{input:?} leaked {leak:?}: {shown}");
                }
                assert_eq!(redact_text(&shown), shown, "not idempotent: {input:?}");
            }
        }
    }

    #[test]
    fn the_marker_itself_is_a_fixpoint_in_every_position() {
        // Each recognizer must treat an already-redacted value as complete;
        // otherwise passes never converge and the whole text is hidden.
        for text in [
            "token=[REDACTED]",
            "token=[REDACTED] rest",
            "password: [REDACTED]",
            "{\"password\":\"[REDACTED]\"}",
            "{\"password\":\"[REDACTED]",
            "Authorization: [REDACTED]\nHost: h",
            "https://[REDACTED]@h.example/",
            "Bearer  [REDACTED]",
            "Digest [REDACTED]",
        ] {
            assert_eq!(scrub_once(text), text, "{text:?}");
            assert_eq!(redact_text(text), text, "{text:?}");
        }
    }
}
