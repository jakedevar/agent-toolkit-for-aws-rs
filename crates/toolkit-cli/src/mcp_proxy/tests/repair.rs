//! Repair 1 of the S0-S3 stack after the S4 security review (#5105): one
//! regression per finding, in-process over the scripted upstream.
//!
//! - #5106 R-STICKY across sessions: a new `initialize` retires the old
//!   session atomically; nothing is ever sent unsigned into a signed session.
//! - #5107 bounded control work: batches are admitted per request, and the
//!   proxy's own cancellations go through one bounded queue.
//! - #5108 one `GET` stream, owned by a session generation.
//! - #5109 `--read-only` gates every `tools/call` object, with or without an id.
//! - #5110 read-only approvals are per session and per listing snapshot.
//! - #5111 retained approvals are charged to the budget and capped.
//! - #5113 D1 (one rebuild and resend after 401/403) for `GET` and `DELETE`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use toolkit_aws::diag::Diag;
use toolkit_aws::preflight::{AuthDecision, SessionAuth};
use toolkit_aws::{AwsError, CredentialRefusal, Credentials, HttpRequest, Method};
use toolkit_core::Json;

use super::harness::{
    config, eventually, id_of, ok_result, request, result, settle, small_limits, spec, start,
    start_with, tool_call, tool_error_text, Gate, Harness, Reply, Seen, Sink, Step, Upstream,
};
use crate::mcp_proxy::proxy::APPROVED_NAME_OVERHEAD;
use crate::mcp_proxy::{Auth, CredentialSource, Limits, LimitsSpec, Resolved, SourceAuth};

const INIT: &str =
    r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

fn init_ok(seen: &Seen, session: &str) -> Reply {
    Reply::json(&result(
        seen.id().expect("an initialize id"),
        r#"{"protocolVersion":"2025-06-18","capabilities":{}}"#,
    ))
    .header("mcp-session-id", session)
}

fn gated(mut reply: Reply, gate: &Gate) -> Reply {
    reply.head_gate = Some(gate.clone());
    reply
}

fn is_ok(line: &Json) -> bool {
    line.get("result").and_then(|r| r.get("ok")) == Some(&Json::Bool(true))
}

fn refused(line: &Json, name: &str) -> bool {
    line.get("result").and_then(|r| r.get("isError")) == Some(&Json::Bool(true))
        && tool_error_text(line) == format!("Tool '{name}' is not available in read-only mode.")
}

// ------------------------------------------------------------ #5106

/// A source with S1's real R-STICKY latch ([`SessionAuth`]) and switchable
/// credentials: present signs, absent is genuine absence (unsigned under
/// `--skip-auth`).
#[derive(Clone, Default)]
struct Latching {
    present: Arc<AtomicBool>,
    latch: Arc<Mutex<SessionAuth>>,
}

impl CredentialSource for Latching {
    fn resolve(&self, skip_auth: bool, _force_rebuild: bool) -> Resolved {
        let decision = if self.present.load(Ordering::SeqCst) {
            AuthDecision::Sign
        } else if skip_auth {
            AuthDecision::Unsigned
        } else {
            AuthDecision::Refuse(CredentialRefusal::NoCredentials)
        };
        match self.latch.lock().unwrap().apply(decision) {
            AuthDecision::Sign => Resolved::Sign(
                Credentials::new("AKIDLATCH", "SECRETCANARY-latch", None, None).unwrap(),
            ),
            AuthDecision::Unsigned => Resolved::Unsigned,
            AuthDecision::Refuse(refusal) => Resolved::Refused(refusal),
        }
    }

    fn end_session(&self) {
        self.latch.lock().unwrap().reset();
    }
}

fn latching_auth(source: &Latching) -> Arc<dyn Auth> {
    Arc::new(SourceAuth::new(
        Box::new(source.clone()),
        Box::new(|_, request, _| request.headers.insert("authorization", "TEST signed")),
        toolkit_aws::SigningScope {
            region: "us-east-1".to_string(),
            service: "aws-mcp".to_string(),
        },
        true,
        Diag::new("mcp-proxy", Box::new(Sink::default())),
    ))
}

