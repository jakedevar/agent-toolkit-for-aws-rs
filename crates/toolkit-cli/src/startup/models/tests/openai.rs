//! `test_openai_model_recommendation.py` and the OpenAI-engine rules.

use toolkit_core::Json;

use super::{
    array_len, codes, entry, get, has_code, input_with, is_null, merged, openai_catalog,
    recommend_input, strings, text,
};
use crate::startup::models::openai::{
    converse_candidate_order, converse_tier_for_source, detect_family, same_model_catalog_key,
    SOURCE_MODEL_KEYS,
};
use crate::startup::models::request::Input;

const BASE: &str = r#"{
  "workload_id": "chat-svc",
  "source": {"provider": "openai", "model_ids": ["gpt-5.4"], "sdk": "openai",
             "api_surface": "responses", "source_paths": ["src/app.py"]},
  "requirements": {"priority": "balanced", "critical_features": []},
  "detected_features": []
}"#;

fn workload(overrides: &str) -> Json {
    merged(BASE, overrides)
}

fn rec_in(overrides: &str, region: &str) -> Json {
    let result =
        recommend_input(&input_with(&workload(overrides), region), true).expect("a recommendation");
    get(get(&result, "workloads"), "chat-svc").clone()
}

fn run(overrides: &str) -> Json {
    rec_in(overrides, "us-east-2")
}

fn source_ids(id: &str, surface: &str) -> String {
    format!(r#""source": {{"model_ids": ["{id}"], "api_surface": "{surface}"}}"#)
}

fn paths(rec: &Json, key: &str) -> Vec<String> {
    let Json::Array(items) = get(rec, key) else {
        panic!("{key}");
    };
    let mut found: Vec<String> = items
        .iter()
        .map(|item| text(item, "api_path").to_string())
        .collect();
    found.sort();
    found
}

fn delta_description<'a>(rec: &'a Json, code: &str) -> &'a str {
    text(entry(rec, "migration_deltas", code), "description")
}

const GPT6: [(&str, &str); 4] = [
    ("gpt-6-astra", "openai.gpt-6-astra"),
    ("gpt-6-sol", "openai.gpt-6-sol"),
    ("gpt-6-luna", "openai.gpt-6-luna"),
    ("gpt-6.1-sol", "openai.gpt-6.1-sol"),
];

// --- Provider and path selection --------------------------------------------

#[test]
fn responses_source_with_continuity_recommends_mantle_responses() {
    let rec = run(r#"{"requirements": {"api_continuity": "required"}}"#);
    assert_eq!(text(&rec, "provider_module"), "openai");
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(text(&rec, "api_path"), "mantle_openai_responses");
    // GPT-5.4 has no exact-source key, so the catalog order decides: the first
    // Mantle Responses model is GPT-5.6 Sol.
    assert_eq!(text(&rec, "primary_model"), "openai.gpt-5.6-sol");
}

#[test]
fn chat_completions_gpt5_target_selects_responses_not_chat() {
    let rec = run(&format!(
        r#"{{{}, "requirements": {{"api_continuity": "required"}}}}"#,
        source_ids("gpt-5.4", "chat_completions")
    ));
    assert_eq!(text(&rec, "api_path"), "mantle_openai_responses");
    assert!(has_code(
        &rec,
        "blocks",
        "chat_completions_to_responses_required"
    ));
    let codes = codes(&rec, "migration_deltas");
    for code in [
        "chat_to_responses_request",
        "chat_to_responses_response",
        "chat_to_responses_tools",
        "chat_to_responses_state",
    ] {
        assert!(codes.iter().any(|known| known == code), "{code}");
    }
}

#[test]
fn governance_selects_runtime_converse() {
    let rec = run(r#"{"requirements": {"governance": ["guardrails", "invocation_logging"]}}"#);
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(text(&rec, "api_path"), "runtime_converse");
    // GPT-5.4 has no runtime path: the balanced Claude tier is the fallback.
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-sonnet-5");
    let identity = get(&rec, "model_identity");
    assert_eq!(text(identity, "version"), "5");
    assert_eq!(
        text(identity, "display_name"),
        "Claude Sonnet 5 (Converse tier match for GPT-5.6 Terra / 5.5 / 5.4)"
    );
    // A Bedrock-native target is a model change by definition.
    let analysis = get(&rec, "source_analysis");
    assert_eq!(text(analysis, "target_version"), "5");
    assert_eq!(get(analysis, "version_changed"), &Json::Bool(true));
    assert_eq!(get(analysis, "model_generation_changes"), &Json::Bool(true));
    assert!(delta_description(&rec, "openai_sdk_to_converse").contains("also changes families"));
}

#[test]
fn continuity_plus_runtime_only_returns_decision_required() {
    let rec =
        run(r#"{"requirements": {"api_continuity": "required", "governance": ["guardrails"]}}"#);
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(is_null(&rec, "primary_model"));
    assert!(is_null(&rec, "api_path"));
    assert_eq!(
        paths(&rec, "decision_options"),
        ["mantle_openai_responses", "runtime_converse"]
    );
    assert!(has_code(&rec, "blocks", "model_path_decision_required"));
    let Json::Array(options) = get(&rec, "decision_options") else {
        panic!("options");
    };
    let reason = |path: &str| {
        text(
            options
                .iter()
                .find(|option| text(option, "api_path") == path)
                .expect("option"),
            "reason",
        )
    };
    assert!(reason("mantle_openai_responses").contains("OpenAI SDK"));
    assert!(reason("runtime_converse").contains("Bedrock-native"));
    let verification = get(&rec, "verification");
    assert_eq!(text(verification, "probe_status"), "not_applicable");
    assert_eq!(text(verification, "availability_claim"), "not_selected");
    assert!(is_null(
        get(&rec, "source_analysis"),
        "model_generation_changes"
    ));
}

// --- GPT-6 exact-model routing -----------------------------------------------

#[test]
fn gpt6_supported_region_selects_exact_model() {
    for (source, target) in GPT6 {
        let critical = if matches!(source, "gpt-6-sol" | "gpt-6-luna") {
            r#", "critical_features": ["image_input_vision"]"#
        } else {
            ""
        };
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"api_continuity": "required"{critical}}}}}"#,
                source_ids(source, "responses")
            ),
            "us-east-1",
        );
        assert_eq!(detect_family(source), "reasoning");
        assert_eq!(text(&rec, "decision_status"), "recommended", "{source}");
        assert_eq!(text(&rec, "primary_model"), target);
        assert_eq!(text(&rec, "api_path"), "mantle_openai_responses");
        if !critical.is_empty() {
            assert!(strings(get(&rec, "compatibility"), "native")
                .contains(&"image_input_vision".to_string()));
        }
    }
}

