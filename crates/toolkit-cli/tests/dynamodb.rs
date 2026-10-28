//! End-to-end tests of `toolkit dynamodb ...`: real process spawns, real exit
//! codes and separated stdout/stderr. Fixtures are deterministic offline
//! directories; no network, AWS call or pricing API is involved.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_root(label: &str) -> TempDir {
    let unique = format!(
        "toolkit-dynamodb-cli-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn toolkit(args: &[&str], cwd: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("spawn toolkit")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

const MODEL: &str = r#"{
    "tables": [{"table_name": "Orders", "key_schema": {"partition_key": "pk"},
        "entities": [{"entity_name": "Order", "estimated_item_size_bytes": 1024,
                      "estimated_item_count": 1000, "attributes": [{"name": "pk", "type": "S"}]}]}],
    "access_patterns": [
        {"pattern_id": "GetOrder", "operation": "GetItem", "table": "Orders",
         "peak_rps": 100, "estimated_item_size_bytes": 1024},
        {"pattern_id": "PutOrder", "operation": "PutItem", "table": "Orders",
         "peak_rps": 10, "estimated_item_size_bytes": 2000}]
}"#;

fn model_dir(label: &str) -> TempDir {
    let dir = temp_root(label);
    fs::write(dir.join("model.json"), MODEL).expect("model");
    dir
}

// ---------------------------------------------------------------------------
// calculate-costs
// ---------------------------------------------------------------------------

#[test]
fn calculate_costs_writes_the_report_beside_the_model() {
    let dir = model_dir("calc");
    let model = dir.join("model.json");
    let output = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            model.to_str().expect("utf-8"),
        ],
        &dir,
    );
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        text(&output.stdout),
        format!(
            "Cost report written to {}\nAnalyzed 2 access patterns across 1 tables.\n",
            dir.join("cost_report.md").display()
        )
    );
    let report = fs::read_to_string(dir.join("cost_report.md")).expect("report");
    assert!(report.starts_with("# DynamoDB Cost Report\n\n> **Disclaimer:**"));
    assert!(report.contains("**Peak-Sustained Monthly Cost: $62.21**"));
    assert!(report
        .contains("| PutOrder | PutItem   | Orders      | 10.0     | 2.00    | $32.40       |"));
    assert!(
        !report.ends_with('\n'),
        "the report has no trailing newline"
    );

    // Re-running is deterministic.
    let again = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            model.to_str().expect("utf-8"),
        ],
        &dir,
    );
    assert_eq!(status(&again), 0);
    assert_eq!(
        fs::read_to_string(dir.join("cost_report.md")).expect("report"),
        report
    );
}

#[test]
fn relative_model_and_output_paths_are_normalized_in_the_summary() {
    let dir = model_dir("relative");
    let output = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "./model.json",
            "--output",
            "./out//custom.md",
        ],
        &dir,
    );
    // The output directory does not exist, so the write fails cleanly.
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stderr).starts_with("error: cannot write "),
        "{}",
        text(&output.stderr)
    );
    fs::create_dir_all(dir.join("out")).expect("out dir");
    let output = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "./model.json",
            "--output",
            "./out//custom.md",
        ],
        &dir,
    );
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert!(text(&output.stdout).starts_with("Cost report written to out/custom.md\n"));
    assert!(dir.join("out/custom.md").exists());
}

