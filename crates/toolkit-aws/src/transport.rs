//! HTTP transport and signing seams.
//!
//! [`Transport`] sends one prepared request and returns one response (or a
//! connect/timeout [`AwsError`]); [`Signer`] adds authentication to a prepared
//! request. The SDK binding implements both; tests use the doubles in
//! [`crate::testing`]. Nothing here performs I/O.
//!
//! Requests and responses print through redacting `Debug` implementations:
//! sensitive header values and URL secrets become `[REDACTED]` and bodies show
//! only their length.

use std::fmt;
use std::time::Duration;

use crate::endpoint::Endpoint;
use crate::error::AwsError;
use crate::redact::{hides_header_value, redact_url, Scrubbed, REDACTED};

/// Longest connect or read timeout accepted.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// HTTP methods the boundary will send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `PATCH`
    Patch,
    /// `DELETE`
    Delete,
    /// `HEAD`
    Head,
}

impl Method {
    /// Upper-case method name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
        }
    }
}

/// Connect and read timeouts. The botocore default is 60 s for both; the
/// Python `launch-with-aws` client uses 120 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    connect: Duration,
    read: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(60),
            read: Duration::from_secs(60),
        }
    }
}

impl Timeouts {
    /// Validated timeouts.
    ///
    /// # Errors
    /// `InvalidConfig` when either duration is zero or above [`MAX_TIMEOUT`].
    pub fn new(connect: Duration, read: Duration) -> Result<Self, AwsError> {
        for (name, value) in [("connect", connect), ("read", read)] {
            if value.is_zero() || value > MAX_TIMEOUT {
                return Err(AwsError::invalid_config(
                    "timeout",
                    format!("the {name} timeout must be between 1 second and 1 hour"),
                ));
            }
        }
        Ok(Self { connect, read })
    }

    /// Connect timeout.
    #[must_use]
    pub fn connect(&self) -> Duration {
        self.connect
    }

    /// Read timeout.
    #[must_use]
    pub fn read(&self) -> Duration {
        self.read
    }
}

/// HTTP headers: names are lowercased, a later insert replaces an earlier one,
/// and `Debug` hides the value of every secret-bearing header name
/// ([`hides_header_value`]) and scrubs all other values.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// No headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a header, replacing any header of the same name.
    ///
    /// # Errors
    /// `InvalidConfig` when the name is empty or has characters outside
    /// `[A-Za-z0-9._-]`, or the value contains a control character other than a
    /// horizontal tab (header injection).
    pub fn insert(&mut self, name: &str, value: &str) -> Result<(), AwsError> {
        let name_ok = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !name_ok {
            return Err(AwsError::invalid_config(
                "header",
                "header names must be non-empty and use only letters, digits, '-', '_' and '.'",
            ));
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(AwsError::invalid_config(
                "header",
                "header values must not contain control characters",
            ));
        }
        let lower = name.to_ascii_lowercase();
        match self.0.iter_mut().find(|(existing, _)| *existing == lower) {
            Some(entry) => entry.1 = value.to_string(),
            None => self.0.push((lower, value.to_string())),
        }
        Ok(())
    }

    /// Value of the header `name` (any case).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.0
            .iter()
            .find(|(existing, _)| *existing == lower)
            .map(|(_, value)| value.as_str())
    }

    /// Headers in insertion order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Number of headers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no headers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (name, value) in self.entries() {
            if hides_header_value(name) {
                map.entry(&Scrubbed(name), &REDACTED);
            } else {
                map.entry(&Scrubbed(name), &Scrubbed(value));
            }
        }
        map.finish()
    }
}

/// A request before the endpoint is applied: path, query, headers and body.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiRequest {
    /// HTTP method.
    pub method: Method,
    /// Absolute path (starting with `/`) below the endpoint's base path,
    /// already percent-encoded where needed.
    pub path: String,
    /// Query parameters; the boundary percent-encodes them.
    pub query: Vec<(String, String)>,
    /// Request headers.
    pub headers: Headers,
    /// Request body.
    pub body: Vec<u8>,
}

impl ApiRequest {
    /// A bodyless request.
    #[must_use]
    pub fn new(method: Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_string(),
            query: Vec::new(),
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    /// Add a query parameter.
    #[must_use]
    pub fn with_query(mut self, name: &str, value: &str) -> Self {
        self.query.push((name.to_string(), value.to_string()));
        self
    }

    /// Set the body.
    #[must_use]
    pub fn with_body(mut self, body: &[u8]) -> Self {
        self.body = body.to_vec();
        self
    }
}

impl fmt::Debug for ApiRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiRequest")
            .field("method", &self.method.as_str())
            .field("path", &redact_url(&self.path))
            .field("query_params", &self.query.len())
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// A fully addressed request handed to the transport.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: Method,
    /// Absolute https URL, query included.
    pub url: String,
    /// Request headers (the signer adds authentication here).
    pub headers: Headers,
    /// Request body.
    pub body: Vec<u8>,
    /// Timeouts to apply.
    pub timeouts: Timeouts,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method.as_str())
            .field("url", &redact_url(&self.url))
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .field("timeouts", &self.timeouts)
            .finish()
    }
}

