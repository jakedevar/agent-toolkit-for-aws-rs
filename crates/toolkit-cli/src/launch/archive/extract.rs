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

use std::ffi::{CStr, CString, OsStr};
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::fsops::{self, DirHandle};
use super::nav::{Ident, Navigator, Tree, ROOT};
use super::plan::{Node, NodeKind, Plan};
use super::zip_read::{decode_entry, Archive};
use crate::launch::clock::{random_hex, Random};
use crate::launch::text::shown;
use crate::launch::LaunchError;

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

/// "The output directory must end in a name".
fn final_component_message(shown_path: &str) -> String {
    format!(
        "The output directory must end in a name (not empty, `.` or `..`): {shown_path}."
    )
}

/// A staging, creation or cleanup failure that is not a decode failure.
fn io_failure(error: &io::Error) -> LaunchError {
    LaunchError::Archive(format!("The snapshot could not be extracted: {error}."))
}

fn changed_failure() -> LaunchError {
    LaunchError::Archive(
        "The staging directory changed during extraction; nothing was published.".to_string(),
    )
}

/// The parent policy: owned by `euid` or root, and group/other-writable
/// only with the sticky bit.
#[must_use]
pub fn parent_policy_allows(facts: ParentFacts, euid: u32) -> bool {
    let owner_ok = facts.uid == euid || facts.uid == 0;
    let writable_by_others = facts.mode & 0o022 != 0;
    let sticky = facts.mode & 0o1000 != 0;
    owner_ok && (!writable_by_others || sticky)
}

/// Check `path` as the output directory before any request.
///
/// # Errors
/// A final component that is empty, `.` or `..`; a missing parent; the
/// parent policy; an existing destination name.
pub fn prepare_output(path: &Path) -> Result<OutputTarget, LaunchError> {
    let shown_path = shown(&path.to_string_lossy());
    let bytes = path.as_os_str().as_bytes();
    let (parent_bytes, name_bytes): (&[u8], &[u8]) =
        match bytes.iter().rposition(|byte| *byte == b'/') {
            Some(0) => (b"/", &bytes[1..]),
            Some(at) => (&bytes[..at], &bytes[at + 1..]),
            None => (b".", bytes),
        };
    let refused = || LaunchError::Archive(final_component_message(&shown_path));
    if name_bytes.is_empty() || name_bytes == b"." || name_bytes == b".." {
        return Err(refused());
    }
    let name = CString::new(name_bytes).map_err(|_| refused())?;
    let parent = fsops::open_root(Path::new(OsStr::from_bytes(parent_bytes))).map_err(|error| {
        LaunchError::Archive(
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) {
                format!("The parent of the output directory does not exist: {shown_path}.")
            } else {
                format!("The parent of the output directory could not be opened: {shown_path}.")
            },
        )
    })?;
    let (uid, mode) = fsops::owner_and_mode(&parent).map_err(|error| io_failure(&error))?;
    if !parent_policy_allows(ParentFacts { uid, mode }, fsops::euid()) {
        return Err(LaunchError::Archive(parent_policy_message(&shown_path)));
    }
    match fsops::stat_at(&parent, &name) {
        Ok(_) => return Err(LaunchError::Archive(exists_message(&shown_path))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_failure(&error)),
    }
    Ok(OutputTarget {
        parent,
        name,
        path: path.to_path_buf(),
    })
}

/// The plan as the navigator sees it.
struct PlanTree<'a> {
    nodes: &'a [Node],
    depth: Vec<u16>,
    data: &'a [u8],
}

impl Tree for PlanTree<'_> {
    fn parent(&self, node: u32) -> u32 {
        self.nodes[node as usize].parent
    }

    fn depth(&self, node: u32) -> usize {
        usize::from(self.depth[node as usize])
    }

    fn name(&self, node: u32) -> CString {
        let node = &self.nodes[node as usize];
        let start = node.name_offset as usize;
        let bytes = &self.data[start..start + usize::from(node.name_len)];
        // Names passed R-AR-READ: no NUL byte.
        CString::new(bytes).unwrap_or_default()
    }
}

impl<'a> PlanTree<'a> {
    fn new(plan: &'a Plan, data: &'a [u8]) -> Self {
        let nodes = plan.nodes();
        let mut depth = Vec::with_capacity(nodes.len());
        for node in nodes {
            // Parents precede children in plan order.
            let parent_depth = if node.parent == ROOT {
                0
            } else {
                depth[node.parent as usize]
            };
            depth.push(parent_depth + 1);
        }
        Self { nodes, depth, data }
    }
}

