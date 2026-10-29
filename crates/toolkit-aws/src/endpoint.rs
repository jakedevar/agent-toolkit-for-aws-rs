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
use crate::redact::{snippet, write_scrubbed_debug, Scrubbed};

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
#[derive(Clone, PartialEq, Eq)]
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

impl fmt::Debug for Endpoint {
    /// Scheme, host and port only. The base path is shown as its length,
    /// `<path: N bytes>`, never as text: validation proves where requests go,
    /// not that the path carries no secret (#4469). The operational URL comes
    /// from `Display`/[`Endpoint::url`], which this does not change.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            f.debug_struct("Endpoint")
                .field("scheme", &"https")
                .field("host", &Scrubbed(&self.host))
                .field("port", &self.port)
                .field("path", &format_args!("<path: {} bytes>", self.path.len()))
                .finish()
        })
    }
}

/// The operational URL (`https://host[:port][path]`) handed to the
/// transport. Diagnostics use `Debug`, which never prints the path.
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
        public_addresses(&host, resolver).map_err(reject)?;
    }
    Ok(Endpoint {
        host,
        port,
        path: tail.trim_end_matches('/').to_string(),
    })
}

/// Resolve `host` and return its addresses only if every answer is public
/// ([`is_non_public`]). This is the check [`validate_endpoint`] applies, and
/// the one the SDK transport applies again at connect time, so the addresses
/// it connects to are exactly the addresses that passed (no rebinding window).
///
/// # Errors
/// A failure description naming the host (and, for a non-public answer, the
/// address and its block) when the lookup fails, returns nothing, or returns
/// any non-public address.
pub fn public_addresses(host: &str, resolver: &dyn HostResolver) -> Result<Vec<IpAddr>, String> {
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
    if let Some(&address) = addresses.iter().find(|&&address| is_non_public(address)) {
        return Err(format!(
            "host {host} resolves to the non-public address {address} ({}). \
             Refusing to send credentials to a non-public address",
            non_public_reason(address)
        ));
    }
    Ok(addresses)
}

/// Special-purpose IPv4 blocks that are never a public AWS endpoint
/// (IANA IPv4 Special-Purpose Address Registry, RFC 6890 and successors).
/// Each entry is `(network, prefix length, what it is)`.
pub const NON_PUBLIC_V4: &[([u8; 4], u8, &str)] = &[
    ([0, 0, 0, 0], 8, "this network (RFC 1122)"),
    ([10, 0, 0, 0], 8, "private (RFC 1918)"),
    ([100, 64, 0, 0], 10, "carrier-grade NAT (RFC 6598)"),
    ([127, 0, 0, 0], 8, "loopback (RFC 1122)"),
    ([169, 254, 0, 0], 16, "link-local (RFC 3927)"),
    ([172, 16, 0, 0], 12, "private (RFC 1918)"),
    ([192, 0, 0, 0], 24, "IETF protocol assignments (RFC 6890)"),
    ([192, 0, 2, 0], 24, "documentation TEST-NET-1 (RFC 5737)"),
    ([192, 88, 99, 0], 24, "6to4 relay anycast (RFC 7526)"),
    ([192, 168, 0, 0], 16, "private (RFC 1918)"),
    ([198, 18, 0, 0], 15, "benchmarking (RFC 2544)"),
    ([198, 51, 100, 0], 24, "documentation TEST-NET-2 (RFC 5737)"),
    ([203, 0, 113, 0], 24, "documentation TEST-NET-3 (RFC 5737)"),
    ([224, 0, 0, 0], 4, "multicast (RFC 5771)"),
    (
        [240, 0, 0, 0],
        4,
        "reserved and limited broadcast (RFC 1112)",
    ),
];

/// IPv6 blocks inside the global-unicast space `2000::/3` that are not public
/// endpoints. Everything *outside* `2000::/3` is non-public as well (see
/// [`is_non_public_v6`]).
pub const NON_PUBLIC_V6_IN_GLOBAL: &[(u128, u8, &str)] = &[
    (
        0x2001_0000 << 96,
        23,
        "IETF protocol assignments incl. Teredo, benchmarking (RFC 2928)",
    ),
    (0x2001_0db8 << 96, 32, "documentation (RFC 3849)"),
    (0x2002_0000 << 96, 16, "6to4 (RFC 3056, deprecated)"),
    (0x3fff_0000 << 96, 20, "documentation (RFC 9637)"),
];

