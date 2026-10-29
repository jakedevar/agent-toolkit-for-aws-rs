//! T-SIZE, the S2 rows of section 4.5: each bound at the limit (accepted) and
//! at limit + 1 (refused; the loop continues and the next message is served).

use std::sync::Arc;
use std::time::Duration;

use toolkit_core::Json;

use super::harness::*;
use crate::mcp_proxy::{Limits, LimitsSpec};

fn limits(edit: impl FnOnce(&mut LimitsSpec)) -> Limits {
    let mut spec = spec(small_limits());
    edit(&mut spec);
    Limits::new(spec).expect("valid limits")
}

fn with_limits(limits: Limits) -> crate::mcp_proxy::ProxyConfig {
    let mut cfg = config();
    cfg.limits = limits;
    cfg
}

/// A ping request padded with a `params.pad` string to exactly `len` bytes.
fn padded_request(id: i64, len: usize) -> String {
    let base = request(id, "ping", r#"{"pad":""}"#);
    let pad = len.checked_sub(base.len()).expect("len too small");
    request(id, "ping", &format!(r#"{{"pad":"{}"}}"#, "p".repeat(pad)))
}

/// A result message padded to exactly `len` bytes.
fn padded_result(id: &Json, len: usize) -> String {
    let base = result(id, r#"{"pad":""}"#);
    let pad = len.checked_sub(base.len()).expect("len too small");
    result(id, &format!(r#"{{"pad":"{}"}}"#, "p".repeat(pad)))
}

fn echo() -> Arc<Upstream> {
    Upstream::new(|seen| Reply::json(&ok_result(seen)))
}

#[test]
fn stdin_line_at_the_limit_is_served_and_one_byte_more_is_refused_with_its_id() {
    let upstream = echo();
    let mut h = start(with_limits(small_limits()), upstream.clone());
    h.send(&padded_request(1, 1024));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&padded_request(2, 1025));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(2));
    assert_eq!(
        error_message(&line),
        (-32600, "Invalid Request: message too large".to_string())
    );
    h.send(&request(3, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(3));
    assert_eq!(upstream.seen().len(), 2, "the oversize line never went upstream");
    h.finish();
}

#[test]
fn an_oversize_line_without_a_recoverable_id_gets_a_diagnostic_only() {
    let mut h = start(with_limits(small_limits()), echo());
    h.send(&format!(r#"{{"jsonrpc":"2.0","method":"x","params":"{}"}}"#, "q".repeat(2000)));
    h.send(&request(3, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(3));
    h.no_more_output();
    assert!(h.diag_text().contains("stdin-line-too-large id=false"), "{}", h.diag_text());
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn an_upstream_json_body_at_the_limit_is_delivered_and_one_byte_more_is_refused() {
    let upstream = Upstream::new(|seen| {
        let id = seen.id().unwrap();
        let len = if id == &Json::Int(1) { 1024 } else if id == &Json::Int(2) { 1025 } else { 50 };
        Reply::json(&padded_result(id, len))
    });
    let mut h = start(with_limits(small_limits()), upstream);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(h.next_text().len(), 1024);
    h.send(&request(2, "ping", "{}"));
    assert_eq!(error_message(&h.next()), (-32603, "response too large".to_string()));
    h.send(&request(3, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(3));
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn an_sse_event_at_the_limit_is_delivered_and_one_byte_more_is_refused() {
    let upstream = Upstream::new(|seen| {
        let id = seen.id().unwrap();
        let len = if id == &Json::Int(1) { 1024 } else if id == &Json::Int(2) { 1025 } else { 50 };
        let event = padded_result(id, len);
        Reply::sse(&[event.as_str()])
    });
    // A longer SSE line bound, so the event bound is what refuses.
    let mut h = start(
        with_limits(limits(|s| {
            s.max_sse_line = 2048;
            s.budget = 16384;
        })),
        upstream,
    );
    h.send(&tool_call(1, "a"));
    assert_eq!(h.next_text().len(), 1024);
    h.send(&tool_call(2, "a"));
    assert_eq!(error_message(&h.next()), (-32603, "response too large".to_string()));
    h.send(&tool_call(3, "a"));
    assert_eq!(id_of(&h.next()), Json::Int(3));
    h.finish();
}

#[test]
fn an_sse_line_at_the_limit_is_accepted_and_one_byte_more_is_refused() {
    let upstream = Upstream::new(|seen| {
        let id = seen.id().unwrap();
        // A comment line of exactly the bound (or one more) before the event.
        let len = if id == &Json::Int(1) { 1024 } else { 1025 };
        let comment = format!(":{}\n", "c".repeat(len - 1));
        let event = format!("data: {}\n\n", ok_result(seen));
        Reply::sse_raw(&[comment.as_bytes(), event.as_bytes()])
    });
    let mut h = start(with_limits(small_limits()), upstream);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&request(2, "ping", "{}"));
    assert_eq!(error_message(&h.next()), (-32603, "response too large".to_string()));
    h.finish();
}

fn nested(depth: usize) -> String {
    format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
}

#[test]
fn json_nesting_at_the_limit_is_delivered_and_one_deeper_is_malformed() {
    // Depth 16 is allowed: the message object is depth 0, `result` depth 1,
    // so the array nest adds 15 levels at most.
    let upstream = Upstream::new(|seen| {
        let id = seen.id().unwrap();
        let depth = if id == &Json::Int(1) { 15 } else { 16 };
        Reply::json(&result(id, &format!(r#"{{"v":{}}}"#, nested(depth - 1))))
    });
    let mut h = start(with_limits(small_limits()), upstream);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&request(2, "ping", "{}"));
    assert_eq!(
        error_message(&h.next()),
        (-32603, "no valid response from the MCP server".to_string())
    );
    assert!(h.diag_text().contains("upstream-message-malformed"));
    h.finish();
}

/// A result with `n` nodes in total: the message object (1), its three
/// members and their values (6: jsonrpc, id, result) and `result`'s members.
fn with_nodes(id: &Json, nodes: usize) -> String {
    // Base message: 1 (object) + 3 members + 3 values = 7, with an empty
    // `result` object. Each extra `result` member adds 2 (member + value).
    let extra = nodes.checked_sub(7).expect("at least 7");
    assert_eq!(extra % 2, 0, "odd node counts are built with an array");
    let members: Vec<String> = (0..extra / 2).map(|k| format!(r#""k{k}":0"#)).collect();
    result(id, &format!("{{{}}}", members.join(",")))
}

#[test]
fn json_nodes_at_the_cap_are_delivered_and_one_more_is_malformed() {
    let upstream = Upstream::new(|seen| {
        let id = seen.id().unwrap();
        let body = if id == &Json::Int(1) {
            // 31 nodes, then `0` becomes `[0]` (two nodes): exactly 32.
            with_nodes(id, 31).replacen(r#""k0":0"#, r#""k0":[0]"#, 1)
        } else {
            with_nodes(id, 33)
        };
        Reply::json(&body)
    });
    let mut h = start(with_limits(limits(|s| s.max_nodes = 32)), upstream);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&request(2, "ping", "{}"));
    assert_eq!(
        error_message(&h.next()).1,
        "no valid response from the MCP server"
    );
    h.finish();
}

#[test]
fn the_seventeenth_concurrent_request_is_refused_and_a_later_one_is_served() {
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |seen| {
        Reply::sse(&[])
            .step(Step::Gate(held.clone()))
            .data(format!("data: {}\n\n", ok_result(seen)).as_bytes())
    });
    let mut h = start(config(), upstream.clone());
    for id in 1..=16 {
        h.send(&tool_call(id, "slow"));
    }
    eventually("16 requests upstream", || upstream.seen().len() == 16);
    h.send(&tool_call(17, "slow"));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(17));
    assert_eq!(
        error_message(&line),
        (-32603, "too many concurrent requests".to_string())
    );
    assert_eq!(upstream.seen().len(), 16);
    gate.open();
    let mut ids: Vec<i64> = (0..16)
        .map(|_| match id_of(&h.next()) {
            Json::Int(id) => id,
            other => panic!("{other:?}"),
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=16).collect::<Vec<_>>());
    h.send(&tool_call(18, "slow"));
    assert_eq!(id_of(&h.next()), Json::Int(18));
    h.finish();
}

#[test]
fn the_data_queue_holds_at_most_its_line_bound_and_the_stream_is_not_read_ahead() {
    let events: Vec<String> = (0..12)
        .map(|n| format!(r#"{{"jsonrpc":"2.0","method":"notifications/progress","params":{{"n":{n}}}}}"#))
        .collect();
    let upstream = Upstream::new(move |seen| {
        let mut reply = Reply::status(200).header("content-type", "text/event-stream");
        for event in &events {
            reply = reply.data(format!("data: {event}\n\n").as_bytes());
        }
        reply.data(format!("data: {}\n\n", ok_result(seen)).as_bytes())
    });
    let mut h = start(
        with_limits(limits(|s| {
            s.data_queue_lines = 3;
            s.budget = 65536;
        })),
        upstream.clone(),
    );
    h.stdout.pause();
    h.send(&tool_call(1, "chatty"));
    // One line in the paused write, three queued, one parsed and waiting for
    // queue room: at most 5 frames pulled of 13.
    settle();
    let pulled = upstream.pulled();
    assert!(pulled <= 5, "pulled {pulled} frames while the sink was paused");
    h.stdout.resume();
    for n in 0..12 {
        assert_eq!(
            h.next().get("params").and_then(|p| p.get("n")),
            Some(&Json::Int(n))
        );
    }
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.finish();
}

#[test]
fn a_full_control_queue_pauses_the_stdin_reader() {
    let mut h = start(
        with_limits(limits(|s| s.control_queue_lines = 2)),
        echo(),
    );
    h.stdout.pause();
    for _ in 0..10 {
        h.send("not json");
    }
    settle();
    // One error is in the paused write, two are queued, and the reader is
    // blocked queueing the fourth: it has read at most 4 of the 10 lines.
    let reads = h.stdin_reads.load(std::sync::atomic::Ordering::SeqCst);
    assert!(reads <= 4, "the reader read {reads} lines past a full control queue");
    h.stdout.resume();
    for _ in 0..10 {
        assert_eq!(error_message(&h.next()).0, -32700);
    }
    h.finish();
}

#[test]
fn the_tool_timeout_bounds_a_stream_with_steady_bytes_but_no_end() {
    // Progress notifications keep arriving (the idle timeout never fires);
    // the tool timeout still ends the call.
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |_| {
        Reply::sse(&[r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#])
            .step(Step::Gate(held.clone()))
            .data(b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n")
    });
    let mut h = start(config(), upstream);
    h.send(&tool_call(1, "forever"));
    assert!(h.next().get("method").is_some());
    h.clock.advance(Duration::from_secs(300));
    assert_eq!(
        tool_error_text(&h.next()),
        "Tool call 'forever' failed: timed out after 300 seconds. Please retry."
    );
    gate.open();
    h.no_more_output();
    h.wait_budget_zero();
    h.finish();
}
