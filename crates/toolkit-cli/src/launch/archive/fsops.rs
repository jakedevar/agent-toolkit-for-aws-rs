//! The only `rustix` user (design 7.1): descriptor-relative traversal and
//! opens for R-AR-OPEN (#5039), the no-follow state-file open of R-TOK-MODE
//! and the process-group kill of R-AR-GIT's time limit.
//!
//! - [`open_root`] opens the caller's path once (`O_DIRECTORY`, following a
//!   symlink at the root and in the caller-named ancestors, parity with
//!   `os.path.isdir`) and records its `(st_dev, st_ino)`.
//! - [`list_names`] lists a held directory; [`stat_at`] classifies a name
//!   without following it.
//! - [`open_dir_at`] enters a subdirectory with `O_NOFOLLOW | O_DIRECTORY`
//!   and requires the identity [`stat_at`] saw; a symlink swapped in fails
//!   at its own open because no component is ever resolved by path.
//! - [`open_file_at`] opens a regular file with `O_NOFOLLOW | O_NONBLOCK |
//!   O_NOCTTY` (a FIFO returns at once, a symlink leaf fails, a terminal
//!   never becomes the controlling terminal) and requires a regular file
//!   with the walked identity on the handle before any read.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::Child;

/// Most names listed from one directory.
pub const MAX_LISTED_NAMES: usize = 1_000_000;

/// A held directory descriptor and its identity.
#[derive(Debug)]
pub struct DirHandle {
    fd: OwnedFd,
    dev: u64,
    ino: u64,
}

impl DirHandle {
    /// `(st_dev, st_ino)` of the directory.
    #[must_use]
    pub fn identity(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }
}

/// What a name is, without following it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// A FIFO, socket or device.
    Special,
}

/// The `statat(AT_SYMLINK_NOFOLLOW)` facts the walk uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStat {
    /// Type.
    pub kind: EntryType,
    /// `st_dev`.
    pub dev: u64,
    /// `st_ino`.
    pub ino: u64,
    /// `st_size`.
    pub size: u64,
    /// `st_mode`.
    pub mode: u32,
    /// `st_mtime`, seconds.
    pub mtime: i64,
}

/// Why a descriptor-relative open was refused.
#[derive(Debug)]
pub enum OpenError {
    /// The entry is not what the walk saw: another type, another identity,
    /// a symlink or gone.
    Changed,
    /// Another I/O failure.
    Io(io::Error),
}

/// Open the walk root.
///
/// # Errors
/// The open failure (`ENOTDIR` for a non-directory).
pub fn open_root(path: &Path) -> io::Result<DirHandle> {
    let _ = path;
    todo!("L2: open_root")
}

/// The names in `dir` (without `.` and `..`), sorted by bytes.
///
/// # Errors
/// The listing failure, or `InvalidData` past [`MAX_LISTED_NAMES`].
pub fn list_names(dir: &DirHandle) -> io::Result<Vec<CString>> {
    let _ = dir;
    todo!("L2: list_names")
}

/// Classify `name` in `dir` without following it.
///
/// # Errors
/// The `statat` failure.
pub fn stat_at(dir: &DirHandle, name: &CStr) -> io::Result<EntryStat> {
    let _ = (dir, name);
    todo!("L2: stat_at")
}

/// Enter the subdirectory `name`, which must still be `expected`.
///
/// # Errors
/// [`OpenError::Changed`] for a symlink, a vanished entry or another
/// identity.
pub fn open_dir_at(
    dir: &DirHandle,
    name: &CStr,
    expected: &EntryStat,
) -> Result<DirHandle, OpenError> {
    let _ = (dir, name, expected);
    todo!("L2: open_dir_at")
}

/// Open the regular file `name`, which must still be `expected`.
///
/// # Errors
/// [`OpenError::Changed`] for a symlink, a special file, a vanished entry or
/// another identity.
pub fn open_file_at(dir: &DirHandle, name: &CStr, expected: &EntryStat) -> Result<File, OpenError> {
    let _ = (dir, name, expected);
    todo!("L2: open_file_at")
}

/// Open `path` for reading without following a final symlink and without
/// blocking on a FIFO; `None` when it is absent, a symlink or not a regular
/// file. The returned handle has been `fstat`ed as a regular file.
///
/// # Errors
/// Any other open failure.
pub fn open_regular_nofollow(path: &Path) -> io::Result<Option<(File, u64, u32)>> {
    let _ = path;
    todo!("L1: state open")
}

/// Kill the process group of `child` (spawned with `process_group(0)`), so a
/// helper it started cannot keep a pipe open past the time limit.
pub fn kill_process_group(child: &Child) {
    let _ = child;
    todo!("L2: kill group")
}

#[cfg(test)]
mod tests_fsops;