/// Every request that carried `session` was signed.
fn assert_signed_into(upstream: &Upstream, session: &str) {
    for seen in upstream.seen() {
        if seen.header("mcp-session-id") == Some(session) {
            assert!(
                seen.header("authorization").is_some(),
                "an unsigned {:?} {:?} was sent into the signed session {session}",
                seen.method,
                seen.rpc_method()
            );
        }
    }
}

#[test]
fn r_sticky_a_failed_reinitialize_never_sends_the_signed_session_unsigned() {
    let inits = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&inits);
    let upstream = Upstream::new(move |seen| match seen.rpc_method() {
        Some("initialize") if n.fetch_add(1, Ordering::SeqCst) == 0 => init_ok(seen, "session-S"),
        Some("initialize") => Reply::status(400),
        None => Reply::status(200),
        _ => Reply::json(&ok_result(seen)),
    });
    let source = Latching::default();
    source.present.store(true, Ordering::SeqCst);
    let mut h = start_with(config(), upstream.clone(), latching_auth(&source));
    h.send(&request(1, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&tool_call(2, "echo"));
    assert!(is_ok(&h.next()));
    // The credentials disappear: genuine absence, unsigned-capable under
    // --skip-auth, but never inside the session that signed.
    source.present.store(false, Ordering::SeqCst);
    h.send(&request(3, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(3));
    h.send(&tool_call(4, "echo"));
    assert_eq!(id_of(&h.next()), Json::Int(4));

    let reinit = upstream.seen_rpc("initialize")[1].clone();
    assert_eq!(
        reinit.header("mcp-session-id"),
        None,
        "a fresh initialize never carries the previous session id"
    );
    assert_signed_into(&upstream, "session-S");
    h.finish();
    assert_signed_into(&upstream, "session-S");
}

#[test]
fn r_sticky_an_old_session_request_is_not_resent_after_its_session_is_retired() {
    let call_gate = Gate::new();
    let init_gate = Gate::new();
    let inits = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let upstream = {
        let (call_gate, init_gate) = (call_gate.clone(), init_gate.clone());
        let (inits, calls) = (Arc::clone(&inits), Arc::clone(&calls));
        Upstream::new(move |seen| match seen.rpc_method() {
            Some("initialize") if inits.fetch_add(1, Ordering::SeqCst) == 0 => {
                init_ok(seen, "session-S")
            }
            Some("initialize") => gated(Reply::status(400), &init_gate),
            Some("tools/call") if calls.fetch_add(1, Ordering::SeqCst) == 0 => {
                gated(Reply::status(503), &call_gate)
            }
            None => Reply::status(200),
            _ => Reply::json(&ok_result(seen)),
        })
    };
    let source = Latching::default();
    source.present.store(true, Ordering::SeqCst);
    let mut cfg = config();
    cfg.retries = 1;
    let mut h = start_with(cfg, upstream.clone(), latching_auth(&source));
    h.send(&request(1, "initialize", INIT));
    h.next();
    h.send(&tool_call(2, "echo"));
    eventually("the first attempt", || {
        upstream.seen_rpc("tools/call").len() == 1
    });
    source.present.store(false, Ordering::SeqCst);
    h.send(&request(3, "initialize", INIT));
    eventually("the re-initialize", || {
        upstream.seen_rpc("initialize").len() == 2
    });
    // The old session's request now gets a 503: its retry belongs to a
    // retired session and must not be sent at all.
    call_gate.open();
    assert_eq!(id_of(&h.next()), Json::Int(2));
    init_gate.open();
    assert_eq!(id_of(&h.next()), Json::Int(3));
    assert_signed_into(&upstream, "session-S");
    assert_eq!(
        upstream.seen_rpc("tools/call").len(),
        1,
        "the retired session's request was resent"
    );
    h.finish();
}

// ------------------------------------------------------------ #5107

fn two_in_flight() -> Limits {
    Limits::new(LimitsSpec {
        max_in_flight: 2,
        ..spec(small_limits())
    })
    .expect("valid limits")
}

#[test]
fn bounded_control_a_batch_needs_one_slot_per_request() {
    let upstream = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let mut cfg = config();
    cfg.limits = two_in_flight();
    let mut h = start(cfg, upstream.clone());
    h.send(
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"ping"},{"jsonrpc":"2.0","id":3,"method":"ping"}]"#,
    );
    let mut ids: Vec<Json> = (0..3).map(|_| id_of(&h.next())).collect();
    ids.sort_by_key(Json::to_compact_string);
    assert_eq!(ids, vec![Json::Int(1), Json::Int(2), Json::Int(3)]);
    settle();
    assert!(
        upstream.seen().is_empty(),
        "a batch beyond the request cap is refused whole"
    );
    h.finish();
}

