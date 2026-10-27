//! Bounded retries with exponential backoff and full jitter.
//!
//! Matches the Python clients' `retries={"max_attempts": 3, "mode": "adaptive"}`:
//! three retries after the first attempt (botocore counts `max_attempts` as
//! retries, so four attempts in total) with botocore's standard schedule
//! (`random() * 2^retry`, capped at 20 seconds). Sleeping and randomness are
//! injected ([`Sleeper`], [`Jitter`]) so tests run instantly and
//! deterministically.
//!
//! [`RetryMode::Adaptive`] is carried as configuration for the SDK binding,
//! which owns the client-side rate limiter; [`run_with_retry`] always applies the
//! standard schedule.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::AwsError;

/// Hard cap on retries accepted by [`RetryPolicy::new`].
pub const MAX_RETRIES_CAP: u32 = 10;

/// Longest backoff accepted by [`RetryPolicy::new`].
pub const MAX_BACKOFF_CAP: Duration = Duration::from_secs(300);

/// Retry strategy label, mirroring botocore's `mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryMode {
    /// Exponential backoff with jitter.
    Standard,
    /// Standard plus a client-side rate limiter (applied by the SDK binding).
    Adaptive,
}

/// How many times and how patiently to retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_retries: u32,
    mode: RetryMode,
    base_delay: Duration,
    max_backoff: Duration,
}

impl Default for RetryPolicy {
    /// Three retries, adaptive mode, 1 s base delay, 20 s cap (the Python
    /// `launch-with-aws` and botocore standard values).
    fn default() -> Self {
        Self {
            max_retries: 3,
            mode: RetryMode::Adaptive,
            base_delay: Duration::from_secs(1),
            max_backoff: Duration::from_secs(20),
        }
    }
}

impl RetryPolicy {
    /// A validated policy.
    ///
    /// # Errors
    /// `InvalidConfig` when `max_retries` exceeds [`MAX_RETRIES_CAP`], the base
    /// delay is zero, or the cap is below the base delay or above
    /// [`MAX_BACKOFF_CAP`].
    pub fn new(
        max_retries: u32,
        mode: RetryMode,
        base_delay: Duration,
        max_backoff: Duration,
    ) -> Result<Self, AwsError> {
        if max_retries > MAX_RETRIES_CAP {
            return Err(AwsError::invalid_config(
                "retries",
                format!("at most {MAX_RETRIES_CAP} retries are allowed"),
            ));
        }
        if base_delay.is_zero() {
            return Err(AwsError::invalid_config(
                "retries",
                "the base delay must be greater than zero",
            ));
        }
        if max_backoff < base_delay || max_backoff > MAX_BACKOFF_CAP {
            return Err(AwsError::invalid_config(
                "retries",
                "the maximum backoff must be at least the base delay and at most 300 seconds",
            ));
        }
        Ok(Self {
            max_retries,
            mode,
            base_delay,
            max_backoff,
        })
    }

    /// A policy that never retries.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Retries after the first attempt.
    #[must_use]
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Total attempts, first included.
    #[must_use]
    pub fn total_attempts(&self) -> u32 {
        self.max_retries + 1
    }

    /// Strategy label passed to the SDK binding.
    #[must_use]
    pub fn mode(&self) -> RetryMode {
        self.mode
    }

    /// Base delay (the delay bound doubles per retry from here).
    #[must_use]
    pub fn base_delay(&self) -> Duration {
        self.base_delay
    }

    /// Upper bound of any single backoff.
    #[must_use]
    pub fn max_backoff(&self) -> Duration {
        self.max_backoff
    }

    /// Largest possible sleep before retry number `retry_number` (1 for the
    /// first retry): `min(max_backoff, base_delay * 2^retry_number)`.
    #[must_use]
    pub fn delay_upper_bound(&self, retry_number: u32) -> Duration {
        let factor = 1_u32 << retry_number.min(16);
        self.base_delay
            .checked_mul(factor)
            .map_or(self.max_backoff, |delay| delay.min(self.max_backoff))
    }

    /// The jittered sleep before retry number `retry_number`.
    #[must_use]
    pub fn delay(&self, retry_number: u32, jitter: &dyn Jitter) -> Duration {
        let upper =
            u64::try_from(self.delay_upper_bound(retry_number).as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(jitter.pick(upper).min(upper))
    }
}

/// Source of backoff randomness.
pub trait Jitter {
    /// A value in `0..=upper_ms`.
    fn pick(&self, upper_ms: u64) -> u64;
}

/// Always the upper bound: deterministic, for tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoJitter;

impl Jitter for NoJitter {
    fn pick(&self, upper_ms: u64) -> u64 {
        upper_ms
    }
}

/// Full jitter from a small xorshift generator seeded by the standard
/// library's per-process random state.
#[derive(Debug)]
pub struct FullJitter {
    state: AtomicU64,
}

impl FullJitter {
    /// A generator with a fresh random seed.
    #[must_use]
    pub fn new() -> Self {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0x9E37_79B9_7F4A_7C15);
        Self {
            state: AtomicU64::new(hasher.finish() | 1),
        }
    }
}

