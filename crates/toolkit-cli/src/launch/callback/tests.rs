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
fn state_comparison_visits_all_equal_length_bytes_after_mismatch() {
    for actual in ["x2345678", "1234567x", "12345678"] {
        let mut visits = 0;
        let equal = super::state_matches_counted(actual, "12345678", || visits += 1);
        assert_eq!(visits, 8);
        assert_eq!(equal, actual == "12345678");
    }
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
fn callback_wait_decision_table_for_all_readable_statuses() {
    use super::{wait_decision, PendingStatus, WaitDecision};
    for status in [
        PendingStatus::Listening,
        PendingStatus::Exchanging,
        PendingStatus::Succeeded,
        PendingStatus::Failed,
        PendingStatus::TimedOut,
    ] {
        for held in [false, true] {
            for valid in [false, true] {
                for now in [99, 100, 104, 105, 3600] {
                    let expected = if valid {
                        WaitDecision::Authenticated
                    } else if held && now < 105 {
                        WaitDecision::Poll
                    } else if held {
                        WaitDecision::HelperBlocked
                    } else {
                        match status {
                            PendingStatus::Failed => WaitDecision::Failed,
                            PendingStatus::TimedOut => WaitDecision::TimedOut,
                            _ if now >= 100 => WaitDecision::TimedOut,
                            _ => WaitDecision::Ended,
                        }
                    };
                    assert_eq!(
                        wait_decision(status, held, valid, now, 100),
                        expected,
                        "{status:?}, held={held}, valid={valid}, now={now}"
                    );
                }
            }
        }
    }
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

#[cfg(unix)]
#[test]
fn wait_observes_held_and_free_lock_without_signalling_pid() {
    use super::{
        pending::{observe, Custody, Observation},
        PendingStatus, WaitDecision,
    };
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    let scratch = Scratch::new("pending-wait");
    assert_eq!(observe(scratch.path(), 66, 200), Observation::Missing);
    let custody = Custody::acquire(scratch.path(), 66).unwrap();
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
        observe(scratch.path(), 66, 164),
        Observation::Decision(WaitDecision::Poll)
    );
    assert_eq!(
        observe(scratch.path(), 66, 165),
        Observation::Decision(WaitDecision::HelperBlocked)
    );
    drop(custody);
    assert_eq!(
        observe(scratch.path(), 66, 165),
        Observation::Decision(WaitDecision::Failed)
    );
}

#[test]
fn child_configuration_is_strict_and_bounded() {
    use super::spawn::HelperConfig;
    let valid = br#"{"client_id":"c","client_secret":"s","client_expires_at":1,"scopes":["launch:access"],"code_verifier":"v","state":"n","deadline":10}"#;
    let config = HelperConfig::read(&valid[..]).unwrap();
    assert_eq!(config.client_id, "c");
    let duplicate = br#"{"client_id":"c","client_id":"x","client_secret":"s","client_expires_at":1,"scopes":[],"code_verifier":"v","state":"n","deadline":10}"#;
    assert!(HelperConfig::read(&duplicate[..]).is_err());
    assert!(HelperConfig::read(&vec![b' '; 65537][..]).is_err());
    assert!(HelperConfig::read(&b"{}"[..]).is_err());
}

#[cfg(unix)]
#[test]
fn stale_cleanup_preserves_held_records_and_unrelated_files() {
    use super::{
        pending::{cleanup_stale, Custody},
        PendingStatus,
    };
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    let scratch = Scratch::new("pending-cleanup");
    let held = Custody::acquire(scratch.path(), 80).unwrap();
    held.write(
        1234,
        100,
        160,
        PendingStatus::Listening,
        None,
        &SeededRandom::new(1),
    )
    .unwrap();
    let free = Custody::acquire(scratch.path(), 81).unwrap();
    free.write(
        1234,
        100,
        160,
        PendingStatus::TimedOut,
        None,
        &SeededRandom::new(2),
    )
    .unwrap();
    drop(free);
    let temp = scratch.write("session.json.0123456789abcdef.tmp", b"x");
    let file = std::fs::OpenOptions::new().write(true).open(&temp).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    scratch.write("session.json.not-owned.tmp", b"x");
    cleanup_stale(scratch.path(), 4000);
    assert!(scratch.join("pending/80.json").exists());
    assert!(!scratch.join("pending/81.json").exists());
    assert!(!temp.exists());
    assert!(scratch.join("session.json.not-owned.tmp").exists());
}
#[cfg(unix)]
#[test]
fn watchdog_terminates_prepublication_process() {
    use super::{
        gate::{start_watchdog, CommitGate},
        pending::Custody,
    };
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::Instant;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    if let Ok(dir) = std::env::var("RSI_CALLBACK_WATCHDOG_CHILD") {
        let dir = std::path::PathBuf::from(dir);
        let _custody = Custody::acquire(&dir, 99).unwrap();
        let gate = Arc::new(CommitGate::default());
        assert!(gate.start_exchange());
        assert!(gate.admit(Instant::now(), Instant::now() + Duration::from_secs(1)));
        start_watchdog(gate, Instant::now() + Duration::from_millis(100), || {
            panic!("unexpected record write")
        });
        println!("watchdog-ready");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
        std::fs::write(dir.join("published"), b"bad").unwrap();
        return;
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("l4-watchdog-{}-{unique}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "launch::callback::tests::watchdog_terminates_prepublication_process",
            "--nocapture",
        ])
        .env("RSI_CALLBACK_WATCHDOG_CHILD", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(stdout.read_line(&mut line).unwrap(), 0);
        if line.contains("watchdog-ready") {
            break;
        }
    }
    let (send, recv) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = send.send(child.wait());
    });
    let result = recv.recv_timeout(Duration::from_secs(5));
    drop(stdin);
    assert!(!result.unwrap().unwrap().success());
    assert!(!dir.join("published").exists());
    assert!(Custody::acquire(&dir, 99).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn request_boundary_rejects_wrong_state_and_bad_heads_without_terminal_result() {
    for query in [
        "code=x",
        "state=wrong&code=x",
        "error=access_denied",
        "state=wrong&error=access_denied",
        "state=secret&state=secret&code=x",
        "state=secret&error=a&error=b",
        "state=secret&code=%GG",
    ] {
        assert_eq!(
            parse_request(&request(query), 1234, "secret"),
            Outcome::Ignore(400),
            "{query}"
        );
    }
    for head in [
        "GET /?state=secret&code=x HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nHost: 127.0.0.1:1234\r\n\r\n",
        "GET  / HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n",
        "GET / HTTP/1.1\r\nBad Header: x\r\nHost: 127.0.0.1:1234\r\n\r\n",
    ] { assert_eq!(parse_request(head.as_bytes(), 1234, "secret"), Outcome::Ignore(400)); }
    assert_eq!(
        parse_request(&request("state=secret&code="), 1234, "secret"),
        Outcome::InvalidCode
    );
    assert_eq!(
        parse_request(
            &request(&format!("state=secret&code={}", "a".repeat(4097))),
            1234,
            "secret"
        ),
        Outcome::InvalidCode
    );
}

#[cfg(unix)]
#[test]
fn unreadable_record_with_free_lock_is_not_missing() {
    use super::{
        pending::{finish_if_final, Custody, Observation},
        WaitDecision,
    };
    use crate::launch::tests_support::Scratch;
    let scratch = Scratch::new("unreadable-wait");
    let custody = Custody::acquire(scratch.path(), 91).unwrap();
    let path = scratch.join("pending/91.json");
    std::fs::write(&path, b"not json").unwrap();
    drop(custody);
    let (observation, _) = finish_if_final(scratch.path(), 91, 100);
    assert_eq!(observation, Observation::Decision(WaitDecision::Ended));
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn terminal_record_remains_until_result_is_reported() {
    use super::{
        pending::{Custody, WaitTracker},
        PendingStatus,
    };
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    let scratch = Scratch::new("wait-report-order");
    let custody = Custody::acquire(scratch.path(), 91).unwrap();
    custody
        .write(
            1234,
            40,
            100,
            PendingStatus::Failed,
            Some("invalid_code"),
            &SeededRandom::new(1),
        )
        .unwrap();
    drop(custody);
    let path = scratch.join("pending/91.json");
    WaitTracker::default().finish_after_report(scratch.path(), 91, 99, |_, _| {
        assert!(path.exists());
        assert!(super::pending::acquire_if_free(scratch.path(), 91).is_none());
    });
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn revision4_unknown_record_decision_matrix() {
    use super::{
        pending::{Custody, Observation, WaitTracker},
        PendingStatus, WaitDecision,
    };
    use crate::launch::{clock::SeededRandom, tests_support::Scratch};
    for disappeared in [false, true] {
        for held in [false, true] {
            for valid in [false, true] {
                for (now, elapsed) in [(99, 659), (100, 660), (104, 664), (105, 665)] {
                    let scratch = Scratch::new("unknown-matrix");
                    let custody = Custody::acquire(scratch.path(), 91).unwrap();
                    let path = scratch.join("pending/91.json");
                    let mut tracker = WaitTracker::default();
                    if disappeared {
                        custody
                            .write(
                                1234,
                                40,
                                100,
                                PendingStatus::Listening,
                                None,
                                &SeededRandom::new(1),
                            )
                            .unwrap();
                        assert_eq!(
                            tracker.finish_at(scratch.path(), 91, 99, 0).0,
                            Observation::Decision(WaitDecision::Poll)
                        );
                        std::fs::remove_file(&path).unwrap();
                    } else {
                        std::fs::write(&path, b"malformed").unwrap();
                    }
                    if valid {
                        let session = serde_json::json!({"client_id":"c","client_secret":"s","client_expires_at":9999,"authorize_endpoint":"https://example.com","token_endpoint":"https://example.com","scopes":["launch:access"],"access_token":"a","refresh_token":"r","token_expires_at":9999});
                        std::fs::write(scratch.join("session.json"), session.to_string()).unwrap();
                    }
                    if !held {
                        drop(custody);
                    }
                    let expected = if valid {
                        WaitDecision::Authenticated
                    } else if held && now < 105 {
                        WaitDecision::Poll
                    } else if held {
                        WaitDecision::HelperBlocked
                    } else if now >= 100 {
                        WaitDecision::TimedOut
                    } else {
                        WaitDecision::Ended
                    };
                    assert_eq!(
                        tracker.finish_at(scratch.path(), 91, now, elapsed).0,
                        Observation::Decision(expected),
                        "disappeared={disappeared} held={held} valid={valid} now={now}"
                    );
                    if !disappeared {
                        assert_eq!(path.exists(), held);
                    }
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn revision4_first_missing_record_wins_over_valid_session() {
    use super::pending::{Observation, WaitTracker};
    use crate::launch::tests_support::Scratch;
    let scratch = Scratch::new("first-missing");
    let session = serde_json::json!({"client_id":"c","client_secret":"s","client_expires_at":9999,"authorize_endpoint":"https://example.com","token_endpoint":"https://example.com","scopes":["launch:access"],"access_token":"a","refresh_token":"r","token_expires_at":9999});
    std::fs::write(scratch.join("session.json"), session.to_string()).unwrap();
    assert_eq!(
        WaitTracker::default()
            .finish_at(scratch.path(), 91, 100, 700)
            .0,
        Observation::Missing
    );
}

#[cfg(unix)]
#[test]
fn revision4_removes_unreadable_symlink_without_touching_target() {
    use super::{
        pending::{Custody, Observation, WaitTracker},
        WaitDecision,
    };
    use crate::launch::tests_support::Scratch;
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("unknown-symlink");
    let custody = Custody::acquire(scratch.path(), 91).unwrap();
    let target = scratch.join("unrelated");
    std::fs::write(&target, b"keep").unwrap();
    let link = scratch.join("pending/91.json");
    symlink(&target, &link).unwrap();
    drop(custody);
    assert_eq!(
        WaitTracker::default()
            .finish_at(scratch.path(), 91, 100, 660)
            .0,
        Observation::Decision(WaitDecision::TimedOut)
    );
    assert!(!link.exists());
    assert_eq!(std::fs::read(target).unwrap(), b"keep");
}
