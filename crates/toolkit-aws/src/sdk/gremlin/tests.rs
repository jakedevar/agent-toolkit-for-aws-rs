//! Offline tests of the Gremlin plane (T-DG-05a/b/c, T-DG-07b; S3, #5515).
//! No socket is opened and no name is resolved: the resolver is scripted and
//! the dialer hands the connection to an in-memory rustls + WebSocket server
//! below the database capability's admission checks, counting every byte the
//! client writes and every client socket it drops.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use toolkit_core::json::parse_json;

use super::graphson::{Results, Status};
use super::*;
use crate::credentials::{Credentials, CredentialsProvider};
use crate::sdk::database::{DatabaseResolver, Dialed, Dialer, ResolveFuture};
use crate::sdk::test_support::Pki;
use crate::sdk::SigV4Signer;

const HOST: &str = "db.cluster-abc123.us-east-1.neptune.amazonaws.com";
const REGION: &str = "us-east-1";
const AKID: &str = "AKIDEXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token-canary";

// ---- credentials and signing ----

struct Static {
    secret: &'static str,
}

impl CredentialsProvider for Static {
    fn name(&self) -> &'static str {
        "static"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        Credentials::new(AKID, self.secret, Some(TOKEN), None)
    }
}

struct Absent;

impl CredentialsProvider for Absent {
    fn name(&self) -> &'static str {
        "absent"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        Err(AwsError::NoCredentials)
    }
}

fn at(seconds_after_noon: u64) -> SystemTime {
    // 2026-10-29T12:00:00Z
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_275_200 + seconds_after_noon)
}

fn clock_a() -> SystemTime {
    at(0)
}

fn clock_b() -> SystemTime {
    at(300)
}

fn signer(clock: fn() -> SystemTime) -> SigV4Signer<Static> {
    SigV4Signer::new(Static { secret: SECRET }).with_clock(clock)
}

/// The authorization an independent signing of the request the server
/// received must produce: GET `https://<received host><received path>`, empty
/// body, at the received `x-amz-date`.
fn expected_authorization(head: &Head) -> Option<String> {
    let clock: fn() -> SystemTime = match head.header("x-amz-date")? {
        "20261029T120000Z" => clock_a,
        "20261029T120500Z" => clock_b,
        _ => return None,
    };
    let mut request = HttpRequest {
        method: Method::Get,
        url: format!("https://{}{}", head.header("host")?, head.path),
        headers: Headers::new(),
        body: Vec::new(),
        timeouts: Timeouts::default(),
    };
    signer(clock)
        .sign(
            &mut request,
            &SigningScope {
                region: REGION.to_string(),
                service: SERVICE.to_string(),
            },
        )
        .ok()?;
    request.headers.get("authorization").map(str::to_string)
}

// ---- the in-memory server ----

/// The upgrade request as the server read it.
#[derive(Debug, Clone)]
struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
    }
}

/// The server's answer to one statement.
#[derive(Clone)]
enum Reply {
    /// Text messages; `{id}` becomes the statement's request id.
    Batches(Vec<String>),
    /// One binary message of exactly this many bytes, status 200.
    Sized(usize),
    /// Raw bytes on the TLS stream, below the WebSocket layer.
    Raw(Vec<u8>),
    /// Drop the connection after reading the statement.
    Hangup,
    /// Read the statement and never answer.
    Silent,
}

#[derive(Clone)]
struct Script {
    chain: Vec<CertificateDer<'static>>,
    key: Arc<PrivateKeyDer<'static>>,
    /// Refuse the upgrade with 403 unless the signature verifies.
    verify: bool,
    replies: Vec<Reply>,
}

#[derive(Default)]
struct Seen {
    heads: Mutex<Vec<Head>>,
    /// Every statement payload the server read, in order.
    statements: Mutex<Vec<Vec<u8>>>,
}

/// Answers per call: an empty script answers [`HOST`] with one public
/// address; counts calls.
struct Resolver {
    answer: Vec<IpAddr>,
    calls: AtomicUsize,
}

impl DatabaseResolver for Resolver {
    fn resolve<'a>(&'a self, _host: &'a str) -> ResolveFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.clone();
        Box::pin(async move { Ok(answer) })
    }
}

struct Wire {
    script: Script,
    /// The peer the socket reports (`None`: the dialed address).
    peer: Option<SocketAddr>,
    seen: Arc<Seen>,
    dials: AtomicUsize,
    written: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    block_writes: Arc<AtomicBool>,
    blocked_writes: Arc<AtomicUsize>,
}

impl Wire {
    fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }

    fn written(&self) -> usize {
        self.written.load(Ordering::SeqCst)
    }

    /// Every dialed client socket has been dropped (read without polling).
    fn released(&self) -> bool {
        self.dropped.load(Ordering::SeqCst) == self.dials()
    }

    fn heads(&self) -> Vec<Head> {
        self.seen.heads.lock().expect("heads").clone()
    }

    fn statements(&self) -> Vec<Vec<u8>> {
        self.seen.statements.lock().expect("statements").clone()
    }
}

