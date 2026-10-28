//! The Rust port of `plugins/aws-startup-advisor/tests/test_estimation_ai_contracts.py`
//! (#4806): the canonical `estimation-ai.json` contract against its schema (through the
//! crate's schema-subset validator), the projection of `cost_by_source[]` into the matching
//! `aws-design-ai.json`, the identity rule the schema cannot express and the vendored-copy
//! gate (the Rust `toolkit sync vendored --check`).
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
    load, push, remove_string, str_of, string_set,
};

const FIXTURES: &str = "fixtures/estimation-ai";
const DESIGN_FIXTURES: &str = "fixtures/aws-design-ai";
const FILE: &str = "estimation-ai.json";
const SCHEMA: &str = "scripts/contracts/estimation-ai.schema.json";
const DROPPED_MODEL: &str = "claude-3-haiku-20240307";

fn fixture(dir: &str) -> Json {
    load(&format!("{FIXTURES}/{dir}/{FILE}"))
}

fn base_estimate() -> Json {
    fixture("multi-provider-direct")
}

fn matching_design(case: &str) -> Json {
    load(&format!("{DESIGN_FIXTURES}/{case}/aws-design-ai.json"))
}

fn estimate_findings(estimate: &Json) -> Vec<(String, String)> {
    findings(SCHEMA, estimate)
}

type Identity = (String, String);

/// `_assert_cost_by_source_projects_into_design`: every `cost_by_source[]` entry's
/// `(provider, source_service, models)` is a subset of the matching design's direct source or
/// gateway upstream.
fn projects_into_design(estimate: &Json, design: &Json) -> Result<(), String> {
    let identity = |entry: &Json| -> Identity {
        (
            str_of(entry, "provider").to_owned(),
            str_of(entry, "source_service").to_owned(),
        )
    };
    let mut direct: BTreeMap<Identity, BTreeSet<String>> = BTreeMap::new();
    for source in items(design, "metadata.ai_sources") {
        direct.insert(identity(source), string_set(at(source, "models")));
    }
    let mut upstream: BTreeMap<Identity, BTreeSet<String>> = BTreeMap::new();
    for gateway in items(design, "metadata.gateways") {
        for entry in items(gateway, "upstreams") {
            let models = upstream.entry(identity(entry)).or_default();
            for family in items(entry, "model_families") {
                models.extend(string_set(at(family, "models")));
            }
        }
    }
    for entry in items(estimate, "cost_by_source") {
        let key = identity(entry);
        let allowed = match direct.get(&key) {
            Some(models) if !models.is_empty() => Some(models),
            _ => upstream.get(&key),
        };
        let Some(allowed) = allowed else {
            return Err(format!(
                "cost_by_source[] entry {key:?} traces to no aws-design-ai.json metadata.ai_sources[] entry or metadata.gateways[] upstream"
            ));
        };
        let named: BTreeSet<String> = items(entry, "models")
            .iter()
            .map(|model| str_of(model, "model").to_owned())
            .collect();
        if !named.is_subset(allowed) {
            let extra: Vec<&String> = named.difference(allowed).collect();
            return Err(format!(
                "cost_by_source[] entry {key:?} names models {extra:?} not present in the matching aws-design-ai.json source"
            ));
        }
    }
    Ok(())
}

