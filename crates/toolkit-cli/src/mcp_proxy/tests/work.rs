//! T-WORK at message level (S0's parser, reused by S2), `_meta` injection and
//! cancellation forwarding.

use std::sync::Arc;

use toolkit_core::Json;

use super::harness::*;
use crate::mcp_proxy::parse_threads;

fn wide_object(n: usize, duplicate_last: bool) -> String {
    let mut members: Vec<String> = (0..n).map(|k| format!(r#""key{k}":{k}"#)).collect();
    if duplicate_last {
        members.push(r#""key0":0"#.to_string());
    }
    format!("{{{}}}", members.join(","))
}

#[test]
fn a_wide_object_is_delivered_with_its_key_order_and_parsed_off_the_protocol_threads() {
    let upstream =
        Upstream::new(|seen| Reply::json(&result(seen.id().unwrap(), &wide_object(20_000, false))));
    let mut h = start(config(), upstream);
    h.send(&request(1, "resources/read", "{}"));
    let line = h.next();
    let Some(Json::Object(members)) = line.get("result") else {
        panic!("an object result");
    };
    assert_eq!(members.len(), 20_000);
    assert!(members
        .iter()
        .enumerate()
        .all(|(k, (key, value))| key == &format!("key{k}")
            && value == &Json::Int(i64::try_from(k).unwrap())));
    let threads = parse_threads();
    assert!(!threads.is_empty());
    for name in &threads {
        assert!(
            name.starts_with("mcp-proxy-request") || name.starts_with("mcp-proxy-get"),
            "an upstream message was parsed on {name}"
        );
    }
    h.finish();
}

#[test]
fn a_wide_object_with_a_duplicate_last_key_is_dropped_as_malformed() {
    let upstream =
        Upstream::new(|seen| Reply::json(&result(seen.id().unwrap(), &wide_object(20_000, true))));
    let mut h = start(config(), upstream);
    h.send(&request(1, "resources/read", "{}"));
    assert_eq!(
        error_message(&h.next()),
        (-32603, "no valid response from the MCP server".to_string())
    );
    assert!(h.diag_text().contains("upstream-message-malformed"));
    h.wait_budget_zero();
    h.finish();
}

fn meta_config() -> crate::mcp_proxy::ProxyConfig {
    let mut cfg = config();
    cfg.metadata = vec![
        (
            "INSTALL_SOURCE".to_string(),
            "agent-toolkit-aws-core".to_string(),
        ),
        ("AWS_REGION".to_string(), "us-east-1".to_string()),
    ];
    cfg
}

fn echo() -> Arc<Upstream> {
    Upstream::new(|seen| {
        if seen.id().is_some() && seen.rpc_method().is_some() {
            Reply::json(&ok_result(seen))
        } else {
            Reply::accepted()
        }
    })
}

#[test]
fn metadata_is_merged_into_params_meta_and_upstream_keys_win() {
    let upstream = echo();
    let mut h = start(meta_config(), upstream.clone());
    h.send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x","_meta":{"AWS_REGION":"eu-west-1","progressToken":7}}}"#);
    h.next();
    assert_eq!(
        upstream.seen_rpc("tools/call")[0].body_text(),
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x","_meta":{"AWS_REGION":"eu-west-1","progressToken":7,"INSTALL_SOURCE":"agent-toolkit-aws-core"}}}"#
    );
    assert!(
        h.diag_text().contains("meta-keys-kept count=1"),
        "{}",
        h.diag_text()
    );
    h.finish();
}

#[test]
fn metadata_creates_meta_when_absent_and_skips_messages_without_a_params_object() {
    let upstream = echo();
    let mut h = start(meta_config(), upstream.clone());
    h.send(&request(1, "tools/list", "{}"));
    h.next();
    h.send(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
    h.next();
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#);
    h.send(r#"{"jsonrpc":"2.0","id":"srv-1","result":{"x":1}}"#);
    h.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"_meta":"not an object"}}"#);
    h.next();
    eventually("all five upstream", || upstream.seen().len() == 5);
    let bodies: Vec<String> = upstream
        .seen()
        .iter()
        .map(|s| s.body_text().to_string())
        .collect();
    let find = |needle: &str| {
        bodies
            .iter()
            .find(|b| b.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in {bodies:?}"))
            .clone()
    };
    assert_eq!(
        find("tools/list"),
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"INSTALL_SOURCE":"agent-toolkit-aws-core","AWS_REGION":"us-east-1"}}}"#
    );
    assert_eq!(
        find("\"ping\""),
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#
    );
    assert_eq!(
        find("notifications/initialized"),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{"_meta":{"INSTALL_SOURCE":"agent-toolkit-aws-core","AWS_REGION":"us-east-1"}}}"#
    );
    assert_eq!(
        find("srv-1"),
        r#"{"jsonrpc":"2.0","id":"srv-1","result":{"x":1}}"#
    );
    assert_eq!(
        find("not an object"),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"_meta":"not an object"}}"#
    );
    assert!(h.diag_text().contains("meta-not-object"));
    h.finish();
}

#[test]
fn a_client_batch_is_forwarded_once_with_meta_in_each_element() {
    let upstream = Upstream::new(|_| {
        Reply::json(
            r#"[{"jsonrpc":"2.0","id":1,"result":{}},{"jsonrpc":"2.0","id":2,"result":{}}]"#,
        )
    });
    let mut h = start(meta_config(), upstream.clone());
    h.send(r#"[{"jsonrpc":"2.0","id":1,"method":"ping","params":{}},{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}]"#);
    assert_eq!(
        h.next_text(),
        r#"[{"jsonrpc":"2.0","id":1,"result":{}},{"jsonrpc":"2.0","id":2,"result":{}}]"#
    );
    let seen = upstream.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body_text().matches("INSTALL_SOURCE").count(), 2);
    h.finish();
}

#[test]
fn cancellation_is_forwarded_and_the_late_response_dropped_and_the_slot_freed() {
    let gate = Gate::new();
    let held = gate.clone();
    let upstream = Upstream::new(move |seen| match seen.rpc_method() {
        Some("tools/call") => Reply::sse(&[])
            .step(Step::Gate(held.clone()))
            .data(format!("data: {}\n\n", ok_result(seen)).as_bytes()),
        Some("notifications/cancelled") => Reply::accepted(),
        _ => Reply::json(&ok_result(seen)),
    });
    let mut cfg = config();
    let mut spec = spec(cfg.limits);
    spec.max_in_flight = 2;
    cfg.limits = crate::mcp_proxy::Limits::new(spec).unwrap();
    let mut h = start(cfg, upstream.clone());
    h.send(&tool_call(1, "slow"));
    eventually("the call upstream", || upstream.seen().len() == 1);
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"reason":"user"}}"#);
    eventually("the cancel upstream", || {
        upstream.seen_rpc("notifications/cancelled").len() == 1
    });
    gate.open();
    h.no_more_output();
    // The cancelled call's slot is free again: two more fit in a cap of 2.
    h.send(&request(2, "ping", "{}"));
    h.send(&request(3, "ping", "{}"));
    let mut ids = vec![id_of(&h.next()), id_of(&h.next())];
    ids.sort_by_key(Json::to_compact_string);
    assert_eq!(ids, vec![Json::Int(2), Json::Int(3)]);
    h.wait_budget_zero();
    h.finish();
}

#[test]
fn a_cancel_for_an_unknown_id_is_only_forwarded() {
    let upstream = echo();
    let h = start(config(), upstream.clone());
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"nope"}}"#);
    eventually("the cancel upstream", || upstream.seen().len() == 1);
    h.no_more_output();
    h.finish();
}
