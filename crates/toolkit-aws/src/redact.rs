//! Secret redaction used by every error, `Debug` and diagnostic in this crate.
//!
//! Redaction is deliberately over-eager: a false positive hides a harmless
//! word, a false negative leaks a credential. [`redact_text`] recognizes
//!
//! - AWS access-key IDs (`AKIA...`, `ASIA...` and the other 20-character IAM
//!   prefixes) and long base64 session tokens (`IQoJ...`, `FwoG...`);
//! - `key = value` / `key: value` / JSON `"key": "value"` pairs whose key names a
//!   secret (`aws_secret_access_key`, `session_token`, `authorization`,
//!   `x-amz-signature`, `password`, ...), unquoted, single or double quoted;
//! - `Bearer <token>` and `Basic <token>` credentials;
//! - userinfo in URLs (see [`redact_url`]).
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

/// Keys (lowercase) whose value is a secret and ends at a delimiter.
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
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "x-api-key",
    "signature",
];

/// Keys (lowercase) whose value is a whole header line (may contain spaces).
const LINE_KEYS: &[&str] = &[
    "proxy-authorization",
    "authorization",
    "set-cookie",
    "cookie",
];

/// Four-character prefixes of 20-character IAM access-key and unique IDs.
const KEY_ID_PREFIXES: &[&str] = &[
    "AKIA", "ASIA", "AGPA", "AIDA", "AIPA", "ANPA", "ANVA", "AROA", "ABIA", "ACCA",
];

/// Base64 prefixes of STS session tokens.
const TOKEN_PREFIXES: &[&str] = &["IQoJ", "FwoG"];

/// Minimum length of a run recognized as an STS session token.
const MIN_SESSION_TOKEN_LEN: usize = 100;

/// The one diagnostic scrubber: replace credential-looking content in `input`
/// with [`REDACTED`].
///
/// Every error `Display`, `Debug`, conversion and rendered failure in this
/// crate goes through this function. It first strips URL userinfo wherever a
/// URL is embedded in the text, then redacts labelled values. Values follow
/// JSON rules (any whitespace, including newlines, around separators; escaped
/// quotes inside strings; embedded, escaped JSON); when the end of a value is
/// uncertain the redaction extends to the end of the input. The result is
/// idempotent: `redact_text(&redact_text(x)) == redact_text(x)`.
#[must_use]
pub fn redact_text(input: &str) -> String {
    scrub_labelled(&strip_url_userinfo(input))
}

/// Strip `user:password@` from every `scheme://...` URL embedded in `input`.
///
/// The authority ends at the first `/`, `?` or `#` (and at a backslash in the
/// JSON-escaped `:\/\/` form); the URL token ends at whitespace, a quote or an
/// angle bracket. Everything before the last `@` of the authority is userinfo.
fn strip_url_userinfo(input: &str) -> String {
    const PLAIN: &str = "://";
    const ESCAPED: &str = ":\\/\\/";
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        let plain = rest.find(PLAIN);
        let escaped = rest.find(ESCAPED);
        let (pos, sep_len, is_escaped) = match (plain, escaped) {
            (Some(p), Some(e)) if e < p => (e, ESCAPED.len(), true),
            (Some(p), _) => (p, PLAIN.len(), false),
            (None, Some(e)) => (e, ESCAPED.len(), true),
            (None, None) => break,
        };
        let authority_start = pos + sep_len;
        out.push_str(&rest[..authority_start]);
        let tail = &rest[authority_start..];
        let token_end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(tail.len());
        let token = &tail[..token_end];
        let authority_len = if is_escaped {
            token.find(['/', '?', '#', '\\'])
        } else {
            token.find(['/', '?', '#'])
        }
        .unwrap_or(token.len());
        if let Some(at) = token[..authority_len].rfind('@') {
            out.push_str(REDACTED);
            out.push('@');
            rest = &tail[at + 1..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// Redact labelled values, access-key IDs, session tokens and bearer tokens.
fn scrub_labelled(input: &str) -> String {
    let bytes = input.as_bytes();
    let lower = input.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if let Some((from, to)) = find_span(bytes, lower, i) {
            out.push_str(&input[copied..from]);
            out.push_str(REDACTED);
            copied = to;
            i = to.max(i + 1);
        } else {
            i += 1;
        }
    }
    out.push_str(&input[copied..]);
    out
}

/// Redact a URL. Kept for readability at URL call sites; it is exactly
/// [`redact_text`], which already strips userinfo and secret query values.
#[must_use]
pub fn redact_url(url: &str) -> String {
    redact_text(url)
}

/// Redact, flatten control characters and truncate to `max_chars` characters.
/// Used for echoing untrusted values (a bad region, a response body) in errors.
#[must_use]
pub fn snippet(value: &str, max_chars: usize) -> String {
    let redacted = redact_text(value);
    let mut out: String = redacted
        .chars()
        .take(max_chars)
        .map(|c| match c {
            // Whitespace controls become a space so the flattened text scrubs
            // to the same result as the original (idempotence).
            '\t' | '\n' | '\r' | '\u{b}' | '\u{c}' => ' ',
            c if c.is_control() => '?',
            c => c,
        })
        .collect();
    if redacted.chars().count() > max_chars {
        out.push('…');
    }
    out
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

fn is_word_boundary(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    // A JSON escape such as `\n` directly before the key ends the previous word.
    if i >= 2 && bytes[i - 2] == b'\\' && matches!(bytes[i - 1], b'n' | b'r' | b't') {
        return true;
    }
    !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' || bytes[i - 1] == b'-')
}

fn is_value_end(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'&' | b',' | b';' | b'"' | b'\'' | b')' | b'}')
}

fn is_base64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'=')
}

