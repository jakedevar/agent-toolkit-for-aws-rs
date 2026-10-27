//! Offline tests for the design-iteration port. Pipeline steps that would
//! touch AWS are replaced by a recording fake environment; the calculator
//! step runs for real through the shared command table.

use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, sha256_hex, ErrorKind, Json, ToolkitError};

use super::iterate::{
    apply_change, compute_deltas, dispatch_step, resolve_config, run_iterate_round,
    schema_fingerprint, utc_iso, IterateArgs, RoundEnv,
};
use super::test_support::{json, TempDir};
use crate::Output;

const MODEL: &str = r#"{
    "tables": [{"table_name": "T", "key_schema": {"partition_key": "pk"},
        "entities": [{"entity_name": "E", "attributes": [{"name": "pk"}]}]}],
    "access_patterns": [{"pattern_id": "Get", "operation": "GetItem", "table": "T",
                         "peak_rps": 100, "estimated_item_size_bytes": 1024}]
}"#;

const BANNER: &str = "========================================================================";

// ---------------------------------------------------------------------------
// A recording environment.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeEnv {
    steps: Vec<Vec<String>>,
    fail_on: Option<&'static str>,
    summary: String,
    findings: String,
    deploy_config: Option<String>,
    bench_config: Option<String>,
}

fn arg_value(argv: &[String], name: &str) -> PathBuf {
    let position = argv.iter().position(|word| word == name).expect(name);
    PathBuf::from(&argv[position + 1])
}

impl RoundEnv for FakeEnv {
    fn now_iso(&mut self) -> String {
        "2026-01-02T03:04:05Z".to_string()
    }

    fn new_loop_id(&mut self) -> String {
        "abcd1234".to_string()
    }

    fn run_step(&mut self, argv: &[String], out: &mut Output<'_>) -> Result<(), ToolkitError> {
        self.steps.push(argv.to_vec());
        if self.fail_on == Some(argv[1].as_str()) {
            return Err(ToolkitError::runtime("boom_code", "boom").with_exit_code(3));
        }
        match argv[1].as_str() {
            "calculate-costs" => dispatch_step(argv, out),
            "deploy-model" => {
                let config = fs::read_to_string(arg_value(argv, "--config")).expect("config");
                let dry = config.contains("\"dry_run\": true");
                self.deploy_config = Some(config);
                if !dry {
                    fs::write(
                        arg_value(argv, "--manifest-out"),
                        r#"{"prefix": "p-new", "lambda": {"name": "fn"}}"#,
                    )
                    .expect("manifest");
                }
                Ok(())
            }
            "benchmark-model" => {
                self.bench_config =
                    Some(fs::read_to_string(arg_value(argv, "--config")).expect("config"));
                fs::write(arg_value(argv, "--summary-out"), &self.summary).expect("summary");
                Ok(())
            }
            "generate-perf-report" => {
                fs::write(arg_value(argv, "--findings-out"), &self.findings).expect("findings");
                Ok(())
            }
            other => panic!("unexpected step {other}"),
        }
    }
}

struct Run {
    result: Result<(), ToolkitError>,
    stdout: String,
    diagnostics: Vec<String>,
}

fn run(args: &IterateArgs, env: &mut FakeEnv) -> Run {
    let mut buffer: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let result = {
        let mut out = Output::with_diagnostics(&mut buffer, &mut stderr);
        let result = run_iterate_round(args, env, &mut out);
        let _ = out.finish();
        result
    };
    Run {
        result,
        stdout: String::from_utf8(buffer).expect("utf-8 stdout"),
        diagnostics: String::from_utf8(stderr)
            .expect("utf-8 stderr")
            .lines()
            .map(ToString::to_string)
            .collect(),
    }
}

fn args_for(dir: &Path) -> IterateArgs {
    IterateArgs {
        model: dir.join("model.json"),
        config: dir.join("config.json"),
        loop_state: dir.join("loop_state.json"),
        manifest: dir.join("created_resources.json"),
        apply_change: None,
        mode: "representative".to_string(),
        timestamp: None,
        yes_deploy: false,
        allow_spend: false,
        skip_deploy: false,
        calculator_only: false,
        dry_run: false,
    }
}

fn workspace(label: &str) -> (TempDir, IterateArgs) {
    let dir = TempDir::new(label);
    fs::write(dir.join("model.json"), MODEL).expect("model");
    fs::write(dir.join("config.json"), "{}").expect("config");
    let args = args_for(&dir);
    (dir, args)
}

fn read_json(path: &Path) -> Json {
    parse_json(&fs::read_to_string(path).expect("read")).expect("valid json")
}

