//! Offline tests of the bounded public service-plane transport (#5522;
//! R-DG-07). No test resolves a real name or reaches the network: the
//! blocking lookup is injected, every answer it gives is loopback or private
//! (so the public-address guard must refuse it), and a loopback listener on
//! the request's port proves that nothing is ever dialed.

use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::lookup::{BlockingLookup, LookupGate, ABANDONED_LOOKUP_REFUSAL};
use super::super::roots::ValidatedRoots;
use super::super::test_support::Pki;
use super::super::{lookup, SdkRuntime, SdkTransport};
use super::Gate;
use crate::error::{AwsError, TimeoutPhase};
use crate::stream::{StreamLimits, StreamingTransport};
use crate::transport::{Headers, HttpRequest, Method, Timeouts, Transport};

/// The operation deadline the tests give each open.
const DEADLINE: Duration = Duration::from_millis(100);
/// Control must come back well inside this, while the lookup stays blocked.
const RETURN_BY: Duration = Duration::from_millis(1500);
/// A blocked fixture lookup gives up after this, so a regression fails the
/// test instead of hanging the suite.
const STUCK_FOR: Duration = Duration::from_secs(5);

fn runtime() -> SdkRuntime {
    SdkRuntime::new().expect("runtime")
}

fn roots() -> ValidatedRoots {
    ValidatedRoots::for_tests(vec![Pki::new("service-plane test CA").ca_der])
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect("test address")
}

/// A listener that must never accept: every dial would land here.
fn silent_listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    listener.set_nonblocking(true).expect("nonblocking");
    listener
}

fn assert_never_dialed(listener: &TcpListener) {
    assert_eq!(
        listener.accept().expect_err("nothing may be dialed").kind(),
        std::io::ErrorKind::WouldBlock,
        "a connection reached the listener"
    );
}

/// A public service request whose port is the silent listener's.
fn request(listener: &TcpListener) -> HttpRequest {
    request_to(&format!(
        "https://rds.us-east-1.amazonaws.com:{}/",
        listener.local_addr().expect("address").port()
    ))
}

fn request_to(url: &str) -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: url.to_string(),
        headers: Headers::new(),
        body: b"Action=DescribeDBClusters".to_vec(),
        timeouts: Timeouts::default(),
    }
}

fn limits_until(deadline: Instant) -> StreamLimits {
    StreamLimits::new(16 * 1024 * 1024, 64 * 1024)
        .expect("limits")
        .with_deadline(deadline)
}

fn open_error(transport: &SdkTransport, request: &HttpRequest, deadline: Instant) -> AwsError {
    match transport.open(request, &limits_until(deadline)) {
        Ok(_) => panic!("the open succeeded"),
        Err(error) => error,
    }
}

/// A lookup whose first call blocks until released (or [`STUCK_FOR`]), then
/// answers loopback; later calls answer loopback at once.
fn stuck_then_loopback(calls: &Arc<AtomicUsize>) -> (BlockingLookup, mpsc::Sender<()>) {
    let (release, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let calls = Arc::clone(calls);
    let lookup: BlockingLookup = Arc::new(move |_host: &str| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = wait.lock().expect("wait").recv_timeout(STUCK_FOR);
        }
        Ok(vec![ip("127.0.0.1")])
    });
    (lookup, release)
}

/// A lookup that answers `answers` at once, counting its calls.
fn answering(calls: &Arc<AtomicUsize>, answers: std::io::Result<Vec<IpAddr>>) -> BlockingLookup {
    let calls = Arc::clone(calls);
    let answers = Mutex::new(answers);
    Arc::new(move |_host: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        match &*answers.lock().expect("answers") {
            Ok(addresses) => Ok(addresses.clone()),
            Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
        }
    })
}

