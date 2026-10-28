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

use toolkit_core::{parse_json, Json};

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

const LIST_RECORDING: &str = r#"{"calls": [
            {"service": "application-signals",
             "operation": "ListInstrumentationConfigurations",
             "request": {"Service": "svc", "Environment": "prod", "InstrumentationType": "BREAKPOINT"},
             "response": {"Configurations": []}},
            {"service": "application-signals",
             "operation": "DeleteInstrumentationConfiguration",
             "response": {}}
        ]}"#;

fn recorded_list(root: &Path) -> String {
    write(root, "recorded.json", LIST_RECORDING)
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

const CREATE_RECORDING: &str = r#"{"calls": [
    {"service": "application-signals", "operation": "CreateInstrumentationConfiguration",
     "request": {"ExpiresAt": "2025-02-03T20:42:00Z", "Service": "svc"},
     "response": {"LocationHash": "0123456789abcdef", "ARN": "arn:aws:x"}}
]}"#;

const CREATE_ARGS: &str = r#"{"instrumentation_type": "BREAKPOINT", "service": "svc", "environment": "prod",
    "language": "python", "file_path": "/app/h.py", "code_unit": "app.h",
    "method_name": "handle", "ttl_hours": 2}"#;

#[test]
fn create_uses_the_pinned_clock_for_expiry_and_creation_time() {
    let root = temp_root("create");
    let recorded = write(&root, "recorded.json", CREATE_RECORDING);
    let output = toolkit(&[
        "cloudwatch",
        "instrumentation",
        "create",
        "--json",
        CREATE_ARGS,
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

fn query_recording(status: &str) -> String {
    format!(
        r#"{{"calls": [
                {{"service": "logs", "operation": "StartQuery", "response": {{"queryId": "q-1"}}}},
                {{"service": "logs", "operation": "GetQueryResults",
                  "response": {{"status": "{status}",
                    "results": [[{{"field": "@message", "value": "{{\"a\": 1}}"}}]]}}}}
            ]}}"#
    )
}

fn recorded_query(root: &Path, status: &str) -> String {
    write(root, "query.json", &query_recording(status))
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

const TRACES_RECORDING: &str = r#"{"calls": [
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
        ]}"#;

fn recorded_traces(root: &Path) -> String {
    write(root, "traces.json", TRACES_RECORDING)
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

const CAPTURE_CREATE_RECORDING: &str = r#"{"calls": [
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
]}"#;

