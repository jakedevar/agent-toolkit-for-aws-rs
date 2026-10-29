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
    exchange: impl FnOnce(&str, &str, Instant) -> Result<Vec<u8>, ()>,
) -> io::Result<HelperResult> {
    let start = Instant::now();
    let flow_end = start + Duration::from_secs(600);
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
    gate::start_watchdog(
        Arc::clone(&gate),
        cutoff + Duration::from_secs(2),
        move || {
            let _ = watchdog_custody.write(
                port,
                wall_deadline,
                wall_cutoff,
                PendingStatus::TimedOut,
                None,
                &OsRandom,
            );
        },
    );
    announce(port)?;
    let outcome = listener::serve(listener, state, flow_end)?;
    let (status, reason, result) = match outcome {
        None => (PendingStatus::TimedOut, None, HelperResult::TimedOut),
        Some(Outcome::Denied(_)) => (
            PendingStatus::Failed,
            Some("authorization_denied"),
            HelperResult::Failed,
        ),
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
                Ok(bytes) => match gate::publish_session(&gate, cutoff, dir, &bytes, &OsRandom) {
                    Ok(()) => (PendingStatus::Succeeded, None, HelperResult::Succeeded),
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        return Ok(HelperResult::TimedOut)
                    }
                    Err(e) => return Err(e),
                },
                Err(()) => (
                    PendingStatus::Failed,
                    Some("exchange_failed"),
                    HelperResult::Failed,
                ),
            }
        }
    };
    custody.write(port, wall_deadline, wall_cutoff, status, reason, &OsRandom)?;
    Ok(result)
}
