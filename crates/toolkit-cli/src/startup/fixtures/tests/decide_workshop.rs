//! The decision-gate, workshop and non-web scaling programs against their
//! committed golden trees, and against deliberately invalid copies of them.

use std::fs;

use super::{
    assert_fails, assert_fails_with, assert_passes, check, golden, remove, set, text, Copy,
};
use crate::startup::scoring::fixtures::path_text;
use toolkit_core::Json;

// ------------------------------------------------------------- decision gate

#[test]
fn the_shipped_decide_complete_runs_pass() {
    assert_passes(
        "heroku-decide",
        &golden("heroku-decision-gate", "after-decide-complete"),
    );
    assert_passes(
        "gcp-decide",
        &golden("gcp-decision-gate", "after-decide-complete"),
    );
}

/// The retained-pack sibling runs the same assertions: the pre-execution
/// check is `phases.generate`-based, so a retained execution pack must NOT
/// fail the Heroku decision gate.
#[test]
fn a_retained_execution_pack_does_not_fail_the_heroku_decide_gate() {
    let retained = golden("heroku-decision-gate", "retained-execution-pack");
    assert!(
        retained.join("terraform").is_dir(),
        "the fixture retains terraform/"
    );
    assert_passes("heroku-decide-retained", &retained);
    assert_passes("heroku-decide", &retained);
}

#[test]
fn heroku_decide_fails_on_a_wrong_terminal_state() {
    let run = Copy::of("heroku-decision-gate", "after-decide-complete");
    run.edit(".phase-status.json", |doc| {
        set(doc, &["run_mode"], text("execute"));
        set(doc, &["phases", "generate"], text("completed"));
        set(doc, &["current_phase"], text("generate"));
    });
    let report = assert_fails("heroku-decide", run.path());
    assert!(report.contains("current_phase=generate"), "{report}");
    assert!(report.contains("run_mode=execute"), "{report}");
    assert!(report.contains("generate=completed"), "{report}");
}

#[test]
fn heroku_decide_fails_without_the_decision_pack() {
    let run = Copy::of("heroku-decision-gate", "after-decide-complete");
    run.remove("DECISION.md");
    assert_fails_with("heroku-decide", run.path(), "missing DECISION.md");
    run.remove("decision-report.html");
    assert_fails_with("heroku-decide", run.path(), "missing decision-report.html");
    run.remove(".phase-status.json");
    assert_fails_with("heroku-decide", run.path(), "missing .phase-status.json");
}

#[test]
fn heroku_decide_fails_when_the_report_loses_its_locks() {
    let run = Copy::of("heroku-decision-gate", "after-decide-complete");
    let html = run.read("decision-report.html");
    assert!(
        html.contains("if you execute"),
        "the golden carries the band label"
    );
    run.write(
        "decision-report.html",
        &html
            .replace("if you execute", "if you proceed")
            .replace("class=\"verdict-headline\"", "class=\"headline\""),
    );
    let report = assert_fails("heroku-decide", run.path());
    assert!(
        report.contains("missing verdict-headline element"),
        "{report}"
    );
    assert!(report.contains("if you execute"), "{report}");
}

#[test]
fn heroku_decide_fails_when_the_estimate_cannot_become_a_plan() {
    let run = Copy::of("heroku-decision-gate", "after-decide-complete");
    run.edit("estimation-infra.json", |doc| {
        remove(doc, &["projected_costs"]);
    });
    let report = assert_fails("heroku-decide", run.path());
    assert!(report.contains("plan.json"), "{report}");
}

#[test]
fn gcp_decide_fails_on_execution_artifacts_and_state() {
    let run = Copy::of("gcp-decision-gate", "after-decide-complete");
    fs::create_dir_all(run.path().join("terraform")).expect("terraform dir");
    run.write("generation-warnings.json", "{}");
    run.edit(".phase-status.json", |doc| {
        set(doc, &["phases", "estimate"], text("pending"))
    });
    let report = assert_fails("gcp-decide", run.path());
    assert!(
        report.contains("terraform/ must not exist on a decide run"),
        "{report}"
    );
    assert!(
        report.contains("generation-*.json must not exist on a decide run"),
        "{report}"
    );
    assert!(report.contains("estimate=pending"), "{report}");
}