#[test]
fn capture_dataset_replays_a_create_end_to_end() {
    let root = temp_root("capture");
    let recorded = write(&root, "capture.json", CAPTURE_CREATE_RECORDING);
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

/// The #4749 fixtures: (recording, command, arguments, forbidden texts).
#[allow(clippy::type_complexity)]
const MALFORMED_APPSIGNALS: [(&str, &[&str], &str, &[&str]); 3] = [
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

/// #4749: a `null` listing, event list or deletion report exits 1 and never
/// reads as an empty or completed result.
#[test]
fn malformed_instrumentation_responses_exit_one_and_claim_nothing() {
    let root = temp_root("bad-appsignals");
    for (index, (recording, command, args, forbidden)) in MALFORMED_APPSIGNALS.iter().enumerate() {
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

/// The wrong-typed `results` of the #4750 fixtures.
const WRONG_RESULTS: [&str; 5] = ["false", "null", "{}", r#""x""#, "1"];

/// The #4750 fixture: a complete query whose `results` is `results`.
fn wrong_results_recording(results: &str) -> String {
    format!(
        r#"{{"calls":[{{"service":"logs","operation":"StartQuery","response":{{"queryId":"q"}}}},{{"service":"logs","operation":"GetQueryResults","response":{{"status":"Complete","results":{results}}}}}]}}"#
    )
}

/// #4750: `results: false` is an invalid response (exit 1), never
/// `NO_SNAPSHOTS_FOUND`.
#[test]
fn wrong_typed_query_results_exit_one_instead_of_no_snapshots() {
    let root = temp_root("bad-logs");
    for results in WRONG_RESULTS {
        let recorded = write(&root, "results.json", &wrong_results_recording(results));
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

fn malformed_create_dataset_recording() -> String {
    let prelude = omni_prelude(
        r#"{"traceId":"aa","spanId":"0000000000000001","input_value":"hi","output_value":"hello"}"#,
    );
    format!(
        r#"{{"calls":[{prelude},
{{"service":"bedrock-agentcore-control","operation":"CreateDataset","response":{{"datasetId":null,"status":false}}}}]}}"#
    )
}

/// #4755: `{datasetId: null, status: false}` is a failed write (exit 1), not
/// `examplesWritten: 1`.
#[test]
fn a_malformed_create_dataset_response_exits_one_without_a_write_count() {
    let root = temp_root("bad-dataset");
    let recorded = write(&root, "create.json", &malformed_create_dataset_recording());
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

/// A `StartQuery` answer with an absent or empty `queryId` and unrelated
/// sentinel members.
fn missing_query_id_recording(label: &str) -> String {
    let extras = r#""authorization":"SYNTHETIC_PRIVATE_SENTINEL","customerPayload":{"text":"SYNTHETIC_CUSTOMER_SENTINEL"}"#;
    let response = if label == "empty" {
        format!(r#"{{"queryId":"",{extras}}}"#)
    } else {
        format!("{{{extras}}}")
    };
    format!(r#"{{"calls":[{{"service":"logs","operation":"StartQuery","response":{response}}}]}}"#)
}

/// #4784: an absent or empty `queryId` fails both snapshot commands, and the
/// diagnostic never carries the other response members.
#[test]
fn a_missing_query_id_fails_without_echoing_the_response() {
    let root = temp_root("missing-query-id");
    for label in ["absent", "empty"] {
        let recorded = write(
            &root,
            &format!("start-{label}.json"),
            &missing_query_id_recording(label),
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

fn malformed_listing_recording() -> String {
    let prelude = omni_prelude(
        r#"{"traceId":"aa","spanId":"0000000000000001","name":"root","input_value":"hi","output_value":"hello","kind":2}"#,
    );
    format!(
        r#"{{"calls":[{prelude},
{{"service":"bedrock-agentcore-control","operation":"ListDatasets","response":{{"datasets":null}}}}]}}"#
    )
}

/// #4785: a malformed `ListDatasets` in add mode fails (exit 1) with the
/// listing error and bb's conversion error, never without it and never with
/// a write count.
#[test]
fn a_malformed_dataset_listing_keeps_the_conversion_errors() {
    let root = temp_root("bad-listing");
    let recorded = write(&root, "list.json", &malformed_listing_recording());
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

fn object_score_recording() -> String {
    let prelude = omni_prelude(
        r#"{"traceId":"aa","spanId":"0000000000000001","name":"root","input_value":"hi","output_value":"hello","kind":2}"#,
    );
    format!(
        r#"{{"calls":[{prelude},
{{"service":"bedrock-agentcore","operation":"Evaluate","response":{{"evaluationResults":[{{"evaluatorId":"Builtin.Helpfulness","value":{{}},"explanation":""}}]}}}}]}}"#
    )
}

/// #4758: an object-valued score is a score error (exit 1), never
/// `tracesScored: 1` / `pairsScored: 1`.
#[test]
fn an_object_valued_score_exits_one_and_scores_nothing() {
    let root = temp_root("bad-score");
    let recorded = write(&root, "evaluate.json", &object_score_recording());
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

// ---- the response-mutation harness (#4381 repair 3, #4816-#4818) --------
//
// One table-driven test over every recording the command tests above
// replay, plus the richer fixtures below that carry every response member
// the slice reads. For each scenario, each recorded response step and each
// member path in it (object members and list items, at every depth), the
// command runs three more times with that member replaced by (a) a unique
// sentinel string, (b) `null` and (c) a value of another type. Each run must:
//
// - never panic;
// - never print the sentinel on stderr, nor on stdout when it exits non-zero;
// - exit non-zero, or print exactly what the unmutated run printed (the
//   member is not read), or be listed in `HARNESS_ALLOWED` with the reason
//   (a value the command exists to print, or a documented absent-default);
// - never conclude a non-terminal state (PENDING, Running, ...) with exit 0
//   that the unmutated run did not, whatever the allowlist says.
//
// Every failure is collected and printed together, so one run lists them
// all.

/// A rich instrumentation configuration: every member `CONFIGURATION` of
/// the gateway checks and the renderers read.
const RICH_CONFIGURATION: &str = r#"{"InstrumentationType": "BREAKPOINT", "SignalType": "SNAPSHOT",
    "LocationHash": "0123456789abcdef",
    "Location": {"CodeLocation": {"Language": "PYTHON", "FilePath": "/app/h.py", "CodeUnit": "app.h",
                                  "ClassName": "Handler", "MethodName": "handle", "LineNumber": 12}},
    "CaptureConfiguration": {"CodeCapture": {"CaptureReturn": true, "CaptureStackTrace": false,
        "CaptureArguments": ["order"], "CaptureLocals": ["total"],
        "CaptureLimits": {"MaxHits": 5, "MaxStringLength": 100, "MaxCollectionWidth": 10,
                          "MaxCollectionDepth": 3, "MaxStackFrames": 20, "MaxStackTraceSize": 4096,
                          "MaxObjectDepth": 3, "MaxFieldsPerObject": 20}}},
    "AttributeFilters": [{"Key": "tenant", "Value": "t1"}],
    "Description": "fixture breakpoint",
    "CreatedAt": "2025-02-03T17:00:00Z", "ExpiresAt": "2025-02-04T17:00:00Z",
    "ARN": "arn:aws:application-signals:us-east-1:123456789012:instrumentation/x"}"#;

fn appsignals(operation: &str, response: &str) -> String {
    format!(
        r#"{{"service":"application-signals","operation":"{operation}","response":{response}}}"#
    )
}

fn calls(steps: &[String]) -> String {
    format!(r#"{{"calls":[{}]}}"#, steps.join(","))
}

const GET_ARGS: &str = r#"{"service":"svc","environment":"prod","instrumentation_type":"BREAKPOINT","location_hash":"0123456789abcdef"}"#;

const STATUS_ARGS: &str = r#"{"service":"svc","environment":"prod","instrumentation_type":"BREAKPOINT","location_hash":"0123456789abcdef","status":"READY"}"#;

const CHECK_ARGS: &str = r#"{"service":"svc","environment":"prod","instrumentation_type":"breakpoint","location_hash":"0123456789abcdef","start_time":"2025-02-03T18:00:00Z","end_time":"2025-02-03T19:00:00Z"}"#;

const BATCH_RESPONSE: &str = r#"{"DeletedCount": 1,
    "SuccessfulDeletions": [{"ResourceArn": "arn:x", "SignalType": "SNAPSHOT", "LocationHash": "0123456789abcdef"}],
    "Errors": [{"ResourceArn": "arn:y", "Code": "ConflictException", "Message": "busy"}]}"#;

/// A batch deletion without per-item errors: the command succeeds.
const BATCH_OK_RESPONSE: &str = r#"{"DeletedCount": 1,
    "SuccessfulDeletions": [{"ResourceArn": "arn:x", "SignalType": "SNAPSHOT", "LocationHash": "0123456789abcdef"}],
    "Errors": []}"#;

/// The consolidated check: `GetInstrumentationConfiguration`, then one
/// status answer per query (ACTIVE, READY, ERROR, as far as it goes).
fn check_status_recording(events: &[&str]) -> String {
    let mut steps = vec![appsignals(
        "GetInstrumentationConfiguration",
        r#"{"Configuration": {"CreatedAt": "2025-02-03T17:00:00Z"}}"#,
    )];
    for (status, list) in ["ACTIVE", "READY", "ERROR"].iter().zip(events) {
        steps.push(format!(
            r#"{{"service":"application-signals","operation":"GetInstrumentationConfigurationStatus","request":{{"Status":"{status}"}},"response":{{"Events":{list}}}}}"#
        ));
    }
    calls(&steps)
}

/// A snapshot message as the Logs Insights `@message` field carries it.
const SNAPSHOT_MESSAGE: &str = r#"{\"traceId\":\"t1\",\"spanId\":\"s1\",\"timeUnixNano\":\"1738608120000000000\",\"attributes\":{\"aws.di.snapshot_id\":\"snap-1\",\"aws.di.location_hash\":\"0123456789abcdef\"},\"body\":{\"captures\":{\"entry\":{\"arguments\":{\"amount\":{\"type\":\"int\",\"value\":\"5\"}}}}}}"#;

fn rich_query_recording() -> String {
    format!(
        r#"{{"calls":[
{{"service":"logs","operation":"StartQuery","response":{{"queryId":"q-1"}}}},
{{"service":"logs","operation":"GetQueryResults","response":{{"status":"Complete",
  "results":[[{{"field":"@timestamp","value":"2025-02-03 18:42:00.000"}},{{"field":"@message","value":"{SNAPSHOT_MESSAGE}"}}]],
  "messages":[{{"queryId":"q-1","text":"partial"}}]}}}}]}}"#
    )
}

/// The Omni steps with a two-page row fetch, so `nextToken` is replayed.
/// The first query reads both pages; any later query of the same run gets
/// the last, token-free page.
fn omni_paged(rows: &str) -> String {
    format!(
        r#"{{"service":"cloudwatchomni","operation":"StartTelemetryQuerySession","response":{{"sessionId":"s"}}}},
{{"service":"cloudwatchomni","operation":"StartTelemetryQuery","response":{{"queryId":"q"}}}},
{{"service":"cloudwatchomni","operation":"GetTelemetryQueryResults","request":{{"maxResults":1}},"response":{{"status":"Complete"}}}},
{{"service":"cloudwatchomni","operation":"GetTelemetryQueryResults","request":{{"maxResults":1000,"nextToken":"p2"}},"response":{{"rows":[]}}}},
{{"service":"cloudwatchomni","operation":"GetTelemetryQueryResults","request":{{"maxResults":1000}},"response":{{"rows":[{rows}],"nextToken":"p2"}}}},
{{"service":"cloudwatchomni","operation":"GetTelemetryQueryResults","request":{{"maxResults":1000}},"response":{{"rows":[{rows}]}}}},
{{"service":"cloudwatchomni","operation":"StopTelemetryQuerySession","response":{{}}}}"#
    )
}

/// A span row with every column the evaluation and capture read.
const RICH_ROW: &str = r#"{"traceId":"aa","spanId":"0000000000000001","parentSpanId":"","name":"root","kind":2,
    "startTimeUnixNano":"1738608120000000000","endTimeUnixNano":"1738608121000000000",
    "svc_name":"svc","scope_name":"strands","status_code":"OK","session_id":"session-1",
    "input_value":"hello","output_value":"world"}"#;

