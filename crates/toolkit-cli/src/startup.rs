//! AWS startup advisor validators (#4399, slice `startup-validators`).
//!
//! Several commands port the repository-health scripts of
//! `plugins/aws-startup-advisor/scripts`:
//!
//! | Command | Python original |
//! | --- | --- |
//! | `startup model-pricing` | `validate-model-pricing.py` |
//! | `startup program-artifacts` | `validate-startup-program-artifacts.py` |
//! | `startup model-id-lint` | `model-id-lint.py` |
//! | `startup artifacts` | `validate-artifacts.py` |
//! | `startup emit-plan` | `emit-plan-json.py` |
//! | `startup consent-audit` | `consent-disclosure-audit.py` |
//! | `startup pricing-parity` | `check-bedrock-pricing-parity.py` |
//! | `startup terraform-policy` | `validate-terraform-policy.py` (tf-best-practices) |
//! | `startup migration-report` | `validate-migration-report.py` |
//!
//! Each prints the Python messages unchanged: findings go to stderr through
//! [`Output::diagnostic`] and a failing run additionally ends with one
//! `error: ...` summary line (exit status 1). The shared contracts live here:
//! bounded, symlink-refusing text reads with Python's universal newlines and a
//! `Path.is_file()` that refuses links instead of following them. The full
//! parity table and every deliberate difference is in
//! `docs/migration/startup-validators.md`.

use std::fs;
use std::io;
use std::path::Path;

use toolkit_core::read_regular_file;

use crate::manifests::{read_failure, universal_newlines};

mod artifacts;
mod consent;
mod model_ids;
mod parity;
mod plan;
mod pricing;
mod program;
mod report;
mod terraform;
pub(crate) mod text;

pub use artifacts::artifacts_command;
pub use consent::consent_audit_command;
pub use model_ids::model_id_lint_command;
pub use parity::pricing_parity_command;
pub use plan::emit_plan_command;
pub use pricing::pricing_command;
pub use program::program_artifacts_command;
pub use report::migration_report_command;
pub use terraform::terraform_policy_command;

/// Largest artifact or plugin file the validators read (64 MiB).
pub(crate) const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// `Path.is_file()` that never follows a symbolic link: a link is an error
/// (the core threat model refuses links) instead of a silent pass, while a
/// missing path or a path through a non-directory is simply "not a file".
pub(crate) fn is_regular_file(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "cannot read {}: symbolic links are not followed",
            path.display()
        )),
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if is_missing(&error) => Ok(false),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// Refuses a symbolic link at any component of `path` below `root`.
///
/// `path` is a validator-owned relative path joined to the selected `root`
/// (the root itself is the caller's explicit choice and is not examined). A
/// component that does not exist, or lies below a regular file, ends the walk:
/// the caller's own presence check then reports the path absent. A dangling
/// link is still a link and is refused (#4584).
pub(crate) fn refuse_links_below(root: &Path, path: &Path) -> Result<(), String> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "cannot read {}: symbolic links are not followed",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => return Ok(()),
            Ok(_) => {}
            Err(error) if is_missing(&error) => return Ok(()),
            Err(error) => return Err(format!("cannot read {}: {error}", current.display())),
        }
    }
    Ok(())
}

/// [`is_regular_file`] for a path below `root`: a link at any component is an
/// error, not only at the last one.
pub(crate) fn is_regular_file_under(root: &Path, path: &Path) -> Result<bool, String> {
    refuse_links_below(root, path)?;
    is_regular_file(path)
}

/// [`read_text`] for a path below `root`, refusing a link at every component.
pub(crate) fn read_text_under(root: &Path, path: &Path, shown: &str) -> Result<String, String> {
    refuse_links_below(root, path)?;
    read_text(path, shown)
}

/// `ENOENT`, or `ENOTDIR` (20) for a path below a regular file.
fn is_missing(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(20)
}

/// Reads a bounded UTF-8 text file with Python's universal newlines. `shown`
/// is the name used in the error message.
pub(crate) fn read_text(path: &Path, shown: &str) -> Result<String, String> {
    read_regular_file(path, MAX_ARTIFACT_BYTES)
        .map(|text| universal_newlines(&text))
        .map_err(|error| format!("cannot read {shown}: {}", read_failure(&error)))
}

#[cfg(test)]
mod tests;
