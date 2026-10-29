//! Private pending records and lifetime lock custody.
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::PendingStatus;
use crate::launch::{clock::Random, state};

/// An exclusive lock owned for the lifetime of a callback helper.
pub struct Custody {
    _lock: File,
    dir: PathBuf,
    pid: u32,
}

impl Custody {
    /// Create a private pending directory and acquire the helper's lock.
    ///
    /// # Errors
    /// Refuses a non-directory or symlink and returns lock and filesystem errors.
    #[cfg(unix)]
    pub fn acquire(state_dir: &Path, pid: u32) -> io::Result<Self> {
        use rustix::fs::{open, Mode, OFlags};
        state::prepare_dir(state_dir)?;
        let dir = state_dir.join("pending");
        state::prepare_dir(&dir)?;
        let fd = open(
            dir.join(format!("{pid}.lock")),
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_bits_truncate(0o600),
        )?;
        let lock = File::from(fd);
        if !lock.metadata()?.is_file() {
            return Err(io::Error::other("invalid pending lock"));
        }
        lock.try_lock().map_err(io::Error::from)?;
        use std::os::unix::fs::PermissionsExt;
        lock.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            _lock: lock,
            dir,
            pid,
        })
    }

    /// Write one record atomically, without secrets.
    ///
    /// # Errors
    /// Returns any private file or random source failure.
    pub fn write(
        &self,
        port: u16,
        deadline: i64,
        cutoff: i64,
        status: PendingStatus,
        reason: Option<&'static str>,
        random: &dyn Random,
    ) -> io::Result<()> {
        let status = match status {
            PendingStatus::Listening => "listening",
            PendingStatus::Exchanging => "exchanging",
            PendingStatus::Succeeded => "succeeded",
            PendingStatus::Failed => "failed",
            PendingStatus::TimedOut => "timed_out",
        };
        let value = serde_json::json!({"pid":self.pid,"port":port,"deadline":deadline,"cutoff":cutoff,"status":status,"reason":reason});
        state::write_private_file(
            &self.dir,
            &format!("{}.json", self.pid),
            value.to_string().as_bytes(),
            random,
        )
    }
}
