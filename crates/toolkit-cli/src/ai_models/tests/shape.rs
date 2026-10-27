//! The absent-versus-present contract of recorded input and AWS responses.
//!
//! An ABSENT optional member takes its default; a PRESENT member of the wrong
//! type, `null` included, is rejected: a recording with exit 1 and no success
//! output, a response as a malformed-response failure (Search then falls back
//! to the complete catalog). Nullable scalars (`FailureReason`) and null or
//! empty terminal pagination tokens stay accepted.

use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::{dumps, exec, j, path_text, recorded, summary, write, Spy, TempDir};
use crate::ai_models::client::{Operation, SageMakerClient};
use crate::ai_models::diagnostics::{collect_endpoint_diagnostics, TimeRange};
use crate::ai_models::hub::{
    customization_model_names, finetuning_techniques, get_deployable_models, list_hub_contents,
    ModelsError,
};
use crate::ai_models::recorded::RecordedClient;
use crate::ai_models::search::{select_public_hub_models, PUBLIC_HUB_NAME};
use crate::cloudwatch::timefmt::parse_iso_timestamp;

/// Values that are neither absent nor of the schema type of a list or object.
const WRONG_FOR_LIST: [&str; 5] = ["null", "\"x\"", "5", "true", "{}"];
const WRONG_FOR_STRING: [&str; 5] = ["null", "5", "true", "[]", "{}"];

fn rejected(text: &str) -> String {
    match RecordedClient::from_json(&j(text)) {
        Err(message) => message,
        Ok(_) => panic!("accepted: {text}"),
    }
}

// ---- recorded steps: every member, every step kind ----------------------------

#[test]
fn every_error_member_is_validated_for_every_kind() {
    for kind in [
        None,
        Some("service"),
        Some("connect"),
        Some("no_credentials"),
    ] {
        for member in ["kind", "code", "message"] {
            for wrong in WRONG_FOR_STRING {
                let mut fields: Vec<String> = Vec::new();
                if let Some(kind) = kind {
                    if member != "kind" {
                        fields.push(format!("\"kind\": \"{kind}\""));
                    }
                }
                fields.push(format!("\"{member}\": {wrong}"));
                let text = format!(
                    "{{\"calls\": {{\"Search\": [{{\"error\": {{{}}}}}]}}}}",
                    fields.join(", ")
                );
                let message = rejected(&text);
                assert!(
                    message
                        .starts_with(&format!("calls.Search[0].error.{member} must be a string")),
                    "{text}: {message}"
                );
            }
        }
        for wrong in ["null", "\"400\"", "99", "600", "4.5", "[]", "true"] {
            let kind_member = kind.map_or(String::new(), |kind| format!("\"kind\": \"{kind}\", "));
            let text = format!(
                "{{\"calls\": {{\"Search\": [{{\"error\": {{{kind_member}\"status\": {wrong}}}}}]}}}}"
            );
            let message = rejected(&text);
            assert!(
                message.starts_with("calls.Search[0].error.status must be an HTTP status code"),
                "{text}: {message}"
            );
        }
    }
}

