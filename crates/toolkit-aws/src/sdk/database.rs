//! The narrow database endpoint capability and its connector (R-DG-03 and
//! R-DG-07 of the docgraph live design; #5366, #5387).
//!
//! [`DatabaseEndpoint`] names exactly one AWS graph database host, its fixed
//! port and region, and the operator's private address ranges. It is the only
//! way to reach a private address with signed traffic: the public guard of
//! [`SdkTransport::new`] is unchanged, and so are the provider and payment
//! planes.
//!
//! [`SdkTransport::for_database`] builds a transport whose connector owns the
//! TCP dial, so admission happens **before any request byte is written**
//! (#5387):
//!
//! 1. the request URL must be `https`, without userinfo, naming the endpoint's
//!    exact host and port;
//! 2. the host is resolved afresh for every request (no pooling, so a
//!    reconnect is always revalidated); an empty answer, more than
//!    [`MAX_ANSWERS`] answers, or **any** answer outside the policy refuses
//!    the request before a dial (mixed answers are not filtered);
//! 3. only the validated answers are dialed, and the socket's actual peer
//!    must equal the dialed answer and be admitted by the policy; otherwise the
//!    socket is dropped with zero bytes written (not even a TLS hello);
//! 4. TLS (rustls, aws-lc, the cipher suites Smithy allows) trusts exactly the
//!    [`ValidatedRoots`] and verifies the original host name;
//! 5. one HTTP/1.1 exchange runs over that connection; the response body is
//!    bounded in bytes, idle time and the absolute operation deadline.
//!
//! Every phase runs inside one absolute deadline (R-DG-07): the earlier of the
//! operation budget from the start of the call and any caller deadline. All
//! waits are async and are dropped when the deadline fires, so no socket or
//! TLS work outlives it; a timeout after the request was handed to the
//! connection is reported as an unknown outcome, never as a rollback.
//!
//! Residual (recorded in docs/migration/docgraph.md): the production resolver
//! is the operating system's, run on Tokio's blocking pool. The deadline
//! abandons the lookup and discards its answer, but the blocked
//! `getaddrinfo` thread itself ends only when the system resolver returns.

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use aws_smithy_runtime_api::client::http::{
    HttpConnector, HttpConnectorFuture, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{
    HttpRequest as SdkHttpRequest, HttpResponse as SdkHttpResponse,
};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_types::body::SdkBody;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::Full;
use hyper_util::rt::TokioIo;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{Instant, Sleep};

use super::roots::ValidatedRoots;
use super::{SdkRuntime, SdkTransport};
use crate::binding::GuardRejection;
use crate::endpoint::is_non_public;
use crate::error::{AwsError, TimeoutPhase};
use crate::transport::{Timeouts, FRAMING_HEADERS};

/// Most private ranges one policy may list.
pub const MAX_PRIVATE_RANGES: usize = 16;
/// Most DNS answers accepted for the database host.
pub const MAX_ANSWERS: usize = 16;
/// Narrowest allowed IPv4 private range (a `/16` or longer prefix).
pub const MIN_V4_PREFIX: u8 = 16;
/// Narrowest allowed IPv6 ULA range (a `/48` or longer prefix).
pub const MIN_V6_PREFIX: u8 = 48;
/// The only DNS suffixes the capability admits: standard-partition Neptune
/// database and Neptune Analytics endpoints. Custom, FIPS and other-partition
/// forms are refused until reviewed.
const SERVICES: [&str; 2] = ["neptune", "neptune-graph"];
const PARTITION_SUFFIX: &str = "amazonaws.com";

/// RFC 1918 blocks a policy range must fall inside.
const PRIVATE_V4: [(Ipv4Addr, u8); 3] = [
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
];
/// IPv6 unique local addresses, `fc00::/7`.
const ULA: (u128, u8) = (0xfc00_u128 << 112, 7);

fn policy_error(reason: &str) -> AwsError {
    AwsError::invalid_config("network_policy", reason)
}

fn refusal(reason: impl AsRef<str>) -> AwsError {
    AwsError::invalid_config("endpoint", reason)
}

/// One operator-listed private range.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Range {
    network: IpAddr,
    prefix: u8,
}

