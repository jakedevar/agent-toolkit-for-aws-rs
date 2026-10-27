use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::{j, recorded};
use crate::ai_models::client::{NoLiveClient, Operation, SageMakerClient};
use crate::ai_models::recorded::RecordedClient;

fn rejected(text: &str) -> String {
    match RecordedClient::from_json(&j(text)) {
        Err(message) => message,
        Ok(_) => panic!("accepted: {text}"),
    }
}

#[test]
fn responses_replay_in_order_per_operation() {
    let mut client = recorded(
        r#"{"calls": {"ListHubContents": [{"response": {"n": 1}}, {"response": {"n": 2}}],
                       "Search": [{"response": {"s": 1}}]}}"#,
    );
    let request = Json::Object(Vec::new());
    assert_eq!(
        client.call(Operation::ListHubContents, &request).unwrap(),
        j(r#"{"n": 1}"#)
    );
    assert_eq!(
        client.call(Operation::Search, &request).unwrap(),
        j(r#"{"s": 1}"#)
    );
    assert_eq!(
        client.call(Operation::ListHubContents, &request).unwrap(),
        j(r#"{"n": 2}"#)
    );
}

#[test]
fn a_recording_that_runs_out_fails_closed() {
    let mut client = recorded(r#"{"calls": {"Search": [{"response": {}}]}}"#);
    let request = Json::Object(Vec::new());
    client.call(Operation::Search, &request).unwrap();
    for operation in [Operation::Search, Operation::DescribeEndpoint] {
        let error = client.call(operation, &request).expect_err("exhausted");
        assert_eq!(
            error.to_string(),
            format!(
                "invalid response: the recorded input has no further response for {}",
                operation.name()
            )
        );
    }
}

#[test]
fn a_recorded_request_must_match_the_request_that_is_made() {
    let mut client = recorded(
        r#"{"calls": {"DescribeEndpoint": [{"request": {"EndpointName": "a"}, "response": {}}]}}"#,
    );
    let error = client
        .call(Operation::DescribeEndpoint, &j(r#"{"EndpointName": "b"}"#))
        .expect_err("mismatch");
    assert_eq!(
        error.to_string(),
        "invalid response: the DescribeEndpoint request differs from the recorded request"
    );
}

#[test]
fn recorded_failures_become_aws_errors() {
    let mut client = recorded(
        r#"{"calls": {"Search": [
            {"error": {"code": "AccessDeniedException", "message": "denied", "status": 403}},
            {"error": {"kind": "no_credentials"}},
            {"error": {"kind": "connect", "message": "refused"}},
            {"error": {}}]}}"#,
    );
    let request = Json::Object(Vec::new());
    let AwsError::Service(service) = client.call(Operation::Search, &request).unwrap_err() else {
        panic!("service error")
    };
    assert_eq!(
        (service.status(), service.code(), service.message()),
        (403, Some("AccessDeniedException"), "denied")
    );
    assert_eq!(
        client.call(Operation::Search, &request).unwrap_err(),
        AwsError::NoCredentials
    );
    assert!(matches!(
        client.call(Operation::Search, &request).unwrap_err(),
        AwsError::Connect { .. }
    ));
    let AwsError::Service(bare) = client.call(Operation::Search, &request).unwrap_err() else {
        panic!("service error")
    };
    assert_eq!((bare.status(), bare.code()), (400, None));
}

#[test]
fn malformed_recordings_are_refused_by_position_without_echoing_values() {
    assert_eq!(rejected("[]"), "the recorded input must be an object");
    assert_eq!(rejected("{}"), "the recorded input needs a `calls` object");
    assert_eq!(
        rejected(r#"{"calls": {}, "extra": 1}"#),
        "the recorded input may only contain `calls`"
    );
    assert!(rejected(r#"{"calls": {"Nope": []}}"#)
        .starts_with("`calls` may only contain `ListHubContents`, "));
    assert_eq!(rejected(r#"{"calls": []}"#), "`calls` must be an object");
    assert_eq!(
        rejected(r#"{"calls": {"Search": {}}}"#),
        "calls.Search must be a list of steps"
    );
    assert_eq!(
        rejected(r#"{"calls": {"Search": [{"response": {}, "error": {}}]}}"#),
        "calls.Search[0] needs exactly one of `response` (an object) or `error`"
    );
    assert_eq!(
        rejected(r#"{"calls": {"Search": [{"response": []}]}}"#),
        "calls.Search[0] needs exactly one of `response` (an object) or `error`"
    );
    assert_eq!(
        rejected(r#"{"calls": {"Search": [{"response": {}, "note": "FAKE-SECRET"}]}}"#),
        "calls.Search[0] may only contain `request`, `response`, `error`"
    );
    assert_eq!(
        rejected(r#"{"calls": {"Search": [{"error": {"kind": "boom"}}]}}"#),
        "calls.Search[0].error.kind must be service, no_credentials or connect"
    );
    assert_eq!(
        rejected(r#"{"calls": {"Search": [{"error": {"status": 99}}]}}"#),
        "calls.Search[0].error.status must be an HTTP status code when present (it is integer)"
    );
    assert_eq!(
        rejected(r#"{"calls": {"Search": [{"error": {"code": 5}}]}}"#),
        "calls.Search[0].error.code must be a string when present (it is integer)"
    );
}

#[test]
fn the_build_client_sends_nothing_and_says_so() {
    let error = NoLiveClient
        .call(Operation::ListHubContents, &Json::Object(Vec::new()))
        .expect_err("no client");
    let text = error.to_string();
    assert!(
        text.contains("no live AWS client") && text.contains("--recorded"),
        "{text}"
    );
}