/// Depth-first pre-order of the plan (parents first, siblings in plan
/// order), so the navigator's steps are linear in the node count.
fn dfs_order(nodes: &[Node]) -> Vec<u32> {
    let count = nodes.len();
    let slot = |parent: u32| {
        if parent == ROOT {
            count
        } else {
            parent as usize
        }
    };
    let mut start = vec![0_u32; count + 2];
    for node in nodes {
        start[slot(node.parent) + 1] += 1;
    }
    for index in 1..start.len() {
        start[index] += start[index - 1];
    }
    let mut fill = start.clone();
    let mut children = vec![0_u32; count];
    for (id, node) in (0_u32..).zip(nodes) {
        let at = &mut fill[slot(node.parent)];
        children[*at as usize] = id;
        *at += 1;
    }
    let range = |parent: usize| start[parent] as usize..start[parent + 1] as usize;
    let mut order = Vec::with_capacity(count);
    let mut stack: Vec<u32> = children[range(count)].iter().rev().copied().collect();
    while let Some(node) = stack.pop() {
        order.push(node);
        stack.extend(children[range(node as usize)].iter().rev());
    }
    order
}

/// The private staging directory and its tree.
struct Staging {
    name: CString,
    dir: DirHandle,
    tree: DirHandle,
}

/// A held directory that must be ours, a directory, and private.
fn check_private(dir: &DirHandle) -> io::Result<bool> {
    let (uid, mode) = fsops::owner_and_mode(dir)?;
    Ok(uid == fsops::euid() && mode & 0o077 == 0)
}

fn create_staging(parent: &DirHandle, random: &dyn Random) -> Result<Staging, LaunchError> {
    let name = CString::new(format!(".launch-snapshot-{}.partial", random_hex(random, 8)?))
        .map_err(|_| changed_failure())?;
    fsops::mkdir_at(parent, &name, 0o700).map_err(|error| io_failure(&error))?;
    let dir = match fsops::enter_dir(parent, &name) {
        Ok(dir) if check_private(&dir).unwrap_or(false) => dir,
        _ => return Err(changed_failure()),
    };
    let tree_name = c"tree";
    let tree = fsops::mkdir_at(&dir, tree_name, 0o700)
        .map_err(|error| io_failure(&error))
        .and_then(|()| fsops::enter_dir(&dir, tree_name).map_err(|_| changed_failure()))
        .and_then(|tree| {
            fsops::chmod_dir(&tree, 0o700).map_err(|error| io_failure(&error))?;
            if check_private(&tree).unwrap_or(false) {
                Ok(tree)
            } else {
                Err(changed_failure())
            }
        });
    match tree {
        Ok(tree) => Ok(Staging { name, dir, tree }),
        Err(error) => {
            let _ = fsops::remove_at(&dir, tree_name, true);
            remove_if_same(parent, &name, dir.identity());
            Err(error)
        }
    }
}

/// `unlinkat(dir, name, AT_REMOVEDIR)` only while `name` is still `ident`.
fn remove_if_same(dir: &DirHandle, name: &CStr, ident: Ident) -> bool {
    match fsops::stat_at(dir, name) {
        Ok(stat)
            if stat.kind == fsops::EntryType::Directory && (stat.dev, stat.ino) == ident =>
        {
            fsops::remove_at(dir, name, true).is_ok()
        }
        _ => false,
    }
}

/// One extraction run: the plan, the ledger and the navigator.
struct Run<'a> {
    archive: &'a Archive<'a>,
    plan: &'a Plan,
    tree: PlanTree<'a>,
    idents: Vec<Option<Ident>>,
    ledger: Vec<(u32, u64, u64)>,
    nav: Navigator,
}

