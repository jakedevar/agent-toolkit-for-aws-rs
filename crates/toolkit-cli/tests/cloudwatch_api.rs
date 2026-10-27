//! End-to-end tests of the `CloudWatch` operation commands (#4381):
//! `cloudwatch instrumentation`, `snapshots`, `evaluate-traces` and
//! `capture-dataset`. Real process spawns, real exit codes and separated
//! stdout/stderr. Every AWS answer is a recorded fixture or absent: no test
//! reaches the network, reads credentials or changes a resource.

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
    let path = std::env::temp_dir().join(format!(
        "toolkit-cloudwatch-api-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

/// A scrubbed environment: no region, no credentials, no endpoint.
fn command(args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn toolkit(args: &[&str]) -> Output {
    command(args).output().expect("spawn toolkit")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

fn write(root: &Path, name: &str, contents: &str) -> String {
    let path = root.join(name);
    fs::write(&path, contents).expect("write fixture");
    path.display().to_string()
}

const HASH: &str = "0123456789abcdef";

// ---- contract -------------------------------------------------------------

#[test]
fn print_contract_lists_every_op_with_required_and_default() {
    let output = toolkit(&["cloudwatch", "instrumentation", "--print-contract"]);
    assert_eq!(status(&output), 0);
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("{\n  \"api_version\": \"2024-04-15\",\n  \"encoding\": \"toolkit cloudwatch instrumentation <op> --json-file args.json\","), "{stdout}");
    for op in [
        "create",
        "list",
        "get",
        "delete",
        "batch-delete-by-scope",
        "batch-delete-by-arns",
        "get-status",
        "check-status",
    ] {
        assert!(stdout.contains(&format!("\"{op}\": {{")), "{op}: {stdout}");
    }
    assert!(stdout.contains("\"default\": \"dynamic instrumentation\""));
    assert!(output.stderr.is_empty());

    let snapshots = toolkit(&["cloudwatch", "snapshots", "--print-contract"]);
    assert_eq!(status(&snapshots), 0);
    let stdout = text(&snapshots.stdout);
    assert!(stdout.contains(
        "\"surface\": \"public CloudWatch Logs Insights (/aws/service-events/{service})\""
    ));
    assert!(stdout.contains("\"sample\": {") && stdout.contains("\"search\": {"));
}

#[test]
fn help_lists_the_operation_commands() {
    let output = toolkit(&["--help"]);
    assert_eq!(status(&output), 0);
    let stdout = text(&output.stdout);
    for expected in [
        "toolkit cloudwatch instrumentation OP",
        "toolkit cloudwatch snapshots OP",
        "toolkit cloudwatch evaluate-traces",
        "toolkit cloudwatch capture-dataset",
    ] {
        assert!(stdout.contains(expected), "{expected}");
    }
}

// ---- instrumentation ------------------------------------------------------

fn recorded_list(root: &Path) -> String {
    write(
        root,
        "recorded.json",
        r#"{"calls": [
            {"service": "application-signals",
             "operation": "ListInstrumentationConfigurations",
             "request": {"Service": "svc", "Environment": "prod", "InstrumentationType": "BREAKPOINT"},
             "response": {"Configurations": []}},
            {"service": "application-signals",
             "operation": "DeleteInstrumentationConfiguration",
             "response": {}}
        ]}"#,
    )
}

#[test]
fn list_replays_a_recorded_response() {
    let root = temp_root("list");
    let recorded = recorded_list(&root);
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        r#"{"service": "svc", "environment": "prod", "instrumentation_type": "breakpoint"}"#,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).contains("BREAKPOINT"),
        "{}",
        text(&output.stdout)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn an_unrecorded_call_fails_closed_without_echoing_the_request() {
    let root = temp_root("unrecorded");
    let recorded = recorded_list(&root);
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        r#"{"service": "SECRET-SERVICE", "environment": "prod", "instrumentation_type": "breakpoint"}"#,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 1);
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains(
            "no recorded response for application-signals ListInstrumentationConfigurations"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("SECRET-SERVICE"),
        "the attempted-parameters block shows the caller's own input"
    );
    assert!(
        !stdout.contains("no recorded response for application-signals ListInstrumentationConfigurations with this request: SECRET"),
        "the replay error itself never carries request values"
    );
}

#[test]
fn without_a_recording_no_call_is_made_and_the_run_says_why() {
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        r#"{"service": "svc", "environment": "prod", "instrumentation_type": "breakpoint"}"#,
    ]);
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stdout)
            .contains("this build includes no live AWS client; pass --recorded FILE"),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn mutating_ops_are_refused_without_the_confirmation_flag() {
    let root = temp_root("mutating");
    let recorded = recorded_list(&root);
    let cases: [(&str, &str); 4] = [
        (
            "create",
            r#"{"instrumentation_type": "BREAKPOINT", "service": "s", "environment": "e", "language": "python", "file_path": "/a.py", "code_unit": "a", "method_name": "m"}"#,
        ),
        (
            "delete",
            r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT", "location_hash": "0123456789abcdef"}"#,
        ),
        (
            "batch-delete-by-scope",
            r#"{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT"}"#,
        ),
        (
            "batch-delete-by-arns",
            r#"{"resource_arns": ["arn:a"], "instrumentation_type": "BREAKPOINT"}"#,
        ),
    ];
    for (op, arguments) in cases {
        let output = toolkit(&[
            "cloudwatch",
            "instrumentation",
            op,
            "--json",
            arguments,
            "--recorded",
            &recorded,
        ]);
        assert_eq!(status(&output), 1, "{op}");
        let stdout = text(&output.stdout);
        assert!(
            stdout.starts_with(&format!(
                "ERROR: refusing to run '{op}': it changes live resources."
            )),
            "{op}: {stdout}"
        );
        assert!(stdout.contains("--i-have-confirmed"));
    }
}

