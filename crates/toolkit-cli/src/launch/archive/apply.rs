//! `launch apply-snapshot` (R-AR-APPLY, #5040; R-AR-META, #5061), the
//! replacement of `rsync -a <outputDir>/ .`.
//!
//! - Phase 0 ([`super::meta`]): the environment precondition, one hardened
//!   `git rev-parse`, the protected identities and the root containment
//!   walk, before the snapshot is read.
//! - Phase 1 (nothing is written): the snapshot is walked through held
//!   no-follow descriptors under the node, file and byte limits of
//!   R-AR-PLAN; only directories and regular files with safe, non-VCS names
//!   pass, and no snapshot directory may be Git-shaped. Every planned path
//!   is checked on the target side: no symlink, no type conflict, no special
//!   file, and every existing directory entered is neither protected nor
//!   Git-shaped; its identity is recorded.
//! - Phase 2: directories are created `0755`; each file is written to a
//!   private temporary name in the held target parent, `fchmod`ed, filled
//!   from the held source descriptor, synced and renamed over the target
//!   name. Every directory is re-entered `O_NOFOLLOW` with its phase-1
//!   identity, so a swap after planning stops the apply before that write.

use std::io;
use std::path::{Path, PathBuf};

use super::git::GitRunner;
use super::GIB;
use crate::launch::clock::Random;
use crate::launch::LaunchError;
use toolkit_aws::Env;

/// The walk limits (R-AR-PLAN's constants; tests inject smaller ones).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyLimits {
    /// Most files (100 000).
    pub max_files: usize,
    /// Most nodes, files and directories (200 000).
    pub max_nodes: usize,
    /// Most bytes in total (2 GiB).
    pub max_bytes: u64,
}

impl ApplyLimits {
    /// The production limits.
    pub const PRODUCTION: Self = Self {
        max_files: 100_000,
        max_nodes: super::plan::MAX_SNAPSHOT_NODES,
        max_bytes: 2 * GIB,
    };
}

/// Which R-AR-META target-side rules run (both in production; T-APPLY-11
/// disables one at a time to show the other refuses on its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaRules {
    /// The protected-identity rule.
    pub identity: bool,
    /// The Git-shaped directory rule.
    pub shaped: bool,
}

impl MetaRules {
    /// Both rules.
    pub const PRODUCTION: Self = Self {
        identity: true,
        shaped: true,
    };
}

/// The `ApplyHook` seam between the phases and before each file's rename
/// (no-op in production).
pub trait ApplyHook {
    /// After phase 1, before the first write.
    fn between_phases(&mut self) {}
    /// After file number `index`'s temporary file is written, before its
    /// rename; an error stops the apply as a write failure would.
    ///
    /// # Errors
    /// The injected failure.
    fn before_rename(&mut self, _index: usize) -> io::Result<()> {
        Ok(())
    }
}

/// The production hook.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoApplyHook;

impl ApplyHook for NoApplyHook {}

/// What [`apply_snapshot`] uses.
pub struct ApplyOptions<'a> {
    /// The Git used for discovery.
    pub git: &'a GitRunner,
    /// The caller's environment (the R-AR-META precondition).
    pub env: &'a dyn Env,
    /// Walk limits.
    pub limits: ApplyLimits,
    /// R-AR-META rules.
    pub rules: MetaRules,
    /// Race and failure seam.
    pub hook: &'a mut dyn ApplyHook,
    /// Temporary names.
    pub random: &'a dyn Random,
}

/// What was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Files written.
    pub files: usize,
    /// Directories created.
    pub directories: usize,
    /// The `--into` path as given.
    pub into: PathBuf,
}

/// The snapshot as phase 1 walked it.
#[derive(Debug)]
pub struct SnapshotTree {
    /// Files.
    pub files: usize,
    /// Directories.
    pub directories: usize,
    /// Bytes in total.
    pub bytes: u64,
}

/// Walk the snapshot rooted at `snapshot` under `limits` (phase 1's source
/// side); nothing is written.
///
/// # Errors
/// The first refusal.
pub fn walk_snapshot(snapshot: &Path, limits: &ApplyLimits) -> Result<SnapshotTree, LaunchError> {
    let _ = (snapshot, limits);
    todo!("R-AR-APPLY phase 1")
}

/// Apply the extracted snapshot at `snapshot` to the work tree at `into`.
///
/// # Errors
/// Any refusal before the first write, or the phase-2 stop message.
pub fn apply_snapshot(
    snapshot: &Path,
    into: &Path,
    options: &mut ApplyOptions<'_>,
) -> Result<Applied, LaunchError> {
    let _ = (snapshot, into, options);
    todo!("R-AR-APPLY")
}

#[cfg(test)]
mod tests_apply;