#[test]
fn gpt6_governance_selects_exact_runtime_model_and_cris_id() {
    for (source, target) in GPT6 {
        for (residency, extra, prefix) in [
            ("global_allowed", "", "global."),
            ("geo_required", r#", "cris_geography": "us""#, "us."),
        ] {
            let rec = run(&format!(
                r#"{{{}, "requirements": {{"governance": ["guardrails"], "data_residency": "{residency}"{extra}}}}}"#,
                source_ids(source, "responses")
            ));
            assert_eq!(
                text(&rec, "decision_status"),
                "recommended",
                "{source} {residency}"
            );
            assert_eq!(text(&rec, "primary_model"), target);
            assert_eq!(text(&rec, "api_path"), "runtime_converse");
            assert_eq!(
                text(&rec, "invocation_model_id"),
                format!("{prefix}{target}")
            );
            let description = delta_description(&rec, "openai_sdk_to_converse");
            assert!(description.contains("identity is preserved"));
            assert!(!description.contains("changes families"));
        }
    }
}

#[test]
fn gpt6_unsupported_geo_profile_falls_back_without_fabricated_id() {
    for (source, region, geography, unsupported) in [
        ("gpt-6-sol", "eu-west-1", "eu", "eu.openai.gpt-6-sol"),
        (
            "gpt-6.1-sol",
            "ap-southeast-2",
            "au",
            "au.openai.gpt-6.1-sol",
        ),
    ] {
        let requirements = format!(
            r#""governance": ["guardrails"], "data_residency": "geo_required", "cris_geography": "{geography}""#
        );
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{{requirements}}}}}"#,
                source_ids(source, "responses")
            ),
            region,
        );
        assert_eq!(text(&rec, "decision_status"), "recommended");
        assert_ne!(
            rec.get("invocation_model_id").and_then(Json::as_str),
            Some(unsupported)
        );
        let conflict = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"api_continuity": "required", {requirements}}}}}"#,
                source_ids(source, "chat_completions")
            ),
            region,
        );
        assert_eq!(text(&conflict, "decision_status"), "decision_required");
        let Json::Array(options) = get(&conflict, "decision_options") else {
            panic!("options");
        };
        assert!(options.iter().all(|option| option
            .get("invocation_model_id")
            .and_then(Json::as_str)
            != Some(unsupported)));
    }
}

#[test]
fn the_catalog_carries_gpt6_profile_metadata() {
    let catalog = openai_catalog();
    assert_eq!(catalog.verified_at, "2026-09-24");
    for (identity, key) in SOURCE_MODEL_KEYS {
        if !identity.starts_with("gpt-6") || identity == "gpt-6-astra" {
            continue;
        }
        let model = catalog.require(key).expect("model");
        let mut names: Vec<&str> = model
            .inference_profiles
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["global", "us"], "{key}");
    }
}

#[test]
fn gpt6_chat_completions_preserves_chat_surface() {
    for (source, target) in GPT6 {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"api_continuity": "required"}}}}"#,
                source_ids(source, "chat_completions")
            ),
            "us-east-1",
        );
        assert_eq!(text(&rec, "primary_model"), target, "{source}");
        assert_eq!(text(&rec, "api_path"), "mantle_openai_chat");
        assert!(!has_code(
            &rec,
            "blocks",
            "chat_completions_to_responses_required"
        ));
    }
}

#[test]
fn gpt6_mantle_skips_exact_model_outside_cataloged_region() {
    let rec = rec_in(
        &format!(
            r#"{{{}, "requirements": {{"api_continuity": "required"}}}}"#,
            source_ids("gpt-6-sol", "responses")
        ),
        "us-west-2",
    );
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_ne!(text(&rec, "primary_model"), "openai.gpt-6-sol");
}