#[test]
fn gcp_decide_fails_when_the_report_loses_its_sentences() {
    let run = Copy::of("gcp-decision-gate", "after-decide-complete");
    let html = run.read("decision-report.html");
    assert!(
        html.contains("not directly comparable"),
        "the golden carries the baseline sentence"
    );
    run.write(
        "decision-report.html",
        &html.replace("not directly comparable", "not the same thing"),
    );
    assert_fails_with(
        "gcp-decide",
        run.path(),
        "missing not-comparable baseline sentence",
    );
}

#[test]
fn decide_checks_the_emitted_plan_platform() {
    // A GCP run checked as a Heroku run: the emitter writes a GCP plan, the
    // asserter wants HEROKU.
    let run = Copy::of("gcp-decision-gate", "after-decide-complete");
    let report = assert_fails("heroku-decide", run.path());
    assert!(report.contains("sourcePlatform=GCP"), "{report}");
}

// ------------------------------------------------------------------ workshop

#[test]
fn the_shipped_workshop_runs_pass() {
    assert_passes(
        "heroku-workshop",
        &golden("heroku-workshop", "after-arm64-reprice"),
    );
    assert_passes(
        "gcp-workshop",
        &golden("gcp-workshop", "after-graviton-reprice"),
    );
}

#[test]
fn a_workshop_accepts_an_explicit_seed_directory() {
    let run = golden("heroku-workshop", "after-arm64-reprice");
    let seed = golden("heroku-workshop", "seed");
    let (status, stdout, stderr) = check("heroku-workshop", &run, &[path_text(&seed)]);
    assert_eq!(status, 0, "{stdout}{stderr}");
    // The other set's seed has no matching inventory bytes.
    let other = golden("gcp-workshop", "seed");
    let (status, stdout, _) = check("heroku-workshop", &run, &[path_text(&other)]);
    assert_eq!(status, 1);
    assert!(stdout.contains("missing seed inventory"), "{stdout}");
}

#[test]
fn heroku_workshop_fails_when_discovery_is_not_frozen() {
    let run = Copy::of("heroku-workshop", "after-arm64-reprice");
    let inventory = run.read("heroku-resource-inventory.json");
    run.write("heroku-resource-inventory.json", &format!("{inventory} "));
    assert_fails_with(
        "heroku-workshop",
        run.path(),
        "inventory bytes changed — workshop must freeze discovery",
    );
}

#[test]
fn heroku_workshop_fails_on_design_and_scenario_drift() {
    let run = Copy::of("heroku-workshop", "after-arm64-reprice");
    run.edit("aws-design.json", |doc| {
        let Json::Array(services) = super::super::ledger::get(doc, "services").clone() else {
            panic!("services");
        };
        let index = services
            .iter()
            .position(|s| {
                super::super::ledger::eq_str(
                    super::super::ledger::get(s, "aws_service"),
                    "Elastic Beanstalk",
                )
            })
            .expect("an Elastic Beanstalk service");
        let at = index.to_string();
        set(
            doc,
            &["services", &at, "aws_config", "instance_type"],
            text("t3.small"),
        );
        set(
            doc,
            &["services", &at, "aws_config", "cpu_architecture"],
            text("x86_64"),
        );
    });
    run.edit("scenarios/index.json", |doc| {
        set(doc, &["active_scenario_id"], text("scenario-009"));
        set(doc, &["scenarios"], Json::Array(vec![text("only-one")]));
    });
    run.edit(".phase-status.json", |doc| {
        set(doc, &["phases", "generate"], text("completed"));
        set(doc, &["current_phase"], text("generate"));
        set(doc, &["phases", "workshop"], text("completed"));
    });
    let report = assert_fails("heroku-workshop", run.path());
    for needle in [
        "EB instance_type=t3.small want t4g.small",
        "EB cpu_architecture mismatch",
        "design instance_type unchanged (t3.small) vs baseline (t3.small)",
        "scenario count 1 < 2",
        "active_scenario_id mismatch",
        "generate must not auto-complete during workshop",
        "current_phase=generate want estimate",
        "phases.workshop=completed want in_progress",
    ] {
        assert!(report.contains(needle), "{needle}\n{report}");
    }
}

