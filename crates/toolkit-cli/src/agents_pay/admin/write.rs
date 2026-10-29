//! Admin config writes (R-ADM-WRITE, launch R-TOK-MODE).
//!
//! 1. Parents are created with `DirBuilder`; intermediate parents get the
//!    default mode, as Python's `mkdir(parents=True)`, and the final
//!    directory is created at 0700.
//! 2. The final directory is opened `O_NOFOLLOW` and `fchmod`ed 0700.
//! 3. A temporary `.config-<16 hex>.tmp` is created there with
//!    `O_CREAT|O_EXCL|O_NOFOLLOW` at 0600.
//! 4. The content is written, then `sync_all`.
//! 5. `renameat` over the target, then `fchmod 0600` on the result.
//! 6. On any failure the temporary file is removed.

use std::io;
use std::path::Path;

/// The prefix and suffix of the temporary file.
pub const TEMP_PREFIX: &str = ".config-";
/// See [`TEMP_PREFIX`].
pub const TEMP_SUFFIX: &str = ".tmp";

/// Write `content` to `path` atomically at 0600 in a 0700 directory.
///
/// # Errors
/// Any file-system failure; the temporary file is removed first.
pub fn atomic_write_0600(path: &Path, content: &str) -> io::Result<()> {
    atomic_write_0600_with(path, content, |_| Ok(()))
}

/// [`atomic_write_0600`] with a hook that runs on the written temporary
/// file (its path) right before the rename: the T-WRITE failure seam.
///
/// # Errors
/// Any file-system failure or the hook's error; the temporary file is
/// removed first.
#[cfg(unix)]
pub fn atomic_write_0600_with(
    path: &Path,
    content: &str,
    before_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    use rustix::fs::{fchmod, open, openat, unlinkat, AtFlags, Mode, OFlags};
    use std::fmt::Write as _;
    use std::os::unix::fs::DirBuilderExt;

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no file name"))?;
    if let Some(grand) = parent.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::DirBuilder::new().recursive(true).create(grand)?;
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(parent)
        .or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    let dir = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    fchmod(&dir, Mode::from_raw_mode(0o700))?;

    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("no randomness"))?;
    let temp = random
        .iter()
        .fold(String::from(TEMP_PREFIX), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
        + TEMP_SUFFIX;
    let fd = openat(
        &dir,
        temp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    let mut file = std::fs::File::from(fd);
    if let Err(error) = fill_and_rename(
        &mut file,
        &dir,
        &temp,
        name,
        content,
        || before_rename(&parent.join(&temp)),
    ) {
        let _ = unlinkat(&dir, temp.as_str(), AtFlags::empty());
        return Err(error);
    }
    fchmod(&file, Mode::from_raw_mode(0o600))?;
    Ok(())
}

/// Steps 3 to 5 on the open temporary file.
#[cfg(unix)]
fn fill_and_rename(
    file: &mut std::fs::File,
    dir: &std::os::fd::OwnedFd,
    temp: &str,
    name: &std::ffi::OsStr,
    content: &str,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write as _;
    rustix::fs::fchmod(&*file, rustix::fs::Mode::from_raw_mode(0o600))?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    before_rename()?;
    rustix::fs::renameat(dir, temp, dir, name)?;
    Ok(())
}

/// No atomic owner-only write on this platform.
///
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn atomic_write_0600_with(
    _path: &Path,
    _content: &str,
    _before_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owner-only config writes need a Unix platform",
    ))
}
