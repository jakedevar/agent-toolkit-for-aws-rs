//! Atomic publication admission and watchdog decision.
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

const LISTENING: u8 = 0;
const EXCHANGING: u8 = 1;
const COMMITTING: u8 = 2;
const COMMITTED: u8 = 3;
const ABORTED: u8 = 4;

/// The watchdog decision. A process-exit implementation must not return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogAction {
    RecordTimeoutAndExit,
    ExitFailure,
    ExitSuccess,
}

/// Coordinates a single helper's exchange and its watchdog.
pub struct CommitGate(AtomicU8);

impl Default for CommitGate {
    fn default() -> Self {
        Self(AtomicU8::new(LISTENING))
    }
}

impl CommitGate {
    /// Move from listening to exchanging. A watchdog abort wins the race.
    pub fn start_exchange(&self) -> bool {
        self.0
            .compare_exchange(LISTENING, EXCHANGING, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Authorize publication only before cutoff and only if the watchdog did not abort.
    /// A successful admission does not establish that rename has begun.
    pub fn admit(&self, now: Instant, cutoff: Instant) -> bool {
        now < cutoff
            && self
                .0
                .compare_exchange(EXCHANGING, COMMITTING, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }

    /// Mark a successful rename complete.
    pub fn committed(&self) -> bool {
        self.0
            .compare_exchange(COMMITTING, COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Decide what the watchdog does without waiting for the worker thread.
    pub fn watchdog(&self) -> WatchdogAction {
        loop {
            let state = self.0.load(Ordering::SeqCst);
            match state {
                LISTENING | EXCHANGING => {
                    if self
                        .0
                        .compare_exchange(state, ABORTED, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        return WatchdogAction::RecordTimeoutAndExit;
                    }
                }
                COMMITTED => return WatchdogAction::ExitSuccess,
                COMMITTING | ABORTED => return WatchdogAction::ExitFailure,
                _ => unreachable!("private gate state"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cutoff_and_watchdog_close_admission() {
        let cutoff = Instant::now() + Duration::from_secs(1);
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert!(!gate.admit(cutoff, cutoff));
        assert_eq!(gate.watchdog(), WatchdogAction::RecordTimeoutAndExit);
        assert!(!gate.admit(cutoff - Duration::from_millis(1), cutoff));
    }

    #[test]
    fn admitted_publication_does_not_make_watchdog_wait() {
        let cutoff = Instant::now() + Duration::from_secs(1);
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert!(gate.admit(cutoff - Duration::from_millis(1), cutoff));
        assert_eq!(gate.watchdog(), WatchdogAction::ExitFailure);
        assert!(gate.committed());
        assert_eq!(gate.watchdog(), WatchdogAction::ExitSuccess);
    }
}

/// Start the process watchdog. The caller must retain its pending-record lock
/// until process exit. The timeout callback makes one best-effort record write;
/// the watchdog never joins the worker thread.
pub fn start_watchdog(
    gate: std::sync::Arc<CommitGate>,
    deadline: Instant,
    record_timeout: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        match gate.watchdog() {
            WatchdogAction::RecordTimeoutAndExit => {
                record_timeout();
                std::process::exit(1);
            }
            WatchdogAction::ExitFailure => std::process::exit(1),
            WatchdogAction::ExitSuccess => std::process::exit(0),
        }
    });
}

/// Prepare the private session file, sync it, and authorize one rename.
///
/// # Errors
/// Returns an I/O error or a timeout if publication is not authorized.
pub fn publish_session(
    gate: &CommitGate,
    cutoff: Instant,
    dir: &std::path::Path,
    bytes: &[u8],
    random: &dyn crate::launch::clock::Random,
) -> std::io::Result<()> {
    use std::io::Write as _;
    let nonce = crate::launch::clock::random_hex(random, 8)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let temp = dir.join(format!("session.json.{nonce}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if !gate.admit(Instant::now(), cutoff) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "callback publication not admitted",
            ));
        }
        std::fs::rename(&temp, dir.join("session.json"))?;
        if !gate.committed() {
            return Err(std::io::Error::other("callback gate state changed"));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
