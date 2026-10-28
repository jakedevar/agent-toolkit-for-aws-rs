//! The Rust port of `plugins/aws-startup-advisor/tests/test_ai_migration_matrix_contracts.py`
//! (#4806): the composable target migration scenarios of
//! `fixtures/ai-migration-matrix` against `ai-migration-scenario.schema.json`
//! (through the crate's schema-subset validator), their semantic invariants and
//! the integration-validation artifact through `toolkit startup artifacts`.
//!
//! A contract is a function over the loaded [`Matrix`] (or over one scenario) that
//! returns `Err(reason)`. Each positive test runs it on the committed fixtures and on
//! a scratch copy edited so that it must fail; the negative mutations of the Python
//! file stay mutations, each tied to an accepted control.

#![allow(clippy::too_many_lines, clippy::type_complexity)]

use std::collections::BTreeSet;
use std::path::Path;

use toolkit_core::Json;

use super::support::{
    assert_rejected, assign, at, edited, findings, has_key, items, load, load_at, push,
    remove_string, same, schema_is_in_the_supported_subset, str_of, string_list, string_set,
    strings, Tree,
};
use super::{artifacts, error_pairs, json, plugin_arg, text, write, TempDir};
use crate::startup::artifacts::pyjson::dumps;

pub(super) const ROOT: &str = "fixtures/ai-migration-matrix";
pub(super) const SCENARIO_SCHEMA: &str = "scripts/contracts/ai-migration-scenario.schema.json";
const INTEGRATION_SCHEMA: &str = "scripts/contracts/integration-validation.schema.json";
const PHASE_STATUS_SCHEMA: &str = "skills/shared/state/phase-status.schema.json";

const INFRA_ARTIFACTS: [&str; 4] = [
    "aws-design.json",
    "estimation-infra.json",
    "generation-infra.json",
    "validation-report.json",
];
pub(super) const AI_ARTIFACTS: [&str; 4] = [
    "ai-workload-profile.json",
    "aws-design-ai.json",
    "estimation-ai.json",
    "generation-ai.json",
];
const INTEGRATION_CHECKS: [&str; 9] = [
    "networking",
    "iam_and_secrets",
    "data_flow",
    "observability",
    "deployment_order",
    "cutover",
    "rollback",
    "cost_estimate",
    "validation_dependencies",
];
const REQUIRED_ADDITIONS: [&str; 3] = [
    "infra-only-heroku",
    "ai-only-anthropic",
    "combined-heroku-anthropic",
];
const ANTHROPIC_MODEL: &str = "claude-3-5-sonnet-20240620";

// ---- the loaded fixture tree -----------------------------------------------------------------

#[derive(Clone)]
struct Case {
    id: String,
    /// The case directory below the matrix root.
    dir: String,
    snapshot: Json,
}

struct Matrix {
    root: std::path::PathBuf,
    manifest: Json,
    cases: Vec<Case>,
}

impl Matrix {
    /// `_manifest()` and `_cases()`: the manifest at `contract_version` 3.0 and every
    /// case it names, once, under the id the snapshot carries.
    fn load(root: &Path) -> Result<Self, String> {
        let manifest = load_at(root, "expected-matrix.json");
        if manifest.get("contract_version").and_then(Json::as_str) != Some("3.0") {
            return Err("the manifest is not contract_version 3.0".to_owned());
        }
        let mut cases: Vec<Case> = Vec::new();
        for entry in items(&manifest, "cases") {
            let snapshot_rel = str_of(entry, "snapshot");
            let snapshot = load_at(root, snapshot_rel);
            let id = str_of(entry, "id");
            if str_of(&snapshot, "id") != id {
                return Err(format!("{snapshot_rel} does not carry the id {id}"));
            }
            if cases.iter().any(|case| case.id == id) {
                return Err(format!("case {id} is listed twice"));
            }
            let dir = snapshot_rel
                .rsplit_once('/')
                .map_or(String::new(), |(dir, _)| dir.to_owned());
            cases.push(Case {
                id: id.to_owned(),
                dir,
                snapshot,
            });
        }
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            cases,
        })
    }

    fn real() -> Self {
        Self::load(&super::real_plugin().join(ROOT)).expect("the committed matrix loads")
    }

    fn case(&self, id: &str) -> &Json {
        &self
            .cases
            .iter()
            .find(|case| case.id == id)
            .unwrap_or_else(|| panic!("no case {id}"))
            .snapshot
    }
}

/// A scratch copy of the matrix tree, edited by `edit`, reloaded.
fn broken(label: &str, edit: impl FnOnce(&Tree)) -> (Tree, Result<Matrix, String>) {
    let copy = Tree::of(ROOT, label);
    edit(&copy);
    let matrix = Matrix::load(copy.root());
    (copy, matrix)
}

/// Requires `contract` to pass on the committed matrix and to fail, with a reason
/// containing `expected`, on the copy edited by `edit`.
fn check_matrix(
    label: &str,
    contract: fn(&Matrix) -> Result<(), String>,
    edit: impl FnOnce(&Tree),
    expected: &str,
) {
    if let Err(reason) = contract(&Matrix::real()) {
        panic!("the committed matrix breaks the contract: {reason}");
    }
    let (_copy, matrix) = broken(label, edit);
    let reason = match matrix {
        Ok(matrix) => contract(&matrix).expect_err("the edited copy still passes the contract"),
        Err(reason) => reason,
    };
    assert!(
        reason.contains(expected),
        "{label}: failed for another reason: {reason} (wanted {expected:?})"
    );
}

// ---- the schema and the semantic oracle ---------------------------------------------------

/// `_scenario_validation_findings`.
pub(super) fn scenario_findings(snapshot: &Json) -> Vec<(String, String)> {
    findings(SCENARIO_SCHEMA, snapshot)
}

/// `_validate_scenario`.
fn validate_scenario(snapshot: &Json) -> Result<(), String> {
    let found = scenario_findings(snapshot);
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!("schema findings: {found:?}"))
    }
}

