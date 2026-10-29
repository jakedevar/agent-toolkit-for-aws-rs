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
        use std::os::unix::fs::PermissionsExt;
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
        reason: Option<&str>,
        random: &dyn Random,
    ) -> io::Result<()> {
        self.write_with_error(port, deadline, cutoff, status, reason, None, random)
    }

    /// Write a denied record including the validated OAuth error code.
    #[allow(clippy::too_many_arguments)]
    pub fn write_with_error(
        &self,
        port: u16,
        deadline: i64,
        cutoff: i64,
        status: PendingStatus,
        reason: Option<&str>,
        oauth_error: Option<&str>,
        random: &dyn Random,
    ) -> io::Result<()> {
        let status = match status {
            PendingStatus::Listening => "listening",
            PendingStatus::Exchanging => "exchanging",
            PendingStatus::Succeeded => "succeeded",
            PendingStatus::Failed => "failed",
            PendingStatus::TimedOut => "timed_out",
        };
        let mut value = serde_json::json!({"pid":self.pid,"port":port,"deadline":deadline,"cutoff":cutoff,"status":status,"reason":reason});
        if let Some(error) = oauth_error {
            value["oauth_error"] = serde_json::json!(error);
        }
        state::write_private_file(
            &self.dir,
            &format!("{}.json", self.pid),
            value.to_string().as_bytes(),
            random,
        )
    }
}

/// Record fields needed for the auth-wait decision.
#[derive(Debug, PartialEq, Eq)]
pub struct Record {
    pub status: PendingStatus,
    pub cutoff: i64,
    pub reason: Option<String>,
}

