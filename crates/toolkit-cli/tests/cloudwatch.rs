//! End-to-end tests of `toolkit cloudwatch ...`: real process spawns, real
//! exit codes and separated stdout/stderr. Fixtures are literals and temp
//! files; no network or AWS call is involved.

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

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
        "toolkit-cloudwatch-cli-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn toolkit(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .output()
        .expect("spawn toolkit")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

#[test]
fn render_status_history_from_a_file() {
    let root = temp_root("history");
    let path = root.join("args.json");
    fs::write(
        &path,
        r#"{
            "data": {
                "Service": "svc2",
                "Status": "ACTIVE",
                "Events": [{"Time": "2025-02-03T10:05:00Z"}],
                "Location": {"CodeLocation": {"Language": "Python", "FilePath": "/a.py", "CodeUnit": "m", "MethodName": "f"}},
                "LocationHash": "0123456789abcdef"
            },
            "normalized_type": "BREAKPOINT",
            "service": "svc",
            "environment": "prod",
            "requested_status": "ACTIVE"
        }"#,
    )
    .expect("write arguments");
    let output = toolkit(&[
        "cloudwatch",
        "render",
        "status-history",
        "--json-file",
        path.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("INSTRUMENTATION STATUS\n\nTYPE: BREAKPOINT\nSERVICE: svc2\n"));
    assert!(stdout.contains("- Status Confirmation: CONFIRMED (ACTIVE events present)\n"));
    assert!(stdout.ends_with("STATUS EVENTS:\n- Event 1: 2025-02-03T10:05:00Z\n\n"));
}

#[test]
fn validate_failure_is_reported_on_stdout_with_exit_one() {
    let output = toolkit(&[
        "cloudwatch",
        "validate",
        "capture-names",
        "--json",
        r#"{"field_name":"capture_arguments","names":[]}"#,
    ]);
    assert_eq!(status(&output), 1);
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        text(&output.stdout),
        "ERROR: capture_arguments must contain at least one name if provided. Omit it to capture none.\n"
    );
}

#[test]
fn argument_errors_exit_two_with_stderr_only() {
    let unknown = toolkit(&[
        "cloudwatch",
        "validate",
        "location-hash",
        "--json",
        r#"{"location_hash":"0123456789abcdef","extra":true}"#,
    ]);
    assert_eq!(status(&unknown), 2);
    assert_eq!(text(&unknown.stdout), "");
    assert_eq!(
        text(&unknown.stderr),
        "error: invalid arguments for op 'location-hash': got an unexpected argument 'extra'\n"
    );

    let missing = toolkit(&["cloudwatch", "validate", "location-hash"]);
    assert_eq!(status(&missing), 2);
    assert_eq!(text(&missing.stdout), "");
    assert!(text(&missing.stderr).contains("the op's arguments are required"));

    let bad_choice = toolkit(&["cloudwatch", "render", "bogus", "--json", "{}"]);
    assert_eq!(status(&bad_choice), 2);
    assert!(text(&bad_choice.stderr).contains("invalid choice: 'bogus'"));
}

#[test]
fn region_follows_the_documented_precedence_and_never_prints_credentials() {
    let secret = "AKIAEXAMPLESECRETVALUE0000";
    let run = |args: &[&str], region: Option<(&str, &str)>| {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .stdin(Stdio::null())
            .env_remove("AWS_REGION")
            .env_remove("AWS_DEFAULT_REGION")
            .env("AWS_SECRET_ACCESS_KEY", secret)
            .env("AWS_SESSION_TOKEN", secret);
        if let Some((name, value)) = region {
            command.env(name, value);
        }
        command.output().expect("spawn toolkit")
    };

    let default = run(&["cloudwatch", "region"], None);
    assert_eq!(status(&default), 0);
    assert_eq!(text(&default.stdout), "us-east-1\n");

    let default_region = run(
        &["cloudwatch", "region"],
        Some(("AWS_DEFAULT_REGION", "ap-south-1")),
    );
    assert_eq!(text(&default_region.stdout), "ap-south-1\n");

    let env_region = run(&["cloudwatch", "region"], Some(("AWS_REGION", "eu-west-1")));
    assert_eq!(text(&env_region.stdout), "eu-west-1\n");

    let flag = run(
        &["cloudwatch", "region", "--region", "us-west-2"],
        Some(("AWS_REGION", "eu-west-1")),
    );
    assert_eq!(text(&flag.stdout), "us-west-2\n");

    for output in [&default, &default_region, &env_region, &flag] {
        assert!(!text(&output.stdout).contains(secret));
        assert!(!text(&output.stderr).contains(secret));
    }
}

