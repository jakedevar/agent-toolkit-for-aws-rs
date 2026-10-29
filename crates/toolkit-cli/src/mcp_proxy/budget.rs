//! R-MEM: one process-wide byte budget with RAII reservations (#4999).
//!
//! Every variable-size buffer reserves its bytes here *before* it grows:
//! stdin line buffers, upstream body and SSE buffers, the parse charge, the
//! serialised output and queued stdout lines. A [`Reservation`] gives its
//! bytes back when it is dropped, so a delivered line, a rejection, an error,
//! a cancellation, a timeout or a disconnect always releases them.
//!
//! **Waiting without deadlock.** A reservation that does not fit waits only
//! while the stdout data queue holds bytes ([`Reservation::mark_queued`]),
//! because only the writer draining them is sure to free budget, and never
//! past its deadline. With the queue empty, or once the deadline passes, it
//! fails at once ([`BudgetError::Exhausted`]) and the caller drops what it
//! holds. Partial holders therefore never wait on one another.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::clock::Clock;

/// Why a reservation was not granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetError {
    /// It does not fit, and the data queue is empty or the wait timed out.
    Exhausted,
    /// The caller was cancelled while it waited.
    Cancelled,
    /// The proxy is shutting down.
    Closed,
}

#[derive(Debug, Default)]
struct State {
    in_use: usize,
    peak: usize,
    queued: usize,
    closed: bool,
}

#[derive(Debug)]
struct Inner {
    limit: usize,
    state: Mutex<State>,
    changed: Condvar,
}

/// A shared byte budget. Cloning shares it.
#[derive(Debug, Clone)]
pub struct Budget {
    inner: Arc<Inner>,
}

/// How a reservation may wait.
pub(crate) struct Wait<'a> {
    /// Time source for the deadline.
    pub clock: &'a dyn Clock,
    /// Give up at this time ([`Clock::now`] scale); `None` waits with no
    /// deadline (the stdin reader, which waits only on the proxy's own
    /// producers).
    pub deadline: Option<Duration>,
    /// Whether the waiter has been cancelled.
    pub cancelled: &'a dyn Fn() -> bool,
}

