//! T-ERR (S2): timeouts, retries and the fixed error templates.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use toolkit_aws::testing::FakeSigner;
use toolkit_aws::{AwsError, HttpRequest, SigningScope};
use toolkit_core::Json;

use super::harness::*;
use crate::mcp_proxy::{Auth, SignerAuth, MAX_CONTROL_LINE};

const CREDENTIALS_TEXT: &str =
    "Tool call 'echo' failed due to expired or invalid AWS credentials. \
Please refresh your credentials and retry. The proxy will automatically use the new credentials on \
the next request.";

fn scope() -> SigningScope {
    SigningScope {
        region: "us-east-1".to_string(),
        service: "aws-mcp".to_string(),
    }
}

fn signer_auth() -> (Arc<Mutex<FakeSigner>>, Arc<SignerAuth<FakeSigner>>) {
    let signer = Arc::new(Mutex::new(FakeSigner::new()));
    let auth = Arc::new(SignerAuth::new(Arc::clone(&signer), scope()));
    (signer, auth)
}

fn signed(signer: &Arc<Mutex<FakeSigner>>) -> u32 {
    signer.lock().unwrap().signed()
}

/// Replies with `statuses` in turn for `tools/call`, then 200.
fn sequence(statuses: &'static [u16]) -> (Arc<Upstream>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let upstream = Upstream::new(move |seen| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        match statuses.get(n) {
            Some(status) => Reply::status(*status)
                .header("content-type", "text/plain")
                .data(b"CANARY-UPSTREAM-BODY"),
            None => Reply::json(&ok_result(seen)),
        }
    });
    (upstream, calls)
}

#[test]
fn a_401_or_403_on_tools_call_gives_the_fixed_credentials_text() {
    for status in [401_u16, 403] {
        let statuses: &'static [u16] = Box::leak(vec![status].into_boxed_slice());
        let (upstream, calls) = sequence(statuses);
        let (signer, auth) = signer_auth();
        let mut h = start_with(config(), upstream.clone(), auth);
        h.send(&tool_call(1, "echo"));
        let line = h.next();
        assert_eq!(id_of(&line), Json::Int(1));
        assert_eq!(tool_error_text(&line), CREDENTIALS_TEXT);
        // A static signer cannot refresh: one attempt, one signature.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(signed(&signer), 1);
        assert!(!h.stdout.text().contains("CANARY"));
        assert!(!h.diag_text().contains("CANARY"));
        h.finish();
    }
}

#[test]
fn a_non_tool_401_is_a_json_rpc_error_with_the_credentials_text() {
    let (upstream, _) = sequence(&[403]);
    let mut h = start(config(), upstream);
    h.send(&request(4, "tools/list", "{}"));
    assert_eq!(
        error_message(&h.next()),
        (
            -32603,
            "AWS credentials are expired or invalid. Please refresh your credentials and retry. \
The proxy will automatically use the new credentials on the next request."
                .to_string()
        )
    );
    h.finish();
}

