//! Offline tests of the database capability and its pre-send guarded
//! connector (T-DG-03a/c/d, T-DG-07a/b/c; #5366, #5387). Nothing here opens a
//! socket or resolves a name: the resolver is injected, and the dialer is an
//! in-memory recorder **below** the admission checks that counts every byte
//! the connector writes and can run a rustls server on the far end.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadBuf};

use super::*;
use crate::sdk::test_support::Pki;
use crate::transport::{Headers, HttpRequest, Method, Transport};

const HOST: &str = "db.cluster-abc123.us-east-1.neptune.amazonaws.com";
const PORT: u16 = 8182;
const PRIVATE: &str = "10.1.2.3";

fn ip(text: &str) -> IpAddr {
    text.parse().expect("test address")
}

fn endpoint() -> DatabaseEndpoint {
    DatabaseEndpoint::new(
        HOST,
        PORT,
        "us-east-1",
        &["10.1.0.0/16", "fd12:3456:789a::/48"],
    )
    .expect("valid policy")
}

fn request(url: &str) -> HttpRequest {
    let mut headers = Headers::new();
    headers
        .insert("authorization", "AWS4-HMAC-SHA256 synthetic")
        .expect("header");
    HttpRequest {
        method: Method::Post,
        url: url.to_string(),
        headers,
        body: b"{\"gremlin\":\"g.V().count()\"}".to_vec(),
        timeouts: Timeouts::default(),
    }
}

fn db_url() -> String {
    format!("https://{HOST}:{PORT}/gremlin")
}

/// Answers per call (the last repeats); counts calls.
struct ScriptedResolver {
    answers: Mutex<VecDeque<Vec<IpAddr>>>,
    calls: AtomicUsize,
    stall: bool,
}

impl ScriptedResolver {
    fn new(answers: Vec<Vec<IpAddr>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            calls: AtomicUsize::new(0),
            stall: false,
        })
    }

    fn stalled() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
            stall: true,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl DatabaseResolver for ScriptedResolver {
    fn resolve<'a>(&'a self, _host: &'a str) -> ResolveFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall {
            return Box::pin(std::future::pending());
        }
        let mut answers = self.answers.lock().expect("answers");
        let next = if answers.len() > 1 {
            answers.pop_front().expect("answer")
        } else {
            answers.front().cloned().unwrap_or_default()
        };
        Box::pin(async move { Ok(next) })
    }
}

/// What the far end of a dialed connection does.
#[derive(Clone)]
enum Far {
    /// Nothing: the far end only counts what arrives.
    Silent,
    /// A rustls server presenting `chain`; after reading one request it runs
    /// `acts`.
    Tls {
        chain: Vec<CertificateDer<'static>>,
        key: Arc<PrivateKeyDer<'static>>,
        acts: Vec<Act>,
    },
}

#[derive(Clone)]
enum Act {
    Write(Vec<u8>),
    Sleep(Duration),
}

/// The zero-byte recorder: every dial, every byte the connector writes.
struct Recorder {
    /// The peer the socket reports (`None`: the dialed address).
    peer: Option<SocketAddr>,
    stall: bool,
    far: Far,
    dials: Mutex<Vec<SocketAddr>>,
    written: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<u8>>>,
    /// Far ends that have finished (the connection closed under them).
    closed: Arc<AtomicUsize>,
    /// Client ends (the connector's socket) that have been dropped (#5429).
    dropped: Arc<AtomicUsize>,
}