#[test]
fn gpt6_chat_outside_every_mantle_footprint_returns_blocking_decision() {
    let rec = rec_in(
        &format!(
            r#"{{{}, "requirements": {{"api_continuity": "required"}}}}"#,
            source_ids("gpt-6-sol", "chat_completions")
        ),
        "eu-west-1",
    );
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(is_null(&rec, "primary_model"));
    assert!(has_code(&rec, "blocks", "mantle_region_unavailable"));
    assert!(text(&get_first(&rec, "blocks"), "message").contains("eu-west-1"));
    assert_eq!(
        paths(&rec, "decision_options"),
        ["mantle_openai_chat", "runtime_converse"]
    );
    let Json::Array(options) = get(&rec, "decision_options") else {
        panic!("options");
    };
    let mantle = options
        .iter()
        .find(|option| text(option, "api_path") == "mantle_openai_chat")
        .expect("mantle option");
    assert!(text(mantle, "reason").contains("Chat Completions surface"));
    assert!(text(mantle, "reason").contains("Requires changing from eu-west-1"));
}

fn get_first(value: &Json, key: &str) -> Json {
    match get(value, key) {
        Json::Array(items) => items.first().cloned().expect("a first entry"),
        other => panic!("{key} is a {}", other.type_name()),
    }
}

#[test]
fn gpt6_chat_findings_preserve_chat_request_shapes() {
    let rec = rec_in(
        &format!(
            r#"{{{}, "requirements": {{"api_continuity": "required", "uses_n": true,
                "uses_hosted_state": true}},
                "detected_features": ["tool_or_function_calling", "conversation_state"]}}"#,
            source_ids("gpt-6-sol", "chat_completions")
        ),
        "us-east-1",
    );
    assert_eq!(text(&rec, "api_path"), "mantle_openai_chat");
    assert!(!has_code(&rec, "migration_deltas", "responses_no_n"));
    let tool = delta_description(&rec, "tool_result_shape");
    assert!(tool.contains("role: tool") && tool.contains("tool_call_id"));
    assert!(!tool.contains("function_call_output"));
    let state = delta_description(&rec, "conversation_state_ownership");
    assert!(state.contains("application-owned message history"));
    assert!(!state.contains("previous_response_id"));
}

#[test]
fn runtime_unresolved_branches_outside_mantle_region_keep_two_options() {
    for requirements in [
        r#"{"api_continuity": "preferred", "governance": ["guardrails"], "critical_features": ["streaming"]}"#,
        r#"{"api_continuity": "preferred", "governance": ["guardrails"], "expected_output_tokens": 10000000}"#,
    ] {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {requirements}}}"#,
                source_ids("gpt-6-sol", "chat_completions")
            ),
            "eu-west-1",
        );
        assert_eq!(
            text(&rec, "decision_status"),
            "decision_required",
            "{requirements}"
        );
        assert_eq!(
            paths(&rec, "decision_options"),
            ["mantle_openai_chat", "runtime_converse"]
        );
    }
    let rec = rec_in(
        &format!(
            r#"{{{}, "requirements": {{"api_continuity": "required", "critical_features": ["streaming"]}},
                "feature_status": {{"streaming": "unknown"}}}}"#,
            source_ids("gpt-6-sol", "chat_completions")
        ),
        "eu-west-1",
    );
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(array_len(&rec, "decision_options") >= 2);
    assert!(has_code(&rec, "blocks", "feature_scan_incomplete"));
}

// --- Model-generation analysis -------------------------------------------------

#[test]
fn gpt4_to_gpt5_emits_model_generation_finding() {
    let rec = run(&format!(
        r#"{{{}, "requirements": {{"api_continuity": "required"}}}}"#,
        source_ids("gpt-4o", "chat_completions")
    ));
    let analysis = get(&rec, "source_analysis");
    assert_eq!(text(analysis, "source_family"), "legacy");
    assert_eq!(get(analysis, "model_generation_changes"), &Json::Bool(true));
    assert_eq!(get(analysis, "version_changed"), &Json::Bool(true));
    assert!(delta_description(&rec, "model_generation_hop").contains("'gpt-4o'"));
}

#[test]
fn source_families() {
    for (id, family) in [
        ("o3-mini", "reasoning"),
        ("o1", "reasoning"),
        ("gpt-5.4", "reasoning"),
        ("GPT_6", "reasoning"),
        ("gpt-4o", "legacy"),
        ("gpt-4-32k", "legacy"),
        ("gpt-3.5-turbo", "legacy"),
        ("gpt-35-turbo", "legacy"),
        ("gpt-4.1", "legacy"),
        ("prod-default", "unknown"),
        ("", "unknown"),
        ("  ", "unknown"),
        ("o5", "unknown"),
        ("echo1", "unknown"),
    ] {
        assert_eq!(detect_family(id), family, "{id:?}");
    }
    let rec = run(&source_ids_object("o3-mini"));
    let analysis = get(&rec, "source_analysis");
    assert_eq!(text(analysis, "source_family"), "reasoning");
    assert_eq!(
        get(analysis, "model_generation_changes"),
        &Json::Bool(false)
    );
    let rec = rec_opaque();
    assert_eq!(
        text(get(&rec, "source_analysis"), "source_family"),
        "unknown"
    );
    assert!(is_null(get(&rec, "source_analysis"), "version_changed"));
}

