//! T-TLS-1 to T-TLS-20 and the transport rows of T-SIZE (#4413 S1).
//!
//! Every handshake runs through the **production** rustls connector and the
//! trust store from the production R-ROOTS path; only DNS is replaced, so a
//! synthetic AWS-style hostname reaches a loopback server. Each case is its
//! own child process (roots are validated once per process, from the process
//! environment). For a refusal the parent asserts the fixed stderr line, the
//! startup-refusal status, no panic text, and that the server accepted **no**
//! connection.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use super::roots::{RootsFailure, ValidatedRoots};
use super::test_support::{
    child_case, child_command, http_ok, report, run, Child, CountingListener, Part, Pki, Responder,
    TempDir, TlsServer,
};
use super::{SdkRuntime, SdkTransport};
use crate::endpoint::HostResolver;
use crate::stream::{StreamError, StreamLimits, StreamingTransport};
use crate::transport::{Headers, HttpRequest, Method, Timeouts, Transport};

const ENTRY: &str = "sdk::tls_tests::child_entry";

/// The synthetic endpoint host (an AWS-shaped name; DNS is the test's).
const HOST: &str = "loopback.mcp-test.api.aws";

/// Exit status a child uses for a startup refusal (S3 maps the real command's
/// own status onto the same fixed line).
const STARTUP_REFUSAL: i32 = 2;

/// Resolves every name to loopback (test-only; production DNS is guarded).
struct Loopback;

impl HostResolver for Loopback {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        Ok(vec![IpAddr::from([127, 0, 0, 1])])
    }
}

fn digest(roots: &[rustls_pki_types::CertificateDer<'_>]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for root in roots {
        root.as_ref().hash(&mut hasher);
    }
    hasher.finish()
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn request(url: String, read_timeout: Duration) -> HttpRequest {
    let mut headers = Headers::new();
    headers.insert("content-type", "application/json").unwrap();
    HttpRequest {
        method: Method::Post,
        url,
        headers,
        body: b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}".to_vec(),
        timeouts: Timeouts::new(Duration::from_secs(5), read_timeout).unwrap(),
    }
}

/// The child: validate roots like startup does, then make one request (or open
/// one stream) through the production connector.
#[test]
fn child_entry() {
    let Some(case) = child_case() else {
        return;
    };
    if case == "native-set" {
        // The sibling of T-TLS-16/17: what `load_native_certs()` yields with
        // both variables removed, filtered to what rustls accepts.
        let loaded = rustls_native_certs::load_native_certs();
        let mut valid: Vec<_> = loaded
            .certs
            .into_iter()
            .filter(|cert| {
                let mut store = rustls::RootCertStore::empty();
                store.add_parsable_certificates([cert.clone()]).0 == 1
            })
            .collect();
        valid.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        valid.dedup();
        report("count", valid.len());
        report("digest", digest(&valid));
        return;
    }
    let roots = match ValidatedRoots::from_process() {
        Ok(roots) => roots,
        Err(failure) => {
            eprintln!("toolkit mcp-proxy: {}", failure.message());
            std::process::exit(STARTUP_REFUSAL);
        }
    };
    report("count", roots.certificates().len());
    report("digest", digest(roots.certificates()));
    let runtime = SdkRuntime::new().expect("runtime");
    let transport =
        SdkTransport::loopback_for_tests(&runtime, &roots, Arc::new(Loopback)).expect("transport");
    let scheme = var("TLS_SCHEME").unwrap_or_else(|| "https".to_string());
    let port = var("TLS_PORT").expect("port");
    let url = format!("{scheme}://{HOST}:{port}/mcp");
    let read = Duration::from_secs(var("READ_SECS").map_or(10, |s| s.parse().unwrap()));
    if case == "stream" {
        let max_body: u64 = var("MAX_BODY").map_or(1 << 20, |s| s.parse().unwrap());
        let limits = StreamLimits::new(max_body, 4).unwrap();
        match transport.open(&request(url, read), &limits) {
            Ok(mut stream) => {
                report("status", stream.status());
                let mut chunks = 0;
                let mut bytes = Vec::new();
                loop {
                    match stream.next_chunk() {
                        Ok(Some(chunk)) => {
                            assert!(chunk.len() <= 4);
                            chunks += 1;
                            bytes.extend(chunk);
                        }
                        Ok(None) => {
                            report("end", "eof");
                            break;
                        }
                        Err(StreamError::TooLarge) => {
                            report("end", "too-large");
                            break;
                        }
                        Err(StreamError::IdleTimeout) => {
                            report("end", "idle-timeout");
                            break;
                        }
                        Err(StreamError::DeadlineExceeded) => {
                            report("end", "deadline");
                            break;
                        }
                        Err(StreamError::Failed(error)) => {
                            report("end", error.code());
                            break;
                        }
                    }
                }
                report("chunks", chunks);
                report("body", String::from_utf8_lossy(&bytes).escape_default());
            }
            Err(error) => report("error", error.code()),
        }
        return;
    }
    match transport.send(&request(url, read)) {
        Ok(response) => {
            report("status", response.status);
            report("body", response.body_text().unwrap_or(""));
        }
        Err(error) => report("error", error.code()),
    }
}

