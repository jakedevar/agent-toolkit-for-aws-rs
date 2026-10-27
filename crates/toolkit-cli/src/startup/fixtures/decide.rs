//! The Decision-gate choice-A asserters (`check_expected_decide.py` of the
//! Heroku and GCP decision-gate fixtures, and the Heroku retained-pack
//! wrapper, which runs the same assertions on the sibling fixture).
//!
//! A decide-complete run must sit in the terminal phase state
//! (`current_phase` complete, `run_mode` decide, `generate` pending, estimate
//! and workshop completed), carry `decision-report.html` and `DECISION.md`,
//! pass the report validator in decision mode, keep the typography locks, and
//! let `startup emit-plan` write a schema-shaped `plan.json` from the run's
//! estimate artifacts. The Python programs ran the shipped validator and
//! emitter scripts as subprocesses; this runs the Rust commands that replace
//! them (`startup migration-report`, `startup heroku-report`, `startup
//! emit-plan`) in memory.

use std::fs;
use std::path::Path;

use toolkit_core::Json;

use super::ledger::{get, number_or_bool, ps, truthy};
use super::{capture, Context, Flow, Scratch};

/// Which skill's decision gate is being checked.
#[derive(Clone, Copy)]
enum Platform {
    Heroku,
    Gcp,
}

pub(super) fn heroku(context: &Context<'_>) -> Flow {
    run(context, Platform::Heroku)
}

pub(super) fn gcp(context: &Context<'_>) -> Flow {
    run(context, Platform::Gcp)
}

fn run(context: &Context<'_>, platform: Platform) -> Flow {
    let l = context.ledger;
    let run = &context.run;

    // Terminal state: decide-complete, not failure, not in-flight.
    let ph = context.json_in_dir(run, ".phase-status.json")?;
    let phases = get(&ph, "phases");
    l.check(
        super::ledger::eq_str(get(&ph, "current_phase"), "complete"),
        format!("current_phase={}", ps(get(&ph, "current_phase"))),
    );
    l.check(
        super::ledger::eq_str(get(&ph, "run_mode"), "decide"),
        format!("run_mode={}", ps(get(&ph, "run_mode"))),
    );
    for (phase, want) in [
        ("generate", "pending"),
        ("estimate", "completed"),
        ("workshop", "completed"),
    ] {
        l.check(
            super::ledger::eq_str(get(phases, phase), want),
            format!("{phase}={}", ps(get(phases, phase))),
        );
    }

    // Decision pack exists. Pre-execution is a STATE fact (phases.generate
    // above), not raw file absence: a prior cycle's execution pack may remain
    // on disk after a workshop reprice, so the Heroku asserter does not demand
    // terraform/ or generation-*.json be absent (see the retained-execution-pack
    // fixture, which deliberately has both).
    let report_present = context.exists(run, "decision-report.html");
    l.check(report_present, "missing decision-report.html");
    l.check(context.exists(run, "DECISION.md"), "missing DECISION.md");
    if matches!(platform, Platform::Gcp) {
        l.check(
            fs::symlink_metadata(run.join("terraform")).is_err(),
            "terraform/ must not exist on a decide run",
        );
        l.check(
            !has_generation_json(run),
            "generation-*.json must not exist on a decide run",
        );
    }

    if report_present {
        check_report(context, platform);
    }
    check_plan(context, platform);
    Some(())
}

/// `any(run.glob("generation-*.json"))`.
fn has_generation_json(run: &Path) -> bool {
    let Ok(entries) = fs::read_dir(run) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("generation-") && name.ends_with(".json")
    })
}

