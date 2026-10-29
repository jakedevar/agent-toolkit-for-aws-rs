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
