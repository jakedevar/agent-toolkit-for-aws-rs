//! Atomic no-replace publication for the future render command.
//!
//! Directory checks are static: as in toolkit-core, concurrent replacement of
//! ancestor directories is outside the threat model.
#![allow(dead_code)] // S2a is deliberately not routed until S2b.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) trait PublishHooks {
    fn after_temp_written(&mut self, _temp: &Path, _dest: &Path) {}
    fn before_commit(&mut self, _temp: &Path, _dest: &Path) {}
    fn after_commit_before_unlink(&mut self, _temp: &Path, _dest: &Path) {}
    fn fault(&mut self, _stage: &str) -> Option<io::Error> {
        None
    }
}
pub(crate) struct NoHooks;
impl PublishHooks for NoHooks {}

#[derive(Debug)]
pub(crate) struct PublishError {
    pub code: &'static str,
    pub detail: String,
}
#[derive(Debug)]
pub(crate) struct Published {
    pub leftover_temp: Option<PathBuf>,
}

fn error(code: &'static str, detail: impl ToString) -> PublishError {
    PublishError {
        code,
        detail: detail.to_string(),
    }
}
pub(crate) fn unsupported_platform() -> PublishError {
    error(
        "publish_unsupported_platform",
        "atomic publication is unsupported on this platform",
    )
}

/// Check every existing directory component without following symlinks.
fn check_directory(dir: &Path) -> Result<(), PublishError> {
    let mut path = PathBuf::new();
    for part in dir.components() {
        match part {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                path.push(part.as_os_str())
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(error(
                    "publish_invalid_path",
                    "parent directory component is not allowed",
                ))
            }
        }
        let metadata = fs::symlink_metadata(&path).map_err(|e| error("publish_io", e))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(error(
                "publish_invalid_path",
                "directory component is a symlink or not a directory",
            ));
        }
    }
    Ok(())
}

fn cleanup(temp: &Path, original: PublishError) -> PublishError {
    match fs::remove_file(temp) {
        Ok(()) => original,
        Err(e) => error(
            "publish_cleanup_failed",
            format!("{}; cannot remove {}: {e}", original.detail, temp.display()),
        ),
    }
}

#[cfg(unix)]
pub(crate) fn publish_new(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    hooks: &mut dyn PublishHooks,
) -> Result<Published, PublishError> {
    if name.is_empty()
        || !matches!(
            (
                Path::new(name).components().next(),
                Path::new(name).components().count()
            ),
            (Some(Component::Normal(_)), 1)
        )
    {
        return Err(error(
            "publish_invalid_path",
            "output name must be a single filename",
        ));
    }
    check_directory(dir)?;
    let dest = dir.join(name);
    if let Some(e) = hooks.fault("temp_create") {
        return Err(error("publish_io", e));
    }
    let (temp, mut file) = loop {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(
            ".{name}.toolkit-render-{}-{counter}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(error("publish_io", e)),
        }
    };
    let written = hooks
        .fault("write")
        .map_or_else(|| file.write_all(bytes), Err)
        .and_then(|()| hooks.fault("sync").map_or_else(|| file.sync_all(), Err));
    drop(file);
    if let Err(e) = written {
        return Err(cleanup(&temp, error("publish_io", e)));
    }
    hooks.after_temp_written(&temp, &dest);
    hooks.before_commit(&temp, &dest);
    let linked = hooks
        .fault("link")
        .map_or_else(|| fs::hard_link(&temp, &dest), Err);
    if let Err(e) = linked {
        let code = match e.kind() {
            io::ErrorKind::AlreadyExists => "destination_exists",
            io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported => {
                "publish_no_atomic_noreplace"
            }
            _ => "publish_io",
        };
        return Err(cleanup(&temp, error(code, e)));
    }
    hooks.after_commit_before_unlink(&temp, &dest);
    let removed = hooks
        .fault("unlink")
        .map_or_else(|| fs::remove_file(&temp), Err);
    Ok(Published {
        leftover_temp: removed.err().map(|_| temp),
    })
}

#[cfg(not(unix))]
pub(crate) fn publish_new(
    _dir: &Path,
    _name: &str,
    _bytes: &[u8],
    _hooks: &mut dyn PublishHooks,
) -> Result<Published, PublishError> {
    Err(unsupported_platform())
}

#[cfg(test)]
#[path = "place_tests.rs"]
mod tests;
