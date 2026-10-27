//! Regressions of the review of 3bf3a8db (#4744, findings #4746 #4747 #4748
//! #4753 #4754 #4759). Each case fails on that commit (the check passed, or
//! rejected a valid run) and holds after the repair.

use std::fs;
use std::path::Path;

use toolkit_core::Json;

use super::capture::{gcp_baseline_run, heroku_estimate_run, GCP_ESTIMATE};
use super::{
    assert_fails, assert_fails_with, assert_passes, copy_dir, golden, plugin_root, set, text, Copy,
};
use crate::startup::scoring::fixtures::{exec, j, path_text, TempDir};

const AZURE: &str = "azure-iac-terraform";

fn text_of(doc: &Json, path: &[&str]) -> String {
    let mut node = doc;
    for key in path {
        node = super::super::ledger::get(node, key);
    }
    node.as_str().expect("a string").to_string()
}

// ------------------------------------------------- #4746: full values are read

#[test]
fn a_forbidden_dollar_after_the_displayed_prefix_is_still_a_blocker_cost() {
    let run = Copy::of(AZURE, "after-estimate");
    let pad = " ".repeat(180);
    run.edit("estimation-infra.json", |doc| {
        let Json::Array(items) =
            super::step_mut(doc, &["licensing_delta", "blockers_are_not_costs"])
        else {
            panic!("blockers array");
        };
        let first = items[0].as_str().expect("blocker text").to_string();
        items[0] = text(&format!("{first}{pad}$100"));
    });
    let report = assert_fails("azure-estimate", run.path());
    assert!(
        report.contains("a blocker entry carries a dollar figure"),
        "{report}"
    );
}

#[test]
fn a_valid_caveat_after_a_long_prefix_is_still_found() {
    let run = gcp_baseline_run();
    run.edit("estimation-infra.json", |doc| {
        let note = text_of(doc, &["current_costs", "baseline_note"]);
        set(
            doc,
            &["current_costs", "baseline_note"],
            text(&format!("{}{note}", "x".repeat(400))),
        );
    });
    assert_passes("gcp-baseline", run.path());

    // The Heroku caveat is read in full as well.
    let run = heroku_estimate_run();
    run.edit("estimation-infra.json", |doc| {
        let note = text_of(doc, &["current_costs", "baseline_note"]);
        set(
            doc,
            &["current_costs", "baseline_note"],
            text(&format!("{}{note}", "y".repeat(400))),
        );
    });
    assert_passes("heroku-estimate", run.path());
}

#[test]
fn a_report_line_stays_bounded_whatever_the_run_value() {
    let run = gcp_baseline_run();
    run.edit("estimation-infra.json", |doc| {
        set(
            doc,
            &["current_costs", "accuracy"],
            text(&"z".repeat(200_000)),
        );
    });
    let report = assert_fails("gcp-baseline", run.path());
    assert!(report.contains("more chars]"), "{}", report.len());
    assert!(report.len() < 8_000, "{}", report.len());
}

// ---------------------------------------------- #4747: an abort is never a PASS

fn plugin_with_expectation(content: &str) -> (TempDir, String) {
    let scratch = TempDir::new("check-expected-empty-spec");
    let root = scratch.dir("plugin");
    let set_dir = root.join("fixtures").join(AZURE);
    fs::create_dir_all(&set_dir).expect("set dir");
    fs::write(set_dir.join("expected-estimate.json"), content).expect("spec");
    let root_text = path_text(&root).to_string();
    (scratch, root_text)
}

#[test]
fn an_empty_estimate_document_fails_for_every_falsey_root() {
    for empty in ["{}", "null", "[]", "false", "0", "\"\"", "", "  \n"] {
        let run = Copy::of(AZURE, "after-estimate");
        run.write("estimation-infra.json", empty);
        let report = assert_fails("azure-estimate", run.path());
        assert!(
            report.contains("estimation-infra.json"),
            "{empty:?}: {report}"
        );
    }
}

#[test]
fn an_empty_expectation_document_fails_even_with_an_absent_run_directory() {
    for empty in ["{}", "null", "[]", "false", "0", "\"\""] {
        let (scratch, root) = plugin_with_expectation(empty);
        let missing = scratch.join("no-such-run");
        let (status, stdout, stderr) = exec(&[
            "startup",
            "check-expected",
            "azure-estimate",
            path_text(&missing),
            "--plugin-root",
            &root,
        ]);
        assert_eq!(status, 1, "{empty}: {stdout}{stderr}");
        assert!(!stdout.contains("PASS"), "{empty}: {stdout}");
        assert!(
            stdout.contains("expected-estimate.json"),
            "{empty}: {stdout}"
        );
    }
}

