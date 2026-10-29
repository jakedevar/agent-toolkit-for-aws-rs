//! Time, injectable so every deadline is tested with a fake clock.
//!
//! The proxy never reads the wall clock directly. Deadlines are durations
//! since the clock's origin. A waiter blocks on its condition variable for at
//! most [`Clock::wait_slice`] and then re-checks the clock, so a test that
//! advances a [`FakeClock`] decides every timeout without real waiting.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Monotonic time and backoff sleeps.
pub trait Clock: Send + Sync {
    /// Time since the clock's origin.
    fn now(&self) -> Duration;

    /// How long a waiter whose deadline is `remaining` away may block before
    /// it re-checks [`Clock::now`].
    fn wait_slice(&self, remaining: Duration) -> Duration;

    /// Sleep before a retry.
    fn sleep(&self, duration: Duration);
}

/// The real monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose origin is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn wait_slice(&self, remaining: Duration) -> Duration {
        remaining
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// How often a waiter re-checks a [`FakeClock`].
const FAKE_SLICE: Duration = Duration::from_millis(2);

/// A clock that moves only when a test calls [`FakeClock::advance`]. Sleeps
/// are recorded and return at once.
#[derive(Debug, Default)]
pub struct FakeClock {
    now: Mutex<Duration>,
    sleeps: Mutex<Vec<Duration>>,
}

impl FakeClock {
    /// A clock at time zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Move time forward.
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *now = now.saturating_add(by);
    }

    /// Backoff sleeps requested so far.
    #[must_use]
    pub fn sleeps(&self) -> Vec<Duration> {
        self.sleeps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_slice(&self, remaining: Duration) -> Duration {
        remaining.min(FAKE_SLICE)
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(duration);
    }
}