/// A response from the transport.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Headers,
    /// Response body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response with a status, no headers and the given body.
    #[must_use]
    pub fn new(status: u16, body: &[u8]) -> Self {
        Self {
            status,
            headers: Headers::new(),
            body: body.to_vec(),
        }
    }

    /// The body as UTF-8 text, if valid.
    #[must_use]
    pub fn body_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.body).ok()
    }
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Region and service a request is signed for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningScope {
    /// Signing region.
    pub region: String,
    /// Signing service name.
    pub service: String,
}

/// Sends one prepared request.
pub trait Transport {
    /// Send `request`.
    ///
    /// # Errors
    /// `Connect` when the endpoint cannot be reached, `Timeout` when a timeout
    /// fires. HTTP error statuses are *not* errors here; they are returned as
    /// responses and classified by the client.
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, AwsError>;
}

/// Authenticates a prepared request (for example AWS Signature V4).
pub trait Signer {
    /// Add authentication to `request`, using credentials from the provider
    /// chain. Called once per attempt so retries get fresh signatures.
    ///
    /// # Errors
    /// Credential errors from the provider chain, or signing failures.
    fn sign(&self, request: &mut HttpRequest, scope: &SigningScope) -> Result<(), AwsError>;
}

/// A signer that adds nothing (anonymous or token-in-header requests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSigner;

impl Signer for NoSigner {
    fn sign(&self, _request: &mut HttpRequest, _scope: &SigningScope) -> Result<(), AwsError> {
        Ok(())
    }
}

/// Upper-case hexadecimal digits for percent-encoding.
const HEX_DIGITS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];

/// Percent-encode `value` for a URL component (RFC 3986 unreserved characters
/// are kept).
#[must_use]
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(HEX_DIGITS[usize::from(byte >> 4)]);
            out.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
        }
    }
    out
}

