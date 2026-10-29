//! The bridge route over injected worlds (`OpenClaw` B2, #5443): T-BR-SVC-1/2,
//! T-BR-TOK, T-BR-PRE, T-BR-POST, T-BR-POST-PROOF, T-BR-DL, T-BR-NOCFG,
//! T-BR-QUIET, T-BR-SCRUB and the runtime half of T-OC-FINAL-ROUTE.
//!
//! Every request goes through the whole pipe ([`serve_frame`]: bounded read,
//! B1 validation, dispatch), and every fake counts its calls.

use std::cell::{Cell, RefCell};
use std::io::Cursor;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::testing::{FakeChunk, FakeSigner, FakeStreamingTransport, FixedResolver};
use toolkit_core::{parse_json, Json};

use super::{handle_service, serve_frame, serve_quiet, Frame, DEADLINE};
use crate::agents_pay::agentcore::{
    FakeOutcome, FakePaymentsApi, PaymentClient, PaymentError, PaymentErrorClass, PaymentProof,
    PaymentsApi, ProcessPaymentRequest,
};
use crate::agents_pay::deps::fixtures::{FakeClock, FakeMerchant};
use crate::agents_pay::deps::{ConfigStore, Deps, PaymentsConnector, TEST_FIXTURE_REFUSAL};
use crate::Output;

const MANAGER: &str =
    "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/bridge-abcdefghij";
const SESSION: &str = "payment-session-TEST00000000001";
const INSTRUMENT: &str = "payment-instrument-TEST00000000001";
const USER: &str = "user";
const URL: &str = "https://merchant.test/pay/weather?city=sydney";
const PAY_TO: &str = "0x1111111111111111111111111111111111111111";
const ASSET: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
const NOW: i64 = 1_800_000_000;
const TOKEN_1000: &str = "e36c6d851f2fc37ad61d084f213d568e5c6920da19a3aa2675babb3bcff5c81d";
const TOKEN_1001: &str = "617369d56a4f010acec1db48579ab07eaf78887e3464e044a68c04c5b6162b3d";
const INPUT: &str = "{\"ok\":false,\"error\":\"invalid bridge request\"}\n";
const FAILURE: &str = "{\"ok\":false,\"error\":\"AgentCore request failed\"}\n";

fn json(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

fn accepted() -> Json {
    json(&format!(
        r#"{{"scheme":"exact","network":"eip155:84532","amount":"1000","asset":"{ASSET}","payTo":"{PAY_TO}","maxTimeoutSeconds":60,"extra":{{"name":"USDC","version":"2","nonce":"n1"}}}}"#
    ))
}

fn policy() -> Json {
    json(&format!(
        r#"{{"networkPreferences":["eip155:84532"],"allowedOrigins":["https://merchant.test"],"allowedRecipients":["{PAY_TO}"],"maxPaymentAmountAtomic":"100000"}}"#
    ))
}

fn payment_request() -> Json {
    Json::object(vec![
        ("operation", Json::string("process_payment")),
        ("region", Json::string("us-west-2")),
        ("payment_manager_arn", Json::string(MANAGER)),
        ("payment_session_id", Json::string(SESSION)),
        ("payment_instrument_id", Json::string(INSTRUMENT)),
        ("user_id", Json::string(USER)),
        ("agent_name", Json::string("openclaw-aws-agents-pay")),
        ("version", Json::string("2")),
        ("request_url", Json::string(URL)),
        ("accepted", accepted()),
        ("resource", Json::object(vec![("url", Json::string(URL))])),
        ("policy", policy()),
    ])
}

fn session_request() -> Json {
    Json::object(vec![
        ("operation", Json::string("get_payment_session")),
        ("region", Json::string("eu-west-1")),
        ("payment_manager_arn", Json::string(MANAGER)),
        ("payment_session_id", Json::string(SESSION)),
        ("user_id", Json::string(USER)),
        ("agent_name", Json::string("openclaw-aws-agents-pay")),
    ])
}

/// `value` with member `key` replaced (or appended).
fn with(value: &Json, key: &str, member: Json) -> Json {
    let Json::Object(mut members) = value.clone() else {
        panic!("object")
    };
    match members.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = member,
        None => members.push((key.to_owned(), member)),
    }
    Json::Object(members)
}

fn with_accepted(key: &str, member: Json) -> Json {
    with(
        &payment_request(),
        "accepted",
        with(&accepted(), key, member),
    )
}

fn with_policy(policy: &Json) -> Json {
    with(&payment_request(), "policy", policy.clone())
}

