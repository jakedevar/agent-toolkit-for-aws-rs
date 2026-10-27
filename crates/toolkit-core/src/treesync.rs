//! Byte-exact tree primitives shared by every synchronization command (#4377).
//!
//! Canonical-to-plugin skill sync, vendored-copy sync and (later) the
//! telemetry bundle sync (#4398) all need the same safe operations, so they
//! live here once:
//!
//! - [`list_tree`] / [`path_state`]: `lstat`-based listing and probing. A
//!   symbolic link is reported as [`Kind::Symlink`] and **never followed or
//!   descended into**; directory names must be UTF-8.
//! - [`files_equal`] / [`trees_match`]: byte-for-byte comparison (no text
//!   decoding, no size limit, symlink-refusing opens via
//!   [`crate::paths::open_regular_file`]).
//! - [`copy_file_atomic`]: write a sibling temporary file, copy the source
//!   permission bits, then `rename` over the destination, so a reader never
//!   sees a half-written file and an interrupted run leaves the old bytes.
//! - [`replace_tree`]: stage a full copy next to the destination and swap it
//!   in, then delete the old copy. The only recursive deletion in the sync
//!   commands is of that old destination directory (a real directory the
//!   caller already proved is a direct child of a known parent); the standard
//!   library's `remove_dir_all` unlinks symbolic links it meets instead of
//!   following them.
//!
//! Threat model: same as [`crate::paths`] — static symlinks are refused, and
//! concurrent mutation of the tree during a run is not defended against.

use std::cmp::Ordering;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use crate::error::ToolkitError;
use crate::paths::open_regular_file;

/// Deepest directory nesting [`list_tree`] will descend into.
pub const MAX_TREE_DEPTH: usize = 64;

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// File type of a directory entry, from `lstat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A real directory.
    Dir,
    /// A symbolic link (never followed).
    Symlink,
    /// A socket, FIFO, device or other special file.
    Other,
}

impl Kind {
    fn of(metadata: &fs::Metadata) -> Self {
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            Self::Symlink
        } else if file_type.is_dir() {
            Self::Dir
        } else if file_type.is_file() {
            Self::File
        } else {
            Self::Other
        }
    }

    /// Short description used in messages.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::File => "regular file",
            Self::Dir => "directory",
            Self::Symlink => "symbolic link",
            Self::Other => "special file",
        }
    }
}

/// One entry of a listed tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Path relative to the listed root, `/`-separated.
    pub rel: String,
    /// What the entry is.
    pub kind: Kind,
}

fn io_failure(action: &str, path: &Path, error: &io::Error) -> ToolkitError {
    ToolkitError::runtime(
        "io_error",
        format!("cannot {action} {}: {error}", path.display()),
    )
}

/// Orders `/`-separated relative paths component by component, the order a
/// sorted list of path objects has (so `a/x` sorts before `a.b`).
#[must_use]
pub fn component_cmp(left: &str, right: &str) -> Ordering {
    left.split('/').cmp(right.split('/'))
}

/// `lstat` of `path`: `None` when it does not exist.
///
/// # Errors
///
/// Returns a runtime error for any other filesystem failure.
pub fn lstat_kind(path: &Path) -> Result<Option<Kind>, ToolkitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(Kind::of(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_failure("inspect", path, &error)),
    }
}

/// Probes `base/relative` one component at a time without following links.
///
/// Returns the kind of the final component, `None` when a component is
/// missing or an intermediate component is not a directory, and
/// `Some(Kind::Symlink)` as soon as **any** component below `base` is a
/// symbolic link (the link itself is never resolved). `base` may itself be a
/// link: it is the caller's explicit choice.
///
/// # Errors
///
/// Returns a runtime error for filesystem failures other than "not found".
pub fn path_state(base: &Path, relative: &str) -> Result<Option<Kind>, ToolkitError> {
    let mut current = base.to_path_buf();
    let mut segments = relative.split('/').filter(|s| !s.is_empty()).peekable();
    let mut last = None;
    while let Some(segment) = segments.next() {
        current.push(segment);
        let Some(kind) = lstat_kind(&current)? else {
            return Ok(None);
        };
        if kind == Kind::Symlink {
            return Ok(Some(Kind::Symlink));
        }
        if segments.peek().is_some() && kind != Kind::Dir {
            return Ok(None);
        }
        last = Some(kind);
    }
    Ok(last)
}