#[test]
fn a_confirmed_delete_replays_the_recorded_answer() {
    let root = temp_root("delete");
    let recorded = recorded_list(&root);
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "delete",
        "--json",
        &format!(
            r#"{{"service": "s", "environment": "e", "instrumentation_type": "BREAKPOINT", "location_hash": "{HASH}"}}"#
        ),
        "--recorded",
        &recorded,
        "--i-have-confirmed",
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stdout));
    assert!(text(&output.stdout).starts_with("Successfully deleted BREAKPOINT instrumentation"));
}

#[test]
fn create_uses_the_pinned_clock_for_expiry_and_creation_time() {
    let root = temp_root("create");
    let recorded = write(
        &root,
        "recorded.json",
        r#"{"calls": [
            {"service": "application-signals", "operation": "CreateInstrumentationConfiguration",
             "request": {"ExpiresAt": "2025-02-03T20:42:00Z", "Service": "svc"},
             "response": {"LocationHash": "0123456789abcdef", "ARN": "arn:aws:x"}}
        ]}"#,
    );
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "create",
        "--json",
        r#"{"instrumentation_type": "BREAKPOINT", "service": "svc", "environment": "prod",
            "language": "python", "file_path": "/app/h.py", "code_unit": "app.h",
            "method_name": "handle", "ttl_hours": 2}"#,
        "--recorded",
        &recorded,
        "--now",
        "2025-02-03T18:42:00Z",
        "--i-have-confirmed",
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stdout));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("0123456789abcdef"), "{stdout}");
    assert!(
        stdout.contains("2025-02-03T18:42:00Z"),
        "the pinned clock is the CreatedAt fallback: {stdout}"
    );
}

#[test]
fn arguments_can_come_from_a_file_but_not_from_stdin() {
    let root = temp_root("argsfile");
    let recorded = recorded_list(&root);
    let arguments =
        r#"{"service": "svc", "environment": "prod", "instrumentation_type": "BREAKPOINT"}"#;
    let file = write(&root, "args.json", arguments);
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json-file",
        &file,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));

    // The router refuses a value that starts with `-`, so `-` (stdin) is a usage error.
    let output = toolkit(&["cloudwatch", "instrumentation", "list", "--json", "-"]);
    assert_eq!(status(&output), 2);
    assert!(output.stdout.is_empty());
}

