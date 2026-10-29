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

use std::ffi::{CStr, CString, OsStr};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::fsops::{self, DirHandle, EntryStat, EntryType};
use super::git::GitRunner;
pub use super::meta::MetaRules;
use super::meta::{self, Protected};
use super::nav::{Ident, Navigator, Tree, ROOT};
use super::vcs::is_vcs_component;
use super::zip_read::check_name;
use super::GIB;
use crate::launch::clock::{random_hex, Random};
use crate::launch::text::shown;
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

/// One snapshot entry: a file or a directory.
#[derive(Debug)]
struct Node {
    parent: u32,
    depth: u16,
    name: CString,
    /// The source `statat` facts (type, identity, size, mode).
    source: EntryStat,
}

/// The snapshot as phase 1 walked it: nodes with every directory's
/// children contiguous and directories visited depth first, so walking the
/// ids in order moves the navigators a linear number of steps.
#[derive(Debug)]
pub struct SnapshotTree {
    nodes: Vec<Node>,
    /// Files.
    pub files: usize,
    /// Directories.
    pub directories: usize,
    /// Bytes in total.
    pub bytes: u64,
}

impl Tree for SnapshotTree {
    fn parent(&self, node: u32) -> u32 {
        self.nodes[node as usize].parent
    }

    fn depth(&self, node: u32) -> usize {
        usize::from(self.nodes[node as usize].depth)
    }

    fn name(&self, node: u32) -> CString {
        self.nodes[node as usize].name.clone()
    }
}

impl SnapshotTree {
    /// The `/`-separated path of `node` (for messages).
    fn path(&self, node: u32) -> String {
        let mut parts = Vec::new();
        let mut at = node;
        while at != ROOT {
            let entry = &self.nodes[at as usize];
            parts.push(String::from_utf8_lossy(entry.name.as_bytes()).into_owned());
            at = entry.parent;
        }
        parts.reverse();
        parts.join("/")
    }
}

fn refusal(text: String) -> LaunchError {
    LaunchError::Archive(text)
}

fn walk_failure() -> LaunchError {
    refusal("The snapshot changed while it was read and was not applied.".to_string())
}

/// Walk the snapshot rooted at `snapshot` under `limits` (phase 1's source
/// side); nothing is written.
///
/// # Errors
/// The first refusal.
pub fn walk_snapshot(snapshot: &Path, limits: &ApplyLimits) -> Result<SnapshotTree, LaunchError> {
    let root = fsops::open_root(snapshot).map_err(|_| {
        refusal(format!(
            "The snapshot directory could not be opened: {}.",
            shown(&snapshot.to_string_lossy())
        ))
    })?;
    walk_from(root, limits)
}

#[allow(clippy::too_many_lines)]
fn walk_from(root: DirHandle, limits: &ApplyLimits) -> Result<SnapshotTree, LaunchError> {
    let mut tree = SnapshotTree {
        nodes: Vec::new(),
        files: 0,
        directories: 0,
        bytes: 0,
    };
    let mut idents: Vec<Option<Ident>> = Vec::new();
    let mut nav = Navigator::new(root);
    let mut pending = vec![ROOT];
    while let Some(dir) = pending.pop() {
        let held = nav.goto(&tree, dir, &mut idents).map_err(|_| walk_failure())?;
        let listed = fsops::list_names(held).map_err(|_| walk_failure())?;
        let prefix = if dir == ROOT {
            String::new()
        } else {
            format!("{}/", tree.path(dir))
        };
        if meta::names_are_git_shaped(listed.iter().map(|name| name.as_bytes())) {
            return Err(refusal(meta::shaped_snapshot_message(&shown(
                prefix.trim_end_matches('/'),
            ))));
        }
        let depth = if dir == ROOT {
            1
        } else {
            tree.nodes[dir as usize].depth + 1
        };
        let first_child = tree.nodes.len();
        for name in listed {
            let Ok(text) = std::str::from_utf8(name.as_bytes()) else {
                return Err(refusal(
                    "The snapshot contains a name that is not valid UTF-8 and was not applied."
                        .to_string(),
                ));
            };
            let path = format!("{prefix}{text}");
            check_name(&path).map_err(|_| {
                refusal(format!(
                    "The snapshot contains an unsafe path and was not applied: {}.",
                    shown(&path)
                ))
            })?;
            if is_vcs_component(text) {
                return Err(refusal(format!(
                    "The snapshot contains version-control metadata and was not applied: {}.",
                    shown(&path)
                )));
            }
            let stat = fsops::stat_at(held, &name).map_err(|_| walk_failure())?;
            match stat.kind {
                EntryType::Directory => tree.directories += 1,
                EntryType::Regular => {
                    tree.files += 1;
                    tree.bytes = tree.bytes.saturating_add(stat.size);
                }
                EntryType::Symlink | EntryType::Special => {
                    return Err(refusal(format!(
                        "The snapshot contains a symlink or special file and was not applied: \
                         {}.",
                        shown(&path)
                    )))
                }
            }
            if tree.files > limits.max_files {
                return Err(refusal(format!(
                    "The snapshot has more than {} files and was not applied.",
                    limits.max_files
                )));
            }
            if tree.nodes.len() == limits.max_nodes {
                return Err(refusal(format!(
                    "The snapshot would create more than {} files and directories and was not \
                     applied.",
                    limits.max_nodes
                )));
            }
            if tree.bytes > limits.max_bytes {
                return Err(refusal(format!(
                    "The snapshot is larger than {} GiB and was not applied.",
                    limits.max_bytes / GIB
                )));
            }
            let parent = dir;
            tree.nodes.push(Node {
                parent,
                depth,
                name,
                source: stat,
            });
            idents.push((stat.kind == EntryType::Directory).then_some((stat.dev, stat.ino)));
        }
        // Visit the new directories in name order (the stack pops the last).
        for child in (first_child..tree.nodes.len()).rev() {
            if tree.nodes[child].source.kind == EntryType::Directory {
                pending.push(u32::try_from(child).map_err(|_| walk_failure())?);
            }
        }
    }
    Ok(tree)
}

