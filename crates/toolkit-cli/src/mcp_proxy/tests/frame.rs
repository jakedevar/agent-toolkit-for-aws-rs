//! T-FRAME (S2): every upstream message is parsed and re-serialised, so stdout
//! always carries exactly one valid JSON-RPC message per line.

use std::sync::Arc;

use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport};
use toolkit_core::{parse_json, Json};

use super::harness::*;

fn sse(chunks: Vec<FakeChunk>) -> FakeStreamingTransport {
    FakeStreamingTransport::new().respond(200, &[("content-type", "text/event-stream")], chunks)
}

/// Every stdout line is one valid JSON object or batch.
fn assert_framed(h: &Harness) {
    for line in h.stdout.lines() {
        let value = parse_json(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert!(
            matches!(value, Json::Object(_) | Json::Array(_)),
            "not a message: {line}"
        );
        assert!(
            !line.contains('\u{2028}') && !line.contains('\u{2029}'),
            "{line}"
        );
    }
}

#[test]
fn an_sse_event_with_newline_escapes_and_u2028_is_one_line() {
    let event = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"text\":\"a\\nb\u{2028}c\u{2029}d\"}}";
    let transport = Arc::new(sse(vec![FakeChunk::data(
        format!("data: {event}\n\n").as_bytes(),
    )]));
    let mut h = start(config(), transport);
    h.send(&request(1, "ping", "{}"));
    let raw = h.next_text();
    assert!(raw.contains("\\u2028") && raw.contains("\\u2029"), "{raw}");
    let line = parse_json(&raw).unwrap();
    assert_eq!(
        line.get("result")
            .and_then(|r| r.get("text"))
            .and_then(Json::as_str),
        Some("a\nb\u{2028}c\u{2029}d")
    );
    assert_framed(&h);
    h.finish();
}

#[test]
fn multi_line_data_comments_and_crlf_make_one_message() {
    let transport = Arc::new(sse(vec![FakeChunk::data(
        b": keep-alive\r\nevent: message\r\nid: 4\r\nretry: 10\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata:  \"id\":1,\r\ndata: \"result\":{}}\r\n\r\n",
    )]));
    let mut h = start(config(), transport);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(h.next_text(), r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    assert_framed(&h);
    h.finish();
}

#[test]
fn an_event_split_across_chunks_including_inside_a_character() {
    let stream = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"t\":\"é€😀\"}}\r\n\r\n";
    let chunks = stream
        .as_bytes()
        .chunks(1)
        .map(FakeChunk::data)
        .collect::<Vec<_>>();
    let transport = Arc::new(sse(chunks));
    let mut h = start(config(), transport);
    h.send(&request(1, "ping", "{}"));
    let line = h.next();
    assert_eq!(
        line.get("result")
            .and_then(|r| r.get("t"))
            .and_then(Json::as_str),
        Some("é€😀")
    );
    h.finish();
}

#[test]
fn a_batch_array_from_upstream_is_one_line() {
    let transport = Upstream::new(|seen| {
        if seen.rpc_method() == Some("ping") {
            Reply::sse(&[
                r#"[{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}},{"jsonrpc":"2.0","id":1,"result":{}}]"#,
            ])
        } else {
            Reply::accepted()
        }
    });
    let mut h = start(config(), transport);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(
        h.next_text(),
        r#"[{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}},{"jsonrpc":"2.0","id":1,"result":{}}]"#
    );
    h.no_more_output();
    h.finish();
}

#[test]
fn a_non_json_event_is_dropped_and_the_next_event_is_delivered() {
    let transport = Upstream::new(|seen| {
        Reply::sse(&[
            "this is not json",
            "42",
            r#""a string""#,
            "[]",
            &result(seen.id().unwrap(), "{}"),
        ])
    });
    let mut h = start(config(), transport);
    h.send(&request(1, "ping", "{}"));
    assert_eq!(h.next_text(), r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    h.no_more_output();
    let diag = h.diag_text();
    assert!(diag.contains("upstream-message-malformed"), "{diag}");
    assert!(diag.contains("upstream-message-not-rpc"), "{diag}");
    assert!(
        !diag.contains("not json"),
        "no body text in diagnostics: {diag}"
    );
    h.finish();
}

#[test]
fn a_stream_that_ends_without_a_valid_response_gets_one_error() {
    let transport = Upstream::new(|_| Reply::sse(&["{not json"]));
    let mut h = start(config(), transport);
    h.send(&request(7, "resources/list", "{}"));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(7));
    assert_eq!(
        error_message(&line),
        (-32603, "no valid response from the MCP server".to_string())
    );
    h.no_more_output();
    h.finish();
}

#[test]
fn a_json_body_is_re_serialised_compactly() {
    let transport = Upstream::new(|_| {
        Reply::json("{\n  \"jsonrpc\": \"2.0\",\n  \"id\": 3,\n  \"result\": { \"x\": [1, 2.5, \"\\u0041\"] }\n}\n")
    });
    let mut h = start(config(), transport);
    h.send(&request(3, "ping", "{}"));
    assert_eq!(
        h.next_text(),
        r#"{"jsonrpc":"2.0","id":3,"result":{"x":[1,2.5,"A"]}}"#
    );
    h.finish();
}

#[test]
fn server_messages_beside_the_response_are_forwarded_in_order() {
    let transport = Upstream::new(|seen| {
        Reply::sse(&[
            r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}}"#,
            r#"{"jsonrpc":"2.0","id":"srv-1","method":"sampling/createMessage","params":{}}"#,
            &result(seen.id().unwrap(), r#"{"done":true}"#),
        ])
    });
    let mut h = start(config(), transport);
    h.send(&tool_call(5, "echo"));
    assert_eq!(
        h.next().get("method").and_then(Json::as_str),
        Some("notifications/progress")
    );
    assert_eq!(
        h.next().get("method").and_then(Json::as_str),
        Some("sampling/createMessage")
    );
    assert_eq!(id_of(&h.next()), Json::Int(5));
    h.finish();
}

#[test]
fn client_lines_that_are_not_json_rpc_get_standard_errors_and_the_loop_continues() {
    let transport = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let mut h = start(config(), transport);
    h.send("this is not json");
    let line = h.next();
    assert_eq!(line.get("id"), Some(&Json::Null));
    assert_eq!(error_message(&line), (-32700, "Parse error".to_string()));
    h.send("[1,2]");
    assert_eq!(
        error_message(&h.next()),
        (-32600, "Invalid Request".to_string())
    );
    h.send(r#"{"jsonrpc":"2.0","id":{"x":1},"method":"ping"}"#);
    let line = h.next();
    assert_eq!(line.get("id"), Some(&Json::Null));
    assert_eq!(
        error_message(&line),
        (-32600, "Invalid Request".to_string())
    );
    h.send("");
    h.send("   \r");
    h.send(&request(9, "ping", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(9));
    h.finish();
}

#[test]
fn crlf_client_lines_are_accepted() {
    let transport = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let mut h = start(config(), transport.clone());
    h.send_raw(format!("{}\r\n", request(2, "ping", "{}")).as_bytes());
    assert_eq!(id_of(&h.next()), Json::Int(2));
    // Nothing was injected, so the client's bytes go upstream unchanged.
    assert_eq!(
        transport.seen_rpc("ping")[0].body_text(),
        request(2, "ping", "{}")
    );
    h.finish();
}