/// The evaluation of `aa` with writeback: the group is missing, so it is
/// created (`kms_error`: the creation fails on the key; otherwise the
/// retention call fails), then the scores are written.
fn writeback_recording(kms_error: bool) -> String {
    let provision = if kms_error {
        r#"{"service":"logs","operation":"CreateLogGroup","error":{"code":"InvalidParameterException","message":"bad key"}}"#
    } else {
        r#"{"service":"logs","operation":"CreateLogGroup","response":{}},
{"service":"logs","operation":"PutRetentionPolicy","error":{"code":"AccessDeniedException","message":"no"}},
{"service":"logs","operation":"CreateLogStream","response":{}}"#
    };
    format!(
        r#"{{"calls":[{},
{{"service":"bedrock-agentcore","operation":"Evaluate","response":{{"evaluationResults":[{{"evaluatorId":"Builtin.Helpfulness","value":1,"label":"Helpful","explanation":"why"}}]}}}},
{{"service":"logs","operation":"PutLogEvents","error":{{"code":"ResourceNotFoundException","message":"gone"}}}},
{provision},
{{"service":"logs","operation":"PutLogEvents","response":{{}}}}]}}"#,
        omni_paged(RICH_ROW)
    )
}

/// An evaluation whose results carry every member `EVALUATION_RESULT`
/// reads, one scored and one an error row.
fn rich_evaluate_recording() -> String {
    format!(
        r#"{{"calls":[{},
{{"service":"bedrock-agentcore","operation":"Evaluate","response":{{"evaluationResults":[
  {{"evaluatorId":"Builtin.Helpfulness","value":0.5,"label":"Somewhat","explanation":"why"}},
  {{"evaluatorId":"Builtin.Helpfulness","value":null,"errorCode":"Throttled","errorMessage":"slow"}}]}}}}]}}"#,
        omni_paged(RICH_ROW)
    )
}

/// `--mode add` by name: two listing pages, a count and the write.
/// `count` is the dataset's current example count.
fn add_recording(count: i64) -> String {
    format!(
        r#"{{"calls":[{},
{{"service":"bedrock-agentcore-control","operation":"ListDatasets","request":{{"nextToken":"d2"}},"response":{{"datasets":[{{"datasetName":"fixture","datasetId":"RESOLVED_ID"}}]}}}},
{{"service":"bedrock-agentcore-control","operation":"ListDatasets","response":{{"datasets":[{{"datasetName":"other","datasetId":"OTHER_ID"}}],"nextToken":"d2"}}}},
{{"service":"bedrock-agentcore-control","operation":"GetDataset","response":{{"exampleCount":{count}}}}},
{{"service":"bedrock-agentcore-control","operation":"AddDatasetExamples","response":{{"exampleIds":["e1"]}}}}]}}"#,
        omni_paged(RICH_ROW)
    )
}

