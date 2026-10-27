//! Tests of the CloudWatch client seam, recorded replay, gateway, Logs query
//! polling and the instrumentation / snapshot operations (#4381). No test
//! reaches AWS: every client is scripted or recorded.

use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::{parse_json, Json};

use super::aws_client::{
    failure_of, failure_text, AwsCalls, NoLiveClient, Service, NO_LIVE_CLIENT_MESSAGE,
};
use super::cli::Args;
use super::crud_tools;
use super::errors::AwsFailure;
use super::gateway::{AppSignalsOp, Gateway};
use super::logs_query::{execute_cloudwatch_query, LogsOp, Pacer, VirtualPacer};
use super::ops_cli::{
    instrumentation_contract, snapshot_contract, write_owner_only_with, WriteStage,
    INSTRUMENTATION_ARGUMENTS, INSTRUMENTATION_OPS, SNAPSHOT_ARGUMENTS, SNAPSHOT_OPS,
};
use super::recorded::RecordedClient;
use super::snapshot_tools;
use super::status_tools;
use super::timefmt::parse_iso_timestamp;
use crate::dynamodb::pyfmt;

type Handler = Box<dyn FnMut(Service, &str, &Json) -> Result<Json, AwsError>>;

/// A scripted client that logs every call.
pub(super) struct Fake {
    pub calls: Vec<(Service, String, Json)>,
    handler: Handler,
}

impl Fake {
    pub fn new(
        handler: impl FnMut(Service, &str, &Json) -> Result<Json, AwsError> + 'static,
    ) -> Self {
        Self {
            calls: Vec::new(),
            handler: Box::new(handler),
        }
    }

    /// Answers every call with `response`.
    pub fn always(response: &str) -> Self {
        let response = json(response);
        Self::new(move |_, _, _| Ok(response.clone()))
    }

    pub fn request_text(&self, index: usize) -> String {
        pyfmt::dumps(&self.calls[index].2, false)
    }
}

impl AwsCalls for Fake {
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, AwsError> {
        self.calls
            .push((service, operation.to_string(), request.clone()));
        (self.handler)(service, operation, request)
    }
}

pub(super) fn service_error(code: &str, message: &str) -> AwsError {
    AwsError::Service(Box::new(ServiceError::new(
        "POST",
        "/",
        400,
        Some(code),
        message,
        None,
    )))
}

pub(super) fn json(text: &str) -> Json {
    parse_json(text).unwrap_or_else(|error| panic!("bad test JSON {text:?}: {error}"))
}

fn op_args(table: &[(&str, super::cli::ArgSpec)], op: &str, text: &str) -> Args {
    let spec = super::cli::spec_of(table, op).expect("known op");
    Args::new(op, &json(text), spec).expect("valid arguments")
}

fn instrumentation_args(op: &str, text: &str) -> Args {
    op_args(INSTRUMENTATION_ARGUMENTS, op, text)
}

fn snapshot_args(op: &str, text: &str) -> Args {
    op_args(SNAPSHOT_ARGUMENTS, op, text)
}

fn now() -> super::timefmt::UtcTime {
    parse_iso_timestamp("2025-02-03T18:42:00Z").expect("fixed instant")
}

// ---- client seam --------------------------------------------------------

#[test]
fn no_live_client_sends_nothing_and_reports_why() {
    let mut client = NoLiveClient;
    let error = client
        .call(
            Service::ApplicationSignals,
            "GetInstrumentationConfiguration",
            &json("{}"),
        )
        .expect_err("no live client");
    assert!(
        error.to_string().contains(NO_LIVE_CLIENT_MESSAGE),
        "{error}"
    );
}

#[test]
fn service_names_round_trip() {
    for service in Service::ALL {
        assert_eq!(Service::from_name(service.name()), Some(service));
    }
    assert_eq!(Service::from_name("s3"), None);
}

#[test]
fn failures_map_onto_the_legacy_exception_classes() {
    let client = failure_of(
        "StartQuery",
        &service_error("ThrottlingException", "slow down"),
    );
    assert_eq!(
        client,
        AwsFailure::Client {
            code: Some("ThrottlingException".to_string()),
            message: Some("slow down".to_string()),
            display: "An error occurred (ThrottlingException) when calling the StartQuery \
                      operation: slow down"
                .to_string(),
        }
    );
    assert!(matches!(
        failure_of("X", &AwsError::NoCredentials),
        AwsFailure::Credentials { ref type_name, .. } if type_name == "NoCredentialsError"
    ));
    assert!(matches!(
        failure_of("X", &AwsError::connect("down")),
        AwsFailure::EndpointConnection(_)
    ));
    assert_eq!(
        failure_text(&failure_of("X", &service_error("", ""))),
        "An error occurred (Unknown) when calling the X operation: "
    );
}

#[test]
fn gateway_allowlists_six_operations_and_three_mutate() {
    let names: Vec<&str> = AppSignalsOp::ALL.iter().map(|op| op.api_name()).collect();
    assert_eq!(
        names,
        [
            "CreateInstrumentationConfiguration",
            "ListInstrumentationConfigurations",
            "GetInstrumentationConfiguration",
            "DeleteInstrumentationConfiguration",
            "BatchDeleteInstrumentationConfigurations",
            "GetInstrumentationConfigurationStatus",
        ]
    );
    let mutating: Vec<&str> = AppSignalsOp::ALL
        .iter()
        .filter(|op| op.is_mutating())
        .map(|op| op.api_name())
        .collect();
    assert_eq!(mutating.len(), 3);
    assert!(mutating.iter().all(|name| {
        name.starts_with("Create") || name.starts_with("Delete") || name.starts_with("BatchDelete")
    }));
    let logs: Vec<&str> = LogsOp::ALL.iter().map(|op| op.api_name()).collect();
    assert_eq!(logs, ["StartQuery", "GetQueryResults"]);
}

#[test]
fn gateway_rejects_a_non_object_response() {
    let mut fake = Fake::always("[]");
    let mut gateway = Gateway::new(&mut fake);
    let error = gateway
        .call(AppSignalsOp::List, &json("{}"))
        .expect_err("an array is not a response");
    assert_eq!(
        failure_text(&error.failure),
        "the ListInstrumentationConfigurations response is not an object (it is array)"
    );
}

// ---- recorded replay ----------------------------------------------------

fn recorded(text: &str) -> RecordedClient {
    RecordedClient::from_document(&json(text)).expect("valid recording")
}

#[test]
fn recorded_calls_are_consumed_in_order_and_the_last_one_repeats() {
    let mut client = recorded(
        r#"{"calls": [
            {"service": "logs", "operation": "GetQueryResults", "response": {"status": "Running"}},
            {"service": "logs", "operation": "GetQueryResults", "response": {"status": "Complete"}}
        ]}"#,
    );
    let statuses: Vec<String> = (0..3)
        .map(|_| {
            client
                .call(
                    Service::Logs,
                    "GetQueryResults",
                    &json(r#"{"queryId": "q"}"#),
                )
                .expect("recorded")
                .get("status")
                .and_then(Json::as_str)
                .expect("status")
                .to_string()
        })
        .collect();
    assert_eq!(statuses, ["Running", "Complete", "Complete"]);
}

