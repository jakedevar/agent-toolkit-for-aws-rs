//! The document-specific cases of `tests/test_validate_artifacts.py` (the
//! review-finding regressions that mutate a documented example, a fixture or a
//! golden run and run the CLI against the real contract documents).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::Json;

use super::{artifacts, error_pairs, json, plugin_arg, real_plugin, text, TempDir};
use crate::startup::artifacts::contracts::{parse_block, scan_doc};
use crate::startup::artifacts::jsonc::strip_jsonc;
use crate::startup::artifacts::patterns::{enum_from_string, heading_matches};
use crate::startup::artifacts::pyjson::dumps;

static RUNS: AtomicUsize = AtomicUsize::new(0);

type Errors = BTreeSet<(String, String)>;

const AZURE: &str = "azure-to-aws";
const GCP: &str = "gcp-to-aws";
const HEROKU: &str = "heroku-to-aws";
const QUIET: &[&str] = &["--no-baseline"];
const SHIPPED: &[&str] = &[];

// ---- helpers ---------------------------------------------------------------

fn read(rel: &str) -> String {
    fs::read_to_string(real_plugin().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// A JSON file below `fixtures/`.
fn fixture(rel: &str) -> Json {
    json(&read(&format!("fixtures/{rel}")))
}

fn errs(items: &[(&str, &str)]) -> Errors {
    items
        .iter()
        .map(|(code, path)| ((*code).to_string(), (*path).to_string()))
        .collect()
}

fn minus(left: &Errors, right: &Errors) -> Errors {
    left.difference(right).cloned().collect()
}

fn has(errors: &Errors, code: &str, path: &str) -> bool {
    errors.contains(&(code.to_string(), path.to_string()))
}

/// The value at a dotted path (array indexes are numbers; `""` is the root).
pub(super) fn node<'a>(value: &'a mut Json, path: &str) -> &'a mut Json {
    let mut current = value;
    for part in path.split('.').filter(|part| !part.is_empty()) {
        current = match current {
            Json::Object(members) => members
                .iter_mut()
                .find(|(key, _)| key.as_str() == part)
                .map_or_else(|| panic!("no key {part} in {path}"), |(_, inner)| inner),
            Json::Array(items) => {
                let index: usize = part.parse().expect("array index");
                &mut items[index]
            }
            other => panic!("cannot descend into {other:?} at {part}"),
        };
    }
    current
}

/// Sets (or adds) the member `key` of the object at `path`.
pub(super) fn put(value: &mut Json, path: &str, key: &str, new: Json) {
    let Json::Object(members) = node(value, path) else {
        panic!("{path} is not an object");
    };
    match members.iter_mut().find(|(name, _)| name.as_str() == key) {
        Some(slot) => slot.1 = new,
        None => members.push((key.to_string(), new)),
    }
}

/// Removes the member `key` of the object at `path`.
pub(super) fn remove(value: &mut Json, path: &str, key: &str) -> Option<Json> {
    let Json::Object(members) = node(value, path) else {
        panic!("{path} is not an object");
    };
    let position = members.iter().position(|(name, _)| name == key)?;
    Some(members.remove(position).1)
}

fn member(value: &Json, key: &str) -> Json {
    value
        .get(key)
        .unwrap_or_else(|| panic!("no member {key}"))
        .clone()
}

/// `target.update(source)` for objects.
fn update(target: &mut Json, source: &Json) {
    let Json::Object(members) = source else {
        panic!("not an object");
    };
    for (key, value) in members {
        put(target, "", key, value.clone());
    }
}

fn strings(items: &[&str]) -> Json {
    Json::Array(items.iter().map(|item| Json::string(*item)).collect())
}

/// Runs the CLI over a run directory and returns the status and the
/// `(code, path)` pairs of the report.
fn run_dir(run: &Path, skill: &str, flags: &[&str]) -> (u8, Errors) {
    let plugin = plugin_arg();
    let mut args = vec!["--run-dir", text(run), "--skill", skill];
    args.extend_from_slice(flags);
    args.extend(["--json", "--plugin-root", plugin.as_str()]);
    let (status, stdout, stderr) = artifacts(&args);
    assert!(!stdout.is_empty(), "{stderr}");
    (status, error_pairs(&stdout).into_iter().collect())
}

fn errors_of_dir(run: &Path, skill: &str, flags: &[&str]) -> Errors {
    run_dir(run, skill, flags).1
}

/// Writes one artifact into a fresh run directory and validates it.
fn run_status(
    tmp: &Path,
    artifact: &str,
    data: &Json,
    skill: &str,
    flags: &[&str],
) -> (u8, Errors) {
    let run = tmp.join(format!("run{}", RUNS.fetch_add(1, Ordering::Relaxed)));
    let target = run.join(artifact);
    fs::create_dir_all(target.parent().expect("artifact parent")).expect("create run dir");
    fs::write(&target, dumps(data, None, false)).expect("write artifact");
    run_dir(&run, skill, flags)
}

fn run_errors(tmp: &Path, artifact: &str, data: &Json, skill: &str, flags: &[&str]) -> Errors {
    run_status(tmp, artifact, data, skill, flags).1
}

/// The first block under `heading` with `a|b` alternations resolved to `a`.
fn doc_example(doc_rel: &str, heading: &str) -> Json {
    let (blocks, _) = scan_doc(&read(doc_rel));
    let block = blocks
        .iter()
        .find(|block| heading_matches(&block.heading, heading))
        .unwrap_or_else(|| panic!("{doc_rel} has no block under {heading}"));
    let mut findings = Vec::new();
    let (value, _) = parse_block(block, doc_rel, &mut findings).expect("the example parses");
    assert!(findings.is_empty(), "{findings:?}");
    pick(&value)
}

