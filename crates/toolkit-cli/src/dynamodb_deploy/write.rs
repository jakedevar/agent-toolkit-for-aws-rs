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
///
/// Publication replaces the final directory entry without opening it. A link
/// swapped in after the initial check is replaced, never followed. Ancestor
/// directories must remain trusted and stable during publication.
pub(crate) fn write_private(path: &Path, contents: &str, mode: u32) -> Result<(), ToolkitError> {
    write_private_with(path, mode, |file| file.write_all(contents.as_bytes()))
}

/// Inject only the staging write; validation, sync, cleanup and publication
/// are shared so failure tests exercise the production publication path.
fn write_private_with(
    path: &Path,
    mode: u32,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> Result<(), ToolkitError> {
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
    let written = write(&mut file)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamodb::test_support::TempDir;

    #[test]
    fn partial_staging_failure_preserves_existing_content_and_cleans_the_temp() {
        for existing in [false, true] {
            let root = TempDir::new("atomic-partial-failure");
            let path = root.join("report.md");
            if existing {
                fs::write(&path, "complete old report").unwrap();
            }
            let error = write_private_with(&path, PRIVATE_FILE, |file| {
                file.write_all(b"partial new report")?;
                if existing {
                    assert_eq!(fs::read_to_string(&path).unwrap(), "complete old report");
                } else {
                    assert!(!path.exists());
                }
                Err(io::Error::other("injected staging failure"))
            })
            .expect_err("partial staging must not publish");
            assert!(error.to_string().contains("injected staging failure"));
            if existing {
                assert_eq!(fs::read_to_string(&path).unwrap(), "complete old report");
            } else {
                assert!(!path.exists());
            }
            assert_eq!(fs::read_dir(&*root).unwrap().count(), usize::from(existing));
        }
    }

    #[test]
    fn readers_see_complete_old_content_until_complete_new_content_is_published() {
        use std::io::Read;

        let root = TempDir::new("atomic-visibility");
        let path = root.join("report.md");
        let old = "complete old report".repeat(512);
        let new = "complete new report".repeat(1_024);
        fs::write(&path, &old).unwrap();
        let mut old_reader = fs::File::open(&path).unwrap();
        write_private_with(&path, PRIVATE_FILE, |file| {
            for chunk in new.as_bytes().chunks(256) {
                file.write_all(chunk)?;
                // Inspect each partial staging state deterministically.
                assert_eq!(fs::read_to_string(&path).unwrap(), old);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), new);
        let mut held = String::new();
        old_reader.read_to_string(&mut held).unwrap();
        assert_eq!(held, old);
        assert_eq!(fs::read_dir(&*root).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_destination_link_swapped_in_during_staging_is_never_followed() {
        let root = TempDir::new("atomic-link-swap");
        let path = root.join("report.md");
        let target = root.join("target");
        fs::write(&path, "old report").unwrap();
        fs::write(&target, "KEEP").unwrap();
        write_private_with(&path, PRIVATE_FILE, |file| {
            file.write_all(b"new report")?;
            fs::remove_file(&path)?;
            std::os::unix::fs::symlink(&target, &path)
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "KEEP");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new report");
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_dir(&*root).unwrap().count(), 2);
    }
}