#[test]
fn recorded_request_must_be_contained_in_the_actual_request() {
    let mut client = recorded(
        r#"{"calls": [
            {"service": "bedrock-agentcore", "operation": "Evaluate",
             "request": {"evaluationTarget": {"traceIds": ["aa"]}},
             "response": {"evaluationResults": []}}
        ]}"#,
    );
    let actual = json(
        r#"{"evaluatorId": "E", "evaluationInput": {"sessionSpans": []},
            "evaluationTarget": {"traceIds": ["aa"]}}"#,
    );
    assert!(client
        .call(Service::BedrockAgentCore, "Evaluate", &actual)
        .is_ok());
    let other = json(r#"{"evaluationTarget": {"traceIds": ["bb"]}}"#);
    let error = client
        .call(Service::BedrockAgentCore, "Evaluate", &other)
        .expect_err("different trace id");
    let text = error.to_string();
    assert!(text.contains("bedrock-agentcore Evaluate"), "{text}");
    assert!(
        !text.contains("bb"),
        "an unmatched call never echoes the request: {text}"
    );
}

#[test]
fn recorded_errors_become_sdk_errors_of_each_kind() {
    let mut client = recorded(
        r#"{"calls": [
            {"service": "logs", "operation": "A", "error": {"code": "Boom", "message": "bad"}},
            {"service": "logs", "operation": "B", "error": {"kind": "connect", "message": "down"}},
            {"service": "logs", "operation": "C", "error": {"kind": "timeout", "message": "slow"}},
            {"service": "logs", "operation": "D", "error": {"kind": "no_credentials"}}
        ]}"#,
    );
    let request = json("{}");
    assert!(matches!(
        failure_of("A", &client.call(Service::Logs, "A", &request).unwrap_err()),
        AwsFailure::Client { code: Some(code), .. } if code == "Boom"
    ));
    assert!(matches!(
        failure_of("B", &client.call(Service::Logs, "B", &request).unwrap_err()),
        AwsFailure::EndpointConnection(_)
    ));
    assert!(matches!(
        failure_of("C", &client.call(Service::Logs, "C", &request).unwrap_err()),
        AwsFailure::Timeout(_)
    ));
    assert!(matches!(
        failure_of("D", &client.call(Service::Logs, "D", &request).unwrap_err()),
        AwsFailure::Credentials { .. }
    ));
}

#[test]
fn recorded_files_with_wrong_typed_or_null_members_are_rejected() {
    let cases = [
        (r"[]", "JSON object"),
        (r"{}", "\"calls\" array"),
        (r#"{"calls": null}"#, "\"calls\" array"),
        (r#"{"calls": [1]}"#, "recorded call 0: must be an object"),
        (
            r#"{"calls": [{"service": "s3", "operation": "X", "response": {}}]}"#,
            "\"service\" must be",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "", "response": {}}]}"#,
            "non-empty string",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "request": null, "response": {}}]}"#,
            "\"request\" must be an object when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "request": [], "response": {}}]}"#,
            "\"request\" must be an object when present (it is array)",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "error": {"code": 5}}]}"#,
            "error \"code\" must be a string when present (it is integer)",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "error": {"message": null}}]}"#,
            "error \"message\" must be a string when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "error": {"kind": 1}}]}"#,
            "error \"kind\" must be a string when present (it is integer)",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "error": {"kind": "weird"}}]}"#,
            "\"error.kind\" must be client, connect, timeout or no_credentials",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "response": {}, "error": {}}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "logs", "operation": "X", "response": []}]}"#,
            "exactly one of",
        ),
    ];
    for (text, expected) in cases {
        let Err(reason) = RecordedClient::from_document(&json(text)) else {
            panic!("{text} should be rejected");
        };
        assert!(reason.contains(expected), "{text}: {reason}");
    }
}

// ---- logs query ---------------------------------------------------------

fn query_with(
    client: &mut dyn AwsCalls,
    max_timeout: i64,
) -> super::snapshot_rendering::QueryResult {
    execute_cloudwatch_query(
        client,
        &mut VirtualPacer::default(),
        "fields @timestamp",
        10,
        20,
        "/aws/service-events/svc",
        max_timeout,
    )
}