#[test]
fn usage_errors_exit_two_with_stderr_only() {
    let cases: [&[&str]; 6] = [
        &["cloudwatch", "instrumentation"],
        &["cloudwatch", "instrumentation", "nope", "--json", "{}"],
        &["cloudwatch", "instrumentation", "list"],
        &["cloudwatch", "instrumentation", "list", "--json", "[]"],
        &[
            "cloudwatch",
            "instrumentation",
            "list",
            "--json",
            "{\"bogus\": 1}",
        ],
        &[
            "cloudwatch",
            "snapshots",
            "sample",
            "--json",
            "{\"service\": 5}",
        ],
    ];
    for args in cases {
        let output = toolkit(args);
        assert_eq!(status(&output), 2, "{args:?}: {}", text(&output.stdout));
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    let both = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        "{}",
        "--json-file",
        "x",
    ]);
    assert_eq!(status(&both), 2);
    let bad_now = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        "{}",
        "--now",
        "someday",
    ]);
    assert_eq!(status(&bad_now), 2);
}

#[test]
fn malformed_recordings_are_rejected_without_echoing_their_values() {
    let root = temp_root("badrecording");
    let recorded = write(
        &root,
        "recorded.json",
        r#"{"calls": [{"service": "logs", "operation": "X", "request": null, "response": {"k": "FAKE-SECRET-VALUE"}}]}"#,
    );
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "PROBE"}"#,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 1);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("recorded call 0: \"request\" must be an object when present (it is null)"),
        "{stderr}"
    );
    assert!(!stderr.contains("FAKE-SECRET"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlinked_recording_is_refused() {
    let root = temp_root("symlink-recording");
    let real = write(&root, "real.json", r#"{"calls": []}"#);
    let link = root.join("link.json");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        r#"{"service": "s", "environment": "e", "instrumentation_type": "PROBE"}"#,
        "--recorded",
        &link.display().to_string(),
    ]);
    assert_eq!(status(&output), 1);
    assert!(output.stdout.is_empty());
}

#[test]
fn caller_and_aws_text_cannot_inject_controls_or_forged_lines() {
    let root = temp_root("hostile");
    let recorded = write(
        &root,
        "recorded.json",
        r#"{"calls": [{"service": "application-signals",
            "operation": "ListInstrumentationConfigurations",
            "error": {"code": "Boom\u001b[2J", "message": "AWS_SENTINEL\u001b[2J\nFORGED_AWS_LINE"}}]}"#,
    );
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "list",
        "--json",
        r#"{"service": "CALLER_SENTINEL\u001b[2J\nFORGED_CALLER_LINE", "environment": "e", "instrumentation_type": "PROBE"}"#,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 1);
    for stream in [text(&output.stdout), text(&output.stderr)] {
        for c in stream.chars() {
            assert!(
                c == '\n' || !c.is_control(),
                "raw control {c:?} in {stream:?}"
            );
        }
        assert!(
            stream.lines().all(|line| {
                !line.starts_with("FORGED_AWS_LINE") && !line.starts_with("FORGED_CALLER_LINE")
            }),
            "forged line in {stream:?}"
        );
    }
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("CALLER_SENTINEL\\x1b[2J\\nFORGED_CALLER_LINE"),
        "{stdout}"
    );
    assert!(stdout.contains("AWS_SENTINEL"), "{stdout}");
}

// ---- snapshots ------------------------------------------------------------

fn recorded_query(root: &Path, status: &str) -> String {
    write(
        root,
        "query.json",
        &format!(
            r#"{{"calls": [
                {{"service": "logs", "operation": "StartQuery", "response": {{"queryId": "q-1"}}}},
                {{"service": "logs", "operation": "GetQueryResults",
                  "response": {{"status": "{status}",
                    "results": [[{{"field": "@message", "value": "{{\"a\": 1}}"}}]]}}}}
            ]}}"#
        ),
    )
}

const SAMPLE_ARGS: &str = r#"{"service": "svc", "environment": "prod", "location_hash": "0123456789abcdef", "status_timestamp": "2025-02-03T18:42:00Z"}"#;