fn pick(value: &Json) -> Json {
    match value {
        Json::Object(members) => Json::Object(
            members
                .iter()
                .map(|(key, inner)| (key.clone(), pick(inner)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(pick).collect()),
        Json::String(text) if text.contains('|') && enum_from_string(text).is_some() => {
            Json::string(text.split('|').next().unwrap_or(text))
        }
        other => other.clone(),
    }
}

/// The JSON of the blocks under `heading`, in document order.
fn doc_blocks(doc_rel: &str, heading: &str) -> Vec<Json> {
    let (blocks, _) = scan_doc(&read(doc_rel));
    blocks
        .iter()
        .filter(|block| heading_matches(&block.heading, heading))
        .map(|block| json(&strip_jsonc(&block.text).json))
        .collect()
}

const ESTIMATE_DOC: &str = "skills/gcp-to-aws/references/shared/schema-estimate-infra.md";

/// A complete GCP estimate carrying the documented extensions.
fn extended_gcp_estimate() -> Json {
    let mut est = fixture("gcp-decision-gate/after-decide-complete/estimation-infra.json");
    let sections = doc_blocks(ESTIMATE_DOC, "Security Baseline Entries in");
    assert_eq!(sections.len(), 2, "two Security Baseline blocks");
    let deltas = member(&doc_example(ESTIMATE_DOC, "Cost tiers"), "scenario_deltas");
    put(&mut est, "projected_costs", "scenario_deltas", deltas);
    let breakdown = Json::object(vec![
        ("compute", json(r#"{"service": "Fargate", "monthly": 71}"#)),
        (
            "observability",
            doc_example(ESTIMATE_DOC, "Observability Entry in"),
        ),
        ("security_baseline", sections[0].clone()),
        ("security_baseline_compliance", sections[1].clone()),
    ]);
    put(&mut est, "projected_costs", "breakdown", breakdown);
    est
}

// ---- the cases ---------------------------------------------------------------

#[test]
fn the_documented_default_run_dir_command_passes_for_a_valid_run() {
    let tmp = TempDir::new("default-run");
    let status = fixture("gcp-decision-gate/after-decide-complete/.phase-status.json");
    let run = tmp.join("run");
    fs::create_dir_all(&run).expect("mkdir");
    fs::write(run.join(".phase-status.json"), dumps(&status, None, false)).expect("write");
    let plugin = plugin_arg();
    let (code, stdout, stderr) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        GCP,
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let report = json(&stdout);
    assert_eq!(report.get("errors"), Some(&Json::Array(vec![])));
    assert_eq!(
        report.get("stale_baseline_entries"),
        Some(&Json::Array(vec![]))
    );
    // The real fixture run passes the same way as a run directory.
    let golden = real_plugin().join("fixtures/azure-iac-terraform/after-discover");
    let (code, errors) = run_dir(&golden, AZURE, SHIPPED);
    assert_eq!(code, 0, "{errors:?}");
}

#[test]
fn heroku_complete_example_enums_are_enforced_after_merge() {
    let tmp = TempDir::new("heroku-enums");
    let doc = "skills/heroku-to-aws/references/shared/schema-discover-heroku.md";
    let good = doc_example(doc, "Complete Example");
    let (code, errors) = run_status(&tmp, "heroku-resource-inventory.json", &good, HEROKU, QUIET);
    assert_eq!(code, 0, "{errors:?}");
    let Some(Json::Array(resources)) = good.get("resources") else {
        panic!("resources");
    };
    let formation = resources
        .iter()
        .position(|r| r.get("resource_type").and_then(Json::as_str) == Some("formation"))
        .expect("a formation resource");
    let mutants = [
        (
            "metadata.confidence".to_string(),
            "metadata".to_string(),
            "confidence",
            "bogus",
        ),
        (
            "apps[0].heroku_generation".to_string(),
            "apps.0".to_string(),
            "heroku_generation",
            "mars",
        ),
        (
            "apps[0].discovery_status".to_string(),
            "apps.0".to_string(),
            "discovery_status",
            "whatever",
        ),
        (
            format!("resources[{formation}].config.dyno_type"),
            format!("resources.{formation}.config"),
            "dyno_type",
            "giga-9000",
        ),
    ];
    for (path, parent, key, value) in mutants {
        let mut bad = good.clone();
        put(&mut bad, &parent, key, Json::string(value));
        let (code, errors) =
            run_status(&tmp, "heroku-resource-inventory.json", &bad, HEROKU, QUIET);
        assert!(
            code == 1 && has(&errors, "ENUM_VIOLATION", &path),
            "{path}: {errors:?}"
        );
    }
}

#[test]
fn heroku_required_table_sections_are_enforced() {
    let tmp = TempDir::new("heroku-tables");
    let doc = "skills/heroku-to-aws/references/shared/schema-discover-heroku.md";
    let good = doc_example(doc, "Complete Example");
    for key in ["metadata", "apps", "resources"] {
        let mut bad = good.clone();
        remove(&mut bad, "", key);
        let (code, errors) =
            run_status(&tmp, "heroku-resource-inventory.json", &bad, HEROKU, QUIET);
        assert!(
            code == 1 && errors == errs(&[("MISSING_REQUIRED", key)]),
            "{key}: {errors:?}"
        );
    }
    // OPTIONAL sections stay optional.
    for key in ["billing_profile", "terraform_metadata"] {
        let mut ok = good.clone();
        remove(&mut ok, "", key);
        let (code, errors) = run_status(&tmp, "heroku-resource-inventory.json", &ok, HEROKU, QUIET);
        assert_eq!(code, 0, "{key}: {errors:?}");
    }
    let empty = json("{}");
    let errors = run_errors(
        &tmp,
        "heroku-resource-inventory.json",
        &empty,
        HEROKU,
        QUIET,
    );
    assert_eq!(
        errors,
        errs(&[
            ("MISSING_REQUIRED", "metadata"),
            ("MISSING_REQUIRED", "apps"),
            ("MISSING_REQUIRED", "resources"),
        ])
    );
}

#[test]
fn gcp_estimate_extensions_bind_to_their_object_keys() {
    let tmp = TempDir::new("gcp-extensions");
    let est = extended_gcp_estimate();
    let (code, errors) = run_status(&tmp, "estimation-infra.json", &est, GCP, SHIPPED);
    assert_eq!(code, 0, "{errors:?}");
    let mut bad = est.clone();
    put(
        &mut bad,
        "projected_costs.breakdown.observability.components",
        "bogus",
        Json::Int(1),
    );
    put(
        &mut bad,
        "projected_costs.breakdown.security_baseline",
        "invented",
        Json::Int(1),
    );
    put(
        &mut bad,
        "projected_costs.scenario_deltas",
        "weird",
        Json::Array(vec![]),
    );
    put(&mut bad, "projected_costs", "unrelated", Json::Int(1));
    let (code, errors) = run_status(&tmp, "estimation-infra.json", &bad, GCP, SHIPPED);
    assert_eq!(code, 1);
    assert_eq!(
        errors,
        errs(&[
            (
                "UNKNOWN_KEY",
                "projected_costs.breakdown.observability.components.bogus"
            ),
            (
                "UNKNOWN_KEY",
                "projected_costs.breakdown.security_baseline.invented"
            ),
            ("UNKNOWN_KEY", "projected_costs.scenario_deltas.weird"),
            ("UNKNOWN_KEY", "projected_costs.unrelated"),
        ])
    );
}

#[test]
fn azure_design_graviton_block_is_the_design_section_not_the_discovery_profile() {
    let tmp = TempDir::new("azure-graviton");
    let src = real_plugin().join("fixtures/azure-iac-terraform/after-design");
    let control = errors_of_dir(&src, AZURE, QUIET);
    let mut design = fixture("azure-iac-terraform/after-design/aws-design.json");
    put(
        &mut design,
        "services.0",
        "graviton",
        json(r#"{"compatibility": "ready", "target_architecture": "arm64", "caveats": []}"#),
    );
    let errors = run_errors(&tmp, "aws-design.json", &design, AZURE, QUIET);
    assert_eq!(minus(&errors, &control), errs(&[]), "{errors:?}");
    put(
        &mut design,
        "services.0",
        "graviton",
        json(r#"{"compatibility": "ready", "tier": "ready", "service_name": "x"}"#),
    );
    let errors = run_errors(&tmp, "aws-design.json", &design, AZURE, QUIET);
    assert_eq!(
        minus(&errors, &control),
        errs(&[
            ("UNKNOWN_KEY", "services[0].graviton.tier"),
            ("UNKNOWN_KEY", "services[0].graviton.service_name"),
        ])
    );
    // The discovery profile is still selected for the inventory.
    let mut inv = fixture("azure-iac-terraform/after-discover/azure-resource-inventory.json");
    put(
        &mut inv,
        "",
        "graviton_profile",
        json(
            r#"[{"service_name": "api", "tier": "ready", "target_architecture": "arm64",
                "signals": [], "caveats": [], "source": "app_code"}]"#,
        ),
    );
    let errors = run_errors(&tmp, "azure-resource-inventory.json", &inv, AZURE, QUIET);
    assert!(
        !errors
            .iter()
            .any(|(_, path)| path.starts_with("graviton_profile")),
        "{errors:?}"
    );
}

#[test]
fn gcp_ai_workloads_fields_are_checked() {
    let tmp = TempDir::new("gcp-ai");
    let doc = "skills/gcp-to-aws/references/shared/schema-discover-ai.md";
    let mut profile = doc_example(doc, "ai-workload-profile.json");
    let workloads = member(&doc_example(doc, "workloads[]"), "workloads");
    put(&mut profile, "", "workloads", workloads);
    let (code, errors) = run_status(&tmp, "ai-workload-profile.json", &profile, GCP, SHIPPED);
    assert_eq!(code, 0, "{errors:?}");
    let mut empty = profile.clone();
    put(&mut empty, "", "workloads", Json::Array(vec![]));
    let (code, errors) = run_status(&tmp, "ai-workload-profile.json", &empty, GCP, SHIPPED);
    assert_eq!(code, 0, "{errors:?}");

    let mut bad = profile.clone();
    let model_id = remove(&mut bad, "workloads.0", "model_id").expect("model_id");
    put(&mut bad, "workloads.0", "model_name", model_id);
    put(
        &mut bad,
        "workloads.0",
        "structured_output",
        Json::string("yes"),
    );
    put(
        &mut bad,
        "workloads.0",
        "call_sites",
        Json::string("lib/gemini.ts"),
    );
    let (code, errors) = run_status(&tmp, "ai-workload-profile.json", &bad, GCP, SHIPPED);
    assert_eq!(code, 1);
    assert_eq!(
        errors,
        errs(&[
            ("UNKNOWN_KEY", "workloads[0].model_name"),
            ("TYPE_MISMATCH", "workloads[0].structured_output"),
            ("TYPE_MISMATCH", "workloads[0].call_sites"),
        ])
    );
    let mut object = profile.clone();
    put(
        &mut object,
        "",
        "workloads",
        json(r#"{"invented": "not-an-array"}"#),
    );
    let errors = run_errors(&tmp, "ai-workload-profile.json", &object, GCP, SHIPPED);
    assert!(has(&errors, "TYPE_MISMATCH", "workloads"), "{errors:?}");
}

#[test]
fn heroku_workshop_preferences_subset_is_an_open_knob_map() {
    let tmp = TempDir::new("heroku-subset");
    let artifact = "scenarios/scenario-002.json";
    let mut sc = fixture("heroku-workshop/after-arm64-reprice/scenarios/scenario-002.json");
    for (key, value) in [
        ("operational.cost_optimization", Json::string("aggressive")),
        ("availability.multi_az", Json::Bool(true)),
        ("database.ha", Json::string("multi_az")),
        ("compute.target", Json::string("fargate")),
    ] {
        put(&mut sc, "preferences_subset", key, value);
    }
    let (code, errors) = run_status(&tmp, artifact, &sc, HEROKU, SHIPPED);
    assert_eq!(code, 0, "{errors:?}");
    // The manifest itself is still closed.
    put(&mut sc, "", "invented_root_key", Json::Int(1));
    let errors = run_errors(&tmp, artifact, &sc, HEROKU, SHIPPED);
    assert_eq!(errors, errs(&[("UNKNOWN_KEY", "invented_root_key")]));
}

#[test]
fn azure_inventory_source_accepts_the_documented_joined_set() {
    let tmp = TempDir::new("azure-source");
    let artifact = "azure-resource-inventory.json";
    let mut inv = fixture("azure-iac-terraform/after-discover/azure-resource-inventory.json");
    let bad = errs(&[("ENUM_VIOLATION", "resources[0].source")]);
    for (value, expect) in [
        ("live+terraform", errs(&[])),
        ("terraform+live", errs(&[])),
        ("terraform", errs(&[])),
        ("terraform+bogus", bad.clone()),
        ("bogus", bad.clone()),
    ] {
        put(&mut inv, "resources.0", "source", Json::string(value));
        let errors = run_errors(&tmp, artifact, &inv, AZURE, SHIPPED);
        assert_eq!(errors, expect, "{value}");
    }
    put(&mut inv, "resources.0", "source", Json::string("terraform"));
    // A genuinely closed enum stays closed.
    put(
        &mut inv,
        "resources.0",
        "azure_type_provenance",
        Json::string("terraform+live"),
    );
    let errors = run_errors(&tmp, artifact, &inv, AZURE, SHIPPED);
    assert_eq!(
        errors,
        errs(&[("ENUM_VIOLATION", "resources[0].azure_type_provenance")])
    );
}

#[test]
fn gcp_preferences_chosen_by_enum_is_enforced_including_derived() {
    let tmp = TempDir::new("gcp-chosen-by");
    let mut pref = fixture("gcp-workshop/after-graviton-reprice/preferences.json");
    let path = "design_constraints.target_region";
    for (value, expect) in [
        ("derived", errs(&[])),
        ("default", errs(&[])),
        (
            "invalid-origin",
            errs(&[(
                "ENUM_VIOLATION",
                "design_constraints.target_region.chosen_by",
            )]),
        ),
    ] {
        put(&mut pref, path, "chosen_by", Json::string(value));
        let errors = run_errors(&tmp, "preferences.json", &pref, GCP, SHIPPED);
        assert_eq!(errors, expect, "{value}");
    }
    put(&mut pref, path, "chosen_by", Json::string("user"));
    put(
        &mut pref,
        "design_constraints",
        "cpu_architecture",
        json(r#"{"value": "graviton", "chosen_by": "bogus"}"#),
    );
    let errors = run_errors(&tmp, "preferences.json", &pref, GCP, SHIPPED);
    assert_eq!(
        errors,
        errs(&[(
            "ENUM_VIOLATION",
            "design_constraints.cpu_architecture.chosen_by"
        )])
    );
}

#[test]
fn azure_design_arrays_reject_a_scalar_after_the_empty_seed() {
    let tmp = TempDir::new("azure-arrays");
    let src = real_plugin().join("fixtures/azure-iac-terraform/after-design");
    let design = fixture("azure-iac-terraform/after-design/aws-design.json");
    let control = errors_of_dir(&src, AZURE, QUIET);
    let run = |data: &Json| run_errors(&tmp, "aws-design.json", data, AZURE, QUIET);
    for key in ["services", "clusters", "deferred", "pending_rubric"] {
        let wanted = format!("{key}[0]");
        for member in [
            Json::string("not-an-object"),
            Json::Int(17),
            Json::Bool(false),
            Json::Array(vec![]),
        ] {
            let mut bad = design.clone();
            put(&mut bad, "", key, Json::Array(vec![member]));
            assert!(
                has(&minus(&run(&bad), &control), "TYPE_MISMATCH", &wanted),
                "{key}"
            );
        }
        let mut empty = design.clone();
        put(&mut empty, "", key, Json::Array(vec![]));
        assert!(!has(&run(&empty), "TYPE_MISMATCH", &wanted), "{key} empty");
        let mut blank = design.clone();
        put(&mut blank, "", key, Json::Array(vec![json("{}")]));
        assert!(!has(&run(&blank), "TYPE_MISMATCH", &wanted), "{key} object");
    }
    let mut blank = design.clone();
    put(&mut blank, "", "services", Json::Array(vec![json("{}")]));
    assert!(has(
        &run(&blank),
        "MISSING_REQUIRED",
        "services[0].routing_provenance"
    ));
    let profile = doc_example(
        "skills/azure-to-aws/references/shared/schema-discover-ai.md",
        "Shape",
    );
    let ok = run_errors(&tmp, "ai-workload-profile.json", &profile, AZURE, QUIET);
    let mut wl_bad = profile.clone();
    put(
        &mut wl_bad,
        "",
        "workloads",
        Json::Array(vec![Json::Int(42)]),
    );
    let errors = run_errors(&tmp, "ai-workload-profile.json", &wl_bad, AZURE, QUIET);
    assert!(has(&minus(&errors, &ok), "TYPE_MISMATCH", "workloads[0]"));
}

#[test]
fn gcp_cluster_edges_reject_a_scalar_member() {
    let tmp = TempDir::new("gcp-edges");
    let doc = "skills/gcp-to-aws/references/shared/schema-discover-iac.md";
    let clusters = doc_example(doc, "gcp-resource-clusters.json");
    let (code, errors) = run_status(&tmp, "gcp-resource-clusters.json", &clusters, GCP, QUIET);
    assert_eq!(code, 0, "{errors:?}");
    let mut bad = clusters.clone();
    put(
        &mut bad,
        "clusters.1",
        "edges",
        Json::Array(vec![Json::Int(42)]),
    );
    let errors = run_errors(&tmp, "gcp-resource-clusters.json", &bad, GCP, QUIET);
    assert!(
        has(&errors, "TYPE_MISMATCH", "clusters[1].edges[0]"),
        "{errors:?}"
    );
    put(&mut bad, "clusters.1", "edges", Json::Array(vec![]));
    let (code, errors) = run_status(&tmp, "gcp-resource-clusters.json", &bad, GCP, QUIET);
    assert_eq!(code, 0, "{errors:?}");
}

#[test]
fn heroku_inventory_clarifications_is_a_signal_map() {
    let tmp = TempDir::new("heroku-clarifications");
    let mut pref = fixture("heroku-workshop/seed/preferences.json");
    put(
        &mut pref,
        "metadata",
        "inventory_clarifications",
        json(
            r#"{"database_ha": "plan:premium-0", "redis_ha": "plan:mini",
                "target_region": "space:virginia"}"#,
        ),
    );
    put(&mut pref, "sources", "Q1", Json::string("extracted"));
    for artifact in [
        "preferences.json",
        "scenarios/scenario-001.preferences.json",
    ] {
        let errors = run_errors(&tmp, artifact, &pref, HEROKU, SHIPPED);
        assert_eq!(errors, errs(&[]), "{artifact}");
    }
    let mut bad = pref.clone();
    put(&mut bad, "", "invented_preference", Json::Int(1));
    let errors = run_errors(&tmp, "preferences.json", &bad, HEROKU, SHIPPED);
    assert_eq!(errors, errs(&[("UNKNOWN_KEY", "invented_preference")]));
}

#[test]
fn schema_declared_estimate_properties_are_admitted() {
    let tmp = TempDir::new("declared-properties");
    let mut est = fixture("gcp-decision-gate/after-decide-complete/estimation-infra.json");
    let run = |data: &Json| run_errors(&tmp, "estimation-infra.json", data, GCP, SHIPPED);
    let control = run(&est);
    put(
        &mut est,
        "",
        "complexity_inputs",
        json(r#"{"tier_inputs": {"requests": 1}}"#),
    );
    put(
        &mut est,
        "",
        "workshop",
        json(r#"{"scenario_id": "scenario-002", "region_note": "eu-west-1 is a what-if"}"#),
    );
    assert_eq!(minus(&run(&est), &control), errs(&[]));
    let mut unknown = est.clone();
    put(&mut unknown, "", "not_in_schema", Json::Int(1));
    put(&mut unknown, "workshop", "invented", Json::Int(1));
    put(
        &mut unknown,
        "pricing_source",
        "services_by_source",
        json(r#"{"cached": ["Fargate"], "live": ["Fargate"]}"#),
    );
    let errors = run(&unknown);
    assert!(has(&errors, "UNKNOWN_KEY", "not_in_schema"), "{errors:?}");
    assert!(
        has(&errors, "UNKNOWN_KEY", "workshop.invented"),
        "{errors:?}"
    );
    assert!(
        has(
            &errors,
            "UNKNOWN_KEY",
            "pricing_source.services_by_source.live"
        ),
        "{errors:?}"
    );
}

#[test]
fn azure_preferences_follow_the_producer_route() {
    let tmp = TempDir::new("azure-route");
    let infra = fixture("azure-iac-terraform/after-clarify/preferences.json");
    let rows = doc_example(
        "skills/azure-to-aws/references/phases/clarify/clarify-ai.md",
        "Step 3: Rows returned",
    );
    let mut mixed = infra.clone();
    update(&mut mixed, &rows);
    let run = |data: &Json| run_errors(&tmp, "preferences.json", data, AZURE, QUIET);

    assert!(!has(&run(&infra), "MISSING_REQUIRED", "clarify_status"));
    let mixed_errors = run(&mixed);
    assert_eq!(minus(&mixed_errors, &run(&infra)), errs(&[]));
    assert!(
        !mixed_errors.iter().any(|(_, path)| {
            path.starts_with("ai_")
                || path.starts_with("startup_program_status")
                || path.starts_with("ai_constraints")
        }),
        "{mixed_errors:?}"
    );
    let mut bad = mixed.clone();
    put(&mut bad, "", "not_a_preference", Json::Int(1));
    assert!(has(&run(&bad), "UNKNOWN_KEY", "not_a_preference"));
    let mut bad = mixed.clone();
    put(&mut bad, "", "workloads", Json::Array(vec![Json::Int(42)]));
    assert!(has(&run(&bad), "TYPE_MISMATCH", "workloads[0]"));

    let ai = doc_example(
        "skills/azure-to-aws/references/phases/clarify/clarify-ai-only.md",
        "preferences.json (AI-only)",
    );
    assert_eq!(run(&ai), errs(&[]));
    let mut omitted = ai.clone();
    remove(&mut omitted, "", "workloads");
    assert!(has(&run(&omitted), "MISSING_REQUIRED", "workloads"));
    let mut dropped = ai.clone();
    remove(&mut dropped, "ai_constraints", "ai_framework");
    assert!(has(
        &run(&dropped),
        "MISSING_REQUIRED",
        "ai_constraints.ai_framework"
    ));
    let mut invented = ai.clone();
    put(&mut invented, "metadata", "invented", Json::Int(1));
    assert!(has(&run(&invented), "UNKNOWN_KEY", "metadata.invented"));

    // The documented shapes of the AI-only producer contract are accepted.
    let mut multi = ai.clone();
    put(
        &mut multi,
        "ai_constraints.ai_framework",
        "value",
        strings(&["direct", "langchain"]),
    );
    assert_eq!(run(&multi), errs(&[]));
    let mut caveat = ai.clone();
    put(
        &mut caveat,
        "metadata",
        "report_caveats",
        strings(&["Compliance requirements were not confirmed by the user"]),
    );
    assert_eq!(run(&caveat), errs(&[]));
    let mut sourced = ai.clone();
    put(
        &mut sourced,
        "design_constraints.target_region",
        "source",
        Json::string("discovery.ai-workload-profile.json"),
    );
    assert_eq!(run(&sourced), errs(&[]));

    // The mixed example carries the ai_latency row.
    assert_eq!(
        mixed
            .get("ai_latency")
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str),
        Some("important")
    );
    // The infra verdict stays required on the infra route.
    let mut no_status = infra.clone();
    remove(&mut no_status, "", "clarify_status");
    assert!(has(&run(&no_status), "MISSING_REQUIRED", "clarify_status"));
}

#[test]
fn ai_workload_capability_uses_the_discovery_design_vocabulary() {
    let tmp = TempDir::new("capability");
    let vocabulary = read("skills/azure-to-aws/references/shared/schema-discover-ai.md");
    assert!(vocabulary.contains("`capability` enum: `text_generation`"));
    let ai = doc_example(
        "skills/azure-to-aws/references/phases/clarify/clarify-ai-only.md",
        "preferences.json (AI-only)",
    );
    let capability = ai
        .get("workloads")
        .and_then(|w| match w {
            Json::Array(items) => items.first(),
            _ => None,
        })
        .and_then(|w| w.get("capability"))
        .and_then(Json::as_str);
    assert_eq!(capability, Some("text_generation"));
    let errors = run_errors(&tmp, "preferences.json", &ai, AZURE, QUIET);
    assert_eq!(errors, errs(&[]));
    let mixed = doc_example(
        "skills/azure-to-aws/references/phases/clarify/clarify-ai.md",
        "workloads[]",
    );
    let capability = match &mixed {
        Json::Array(items) => items.first(),
        _ => None,
    }
    .and_then(|w| w.get("capability"))
    .and_then(Json::as_str);
    assert_eq!(capability, Some("text_generation"));
}

#[test]
fn gcp_drift_values_keep_their_json_types() {
    let tmp = TempDir::new("gcp-drift");
    let mut inv = fixture("gcp-workshop/seed/gcp-resource-inventory.json");
    put(
        &mut inv,
        "",
        "live_metadata",
        json(
            r#"{"found": true, "drift": {"config_conflicts": [{"address": "google_compute_disk.d",
                "field": "size_gb", "terraform_value": 10, "live_value": 20}]}}"#,
        ),
    );
    let run = |data: &Json| run_errors(&tmp, "gcp-resource-inventory.json", data, GCP, SHIPPED);
    let base = "live_metadata.drift.config_conflicts[0]";
    let (terraform, live, address) = (
        format!("{base}.terraform_value"),
        format!("{base}.live_value"),
        format!("{base}.address"),
    );
    let numeric = run(&inv);
    assert!(!has(&numeric, "TYPE_MISMATCH", &terraform));
    assert!(!has(&numeric, "TYPE_MISMATCH", &live));
    let conflict = "live_metadata.drift.config_conflicts.0";
    put(&mut inv, conflict, "terraform_value", Json::Bool(false));
    put(&mut inv, conflict, "live_value", Json::Bool(true));
    assert!(!has(&run(&inv), "TYPE_MISMATCH", &live));
    put(
        &mut inv,
        conflict,
        "terraform_value",
        Json::string("db-f1-micro"),
    );
    put(
        &mut inv,
        conflict,
        "live_value",
        json(r#"{"tier": "db-custom-2-8192"}"#),
    );
    let structured = run(&inv);
    assert!(!has(&structured, "TYPE_MISMATCH", &terraform));
    assert!(!has(&structured, "TYPE_MISMATCH", &live));
    put(&mut inv, conflict, "address", Json::Int(10));
    assert!(has(&run(&inv), "TYPE_MISMATCH", &address));
}

#[test]
fn quoted_azure_enums_reject_invented_members() {
    let tmp = TempDir::new("azure-quoted");
    let inv = fixture("azure-iac-terraform/after-discover/azure-resource-inventory.json");
    let run_inv =
        |data: &Json| run_errors(&tmp, "azure-resource-inventory.json", data, AZURE, SHIPPED);
    let control = run_inv(&inv);
    assert!(!has(&control, "ENUM_VIOLATION", "metadata.clustering_mode"));
    for mode in ["full", "simplified_live"] {
        let mut data = inv.clone();
        put(&mut data, "metadata", "clustering_mode", Json::string(mode));
        assert!(!has(
            &minus(&run_inv(&data), &control),
            "ENUM_VIOLATION",
            "metadata.clustering_mode"
        ));
    }
    let mut bad = inv.clone();
    put(
        &mut bad,
        "metadata",
        "clustering_mode",
        Json::string("invented-mode"),
    );
    assert!(has(
        &run_inv(&bad),
        "ENUM_VIOLATION",
        "metadata.clustering_mode"
    ));

    let pref = fixture("azure-iac-terraform/after-clarify-fast-path/preferences.json");
    let run_pref = |data: &Json| run_errors(&tmp, "preferences.json", data, AZURE, SHIPPED);
    for mode in ["fast_path", "wizard"] {
        let mut ok = pref.clone();
        put(&mut ok, "metadata", "clarify_mode", Json::string(mode));
        assert!(!has(
            &run_pref(&ok),
            "ENUM_VIOLATION",
            "metadata.clarify_mode"
        ));
    }
    let mut bad = pref.clone();
    put(
        &mut bad,
        "metadata",
        "clarify_mode",
        Json::string("invented-mode"),
    );
    assert!(has(
        &run_pref(&bad),
        "ENUM_VIOLATION",
        "metadata.clarify_mode"
    ));
}

#[test]
fn azure_user_stated_db_size_is_optional() {
    let tmp = TempDir::new("azure-db-size");
    let pref = fixture("azure-iac-terraform/after-clarify-fast-path/preferences.json");
    let run = |data: &Json| run_errors(&tmp, "preferences.json", data, AZURE, SHIPPED);
    assert!(!has(
        &run(&pref),
        "UNKNOWN_KEY",
        "data.db_cutover.user_stated_size_gib"
    ));
    let mut confirmed = pref.clone();
    let row = "data.db_cutover";
    put(&mut confirmed, row, "user_stated_size_gib", Json::Int(120));
    put(&mut confirmed, row, "value", Json::string("dump_restore"));
    put(
        &mut confirmed,
        row,
        "source",
        Json::string("user_confirmed_at_generate"),
    );
    put(
        &mut confirmed,
        row,
        "deferred_to_generate",
        Json::Bool(false),
    );
    for key in ["deferred_to_generate", "questions_defaulted"] {
        let kept: Vec<Json> = match confirmed.get("metadata").and_then(|m| m.get(key)) {
            Some(Json::Array(items)) => items
                .iter()
                .filter(|item| item.as_str() != Some("data.db_cutover"))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        put(&mut confirmed, "metadata", key, Json::Array(kept));
    }
    assert_eq!(run(&confirmed), errs(&[]));
    let mut extra = confirmed.clone();
    put(&mut extra, row, "invented", Json::Int(1));
    assert!(has(&run(&extra), "UNKNOWN_KEY", "data.db_cutover.invented"));
}

#[test]
fn azure_workshop_index_records_capture_state() {
    let tmp = TempDir::new("azure-index");
    let doc = "skills/azure-to-aws/references/shared/schema-workshop-scenarios.md";
    let index = doc_example(doc, "scenarios/index.json");
    let run = |data: &Json| run_errors(&tmp, "scenarios/index.json", data, AZURE, QUIET);
    assert_eq!(run(&index), errs(&[]), "{index:?}");
    let mut active = index.clone();
    put(
        &mut active,
        "",
        "active_scenario_id",
        Json::string("scenario-002"),
    );
    assert_eq!(run(&active), errs(&[]));
    let mut resumed = active.clone();
    let Json::Array(scenarios) = node(&mut resumed, "scenarios") else {
        panic!("scenarios");
    };
    scenarios.push(json(
        r#"{"scenario_id": "scenario-002", "label": "Graviton",
            "created_at": "2026-10-02T00:00:00Z", "preference_patch": {},
            "totals": {"non_optimized_monthly": 1, "right_sized_monthly": 1},
            "pricing_source": "cached",
            "design_snapshot": "scenarios/scenario-002/aws-design.json",
            "estimate_snapshot": "scenarios/scenario-002/estimation-infra.json"}"#,
    ));
    assert_eq!(run(&resumed), errs(&[]));
    let mut omitted = index.clone();
    for key in [
        "inventory_fingerprint",
        "active_scenario_id",
        "max_scenarios",
    ] {
        remove(&mut omitted, "", key);
    }
    assert_eq!(run(&omitted), errs(&[]));
    let mut bad = index.clone();
    put(&mut bad, "", "invented", Json::Int(1));
    assert!(has(&run(&bad), "UNKNOWN_KEY", "invented"));
}

#[test]
fn gcp_inventory_estimate_accepts_the_cache_category() {
    let tmp = TempDir::new("gcp-cache");
    let mut est = fixture("gcp-decision-gate/after-decide-complete/estimation-infra.json");
    let run = |data: &Json| run_errors(&tmp, "estimation-infra.json", data, GCP, SHIPPED);
    let control = run(&est);
    if est.get("current_costs").is_none() {
        put(&mut est, "", "current_costs", json("{}"));
    }
    put(
        &mut est,
        "current_costs",
        "source",
        Json::string("inventory_estimate"),
    );
    if est
        .get("current_costs")
        .and_then(|c| c.get("breakdown"))
        .is_none()
    {
        put(&mut est, "current_costs", "breakdown", json("{}"));
    }
    put(&mut est, "current_costs.breakdown", "cache", Json::Int(10));
    assert_eq!(minus(&run(&est), &control), errs(&[]));
    let mut bad = est.clone();
    put(
        &mut bad,
        "current_costs.breakdown",
        "invented",
        Json::Int(1),
    );
    assert!(has(
        &run(&bad),
        "UNKNOWN_KEY",
        "current_costs.breakdown.invented"
    ));
}

#[test]
fn workshop_snapshots_reuse_the_active_contracts() {
    let tmp = TempDir::new("snapshots");
    let mut est = fixture("gcp-decision-gate/after-decide-complete/estimation-infra.json");
    let deltas = member(&doc_example(ESTIMATE_DOC, "Cost tiers"), "scenario_deltas");
    put(&mut est, "projected_costs", "scenario_deltas", deltas);
    for artifact in [
        "estimation-infra.json",
        "scenarios/scenario-017.estimation-infra.json",
    ] {
        let mut bad = est.clone();
        put(
            &mut bad,
            "projected_costs.scenario_deltas",
            "bogus",
            Json::Array(vec![]),
        );
        let errors = run_errors(&tmp, artifact, &bad, GCP, QUIET);
        assert!(
            has(
                &errors,
                "UNKNOWN_KEY",
                "projected_costs.scenario_deltas.bogus"
            ),
            "{artifact}: {errors:?}"
        );
    }

    let mut pref = fixture("azure-iac-terraform/after-clarify/preferences.json");
    put(
        &mut pref,
        "",
        "clarify_status",
        Json::string("invented-status"),
    );
    let mut design = fixture("azure-iac-terraform/after-design/aws-design.json");
    remove(&mut design, "services.0", "routing_provenance");
    let mut azure_est = fixture("azure-iac-terraform/after-estimate/estimation-infra.json");
    put(
        &mut azure_est,
        "",
        "complexity_tier",
        Json::string("enormous"),
    );
    let cases = [
        ("preferences.json", pref, "ENUM_VIOLATION", "clarify_status"),
        (
            "aws-design.json",
            design,
            "MISSING_REQUIRED",
            "services[0].routing_provenance",
        ),
        (
            "estimation-infra.json",
            azure_est,
            "SCHEMA_VIOLATION",
            "complexity_tier",
        ),
    ];
    for (active, data, code, path) in cases {
        let snapshot = format!("scenarios/scenario-001.{active}");
        let errors = run_errors(&tmp, active, &data, AZURE, QUIET);
        assert!(has(&errors, code, path), "{active}: {errors:?}");
        let errors = run_errors(&tmp, &snapshot, &data, AZURE, QUIET);
        assert!(has(&errors, code, path), "{snapshot}: {errors:?}");
        assert!(
            !errors.iter().any(|(c, _)| c == "NO_CONTRACT"),
            "{errors:?}"
        );
    }
}

#[test]
fn heroku_scenario_accepts_recommendation_outcome() {
    let tmp = TempDir::new("heroku-outcome");
    let sc = fixture("heroku-workshop/after-arm64-reprice/scenarios/scenario-002.json");
    let run = |data: &Json| run_errors(&tmp, "scenarios/scenario-002.json", data, HEROKU, SHIPPED);
    let control = run(&sc);
    for value in [Json::string("conditional_go"), Json::Null] {
        let mut ok = sc.clone();
        put(
            &mut ok,
            "estimation_summary",
            "recommendation_outcome",
            value,
        );
        let errors = run(&ok);
        assert!(!has(
            &errors,
            "UNKNOWN_KEY",
            "estimation_summary.recommendation_outcome"
        ));
        assert_eq!(minus(&errors, &control), errs(&[]));
    }
    let mut bad = sc.clone();
    put(&mut bad, "estimation_summary", "invented", Json::Int(1));
    assert!(has(
        &run(&bad),
        "UNKNOWN_KEY",
        "estimation_summary.invented"
    ));
}

#[test]
fn azure_quoted_preference_enums_reject_unknown_values() {
    let tmp = TempDir::new("azure-pref-enums");
    let src = real_plugin().join("fixtures/azure-iac-terraform/after-clarify-fast-path");
    let pref = fixture("azure-iac-terraform/after-clarify-fast-path/preferences.json");
    assert_eq!(errors_of_dir(&src, AZURE, QUIET), errs(&[]));
    let run = |data: &Json| run_errors(&tmp, "preferences.json", data, AZURE, QUIET);
    for mode in ["fast_path", "wizard"] {
        let mut ok = pref.clone();
        put(&mut ok, "metadata", "clarify_mode", Json::string(mode));
        assert_eq!(run(&ok), errs(&[]), "{mode}");
    }
    let mut bad = pref.clone();
    put(
        &mut bad,
        "metadata",
        "clarify_mode",
        Json::string("invented-mode"),
    );
    assert_eq!(
        run(&bad),
        errs(&[("ENUM_VIOLATION", "metadata.clarify_mode")])
    );
    for coverage in ["complete", "partial", "unknown"] {
        let mut ok = pref.clone();
        put(
            &mut ok,
            "data.db_cutover",
            "size_coverage",
            Json::string(coverage),
        );
        assert_eq!(run(&ok), errs(&[]), "{coverage}");
    }
    let mut bad = pref.clone();
    put(
        &mut bad,
        "data.db_cutover",
        "size_coverage",
        Json::string("invented-coverage"),
    );
    assert_eq!(
        run(&bad),
        errs(&[("ENUM_VIOLATION", "data.db_cutover.size_coverage")])
    );
    // The unquoted clarify_status control still rejects an unknown value.
    let mut bad = pref.clone();
    put(
        &mut bad,
        "",
        "clarify_status",
        Json::string("invented-status"),
    );
    assert!(has(&run(&bad), "ENUM_VIOLATION", "clarify_status"));
}

#[test]
fn azure_discovery_verdict_is_required() {
    let tmp = TempDir::new("azure-verdict");
    let inv = fixture("azure-iac-terraform/after-discover/azure-resource-inventory.json");
    let run = |data: &Json| run_errors(&tmp, "azure-resource-inventory.json", data, AZURE, QUIET);
    assert!(!has(
        &run(&inv),
        "MISSING_REQUIRED",
        "metadata.clarify_fast_path"
    ));
    let mut fresh = inv.clone();
    remove(&mut fresh, "metadata", "clarify_fast_path");
    assert!(has(
        &run(&fresh),
        "MISSING_REQUIRED",
        "metadata.clarify_fast_path"
    ));
}

#[test]
fn gcp_estimate_snapshots_use_the_active_estimate_shape() {
    let tmp = TempDir::new("gcp-snapshots");
    let mut est = extended_gcp_estimate();
    put(
        &mut est,
        "pricing_source",
        "services_by_source",
        json(r#"{"cached": ["Fargate"], "fallback": ["NAT Gateway"], "estimated": []}"#),
    );
    for artifact in [
        "estimation-infra.json",
        "scenarios/scenario-017.estimation-infra.json",
    ] {
        assert_eq!(
            run_errors(&tmp, artifact, &est, GCP, QUIET),
            errs(&[]),
            "{artifact}"
        );
        let mut bad = est.clone();
        put(
            &mut bad,
            "pricing_source.services_by_source",
            "live",
            strings(&["Fargate"]),
        );
        let errors = run_errors(&tmp, artifact, &bad, GCP, QUIET);
        assert!(
            has(
                &errors,
                "UNKNOWN_KEY",
                "pricing_source.services_by_source.live"
            ),
            "{artifact}: {errors:?}"
        );
    }
}
