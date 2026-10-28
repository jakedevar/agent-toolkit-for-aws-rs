//! The Rust port of `plugins/aws-startup-advisor/tests/test_generation_ai_contracts.py`
//! (#4806): the canonical `generation-ai.json` contract against its schema (through the
//! crate's schema-subset validator), the projection of `artifacts_by_source[]` into the
//! matching `estimation-ai.json`, the identity rule the schema cannot express and the
//! vendored-copy gate (the Rust `toolkit sync vendored --check`).
//!
//! Each positive check is shown to fail on an edited copy of the fixture; the negative
//! mutations of the Python file stay mutations, each tied to an accepted control.
#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::Json;

use super::documents::remove;
use super::support::{
    assert_draft07_shaped, assert_no_unsupported_keywords, assert_rejected,
    assert_vendored_copies_are_byte_identical, assign, at, edited, findings, fixtures_named, items,
    load, push, str_of, strings,
};

const FIXTURES: &str = "fixtures/generation-ai";
const ESTIMATION_FIXTURES: &str = "fixtures/estimation-ai";
const FILE: &str = "generation-ai.json";
const SCHEMA: &str = "scripts/contracts/generation-ai.schema.json";
const DROPPED_MODEL: &str = "claude-3-haiku-20240307";

fn fixture(dir: &str) -> Json {
    load(&format!("{FIXTURES}/{dir}/{FILE}"))
}

fn base_generation() -> Json {
    fixture("multi-provider-direct")
}

fn matching_estimation(case: &str) -> Json {
    load(&format!("{ESTIMATION_FIXTURES}/{case}/estimation-ai.json"))
}

fn generation_findings(generation: &Json) -> Vec<(String, String)> {
    findings(SCHEMA, generation)
}

type Identity = (String, String);

fn identity(entry: &Json) -> Identity {
    (
        str_of(entry, "provider").to_owned(),
        str_of(entry, "source_service").to_owned(),
    )
}

/// `_assert_artifacts_by_source_projects_into_estimation`: every `artifacts_by_source[]`
/// entry's `(provider, source_service, models)` is a subset of the matching `cost_by_source[]`.
fn projects_into_estimation(generation: &Json, estimation: &Json) -> Result<(), String> {
    let mut allowed: BTreeMap<Identity, BTreeSet<String>> = BTreeMap::new();
    for entry in items(estimation, "cost_by_source") {
        allowed.insert(
            identity(entry),
            items(entry, "models")
                .iter()
                .map(|model| str_of(model, "model").to_owned())
                .collect(),
        );
    }
    for entry in items(generation, "artifacts_by_source") {
        let key = identity(entry);
        let Some(allowed_models) = allowed.get(&key) else {
            return Err(format!(
                "artifacts_by_source[] entry {key:?} traces to no estimation-ai.json cost_by_source[] entry"
            ));
        };
        let named: BTreeSet<String> = items(entry, "models")
            .iter()
            .map(|model| str_of(model, "model").to_owned())
            .collect();
        if !named.is_subset(allowed_models) {
            let extra: Vec<&String> = named.difference(allowed_models).collect();
            return Err(format!(
                "artifacts_by_source[] entry {key:?} names models {extra:?} not present in the matching estimation-ai.json cost_by_source[] entry"
            ));
        }
    }
    Ok(())
}

/// `_assert_artifacts_by_source_identity_is_unique`.
fn identity_is_unique(generation: &Json) -> Result<(), String> {
    let mut seen: BTreeSet<Identity> = BTreeSet::new();
    for entry in items(generation, "artifacts_by_source") {
        let key = identity(entry);
        if !seen.insert(key.clone()) {
            return Err(format!(
                "duplicate or overlapping artifacts_by_source identity: {key:?}"
            ));
        }
    }
    Ok(())
}

// ---- schema self-validity -------------------------------------------------------------------

#[test]
fn test_schema_has_no_unsupported_keywords() {
    assert_no_unsupported_keywords(SCHEMA);
}

#[test]
fn test_schema_is_valid_json_and_draft07_shaped() {
    assert_draft07_shaped(SCHEMA);
}

// ---- positive fixtures ----------------------------------------------------------------------------