impl Range {
    fn parse(text: &str) -> Result<Self, AwsError> {
        let (address, prefix) = text
            .split_once('/')
            .ok_or_else(|| policy_error("a private range must be written as ADDRESS/PREFIX"))?;
        let prefix: u8 = (!prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()))
            .then(|| prefix.parse().ok())
            .flatten()
            .ok_or_else(|| policy_error("a private range has an unreadable prefix length"))?;
        let network: IpAddr = address
            .parse()
            .map_err(|_| policy_error("a private range has an unreadable address"))?;
        let range = Self { network, prefix };
        match network {
            IpAddr::V4(v4) => {
                if !(MIN_V4_PREFIX..=32).contains(&prefix) {
                    return Err(policy_error(
                        "an IPv4 private range must have a prefix between /16 and /32",
                    ));
                }
                let inside = PRIVATE_V4.iter().any(|&(block, bits)| {
                    in_block(
                        u128::from(u32::from(v4)),
                        u128::from(u32::from(block)),
                        bits,
                        32,
                    )
                });
                if !inside {
                    return Err(policy_error(
                        "an IPv4 private range must lie inside 10/8, 172.16/12 or 192.168/16",
                    ));
                }
                if mask(u128::from(u32::from(v4)), prefix, 32) != u128::from(u32::from(v4)) {
                    return Err(policy_error("a private range has host bits set"));
                }
            }
            IpAddr::V6(v6) => {
                if v6.to_ipv4_mapped().is_some() {
                    return Err(policy_error(
                        "write an IPv4 private range in IPv4 form, not IPv4-mapped IPv6",
                    ));
                }
                if !(MIN_V6_PREFIX..=128).contains(&prefix) {
                    return Err(policy_error(
                        "an IPv6 private range must have a prefix between /48 and /128",
                    ));
                }
                if !in_block(u128::from(v6), ULA.0, ULA.1, 128) {
                    return Err(policy_error(
                        "an IPv6 private range must lie inside fc00::/7",
                    ));
                }
                if mask(u128::from(v6), prefix, 128) != u128::from(v6) {
                    return Err(policy_error("a private range has host bits set"));
                }
            }
        }
        Ok(range)
    }

    fn contains(self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => in_block(
                u128::from(u32::from(address)),
                u128::from(u32::from(network)),
                self.prefix,
                32,
            ),
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                in_block(u128::from(address), u128::from(network), self.prefix, 128)
            }
            _ => false,
        }
    }
}

fn mask(value: u128, prefix: u8, width: u8) -> u128 {
    if prefix == 0 {
        return 0;
    }
    let shift = u32::from(width - prefix);
    let all = if width == 128 {
        u128::MAX
    } else {
        (1_u128 << width) - 1
    };
    ((all >> shift) << shift) & value
}

fn in_block(value: u128, network: u128, prefix: u8, width: u8) -> bool {
    mask(value, prefix, width) == mask(network, prefix, width)
}

/// An IPv4-mapped IPv6 address as the IPv4 address it reaches.
fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        IpAddr::V4(_) => address,
    }
}

fn valid_region(region: &str) -> bool {
    let parts: Vec<&str> = region.split('-').collect();
    parts.len() >= 3
        && parts[0].len() == 2
        && parts[..parts.len() - 1]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase()))
        && parts[parts.len() - 1].len() <= 2
        && !parts[parts.len() - 1].is_empty()
        && parts[parts.len() - 1].bytes().all(|b| b.is_ascii_digit())
}

fn valid_host_labels(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The narrow database endpoint capability: one exact AWS graph database host
/// and port in one region, plus operator-listed private ranges. Built only
/// from explicit values (for docgraph, an explicit `--network-policy` file);
/// nothing in the environment widens it.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseEndpoint {
    host: String,
    port: u16,
    region: String,
    ranges: Vec<Range>,
}