/// Whether `address` lies in `network/prefix`.
fn in_block(address: u128, network: u128, prefix: u8, width: u8) -> bool {
    let shift = u32::from(width - prefix);
    // A prefix of 0 would shift by the full width; no block here has one.
    (address >> shift) == (network >> shift)
}

/// Whether an address is not a publicly routable unicast address, so a signed
/// request must never be sent to it.
///
/// The policy is an explicit range list, applied to *every* answer (one
/// non-public answer rejects the host):
///
/// - IPv4: the special-purpose blocks in [`NON_PUBLIC_V4`] (private, loopback,
///   link-local, carrier-grade NAT, multicast, benchmarking `198.18.0.0/15`,
///   documentation, `0.0.0.0/8`, and `240.0.0.0/4` including the broadcast
///   address).
/// - IPv6: only the global-unicast block `2000::/3` can be public, minus the
///   blocks in [`NON_PUBLIC_V6_IN_GLOBAL`]. That makes `::`, `::1`,
///   `::2`-style reserved addresses, `100::/64`, `fc00::/7`, `fe80::/10`,
///   `ff00::/8` and the rest of the unallocated space non-public.
/// - IPv4-mapped (`::ffff:a.b.c.d`) addresses are judged by their IPv4 form.
///   The well-known NAT64 prefix `64:ff9b::/96` is judged by its embedded IPv4
///   address (a translated public address stays public); the local-use
///   `64:ff9b:1::/48` is non-public. IPv4-compatible `::/96` addresses are
///   deprecated and always non-public.
#[must_use]
pub fn is_non_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_non_public_v4(mapped);
            }
            if let Some(embedded) = nat64_embedded_v4(v6) {
                return is_non_public_v4(embedded);
            }
            is_non_public_v6(v6)
        }
    }
}

/// The IPv4 address embedded in a well-known NAT64 (`64:ff9b::/96`) address.
fn nat64_embedded_v4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = address.segments();
    (segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0]).then(|| {
        let [a, b] = segments[6].to_be_bytes();
        let [c, d] = segments[7].to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    })
}

fn is_non_public_v4(address: Ipv4Addr) -> bool {
    let value = u128::from(u32::from(address));
    NON_PUBLIC_V4.iter().any(|&(network, prefix, _)| {
        in_block(
            value,
            u128::from(u32::from(Ipv4Addr::from(network))),
            prefix,
            32,
        )
    })
}

fn is_non_public_v6(address: Ipv6Addr) -> bool {
    let value = u128::from(address);
    let global_unicast = in_block(value, 0x2000_u128 << 112, 3, 128);
    !global_unicast
        || NON_PUBLIC_V6_IN_GLOBAL
            .iter()
            .any(|&(network, prefix, _)| in_block(value, network, prefix, 128))
}

/// Name of the first non-public block containing `address`, for messages.
pub(crate) fn non_public_reason(address: IpAddr) -> &'static str {
    match address {
        IpAddr::V4(v4) => v4_reason(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return v4_reason(mapped);
            }
            if let Some(embedded) = nat64_embedded_v4(v6) {
                return v4_reason(embedded);
            }
            let value = u128::from(v6);
            NON_PUBLIC_V6_IN_GLOBAL
                .iter()
                .find(|&&(network, prefix, _)| in_block(value, network, prefix, 128))
                .map_or("outside the global unicast range", |&(_, _, what)| what)
        }
    }
}

fn v4_reason(address: Ipv4Addr) -> &'static str {
    let value = u128::from(u32::from(address));
    NON_PUBLIC_V4
        .iter()
        .find(|&&(network, prefix, _)| {
            in_block(
                value,
                u128::from(u32::from(Ipv4Addr::from(network))),
                prefix,
                32,
            )
        })
        .map_or("not publicly routable", |&(_, _, what)| what)
}