impl Recorder {
    fn new(far: Far) -> Arc<Self> {
        Arc::new(Self {
            peer: None,
            stall: false,
            far,
            dials: Mutex::new(Vec::new()),
            written: Arc::new(AtomicUsize::new(0)),
            received: Arc::new(Mutex::new(Vec::new())),
            closed: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn reporting_peer(peer: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            peer: Some(peer),
            stall: false,
            far: Far::Silent,
            dials: Mutex::new(Vec::new()),
            written: Arc::new(AtomicUsize::new(0)),
            received: Arc::new(Mutex::new(Vec::new())),
            closed: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn stalled() -> Arc<Self> {
        Arc::new(Self {
            peer: None,
            stall: true,
            far: Far::Silent,
            dials: Mutex::new(Vec::new()),
            written: Arc::new(AtomicUsize::new(0)),
            received: Arc::new(Mutex::new(Vec::new())),
            closed: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn dials(&self) -> Vec<SocketAddr> {
        self.dials.lock().expect("dials").clone()
    }

    fn written(&self) -> usize {
        self.written.load(Ordering::SeqCst)
    }

    fn received(&self) -> Vec<u8> {
        self.received.lock().expect("received").clone()
    }

    /// Whether every dialed socket has been released (dropped by the
    /// connector), read without driving any runtime.
    fn released(&self) -> bool {
        self.dropped.load(Ordering::SeqCst) == self.dials().len()
    }
}

/// The connector's end of the in-memory connection, counting writes.
struct Counting {
    inner: DuplexStream,
    written: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl Drop for Counting {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl AsyncRead for Counting {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Counting {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(count)) = result {
            self.written.fetch_add(count, Ordering::SeqCst);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl Dialer for Recorder {
    fn dial(&self, address: SocketAddr) -> DialFuture<'_> {
        self.dials.lock().expect("dials").push(address);
        if self.stall {
            return Box::pin(std::future::pending());
        }
        let (client, server) = tokio::io::duplex(1 << 20);
        let received = Arc::clone(&self.received);
        let far = self.far.clone();
        let closed = Arc::clone(&self.closed);
        tokio::spawn(async move {
            serve(far, server, received).await;
            closed.fetch_add(1, Ordering::SeqCst);
        });
        let stream = Counting {
            inner: client,
            written: Arc::clone(&self.written),
            dropped: Arc::clone(&self.dropped),
        };
        let peer = self.peer.unwrap_or(address);
        Box::pin(async move {
            Ok(Dialed {
                stream: Box::new(stream),
                peer,
            })
        })
    }
}

fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

async fn serve(far: Far, mut server: DuplexStream, received: Arc<Mutex<Vec<u8>>>) {
    let Far::Tls { chain, key, acts } = far else {
        let mut sink = Vec::new();
        let _ = server.read_to_end(&mut sink).await;
        received.lock().expect("received").extend(sink);
        return;
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(chain, key.clone_key())
        .expect("server certificate");
    let Ok(mut tls) = tokio_rustls::TlsAcceptor::from(Arc::new(config))
        .accept(server)
        .await
    else {
        return;
    };
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(end) = head_end(&buffer) {
            if buffer.len() >= end + content_length(&buffer[..end]) {
                break;
            }
        }
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    }
    received.lock().expect("received").extend(&buffer);
    for act in acts {
        match act {
            Act::Write(bytes) => {
                if tls.write_all(&bytes).await.is_err() || tls.flush().await.is_err() {
                    return;
                }
            }
            Act::Sleep(pause) => tokio::time::sleep(pause).await,
        }
    }
    let mut rest = Vec::new();
    let _ = tls.read_to_end(&mut rest).await;
}

fn ok_reply(body: &str) -> Act {
    Act::Write(
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

struct Fixture {
    pki: Pki,
    runtime: SdkRuntime,
}

impl Fixture {
    fn new() -> Self {
        Self {
            pki: Pki::new("docgraph test CA"),
            runtime: SdkRuntime::new().expect("runtime"),
        }
    }

    fn roots(&self) -> ValidatedRoots {
        ValidatedRoots::for_tests(vec![self.pki.ca_der.clone()])
    }

    fn tls_far(&self, acts: Vec<Act>) -> Far {
        let (chain, key) = self.pki.server(HOST);
        Far::Tls {
            chain,
            key: Arc::new(key),
            acts,
        }
    }

    fn transport(
        &self,
        bounds: DatabaseBounds,
        resolver: Arc<ScriptedResolver>,
        dialer: Arc<Recorder>,
    ) -> SdkTransport {
        self.transport_for(endpoint(), bounds, resolver, dialer)
    }

    fn transport_for(
        &self,
        policy: DatabaseEndpoint,
        bounds: DatabaseBounds,
        resolver: Arc<ScriptedResolver>,
        dialer: Arc<Recorder>,
    ) -> SdkTransport {
        SdkTransport::database(
            &self.runtime,
            &self.roots(),
            policy,
            bounds,
            resolver,
            dialer,
        )
        .expect("database transport")
    }
}

fn quick() -> DatabaseBounds {
    DatabaseBounds {
        connect: Duration::from_millis(300),
        idle: Duration::from_millis(300),
        operation: Duration::from_millis(900),
        max_response_bytes: 64,
    }
}

fn endpoint_refusal(error: &AwsError) -> bool {
    matches!(error, AwsError::InvalidConfig { field, .. } if *field == "endpoint")
}

fn message(error: &AwsError) -> String {
    error.to_string()
}

// ---- policy (T-DG-03c malformed policy, region/host/port mismatch) ----

#[test]
fn the_policy_accepts_standard_neptune_endpoints_only() {
    for host in [
        HOST,
        "db.cluster-ro-abc123.us-east-1.neptune.amazonaws.com",
        "g-abc123.us-east-1.neptune-graph.amazonaws.com",
    ] {
        DatabaseEndpoint::new(host, 8182, "us-east-1", &[]).expect("standard endpoint");
    }
    for host in [
        "*.us-east-1.neptune.amazonaws.com",
        "DB.cluster-abc.us-east-1.neptune.amazonaws.com",
        "db.cluster-abc.us-east-1.neptune.amazonaws.com.",
        "db.example.com",
        "us-east-1.neptune.amazonaws.com",
        "db.cluster-abc.us-east-1.neptune-fips.amazonaws.com",
        "db.cluster-abc.us-east-1.neptune.amazonaws.com.cn",
        "db.cluster-abc.us-east-1.docdb.amazonaws.com",
        "user@db.cluster-abc.us-east-1.neptune.amazonaws.com",
        "db.cluster-abc.us-east-1.neptune.amazonaws.com:8182",
        "10.1.2.3",
        "",
    ] {
        let error = DatabaseEndpoint::new(host, 8182, "us-east-1", &[]).expect_err(host);
        assert!(
            matches!(&error, AwsError::InvalidConfig { field, .. } if *field == "network_policy"),
            "{host}"
        );
        if !host.is_empty() {
            assert!(!message(&error).contains(host), "the host is not echoed");
        }
    }
}

#[test]
fn the_policy_refuses_region_mismatch_bad_region_and_port_zero() {
    let mismatch = DatabaseEndpoint::new(HOST, PORT, "us-west-2", &[]).expect_err("mismatch");
    assert!(message(&mismatch).contains("different region"));
    for region in ["", "us-east", "useast1", "US-EAST-1", "us-east-123"] {
        DatabaseEndpoint::new(HOST, PORT, region, &[]).expect_err(region);
    }
    DatabaseEndpoint::new(HOST, 0, "us-east-1", &[]).expect_err("port zero");
}

#[test]
fn private_ranges_must_be_narrow_canonical_rfc1918_or_ula() {
    for good in [
        "10.1.0.0/16",
        "172.16.4.0/24",
        "192.168.1.7/32",
        "fd00:1:2::/48",
        "fdff::1/128",
    ] {
        DatabaseEndpoint::new(HOST, PORT, "us-east-1", &[good]).expect(good);
    }
    for bad in [
        "10.0.0.0/8",
        "0.0.0.0/0",
        "127.0.0.0/16",
        "169.254.169.254/32",
        "100.64.0.0/16",
        "8.8.8.0/24",
        "10.1.0.1/16",
        "10.1.0.0/33",
        "10.1.0.0",
        "10.1.0.0/",
        "10.1.0.0/+16",
        "::ffff:10.1.0.0/112",
        "fd00::/8",
        "fe80::/64",
        "2001:db8::/48",
        "::1/128",
        "ff02::/16",
        "not-an-address/16",
    ] {
        let error = DatabaseEndpoint::new(HOST, PORT, "us-east-1", &[bad]).expect_err(bad);
        assert!(
            matches!(&error, AwsError::InvalidConfig { field, .. } if *field == "network_policy"),
            "{bad}"
        );
    }
    let many: Vec<String> = (0..=MAX_PRIVATE_RANGES)
        .map(|i| format!("10.{i}.0.0/16"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    DatabaseEndpoint::new(HOST, PORT, "us-east-1", &many).expect_err("too many ranges");
}

#[test]
fn admission_covers_public_listed_private_and_mapped_addresses_only() {
    let policy = endpoint();
    for admitted in [
        "52.94.1.1",
        PRIVATE,
        "10.1.255.255",
        "::ffff:10.1.2.3",
        "fd12:3456:789a::5",
        "2600:1f18::1",
    ] {
        assert!(policy.admits(ip(admitted)), "{admitted}");
    }
    for refused in [
        "10.2.0.1",
        "192.168.1.1",
        "127.0.0.1",
        "::ffff:127.0.0.1",
        "169.254.169.254",
        "::ffff:169.254.169.254",
        "fd00:ec2::254",
        "0.0.0.0",
        "::",
        "::1",
        "224.0.0.1",
        "ff02::1",
        "fe80::1",
        "64:ff9b::a01:203",
        "fd12:3456:789b::5",
    ] {
        assert!(!policy.admits(ip(refused)), "{refused}");
    }
}

// ---- pre-send peer check: the zero-byte recorder (#5387) ----

#[test]
fn a_forbidden_connected_peer_receives_zero_bytes() {
    let fixture = Fixture::new();
    for peer in ["10.9.9.9", "127.0.0.1", "169.254.169.254", "10.1.2.4"] {
        let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
        let dialer = Recorder::reporting_peer(SocketAddr::new(ip(peer), PORT));
        let transport = fixture.transport(quick(), Arc::clone(&resolver), Arc::clone(&dialer));
        let error = transport.send(&request(&db_url())).expect_err(peer);
        assert!(endpoint_refusal(&error), "{peer}: {error}");
        assert!(!error.is_retryable(), "{peer}");
        assert_eq!(dialer.dials(), vec![SocketAddr::new(ip(PRIVATE), PORT)]);
        assert_eq!(dialer.written(), 0, "{peer}: no byte, not even a TLS hello");
        assert!(dialer.received().is_empty(), "{peer}");
    }
}

#[test]
fn a_peer_on_another_port_receives_zero_bytes() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::reporting_peer(SocketAddr::new(ip(PRIVATE), 443));
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    let error = transport.send(&request(&db_url())).expect_err("port");
    assert!(endpoint_refusal(&error), "{error}");
    assert_eq!(dialer.written(), 0);
}

#[test]
fn forbidden_or_mixed_answers_are_never_dialed() {
    let fixture = Fixture::new();
    for answers in [
        vec![ip("169.254.169.254")],
        vec![ip("127.0.0.1")],
        vec![ip(PRIVATE), ip("127.0.0.1")],
        vec![ip("52.94.1.1"), ip("10.2.0.1")],
        vec![ip("::ffff:127.0.0.1")],
        vec![ip("64:ff9b::a01:203")],
        vec![],
        (0..=MAX_ANSWERS)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 1, 0, u8::try_from(i).expect("small"))))
            .collect(),
    ] {
        let resolver = ScriptedResolver::new(vec![answers.clone()]);
        let dialer = Recorder::new(Far::Silent);
        let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
        let error = transport.send(&request(&db_url())).expect_err("refused");
        assert!(endpoint_refusal(&error), "{answers:?}: {error}");
        assert!(dialer.dials().is_empty(), "{answers:?}");
        assert_eq!(dialer.written(), 0);
    }
}

#[test]
fn every_request_revalidates_so_rebinding_is_refused() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)], vec![ip("127.0.0.1")]]);
    let dialer = Recorder::new(fixture.tls_far(vec![ok_reply("{}"), ok_reply("{}")]));
    let transport = fixture.transport(quick(), Arc::clone(&resolver), Arc::clone(&dialer));
    let first = transport.send(&request(&db_url())).expect("first request");
    assert_eq!(first.status, 200);
    let written = dialer.written();
    let error = transport.send(&request(&db_url())).expect_err("rebound");
    assert!(endpoint_refusal(&error), "{error}");
    assert_eq!(
        resolver.calls(),
        2,
        "the host is resolved for every request"
    );
    assert_eq!(
        dialer.dials().len(),
        1,
        "the rebound answer is never dialed"
    );
    assert_eq!(dialer.written(), written);
}

// ---- URL admission (T-DG-03d) ----

#[test]
fn requests_outside_the_capability_never_resolve_or_dial() {
    let fixture = Fixture::new();
    for url in [
        format!("http://{HOST}:{PORT}/gremlin"),
        format!("https://user:pw@{HOST}:{PORT}/gremlin"),
        format!("https://{HOST}/gremlin"),
        format!("https://{HOST}:443/gremlin"),
        format!("https://other.cluster-abc123.us-east-1.neptune.amazonaws.com:{PORT}/"),
        format!("https://{PRIVATE}:{PORT}/gremlin"),
        "https://neptune.us-east-1.amazonaws.com/".to_string(),
    ] {
        let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
        let dialer = Recorder::new(Far::Silent);
        let transport = fixture.transport(quick(), Arc::clone(&resolver), Arc::clone(&dialer));
        let error = transport.send(&request(&url)).expect_err(&url);
        assert!(
            matches!(&error, AwsError::InvalidConfig { .. }),
            "{url}: {error}"
        );
        assert_eq!(resolver.calls(), 0, "{url}");
        assert!(dialer.dials().is_empty(), "{url}");
        assert!(!message(&error).contains("pw"), "{url}");
    }
}

#[test]
fn proxy_and_endpoint_environment_cannot_redirect_the_dial() {
    // The connector reads no environment: whatever proxy or endpoint
    // variables the process has, the only address dialed is the validated
    // answer for the policy host.
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![ok_reply("{}")]));
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    transport.send(&request(&db_url())).expect("sent");
    assert_eq!(dialer.dials(), vec![SocketAddr::new(ip(PRIVATE), PORT)]);
    let source = include_str!("../database.rs");
    for needle in ["std::env", "env::var", "HTTPS_PROXY", "AWS_ENDPOINT_URL"] {
        assert!(!source.contains(needle), "{needle}");
    }
}

#[test]
fn redirects_are_returned_not_followed() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let redirect = Act::Write(
        b"HTTP/1.1 307 Temporary Redirect\r\nlocation: https://169.254.169.254/\r\ncontent-length: 0\r\n\r\n"
            .to_vec(),
    );
    let dialer = Recorder::new(fixture.tls_far(vec![redirect]));
    let transport = fixture.transport(quick(), Arc::clone(&resolver), Arc::clone(&dialer));
    let response = transport.send(&request(&db_url())).expect("response");
    assert_eq!(response.status, 307);
    assert_eq!(dialer.dials().len(), 1);
    assert_eq!(resolver.calls(), 1);
}

// ---- TLS (T-DG-03a) and the exchange ----

#[test]
fn the_request_reaches_a_trusted_server_exactly_as_signed() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip("fd12:3456:789a::5")]]);
    let dialer = Recorder::new(fixture.tls_far(vec![ok_reply("{\"ok\":true}")]));
    let transport = fixture.transport(DatabaseBounds::default(), resolver, Arc::clone(&dialer));
    let response = transport.send(&request(&db_url())).expect("exchange");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"{\"ok\":true}");
    let received = String::from_utf8(dialer.received()).expect("utf-8 request");
    assert!(
        received.starts_with("POST /gremlin HTTP/1.1\r\n"),
        "{received}"
    );
    assert!(
        received.contains(&format!("host: {HOST}:{PORT}\r\n")),
        "{received}"
    );
    assert!(received.contains("authorization: AWS4-HMAC-SHA256 synthetic\r\n"));
    assert!(received.ends_with("{\"gremlin\":\"g.V().count()\"}"));
}