/// One command run over one recording.
struct Scenario {
    /// The command as `HARNESS_ALLOWED` names it.
    command: &'static str,
    /// The arguments after `cloudwatch`; `@REC` is the recording path.
    argv: Vec<String>,
    recording: String,
    /// The exit status of the unmutated run.
    expect: i32,
    /// For a malformed fixture (`expect` 1): the generic paths whose
    /// mutation may legitimately repair it, so the run succeeds.
    repairs: Vec<&'static str>,
}

fn scenario(command: &'static str, argv: &[&str], recording: impl Into<String>) -> Scenario {
    Scenario {
        command,
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        recording: recording.into(),
        expect: 0,
        repairs: Vec::new(),
    }
}

impl Scenario {
    /// The unmutated run fails (exit 1): a malformed or refused fixture.
    fn failing(mut self) -> Self {
        self.expect = 1;
        self
    }

    /// A mutation of `path` may make the malformed fixture well-formed.
    fn repaired_by(mut self, path: &'static str) -> Self {
        self.repairs.push(path);
        self
    }
}

/// Every recording replayed by the command tests above, and the richer
/// fixtures of this harness.
#[allow(clippy::too_many_lines)]
fn harness_scenarios() -> Vec<Scenario> {
    let list = [
        "instrumentation",
        "list",
        "--json",
        SCOPE_ARGS,
        "--recorded",
        "@REC",
    ];
    let create = [
        "instrumentation",
        "create",
        "--json",
        CREATE_ARGS,
        "--recorded",
        "@REC",
        "--now",
        "2025-02-03T18:42:00Z",
        "--i-have-confirmed",
    ];
    let get = [
        "instrumentation",
        "get",
        "--json",
        GET_ARGS,
        "--recorded",
        "@REC",
    ];
    let delete = [
        "instrumentation",
        "delete",
        "--json",
        GET_ARGS,
        "--recorded",
        "@REC",
        "--i-have-confirmed",
    ];
    let by_scope = [
        "instrumentation",
        "batch-delete-by-scope",
        "--json",
        SCOPE_ARGS,
        "--recorded",
        "@REC",
        "--i-have-confirmed",
    ];
    let by_arns = [
        "instrumentation",
        "batch-delete-by-arns",
        "--json",
        r#"{"instrumentation_type":"BREAKPOINT","resource_arns":["arn:x","arn:y"]}"#,
        "--recorded",
        "@REC",
        "--i-have-confirmed",
    ];
    let get_status = [
        "instrumentation",
        "get-status",
        "--json",
        STATUS_ARGS,
        "--recorded",
        "@REC",
    ];
    let check = [
        "instrumentation",
        "check-status",
        "--json",
        CHECK_ARGS,
        "--recorded",
        "@REC",
    ];
    let sample = [
        "snapshots",
        "sample",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        "@REC",
    ];
    let search = [
        "snapshots",
        "search",
        "--json",
        SAMPLE_ARGS,
        "--recorded",
        "@REC",
    ];
    let evaluate = [
        "evaluate-traces",
        "--trace-id",
        "aa",
        "--no-writeback",
        "--recorded",
        "@REC",
        "--now",
        "2025-02-03T18:42:00Z",
    ];
    let explained = [
        "evaluate-traces",
        "--trace-id",
        "aa",
        "--no-writeback",
        "--include-explanations",
        "--recorded",
        "@REC",
        "--now",
        "2025-02-03T18:42:00Z",
    ];
    let writeback = [
        "evaluate-traces",
        "--trace-id",
        "aa",
        "--session-id",
        "session-1",
        "--kms-key-id",
        "k",
        "--recorded",
        "@REC",
        "--now",
        "2025-02-03T18:42:00Z",
    ];
    let capture = [
        "capture-dataset",
        "--trace-ids",
        "aa",
        "--dataset-name",
        "golden",
        "--i-have-confirmed",
        "--recorded",
        "@REC",
    ];
    let add = [
        "capture-dataset",
        "--trace-ids",
        "aa,bb",
        "--mode",
        "add",
        "--dataset-name",
        "fixture",
        "--i-have-confirmed",
        "--recorded",
        "@REC",
    ];

    let configuration = format!(r#"{{"Configuration": {RICH_CONFIGURATION}}}"#);
    let listing = format!(
        r#"{{"LatestConfigurations": [{RICH_CONFIGURATION}], "SyncedAt": "2025-02-03T18:00:00Z", "NextToken": "page-2"}}"#
    );
    let status_response = r#"{"Events": [{"Time": "2025-02-03T18:40:00Z", "Status": "READY", "ErrorCause": "none"}],
        "Service": "svc", "Environment": "prod", "SignalType": "SNAPSHOT", "Status": "READY",
        "Location": {"CodeLocation": {"Language": "PYTHON", "FilePath": "/app/h.py", "LineNumber": 12}},
        "LocationHash": "0123456789abcdef", "NextToken": "n"}"#;
    let event = |status: &str| {
        format!(
            r#"[{{"Time": "2025-02-03T18:40:00Z", "Status": "{status}", "ErrorCause": "agent crashed"}}]"#
        )
    };

    let mut scenarios = vec![
        // Committed command-test recordings.
        scenario("instrumentation list", &list, LIST_RECORDING),
        scenario("instrumentation delete", &delete, LIST_RECORDING),
        scenario("instrumentation create", &create, CREATE_RECORDING),
        scenario("snapshots sample", &sample, query_recording("Complete")),
        scenario("snapshots sample", &sample, query_recording("Failed")).failing(),
        scenario("snapshots search", &search, query_recording("Complete")),
        scenario(
            "snapshots sample",
            &sample,
            missing_query_id_recording("absent"),
        )
        .failing(),
        scenario(
            "snapshots search",
            &search,
            missing_query_id_recording("empty"),
        )
        .failing(),
        scenario("evaluate-traces", &evaluate, TRACES_RECORDING),
        scenario(
            "evaluate-traces --include-explanations",
            &explained,
            TRACES_RECORDING,
        ),
        // A `null` score is a valid unscored row: the receipt exits 0 with
        // `pairsScored: 0` (Python parity), never a score.
        scenario("evaluate-traces", &evaluate, object_score_recording())
            .failing()
            .repaired_by("evaluationResults[*].value"),
        scenario("capture-dataset create", &capture, CAPTURE_CREATE_RECORDING),
        scenario(
            "capture-dataset create",
            &capture,
            malformed_create_dataset_recording(),
        )
        .failing(),
        scenario("capture-dataset add", &add, malformed_listing_recording()).failing(),
        // Rich fixtures: every member each command reads.
        scenario(
            "instrumentation list",
            &list,
            calls(&[appsignals("ListInstrumentationConfigurations", &listing)]),
        ),
        scenario(
            "instrumentation get",
            &get,
            calls(&[appsignals(
                "GetInstrumentationConfiguration",
                &configuration,
            )]),
        ),
        scenario(
            "instrumentation create",
            &create,
            calls(&[appsignals(
                "CreateInstrumentationConfiguration",
                r#"{"LocationHash": "0123456789abcdef", "ARN": "arn:aws:x", "CreatedAt": "2025-02-03T18:42:00Z", "ExpiresAt": "2025-02-03T20:42:00Z"}"#,
            )]),
        ),
        scenario(
            "instrumentation batch-delete-by-scope",
            &by_scope,
            calls(&[appsignals(
                "BatchDeleteInstrumentationConfigurations",
                BATCH_RESPONSE,
            )]),
        )
        .failing(),
        scenario(
            "instrumentation batch-delete-by-scope",
            &by_scope,
            calls(&[appsignals(
                "BatchDeleteInstrumentationConfigurations",
                BATCH_OK_RESPONSE,
            )]),
        ),
        scenario(
            "instrumentation batch-delete-by-arns",
            &by_arns,
            calls(&[appsignals(
                "BatchDeleteInstrumentationConfigurations",
                BATCH_RESPONSE,
            )]),
        )
        .failing(),
        scenario(
            "instrumentation batch-delete-by-arns",
            &by_arns,
            calls(&[appsignals(
                "BatchDeleteInstrumentationConfigurations",
                BATCH_OK_RESPONSE,
            )]),
        ),
        scenario(
            "instrumentation get-status",
            &get_status,
            calls(&[appsignals(
                "GetInstrumentationConfigurationStatus",
                status_response,
            )]),
        ),
        scenario(
            "instrumentation check-status",
            &check,
            check_status_recording(&[event("ACTIVE").as_str()]),
        ),
        scenario(
            "instrumentation check-status",
            &check,
            check_status_recording(&["[]", event("READY").as_str()]),
        ),
        scenario(
            "instrumentation check-status",
            &check,
            check_status_recording(&["[]", "[]", event("ERROR").as_str()]),
        ),
        // #4818: PENDING is only the conclusion of three valid empty answers.
        scenario(
            "instrumentation check-status",
            &check,
            check_status_recording(&["[]", "[]", "[]"]),
        ),
        scenario("snapshots sample", &sample, rich_query_recording()),
        scenario("snapshots search", &search, rich_query_recording()),
        scenario("evaluate-traces", &evaluate, rich_evaluate_recording()),
        // #4817: both writeback diagnostics.
        scenario(
            "evaluate-traces --kms-key-id",
            &writeback,
            writeback_recording(true),
        ),
        scenario(
            "evaluate-traces --kms-key-id",
            &writeback,
            writeback_recording(false),
        ),
        scenario("capture-dataset add", &add, add_recording(10)),
        // #4816: the cap refusal of a dataset resolved by name.
        scenario("capture-dataset add", &add, add_recording(1000)).failing(),
    ];
    for (recording, command, args, _) in MALFORMED_APPSIGNALS {
        let mut argv: Vec<&str> = command.to_vec();
        argv.extend_from_slice(&["--json", args, "--recorded", "@REC"]);
        let label = if command.contains(&"get-status") {
            "instrumentation get-status"
        } else if command.contains(&"list") {
            "instrumentation list"
        } else {
            "instrumentation batch-delete-by-scope"
        };
        scenarios.push(scenario(label, &argv, recording).failing());
    }
    for results in WRONG_RESULTS {
        // `{}` becomes `[]` as its wrong-typed mutation: a valid empty list.
        scenarios.push(
            scenario(
                "snapshots sample",
                &sample,
                wrong_results_recording(results),
            )
            .failing()
            .repaired_by("results"),
        );
    }
    scenarios
}

