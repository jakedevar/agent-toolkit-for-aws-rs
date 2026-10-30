//! x402 S5 (#5352): the payment flow over the public, harmless fixtures
//! (design `docs/migration/x402-payments-design.md` sections 3, 4 and 7).
//!
//! Every world here is in memory: `FakePaymentsApi` (fixture account only),
//! `FakeMerchant`, `FixedResolver`, `FakeClock` and a `TempConfigStore`
//! rooted in a temporary directory. No test opens a socket, reads AWS
//! credentials or can spend.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::cell::Cell;
use std::net::IpAddr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport};
use toolkit_cli::agents_pay::agentcore::{
    FakeOutcome, FakePaymentsApi, PaymentError, PaymentErrorClass, PaymentProof, PaymentsApi,
    ProcessPaymentRequest,
};
use toolkit_cli::agents_pay::deps::fixtures::{FakeClock, FakeMerchant, TempConfigStore};
use toolkit_cli::agents_pay::deps::Deps;
use toolkit_cli::agents_pay::flow;
use toolkit_cli::agents_pay::net::StreamingMerchant;
use toolkit_cli::agents_pay::policy::{FixedResolver, UNIFORM_REFUSAL};
use toolkit_cli::agents_pay::proof::{MISMATCH, OUTSIDE_WINDOW};
use toolkit_cli::agents_pay::vault::Vault;
use toolkit_core::{parse_json, Json};

const URL: &str = "https://merchant.test/v1/x402-test";
const ORIGIN: &str = "https://merchant.test";
const USDC: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
const PAY_TO: &str = "0x1111111111111111111111111111111111111111";
const OTHER_PAY_TO: &str = "0x3333333333333333333333333333333333333333";
const MANAGER: &str =
    "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/test-abcdefghij";
/// R-TOK golden vector: session `ps-1`, this URL, no purchase id.
const GOLDEN: &str = "c4293dfd364640b953d5ab169ea31f99eed39e706ace87581a511c65b06c2a87";
const NOW: i64 = 1_800_000_000;
const EXHAUSTED_TAIL: &str = "times but the merchant still returns 402 \u{2014} usually transient on-chain settlement. The same authorization was replayed each time, so there is no double charge. Retry shortly, or raise X402_MAX_PAYMENT_ATTEMPTS.";

fn json(text: &str) -> Json {
    parse_json(text).unwrap()
}

fn entry(amount: &str, pay_to: &str, extra: &str) -> String {
    format!(
        r#"{{"scheme":"exact","network":"eip155:84532","asset":"{USDC}","payTo":"{pay_to}","amount":"{amount}"{extra}}}"#
    )
}

fn challenge_with(amount: &str, pay_to: &str, extra: &str) -> Vec<u8> {
    format!(
        r#"{{"x402Version":2,"accepts":[{}]}}"#,
        entry(amount, pay_to, extra)
    )
    .into_bytes()
}

fn challenge() -> Vec<u8> {
    challenge_with("100000", PAY_TO, "")
}

/// The fixture config; a key in `resources_extra` replaces its default.
fn config_with(policy_extra: &str, resources_extra: &str) -> Json {
    let defaults = [
        ("payment_manager_arn", MANAGER),
        (
            "payment_instrument_id",
            "payment-instrument-TEST12345678901",
        ),
        ("payment_session_id", "ps-1"),
        ("user_id", "agents-pay-0123456789ab"),
    ];
    let resources: Vec<String> = defaults
        .iter()
        .filter(|(key, _)| !resources_extra.contains(&format!("\"{key}\"")))
        .map(|(key, value)| format!(r#""{key}":"{value}""#))
        .collect();
    let resources = resources.join(",");
    json(&format!(
        r#"{{"resources":{{{resources}{resources_extra}}},
          "policy":{{"max_per_payment_usd":"0.50","allowed_networks":["eip155:84532"],
            "allowed_assets":{{"eip155:84532":["{USDC}"]}},
            "allowed_recipients":["{PAY_TO}","{OTHER_PAY_TO}"],
            "allowed_schemes":["exact"]{policy_extra}}}}}"#
    ))
}

fn config() -> Json {
    config_with("", "")
}

fn public() -> Vec<IpAddr> {
    vec!["93.184.216.34".parse().unwrap()]
}

/// One in-memory world, reused across invocations for T-RESTART.
struct World {
    store: TempConfigStore,
    api: FakePaymentsApi,
    merchant: FakeMerchant,
    resolver: FixedResolver,
    clock: FakeClock,
}

impl World {
    fn new(label: &str) -> Self {
        Self::with(
            TempConfigStore::new(label, &config()),
            FakePaymentsApi::signing(),
        )
    }
    fn with(store: TempConfigStore, api: FakePaymentsApi) -> Self {
        Self {
            store,
            api,
            merchant: FakeMerchant::new(),
            resolver: FixedResolver::new().with_host("merchant.test", public()),
            clock: FakeClock::at_unix(NOW),
        }
    }
    fn deps(&self) -> Deps<'_> {
        Deps {
            config: &self.store,
            payments: &self.api,
            merchant: &self.merchant,
            resolver: &self.resolver,
            clock: &self.clock,
        }
    }
    fn fetch(&self) -> Json {
        flow::fetch(&self.deps(), URL, None, "GET")
    }
    fn headers(&self) -> Vec<(String, String)> {
        self.merchant
            .requests()
            .into_iter()
            .filter_map(|request| request.proof)
            .collect()
    }
}

fn text<'a>(value: &'a Json, key: &str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap_or_default()
}

fn assert_refused(value: &Json, reason: &str) {
    assert_eq!(value.get("refused"), Some(&Json::Bool(true)), "{value:?}");
    assert_eq!(value.get("paid"), Some(&Json::Bool(false)));
    assert_eq!(text(value, "reason"), reason);
}

// ------------------------------------------------------------- T-RETRY