#[test]
fn heroku_workshop_fails_on_secret_material_and_a_flat_total() {
    let run = Copy::of("heroku-workshop", "after-arm64-reprice");
    let prefs = run.read("preferences.json");
    run.write(
        "preferences.json",
        &prefs.replacen('{', "{\"leak\": \"HEROKU_API_KEY Bearer \", ", 1),
    );
    run.edit("estimation-infra.json", |doc| {
        let baseline = super::super::ledger::path(
            &crate::startup::scoring::fixtures::j(&run_read(
                &golden("heroku-workshop", "after-arm64-reprice"),
                "scenarios/scenario-001.json",
            )),
            &["estimation_summary", "aws_monthly_balanced"],
        )
        .clone();
        set(doc, &["projected_costs", "aws_monthly_balanced"], baseline);
    });
    let report = assert_fails("heroku-workshop", run.path());
    assert!(
        report.contains("possible secret material: HEROKU_API_KEY"),
        "{report}"
    );
    assert!(
        report.contains("possible secret material: Bearer "),
        "{report}"
    );
    assert!(report.contains("balanced total unchanged"), "{report}");
}

fn run_read(base: &std::path::Path, relative: &str) -> String {
    fs::read_to_string(base.join(relative)).expect("read golden")
}

#[test]
fn heroku_workshop_fails_without_its_scenario_index() {
    let run = Copy::of("heroku-workshop", "after-arm64-reprice");
    run.remove("scenarios/index.json");
    assert_fails_with(
        "heroku-workshop",
        run.path(),
        "missing scenarios/index.json",
    );
}

#[test]
fn gcp_workshop_fails_on_graviton_and_architecture_drift() {
    let run = Copy::of("gcp-workshop", "after-graviton-reprice");
    run.edit("preferences.json", |doc| {
        set(
            doc,
            &["design_constraints", "cpu_architecture", "value"],
            text("x86_64"),
        );
        set(
            doc,
            &["design_constraints", "cpu_architecture", "prompt"],
            text("Pick one"),
        );
        set(doc, &["workshop", "graviton_note"], text(""));
    });
    run.edit("aws-design.json", |doc| {
        let Json::Array(services) = super::super::ledger::get(doc, "services").clone() else {
            panic!("services");
        };
        let at = services
            .iter()
            .position(|s| {
                super::super::ledger::eq_str(
                    super::super::ledger::get(s, "source_resource_id"),
                    "run:worker",
                )
            })
            .expect("the worker service")
            .to_string();
        set(
            doc,
            &["services", &at, "aws_config", "cpu_architecture"],
            text("ARM64"),
        );
    });
    let report = assert_fails("gcp-workshop", run.path());
    for needle in [
        "arch=x86_64",
        "Q11b prompt missing real clarify text",
        "missing workshop.graviton_note",
        "incompatible worker must stay X86_64",
    ] {
        assert!(report.contains(needle), "{needle}\n{report}");
    }
}

#[test]
fn gcp_workshop_fails_without_inventory_or_index() {
    let run = Copy::of("gcp-workshop", "after-graviton-reprice");
    run.remove("gcp-resource-inventory.json");
    run.remove("scenarios/index.json");
    let report = assert_fails("gcp-workshop", run.path());
    assert!(report.contains("missing index"), "{report}");
    let run = Copy::of("gcp-workshop", "after-graviton-reprice");
    run.write("gcp-resource-inventory.json", "{}");
    assert_fails_with("gcp-workshop", run.path(), "inventory bytes changed");
}

// ---------------------------------------------------------- non-web scaling

#[test]
fn the_shipped_non_web_scaling_run_passes() {
    assert_passes(
        "heroku-nonweb-scaling",
        &golden("heroku-nonweb-scaling", "after-design"),
    );
}

#[test]
fn nonweb_scaling_fails_when_a_worker_is_not_routed_to_fargate() {
    let run = Copy::of("heroku-nonweb-scaling", "after-design");
    let design = run.read("aws-design.json");
    assert!(
        design.contains("Fargate"),
        "the golden routes workers to Fargate"
    );
    run.write(
        "aws-design.json",
        &design.replace("\"Fargate\"", "\"Elastic Beanstalk\""),
    );
    let report = assert_fails("heroku-nonweb-scaling", run.path());
    assert!(
        report.contains("is non-web with quantity=2 but was not routed to Fargate"),
        "{report}"
    );
    assert!(
        report.contains("no Fargate service for formation:acme-jobs:worker"),
        "{report}"
    );
}

