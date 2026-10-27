//! `test_model_recommendation.py` and the Anthropic-engine rules.

use toolkit_core::Json;

use super::{
    anthropic_catalog, array_len, codes, entry, get, has_code, input_with, is_null, j, merged,
    recommend_input, strings, text,
};
use crate::startup::models::anthropic::{source_version, version_tuple, ANTHROPIC_POOL};
use crate::startup::models::recommend::provider_module;
use crate::startup::models::request::Input;

const BASE: &str = r#"{
  "workload_id": "support-agent",
  "source": {"provider": "anthropic", "model_ids": ["claude-3-7-sonnet-latest"],
             "sdk": "anthropic", "api_surface": "messages", "source_paths": ["src/agent.py"]},
  "requirements": {"priority": "balanced", "critical_features": ["tool_use"]},
  "detected_features": []
}"#;

fn workload(overrides: &str) -> Json {
    merged(BASE, overrides)
}

fn recommend_in(workload: &Json, region: &str) -> Json {
    let result = recommend_input(&input_with(workload, region), false).expect("recommendation");
    get(get(&result, "workloads"), "support-agent").clone()
}

fn rec(overrides: &str) -> Json {
    recommend_in(&workload(overrides), "us-east-1")
}

fn failure(overrides: &str) -> String {
    recommend_input(&input_with(&workload(overrides), "us-east-1"), false)
        .expect_err("a failing recommendation")
        .to_string()
}

#[test]
fn default_anthropic_messages_prefers_mantle_and_sonnet5() {
    // Sonnet 5 supports Mantle Messages, so the balanced priority no longer has
    // to escalate to Opus for Messages continuity.
    let rec = rec("{}");
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-sonnet-5");
    let identity = get(&rec, "model_identity");
    assert_eq!(text(identity, "display_name"), "Claude Sonnet 5");
    assert_eq!(text(identity, "version"), "5.0");
    assert_eq!(text(&rec, "api_path"), "mantle_messages");
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "anthropic.claude-sonnet-5"
    );
    assert_eq!(text(identity, "path_model_id"), text(&rec, "primary_model"));
    assert_eq!(text(get(&rec, "verification"), "probe_status"), "not_run");
    assert_eq!(text(&rec, "provider_module"), "anthropic");
}

#[test]
fn cost_priority_on_messages_uses_clean_mantle_haiku_id() {
    let rec = rec(r#"{"requirements": {"priority": "cost"}}"#);
    assert_eq!(text(&rec, "api_path"), "mantle_messages");
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-haiku-4-5");
    assert_eq!(
        get(get(&rec, "model_identity"), "requires_cris"),
        &Json::Bool(false)
    );
}

#[test]
fn governance_selects_runtime_converse_when_messages_is_a_preference() {
    let rec = rec(r#"{"requirements": {"governance": ["guardrails", "invocation_logging"]}}"#);
    assert_eq!(text(&rec, "decision_status"), "recommended");
    assert_eq!(text(&rec, "api_path"), "runtime_converse");
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-sonnet-5");
}

#[test]
fn required_messages_and_governance_require_user_decision() {
    let rec =
        rec(r#"{"requirements": {"preserve_messages_api": true, "governance": ["guardrails"]}}"#);
    assert_eq!(text(&rec, "decision_status"), "decision_required");
    assert!(is_null(&rec, "primary_model"));
    assert!(is_null(&rec, "api_path"));
    assert!(is_null(&rec, "model_identity"));
    let Json::Array(options) = get(&rec, "decision_options") else {
        panic!("options");
    };
    let mut paths: Vec<&str> = options
        .iter()
        .map(|option| text(option, "api_path"))
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["mantle_messages", "runtime_converse"]);
    assert!(has_code(&rec, "blocks", "model_path_decision_required"));
    let verification = get(&rec, "verification");
    assert_eq!(text(verification, "probe_status"), "not_applicable");
    assert_eq!(text(verification, "availability_claim"), "not_selected");
    assert_eq!(text(get(&rec, "rollout"), "strategy"), "decision_required");
}

#[test]
fn conflicting_paths_without_catalog_candidates_are_rejected() {
    let message = failure(
        r#"{"requirements": {"preserve_messages_api": true, "governance": ["guardrails"],
            "min_context_tokens": 2000000}}"#,
    );
    assert!(
        message.contains("no candidate for every conflicting"),
        "{message}"
    );
    assert!(message.contains("support-agent"), "{message}");
}