#[test]
fn t_retry_one_authorization_over_replays_with_the_golden_token_and_equal_headers() {
    let world = World::new("retry");
    world
        .merchant
        .respond(402, &[], &challenge())
        .respond(402, &[], b"")
        .respond(402, &[], b"")
        .respond(
            200,
            &[("content-type", "Text/Plain; charset=utf-8")],
            b"paid!",
        );
    let out = world.fetch();
    assert_eq!(out.get("paid"), Some(&Json::Bool(true)), "{out:?}");
    assert_eq!(out.get("status_code"), Some(&Json::Int(200)));
    assert_eq!(world.api.calls(), 1);
    assert_eq!(world.api.authorizations(), 1);
    let recorded = world.api.recorded_requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].token, GOLDEN);
    let headers = world.headers();
    assert_eq!(headers.len(), 3);
    assert!(headers.iter().all(|header| header == &headers[0]));
    assert_eq!(headers[0].0, "PAYMENT-SIGNATURE");
    // D13: a fixed 1 s pause between merchant replays, never before the first.
    assert_eq!(
        world.clock.sleeps(),
        vec![Duration::from_secs(1), Duration::from_secs(1)]
    );
    let receipt = out.get("receipt").unwrap();
    assert_eq!(text(receipt, "amount_usd"), "0.1");
    assert_eq!(text(receipt, "network"), "eip155:84532");
    assert_eq!(text(receipt, "resource"), URL);
    assert_eq!(text(&out, "content_type"), "text/plain");
    assert_eq!(out.get("bytes"), Some(&Json::Int(5)));
    assert_eq!(out.get("body_returned"), Some(&Json::Bool(false)));
    let rendered = flow::render(&out);
    assert!(rendered.starts_with(r#"{"status_code": 200, "paid": true, "untrusted": true, "receipt": {"amount_usd": "0.1""#), "{rendered}");
    assert!(!rendered.contains(&headers[0].1));
}

#[test]
fn t_retry_a_hostile_402_on_a_replay_changes_nothing() {
    let world = World::new("hostile");
    world.merchant.respond(402, &[], &challenge());
    for _ in 0..5 {
        world
            .merchant
            .respond(402, &[], &challenge_with("400000", OTHER_PAY_TO, ""));
    }
    let out = world.fetch();
    assert_eq!(out.get("refused"), Some(&Json::Bool(false)), "{out:?}");
    assert_eq!(out.get("paid"), Some(&Json::Bool(false)));
    assert_eq!(out.get("status_code"), Some(&Json::Int(402)));
    assert_eq!(out.get("attempts"), Some(&Json::Int(5)));
    assert_eq!(
        text(&out, "reason"),
        format!("Paid and replayed 5 {EXHAUSTED_TAIL}")
    );
    assert_eq!(world.api.calls(), 1);
    assert_eq!(world.api.recorded_requests()[0].token, GOLDEN);
    let headers = world.headers();
    assert_eq!(headers.len(), 5);
    assert!(headers.iter().all(|header| header == &headers[0]));
    assert_eq!(
        flow::render(&out),
        format!(
            r#"{{"paid": false, "refused": false, "status_code": 402, "attempts": 5, "reason": "Paid and replayed 5 times but the merchant still returns 402 \u2014 usually transient on-chain settlement. The same authorization was replayed each time, so there is no double charge. Retry shortly, or raise X402_MAX_PAYMENT_ATTEMPTS."}}"#
        )
    );
}

#[test]
fn transient_settlement_attempt_tunable_is_bounded() {
    for (value, attempts) in [
        ("0", 5),
        ("-1", 5),
        ("abc", 5),
        ("99999", 5),
        ("nan", 5),
        ("3", 3),
        ("10", 10),
    ] {
        let store =
            TempConfigStore::new("tunable", &config()).with_env("X402_MAX_PAYMENT_ATTEMPTS", value);
        let world = World::with(store, FakePaymentsApi::signing());
        world.merchant.respond(402, &[], &challenge());
        for _ in 0..12 {
            world.merchant.respond(402, &[], b"");
        }
        let out = world.fetch();
        assert_eq!(out.get("attempts"), Some(&Json::Int(attempts)), "{value}");
        assert_eq!(world.headers().len(), usize::try_from(attempts).unwrap());
        assert_eq!(world.api.calls(), 1, "{value}");
    }
}

// ---------------------------------------------- T-ORDER-PRE (flow part)

fn assert_zero_authorizations(world: &World) {
    assert_eq!(world.api.calls(), 0);
    assert_eq!(world.api.opens(), 0);
    assert_eq!(world.headers().len(), 0);
}

#[test]
fn t_order_pre_configuration_and_resource_refusals_make_zero_requests() {
    let world = World::with(
        TempConfigStore::empty("missing"),
        FakePaymentsApi::signing(),
    );
    let out = world.fetch();
    assert!(
        text(&out, "reason").starts_with("No payment config at "),
        "{out:?}"
    );
    assert_eq!(world.merchant.requests().len(), 0);
    assert_zero_authorizations(&world);

    for (resources, reason) in [
        (
            r#","payment_instrument_id":"""#,
            "Missing payment resource 'payment_instrument_id'. Add it to the config (toolkit agents-pay admin init-config) or set PAYMENT_INSTRUMENT_ID. Provision resources with the trusted setup path first.",
        ),
        (
            r#","user_id":["x"]"#,
            "Payment config resource 'user_id' must be a string.",
        ),
    ] {
        let world = World::with(
            TempConfigStore::new("resources", &config_with("", resources)),
            FakePaymentsApi::signing(),
        );
        world.merchant.respond(402, &[], &challenge());
        let out = world.fetch();
        assert_refused(&out, reason);
        assert_zero_authorizations(&world);
    }
}

#[test]
fn t_order_pre_origin_refusals_happen_before_any_merchant_request() {
    for (url, resolver, reason) in [
        (
            "http://merchant.test/x",
            FixedResolver::new(),
            "Only https:// payment URLs are allowed.",
        ),
        (
            "https:///x",
            FixedResolver::new(),
            "Payment URL has no host.",
        ),
        (
            "https://user:pw@merchant.test/x",
            FixedResolver::new(),
            "Payment URL must not contain embedded credentials.",
        ),
        (
            "https://merchant.test/x",
            FixedResolver::new(),
            "Payment URL hostname does not resolve.",
        ),
        (
            "https://merchant.test/x",
            FixedResolver::new().with_host("merchant.test", vec!["10.0.0.1".parse().unwrap()]),
            "Payment URL resolves to a non-public address.",
        ),
        (
            "https://169.254.169.254/latest/meta-data/",
            FixedResolver::new(),
            "Payment URL resolves to a non-public address.",
        ),
    ] {
        let mut world = World::new("origin");
        world.resolver = resolver;
        let out = flow::fetch(&world.deps(), url, None, "GET");
        assert_refused(&out, reason);
        assert_eq!(world.merchant.requests().len(), 0, "{url}");
        assert_zero_authorizations(&world);
    }
    let world = World::with(
        TempConfigStore::new(
            "pinned",
            &config_with(r#","allowed_origins":["https://elsewhere.test"]"#, ""),
        ),
        FakePaymentsApi::signing(),
    );
    let out = world.fetch();
    assert_refused(
        &out,
        "Origin https://merchant.test is not in the configured allowed_origins.",
    );
    assert_eq!(world.merchant.requests().len(), 0);
    assert_zero_authorizations(&world);
}

#[test]
fn t_order_pre_method_gate_refuses_before_the_probe() {
    for method in ["POST", "put", "DELETE", "PATCH"] {
        let world = World::new("method");
        world.merchant.respond(402, &[], &challenge());
        let out = flow::fetch(&world.deps(), URL, None, method);
        assert!(
            text(&out, "reason").starts_with(&format!(
                "Method {} is not allowed; this skill fetches paid content with GET or HEAD.",
                method.to_uppercase()
            )),
            "{out:?}"
        );
        assert_eq!(world.merchant.requests().len(), 0);
        assert_zero_authorizations(&world);
    }
}

#[test]
fn t_order_pre_probe_and_challenge_refusals_make_zero_authorizations() {
    let not_402 = World::new("not402");
    not_402
        .merchant
        .respond(200, &[("content-type", "application/json")], b"{}");
    let out = not_402.fetch();
    assert_eq!(
        flow::render(&out),
        r#"{"status_code": 200, "paid": false, "untrusted": true, "content_type": "application/json", "body_sha256": "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a", "bytes": 2, "body_returned": false, "note": "Body withheld: paid publisher content is untrusted and must not enter the payment-capable model context. Use a separate no-payment/no-network analysis context if summarisation is required. To opt in, set 'return_body: true' in the policy section of the config file."}"#
    );
    assert_eq!(not_402.merchant.requests().len(), 1);
    assert_zero_authorizations(&not_402);

    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            b"not json".to_vec(),
            "402 response contains no parseable x402 challenge.",
        ),
        (
            br#"{"x402Version":2}"#.to_vec(),
            "402 challenge is missing the required 'accepts' array.",
        ),
        (
            challenge_with("100000", "0x9999999999999999999999999999999999999999", ""),
            UNIFORM_REFUSAL,
        ),
        (challenge_with("900000", PAY_TO, ""), UNIFORM_REFUSAL),
    ];
    for (body, reason) in cases {
        let world = World::new("challenge");
        world.merchant.respond(402, &[], &body);
        let out = world.fetch();
        assert_refused(&out, reason);
        assert_eq!(world.merchant.requests().len(), 1);
        assert_zero_authorizations(&world);
    }

    let failed = World::new("probe-fail");
    failed.merchant.fail("ConnectError during payment flow.");
    assert_refused(&failed.fetch(), "ConnectError during payment flow.");
    assert_zero_authorizations(&failed);
}

