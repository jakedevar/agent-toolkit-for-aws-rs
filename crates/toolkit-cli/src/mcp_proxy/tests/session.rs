//! T-SESS (S2): the session lifecycle.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use toolkit_aws::Method;
use toolkit_core::Json;

use super::harness::*;

const INIT_RESULT: &str =
    r#"{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"s","version":"1"}}"#;

fn initialize() -> String {
    request(
        0,
        "initialize",
        r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}"#,
    )
}

const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// An upstream that hands out `session` on `initialize`, answers requests,
/// accepts notifications, and answers `GET` and `DELETE` with `get`/`delete`.
fn server(session: &'static str, get: u16, delete: u16) -> Arc<Upstream> {
    Upstream::new(move |seen| match seen.method {
        Method::Get => Reply::status(get),
        Method::Delete => Reply::status(delete),
        _ => match seen.rpc_method() {
            Some("initialize") => Reply::json(&result(seen.id().unwrap(), INIT_RESULT))
                .header("Mcp-Session-Id", session),
            _ if seen.id().is_some() => Reply::json(&ok_result(seen)),
            _ => Reply::accepted(),
        },
    })
}

#[test]
fn the_session_id_is_captured_from_initialize_and_sent_afterwards() {
    let upstream = server("sess-123", 405, 200);
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(0));
    assert_eq!(
        line.get("result")
            .and_then(|r| r.get("protocolVersion"))
            .and_then(Json::as_str),
        Some("2025-06-18")
    );
    h.send(INITIALIZED);
    h.send(&request(1, "tools/list", "{}"));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    let init = &upstream.seen_rpc("initialize")[0];
    assert_eq!(init.header("mcp-session-id"), None);
    assert_eq!(init.header("mcp-protocol-version"), None);
    assert_eq!(
        init.header("accept"),
        Some("application/json, text/event-stream")
    );
    assert_eq!(init.header("content-type"), Some("application/json"));
    for later in [
        &upstream.seen_rpc("notifications/initialized")[0],
        &upstream.seen_rpc("tools/list")[0],
    ] {
        assert_eq!(later.header("mcp-session-id"), Some("sess-123"));
        assert_eq!(later.header("mcp-protocol-version"), Some("2025-06-18"));
        assert_eq!(later.url, URL);
    }
    h.finish();
}

#[test]
fn an_invalid_session_id_is_refused_and_never_sent() {
    let long = "a".repeat(257);
    for bad in ["", "has space", "tab\there", "ünïcode", long.as_str()] {
        let bad: &'static str = Box::leak(bad.to_string().into_boxed_str());
        let upstream = Upstream::new(move |seen| match seen.rpc_method() {
            Some("initialize") => {
                let reply = Reply::json(&result(seen.id().unwrap(), INIT_RESULT));
                // `Headers` itself refuses control characters, so a tab is
                // the worst a header value can carry.
                reply.header("Mcp-Session-Id", bad)
            }
            _ => Reply::json(&ok_result(seen)),
        });
        let mut h = start(config(), upstream.clone());
        h.send(&initialize());
        let line = h.next();
        assert_eq!(id_of(&line), Json::Int(0), "{bad:?}");
        assert_eq!(
            error_message(&line),
            (
                -32603,
                "the MCP server returned an invalid session".to_string()
            ),
            "{bad:?}"
        );
        h.send(&request(1, "ping", "{}"));
        assert_eq!(id_of(&h.next()), Json::Int(1));
        assert_eq!(upstream.seen_rpc("ping")[0].header("mcp-session-id"), None);
        assert!(h.diag_text().contains("session-id-refused"));
        h.finish();
        assert_eq!(upstream.count(Method::Delete), 0, "{bad:?}");
    }
}

#[test]
fn a_session_id_of_256_visible_characters_is_accepted() {
    let id: &'static str = Box::leak(format!("!~{}", "Z".repeat(254)).into_boxed_str());
    let upstream = server(id, 405, 200);
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    assert!(h.next().get("result").is_some());
    h.send(&request(1, "ping", "{}"));
    h.next();
    assert_eq!(
        upstream.seen_rpc("ping")[0].header("mcp-session-id"),
        Some(id)
    );
    h.finish();
}

#[test]
fn an_invalid_protocol_version_is_refused() {
    let upstream = Upstream::new(|seen| match seen.rpc_method() {
        Some("initialize") => Reply::json(&result(
            seen.id().unwrap(),
            r#"{"protocolVersion":"2025-06-18\r\nX: y","capabilities":{}}"#,
        ))
        .header("Mcp-Session-Id", "s1"),
        _ => Reply::json(&ok_result(seen)),
    });
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    assert_eq!(
        error_message(&h.next()).1,
        "the MCP server returned an invalid session"
    );
    h.send(&request(1, "ping", "{}"));
    h.next();
    let ping = &upstream.seen_rpc("ping")[0];
    assert_eq!(ping.header("mcp-protocol-version"), None);
    assert_eq!(ping.header("mcp-session-id"), None);
    assert!(h.diag_text().contains("protocol-version-refused"));
    h.finish();
}

#[test]
fn stdin_eof_sends_exactly_one_delete_with_the_session_id_and_tolerates_405() {
    let upstream = server("sess-del", 405, 405);
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    h.next();
    h.send(&request(1, "ping", "{}"));
    h.next();
    let diag_before = h.diag_text();
    let exit = h.finish();
    assert_eq!(exit, crate::mcp_proxy::Exit::StdinClosed);
    let deletes: Vec<_> = upstream
        .seen()
        .into_iter()
        .filter(|s| s.method == Method::Delete)
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0].header("mcp-session-id"), Some("sess-del"));
    assert_eq!(deletes[0].url, URL);
    assert!(deletes[0].body.is_empty());
    let _ = diag_before;
}

