//! The Rust port of `plugins/aws-startup-advisor/tests/test_ai_workload_profile_contracts.py`
//! (#4806): the target-only `ai-workload-profile.json` observation contract, its
//! semantic invariants and its deterministic projection into the migration-scenario
//! schema (through the crate's schema-subset validator).
//!
//! Each positive check is shown to fail on an edited copy of the fixture; the
//! negative mutations of the Python file stay mutations, each tied to an accepted
//! control.
#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::Json;

use super::matrix::{scenario_findings, scenario_semantics, AI_ARTIFACTS};
use super::support::{
    assert_draft07_shaped, assert_no_unsupported_keywords, assign, at, edited, findings,
    fixtures_named, has_key, items, load, push, remove_string, str_of, string_list, string_set,
    strings,
};

pub(super) const FIXTURES: &str = "fixtures/ai-workload-profiles";
pub(super) const SCHEMA: &str = "scripts/contracts/ai-workload-profile.schema.json";
const PROFILE_FILE: &str = "ai-workload-profile.json";

fn fixture(dir: &str) -> Json {
    load(&format!("{FIXTURES}/{dir}/{PROFILE_FILE}"))
}

fn base_profile() -> Json {
    fixture("ai-only-multi-direct")
}

/// `_profile_findings`.
pub(super) fn profile_findings(profile: &Json) -> Vec<(String, String)> {
    findings(SCHEMA, profile)
}

/// `_assert_profile_semantics`: no duplicate or overlapping source, no gateway as a
/// direct provider, `model_observations` corresponding exactly to `models[]` with
/// families the source declares, and no duplicate gateway upstream.
pub(super) fn profile_semantics(profile: &Json) -> Result<(), String> {
    let mut sources: Vec<(String, String)> = Vec::new();
    for source in items(profile, "ai_sources") {
        let key = (
            str_of(source, "provider").to_owned(),
            str_of(source, "source_service").to_owned(),
        );
        if sources.contains(&key) {
            return Err(format!("duplicate or overlapping AI source: {key:?}"));
        }
        if key.0 == "openrouter" {
            return Err("openrouter is not a direct provider".to_owned());
        }
        sources.push(key);

        let families = string_set(at(source, "model_families"));
        let models = string_set(at(source, "models"));
        let observations: BTreeMap<&str, &Json> = items(source, "model_observations")
            .iter()
            .map(|obs| (str_of(obs, "normalized_model"), obs))
            .collect();
        let observed: BTreeSet<String> = observations.keys().map(|key| (*key).to_owned()).collect();
        if observed != models {
            return Err(format!(
                "model_observations must correspond exactly to models[]: {observed:?} != {models:?}"
            ));
        }
        for obs in items(source, "model_observations") {
            let family = str_of(obs, "model_family");
            if !families.contains(family) {
                return Err(format!(
                    "model_observations family not in model_families: {family}"
                ));
            }
        }
    }
    for gateway in items(profile, "gateways") {
        let mut upstream_keys: Vec<(String, Option<String>)> = Vec::new();
        for upstream in items(gateway, "upstreams") {
            let key = (
                str_of(upstream, "provider").to_owned(),
                upstream
                    .get("source_service")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
            );
            if upstream_keys.contains(&key) {
                return Err(format!(
                    "duplicate or overlapping gateway upstream: {key:?}"
                ));
            }
            upstream_keys.push(key);
        }
    }
    Ok(())
}

/// `_project_to_scenario`: `ai_sources[]` keep their five #410-shaped fields (the
/// observations are dropped), `gateways[]` are copied, inside a synthetic `ai_only`
/// scenario skeleton so the scenario schema and semantics can be reused unmodified.
pub(super) fn project_to_scenario(profile: &Json) -> Json {
    let sources: Vec<Json> = items(profile, "ai_sources")
        .iter()
        .map(|source| {
            Json::object(
                [
                    "provider",
                    "source_service",
                    "model_families",
                    "model_evidence",
                    "models",
                ]
                .iter()
                .map(|key| (*key, at(source, key).clone()))
                .collect(),
            )
        })
        .collect();
    let outcomes = Json::object(vec![
        ("infrastructure", Json::Null),
        (
            "ai",
            Json::object(vec![
                ("verdict", Json::string("passed")),
                ("artifacts", strings(&AI_ARTIFACTS)),
            ]),
        ),
        ("integration_validation", Json::Null),
    ]);
    Json::object(vec![
        ("id", Json::string("ai-workload-profile-projection")),
        ("mode", Json::string("ai_only")),
        (
            "migration_id",
            Json::string("ai-workload-profile-projection"),
        ),
        (
            "run_id",
            Json::string("00000000-0000-4000-8000-000000000099"),
        ),
        ("state", Json::string(".phase-status.json")),
        (
            "dimensions",
            Json::object(vec![
                ("infrastructure_source", Json::Null),
                ("ai_sources", Json::Array(sources)),
                ("gateways", at(profile, "gateways").clone()),
            ]),
        ),
        ("outcomes", outcomes),
    ])
}