/// `_assert_scenario_semantics`: the duplicate and overlap rules, and the exact
/// pairing of every gateway upstream with the one retained source it resolves to.
pub(super) fn scenario_semantics(snapshot: &Json) -> Result<(), String> {
    let dimensions = at(snapshot, "dimensions");
    if at(dimensions, "infrastructure_source").as_str() == Some("openrouter") {
        return Err("openrouter is not an infrastructure source".to_owned());
    }

    let mut sources: Vec<((String, String), &Json)> = Vec::new();
    for source in items(dimensions, "ai_sources") {
        let key = (
            str_of(source, "provider").to_owned(),
            str_of(source, "source_service").to_owned(),
        );
        if sources.iter().any(|(known, _)| *known == key) {
            return Err(format!("duplicate or overlapping AI source: {key:?}"));
        }
        if key.0 == "openrouter" {
            return Err("openrouter is not a direct provider".to_owned());
        }
        sources.push((key, source));
    }

    for gateway in items(dimensions, "gateways") {
        let mut resolved_keys: Vec<(String, String)> = Vec::new();
        for upstream in items(gateway, "upstreams") {
            // Resolve the upstream's optional provenance to its effective source
            // first: an explicit-service and a service-less upstream that resolve to
            // the same retained source are one association.
            let provider = str_of(upstream, "provider");
            let source = if has_key(upstream, "source_service") {
                let raw = (
                    provider.to_owned(),
                    str_of(upstream, "source_service").to_owned(),
                );
                sources
                    .iter()
                    .find(|(known, _)| *known == raw)
                    .map(|(_, source)| *source)
                    .ok_or_else(|| format!("unmatched gateway upstream: {raw:?}"))?
            } else {
                let candidates: Vec<&Json> = sources
                    .iter()
                    .filter(|((known, _), _)| known == provider)
                    .map(|(_, source)| *source)
                    .collect();
                if candidates.len() != 1 {
                    return Err(format!(
                        "service-less upstream must match one source: {provider}"
                    ));
                }
                candidates[0]
            };

            let resolved = (
                str_of(source, "provider").to_owned(),
                str_of(source, "source_service").to_owned(),
            );
            if resolved_keys.contains(&resolved) {
                return Err(format!(
                    "duplicate or overlapping gateway upstream: {resolved:?}"
                ));
            }
            resolved_keys.push(resolved);

            let families = string_set(at(source, "model_families"));
            let models = string_set(at(source, "models"));
            let mut seen_families = BTreeSet::new();
            let mut seen_models = BTreeSet::new();
            for family in items(upstream, "model_families") {
                let name = str_of(family, "family").to_owned();
                if seen_families.contains(&name) {
                    return Err(format!("duplicate or overlapping upstream family: {name}"));
                }
                if !families.contains(&name) {
                    return Err(format!("wrongly paired upstream family: {name}"));
                }
                seen_families.insert(name);
                if family.get("model_evidence") != source.get("model_evidence") {
                    return Err(format!(
                        "wrongly paired upstream model evidence: {:?}",
                        family.get("model_evidence")
                    ));
                }
                let family_models = string_set(at(family, "models"));
                if !family_models.is_subset(&models) {
                    return Err(format!("wrongly paired upstream models: {family_models:?}"));
                }
                seen_models.extend(family_models);
            }
            if seen_families != families {
                return Err(format!(
                    "gateway upstream families do not match source: {seen_families:?}"
                ));
            }
            if seen_models != models {
                return Err(format!(
                    "gateway upstream models do not match source: {seen_models:?}"
                ));
            }
        }
    }
    Ok(())
}

/// Schema, then semantics.
fn validate_and_check(snapshot: &Json) -> Result<(), String> {
    validate_scenario(snapshot)?;
    scenario_semantics(snapshot)
}

/// Python's `if not findings: pytest.raises(AssertionError) around the semantics /
/// else: assert findings`: the mutation is refused by the schema or, failing that,
/// by the semantic oracle.
fn refused(snapshot: &Json) -> bool {
    !scenario_findings(snapshot).is_empty() || scenario_semantics(snapshot).is_err()
}

// ---- scenario builders ------------------------------------------------------------------------

fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::object(pairs)
}

fn source(provider: &str, service: &str, family: &str, models: &[&str]) -> Json {
    object(vec![
        ("provider", Json::string(provider)),
        ("source_service", Json::string(service)),
        ("model_families", strings(&[family])),
        ("model_evidence", Json::string("observed")),
        ("models", strings(models)),
    ])
}

fn family_entry(family: &str, models: &[&str]) -> Json {
    object(vec![
        ("family", Json::string(family)),
        ("model_evidence", Json::string("observed")),
        ("models", strings(models)),
    ])
}

fn upstream(provider: &str, service: &str, family: &str, models: &[&str]) -> Json {
    object(vec![
        ("provider", Json::string(provider)),
        ("source_service", Json::string(service)),
        (
            "model_families",
            Json::Array(vec![family_entry(family, models)]),
        ),
    ])
}

fn gateway(upstreams: Vec<Json>) -> Json {
    object(vec![
        ("product", Json::string("openrouter")),
        ("type", Json::string("llm_router")),
        ("upstream_evidence", Json::string("observed")),
        ("upstreams", Json::Array(upstreams)),
    ])
}

fn synthetic(
    matrix: &Matrix,
    infrastructure_source: &str,
    (provider, service, family, model): (&str, &str, &str, &str),
    openrouter: bool,
) -> Json {
    let mut scenario = matrix.case("combined-heroku-anthropic").clone();
    assign(&mut scenario, "id", Json::string("synthetic-composition"));
    assign(
        &mut scenario,
        "migration_id",
        Json::string("synthetic-composition"),
    );
    let gateways = if openrouter {
        vec![gateway(vec![upstream(provider, service, family, &[model])])]
    } else {
        Vec::new()
    };
    assign(
        &mut scenario,
        "dimensions",
        object(vec![
            ("infrastructure_source", Json::string(infrastructure_source)),
            (
                "ai_sources",
                Json::Array(vec![source(provider, service, family, &[model])]),
            ),
            ("gateways", Json::Array(gateways)),
        ]),
    );
    scenario
}

/// `(provider, service, family, models)` of the five associations of the multi-cardinality
/// `OpenRouter` scenario.
const ASSOCIATIONS: [(&str, &str, &str, &[&str]); 5] = [
    (
        "anthropic",
        "anthropic_api",
        "claude",
        &["claude-3-5-sonnet-20240620", "claude-3-haiku-20240307"],
    ),
    ("google", "vertex_ai", "gemini", &["gemini-1.5-pro"]),
    ("openai", "openai_api", "gpt", &["gpt-4o"]),
    ("openai", "azure_openai", "gpt", &["gpt-4o"]),
    (
        "future_provider",
        "future_api",
        "future_family",
        &["future-model-v1"],
    ),
];

fn multi_openrouter(matrix: &Matrix, infrastructure_source: &str) -> Json {
    let mut scenario = matrix.case("combined-heroku-anthropic").clone();
    assign(
        &mut scenario,
        "id",
        Json::string("multi-cardinality-openrouter"),
    );
    assign(
        &mut scenario,
        "migration_id",
        Json::string("multi-cardinality-openrouter"),
    );
    let sources = ASSOCIATIONS
        .iter()
        .map(|(provider, service, family, models)| source(provider, service, family, models))
        .collect();
    let upstreams = ASSOCIATIONS
        .iter()
        .map(|(provider, service, family, models)| upstream(provider, service, family, models))
        .collect();
    assign(
        &mut scenario,
        "dimensions",
        object(vec![
            ("infrastructure_source", Json::string(infrastructure_source)),
            ("ai_sources", Json::Array(sources)),
            ("gateways", Json::Array(vec![gateway(upstreams)])),
        ]),
    );
    scenario
}

fn gateway_upstreams(scenario: &mut Json) -> &mut Vec<Json> {
    match super::documents::node(scenario, "dimensions.gateways.0.upstreams") {
        Json::Array(upstreams) => upstreams,
        other => panic!("not an array: {other:?}"),
    }
}

// ---- manifest and snapshots ----------------------------------------------------------------------