#[test]
fn the_reason_names_the_position_and_never_the_value() {
    let message =
        rejected(r#"{"calls": {"Search": [{"error": {"code": ["FAKE-SECRET-VALUE"]}}]}}"#);
    assert_eq!(
        message,
        "calls.Search[0].error.code must be a string when present (it is array)"
    );
    let message =
        rejected(r#"{"calls": {"Search": [{"request": "FAKE-SECRET-VALUE", "response": {}}]}}"#);
    assert_eq!(
        message,
        "calls.Search[0].request must be an object when present (it is string)"
    );
}

#[test]
fn a_step_request_is_an_object_when_present_for_either_outcome() {
    for outcome in [r#""response": {}"#, r#""error": {}"#] {
        for wrong in ["null", "\"x\"", "5", "true", "[]"] {
            let text = format!(
                "{{\"calls\": {{\"DescribeEndpoint\": [{{\"request\": {wrong}, {outcome}}}]}}}}"
            );
            let message = rejected(&text);
            assert!(
                message.starts_with("calls.DescribeEndpoint[0].request must be an object"),
                "{text}: {message}"
            );
        }
    }
}

#[test]
fn omitted_members_keep_their_defaults() {
    let mut client = recorded(
        r#"{"calls": {"Search": [{"error": {}}, {"error": {"kind": "service"}},
                                 {"error": {"kind": "connect"}},
                                 {"error": {"kind": "no_credentials", "code": "X", "status": 500}},
                                 {"response": {}}]}}"#,
    );
    let request = Json::Object(Vec::new());
    for _ in 0..2 {
        let AwsError::Service(service) = client.call(Operation::Search, &request).unwrap_err()
        else {
            panic!("a service error")
        };
        assert_eq!(
            (service.status(), service.code(), service.message()),
            (400, None, "")
        );
    }
    assert!(matches!(
        client.call(Operation::Search, &request).unwrap_err(),
        AwsError::Connect { .. }
    ));
    assert_eq!(
        client.call(Operation::Search, &request).unwrap_err(),
        AwsError::NoCredentials
    );
    assert_eq!(
        client.call(Operation::Search, &request).unwrap(),
        Json::Object(Vec::new())
    );
}

// ---- the expected request guards error steps as it guards responses -----------

#[test]
fn an_error_step_checks_its_request_before_it_is_replayed() {
    let document = r#"{"calls": {"DescribeEndpoint": [
        {"request": {"EndpointName": "a"}, "error": {"code": "AccessDenied", "message": "denied"}},
        {"request": {"EndpointName": "a"}, "error": {"kind": "no_credentials"}},
        {"request": {"EndpointName": "a"}, "error": {"kind": "connect", "message": "down"}}]}}"#;
    // A mismatch is an invalid-response data error for every kind, never the
    // recorded outcome.
    let mut client = recorded(document);
    for _ in 0..3 {
        let error = client
            .call(Operation::DescribeEndpoint, &j(r#"{"EndpointName": "b"}"#))
            .expect_err("mismatch");
        assert_eq!(
            error.to_string(),
            "invalid response: the DescribeEndpoint request differs from the recorded request"
        );
        assert!(matches!(error, AwsError::InvalidResponse { .. }));
    }
    // A match replays the recorded outcome.
    let mut client = recorded(document);
    let request = j(r#"{"EndpointName": "a"}"#);
    let AwsError::Service(service) = client
        .call(Operation::DescribeEndpoint, &request)
        .unwrap_err()
    else {
        panic!("service error")
    };
    assert_eq!(service.code(), Some("AccessDenied"));
    assert_eq!(
        client
            .call(Operation::DescribeEndpoint, &request)
            .unwrap_err(),
        AwsError::NoCredentials
    );
    assert!(matches!(
        client
            .call(Operation::DescribeEndpoint, &request)
            .unwrap_err(),
        AwsError::Connect { .. }
    ));
}

#[test]
fn a_mismatched_error_step_is_not_a_tolerated_diagnostics_error() {
    let dir = TempDir::new("ai-models-shape-mismatch");
    write(
        &dir,
        "r.json",
        r#"{"calls": {"DescribeEndpoint": [{"request": {"EndpointName": "different-endpoint"},
            "error": {"code": "AccessDeniedException", "message": "denied"}}],
            "GetMetricData": [{"response": {}}], "FilterLogEvents": [{"response": {}}]}}"#,
    );
    let path = path_text(&dir.join("r.json")).to_string();
    let (status, out, err) = diagnostics_cli(&path);
    assert_eq!((status, out.as_str()), (1, ""), "{err}");
    assert!(
        err.contains("the DescribeEndpoint request differs from the recorded request"),
        "{err}"
    );
}

// ---- the review reproductions through the real router -------------------------

fn diagnostics_cli(path: &str) -> (u8, String, String) {
    exec(&[
        "ai-models",
        "endpoint-diagnostics",
        "my-endpoint",
        "us-east-1",
        "--now",
        "2026-10-27T12:00:00Z",
        "--recorded",
        path,
    ])
}

