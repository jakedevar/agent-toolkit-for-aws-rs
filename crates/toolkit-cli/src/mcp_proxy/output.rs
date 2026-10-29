//! The single stdout writer and its two bounded queues.
//!
//! - **Control queue**: proxy-generated lines (errors, timeouts), each held
//!   by a reservation from the separate control reserve. Served first at
//!   every line boundary. A producer waits while it is full, which pauses
//!   the stdin reader when the reader is the producer.
//! - **Data queue**: re-serialised upstream messages, each held by a
//!   reservation from the shared budget (counted as queued bytes, so budget
//!   waiters know draining will free room). A producer waits while it is
//!   full (backpressure), until it is cancelled or the proxy shuts down.
//!
//! Lines are written whole. A write error (EPIPE) ends the writer and is
//! reported to the proxy, which aborts everything.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::budget::Reservation;

/// One line ready for stdout (the newline included) and what holds its bytes.
#[derive(Debug)]
pub(crate) struct OutLine {
    pub bytes: Vec<u8>,
    pub hold: Reservation,
}

#[derive(Debug, Default)]
struct State {
    control: VecDeque<OutLine>,
    data: VecDeque<OutLine>,
    closed: bool,
}

/// Why a push did not queue its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushError {
    /// The producer was cancelled while it waited.
    Cancelled,
    /// The writer is gone.
    Closed,
}

/// How the writer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterEnd {
    /// The proxy closed the queues.
    Closed,
    /// stdout failed.
    Failed,
}

#[derive(Debug)]
pub(crate) struct Output {
    state: Mutex<State>,
    changed: Condvar,
    control_lines: usize,
    data_lines: usize,
}

/// How long a waiting producer sleeps before re-checking its cancellation
/// (cancellers also notify, so this is only a backstop).
const RECHECK: Duration = Duration::from_millis(50);

impl Output {
    pub(crate) fn new(control_lines: usize, data_lines: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            control_lines,
            data_lines,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue a control line, waiting while the control queue is full.
    pub(crate) fn push_control(&self, line: OutLine) -> Result<(), PushError> {
        let mut state = self.state();
        loop {
            if state.closed {
                return Err(PushError::Closed);
            }
            if state.control.len() < self.control_lines {
                state.control.push_back(line);
                drop(state);
                self.changed.notify_all();
                return Ok(());
            }
            state = self
                .changed
                .wait_timeout(state, RECHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Queue a data line, waiting while the data queue is full.
    pub(crate) fn push_data(
        &self,
        mut line: OutLine,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), PushError> {
        let mut state = self.state();
        loop {
            if state.closed {
                return Err(PushError::Closed);
            }
            if cancelled() {
                return Err(PushError::Cancelled);
            }
            if state.data.len() < self.data_lines {
                line.hold.mark_queued();
                state.data.push_back(line);
                drop(state);
                self.changed.notify_all();
                return Ok(());
            }
            state = self
                .changed
                .wait_timeout(state, RECHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Wake waiting producers so they re-check cancellation.
    pub(crate) fn wake(&self) {
        self.changed.notify_all();
    }

    /// Stop: queued lines are dropped (their reservations released) and
    /// every push fails from now on.
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        let control = std::mem::take(&mut state.control);
        let data = std::mem::take(&mut state.data);
        drop(state);
        drop(control);
        drop(data);
        self.changed.notify_all();
    }

    /// Write lines until closed or stdout fails: control first.
    pub(crate) fn run_writer(&self, sink: &mut dyn Write) -> WriterEnd {
        loop {
            let line = {
                let mut state = self.state();
                loop {
                    if state.closed {
                        return WriterEnd::Closed;
                    }
                    if let Some(line) = state.control.pop_front().or_else(|| state.data.pop_front())
                    {
                        break line;
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            // A producer may be waiting for queue room.
            self.changed.notify_all();
            let written = sink.write_all(&line.bytes).and_then(|()| sink.flush());
            drop(line);
            if written.is_err() {
                return WriterEnd::Failed;
            }
        }
    }
}