#[test]
fn test_positive_fixtures_validate_against_generation_ai_schema() {
    let all = fixtures_named(FIXTURES, FILE);
    assert_eq!(all.len(), 2);
    for (name, generation) in &all {
        assert!(generation_findings(generation).is_empty(), "{name}");
        assert_rejected(SCHEMA, generation, |g| {
            assign(g, "rollback_plan.mechanism", Json::string("manual_cutover"));
        });
    }
}

fn two_simultaneous_direct_sources(generation: &Json) -> Result<(), String> {
    let providers: BTreeSet<&str> = items(generation, "artifacts_by_source")
        .iter()
        .map(|source| str_of(source, "provider"))
        .collect();
    if providers == BTreeSet::from(["anthropic", "openai"]) {
        Ok(())
    } else {
        Err(format!("providers are {providers:?}"))
    }
}

#[test]
fn test_multi_provider_fixture_carries_two_simultaneous_direct_sources() {
    let generation = fixture("multi-provider-direct");
    two_simultaneous_direct_sources(&generation).expect("the committed fixture");
    let single = edited(&generation, |g| {
        assign(
            g,
            "artifacts_by_source.1.provider",
            Json::string("anthropic"),
        );
    });
    assert!(two_simultaneous_direct_sources(&single).is_err());
}

fn gateway_source_model_counts(generation: &Json) -> Result<(), String> {
    let by_provider = |provider: &str| -> Result<usize, String> {
        items(generation, "artifacts_by_source")
            .iter()
            .find(|source| str_of(source, "provider") == provider)
            .map(|source| items(source, "models").len())
            .ok_or_else(|| format!("no {provider} source"))
    };
    if by_provider("anthropic")? != 2 {
        return Err("claude does not have two models".to_owned());
    }
    if by_provider("openai")? != 1 {
        return Err("openai does not have one model".to_owned());
    }
    Ok(())
}

#[test]
fn test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers() {
    let generation = fixture("openrouter-gateway-multi-upstream");
    gateway_source_model_counts(&generation).expect("the committed fixture");
    let claude_index = items(&generation, "artifacts_by_source")
        .iter()
        .position(|source| str_of(source, "provider") == "anthropic")
        .expect("anthropic");
    let one_claude = edited(&generation, |g| {
        let Json::Array(models) =
            super::documents::node(g, &format!("artifacts_by_source.{claude_index}.models"))
        else {
            panic!("models");
        };
        models.pop();
    });
    assert!(gateway_source_model_counts(&one_claude)
        .expect_err("one claude model")
        .contains("two models"));
    let openai_index = items(&generation, "artifacts_by_source")
        .iter()
        .position(|source| str_of(source, "provider") == "openai")
        .expect("openai");
    let two_openai = edited(&generation, |g| {
        let path = format!("artifacts_by_source.{openai_index}.models");
        let model = at(g, &format!("{path}.0")).clone();
        push(g, &path, model);
    });
    assert!(gateway_source_model_counts(&two_openai)
        .expect_err("two openai models")
        .contains("one model"));
}

// ---- cross-phase projection -----------------------------------------------------------------------------

#[test]
fn test_positive_fixtures_project_into_matching_estimation_source() {
    for (name, generation) in fixtures_named(FIXTURES, FILE) {
        let estimation = matching_estimation(&name);
        projects_into_estimation(&generation, &estimation)
            .unwrap_or_else(|reason| panic!("{name}: {reason}"));
        let untraceable = edited(&generation, |g| {
            assign(
                g,
                "artifacts_by_source.0.models.0.model",
                Json::string("made-up-model"),
            );
        });
        assert!(
            projects_into_estimation(&untraceable, &estimation).is_err(),
            "{name}"
        );
        let unknown_source = edited(&generation, |g| {
            assign(
                g,
                "artifacts_by_source.0.source_service",
                Json::string("unknown_api"),
            );
        });
        assert!(
            projects_into_estimation(&unknown_source, &estimation).is_err(),
            "{name}"
        );
    }
}

#[test]
fn test_untraceable_model_under_resolved_estimation_source_is_rejected() {
    let estimation = matching_estimation("multi-provider-direct");
    let control = base_generation();
    projects_into_estimation(&control, &estimation).expect("the control generation");
    let untraceable = edited(&control, |g| {
        assign(
            g,
            "artifacts_by_source.0.models.0.model",
            Json::string("made-up-model"),
        );
    });
    assert!(projects_into_estimation(&untraceable, &estimation)
        .expect_err("a fabricated model")
        .contains("made-up-model"));
}