fn recording(label: &str, text: &str) -> (TempDir, String) {
    let dir = TempDir::new(label);
    write(&dir, "r.json", text);
    let path = path_text(&dir.join("r.json")).to_string();
    (dir, path)
}

#[test]
fn a_null_error_object_is_rejected_with_exit_one_and_no_success_output() {
    for member in ["kind", "code", "message", "status"] {
        let (_dir, path) = recording(
            "ai-models-shape-null-error",
            &format!(
                "{{\"calls\": {{\"DescribeEndpoint\": [{{\"error\": {{\"{member}\": null}}}}],
                  \"GetMetricData\": [{{\"response\": {{}}}}], \"FilterLogEvents\": [{{\"response\": {{}}}}]}}}}"
            ),
        );
        let (status, out, err) = diagnostics_cli(&path);
        assert_eq!((status, out.as_str()), (1, ""), "{member}: {err}");
        assert!(
            err.contains(&format!("calls.DescribeEndpoint[0].error.{member} must be")),
            "{member}: {err}"
        );
    }
    // The review's all-null error.
    let (_dir, path) = recording(
        "ai-models-shape-null-error-all",
        r#"{"calls":{"DescribeEndpoint":[{"error":{"kind":null,"code":null,"message":null}}],
            "GetMetricData":[{"response":{}}],"FilterLogEvents":[{"response":{}}]}}"#,
    );
    let (status, out, _) = diagnostics_cli(&path);
    assert_eq!((status, out.as_str()), (1, ""));
}

#[test]
fn a_null_search_results_list_falls_back_to_the_complete_catalog() {
    let (_dir, path) = recording(
        "ai-models-shape-null-search",
        r#"{"calls":{"ListHubContents":[{"response":{"HubContentSummaries":[
            {"HubContentName":"known-model","HubContentSearchKeywords":["@task:chat"]}]}}],
            "Search":[{"response":{"Results":null}}]}}"#,
    );
    let (status, out, err) = exec(&[
        "ai-models",
        "search",
        "SageMakerPublicHub",
        "task:chat",
        "--recorded",
        &path,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let printed = j(&out);
    assert_eq!(
        printed.get("retrieval_backend").and_then(Json::as_str),
        Some("list_fallback")
    );
    assert_eq!(
        dumps(
            printed
                .get("fallback_reason")
                .expect("reason")
                .get("type")
                .expect("type")
        ),
        "\"SearchResponseError\""
    );
    assert!(out.contains("known-model"), "{out}");
}

#[test]
fn null_response_lists_fail_the_listing_commands_without_success_output() {
    for (label, command, recording_text) in [
        (
            "null-summaries",
            vec!["list-models", "my-hub"],
            r#"{"calls":{"ListHubContents":[{"response":{"HubContentSummaries":null}}]}}"#,
        ),
        (
            "null-keywords",
            vec!["model-names", "my-hub"],
            r#"{"calls":{"ListHubContents":[{"response":{"HubContentSummaries":
                [{"HubContentName":"m","HubContentSearchKeywords":null}]}}]}}"#,
        ),
        (
            "null-recipe-keywords",
            vec!["recipes", "m", "my-hub"],
            r#"{"calls":{"DescribeHubContent":[{"response":{"HubContentSearchKeywords":null}}]}}"#,
        ),
    ] {
        let (_dir, path) = recording(&format!("ai-models-shape-{label}"), recording_text);
        let mut args = vec!["ai-models"];
        args.extend(command);
        args.extend(["--recorded", path.as_str()]);
        let (status, out, err) = exec(&args);
        assert_eq!((status, out.as_str()), (1, ""), "{label}: {err}");
        assert!(err.contains("when present (it is null)"), "{label}: {err}");
    }
}

