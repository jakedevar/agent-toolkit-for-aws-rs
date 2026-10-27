//! Endpoint validation: the guard between a configured URL and signed
//! requests.
//!
//! Port of the Python `launch_config._validate_base_url` rules, applied to every
//! override (flag, `AWS_ENDPOINT_URL_<SERVICE>`, `AWS_ENDPOINT_URL`, service
//! specific variables): `https` only, no userinfo, no query or fragment, a
//! restricted hostname, a host on the AWS domain allowlist, and (through an
//! injected [`HostResolver`]) a DNS answer containing only public addresses.
//! Loopback and private endpoints (for example `LocalStack`) are refused by the
//! default policy; that is the documented Python behavior.
//!
//! Residual risk, shared with the Python original: DNS may answer differently
//! when the SDK connects later (rebinding); the check narrows, not closes, that
//! window.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use crate::error::AwsError;
use crate::redact::snippet;

/// Hostname suffixes accepted by [`EndpointPolicy::aws_public`].
pub const DEFAULT_ALLOWED_SUFFIXES: &[&str] = &[".api.aws", ".amazonaws.com"];

/// Resolves a hostname to IP addresses; injected so tests do no DNS.
pub trait HostResolver {
    /// All addresses for `host`, or a failure description.
    ///
    /// # Errors
    /// Returns the resolver's failure text when the name does not resolve.
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
}

/// Resolver backed by the operating system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl HostResolver for SystemResolver {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        (host, 443_u16)
            .to_socket_addrs()
            .map(|addresses| addresses.map(|address| SocketAddr::ip(&address)).collect())
            .map_err(|error| ToString::to_string(&error))
    }
}

/// Which hosts may receive signed requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointPolicy {
    suffixes: &'static [&'static str],
    check_dns: bool,
}

impl EndpointPolicy {
    /// Official AWS domains only (`.api.aws`, `.amazonaws.com`), DNS checked.
    #[must_use]
    pub const fn aws_public() -> Self {
        Self {
            suffixes: DEFAULT_ALLOWED_SUFFIXES,
            check_dns: true,
        }
    }

    /// The same allowlist without the DNS answer check (synthesized or already
    /// resolved endpoints; the host rules still apply).
    #[must_use]
    pub const fn without_dns_check(self) -> Self {
        Self {
            suffixes: self.suffixes,
            check_dns: false,
        }
    }

    /// A different suffix allowlist (each entry starts with `.`).
    #[must_use]
    pub const fn with_suffixes(self, suffixes: &'static [&'static str]) -> Self {
        Self {
            suffixes,
            check_dns: self.check_dns,
        }
    }
}

/// A validated https endpoint: host, optional port and optional base path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    host: String,
    port: Option<u16>,
    path: String,
}

impl Endpoint {
    /// Lowercase hostname.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Explicit port, if any.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Base path without a trailing slash (empty or starting with `/`).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The URL, `https://host[:port][path]`, never carrying userinfo.
    #[must_use]
    pub fn url(&self) -> String {
        self.to_string()
    }

    /// The conventional endpoint `https://<service>.<region>.amazonaws.com`
    /// (`.amazonaws.com.cn` in `cn-` regions) for services whose endpoint
    /// prefix equals the service label.
    ///
    /// # Errors
    /// `InvalidConfig` when `service` is not a DNS label or `region` is not an
    /// AWS region name, so neither can inject host syntax.
    pub fn for_service(service: &str, region: &str) -> Result<Self, AwsError> {
        if !is_valid_service_label(service) {
            return Err(AwsError::invalid_config(
                "service",
                format!("{} is not a valid service label", snippet(service, 64)),
            ));
        }
        if !is_valid_region(region) {
            return Err(AwsError::invalid_config(
                "region",
                format!("{} is not a valid AWS region name", snippet(region, 64)),
            ));
        }
        let dns_suffix = if region.starts_with("cn-") {
            "amazonaws.com.cn"
        } else {
            "amazonaws.com"
        };
        Ok(Self {
            host: format!("{service}.{region}.{dns_suffix}"),
            port: None,
            path: String::new(),
        })
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "https://{}", self.host)?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        f.write_str(&self.path)
    }
}

/// Whether `region` has the shape of an AWS region name (`us-east-1`,
/// `us-gov-west-1`, `cn-north-1`): a two-letter prefix, lowercase words and a
/// numeric suffix, at most 32 characters.
#[must_use]
pub fn is_valid_region(region: &str) -> bool {
    if region.len() > 32 {
        return false;
    }
    let parts: Vec<&str> = region.split('-').collect();
    if parts.len() < 3 {
        return false;
    }
    let lower = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase());
    let first_ok = parts[0].len() == 2 && lower(parts[0]);
    let middle_ok = parts[1..parts.len() - 1].iter().all(|part| lower(part));
    let last = parts[parts.len() - 1];
    first_ok && middle_ok && !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `service` is a DNS label of lowercase letters, digits and hyphens.