#[test]
fn query_returns_rows_as_field_value_objects() {
    let mut fake = Fake::new(|_, operation, _| match operation {
        "StartQuery" => Ok(json(r#"{"queryId": "q-1"}"#)),
        _ => Ok(json(
            r#"{"status": "Complete", "messages": [{"note": 1}],
                "results": [[{"field": "@timestamp", "value": "t"},
                             {"field": "@message", "value": "m1"},
                             {"field": "@message", "value": "m2"},
                             {"value": "nameless"}]]}"#,
        )),
    });
    let result = query_with(&mut fake, 30);
    assert_eq!(result.status, "Complete");
    assert_eq!(result.query_id, Json::string("q-1"));
    assert_eq!(result.error, None);
    assert_eq!(
        pyfmt::dumps(&result.results[0], false),
        r#"{"@timestamp": "t", "@message": "m2", "": "nameless"}"#
    );
    assert_eq!(result.messages.len(), 1);
    assert_eq!(
        fake.request_text(0),
        r#"{"logGroupName": "/aws/service-events/svc", "startTime": 10, "endTime": 20, "queryString": "fields @timestamp"}"#
    );
    assert_eq!(fake.request_text(1), r#"{"queryId": "q-1"}"#);
}

#[test]
fn query_start_errors_keep_the_legacy_texts() {
    let mut client_error = Fake::new(|_, _, _| Err(service_error("AccessDenied", "no")));
    let result = query_with(&mut client_error, 30);
    assert_eq!(result.status, "Error");
    assert_eq!(
        result.error.as_deref(),
        Some("Failed to start query: An error occurred (AccessDenied) when calling the StartQuery operation: no")
    );
    assert_eq!(result.query_id, Json::Null);

    let mut transport = Fake::new(|_, _, _| Err(AwsError::connect("no route")));
    let result = query_with(&mut transport, 30);
    assert_eq!(result.error.as_deref(), Some("no route"));

    let mut no_id = Fake::always(r#"{"queryId": ""}"#);
    let result = query_with(&mut no_id, 30);
    assert_eq!(
        result.error.as_deref(),
        Some("start_query did not return a queryId (response: {'queryId': ''})")
    );
}

#[test]
fn query_poll_errors_carry_the_query_id() {
    let mut fake = Fake::new(|_, operation, _| match operation {
        "StartQuery" => Ok(json(r#"{"queryId": "q-9"}"#)),
        _ => Err(service_error("ThrottlingException", "slow")),
    });
    let result = query_with(&mut fake, 30);
    assert_eq!(result.status, "Error");
    assert_eq!(result.query_id, Json::string("q-9"));
    assert!(result.error.as_deref().is_some_and(
        |text| text.starts_with("Failed to get results: An error occurred (Throttling")
    ));
}

#[test]
fn query_times_out_after_max_timeout_polls_without_sleeping() {
    let mut fake = Fake::new(|_, operation, _| match operation {
        "StartQuery" => Ok(json(r#"{"queryId": "q-1"}"#)),
        _ => Ok(json(r#"{"status": "Running"}"#)),
    });
    let result = query_with(&mut fake, 5);
    assert_eq!(result.status, "Polling Timeout");
    assert_eq!(
        result.error.as_deref(),
        Some("Query did not complete within 5 seconds.")
    );
    assert_eq!(fake.calls.len(), 1 + 5, "one start and one poll per second");

    let mut none = Fake::new(|_, _, _| Ok(json(r#"{"queryId": "q"}"#)));
    let result = query_with(&mut none, 0);
    assert_eq!(result.status, "Polling Timeout");
    assert_eq!(none.calls.len(), 1, "a zero timeout never polls");
}

#[test]
fn query_rejects_a_malformed_result_row_without_panicking() {
    let mut fake = Fake::new(|_, operation, _| match operation {
        "StartQuery" => Ok(json(r#"{"queryId": "q"}"#)),
        _ => Ok(json(r#"{"status": "Complete", "results": ["not a row"]}"#)),
    });
    let result = query_with(&mut fake, 30);
    assert_eq!(result.status, "Error");
    assert!(result
        .error
        .as_deref()
        .is_some_and(|e| e.contains("unexpected shape")));
}

#[test]
fn virtual_pacer_counts_sleeps_instead_of_waiting() {
    let mut pacer = VirtualPacer::default();
    pacer.sleep(1.0);
    pacer.sleep(2.0);
    assert!((pacer.now() - 3.0).abs() < f64::EPSILON);
}

// ---- create / list / get / delete ---------------------------------------

const CREATE_OK: &str = r#"{"LocationHash": "0123456789abcdef", "ARN": "arn:aws:x",
    "CreatedAt": "2025-02-03T18:42:00Z"}"#;

fn create(text: &str, fake: &mut Fake) -> super::result::OpResult {
    let args = instrumentation_args("create", text);
    let mut gateway = Gateway::new(fake);
    crud_tools::create_instrumentation(&mut gateway, &args, now()).expect("usable arguments")
}

#[test]
fn create_sends_the_python_request_in_member_order() {
    let mut fake = Fake::always(CREATE_OK);
    let result = create(
        r#"{"instrumentation_type": " breakpoint ", "service": "svc", "environment": "prod",
            "language": "python", "file_path": "/app/h.py", "code_unit": "app.h",
            "method_name": "handle", "capture_arguments": ["x"], "max_hits": 5,
            "ttl_hours": 2, "attribute_filters": [{"service.version": "1.2.0"}],
            "description": "mine"}"#,
        &mut fake,
    );
    assert!(result.ok, "{}", result.text);
    assert_eq!(fake.calls.len(), 1);
    assert_eq!(fake.calls[0].1, "CreateInstrumentationConfiguration");
    assert_eq!(
        fake.request_text(0),
        concat!(
            r#"{"InstrumentationType": "BREAKPOINT", "Service": "svc", "Environment": "prod", "#,
            r#""SignalType": "SNAPSHOT", "Location": {"CodeLocation": {"Language": "Python", "#,
            r#""FilePath": "/app/h.py", "CodeUnit": "app.h", "MethodName": "handle"}}, "#,
            r#""CaptureConfiguration": {"CodeCapture": {"CaptureReturn": true, "#,
            r#""CaptureStackTrace": true, "CaptureLimits": {"MaxHits": 5}, "#,
            r#""CaptureArguments": ["x"]}}, "Description": "mine", "#,
            r#""ExpiresAt": "2025-02-03T20:42:00Z", "#,
            r#""AttributeFilters": [{"service.version": "1.2.0"}]}"#
        )
    );
    assert!(result.text.contains("0123456789abcdef"), "{}", result.text);
}

#[test]
fn create_input_failures_never_reach_the_client() {
    let cases = [
        (
            r#"{"instrumentation_type": "x", "service": "s", "environment": "e"}"#,
            "ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: x)",
        ),
        (
            r#"{"instrumentation_type": "PROBE", "service": "s", "environment": "e",
                "language": "javascript", "file_path": "/a.js"}"#,
            "ERROR: PROBE is not supported for JavaScript.",
        ),
        (
            r#"{"instrumentation_type": "PROBE", "service": "s", "environment": "e",
                "language": "python", "file_path": "/a.py", "line_number": 3}"#,
            "ERROR: PROBE does not support line_number",
        ),
        (
            r#"{"instrumentation_type": "BREAKPOINT", "service": "s", "environment": "e"}"#,
            "ERROR: BREAKPOINT/PROBE require language and file_path.",
        ),
        (
            r#"{"instrumentation_type": "BREAKPOINT", "service": "s", "environment": "e",
                "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m",
                "capture_arguments": ["*"]}"#,
            "ERROR: capture_arguments does not support the wildcard \"*\".",
        ),
        (
            r#"{"instrumentation_type": "BREAKPOINT", "service": "s", "environment": "e",
                "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m",
                "line_number": 4}"#,
            "ERROR: line-level instrumentation (line_number set) requires capture_locals.",
        ),
    ];
    for (text, expected) in cases {
        let mut fake = Fake::always("{}");
        let result = create(text, &mut fake);
        assert!(!result.ok, "{text}");
        assert!(
            result.text.starts_with(expected) || result.text.contains(expected),
            "{}",
            result.text
        );
        assert!(fake.calls.is_empty(), "{text} must not call the API");
    }
}

#[test]
fn create_defaults_line_level_capture_to_no_return_value() {
    let mut fake = Fake::always(CREATE_OK);
    let result = create(
        r#"{"instrumentation_type": "BREAKPOINT", "service": "s", "environment": "e",
            "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m",
            "line_number": 4, "capture_locals": ["v"], "ttl_hours": 99999999999}"#,
        &mut fake,
    );
    assert!(!result.ok);
    assert!(
        result.text.contains("ttl_hours puts the expiry outside"),
        "{}",
        result.text
    );
    assert!(fake.calls.is_empty());

    let mut fake = Fake::always(CREATE_OK);
    let result = create(
        r#"{"instrumentation_type": "BREAKPOINT", "service": "s", "environment": "e",
            "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m",
            "line_number": 4, "capture_locals": ["v"]}"#,
        &mut fake,
    );
    assert!(result.ok, "{}", result.text);
    assert!(
        fake.request_text(0).contains(r#""CaptureReturn": false"#),
        "{}",
        fake.request_text(0)
    );
}

#[test]
fn create_failure_uses_the_tailored_template_with_location_troubleshooting() {
    let mut fake = Fake::new(|_, _, _| Err(service_error("ConflictException", "exists")));
    let result = create(
        r#"{"instrumentation_type": "BREAKPOINT", "service": "svc", "environment": "prod",
            "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m"}"#,
        &mut fake,
    );
    assert!(!result.ok);
    for expected in [
        "Failed to create BREAKPOINT instrumentation",
        "ConflictException",
        "ATTEMPTED CONFIGURATION:",
        "- Service: svc",
        "POSSIBLE CAUSES:",
        "1. AWS credentials missing or scoped to a different account",
        "TROUBLESHOOTING:",
        "CODE LOCATION TROUBLESHOOTING:",
    ] {
        assert!(
            result.text.contains(expected),
            "{expected}: {}",
            result.text
        );
    }
}

#[test]
fn transport_failures_use_the_canned_template_not_the_tailored_one() {
    let mut fake = Fake::new(|_, _, _| Err(AwsError::NoCredentials));
    let result = create(
        r#"{"instrumentation_type": "BREAKPOINT", "service": "svc", "environment": "prod",
            "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m"}"#,
        &mut fake,
    );
    assert!(!result.ok);
    assert!(
        !result.text.contains("ATTEMPTED CONFIGURATION:"),
        "{}",
        result.text
    );
    assert!(
        result.text.contains("Unable to locate credentials"),
        "{}",
        result.text
    );
}

#[test]
fn list_omits_default_members_and_sends_set_ones() {
    let mut fake = Fake::always(r#"{"Configurations": []}"#);
    let args = instrumentation_args(
        "list",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "probe"}"#,
    );
    let result = crud_tools::list_instrumentations(&mut Gateway::new(&mut fake), &args).unwrap();
    assert!(result.ok, "{}", result.text);
    assert_eq!(
        fake.request_text(0),
        r#"{"Service": "s", "Environment": "e", "InstrumentationType": "PROBE"}"#
    );

    let mut fake = Fake::always(r#"{"Configurations": []}"#);
    let args = instrumentation_args(
        "list",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "PROBE",
            "synced_at": "2025-01-01T00:00:00Z", "max_results": 5, "next_token": "tok"}"#,
    );
    crud_tools::list_instrumentations(&mut Gateway::new(&mut fake), &args).unwrap();
    assert_eq!(
        fake.request_text(0),
        concat!(
            r#"{"Service": "s", "Environment": "e", "InstrumentationType": "PROBE", "#,
            r#""SyncedAt": "2025-01-01T00:00:00Z", "MaxResults": 5, "NextToken": "tok"}"#
        )
    );
}

#[test]
fn delete_and_get_resolve_a_hash_or_a_code_location() {
    let mut fake = Fake::always("{}");
    let args = instrumentation_args(
        "delete",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
            "location_hash": "0123456789abcdef"}"#,
    );
    let result = crud_tools::delete_instrumentation(&mut Gateway::new(&mut fake), &args).unwrap();
    assert!(result.ok);
    assert_eq!(
        result.text,
        "Successfully deleted BREAKPOINT instrumentation\n\n\
         Target: LocationHash 0123456789abcdef\nService: s\nEnvironment: e\n\n\
         TIP: Use list_instrumentations to verify removal."
    );
    assert_eq!(
        fake.request_text(0),
        concat!(
            r#"{"InstrumentationType": "BREAKPOINT", "Service": "s", "Environment": "e", "#,
            r#""SignalType": "SNAPSHOT", "LocationIdentifier": {"LocationHash": "0123456789abcdef"}}"#
        )
    );
}

#[test]
fn delete_and_get_without_an_identifier_print_the_usage_help() {
    for (op, action) in [("delete", "delete"), ("get", "get")] {
        let mut fake = Fake::always("{}");
        let args = instrumentation_args(
            op,
            r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT"}"#,
        );
        let result = if op == "delete" {
            crud_tools::delete_instrumentation(&mut Gateway::new(&mut fake), &args)
        } else {
            crud_tools::get_instrumentation(&mut Gateway::new(&mut fake), &args)
        }
        .unwrap();
        assert!(!result.ok);
        assert!(
            result
                .text
                .starts_with("ERROR: Must provide one of:\n- location_hash\n"),
            "{}",
            result.text
        );
        assert!(
            result.text.contains(&format!(
                "1. {action} by hash:\n   {action}_instrumentation(location_hash="
            )),
            "{}",
            result.text
        );
        assert!(fake.calls.is_empty());
    }
}

#[test]
fn get_reports_a_missing_configuration() {
    let mut fake = Fake::always(r#"{"Configuration": {}}"#);
    let args = instrumentation_args(
        "get",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
            "location_hash": "0123456789abcdef"}"#,
    );
    let result = crud_tools::get_instrumentation(&mut Gateway::new(&mut fake), &args).unwrap();
    assert!(!result.ok);
    assert_eq!(
        result.text,
        "No instrumentation found for LocationHash 0123456789abcdef"
    );
}

#[test]
fn batch_delete_ok_reflects_per_item_errors_in_the_response() {
    let args = instrumentation_args(
        "batch-delete-by-scope",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT"}"#,
    );
    let mut clean = Fake::always(r#"{"DeletedCount": 2, "SuccessfulDeletions": [], "Errors": []}"#);
    let result = crud_tools::batch_delete_by_scope(&mut Gateway::new(&mut clean), &args).unwrap();
    assert!(result.ok);
    assert!(
        result.text.starts_with("BATCH DELETE COMPLETED"),
        "{}",
        result.text
    );
    assert_eq!(
        clean.request_text(0),
        concat!(
            r#"{"DeletionTarget": {"Scope": {"Service": "s", "Environment": "e", "#,
            r#""InstrumentationType": "BREAKPOINT"}}}"#
        )
    );

    let mut failed = Fake::always(r#"{"DeletedCount": 0, "Errors": [{"Message": "x"}]}"#);
    let result = crud_tools::batch_delete_by_scope(&mut Gateway::new(&mut failed), &args).unwrap();
    assert!(
        !result.ok,
        "a batch where every item errored must report failure"
    );
    assert!(
        result.text.starts_with("BATCH DELETE COMPLETED"),
        "{}",
        result.text
    );
}

#[test]
fn batch_delete_by_arns_validates_before_sending() {
    let run = |arns: &str| {
        let mut fake = Fake::always(r#"{"DeletedCount": 1}"#);
        let args = instrumentation_args(
            "batch-delete-by-arns",
            &format!(r#"{{"resource_arns": {arns}, "instrumentation_type": "BREAKPOINT"}}"#),
        );
        let result = crud_tools::batch_delete_by_arns(&mut Gateway::new(&mut fake), &args).unwrap();
        (result, fake)
    };
    let (empty, fake) = run("[]");
    assert_eq!(
        empty.text,
        "ERROR: resource_arns must contain at least one ARN."
    );
    assert!(fake.calls.is_empty());

    let many = format!("[{}]", vec!["\"arn:a\""; 51].join(", "));
    let (too_many, fake) = run(&many);
    assert_eq!(
        too_many.text,
        "ERROR: resource_arns can include at most 50 ARNs per request."
    );
    assert!(fake.calls.is_empty());

    for bad in [r#"["arn:a", " "]"#, r#"["arn:a", 5]"#, r#"["arn:a", null]"#] {
        let (result, fake) = run(bad);
        assert_eq!(
            result.text, "ERROR: resource_arns must contain non-empty ARN strings only.",
            "{bad}"
        );
        assert!(fake.calls.is_empty());
    }

    let (ok, fake) = run(r#"["arn:a", "arn:b"]"#);
    assert!(ok.ok, "{}", ok.text);
    assert_eq!(
        fake.request_text(0),
        concat!(
            r#"{"DeletionTarget": {"ResourceArns": {"ResourceArns": ["arn:a", "arn:b"], "#,
            r#""InstrumentationType": "BREAKPOINT"}}}"#
        )
    );
}

// ---- status -------------------------------------------------------------

fn get_status(text: &str, fake: &mut Fake) -> super::result::OpResult {
    let args = instrumentation_args("get-status", text);
    status_tools::get_status(&mut Gateway::new(fake), &args).expect("usable arguments")
}

#[test]
fn get_status_requires_an_explicit_known_status() {
    let base = r#""service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
        "location_hash": "0123456789abcdef""#;
    let mut fake = Fake::always("{}");
    let missing = get_status(&format!("{{{base}}}"), &mut fake);
    assert!(
        missing
            .text
            .starts_with("ERROR: status is required\n\nThis API cannot"),
        "{}",
        missing.text
    );
    let blank = get_status(&format!(r#"{{{base}, "status": "  "}}"#), &mut fake);
    assert!(blank.text.starts_with("ERROR: status is required"));
    let unknown = get_status(&format!(r#"{{{base}, "status": "bogus"}}"#), &mut fake);
    assert_eq!(
        unknown.text,
        "ERROR: invalid status. Must be one of: READY, ACTIVE, ERROR, DISABLED (received: bogus)"
    );
    let signal = get_status(
        &format!(r#"{{{base}, "status": "ready", "signal_type": "TRACE"}}"#),
        &mut fake,
    );
    assert!(
        signal
            .text
            .starts_with("ERROR: signal_type must be SNAPSHOT"),
        "{}",
        signal.text
    );
    assert!(fake.calls.is_empty());
}

#[test]
fn get_status_sends_the_normalized_status_and_parsed_window() {
    let mut fake = Fake::always(r#"{"Events": []}"#);
    let result = get_status(
        r#"{"service": "s", "environment": "e", "instrumentation_type": "breakpoint",
            "location_hash": "0123456789abcdef", "status": " active ",
            "start_time": "2025-02-03T18:00:00Z", "end_time": "2025-02-03T19:00:00",
            "max_results": 7, "next_token": "t"}"#,
        &mut fake,
    );
    assert!(result.ok, "{}", result.text);
    assert_eq!(
        fake.request_text(0),
        concat!(
            r#"{"InstrumentationType": "BREAKPOINT", "Service": "s", "Environment": "e", "#,
            r#""SignalType": "SNAPSHOT", "Status": "ACTIVE", "#,
            r#""LocationIdentifier": {"LocationHash": "0123456789abcdef"}, "#,
            r#""StartTime": "2025-02-03T18:00:00Z", "EndTime": "2025-02-03T19:00:00Z", "#,
            r#""MaxResults": 7, "NextToken": "t"}"#
        )
    );

    let mut fake = Fake::always("{}");
    let bad = get_status(
        r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
            "location_hash": "0123456789abcdef", "status": "READY", "start_time": "yesterday"}"#,
        &mut fake,
    );
    assert!(
        bad.text.starts_with(
            "ERROR: Invalid start_time format. Expected ISO 8601. Error: Invalid isoformat string:"
        ),
        "{}",
        bad.text
    );
    assert!(fake.calls.is_empty());
}

fn check_status(text: &str, fake: &mut Fake) -> super::result::OpResult {
    let args = instrumentation_args("check-status", text);
    status_tools::check_status(&mut Gateway::new(fake), &args).expect("usable arguments")
}

const CHECK_ARGS: &str = r#""service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
    "location_hash": "0123456789abcdef""#;

fn status_script(
    created_at: &'static str,
    events: &'static [(&'static str, &'static str)],
) -> Fake {
    Fake::new(move |_, operation, request| {
        if operation == "GetInstrumentationConfiguration" {
            return Ok(json(&format!(
                r#"{{"Configuration": {{"CreatedAt": "{created_at}"}}}}"#
            )));
        }
        let status = request.get("Status").and_then(Json::as_str).unwrap_or("");
        let body = events
            .iter()
            .find(|(name, _)| *name == status)
            .map_or("[]", |(_, body)| *body);
        Ok(json(&format!(r#"{{"Events": {body}}}"#)))
    })
}

#[test]
fn check_status_clamps_the_active_window_to_the_creation_time() {
    let mut fake = status_script(
        "2025-02-03T18:30:00Z",
        &[("ACTIVE", r#"[{"Time": "2025-02-03T18:40:00Z"}]"#)],
    );
    let result = check_status(
        &format!(
            r#"{{{CHECK_ARGS}, "start_time": "2025-02-03T18:00:00Z", "end_time": "2025-02-03T19:00:00Z"}}"#
        ),
        &mut fake,
    );
    assert!(result.ok, "{}", result.text);
    assert!(result.text.contains("ACTIVE"), "{}", result.text);
    assert_eq!(
        fake.calls.len(),
        2,
        "one created_at fetch and the ACTIVE check only"
    );
    assert!(fake.request_text(1).contains(r#""Status": "ACTIVE""#));
    assert!(
        fake.request_text(1)
            .contains(r#""StartTime": "2025-02-03T18:30:00Z""#),
        "ACTIVE starts at max(created_at, start): {}",
        fake.request_text(1)
    );
}

#[test]
fn check_status_falls_through_active_then_ready_then_error() {
    let mut fake = status_script("2025-02-03T17:00:00Z", &[]);
    let result = check_status(
        &format!(
            r#"{{{CHECK_ARGS}, "start_time": "2025-02-03T18:00:00Z", "end_time": "2025-02-03T19:00:00Z"}}"#
        ),
        &mut fake,
    );
    assert!(result.ok, "{}", result.text);
    let statuses: Vec<String> = (1..fake.calls.len())
        .map(|i| {
            fake.calls[i]
                .2
                .get("Status")
                .and_then(Json::as_str)
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(statuses, ["ACTIVE", "READY", "ERROR"]);
    assert!(
        result.text.contains("PENDING"),
        "no events at all is PENDING: {}",
        result.text
    );
}

#[test]
fn check_status_skips_active_when_the_window_is_empty() {
    let mut fake = status_script("2025-02-03T20:00:00Z", &[]);
    let result = check_status(
        &format!(
            r#"{{{CHECK_ARGS}, "start_time": "2025-02-03T18:00:00Z", "end_time": "2025-02-03T19:00:00Z"}}"#
        ),
        &mut fake,
    );
    assert!(result.ok, "{}", result.text);
    assert!(
        result
            .text
            .contains("Skipped: ACTIVE query window is empty"),
        "{}",
        result.text
    );
    assert!(!fake
        .calls
        .iter()
        .skip(1)
        .any(|c| c.2.get("Status").and_then(Json::as_str) == Some("ACTIVE")));
}

#[test]
fn check_status_reports_a_status_query_failure_as_an_api_error_line() {
    let mut fake = Fake::new(|_, operation, _| {
        if operation == "GetInstrumentationConfiguration" {
            Ok(json(
                r#"{"Configuration": {"CreatedAt": "2025-02-03T17:00:00Z"}}"#,
            ))
        } else {
            Err(service_error("ThrottlingException", "slow"))
        }
    });
    let result = check_status(
        &format!(
            r#"{{{CHECK_ARGS}, "start_time": "2025-02-03T18:00:00Z", "end_time": "2025-02-03T19:00:00Z"}}"#
        ),
        &mut fake,
    );
    assert!(
        result.ok,
        "an unreadable status is reported, not fatal: {}",
        result.text
    );
    assert!(
        result.text.contains("API error: An error occurred (ThrottlingException) when calling the GetInstrumentationConfigurationStatus operation: slow"),
        "{}",
        result.text
    );
}

#[test]
fn check_status_input_and_created_at_failures() {
    let window = r#""start_time": "2025-02-03T18:00:00Z", "end_time": "2025-02-03T19:00:00Z""#;
    let mut fake = Fake::always("{}");
    let bad_hash = check_status(
        &format!(
            r#"{{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT", "location_hash": "ABC", {window}}}"#
        ),
        &mut fake,
    );
    assert_eq!(
        bad_hash.text,
        "ERROR: location_hash must be a 16-character hex string"
    );
    assert!(fake.calls.is_empty());

    let mut fake = Fake::always(r#"{"Configuration": {}}"#);
    let missing = check_status(&format!("{{{CHECK_ARGS}, {window}}}"), &mut fake);
    assert_eq!(
        missing.text,
        "ERROR: Failed to fetch created_at: No instrumentation found for LocationHash 0123456789abcdef"
    );

    let mut fake = Fake::always(r#"{"Configuration": {"ARN": "x"}}"#);
    let no_created = check_status(&format!("{{{CHECK_ARGS}, {window}}}"), &mut fake);
    assert_eq!(
        no_created.text,
        "ERROR: Failed to fetch created_at: CreatedAt not found in instrumentation configuration"
    );

    let mut fake = Fake::new(|_, _, _| Err(service_error("ResourceNotFoundException", "gone")));
    let failed = check_status(&format!("{{{CHECK_ARGS}, {window}}}"), &mut fake);
    assert_eq!(
        failed.text,
        "ERROR: Failed to fetch created_at: Exception: An error occurred (ResourceNotFoundException) when calling the GetInstrumentationConfiguration operation: gone"
    );

    let mut fake = status_script("2025-02-03T17:00:00Z", &[]);
    let inverted = check_status(
        &format!(
            r#"{{{CHECK_ARGS}, "start_time": "2025-02-03T19:00:00Z", "end_time": "2025-02-03T18:00:00Z"}}"#
        ),
        &mut fake,
    );
    assert_eq!(
        inverted.text,
        "ERROR: end_time must be later than start_time"
    );
}

// ---- snapshots ----------------------------------------------------------

const HASH: &str = "0123456789abcdef";

fn query_script(rows: &'static str) -> Fake {
    Fake::new(move |_, operation, _| {
        if operation == "StartQuery" {
            Ok(json(r#"{"queryId": "q-1"}"#))
        } else {
            Ok(json(&format!(
                r#"{{"status": "Complete", "results": {rows}}}"#
            )))
        }
    })
}

fn search(text: &str, fake: &mut Fake) -> String {
    let args = snapshot_args("search", text);
    snapshot_tools::search_snapshots(fake, &mut VirtualPacer::default(), &args)
        .expect("usable arguments")
}

#[test]
fn search_builds_the_anchored_window_and_the_escaped_query() {
    let mut fake = query_script("[]");
    let text = search(
        &format!(
            r#"{{"service": "svc\"x", "environment": "prod", "location_hash": "{HASH}",
            "status_timestamp": "2025-02-03T18:42:00Z", "limit": "5",
            "custom_filters": "@message like /ORD-1/"}}"#
        ),
        &mut fake,
    );
    let start = &fake.calls[0].2;
    assert_eq!(
        start.get("startTime"),
        Some(&Json::Int(1_738_608_115)),
        "-5 s"
    );
    assert_eq!(
        start.get("endTime"),
        Some(&Json::Int(1_738_608_180)),
        "+60 s"
    );
    assert_eq!(
        start.get("logGroupName").and_then(Json::as_str),
        Some("/aws/service-events/svc\"x")
    );
    let query = start
        .get("queryString")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert_eq!(
        query,
        format!(
            "fields @timestamp, @message\n| filter attributes.aws.di.location_hash = \"{HASH}\" \
             and (resource.attributes.service.name = \"svc\\\"x\"      or not \
             ispresent(resource.attributes.service.name)) and \
             (resource.attributes.deployment.environment = \"prod\"      or \
             resource.attributes.deployment.environment.name = \"prod\"      or not \
             ispresent(resource.attributes.deployment.environment)) and @message like /ORD-1/\n\
             | sort @timestamp asc\n| limit 5"
        )
    );
    let parsed = parse_json(&text).expect("JSON output");
    assert!(parsed.get("status").is_some(), "{text}");
}

#[test]
fn search_input_failures_are_bare_error_lines_that_never_query() {
    let base = |extra: &str| {
        format!(
            r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}", "status_timestamp": "2025-02-03T18:42:00Z"{extra}}}"#
        )
    };
    let cases = [
        (
            r#"{"service": "s", "environment": "e", "location_hash": "XYZ", "status_timestamp": "2025-02-03T18:42:00Z"}"#.to_string(),
            "ERROR: location_hash must be a 16-character hex string",
        ),
        (base(r#", "limit": "many""#), "ERROR: limit must be an integer"),
        (base(r#", "limit": null"#), "ERROR: limit must be an integer"),
        (base(r#", "limit": [1]"#), "ERROR: limit must be an integer"),
        (
            base("").replace("2025-02-03T18:42:00Z", "soon"),
            "ERROR: status_timestamp must be ISO 8601 format like \"2025-02-03T18:42:00Z\"",
        ),
        (
            base(r#", "start_time": "2025-02-03T18:00:00Z""#),
            "ERROR: start_time and end_time must be provided together",
        ),
        (
            base(r#", "start_time": "x", "end_time": "y""#),
            "ERROR: start_time and end_time must be ISO 8601 format like",
        ),
        (
            base(r#", "start_time": "2025-02-03T19:00:00Z", "end_time": "2025-02-03T18:00:00Z""#),
            "ERROR: end_time must be after start_time",
        ),
        (
            base(r#", "custom_filters": ["a = \"b"]"#),
            "ERROR: custom_filters has unbalanced quotes: 'a = \"b'",
        ),
    ];
    for (text, expected) in cases {
        let mut fake = query_script("[]");
        let output = search(&text, &mut fake);
        assert!(output.starts_with(expected), "{text}: {output}");
        assert!(fake.calls.is_empty(), "{text} must not query");
        assert!(snapshot_tools::is_failure(&output));
    }
}

#[test]
fn search_accepts_escaped_quotes_and_skips_blank_filters() {
    let mut fake = query_script("[]");
    let output = search(
        &format!(
            r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}",
            "status_timestamp": "2025-02-03T18:42:00Z",
            "custom_filters": ["  ", "@message like /a\\\"b/ ", "x = \"y\""]}}"#
        ),
        &mut fake,
    );
    assert!(!output.starts_with("ERROR"), "{output}");
    let query = fake.calls[0]
        .2
        .get("queryString")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert!(
        query.contains(" and @message like /a\\\"b/ and x = \"y\"\n"),
        "{query}"
    );
}

#[test]
fn search_explicit_window_overrides_the_anchor() {
    let mut fake = query_script("[]");
    search(
        &format!(
            r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}",
            "status_timestamp": "2025-02-03T18:42:00Z",
            "start_time": "2025-02-03T00:00:00Z", "end_time": "2025-02-04T00:00:00Z"}}"#
        ),
        &mut fake,
    );
    let request = &fake.calls[0].2;
    assert_eq!(request.get("startTime"), Some(&Json::Int(1_738_540_800)));
    assert_eq!(request.get("endTime"), Some(&Json::Int(1_738_627_200)));
}

#[test]
fn sample_uses_a_wider_window_and_one_descending_row() {
    let mut fake = query_script(r#"[[{"field": "@message", "value": "{\"a\": 1}"}]]"#);
    let args = snapshot_args(
        "sample",
        &format!(
            r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}", "status_timestamp": "2025-02-03T18:42:00Z"}}"#
        ),
    );
    let output =
        snapshot_tools::sample_snapshot(&mut fake, &mut VirtualPacer::default(), &args).unwrap();
    let request = &fake.calls[0].2;
    assert_eq!(
        request.get("startTime"),
        Some(&Json::Int(1_738_608_120 - 30))
    );
    assert_eq!(request.get("endTime"), Some(&Json::Int(1_738_608_120 + 90)));
    let query = request.get("queryString").and_then(Json::as_str).unwrap();
    assert!(
        query.ends_with("| sort @timestamp desc\n| limit 1"),
        "{query}"
    );
    assert!(!snapshot_tools::is_failure(&output), "{output}");
}

#[test]
fn snapshot_aws_failures_are_json_with_a_failing_inner_status() {
    let mut fake = Fake::new(|_, _, _| Err(service_error("ResourceNotFoundException", "no group")));
    let args = snapshot_args(
        "sample",
        &format!(
            r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}", "status_timestamp": "2025-02-03T18:42:00Z"}}"#
        ),
    );
    let output =
        snapshot_tools::sample_snapshot(&mut fake, &mut VirtualPacer::default(), &args).unwrap();
    assert!(output.trim_start().starts_with('{'), "{output}");
    assert!(output.contains("no group"), "{output}");
    assert!(snapshot_tools::is_failure(&output), "{output}");
}

#[test]
fn is_failure_classifies_by_structure() {
    for failing in [
        "ERROR: x",
        "  ERROR",
        r#"{"status": "ERROR"}"#,
        r#"{"status": "timeout"}"#,
        r#"{"status": " Polling Timeout "}"#,
        r#"{"status": "FAILED"}"#,
        r#"{"status": "Cancelled"}"#,
    ] {
        assert!(snapshot_tools::is_failure(failing), "{failing}");
    }
    for fine in [
        r#"{"status": "SUCCESS"}"#,
        r#"{"status": "Complete"}"#,
        "[]",
        "not json",
        r"{}",
    ] {
        assert!(!snapshot_tools::is_failure(fine), "{fine}");
    }
}

// ---- contract -----------------------------------------------------------

#[test]
fn contract_tables_cover_every_op_and_agree_with_the_argument_tables() {
    let contract = instrumentation_contract();
    let ops = contract.get("ops").expect("ops");
    for op in INSTRUMENTATION_OPS {
        let args = ops
            .get(op)
            .and_then(|o| o.get("args"))
            .expect("op in contract");
        let (_, spec) = INSTRUMENTATION_ARGUMENTS
            .iter()
            .find(|(name, _)| name == op)
            .expect("spec");
        let Json::Object(contract_args) = args else {
            panic!("args object")
        };
        let from_contract: Vec<&str> = contract_args
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let mut from_spec: Vec<&str> = spec.iter().map(|(name, _)| *name).collect();
        let mut sorted_contract = from_contract.clone();
        sorted_contract.sort_unstable();
        from_spec.sort_unstable();
        assert_eq!(sorted_contract, from_spec, "{op}");
    }
    let snapshots = snapshot_contract();
    for op in SNAPSHOT_OPS {
        assert!(
            snapshots.get("ops").and_then(|o| o.get(op)).is_some(),
            "{op}"
        );
    }
}

#[test]
fn contract_carries_required_defaults_and_hints() {
    let contract = instrumentation_contract();
    let args = |op: &str| {
        contract
            .get("ops")
            .and_then(|o| o.get(op))
            .and_then(|o| o.get("args"))
            .unwrap()
            .clone()
    };
    let create = args("create");
    assert_eq!(
        pyfmt::dumps(create.get("instrumentation_type").unwrap(), false),
        concat!(
            r#"{"required": true, "enum": ["BREAKPOINT", "PROBE"], "#,
            r#""note": "required on every op; must match how the breakpoint was created"}"#
        )
    );
    assert_eq!(
        pyfmt::dumps(create.get("description").unwrap(), false),
        r#"{"required": false, "default": "dynamic instrumentation"}"#
    );
    assert_eq!(
        pyfmt::dumps(create.get("ttl_hours").unwrap(), false),
        r#"{"required": false}"#
    );
    assert_eq!(
        pyfmt::dumps(args("list").get("max_results").unwrap(), false),
        r#"{"required": false, "default": 100}"#
    );
    assert_eq!(
        pyfmt::dumps(args("check-status").get("signal_type").unwrap(), false),
        r#"{"required": false, "default": "SNAPSHOT"}"#
    );
    let snapshots = snapshot_contract();
    let sample = snapshots
        .get("ops")
        .and_then(|o| o.get("sample"))
        .and_then(|o| o.get("args"))
        .unwrap();
    assert_eq!(
        pyfmt::dumps(sample.get("include_raw").unwrap(), false),
        r#"{"required": false, "default": false}"#
    );
}

// ---- malformed responses fail closed (#4749, #4750) ---------------------

const SCOPE: &str =
    r#"{"service": "svc", "environment": "prod", "instrumentation_type": "BREAKPOINT"}"#;

/// Every wrong JSON type of a present member, `null` included.
const WRONG_LISTS: [&str; 5] = ["null", "false", "0", r#""x""#, "{}"];

#[test]
fn a_malformed_listing_is_a_failure_never_an_empty_list() {
    let args = instrumentation_args("list", SCOPE);
    for bad in WRONG_LISTS {
        let mut fake = Fake::always(&format!(r#"{{"LatestConfigurations": {bad}}}"#));
        let result =
            crud_tools::list_instrumentations(&mut Gateway::new(&mut fake), &args).unwrap();
        assert!(!result.ok, "LatestConfigurations: {bad} -> {}", result.text);
        assert!(!result.text.contains("No active"), "{bad}: {}", result.text);
        assert!(
            result.text.contains("`LatestConfigurations`"),
            "{bad}: {}",
            result.text
        );
    }
    for bad in [
        r#"{"LatestConfigurations": [1]}"#,
        r#"{"LatestConfigurations": [{"Location": null}]}"#,
        r#"{"LatestConfigurations": [{"CaptureConfiguration": {"CodeCapture": []}}]}"#,
        r#"{"LatestConfigurations": [{"LocationHash": 5}]}"#,
        r#"{"LatestConfigurations": [], "NextToken": 7}"#,
    ] {
        let mut fake = Fake::always(bad);
        let result =
            crud_tools::list_instrumentations(&mut Gateway::new(&mut fake), &args).unwrap();
        assert!(!result.ok, "{bad} -> {}", result.text);
    }
    // Absence and an empty list are the genuine empty listing; a null token
    // is the explicit end-of-pages opt-in.
    for good in [
        "{}",
        r#"{"LatestConfigurations": []}"#,
        r#"{"LatestConfigurations": [], "NextToken": null}"#,
    ] {
        let mut fake = Fake::always(good);
        let result =
            crud_tools::list_instrumentations(&mut Gateway::new(&mut fake), &args).unwrap();
        assert!(result.ok, "{good} -> {}", result.text);
        assert!(
            result.text.starts_with("No active BREAKPOINT"),
            "{}",
            result.text
        );
    }
}

#[test]
fn malformed_events_are_a_failure_never_zero_events() {
    let base = r#""service": "svc", "environment": "prod", "instrumentation_type": "BREAKPOINT",
        "location_hash": "0123456789abcdef", "status": "READY""#;
    for bad in WRONG_LISTS
        .iter()
        .map(|bad| format!(r#"{{"Events": {bad}}}"#))
        .chain([
            r#"{"Events": [null]}"#.to_string(),
            r#"{"Events": [{"Time": null}]}"#.to_string(),
            r#"{"Events": [], "Status": false}"#.to_string(),
        ])
    {
        let mut fake = Fake::always(&bad);
        let result = get_status(&format!("{{{base}}}"), &mut fake);
        assert!(!result.ok, "{bad} -> {}", result.text);
        assert!(
            !result.text.contains("Events Returned"),
            "{bad}: {}",
            result.text
        );
        assert!(!result.text.contains("No READY"), "{bad}: {}", result.text);
    }
    let mut empty = Fake::always(r#"{"Events": []}"#);
    let result = get_status(&format!("{{{base}}}"), &mut empty);
    assert!(result.ok, "{}", result.text);
    assert!(
        result.text.contains("Events Returned: 0"),
        "{}",
        result.text
    );
}

#[test]
fn a_malformed_deletion_report_is_never_a_completed_batch() {
    let scope = instrumentation_args("batch-delete-by-scope", SCOPE);
    let arns = instrumentation_args(
        "batch-delete-by-arns",
        r#"{"resource_arns": ["arn:a"], "instrumentation_type": "BREAKPOINT"}"#,
    );
    let bad_reports = WRONG_LISTS
        .iter()
        .flat_map(|bad| {
            [
                format!(r#"{{"Errors": {bad}}}"#),
                format!(r#"{{"SuccessfulDeletions": {bad}}}"#),
            ]
        })
        .chain([
            r#"{"DeletedCount": "2"}"#.to_string(),
            r#"{"DeletedCount": null}"#.to_string(),
            r#"{"Errors": [{"Code": 5}]}"#.to_string(),
        ]);
    for bad in bad_reports {
        for by_scope in [true, false] {
            let mut fake = Fake::always(&bad);
            let result = if by_scope {
                crud_tools::batch_delete_by_scope(&mut Gateway::new(&mut fake), &scope)
            } else {
                crud_tools::batch_delete_by_arns(&mut Gateway::new(&mut fake), &arns)
            }
            .unwrap();
            assert!(!result.ok, "{bad} -> {}", result.text);
            assert!(
                !result.text.contains("BATCH DELETE COMPLETED"),
                "{bad}: {}",
                result.text
            );
        }
    }
}

#[test]
fn malformed_create_and_get_responses_fail_and_name_only_the_member() {
    let mut get = Fake::always(r#"{"Configuration": null}"#);
    let args = instrumentation_args(
        "get",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
            "location_hash": "0123456789abcdef"}"#,
    );
    let result = crud_tools::get_instrumentation(&mut Gateway::new(&mut get), &args).unwrap();
    assert!(!result.ok);
    assert!(
        result
            .text
            .contains("the member `Configuration` of the response is not an object (it is null)"),
        "{}",
        result.text
    );

    let created = create(
        r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT",
            "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "f", "ttl_hours": 2}"#,
        &mut Fake::always(r#"{"ARN": {"secret": "AWS-SECRET-SENTINEL"}}"#),
    );
    assert!(!created.ok, "{}", created.text);
    assert!(created.text.contains("`ARN`"), "{}", created.text);
    assert!(
        !created.text.contains("AWS-SECRET-SENTINEL"),
        "{}",
        created.text
    );
}

#[test]
fn malformed_query_results_are_an_error_never_no_snapshots() {
    let script = |poll: &'static str| {
        Fake::new(move |_, operation, _| {
            if operation == "StartQuery" {
                Ok(json(r#"{"queryId": "q"}"#))
            } else {
                Ok(json(poll))
            }
        })
    };
    let wrong = WRONG_LISTS.iter().flat_map(|bad| {
        [
            format!(r#"{{"status": "Complete", "results": {bad}}}"#),
            format!(r#"{{"status": "Complete", "results": [], "messages": {bad}}}"#),
        ]
    });
    for bad in wrong.chain([
        r#"{"status": null, "results": []}"#.to_string(),
        r#"{"status": "Complete", "results": [[{"field": null, "value": "v"}]]}"#.to_string(),
        r#"{"status": "Complete", "results": [[{"field": "@message", "value": 7}]]}"#.to_string(),
    ]) {
        let poll: &'static str = Box::leak(bad.clone().into_boxed_str());
        let result = query_with(&mut script(poll), 30);
        assert_eq!(result.status, "Error", "{bad}");
        assert_eq!(result.query_id, Json::string("q"), "{bad}");

        let args = snapshot_args(
            "sample",
            &format!(
                r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}", "status_timestamp": "2025-02-03T18:42:00Z"}}"#
            ),
        );
        let output =
            snapshot_tools::sample_snapshot(&mut script(poll), &mut VirtualPacer::default(), &args)
                .unwrap();
        assert!(snapshot_tools::is_failure(&output), "{bad}: {output}");
        assert!(!output.contains("NO_SNAPSHOTS_FOUND"), "{bad}: {output}");

        let output = search(
            &format!(
                r#"{{"service": "s", "environment": "e", "location_hash": "{HASH}", "status_timestamp": "2025-02-03T18:42:00Z"}}"#
            ),
            &mut script(poll),
        );
        assert!(snapshot_tools::is_failure(&output), "{bad}: {output}");
        assert!(!output.contains("NO_SNAPSHOTS_FOUND"), "{bad}: {output}");
    }
    // An absent or empty `results` is the genuine empty result.
    for good in [
        r#"{"status": "Complete"}"#,
        r#"{"status": "Complete", "results": [], "messages": []}"#,
    ] {
        let result = query_with(&mut script(good), 30);
        assert_eq!(result.status, "Complete", "{good}");
        assert!(result.results.is_empty());
    }
    // A wrong-typed queryId is an invalid response, not a missing one.
    let mut bad_id = Fake::always(r#"{"queryId": 5}"#);
    let result = query_with(&mut bad_id, 30);
    assert_eq!(result.status, "Error");
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("`queryId`") && e.contains("(it is integer)")),
        "{:?}",
        result.error
    );
    assert_eq!(bad_id.calls.len(), 1, "no poll after an invalid start");
}

// ---- the --out write never follows a swapped link (#4757) ---------------

#[cfg(unix)]
fn out_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolkit-cloudwatch-out-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[cfg(unix)]
#[test]
fn a_symlink_swapped_in_after_the_check_is_replaced_never_followed() {
    use std::os::unix::fs::PermissionsExt;

    for stage in [WriteStage::Checked, WriteStage::TempWritten] {
        let dir = out_dir(&format!("{stage:?}"));
        let out = dir.join("out.json");
        let victim = dir.join("victim.txt");
        std::fs::write(&out, "PLACEHOLDER\n").expect("placeholder");
        std::fs::write(&victim, "VICTIM_SENTINEL\n").expect("victim");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        // The interleaving of the review (#4757): the checked regular file is
        // replaced by a link to the victim before the write.
        write_owner_only_with(&out, "SNAPSHOT\n", &mut |at| {
            if at == stage {
                std::fs::rename(&out, dir.join("out.before")).expect("move away");
                std::os::unix::fs::symlink(&victim, &out).expect("plant link");
            }
        })
        .expect("written");

        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim"),
            "VICTIM_SENTINEL\n",
            "{stage:?}: the link target is never written"
        );
        assert_eq!(
            std::fs::metadata(&victim)
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "{stage:?}: the link target is never chmodded"
        );
        let written = std::fs::symlink_metadata(&out).expect("out");
        assert!(
            written.file_type().is_file(),
            "{stage:?}: the link was replaced"
        );
        assert_eq!(written.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&out).expect("out"), "SNAPSHOT\n");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{stage:?}: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(unix)]
#[test]
fn a_dangling_link_planted_at_a_new_path_is_replaced_and_its_target_never_created() {
    let dir = out_dir("dangling");
    let out = dir.join("new.json");
    let target = dir.join("created-through-link.txt");
    write_owner_only_with(&out, "SNAPSHOT\n", &mut |at| {
        if at == WriteStage::Checked {
            std::os::unix::fs::symlink(&target, &out).expect("plant link");
        }
    })
    .expect("written");
    assert!(!target.exists(), "the link target is never created");
    assert!(std::fs::symlink_metadata(&out)
        .expect("out")
        .file_type()
        .is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn pre_existing_links_and_non_files_are_refused_before_any_file_is_created() {
    let dir = out_dir("refused");
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, "untouched").expect("victim");
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&victim, &link).expect("link");
    let mut stages = Vec::new();
    let error =
        write_owner_only_with(&link, "SNAPSHOT", &mut |at| stages.push(at)).expect_err("refused");
    assert_eq!(error, "the path is a symbolic link");
    let error =
        write_owner_only_with(&dir, "SNAPSHOT", &mut |at| stages.push(at)).expect_err("refused");
    assert_eq!(error, "the path is not a regular file");
    assert!(stages.is_empty(), "refused before any file is created");
    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim"),
        "untouched"
    );
    assert_eq!(std::fs::read_dir(&dir).expect("list").count(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