#[test]
fn option_spellings_and_requirements_are_accepted() {
    let dir = model_dir("options");
    let updates = r#"{"tables":[{"table_name":"T","entities":[{"entity_name":"E"}]}],
        "access_patterns":[{"pattern_id":"P","operation":"UpdateItem","table":"T",
                            "peak_rps":1,"estimated_item_size_bytes":900,"source_requirement":"R1"}]}"#;
    fs::write(dir.join("updates.json"), updates).expect("model");
    fs::write(
        dir.join("req.json"),
        r#"{"updates":{"R1":{"write_action":"create"}}}"#,
    )
    .expect("req");

    let plain = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model=updates.json",
            "--output=plain.md",
        ],
        &dir,
    );
    assert_eq!(status(&plain), 0, "{}", text(&plain.stderr));
    let with = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "updates.json",
            "--requirements",
            "req.json",
            "--output",
            "with.md",
        ],
        &dir,
    );
    assert_eq!(status(&with), 0, "{}", text(&with.stderr));
    let long = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "updates.json",
            "--requirements",
            "req.json",
            "--output",
            "long.md",
        ],
        &dir,
    );
    assert_eq!(status(&long), 0);
    // Without the requirement an UpdateItem creates nothing (0.11 GB fallback);
    // with it the pattern's writes drive storage (2.59 GB).
    assert!(fs::read_to_string(dir.join("plain.md"))
        .expect("plain")
        .contains("| 0.11         |"));
    let with_report = fs::read_to_string(dir.join("with.md")).expect("with");
    assert!(with_report.contains("| 2.59         |"));
    assert_eq!(
        with_report,
        fs::read_to_string(dir.join("long.md")).expect("long")
    );
}

