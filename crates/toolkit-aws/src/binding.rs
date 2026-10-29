//! The SDK-independent half of the AWS SDK adapter (#4463).
//!
//! The `sdk` cargo feature binds [`CredentialsProvider`], [`Signer`] and
//! [`Transport`] to the AWS SDK for Rust (see [`crate::sdk`] when the feature
//! is enabled). Every decision that adapter makes that does not need an SDK
//! type lives here, so the default, SDK-free build compiles and tests it:
//!
//! - how an SDK credential failure becomes an [`AwsError`] (`NoCredentials`
//!   falls through a [`crate::ProviderChain`]; anything else stops it). The
//!   provider's own error text is never kept (#4594);
//! - how SDK credentials are adopted (empty parts and expired credentials are
//!   refused);
//! - how an HTTP connector failure becomes a retryable `Connect` / `Timeout`
//!   or a non-retryable endpoint refusal;
//! - the connect-time DNS guard that re-applies the public-address policy of
//!   [`crate::validate_endpoint`] to the very addresses the connection uses;
//! - the same policy for a literal IP host in a hand-built URL, in every form
//!   a resolver accepts, and for the socket's peer address (#4593);
//! - the https-only gate for a hand-built [`crate::HttpRequest`] URL;
//! - copying wire response headers into [`Headers`].
//!
//! Every message passes through the [`AwsError`] constructors, which redact it;
//! nothing here formats an SDK value with `Debug`.
//!
//! [`CredentialsProvider`]: crate::CredentialsProvider
//! [`Signer`]: crate::Signer
//! [`Transport`]: crate::Transport

use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::SystemTime;

use crate::credentials::Credentials;
use crate::endpoint::{is_non_public, non_public_reason, public_addresses, HostResolver};
use crate::error::{AwsError, TimeoutPhase};
use crate::redact::snippet;
use crate::transport::Headers;

/// Provider name the SDK default credential chain reports.
pub const SDK_DEFAULT_CHAIN: &str = "sdk-default-chain";

/// Longest SDK failure detail kept in an error message (after redaction).
pub const MAX_DETAIL_CHARS: usize = 512;

/// The kinds of SDK credential failure (`aws_credential_types`
/// `CredentialsError` variants, named without depending on the SDK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialFailure {
    /// No provider in the chain had credentials.
    NotLoaded,
    /// A provider took too long.
    TimedOut,
    /// A profile or setting is malformed.
    InvalidConfiguration,
    /// A provider failed (for example an SSO token has expired).
    Provider,
    /// Anything else.
    Unhandled,
}

/// Map an SDK credential failure onto the boundary error.
///
/// `NotLoaded` becomes `NoCredentials` (as botocore's `Unable to locate
/// credentials`); every other kind becomes a `CredentialProvider` error from
/// `provider` whose message is a fixed label for the kind.
///
/// The provider's own error text is deliberately not taken (#4594): it can
/// carry anything a `credential_process` printed on stderr or a custom
/// provider put in its message, and pattern redaction cannot recognise an
/// unlabelled bare secret. Only the kind and the provider name are reported.
#[must_use]
pub fn credential_error(provider: &'static str, kind: CredentialFailure) -> AwsError {
    let label = match kind {
        CredentialFailure::NotLoaded => return AwsError::NoCredentials,
        CredentialFailure::TimedOut => "the provider timed out",
        CredentialFailure::InvalidConfiguration => "invalid credential configuration",
        CredentialFailure::Provider => "the credential provider failed",
        CredentialFailure::Unhandled => "unexpected credential error",
    };
    AwsError::provider(provider, label)
}

/// Adopt credentials handed over by the SDK.
///
/// # Errors
/// `PartialCredentials` when the key id or secret is empty, and
/// `CredentialsExpired` when the credentials expire at or before `now` (the
/// SDK may return cached credentials that lapsed before a request is signed).
pub fn adopt_credentials(
    access_key_id: &str,
    secret_access_key: &str,
    session_token: Option<&str>,
    expires_at: Option<SystemTime>,
    now: SystemTime,
) -> Result<Credentials, AwsError> {
    let credentials =
        Credentials::new(access_key_id, secret_access_key, session_token, expires_at)?;
    if credentials.is_expired_at(now) {
        return Err(AwsError::CredentialsExpired);
    }
    Ok(credentials)
}

