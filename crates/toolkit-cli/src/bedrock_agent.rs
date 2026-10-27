//! `toolkit bedrock-agent fetch` (#4383, slice `bedrock-fetch`).
//!
//! Ports `skills/core-skills/amazon-bedrock/scripts/fetch_bedrock_agent.py`
//! (and its byte-identical plugin mirror): fetch a complete manifest of a
//! Bedrock Agent (Phase 1 of the agent migration) into one JSON document.
//!
//! - [`client`]: the seam. Every `boto3` call of the script is one service and
//!   operation with its request. The command wires the [`recorded`] client
//!   (`--recorded FILE`, deterministic replay of recorded responses) or a client
//!   that sends nothing; a live client behind the `toolkit-aws` transport is a
//!   follow-up (`docs/migration/bedrock-fetch.md`).
//! - [`fetch`]: discovery, pagination and the tolerated-error rules.
//!
//! The manifest holds account ids, role ARNs and inline IAM policies. It is
//! written with owner-only permissions where the platform supports them, a
//! symbolic link at the output path is refused, and no value from the agent or
//! from AWS reaches a diagnostic unescaped.

use std::path::Path;

use toolkit_aws::env::non_empty;
use toolkit_aws::{resolve_region, AwsError, Env, ProcessEnv};
use toolkit_core::{neutralize_line, quote_untrusted, write_regular_file, Json, ToolkitError};
use toolkit_msk::pyfmt::ascii_escape;

use crate::manifests::read_failure;
use crate::safe_text::safe_path;
use crate::startup::read_json_document;
use crate::{Invocation, Output};

mod client;
mod fetch;
mod recorded;

#[cfg(test)]
mod tests;

use client::{AgentClient, NoLiveClient};
use fetch::{discover, Failure, Params};
use recorded::RecordedClient;

/// Value flags of `bedrock-agent fetch`.
pub const FETCH_VALUE_FLAGS: &[&str] = &[
    "--agent-id",
    "--agent-version",
    "--agent-alias-id",
    "--region",
    "--profile",
    "--out",
    "--recorded",
];

/// Boolean flags of `bedrock-agent fetch`.
pub const FETCH_FLAGS: &[&str] = &["--inline-s3-schemas"];

const USAGE: &str = "usage: toolkit bedrock-agent fetch --agent-id ID --out FILE \
[--agent-version VERSION] [--agent-alias-id ID] [--region REGION] [--profile PROFILE] \
[--inline-s3-schemas] [--recorded FILE]";

/// The region of the run, or `None` when nothing selects one (the script's
/// `ERROR: no region resolved`): `--region`, else `AWS_REGION`, else
/// `AWS_DEFAULT_REGION`. There is no built-in default region.
fn region_of(explicit: Option<&str>, env: &dyn Env) -> Result<Option<String>, AwsError> {
    let selected = explicit.is_some_and(|value| !value.is_empty())
        || non_empty(env, "AWS_REGION").is_some()
        || non_empty(env, "AWS_DEFAULT_REGION").is_some();
    if !selected {
        return Ok(None);
    }
    Ok(Some(resolve_region(explicit, env)?.name().to_string()))
}

/// Writes the manifest like `json.dump(manifest, f, indent=2)` (ASCII only, no
/// trailing newline) and restricts it to its owner.
fn write_manifest(out: &str, manifest: &Json, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let path = Path::new(out);
    let shown = safe_path(out);
    let failed = |reason: String| {
        ToolkitError::runtime(
            "bedrock_agent_write",
            format!("cannot write {shown}: {reason}"),
        )
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
    }
    let text = ascii_escape(&manifest.to_pretty_string());
    write_regular_file(path, &text).map_err(|error| failed(read_failure(&error)))?;
    // The manifest holds sensitive data (account ids, role ARNs, inline IAM
    // policies): owner read/write only.
    if let Err(reason) = restrict_to_owner(path) {
        output.diagnostic(&format!(
            "WARNING: could not restrict permissions on {} ({}). It holds sensitive data \
             (account ids, role ARNs, IAM policies) and may be readable by others \u{2014} \
             secure or delete it manually.",
            neutralize_line(out),
            neutralize_line(&reason)
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// The `warnings` of a manifest's `discovery` object.
fn manifest_warnings(manifest: &Json) -> Vec<String> {
    match manifest.get("discovery").and_then(|d| d.get("warnings")) {
        Some(Json::Array(warnings)) => warnings
            .iter()
            .filter_map(|warning| warning.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Runs the discovery against `client` and writes the manifest.
fn fetch_with(
    client: &mut dyn AgentClient,
    params: &Params,
    out: &str,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let manifest = match discover(client, params) {
        Ok(manifest) => manifest,
        Err(Failure::Agent(reason)) => {
            output.diagnostic(&format!(
                "ERROR: failed to fetch agent {}: {}",
                quote_untrusted(&params.agent_id),
                neutralize_line(&reason)
            ));
            output.set_exit_status(1);
            return Ok(());
        }
        Err(Failure::Fatal(error)) => return Err(error.into()),
    };
    write_manifest(out, &manifest, output)?;
    output.line(&format!("Wrote {}", neutralize_line(out)))?;
    for warning in manifest_warnings(&manifest) {
        output.line(&format!("  WARNING: {}", neutralize_line(&warning)))?;
    }
    Ok(())
}

/// Handler for `bedrock-agent fetch --agent-id ID --out FILE ...`.
///
/// Prints `Wrote FILE` and one `  WARNING: ...` line per discovery warning on
/// stdout. Without `--recorded FILE` this build has no live client and the
/// first call fails with a runtime error (exit status 1).
///
/// # Errors
///
/// A usage error (exit status 2) without `--agent-id` or `--out`; a runtime
/// error (exit status 1) for an unreadable or malformed recorded file, a
/// transport failure, a malformed response or an unwritable output. A
/// `get_agent` service error and a missing region print the script's
/// `ERROR:` line on stderr with exit status 1 and 2.
pub fn fetch_command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let (Some(agent_id), Some(out)) = (invocation.value("--agent-id"), invocation.value("--out"))
    else {
        return Err(ToolkitError::usage("missing_argument", USAGE));
    };
    let Some(region) = region_of(invocation.value("--region"), &ProcessEnv)? else {
        output.diagnostic(
            "ERROR: no region resolved from credentials. Pass --region or set AWS_DEFAULT_REGION.",
        );
        output.set_exit_status(2);
        return Ok(());
    };
    let params = Params {
        agent_id: agent_id.to_string(),
        agent_version: invocation
            .value("--agent-version")
            .unwrap_or("DRAFT")
            .to_string(),
        agent_alias_id: invocation.value("--agent-alias-id").map(str::to_string),
        region,
        inline_s3_schemas: invocation.has_flag("--inline-s3-schemas"),
    };
    match invocation.value("--recorded") {
        Some(recorded_path) => {
            let document = read_json_document(Path::new(recorded_path))
                .map_err(|reason| ToolkitError::runtime("bedrock_agent_recorded", reason))?;
            let mut client = RecordedClient::from_document(&document).map_err(|reason| {
                ToolkitError::runtime(
                    "bedrock_agent_recorded",
                    format!("{}: {reason}", safe_path(recorded_path)),
                )
            })?;
            fetch_with(&mut client, &params, out, output)
        }
        None => fetch_with(&mut NoLiveClient, &params, out, output),
    }
}
