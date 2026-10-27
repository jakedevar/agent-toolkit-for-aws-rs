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
/// key, case-insensitively (`my_token=`, `nextToken:`, `token_value:`,
/// `password_backup=` all count): the key is read through to its end and its
/// value is hidden when a `=`, `:` or `=>` separator follows the whole key.
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

/// Longest text [`redact_text`] scans, in bytes (256 KiB). Longer text is
/// hidden entirely instead of being cut first (#4536): cutting raw text could
/// split a key ID or token so that its remainder is no longer recognized. An
/// error message or `Debug` output this long carries no useful diagnostic, and
/// the cap bounds the per-pass lookup tables to a fixed amount of memory.
pub const MAX_REDACT_INPUT_BYTES: usize = 256 * 1024;

/// Deepest quote escaping (backslashes before a quote) whose closing quote is
/// searched for. A quoted value opened by a deeper escape is hidden to the end
/// of the input. Each nesting of `Debug` inside a string roughly doubles the
/// depth (1, 3, 7, 15, 31), so 32 covers five levels. The cap is what keeps the
/// quoted-value scans linear (see [`Text`]).
const MAX_QUOTE_ESCAPE_DEPTH: usize = 32;

/// The one diagnostic scrubber: replace credential-looking content in `input`
/// with [`REDACTED`].
///
/// Every error `Display`, `Debug`, conversion and rendered failure in this
/// crate goes through this function. One pass collects every span that any
/// recognizer wants hidden (URL userinfo, labelled values, scheme credentials,
/// key IDs and session tokens), each recognizer tried at every position of the
/// same input even inside another's span, and replaces their union. Passes
/// repeat until the text no longer changes, so the result is idempotent by
/// construction: `redact_text(&redact_text(x)) == redact_text(x)`. When the
/// end of a value is uncertain the redaction extends to the end of the input.
///
/// Each pass does work linear in the length of its text, there
/// are at most `MAX_PASSES + 1` passes, and a text longer than
/// [`MAX_REDACT_INPUT_BYTES`] is replaced by [`REDACTED`] without scanning, so
/// untrusted error text cannot make redaction expensive (#4536).
#[must_use]
pub fn redact_text(input: &str) -> String {
    if input.len() > MAX_REDACT_INPUT_BYTES {
        return REDACTED.to_string();
    }
    let mut current = scrub_once(input);
    for _ in 0..MAX_PASSES {
        if current.len() > MAX_REDACT_INPUT_BYTES {
            return REDACTED.to_string();
        }
        let next = scrub_once(&current);
        if next == current {
            return current;
        }
        current = next;
    }
    REDACTED.to_string()
}

