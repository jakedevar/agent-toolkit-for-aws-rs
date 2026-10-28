//! The Rust port of `plugins/aws-startup-advisor/tests/test_aws_design_ai_contracts.py`
//! (#4806): the canonical `aws-design-ai.json` contract, its projection into the
//! workload-profile semantics, the model-traceability rule and the per-skill manifest
//! entries that shadow the target-only schema for Azure and GCP.
//!
//! The Python file runs the validator's `load_manifest`, `build_contracts`,
//! `validate_shape` and `validate_json_schema`; this file runs the same functions of the
//! Rust validator. Each positive check is shown to fail on an edited copy of the fixture
//! and each negative mutation is tied to an accepted control.
#![allow(clippy::too_many_lines)]

use toolkit_core::Json;

use super::documents::{put, remove};
use super::matrix::scenario_semantics;
use super::support::{
    assert_draft07_shaped, assert_no_unsupported_keywords, assert_rejected, assign, at, edited,
    findings, fixtures_named, items, load, push, remove_string, str_of, string_set, strings,
};
use super::workload::{profile_findings, profile_semantics, project_to_scenario};
use super::{json, real_plugin};
use crate::startup::artifacts::contracts::{build_contracts, Contract, Env};
use crate::startup::artifacts::shape::validate_shape;

const FIXTURES: &str = "fixtures/aws-design-ai";
const FILE: &str = "aws-design-ai.json";
const SCHEMA: &str = "scripts/contracts/aws-design-ai.schema.json";
const MANIFEST: &str = "scripts/artifact-contracts.json";
const DROPPED_MODEL: &str = "claude-3-haiku-20240307";

fn fixture(dir: &str) -> Json {
    load(&format!("{FIXTURES}/{dir}/{FILE}"))
}

fn base_design() -> Json {
    fixture("multi-provider-direct")
}

fn gateway_design() -> Json {
    fixture("openrouter-gateway-multi-upstream")
}

fn design_findings(design: &Json) -> Vec<(String, String)> {
    findings(SCHEMA, design)
}

/// `_project_to_profile`: the sources and gateways of `metadata`, which already carry the
/// workload-profile fields, wrapped in a synthetic profile.
fn project_to_profile(design: &Json) -> Json {
    Json::object(vec![
        ("schema_version", Json::string("1.0")),
        ("ai_sources", at(design, "metadata.ai_sources").clone()),
        ("gateways", at(design, "metadata.gateways").clone()),
    ])
}

/// The profile projection checks of a design: the profile schema, the profile semantics and
/// the stronger scenario-level correspondence of every gateway upstream.
fn projects_into_profile_semantics(design: &Json) -> Result<(), String> {
    let profile = project_to_profile(design);
    let found = profile_findings(&profile);
    if !found.is_empty() {
        return Err(format!("profile schema findings: {found:?}"));
    }
    profile_semantics(&profile)?;
    // `profile_semantics` checks the observations and the duplicate upstream identities, not
    // whether an upstream's retained models and families match its resolved source's full
    // set; the scenario-level oracle does.
    scenario_semantics(&project_to_scenario(&profile))
}

