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