/// The D1 seam: an auth that can refresh gets exactly one re-signed resend.
#[test]
fn an_auth_that_refreshes_gets_one_resend_after_401() {
    struct Refreshing {
        signs: AtomicUsize,
        refreshes: AtomicUsize,
    }
    impl Auth for Refreshing {
        fn sign(&self, request: &mut HttpRequest) -> Result<(), AwsError> {
            let n = self.signs.fetch_add(1, Ordering::SeqCst);
            request.headers.insert("authorization", &format!("SIG-{n}"))
        }
        fn refresh(&self) -> bool {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            true
        }
    }
    // 401 then success: recovered by the one refresh.
    let (upstream, calls) = sequence(&[401]);
    let auth = Arc::new(Refreshing {
        signs: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
    });
    let mut h = start_with(config(), upstream.clone(), auth.clone());
    h.send(&tool_call(1, "echo"));
    assert_eq!(
        h.next().get("result"),
        Some(&Json::object(vec![("ok", Json::Bool(true))]))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let seen = upstream.seen();
    assert_eq!(seen[0].header("authorization"), Some("SIG-0"));
    assert_eq!(seen[1].header("authorization"), Some("SIG-1"));
    h.finish();

    // 401 twice: one refresh only, then the credentials text.
    let (upstream, calls) = sequence(&[401, 403]);
    let auth = Arc::new(Refreshing {
        signs: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
    });
    let mut h = start_with(config(), upstream, auth.clone());
    h.send(&tool_call(1, "echo"));
    assert_eq!(tool_error_text(&h.next()), CREDENTIALS_TEXT);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    h.finish();
}

#[test]
fn a_5xx_on_tools_call_gives_the_fixed_failure_text_without_the_body() {
    let (upstream, _) = sequence(&[503]);
    let mut h = start(config(), upstream);
    h.send(&tool_call(1, "echo"));
    assert_eq!(
        tool_error_text(&h.next()),
        "Tool call 'echo' failed: HTTP 503 Service Unavailable. Please retry."
    );
    assert!(!h.stdout.text().contains("CANARY"));
    assert!(!h.diag_text().contains("CANARY"));
    h.finish();
}

#[test]
fn a_non_tool_request_failure_is_json_rpc_32603() {
    let upstream = Upstream::new(|_| {
        Reply::fail_open(AwsError::Connect {
            message: "CANARY-CONNECT-DETAIL".to_string(),
        })
    });
    let mut h = start(config(), upstream);
    h.send(&request(2, "resources/read", r#"{"uri":"x"}"#));
    let line = h.next();
    assert_eq!(
        error_message(&line),
        (-32603, "the MCP endpoint could not be reached".to_string())
    );
    h.send(&request(3, "ping", "{}"));
    assert_eq!(
        error_message(&h.next()).1,
        "the MCP endpoint could not be reached"
    );
    assert!(!h.stdout.text().contains("CANARY"));
    assert!(!h.diag_text().contains("CANARY"));
    h.finish();
}

#[test]
fn an_http_error_carrying_a_json_rpc_error_is_forwarded() {
    let upstream = Upstream::new(|seen| {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32602,"message":"bad params"}}}}"#,
            seen.id().unwrap().to_compact_string()
        );
        let mut reply = Reply::json(&body);
        reply.status = 400;
        reply
    });
    let mut h = start(config(), upstream);
    h.send(&tool_call(8, "echo"));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(8));
    assert_eq!(error_message(&line), (-32602, "bad params".to_string()));
    h.finish();
}

#[test]
fn the_tool_timeout_fires_at_the_configured_duration() {
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |seen| {
        Reply::sse(&[])
            .step(Step::Gate(held.clone()))
            .data(format!("data: {}\n\n", ok_result(seen)).as_bytes())
    });
    let mut h = start(config(), upstream);
    h.send(&tool_call(1, "slow"));
    settle();
    h.clock.advance(Duration::from_secs(299));
    h.no_more_output();
    h.clock.advance(Duration::from_secs(1));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(1));
    assert_eq!(
        tool_error_text(&line),
        "Tool call 'slow' failed: timed out after 300 seconds. Please retry."
    );
    // The late response is dropped.
    gate.open();
    h.no_more_output();
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn a_non_tool_request_times_out_at_the_request_timeout() {
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |_| Reply::sse(&[]).step(Step::Gate(held.clone())));
    let mut h = start(config(), upstream);
    h.send(&request(1, "resources/list", "{}"));
    settle();
    h.clock.advance(Duration::from_secs(179));
    h.no_more_output();
    h.clock.advance(Duration::from_secs(1));
    assert_eq!(
        error_message(&h.next()),
        (-32603, "request timed out after 180 seconds".to_string())
    );
    gate.open();
    h.finish();
}

#[test]
fn retries_resend_5xx_429_and_connect_failures_with_fresh_signatures() {
    let (upstream, calls) = sequence(&[503, 429]);
    let (signer, auth) = signer_auth();
    let mut cfg = config();
    cfg.retries = 2;
    let mut h = start_with(cfg, upstream.clone(), auth);
    h.send(&tool_call(1, "echo"));
    assert!(h.next().get("result").and_then(|r| r.get("ok")).is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(signed(&signer), 3);
    assert_eq!(h.clock.sleeps().len(), 2);
    h.finish();

    // A connect failure is retried too.
    let n = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&n);
    let upstream = Upstream::new(move |seen| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::fail_open(AwsError::Connect {
                message: "refused".to_string(),
            })
        } else {
            Reply::json(&ok_result(seen))
        }
    });
    let mut cfg = config();
    cfg.retries = 1;
    let mut h = start(cfg, upstream);
    h.send(&request(1, "ping", "{}"));
    assert!(h.next().get("result").is_some());
    assert_eq!(n.load(Ordering::SeqCst), 2);
    h.finish();
}