/// `_assert_cost_by_source_identity_is_unique`.
fn identity_is_unique(estimate: &Json) -> Result<(), String> {
    let mut seen: BTreeSet<Identity> = BTreeSet::new();
    for entry in items(estimate, "cost_by_source") {
        let key = (
            str_of(entry, "provider").to_owned(),
            str_of(entry, "source_service").to_owned(),
        );
        if !seen.insert(key.clone()) {
            return Err(format!(
                "duplicate or overlapping cost_by_source identity: {key:?}"
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
fn test_positive_fixtures_validate_against_estimation_ai_schema() {
    let all = fixtures_named(FIXTURES, FILE);
    assert_eq!(all.len(), 2);
    for (name, estimate) in &all {
        assert!(estimate_findings(estimate).is_empty(), "{name}");
        assert_rejected(SCHEMA, estimate, |e| {
            assign(e, "recommendation.path", Json::string("not_a_real_path"));
        });
    }
}

fn two_simultaneous_direct_sources(estimate: &Json) -> Result<(), String> {
    let providers: BTreeSet<&str> = items(estimate, "cost_by_source")
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
    let estimate = fixture("multi-provider-direct");
    two_simultaneous_direct_sources(&estimate).expect("the committed fixture");
    let single = edited(&estimate, |e| {
        assign(e, "cost_by_source.1.provider", Json::string("anthropic"));
    });
    assert!(two_simultaneous_direct_sources(&single).is_err());
}

fn gateway_source_model_counts(estimate: &Json) -> Result<(), String> {
    let by_provider = |provider: &str| -> Result<usize, String> {
        items(estimate, "cost_by_source")
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
    let estimate = fixture("openrouter-gateway-multi-upstream");
    gateway_source_model_counts(&estimate).expect("the committed fixture");
    let claude_index = items(&estimate, "cost_by_source")
        .iter()
        .position(|source| str_of(source, "provider") == "anthropic")
        .expect("anthropic");
    let one_claude = edited(&estimate, |e| {
        let Json::Array(models) =
            super::documents::node(e, &format!("cost_by_source.{claude_index}.models"))
        else {
            panic!("models");
        };
        models.pop();
    });
    assert!(gateway_source_model_counts(&one_claude)
        .expect_err("one claude model")
        .contains("two models"));
    let openai_index = items(&estimate, "cost_by_source")
        .iter()
        .position(|source| str_of(source, "provider") == "openai")
        .expect("openai");
    let two_openai = edited(&estimate, |e| {
        let path = format!("cost_by_source.{openai_index}.models");
        let model = at(e, &format!("{path}.0")).clone();
        push(e, &path, model);
    });
    assert!(gateway_source_model_counts(&two_openai)
        .expect_err("two openai models")
        .contains("one model"));
}

#[test]
fn test_unknown_source_baseline_propagates_null_to_dependent_deltas() {
    let base = base_estimate();
    assert!(estimate_findings(&base).is_empty());
    assert!(at(&base, "cost_comparison.projected_bedrock_monthly") != &Json::Null);
    let mut estimate = base;
    assign(&mut estimate, "current_costs.monthly_ai_spend", Json::Null);
    for field in [
        "current_source_monthly",
        "monthly_difference",
        "annual_difference",
        "percent_change",
    ] {
        assign(
            &mut estimate,
            &format!("cost_comparison.{field}"),
            Json::Null,
        );
    }
    for source in 0..items(&estimate, "cost_by_source").len() {
        for model in 0..items(&estimate, &format!("cost_by_source.{source}.models")).len() {
            assign(
                &mut estimate,
                &format!("cost_by_source.{source}.models.{model}.vs_source_percent"),
                Json::Null,
            );
        }
    }

    assert!(estimate_findings(&estimate).is_empty());
    assert!(at(&estimate, "cost_comparison.projected_bedrock_monthly") != &Json::Null);

    // A projected cost of zero is a known number, not a missing one.
    assign(
        &mut estimate,
        "cost_comparison.projected_bedrock_monthly",
        Json::Int(0),
    );
    assert_eq!(
        at(&estimate, "cost_comparison.projected_bedrock_monthly"),
        &Json::Int(0)
    );
    assert!(estimate_findings(&estimate).is_empty());

    // ... and a stringified zero is neither.
    assign(
        &mut estimate,
        "cost_comparison.projected_bedrock_monthly",
        Json::string("0"),
    );
    assert!(!estimate_findings(&estimate).is_empty());
}

#[test]
fn test_unavailable_target_pricing_preserves_unknown_costs_as_null() {
    let mut estimate = base_estimate();
    assign(&mut estimate, "pricing_source", Json::string("unavailable"));
    for field in [
        "projected_bedrock_monthly",
        "monthly_difference",
        "annual_difference",
        "percent_change",
    ] {
        assign(
            &mut estimate,
            &format!("cost_comparison.{field}"),
            Json::Null,
        );
    }
    for source in 0..items(&estimate, "cost_by_source").len() {
        for model in 0..items(&estimate, &format!("cost_by_source.{source}.models")).len() {
            let path = format!("cost_by_source.{source}.models.{model}");
            assign(&mut estimate, &format!("{path}.monthly_cost"), Json::Null);
            assign(
                &mut estimate,
                &format!("{path}.vs_source_percent"),
                Json::Null,
            );
        }
    }
    assert!(estimate_findings(&estimate).is_empty());
    // Non-vacuous: the same document with a pricing source outside the enumeration is refused.
    let made_up = edited(&estimate, |e| {
        assign(e, "pricing_source", Json::string("guessed"));
    });
    assert!(!estimate_findings(&made_up).is_empty());
}

// ---- cross-phase projection -----------------------------------------------------------------------------

#[test]
fn test_positive_fixtures_project_into_matching_design_source() {
    for (name, estimate) in fixtures_named(FIXTURES, FILE) {
        let design = matching_design(&name);
        projects_into_design(&estimate, &design)
            .unwrap_or_else(|reason| panic!("{name}: {reason}"));
        let untraceable = edited(&estimate, |e| {
            assign(
                e,
                "cost_by_source.0.models.0.model",
                Json::string("made-up-model"),
            );
        });
        assert!(
            projects_into_design(&untraceable, &design).is_err(),
            "{name}"
        );
        let unknown_source = edited(&estimate, |e| {
            assign(
                e,
                "cost_by_source.0.source_service",
                Json::string("unknown_api"),
            );
        });
        assert!(
            projects_into_design(&unknown_source, &design).is_err(),
            "{name}"
        );
    }
}

#[test]
fn test_untraceable_model_under_resolved_design_source_is_rejected() {
    let design = matching_design("multi-provider-direct");
    let control = base_estimate();
    projects_into_design(&control, &design).expect("the control estimate");
    let untraceable = edited(&control, |e| {
        assign(
            e,
            "cost_by_source.0.models.0.model",
            Json::string("made-up-model"),
        );
    });
    assert!(projects_into_design(&untraceable, &design)
        .expect_err("a fabricated model")
        .contains("made-up-model"));
}

#[test]
fn test_dropped_model_under_a_resolved_gateway_upstream_is_rejected() {
    let case = "openrouter-gateway-multi-upstream";
    let estimate = fixture(case);
    let design = matching_design(case);
    projects_into_design(&estimate, &design).expect("the control pair");
    // The model is removed from BOTH the gateway upstream and the direct source of the design.
    let dropped = edited(&design, |d| {
        let source = items(d, "metadata.ai_sources")
            .iter()
            .position(|source| str_of(source, "provider") == "anthropic")
            .expect("anthropic source");
        let prefix = format!("metadata.ai_sources.{source}");
        remove_string(d, &format!("{prefix}.models"), DROPPED_MODEL);
        let kept: Vec<Json> = items(d, &format!("{prefix}.model_observations"))
            .iter()
            .filter(|obs| str_of(obs, "normalized_model") != DROPPED_MODEL)
            .cloned()
            .collect();
        assign(
            d,
            &format!("{prefix}.model_observations"),
            Json::Array(kept),
        );
        let upstream = items(d, "metadata.gateways.0.upstreams")
            .iter()
            .position(|upstream| str_of(upstream, "provider") == "anthropic")
            .expect("anthropic upstream");
        for family in 0..items(
            d,
            &format!("metadata.gateways.0.upstreams.{upstream}.model_families"),
        )
        .len()
        {
            let path =
                format!("metadata.gateways.0.upstreams.{upstream}.model_families.{family}.models");
            if string_set(at(d, &path)).contains(DROPPED_MODEL) {
                remove_string(d, &path, DROPPED_MODEL);
            }
        }
    });
    assert!(projects_into_design(&estimate, &dropped)
        .expect_err("the model is no longer traceable")
        .contains(DROPPED_MODEL));
}

// ---- negative mutations ---------------------------------------------------------------------------------------

#[test]
fn test_duplicate_cost_by_source_identity_is_rejected() {
    assert_rejected(SCHEMA, &base_estimate(), |e| {
        let entry = at(e, "cost_by_source.0").clone();
        push(e, "cost_by_source", entry);
    });
}

#[test]
fn test_positive_fixtures_have_unique_cost_by_source_identity() {
    for (name, estimate) in fixtures_named(FIXTURES, FILE) {
        identity_is_unique(&estimate).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        let repeated = edited(&estimate, |e| {
            let entry = at(e, "cost_by_source.0").clone();
            push(e, "cost_by_source", entry);
        });
        assert!(identity_is_unique(&repeated).is_err(), "{name}");
    }
}

#[test]
fn test_overlapping_cost_by_source_identity_is_rejected() {
    let estimate = base_estimate();
    identity_is_unique(&estimate).expect("the control estimate");
    let overlapping = edited(&estimate, |e| {
        let mut entry = at(e, "cost_by_source.0").clone();
        assign(&mut entry, "models.0.monthly_cost", Json::Float(1.0));
        push(e, "cost_by_source", entry);
    });
    // A different payload under the same identity is not a byte-identical duplicate, so only
    // the semantic helper refuses it.
    assert!(identity_is_unique(&overlapping)
        .expect_err("an overlapping identity")
        .contains("duplicate or overlapping cost_by_source identity"));
}

#[test]
fn test_openrouter_as_a_direct_cost_by_source_provider_is_rejected() {
    assert_rejected(SCHEMA, &base_estimate(), |e| {
        assign(e, "cost_by_source.0.provider", Json::string("openrouter"));
    });
}

#[test]
fn test_malformed_cost_by_source_entry_missing_models_is_rejected() {
    assert_rejected(SCHEMA, &base_estimate(), |e| {
        remove(e, "cost_by_source.0", "models");
    });
}

#[test]
fn test_recommendation_requires_a_valid_path_enum() {
    assert_rejected(SCHEMA, &base_estimate(), |e| {
        assign(e, "recommendation.path", Json::string("not_a_real_path"));
    });
}

#[test]
fn test_optimization_opportunity_missing_type_is_rejected() {
    assert_rejected(SCHEMA, &base_estimate(), |e| {
        remove(e, "optimization_opportunities.0", "type");
    });
}

#[test]
fn test_services_not_estimated_entry_requires_not_token_priced_reason() {
    let reason = |text: &str| {
        Json::Array(vec![Json::object(vec![
            ("workload_id", Json::string("wl_doc1")),
            ("target_aws_service", Json::string("textract")),
            ("reason", Json::string(text)),
        ])])
    };
    let base = base_estimate();
    // The one allowed reason is accepted; any other is refused.
    let accepted = edited(&base, |e| {
        assign(e, "services_not_estimated", reason("not_token_priced"));
    });
    assert!(estimate_findings(&accepted).is_empty());
    assert_rejected(SCHEMA, &base, |e| {
        assign(e, "services_not_estimated", reason("something_else"));
    });
}

// ---- vendored byte-identity --------------------------------------------------------------------------------------

#[test]
fn test_vendored_copies_are_byte_identical() {
    assert_vendored_copies_are_byte_identical();
}
