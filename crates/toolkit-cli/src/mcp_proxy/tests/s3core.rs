//! S3 behaviour of the protocol core, in-process over the scripted upstream:
//! T-RO (the read-only filter), T-CRED-1 (one credential resolution per
//! attempt and a forced rebuild after a 403), T-SIG-4 at the seam (`_meta`
//! is injected before signing; the signed body is the sent body), the
//! `notifications/cancelled` sent when the proxy's own deadline fires, the
//! Q3 `User-Agent`, and R-STICKY's session boundary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use toolkit_aws::diag::Diag;
use toolkit_aws::{CredentialRefusal, Credentials, Method};
use toolkit_core::Json;

use super::harness::{
    config, eventually, id_of, ok_result, request, result, start, start_with, tool_call,
    tool_error_text, Gate, Reply, Sink, Step, Upstream,
};
use crate::mcp_proxy::scope::USER_AGENT;
use crate::mcp_proxy::{CredentialSource, Resolved, SignWith, SourceAuth};

fn tool_names(line: &Json) -> Vec<String> {
    let Some(Json::Array(tools)) = line.get("result").and_then(|r| r.get("tools")) else {
        panic!("a tools/list result: {line:?}");
    };
    tools
        .iter()
        .map(|tool| tool.get("name").and_then(Json::as_str).unwrap().to_string())
        .collect()
}

const LISTING: &str = r#"{"tools":[{"name":"read","annotations":{"readOnlyHint":true}},{"name":"write","annotations":{"readOnlyHint":false}},{"name":"bare"},{"name":"stringy","annotations":{"readOnlyHint":"true"}}],"nextCursor":"c"}"#;

fn listing_upstream() -> Arc<Upstream> {
    Upstream::new(|seen| match seen.rpc_method() {
        Some("tools/list") => Reply::json(&result(seen.id().unwrap(), LISTING)),
        _ => Reply::json(&ok_result(seen)),
    })
}

#[test]
fn t_ro_tools_list_keeps_only_read_only_tools_and_other_calls_never_leave() {
    let upstream = listing_upstream();
    let mut cfg = config();
    cfg.read_only = true;
    let mut h = start(cfg, upstream.clone());

    // Before any listing, every tool is refused.
    h.send(&tool_call(1, "read"));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(1));
    assert_eq!(
        tool_error_text(&line),
        "Tool 'read' is not available in read-only mode."
    );

    h.send(&request(2, "tools/list", "{}"));
    let line = h.next();
    assert_eq!(tool_names(&line), vec!["read"]);
    // Other result members are kept.
    assert_eq!(
        line.get("result")
            .unwrap()
            .get("nextCursor")
            .and_then(Json::as_str),
        Some("c")
    );

    h.send(&tool_call(3, "read"));
    assert_eq!(
        h.next().get("result").unwrap().get("ok"),
        Some(&Json::Bool(true))
    );
    for (id, name) in [(4, "write"), (5, "bare"), (6, "stringy"), (7, "unknown")] {
        h.send(&tool_call(id, name));
        let line = h.next();
        assert_eq!(id_of(&line), Json::Int(id));
        assert_eq!(
            tool_error_text(&line),
            format!("Tool '{name}' is not available in read-only mode.")
        );
    }
    // A batch holding a refused call is not sent at all.
    h.send(
        r#"[{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"write"}},{"jsonrpc":"2.0","id":9,"method":"ping"}]"#,
    );
    let first = h.next();
    let second = h.next();
    assert_eq!(id_of(&first), Json::Int(8));
    assert!(tool_error_text(&first).contains("read-only mode"));
    assert_eq!(id_of(&second), Json::Int(9));
    assert!(second.get("error").is_some());

    let calls: Vec<Json> = upstream
        .seen_rpc("tools/call")
        .iter()
        .map(|seen| seen.id().cloned().unwrap())
        .collect();
    assert_eq!(
        calls,
        vec![Json::Int(3)],
        "only the read-only call left the proxy"
    );
    assert!(upstream.seen_rpc("ping").is_empty());
    assert!(h.diag_text().contains("read-only-tool-refused"));
    h.finish();
}