#[test]
fn an_unreadable_requirements_file_only_warns() {
    let dir = model_dir("warn");
    let output = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "model.json",
            "--requirements",
            "absent.json",
        ],
        &dir,
    );
    assert_eq!(status(&output), 0);
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("Warning: Could not load requirements ("),
        "{stderr}"
    );
    assert!(stderr.ends_with("), using defaults.\n"), "{stderr}");
    assert!(dir.join("cost_report.md").exists());
    // Malformed requirements behave the same way.
    fs::write(dir.join("bad.json"), "{nope").expect("bad");
    let output = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "model.json",
            "--requirements",
            "bad.json",
        ],
        &dir,
    );
    assert_eq!(status(&output), 0);
    assert!(
        text(&output.stderr).contains("JSON parse error"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn calculate_costs_usage_and_model_errors_have_distinct_exit_codes() {
    let dir = model_dir("errors");
    // Usage errors: exit 2, usage text on stderr, nothing on stdout.
    for args in [
        vec!["dynamodb", "calculate-costs"],
        vec!["dynamodb", "calculate-costs", "--model"],
        vec!["dynamodb", "calculate-costs", "--model", "--output", "x"],
        vec![
            "dynamodb",
            "calculate-costs",
            "--model",
            "model.json",
            "extra",
        ],
        vec![
            "dynamodb",
            "calculate-costs",
            "--model",
            "model.json",
            "--bogus",
        ],
        vec![
            "dynamodb",
            "calculate-costs",
            "--model",
            "model.json",
            "--output",
        ],
    ] {
        let output = toolkit(&args, &dir);
        assert_eq!(status(&output), 2, "{args:?}");
        assert_eq!(text(&output.stdout), "");
        assert!(text(&output.stderr).starts_with("error: "), "{args:?}");
        assert!(text(&output.stderr).contains("Usage:"), "{args:?}");
    }
    let missing = toolkit(&["dynamodb", "calculate-costs"], &dir);
    assert!(
        text(&missing.stderr).starts_with("error: the following arguments are required: --model\n")
    );

    // Runtime errors: exit 1, message only.
    fs::write(
        dir.join("empty.json"),
        r#"{"tables": [], "access_patterns": []}"#,
    )
    .expect("empty");
    fs::write(dir.join("broken.json"), "{not json").expect("broken");
    fs::write(dir.join("list.json"), "[]").expect("list");
    for (model, expected) in [
        (
            "empty.json",
            "error: No access patterns found in the data model.\n",
        ),
        ("list.json", "error: the data model must be a JSON object\n"),
    ] {
        let output = toolkit(&["dynamodb", "calculate-costs", "--model", model], &dir);
        assert_eq!(status(&output), 1, "{model}");
        assert_eq!(text(&output.stderr), expected);
        assert_eq!(text(&output.stdout), "");
    }
    for (model, expected) in [
        (
            "broken.json",
            "error: cannot load model: JSON parse error at line 1,",
        ),
        (
            "absent.json",
            "error: cannot load model: path does not exist: ",
        ),
    ] {
        let output = toolkit(&["dynamodb", "calculate-costs", "--model", model], &dir);
        assert_eq!(status(&output), 1, "{model}");
        assert!(
            text(&output.stderr).starts_with(expected),
            "{}",
            text(&output.stderr)
        );
        assert!(!text(&output.stderr).contains("Usage:"));
    }
    // A malformed pattern is a model error, never a panic.
    fs::write(
        dir.join("typed.json"),
        r#"{"tables":[{"table_name":"T"}],"access_patterns":[{"pattern_id":"P","operation":"GetItem","peak_rps":"fast"}]}"#,
    )
    .expect("typed");
    let output = toolkit(
        &["dynamodb", "calculate-costs", "--model", "typed.json"],
        &dir,
    );
    assert_eq!(status(&output), 1);
    assert_eq!(
        text(&output.stderr),
        "error: `peak_rps` must be a number, got 'fast'\n"
    );
    assert!(
        !dir.join("cost_report.md").exists(),
        "failed runs write no report"
    );
}

#[test]
fn an_unpriceable_vector_design_fails_with_the_fix_hint_and_no_report() {
    let dir = temp_root("vector");
    let model = r#"{
        "tables": [{"table_name": "T", "vector_indexes": [
            {"name": "x", "dimensions": "1k", "distance_function": "COSINE"}]}],
        "access_patterns": [
            {"pattern_id": "Find", "operation": "SearchVectors", "table": "T", "vector_index": "x"}]
    }"#;
    fs::write(dir.join("model.json"), model).expect("model");
    let output = toolkit(
        &["dynamodb", "calculate-costs", "--model", "model.json"],
        &dir,
    );
    assert_eq!(status(&output), 1);
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: the vector portion of this data model cannot be priced as written.\n  - T: a vector index is missing `index_name` (found keys: ['dimensions', 'distance_function', 'name'])"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("  - T.<unnamed>: dimensions must be an integer 1-4096, got '1k'\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("  - T.<unnamed>: vector_attribute is required\n"),
        "{stderr}"
    );
    assert!(stderr.contains("  - Find: operation SearchVectors has no `index` (found keys: ['operation', 'pattern_id', 'table', 'vector_index'])"), "{stderr}");
    assert!(stderr.contains("\n\nFix the model — do NOT delete the vector index to get past this, and do NOT estimate the vector cost by hand."), "{stderr}");
    assert!(
        stderr.contains("      \"index_name\": \"ProductEmbeddingIndex\",\n"),
        "{stderr}"
    );
    assert!(
        stderr
            .ends_with("references/cost-model-schema.md ('Vector index' and 'Access pattern').\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("Usage:"));
    assert!(
        !dir.join("cost_report.md").exists(),
        "nothing is written for an unpriceable design"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_model_and_report_paths_are_refused() {
    let dir = model_dir("symlinks");
    let outside = temp_root("symlinks-outside");
    fs::write(outside.join("target.md"), "do not touch").expect("target");
    std::os::unix::fs::symlink(outside.join("target.md"), dir.join("cost_report.md"))
        .expect("link");
    let output = toolkit(
        &["dynamodb", "calculate-costs", "--model", "model.json"],
        &dir,
    );
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stderr).starts_with("error: refusing to follow symbolic link: "),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(outside.join("target.md")).expect("target"),
        "do not touch"
    );

    std::os::unix::fs::symlink(dir.join("model.json"), dir.join("linked.json")).expect("link");
    let output = toolkit(
        &[
            "dynamodb",
            "calculate-costs",
            "--model",
            "linked.json",
            "--output",
            "other.md",
        ],
        &dir,
    );
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stderr)
            .starts_with("error: cannot load model: refusing to follow symbolic link: "),
        "{}",
        text(&output.stderr)
    );
    assert!(!dir.join("other.md").exists());
}

