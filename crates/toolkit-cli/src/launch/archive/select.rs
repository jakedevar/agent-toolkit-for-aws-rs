//! The upload source walk (R-AR-SEL, R-AR-OPEN; Python `zip_local_repo`).
//!
//! The caller's path is opened once ([`fsops::open_root`]); every listing,
//! classification and open below it is relative to held descriptors. The
//! walk never follows a symlink below the root, skips every non-regular file
//! (a FIFO is never opened), applies the Python skip tables exactly, visits
//! names in byte order per directory, and reads each selected file through a
//! handle whose type and identity were checked first, at most its walked
//! size plus one byte (capped at the per-entry limit). Any change seen
//! during the walk fails the build before an archive exists, so no upload
//! request can follow.
//!
//! [`WalkHook`] is the race seam of the tests: it is called between the
//! classification and the open of every file and between the listing and
//! the entering of every directory. Production uses [`NoHook`].

use std::path::{Path, PathBuf};

use super::fsops;
use super::git::GitRunner;
use super::ArchiveLimits;
use crate::launch::clock::Random;
use crate::launch::LaunchError;

/// Directory names never entered (`_SKIP_DIRS`, exact match).
pub const SKIP_DIRS: [&str; 28] = [
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    ".parcel-cache",
    "dist",
    "build",
    "out",
    "coverage",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".idea",
    ".vscode",
    ".aws",
    ".ssh",
    ".gnupg",
    ".gcp",
    ".azure",
    ".docker",
    ".kube",
];

/// File names skipped as secrets (`_SKIP_FILES`, case-insensitive).
pub const SKIP_FILES: [&str; 13] = [
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".env.test",
    ".env.staging",
    "credentials",
    "credentials.json",
    ".git-credentials",
    ".netrc",
    ".pypirc",
    ".npmrc",
    ".htpasswd",
];

/// Secret suffixes (`_SKIP_SUFFIXES`, case-insensitive).
pub const SKIP_SUFFIXES: [&str; 6] = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"];

/// Secret name prefixes (`_SKIP_NAME_PREFIXES`, case-insensitive).
pub const SKIP_NAME_PREFIXES: [&str; 4] = ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"];

/// Deepest directory nesting walked (R-AR-OPEN: 4096 / 2).
pub const MAX_DEPTH: usize = 2048;

/// Python `_is_secret_file`.
#[must_use]
pub fn is_secret_file(name: &str) -> bool {
    let _ = name;
    todo!("L2: secret names")
}

/// Whether a directory name is in [`SKIP_DIRS`].
#[must_use]
pub fn is_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// The race seam.
pub trait WalkHook {
    /// Between classifying the regular file `rel` and opening it.
    fn before_open(&self, rel: &str) {
        let _ = rel;
    }
    /// Between listing the directory `rel` in its parent and entering it.
    fn before_enter(&self, rel: &str) {
        let _ = rel;
    }
}

/// The production hook: does nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHook;

impl WalkHook for NoHook {}

/// Everything [`build_archive`] needs besides the caller's arguments.
pub struct BuildOptions<'a> {
    /// How Git runs.
    pub git: &'a GitRunner,
    /// The race seam.
    pub hook: &'a dyn WalkHook,
    /// Archive limits.
    pub limits: ArchiveLimits,
    /// `HOME`, for `~` expansion.
    pub home: Option<&'a Path>,
    /// The current directory, for a relative path.
    pub cwd: &'a Path,
    /// Parent of the private temporary Git metadata.
    pub temp_parent: &'a Path,
    /// Randomness for temporary names.
    pub random: &'a dyn Random,
}

/// A finished upload archive.
#[derive(Debug)]
pub struct BuiltArchive {
    /// The archive bytes (at most the archive limit).
    pub bytes: Vec<u8>,
    /// Files archived.
    pub files: usize,
    /// Relative paths skipped as secrets (Python logged them at INFO).
    pub skipped_secrets: Vec<String>,
}

/// The absolute, lexically normalised form of `path` (Python
/// `os.path.abspath(os.path.expanduser(path))`).
#[must_use]
pub fn absolute_root(path: &str, home: Option<&Path>, cwd: &Path) -> PathBuf {
    let _ = (path, home, cwd);
    todo!("L2: abspath")
}

/// Build the upload archive of the directory `path` under the top-level
/// name `sanitize_root_name(root_name)`.
///
/// # Errors
/// The Python `ArchiveError` messages ("Not a directory", "No files found",
/// the limits, Git), the changed-file refusal and the non-UTF-8 refusal.
pub fn build_archive(
    path: &str,
    root_name: &str,
    options: &BuildOptions<'_>,
) -> Result<BuiltArchive, LaunchError> {
    let _ = (path, root_name, options, fsops::MAX_LISTED_NAMES);
    todo!("L2: build_archive")
}

#[cfg(test)]
mod tests_select;
