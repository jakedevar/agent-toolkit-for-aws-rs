//! Offline, deterministic tests of `startup model-staleness` (#4407). The
//! current date is always injected ([`Date`]); nothing reads the clock except
//! the CLI-routing tests, whose registries use a window so wide that any real
//! date is inside it.

#![allow(clippy::too_many_lines)]

use std::fs;
use std::path::Path;

use toolkit_core::ErrorKind;

use super::*;
use crate::startup::scoring::fixtures::{exec, path_text, real_skill, write, TempDir};

const NOW: (i64, i64, i64) = (2026, 10, 27);
const CANONICAL_PATH: &str = "skills/shared/ai/ai-model-lifecycle.md";
const GCP_COPY: &str = "skills/gcp-to-aws/references/vendored/ai/ai-model-lifecycle.md";
const AZURE_COPY: &str = "skills/azure-to-aws/references/vendored/ai/ai-model-lifecycle.md";
const EXTRA_COPY: &str = "skills/agent-advisor/references/vendored/ai/ai-model-lifecycle.md";

fn now() -> Date {
    Date::new(NOW.0, NOW.1, NOW.2).expect("a real date")
}

fn registry(updated: &str, window: &str) -> String {
    format!(
        "# Bedrock Model Lifecycle Awareness\n\n<!-- freshness-contract -->\n\n\
         **Last updated:** {updated}\n\n**Staleness window:** {window}\n\nbody\n"
    )
}

/// A plugin whose canonical registry and both required copies hold `text`.
fn plugin_with(text: &str) -> TempDir {
    let root = TempDir::new("staleness");
    for relative in [CANONICAL_PATH, GCP_COPY, AZURE_COPY] {
        write(&root, relative, text);
    }
    root
}

fn fresh_plugin() -> TempDir {
    plugin_with(&registry("2026-09-21", "45 days"))
}

struct Run {
    result: Result<(), ToolkitError>,
    stdout: String,
    stderr: String,
}

