//! Held-descriptor staging, extraction and exclusive publication
//! (R-AR-STAGE, #5047; R-AR-EXT steps 5-7; R-AR-PLAN's use by staging).
//!
//! [`prepare_output`] runs before any request: it opens the parent of the
//! output once (following the caller-named path), applies the parent policy
//! and refuses an existing destination name. [`extract`] then creates the
//! private staging directory `.launch-snapshot-<16 hex>.partial` with its
//! `tree` inside the held parent, creates exactly the plan's nodes below
//! `tree` (every step relative to held descriptors, every directory entered
//! `O_NOFOLLOW` with its recorded identity), decodes each file through
//! R-ZIP-FRAME step 8 and publishes `tree` with an exclusive rename whose
//! source and result are identity-checked. Any failure removes exactly the
//! entries the ledger recorded, deepest first, descriptor-relative.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use super::fsops::DirHandle;
use super::plan::Plan;
use super::zip_read::Archive;
use crate::launch::clock::Random;
use crate::launch::LaunchError;

/// `(st_dev, st_ino)`.
pub type Ident = (u64, u64);

/// Where the race seam runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    /// Before creating the `index`-th plan node (0 = the first creation
    /// inside `tree`; the staging directory already exists).
    BeforeCreate {
        /// Creation number.
        index: usize,
    },
    /// Before the exclusive rename that publishes the tree.
    BeforePublish,
}

/// The `ExtractHook` race seam (no-op in production).
pub trait ExtractHook {
    /// Runs at `point`.
    fn before(&mut self, _point: HookPoint) {}
    /// Runs after each decoded piece is written, with the file's running
    /// byte count.
    fn written(&mut self, _node: u32, _total: u64) {}
}

/// The production hook.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoExtractHook;

impl ExtractHook for NoExtractHook {}

/// The output destination, checked and with its parent held.
#[derive(Debug)]
pub struct OutputTarget {
    parent: DirHandle,
    name: CString,
    path: PathBuf,
}

impl OutputTarget {
    /// The caller's output path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The owner and mode facts of the parent policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentFacts {
    /// `st_uid`.
    pub uid: u32,
    /// `st_mode`.
    pub mode: u32,
}

/// What extraction created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extracted {
    /// Files (from the plan).
    pub files: usize,
    /// Directories (from the plan).
    pub directories: usize,
    /// Decoded bytes written.
    pub bytes: u64,
}

/// Ledger instrumentation (T-PLAN-6).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LedgerStats {
    /// Plan length the ledger was sized for.
    pub plan_len: usize,
    /// Ledger capacity right after its one allocation.
    pub capacity_reserved: usize,
    /// Largest capacity seen while extracting.
    pub capacity_max: usize,
    /// Entries recorded when extraction stopped.
    pub len_at_stop: usize,
    /// Entries cleanup removed.
    pub removed: usize,
    /// Entries cleanup left alone (identity changed or removal failed).
    pub left: usize,
}

/// "The parent of the output directory can be modified by other users".
#[must_use]
pub fn parent_policy_message(shown_path: &str) -> String {
    format!(
        "The parent of the output directory can be modified by other users: {shown_path}. \
         Choose a private directory."
    )
}

/// "Output directory already exists".
#[must_use]
pub fn exists_message(shown_path: &str) -> String {
    format!("Output directory already exists: {shown_path}.")
}

/// The parent policy: owned by `euid` or root, and group/other-writable
/// only with the sticky bit.
#[must_use]
pub fn parent_policy_allows(facts: ParentFacts, euid: u32) -> bool {
    let _ = (facts, euid);
    todo!("R-AR-STAGE parent policy")
}

/// Check `path` as the output directory before any request.
///
/// # Errors
/// A final component that is empty, `.` or `..`; a missing parent; the
/// parent policy; an existing destination name.
pub fn prepare_output(path: &Path) -> Result<OutputTarget, LaunchError> {
    let _ = path;
    todo!("R-AR-STAGE prepare")
}

/// Extract `archive` per `plan` into staging under `target`, then publish.
///
/// # Errors
/// Any creation, decode or publication failure, after cleanup.
pub fn extract(
    target: &OutputTarget,
    archive: &Archive<'_>,
    plan: &Plan,
    random: &dyn Random,
    hook: &mut dyn ExtractHook,
    stats: &mut LedgerStats,
) -> Result<Extracted, LaunchError> {
    let _ = (target, archive, plan, random, hook, stats);
    todo!("R-AR-STAGE extract")
}

#[cfg(test)]
mod tests_stage;
