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

use rustix::fs::{fstat, openat, statat, AtFlags, Dir, FileType, Mode, OFlags, RawMode, Stat, CWD};
use rustix::io::Errno;
use rustix::process::{Pid, Signal};

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

/// `(st_dev, st_ino)`, `st_size`, `st_mode` and `st_mtime` of a `Stat`,
/// whatever their platform widths.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unnecessary_cast,
    clippy::useless_conversion
)]
fn facts(stat: &Stat) -> EntryStat {
    let mode = stat.st_mode as u32;
    let kind = match FileType::from_raw_mode(stat.st_mode as RawMode) {
        FileType::RegularFile => EntryType::Regular,
        FileType::Directory => EntryType::Directory,
        FileType::Symlink => EntryType::Symlink,
        _ => EntryType::Special,
    };
    EntryStat {
        kind,
        dev: stat.st_dev as u64,
        ino: stat.st_ino as u64,
        size: u64::try_from(stat.st_size).unwrap_or(0),
        mode,
        mtime: stat.st_mtime as i64,
    }
}

/// Whether an open failure means "the entry is not what the walk saw".
fn is_changed(errno: Errno) -> bool {
    errno == Errno::LOOP || errno == Errno::NOTDIR || errno == Errno::NOENT || errno == Errno::NXIO
}

fn open_error(errno: Errno) -> OpenError {
    if is_changed(errno) {
        OpenError::Changed
    } else {
        OpenError::Io(io::Error::from(errno))
    }
}

/// Open the walk root.
///
/// # Errors
/// The open failure (`ENOTDIR` for a non-directory).
pub fn open_root(path: &Path) -> io::Result<DirHandle> {
    let fd = openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let stat = facts(&fstat(&fd)?);
    if stat.kind != EntryType::Directory {
        return Err(io::Error::from(Errno::NOTDIR));
    }
    Ok(DirHandle {
        fd,
        dev: stat.dev,
        ino: stat.ino,
    })
}

/// The names in `dir` (without `.` and `..`), sorted by bytes.
///
/// # Errors
/// The listing failure, or `InvalidData` past [`MAX_LISTED_NAMES`].
pub fn list_names(dir: &DirHandle) -> io::Result<Vec<CString>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(&dir.fd)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        if names.len() == MAX_LISTED_NAMES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a directory has too many entries",
            ));
        }
        names.push(name.to_owned());
    }
    names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}

/// Classify `name` in `dir` without following it.
///
/// # Errors
/// The `statat` failure.
pub fn stat_at(dir: &DirHandle, name: &CStr) -> io::Result<EntryStat> {
    Ok(facts(&statat(&dir.fd, name, AtFlags::SYMLINK_NOFOLLOW)?))
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
    let fd = openat(
        &dir.fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(open_error)?;
    let stat = facts(&fstat(&fd).map_err(open_error)?);
    if stat.kind != EntryType::Directory || (stat.dev, stat.ino) != (expected.dev, expected.ino) {
        return Err(OpenError::Changed);
    }
    Ok(DirHandle {
        fd,
        dev: stat.dev,
        ino: stat.ino,
    })
}

/// Open the regular file `name`, which must still be `expected`.
///
/// # Errors
/// [`OpenError::Changed`] for a symlink, a special file, a vanished entry or
/// another identity.
pub fn open_file_at(dir: &DirHandle, name: &CStr, expected: &EntryStat) -> Result<File, OpenError> {
    let fd = openat(
        &dir.fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(open_error)?;
    let stat = facts(&fstat(&fd).map_err(open_error)?);
    if stat.kind != EntryType::Regular || (stat.dev, stat.ino) != (expected.dev, expected.ino) {
        return Err(OpenError::Changed);
    }
    Ok(File::from(fd))
}

/// Open `path` for reading without following a final symlink and without
/// blocking on a FIFO; `None` when it is absent, a symlink or not a regular
/// file. The returned handle has been `fstat`ed as a regular file.
///
/// # Errors
/// Any other open failure.
pub fn open_regular_nofollow(path: &Path) -> io::Result<Option<(File, u64, u32)>> {
    let fd = match openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(errno) if errno == Errno::NOENT || errno == Errno::LOOP || errno == Errno::NXIO => {
            return Ok(None)
        }
        Err(errno) => return Err(io::Error::from(errno)),
    };
    let stat = facts(&fstat(&fd)?);
    if stat.kind != EntryType::Regular {
        return Ok(None);
    }
    Ok(Some((File::from(fd), stat.size, stat.mode)))
}

/// Kill the process group of `child` (spawned with `process_group(0)`), so a
/// helper it started cannot keep a pipe open past the time limit.
pub fn kill_process_group(child: &Child) {
    let _ = rustix::process::kill_process_group(Pid::from_child(child), Signal::KILL);
}

#[cfg(test)]
mod tests_fsops;
