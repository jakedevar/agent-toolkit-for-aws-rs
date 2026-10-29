//! Git ignore rules for the upload selection (R-AR-GIT; Python
//! `_git_included_files`, `_gitignore_files_without_repo`,
//! `_has_git_metadata`, `_has_gitignore`).
//!
//! Git runs from the configured program path with `current_dir = root`,
//! `stdin` null, `stderr` discarded, `stdout` collected into a [`CappedBuf`]
//! (256 MiB, #5082), a wall-clock limit per call (30 s; the whole process
//! group is killed at the limit), `-c core.fsmonitor=false` prepended, and
//! the caller's `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
//! `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`,
//! `GIT_ALTERNATE_OBJECT_DIRECTORIES` and every `GIT_CONFIG*` variable
//! removed, so the selection always describes `root`. Git's output only
//! selects names; it never supplies archive bytes.
//!
//! [`CappedBuf`]: crate::launch::buffer::CappedBuf

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::fsops::DirHandle;
use crate::launch::clock::Random;
use crate::launch::LaunchError;

/// Wall-clock limit per Git call.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest captured Git output.
pub const GIT_STDOUT_CAP: usize = 256 * 1024 * 1024;

/// The fixed messages (Python parity).
pub mod message {
    /// `ls-files --ignored` failed after the selection succeeded.
    pub const TRACKED_IGNORED: &str =
        "Git could not determine which tracked files match ignore rules.";
    /// The selection failed in a Git worktree (or the output was refused).
    pub const SELECTION: &str = "Git could not determine which files are safe to upload. \
                                 Resolve the Git error and try again.";
    /// A call passed the time limit.
    pub const TIMEOUT: &str = "Git timed out while determining which files are safe to upload.";
    /// Git could not run while rules apply.
    pub const REQUIRED: &str =
        "Git is required to apply this source directory's ignore rules before upload.";
    /// `git init --bare` failed for the temporary metadata.
    pub const INIT: &str = "Git could not initialize temporary metadata to apply ignore rules.";
}

/// How Git is run.
#[derive(Debug, Clone)]
pub struct GitRunner {
    /// The program (`git` from `PATH` in production).
    pub program: PathBuf,
    /// Per-call wall-clock limit.
    pub timeout: Duration,
    /// Largest captured output.
    pub stdout_cap: usize,
}

impl GitRunner {
    /// `git` from `PATH` with the production limits.
    #[must_use]
    pub fn system() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: GIT_TIMEOUT,
            stdout_cap: GIT_STDOUT_CAP,
        }
    }
}

/// The paths (relative to `root`, `/`-separated) Git selects, or `None` when
/// no ignore rules apply.
///
/// `root_path` is the absolute root Git runs in; `root` is the held
/// descriptor the `.gitignore` search walks; `temp_parent` holds the
/// private temporary metadata of a tree outside Git.
///
/// # Errors
/// The Python messages of [`message`], or the non-UTF-8 name refusal.
pub fn git_included_files(
    runner: &GitRunner,
    root_path: &Path,
    root: &DirHandle,
    temp_parent: &Path,
    random: &dyn Random,
) -> Result<Option<HashSet<String>>, LaunchError> {
    let _ = (runner, root_path, root, temp_parent, random);
    todo!("L2: git selection")
}

/// Whether `root` (absolute) is at or below a directory containing `.git`.
#[must_use]
pub fn has_git_metadata(root: &Path) -> bool {
    let _ = root;
    todo!("L2: has_git_metadata")
}

/// Whether the tree below `root` holds a `.gitignore`, skipping the
/// `_SKIP_DIRS` directories and never following a symlink.
#[must_use]
pub fn has_gitignore(root: &DirHandle) -> bool {
    let _ = root;
    todo!("L2: has_gitignore")
}

#[cfg(test)]
mod tests_git;