#[test]
fn bounded_control_timeout_cancellations_use_one_bounded_queue() {
    let cancel_gate = Gate::new();
    let waves = [Gate::new(), Gate::new(), Gate::new()];
    let upstream = {
        let cancel_gate = cancel_gate.clone();
        let waves = waves.clone();
        Upstream::new(move |seen| match seen.rpc_method() {
            Some("ping") => {
                let Some(Json::Int(id)) = seen.id() else {
                    panic!("ping id");
                };
                let wave = usize::try_from((id - 1) / 2).unwrap().min(2);
                Reply::sse(&[]).step(Step::Gate(waves[wave].clone()))
            }
            Some("notifications/cancelled") => gated(Reply::accepted(), &cancel_gate),
            _ => Reply::accepted(),
        })
    };
    let mut cfg = config();
    cfg.limits = two_in_flight();
    let mut h = start(cfg, upstream.clone());
    let timeout = Duration::from_secs(180);

    // Wave 1: two timeouts; one cancel is open (its head is held), one queued.
    h.send(&request(1, "ping", "{}"));
    h.send(&request(2, "ping", "{}"));
    eventually("wave 1 sent", || upstream.seen_rpc("ping").len() == 2);
    h.clock.advance(timeout);
    h.next();
    h.next();
    eventually("the first cancel", || {
        upstream.seen_rpc("notifications/cancelled").len() == 1
    });
    settle();
    assert_eq!(
        upstream.seen_rpc("notifications/cancelled").len(),
        1,
        "one control worker at a time"
    );

    // Wave 2: the queue (capacity = the request cap) fills and drops.
    waves[0].open();
    settle();
    h.send(&request(3, "ping", "{}"));
    h.send(&request(4, "ping", "{}"));
    eventually("wave 2 sent", || upstream.seen_rpc("ping").len() == 4);
    h.clock.advance(timeout);
    h.next();
    h.next();
    eventually("a dropped cancel", || {
        h.diag_text().contains("timeout-cancel-dropped")
    });

    // The supervisor still answers later deadlines.
    waves[1].open();
    settle();
    h.send(&request(5, "ping", "{}"));
    eventually("ping 5 sent", || upstream.seen_rpc("ping").len() == 5);
    h.clock.advance(timeout);
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(5));

    cancel_gate.open();
    waves[2].open();
    settle();
    let cancels = upstream.seen_rpc("notifications/cancelled");
    assert!(
        cancels.len() <= 3,
        "at most one in progress plus a full queue: {}",
        cancels.len()
    );
    h.wait_budget_zero();
    h.finish();
}

// ------------------------------------------------------------ #5108

fn get_sessions_upstream(gates: HashMap<String, Gate>) -> Arc<Upstream> {
    let inits = Arc::new(AtomicUsize::new(0));
    Upstream::new(move |seen| match (seen.method, seen.rpc_method()) {
        (_, Some("initialize")) => {
            let n = inits.fetch_add(1, Ordering::SeqCst) + 1;
            init_ok(seen, &format!("s-{n}"))
        }
        (Method::Get, _) => {
            let session = seen.header("mcp-session-id").unwrap_or("").to_string();
            let gate = gates.get(&session).cloned().unwrap_or_default();
            Reply::sse(&[]).step(Step::Gate(gate)).data(
                format!(
                    "data: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{{\"from\":\"{session}\"}}}}\n\n"
                )
                .as_bytes(),
            )
        }
        _ => Reply::accepted(),
    })
}

