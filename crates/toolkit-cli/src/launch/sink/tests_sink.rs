//! The known-secret output sink (#5406): once armed with the stored session,
//! no stdout line or diagnostic carrying one of its secrets is written, the
//! generic failure is written once instead with exit 1, and a missing or
//! unreadable session record changes nothing. Assertion messages never print
//! the inspected text.

use super::*;
use crate::launch::clock::SeededRandom;
use crate::launch::state::StoredSession;
use crate::launch::tests_support::Scratch;

const ACCESS: &str = "FAKE-ACCESS-TOKEN-0001";
const REFRESH: &str = "FAKE-REFRESH-TOKEN-0001";
const CLIENT_SECRET: &str = "FAKE-CLIENT-SECRET-0001";
const SEEDED: [&str; 3] = [ACCESS, REFRESH, CLIENT_SECRET];

fn seeded() -> Scratch {
    let scratch = Scratch::new("sink");
    let session = StoredSession {
        client_id: "client-id".into(),
        client_secret: Secret::new(CLIENT_SECRET),
        client_expires_at: 2_000_000_000,
        authorize_endpoint: "https://oidc.us-east-1.amazonaws.com/authorize".into(),
        token_endpoint: "https://oidc.us-east-1.amazonaws.com/token".into(),
        scopes: vec!["launch:all".into()],
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH),
        token_expires_at: 2_000_000_000,
    };
    state::save_session(scratch.path(), &session, &SeededRandom::new(1)).unwrap();
    scratch
}

/// Arm the sink for `dir`, run `route`, return (stdout, stderr, status).
fn routed(
    dir: Option<&Path>,
    route: impl FnOnce(&mut Output<'_>) -> Result<(), toolkit_core::ToolkitError>,
) -> (String, String, u8) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = {
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        guard_in(dir, &mut output);
        route(&mut output).unwrap();
        output.exit_status()
    };
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        status,
    )
}

fn generic() -> String {
    format!("{{\"error\": \"{GENERIC_FAILURE}\"}}\n")
}

#[test]
fn t_tok_sink_refuses_a_diagnostic_or_line_carrying_a_stored_secret() {
    let scratch = seeded();
    for secret in SEEDED {
        let (stdout, stderr, status) = routed(Some(scratch.path()), |output| {
            super::super::text::fail(output, &format!("Invalid region '{secret}'."));
            Ok(())
        });
        assert!(
            stdout.is_empty() && stderr == generic() && status == 1,
            "diagnostic: expected the generic failure"
        );
        let (stdout, stderr, status) = routed(Some(scratch.path()), |output| {
            output.line(&format!("{{\"baseUrl\": \"https://h.example/{secret}\"}}"))
        });
        assert!(
            stdout.is_empty() && stderr == generic() && status == 1,
            "line: expected the generic failure"
        );
        // Several refused writes: one refusal line.
        let (stdout, stderr, status) = routed(Some(scratch.path()), |output| {
            output.diagnostic(&format!("warning {secret}"));
            output.diagnostic(&format!("again {secret}"));
            output.line(secret)
        });
        assert!(
            stdout.is_empty() && stderr == generic() && status == 1,
            "repeated: expected one generic failure"
        );
    }
}

#[test]
fn t_tok_sink_passes_clean_output_unchanged() {
    let scratch = seeded();
    let (stdout, stderr, status) = routed(Some(scratch.path()), |output| {
        output.diagnostic("Using non-default IdC issuer URL: https://issuer.example");
        output.line("{\n  \"authenticated\": true\n}")
    });
    assert_eq!(
        (stdout.as_str(), stderr.as_str(), status),
        (
            "{\n  \"authenticated\": true\n}\n",
            "Using non-default IdC issuer URL: https://issuer.example\n",
            0
        )
    );
}

#[test]
fn t_tok_sink_missing_or_unreadable_session_is_an_empty_set() {
    let echo = |output: &mut Output<'_>| {
        super::super::text::fail(output, &format!("Invalid region '{ACCESS}'."));
        Ok(())
    };
    let expected = format!("{{\"error\": \"Invalid region '{ACCESS}'.\"}}\n");
    // No state directory, an empty one, and a session record that is not JSON.
    let empty = Scratch::new("sink-empty");
    let broken = Scratch::new("sink-broken");
    std::fs::write(broken.path().join(state::SESSION_FILE_NAME), b"{").unwrap();
    for dir in [None, Some(empty.path()), Some(broken.path())] {
        assert_eq!(routed(dir, echo), (String::new(), expected.clone(), 1));
    }
}
