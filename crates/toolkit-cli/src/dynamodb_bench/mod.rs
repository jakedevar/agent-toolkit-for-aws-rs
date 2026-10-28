//! `toolkit dynamodb benchmark-model` and `toolkit dynamodb generate-perf-report`
//! (slice `dynamodb-bench`, Issue #4390).
//!
//! Ports `amazon-dynamodb/scripts/benchmark_model.py` (the orchestrator of the
//! in-region benchmark Lambda) and `generate_perf_report.py` (the performance
//! report and the machine-readable findings). The behavior contract, the
//! classification of the remote benchmark workload and every intentional change
//! are recorded in `docs/migration/dynamodb-bench.md`.
//!
//! - `benchmark-model` validates the design and the config, applies the mode
//!   preset, splits the measurement across invocations, estimates the spend and
//!   refuses past the guardrail, then invokes the Lambda through a
//!   [`recorded::LambdaClient`] and aggregates the rows. Nothing is driven
//!   against AWS: the build has no live client, so the invocations are
//!   **replayed from a recording** (`--recorded FILE`). Without it the command
//!   stops at the first invocation with a connect error, after every offline
//!   step ran. The live binding is a follow-up (see the document).
//! - `generate-perf-report` is fully offline.
//!
//! The planning, aggregation and report logic is in `crate::dynamodb`
//! (`bench`, `bench_rows`, `perf_report`, `perf_render`); this module owns the
//! command surface, the recorded client and the validation of the response.

use std::path::Path;

use toolkit_core::{parse_json, read_regular_file, Json, ToolkitError};

use crate::dynamodb::bench_fmt::{diag, die, dumps_indent};
use crate::dynamodb::{perf_render, perf_report};
use crate::dynamodb_deploy::write::{write_private, PRIVATE_FILE};
use crate::manifests::read_failure;
use crate::safe_text::safe_path;
use crate::{Invocation, Output};

mod recorded;
mod response;
mod run;
mod shape;
#[cfg(test)]
mod tests;

use recorded::{NoLiveClient, RecordedClient};
pub(crate) use response::check_seed_entry;

/// Registration of the `dynamodb-bench` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "dynamodb-bench",
    commands: &[
        crate::CommandSpec {
            path: &["dynamodb", "benchmark-model"],
            flags: BENCHMARK_MODEL_FLAGS,
            value_flags: BENCHMARK_MODEL_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: benchmark_model_command,
        },
        crate::CommandSpec {
            path: &["dynamodb", "generate-perf-report"],
            flags: &[],
            value_flags: GENERATE_PERF_REPORT_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: generate_perf_report_command,
        },
    ],
    synopsis: r"  toolkit dynamodb benchmark-model --model FILE --config FILE --manifest FILE
      --raw-out FILE --summary-out FILE [--allow-spend] [--recorded FILE] [--now TS]
  toolkit dynamodb generate-perf-report --model FILE --summary FILE --output FILE
      [--findings-out FILE]
",
    description: r"  dynamodb benchmark-model
                Orchestrate the in-region benchmark Lambda (#4390): budget the run, estimate
                the spend, invoke, stitch and aggregate into perf_summary.json. This build
                drives nothing against AWS: the invocations replay --recorded FILE.
  dynamodb generate-perf-report
                Turn perf_summary.json and the design into performance_report.md and
                design_findings.json. Offline.
",
    exit_notes: "",
    doc: "docs/migration/dynamodb-bench.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// Value flags of `dynamodb benchmark-model`.
pub const BENCHMARK_MODEL_VALUE_FLAGS: &[&str] = &[
    "--model",
    "--config",
    "--manifest",
    "--raw-out",
    "--summary-out",
    "--recorded",
    "--now",
];

/// Boolean flags of `dynamodb benchmark-model`.
pub const BENCHMARK_MODEL_FLAGS: &[&str] = &["--allow-spend"];

/// Value flags of `dynamodb generate-perf-report`.
pub const GENERATE_PERF_REPORT_VALUE_FLAGS: &[&str] =
    &["--model", "--summary", "--output", "--findings-out"];

/// Upper bound for the design, config, manifest, summary and recorded JSON.
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

fn usage_missing(name: &str) -> ToolkitError {
    ToolkitError::usage(
        "missing_option",
        format!("the following arguments are required: {name}"),
    )
}

fn required<'a>(invocation: &'a Invocation, name: &str) -> Result<&'a str, ToolkitError> {
    invocation.value(name).ok_or_else(|| usage_missing(name))
}

