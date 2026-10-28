//! End-to-end tests of `toolkit dynamodb benchmark-model` and
//! `generate-perf-report` (#4390): real process spawns, real exit codes,
//! separated stdout/stderr. Every fixture is a deterministic offline file; the
//! "Lambda" is a recording of the handler's response contract. Nothing contacts
//! AWS and no load is generated.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");
const NOW: &str = "2026-01-31T12:00:00Z";

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
    let path = std::env::temp_dir().join(format!(
        "toolkit-ddb-bench-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

fn toolkit(args: &[&str], cwd: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env_clear()
        .output()
        .expect("spawn toolkit")
}

fn json(source: &str) -> Json {
    parse_json(source).expect("valid json")
}

fn at<'a>(value: &'a Json, path: &[&str]) -> &'a Json {
    let mut current = value;
    for key in path {
        current = current
            .get(key)
            .unwrap_or_else(|| panic!("missing {key} in {path:?}"));
    }
    current
}

fn items(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        other => panic!("expected an array, got {}", other.type_name()),
    }
}

const MODEL: &str = r#"{
  "tables": [{"table_name": "Orders",
    "entities": [{"entity_name": "Order", "estimated_item_size_bytes": 1024,
                  "attributes": [{"name": "pk", "type": "S"}, {"name": "sk", "type": "S"}]}]}],
  "access_patterns": [
    {"pattern_id": "GetOrder", "operation": "GetItem", "table": "Orders", "peak_rps": 100,
     "estimated_item_size_bytes": 1024},
    {"pattern_id": "PutOrder", "operation": "PutItem", "table": "Orders", "peak_rps": 50,
     "estimated_item_size_bytes": 1024}]}"#;

const CONFIG: &str = r#"{"aws_profile": "bench", "region": "us-east-1", "mode": "quick"}"#;

const MANIFEST: &str = r#"{"account": "111122223333", "region": "us-east-1",
  "prefix": "ddb-skill-bench-abc-", "run_id": "0123456789abcdef0123456789abcdef",
  "tables": [{"name": "ddb-skill-bench-abc-Orders", "original_name": "Orders"}],
  "lambda": {"function_name": "ddb-skill-bench-abc-bench"}}"#;

const GOOD_PAYLOAD: &str = r#"{
  "invocation_index": 0,
  "phases_run": ["settle", "seed", "warmup", "measure"],
  "raw_rows": [
    {"pattern_id": "GetOrder", "op": "GetItem", "phase": "warmup", "latency_ms": 3.0, "consumed_cu": 0.5, "gsi_cu": {}, "throttled": false, "error": null},
    {"pattern_id": "GetOrder", "op": "GetItem", "phase": "measure", "latency_ms": 2.0, "consumed_cu": 0.5, "gsi_cu": {}, "throttled": false, "error": null},
    {"pattern_id": "GetOrder", "op": "GetItem", "phase": "measure", "latency_ms": 4.0, "consumed_cu": 0.5, "gsi_cu": {}, "throttled": false, "error": null},
    {"pattern_id": "GetOrder", "op": "GetItem", "phase": "measure", "latency_ms": 6.0, "consumed_cu": 0.5, "gsi_cu": {}, "throttled": false, "error": null},
    {"pattern_id": "PutOrder", "op": "PutItem", "phase": "measure", "latency_ms": 5.0, "consumed_cu": 1.0, "gsi_cu": {}, "throttled": false, "error": null},
    {"pattern_id": "PutOrder", "op": "PutItem", "phase": "measure", "latency_ms": 7.0, "consumed_cu": 1.0, "gsi_cu": {}, "throttled": false, "error": null}],
  "seed_verification": {"Orders": {"expected": 100, "actual": 100, "sampled": false, "passed": true, "seed_shortfall_ratio": 0.0}},
  "coverage": {"measured_patterns": ["GetOrder", "PutOrder"], "missing_patterns": [], "coverage_incomplete": false},
  "measurement_tainted": {},
  "exact_counts": [
    {"pattern_id": "GetOrder", "phase": "measure", "calls": 18, "throttles": 0, "errors": 0, "error_codes": {}, "cancellation_reason_codes": {}},
    {"pattern_id": "PutOrder", "phase": "measure", "calls": 18, "throttles": 0, "errors": 0, "error_codes": {}, "cancellation_reason_codes": {}}],
  "mode": "quick", "key_sampling": "uniform", "items_per_partition": 1}"#;

fn recording(steps: &[String]) -> String {
    format!(r#"{{"calls": [{}]}}"#, steps.join(","))
}

fn ok_step(payload: &str) -> String {
    format!(
        r#"{{"service": "lambda", "operation": "Invoke", "request": {{"FunctionName": "ddb-skill-bench-abc-bench"}},
            "response": {{"StatusCode": 200, "Payload": {payload}}}}}"#
    )
}

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new(label: &str, model: &str, config: &str, manifest: &str, steps: &[String]) -> Self {
        let root = temp_root(label);
        fs::write(root.join("model.json"), model).expect("model");
        fs::write(root.join("config.json"), config).expect("config");
        fs::write(root.join("manifest.json"), manifest).expect("manifest");
        fs::write(root.join("recorded.json"), recording(steps)).expect("recording");
        Self { root }
    }

    fn standard(label: &str) -> Self {
        Self::new(label, MODEL, CONFIG, MANIFEST, &[ok_step(GOOD_PAYLOAD)])
    }

    fn bench(&self, extra: &[&str]) -> Output {
        let mut args = vec![
            "dynamodb",
            "benchmark-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest",
            "manifest.json",
            "--raw-out",
            "perf_raw.jsonl",
            "--summary-out",
            "perf_summary.json",
            "--now",
            NOW,
        ];
        args.extend_from_slice(extra);
        toolkit(&args, &self.root)
    }

    fn recorded(&self) -> Output {
        self.bench(&["--recorded", "recorded.json"])
    }

    fn summary(&self) -> Json {
        json(&fs::read_to_string(self.root.join("perf_summary.json")).expect("summary"))
    }

    fn report(&self, summary: &str) -> Output {
        fs::write(self.root.join("summary_in.json"), summary).expect("summary in");
        toolkit(
            &[
                "dynamodb",
                "generate-perf-report",
                "--model",
                "model.json",
                "--summary",
                "summary_in.json",
                "--output",
                "report.md",
                "--findings-out",
                "findings.json",
            ],
            &self.root,
        )
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).expect(name)
    }
}

