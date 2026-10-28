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
//! files stay the single source of the contract. The same-model rule of
//! `model_identity.py` is the canonical `bedrock_resolve::identity` (#4403,
//! #4607); preflight and IAM are #4404.
//! See `docs/migration/bedrock-report.md` for the parity table and the
//! intentional changes.

use toolkit_core::{PathError, ToolkitError};

use crate::{Invocation, Output};

mod render;
mod validate;

/// Registration of the `bedrock-report` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "bedrock-report",
    commands: &[
        crate::CommandSpec {
            path: &["bedrock-report", "render"],
            flags: &[],
            value_flags: RENDER_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: render_command,
        },
        crate::CommandSpec {
            path: &["bedrock-report", "validate"],
            flags: &[],
            value_flags: VALIDATE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: validate_command,
        },
    ],
    synopsis: r"  toolkit bedrock-report render [PAYLOAD_JSON] [--phase-results DIR --repo DIR]
      [--date-suffix YYYY-MM-DD] [--results-dir DIR]
  toolkit bedrock-report validate (--schema analysis|ingestion|eval|rewrite|delta-decisions FILE
      | --check-run-context SAVED_JSON --current CURRENT_JSON)
",
    description: r"  bedrock-report
                Reports of the llm-to-bedrock skill (#4405). render prints the
                in-chat summary of a finished run from the phase-result files
                in --phase-results DIR (with --repo) or from one legacy
                PAYLOAD_JSON, and copies the repository's newest
                MIGRATION_REPORT_*.md to --results-dir (default
                ~/saws-migrate-results). validate checks one phase-result
                file against the schema analysis, ingestion, eval, rewrite or
                delta-decisions (RESULT=valid CONTROL=ok|blocked|partial,
                or RESULT=invalid and one line per finding, exit 1; exit 2
                for an unreadable file) or compares a saved and a current
                run context (RUN_CONTEXT=match, or mismatch and MISMATCH
                lines, exit 1). Findings never print values of the checked
                file. See docs/migration/bedrock-report.md.
",
    exit_notes: "",
    doc: "docs/migration/bedrock-report.md",
    synopsis_rank: 280,
    description_rank: 280,
};

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