/// A trusted CA, an untrusted CA, and their files.
struct Fixture {
    tmp: TempDir,
    trusted: Pki,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tmp: TempDir::new("tls"),
            trusted: Pki::new("toolkit test trusted CA"),
        }
    }

    fn ca_file(&self) -> String {
        self.tmp
            .write("trusted.pem", &self.trusted.ca_pem)
            .display()
            .to_string()
    }

    fn ca_dir(&self) -> String {
        let dir = self.tmp.dir("trusted-dir");
        std::fs::write(dir.join("ca.pem"), &self.trusted.ca_pem).unwrap();
        dir.display().to_string()
    }

    fn ok_server(&self) -> TlsServer {
        serve_as(&self.trusted, HOST, Arc::new(|_, _| http_ok("{\"ok\":1}")))
    }
}

/// A loopback server presenting a certificate for `host` from `issuer`.
fn serve_as(issuer: &Pki, host: &str, respond: Responder) -> TlsServer {
    let (chain, key) = issuer.server(host);
    TlsServer::start(chain, key, respond)
}

fn child(case: &str, server: &TlsServer, vars: &[(&str, &str)]) -> Child {
    let mut command: Command = child_command(ENTRY, case);
    command.env("TLS_PORT", server.port().to_string());
    for (name, value) in vars {
        command.env(name, value);
    }
    run(&mut command)
}

fn assert_refused(outcome: &Child, server: &TlsServer, failure: RootsFailure) {
    assert_eq!(outcome.code, Some(STARTUP_REFUSAL), "{}", outcome.stderr);
    assert_eq!(
        outcome.stderr.trim_end(),
        format!("toolkit mcp-proxy: {}", failure.message()),
        "{}",
        outcome.stdout
    );
    for text in ["panicked at", "debug_assert", "assertion"] {
        assert!(!outcome.stderr.contains(text), "{}", outcome.stderr);
    }
    assert!(outcome.stdout.lines().all(|l| !l.starts_with("RESULT")));
    assert_eq!(server.accepted(), 0, "no connection may be opened");
}

#[test]
fn t_tls_1_a_certificate_from_an_untrusted_ca_fails_the_handshake() {
    let fixture = Fixture::new();
    let stranger = Pki::new("toolkit test untrusted CA");
    let server = serve_as(&stranger, HOST, Arc::new(|_, _| http_ok("{}")));
    let outcome = child("request", &server, &[("SSL_CERT_FILE", &fixture.ca_file())]);
    outcome.assert_ok();
    assert_eq!(outcome.result("error"), "aws_connect");
    assert!(server.accepted() >= 1);
    assert!(server.requests().is_empty(), "the server read no request");
}

#[test]
fn t_tls_2_a_trusted_certificate_for_another_host_fails() {
    let fixture = Fixture::new();
    let server = serve_as(
        &fixture.trusted,
        "other.mcp-test.api.aws",
        Arc::new(|_, _| http_ok("{}")),
    );
    let outcome = child("request", &server, &[("SSL_CERT_FILE", &fixture.ca_file())]);
    outcome.assert_ok();
    assert_eq!(outcome.result("error"), "aws_connect");
    assert!(server.requests().is_empty());
}

