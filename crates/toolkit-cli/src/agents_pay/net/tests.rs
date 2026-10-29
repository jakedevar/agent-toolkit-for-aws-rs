use super::*;
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, FixedResolver};

fn input(deadline: Instant) -> MerchantRequest<'static> {
    MerchantRequest {
        url: "https://merchant.example/path",
        method: "get",
        proof: None,
        body_cap: 4,
        timeout: Duration::from_secs(2),
        operation_deadline: deadline,
    }
}
fn merchant(
    transport: FakeStreamingTransport,
) -> StreamingMerchant<FakeStreamingTransport, FixedResolver, SystemClock> {
    StreamingMerchant {
        transport,
        resolver: FixedResolver::new().answer_any(&["8.8.8.8"]),
        clock: SystemClock,
    }
}
#[test]
fn safe_method_allows_only_get_and_head_case_insensitively() {
    assert!(safe_method("get"));
    assert!(safe_method("HEAD"));
    for method in ["POST", "PUT", "PATCH", "DELETE", "GET ", ""] {
        assert!(!safe_method(method), "{method}");
    }
}
#[test]
fn headers_are_fresh_redirect_is_not_followed_and_body_is_raw() {
    let m = merchant(
        FakeStreamingTransport::new()
            .respond(
                302,
                &[
                    ("location", "https://secret.example"),
                    ("content-encoding", "gzip"),
                    ("set-cookie", "x=y"),
                ],
                vec![FakeChunk::data(b"raw")],
            )
            .respond(200, &[], vec![]),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let response = m.request(&input(deadline)).unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(response.body, b"raw");
    assert!(response.headers.get("location").is_none());
    let mut replay = input(deadline);
    replay.proof = Some(("payment-signature", "secret"));
    m.request(&replay).unwrap();
    let requests = m.transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers.get("accept"),
        Some("application/json, text/plain")
    );
    assert_eq!(requests[0].headers.get("accept-encoding"), Some("identity"));
    assert_eq!(
        requests[0].headers.get("user-agent"),
        Some(concat!("aws-agents-pay/", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(requests[0].headers.len(), 3);
    assert_eq!(requests[1].headers.len(), 4);
    assert!(requests[1].headers.is_sensitive("payment-signature"));
    assert!(requests[1].headers.get("cookie").is_none());
    assert!(requests.iter().all(|r| r.body.is_empty()));
}
#[test]
fn cap_precheck_and_stream_and_deadline() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let m = merchant(FakeStreamingTransport::new().respond(
        200,
        &[("content-length", "00005")],
        vec![FakeChunk::data(b"x")],
    ));
    assert_eq!(
        m.request(&input(deadline)).unwrap_err().0,
        "Response declares 00005 bytes, over the 4 limit."
    );
    assert_eq!(m.transport.frames_pulled(), 0);
    let m =
        merchant(FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::data(b"12345")]));
    assert_eq!(
        m.request(&input(deadline)).unwrap_err().0,
        "Response exceeded 4 bytes; refusing to buffer more."
    );
    let m = merchant(FakeStreamingTransport::new().respond(
        200,
        &[],
        vec![FakeChunk::data(b"1"), FakeChunk::Stall],
    ));
    assert_eq!(m.request(&input(deadline)).unwrap_err(), timeout());
    assert_eq!(m.transport.narrow_calls(), vec![4]);
    assert!(m.transport.open_limits()[0].deadline().is_some());
}

#[test]
fn dns_deadline_admission_prevents_lookup_and_connect() {
    use std::cell::Cell;
    use std::net::IpAddr;
    use std::rc::Rc;
    struct TestClock(Rc<Cell<Instant>>);
    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
    }
    struct AdvancingResolver {
        now: Rc<Cell<Instant>>,
        after: Instant,
        calls: Cell<usize>,
    }
    impl HostResolver for AdvancingResolver {
        fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, String> {
            self.calls.set(self.calls.get() + 1);
            self.now.set(self.after);
            Ok(vec!["8.8.8.8".parse().unwrap()])
        }
    }
    let now = Instant::now();
    let deadline = now + Duration::from_secs(10);
    let time = Rc::new(Cell::new(deadline));
    let m = StreamingMerchant {
        transport: FakeStreamingTransport::new().respond(200, &[], vec![]),
        resolver: AdvancingResolver {
            now: time.clone(),
            after: deadline,
            calls: Cell::new(0),
        },
        clock: TestClock(time.clone()),
    };
    assert_eq!(m.request(&input(deadline)).unwrap_err(), timeout());
    assert_eq!(m.resolver.calls.get(), 0);
    assert!(m.transport.requests().is_empty());
    time.set(now);
    assert_eq!(m.request(&input(deadline)).unwrap_err(), timeout());
    assert_eq!(m.resolver.calls.get(), 1);
    assert!(m.transport.requests().is_empty());
    time.set(now);
    let m = merchant(FakeStreamingTransport::new().respond(200, &[], vec![]));
    assert_eq!(m.request(&input(deadline)).unwrap().status, 200);
}

#[test]
fn unsafe_methods_and_nonpublic_answers_never_connect() {
    let m = merchant(FakeStreamingTransport::new());
    let mut req = input(Instant::now() + Duration::from_secs(30));
    req.method = "POST";
    assert!(m
        .request(&req)
        .unwrap_err()
        .0
        .starts_with("Method POST is not allowed"));
    assert!(m.resolver.lookups().is_empty());
    assert!(m.transport.requests().is_empty());
    let m = StreamingMerchant {
        transport: FakeStreamingTransport::new(),
        resolver: FixedResolver::new().answer_any(&["8.8.8.8", "127.0.0.1"]),
        clock: SystemClock,
    };
    req.method = "GET";
    assert_eq!(
        m.request(&req).unwrap_err().0,
        "Payment URL resolves to a non-public address."
    );
    assert!(m.transport.requests().is_empty());
}