impl Run<'_> {
    fn record(&mut self, node: u32, ident: Ident, stats: &mut LedgerStats) {
        // The ledger was sized to the plan and every node is created once,
        // so this push never reallocates.
        self.ledger.push((node, ident.0, ident.1));
        self.idents[node as usize] = Some(ident);
        stats.capacity_max = stats.capacity_max.max(self.ledger.capacity());
    }

    fn create_all(
        &mut self,
        hook: &mut dyn ExtractHook,
        stats: &mut LedgerStats,
    ) -> Result<u64, LaunchError> {
        let nodes = self.plan.nodes();
        let mut node_entry = vec![u32::MAX; nodes.len()];
        for (entry, &node) in (0_u32..).zip(self.plan.entry_nodes()) {
            node_entry[node as usize] = entry;
        }
        let mut bytes = 0_u64;
        for (index, node) in dfs_order(nodes).into_iter().enumerate() {
            hook.before(HookPoint::BeforeCreate { index });
            let spec = nodes[node as usize];
            let name = self.tree.name(node);
            let parent = self
                .nav
                .goto(&self.tree, spec.parent, &mut self.idents)
                .map_err(|_| changed_failure())?;
            match spec.kind {
                NodeKind::Directory => {
                    fsops::mkdir_at(parent, &name, 0o755).map_err(|error| io_failure(&error))?;
                    let created = fsops::enter_dir(parent, &name).map_err(|_| changed_failure())?;
                    let ident = created.identity();
                    self.record(node, ident, stats);
                    fsops::chmod_dir(&created, 0o755).map_err(|error| io_failure(&error))?;
                }
                NodeKind::File => {
                    let entry_index = node_entry[node as usize];
                    let entry = self
                        .archive
                        .entries()
                        .get(entry_index as usize)
                        .ok_or_else(changed_failure)?;
                    let mode = if entry.unix_mode.is_some_and(|mode| mode & 0o100 != 0) {
                        0o755
                    } else {
                        0o644
                    };
                    let (mut file, ident) = fsops::create_file_at(parent, &name, mode)
                        .map_err(|error| io_failure(&error))?;
                    self.record(node, ident, stats);
                    let mut written = 0_u64;
                    decode_entry(self.archive, entry, &mut |piece| {
                        file.write_all(piece).map_err(|error| io_failure(&error))?;
                        written += piece.len() as u64;
                        hook.written(node, written);
                        Ok(())
                    })?;
                    bytes += written;
                }
            }
        }
        Ok(bytes)
    }

    /// Remove the ledger's entries deepest first; returns (removed, left).
    fn cleanup(&mut self) -> (usize, usize) {
        let (mut removed, mut left) = (0, 0);
        let ledger = std::mem::take(&mut self.ledger);
        for &(node, dev, ino) in ledger.iter().rev() {
            let spec = self.plan.nodes()[node as usize];
            let name = self.tree.name(node);
            let Ok(parent) = self.nav.goto(&self.tree, spec.parent, &mut self.idents) else {
                left += 1;
                continue;
            };
            let same = fsops::stat_at(parent, &name)
                .is_ok_and(|stat| (stat.dev, stat.ino) == (dev, ino));
            if same
                && fsops::remove_at(parent, &name, spec.kind == NodeKind::Directory).is_ok()
            {
                removed += 1;
            } else {
                left += 1;
            }
        }
        (removed, left)
    }
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
    let count = plan.nodes().len();
    stats.plan_len = count;
    let ledger = Vec::with_capacity(count);
    stats.capacity_reserved = ledger.capacity();
    stats.capacity_max = ledger.capacity();
    let staging = create_staging(&target.parent, random)?;
    let navigator_root =
        fsops::enter_dir(&staging.tree, c".").map_err(|_| changed_failure());
    let mut outcome = navigator_root.and_then(|root| {
        let mut run = Run {
            archive,
            plan,
            tree: PlanTree::new(plan, archive.data()),
            idents: vec![None; count],
            ledger,
            nav: Navigator::new(root),
        };
        let created = run.create_all(hook, stats);
        stats.len_at_stop = run.ledger.len();
        let result = created.and_then(|bytes| publish(target, &staging, hook).map(|()| bytes));
        if result.is_err() {
            let (removed, left) = run.cleanup();
            stats.removed = removed;
            stats.left = left;
        }
        result
    });
    if outcome.is_err() {
        let _ = remove_if_same(&staging.dir, c"tree", staging.tree.identity());
        let _ = remove_if_same(&target.parent, &staging.name, staging.dir.identity());
        if stats.left > 0 {
            outcome = outcome.map_err(|error| {
                LaunchError::Archive(format!(
                    "{} Some partially extracted entries could not be removed.",
                    error.message()
                ))
            });
        }
    }
    outcome.map(|bytes| Extracted {
        files: plan.files(),
        directories: plan.directories(),
        bytes,
    })
}

/// Publish the staged tree: identity of the staging name, exclusive rename,
/// identity of the result, removal of the staging directory.
fn publish(
    target: &OutputTarget,
    staging: &Staging,
    hook: &mut dyn ExtractHook,
) -> Result<(), LaunchError> {
    hook.before(HookPoint::BeforePublish);
    let shown_path = shown(&target.path.to_string_lossy());
    let still_ours = fsops::stat_at(&target.parent, &staging.name).is_ok_and(|stat| {
        stat.kind == fsops::EntryType::Directory
            && (stat.dev, stat.ino) == staging.dir.identity()
    });
    if !still_ours {
        return Err(changed_failure());
    }
    if let Err(error) =
        fsops::rename_noreplace(&staging.dir, c"tree", &target.parent, &target.name)
    {
        return Err(LaunchError::Archive(if fsops::is_taken(&error) {
            exists_message(&shown_path)
        } else if fsops::is_invalid(&error) || error.kind() == io::ErrorKind::Unsupported {
            "The output file system does not support an exclusive rename; choose another \
             --output."
                .to_string()
        } else {
            format!("The snapshot could not be published: {error}.")
        }));
    }
    let published = fsops::stat_at(&target.parent, &target.name).is_ok_and(|stat| {
        stat.kind == fsops::EntryType::Directory
            && (stat.dev, stat.ino) == staging.tree.identity()
    });
    let _ = remove_if_same(&target.parent, &staging.name, staging.dir.identity());
    if !published {
        return Err(LaunchError::Archive(format!(
            "The output directory could not be verified after publication: {shown_path}."
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests_stage;