/// The kinds of HTTP connector failure (`aws_smithy_runtime_api`
/// `ConnectorError` classes, named without depending on the SDK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorFailure {
    /// A connect or read timeout fired.
    Timeout,
    /// An I/O failure: refused, reset, DNS failure, TLS failure.
    Io,
    /// The request could not be sent as built (a caller bug).
    User,
    /// Anything else the connector reported.
    Other,
}

/// Which timeout fired, from the connector's failure text: the smithy hyper
/// connector names its timeouts `HTTP connect` and `HTTP read`. Anything not
/// naming the connect phase is treated as a read timeout.
#[must_use]
pub fn timeout_phase(detail: &str) -> TimeoutPhase {
    if detail.to_ascii_lowercase().contains("http connect") {
        TimeoutPhase::Connect
    } else {
        TimeoutPhase::Read
    }
}

/// Map a connector failure onto the boundary error: timeouts become
/// `Timeout` and I/O and unknown failures `Connect` (all retryable); a request
/// the connector refused as built becomes a non-retryable `InvalidConfig`.
#[must_use]
pub fn connector_error(kind: ConnectorFailure, detail: &str) -> AwsError {
    match kind {
        ConnectorFailure::Timeout => AwsError::timeout(
            timeout_phase(detail),
            with_detail("the request timed out", detail),
        ),
        ConnectorFailure::Io => AwsError::connect(with_detail("I/O failure", detail)),
        ConnectorFailure::Other => AwsError::connect(with_detail("connector failure", detail)),
        ConnectorFailure::User => AwsError::invalid_config(
            "request",
            with_detail("the HTTP client refused the request", detail),
        ),
    }
}

/// The connect-time DNS guard refused a host. Carried as the source of the
/// SDK's DNS error so the transport can recognise it in the failure chain and
/// report a non-retryable endpoint refusal instead of a retryable I/O error.
#[derive(Clone, PartialEq, Eq)]
pub struct GuardRejection(AwsError);

impl GuardRejection {
    /// The boundary error (`InvalidConfig` for field `endpoint`).
    #[must_use]
    pub fn error(&self) -> &AwsError {
        &self.0
    }
}

impl fmt::Display for GuardRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for GuardRejection {
    /// The redacting `Debug` of the wrapped error.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("GuardRejection").field(&self.0).finish()
    }
}

impl Error for GuardRejection {}

/// Resolve `host` for a connection, accepting it only if every answer is
/// public: the same policy as [`crate::validate_endpoint`], applied to the
/// addresses the connection will actually use. Closes the DNS-rebinding
/// window between endpoint validation and connecting.
///
/// # Errors
/// A [`GuardRejection`] holding `InvalidConfig` (field `endpoint`) when the
/// lookup fails, is empty or has any non-public answer.
pub fn guarded_addresses(
    host: &str,
    resolver: &dyn HostResolver,
) -> Result<Vec<IpAddr>, GuardRejection> {
    public_addresses(&host.to_ascii_lowercase(), resolver)
        .map_err(|reason| GuardRejection(AwsError::invalid_config("endpoint", reason)))
}

/// Find a [`GuardRejection`] anywhere in an error chain, following both
/// `source()` and the payload of `std::io::Error` (which `source()` skips).
#[must_use]
pub fn find_guard_rejection<'e>(error: &'e (dyn Error + 'static)) -> Option<&'e GuardRejection> {
    let mut next = Some(error);
    // Bounded so a cyclic chain cannot loop forever.
    for _ in 0..64 {
        let current = next?;
        if let Some(rejection) = current.downcast_ref::<GuardRejection>() {
            return Some(rejection);
        }
        next = match current.downcast_ref::<std::io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn Error + 'static))
                .or_else(|| current.source()),
            None => current.source(),
        };
    }
    None
}