/// A fixture is valid when the schema finds nothing.
fn schema_accepts(profile: &Json) -> Result<(), String> {
    let found = profile_findings(profile);
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!("schema findings: {found:?}"))
    }
}

/// The profile projects into a scenario the migration-scenario schema and semantics accept.
fn projects_exactly(profile: &Json) -> Result<(), String> {
    let scenario = project_to_scenario(profile);
    let found = scenario_findings(&scenario);
    if !found.is_empty() {
        return Err(format!("scenario schema findings: {found:?}"));
    }
    scenario_semantics(&scenario)
}

// ---- schema self-validity -----------------------------------------------------------

#[test]
fn test_schema_has_no_unsupported_keywords() {
    assert_no_unsupported_keywords(SCHEMA);
}

#[test]
fn test_schema_is_valid_json_and_draft07_shaped() {
    assert_draft07_shaped(SCHEMA);
}

// ---- positive fixtures ----------------------------------------------------------------

#[test]
fn test_positive_fixtures_validate_against_profile_schema() {
    let all = fixtures_named(FIXTURES, PROFILE_FILE);
    assert_eq!(all.len(), 4);
    for (name, profile) in &all {
        schema_accepts(profile).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        // Non-vacuous: the same fixture without its sources, or with the gateway
        // as a direct provider, is refused.
        let without_sources = edited(profile, |p| {
            let Json::Object(members) = p else {
                panic!("profile")
            };
            members.retain(|(key, _)| key != "ai_sources");
        });
        assert!(schema_accepts(&without_sources).is_err(), "{name}");
        if !items(profile, "ai_sources").is_empty() {
            let gateway_as_provider = edited(profile, |p| {
                assign(p, "ai_sources.0.provider", Json::string("openrouter"));
            });
            assert!(schema_accepts(&gateway_as_provider).is_err(), "{name}");
        }
    }
}

#[test]
fn test_positive_fixtures_satisfy_semantic_invariants() {
    for (name, profile) in fixtures_named(FIXTURES, PROFILE_FILE) {
        profile_semantics(&profile).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        // Non-vacuous: a duplicated source, an observation that no longer corresponds to
        // models[], and a duplicated gateway upstream are each refused.
        let duplicated = edited(&profile, |p| {
            let source = at(p, "ai_sources.0").clone();
            push(p, "ai_sources", source);
        });
        assert!(profile_semantics(&duplicated).is_err(), "{name}");
        let drifted = edited(&profile, |p| {
            assign(p, "ai_sources.0.models", strings(&["made-up-model"]));
        });
        assert!(profile_semantics(&drifted).is_err(), "{name}");
        let foreign_family = edited(&profile, |p| {
            assign(
                p,
                "ai_sources.0.model_observations.0.model_family",
                Json::string("not-a-family"),
            );
        });
        assert!(profile_semantics(&foreign_family).is_err(), "{name}");
        if let Some(Json::Array(gateways)) = profile.get("gateways") {
            if gateways
                .iter()
                .any(|gateway| !items(gateway, "upstreams").is_empty())
            {
                let at_index = gateways
                    .iter()
                    .position(|gateway| !items(gateway, "upstreams").is_empty())
                    .expect("a gateway with upstreams");
                let twice = edited(&profile, |p| {
                    let path = format!("gateways.{at_index}.upstreams");
                    let first = at(p, &format!("{path}.0")).clone();
                    push(p, &path, first);
                });
                assert!(profile_semantics(&twice).is_err(), "{name}");
            }
        }
    }
}

#[test]
fn test_positive_fixtures_project_exactly_into_pr410_semantics() {
    for (name, profile) in fixtures_named(FIXTURES, PROFILE_FILE) {
        projects_exactly(&profile).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        // Non-vacuous: a source with a made-up model or a duplicated source does not project.
        let fabricated = edited(&profile, |p| {
            assign(p, "ai_sources.0.models", strings(&["unknown"]));
        });
        assert!(projects_exactly(&fabricated).is_err(), "{name}");
        let duplicated = edited(&profile, |p| {
            let source = at(p, "ai_sources.0").clone();
            push(p, "ai_sources", source);
        });
        assert!(projects_exactly(&duplicated).is_err(), "{name}");
    }
}

