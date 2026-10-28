//! Unit tests of the recorded client and the response validation.

use toolkit_core::{parse_json, Json};

use super::recorded::{LambdaClient, RecordedClient};
use super::response;

fn j(text: &str) -> Json {
    parse_json(text).expect("fixture json")
}

fn client(text: &str) -> Result<RecordedClient, String> {
    RecordedClient::from_document(&j(text))
}

fn request(index: i64) -> Json {
    j(&format!(
        r#"{{"FunctionName": "fn", "InvocationType": "RequestResponse",
            "Payload": {{"invocation_index": {index}, "phase_plan": ["measure"], "extra": [1]}}}}"#
    ))
}

#[test]
fn recorded_steps_replay_in_order_and_compare_a_contained_request() {
    let mut recorded = client(
        r#"{"calls": [
          {"service": "lambda", "operation": "Invoke",
           "request": {"Payload": {"invocation_index": 0}},
           "response": {"StatusCode": 200, "Payload": {"n": 1}}},
          {"service": "lambda", "operation": "Invoke",
           "response": {"Payload": {"n": 2}}}]}"#,
    )
    .expect("recording");
    assert_eq!(recorded.unconsumed(), 2);
    assert_eq!(
        recorded
            .invoke(&request(0))
            .expect("first")
            .get("StatusCode"),
        Some(&Json::Int(200))
    );
    assert!(
        recorded.invoke(&request(1)).is_ok(),
        "an absent request matches anything"
    );
    assert_eq!(recorded.unconsumed(), 0);
    let exhausted = recorded.invoke(&request(2)).expect_err("exhausted");
    assert!(
        exhausted.to_string().contains("no recorded response left"),
        "{exhausted}"
    );
}

#[test]
fn a_request_that_differs_fails_even_for_an_error_step() {
    for step in [
        r#"{"service": "lambda", "operation": "Invoke", "request": {"Payload": {"invocation_index": 5}},
            "response": {"Payload": {}}}"#,
        r#"{"service": "lambda", "operation": "Invoke", "request": {"Payload": {"invocation_index": 5}},
            "error": {"kind": "client", "code": "Boom", "message": "no"}}"#,
    ] {
        let mut recorded = client(&format!(r#"{{"calls": [{step}]}}"#)).expect("recording");
        let error = recorded.invoke(&request(0)).expect_err("mismatch");
        assert!(
            error.to_string().contains("differs from the request made"),
            "{error}"
        );
    }
}

#[test]
fn error_steps_replay_their_kind() {
    let mut recorded = client(
        r#"{"calls": [
          {"service": "lambda", "operation": "Invoke", "error": {"code": "ResourceNotFoundException", "message": "gone"}},
          {"service": "lambda", "operation": "Invoke", "error": {"kind": "no_credentials"}},
          {"service": "lambda", "operation": "Invoke", "error": {"kind": "connect", "message": "refused"}}]}"#,
    )
    .expect("recording");
    let first = recorded.invoke(&request(0)).expect_err("client error");
    assert!(
        matches!(first, toolkit_aws::AwsError::Service(_)),
        "{first}"
    );
    assert!(matches!(
        recorded.invoke(&request(0)),
        Err(toolkit_aws::AwsError::NoCredentials)
    ));
    assert!(recorded.invoke(&request(0)).is_err());
}

#[test]
fn malformed_recordings_are_rejected_naming_the_member() {
    for (text, needle) in [
        (r"{}", "needs a \"calls\" array"),
        (r#"{"calls": null}"#, "\"calls\" must be a list"),
        (
            r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "response": {}}]}"#,
            "lambda Invoke",
        ),
        (
            r#"{"calls": [{"service": null, "operation": "Invoke", "response": {}}]}"#,
            "\"service\" must be a string",
        ),
        (
            r#"{"calls": [{"service": "lambda", "operation": "Invoke"}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "lambda", "operation": "Invoke", "request": null, "response": {}}]}"#,
            "\"request\" must be an object",
        ),
        (
            r#"{"calls": [{"service": "lambda", "operation": "Invoke", "response": {}, "error": {}}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "lambda", "operation": "Invoke", "error": {"kind": "weird"}}]}"#,
            "error.kind",
        ),
        (
            r#"{"calls": [{"service": "lambda", "operation": "Invoke", "error": {"message": 5}}]}"#,
            "\"message\" must be a string",
        ),
    ] {
        let error = client(text).err().unwrap_or_default();
        assert!(error.contains(needle), "{text}: {error}");
    }
}

fn parse(text: &str) -> Result<response::LambdaResponse, String> {
    response::parse(&j(text))
}

#[test]
fn a_complete_response_is_read_member_by_member() {
    let parsed = parse(
        r#"{"phases_run": ["measure"],
            "raw_rows": [{"pattern_id": "P", "phase": "measure", "latency_ms": 1.5, "consumed_cu": null,
                          "gsi_cu": {"G": 2.0}, "throttled": false, "error": "", "key_idx": 3,
                          "vector_search_bytes": 10, "vector_write_bytes": {"V": 5.5}}],
            "seed_verification": {"T": {"expected": 1, "actual": 1, "passed": true}},
            "seed_verification_failed": false,
            "measurement_tainted": {"P": "ramp:throttle_rate=0.30"},
            "coverage": {"measured_patterns": ["P"]},
            "exact_counts": [{"pattern_id": "P", "phase": "measure", "calls": 10, "throttles": 1, "errors": 2,
                              "error_codes": {"X": 2}, "cancellation_reason_codes": {"TransactionConflict": 3}}],
            "mode": "quick", "key_sampling": "uniform", "items_per_partition": 1}"#,
    )
    .expect("valid");
    let row = &parsed.raw_rows[0];
    assert_eq!(row.latency_ms, Some(1.5));
    assert_eq!(row.consumed_cu, None);
    assert_eq!(row.error, None, "an empty error code is no error");
    assert_eq!(row.key_idx, Some(3));
    assert_eq!(row.gsi_cu, vec![("G".to_string(), 2.0)]);
    assert_eq!(row.vector_search_bytes, Some(10.0));
    assert_eq!(parsed.measured_patterns, vec!["P".to_string()]);
    assert_eq!(parsed.exact_counts[0].agg.calls, 10);
    assert_eq!(
        parsed.exact_counts[0].agg.cancellation_reason_codes.0,
        vec![("TransactionConflict".to_string(), 3)]
    );
    assert_eq!(parsed.measurement_tainted.len(), 1);
    assert_eq!(parsed.mode.as_deref(), Some("quick"));
    assert_eq!(parsed.items_per_partition, Some(1));
}

