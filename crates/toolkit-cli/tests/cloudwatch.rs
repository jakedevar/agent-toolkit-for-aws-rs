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