/// The multi-upstream fixture carries at least two Claude models in one family.
fn two_models_in_one_family(profile: &Json) -> Result<(), String> {
    let claude = items(profile, "ai_sources")
        .iter()
        .find(|source| str_of(source, "provider") == "anthropic")
        .ok_or("no anthropic source")?;
    if items(claude, "models").len() >= 2 {
        Ok(())
    } else {
        Err("fewer than two anthropic models".to_owned())
    }
}

#[test]
fn test_openrouter_multi_upstream_fixture_has_two_models_in_one_family() {
    let profile = fixture("openrouter-multi-upstream");
    two_models_in_one_family(&profile).expect("the committed fixture");
    let one_model = edited(&profile, |p| {
        let index = items(p, "ai_sources")
            .iter()
            .position(|source| str_of(source, "provider") == "anthropic")
            .expect("anthropic source");
        let models = format!("ai_sources.{index}.models");
        let first = string_list(at(p, &models))[0].clone();
        assign(p, &models, strings(&[first.as_str()]));
    });
    assert!(two_models_in_one_family(&one_model).is_err());
}

/// `OpenAI` direct and Azure `OpenAI` stay distinct sources and keep distinct raw aliases.
fn distinct_provenance(profile: &Json) -> Result<(), String> {
    let openai: Vec<&Json> = items(profile, "ai_sources")
        .iter()
        .filter(|source| str_of(source, "provider") == "openai")
        .collect();
    let services: BTreeSet<&str> = openai
        .iter()
        .map(|source| str_of(source, "source_service"))
        .collect();
    if services != BTreeSet::from(["openai_api", "azure_openai"]) {
        return Err(format!("openai services are {services:?}"));
    }
    // raw_model differs across services even though normalized_model is identical:
    // the raw alias is retained independently of normalization.
    let raw: BTreeMap<&str, &str> = openai
        .iter()
        .map(|source| {
            (
                str_of(source, "source_service"),
                str_of(at(source, "model_observations.0"), "raw_model"),
            )
        })
        .collect();
    if raw.values().collect::<BTreeSet<_>>().len() == 2 {
        Ok(())
    } else {
        Err(format!("raw aliases are not distinct: {raw:?}"))
    }
}

#[test]
fn test_openai_direct_and_azure_fixture_preserves_distinct_provenance() {
    let profile = fixture("openai-direct-and-azure-openai");
    distinct_provenance(&profile).expect("the committed fixture");
    let same_alias = edited(&profile, |p| {
        let alias = text_of(p, "ai_sources.0.model_observations.0.raw_model");
        assign(
            p,
            "ai_sources.1.model_observations.0.raw_model",
            Json::string(alias),
        );
    });
    assert!(distinct_provenance(&same_alias)
        .expect_err("same alias")
        .contains("not distinct"));
    let one_service = edited(&profile, |p| {
        assign(p, "ai_sources.1.source_service", Json::string("openai_api"));
    });
    assert!(distinct_provenance(&one_service)
        .expect_err("one service")
        .contains("openai services"));
}

fn text_of(value: &Json, path: &str) -> String {
    super::support::text_at(value, path).to_owned()
}

/// The unresolved gateway fixture carries an empty `upstreams` array.
fn unresolved_gateway_is_empty(profile: &Json) -> Result<(), String> {
    let gateway = at(profile, "gateways.0");
    if str_of(gateway, "upstream_evidence") != "unresolved" {
        return Err("the gateway is not unresolved".to_owned());
    }
    if !items(gateway, "upstreams").is_empty() {
        return Err("an unresolved gateway lists upstreams".to_owned());
    }
    Ok(())
}

#[test]
fn test_unresolved_gateway_fixture_has_empty_upstreams() {
    let profile = fixture("openrouter-unresolved");
    unresolved_gateway_is_empty(&profile).expect("the committed fixture");
    let observed = edited(&profile, |p| {
        assign(p, "gateways.0.upstream_evidence", Json::string("observed"));
    });
    assert!(unresolved_gateway_is_empty(&observed)
        .expect_err("observed")
        .contains("not unresolved"));
    let invented = fixture("openrouter-multi-upstream");
    let reason = unresolved_gateway_is_empty(&edited(&invented, |p| {
        assign(
            p,
            "gateways.0.upstream_evidence",
            Json::string("unresolved"),
        );
    }))
    .expect_err("an unresolved gateway with upstreams");
    assert!(reason.contains("lists upstreams"), "{reason}");
}