#[test]
fn untrusted_expired_and_wrong_host_certificates_get_no_request() {
    let fixture = Fixture::new();
    let other = Pki::new("untrusted CA");
    let (untrusted, untrusted_key) = other.server(HOST);
    let (expired, expired_key) = fixture.pki.expired_server(HOST);
    let (wrong, wrong_key) = fixture
        .pki
        .server("other.cluster-abc123.us-east-1.neptune.amazonaws.com");
    for (label, chain, key) in [
        ("untrusted", untrusted, untrusted_key),
        ("expired", expired, expired_key),
        ("wrong host", wrong, wrong_key),
    ] {
        let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
        let dialer = Recorder::new(Far::Tls {
            chain,
            key: Arc::new(key),
            acts: vec![ok_reply("{}")],
        });
        let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
        let error = transport.send(&request(&db_url())).expect_err(label);
        assert!(
            matches!(error, AwsError::Connect { .. }),
            "{label}: {error}"
        );
        assert!(
            dialer.received().is_empty(),
            "{label}: no HTTP byte decrypted"
        );
    }
}

#[test]
fn streaming_request_bodies_and_framing_headers_are_refused_before_dns() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(Far::Silent);
    let transport = fixture.transport(quick(), Arc::clone(&resolver), Arc::clone(&dialer));
    let mut framed = request(&db_url());
    framed
        .headers
        .insert("transfer-encoding", "chunked")
        .expect("header");
    transport.send(&framed).expect_err("framing header");
    assert_eq!(resolver.calls(), 0);
    assert!(dialer.dials().is_empty());
}

