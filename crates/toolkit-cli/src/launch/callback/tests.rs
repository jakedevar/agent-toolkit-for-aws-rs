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
    assert!(headers.contains(&format!("Content-Length: {}", body.len())));
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

#[test]
fn loopback_ignores_bad_state_and_closes_listener_on_success() {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::time::{Duration, Instant};
    let listener = super::listener::bind().unwrap();
    let address = listener.local_addr().unwrap();
    assert!(
        matches!(address, SocketAddr::V4(v4) if *v4.ip() == Ipv4Addr::LOCALHOST && v4.port() != 0)
    );
    let thread = std::thread::spawn(move || {
        super::listener::serve(listener, "secret", Instant::now() + Duration::from_secs(5)).unwrap()
    });
    let send = |query: &str| {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(
            stream,
            "GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            address.port()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    };
    assert!(send("state=wrong&code=x").starts_with("HTTP/1.1 400"));
    assert!(send("state=secret&code=abc").starts_with("HTTP/1.1 200"));
    assert_eq!(thread.join().unwrap(), Some(Outcome::Code("abc".into())));
    assert!(TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err());
}

#[test]
fn session_publication_requires_admission_before_cutoff() {
    use super::gate::{publish_session, CommitGate};
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    use std::time::{Duration, Instant};
    let scratch = Scratch::new("callback-gate");
    let gate = CommitGate::default();
    assert!(gate.start_exchange());
    assert!(publish_session(
        &gate,
        Instant::now(),
        scratch.path(),
        b"secret",
        &SeededRandom::new(1)
    )
    .is_err());
    assert!(!scratch.join("session.json").exists());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    publish_session(
        &gate,
        Instant::now() + Duration::from_secs(2),
        scratch.path(),
        b"secret",
        &SeededRandom::new(2),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(scratch.join("session.json")).unwrap(),
        b"secret"
    );
}

#[cfg(unix)]
#[test]
fn pending_custody_keeps_private_record_and_exclusive_lock() {
    use super::{pending::Custody, PendingStatus};
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("callback-pending");
    let custody = Custody::acquire(scratch.path(), 12345).unwrap();
    assert!(Custody::acquire(scratch.path(), 12345).is_err());
    custody
        .write(
            1234,
            100,
            160,
            PendingStatus::Listening,
            None,
            &SeededRandom::new(1),
        )
        .unwrap();
    let path = scratch.join("pending/12345.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(record["status"], "listening");
    assert_eq!(record["cutoff"], 160);
    drop(custody);
    assert!(Custody::acquire(scratch.path(), 12345).is_ok());
}

#[cfg(unix)]
#[test]
fn pending_reader_validates_schema_and_lock_custody() {
    use super::{
        pending::{acquire_if_free, read_record, Custody},
        PendingStatus,
    };
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    let scratch = Scratch::new("pending-read");
    let custody = Custody::acquire(scratch.path(), 55).unwrap();
    custody
        .write(
            1234,
            100,
            160,
            PendingStatus::Failed,
            Some("invalid_code"),
            &SeededRandom::new(1),
        )
        .unwrap();
    assert_eq!(
        read_record(scratch.path(), 55).unwrap().reason.as_deref(),
        Some("invalid_code")
    );
    assert!(acquire_if_free(scratch.path(), 55).is_none());
    drop(custody);
    assert!(acquire_if_free(scratch.path(), 55).is_some());
    std::fs::write(scratch.join("pending/55.json"), b"{\"pid\":55,\"pid\":55}").unwrap();
    assert!(read_record(scratch.path(), 55).is_none());
}