// ------------------------------------------------------- T-ORDER-POST

fn post_world(label: &str, outcome: FakeOutcome) -> World {
    let world = World::new(label);
    world.api.push_outcome(outcome);
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"never");
    world
}

#[test]
fn t_order_post_service_failures_make_one_authorization_and_no_proof_send() {
    for (outcome, reason) in [
        (
            FakeOutcome::Fail(PaymentError::Service("ValidationException")),
            "ClientError during payment flow.",
        ),
        (
            FakeOutcome::Fail(PaymentError::Service("ConflictException")),
            "ClientError during payment flow.",
        ),
        (
            FakeOutcome::Fail(PaymentError::Service("ThrottlingException")),
            "ClientError during payment flow.",
        ),
        (
            FakeOutcome::Fail(PaymentError::InvalidResponse),
            "Payment header generation returned an unexpected shape.",
        ),
        (
            FakeOutcome::Fail(PaymentError::Transport(PaymentErrorClass::ReadTimeout)),
            "ReadTimeout during payment flow.",
        ),
        (
            FakeOutcome::Fail(PaymentError::Transport(
                PaymentErrorClass::NoCredentialsError,
            )),
            "NoCredentialsError during payment flow.",
        ),
    ] {
        let world = post_world("post", outcome);
        let out = world.fetch();
        assert_refused(&out, reason);
        assert_eq!(world.api.calls(), 1, "{reason}");
        let recorded = world.api.recorded_requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].token, GOLDEN);
        assert_eq!(world.headers().len(), 0, "{reason}");
        assert_eq!(world.merchant.requests().len(), 1);
    }
}

/// An `AgentCore` call that takes longer than the whole operation budget.
struct SlowApi<'a> {
    inner: &'a FakePaymentsApi,
    clock: &'a FakeClock,
}
impl PaymentsApi for SlowApi<'_> {
    fn get_session(&self, manager: &str, session: &str, user: &str) -> Result<Json, PaymentError> {
        self.inner.get_session(manager, session, user)
    }
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError> {
        self.inner
            .create_session(manager, user, budget, expiry, token)
    }
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError> {
        self.inner
            .create_instrument(manager, user, connector, network, email, token)
    }
    fn process_payment(
        &self,
        request: &ProcessPaymentRequest,
    ) -> Result<PaymentProof, PaymentError> {
        let proof = self.inner.process_payment(request);
        self.clock.advance(Duration::from_secs(10_000));
        proof
    }
}
impl toolkit_cli::agents_pay::deps::PaymentsConnector for SlowApi<'_> {
    fn profile_region(&self) -> Option<String> {
        None
    }
    fn open<'s>(
        &'s self,
        _region: &str,
        _manager_arn: &str,
        _deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String> {
        Ok(Box::new(SlowApi {
            inner: self.inner,
            clock: self.clock,
        }))
    }
}

