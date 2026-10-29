//! Contract tests for the streaming response boundary, before implementation.

#[cfg(test)]
mod tests {
    use super::{collect_response, BodyError, Operation};
    use std::time::{Duration, Instant};
    use toolkit_aws::stream::StreamingTransport;
    use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport};
    use toolkit_aws::transport::{Headers, HttpRequest, Method, Timeouts};

    fn request() -> HttpRequest {
        HttpRequest { method: Method::Post, url: "https://oidc.us-east-1.amazonaws.com/token".into(), headers: Headers::new(), body: Vec::new(), timeouts: Timeouts::default() }
    }

    #[test]
    fn oidc_collects_with_deadline_and_rejects_misleading_length() {
        let fake = FakeStreamingTransport::new().respond(200, &[("content-length", "2")], vec![FakeChunk::Data(b"abc".to_vec())]);
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = collect_response(&fake, &request(), Operation::Oidc, deadline);
        assert_eq!(result.unwrap_err(), BodyError::InvalidResponse);
        assert_eq!(fake.open_limits()[0].deadline(), Some(deadline));
    }

    #[test]
    fn put_never_reads_even_with_absurd_content_length() {
        let fake = FakeStreamingTransport::new().respond(200, &[("content-length", "999999999999999999")], vec![FakeChunk::Data(vec![1])]);
        let result = collect_response(&fake, &request(), Operation::Put, Instant::now() + Duration::from_secs(30)).unwrap();
        assert_eq!(result.status, 200);
        assert!(result.body.is_empty());
        assert_eq!(fake.frames_pulled(), 0);
        assert_eq!(fake.open_limits()[0].max_body_bytes(), 1);
    }

    #[test]
    fn error_cap_is_narrowed_before_the_first_frame() {
        let fake = FakeStreamingTransport::new().respond(500, &[], vec![FakeChunk::Data(vec![0; 65537])]);
        let result = collect_response(&fake, &request(), Operation::ApiJson, Instant::now() + Duration::from_secs(30)).unwrap();
        assert_eq!(result.status, 500);
        assert!(result.body.is_empty());
        assert_eq!(fake.narrow_calls(), vec![65536]);
    }
}
