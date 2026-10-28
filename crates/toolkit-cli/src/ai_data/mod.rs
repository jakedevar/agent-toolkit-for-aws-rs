//! AI dataset detection, transformation and metric validation tools (#4385).
//!
//! - `toolkit ai-data detect-format PATH_OR_S3_URI [--sample-size N] [--json]`
//!   replaces `format_detector.py`.
//! - `toolkit ai-data validate-metrics [JSON_OR_FILE]` replaces
//!   `validate_custom_metrics.py`.
//! - `toolkit ai-data transform plan | submit | describe` replaces the two
//!   functions of `transformation_tools.py`.
//!
//! Service access (S3 range reads, `SageMaker`) goes through the
//! [`format::ObjectStore`] and [`transform::ProcessingService`] traits; this
//! build wires the "unavailable" implementations, and the tests inject canned
//! ones. See `docs/migration/ai-data.md` for the parity table and the
//! intentional changes.

use std::path::{Path, PathBuf};

use toolkit_core::{Json, ParseError, ToolkitError};
use toolkit_msk::pyfmt::{json_dumps, py_int_parse};

use crate::{Invocation, Output};

mod format;
mod metrics;
mod transform;

/// Registration of the `ai-data` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "ai-data",
    commands: &[
        crate::CommandSpec {
            path: &["ai-data", "detect-format"],
            flags: DETECT_FORMAT_FLAGS,
            value_flags: DETECT_FORMAT_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: detect_format_command,
        },
        crate::CommandSpec {
            path: &["ai-data", "validate-metrics"],
            flags: &[],
            value_flags: &[],
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: validate_metrics_command,
        },
        crate::CommandSpec {
            path: &["ai-data", "transform", "plan"],
            flags: &[],
            value_flags: TRANSFORM_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: transform_plan_command,
        },
        crate::CommandSpec {
            path: &["ai-data", "transform", "submit"],
            flags: &[],
            value_flags: TRANSFORM_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: transform_submit_command,
        },
        crate::CommandSpec {
            path: &["ai-data", "transform", "describe"],
            flags: &[],
            value_flags: DESCRIBE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: transform_describe_command,
        },
    ],
    synopsis: r"  toolkit ai-data detect-format PATH_OR_S3_URI [--sample-size BYTES] [--json]
  toolkit ai-data validate-metrics [JSON_OR_FILE]
  toolkit ai-data transform plan --script FILE --source-s3 URI --output-s3 URI
      [--instance-type T] [--region R] [--execution-role ARN]
      [--base-job-name N] [--image-uri URI]
  toolkit ai-data transform submit (same options as plan)
  toolkit ai-data transform describe JOB_NAME [--region R]
",
    description: r"  ai-data       Dataset and metric tools of the AWS AI/ML skill (#4385).
                detect-format samples the first --sample-size bytes (default
                1 MiB, at most 16 MiB) of a local JSONL file or s3:// object and
                reports one of 11 training formats with its validation errors
                (exit 1 when invalid). validate-metrics checks a custom-metric
                JSON array (inline or a file). transform builds (plan) or
                submits and describes (submit, describe) a SageMaker Processing
                job; this build has no SageMaker or S3 client, so submit and
                describe exit 1 and s3:// inputs are refused. See
                docs/migration/ai-data.md.
",
    exit_notes: "",
    doc: "docs/migration/ai-data.md",
    synopsis_rank: 250,
    description_rank: 250,
};

#[cfg(test)]
mod tests;

use format::{
    detect_lines, sample_local_file, sample_s3_file, FormatDetectionResult, NoObjectStore,
    ObjectStore, DEFAULT_SAMPLE_BYTES, MAX_SAMPLE_BYTES,
};
use transform::{NoSageMaker, ProcessingService, TransformRequest};

