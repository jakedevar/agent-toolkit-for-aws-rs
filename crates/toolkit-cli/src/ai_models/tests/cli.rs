//! End-to-end tests of the six commands through the shared router, with
//! recorded responses and temporary files only.

use std::fs;

use toolkit_core::{parse_json, Json};

use super::{catalog, dumps, j};
use super::{exec, path_text, write, TempDir};
use crate::ai_models::snapshot::{build_catalog_snapshot, write_catalog_snapshot};

const LIST_ONE: &str = r#"{"calls": {"ListHubContents": [{"request":
    {"HubName": "my-hub", "HubContentType": "Model", "MaxResults": 100},
    "response": {"HubContentSummaries": [{"HubContentName": "m1", "HubContentSearchKeywords":
        ["@task:text generation", "@model-size:7B", "@capability:customization"],
        "OriginalCreationTime": "2024-01-02T03:04:05+00:00"}]}}]}}"#;

const ONE_MODEL: &str = concat!(
    r#"{"name": "m1", "tasks": ["text generation"], "size_raw": "7B", "size": "<=10b", "#,
    r#""bedrock_eligible": false, "customization_eligible": true, "#,
    r#""original_creation_time": "2024-01-02T03:04:05+00:00"}"#
);

fn with_recorded(label: &str, recording: &str) -> (TempDir, String) {
    let dir = TempDir::new(label);
    write(&dir, "recorded.json", recording);
    let path = path_text(&dir.join("recorded.json")).to_string();
    (dir, path)
}

#[test]
fn list_models_prints_the_models_as_python_json() {
    let (_dir, recorded) = with_recorded("ai-models-list", LIST_ONE);
    let (status, out, err) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    assert_eq!(out, format!("[{ONE_MODEL}]\n"));
}

#[test]
fn list_values_prints_the_distinct_values() {
    let (_dir, recorded) = with_recorded("ai-models-values", LIST_ONE);
    let (status, out, _) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "--list-values",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 0);
    assert_eq!(
        out,
        concat!(
            r#"{"tasks": ["text generation"], "data_types": [], "input_modalities": [], "output_modalities": [], "#,
            r#""sizes": ["<=10b"], "licenses": [], "languages": [], "context_windows": [], "model_types": [], "#,
            r#""providers": [], "frameworks": []}"#,
            "\n"
        )
    );
}

#[test]
fn a_snapshot_file_is_written_while_the_models_are_still_printed() {
    let (dir, recorded) = with_recorded("ai-models-snapshot", LIST_ONE);
    let snapshot = path_text(&dir.join("snap.json")).to_string();
    let (status, out, err) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "us-east-2",
        "--snapshot-file",
        &snapshot,
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    assert_eq!(out, format!("[{ONE_MODEL}]\n"));
    let written = parse_json(&fs::read_to_string(&snapshot).expect("snapshot")).expect("json");
    assert_eq!(written.get("hub_name"), Some(&Json::string("my-hub")));
    assert_eq!(written.get("region"), Some(&Json::string("us-east-2")));
    assert_eq!(written.get("model_count"), Some(&Json::Int(1)));
    assert!(written
        .get("fetched_at")
        .and_then(Json::as_str)
        .is_some_and(|t| t.ends_with("+00:00")));
}

#[test]
fn create_snapshot_prints_the_new_private_file_and_the_values() {
    let (_dir, recorded) = with_recorded("ai-models-create", LIST_ONE);
    let (status, out, err) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "us-east-2",
        "--list-values",
        "--create-snapshot",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let printed = parse_json(&out).expect("json");
    let file = printed
        .get("snapshot_file")
        .and_then(Json::as_str)
        .expect("path")
        .to_string();
    assert!(file.contains("sherpa-model-catalog."), "{file}");
    assert!(printed
        .get("available_values")
        .and_then(|v| v.get("tasks"))
        .is_some());
    assert!(fs::metadata(&file).expect("exists").is_file());
    fs::remove_file(&file).expect("cleanup");
}