fn session_doc(members: &[(&str, Json)]) -> Json {
    let mut session = vec![
        ("paymentSessionId", Json::string(SESSION)),
        ("paymentManagerArn", Json::string(MANAGER)),
        ("userId", Json::string(USER)),
        ("expiryTimeInMinutes", Json::Int(60)),
        ("createdAt", Json::string("2026-08-07T00:00:00Z")),
        ("updatedAt", Json::string("2026-08-07T00:00:00.5Z")),
    ];
    for (key, value) in members {
        session.retain(|(k, _)| k != key);
        session.push((key, value.clone()));
    }
    Json::object(vec![("paymentSession", Json::object(session))])
}

fn proof(authorization: &[(&str, &str)]) -> Json {
    let mut fields = vec![
        ("from", "0x2222222222222222222222222222222222222222"),
        ("to", PAY_TO),
        ("value", "1000"),
        ("validAfter", "0"),
        ("validBefore", "4102444800"),
        ("nonce", "0x01"),
    ];
    for (key, value) in authorization {
        fields.retain(|(k, _)| k != key);
        fields.push((key, value));
    }
    Json::object(vec![
        ("signature", Json::string(SIGNATURE)),
        (
            "authorization",
            Json::object(
                fields
                    .into_iter()
                    .map(|(k, v)| (k, Json::string(v)))
                    .collect(),
            ),
        ),
    ])
}

const SIGNATURE: &str = "0xb0a1c2d3e4f5061728394a5b6c7d8e9fb0a1c2d3e4f5061728394a5b6c7d8e9f1c";

/// A runtime config and environment that would change every decision if
/// they were ever read; each read is counted.
#[derive(Default)]
struct HostileConfig {
    reads: Cell<usize>,
}

impl ConfigStore for HostileConfig {
    fn runtime_config(&self) -> Result<Json, String> {
        self.reads.set(self.reads.get() + 1);
        Ok(json(
            r#"{"resources":{"payment_manager_arn":"arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/prod-abcdefghij","payment_session_id":"payment-session-PRODabcdefghijk"},"policy":{"max_per_payment_usd":"500","allowed_networks":["eip155:8453"],"allow_any_recipient":true}}"#,
        ))
    }
    fn env(&self, name: &str) -> Option<String> {
        self.reads.set(self.reads.get() + 1);
        Some(format!("hostile-{name}"))
    }
}

struct World {
    clock: FakeClock,
    config: HostileConfig,
    payments: FakePaymentsApi,
    merchant: FakeMerchant,
    resolver: FixedResolver,
}

impl World {
    fn new(payments: FakePaymentsApi) -> Self {
        Self::resolving(payments, &["93.184.216.34"])
    }
    fn resolving(payments: FakePaymentsApi, addresses: &[&str]) -> Self {
        Self {
            clock: FakeClock::at_unix(NOW),
            config: HostileConfig::default(),
            payments,
            merchant: FakeMerchant::new(),
            resolver: FixedResolver::new().answer("merchant.test", addresses),
        }
    }
    fn deps(&self) -> Deps<'_> {
        Deps {
            config: &self.config,
            payments: &self.payments,
            merchant: &self.merchant,
            resolver: &self.resolver,
            clock: &self.clock,
        }
    }
    /// No world call but the payment ones.
    fn assert_untouched(&self) {
        assert_eq!(self.config.reads.get(), 0, "config or environment read");
        assert!(self.merchant.requests().is_empty(), "merchant contacted");
        assert!(self.clock.sleeps().is_empty(), "the bridge slept");
    }
}

fn pipe_bytes(bytes: Vec<u8>, deps: &Deps<'_>) -> (String, i32) {
    let (frame, status) = serve_frame(&mut Cursor::new(bytes), &|request| {
        handle_service(deps, request)
    });
    (String::from_utf8(frame).expect("ASCII frame"), status)
}

fn pipe(world: &World, request: &Json) -> (String, i32) {
    pipe_bytes(request.to_compact_string().into_bytes(), &world.deps())
}

fn result_of(frame: &str) -> Json {
    let document = parse_json(frame.trim_end_matches('\n')).expect("one document");
    assert_eq!(document.get("ok"), Some(&Json::Bool(true)), "{frame}");
    document.get("result").cloned().expect("result")
}

