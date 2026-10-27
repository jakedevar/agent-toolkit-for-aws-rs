use toolkit_core::Json;

use super::{dumps, j, summary};
use crate::ai_models::normalize::{
    list_available_values, normalize_hub_content, normalize_size_bucket,
};

#[test]
fn size_buckets_follow_the_documented_rule() {
    // (raw value, bucket): exact counts, the Hub's range labels (classified by
    // their upper bound), open-ended labels and unparseable input.
    let cases = [
        ("7b", "<=10b"),
        ("10b", "<=10b"),
        ("10.5b", "11b-70b"),
        ("11b", "11b-70b"),
        ("70b", "11b-70b"),
        ("71b", "71b-100b"),
        ("100b", "71b-100b"),
        ("101b", ">100b"),
        ("550b", ">100b"),
        ("<1b", "<=10b"),
        ("1b-10b", "<=10b"),
        ("10b-70b", "11b-70b"),
        ("70b-100b", "71b-100b"),
        (">100b", ">100b"),
        (">1b", ">100b"),
        ("  7B  ", "<=10b"),
        ("7bb", "<=10b"),
        ("1.5b", "<=10b"),
        ("12", "11b-70b"),
        ("unknown", "unknown"),
        ("UNKNOWN", "unknown"),
        ("", "unknown"),
        ("   ", "unknown"),
        ("banana", "unknown"),
        ("<", "unknown"),
        ("1b-", "unknown"),
        ("1b-x", "unknown"),
        ("-10b", "<=10b"),
        ("1-2-3", "unknown"),
    ];
    for (raw, bucket) in cases {
        assert_eq!(normalize_size_bucket(Some(raw)), bucket, "{raw:?}");
    }
    assert_eq!(normalize_size_bucket(None), "unknown");
}

#[test]
fn a_record_is_normalized_in_the_python_key_order() {
    let record = summary(
        "m1",
        &[
            "@data-type:text",
            "@input-modality:text",
            "@input-modality:image",
            "@output-modality:text",
            "@model-size:7B",
            "@license:Apache 2.0",
            "@language:en",
            "@language:fr",
            "@context-window:128k",
            "@model-type:LLM",
            "@framework:pytorch",
            "@provider:meta",
            "@task:text generation",
            "@capability:bedrock_console",
            "@capability:customization",
            "@unrelated:x",
        ],
        Some("2024-01-02T03:04:05+00:00"),
    );
    assert_eq!(
        dumps(&normalize_hub_content(&record).expect("normalizes")),
        concat!(
            r#"{"name": "m1", "data_types": ["text"], "input_modalities": ["text", "image"], "#,
            r#""output_modalities": ["text"], "size_raw": "7B", "size": "<=10b", "#,
            r#""license": "Apache 2.0", "languages": ["en", "fr"], "context_window": "128k", "#,
            r#""model_type": "LLM", "framework": "pytorch", "provider": "meta", "#,
            r#""tasks": ["text generation"], "bedrock_eligible": true, "#,
            r#""customization_eligible": true, "#,
            r#""original_creation_time": "2024-01-02T03:04:05+00:00"}"#
        )
    );
}

#[test]
fn a_bare_record_has_only_a_name_and_the_two_flags() {
    let normalized = normalize_hub_content(&j(r#"{"HubContentName": "x"}"#)).expect("ok");
    assert_eq!(
        dumps(&normalized),
        r#"{"name": "x", "bedrock_eligible": false, "customization_eligible": false}"#
    );
    // A record without a name keeps the field, as null.
    let nameless = normalize_hub_content(&j("{}")).expect("ok");
    assert_eq!(nameless.get("name"), Some(&Json::Null));
}

#[test]
fn a_repeated_scalar_keyword_keeps_its_position_and_the_last_value() {
    let record = summary("m", &["@license:a", "@provider:p", "@license:b"], None);
    assert_eq!(
        dumps(&normalize_hub_content(&record).expect("ok")),
        r#"{"name": "m", "license": "b", "provider": "p", "bedrock_eligible": false, "customization_eligible": false}"#
    );
}

#[test]
fn the_document_fills_what_the_keyword_cap_dropped() {
    let document = r#"{"HubContentName": "m", "HubContentSearchKeywords": [],
        "HubContentDocument": "{\"ModelSize\": \"7B\", \"ContextWindow\": [\"\", \"128K\"]}"}"#;
    assert_eq!(
        dumps(&normalize_hub_content(&j(document)).expect("ok")),
        concat!(
            r#"{"name": "m", "bedrock_eligible": false, "customization_eligible": false, "#,
            r#""size_raw": "7b", "size": "<=10b", "context_window": "128k"}"#
        )
    );
}

#[test]
fn a_keyword_is_authoritative_over_the_document() {
    let document = r#"{"HubContentName": "m", "HubContentSearchKeywords": ["@model-size:30b"],
        "HubContentDocument": "{\"ModelSize\": \"7B\"}"}"#;
    let normalized = normalize_hub_content(&j(document)).expect("ok");
    assert_eq!(normalized.get("size_raw"), Some(&Json::string("30b")));
    assert_eq!(normalized.get("size"), Some(&Json::string("11b-70b")));
}