#[test]
fn sample_prints_json_and_exits_zero_on_a_complete_query() {
    let root = temp_root("sample");
    let recorded = recorded_query(&root, "Complete");
    let output = toolkit(&[
        "cloudwatch",
        "snapshots",
        "sample",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stdout));
    assert!(text(&output.stdout).trim_start().starts_with('{'));
}

#[test]
fn a_failed_query_exits_one_even_though_the_output_is_json() {
    let root = temp_root("failed");
    let recorded = recorded_query(&root, "Failed");
    let output = toolkit(&[
        "cloudwatch",
        "snapshots",
        "sample",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 1);
    assert!(text(&output.stdout).trim_start().starts_with('{'));
}

#[test]
fn sample_without_a_recording_reports_the_missing_client_and_exits_one() {
    let output = toolkit(&["cloudwatch", "snapshots", "sample", "--json", SAMPLE_ARGS]);
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stdout).contains("this build includes no live AWS client"),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn an_invalid_hash_is_a_bare_error_line_with_exit_one() {
    let output = toolkit(&[
        "cloudwatch",
        "snapshots",
        "sample",
        "--json",
        r#"{"service": "s", "environment": "e", "location_hash": "NOPE", "status_timestamp": "2025-02-03T18:42:00Z"}"#,
    ]);
    assert_eq!(status(&output), 1);
    assert_eq!(
        text(&output.stdout),
        "ERROR: location_hash must be a 16-character hex string\n"
    );
}

#[cfg(unix)]
#[test]
fn out_files_are_owner_only_and_never_written_through_a_symlink() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("out");
    let recorded = recorded_query(&root, "Complete");
    let target = root.join("snapshot.json");
    let output = toolkit(&[
        "cloudwatch",
        "snapshots",
        "sample",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        &recorded,
        "--out",
        &target.display().to_string(),
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!("wrote result to {} (0600)\n", target.display())
    );
    let mode = fs::metadata(&target).expect("written").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(fs::read_to_string(&target)
        .expect("read")
        .trim_start()
        .starts_with('{'));

    // A looser pre-existing file is narrowed before it is rewritten.
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).expect("chmod");
    let output = toolkit(&[
        "cloudwatch",
        "snapshots",
        "sample",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        &recorded,
        "--out",
        &target.display().to_string(),
    ]);
    assert_eq!(status(&output), 0);
    assert_eq!(
        fs::metadata(&target).expect("meta").permissions().mode() & 0o777,
        0o600
    );

    // A planted symlink is refused and its target is left alone.
    let victim = root.join("victim.txt");
    fs::write(&victim, "untouched").expect("victim");
    let link = root.join("link.json");
    std::os::unix::fs::symlink(&victim, &link).expect("symlink");
    let output = toolkit(&[
        "cloudwatch",
        "snapshots",
        "sample",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        &recorded,
        "--out",
        &link.display().to_string(),
    ]);
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stderr).contains("symbolic link"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&victim).expect("victim"), "untouched");
}

// ---- trace evaluation and dataset capture ---------------------------------

fn recorded_traces(root: &Path) -> String {
    write(
        root,
        "traces.json",
        r#"{"calls": [
            {"service": "cloudwatchomni", "operation": "StartTelemetryQuerySession", "response": {"sessionId": "s"}},
            {"service": "cloudwatchomni", "operation": "StartTelemetryQuery", "response": {"queryId": "q"}},
            {"service": "cloudwatchomni", "operation": "GetTelemetryQueryResults",
             "request": {"maxResults": 1}, "response": {"status": "Complete"}},
            {"service": "cloudwatchomni", "operation": "GetTelemetryQueryResults",
             "request": {"maxResults": 1000},
             "response": {"rows": [{"traceId": "aa", "spanId": "0000000000000001", "name": "root",
                                    "kind": 2, "input_value": "hello", "svc_name": "svc"}]}},
            {"service": "cloudwatchomni", "operation": "StopTelemetryQuerySession", "response": {}},
            {"service": "bedrock-agentcore", "operation": "Evaluate",
             "request": {"evaluationTarget": {"traceIds": ["aa"]}},
             "response": {"evaluationResults": [{"evaluatorId": "Builtin.Helpfulness",
                                                 "value": 0.75, "label": "Helpful",
                                                 "explanation": "EXPLANATION_SENTINEL"}]}}
        ]}"#,
    )
}

