//! Bedrock migration report rendering and phase-result validation (#4405,
//! slice `bedrock-report`).
//!
//! - `toolkit bedrock-report render [PAYLOAD_JSON] [--phase-results DIR --repo DIR]
//!   [--date-suffix S] [--results-dir DIR]` replaces `render_report.py`.
//! - `toolkit bedrock-report validate (--schema NAME FILE | --check-run-context
//!   SAVED --current CURRENT)` replaces `validate_result.py`.
//!
//! Both scripts belong to the `llm-to-bedrock` skill of the
//! `aws-startup-advisor` plugin. The five phase-result schemas are the plugin's
//! own `scripts/schemas/*.json` files, compiled into the binary, so the schema
//! files stay the single source of the contract. Model identity resolution
//! (`model_identity.py`) is #4403 and preflight/IAM is #4404; only the
//! `same_model` rule `validate_result.py` needs is mirrored in `identity`.
//! See `docs/migration/bedrock-report.md` for the parity table and the
//! intentional changes.

use toolkit_core::{PathError, ToolkitError};

use crate::{Invocation, Output};

mod identity;
mod render;
mod validate;

#[cfg(test)]
mod contracts;
#[cfg(test)]
mod tests;

/// Value flags of `bedrock-report render`.
pub(crate) const RENDER_VALUE_FLAGS: &[&str] = &[
    "--phase-results",
    "--repo",
    "--date-suffix",
    "--results-dir",
];
/// Value flags of `bedrock-report validate`.
pub(crate) const VALIDATE_VALUE_FLAGS: &[&str] = &["--schema", "--check-run-context", "--current"];

/// `toolkit bedrock-report render`.
pub(crate) fn render_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    render::command(invocation, output)
}

/// `toolkit bedrock-report validate`.
pub(crate) fn validate_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    validate::command(invocation, output)
}

/// An `argparse`-style usage error (exit status 2).
fn usage(message: &str) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", format!("invalid arguments: {message}"))
}

/// Why a file could not be read, in words that never repeat the path.
fn path_reason(error: &PathError) -> String {
    match error {
        PathError::NotFound(_) => "No such file or directory".to_string(),
        PathError::Symlink(_) => "refusing to follow a symbolic link".to_string(),
        PathError::NotRegularFile(_) => "not a regular file".to_string(),
        PathError::TooLarge { limit, .. } => format!("larger than {limit} bytes"),
        PathError::NotUtf8(_) => "not valid UTF-8".to_string(),
        PathError::Changed(_) => "changed while it was being opened".to_string(),
        PathError::Invalid(_) => "invalid path".to_string(),
        PathError::Io(error, _) | PathError::WriteFailed(error, _) => error.to_string(),
    }
}