// ---- negative mutations ----------------------------------------------------------------

#[test]
fn test_empty_model_observations_on_an_observed_source_is_rejected() {
    let base = base_profile();
    assert!(profile_findings(&base).is_empty());
    profile_semantics(&base).expect("the control profile");
    let cleared = edited(&base, |p| {
        assign(
            p,
            "ai_sources.0.model_observations",
            Json::Array(Vec::new()),
        );
    });
    // The schema's observed branch requires model_observations non-empty directly; the
    // semantic oracle stays as defence in depth for the exact-set equality.
    assert!(!profile_findings(&cleared).is_empty());
    assert!(profile_semantics(&cleared).is_err());
}

#[test]
fn test_duplicate_ai_source_identity_is_rejected() {
    let base = base_profile();
    assert!(profile_findings(&base).is_empty());
    profile_semantics(&base).expect("the control profile");
    let duplicated = edited(&base, |p| {
        let source = at(p, "ai_sources.0").clone();
        push(p, "ai_sources", source);
    });
    // A byte-identical duplicate is a uniqueItems violation of the schema; a merely
    // overlapping one reaches the semantic oracle. Either refuses the mutation.
    assert!(!profile_findings(&duplicated).is_empty() || profile_semantics(&duplicated).is_err());
    let overlapping = edited(&base, |p| {
        let mut source = at(p, "ai_sources.0").clone();
        assign(&mut source, "models", strings(&["claude-3-haiku-20240307"]));
        assign(
            &mut source,
            "model_observations.0.normalized_model",
            Json::string("claude-3-haiku-20240307"),
        );
        push(p, "ai_sources", source);
    });
    assert!(profile_findings(&overlapping).is_empty());
    assert!(profile_semantics(&overlapping).is_err());
}

#[test]
fn test_dropped_model_under_a_resolved_upstream_is_rejected() {
    let profile = fixture("openrouter-multi-upstream");
    let scenario = project_to_scenario(&profile);
    scenario_semantics(&scenario).expect("the control projection");
    let broken = edited(&scenario, |s| {
        let index = items(s, "dimensions.gateways.0.upstreams")
            .iter()
            .position(|upstream| str_of(upstream, "provider") == "anthropic")
            .expect("anthropic upstream");
        // Drop one of Claude's two models from the gateway upstream while the source keeps both.
        remove_string(
            s,
            &format!("dimensions.gateways.0.upstreams.{index}.model_families.0.models"),
            "claude-3-haiku-20240307",
        );
    });
    assert!(scenario_semantics(&broken)
        .expect_err("a dropped model")
        .contains("do not match source"));
}

#[test]
fn test_fabricated_unknown_model_id_is_rejected() {
    let base = base_profile();
    assert!(profile_findings(&base).is_empty());
    let fabricated = edited(&base, |p| {
        assign(p, "ai_sources.0.models", strings(&["unknown"]));
        assign(
            p,
            "ai_sources.0.model_observations.0.normalized_model",
            Json::string("unknown"),
        );
    });
    assert!(profile_findings(&fabricated)
        .iter()
        .any(|(_, message)| message.contains("unknown") || message.contains("pattern")));
}

#[test]
fn test_openrouter_as_a_direct_provider_is_rejected() {
    super::support::assert_rejected(SCHEMA, &base_profile(), |p| {
        assign(p, "ai_sources.0.provider", Json::string("openrouter"));
    });
}

#[test]
fn test_empty_upstreams_on_an_observed_gateway_is_rejected() {
    let gateway = Json::object(vec![
        ("product", Json::string("openrouter")),
        ("type", Json::string("llm_router")),
        ("upstream_evidence", Json::string("observed")),
        ("upstreams", Json::Array(Vec::new())),
    ]);
    super::support::assert_rejected(SCHEMA, &base_profile(), |p| {
        assign(p, "gateways", Json::Array(vec![gateway]));
    });
}

#[test]
fn test_invented_upstream_on_an_unresolved_gateway_is_rejected() {
    super::support::assert_rejected(SCHEMA, &fixture("openrouter-multi-upstream"), |p| {
        assign(
            p,
            "gateways.0.upstream_evidence",
            Json::string("unresolved"),
        );
        // upstreams[] is still populated: an invented upstream on an unresolved gateway.
        assert!(has_key(at(p, "gateways.0"), "upstreams"));
        assert!(!items(p, "gateways.0.upstreams").is_empty());
    });
}