/// Span `[from, to)` of `bytes` to redact when a secret starts at `i`.
fn find_span(bytes: &[u8], lower: &[u8], i: usize) -> Option<(usize, usize)> {
    if !bytes[i].is_ascii_alphanumeric() {
        return None;
    }
    if let Some(span) = identifier_span(bytes, i) {
        return Some(span);
    }
    if !is_word_boundary(bytes, i) {
        return None;
    }
    for key in LINE_KEYS {
        if lower[i..].starts_with(key.as_bytes()) {
            if let Some(span) = value_span(bytes, i + key.len(), true) {
                return Some(span);
            }
        }
    }
    for key in SECRET_KEYS {
        if lower[i..].starts_with(key.as_bytes()) {
            if let Some(span) = value_span(bytes, i + key.len(), false) {
                return Some(span);
            }
        }
    }
    for scheme in ["bearer ", "basic "] {
        if lower[i..].starts_with(scheme.as_bytes()) {
            let start = i + scheme.len();
            let end = scan_until(bytes, start, is_value_end);
            if end > start {
                return Some((start, end));
            }
        }
    }
    None
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

/// Skip ASCII whitespace (newlines included) and the JSON escapes `\n`, `\r`
/// and `\t` written as two characters.
fn skip_ws(bytes: &[u8], mut p: usize) -> usize {
    loop {
        if p < bytes.len() && bytes[p].is_ascii_whitespace() {
            p += 1;
        } else if p + 1 < bytes.len()
            && bytes[p] == b'\\'
            && matches!(bytes[p + 1], b'n' | b'r' | b't')
        {
            p += 2;
        } else {
            return p;
        }
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
/// (including newlines) around the separator.
fn value_span(bytes: &[u8], after_key: usize, whole_line: bool) -> Option<(usize, usize)> {
    let mut pos = skip_ws(bytes, after_key);
    if let Some((_, key_quote)) = quote_at(bytes, pos) {
        pos = skip_ws(bytes, key_quote + 1);
    }
    if pos >= bytes.len() || !matches!(bytes[pos], b'=' | b':') {
        return None;
    }
    pos += 1;
    if bytes.get(pos) == Some(&b'>') {
        pos += 1;
    }
    let mut value_at = skip_ws(bytes, pos);
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
    let (start, end) = if let Some((depth, open)) = quote_at(bytes, value_at) {
        let start = open + 1;
        (start, quoted_value_end(bytes, start, bytes[open], depth))
    } else if whole_line {
        (
            value_at,
            scan_until(bytes, value_at, |b| b == b'\n' || b == b'\r'),
        )
    } else {
        (value_at, scan_until(bytes, value_at, is_value_end))
    };
    (end > start).then_some((start, end))
}

/// End (exclusive) of a quoted value that starts at `start`.
///
/// `escape_depth` is the number of backslashes before the opening quote: 0 for
/// plain JSON, 1 for JSON embedded in a string (`\"`), and so on. In plain
/// text a quote preceded by an odd number of backslashes is escaped; in an
/// escaped context only a quote preceded by exactly `escape_depth` backslashes
/// closes the value. An unterminated value, or text glued to the closing
/// quote, makes the end uncertain, so the span then runs to the end of the
/// input (hiding too much is the safe error).
fn quoted_value_end(bytes: &[u8], start: usize, quote: u8, escape_depth: usize) -> usize {
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
                    return end_after_close(bytes, end, p, quote, escape_depth);
                }
                p += 1;
            }
        } else if bytes[p] == quote && escape_depth == 0 {
            return end_after_close(bytes, p, p, quote, escape_depth);
        } else {
            p += 1;
        }
    }
    bytes.len()
}

/// Validate what follows a closing quote at `quote_index`; `end` is the span end
/// if the value really closes there.
fn end_after_close(bytes: &[u8], end: usize, quote_index: usize, quote: u8, depth: usize) -> usize {
    let after = quote_index + 1;
    if after >= bytes.len() || is_close_delimiter(bytes[after], quote) {
        return end;
    }
    if depth == 0 {
        // `"abc"def"`: stray text glued to the quote. Take everything up to the
        // next delimiter, keeping a final quote so the output is stable.
        let stop = scan_until(bytes, after, |b| {
            b.is_ascii_whitespace() || matches!(b, b',' | b'}' | b']' | b';' | b'&' | b')')
        });
        if stop > after && bytes[stop - 1] == quote {
            return stop - 1;
        }
    }
    bytes.len()
}

fn is_close_delimiter(byte: u8, quote: u8) -> bool {
    byte != quote
        && (byte.is_ascii_whitespace()
            || matches!(
                byte,
                b',' | b'}'
                    | b']'
                    | b';'
                    | b'&'
                    | b')'
                    | b':'
                    | b'\\'
                    | b'<'
                    | b'>'
                    | b'|'
                    | b'"'
                    | b'\''
            ))
}

fn scan_until(bytes: &[u8], start: usize, stop: impl Fn(u8) -> bool) -> usize {
    let mut p = start;
    while p < bytes.len() && !stop(bytes[p]) {
        p += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "session_token='[REDACTED]' done",
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
            // Text glued to the closing quote.
            (
                r#"{"password":"aa"tail-leak"}"#,
                "tail-leak",
                r#"{"password":"[REDACTED]"}"#,
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
}