#[test]
fn snapshot_options_are_checked_before_anything_is_called() {
    let cases: [(&[&str], &str); 3] = [
        (
            &["--create-snapshot"],
            "--create-snapshot requires --list-values",
        ),
        (
            &[
                "--create-snapshot",
                "--list-values",
                "--snapshot-file",
                "x.json",
            ],
            "Use either --create-snapshot or --snapshot-file, not both",
        ),
        (
            &["--list-values", "--create-snapshot"],
            "A configured AWS region is required",
        ),
    ];
    for (extra, message) in cases {
        let mut args = vec!["ai-models", "list-models", "my-hub"];
        args.extend_from_slice(extra);
        let (status, out, err) = exec(&args);
        assert!(err.contains(message) || status == 1, "{err}");
        assert_eq!(out, "");
        if message.starts_with("--create") || message.starts_with("Use either") {
            assert_eq!(status, 2, "{err}");
            assert!(err.contains(message), "{err}");
        }
    }
}

#[test]
fn a_snapshot_without_any_region_is_refused_after_the_listing() {
    let (_dir, recorded) = with_recorded("ai-models-noregion", LIST_ONE);
    let (status, out, err) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "--list-values",
        "--create-snapshot",
        "--recorded",
        &recorded,
    ]);
    // AWS_REGION may be set in the test environment: only the refusal path is checked.
    if std::env::var_os("AWS_REGION").is_none() && std::env::var_os("AWS_DEFAULT_REGION").is_none()
    {
        assert_eq!(status, 1);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "error: A configured AWS region is required to create a catalog snapshot\n"
        );
    }
}