#[test]
fn an_empty_response_has_every_default() {
    let parsed = parse("{}").expect("valid");
    assert!(parsed.raw_rows.is_empty());
    assert!(parsed.phases_run.is_none());
    assert!(!parsed.seed_verification_failed);
    assert!(parsed.handler_error.is_none());
    assert!(parsed.exact_counts.is_empty());
}

#[test]
fn wrong_typed_or_null_members_reject_the_response_without_echoing_values() {
    const SECRET: &str = "FAKE-RECORDED-SECRET-4390";
    for (text, needle) in [
        ("[]", "must be a JSON object"),
        (
            r#"{"raw_rows": null}"#,
            "\"raw_rows\" must be a list when present (it is null)",
        ),
        (r#"{"raw_rows": [1]}"#, "raw_rows[0]: must be an object"),
        (
            r#"{"raw_rows": [{"latency_ms": "x"}]}"#,
            "raw_rows[0]: \"latency_ms\"",
        ),
        (
            r#"{"raw_rows": [{"throttled": 1}]}"#,
            "raw_rows[0]: \"throttled\"",
        ),
        (r#"{"raw_rows": [{"error": 5}]}"#, "raw_rows[0]: \"error\""),
        (
            r#"{"raw_rows": [{"key_idx": "a"}]}"#,
            "raw_rows[0]: \"key_idx\"",
        ),
        (
            r#"{"raw_rows": [{"gsi_cu": {"G": "x"}}]}"#,
            "\"gsi_cu\" must be an object of numbers",
        ),
        (
            r#"{"raw_rows": [{"pattern_id": null}]}"#,
            "\"pattern_id\" must be a string",
        ),
        (
            r#"{"seed_verification": null}"#,
            "\"seed_verification\" must be an object",
        ),
        (
            r#"{"seed_verification": {"T": 1}}"#,
            "must map tables to objects",
        ),
        (
            r#"{"seed_verification": {"T": {"passed": "false"}}}"#,
            "\"passed\" must be a boolean",
        ),
        (
            r#"{"seed_verification": {"T": {"passed": null}}}"#,
            "\"passed\" must be a boolean",
        ),
        (
            r#"{"seed_verification": {"T": {"actual": 1}}}"#,
            "\"passed\" is required",
        ),
        (
            r#"{"seed_verification": {"T": {"passed": true, "sampled": 1}}}"#,
            "\"sampled\" must be a boolean",
        ),
        (
            r#"{"seed_verification": {"T": {"passed": true, "expected": []}}}"#,
            "\"expected\" must be an integer",
        ),
        (
            r#"{"seed_verification": {"T": {"passed": true, "actual": null}}}"#,
            "\"actual\" must be an integer",
        ),
        (
            r#"{"seed_verification": {"T": {"passed": true, "seed_shortfall_ratio": {}}}}"#,
            "\"seed_shortfall_ratio\" must be a number",
        ),
        (
            r#"{"measurement_tainted": {"P": 1}}"#,
            "must map pattern ids to strings",
        ),
        (r#"{"coverage": null}"#, "\"coverage\" must be an object"),
        (
            r#"{"coverage": {"measured_patterns": [1]}}"#,
            "\"measured_patterns\" must be a list of strings",
        ),
        (r#"{"exact_counts": {}}"#, "\"exact_counts\" must be a list"),
        (
            r#"{"exact_counts": [1]}"#,
            "exact_counts[0]: must be an object",
        ),
        (
            r#"{"exact_counts": [{"phase": "measure"}]}"#,
            "\"pattern_id\" is required",
        ),
        (
            r#"{"exact_counts": [{"pattern_id": "P", "phase": "m", "calls": "1"}]}"#,
            "\"calls\" must be an integer",
        ),
        (
            r#"{"exact_counts": [{"pattern_id": "P", "phase": "m", "error_codes": {"X": 1.5}}]}"#,
            "integer counts",
        ),
        (
            r#"{"phases_run": null}"#,
            "\"phases_run\" must be a list of strings",
        ),
        (r#"{"mode": 1}"#, "\"mode\" must be a string"),
        (
            r#"{"items_per_partition": "1"}"#,
            "\"items_per_partition\" must be an integer",
        ),
        (
            r#"{"seed_verification_failed": "yes"}"#,
            "\"seed_verification_failed\" must be a boolean",
        ),
    ] {
        let error = parse(text).err().unwrap_or_default();
        assert!(error.contains(needle), "{text}: {error}");
    }
    let secret_row = format!(r#"{{"raw_rows": [{{"latency_ms": "{SECRET}"}}]}}"#);
    let error = parse(&secret_row).err().unwrap_or_default();
    assert!(!error.contains(SECRET), "{error}");
}