#[test]
fn t_ro_control_without_read_only_nothing_is_filtered() {
    let upstream = listing_upstream();
    let mut h = start(config(), upstream.clone());
    h.send(&request(1, "tools/list", "{}"));
    assert_eq!(
        tool_names(&h.next()),
        vec!["read", "write", "bare", "stringy"]
    );
    h.send(&tool_call(2, "write"));
    assert_eq!(
        h.next().get("result").unwrap().get("ok"),
        Some(&Json::Bool(true))
    );
    assert_eq!(upstream.seen_rpc("tools/call").len(), 1);
    h.finish();
}

// ------------------------------------------------------------ T-CRED-1

#[derive(Clone, Default)]
struct Counting {
    events: Arc<Mutex<Vec<String>>>,
    refuse: bool,
}

impl CredentialSource for Counting {
    fn resolve(&self, skip_auth: bool, force_rebuild: bool) -> Resolved {
        self.events
            .lock()
            .unwrap()
            .push(format!("resolve skip={skip_auth} rebuild={force_rebuild}"));
        if self.refuse {
            return Resolved::Refused(CredentialRefusal::ProviderFailed);
        }
        Resolved::Sign(Credentials::new("AKIDCOUNTING", "SECRETCANARY-c1", None, None).unwrap())
    }

    fn end_session(&self) {
        self.events.lock().unwrap().push("end_session".to_string());
    }
}

type SignedBodies = Arc<Mutex<Vec<Vec<u8>>>>;

fn recording_signer(bodies: &SignedBodies) -> SignWith {
    let bodies = Arc::clone(bodies);
    Box::new(move |credentials, request, scope| {
        bodies.lock().unwrap().push(request.body.clone());
        request.headers.insert(
            "authorization",
            &format!(
                "TEST {} {}/{}",
                credentials.access_key_id().expose(),
                scope.region,
                scope.service
            ),
        )
    })
}

fn scope() -> toolkit_aws::SigningScope {
    toolkit_aws::SigningScope {
        region: "us-east-1".to_string(),
        service: "aws-mcp".to_string(),
    }
}

#[test]
fn t_cred_1_one_resolution_per_attempt_and_a_rebuild_after_a_403() {
    let posts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&posts);
    let upstream = Upstream::new(move |seen| match seen.rpc_method() {
        Some("initialize") => Reply::json(&result(
            seen.id().unwrap(),
            r#"{"protocolVersion":"2025-06-18","capabilities":{}}"#,
        ))
        .header("mcp-session-id", "session-1"),
        Some("tools/call") if counter.fetch_add(1, Ordering::SeqCst) == 0 => Reply::status(403),
        None => Reply::status(200), // the session DELETE at EOF
        _ => Reply::json(&ok_result(seen)),
    });
    let source = Counting::default();
    let bodies = SignedBodies::default();
    let diag = Sink::default();
    let auth = SourceAuth::new(
        Box::new(source.clone()),
        recording_signer(&bodies),
        scope(),
        true,
        Diag::new("mcp-proxy", Box::new(diag.clone())),
    );
    let mut h = start_with(config(), upstream.clone(), Arc::new(auth));
    h.send(&request(1, "initialize", r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}"#));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&tool_call(2, "echo"));
    assert_eq!(
        h.next().get("result").unwrap().get("ok"),
        Some(&Json::Bool(true))
    );
    let events = source.events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            // R-STICKY: the session boundary comes before the initialize is signed.
            "end_session",
            "resolve skip=true rebuild=false",
            // tools/call: the load, the 403, D1's forced rebuild, the re-signed resend.
            "resolve skip=true rebuild=false",
            "resolve skip=true rebuild=true",
            "resolve skip=true rebuild=false",
        ]
    );
    let calls = upstream.seen_rpc("tools/call");
    assert_eq!(calls.len(), 2);
    for seen in &calls {
        assert_eq!(
            seen.header("authorization"),
            Some("TEST AKIDCOUNTING us-east-1/aws-mcp")
        );
    }
    h.finish();
    assert!(!diag.text().contains("SECRETCANARY"));
}