/// Service and region a request to `host` is signed for (the
/// `mcp-proxy-for-aws` rules, shared by `asm exec` and the MCP proxy):
/// `service.region.api.aws` gives `(service, region)`; a `bedrock-agentcore`
/// host under `amazonaws.com` gives that service and the region before it;
/// anything else gives the first label and no region (the caller then uses
/// its own region precedence).
#[must_use]
pub fn signing_service_region(host: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = host.split('.').collect();
    match parts.as_slice() {
        [_, .., "bedrock-agentcore", region, "amazonaws", "com"] => {
            ("bedrock-agentcore".to_string(), Some((*region).to_string()))
        }
        [service, region, "api", "aws"] => ((*service).to_string(), Some((*region).to_string())),
        _ => (
            parts.first().copied().unwrap_or("aws-mcp").to_string(),
            None,
        ),
    }
}

#[cfg(test)]
mod signing_scope_tests {
    use super::signing_service_region;

    #[test]
    fn signing_service_and_region_come_from_the_endpoint_host() {
        let some = |s: &str, r: &str| (s.to_string(), Some(r.to_string()));
        assert_eq!(
            signing_service_region("aws-mcp.us-east-1.api.aws"),
            some("aws-mcp", "us-east-1")
        );
        assert_eq!(
            signing_service_region("aidevops.us-west-2.api.aws"),
            some("aidevops", "us-west-2")
        );
        assert_eq!(
            signing_service_region("x.bedrock-agentcore.us-west-2.amazonaws.com"),
            some("bedrock-agentcore", "us-west-2")
        );
        assert_eq!(
            signing_service_region("custom.amazonaws.com"),
            ("custom".to_string(), None)
        );
        assert_eq!(
            signing_service_region("a.b.c.api.aws"),
            ("a".to_string(), None)
        );
        assert_eq!(signing_service_region(""), (String::new(), None));
    }
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

    fn ip(text: &str) -> IpAddr {
        text.parse()
            .unwrap_or_else(|_| panic!("test fixture {text:?} is not an IP address"))
    }

    fn rejected_by_validate(address: &str) -> bool {
        let resolver = FixedResolver::new().answer("x.amazonaws.com", &[address]);
        validate_endpoint(
            "https://x.amazonaws.com",
            "e",
            &EndpointPolicy::aws_public(),
            &resolver,
        )
        .is_err()
    }

    #[test]
    fn ipv4_special_purpose_blocks_are_rejected_at_both_edges() {
        // (first, last) of every block in the policy, spelled out independently
        // of the table in the code.
        let blocks = [
            ("0.0.0.0", "0.255.255.255"),
            ("10.0.0.0", "10.255.255.255"),
            ("100.64.0.0", "100.127.255.255"),
            ("127.0.0.0", "127.255.255.255"),
            ("169.254.0.0", "169.254.255.255"),
            ("172.16.0.0", "172.31.255.255"),
            ("192.0.0.0", "192.0.0.255"),
            ("192.0.2.0", "192.0.2.255"),
            ("192.88.99.0", "192.88.99.255"),
            ("192.168.0.0", "192.168.255.255"),
            ("198.18.0.0", "198.19.255.255"),
            ("198.51.100.0", "198.51.100.255"),
            ("203.0.113.0", "203.0.113.255"),
            ("224.0.0.0", "239.255.255.255"),
            ("240.0.0.0", "255.255.255.255"),
        ];
        assert_eq!(blocks.len(), NON_PUBLIC_V4.len());
        for (first, last) in blocks {
            for edge in [first, last] {
                assert!(is_non_public(ip(edge)), "{edge}");
                assert!(rejected_by_validate(edge), "{edge}");
                // Also as an IPv4-mapped IPv6 answer.
                let mapped = format!("::ffff:{edge}");
                assert!(is_non_public(ip(&mapped)), "{mapped}");
            }
        }
        for named in [
            "224.0.0.1",
            "198.18.0.1",
            "198.19.255.254",
            "255.255.255.255",
        ] {
            assert!(rejected_by_validate(named), "{named}");
        }
    }