impl DatabaseEndpoint {
    /// Validate a capability.
    ///
    /// The host must be lowercase, have no wildcard, trailing dot or
    /// userinfo, and end in `.<region>.neptune.amazonaws.com` or
    /// `.<region>.neptune-graph.amazonaws.com` with at least one label before;
    /// no label may mention FIPS. The region must look like `us-east-1` and
    /// match the host. The port is fixed and non-zero. Each private range must
    /// be canonical, inside RFC 1918 (prefix `/16` or longer) or `fc00::/7`
    /// (prefix `/48` or longer); at most [`MAX_PRIVATE_RANGES`].
    ///
    /// # Errors
    /// `InvalidConfig` (field `network_policy`) naming the broken rule; no
    /// supplied value is echoed.
    pub fn new(
        host: &str,
        port: u16,
        region: &str,
        private_ranges: &[&str],
    ) -> Result<Self, AwsError> {
        if !valid_region(region) {
            return Err(policy_error("the region is not a standard AWS region name"));
        }
        if !valid_host_labels(host) {
            return Err(policy_error(
                "the database host must be a lowercase DNS name without wildcards",
            ));
        }
        let labels: Vec<&str> = host.split('.').collect();
        if labels.iter().any(|label| label.contains("fips")) {
            return Err(policy_error(
                "FIPS database endpoints are not supported yet",
            ));
        }
        let suffix_ok = labels.len() >= 5
            && labels[labels.len() - 2..].join(".") == PARTITION_SUFFIX
            && SERVICES.contains(&labels[labels.len() - 3]);
        if !suffix_ok {
            return Err(policy_error(
                "the database host must be a standard Neptune or Neptune Analytics endpoint",
            ));
        }
        if labels[labels.len() - 4] != region {
            return Err(policy_error(
                "the database host names a different region than the policy",
            ));
        }
        if port == 0 {
            return Err(policy_error("the database port must be non-zero"));
        }
        if private_ranges.len() > MAX_PRIVATE_RANGES {
            return Err(policy_error("the policy lists too many private ranges"));
        }
        let ranges = private_ranges
            .iter()
            .map(|text| Range::parse(text))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            host: host.to_string(),
            port,
            region: region.to_string(),
            ranges,
        })
    }

    /// The exact database host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The fixed port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The region the host is in.
    #[must_use]
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Whether signed traffic may reach `address`: any public address (the
    /// shared public policy), or a private address inside a listed range.
    /// IPv4-mapped IPv6 is judged as the IPv4 address it reaches; loopback,
    /// link-local (including instance metadata), unspecified, multicast and
    /// every other non-public block can never be listed, so they are refused.
    #[must_use]
    pub fn admits(&self, address: IpAddr) -> bool {
        let address = canonical(address);
        !is_non_public(address) || self.ranges.iter().any(|range| range.contains(address))
    }

    fn check_answer(&self, address: IpAddr) -> Result<(), AwsError> {
        if self.admits(address) {
            Ok(())
        } else {
            Err(refusal(format!(
                "the database host resolved to {address}, outside the network policy. \
                 Refusing to send credentials to it"
            )))
        }
    }
}

impl fmt::Debug for DatabaseEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseEndpoint")
    }
}

/// Deadlines and response bounds for database traffic (R-DG-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseBounds {
    /// Budget for each of DNS, TCP connect and the TLS/HTTP handshake.
    pub connect: Duration,
    /// Longest wait for the response head or between body frames.
    pub idle: Duration,
    /// Absolute budget for one operation, from the start of the call to the
    /// end of the response body.
    pub operation: Duration,
    /// Most response body bytes accepted.
    pub max_response_bytes: u64,
}

impl Default for DatabaseBounds {
    /// The design defaults: 10 s connect phases, 30 s idle, 60 s per
    /// operation, 16 MiB per response.
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            idle: Duration::from_secs(30),
            operation: Duration::from_secs(60),
            max_response_bytes: 16 * 1024 * 1024,
        }
    }
}

/// A database host lookup that can be abandoned at the deadline.
pub type ResolveFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, String>> + Send + 'a>>;

/// Resolves the database host (async, so the deadline can drop the wait).
pub trait DatabaseResolver: Send + Sync {
    /// All addresses for `host`, or a failure description.
    fn resolve<'a>(&'a self, host: &'a str) -> ResolveFuture<'a>;
}

