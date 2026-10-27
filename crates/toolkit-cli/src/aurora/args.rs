//! Argument decoding with Python `argparse` semantics and messages.
//!
//! Routing (which flags exist, `--flag=value`, repeated flags) is the shared
//! router's job. These helpers only decode the text of a value flag the way
//! `type=float` / `type=int` / `choices=[...]` did, and report mistakes as the
//! usual usage error (exit status 2).

use toolkit_core::ToolkitError;

use crate::manifests::py_repr_str;
use crate::Invocation;

/// Output format of an analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Json,
    Table,
}

/// A usage error with the code every argument mistake shares.
pub(crate) fn usage(message: impl Into<String>) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", message)
}

/// Python `float(text)` as `argparse` applies it (surrounding whitespace and
/// single underscores between digits are accepted).
pub(crate) fn parse_py_float(text: &str) -> Option<f64> {
    let cleaned = strip_underscores(text.trim())?;
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// Python `int(text)` restricted to the `i64` range (larger magnitudes are
/// rejected, see the slice document).
pub(crate) fn parse_py_int(text: &str) -> Option<i64> {
    let cleaned = strip_underscores(text.trim())?;
    cleaned.parse::<i64>().ok()
}

/// Removes `_` separators, which Python allows only between two digits.
fn strip_underscores(text: &str) -> Option<String> {
    if !text.contains('_') {
        return Some(text.to_string());
    }
    let chars: Vec<char> = text.chars().collect();
    for (index, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = index > 0 && chars[index - 1].is_ascii_digit();
            let after = chars.get(index + 1).is_some_and(char::is_ascii_digit);
            if !(before && after) {
                return None;
            }
        }
    }
    Some(text.replace('_', ""))
}

/// `type=float` value of `flag`, if given.
pub(crate) fn float_value(
    invocation: &Invocation,
    flag: &str,
) -> Result<Option<f64>, ToolkitError> {
    match invocation.value(flag) {
        None => Ok(None),
        Some(text) => parse_py_float(text).map(Some).ok_or_else(|| {
            usage(format!(
                "argument {flag}: invalid float value: {}",
                py_repr_str(text)
            ))
        }),
    }
}

/// `type=int` value of `flag`, if given.
pub(crate) fn int_value(invocation: &Invocation, flag: &str) -> Result<Option<i64>, ToolkitError> {
    match invocation.value(flag) {
        None => Ok(None),
        Some(text) => parse_py_int(text).map(Some).ok_or_else(|| {
            usage(format!(
                "argument {flag}: invalid int value: {}",
                py_repr_str(text)
            ))
        }),
    }
}

/// `--format` (`json` by default; `choices=["json", "table"]`).
pub(crate) fn format_value(invocation: &Invocation) -> Result<Format, ToolkitError> {
    match invocation.value("--format") {
        None | Some("json") => Ok(Format::Json),
        Some("table") => Ok(Format::Table),
        Some(other) => Err(usage(format!(
            "argument --format: invalid choice: {} (choose from 'json', 'table')",
            py_repr_str(other)
        ))),
    }
}

/// `--region`, defaulting to `us-east-1`.
pub(crate) fn region_value(invocation: &Invocation) -> String {
    invocation
        .value("--region")
        .unwrap_or("us-east-1")
        .to_string()
}

/// The `argparse` message for required options that are missing.
pub(crate) fn required(missing: &[&str]) -> Result<(), ToolkitError> {
    if missing.is_empty() {
        Ok(())
    } else {
        Err(usage(format!(
            "the following arguments are required: {}",
            missing.join(", ")
        )))
    }
}

/// Rejects flags that exist for another mode of the same command, like
/// `argparse` rejecting an option its parser does not define.
pub(crate) fn reject_unrecognized(
    invocation: &Invocation,
    flags: &[&str],
) -> Result<(), ToolkitError> {
    let present: Vec<&str> = flags
        .iter()
        .copied()
        .filter(|flag| invocation.has_flag(flag) || invocation.value(flag).is_some())
        .collect();
    if present.is_empty() {
        Ok(())
    } else {
        Err(usage(format!(
            "unrecognized arguments: {}",
            present.join(" ")
        )))
    }
}
