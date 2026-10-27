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

/// Replace credential-looking content in `input` with [`REDACTED`].
#[must_use]
pub fn redact_text(input: &str) -> String {
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

/// Redact a URL: userinfo becomes `[REDACTED]@` and secret query values are
/// replaced like in [`redact_text`].
#[must_use]
pub fn redact_url(url: &str) -> String {
    let stripped = match url.find("://") {
        Some(scheme_end) => {
            let authority_start = scheme_end + 3;
            let rest = &url[authority_start..];
            let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            match rest[..authority_len].rfind('@') {
                Some(at) => format!(
                    "{}{REDACTED}@{}",
                    &url[..authority_start],
                    &url[authority_start + at + 1..]
                ),
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    };
    redact_text(&stripped)
}

/// Redact, flatten control characters and truncate to `max_chars` characters.
/// Used for echoing untrusted values (a bad region, a response body) in errors.
#[must_use]
pub fn snippet(value: &str, max_chars: usize) -> String {
    let redacted = redact_text(value);
    let mut out: String = redacted
        .chars()
        .take(max_chars)
        .map(|c| if c.is_control() { '?' } else { c })
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

fn is_word_boundary(bytes: &[u8], i: usize) -> bool {
    i == 0
        || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' || bytes[i - 1] == b'-')
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

/// Value span after a key that ended at `after_key`.
fn value_span(bytes: &[u8], after_key: usize, whole_line: bool) -> Option<(usize, usize)> {
    let mut p = after_key;
    if p < bytes.len() && matches!(bytes[p], b'"' | b'\'') {
        p += 1;
    }
    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t') {
        p += 1;
    }
    if p >= bytes.len() || !matches!(bytes[p], b'=' | b':') {
        return None;
    }
    p += 1;
    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t') {
        p += 1;
    }
    if p >= bytes.len() {
        return None;
    }
    let (start, end) = if matches!(bytes[p], b'"' | b'\'') {
        let quote = bytes[p];
        let start = p + 1;
        (start, scan_until(bytes, start, |b| b == quote))
    } else if whole_line {
        (p, scan_until(bytes, p, |b| b == b'\n' || b == b'\r'))
    } else {
        (p, scan_until(bytes, p, is_value_end))
    };
    (end > start).then_some((start, end))
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
        assert_eq!(snippet("a\nb\u{0}c", 10), "a?b?c");
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
}