/// What phase 1 found on the target side for each node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Absent: created in phase 2.
    New,
    /// An existing directory with this identity.
    Directory(Ident),
    /// An existing regular file, replaced in phase 2.
    File,
}

/// Phase 1, target side: classify every planned path without writing.
fn plan_target(
    tree: &SnapshotTree,
    into: &DirHandle,
    into_path: &Path,
    protected: &Protected,
    rules: MetaRules,
) -> Result<Vec<Target>, LaunchError> {
    let mut targets = Vec::with_capacity(tree.nodes.len());
    let mut idents: Vec<Option<Ident>> = vec![None; tree.nodes.len()];
    let root = fsops::enter_dir(into, c".").map_err(|_| walk_failure())?;
    let mut nav = Navigator::new(root);
    for (id, node) in (0_u32..).zip(&tree.nodes) {
        let parent_new = node.parent != ROOT && targets[node.parent as usize] == Target::New;
        if parent_new {
            targets.push(Target::New);
            continue;
        }
        let shown_path = || shown(&into_path.join(tree.path(id)).to_string_lossy());
        let held = nav.goto(tree, node.parent, &mut idents).map_err(|_| {
            refusal(format!(
                "The target changed while the snapshot was planned and was not applied: {}.",
                shown_path()
            ))
        })?;
        let existing = match fsops::stat_at(held, &node.name) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                targets.push(Target::New);
                continue;
            }
            Err(_) => return Err(walk_failure()),
        };
        let conflict = |what: &str| {
            Err(refusal(format!(
                "The target has {what} at {} and the snapshot was not applied.",
                shown_path()
            )))
        };
        let wanted_dir = node.source.kind == EntryType::Directory;
        match (existing.kind, wanted_dir) {
            (EntryType::Symlink, _) => return conflict("a symlink"),
            (EntryType::Special, _) => return conflict("a special file"),
            (EntryType::Regular, true) => return conflict("a file where the snapshot has a directory"),
            (EntryType::Directory, false) => {
                return conflict("a directory where the snapshot has a file")
            }
            (EntryType::Regular, false) => targets.push(Target::File),
            (EntryType::Directory, true) => {
                let entered = fsops::open_dir_at(held, &node.name, &existing)
                    .map_err(|_| walk_failure())?;
                if !protected.allows(&entered, rules) {
                    return Err(refusal(meta::metadata_message(&shown_path())));
                }
                idents[id as usize] = Some(entered.identity());
                targets.push(Target::Directory(entered.identity()));
            }
        }
    }
    Ok(targets)
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
    // Phase 0: R-AR-META, before the snapshot is read.
    meta::check_environment(options.env)?;
    let into_handle = fsops::open_root(into).map_err(|_| {
        refusal(format!(
            "The target directory could not be opened: {}.",
            shown(&into.to_string_lossy())
        ))
    })?;
    let protected = meta::discover(options.git, into)?;
    meta::check_root(&into_handle, into, &protected, options.rules)?;
    // Phase 1: plan; nothing is written.
    let tree = walk_snapshot(snapshot, &options.limits)?;
    let targets = plan_target(&tree, &into_handle, into, &protected, options.rules)?;
    options.hook.between_phases();
    // Phase 2: write.
    let source = fsops::open_root(snapshot).map_err(|_| walk_failure())?;
    write_phase(&tree, &targets, source, &into_handle, options)
}

fn write_phase(
    tree: &SnapshotTree,
    targets: &[Target],
    source: DirHandle,
    into_handle: &DirHandle,
    options: &mut ApplyOptions<'_>,
) -> Result<Applied, LaunchError> {
    let _ = (tree, targets, source, into_handle, options);
    todo!("R-AR-APPLY phase 2")
}

#[cfg(test)]
mod tests_apply;
#[cfg(test)]
mod tests_meta;
