//! Bounded execution: input cap, wall-clock deadline and panic containment.

use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::decision::{Decision, DenyReason};
use crate::policy::{evaluate, MAX_INPUT_BYTES};

/// Evaluation budget. The registered hook timeout is five seconds; this leaves
/// time to print the deny before the host gives up.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(3);

/// Runs `work` on its own thread and waits at most `deadline` for the verdict.
///
/// A slow, panicking or unspawnable worker is a deny. The worker thread is
/// detached; the caller is expected to end the process after printing.
#[must_use]
pub fn decide_with_deadline<F>(deadline: Duration, work: F) -> Decision
where
    F: FnOnce() -> Decision + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("secret-safety-eval".to_string())
        .spawn(move || {
            let decision = work();
            let _ = sender.send(decision);
        });
    if spawned.is_err() {
        return Decision::Deny(DenyReason::InternalFailure);
    }
    match receiver.recv_timeout(deadline) {
        Ok(decision) => decision,
        Err(mpsc::RecvTimeoutError::Timeout) => Decision::Deny(DenyReason::DeadlineExceeded),
        Err(mpsc::RecvTimeoutError::Disconnected) => Decision::Deny(DenyReason::InternalFailure),
    }
}

/// Reads at most [`MAX_INPUT_BYTES`] plus one byte and evaluates them, all
/// inside the deadline (so a stdin that never closes is a deny too).
#[must_use]
pub fn decide_from_reader<R>(reader: R, deadline: Duration) -> Decision
where
    R: Read + Send + 'static,
{
    decide_with_deadline(deadline, move || evaluate_reader(reader))
}

fn evaluate_reader<R: Read>(reader: R) -> Decision {
    let limit = u64::try_from(MAX_INPUT_BYTES).map_or(u64::MAX, |cap| cap.saturating_add(1));
    let mut buffer = Vec::new();
    if reader.take(limit).read_to_end(&mut buffer).is_err() {
        return Decision::Deny(DenyReason::MalformedInput);
    }
    evaluate(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::time::Instant;

    struct Blocked;

    impl Read for Blocked {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_secs(30));
            Ok(0)
        }
    }

    struct Failing;

    impl Read for Failing {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic read failure"))
        }
    }

    #[test]
    fn fast_work_returns_its_verdict() {
        assert_eq!(
            decide_with_deadline(Duration::from_secs(10), || Decision::Allow),
            Decision::Allow
        );
    }

    #[test]
    fn slow_work_is_denied_at_the_deadline() {
        let started = Instant::now();
        let decision = decide_with_deadline(Duration::from_millis(100), || {
            thread::sleep(Duration::from_secs(30));
            Decision::Allow
        });
        assert_eq!(decision, Decision::Deny(DenyReason::DeadlineExceeded));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn panicking_work_is_denied() {
        let decision = decide_with_deadline(Duration::from_secs(10), || {
            panic!("synthetic evaluator failure")
        });
        assert_eq!(decision, Decision::Deny(DenyReason::InternalFailure));
    }

    #[test]
    fn a_stdin_that_never_closes_is_denied() {
        let decision = decide_from_reader(Blocked, Duration::from_millis(100));
        assert_eq!(decision, Decision::Deny(DenyReason::DeadlineExceeded));
    }

    #[test]
    fn an_unreadable_stdin_is_denied() {
        let decision = decide_from_reader(Failing, Duration::from_secs(10));
        assert_eq!(decision, Decision::Deny(DenyReason::MalformedInput));
    }

    #[test]
    fn reader_input_is_evaluated() {
        let allow = br#"{"tool_name": "Bash", "tool_input": {"command": "ls"}}"#.to_vec();
        assert_eq!(
            decide_from_reader(io::Cursor::new(allow), Duration::from_secs(10)),
            Decision::Allow
        );
        let deny =
            br#"{"tool_name": "Bash", "tool_input": {"command": "aws secretsmanager get-secret-value"}}"#
                .to_vec();
        assert_eq!(
            decide_from_reader(io::Cursor::new(deny), Duration::from_secs(10)),
            Decision::Deny(DenyReason::SecretFetch)
        );
    }

    #[test]
    fn oversized_reader_input_is_denied() {
        let big = vec![b' '; MAX_INPUT_BYTES + 10];
        assert_eq!(
            decide_from_reader(io::Cursor::new(big), Duration::from_secs(10)),
            Decision::Deny(DenyReason::InputTooLarge)
        );
    }
}