#[test]
fn t_br_svc_2_process_payment_maps_the_exact_service_request() {
    let world = World::new(FakePaymentsApi::signing());
    let (frame, status) = pipe(&world, &payment_request());
    assert_eq!(status, 0, "{frame}");
    assert_eq!(world.payments.calls(), 1);
    assert_eq!(world.payments.authorizations(), 1);
    assert_eq!(
        world.payments.opened_regions(),
        vec!["us-west-2".to_owned()]
    );
    let recorded = world.payments.recorded_requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].token, TOKEN_1000);
    let expected = format!(
        r#"{{"paymentManagerArn":"{MANAGER}","paymentSessionId":"{SESSION}","paymentInstrumentId":"{INSTRUMENT}","paymentType":"CRYPTO_X402","paymentInput":{{"cryptoX402":{{"version":"2","payload":{{"scheme":"exact","network":"eip155:84532","asset":"{ASSET}","payTo":"{PAY_TO}","maxTimeoutSeconds":60,"extra":{{"name":"USDC","version":"2","nonce":"n1"}},"amount":"1000"}}}}}},"clientToken":"{TOKEN_1000}"}}"#
    );
    assert_eq!(
        String::from_utf8(recorded[0].body.clone()).unwrap(),
        expected
    );
    // R-BR-OUT-3: exactly the reduced document, the signature once.
    let result = result_of(&frame);
    let Json::Object(members) = &result else {
        panic!("object")
    };
    assert_eq!(members.len(), 1, "{frame}");
    let payload = result
        .get("paymentOutput")
        .and_then(|v| v.get("cryptoX402"))
        .and_then(|v| v.get("payload"))
        .expect("payload");
    let signature = format!("0xsig0001{TOKEN_1000}");
    assert_eq!(
        payload.get("signature").and_then(Json::as_str),
        Some(signature.as_str())
    );
    assert_eq!(frame.matches(&signature).count(), 1);
    assert!(frame.ends_with("}}}}\n"));
    world.assert_untouched();
}

#[test]
fn t_br_tok_golden_vectors_through_the_bridge() {
    let token = |request: &Json| {
        let world = World::new(FakePaymentsApi::signing());
        let (frame, status) = pipe(&world, request);
        assert_eq!(status, 0, "{frame}");
        world.payments.recorded_requests()[0].token.clone()
    };
    assert_eq!(token(&payment_request()), TOKEN_1000);
    assert_eq!(
        token(&with_accepted("amount", Json::string("1001"))),
        TOKEN_1001
    );
    let mut extra = accepted().get("extra").cloned().unwrap();
    extra = with(&extra, "nonce", Json::string("n2"));
    assert_eq!(token(&with_accepted("extra", extra)), TOKEN_1000);
    assert_eq!(
        token(&with_accepted("maxTimeoutSeconds", Json::Int(120))),
        TOKEN_1000
    );
    let other_session = with(
        &payment_request(),
        "payment_session_id",
        Json::string("payment-session-TEST00000000002"),
    );
    let moved = token(&other_session);
    assert_ne!(moved, TOKEN_1000);
    assert_eq!(moved.len(), 64);
}

/// A connector over S4's real client, a scripted transport and a fake
/// signer: the request bytes are the wire bytes.
struct Wire {
    transport: FakeStreamingTransport,
    signer: FakeSigner,
    opened: RefCell<Vec<(String, Instant)>>,
}

impl Wire {
    fn new(transport: FakeStreamingTransport) -> Self {
        Self {
            transport,
            signer: FakeSigner::new(),
            opened: RefCell::new(Vec::new()),
        }
    }
}

impl PaymentsConnector for Wire {
    fn profile_region(&self) -> Option<String> {
        None
    }
    fn open<'s>(
        &'s self,
        region: &str,
        _manager_arn: &str,
        operation_deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String> {
        self.opened
            .borrow_mut()
            .push((region.to_owned(), operation_deadline));
        let client = PaymentClient::new(region, &self.transport, &self.signer)
            .map_err(|_| "region".to_owned())?;
        Ok(Box::new(client.with_operation_deadline(operation_deadline)))
    }
}

fn wire_deps<'a>(world: &'a World, wire: &'a Wire) -> Deps<'a> {
    Deps {
        payments: wire,
        ..world.deps()
    }
}