/// Flags of `ai-data detect-format`.
pub(crate) const DETECT_FORMAT_FLAGS: &[&str] = &["--json"];
/// Value flags of `ai-data detect-format`.
pub(crate) const DETECT_FORMAT_VALUE_FLAGS: &[&str] = &["--sample-size"];
/// Value flags of `ai-data transform plan` and `ai-data transform submit`.
pub(crate) const TRANSFORM_VALUE_FLAGS: &[&str] = &[
    "--script",
    "--source-s3",
    "--output-s3",
    "--instance-type",
    "--region",
    "--execution-role",
    "--base-job-name",
    "--image-uri",
];
/// Value flags of `ai-data transform describe`.
pub(crate) const DESCRIBE_VALUE_FLAGS: &[&str] = &["--region"];

/// Python type name of a parsed JSON value, as `type(value).__name__` prints it.
pub(crate) fn py_type_name(value: &Json) -> &'static str {
    match value {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Int(_) => "int",
        Json::Float(_) => "float",
        Json::String(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// The reason text of a JSON syntax error: the parser's message with the
/// position (`Expecting value: line 1 column 1` in the Python module).
pub(crate) fn parse_failure_text(error: &ParseError) -> String {
    if error.column == 0 {
        format!("{}: line {}", error.message, error.line)
    } else {
        format!(
            "{}: line {} column {}",
            error.message, error.line, error.column
        )
    }
}

fn usage(message: &str) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", format!("invalid arguments: {message}"))
}

/// `json.dumps(value, indent=2)`: ASCII only, including U+007F.
fn dumps(value: &Json) -> String {
    json_dumps(value).replace('\u{7f}', "\\u007f")
}

fn count_json(count: usize) -> Json {
    Json::Int(i64::try_from(count).unwrap_or(i64::MAX))
}

fn parse_sample_size(text: &str) -> Result<u64, ToolkitError> {
    let Some(number) = py_int_parse(text) else {
        return Err(usage("--sample-size must be an integer"));
    };
    match u64::try_from(number) {
        Ok(size) if size <= MAX_SAMPLE_BYTES => Ok(size),
        Ok(_) => Err(usage(&format!(
            "--sample-size must be at most {MAX_SAMPLE_BYTES} bytes"
        ))),
        Err(_) => Err(usage("--sample-size must not be negative")),
    }
}

fn result_json(result: &FormatDetectionResult) -> String {
    let errors = result
        .errors
        .iter()
        .map(|error| {
            Json::object(vec![
                ("line_number", count_json(error.line_number)),
                ("error_type", Json::string(error.error_type)),
                ("message", Json::string(error.message.as_str())),
            ])
        })
        .collect();
    let mut pairs = vec![
        ("format_type", Json::string(result.format_type.value())),
        ("is_valid", Json::Bool(result.is_valid)),
        ("confidence", Json::string(result.confidence.value())),
        ("lines_sampled", count_json(result.lines_sampled)),
        ("errors", Json::Array(errors)),
    ];
    if result.errors_omitted > 0 {
        pairs.push(("errors_omitted", count_json(result.errors_omitted)));
    }
    dumps(&Json::object(pairs))
}

fn write_text_report(
    result: &FormatDetectionResult,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    output.line(&format!("Format: {}", result.format_type.value()))?;
    output.line(&format!(
        "Valid: {}",
        if result.is_valid {
            "\u{2713}"
        } else {
            "\u{2717}"
        }
    ))?;
    output.line(&format!("Confidence: {}", result.confidence.name()))?;
    output.line(&format!("Lines sampled: {}", result.lines_sampled))?;
    if !result.errors.is_empty() {
        output.line("Errors:")?;
        for error in &result.errors {
            output.line(&format!("  Line {}: {}", error.line_number, error.message))?;
            if output.is_closed() {
                return Ok(());
            }
        }
    }
    if result.errors_omitted > 0 {
        output.line(&format!(
            "  ... and {} more error(s) not shown",
            result.errors_omitted
        ))?;
    }
    Ok(())
}

/// Runs `detect-format` against the given object store.
fn detect_format_with(
    invocation: &Invocation,
    output: &mut Output<'_>,
    store: &dyn ObjectStore,
) -> Result<(), ToolkitError> {
    let Some(target) = invocation.positional(0) else {
        return Err(usage("FILE_PATH is required"));
    };
    let sample_size = match invocation.value("--sample-size") {
        Some(text) => parse_sample_size(text)?,
        None => DEFAULT_SAMPLE_BYTES,
    };
    let sampled = if target.starts_with("s3://") {
        sample_s3_file(target, sample_size, store)
    } else {
        sample_local_file(Path::new(target), sample_size)
    };
    let lines = match sampled {
        Ok(lines) => lines,
        Err(message) => {
            output.diagnostic(&format!("Error: {message}"));
            output.set_exit_status(1);
            return Ok(());
        }
    };
    let result = detect_lines(&lines);
    if invocation.has_flag("--json") {
        output.line(&result_json(&result))?;
    } else {
        write_text_report(&result, output)?;
    }
    if !result.is_valid {
        output.set_exit_status(1);
    }
    Ok(())
}

/// `toolkit ai-data detect-format`.
pub(crate) fn detect_format_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    detect_format_with(invocation, output, &NoObjectStore)
}