#[test]
fn no_candidate_at_all_is_an_error_naming_the_workload() {
    let message = failure(r#"{"requirements": {"min_context_tokens": 2000000}}"#);
    assert!(
        message.contains("no model/path candidate satisfying workload support-agent"),
        "{message}"
    );
}

#[test]
fn native_payload_selects_runtime_invoke() {
    let rec = rec(r#"{"requirements": {"requires_native_payload": true}}"#);
    assert_eq!(text(&rec, "api_path"), "runtime_invoke");
}

#[test]
fn context_and_output_limits_filter_candidates() {
    let rec = rec(
        r#"{"requirements": {"priority": "cost", "min_context_tokens": 1000000,
            "expected_output_tokens": 100000}}"#,
    );
    assert_eq!(
        text(get(&rec, "model_identity"), "model_key"),
        "claude_sonnet_5"
    );
    let Json::Array(alternatives) = get(&rec, "alternatives") else {
        panic!("alternatives");
    };
    assert!(alternatives
        .iter()
        .all(|option| text(option, "model_key") != "claude_haiku_4_5"));
}

#[test]
fn thinking_requirement_filters_haiku() {
    let rec = rec(r#"{"requirements": {"priority": "cost", "thinking_enabled": true}}"#);
    assert_eq!(
        text(get(&rec, "model_identity"), "model_key"),
        "claude_sonnet_5"
    );
}

#[test]
fn multimodal_is_a_vision_requirement() {
    let rec = rec(r#"{"requirements": {"priority": "cost", "critical_features": ["multimodal"]}}"#);
    // Haiku has vision, so it stays the cost choice; the alias is not ignored.
    assert_eq!(
        text(get(&rec, "model_identity"), "model_key"),
        "claude_haiku_4_5"
    );
    assert_eq!(
        strings(get(&rec, "compatibility"), "native"),
        ["multimodal"]
    );
}

#[test]
fn source_version_hop_requires_explicit_feature_scan() {
    let rec = rec("{}");
    let analysis = get(&rec, "source_analysis");
    assert_eq!(text(analysis, "detected_version"), "3.7");
    assert_eq!(text(analysis, "target_version"), "5.0");
    assert_eq!(get(analysis, "version_changed"), &Json::Bool(true));
    assert!(has_code(&rec, "migration_deltas", "claude_version_hop"));
    assert_eq!(
        text(
            entry(&rec, "migration_deltas", "claude_version_hop"),
            "description"
        ),
        "Claude 3.7 to 5.0 requires version migration checks."
    );
    assert!(has_code(&rec, "blocks", "version_scan_incomplete"));
    let Json::Object(assessment) = get(&rec, "feature_assessment") else {
        panic!("assessment");
    };
    let unknown: Vec<&str> = assessment
        .iter()
        .filter(|(_, status)| status.as_str() == Some("unknown"))
        .map(|(feature, _)| feature.as_str())
        .collect();
    assert_eq!(
        unknown,
        [
            "assistant_prefill",
            "budget_tokens",
            "max_tokens_headroom",
            "refusal_handling",
            "sampling_parameters",
            "tokenizer_rebaseline"
        ]
    );
    assert!(text(entry(&rec, "blocks", "version_scan_incomplete"), "message")
        .ends_with("assistant_prefill, budget_tokens, max_tokens_headroom, refusal_handling, sampling_parameters, tokenizer_rebaseline"));
}

#[test]
fn explicit_absent_status_clears_version_scan_block() {
    let rec = rec(
        r#"{"feature_status": {"assistant_prefill": "absent", "budget_tokens": "absent",
            "max_tokens_headroom": "absent", "refusal_handling": "absent",
            "sampling_parameters": "absent", "tokenizer_rebaseline": "absent"}}"#,
    );
    assert!(!has_code(&rec, "blocks", "version_scan_incomplete"));
}

