//! `toolkit mcp-proxy` arguments (#4413 S3): the `mcp-proxy-for-aws-cli`
//! v1.7.0 flags the repository's configs and documents use, checked before
//! anything else runs.
//!
//! - The endpoint (one positional) must satisfy R-URL ([`McpTarget`]).
//! - `--header NAME=VALUE` (repeatable) may not name a header the proxy owns
//!   (R-HDR: authority and framing, proxy, `SigV4`, MCP protocol and session,
//!   `User-Agent`), compared ASCII case-insensitively, nor the same name
//!   twice. Values are secrets: they are marked sensitive and never echoed.
//! - `--metadata KEY=VALUE` is repeatable, one pair per flag (upstream's
//!   `--metadata A=1 B=2` form leaves `B=2` as a second positional, which is
//!   a usage error rather than a silent change).
//! - `--profile` takes one name (upstream's `--profile A B` is a usage error;
//!   multi-profile is a follow-up).
//! - `--retries` is 0-10; each timeout is a number of seconds from 1 to 3600.
//! - `--log-level` is one of upstream's names; it sets the diagnostics
//!   threshold only (no logger is ever installed, R-LOG).
//!
//! Every refusal is a usage error with fixed text that never echoes a value.

use std::time::Duration;

use toolkit_aws::diag::Level;
use toolkit_aws::{is_valid_region, Headers, McpTarget};
use toolkit_core::ToolkitError;

use crate::Invocation;

/// Header names only the proxy or its HTTP stack sets (R-HDR, design 4.4.2).
pub const OWNED_HEADERS: &[&str] = &[
    // Authority and framing (hop-by-hop included).
    "host",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "connection",
    "keep-alive",
    "proxy-connection",
    "upgrade",
    "expect",
    // Proxy.
    "proxy-authorization",
    // SigV4.
    "authorization",
    "x-amz-date",
    "x-amz-security-token",
    "x-amz-content-sha256",
    "date",
    // MCP protocol and session.
    "mcp-session-id",
    "mcp-protocol-version",
    "last-event-id",
    "accept",
    "content-type",
    // Identity (Q3).
    "user-agent",
];

/// Value flags of the command.
pub const VALUE_FLAGS: &[&str] = &[
    "--service",
    "--region",
    "--profile",
    "--retries",
    "--timeout",
    "--connect-timeout",
    "--read-timeout",
    "--write-timeout",
    "--tool-timeout",
    "--log-level",
];

/// Repeatable value flags.
pub const REPEATABLE_FLAGS: &[&str] = &["--metadata", "--header"];

/// Boolean flags.
pub const FLAGS: &[&str] = &["--skip-auth", "--read-only"];

/// The parsed command line.
#[derive(Debug, Clone)]
pub struct ProxyArgs {
    /// The endpoint (R-URL).
    pub target: McpTarget,
    /// `--service`.
    pub service: Option<String>,
    /// `--region`.
    pub region: Option<String>,
    /// `--profile`.
    pub profile: Option<String>,
    /// `--metadata` pairs, in order (a repeated key keeps its last value).
    pub metadata: Vec<(String, String)>,
    /// `--header` pairs (names lowercased), in order.
    pub headers: Vec<(String, String)>,
    /// `--skip-auth`.
    pub skip_auth: bool,
    /// `--read-only`.
    pub read_only: bool,
    /// `--retries`.
    pub retries: u32,
    /// `--timeout`: the deadline of a request other than `tools/call`.
    pub timeout: Duration,
    /// `--connect-timeout`.
    pub connect_timeout: Duration,
    /// `--read-timeout`: the idle-read bound.
    pub read_timeout: Duration,
    /// `--write-timeout` (accepted for compatibility; the request deadline
    /// bounds the body write).
    pub write_timeout: Duration,
    /// `--tool-timeout`: the deadline of a `tools/call`.
    pub tool_timeout: Duration,
    /// `--log-level`.
    pub log_level: Level,
}

fn usage(message: &str) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", format!("invalid arguments: {message}"))
}