fn wait_until_clear(gate: &LookupGate) {
    let start = Instant::now();
    while gate.has_abandoned() {
        assert!(start.elapsed() < STUCK_FOR * 2, "never cleared");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_endpoint_refusal(error: &AwsError, shows: &str) {
    assert_eq!(error.code(), "aws_invalid_config", "{error:?}");
    assert!(!error.is_retryable(), "{error:?}");
    assert!(error.to_string().contains(shows), "{error}");
}

#[test]
fn a_blocked_service_lookup_returns_control_at_the_operation_deadline() {
    let rt = runtime();
    let gate = Arc::new(LookupGate::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (lookup, release) = stuck_then_loopback(&calls);
    let transport =
        SdkTransport::public_service(&rt, &roots(), &Gate::Owned(Arc::clone(&gate)), lookup)
            .expect("transport");
    let listener = silent_listener();

    let started = Instant::now();
    let error = open_error(&transport, &request(&listener), started + DEADLINE);
    let elapsed = started.elapsed();

    assert!(
        elapsed < RETURN_BY,
        "the blocked lookup held the caller past its deadline: {elapsed:?}"
    );
    assert!(
        matches!(
            error,
            AwsError::Timeout {
                phase: TimeoutPhase::Deadline,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.is_retryable());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one lookup started");
    assert!(gate.has_abandoned(), "the deadline abandoned the lookup");
    assert_never_dialed(&listener);

    // The abandoned answer is discarded when it arrives: still no dial.
    release.send(()).expect("release");
    wait_until_clear(&gate);
    assert_never_dialed(&listener);
}

#[test]
fn abandoned_service_lookups_are_capped_at_one_and_fail_closed() {
    let rt = runtime();
    let gate = Arc::new(LookupGate::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (lookup, release) = stuck_then_loopback(&calls);
    let transport =
        SdkTransport::public_service(&rt, &roots(), &Gate::Owned(Arc::clone(&gate)), lookup)
            .expect("transport");
    let listener = silent_listener();

    let started = Instant::now();
    let first = open_error(&transport, &request(&listener), started + DEADLINE);
    assert!(started.elapsed() < RETURN_BY, "{:?}", started.elapsed());
    assert!(matches!(first, AwsError::Timeout { .. }), "{first:?}");
    assert!(gate.has_abandoned());

    // While that lookup still runs, a new one fails closed at once and starts
    // no second lookup thread, though its own deadline is far away.
    for _ in 0..3 {
        let again = Instant::now();
        let error = open_error(
            &transport,
            &request(&listener),
            again + Duration::from_secs(10),
        );
        assert!(again.elapsed() < RETURN_BY, "{:?}", again.elapsed());
        assert_endpoint_refusal(&error, ABANDONED_LOOKUP_REFUSAL);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no second lookup started");
    }
    // The buffered (non-streaming) path shares the same gate.
    let error = transport.send(&request(&listener)).unwrap_err();
    assert_endpoint_refusal(&error, ABANDONED_LOOKUP_REFUSAL);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_never_dialed(&listener);

    // Once the resolver returns, lookups run again (the loopback answer is
    // then refused by the public-address guard, with nothing dialed).
    release.send(()).expect("release");
    wait_until_clear(&gate);
    let error = open_error(
        &transport,
        &request(&listener),
        Instant::now() + Duration::from_secs(10),
    );
    assert_endpoint_refusal(&error, "127.0.0.1");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_never_dialed(&listener);
}

#[test]
fn the_service_transport_keeps_the_public_and_private_endpoint_guards() {
    let rt = runtime();
    let listener = silent_listener();
    let refused: [(std::io::Result<Vec<IpAddr>>, &str); 5] = [
        (Ok(vec![ip("10.1.2.3")]), "10.1.2.3"),
        (Ok(vec![ip("169.254.169.254")]), "169.254.169.254"),
        // Mixed answers are refused, never filtered.
        (Ok(vec![ip("52.94.0.10"), ip("127.0.0.1")]), "127.0.0.1"),
        (Ok(Vec::new()), "no addresses"),
        (
            Err(std::io::Error::other("secret-resolver-detail")),
            "could not be resolved",
        ),
    ];
    for (answers, shows) in refused {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = Gate::Owned(Arc::new(LookupGate::new()));
        let transport =
            SdkTransport::public_service(&rt, &roots(), &gate, answering(&calls, answers))
                .expect("transport");
        for error in [
            open_error(
                &transport,
                &request(&listener),
                Instant::now() + Duration::from_secs(10),
            ),
            transport.send(&request(&listener)).unwrap_err(),
        ] {
            assert_endpoint_refusal(&error, shows);
            assert!(!error.to_string().contains("secret-resolver-detail"));
            assert!(!format!("{error:?}").contains("secret-resolver-detail"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{shows}: one lookup per request");
        assert_never_dialed(&listener);
    }

    // The literal-host and scheme guards run before any lookup.
    let port = listener.local_addr().expect("address").port();
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Gate::Owned(Arc::new(LookupGate::new()));
    let transport = SdkTransport::public_service(
        &rt,
        &roots(),
        &gate,
        answering(&calls, Ok(vec![ip("127.0.0.1")])),
    )
    .expect("transport");
    for (url, shows) in [
        (format!("https://127.0.0.1:{port}/"), "127.0.0.1"),
        (format!("https://10.0.0.1:{port}/"), "10.0.0.1"),
        (format!("https://[::1]:{port}/"), "::1"),
    ] {
        let error = open_error(
            &transport,
            &request_to(&url),
            Instant::now() + Duration::from_secs(10),
        );
        assert_endpoint_refusal(&error, shows);
    }
    let error = open_error(
        &transport,
        &request_to(&format!("http://rds.us-east-1.amazonaws.com:{port}/")),
        Instant::now() + Duration::from_secs(10),
    );
    assert_eq!(error.code(), "aws_invalid_config", "{error:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no lookup for a refused URL");
    assert_never_dialed(&listener);
    assert!(transport.roots().is_some());
}

#[test]
fn the_production_service_transport_queues_on_the_process_wide_gate() {
    assert!(std::ptr::eq(Gate::Process.get(), lookup::system_gate()));
    // Building it loads TLS roots but resolves and opens nothing.
    let transport = SdkTransport::for_public_service(&runtime(), &roots()).expect("transport");
    assert!(transport.roots().is_some());
}
