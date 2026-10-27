//! End-to-end tests of `toolkit cloudwatch ...`: real process spawns, real
//! exit codes and separated stdout/stderr. Fixtures are literals and temp
//! files; no network or AWS call is involved.

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