fn manifest_shape(matrix: &Matrix) -> Result<(), String> {
    if matrix.cases.len() != 14 {
        return Err(format!("{} scenarios, not fourteen", matrix.cases.len()));
    }
    let ids: BTreeSet<&str> = matrix.cases.iter().map(|case| case.id.as_str()).collect();
    for required in REQUIRED_ADDITIONS {
        if !ids.contains(required) {
            return Err(format!("required scenario {required} is missing"));
        }
    }
    let modes: BTreeSet<&str> = matrix
        .cases
        .iter()
        .map(|case| str_of(&case.snapshot, "mode"))
        .collect();
    let all = BTreeSet::from(["infrastructure_only", "ai_only", "combined"]);
    if modes != all {
        return Err(format!("modes {modes:?} are not all three"));
    }
    Ok(())
}

#[test]
fn test_manifest_has_fourteen_representative_scenarios_and_all_modes() {
    check_matrix(
        "manifest-fourteen",
        manifest_shape,
        |copy| {
            copy.edit_json(
                "expected-matrix.json",
                |manifest| match super::documents::node(manifest, "cases") {
                    Json::Array(cases) => {
                        cases.pop();
                    }
                    other => panic!("{other:?}"),
                },
            );
        },
        "not fourteen",
    );
    // The mode set is a separate assertion: moving the one ai_only-less scenarios
    // over to combined leaves fourteen scenarios but not all three modes.
    check_matrix(
        "manifest-modes",
        manifest_shape,
        |copy| {
            for dir in [
                "ai-only-gemini",
                "ai-only-openai",
                "ai-only-anthropic",
                "ai-only-openrouter",
            ] {
                copy.edit_json(&format!("{dir}/expected-artifacts.json"), |snapshot| {
                    assign(snapshot, "mode", Json::string("combined"));
                });
            }
        },
        "not all three",
    );
    check_matrix(
        "manifest-contract-version",
        manifest_shape,
        |copy| {
            copy.edit_json("expected-matrix.json", |manifest| {
                assign(manifest, "contract_version", Json::string("2.0"));
            });
        },
        "contract_version",
    );
}

fn every_snapshot_validates(matrix: &Matrix) -> Result<(), String> {
    if !schema_is_in_the_supported_subset(SCENARIO_SCHEMA) {
        return Err("the scenario schema uses an unsupported keyword".to_owned());
    }
    for case in &matrix.cases {
        validate_and_check(&case.snapshot).map_err(|reason| format!("{}: {reason}", case.id))?;
    }
    Ok(())
}

#[test]
fn test_every_snapshot_validates_against_scenario_schema() {
    check_matrix(
        "snapshot-schema",
        every_snapshot_validates,
        |copy| {
            copy.edit_json("ai-only-gemini/expected-artifacts.json", |snapshot| {
                assign(snapshot, "mode", Json::string("bogus"));
            });
        },
        "schema findings",
    );
    check_matrix(
        "snapshot-semantics",
        every_snapshot_validates,
        |copy| {
            copy.edit_json("combined-gcp-gemini/expected-artifacts.json", |snapshot| {
                let mut overlapping = at(snapshot, "dimensions.ai_sources.0").clone();
                assign(&mut overlapping, "models", strings(&["gemini-1.5-flash"]));
                push(snapshot, "dimensions.ai_sources", overlapping);
            });
        },
        "duplicate or overlapping AI source",
    );
}

fn supported_axes(matrix: &Matrix) -> Result<(), String> {
    let snapshots: Vec<&Json> = matrix.cases.iter().map(|case| &case.snapshot).collect();
    let supported = at(&matrix.manifest, "supported");
    let providers_of = |snapshot: &Json| -> BTreeSet<String> {
        items(snapshot, "dimensions.ai_sources")
            .iter()
            .map(|source| str_of(source, "provider").to_owned())
            .collect()
    };
    let no_gateways = |snapshot: &Json| items(snapshot, "dimensions.gateways").is_empty();
    let products_of = |snapshot: &Json| -> BTreeSet<String> {
        items(snapshot, "dimensions.gateways")
            .iter()
            .map(|gateway| str_of(gateway, "product").to_owned())
            .collect()
    };
    let mode_is = |snapshot: &Json, mode: &str| str_of(snapshot, "mode") == mode;

    for infrastructure_source in string_list(at(supported, "infrastructure_sources")) {
        let hosted = |mode: &str| {
            snapshots.iter().any(|snapshot| {
                mode_is(snapshot, mode)
                    && at(snapshot, "dimensions.infrastructure_source").as_str()
                        == Some(infrastructure_source.as_str())
            })
        };
        if !hosted("infrastructure_only") || !hosted("combined") {
            return Err(format!(
                "infrastructure source {infrastructure_source} lacks an independent representative"
            ));
        }
    }
    for provider in string_list(at(supported, "direct_ai_providers")) {
        let ai_only = snapshots.iter().any(|snapshot| {
            mode_is(snapshot, "ai_only")
                && no_gateways(snapshot)
                && providers_of(snapshot).contains(&provider)
        });
        let combined = snapshots.iter().any(|snapshot| {
            mode_is(snapshot, "combined") && providers_of(snapshot).contains(&provider)
        });
        if !ai_only || !combined {
            return Err(format!(
                "direct provider {provider} lacks an independent representative"
            ));
        }
    }
    for product in string_list(at(supported, "gateway_products")) {
        let in_mode = |mode: &str| {
            snapshots
                .iter()
                .any(|snapshot| mode_is(snapshot, mode) && products_of(snapshot).contains(&product))
        };
        if !in_mode("ai_only") || !in_mode("combined") {
            return Err(format!(
                "gateway product {product} lacks an independent representative"
            ));
        }
    }
    Ok(())
}

#[test]
fn test_supported_axes_have_independent_representatives() {
    for (label, axis) in [
        ("axis-infrastructure", "infrastructure_sources"),
        ("axis-provider", "direct_ai_providers"),
        ("axis-gateway", "gateway_products"),
    ] {
        check_matrix(
            label,
            supported_axes,
            |copy| {
                copy.edit_json("expected-matrix.json", |manifest| {
                    push(
                        manifest,
                        &format!("supported.{axis}"),
                        Json::string("unrepresented"),
                    );
                });
            },
            "lacks an independent representative",
        );
    }
}

fn openai_provenance(matrix: &Matrix) -> Result<(), String> {
    let direct = at(
        matrix.case("combined-gcp-openai"),
        "dimensions.ai_sources.0",
    );
    let azure = at(
        matrix.case("combined-azure-openai"),
        "dimensions.ai_sources.0",
    );
    if !same(direct, &source("openai", "openai_api", "gpt", &["gpt-4o"])) {
        return Err("the direct OpenAI source lost its endpoint provenance".to_owned());
    }
    if !same(azure, &source("openai", "azure_openai", "gpt", &["gpt-4o"])) {
        return Err("the Azure OpenAI source lost its endpoint provenance".to_owned());
    }
    Ok(())
}

#[test]
fn test_openai_provider_preserves_direct_and_azure_endpoint_provenance() {
    check_matrix(
        "openai-azure-provenance",
        openai_provenance,
        |copy| {
            copy.edit_json(
                "combined-azure-openai/expected-artifacts.json",
                |snapshot| {
                    assign(
                        snapshot,
                        "dimensions.ai_sources.0.source_service",
                        Json::string("openai_api"),
                    );
                },
            );
        },
        "Azure OpenAI source",
    );
    check_matrix(
        "openai-direct-provenance",
        openai_provenance,
        |copy| {
            copy.edit_json("combined-gcp-openai/expected-artifacts.json", |snapshot| {
                assign(
                    snapshot,
                    "dimensions.ai_sources.0.models",
                    strings(&["gpt-4o", "gpt-4o-mini"]),
                );
            });
        },
        "direct OpenAI source",
    );
}