#[test]
fn help_lists_the_cloudwatch_commands() {
    let output = toolkit(&["--help"]);
    assert_eq!(status(&output), 0);
    let stdout = text(&output.stdout);
    assert!(stdout.contains("toolkit cloudwatch render RENDERER (--json TEXT | --json-file FILE)"));
    assert!(stdout.contains("toolkit cloudwatch validate CHECK (--json TEXT | --json-file FILE)"));
    assert!(stdout.contains("toolkit cloudwatch region [--region REGION]"));
}

// ───────────── #4634: caller text never injects controls or forged lines ─────────────

/// ESC `[2J` clears a terminal; the newline forges a report line.
const HOSTILE: &str = "REVIEW_SENTINEL\u{1b}[2J\nFORGED_REPORT";
const ESCAPED: &str = "REVIEW_SENTINEL\\x1b[2J\\nFORGED_REPORT";
const FAKE_SECRET: &str = "AKIAFAKESECRETSENTINEL0000";

/// JSON source whose string value (or key) is [`HOSTILE`], written with JSON
/// escapes so the argv itself stays printable.
const HOSTILE_JSON: &str = r"REVIEW_SENTINEL\u001b[2J\nFORGED_REPORT";

fn toolkit_with_secret(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .env("AWS_SECRET_ACCESS_KEY", FAKE_SECRET)
        .env("AWS_SESSION_TOKEN", FAKE_SECRET)
        .output()
        .expect("spawn toolkit")
}

/// Neither channel carries a raw control other than the newline, a forged
/// line, or the environment secret.
fn assert_channels_safe(output: &Output) {
    for stream in [text(&output.stdout), text(&output.stderr)] {
        for c in stream.chars() {
            assert!(
                c == '\n' || !c.is_control(),
                "raw control {c:?} in {stream:?}"
            );
        }
        assert!(!stream.contains(FAKE_SECRET));
        assert!(
            stream
                .lines()
                .all(|line| !line.starts_with("FORGED_REPORT")),
            "forged line in {stream:?}"
        );
    }
}

#[test]
fn validate_failure_escapes_the_received_value_and_keeps_stdout_exit_one() {
    let json = format!(r#"{{"instrumentation_type":"{HOSTILE_JSON}"}}"#);
    let output = toolkit_with_secret(&[
        "cloudwatch",
        "validate",
        "instrumentation-type",
        "--json",
        &json,
    ]);
    assert_eq!(status(&output), 1);
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        text(&output.stdout),
        format!(
            "ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: {ESCAPED})\n"
        )
    );
    assert_channels_safe(&output);
}

#[test]
fn unknown_argument_key_is_escaped_on_stderr_with_exit_two() {
    let json = format!(r#"{{"{HOSTILE_JSON}":true}}"#);
    let output = toolkit_with_secret(&["cloudwatch", "validate", "location-hash", "--json", &json]);
    assert_eq!(status(&output), 2);
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: invalid arguments for op 'location-hash': got an unexpected argument '{ESCAPED}'\n"
        )
    );
    assert_channels_safe(&output);
}

