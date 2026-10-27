//! Offline tests for the Secrets Manager Agent backend, over a scripted
//! transport. Values are synthetic.

use super::*;
use toolkit_aws::testing::MockTransport;
use toolkit_aws::AwsError;

const BASE: &str = "http://localhost:2773";

fn backend<'a>(token: &str, transport: &'a MockTransport) -> SmaBackend<'a> {
    SmaBackend::new(BASE.to_string(), token, transport)
}

#[test]
fn availability_is_asked_once_and_needs_a_2xx_ping() {
    let up = MockTransport::new().respond(200, "healthy");
    let sma = backend("", &up);
    assert!(sma.is_available());
    assert!(sma.is_available());
    let requests = up.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::Get);
    assert_eq!(requests[0].url, "http://localhost:2773/ping");
    assert_eq!(requests[0].timeouts.read(), Duration::from_secs(1));
    assert!(requests[0].headers.is_empty());

    for outcome in [
        MockTransport::new().respond(404, ""),
        MockTransport::new().respond(500, ""),
        MockTransport::new().respond(302, ""),
        MockTransport::new().fail(AwsError::connect("refused")),
    ] {
        let sma = backend("", &outcome);
        assert!(!sma.is_available());
        assert!(!sma.is_available());
        assert_eq!(outcome.requests().len(), 1);
    }
}

#[test]
fn a_secret_request_encodes_both_parameters_and_carries_the_token() {
    let transport = MockTransport::new().respond(200, r#"{"SecretString":"synthetic-value"}"#);
    let sma = backend("session-token-abc", &transport);
    let value = sma.get("prod/db name", "AWSCURRENT").unwrap();
    assert_eq!(value.expose(), "synthetic-value");
    let requests = transport.requests();
    let request = &requests[0];
    assert_eq!(
        request.url,
        "http://localhost:2773/secretsmanager/get?secretId=prod%2Fdb%20name&versionStage=AWSCURRENT"
    );
    assert_eq!(request.timeouts.read(), Duration::from_secs(5));
    assert_eq!(
        request.headers.get("x-aws-parameters-secrets-token"),
        Some("session-token-abc")
    );
}

#[test]
fn no_token_header_without_a_token() {
    let transport = MockTransport::new().respond(200, r#"{"SecretString":"v"}"#);
    backend("", &transport).get("n", "AWSCURRENT").unwrap();
    assert!(transport.requests()[0].headers.is_empty());
}

#[test]
fn the_version_stage_cannot_inject_query_parameters() {
    let transport = MockTransport::new().respond(200, r#"{"SecretString":"v"}"#);
    backend("", &transport)
        .get("n", "x&secretId=evil#frag")
        .unwrap();
    let requests = transport.requests();
    let url = &requests[0].url;
    assert!(
        url.ends_with("&versionStage=x%26secretId%3Devil%23frag"),
        "{url}"
    );
    assert_eq!(url.matches("secretId=").count(), 1, "{url}");
}

#[test]
fn anything_but_a_non_empty_secret_string_is_not_resolved() {
    for body in [
        (404, r#"{"SecretString":"v"}"#),
        (500, "oops"),
        (200, "not json"),
        (200, r#"{"SecretString":""}"#),
        (200, r#"{"SecretString":5}"#),
        (200, r#"{"SecretString":null}"#),
        (200, r#"{"SecretBinary":"AAAA"}"#),
        (200, r#"["SecretString"]"#),
        (200, r#"{"SecretString":"a","SecretString":"b"}"#),
        (200, ""),
    ] {
        let transport = MockTransport::new().respond(body.0, body.1);
        assert!(
            backend("", &transport).get("n", "AWSCURRENT").is_none(),
            "{body:?}"
        );
    }
    let broken = MockTransport::new().fail(AwsError::connect("reset"));
    assert!(backend("", &broken).get("n", "AWSCURRENT").is_none());
}

#[test]
fn a_token_that_cannot_be_a_header_value_means_not_resolved_here() {
    let transport = MockTransport::new().respond(200, r#"{"SecretString":"v"}"#);
    assert!(backend("bad\ntoken", &transport)
        .get("n", "AWSCURRENT")
        .is_none());
    assert!(transport.requests().is_empty());
}

#[test]
fn debug_never_shows_the_token() {
    let transport = MockTransport::new();
    let text = format!("{:?}", backend("session-token-abc", &transport));
    assert!(!text.contains("session-token-abc"), "{text}");
}