#[test]
fn nonweb_scaling_fails_on_a_dropped_formation_and_warning_drift() {
    let run = Copy::of("heroku-nonweb-scaling", "after-design");
    run.edit("aws-design.json", |doc| {
        set(doc, &["services"], Json::Array(Vec::new()));
        set(
            doc,
            &["warnings"],
            Json::Array(vec![text("Unsupported dyno type `weird`")]),
        );
    });
    let report = assert_fails("heroku-nonweb-scaling", run.path());
    assert!(
        report.contains("has no service in the design — silently dropped"),
        "{report}"
    );
    assert!(
        report.contains("missing expected warning substring"),
        "{report}"
    );
    assert!(
        report.contains("design carries a forbidden warning substring: 'Unsupported dyno type'"),
        "{report}"
    );
}

#[test]
fn nonweb_scaling_fails_without_the_inventory_or_formations() {
    let run = Copy::of("heroku-nonweb-scaling", "after-design");
    run.edit("heroku-resource-inventory.json", |doc| {
        set(doc, &["resources"], Json::Array(Vec::new()))
    });
    assert_fails_with(
        "heroku-nonweb-scaling",
        run.path(),
        "inventory has no non-release formation resources to check",
    );
    run.remove("heroku-resource-inventory.json");
    assert_fails_with(
        "heroku-nonweb-scaling",
        run.path(),
        "missing heroku-resource-inventory.json",
    );
}

/// The sizing-table cross-checks read shipped tables, not the run: a broken
/// table is caught even when the run is fine.
#[test]
fn nonweb_scaling_checks_the_sizing_tables_through_the_plugin_root() {
    use crate::startup::scoring::fixtures::{write, TempDir};
    let scratch = TempDir::new("check-expected-tables");
    let root = scratch.dir("plugin");
    let plugin = super::plugin_root();
    // The real expectation document, with sizing tables that disagree.
    let set_dir = "fixtures/heroku-nonweb-scaling";
    write(
        &root,
        &format!("{set_dir}/expected-nonweb-scaling.json"),
        &fs::read_to_string(plugin.join(set_dir).join("expected-nonweb-scaling.json"))
            .expect("spec"),
    );
    let tables = "skills/heroku-to-aws/knowledge/design";
    write(
        &root,
        &format!("{tables}/dyno-eb-sizing.json"),
        r#"{"rows": {"eco": {}}}"#,
    );
    write(
        &root,
        &format!("{tables}/dyno-fargate-sizing.json"),
        r#"{"rows": {"eco": {"fargate_cpu": 256, "fargate_memory": 512}, "basic": {"fargate_cpu": 256, "fargate_memory": 777}, "extra": {"fargate_cpu": 333, "fargate_memory": 512}, "performance-xl": {"fargate_cpu": 8192, "fargate_memory": 65536}}}"#,
    );
    write(
        &root,
        &format!("{tables}/eks-pod-sizing.json"),
        r#"{"rows": {"eco": {}, "basic": {}}}"#,
    );
    let run = golden("heroku-nonweb-scaling", "after-design");
    let (status, stdout, _) = crate::startup::scoring::fixtures::exec(&[
        "startup",
        "check-expected",
        "heroku-nonweb-scaling",
        path_text(&run),
        "--plugin-root",
        path_text(&root),
    ]);
    assert_eq!(status, 1, "{stdout}");
    for needle in [
        "eb sizing table has no 'basic' row",
        "tiers in the Fargate table but not EKS: ['extra', 'performance-xl']",
        "tiers in the Fargate table but not EB: ['basic', 'extra', 'performance-xl']",
        "fargate row 'basic': cpu=256 with memory=777 MiB is not a valid Fargate pairing",
        "fargate row 'extra': fargate_cpu=333 is not a valid Fargate task CPU value",
    ] {
        assert!(stdout.contains(needle), "{needle}\n{stdout}");
    }
    // A baselined row is skipped with a note, not failed.
    assert!(
        stdout.contains("note: fargate matrix check skipped for baselined row 'performance-xl'"),
        "{stdout}"
    );
}