/// `toolkit ai-data validate-metrics`.
pub(crate) fn validate_metrics_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(argument) = invocation.positional(0) else {
        output.line(metrics::USAGE_LINE)?;
        output.set_exit_status(1);
        return Ok(());
    };
    let raw = metrics::read_input(argument)?;
    match metrics::validate(&raw) {
        Ok(count) => {
            let plural = if count == 1 { "" } else { "s" };
            output.line(&format!(
                "\u{2705} Valid \u{2014} {count} custom metric{plural} defined."
            ))
        }
        Err(errors) => {
            output.line("\u{274c} Validation failed:")?;
            for error in &errors {
                output.line(&format!("  - {error}"))?;
            }
            output.set_exit_status(1);
            Ok(())
        }
    }
}

fn transform_request(invocation: &Invocation) -> Result<TransformRequest, ToolkitError> {
    let required = |name: &str| {
        invocation
            .value(name)
            .map(str::to_string)
            .ok_or_else(|| usage(&format!("{name} is required")))
    };
    let optional = |name: &str| invocation.value(name).map(str::to_string);
    Ok(TransformRequest {
        script: PathBuf::from(required("--script")?),
        source_s3: required("--source-s3")?,
        output_s3: required("--output-s3")?,
        instance_type: optional("--instance-type")
            .unwrap_or_else(|| transform::DEFAULT_INSTANCE_TYPE.to_string()),
        region: optional("--region"),
        execution_role: optional("--execution-role"),
        base_job_name: optional("--base-job-name")
            .unwrap_or_else(|| transform::DEFAULT_BASE_JOB_NAME.to_string()),
        image_uri: optional("--image-uri"),
    })
}

fn failure(message: &str) -> ToolkitError {
    ToolkitError::runtime("ai_data_failed", message.to_string())
}

/// `toolkit ai-data transform plan`: the job specification, offline.
pub(crate) fn transform_plan_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let request = transform_request(invocation)?;
    let plan = transform::build_plan(&request).map_err(|message| failure(&message))?;
    output.line(&dumps(&plan.to_json()))
}

fn submit_with(
    invocation: &Invocation,
    output: &mut Output<'_>,
    service: &dyn ProcessingService,
) -> Result<(), ToolkitError> {
    let request = transform_request(invocation)?;
    let message = transform::submit(&request, service).map_err(|message| failure(&message))?;
    output.line(&message)
}

/// `toolkit ai-data transform submit`.
pub(crate) fn transform_submit_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    submit_with(invocation, output, &NoSageMaker)
}

fn describe_with(
    invocation: &Invocation,
    output: &mut Output<'_>,
    service: &dyn ProcessingService,
) -> Result<(), ToolkitError> {
    let Some(job_name) = invocation.positional(0) else {
        return Err(usage("JOB_NAME is required"));
    };
    let details = transform::describe(job_name, invocation.value("--region"), service)
        .map_err(|message| failure(&message))?;
    output.line(&dumps(&details))
}

/// `toolkit ai-data transform describe`.
pub(crate) fn transform_describe_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    describe_with(invocation, output, &NoSageMaker)
}
