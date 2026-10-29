//! Streaming seam: limits, chunking, the body cap and the fake transport
//! (T-SIZE transport rows that need no socket).

use super::*;
use crate::testing::{FakeChunk, FakeStreamingTransport};
use crate::transport::{Headers, HttpRequest, Method, Timeouts};

fn request() -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: "https://aws-mcp.us-east-1.api.aws/mcp".to_string(),
        headers: Headers::new(),
        body: b"{}".to_vec(),
        timeouts: Timeouts::default(),
    }
}

fn limits(body: u64, chunk: usize) -> StreamLimits {
    StreamLimits::new(body, chunk).expect("limits")
}

fn drain(stream: &mut dyn ResponseStream) -> (Vec<Vec<u8>>, Option<StreamError>) {
    let mut chunks = Vec::new();
    loop {
        match stream.next_chunk() {
            Ok(Some(chunk)) => chunks.push(chunk),
            Ok(None) => return (chunks, None),
            Err(error) => return (chunks, Some(error)),
        }
    }
}

#[test]
fn limits_must_be_positive() {
    assert!(StreamLimits::new(0, 1).is_err());
    assert!(StreamLimits::new(1, 0).is_err());
    let ok = limits(10, 3);
    assert_eq!((ok.max_body_bytes(), ok.max_chunk_bytes()), (10, 3));
}

#[test]
fn a_body_at_the_cap_is_delivered_and_one_byte_more_is_refused() {
    let transport = FakeStreamingTransport::new()
        .respond(
            200,
            &[],
            vec![FakeChunk::data(b"abcd"), FakeChunk::data(b"ef")],
        )
        .respond(
            200,
            &[],
            vec![FakeChunk::data(b"abcd"), FakeChunk::data(b"efg")],
        );
    let mut at_cap = transport.open(&request(), &limits(6, 64)).expect("open");
    let (chunks, error) = drain(at_cap.as_mut());
    assert_eq!(chunks.concat(), b"abcdef");
    assert_eq!(error, None);

    let mut over = transport.open(&request(), &limits(6, 64)).expect("open");
    let (chunks, error) = drain(over.as_mut());
    // Nothing past the cap is handed over, and the stream stays refused.
    assert_eq!(chunks.concat(), b"abcd");
    assert_eq!(error, Some(StreamError::TooLarge));
    assert_eq!(over.next_chunk(), Err(StreamError::TooLarge));
}

#[test]
fn frames_are_split_to_the_chunk_bound() {
    let transport =
        FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::data(b"0123456789")]);
    let mut stream = transport.open(&request(), &limits(100, 4)).expect("open");
    let (chunks, error) = drain(stream.as_mut());
    assert_eq!(
        chunks,
        vec![b"0123".to_vec(), b"4567".to_vec(), b"89".to_vec()]
    );
    assert_eq!(error, None);
}

#[test]
fn nothing_is_read_ahead_of_the_caller() {
    // R-MEM hook: the stream pulls the next frame only when asked, so a
    // caller that waits for budget before asking stops reading the socket.
    let transport = FakeStreamingTransport::new().respond(
        200,
        &[],
        vec![
            FakeChunk::data(b"one"),
            FakeChunk::data(b"two"),
            FakeChunk::data(b"three"),
        ],
    );
    let mut stream = transport.open(&request(), &limits(100, 64)).expect("open");
    assert_eq!(transport.frames_pulled(), 0);
    assert_eq!(stream.next_chunk(), Ok(Some(b"one".to_vec())));
    assert_eq!(transport.frames_pulled(), 1);
    assert_eq!(stream.next_chunk(), Ok(Some(b"two".to_vec())));
    assert_eq!(transport.frames_pulled(), 2);
}