#[test]
fn this_build_sends_nothing_without_a_recording() {
    let (status, out, err) = exec(&["ai-models", "list-models", "my-hub"]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(
        err.starts_with(
            "error: invalid client: this build has no live AWS client and sent nothing"
        ),
        "{err}"
    );
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn aws_failures_use_the_python_wording_on_one_escaped_line() {
    let (_dir, recorded) = with_recorded(
        "ai-models-denied",
        r#"{"calls": {"ListHubContents": [{"error": {"code": "AccessDenied\u001b", "message": "no\nway\u202e"}}]}}"#,
    );
    let (status, out, err) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(
        err,
        "error: AWS API call failed (AccessDenied?): no way\\u202E\n"
    );
    let (_dir, no_credentials) = with_recorded(
        "ai-models-nocreds",
        r#"{"calls": {"ListHubContents": [{"error": {"kind": "no_credentials"}}]}}"#,
    );
    let (_, _, err) = exec(&[
        "ai-models",
        "list-models",
        "my-hub",
        "--recorded",
        &no_credentials,
    ]);
    assert_eq!(
        err,
        "error: No AWS credentials found. Configure credentials via 'aws configure' or environment variables.\n"
    );
}

#[test]
fn invalid_names_and_regions_are_usage_errors_that_do_not_echo_them() {
    let hostile = "bad\u{1b}[31m\u{202e}FAKE-SECRET";
    for args in [
        vec!["ai-models", "list-models", hostile],
        vec!["ai-models", "list-models", "my-hub", hostile],
        vec!["ai-models", "model-names", hostile],
        vec!["ai-models", "recipes", hostile, "my-hub"],
        vec!["ai-models", "recipes", "m", hostile],
        vec!["ai-models", "endpoint-diagnostics", hostile, "us-east-1"],
        vec!["ai-models", "endpoint-diagnostics", "e", hostile],
    ] {
        let (status, out, err) = exec(&args);
        assert_eq!(status, 2, "{args:?}: {err}");
        assert_eq!(out, "");
        assert!(
            !err.contains("FAKE-SECRET") && !err.contains('\u{1b}') && !err.contains('\u{202e}'),
            "{err}"
        );
    }
}

#[test]
fn filter_applies_constraints_to_a_file_and_warns_about_bad_words() {
    let dir = TempDir::new("ai-models-filter");
    write(
        &dir,
        "models.json",
        r#"[{"name": "a", "tasks": ["Text Generation"], "size": "<=10b", "original_creation_time": "2024-01-01T00:00:00+00:00"},
            {"name": "b", "tasks": ["embedding"], "size": "<=10b"}]"#,
    );
    let file = path_text(&dir.join("models.json")).to_string();
    let (status, out, err) = exec(&[
        "ai-models",
        "filter",
        &file,
        "task:TEXT",
        "size:<=10B",
        "oops",
        "colour:red",
    ]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        out,
        concat!(
            r#"{"total_models": 2, "filters_applied": {"task": ["text"], "size": ["<=10b"]}, "matched": 1, "#,
            r#""models": [{"name": "a", "tasks": ["Text Generation"], "size": "<=10b", "#,
            r#""original_creation_time": "2024-01-01T00:00:00+00:00"}]}"#,
            "\n"
        )
    );
    assert_eq!(
        err,
        concat!(
            "Warning: Ignoring malformed constraint (no ':' separator): \"oops\"\n",
            "Warning: Unrecognized constraint key \"colour\". Valid keys: bedrock, context_window, ",
            "customization, data_type, input_modality, language, license, model_type, name, ",
            "output_modality, provider, size, task\n"
        )
    );
}

#[test]
fn filter_warnings_quote_hostile_words() {
    let dir = TempDir::new("ai-models-filter-hostile");
    write(&dir, "models.json", "[]");
    let file = path_text(&dir.join("models.json")).to_string();
    let hostile = "x\u{1b}[31m\u{202e}\nerror: injected";
    let (status, _, err) = exec(&["ai-models", "filter", &file, hostile]);
    assert_eq!(status, 0);
    assert!(
        !err.contains('\u{1b}') && !err.contains('\u{202e}'),
        "{err}"
    );
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn filter_fails_closed_on_ambiguous_booleans_and_bad_files() {
    let dir = TempDir::new("ai-models-filter-bad");
    write(
        &dir,
        "models.json",
        r#"[{"name": "a", "bedrock_eligible": true}]"#,
    );
    write(&dir, "dict.json", r#"{"name": "a"}"#);
    write(&dir, "items.json", "[1]");
    write(&dir, "broken.json", "[{");
    let at = |name: &str| path_text(&dir.join(name)).to_string();
    let (status, out, err) = exec(&["ai-models", "filter", &at("models.json"), "bedrock:maybe"]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(err, "error: constraint bedrock must be true or false\n");
    for name in ["dict.json", "items.json"] {
        let (status, _, err) = exec(&["ai-models", "filter", &at(name)]);
        assert_eq!(status, 1);
        assert!(
            err.contains("must hold a JSON list of model objects"),
            "{err}"
        );
    }
    let (status, _, err) = exec(&["ai-models", "filter", &at("broken.json")]);
    assert_eq!(status, 1);
    assert!(err.starts_with("error: Invalid JSON in "), "{err}");
    let (status, _, err) = exec(&["ai-models", "filter", &at("missing.json")]);
    assert_eq!(status, 1);
    assert!(err.starts_with("error: File not found: "), "{err}");
    let (status, _, err) = exec(&["ai-models", "filter"]);
    assert_eq!(status, 2);
    assert!(err.contains("usage: toolkit ai-models filter"), "{err}");
}

#[cfg(unix)]
#[test]
fn filter_and_recorded_inputs_refuse_symbolic_links() {
    let dir = TempDir::new("ai-models-links");
    write(&dir, "real.json", "[]");
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(dir.join("real.json"), &link).expect("symlink");
    let (status, _, err) = exec(&["ai-models", "filter", path_text(&link)]);
    assert_eq!(status, 1);
    assert!(err.contains("symbolic links are not followed"), "{err}");
    let (status, _, err) = exec(&[
        "ai-models",
        "model-names",
        "my-hub",
        "--recorded",
        path_text(&link),
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("symbolic links are not followed"), "{err}");
}

#[test]
fn a_bad_recording_is_refused_by_position() {
    let (_dir, recorded) = with_recorded(
        "ai-models-badrec",
        r#"{"calls": {"Search": [{"response": 5, "x": "FAKE-SECRET"}]}}"#,
    );
    let (status, _, err) = exec(&[
        "ai-models",
        "model-names",
        "my-hub",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        err,
        "error: the recorded input is invalid: calls.Search[0] may only contain `request`, `response`, `error`\n"
    );
}

#[test]
fn model_names_are_sorted_and_recipes_are_listed() {
    let (_dir, recorded) = with_recorded(
        "ai-models-names",
        r#"{"calls": {"ListHubContents": [{"response": {"HubContentSummaries": [
            {"HubContentName": "zed", "HubContentSearchKeywords": ["@capability:customization"]},
            {"HubContentName": "Alpha", "HubContentSearchKeywords": ["@capability:customization"]},
            {"HubContentName": "plain", "HubContentSearchKeywords": []}]}}]}}"#,
    );
    let (status, out, _) = exec(&[
        "ai-models",
        "model-names",
        "my-hub",
        "us-west-2",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, out.as_str()), (0, "[\"Alpha\", \"zed\"]\n"));

    let (_dir, recorded) = with_recorded(
        "ai-models-recipes",
        r#"{"calls": {"DescribeHubContent": [{"request":
            {"HubName": "my-hub", "HubContentType": "Model", "HubContentName": "m1"},
            "response": {"HubContentSearchKeywords":
              ["@recipe:finetuning_sft_lora", "@recipe:finetuning_dpo_full", "@recipe:finetuning_other"]}}]}}"#,
    );
    let (status, out, _) = exec(&[
        "ai-models",
        "recipes",
        "m1",
        "my-hub",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, out.as_str()), (0, "[\"dpo\", \"sft\"]\n"));
}