/// Read the pending record without following a final symlink.
/// Malformed or missing records return `None`.
pub fn read_record(state_dir: &Path, pid: u32) -> Option<Record> {
    use toolkit_core::{parse_json, Json};
    let bytes =
        state::read_private_file(&state_dir.join("pending"), &format!("{pid}.json")).ok()??;
    let Json::Object(members) = parse_json(std::str::from_utf8(&bytes).ok()?).ok()? else {
        return None;
    };
    let keys = ["pid", "port", "deadline", "cutoff", "status", "reason"];
    if !(members.len() == keys.len()
        || (members.len() == keys.len() + 1
            && members
                .iter()
                .filter(|(name, _)| name == "oauth_error")
                .count()
                == 1))
        || !keys
            .iter()
            .all(|key| members.iter().filter(|(name, _)| name == key).count() == 1)
    {
        return None;
    }
    let get = |key: &str| {
        members
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    if !matches!(get("pid"), Some(Json::Int(value)) if *value == i64::from(pid)) {
        return None;
    }
    if !matches!(get("port"), Some(Json::Int(value)) if (1..=65535).contains(value)) {
        return None;
    }
    if !matches!(get("deadline"), Some(Json::Int(_))) {
        return None;
    }
    let cutoff = match get("cutoff")? {
        Json::Int(value) => *value,
        _ => return None,
    };
    let status = match get("status")? {
        Json::String(value) => match value.as_str() {
            "listening" => PendingStatus::Listening,
            "exchanging" => PendingStatus::Exchanging,
            "succeeded" => PendingStatus::Succeeded,
            "failed" => PendingStatus::Failed,
            "timed_out" => PendingStatus::TimedOut,
            _ => return None,
        },
        _ => return None,
    };
    let reason = match get("reason")? {
        Json::Null => None,
        Json::String(value)
            if matches!(
                value.as_str(),
                "authorization_denied"
                    | "invalid_code"
                    | "exchange_failed"
                    | "transport_unavailable"
            ) =>
        {
            Some(value.clone())
        }
        _ => return None,
    };
    if let Some(value) = get("oauth_error") {
        if !matches!(value, Json::String(error) if super::valid_oauth_error(error)) {
            return None;
        }
    }
    Some(Record {
        status,
        cutoff,
        reason,
    })
}

/// Acquire a free helper lock while inspecting its final session state.
/// Returns `None` for a held or unverifiable lock, never treating an error as free.
#[cfg(unix)]
pub fn acquire_if_free(state_dir: &Path, pid: u32) -> Option<File> {
    use rustix::fs::{open, Mode, OFlags};
    let fd = open(
        state_dir.join("pending").join(format!("{pid}.lock")),
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .ok()?;
    let file = File::from(fd);
    if !file.metadata().ok()?.is_file() || file.try_lock().is_err() {
        return None;
    }
    Some(file)
}

/// One auth-wait observation; `Poll` and `HelperBlocked` are non-final.
#[derive(Debug, PartialEq, Eq)]
pub enum Observation {
    Missing,
    Decision(super::WaitDecision),
}

/// Observe a record while holding the free lock, if available. An unreadable
/// lock is treated as held so it cannot produce a false final failure.
#[cfg(unix)]
#[must_use]
pub fn observe(state_dir: &Path, pid: u32, now: i64) -> Observation {
    let Some(record) = read_record(state_dir, pid) else {
        return Observation::Missing;
    };
    let lock = acquire_if_free(state_dir, pid);
    let valid = state::load_session(state_dir)
        .is_some_and(|session| session.token_expires_at > now.saturating_add(300));
    Observation::Decision(super::wait_decision(
        record.status,
        lock.is_none(),
        valid,
        now,
        record.cutoff,
    ))
}

/// Evaluate a wait and remove a final record only while the lock is held here.
#[cfg(unix)]
pub fn finish_if_final(state_dir: &Path, pid: u32, now: i64) -> (Observation, Option<String>) {
    let Some(mut record) = read_record(state_dir, pid) else {
        return (Observation::Missing, None);
    };
    let lock = acquire_if_free(state_dir, pid);
    if lock.is_some() {
        let Some(final_record) = read_record(state_dir, pid) else {
            return (Observation::Missing, None);
        };
        record = final_record;
    }
    let valid = state::load_session(state_dir)
        .is_some_and(|session| session.token_expires_at > now.saturating_add(300));
    let decision = super::wait_decision(record.status, lock.is_none(), valid, now, record.cutoff);
    if lock.is_some()
        && decision != super::WaitDecision::Poll
        && decision != super::WaitDecision::HelperBlocked
    {
        let _ = state::remove_state_file(&state_dir.join("pending"), &format!("{pid}.json"));
    }
    (Observation::Decision(decision), record.reason)
}

/// Remove stale records with free locks and old temporary session files.
/// Unreadable, held or unrecognized entries are left alone.
#[cfg(unix)]
pub fn cleanup_stale(state_dir: &Path, now: i64) {
    let pending = state_dir.join("pending");
    if let Ok(entries) = std::fs::read_dir(&pending) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(pid) = name
                .strip_suffix(".json")
                .and_then(|v| v.parse::<u32>().ok())
            else {
                continue;
            };
            if name != format!("{pid}.json") {
                continue;
            }
            let Some(record) = read_record(state_dir, pid) else {
                continue;
            };
            if now <= record.cutoff.saturating_add(3600) {
                continue;
            }
            if let Some(_lock) = acquire_if_free(state_dir, pid) {
                if read_record(state_dir, pid)
                    .is_some_and(|current| now > current.cutoff.saturating_add(3600))
                {
                    let _ = state::remove_state_file(&pending, name);
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(state_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(nonce) = name
                .strip_prefix("session.json.")
                .and_then(|s| s.strip_suffix(".tmp"))
            else {
                continue;
            };
            if nonce.len() != 16
                || !nonce
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Ok(modified) = meta.modified().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) else {
                continue;
            };
            let Ok(modified) = i64::try_from(modified.as_secs()) else {
                continue;
            };
            if now > modified.saturating_add(3600) {
                let _ = state::remove_state_file(state_dir, name);
            }
        }
    }
}
