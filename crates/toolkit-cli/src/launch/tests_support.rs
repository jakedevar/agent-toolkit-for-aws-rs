//! Test-only helpers shared by the launch unit tests: private scratch
//! directories under the system temporary directory, removed on drop.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, empty directory removed (with its contents) on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A new scratch directory whose name includes `label`.
    pub fn new(label: &str) -> Self {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).expect("csprng");
        let path = std::env::temp_dir().join(format!(
            "toolkit-launch-test-{label}-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            u64::from_le_bytes(nonce)
        ));
        fs::create_dir(&path).expect("create scratch dir");
        Self(path)
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// `relative` under the directory.
    pub fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// Write `contents` to `relative`, creating parent directories.
    pub fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, contents).expect("write fixture");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Make every directory writable again so cleanup succeeds after a
        // test that removed permissions.
        #[cfg(unix)]
        restore_permissions(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn restore_permissions(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            restore_permissions(&entry.path());
        }
    }
}

/// The production Git runner with the test discovery ceiling at the
/// temporary directory (see `GitRunner::ceiling`).
#[cfg(unix)]
pub fn test_git() -> super::archive::git::GitRunner {
    super::archive::git::GitRunner {
        ceiling: Some(std::env::temp_dir()),
        ..super::archive::git::GitRunner::system()
    }
}