#[test]
fn unknown_op_and_unknown_choices_are_escaped_with_exit_two() {
    let unknown_op = toolkit_with_secret(&["cloudwatch", "render", HOSTILE, "--json", "{}"]);
    assert_eq!(status(&unknown_op), 2);
    assert_eq!(text(&unknown_op.stdout), "");
    assert!(text(&unknown_op.stderr).contains(&format!("invalid choice: '{ESCAPED}'")));
    // A usage error appends help text, so only the first line is the diagnostic.
    let first = text(&unknown_op.stderr)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(
        first.contains(&format!("invalid choice: '{ESCAPED}'")),
        "{first}"
    );
    assert_channels_safe(&unknown_op);

    let mode = format!(r#"{{"mode":"{HOSTILE_JSON}"}}"#);
    let bad_mode =
        toolkit_with_secret(&["cloudwatch", "validate", "location-inputs", "--json", &mode]);
    assert_eq!(status(&bad_mode), 2);
    assert_eq!(text(&bad_mode.stdout), "");
    assert!(text(&bad_mode.stderr).contains(&format!("(got '{ESCAPED}')")));
    assert_eq!(text(&bad_mode.stderr).lines().count(), 1);
    assert_channels_safe(&bad_mode);

    let failure = format!(r#"{{"action":"x","failure":{{"kind":"{HOSTILE_JSON}"}}}}"#);
    let bad_kind = toolkit_with_secret(&["cloudwatch", "render", "failure", "--json", &failure]);
    assert_eq!(status(&bad_kind), 2);
    assert_eq!(text(&bad_kind.stdout), "");
    assert!(text(&bad_kind.stderr).contains(&format!("(got '{ESCAPED}')")));
    assert_eq!(text(&bad_kind.stderr).lines().count(), 1);
    assert_channels_safe(&bad_kind);
}

#[test]
fn location_input_failures_escape_every_received_field_on_stdout() {
    let json = format!(
        r#"{{"mode":"create","language":"{HOSTILE_JSON}","file_path":"{HOSTILE_JSON}","code_unit":"{HOSTILE_JSON}","class_name":"{HOSTILE_JSON}","method_name":"{HOSTILE_JSON}","line_number":0}}"#
    );
    let output =
        toolkit_with_secret(&["cloudwatch", "validate", "location-inputs", "--json", &json]);
    assert_eq!(status(&output), 1);
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert!(stdout.contains(&format!("- language={ESCAPED}\n")));
    assert!(stdout.contains(&format!("- file_path={ESCAPED}\n")));
    assert!(stdout.contains(&format!("(received: {ESCAPED}).")));
    assert_channels_safe(&output);
}

#[test]
fn failure_rendering_escapes_message_action_and_context() {
    let json = format!(
        r#"{{"action":"{HOSTILE_JSON}","failure":{{"kind":"client_error","code":"{HOSTILE_JSON}","message":"{HOSTILE_JSON}"}},"attempted":{{"{HOSTILE_JSON}":"{HOSTILE_JSON}"}}}}"#
    );
    let output = toolkit_with_secret(&["cloudwatch", "render", "failure", "--json", &json]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with(&format!("Failed to {ESCAPED}\n")));
    assert!(stdout.contains(&format!("Error: {ESCAPED} - {ESCAPED}\n")));
    assert!(stdout.contains(&format!("- {ESCAPED}: {ESCAPED}\n")));
    assert_channels_safe(&output);
}

#[test]
fn location_rendering_escapes_response_values_and_keys() {
    let json = format!(
        r#"{{"location":{{"CodeLocation":{{"Language":"{HOSTILE_JSON}","FilePath":"/a.py","{HOSTILE_JSON}":"{HOSTILE_JSON}"}}}},"location_hash":"{HOSTILE_JSON}"}}"#
    );
    let output = toolkit_with_secret(&["cloudwatch", "render", "location", "--json", &json]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains(&format!("- LocationHash: {ESCAPED}\n")));
    assert!(stdout.contains(&format!("- Language: {ESCAPED}\n")));
    assert!(stdout.contains(&format!("- {ESCAPED}: {ESCAPED}\n")));
    assert_channels_safe(&output);
}

#[test]
fn region_value_from_the_environment_is_escaped() {
    let output = Command::new(BIN)
        .args(["cloudwatch", "region"])
        .stdin(Stdio::null())
        .env_remove("AWS_DEFAULT_REGION")
        .env("AWS_REGION", HOSTILE)
        .env("AWS_SECRET_ACCESS_KEY", FAKE_SECRET)
        .output()
        .expect("spawn toolkit");
    assert_eq!(status(&output), 0);
    assert_eq!(text(&output.stdout), format!("{ESCAPED}\n"));
    assert_eq!(text(&output.stderr), "");
    assert_channels_safe(&output);
}

// ──── #4652 / #4653: caller context and caller prose stay on one line ────

/// The reviewer's payload: a bare newline that would start a forged line.
const FORGE_JSON: &str = r"REVIEW_SENTINEL\nFORGED_REPORT";
const FORGE_ESCAPED: &str = "REVIEW_SENTINEL\\nFORGED_REPORT";

/// A benign stand-in placed in every caller field of a renderer fixture.
const TOKEN: &str = "CALLERTOKEN";

/// Runs `cloudwatch render RENDERER` on `template` with every `CALLERTOKEN`
/// replaced by `value` (JSON source text), asserting exit 0 and empty stderr.
fn render_with(renderer: &str, template: &str, value: &str) -> String {
    let json = template.replace(TOKEN, value);
    let output = toolkit_with_secret(&["cloudwatch", "render", renderer, "--json", &json]);
    assert_eq!(status(&output), 0, "{renderer}: {}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "", "{renderer}");
    assert_channels_safe(&output);
    text(&output.stdout)
}

/// The hostile run must print exactly the benign run with each token
/// replaced by `shown`: every caller field is escaped onto its own line and
/// nothing else in the report moves.
fn assert_caller_fields_contained(renderer: &str, template: &str, shown: &str) {
    let benign = render_with(renderer, template, TOKEN);
    assert!(benign.contains(TOKEN), "{renderer} fixture prints no token");
    let hostile = render_with(renderer, template, HOSTILE_JSON);
    assert_eq!(hostile, benign.replace(TOKEN, shown), "{renderer}");
    let forged = render_with(renderer, template, FORGE_JSON);
    assert!(!forged.lines().any(|line| line.starts_with("FORGED_REPORT")));
}

#[test]
fn list_render_reproduction_from_4652_is_byte_exact() {
    let json = r#"{"data":{},"normalized_type":"BREAKPOINT","service":"REVIEW_SENTINEL\nFORGED_REPORT","environment":"prod"}"#;
    let output = toolkit_with_secret(&["cloudwatch", "render", "list", "--json", json]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        text(&output.stdout),
        format!(
            "No active BREAKPOINT instrumentations found\n\nService: {FORGE_ESCAPED}\n\
             Environment: prod\n\nTIP: Use create_instrumentation to add instrumentations.\n"
        )
    );
    assert_channels_safe(&output);
}

#[test]
fn create_success_limit_reproduction_from_4662_is_byte_exact() {
    for (key, label) in [
        ("MaxHits", "Max Hits"),
        ("MaxStringLength", "Max String Length"),
        ("MaxCollectionWidth", "Max Collection Width"),
        ("MaxCollectionDepth", "Max Collection Depth"),
        ("MaxStackFrames", "Max Stack Frames"),
        ("MaxStackTraceSize", "Max Stack Trace Size"),
        ("MaxObjectDepth", "Max Object Depth"),
        ("MaxFieldsPerObject", "Max Fields Per Object"),
    ] {
        let json = format!(
            r#"{{"normalized_type":"BREAKPOINT","service":"svc","environment":"prod","now":"2025-02-03T10:00:00Z","limits":{{"{key}":"{HOSTILE_JSON}"}}}}"#
        );
        let output =
            toolkit_with_secret(&["cloudwatch", "render", "create-success", "--json", &json]);
        assert_eq!(status(&output), 0, "{}", text(&output.stderr));
        assert_eq!(text(&output.stderr), "");
        let stdout = text(&output.stdout);
        assert!(
            stdout.contains(&format!("\nCAPTURE LIMITS:\n- {label}: {ESCAPED}\n\nNOTE:")),
            "{key}: {stdout}"
        );
        assert_channels_safe(&output);
    }
}

#[test]
fn text_renderers_keep_every_caller_scalar_on_one_line() {
    let cases: &[(&str, &str)] = &[
        (
            "list",
            r#"{"data":{},"normalized_type":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN"}"#,
        ),
        (
            "list",
            r#"{"data":{"LatestConfigurations":[{"LocationHash":"0123456789abcdef"}]},"normalized_type":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN"}"#,
        ),
        (
            "get",
            r#"{"config":{},"service":"CALLERTOKEN","environment":"CALLERTOKEN"}"#,
        ),
        (
            "batch-delete",
            r#"{"mode":"CALLERTOKEN","data":{},"instrumentation_type":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN"}"#,
        ),
        (
            "batch-delete",
            r#"{"mode":"service","data":{"Errors":[{"ResourceArn":"CALLERTOKEN","Code":"CALLERTOKEN","Message":"CALLERTOKEN"}]},"instrumentation_type":"BREAKPOINT"}"#,
        ),
        (
            "status-history",
            r#"{"data":{"Events":[{"Time":"2025-02-03T10:05:00Z","ErrorCause":"CALLERTOKEN"}]},"normalized_type":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN","requested_status":"CALLERTOKEN"}"#,
        ),
        (
            "create-success",
            r#"{"response":{},"location":{"CodeLocation":{"Language":"CALLERTOKEN","FilePath":"CALLERTOKEN","MethodName":"CALLERTOKEN"}},"normalized_type":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN","capture_arguments":["CALLERTOKEN"],"capture_locals":["CALLERTOKEN"],"now":"2025-02-03T10:00:00Z"}"#,
        ),
        // #4662: all eight caller capture limits.
        (
            "create-success",
            r#"{"normalized_type":"BREAKPOINT","service":"svc","environment":"prod","now":"2025-02-03T10:00:00Z","limits":{"MaxHits":"CALLERTOKEN","MaxStringLength":"CALLERTOKEN","MaxCollectionWidth":"CALLERTOKEN","MaxCollectionDepth":"CALLERTOKEN","MaxStackFrames":"CALLERTOKEN","MaxStackTraceSize":"CALLERTOKEN","MaxObjectDepth":"CALLERTOKEN","MaxFieldsPerObject":"CALLERTOKEN"}}"#,
        ),
        // ACTIVE confirmed: the subject block and the snapshot log-group tip.
        (
            "status-assessment",
            r#"{"created_at":"2025-02-03T10:00:00Z","requested_start":"2025-02-03T09:00:00Z","query_end":"2025-02-03T11:00:00Z","location_hash":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN","normalized_type":"CALLERTOKEN","checks":{"ACTIVE":{"events":[{"Time":"2025-02-03T10:30:00Z"}]}}}"#,
        ),
        // Nothing confirmed: the recorded check errors and the ERROR cause.
        (
            "status-assessment",
            r#"{"created_at":"2025-02-03T10:00:00Z","requested_start":"2025-02-03T09:00:00Z","query_end":"2025-02-03T11:00:00Z","location_hash":"CALLERTOKEN","service":"CALLERTOKEN","environment":"CALLERTOKEN","normalized_type":"CALLERTOKEN","checks":{"ACTIVE":{"error":"CALLERTOKEN"},"READY":{"error":"CALLERTOKEN"},"ERROR":{"events":[{"Time":"2025-02-03T09:45:00Z","ErrorCause":"CALLERTOKEN"}]}}}"#,
        ),
    ];
    for (renderer, template) in cases {
        assert_caller_fields_contained(renderer, template, ESCAPED);
    }
}

#[test]
fn json_renderers_keep_caller_scalars_inside_json_strings() {
    // These renderers print `json.dumps(..., indent=2)`: a caller value is a
    // JSON string, so its newline and ESC appear as JSON escapes.
    let cases: &[(&str, &str)] = &[
        (
            "snapshot-search",
            r#"{"query_result":{"status":"Error","error":"CALLERTOKEN"},"service_name":"CALLERTOKEN","environment":"CALLERTOKEN","location_hash":"CALLERTOKEN","custom_filters":["CALLERTOKEN"],"start_time_utc":"CALLERTOKEN","end_time_utc":"CALLERTOKEN","start_epoch":1,"end_epoch":2,"query_string":"CALLERTOKEN"}"#,
        ),
        (
            "snapshot-sample",
            r#"{"query_result":{"status":"Error","error":"CALLERTOKEN"},"service_name":"CALLERTOKEN","environment":"CALLERTOKEN","location_hash":"CALLERTOKEN","start_time_utc":"CALLERTOKEN","end_time_utc":"CALLERTOKEN","query_string":"CALLERTOKEN"}"#,
        ),
    ];
    for (renderer, template) in cases {
        assert_caller_fields_contained(renderer, template, HOSTILE_JSON);
    }
}

fn render_failure_stdout(json: &str) -> String {
    let output = toolkit_with_secret(&["cloudwatch", "render", "failure", "--json", json]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert_channels_safe(&output);
    text(&output.stdout)
}

#[test]
fn caller_possible_causes_cannot_forge_a_line_4653() {
    let json = r#"{"failure":{"kind":"client_error","message":"ordinary error"},"action":"read","possible_causes":["REVIEW_SENTINEL\nFORGED_REPORT"]}"#;
    assert_eq!(
        render_failure_stdout(json),
        format!(
            "Failed to read\n\nError: ClientError - ordinary error\n\n\
             POSSIBLE CAUSES:\n1. {FORGE_ESCAPED}\n"
        )
    );
}

#[test]
fn caller_troubleshooting_cannot_forge_a_line_4653() {
    let json = r#"{"failure":{"kind":"client_error","message":"ordinary error"},"action":"read","troubleshooting":["REVIEW_SENTINEL\nFORGED_REPORT"]}"#;
    assert_eq!(
        render_failure_stdout(json),
        format!(
            "Failed to read\n\nError: ClientError - ordinary error\n\n\
             TROUBLESHOOTING:\n1. {FORGE_ESCAPED}\n"
        )
    );
}

#[test]
fn caller_attempted_label_cannot_forge_a_line_4653() {
    let json = r#"{"failure":{"kind":"client_error","message":"ordinary error"},"action":"read","attempted":{"service":"svc"},"attempted_label":"REVIEW_SENTINEL\nFORGED_REPORT","possible_causes":["c"]}"#;
    assert_eq!(
        render_failure_stdout(json),
        format!(
            "Failed to read\n\nError: ClientError - ordinary error\n\n\
             {FORGE_ESCAPED}\n- service: svc\n\nPOSSIBLE CAUSES:\n1. c\n"
        )
    );
}

#[test]
fn caller_trailer_cannot_forge_a_line_4653() {
    let json = r#"{"failure":{"kind":"client_error","message":"ordinary error"},"action":"read","trailer":"REVIEW_SENTINEL\nFORGED_REPORT"}"#;
    assert_eq!(
        render_failure_stdout(json),
        format!("Failed to read\n\nError: ClientError - ordinary error\n\n{FORGE_ESCAPED}\n")
    );
}

#[test]
fn caller_prose_escapes_terminal_controls_too_4653() {
    let json = format!(
        r#"{{"failure":{{"kind":"client_error","message":"m"}},"action":"read","attempted":{{"k":"v"}},"attempted_label":"{HOSTILE_JSON}","possible_causes":["{HOSTILE_JSON}"],"troubleshooting":["{HOSTILE_JSON}"],"trailer":"{HOSTILE_JSON}"}}"#
    );
    assert_eq!(
        render_failure_stdout(&json),
        format!(
            "Failed to read\n\nError: ClientError - m\n\n{ESCAPED}\n- k: v\n\n\
             POSSIBLE CAUSES:\n1. {ESCAPED}\n\nTROUBLESHOOTING:\n1. {ESCAPED}\n\n{ESCAPED}\n"
        )
    );
}
