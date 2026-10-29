//! T-BUDGET (S2): the R-MEM shared byte budget with injected small limits
//! (8 KiB budget, 1 KiB messages, 32 nodes, a 1,025 B control reserve), a
//! routing fake upstream with concurrent producers and a pausable sink. No
//! large allocation, no network.
//!
//! The design's illustrative 4 KiB budget and 512 B reserve cannot hold one
//! 1 KiB message's own charge (32 × 112 B nodes + 1 KiB strings + 1 KiB
//! line) or one maximal 1 KiB control line, which `Limits::new` refuses; the
//! cases are otherwise exactly section 4.5's.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use toolkit_core::Json;

use super::harness::*;
use crate::mcp_proxy::{Exit, Limits, LimitsSpec, ProxyConfig, NODE_CHARGE};

fn cfg(edit: impl FnOnce(&mut LimitsSpec)) -> ProxyConfig {
    let mut spec = spec(small_limits());
    edit(&mut spec);
    let mut cfg = config();
    cfg.limits = Limits::new(spec).expect("valid limits");
    cfg
}

fn padded_result(id: &Json, len: usize) -> String {
    let base = result(id, r#"{"pad":""}"#);
    let pad = len.checked_sub(base.len()).expect("len too small");
    result(id, &format!(r#"{{"pad":"{}"}}"#, "p".repeat(pad)))
}

/// Response sizes by request id (default 50 bytes).
fn sized(sizes: &'static [(i64, usize)]) -> Arc<Upstream> {
    Upstream::new(move |seen| {
        let Some(id) = seen.id() else {
            return Reply::accepted();
        };
        let len = sizes
            .iter()
            .find(|(k, _)| id == &Json::Int(*k))
            .map_or(50, |(_, len)| *len);
        Reply::json(&padded_result(id, len))
    })
}

#[test]
fn node_charge_is_pinned() {
    assert_eq!(NODE_CHARGE, 112);
    let limits = small_limits();
    assert_eq!(limits.parse_charge(600), 112 * 32 + 600);
    assert_eq!(limits.parse_charge(10), 112 * 6 + 10);
    assert_eq!(limits.peak_charge(600), 112 * 32 + 600 + 601);
}

#[test]
fn boundary_equal_to_the_remaining_budget_is_delivered_one_byte_more_fails_or_waits() {
    let upstream = sized(&[(1, 600), (2, 600), (3, 50), (4, 600), (5, 50)]);
    let mut h = start(cfg(|_| {}), upstream);
    let peak = h.budget.limit();
    let need = small_limits().peak_charge(600);

    // Exactly the remaining budget: delivered.
    let hold = h.budget.try_reserve(peak - need).unwrap();
    h.send(&request(1, "ping", "{}"));
    assert_eq!(h.next_text().len(), 600);
    h.wait_for_in_use(peak - need);

    // One byte more with an empty queue: fails at once.
    let extra = h.budget.try_reserve(1).unwrap();
    h.send(&request(2, "ping", "{}"));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(2));
    assert_eq!(
        error_message(&line),
        (-32603, "proxy memory budget exhausted".to_string())
    );
    drop(extra);
    drop(hold);
    h.wait_budget_zero();

    // One byte more while the queue holds bytes: waits, then succeeds when
    // the writer drains.
    h.stdout.pause();
    h.send(&request(3, "ping", "{}"));
    eventually("request 3 in the paused write", || h.budget.queued() == 51);
    let hold = h
        .budget
        .try_reserve(peak - h.budget.in_use() - (need - 1))
        .unwrap();
    h.send(&request(4, "ping", "{}"));
    settle();
    assert_eq!(h.stdout.lines().len(), 2, "request 4 must wait");
    assert!(h.budget.peak() <= peak);
    h.stdout.resume();
    assert_eq!(id_of(&h.next()), Json::Int(3));
    assert_eq!(h.next_text().len(), 600);
    drop(hold);

    // Recovery: a new request succeeds.
    h.send(&request(5, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(5));
    assert!(h.budget.peak() <= peak);
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn backpressure_stops_producers_at_the_budget_and_control_lines_jump_the_queue() {
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"p":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}}"#;
    let tool_gate = Gate::new();
    let held = tool_gate.clone();
    let start_gate = Gate::new();
    let begin = start_gate.clone();
    let upstream = Upstream::new(move |seen| {
        if seen.rpc_method() == Some("tools/call") {
            return Reply::sse(&[]).step(Step::Gate(held.clone()));
        }
        // Producers start together, once every request has been read.
        let mut reply = Reply::status(200)
            .header("content-type", "text/event-stream")
            .step(Step::Gate(begin.clone()));
        for _ in 0..10 {
            reply = reply.data(format!("data: {notification}\n\n").as_bytes());
        }
        reply.data(format!("data: {}\n\n", ok_result(seen)).as_bytes())
    });
    let mut h = start(cfg(|s| s.tool_timeout = Duration::from_secs(10)), upstream.clone());
    let limit = h.budget.limit();
    h.stdout.pause();
    h.send(&tool_call(99, "stuck"));
    for id in 1..=4 {
        h.send(&request(id, "resources/read", "{}"));
    }
    eventually("all five requests upstream", || upstream.seen().len() == 5);
    start_gate.open();
    settle();
    assert!(h.budget.peak() <= limit, "peak {}", h.budget.peak());
    let pulled = upstream.pulled();
    assert!(pulled < 44, "producers read ahead of the budget: {pulled}");
    // A control line (the stuck tool call's timeout) while data is queued.
    h.clock.advance(Duration::from_secs(10));
    eventually("the timeout on the control queue", || h.control.in_use() > 0);
    h.stdout.resume();
    let mut lines = Vec::new();
    for _ in 0..45 {
        lines.push(h.next());
    }
    let control_at = lines
        .iter()
        .position(|line| line.get("id") == Some(&Json::Int(99)))
        .expect("the timeout line");
    assert!(control_at <= 1, "the control line came out at position {control_at}");
    assert_eq!(
        tool_error_text(&lines[control_at]),
        "Tool call 'stuck' failed: timed out after 10 seconds. Please retry."
    );
    let responses = lines.iter().filter(|l| l.get("result").is_some()).count();
    assert_eq!(responses, 4 + 1);
    assert!(h.budget.peak() <= limit);
    tool_gate.open();
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn cancelling_a_waiting_request_lowers_in_use_by_exactly_its_charge() {
    let upstream = sized(&[(1, 50), (2, 1000)]);
    let mut h = start(cfg(|_| {}), upstream.clone());
    h.stdout.pause();
    h.send(&request(1, "ping", "{}"));
    eventually("request 1 in the paused write", || h.budget.queued() == 51);
    // Request 2's 1,000 raw bytes fit, its parse charge (4,584) does not,
    // and the reader still has room to read the cancellation.
    let remaining = 1000 + 4000;
    let hold = h
        .budget
        .try_reserve(h.budget.limit() - h.budget.in_use() - remaining)
        .unwrap();
    let base = h.budget.in_use();
    h.send(&request(2, "resources/read", "{}"));
    h.wait_for_in_use(base + 1000);
    settle();
    assert_eq!(h.budget.in_use(), base + 1000, "request 2 is waiting");
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"user"}}"#);
    h.wait_for_in_use(base);
    let cancels = upstream.seen_rpc("notifications/cancelled");
    assert_eq!(cancels.len(), 1);
    assert_eq!(
        cancels[0]
            .json
            .as_ref()
            .and_then(|j| j.get("params"))
            .and_then(|p| p.get("requestId")),
        Some(&Json::Int(2))
    );
    h.stdout.resume();
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.no_more_output(); // a cancelled request gets nothing
    drop(hold);
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn in_use_returns_to_zero_after_every_kind_of_end() {
    // Rejections: oversize, malformed, node cap.
    for body in [
        "oversize".to_string(),
        "{not json".to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":1,"result":[{}]}}"#, vec!["0"; 40].join(",")),
    ] {
        let upstream = Upstream::new(move |seen| {
            if body == "oversize" {
                Reply::json(&padded_result(seen.id().unwrap(), 1025))
            } else {
                Reply::json(&body)
            }
        });
        let mut h = start(cfg(|_| {}), upstream);
        h.send(&request(1, "ping", "{}"));
        assert_eq!(error_message(&h.next()).0, -32603);
        h.wait_budget_zero();
        h.finish();
    }

    // Timeout (fake clock) and cancellation of a request mid-stream.
    for cancel in [false, true] {
        let gate = Gate::new();
        let held = gate.clone();
        let upstream = Upstream::new(move |seen| {
            Reply::sse_raw(&[b"data: {\"jsonrpc\":\"2.0\","])
                .step(Step::Gate(held.clone()))
                .data(format!("\"id\":1,\"result\":{{}}}}\n\n{}", "").as_bytes())
                .header("x-id", &seen.id().map_or(String::new(), Json::to_compact_string))
        });
        let mut h = start(cfg(|_| {}), upstream.clone());
        h.send(&tool_call(1, "t"));
        eventually("a partial event held", || h.budget.in_use() > 0);
        if cancel {
            h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#);
            eventually("the cancel forwarded", || {
                upstream.seen_rpc("notifications/cancelled").len() == 1
            });
        } else {
            h.clock.advance(Duration::from_secs(300));
            assert!(tool_error_text(&h.next()).contains("timed out"));
        }
        gate.open();
        h.wait_budget_zero();
        h.no_more_output();
        h.finish();
    }

    // stdin EOF with a request in flight.
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |_| {
        Reply::sse_raw(&[b"data: {\"partial\":"]).step(Step::Gate(held.clone()))
    });
    let h = start(cfg(|_| {}), upstream);
    h.send(&tool_call(1, "t"));
    eventually("a partial event held", || h.budget.in_use() > 0);
    let budget = h.budget.clone();
    assert_eq!(h.finish(), Exit::StdinClosed);
    gate.open();
    eventually("zero after EOF", || budget.in_use() == 0);

    // stdout EPIPE.
    let upstream = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let mut h = start(cfg(|_| {}), upstream);
    h.stdout.break_pipe();
    h.send(&request(1, "ping", "{}"));
    let budget = h.budget.clone();
    let control = h.control.clone();
    h.close_stdin();
    assert_eq!(h.join(), Exit::StdoutClosed);
    eventually("zero after EPIPE", || budget.in_use() == 0 && control.in_use() == 0);
}

#[test]
fn producers_with_partial_reservations_and_an_empty_queue_fail_instead_of_hanging() {
    let gates: Arc<Vec<Gate>> = Arc::new((0..4).map(|_| Gate::new()).collect());
    let held = Arc::clone(&gates);
    let upstream = Upstream::new(move |seen| {
        let Some(Json::Int(id)) = seen.id() else {
            return Reply::accepted();
        };
        let event = padded_result(&Json::Int(*id), 1000);
        let (first, rest) = event.split_at(500);
        Reply::sse_raw(&[format!("data: {first}").as_bytes()])
            .step(Step::Gate(held[usize::try_from(*id).unwrap() - 1].clone()))
            .data(format!("{rest}\n\n").as_bytes())
    });
    let mut h = start(cfg(|_| {}), upstream.clone());
    for id in 1..=4 {
        h.send(&request(id, "resources/read", "{}"));
    }
    eventually("four partial events held", || h.budget.in_use() >= 4 * 506);
    for gate in gates.iter() {
        gate.open();
    }
    // Whichever cannot reserve fails at once (the queue is empty); nobody
    // waits, so every request ends without the clock moving.
    let mut ok = 0;
    let mut exhausted = 0;
    for _ in 0..4 {
        let line = h.next();
        if line.get("result").is_some() {
            ok += 1;
        } else {
            assert_eq!(error_message(&line).1, "proxy memory budget exhausted");
            exhausted += 1;
        }
    }
    assert_eq!(ok + exhausted, 4);
    assert!(h.budget.peak() <= h.budget.limit());
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn waiters_behind_a_paused_sink_fail_at_the_idle_read_bound() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let upstream = Upstream::new(move |seen| {
        counter.fetch_add(1, Ordering::SeqCst);
        Reply::json(&padded_result(seen.id().unwrap(), 1000))
    });
    let mut h = start(cfg(|_| {}), upstream);
    h.stdout.pause();
    h.send(&request(1, "resources/read", "{}"));
    eventually("one line in the paused write", || h.budget.queued() > 0);
    for id in 2..=4 {
        h.send(&request(id, "resources/read", "{}"));
    }
    eventually("three producers waiting", || calls.load(Ordering::SeqCst) == 4);
    settle();
    let waiting = h.stdout.lines().len();
    assert_eq!(waiting, 0);
    // The idle-read timeout (60 s by default) caps the wait.
    h.clock.advance(Duration::from_secs(61));
    eventually("the waiters to fail onto the control queue", || {
        h.control.in_use() > 0
    });
    h.stdout.resume();
    let mut exhausted = 0;
    for _ in 0..4 {
        let line = h.next();
        if line.get("error").is_some() {
            assert_eq!(error_message(&line).1, "proxy memory budget exhausted");
            exhausted += 1;
        }
    }
    assert_eq!(exhausted, 3);
    h.wait_budget_zero();
    h.finish();
}