#[test]
fn null_diagnostics_lists_fail_the_run_without_success_output() {
    for (member, text) in [
        (
            "ProductionVariants",
            r#"{"calls":{"DescribeEndpoint":[{"response":{"ProductionVariants":null}}]}}"#,
        ),
        (
            "MetricDataResults",
            r#"{"calls":{"DescribeEndpoint":[{"response":{}}],
                "GetMetricData":[{"response":{"MetricDataResults":null}}]}}"#,
        ),
        (
            "Values",
            r#"{"calls":{"DescribeEndpoint":[{"response":{}}],
                "GetMetricData":[{"response":{"MetricDataResults":[{"Id":"i","Values":null}]}}]}}"#,
        ),
        (
            "events",
            r#"{"calls":{"DescribeEndpoint":[{"response":{}}],"GetMetricData":[{"response":{}}],
                "FilterLogEvents":[{"response":{"events":null}}]}}"#,
        ),
    ] {
        let (_dir, path) = recording("ai-models-shape-diag-null", text);
        let (status, out, err) = diagnostics_cli(&path);
        assert_eq!((status, out.as_str()), (1, ""), "{member}: {err}");
        assert!(
            err.contains(&format!("{member} must be")) && err.contains("(it is null)"),
            "{member}: {err}"
        );
    }
}

// ---- the sweep: every response array/object field, every wrong type -----------

#[allow(clippy::unnecessary_wraps)]
fn ok(text: &str) -> Result<Json, AwsError> {
    Ok(j(text))
}

fn assert_response_error(error: &AwsError, member: &str) {
    assert!(
        matches!(error, AwsError::InvalidResponse { .. }),
        "{member}: {error:?}"
    );
    let text = error.to_string();
    assert!(text.contains(&format!("{member} must be")), "{text}");
    assert!(text.contains("when present (it is "), "{text}");
}

#[test]
fn hub_summaries_reject_every_wrong_type_and_non_object_item() {
    for wrong in WRONG_FOR_LIST.iter().chain(&["[1]", "[null]", "[[]]"]) {
        let mut spy = Spy::new(vec![ok(&format!("{{\"HubContentSummaries\": {wrong}}}"))]);
        let error = list_hub_contents(&mut spy, "h").expect_err(wrong);
        assert_response_error(&error, "HubContentSummaries");
    }
}

#[test]
fn keywords_reject_every_wrong_type_at_every_consumer() {
    for wrong in WRONG_FOR_LIST.iter().chain(&["[1]", "[null]", "[[\"a\"]]"]) {
        let record =
            format!("{{\"HubContentName\": \"m\", \"HubContentSearchKeywords\": {wrong}}}");
        let list = format!("{{\"HubContentSummaries\": [{record}]}}");
        let mut spy = Spy::new(vec![ok(&list)]);
        match get_deployable_models(&mut spy, "h") {
            Err(ModelsError::Data(message)) => {
                assert!(
                    message.starts_with("model 0: HubContentSearchKeywords must be a list"),
                    "{wrong}: {message}"
                );
            }
            other => panic!("{wrong}: {other:?}"),
        }
        let mut spy = Spy::new(vec![ok(&list)]);
        assert!(
            matches!(
                customization_model_names(&mut spy, "h"),
                Err(ModelsError::Data(_))
            ),
            "{wrong}"
        );
        let mut spy = Spy::new(vec![ok(&record)]);
        assert!(
            matches!(
                finetuning_techniques(&mut spy, "h", "m"),
                Err(ModelsError::Data(_))
            ),
            "{wrong}"
        );
    }
}

fn search_fallback(answers: Vec<Result<Json, AwsError>>) -> (String, String, Vec<String>) {
    let catalog = vec![crate::ai_models::normalize::normalize_hub_content(&summary(
        "known-model",
        &["@task:chat"],
        None,
    ))
    .expect("normalizes")];
    let constraints = crate::ai_models::search::parse_constraints(&["task:chat"]).expect("parses");
    let mut spy = Spy::new(answers);
    let selection = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &constraints,
        &catalog,
        &mut spy,
        "snapshot",
    )
    .expect("selects");
    let names = selection
        .models
        .iter()
        .filter_map(|m| m.get("name").and_then(Json::as_str))
        .map(str::to_string)
        .collect();
    let (kind, message) = selection.fallback_reason.expect("falls back");
    (kind.to_string(), message, names)
}

