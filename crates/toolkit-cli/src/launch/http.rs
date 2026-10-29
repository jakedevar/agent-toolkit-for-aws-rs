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
    pub collector_capacity: usize,
    pub collector_peak: usize,
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
    collect_with_ceiling(transport, request, operation, deadline, operation.ceiling())
}
#[allow(clippy::too_many_lines)]
fn collect_with_ceiling(
    transport: &dyn StreamingTransport,
    request: &HttpRequest,
    operation: Operation,
    deadline: Instant,
    ceiling: usize,
) -> Result<Response, BodyError> {
    collect_observed(
        transport,
        request,
        operation,
        deadline,
        ceiling,
        &mut |_, _, _| {},
    )
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn collect_observed(
    transport: &dyn StreamingTransport,
    request: &HttpRequest,
    operation: Operation,
    deadline: Instant,
    ceiling: usize,
    observe: &mut dyn FnMut(usize, usize, usize),
) -> Result<Response, BodyError> {
    let limits = StreamLimits::new(
        ceiling as u64,
        if operation == Operation::Put {
            1
        } else {
            SMALL_CAP
        },
    )
    .expect("positive stream limits")
    .with_deadline(deadline);
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
            collector_capacity: 0,
            collector_peak: 0,
        });
    };
    let cap = cap.min(ceiling);
    if cap < ceiling {
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
                collector_capacity: 0,
                collector_peak: 0,
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
                    collector_capacity: buf.capacity(),
                    collector_peak: buf.peak(),
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
        observe(buf.capacity(), buf.peak(), chunk.capacity());
    }
    let collector_capacity = buf.capacity();
    let collector_peak = buf.peak();
    Ok(Response {
        status,
        body: buf.into_vec(),
        error_header,
        collector_capacity,
        collector_peak,
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
    fn production_limits_are_positive_for_every_operation() {
        let deadline = Instant::now() + Duration::from_secs(30);
        for (op, cap, chunk) in [
            (Operation::Oidc, super::SMALL_CAP, super::SMALL_CAP),
            (Operation::ApiJson, super::API_CAP, super::SMALL_CAP),
            (Operation::Delete, super::SMALL_CAP, super::SMALL_CAP),
            (Operation::Put, 1, 1),
            (Operation::Download, super::DOWNLOAD_CAP, super::SMALL_CAP),
        ] {
            let limits = super::limits_for(op, deadline);
            assert_eq!(limits.max_body_bytes(), cap as u64);
            assert_eq!(limits.max_chunk_bytes(), chunk);
            assert_eq!(limits.deadline(), Some(deadline));
        }
    }

    #[test]
    fn launch_sources_do_not_use_unbounded_transport_calls() {
        fn scan(path: &std::path::Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path);
                    continue;
                }
                if path.extension().and_then(|s| s.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for forbidden in [concat!("Api", "Client"), concat!("Transport", "::send")] {
                    assert!(
                        !source.contains(forbidden),
                        "{} names {forbidden}",
                        path.display()
                    );
                }
                for line in source.lines() {
                    if line.contains(concat!(".", "send(")) {
                        let channel_call = path.ends_with("archive/git.rs")
                            && line.trim()
                                == concat!(
                                    "let _ = sender",
                                    ".",
                                    "send(read_capped(&mut stdout, cap));"
                                )
                            || path.ends_with("archive/select/tests_select.rs")
                                && line.trim() == concat!("let _ = sender", ".", "send(f());");
                        assert!(channel_call, "{}: {line}", path.display());
                    }
                }
            }
        }
        scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/launch"));
        let _: fn(
            &dyn toolkit_aws::stream::StreamingTransport,
            &HttpRequest,
            Operation,
            Instant,
        ) -> Result<super::Response, BodyError> = collect_response;
    }

    #[test]
    fn oversized_error_length_keeps_header_and_never_pulls() {
        let fake = FakeStreamingTransport::new().respond(
            500,
            &[
                ("content-length", "65537"),
                ("x-amzn-errortype", "InvalidGrantException"),
            ],
            vec![FakeChunk::Data(vec![0])],
        );
        let result = collect_response(
            &fake,
            &request(),
            Operation::ApiJson,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(result.status, 500);
        assert!(result.body.is_empty());
        assert_eq!(
            super::super::oidc::error_code(result.error_header.as_deref(), &result.body),
            "InvalidGrantException"
        );
        assert_eq!(fake.narrow_calls(), vec![super::SMALL_CAP as u64]);
        assert_eq!(fake.frames_pulled(), 0);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn streams_are_dropped_on_success_and_all_collector_failures() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use toolkit_aws::{
            stream::{ResponseStream, StreamError, StreamLimits, StreamingTransport},
            AwsError,
        };
        struct Tracked<'a> {
            fake: &'a FakeStreamingTransport,
            dropped: &'a AtomicBool,
        }
        struct Stream<'a> {
            inner: Box<dyn ResponseStream + 'a>,
            dropped: &'a AtomicBool,
        }
        impl Drop for Stream<'_> {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }
        impl ResponseStream for Stream<'_> {
            fn status(&self) -> u16 {
                self.inner.status()
            }
            fn headers(&self) -> &Headers {
                self.inner.headers()
            }
            fn narrow_body_limit(&mut self, cap: u64) {
                self.inner.narrow_body_limit(cap);
            }
            fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
                self.inner.next_chunk()
            }
        }
        impl StreamingTransport for Tracked<'_> {
            fn open<'a>(
                &'a self,
                request: &HttpRequest,
                limits: &StreamLimits,
            ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
                Ok(Box::new(Stream {
                    inner: self.fake.open(request, limits)?,
                    dropped: self.dropped,
                }))
            }
        }
        for (status, headers, chunks, op, expected) in [
            (
                200,
                vec![],
                vec![FakeChunk::Data(b"ok".to_vec())],
                Operation::Oidc,
                None,
            ),
            (
                200,
                vec![("content-length", "bad")],
                vec![],
                Operation::Oidc,
                Some(BodyError::InvalidResponse),
            ),
            (
                200,
                vec![("content-length", "1,1")],
                vec![],
                Operation::Oidc,
                Some(BodyError::InvalidResponse),
            ),
            (
                200,
                vec![("content-length", "1")],
                vec![FakeChunk::Data(b"ab".to_vec())],
                Operation::Oidc,
                Some(BodyError::InvalidResponse),
            ),
            (
                200,
                vec![],
                vec![FakeChunk::Data(vec![0; super::SMALL_CAP + 1])],
                Operation::Oidc,
                Some(BodyError::TooLarge),
            ),
            (
                200,
                vec![],
                vec![FakeChunk::Stall],
                Operation::Oidc,
                Some(BodyError::Deadline),
            ),
            (
                200,
                vec![],
                vec![FakeChunk::IdleTimeout],
                Operation::Oidc,
                Some(BodyError::Transport { retryable: true }),
            ),
            (
                403,
                vec![("content-length", "bad")],
                vec![FakeChunk::Data(vec![0])],
                Operation::Put,
                None,
            ),
            (
                500,
                vec![("content-length", "65537")],
                vec![],
                Operation::ApiJson,
                None,
            ),
        ] {
            let fake = FakeStreamingTransport::new().respond(status, &headers, chunks);
            let dropped = AtomicBool::new(false);
            let tracked = Tracked {
                fake: &fake,
                dropped: &dropped,
            };
            let result = collect_response(
                &tracked,
                &request(),
                op,
                Instant::now() + Duration::from_secs(30),
            );
            assert_eq!(result.err(), expected);
            assert!(dropped.load(Ordering::SeqCst));
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

    #[test]
    fn non_power_of_two_cap_bounds_allocation_across_growth() {
        let chunks = (0..10).map(|_| FakeChunk::Data(vec![7; 1000])).collect();
        let fake = FakeStreamingTransport::new().respond(200, &[], chunks);
        let result = super::collect_with_ceiling(
            &fake,
            &request(),
            Operation::Download,
            Instant::now() + Duration::from_secs(30),
            10003,
        )
        .unwrap();
        assert_eq!(result.body.len(), 10000);
        assert!(result.collector_capacity <= 10003);
        assert!(result.collector_peak < 2 * 10003);
        assert!(result.collector_peak > result.collector_capacity);
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[("content-length", "10001")],
            vec![FakeChunk::Data(vec![0; 1000]); 11],
        );
        assert_eq!(
            super::collect_with_ceiling(
                &fake,
                &request(),
                Operation::Download,
                Instant::now() + Duration::from_secs(30),
                10003
            )
            .unwrap_err(),
            BodyError::TooLarge
        );
    }

    #[test]
    fn pending_frame_and_collector_growth_share_a_bounded_peak_and_drop() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        use toolkit_aws::{
            stream::{ChunkLimiter, ResponseStream, StreamError, StreamLimits, StreamingTransport},
            AwsError,
        };

        struct Generator {
            pending: Arc<AtomicUsize>,
            dropped: Arc<AtomicBool>,
            declared: bool,
        }
        struct Stream {
            limiter: ChunkLimiter,
            pending: Arc<AtomicUsize>,
            dropped: Arc<AtomicBool>,
            headers: Headers,
            frame: usize,
        }
        impl Drop for Stream {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
                self.pending.store(0, Ordering::SeqCst);
            }
        }
        impl ResponseStream for Stream {
            fn status(&self) -> u16 {
                200
            }
            fn headers(&self) -> &Headers {
                &self.headers
            }
            fn narrow_body_limit(&mut self, cap: u64) {
                self.limiter.narrow(cap);
            }
            fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
                self.limiter.next(|| {
                    let size = match self.frame {
                        0 => 70_000,
                        1 => 30_003,
                        _ => 1,
                    };
                    self.frame += 1;
                    self.pending.store(size, Ordering::SeqCst);
                    Ok(Some(vec![7; size]))
                })
            }
        }
        impl StreamingTransport for Generator {
            fn open<'a>(
                &'a self,
                _: &HttpRequest,
                limits: &StreamLimits,
            ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
                let mut headers = Headers::new();
                if self.declared {
                    headers.insert("content-length", "100003").unwrap();
                }
                Ok(Box::new(Stream {
                    limiter: ChunkLimiter::new(*limits),
                    pending: self.pending.clone(),
                    dropped: self.dropped.clone(),
                    headers,
                    frame: 0,
                }))
            }
        }
        for declared in [false, true] {
            let fake = Generator {
                pending: Arc::new(AtomicUsize::new(0)),
                dropped: Arc::new(AtomicBool::new(false)),
                declared,
            };
            let mut combined_peak = 0;
            let mut collector_peak = 0;
            let result = super::collect_observed(
                &fake,
                &request(),
                Operation::Download,
                Instant::now() + Duration::from_secs(30),
                100_003,
                &mut |capacity, peak, chunk| {
                    assert!(capacity <= 100_003);
                    collector_peak = collector_peak.max(peak);
                    combined_peak =
                        combined_peak.max(peak + fake.pending.load(Ordering::SeqCst) + chunk);
                },
            );
            assert_eq!(result.unwrap_err(), BodyError::TooLarge);
            assert!(collector_peak > 100_003);
            assert!(combined_peak < 2 * 100_003 + 512 * 1024 + 64 * 1024);
            assert!(fake.dropped.load(Ordering::SeqCst));
            assert_eq!(fake.pending.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn retry_backoff_advances_clock_and_clamps_the_next_attempt() {
        use super::{run, MonotonicClock};
        use std::cell::Cell;
        use toolkit_aws::retry::{Jitter, RetryPolicy, Sleeper};
        struct Time(Cell<Instant>);
        impl MonotonicClock for Time {
            fn now(&self) -> Instant {
                self.0.get()
            }
        }
        impl Sleeper for Time {
            fn sleep(&self, d: Duration) {
                self.0.set(self.0.get() + d);
            }
        }
        struct HalfSecond;
        impl Jitter for HalfSecond {
            fn pick(&self, _: u64) -> u64 {
                500
            }
        }
        let start = Instant::now();
        let clock = Time(Cell::new(start));
        let fake = FakeStreamingTransport::new()
            .respond(503, &[], vec![])
            .respond(200, &[], vec![FakeChunk::Data(b"{}".to_vec())]);
        let deadline = start + Duration::from_secs(1);
        let result = run(
            &fake,
            &request(),
            Operation::Oidc,
            deadline,
            Duration::from_secs(30),
            false,
            &clock,
            &clock,
            &HalfSecond,
            &RetryPolicy::default(),
        )
        .unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(clock.now(), start + Duration::from_millis(500));
        assert_eq!(
            fake.open_limits()
                .iter()
                .map(toolkit_aws::stream::StreamLimits::deadline)
                .collect::<Vec<_>>(),
            vec![Some(deadline), Some(deadline)]
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn endless_generated_body_is_bounded_and_stream_dropped() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        use toolkit_aws::{
            stream::{ChunkLimiter, ResponseStream, StreamError, StreamLimits, StreamingTransport},
            AwsError,
        };
        struct Generator {
            pulls: Arc<AtomicUsize>,
            dropped: Arc<AtomicBool>,
            status: u16,
        }
        struct Stream {
            pulls: Arc<AtomicUsize>,
            dropped: Arc<AtomicBool>,
            limiter: ChunkLimiter,
            headers: Headers,
            status: u16,
        }
        impl Drop for Stream {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }
        impl ResponseStream for Stream {
            fn status(&self) -> u16 {
                self.status
            }
            fn headers(&self) -> &Headers {
                &self.headers
            }
            fn narrow_body_limit(&mut self, cap: u64) {
                self.limiter.narrow(cap);
            }
            fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
                self.limiter.next(|| {
                    self.pulls.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(vec![1; 1000]))
                })
            }
        }
        impl StreamingTransport for Generator {
            fn open<'a>(
                &'a self,
                _: &HttpRequest,
                limits: &StreamLimits,
            ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
                let mut headers = Headers::new();
                headers
                    .insert("x-amzn-errortype", "InternalServerException")
                    .unwrap();
                Ok(Box::new(Stream {
                    pulls: self.pulls.clone(),
                    dropped: self.dropped.clone(),
                    limiter: ChunkLimiter::new(*limits),
                    headers,
                    status: self.status,
                }))
            }
        }
        let fake = Generator {
            pulls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
            status: 200,
        };
        assert_eq!(
            super::collect_with_ceiling(
                &fake,
                &request(),
                Operation::Download,
                Instant::now() + Duration::from_secs(30),
                10003
            )
            .unwrap_err(),
            BodyError::TooLarge
        );
        assert_eq!(fake.pulls.load(Ordering::SeqCst), 11);
        assert!(fake.dropped.load(Ordering::SeqCst));
        let oidc_stream = Generator {
            pulls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
            status: 200,
        };
        let sleeper = toolkit_aws::testing::RecordingSleeper::new();
        let oidc = super::super::oidc::Oidc {
            transport: &oidc_stream,
            clock: &super::SystemClock,
            sleeper: &sleeper,
            jitter: &toolkit_aws::retry::NoJitter,
            policy: toolkit_aws::retry::RetryPolicy::none(),
        };
        let client = super::super::oidc::Client {
            id: "id".into(),
            secret: toolkit_aws::Secret::new("FAKE-CLIENT-SECRET"),
            expires_at: 100,
            scopes: vec![],
        };
        let scratch = super::super::tests_support::Scratch::new("endless-oidc");
        assert_eq!(
            oidc.exchange(
                &client,
                &toolkit_aws::Secret::new("FAKE-CODE"),
                &toolkit_aws::Secret::new("FAKE-VERIFIER"),
                "http://127.0.0.1",
                0,
                Instant::now() + Duration::from_secs(30)
            )
            .unwrap_err(),
            super::super::oidc::OidcError::InvalidResponse
        );
        assert!(super::super::state::load_session(scratch.path()).is_none());
        assert_eq!(oidc_stream.pulls.load(Ordering::SeqCst), 66);
        assert!(oidc_stream.dropped.load(Ordering::SeqCst));
        let error = Generator {
            pulls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
            status: 500,
        };
        let response = collect_response(
            &error,
            &request(),
            Operation::ApiJson,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(response.status, 500);
        assert!(response.body.is_empty());
        assert_eq!(
            super::super::oidc::error_code(response.error_header.as_deref(), &response.body),
            "InternalServerException"
        );
        assert_eq!(error.pulls.load(Ordering::SeqCst), 66);
        assert!(error.dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn error_is_narrowed_on_first_attempt_and_success_uses_full_ceiling() {
        use super::{run, SystemClock};
        use toolkit_aws::retry::{NoJitter, RetryPolicy};
        use toolkit_aws::testing::RecordingSleeper;
        let fake = FakeStreamingTransport::new()
            .respond(
                500,
                &[("x-amzn-errortype", "InternalServerException")],
                vec![FakeChunk::Data(vec![0; 65537])],
            )
            .respond(200, &[], vec![FakeChunk::Data(vec![7; 1024 * 1024])]);
        let sleeper = RecordingSleeper::new();
        let result = run(
            &fake,
            &request(),
            Operation::ApiJson,
            Instant::now() + Duration::from_secs(30),
            Duration::from_secs(120),
            false,
            &SystemClock,
            &sleeper,
            &NoJitter,
            &RetryPolicy::default(),
        )
        .unwrap();
        assert_eq!(result.body.len(), 1024 * 1024);
        assert_eq!(fake.narrow_calls(), vec![65536]);
        assert_eq!(
            fake.open_limits()
                .iter()
                .map(toolkit_aws::stream::StreamLimits::max_body_bytes)
                .collect::<Vec<_>>(),
            vec![super::API_CAP as u64; 2]
        );
        assert_eq!(sleeper.sleeps(), vec![Duration::from_secs(2)]);
    }
}