// ---- deadlines (T-DG-07a/c) ----

fn assert_within(start: std::time::Instant, limit: Duration) {
    assert!(start.elapsed() < limit, "took {:?}", start.elapsed());
}

#[test]
fn a_stalled_resolver_stops_at_the_connect_budget_without_dialing() {
    let fixture = Fixture::new();
    let dialer = Recorder::new(Far::Silent);
    let transport = fixture.transport(quick(), ScriptedResolver::stalled(), Arc::clone(&dialer));
    let start = std::time::Instant::now();
    let error = transport
        .send(&request(&db_url()))
        .expect_err("stalled DNS");
    assert_within(start, Duration::from_secs(3));
    assert!(
        matches!(
            error,
            AwsError::Timeout {
                phase: TimeoutPhase::Connect,
                ..
            }
        ),
        "{error}"
    );
    assert!(dialer.dials().is_empty());
}

#[test]
fn a_stalled_dial_stops_at_the_connect_budget() {
    let fixture = Fixture::new();
    let dialer = Recorder::stalled();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    let start = std::time::Instant::now();
    let error = transport
        .send(&request(&db_url()))
        .expect_err("stalled dial");
    assert_within(start, Duration::from_secs(3));
    assert!(
        matches!(
            error,
            AwsError::Timeout {
                phase: TimeoutPhase::Connect,
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_caller_deadline_bounds_every_phase() {
    // The streaming entry point passes the caller's absolute deadline; a
    // stalled resolver cannot extend it even with generous phase budgets.
    let fixture = Fixture::new();
    let generous = DatabaseBounds {
        connect: Duration::from_secs(30),
        idle: Duration::from_secs(30),
        operation: Duration::from_secs(60),
        max_response_bytes: 64,
    };
    let dialer = Recorder::new(Far::Silent);
    let transport = fixture.transport(generous, ScriptedResolver::stalled(), Arc::clone(&dialer));
    let limits = crate::stream::StreamLimits::new(64, 64)
        .expect("limits")
        .with_deadline(std::time::Instant::now() + Duration::from_millis(300));
    let start = std::time::Instant::now();
    let error = crate::stream::StreamingTransport::open(&transport, &request(&db_url()), &limits)
        .err()
        .expect("deadline");
    assert_within(start, Duration::from_secs(3));
    assert!(matches!(error, AwsError::Timeout { .. }), "{error}");
    assert!(dialer.dials().is_empty());
}

#[test]
fn a_silent_server_after_send_is_an_unknown_outcome() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![Act::Sleep(Duration::from_secs(30))]));
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    let start = std::time::Instant::now();
    let error = transport.send(&request(&db_url())).expect_err("silent");
    assert_within(start, Duration::from_secs(3));
    let text = message(&error);
    assert!(matches!(error, AwsError::Timeout { .. }), "{error}");
    assert!(text.contains("unknown"), "{text}");
    assert!(
        !text.contains("rolled back") && !text.contains("not applied"),
        "{text}"
    );
    assert!(!dialer.received().is_empty(), "the request was sent");
}

#[test]
fn the_operation_deadline_after_send_is_unknown_and_not_retryable() {
    let fixture = Fixture::new();
    let bounds = DatabaseBounds {
        connect: Duration::from_millis(500),
        idle: Duration::from_secs(5),
        operation: Duration::from_millis(400),
        max_response_bytes: 64,
    };
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![Act::Sleep(Duration::from_secs(30))]));
    let transport = fixture.transport(bounds, resolver, dialer);
    let error = transport.send(&request(&db_url())).expect_err("deadline");
    assert!(
        matches!(
            error,
            AwsError::Timeout {
                phase: TimeoutPhase::Deadline,
                ..
            }
        ),
        "{error}"
    );
    assert!(!error.is_retryable());
    assert!(message(&error).contains("unknown"));
}

#[test]
fn a_slow_drip_cannot_extend_the_operation_deadline() {
    let fixture = Fixture::new();
    let bounds = DatabaseBounds {
        connect: Duration::from_millis(500),
        idle: Duration::from_millis(300),
        operation: Duration::from_millis(700),
        max_response_bytes: 4096,
    };
    let mut acts = vec![Act::Write(
        b"HTTP/1.1 200 OK\r\ncontent-length: 4000\r\n\r\n".to_vec(),
    )];
    for _ in 0..200 {
        acts.push(Act::Write(b"x".to_vec()));
        acts.push(Act::Sleep(Duration::from_millis(100)));
    }
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(acts));
    let transport = fixture.transport(bounds, resolver, dialer);
    let start = std::time::Instant::now();
    transport.send(&request(&db_url())).expect_err("drip");
    assert_within(start, Duration::from_secs(3));
}