struct Counting {
    inner: DuplexStream,
    written: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    block_writes: Arc<AtomicBool>,
    blocked_writes: Arc<AtomicUsize>,
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
        if self.block_writes.load(Ordering::SeqCst) {
            self.blocked_writes.fetch_add(1, Ordering::SeqCst);
            return Poll::Pending;
        }
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

impl Dialer for Wire {
    fn dial(
        &self,
        address: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Dialed>> + Send + '_>> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        let (client, server) = tokio::io::duplex(4 << 20);
        let script = self.script.clone();
        let seen = Arc::clone(&self.seen);
        tokio::spawn(serve(script, server, seen));
        let stream = Counting {
            inner: client,
            written: Arc::clone(&self.written),
            dropped: Arc::clone(&self.dropped),
            block_writes: Arc::clone(&self.block_writes),
            blocked_writes: Arc::clone(&self.blocked_writes),
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

fn request_id_of(payload: &[u8]) -> String {
    let skip = 1 + usize::from(payload[0]);
    let text = std::str::from_utf8(&payload[skip..]).expect("utf-8 request");
    parse_json(text)
        .expect("request JSON")
        .get("requestId")
        .and_then(Json::as_str)
        .expect("request id")
        .to_string()
}

fn sized_message(id: &str, size: usize) -> Vec<u8> {
    let frame = |fill: usize| {
        format!(
            r#"{{"requestId":"{id}","status":{{"code":200}},"result":{{"data":["{}"]}}}}"#,
            "a".repeat(fill)
        )
    };
    let base = frame(0).len();
    frame(size - base).into_bytes()
}

#[allow(clippy::result_large_err)]
async fn serve(script: Script, server: DuplexStream, seen: Arc<Seen>) {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(script.chain.clone(), script.key.clone_key())
        .expect("server certificate");
    let Ok(tls) = tokio_rustls::TlsAcceptor::from(Arc::new(config))
        .accept(server)
        .await
    else {
        return;
    };
    let verify = script.verify;
    let record = Arc::clone(&seen);
    let callback = move |request: &Request, response: Response| {
        let head = Head {
            method: request.method().as_str().to_string(),
            path: request.uri().to_string(),
            headers: request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect(),
        };
        let valid = expected_authorization(&head).as_deref() == head.header("authorization");
        record.heads.lock().expect("heads").push(head);
        if verify && !valid {
            let mut refusal = ErrorResponse::new(Some("signature mismatch".to_string()));
            *refusal.status_mut() = http::StatusCode::FORBIDDEN;
            return Err(refusal);
        }
        Ok(response)
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tls, callback).await else {
        return;
    };
    for reply in script.replies {
        let payload = loop {
            match ws.next().await {
                Some(Ok(Message::Binary(payload))) => break payload.to_vec(),
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => {}
            }
        };
        let id = request_id_of(&payload);
        seen.statements.lock().expect("statements").push(payload);
        match reply {
            Reply::Batches(batches) => {
                for batch in batches {
                    let text = batch.replace("{id}", &id);
                    if ws.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
            }
            Reply::Sized(size) => {
                if ws
                    .send(Message::Binary(sized_message(&id, size).into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Reply::Raw(bytes) => {
                let stream = ws.get_mut();
                if stream.write_all(&bytes).await.is_err() || stream.flush().await.is_err() {
                    return;
                }
            }
            Reply::Hangup => return,
            Reply::Silent => std::future::pending::<()>().await,
        }
    }
    // Keep the connection open (never answering a close) until dropped.
    std::future::pending::<()>().await;
}

// ---- fixture ----

struct Fixture {
    pki: Pki,
    runtime: SdkRuntime,
}

fn policy() -> DatabaseEndpoint {
    DatabaseEndpoint::new(HOST, PORT, REGION, &["10.1.0.0/16"]).expect("valid policy")
}

fn quick() -> DatabaseBounds {
    DatabaseBounds {
        connect: Duration::from_secs(5),
        idle: Duration::from_secs(5),
        operation: Duration::from_secs(20),
        max_response_bytes: 16 * 1024 * 1024,
    }
}

fn later() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

impl Fixture {
    fn new() -> Self {
        Self {
            pki: Pki::new("gremlin test CA"),
            runtime: SdkRuntime::new().expect("runtime"),
        }
    }

    fn script(&self, replies: Vec<Reply>) -> Script {
        let (chain, key) = self.pki.server(HOST);
        Script {
            chain,
            key: Arc::new(key),
            verify: true,
            replies,
        }
    }

    #[allow(clippy::unused_self)]
    fn wire(&self, script: Script) -> Arc<Wire> {
        Arc::new(Wire {
            script,
            peer: None,
            seen: Arc::new(Seen::default()),
            dials: AtomicUsize::new(0),
            written: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
            block_writes: Arc::new(AtomicBool::new(false)),
            blocked_writes: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn opener(
        &self,
        endpoint: DatabaseEndpoint,
        bounds: DatabaseBounds,
        answer: &str,
        wire: &Arc<Wire>,
    ) -> (SecureOpener, Arc<Resolver>) {
        let resolver = Arc::new(Resolver {
            answer: vec![answer.parse().expect("address")],
            calls: AtomicUsize::new(0),
        });
        let opener = SecureOpener::new(
            &ValidatedRoots::for_tests(vec![self.pki.ca_der.clone()]),
            endpoint,
            bounds,
            Arc::clone(&resolver) as Arc<dyn DatabaseResolver>,
            Arc::clone(wire) as Arc<dyn Dialer>,
        )
        .expect("opener");
        (opener, resolver)
    }

    /// Open a connection to `wire` with the default policy and bounds.
    fn connect(
        &self,
        wire: &Arc<Wire>,
        signer: &dyn Signer,
    ) -> Result<GremlinConnection, AwsError> {
        let (opener, _) = self.opener(policy(), quick(), "10.1.2.3", wire);
        GremlinConnection::open(
            &self.runtime,
            &opener,
            MAX_FRAME_BYTES,
            signer,
            REGION,
            later(),
            later(),
        )
    }
}

fn ok(data: &str) -> String {
    format!(
        r#"{{"requestId":"{{id}}","status":{{"message":"","code":200,"attributes":{{}}}},"result":{{"data":{data},"meta":{{}}}}}}"#
    )
}

fn status(code: u16, data: &str) -> String {
    format!(
        r#"{{"requestId":"{{id}}","status":{{"message":"server text canary","code":{code},"attributes":{{}}}},"result":{{"data":{data},"meta":{{}}}}}}"#
    )
}

fn text(error: &AwsError) -> String {
    error.to_string()
}

// Unlike Result::expect_err, this never formats a successful response. Guard
// mutations can return megabytes of results; diagnostics must stay bounded.
trait Refused {
    fn expect_refused(self, context: &str) -> AwsError;
}

impl<T> Refused for Result<T, AwsError> {
    fn expect_refused(self, context: &str) -> AwsError {
        match self {
            Err(error) => error,
            Ok(_) => panic!("{context}: unexpectedly succeeded"),
        }
    }
}

// ---- T-DG-05a: the signed upgrade ----

#[test]
fn signing_consumes_the_operation_budget_and_exhaustion_stops_before_dns() {
    struct SlowSigner(AtomicUsize);
    impl Signer for SlowSigner {
        fn sign(&self, request: &mut HttpRequest, scope: &SigningScope) -> Result<(), AwsError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(150));
            signer(clock_a).sign(request, scope)
        }
    }
    for expired_at_start in [false, true] {
        let fixture = Fixture::new();
        let wire = fixture.wire(fixture.script(vec![]));
        let bounds = DatabaseBounds {
            operation: Duration::from_millis(50),
            ..quick()
        };
        let (opener, resolver) = fixture.opener(policy(), bounds, "10.1.2.3", &wire);
        let slow = SlowSigner(AtomicUsize::new(0));
        let invocation = if expired_at_start {
            std::time::Instant::now()
        } else {
            later()
        };
        let error = GremlinConnection::open(
            &fixture.runtime,
            &opener,
            MAX_FRAME_BYTES,
            &slow,
            REGION,
            invocation,
            invocation,
        )
        .expect_refused("exhausted budget must refuse the upgrade");
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
        assert_eq!(
            slow.0.load(Ordering::SeqCst),
            usize::from(!expired_at_start)
        );
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            0,
            "DNS ran after budget exhaustion"
        );
        assert_eq!(wire.dials(), 0);
        assert_eq!(wire.written(), 0);
    }
}

#[test]
fn the_caller_operation_deadline_is_not_renewed_by_connect_or_first_submit() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![Reply::Silent]));
    // Represents a budget already consumed in credential resolution.
    let expired = std::time::Instant::now();
    let (opener, resolver) = fixture.opener(policy(), quick(), "10.1.2.3", &wire);
    let error = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        REGION,
        expired,
        later(),
    )
    .expect_refused("the caller's exhausted operation is refused");
    assert!(matches!(
        error,
        AwsError::Timeout {
            phase: TimeoutPhase::Deadline,
            ..
        }
    ));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(wire.written(), 0);

    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    let mut connection = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        REGION,
        deadline,
        later(),
    )
    .expect("open within the budget");
    let error = connection
        .submit("g.V().drop()", None)
        .expect_refused("first statement shares the deadline");
    assert!(
        std::time::Instant::now() < deadline + Duration::from_secs(1),
        "submit renewed the budget"
    );
    assert_eq!(error.code(), "aws_timeout");
    assert!(!error.is_retryable());
    assert!(text(&error).contains("unknown"));
    assert!(wire.released());
    assert_eq!(wire.statements().len(), 1);
}

#[test]
fn expired_invocation_refuses_the_next_statement_without_claiming_no_prior_send() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![Reply::Batches(vec![ok("[]")])]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    connection.submit("g.V().drop()", None).expect("first");
    connection.invocation = Instant::now();
    let error = connection
        .submit("g.V().count()", None)
        .expect_refused("expired invocation");
    assert!(
        text(&error).contains("before this statement was sent"),
        "{error}"
    );
    assert_eq!(error.code(), "aws_timeout");
    assert_eq!(wire.statements().len(), 1);
    assert!(wire.released());
}

#[test]
fn the_upgrade_is_signed_for_the_exact_authority_path_and_empty_body() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![]));
    let connection = fixture
        .connect(&wire, &signer(clock_a))
        .expect("the signed upgrade is accepted");
    assert!(connection.is_open());
    let heads = wire.heads();
    assert_eq!(heads.len(), 1);
    let head = &heads[0];
    assert_eq!(head.method, "GET");
    assert_eq!(head.path, PATH);
    assert_eq!(head.header("host"), Some(format!("{HOST}:{PORT}").as_str()));
    assert_eq!(head.header("x-amz-date"), Some("20261029T120000Z"));
    assert_eq!(head.header("x-amz-security-token"), Some(TOKEN));
    let authorization = head.header("authorization").expect("signed");
    assert!(
        authorization.starts_with(&format!(
            "AWS4-HMAC-SHA256 Credential={AKID}/20261029/{REGION}/{SERVICE}/aws4_request, SignedHeaders=host;x-amz-date;x-amz-security-token, Signature="
        )),
        "{authorization}"
    );
    // The handshake headers were added after signing and are not signed.
    assert_eq!(head.header("upgrade"), Some("websocket"));
    assert_eq!(head.header("sec-websocket-version"), Some("13"));
    assert!(head.header("sec-websocket-key").is_some());
    // An independent signing of what arrived (method, host with port, path,
    // empty body, date) yields exactly the signature that was sent.
    assert_eq!(
        expected_authorization(head).as_deref(),
        Some(authorization),
        "the sent upgrade differs from the signed request"
    );
    connection.close();
    assert!(wire.released());
}