#[test]
fn retries_never_exceed_n() {
    let (upstream, calls) = sequence(&[503, 503, 503, 503]);
    let (signer, auth) = signer_auth();
    let mut cfg = config();
    cfg.retries = 1;
    let mut h = start_with(cfg, upstream, auth);
    h.send(&tool_call(1, "echo"));
    assert_eq!(
        tool_error_text(&h.next()),
        "Tool call 'echo' failed: HTTP 503 Service Unavailable. Please retry."
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(signed(&signer), 2);
    h.finish();
}

#[test]
fn a_4xx_is_not_retried_and_without_retries_nothing_is() {
    let (upstream, calls) = sequence(&[400]);
    let mut cfg = config();
    cfg.retries = 3;
    let mut h = start(cfg, upstream);
    h.send(&tool_call(1, "echo"));
    assert_eq!(
        tool_error_text(&h.next()),
        "Tool call 'echo' failed: HTTP 400 Bad Request. Please retry."
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    h.finish();

    let (upstream, calls) = sequence(&[500]);
    let mut h = start(config(), upstream);
    h.send(&tool_call(1, "echo"));
    h.next();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(h.clock.sleeps().is_empty());
    h.finish();
}

#[test]
fn a_signing_failure_sends_nothing_and_gives_the_credentials_text() {
    let upstream = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let signer = Arc::new(Mutex::new(FakeSigner::failing(
        AwsError::CredentialsExpired,
    )));
    let auth = Arc::new(SignerAuth::new(signer, scope()));
    let mut h = start_with(config(), upstream.clone(), auth);
    h.send(&tool_call(1, "echo"));
    assert_eq!(tool_error_text(&h.next()), CREDENTIALS_TEXT);
    h.send(&request(2, "tools/list", "{}"));
    assert_eq!(error_message(&h.next()).0, -32603);
    assert!(upstream.seen().is_empty());
    h.finish();
}

#[test]
fn the_fake_signer_signs_every_request_with_its_scope() {
    let upstream = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let (_signer, auth) = signer_auth();
    let mut h = start_with(config(), upstream.clone(), auth);
    h.send(&request(1, "ping", "{}"));
    h.next();
    assert_eq!(
        upstream.seen()[0].header("authorization"),
        Some("FAKE-SIGNED us-east-1/aws-mcp")
    );
    h.finish();
}

#[test]
fn control_lines_stay_within_one_kib_with_the_longest_id_and_tool_name() {
    let upstream = Upstream::new(|_| Reply::status(503));
    let mut h = start(config(), upstream);
    let id = format!("\"{}\"", "i".repeat(254)); // 256 bytes of compact JSON
    let name = "n\\\"".repeat(500);
    h.send(&format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}"}}}}"#
    ));
    let line = h.next_text();
    assert!(line.len() <= MAX_CONTROL_LINE, "{} bytes", line.len());
    let parsed = toolkit_core::parse_json(&line).unwrap();
    assert!(tool_error_text(&parsed).starts_with("Tool call 'n\"n\"n\""));
    // One byte longer an id is refused at intake.
    let id = format!("\"{}\"", "i".repeat(255));
    h.send(&format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"x"}}}}"#
    ));
    let line = h.next();
    assert_eq!(line.get("id"), Some(&Json::Null));
    assert_eq!(
        error_message(&line),
        (-32600, "Invalid Request".to_string())
    );
    h.finish();
}

#[test]
fn a_duplicate_in_flight_id_is_refused() {
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |seen| {
        Reply::sse(&[])
            .step(Step::Gate(held.clone()))
            .data(format!("data: {}\n\n", ok_result(seen)).as_bytes())
    });
    let mut h = start(config(), upstream.clone());
    h.send(&tool_call(1, "echo"));
    eventually("the first request upstream", || upstream.seen().len() == 1);
    h.send(&tool_call(1, "echo"));
    assert_eq!(
        error_message(&h.next()),
        (-32600, "Invalid Request: duplicate request id".to_string())
    );
    gate.open();
    assert_eq!(id_of(&h.next()), Json::Int(1));
    assert_eq!(upstream.seen().len(), 1);
    h.finish();
}

#[test]
fn an_idle_read_timeout_or_a_failure_mid_stream_gives_a_fixed_error() {
    let upstream = Upstream::new(|seen| {
        let step = if seen.id() == Some(&Json::Int(1)) {
            Step::IdleTimeout
        } else {
            Step::Fail(AwsError::Connect {
                message: "CANARY-RESET".to_string(),
            })
        };
        Reply::sse_raw(&[b"data: {\"jsonrpc\":"]).step(step)
    });
    let mut h = start(config(), upstream);
    h.send(&tool_call(1, "echo"));
    assert_eq!(
        tool_error_text(&h.next()),
        "Tool call 'echo' failed: the MCP endpoint did not respond in time. Please retry."
    );
    h.send(&request(2, "ping", "{}"));
    assert_eq!(
        error_message(&h.next()),
        (-32603, "the MCP endpoint could not be reached".to_string())
    );
    assert!(!h.stdout.text().contains("CANARY") && !h.diag_text().contains("CANARY"));
    h.wait_budget_zero();
    h.finish();
}