/// Join an endpoint, a request path and encoded query parameters.
///
/// # Errors
/// `InvalidConfig` when the path is empty, does not start with `/`, or has
/// whitespace, control characters, `\`, `?`, `#` or a `..` segment.
pub fn build_url(
    endpoint: &Endpoint,
    path: &str,
    query: &[(String, String)],
) -> Result<String, AwsError> {
    let path_ok = path.starts_with('/')
        && !path
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\\' | '?' | '#'))
        && !path.split('/').any(|segment| segment == "..");
    if !path_ok {
        return Err(AwsError::invalid_config(
            "path",
            "request paths must start with '/' and contain no whitespace, control characters, \
             '\\', '?', '#' or '..' segments",
        ));
    }
    let mut url = format!("{}{path}", endpoint.url());
    for (index, (name, value)) in query.iter().enumerate() {
        url.push(if index == 0 { '?' } else { '&' });
        url.push_str(&percent_encode(name));
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{validate_endpoint, EndpointPolicy};
    use crate::testing::FixedResolver;

    fn endpoint(url: &str) -> Endpoint {
        validate_endpoint(
            url,
            "endpoint",
            &EndpointPolicy::aws_public().without_dns_check(),
            &FixedResolver::new(),
        )
        .unwrap()
    }

    #[test]
    fn headers_lowercase_names_and_replace_by_name() {
        let mut headers = Headers::new();
        headers.insert("Content-Type", "a").unwrap();
        headers.insert("X-Other", "1").unwrap();
        headers.insert("content-type", "b").unwrap();
        assert_eq!(headers.len(), 2);
        assert!(!headers.is_empty());
        assert_eq!(headers.get("CONTENT-TYPE"), Some("b"));
        assert_eq!(headers.get("missing"), None);
        let order: Vec<&str> = headers.entries().map(|(name, _)| name).collect();
        assert_eq!(order, vec!["content-type", "x-other"]);
    }

    #[test]
    fn header_injection_and_bad_names_are_rejected() {
        let mut headers = Headers::new();
        for (name, value) in [
            ("x-a", "line1\r\nInjected: yes"),
            ("x-a", "nul\0byte"),
            ("", "v"),
            ("bad name", "v"),
            ("bad:name", "v"),
            ("x-a\r\nb", "v"),
        ] {
            let error = headers.insert(name, value).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{name:?} {value:?}");
        }
        assert!(headers.is_empty());
        headers.insert("x-tab", "a\tb").unwrap();
        assert_eq!(headers.get("x-tab"), Some("a\tb"));
    }

    #[test]
    fn headers_debug_hides_sensitive_values_only() {
        let mut headers = Headers::new();
        headers
            .insert("Authorization", "AWS4-HMAC-SHA256 Signature=abc")
            .unwrap();
        headers
            .insert("X-Amz-Security-Token", "session-token-value")
            .unwrap();
        headers.insert("Content-Type", "application/json").unwrap();
        let shown = format!("{headers:?}");
        assert_eq!(
            shown,
            "{\"authorization\": \"[REDACTED]\", \"x-amz-security-token\": \"[REDACTED]\", \"content-type\": \"application/json\"}"
        );
    }

    #[test]
    fn request_and_response_debug_show_lengths_not_bodies_or_secrets() {
        let mut headers = Headers::new();
        headers.insert("authorization", "Bearer abc").unwrap();
        let request = HttpRequest {
            method: Method::Post,
            url: "https://u:pw@x.amazonaws.com/p?X-Amz-Signature=deadbeef&a=1".to_string(),
            headers,
            body: b"{\"password\":\"hunter2\"}".to_vec(),
            timeouts: Timeouts::default(),
        };
        let shown = format!("{request:?}");
        for leak in ["hunter2", "deadbeef", "u:pw", "Bearer abc"] {
            assert!(!shown.contains(leak), "{leak} leaked in {shown}");
        }
        assert!(shown.contains("body_len: 22"));
        assert!(
            shown.contains("https://[REDACTED]@x.amazonaws.com/p?X-Amz-Signature=[REDACTED]&a=1")
        );
        let response = HttpResponse::new(200, b"{\"token\":\"zzz\"}");
        let shown = format!("{response:?}");
        assert!(shown.contains("status: 200") && shown.contains("body_len: 15"));
        assert!(!shown.contains("zzz"));
        let api = ApiRequest::new(Method::Get, "/a")
            .with_query("k", "v")
            .with_body(b"secret-body");
        let shown = format!("{api:?}");
        assert!(shown.contains("query_params: 1") && shown.contains("body_len: 11"));
        assert!(!shown.contains("secret-body"));
    }

    fn leaky_headers() -> Headers {
        let mut headers = Headers::new();
        for (name, value) in [
            ("x-extra", "Bearer FAKE-BEARER-VALUE"),
            ("x-access-token", "FAKE-ACCESS-TOKEN"),
            ("x-custom-key", "FAKE-KEY-VALUE"),
            ("x-session-ref", "FAKE-SESSION-VALUE"),
            ("x-note", "retry token=FAKE-NOTE-TOKEN later"),
            (
                "x-origin",
                "https://FAKE-USER:FAKE-ORIGIN-PASSWORD@host.example/",
            ),
            ("x-json", "{\"password\":\"aa\\\"FAKE-JSON-TAIL\"}"),
            ("content-type", "application/json"),
        ] {
            headers.insert(name, value).unwrap();
        }
        headers
    }

    const HEADER_LEAKS: &[&str] = &[
        "FAKE-BEARER-VALUE",
        "FAKE-ACCESS-TOKEN",
        "FAKE-KEY-VALUE",
        "FAKE-SESSION-VALUE",
        "FAKE-NOTE-TOKEN",
        "FAKE-ORIGIN-PASSWORD",
        "FAKE-USER",
        "FAKE-JSON-TAIL",
    ];

    #[test]
    fn headers_debug_hides_secret_named_values_and_scrubs_the_rest() {
        let shown = format!("{:?}", leaky_headers());
        for leak in HEADER_LEAKS {
            assert!(!shown.contains(leak), "{leak} leaked in {shown}");
        }
        // Secret-named headers are hidden entirely; others keep scrubbed text.
        assert!(shown.contains("\"x-extra\": \"Bearer [REDACTED]\""));
        assert!(shown.contains("\"x-access-token\": \"[REDACTED]\""));
        assert!(shown.contains("\"x-note\": \"retry token=[REDACTED] later\""));
        assert!(shown.contains("\"x-origin\": \"https://[REDACTED]@host.example/\""));
        assert!(shown.contains("\"content-type\": \"application/json\""));
        let pretty = format!("{:#?}", leaky_headers());
        for leak in HEADER_LEAKS {
            assert!(!pretty.contains(leak), "{leak} leaked in {pretty}");
        }
    }

    #[test]
    fn every_request_and_response_debug_path_scrubs_headers_and_urls() {
        let mut api = ApiRequest::new(
            Method::Get,
            "/p?x=1&token=FAKE-PATH-TOKEN#https://FAKE-USER:FAKE-PATH-PASSWORD@h.example/",
        );
        api.headers = leaky_headers();
        let request = HttpRequest {
            method: Method::Post,
            url: "https://FAKE-USER:FAKE-URL-PASSWORD@x.amazonaws.com/p?token=FAKE-URL-TOKEN"
                .to_string(),
            headers: leaky_headers(),
            body: b"FAKE-BODY".to_vec(),
            timeouts: Timeouts::default(),
        };
        let mut response = HttpResponse::new(500, b"FAKE-BODY");
        response.headers = leaky_headers();
        let shown = [
            format!("{api:?}"),
            format!("{api:#?}"),
            format!("{request:?}"),
            format!("{request:#?}"),
            format!("{response:?}"),
            format!("{response:#?}"),
        ];
        for text in &shown {
            for leak in HEADER_LEAKS.iter().chain(&[
                "FAKE-PATH-TOKEN",
                "FAKE-PATH-PASSWORD",
                "FAKE-URL-PASSWORD",
                "FAKE-URL-TOKEN",
                "FAKE-BODY",
            ]) {
                assert!(!text.contains(leak), "{leak} leaked in {text}");
            }
        }
    }

    #[test]
    fn methods_and_response_text() {
        let names: Vec<&str> = [
            Method::Get,
            Method::Post,
            Method::Put,
            Method::Patch,
            Method::Delete,
            Method::Head,
        ]
        .into_iter()
        .map(Method::as_str)
        .collect();
        assert_eq!(names, vec!["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"]);
        assert_eq!(HttpResponse::new(200, b"hi").body_text(), Some("hi"));
        assert_eq!(HttpResponse::new(200, &[0xff, 0xfe]).body_text(), None);
    }

    #[test]
    fn timeouts_are_validated() {
        let default = Timeouts::default();
        assert_eq!(default.connect(), Duration::from_secs(60));
        assert_eq!(default.read(), Duration::from_secs(60));
        let launch = Timeouts::new(Duration::from_secs(120), Duration::from_secs(120)).unwrap();
        assert_eq!(launch.read(), Duration::from_secs(120));
        assert!(Timeouts::new(MAX_TIMEOUT, MAX_TIMEOUT).is_ok());
        for (connect, read) in [
            (Duration::ZERO, Duration::from_secs(1)),
            (Duration::from_secs(1), Duration::ZERO),
            (MAX_TIMEOUT + Duration::from_secs(1), Duration::from_secs(1)),
            (Duration::from_secs(1), MAX_TIMEOUT + Duration::from_secs(1)),
        ] {
            assert_eq!(
                Timeouts::new(connect, read).unwrap_err().code(),
                "aws_invalid_config"
            );
        }
    }

    #[test]
    fn percent_encoding_keeps_only_unreserved_characters() {
        assert_eq!(percent_encode("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(percent_encode("a b&c=d/e?f#g"), "a%20b%26c%3Dd%2Fe%3Ff%23g");
        assert_eq!(percent_encode("é"), "%C3%A9");
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn urls_join_endpoint_base_path_and_encoded_query() {
        let plain = endpoint("https://x.us-east-1.amazonaws.com");
        assert_eq!(
            build_url(&plain, "/api/launches", &[]).unwrap(),
            "https://x.us-east-1.amazonaws.com/api/launches"
        );
        let based = endpoint("https://x.us-east-1.amazonaws.com:8443/prod/");
        let query = vec![
            ("include".to_string(), "a,b".to_string()),
            ("next token".to_string(), "x/y=".to_string()),
        ];
        assert_eq!(
            build_url(&based, "/v1/items", &query).unwrap(),
            "https://x.us-east-1.amazonaws.com:8443/prod/v1/items?include=a%2Cb&next%20token=x%2Fy%3D"
        );
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        let target = endpoint("https://x.amazonaws.com");
        for path in [
            "", "api", "/a b", "/a\nb", "/a\\b", "/a?x=1", "/a#f", "/a/../b", "/..",
        ] {
            let error = build_url(&target, path, &[]).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{path:?}");
        }
        assert!(build_url(&target, "/a..b/c", &[]).is_ok());
    }

    #[test]
    fn no_signer_leaves_the_request_unchanged() {
        let mut request = HttpRequest {
            method: Method::Get,
            url: "https://x.amazonaws.com/".to_string(),
            headers: Headers::new(),
            body: Vec::new(),
            timeouts: Timeouts::default(),
        };
        let before = request.clone();
        let scope = SigningScope {
            region: "us-east-1".to_string(),
            service: "x".to_string(),
        };
        NoSigner.sign(&mut request, &scope).unwrap();
        assert_eq!(request, before);
    }
}