// ---------------------------------------------------------------------------
// benchmark-model
// ---------------------------------------------------------------------------

#[test]
fn a_recorded_run_writes_the_raw_rows_and_the_summary() {
    let fixture = Fixture::standard("run");
    let output = fixture.recorded();
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("Quick mode: running a short per-pattern window (settle=10s, warmup=3s, duration=15s, seed=100 items)."), "{stdout}");
    assert!(
        stdout.contains(
            "Benchmark budget: 2 pattern(s), splitting measurement across 1 Lambda invocation(s)."
        ),
        "{stdout}"
    );
    assert!(stdout.contains("Estimated wall-clock: ~2 min (96s: 10s settle + ~30s seed + 2 patterns x (3s warmup + 15s measure), serial)."), "{stdout}");
    assert!(!stdout.contains("RUN THIS IN THE FOREGROUND"), "{stdout}");
    assert!(stdout.contains("Estimated AWS spend for this run: ~$0.000 (driven load ~$0.000 + seeding ~$0.000). Guardrail: $0.50."), "{stdout}");
    assert!(
        stdout.contains(
            "Invoking Lambda (invocation 1/1, phases=['settle', 'seed', 'warmup', 'measure']) …"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "  returned in 0.0s, 6 rows, phases_run=['settle', 'seed', 'warmup', 'measure']"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("Raw rows: perf_raw.jsonl (6 rows)"),
        "{stdout}"
    );
    assert!(stdout.contains("Summary: perf_summary.json"), "{stdout}");
    assert!(stdout.contains("Benchmark completed at 2026-01-31T12:00:00+00:00 (0.0s wall, 6 rows) — verify this timestamp is newer than when you launched the run before trusting the report."), "{stdout}");
    assert_eq!(text(&output.stderr), "");

    let raw = fixture.read("perf_raw.jsonl");
    assert_eq!(raw.lines().count(), 6);
    assert!(raw.lines().next().expect("row").starts_with(
        r#"{"pattern_id": "GetOrder", "op": "GetItem", "phase": "warmup", "latency_ms": 3.0,"#
    ));

    let summary = fixture.summary();
    assert_eq!(
        at(&summary, &["run_id"]),
        &Json::string("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        at(&summary, &["manifest", "account"]),
        &Json::string("111122223333")
    );
    assert_eq!(
        at(&summary, &["run_started_at"]),
        &Json::string("2026-01-31T12:00:00+00:00")
    );
    assert_eq!(at(&summary, &["benchmark_wall_seconds"]), &Json::Float(0.0));
    assert_eq!(at(&summary, &["total_rows"]), &Json::Int(6));
    assert_eq!(at(&summary, &["mode"]), &Json::string("quick"));
    assert_eq!(at(&summary, &["invocations_total"]), &Json::Int(1));
    assert_eq!(
        at(&summary, &["config", "duration_seconds"]),
        &Json::Int(15)
    );
    assert_eq!(
        at(&summary, &["coverage", "coverage_incomplete"]),
        &Json::Bool(false)
    );
    let patterns = items(at(&summary, &["patterns"]));
    let get = &patterns[0];
    assert_eq!(at(get, &["pattern_id"]), &Json::string("GetOrder"));
    assert_eq!(at(get, &["bench_rps"]), &Json::Float(1.0));
    assert_eq!(at(get, &["steady_state", "call_count"]), &Json::Int(18));
    assert_eq!(at(get, &["steady_state", "rows_sampled"]), &Json::Int(3));
    assert_eq!(at(get, &["steady_state", "p50_ms"]), &Json::Float(4.0));
    assert_eq!(at(get, &["steady_state", "p95_ms"]), &Json::Float(5.8));
    assert_eq!(at(get, &["steady_state", "p99_ms"]), &Json::Float(5.96));
    assert_eq!(
        at(get, &["steady_state", "mean_observed_cu"]),
        &Json::Float(0.5)
    );
    assert_eq!(at(get, &["cold_start", "warmup_call_count"]), &Json::Int(1));
    assert_eq!(at(get, &["cold_start", "warmup_p50_ms"]), &Json::Float(3.0));
    assert_eq!(at(get, &["measurement_tainted"]), &Json::Bool(false));
    let put = &patterns[1];
    assert_eq!(at(put, &["steady_state", "p50_ms"]), &Json::Float(6.0));
    let text_out = fixture.read("perf_summary.json");
    assert!(
        text_out.starts_with("{\n  \"run_id\": \"0123456789abcdef0123456789abcdef\","),
        "{text_out}"
    );
    assert!(
        !text_out.ends_with('\n'),
        "json.dumps adds no trailing newline"
    );
}

#[test]
fn the_summary_feeds_the_report_and_a_clean_run_is_clean() {
    let fixture = Fixture::standard("report");
    assert_eq!(status(&fixture.recorded()), 0);
    let output = fixture.report(&fixture.read("perf_summary.json"));
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "Report: report.md\nFindings: findings.json\nAnalyzed 2 patterns, extracted 1 findings (clean).\n"
    );
    let report = fixture.read("report.md");
    assert!(
        report.starts_with("# DynamoDB Live Performance Report\n\n> **Disclaimer:**"),
        "{report}"
    );
    assert!(
        report.contains("**Extrapolated Monthly Cost (measurement-based): $97.20**"),
        "{report}"
    );
    assert!(
        report.contains("**Calculator Monthly Cost (expected):              $97.20**"),
        "{report}"
    );
    assert!(report.contains("Account: 111122223333   Region: us-east-1   Run ID: 0123456789abcdef0123456789abcdef   "), "{report}");
    assert!(
        report.contains(
            "Window: completed 2026-01-31T12:00:00+00:00 UTC, 0.0s wall, 6 measured rows.  "
        ),
        "{report}"
    );
    assert!(
        report.contains("Resources: 1 tables, 0 GSIs. Manifest: created_resources.json.  "),
        "{report}"
    );
    assert!(report.contains("| GetOrder | GetItem | Orders | 100 | 1.00 | 0.500 | 0.500 | +0.0% | 4.0 | 5.96 | 0 | 0 | $16.20 |"), "{report}");
    assert!(report.contains("| PutOrder | PutItem | Orders | 50 | 1.00 | 1.000 | 1.000 | +0.0% | 6.0 | 6.98 | 0 | 0 | $81.00 |"), "{report}");
    assert!(
        report.contains("| Seed verification") || report.contains("## Seed verification"),
        "{report}"
    );
    assert!(report.contains("| `Orders` | 100 | 100 | ok |"), "{report}");
    assert!(
        report.contains("Cost concentration: `PutOrder` drives ~83% of the estimate"),
        "{report}"
    );
    assert!(
        report.contains("- None: every pattern within ±10% of calculator prediction."),
        "{report}"
    );
    assert!(report.contains("Mechanics #3 (per-partition ceilings 1000 WCU / 3000 RCU): bench did not push to ceilings by design."), "{report}");
    assert!(
        report.contains("## Iteration offer"),
        "a medium design finding still offers an iteration: {report}"
    );
    assert!(!report.contains("INCOMPLETE COVERAGE"), "{report}");
    assert!(
        report.contains("drove 1 rps/pattern — the `min_rps_per_pattern` floor"),
        "{report}"
    );

    let findings = json(&fixture.read("findings.json"));
    assert_eq!(
        at(&findings, &["no_significant_findings"]),
        &Json::Bool(true)
    );
    assert_eq!(at(&findings, &["coverage_incomplete"]), &Json::Bool(false));
    let classified = items(at(&findings, &["classified_findings"]));
    assert_eq!(classified.len(), 1);
    assert_eq!(at(&classified[0], &["id"]), &Json::string("finding-1"));
    assert_eq!(
        at(&classified[0], &["signal"]),
        &Json::string("dominant_cost_patterns")
    );
    assert_eq!(at(&classified[0], &["severity"]), &Json::string("medium"));
    assert_eq!(
        items(at(&classified[0], &["axioms"])),
        [Json::string("Mechanics #1"), Json::string("Mechanics #8")]
    );
    let drivers = items(at(&findings, &["top_cost_drivers"]));
    assert_eq!(at(&drivers[0], &["pattern_id"]), &Json::string("PutOrder"));
}