fn shared_artifacts_validate(matrix: &Matrix) -> Result<(), String> {
    for case in &matrix.cases {
        let snapshot = &case.snapshot;
        if str_of(snapshot, "state") != ".phase-status.json" {
            return Err(format!("{}: state is not .phase-status.json", case.id));
        }
        let case_dir = matrix.root.join(&case.dir);
        let state = load_at(&case_dir, str_of(snapshot, "state"));
        if state.get("migration_id") != snapshot.get("migration_id") {
            return Err(format!("{}: migration_id differs from the state", case.id));
        }
        if state.get("run_id") != snapshot.get("run_id") {
            return Err(format!("{}: run_id differs from the state", case.id));
        }
        let phases = match at(&state, "phases") {
            Json::Object(phases) => phases,
            other => panic!("phases is not an object: {other:?}"),
        };
        if phases
            .iter()
            .any(|(_, status)| status.as_str() != Some("completed"))
            || phases.is_empty()
        {
            return Err(format!("{}: a phase is not completed", case.id));
        }
        if !findings(PHASE_STATUS_SCHEMA, &state).is_empty() {
            return Err(format!("{}: the state fails its schema", case.id));
        }
        if case_dir.join("integration-validation.json").is_file() {
            let validation = load_at(&case_dir, "integration-validation.json");
            if !findings(INTEGRATION_SCHEMA, &validation).is_empty() {
                return Err(format!(
                    "{}: integration-validation fails its schema",
                    case.id
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn test_each_case_shared_artifacts_validate_directly() {
    check_matrix(
        "shared-run-id",
        shared_artifacts_validate,
        |copy| {
            copy.edit_json("ai-only-gemini/.phase-status.json", |state| {
                assign(
                    state,
                    "run_id",
                    Json::string("00000000-0000-4000-8000-0000000000ff"),
                );
            });
        },
        "run_id differs",
    );
    check_matrix(
        "shared-phase",
        shared_artifacts_validate,
        |copy| {
            copy.edit_json("infra-only-gcp/.phase-status.json", |state| {
                let Json::Object(phases) = super::documents::node(state, "phases") else {
                    panic!("phases");
                };
                phases[0].1 = Json::string("in_progress");
            });
        },
        "a phase is not completed",
    );
    check_matrix(
        "shared-integration-schema",
        shared_artifacts_validate,
        |copy| {
            copy.edit_json(
                "combined-gcp-gemini/integration-validation.json",
                |validation| {
                    assign(validation, "verdict", Json::string("maybe"));
                },
            );
        },
        "fails its schema",
    );
}

fn required_artifacts_and_verdicts(matrix: &Matrix) -> Result<(), String> {
    let infra_expected = Json::object(vec![
        ("verdict", Json::string("passed")),
        ("artifacts", strings(&INFRA_ARTIFACTS)),
    ]);
    let ai_expected = Json::object(vec![
        ("verdict", Json::string("passed")),
        ("artifacts", strings(&AI_ARTIFACTS)),
    ]);
    let integration_expected = Json::object(vec![
        ("verdict", Json::string("passed")),
        ("artifact", Json::string("integration-validation.json")),
    ]);
    for case in &matrix.cases {
        let snapshot = &case.snapshot;
        let outcomes = at(snapshot, "outcomes");
        let mode = str_of(snapshot, "mode");
        let id = &case.id;
        let infra = at(outcomes, "infrastructure");
        let ai = at(outcomes, "ai");
        let integration = at(outcomes, "integration_validation");
        let wants_infra = matches!(mode, "infrastructure_only" | "combined");
        if wants_infra && !same(infra, &infra_expected) {
            return Err(format!("{id}: wrong infrastructure outcome"));
        }
        if !wants_infra && *infra != Json::Null {
            return Err(format!("{id}: infrastructure outcome must be null"));
        }
        let wants_ai = matches!(mode, "ai_only" | "combined");
        if wants_ai && !same(ai, &ai_expected) {
            return Err(format!("{id}: wrong AI outcome"));
        }
        if !wants_ai && *ai != Json::Null {
            return Err(format!("{id}: AI outcome must be null"));
        }
        let integration_path = matrix
            .root
            .join(&case.dir)
            .join("integration-validation.json");
        if mode == "combined" {
            if !same(integration, &integration_expected) {
                return Err(format!("{id}: wrong integration outcome"));
            }
            // `integration_path.is_file()`: a directory of that name is not the artifact.
            if !integration_path.is_file() {
                return Err(format!("{id}: integration-validation.json is missing"));
            }
        } else {
            if *integration != Json::Null {
                return Err(format!("{id}: integration outcome must be null"));
            }
            // `not integration_path.exists()`: nothing of that name may exist.
            if integration_path.exists() {
                return Err(format!("{id}: unexpected integration-validation.json"));
            }
        }
    }
    Ok(())
}

#[test]
fn test_required_artifacts_and_verdicts_match_each_mode() {
    check_matrix(
        "verdicts-infra",
        required_artifacts_and_verdicts,
        |copy| {
            copy.edit_json("infra-only-gcp/expected-artifacts.json", |snapshot| {
                remove_string(
                    snapshot,
                    "outcomes.infrastructure.artifacts",
                    "validation-report.json",
                );
            });
        },
        "wrong infrastructure outcome",
    );
    check_matrix(
        "verdicts-ai",
        required_artifacts_and_verdicts,
        |copy| {
            copy.edit_json("ai-only-gemini/expected-artifacts.json", |snapshot| {
                assign(snapshot, "outcomes.ai.verdict", Json::string("failed"));
            });
        },
        "wrong AI outcome",
    );
    check_matrix(
        "verdicts-null-infra",
        required_artifacts_and_verdicts,
        |copy| {
            copy.edit_json("ai-only-gemini/expected-artifacts.json", |snapshot| {
                assign(
                    snapshot,
                    "outcomes.infrastructure",
                    Json::object(vec![("verdict", Json::string("passed"))]),
                );
            });
        },
        "infrastructure outcome must be null",
    );
    check_matrix(
        "verdicts-integration-file",
        required_artifacts_and_verdicts,
        |copy| copy.remove("combined-gcp-gemini/integration-validation.json"),
        "integration-validation.json is missing",
    );
    check_matrix(
        "verdicts-integration-directory",
        required_artifacts_and_verdicts,
        |copy| copy.replace_with_dir("combined-gcp-gemini/integration-validation.json"),
        "integration-validation.json is missing",
    );
    check_matrix(
        "verdicts-stray-integration-file",
        required_artifacts_and_verdicts,
        |copy| copy.write("infra-only-gcp/integration-validation.json", "{}"),
        "unexpected integration-validation.json",
    );
}

fn combined_validation(matrix: &Matrix) -> Result<(), String> {
    let infra: BTreeSet<String> = INFRA_ARTIFACTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let ai: BTreeSet<String> = AI_ARTIFACTS.iter().map(|name| (*name).to_owned()).collect();
    let checks: BTreeSet<String> = INTEGRATION_CHECKS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut combined = 0;
    for case in &matrix.cases {
        if str_of(&case.snapshot, "mode") != "combined" {
            continue;
        }
        combined += 1;
        let id = &case.id;
        let validation = load_at(&matrix.root.join(&case.dir), "integration-validation.json");
        if str_of(&validation, "schema_version") != "1.0" {
            return Err(format!("{id}: schema_version"));
        }
        if validation.get("run_id") != case.snapshot.get("run_id") {
            return Err(format!("{id}: run_id"));
        }
        if str_of(&validation, "verdict") != "passed" {
            return Err(format!("{id}: verdict"));
        }
        let tracks: BTreeSet<String> = match at(&validation, "track_results") {
            Json::Object(tracks) => tracks.iter().map(|(name, _)| name.clone()).collect(),
            other => panic!("{other:?}"),
        };
        if tracks != BTreeSet::from(["infrastructure".to_owned(), "ai".to_owned()]) {
            return Err(format!("{id}: track_results are not infrastructure and ai"));
        }
        if string_set(at(&validation, "track_results.infrastructure.references")) != infra {
            return Err(format!("{id}: infrastructure references"));
        }
        if string_set(at(&validation, "track_results.ai.references")) != ai {
            return Err(format!("{id}: ai references"));
        }
        let names: BTreeSet<String> = match at(&validation, "checks") {
            Json::Object(entries) => entries.iter().map(|(name, _)| name.clone()).collect(),
            other => panic!("{other:?}"),
        };
        if names != checks {
            return Err(format!("{id}: the checks are not the nine categories"));
        }
        for section in ["track_results", "checks"] {
            let Json::Object(results) = at(&validation, section) else {
                panic!("{section}");
            };
            for (name, result) in results {
                if str_of(result, "verdict") != "passed" {
                    return Err(format!("{id}: {section}.{name} did not pass"));
                }
                if items(result, "references").is_empty() {
                    return Err(format!("{id}: {section}.{name} has no references"));
                }
            }
        }
    }
    if combined == 0 {
        return Err("no combined scenario".to_owned());
    }
    Ok(())
}

#[test]
fn test_combined_validation_references_both_tracks_and_all_categories() {
    check_matrix(
        "combined-failed-check",
        combined_validation,
        |copy| {
            copy.edit_json(
                "combined-gcp-gemini/integration-validation.json",
                |validation| {
                    assign(validation, "checks.cutover.verdict", Json::string("failed"));
                },
            );
        },
        "did not pass",
    );
    check_matrix(
        "combined-empty-references",
        combined_validation,
        |copy| {
            copy.edit_json(
                "combined-gcp-gemini/integration-validation.json",
                |validation| {
                    assign(
                        validation,
                        "checks.networking.references",
                        Json::Array(Vec::new()),
                    );
                },
            );
        },
        "has no references",
    );
    check_matrix(
        "combined-tracks",
        combined_validation,
        |copy| {
            copy.edit_json(
                "combined-gcp-gemini/integration-validation.json",
                |validation| {
                    remove_string(
                        validation,
                        "track_results.ai.references",
                        "generation-ai.json",
                    );
                },
            );
        },
        "ai references",
    );
    check_matrix(
        "combined-categories",
        combined_validation,
        |copy| {
            copy.edit_json(
                "combined-gcp-gemini/integration-validation.json",
                |validation| {
                    super::support::assign(
                        validation,
                        "run_id",
                        Json::string("00000000-0000-4000-8000-0000000000ff"),
                    );
                },
            );
        },
        "run_id",
    );
}

// ---- synthetic compositions -----------------------------------------------------------------------

#[test]
fn test_heroku_composes_with_each_direct_ai_provider() {
    let matrix = Matrix::real();
    for association in [
        ("google", "vertex_ai", "gemini", "gemini-1.5-pro"),
        ("openai", "openai_api", "gpt", "gpt-4o"),
        ("anthropic", "anthropic_api", "claude", ANTHROPIC_MODEL),
    ] {
        let scenario = synthetic(&matrix, "heroku", association, false);
        validate_and_check(&scenario).unwrap_or_else(|reason| panic!("{association:?}: {reason}"));
        // Non-vacuous: the same composition with a fabricated model id or with a
        // gateway-only provider is refused.
        let fabricated = edited(&scenario, |s| {
            assign(s, "dimensions.ai_sources.0.models", strings(&["unknown"]));
        });
        assert!(validate_and_check(&fabricated).is_err(), "{association:?}");
        let gateway_only = edited(&scenario, |s| {
            assign(
                s,
                "dimensions.ai_sources.0.provider",
                Json::string("openrouter"),
            );
        });
        assert!(
            validate_and_check(&gateway_only).is_err(),
            "{association:?}"
        );
    }
}

#[test]
fn test_openrouter_dimensions_compose_with_each_infrastructure_source() {
    let matrix = Matrix::real();
    for infrastructure_source in ["gcp", "azure", "heroku"] {
        let scenario = multi_openrouter(&matrix, infrastructure_source);
        assert!(at(&scenario, "dimensions.infrastructure_source")
            .as_str()
            .is_some());
        validate_and_check(&scenario)
            .unwrap_or_else(|reason| panic!("{infrastructure_source}: {reason}"));
        // Non-vacuous: a sentinel or the gateway product as the infrastructure source,
        // and an array of sources, are refused.
        for bad in [
            Json::string("both"),
            Json::string("openrouter"),
            strings(&["gcp", "heroku"]),
        ] {
            let broken = edited(&scenario, |s| {
                assign(s, "dimensions.infrastructure_source", bad.clone());
            });
            assert!(validate_and_check(&broken).is_err(), "{bad:?}");
        }
    }
}

/// The assertions of `test_multi_cardinality_openrouter_preserves_exact_associations`.
fn multi_cardinality_preserved(scenario: &Json) -> Result<(), String> {
    let sources = items(scenario, "dimensions.ai_sources");
    let upstreams = items(scenario, "dimensions.gateways.0.upstreams");
    let distinct = |key: &str| -> BTreeSet<String> {
        sources
            .iter()
            .flat_map(|source| string_list(at(source, key)))
            .collect()
    };
    let providers: BTreeSet<&str> = sources.iter().map(|s| str_of(s, "provider")).collect();
    if providers.len() <= 1 {
        return Err("fewer than two providers".to_owned());
    }
    if distinct("model_families").len() <= 1 {
        return Err("fewer than two families".to_owned());
    }
    if distinct("models").len() <= 1 {
        return Err("fewer than two models".to_owned());
    }
    let claude_models = upstreams
        .iter()
        .filter(|upstream| str_of(upstream, "provider") == "anthropic")
        .flat_map(|upstream| items(upstream, "model_families"))
        .find(|family| str_of(family, "family") == "claude")
        .map(|family| string_list(at(family, "models")))
        .ok_or_else(|| "no claude family under the anthropic upstream".to_owned())?;
    if claude_models != ["claude-3-5-sonnet-20240620", "claude-3-haiku-20240307"] {
        return Err(format!("claude models are {claude_models:?}"));
    }
    // The upstream associations equal the source associations exactly.
    let association = |entry: &Json, family_of: &dyn Fn(&Json) -> Vec<(String, Vec<String>)>| {
        (
            (
                str_of(entry, "provider").to_owned(),
                str_of(entry, "source_service").to_owned(),
            ),
            family_of(entry),
        )
    };
    let mut upstream_view: Vec<_> = upstreams
        .iter()
        .map(|entry| {
            association(entry, &|u| {
                items(u, "model_families")
                    .iter()
                    .map(|f| (str_of(f, "family").to_owned(), string_list(at(f, "models"))))
                    .collect()
            })
        })
        .collect();
    let mut source_view: Vec<_> = sources
        .iter()
        .map(|entry| {
            association(entry, &|s| {
                vec![(
                    string_list(at(s, "model_families"))[0].clone(),
                    string_list(at(s, "models")),
                )]
            })
        })
        .collect();
    upstream_view.sort();
    source_view.sort();
    if upstream_view != source_view {
        return Err("the upstream associations differ from the retained sources".to_owned());
    }
    let openai_services: Vec<&str> = sources
        .iter()
        .filter(|source| str_of(source, "provider") == "openai")
        .map(|source| str_of(source, "source_service"))
        .collect();
    if openai_services != ["openai_api", "azure_openai"] {
        return Err(format!("openai services are {openai_services:?}"));
    }
    validate_and_check(scenario)
}

#[test]
fn test_multi_cardinality_openrouter_preserves_exact_associations() {
    let matrix = Matrix::real();
    let scenario = multi_openrouter(&matrix, "heroku");
    multi_cardinality_preserved(&scenario).expect("the multi-cardinality scenario");
    let cases: [(&str, fn(&mut Json), &str); 5] = [
        (
            "one provider only",
            |s| {
                assign(
                    s,
                    "dimensions.ai_sources",
                    Json::Array(vec![source(
                        "anthropic",
                        "anthropic_api",
                        "claude",
                        &["claude-3-5-sonnet-20240620", "claude-3-haiku-20240307"],
                    )]),
                );
                assign(
                    s,
                    "dimensions.gateways",
                    Json::Array(vec![gateway(vec![upstream(
                        "anthropic",
                        "anthropic_api",
                        "claude",
                        &["claude-3-5-sonnet-20240620", "claude-3-haiku-20240307"],
                    )])]),
                );
            },
            "fewer than two providers",
        ),
        (
            "claude models reordered",
            |s| {
                assign(
                    s,
                    "dimensions.gateways.0.upstreams.0.model_families.0.models",
                    strings(&["claude-3-haiku-20240307", "claude-3-5-sonnet-20240620"]),
                );
            },
            "claude models are",
        ),
        (
            "an upstream model dropped",
            |s| {
                assign(
                    s,
                    "dimensions.gateways.0.upstreams.1.model_families.0.models",
                    strings(&["gemini-1.5-flash"]),
                );
            },
            "differ from the retained sources",
        ),
        (
            "openai services reordered",
            |s| {
                let Json::Array(sources) = super::documents::node(s, "dimensions.ai_sources")
                else {
                    panic!("sources");
                };
                sources.swap(2, 3);
            },
            "openai services are",
        ),
        (
            "gateway removed",
            |s| {
                let Json::Array(upstreams) =
                    super::documents::node(s, "dimensions.gateways.0.upstreams")
                else {
                    panic!("upstreams");
                };
                upstreams.truncate(1);
            },
            "differ from the retained sources",
        ),
    ];
    for (label, edit, expected) in cases {
        let broken = edited(&scenario, edit);
        let reason =
            multi_cardinality_preserved(&broken).expect_err(&format!("{label}: still preserved"));
        assert!(reason.contains(expected), "{label}: {reason}");
    }
}

fn future_identity(manifest: &Json, matrix: &Matrix) -> Result<(), String> {
    let mut scenario = synthetic(
        matrix,
        "future_platform",
        (
            "future_provider",
            "future_api",
            "future_family",
            "future-model-v1",
        ),
        false,
    );
    push(
        &mut scenario,
        "dimensions.ai_sources",
        source("anthropic", "anthropic_api", "claude", &[ANTHROPIC_MODEL]),
    );
    validate_and_check(&scenario)?;
    if string_list(at(manifest, "supported.direct_ai_providers"))
        .iter()
        .any(|provider| provider == "future_provider")
    {
        return Err("future_provider is claimed as supported".to_owned());
    }
    Ok(())
}

#[test]
fn test_future_structural_identity_does_not_claim_runtime_support() {
    let matrix = Matrix::real();
    future_identity(&matrix.manifest, &matrix).expect("the committed manifest");
    let claimed = edited(&matrix.manifest, |manifest| {
        push(
            manifest,
            "supported.direct_ai_providers",
            Json::string("future_provider"),
        );
    });
    let reason = future_identity(&claimed, &matrix).expect_err("a claimed provider");
    assert!(reason.contains("claimed as supported"), "{reason}");
}

// ---- duplicates, overlaps, pairing -------------------------------------------------------------

#[test]
fn test_duplicate_or_overlapping_ai_sources_are_rejected() {
    let matrix = Matrix::real();
    let base = synthetic(
        &matrix,
        "heroku",
        (
            "anthropic",
            "anthropic_api",
            "claude",
            "claude-3-5-sonnet-20240620",
        ),
        false,
    );
    assert!(!refused(&base), "the control composition");
    let exact = edited(&base, |s| {
        let duplicate = at(s, "dimensions.ai_sources.0").clone();
        push(s, "dimensions.ai_sources", duplicate);
    });
    let overlapping = edited(&base, |s| {
        push(
            s,
            "dimensions.ai_sources",
            source(
                "anthropic",
                "anthropic_api",
                "claude",
                &["claude-3-haiku-20240307"],
            ),
        );
    });
    assert!(refused(&exact));
    assert!(refused(&overlapping));
    // The overlapping spelling is not a byte-identical duplicate: the schema accepts
    // it and the semantic oracle is what refuses it.
    assert!(scenario_findings(&overlapping).is_empty());
    assert!(scenario_semantics(&overlapping).is_err());
}

#[test]
fn test_duplicate_or_overlapping_gateway_upstreams_are_rejected() {
    let matrix = Matrix::real();
    let base = multi_openrouter(&matrix, "heroku");
    assert!(!refused(&base), "the control composition");
    for overlap in ["exact", "split", "ambiguous-service"] {
        let scenario = edited(&base, |s| {
            let extra = match overlap {
                "exact" => at(s, "dimensions.gateways.0.upstreams.0").clone(),
                "split" => upstream(
                    "anthropic",
                    "anthropic_api",
                    "claude",
                    &["claude-3-haiku-20240307"],
                ),
                _ => object(vec![
                    ("provider", Json::string("openai")),
                    (
                        "model_families",
                        Json::Array(vec![family_entry("gpt", &["gpt-4o"])]),
                    ),
                ]),
            };
            gateway_upstreams(s).push(extra);
        });
        assert!(refused(&scenario), "{overlap}");
    }
}

#[test]
fn test_overlapping_explicit_and_service_less_upstream_for_one_retained_source_is_rejected() {
    let matrix = Matrix::real();
    let base = matrix.case("ai-only-openrouter").clone();
    assert!(!refused(&base), "the control scenario");
    for explicit_service_first in [true, false] {
        let scenario = edited(&base, |s| {
            let explicit = at(s, "dimensions.gateways.0.upstreams.0").clone();
            let service_less = object(vec![
                ("provider", Json::string("openai")),
                ("model_families", at(&explicit, "model_families").clone()),
            ]);
            let ordered = if explicit_service_first {
                vec![explicit, service_less]
            } else {
                vec![service_less, explicit]
            };
            assign(s, "dimensions.gateways.0.upstreams", Json::Array(ordered));
        });
        assert!(
            refused(&scenario),
            "explicit first: {explicit_service_first}"
        );
        // It is the resolved association the oracle refuses, not a schema violation.
        assert!(scenario_findings(&scenario).is_empty());
        let reason = scenario_semantics(&scenario).expect_err("the overlap");
        assert!(
            reason.contains("duplicate or overlapping gateway upstream"),
            "{reason}"
        );
    }
}

#[test]
fn test_gateway_upstreams_must_match_retained_ai_sources_exactly() {
    let matrix = Matrix::real();
    let base = synthetic(
        &matrix,
        "heroku",
        ("openai", "openai_api", "gpt", "gpt-4o"),
        true,
    );
    scenario_semantics(&base).expect("the control composition");
    let mutations: [fn(&mut Json); 4] = [
        |s| {
            assign(
                s,
                "dimensions.gateways.0.upstreams.0.provider",
                Json::string("google"),
            );
        },
        |s| {
            assign(
                s,
                "dimensions.gateways.0.upstreams.0.source_service",
                Json::string("azure_openai"),
            );
        },
        |s| {
            assign(
                s,
                "dimensions.gateways.0.upstreams.0.model_families.0.family",
                Json::string("claude"),
            );
        },
        |s| {
            assign(
                s,
                "dimensions.gateways.0.upstreams.0.model_families.0.models",
                strings(&[ANTHROPIC_MODEL]),
            );
        },
    ];
    for (index, mutation) in mutations.into_iter().enumerate() {
        let broken = edited(&base, mutation);
        assert!(scenario_semantics(&broken).is_err(), "mutation {index}");
    }
}

#[test]
fn test_gateway_upstreams_cannot_omit_source_model_details() {
    let matrix = Matrix::real();
    let mutations: [fn(&mut Json); 3] = [
        |s| {
            let path = "dimensions.gateways.0.upstreams.0.model_families.1.models";
            remove_string(s, path, "claude-3-haiku-20240307");
        },
        |s| {
            let Json::Array(families) =
                super::documents::node(s, "dimensions.gateways.0.upstreams.0.model_families")
            else {
                panic!("families");
            };
            families.pop();
        },
        |s| {
            let family = "dimensions.gateways.0.upstreams.0.model_families.1";
            assign(
                s,
                &format!("{family}.model_evidence"),
                Json::string("not_observed"),
            );
            assign(s, &format!("{family}.models"), Json::Array(Vec::new()));
        },
    ];
    for (index, mutation) in mutations.into_iter().enumerate() {
        let mut scenario = multi_openrouter(&matrix, "heroku");
        // The Anthropic source and upstream are split over two families.
        assign(
            &mut scenario,
            "dimensions.ai_sources.0.model_families",
            strings(&["claude", "claude_haiku"]),
        );
        assign(
            &mut scenario,
            "dimensions.gateways.0.upstreams.0.model_families",
            Json::Array(vec![
                family_entry("claude", &[ANTHROPIC_MODEL]),
                family_entry("claude_haiku", &["claude-3-haiku-20240307"]),
            ]),
        );
        validate_and_check(&scenario).expect("the split-family control");
        mutation(&mut scenario);
        assert!(scenario_semantics(&scenario).is_err(), "mutation {index}");
    }
}

// ---- the schema rejects invalid shapes -------------------------------------------------------------

#[test]
fn test_schema_rejects_invalid_modes_ids_cardinality_and_sentinels() {
    let matrix = Matrix::real();
    let base = matrix.case("combined-heroku-anthropic").clone();
    assert!(scenario_findings(&base).is_empty(), "the control scenario");
    let anthropic = || upstream("anthropic", "anthropic_api", "claude", &[ANTHROPIC_MODEL]);
    let gateway_with = |product: &str, evidence: &str, upstreams: Vec<Json>| {
        Json::Array(vec![object(vec![
            ("product", Json::string(product)),
            ("type", Json::string("llm_router")),
            ("upstream_evidence", Json::string(evidence)),
            ("upstreams", Json::Array(upstreams)),
        ])])
    };
    let source_edit = |key: &'static str, value: Json| {
        move |s: &mut Json| assign(s, &format!("dimensions.ai_sources.0.{key}"), value.clone())
    };
    let mutations: Vec<(&str, Box<dyn Fn(&mut Json)>)> = vec![
        (
            "mode ai_only",
            Box::new(|s| assign(s, "mode", Json::string("ai_only"))),
        ),
        (
            "no infrastructure source",
            Box::new(|s| assign(s, "dimensions.infrastructure_source", Json::Null)),
        ),
        (
            "two infrastructure sources",
            Box::new(|s| {
                assign(
                    s,
                    "dimensions.infrastructure_source",
                    strings(&["heroku", "gcp"]),
                );
            }),
        ),
        (
            "empty ai_only scenario",
            Box::new(|s| {
                assign(s, "mode", Json::string("ai_only"));
                assign(s, "dimensions.infrastructure_source", Json::Null);
                assign(s, "dimensions.ai_sources", Json::Array(Vec::new()));
                assign(s, "dimensions.gateways", Json::Array(Vec::new()));
                assign(s, "outcomes.infrastructure", Json::Null);
                assign(s, "outcomes.ai", Json::Null);
                assign(s, "outcomes.integration_validation", Json::Null);
            }),
        ),
        (
            "no ai sources",
            Box::new(|s| assign(s, "dimensions.ai_sources", Json::Array(Vec::new()))),
        ),
        (
            "no infrastructure outcome",
            Box::new(|s| assign(s, "outcomes.infrastructure", Json::Null)),
        ),
        (
            "no ai outcome",
            Box::new(|s| assign(s, "outcomes.ai", Json::Null)),
        ),
        (
            "no integration outcome",
            Box::new(|s| assign(s, "outcomes.integration_validation", Json::Null)),
        ),
        (
            "infrastructure both",
            Box::new(|s| assign(s, "dimensions.infrastructure_source", Json::string("both"))),
        ),
        (
            "infrastructure openrouter",
            Box::new(|s| {
                assign(
                    s,
                    "dimensions.infrastructure_source",
                    Json::string("openrouter"),
                );
            }),
        ),
        (
            "provider both",
            Box::new(source_edit("provider", Json::string("both"))),
        ),
        (
            "provider openrouter",
            Box::new(source_edit("provider", Json::string("openrouter"))),
        ),
        (
            "service both",
            Box::new(source_edit("source_service", Json::string("both"))),
        ),
        (
            "no families",
            Box::new(source_edit("model_families", Json::Array(Vec::new()))),
        ),
        (
            "provider case",
            Box::new(source_edit("provider", Json::string("OpenAI"))),
        ),
        (
            "service dash",
            Box::new(source_edit("source_service", Json::string("openai-api"))),
        ),
        (
            "family case",
            Box::new(source_edit("model_families", strings(&["GPT"]))),
        ),
        (
            "model case",
            Box::new(source_edit("models", strings(&["GPT-4o"]))),
        ),
        (
            "model unknown",
            Box::new(source_edit("models", strings(&["unknown"]))),
        ),
        (
            "no models",
            Box::new(source_edit("models", Json::Array(Vec::new()))),
        ),
        (
            "model evidence not_observed",
            Box::new(source_edit("model_evidence", Json::string("not_observed"))),
        ),
        (
            "gateway product both",
            Box::new(move |s| {
                assign(
                    s,
                    "dimensions.gateways",
                    gateway_with("both", "observed", vec![anthropic()]),
                );
            }),
        ),
        (
            "observed gateway without upstreams",
            Box::new(move |s| {
                assign(
                    s,
                    "dimensions.gateways",
                    gateway_with("openrouter", "observed", Vec::new()),
                );
            }),
        ),
        (
            "unresolved gateway with upstreams",
            Box::new(move |s| {
                assign(
                    s,
                    "dimensions.gateways",
                    gateway_with("openrouter", "unresolved", vec![anthropic()]),
                );
            }),
        ),
    ];
    assert_eq!(mutations.len(), 24);
    for (label, mutation) in &mutations {
        let broken = edited(&base, |s| mutation(s));
        assert!(
            !scenario_findings(&broken).is_empty(),
            "the schema accepts: {label}"
        );
    }
}

#[test]
fn test_unresolved_gateway_upstreams_are_explicit_and_unambiguous() {
    let matrix = Matrix::real();
    let mut scenario = synthetic(
        &matrix,
        "heroku",
        ("openai", "openai_api", "gpt", "gpt-4o"),
        true,
    );
    assign(
        &mut scenario,
        "dimensions.gateways",
        Json::Array(vec![object(vec![
            ("product", Json::string("openrouter")),
            ("type", Json::string("llm_router")),
            ("upstream_evidence", Json::string("unresolved")),
            ("upstreams", Json::Array(Vec::new())),
        ])]),
    );
    validate_and_check(&scenario).expect("an explicitly unresolved gateway");
    let observed = edited(&scenario, |s| {
        assign(
            s,
            "dimensions.gateways.0.upstream_evidence",
            Json::string("observed"),
        );
    });
    assert!(!scenario_findings(&observed).is_empty());
    let missing = edited(&scenario, |s| {
        let Json::Array(gateways) = super::documents::node(s, "dimensions.gateways") else {
            panic!("gateways");
        };
        let Json::Object(members) = &mut gateways[0] else {
            panic!("gateway");
        };
        members.retain(|(key, _)| key != "upstream_evidence");
    });
    assert!(!scenario_findings(&missing).is_empty());
}

// ---- integration-validation through the CLI -------------------------------------------------------------

/// `_run_artifact_validator`: the artifact validator as `gcp-to-aws` over a run directory
/// holding `validation` as its only artifact; the exit status and the codes of the findings.
fn run_integration_validator(label: &str, validation: &Json) -> (u8, Vec<String>) {
    let dir = TempDir::new(label);
    write(
        &dir,
        "integration-validation.json",
        &dumps(validation, None, false),
    );
    let plugin = plugin_arg();
    let (status, stdout, stderr) = artifacts(&[
        "--run-dir",
        text(&dir),
        "--skill",
        "gcp-to-aws",
        "--no-baseline",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert!(!stdout.is_empty(), "{stderr}");
    let _ = json(&stdout);
    let codes = error_pairs(&stdout)
        .into_iter()
        .map(|(code, _)| code)
        .collect();
    (status, codes)
}

fn integration_fixture() -> Json {
    load(&format!(
        "{ROOT}/combined-gcp-gemini/integration-validation.json"
    ))
}

#[test]
fn test_integration_validation_accepts_uppercase_and_lowercase_run_ids() {
    for (label, run_id, expected_status) in [
        ("uppercase", "ABCDEF12-ABCD-4ABC-8DEF-ABCDEF123456", 0),
        ("lowercase", "abcdef12-abcd-4abc-8def-abcdef123456", 0),
        ("malformed", "not-a-uuid", 1),
    ] {
        let validation = edited(&integration_fixture(), |v| {
            assign(v, "run_id", Json::string(run_id));
        });
        let (status, codes) = run_integration_validator(label, &validation);
        assert_eq!(status, expected_status, "{label}: {codes:?}");
        if expected_status == 0 {
            assert!(codes.is_empty(), "{label}: {codes:?}");
        } else {
            assert!(
                codes.iter().any(|code| code == "SCHEMA_VIOLATION"),
                "{label}: {codes:?}"
            );
        }
    }
}

#[test]
fn test_success_verdict_rejects_any_failed_child() {
    let (status, codes) = run_integration_validator("failed-control", &integration_fixture());
    assert_eq!(
        (status, codes.len()),
        (0, 0),
        "the unedited artifact is accepted"
    );
    for (section, child) in [
        ("track_results", "infrastructure"),
        ("track_results", "ai"),
        ("checks", "iam_and_secrets"),
    ] {
        let invalid = edited(&integration_fixture(), |v| {
            assign(
                v,
                &format!("{section}.{child}.verdict"),
                Json::string("failed"),
            );
        });
        let (status, codes) = run_integration_validator(&format!("{section}-{child}"), &invalid);
        assert_eq!(status, 1, "{section}.{child}: {codes:?}");
        assert!(
            codes.iter().any(|code| code == "SCHEMA_VIOLATION"),
            "{section}.{child}: {codes:?}"
        );
    }
}

#[test]
fn test_success_verdict_rejects_empty_references() {
    let invalid = edited(&integration_fixture(), |v| {
        assign(v, "checks.networking.references", Json::Array(Vec::new()));
    });
    let (status, codes) = run_integration_validator("empty-references", &invalid);
    assert_eq!(status, 1, "{codes:?}");
    assert!(
        codes.iter().any(|code| code == "SCHEMA_VIOLATION"),
        "{codes:?}"
    );
    assert_rejected(INTEGRATION_SCHEMA, &integration_fixture(), |v| {
        assign(v, "checks.networking.references", Json::Array(Vec::new()));
    });
}

#[test]
fn test_an_exact_duplicate_snapshot_source_is_refused_by_the_schema() {
    // The byte-identical duplicate is a uniqueItems violation: the schema refuses it before
    // the semantic oracle runs.
    check_matrix(
        "snapshot-unique-items",
        every_snapshot_validates,
        |copy| {
            copy.edit_json("combined-gcp-gemini/expected-artifacts.json", |snapshot| {
                let duplicate = at(snapshot, "dimensions.ai_sources.0").clone();
                push(snapshot, "dimensions.ai_sources", duplicate);
            });
        },
        "uniqueItems",
    );
}
