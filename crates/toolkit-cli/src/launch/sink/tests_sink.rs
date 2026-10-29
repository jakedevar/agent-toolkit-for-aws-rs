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

/// #5426: a synthetic secret whose every rendered form differs: `/`, `+`,
/// `=` and space change under percent and form encoding (and hex case), both
/// quotes and the backslash under each `text::repr` escaping and JSON, the
/// non-ASCII letter between `pyfmt` and `serde_json`, and the control
/// character between `text::repr` (`\x01`) and JSON (`\u0001`).
const RENDERED: &str = "Ab/+= '\"\\\u{e9}\u{1}-0001";

/// `text` written as a stdout line by an output armed with `secrets`:
/// (stdout, stderr, status).
fn screened(secrets: Secrets, text: &str) -> (String, String, u8) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = {
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        output.refuse_known(secrets.0, super::super::text::error_text(GENERIC_FAILURE));
        output.line(&format!("{{\"baseUrl\": \"https://h.example/{text}?x=1\"}}")).unwrap();
        output.exit_status()
    };
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        status,
    )
}

fn registered(escapes: &[Escape], layers: &[Layer]) -> Secrets {
    let mut secrets = Secrets::default();
    secrets.add_forms(RENDERED, escapes, layers);
    secrets
}

/// #5426: every escape inside every JSON layer of a registered secret is
/// refused; and each escape and each layer is needed: with it left out, the
/// form only it covers is written (so removing any one from [`ESCAPES`] or
/// [`LAYERS`] turns the first half of this test red). Assertion messages
/// name the form, never the inspected text.
#[test]
fn t_tok_sink_refuses_every_rendered_form_and_each_form_is_needed() {
    let mut missed = Vec::new();
    for escape in ESCAPES {
        for layer in LAYERS {
            let form = layer.render(&escape.render(RENDERED));
            let mut secrets = Secrets::default();
            secrets.add(RENDERED);
            if screened(secrets, &form) != (String::new(), generic(), 1) {
                missed.push(format!("{escape:?} in {layer:?}"));
            }
        }
    }
    assert!(missed.is_empty(), "forms written: {missed:?}");
    let leaks = |secrets: Secrets, form: &str| {
        let (stdout, stderr, status) = screened(secrets, form);
        !stdout.is_empty() && stderr.is_empty() && status == 0
    };
    for escape in ESCAPES {
        let others: Vec<Escape> = ESCAPES.into_iter().filter(|e| *e != escape).collect();
        assert!(
            leaks(registered(&others, &LAYERS), &escape.render(RENDERED)),
            "{escape:?} is covered without itself"
        );
    }
    for layer in LAYERS {
        let others: Vec<Layer> = LAYERS.into_iter().filter(|l| *l != layer).collect();
        assert!(
            leaks(registered(&ESCAPES, &others), &layer.render(RENDERED)),
            "{layer:?} is covered without itself"
        );
    }
    // Short values stay unregistered in every form.
    let mut short = Secrets::default();
    short.add("a/b\"c");
    assert!(short.0.is_empty(), "a short value was registered");
}

/// #5426: the forms of a value without anything to escape or encode are the
/// value itself, so clean output is unchanged and nothing new is matched.
#[test]
fn t_tok_sink_plain_secret_has_one_form() {
    let mut secrets = Secrets::default();
    secrets.add(ACCESS);
    assert_eq!(secrets.0, vec![ACCESS.to_owned()]);
}