/// Refuse a URL the transport must not send to: anything but `https://`, or
/// an authority carrying userinfo. [`crate::ApiClient`] only builds URLs from
/// a validated [`crate::Endpoint`]; this guards a hand-built request.
///
/// # Errors
/// `InvalidConfig` (field `url`) without echoing the URL.
pub fn require_https(url: &str) -> Result<(), AwsError> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(AwsError::invalid_config(
            "url",
            "expected an absolute https URL",
        ));
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(AwsError::invalid_config(
            "url",
            "only https URLs are sent. Refusing to send credentials over a non-HTTPS connection",
        ));
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(AwsError::invalid_config(
            "url",
            "the URL must name a host and carry no userinfo (credentials)",
        ));
    }
    Ok(())
}

/// Apply the public-address policy to a literal IP host in `url` before any
/// connection is made (#4593).
///
/// The hyper connector skips the resolver, and so the DNS guard, for a host
/// that parses as an IP address, and the system resolver turns the legacy
/// numeric forms (`2130706433`, `0177.0.0.1`, `0x7f.1`, `127.1`) into
/// addresses too. So the host is read the way a resolver would read it:
///
/// - a bracketed host must be a plain IPv6 address (IPv4-mapped, NAT64 and
///   IPv4-compatible forms are judged by [`is_non_public`]); a zone identifier
///   (`[fe80::1%25eth0]`) or anything else in brackets is refused;
/// - a host whose last label is numeric (decimal, octal with a leading `0`, or
///   hexadecimal with `0x`) is an IPv4 literal in `inet_aton` form, one to
///   four parts, optional trailing dot; one that does not parse is refused;
/// - any other host is a name, left to the connect-time DNS guard. A host with
///   characters outside letters, digits, `-` and `.` (percent-encoding, `_`,
///   whitespace) is refused, so this reading and the HTTP client's cannot
///   differ.
///
/// # Errors
/// `InvalidConfig` (field `endpoint`, not retryable) naming the address and
/// its block for a non-public literal; `InvalidConfig` (field `url`) for a
/// host that cannot be read unambiguously. The URL itself is never echoed.
pub fn require_public_host(url: &str) -> Result<(), AwsError> {
    match literal_host_address(url)? {
        Some(address) => require_public_address(address, "the URL names"),
        None => Ok(()),
    }
}

/// Apply the public-address policy to the peer address of the socket a
/// request actually used: a backstop behind [`require_public_host`] and the
/// DNS guard, which between them already cover every address the connector
/// dials.
///
/// # Errors
/// `InvalidConfig` (field `endpoint`, not retryable) for a non-public peer.
pub fn require_public_peer(address: IpAddr) -> Result<(), AwsError> {
    require_public_address(address, "the connection reached")
}

fn require_public_address(address: IpAddr, what: &str) -> Result<(), AwsError> {
    if is_non_public(address) {
        return Err(AwsError::invalid_config(
            "endpoint",
            format!(
                "{what} the non-public address {address} ({}). \
                 Refusing to send credentials to a non-public address",
                non_public_reason(address)
            ),
        ));
    }
    Ok(())
}

fn unreadable_host(reason: &str) -> AwsError {
    AwsError::invalid_config("url", format!("the URL host {reason}"))
}

/// The IP address a URL's host names literally, if it is one (see
/// [`require_public_host`] for the forms recognised).
///
/// # Errors
/// `InvalidConfig` (field `url`) for a host that cannot be read unambiguously.
pub fn literal_host_address(url: &str) -> Result<Option<IpAddr>, AwsError> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let port_ok = |port: &str| port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
    if let Some(inner) = authority.strip_prefix('[') {
        let Some((host, after)) = inner.split_once(']') else {
            return Err(unreadable_host("has an unclosed bracket"));
        };
        if !(after.is_empty() || after.strip_prefix(':').is_some_and(port_ok)) {
            return Err(unreadable_host(
                "is followed by something other than a port",
            ));
        }
        if host.contains('%') {
            return Err(unreadable_host(
                "carries an IPv6 zone identifier, which a public endpoint never needs",
            ));
        }
        return host
            .parse::<Ipv6Addr>()
            .map(|address| Some(IpAddr::V6(address)))
            .map_err(|_| unreadable_host("is not a valid IPv6 address"));
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if port.is_some_and(|port| !port_ok(port)) {
        return Err(unreadable_host("has a malformed port"));
    }
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return Err(unreadable_host(
            "must be made of letters, digits, '-' and '.' only",
        ));
    }
    if !ends_in_number(host) {
        return Ok(None);
    }
    parse_ipv4_literal(host)
        .map(|address| Some(IpAddr::V4(address)))
        .ok_or_else(|| unreadable_host("ends in a number but is not a valid IPv4 address"))
}