#[test]
fn evaluate_traces_prints_the_receipt_without_explanations() {
    let root = temp_root("evaluate");
    let recorded = recorded_traces(&root);
    let output = toolkit(&[
        "cloudwatch",
        "evaluate-traces",
        "--trace-id",
        "AA",
        "--no-writeback",
        "--recorded",
        &recorded,
        "--now",
        "2025-02-03T18:42:00Z",
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stdout));
    let stdout = text(&output.stdout);
    assert!(
        stdout.starts_with("{\n  \"level\": \"TRACE\",\n"),
        "{stdout}"
    );
    assert!(stdout.contains("\"pairsScored\": 1"), "{stdout}");
    assert!(stdout.contains("\"hasExplanation\": true"), "{stdout}");
    assert!(
        !stdout.contains("EXPLANATION_SENTINEL"),
        "the judge's prose stays off the receipt"
    );

    let explained = toolkit(&[
        "cloudwatch",
        "evaluate-traces",
        "--trace-ids",
        "aa",
        "--no-writeback",
        "--include-explanations",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&explained), 0);
    assert!(text(&explained.stdout).contains("EXPLANATION_SENTINEL"));
}

#[test]
fn evaluate_traces_without_a_recording_sends_nothing_and_exits_one() {
    let output = toolkit(&["cloudwatch", "evaluate-traces", "--trace-id", "aa"]);
    assert_eq!(status(&output), 1);
    let stdout = text(&output.stdout);
    assert!(
        stdout.starts_with("{\"error\": \"no (trace, evaluator) pair produced a score\""),
        "{stdout}"
    );
    assert!(
        stdout.contains("this build includes no live AWS client"),
        "{stdout}"
    );
    assert_eq!(
        stdout.lines().count(),
        1,
        "a failure receipt is one compact line"
    );
}

#[test]
fn evaluate_traces_usage_and_validation_errors() {
    let bad_level = toolkit(&[
        "cloudwatch",
        "evaluate-traces",
        "--trace-id",
        "aa",
        "--level",
        "ROOT",
    ]);
    assert_eq!(status(&bad_level), 2);
    assert!(text(&bad_level.stderr).contains("argument --level: invalid choice: 'ROOT'"));
    let bad_days = toolkit(&[
        "cloudwatch",
        "evaluate-traces",
        "--trace-id",
        "aa",
        "--window-days",
        "many",
    ]);
    assert_eq!(status(&bad_days), 2);

    let none = toolkit(&["cloudwatch", "evaluate-traces"]);
    assert_eq!(status(&none), 1);
    assert_eq!(
        text(&none.stdout),
        "{\"error\": \"no trace ids given (use --trace-id or --trace-ids)\"}\n"
    );

    let hostile = toolkit(&[
        "cloudwatch",
        "evaluate-traces",
        "--trace-id",
        "zz\u{1b}[2J\nFORGED",
    ]);
    assert_eq!(status(&hostile), 1);
    let stdout = text(&hostile.stdout);
    assert!(
        !stdout.contains('\u{1b}') && stdout.lines().count() == 1,
        "{stdout:?}"
    );
}

#[test]
fn capture_dataset_refuses_to_write_without_confirmation() {
    let output = toolkit(&[
        "cloudwatch",
        "capture-dataset",
        "--trace-ids",
        "aa",
        "--dataset-name",
        "d",
    ]);
    assert_eq!(status(&output), 1);
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("\"confirmationRequired\": true"),
        "{stdout}"
    );
    assert!(stdout.starts_with(
        "{\"error\": \"refusing to write: creating/extending a dataset is a real resource write."
    ));
    assert!(output.stderr.is_empty());
}