/// The operating system resolver on Tokio's blocking pool (see the module
/// residual note).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDatabaseResolver;

impl DatabaseResolver for SystemDatabaseResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> ResolveFuture<'a> {
        Box::pin(async move {
            tokio::net::lookup_host((host, 0_u16))
                .await
                .map(|answers| answers.map(|answer| answer.ip()).collect())
                .map_err(|error| error.to_string())
        })
    }
}

/// A connected byte stream.
pub(crate) trait Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Stream for T {}

/// A dialed connection and the peer its socket reports.
pub(crate) struct Dialed {
    pub(crate) stream: Box<dyn Stream>,
    pub(crate) peer: SocketAddr,
}

type DialFuture<'a> = Pin<Box<dyn Future<Output = std::io::Result<Dialed>> + Send + 'a>>;

/// Opens the connection to one validated answer. Production dials TCP; tests
/// substitute an in-memory recorder below the admission checks.
pub(crate) trait Dialer: Send + Sync {
    fn dial(&self, address: SocketAddr) -> DialFuture<'_>;
}

struct TcpDialer;

impl Dialer for TcpDialer {
    fn dial(&self, address: SocketAddr) -> DialFuture<'_> {
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect(address).await?;
            let peer = stream.peer_addr()?;
            Ok(Dialed {
                stream: Box::new(stream),
                peer,
            })
        })
    }
}

/// The cipher suites Smithy's rustls connector allows, so the database plane
/// negotiates exactly what the AWS service plane does.
const SUITES: [rustls::CipherSuite; 7] = [
    rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
    rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
    rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    rustls::CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
];

/// An explicit rustls (aws-lc) client context trusting exactly `roots`.
fn tls_config(roots: &ValidatedRoots) -> Result<rustls::ClientConfig, AwsError> {
    let roots_error =
        || AwsError::invalid_config("tls_roots", "the TLS context could not be built");
    let mut store = rustls::RootCertStore::empty();
    for der in roots.certificates() {
        store.add(der.clone()).map_err(|_| roots_error())?;
    }
    let base = rustls::crypto::aws_lc_rs::default_provider();
    let cipher_suites = SUITES
        .iter()
        .filter_map(|suite| {
            base.cipher_suites
                .iter()
                .find(|candidate| candidate.suite() == *suite)
                .copied()
        })
        .collect();
    let provider = rustls::crypto::CryptoProvider {
        cipher_suites,
        ..base
    };
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|_| roots_error())?
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

struct Shared {
    endpoint: DatabaseEndpoint,
    tls: tokio_rustls::TlsConnector,
    resolver: Arc<dyn DatabaseResolver>,
    dialer: Arc<dyn Dialer>,
    bounds: DatabaseBounds,
}

/// The pre-send guarded connector (one exchange per call, no pooling).
#[derive(Clone)]
struct DatabaseConnector {
    shared: Arc<Shared>,
    caller_deadline: Option<std::time::Instant>,
    connect: Duration,
    idle: Duration,
}

impl fmt::Debug for DatabaseConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseConnector")
    }
}

/// A boundary verdict carried through the connector error chain.
fn verdict(error: AwsError) -> ConnectorError {
    let timeout = matches!(error, AwsError::Timeout { .. });
    let carried: Box<dyn std::error::Error + Send + Sync> =
        Box::new(GuardRejection::carrying(error));
    if timeout {
        ConnectorError::timeout(carried)
    } else {
        ConnectorError::user(carried)
    }
}

fn deadline_error(sent: bool) -> AwsError {
    if sent {
        AwsError::timeout(
            TimeoutPhase::Deadline,
            "the operation deadline elapsed after the request was sent; \
             whether the database applied it is unknown",
        )
    } else {
        AwsError::timeout(
            TimeoutPhase::Deadline,
            "the operation deadline elapsed before the request was sent",
        )
    }
}

/// The request target: `Host` header value and origin-form path.
struct Target {
    authority: String,
    path: String,
}