fn failure_of(run: &Run) -> &ToolkitError {
    run.result.as_ref().expect_err("the round must fail")
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

#[test]
fn utc_iso_formats_epoch_seconds() {
    assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_iso(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(utc_iso(951_782_400), "2000-02-29T00:00:00Z", "leap day");
    assert_eq!(utc_iso(951_868_800), "2000-03-01T00:00:00Z");
    assert_eq!(utc_iso(-1), "1969-12-31T23:59:59Z");
    assert_eq!(utc_iso(4_102_444_799), "2099-12-31T23:59:59Z");
}

#[test]
fn mode_presets_fill_only_missing_values() {
    let representative = resolve_config(&json(r#"{"mode":"representative","duration_seconds":7}"#));
    assert_eq!(
        representative.get("duration_seconds"),
        Some(&Json::Int(7)),
        "explicit wins"
    );
    assert_eq!(representative.get("warmup_seconds"), Some(&Json::Int(15)));
    assert_eq!(representative.get("scale_factor"), Some(&Json::Float(0.15)));
    assert_eq!(representative.get("zipf_s"), Some(&Json::Float(1.1)));
    assert_eq!(
        representative.get("read_pattern_key_sampling"),
        Some(&Json::string("zipf"))
    );
    // User keys keep their position; preset keys follow in preset order.
    let Json::Object(pairs) = &representative else {
        panic!("object expected");
    };
    assert_eq!(pairs[0].0, "mode");
    assert_eq!(pairs[1].0, "duration_seconds");
    assert_eq!(pairs[2].0, "table_settle_seconds");
    let quick = resolve_config(&json(r#"{"mode":"quick"}"#));
    assert_eq!(quick.get("seed_items_per_table"), Some(&Json::Int(100)));
    assert_eq!(quick.get("scale_factor"), None, "quick has no scale factor");
    // standard, a missing/null mode and an unknown mode are left alone.
    for source in [
        r#"{"mode":"standard"}"#,
        r"{}",
        r#"{"mode":null}"#,
        r#"{"mode":"quik"}"#,
        r#"{"mode":7}"#,
    ] {
        let config = json(source);
        assert_eq!(resolve_config(&config), config, "{source}");
    }
}

#[test]
fn merge_patches_deep_merge_and_null_removes_keys() {
    let model = json(r#"{"a":{"b":1,"c":2},"keep":true,"list":[1,2]}"#);
    let merged = apply_change(
        model,
        &json(r#"{"merge":{"a":{"b":9,"c":null,"d":{"x":null,"y":1}},"list":[3],"new":"v","keep":null}}"#),
    )
    .expect("merge");
    // `c` and `keep` are removed; a new nested object keeps its inner null
    // (the Python source only strips nulls where a base key already exists);
    // lists are replaced, not merged; new keys are appended.
    assert_eq!(
        merged,
        json(r#"{"a":{"b":9,"d":{"x":null,"y":1}},"list":[3],"new":"v"}"#)
    );
}

fn ops(model: &str, change: &str) -> Result<Json, ToolkitError> {
    apply_change(json(model), &json(change))
}

#[test]
fn op_lists_add_replace_and_remove_by_json_pointer() {
    let model = r#"{"tables":[{"gsis":[{"projection":{"type":"ALL"}},{"projection":{"type":"KEYS_ONLY"}}]}],"n":1}"#;
    let replaced = ops(
        model,
        r#"{"ops":[{"op":"replace","path":"/tables/0/gsis/1/projection/type","value":"INCLUDE"}]}"#,
    )
    .expect("replace");
    assert_eq!(
        replaced.get("tables"),
        Some(&json(
            r#"[{"gsis":[{"projection":{"type":"ALL"}},{"projection":{"type":"INCLUDE"}}]}]"#
        ))
    );
    // add: dict key, list insert (position clamps), '-' append, negative index.
    let added = ops(
        r#"{"o":{},"l":[1,2,3]}"#,
        r#"{"ops":[{"op":"add","path":"/o/k","value":1},
                   {"op":"add","path":"/l/1","value":"ins"},
                   {"op":"add","path":"/l/-","value":"end"},
                   {"op":"add","path":"/l/99","value":"clamped"},
                   {"op":"add","path":"/l/-100","value":"front"}]}"#,
    )
    .expect("add");
    assert_eq!(
        added,
        json(r#"{"o":{"k":1},"l":["front",1,"ins",2,3,"end","clamped"]}"#)
    );
    // remove: list index (negative counts from the end), dict key; a missing
    // dict key is silently fine.
    let removed = ops(
        r#"{"l":[1,2,3],"o":{"a":1}}"#,
        r#"{"ops":[{"op":"remove","path":"/l/-1"},{"op":"remove","path":"/o/a"},{"op":"remove","path":"/o/missing"}]}"#,
    )
    .expect("remove");
    assert_eq!(removed, json(r#"{"l":[1,2],"o":{}}"#));
    // Replacing at a negative index and replacing a dict value.
    let negative = ops(
        r#"{"l":[1,2,3],"o":{"a":1}}"#,
        r#"{"ops":[{"op":"replace","path":"/l/-2","value":"x"},{"op":"replace","path":"/o/a","value":2}]}"#,
    )
    .expect("replace");
    assert_eq!(negative, json(r#"{"l":[1,"x",3],"o":{"a":2}}"#));
}

#[test]
fn refused_changes_keep_the_published_exit_status() {
    // `_die` paths exit 2.
    for (change, expected) in [
        (r#"{"foo":1,"bar":2}"#, "Got keys: foo, bar. In the --apply-change file, each entry in \"ops\" needs"),
        (r"{}", "Got keys: <none>. "),
        (
            r#"{"ops":[{"op":"replace","path":"","value":1}]}"#,
            "--apply-change op is missing a non-empty \"path\": {'op': 'replace', 'path': '', 'value': 1}. ",
        ),
        (
            r#"{"ops":[{"op":"replace","path":"/l/-","value":1}]}"#,
            "a trailing '-' in path '/l/-' appends to a list and is only valid for op \"add\", not 'replace'.",
        ),
        (
            r#"{"ops":[{"op":"move","path":"/l/0","value":1}]}"#,
            "unsupported op 'move' at path '/l/0'.",
        ),
        (
            r#"{"ops":[{"op":"move","path":"/o/x","value":1}]}"#,
            "unsupported op 'move' at path '/o/x'.",
        ),
        (
            r#"{"ops":[{"path":"/o/x","value":1}]}"#,
            "unsupported op None at path '/o/x'.",
        ),
    ] {
        let error = ops(r#"{"l":[1],"o":{}}"#, change).expect_err(change);
        assert_eq!(error.exit_code(), 2, "{change}");
        assert!(error.message().contains(expected), "{change}: {error}");
    }
    // Tracebacks in the Python source were exit 1: bad pointer walks,
    // non-numeric / out-of-range indices and missing values.
    for change in [
        r#"{"ops":[{"op":"replace","path":"/o/a/b","value":1}]}"#,
        r#"{"ops":[{"op":"replace","path":"/l/x","value":1}]}"#,
        r#"{"ops":[{"op":"replace","path":"/l/5","value":1}]}"#,
        r#"{"ops":[{"op":"remove","path":"/l/5"}]}"#,
        r#"{"ops":[{"op":"add","path":"/o/k"}]}"#,
        r#"{"ops":[{"op":"replace","path":"/l/0/deeper/x","value":1}]}"#,
        r#"{"ops":["not-an-object"]}"#,
        r#"{"ops":[{"op":"add","path":7,"value":1}]}"#,
        r#"{"ops":"nope"}"#,
        r"[1,2]",
    ] {
        let error = ops(r#"{"l":[1],"o":{}}"#, change).expect_err(change);
        assert_eq!(error.exit_code(), 1, "{change}");
        assert_eq!(error.kind(), ErrorKind::Runtime);
    }
}

#[test]
fn fingerprint_ignores_volume_and_tracks_physical_schema() {
    let base = r#"{"tables":[{"table_name":"T","key_schema":{"partition_key":"pk","sort_key":"sk"},
        "gsis":[{"index_name":"G","partition_key":"gk","projection":{"type":"include","attributes":["b","a"]}}],
        "streams":{"enabled":true,"view_type":"NEW_IMAGE"}}],
        "access_patterns":[{"pattern_id":"P","operation":"GetItem","peak_rps":1}]}"#;
    // The hashed blob is exactly Python's
    // json.dumps(sig, sort_keys=True): ", " / ": " separators, upper-cased
    // projection type, sorted attributes.
    let blob =
        r#"[["T", "pk", "sk", [["G", "gk", null, "INCLUDE", ["a", "b"]]], true, "NEW_IMAGE"]]"#;
    let expected = sha256_hex(blob.as_bytes())[..12].to_string();
    let fingerprint = schema_fingerprint(&json(base)).expect("fingerprint");
    assert_eq!(fingerprint, expected);
    assert_eq!(fingerprint.len(), 12);
    assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));

    // Request volume, item size and consistency do not change it.
    let busier = base.replace(
        r#""peak_rps":1"#,
        r#""peak_rps":9000,"estimated_item_size_bytes":99"#,
    );
    assert_eq!(
        schema_fingerprint(&json(&busier)).expect("fingerprint"),
        fingerprint
    );
    // Attribute order and projection-type case do not matter either.
    let reordered = base
        .replace(r#"["b","a"]"#, r#"["a","b"]"#)
        .replace("include", "INCLUDE");
    assert_eq!(
        schema_fingerprint(&json(&reordered)).expect("fingerprint"),
        fingerprint
    );

    // Everything physical does.
    for (from, to) in [
        (r#""partition_key":"gk""#, r#""partition_key":"other""#),
        (r#""attributes":["b","a"]"#, r#""attributes":["b","a","c"]"#),
        (r#""type":"include""#, r#""type":"ALL""#),
        (r#""enabled":true"#, r#""enabled":false"#),
        (r#""view_type":"NEW_IMAGE""#, r#""view_type":"OLD_IMAGE""#),
        (r#""sort_key":"sk""#, r#""sort_key":"sk2""#),
        (r#""table_name":"T""#, r#""table_name":"T2""#),
        (r#""index_name":"G""#, r#""index_name":"G2""#),
    ] {
        assert!(base.contains(from), "{from}");
        let changed = schema_fingerprint(&json(&base.replace(from, to))).expect("fingerprint");
        assert_ne!(changed, fingerprint, "{from} -> {to}");
    }
}

#[test]
fn fingerprint_defaults_and_table_order() {
    let bare =
        schema_fingerprint(&json(r#"{"tables":[{"table_name":"T"}]}"#)).expect("fingerprint");
    assert_eq!(
        bare,
        sha256_hex(br#"[["T", null, null, [], false, null]]"#)[..12]
    );
    let empty = schema_fingerprint(&json(r"{}")).expect("fingerprint");
    assert_eq!(empty, sha256_hex(b"[]")[..12]);
    // Tables are sorted by name, so declaration order is irrelevant.
    let one = schema_fingerprint(&json(
        r#"{"tables":[{"table_name":"A"},{"table_name":"B"}]}"#,
    ))
    .expect("fingerprint");
    let two = schema_fingerprint(&json(
        r#"{"tables":[{"table_name":"B"},{"table_name":"A"}]}"#,
    ))
    .expect("fingerprint");
    assert_eq!(one, two);
    // Non-string names cannot be ordered: a model error, not a panic.
    let error = schema_fingerprint(&json(r#"{"tables":[{"table_name":5}]}"#)).expect_err("type");
    assert!(
        error.message().contains("`table_name` must be a string"),
        "{error}"
    );
}

#[test]
fn deltas_compare_headlines_with_sorted_keys() {
    let none = compute_deltas(&json("{}"), None).expect("deltas");
    let defaults = json(
        r#"{"monthly_usd_pct":null,"throttle_delta":{},"p99_delta_ms":{},"gsi_amp_delta":null}"#,
    );
    assert_eq!(none, defaults);
    assert_eq!(
        compute_deltas(&json("{}"), Some(&json("{}"))).expect("deltas"),
        defaults
    );

    let prev = json(
        r#"{"extrapolated_monthly_usd":100,"hot_pattern_throttles":{"a":1,"c":2},
            "p99_ms_by_pattern":{"a":8.0,"gone":1.0},"max_gsi_amplification":1.5}"#,
    );
    let cur = json(
        r#"{"extrapolated_monthly_usd":150,"hot_pattern_throttles":{"b":1,"a":3},
            "p99_ms_by_pattern":{"a":10.5,"new":3.0},"max_gsi_amplification":2.0}"#,
    );
    let deltas = compute_deltas(&cur, Some(&prev)).expect("deltas");
    assert_eq!(
        deltas,
        json(
            r#"{"monthly_usd_pct":50.0,"throttle_delta":{"a":2,"b":1,"c":-2},
                "p99_delta_ms":{"a":2.5},"gsi_amp_delta":0.5}"#
        )
    );
    // Percent change rounds to one decimal: (130 - 120) / 120 * 100 = 8.33.
    let rounded = compute_deltas(
        &json(r#"{"extrapolated_monthly_usd":130}"#),
        Some(&json(r#"{"extrapolated_monthly_usd":120}"#)),
    )
    .expect("deltas");
    assert_eq!(rounded.get("monthly_usd_pct"), Some(&Json::Float(8.3)));
    // A zero or missing baseline, or a missing current value, gives null.
    for (before, now) in [
        (
            r#"{"extrapolated_monthly_usd":0}"#,
            r#"{"extrapolated_monthly_usd":5}"#,
        ),
        (
            r#"{"extrapolated_monthly_usd":null}"#,
            r#"{"extrapolated_monthly_usd":5}"#,
        ),
        (
            r#"{"extrapolated_monthly_usd":5}"#,
            r#"{"extrapolated_monthly_usd":null}"#,
        ),
        (r#"{"extrapolated_monthly_usd":5}"#, r"{}"),
    ] {
        let deltas = compute_deltas(&json(now), Some(&json(before))).expect("deltas");
        assert_eq!(
            deltas.get("monthly_usd_pct"),
            Some(&Json::Null),
            "{before} -> {now}"
        );
    }
    let error = compute_deltas(
        &json(r#"{"hot_pattern_throttles":{"a":"many"}}"#),
        Some(&json(r#"{"hot_pattern_throttles":{"a":1}}"#)),
    )
    .expect_err("non-numeric throttles");
    assert_eq!(error.exit_code(), 1);
}

// ---------------------------------------------------------------------------
// Rounds.
// ---------------------------------------------------------------------------

#[test]
fn calculator_only_round_recosts_records_and_stops() {
    let (dir, mut args) = workspace("calc-only");
    args.calculator_only = true;
    let mut env = FakeEnv::default();
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("round succeeds");
    assert!(outcome.diagnostics.is_empty());

    let workdir = fs::canonicalize(&*dir).expect("canonical dir");
    let fingerprint = schema_fingerprint(&json(MODEL)).expect("fingerprint");
    let model_path = args.model.display();
    let report_path = workdir.join("cost_report.md");
    let expected = format!(
        "[iterate_design] round 0: schema_fingerprint={fingerprint} (prior=None) -> decision=calculator-only\n\
         \n\
         [iterate_design] calculate_costs (calculator-only): toolkit dynamodb calculate-costs --model {model_path} --output {report}\n\
         Cost report written to {report}\n\
         Analyzed 1 access patterns across 1 tables.\n\
         \n\
         {BANNER}\n\
         ROUND 0 SUMMARY\n\
         {BANNER}\n\
         \x20 decision: calculator-only\n\
         \x20 calculator monthly: $16.20\n\
         \x20 design-finding signals: none (clean)\n\
         \x20 artifacts: design_findings.json, cost_report.md\n\
         {BANNER}\n\
         === ROUND 0 COMPLETE — handing back to user. The loop does not self-iterate. Review the findings, decide a change, and re-run for the next round. ===\n",
        report = report_path.display()
    );
    assert_eq!(outcome.stdout, expected);

    // Only the calculator ran; the report exists beside the model.
    assert_eq!(env.steps.len(), 1);
    assert!(fs::read_to_string(&report_path)
        .expect("report")
        .starts_with("# DynamoDB Cost Report\n"));
    assert!(
        !workdir.join(".iterate_config.json").exists(),
        "no overlay on this path"
    );

    let state = read_json(&args.loop_state);
    assert_eq!(state.get("loop_id"), Some(&Json::string("abcd1234")));
    assert_eq!(
        state.get("model_path"),
        Some(&Json::string(args.model.display().to_string()))
    );
    assert_eq!(
        state.get("created_at"),
        Some(&Json::string("2026-01-02T03:04:05Z"))
    );
    assert_eq!(
        state.get("current_schema_fingerprint"),
        Some(&Json::string(fingerprint.as_str()))
    );
    assert_eq!(state.get("active_manifest"), Some(&Json::Null));
    let Some(Json::Array(rounds)) = state.get("rounds") else {
        panic!("rounds list");
    };
    assert_eq!(rounds.len(), 1);
    let entry = &rounds[0];
    assert_eq!(entry.get("round"), Some(&Json::Int(0)));
    assert_eq!(
        entry.get("timestamp"),
        Some(&Json::string("2026-01-02T03:04:05Z"))
    );
    assert_eq!(entry.get("mode"), Some(&Json::string("representative")));
    assert_eq!(
        entry.get("scale_factor"),
        Some(&Json::Float(0.15)),
        "resolved preset value"
    );
    assert_eq!(entry.get("applied_diff"), Some(&Json::Null));
    assert_eq!(
        entry.get("deploy_decision"),
        Some(&Json::string("calculator-only"))
    );
    assert_eq!(entry.get("finding_signals"), Some(&json("[]")));
    assert_eq!(entry.get("user_decision"), Some(&Json::Null));
    assert_eq!(
        entry.get("headline"),
        Some(&json(
            r#"{"extrapolated_monthly_usd":null,"calculator_monthly_usd":16.2,"hot_pattern_throttles":{},
                "p99_ms_by_pattern":{},"max_gsi_amplification":null,"top_key_share_by_pattern":{}}"#
        ))
    );
    assert_eq!(
        entry.get("delta_vs_prev"),
        Some(&json(
            r#"{"monthly_usd_pct":null,"throttle_delta":{},"p99_delta_ms":{},"gsi_amp_delta":null}"#
        ))
    );
}

#[test]
fn a_second_round_applies_the_change_and_reports_the_delta() {
    let (dir, mut args) = workspace("round-two");
    args.calculator_only = true;
    args.timestamp = Some("2026-02-03T00:00:00Z".to_string());
    let mut env = FakeEnv::default();
    run(&args, &mut env).result.expect("round 0");
    let first = read_json(&args.loop_state);
    let fingerprint = first
        .get("current_schema_fingerprint")
        .cloned()
        .expect("fingerprint");

    // Doubling the traffic changes the cost but not the physical schema.
    let change = dir.join("change.json");
    fs::write(
        &change,
        r#"{"ops":[{"op":"replace","path":"/access_patterns/0/peak_rps","value":200}]}"#,
    )
    .expect("change");
    args.apply_change = Some(change);
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("round 1");
    assert!(
        outcome
            .stdout
            .starts_with("[iterate_design] applied change to model.json\n"),
        "{}",
        outcome.stdout
    );
    assert!(outcome.stdout.contains("-> decision=calculator-only"));
    assert!(outcome
        .stdout
        .contains(&format!("(prior={})", fingerprint.as_str().expect("text"))));
    assert!(outcome.stdout.contains("ROUND 1 SUMMARY"));
    assert!(outcome.stdout.contains("  calculator monthly: $32.40\n"));
    assert!(outcome.stdout.contains(
        "  delta vs round 0: {\"monthly_usd_pct\": null, \"throttle_delta\": {}, \"p99_delta_ms\": {}, \"gsi_amp_delta\": null}\n"
    ));

    // The change is persisted in the design file and recorded in loop state.
    let model = read_json(&args.model);
    assert_eq!(
        model.get("access_patterns"),
        Some(&json(
            r#"[{"pattern_id":"Get","operation":"GetItem","table":"T","peak_rps":200,"estimated_item_size_bytes":1024}]"#
        ))
    );
    let state = read_json(&args.loop_state);
    let Some(Json::Array(rounds)) = state.get("rounds") else {
        panic!("rounds list");
    };
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[1].get("round"), Some(&Json::Int(1)));
    assert_eq!(
        rounds[1].get("timestamp"),
        Some(&Json::string("2026-02-03T00:00:00Z"))
    );
    assert_eq!(
        rounds[1].get("applied_diff"),
        Some(&read_json(&dir.join("change.json")))
    );
    assert_eq!(
        state.get("current_schema_fingerprint"),
        Some(&fingerprint),
        "volume is not schema"
    );
    let headline = rounds[1].get("headline").expect("headline");
    let monthly = headline.get("calculator_monthly_usd").cloned();
    assert_eq!(monthly, Some(Json::Float(32.4)));
}

#[test]
fn a_schema_change_moves_the_fingerprint() {
    let (dir, mut args) = workspace("schema-move");
    args.calculator_only = true;
    let mut env = FakeEnv::default();
    run(&args, &mut env).result.expect("round 0");
    let before = read_json(&args.loop_state)
        .get("current_schema_fingerprint")
        .cloned();
    let change = dir.join("change.json");
    fs::write(
        &change,
        r#"{"ops":[{"op":"add","path":"/tables/0/gsis","value":[{"index_name":"G","partition_key":"gk"}]}]}"#,
    )
    .expect("change");
    args.apply_change = Some(change);
    run(&args, &mut env).result.expect("round 1");
    let after = read_json(&args.loop_state)
        .get("current_schema_fingerprint")
        .cloned();
    assert_ne!(before, after);
}

#[test]
fn a_round_that_needs_a_deploy_refuses_without_consent_before_any_step() {
    let (dir, args) = workspace("no-consent");
    let mut env = FakeEnv::default();
    let outcome = run(&args, &mut env);
    let error = failure_of(&outcome);
    assert_eq!(error.exit_code(), 4);
    assert!(error.message().starts_with("this round requires a real AWS deploy (schema changed or no active deployment), but --yes-deploy was not passed."));
    assert!(error
        .message()
        .contains("run the prior teardown.sh first if the schema changed.)"));
    assert!(env.steps.is_empty(), "no step may run before consent");
    assert!(
        !args.loop_state.exists(),
        "nothing is recorded for a refused round"
    );
    let workdir = fs::canonicalize(&*dir).expect("canonical dir");
    assert!(
        !workdir.join(".iterate_config.json").exists(),
        "overlay cleaned up"
    );
    assert!(outcome.stdout.contains("-> decision=deploy"));
    // A dry run is still a deploy step and needs the same consent.
    let mut dry = args_for(&dir);
    dry.dry_run = true;
    assert_eq!(failure_of(&run(&dry, &mut env)).exit_code(), 4);
}

#[test]
fn skip_deploy_refuses_a_round_that_needs_a_redeploy() {
    let (_dir, mut args) = workspace("skip-deploy");
    args.skip_deploy = true;
    args.yes_deploy = true;
    let mut env = FakeEnv::default();
    let outcome = run(&args, &mut env);
    let error = failure_of(&outcome);
    let fingerprint = schema_fingerprint(&json(MODEL)).expect("fingerprint");
    assert_eq!(error.exit_code(), 2);
    assert_eq!(
        error.message(),
        format!("--skip-deploy was passed but this round needs a redeploy (schema fingerprint changed: None -> {fingerprint}). Re-run without --skip-deploy and with --yes-deploy to redeploy.")
    );
    assert!(env.steps.is_empty());
    assert!(
        !outcome.stdout.contains("round 0"),
        "refused before the round line"
    );
}

const SUMMARY: &str = r#"{"patterns":[
    {"pattern_id":"Hot","steady_state":{"throttles":3,"p99_ms":12.345,"amplification_ratio":1.5,
                                        "key_distribution":{"top_key_share":0.33333}}},
    {"pattern_id":"Cold","steady_state":{"throttles":0,"p99_ms":7,"amplification_ratio":2.25}},
    {"pattern_id":"Bare"}]}"#;

const FINDINGS: &str = r#"{"top_cost_drivers":[{"monthly":9.0,"share":0},{"monthly":50.0,"share":0.5}],
    "classified_findings":[{"signal":"hot_partition"},{"signal":"gsi_amp"},{"signal":"gsi_amp"}]}"#;

fn step_names(env: &FakeEnv) -> Vec<&str> {
    env.steps.iter().map(|argv| argv[1].as_str()).collect()
}

#[test]
fn a_reuse_round_runs_benchmark_report_and_cost_in_order() {
    let (dir, mut args) = workspace("reuse");
    fs::write(&args.manifest, r#"{"prefix":"p1","lambda":{"name":"fn"}}"#).expect("manifest");
    fs::write(&args.config, r#"{"duration_seconds":7}"#).expect("config");
    args.allow_spend = true;
    let mut env = FakeEnv {
        summary: SUMMARY.to_string(),
        findings: FINDINGS.to_string(),
        ..FakeEnv::default()
    };
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("round succeeds");

    assert_eq!(
        step_names(&env),
        ["benchmark-model", "generate-perf-report", "calculate-costs"]
    );
    let workdir = fs::canonicalize(&*dir).expect("canonical dir");
    let overlay = workdir.join(".iterate_config.json");
    let bench = &env.steps[0];
    assert_eq!(arg_value(bench, "--config"), overlay);
    assert_eq!(arg_value(bench, "--manifest"), args.manifest);
    assert_eq!(
        arg_value(bench, "--raw-out"),
        workdir.join("perf_raw.jsonl")
    );
    assert_eq!(
        arg_value(bench, "--summary-out"),
        workdir.join("perf_summary.json")
    );
    assert!(
        bench.contains(&"--allow-spend".to_string()),
        "spend consent is forwarded"
    );
    assert_eq!(
        arg_value(&env.steps[1], "--findings-out"),
        workdir.join("design_findings.json")
    );
    assert_eq!(
        arg_value(&env.steps[1], "--output"),
        workdir.join("performance_report.md")
    );

    // The benchmark read the RESOLVED config: explicit values win, the mode
    // preset fills the rest, and the overlay is gone afterwards.
    let resolved = parse_json(env.bench_config.as_deref().expect("bench config")).expect("json");
    assert_eq!(resolved.get("duration_seconds"), Some(&Json::Int(7)));
    assert_eq!(resolved.get("mode"), Some(&Json::string("representative")));
    assert_eq!(resolved.get("zipf_s"), Some(&Json::Float(1.1)));
    assert_eq!(resolved.get("max_rps_per_pattern"), Some(&Json::Int(1800)));
    assert!(!overlay.exists());

    assert!(outcome.stdout.contains("(prior=None) -> decision=reuse\n[iterate_design] reusing existing deployment (prefix=p1).\n"));
    assert!(outcome
        .stdout
        .contains("\n[iterate_design] benchmark_model: toolkit dynamodb benchmark-model --model "));
    assert!(outcome.stdout.contains("  calculator monthly: $16.20\n"));
    assert!(outcome
        .stdout
        .contains("  measured-extrapolated monthly: $100.00\n"));
    assert!(outcome
        .stdout
        .contains("  hot-pattern throttles: {'Hot': 3}\n"));
    assert!(outcome.stdout.contains("  max GSI amplification: 2.25×\n"));
    assert!(outcome
        .stdout
        .contains("  design-finding signals: ['gsi_amp', 'hot_partition']\n"));
    assert!(outcome
        .stdout
        .contains("  artifacts: design_findings.json, cost_report.md, performance_report.md\n"));

    let state = read_json(&args.loop_state);
    assert_eq!(state.get("active_manifest"), Some(&Json::string("p1")));
    let Some(Json::Array(rounds)) = state.get("rounds") else {
        panic!("rounds list");
    };
    assert_eq!(
        rounds[0].get("deploy_decision"),
        Some(&Json::string("reuse"))
    );
    assert_eq!(
        rounds[0].get("finding_signals"),
        Some(&json(r#"["gsi_amp","hot_partition"]"#))
    );
    assert_eq!(
        rounds[0].get("headline"),
        Some(&json(
            r#"{"extrapolated_monthly_usd":100.0,"calculator_monthly_usd":16.2,
                "hot_pattern_throttles":{"Hot":3},"p99_ms_by_pattern":{"Hot":12.3,"Cold":7},
                "max_gsi_amplification":2.25,"top_key_share_by_pattern":{"Hot":0.333}}"#
        ))
    );
}

#[test]
fn a_matching_fingerprint_with_a_manifest_reuses_and_a_changed_one_redeploys() {
    let (dir, mut args) = workspace("redeploy");
    // Round 0 records the fingerprint (calculator only).
    args.calculator_only = true;
    let mut env = FakeEnv::default();
    run(&args, &mut env).result.expect("round 0");
    fs::write(&args.manifest, r#"{"prefix":"old","lambda":{"name":"fn"}}"#).expect("manifest");

    // Same schema + manifest -> reuse.
    args.calculator_only = false;
    env.summary = "{}".to_string();
    env.findings = "{}".to_string();
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("reuse round");
    assert!(outcome.stdout.contains("-> decision=reuse"));
    assert!(outcome
        .stdout
        .contains("  hot-pattern throttles: none observed\n"));
    assert!(outcome
        .stdout
        .contains("  design-finding signals: none (clean)\n"));

    // A new GSI changes the fingerprint: deploy, with consent, and the
    // teardown reminder because a previous deployment exists.
    let change = dir.join("change.json");
    fs::write(
        &change,
        r#"{"ops":[{"op":"add","path":"/tables/0/gsis","value":[{"index_name":"G","partition_key":"gk"}]}]}"#,
    )
    .expect("change");
    args.apply_change = Some(change);
    args.yes_deploy = true;
    env.steps.clear();
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("deploy round");
    assert_eq!(
        step_names(&env),
        [
            "deploy-model",
            "benchmark-model",
            "generate-perf-report",
            "calculate-costs"
        ]
    );
    assert!(outcome.stdout.contains("-> decision=deploy"));
    assert!(outcome.stdout.contains(
        "[iterate_design] NOTE: schema changed since the last deployment. The prior resources are NOT torn down automatically — run the prior teardown.sh to avoid orphans.\n"
    ));
    let deploy = &env.steps[0];
    assert!(deploy.contains(&"--yes-deploy".to_string()));
    assert_eq!(arg_value(deploy, "--manifest-out"), args.manifest);
    let state = read_json(&args.loop_state);
    assert_eq!(
        state.get("active_manifest"),
        Some(&Json::string("p-new")),
        "the deployed manifest is recorded"
    );
}

#[test]
fn a_first_deploy_has_no_teardown_reminder_and_dry_run_stops_after_deploy() {
    let (dir, mut args) = workspace("dry-run");
    args.yes_deploy = true;
    args.dry_run = true;
    let mut env = FakeEnv::default();
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("dry run");
    assert_eq!(
        step_names(&env),
        ["deploy-model"],
        "no benchmark after a rehearsal"
    );
    assert!(!outcome.stdout.contains("NOTE: schema changed"));
    assert!(outcome.stdout.contains(
        "[iterate_design] dry-run: deploy rehearsed, no resources created; skipping benchmark.\n"
    ));
    assert!(outcome.stdout.contains("  decision: deploy(dry-run)\n"));
    assert!(outcome
        .stdout
        .contains("  hot-pattern throttles: none observed\n"));
    // The deploy step saw the dry-run flag layered on the resolved config.
    let config = parse_json(env.deploy_config.as_deref().expect("deploy config")).expect("json");
    assert_eq!(config.get("dry_run"), Some(&Json::Bool(true)));
    assert_eq!(config.get("zipf_s"), Some(&Json::Float(1.1)));
    // Both transient overlays are removed; the user's config is untouched.
    let workdir = fs::canonicalize(&*dir).expect("canonical dir");
    assert!(!workdir.join(".iterate_config.json").exists());
    assert!(!workdir.join(".iterate_dry_config.json").exists());
    assert_eq!(fs::read_to_string(&args.config).expect("config"), "{}");
    assert!(!args.manifest.exists(), "a rehearsal creates no manifest");
    let state = read_json(&args.loop_state);
    let Some(Json::Array(rounds)) = state.get("rounds") else {
        panic!("rounds list");
    };
    assert_eq!(
        rounds[0].get("deploy_decision"),
        Some(&Json::string("deploy(dry-run)"))
    );
}

#[test]
fn a_failing_step_stops_the_round_with_its_exit_status_and_cleans_up() {
    let (dir, mut args) = workspace("step-fails");
    fs::write(&args.manifest, r#"{"prefix":"p1","lambda":{"name":"fn"}}"#).expect("manifest");
    args.timestamp = Some("t".to_string());
    let mut env = FakeEnv {
        fail_on: Some("benchmark-model"),
        ..FakeEnv::default()
    };
    let outcome = run(&args, &mut env);
    let error = failure_of(&outcome);
    assert_eq!(error.exit_code(), 3);
    assert_eq!(
        error.message(),
        "step 'benchmark_model' failed (exit 3). See output above."
    );
    assert_eq!(outcome.diagnostics, vec!["error: boom".to_string()]);
    assert_eq!(
        step_names(&env),
        ["benchmark-model"],
        "later steps do not run"
    );
    assert!(!args.loop_state.exists(), "a failed round is not recorded");
    let workdir = fs::canonicalize(&*dir).expect("canonical dir");
    assert!(
        !workdir.join(".iterate_config.json").exists(),
        "overlay removed after a failure"
    );
}

#[test]
fn a_corrupt_loop_state_is_an_error_not_a_silent_reset() {
    let (_dir, mut args) = workspace("bad-state");
    args.calculator_only = true;
    fs::write(&args.loop_state, r#"{"loop_id":"x"}"#).expect("state");
    let mut env = FakeEnv::default();
    let error = failure_of(&run(&args, &mut env)).clone();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("no `rounds` list"), "{error}");
    fs::write(&args.loop_state, "[1]").expect("state");
    let error = failure_of(&run(&args, &mut env)).clone();
    assert!(error.message().contains("must be a JSON object"), "{error}");
}

#[test]
fn missing_inputs_exit_two_and_unreadable_ones_exit_one() {
    let (dir, _) = workspace("inputs");
    let mut env = FakeEnv::default();

    let mut missing_model = args_for(&dir);
    missing_model.model = dir.join("absent.json");
    let outcome = run(&missing_model, &mut env);
    let error = failure_of(&outcome);
    assert_eq!(error.exit_code(), 2);
    assert_eq!(
        error.message(),
        format!("file not found: {}", dir.join("absent.json").display())
    );

    let mut missing_config = args_for(&dir);
    missing_config.config = dir.join("absent.json");
    assert_eq!(failure_of(&run(&missing_config, &mut env)).exit_code(), 2);

    let mut missing_change = args_for(&dir);
    missing_change.calculator_only = true;
    missing_change.apply_change = Some(dir.join("absent.json"));
    assert_eq!(failure_of(&run(&missing_change, &mut env)).exit_code(), 2);

    // Malformed JSON is a runtime error (the Python traceback exited 1).
    fs::write(dir.join("broken.json"), "{not json").expect("broken");
    let mut broken = args_for(&dir);
    broken.model = dir.join("broken.json");
    let error = failure_of(&run(&broken, &mut env)).clone();
    assert_eq!(error.exit_code(), 1);
    assert_eq!(error.code(), "parse_error");

    // Models and configs must be objects.
    fs::write(dir.join("list.json"), "[]").expect("list");
    let mut list_model = args_for(&dir);
    list_model.model = dir.join("list.json");
    assert!(failure_of(&run(&list_model, &mut env))
        .message()
        .contains("data model must be a JSON object"));
    let mut list_config = args_for(&dir);
    list_config.config = dir.join("list.json");
    assert!(failure_of(&run(&list_config, &mut env))
        .message()
        .contains("benchmark config must be a JSON object"));
    assert!(env.steps.is_empty());
}

#[cfg(unix)]
#[test]
fn symlinked_inputs_are_refused() {
    let (dir, mut args) = workspace("symlinks");
    args.calculator_only = true;
    let real = dir.join("real-model.json");
    fs::rename(&args.model, &real).expect("rename");
    std::os::unix::fs::symlink(&real, &args.model).expect("symlink");
    let mut env = FakeEnv::default();
    let error = failure_of(&run(&args, &mut env)).clone();
    assert_eq!(error.code(), "path_symlink");
    assert_eq!(error.exit_code(), 1);
    assert!(env.steps.is_empty());
}

// ---------------------------------------------------------------------------
// Step dispatch.
// ---------------------------------------------------------------------------

#[test]
fn unported_pipeline_steps_fail_before_any_cloud_action() {
    let mut buffer: Vec<u8> = Vec::new();
    let mut out = Output::new(&mut buffer);
    for step in ["benchmark-model", "generate-perf-report", "nonsense"] {
        let argv: Vec<String> = ["dynamodb", step, "--model", "m.json"]
            .map(String::from)
            .to_vec();
        let error = dispatch_step(&argv, &mut out).expect_err(step);
        assert_eq!(error.code(), "dynamodb_step_unavailable", "{step}");
        assert_eq!(error.exit_code(), 1);
        assert!(
            error.message().contains(&format!(
                "`toolkit dynamodb {step}` is not available in this build"
            )),
            "{error}"
        );
    }
    // A registered step with bad arguments is a usage error, not "unavailable".
    let argv: Vec<String> = ["dynamodb", "calculate-costs", "--bogus"]
        .map(String::from)
        .to_vec();
    let error = dispatch_step(&argv, &mut out).expect_err("usage");
    assert_eq!(error.kind(), ErrorKind::Usage);
    // `deploy-model` (#4391) is registered: missing options are a usage error.
    let argv: Vec<String> = ["dynamodb", "deploy-model", "--model", "m.json"]
        .map(String::from)
        .to_vec();
    let error = dispatch_step(&argv, &mut out).expect_err("usage");
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(buffer.is_empty());
}

// Python's json.dumps(ensure_ascii=True) escapes U+007F as the six ASCII bytes
// `\u007f` (its ESCAPE_ASCII class keeps only U+0020..U+007E raw). The digest
// below is the SHA-256 of the signature text below, computed with `sha256sum`
// from the literal bytes (not from this code), so a raw DEL byte in the hashed
// text (digest 5a6514eb7310...) would fail here and make an unchanged design
// look like a schema change.
const DEL_SIGNATURE: &str = r#"[["Tbl", "pk\u007f", null, [], false, null]]"#;
const DEL_SIGNATURE_SHA256: &str =
    "2212db5492d3842b139f0f5e387159aac665e0f01b1048e379f2385b74434630";
const DEL_MODEL: &str = r#"{"tables":[{"table_name":"Tbl","key_schema":{"partition_key":"pk\u007f"}}],
    "access_patterns":[{"pattern_id":"P","operation":"GetItem","table":"Tbl","peak_rps":1}]}"#;

#[test]
fn a_del_character_in_a_key_name_keeps_the_legacy_fingerprint() {
    assert_eq!(sha256_hex(DEL_SIGNATURE.as_bytes()), DEL_SIGNATURE_SHA256);
    // The raw-DEL spelling is what the old (wrong) serializer hashed.
    assert_eq!(
        sha256_hex("[[\"Tbl\", \"pk\u{7f}\", null, [], false, null]]".as_bytes()),
        "5a6514eb73103e53c10a1d43217a9af688b449a7fc8a7c073bc2345f47eeb11b"
    );
    let fingerprint = schema_fingerprint(&json(DEL_MODEL)).expect("fingerprint");
    assert_eq!(fingerprint, DEL_SIGNATURE_SHA256[..12]);
    assert_eq!(fingerprint, "2212db5492d3");
}

#[test]
fn a_del_character_in_a_key_name_reuses_the_recorded_deployment() {
    let dir = TempDir::new("del-reuse");
    fs::write(dir.join("model.json"), DEL_MODEL).expect("model");
    fs::write(dir.join("config.json"), "{}").expect("config");
    fs::write(
        dir.join("created_resources.json"),
        r#"{"prefix":"existing","lambda":{"name":"fn"}}"#,
    )
    .expect("manifest");
    // A loop state written by the legacy script for this very design.
    fs::write(
        dir.join("loop_state.json"),
        r#"{"rounds":[],"current_schema_fingerprint":"2212db5492d3"}"#,
    )
    .expect("loop state");
    let args = args_for(&dir);
    let mut env = FakeEnv {
        summary: "{}".to_string(),
        findings: "{}".to_string(),
        ..FakeEnv::default()
    };
    // No --yes-deploy: a redeploy decision would be refused with status 4.
    let outcome = run(&args, &mut env);
    outcome.result.as_ref().expect("an unchanged design reuses");
    assert!(
        outcome
            .stdout
            .contains("schema_fingerprint=2212db5492d3 (prior=2212db5492d3) -> decision=reuse"),
        "{}",
        outcome.stdout
    );
    assert!(!step_names(&env).contains(&"deploy-model"));
}