#[test]
fn capture_dataset_confirmed_but_without_a_recording_writes_nothing() {
    let output = toolkit(&[
        "cloudwatch",
        "capture-dataset",
        "--trace-ids",
        "aa",
        "--dataset-name",
        "d",
        "--i-have-confirmed",
    ]);
    assert_eq!(status(&output), 1);
    assert!(
        text(&output.stdout).contains("this build includes no live AWS client"),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn capture_dataset_usage_errors() {
    let missing = toolkit(&["cloudwatch", "capture-dataset"]);
    assert_eq!(status(&missing), 2);
    assert!(text(&missing.stderr).contains("--trace-ids"));
    let bad_mode = toolkit(&[
        "cloudwatch",
        "capture-dataset",
        "--trace-ids",
        "aa",
        "--mode",
        "drop",
    ]);
    assert_eq!(status(&bad_mode), 2);
    assert!(text(&bad_mode.stderr).contains("argument --mode: invalid choice: 'drop'"));
}

#[test]
fn capture_dataset_replays_a_create_end_to_end() {
    let root = temp_root("capture");
    let recorded = write(
        &root,
        "capture.json",
        r#"{"calls": [
            {"service": "cloudwatchomni", "operation": "StartTelemetryQuerySession", "response": {"sessionId": "s"}},
            {"service": "cloudwatchomni", "operation": "StartTelemetryQuery", "response": {"queryId": "q"}},
            {"service": "cloudwatchomni", "operation": "GetTelemetryQueryResults",
             "request": {"maxResults": 1}, "response": {"status": "Complete"}},
            {"service": "cloudwatchomni", "operation": "GetTelemetryQueryResults",
             "request": {"maxResults": 1000},
             "response": {"rows": [{"traceId": "aa", "spanId": "r", "startTimeUnixNano": "1",
                                    "input_value": "hello", "output_value": "world"}]}},
            {"service": "cloudwatchomni", "operation": "StopTelemetryQuerySession", "response": {}},
            {"service": "bedrock-agentcore-control", "operation": "CreateDataset",
             "request": {"datasetName": "golden", "schemaType": "AGENTCORE_EVALUATION_PREDEFINED_V1"},
             "response": {"datasetId": "ds-1", "status": "CREATING"}}
        ]}"#,
    );
    let output = toolkit(&[
        "cloudwatch",
        "capture-dataset",
        "--trace-ids",
        "aa",
        "--dataset-name",
        "golden",
        "--i-have-confirmed",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stdout));
    assert_eq!(
        text(&output.stdout),
        "{\n  \"mode\": \"create\",\n  \"datasetId\": \"ds-1\",\n  \"status\": \"CREATING\",\n  \"examplesWritten\": 1\n}\n"
    );
}

// ---- the review #4742 fixtures: malformed responses fail closed ---------

const SCOPE_ARGS: &str =
    r#"{"service":"svc","environment":"prod","instrumentation_type":"BREAKPOINT"}"#;

/// #4749: a `null` listing, event list or deletion report exits 1 and never
/// reads as an empty or completed result.
#[test]
fn malformed_instrumentation_responses_exit_one_and_claim_nothing() {
    let root = temp_root("bad-appsignals");
    let cases: [(&str, &[&str], &str, &[&str]); 3] = [
        (
            r#"{"calls":[{"service":"application-signals","operation":"ListInstrumentationConfigurations","response":{"LatestConfigurations":null}}]}"#,
            &["instrumentation", "list"],
            SCOPE_ARGS,
            &["No active", "TIP: Use create_instrumentation"],
        ),
        (
            r#"{"calls":[{"service":"application-signals","operation":"GetInstrumentationConfigurationStatus","response":{"Events":null}}]}"#,
            &["instrumentation", "get-status"],
            r#"{"service":"svc","environment":"prod","instrumentation_type":"BREAKPOINT","location_hash":"0123456789abcdef","status":"READY"}"#,
            &["Events Returned", "No READY status events found"],
        ),
        (
            r#"{"calls":[{"service":"application-signals","operation":"BatchDeleteInstrumentationConfigurations","response":{"Errors":null}}]}"#,
            &[
                "instrumentation",
                "batch-delete-by-scope",
                "--i-have-confirmed",
            ],
            SCOPE_ARGS,
            &["BATCH DELETE COMPLETED", "Errors: 0"],
        ),
    ];
    for (index, (recording, command, args, forbidden)) in cases.iter().enumerate() {
        let recorded = write(&root, &format!("bad-{index}.json"), recording);
        let mut argv = vec!["cloudwatch"];
        argv.extend_from_slice(command);
        argv.extend_from_slice(&["--json", args, "--recorded", &recorded]);
        let output = toolkit(&argv);
        let stdout = text(&output.stdout);
        assert_eq!(status(&output), 1, "{command:?}: {stdout}");
        assert!(
            stdout.contains("of the response is not a list of objects (it is null)"),
            "{command:?}: {stdout}"
        );
        for text in *forbidden {
            assert!(
                !stdout.contains(text),
                "{command:?} printed {text:?}: {stdout}"
            );
        }
    }
}