#[test]
fn test_dropped_model_under_a_resolved_gateway_upstream_is_rejected() {
    let case = "openrouter-gateway-multi-upstream";
    let generation = fixture(case);
    let estimation = matching_estimation(case);
    projects_into_estimation(&generation, &estimation).expect("the control pair");
    // A model removed from the matching estimate's cost_by_source[] entry is untraceable.
    let dropped = edited(&estimation, |e| {
        let source = items(e, "cost_by_source")
            .iter()
            .position(|entry| str_of(entry, "provider") == "anthropic")
            .expect("anthropic entry");
        let path = format!("cost_by_source.{source}.models");
        let kept: Vec<Json> = items(e, &path)
            .iter()
            .filter(|model| str_of(model, "model") != DROPPED_MODEL)
            .cloned()
            .collect();
        assign(e, &path, Json::Array(kept));
    });
    assert!(projects_into_estimation(&generation, &dropped)
        .expect_err("the model is no longer traceable")
        .contains(DROPPED_MODEL));
}

// ---- negative mutations ---------------------------------------------------------------------------------------

#[test]
fn test_duplicate_artifacts_by_source_identity_is_rejected() {
    assert_rejected(SCHEMA, &base_generation(), |g| {
        let entry = at(g, "artifacts_by_source.0").clone();
        push(g, "artifacts_by_source", entry);
    });
}

#[test]
fn test_positive_fixtures_have_unique_artifacts_by_source_identity() {
    for (name, generation) in fixtures_named(FIXTURES, FILE) {
        identity_is_unique(&generation).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        let repeated = edited(&generation, |g| {
            let entry = at(g, "artifacts_by_source.0").clone();
            push(g, "artifacts_by_source", entry);
        });
        assert!(identity_is_unique(&repeated).is_err(), "{name}");
    }
}

#[test]
fn test_overlapping_artifacts_by_source_identity_is_rejected() {
    let generation = base_generation();
    identity_is_unique(&generation).expect("the control generation");
    let overlapping = edited(&generation, |g| {
        let mut entry = at(g, "artifacts_by_source.0").clone();
        assign(
            &mut entry,
            "models.0.generated_files",
            strings(&["src/ai/other_client.py"]),
        );
        push(g, "artifacts_by_source", entry);
    });
    // Different generated files under the same identity are not a byte-identical duplicate, so
    // only the semantic helper refuses them.
    assert!(identity_is_unique(&overlapping)
        .expect_err("an overlapping identity")
        .contains("duplicate or overlapping artifacts_by_source identity"));
}

#[test]
fn test_openrouter_as_a_direct_artifacts_by_source_provider_is_rejected() {
    assert_rejected(SCHEMA, &base_generation(), |g| {
        assign(
            g,
            "artifacts_by_source.0.provider",
            Json::string("openrouter"),
        );
    });
}

#[test]
fn test_malformed_artifacts_by_source_entry_missing_models_is_rejected() {
    assert_rejected(SCHEMA, &base_generation(), |g| {
        remove(g, "artifacts_by_source.0", "models");
    });
}

#[test]
fn test_rollback_plan_mechanism_must_be_feature_flag() {
    assert_rejected(SCHEMA, &base_generation(), |g| {
        assign(g, "rollback_plan.mechanism", Json::string("manual_cutover"));
    });
}

#[test]
fn test_production_readiness_checklist_requires_at_least_five_items() {
    let base = base_generation();
    assert!(items(&base, "production_readiness_checklist").len() >= 5);
    assert_rejected(SCHEMA, &base, |g| {
        let kept: Vec<Json> = items(g, "production_readiness_checklist")[..2].to_vec();
        assign(g, "production_readiness_checklist", Json::Array(kept));
    });
}

#[test]
fn test_generated_files_entry_requires_at_least_one_file() {
    assert_rejected(SCHEMA, &base_generation(), |g| {
        assign(
            g,
            "artifacts_by_source.0.models.0.generated_files",
            Json::Array(Vec::new()),
        );
    });
}

// ---- vendored byte-identity --------------------------------------------------------------------------------------

#[test]
fn test_vendored_copies_are_byte_identical() {
    assert_vendored_copies_are_byte_identical();
}