#[test]
fn search_results_and_hub_content_reject_every_wrong_type_by_falling_back() {
    for wrong in WRONG_FOR_LIST {
        let (kind, message, names) =
            search_fallback(vec![ok(&format!("{{\"Results\": {wrong}}}"))]);
        assert_eq!(kind, "SearchResponseError", "{wrong}");
        assert!(
            message.contains("Results must be a list when present"),
            "{message}"
        );
        assert_eq!(names, ["known-model"], "{wrong}");
    }
    // Results items need an object HubContent; keywords inside obey the same rule.
    for wrong in [
        "[1]",
        "[null]",
        "[{}]",
        "[{\"HubContent\": null}]",
        "[{\"HubContent\": 5}]",
    ] {
        let (kind, _, names) = search_fallback(vec![ok(&format!("{{\"Results\": {wrong}}}"))]);
        assert_eq!(kind, "SearchResponseError", "{wrong}");
        assert_eq!(names, ["known-model"]);
    }
    for wrong in WRONG_FOR_LIST.iter().chain(&["[1]", "[null]"]) {
        let result = format!(
            "{{\"Results\": [{{\"HubContent\": {{\"HubContentName\": \"x\", \"HubContentSearchKeywords\": {wrong}}}}}]}}"
        );
        let (kind, message, names) = search_fallback(vec![ok(&result)]);
        assert_eq!(kind, "SearchResponseError", "{wrong}");
        assert!(
            message.contains("HubContentSearchKeywords must be a list"),
            "{message}"
        );
        assert_eq!(names, ["known-model"]);
    }
}

fn diagnostics(answers: Vec<Result<Json, AwsError>>) -> Result<Json, ModelsError> {
    let mut spy = Spy::new(answers);
    collect_endpoint_diagnostics(
        &mut spy,
        "my-endpoint",
        TimeRange::Last(5),
        parse_iso_timestamp("2024-03-15T15:00:00Z").expect("timestamp"),
    )
}

fn data_error(result: Result<Json, ModelsError>, member: &str) {
    match result {
        Err(ModelsError::Data(message)) => {
            assert!(message.contains(&format!("{member} must be")), "{message}");
            assert!(message.contains("when present (it is "), "{message}");
        }
        other => panic!("{member}: {other:?}"),
    }
}

#[test]
fn diagnostics_reject_every_wrong_typed_list_and_object_member() {
    for wrong in WRONG_FOR_LIST.iter().chain(&["[1]", "[null]", "[[]]"]) {
        data_error(
            diagnostics(vec![ok(&format!("{{\"ProductionVariants\": {wrong}}}"))]),
            "ProductionVariants",
        );
        data_error(
            diagnostics(vec![
                ok("{}"),
                ok(&format!("{{\"MetricDataResults\": {wrong}}}")),
            ]),
            "MetricDataResults",
        );
        data_error(
            diagnostics(vec![
                ok("{}"),
                ok("{}"),
                ok(&format!("{{\"events\": {wrong}}}")),
            ]),
            "events",
        );
    }
    for wrong in WRONG_FOR_LIST {
        data_error(
            diagnostics(vec![
                ok("{}"),
                ok(&format!(
                    "{{\"MetricDataResults\": [{{\"Id\": \"i\", \"Values\": {wrong}}}]}}"
                )),
            ]),
            "Values",
        );
    }
}

// ---- a later page is held to the same rule; nothing partial is returned -------