#[test]
fn server_query_timeout_fits_the_remaining_budget() {
    let margin = Duration::from_secs(5);
    let cap = Duration::from_secs(55);
    assert_eq!(
        server_query_timeout(Duration::from_secs(60), margin, cap),
        Some(cap)
    );
    assert_eq!(
        server_query_timeout(Duration::from_secs(20), margin, cap),
        Some(Duration::from_secs(15))
    );
    assert_eq!(server_query_timeout(margin, margin, cap), None);
    assert_eq!(
        server_query_timeout(Duration::from_secs(1), margin, cap),
        None
    );
}

// ---- response bounds (T-DG-07b) ----

fn sized_reply(declared: usize, actual: usize) -> Act {
    let mut bytes = format!("HTTP/1.1 200 OK\r\ncontent-length: {declared}\r\n\r\n").into_bytes();
    bytes.extend(std::iter::repeat_n(b'a', actual));
    Act::Write(bytes)
}

#[test]
fn the_body_limit_admits_the_boundary_and_refuses_one_more_byte() {
    let fixture = Fixture::new();
    let limit = usize::try_from(quick().max_response_bytes).expect("small");
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![sized_reply(limit, limit)]));
    let transport = fixture.transport(quick(), resolver, dialer);
    let response = transport.send(&request(&db_url())).expect("at the limit");
    assert_eq!(response.body.len(), limit);

    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![sized_reply(limit + 1, limit + 1)]));
    let transport = fixture.transport(quick(), resolver, dialer);
    transport
        .send(&request(&db_url()))
        .expect_err("over the limit");
}

#[test]
fn an_undeclared_body_over_the_limit_is_refused_while_reading() {
    let fixture = Fixture::new();
    let limit = usize::try_from(quick().max_response_bytes).expect("small");
    let mut bytes = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', limit + 1));
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![Act::Write(bytes)]));
    let transport = fixture.transport(quick(), resolver, dialer);
    let error = transport
        .send(&request(&db_url()))
        .expect_err("over the limit");
    assert!(message(&error).contains("size limit"), "{error}");
}

#[test]
fn a_huge_declared_length_fails_before_any_body_is_read() {
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![sized_reply(usize::MAX / 2, 3)]));
    let transport = fixture.transport(quick(), resolver, dialer);
    let start = std::time::Instant::now();
    let error = transport.send(&request(&db_url())).expect_err("declared");
    assert_within(start, Duration::from_secs(3));
    assert!(matches!(error, AwsError::InvalidResponse { .. }), "{error}");
}

// ---- isolation from the public and payment planes (T-DG-03b) ----

#[test]
fn the_database_transport_shares_the_validated_roots_and_no_public_connector() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let transport = SdkTransport::for_database_with_resolver(
        &fixture.runtime,
        &roots,
        endpoint(),
        DatabaseBounds::default(),
        ScriptedResolver::new(vec![vec![ip(PRIVATE)]]),
    )
    .expect("transport");
    assert!(transport
        .roots()
        .is_some_and(|held| held.same_allocation(&roots)));
}

// ---- T-DG-07c close budget; R-DG-07 DNS residual cap (PM ruling 2) ----