#[test]
fn t_order_post_deadline_after_authorization_sends_no_proof() {
    let world = World::new("slow");
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"never");
    let slow = SlowApi {
        inner: &world.api,
        clock: &world.clock,
    };
    let deps = Deps {
        payments: &slow,
        ..world.deps()
    };
    let out = flow::fetch(&deps, URL, None, "GET");
    assert_refused(&out, "ReadTimeout during payment flow.");
    assert_eq!(world.api.calls(), 1);
    assert_eq!(world.headers().len(), 0);
}

#[test]
fn t_order_post_merchant_outcomes_after_authorization() {
    // A replay transport failure ends the invocation after one send.
    let failed = World::new("replay-fail");
    failed.merchant.respond(402, &[], &challenge());
    failed.merchant.fail("ReadTimeout during payment flow.");
    failed.merchant.respond(200, &[], b"never");
    assert_refused(&failed.fetch(), "ReadTimeout during payment flow.");
    assert_eq!(failed.api.calls(), 1);
    assert_eq!(failed.headers().len(), 1);

    // A 3xx on a replay is reported as sent and ends the loop.
    let redirect = World::new("redirect");
    redirect.merchant.respond(402, &[], &challenge());
    redirect
        .merchant
        .respond(302, &[("location", "https://evil.test/")], b"");
    redirect.merchant.respond(200, &[], b"never");
    let out = redirect.fetch();
    assert_eq!(out.get("paid"), Some(&Json::Bool(false)));
    assert_eq!(out.get("status_code"), Some(&Json::Int(302)));
    assert!(!flow::render(&out).contains("evil.test"));
    assert_eq!(redirect.headers().len(), 1);
    assert_eq!(redirect.api.calls(), 1);
}

// -------------------------------------------------------------- T-PROOF

fn proof(to: &str, value: &str, valid_after: &str, valid_before: &str) -> Json {
    json(&format!(
        r#"{{"signature":"0xfeedfacefeedface","authorization":{{"from":"0x2222222222222222222222222222222222222222","to":"{to}","value":"{value}","validAfter":"{valid_after}","validBefore":"{valid_before}","nonce":"0x01"}}}}"#
    ))
}

fn proof_world(label: &str, payload: Json) -> World {
    let world = World::with(
        TempConfigStore::new(label, &config()),
        FakePaymentsApi::with_proof(payload),
    );
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    world
}