/// Whether the last label of `host` is numeric, which makes the whole host an
/// IPv4 literal to a resolver (the WHATWG URL "ends in a number" rule). No DNS
/// top-level domain is numeric.
fn ends_in_number(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    let last = host.rsplit('.').next().unwrap_or(host);
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x")
        .or_else(|| last.strip_prefix("0X"))
        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Parse an IPv4 literal the way `inet_aton` does: one to four parts, each
/// decimal, octal (leading `0`) or hexadecimal (`0x`); the last part fills the
/// remaining bytes (`127.1` is `127.0.0.1`, `2130706433` is `127.0.0.1`).
fn parse_ipv4_literal(host: &str) -> Option<Ipv4Addr> {
    let host = host.strip_suffix('.').unwrap_or(host);
    let parts = host
        .split('.')
        .map(parse_ipv4_number)
        .collect::<Option<Vec<u64>>>()?;
    let (&last, init) = parts.split_last()?;
    if init.len() > 3 || init.iter().any(|&part| part > 255) {
        return None;
    }
    let mut value = 0_u64;
    for &part in init {
        value = (value << 8) | part;
    }
    // Bits left for the last part: 32, 24, 16 or 8.
    let width = 32 - 8 * u32::try_from(init.len()).ok()?;
    if last >> width != 0 {
        return None;
    }
    u32::try_from((value << width) | last)
        .ok()
        .map(Ipv4Addr::from)
}

fn parse_ipv4_number(part: &str) -> Option<u64> {
    let (digits, radix) =
        if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            if hex.is_empty() {
                return Some(0);
            }
            (hex, 16)
        } else if let Some(octal) = part.strip_prefix('0').filter(|rest| !rest.is_empty()) {
            (octal, 8)
        } else {
            (part, 10)
        };
    if digits.is_empty() || digits.len() > 16 || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(digits, radix).ok()
}

/// Copy response headers from the wire. Names and values that
/// [`Headers::insert`] refuses (characters outside its name alphabet, control
/// characters) are dropped rather than failing the whole response; a repeated
/// name keeps its last value, except duplicate Content-Length values are
/// combined so consumers can detect the invalid framing.
#[must_use]
pub fn response_headers<'h, I>(entries: I) -> Headers
where
    I: IntoIterator<Item = (&'h str, &'h str)>,
{
    let mut headers = Headers::new();
    for (name, value) in entries {
        // A refused header is dropped on purpose (see above).
        // Preserve duplicate Content-Length as an invalid comma-separated value
        // so status-aware consumers can reject it rather than lose evidence.
        if name.eq_ignore_ascii_case("content-length") {
            if let Some(previous) = headers.get(name) {
                let combined = format!("{previous},{value}");
                let _ = headers.insert(name, &combined);
                continue;
            }
        }
        let _ = headers.insert(name, value);
    }
    headers
}

