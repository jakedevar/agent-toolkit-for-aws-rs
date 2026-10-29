//! Offline tests for the internal sign-in helper.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::io::Write;
#[cfg(all(unix, not(feature = "aws-sdk")))]
use std::io::{BufRead, BufReader, Read};
#[cfg(all(unix, not(feature = "aws-sdk")))]
use std::net::TcpStream;
use std::process::{Command, Stdio};
#[cfg(all(unix, not(feature = "aws-sdk")))]
use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

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
#[allow(clippy::too_many_lines)]
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
    let lock_path = home.join(format!(".launch-with-aws/pending/{pid}.lock"));
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    assert!(lock.try_lock().is_err());
    let record_path = home.join(format!(".launch-with-aws/pending/{pid}.json"));
    let before = std::fs::read(&record_path).unwrap();
    #[cfg(target_os = "linux")]
    {
        let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap();
        assert!(children.trim().is_empty());
        let descriptors = std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap();
        let mut found_lock = false;
        for entry in descriptors.flatten() {
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            if target == home.join(format!(".launch-with-aws/pending/{pid}.lock")) {
                let info = std::fs::read_to_string(format!(
                    "/proc/{pid}/fdinfo/{}",
                    entry.file_name().to_string_lossy()
                ))
                .unwrap();
                let flags = info
                    .lines()
                    .find_map(|line| line.strip_prefix("flags:\t"))
                    .unwrap();
                let flags = u32::from_str_radix(flags, 8).unwrap();
                assert_ne!(flags & 0o2_000_000, 0);
                found_lock = true;
            }
        }
        assert!(found_lock);
    }
    {
        let closed = TcpStream::connect(("127.0.0.1", port)).unwrap();
        drop(closed);
        let mut malformed = TcpStream::connect(("127.0.0.1", port)).unwrap();
        malformed
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        malformed.write_all(b"INVALID\r\n\r\n").unwrap();
        let mut response = String::new();
        malformed.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
        assert_eq!(std::fs::read(&record_path).unwrap(), before);
    }
    for query in ["state=wrong&code=abc", "state=wrong&error=denied"] {
        let mut bad = TcpStream::connect(("127.0.0.1", port)).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(
            bad,
            "GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        let mut ignored = String::new();
        bad.read_to_string(&mut ignored).unwrap();
        assert!(ignored.starts_with("HTTP/1.1 400"));
        assert_eq!(std::fs::read(&record_path).unwrap(), before);
    }
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
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{waitid, WaitId, WaitIdOptions};
        let child_pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        assert!(waitid(
            WaitId::Pid(child_pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT
        )
        .unwrap()
        .is_some());
        assert!(lock.try_lock().is_ok());
        lock.unlock().unwrap();
    }
    assert!(!child.wait().unwrap().success());
    let record =
        std::fs::read_to_string(home.join(format!(".launch-with-aws/pending/{pid}.json"))).unwrap();
    assert!(record.contains("transport_unavailable"));
    assert!(!record.contains("secret"));
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[test]
fn auth_wait_reports_unknown_pid_without_signalling() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("l4-wait-{}-{unique}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-wait", "999999"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains("No sign-in is in progress for pid 999999"));
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(all(unix, not(feature = "aws-sdk")))]
#[test]
fn auth_start_spawns_helper_without_secrets_in_argv() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("l4-start-{}-{unique}", std::process::id()));
    let state_dir = home.join(".launch-with-aws");
    std::fs::create_dir_all(&state_dir).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let session = serde_json::json!({"client_id":"example","client_secret":"private-client-secret","client_expires_at":now+3600,"authorize_endpoint":"https://oidc.us-east-1.amazonaws.com/authorize","token_endpoint":"https://oidc.us-east-1.amazonaws.com/token","scopes":["launch:access"],"access_token":"old","refresh_token":"old-refresh","token_expires_at":now-1});
    std::fs::write(state_dir.join("session.json"), session.to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-start"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let pid = response["pid"].as_u64().unwrap();
    let port = u16::try_from(response["port"].as_u64().unwrap()).unwrap();
    let url = response["signInUrl"].as_str().unwrap();
    assert!(!url.contains("private-client-secret"));
    #[cfg(target_os = "linux")]
    {
        let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        assert!(!String::from_utf8_lossy(&argv).contains("private-client-secret"));
    }
    let state = url
        .split('&')
        .find_map(|v| v.strip_prefix("state="))
        .unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(
        stream,
        "GET /?state={state}&code=abc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let wait = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-wait", &pid.to_string()])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(!wait.status.success());
    assert!(String::from_utf8_lossy(&wait.stderr).contains("transport is unavailable"));
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(all(unix, not(feature = "aws-sdk")))]
#[test]
fn helper_retains_valid_oauth_error_without_description() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("l4-denied-{}-{unique}", std::process::id()));
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
        .stderr(Stdio::null())
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
    write!(stream, "GET /?state=secret&error=access_denied&error_description=private_description HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(!child.wait().unwrap().success());
    let record =
        std::fs::read_to_string(home.join(format!(".launch-with-aws/pending/{pid}.json"))).unwrap();
    assert!(record.contains("access_denied"), "{record}");
    assert!(!record.contains("private_description"));
    assert!(!response.contains("private_description"));
    let wait = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-wait", &pid.to_string()])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(!wait.status.success());
    let message = String::from_utf8(wait.stderr).unwrap();
    assert!(message.contains("Sign-in was denied. Run auth-start again."));
    assert!(!message.contains("private_description"));
    assert!(!home
        .join(format!(".launch-with-aws/pending/{pid}.json"))
        .exists());
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(all(unix, not(feature = "aws-sdk")))]
#[test]
fn helper_closes_stdout_after_port_while_still_listening() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("l4-stdout-{}-{unique}", std::process::id()));
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
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
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
    assert_ne!(port, 0);
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut rest = Vec::new();
        let _ = send.send(stdout.read_to_end(&mut rest).map(|_| rest));
    });
    let result = receive.recv_timeout(Duration::from_secs(2));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(home).unwrap();
    assert!(still_running);
    assert!(result.expect("stdout remained open").unwrap().is_empty());
}
