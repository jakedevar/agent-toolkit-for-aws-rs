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

    /// Claim an exchange failure only if the watchdog has not aborted it.
    pub fn fail_exchange(&self) -> bool {
        self.0
            .compare_exchange(EXCHANGING, ABORTED, Ordering::SeqCst, Ordering::SeqCst)
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
        watchdog_step(&gate, record_timeout, |code| std::process::exit(code));
    });
}

pub(super) fn watchdog_step(
    gate: &CommitGate,
    record_timeout: impl FnOnce(),
    exit: impl FnOnce(i32),
) {
    let code = match gate.watchdog() {
        WatchdogAction::RecordTimeoutAndExit => {
            record_timeout();
            1
        }
        WatchdogAction::ExitFailure => 1,
        WatchdogAction::ExitSuccess => 0,
    };
    exit(code);
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
    publish_with(
        gate,
        cutoff,
        dir,
        bytes,
        random,
        Instant::now,
        || {},
        |from, to| std::fs::rename(from, to),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_with(
    gate: &CommitGate,
    cutoff: Instant,
    dir: &std::path::Path,
    bytes: &[u8],
    random: &dyn crate::launch::clock::Random,
    now: impl FnOnce() -> Instant,
    admitted: impl FnOnce(),
    rename: impl FnOnce(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    publish_observed(
        gate,
        cutoff,
        dir,
        bytes,
        random,
        now,
        admitted,
        rename,
        |_| {},
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_observed(
    gate: &CommitGate,
    cutoff: Instant,
    dir: &std::path::Path,
    bytes: &[u8],
    random: &dyn crate::launch::clock::Random,
    now: impl FnOnce() -> Instant,
    admitted: impl FnOnce(),
    rename: impl FnOnce(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
    mut observed: impl FnMut(&'static str),
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
        observed("write");
        file.sync_all()?;
        observed("sync");
        drop(file);
        observed("admit");
        if !gate.admit(now(), cutoff) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "callback publication not admitted",
            ));
        }
        admitted();
        rename(&temp, &dir.join("session.json"))?;
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn watchdog_attempts_timeout_record_once_before_failure_exit() {
        let gate = CommitGate::default();
        let calls = std::cell::RefCell::new(Vec::new());
        watchdog_step(
            &gate,
            || calls.borrow_mut().push("record"),
            |code| {
                assert_eq!(code, 1);
                calls.borrow_mut().push("exit:1");
            },
        );
        assert_eq!(*calls.borrow(), vec!["record", "exit:1"]);
    }

    #[test]
    fn publication_admission_precedes_rename_and_watchdog_never_waits() {
        use crate::launch::{clock::SeededRandom, tests_support::Scratch};
        let scratch = Scratch::new("gate-hook");
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let cutoff = Instant::now() + Duration::from_secs(1);
        let calls = std::cell::RefCell::new(Vec::new());
        publish_with(
            &gate,
            cutoff,
            scratch.path(),
            b"session",
            &SeededRandom::new(1),
            || {
                let temp = std::fs::read_dir(scratch.path())
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                assert_eq!(std::fs::read(temp).unwrap(), b"session");
                calls.borrow_mut().push("prepared");
                cutoff.checked_sub(Duration::from_millis(1)).unwrap()
            },
            || {
                calls.borrow_mut().push("admitted");
                assert_eq!(gate.watchdog(), WatchdogAction::ExitFailure);
                calls.borrow_mut().push("watchdog:1");
            },
            |from, to| {
                calls.borrow_mut().push("rename");
                std::fs::rename(from, to)
            },
        )
        .unwrap();
        assert_eq!(
            *calls.borrow(),
            vec!["prepared", "admitted", "watchdog:1", "rename"]
        );
        assert_eq!(
            std::fs::read(scratch.join("session.json")).unwrap(),
            b"session"
        );
    }

    #[test]
    fn publication_at_cutoff_removes_temp_without_rename() {
        use crate::launch::{clock::SeededRandom, tests_support::Scratch};
        let scratch = Scratch::new("gate-cutoff");
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let cutoff = Instant::now();
        let result = publish_with(
            &gate,
            cutoff,
            scratch.path(),
            b"session",
            &SeededRandom::new(1),
            || cutoff,
            || panic!("admission after cutoff"),
            |_, _| panic!("rename after cutoff"),
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn session_write_and_sync_complete_before_admission() {
        use crate::launch::{clock::SeededRandom, tests_support::Scratch};
        let scratch = Scratch::new("prepare-order");
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let cutoff = Instant::now() + Duration::from_secs(1);
        let events = std::cell::RefCell::new(Vec::new());
        publish_observed(
            &gate,
            cutoff,
            scratch.path(),
            b"session",
            &SeededRandom::new(1),
            || cutoff.checked_sub(Duration::from_millis(1)).unwrap(),
            || events.borrow_mut().push("admitted"),
            |from, to| {
                events.borrow_mut().push("rename");
                std::fs::rename(from, to)
            },
            |event| events.borrow_mut().push(event),
        )
        .unwrap();
        assert_eq!(
            *events.borrow(),
            ["write", "sync", "admit", "admitted", "rename"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_admitted_rename_preserves_timeout_decision() {
        use crate::launch::{
            callback::{
                pending::{acquire_if_free, Custody},
                wait_decision, PendingStatus, WaitDecision,
            },
            clock::SeededRandom,
            tests_support::Scratch,
        };
        let scratch = Scratch::new("failed-rename");
        let custody = Custody::acquire(scratch.path(), 81).unwrap();
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let cutoff = Instant::now();
        let result = publish_with(
            &gate,
            cutoff,
            scratch.path(),
            b"session",
            &SeededRandom::new(1),
            || cutoff.checked_sub(Duration::from_millis(1)).unwrap(),
            || {
                for now in [100, 102] {
                    assert_eq!(
                        wait_decision(PendingStatus::Exchanging, true, false, now, 100),
                        WaitDecision::Poll
                    );
                }
                watchdog_step(
                    &gate,
                    || panic!("no record while committing"),
                    |code| assert_eq!(code, 1),
                );
                assert_eq!(
                    wait_decision(PendingStatus::Exchanging, true, false, 105, 100),
                    WaitDecision::HelperBlocked
                );
            },
            |_, _| Err(std::io::Error::other("rename failed")),
        );
        assert!(result.is_err());
        assert!(!scratch.join("session.json").exists());
        custody.release_settled();
        assert!(acquire_if_free(scratch.path(), 81).is_some());
        assert_eq!(
            wait_decision(PendingStatus::Exchanging, false, false, 105, 100),
            WaitDecision::TimedOut
        );
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_exits_after_real_record_write_failure() {
        use crate::launch::{
            callback::{pending::Custody, PendingStatus},
            clock::SeededRandom,
            tests_support::Scratch,
        };
        let scratch = Scratch::new("watchdog-write-error");
        let custody = Custody::acquire(scratch.path(), 81).unwrap();
        std::fs::create_dir(scratch.join("pending/81.json")).unwrap();
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let writes = std::cell::Cell::new(0);
        let exits = std::cell::Cell::new(0);
        watchdog_step(
            &gate,
            || {
                writes.set(writes.get() + 1);
                assert!(custody
                    .write(
                        1234,
                        40,
                        100,
                        PendingStatus::TimedOut,
                        None,
                        &SeededRandom::new(1)
                    )
                    .is_err());
            },
            |code| {
                assert_eq!(code, 1);
                exits.set(exits.get() + 1);
            },
        );
        assert_eq!(writes.get(), 1);
        assert_eq!(exits.get(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn stalled_rename_keeps_custody_and_watchdog_returns_without_waiting() {
        use crate::launch::{
            callback::{
                pending::{acquire_if_free, Custody},
                wait_decision, PendingStatus, WaitDecision,
            },
            clock::SeededRandom,
            tests_support::Scratch,
        };
        let scratch = Scratch::new("stalled-rename");
        let custody = Custody::acquire(scratch.path(), 81).unwrap();
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let cutoff = Instant::now() + Duration::from_secs(1);
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let gate_ref = &gate;
            let path = scratch.path();
            let worker = scope.spawn(move || {
                publish_with(
                    gate_ref,
                    cutoff,
                    path,
                    b"session",
                    &SeededRandom::new(1),
                    || cutoff.checked_sub(Duration::from_millis(1)).unwrap(),
                    || {},
                    |from, to| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        std::fs::rename(from, to)
                    },
                )
            });
            entered_rx.recv().unwrap();
            watchdog_step(
                &gate,
                || panic!("committing must not write record"),
                |code| assert_eq!(code, 1),
            );
            assert!(acquire_if_free(scratch.path(), 81).is_none());
            assert_eq!(
                wait_decision(PendingStatus::Exchanging, true, false, 104, 100),
                WaitDecision::Poll
            );
            assert_eq!(
                wait_decision(PendingStatus::Exchanging, true, false, 105, 100),
                WaitDecision::HelperBlocked
            );
            assert_eq!(
                wait_decision(PendingStatus::Exchanging, true, false, 3700, 100),
                WaitDecision::HelperBlocked
            );
            release_tx.send(()).unwrap();
            worker.join().unwrap().unwrap();
        });
        custody.release_settled();
        assert!(acquire_if_free(scratch.path(), 81).is_some());
        assert_eq!(
            wait_decision(PendingStatus::Exchanging, false, true, 105, 100),
            WaitDecision::Authenticated
        );
        assert_eq!(
            std::fs::read(scratch.join("session.json")).unwrap(),
            b"session"
        );
    }

    #[test]
    fn cutoff_and_watchdog_close_admission() {
        let cutoff = Instant::now() + Duration::from_secs(1);
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert!(!gate.admit(cutoff, cutoff));
        assert_eq!(gate.watchdog(), WatchdogAction::RecordTimeoutAndExit);
        assert!(!gate.admit(
            cutoff.checked_sub(Duration::from_millis(1)).unwrap(),
            cutoff
        ));
    }

    #[test]
    fn aborted_exchange_never_invokes_rename_or_admission_hook() {
        use crate::launch::{clock::SeededRandom, tests_support::Scratch};
        let scratch = Scratch::new("gate-abort");
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert_eq!(gate.watchdog(), WatchdogAction::RecordTimeoutAndExit);
        let cutoff = Instant::now() + Duration::from_secs(5);
        let result = publish_with(
            &gate,
            cutoff,
            scratch.path(),
            b"session",
            &SeededRandom::new(1),
            Instant::now,
            || panic!("aborted gate admitted"),
            |_, _| panic!("aborted gate renamed"),
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn watchdog_does_not_write_record_in_committing_or_committed_state() {
        let cutoff = Instant::now() + Duration::from_secs(5);
        for committed in [false, true] {
            let gate = CommitGate::default();
            assert!(gate.start_exchange());
            assert!(gate.admit(Instant::now(), cutoff));
            if committed {
                assert!(gate.committed());
            }
            watchdog_step(
                &gate,
                || panic!("unexpected record write"),
                |code| assert_eq!(code, i32::from(!committed)),
            );
        }
    }

    #[test]
    fn failed_watchdog_record_attempt_still_exits_once() {
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let calls = std::cell::RefCell::new(Vec::new());
        watchdog_step(
            &gate,
            || {
                calls.borrow_mut().push("write");
                let failure: std::io::Result<()> = Err(std::io::Error::other("injected"));
                let _ = failure;
            },
            |code| {
                assert_eq!(code, 1);
                calls.borrow_mut().push("exit");
            },
        );
        assert_eq!(*calls.borrow(), vec!["write", "exit"]);
    }

    #[cfg(unix)]
    #[test]
    fn blocked_watchdog_record_keeps_lock_and_attempts_only_once() {
        use crate::launch::{
            callback::{
                pending::{acquire_if_free, Custody},
                wait_decision, PendingStatus, WaitDecision,
            },
            tests_support::Scratch,
        };
        let scratch = Scratch::new("blocked-record");
        let custody = Custody::acquire(scratch.path(), 82).unwrap();
        let gate = CommitGate::default();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let gate_ref = &gate;
            let attempts_ref = &attempts;
            let worker = scope.spawn(move || {
                watchdog_step(
                    gate_ref,
                    || {
                        attempts_ref.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    },
                    |code| assert_eq!(code, 1),
                );
            });
            entered_rx.recv().unwrap();
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            assert!(acquire_if_free(scratch.path(), 82).is_none());
            assert_eq!(
                wait_decision(PendingStatus::Listening, true, false, 105, 100),
                WaitDecision::HelperBlocked
            );
            release_tx.send(()).unwrap();
            worker.join().unwrap();
        });
        custody.release_settled();
        assert!(acquire_if_free(scratch.path(), 82).is_some());
        assert_eq!(
            wait_decision(PendingStatus::Listening, false, false, 105, 100),
            WaitDecision::TimedOut
        );
    }

    #[test]
    fn admitted_publication_does_not_make_watchdog_wait() {
        let cutoff = Instant::now() + Duration::from_secs(1);
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert!(gate.admit(
            cutoff.checked_sub(Duration::from_millis(1)).unwrap(),
            cutoff
        ));
        assert_eq!(gate.watchdog(), WatchdogAction::ExitFailure);
        // Production exits immediately on this action; a returning test stub must
        // not simulate subsequent publication.
    }
}