fn run_at(root: &Path, strict: bool, today: Date) -> Run {
    let invocation = Invocation {
        positionals: Vec::new(),
        flags: if strict {
            vec!["--strict".to_string()]
        } else {
            Vec::new()
        },
        values: vec![("--plugin-root".to_string(), path_text(root).to_string())],
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = {
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        run_model_staleness(&invocation, &mut output, today)
    };
    Run {
        result,
        stdout: String::from_utf8(out).expect("utf-8"),
        stderr: String::from_utf8(err).expect("utf-8"),
    }
}

fn findings_at(root: &Path, today: Date) -> Findings {
    let mut findings = Findings::default();
    check_plugin(root, today, &mut findings);
    findings
}

fn failure_message(run: &Run) -> &str {
    run.result.as_ref().expect_err("the run fails").message()
}

// ---- dates -----------------------------------------------------------------

#[test]
fn only_real_calendar_dates_exist() {
    for (y, m, d) in [
        (2024, 2, 29),
        (2000, 2, 29),
        (1, 1, 1),
        (9999, 12, 31),
        (2026, 4, 30),
    ] {
        assert!(Date::new(y, m, d).is_some(), "{y}-{m}-{d}");
    }
    for (y, m, d) in [
        (2023, 2, 29),
        (1900, 2, 29),
        (2026, 4, 31),
        (2026, 13, 1),
        (2026, 0, 1),
        (2026, 1, 0),
        (2026, 1, 32),
        (0, 1, 1),
        (10000, 1, 1),
    ] {
        assert!(Date::new(y, m, d).is_none(), "{y}-{m}-{d}");
    }
}

#[test]
fn day_counts_cross_months_years_and_leap_days() {
    let date = |y, m, d| Date::new(y, m, d).expect("date");
    assert_eq!(date(1970, 1, 1), Date(0));
    assert_eq!(date(2026, 10, 27).days_since(date(2026, 9, 21)), 36);
    assert_eq!(date(2024, 3, 1).days_since(date(2024, 2, 28)), 2);
    assert_eq!(date(2023, 3, 1).days_since(date(2023, 2, 28)), 1);
    assert_eq!(date(2000, 3, 1).days_since(date(1999, 3, 1)), 366);
    assert_eq!(date(2026, 1, 1).days_since(date(2027, 1, 1)), -365);
    assert_eq!(date(1969, 12, 31), Date(-1));
}

#[test]
fn iso_dates_are_parsed_like_strptime() {
    assert!(parse_iso_date("2026-02-28").is_some());
    for bad in [
        "2026-02-30",
        "2026-00-10",
        "2026-13-01",
        "2026-01-00",
        "0000-01-01",
    ] {
        assert!(parse_iso_date(bad).is_none(), "{bad}");
    }
}

// ---- the freshness contract --------------------------------------------------

#[test]
fn a_fresh_registry_passes_and_reports_its_headroom() {
    let root = fresh_plugin();
    let run = run_at(&root, true, now());
    assert!(run.result.is_ok(), "{:?}", run.result);
    assert_eq!(
        run.stdout,
        "  ok: skills/shared/ai/ai-model-lifecycle.md: fresh — 36d old (window 45d, 9d of \
         headroom).\n\nmodel-staleness: model registry fresh and in sync.\n"
    );
    assert_eq!(run.stderr, "");
}

#[test]
fn the_window_boundary_is_inclusive() {
    let root = fresh_plugin();
    let on_edge = Date::new(2026, 11, 5).expect("date");
    let findings = findings_at(&root, on_edge);
    assert!(
        findings.stale.is_empty() && findings.hard.is_empty(),
        "{findings:?}"
    );
    assert!(findings.notes[0].contains("45d old (window 45d, 0d of headroom)"));

    let over = Date::new(2026, 11, 6).expect("date");
    let findings = findings_at(&root, over);
    assert_eq!(
        findings.stale,
        vec![
            "skills/shared/ai/ai-model-lifecycle.md: STALE — last updated 2026-09-21 (46d ago), \
             window 45d, over by 1d. Refresh the registry against the Bedrock model lifecycle \
             page and bump 'Last updated'."
                .to_string()
        ]
    );
    assert!(findings.hard.is_empty() && findings.notes.is_empty());
}

#[test]
fn staleness_warns_without_strict_and_fails_with_it() {
    let root = fresh_plugin();
    let later = Date::new(2027, 1, 1).expect("date");
    let lax = run_at(&root, false, later);
    assert!(lax.result.is_ok());
    assert!(lax
        .stderr
        .starts_with("  WARN: skills/shared/ai/ai-model-lifecycle.md: STALE"));
    assert_eq!(
        lax.stdout,
        "\nmodel-staleness: 1 stale registry(ies) — warn-only (--strict fails on these).\n"
    );

    let strict = run_at(&root, true, later);
    assert!(strict
        .stderr
        .starts_with("  ERROR: skills/shared/ai/ai-model-lifecycle.md: STALE"));
    assert_eq!(
        failure_message(&strict),
        "model-staleness: 1 problem(s) — failing."
    );
    assert_eq!(
        strict.result.as_ref().expect_err("fails").kind(),
        ErrorKind::Runtime
    );
    assert_eq!(strict.stdout, "");
}

#[test]
fn a_future_date_is_a_hard_error_even_without_strict() {
    let root = plugin_with(&registry("2026-10-30", "45 days"));
    let run = run_at(&root, false, now());
    assert_eq!(
        run.stderr,
        "  ERROR: skills/shared/ai/ai-model-lifecycle.md: '**Last updated:** 2026-10-30' is in \
         the future (3d from now) — a bad bump would disable the freshness gate. Fix the date.\n"
    );
    assert_eq!(
        failure_message(&run),
        "model-staleness: 1 problem(s) — failing."
    );
}

#[test]
fn an_impossible_date_is_a_hard_error() {
    let root = plugin_with(&registry("2026-02-30", "45 days"));
    let findings = findings_at(&root, now());
    assert_eq!(
        findings.hard,
        vec![
            "skills/shared/ai/ai-model-lifecycle.md: '**Last updated:** 2026-02-30' is not a \
             real calendar date."
                .to_string()
        ]
    );
}

#[test]
fn a_missing_or_incomplete_contract_is_a_hard_error() {
    let no_date = plugin_with("# R\n\n**Staleness window:** 45 days\n");
    assert_eq!(
        findings_at(&no_date, now()).hard,
        vec![
            "skills/shared/ai/ai-model-lifecycle.md: no '**Last updated:** YYYY-MM-DD' line — \
             the freshness contract is missing."
                .to_string()
        ]
    );
    let no_window = plugin_with("# R\n\n**Last updated:** 2026-09-21\n");
    assert_eq!(
        findings_at(&no_window, now()).hard,
        vec![
            "skills/shared/ai/ai-model-lifecycle.md: no '**Staleness window:** N days' line — \
             the freshness contract is incomplete."
                .to_string()
        ]
    );
}

#[test]
fn contract_lines_follow_the_python_patterns() {
    let accepted = [
        // No space, trailing blanks, a value on the next line (`\s*` crosses lines).
        "**Last updated:**2026-09-21   \n**Staleness window:**45 day\n",
        "**Last updated:**\n\n2026-09-21\n**Staleness window:** 45 days\t\n",
        // Leading zeros print without them.
        "**Last updated:** 2026-09-21\n**Staleness window:** 045 days",
        // CRLF is read as LF.
        "**Last updated:** 2026-09-21\r\n**Staleness window:** 45 days\r\n",
        // An earlier line that does not match is skipped for a later one.
        "**Last updated:** see below\n**Last updated:** 2026-09-21\n**Staleness window:** 45 days\n",
    ];
    for text in accepted {
        let root = plugin_with(text);
        let findings = findings_at(&root, now());
        assert!(findings.hard.is_empty(), "{text:?}: {findings:?}");
        assert!(
            findings.notes[0].contains("(window 45d, 9d of headroom)"),
            "{text:?}"
        );
    }
    let rejected = [
        "  **Last updated:** 2026-09-21\n**Staleness window:** 45 days\n",
        "**Last updated:** 2026-09-21 extra\n**Staleness window:** 45 days\n",
        "**Last updated:** 2026-09-211\n**Staleness window:** 45 days\n",
        "**Last updated:** 26-09-21\n**Staleness window:** 45 days\n",
        "**Last updated:** 2026-09-21\n**Staleness window:** 45 Days\n",
        "**Last updated:** 2026-09-21\n**Staleness window:** 45 days later\n",
        "**Last updated:** 2026-09-21\n**Staleness window:** days\n",
        "**Last updated:** 2026-09-21\n**Staleness window:** 45 dayss\n",
        "**last updated:** 2026-09-21\n**Staleness window:** 45 days\n",
    ];
    for text in rejected {
        let root = plugin_with(text);
        assert!(!findings_at(&root, now()).hard.is_empty(), "{text:?}");
    }
}

#[test]
fn a_window_too_large_for_a_day_count_is_rejected() {
    let huge = "9".repeat(40);
    let root = plugin_with(&registry("2026-09-21", &format!("{huge} days")));
    let hard = findings_at(&root, now()).hard;
    assert_eq!(hard.len(), 1, "{hard:?}");
    assert!(hard[0].contains("is too large to be a day count"));
    // 38 digits still fit.
    let fits = "1".repeat(38);
    let root = plugin_with(&registry("2026-09-21", &format!("{fits} days")));
    let findings = findings_at(&root, now());
    assert!(
        findings.hard.is_empty() && findings.stale.is_empty(),
        "{findings:?}"
    );
}

// ---- vendored copies ---------------------------------------------------------

#[test]
fn a_drifted_copy_fails_even_without_strict() {
    let root = fresh_plugin();
    write(
        &root,
        GCP_COPY,
        &format!("{}\nedit\n", registry("2026-09-21", "45 days")),
    );
    let run = run_at(&root, false, now());
    assert_eq!(
        run.stderr,
        format!(
            "  ERROR: {GCP_COPY}: DRIFTED from canonical {CANONICAL_PATH} — copy the canonical \
             file over it (they must stay byte-identical).\n"
        )
    );
    assert_eq!(
        failure_message(&run),
        "model-staleness: 1 problem(s) — failing."
    );
}

#[test]
fn a_copy_that_differs_only_in_line_endings_has_drifted() {
    let root = fresh_plugin();
    write(
        &root,
        AZURE_COPY,
        &registry("2026-09-21", "45 days").replace('\n', "\r\n"),
    );
    let hard = findings_at(&root, now()).hard;
    assert_eq!(hard.len(), 1, "{hard:?}");
    assert!(hard[0].starts_with(&format!("{AZURE_COPY}: DRIFTED")));
}

#[test]
fn a_missing_required_copy_fails_and_an_extra_matching_copy_passes() {
    let root = fresh_plugin();
    fs::remove_file(root.join(AZURE_COPY)).expect("remove copy");
    write(&root, EXTRA_COPY, &registry("2026-09-21", "45 days"));
    let hard = findings_at(&root, now()).hard;
    assert_eq!(
        hard,
        vec![format!(
            "{AZURE_COPY}: MISSING — a required vendored copy of the model-lifecycle registry. \
             AI estimate/design load this path; do not delete it. Copy the canonical \
             {CANONICAL_PATH} into place."
        )]
    );
    // The extra copy is compared too.
    write(&root, EXTRA_COPY, "different\n");
    let hard = findings_at(&root, now()).hard;
    assert!(
        hard.iter()
            .any(|m| m.starts_with(&format!("{EXTRA_COPY}: DRIFTED"))),
        "{hard:?}"
    );
}

#[test]
fn a_registry_without_its_canonical_file_fails_while_a_copy_survives() {
    let root = fresh_plugin();
    fs::remove_file(root.join(CANONICAL_PATH)).expect("remove canonical");
    write(&root, "skills/other/deeper/ai-model-lifecycle.md", "x\n");
    let hard = findings_at(&root, now()).hard;
    assert_eq!(
        hard,
        vec![format!(
            "canonical {CANONICAL_PATH} is MISSING, but {GCP_COPY}, {AZURE_COPY}, \
             skills/other/deeper/ai-model-lifecycle.md still exist and are read by \
             estimate-ai.md/design.md — this plugin still has an active model registry with NO \
             freshness gate. Restore the canonical file (or delete every surviving copy if the \
             registry is being retired intentionally)."
        )]
    );
    // Only an unlisted copy survives: still a failure.
    fs::remove_file(root.join(GCP_COPY)).expect("remove");
    fs::remove_file(root.join(AZURE_COPY)).expect("remove");
    let hard = findings_at(&root, now()).hard;
    assert_eq!(hard.len(), 1);
    assert!(hard[0].contains("but skills/other/deeper/ai-model-lifecycle.md still exist"));
}

#[test]
fn a_plugin_without_any_registry_has_nothing_to_gate() {
    let root = TempDir::new("staleness-none");
    write(&root, "skills/x/SKILL.md", "x\n");
    let run = run_at(&root, true, now());
    assert!(run.result.is_ok());
    assert_eq!(
        run.stdout,
        "\nmodel-staleness: model registry fresh and in sync.\n"
    );
    assert_eq!(run.stderr, "");
}

#[test]
fn every_problem_is_counted() {
    let root = fresh_plugin();
    write(&root, GCP_COPY, "drift\n");
    fs::remove_file(root.join(AZURE_COPY)).expect("remove");
    let later = Date::new(2027, 6, 1).expect("date");
    let run = run_at(&root, true, later);
    assert_eq!(
        failure_message(&run),
        "model-staleness: 3 problem(s) — failing."
    );
    let lax = run_at(&root, false, later);
    assert_eq!(
        failure_message(&lax),
        "model-staleness: 2 problem(s) — failing."
    );
    assert!(lax.stderr.contains("  WARN: "), "{}", lax.stderr);
}

// ---- unreadable and hostile inputs -------------------------------------------

#[test]
fn an_unreadable_canonical_file_is_reported_not_guessed() {
    let root = fresh_plugin();
    fs::write(root.join(CANONICAL_PATH), [0xff, 0xfe, 0x00]).expect("write");
    let hard = findings_at(&root, now()).hard;
    assert_eq!(
        hard,
        vec![format!("{CANONICAL_PATH}: cannot read: not valid UTF-8")]
    );
}

#[cfg(unix)]
#[test]
fn linked_registry_files_are_refused() {
    use std::os::unix::fs::symlink;

    let root = fresh_plugin();
    let elsewhere = TempDir::new("staleness-target");
    write(&elsewhere, "real.md", &registry("2026-09-21", "45 days"));
    fs::remove_file(root.join(CANONICAL_PATH)).expect("remove");
    symlink(elsewhere.join("real.md"), root.join(CANONICAL_PATH)).expect("link");
    let hard = findings_at(&root, now()).hard;
    assert_eq!(hard.len(), 1, "{hard:?}");
    assert!(
        hard[0].contains("symbolic links are not followed"),
        "{hard:?}"
    );

    let root = fresh_plugin();
    fs::remove_file(root.join(GCP_COPY)).expect("remove");
    symlink(root.join(CANONICAL_PATH), root.join(GCP_COPY)).expect("link");
    let hard = findings_at(&root, now()).hard;
    assert!(
        hard.iter()
            .all(|m| m.contains("symbolic links are not followed"))
            && !hard.is_empty(),
        "{hard:?}"
    );
}

#[cfg(unix)]
#[test]
fn caller_derived_names_are_escaped_in_every_message() {
    let root = fresh_plugin();
    let hostile = "evil\u{1b}[31m\u{7}\nerror: forged\u{202e}x";
    write(
        &root,
        &format!("skills/{hostile}/references/vendored/ai/ai-model-lifecycle.md"),
        "drift\n",
    );
    let run = run_at(&root, true, now());
    for stream in [&run.stdout, &run.stderr] {
        for raw in ['\u{1b}', '\u{7}', '\r', '\u{202e}'] {
            assert!(!stream.contains(raw), "{raw:?} in {stream:?}");
        }
        assert!(
            !stream.lines().any(|l| l.starts_with("error: forged")),
            "{stream:?}"
        );
    }
    assert!(run.stderr.contains("DRIFTED"), "{}", run.stderr);
    assert!(run.stderr.contains("\\u001B[31m"), "{}", run.stderr);
    // The canonical-missing message lists the same hostile name.
    fs::remove_file(root.join(CANONICAL_PATH)).expect("remove");
    let run = run_at(&root, true, now());
    assert!(!run.stderr.contains('\u{1b}') && !run.stderr.contains('\u{202e}'));
    assert!(!run.stderr.lines().any(|l| l.starts_with("error: forged")));
}

// ---- command routing -----------------------------------------------------------

#[test]
fn the_routed_command_checks_a_plugin_and_reports_failures() {
    // A window wide enough that the real clock is always inside it.
    let root = plugin_with(&registry("2026-01-01", "99999 days"));
    let (status, stdout, stderr) = exec(&[
        "startup",
        "model-staleness",
        "--plugin-root",
        path_text(&root),
        "--strict",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    assert!(
        stdout.contains("model registry fresh and in sync."),
        "{stdout}"
    );

    write(&root, AZURE_COPY, "drift\n");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "model-staleness",
        "--plugin-root",
        path_text(&root),
    ]);
    assert_eq!(status, 1);
    assert!(stdout.starts_with("  ok: "), "{stdout}");
    assert!(stderr.contains("  ERROR: "), "{stderr}");
    assert!(
        stderr.ends_with("error: model-staleness: 1 problem(s) — failing.\n"),
        "{stderr}"
    );
}

#[test]
fn a_missing_plugin_root_and_unknown_flags_are_usage_errors() {
    let missing = TempDir::new("staleness-missing");
    let nowhere = missing.join("absent");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "model-staleness",
        "--plugin-root",
        path_text(&nowhere),
    ]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(
        stderr.starts_with("error: Plugin directory not found: "),
        "{stderr}"
    );
    for args in [
        vec!["startup", "model-staleness", "--wat"],
        vec!["startup", "model-staleness", "extra"],
        vec!["startup", "model-staleness", "--plugin-root"],
        vec!["startup", "model-staleness", "--plugin-root", "--strict"],
        vec!["startup", "model-staleness", "--strict=1"],
    ] {
        assert_eq!(exec(&args).0, 2, "{args:?}");
    }
}

// ---- the real plugin tree --------------------------------------------------------

#[test]
fn the_real_registry_and_its_copies_are_consistent() {
    let plugin = real_skill().join("../..");
    // Inside the window as of the injected date: fresh and in sync.
    let findings = findings_at(&plugin, now());
    assert!(findings.hard.is_empty(), "{findings:?}");
    assert!(findings.stale.is_empty(), "{findings:?}");
    assert_eq!(findings.notes.len(), 1);
    // A later injected date proves the age is computed from the registry's own
    // declared date and window.
    let later = Date::new(2028, 1, 1).expect("date");
    let findings = findings_at(&plugin, later);
    assert!(findings.hard.is_empty(), "{findings:?}");
    assert_eq!(findings.stale.len(), 1, "{findings:?}");
    assert!(findings.stale[0].contains("window 45d"), "{findings:?}");
}
