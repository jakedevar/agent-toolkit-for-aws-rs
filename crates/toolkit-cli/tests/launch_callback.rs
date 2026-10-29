//! Offline tests for the internal sign-in helper.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn invalid_stdin_fails_silently_before_binding() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-callback"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(all(unix, not(feature = "aws-sdk")))]
#[test]
fn helper_announces_loopback_and_retains_transport_failure() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("l4-helper-{}-{unique}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 10;
    let mut child = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-callback"])
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let input = format!(
        r#"{{"client_id":"c","client_secret":"s","client_expires_at":1,"scopes":["launch:access"],"code_verifier":"v","state":"secret","deadline":{deadline}}}"#
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(
        stream,
        "GET /?state=secret&code=abc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!child.wait().unwrap().success());
    let record =
        std::fs::read_to_string(home.join(format!(".launch-with-aws/pending/{pid}.json"))).unwrap();
    assert!(record.contains("transport_unavailable"));
    assert!(!record.contains("secret"));
    std::fs::remove_dir_all(home).unwrap();
}