#[test]
fn detected_version_features_emit_blocks_and_tuning() {
    let rec = rec(
        r#"{"detected_features": ["budget_tokens", "sampling_parameters", "assistant_prefill",
            "refusal_handling", "tokenizer_rebaseline", "max_tokens_headroom"]}"#,
    );
    let blocks = codes(&rec, "blocks");
    for code in [
        "budget_tokens_removed",
        "sampling_parameters_removed",
        "assistant_prefill_removed",
    ] {
        assert!(blocks.iter().any(|known| known == code), "{code}");
    }
    let tuning = codes(&rec, "tuning");
    for code in [
        "refusal_handling",
        "tokenizer_rebaseline",
        "max_tokens_headroom",
    ] {
        assert!(tuning.iter().any(|known| known == code), "{code}");
    }
    // Detected surfaces are portable request-shape changes.
    assert_eq!(
        strings(get(&rec, "compatibility"), "portable"),
        [
            "assistant_prefill",
            "budget_tokens",
            "max_tokens_headroom",
            "refusal_handling",
            "sampling_parameters",
            "tokenizer_rebaseline"
        ]
    );
}

#[test]
fn older_target_forced_tool_guidance_requires_verified_support() {
    for (requirements, expected) in [
        (r#"{"preserve_messages_api": true}"#, "mantle_messages"),
        (r#"{"governance": ["guardrails"]}"#, "runtime_converse"),
    ] {
        let rec = rec(&format!(
            r#"{{"requirements": {requirements}, "detected_features": ["structured_output"]}}"#
        ));
        assert_eq!(text(&rec, "api_path"), expected);
        assert!(has_code(
            &rec,
            "blocks",
            "structured_output_portable_pattern"
        ));
        assert!(has_code(&rec, "migration_deltas", "structured_output"));
        let remediation = text(
            entry(&rec, "blocks", "structured_output_portable_pattern"),
            "remediation",
        );
        assert!(remediation.contains("forced tool without strict"));
        assert!(remediation.contains("only after verifying"));
    }
}

#[test]
fn structured_output_and_citations_emit_conflict() {
    let rec = rec(r#"{"detected_features": ["structured_output", "citations"]}"#);
    assert!(has_code(
        &rec,
        "blocks",
        "structured_output_citations_conflict"
    ));
    assert!(strings(get(&rec, "compatibility"), "native").contains(&"citations".to_string()));
}

#[test]
fn agent_features_produce_architecture_and_trajectory_requirements() {
    let rec = rec(
        r#"{"requirements": {"critical_features": ["agentic", "tool_use"]},
            "detected_features": ["agent_infra", "server_tools"]}"#,
    );
    assert_eq!(text(get(&rec, "evaluation"), "mode"), "trajectory");
    assert_eq!(array_len(get(&rec, "evaluation"), "gates"), 4);
    let Json::Array(impacts) = get(&rec, "architecture_impacts") else {
        panic!("impacts");
    };
    let features: Vec<&str> = impacts.iter().map(|item| text(item, "feature")).collect();
    assert_eq!(features, ["agent_infra", "server_tools"]);
    assert_eq!(
        strings(get(&rec, "compatibility"), "rearchitecture"),
        ["agent_infra", "server_tools"]
    );
    // The agentic order puts Sonnet first whatever the priority.
    let quality = super::anthropic::rec_quality_agentic();
    assert_eq!(
        text(get(&quality, "model_identity"), "model_key"),
        "claude_sonnet_5"
    );
}

pub(super) fn rec_quality_agentic() -> Json {
    rec(r#"{"requirements": {"priority": "quality", "critical_features": ["agentic"]}}"#)
}

#[test]
fn runtime_cris_resolution() {
    let cases = [
        (r#"{"data_residency": "unknown"}"#, None),
        (
            r#"{"data_residency": "global_allowed"}"#,
            Some("global.anthropic.claude-sonnet-5"),
        ),
        (
            r#"{"data_residency": "geo_required", "cris_geography": "eu"}"#,
            Some("eu.anthropic.claude-sonnet-5"),
        ),
        (
            r#"{"inference_profile_id": "arn:aws:bedrock:us-east-1:123:inference-profile/custom"}"#,
            Some("arn:aws:bedrock:us-east-1:123:inference-profile/custom"),
        ),
    ];
    for (requirements, expected) in cases {
        let Json::Object(mut members) = j(requirements) else {
            panic!("requirements");
        };
        members.push(("governance".to_string(), j(r#"["guardrails"]"#)));
        let overrides = Json::object(vec![("requirements", Json::Object(members))]);
        let rec = recommend_in(&merged(BASE, &overrides.to_compact_string()), "us-east-1");
        assert_eq!(text(&rec, "api_path"), "runtime_converse");
        let verification = get(&rec, "verification");
        if let Some(id) = expected {
            assert_eq!(text(&rec, "invocation_model_id"), id);
            assert_eq!(text(verification, "invocation_model_id"), id);
        } else {
            assert!(is_null(&rec, "invocation_model_id"));
            assert!(is_null(verification, "invocation_model_id"));
        }
    }
}

#[test]
fn opus_profiles_are_resolved_against_the_region_and_residency() {
    let opus = |region: &str, requirements: &str| {
        recommend_in(
            &workload(&format!(r#"{{"requirements": {requirements}}}"#)),
            region,
        )
    };
    let rec = opus(
        "us-east-1",
        r#"{"priority": "quality", "governance": ["guardrails"], "data_residency": "global_allowed"}"#,
    );
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-opus-5-5");
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "global.anthropic.claude-opus-5-5"
    );
    let verification = get(&rec, "verification");
    assert_eq!(
        strings(verification, "allowed_inference_profiles"),
        [
            "us.anthropic.claude-opus-5-5",
            "global.anthropic.claude-opus-5-5"
        ]
    );
    assert_eq!(
        get(get(&rec, "model_identity"), "requires_cris"),
        &Json::Bool(true)
    );
    assert!(has_code(&rec, "tuning", "adaptive_thinking_required"));

    // eu-west-2 lists Opus 5.5 for in-region runtime calls.
    let rec = opus(
        "eu-west-2",
        r#"{"priority": "quality", "governance": ["guardrails"]}"#,
    );
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "anthropic.claude-opus-5-5"
    );
    // In-region needs no profile, so the verification lists none (requires_cris is false).
    assert!(get(&rec, "verification")
        .get("allowed_inference_profiles")
        .is_none());
    assert_eq!(
        get(get(&rec, "model_identity"), "requires_cris"),
        &Json::Bool(false)
    );
    // An explicit geography profile is still callable there; both lists are verified.
    let rec = opus(
        "eu-west-2",
        r#"{"priority": "quality", "governance": ["guardrails"], "inference_profile_id": "eu.anthropic.claude-opus-5-5"}"#,
    );
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "eu.anthropic.claude-opus-5-5"
    );
    assert_eq!(
        strings(get(&rec, "verification"), "allowed_in_region_model_arns"),
        ["arn:aws:bedrock:eu-west-2::foundation-model/anthropic.claude-opus-5-5"]
    );
    assert_eq!(
        strings(get(&rec, "verification"), "allowed_inference_profiles"),
        [
            "eu.anthropic.claude-opus-5-5",
            "global.anthropic.claude-opus-5-5"
        ]
    );

    // A geography that does not cover the region leaves the profile unresolved.
    let rec = opus(
        "us-east-1",
        r#"{"priority": "quality", "governance": ["guardrails"], "data_residency": "geo_required", "cris_geography": "eu"}"#,
    );
    assert!(is_null(&rec, "invocation_model_id"));
    assert!(has_code(&rec, "blocks", "inference_profile_unresolved"));
    assert_eq!(
        strings(get(&rec, "verification"), "allowed_inference_profiles"),
        Vec::<String>::new()
    );
}

#[test]
fn opus_application_profile_arns_keep_the_callable_arn() {
    let arn = "arn:aws:bedrock:us-east-1:111122223333:application-inference-profile/mine";
    let rec = recommend_in(
        &workload(&format!(
            r#"{{"requirements": {{"priority": "quality", "governance": ["guardrails"], "inference_profile_id": "{arn}"}}}}"#
        )),
        "us-east-1",
    );
    assert_eq!(text(&rec, "invocation_model_id"), arn);
    for wrong in [
        "arn:aws:bedrock:eu-west-1:111122223333:inference-profile/us.anthropic.claude-opus-5-5",
        "arn:aws-cn:bedrock:us-east-1:111122223333:inference-profile/us.anthropic.claude-opus-5-5",
        "arn:aws:bedrock:us-east-1:111122223333:inference-profile/eu.anthropic.claude-opus-5-5",
    ] {
        let rec = recommend_in(
            &workload(&format!(
                r#"{{"requirements": {{"priority": "quality", "governance": ["guardrails"], "inference_profile_id": "{wrong}"}}}}"#
            )),
            "us-east-1",
        );
        assert!(is_null(&rec, "invocation_model_id"), "{wrong}");
    }
    let rec = recommend_in(
        &workload(
            r#"{"requirements": {"priority": "quality", "governance": ["guardrails"],
                "inference_profile_id": "arn:aws:bedrock:us-east-1:111122223333:inference-profile/us.anthropic.claude-opus-5-5"}}"#,
        ),
        "us-east-1",
    );
    assert_eq!(
        text(&rec, "invocation_model_id"),
        "arn:aws:bedrock:us-east-1:111122223333:inference-profile/us.anthropic.claude-opus-5-5"
    );
}

#[test]
fn opus_adaptive_thinking_and_batch_findings() {
    let rec = rec(
        r#"{"requirements": {"priority": "quality", "thinking_enabled": false},
            "detected_features": ["message_batches"]}"#,
    );
    // Disabled thinking cannot satisfy the extended_thinking-free filter; Opus
    // is chosen by priority and the adaptive-thinking conflict blocks.
    assert_eq!(text(&rec, "primary_model"), "anthropic.claude-opus-5-5");
    assert_eq!(
        text(entry(&rec, "blocks", "adaptive_thinking_required"), "tag"),
        "[BLOCKS]"
    );
    assert!(text(
        entry(&rec, "blocks", "message_batches_not_portable"),
        "remediation"
    )
    .starts_with("Opus 5.5 does not support Bedrock Batch."));
    // The architecture impact follows the rewritten remediation.
    let Json::Array(impacts) = get(&rec, "architecture_impacts") else {
        panic!("impacts");
    };
    assert_eq!(impacts.len(), 1);
    assert!(
        text(&impacts[0], "recommendation").starts_with("Opus 5.5 does not support Bedrock Batch.")
    );
}

#[test]
fn provider_treatment_matches_the_input_schema() {
    // The intended treatment of every provider the schema allows, written here
    // rather than derived from the constant it checks.
    let treatment = [
        ("anthropic", "anthropic"),
        ("none", "anthropic"),
        ("unknown", "anthropic"),
        ("openai", "openai"),
        ("azure_openai", "generic"),
        ("google_genai", "generic"),
        ("bedrock", "generic"),
    ];
    let schema = j(&std::fs::read_to_string(
        super::plugin_root()
            .join("skills/agent-advisor/scripts/schemas/model-recommendation-input.json"),
    )
    .expect("schema"));
    assert_eq!(
        get(
            get(
                get(
                    get(get(get(&schema, "properties"), "workloads"), "items"),
                    "properties"
                ),
                "source"
            ),
            "$ref"
        ),
        &Json::string("#/$defs/source")
    );
    let Json::Array(providers) = get(
        get(
            get(get(get(&schema, "$defs"), "source"), "properties"),
            "provider",
        ),
        "enum",
    ) else {
        panic!("providers");
    };
    let mut listed: Vec<&str> = providers.iter().filter_map(Json::as_str).collect();
    let mut known: Vec<&str> = treatment.iter().map(|(provider, _)| *provider).collect();
    listed.sort_unstable();
    known.sort_unstable();
    assert_eq!(
        listed, known,
        "a provider was added to the schema without deciding its treatment"
    );
    let mut pool: Vec<&str> = ANTHROPIC_POOL.to_vec();
    pool.sort_unstable();
    let mut expected: Vec<&str> = treatment
        .iter()
        .filter(|(_, treated)| *treated == "anthropic")
        .map(|(provider, _)| *provider)
        .collect();
    expected.sort_unstable();
    assert_eq!(pool, expected);

    for (provider, treated) in treatment {
        assert_eq!(
            provider_module(provider),
            if treated == "openai" {
                "openai"
            } else {
                "anthropic"
            },
            "{provider}"
        );
        if treated == "openai" {
            continue;
        }
        let rec = rec(&format!(r#"{{"source": {{"provider": "{provider}"}}}}"#));
        assert_eq!(text(&rec, "provider_module"), treated, "{provider}");
        assert_eq!(
            has_code(&rec, "blocks", "provider_module_pending"),
            treated == "generic",
            "{provider}"
        );
        // Only an Anthropic source carries the Anthropic migration deltas.
        assert_eq!(
            array_len(&rec, "migration_deltas") > 0,
            provider == "anthropic",
            "{provider}"
        );
    }
}

#[test]
fn future_provider_is_explicitly_provisional() {
    let rec = rec(
        r#"{"source": {"provider": "azure_openai", "model_ids": ["gpt-5.4"], "sdk": "azure_openai",
            "api_surface": "responses"}}"#,
    );
    assert_eq!(text(&rec, "provider_module"), "generic");
    assert_eq!(
        text(entry(&rec, "blocks", "provider_module_pending"), "message"),
        "The azure_openai to Bedrock compatibility module is not implemented yet."
    );
}

#[test]
fn duplicate_workload_ids_and_a_missing_primary_unit_are_rejected() {
    let mut document = input_with(&workload("{}"), "us-east-1");
    let Json::Object(members) = &mut document else {
        panic!("input");
    };
    let slot = members
        .iter_mut()
        .find(|(name, _)| name == "workloads")
        .expect("workloads");
    let Json::Array(items) = &mut slot.1 else {
        panic!("workloads");
    };
    items.push(workload("{}"));
    let message = recommend_input(&document, false)
        .expect_err("duplicate")
        .to_string();
    assert_eq!(message, "duplicate workload_id: support-agent");

    let mut document = input_with(&workload("{}"), "us-east-1");
    let Json::Object(members) = &mut document else {
        panic!("input");
    };
    members
        .iter_mut()
        .find(|(name, _)| name == "primary_unit")
        .expect("primary_unit")
        .1 = Json::string("other");
    let message = recommend_input(&document, false)
        .expect_err("missing primary")
        .to_string();
    assert_eq!(
        message,
        "primary_unit does not name a workload_id of the input"
    );
}

#[test]
fn a_mixed_run_keeps_catalog_provenance() {
    let mut document = input_with(&workload("{}"), "us-east-2");
    let openai = merged(
        r#"{"workload_id": "openai-svc",
            "source": {"provider": "openai", "model_ids": ["gpt-5.4"], "sdk": "openai",
                       "api_surface": "responses", "source_paths": ["b.py"]},
            "requirements": {"priority": "balanced", "critical_features": [], "api_continuity": "required"},
            "detected_features": []}"#,
        "{}",
    );
    let Json::Object(members) = &mut document else {
        panic!("input");
    };
    let slot = members
        .iter_mut()
        .find(|(name, _)| name == "workloads")
        .expect("workloads");
    let Json::Array(items) = &mut slot.1 else {
        panic!("workloads");
    };
    items.push(openai);
    let result = recommend_input(&document, true).expect("mixed run");
    let workloads = get(&result, "workloads");
    assert_eq!(
        text(get(workloads, "support-agent"), "provider_module"),
        "anthropic"
    );
    assert_eq!(
        text(get(workloads, "openai-svc"), "provider_module"),
        "openai"
    );
    let provenance = get(&result, "catalog_provenance");
    assert_eq!(
        text(get(provenance, "support-agent"), "provider"),
        "anthropic"
    );
    assert_eq!(text(get(provenance, "openai-svc"), "provider"), "openai");
    // The top-level catalog is the primary unit's.
    assert_eq!(text(get(&result, "catalog"), "provider"), "anthropic");
    assert_eq!(get(&result, "schema_version"), &Json::Int(2));
}

#[test]
fn the_openai_catalog_is_loaded_only_for_an_openai_workload() {
    // `recommend_input(.., false)` makes the loader fail: an Anthropic-only run
    // must never call it.
    assert!(recommend_input(&input_with(&workload("{}"), "us-east-1"), false).is_ok());
}

#[test]
fn catalog_records_path_specific_ids_and_limits() {
    let catalog = anthropic_catalog();
    assert_eq!(catalog.verified_at, "2026-09-24");
    assert_eq!(catalog.verified_region, "us-east-1");
    let sonnet = catalog.require("claude_sonnet_5").expect("sonnet");
    assert!(sonnet.path("mantle_messages").expect("mantle").available);
    let haiku = catalog.require("claude_haiku_4_5").expect("haiku");
    assert_eq!(
        haiku.path("mantle_messages").expect("mantle").model_id,
        "anthropic.claude-haiku-4-5"
    );
    assert_eq!(
        haiku.path("runtime_converse").expect("converse").model_id,
        "anthropic.claude-haiku-4-5-20251001-v1:0"
    );
    assert_eq!(
        catalog
            .require("claude_opus_5_5")
            .expect("opus")
            .output_token_ceiling,
        crate::startup::models::catalog::Limit::Known(128_000)
    );
}

#[test]
fn source_versions_follow_the_python_patterns() {
    let version = |id: &str| {
        let input = Input::from_json(&input_with(
            &workload(&format!(r#"{{"source": {{"model_ids": ["{id}"]}}}}"#)),
            "us-east-1",
        ))
        .expect("input");
        source_version(&input.workloads[0].source)
    };
    assert_eq!(version("claude-3-7-sonnet-latest").as_deref(), Some("3.7"));
    assert_eq!(
        version("claude-sonnet-4-5-20250929").as_deref(),
        Some("4.5")
    );
    assert_eq!(
        version("anthropic.claude-3-5-haiku-20241022-v1:0").as_deref(),
        Some("3.5")
    );
    assert_eq!(version("Claude_Haiku_4.5").as_deref(), Some("4.5"));
    assert_eq!(version("claude-03-07").as_deref(), Some("3.7"));
    assert_eq!(version("claude-3.5-sonnet").as_deref(), Some("3.5"));
    // Python quirk kept: the date is read as the minor version.
    assert_eq!(
        version("claude-opus-4-20250514").as_deref(),
        Some("4.20250514")
    );
    assert_eq!(version("claude-opus-4-latest").as_deref(), Some("4.0"));
    assert_eq!(version("claude-opus-4").as_deref(), Some("4.0"));
    assert_eq!(version("claude-instant-1").as_deref(), None);
    assert_eq!(version("my-deployment").as_deref(), None);
    assert_eq!(version_tuple("4.7"), Some((4, 7)));
    assert_eq!(version_tuple("5"), None);
    // The first id with a version wins; an unversioned one is skipped.
    let input = Input::from_json(&input_with(
        &workload(r#"{"source": {"model_ids": ["prod", "claude-3-5-sonnet"]}}"#),
        "us-east-1",
    ))
    .expect("input");
    assert_eq!(
        source_version(&input.workloads[0].source).as_deref(),
        Some("3.5")
    );
}

#[test]
fn a_hop_below_4_7_does_not_demand_the_version_scan() {
    // 4.5 -> 4.5: no hop. 3.7 -> Haiku 4.5: a hop, but the target is below 4.7.
    let rec = rec(
        r#"{"source": {"model_ids": ["claude-3-7-sonnet"]}, "requirements": {"priority": "cost"}}"#,
    );
    assert_eq!(text(get(&rec, "source_analysis"), "target_version"), "4.5");
    assert!(!has_code(&rec, "blocks", "version_scan_incomplete"));
    let Json::Object(assessment) = get(&rec, "feature_assessment") else {
        panic!("assessment");
    };
    assert!(assessment.is_empty());
}

#[test]
fn an_unversioned_source_has_no_version_change() {
    let rec = rec(r#"{"source": {"model_ids": ["prod-default"]}}"#);
    let analysis = get(&rec, "source_analysis");
    assert!(is_null(analysis, "detected_version"));
    assert!(is_null(analysis, "version_changed"));
    assert!(!has_code(&rec, "migration_deltas", "claude_version_hop"));
}

#[test]
fn preferred_paths_and_conflicts() {
    let rec = rec(r#"{"requirements": {"preferred_api_path": "runtime_invoke"}}"#);
    assert_eq!(text(&rec, "api_path"), "runtime_invoke");
    assert!(strings(&rec, "rationale")[0].contains("User selected the runtime_invoke API path."));
    // A runtime preference against required Messages is a decision, not a choice.
    let conflict = super::anthropic::conflict_case();
    assert_eq!(text(&conflict, "decision_status"), "decision_required");
}

pub(super) fn conflict_case() -> Json {
    rec(
        r#"{"requirements": {"preferred_api_path": "runtime_converse", "newest_anthropic_betas": true}}"#,
    )
}

#[test]
fn the_default_for_a_new_workload_is_converse() {
    let rec = rec(
        r#"{"source": {"provider": "none", "model_ids": [], "sdk": "none", "api_surface": "none"}}"#,
    );
    assert_eq!(text(&rec, "api_path"), "runtime_converse");
    assert!(strings(&rec, "rationale")[0].starts_with("Converse is the default"));
    assert!(!anthropic_catalog().models.is_empty());
}