#[test]
fn without_a_recording_every_offline_step_runs_then_the_connect_fails() {
    let fixture = Fixture::standard("nolive");
    let output = fixture.bench(&[]);
    assert_eq!(status(&output), 2);
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("Benchmark budget: 2 pattern(s)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Estimated AWS spend for this run"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Invoking Lambda (invocation 1/1"),
        "{stdout}"
    );
    let stderr = text(&output.stderr);
    assert!(stderr.contains("cannot reach AWS endpoint"), "{stderr}");
    assert!(stderr.contains("no live AWS client"), "{stderr}");
    assert!(!fixture.root.join("perf_summary.json").exists());
    assert!(!fixture.root.join("perf_raw.jsonl").exists());
}

#[test]
fn the_guardrail_refuses_with_status_3_unless_spend_is_allowed() {
    let config = r#"{"aws_profile": "bench", "region": "us-east-1", "mode": "quick", "cost_guardrail_usd": 0.0000001}"#;
    let fixture = Fixture::new("guard", MODEL, config, MANIFEST, &[ok_step(GOOD_PAYLOAD)]);
    let output = fixture.recorded();
    assert_eq!(status(&output), 3);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("estimated spend $0.000 exceeds the cost guardrail $0.00."),
        "{stderr}"
    );
    assert!(stderr.contains("--allow-spend"), "{stderr}");
    assert!(!text(&output.stdout).contains("Invoking Lambda"));
    assert!(!fixture.root.join("perf_summary.json").exists());
    let allowed = fixture.bench(&["--recorded", "recorded.json", "--allow-spend"]);
    assert_eq!(status(&allowed), 0, "{}", text(&allowed.stderr));
    assert!(fixture.root.join("perf_summary.json").exists());
}

#[test]
fn a_long_run_is_split_and_every_step_is_replayed_in_order() {
    let config = r#"{"aws_profile": "bench", "region": "us-east-1", "lambda_timeout_seconds": 100,
      "duration_seconds": 30, "warmup_seconds": 1, "table_settle_seconds": 1, "seed_items_per_table": 25}"#;
    let second = r#"{"invocation_index": 1, "phases_run": ["measure"],
      "raw_rows": [{"pattern_id": "GetOrder", "phase": "measure", "latency_ms": 8.0, "consumed_cu": 0.5}],
      "coverage": {"measured_patterns": ["GetOrder", "PutOrder"]},
      "measurement_tainted": {"PutOrder": "ramp:throttle_rate=0.30"},
      "exact_counts": [
        {"pattern_id": "GetOrder", "phase": "measure", "calls": 5, "throttles": 1, "errors": 0},
        {"pattern_id": "PutOrder", "phase": "measure", "calls": 4, "throttles": 2, "errors": 1, "error_codes": {"ValidationException": 1}}]}"#;
    let steps = vec![
        format!(
            r#"{{"service": "lambda", "operation": "Invoke",
              "request": {{"Payload": {{"invocation_index": 0, "invocations_total": 2,
                                       "phase_plan": ["settle", "seed", "warmup", "measure"]}}}},
              "response": {{"Payload": {GOOD_PAYLOAD}}}}}"#
        ),
        format!(
            r#"{{"service": "lambda", "operation": "Invoke",
              "request": {{"Payload": {{"invocation_index": 1, "phase_plan": ["measure"]}}}},
              "response": {{"Payload": {second}}}}}"#
        ),
    ];
    let fixture = Fixture::new("split", MODEL, config, MANIFEST, &steps);
    let output = fixture.recorded();
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("splitting measurement across 2 Lambda invocation(s)."),
        "{stdout}"
    );
    assert!(
        stdout.contains("Invoking Lambda (invocation 2/2, phases=['measure']) …"),
        "{stdout}"
    );
    let summary = fixture.summary();
    assert_eq!(at(&summary, &["total_rows"]), &Json::Int(7));
    let patterns = items(at(&summary, &["patterns"]));
    // Exact tallies add across invocations: 18 + 5 calls, 0 + 1 throttle.
    assert_eq!(
        at(&patterns[0], &["steady_state", "call_count"]),
        &Json::Int(23)
    );
    assert_eq!(
        at(&patterns[0], &["steady_state", "throttles"]),
        &Json::Int(1)
    );
    assert_eq!(
        at(&patterns[1], &["steady_state", "call_count"]),
        &Json::Int(22)
    );
    assert_eq!(at(&patterns[1], &["steady_state", "errors"]), &Json::Int(1));
    assert_eq!(
        at(&patterns[1], &["measurement_tainted"]),
        &Json::Bool(true)
    );
    assert_eq!(
        at(&patterns[1], &["tainted_reason"]),
        &Json::string("ramp:throttle_rate=0.30")
    );
    assert!(patterns[0].get("tainted_reason").is_none());
}