fn with_detail(label: &str, detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {}", snippet(detail, MAX_DETAIL_CHARS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FixedResolver;
    use std::time::Duration;

    const LEAKY: &str = "profile failed: aws_secret_access_key=FAKESECRETVALUE0123456789 \
                         Authorization: Bearer FAKE-BEARER-TOKEN-VALUE";

    fn assert_clean(error: &AwsError) {
        for shown in [
            error.to_string(),
            format!("{error:?}"),
            format!("{error:#?}"),
        ] {
            assert!(!shown.contains("FAKESECRETVALUE"), "{shown}");
            assert!(!shown.contains("FAKE-BEARER"), "{shown}");
        }
    }

    #[test]
    fn not_loaded_falls_through_and_other_kinds_stop_the_chain() {
        assert_eq!(
            credential_error(SDK_DEFAULT_CHAIN, CredentialFailure::NotLoaded),
            AwsError::NoCredentials
        );
        for (kind, label) in [
            (CredentialFailure::TimedOut, "the provider timed out"),
            (
                CredentialFailure::InvalidConfiguration,
                "invalid credential configuration",
            ),
            (
                CredentialFailure::Provider,
                "the credential provider failed",
            ),
            (CredentialFailure::Unhandled, "unexpected credential error"),
        ] {
            let error = credential_error(SDK_DEFAULT_CHAIN, kind);
            assert_eq!(error.code(), "aws_credential_provider", "{kind:?}");
            assert!(!error.is_retryable(), "{kind:?}");
            // The whole message is the fixed label: nothing a provider said.
            assert_eq!(
                error.to_string(),
                format!("credential provider {SDK_DEFAULT_CHAIN} failed: {label}")
            );
        }
    }

    #[test]
    fn credential_errors_carry_only_the_kind_and_the_provider_name() {
        let error = credential_error("p", CredentialFailure::Provider);
        assert_eq!(
            error,
            AwsError::CredentialProvider {
                provider: "p",
                message: "the credential provider failed".to_string(),
            }
        );
        assert_eq!(
            format!("{error:?}"),
            format!(
                "{:?}",
                AwsError::provider("p", "the credential provider failed")
            )
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn adopted_credentials_keep_their_parts_and_refuse_partial_or_expired_ones() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let later = now + Duration::from_secs(900);
        let adopted = adopt_credentials("FAKEKEY", "fake-secret", Some("tok"), Some(later), now)
            .expect("valid credentials");
        assert_eq!(adopted.access_key_id().expose(), "FAKEKEY");
        assert_eq!(adopted.secret_access_key().expose(), "fake-secret");
        assert_eq!(
            adopted.session_token().map(crate::redact::Secret::expose),
            Some("tok")
        );
        assert_eq!(adopted.expires_at(), Some(later));
        assert!(adopt_credentials("K", "S", None, None, now).is_ok());
        assert_eq!(
            adopt_credentials("K", "S", None, Some(now), now).unwrap_err(),
            AwsError::CredentialsExpired
        );
        assert_eq!(
            adopt_credentials("", "S", None, None, now).unwrap_err(),
            AwsError::PartialCredentials {
                missing: "access_key_id"
            }
        );
        assert_eq!(
            adopt_credentials("K", "", None, None, now).unwrap_err(),
            AwsError::PartialCredentials {
                missing: "secret_access_key"
            }
        );
    }

    #[test]
    fn connector_failures_map_to_retryable_or_refused_errors() {
        let connect = connector_error(
            ConnectorFailure::Timeout,
            "HTTP connect timeout occurred after 1s",
        );
        assert!(matches!(
            connect,
            AwsError::Timeout {
                phase: TimeoutPhase::Connect,
                ..
            }
        ));
        let read = connector_error(ConnectorFailure::Timeout, "HTTP read timeout occurred");
        assert!(matches!(
            read,
            AwsError::Timeout {
                phase: TimeoutPhase::Read,
                ..
            }
        ));
        for error in [&connect, &read] {
            assert!(error.is_retryable());
        }
        for kind in [ConnectorFailure::Io, ConnectorFailure::Other] {
            let error = connector_error(kind, LEAKY);
            assert_eq!(error.code(), "aws_connect", "{kind:?}");
            assert!(error.is_retryable(), "{kind:?}");
            assert_clean(&error);
        }
        let user = connector_error(ConnectorFailure::User, LEAKY);
        assert_eq!(user.code(), "aws_invalid_config");
        assert!(!user.is_retryable());
        assert_clean(&user);
    }

    #[test]
    fn timeout_phase_reads_the_connector_wording() {
        assert_eq!(timeout_phase("HTTP connect timeout"), TimeoutPhase::Connect);
        assert_eq!(timeout_phase("http CONNECT timeout"), TimeoutPhase::Connect);
        assert_eq!(timeout_phase("HTTP read timeout"), TimeoutPhase::Read);
        assert_eq!(timeout_phase(""), TimeoutPhase::Read);
    }

    #[test]
    fn the_guard_accepts_only_all_public_answers_and_returns_them() {
        let resolver = FixedResolver::new()
            .answer(
                "logs.us-east-1.amazonaws.com",
                &["52.94.0.10", "2600:1f18::1"],
            )
            .answer("evil.amazonaws.com", &["52.94.0.10", "169.254.169.254"])
            .answer("empty.amazonaws.com", &[]);
        let addresses = guarded_addresses("LOGS.us-east-1.amazonaws.com", &resolver).unwrap();
        assert_eq!(addresses.len(), 2);
        assert_eq!(resolver.lookups(), vec!["logs.us-east-1.amazonaws.com"]);

        let rejected = guarded_addresses("evil.amazonaws.com", &resolver).unwrap_err();
        assert_eq!(rejected.error().code(), "aws_invalid_config");
        assert!(!rejected.error().is_retryable());
        let shown = rejected.to_string();
        assert!(shown.contains("169.254.169.254"), "{shown}");
        assert!(shown.contains("link-local"), "{shown}");
        assert!(guarded_addresses("empty.amazonaws.com", &resolver).is_err());
        assert!(guarded_addresses("unknown.amazonaws.com", &resolver).is_err());
    }

    #[derive(Debug)]
    struct Wrapper(Box<dyn Error + Send + Sync>);

    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("failed to perform DNS lookup")
        }
    }

    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self.0.as_ref())
        }
    }

    #[test]
    fn a_guard_rejection_is_found_through_sources_and_io_payloads() {
        let resolver = FixedResolver::new().answer_any(&["10.0.0.1"]);
        let rejection = guarded_addresses("x.amazonaws.com", &resolver).unwrap_err();
        let direct = Wrapper(Box::new(rejection.clone()));
        assert_eq!(find_guard_rejection(&direct), Some(&rejection));
        let through_io = Wrapper(Box::new(std::io::Error::other(Wrapper(Box::new(
            rejection.clone(),
        )))));
        assert_eq!(find_guard_rejection(&through_io), Some(&rejection));
        let unrelated = Wrapper(Box::new(std::io::Error::other("connection refused")));
        assert_eq!(find_guard_rejection(&unrelated), None);
    }

    #[test]
    fn only_https_urls_without_userinfo_are_sent() {
        assert!(require_https("https://logs.us-east-1.amazonaws.com/").is_ok());
        assert!(require_https("HTTPS://logs.us-east-1.amazonaws.com:8443/x?y=z").is_ok());
        for bad in [
            "http://logs.us-east-1.amazonaws.com/",
            "logs.us-east-1.amazonaws.com/",
            "https:///path",
            "https://user:FAKEPASS@logs.us-east-1.amazonaws.com/",
            "file:///etc/passwd",
        ] {
            let error = require_https(bad).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{bad}");
            assert!(!error.to_string().contains("FAKEPASS"), "{error}");
            assert!(!error.to_string().contains("logs.us-east-1"), "{error}");
        }
    }

    fn literal(url: &str) -> Option<IpAddr> {
        literal_host_address(url).unwrap_or_else(|error| panic!("{url}: {error}"))
    }

    #[test]
    fn literal_hosts_are_read_in_every_form_a_resolver_accepts() {
        let v4 = |text: &str| Some(text.parse::<IpAddr>().unwrap());
        for (url, expected) in [
            ("https://127.0.0.1/", v4("127.0.0.1")),
            ("https://127.0.0.1:8443/x", v4("127.0.0.1")),
            ("https://127.0.0.1./", v4("127.0.0.1")),
            ("https://2130706433/", v4("127.0.0.1")),
            ("https://0177.0.0.1/", v4("127.0.0.1")),
            ("https://0x7f.0.0.1/", v4("127.0.0.1")),
            ("https://0x7F000001/", v4("127.0.0.1")),
            ("https://017700000001/", v4("127.0.0.1")),
            ("https://127.1/", v4("127.0.0.1")),
            ("https://10.1.258/", v4("10.1.1.2")),
            ("https://169.254.43518/", v4("169.254.169.254")),
            ("https://0/", v4("0.0.0.0")),
            ("https://[::1]/", v4("::1")),
            ("https://[::1]:443/", v4("::1")),
            ("https://[::ffff:127.0.0.1]/", v4("::ffff:127.0.0.1")),
            ("https://[::FFFF:7f00:1]/", v4("::ffff:127.0.0.1")),
            (
                "https://[64:ff9b::a9fe:a9fe]/",
                v4("64:ff9b::169.254.169.254"),
            ),
            ("https://[2600:1f18::1]/", v4("2600:1f18::1")),
            ("https://logs.us-east-1.amazonaws.com/", None),
            ("https://logs.us-east-1.amazonaws.com:443/a?b=c", None),
            ("https://127.0.0.1.nip.io/", None),
        ] {
            assert_eq!(literal(url), expected, "{url}");
        }
    }

    #[test]
    fn non_public_literal_hosts_are_refused_before_connecting() {
        for (url, block) in [
            ("https://127.0.0.1/", "loopback"),
            ("https://2130706433/", "loopback"),
            ("https://0177.0.0.1/", "loopback"),
            ("https://0x7f.1/", "loopback"),
            ("https://127.1/", "loopback"),
            ("https://10.0.0.1/", "private"),
            ("https://012.0.0.1/", "private"),
            ("https://169.254.169.254/", "link-local"),
            ("https://0xa9.0xfe.0xa9.0xfe/", "link-local"),
            ("https://2852039166/", "link-local"),
            ("https://0/", "this network"),
            ("https://[::1]/", "outside the global unicast range"),
            ("https://[::]/", "outside the global unicast range"),
            ("https://[fe80::1]/", "outside the global unicast range"),
            ("https://[fd00::1]/", "outside the global unicast range"),
            ("https://[::ffff:127.0.0.1]/", "loopback"),
            ("https://[::ffff:a9fe:a9fe]/", "link-local"),
            ("https://[64:ff9b::10.0.0.1]/", "private"),
            ("https://[::127.0.0.1]/", "outside the global unicast range"),
        ] {
            let error = require_public_host(url).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{url}");
            assert!(!error.is_retryable(), "{url}");
            let shown = error.to_string();
            assert!(shown.contains("non-public address"), "{url}: {shown}");
            assert!(shown.contains(block), "{url}: {shown}");
        }
        for url in [
            "https://52.94.0.10/",
            "https://[2600:1f18::1]:443/",
            "https://[::ffff:52.94.0.10]/",
            "https://[64:ff9b::52.94.0.10]/",
            "https://logs.us-east-1.amazonaws.com/",
        ] {
            assert!(require_public_host(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn hosts_that_cannot_be_read_unambiguously_are_refused() {
        for url in [
            "https://[fe80::1%25eth0]/",
            "https://[fe80::1%eth0]/",
            "https://[::1/",
            "https://[::1]x/",
            "https://[v1.fe80]/",
            "https://[127.0.0.1]/",
            "https://::1/",
            "https:///",
            "https://127.0.0.%31/",
            "https://evil_host.amazonaws.com/",
            "https://256.0.0.1/",
            "https://1.2.3.4.5/",
            "https://4294967296/",
            "https://08.0.0.1/",
            "https://example.0x/x",
            "https://.1/",
            "https://host:44a/",
            "https://host:123456/",
        ] {
            let error = literal_host_address(url).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{url}");
            assert!(require_public_host(url).is_err(), "{url}");
        }
    }

    #[test]
    fn non_public_socket_peers_are_refused() {
        for peer in ["127.0.0.1", "10.0.0.1", "::1", "::ffff:169.254.169.254"] {
            let error = require_public_peer(peer.parse().unwrap()).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{peer}");
            assert!(!error.is_retryable(), "{peer}");
            assert!(
                error.to_string().contains("the connection reached"),
                "{error}"
            );
        }
        assert!(require_public_peer("52.94.0.10".parse().unwrap()).is_ok());
        assert!(require_public_peer("2600:1f18::1".parse().unwrap()).is_ok());
    }

    #[test]
    fn wire_headers_are_copied_and_unacceptable_ones_dropped() {
        let headers = response_headers([
            ("X-Amzn-RequestId", "req-1"),
            ("content-type", "application/json"),
            ("bad header", "x"),
            ("x-injected", "a\r\nb"),
            ("x-amzn-requestid", "req-2"),
        ]);
        assert_eq!(headers.get("x-amzn-requestid"), Some("req-2"));
        assert_eq!(headers.get("content-type"), Some("application/json"));
        assert_eq!(headers.len(), 2);
    }
}