impl Budget {
    /// A budget of `limit` bytes with nothing reserved.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                limit,
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The limit.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.inner.limit
    }

    /// Bytes reserved now.
    #[must_use]
    pub fn in_use(&self) -> usize {
        self.state().in_use
    }

    /// The most bytes ever reserved at once (the test hook for
    /// `in_use <= limit` after every step).
    #[must_use]
    pub fn peak(&self) -> usize {
        self.state().peak
    }

    /// Bytes held by lines waiting in the stdout data queue.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.state().queued
    }

    /// A reservation of zero bytes, to grow later.
    #[must_use]
    pub fn empty(&self) -> Reservation {
        Reservation {
            budget: self.clone(),
            bytes: 0,
            queued: false,
        }
    }

    /// Reserve `bytes` if they fit now.
    #[must_use]
    pub fn try_reserve(&self, bytes: usize) -> Option<Reservation> {
        let mut state = self.state();
        if state.closed || !fits(self.inner.limit, state.in_use, bytes) {
            return None;
        }
        take(&mut state, bytes);
        drop(state);
        Some(Reservation {
            budget: self.clone(),
            bytes,
            queued: false,
        })
    }

    /// Reserve `bytes`, waiting as the module documentation describes.
    pub(crate) fn reserve(
        &self,
        bytes: usize,
        wait: &Wait<'_>,
    ) -> Result<Reservation, BudgetError> {
        self.acquire(bytes, wait)?;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
            queued: false,
        })
    }

    fn acquire(&self, bytes: usize, wait: &Wait<'_>) -> Result<(), BudgetError> {
        let mut state = self.state();
        loop {
            if state.closed {
                return Err(BudgetError::Closed);
            }
            if (wait.cancelled)() {
                return Err(BudgetError::Cancelled);
            }
            if fits(self.inner.limit, state.in_use, bytes) {
                take(&mut state, bytes);
                return Ok(());
            }
            let slice = match wait.deadline {
                Some(deadline) => {
                    let now = wait.clock.now();
                    if state.queued == 0 || now >= deadline {
                        return Err(BudgetError::Exhausted);
                    }
                    wait.clock.wait_slice(deadline - now)
                }
                // No deadline: the waiter is the stdin reader, which only the
                // proxy's own producers block; they always finish or fail.
                None => wait.clock.wait_slice(Duration::from_millis(50)),
            };
            state = self
                .inner
                .changed
                .wait_timeout(state, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn release(&self, bytes: usize, queued: bool) {
        let mut state = self.state();
        state.in_use = state.in_use.saturating_sub(bytes);
        if queued {
            state.queued = state.queued.saturating_sub(bytes);
        }
        drop(state);
        self.inner.changed.notify_all();
    }

    /// Fail every wait from now on and wake the waiters (shutdown).
    pub(crate) fn close(&self) {
        self.state().closed = true;
        self.inner.changed.notify_all();
    }

    /// Wake waiters so they re-check cancellation.
    pub(crate) fn wake(&self) {
        self.inner.changed.notify_all();
    }
}

fn fits(limit: usize, in_use: usize, bytes: usize) -> bool {
    in_use
        .checked_add(bytes)
        .is_some_and(|total| total <= limit)
}

fn take(state: &mut State, bytes: usize) {
    state.in_use += bytes;
    state.peak = state.peak.max(state.in_use);
}

/// Bytes held from a [`Budget`]; released when dropped.
#[derive(Debug)]
pub struct Reservation {
    budget: Budget,
    bytes: usize,
    queued: bool,
}

impl Reservation {
    /// Bytes held.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Hold `more` bytes too, waiting as [`Budget::reserve`] does.
    pub(crate) fn grow(&mut self, more: usize, wait: &Wait<'_>) -> Result<(), BudgetError> {
        if more == 0 {
            return Ok(());
        }
        self.budget.acquire(more, wait)?;
        self.bytes += more;
        if self.queued {
            self.budget.state().queued += more;
        }
        Ok(())
    }

    /// Give back everything above `bytes`.
    pub(crate) fn shrink_to(&mut self, bytes: usize) {
        if bytes < self.bytes {
            let excess = self.bytes - bytes;
            self.bytes = bytes;
            self.budget.release(excess, self.queued);
        }
    }

    /// Move `bytes` (at most all held) into a new reservation, without
    /// touching the budget: an SSE event leaving the parser's buffer keeps
    /// the bytes it already held.
    pub(crate) fn split(&mut self, bytes: usize) -> Reservation {
        debug_assert!(!self.queued, "queued bytes are never split");
        let moved = bytes.min(self.bytes);
        self.bytes -= moved;
        Reservation {
            budget: self.budget.clone(),
            bytes: moved,
            queued: false,
        }
    }

    /// Count these bytes as waiting in the stdout data queue.
    pub(crate) fn mark_queued(&mut self) {
        if !self.queued {
            self.queued = true;
            self.budget.state().queued += self.bytes;
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.budget.release(self.bytes, self.queued);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_proxy::clock::FakeClock;

    fn never() -> bool {
        false
    }

    #[test]
    fn reservations_release_on_drop_and_track_the_peak() {
        let budget = Budget::new(100);
        let a = budget.try_reserve(60).expect("fits");
        assert!(budget.try_reserve(41).is_none());
        let b = budget.try_reserve(40).expect("fits exactly");
        assert_eq!(budget.in_use(), 100);
        drop(a);
        drop(b);
        assert_eq!(budget.in_use(), 0);
        assert_eq!(budget.peak(), 100);
    }

    #[test]
    fn a_wait_with_an_empty_queue_fails_at_once() {
        let budget = Budget::new(10);
        let _held = budget.try_reserve(10).unwrap();
        let clock = FakeClock::new();
        let wait = Wait {
            clock: &clock,
            deadline: Some(Duration::from_secs(120)),
            cancelled: &never,
        };
        assert_eq!(
            budget.reserve(1, &wait).unwrap_err(),
            BudgetError::Exhausted
        );
    }

    #[test]
    fn a_wait_behind_queued_bytes_succeeds_when_they_drain() {
        let budget = Budget::new(10);
        let mut queued = budget.try_reserve(10).unwrap();
        queued.mark_queued();
        assert_eq!(budget.queued(), 10);
        let waiter = {
            let budget = budget.clone();
            std::thread::spawn(move || {
                let clock = FakeClock::new();
                let wait = Wait {
                    clock: &clock,
                    deadline: Some(Duration::from_secs(1)),
                    cancelled: &never,
                };
                budget.reserve(4, &wait).map(|r| r.bytes())
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        drop(queued);
        assert_eq!(waiter.join().unwrap(), Ok(4));
        assert_eq!(budget.in_use(), 0);
        assert_eq!(budget.queued(), 0);
    }

    #[test]
    fn grow_and_shrink_keep_the_count_exact() {
        let budget = Budget::new(50);
        let clock = FakeClock::new();
        let wait = Wait {
            clock: &clock,
            deadline: None,
            cancelled: &never,
        };
        let mut held = budget.empty();
        held.grow(30, &wait).unwrap();
        held.mark_queued();
        held.grow(5, &wait).unwrap();
        assert_eq!((budget.in_use(), budget.queued()), (35, 35));
        held.shrink_to(10);
        assert_eq!((budget.in_use(), budget.queued()), (10, 10));
        drop(held);
        assert_eq!((budget.in_use(), budget.queued()), (0, 0));
    }

    #[test]
    fn close_fails_waiters() {
        let budget = Budget::new(1);
        let _held = budget.try_reserve(1).unwrap();
        budget.close();
        let clock = FakeClock::new();
        let wait = Wait {
            clock: &clock,
            deadline: None,
            cancelled: &never,
        };
        assert_eq!(budget.reserve(1, &wait).unwrap_err(), BudgetError::Closed);
        assert!(budget.try_reserve(0).is_none());
    }
}