#[test]
fn t_br_svc_1_get_payment_session_golden_request_bytes() {
    let wire_body = session_doc(&[("unknown", Json::string("drop"))]).to_compact_string();
    let wire = Wire::new(FakeStreamingTransport::new().respond(
        200,
        &[],
        vec![FakeChunk::data(wire_body.as_bytes())],
    ));
    let world = World::new(FakePaymentsApi::default());
    let (frame, status) = pipe_bytes(
        session_request().to_compact_string().into_bytes(),
        &wire_deps(&world, &wire),
    );
    assert_eq!(status, 0, "{frame}");
    let requests = wire.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(wire.opened.borrow()[0].0, "eu-west-1");
    // The same request S4's reader sends for the request's region.
    let direct = FakeStreamingTransport::new().respond(
        200,
        &[],
        vec![FakeChunk::data(wire_body.as_bytes())],
    );
    let signer = FakeSigner::new();
    PaymentClient::new("eu-west-1", &direct, &signer)
        .unwrap()
        .get_session(MANAGER, SESSION, USER)
        .unwrap();
    let golden = &direct.requests()[0];
    let sent = &requests[0];
    assert_eq!(sent.url, golden.url);
    assert!(
        sent.url
            .starts_with("https://bedrock-agentcore.eu-west-1.amazonaws.com/"),
        "{}",
        sent.url
    );
    assert_eq!(
        String::from_utf8(sent.body.clone()).unwrap(),
        format!(r#"{{"paymentManagerArn":"{MANAGER}","paymentSessionId":"{SESSION}"}}"#)
    );
    assert_eq!(sent.body, golden.body);
    for header in [
        "authorization",
        "content-type",
        "x-amzn-bedrock-agentcore-payments-user-id",
        "x-amzn-bedrock-agentcore-payments-agent-name",
    ] {
        assert_eq!(
            sent.headers.get(header),
            golden.headers.get(header),
            "{header}"
        );
    }
    assert_eq!(
        sent.headers.get("authorization"),
        Some("FAKE-SIGNED eu-west-1/bedrock-agentcore")
    );
    // R-BR-OUT-2: model order, unknown member dropped, timestamps re-rendered.
    assert_eq!(
        frame,
        format!(
            "{{\"ok\":true,\"result\":{{\"paymentSession\":{{\"paymentSessionId\":\"{SESSION}\",\"paymentManagerArn\":\"{MANAGER}\",\"userId\":\"{USER}\",\"expiryTimeInMinutes\":60,\"createdAt\":\"2026-08-07T00:00:00+00:00\",\"updatedAt\":\"2026-08-07T00:00:00.500000+00:00\"}}}}}}\n"
        )
    );
    world.assert_untouched();
    assert!(
        world.resolver.lookups().is_empty(),
        "no DNS for a session read"
    );
}

#[test]
fn t_br_pre_every_pre_authorization_class_refuses_with_zero_calls() {
    let http = "http://merchant.test/pay/weather?city=sydney";
    let userinfo = "https://u:p@merchant.test/pay/weather?city=sydney";
    let with_url = |url: &str| {
        with(
            &with(&payment_request(), "request_url", Json::string(url)),
            "resource",
            Json::object(vec![("url", Json::string(url))]),
        )
    };
    let solana = with(
        &with(
            &payment_request(),
            "policy",
            json(&format!(
                r#"{{"networkPreferences":["solana:mainnet"],"allowedAssetsByNetwork":{{"solana:mainnet":["{ASSET}"]}},"allowAnyRecipient":true,"maxPaymentAmountAtomic":"100000"}}"#
            )),
        ),
        "accepted",
        with(&accepted(), "network", Json::string("solana:mainnet")),
    );
    let cases: Vec<(&str, Json)> = vec![
        ("R-ORIGIN http", with_url(http)),
        ("R-ORIGIN userinfo", with_url(userinfo)),
        (
            "allowedOrigins pin",
            with_policy(&with(
                &policy(),
                "allowedOrigins",
                json(r#"["https://other.test"]"#),
            )),
        ),
        (
            "R-POL recipient",
            with_accepted(
                "payTo",
                Json::string("0x2222222222222222222222222222222222222222"),
            ),
        ),
        (
            "R-POL asset",
            with_accepted(
                "asset",
                Json::string("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead"),
            ),
        ),
        (
            "R-POL network",
            with_accepted("network", Json::string("eip155:8453")),
        ),
        (
            "R-POL cap",
            with_policy(&with(
                &policy(),
                "maxPaymentAmountAtomic",
                Json::string("999"),
            )),
        ),
        (
            "R-POL scheme",
            with_accepted("scheme", Json::string("upto")),
        ),
        ("D4 non-EVM", solana),
        (
            "D11 timeout 0",
            with_accepted("maxTimeoutSeconds", Json::Int(0)),
        ),
        (
            "D11 timeout 3601",
            with_accepted("maxTimeoutSeconds", Json::Int(3601)),
        ),
        (
            "D11 extra",
            with_accepted(
                "extra",
                Json::object(vec![("pad", Json::string("x".repeat(5000)))]),
            ),
        ),
        ("D2 non-scalar", with_accepted("asset", json("[1]"))),
        (
            "R-AMT decimal",
            with_accepted("amount", Json::string("1.0")),
        ),
        (
            "R-AMT negative",
            with_accepted("amount", Json::string("-1")),
        ),
        (
            "R-AMT conflict",
            with_accepted("maxAmountRequired", Json::string("2000")),
        ),
        (
            "R-RESBIND",
            with(
                &payment_request(),
                "resource",
                json(r#"{"url":"https://other.test/pay/weather"}"#),
            ),
        ),
        (
            "R-RESBIND case",
            with(
                &payment_request(),
                "resource",
                json(r#"{"url":"https://Merchant.test/pay/weather?city=sydney"}"#),
            ),
        ),
    ];
    for (label, request) in cases {
        let world = World::new(FakePaymentsApi::signing());
        let (frame, status) = pipe(&world, &request);
        assert_eq!((frame.as_str(), status), (INPUT, 2), "{label}");
        assert_eq!(world.payments.calls(), 0, "{label}");
        assert_eq!(world.payments.opens(), 0, "{label}");
        world.assert_untouched();
    }
    for (label, addresses) in [
        ("non-public", &["10.0.0.7"][..]),
        ("loopback", &["127.0.0.1"][..]),
        ("unresolvable", &[][..]),
    ] {
        let world = World::resolving(FakePaymentsApi::signing(), addresses);
        let (frame, status) = pipe(&world, &payment_request());
        assert_eq!((frame.as_str(), status), (INPUT, 2), "{label}");
        assert_eq!(world.payments.calls(), 0, "{label}");
    }
}

/// G3 as a live world applies it: the connector refuses a test identity.
struct RefusingConnector {
    opens: Cell<usize>,
}

impl PaymentsConnector for RefusingConnector {
    fn profile_region(&self) -> Option<String> {
        None
    }
    fn open<'s>(
        &'s self,
        _region: &str,
        _manager_arn: &str,
        _operation_deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String> {
        self.opens.set(self.opens.get() + 1);
        Err(TEST_FIXTURE_REFUSAL.to_owned())
    }
}

#[test]
fn t_br_pre_g3_connector_refusal_is_an_input_error_with_zero_calls() {
    let world = World::new(FakePaymentsApi::signing());
    let refusing = RefusingConnector {
        opens: Cell::new(0),
    };
    let deps = Deps {
        payments: &refusing,
        ..world.deps()
    };
    for request in [payment_request(), session_request()] {
        let (frame, status) = pipe_bytes(request.to_compact_string().into_bytes(), &deps);
        assert_eq!((frame.as_str(), status), (INPUT, 2));
    }
    assert_eq!(refusing.opens.get(), 2);
    assert_eq!(world.payments.calls(), 0);
}

#[test]
fn t_br_post_service_failures_after_authorization() {
    let mut errors: Vec<PaymentError> = [
        "AccessDeniedException",
        "ConflictException",
        "InternalServerException",
        "ResourceNotFoundException",
        "ServiceQuotaExceededException",
        "ThrottlingException",
        "ValidationException",
        "UnrecognizedClientException",
        "UnknownError",
    ]
    .into_iter()
    .map(PaymentError::Service)
    .collect();
    errors.push(PaymentError::InvalidResponse);
    for class in [
        PaymentErrorClass::ConnectError,
        PaymentErrorClass::ConnectTimeout,
        PaymentErrorClass::ReadTimeout,
        PaymentErrorClass::RemoteProtocolError,
        PaymentErrorClass::NoCredentialsError,
        PaymentErrorClass::ClientError,
        PaymentErrorClass::RuntimeError,
    ] {
        errors.push(PaymentError::Transport(class));
    }
    for error in errors {
        for outcome in [FakeOutcome::Fail(error), FakeOutcome::RecordThenFail(error)] {
            let world = World::new(FakePaymentsApi::signing());
            world.payments.push_outcome(outcome.clone());
            let (frame, status) = pipe(&world, &payment_request());
            assert_eq!((frame.as_str(), status), (FAILURE, 1), "{outcome:?}");
            assert_eq!(world.payments.calls(), 1, "{outcome:?}");
            assert_eq!(world.payments.recorded_requests().len(), 1);
            world.assert_untouched();
        }
    }
}

#[test]
fn t_br_post_wire_failures_are_one_logical_call() {
    let ok = |status: &str, session: &str, payload: &Json| {
        Json::object(vec![
            (
                "processPaymentId",
                Json::string("12345678-abcd-1234-abcd-123456789abc"),
            ),
            ("paymentManagerArn", Json::string(MANAGER)),
            ("paymentSessionId", Json::string(session)),
            ("paymentInstrumentId", Json::string(INSTRUMENT)),
            ("paymentType", Json::string("CRYPTO_X402")),
            ("status", Json::string(status)),
            (
                "paymentOutput",
                Json::object(vec![(
                    "cryptoX402",
                    Json::object(vec![
                        ("version", Json::string("2")),
                        ("payload", payload.clone()),
                    ]),
                )]),
            ),
            ("createdAt", Json::Int(1)),
            ("updatedAt", Json::Int(1)),
        ])
        .to_compact_string()
        .into_bytes()
    };
    let big = Json::object(vec![("signature", Json::string("s".repeat(17_000)))]);
    let cases: Vec<(&str, u16, Vec<(&str, &str)>, Vec<u8>)> = vec![
        (
            "ConflictException",
            409,
            vec![("x-amzn-errortype", "ConflictException")],
            b"{\"message\":\"conflict\"}".to_vec(),
        ),
        (
            "unknown code",
            400,
            vec![("x-amzn-errortype", "SomethingNewException")],
            b"{}".to_vec(),
        ),
        ("64 KiB + 1 body", 200, vec![], vec![b' '; 65_537]),
        (
            "status mismatch",
            200,
            vec![],
            ok("FAILED", SESSION, &proof(&[])),
        ),
        (
            "echo mismatch",
            200,
            vec![],
            ok(
                "PROOF_GENERATED",
                "payment-session-TEST99999999999",
                &proof(&[]),
            ),
        ),
        (
            "missing payload",
            200,
            vec![],
            b"{\"status\":\"PROOF_GENERATED\"}".to_vec(),
        ),
        (
            "payload over 16 KiB",
            200,
            vec![],
            ok("PROOF_GENERATED", SESSION, &big),
        ),
    ];
    for (label, code, headers, body) in cases {
        let wire = Wire::new(FakeStreamingTransport::new().respond(
            code,
            &headers,
            vec![FakeChunk::data(&body)],
        ));
        let world = World::new(FakePaymentsApi::default());
        let (frame, status) = pipe_bytes(
            payment_request().to_compact_string().into_bytes(),
            &wire_deps(&world, &wire),
        );
        assert_eq!((frame.as_str(), status), (FAILURE, 1), "{label}");
        assert_eq!(
            wire.transport.requests().len(),
            1,
            "{label}: one logical call"
        );
        assert!(!frame.contains(SIGNATURE));
    }
}

#[test]
fn t_br_post_proof_verification_failures_send_no_proof() {
    let after = |seconds: i64| (NOW + seconds).to_string();
    for (label, authorization) in [
        (
            "to",
            vec![("to", "0x3333333333333333333333333333333333333333")],
        ),
        ("value", vec![("value", "999")]),
        ("expired validBefore", vec![("validBefore", "1")]),
    ] {
        let world = World::new(FakePaymentsApi::with_proof(proof(&authorization)));
        let (frame, status) = pipe(&world, &payment_request());
        assert_eq!((frame.as_str(), status), (FAILURE, 1), "{label}");
        assert_eq!(world.payments.calls(), 1);
        world.assert_untouched();
    }
    let late = after(16);
    let world = World::new(FakePaymentsApi::with_proof(proof(&[("validAfter", &late)])));
    let (frame, status) = pipe(&world, &payment_request());
    assert_eq!((frame.as_str(), status), (FAILURE, 1), "validAfter 16 s");
    assert!(world.clock.sleeps().is_empty());
    let soon = after(10);
    let world = World::new(FakePaymentsApi::with_proof(proof(&[("validAfter", &soon)])));
    let (frame, status) = pipe(&world, &payment_request());
    assert_eq!(status, 0, "validAfter 10 s: {frame}");
    assert!(frame.contains(SIGNATURE));
    assert!(world.clock.sleeps().is_empty(), "the wait stays in TS");
    world.assert_untouched();
}

/// An `AgentCore` that stalls past any deadline, then times out.
struct Stall {
    clock: FakeClock,
    deadlines: RefCell<Vec<Instant>>,
}

impl PaymentsApi for Stall {
    fn get_session(&self, _: &str, _: &str, _: &str) -> Result<Json, PaymentError> {
        self.clock.advance(Duration::from_secs(30));
        Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout))
    }
    fn create_session(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: i64,
        _: &str,
    ) -> Result<Json, PaymentError> {
        Err(PaymentError::InvalidResponse)
    }
    fn create_instrument(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Json, PaymentError> {
        Err(PaymentError::InvalidResponse)
    }
    fn process_payment(&self, _: &ProcessPaymentRequest) -> Result<PaymentProof, PaymentError> {
        self.clock.advance(Duration::from_secs(30));
        Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout))
    }
}

impl PaymentsConnector for Stall {
    fn profile_region(&self) -> Option<String> {
        None
    }
    fn open<'s>(
        &'s self,
        _region: &str,
        _manager_arn: &str,
        operation_deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String> {
        self.deadlines.borrow_mut().push(operation_deadline);
        Ok(Box::new(self))
    }
}

/// A resolver whose lookup takes `delay` of fake time.
struct SlowResolver {
    clock: FakeClock,
    delay: Duration,
}

impl HostResolver for SlowResolver {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        self.clock.advance(self.delay);
        Ok(vec!["93.184.216.34".parse().unwrap()])
    }
}