/// The three replacements of one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    Sentinel,
    Null,
    WrongType,
}

use Mutation::{Null as MNull, Sentinel as MSentinel, WrongType as MWrongType};

/// A (command, operation, member path) whose changed value may change a
/// successful run's stdout, and why. `[*]` is any list index; a path that
/// ends in `**` covers every member under it. `on_failure` also lets the
/// value appear on the stdout of a failing run (a partial-failure report).
struct Allowed {
    commands: &'static [&'static str],
    operation: &'static str,
    path: &'static str,
    kinds: &'static [Mutation],
    on_failure: bool,
    why: &'static str,
}

const SHOWN: &[Mutation] = &[MSentinel];
/// An integer replacing a timestamp string is epoch seconds: a valid
/// timestamp, so the run renders that time.
const EPOCH: &[Mutation] = &[MWrongType];
const NULL: &[Mutation] = &[MNull];

const LIST: &[&str] = &["instrumentation list"];
const GET: &[&str] = &["instrumentation get"];
const CREATE: &[&str] = &["instrumentation create"];
const BATCH: &[&str] = &[
    "instrumentation batch-delete-by-scope",
    "instrumentation batch-delete-by-arns",
];
const STATUS: &[&str] = &["instrumentation get-status"];
const CHECK: &[&str] = &["instrumentation check-status"];
const SNAPSHOTS: &[&str] = &["snapshots sample", "snapshots search"];
const EVALUATE: &[&str] = &[
    "evaluate-traces",
    "evaluate-traces --include-explanations",
    "evaluate-traces --kms-key-id",
];

