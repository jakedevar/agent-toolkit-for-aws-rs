//! `toolkit mcp-proxy` through the real binary (#4413 S3): start-up
//! refusals exit 2, write nothing to stdout and never echo a refused value
//! (T-CLI, T-HDR CLI, T-CRED-5, T-EP); a build without `aws-sdk` refuses
//! every start with one fixed line; an `aws-sdk` build refuses unusable TLS
//! roots before reading stdin (R-ROOTS). No network: every case stops before
//! a DNS lookup.

use std::process::{Command, Output, Stdio};

const ENDPOINT: &str = "https://aws-mcp.us-east-1.api.aws/mcp";

fn toolkit(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolkit"));
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("toolkit runs")
}

fn refused(args: &[&str]) -> String {
    let output = toolkit(args, &[]);
    assert_eq!(output.status.code(), Some(2), "{args:?}");
    assert!(output.stdout.is_empty(), "{args:?}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.starts_with("error: invalid arguments"), "{stderr}");
    assert!(!stderr.contains("CANARY"), "{stderr}");
    stderr
}

#[test]
fn usage_errors_exit_2_with_nothing_on_stdout_and_no_echo() {
    let stderr = refused(&["mcp-proxy"]);
    assert!(stderr.contains("the endpoint URL is required"));
    refused(&[
        "mcp-proxy",
        "https://aws-mcp.us-east-1.api.aws/mcp?token=CANARY",
    ]);
    refused(&["mcp-proxy", "https://CANARY.example.com/mcp"]);
    refused(&["mcp-proxy", ENDPOINT, "--header", "Host=CANARY.example"]);
    refused(&["mcp-proxy", ENDPOINT, "--header", "Authorization=CANARY"]);
    refused(&["mcp-proxy", ENDPOINT, "--secret-access-key", "CANARY"]);
    refused(&["mcp-proxy", ENDPOINT, "--profile", "a", "CANARY"]);
    refused(&["mcp-proxy", ENDPOINT, "--disable-telemetry"]);
}

#[cfg(not(feature = "aws-sdk"))]
#[test]
fn a_build_without_the_sdk_refuses_every_start() {
    let output = toolkit(&["mcp-proxy", ENDPOINT, "--skip-auth"], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "toolkit mcp-proxy: this toolkit build has no AWS SDK support; rebuild it with --features aws-sdk\n"
    );
}

#[cfg(feature = "aws-sdk")]
#[test]
fn unusable_tls_roots_refuse_the_start() {
    for (value, reason) in [(
        "/nonexistent/toolkit-roots.pem",
        "configured-location-error",
    )] {
        let output = toolkit(
            &["mcp-proxy", ENDPOINT, "--skip-auth"],
            &[("SSL_CERT_FILE", value)],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "toolkit mcp-proxy: TLS trust roots are unusable ({reason}); fix or unset SSL_CERT_FILE/SSL_CERT_DIR\n"
            )
        );
    }
    // An empty SSL_CERT_DIR list is refused as well.
    let output = toolkit(&["mcp-proxy", ENDPOINT], &[("SSL_CERT_DIR", ":")]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("(dir-list-empty)"));
}
