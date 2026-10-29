use super::{parse_request, Outcome};

fn request(query: &str) -> Vec<u8> {
    format!("GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n").into_bytes()
}

#[test]
fn callback_validates_state_and_never_reflects_error_description() {
    assert_eq!(
        parse_request(&request("state=wrong&code=abc"), 1234, "secret"),
        Outcome::Ignore(400)
    );
    assert_eq!(
        parse_request(&request("state=secret&code=abc"), 1234, "secret"),
        Outcome::Code("abc".into())
    );
    assert_eq!(
        parse_request(
            &request("state=secret&error=access_denied&error_description=private"),
            1234,
            "secret"
        ),
        Outcome::Denied("access_denied".into())
    );
}

#[test]
fn callback_rejects_duplicate_parameters_and_invalid_code() {
    assert_eq!(
        parse_request(&request("state=secret&code=a&code=b"), 1234, "secret"),
        Outcome::Ignore(400)
    );
    assert_eq!(
        parse_request(&request("state=secret&code=%0A"), 1234, "secret"),
        Outcome::InvalidCode
    );
}

#[test]
fn request_head_rules() {
    for head in [
        "GET /?state=secret&code=x HTTP/1.1\r\n\r\n",
        "GET /?state=secret&code=x HTTP/1.1\r\nHost: evil\r\n\r\n",
        "GET /?state=secret&code=x HTTP/2\r\nHost: 127.0.0.1:1234\r\n\r\n",
    ] {
        assert_eq!(
            parse_request(head.as_bytes(), 1234, "secret"),
            Outcome::Ignore(400)
        );
    }
    assert_eq!(
        parse_request(
            b"POST / HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n",
            1234,
            "secret"
        ),
        Outcome::Ignore(405)
    );
    assert_eq!(
        parse_request(
            b"GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n",
            1234,
            "secret"
        ),
        Outcome::Ignore(404)
    );
    assert_eq!(
        parse_request(&vec![b'x'; 8193], 1234, "secret"),
        Outcome::Ignore(400)
    );
    assert_eq!(
        parse_request(&request("state=secret&code=%FF"), 1234, "secret"),
        Outcome::Ignore(400)
    );
    assert_eq!(
        parse_request(&request("state=secret&code=a%2Bb"), 1234, "secret"),
        Outcome::Code("a+b".into())
    );
    assert_eq!(
        parse_request(&request("state=secret&error=%22"), 1234, "secret"),
        Outcome::Denied("invalid_error_code".into())
    );
}

#[test]
fn response_has_fixed_security_headers_and_exact_length() {
    let bytes = super::response(400);
    let response = std::str::from_utf8(&bytes).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(headers.contains("Cache-Control: no-store\r\n"));
    assert!(headers.contains("Content-Security-Policy: default-src 'none'\r\n"));
    assert!(headers.contains(&format!("Content-Length: {}\r\n", body.len())));
}

#[test]
fn wait_result_requires_free_lock_for_final_failures() {
    use super::{wait_decision, PendingStatus, WaitDecision};
    for status in [
        PendingStatus::Listening,
        PendingStatus::Exchanging,
        PendingStatus::Succeeded,
        PendingStatus::Failed,
        PendingStatus::TimedOut,
    ] {
        assert_eq!(
            wait_decision(status, true, false, 104, 100),
            WaitDecision::Poll
        );
        assert_eq!(
            wait_decision(status, true, false, 105, 100),
            WaitDecision::HelperBlocked
        );
        assert_eq!(
            wait_decision(status, true, true, 105, 100),
            WaitDecision::Authenticated
        );
    }
    assert_eq!(
        wait_decision(PendingStatus::Failed, false, false, 0, 100),
        WaitDecision::Failed
    );
    assert_eq!(
        wait_decision(PendingStatus::Listening, false, false, 100, 100),
        WaitDecision::TimedOut
    );
    assert_eq!(
        wait_decision(PendingStatus::Listening, false, false, 99, 100),
        WaitDecision::Ended
    );
}