#[test]
fn every_connection_is_signed_afresh() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![]));
    let first = fixture.connect(&wire, &signer(clock_a)).expect("first");
    drop(first);
    let second = fixture.connect(&wire, &signer(clock_b)).expect("second");
    drop(second);
    let heads = wire.heads();
    assert_eq!(heads.len(), 2);
    assert_eq!(
        wire.dials(),
        2,
        "each connection is dialed and admitted anew"
    );
    assert_eq!(heads[0].header("x-amz-date"), Some("20261029T120000Z"));
    assert_eq!(heads[1].header("x-amz-date"), Some("20261029T120500Z"));
    assert_ne!(
        heads[0].header("authorization"),
        heads[1].header("authorization")
    );
    for head in &heads {
        assert_eq!(
            expected_authorization(head).as_deref(),
            head.header("authorization")
        );
    }
    assert!(wire.released());
}

#[test]
fn a_refused_upgrade_sends_no_statement() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![Reply::Batches(vec![ok("[]")])]));
    let wrong = SigV4Signer::new(Static {
        secret: "not-the-secret",
    })
    .with_clock(clock_a);
    let error = fixture
        .connect(&wire, &wrong)
        .expect_refused("a mismatched signature is refused");
    let message = text(&error);
    assert!(message.contains("HTTP 403"), "{message}");
    assert!(message.contains("no statement was sent"), "{message}");
    assert!(
        !message.contains("signature mismatch"),
        "server text leaked: {message}"
    );
    assert_eq!(wire.heads().len(), 1);
    assert!(wire.statements().is_empty(), "a Gremlin frame was sent");
    assert!(wire.released());
}

