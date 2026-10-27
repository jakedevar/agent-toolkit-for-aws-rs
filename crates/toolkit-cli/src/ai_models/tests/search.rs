use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::{catalog, dumps, j, summary, Spy};
use crate::ai_models::client::{NoLiveClient, Operation};
use crate::ai_models::filter::Constraints;
use crate::ai_models::search::{
    build_search_expression, parse_constraints, restore_catalog_order, search_request,
    select_public_hub_models, validate_search_expression, SearchAdapterError, PUBLIC_HUB_NAME,
};

fn parsed(words: &[&str]) -> Constraints {
    parse_constraints(words).expect("parses")
}

fn expression(words: &[&str]) -> String {
    dumps(&build_search_expression(PUBLIC_HUB_NAME, &parsed(words), &catalog()).expect("builds"))
}

fn refused(words: &[&str]) -> String {
    match build_search_expression(PUBLIC_HUB_NAME, &parsed(words), &catalog()) {
        Err(SearchAdapterError::Constraint(message)) => message,
        other => panic!("expected a constraint error, got {other:?}"),
    }
}

fn service_error(code: &str, message: &str) -> AwsError {
    AwsError::service(ServiceError::new(
        "POST",
        "/",
        400,
        Some(code),
        message,
        None,
    ))
}

fn hub_content(name: &str, keywords: &[&str]) -> Json {
    Json::object(vec![("HubContent", summary(name, keywords, None))])
}

#[test]
fn constraints_are_trimmed_lowered_deduplicated_and_keyed_in_order() {
    let built = parsed(&[
        " task :Text Generation ",
        "task:text generation",
        "size:<=10B",
    ]);
    assert_eq!(
        dumps(&crate::ai_models::filter::constraints_json(&built)),
        r#"{"task": ["text generation"], "size": ["<=10b"]}"#
    );
}

#[test]
fn bad_constraint_words_are_refused_with_quoted_text() {
    let message = |words: &[&str]| match parse_constraints(words) {
        Err(SearchAdapterError::Constraint(message)) => message,
        other => panic!("expected a constraint error, got {other:?}"),
    };
    assert_eq!(
        message(&["nocolon"]),
        "Malformed constraint \"nocolon\"; expected key:value"
    );
    assert_eq!(
        message(&["task:  "]),
        "Constraint \"task\" contains an empty or non-string value"
    );
    assert_eq!(
        message(&["color:red"]),
        concat!(
            "Unsupported constraint key \"color\". Supported keys: task, data_type, ",
            "input_modality, output_modality, license, size, context_window, language, ",
            "model_type, provider, bedrock, customization, name"
        )
    );
    let hostile = message(&["a\u{1b}[31m\u{202e}\nb:x"]);
    assert!(
        !hostile.contains('\u{1b}') && !hostile.contains('\u{202e}') && !hostile.contains('\n')
    );
}

#[test]
fn a_combined_query_resolves_every_constraint_against_the_catalog() {
    assert_eq!(
        expression(&[
            "task:text generation",
            "license:apache",
            "size:<=10b",
            "model_type:llm",
            "bedrock:true"
        ]),
        concat!(
            r#"{"Operator": "And", "Filters": ["#,
            r#"{"Name": "HubName", "Operator": "Equals", "Value": "SageMakerPublicHub"}, "#,
            r#"{"Name": "HubContentType", "Operator": "Equals", "Value": "Model"}, "#,
            r#"{"Name": "HubContentSearchKeywords", "Operator": "Equals", "Value": "@task:text generation"}, "#,
            // Both spellings of the license are sent, space before hyphen.
            r#"{"Name": "HubContentSearchKeywords", "Operator": "In", "Value": "@license:Apache 2.0,@license:Apache-2.0"}, "#,
            // The bucket expands to every raw label in it, in every casing.
            r#"{"Name": "Metadata.ModelSize", "Operator": "In", "Value": "7b,7B,8B,8b,<1b,<1B"}, "#,
            r#"{"Name": "Metadata.ModelTypes", "Operator": "Equals", "Value": "LLM"}, "#,
            r#"{"Name": "Metadata.Capabilities", "Operator": "Equals", "Value": "BEDROCK_CONSOLE"}]}"#
        )
    );
}