/// Lists every entry below `root` (files, directories, links, special
/// files), sorted with [`component_cmp`]. Symbolic links are listed but not
/// followed; real directories are descended into.
///
/// # Errors
///
/// Returns a runtime error when `root` is not a real directory (a link is
/// refused), a directory cannot be read, a name is not UTF-8 or the tree is
/// deeper than [`MAX_TREE_DEPTH`].
pub fn list_tree(root: &Path) -> Result<Vec<TreeEntry>, ToolkitError> {
    match lstat_kind(root)? {
        Some(Kind::Dir) => {}
        Some(Kind::Symlink) => {
            return Err(ToolkitError::runtime(
                "path_symlink",
                format!("refusing to follow symbolic link: {}", root.display()),
            ));
        }
        Some(other) => {
            return Err(ToolkitError::runtime(
                "not_a_directory",
                format!("not a directory ({}): {}", other.describe(), root.display()),
            ));
        }
        None => {
            return Err(ToolkitError::runtime(
                "path_not_found",
                format!("path does not exist: {}", root.display()),
            ));
        }
    }
    let mut out = Vec::new();
    walk(root, "", 0, &mut out)?;
    out.sort_by(|a, b| component_cmp(&a.rel, &b.rel));
    Ok(out)
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<TreeEntry>,
) -> Result<(), ToolkitError> {
    if depth >= MAX_TREE_DEPTH {
        return Err(ToolkitError::runtime(
            "tree_too_deep",
            format!(
                "directory tree is deeper than {MAX_TREE_DEPTH} levels: {}",
                dir.display()
            ),
        ));
    }
    let entries = fs::read_dir(dir).map_err(|e| io_failure("read directory", dir, &e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_failure("read directory", dir, &e))?;
        let path = entry.path();
        let Ok(name) = entry.file_name().into_string() else {
            return Err(ToolkitError::runtime(
                "path_not_utf8",
                format!("file name is not valid UTF-8: {}", path.display()),
            ));
        };
        let metadata = fs::symlink_metadata(&path).map_err(|e| io_failure("inspect", &path, &e))?;
        let kind = Kind::of(&metadata);
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        out.push(TreeEntry {
            rel: rel.clone(),
            kind,
        });
        if kind == Kind::Dir {
            walk(&path, &rel, depth + 1, out)?;
        }
    }
    Ok(())
}

/// Lists the direct children of the real directory `dir` (not following
/// links), sorted by name.
///
/// # Errors
///
/// Returns a runtime error when `dir` cannot be read or a name is not UTF-8.
pub fn list_children(dir: &Path) -> Result<Vec<TreeEntry>, ToolkitError> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| io_failure("read directory", dir, &e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_failure("read directory", dir, &e))?;
        let path = entry.path();
        let Ok(name) = entry.file_name().into_string() else {
            return Err(ToolkitError::runtime(
                "path_not_utf8",
                format!("file name is not valid UTF-8: {}", path.display()),
            ));
        };
        let metadata = fs::symlink_metadata(&path).map_err(|e| io_failure("inspect", &path, &e))?;
        out.push(TreeEntry {
            rel: name,
            kind: Kind::of(&metadata),
        });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// The first entry that is neither a regular file nor a directory.
#[must_use]
pub fn first_unsafe_entry(entries: &[TreeEntry]) -> Option<&TreeEntry> {
    entries
        .iter()
        .find(|entry| !matches!(entry.kind, Kind::File | Kind::Dir))
}

