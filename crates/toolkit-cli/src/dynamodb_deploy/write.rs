//! Private, symlink-refusing, atomic output files.
//!
//! The deployment plan holds the account id and the generated IAM policy text;
//! the teardown script is executable. Both are written to a unique temporary
//! file in the destination directory (created exclusively, owner-only before
//! any byte is written) and renamed over the destination, so a crash leaves
//! only an orphan temporary file and never a truncated output. A destination
//! that is a symbolic link or not a regular file is refused, never followed.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use toolkit_core::ToolkitError;

use crate::safe_text::safe_path;

/// Owner read/write.
pub(crate) const PRIVATE_FILE: u32 = 0o600;
/// Owner read/write/execute (the teardown script).
pub(crate) const PRIVATE_EXECUTABLE: u32 = 0o700;

fn failure(path: &Path, reason: &str) -> ToolkitError {
    ToolkitError::runtime(
        "dynamodb_deploy_write",
        format!(
            "cannot write {}: {}",
            safe_path(&path.display().to_string()),
            crate::safe_text::safe_text(reason)
        ),
    )
    .with_exit_code(2)
}

fn unique_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    format!("{:016x}", hasher.finish())
}

fn create_temp(dir: &Path, name: &str, mode: u32) -> io::Result<(fs::File, PathBuf)> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..100 {
        let path = dir.join(format!(".{name}.{}.tmp", unique_token()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Writes `contents` to `path` with `mode` (an owner-only mode), atomically.
///
/// # Errors
///
/// A runtime error (exit status 2) when the destination is a symbolic link or
/// not a regular file, or when the temporary file cannot be written or
/// renamed. A failed write removes its own temporary file and leaves an
/// existing destination untouched.
pub(crate) fn write_private(path: &Path, contents: &str, mode: u32) -> Result<(), ToolkitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(failure(path, "symbolic links are not followed"));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(failure(path, "not a regular file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failure(path, &error.to_string())),
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failure(path, "the file name is not valid UTF-8"))?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let (mut file, temp) =
        create_temp(dir, name, mode).map_err(|error| failure(path, &error.to_string()))?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| restrict(&file, mode))
        .and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| fs::rename(&temp, path));
    if let Err(error) = result {
        // Only this invocation's own temporary file is removed.
        let _ = fs::remove_file(&temp);
        return Err(failure(path, &error.to_string()));
    }
    Ok(())
}

#[cfg(unix)]
fn restrict(file: &fs::File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restrict(_file: &fs::File, _mode: u32) -> io::Result<()> {
    Ok(())
}
