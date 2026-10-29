//! Callback helper lifecycle. Exchange and port reporting are injected by the command layer.
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    gate::{self, CommitGate},
    listener,
    pending::Custody,
    Outcome, PendingStatus,
};
use crate::launch::clock::OsRandom;

/// Callback outcome retained in the pending record as a fixed reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperResult {
    Succeeded,
    TimedOut,
    Failed,
}

fn exchange_failure(gate: &CommitGate, now: Instant, cutoff: Instant) -> Option<HelperResult> {
    if !gate.fail_exchange() {
        return None;
    }
    Some(if now >= cutoff {
        HelperResult::TimedOut
    } else {
        HelperResult::Failed
    })
}

fn publication_timeout(gate: &CommitGate) -> Option<PendingStatus> {
    gate.fail_exchange().then_some(PendingStatus::TimedOut)
}

/// Run a helper in its own process, retaining lock custody through return.
/// `exchange` receives the code, redirect URI and absolute exchange cutoff;
/// it must return complete session JSON or a fixed failure, with no secrets in errors.
/// The caller must exit immediately after return: a watchdog thread exits the
/// process on deadline and deliberately never joins or signals another thread.
///
/// # Errors
/// Returns a local socket or private-record failure.
#[cfg(unix)]
pub fn run(
    dir: &Path,
    state: &str,
    wall_deadline: i64,
    announce: impl FnOnce(u16) -> io::Result<()>,
    exchange: impl FnOnce(&str, &str, Instant) -> Result<Vec<u8>, &'static str>,
) -> io::Result<HelperResult> {
    run_with(
        dir,
        state,
        wall_deadline,
        announce,
        exchange,
        Instant::now,
        gate::start_watchdog,
        listener::serve,
    )
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_with(
    dir: &Path,
    state: &str,
    wall_deadline: i64,
    announce: impl FnOnce(u16) -> io::Result<()>,
    exchange: impl FnOnce(&str, &str, Instant) -> Result<Vec<u8>, &'static str>,
    now: impl Fn() -> Instant,
    watchdog: impl FnOnce(Arc<CommitGate>, Instant, Box<dyn FnOnce() + Send>),
    serve: impl FnOnce(std::net::TcpListener, &str, Instant) -> io::Result<Option<Outcome>>,
) -> io::Result<HelperResult> {
    let start = now();
    let wall_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| io::Error::other("clock unavailable"))?
        .as_secs();
    let remaining = u64::try_from(wall_deadline)
        .unwrap_or(0)
        .saturating_sub(wall_now)
        .min(600);
    let flow_end = start + Duration::from_secs(remaining);
    let cutoff = flow_end + Duration::from_secs(60);
    let wall_cutoff = wall_deadline.saturating_add(60);
    let custody = Arc::new(Custody::acquire(dir, std::process::id())?);
    let listener = listener::bind()?;
    let port = listener.local_addr()?.port();
    custody.write(
        port,
        wall_deadline,
        wall_cutoff,
        PendingStatus::Listening,
        None,
        &OsRandom,
    )?;
    let gate = Arc::new(CommitGate::default());
    let watchdog_custody = Arc::clone(&custody);
    watchdog(
        Arc::clone(&gate),
        cutoff + Duration::from_secs(2),
        Box::new(move || {
            let _ = watchdog_custody.write(
                port,
                wall_deadline,
                wall_cutoff,
                PendingStatus::TimedOut,
                None,
                &OsRandom,
            );
        }),
    );
    announce(port)?;
    let outcome = serve(listener, state, flow_end)?;
    let (status, reason, result) = match outcome {
        None => (PendingStatus::TimedOut, None, HelperResult::TimedOut),
        Some(Outcome::Denied(error)) => {
            custody.write_with_error(
                port,
                wall_deadline,
                wall_cutoff,
                PendingStatus::Failed,
                Some("authorization_denied"),
                Some(&error),
                &OsRandom,
            )?;
            return Ok(HelperResult::Failed);
        }
        Some(Outcome::InvalidCode) => (
            PendingStatus::Failed,
            Some("invalid_code"),
            HelperResult::Failed,
        ),
        Some(Outcome::Ignore(_)) => unreachable!("listener does not terminate on ignored requests"),
        Some(Outcome::Code(code)) => {
            if !gate.start_exchange() {
                return Ok(HelperResult::TimedOut);
            }
            custody.write(
                port,
                wall_deadline,
                wall_cutoff,
                PendingStatus::Exchanging,
                None,
                &OsRandom,
            )?;
            let redirect = format!("http://127.0.0.1:{port}");
            match exchange(&code, &redirect, cutoff) {
                Ok(bytes) => match gate::publish_with(
                    &gate,
                    cutoff,
                    dir,
                    &bytes,
                    &OsRandom,
                    &now,
                    || {},
                    |from, to| std::fs::rename(from, to),
                ) {
                    Ok(()) => (PendingStatus::Succeeded, None, HelperResult::Succeeded),
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        match publication_timeout(&gate) {
                            Some(status) => (status, None, HelperResult::TimedOut),
                            None => return Ok(HelperResult::TimedOut),
                        }
                    }
                    Err(e) => return Err(e),
                },
                Err(reason) => match exchange_failure(&gate, now(), cutoff) {
                    None => return Ok(HelperResult::TimedOut),
                    Some(HelperResult::TimedOut) => {
                        (PendingStatus::TimedOut, None, HelperResult::TimedOut)
                    }
                    Some(HelperResult::Failed) => {
                        (PendingStatus::Failed, Some(reason), HelperResult::Failed)
                    }
                    Some(HelperResult::Succeeded) => unreachable!("failure cannot succeed"),
                },
            }
        }
    };
    custody.write(port, wall_deadline, wall_cutoff, status, reason, &OsRandom)?;
    Ok(result)
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    fn wall_deadline() -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap()
            + 30
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_rejects_exchange_response_after_cutoff() {
        use crate::launch::{callback::pending::read_record, tests_support::Scratch};
        let scratch = Scratch::new("late-lifecycle");
        let clock = std::cell::Cell::new(Instant::now());
        let result = run_with(
            scratch.path(),
            "s",
            wall_deadline(),
            |_| Ok(()),
            |_, _, cutoff| {
                clock.set(cutoff + Duration::from_millis(1));
                Ok(b"session".to_vec())
            },
            || clock.get(),
            |_, _, _| {},
            |_, _, _| Ok(Some(Outcome::Code("ok".into()))),
        )
        .unwrap();
        assert_eq!(result, HelperResult::TimedOut);
        assert!(!scratch.join("session.json").exists());
        assert_eq!(
            read_record(scratch.path(), std::process::id())
                .unwrap()
                .status,
            PendingStatus::TimedOut
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_transport_attempt_crossing_cutoff_never_retries() {
        use crate::launch::{
            callback::pending::read_record,
            http::{self, MonotonicClock, Operation},
            tests_support::Scratch,
        };
        use toolkit_aws::{
            retry::{NoJitter, RetryPolicy, Sleeper},
            stream::{ResponseStream, StreamLimits, StreamingTransport},
            testing::FakeStreamingTransport,
            transport::{Headers, HttpRequest, Method, Timeouts},
            AwsError,
        };
        struct Clock(std::cell::Cell<Instant>);
        impl MonotonicClock for Clock {
            fn now(&self) -> Instant {
                self.0.get()
            }
        }
        impl Sleeper for Clock {
            fn sleep(&self, duration: Duration) {
                self.0.set(self.0.get() + duration);
            }
        }
        struct Crossing<'a> {
            clock: &'a Clock,
            fake: FakeStreamingTransport,
            cutoff: Instant,
        }
        impl StreamingTransport for Crossing<'_> {
            fn open<'a>(
                &'a self,
                request: &HttpRequest,
                limits: &StreamLimits,
            ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
                assert!(self.clock.now() < self.cutoff);
                self.clock.0.set(self.cutoff + Duration::from_millis(1));
                self.fake.open(request, limits)
            }
        }
        let scratch = Scratch::new("crossing-attempt");
        let clock = Clock(std::cell::Cell::new(Instant::now()));
        let result = run_with(
            scratch.path(),
            "s",
            wall_deadline(),
            |_| Ok(()),
            |_, _, cutoff| {
                let transport = Crossing {
                    clock: &clock,
                    cutoff,
                    fake: FakeStreamingTransport::new()
                        .respond(503, &[], vec![])
                        .respond(200, &[], vec![]),
                };
                let request = HttpRequest {
                    method: Method::Post,
                    url: "https://example.test/token".into(),
                    headers: Headers::new(),
                    body: vec![],
                    timeouts: Timeouts::default(),
                };
                let response = http::run(
                    &transport,
                    &request,
                    Operation::Oidc,
                    cutoff,
                    Duration::from_secs(30),
                    true,
                    &clock,
                    &clock,
                    &NoJitter,
                    &RetryPolicy::default(),
                )
                .unwrap();
                assert_eq!(response.status, 503);
                assert_eq!(transport.fake.requests().len(), 1);
                Err("exchange_failed")
            },
            || clock.now(),
            |_, _, _| {},
            |_, _, _| Ok(Some(Outcome::Code("ok".into()))),
        )
        .unwrap();
        assert_eq!(result, HelperResult::TimedOut);
        assert!(!scratch.join("session.json").exists());
        assert_eq!(
            read_record(scratch.path(), std::process::id())
                .unwrap()
                .status,
            PendingStatus::TimedOut
        );
    }

    #[cfg(unix)]
    #[test]
    fn exhausted_listener_records_timeout_without_traffic_status() {
        use crate::launch::{callback::pending::read_record, tests_support::Scratch};
        let scratch = Scratch::new("listener-timeout");
        let result = run_with(
            scratch.path(),
            "s",
            wall_deadline(),
            |_| Ok(()),
            |_, _, _| panic!("no exchange"),
            Instant::now,
            |_, _, _| {},
            |_, _, _| Ok(None),
        )
        .unwrap();
        assert_eq!(result, HelperResult::TimedOut);
        let status = read_record(scratch.path(), std::process::id())
            .unwrap()
            .status;
        assert_eq!(status, PendingStatus::TimedOut);
        // An exhaustive match makes adding a traffic-driven state visible here.
        match status {
            PendingStatus::Listening
            | PendingStatus::Exchanging
            | PendingStatus::Succeeded
            | PendingStatus::Failed
            | PendingStatus::TimedOut => {}
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_socket_lifecycle_records_exchange_and_success() {
        use crate::launch::{callback::pending::read_record, tests_support::Scratch};
        use std::io::Write;
        let scratch = Scratch::new("real-lifecycle");
        let result = run_with(
            scratch.path(),
            "s",
            wall_deadline(),
            |port| {
                let mut stream =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
                write!(
                    stream,
                    "GET /?state=s&code=ok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
                )?;
                Ok(())
            },
            |code, _, _| {
                assert_eq!(code, "ok");
                assert_eq!(
                    read_record(scratch.path(), std::process::id())
                        .unwrap()
                        .status,
                    PendingStatus::Exchanging
                );
                Ok(b"session".to_vec())
            },
            Instant::now,
            |_, _, _| {},
            listener::serve,
        )
        .unwrap();
        assert_eq!(result, HelperResult::Succeeded);
        assert_eq!(
            read_record(scratch.path(), std::process::id())
                .unwrap()
                .status,
            PendingStatus::Succeeded
        );
        assert_eq!(
            std::fs::read(scratch.join("session.json")).unwrap(),
            b"session"
        );
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_aborts_blocked_exchange_without_late_publication() {
        use crate::launch::{callback::pending::read_record, tests_support::Scratch};
        let scratch = Scratch::new("blocked-lifecycle");
        let (watch_tx, watch_rx) = std::sync::mpsc::sync_channel(1);
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let path = scratch.path();
            let worker = scope.spawn(move || {
                run_with(
                    path,
                    "s",
                    wall_deadline(),
                    |_| Ok(()),
                    |_, _, _| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(b"session".to_vec())
                    },
                    Instant::now,
                    |gate, _, record| {
                        watch_tx.send((gate, record)).unwrap();
                    },
                    |_, _, _| Ok(Some(Outcome::Code("ok".into()))),
                )
            });
            let (gate, record) = watch_rx.recv().unwrap();
            entered_rx.recv().unwrap();
            assert_eq!(
                read_record(scratch.path(), std::process::id())
                    .unwrap()
                    .status,
                PendingStatus::Exchanging
            );
            gate::watchdog_step(&gate, record, |code| assert_eq!(code, 1));
            assert_eq!(
                read_record(scratch.path(), std::process::id())
                    .unwrap()
                    .status,
                PendingStatus::TimedOut
            );
            release_tx.send(()).unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), HelperResult::TimedOut);
        });
        assert!(!scratch.join("session.json").exists());
        assert_eq!(
            read_record(scratch.path(), std::process::id())
                .unwrap()
                .status,
            PendingStatus::TimedOut
        );
    }
    #[test]
    fn exchange_failure_at_cutoff_reports_timeout() {
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        let now = Instant::now();
        assert_eq!(
            exchange_failure(&gate, now, now),
            Some(HelperResult::TimedOut)
        );
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert_eq!(
            exchange_failure(&gate, now, now + Duration::from_secs(1)),
            Some(HelperResult::Failed)
        );
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert_eq!(gate.watchdog(), gate::WatchdogAction::RecordTimeoutAndExit);
        assert_eq!(exchange_failure(&gate, now, now), None);
    }

    #[test]
    fn refused_publication_after_cutoff_records_timeout() {
        let gate = CommitGate::default();
        assert!(gate.start_exchange());
        assert_eq!(publication_timeout(&gate), Some(PendingStatus::TimedOut));
        let aborted = CommitGate::default();
        assert!(aborted.start_exchange());
        assert_eq!(
            aborted.watchdog(),
            gate::WatchdogAction::RecordTimeoutAndExit
        );
        assert_eq!(publication_timeout(&aborted), None);
    }
}