/// #4750: `results: false` is an invalid response (exit 1), never
/// `NO_SNAPSHOTS_FOUND`.
#[test]
fn wrong_typed_query_results_exit_one_instead_of_no_snapshots() {
    let root = temp_root("bad-logs");
    for results in ["false", "null", "{}", r#""x""#, "1"] {
        let recorded = write(
            &root,
            "results.json",
            &format!(
                r#"{{"calls":[{{"service":"logs","operation":"StartQuery","response":{{"queryId":"q"}}}},{{"service":"logs","operation":"GetQueryResults","response":{{"status":"Complete","results":{results}}}}}]}}"#
            ),
        );
        let output = toolkit(&[
            "cloudwatch",
            "snapshots",
            "sample",
            "--json",
            SAMPLE_ARGS,
            "--recorded",
            &recorded,
        ]);
        let stdout = text(&output.stdout);
        assert_eq!(status(&output), 1, "results={results}: {stdout}");
        assert!(
            !stdout.contains("NO_SNAPSHOTS_FOUND"),
            "{results}: {stdout}"
        );
        assert!(
            stdout.contains("the member `results` of the response is not a list"),
            "{results}: {stdout}"
        );
    }
}

fn omni_prelude(row: &str) -> String {
    format!(
        r#"{{"service":"cloudwatchomni","operation":"StartTelemetryQuerySession","response":{{"sessionId":"s"}}}},
{{"service":"cloudwatchomni","operation":"StartTelemetryQuery","response":{{"queryId":"q"}}}},
{{"service":"cloudwatchomni","operation":"GetTelemetryQueryResults","request":{{"maxResults":1}},"response":{{"status":"Complete"}}}},
{{"service":"cloudwatchomni","operation":"GetTelemetryQueryResults","request":{{"maxResults":1000}},"response":{{"rows":[{row}]}}}},
{{"service":"cloudwatchomni","operation":"StopTelemetryQuerySession","response":{{}}}}"#
    )
}

/// #4755: `{datasetId: null, status: false}` is a failed write (exit 1), not
/// `examplesWritten: 1`.
#[test]
fn a_malformed_create_dataset_response_exits_one_without_a_write_count() {
    let root = temp_root("bad-dataset");
    let prelude = omni_prelude(
        r#"{"traceId":"aa","spanId":"0000000000000001","input_value":"hi","output_value":"hello"}"#,
    );
    let recorded = write(
        &root,
        "create.json",
        &format!(
            r#"{{"calls":[{prelude},
{{"service":"bedrock-agentcore-control","operation":"CreateDataset","response":{{"datasetId":null,"status":false}}}}]}}"#
        ),
    );
    let output = toolkit(&[
        "cloudwatch",
        "capture-dataset",
        "--trace-ids",
        "aa",
        "--dataset-name",
        "fixture",
        "--i-have-confirmed",
        "--recorded",
        &recorded,
    ]);
    let stdout = text(&output.stdout);
    assert_eq!(status(&output), 1, "{stdout}");
    assert!(!stdout.contains("examplesWritten"), "{stdout}");
    assert!(
        stdout.contains("the member `datasetId` of the response is not a string (it is null)"),
        "{stdout}"
    );
}

// ---- the round-2 review #4742 fixtures (#4784, #4785) -------------------