fn seconds(
    invocation: &Invocation,
    flag: &'static str,
    default: u64,
) -> Result<Duration, ToolkitError> {
    let Some(text) = invocation.value(flag) else {
        return Ok(Duration::from_secs(default));
    };
    let refused = || {
        usage(&format!(
            "{flag} must be a number of seconds from 1 to 3600"
        ))
    };
    let value: f64 = text.parse().map_err(|_| refused())?;
    if !value.is_finite() || !(1.0..=3600.0).contains(&value) {
        return Err(refused());
    }
    Ok(Duration::from_secs_f64(value))
}

fn pair(text: &str) -> Option<(&str, &str)> {
    let (key, value) = text.split_once('=')?;
    (!key.is_empty()).then_some((key, value))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| !c.is_control() && !c.is_whitespace())
}

/// Parse and check the command line.
///
/// # Errors
/// A usage error (exit 2) with fixed text for any refused argument.
pub fn parse(invocation: &Invocation) -> Result<ProxyArgs, ToolkitError> {
    let Some(endpoint) = invocation.positional(0) else {
        return Err(usage("the endpoint URL is required"));
    };
    let target = McpTarget::parse(endpoint).map_err(|error| usage(error.message()))?;

    let service = invocation.value("--service").map(str::to_string);
    if service
        .as_deref()
        .is_some_and(|s| !toolkit_aws::endpoint::is_valid_service_label(s))
    {
        return Err(usage("--service must be a lowercase signing service name"));
    }
    let region = invocation.value("--region").map(str::to_string);
    if region.as_deref().is_some_and(|r| !is_valid_region(r)) {
        return Err(usage("--region must be a valid AWS region name"));
    }
    let profile = invocation.value("--profile").map(str::to_string);
    if profile.as_deref().is_some_and(|p| !valid_name(p)) {
        return Err(usage("--profile must be a profile name"));
    }

    let mut metadata: Vec<(String, String)> = Vec::new();
    for text in invocation.values_of("--metadata") {
        let Some((key, value)) = pair(text) else {
            return Err(usage("--metadata must be KEY=VALUE with a non-empty KEY"));
        };
        metadata.retain(|(existing, _)| existing != key);
        metadata.push((key.to_string(), value.to_string()));
    }

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut probe = Headers::new();
    for text in invocation.values_of("--header") {
        let Some((name, value)) = pair(text) else {
            return Err(usage("--header must be NAME=VALUE"));
        };
        let lower = name.to_ascii_lowercase();
        if OWNED_HEADERS.contains(&lower.as_str()) {
            return Err(usage(
                "--header names a header the proxy sets itself (Host, framing, \
                 proxy, SigV4, MCP session, Accept, Content-Type or User-Agent)",
            ));
        }
        if headers.iter().any(|(existing, _)| *existing == lower) {
            return Err(usage("--header names the same header twice"));
        }
        if probe.insert_sensitive(&lower, value).is_err() {
            return Err(usage(
                "--header must be NAME=VALUE with a valid header name and value",
            ));
        }
        headers.push((lower, value.to_string()));
    }

    let retries = match invocation.value("--retries") {
        None => 0,
        Some(text) => match text.parse::<u32>() {
            Ok(n) if n <= 10 => n,
            _ => return Err(usage("--retries must be an integer from 0 to 10")),
        },
    };
    let log_level = match invocation.value("--log-level") {
        None => Level::Error,
        Some(text) => Level::parse(text)
            .ok_or_else(|| usage("--log-level must be DEBUG, INFO, WARNING, ERROR or CRITICAL"))?,
    };

    Ok(ProxyArgs {
        target,
        service,
        region,
        profile,
        metadata,
        headers,
        skip_auth: invocation.has_flag("--skip-auth"),
        read_only: invocation.has_flag("--read-only"),
        retries,
        timeout: seconds(invocation, "--timeout", 180)?,
        connect_timeout: seconds(invocation, "--connect-timeout", 60)?,
        read_timeout: seconds(invocation, "--read-timeout", 120)?,
        write_timeout: seconds(invocation, "--write-timeout", 180)?,
        tool_timeout: seconds(invocation, "--tool-timeout", 300)?,
        log_level,
    })
}
