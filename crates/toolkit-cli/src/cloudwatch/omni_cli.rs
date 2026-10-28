//! `toolkit cloudwatch evaluate-traces` and `toolkit cloudwatch
//! capture-dataset`: the helpers `evaluate_traces.py` and
//! `capture_dataset_from_traces.py` of the CloudWatch Omni skill.
//!
//! Both print one JSON receipt. A success is pretty-printed
//! (`json.dumps(indent=2)`) with exit status 0; a failure receipt is compact
//! JSON with exit status 1 (`_die`). The clients are the recorded replay
//! (`--recorded FILE`) or one that sends nothing; this build has no live AWS
//! client. `capture-dataset` writes a dataset and refuses to run without
//! `--i-have-confirmed`, before any client call.

use toolkit_core::{Json, ToolkitError};

use super::dataset::{self, Mode};
use super::evaluate::{self, Level, Limits, LEVELS};
use super::logs_query::VirtualPacer;
use super::ops_cli::{client_of, clock_of};
use super::pytext::{dumps_indent2, safe_text, strip};
use crate::dynamodb::pyfmt;
use crate::{Invocation, Output};

/// Value flags of `cloudwatch evaluate-traces`.
pub const EVALUATE_VALUE_FLAGS: &[&str] = &[
    "--trace-id",
    "--trace-ids",
    "--session-id",
    "--evaluator-id",
    "--evaluator-ids",
    "--level",
    "--region",
    "--window-days",
    "--retention-days",
    "--kms-key-id",
    "--recorded",
    "--now",
];

/// Boolean flags of `cloudwatch evaluate-traces`.
pub const EVALUATE_FLAGS: &[&str] = &["--include-explanations", "--no-writeback"];

/// Value flags of `cloudwatch capture-dataset`.
pub const CAPTURE_VALUE_FLAGS: &[&str] = &[
    "--trace-ids",
    "--mode",
    "--dataset-name",
    "--dataset-id",
    "--description",
    "--region",
    "--window-days",
    "--recorded",
];

/// Boolean flags of `cloudwatch capture-dataset`.
pub const CAPTURE_FLAGS: &[&str] = &["--i-have-confirmed"];

fn usage(code: &'static str, message: &str) -> ToolkitError {
    ToolkitError::usage(code, safe_text(message))
}

/// An `argparse` `type=int` value: surrounding white space and underscores
/// between digits are accepted, anything else is a usage error.
fn int_flag(invocation: &Invocation, flag: &str, default: i64) -> Result<i64, ToolkitError> {
    let Some(text) = invocation.value(flag) else {
        return Ok(default);
    };
    let trimmed = strip(text);
    let digits = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    let well_formed = !digits.is_empty()
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_');
    let parsed = if well_formed {
        trimmed.replace('_', "").parse::<i64>().ok()
    } else {
        None
    };
    parsed.ok_or_else(|| {
        usage(
            "invalid_int",
            &format!(
                "argument {flag}: invalid int value: {}",
                pyfmt::repr_str(text)
            ),
        )
    })
}

fn owned(invocation: &Invocation, flag: &str) -> Option<String> {
    invocation.value(flag).map(str::to_string)
}

/// Prints a receipt: pretty JSON and exit 0, or compact JSON and exit 1.
fn print_receipt(out: &mut Output<'_>, outcome: &Result<Json, Json>) -> Result<(), ToolkitError> {
    match outcome {
        Ok(receipt) => out.line(&dumps_indent2(receipt)),
        Err(payload) => {
            out.set_exit_status(1);
            out.line(&pyfmt::dumps(payload, false))
        }
    }
}

/// `toolkit cloudwatch evaluate-traces (--trace-id ID | --trace-ids IDS)
/// [--evaluator-id ID | --evaluator-ids IDS] [--level LEVEL] ...`.
///
/// # Errors
///
/// A usage error (exit status 2) for a bad `--level` or integer, a runtime
/// error for an unreadable or malformed recording. Run failures are failure
/// receipts with exit status 1.
pub fn evaluate_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let level = match invocation.value("--level") {
        None => Level::Trace,
        Some(name) => Level::from_name(name).ok_or_else(|| {
            usage(
                "invalid_choice",
                &format!(
                    "argument --level: invalid choice: {} (choose from {})",
                    pyfmt::repr_str(name),
                    LEVELS
                        .iter()
                        .map(|level| pyfmt::repr_str(level))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })?,
    };
    let options = evaluate::Options {
        trace_ids: owned(invocation, "--trace-ids"),
        trace_id: owned(invocation, "--trace-id"),
        session_id: owned(invocation, "--session-id"),
        evaluator_ids: owned(invocation, "--evaluator-ids"),
        evaluator_id: owned(invocation, "--evaluator-id"),
        level,
        window_days: int_flag(invocation, "--window-days", 30)?,
        include_explanations: invocation.has_flag("--include-explanations"),
        no_writeback: invocation.has_flag("--no-writeback"),
        retention_days: int_flag(
            invocation,
            "--retention-days",
            evaluate::DEFAULT_RESULTS_RETENTION_DAYS,
        )?,
        kms_key_id: owned(invocation, "--kms-key-id"),
    };
    let now = clock_of(invocation)?;
    let mut client = client_of(invocation)?;
    let mut pacer = VirtualPacer::default();
    let outcome = evaluate::run(
        client.as_mut(),
        &mut pacer,
        &options,
        &Limits::default(),
        now,
    );
    print_receipt(out, &outcome)
}

/// `toolkit cloudwatch capture-dataset --trace-ids IDS [--mode create|add]
/// [--dataset-name NAME | --dataset-id ID] [--i-have-confirmed] ...`.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing `--trace-ids`, a bad `--mode`
/// or integer, a runtime error for an unreadable or malformed recording. Run
/// failures, the confirmation refusal included, are failure receipts with exit
/// status 1.
pub fn capture_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let Some(trace_ids) = owned(invocation, "--trace-ids") else {
        return Err(usage(
            "missing_argument",
            "the following arguments are required: --trace-ids",
        ));
    };
    let mode = match invocation.value("--mode") {
        None | Some("create") => Mode::Create,
        Some("add") => Mode::Add,
        Some(other) => {
            return Err(usage(
                "invalid_choice",
                &format!(
                    "argument --mode: invalid choice: {} (choose from 'create', 'add')",
                    pyfmt::repr_str(other)
                ),
            ));
        }
    };
    let options = dataset::Options {
        trace_ids,
        mode,
        dataset_name: owned(invocation, "--dataset-name"),
        dataset_id: owned(invocation, "--dataset-id"),
        description: owned(invocation, "--description"),
        window_days: int_flag(invocation, "--window-days", 30)?,
        confirmed: invocation.has_flag("--i-have-confirmed"),
    };
    let mut client = client_of(invocation)?;
    let mut pacer = VirtualPacer::default();
    let outcome = dataset::run(client.as_mut(), &mut pacer, &options);
    print_receipt(out, &outcome)
}