#[test]
fn missing_credentials_or_a_mismatched_scope_send_nothing() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![]));
    let (opener, resolver) = fixture.opener(policy(), quick(), "10.1.2.3", &wire);
    let unsigned = SigV4Signer::new(Absent).with_clock(clock_a);
    let error = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &unsigned,
        REGION,
        later(),
        later(),
    )
    .expect_refused("no credentials, no upgrade");
    assert!(matches!(error, AwsError::NoCredentials), "{error}");
    let error = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        "us-west-2",
        later(),
        later(),
    )
    .expect_refused("a region other than the policy's is refused");
    assert!(text(&error).contains("nothing was signed or sent"));
    let other_port = DatabaseEndpoint::new(HOST, 8183, REGION, &[]).expect("policy");
    let (opener, other) = fixture.opener(other_port, quick(), "10.1.2.3", &wire);
    let error = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        REGION,
        later(),
        later(),
    )
    .expect_refused("only port 8182 is the Gremlin port");
    assert!(text(&error).contains("8182"));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(other.calls.load(Ordering::SeqCst), 0);
    assert_eq!(wire.dials(), 0);
}

#[test]
fn a_forbidden_answer_or_peer_never_receives_the_signed_upgrade() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![]));
    let (opener, _) = fixture.opener(policy(), quick(), "169.254.169.254", &wire);
    let error = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        REGION,
        later(),
        later(),
    )
    .expect_refused("the metadata address is refused");
    assert!(matches!(error, AwsError::InvalidConfig { field, .. } if field == "endpoint"));
    assert_eq!(wire.dials(), 0, "a forbidden answer was dialed");

    let mut script = fixture.script(vec![]);
    script.verify = false;
    let lying = Arc::new(Wire {
        script,
        peer: Some("127.0.0.1:8182".parse().expect("peer")),
        seen: Arc::new(Seen::default()),
        dials: AtomicUsize::new(0),
        written: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(AtomicUsize::new(0)),
        block_writes: Arc::new(AtomicBool::new(false)),
        blocked_writes: Arc::new(AtomicUsize::new(0)),
    });
    let error = fixture
        .connect(&lying, &signer(clock_a))
        .expect_refused("a loopback peer is refused");
    assert!(matches!(error, AwsError::InvalidConfig { field, .. } if field == "endpoint"));
    assert_eq!(lying.dials(), 1);
    assert_eq!(lying.written(), 0, "bytes reached a forbidden peer");
    assert!(lying.heads().is_empty());
    assert!(lying.released());
}