#[test]
fn t_proof_mismatched_terms_are_never_sent() {
    let before = (NOW + 60).to_string();
    for payload in [
        proof(OTHER_PAY_TO, "100000", "0", &before),
        proof(PAY_TO, "100001", "0", &before),
        proof(PAY_TO, "0100000", "0", &before),
        proof(PAY_TO, "100000", "0", &NOW.to_string()),
        proof(PAY_TO, "100000", "0", &(NOW - 1).to_string()),
        proof(PAY_TO, "100000", "0", "soon"),
        json(r#"{"signature":"0xfeedfacefeedface"}"#),
        json(r#"{"authorization":"x"}"#),
    ] {
        let world = proof_world("mismatch", payload);
        assert_refused(&world.fetch(), MISMATCH);
        assert_eq!(world.api.calls(), 1);
        assert_eq!(world.headers().len(), 0);
    }
    // `to` is compared case-insensitively; integer fields are accepted.
    let world = proof_world(
        "case",
        json(&format!(
            r#"{{"signature":"0xfeedfacefeedface","authorization":{{"to":"{}","value":100000,"validAfter":0,"validBefore":{}}}}}"#,
            PAY_TO.to_uppercase().replace("0X", "0x"),
            NOW + 60
        )),
    );
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    assert_eq!(world.headers().len(), 1);
}

#[test]
fn t_proof_valid_after_waits_at_most_fifteen_seconds() {
    let before = (NOW + 300).to_string();
    let world = proof_world(
        "wait",
        proof(PAY_TO, "100000", &(NOW + 14).to_string(), &before),
    );
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    assert_eq!(world.clock.sleeps(), vec![Duration::from_secs(15)]);
    assert_eq!(world.headers().len(), 1);

    let world = proof_world(
        "late",
        proof(PAY_TO, "100000", &(NOW + 15).to_string(), &before),
    );
    assert_refused(&world.fetch(), OUTSIDE_WINDOW);
    assert!(world.clock.sleeps().is_empty());
    assert_eq!(world.headers().len(), 0);

    // The wait must not outlive the authorization itself.
    let short = (NOW + 5).to_string();
    let world = proof_world(
        "expires",
        proof(PAY_TO, "100000", &(NOW + 4).to_string(), &short),
    );
    assert_refused(&world.fetch(), OUTSIDE_WINDOW);
    assert_eq!(world.headers().len(), 0);
}

// --------------------------------------------------------------- T-HDR

const V2_HEADER: &str = "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0ZWQiOnsic2NoZW1lIjoiZXhhY3QiLCJuZXR3b3JrIjoiZWlwMTU1Ojg0NTMyIiwiYXNzZXQiOiIweDAzNmNiZDUzODQyYzU0MjY2MzRlNzkyOTU0MWVjMjMxOGYzZGNmN2UiLCJwYXlUbyI6IjB4MTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMSIsIm1heFRpbWVvdXRTZWNvbmRzIjo2MCwiYW1vdW50IjoiMTAwMDAwIn0sInBheWxvYWQiOnsic2lnbmF0dXJlIjoiMHhmZWVkZmFjZWZlZWRmYWNlIiwiYXV0aG9yaXphdGlvbiI6eyJmcm9tIjoiMHgyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyIiwidG8iOiIweDExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTEiLCJ2YWx1ZSI6IjEwMDAwMCIsInZhbGlkQWZ0ZXIiOiIwIiwidmFsaWRCZWZvcmUiOiIxOTAwMDAwMDAwIiwibm9uY2UiOiIweDAxIn19LCJyZXNvdXJjZSI6eyJ1cmwiOiJodHRwczovL21lcmNoYW50LnRlc3QvdjEveDQwMi10ZXN0In19";
const V1_HEADER: &str = "eyJ4NDAyVmVyc2lvbiI6MSwic2NoZW1lIjoiZXhhY3QiLCJuZXR3b3JrIjoiZWlwMTU1Ojg0NTMyIiwicGF5bG9hZCI6eyJzaWduYXR1cmUiOiIweGZlZWRmYWNlZmVlZGZhY2UiLCJhdXRob3JpemF0aW9uIjp7ImZyb20iOiIweDIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIiLCJ0byI6IjB4MTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExMSIsInZhbHVlIjoiMTAwMDAwIiwidmFsaWRBZnRlciI6IjAiLCJ2YWxpZEJlZm9yZSI6IjE5MDAwMDAwMDAiLCJub25jZSI6IjB4MDEifX19";

#[test]
fn t_hdr_v2_and_v1_header_bytes() {
    let payload = proof(PAY_TO, "100000", "0", "1900000000");
    let v2 = World::with(
        TempConfigStore::new("v2", &config()),
        FakePaymentsApi::with_proof(payload.clone()),
    );
    v2.merchant.respond(
        402,
        &[],
        format!(
            r#"{{"x402Version":2,"accepts":[{}],"resource":{{"url":"{URL}","description":"dropped"}}}}"#,
            entry("100000", PAY_TO, r#","maxTimeoutSeconds":60"#)
        )
        .as_bytes(),
    );
    v2.merchant.respond(200, &[], b"ok");
    assert_eq!(v2.fetch().get("paid"), Some(&Json::Bool(true)));
    assert_eq!(
        v2.headers(),
        vec![("PAYMENT-SIGNATURE".to_owned(), V2_HEADER.to_owned())]
    );

    let v1 = World::with(
        TempConfigStore::new("v1", &config()),
        FakePaymentsApi::with_proof(payload),
    );
    v1.merchant.respond(
        402,
        &[],
        format!(
            r#"{{"x402Version":1,"accepts":[{{"scheme":"exact","network":"eip155:84532","asset":"{USDC}","payTo":"{PAY_TO}","maxAmountRequired":"100000"}}]}}"#
        )
        .as_bytes(),
    );
    v1.merchant.respond(200, &[], b"ok");
    assert_eq!(v1.fetch().get("paid"), Some(&Json::Bool(true)));
    assert_eq!(
        v1.headers(),
        vec![("X-PAYMENT".to_owned(), V1_HEADER.to_owned())]
    );
}

// ------------------------------------------------------------- T-SCRUB

#[test]
fn t_scrub_an_echoed_proof_in_the_body_is_redacted() {
    let world = World::with(
        TempConfigStore::new("scrub", &config_with(r#","return_body":true"#, "")),
        FakePaymentsApi::signing(),
    );
    world.merchant.respond(402, &[], &challenge());
    world.merchant.echo_proof_in_body(200);
    let out = world.fetch();
    let header = &world.headers()[0].1;
    let rendered = flow::render(&out);
    assert!(!rendered.contains(header.as_str()), "{rendered}");
    let body = text(&out, "body");
    assert!(body.contains("[REDACTED]"), "{body}");
    assert_eq!(out.get("redacted"), Some(&Json::Bool(true)));
    assert_eq!(out.get("body_returned"), Some(&Json::Bool(true)));
    assert_eq!(out.get("paid"), Some(&Json::Bool(true)));
}

#[test]
fn t_scrub_an_echoed_proof_outside_the_body_replaces_the_document() {
    let world = World::new("scrub-ct");
    world.merchant.respond(402, &[], &challenge());
    world.merchant.echo_proof_in_content_type(200);
    let out = world.fetch();
    assert_eq!(
        flow::render(&out),
        r#"{"paid": false, "refused": true, "reason": "RuntimeError during payment flow."}"#
    );
}

/// The deterministic proof header of the fixture world: one paid fetch,
/// captured from the merchant's replay request. The world's idempotency
/// record makes every later fetch of the same terms send the same bytes.
fn paid_header(world: &World) -> String {
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    let first = world.fetch();
    assert_eq!(first.get("paid"), Some(&Json::Bool(true)), "{first:?}");
    world.headers()[0].1.clone()
}

/// Whether `shown` still holds any piece of `secret` of at least 8 bytes,
/// ignoring ASCII case.
fn holds_fragment(shown: &str, secret: &str) -> Option<String> {
    let shown = shown.to_ascii_lowercase();
    let secret = secret.to_ascii_lowercase();
    (0..=secret.len().saturating_sub(8))
        .filter_map(|at| secret.get(at..at + 8))
        .find(|window| shown.contains(window))
        .map(str::to_owned)
}

/// #5374 (review #5373): a 32-byte echo at an offset that is not a multiple
/// of 8, lower-cased by the content-type rendering, is withheld.
#[test]
fn review_scrub_unaligned_32_byte_echo_is_withheld() {
    let world = World::new("scrub-unaligned");
    let header = paid_header(&world);
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(
        200,
        &[("content-type", &format!("text/{}", &header[1..33]))],
        b"ok",
    );
    assert_refused(&world.fetch(), "RuntimeError during payment flow.");
}

/// #5375 (review #5373): an eight-byte prefix of the proof in an opt-in
/// body is redacted, not returned verbatim.
#[test]
fn review_scrub_eight_byte_prefix_in_body_is_redacted() {
    let world = World::with(
        TempConfigStore::new("scrub-prefix", &config_with(r#","return_body":true"#, "")),
        FakePaymentsApi::signing(),
    );
    let header = paid_header(&world);
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], &header.as_bytes()[..8]);
    let out = world.fetch();
    let rendered = flow::render(&out);
    assert!(
        !rendered.contains(&header[..8]),
        "eight-byte proof prefix escaped the scrub: {rendered}"
    );
    assert_eq!(out.get("redacted"), Some(&Json::Bool(true)), "{rendered}");
    assert_eq!(text(&out, "body"), "[REDACTED]");
}

/// R-SCRUB boundary: every offset of the proof, fragment lengths 8..=40, in
/// the body as sent, upper-cased and lower-cased, is redacted to
/// `[REDACTED]` with the text around it kept; a 7-byte piece is ordinary.
#[test]
fn t_scrub_body_fragments_at_every_offset_and_length_are_redacted() {
    let world = World::with(
        TempConfigStore::new("scrub-every", &config_with(r#","return_body":true"#, "")),
        FakePaymentsApi::signing(),
    );
    let header = paid_header(&world);
    let mut bodies = Vec::new();
    for at in 0..header.len() - 8 {
        for len in [8, 9, 15, 31, 32, 33, 40] {
            if let Some(piece) = header.get(at..at + len) {
                bodies.push(piece.to_owned());
            }
        }
    }
    // One fetch per body, batched so the world is reused.
    for (index, piece) in bodies.iter().enumerate() {
        let spelling = match index % 3 {
            0 => piece.clone(),
            1 => piece.to_ascii_uppercase(),
            _ => piece.to_ascii_lowercase(),
        };
        world.merchant.respond(402, &[], &challenge());
        world
            .merchant
            .respond(200, &[], format!("<p>{spelling}</p>").as_bytes());
        let out = world.fetch();
        let rendered = flow::render(&out);
        assert_eq!(out.get("redacted"), Some(&Json::Bool(true)), "{rendered}");
        assert_eq!(text(&out, "body"), "<p>[REDACTED]</p>", "{piece}");
        assert_eq!(holds_fragment(&rendered, &header), None, "{rendered}");
    }
    // Seven bytes are below the fragment bound: returned unchanged.
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], &header.as_bytes()[3..10]);
    let out = world.fetch();
    assert_eq!(text(&out, "body"), &header[3..10]);
    assert!(out.get("redacted").is_none());
}

/// R-SCRUB boundary outside the body: a fragment of 8..=40 bytes at every
/// offset in the content type (lower-cased and cut to 256 by D17) replaces
/// the whole document with the generic refusal.
#[test]
fn t_scrub_content_type_fragments_at_every_offset_and_length_are_refused() {
    let world = World::new("scrub-ct-every");
    let header = paid_header(&world);
    for at in 0..header.len() - 8 {
        let len = [8, 13, 31, 32, 40][at % 5];
        let Some(piece) = header.get(at..at + len) else {
            continue;
        };
        let spelling = if at % 2 == 0 {
            piece.to_ascii_uppercase()
        } else {
            piece.to_owned()
        };
        world.merchant.respond(402, &[], &challenge());
        world
            .merchant
            .respond(200, &[("content-type", &format!("text/{spelling}"))], b"ok");
        assert_refused(&world.fetch(), "RuntimeError during payment flow.");
    }
    // The whole header cut by the 256-character bound still refuses.
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(
        200,
        &[("content-type", &format!("text/{}{header}", "x".repeat(240)))],
        b"ok",
    );
    assert_refused(&world.fetch(), "RuntimeError during payment flow.");
}

#[test]
fn t_scrub_a_body_without_secrets_is_returned_as_is() {
    let world = World::with(
        TempConfigStore::new("plain", &config_with(r#","return_body":true"#, "")),
        FakePaymentsApi::signing(),
    );
    world.merchant.respond(402, &[], &challenge());
    world
        .merchant
        .respond(200, &[], "caf\u{e9} \u{1F600}".as_bytes());
    let out = world.fetch();
    assert_eq!(text(&out, "body"), "caf\u{e9} \u{1F600}");
    assert_eq!(out.get("truncated"), Some(&Json::Bool(false)));
    assert!(out.get("redacted").is_none());
    assert!(flow::render(&out).contains(r#""body": "caf\u00e9 \ud83d\ude00""#));
}

// ----------------------------------------------------------- T-DNS-DL-3

/// A resolver that stalls (advances the fake clock) on one lookup.
#[derive(Clone)]
struct StallingResolver {
    count: Rc<Cell<usize>>,
    stall_on: usize,
    clock: FakeClock,
}
impl HostResolver for StallingResolver {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        self.count.set(self.count.get() + 1);
        if self.count.get() == self.stall_on {
            self.clock.advance(Duration::from_secs(10_000));
        }
        Ok(public())
    }
}

#[test]
fn t_dns_dl_3_a_replay_lookup_past_the_deadline_sends_no_proof() {
    let world = World::new("dns");
    let resolver = StallingResolver {
        count: Rc::new(Cell::new(0)),
        // 1 the flow's origin check, 2 the probe, 3 the gate's re-vet,
        // 4 the first replay.
        stall_on: 4,
        clock: world.clock.clone(),
    };
    let transport = FakeStreamingTransport::new()
        .respond(402, &[], vec![FakeChunk::data(&challenge())])
        .respond(200, &[], vec![FakeChunk::data(b"never")]);
    let merchant = StreamingMerchant {
        transport,
        resolver: resolver.clone(),
        clock: world.clock.clone(),
    };
    let deps = Deps {
        merchant: &merchant,
        resolver: &resolver,
        ..world.deps()
    };
    let out = flow::fetch(&deps, URL, None, "GET");
    assert_refused(&out, "ReadTimeout during payment flow.");
    assert_eq!(world.api.calls(), 1);
    let requests = merchant.transport.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("payment-signature").is_none());
    assert_eq!(resolver.count.get(), 4);
}

// ------------------------------------------------------------ T-RESTART

#[test]
fn t_restart_1_aborted_before_the_send_reuses_the_stored_proof() {
    let world = World::new("restart1");
    world.merchant.respond(402, &[], &challenge());
    let slow = SlowApi {
        inner: &world.api,
        clock: &world.clock,
    };
    let first = flow::fetch(
        &Deps {
            payments: &slow,
            ..world.deps()
        },
        URL,
        None,
        "GET",
    );
    assert_refused(&first, "ReadTimeout during payment flow.");
    assert_eq!(world.headers().len(), 0);
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    let second = world.fetch();
    assert_eq!(second.get("paid"), Some(&Json::Bool(true)));
    let recorded = world.api.recorded_requests();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].token, recorded[1].token);
    assert_eq!(recorded[0].body, recorded[1].body);
    assert_eq!(world.api.authorizations(), 1);
    assert_eq!(world.headers().len(), 1);
    assert_eq!(world.merchant.settlements(), 1);
}

#[test]
fn t_restart_2_a_lost_merchant_response_replays_the_same_proof_bytes() {
    let world = World::new("restart2");
    world.merchant.respond(402, &[], &challenge());
    world
        .merchant
        .settle_then_fail("ConnectError during payment flow.");
    assert_refused(&world.fetch(), "ConnectError during payment flow.");
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    let recorded = world.api.recorded_requests();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].token, recorded[1].token);
    let headers = world.headers();
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[0], headers[1]);
    assert_eq!(world.api.authorizations(), 1);
    assert_eq!(world.merchant.settlements(), 1);
}

#[test]
fn t_restart_3_a_lost_agentcore_response_returns_the_stored_proof() {
    let world = World::new("restart3");
    world
        .api
        .push_outcome(FakeOutcome::RecordThenFail(PaymentError::Transport(
            PaymentErrorClass::ReadTimeout,
        )));
    world.merchant.respond(402, &[], &challenge());
    assert_refused(&world.fetch(), "ReadTimeout during payment flow.");
    assert_eq!(world.headers().len(), 0);
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    let recorded = world.api.recorded_requests();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].token, recorded[1].token);
    assert_eq!(world.api.authorizations(), 1);
}