#[test]
fn a_recorded_request_that_differs_stops_the_run() {
    let steps = vec![format!(
        r#"{{"service": "lambda", "operation": "Invoke",
          "request": {{"Payload": {{"invocation_index": 9}}}},
          "response": {{"Payload": {GOOD_PAYLOAD}}}}}"#
    )];
    let fixture = Fixture::new("mismatch", MODEL, CONFIG, MANIFEST, &steps);
    let output = fixture.recorded();
    assert_eq!(status(&output), 2);
    assert!(
        text(&output.stderr).contains("differs from the request made"),
        "{}",
        text(&output.stderr)
    );
    assert!(!fixture.root.join("perf_summary.json").exists());
}

#[test]
fn recorded_steps_the_run_never_used_fail_it() {
    let steps = vec![ok_step(GOOD_PAYLOAD), ok_step(GOOD_PAYLOAD)];
    let fixture = Fixture::new("unused", MODEL, CONFIG, MANIFEST, &steps);
    let output = fixture.recorded();
    assert_eq!(status(&output), 2);
    assert!(
        text(&output.stderr).contains("the recorded document has 1 step(s) this run never used"),
        "{}",
        text(&output.stderr)
    );
    assert!(!fixture.root.join("perf_summary.json").exists());
}

#[test]
fn a_seed_failure_or_handler_error_stops_early_with_status_1_but_writes_the_outputs() {
    for (label, payload, needle) in [
        (
            "seedfail",
            r#"{"phases_run": ["settle", "seed"], "seed_verification_failed": true,
                "seed_verification": {"Orders": {"expected": 100, "actual": 0, "sampled": false, "passed": false, "seed_shortfall_ratio": 1.0}},
                "raw_rows": [{"pattern_id": "GetOrder", "phase": "seed", "latency_ms": 1.0}]}"#,
            "seed verification failed — aborting further invocations.",
        ),
        (
            "handlererr",
            r#"{"phases_run": ["settle"], "handler_error": "ClientError: AccessDenied for AKIAIOSFODNN7EXAMPLE"}"#,
            "Lambda handler error: ",
        ),
    ] {
        // The unused second step proves an early abort does not trip the unused-step check.
        let config = r#"{"aws_profile": "bench", "region": "us-east-1", "lambda_timeout_seconds": 100,
          "duration_seconds": 30, "warmup_seconds": 1, "table_settle_seconds": 1, "seed_items_per_table": 25}"#;
        let fixture = Fixture::new(
            label,
            MODEL,
            config,
            MANIFEST,
            &[ok_step(payload), ok_step("{}")],
        );
        let output = fixture.recorded();
        assert_eq!(status(&output), 1, "{label}: {}", text(&output.stderr));
        let stderr = text(&output.stderr);
        assert!(stderr.contains(needle), "{stderr}");
        assert!(
            stderr.contains("WARNING: coverage incomplete — missing: [\"GetOrder\", \"PutOrder\"]"),
            "{stderr}"
        );
        assert!(
            !stderr.contains("AKIAIOSFODNN7EXAMPLE"),
            "handler text is redacted: {stderr}"
        );
        let summary = fixture.summary();
        assert_eq!(
            at(&summary, &["coverage", "coverage_incomplete"]),
            &Json::Bool(true)
        );
        assert_eq!(
            items(at(&summary, &["coverage", "missing_patterns"])).len(),
            2
        );
        assert!(fixture.root.join("perf_raw.jsonl").exists());
    }
}

#[test]
fn missing_patterns_warn_on_stderr_but_exit_zero() {
    let payload = GOOD_PAYLOAD.replace(
        r#""measured_patterns": ["GetOrder", "PutOrder"]"#,
        r#""measured_patterns": ["GetOrder"]"#,
    );
    let fixture = Fixture::new("missing", MODEL, CONFIG, MANIFEST, &[ok_step(&payload)]);
    let output = fixture.recorded();
    assert_eq!(status(&output), 0);
    assert!(
        text(&output.stderr).contains("\nWARNING: coverage incomplete — missing: [\"PutOrder\"]"),
        "{}",
        text(&output.stderr)
    );
    let report = fixture.report(&fixture.read("perf_summary.json"));
    assert_eq!(status(&report), 0);
    assert!(
        fixture
            .read("report.md")
            .contains("Patterns with no measurement: PutOrder."),
        "{}",
        fixture.read("report.md")
    );
    assert!(
        text(&report.stdout).contains("(has design findings)"),
        "incomplete coverage is never a clean run"
    );
}