const fn allow(
    commands: &'static [&'static str],
    operation: &'static str,
    path: &'static str,
    kinds: &'static [Mutation],
    why: &'static str,
) -> Allowed {
    Allowed {
        commands,
        operation,
        path,
        kinds,
        on_failure: false,
        why,
    }
}

/// The documented exceptions (docs/migration/cw-api.md item 23). Every
/// other changed member must fail the run or leave its output unchanged.
const HARNESS_ALLOWED: &[Allowed] = &[
    // Listings, lookups and receipts print the configuration they read.
    allow(
        LIST,
        "ListInstrumentationConfigurations",
        "LatestConfigurations[*].**",
        SHOWN,
        "the listing renders each configuration",
    ),
    allow(
        LIST,
        "ListInstrumentationConfigurations",
        "LatestConfigurations[*].CreatedAt",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        LIST,
        "ListInstrumentationConfigurations",
        "LatestConfigurations[*].ExpiresAt",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        LIST,
        "ListInstrumentationConfigurations",
        "SyncedAt",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        LIST,
        "ListInstrumentationConfigurations",
        "NextToken",
        SHOWN,
        "the token is printed for the caller to pass back as next_token",
    ),
    allow(
        LIST,
        "ListInstrumentationConfigurations",
        "NextToken",
        NULL,
        "a null NextToken means no further page (NullableText, #4727)",
    ),
    allow(
        GET,
        "GetInstrumentationConfiguration",
        "Configuration.**",
        SHOWN,
        "get renders the configuration",
    ),
    allow(
        GET,
        "GetInstrumentationConfiguration",
        "Configuration.CreatedAt",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        GET,
        "GetInstrumentationConfiguration",
        "Configuration.ExpiresAt",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        CREATE,
        "CreateInstrumentationConfiguration",
        "LocationHash",
        SHOWN,
        "the receipt names the created configuration",
    ),
    allow(
        CREATE,
        "CreateInstrumentationConfiguration",
        "ARN",
        SHOWN,
        "the receipt names the created configuration",
    ),
    allow(
        CREATE,
        "CreateInstrumentationConfiguration",
        "CreatedAt",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        CREATE,
        "CreateInstrumentationConfiguration",
        "ExpiresAt",
        EPOCH,
        "epoch seconds",
    ),
    // The batch report lists each deletion and each per-item error; with
    // any error the command exits 1, and the report is still its output.
    Allowed {
        commands: BATCH,
        operation: "BatchDeleteInstrumentationConfigurations",
        path: "SuccessfulDeletions[*].ResourceArn",
        kinds: SHOWN,
        on_failure: true,
        why: "the batch report lists the deleted ARNs",
    },
    Allowed {
        commands: BATCH,
        operation: "BatchDeleteInstrumentationConfigurations",
        path: "Errors[*].**",
        kinds: SHOWN,
        on_failure: true,
        why: "the batch report lists each failed ARN with the service's own error code and message",
    },
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "Events[*].ErrorCause",
        SHOWN,
        "the status report prints each event's cause",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "Events[*].Time",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "Service",
        SHOWN,
        "the status report header",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "Environment",
        SHOWN,
        "the status report header",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "SignalType",
        SHOWN,
        "the status report header",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "Status",
        SHOWN,
        "the status report header",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "LocationHash",
        SHOWN,
        "the status report header",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "Location.**",
        SHOWN,
        "the status report header",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "NextToken",
        SHOWN,
        "the token is printed for the caller to pass back as next_token",
    ),
    allow(
        STATUS,
        "GetInstrumentationConfigurationStatus",
        "NextToken",
        NULL,
        "a null NextToken means no further page (NullableText, #4727)",
    ),
    allow(
        CHECK,
        "GetInstrumentationConfigurationStatus",
        "Events[*].ErrorCause",
        SHOWN,
        "the ERROR verdict prints the cause of the latest error event",
    ),
    allow(
        CHECK,
        "GetInstrumentationConfigurationStatus",
        "Events[*].Time",
        EPOCH,
        "epoch seconds",
    ),
    allow(
        CHECK,
        "GetInstrumentationConfiguration",
        "Configuration.CreatedAt",
        EPOCH,
        "epoch seconds: the ACTIVE window is clamped to that time",
    ),
    // Snapshot commands print the records and the query receipt.
    allow(
        SNAPSHOTS,
        "GetQueryResults",
        "results[*][*].value",
        SHOWN,
        "the snapshot record is the data the command prints",
    ),
    allow(
        SNAPSHOTS,
        "GetQueryResults",
        "results[*][*].field",
        SHOWN,
        "the record's field names are part of the printed record",
    ),
    allow(
        SNAPSHOTS,
        "GetQueryResults",
        "messages[*].**",
        SHOWN,
        "the service's query messages are passed through, as in Python",
    ),
    Allowed {
        commands: SNAPSHOTS,
        operation: "StartQuery",
        path: "queryId",
        kinds: SHOWN,
        on_failure: true,
        why: "the query receipt carries its queryId on success and failure, as in Python",
    },
    // Evaluation and dataset receipts.
    allow(
        EVALUATE,
        "Evaluate",
        "evaluationResults[*].evaluatorId",
        SHOWN,
        "each score row names its evaluator",
    ),
    allow(
        EVALUATE,
        "Evaluate",
        "evaluationResults[*].label",
        SHOWN,
        "each score row carries its label",
    ),
    allow(
        EVALUATE,
        "Evaluate",
        "evaluationResults[*].value",
        NULL,
        "a null value is an unscored row (NullableNumber, #4758)",
    ),
    allow(
        EVALUATE,
        "Evaluate",
        "evaluationResults[*].errorCode",
        SHOWN,
        "an error row carries the service's own error code",
    ),
    allow(
        EVALUATE,
        "Evaluate",
        "evaluationResults[*].errorMessage",
        SHOWN,
        "an error row carries the service's own error message",
    ),
    allow(
        &["evaluate-traces --include-explanations"],
        "Evaluate",
        "evaluationResults[*].explanation",
        SHOWN,
        "--include-explanations asks for the judge's explanation",
    ),
    allow(
        &["capture-dataset create"],
        "CreateDataset",
        "datasetId",
        SHOWN,
        "the receipt names the created dataset",
    ),
    allow(
        &["capture-dataset create"],
        "CreateDataset",
        "status",
        SHOWN,
        "the receipt carries its status",
    ),
    allow(
        &["capture-dataset add"],
        "ListDatasets",
        "datasets[*].datasetId",
        SHOWN,
        "a successful add's receipt names the dataset written (never a failure, #4816)",
    ),
    allow(
        &["capture-dataset add"],
        "AddDatasetExamples",
        "exampleIds[*]",
        SHOWN,
        "the receipt lists the written example ids",
    ),
];