fn paid_then(world: &World, second: &[u8]) -> Json {
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    world.merchant.respond(402, &[], second);
    world.merchant.respond(200, &[], b"ok");
    world.fetch()
}

#[test]
fn t_restart_4_and_5_changed_terms_authorize_again_l_terms() {
    for second in [
        challenge_with("200000", PAY_TO, ""),
        challenge_with("100000", OTHER_PAY_TO, ""),
    ] {
        let world = World::new("restart45");
        assert_eq!(
            paid_then(&world, &second).get("paid"),
            Some(&Json::Bool(true))
        );
        let recorded = world.api.recorded_requests();
        assert_eq!(recorded.len(), 2);
        assert_ne!(recorded[0].token, recorded[1].token);
        assert_eq!(world.api.authorizations(), 2);
        assert_eq!(world.merchant.settlements(), 2);
    }
}

#[test]
fn t_restart_6_same_token_different_body_conflicts_without_a_fresh_token() {
    for extra in [r#","extra":{"name":"USDC"}"#, r#","maxTimeoutSeconds":30"#] {
        let world = World::new("restart6");
        world.merchant.respond(402, &[], &challenge());
        world.merchant.respond(200, &[], b"ok");
        assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
        world
            .merchant
            .respond(402, &[], &challenge_with("100000", PAY_TO, extra));
        world.merchant.respond(200, &[], b"never");
        let second = world.fetch();
        assert_refused(&second, "ClientError during payment flow.");
        assert_eq!(flow::exit_code(&second), 2);
        let recorded = world.api.recorded_requests();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].token, recorded[1].token);
        assert_ne!(recorded[0].body, recorded[1].body);
        assert_eq!(world.api.authorizations(), 1);
        assert_eq!(world.headers().len(), 1);
    }
}