/// Drive the runtime until `dialer`'s far end has seen its connection close,
/// or the close budget passes; returns how long that took.
fn wait_for_close(fixture: &Fixture, dialer: &Recorder) -> Duration {
    let start = std::time::Instant::now();
    fixture.runtime.block_on(async {
        while dialer.closed.load(Ordering::SeqCst) == 0 && start.elapsed() < CLOSE_BUDGET {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    start.elapsed()
}

#[test]
fn a_finished_or_failed_exchange_closes_within_the_close_budget() {
    // Success: the server holds the connection open after its reply.
    let fixture = Fixture::new();
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(vec![ok_reply("{}")]));
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    transport.send(&request(&db_url())).expect("exchange");
    let took = wait_for_close(&fixture, &dialer);
    assert_eq!(dialer.closed.load(Ordering::SeqCst), 1, "never closed");
    assert!(took < CLOSE_BUDGET, "{took:?}");

    // Failure mid-body: the server keeps dripping a chunked body past the
    // limit and would never stop on its own.
    let fixture = Fixture::new();
    let mut acts = vec![Act::Write(
        b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec(),
    )];
    for _ in 0..500 {
        let mut chunk = b"20\r\n".to_vec();
        chunk.extend([b'a'; 32]);
        chunk.extend(b"\r\n");
        acts.push(Act::Write(chunk));
        acts.push(Act::Sleep(Duration::from_millis(10)));
    }
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(fixture.tls_far(acts));
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    let error = transport.send(&request(&db_url())).expect_err("limit");
    assert!(message(&error).contains("size limit"), "{error}");
    let took = wait_for_close(&fixture, &dialer);
    assert_eq!(dialer.closed.load(Ordering::SeqCst), 1, "never closed");
    assert!(took < CLOSE_BUDGET, "{took:?}");
}

#[test]
fn an_abandoned_lookup_makes_the_next_request_fail_closed_without_a_dial() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let counted = Arc::clone(&calls);
    let resolver = Arc::new(GatedResolver {
        gate: Arc::new(lookup::LookupGate::new()),
        lookup: Arc::new(move |_: &str| {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = wait.lock().expect("wait").recv();
            }
            Ok(vec![ip(PRIVATE)])
        }),
    });
    let dialer = Recorder::new(fixture.tls_far(vec![ok_reply("{}")]));
    let transport = SdkTransport::database(
        &fixture.runtime,
        &fixture.roots(),
        endpoint(),
        quick(),
        Arc::clone(&resolver) as Arc<dyn DatabaseResolver>,
        Arc::clone(&dialer) as Arc<dyn Dialer>,
    )
    .expect("database transport");

    let start = std::time::Instant::now();
    let error = transport.send(&request(&db_url())).expect_err("stuck DNS");
    assert_within(start, Duration::from_secs(3));
    assert!(matches!(error, AwsError::Timeout { .. }), "{error}");
    assert!(resolver.gate.has_abandoned());

    let start = std::time::Instant::now();
    let error = transport
        .send(&request(&db_url()))
        .expect_err("abandoned lookup outstanding");
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "fails at once"
    );
    assert!(endpoint_refusal(&error), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no second lookup thread");
    assert!(dialer.dials().is_empty(), "nothing was dialed");

    release.send(()).expect("release");
    let start = std::time::Instant::now();
    while resolver.gate.has_abandoned() {
        assert!(start.elapsed() < Duration::from_secs(5), "never cleared");
        std::thread::sleep(Duration::from_millis(10));
    }
    let response = transport.send(&request(&db_url())).expect("recovered");
    assert_eq!(response.status, 200);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// ---- unconditional forbidden addresses before listed ranges (#5428) ----

const METADATA_V6: &str = "fd00:ec2::254";

/// A policy built without [`DatabaseEndpoint::new`], so it can list ranges
/// `new` refuses: admission's own ordering is tested without that layer.
fn listing_unchecked(ranges: &[(&str, u8)]) -> DatabaseEndpoint {
    DatabaseEndpoint {
        host: HOST.to_string(),
        port: PORT,
        region: "us-east-1".to_string(),
        ranges: ranges
            .iter()
            .map(|&(network, prefix)| Range {
                network: ip(network),
                prefix,
            })
            .collect(),
    }
}

#[test]
fn a_policy_cannot_list_a_range_holding_a_forbidden_address() {
    for bad in [
        "fd00:ec2::/48",
        "fd00:ec2::/56",
        "fd00:ec2::/64",
        "fd00:ec2::254/128",
        "fd00:ec2::23/128",
    ] {
        let error =
            DatabaseEndpoint::new(HOST, PORT, "us-east-1", &["10.1.0.0/16", bad]).expect_err(bad);
        assert!(
            matches!(&error, AwsError::InvalidConfig { field, .. } if *field == "network_policy"),
            "{bad}: {error}"
        );
        assert!(!message(&error).contains("ec2"), "{bad}: no echo");
    }
    // Neighbouring unique-local ranges stay listable.
    for good in ["fd00:ec2:0:1::/64", "fd00:ec2:0:1::5/128", "fd00:ec3::/48"] {
        DatabaseEndpoint::new(HOST, PORT, "us-east-1", &[good]).expect(good);
    }
}

#[test]
fn forbidden_addresses_are_refused_even_inside_a_listed_range() {
    let policy = listing_unchecked(&[
        ("fd00:ec2::", 48),
        ("127.0.0.0", 8),
        ("169.254.0.0", 16),
        ("0.0.0.0", 8),
        ("224.0.0.0", 4),
        ("fe80::", 64),
        ("::", 128),
        ("::1", 128),
        ("ff02::", 16),
    ]);
    for forbidden in [
        METADATA_V6,
        "fd00:ec2::23",
        "fd00:ec2::253",
        "127.0.0.1",
        "::ffff:127.0.0.1",
        "169.254.169.254",
        "::ffff:169.254.169.254",
        "169.254.170.2",
        "0.0.0.0",
        "::ffff:0.0.0.0",
        "224.0.0.1",
        "::ffff:224.0.0.1",
        "fe80::1",
        "::",
        "::1",
        "ff02::1",
    ] {
        assert!(!policy.admits(ip(forbidden)), "{forbidden}");
        let at = SocketAddr::new(ip(forbidden), PORT);
        let error = policy.check_peer(at, at).expect_err(forbidden);
        assert!(endpoint_refusal(&error), "{forbidden}: {error}");
    }
    // The listed range still admits its ordinary addresses.
    assert!(policy.admits(ip("fd00:ec2:0:1::5")));
    let exact = listing_unchecked(&[(METADATA_V6, 128)]);
    assert!(!exact.admits(ip(METADATA_V6)));
    let at = SocketAddr::new(ip(METADATA_V6), PORT);
    assert!(endpoint_refusal(
        &exact.check_peer(at, at).expect_err("exact /128")
    ));
}

#[test]
fn forbidden_answers_inside_a_listed_range_are_never_dialed() {
    let fixture = Fixture::new();
    for listed in [("fd00:ec2::", 48), (METADATA_V6, 128)] {
        for answers in [
            vec![ip(METADATA_V6)],
            vec![ip("fd00:ec2:0:1::5"), ip(METADATA_V6)],
            vec![ip(PRIVATE), ip(METADATA_V6)],
            vec![ip("52.94.1.1"), ip(METADATA_V6)],
            vec![ip(METADATA_V6), ip(PRIVATE)],
        ] {
            let policy = listing_unchecked(&[listed, ("10.1.0.0", 16)]);
            let resolver = ScriptedResolver::new(vec![answers.clone()]);
            let dialer = Recorder::new(Far::Silent);
            let transport = fixture.transport_for(policy, quick(), resolver, Arc::clone(&dialer));
            let error = transport
                .send(&request(&db_url()))
                .expect_err("metadata answer");
            assert!(endpoint_refusal(&error), "{listed:?} {answers:?}: {error}");
            assert!(dialer.dials().is_empty(), "{listed:?} {answers:?}");
            assert_eq!(dialer.written(), 0, "{listed:?} {answers:?}");
        }
    }
}

// ---- the socket is released before the call returns (#5429) ----
//
// Each case checks the client stream's drop right after the synchronous call
// returns, without driving the production runtime again: nothing may depend
// on a later poll to close the socket.

#[test]
fn every_send_releases_its_socket_before_it_returns() {
    let fixture = Fixture::new();
    let send = |far: Far| {
        let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
        let dialer = Recorder::new(far);
        let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
        let result = transport.send(&request(&db_url()));
        (dialer, result)
    };
    // Success; the server holds the connection open after its reply.
    let (dialer, result) = send(fixture.tls_far(vec![ok_reply("{}")]));
    assert_eq!(result.expect("exchange").status, 200);
    assert!(dialer.released(), "success left the socket open");
    // A declared length over the limit, then a body over it mid-read.
    let (dialer, result) = send(fixture.tls_far(vec![sized_reply(65, 65)]));
    result.expect_err("declared over the limit");
    assert!(
        dialer.released(),
        "a declared-length failure left the socket open"
    );
    let mut bytes = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..4 {
        bytes.extend(b"20\r\n");
        bytes.extend([b'a'; 32]);
        bytes.extend(b"\r\n");
    }
    let (dialer, result) = send(fixture.tls_far(vec![Act::Write(bytes)]));
    assert!(message(&result.expect_err("mid-body")).contains("size limit"));
    assert!(dialer.released(), "a body failure left the socket open");
    // Timeout after send: the server never answers.
    let (dialer, result) = send(fixture.tls_far(vec![Act::Sleep(Duration::from_secs(30))]));
    assert!(matches!(result, Err(AwsError::Timeout { .. })));
    assert!(dialer.released(), "a timeout left the socket open");
    // Open failure before any response exists: an untrusted certificate.
    let other = Pki::new("untrusted CA");
    let (chain, key) = other.server(HOST);
    let (dialer, result) = send(Far::Tls {
        chain,
        key: Arc::new(key),
        acts: vec![ok_reply("{}")],
    });
    result.expect_err("untrusted");
    assert_eq!(dialer.dials().len(), 1);
    assert!(dialer.released(), "a TLS failure left the socket open");
    // A refused peer.
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::reporting_peer(SocketAddr::new(ip("10.9.9.9"), PORT));
    let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
    transport.send(&request(&db_url())).expect_err("peer");
    assert!(dialer.released(), "a refused peer left the socket open");
}

#[test]
fn a_streamed_response_releases_its_socket_when_dropped_or_failed() {
    let fixture = Fixture::new();
    let open = |far: Far, limits: crate::stream::StreamLimits| {
        let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
        let dialer = Recorder::new(far);
        let transport = fixture.transport(quick(), resolver, Arc::clone(&dialer));
        let opened =
            crate::stream::StreamingTransport::open(&transport, &request(&db_url()), &limits).map(
                |mut stream| {
                    let held = dialer.dropped.load(Ordering::SeqCst);
                    let first = crate::stream::ResponseStream::next_chunk(stream.as_mut());
                    drop(stream);
                    (held, first.is_ok())
                },
            );
        (dialer, opened)
    };
    let limits = || crate::stream::StreamLimits::new(64, 64).expect("limits");
    // A held stream owns its socket; dropping it (no deadline, so no
    // runtime-driving drop) releases it at once.
    let (dialer, opened) = open(fixture.tls_far(vec![ok_reply("{}")]), limits());
    let (held, read) = opened.expect("opened");
    assert_eq!(held, 0, "the open stream owns its socket");
    assert!(read);
    assert!(
        dialer.released(),
        "dropping the stream left the socket open"
    );
    // The open fails at the caller deadline after the request was sent.
    let deadline = std::time::Instant::now() + Duration::from_millis(250);
    let (dialer, opened) = open(
        fixture.tls_far(vec![Act::Sleep(Duration::from_secs(30))]),
        limits().with_deadline(deadline),
    );
    assert!(opened.is_err(), "deadline");
    assert!(dialer.released(), "a failed open left the socket open");
}

// ---- #5465: a terminal streamed read releases its socket before it returns ----
//
// The caller keeps the stream after the read returns. The client stream's drop
// is read right after `next_chunk` (or `narrow_body_limit`) returns, without
// driving any runtime: a terminal result must release the socket itself, not
// leave it to the caller dropping the stream later.

/// Open `far` with `bounds` and `limits` and hand the retained stream to `read`.
fn with_retained_stream<T>(
    fixture: &Fixture,
    far: Far,
    bounds: DatabaseBounds,
    limits: crate::stream::StreamLimits,
    read: impl FnOnce(&Recorder, &mut dyn crate::stream::ResponseStream) -> T,
) -> T {
    let resolver = ScriptedResolver::new(vec![vec![ip(PRIVATE)]]);
    let dialer = Recorder::new(far);
    let transport = fixture.transport(bounds, resolver, Arc::clone(&dialer));
    let mut stream =
        crate::stream::StreamingTransport::open(&transport, &request(&db_url()), &limits)
            .expect("opened");
    assert!(!dialer.released(), "the open stream owns its socket");
    let out = read(&dialer, stream.as_mut());
    drop(stream);
    assert!(
        dialer.released(),
        "dropping the stream left the socket open"
    );
    out
}

/// A head declaring ten body bytes, then silence.
fn stalled_body() -> Vec<Act> {
    vec![
        Act::Write(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n".to_vec()),
        Act::Sleep(Duration::from_secs(30)),
    ]
}

/// One chunked frame of `size` bytes, then silence.
fn chunked_frame(size: usize) -> Vec<Act> {
    let mut bytes = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
    bytes.extend(format!("{size:x}\r\n").into_bytes());
    bytes.extend(std::iter::repeat_n(b'a', size));
    bytes.extend(b"\r\n");
    vec![Act::Write(bytes), Act::Sleep(Duration::from_secs(30))]
}

/// Read once, expect a terminal error, check the socket is already released,
/// and check a later read repeats a failure instead of reporting a clean end.
fn read_fails_and_releases(
    dialer: &Recorder,
    stream: &mut dyn crate::stream::ResponseStream,
    case: &str,
) -> crate::stream::StreamError {
    let error = stream.next_chunk().expect_err(case);
    assert!(
        dialer.released(),
        "{case}: the read returned {error:?} with the socket still open"
    );
    let again = stream.next_chunk();
    assert!(
        again.is_err(),
        "{case}: a read after the failure returned {again:?}"
    );
    error
}

#[test]
fn a_terminal_database_body_error_releases_the_socket_before_it_returns() {
    let fixture = Fixture::new();
    let limits = || crate::stream::StreamLimits::new(256, 256).expect("limits");
    // The database body's idle bound.
    let error = with_retained_stream(
        &fixture,
        fixture.tls_far(stalled_body()),
        quick(),
        limits(),
        |dialer, stream| read_fails_and_releases(dialer, stream, "idle"),
    );
    assert!(error.to_string().contains("stalled"), "{error}");
    // The database body's operation deadline (idle is longer).
    let bounds = DatabaseBounds {
        idle: Duration::from_secs(10),
        operation: Duration::from_millis(150),
        ..quick()
    };
    let error = with_retained_stream(
        &fixture,
        fixture.tls_far(stalled_body()),
        bounds,
        limits(),
        |dialer, stream| read_fails_and_releases(dialer, stream, "operation deadline"),
    );
    assert!(error.to_string().contains("operation deadline"), "{error}");
    // The database byte cap (64) against one 128-byte frame.
    let error = with_retained_stream(
        &fixture,
        fixture.tls_far(chunked_frame(128)),
        quick(),
        limits(),
        |dialer, stream| read_fails_and_releases(dialer, stream, "byte cap"),
    );
    assert!(error.to_string().contains("size limit"), "{error}");
}

#[test]
fn a_terminal_stream_limit_refusal_releases_the_socket_before_it_returns() {
    let fixture = Fixture::new();
    // The stream's own byte cap (16) is below the database cap (64).
    let error = with_retained_stream(
        &fixture,
        fixture.tls_far(chunked_frame(32)),
        quick(),
        crate::stream::StreamLimits::new(16, 256).expect("limits"),
        |dialer, stream| read_fails_and_releases(dialer, stream, "stream cap"),
    );
    assert_eq!(error, crate::stream::StreamError::TooLarge);
    // The caller deadline has already passed when the body is first read, so
    // the read refuses before polling the body.
    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    let limits = crate::stream::StreamLimits::new(256, 256)
        .expect("limits")
        .with_deadline(deadline);
    let error = with_retained_stream(
        &fixture,
        fixture.tls_far(stalled_body()),
        DatabaseBounds {
            idle: Duration::from_secs(10),
            operation: Duration::from_secs(10),
            ..quick()
        },
        limits,
        |dialer, stream| {
            std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
            std::thread::sleep(Duration::from_millis(50));
            read_fails_and_releases(dialer, stream, "expired caller deadline")
        },
    );
    assert_eq!(error, crate::stream::StreamError::DeadlineExceeded);
    // The caller deadline fires while the read waits.
    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    let limits = crate::stream::StreamLimits::new(256, 256)
        .expect("limits")
        .with_deadline(deadline);
    let error = with_retained_stream(
        &fixture,
        fixture.tls_far(stalled_body()),
        DatabaseBounds {
            idle: Duration::from_secs(10),
            operation: Duration::from_secs(10),
            ..quick()
        },
        limits,
        |dialer, stream| read_fails_and_releases(dialer, stream, "caller deadline while reading"),
    );
    assert!(error.to_string().contains("deadline"), "{error}");
    // Narrowing the cap below what was already delivered refuses the stream.
    with_retained_stream(
        &fixture,
        fixture.tls_far(chunked_frame(32)),
        quick(),
        crate::stream::StreamLimits::new(256, 256).expect("limits"),
        |dialer, stream| {
            assert_eq!(
                stream.next_chunk().expect("first frame").map(|c| c.len()),
                Some(32)
            );
            assert!(!dialer.released(), "an unfinished stream owns its socket");
            stream.narrow_body_limit(8);
            assert!(
                dialer.released(),
                "a refusing narrow_body_limit left the socket open"
            );
            assert_eq!(
                stream.next_chunk(),
                Err(crate::stream::StreamError::TooLarge)
            );
        },
    );
}

#[test]
fn a_completed_stream_releases_its_socket_at_the_end_of_the_body() {
    let fixture = Fixture::new();
    // The server keeps the connection open after a complete body.
    with_retained_stream(
        &fixture,
        fixture.tls_far(vec![
            sized_reply(10, 10),
            Act::Sleep(Duration::from_secs(30)),
        ]),
        quick(),
        crate::stream::StreamLimits::new(256, 4).expect("limits"),
        |dialer, stream| {
            let mut body = Vec::new();
            while let Some(chunk) = stream.next_chunk().expect("chunk") {
                body.extend(chunk);
            }
            assert_eq!(body, vec![b'a'; 10]);
            assert!(
                dialer.released(),
                "the end of the body left the socket open"
            );
            assert_eq!(stream.next_chunk(), Ok(None));
        },
    );
}

/// A scripted inner stream that records its own drop.
struct ScriptedStream {
    reads: VecDeque<Result<Option<Vec<u8>>, crate::stream::StreamError>>,
    dropped: Arc<AtomicUsize>,
    narrowed: Arc<Mutex<Vec<u64>>>,
}

impl Drop for ScriptedStream {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl crate::stream::ResponseStream for ScriptedStream {
    fn status(&self) -> u16 {
        200
    }

    fn headers(&self) -> &Headers {
        static EMPTY: std::sync::OnceLock<Headers> = std::sync::OnceLock::new();
        EMPTY.get_or_init(Headers::new)
    }

    fn narrow_body_limit(&mut self, max_body_bytes: u64) {
        self.narrowed.lock().expect("narrowed").push(max_body_bytes);
    }

    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, crate::stream::StreamError> {
        self.reads.pop_front().unwrap_or(Ok(None))
    }
}

#[test]
fn the_releasing_stream_drops_its_inner_stream_on_every_terminal_read() {
    use crate::stream::{ResponseStream, StreamError};
    let scripted = |reads: Vec<Result<Option<Vec<u8>>, StreamError>>| {
        let dropped = Arc::new(AtomicUsize::new(0));
        let narrowed = Arc::new(Mutex::new(Vec::new()));
        let stream = ReleasingStream::new(ScriptedStream {
            reads: reads.into(),
            dropped: Arc::clone(&dropped),
            narrowed: Arc::clone(&narrowed),
        });
        (stream, dropped, narrowed)
    };
    let failures = [
        StreamError::IdleTimeout,
        StreamError::DeadlineExceeded,
        StreamError::TooLarge,
        StreamError::Failed(AwsError::Protocol),
    ];
    for failure in failures {
        let (mut stream, dropped, _) = scripted(vec![
            Ok(Some(b"ab".to_vec())),
            Err(failure.clone()),
            Ok(None),
        ]);
        assert_eq!(stream.next_chunk(), Ok(Some(b"ab".to_vec())));
        assert_eq!(dropped.load(Ordering::SeqCst), 0, "{failure:?}: still open");
        assert_eq!(stream.next_chunk(), Err(failure.clone()));
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "{failure:?}: not released"
        );
        // The failure repeats; it never becomes a clean end.
        assert_eq!(stream.next_chunk(), Err(failure.clone()));
        assert_eq!(stream.status(), 200);
    }
    // The end of the body releases, and the end repeats.
    let (mut stream, dropped, _) = scripted(vec![Ok(Some(b"ab".to_vec())), Ok(None)]);
    assert_eq!(stream.next_chunk(), Ok(Some(b"ab".to_vec())));
    assert_eq!(stream.next_chunk(), Ok(None));
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "end of body: not released"
    );
    assert_eq!(stream.next_chunk(), Ok(None));
    // A narrowed limit at or above what was delivered keeps the stream; one
    // below it refuses and releases.
    let (mut stream, dropped, narrowed) =
        scripted(vec![Ok(Some(b"abcd".to_vec())), Ok(Some(b"ef".to_vec()))]);
    assert_eq!(stream.next_chunk(), Ok(Some(b"abcd".to_vec())));
    stream.narrow_body_limit(4);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        0,
        "narrow to the delivered size"
    );
    stream.narrow_body_limit(3);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "a refusing narrow: not released"
    );
    assert_eq!(*narrowed.lock().expect("narrowed"), vec![4, 3]);
    assert_eq!(stream.next_chunk(), Err(StreamError::TooLarge));
}