/// `_assert_model_traceability`: every `design_blocks[].model_id` traces to a direct source
/// model or, through a gateway upstream family, to an upstream model.
fn model_traceability(design: &Json) -> Result<(), String> {
    let mut reachable = std::collections::BTreeSet::new();
    for source in items(design, "metadata.ai_sources") {
        reachable.extend(string_set(at(source, "models")));
    }
    for gateway in items(design, "metadata.gateways") {
        for upstream in items(gateway, "upstreams") {
            for family in items(upstream, "model_families") {
                reachable.extend(string_set(at(family, "models")));
            }
        }
    }
    for block in items(design, "design_blocks") {
        let model = str_of(block, "model_id");
        if !reachable.contains(model) {
            return Err(format!(
                "design_blocks[] model_id {model:?} traces to no metadata.ai_sources[] entry or metadata.gateways[] upstream"
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

/// `additionalProperties: false` on `metadata` and no `ai_source` property: the retired
/// scalar shape is rejected outright, not merely undocumented.
fn metadata_is_closed_without_scalar(schema: &Json) -> Result<(), String> {
    let metadata = at(schema, "definitions.metadata");
    if metadata.get("additionalProperties") != Some(&Json::Bool(false)) {
        return Err("metadata is not closed".to_owned());
    }
    if at(metadata, "properties").get("ai_source").is_some() {
        return Err("metadata declares the retired ai_source".to_owned());
    }
    Ok(())
}

#[test]
fn test_metadata_rejects_the_retired_single_scalar_ai_source() {
    let schema = load(SCHEMA);
    metadata_is_closed_without_scalar(&schema).expect("the committed schema");
    let open = edited(&schema, |s| {
        assign(
            s,
            "definitions.metadata.additionalProperties",
            Json::Bool(true),
        );
    });
    assert!(metadata_is_closed_without_scalar(&open)
        .expect_err("open metadata")
        .contains("not closed"));
    let declared = edited(&schema, |s| {
        put(
            s,
            "definitions.metadata.properties",
            "ai_source",
            Json::object(vec![("type", Json::string("string"))]),
        );
    });
    assert!(metadata_is_closed_without_scalar(&declared)
        .expect_err("a declared scalar")
        .contains("retired ai_source"));
}

// ---- positive fixtures ----------------------------------------------------------------------------

#[test]
fn test_positive_fixtures_validate_against_design_ai_schema() {
    let all = fixtures_named(FIXTURES, FILE);
    assert_eq!(all.len(), 2);
    for (name, design) in &all {
        assert!(design_findings(design).is_empty(), "{name}");
        assert_rejected(SCHEMA, design, |d| {
            assign(d, "metadata.regional_validation", Json::string("never"));
        });
    }
}

#[test]
fn test_positive_fixtures_project_exactly_into_pr415_semantics() {
    for (name, design) in fixtures_named(FIXTURES, FILE) {
        projects_into_profile_semantics(&design)
            .unwrap_or_else(|reason| panic!("{name}: {reason}"));
        // Non-vacuous: a source whose observations no longer match its models, and a source
        // listed twice, do not project.
        let drifted = edited(&design, |d| {
            assign(
                d,
                "metadata.ai_sources.0.models",
                strings(&["made-up-model"]),
            );
        });
        assert!(projects_into_profile_semantics(&drifted).is_err(), "{name}");
        let duplicated = edited(&design, |d| {
            let source = at(d, "metadata.ai_sources.0").clone();
            push(d, "metadata.ai_sources", source);
        });
        assert!(
            projects_into_profile_semantics(&duplicated).is_err(),
            "{name}"
        );
    }
    // The stronger scenario-level oracle catches what the profile-level one cannot.
    let dropped = edited(&gateway_design(), |d| {
        let index = anthropic_upstream_index(d);
        remove_string(
            d,
            &format!("metadata.gateways.0.upstreams.{index}.model_families.0.models"),
            DROPPED_MODEL,
        );
    });
    let profile = project_to_profile(&dropped);
    profile_semantics(&profile).expect("the profile-level check passes");
    assert!(projects_into_profile_semantics(&dropped).is_err());
}

fn anthropic_upstream_index(design: &Json) -> usize {
    items(design, "metadata.gateways.0.upstreams")
        .iter()
        .position(|upstream| str_of(upstream, "provider") == "anthropic")
        .expect("anthropic upstream")
}

fn two_simultaneous_direct_sources(design: &Json) -> Result<(), String> {
    let providers: std::collections::BTreeSet<&str> = items(design, "metadata.ai_sources")
        .iter()
        .map(|source| str_of(source, "provider"))
        .collect();
    if providers != std::collections::BTreeSet::from(["anthropic", "openai"]) {
        return Err(format!("providers are {providers:?}"));
    }
    if !items(design, "metadata.gateways").is_empty() {
        return Err("a gateway is involved".to_owned());
    }
    if items(design, "design_blocks").len() != 2 {
        return Err("not two design blocks".to_owned());
    }
    Ok(())
}

#[test]
fn test_multi_provider_fixture_carries_two_simultaneous_direct_sources() {
    let design = base_design();
    two_simultaneous_direct_sources(&design).expect("the committed fixture");
    let one_provider = edited(&design, |d| {
        assign(
            d,
            "metadata.ai_sources.1.provider",
            Json::string("anthropic"),
        );
    });
    assert!(two_simultaneous_direct_sources(&one_provider)
        .expect_err("one provider")
        .contains("providers are"));
    let with_gateway = edited(&design, |d| {
        let gateway = at(&gateway_design(), "metadata.gateways.0").clone();
        assign(d, "metadata.gateways", Json::Array(vec![gateway]));
    });
    assert!(two_simultaneous_direct_sources(&with_gateway)
        .expect_err("a gateway")
        .contains("gateway"));
    let extra_block = edited(&design, |d| {
        let block = at(d, "design_blocks.0").clone();
        push(d, "design_blocks", block);
    });
    assert!(two_simultaneous_direct_sources(&extra_block)
        .expect_err("a third block")
        .contains("two design blocks"));
}

fn gateway_fronting_two_providers(design: &Json) -> Result<(), String> {
    let gateways = items(design, "metadata.gateways");
    if gateways.len() != 1 {
        return Err("not exactly one gateway".to_owned());
    }
    let upstream_providers: std::collections::BTreeSet<&str> = items(&gateways[0], "upstreams")
        .iter()
        .map(|upstream| str_of(upstream, "provider"))
        .collect();
    if upstream_providers != std::collections::BTreeSet::from(["anthropic", "openai"]) {
        return Err(format!("upstream providers are {upstream_providers:?}"));
    }
    let claude = items(design, "metadata.ai_sources")
        .iter()
        .find(|source| str_of(source, "provider") == "anthropic")
        .ok_or("no anthropic source")?;
    if items(claude, "models").len() != 2 {
        return Err("claude does not have two models".to_owned());
    }
    if items(design, "design_blocks").len() != 3 {
        return Err("not three design blocks".to_owned());
    }
    Ok(())
}

#[test]
fn test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers() {
    let design = gateway_design();
    gateway_fronting_two_providers(&design).expect("the committed fixture");
    let no_gateway = edited(&design, |d| {
        assign(d, "metadata.gateways", Json::Array(Vec::new()));
    });
    assert!(gateway_fronting_two_providers(&no_gateway)
        .expect_err("no gateway")
        .contains("exactly one gateway"));
    let one_upstream = edited(&design, |d| {
        let Json::Array(upstreams) = super::documents::node(d, "metadata.gateways.0.upstreams")
        else {
            panic!("upstreams");
        };
        upstreams.truncate(1);
    });
    assert!(gateway_fronting_two_providers(&one_upstream)
        .expect_err("one upstream")
        .contains("upstream providers"));
    let one_model = edited(&design, |d| {
        let index = items(d, "metadata.ai_sources")
            .iter()
            .position(|source| str_of(source, "provider") == "anthropic")
            .expect("anthropic");
        remove_string(
            d,
            &format!("metadata.ai_sources.{index}.models"),
            DROPPED_MODEL,
        );
    });
    assert!(gateway_fronting_two_providers(&one_model)
        .expect_err("one model")
        .contains("two models"));
    let two_blocks = edited(&design, |d| {
        let Json::Array(blocks) = super::documents::node(d, "design_blocks") else {
            panic!("blocks");
        };
        blocks.pop();
    });
    assert!(gateway_fronting_two_providers(&two_blocks)
        .expect_err("two blocks")
        .contains("three design blocks"));
}

// ---- negative mutations --------------------------------------------------------------------------------

/// The retired Azure-only shape: a scalar `metadata.ai_source` standing in for
/// `ai_sources[]`/`gateways[]`.
fn with_scalar_ai_source(value: &str) -> impl FnOnce(&mut Json) + '_ {
    move |d| {
        remove(d, "metadata", "ai_sources");
        remove(d, "metadata", "gateways");
        put(d, "metadata", "ai_source", Json::string(value));
    }
}

#[test]
fn test_old_single_scalar_ai_source_shape_is_rejected() {
    assert_rejected(SCHEMA, &base_design(), with_scalar_ai_source("both"));
}

#[test]
fn test_old_single_scalar_ai_source_with_one_provider_is_also_rejected() {
    assert_rejected(SCHEMA, &base_design(), with_scalar_ai_source("anthropic"));
}

#[test]
fn test_duplicate_ai_source_identity_is_rejected() {
    assert_rejected(SCHEMA, &base_design(), |d| {
        let source = at(d, "metadata.ai_sources.0").clone();
        push(d, "metadata.ai_sources", source);
    });
}

#[test]
fn test_openrouter_as_a_direct_provider_is_rejected() {
    assert_rejected(SCHEMA, &base_design(), |d| {
        assign(
            d,
            "metadata.ai_sources.0.provider",
            Json::string("openrouter"),
        );
    });
}

#[test]
fn test_dropped_model_under_a_resolved_gateway_upstream_is_rejected() {
    let design = gateway_design();
    let control = project_to_scenario(&project_to_profile(&design));
    scenario_semantics(&control).expect("the control projection");
    let profile = edited(&project_to_profile(&design), |p| {
        let index = items(p, "gateways.0.upstreams")
            .iter()
            .position(|upstream| str_of(upstream, "provider") == "anthropic")
            .expect("anthropic upstream");
        remove_string(
            p,
            &format!("gateways.0.upstreams.{index}.model_families.0.models"),
            DROPPED_MODEL,
        );
    });
    assert!(scenario_semantics(&project_to_scenario(&profile))
        .expect_err("a dropped upstream model")
        .contains("do not match source"));
}

#[test]
fn test_design_block_without_exactly_one_target_is_rejected() {
    assert_rejected(SCHEMA, &base_design(), |d| {
        assign(d, "design_blocks.0.target_bedrock_model", Json::Null);
    });
    assert_rejected(SCHEMA, &base_design(), |d| {
        assign(
            d,
            "design_blocks.0.target_aws_service",
            Json::string("textract"),
        );
    });
}

#[test]
fn test_fabricated_unknown_model_in_metadata_is_rejected() {
    assert_rejected(SCHEMA, &base_design(), |d| {
        assign(d, "metadata.ai_sources.0.models", strings(&["unknown"]));
        assign(
            d,
            "metadata.ai_sources.0.model_observations.0.normalized_model",
            Json::string("unknown"),
        );
    });
}

#[test]
fn test_observed_source_with_empty_model_observations_is_rejected_by_profile_semantics() {
    let design = base_design();
    projects_into_profile_semantics(&design).expect("the control design");
    let cleared = edited(&design, |d| {
        let index = items(d, "metadata.ai_sources")
            .iter()
            .position(|source| str_of(source, "provider") == "anthropic")
            .expect("anthropic");
        assign(
            d,
            &format!("metadata.ai_sources.{index}.model_observations"),
            Json::Array(Vec::new()),
        );
    });
    // The schema now rejects this directly; the projected-profile oracle remains as defence
    // in depth for the exact correspondence minItems cannot express.
    assert!(!design_findings(&cleared).is_empty());
    assert!(profile_semantics(&project_to_profile(&cleared)).is_err());
}

// ---- model traceability -----------------------------------------------------------------------------------

#[test]
fn test_positive_fixtures_have_traceable_design_block_models() {
    for (name, design) in fixtures_named(FIXTURES, FILE) {
        model_traceability(&design).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        let untraceable = edited(&design, |d| {
            assign(d, "design_blocks.0.model_id", Json::string("made-up-model"));
        });
        assert!(model_traceability(&untraceable).is_err(), "{name}");
    }
}

#[test]
fn test_design_block_naming_an_untraceable_model_is_rejected() {
    let design = base_design();
    model_traceability(&design).expect("the control design");
    let untraceable = edited(&design, |d| {
        assign(d, "design_blocks.0.model_id", Json::string("made-up-model"));
    });
    assert!(model_traceability(&untraceable)
        .expect_err("an untraceable model")
        .contains("made-up-model"));
}

/// `claude-3-haiku-20240307` removed from the Anthropic source's models and observations.
fn drop_from_anthropic_source(design: &mut Json) {
    let index = items(design, "metadata.ai_sources")
        .iter()
        .position(|source| str_of(source, "provider") == "anthropic")
        .expect("anthropic");
    let source = format!("metadata.ai_sources.{index}");
    remove_string(design, &format!("{source}.models"), DROPPED_MODEL);
    let kept: Vec<Json> = items(design, &format!("{source}.model_observations"))
        .iter()
        .filter(|obs| str_of(obs, "normalized_model") != DROPPED_MODEL)
        .cloned()
        .collect();
    assign(
        design,
        &format!("{source}.model_observations"),
        Json::Array(kept),
    );
}

#[test]
fn test_model_reachable_only_through_gateway_upstream_passes_traceability() {
    let design = edited(&gateway_design(), drop_from_anthropic_source);
    // The block naming the model is still traceable through the gateway upstream, and the
    // model really is absent from the direct source.
    assert!(items(&design, "design_blocks")
        .iter()
        .any(|block| str_of(block, "model_id") == DROPPED_MODEL));
    model_traceability(&design).expect("traceable through the gateway upstream");
    let direct_models: Vec<String> = items(&design, "metadata.ai_sources")
        .iter()
        .flat_map(|source| string_set(at(source, "models")))
        .collect();
    assert!(!direct_models.iter().any(|model| model == DROPPED_MODEL));
}

#[test]
fn test_model_removed_from_both_source_and_gateway_upstream_fails_traceability() {
    let design = edited(&gateway_design(), |d| {
        drop_from_anthropic_source(d);
        let index = anthropic_upstream_index(d);
        remove_string(
            d,
            &format!("metadata.gateways.0.upstreams.{index}.model_families.0.models"),
            DROPPED_MODEL,
        );
    });
    assert!(model_traceability(&design)
        .expect_err("no source carries the model")
        .contains(DROPPED_MODEL));
}

// ---- manifest shadowing (Azure and GCP) -----------------------------------------------------------------------

/// `build_contracts(manifest, skill, [])` filtered to the one `aws-design-ai.json` contract.
fn design_contract(skill: Option<&str>) -> Contract {
    let plugin = real_plugin();
    let manifest = json(
        &std::fs::read_to_string(plugin.join(MANIFEST)).expect("the artifact contracts manifest"),
    );
    let mut env = Env::new(&plugin);
    let mut sink = Vec::new();
    let mut matched: Vec<Contract> = build_contracts(&mut env, &manifest, skill, &mut sink, false)
        .expect("the contracts build")
        .into_iter()
        .filter(|contract| contract.glob == FILE)
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "exactly one {FILE} contract for {skill:?}"
    );
    matched.remove(0)
}

/// The findings of the shape contract of `skill` for `design` (and the shape's presence).
fn shape_findings(skill: &str, design: &Json) -> Vec<String> {
    let contract = design_contract(Some(skill));
    let shape = contract.shape.as_ref().unwrap_or_else(|| {
        panic!("{skill} must shadow the shared {FILE} schema with its own Shape contract")
    });
    let mut out = Vec::new();
    validate_shape(&shape.root, design, "", "design", &shape.label, &mut out);
    out.into_iter().map(|finding| finding.message).collect()
}

/// The findings of the target-only schema (no skill) for `design`.
fn target_schema_findings(design: &Json) -> Vec<String> {
    let contract = design_contract(None);
    let schema = contract
        .json_schema
        .as_ref()
        .expect("the shared contract is a JSON schema");
    let mut validator = crate::startup::artifacts::schema::SubsetValidator::default();
    let mut out = Vec::new();
    let context = crate::startup::artifacts::schema::Context {
        artifact: "design",
        contract: &contract.json_schema_path,
    };
    validator
        .validate(schema, design, "", &context, &mut out, schema)
        .expect("the schema evaluates");
    out.into_iter().map(|finding| finding.message).collect()
}

/// A document in Azure's old single-scalar `metadata.ai_source` shape.
fn azure_scalar_design() -> Json {
    let base = base_design();
    let mut design = base.clone();
    assign(
        &mut design,
        "metadata",
        Json::object(vec![
            ("ai_source", Json::string("azure_openai")),
            (
                "bedrock_models_selected",
                at(&base, "metadata.bedrock_models_selected").clone(),
            ),
            (
                "regional_validation",
                at(&base, "metadata.regional_validation").clone(),
            ),
        ]),
    );
    design
}

/// A document in GCP's current producer envelope (`design-ai.md`'s top-level fields table).
fn gcp_scalar_design() -> Json {
    json(
        r#"{
  "metadata": {
    "phase": "design",
    "focus": "ai",
    "ai_source": "gemini",
    "bedrock_models_selected": ["anthropic.claude-sonnet-5"],
    "timestamp": "2026-10-06T00:00:00Z"
  },
  "source_profile": "ai-workload-profile.json",
  "design_blocks": [
    {
      "workload_id": "wl_doc1",
      "model_id": "gemini-1.5-pro-vision",
      "target_bedrock_model": null,
      "target_aws_service": "textract",
      "capability": "document_extraction",
      "capability_confidence": "high",
      "rationale": "Traditional-AI capability swap, not a model migration.",
      "confidence_warning": null,
      "honest_assessment": "not_applicable"
    }
  ],
  "ai_architecture": {},
  "regional_warnings": [],
  "multi_model_warnings": [],
  "agentic_design": null
}"#,
    )
}

#[test]
fn test_azure_scalar_ai_source_validates_under_the_restored_azure_shape_contract() {
    let design = azure_scalar_design();
    assert_eq!(
        shape_findings("azure-to-aws", &design),
        Vec::<String>::new()
    );
    // Non-vacuous: the shape contract still refuses a scalar of the wrong type.
    let wrong_type = edited(&design, |d| {
        assign(d, "metadata.ai_source", Json::Array(Vec::new()));
    });
    assert!(!shape_findings("azure-to-aws", &wrong_type).is_empty());
}

#[test]
fn test_azure_scalar_ai_source_fails_the_target_only_schema_with_no_skill() {
    assert!(design_contract(None).json_schema.is_some());
    assert!(!target_schema_findings(&azure_scalar_design()).is_empty());
    // Non-vacuous: the same checker accepts the canonical document.
    assert_eq!(target_schema_findings(&base_design()), Vec::<String>::new());
}

#[test]
fn test_gcp_scalar_ai_source_validates_under_the_gcp_shape_contract() {
    let design = gcp_scalar_design();
    assert_eq!(shape_findings("gcp-to-aws", &design), Vec::<String>::new());
    let wrong_type = edited(&design, |d| {
        assign(d, "metadata.ai_source", Json::Array(Vec::new()));
    });
    assert!(!shape_findings("gcp-to-aws", &wrong_type).is_empty());
}

#[test]
fn test_gcp_scalar_ai_source_fails_the_target_only_schema_with_no_skill() {
    assert!(design_contract(None).json_schema.is_some());
    assert!(!target_schema_findings(&gcp_scalar_design()).is_empty());
    assert_eq!(target_schema_findings(&base_design()), Vec::<String>::new());
}
