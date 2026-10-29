//! Bounded, status-aware streaming for Launch and OIDC.
use super::buffer::CappedBuf;
use std::time::{Duration, Instant};
use toolkit_aws::retry::{Jitter, RetryPolicy, Sleeper};
use toolkit_aws::stream::{StreamError, StreamLimits, StreamingTransport};
use toolkit_aws::transport::HttpRequest;

pub const SMALL_CAP: usize = 64 * 1024;
pub const API_CAP: usize = 64 * 1024 * 1024;
pub const DOWNLOAD_CAP: usize = 500 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Oidc,
    ApiJson,
    Delete,
    Put,
    Download,
}
impl Operation {
    fn ceiling(self) -> usize {
        match self {
            Self::Oidc | Self::Delete => SMALL_CAP,
            Self::ApiJson => API_CAP,
            Self::Put => 1,
            Self::Download => DOWNLOAD_CAP,
        }
    }
    fn cap(self, status: u16) -> Option<usize> {
        if (300..400).contains(&status)
            || self == Self::Put
            || (self == Self::Download && status >= 400)
            || (self == Self::Delete && status < 300)
        {
            None
        } else if status >= 400 {
            Some(SMALL_CAP)
        } else {
            Some(self.ceiling())
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    InvalidResponse,
    TooLarge,
    Deadline,
    Transport { retryable: bool },
    Allocation,
}
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub error_header: Option<String>,
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}
pub fn limits_for(operation: Operation, deadline: Instant) -> StreamLimits {
    // Both pairs are fixed, positive production constants.
    StreamLimits::new(
        operation.ceiling() as u64,
        if operation == Operation::Put {
            1
        } else {
            SMALL_CAP
        },
    )
    .expect("positive stream limits")
    .with_deadline(deadline)
}
pub fn collect_response(
    transport: &dyn StreamingTransport,
    request: &HttpRequest,
    operation: Operation,
    deadline: Instant,
) -> Result<Response, BodyError> {
    let limits = limits_for(operation, deadline);
    let mut stream = transport.open(request, &limits).map_err(|e| match e {
        toolkit_aws::AwsError::Timeout {
            phase: toolkit_aws::error::TimeoutPhase::Deadline,
            ..
        } => BodyError::Deadline,
        other => BodyError::Transport {
            retryable: other.is_retryable(),
        },
    })?;
    let status = stream.status();
    let error_header = if status >= 400 {
        stream.headers().get("x-amzn-errortype").map(str::to_owned)
    } else {
        None
    };
    let Some(cap) = operation.cap(status) else {
        return Ok(Response {
            status,
            body: Vec::new(),
            error_header,
        });
    };
    if cap < operation.ceiling() {
        stream.narrow_body_limit(cap as u64);
    }
    let length = stream
        .headers()
        .get("content-length")
        .map(|s| {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(BodyError::InvalidResponse);
            }
            s.parse::<u64>().map_err(|_| BodyError::InvalidResponse)
        })
        .transpose()?;
    if length.is_some_and(|n| n > cap as u64) {
        return if status >= 400 {
            Ok(Response {
                status,
                body: Vec::new(),
                error_header,
            })
        } else {
            Err(BodyError::TooLarge)
        };
    }
    let mut buf = CappedBuf::new(cap);
    loop {
        let chunk = match stream.next_chunk() {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(StreamError::TooLarge) if status >= 400 => {
                return Ok(Response {
                    status,
                    body: Vec::new(),
                    error_header,
                })
            }
            Err(StreamError::TooLarge) => return Err(BodyError::TooLarge),
            Err(StreamError::DeadlineExceeded) => return Err(BodyError::Deadline),
            Err(StreamError::IdleTimeout) => return Err(BodyError::Transport { retryable: true }),
            Err(StreamError::Failed(e)) => {
                return Err(BodyError::Transport {
                    retryable: e.is_retryable(),
                })
            }
        };
        if length.is_some_and(|n| (buf.len() as u64).saturating_add(chunk.len() as u64) > n) {
            return Err(BodyError::InvalidResponse);
        }
        buf.extend(&chunk).map_err(|e| match e {
            super::buffer::BufError::Limit { .. } => BodyError::TooLarge,
            super::buffer::BufError::Alloc => BodyError::Allocation,
        })?;
    }
    Ok(Response {
        status,
        body: buf.into_vec(),
        error_header,
    })
}