#[test]
fn t_cred_1_a_refused_source_sends_nothing_and_reports_the_kind_only() {
    let upstream = listing_upstream();
    let source = Counting {
        refuse: true,
        ..Counting::default()
    };
    let diag = Sink::default();
    let auth = SourceAuth::new(
        Box::new(source.clone()),
        recording_signer(&SignedBodies::default()),
        scope(),
        true,
        Diag::new("mcp-proxy", Box::new(diag.clone())),
    );
    let mut h = start_with(config(), upstream.clone(), Arc::new(auth));
    h.send(&tool_call(1, "echo"));
    let text = tool_error_text(&h.next());
    assert!(
        text.contains("failed due to expired or invalid AWS credentials"),
        "{text}"
    );
    assert!(upstream.seen().is_empty(), "nothing was sent");
    assert!(diag
        .text()
        .contains("credential-failure kind=provider-failed"));
    h.finish();
}

// ------------------------------------------------------------- T-SIG-4

#[test]
fn t_sig_4_meta_is_injected_before_signing_and_the_signed_body_is_sent() {
    let upstream = listing_upstream();
    let bodies = SignedBodies::default();
    let auth = SourceAuth::new(
        Box::new(Counting::default()),
        recording_signer(&bodies),
        scope(),
        false,
        Diag::new("mcp-proxy", Box::new(Sink::default())),
    );
    let mut cfg = config();
    cfg.metadata = vec![(
        "INSTALL_SOURCE".to_string(),
        "agent-toolkit-core".to_string(),
    )];
    let mut h = start_with(cfg, upstream.clone(), Arc::new(auth));
    h.send(&tool_call(1, "echo"));
    h.next();
    let sent = upstream.seen_rpc("tools/call")[0].body.clone();
    let signed = bodies.lock().unwrap()[0].clone();
    assert_eq!(signed, sent);
    let text = String::from_utf8(sent).unwrap();
    assert!(
        text.contains(r#""_meta":{"INSTALL_SOURCE":"agent-toolkit-core"}"#),
        "{text}"
    );
    h.finish();
}

// ------------------------------------------------- forwarded cancellation

#[test]
fn the_proxys_own_tool_timeout_is_forwarded_as_notifications_cancelled() {
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |seen| match seen.rpc_method() {
        Some("tools/call") => Reply::sse(&[]).step(Step::Gate(held.clone())),
        _ => Reply::accepted(),
    });
    let mut h = start(config(), upstream.clone());
    h.send(&tool_call(7, "slow"));
    super::harness::settle();
    h.clock.advance(Duration::from_secs(300));
    assert!(tool_error_text(&h.next()).contains("timed out after 300 seconds"));
    eventually("the cancel notification", || {
        !upstream.seen_rpc("notifications/cancelled").is_empty()
    });
    let cancel = upstream.seen_rpc("notifications/cancelled")[0].clone();
    assert_eq!(cancel.method, Method::Post);
    let params = cancel.json.as_ref().unwrap().get("params").unwrap();
    assert_eq!(params.get("requestId"), Some(&Json::Int(7)));
    assert_eq!(
        params.get("reason").and_then(Json::as_str),
        Some("request timed out")
    );
    eventually("the diagnostic", || {
        h.diag_text().contains("timeout-cancel-forwarded")
    });
    gate.open();
    h.finish();
}

// ------------------------------------------------------------- Q3 UA

#[test]
fn the_user_agent_never_carries_the_client_name_or_version() {
    let upstream = listing_upstream();
    let mut cfg = config();
    cfg.user_agent = Some(USER_AGENT.to_string());
    let mut h = start(cfg, upstream.clone());
    h.send(&request(
        1,
        "initialize",
        r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"claude-code","version":"9.9.9"}}"#,
    ));
    h.next();
    let seen = upstream.seen_rpc("initialize")[0].clone();
    assert_eq!(seen.header("user-agent"), Some(USER_AGENT));
    h.finish();
}