#[test]
fn t_br_dl_the_25_second_deadline() {
    for request in [payment_request(), session_request()] {
        let world = World::new(FakePaymentsApi::default());
        let stall = Stall {
            clock: world.clock.clone(),
            deadlines: RefCell::new(Vec::new()),
        };
        let start = world.clock.now_instant();
        let deps = Deps {
            payments: &stall,
            ..world.deps()
        };
        let (frame, status) = pipe_bytes(request.to_compact_string().into_bytes(), &deps);
        assert_eq!((frame.as_str(), status), (FAILURE, 1));
        assert_eq!(*stall.deadlines.borrow(), vec![start + DEADLINE]);
        assert_eq!(DEADLINE, Duration::from_secs(25));
        assert!(world.clock.sleeps().is_empty());
    }
    // A deadline that passes before authorization is a refusal: exit 2.
    let world = World::new(FakePaymentsApi::signing());
    let slow = SlowResolver {
        clock: world.clock.clone(),
        delay: Duration::from_secs(26),
    };
    let deps = Deps {
        resolver: &slow,
        ..world.deps()
    };
    let (frame, status) = pipe_bytes(payment_request().to_compact_string().into_bytes(), &deps);
    assert_eq!((frame.as_str(), status), (INPUT, 2));
    assert_eq!(world.payments.calls(), 0);
    assert_eq!(world.payments.opens(), 0);
    // Within the deadline the same slow lookup pays.
    let world = World::new(FakePaymentsApi::signing());
    let slow = SlowResolver {
        clock: world.clock.clone(),
        delay: Duration::from_secs(20),
    };
    let deps = Deps {
        resolver: &slow,
        ..world.deps()
    };
    let (_, status) = pipe_bytes(payment_request().to_compact_string().into_bytes(), &deps);
    assert_eq!(status, 0);
    assert_eq!(world.payments.calls(), 1);
}