fn gets_for(upstream: &Upstream, session: &str) -> usize {
    upstream
        .seen()
        .iter()
        .filter(|s| s.method == Method::Get && s.header("mcp-session-id") == Some(session))
        .count()
}

fn open_session(h: &mut Harness, id: i64) {
    h.send(&request(id, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(id));
    h.send(INITIALIZED);
}

#[test]
fn get_stream_one_stream_per_session_generation_and_old_messages_are_dropped() {
    let gates: HashMap<String, Gate> = ["s-1", "s-2", "s-3"]
        .iter()
        .map(|s| ((*s).to_string(), Gate::new()))
        .collect();
    let upstream = get_sessions_upstream(gates.clone());
    let mut h = start(config(), upstream.clone());

    open_session(&mut h, 1);
    eventually("GET for s-1", || gets_for(&upstream, "s-1") == 1);
    open_session(&mut h, 2);
    settle();
    assert_eq!(
        upstream.count(Method::Get),
        1,
        "at most one GET stream: the old one is still open"
    );
    // The old stream yields its message: dropped, and it is not reopened.
    gates["s-1"].open();
    eventually("GET for s-2", || gets_for(&upstream, "s-2") == 1);
    open_session(&mut h, 3);
    settle();
    assert_eq!(upstream.count(Method::Get), 2);
    gates["s-2"].open();
    eventually("GET for s-3", || gets_for(&upstream, "s-3") == 1);
    gates["s-3"].open();
    let line = h.next();
    assert_eq!(
        line.get("params")
            .and_then(|p| p.get("from"))
            .and_then(Json::as_str),
        Some("s-3")
    );
    settle();
    let text = h.stdout.text();
    assert!(!text.contains("\"from\":\"s-1\""), "{text}");
    assert!(!text.contains("\"from\":\"s-2\""), "{text}");
    assert_eq!(gets_for(&upstream, "s-1"), 1);
    assert_eq!(gets_for(&upstream, "s-2"), 1);
    h.finish();
}

// ------------------------------------------------------------ #5109

const READ_LISTING: &str =
    r#"{"tools":[{"name":"read","annotations":{"readOnlyHint":true}},{"name":"write"}]}"#;

#[test]
fn read_only_a_tools_call_without_an_id_is_never_forwarded() {
    let upstream = Upstream::new(|seen| match seen.rpc_method() {
        Some("tools/list") => Reply::json(&result(seen.id().unwrap(), READ_LISTING)),
        Some(_) if seen.id().is_some() => Reply::json(&ok_result(seen)),
        _ => Reply::accepted(),
    });
    let mut cfg = config();
    cfg.read_only = true;
    let mut h = start(cfg, upstream.clone());
    let no_id = |name: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#
        )
    };
    // Before any listing, and for unknown or writable names after one.
    h.send(&no_id("write"));
    h.send(&request(1, "tools/list", "{}"));
    h.next();
    for name in ["write", "unknown", "read"] {
        h.send(&no_id(name));
    }
    // In a batch with a permitted request: the whole batch is refused.
    h.send(&format!(
        r#"[{},{{"jsonrpc":"2.0","id":9,"method":"ping"}}]"#,
        no_id("write")
    ));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(9));
    assert!(line.get("error").is_some(), "{line:?}");
    // Genuine notifications still forward.
    h.send(INITIALIZED);
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}"#);
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":77}}"#);
    eventually("the notifications", || {
        upstream.seen_rpc("notifications/cancelled").len() == 1
    });
    settle();
    assert_eq!(upstream.seen_rpc("notifications/initialized").len(), 1);
    assert_eq!(
        upstream.seen_rpc("notifications/roots/list_changed").len(),
        1
    );
    assert!(
        upstream
            .seen()
            .iter()
            .all(|s| !s.body_text().contains("tools/call")),
        "no tools/call object left the proxy"
    );
    assert!(upstream.seen_rpc("ping").is_empty());
    assert!(h.diag_text().contains("read-only-tool-refused"));
    h.no_more_output();
    h.finish();
}