impl DatabaseConnector {
    fn deadline(&self, start: Instant) -> Instant {
        let operation = start + self.shared.bounds.operation;
        self.caller_deadline
            .map_or(operation, |caller| operation.min(Instant::from_std(caller)))
    }

    /// Step 1: the URL names exactly the capability's host and port.
    fn admit(&self, request: &SdkHttpRequest) -> Result<Target, AwsError> {
        let uri: http::Uri = request
            .uri()
            .parse()
            .map_err(|_| AwsError::invalid_config("url", "the request URL could not be parsed"))?;
        if uri.scheme_str() != Some("https") {
            return Err(AwsError::invalid_config(
                "url",
                "only https URLs are sent. Refusing to send credentials over a non-HTTPS connection",
            ));
        }
        let authority = uri
            .authority()
            .ok_or_else(|| AwsError::invalid_config("url", "the URL must name a host"))?;
        if authority.as_str().contains('@') {
            return Err(AwsError::invalid_config(
                "url",
                "the URL must name a host and carry no userinfo (credentials)",
            ));
        }
        let endpoint = &self.shared.endpoint;
        if authority.host() != endpoint.host {
            return Err(refusal(
                "the request host is not the network policy's database host",
            ));
        }
        if authority.port_u16().unwrap_or(443) != endpoint.port {
            return Err(refusal(
                "the request port is not the network policy's database port",
            ));
        }
        for (name, _) in request.headers() {
            if FRAMING_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                return Err(AwsError::invalid_config(
                    "header",
                    "authority and framing headers are set by the transport",
                ));
            }
        }
        Ok(Target {
            authority: authority.as_str().to_string(),
            path: uri
                .path_and_query()
                .map_or_else(|| "/".to_string(), |path| path.as_str().to_string()),
        })
    }

    /// Step 2: resolve and validate every answer before any dial.
    async fn resolve(&self, deadline: Instant) -> Result<Vec<IpAddr>, ConnectorError> {
        let endpoint = &self.shared.endpoint;
        let bound = deadline.min(Instant::now() + self.connect);
        let answers = tokio::time::timeout_at(bound, self.shared.resolver.resolve(&endpoint.host))
            .await
            .map_err(|_| {
                verdict(AwsError::timeout(
                    TimeoutPhase::Connect,
                    "the database host was not resolved in time",
                ))
            })?
            .map_err(|_| verdict(refusal("the database host could not be resolved")))?;
        if answers.is_empty() {
            return Err(verdict(refusal("the database host has no addresses")));
        }
        if answers.len() > MAX_ANSWERS {
            return Err(verdict(refusal("the database host has too many addresses")));
        }
        let mut validated: Vec<IpAddr> = Vec::with_capacity(answers.len());
        for answer in answers {
            endpoint.check_answer(answer).map_err(verdict)?;
            let answer = canonical(answer);
            if !validated.contains(&answer) {
                validated.push(answer);
            }
        }
        Ok(validated)
    }

    /// Step 3: dial validated answers only; check the actual peer before any
    /// byte is written.
    async fn connect(
        &self,
        answers: &[IpAddr],
        deadline: Instant,
    ) -> Result<Box<dyn Stream>, ConnectorError> {
        let port = self.shared.endpoint.port;
        let mut last: Option<std::io::Error> = None;
        for &answer in answers {
            let dialed_address = SocketAddr::new(answer, port);
            let bound = deadline.min(Instant::now() + self.connect);
            let dialed =
                match tokio::time::timeout_at(bound, self.shared.dialer.dial(dialed_address)).await
                {
                    Err(_) => {
                        return Err(verdict(AwsError::timeout(
                            TimeoutPhase::Connect,
                            "HTTP connect to the database timed out",
                        )))
                    }
                    Ok(Err(error)) => {
                        last = Some(error);
                        continue;
                    }
                    Ok(Ok(dialed)) => dialed,
                };
            self.check_peer(dialed_address, dialed.peer)
                .map_err(verdict)?;
            return Ok(dialed.stream);
        }
        Err(ConnectorError::io(Box::new(last.unwrap_or_else(|| {
            std::io::Error::other("no database address accepted a connection")
        }))))
    }

    fn check_peer(&self, dialed: SocketAddr, peer: SocketAddr) -> Result<(), AwsError> {
        if canonical(peer.ip()) != canonical(dialed.ip()) || peer.port() != dialed.port() {
            return Err(refusal(
                "the connection reached a peer other than the validated database address. \
                 Refusing to send credentials to it",
            ));
        }
        if !self.shared.endpoint.admits(peer.ip()) {
            return Err(refusal(format!(
                "the connection reached {}, outside the network policy. \
                 Refusing to send credentials to it",
                peer.ip()
            )));
        }
        Ok(())
    }

    async fn exchange(
        &self,
        request: SdkHttpRequest,
        deadline: Instant,
        sent: Arc<AtomicBool>,
    ) -> Result<SdkHttpResponse, ConnectorError> {
        let target = self.admit(&request).map_err(verdict)?;
        let body = request.body().bytes().map(<[u8]>::to_vec).ok_or_else(|| {
            verdict(AwsError::invalid_config(
                "request",
                "streaming request bodies are not sent to a database endpoint",
            ))
        })?;
        let answers = self.resolve(deadline).await?;
        let stream = self.connect(&answers, deadline).await?;

        // Step 4: TLS to the original host name, trusting only the roots.
        let name = ServerName::try_from(self.shared.endpoint.host.clone())
            .map_err(|_| verdict(refusal("the database host is not a valid TLS name")))?;
        let bound = deadline.min(Instant::now() + self.connect);
        let tls = tokio::time::timeout_at(bound, self.shared.tls.connect(name, stream))
            .await
            .map_err(|_| {
                verdict(AwsError::timeout(
                    TimeoutPhase::Connect,
                    "HTTP connect to the database timed out during TLS",
                ))
            })?
            .map_err(|error| ConnectorError::io(Box::new(error)))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| ConnectorError::io(Box::new(error)))?;
        let driver = AbortOnDrop(tokio::spawn(async move {
            let _ = connection.await;
        }));

        // Step 5: one exchange.
        let mut builder = http::Request::builder()
            .method(request.method())
            .uri(target.path.as_str())
            .header("host", target.authority.as_str());
        for (name, value) in request.headers() {
            builder = builder.header(name, value);
        }
        let outgoing = builder.body(Full::new(Bytes::from(body))).map_err(|_| {
            verdict(AwsError::invalid_config(
                "request",
                "the request could not be built",
            ))
        })?;
        sent.store(true, Ordering::SeqCst);
        let response = tokio::time::timeout(self.idle, sender.send_request(outgoing))
            .await
            .map_err(|_| {
                verdict(AwsError::timeout(
                    TimeoutPhase::Read,
                    "no response head arrived in time after the request was sent; \
                     whether the database applied it is unknown",
                ))
            })?
            .map_err(|error| ConnectorError::io(Box::new(error)))?;
        let limit = self.shared.bounds.max_response_bytes;
        let declared = response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if declared.is_some_and(|length| length > limit) {
            return Err(verdict(AwsError::invalid_response(
                "the database response declares a body larger than the limit",
            )));
        }
        let (parts, incoming) = response.into_parts();
        let bounded = BoundedBody {
            inner: incoming,
            remaining: limit,
            idle: self.idle,
            idle_timer: Box::pin(tokio::time::sleep(self.idle)),
            deadline_timer: Box::pin(tokio::time::sleep_until(deadline)),
            _driver: driver,
        };
        let response = http::Response::from_parts(parts, SdkBody::from_body_1_x(bounded));
        SdkHttpResponse::try_from(response).map_err(|_| {
            verdict(AwsError::invalid_response(
                "the database response head could not be read",
            ))
        })
    }
}