fn source_ids_object(id: &str) -> String {
    format!(r#"{{"source": {{"model_ids": ["{id}"]}}}}"#)
}

fn rec_opaque() -> Json {
    run(&source_ids_object("prod-default"))
}

#[test]
fn reasoning_workload_requires_output_headroom() {
    let rec = run("{}");
    let finding = entry(&rec, "tuning", "reasoning_token_headroom");
    let remediation = text(finding, "remediation");
    assert!(remediation.contains("2.5x"));
    assert!(remediation.contains("STARTING heuristic"));
}

// --- Parameter and API behavior -----------------------------------------------

#[test]
fn sampling_is_target_derived_not_source_derived() {
    for id in ["gpt-5.1", "gpt-5.4"] {
        let rec = run(&source_ids_object(id));
        assert!(has_code(&rec, "tuning", "sampling_params_accepted"), "{id}");
        assert!(
            has_code(&rec, "tuning", "sampling_penalties_rejected"),
            "{id}"
        );
        assert!(
            !has_code(&rec, "blocks", "sampling_parameters_removed"),
            "{id}"
        );
    }
    let rec = run("{}");
    let finding = entry(&rec, "tuning", "sampling_penalties_rejected");
    let message = text(finding, "message");
    assert!(message.contains("frequency_penalty") && message.contains("presence_penalty"));
    assert!(!message.contains("logprobs") && !message.contains("stop"));
    assert!(text(finding, "remediation")
        .to_lowercase()
        .contains("verify"));
}

#[test]
fn deltas_follow_detected_features_and_requirements() {
    assert!(has_code(
        &run(r#"{"requirements": {"uses_n": true}}"#),
        "migration_deltas",
        "responses_no_n"
    ));
    assert!(has_code(
        &run(r#"{"detected_features": ["tool_or_function_calling"]}"#),
        "migration_deltas",
        "tool_result_shape"
    ));
    let structured = run(r#"{"detected_features": ["structured_output_json"]}"#);
    let description = delta_description(&structured, "structured_output_text_format");
    assert!(description.contains("responses.parse"));
    assert!(!description.to_lowercase().contains("no direct"));
    let state = run(r#"{"requirements": {"uses_hosted_state": true}}"#);
    let description = delta_description(&state, "conversation_state_ownership");
    assert!(description.contains("store=True") && description.contains("store=False"));
}

#[test]
fn hosted_tools_and_modalities_are_architecture_impacts() {
    let rec = run(r#"{"detected_features": ["web_search"]}"#);
    let Json::Array(impacts) = get(&rec, "architecture_impacts") else {
        panic!("impacts");
    };
    assert_eq!(text(&impacts[0], "feature"), "web_search");
    assert!(text(&impacts[0], "impact")
        .to_lowercase()
        .contains("passthrough"));

    let rec =
        run(r#"{"detected_features": ["file_search_retrieval", "files_api", "vector_stores"]}"#);
    let features: Vec<String> = match get(&rec, "architecture_impacts") {
        Json::Array(items) => items
            .iter()
            .map(|item| text(item, "feature").to_string())
            .collect(),
        _ => panic!("impacts"),
    };
    assert_eq!(
        features,
        ["file_search_retrieval", "files_api", "vector_stores"]
    );
    assert_eq!(
        strings(get(&rec, "compatibility"), "rearchitecture"),
        ["file_search_retrieval", "files_api", "vector_stores"]
    );
    assert_eq!(text(get(&rec, "evaluation"), "mode"), "trajectory");

    let rec = run(r#"{"detected_features": ["audio_modality", "embeddings", "images"]}"#);
    let Json::Array(targets) = get(&rec, "additional_targets") else {
        panic!("targets");
    };
    let capabilities: Vec<&str> = targets.iter().map(|t| text(t, "capability")).collect();
    assert_eq!(capabilities, ["audio_modality", "embeddings", "images"]);
    for target in targets {
        assert_eq!(text(target, "status"), "unresolved");
        assert!(is_null(target, "candidate"));
        assert!(!text(target, "service").is_empty());
    }
    let none = rec_none();
    assert_eq!(array_len(&none, "additional_targets"), 0);
    assert_eq!(text(get(&none, "evaluation"), "mode"), "prompt");
}

fn rec_none() -> Json {
    run(r#"{"detected_features": ["max_tokens"]}"#)
}

// --- Catalog and verification ---------------------------------------------------

#[test]
fn numeric_requirements_fail_closed() {
    let rec = run(r#"{"requirements": {"min_context_tokens": 2000000}}"#);
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(has_code(&rec, "blocks", "unverified_capacity"));
    // The output ceiling is unknown for every Mantle candidate: an unknown limit
    // cannot satisfy a hard need.
    let rec = run(r#"{"requirements": {"expected_output_tokens": 100000}}"#);
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    let message = text(entry(&rec, "blocks", "unverified_capacity"), "message");
    assert!(message.contains("is unknown in the catalog"), "{message}");
    assert!(message.contains("a required 100000-token need cannot be confirmed"));
}

#[test]
fn cris_only_path_with_unresolvable_residency_blocks_explicitly() {
    let rec = run(&format!(
        r#"{{{}, "requirements": {{"governance": ["guardrails"], "data_residency": "unknown"}}}}"#,
        source_ids("gpt-5.6-terra", "responses")
    ));
    assert!(has_code(&rec, "blocks", "cris_residency_unresolved"));
    assert!(is_null(&rec, "invocation_model_id"));
    assert_eq!(
        get(get(&rec, "model_identity"), "requires_cris"),
        &Json::Bool(true)
    );
}

#[test]
fn verification_is_provisional_until_probed() {
    let rec = run("{}");
    let verification = get(&rec, "verification");
    assert_eq!(text(verification, "probe_status"), "not_run");
    assert_eq!(text(verification, "availability_claim"), "provisional");
    assert_eq!(strings(verification, "required_checks").len(), 2);
    let rec = run(
        r#"{"requirements": {"governance": ["guardrails"], "data_residency": "global_allowed"}}"#,
    );
    assert_eq!(
        strings(get(&rec, "verification"), "required_checks").len(),
        3
    );
}

#[test]
fn catalog_limits_are_sourced_or_unknown() {
    for (key, model) in &openai_catalog().models {
        let _ = model.context_window.json();
        let _ = model.output_token_ceiling.json();
        assert!(!model.display_name.is_empty(), "{key}");
    }
}

// --- Gap-review regressions -----------------------------------------------------

#[test]
fn same_version_source_and_target_is_not_changed() {
    let rec = run(&source_ids_object("gpt-5.6"));
    let analysis = get(&rec, "source_analysis");
    assert_eq!(text(analysis, "target_version"), "5.6");
    assert_eq!(get(analysis, "version_changed"), &Json::Bool(false));
}

#[test]
fn runtime_selection_fails_closed_on_unproven_capability() {
    let rec = run(
        r#"{"requirements": {"governance": ["guardrails"], "critical_features": ["streaming"]}}"#,
    );
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(has_code(&rec, "blocks", "unverified_capability"));
    assert!(text(&get_first(&rec, "blocks"), "message").contains("streaming"));
}

#[test]
fn native_only_lists_catalog_evidenced_features() {
    let rec =
        run(r#"{"detected_features": ["tool_or_function_calling", "structured_output_json"]}"#);
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(text(&rec, "primary_model"), "openai.gpt-5.4");
    assert_eq!(
        strings(get(&rec, "compatibility"), "native"),
        ["structured_output_json", "tool_or_function_calling"]
    );
    assert_eq!(array_len(get(&rec, "compatibility"), "unsupported"), 0);
    let vision = run(r#"{"requirements": {"critical_features": ["image_input_vision"]}}"#);
    assert_eq!(text(&vision, "primary_model"), "openai.gpt-5.6-sol");
}

#[test]
fn unknown_required_feature_forces_decision_required() {
    let rec = run(
        r#"{"requirements": {"critical_features": ["structured_output_json"]},
            "feature_status": {"structured_output_json": "unknown"}}"#,
    );
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(is_null(&rec, "primary_model"));
    assert_eq!(
        text(entry(&rec, "blocks", "feature_scan_incomplete"), "message"),
        "Required features are unresolved (status unknown): structured_output_json"
    );
}

#[test]
fn the_feature_assessment_merges_detected_and_status() {
    let rec = run(
        r#"{"detected_features": ["tool_or_function_calling"], "feature_status": {"web_search": "absent"}}"#,
    );
    let assessment = get(&rec, "feature_assessment");
    assert_eq!(text(assessment, "tool_or_function_calling"), "detected");
    assert_eq!(text(assessment, "web_search"), "absent");
    // An explicit absent beats the detected list; a status-only detection flows.
    let rec = run(
        r#"{"feature_status": {"structured_output_json": "absent", "tool_or_function_calling": "detected"},
            "detected_features": ["structured_output_json"]}"#,
    );
    assert!(!strings(get(&rec, "compatibility"), "native")
        .contains(&"structured_output_json".to_string()));
    assert!(!has_code(
        &rec,
        "migration_deltas",
        "structured_output_text_format"
    ));
    assert!(strings(get(&rec, "compatibility"), "native")
        .contains(&"tool_or_function_calling".to_string()));
    assert!(has_code(&rec, "migration_deltas", "tool_result_shape"));
}

// --- Converse tier mapping --------------------------------------------------------

fn parsed_source(id: &str) -> crate::startup::models::request::Source {
    Input::from_json(&input_with(&workload(&source_ids_object(id)), "us-east-1"))
        .expect("input")
        .workloads
        .remove(0)
        .source
}

#[test]
fn the_converse_candidate_order_keeps_the_tier_map() {
    let order = |id: &str| converse_candidate_order(&parsed_source(id), None);
    assert_eq!(
        order("gpt-5.6-sol")[..2],
        ["openai_gpt_5_6_sol", "anthropic_claude_opus_5_5"]
    );
    assert_eq!(
        order("gpt-5.6-luna")[..2],
        ["openai_gpt_5_6_luna", "anthropic_claude_haiku_4_5"]
    );
    assert_eq!(
        order("gpt-5.4"),
        [
            "anthropic_claude_sonnet_5",
            "anthropic_claude_opus_5_5",
            "anthropic_claude_haiku_4_5"
        ]
    );
    assert_eq!(
        converse_tier_for_source(&parsed_source("gpt-6-astra")),
        "anthropic_claude_opus_5_5"
    );
    let quality = Input::from_json(&input_with(
        &workload(r#"{"requirements": {"priority": "quality"}}"#),
        "us-east-1",
    ))
    .expect("input");
    assert_eq!(
        converse_candidate_order(
            &quality.workloads[0].source,
            Some(&quality.workloads[0].requirements)
        )[0],
        "anthropic_claude_opus_5_5"
    );
}

#[test]
fn same_model_governance_keeps_the_source_model() {
    for (id, expected) in [
        ("gpt-5.6-sol", "openai.gpt-5.6-sol"),
        ("gpt-5.6-luna", "openai.gpt-5.6-luna"),
        ("gpt-5.6-terra", "openai.gpt-5.6-terra"),
    ] {
        let rec = run(&format!(
            r#"{{{}, "requirements": {{"governance": ["guardrails"], "critical_features": ["reasoning"],
                "data_residency": "global_allowed"}}}}"#,
            source_ids(id, "responses")
        ));
        assert_eq!(text(&rec, "primary_model"), expected, "{id}");
        assert_eq!(text(&rec, "api_path"), "runtime_converse");
    }
    // GPT-5.5 has no runtime path: its governance target is the balanced tier.
    let rec = run(&format!(
        r#"{{{}, "requirements": {{"governance": ["guardrails"]}}}}"#,
        source_ids("gpt-5.5", "responses")
    ));
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-sonnet-5");
}

#[test]
fn source_identities_need_word_boundaries() {
    let key = |id: &str| same_model_catalog_key(&parsed_source(id));
    assert_eq!(key("GPT_6_Sol"), Some("openai_gpt_6_sol"));
    assert_eq!(key("openai.gpt-6.1-sol"), Some("openai_gpt_6_1_sol"));
    assert_eq!(key("azure/gpt-6-astra-preview"), Some("openai_gpt_6_astra"));
    assert_eq!(key("gpt-6-solar"), None);
    assert_eq!(key("xgpt-6-sol"), None);
    assert_eq!(key("gpt-6-sol2"), None);
    assert_eq!(key("gpt-5.4"), None);
    assert_eq!(key("gpt 6 luna"), Some("openai_gpt_6_luna"));
}

// --- GPT-6 Astra ------------------------------------------------------------------

#[test]
fn astra_keeps_source_model_and_api_on_mantle() {
    for (surface, path) in [
        ("responses", "mantle_openai_responses"),
        ("chat_completions", "mantle_openai_chat"),
    ] {
        for region in ["us-east-1", "us-west-2"] {
            let rec = rec_in(
                &format!("{{{}}}", source_ids("gpt-6-astra", surface)),
                region,
            );
            assert_eq!(
                text(&rec, "decision_status"),
                "recommended",
                "{surface} {region}"
            );
            assert_eq!(text(&rec, "primary_model"), "openai.gpt-6-astra");
            assert_eq!(text(&rec, "api_path"), path);
            let analysis = get(&rec, "source_analysis");
            assert_eq!(text(analysis, "source_family"), "reasoning");
            assert_eq!(get(analysis, "version_changed"), &Json::Bool(false));
            assert!(!has_code(
                &rec,
                "blocks",
                "chat_completions_to_responses_required"
            ));
            assert!(!has_code(&rec, "tuning", "sampling_params_accepted"));
            assert!(has_code(&rec, "tuning", "sampling_params_unverified"));
        }
    }
}

#[test]
fn astra_mantle_outside_supported_regions_requires_decision() {
    for region in ["us-east-2", "eu-west-1"] {
        for surface in ["responses", "chat_completions"] {
            let rec = rec_in(
                &format!("{{{}}}", source_ids("gpt-6-astra", surface)),
                region,
            );
            assert_eq!(text(&rec, "decision_status"), "decision_required");
            assert!(is_null(&rec, "primary_model"));
            assert!(has_code(&rec, "blocks", "mantle_region_unavailable"));
        }
    }
}

#[test]
fn astra_explicit_api_preference_reaches_validated_output() {
    for (surface, preferred, expected, delta) in [
        (
            "chat_completions",
            "mantle_openai_responses",
            "mantle_openai_responses",
            Some("chat_to_responses_request"),
        ),
        (
            "chat_completions",
            "mantle_openai_chat",
            "mantle_openai_chat",
            None,
        ),
        (
            "responses",
            "mantle_openai_chat",
            "mantle_openai_chat",
            Some("responses_to_chat_request"),
        ),
        (
            "responses",
            "mantle_openai_responses",
            "mantle_openai_responses",
            None,
        ),
        (
            "chat_completions",
            "runtime_converse",
            "runtime_converse",
            Some("openai_sdk_to_converse"),
        ),
    ] {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"preferred_api_path": "{preferred}", "data_residency": "global_allowed"}}}}"#,
                source_ids("gpt-6-astra", surface)
            ),
            "us-west-2",
        );
        assert_eq!(
            text(&rec, "decision_status"),
            "recommended",
            "{surface} {preferred}"
        );
        assert_eq!(text(&rec, "api_path"), expected);
        assert_eq!(text(&rec, "primary_model"), "openai.gpt-6-astra");
        if let Some(delta) = delta {
            assert!(has_code(&rec, "migration_deltas", delta), "{delta}");
        }
        let Json::Array(blocks) = get(&rec, "blocks") else {
            panic!("blocks");
        };
        assert!(blocks
            .iter()
            .all(|block| !text(block, "message").contains("GPT-5.x")));
    }
}

#[test]
fn astra_api_preference_conflicts_require_an_explicit_decision() {
    for (preferred, constraints) in [
        ("mantle_openai_chat", r#", "governance": ["guardrails"]"#),
        (
            "mantle_openai_responses",
            r#", "governance": ["guardrails"]"#,
        ),
        ("runtime_converse", r#", "api_continuity": "required""#),
        ("runtime_invoke", ""),
        ("mantle_messages", ""),
    ] {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"preferred_api_path": "{preferred}"{constraints}}}}}"#,
                source_ids("gpt-6-astra", "responses")
            ),
            "us-west-2",
        );
        assert_eq!(
            text(&rec, "decision_status"),
            "decision_required",
            "{preferred}"
        );
        assert!(is_null(&rec, "api_path"));
        assert!(has_code(&rec, "blocks", "preferred_api_path_conflict"));
        assert_eq!(
            text(&get_first(&rec, "blocks"), "message"),
            format!("The requested {preferred} path cannot satisfy this workload.")
        );
    }
}