// Count of elementary scan steps, so tests can prove the work bound
// deterministically instead of timing it (#4536). Compiled out of non-test
// builds.
#[cfg(test)]
thread_local! {
    static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn tick(steps: usize) {
    WORK.with(|work| work.set(work.get() + steps));
}

#[cfg(not(test))]
#[inline]
fn tick(_steps: usize) {}

/// One pass: hide the union of every recognizer's spans.
fn scrub_once(input: &str) -> String {
    let text = Text::new(input);
    let mut spans = url_userinfo_spans(&text);
    spans.extend(labelled_spans(&text));
    tick(spans.len());
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

/// One pass's view of the input with lookup tables that make every scan a
/// table lookup (#4536).
///
/// Each table is filled right to left in one sweep, so building them is
/// linear. With them a pass is linear in the input length `n`:
///
/// - labels are matched once per position, and each *complete* key (a maximal
///   run of key bytes) has its separator and value parsed at most twice (line
///   label, other label) however many labels it contains;
/// - whitespace runs, backslash runs, unquoted value ends, line ends and
///   base64 runs are table lookups; an escape body is at most 9 bytes;
/// - a quoted value scan stops at the first quote that could close it. Two
///   values with the same quote and escape depth cannot overlap, since the
///   later opening quote would close the earlier value. Every scan therefore
///   belongs to one of at most `2 * (MAX_QUOTE_ESCAPE_DEPTH + 1)` classes and
///   each class covers the input at most once. The clean-close check after a
///   closing quote crosses only whitespace and quotes, which a separator or
///   scheme name ends, so it also covers the input at most once per class;
/// - the clean-close check after an unquoted value is cached by the value's
///   end, which many nested keys (`token=token=...x"`) may share.
struct Text<'a> {
    input: &'a str,
    bytes: &'a [u8],
    lower: Vec<u8>,
    /// Index after the run of backslashes starting at each index.
    backslash_end: Vec<usize>,
    /// `Text::skip_ws` from each index.
    ws_end: Vec<usize>,
    /// Next `\n` or `\r` at or after each index.
    line_end: Vec<usize>,
    /// Next [`is_value_end`] byte at or after each index.
    value_end: Vec<usize>,
    /// Where an unquoted labelled value starting at each index stops: a value
    /// end byte or a backslash run followed by a quote.
    unquoted_end: Vec<usize>,
    /// Index after the run of base64 bytes starting at each index.
    base64_end: Vec<usize>,
    /// Clean-close result per unquoted value end.
    unquoted_clean: std::cell::RefCell<std::collections::HashMap<usize, bool>>,
}

/// Right-to-left table: the first index at or after each index where `stop`
/// holds (or the length).
fn next_stop(len: usize, stop: impl Fn(usize) -> bool) -> Vec<usize> {
    let mut table = vec![len; len + 1];
    for i in (0..len).rev() {
        table[i] = if stop(i) { i } else { table[i + 1] };
    }
    tick(len);
    table
}

impl<'a> Text<'a> {
    fn new(input: &'a str) -> Self {
        let bytes = input.as_bytes();
        let len = bytes.len();
        let lower = input.to_ascii_lowercase().into_bytes();
        let backslash_end = next_stop(len, |i| bytes[i] != b'\\');
        let line_end = next_stop(len, |i| matches!(bytes[i], b'\n' | b'\r'));
        let value_end = next_stop(len, |i| is_value_end(bytes[i]));
        let unquoted_end = next_stop(len, |i| {
            is_value_end(bytes[i])
                || (bytes[i] == b'\\' && bytes.get(backslash_end[i]).copied().is_some_and(is_quote))
        });
        let base64_end = next_stop(len, |i| !is_base64(bytes[i]));
        let mut ws_end = vec![len; len + 1];
        for p in (0..len).rev() {
            ws_end[p] = if bytes[p] == b'\\' {
                match escaped_whitespace_end(input, backslash_end[p]) {
                    Some(next) => ws_end[next],
                    None => p,
                }
            } else if input.is_char_boundary(p) {
                match input[p..].chars().next() {
                    Some(c) if c.is_whitespace() => ws_end[p + c.len_utf8()],
                    _ => p,
                }
            } else {
                p
            };
        }
        tick(len);
        Self {
            input,
            bytes,
            lower,
            backslash_end,
            ws_end,
            line_end,
            value_end,
            unquoted_end,
            base64_end,
            unquoted_clean: std::cell::RefCell::default(),
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Skip whitespace (Unicode, newlines included) and escaped whitespace at
    /// any escape depth: `\n`, `\r`, `\t`, JSON `\u00a0` and Rust `\u{2003}`
    /// forms, also with doubled backslashes (`\\t`).
    fn skip_ws(&self, p: usize) -> usize {
        self.ws_end[p.min(self.len())]
    }

    /// Number of consecutive backslashes at `p`, and the index after them.
    fn skip_backslashes(&self, p: usize) -> (usize, usize) {
        let p = p.min(self.len());
        let after = self.backslash_end[p];
        (after - p, after)
    }

    /// If a quote, possibly written as an escaped quote (`\"`), starts at
    /// `at`: its escape depth (number of backslashes) and the index of the
    /// quote itself.
    fn quote_at(&self, at: usize) -> Option<(usize, usize)> {
        let (depth, index) = self.skip_backslashes(at);
        (index < self.len() && is_quote(self.bytes[index])).then_some((depth, index))
    }
}

/// Userinfo spans of every `scheme://` URL in `input`, plain or with escaped
/// slashes at any depth (`:\/\/` in JSON, `:\\/\\/` once more escaped).
///
/// The authority runs to the first `/`, `?`, `#` or whitespace. Everything
/// before its *last* `@` is userinfo, whatever it contains (quotes,
/// apostrophes, backslashes, more `@`). An `@` in the path, query or fragment
/// is not userinfo. Authorities of different URLs never overlap (each ends at
/// the next `/`), so the scan is linear.
fn url_userinfo_spans(text: &Text) -> Vec<(usize, usize)> {
    let input = text.input;
    let mut spans = Vec::new();
    for (colon, _) in input.match_indices(':') {
        tick(1);
        let Some(authority) = after_double_slash(text, colon + 1) else {
            continue;
        };
        let tail = &input[authority..];
        let authority_len = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
            .unwrap_or(tail.len());
        tick(authority_len);
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
fn after_double_slash(text: &Text, mut p: usize) -> Option<usize> {
    for _ in 0..2 {
        let (_, after) = text.skip_backslashes(p);
        if text.bytes.get(after) != Some(&b'/') {
            return None;
        }
        p = after + 1;
    }
    Some(p)
}

/// Spans of labelled values, scheme credentials, key IDs and session tokens.
///
/// Every recognizer runs at *every* position of the original input, including
/// positions inside a span another recognizer already matched (#4515): in
/// `token=Bearer X` the label hides `Bearer` and the scheme still hides `X`.
/// No span ever lets the scan skip ahead.
///
/// A secret label may sit anywhere in a key (#4516): each complete key (a
/// maximal run of key bytes such as `token_value`, `password_backup` or
/// `ExpiredTokenException`) is classified once by the labels it contains, and
/// its separator and value are then parsed once (#4536), so trailing name text
/// never hides a labelled value and repeated labels never repeat the work.
///
/// A scheme word that is itself (the start of) a labelled value never gets
/// the prose exemption: in `password=Token abcd` the label hides `Token`, so
/// `abcd` is treated as its credential and hidden too.
fn labelled_spans(text: &Text) -> Vec<(usize, usize)> {
    let bytes = text.bytes;
    let mut spans = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !is_key_byte(bytes[start]) {
            start += 1;
            continue;
        }
        let key_end = scan_until(bytes, start, |b| !is_key_byte(b));
        let (mut line_label, mut secret_label) = (false, false);
        for (at, byte) in bytes.iter().enumerate().take(key_end).skip(start) {
            if !byte.is_ascii_alphanumeric() {
                continue;
            }
            spans.extend(identifier_span(text, at));
            let lower = &text.lower[at..];
            tick(LINE_KEYS.len() + SECRET_KEYS.len());
            line_label = line_label
                || LINE_KEYS
                    .iter()
                    .any(|key| lower.starts_with(key.as_bytes()));
            secret_label = secret_label
                || SECRET_KEYS
                    .iter()
                    .any(|key| lower.starts_with(key.as_bytes()));
        }
        if line_label {
            spans.extend(value_span(text, key_end, true));
        }
        if secret_label {
            spans.extend(value_span(text, key_end, false));
        }
        start = key_end;
    }
    // How many value spans open and close at each byte, so one sweep knows
    // whether a position lies inside a labelled value.
    let mut opens = vec![0_usize; bytes.len() + 1];
    let mut closes = vec![0_usize; bytes.len() + 1];
    for &(from, to) in &spans {
        opens[from] += 1;
        closes[to] += 1;
    }
    tick(spans.len() + bytes.len());
    let mut covering = 0_usize;
    for (i, (open, close)) in opens.iter().zip(&closes).take(bytes.len()).enumerate() {
        covering = covering + open - close;
        spans.extend(scheme_span(text, i, covering == 0));
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

/// Bytes that continue a field or key name (`token_value`, `x-amz-token`,
/// `auth.token.id`).
fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

/// Credential after an authentication scheme starting at `i`.
///
/// Any run of whitespace (Unicode, newlines and `\n`/`\t` escapes included)
/// separates the scheme from its credential. A token credential runs to the
/// next delimiter; a parameter-list credential runs to the end of the line; a
/// quoted credential follows the quoted-value rules. `prose_ok` allows the
/// prose exemption of English scheme words (`token included`).
fn scheme_span(text: &Text, i: usize, prose_ok: bool) -> Option<(usize, usize)> {
    let bytes = text.bytes;
    if !bytes[i].is_ascii_alphabetic() || !is_word_boundary(bytes, i) {
        return None;
    }
    tick(AUTH_SCHEMES.len());
    for auth in AUTH_SCHEMES {
        if !text.lower[i..].starts_with(auth.name.as_bytes()) {
            continue;
        }
        let after = i + auth.name.len();
        let start = text.skip_ws(after);
        if start == after || start >= bytes.len() {
            continue;
        }
        if let Some((depth, open)) = text.quote_at(start) {
            let from = open + 1;
            let end = quoted_value_end(text, from, bytes[open], depth);
            if end > from {
                return Some((from, end));
            }
            continue;
        }
        if !is_credential_start(bytes[start]) {
            continue;
        }
        if prose_ok && auth.prose_word && is_prose_word(bytes, start) {
            continue;
        }
        let end = if auth.params {
            text.line_end[start]
        } else {
            text.value_end[start]
        };
        return Some((start, end));
    }
    None
}

/// A short all-lowercase word followed by whitespace, punctuation or the end.
/// Looks at no more than 16 bytes.
fn is_prose_word(bytes: &[u8], start: usize) -> bool {
    const MAX_PROSE_WORD: usize = 16;
    let run = bytes[start..]
        .iter()
        .take(MAX_PROSE_WORD)
        .take_while(|b| b.is_ascii_lowercase())
        .count();
    tick(run);
    let end = start + run;
    run > 0
        && run < MAX_PROSE_WORD
        && (end == bytes.len()
            || bytes[end].is_ascii_whitespace()
            || matches!(bytes[end], b'.' | b',' | b')' | b'!' | b'?'))
}

/// Access-key IDs and STS session tokens, recognized by shape alone. Only the
/// start of an alphanumeric run is examined, so each run is scanned once.
fn identifier_span(text: &Text, i: usize) -> Option<(usize, usize)> {
    let bytes = text.bytes;
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
        let end = text.base64_end[i];
        if end - i >= MIN_SESSION_TOKEN_LEN {
            return Some((i, end));
        }
    }
    None
}

/// End of an escape body at `p` (just after its backslashes) that encodes
/// whitespace: `n`, `r`, `t`, `uXXXX` or `u{X..}` naming a whitespace char.
/// Looks at no more than 9 bytes.
fn escaped_whitespace_end(input: &str, p: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    match *bytes.get(p)? {
        b'n' | b'r' | b't' => Some(p + 1),
        b'u' => {
            let (digits, end) = if bytes.get(p + 1) == Some(&b'{') {
                // At most six digits, so the `}` is within the next 7 bytes.
                let window = bytes.get(p + 2..).unwrap_or_default();
                let window = &window[..window.len().min(7)];
                let close = p + 2 + window.iter().position(|&b| b == b'}')?;
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

fn is_quote(byte: u8) -> bool {
    matches!(byte, b'"' | b'\'')
}

/// Value span after a key that ended at `after_key`.
///
/// Accepts `key=value`, `key: value`, `"key": "value"`, `key => value` and the
/// same inside escaped JSON (`\"key\":\"value\"`), with any whitespace
/// (including newlines) around the separator. Once a label and separator are
/// found, a value that is not cleanly terminated is hidden to the end of the
/// input.
fn value_span(text: &Text, after_key: usize, whole_line: bool) -> Option<(usize, usize)> {
    let bytes = text.bytes;
    let mut pos = text.skip_ws(after_key);
    if let Some((_, key_quote)) = text.quote_at(pos) {
        pos = text.skip_ws(key_quote + 1);
    }
    if pos >= bytes.len() || !matches!(bytes[pos], b'=' | b':') {
        return None;
    }
    pos += 1;
    if bytes.get(pos) == Some(&b'>') {
        pos += 1;
    }
    let mut value_at = text.skip_ws(pos);
    if whole_line && text.quote_at(value_at).is_none() {
        // An empty header value must not swallow the next header line; only a
        // quoted value may start on a later line.
        value_at = pos;
        while bytes
            .get(value_at)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            value_at += 1;
        }
        tick(value_at - pos);
    }
    if value_at >= bytes.len() {
        return None;
    }
    if let Some((depth, open)) = text.quote_at(value_at) {
        let start = open + 1;
        let end = quoted_value_end(text, start, bytes[open], depth);
        return (end > start).then_some((start, end));
    }
    if whole_line {
        let end = text.line_end[value_at];
        return (end > value_at).then_some((value_at, end));
    }
    unquoted_value_span(text, value_at)
}

/// An unquoted value: a token ending at whitespace, `&`, `,`, `;`, `)`, `}` or
/// the end (not `]`, which ends the `[REDACTED]` marker itself and would stop
/// redaction converging). A value cut short by a quote (plain or escaped) is
/// trusted only when that quote closes cleanly; otherwise it is hidden to the
/// end.
fn unquoted_value_span(text: &Text, start: usize) -> Option<(usize, usize)> {
    let end = text.unquoted_end[start];
    if end == start {
        return None;
    }
    if let Some((depth, quote_index)) = text.quote_at(end) {
        let cached = text.unquoted_clean.borrow().get(&end).copied();
        let clean = cached.unwrap_or_else(|| {
            let clean = closes_cleanly(text, quote_index + 1, text.bytes[quote_index], depth);
            text.unquoted_clean.borrow_mut().insert(end, clean);
            clean
        });
        if !clean {
            return Some((start, text.len()));
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
/// closes the value. An unterminated value, a closing quote that is not
/// followed by a structural delimiter ([`closes_cleanly`]), or an escape depth
/// above `MAX_QUOTE_ESCAPE_DEPTH` makes the end uncertain, so the span then
/// runs to the end of the input.
fn quoted_value_end(text: &Text, start: usize, quote: u8, escape_depth: usize) -> usize {
    let bytes = text.bytes;
    if escape_depth > MAX_QUOTE_ESCAPE_DEPTH {
        return bytes.len();
    }
    let mut p = start;
    while p < bytes.len() {
        tick(1);
        if bytes[p] == b'\\' {
            let run_start = p;
            let (run, after_run) = text.skip_backslashes(p);
            p = after_run;
            if p < bytes.len() && bytes[p] == quote {
                let closes = if escape_depth == 0 {
                    run % 2 == 0
                } else {
                    run == escape_depth
                };
                if closes {
                    let end = if escape_depth == 0 { p } else { run_start };
                    return if closes_cleanly(text, p + 1, quote, escape_depth) {
                        end
                    } else {
                        bytes.len()
                    };
                }
                p += 1;
            }
        } else if bytes[p] == quote && escape_depth == 0 {
            return if closes_cleanly(text, p + 1, quote, 0) {
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
/// text and makes the end uncertain, as does an escape depth above
/// `MAX_QUOTE_ESCAPE_DEPTH`.
fn closes_cleanly(text: &Text, mut p: usize, quote: u8, escape_depth: usize) -> bool {
    let bytes = text.bytes;
    if escape_depth > MAX_QUOTE_ESCAPE_DEPTH {
        return false;
    }
    let mut depth = escape_depth;
    let mut quote = quote;
    loop {
        tick(1);
        p = text.skip_ws(p);
        if p >= bytes.len() || matches!(bytes[p], b',' | b'}' | b']' | b';' | b')') {
            return true;
        }
        let (run, after) = text.skip_backslashes(p);
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
    tick(p - start);
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
    // Re-review #4467 finding 1 (#4515): a label whose unquoted value is a
    // scheme word must not stop the scheme recognizer from hiding the
    // credential after it.
    (
        "token=Bearer FAKE-OVERLAP-VALUE",
        "token=[REDACTED] [REDACTED]",
        &["FAKE-OVERLAP-VALUE"],
    ),
    (
        "password=Basic RkFLRTpPVkVSTEFQ",
        "password=[REDACTED] [REDACTED]",
        &["RkFLRTpPVkVSTEFQ"],
    ),
    (
        "secret:\tbearer\tFAKE-OVL37 next",
        "secret:\t[REDACTED]\t[REDACTED] next",
        &["FAKE-OVL37"],
    ),
    (
        r#"{"msg":"token=Bearer FAKE-OVL38"}"#,
        r#"{"msg":"token=[REDACTED] [REDACTED]"}"#,
        &["FAKE-OVL38"],
    ),
    (
        "api_key: Token fakeovl",
        "api_key: [REDACTED] [REDACTED]",
        &["fakeovl"],
    ),
    // Finding 2 (#4516): the label at the start, middle and end of a longer
    // key, any case, plain and escaped JSON.
    (
        r#"{"token_value":"FAKE-SUFFIX39"}"#,
        r#"{"token_value":"[REDACTED]"}"#,
        &["FAKE-SUFFIX39"],
    ),
    (
        "password_backup=FAKE-SUFFIX40 rest",
        "password_backup=[REDACTED] rest",
        &["FAKE-SUFFIX40"],
    ),
    (
        "my_Password_Old: FAKE-MID41",
        "my_Password_Old: [REDACTED]",
        &["FAKE-MID41"],
    ),
    (
        "CLIENT_SECRET_VALUE = FAKE-UPPER42, ok",
        "CLIENT_SECRET_VALUE = [REDACTED], ok",
        &["FAKE-UPPER42"],
    ),
    (
        r#"{\"client_secret_value\":\"FAKE-ESC43\",\"n\":1}"#,
        r#"{\"client_secret_value\":\"[REDACTED]\",\"n\":1}"#,
        &["FAKE-ESC43"],
    ),
    (
        "x-session-token-v2: FAKE-HDR44",
        "x-session-token-v2: [REDACTED]",
        &["FAKE-HDR44"],
    ),
    (
        "auth.token.value=FAKE-DOT45&x=1",
        "auth.token.value=[REDACTED]&x=1",
        &["FAKE-DOT45"],
    ),
    (
        "authorization_header: Bearer FAKE-LINE46\nnext: 1",
        "authorization_header: [REDACTED]\nnext: 1",
        &["FAKE-LINE46"],
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
            "Verify AWS credentials with: aws configure list",
            "userinfo (credentials) in the URL is not allowed",
            "Error: ExpiredTokenException - boom",
            "GET /x failed (403): InvalidClientTokenId",
            "the password_backup file is stale",
            "Partial credentials found: missing secret_access_key",
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
            // Full-key containment (#4516): any key containing a label hides
            // its value, ordinary words included.
            (
                "Verify AWS credentials: aws configure list",
                "Verify AWS credentials: [REDACTED] configure list",
            ),
            (
                "ExpiredTokenException: The security token expired",
                "ExpiredTokenException: [REDACTED] security token expired",
            ),
            ("tokens: 5", "tokens: [REDACTED]"),
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

    /// Elementary scan steps one `redact_text` call spends on `input` (#4536).
    fn work_of(input: &str) -> usize {
        WORK.with(|work| work.set(0));
        let out = redact_text(input);
        let work = WORK.with(std::cell::Cell::get);
        assert_eq!(redact_text(&out), out, "not idempotent");
        work
    }

    /// `token=<d backslashes>"a ` for each depth `d`.
    fn quoted_at_depths(depths: impl Iterator<Item = usize>) -> String {
        let mut out = String::new();
        for depth in depths {
            out.push_str("token=");
            out.push_str(&"\\".repeat(depth));
            out.push_str("\"a ");
        }
        out
    }

    /// Smallest `m` with `m * m >= k`.
    fn ceil_sqrt(k: usize) -> usize {
        let mut m = 1;
        while m * m < k {
            m += 1;
        }
        m
    }

    type Family = (&'static str, Box<dyn Fn(usize) -> String>);

    fn fam(name: &'static str, build: impl Fn(usize) -> String + 'static) -> Family {
        (name, Box::new(build))
    }

    /// Input families whose scans were (or could become) quadratic: each maps
    /// a repetition count to an input whose length is linear in it.
    fn work_bound_families() -> Vec<Family> {
        vec![
            // The #4536 report: one key with the label repeated.
            fam("repeated label, separator, short value", |k| {
                format!("{}=x", "token".repeat(k))
            }),
            fam("repeated label, no separator", |k| "token".repeat(k)),
            fam("repeated label, long value", |k| {
                format!("{}={}", "token".repeat(k), "v".repeat(5 * k))
            }),
            fam("repeated label, long quoted value", |k| {
                format!("{}=\"{}\"", "token".repeat(k), "v".repeat(5 * k))
            }),
            fam("mixed labels in one key", |k| {
                format!("{}: {}", "Password_Secret-token.".repeat(k), "w".repeat(k))
            }),
            fam("mixed labels in one key, no separator", |k| {
                format!("{} x", "Authorization-Cookie_pwd".repeat(k))
            }),
            // Values that contain further labelled keys.
            fam("nested unquoted values", |k| {
                format!("{}x", "token=".repeat(k))
            }),
            fam("nested values sharing a quoted end", |k| {
                format!("{}x{}", "token=".repeat(k), "'\"".repeat(k))
            }),
            fam("nested line values", |k| "authorization: ".repeat(k)),
            fam("quoted openers", |k| "token=\"".repeat(k)),
            fam("escaped quoted openers", |k| "token=\\\"a ".repeat(k)),
            fam("closed values before alternating quotes", |k| {
                "token=\"a\"'\"'\" ".repeat(k)
            }),
            fam("escape depths cycling past the cap", |k| {
                quoted_at_depths((0..k / 10).map(|i| i % 40 + 1))
            }),
            fam("escape depths growing with the input", |k| {
                quoted_at_depths(1..=ceil_sqrt(k))
            }),
            // Scheme credentials.
            fam("parameter schemes", |k| "AWS4-HMAC-SHA256 ".repeat(k)),
            fam("credentials across escaped newlines", |k| {
                "Bearer x\\n".repeat(k)
            }),
            fam("credentials across Unicode spaces", |k| {
                "Bearer\u{2003}x\u{2003}".repeat(k)
            }),
            fam("prose word then a long word", |k| {
                format!("token {}", "a".repeat(5 * k))
            }),
            fam("repeated prose scheme", |k| "token ".repeat(k)),
            // Shapes, whitespace and escapes.
            fam("base64 runs", |k| "IQoJ+".repeat(k)),
            fam("key IDs between labels", |k| {
                "AKIAIOSFODNN7EXAMPLE token=".repeat(k)
            }),
            fam("backslash run in a value", |k| {
                format!("token={}", "\\".repeat(5 * k))
            }),
            fam("unclosed brace escapes", |k| "token \\u{".repeat(k)),
            fam("long escaped whitespace before a separator", |k| {
                format!("token{}=x", "\\n\\u{2003} ".repeat(k))
            }),
            fam("Unicode whitespace after labels", |k| {
                "token\u{2003}".repeat(k)
            }),
            fam("URL authorities", |k| "a://b:c@".repeat(k)),
            fam("escaped URL slashes", |k| "a:\\/\\/".repeat(k)),
        ]
    }

    #[test]
    fn redaction_work_is_linear_in_the_input_length() {
        // Quadratic work grows 16-fold when the input grows 4-fold; linear
        // work about 4-fold. The bound per byte covers every pass.
        const K: usize = 1000;
        const PER_BYTE: usize = 1000;
        for (name, family) in work_bound_families() {
            let small = family(K);
            let large = family(4 * K);
            assert!(
                large.len() <= MAX_REDACT_INPUT_BYTES,
                "{name}: input too long"
            );
            assert!(large.len() >= 3 * small.len(), "{name}: not linear in k");
            let (small_work, large_work) = (work_of(&small), work_of(&large));
            assert!(
                large_work <= 5 * small_work.max(small.len()),
                "{name}: work {small_work} -> {large_work} for {} -> {} bytes",
                small.len(),
                large.len()
            );
            assert!(
                large_work <= PER_BYTE * large.len(),
                "{name}: {large_work} steps for {} bytes",
                large.len()
            );
        }
    }

    #[test]
    fn the_reported_repeated_label_input_is_redacted_in_linear_work() {
        // #4536: "token" x 20,000 + "=x" (100,002 bytes) took about 10^9 key
        // byte checks per pass before the fix.
        let input = format!("{}=x", "token".repeat(20_000));
        assert_eq!(input.len(), 100_002);
        let work = work_of(&input);
        assert!(work <= 400 * input.len(), "{work} steps");
        assert_eq!(
            redact_text(&input),
            format!("{}=[REDACTED]", "token".repeat(20_000))
        );
        // Without a separator nothing is hidden, and the work stays linear.
        let bare = "token".repeat(20_000);
        assert!(work_of(&bare) <= 400 * bare.len());
        assert_eq!(redact_text(&bare), bare);
    }

    #[test]
    fn text_over_the_size_cap_is_hidden_without_scanning() {
        let at_cap = "a".repeat(MAX_REDACT_INPUT_BYTES);
        assert_eq!(redact_text(&at_cap), at_cap);
        let over = format!("token=FAKE-OVER-CAP {}", "a".repeat(MAX_REDACT_INPUT_BYTES));
        assert_eq!(work_of(&over), 0);
        assert_eq!(redact_text(&over), REDACTED);
        assert_eq!(snippet(&over, 1024), REDACTED);
        assert_eq!(Scrubbed(&over).to_string(), REDACTED);
    }

    #[test]
    fn quotes_escaped_past_the_depth_cap_hide_to_the_end() {
        let deep = "\\".repeat(MAX_QUOTE_ESCAPE_DEPTH + 1);
        let input = format!("token={deep}\"FAKE-DEEP{deep}\", tail");
        assert_eq!(redact_text(&input), format!("token={deep}\"[REDACTED]"));
        let ok = "\\".repeat(MAX_QUOTE_ESCAPE_DEPTH - 1);
        let input = format!("token={ok}\"FAKE-OK{ok}\", tail");
        assert_eq!(
            redact_text(&input),
            format!("token={ok}\"[REDACTED]{ok}\", tail")
        );
    }
}