#[test]
fn t_br_nocfg_decisions_depend_only_on_stdin() {
    let session = session_doc(&[]);
    let mut frames = Vec::new();
    for _ in 0..2 {
        let world =
            World::new(FakePaymentsApi::signing().with_session_for(SESSION, session.clone()));
        let paid = pipe(&world, &payment_request());
        let read = pipe(&world, &session_request());
        assert_eq!(paid.1, 0, "{}", paid.0);
        assert_eq!(read.1, 0, "{}", read.0);
        world.assert_untouched();
        assert_eq!(
            world.payments.opened_regions(),
            vec!["us-west-2", "eu-west-1"]
        );
        assert_eq!(
            world.payments.session_reads(),
            vec![(MANAGER.to_owned(), SESSION.to_owned(), USER.to_owned())]
        );
        frames.push((paid, read, world.payments.recorded_requests()));
    }
    assert_eq!(frames[0], frames[1]);
}

#[test]
fn t_br_quiet_no_stderr_on_any_path() {
    let world = World::new(FakePaymentsApi::signing().with_session_for(SESSION, session_doc(&[])));
    let deps = world.deps();
    let service = |request: &Json| handle_service(&deps, request);
    let sanitize = r#"{"operation":"sanitize_output","candidate":{"content":[{"type":"text","text":"{\"paid\":true}"}],"details":{"paid":true}},"secrets":["0123456789abcdef"]}"#;
    let cases: Vec<(Vec<u8>, i32)> = vec![
        (Vec::new(), 2),
        (b"\xff\xfe{".to_vec(), 2),
        (br#"{"operation":"not-supported"}"#.to_vec(), 2),
        (sanitize.as_bytes().to_vec(), 0),
        (session_request().to_compact_string().into_bytes(), 0),
        (payment_request().to_compact_string().into_bytes(), 0),
        (
            with_accepted("payTo", Json::string("0x9"))
                .to_compact_string()
                .into_bytes(),
            2,
        ),
    ];
    for (bytes, expected) in cases {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        serve_quiet(&mut Cursor::new(bytes.clone()), &mut output, &service).unwrap();
        let status = output.exit_status();
        drop(output);
        assert_eq!(
            i32::from(status),
            expected,
            "{}",
            String::from_utf8_lossy(&out)
        );
        assert!(err.is_empty(), "stderr: {}", String::from_utf8_lossy(&err));
        assert!(out.ends_with(b"\n") && out.is_ascii());
        assert_eq!(out.iter().filter(|b| **b == b'\n').count(), 1, "one frame");
    }
    // A panicking world is the AgentCore failure, still with no stderr.
    let panicking = |_: &Json| -> Frame { panic!("synthetic bridge panic") };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut output = Output::with_diagnostics(&mut out, &mut err);
    serve_quiet(
        &mut Cursor::new(payment_request().to_compact_string().into_bytes()),
        &mut output,
        &panicking,
    )
    .unwrap();
    assert_eq!(output.exit_status(), 1);
    drop(output);
    assert_eq!(String::from_utf8(out).unwrap(), FAILURE);
    assert!(err.is_empty());
}

#[test]
fn t_br_scrub_a_service_echo_of_the_session_fails() {
    // The session id echoed outside `paymentSessionId`: exit 1.
    let echo = session_doc(&[(
        "limits",
        json(&format!(
            r#"{{"maxSpendAmount":{{"value":"10","currency":"{SESSION}"}}}}"#
        )),
    )]);
    let world = World::new(FakePaymentsApi::default().with_session_for(SESSION, echo));
    let (frame, status) = pipe(&world, &session_request());
    assert_eq!((frame.as_str(), status), (FAILURE, 1));
    // Only as `paymentSessionId`: exit 0, today's result.
    let world = World::new(FakePaymentsApi::default().with_session_for(SESSION, session_doc(&[])));
    let (frame, status) = pipe(&world, &session_request());
    assert_eq!(status, 0, "{frame}");
    assert_eq!(frame.matches(SESSION).count(), 1);
    // A proof echoing the session id: the payload path is exempt from the
    // proof's own match only.
    let mut echoing = proof(&[]);
    echoing = with(&echoing, "memo", Json::string(SESSION));
    let world = World::new(FakePaymentsApi::with_proof(echoing));
    let (frame, status) = pipe(&world, &payment_request());
    assert_eq!((frame.as_str(), status), (FAILURE, 1));
}

#[test]
fn t_oc_final_route_sanitize_reaches_no_world_and_no_payment() {
    let world = World::new(FakePaymentsApi::signing());
    let deps = world.deps();
    let services = Cell::new(0_usize);
    let service = |request: &Json| {
        services.set(services.get() + 1);
        handle_service(&deps, request)
    };
    let candidate = |body: &str, secrets: &str| {
        format!(
            r#"{{"operation":"sanitize_output","candidate":{{"content":[{{"type":"text","text":"{{\"body\":\"{body}\",\"paid\":true}}"}}],"details":{{"body":"{body}","paid":true}}}},"secrets":{secrets}}}"#
        )
    };
    let secret = format!("[\"{SIGNATURE}\"]");
    for (label, request, expected) in [
        ("clean", candidate("hello", &secret), 0),
        ("redacted", candidate(SIGNATURE, &secret), 0),
        ("short registry", candidate("hello", r#"["short"]"#), 2),
        (
            "malformed",
            r#"{"operation":"sanitize_output","candidate":{}}"#.to_owned(),
            2,
        ),
    ] {
        let (frame, status) = serve_frame(&mut Cursor::new(request.into_bytes()), &service);
        assert_eq!(
            status,
            expected,
            "{label}: {}",
            String::from_utf8_lossy(&frame)
        );
    }
    assert_eq!(
        services.get(),
        0,
        "sanitize_output reached the service world"
    );
    assert_eq!(world.payments.calls(), 0);
    assert_eq!(world.payments.opens(), 0);
    assert!(world.resolver.lookups().is_empty());
    world.assert_untouched();
    // A service operation does reach it, once.
    let _ = serve_frame(
        &mut Cursor::new(session_request().to_compact_string().into_bytes()),
        &service,
    );
    assert_eq!(services.get(), 1);
}
