use toolkit_core::Json;

use super::{dumps, j};
use crate::ai_models::filter::{
    add_constraint, filter_models, matches, sort_models_newest_first, Constraints,
};

fn constraints(pairs: &[(&str, &str)]) -> Constraints {
    let mut built = Vec::new();
    for (key, value) in pairs {
        add_constraint(&mut built, key, (*value).to_string());
    }
    built
}

fn names(models: &[Json]) -> Vec<String> {
    models
        .iter()
        .map(|m| {
            m.get("name")
                .and_then(Json::as_str)
                .unwrap_or("?")
                .to_string()
        })
        .collect()
}

fn fixture() -> Vec<Json> {
    vec![
        j(
            r#"{"name": "Alpha-7B", "tasks": ["Text Generation"], "size": "<=10b", "license": "Apache-2.0",
              "languages": ["en", "de"], "model_type": "LLM", "provider": "Meta", "context_window": "8k",
              "data_types": ["text"], "input_modalities": ["text", "image"], "output_modalities": ["text"],
              "bedrock_eligible": true, "customization_eligible": false,
              "original_creation_time": "2024-05-01T00:00:00+00:00"}"#,
        ),
        j(
            r#"{"name": "beta-70b", "tasks": ["embedding"], "size": "11b-70b", "license": "MIT",
              "languages": ["fr"], "model_type": "EMBEDDING", "provider": "acme", "context_window": "32k",
              "bedrock_eligible": false, "customization_eligible": true,
              "original_creation_time": "2024-06-01T00:00:00+00:00"}"#,
        ),
        j(r#"{"name": "gamma"}"#),
    ]
}

fn run(pairs: &[(&str, &str)]) -> Vec<String> {
    names(&filter_models(&fixture(), &constraints(pairs)).expect("filters"))
}

#[test]
fn substring_keys_match_case_insensitively() {
    assert_eq!(run(&[("task", "text gen")]), ["Alpha-7B"]);
    assert_eq!(run(&[("license", "apache")]), ["Alpha-7B"]);
    assert_eq!(
        run(&[("language", "DE".to_lowercase().as_str())]),
        ["Alpha-7B"]
    );
    assert_eq!(run(&[("provider", "meta")]), ["Alpha-7B"]);
    assert_eq!(run(&[("data_type", "tex")]), ["Alpha-7B"]);
    assert_eq!(run(&[("input_modality", "image")]), ["Alpha-7B"]);
    assert_eq!(run(&[("output_modality", "text")]), ["Alpha-7B"]);
    assert_eq!(
        run(&[("name", "BETA".to_lowercase().as_str())]),
        ["beta-70b"]
    );
}

#[test]
fn exact_keys_do_not_match_substrings() {
    assert_eq!(run(&[("size", "<=10b")]), ["Alpha-7B"]);
    assert!(run(&[("size", "10b")]).is_empty());
    assert_eq!(run(&[("context_window", "32k")]), ["beta-70b"]);
    assert!(run(&[("context_window", "32")]).is_empty());
    assert_eq!(run(&[("model_type", "llm")]), ["Alpha-7B"]);
    assert!(run(&[("model_type", "ll")]).is_empty());
}

#[test]
fn values_of_one_key_are_ored_and_keys_are_anded() {
    // Newest first: beta (June) before Alpha (May).
    assert_eq!(
        run(&[("size", "<=10b"), ("size", "11b-70b")]),
        ["beta-70b", "Alpha-7B"]
    );
    assert_eq!(
        run(&[("size", "<=10b"), ("size", "11b-70b"), ("license", "mit")]),
        ["beta-70b"]
    );
    assert!(run(&[("size", "<=10b"), ("license", "mit")]).is_empty());
}

#[test]
fn a_model_without_the_metadata_is_excluded_and_no_constraint_keeps_all() {
    assert_eq!(run(&[("provider", "a")]), ["beta-70b", "Alpha-7B"]);
    assert_eq!(run(&[]), ["beta-70b", "Alpha-7B", "gamma"]);
}