#[test]
fn untrusted_and_wrong_host_certificates_get_no_upgrade() {
    let fixture = Fixture::new();
    let stranger = Pki::new("another CA");
    let cases = [
        ("untrusted", stranger.server(HOST)),
        (
            "wrong host",
            fixture
                .pki
                .server("db.cluster-other.us-east-1.neptune.amazonaws.com"),
        ),
        ("expired", fixture.pki.expired_server(HOST)),
    ];
    for (case, (chain, key)) in cases {
        let wire = fixture.wire(Script {
            chain,
            key: Arc::new(key),
            verify: false,
            replies: vec![],
        });
        let error = fixture
            .connect(&wire, &signer(clock_a))
            .expect_refused("the certificate is refused");
        assert!(
            text(&error).ends_with("the TLS connection to the database could not be established"),
            "{case}: {}",
            text(&error)
        );
        assert!(wire.heads().is_empty(), "{case}: the upgrade was sent");
        assert!(wire.released(), "{case}");
    }
}

// ---- T-DG-05b: GraphSON v2 over the socket ----

#[test]
fn the_request_is_a_graphson_v2_eval_with_bound_values_and_unique_ids() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![
        Reply::Batches(vec![ok(r#"[{"@type":"g:Int64","@value":1}]"#)]),
        Reply::Batches(vec![ok(r#"[{"@type":"g:Int64","@value":2}]"#)]),
    ]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    let bindings = Json::object(vec![
        ("uid", Json::string("U1")),
        ("age", Json::Int(30)),
        ("big", Json::Int(1 << 40)),
        ("price", Json::Float(29.99)),
    ]);
    let statement = "g.addV('Person').property('id', uid).property('age', age)";
    assert_eq!(
        connection
            .submit(statement, Some(&bindings))
            .expect("first"),
        vec![Json::Int(1)]
    );
    assert_eq!(
        connection.submit("g.V().count()", None).expect("second"),
        vec![Json::Int(2)]
    );
    let statements = wire.statements();
    assert_eq!(statements.len(), 2);
    let first = &statements[0];
    assert_eq!(usize::from(first[0]), MIME.len());
    assert_eq!(&first[1..=MIME.len()], MIME.as_bytes());
    let message =
        parse_json(std::str::from_utf8(&first[1 + MIME.len()..]).expect("utf-8")).expect("JSON");
    assert_eq!(message.get("op"), Some(&Json::string("eval")));
    let args = message.get("args").expect("args");
    assert_eq!(args.get("gremlin"), Some(&Json::string(statement)));
    assert_eq!(
        args.get("aliases"),
        Some(&Json::object(vec![("g", Json::string("g"))]))
    );
    let bound = args.get("bindings").expect("bindings");
    assert_eq!(bound.get("uid"), Some(&Json::string("U1")));
    assert_eq!(
        bound.get("age").map(Json::to_compact_string).as_deref(),
        Some(r#"{"@type":"g:Int32","@value":30}"#)
    );
    assert_eq!(
        bound.get("big").map(Json::to_compact_string).as_deref(),
        Some(r#"{"@type":"g:Int64","@value":1099511627776}"#)
    );
    assert_eq!(
        bound.get("price").map(Json::to_compact_string).as_deref(),
        Some(r#"{"@type":"g:Double","@value":29.99}"#)
    );
    let ids: Vec<String> = statements.iter().map(|s| request_id_of(s)).collect();
    assert_ne!(ids[0], ids[1], "request ids are unique");
    assert_eq!(ids[0].len(), 36);
    connection.close();
    assert!(wire.released());
}

#[test]
fn partial_batches_are_collected_in_order_through_the_final_status() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![
        Reply::Batches(vec![
            status(206, r#"["A1",{"@type":"g:Int32","@value":2}]"#),
            status(206, r#"[{"@type":"g:Traverser","@value":{"bulk":{"@type":"g:Int64","@value":2},"value":"A3"}}]"#),
            ok(r#"[{"name":"Bob","tags":["x",{"@type":"g:Double","@value":1.5}]}]"#),
        ]),
        Reply::Batches(vec![status(204, "null")]),
    ]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    let results = connection
        .submit("g.V().values('id')", None)
        .expect("batches");
    let rendered: Vec<String> = results.iter().map(Json::to_compact_string).collect();
    assert_eq!(
        rendered,
        vec![
            r#""A1""#,
            "2",
            r#""A3""#,
            r#""A3""#,
            r#"{"name":"Bob","tags":["x",1.5]}"#
        ]
    );
    assert!(connection
        .submit("g.V().drop()", None)
        .expect("204")
        .is_empty());
    connection.close();
}

#[test]
fn foreign_ids_server_errors_and_invalid_types_fail_and_spend_the_connection() {
    let foreign = r#"{"requestId":"00000000-0000-4000-8000-000000000000","status":{"code":200},"result":{"data":[1]}}"#;
    let cases = [
        (
            "foreign id",
            Reply::Batches(vec![foreign.to_string()]),
            "names another request",
        ),
        (
            "server error",
            Reply::Batches(vec![status(597, "null")]),
            "597",
        ),
        (
            "unsupported type",
            Reply::Batches(vec![ok(r#"[{"@type":"g:Vertex","@value":{"id":1}}]"#)]),
            "unsupported GraphSON type",
        ),
        (
            "binary garbage",
            Reply::Batches(vec!["not json".to_string()]),
            "not valid JSON",
        ),
        ("frame limit", Reply::Sized(1_048_577), "size limit"),
        ("protocol", Reply::Raw(vec![0x83, 0]), "WebSocket protocol"),
        ("dropped connection", Reply::Hangup, "connection failed"),
        (
            "server close",
            Reply::Raw(vec![0x88, 0]),
            "closed before the response completed",
        ),
        (
            "idle timeout",
            Reply::Silent,
            "no Gremlin response arrived in time",
        ),
        (
            "result count",
            Reply::Batches(vec![ok(&format!("[{}]", traverser(100_001, "1")))]),
            "100000",
        ),
        (
            "depth",
            Reply::Batches(vec![ok(&nested(65, "[", "]"))]),
            "deeper than the limit",
        ),
    ];
    for (case, reply, expected) in cases {
        let fixture = Fixture::new();
        let wire = fixture.wire(fixture.script(vec![reply, Reply::Batches(vec![ok("[]")])]));
        let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
        let error = connection
            .submit("g.V().count()", None)
            .expect_refused(&format!("{case}: the response fails the statement"));
        let message = text(&error);
        assert!(message.contains(expected), "{case}: {message}");
        assert_eq!(
            error.code(),
            match case {
                "server error" => "aws_service_error",
                "dropped connection" | "server close" => "aws_connect",
                "idle timeout" => "aws_timeout",
                _ => "aws_invalid_response",
            },
            "{case}"
        );
        assert!(
            !error.is_retryable(),
            "{case}: uncertain outcome must not enable retry"
        );
        assert!(
            message.contains("whether the graph applied it is unknown"),
            "{case}: {message}"
        );
        assert!(!message.contains("canary"), "{case}: server text leaked");
        assert!(!connection.is_open(), "{case}");
        assert!(wire.released(), "{case}: the socket was kept");
        let error = connection
            .submit("g.V().count()", None)
            .expect_refused("a spent connection sends nothing");
        assert!(text(&error).ends_with(SPENT), "{case}");
        assert_eq!(
            wire.statements().len(),
            1,
            "{case}: a later statement was sent"
        );
    }
}

#[test]
fn cumulative_response_bytes_report_an_unknown_outcome_and_spend_the_socket() {
    let fixture = Fixture::new();
    let data = format!("[\"{}\"]", "a".repeat(200));
    let wire =
        fixture.wire(fixture.script(vec![Reply::Batches(vec![status(206, &data), ok(&data)])]));
    let bounds = DatabaseBounds {
        max_response_bytes: 512,
        ..quick()
    };
    let (opener, _) = fixture.opener(policy(), bounds, "10.1.2.3", &wire);
    let mut connection = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        REGION,
        later(),
        later(),
    )
    .expect("open");
    let error = connection
        .submit("g.V().drop()", None)
        .expect_refused("cumulative byte limit");
    assert_eq!(error.code(), "aws_invalid_response");
    assert!(text(&error).contains("limit of one operation"), "{error}");
    assert!(
        text(&error).contains("whether the graph applied it is unknown"),
        "{error}"
    );
    assert!(!connection.is_open());
    assert!(wire.released());
    assert_eq!(wire.statements().len(), 1);
}

// ---- T-DG-05c: drop after send, one send, cleanup on every exit ----

#[test]
fn a_drop_after_the_statement_is_sent_is_one_send_no_retry_and_no_later_statement() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![
        Reply::Batches(vec![ok("[]")]),
        Reply::Hangup,
        Reply::Batches(vec![ok("[]")]),
    ]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    connection.submit("g.V().drop()", None).expect("first");
    let error = connection
        .submit("g.addV('Person')", None)
        .expect_refused("the connection dropped");
    assert!(text(&error).contains("unknown"), "{}", text(&error));
    let error = connection
        .submit("g.V().count()", None)
        .expect_refused("no later statement");
    assert!(text(&error).ends_with(SPENT));
    assert_eq!(wire.statements().len(), 2, "one send each, no retry");
    assert_eq!(wire.dials(), 1, "no reconnect");
    assert!(wire.released());
}

#[test]
fn a_silent_server_stops_at_the_idle_bound_with_an_unknown_outcome() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![Reply::Silent]));
    let bounds = DatabaseBounds {
        idle: Duration::from_millis(300),
        ..quick()
    };
    let (opener, _) = fixture.opener(policy(), bounds, "10.1.2.3", &wire);
    let mut connection = GremlinConnection::open(
        &fixture.runtime,
        &opener,
        MAX_FRAME_BYTES,
        &signer(clock_a),
        REGION,
        later(),
        later(),
    )
    .expect("open");
    let start = std::time::Instant::now();
    let error = connection
        .submit("g.V().count()", None)
        .expect_refused("no answer");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(error.code(), "aws_timeout");
    assert!(!error.is_retryable());
    assert!(text(&error).contains("unknown"));
    assert!(wire.released());
}

#[test]
fn close_is_bounded_against_a_server_that_never_answers_it() {
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![]));
    let connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    let start = std::time::Instant::now();
    connection.close();
    assert!(start.elapsed() < CLOSE_BUDGET + Duration::from_secs(1));
    assert!(wire.released());
}

#[test]
fn a_blocked_close_write_uses_the_close_budget_capped_by_the_invocation() {
    for (remaining, operation_remaining) in [
        (Duration::from_secs(4), Duration::from_secs(20)),
        (Duration::from_millis(150), Duration::from_secs(20)),
        (Duration::from_secs(4), Duration::from_millis(150)),
    ] {
        let fixture = Fixture::new();
        let wire = fixture.wire(fixture.script(vec![]));
        let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
        connection.invocation = Instant::now() + remaining;
        connection.operation_deadline = Instant::now() + operation_remaining;
        wire.block_writes.store(true, Ordering::SeqCst);
        let start = std::time::Instant::now();
        connection.close();
        let elapsed = start.elapsed();
        let expected = remaining
            .min(operation_remaining)
            .min(Duration::from_secs(2));
        assert!(
            elapsed >= expected.saturating_sub(Duration::from_millis(10)),
            "close returned before its blocked write timed out"
        );
        assert!(
            elapsed < expected + Duration::from_secs(1),
            "close took {elapsed:?}"
        );
        assert!(
            wire.blocked_writes.load(Ordering::SeqCst) > 0,
            "close never attempted its write"
        );
        assert!(wire.released(), "close timeout retained the socket");
    }
}

// ---- T-DG-07b: frame, depth and bulk bounds at the boundary and one past ----

#[test]
fn a_frame_at_the_limit_is_read_and_one_byte_more_is_refused() {
    assert_eq!(MAX_FRAME_BYTES, 1_048_576);
    let fixture = Fixture::new();
    let wire = fixture.wire(fixture.script(vec![Reply::Sized(1_048_576)]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    let results = connection
        .submit("g.V().count()", None)
        .expect("a frame of exactly 1 MiB is read");
    assert_eq!(results.len(), 1);
    connection.close();

    let wire = fixture.wire(fixture.script(vec![Reply::Sized(1_048_577)]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    let error = connection
        .submit("g.V().count()", None)
        .expect_refused("one byte more is refused");
    assert!(text(&error).contains("size limit"), "{}", text(&error));
    assert!(wire.released());
}

#[test]
fn a_tiny_frame_declaring_a_huge_length_fails_at_its_header() {
    let fixture = Fixture::new();
    // FIN + binary, 64-bit length 2^40, and no payload at all.
    let header = vec![0x82, 0x7f, 0, 0, 0x01, 0, 0, 0, 0, 0];
    let wire = fixture.wire(fixture.script(vec![Reply::Raw(header)]));
    let mut connection = fixture.connect(&wire, &signer(clock_a)).expect("open");
    let start = std::time::Instant::now();
    let error = connection
        .submit("g.V().count()", None)
        .expect_refused("the declared length is refused");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the client waited for the declared payload"
    );
    assert!(text(&error).contains("size limit"), "{}", text(&error));
    assert!(wire.released());
}

fn decode(data: &str) -> Result<Vec<Json>, AwsError> {
    let mut results = Results::default();
    let message =
        format!(r#"{{"requestId":"r","status":{{"code":200}},"result":{{"data":{data}}}}}"#);
    results.add_message(message.as_bytes(), "r").map(|status| {
        assert_eq!(status, Status::Complete);
        results.items
    })
}

fn nested(levels: usize, open: &str, close: &str) -> String {
    format!("[{}1{}]", open.repeat(levels), close.repeat(levels))
}

#[test]
fn graphson_depth_is_admitted_at_64_and_refused_at_65() {
    assert_eq!(MAX_DEPTH, 64);
    for (shape, open, close) in [
        ("plain lists", "[", "]"),
        ("typed lists", r#"{"@type":"g:List","@value":["#, "]}"),
        ("maps", r#"{"k":"#, "}"),
    ] {
        let at_limit = decode(&nested(64, open, close));
        assert!(at_limit.is_ok(), "{shape}: depth 64 refused: {at_limit:?}");
        let error = decode(&nested(65, open, close)).expect_refused("depth 65");
        assert!(text(&error).contains("deeper than the limit"), "{shape}");
    }
    // Raw nesting far past anything decodable is refused before parsing.
    let error = decode(&nested(10_000, "[", "]")).expect_refused("deep raw JSON");
    assert!(text(&error).contains("deeper than the limit"));
}

fn traverser(bulk: i64, value: &str) -> String {
    format!(
        r#"{{"@type":"g:Traverser","@value":{{"bulk":{{"@type":"g:Int64","@value":{bulk}}},"value":{value}}}}}"#
    )
}

#[test]
fn bulk_expansion_is_admitted_at_100000_results_and_refused_at_100001() {
    assert_eq!(MAX_RESULTS, 100_000);
    assert_eq!(MAX_EXPANDED_BYTES, 16_777_216);
    let limit = 100_000;
    assert_eq!(
        decode(&format!("[{}]", traverser(limit, "1")))
            .expect("exactly 100000")
            .len(),
        100_000
    );
    for (case, data) in [
        ("one bulk past", format!("[{}]", traverser(limit + 1, "1"))),
        ("huge bulk", format!("[{}]", traverser(i64::MAX, "1"))),
        (
            "plain plus bulk",
            format!(r#"["a",{}]"#, traverser(limit, "1")),
        ),
    ] {
        let start = std::time::Instant::now();
        let error = decode(&data).expect_refused(case);
        assert!(text(&error).contains("100000"), "{case}: {}", text(&error));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{case}: copies were made"
        );
    }
    // Across batches of one operation.
    let mut results = Results::default();
    let batch = |code: u16, bulk: i64| {
        format!(
            r#"{{"requestId":"r","status":{{"code":{code}}},"result":{{"data":[{}]}}}}"#,
            traverser(bulk, "1")
        )
    };
    assert_eq!(
        results
            .add_message(batch(206, 50_000).as_bytes(), "r")
            .expect("first half"),
        Status::Partial
    );
    let error = results
        .add_message(batch(200, 50_001).as_bytes(), "r")
        .expect_refused("the second batch passes the limit");
    assert!(text(&error).contains("100000"));
    // Bulk copies are bounded in bytes too, before any copy.
    let wide = format!("\"{}\"", "x".repeat(200));
    let error =
        decode(&format!("[{}]", traverser(limit, &wide))).expect_refused("16 MiB of copies");
    assert!(text(&error).contains("16 MiB"), "{}", text(&error));
}

#[test]
fn graphson_v2_typed_values_decode_and_others_fail_closed() {
    let values = decode(
        r#"[{"@type":"g:Int64","@value":19},{"@type":"g:Int32","@value":7},{"@type":"g:Double","@value":2.5},
            {"@type":"g:Float","@value":3},{"@type":"g:UUID","@value":"u"},{"@type":"g:Date","@value":1700000000000},
            {"@type":"g:Set","@value":["a"]},{"m":{"n":[{"@type":"g:Int64","@value":1}]}},null,true]"#,
    )
    .expect("supported values");
    let rendered: Vec<String> = values.iter().map(Json::to_compact_string).collect();
    assert_eq!(
        rendered,
        vec![
            "19",
            "7",
            "2.5",
            "3.0",
            r#""u""#,
            "1700000000000",
            r#"["a"]"#,
            r#"{"m":{"n":[1]}}"#,
            "null",
            "true"
        ]
    );
    for data in [
        r#"[{"@type":"g:Vertex","@value":{}}]"#,
        r#"[{"@type":"g:Map","@value":["k",1]}]"#,
        r#"[{"@type":"g:Double","@value":"NaN"}]"#,
        r#"[{"@type":"g:Int32","@value":4294967296}]"#,
        r#"[{"@type":"g:Int64"}]"#,
        r#"[[{"@type":"g:Traverser","@value":{"bulk":1,"value":1}}]]"#,
        r#"[{"@type":"g:Traverser","@value":{"bulk":0,"value":1}}]"#,
        r#"{"@type":"g:List","@value":[]}"#,
    ] {
        assert!(decode(data).is_err(), "{data} was accepted");
    }
}