pub trait MonotonicClock {
    fn now(&self) -> Instant;
}
pub struct SystemClock;
impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
/// Retry only before the absolute operation deadline. An authorization-code
/// exchange does not retry ambiguous transport failures or HTTP responses.
#[allow(clippy::too_many_arguments)]
pub fn run(
    transport: &dyn StreamingTransport,
    request: &HttpRequest,
    operation: Operation,
    deadline: Instant,
    attempt_limit: Duration,
    code_exchange: bool,
    clock: &dyn MonotonicClock,
    sleeper: &dyn Sleeper,
    jitter: &dyn Jitter,
    policy: &RetryPolicy,
) -> Result<Response, BodyError> {
    let mut attempt = 0;
    loop {
        let now = clock.now();
        if now >= deadline {
            return Err(BodyError::Deadline);
        }
        attempt += 1;
        let attempt_deadline = now
            .checked_add(attempt_limit)
            .unwrap_or(deadline)
            .min(deadline);
        let result = collect_response(transport, request, operation, attempt_deadline);
        let retryable = match &result {
            Ok(r) => !code_exchange && matches!(r.status, 429 | 500 | 502 | 503 | 504),
            Err(BodyError::Transport { retryable }) => *retryable && !code_exchange,
            _ => false,
        };
        if !retryable || attempt >= policy.total_attempts() {
            return result;
        }
        let delay = policy.delay(attempt, jitter);
        if clock
            .now()
            .checked_add(delay)
            .is_none_or(|end| end >= deadline)
        {
            return result;
        }
        sleeper.sleep(delay);
    }
}
#[cfg(test)]
mod tests {
    use super::{collect_response, BodyError, Operation};
    use std::time::{Duration, Instant};
    use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport};
    use toolkit_aws::transport::{Headers, HttpRequest, Method, Timeouts};

    fn request() -> HttpRequest {
        HttpRequest {
            method: Method::Post,
            url: "https://oidc.us-east-1.amazonaws.com/token".into(),
            headers: Headers::new(),
            body: Vec::new(),
            timeouts: Timeouts::default(),
        }
    }

    #[test]
    fn oidc_collects_with_deadline_and_rejects_misleading_length() {
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[("content-length", "2")],
            vec![FakeChunk::Data(b"abc".to_vec())],
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = collect_response(&fake, &request(), Operation::Oidc, deadline);
        assert_eq!(result.unwrap_err(), BodyError::InvalidResponse);
        assert_eq!(fake.open_limits()[0].deadline(), Some(deadline));
    }

    #[test]
    fn put_never_reads_even_with_absurd_content_length() {
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[("content-length", "999999999999999999")],
            vec![FakeChunk::Data(vec![1])],
        );
        let result = collect_response(
            &fake,
            &request(),
            Operation::Put,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(result.status, 200);
        assert!(result.body.is_empty());
        assert_eq!(fake.frames_pulled(), 0);
        assert_eq!(fake.open_limits()[0].max_body_bytes(), 1);
    }

    #[test]
    fn error_cap_is_narrowed_before_the_first_frame() {
        let fake =
            FakeStreamingTransport::new().respond(500, &[], vec![FakeChunk::Data(vec![0; 65537])]);
        let result = collect_response(
            &fake,
            &request(),
            Operation::ApiJson,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(result.status, 500);
        assert!(result.body.is_empty());
        assert_eq!(fake.narrow_calls(), vec![65536]);
    }

    #[test]
    fn all_zero_read_statuses_ignore_lengths_and_frames() {
        for (op, status) in [
            (Operation::Put, 403),
            (Operation::Delete, 200),
            (Operation::Delete, 204),
            (Operation::Download, 307),
            (Operation::Download, 403),
            (Operation::Oidc, 307),
        ] {
            let fake = FakeStreamingTransport::new().respond(
                status,
                &[("content-length", "bad")],
                vec![FakeChunk::Data(vec![0; 2])],
            );
            assert_eq!(
                collect_response(
                    &fake,
                    &request(),
                    op,
                    Instant::now() + Duration::from_secs(30)
                )
                .unwrap()
                .status,
                status
            );
            assert_eq!(fake.frames_pulled(), 0);
        }
    }

    #[test]
    fn length_and_limit_fail_before_further_reads() {
        for length in ["bogus", "1,1", "65537"] {
            let fake = FakeStreamingTransport::new().respond(
                200,
                &[("content-length", length)],
                vec![FakeChunk::Data(vec![0])],
            );
            assert!(collect_response(
                &fake,
                &request(),
                Operation::Oidc,
                Instant::now() + Duration::from_secs(30)
            )
            .is_err());
            assert_eq!(fake.frames_pulled(), 0);
        }
        let fake =
            FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::Data(vec![0; 65536])]);
        assert_eq!(
            collect_response(
                &fake,
                &request(),
                Operation::Oidc,
                Instant::now() + Duration::from_secs(30)
            )
            .unwrap()
            .body
            .len(),
            65536
        );
        let fake =
            FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::Data(vec![0; 65537])]);
        assert_eq!(
            collect_response(
                &fake,
                &request(),
                Operation::Oidc,
                Instant::now() + Duration::from_secs(30)
            )
            .unwrap_err(),
            BodyError::TooLarge
        );
    }

    #[test]
    fn retry_stops_before_deadline_and_deadline_failure_is_terminal() {
        use super::{run, SystemClock};
        use toolkit_aws::retry::{NoJitter, RetryPolicy};
        use toolkit_aws::testing::RecordingSleeper;
        let fake = FakeStreamingTransport::new()
            .respond(503, &[], vec![])
            .respond(200, &[], vec![]);
        let sleeper = RecordingSleeper::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        let r = run(
            &fake,
            &request(),
            Operation::Oidc,
            deadline,
            Duration::from_secs(30),
            false,
            &SystemClock,
            &sleeper,
            &NoJitter,
            &RetryPolicy::default(),
        )
        .unwrap();
        assert_eq!(r.status, 503);
        assert!(sleeper.sleeps().is_empty());
        assert_eq!(fake.requests().len(), 1);
        assert_eq!(fake.open_limits()[0].deadline(), Some(deadline));
        let stalled = FakeStreamingTransport::new()
            .respond(200, &[], vec![FakeChunk::Stall])
            .respond(200, &[], vec![]);
        assert_eq!(
            run(
                &stalled,
                &request(),
                Operation::Oidc,
                deadline,
                Duration::from_secs(30),
                false,
                &SystemClock,
                &sleeper,
                &NoJitter,
                &RetryPolicy::default()
            )
            .unwrap_err(),
            BodyError::Deadline
        );
        assert_eq!(stalled.requests().len(), 1);
    }
}