#[test]
fn lambda_failures_are_refused_with_redacted_text() {
    const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "nonjson",
            format!(
                r#"{{"service": "lambda", "operation": "Invoke", "response": {{"StatusCode": 502, "Payload": "oops {SECRET}\nforged: line"}}}}"#
            ),
            "Lambda returned non-JSON payload (status 502): ",
        ),
        (
            "functionerror",
            format!(
                r#"{{"service": "lambda", "operation": "Invoke", "response": {{"FunctionError": "Unhandled", "Payload": {{"errorMessage": "{SECRET}"}}}}}}"#
            ),
            "Lambda function error (\"Unhandled\"): ",
        ),
        (
            "clienterror",
            format!(
                r#"{{"service": "lambda", "operation": "Invoke", "error": {{"code": "ResourceNotFoundException", "message": "no {SECRET}"}}}}"#
            ),
            "lambda invoke failed (\"ResourceNotFoundException\"): ",
        ),
        (
            "nocreds",
            r#"{"service": "lambda", "operation": "Invoke", "error": {"kind": "no_credentials"}}"#
                .to_string(),
            "AWS credentials not found",
        ),
        (
            "nopayload",
            r#"{"service": "lambda", "operation": "Invoke", "response": {"StatusCode": 200}}"#
                .to_string(),
            "\"Payload\" is required",
        ),
        (
            "notobject",
            r#"{"service": "lambda", "operation": "Invoke", "response": {"Payload": []}}"#
                .to_string(),
            "\"Payload\" must be an object or a string",
        ),
    ];
    for (label, step, needle) in cases {
        let fixture = Fixture::new(label, MODEL, CONFIG, MANIFEST, &[step]);
        let output = fixture.recorded();
        assert_eq!(status(&output), 2, "{label}");
        let stderr = text(&output.stderr);
        assert!(stderr.contains(needle), "{label}: {stderr}");
        assert!(!stderr.contains(SECRET), "{label}: {stderr}");
        assert_eq!(
            stderr.lines().filter(|l| l.starts_with("error:")).count(),
            1,
            "{label}: {stderr}"
        );
        assert!(!fixture.root.join("perf_summary.json").exists(), "{label}");
    }
}

#[test]
fn a_malformed_lambda_response_names_the_member_not_the_value() {
    const SECRET: &str = "FAKE-RECORDED-SECRET-4390";
    for (label, payload, needle) in [
        (
            "nullrows",
            r#"{"raw_rows": null}"#.to_string(),
            "\"raw_rows\" must be a list when present (it is null)",
        ),
        (
            "badrow",
            format!(r#"{{"raw_rows": [{{"latency_ms": "{SECRET}"}}]}}"#),
            "raw_rows[0]: \"latency_ms\" must be a number or null",
        ),
        ("array", "[1]".to_string(), "must be a JSON object"),
    ] {
        let fixture = Fixture::new(label, MODEL, CONFIG, MANIFEST, &[ok_step(&payload)]);
        let output = fixture.recorded();
        assert_eq!(status(&output), 2, "{label}");
        let stderr = text(&output.stderr);
        assert!(stderr.contains(needle), "{label}: {stderr}");
        assert!(!stderr.contains(SECRET), "{label}: {stderr}");
    }
}

#[test]
fn an_invalid_design_config_or_manifest_is_refused_before_any_invocation() {
    let no_lambda = r#"{"account": "1"}"#;
    let cases: Vec<(&str, &str, &str, &str, &str)> = vec![
        (
            "nopatterns",
            r#"{"tables": [], "access_patterns": []}"#,
            CONFIG,
            MANIFEST,
            "access_patterns empty",
        ),
        (
            "zeropeak",
            r#"{"access_patterns": [{"pattern_id": "X", "peak_rps": 0}]}"#,
            CONFIG,
            MANIFEST,
            "has missing or zero peak_rps",
        ),
        (
            "undefinedtable",
            r#"{"tables": [], "access_patterns": [{"pattern_id": "X", "peak_rps": 1, "table": "Nope"}]}"#,
            CONFIG,
            MANIFEST,
            "not defined in tables[]",
        ),
        (
            "badmode",
            MODEL,
            r#"{"aws_profile": "p", "region": "r", "mode": "quik"}"#,
            MANIFEST,
            "unknown benchmark mode \"quik\"",
        ),
        (
            "noprofile",
            MODEL,
            r#"{"region": "r"}"#,
            MANIFEST,
            "needs a non-empty string `aws_profile`",
        ),
        (
            "nolambda",
            MODEL,
            CONFIG,
            no_lambda,
            "manifest has no 'lambda' block",
        ),
        (
            "badfunction",
            MODEL,
            CONFIG,
            r#"{"lambda": {"function_name": "a b;c"}}"#,
            "`lambda.function_name` must be a function name or ARN",
        ),
        (
            "hopeless",
            MODEL,
            r#"{"aws_profile": "p", "region": "r", "warmup_seconds": 1000}"#,
            MANIFEST,
            "Even a single Lambda invocation's settle+seed+warmup overhead exceeds",
        ),
        (
            "badnumber",
            MODEL,
            r#"{"aws_profile": "p", "region": "r", "duration_seconds": "x"}"#,
            MANIFEST,
            "`duration_seconds` must be an integer",
        ),
    ];
    for (label, model, config, manifest, needle) in cases {
        let fixture = Fixture::new(label, model, config, manifest, &[ok_step(GOOD_PAYLOAD)]);
        let output = fixture.recorded();
        assert_eq!(status(&output), 2, "{label}: {}", text(&output.stderr));
        let stderr = text(&output.stderr);
        assert!(stderr.contains(needle), "{label}: {stderr}");
        assert!(!text(&output.stdout).contains("Invoking Lambda"), "{label}");
        assert!(!fixture.root.join("perf_summary.json").exists(), "{label}");
    }
}

#[test]
fn bad_arguments_and_paths_are_usage_errors_or_refusals() {
    let fixture = Fixture::standard("args");
    // Missing required option.
    let output = toolkit(
        &["dynamodb", "benchmark-model", "--model", "model.json"],
        &fixture.root,
    );
    assert_eq!(status(&output), 2);
    assert!(
        text(&output.stderr).contains("the following arguments are required: --config"),
        "{}",
        text(&output.stderr)
    );
    // A bare `-` is never a stream.
    let dash = toolkit(
        &[
            "dynamodb",
            "benchmark-model",
            "--model",
            "-",
            "--config",
            "config.json",
            "--manifest",
            "manifest.json",
            "--raw-out",
            "r",
            "--summary-out",
            "s",
        ],
        &fixture.root,
    );
    assert_eq!(status(&dash), 2);
    assert!(!fixture.root.join("-").exists());
    let dash_report = toolkit(
        &[
            "dynamodb",
            "generate-perf-report",
            "--model",
            "model.json",
            "--summary",
            "-",
            "--output",
            "o",
        ],
        &fixture.root,
    );
    assert_eq!(status(&dash_report), 2);
    // Missing file, bad --now, bad recording.
    let missing = toolkit(
        &[
            "dynamodb",
            "benchmark-model",
            "--model",
            "absent.json",
            "--config",
            "config.json",
            "--manifest",
            "manifest.json",
            "--raw-out",
            "r",
            "--summary-out",
            "s",
        ],
        &fixture.root,
    );
    assert_eq!(status(&missing), 2);
    assert!(
        text(&missing.stderr)
            .contains("cannot read the design JSON absent.json: file does not exist"),
        "{}",
        text(&missing.stderr)
    );
    let bad_now = fixture.bench(&["--now", "yesterday"]);
    assert_eq!(status(&bad_now), 2);
    fs::write(
        fixture.root.join("recorded.json"),
        r#"{"calls": [{"service": "sts"}]}"#,
    )
    .expect("recording");
    let bad_recording = fixture.recorded();
    assert_eq!(status(&bad_recording), 2);
    assert!(
        text(&bad_recording.stderr)
            .contains("the recorded document recorded.json: recorded call 0"),
        "{}",
        text(&bad_recording.stderr)
    );
}

#[cfg(unix)]
#[test]
fn outputs_never_follow_a_symbolic_link() {
    let fixture = Fixture::standard("symlink");
    let target = fixture.root.join("victim.txt");
    fs::write(&target, "keep").expect("victim");
    std::os::unix::fs::symlink(&target, fixture.root.join("perf_summary.json")).expect("symlink");
    let output = fixture.recorded();
    assert_eq!(status(&output), 2);
    assert_eq!(fs::read_to_string(&target).expect("victim"), "keep");
}

// ---------------------------------------------------------------------------
// generate-perf-report with captured summaries
// ---------------------------------------------------------------------------

const GSI_MODEL: &str = r#"{
  "tables": [{"table_name": "Orders",
    "entities": [{"entity_name": "Order", "estimated_item_size_bytes": 1024,
                  "attributes": [{"name": "pk", "type": "S"}, {"name": "gsi1pk", "type": "S"}]}],
    "gsis": [{"index_name": "ByCustomer", "partition_key": "gsi1pk", "projection": {"type": "KEYS_ONLY"}}],
    "streams": {"enabled": true}}],
  "access_patterns": [
    {"pattern_id": "Get", "operation": "GetItem", "table": "Orders", "peak_rps": 100, "consistency": "strong"},
    {"pattern_id": "Put", "operation": "PutItem", "table": "Orders", "peak_rps": 50},
    {"pattern_id": "Txn", "operation": "TransactWriteItems", "table": "Orders", "peak_rps": 10},
    {"pattern_id": "Bad", "operation": "Query", "table": "Orders", "peak_rps": 5, "items_per_request": 1000, "estimated_item_size_bytes": 1024}]}"#;