#[must_use]
pub fn is_valid_service_label(service: &str) -> bool {
    !service.is_empty()
        && service.len() <= 63
        && !service.starts_with('-')
        && !service.ends_with('-')
        && service
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Validate an endpoint override.
///
/// `field` names the setting for error messages; the rejected URL is never
/// echoed (it may carry credentials).
///
/// # Errors
/// `InvalidConfig` naming `field` when the scheme is not https, the URL has
/// userinfo, a query, a fragment, a malformed host or path, a host outside the
/// policy allowlist, or (with the DNS check) a host that does not resolve or
/// resolves to a non-public address.
pub fn validate_endpoint(
    url: &str,
    field: &'static str,
    policy: &EndpointPolicy,
    resolver: &dyn HostResolver,
) -> Result<Endpoint, AwsError> {
    let reject = |reason: String| AwsError::invalid_config(field, reason);

    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(reject("expected an absolute https URL".to_string()));
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(reject(format!(
            "scheme must be https, got {}. Refusing to send credentials over a non-HTTPS connection",
            snippet(scheme, 16)
        )));
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if authority.contains('@') {
        return Err(reject(
            "userinfo (credentials) in the URL is not allowed".to_string(),
        ));
    }
    if tail.contains(['?', '#']) {
        return Err(reject(
            "query strings and fragments are not allowed".to_string(),
        ));
    }
    let (host_text, port_text) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host_text.is_empty() {
        return Err(reject("no hostname in the URL".to_string()));
    }
    let host = host_text.to_ascii_lowercase();
    let host_chars_ok = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty() && label.len() <= 63 && !label.starts_with('-') && !label.ends_with('-')
    });
    if !host_chars_ok || !labels_ok {
        return Err(reject("malformed hostname".to_string()));
    }
    let port = match port_text {
        None => None,
        Some(text) => match text.parse::<u16>() {
            Ok(port) if port != 0 => Some(port),
            _ => return Err(reject("port must be a number from 1 to 65535".to_string())),
        },
    };
    let allowed = policy
        .suffixes
        .iter()
        .any(|suffix| host == suffix.trim_start_matches('.') || host.ends_with(suffix));
    if !allowed {
        return Err(reject(format!(
            "host {} is not in the allowed domains ({}). Only official AWS endpoints are permitted",
            snippet(&host, 80),
            policy.suffixes.join(", ")
        )));
    }
    if tail
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        || tail.split('/').any(|segment| segment == "..")
    {
        return Err(reject("malformed path".to_string()));
    }
    if policy.check_dns {
        check_resolution(&host, resolver).map_err(reject)?;
    }
    Ok(Endpoint {
        host,
        port,
        path: tail.trim_end_matches('/').to_string(),
    })
}

fn check_resolution(host: &str, resolver: &dyn HostResolver) -> Result<(), String> {
    let addresses = resolver.resolve(host).map_err(|error| {
        format!(
            "DNS resolution failed for {host}: {}. Refusing to proceed with an unresolvable host",
            snippet(&error, 120)
        )
    })?;
    if addresses.is_empty() {
        return Err(format!(
            "DNS resolution returned no addresses for {host}. Refusing to proceed with an unresolvable host"
        ));
    }
    for address in addresses {
        if is_non_public(address) {
            return Err(format!(
                "host {host} resolves to the private, loopback or link-local address {address}. \
                 Refusing to send credentials to a non-public address"
            ));
        }
    }
    Ok(())
}

/// Whether an address is private, loopback, link-local, reserved or otherwise
/// not publicly routable (Python: `is_private`, `is_loopback`, `is_link_local`,
/// `is_reserved`, plus carrier-grade NAT and unspecified/multicast ranges).
#[must_use]
pub fn is_non_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_non_public_v4(mapped);
            }
            is_non_public_v6(v6)
        }
    }
}

fn is_non_public_v4(address: Ipv4Addr) -> bool {
    let [a, b, _, _] = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (b & 0xc0) == 64)
}