#[test]
fn an_unusable_document_is_ignored() {
    for document in [
        "\"\"",
        "\"not json\"",
        "\"[1, 2]\"",
        "\"{\\\"ModelSize\\\": 7}\"",
        "\"{\\\"ModelSize\\\": [\\\"\\\", 3]}\"",
        "42",
    ] {
        let record = format!(r#"{{"HubContentName": "m", "HubContentDocument": {document}}}"#);
        let normalized = normalize_hub_content(&j(&record)).expect("ok");
        assert_eq!(normalized.get("size"), None, "{document}");
    }
}

#[test]
fn creation_times_are_text_and_epochs_are_utc_isoformat() {
    let time = |value: &str| {
        let record = format!(r#"{{"HubContentName": "m", "OriginalCreationTime": {value}}}"#);
        normalize_hub_content(&j(&record))
            .expect("ok")
            .get("original_creation_time")
            .cloned()
    };
    assert_eq!(time("\"2024-01-02\""), Some(Json::string("2024-01-02")));
    assert_eq!(
        time("1704164645"),
        Some(Json::string("2024-01-02T03:04:05+00:00"))
    );
    assert_eq!(
        time("1704164645.5"),
        Some(Json::string("2024-01-02T03:04:05.500000+00:00"))
    );
    for falsy in ["null", "0", "0.0", "\"\"", "false"] {
        assert_eq!(time(falsy), None, "{falsy}");
    }
}

#[test]
fn malformed_keywords_are_an_error_not_a_guess() {
    for record in [
        r#"{"HubContentSearchKeywords": "task"}"#,
        r#"{"HubContentSearchKeywords": [1]}"#,
        r#"{"HubContentSearchKeywords": {"a": "b"}}"#,
    ] {
        let error = normalize_hub_content(&j(record)).expect_err("rejected");
        assert!(error.contains("HubContentSearchKeywords"), "{error}");
    }
}

#[test]
fn available_values_are_distinct_and_sorted() {
    let models = [
        j(
            r#"{"name": "a", "tasks": ["t2", "t1"], "size": "<=10b", "license": "MIT", "languages": ["en"],
              "context_window": "8k", "model_type": "LLM", "provider": "p", "framework": "f",
              "data_types": ["text"], "input_modalities": ["text"], "output_modalities": ["text"]}"#,
        ),
        j(
            r#"{"name": "b", "tasks": ["t1"], "size": "11b-70b", "license": "Apache 2.0",
              "languages": ["de", "en"], "provider": "p"}"#,
        ),
    ];
    assert_eq!(
        dumps(&list_available_values(&models).expect("ok")),
        concat!(
            r#"{"tasks": ["t1", "t2"], "data_types": ["text"], "input_modalities": ["text"], "#,
            r#""output_modalities": ["text"], "sizes": ["11b-70b", "<=10b"], "#,
            r#""licenses": ["Apache 2.0", "MIT"], "languages": ["de", "en"], "#,
            r#""context_windows": ["8k"], "model_types": ["LLM"], "providers": ["p"], "#,
            r#""frameworks": ["f"]}"#
        )
    );
    let empty = list_available_values(&[]).expect("ok");
    assert!(dumps(&empty).starts_with(r#"{"tasks": [], "data_types": []"#));
}

#[test]
fn a_mistyped_member_names_the_model_not_its_value() {
    let error = list_available_values(&[j(r#"{"name": "a"}"#), j(r#"{"tasks": "FAKE-SECRET"}"#)])
        .expect_err("rejected");
    assert_eq!(error, "model 1: tasks must be a list of strings");
}