/// Reads and parses a JSON input through the shared path contract (symbolic
/// links and non-regular files refused, size bounded). Exit status 2, like the
/// script's `_die` for a missing file.
fn load(path: &Path, what: &str) -> Result<Json, ToolkitError> {
    let shown = safe_path(&path.display().to_string());
    let text = read_regular_file(path, MAX_INPUT_BYTES).map_err(|error| {
        die(format!(
            "cannot read {what} {shown}: {}",
            read_failure(&error)
        ))
    })?;
    parse_json(&text).map_err(|error| {
        die(format!(
            "{what} {shown} is not valid JSON: {}",
            diag(&error.to_string())
        ))
    })
}

/// Publishes one export atomically through the shared private writer of
/// `dynamodb-deploy`: a same-directory temporary file created exclusively with
/// mode 0600, written and synced, then renamed over the destination. A
/// destination that is a link or not a regular file is refused (never
/// followed); a failed write leaves any existing destination untouched.
fn write(path: &Path, contents: &str) -> Result<(), ToolkitError> {
    write_private(path, contents, PRIVATE_FILE)
}

/// Handler of `toolkit dynamodb benchmark-model`.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing option; a runtime error with
/// exit status 2 for a refused design, config, manifest, recorded document,
/// Lambda response or output, and with exit status 3 when the estimated spend
/// exceeds the guardrail without `--allow-spend`.
pub fn benchmark_model_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let args = run::Args {
        model: Path::new(required(invocation, "--model")?),
        config: Path::new(required(invocation, "--config")?),
        manifest: Path::new(required(invocation, "--manifest")?),
        raw_out: Path::new(required(invocation, "--raw-out")?),
        summary_out: Path::new(required(invocation, "--summary-out")?),
        allow_spend: invocation.has_flag("--allow-spend"),
        now: invocation.value("--now"),
    };
    let clock = run::Clock::new(args.now)?;
    if let Some(recorded_path) = invocation.value("--recorded") {
        let document = load(Path::new(recorded_path), "the recorded document")?;
        let mut client = RecordedClient::from_document(&document).map_err(|reason| {
            die(format!(
                "the recorded document {}: {reason}",
                safe_path(recorded_path)
            ))
        })?;
        return run::run(&args, &mut client, &clock, out);
    }
    run::run(&args, &mut NoLiveClient, &clock, out)
}

/// Handler of `toolkit dynamodb generate-perf-report`.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing option; a runtime error with
/// exit status 2 for an unreadable or malformed design or summary, or an
/// unwritable output.
pub fn generate_perf_report_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let model_path = Path::new(required(invocation, "--model")?);
    let summary_path = Path::new(required(invocation, "--summary")?);
    let output = required(invocation, "--output")?;
    let findings_out = invocation
        .value("--findings-out")
        .unwrap_or("design_findings.json");

    let model = load(model_path, "the design JSON")?;
    let summary = load(summary_path, "the summary JSON")?;
    let analysis = perf_report::analyze(&model, &summary)?;
    let report = perf_render::render_report(&model, &summary, &analysis)?;
    let payload = perf_report::findings_payload(
        &analysis.findings,
        &analysis.drivers,
        analysis.no_significant,
        analysis.coverage_incomplete,
    );
    write(Path::new(output), &report)?;
    write(Path::new(findings_out), &dumps_indent(&payload))?;
    out.line(&format!("Report: {}", safe_path(output)))?;
    out.line(&format!("Findings: {}", safe_path(findings_out)))?;
    out.line(&format!(
        "Analyzed {} patterns, extracted {} findings ({}).",
        analysis.rows.len(),
        analysis.findings.len(),
        if analysis.no_significant {
            "clean"
        } else {
            "has design findings"
        }
    ))
}
