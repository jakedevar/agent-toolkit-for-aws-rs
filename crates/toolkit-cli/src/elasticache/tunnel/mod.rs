//! `toolkit elasticache find-tunnel-host | start-tunnel | test-connection`
//! (slice `cache-ops`, Issue #4393): the Rust port of the `amazon-elasticache`
//! skill's tunnel discovery, tunnel start and connection-test scripts.
//!
//! | Command | Replaces |
//! | --- | --- |
//! | `elasticache find-tunnel-host` | `find_tunnel_host.py` |
//! | `elasticache start-tunnel` | `start_tunnel.py` |
//! | `elasticache test-connection` | `test_connection.py` |
//!
//! **Offline.** This build opens no socket, starts no process and connects to
//! no AWS API, Redis or Valkey. Each of those is a seam ([`seams`]) that refuses
//! with a typed error without `--recorded FILE` and replays a recording with
//! it ([`recorded`]). The live implementations are follow-ups; see
//! `docs/migration/elasticache-tunnel.md` for the contract, the recording
//! format and every divergence from the scripts.

use toolkit_core::ToolkitError;

use crate::aurora::args::{float_value, int_value, usage};
use crate::{Invocation, Output};

mod clean;
mod connect;
mod discover;
mod recorded;
mod seams;
mod start;

use clean::clean;
use recorded::Recording;
use seams::{
    CacheEnvironment, TunnelAws, TunnelHost, UnavailableAws, UnavailableCache, UnavailableHost,
};

/// The recording option of the three commands.
const TUNNEL_RECORDED_FLAG: &str = "--recorded";

/// Registration of the tunnel commands of the `elasticache` slice (see
/// `crate::registry`). They extend the `elasticache` command family of
/// [`super::SLICE`] and are documented in `docs/migration/elasticache-tunnel.md`.
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "elasticache-tunnel",
    commands: &[
        crate::CommandSpec {
            path: &["elasticache", "find-tunnel-host"],
            flags: &[],
            value_flags: FIND_TUNNEL_HOST_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: find_tunnel_host_command,
        },
        crate::CommandSpec {
            path: &["elasticache", "start-tunnel"],
            flags: &[],
            value_flags: START_TUNNEL_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: start_tunnel_command,
        },
        crate::CommandSpec {
            path: &["elasticache", "test-connection"],
            flags: TEST_CONNECTION_FLAGS,
            value_flags: TEST_CONNECTION_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: test_connection_command,
        },
    ],
    synopsis: r"  toolkit elasticache find-tunnel-host --vpc-id VPC [--region R] [--profile P]
      [--recorded FILE]
  toolkit elasticache start-tunnel --instance-id ID --cache-host HOST
      [--cache-port N] [--local-port N] [--region R] [--profile P] [--recorded FILE]
  toolkit elasticache test-connection HOST [--port N] [--timeout S] [--no-tls]
      [--tunnel-mode] [--server-name NAME] [--username U] [--password P]
      [--iam-auth --iam-user U --cache-name NAME] [--serverless] [--region R]
      [--recorded FILE]
",
    description: r"  elasticache find-tunnel-host | start-tunnel | test-connection
                ElastiCache tunnel tools (#4393). `find-tunnel-host` lists the
                SSM-managed EC2 instances of a VPC, `start-tunnel` runs the
                pre-flight checks and supervises an SSM port-forwarding
                session, `test-connection` pings an endpoint. This build opens
                no sockets and starts no processes: each needs --recorded FILE
                (a recorded run) and refuses without it. See
                docs/migration/elasticache-tunnel.md.
",
    exit_notes: "",
    doc: "docs/migration/elasticache-tunnel.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// Value flags of `elasticache find-tunnel-host`.
pub const FIND_TUNNEL_HOST_VALUE_FLAGS: &[&str] =
    &["--vpc-id", "--region", "--profile", TUNNEL_RECORDED_FLAG];

/// Value flags of `elasticache start-tunnel`.
pub const START_TUNNEL_VALUE_FLAGS: &[&str] = &[
    "--instance-id",
    "--cache-host",
    "--cache-port",
    "--local-port",
    "--region",
    "--profile",
    TUNNEL_RECORDED_FLAG,
];

/// Boolean flags of `elasticache test-connection`.
pub const TEST_CONNECTION_FLAGS: &[&str] =
    &["--no-tls", "--tunnel-mode", "--iam-auth", "--serverless"];

/// Value flags of `elasticache test-connection`.
pub const TEST_CONNECTION_VALUE_FLAGS: &[&str] = &[
    "--port",
    "--timeout",
    "--server-name",
    "--username",
    "--password",
    "--iam-user",
    "--cache-name",
    "--region",
    TUNNEL_RECORDED_FLAG,
];

/// Longest accepted host name (DNS limit).
const MAX_HOST_CHARS: usize = 253;

fn required_value<'a>(invocation: &'a Invocation, flag: &str) -> Result<&'a str, ToolkitError> {
    invocation
        .value(flag)
        .ok_or_else(|| usage(format!("the following arguments are required: {flag}")))
}

