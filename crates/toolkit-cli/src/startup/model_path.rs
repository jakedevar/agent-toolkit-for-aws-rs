//! `toolkit startup verify-model-path` (#4402, slice `startup-diagram`).
//!
//! Ports `plugins/aws-startup-advisor/skills/agent-advisor/scripts/verify_model_path.py`:
//! probe the model paths of a `model-recommendation.json` in the target
//! account and write `model-verification.json`. The recommendation is checked
//! against the `model-recommendation` schema before any probe, the result
//! against the `model-verification` schema before it is written; both schemas
//! are the plugin's own `scripts/schemas/*.json`, compiled into the binary.
//!
//! - [`probe`]: the client seam. Every call the Python made on a `boto3`,
//!   `anthropic` or `openai` client is one operation with the request it built;
//!   this build wires no live client, so nothing is sent and every probe is
//!   stored as a `RuntimeError` failure.
//! - [`verify`]: workload and recommendation verification, application
//!   inference profile checks and the redaction of probe errors.
//!
//! Diagnostics never echo a value of the recommendation: findings name the
//! path (keys escaped with `safe_text`) and the schema rule, a bad
//! `--workload` is reported by count, and probe errors are redacted and
//! escaped before they are stored. See `docs/migration/startup-diagram.md`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use toolkit_aws::redact_text;
use toolkit_core::{parse_json, write_regular_file, Json, ToolkitError};
use toolkit_msk::pyfmt::json_dumps;

use super::read_json_document;
use crate::manifests::read_failure;
use crate::safe_text::{safe_path, safe_text};
use crate::spec::engine::{self, Segment};
use crate::{Invocation, Output};

mod probe;
mod verify;

/// Registration of the `startup-model-path` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-model-path",
    commands: &[crate::CommandSpec {
        path: &["startup", "verify-model-path"],
        flags: &[],
        value_flags: &["--output", "--workload"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: crate::startup::verify_model_path_command,
    }],
    synopsis: r"  toolkit startup verify-model-path RECOMMENDATION [--output FILE] [--workload ID[,ID...]]
",
    description: r"  startup verify-model-path RECOMMENDATION
                Probe the model paths of a model-recommendation.json
                (agent-advisor verify_model_path.py) and write
                model-verification.json beside it (--output FILE overrides).
                --workload ID[,ID...] probes only those workloads. Prints
                RESULT=ok|failed WORKLOADS=<n>; exit 2 when a probe failed or
                needs resolution. Probe errors are redacted. This build has no
                live Bedrock client: a probe reports a RuntimeError.
",
    exit_notes: "",
    doc: "docs/migration/startup-diagram.md",
    synopsis_rank: 160,
    description_rank: 160,
};

#[cfg(test)]
mod tests;

use probe::NoLiveClient;
use verify::verify_recommendation;

/// The `model-recommendation` schema of the agent-advisor skill.
const RECOMMENDATION_SCHEMA: &str = include_str!(
    "../../../../plugins/aws-startup-advisor/skills/agent-advisor/scripts/schemas/model-recommendation.json"
);

/// The `model-verification` schema of the agent-advisor skill.
const VERIFICATION_SCHEMA: &str = include_str!(
    "../../../../plugins/aws-startup-advisor/skills/agent-advisor/scripts/schemas/model-verification.json"
);

/// Name of the file written beside the recommendation by default.
const VERIFICATION_FILENAME: &str = "model-verification.json";

/// Longest finding line printed, in characters.
const MAX_LINE_CHARS: usize = 400;

/// Most schema findings printed before the rest is only counted.
const MAX_FINDINGS_SHOWN: usize = 20;

/// Statuses that make the run fail (exit status 2).
const FAILING_STATUSES: [&str; 2] = ["failed", "needs_resolution"];

/// The workload ids selected by `--workload`: the whole value when it names a
/// workload, else its comma-separated parts. The router keeps the last value
/// of a repeated flag.
fn selected_workloads(value: Option<&str>, recommendation: &Json) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(Json::Object(workloads)) = recommendation.get("workloads") {
        if workloads.iter().any(|(name, _)| name == value) {
            return vec![value.to_string()];
        }
    }
    value
        .split(',')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// `line` cut to [`MAX_LINE_CHARS`] characters (a finding about thousands of
