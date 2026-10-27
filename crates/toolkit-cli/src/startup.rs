//! AWS startup advisor validators (#4399, slice `startup-validators`).
//!
//! Three commands port the repository-health scripts of
//! `plugins/aws-startup-advisor/scripts`:
//!
//! | Command | Python original |
//! | --- | --- |
//! | `startup model-pricing` | `validate-model-pricing.py` |
//! | `startup program-artifacts` | `validate-startup-program-artifacts.py` |
//! | `startup model-id-lint` | `model-id-lint.py` |
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
mod model_ids;
mod pricing;
mod program;
mod text;

pub use artifacts::artifacts_command;
pub use model_ids::model_id_lint_command;
pub use pricing::pricing_command;
pub use program::program_artifacts_command;

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