#[test]
fn an_unknown_model_is_an_error_not_an_empty_list() {
    let (_dir, recorded) = with_recorded(
        "ai-models-unknown",
        r#"{"calls": {"DescribeHubContent": [{"error": {"code": "ResourceNotFound", "message": "Requested resource not found"}}]}}"#,
    );
    let (status, out, err) = exec(&[
        "ai-models",
        "recipes",
        "nope",
        "my-hub",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(
        err,
        "error: AWS API call failed (ResourceNotFound): Requested resource not found\n"
    );
}

fn snapshot_file(dir: &TempDir) -> String {
    let path = dir.join("catalog.json");
    let snapshot = build_catalog_snapshot(
        "SageMakerPublicHub",
        Some("us-east-2"),
        &catalog(),
        "2024-07-01T12:00:00+00:00",
    )
    .expect("builds");
    write_catalog_snapshot(&path, &snapshot).expect("writes");
    path_text(&path).to_string()
}

#[test]
fn search_uses_the_recorded_response_and_the_snapshot_dates() {
    let dir = TempDir::new("ai-models-search");
    let snapshot = snapshot_file(&dir);
    write(
        &dir,
        "search.json",
        r#"{"calls": {"Search": [{"response": {"Results": [
            {"HubContent": {"HubContentName": "Mistral-7B", "HubContentSearchKeywords": ["@task:text generation"]}}]}}]}}"#,
    );
    let (status, out, err) = exec(&[
        "ai-models",
        "search",
        "SageMakerPublicHub",
        "task:text generation",
        "--region",
        "us-east-2",
        "--snapshot-file",
        &snapshot,
        "--recorded",
        path_text(&dir.join("search.json")),
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let printed = j(&out);
    assert_eq!(printed.get("total_models"), Some(&Json::Int(4)));
    assert_eq!(printed.get("matched"), Some(&Json::Int(1)));
    assert_eq!(
        printed.get("catalog_source"),
        Some(&Json::string("snapshot"))
    );
    assert_eq!(
        printed.get("retrieval_backend"),
        Some(&Json::string("search"))
    );
    assert_eq!(printed.get("fallback_reason"), None);
    assert_eq!(
        dumps(printed.get("filters_applied").expect("filters")),
        r#"{"task": ["text generation"]}"#
    );
    assert_eq!(
        dumps(printed.get("ordering").expect("ordering")),
        r#"{"field": "OriginalCreationTime", "timestamp_source": "selection_session_snapshot", "dated_models": 1, "undated_models": 0}"#
    );
    let models = dumps(printed.get("models").expect("models"));
    assert!(
        models.contains(r#""original_creation_time": "2024-06-01T00:00:00+00:00""#),
        "{models}"
    );
}

#[test]
fn without_a_client_search_falls_back_visibly_to_the_snapshot() {
    let dir = TempDir::new("ai-models-search-fallback");
    let snapshot = snapshot_file(&dir);
    let (status, out, err) = exec(&[
        "ai-models",
        "search",
        "SageMakerPublicHub",
        "task:text generation",
        "size:<=10b",
        "--snapshot-file",
        &snapshot,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let printed = j(&out);
    assert_eq!(
        printed.get("retrieval_backend"),
        Some(&Json::string("list_fallback"))
    );
    assert_eq!(printed.get("matched"), Some(&Json::Int(2)));
    let reason = printed.get("fallback_reason").expect("reason");
    assert_eq!(
        reason.get("type"),
        Some(&Json::string("SearchServiceError"))
    );
    assert!(dumps(reason).contains("no live AWS client"));
    let names: Vec<&str> = match printed.get("models") {
        Some(Json::Array(models)) => models
            .iter()
            .filter_map(|m| m.get("name").and_then(Json::as_str))
            .collect(),
        _ => panic!("models"),
    };
    assert_eq!(names, ["Mistral-7B", "llama-3-8b"]);
    // Insertion order of the output members is the dataclass order.
    assert!(out.find("\"retrieval_backend\"") < out.find("\"fallback_reason\""));
}

#[test]
fn search_refuses_private_hubs_unresolvable_values_and_a_foreign_snapshot() {
    let dir = TempDir::new("ai-models-search-refuse");
    let snapshot = snapshot_file(&dir);
    let run = |args: &[&str]| {
        let mut full = vec!["ai-models", "search"];
        full.extend_from_slice(args);
        exec(&full)
    };
    let (status, out, err) = run(&["my-hub", "--snapshot-file", &snapshot]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(
        err,
        "error: The public-Hub Search adapter supports SageMakerPublicHub only; private Hubs must continue using ListHubContents\n"
    );
    let (status, _, err) = run(&[
        "SageMakerPublicHub",
        "task:nonexistent",
        "--snapshot-file",
        &snapshot,
    ]);
    assert_eq!(status, 1);
    assert!(
        err.contains("do not match any current public-Hub catalog value"),
        "{err}"
    );
    let (status, _, err) = run(&[
        "SageMakerPublicHub",
        "bedrock:false",
        "--snapshot-file",
        &snapshot,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("supports bedrock:true only"), "{err}");
    let (status, _, err) = run(&[
        "SageMakerPublicHub",
        "--region",
        "eu-west-1",
        "--snapshot-file",
        &snapshot,
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        err,
        "error: Catalog snapshot is for region \"us-east-2\", not \"eu-west-1\"\n"
    );
    let (status, _, err) = run(&[
        "SageMakerPublicHub",
        "nocolon",
        "--snapshot-file",
        &snapshot,
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        err,
        "error: Malformed constraint \"nocolon\"; expected key:value\n"
    );
}

#[test]
fn search_without_a_snapshot_lists_the_catalog_first() {
    let (_dir, recorded) = with_recorded(
        "ai-models-search-live",
        r#"{"calls": {
            "ListHubContents": [{"response": {"HubContentSummaries": [
              {"HubContentName": "a", "HubContentSearchKeywords": ["@task:embedding"], "OriginalCreationTime": "2024-01-01T00:00:00+00:00"}]}}],
            "Search": [{"response": {"Results": [{"HubContent": {"HubContentName": "a", "HubContentSearchKeywords": ["@task:embedding"]}}]}}]}}"#,
    );
    let (status, out, err) = exec(&[
        "ai-models",
        "search",
        "SageMakerPublicHub",
        "task:embedding",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let printed = j(&out);
    assert_eq!(
        printed.get("catalog_source"),
        Some(&Json::string("live_list"))
    );
    assert_eq!(
        printed.get("retrieval_backend"),
        Some(&Json::string("search"))
    );
    assert_eq!(printed.get("total_models"), Some(&Json::Int(1)));
}

#[test]
fn a_failed_catalog_listing_is_reported_as_discovery() {
    let (_dir, recorded) = with_recorded(
        "ai-models-search-discovery",
        r#"{"calls": {"ListHubContents": [{"error": {"code": "AccessDeniedException", "message": "denied"}}]}}"#,
    );
    let (status, _, err) = exec(&[
        "ai-models",
        "search",
        "SageMakerPublicHub",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        err,
        "error: Public-Hub catalog discovery failed (AccessDeniedException): denied\n"
    );
}

const DIAGNOSTICS: &str = r#"{"calls": {
    "DescribeEndpoint": [{"request": {"EndpointName": "my-endpoint"}, "response":
        {"EndpointStatus": "InService", "ProductionVariants": [{"VariantName": "AllTraffic"}]}}],
    "GetMetricData": [{"response": {"MetricDataResults": [{"Id": "invocations", "Values": [3.0]}]}}],
    "FilterLogEvents": [{"request": {"logGroupName": "/aws/sagemaker/Endpoints/my-endpoint",
        "startTime": 1710513900000, "endTime": 1710514800000, "limit": 100},
        "response": {"events": [{"message": "ready", "timestamp": 1710514500000}]}}]}}"#;

#[test]
fn endpoint_diagnostics_runs_offline_against_a_pinned_clock() {
    let (_dir, recorded) = with_recorded("ai-models-diag", DIAGNOSTICS);
    let (status, out, err) = exec(&[
        "ai-models",
        "endpoint-diagnostics",
        "my-endpoint",
        "us-east-1",
        "--now",
        "2024-03-15T15:00:00Z",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let printed = j(&out);
    assert_eq!(
        dumps(printed.get("metrics").expect("metrics")),
        r#"{"invocations": [3.0]}"#
    );
    assert_eq!(
        dumps(printed.get("recent_logs").expect("logs")),
        r#"[{"message": "ready", "timestamp": "2024-03-15T14:55:00+00:00"}]"#
    );
    assert_eq!(dumps(printed.get("errors").expect("errors")), "[]");
}

#[test]
fn endpoint_diagnostics_validates_its_window_arguments() {
    let (_dir, recorded) = with_recorded("ai-models-diag-args", DIAGNOSTICS);
    let base = [
        "ai-models",
        "endpoint-diagnostics",
        "my-endpoint",
        "us-east-1",
    ];
    let cases: [(&[&str], &str); 7] = [
        (
            &["--minutes", "5", "--start", "2024-03-15T14:00:00Z"],
            "--minutes cannot be combined with --start",
        ),
        (&["--end", "2024-03-15T14:00:00Z"], "--end requires --start"),
        (
            &["--minutes", "0"],
            "--minutes must be a whole number from 1 to 525600",
        ),
        (
            &["--minutes", "+5"],
            "--minutes must be a whole number from 1 to 525600",
        ),
        (
            &["--minutes", "525601"],
            "--minutes must be a whole number from 1 to 525600",
        ),
        (
            &["--start", "yesterday"],
            "--start is not a valid ISO 8601 timestamp",
        ),
        (&["--now", "x"], "--now is not a valid ISO 8601 timestamp"),
    ];
    for (extra, message) in cases {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--recorded", &recorded]);
        let (status, out, err) = exec(&args);
        assert_eq!((status, out.as_str()), (2, ""), "{extra:?}: {err}");
        assert!(err.contains(message), "{extra:?}: {err}");
    }
    let (status, _, err) = exec(&["ai-models", "endpoint-diagnostics", "my-endpoint"]);
    assert_eq!(status, 2);
    assert!(
        err.contains("endpoint_name and region are required"),
        "{err}"
    );
}

#[test]
fn endpoint_diagnostics_stops_on_a_failure_that_is_not_a_service_error() {
    let (_dir, recorded) = with_recorded(
        "ai-models-diag-stop",
        r#"{"calls": {"DescribeEndpoint": [{"error": {"kind": "no_credentials"}}]}}"#,
    );
    let (status, out, err) = exec(&[
        "ai-models",
        "endpoint-diagnostics",
        "my-endpoint",
        "us-east-1",
        "--recorded",
        &recorded,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.starts_with("error: No AWS credentials found."), "{err}");
}

#[test]
fn every_command_is_registered_with_strict_values_and_no_passthrough() {
    let commands: Vec<_> = crate::COMMANDS
        .iter()
        .filter(|c| c.path[0] == "ai-models")
        .collect();
    assert_eq!(commands.len(), 6);
    for command in commands {
        assert!(
            command.strict_values && !command.passthrough,
            "{:?}",
            command.path
        );
        assert!(command.repeatable_flags.is_empty(), "{:?}", command.path);
    }
}