#[test]
fn eligibility_constraints_use_the_last_value() {
    assert_eq!(run(&[("bedrock", "true")]), ["Alpha-7B"]);
    assert_eq!(run(&[("bedrock", "false")]), ["beta-70b"]);
    assert_eq!(
        run(&[("bedrock", "false"), ("bedrock", "true")]),
        ["Alpha-7B"]
    );
    assert_eq!(run(&[("customization", "true")]), ["beta-70b"]);
    assert_eq!(run(&[("customization", "false")]), ["Alpha-7B"]);
}

#[test]
fn an_ambiguous_eligibility_value_fails_closed() {
    // The Python script ignored the constraint and kept every model.
    for key in ["bedrock", "customization"] {
        let error =
            filter_models(&fixture(), &constraints(&[(key, "maybe")])).expect_err("rejected");
        assert_eq!(error, format!("constraint {key} must be true or false"));
    }
}

#[test]
fn a_mistyped_member_is_an_error_naming_the_model() {
    let models = [
        j(r#"{"name": "a"}"#),
        j(r#"{"name": "b", "tasks": "FAKE-SECRET"}"#),
    ];
    let error = filter_models(&models, &constraints(&[("task", "x")])).expect_err("rejected");
    assert_eq!(error, "model 1: tasks must be a list of strings");
    let flag = [j(r#"{"name": "a", "bedrock_eligible": "yes"}"#)];
    let error = filter_models(&flag, &constraints(&[("bedrock", "true")])).expect_err("rejected");
    assert_eq!(error, "model 0: bedrock_eligible must be true or false");
}

#[test]
fn null_members_count_as_missing() {
    let model = j(r#"{"name": "a", "tasks": null, "license": null}"#);
    assert!(!matches(&model, &constraints(&[("task", "x")])).expect("ok"));
    assert!(!matches(&model, &constraints(&[("license", "x")])).expect("ok"));
}

#[test]
fn newest_first_is_total_and_stable() {
    let models = [
        j(r#"{"name": "undated-b"}"#),
        j(r#"{"name": "Old", "original_creation_time": "2020-01-01T00:00:00+00:00"}"#),
        j(r#"{"name": "undated-A"}"#),
        j(r#"{"name": "tie-b", "original_creation_time": "2024-01-01T00:00:00+00:00"}"#),
        j(r#"{"name": "Tie-A", "original_creation_time": "2024-01-01T00:00:00+00:00"}"#),
        // Isoformat omits microseconds when zero: the shorter string is a
        // prefix of the longer one and must still sort as the older.
        j(r#"{"name": "frac", "original_creation_time": "2024-01-01T00:00:00.5+00:00"}"#),
        j(r#"{"name": "null-date", "original_creation_time": null}"#),
    ];
    let sorted = sort_models_newest_first(&models).expect("sorts");
    assert_eq!(
        names(&sorted),
        [
            "frac",
            "Tie-A",
            "tie-b",
            "Old",
            "null-date",
            "undated-A",
            "undated-b"
        ]
    );
    // The input is untouched.
    assert_eq!(names(&models)[0], "undated-b");
    // Same input, same output.
    assert_eq!(sorted, sort_models_newest_first(&models).expect("sorts"));
}

#[test]
fn a_non_string_creation_time_is_an_error() {
    let error = sort_models_newest_first(&[j(r#"{"name": "a", "original_creation_time": 5}"#)])
        .expect_err("rejected");
    assert_eq!(error, "original_creation_time must be a string");
}

#[test]
fn filters_applied_prints_in_first_seen_order() {
    let built = constraints(&[("size", "a"), ("task", "b"), ("size", "c")]);
    assert_eq!(
        dumps(&crate::ai_models::filter::constraints_json(&built)),
        r#"{"size": ["a", "c"], "task": ["b"]}"#
    );
}