    #[test]
    fn ipv4_neighbours_of_the_blocks_and_real_aws_addresses_stay_public() {
        for public in [
            "1.0.0.0",
            "9.255.255.255",
            "11.0.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "126.255.255.255",
            "128.0.0.0",
            "169.253.255.255",
            "169.255.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.1.0",
            "192.0.3.0",
            "192.88.98.255",
            "192.88.100.0",
            "192.167.255.255",
            "192.169.0.0",
            "198.17.255.255",
            "198.20.0.0",
            "198.51.99.255",
            "198.51.101.0",
            "203.0.112.255",
            "203.0.114.0",
            "223.255.255.255",
            "3.5.140.2",
            "8.8.8.8",
            "52.1.2.3",
            "54.239.28.85",
            "99.77.128.1",
        ] {
            assert!(!is_non_public(ip(public)), "{public}");
            assert!(!rejected_by_validate(public), "{public}");
            let mapped = format!("::ffff:{public}");
            assert!(!is_non_public(ip(&mapped)), "{mapped}");
        }
    }

    #[test]
    fn ipv6_reserved_special_purpose_and_unallocated_space_is_rejected() {
        for address in [
            // Unspecified, loopback and the rest of ::/96 (reserved,
            // IPv4-compatible - always rejected, even with a public tail).
            "::",
            "::1",
            "::2",
            "::3",
            "::8.8.8.8",
            "::52.1.2.3",
            // IPv4-translated / mapped forms of non-public IPv4.
            "::ffff:0:0",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:224.0.0.1",
            "::ffff:198.18.0.1",
            "::ffff:255.255.255.255",
            "::ffff:0:10.0.0.1",
            // NAT64: local-use prefix and embedded non-public IPv4.
            "64:ff9b:1::1",
            "64:ff9b::10.0.0.1",
            "64:ff9b::127.0.0.1",
            "64:ff9b::192.168.1.1",
            "64:ff9b::198.18.0.1",
            "64:ff9b::224.0.0.1",
            "64:ff9b::",
            // Discard-only, SRv6, unallocated and outside 2000::/3.
            "100::1",
            "5f00::1",
            "1fff:ffff::1",
            "4000::1",
            "8000::1",
            "e000::1",
            // Unique local, link-local, site-local, multicast.
            "fc00::1",
            "fd00::1",
            "fdff:ffff::1",
            "fe80::1",
            "febf:ffff::1",
            "fec0::1",
            "ff00::",
            "ff02::1",
            // Inside 2000::/3 but special purpose: edges of each block.
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:1ff:ffff::1",
            "2001:db8::1",
            "2001:db8:ffff:ffff::1",
            "2002::1",
            "2002:ffff::1",
            "2002:a00:1::1",
            "3fff::1",
            "3fff:fff:ffff::1",
        ] {
            assert!(is_non_public(ip(address)), "{address}");
            assert!(rejected_by_validate(address), "{address}");
        }
    }

    #[test]
    fn ipv6_public_unicast_and_translated_public_addresses_pass() {
        for address in [
            "2600:1f18::1",
            "2a05:d018::1",
            "2406:da00::1",
            "2001:200::1",
            "2001:4860:4860::8888",
            "2001:db7:ffff::1",
            "2001:db9::1",
            "2003::1",
            "3fff:1000::1",
            "2000::1",
            "::ffff:52.1.2.3",
            "64:ff9b::52.1.2.3",
            "64:ff9b::8.8.8.8",
        ] {
            assert!(!is_non_public(ip(address)), "{address}");
            assert!(!rejected_by_validate(address), "{address}");
        }
    }

    #[test]
    fn any_non_public_answer_rejects_the_host_whatever_its_position() {
        let policy = EndpointPolicy::aws_public();
        let mixes: [&[&str]; 5] = [
            &["52.1.2.3", "2600:1f18::1", "198.18.0.1"],
            &["224.0.0.1", "52.1.2.3"],
            &["52.1.2.3", "2001:db8::1"],
            &["2600:1f18::1", "::2"],
            &["52.1.2.3", "::ffff:10.0.0.1", "54.239.28.85"],
        ];
        for answers in mixes {
            let resolver = FixedResolver::new().answer("x.amazonaws.com", answers);
            let error = validate_endpoint("https://x.amazonaws.com", "e", &policy, &resolver)
                .expect_err(&answers.join(","));
            assert!(
                error.to_string().contains("non-public address"),
                "{answers:?}"
            );
        }
        let all_public = FixedResolver::new().answer(
            "x.amazonaws.com",
            &[
                "52.1.2.3",
                "2600:1f18::1",
                "54.239.28.85",
                "::ffff:99.77.128.1",
            ],
        );
        assert!(validate_endpoint("https://x.amazonaws.com", "e", &policy, &all_public).is_ok());
    }

