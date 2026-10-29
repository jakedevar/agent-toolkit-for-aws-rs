//! Offline tests for the internal sign-in helper.
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
        // A completed later request proves the earlier closed peer was serviced.
        let mut barrier = TcpStream::connect(("127.0.0.1", port)).unwrap();
        barrier
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            barrier,
            "GET /?state=wrong&code=x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        let mut ignored = String::new();
        barrier.read_to_string(&mut ignored).unwrap();
        assert!(ignored.starts_with("HTTP/1.1 400"));
        assert_eq!(
            std::fs::read(&record_path).unwrap(),
            before,
            "closed peer changed pending record"
        );
        let mut malformed = TcpStream::connect(("127.0.0.1", port)).unwrap();
        malformed
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        malformed.write_all(b"INVALID\r\n\r\n").unwrap();
        let mut response = String::new();
        malformed.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
        assert_eq!(
            std::fs::read(&record_path).unwrap(),
            before,
            "malformed peer changed pending record"
        );
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

// A private network and mount namespace make the fixed production hostname
// resolve only to this test's loopback TLS server, without changing host DNS.
#[cfg(all(target_os = "linux", feature = "aws-sdk"))]
#[test]
#[allow(clippy::too_many_lines)]
fn shipped_binary_exchange_publishes_session() {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::io::{BufRead, BufReader, Read};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    const CHILD: &str = "RSI_CALLBACK_NET5_NAMESPACE";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "--net"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "shipped_binary_exchange_publishes_session",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "namespace test failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(Command::new("mount")
        .args(["--make-rprivate", "/"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("ip")
        .args(["link", "set", "lo", "up"])
        .status()
        .unwrap()
        .success());
    // The production DNS guard rejects loopback addresses. This public-shaped
    // address exists only on the private namespace's loopback interface; the
    // namespace has no external interface or route.
    assert!(Command::new("ip")
        .args(["addr", "add", "1.1.1.1/32", "dev", "lo"])
        .status()
        .unwrap()
        .success());
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("net5-{}-{unique}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let hosts = home.join("hosts");
    std::fs::write(
        &hosts,
        b"127.0.0.1 localhost\n1.1.1.1 oidc.us-east-1.amazonaws.com\n",
    )
    .unwrap();
    assert!(Command::new("mount")
        .arg("--bind")
        .arg(&hosts)
        .arg("/etc/hosts")
        .status()
        .unwrap()
        .success());
    // Do not let host systemd-resolved or nscd bypass the private hosts file.
    let nss = home.join("nsswitch.conf");
    let original = std::fs::read_to_string("/etc/nsswitch.conf").unwrap();
    let isolated = original
        .lines()
        .map(|line| {
            if line.starts_with("hosts:") {
                "hosts: files"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&nss, isolated).unwrap();
    assert!(Command::new("mount")
        .arg("--bind")
        .arg(&nss)
        .arg("/etc/nsswitch.conf")
        .status()
        .unwrap()
        .success());
    use std::net::ToSocketAddrs;
    let resolved: Vec<_> = ("oidc.us-east-1.amazonaws.com", 443)
        .to_socket_addrs()
        .unwrap()
        .collect();
    assert!(resolved
        .iter()
        .all(|address| address.ip().to_string() == "1.1.1.1"));
    assert!(!resolved.is_empty());

    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let server_key = KeyPair::generate().unwrap();
    let server_params =
        CertificateParams::new(vec!["oidc.us-east-1.amazonaws.com".into()]).unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &Issuer::from_params(&ca_params, &ca_key))
        .unwrap();
    let ca_path = home.join("ca.pem");
    std::fs::write(&ca_path, ca.pem()).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![server_cert.der().clone(), ca.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
        )
        .unwrap();
    let listener = TcpListener::bind("1.1.1.1:443").unwrap();
    let (request_tx, request_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let server = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let mut reader = BufReader::new(&mut stream);
        let mut head = String::new();
        let mut length = None;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            head.push_str(&line);
        }
        let mut body = vec![0; length.unwrap()];
        reader.read_exact(&mut body).unwrap();
        std::sync::mpsc::SyncSender::send(&request_tx, (head, body)).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let body =
            br#"{"accessToken":"test-access","refreshToken":"test-refresh","expiresIn":3600}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();
    });
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 30;
    let mut child = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["launch", "auth-callback"])
        .env("HOME", &home)
        .env("SSL_CERT_FILE", &ca_path)
        .env_remove("SSL_CERT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let input = format!(
        r#"{{"client_id":"test-client","client_secret":"test-secret","client_expires_at":9999999999,"scopes":["launch:access"],"code_verifier":"test-verifier","state":"secret","deadline":{deadline}}}"#
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
    let mut callback = TcpStream::connect(("127.0.0.1", port)).unwrap();
    callback
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        callback,
        "GET /?state=secret&code=test-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    callback.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    let (head, body) = request_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|error| {
            let status = child.try_wait().unwrap();
            let record =
                std::fs::read_to_string(home.join(format!(".launch-with-aws/pending/{pid}.json")));
            panic!("TLS request {error}: child={status:?}, record={record:?}");
        });
    assert!(head.starts_with("POST /token "));
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["code"], "test-code");
    assert_eq!(body["codeVerifier"], "test-verifier");
    assert_eq!(body["redirectUri"], format!("http://127.0.0.1:{port}"));
    let state_dir = home.join(".launch-with-aws");
    let record_path = state_dir.join(format!("pending/{pid}.json"));
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["status"], "exchanging");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(state_dir.join(format!("pending/{pid}.lock")))
        .unwrap();
    assert!(lock.try_lock().is_err());
    std::sync::mpsc::SyncSender::send(&release_tx, ()).unwrap();
    server.join().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(lock.try_lock().is_ok());
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(record["status"], "succeeded");
    let session: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state_dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(session["access_token"], "test-access");
    assert_eq!(session["refresh_token"], "test-refresh");
    std::fs::remove_dir_all(home).unwrap();
}