#[test]
fn status_headers_idle_timeout_and_failures_come_through() {
    let failure = AwsError::connect("connection reset");
    let transport = FakeStreamingTransport::new()
        .respond(
            202,
            &[
                ("Content-Type", "text/event-stream"),
                ("Mcp-Session-Id", "abc"),
            ],
            vec![FakeChunk::data(b"data: 1\n\n"), FakeChunk::IdleTimeout],
        )
        .respond(200, &[], vec![FakeChunk::Fail(failure.clone())])
        .open_error(AwsError::connect("refused"));
    let mut stream = transport.open(&request(), &limits(100, 64)).expect("open");
    assert_eq!(stream.status(), 202);
    assert_eq!(
        stream.headers().get("content-type"),
        Some("text/event-stream")
    );
    assert_eq!(stream.headers().get("mcp-session-id"), Some("abc"));
    let (chunks, error) = drain(stream.as_mut());
    assert_eq!(chunks.concat(), b"data: 1\n\n");
    assert_eq!(error, Some(StreamError::IdleTimeout));

    let mut failing = transport.open(&request(), &limits(100, 64)).expect("open");
    assert_eq!(failing.next_chunk(), Err(StreamError::Failed(failure)));
    assert_eq!(
        transport
            .open(&request(), &limits(100, 64))
            .err()
            .map(|e| e.code()),
        Some("aws_connect")
    );
    let seen = transport.requests();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].url, "https://aws-mcp.us-east-1.api.aws/mcp");
}

#[test]
fn the_fake_refuses_framing_headers_like_the_real_transport() {
    let transport = FakeStreamingTransport::new().respond(200, &[], vec![]);
    let mut bad = request();
    bad.headers.insert("Host", "evil.example").unwrap();
    assert_eq!(
        transport.open(&bad, &limits(1, 1)).err().map(|e| e.code()),
        Some("aws_invalid_config")
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn stream_errors_print_fixed_text() {
    assert_eq!(
        StreamError::TooLarge.to_string(),
        "the response body exceeded its size limit"
    );
    assert_eq!(
        StreamError::IdleTimeout.to_string(),
        "no response bytes arrived within the read timeout"
    );
    let wrapped = StreamError::Failed(AwsError::connect("password=FAKESECRETVALUE1"));
    assert!(!wrapped.to_string().contains("FAKESECRETVALUE1"));
    assert!(!format!("{wrapped:?}").contains("FAKESECRETVALUE1"));
}

#[test]
fn t_sdl_limits_narrow_and_fake_stall() {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(10);
    let bounded = limits(10, 4).with_deadline(deadline);
    assert_eq!(bounded.deadline(), Some(deadline));
    assert_eq!(limits(10, 4).deadline(), None);
    let transport = FakeStreamingTransport::new()
        .respond(
            200,
            &[],
            vec![FakeChunk::data(b"abc"), FakeChunk::data(b"def")],
        )
        .respond(200, &[], vec![FakeChunk::Stall])
        .respond(200, &[], vec![FakeChunk::Stall]);
    let mut stream = transport.open(&request(), &bounded).unwrap();
    assert_eq!(stream.next_chunk(), Ok(Some(b"abc".to_vec())));
    stream.narrow_body_limit(5);
    stream.narrow_body_limit(100);
    assert_eq!(stream.next_chunk(), Err(StreamError::TooLarge));
    assert_eq!(stream.next_chunk(), Err(StreamError::TooLarge));
    let mut stalled = transport.open(&request(), &bounded).unwrap();
    assert_eq!(stalled.next_chunk(), Err(StreamError::DeadlineExceeded));
    assert_eq!(stalled.next_chunk(), Err(StreamError::DeadlineExceeded));
    let mut idle = transport.open(&request(), &limits(10, 4)).unwrap();
    assert_eq!(idle.next_chunk(), Err(StreamError::IdleTimeout));
    assert_eq!(
        transport.open_limits(),
        vec![bounded, bounded, limits(10, 4)]
    );
    assert_eq!(transport.narrow_calls(), vec![5, 100]);

    let transport = FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::data(b"abc")]);
    let mut stream = transport.open(&request(), &limits(10, 4)).unwrap();
    assert_eq!(stream.next_chunk(), Ok(Some(b"abc".to_vec())));
    stream.narrow_body_limit(2);
    assert_eq!(stream.next_chunk(), Err(StreamError::TooLarge));
}