fn is_non_public_v6(address: Ipv6Addr) -> bool {
    let first = address.segments()[0];
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FixedResolver;

    fn public_resolver() -> FixedResolver {
        FixedResolver::new().answer_any(&["52.1.2.3"])
    }

    fn validate(url: &str) -> Result<Endpoint, AwsError> {
        validate_endpoint(
            url,
            "AWS_ENDPOINT_URL",
            &EndpointPolicy::aws_public(),
            &public_resolver(),
        )
    }

    fn reason(url: &str) -> String {
        validate(url).expect_err(url).to_string()
    }

    #[test]
    fn accepts_official_aws_hosts() {
        for (url, host) in [
            (
                "https://launch-with-aws.us-east-1.api.aws",
                "launch-with-aws.us-east-1.api.aws",
            ),
            (
                "https://logs.eu-central-1.amazonaws.com",
                "logs.eu-central-1.amazonaws.com",
            ),
            ("https://amazonaws.com", "amazonaws.com"),
            ("https://api.aws", "api.aws"),
            (
                "HTTPS://Logs.US-East-1.AmazonAWS.com",
                "logs.us-east-1.amazonaws.com",
            ),
        ] {
            let endpoint = validate(url).unwrap();
            assert_eq!(endpoint.host(), host, "{url}");
            assert_eq!(endpoint.port(), None);
            assert_eq!(endpoint.path(), "");
        }
    }

    #[test]
    fn trailing_slashes_are_stripped_and_paths_and_ports_kept() {
        let endpoint = validate("https://x.us-east-1.amazonaws.com/prod/api//").unwrap();
        assert_eq!(endpoint.path(), "/prod/api");
        assert_eq!(endpoint.url(), "https://x.us-east-1.amazonaws.com/prod/api");
        let endpoint = validate("https://x.us-east-1.amazonaws.com:8443/").unwrap();
        assert_eq!(endpoint.port(), Some(8443));
        assert_eq!(endpoint.url(), "https://x.us-east-1.amazonaws.com:8443");
    }

    #[test]
    fn rejects_non_https_and_non_urls() {
        assert!(reason("http://x.amazonaws.com").contains("scheme must be https, got http"));
        assert!(reason("ftp://x.amazonaws.com").contains("scheme must be https"));
        assert!(reason("x.amazonaws.com").contains("expected an absolute https URL"));
        assert!(reason("").contains("expected an absolute https URL"));
    }

    #[test]
    fn rejects_userinfo_query_and_fragment_without_echoing_them() {
        let text = reason("https://admin:hunter2@x.amazonaws.com/");
        assert!(text.contains("userinfo"));
        assert!(!text.contains("hunter2") && !text.contains("admin"));
        assert!(reason("https://x.amazonaws.com/?token=abc").contains("query strings"));
        assert!(reason("https://x.amazonaws.com/a#frag").contains("query strings"));
        assert!(reason("https://x.amazonaws.com?x=1").contains("query strings"));
    }

    #[test]
    fn rejects_hosts_outside_the_allowlist() {
        for url in [
            "https://evil.example.com",
            "https://evil-amazonaws.com",
            "https://amazonaws.com.evil.com",
            "https://amazonaws.com.",
            "https://x.amazonaws.com.cn",
            "https://127.0.0.1",
            "https://169.254.169.254",
        ] {
            let text = reason(url);
            assert!(
                text.contains("is not in the allowed domains")
                    || text.contains("malformed hostname"),
                "{url}: {text}"
            );
        }
    }

    #[test]
    fn rejects_malformed_hosts_ports_and_paths() {
        assert!(reason("https://[::1]").contains("malformed hostname"));
        assert!(reason("https:///x").contains("no hostname"));
        assert!(reason("https://a..amazonaws.com").contains("malformed hostname"));
        assert!(reason("https://-a.amazonaws.com").contains("malformed hostname"));
        assert!(reason("https://a b.amazonaws.com").contains("malformed hostname"));
        assert!(reason("https://x.amazonaws.com\\@evil.com").contains("userinfo"));
        assert!(reason("https://x.amazonaws.com:0").contains("port must be"));
        assert!(reason("https://x.amazonaws.com:65536").contains("port must be"));
        assert!(reason("https://x.amazonaws.com:abc").contains("port must be"));
        assert!(reason("https://x.amazonaws.com/a/../b").contains("malformed path"));
        assert!(reason("https://x.amazonaws.com/a b").contains("malformed path"));
        assert!(reason("https://x.amazonaws.com/a\\b").contains("malformed path"));
        assert!(reason("https://x.amazonaws.com/a\nb").contains("malformed path"));
    }

    #[test]
    fn errors_name_the_field_and_never_echo_the_url() {
        let error = validate("https://u:p@evil.example.com/").unwrap_err();
        assert!(error.to_string().starts_with("invalid AWS_ENDPOINT_URL: "));
        assert!(!error.to_string().contains("evil.example.com"));
    }

    #[test]
    fn dns_failures_and_empty_answers_are_rejected() {
        let policy = EndpointPolicy::aws_public();
        let unresolved = FixedResolver::new();
        let error =
            validate_endpoint("https://x.amazonaws.com", "e", &policy, &unresolved).unwrap_err();
        assert!(error
            .to_string()
            .contains("DNS resolution failed for x.amazonaws.com"));
        let empty = FixedResolver::new().answer("x.amazonaws.com", &[]);
        let error = validate_endpoint("https://x.amazonaws.com", "e", &policy, &empty).unwrap_err();
        assert!(error.to_string().contains("returned no addresses"));
    }

    #[test]
    fn private_loopback_and_reserved_answers_are_rejected() {
        let policy = EndpointPolicy::aws_public();
        for address in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
        ] {
            let resolver = FixedResolver::new().answer("x.amazonaws.com", &[address]);
            let error = validate_endpoint("https://x.amazonaws.com", "e", &policy, &resolver)
                .expect_err(address);
            assert!(
                error.to_string().contains("non-public address"),
                "{address}"
            );
        }
    }

    #[test]
    fn one_private_answer_among_public_ones_is_rejected() {
        let resolver = FixedResolver::new().answer("x.amazonaws.com", &["52.1.2.3", "10.1.1.1"]);
        let result = validate_endpoint(
            "https://x.amazonaws.com",
            "e",
            &EndpointPolicy::aws_public(),
            &resolver,
        );
        assert!(result.is_err());
    }

    #[test]
    fn public_answers_pass_including_ipv6_and_mapped() {
        for address in ["52.1.2.3", "8.8.8.8", "2600:1f18::1", "::ffff:52.1.2.3"] {
            let resolver = FixedResolver::new().answer("x.amazonaws.com", &[address]);
            assert!(
                validate_endpoint(
                    "https://x.amazonaws.com",
                    "e",
                    &EndpointPolicy::aws_public(),
                    &resolver
                )
                .is_ok(),
                "{address}"
            );
        }
    }

    #[test]
    fn without_dns_check_never_calls_the_resolver() {
        let resolver = FixedResolver::new();
        let policy = EndpointPolicy::aws_public().without_dns_check();
        assert!(validate_endpoint("https://x.amazonaws.com", "e", &policy, &resolver).is_ok());
        assert!(resolver.lookups().is_empty());
        // Host rules still apply.
        assert!(validate_endpoint("https://evil.example.com", "e", &policy, &resolver).is_err());
    }

    #[test]
    fn resolver_is_asked_once_with_the_lowercase_host() {
        let resolver = FixedResolver::new().answer_any(&["52.1.2.3"]);
        validate_endpoint(
            "https://X.Amazonaws.com",
            "e",
            &EndpointPolicy::aws_public(),
            &resolver,
        )
        .unwrap();
        assert_eq!(resolver.lookups(), vec!["x.amazonaws.com".to_string()]);
    }

    #[test]
    fn custom_suffix_allowlists_replace_the_default() {
        static CN: &[&str] = &[".amazonaws.com.cn"];
        let policy = EndpointPolicy::aws_public()
            .with_suffixes(CN)
            .without_dns_check();
        let resolver = FixedResolver::new();
        assert!(validate_endpoint(
            "https://x.cn-north-1.amazonaws.com.cn",
            "e",
            &policy,
            &resolver
        )
        .is_ok());
        assert!(
            validate_endpoint("https://x.us-east-1.amazonaws.com", "e", &policy, &resolver)
                .is_err()
        );
    }

    #[test]
    fn region_shapes() {
        for good in [
            "us-east-1",
            "eu-central-1",
            "us-gov-west-1",
            "cn-north-1",
            "us-iso-east-1",
            "ap-southeast-2",
        ] {
            assert!(is_valid_region(good), "{good}");
        }
        for bad in [
            "",
            "us",
            "us-east",
            "us-east-1a",
            "US-EAST-1",
            "us_east_1",
            "u-east-1",
            "usa-east-1",
            "us--1",
            "-east-1",
            "us-east-",
            "us-east-1.evil.com",
            "us-east-1/x",
            "aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-1",
        ] {
            assert!(!is_valid_region(bad), "{bad}");
        }
    }

    #[test]
    fn service_labels_and_synthesized_endpoints() {
        assert!(is_valid_service_label("logs"));
        assert!(is_valid_service_label("bedrock-agent"));
        let long = "a".repeat(64);
        for bad in [
            "",
            "-logs",
            "logs-",
            "Logs",
            "a.b",
            "a/b",
            "a b",
            long.as_str(),
        ] {
            assert!(!is_valid_service_label(bad), "{bad}");
        }
        let endpoint = Endpoint::for_service("logs", "us-east-1").unwrap();
        assert_eq!(endpoint.url(), "https://logs.us-east-1.amazonaws.com");
        let china = Endpoint::for_service("logs", "cn-north-1").unwrap();
        assert_eq!(china.url(), "https://logs.cn-north-1.amazonaws.com.cn");
        assert!(Endpoint::for_service("a.b", "us-east-1").is_err());
        assert!(Endpoint::for_service("logs", "us-east-1.evil.com").is_err());
    }
}