/// The decision report passes the validator in decision mode and keeps the
/// content locks.
fn check_report(context: &Context<'_>, platform: Platform) {
    let l = context.ledger;
    let report = context.run.join("decision-report.html");
    let report_text = report.display().to_string();
    if matches!(platform, Platform::Gcp)
        && !context.guard_derived(
            &context
                .fixture
                .join("after-decide-complete")
                .join("estimation-infra.json"),
        )
    {
        return;
    }
    let (result, stdout, stderr) = match platform {
        // The cost-figure anchor gate is skipped (fail open on absence) unless
        // the estimation artifact is supplied, and `--no-require-toc` would
        // defeat the mandatory anchor gate, so neither is left out.
        Platform::Gcp => capture(
            crate::startup::migration_report_command,
            &[report_text.as_str()],
            &[
                ("--mode", "decision".to_string()),
                (
                    "--estimation-infra",
                    context
                        .fixture
                        .join("after-decide-complete")
                        .join("estimation-infra.json")
                        .display()
                        .to_string(),
                ),
            ],
        ),
        // Including the cross-artifact disk checks of the run directory.
        Platform::Heroku => capture(
            crate::startup::heroku_report_command,
            &[report_text.as_str()],
            &[
                ("--mode", "decision".to_string()),
                ("--migration-dir", context.run.display().to_string()),
            ],
        ),
    };
    l.check(
        result.is_ok(),
        format!(
            "decision-report.html fails --mode decision:\n{}",
            first_lines(&format!("{stdout}{stderr}"))
        ),
    );
    // Content locks (beyond structure): the typography-first verdict and the
    // "if you execute" timeline-band label must survive refactors.
    let Some(html) = context.text_if_exists(&context.run, "decision-report.html") else {
        return;
    };
    l.check(
        html.contains("class=\"verdict-headline\""),
        match platform {
            Platform::Heroku => "missing verdict-headline element",
            Platform::Gcp => "missing verdict-headline element (verdict typography)",
        },
    );
    if matches!(platform, Platform::Gcp) {
        l.check(
            html.contains("not directly comparable"),
            "missing not-comparable baseline sentence",
        );
    }
    l.check(
        html.contains("if you execute"),
        "timeline must be labeled \"if you execute\" (band, not schedule)",
    );
}

/// At most a few lines of a validator's output, so a message stays small.
fn first_lines(text: &str) -> String {
    text.lines().take(12).collect::<Vec<_>>().join("\n")
}

/// The decision-only exit writes the web-handoff `plan.json` from the run's
/// estimate artifacts. The emitter runs against a scratch copy (so the golden
/// directory stays clean) and must EMIT a schema-shaped file, not a skip.
fn check_plan(context: &Context<'_>, platform: Platform) {
    let l = context.ledger;
    let plugin_json = context
        .plugin_root
        .join(".claude-plugin")
        .join("plugin.json");
    if !context.guard_derived(&plugin_json) {
        return;
    }
    let scratch = match Scratch::copy_of(&context.run) {
        Ok(scratch) => scratch,
        Err(message) => {
            l.fail(message);
            return;
        }
    };
    let scratch_run = scratch.run_dir();
    let (result, stdout, stderr) = capture(
        crate::startup::emit_plan_command,
        &[],
        &[
            ("--migration-dir", scratch_run.display().to_string()),
            ("--plugin-json", plugin_json.display().to_string()),
        ],
    );
    l.check(
        result.is_ok(),
        format!("startup emit-plan failed: {}", first_lines(&stderr)),
    );
    let Some(data) = context.json_if_exists(&scratch_run, "plan.json") else {
        l.fail(format!(
            "decision-only exit did not write plan.json (writer: {})",
            stdout.trim()
        ));
        return;
    };
    let cost = get(&data, "cost");
    let want_platform = match platform {
        Platform::Heroku => "HEROKU",
        Platform::Gcp => "GCP",
    };
    l.check(
        super::ledger::eq_str(get(&data, "sourcePlatform"), want_platform),
        format!("sourcePlatform={}", ps(get(&data, "sourcePlatform"))),
    );
    l.check(
        super::ledger::eq_str(get(&data, "scope"), "INFRA_ONLY"),
        format!("scope={}", ps(get(&data, "scope"))),
    );
    l.check(
        super::ledger::eq_str(get(cost, "awsMonthlyBasis"), "BALANCED"),
        format!("awsMonthlyBasis={}", ps(get(cost, "awsMonthlyBasis"))),
    );
    l.check(
        number_or_bool(get(cost, "awsMonthly")).is_some(),
        "cost.awsMonthly missing/not a number",
    );
    l.check(
        truthy(get(&data, "producerVersion")),
        "producerVersion missing — plugin.json was not read",
    );
    let _: &Json = &data;
}