#[test]
fn free_text_metadata_is_sent_in_every_casing_and_enums_in_one() {
    assert_eq!(
        expression(&["context_window:8k"]),
        concat!(
            r#"{"Operator": "And", "Filters": ["#,
            r#"{"Name": "HubName", "Operator": "Equals", "Value": "SageMakerPublicHub"}, "#,
            r#"{"Name": "HubContentType", "Operator": "Equals", "Value": "Model"}, "#,
            r#"{"Name": "Metadata.ContextWindow", "Operator": "In", "Value": "8k,8K"}]}"#
        )
    );
    assert!(expression(&["data_type:text"]).contains(r#""Value": "text,Text,TEXT""#));
    assert!(expression(&["input_modality:text"]).contains(r#""Name": "Metadata.InputModalities""#));
    assert!(
        expression(&["output_modality:text"]).contains(r#""Name": "Metadata.OutputModalities""#)
    );
    assert!(expression(&["model_type:embedding"]).contains(r#""Value": "EMBEDDING""#));
}

#[test]
fn customization_is_a_keyword_tag_and_language_a_substring() {
    assert!(expression(&["customization:true"])
        .contains(r#"{"Name": "HubContentSearchKeywords", "Operator": "Equals", "Value": "@capability:customization"}"#));
    assert!(expression(&["language:e"]).contains(r#""Value": "@language:de,@language:en""#));
}

#[test]
fn a_single_name_uses_the_services_contains_with_the_catalog_casing() {
    assert!(expression(&["name:MISTRAL"])
        .contains(r#"{"Name": "HubContentName", "Operator": "Contains", "Value": "Mistral"}"#));
    assert!(expression(&["name:7b"]).contains(r#""Operator": "Contains", "Value": "7B""#));
    assert!(expression(&["name:-"]).contains(r#""Operator": "Contains", "Value": "-""#));
}

#[test]
fn several_names_or_mixed_casing_use_exact_names() {
    assert!(expression(&["name:mistral", "name:tiny"])
        .contains(r#""Operator": "In", "Value": "Mistral-7B,tiny-embedder""#));
    let mixed: Vec<Json> = ["Foo-1", "foo-2"]
        .iter()
        .map(|name| {
            crate::ai_models::normalize::normalize_hub_content(&summary(name, &[], None)).unwrap()
        })
        .collect();
    let built =
        build_search_expression(PUBLIC_HUB_NAME, &parsed(&["name:foo"]), &mixed).expect("builds");
    assert!(dumps(&built).contains(r#""Operator": "In", "Value": "Foo-1,foo-2""#));
}

#[test]
fn unresolvable_and_unsupported_requests_fail_before_any_call() {
    assert_eq!(
        refused(&["task:nonexistent"]),
        "Constraint \"task\" values [\"nonexistent\"] do not match any current public-Hub catalog value"
    );
    assert_eq!(
        refused(&["size:>100b"]),
        "Size values [\">100b\"] do not match any current public-Hub raw model-size value"
    );
    assert_eq!(
        refused(&["name:zzz"]),
        "Model-name values [\"zzz\"] do not match any current public-Hub model"
    );
    for key in ["bedrock", "customization"] {
        assert_eq!(
            refused(&[format!("{key}:false").as_str()]),
            format!(
                "The Search adapter supports {key}:true only; absence-based false matching is not yet verified"
            )
        );
    }
    assert_eq!(
        refused(&["bedrock:true", "bedrock:false"]),
        "The Search adapter supports bedrock:true only; absence-based false matching is not yet verified"
    );
    let error =
        build_search_expression("MyPrivateHub", &parsed(&[]), &catalog()).expect_err("refused");
    assert_eq!(
        error.message(),
        "The Search adapter supports SageMakerPublicHub only; private Hubs must continue using ListHubContents"
    );
}

#[test]
fn no_constraint_queries_the_whole_public_hub() {
    assert_eq!(
        expression(&[]),
        concat!(
            r#"{"Operator": "And", "Filters": ["#,
            r#"{"Name": "HubName", "Operator": "Equals", "Value": "SageMakerPublicHub"}, "#,
            r#"{"Name": "HubContentType", "Operator": "Equals", "Value": "Model"}]}"#
        )
    );
}

#[test]
fn values_with_commas_get_their_own_equals_filter() {
    let providers: Vec<Json> = ["p,b", "pa", "pc"]
        .iter()
        .map(|p| {
            let tag = format!("@provider:{p}");
            crate::ai_models::normalize::normalize_hub_content(&summary(p, &[tag.as_str()], None))
                .unwrap()
        })
        .collect();
    let built = build_search_expression(PUBLIC_HUB_NAME, &parsed(&["provider:p"]), &providers)
        .expect("builds");
    assert_eq!(
        dumps(built.get("SubExpressions").expect("subexpressions")),
        concat!(
            r#"[{"Operator": "Or", "Filters": ["#,
            r#"{"Name": "HubContentSearchKeywords", "Operator": "Equals", "Value": "@provider:p,b"}, "#,
            r#"{"Name": "HubContentSearchKeywords", "Operator": "In", "Value": "@provider:pa,@provider:pc"}]}]"#
        )
    );
}

fn crowded_catalog(count: usize) -> Vec<Json> {
    (0..count)
        .map(|i| {
            let provider = format!("prov{i:03}{}", "x".repeat(92));
            let tag = format!("@provider:{provider}");
            crate::ai_models::normalize::normalize_hub_content(&summary(
                &format!("m{i}"),
                &[tag.as_str()],
                None,
            ))
            .unwrap()
        })
        .collect()
}

fn value_lengths(expression: &Json, lengths: &mut Vec<usize>, widths: &mut Vec<usize>) {
    for collection in ["Filters", "SubExpressions"] {
        if let Some(Json::Array(items)) = expression.get(collection) {
            widths.push(items.len());
            for item in items {
                if let Some(Json::String(value)) = item.get("Value") {
                    lengths.push(value.chars().count());
                }
                value_lengths(item, lengths, widths);
            }
        }
    }
}

#[test]
fn many_long_alternatives_are_packed_and_nested_within_the_service_limits() {
    // 250 tags of 109 characters: nine fit one In value of at most 1024
    // characters, so 28 filters need two nested levels of at most 20.
    let catalog = crowded_catalog(250);
    let built = build_search_expression(PUBLIC_HUB_NAME, &parsed(&["provider:prov"]), &catalog)
        .expect("builds");
    let (mut lengths, mut widths) = (Vec::new(), Vec::new());
    value_lengths(&built, &mut lengths, &mut widths);
    assert!(lengths.iter().all(|length| *length <= 1024), "{lengths:?}");
    assert!(widths.iter().all(|width| *width <= 20), "{widths:?}");
    let Some(Json::Array(subs)) = built.get("SubExpressions") else {
        panic!("subexpressions")
    };
    assert_eq!(subs.len(), 1);
    let Some(Json::Array(level)) = subs[0].get("SubExpressions") else {
        panic!("nested level: {}", dumps(&subs[0]))
    };
    assert_eq!(level.len(), 2);
    let Some(Json::Array(first)) = level[0].get("Filters") else {
        panic!("filters")
    };
    assert_eq!(first.len(), 20);
    // Every one of the 250 values is sent exactly once.
    let sent: usize = lengths.len();
    assert!(sent >= 28);
    validate_search_expression(&built, "SearchExpression").expect("within limits");
}

#[test]
fn the_validator_enforces_the_service_limits() {
    let check = |text: &str| match validate_search_expression(&j(text), "E") {
        Err(SearchAdapterError::Constraint(message)) => message,
        other => panic!("expected a constraint error, got {other:?}"),
    };
    assert_eq!(check("[]"), "E must be an object");
    assert_eq!(check("{}"), "E must contain at least one condition");
    assert_eq!(
        check(r#"{"Filters": []}"#),
        "E.Filters must be a non-empty list"
    );
    assert_eq!(
        check(r#"{"Filters": [{"Name": "a", "Value": ""}]}"#),
        "E.Filters[0].Value must be a non-empty string"
    );
    let long = "x".repeat(1025);
    assert_eq!(
        check(&format!(
            r#"{{"Filters": [{{"Name": "a", "Value": "{long}"}}]}}"#
        )),
        "E.Filters[0].Value exceeds 1024 characters"
    );
    let many = vec![r#"{"Name": "a", "Value": "v"}"#; 21].join(",");
    assert_eq!(
        check(&format!(r#"{{"Filters": [{many}]}}"#)),
        "E.Filters has 21 elements; the maximum is 20"
    );
    assert_eq!(
        check(r#"{"SubExpressions": [{"Filters": []}]}"#),
        "E.SubExpressions[0].Filters must be a non-empty list"
    );
    assert_eq!(
        check(r#"{"NestedFilters": [{"Filters": [{"Name": "a", "Value": ""}]}]}"#),
        "E.NestedFilters[0].Filters[0].Value must be a non-empty string"
    );
}

#[test]
fn a_search_pages_through_results_and_keeps_each_name_once() {
    let page1 = Json::object(vec![
        (
            "Results",
            Json::Array(vec![hub_content("Mistral-7B", &["@task:text generation"])]),
        ),
        ("NextToken", Json::string("t1")),
    ]);
    let page2 = Json::object(vec![(
        "Results",
        Json::Array(vec![
            hub_content("Mistral-7B", &["@task:text generation"]),
            hub_content("llama-3-8b", &["@task:text generation"]),
        ]),
    )]);
    let mut client = Spy::new(vec![Ok(page1), Ok(page2)]);
    let constraints = parsed(&["task:text generation"]);
    let selection = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &constraints,
        &catalog(),
        &mut client,
        "snapshot",
    )
    .expect("selects");
    let expression =
        build_search_expression(PUBLIC_HUB_NAME, &constraints, &catalog()).expect("builds");
    assert_eq!(
        client.seen,
        vec![
            (Operation::Search, search_request(&expression, None)),
            (Operation::Search, search_request(&expression, Some("t1"))),
        ]
    );
    let request = dumps(&client.seen[1].1);
    assert!(request.contains(r#""Resource": "HubContent""#), "{request}");
    assert!(request.contains(r#""MaxResults": 100, "SortBy": "HubContentName", "SortOrder": "Ascending", "NextToken": "t1""#), "{request}");
    assert_eq!(selection.retrieval_backend, "search");
    assert!(selection.fallback_reason.is_none());
    assert!(selection.warnings.is_empty());
    // Dates come from the snapshot: June before May.
    let names: Vec<&str> = selection
        .models
        .iter()
        .filter_map(|m| m.get("name").and_then(Json::as_str))
        .collect();
    assert_eq!(names, ["Mistral-7B", "llama-3-8b"]);
    assert_eq!(
        selection.ordering.timestamp_source,
        "selection_session_snapshot"
    );
    assert_eq!(
        (
            selection.ordering.dated_models,
            selection.ordering.undated_models
        ),
        (2, 0)
    );
}

#[test]
fn a_search_only_model_is_undated_and_reported() {
    let page = Json::object(vec![(
        "Results",
        Json::Array(vec![
            hub_content("brand-new", &["@task:text generation"]),
            hub_content("Mistral-7B", &["@task:text generation"]),
        ]),
    )]);
    let mut client = Spy::new(vec![Ok(page)]);
    let selection = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &parsed(&["task:text generation"]),
        &catalog(),
        &mut client,
        "live_list",
    )
    .expect("selects");
    let names: Vec<&str> = selection
        .models
        .iter()
        .filter_map(|m| m.get("name").and_then(Json::as_str))
        .collect();
    assert_eq!(names, ["Mistral-7B", "brand-new"]);
    assert_eq!(
        selection.warnings,
        ["1 of 2 Search results had no cached OriginalCreationTime and were ordered after dated models."]
    );
    assert_eq!(selection.ordering.timestamp_source, "live_list");
}

#[test]
fn an_empty_successful_search_is_authoritative() {
    let mut client = Spy::new(vec![Ok(j(r#"{"Results": []}"#))]);
    let selection = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &parsed(&["task:text generation"]),
        &catalog(),
        &mut client,
        "snapshot",
    )
    .expect("selects");
    assert_eq!(selection.retrieval_backend, "search");
    assert!(selection.models.is_empty());
}

fn fallback_of(mut client: Spy, words: &[&str]) -> (Vec<String>, String, String, usize) {
    let selection = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &parsed(words),
        &catalog(),
        &mut client,
        "snapshot",
    )
    .expect("selects");
    assert_eq!(selection.retrieval_backend, "list_fallback");
    assert_eq!(
        selection.ordering.timestamp_source,
        "list_hub_contents_snapshot"
    );
    assert_eq!(
        selection.warnings,
        ["SageMaker Search was unavailable; used the complete ListHubContents snapshot and local filtering instead."]
    );
    let names = selection
        .models
        .iter()
        .filter_map(|m| m.get("name").and_then(Json::as_str))
        .map(str::to_string)
        .collect();
    let (kind, message) = selection.fallback_reason.expect("reason");
    (names, kind.to_string(), message, client.seen.len())
}

#[test]
fn a_service_failure_falls_back_to_the_complete_local_filter() {
    let client = Spy::new(vec![Err(service_error("AccessDeniedException", "denied"))]);
    let (names, kind, message, calls) = fallback_of(client, &["task:text generation"]);
    // Newest first, undated last: the same order the list workflow prints.
    assert_eq!(names, ["Mistral-7B", "llama-3-8b", "big-model-70b"]);
    assert_eq!(kind, "SearchServiceError");
    assert_eq!(
        message,
        "SageMaker Search failed (AccessDeniedException): denied"
    );
    assert_eq!(calls, 1);
}

#[test]
fn a_malformed_response_falls_back_as_a_response_error() {
    for (response, needle) in [
        (
            r#"{"Results": "x"}"#,
            "Results must be a list when present (it is string)",
        ),
        (
            r#"{"Results": [{"Other": 1}]}"#,
            "Search result 0 does not contain a HubContent object",
        ),
        (
            r#"{"Results": [], "NextToken": 5}"#,
            "NextToken must be a string, got integer",
        ),
    ] {
        let (_, kind, message, _) =
            fallback_of(Spy::new(vec![Ok(j(response))]), &["task:embedding"]);
        assert_eq!(kind, "SearchResponseError", "{response}");
        assert!(message.contains(needle), "{message}");
    }
}

#[test]
fn a_repeated_page_token_falls_back_without_echoing_it() {
    let page = j(r#"{"Results": [], "NextToken": "SECRET-LOOKING-TOKEN"}"#);
    let (_, kind, message, calls) = fallback_of(
        Spy::new(vec![Ok(page.clone()), Ok(page)]),
        &["task:embedding"],
    );
    assert_eq!(kind, "SearchResponseError");
    assert!(message.contains("already returned"), "{message}");
    assert!(!message.contains("SECRET"), "{message}");
    assert_eq!(calls, 2);
}

#[test]
fn an_unsupported_client_falls_back_without_sending_anything() {
    let mut client = Spy::new(vec![]);
    client.search_support = Err("the linked SDK has no HubContent Search".to_string());
    let (names, kind, message, calls) = fallback_of(client, &["task:embedding"]);
    assert_eq!(names, ["tiny-embedder"]);
    assert_eq!(kind, "UnsupportedSdkError");
    assert_eq!(message, "the linked SDK has no HubContent Search");
    assert_eq!(calls, 0);
}

#[test]
fn the_build_without_a_live_client_falls_back_and_sends_nothing() {
    let selection = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &parsed(&["task:embedding"]),
        &catalog(),
        &mut NoLiveClient,
        "snapshot",
    )
    .expect("selects");
    assert_eq!(selection.retrieval_backend, "list_fallback");
    let (kind, message) = selection.fallback_reason.expect("reason");
    assert_eq!(kind, "SearchServiceError");
    assert!(
        message.starts_with("SageMaker Search failed: "),
        "{message}"
    );
    assert!(message.contains("no live AWS client"), "{message}");
}

#[test]
fn hostile_service_text_is_escaped_onto_one_line() {
    let client = Spy::new(vec![Err(service_error(
        "Bad\u{1b}Code",
        "line1\nline2\u{202e}",
    ))]);
    let (_, _, message, _) = fallback_of(client, &["task:embedding"]);
    assert!(
        !message.contains('\n') && !message.contains('\u{1b}') && !message.contains('\u{202e}'),
        "{message}"
    );
    assert!(message.contains("line1 line2\\u202E"), "{message}");
}

#[test]
fn a_constraint_problem_is_never_hidden_by_the_fallback() {
    let mut client = Spy::new(vec![]);
    let error = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &parsed(&["task:nonexistent"]),
        &catalog(),
        &mut client,
        "snapshot",
    )
    .expect_err("visible");
    assert_eq!(error.kind(), "ConstraintValidationError");
    assert!(client.seen.is_empty());
    let error = select_public_hub_models(
        PUBLIC_HUB_NAME,
        &parsed(&[]),
        &catalog(),
        &mut client,
        "cache",
    )
    .expect_err("refused");
    assert_eq!(error.message(), "Unsupported catalog source: \"cache\"");
}

#[test]
fn a_malformed_search_record_is_a_response_error() {
    let mut record = hub_content("x", &[]);
    if let Json::Object(members) = &mut record {
        members[0].1 = j(r#"{"HubContentName": "x", "HubContentSearchKeywords": "oops"}"#);
    }
    let client = Spy::new(vec![Ok(Json::object(vec![(
        "Results",
        Json::Array(vec![record]),
    )]))]);
    let (_, kind, message, _) = fallback_of(client, &["task:embedding"]);
    assert_eq!(kind, "SearchResponseError");
    assert!(message.starts_with("Search result 0: "), "{message}");
}

#[test]
fn catalog_dates_replace_search_dates_and_search_only_models_stay_undated() {
    let search = [
        j(r#"{"name": "Mistral-7B", "original_creation_time": "2099-01-01T00:00:00+00:00"}"#),
        j(r#"{"name": "unknown-model", "original_creation_time": "2098-01-01T00:00:00+00:00"}"#),
    ];
    let (ordered, summary) =
        restore_catalog_order(&search, &catalog(), "selection_session_snapshot").expect("orders");
    assert_eq!(
        dumps(&Json::Array(ordered)),
        r#"[{"name": "Mistral-7B", "original_creation_time": "2024-06-01T00:00:00+00:00"}, {"name": "unknown-model"}]"#
    );
    assert_eq!((summary.dated_models, summary.undated_models), (1, 1));
}