/// A usage error that names the flag and the rule, never the value.
fn invalid(flag: &str, rule: &str, value: &str) -> ToolkitError {
    usage(format!(
        "argument {flag}: invalid value ({rule}; the value has {} characters)",
        value.chars().count()
    ))
}

/// `i-` or `mi-` and 8 to 17 lowercase hex digits (EC2 and SSM-managed ids).
fn is_instance_id(value: &str) -> bool {
    value
        .strip_prefix("mi-")
        .or_else(|| value.strip_prefix("i-"))
        .is_some_and(|hex| {
            (8..=17).contains(&hex.len())
                && hex
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

/// A DNS name or an IP address: ASCII letters, digits, `.`, `-`, `_` and `:`
/// (IPv6), at most 253 characters, not starting with `-`.
fn is_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_HOST_CHARS
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c))
}

/// An AWS region code such as `us-east-1` or `us-gov-west-1`.
fn is_region(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A profile or identity name: 1 to `max` characters, no control, format or
/// separator character (the shared invisible-character set), not starting with
/// `-`.
fn is_word(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max
        && !value.starts_with('-')
        && !value.chars().any(crate::safe_text::is_unsafe)
}

fn region_of(invocation: &Invocation) -> Result<String, ToolkitError> {
    match invocation.value("--region") {
        None => {
            let region = super::default_region();
            if is_region(&region) {
                Ok(region)
            } else {
                Err(usage(
                    "the default region (AWS_REGION or AWS_DEFAULT_REGION) is not a region code; pass --region",
                ))
            }
        }
        Some(region) if is_region(region) => Ok(region.to_string()),
        Some(region) => Err(invalid(
            "--region",
            "a region code such as us-east-1",
            region,
        )),
    }
}

fn profile_of(invocation: &Invocation) -> Result<Option<&str>, ToolkitError> {
    match invocation.value("--profile") {
        None => Ok(None),
        Some(profile) if is_word(profile, 128) => Ok(Some(profile)),
        Some(profile) => Err(invalid(
            "--profile",
            "1-128 printable characters, not starting with '-'",
            profile,
        )),
    }
}

/// The recording of `--recorded FILE`, when given.
fn recording_of(
    invocation: &Invocation,
    accepted: &[&str],
) -> Result<Option<Recording>, ToolkitError> {
    let Some(path) = invocation.value(TUNNEL_RECORDED_FLAG) else {
        return Ok(None);
    };
    let document = super::read_json_file(path)?;
    Recording::from_document(&document, accepted)
        .map(Some)
        .map_err(|reason| {
            ToolkitError::runtime(
                "elasticache_recording_invalid",
                format!(
                    "the recorded document {}: {}",
                    crate::safe_text::safe_path(path),
                    clean(&reason)
                ),
            )
        })
}

/// A run must use the whole recording (a step no call used is a failure).
fn all_used(unused: usize) -> Result<(), ToolkitError> {
    if unused == 0 {
        Ok(())
    } else {
        Err(ToolkitError::runtime(
            "elasticache_recording_unused",
            format!("{unused} recorded step(s), probe(s) or event(s) were never used by this run"),
        ))
    }
}

/// Handler of `toolkit elasticache find-tunnel-host`.
pub fn find_tunnel_host_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let vpc_id = required_value(invocation, "--vpc-id")?;
    super::validator::validate_vpc_id(vpc_id)
        .map_err(|_| invalid("--vpc-id", "vpc- and 8 to 17 lowercase hex digits", vpc_id))?;
    let region = region_of(invocation)?;
    profile_of(invocation)?;
    let recording = recording_of(invocation, &["calls"])?;

    let args = discover::DiscoverArgs {
        vpc_id,
        region: &region,
    };
    let mut live = UnavailableAws;
    let mut replay;
    let aws: &mut dyn TunnelAws = match recording {
        Some(recording) => {
            replay = recording.aws;
            &mut replay
        }
        None => &mut live,
    };
    discover::run(&args, aws, out)?;
    all_used(aws.unconsumed())
}