#[test]
fn astra_required_caching_is_checked_on_the_selected_api() {
    for region in ["us-east-1", "us-west-2"] {
        for (surface, preferred, expected) in [
            ("chat_completions", None, None),
            ("responses", Some("mantle_openai_chat"), None),
            ("responses", None, Some("mantle_openai_responses")),
            (
                "chat_completions",
                Some("mantle_openai_responses"),
                Some("mantle_openai_responses"),
            ),
        ] {
            let preference = preferred
                .map(|path| format!(r#", "preferred_api_path": "{path}""#))
                .unwrap_or_default();
            let rec = rec_in(
                &format!(
                    r#"{{{}, "requirements": {{"critical_features": ["prompt_caching"]{preference}}}}}"#,
                    source_ids("gpt-6-astra", surface)
                ),
                region,
            );
            match expected {
                None => {
                    assert_eq!(
                        text(&rec, "decision_status"),
                        "decision_required",
                        "{surface} {preferred:?}"
                    );
                    assert!(is_null(&rec, "api_path"));
                    assert!(has_code(&rec, "blocks", "unverified_capability"));
                    assert!(text(&get_first(&rec, "blocks"), "message").contains("prompt_caching"));
                    let Json::Array(options) = get(&rec, "decision_options") else {
                        panic!("options");
                    };
                    assert_eq!(text(&options[0], "api_path"), "mantle_openai_responses");
                }
                Some(path) => {
                    assert_eq!(text(&rec, "decision_status"), "recommended");
                    assert_eq!(text(&rec, "api_path"), path);
                    assert!(strings(get(&rec, "compatibility"), "native")
                        .contains(&"prompt_caching".to_string()));
                }
            }
        }
    }
}

#[test]
fn astra_decision_options_ignore_an_explicitly_absent_cache_signal() {
    let rec = rec_in(
        &format!(
            r#"{{{}, "requirements": {{"preferred_api_path": "mantle_openai_chat", "api_continuity": "required",
                "governance": ["guardrails"]}},
                "detected_features": ["prompt_caching"], "feature_status": {{"prompt_caching": "absent"}}}}"#,
            source_ids("gpt-6-astra", "chat_completions")
        ),
        "us-west-2",
    );
    let Json::Array(options) = get(&rec, "decision_options") else {
        panic!("options");
    };
    assert_eq!(text(&options[0], "api_path"), "mantle_openai_chat");
}