#[test]
fn a_malformed_later_listing_page_fails_the_whole_listing() {
    for page in [
        r#"{"HubContentSummaries": null}"#,
        r#"{"HubContentSummaries": "x"}"#,
        r#"{"HubContentSummaries": [{"HubContentName": "b", "HubContentSearchKeywords": null}]}"#,
    ] {
        let mut spy = Spy::new(vec![
            ok(r#"{"HubContentSummaries": [{"HubContentName": "a"}], "NextToken": "t"}"#),
            ok(page),
        ]);
        let result = get_deployable_models(&mut spy, "h");
        assert!(result.is_err(), "{page}: {result:?}");
        assert_eq!(spy.seen.len(), 2, "{page}");
    }
}

#[test]
fn a_malformed_later_search_page_falls_back_to_the_whole_catalog() {
    for page in [r#"{"Results": null}"#, r#"{"Results": {}}"#] {
        let (kind, _, names) = search_fallback(vec![
            ok(r#"{"Results": [{"HubContent": {"HubContentName": "partial",
                "HubContentSearchKeywords": ["@task:chat"]}}], "NextToken": "t"}"#),
            ok(page),
        ]);
        assert_eq!(kind, "SearchResponseError", "{page}");
        // No partial first page: the complete catalog answers.
        assert_eq!(names, ["known-model"], "{page}");
    }
}

// ---- positive controls --------------------------------------------------------

#[test]
fn omitted_members_empty_lists_and_nullable_scalars_are_accepted() {
    // Absent list members and empty lists: an authoritative empty listing.
    for page in [r"{}", r#"{"HubContentSummaries": []}"#] {
        let mut spy = Spy::new(vec![ok(page)]);
        assert_eq!(list_hub_contents(&mut spy, "h").expect("empty"), Vec::new());
    }
    // A record without keywords, and with an empty list, is a model without any.
    let mut spy = Spy::new(vec![ok(
        r#"{"HubContentSummaries": [{"HubContentName": "a"},
            {"HubContentName": "b", "HubContentSearchKeywords": []}]}"#,
    )]);
    let models = get_deployable_models(&mut spy, "h").expect("models");
    assert_eq!(models.len(), 2);
    // A terminal NextToken may be absent, null or empty.
    for page in [
        r#"{"HubContentSummaries": []}"#,
        r#"{"HubContentSummaries": [], "NextToken": null}"#,
        r#"{"HubContentSummaries": [], "NextToken": ""}"#,
    ] {
        let mut spy = Spy::new(vec![ok(page)]);
        assert!(list_hub_contents(&mut spy, "h").is_ok(), "{page}");
        assert_eq!(spy.seen.len(), 1, "{page}");
    }
    // An empty Search is authoritative; an absent Results is empty too.
    for page in [r#"{"Results": []}"#, r"{}"] {
        let catalog = vec![crate::ai_models::normalize::normalize_hub_content(&summary(
            "known-model",
            &["@task:chat"],
            None,
        ))
        .expect("normalizes")];
        let constraints =
            crate::ai_models::search::parse_constraints(&["task:chat"]).expect("parses");
        let mut spy = Spy::new(vec![ok(page)]);
        let selection = select_public_hub_models(
            PUBLIC_HUB_NAME,
            &constraints,
            &catalog,
            &mut spy,
            "snapshot",
        )
        .expect("selects");
        assert_eq!(selection.retrieval_backend, "search", "{page}");
        assert!(selection.models.is_empty() && selection.fallback_reason.is_none());
    }
    // Diagnostics: omitted lists, empty lists and a null FailureReason.
    let result = diagnostics(vec![
        ok(r#"{"EndpointStatus": "Failed", "FailureReason": null, "ProductionVariants": []}"#),
        ok(r#"{"MetricDataResults": [{"Id": "invocations"}, {"Id": "cpu", "Values": []}]}"#),
        ok(r#"{"events": []}"#),
    ])
    .expect("collects");
    assert_eq!(
        dumps(result.get("metrics").expect("metrics")),
        r#"{"invocations": [], "cpu": []}"#
    );
    assert!(dumps(&result).contains(r#""failure_reason": null"#));
    let result = diagnostics(vec![ok("{}"), ok("{}"), ok("{}")]).expect("collects");
    assert_eq!(dumps(result.get("recent_logs").expect("logs")), "[]");
}