/// Handler of `toolkit elasticache start-tunnel`.
pub fn start_tunnel_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let instance_id = required_value(invocation, "--instance-id")?;
    let cache_host = required_value(invocation, "--cache-host")?;
    if !is_instance_id(instance_id) {
        return Err(invalid(
            "--instance-id",
            "i- or mi- and 8 to 17 lowercase hex digits",
            instance_id,
        ));
    }
    if !is_host(cache_host) {
        return Err(invalid(
            "--cache-host",
            "a host name or address of letters, digits and . - _ :",
            cache_host,
        ));
    }
    let cache_port = int_value(invocation, "--cache-port")?.unwrap_or(6379);
    let local_port = int_value(invocation, "--local-port")?.unwrap_or(6379);
    let region = region_of(invocation)?;
    let profile = profile_of(invocation)?;
    let recording = recording_of(invocation, &["calls", "host", "tunnel"])?;

    let args = start::StartArgs {
        instance_id,
        cache_host,
        cache_port,
        local_port,
        region: &region,
        profile,
    };
    let mut live_aws = UnavailableAws;
    let mut live_host = UnavailableHost;
    let (mut replay_aws, mut replay_host);
    let (aws, host): (&mut dyn TunnelAws, &mut dyn TunnelHost) = match recording {
        Some(recording) => {
            replay_aws = recording.aws;
            replay_host = recording.host;
            (&mut replay_aws, &mut replay_host)
        }
        None => (&mut live_aws, &mut live_host),
    };
    start::run(&args, aws, host, out)?;
    all_used(aws.unconsumed() + host.unconsumed())
}

/// Handler of `toolkit elasticache test-connection`.
pub fn test_connection_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(host) = invocation.positional(0) else {
        return Err(usage("the following arguments are required: host"));
    };
    if !is_host(host) {
        return Err(invalid(
            "host",
            "a host name or address of letters, digits and . - _ :",
            host,
        ));
    }
    let port = int_value(invocation, "--port")?.unwrap_or(6379);
    let port = u16::try_from(port)
        .ok()
        .filter(|port| *port >= 1)
        .ok_or_else(|| usage("argument --port: must be between 1 and 65535"))?;
    let timeout_secs = float_value(invocation, "--timeout")?.unwrap_or(5.0);
    if !timeout_secs.is_finite() || timeout_secs <= 0.0 {
        return Err(usage(
            "argument --timeout: must be a positive, finite number of seconds",
        ));
    }
    let server_name = invocation.value("--server-name");
    if let Some(name) = server_name {
        if !is_host(name) {
            return Err(invalid(
                "--server-name",
                "a host name of letters, digits and . - _ :",
                name,
            ));
        }
    }
    for flag in ["--username", "--iam-user", "--cache-name"] {
        if let Some(value) = invocation.value(flag) {
            if !is_word(value, 128) {
                return Err(invalid(
                    flag,
                    "at most 128 printable characters, not starting with '-'",
                    value,
                ));
            }
        }
    }
    let region = region_of(invocation)?;
    let args = connect::ConnectArgs {
        host,
        port,
        timeout_secs,
        no_tls: invocation.has_flag("--no-tls"),
        tunnel_mode: invocation.has_flag("--tunnel-mode"),
        server_name,
        username: invocation.value("--username"),
        password: invocation.value("--password"),
        iam_auth: invocation.has_flag("--iam-auth"),
        iam_user: invocation.value("--iam-user"),
        cache_name: invocation.value("--cache-name"),
        serverless: invocation.has_flag("--serverless"),
        region: &region,
    };
    let recording = recording_of(invocation, &["cache"])?;
    let mut live = UnavailableCache;
    let mut replay;
    let env: &mut dyn CacheEnvironment = match recording {
        Some(recording) => {
            replay = recording.cache;
            &mut replay
        }
        None => &mut live,
    };
    connect::run(&args, env, out)?;
    all_used(env.unconsumed())
}

#[cfg(test)]
mod tests;