#[test]
fn astra_chat_n_requires_verification_without_responses_reshape() {
    for signals in [
        r#""requirements": {"uses_n": true}"#,
        r#""detected_features": ["multiple_candidates_n"]"#,
    ] {
        let rec = rec_in(
            &format!(
                "{{{}, {signals}}}",
                source_ids("gpt-6-astra", "chat_completions")
            ),
            "us-west-2",
        );
        assert_eq!(text(&rec, "api_path"), "mantle_openai_chat", "{signals}");
        assert!(has_code(&rec, "tuning", "multiple_candidates_n_unverified"));
        assert!(!has_code(&rec, "migration_deltas", "responses_no_n"));
        assert!(!has_code(
            &rec,
            "blocks",
            "chat_completions_to_responses_required"
        ));
    }
}

#[test]
fn astra_runtime_preserves_model_with_sourced_streaming_and_limits() {
    let rec = run(&format!(
        r#"{{{}, "requirements": {{"governance": ["guardrails"], "critical_features": ["streaming"],
            "data_residency": "global_allowed", "min_context_tokens": 1050000,
            "expected_output_tokens": 128000}}}}"#,
        source_ids("gpt-6-astra", "responses")
    ));
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(text(&rec, "primary_model"), "openai.gpt-6-astra");
    assert_eq!(text(&rec, "api_path"), "runtime_converse");
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "global.openai.gpt-6-astra"
    );
    assert!(strings(get(&rec, "compatibility"), "native").contains(&"streaming".to_string()));
}