/// #4784: an absent or empty `queryId` fails both snapshot commands, and the
/// diagnostic never carries the other response members.
#[test]
fn a_missing_query_id_fails_without_echoing_the_response() {
    let root = temp_root("missing-query-id");
    let extras = r#""authorization":"SYNTHETIC_PRIVATE_SENTINEL","customerPayload":{"text":"SYNTHETIC_CUSTOMER_SENTINEL"}"#;
    for (label, response) in [
        ("absent", format!("{{{extras}}}")),
        ("empty", format!(r#"{{"queryId":"",{extras}}}"#)),
    ] {
        let recorded = write(
            &root,
            &format!("start-{label}.json"),
            &format!(
                r#"{{"calls":[{{"service":"logs","operation":"StartQuery","response":{response}}}]}}"#
            ),
        );
        for op in ["sample", "search"] {
            let output = toolkit(&[
                "cloudwatch",
                "snapshots",
                op,
                "--json",
                SAMPLE_ARGS,
                "--recorded",
                &recorded,
            ]);
            let stdout = text(&output.stdout);
            let stderr = text(&output.stderr);
            assert_eq!(status(&output), 1, "{op} {label}: {stdout}{stderr}");
            assert!(
                stdout.contains("start_query did not return a queryId"),
                "{op} {label}: {stdout}"
            );
            for sentinel in ["SYNTHETIC", "authorization", "customerPayload"] {
                assert!(
                    !stdout.contains(sentinel) && !stderr.contains(sentinel),
                    "{op} {label} leaked {sentinel}: {stdout}{stderr}"
                );
            }
        }
    }
}

/// #4785: a malformed `ListDatasets` in add mode fails (exit 1) with the
/// listing error and bb's conversion error, never without it and never with
/// a write count.
#[test]
fn a_malformed_dataset_listing_keeps_the_conversion_errors() {
    let root = temp_root("bad-listing");
    let prelude = omni_prelude(
        r#"{"traceId":"aa","spanId":"0000000000000001","name":"root","input_value":"hi","output_value":"hello","kind":2}"#,
    );
    let recorded = write(
        &root,
        "list.json",
        &format!(
            r#"{{"calls":[{prelude},
{{"service":"bedrock-agentcore-control","operation":"ListDatasets","response":{{"datasets":null}}}}]}}"#
        ),
    );
    let output = toolkit(&[
        "cloudwatch",
        "capture-dataset",
        "--trace-ids",
        "aa,bb",
        "--mode",
        "add",
        "--dataset-name",
        "fixture",
        "--i-have-confirmed",
        "--recorded",
        &recorded,
    ]);
    let stdout = text(&output.stdout);
    assert_eq!(status(&output), 1, "{stdout}");
    assert!(
        stdout.contains(
            "list_datasets failed: the ListDatasets response is invalid: the member \
             `datasets` of the response is not a list of objects (it is null)"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            r#""conversionErrors": [{"traceId": "bb", "error": "no spans found in window"}]"#
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("examplesWritten"), "{stdout}");
}

/// #4758: an object-valued score is a score error (exit 1), never
/// `tracesScored: 1` / `pairsScored: 1`.
#[test]
fn an_object_valued_score_exits_one_and_scores_nothing() {
    let root = temp_root("bad-score");
    let prelude = omni_prelude(
        r#"{"traceId":"aa","spanId":"0000000000000001","name":"root","input_value":"hi","output_value":"hello","kind":2}"#,
    );
    let recorded = write(
        &root,
        "evaluate.json",
        &format!(
            r#"{{"calls":[{prelude},
{{"service":"bedrock-agentcore","operation":"Evaluate","response":{{"evaluationResults":[{{"evaluatorId":"Builtin.Helpfulness","value":{{}},"explanation":""}}]}}}}]}}"#
        ),
    );
    let output = toolkit(&[
        "cloudwatch",
        "evaluate-traces",
        "--trace-id",
        "aa",
        "--session-id",
        "sid",
        "--no-writeback",
        "--recorded",
        &recorded,
    ]);
    let stdout = text(&output.stdout);
    assert_eq!(status(&output), 1, "{stdout}");
    assert!(!stdout.contains("tracesScored"), "{stdout}");
    assert!(!stdout.contains("pairsScored"), "{stdout}");
    assert!(
        stdout.contains("`evaluationResults[0].value` of the response is not a number or null"),
        "{stdout}"
    );
}