/// Trace the real helper from exec to exit: no sampling window can hide a
/// short-lived descriptor or a process that is spawned and immediately exits.
#[cfg(all(target_os = "linux", target_arch = "x86_64", not(feature = "aws-sdk")))]
#[test]
#[allow(clippy::too_many_lines)]
fn helper_lifetime_has_no_process_spawns_and_opened_descriptors_are_cloexec() {
    use nix::libc;
    use nix::sys::{ptrace, signal, wait};
    use nix::unistd::Pid;
    use std::collections::{HashMap, HashSet};
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::process::CommandExt;
    use std::time::Duration;

    const MODE: &str = "RSI_CALLBACK_CUSTODY_TRACE";
    match std::env::var(MODE).as_deref() {
        Ok("helper") => {
            ptrace::traceme().unwrap();
            signal::raise(signal::Signal::SIGSTOP).unwrap();
            panic!(
                "helper exec failed: {}",
                Command::new(env!("CARGO_BIN_EXE_toolkit"))
                    .args(["launch", "auth-callback"])
                    .exec()
            );
        }
        Ok("runner") => {}
        _ => {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "helper_lifetime_has_no_process_spawns_and_opened_descriptors_are_cloexec",
                    "--nocapture",
                ])
                .env(MODE, "runner")
                .process_group(0)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let group = Pid::from_raw(i32::try_from(child.id()).unwrap());
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let _ = std::sync::mpsc::SyncSender::send(&tx, child.wait_with_output());
            });
            let output = rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|error| {
                    let _ = signal::killpg(group, signal::Signal::SIGKILL);
                    panic!("trace runner did not finish: {error}");
                })
                .unwrap();
            assert!(
                output.status.success(),
                "trace runner failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("callback-trace-{}-{unique}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "helper_lifetime_has_no_process_spawns_and_opened_descriptors_are_cloexec",
            "--nocapture",
        ])
        .env(MODE, "helper")
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 30;
    let input = format!(
        r#"{{"client_id":"c","client_secret":"s","client_expires_at":1,"scopes":["launch:access"],"code_verifier":"v","state":"secret","deadline":{deadline}}}"#
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let controller = std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout);
        let port = loop {
            let mut line = String::new();
            assert_ne!(lines.read_line(&mut line).unwrap(), 0);
            if let Some(port) = line
                .split_whitespace()
                .last()
                .and_then(|part| part.parse::<u16>().ok())
            {
                break port;
            }
        };
        let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            socket,
            "GET /?state=secret&error=access_denied HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
    });
    let trace_tid = match wait::waitpid(None, Some(wait::WaitPidFlag::__WALL)).unwrap() {
        wait::WaitStatus::Stopped(tid, signal::Signal::SIGSTOP) => tid,
        other => panic!("unexpected initial trace event: {other:?}"),
    };
    ptrace::setoptions(
        trace_tid,
        ptrace::Options::PTRACE_O_TRACESYSGOOD
            | ptrace::Options::PTRACE_O_TRACEEXEC
            | ptrace::Options::PTRACE_O_TRACEFORK
            | ptrace::Options::PTRACE_O_TRACEVFORK
            | ptrace::Options::PTRACE_O_TRACECLONE
            | ptrace::Options::PTRACE_O_EXITKILL,
    )
    .unwrap();
    ptrace::syscall(trace_tid, None).unwrap();
    let mut tids = HashSet::from([trace_tid]);
    let mut entering = HashMap::new();
    let mut running_helper = false;
    let mut opens = 0;
    let mut spawns = 0;
    let mut violations = Vec::new();
    let mut root_exit = None;
    while !tids.is_empty() {
        let status = wait::waitpid(None, Some(wait::WaitPidFlag::__WALL)).unwrap();
        match status {
            wait::WaitStatus::PtraceSyscall(tid) => {
                let regs = ptrace::getregs(tid).unwrap();
                let syscall = i64::try_from(regs.orig_rax).unwrap();
                if regs.rax == (-i64::from(libc::ENOSYS)) as u64 {
                    entering.insert(tid, (syscall, regs.rdi));
                } else if running_helper && (regs.rax as i64) >= 0 {
                    let is_fd = [
                        libc::SYS_open,
                        libc::SYS_openat,
                        libc::SYS_openat2,
                        libc::SYS_creat,
                        libc::SYS_socket,
                        libc::SYS_accept,
                        libc::SYS_accept4,
                        libc::SYS_eventfd,
                        libc::SYS_eventfd2,
                        libc::SYS_epoll_create,
                        libc::SYS_epoll_create1,
                        libc::SYS_dup,
                        libc::SYS_dup2,
                        libc::SYS_dup3,
                        libc::SYS_memfd_create,
                    ]
                    .contains(&syscall)
                        || (syscall == libc::SYS_fcntl
                            && matches!(regs.rsi as i32, libc::F_DUPFD | libc::F_DUPFD_CLOEXEC));
                    if is_fd {
                        let fd = regs.rax;
                        opens += 1;
                        match std::fs::read_to_string(format!("/proc/{tid}/fdinfo/{fd}")) {
                            Ok(info) => {
                                let flags = info
                                    .lines()
                                    .find_map(|line| line.strip_prefix("flags:\t"))
                                    .unwrap();
                                let flags = u64::from_str_radix(flags, 8).unwrap();
                                if flags & (libc::O_CLOEXEC as u64) == 0 {
                                    violations.push(format!("syscall {syscall} fd {fd}"));
                                }
                            }
                            Err(error) => {
                                violations.push(format!("could not audit fd {fd}: {error}"))
                            }
                        }
                    }
                }
                ptrace::syscall(tid, None).unwrap();
            }
            wait::WaitStatus::PtraceEvent(tid, _, event) => {
                if event == libc::PTRACE_EVENT_EXEC {
                    if running_helper {
                        spawns += 1;
                    } else {
                        tids.clear();
                        tids.insert(tid);
                    }
                    running_helper = true;
                } else if [
                    libc::PTRACE_EVENT_FORK,
                    libc::PTRACE_EVENT_VFORK,
                    libc::PTRACE_EVENT_CLONE,
                ]
                .contains(&event)
                {
                    let new_tid =
                        Pid::from_raw(i32::try_from(ptrace::getevent(tid).unwrap()).unwrap());
                    tids.insert(new_tid);
                    let is_thread = entering.get(&tid).is_some_and(|(call, flags)| {
                        *call == libc::SYS_clone && flags & (libc::CLONE_THREAD as u64) != 0
                    });
                    if running_helper && !is_thread {
                        spawns += 1;
                    }
                }
                ptrace::syscall(tid, None).unwrap();
            }
            wait::WaitStatus::Stopped(tid, sig) => {
                ptrace::syscall(
                    tid,
                    if sig == signal::Signal::SIGSTOP || sig == signal::Signal::SIGTRAP {
                        None
                    } else {
                        Some(sig)
                    },
                )
                .unwrap();
            }
            wait::WaitStatus::Exited(tid, code) => {
                tids.remove(&tid);
                if tid == pid {
                    root_exit = Some(code);
                }
            }
            wait::WaitStatus::Signaled(tid, sig, _) => panic!("traced task {tid} died: {sig}"),
            other => panic!("unexpected trace event: {other:?}"),
        }
    }
    controller.join().unwrap();
    assert_eq!(root_exit, Some(1)); // Denial is a normal unsuccessful sign-in.
    assert_eq!(spawns, 0, "helper spawned a process");
    assert!(
        opens >= 4,
        "trace did not observe the helper's file/socket opens"
    );
    assert!(
        violations.is_empty(),
        "non-CLOEXEC descriptors: {violations:?}"
    );
    std::fs::remove_dir_all(home).unwrap();
}