#[test]
fn astra_rejects_unsupported_geo_profile_or_caller_region() {
    for (region, geography) in [
        ("eu-west-1", "eu"),
        ("eu-west-1", "us"),
        ("ap-south-1", "au"),
    ] {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"governance": ["guardrails"], "data_residency": "geo_required",
                    "cris_geography": "{geography}"}}}}"#,
                source_ids("gpt-6-astra", "responses")
            ),
            region,
        );
        assert_eq!(
            text(&rec, "decision_status"),
            "decision_required",
            "{region} {geography}"
        );
        assert!(has_code(&rec, "blocks", "inference_profile_unverified"));
    }
}

#[test]
fn astra_runtime_does_not_inherit_mantle_tools() {
    let rec = run(&format!(
        r#"{{{}, "requirements": {{"governance": ["guardrails"],
            "critical_features": ["tool_or_function_calling"]}}}}"#,
        source_ids("gpt-6-astra", "responses")
    ));
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(is_null(&rec, "primary_model"));
    assert!(has_code(&rec, "blocks", "unverified_capability"));
}

#[test]
fn astra_runtime_json_schema_retains_model_and_documented_configuration() {
    let overrides = format!(
        r#"{{{}, "requirements": {{"governance": ["guardrails"], "data_residency": "global_allowed",
            "critical_features": ["structured_output_json"]}}}}"#,
        source_ids("gpt-6-astra", "responses")
    );
    let rec = rec_in(&overrides, "us-west-2");
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "global.openai.gpt-6-astra"
    );
    assert_eq!(
        text(get(&rec, "verification"), "availability_claim"),
        "provisional"
    );
    assert!(strings(get(&rec, "compatibility"), "native")
        .contains(&"structured_output_json".to_string()));
    let guidance = delta_description(&rec, "structured_output_text_format");
    for needle in [
        "outputConfig.textFormat.structure.jsonSchema",
        "additionalModelRequestFields.text.format.strict=true",
        "JSON string",
        "non-streaming",
    ] {
        assert!(guidance.contains(needle), "{needle}");
    }
}