impl HttpConnector for DatabaseConnector {
    fn call(&self, request: SdkHttpRequest) -> HttpConnectorFuture {
        let this = self.clone();
        HttpConnectorFuture::new(async move {
            let deadline = this.deadline(Instant::now());
            let sent = Arc::new(AtomicBool::new(false));
            match tokio::time::timeout_at(
                deadline,
                this.exchange(request, deadline, Arc::clone(&sent)),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(verdict(deadline_error(sent.load(Ordering::SeqCst)))),
            }
        })
    }
}

/// Aborts the HTTP connection driver when the response (or a failed call)
/// is dropped, so no socket work outlives its operation.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A body-bound failure (byte limit, idle time or operation deadline).
#[derive(Debug)]
struct BodyBoundError(&'static str);

impl fmt::Display for BodyBoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BodyBoundError {}

/// The response body under the byte limit, idle limit and deadline. Each
/// frame is checked against the remaining allowance before it is passed on.
struct BoundedBody {
    inner: hyper::body::Incoming,
    remaining: u64,
    idle: Duration,
    idle_timer: Pin<Box<Sleep>>,
    deadline_timer: Pin<Box<Sleep>>,
    _driver: AbortOnDrop,
}

impl Body for BoundedBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.deadline_timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Some(Err(Box::new(BodyBoundError(
                "the operation deadline elapsed while reading the database response",
            )))));
        }
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let length = frame
                    .data_ref()
                    .map_or(0, |data| u64::try_from(data.len()).unwrap_or(u64::MAX));
                if length > self.remaining {
                    self.remaining = 0;
                    return Poll::Ready(Some(Err(Box::new(BodyBoundError(
                        "the database response exceeded the size limit",
                    )))));
                }
                self.remaining -= length;
                let next = Instant::now() + self.idle;
                self.idle_timer.as_mut().reset(next);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                if self.idle_timer.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Some(Err(Box::new(BodyBoundError(
                        "the database response stalled",
                    )))));
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl SdkTransport {
    /// The database transport (R-DG-03): system DNS, TCP and TLS trusting
    /// exactly `roots`, admitting only `endpoint`. The public guard of
    /// [`SdkTransport::new`] and its connectors are not used or changed.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn for_database(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        endpoint: DatabaseEndpoint,
        bounds: DatabaseBounds,
    ) -> Result<Self, AwsError> {
        Self::for_database_with_resolver(
            runtime,
            roots,
            endpoint,
            bounds,
            Arc::new(SystemDatabaseResolver),
        )
    }

    /// [`Self::for_database`] with an injected resolver (the dial, peer check
    /// and TLS are unchanged).
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn for_database_with_resolver(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        endpoint: DatabaseEndpoint,
        bounds: DatabaseBounds,
        resolver: Arc<dyn DatabaseResolver>,
    ) -> Result<Self, AwsError> {
        Self::database(
            runtime,
            roots,
            endpoint,
            bounds,
            resolver,
            Arc::new(TcpDialer),
        )
    }

    pub(crate) fn database(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        endpoint: DatabaseEndpoint,
        bounds: DatabaseBounds,
        resolver: Arc<dyn DatabaseResolver>,
        dialer: Arc<dyn Dialer>,
    ) -> Result<Self, AwsError> {
        let shared = Arc::new(Shared {
            endpoint,
            tls: tokio_rustls::TlsConnector::from(Arc::new(tls_config(roots)?)),
            resolver,
            dialer,
            bounds,
        });
        let factory = move |timeouts: &Timeouts, deadline: Option<std::time::Instant>| {
            SharedHttpConnector::new(DatabaseConnector {
                shared: Arc::clone(&shared),
                caller_deadline: deadline,
                connect: bounds.connect.min(timeouts.connect()),
                idle: bounds.idle.min(timeouts.read()),
            })
        };
        Ok(Self {
            runtime: runtime.clone(),
            factory: Box::new(factory),
            connectors: Mutex::new(Vec::new()),
            roots: Some(roots.clone()),
            peer_check: true,
        })
    }
}

/// The server-side query timeout to send with an operation (T-DG-07c): the
/// remaining client budget less a `margin` for the response to travel back,
/// capped at `cap`; `None` when no useful budget remains, so the caller must
/// not send.
#[must_use]
pub fn server_query_timeout(
    remaining: Duration,
    margin: Duration,
    cap: Duration,
) -> Option<Duration> {
    remaining
        .checked_sub(margin)
        .filter(|left| !left.is_zero())
        .map(|left| left.min(cap))
}

#[cfg(test)]
mod tests;
