//! T-CMD goldens of the offline commands with a seeded session, and the
//! Python output writers every launch command shares (section 1.3).
use super::*;
use crate::launch::state::StoredSession;
use crate::launch::text::{error_text, ok_text};
use toolkit_aws::Secret;

fn session(token_expires_at: i64, client_expires_at: i64) -> StoredSession {
    StoredSession {
        client_id: "client".into(),
        client_secret: Secret::new("FAKE-CLIENT-SECRET-0001"),
        client_expires_at,
        authorize_endpoint: "https://oidc.us-east-1.amazonaws.com/authorize".into(),
        token_endpoint: "https://oidc.us-east-1.amazonaws.com/token".into(),
        scopes: vec![],
        access_token: Secret::new("FAKE-ACCESS-TOKEN-0001"),
        refresh_token: Secret::new("FAKE-REFRESH-TOKEN-0001"),
        token_expires_at,
    }
}

#[test]
fn t_cmd_session_status_branches() {
    let now = 1_000_000;
    for (session, golden) in [
        (None, "{\n  \"authenticated\": false\n}"),
        (
            Some(session(now + 3600, now + 86_400)),
            "{\n  \"authenticated\": true,\n  \"tokenExpiresInSecs\": 3600,\n  \"sessionExpiresInSecs\": 86400,\n  \"canRefresh\": true\n}",
        ),
        // Within the 300 s buffer: not authenticated, but refreshable.
        (
            Some(session(now + 300, now + 10)),
            "{\n  \"authenticated\": false,\n  \"tokenExpiresInSecs\": 300,\n  \"sessionExpiresInSecs\": 10,\n  \"canRefresh\": true\n}",
        ),
        // Both expired: counts clamp at zero.
        (
            Some(session(now - 5, now)),
            "{\n  \"authenticated\": false,\n  \"tokenExpiresInSecs\": 0,\n  \"sessionExpiresInSecs\": 0,\n  \"canRefresh\": false\n}",
        ),
    ] {
        let shown = ok_text(&session_value(session.as_ref(), now));
        assert_eq!(shown, golden);
        assert!(!shown.contains("FAKE-"));
    }
}

#[test]
fn t_cmd_python_writers_use_python_separators_and_ascii() {
    assert_eq!(
        error_text("Missing required argument for get-launch"),
        r#"{"error": "Missing required argument for get-launch"}"#
    );
    assert_eq!(error_text("caf\u{e9} \"q\"\n\u{1f600}"), r#"{"error": "caf\u00e9 \"q\"\n\ud83d\ude00"}"#);
    assert_eq!(
        ok_text(&Json::object(vec![
            ("b", Json::Float(12.0)),
            ("a", Json::Array(vec![])),
            ("c", Json::object(vec![])),
            ("d", Json::string("\u{e9}")),
        ])),
        "{\n  \"b\": 12.0,\n  \"a\": [],\n  \"c\": {},\n  \"d\": \"\\u00e9\"\n}"
    );
}