#[test]
fn an_empty_or_blank_document_fails_every_program_that_reads_it() {
    for blank in ["", " \n\t "] {
        let run = Copy::of("heroku-decision-gate", "after-decide-complete");
        run.write(".phase-status.json", blank);
        assert_fails_with("heroku-decide", run.path(), ".phase-status.json");
        let run = heroku_estimate_run();
        run.write("estimation-infra.json", blank);
        assert_fails_with("heroku-estimate", run.path(), "estimation-infra.json");
    }
}

// ---------------------------------------------- #4748: warnings keep their type

#[test]
fn forbidden_warning_text_is_found_in_every_shape_of_warnings() {
    let forbidden = r#"{"code":"unpriced_heroku_addon"}"#;
    for (label, value) in [
        ("array", format!("[{forbidden}]")),
        ("object", forbidden.to_string()),
        ("string", "\"unpriced_heroku_addon\"".to_string()),
    ] {
        let run = heroku_estimate_run();
        run.edit("estimation-infra.json", |doc| {
            set(doc, &["warnings"], j(&value));
        });
        let report = assert_fails("heroku-estimate", run.path());
        assert!(
            report.contains("warnings contain 'unpriced_heroku'"),
            "{label}: {report}"
        );
    }
}

#[test]
fn a_warned_service_in_a_non_array_warnings_value_still_counts_as_warned() {
    // The service is neither priced nor named in the breakdown; the supplied
    // warnings text (whatever its type) is what names it.
    let run = heroku_estimate_run();
    let design = j(&run.read("aws-design.json"));
    let sid = text_of(
        super::super::ledger::list(&design, "services")
            .first()
            .expect("a service"),
        &["service_id"],
    );
    run.edit("estimation-infra.json", |doc| {
        remove_breakdown_line(doc, &sid);
        set(doc, &["warnings"], text(&format!("{sid} is unpriced")));
        set(doc, &["projected_costs", "aws_monthly_balanced"], j("20.0"));
    });
    let report = assert_fails("heroku-estimate", run.path());
    // The sum identity breaks (one line fewer) but the service is covered.
    assert!(
        !report.contains(&format!("design service {sid} neither")),
        "{report}"
    );
}

fn remove_breakdown_line(doc: &mut Json, sid: &str) {
    let Json::Object(members) = super::step_mut(doc, &["projected_costs", "breakdown"]) else {
        panic!("breakdown object");
    };
    members.retain(|(key, _)| key != sid);
}

