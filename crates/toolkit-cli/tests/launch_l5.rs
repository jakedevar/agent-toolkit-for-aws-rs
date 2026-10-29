//! Offline Launch L5 command contracts.
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn invoke(args: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "launch-l5-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(args)
        .env("HOME", &dir)
        .env_remove("LAUNCH_WITH_AWS_REGION")
        .env_remove("LAUNCH_WITH_AWS_BASE_URL")
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .env_remove("AWS_PROFILE")
        .output()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    output
}
#[test]
fn t_cmd_resolve_region_default() {
    let output = invoke(&["launch", "resolve-region"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "{\n  \"region\": \"us-east-1\",\n  \"regionName\": \"US East (N. Virginia)\",\n  \"source\": \"default\",\n  \"sourceLabel\": \"the default\",\n  \"baseUrl\": \"https://launch-with-aws.us-east-1.api.aws\",\n  \"otherRegions\": [\n    {\n      \"region\": \"eu-central-1\",\n      \"regionName\": \"Europe (Frankfurt)\"\n    }\n  ]\n}\n");
    assert!(output.stderr.is_empty());
}
#[test]
fn t_cmd_session_status_without_session() {
    let output = invoke(&["launch", "session-status"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\n  \"authenticated\": false\n}\n");
    assert!(output.stderr.is_empty());
}
#[test]
fn t_cmd_sign_out_without_session() {
    let output = invoke(&["launch", "sign-out"]);
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"{\n  \"signedOut\": true,\n  \"sessionRemoved\": false\n}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn t_cli_missing_required_arguments_use_json() {
    for name in [
        "create-launch",
        "get-launch",
        "delete-launch",
        "start-launch-execution",
        "get-launch-status",
        "get-launch-download-url",
    ] {
        let output = invoke(&["launch", name]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("{{\"error\":\"Missing required argument for {name}\"}}\n")
        );
    }
}
#[test]
fn t_cli_rejects_bare_dash_as_positional() {
    for name in [
        "create-launch",
        "get-launch",
        "delete-launch",
        "start-launch-execution",
        "get-launch-status",
        "get-launch-download-url",
    ] {
        let output = invoke(&["launch", name, "-"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
#[cfg(not(feature = "aws-sdk"))]
#[test]
fn t_cmd_network_commands_fail_without_network_support() {
    for args in [
        &["launch", "get-launch", "l-1"][..],
        &["launch", "delete-launch", "l-1"],
        &["launch", "start-launch-execution", "l-1"],
        &["launch", "get-launch-status", "l-1"],
        &["launch", "list-launches"],
    ] {
        let output = invoke(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"{\"error\":\"This toolkit build has no network support; install a build with the aws-sdk feature.\"}\n");
    }
}

#[test]
fn t_api_create_name_validation_precedes_network() {
    let long_name = "x".repeat(129);
    let output = invoke(&["launch", "create-launch", ".", &long_name]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"{\"error\":\"Invalid launch name: it must be 1 to 128 characters.\"}\n"
    );
}

#[test]
fn t_cmd_create_rejects_credential_bearing_url_without_echoing_credentials() {
    let output = invoke(&[
        "launch",
        "create-launch",
        "https://user:FAKE-URL-SECRET@example.test/repo",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("create-launch takes a local directory"));
    assert!(!diagnostic.contains("FAKE-URL-SECRET"));
}