#[test]
fn t_tls_3_and_4_a_configured_file_or_directory_verifies() {
    let fixture = Fixture::new();
    for var in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
        let server = fixture.ok_server();
        let value = if var == "SSL_CERT_FILE" {
            fixture.ca_file()
        } else {
            fixture.ca_dir()
        };
        let outcome = child("request", &server, &[(var, &value)]);
        outcome.assert_ok();
        assert_eq!(outcome.result("status"), "200", "{var}");
        assert_eq!(outcome.result("body"), "{\"ok\":1}");
        assert_eq!(
            outcome.result("count"),
            "1",
            "{var}: replacement, not addition"
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].0.starts_with("POST /mcp HTTP/1.1\r\n"),
            "{}",
            requests[0].0
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One table of refusal rows.
fn t_tls_5_to_15_and_18_broken_root_configuration_is_refused_at_start() {
    let fixture = Fixture::new();
    let tmp = &fixture.tmp;
    let file = fixture.ca_file();
    let dir = fixture.ca_dir();
    let empty_file = tmp.write("empty.pem", "").display().to_string();
    let empty_dir = tmp.dir("empty-dir").display().to_string();
    let missing = tmp.path().join("missing.pem").display().to_string();
    let missing_dir = tmp.path().join("missing-dir").display().to_string();
    let malformed = tmp.dir("malformed");
    std::fs::write(
        malformed.join("bad.pem"),
        "-----BEGIN CERTIFICATE-----\n%%%%\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let invalid = tmp.dir("invalid");
    std::fs::write(
        invalid.join("x.pem"),
        "-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let looped = tmp.dir("loop");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(looped.join("a"), looped.join("b")).unwrap();
        std::os::unix::fs::symlink(looped.join("b"), looped.join("a")).unwrap();
    }
    let both = |a: &str, b: &str| format!("{a}:{b}");
    let cases: Vec<RefusalCase<'_>> = vec![
        (
            "T-TLS-5",
            vec![("SSL_CERT_FILE", empty_file)],
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            "T-TLS-6",
            vec![("SSL_CERT_FILE", missing.clone())],
            RootsFailure::ConfiguredLocationError,
        ),
        (
            "T-TLS-7",
            vec![("SSL_CERT_DIR", empty_dir.clone())],
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            "T-TLS-12",
            vec![("SSL_CERT_DIR", dir.clone()), ("SSL_CERT_FILE", missing)],
            RootsFailure::ConfiguredLocationError,
        ),
        (
            "T-TLS-13a",
            vec![
                ("SSL_CERT_FILE", file.clone()),
                ("SSL_CERT_DIR", missing_dir),
            ],
            RootsFailure::ConfiguredLocationError,
        ),
        (
            "T-TLS-13c",
            vec![
                ("SSL_CERT_FILE", file.clone()),
                ("SSL_CERT_DIR", malformed.display().to_string()),
            ],
            RootsFailure::ConfiguredLocationError,
        ),
        (
            "T-TLS-14",
            vec![
                ("SSL_CERT_FILE", file.clone()),
                ("SSL_CERT_DIR", invalid.display().to_string()),
            ],
            RootsFailure::ConfiguredInvalidCertificate,
        ),
        (
            "T-TLS-15",
            vec![("SSL_CERT_DIR", both(&dir, &empty_dir))],
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            "T-TLS-18",
            vec![("SSL_CERT_DIR", ":".to_string())],
            RootsFailure::DirListEmpty,
        ),
    ];
    let mut cases = cases;
    if cfg!(unix) {
        cases.push((
            "T-TLS-13b",
            vec![
                ("SSL_CERT_FILE", file.clone()),
                ("SSL_CERT_DIR", looped.display().to_string()),
            ],
            RootsFailure::ConfiguredLocationError,
        ));
    }
    for (name, vars, failure) in cases {
        // The server would verify under the valid location (T-TLS-12).
        let server = fixture.ok_server();
        let vars: Vec<(&str, &str)> = vars.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let outcome = child("request", &server, &vars);
        assert_refused(&outcome, &server, failure);
        assert!(
            !outcome
                .stderr
                .contains(&fixture.tmp.path().display().to_string()),
            "{name}"
        );
    }
}

/// A refusal row: name, variables, expected reason.
type RefusalCase<'a> = (&'a str, Vec<(&'a str, String)>, RootsFailure);

fn native_set() -> Child {
    let mut command = child_command(ENTRY, "native-set");
    let outcome = run(&mut command);
    outcome.assert_ok();
    outcome
}

fn assert_platform_mode(vars: &[(&str, &str)]) {
    let fixture = Fixture::new();
    let sibling = native_set();
    // The synthetic-CA server: the empty value must make nothing trusted.
    let server = fixture.ok_server();
    let outcome = child("request", &server, vars);
    if sibling.result("count") == "0" {
        assert_refused(&outcome, &server, RootsFailure::NoRoots);
        return;
    }
    outcome.assert_ok();
    assert_eq!(outcome.result("count"), sibling.result("count"));
    assert_eq!(outcome.result("digest"), sibling.result("digest"));
    assert_eq!(outcome.result("error"), "aws_connect");
    assert!(server.requests().is_empty());
}

#[test]
fn t_tls_16_an_empty_dir_variable_is_platform_mode() {
    assert_platform_mode(&[("SSL_CERT_DIR", "")]);
}

#[test]
fn t_tls_17_an_empty_file_variable_follows_the_platform_rule() {
    if cfg!(all(unix, not(target_os = "macos"))) {
        assert_platform_mode(&[("SSL_CERT_FILE", "")]);
    } else {
        let fixture = Fixture::new();
        let server = fixture.ok_server();
        let outcome = child("request", &server, &[("SSL_CERT_FILE", "")]);
        assert_refused(&outcome, &server, RootsFailure::EmptyFileVarUnsupported);
    }
}

#[test]
fn t_tls_8_proxy_variables_are_ignored() {
    let fixture = Fixture::new();
    let server = fixture.ok_server();
    let proxy = CountingListener::start();
    let proxy_url = format!("http://127.0.0.1:{}", proxy.port());
    let ca = fixture.ca_file();
    let outcome = child(
        "request",
        &server,
        &[
            ("SSL_CERT_FILE", &ca),
            ("HTTPS_PROXY", &proxy_url),
            ("https_proxy", &proxy_url),
            ("HTTP_PROXY", &proxy_url),
            ("ALL_PROXY", &proxy_url),
        ],
    );
    outcome.assert_ok();
    assert_eq!(outcome.result("status"), "200");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(proxy.accepted(), 0, "the proxy saw no connection");
}

#[test]
fn t_tls_9_plain_http_is_refused_before_connecting() {
    let fixture = Fixture::new();
    let server = fixture.ok_server();
    let ca = fixture.ca_file();
    let outcome = child(
        "request",
        &server,
        &[("SSL_CERT_FILE", &ca), ("TLS_SCHEME", "http")],
    );
    outcome.assert_ok();
    assert_eq!(outcome.result("error"), "aws_invalid_config");
    assert_eq!(server.accepted(), 0);
}

#[test]
fn t_tls_10_redirects_are_not_followed() {
    let fixture = Fixture::new();
    for status in ["301 Moved Permanently", "307 Temporary Redirect"] {
        let reply = format!(
            "HTTP/1.1 {status}\r\nlocation: https://{HOST}/elsewhere\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let server = serve_as(
            &fixture.trusted,
            HOST,
            Arc::new(move |_, _| vec![Part::Bytes(reply.clone().into_bytes())]),
        );
        let outcome = child("request", &server, &[("SSL_CERT_FILE", &fixture.ca_file())]);
        outcome.assert_ok();
        assert_eq!(outcome.result("status"), &status[..3]);
        assert_eq!(server.requests().len(), 1, "exactly one request");
    }
}

fn headers_reply(extra: usize) -> Vec<Part> {
    let mut head = String::from("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n");
    for index in 0..extra {
        let _ = write!(head, "x-filler-{index}: v\r\n");
    }
    head.push_str("\r\n{}");
    vec![Part::Bytes(head.into_bytes())]
}

#[test]
fn t_size_response_header_count_limit_is_pinned() {
    // hyper's HTTP/1 client parses at most 100 response headers.
    let fixture = Fixture::new();
    for (extra, accepted) in [(99, true), (100, false)] {
        let server = serve_as(
            &fixture.trusted,
            HOST,
            Arc::new(move |_, _| headers_reply(extra)),
        );
        let outcome = child("request", &server, &[("SSL_CERT_FILE", &fixture.ca_file())]);
        outcome.assert_ok();
        if accepted {
            assert_eq!(outcome.result("status"), "200", "{} headers", extra + 1);
        } else {
            assert_eq!(
                outcome.result("error"),
                "aws_connect",
                "{} headers",
                extra + 1
            );
        }
    }
}

#[test]
fn t_size_response_head_size_limit_is_pinned() {
    // hyper's default read buffer bound (~400 KiB) caps the response head.
    let fixture = Fixture::new();
    for (size, accepted) in [(64 * 1024, true), (512 * 1024, false)] {
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nx-big: {}\r\n\r\n{{}}",
            "a".repeat(size)
        );
        let server = serve_as(
            &fixture.trusted,
            HOST,
            Arc::new(move |_, _| vec![Part::Bytes(reply.clone().into_bytes())]),
        );
        let outcome = child("request", &server, &[("SSL_CERT_FILE", &fixture.ca_file())]);
        outcome.assert_ok();
        if accepted {
            assert_eq!(outcome.result("status"), "200", "{size}");
        } else {
            assert_eq!(outcome.result("error"), "aws_connect", "{size}");
        }
    }
}

fn chunked(parts: &[&str], pause: Option<Duration>) -> Vec<Part> {
    let mut out = vec![Part::Bytes(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n"
            .to_vec(),
    )];
    for part in parts {
        out.push(Part::Bytes(
            format!("{:x}\r\n{part}\r\n", part.len()).into_bytes(),
        ));
        if let Some(pause) = pause {
            out.push(Part::Pause(pause));
        }
    }
    out.push(Part::Bytes(b"0\r\n\r\n".to_vec()));
    out
}

#[test]
fn the_streaming_transport_reads_incrementally_over_the_real_connector() {
    let fixture = Fixture::new();
    let ca = fixture.ca_file();
    let stream_server =
        |reply: Vec<Part>| serve_as(&fixture.trusted, HOST, Arc::new(move |_, _| reply.clone()));
    // Whole stream, split to the 4-byte chunk bound.
    let server = stream_server(chunked(&["data: a\n\n", "data: bb\n\n"], None));
    let outcome = child("stream", &server, &[("SSL_CERT_FILE", &ca)]);
    outcome.assert_ok();
    assert_eq!(outcome.result("status"), "200");
    assert_eq!(outcome.result("end"), "eof");
    assert_eq!(outcome.result("body"), r"data: a\n\ndata: bb\n\n");
    assert!(outcome.result("chunks").parse::<usize>().unwrap() >= 5);

    // Body cap: at the limit delivered, one byte more refused.
    let total = "data: a\n\ndata: bb\n\n".len();
    for (cap, end) in [(total, "eof"), (total - 1, "too-large")] {
        let server = stream_server(chunked(&["data: a\n\n", "data: bb\n\n"], None));
        let cap = cap.to_string();
        let outcome = child(
            "stream",
            &server,
            &[("SSL_CERT_FILE", &ca), ("MAX_BODY", &cap)],
        );
        outcome.assert_ok();
        assert_eq!(outcome.result("end"), end, "cap {cap}");
    }

    // Idle-read timeout between events.
    let server = stream_server(chunked(
        &["data: a\n\n", "data: late\n\n"],
        Some(Duration::from_secs(3)),
    ));
    let outcome = child(
        "stream",
        &server,
        &[("SSL_CERT_FILE", &ca), ("READ_SECS", "1")],
    );
    outcome.assert_ok();
    assert_eq!(outcome.result("end"), "idle-timeout");
    assert_eq!(outcome.result("body"), r"data: a\n\n");
}

#[test]
fn t_sdl_real_deadlines_and_frame_bound() {
    use crate::error::{AwsError, TimeoutPhase};
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Instant;

    let fixture = Fixture::new();
    let roots = ValidatedRoots::for_tests(vec![fixture.trusted.ca_der.clone()]);
    let runtime = SdkRuntime::new().unwrap();
    let transport = SdkTransport::loopback_for_tests(&runtime, &roots, Arc::new(Loopback)).unwrap();
    let make_request = |port| {
        request(
            format!("https://{HOST}:{port}/mcp"),
            Duration::from_secs(120),
        )
    };
    let deadline = || Instant::now() + Duration::from_millis(300);
    let assert_elapsed = |start: Instant| {
        assert!(
            start.elapsed() < Duration::from_millis(550),
            "elapsed {:?}",
            start.elapsed()
        )
    };

    // T-SDL-1: a real body read is cancelled, and drop closes the socket.
    let (tx, rx) = std::sync::mpsc::channel();
    let server = serve_as(
        &fixture.trusted,
        HOST,
        Arc::new(move |_, _| {
            vec![
                Part::Bytes(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nx".to_vec()),
                Part::ObserveClose(tx.clone()),
            ]
        }),
    );
    let start = Instant::now();
    let limits = StreamLimits::new(10, 10).unwrap().with_deadline(deadline());
    let mut stream = transport
        .open(&make_request(server.port()), &limits)
        .unwrap();
    assert_eq!(stream.next_chunk().unwrap(), Some(b"x".to_vec()));
    assert_eq!(stream.next_chunk(), Err(StreamError::DeadlineExceeded));
    assert_elapsed(start);
    assert_eq!(stream.next_chunk(), Err(StreamError::DeadlineExceeded));
    drop(stream);
    assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());

    // T-SDL-2: TLS completes but the response head never arrives.
    let server = serve_as(
        &fixture.trusted,
        HOST,
        Arc::new(|_, _| vec![Part::Pause(Duration::from_millis(700))]),
    );
    let start = Instant::now();
    let result = transport.open(
        &make_request(server.port()),
        &StreamLimits::new(10, 10).unwrap().with_deadline(deadline()),
    );
    assert!(matches!(
        result,
        Err(AwsError::Timeout {
            phase: TimeoutPhase::Deadline,
            ..
        })
    ));
    assert_elapsed(start);

    // T-SDL-3: the accepted TCP connection never completes TLS.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0; 4096];
        while let Ok(n) = socket.read(&mut buf) {
            if n == 0 {
                break;
            }
        }
    });
    let start = Instant::now();
    let result = transport.open(
        &make_request(port),
        &StreamLimits::new(10, 10).unwrap().with_deadline(deadline()),
    );
    assert!(matches!(
        result,
        Err(AwsError::Timeout {
            phase: TimeoutPhase::Deadline,
            ..
        })
    ));
    assert_elapsed(start);
    handle.join().unwrap();

    // T-SDL-6: an expired deadline must not dial.
    let server = fixture.ok_server();
    let result = transport.open(
        &make_request(server.port()),
        &StreamLimits::new(10, 10)
            .unwrap()
            .with_deadline(Instant::now() - Duration::from_secs(1)),
    );
    assert!(matches!(
        result,
        Err(AwsError::Timeout {
            phase: TimeoutPhase::Deadline,
            ..
        })
    ));
    assert_eq!(server.accepted(), 0);

    // T-SDL-7: with a chunk limit above the body, each returned chunk is one raw frame.
    let server = serve_as(
        &fixture.trusted,
        HOST,
        Arc::new(|_, _| {
            let mut reply = b"HTTP/1.1 200 OK\r\ncontent-length: 8388608\r\n\r\n".to_vec();
            reply.extend(vec![b'x'; 8 * 1024 * 1024]);
            vec![Part::Bytes(reply)]
        }),
    );
    let mut stream = transport
        .open(
            &make_request(server.port()),
            &StreamLimits::new(8 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    let mut total = 0;
    while let Some(frame) = stream.next_chunk().unwrap() {
        assert!(frame.len() <= 512 * 1024, "raw frame {}", frame.len());
        total += frame.len();
    }
    assert_eq!(total, 8 * 1024 * 1024);
}