#[test]
fn astra_json_schema_does_not_claim_streaming_support() {
    for (status, expected) in [("detected", "decision_required"), ("absent", "recommended")] {
        let rec = run(&format!(
            r#"{{{}, "requirements": {{"governance": ["guardrails"], "data_residency": "global_allowed",
                "critical_features": ["structured_output_json"]}},
                "detected_features": ["streaming"], "feature_status": {{"streaming": "{status}"}}}}"#,
            source_ids("gpt-6-astra", "responses")
        ));
        assert_eq!(text(&rec, "decision_status"), expected, "{status}");
        if expected == "decision_required" {
            assert!(has_code(
                &rec,
                "blocks",
                "structured_output_streaming_unverified"
            ));
        }
    }
}

#[test]
fn astra_rejects_unverified_profile_arn_or_region() {
    for profile in [
        "arn:aws:bedrock:us-east-1:111122223333:inference-profile/us.openai.gpt-6-astra",
        "arn:aws-cn:bedrock:us-west-2:111122223333:inference-profile/us.openai.gpt-6-astra",
        "arn:aws:bedrock:us-west-2:bad-account:inference-profile/us.openai.gpt-6-astra",
        "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/example",
        "arn:aws:bedrock:us-west-2:111122223333:inference-profile/eu.openai.gpt-6-astra",
        "arn:aws:bedrock:us-west-2:111122223333:inference-profile/us.openai.gpt-5.6-sol",
    ] {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"governance": ["guardrails"], "inference_profile_id": "{profile}"}}}}"#,
                source_ids("gpt-6-astra", "responses")
            ),
            "us-west-2",
        );
        assert_eq!(
            text(&rec, "decision_status"),
            "decision_required",
            "{profile}"
        );
        assert!(
            has_code(&rec, "blocks", "inference_profile_unverified"),
            "{profile}"
        );
    }
    let accepted = rec_in(
        &format!(
            r#"{{{}, "requirements": {{"governance": ["guardrails"],
                "inference_profile_id": "arn:aws:bedrock:us-west-2:111122223333:inference-profile/us.openai.gpt-6-astra"}}}}"#,
            source_ids("gpt-6-astra", "responses")
        ),
        "us-west-2",
    );
    assert_eq!(text(&accepted, "decision_status"), "recommended");
}

#[test]
fn astra_rejects_capacity_above_documented_limit() {
    for (requirement, limit) in [
        ("min_context_tokens", 1_050_001),
        ("expected_output_tokens", 128_001),
    ] {
        let rec = rec_in(
            &format!(
                r#"{{{}, "requirements": {{"{requirement}": {limit}}}}}"#,
                source_ids("gpt-6-astra", "responses")
            ),
            "us-west-2",
        );
        assert_eq!(
            text(&rec, "decision_status"),
            "decision_required",
            "{requirement}"
        );
        let message = text(entry(&rec, "blocks", "unverified_capacity"), "message");
        assert!(message.contains("is below the required"), "{message}");
    }
}