fn pattern(pid: &str, steady: &str, cold: &str, extra: &str) -> String {
    format!(
        r#"{{"pattern_id": "{pid}", "bench_rps": 2.0, "steady_state": {steady}, "cold_start": {cold}{extra}}}"#
    )
}

const COLD_OK: &str = r#"{"warmup_call_count": 5, "warmup_p50_ms": 3.0, "warmup_p99_ms": 4.0, "warmup_throttles": 0, "cold_start_elevated": false}"#;

fn steady(cu: f64, extra: &str) -> String {
    format!(
        r#"{{"call_count": 100, "rows_sampled": 100, "p50_ms": 4.0, "p95_ms": 5.0, "p99_ms": 6.0, "mean_observed_cu": {cu},
            "gsi_cu_by_index": {{}}, "amplification_ratio": 0.0, "throttles": 0, "errors": 0, "error_rate": 0.0,
            "error_codes": {{}}, "cancellation_reason_codes": {{}}{extra}}}"#
    )
}

fn summary_of(patterns: &[String], extra: &str) -> String {
    format!(
        r#"{{"manifest": {{"account": "1", "region": "us-east-1", "prefix": "p-", "run_id": "r"}},
            "config": {{"duration_seconds": 15, "min_rps_per_pattern": 1}},
            "benchmark_completed_at": "2026-01-31T12:00:00+00:00", "benchmark_wall_seconds": 1.5, "total_rows": 9,
            "mode": "quick", "coverage": {{"measured_patterns": [], "missing_patterns": [], "coverage_incomplete": false}},
            "patterns": [{}]{extra}}}"#,
        patterns.join(",")
    )
}