/// The allowlist entry for a changed member, if any.
fn allowed(
    command: &str,
    operation: &str,
    path: &str,
    kind: Mutation,
    failing: bool,
) -> Option<&'static str> {
    let generic = generic_path(path);
    HARNESS_ALLOWED
        .iter()
        .find(|entry| {
            entry.commands.contains(&command)
                && entry.operation == operation
                && entry.kinds.contains(&kind)
                && (!failing || entry.on_failure)
                && match entry.path.strip_suffix("**") {
                    Some(prefix) => generic.starts_with(prefix),
                    None => generic == entry.path,
                }
        })
        .map(|entry| entry.why)
}

/// `rows[0].name` -> `rows[*].name`.
fn generic_path(path: &str) -> String {
    let mut out = String::new();
    let mut in_index = false;
    for c in path.chars() {
        match c {
            '[' => {
                in_index = true;
                out.push_str("[*");
            }
            ']' => {
                in_index = false;
                out.push(']');
            }
            _ if in_index => {}
            _ => out.push(c),
        }
    }
    out
}

/// One step of a member path.
#[derive(Debug, Clone)]
enum Seg {
    Key(String),
    Index(usize),
}

fn show_path(path: &[Seg]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            Seg::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Seg::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }
    out
}

/// Every member path under `value` (members of objects, items of lists).
fn member_paths(value: &Json, prefix: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    match value {
        Json::Object(members) => {
            for (key, member) in members {
                prefix.push(Seg::Key(key.clone()));
                out.push(prefix.clone());
                member_paths(member, prefix, out);
                prefix.pop();
            }
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                prefix.push(Seg::Index(index));
                out.push(prefix.clone());
                member_paths(item, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

fn slot<'j>(value: &'j mut Json, path: &[Seg]) -> &'j mut Json {
    path.iter().fold(value, |value, seg| match (seg, value) {
        (Seg::Key(key), Json::Object(members)) => {
            &mut members
                .iter_mut()
                .find(|(name, _)| name == key)
                .expect("path member")
                .1
        }
        (Seg::Index(index), Json::Array(items)) => &mut items[*index],
        _ => panic!("path does not fit"),
    })
}

fn replacement(kind: Mutation, original: &Json, sentinel: &str) -> Json {
    match kind {
        Mutation::Sentinel => Json::String(sentinel.to_string()),
        Mutation::Null => Json::Null,
        Mutation::WrongType => match original {
            Json::String(_) => Json::Int(424_242),
            Json::Int(_) | Json::Float(_) => Json::Bool(true),
            Json::Bool(_) => Json::Int(1),
            Json::Object(_) => Json::Array(Vec::new()),
            Json::Array(_) => Json::Object(Vec::new()),
            Json::Null => Json::Object(Vec::new()),
        },
    }
}

/// Text a successful run may not newly print with exit 0: a non-terminal
/// state concluded from a changed response.
const NON_TERMINAL: &[&str] = &[
    "OVERALL STATUS: PENDING",
    "\"status\": \"Running\"",
    "\"status\": \"Scheduled\"",
    "\"status\": \"Polling Timeout\"",
];

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_scenario(scenario: &Scenario, recording: &Path) -> Run {
    let path = recording.display().to_string();
    let argv: Vec<&str> = std::iter::once("cloudwatch")
        .chain(scenario.argv.iter().map(|arg| {
            if arg == "@REC" {
                path.as_str()
            } else {
                arg.as_str()
            }
        }))
        .collect();
    let output = toolkit(&argv);
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// One mutated run to make: scenario, call index, operation, path, kind.
struct Case {
    scenario: usize,
    call: usize,
    operation: String,
    path: Vec<Seg>,
    kind: Mutation,
}

/// The violations of one mutated run, if any.
fn judge(
    scenario: &Scenario,
    case: &Case,
    baseline: &Run,
    run: &Run,
    sentinel: &str,
) -> Vec<String> {
    let path = show_path(&case.path);
    let at = format!(
        "{} | {} {} | {:?}",
        scenario.command, case.operation, path, case.kind
    );
    let mut problems = Vec::new();
    let shows = |text: &str| text.to_lowercase().contains(&sentinel.to_lowercase());
    if run.code == 101 || run.stderr.contains("panicked") {
        problems.push(format!("{at}: panicked: {}", run.stderr));
    }
    if shows(&run.stderr) {
        problems.push(format!("{at}: the sentinel is on stderr: {}", run.stderr));
    }
    if run.code != 0 {
        let reported = allowed(scenario.command, &case.operation, &path, case.kind, true);
        if shows(&run.stdout) && reported.is_none() {
            problems.push(format!(
                "{at}: a failing run (exit {}) prints the sentinel: {}",
                run.code, run.stdout
            ));
        }
        return problems;
    }
    for marker in NON_TERMINAL {
        if run.stdout.contains(marker) && !baseline.stdout.contains(marker) {
            problems.push(format!(
                "{at}: exit 0 with a non-terminal state {marker:?}: {}",
                run.stdout
            ));
        }
    }
    if baseline.code != 0 {
        // A malformed fixture may only be repaired by its declared member.
        if !scenario.repairs.contains(&generic_path(&path).as_str()) {
            problems.push(format!(
                "{at}: the malformed fixture (exit {}) now succeeds: {}",
                baseline.code, run.stdout
            ));
        }
        return problems;
    }
    if run.stdout == baseline.stdout {
        return problems;
    }
    match allowed(scenario.command, &case.operation, &path, case.kind, false) {
        Some(_) => {}
        None if shows(&run.stdout) => problems.push(format!(
            "{at}: a successful run prints the response value (not allowlisted): {}",
            run.stdout
        )),
        None => problems.push(format!(
            "{at}: exit 0 with changed output (accepted, not allowlisted):\n--- baseline\n{}\n--- mutated\n{}",
            baseline.stdout, run.stdout
        )),
    }
    problems
}

/// See the section comment. Ignored by nothing: it is the slice's guard
/// against response values in diagnostics and malformed success.
#[test]
fn every_recorded_response_member_fails_closed_or_is_documented() {
    let root = temp_root("mutation");
    let scenarios = harness_scenarios();

    // The unmutated runs, twice, so the comparison is against stable output.
    let mut baselines = Vec::new();
    let mut cases = Vec::new();
    let mut unfit = Vec::new();
    for (index, scenario) in scenarios.iter().enumerate() {
        let file = root.join(format!("base-{index}.json"));
        fs::write(&file, &scenario.recording).expect("write baseline");
        let first = run_scenario(scenario, &file);
        let second = run_scenario(scenario, &file);
        if (first.code, &first.stdout) != (second.code, &second.stdout) {
            unfit.push(format!(
                "scenario {index} ({}) is not deterministic",
                scenario.command
            ));
        }
        if first.code != scenario.expect || first.stdout.contains("no recorded response") {
            unfit.push(format!(
                "scenario {index} ({}) exits {} unmutated (expected {}): {}{}",
                scenario.command, first.code, scenario.expect, first.stdout, first.stderr
            ));
        }
        baselines.push(first);

        let document = parse_json(&scenario.recording).expect("recording is JSON");
        let Some(Json::Array(steps)) = document.get("calls") else {
            panic!("scenario {index}: no calls");
        };
        for (call, step) in steps.iter().enumerate() {
            let Some(response) = step.get("response") else {
                continue;
            };
            let operation = step
                .get("operation")
                .and_then(Json::as_str)
                .expect("operation")
                .to_string();
            let mut paths = Vec::new();
            member_paths(response, &mut Vec::new(), &mut paths);
            for path in paths {
                for kind in [MSentinel, MNull, MWrongType] {
                    cases.push(Case {
                        scenario: index,
                        call,
                        operation: operation.clone(),
                        path: path.clone(),
                        kind,
                    });
                }
            }
        }
    }
    assert!(
        unfit.is_empty(),
        "fixtures that do not replay as declared:\n\n{}",
        unfit.join("\n\n")
    );
    assert!(
        cases.len() > 500,
        "the table covers {} runs only",
        cases.len()
    );

    let next = AtomicUsize::new(0);
    let problems = std::sync::Mutex::new(Vec::<String>::new());
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(case) = cases.get(index) else {
                    break;
                };
                let scenario = &scenarios[case.scenario];
                let sentinel = format!("MUTSENTINEL{index:05}X");
                let mut document = parse_json(&scenario.recording).expect("recording");
                let Some(Json::Array(steps)) = document_calls(&mut document) else {
                    unreachable!()
                };
                let response = match &mut steps[case.call] {
                    Json::Object(members) => {
                        &mut members
                            .iter_mut()
                            .find(|(key, _)| key == "response")
                            .expect("response")
                            .1
                    }
                    _ => unreachable!(),
                };
                let target = slot(response, &case.path);
                let new = replacement(case.kind, target, &sentinel);
                if new == *target {
                    continue;
                }
                *target = new;
                let file = root.join(format!("m-{index}.json"));
                fs::write(&file, document.to_compact_string()).expect("write mutation");
                let run = run_scenario(scenario, &file);
                let _ = fs::remove_file(&file);
                let found = judge(scenario, case, &baselines[case.scenario], &run, &sentinel);
                if !found.is_empty() {
                    problems.lock().expect("lock").extend(found);
                }
            });
        }
    });
    let mut problems = problems.into_inner().expect("problems");
    problems.sort();
    assert!(
        problems.is_empty(),
        "{} of {} mutated runs violate the contract:\n\n{}",
        problems.len(),
        cases.len(),
        problems.join("\n\n")
    );
}

fn document_calls(document: &mut Json) -> Option<&mut Json> {
    match document {
        Json::Object(members) => members
            .iter_mut()
            .find(|(key, _)| key == "calls")
            .map(|(_, calls)| calls),
        _ => None,
    }
}