fn read_full(file: &mut fs::File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Whether two regular files have identical bytes.
///
/// # Errors
///
/// Returns a runtime error when either path is a link, not a regular file or
/// unreadable.
pub fn files_equal(left: &Path, right: &Path) -> Result<bool, ToolkitError> {
    let (mut left_file, left_meta) = open_regular_file(left)?;
    let (mut right_file, right_meta) = open_regular_file(right)?;
    if left_meta.len() != right_meta.len() {
        return Ok(false);
    }
    let mut left_buf = vec![0u8; 64 * 1024];
    let mut right_buf = vec![0u8; 64 * 1024];
    loop {
        let n =
            read_full(&mut left_file, &mut left_buf).map_err(|e| io_failure("read", left, &e))?;
        let m = read_full(&mut right_file, &mut right_buf)
            .map_err(|e| io_failure("read", right, &e))?;
        if n != m || left_buf[..n] != right_buf[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Whether two listed trees hold the same regular files with the same bytes.
///
/// Directories are not compared (an empty directory on one side is not
/// drift). Any link or special file in `dest_entries` makes the trees differ;
/// `source_entries` must be free of them (check with [`first_unsafe_entry`]).
///
/// # Errors
///
/// Returns a runtime error when a file cannot be read.
pub fn trees_match(
    source: &Path,
    source_entries: &[TreeEntry],
    dest: &Path,
    dest_entries: &[TreeEntry],
) -> Result<bool, ToolkitError> {
    if first_unsafe_entry(dest_entries).is_some() {
        return Ok(false);
    }
    let files = |entries: &[TreeEntry]| -> Vec<String> {
        entries
            .iter()
            .filter(|entry| entry.kind == Kind::File)
            .map(|entry| entry.rel.clone())
            .collect()
    };
    let source_files = files(source_entries);
    if source_files != files(dest_entries) {
        return Ok(false);
    }
    for rel in &source_files {
        if !files_equal(&source.join(rel), &dest.join(rel))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed)
    )
}

/// Copies one regular file to `dest` atomically (temporary sibling, copied
/// permission bits, then `rename`). The destination's parent must exist; an
/// existing destination file is replaced, a destination link is replaced as a
/// link (never written through).
///
/// # Errors
///
/// Returns a runtime error when `source` is a link or not a regular file, or
/// on any write failure. A failed copy leaves the old destination untouched
/// and removes its temporary file.
pub fn copy_file_atomic(source: &Path, dest: &Path) -> Result<(), ToolkitError> {
    let (mut input, metadata) = open_regular_file(source)?;
    let (Some(parent), Some(name)) = (dest.parent(), dest.file_name().and_then(|n| n.to_str()))
    else {
        return Err(ToolkitError::runtime(
            "path_invalid",
            format!("invalid destination: {}", dest.display()),
        ));
    };
    let mut attempts = 0;
    let (temp, mut output) = loop {
        let temp = parent.join(format!(".{name}.toolkit-sync-{}", unique_suffix()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempts < 8 => {
                attempts += 1;
            }
            Err(error) => return Err(io_failure("create", &temp, &error)),
        }
    };
    let result = write_temp(&mut input, &mut output, &temp, &metadata, dest);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_temp(
    input: &mut fs::File,
    output: &mut fs::File,
    temp: &Path,
    metadata: &fs::Metadata,
    dest: &Path,
) -> Result<(), ToolkitError> {
    io::copy(input, output).map_err(|e| io_failure("write", temp, &e))?;
    output
        .sync_all()
        .map_err(|e| io_failure("flush", temp, &e))?;
    fs::set_permissions(temp, metadata.permissions())
        .map_err(|e| io_failure("set permissions on", temp, &e))?;
    fs::rename(temp, dest).map_err(|e| io_failure("replace", dest, &e))
}

/// Creates `base/relative_dir` (and parents) one component at a time,
/// refusing to pass through a symbolic link or a non-directory.
///
/// # Errors
///
/// Returns a runtime error naming the offending component.
pub fn ensure_dir_chain(base: &Path, relative_dir: &str) -> Result<(), ToolkitError> {
    let mut current = base.to_path_buf();
    for segment in relative_dir.split('/').filter(|s| !s.is_empty()) {
        current.push(segment);
        match lstat_kind(&current)? {
            Some(Kind::Dir) => {}
            None => fs::create_dir(&current)
                .map_err(|e| io_failure("create directory", &current, &e))?,
            Some(Kind::Symlink) => {
                return Err(ToolkitError::runtime(
                    "path_symlink",
                    format!("refusing to follow symbolic link: {}", current.display()),
                ));
            }
            Some(other) => {
                return Err(ToolkitError::runtime(
                    "not_a_directory",
                    format!(
                        "not a directory ({}): {}",
                        other.describe(),
                        current.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Replaces the real directory `dest` with a copy of `source`.
///
/// The copy is staged in a dot-named sibling of `dest`, then swapped in with
/// two renames (restoring the old directory if the second fails), and the old
/// directory is removed last. Extra files in `dest` disappear; empty source
/// directories and source permission bits are reproduced.
///
/// Both sibling names (staging and moved-aside backup) are reserved by
/// creating them exclusively, retrying with a fresh suffix on a collision, so
/// no existing path is ever renamed onto or removed. Before the old directory
/// is deleted, owner permissions are restored on the directories inside the
/// moved-aside copy only (links are never followed), so a read-only old tree
/// is removed too.
///
/// `source_entries` is the listing of `source` and must be free of links and
/// special files; `dest` must be a real directory, not a link (both checked).
///
/// # Errors
///
/// Returns a runtime error on any refusal or I/O failure; on failure before
/// the swap `dest` is untouched and the staging directory is removed. If only
/// the final removal of the old copy fails, the new tree stays installed and
/// the error (`cleanup_failed`) names the leftover backup path.
pub fn replace_tree(
    source: &Path,
    source_entries: &[TreeEntry],
    dest: &Path,
) -> Result<(), ToolkitError> {
    replace_tree_with(
        source,
        source_entries,
        dest,
        &mut unique_suffix,
        &remove_old_tree,
    )
}

/// [`replace_tree`] with the suffix source and the old-copy removal injected
/// so tests can force name collisions and cleanup failures.
fn replace_tree_with(
    source: &Path,
    source_entries: &[TreeEntry],
    dest: &Path,
    next_suffix: &mut dyn FnMut() -> String,
    remove_backup: &dyn Fn(&Path) -> io::Result<()>,
) -> Result<(), ToolkitError> {
    if let Some(entry) = first_unsafe_entry(source_entries) {
        return Err(ToolkitError::runtime(
            "path_symlink",
            format!(
                "refusing to copy {} ({}): {}",
                entry.rel,
                entry.kind.describe(),
                source.display()
            ),
        ));
    }
    if lstat_kind(dest)? != Some(Kind::Dir) {
        return Err(ToolkitError::runtime(
            "not_a_directory",
            format!("destination is not a real directory: {}", dest.display()),
        ));
    }
    let (Some(parent), Some(name)) = (dest.parent(), dest.file_name().and_then(|n| n.to_str()))
    else {
        return Err(ToolkitError::runtime(
            "path_invalid",
            format!("invalid destination: {}", dest.display()),
        ));
    };
    let stage = reserve_sibling(parent, name, "new", next_suffix)?;
    if let Err(error) = populate_stage(source, source_entries, &stage) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    let backup = match reserve_sibling(parent, name, "old", next_suffix) {
        Ok(backup) => backup,
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
    };
    // `backup` is an empty directory this call just created, so the rename
    // below replaces only that placeholder, never a pre-existing path.
    if let Err(error) = fs::rename(dest, &backup) {
        let _ = fs::remove_dir_all(&stage);
        let _ = fs::remove_dir(&backup);
        return Err(io_failure("move aside", dest, &error));
    }
    if let Err(error) = fs::rename(&stage, dest) {
        let _ = fs::rename(&backup, dest);
        let _ = fs::remove_dir_all(&stage);
        return Err(io_failure("install", dest, &error));
    }
    remove_backup(&backup).map_err(|error| {
        ToolkitError::runtime(
            "cleanup_failed",
            format!(
                "installed the new copy of {} but could not remove the replaced copy ({error}); \
                 remove it manually: {}",
                dest.display(),
                backup.display()
            ),
        )
    })
}

/// Creates the empty directory `parent/.<name>.toolkit-sync-<tag>-<suffix>`
/// exclusively, retrying with a new suffix when the name is taken.
fn reserve_sibling(
    parent: &Path,
    name: &str,
    tag: &str,
    next_suffix: &mut dyn FnMut() -> String,
) -> Result<PathBuf, ToolkitError> {
    // Bounded so a pathological directory cannot loop forever.
    for _ in 0..1000 {
        let path = parent.join(format!(".{name}.toolkit-sync-{tag}-{}", next_suffix()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_failure("create directory", &path, &error)),
        }
    }
    Err(ToolkitError::runtime(
        "path_invalid",
        format!(
            "could not reserve a free sibling name for {}",
            parent.join(name).display()
        ),
    ))
}

/// Removes the moved-aside old tree: first restores owner access on the
/// directories inside it (so a read-only tree can be emptied), then deletes it.
fn remove_old_tree(backup: &Path) -> io::Result<()> {
    make_dirs_owner_writable(backup)?;
    fs::remove_dir_all(backup)
}

/// Adds owner read/write/search permission to `dir` and every directory below
/// it, never following symbolic links and never touching anything outside.
#[cfg(unix)]
fn make_dirs_owner_writable(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700))?;
    }
    for entry in fs::read_dir(dir)? {
        make_dirs_owner_writable(&entry?.path())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_dirs_owner_writable(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn populate_stage(source: &Path, entries: &[TreeEntry], stage: &Path) -> Result<(), ToolkitError> {
    for entry in entries {
        let target = stage.join(&entry.rel);
        match entry.kind {
            Kind::Dir => {
                fs::create_dir(&target).map_err(|e| io_failure("create directory", &target, &e))?;
            }
            _ => copy_file_atomic(&source.join(&entry.rel), &target)?,
        }
    }
    // Directory permission bits last and deepest first, so a read-only source
    // directory cannot block copying into it.
    for entry in entries.iter().rev().filter(|e| e.kind == Kind::Dir) {
        let from = source.join(&entry.rel);
        let metadata = fs::symlink_metadata(&from).map_err(|e| io_failure("inspect", &from, &e))?;
        let to = stage.join(&entry.rel);
        fs::set_permissions(&to, metadata.permissions())
            .map_err(|e| io_failure("set permissions on", &to, &e))?;
    }
    let root = fs::symlink_metadata(source).map_err(|e| io_failure("inspect", source, &e))?;
    fs::set_permissions(stage, root.permissions())
        .map_err(|e| io_failure("set permissions on", stage, &e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, bytes).expect("write");
    }

    #[test]
    fn listing_is_sorted_by_component_and_marks_kinds() {
        let root = TempDir::new("ts-list");
        write(&root, "a.b", b"1");
        write(&root, "a/x", b"2");
        write(&root, "z/deep/file", b"3");
        fs::create_dir(root.join("empty")).expect("mkdir");
        let entries = list_tree(&root).expect("list");
        let rels: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(
            rels,
            ["a", "a/x", "a.b", "empty", "z", "z/deep", "z/deep/file"]
        );
        assert_eq!(entries[1].kind, Kind::File);
        assert_eq!(entries[0].kind, Kind::Dir);
    }

    #[test]
    fn listing_rejects_missing_file_and_depth() {
        let root = TempDir::new("ts-list-bad");
        write(&root, "f", b"x");
        assert_eq!(
            list_tree(&root.join("absent")).expect_err("missing").code(),
            "path_not_found"
        );
        assert_eq!(
            list_tree(&root.join("f")).expect_err("file").code(),
            "not_a_directory"
        );
        let mut deep = root.join("d");
        for _ in 0..=MAX_TREE_DEPTH {
            deep.push("d");
        }
        fs::create_dir_all(&deep).expect("deep");
        assert_eq!(
            list_tree(&root.join("d")).expect_err("deep").code(),
            "tree_too_deep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_listed_but_never_followed() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-outside");
        write(&outside, "secret.txt", b"secret");
        let root = TempDir::new("ts-links");
        write(&root, "real/file", b"x");
        symlink(&*outside, root.join("dirlink")).expect("link dir");
        symlink(outside.join("secret.txt"), root.join("filelink")).expect("link file");
        let entries = list_tree(&root).expect("list");
        let rels: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, ["dirlink", "filelink", "real", "real/file"]);
        assert_eq!(entries[0].kind, Kind::Symlink);
        assert_eq!(entries[1].kind, Kind::Symlink);
        assert_eq!(first_unsafe_entry(&entries).expect("unsafe").rel, "dirlink");
        // The link as the listing root is refused too.
        assert_eq!(
            list_tree(&root.join("dirlink"))
                .expect_err("link root")
                .code(),
            "path_symlink"
        );
        // path_state stops at the first link and reports it.
        assert_eq!(
            path_state(&root, "dirlink/secret.txt").expect("state"),
            Some(Kind::Symlink)
        );
        assert_eq!(
            path_state(&root, "real/file").expect("state"),
            Some(Kind::File)
        );
        assert_eq!(path_state(&root, "real/nope").expect("state"), None);
        assert_eq!(path_state(&root, "real/file/below").expect("state"), None);
    }

    #[test]
    fn files_equal_is_byte_exact_including_binary_and_length() {
        let root = TempDir::new("ts-eq");
        write(&root, "a", &[0, 255, 1, 2]);
        write(&root, "b", &[0, 255, 1, 2]);
        write(&root, "c", &[0, 255, 1, 3]);
        write(&root, "d", &[0, 255, 1]);
        assert!(files_equal(&root.join("a"), &root.join("b")).expect("eq"));
        assert!(!files_equal(&root.join("a"), &root.join("c")).expect("ne"));
        assert!(!files_equal(&root.join("a"), &root.join("d")).expect("len"));
        // Larger than one chunk, differing only in the last byte.
        let mut big = vec![7u8; 200_000];
        write(&root, "big1", &big);
        big[199_999] = 8;
        write(&root, "big2", &big);
        assert!(!files_equal(&root.join("big1"), &root.join("big2")).expect("big"));
        assert_eq!(
            files_equal(&root.join("a"), &root.join("absent"))
                .expect_err("missing")
                .code(),
            "path_not_found"
        );
    }

    #[test]
    fn trees_match_compares_file_sets_and_bytes_but_not_empty_dirs() {
        let a = TempDir::new("ts-match-a");
        let b = TempDir::new("ts-match-b");
        write(&a, "x/one", b"1");
        write(&a, "two", b"2");
        write(&b, "x/one", b"1");
        write(&b, "two", b"2");
        let la = list_tree(&a).expect("list");
        let lb = list_tree(&b).expect("list");
        assert!(trees_match(&a, &la, &b, &lb).expect("match"));

        fs::create_dir(b.join("empty")).expect("mkdir");
        let lb = list_tree(&b).expect("list");
        assert!(trees_match(&a, &la, &b, &lb).expect("empty dir is not drift"));

        write(&b, "extra", b"e");
        let lb = list_tree(&b).expect("list");
        assert!(!trees_match(&a, &la, &b, &lb).expect("extra file"));
        fs::remove_file(b.join("extra")).expect("rm");
        fs::remove_file(b.join("two")).expect("rm");
        let lb = list_tree(&b).expect("list");
        assert!(!trees_match(&a, &la, &b, &lb).expect("missing file"));
        write(&b, "two", b"X");
        let lb = list_tree(&b).expect("list");
        assert!(!trees_match(&a, &la, &b, &lb).expect("changed bytes"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_destination_is_drift_even_when_bytes_match() {
        use std::os::unix::fs::symlink;
        let a = TempDir::new("ts-dlink-a");
        let b = TempDir::new("ts-dlink-b");
        write(&a, "f", b"1");
        write(&b, "real", b"1");
        symlink(b.join("real"), b.join("f")).expect("link");
        let la = list_tree(&a).expect("list");
        let lb = list_tree(&b).expect("list");
        assert!(!trees_match(&a, &la, &b, &lb).expect("link is drift"));
    }

    #[test]
    fn copy_file_atomic_replaces_bytes_and_keeps_permissions_without_leftovers() {
        let root = TempDir::new("ts-copy");
        write(&root, "src", b"new bytes");
        write(&root, "dst", b"old");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("src"), fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        copy_file_atomic(&root.join("src"), &root.join("dst")).expect("copy");
        assert_eq!(fs::read(root.join("dst")).expect("read"), b"new bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join("dst"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        let names: Vec<String> = fs::read_dir(&*root)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
            .collect();
        assert_eq!(names.len(), 2, "no temporary files left: {names:?}");
    }

    #[test]
    fn copy_file_atomic_failure_leaves_destination_and_no_temp() {
        let root = TempDir::new("ts-copy-fail");
        write(&root, "dst", b"keep");
        let error = copy_file_atomic(&root.join("absent"), &root.join("dst")).expect_err("missing");
        assert_eq!(error.code(), "path_not_found");
        assert_eq!(fs::read(root.join("dst")).expect("read"), b"keep");
        // A directory in the way of the rename fails and removes its temp file.
        write(&root, "src", b"x");
        fs::create_dir(root.join("blocked")).expect("mkdir");
        write(&root, "blocked/inner", b"i");
        assert!(copy_file_atomic(&root.join("src"), &root.join("blocked")).is_err());
        let leftovers: Vec<String> = fs::read_dir(&*root)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn copy_file_atomic_replaces_a_destination_link_instead_of_writing_through() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-copy-out");
        write(&outside, "victim", b"untouched");
        let root = TempDir::new("ts-copy-link");
        write(&root, "src", b"payload");
        symlink(outside.join("victim"), root.join("dst")).expect("link");
        copy_file_atomic(&root.join("src"), &root.join("dst")).expect("copy");
        assert_eq!(
            fs::read(outside.join("victim")).expect("read"),
            b"untouched"
        );
        assert_eq!(
            lstat_kind(&root.join("dst")).expect("kind"),
            Some(Kind::File)
        );
        assert_eq!(fs::read(root.join("dst")).expect("read"), b"payload");
    }

    #[cfg(unix)]
    #[test]
    fn copy_file_atomic_refuses_a_source_link() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-copy-src-out");
        write(&outside, "secret", b"secret");
        let root = TempDir::new("ts-copy-src");
        symlink(outside.join("secret"), root.join("src")).expect("link");
        let error = copy_file_atomic(&root.join("src"), &root.join("dst")).expect_err("link");
        assert_eq!(error.code(), "path_symlink");
        assert!(!root.join("dst").exists());
    }

    #[test]
    fn ensure_dir_chain_creates_nested_directories_and_is_idempotent() {
        let root = TempDir::new("ts-chain");
        ensure_dir_chain(&root, "a/b/c").expect("create");
        ensure_dir_chain(&root, "a/b/c").expect("again");
        ensure_dir_chain(&root, "").expect("empty is a no-op");
        assert!(root.join("a/b/c").is_dir());
        write(&root, "file", b"x");
        assert_eq!(
            ensure_dir_chain(&root, "file/under")
                .expect_err("file")
                .code(),
            "not_a_directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_chain_refuses_links() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-chain-out");
        let root = TempDir::new("ts-chain-link");
        symlink(&*outside, root.join("escape")).expect("link");
        let error = ensure_dir_chain(&root, "escape/new").expect_err("link");
        assert_eq!(error.code(), "path_symlink");
        assert!(!outside.join("new").exists());
    }

    #[test]
    fn replace_tree_makes_dest_identical_removing_extras_and_keeping_empty_dirs() {
        let root = TempDir::new("ts-replace");
        write(&root, "src/keep", b"k");
        write(&root, "src/sub/inner", b"i");
        fs::create_dir(root.join("src/empty")).expect("mkdir");
        write(&root, "dest/stale", b"old");
        write(&root, "dest/keep", b"different");
        let entries = list_tree(&root.join("src")).expect("list");
        replace_tree(&root.join("src"), &entries, &root.join("dest")).expect("replace");
        let after = list_tree(&root.join("dest")).expect("list");
        let rels: Vec<&str> = after.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, ["empty", "keep", "sub", "sub/inner"]);
        assert_eq!(fs::read(root.join("dest/keep")).expect("read"), b"k");
        let top: Vec<String> = fs::read_dir(&*root)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
            .collect();
        assert_eq!(top.len(), 2, "no staging or backup left: {top:?}");
    }

    #[cfg(unix)]
    #[test]
    fn replace_tree_refuses_source_links_and_leaves_dest_untouched() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-replace-out");
        write(&outside, "secret", b"secret");
        let root = TempDir::new("ts-replace-link");
        write(&root, "src/ok", b"ok");
        symlink(outside.join("secret"), root.join("src/leak")).expect("link");
        write(&root, "dest/original", b"original");
        let entries = list_tree(&root.join("src")).expect("list");
        let error =
            replace_tree(&root.join("src"), &entries, &root.join("dest")).expect_err("link");
        assert_eq!(error.code(), "path_symlink");
        assert_eq!(
            fs::read(root.join("dest/original")).expect("read"),
            b"original"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replace_tree_removes_dest_links_without_touching_their_targets() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-replace-dest-out");
        write(&outside, "precious/data", b"precious");
        let root = TempDir::new("ts-replace-dest");
        write(&root, "src/a", b"a");
        write(&root, "dest/a", b"a");
        symlink(outside.join("precious"), root.join("dest/dirlink")).expect("link");
        let entries = list_tree(&root.join("src")).expect("list");
        replace_tree(&root.join("src"), &entries, &root.join("dest")).expect("replace");
        assert_eq!(
            fs::read(outside.join("precious/data")).expect("read"),
            b"precious"
        );
        assert!(!root.join("dest/dirlink").exists());
    }

    #[cfg(unix)]
    #[test]
    fn replace_tree_refuses_a_link_as_destination() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ts-replace-root-out");
        write(&outside, "data", b"data");
        let root = TempDir::new("ts-replace-root");
        write(&root, "src/a", b"a");
        symlink(&*outside, root.join("dest")).expect("link");
        let entries = list_tree(&root.join("src")).expect("list");
        let error =
            replace_tree(&root.join("src"), &entries, &root.join("dest")).expect_err("link");
        assert_eq!(error.code(), "not_a_directory");
        assert_eq!(fs::read(outside.join("data")).expect("read"), b"data");
    }

    #[test]
    fn replace_tree_never_clobbers_a_pre_existing_backup_name() {
        let root = TempDir::new("ts-replace-backup");
        write(&root, "src/new", b"new");
        write(&root, "dest/old", b"old");
        // The siblings a naive fixed-suffix swap would pick.
        fs::create_dir(root.join(".dest.toolkit-sync-old-a")).expect("empty sibling");
        write(&root, ".dest.toolkit-sync-new-a/keep", b"keep");
        let entries = list_tree(&root.join("src")).expect("list");
        // stage: a (taken) then b; backup: a (taken) then c.
        let mut names = ["a", "b", "a", "c"].into_iter().map(String::from);
        replace_tree_with(
            &root.join("src"),
            &entries,
            &root.join("dest"),
            &mut || names.next().expect("suffix"),
            &remove_old_tree,
        )
        .expect("replace");
        assert_eq!(fs::read(root.join("dest/new")).expect("read"), b"new");
        assert!(!root.join("dest/old").exists());
        assert!(
            root.join(".dest.toolkit-sync-old-a").is_dir(),
            "pre-existing empty sibling must survive"
        );
        assert_eq!(
            fs::read(root.join(".dest.toolkit-sync-new-a/keep")).expect("read"),
            b"keep"
        );
        let leftovers: Vec<String> = fs::read_dir(&*root)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
            .filter(|n| n.contains("toolkit-sync-") && !n.ends_with("-a"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "own staging/backup left: {leftovers:?}"
        );
    }

    #[test]
    fn replace_tree_refuses_when_no_free_sibling_name_exists() {
        let root = TempDir::new("ts-replace-exhaust");
        write(&root, "src/new", b"new");
        write(&root, "dest/old", b"old");
        fs::create_dir(root.join(".dest.toolkit-sync-new-x")).expect("taken");
        let entries = list_tree(&root.join("src")).expect("list");
        let error = replace_tree_with(
            &root.join("src"),
            &entries,
            &root.join("dest"),
            &mut || "x".to_string(),
            &remove_old_tree,
        )
        .expect_err("exhausted");
        assert_eq!(error.code(), "path_invalid");
        assert_eq!(fs::read(root.join("dest/old")).expect("read"), b"old");
        assert!(root.join(".dest.toolkit-sync-new-x").is_dir());
    }

    /// True when this process can delete files from a read-only directory,
    /// i.e. it is privileged and permission-based tests prove nothing.
    #[cfg(unix)]
    fn permissions_are_not_enforced() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = TempDir::new("ts-perm-probe");
        write(&probe, "d/f", b"x");
        fs::set_permissions(probe.join("d"), fs::Permissions::from_mode(0o555)).expect("chmod");
        let removable = fs::remove_file(probe.join("d/f")).is_ok();
        let _ = fs::set_permissions(probe.join("d"), fs::Permissions::from_mode(0o755));
        removable
    }

    #[cfg(unix)]
    #[test]
    fn replace_tree_replaces_a_read_only_old_tree_and_leaves_no_backup() {
        use std::os::unix::fs::PermissionsExt;
        if permissions_are_not_enforced() {
            eprintln!("skipped: running with privileges that ignore directory modes");
            return;
        }
        let root = TempDir::new("ts-replace-readonly");
        write(&root, "src/new", b"new");
        write(&root, "dest/old", b"old");
        write(&root, "dest/sub/deeper/old2", b"old2");
        for dir in ["dest/sub/deeper", "dest/sub", "dest"] {
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o555)).expect("chmod");
        }
        let entries = list_tree(&root.join("src")).expect("list");
        replace_tree(&root.join("src"), &entries, &root.join("dest")).expect("replace");
        assert_eq!(fs::read(root.join("dest/new")).expect("read"), b"new");
        assert!(!root.join("dest/old").exists());
        let top: Vec<String> = fs::read_dir(&*root)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
            .collect();
        assert_eq!(top.len(), 2, "no staging or backup left: {top:?}");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_cleanup_does_not_follow_links_out_of_the_old_tree() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let outside = TempDir::new("ts-replace-ro-out");
        write(&outside, "data/file", b"data");
        fs::set_permissions(outside.join("data"), fs::Permissions::from_mode(0o500))
            .expect("chmod");
        let root = TempDir::new("ts-replace-ro-link");
        write(&root, "src/new", b"new");
        write(&root, "dest/old", b"old");
        symlink(outside.join("data"), root.join("dest/link")).expect("link");
        let entries = list_tree(&root.join("src")).expect("list");
        replace_tree(&root.join("src"), &entries, &root.join("dest")).expect("replace");
        let mode = fs::metadata(outside.join("data"))
            .expect("meta")
            .permissions()
            .mode();
        fs::set_permissions(outside.join("data"), fs::Permissions::from_mode(0o755))
            .expect("restore");
        assert_eq!(mode & 0o777, 0o500, "link target permissions untouched");
        assert_eq!(fs::read(outside.join("data/file")).expect("read"), b"data");
    }

    #[test]
    fn replace_tree_keeps_the_new_tree_and_names_the_backup_when_cleanup_fails() {
        let root = TempDir::new("ts-replace-cleanup-fail");
        write(&root, "src/new", b"new");
        write(&root, "dest/old", b"old");
        let entries = list_tree(&root.join("src")).expect("list");
        let error = replace_tree_with(
            &root.join("src"),
            &entries,
            &root.join("dest"),
            &mut unique_suffix,
            &|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "forced")),
        )
        .expect_err("cleanup failure");
        assert_eq!(error.code(), "cleanup_failed");
        assert_eq!(fs::read(root.join("dest/new")).expect("read"), b"new");
        assert!(!root.join("dest/old").exists());
        let backups: Vec<PathBuf> = fs::read_dir(&*root)
            .expect("dir")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.to_string_lossy().contains("toolkit-sync-old-"))
            .collect();
        assert_eq!(backups.len(), 1, "one leftover backup: {backups:?}");
        assert_eq!(fs::read(backups[0].join("old")).expect("read"), b"old");
        let message = error.message();
        assert!(
            message.contains(&backups[0].display().to_string()),
            "error names the leftover path: {message}"
        );
    }
}