// ------------------------------------------------------------ #5110

fn listing(names: &[&str], cursor: Option<&str>) -> String {
    let tools: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"name":"{n}","annotations":{{"readOnlyHint":true}}}}"#))
        .collect();
    match cursor {
        Some(c) => format!(r#"{{"tools":[{}],"nextCursor":"{c}"}}"#, tools.join(",")),
        None => format!(r#"{{"tools":[{}]}}"#, tools.join(",")),
    }
}

#[test]
fn read_only_approvals_do_not_survive_a_new_session() {
    let late_gate = Gate::new();
    let upstream = {
        let late_gate = late_gate.clone();
        let inits = Arc::new(AtomicUsize::new(0));
        Upstream::new(move |seen| {
            let id = seen.id().cloned();
            match (seen.rpc_method(), seen.header("mcp-session-id")) {
                (Some("initialize"), _) => {
                    let n = inits.fetch_add(1, Ordering::SeqCst) + 1;
                    init_ok(seen, &format!("s-{n}"))
                }
                (Some("tools/list"), _) if id == Some(Json::Int(4)) => gated(
                    Reply::json(&result(&Json::Int(4), &listing(&["late"], None))),
                    &late_gate,
                ),
                (Some("tools/list"), Some("s-1")) => {
                    Reply::json(&result(seen.id().unwrap(), &listing(&["op"], None)))
                }
                (Some("tools/list"), _) => {
                    Reply::json(&result(seen.id().unwrap(), &listing(&["other"], None)))
                }
                (Some(_), _) if id.is_some() => Reply::json(&ok_result(seen)),
                _ => Reply::accepted(),
            }
        })
    };
    let mut cfg = config();
    cfg.read_only = true;
    let mut h = start(cfg, upstream.clone());
    h.send(&request(1, "initialize", INIT));
    h.next();
    h.send(&request(2, "tools/list", "{}"));
    h.next();
    h.send(&tool_call(3, "op"));
    assert!(is_ok(&h.next()));
    // A listing of session 1 still in flight when session 2 starts.
    h.send(&request(4, "tools/list", "{}"));
    eventually("the late listing", || {
        upstream.seen_rpc("tools/list").len() == 2
    });
    h.send(&request(5, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(5));
    late_gate.open();
    assert_eq!(id_of(&h.next()), Json::Int(4));
    for (id, name) in [(6, "op"), (7, "late")] {
        h.send(&tool_call(id, name));
        let line = h.next();
        assert!(refused(&line, name), "{name} in session 2: {line:?}");
    }
    h.send(&request(8, "tools/list", "{}"));
    h.next();
    h.send(&tool_call(9, "op"));
    assert!(refused(&h.next(), "op"));
    h.send(&tool_call(10, "other"));
    assert!(is_ok(&h.next()));
    h.finish();
}

#[test]
fn read_only_a_fresh_listing_replaces_pages_extend_and_late_listings_are_ignored() {
    let late_gate = Gate::new();
    let upstream = {
        let late_gate = late_gate.clone();
        Upstream::new(move |seen| {
            let Some(Json::Int(id)) = seen.id().cloned() else {
                return Reply::accepted();
            };
            let cursor = seen
                .json
                .as_ref()
                .and_then(|j| j.get("params"))
                .and_then(|p| p.get("cursor"))
                .is_some();
            match (seen.rpc_method(), id, cursor) {
                (Some("tools/list"), 1, _) => {
                    Reply::json(&result(&Json::Int(id), &listing(&["a", "b"], None)))
                }
                (Some("tools/list"), 20, _) => gated(
                    Reply::json(&result(&Json::Int(id), &listing(&["stale"], None))),
                    &late_gate,
                ),
                (Some("tools/list"), _, false) => {
                    Reply::json(&result(&Json::Int(id), &listing(&["a"], Some("p2"))))
                }
                (Some("tools/list"), _, true) => {
                    Reply::json(&result(&Json::Int(id), &listing(&["c"], None)))
                }
                _ => Reply::json(&ok_result(seen)),
            }
        })
    };
    let mut cfg = config();
    cfg.read_only = true;
    let mut h = start(cfg, upstream.clone());
    h.send(&request(1, "tools/list", "{}"));
    h.next();
    h.send(&tool_call(2, "b"));
    assert!(is_ok(&h.next()));
    // A complete fresh listing without b revokes it.
    h.send(&request(3, "tools/list", "{}"));
    h.next();
    h.send(&tool_call(4, "b"));
    assert!(refused(&h.next(), "b"));
    // A continuation page extends the current snapshot.
    h.send(&request(5, "tools/list", r#"{"cursor":"p2"}"#));
    h.next();
    for (id, name) in [(6, "a"), (7, "c")] {
        h.send(&tool_call(id, name));
        assert!(is_ok(&h.next()), "{name}");
    }
    // A listing sent before the latest fresh one cannot restore approvals.
    h.send(&request(20, "tools/list", "{}"));
    eventually("the slow listing", || {
        upstream.seen_rpc("tools/list").len() == 4
    });
    h.send(&request(21, "tools/list", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(21));
    late_gate.open();
    assert_eq!(id_of(&h.next()), Json::Int(20));
    h.send(&tool_call(22, "stale"));
    assert!(refused(&h.next(), "stale"));
    h.send(&tool_call(23, "a"));
    assert!(is_ok(&h.next()));
    h.finish();
}

// ------------------------------------------------------------ #5111

fn charge(names: &[&str]) -> usize {
    names.iter().map(|n| n.len() + APPROVED_NAME_OVERHEAD).sum()
}

#[test]
fn read_only_retained_approvals_are_charged_capped_and_released() {
    let page = |prefix: char| -> Vec<String> {
        (0..5)
            .map(|i| format!("{prefix}{i}{}", "x".repeat(18)))
            .collect()
    };
    let pages = [page('p'), page('q'), page('r')];
    let upstream = {
        let pages = pages.clone();
        let inits = Arc::new(AtomicUsize::new(0));
        Upstream::new(move |seen| {
            let Some(id) = seen.id().cloned() else {
                return Reply::accepted();
            };
            match seen.rpc_method() {
                Some("initialize") => {
                    let n = inits.fetch_add(1, Ordering::SeqCst) + 1;
                    init_ok(seen, &format!("s-{n}"))
                }
                Some("tools/list") => {
                    let Json::Int(n) = id else { panic!("id") };
                    let body = match n {
                        1 => r#"{"tools":[{"name":"a","annotations":{"readOnlyHint":true}},{"name":"denied-tool"}]}"#.to_string(),
                        2 => listing(&["cc", "dd"], None),
                        10..=12 => {
                            let names: Vec<&str> =
                                pages[usize::try_from(n - 10).unwrap()].iter().map(String::as_str).collect();
                            listing(&names, Some("next"))
                        }
                        _ => listing(&[], None),
                    };
                    Reply::json(&result(&id, &body))
                }
                _ => Reply::json(&ok_result(seen)),
            }
        })
    };
    let mut cfg = config();
    cfg.read_only = true;
    // One message is at most 1024 bytes (the catalogue cap); listings of
    // five tools need more than the T-BUDGET node bound.
    cfg.limits = Limits::new(LimitsSpec {
        max_nodes: 128,
        budget: 65536,
        ..spec(small_limits())
    })
    .expect("valid limits");
    let mut h = start(cfg, upstream.clone());

    // Only approved names are retained, and they are charged.
    h.send(&request(1, "tools/list", "{}"));
    h.next();
    h.wait_for_in_use(charge(&["a"]));
    // A fresh listing replaces (and re-charges) the snapshot.
    h.send(&request(2, "tools/list", "{}"));
    h.next();
    h.wait_for_in_use(charge(&["cc", "dd"]));
    // A new session releases it.
    h.send(&request(3, "initialize", INIT));
    h.next();
    h.wait_for_in_use(0);

    // Pages accumulate up to the cap (one message, 1024 bytes here); the
    // page that passes it revokes everything (fail closed).
    let names = |i: usize| -> Vec<&str> { pages[i].iter().map(String::as_str).collect() };
    h.send(&request(10, "tools/list", "{}"));
    h.next();
    h.send(&request(11, "tools/list", r#"{"cursor":"next"}"#));
    h.next();
    let two_pages: Vec<&str> = names(0).into_iter().chain(names(1)).collect();
    h.wait_for_in_use(charge(&two_pages));
    assert!(charge(&two_pages) <= 1024 && charge(&two_pages) + charge(&names(2)) > 1024);
    h.send(&tool_call(4, &pages[1][0]));
    assert!(is_ok(&h.next()));
    h.send(&request(12, "tools/list", r#"{"cursor":"next"}"#));
    h.next();
    h.wait_for_in_use(0);
    assert!(h.diag_text().contains("read-only-catalogue-refused"));
    h.send(&tool_call(5, &pages[0][0]));
    assert!(refused(&h.next(), &pages[0][0]));
    h.finish();
}

// ------------------------------------------------------------ #5113

struct Refreshing {
    signs: AtomicUsize,
    refreshes: AtomicUsize,
}

impl Refreshing {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            signs: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
        })
    }
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

/// `GET` and `DELETE` get these statuses in order, then success.
fn refusing_upstream(method: Method, statuses: &'static [u16]) -> Arc<Upstream> {
    let count = Arc::new(AtomicUsize::new(0));
    Upstream::new(move |seen| {
        if seen.rpc_method() == Some("initialize") {
            return init_ok(seen, "s-1");
        }
        if seen.method != method {
            return Reply::accepted();
        }
        let n = count.fetch_add(1, Ordering::SeqCst);
        match statuses.get(n) {
            Some(status) => Reply::status(*status),
            None if method == Method::Get => Reply::sse(&[
                r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"ok":true}}"#,
            ]),
            None => Reply::status(200),
        }
    })
}

#[test]
fn d1_a_refused_get_is_rebuilt_and_reopened_once() {
    for status in [401, 403] {
        let upstream = refusing_upstream(Method::Get, if status == 401 { &[401] } else { &[403] });
        let auth = Refreshing::new();
        let mut h = start_with(config(), upstream.clone(), auth.clone());
        open_session(&mut h, 1);
        let line = h.next();
        assert_eq!(
            line.get("method").and_then(Json::as_str),
            Some("notifications/message")
        );
        assert_eq!(upstream.count(Method::Get), 2);
        assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
        let gets: Vec<String> = upstream
            .seen()
            .iter()
            .filter(|s| s.method == Method::Get)
            .map(|s| s.header("authorization").unwrap().to_string())
            .collect();
        assert_ne!(gets[0], gets[1], "the resend is re-signed");
        h.finish();
    }
    // Refused twice: one rebuild only, then no further open until the
    // reopen interval.
    let upstream = refusing_upstream(Method::Get, &[403, 403, 403]);
    let auth = Refreshing::new();
    let mut h = start_with(config(), upstream.clone(), auth.clone());
    open_session(&mut h, 1);
    eventually("the resend", || upstream.count(Method::Get) == 2);
    settle();
    assert_eq!(upstream.count(Method::Get), 2);
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    h.finish();
}

#[test]
fn d1_a_refused_session_delete_is_rebuilt_and_resent_once() {
    for statuses in [&[401][..], &[403][..]] {
        let statuses: &'static [u16] = if statuses == [401] { &[401] } else { &[403] };
        let upstream = refusing_upstream(Method::Delete, statuses);
        let auth = Refreshing::new();
        let mut h = start_with(config(), upstream.clone(), auth.clone());
        h.send(&request(1, "initialize", INIT));
        h.next();
        h.finish();
        assert_eq!(upstream.count(Method::Delete), 2);
        assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    }
    let upstream = refusing_upstream(Method::Delete, &[401, 401, 401]);
    let auth = Refreshing::new();
    let mut h = start_with(config(), upstream.clone(), auth.clone());
    h.send(&request(1, "initialize", INIT));
    h.next();
    h.finish();
    assert_eq!(upstream.count(Method::Delete), 2, "one resend only");
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
}