    #[test]
    fn rejection_message_names_the_address_and_the_block() {
        let resolver = FixedResolver::new().answer("x.amazonaws.com", &["198.18.0.1"]);
        let text = validate_endpoint(
            "https://x.amazonaws.com",
            "e",
            &EndpointPolicy::aws_public(),
            &resolver,
        )
        .unwrap_err()
        .to_string();
        assert!(
            text.contains("198.18.0.1") && text.contains("benchmarking"),
            "{text}"
        );
        assert!(text.contains("Refusing to send credentials to a non-public address"));
        assert_eq!(
            non_public_reason(ip("2001:db8::1")),
            "documentation (RFC 3849)"
        );
        assert_eq!(
            non_public_reason(ip("::ffff:10.0.0.1")),
            "private (RFC 1918)"
        );
        assert_eq!(
            non_public_reason(ip("64:ff9b::224.0.0.1")),
            "multicast (RFC 5771)"
        );
        assert_eq!(
            non_public_reason(ip("fe80::1")),
            "outside the global unicast range"
        );
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

    /// Validated endpoints whose path carries synthetic secrets (labelled,
    /// scheme, userinfo-like and unlabelled): `(url, stored path, leaks)`.
    const SECRET_PATHS: &[(&str, &str, &[&str])] = &[
        (
            "https://logs.amazonaws.com/password=FAKE-PATH-PW1",
            "/password=FAKE-PATH-PW1",
            &["FAKE-PATH-PW1"],
        ),
        (
            "https://logs.amazonaws.com:8443/v1/token_value:FAKE-PATH-TK2/",
            "/v1/token_value:FAKE-PATH-TK2",
            &["FAKE-PATH-TK2"],
        ),
        (
            "https://logs.amazonaws.com/u:FAKE-PATH-PW3@x/Bearer%20FAKE-PATH-B4",
            "/u:FAKE-PATH-PW3@x/Bearer%20FAKE-PATH-B4",
            &["FAKE-PATH-PW3", "FAKE-PATH-B4"],
        ),
        (
            "https://logs.amazonaws.com/opaque/FAKEPATHSEGMENT5",
            "/opaque/FAKEPATHSEGMENT5",
            &["FAKEPATHSEGMENT5", "opaque"],
        ),
    ];

    #[test]
    fn endpoint_debug_shows_scheme_and_host_and_only_the_path_length() {
        // Re-review #4467 finding 3 (#4469): validation proves the
        // destination, not that the path is free of secrets.
        let resolver = FixedResolver::new().answer_any(&["54.239.28.85"]);
        for &(url, path, leaks) in SECRET_PATHS {
            let endpoint =
                validate_endpoint(url, "endpoint", &EndpointPolicy::aws_public(), &resolver)
                    .unwrap();
            // The operational URL still carries the real path.
            assert_eq!(endpoint.path(), path);
            assert!(endpoint.url().ends_with(path), "{url}");
            let marker = format!("<path: {} bytes>", path.len());
            let compact = format!("{endpoint:?}");
            assert_eq!(
                compact,
                format!(
                    "Endpoint {{ scheme: \"https\", host: \"logs.amazonaws.com\", \
                     port: {:?}, path: {marker} }}",
                    endpoint.port()
                )
            );
            for shown in [compact, format!("{endpoint:#?}")] {
                for leak in leaks {
                    assert!(!shown.contains(leak), "{leak:?} leaked: {shown}");
                }
                assert!(shown.contains(&marker), "{shown}");
                assert_eq!(crate::redact::redact_text(&shown), shown);
            }
        }
    }
}