/// unexpected keys must not flood the terminal).
fn bounded_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(MAX_LINE_CHARS).collect();
    cut.push_str("...");
    cut
}

/// `$`, `$.key` and `$[0]` style path of a finding; keys are escaped.
fn finding_path(path: &[Segment]) -> String {
    let mut out = String::from("$");
    for segment in path {
        match segment {
            Segment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
            Segment::Key(key) => {
                out.push('.');
                out.push_str(&safe_text(key));
            }
        }
    }
    out
}

/// Checks `document` against the compiled-in `schema` and reports every
/// finding on the diagnostics stream, without any value of the document.
fn check_schema(
    schema_source: &str,
    schema_name: &str,
    document: &Json,
    what: &str,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let invalid = |reason: String| ToolkitError::runtime("startup_model_path_schema", reason);
    let schema = parse_json(schema_source)
        .map_err(|_| invalid(format!("the {schema_name} schema is not valid JSON")))?;
    let compiled = engine::compile(schema)
        .map_err(|reason| invalid(format!("the {schema_name} schema cannot be used: {reason}")))?;
    let findings = compiled
        .validate_redacted(document)
        .map_err(|reason| invalid(format!("cannot check {what}: {reason}")))?;
    if findings.is_empty() {
        return Ok(());
    }
    for finding in findings.iter().take(MAX_FINDINGS_SHOWN) {
        output.diagnostic(&bounded_line(&redact_text(&format!(
            "  {}: {}",
            finding_path(&finding.path),
            finding.message
        ))));
    }
    if findings.len() > MAX_FINDINGS_SHOWN {
        output.diagnostic(&format!(
            "  ... and {} more",
            findings.len() - MAX_FINDINGS_SHOWN
        ));
    }
    Err(invalid(format!(
        "{what} does not match the {schema_name} schema ({} finding(s))",
        findings.len()
    )))
}

/// Handler for `startup verify-model-path RECOMMENDATION`: validates the
/// recommendation, probes the selected workloads, validates and writes the
/// verification (default: `model-verification.json` beside RECOMMENDATION) and
/// prints `RESULT=ok|failed WORKLOADS=<n>`.
///
/// # Errors
///
/// A usage error (exit status 2) without RECOMMENDATION; a runtime error (exit
/// status 1) when a file is unreadable, linked or malformed, the recommendation
/// or the result breaks its schema, a selected workload is unknown, or the
/// output cannot be written. A run in which a workload `failed` or
/// `needs_resolution` ends with exit status 2 and the usual stdout line.
pub fn verify_model_path_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(source) = invocation.positional(0) else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "usage: toolkit startup verify-model-path RECOMMENDATION.json \
             [--output FILE] [--workload ID[,ID...]]",
        ));
    };
    let source_path = Path::new(source);
    let recommendation = read_json_document(source_path)
        .map_err(|reason| ToolkitError::runtime("startup_model_path_read", reason))?;
    check_schema(
        RECOMMENDATION_SCHEMA,
        "model-recommendation",
        &recommendation,
        "the recommendation",
        output,
    )?;
    let workloads = selected_workloads(invocation.value("--workload"), &recommendation);
    let result = verify_recommendation(&recommendation, &workloads, &mut NoLiveClient, None)?;
    check_schema(
        VERIFICATION_SCHEMA,
        "model-verification",
        &result,
        "the verification result",
        output,
    )?;
    let destination: PathBuf = match invocation.value("--output") {
        Some(path) => PathBuf::from(path),
        None => source_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(VERIFICATION_FILENAME),
    };
    write_regular_file(&destination, &format!("{}\n", json_dumps(&result))).map_err(|error| {
        ToolkitError::runtime(
            "startup_model_path_write",
            format!(
                "cannot write {}: {}",
                safe_path(&destination.display().to_string()),
                read_failure(&error)
            ),
        )
    })?;
    let verified = match result.get("workloads") {
        Some(Json::Object(members)) => members.as_slice(),
        _ => &[],
    };
    let failed = verified.iter().any(|(_, workload)| {
        workload
            .get("status")
            .and_then(Json::as_str)
            .is_some_and(|status| FAILING_STATUSES.contains(&status))
    });
    output.line(&format!(
        "RESULT={} WORKLOADS={}",
        if failed { "failed" } else { "ok" },
        verified.len()
    ))?;
    if failed {
        output.set_exit_status(2);
    }
    Ok(())
}