#[test]
fn every_signal_becomes_a_classified_finding_with_the_script_axioms() {
    let fixture = Fixture::new("signals", GSI_MODEL, CONFIG, MANIFEST, &[]);
    let put_steady = steady(1.0, r#", "amplification_ratio_x": 0"#)
        .replace(
            r#""amplification_ratio": 0.0"#,
            r#""amplification_ratio": 1.0"#,
        )
        .replace(r#""throttles": 0"#, r#""throttles": 3"#)
        .replace(
            r#""gsi_cu_by_index": {}"#,
            r#""gsi_cu_by_index": {"ByCustomer": 1.0}"#,
        );
    let txn_steady = steady(2.0, "")
        .replace(r#""errors": 0"#, r#""errors": 90"#)
        .replace(r#""error_rate": 0.0"#, r#""error_rate": 0.9"#)
        .replace(
            r#""error_codes": {}"#,
            r#""error_codes": {"TransactionCanceledException": 90}"#,
        )
        .replace(
            r#""cancellation_reason_codes": {}"#,
            r#""cancellation_reason_codes": {"TransactionConflict": 85}"#,
        );
    let bad_steady = steady(0.0, "")
        .replace(r#""errors": 0"#, r#""errors": 100"#)
        .replace(r#""error_rate": 0.0"#, r#""error_rate": 1.0"#)
        .replace(
            r#""error_codes": {}"#,
            r#""error_codes": {"ValidationException": 100}"#,
        );
    let cold_bad = r#"{"warmup_call_count": 5, "warmup_p50_ms": 30.0, "warmup_p99_ms": 90.0, "warmup_throttles": 0, "cold_start_elevated": true}"#;
    let summary = summary_of(
        &[
            pattern("Get", &steady(0.5, ""), cold_bad, ""),
            pattern("Put", &put_steady, COLD_OK, ""),
            pattern("Txn", &txn_steady, COLD_OK, ""),
            pattern("Bad", &bad_steady, COLD_OK, ""),
        ],
        r#", "seed_verification": {"Orders": {"expected": 100, "actual": 3, "sampled": false, "passed": false, "seed_shortfall_ratio": 0.97}}"#,
    );
    let output = fixture.report(&summary);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let findings = json(&fixture.read("findings.json"));
    assert_eq!(
        at(&findings, &["no_significant_findings"]),
        &Json::Bool(false)
    );
    let classified = items(at(&findings, &["classified_findings"]));
    let signals: Vec<&str> = classified
        .iter()
        .map(|f| at(f, &["signal"]).as_str().expect("signal"))
        .collect();
    for expected in [
        "persistent_throttles",
        "high_gsi_amplification",
        "strong_read_overhead",
        "cold_start_elevated_patterns",
        "pattern_artifact_error_rate",
        "pattern_high_error_rate",
        "seed_shortfall",
    ] {
        assert!(signals.contains(&expected), "{expected} in {signals:?}");
    }
    let by = |signal: &str| {
        classified
            .iter()
            .find(|f| at(f, &["signal"]).as_str() == Some(signal))
            .unwrap_or_else(|| panic!("{signal}"))
    };
    assert_eq!(
        at(by("persistent_throttles"), &["severity"]),
        &Json::string("high")
    );
    assert_eq!(
        at(by("high_gsi_amplification"), &["severity"]),
        &Json::string("high")
    );
    assert_eq!(
        at(by("high_gsi_amplification"), &["evidence", "projection"]),
        &Json::string("KEYS_ONLY")
    );
    assert_eq!(
        at(by("pattern_artifact_error_rate"), &["severity"]),
        &Json::string("medium")
    );
    assert_eq!(
        at(
            by("pattern_artifact_error_rate"),
            &["evidence", "error_kind"]
        ),
        &Json::string("artifact")
    );
    assert_eq!(
        at(by("pattern_high_error_rate"), &["severity"]),
        &Json::string("high")
    );
    assert_eq!(
        at(by("seed_shortfall"), &["id"]),
        &Json::string("seed-Orders")
    );
    assert_eq!(
        at(by("seed_shortfall"), &["evidence", "actual"]),
        &Json::Int(3)
    );
    assert!(at(
        by("pattern_high_error_rate"),
        &["evidence", "interpretation"]
    )
    .as_str()
    .expect("text")
    .starts_with("Most/all calls for this pattern FAILED"));
    // The finding ids count up in signal order.
    assert_eq!(at(&classified[0], &["id"]), &Json::string("finding-1"));

    let report = fixture.read("report.md");
    assert!(
        report.contains("## Correctness (operation errors)"),
        "{report}"
    );
    assert!(
        report.contains("**These patterns FAILED on most/all calls (non-throttle errors).**"),
        "{report}"
    );
    assert!(
        report.contains("| Bad | Query | 100% | `ValidationException` | 100/100 |"),
        "{report}"
    );
    assert!(
        report.contains("**Benchmark artifact — NOT a design defect.**"),
        "{report}"
    );
    assert!(
        report.contains(
            "- Txn cancellation reasons (per-item, observed): `TransactionConflict` ×85."
        ),
        "{report}"
    );
    assert!(
        report.contains("| 100 (100%) *") || report.contains("100 (100%) *"),
        "{report}"
    );
    assert!(report.contains("- Stream on table `Orders`: configured in design; consumers (Lambda / EventBridge Pipe / Kinesis) not deployed."), "{report}");
    assert!(report.contains("- **seed_shortfall** on table `Orders`: severity=high. Seeded 3 of 100 expected items."), "{report}");
    assert!(report.contains("## Iteration offer"), "{report}");
    assert!(
        report.contains("| Orders | 100 | 3 | SHORTFALL |")
            || report.contains("| `Orders` | 100 | 3 | SHORTFALL |"),
        "{report}"
    );
    assert!(
        report.contains("2 pattern(s) excluded") || report.contains("pattern(s) excluded here"),
        "{report}"
    );
}

#[test]
fn representative_runs_split_skew_from_starvation() {
    let model = r#"{
      "tables": [{"table_name": "T", "entities": [{"entity_name": "E", "estimated_item_size_bytes": 1024, "attributes": []}]}],
      "access_patterns": [
        {"pattern_id": "Hot", "operation": "GetItem", "table": "T", "peak_rps": 100},
        {"pattern_id": "Starved", "operation": "GetItem", "table": "T", "peak_rps": 100},
        {"pattern_id": "Calm", "operation": "GetItem", "table": "T", "peak_rps": 100}]}"#;
    let fixture = Fixture::new("rep", model, CONFIG, MANIFEST, &[]);
    let kd = |hot: f64, cold: f64| {
        format!(
            r#", "key_distribution": {{"n_distinct_keys": 50, "top_key_share": 0.4, "stddev_over_mean": 2.0,
                                      "hot_partition_p99_ms": {hot}, "cold_partition_p99_ms": {cold}}}"#
        )
    };
    let throttled =
        |extra: &str| steady(0.5, extra).replace(r#""throttles": 0"#, r#""throttles": 9"#);
    let summary = summary_of(
        &[
            pattern("Hot", &throttled(&kd(30.0, 10.0)), COLD_OK, ""),
            pattern("Starved", &throttled(&kd(10.0, 10.0)), COLD_OK, ""),
            pattern("Calm", &steady(0.5, &kd(30.0, 10.0)), COLD_OK, ""),
        ],
        "",
    )
    .replace(r#""mode": "quick""#, r#""mode": "representative""#);
    let output = fixture.report(&summary);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let report = fixture.read("report.md");
    assert!(
        report.contains("> **Representative mode:** this run drove ~0.15× declared peak"),
        "{report}"
    );
    assert!(
        report.contains("## Load-risk signals (representative mode only)"),
        "{report}"
    );
    assert!(
        report.contains("| Hot | 9 | 6.0 | 0.00× | 40% | 50 |"),
        "{report}"
    );
    assert!(
        report.contains("Write-shard the partition key (hash suffix) or re-aggregate: Hot."),
        "{report}"
    );
    assert!(
        report.contains("**uniform capacity starvation, NOT key skew**"),
        "{report}"
    );
    assert!(report.contains("throttles: Starved."), "{report}");
    assert!(report.contains("Watch: Calm."), "{report}");
    assert!(
        report.contains("PROBED under zipf skew at bounded scale"),
        "{report}"
    );
    let findings = json(&fixture.read("findings.json"));
    let classified = items(at(&findings, &["classified_findings"]));
    let skew: Vec<&Json> = classified
        .iter()
        .filter(|f| at(f, &["signal"]).as_str() == Some("key_skew_patterns"))
        .collect();
    assert_eq!(skew.len(), 3);
    assert_eq!(
        at(skew[2], &["evidence", "evidence_kind"]),
        &Json::string("elevated_latency")
    );
    assert_eq!(at(skew[2], &["severity"]), &Json::string("medium"));
    assert_eq!(at(skew[0], &["severity"]), &Json::string("high"));
}

#[test]
fn vector_capacity_is_reported_from_the_measured_bytes() {
    let model = r#"{
      "tables": [{"table_name": "T", "entities": [{"entity_name": "E", "estimated_item_size_bytes": 100, "attributes": []}],
        "vector_indexes": [{"index_name": "V", "vector_attribute": "vec", "dimensions": 8}]}],
      "access_patterns": [
        {"pattern_id": "Search", "operation": "SearchVectors", "table": "T", "index": "V", "peak_rps": 10}]}"#;
    let fixture = Fixture::new("vec", model, CONFIG, MANIFEST, &[]);
    let steady = steady(
        0.0,
        r#", "mean_vector_search_bytes": 2048.0, "mean_vector_write_bytes_by_index": {"V": 512.0}"#,
    );
    let summary = summary_of(&[pattern("Search", &steady, COLD_OK, "")], "");
    let output = fixture.report(&summary);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let report = fixture.read("report.md");
    assert!(report.contains("## Vector capacity (measured)"), "{report}");
    assert!(
        report.contains("| Search | SearchVectors | V | 2,048 | 512 | 10 |"),
        "{report}"
    );
    assert!(
        report.contains("At least one figure is at or below the 1,024-byte metering minimum"),
        "{report}"
    );
    assert!(report.contains("(~256 dimensions)"), "{report}");
    assert!(
        report.contains("Search is billed at $0.002/GB and writes at $0.52/GB."),
        "{report}"
    );
}

#[test]
fn hostile_names_cannot_forge_report_lines_or_reach_the_terminal() {
    let hostile = "Evil\\u001b[31m\\nFORGED: injected";
    let model = format!(
        r#"{{"tables": [{{"table_name": "T", "entities": [{{"entity_name": "E", "estimated_item_size_bytes": 1024, "attributes": []}}]}}],
          "access_patterns": [{{"pattern_id": "{hostile}", "operation": "GetItem", "table": "T", "peak_rps": 100}}]}}"#
    );
    let fixture = Fixture::new("hostile", &model, CONFIG, MANIFEST, &[]);
    let summary = summary_of(&[pattern(hostile, &steady(0.5, ""), COLD_OK, "")], "");
    let output = fixture.report(&summary);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let report = fixture.read("report.md");
    assert!(!report.contains('\u{1b}'), "{report:?}");
    assert!(
        !report.lines().any(|line| line.starts_with("FORGED")),
        "{report}"
    );
    assert!(
        report.contains("Evil\\x1b[31m\\nFORGED: injected") || report.contains("FORGED: injected"),
        "{report}"
    );
}

#[test]
fn malformed_report_inputs_are_refused_with_status_2() {
    let fixture = Fixture::new("badreport", MODEL, CONFIG, MANIFEST, &[]);
    for (label, summary, needle) in [
        (
            "notobject",
            "[]".to_string(),
            "the summary JSON must be an object",
        ),
        (
            "missingsteady",
            r#"{"patterns": [{"pattern_id": "GetOrder", "bench_rps": 1.0, "cold_start": {}}]}"#
                .to_string(),
            "is missing `steady_state`",
        ),
        (
            "badnumber",
            summary_of(
                &[pattern(
                    "GetOrder",
                    &steady(0.5, "").replace(r#""throttles": 0"#, r#""throttles": "x""#),
                    COLD_OK,
                    "",
                )],
                "",
            ),
            "`throttles` must be an integer",
        ),
        ("badjson", "{".to_string(), "is not valid JSON"),
    ] {
        let output = fixture.report(&summary);
        assert_eq!(status(&output), 2, "{label}");
        assert!(
            text(&output.stderr).contains(needle),
            "{label}: {}",
            text(&output.stderr)
        );
        assert!(!fixture.root.join("report.md").exists(), "{label}");
    }
}

#[test]
fn the_report_default_findings_path_is_design_findings_json() {
    let fixture = Fixture::standard("defaultfindings");
    assert_eq!(status(&fixture.recorded()), 0);
    let output = toolkit(
        &[
            "dynamodb",
            "generate-perf-report",
            "--model",
            "model.json",
            "--summary",
            "perf_summary.json",
            "--output",
            "r.md",
        ],
        &fixture.root,
    );
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("Findings: design_findings.json"));
    assert!(fixture.root.join("design_findings.json").exists());
}