impl Default for FullJitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Jitter for FullJitter {
    fn pick(&self, upper_ms: u64) -> u64 {
        let previous = self
            .state
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |mut x| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                Some(x)
            })
            .unwrap_or_else(|unchanged| unchanged);
        let mixed = previous.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        match upper_ms.checked_add(1) {
            Some(modulus) => mixed % modulus,
            None => mixed,
        }
    }
}

/// Waits between attempts; injected so tests never sleep.
pub trait Sleeper {
    /// Block for `duration`.
    fn sleep(&self, duration: Duration);
}

/// Sleeps the calling thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A successful result and how many attempts it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempted<T> {
    /// The successful value.
    pub value: T,
    /// Attempts made, the successful one included.
    pub attempts: u32,
}

/// The final error and how many attempts were made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryFailure {
    /// The error of the last attempt.
    pub error: AwsError,
    /// Attempts made.
    pub attempts: u32,
}

impl fmt::Display for RetryFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (after {} attempt(s))", self.error, self.attempts)
    }
}

impl std::error::Error for RetryFailure {}

impl From<RetryFailure> for AwsError {
    fn from(failure: RetryFailure) -> Self {
        failure.error
    }
}

/// Run `operation` (given the 1-based attempt number), retrying retryable
/// errors per `policy` with jittered exponential backoff.
///
/// # Errors
/// Returns a [`RetryFailure`] with the last error when the error is not
/// retryable or the retries are exhausted.
pub fn run_with_retry<T>(
    policy: &RetryPolicy,
    sleeper: &dyn Sleeper,
    jitter: &dyn Jitter,
    mut operation: impl FnMut(u32) -> Result<T, AwsError>,
) -> Result<Attempted<T>, RetryFailure> {
    let mut attempt = 1_u32;
    loop {
        match operation(attempt) {
            Ok(value) => {
                return Ok(Attempted {
                    value,
                    attempts: attempt,
                })
            }
            Err(error) => {
                let retries_used = attempt - 1;
                if !error.is_retryable() || retries_used >= policy.max_retries {
                    return Err(RetryFailure {
                        error,
                        attempts: attempt,
                    });
                }
                sleeper.sleep(policy.delay(attempt, jitter));
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ServiceError, TimeoutPhase};
    use crate::testing::RecordingSleeper;

    #[test]
    fn retry_failure_debug_scrubs_nested_hand_built_errors() {
        let failure = RetryFailure {
            error: AwsError::Connect {
                message: "dial https://FAKE-USER:FAKE-URL-PASSWORD@x.amazonaws.com token=FAKE-TOKEN-VALUE"
                    .to_string(),
            },
            attempts: 4,
        };
        for shown in [
            format!("{failure:?}"),
            format!("{failure:#?}"),
            failure.to_string(),
        ] {
            assert!(!shown.contains("FAKE-"), "{shown}");
        }
        assert!(format!("{failure:?}").contains("attempts: 4"));
        let attempted = Attempted {
            value: AwsError::Pagination {
                reason: "{\"password\":\"aa\\\"FAKE-TAIL-LEAK\"}".to_string(),
            },
            attempts: 2,
        };
        assert!(!format!("{attempted:?}").contains("FAKE-TAIL-LEAK"));
    }

    fn status(code: u16) -> AwsError {
        AwsError::service(ServiceError::new("GET", "/x", code, None, "", None))
    }

    fn secs(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|v| Duration::from_secs(*v)).collect()
    }

    #[test]
    fn default_policy_matches_the_python_clients() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries(), 3);
        assert_eq!(policy.total_attempts(), 4);
        assert_eq!(policy.mode(), RetryMode::Adaptive);
        assert_eq!(policy.base_delay(), Duration::from_secs(1));
        assert_eq!(policy.max_backoff(), Duration::from_secs(20));
        assert_eq!(RetryPolicy::none().total_attempts(), 1);
    }

    #[test]
    fn delay_bounds_double_and_cap() {
        let policy = RetryPolicy::default();
        let bounds: Vec<Duration> = (1..=6).map(|n| policy.delay_upper_bound(n)).collect();
        assert_eq!(bounds, secs(&[2, 4, 8, 16, 20, 20]));
        assert_eq!(policy.delay_upper_bound(10_000), Duration::from_secs(20));
    }

    #[test]
    fn validation_rejects_unsafe_policies() {
        let ok = Duration::from_secs(1);
        assert!(RetryPolicy::new(MAX_RETRIES_CAP, RetryMode::Standard, ok, ok).is_ok());
        let too_many = RetryPolicy::new(MAX_RETRIES_CAP + 1, RetryMode::Standard, ok, ok);
        assert_eq!(too_many.unwrap_err().code(), "aws_invalid_config");
        assert!(RetryPolicy::new(1, RetryMode::Standard, Duration::ZERO, ok).is_err());
        assert!(RetryPolicy::new(1, RetryMode::Standard, Duration::from_secs(5), ok).is_err());
        assert!(RetryPolicy::new(1, RetryMode::Standard, ok, Duration::from_secs(301)).is_err());
    }

    #[test]
    fn first_success_makes_one_attempt_and_never_sleeps() {
        let sleeper = RecordingSleeper::new();
        let result = run_with_retry(&RetryPolicy::default(), &sleeper, &NoJitter, |_| Ok("done"));
        assert_eq!(
            result.unwrap(),
            Attempted {
                value: "done",
                attempts: 1
            }
        );
        assert!(sleeper.sleeps().is_empty());
    }

    #[test]
    fn retryable_failures_back_off_then_succeed() {
        let sleeper = RecordingSleeper::new();
        let mut script = vec![Err(status(429)), Err(status(503)), Ok(7)].into_iter();
        let mut seen = Vec::new();
        let result = run_with_retry(&RetryPolicy::default(), &sleeper, &NoJitter, |attempt| {
            seen.push(attempt);
            script.next().expect("script exhausted")
        });
        assert_eq!(
            result.unwrap(),
            Attempted {
                value: 7,
                attempts: 3
            }
        );
        assert_eq!(seen, vec![1, 2, 3]);
        assert_eq!(sleeper.sleeps(), secs(&[2, 4]));
    }

    #[test]
    fn exhausted_retries_return_the_last_error_with_attempt_count() {
        let sleeper = RecordingSleeper::new();
        let mut calls = 0;
        let failure = run_with_retry::<()>(&RetryPolicy::default(), &sleeper, &NoJitter, |_| {
            calls += 1;
            Err(AwsError::timeout(TimeoutPhase::Read, "slow"))
        })
        .unwrap_err();
        assert_eq!(calls, 4);
        assert_eq!(failure.attempts, 4);
        assert_eq!(failure.error.code(), "aws_timeout");
        assert_eq!(sleeper.sleeps(), secs(&[2, 4, 8]));
        assert_eq!(
            failure.to_string(),
            "read timeout: slow (after 4 attempt(s))"
        );
        assert_eq!(AwsError::from(failure).code(), "aws_timeout");
    }

    #[test]
    fn non_retryable_errors_stop_immediately() {
        for error in [
            status(400),
            status(403),
            AwsError::NoCredentials,
            AwsError::invalid_config("region", "bad"),
        ] {
            let sleeper = RecordingSleeper::new();
            let mut calls = 0;
            let failure =
                run_with_retry::<()>(&RetryPolicy::default(), &sleeper, &NoJitter, |_| {
                    calls += 1;
                    Err(error.clone())
                })
                .unwrap_err();
            assert_eq!((calls, failure.attempts), (1, 1), "{error}");
            assert!(sleeper.sleeps().is_empty());
        }
    }

    #[test]
    fn a_none_policy_never_retries_even_retryable_errors() {
        let sleeper = RecordingSleeper::new();
        let failure = run_with_retry::<()>(&RetryPolicy::none(), &sleeper, &NoJitter, |_| {
            Err(status(500))
        })
        .unwrap_err();
        assert_eq!(failure.attempts, 1);
        assert!(sleeper.sleeps().is_empty());
    }

    #[test]
    fn full_jitter_stays_within_bounds_and_varies() {
        let jitter = FullJitter::new();
        let samples: Vec<u64> = (0..200).map(|_| jitter.pick(1000)).collect();
        assert!(samples.iter().all(|s| *s <= 1000));
        assert!(
            samples.iter().any(|s| *s != samples[0]),
            "jitter produced one value"
        );
        assert_eq!(jitter.pick(0), 0);
        let _ = jitter.pick(u64::MAX);
    }

    #[test]
    fn jittered_delay_never_exceeds_the_bound() {
        struct Max;
        impl Jitter for Max {
            fn pick(&self, _: u64) -> u64 {
                u64::MAX
            }
        }
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(1, &Max), Duration::from_secs(2));
        assert_eq!(policy.delay(1, &NoJitter), Duration::from_secs(2));
    }
}