#[test]
fn help_lists_the_dynamodb_commands() {
    let dir = temp_root("help");
    let output = toolkit(&["--help"], &dir);
    assert_eq!(status(&output), 0);
    let stdout = text(&output.stdout);
    assert!(stdout.contains("toolkit dynamodb calculate-costs --model FILE"));
    assert!(stdout
        .contains("toolkit dynamodb iterate-design --model FILE --config FILE --loop-state FILE"));
    let scoped = toolkit(&["dynamodb", "iterate-design", "--help"], &dir);
    assert_eq!(status(&scoped), 0);
    assert_eq!(text(&scoped.stdout), stdout);
}

// ---------------------------------------------------------------------------
// iterate-design
// ---------------------------------------------------------------------------

fn iterate_dir(label: &str) -> TempDir {
    let dir = model_dir(label);
    fs::write(dir.join("config.json"), "{}").expect("config");
    dir
}

const ITERATE: [&str; 8] = [
    "dynamodb",
    "iterate-design",
    "--model",
    "model.json",
    "--config",
    "config.json",
    "--loop-state",
    "loop_state.json",
];

fn iterate(dir: &Path, extra: &[&str]) -> Output {
    let mut args: Vec<&str> = ITERATE.to_vec();
    args.extend_from_slice(extra);
    toolkit(&args, dir)
}