#[test]
fn without_a_session_eof_sends_no_delete() {
    let upstream = Upstream::new(|seen| match seen.rpc_method() {
        Some("initialize") => Reply::json(&result(seen.id().unwrap(), INIT_RESULT)),
        _ => Reply::json(&ok_result(seen)),
    });
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    h.next();
    h.finish();
    assert_eq!(upstream.count(Method::Delete), 0);
}

#[test]
fn the_get_stream_opens_after_initialized_and_405_is_tolerated_silently() {
    let upstream = server("sess-g", 405, 200);
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    h.next();
    h.send(INITIALIZED);
    eventually("the GET stream", || upstream.count(Method::Get) == 1);
    let get = upstream
        .seen()
        .into_iter()
        .find(|s| s.method == Method::Get)
        .unwrap();
    assert_eq!(get.header("accept"), Some("text/event-stream"));
    assert_eq!(get.header("mcp-session-id"), Some("sess-g"));
    assert_eq!(get.header("mcp-protocol-version"), Some("2025-06-18"));
    // 405 means the server offers no GET stream: never reopened, no diagnostic.
    h.clock.advance(Duration::from_secs(3600));
    settle();
    assert_eq!(upstream.count(Method::Get), 1);
    assert!(!h.diag_text().contains("405"), "{}", h.diag_text());
    h.no_more_output();
    h.finish();
}

#[test]
fn server_messages_on_the_get_stream_reach_the_client_and_it_reopens_at_most_every_30s() {
    let opens = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&opens);
    let upstream = Upstream::new(move |seen| match seen.method {
        Method::Get => {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let event = format!(
                r#"{{"jsonrpc":"2.0","method":"notifications/message","params":{{"n":{n}}}}}"#
            );
            Reply::sse(&[event.as_str()])
        }
        Method::Delete => Reply::status(200),
        _ => match seen.rpc_method() {
            Some("initialize") => Reply::json(&result(seen.id().unwrap(), INIT_RESULT))
                .header("Mcp-Session-Id", "sess-r"),
            _ => Reply::accepted(),
        },
    });
    let mut h = start(config(), upstream.clone());
    h.send(&initialize());
    h.next();
    h.send(INITIALIZED);
    let first = h.next();
    assert_eq!(
        first.get("params").and_then(|p| p.get("n")),
        Some(&Json::Int(0))
    );
    // The stream ended; the next open waits for 30 s of (fake) time.
    settle();
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    h.clock.advance(Duration::from_secs(29));
    settle();
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    h.clock.advance(Duration::from_secs(1));
    let second = h.next();
    assert_eq!(
        second.get("params").and_then(|p| p.get("n")),
        Some(&Json::Int(1))
    );
    h.finish();
    assert_eq!(upstream.count(Method::Delete), 1);
}

#[test]
fn a_404_on_a_later_post_is_a_session_expired_error() {
    let upstream = Upstream::new(|seen| match seen.rpc_method() {
        Some("initialize") => {
            Reply::json(&result(seen.id().unwrap(), INIT_RESULT)).header("Mcp-Session-Id", "sess-x")
        }
        Some("tools/call") => Reply::status(404),
        _ => Reply::status(404),
    });
    let mut h = start(config(), upstream);
    h.send(&initialize());
    h.next();
    h.send(&tool_call(1, "echo"));
    let line = h.next();
    assert_eq!(id_of(&line), Json::Int(1));
    assert_eq!(
        error_message(&line),
        (
            -32603,
            "MCP session expired; reconnect the MCP server".to_string()
        )
    );
    h.send(&request(2, "tools/list", "{}"));
    assert_eq!(
        error_message(&h.next()).1,
        "MCP session expired; reconnect the MCP server"
    );
    h.finish();
}

#[test]
fn a_new_initialize_replaces_the_session_and_tells_the_auth_seam() {
    use std::sync::Mutex;

    use toolkit_aws::{AwsError, HttpRequest};

    struct Counting(Mutex<usize>);
    impl crate::mcp_proxy::Auth for Counting {
        fn sign(&self, _: &mut HttpRequest) -> Result<(), AwsError> {
            Ok(())
        }
        fn session_started(&self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    let n = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&n);
    let upstream = Upstream::new(move |seen| match seen.rpc_method() {
        None => Reply::status(200), // the DELETE at EOF
        Some("initialize") => {
            let k = counter.fetch_add(1, Ordering::SeqCst);
            let session: &'static str = if k == 0 { "first" } else { "second" };
            Reply::json(&result(seen.id().unwrap(), INIT_RESULT)).header("Mcp-Session-Id", session)
        }
        _ => Reply::json(&ok_result(seen)),
    });
    let auth = Arc::new(Counting(Mutex::new(0)));
    let mut h = start_with(config(), upstream.clone(), auth.clone());
    h.send(&initialize());
    h.next();
    h.send(&initialize().replace("\"id\":0", "\"id\":10"));
    h.next();
    h.send(&request(1, "ping", "{}"));
    h.next();
    assert_eq!(
        upstream.seen_rpc("ping")[0].header("mcp-session-id"),
        Some("second")
    );
    assert_eq!(*auth.0.lock().unwrap(), 2);
    h.finish();
}