#[test]
fn a_design_with_services_that_is_not_a_list_fails() {
    let run = heroku_estimate_run();
    run.edit("aws-design.json", |doc| {
        set(doc, &["services"], j(r#"{"a": 1}"#));
    });
    assert_fails_with(
        "heroku-estimate",
        run.path(),
        "services is dict, not a list",
    );
}

// ------------------------------------- #4753: a boolean figure is not a number

#[test]
fn a_boolean_cost_difference_is_never_a_number() {
    for (label, edit) in [
        (
            "monthly true",
            [
                ("aws_monthly", "353.0"),
                ("monthly_difference", "true"),
                ("annual_difference", "12.0"),
            ],
        ),
        (
            "monthly false",
            [
                ("aws_monthly", "352.0"),
                ("monthly_difference", "false"),
                ("annual_difference", "0.0"),
            ],
        ),
        (
            "annual true",
            [
                ("aws_monthly", "353.0"),
                ("monthly_difference", "1.0"),
                ("annual_difference", "true"),
            ],
        ),
    ] {
        let run = heroku_estimate_run();
        run.edit("estimation-infra.json", |doc| {
            for (key, value) in edit {
                set(
                    doc,
                    &["cost_comparison", "option_b_balanced", key],
                    j(value),
                );
            }
        });
        let report = assert_fails("heroku-estimate", run.path());
        assert!(
            report.contains("cost_comparison.option_b_balanced")
                && (report.contains("monthly_difference") || report.contains("annual_difference")),
            "{label}: {report}"
        );
    }
    // Numeric controls with the same magnitudes pass.
    let run = heroku_estimate_run();
    run.edit("estimation-infra.json", |doc| {
        let lines = super::super::ledger::members(super::super::ledger::path(
            doc,
            &["projected_costs", "breakdown"],
        ))
        .len();
        let design_total = 10.0 * f64::from(u32::try_from(lines).expect("few lines"));
        let diff = design_total - 352.0;
        set(
            doc,
            &["cost_comparison", "option_b_balanced", "annual_difference"],
            Json::Float(diff * 12.0),
        );
    });
    assert_passes("heroku-estimate", run.path());
}

// ------------------------------ #4754: an explanation must be explanatory text

fn first_breakdown_key(doc: &Json) -> String {
    super::super::ledger::members(super::super::ledger::path(
        doc,
        &["projected_costs", "breakdown"],
    ))
    .iter()
    .map(|(key, _)| key.clone())
    .find(|key| !key.starts_with('_') && key != "total")
    .expect("a breakdown line")
}

#[test]
fn a_non_string_basis_is_not_an_explanation() {
    for value in [
        "true",
        "7",
        r#"{"a": 1}"#,
        r#"["x"]"#,
        "false",
        "null",
        "\"  \"",
    ] {
        let run = Copy::of(AZURE, "after-estimate");
        run.edit("estimation-infra.json", |doc| {
            let key = first_breakdown_key(doc);
            set(
                doc,
                &["projected_costs", "breakdown", &key, "basis"],
                j(value),
            );
        });
        let report = assert_fails("azure-estimate", run.path());
        assert!(
            report.contains("basis is required on every line"),
            "{value}: {report}"
        );
    }
    let run = Copy::of(AZURE, "after-estimate");
    run.edit("estimation-infra.json", |doc| {
        let key = first_breakdown_key(doc);
        set(
            doc,
            &["projected_costs", "breakdown", &key, "basis"],
            text("Billed per hour at the published rate."),
        );
    });
    let _ = assert_passes_or_unrelated(run.path());
}

/// A valid replacement basis must not be what fails the run.
fn assert_passes_or_unrelated(run: &Path) -> String {
    let (_, stdout, _) = super::check("azure-estimate", run, &[]);
    assert!(
        !stdout.contains("basis is required on every line"),
        "{stdout}"
    );
    stdout
}

#[test]
fn a_non_string_zero_cost_cluster_note_is_not_an_explanation() {
    for value in ["true", "7", r#"{"a": 1}"#, r#"["x"]"#] {
        let run = Copy::of(AZURE, "after-estimate");
        run.edit("estimation-infra.json", |doc| {
            let Json::Array(entries) = super::step_mut(doc, &["cost_comparison", "per_cluster"])
            else {
                panic!("per_cluster list");
            };
            let zero = entries
                .iter_mut()
                .find(|entry| {
                    matches!(
                        super::super::ledger::get(entry, "right_sized_monthly"),
                        Json::Int(0)
                    ) || matches!(
                        super::super::ledger::get(entry, "right_sized_monthly"),
                        Json::Float(n) if *n == 0.0
                    )
                })
                .expect("a $0.00 cluster");
            set(zero, &["note"], j(value));
        });
        let report = assert_fails("azure-estimate", run.path());
        assert!(
            report.contains("is $0.00 with no note"),
            "{value}: {report}"
        );
    }
}

// ----------------------------------- #4759: derived directories are link-checked

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    fn gcp_run() -> Copy {
        gcp_baseline_run()
    }

    fn run_in(root: &Path, kind: &str, run: &Path) -> (u8, String, String) {
        exec(&[
            "startup",
            "check-expected",
            kind,
            path_text(run),
            "--plugin-root",
            path_text(root),
        ])
    }

    #[test]
    fn a_linked_fixtures_directory_below_the_plugin_root_is_refused() {
        let scratch = TempDir::new("check-expected-link-fixtures");
        let root = scratch.dir("linked-plugin");
        symlink(plugin_root().join("fixtures"), root.join("fixtures")).expect("link");
        let run = gcp_run();
        let (status, stdout, stderr) = run_in(&root, "gcp-baseline", run.path());
        assert_eq!(status, 1, "{stdout}{stderr}");
        assert!(
            stdout.contains("symbolic links are not followed"),
            "{stdout}"
        );
        assert!(!stdout.contains("PASS"), "{stdout}");
    }

    #[test]
    fn a_linked_fixture_set_directory_is_refused() {
        let scratch = TempDir::new("check-expected-link-set");
        let root = scratch.dir("plugin");
        fs::create_dir_all(root.join("fixtures")).expect("fixtures");
        symlink(
            plugin_root().join("fixtures/gcp-live-capture"),
            root.join("fixtures/gcp-live-capture"),
        )
        .expect("link");
        let run = gcp_run();
        let (status, stdout, stderr) = run_in(&root, "gcp-baseline", run.path());
        assert_eq!(status, 1, "{stdout}{stderr}");
        assert!(
            stdout.contains("symbolic links are not followed"),
            "{stdout}"
        );
        assert!(!stdout.contains("PASS"), "{stdout}");
    }

    #[test]
    fn a_linked_table_directory_is_refused_for_the_nonweb_tables() {
        let scratch = TempDir::new("check-expected-link-table");
        let root = scratch.dir("plugin");
        let real = plugin_root();
        copy_dir(
            &real.join("fixtures/heroku-nonweb-scaling"),
            &root.join("fixtures/heroku-nonweb-scaling"),
        );
        let design = root.join("skills/heroku-to-aws/knowledge");
        fs::create_dir_all(&design).expect("knowledge");
        let outside = scratch.dir("outside-design");
        copy_dir(
            &real.join("skills/heroku-to-aws/knowledge/design"),
            &outside,
        );
        let run = golden("heroku-nonweb-scaling", "after-design");

        // The control: a real table directory passes.
        copy_dir(&outside, &design.join("design"));
        let (status, stdout, stderr) = run_in(&root, "heroku-nonweb-scaling", &run);
        assert_eq!(status, 0, "{stdout}{stderr}");

        fs::remove_dir_all(design.join("design")).expect("remove copy");
        symlink(&outside, design.join("design")).expect("link");
        let (status, stdout, stderr) = run_in(&root, "heroku-nonweb-scaling", &run);
        assert_eq!(status, 1, "{stdout}{stderr}");
        assert!(
            stdout.contains("symbolic links are not followed"),
            "{stdout}"
        );
        assert!(!stdout.contains("PASS"), "{stdout}");
    }

    #[test]
    fn a_linked_skills_directory_is_refused_for_the_design_table() {
        let scratch = TempDir::new("check-expected-link-skills");
        let root = scratch.dir("plugin");
        let real = plugin_root();
        copy_dir(
            &real.join("fixtures/azure-iac-terraform"),
            &root.join("fixtures/azure-iac-terraform"),
        );
        symlink(real.join("skills"), root.join("skills")).expect("link");
        let run = golden(AZURE, "after-design");
        let (status, stdout, stderr) = run_in(&root, "azure-design", &run);
        assert_eq!(status, 1, "{stdout}{stderr}");
        assert!(
            stdout.contains("symbolic links are not followed"),
            "{stdout}"
        );
        assert!(!stdout.contains("PASS"), "{stdout}");
    }

    #[test]
    fn a_linked_plugin_manifest_directory_is_refused_for_the_plan_emitter() {
        let scratch = TempDir::new("check-expected-link-manifest");
        let root = scratch.dir("plugin");
        let real = plugin_root();
        copy_dir(
            &real.join("fixtures/heroku-decision-gate"),
            &root.join("fixtures/heroku-decision-gate"),
        );
        symlink(real.join(".claude-plugin"), root.join(".claude-plugin")).expect("link");
        let run = golden("heroku-decision-gate", "after-decide-complete");
        let (status, stdout, stderr) = run_in(&root, "heroku-decide", &run);
        assert_eq!(status, 1, "{stdout}{stderr}");
        assert!(
            stdout.contains("symbolic links are not followed"),
            "{stdout}"
        );
    }

    #[test]
    fn a_run_directory_below_a_linked_plugin_root_keeps_its_explicit_root() {
        // The explicit --plugin-root may itself be a link (the caller's
        // choice), and so may a directory above RUN_DIR.
        let scratch = TempDir::new("check-expected-explicit-root");
        let via = scratch.dir("via");
        symlink(plugin_root(), via.join("plugin")).expect("link root");
        let run = gcp_run();
        let (status, stdout, stderr) = run_in(&via.join("plugin"), "gcp-baseline", run.path());
        assert_eq!(status, 0, "{stdout}{stderr}");
        let run_via = scratch.dir("run-via");
        symlink(run.path(), run_via.join("run")).expect("link run");
        let (status, stdout, stderr) = run_in(&plugin_root(), "gcp-baseline", &run_via.join("run"));
        assert_eq!(status, 0, "{stdout}{stderr}");
    }
}

#[test]
fn the_synthesised_baseline_is_the_committed_one() {
    // The long-prefix and bound tests above edit this document.
    assert!(GCP_ESTIMATE.contains("baseline_note"));
    assert_fails_with(
        "gcp-baseline",
        Copy::empty().path(),
        "neither estimation-infra.json",
    );
}

// ------------------------------- reads are bounded; the bound is an error

#[test]
fn a_document_over_the_read_bound_is_an_error_not_a_truncated_input() {
    let run = Copy::of(AZURE, "after-estimate");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(run.path().join("estimation-infra.json"))
        .expect("open document");
    // A sparse file one byte past the shared 64 MiB bound.
    file.set_len(64 * 1024 * 1024 + 1).expect("extend document");
    drop(file);
    let report = assert_fails("azure-estimate", run.path());
    assert!(report.contains("larger than 67108864 bytes"), "{report}");

    // The scratch copy of a run directory refuses such a file as well.
    let run = Copy::of("heroku-decision-gate", "after-decide-complete");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(run.path().join("estimation-infra.json"))
        .expect("open document");
    file.set_len(64 * 1024 * 1024 + 1).expect("extend document");
    drop(file);
    let report = assert_fails("heroku-decide", run.path());
    assert!(report.contains("larger than 67108864 bytes"), "{report}");
}