#[test]
fn a_calculator_only_round_records_state_and_stops() {
    let dir = iterate_dir("iterate-calc");
    let first = iterate(
        &dir,
        &["--calculator-only", "--timestamp", "2026-01-01T00:00:00Z"],
    );
    assert_eq!(status(&first), 0, "{}", text(&first.stderr));
    assert_eq!(text(&first.stderr), "");
    let stdout = text(&first.stdout);
    assert!(
        stdout.starts_with("[iterate_design] round 0: schema_fingerprint="),
        "{stdout}"
    );
    assert!(stdout.contains("(prior=None) -> decision=calculator-only\n"));
    assert!(stdout.contains("\n[iterate_design] calculate_costs (calculator-only): toolkit dynamodb calculate-costs --model model.json --output "));
    assert!(stdout.contains("\nROUND 0 SUMMARY\n"));
    assert!(stdout.contains("  decision: calculator-only\n"));
    assert!(stdout.contains("  calculator monthly: $48.60\n"));
    assert!(stdout.ends_with("=== ROUND 0 COMPLETE — handing back to user. The loop does not self-iterate. Review the findings, decide a change, and re-run for the next round. ===\n"));
    assert!(dir.join("cost_report.md").exists());

    let state = fs::read_to_string(dir.join("loop_state.json")).expect("state");
    assert!(
        state.contains("\"created_at\": \"2026-01-01T00:00:00Z\""),
        "{state}"
    );
    assert!(state.contains("\"deploy_decision\": \"calculator-only\""));
    let loop_id = state
        .split("\"loop_id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("loop id");
    assert_eq!(loop_id.len(), 8);
    assert!(loop_id.chars().all(|c| c.is_ascii_hexdigit()), "{loop_id}");

    // The next round continues the same state and reports a delta; the loop
    // id stays.
    let second = iterate(&dir, &["--calculator-only"]);
    assert_eq!(status(&second), 0, "{}", text(&second.stderr));
    let stdout = text(&second.stdout);
    assert!(stdout.contains("ROUND 1 SUMMARY"), "{stdout}");
    assert!(
        stdout.contains("  delta vs round 0: {\"monthly_usd_pct\": null,"),
        "{stdout}"
    );
    let state = fs::read_to_string(dir.join("loop_state.json")).expect("state");
    assert!(state.contains(&format!("\"loop_id\": \"{loop_id}\"")));
    assert!(state.contains("\"round\": 1"));
}

#[test]
fn deploying_rounds_are_gated_and_the_unported_steps_stop_before_any_cloud_action() {
    let dir = iterate_dir("iterate-gates");
    // No consent: exit 4 with the legacy message, nothing written.
    let refused = iterate(&dir, &[]);
    assert_eq!(status(&refused), 4);
    let stderr = text(&refused.stderr);
    assert!(stderr.starts_with("error: this round requires a real AWS deploy (schema changed or no active deployment), but --yes-deploy was not passed."), "{stderr}");
    assert!(
        !stderr.contains("Usage:"),
        "runtime errors carry no usage text"
    );
    assert!(!dir.join("loop_state.json").exists());
    assert!(!dir.join(".iterate_config.json").exists());

    // --skip-deploy refuses a needed redeploy: exit 2.
    let skipped = iterate(&dir, &["--skip-deploy", "--yes-deploy"]);
    assert_eq!(status(&skipped), 2);
    assert!(text(&skipped.stderr).starts_with("error: --skip-deploy was passed but this round needs a redeploy (schema fingerprint changed: None -> "));

    // With consent the deploy step is `deploy-model`, which only plans offline:
    // it refuses this fixture config (no aws_profile) before any cloud action,
    // and the round cleans up.
    let consented = iterate(&dir, &["--yes-deploy"]);
    assert_eq!(status(&consented), 2);
    let stderr = text(&consented.stderr);
    assert!(
        stderr.contains("error: benchmark_config.json missing required field: aws_profile\n"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("error: step 'deploy_model' failed (exit 2). See output above.\n"),
        "{stderr}"
    );
    assert!(text(&consented.stdout).contains("\n[iterate_design] deploy_model: toolkit dynamodb deploy-model --model model.json --config "));
    assert!(!dir.join("loop_state.json").exists());
    assert!(!dir.join(".iterate_config.json").exists());
    assert!(
        !dir.join("created_resources.json").exists(),
        "no manifest was fabricated"
    );
}

#[test]
fn iterate_design_input_and_usage_errors() {
    let dir = iterate_dir("iterate-errors");
    // Missing required options: usage error, exit 2.
    for args in [
        vec!["dynamodb", "iterate-design"],
        vec![
            "dynamodb",
            "iterate-design",
            "--model",
            "model.json",
            "--config",
            "config.json",
        ],
        vec![
            "dynamodb",
            "iterate-design",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--loop-state",
            "s.json",
            "stray",
        ],
    ] {
        let output = toolkit(&args, &dir);
        assert_eq!(status(&output), 2, "{args:?}");
        assert!(text(&output.stderr).starts_with("error: "), "{args:?}");
    }
    let missing = toolkit(
        &[
            "dynamodb",
            "iterate-design",
            "--config",
            "c",
            "--loop-state",
            "s",
        ],
        &dir,
    );
    assert!(
        text(&missing.stderr).starts_with("error: the following arguments are required: --model\n")
    );

    // A missing input file is the legacy `_die` status 2.
    fs::remove_file(dir.join("config.json")).expect("remove");
    let output = iterate(&dir, &["--calculator-only"]);
    assert_eq!(status(&output), 2);
    assert!(
        text(&output.stderr).starts_with("error: file not found: config.json\n"),
        "{}",
        text(&output.stderr)
    );

    // A refused change keeps status 2 and leaves the design untouched.
    fs::write(dir.join("config.json"), "{}").expect("config");
    fs::write(dir.join("change.json"), r#"{"nothing":true}"#).expect("change");
    let before = fs::read_to_string(dir.join("model.json")).expect("model");
    let output = iterate(
        &dir,
        &["--calculator-only", "--apply-change", "change.json"],
    );
    assert_eq!(status(&output), 2);
    assert!(
        text(&output.stderr)
            .contains("--apply-change file must contain a top-level \"merge\" object"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("model.json")).expect("model"),
        before
    );
}
