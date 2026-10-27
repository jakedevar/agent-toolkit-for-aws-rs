//! End-to-end tests of `toolkit asm exec` through the built binary: real
//! process spawns, real exit codes, and a fake Secrets Manager Agent that is a
//! listener on 127.0.0.1 inside this test. No AWS endpoint, DNS name or
//! credential is involved; the secret values are synthetic.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");
const VALUE: &str = "synthetic-secret-Zq9x";
const SESSION_TOKEN: &str = "synthetic-session-token";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "toolkit-asm-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

/// A fake agent: answers `/ping` and `/secretsmanager/get` and records the
/// request lines and token headers it saw.
struct FakeAgent {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl FakeAgent {
    fn start(secret_json: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (stop_flag, seen_log) = (Arc::clone(&stop), Arc::clone(&seen));
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                let first = text.lines().next().unwrap_or_default().to_string();
                let token_header = text
                    .lines()
                    .find(|line| line.starts_with("x-aws-parameters-secrets-token:"))
                    .unwrap_or("no-token")
                    .to_string();
                seen_log
                    .lock()
                    .expect("log")
                    .push(format!("{first} | {token_header}"));
                let body = if first.contains("/secretsmanager/get") {
                    secret_json
                } else {
                    "healthy"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            port,
            stop,
            seen,
            handle: Some(handle),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("log").clone()
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn toolkit(args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("AWS_TOKEN")
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .env_remove("AWS_SECRETS_MANAGER_AGENT_ENDPOINT")
        .env_remove("ASM_EXEC_MCP_ENDPOINT")
        .env_remove("ASM_EXEC_MCP_TIMEOUT")
        .stdin(Stdio::null());
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exited normally")
}

#[test]
fn a_missing_command_is_the_python_usage_error() {
    for args in [&["asm", "exec"][..], &["asm", "exec", "--"][..]] {
        let output = toolkit(args).output().expect("run toolkit");
        assert_eq!(status(&output), 1);
        assert_eq!(text(&output.stdout), "");
        assert_eq!(
            text(&output.stderr),
            "Usage: asm-exec <command> [args...]\n"
        );
    }
}

#[test]
fn the_command_status_is_the_exit_status_and_dash_words_reach_the_command() {
    for (args, expected) in [
        (&["asm", "exec", "--", "/bin/sh", "-c", "exit 5"][..], 5),
        (&["asm", "exec", "/bin/sh", "-c", "exit 6"][..], 6),
        (&["asm", "exec", "/bin/sh", "-c", "kill -9 $$"][..], 247),
        (&["asm", "exec", "/bin/true"][..], 0),
    ] {
        let output = toolkit(args).output().expect("run toolkit");
        assert_eq!(status(&output), expected, "{args:?}");
        assert_eq!(text(&output.stderr), "", "{args:?}");
    }
    // `--help` after `exec` belongs to the command, as it did for the Python.
    let output = toolkit(&["asm", "exec", "--help"])
        .output()
        .expect("run toolkit");
    assert_eq!(status(&output), 1);
    assert!(text(&output.stderr).starts_with("asm-exec: ERROR: Failed to run --help: "));
    assert_eq!(text(&output.stdout), "");
}

#[test]
fn the_command_keeps_the_standard_streams() {
    let output = toolkit(&["asm", "exec", "/bin/sh", "-c", "echo out; echo err >&2"])
        .output()
        .expect("run toolkit");
    assert_eq!(text(&output.stdout), "out\n");
    assert_eq!(text(&output.stderr), "err\n");
}

#[test]
fn a_secret_reaches_the_child_through_the_agent_and_nowhere_else() {
    let dir = temp_dir("agent");
    let agent = FakeAgent::start(r#"{"SecretString":"{\"password\":\"synthetic-secret-Zq9x\"}"}"#);
    let marker = dir.join("child.txt");
    let script = format!(
        r#"printf 'arg=%s\nenv=%s\n' "$1" "$FROM_ENV" > '{}'"#,
        marker.display()
    );
    let output = toolkit(&[
        "asm",
        "exec",
        "--",
        "/bin/sh",
        "-c",
        &script,
        "sh",
        "pw={{resolve:secretsmanager:prod/db:SecretString:password}}",
    ])
    .env("AWS_SECRETS_MANAGER_AGENT_ENDPOINT", agent.endpoint())
    .env("AWS_SESSION_TOKEN", SESSION_TOKEN)
    .env(
        "FROM_ENV",
        "Bearer {{resolve:secretsmanager:prod/db:SecretString:password}}",
    )
    .output()
    .expect("run toolkit");

    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    // The child saw the value in its argument and in its environment...
    assert_eq!(
        fs::read_to_string(&marker).expect("child output"),
        format!("arg=pw={VALUE}\nenv=Bearer {VALUE}\n")
    );
    // ...and nobody else did.
    for stream in [&output.stdout, &output.stderr] {
        assert!(!text(stream).contains(VALUE), "{}", text(stream));
    }
    // One ping (no token), then one fetch per reference carrying the token,
    // with the secret id and stage encoded.
    let seen = agent.seen();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[0], "get /ping http/1.1 | no-token");
    for line in &seen[1..] {
        assert_eq!(
            line,
            &format!(
                "get /secretsmanager/get?secretid=prod%2fdb&versionstage=awscurrent http/1.1 | x-aws-parameters-secrets-token: {SESSION_TOKEN}"
            )
        );
    }
}

#[test]
fn an_unresolvable_reference_fails_closed_and_the_command_never_starts() {
    let dir = temp_dir("fail");
    let marker = dir.join("started.txt");
    let script = format!("echo started > '{}'", marker.display());
    // No agent listens on this port; the MCP endpoint is rejected by the
    // endpoint check before any lookup or connection.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let output = toolkit(&[
        "asm",
        "exec",
        "/bin/sh",
        "-c",
        &script,
        "sh",
        "{{resolve:secretsmanager:prod/db}}",
    ])
    .env(
        "AWS_SECRETS_MANAGER_AGENT_ENDPOINT",
        format!("http://127.0.0.1:{closed}"),
    )
    .env(
        "ASM_EXEC_MCP_ENDPOINT",
        "http://aws-mcp.us-east-1.api.aws/mcp",
    )
    .output()
    .expect("run toolkit");
    assert_eq!(status(&output), 1);
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("asm-exec: MCP resolution failed: invalid ASM_EXEC_MCP_ENDPOINT"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("asm-exec: ERROR: Failed to resolve: prod/db\n"),
        "{stderr}"
    );
    assert!(!marker.exists(), "the command must not start");
}

#[test]
fn an_agent_endpoint_that_is_not_loopback_never_receives_the_token() {
    let output = toolkit(&["asm", "exec", "/bin/true", "{{resolve:secretsmanager:n}}"])
        .env(
            "AWS_SECRETS_MANAGER_AGENT_ENDPOINT",
            "http://192.0.2.1:2773",
        )
        .env("AWS_SESSION_TOKEN", SESSION_TOKEN)
        .env(
            "ASM_EXEC_MCP_ENDPOINT",
            "http://aws-mcp.us-east-1.api.aws/mcp",
        )
        .output()
        .expect("run toolkit");
    assert_eq!(status(&output), 1);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("Secrets Manager Agent not used: the endpoint must be a loopback address"),
        "{stderr}"
    );
    assert!(!stderr.contains(SESSION_TOKEN), "{stderr}");
}

#[test]
fn a_bad_timeout_setting_fails_before_anything_runs() {
    let output = toolkit(&["asm", "exec", "/bin/true"])
        .env("ASM_EXEC_MCP_TIMEOUT", "soon")
        .output()
        .expect("run toolkit");
    assert_eq!(status(&output), 1);
    assert!(text(&output.stderr).contains("ASM_EXEC_MCP_TIMEOUT"));
}