#[test]
fn t_restart_7_an_expired_idempotency_record_is_the_services_boundary() {
    let world = World::new("restart7");
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    world.api.expire_idempotency_records();
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    assert_eq!(world.fetch().get("paid"), Some(&Json::Bool(true)));
    let recorded = world.api.recorded_requests();
    assert_eq!(recorded[0].token, recorded[1].token);
    assert_eq!(world.api.authorizations(), 2);
    let headers = world.headers();
    assert_ne!(headers[0], headers[1]);
}

#[test]
fn t_restart_8_a_conflict_on_the_first_call_never_mints_another_token() {
    let world = post_world(
        "restart8",
        FakeOutcome::Fail(PaymentError::Service("ConflictException")),
    );
    assert_refused(&world.fetch(), "ClientError during payment flow.");
    assert_eq!(world.api.calls(), 1);
    assert_eq!(world.api.recorded_requests().len(), 1);
    assert_eq!(world.api.authorizations(), 0);
    assert_eq!(world.headers().len(), 0);
}

// -------------------------------------------------------- BrowserHandle

#[test]
fn browser_handle_holds_the_proof_and_returns_only_a_handle() {
    let world = World::new("browser");
    world.merchant.respond(402, &[], &challenge());
    let vault = Vault::new();
    let out = flow::prepare_browser_payment(&world.deps(), &vault, URL, None);
    assert_eq!(out.get("paid"), Some(&Json::Bool(true)), "{out:?}");
    let handle = text(&out, "handle").to_owned();
    assert!(handle.starts_with("x402h_"));
    let suffix = &handle["x402h_".len()..];
    assert_eq!(suffix.len(), 32);
    assert!(suffix
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    assert_eq!(out.get("expires_in_seconds"), Some(&Json::Int(90)));
    assert_eq!(text(out.get("receipt").unwrap(), "resource"), URL);
    assert_eq!(
        text(&out, "next_step"),
        "Call attach_browser_payment(handle, url) to get the header to set on the browser, then navigate. The handle is single-use and expires."
    );
    let rendered = flow::render(&out);
    assert!(!rendered.contains("0xsig"), "{rendered}");
    assert!(!rendered.contains("authorization"));
    assert_eq!(world.headers().len(), 0);
    assert_eq!(world.api.calls(), 1);

    let now = world.clock.now_instant();
    assert_eq!(
        vault
            .attach(&handle, "https://evil.example.com/v1/x402-test", now)
            .unwrap_err()
            .reason(),
        "Handle does not match this origin."
    );
    assert_eq!(
        vault
            .attach(&handle, &format!("{ORIGIN}/some/other/path"), now)
            .unwrap_err()
            .reason(),
        "Handle does not match this resource path."
    );
    let header = vault.attach(&handle, URL, now).unwrap();
    assert_eq!(header.name(), "PAYMENT-SIGNATURE");
    assert_eq!(
        vault.attach(&handle, URL, now).unwrap_err().reason(),
        "Unknown, already-used, or expired payment handle."
    );
    assert_eq!(
        vault.attach("x402h_nope", URL, now).unwrap_err().reason(),
        "Unknown, already-used, or expired payment handle."
    );
}

#[test]
fn browser_handle_expires_and_handles_are_unique() {
    let world = World::new("browser-ttl");
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(402, &[], &challenge());
    let vault = Vault::new();
    let first = flow::prepare_browser_payment(&world.deps(), &vault, URL, None);
    let second = flow::prepare_browser_payment(&world.deps(), &vault, URL, Some("order-2"));
    let (first, second) = (text(&first, "handle"), text(&second, "handle"));
    assert_ne!(first, second);
    let later = world.clock.now_instant() + Duration::from_secs(90);
    assert!(vault.attach(first, URL, later).is_err());
    assert!(vault.attach(second, URL, later).is_err());
}

#[test]
fn browser_handle_needs_a_402() {
    let world = World::new("browser-200");
    world.merchant.respond(200, &[], b"free");
    let out = flow::prepare_browser_payment(&world.deps(), &Vault::new(), URL, None);
    assert_refused(&out, "URL returned 200, not 402. No payment needed.");
    assert_zero_authorizations(&world);
}

// ------------------------------------------------------- SessionStatus

fn session(created: i64, available: Option<&str>) -> Json {
    let available = available.map_or(String::new(), |value| {
        format!(r#","availableLimits":{{"availableSpendAmount":{{"value":"{value}","currency":"USD"}},"updatedAt":{created}}}"#)
    });
    json(&format!(
        r#"{{"paymentSession":{{"paymentSessionId":"ps-1","paymentManagerArn":"{MANAGER}","userId":"agents-pay-0123456789ab","expiryTimeInMinutes":60,"createdAt":{created},"updatedAt":{created},"limits":{{"maxSpendAmount":{{"value":"1.00","currency":"USD"}}}}{available}}}}}"#
    ))
}

#[test]
fn status_reports_the_session_from_the_model_members() {
    let world = World::with(
        TempConfigStore::new("status", &config()),
        FakePaymentsApi::with_session(session(NOW - 60, Some("0.75"))),
    );
    let out = flow::status(&world.deps());
    assert_eq!(
        flow::render(&out),
        r#"{"usable": true, "status": "ACTIVE", "budget_usd": "1.00", "spent_usd": "0.25"}"#
    );
    assert_eq!(flow::status_exit_code(&out), 0);
    assert_eq!(world.api.session_calls(), 1);

    for (value, status) in [
        (session(NOW - 7200, Some("0.75")), "EXPIRED"),
        (session(NOW - 60, Some("0")), "DRAINED"),
    ] {
        let world = World::with(
            TempConfigStore::new("status", &config()),
            FakePaymentsApi::with_session(value),
        );
        let out = flow::status(&world.deps());
        assert_eq!(out.get("usable"), Some(&Json::Bool(false)));
        assert_eq!(text(&out, "status"), status);
        assert_eq!(
            text(&out, "next_step"),
            "An operator must run: toolkit agents-pay admin new-session (requires a human at a terminal). The agent cannot mint budget."
        );
        assert_eq!(flow::status_exit_code(&out), 2);
    }
}

#[test]
fn status_without_a_session_or_with_a_failure_is_unusable() {
    let world = World::with(
        TempConfigStore::empty("status-none"),
        FakePaymentsApi::default(),
    );
    let out = flow::status(&world.deps());
    assert_eq!(
        flow::render(&out),
        r#"{"usable": false, "reason": "No payment session is configured.", "next_step": "An operator must run: toolkit agents-pay admin new-session (requires a human at a terminal)."}"#
    );
    assert_eq!(world.api.session_calls(), 0);
    assert_eq!(world.api.opens(), 0);

    // Without a config file the environment is the fallback, as in Python.
    let store = TempConfigStore::empty("status-env")
        .with_env("PAYMENT_SESSION_ID", "ps-1")
        .with_env("PAYMENT_MANAGER_ARN", MANAGER)
        .with_env("PAYMENT_USER_ID", "agents-pay-0123456789ab");
    let world = World::with(store, FakePaymentsApi::default());
    world
        .api
        .push_outcome(FakeOutcome::Fail(PaymentError::Service(
            "AccessDeniedException",
        )));
    let out = flow::status(&world.deps());
    assert_eq!(
        flow::render(&out),
        r#"{"usable": false, "reason": "ClientError while reading session status.", "next_step": "An operator must check PAYMENT_SESSION_ID and AWS credentials."}"#
    );
    assert_eq!(world.api.session_calls(), 1);
    assert_eq!(world.api.opened_regions(), vec!["us-east-1".to_owned()]);
}

#[test]
fn the_region_comes_from_config_then_env_then_the_manager_arn() {
    let world = World::with(
        TempConfigStore::new("region", &config_with("", r#","region":"eu-west-1""#))
            .with_env("AWS_REGION", "us-west-2"),
        FakePaymentsApi::signing(),
    );
    world.merchant.respond(402, &[], &challenge());
    world.merchant.respond(200, &[], b"ok");
    world.fetch();
    let env = World::with(
        TempConfigStore::new("region-env", &config()).with_env("AWS_REGION", "us-west-2"),
        FakePaymentsApi::signing(),
    );
    env.merchant.respond(402, &[], &challenge());
    env.merchant.respond(200, &[], b"ok");
    env.fetch();
    let arn = World::new("region-arn");
    arn.merchant.respond(402, &[], &challenge());
    arn.merchant.respond(200, &[], b"ok");
    arn.fetch();
    assert_eq!(world.api.opened_regions(), vec!["eu-west-1".to_owned()]);
    assert_eq!(env.api.opened_regions(), vec!["us-west-2".to_owned()]);
    assert_eq!(arn.api.opened_regions(), vec!["us-east-1".to_owned()]);
}
