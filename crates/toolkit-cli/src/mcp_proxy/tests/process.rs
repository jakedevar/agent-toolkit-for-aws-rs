//! Process-level pipe tests (#4413 S3): the test binary re-executes itself as
//! a child that runs the production start-up path ([`serve`]) over real
//! stdin, stdout and stderr pipes, with only the transport and the
//! credential source replaced (the default build has no SDK). The parent
//! writes client lines, reads stdout line by line, closes stdin and checks
//! the exit status. T-LEAK runs here too: canary credentials, a `--header`
//! canary and upstream-body canaries under `--log-level DEBUG` never reach
//! stderr or a proxy-generated stdout line.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use toolkit_aws::diag::Diag;
use toolkit_aws::{
    AwsError, Credentials, Env, MapEnv, McpTarget, SigningScope, StreamingTransport,
};
use toolkit_core::{parse_json, Json};

use super::harness::{ok_result, result, Reply, Step, Upstream};
use crate::mcp_proxy::cli::{parse, ProxyArgs};
use crate::mcp_proxy::command::{private_stderr, private_stdout, serve, Backend, Built};
use crate::mcp_proxy::{Clock, CredentialSource, Resolved, SourceAuth, SystemClock};
use crate::{route, Routed, COMMANDS};

const ENTRY: &str = "mcp_proxy::tests::process::child_entry";
const CASE: &str = "MCP_PROXY_TEST_CASE";
const ARGS: &str = "MCP_PROXY_TEST_ARGS";
const CANARIES: &[&str] = &[
    "SECRET-LEAK-CANARY",
    "TOKEN-LEAK-CANARY",
    "HEADER-LEAK-CANARY",
    "BODY-LEAK-CANARY",
];

struct CanarySource;

impl CredentialSource for CanarySource {
    fn resolve(&self, _skip_auth: bool, _force_rebuild: bool) -> Resolved {
        Resolved::Sign(
            Credentials::new(
                "AKIDLEAKFIXTURE",
                "SECRET-LEAK-CANARY",
                Some("TOKEN-LEAK-CANARY"),
                None,
            )
            .unwrap(),
        )
    }

    fn end_session(&self) {}
}

fn tool_name(seen: &super::harness::Seen) -> Option<String> {
    let params = seen.json.as_ref()?.get("params")?;
    Some(params.get("name")?.as_str()?.to_string())
}

fn upstream() -> Arc<Upstream> {
    Upstream::new(|seen| match seen.rpc_method() {
        Some("initialize") => Reply::json(&result(
            seen.id().unwrap(),
            r#"{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}"#,
        ))
        .header("mcp-session-id", "session-pipe-1"),
        Some("tools/call") => match tool_name(seen).as_deref() {
            Some("client-error") => Reply::status(400)
                .header("content-type", "text/plain")
                .data(b"BODY-LEAK-CANARY client"),
            Some("server-error") => Reply::status(503)
                .header("content-type", "application/json")
                .data(br#"{"message":"BODY-LEAK-CANARY server"}"#),
            Some("idle") => Reply::sse(&[]).step(Step::IdleTimeout),
            Some("malformed") => {
                Reply::sse_raw(&[b"data: {BODY-LEAK-CANARY not json\n\n".as_slice()])
            }
            Some("broken") => {
                Reply::sse(&[]).step(Step::Fail(AwsError::connect("BODY-LEAK-CANARY io")))
            }
            _ => Reply::json(&ok_result(seen)),
        },
        Some(_) => Reply::json(&ok_result(seen)),
        None => Reply::status(200),
    })
}

/// The child's backend: real pipes, fake transport and canary credentials.
struct PipeBackend {
    env: MapEnv,
    transport: Arc<Upstream>,
}

impl Backend for PipeBackend {
    fn env(&self) -> &dyn Env {
        &self.env
    }

    fn validate_roots(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn stdin(&mut self) -> Result<(Box<dyn Read + Send>, bool), String> {
        Ok((Box::new(std::io::stdin()), false))
    }

    fn check_target(&self, _target: &McpTarget) -> Result<(), String> {
        Ok(())
    }

    fn profile_region(&self, _profile: Option<&str>) -> Option<String> {
        None
    }

    fn build(
        &mut self,
        args: &ProxyArgs,
        scope: SigningScope,
        _null_stdin: bool,
        diag: Diag,
    ) -> Result<Built, String> {
        let sign_with = Box::new(
            |credentials: &Credentials,
             request: &mut toolkit_aws::HttpRequest,
             scope: &SigningScope| {
                request.headers.insert(
                    "authorization",
                    &format!(
                        "TEST {} {}",
                        credentials.access_key_id().expose(),
                        scope.service
                    ),
                )
            },
        );
        let auth = SourceAuth::new(
            Box::new(CanarySource),
            sign_with,
            scope,
            args.skip_auth,
            diag,
        );
        let transport: Arc<dyn StreamingTransport + Send + Sync> = self.transport.clone();
        Ok((transport, Arc::new(auth)))
    }

    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(SystemClock::new())
    }

    fn stdout(&mut self) -> Result<Box<dyn Write + Send>, String> {
        private_stdout()
            .map(|file| Box::new(file) as Box<dyn Write + Send>)
            .map_err(|_| "stdout".to_string())
    }

    fn stderr(&mut self) -> Box<dyn Write + Send> {
        private_stderr()
    }
}

fn proxy_args(words: &[String]) -> ProxyArgs {
    match route(COMMANDS, words) {
        Ok(Routed::Run(_, invocation)) => parse(&invocation).expect("valid arguments"),
        _ => panic!("routed"),
    }
}

/// The child: run `serve` over the process's own pipes, then exit with its
/// status (nothing else is written to stdout).
#[test]
fn child_entry() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    let words: Vec<String> = std::env::var(ARGS)
        .unwrap()
        .split('\n')
        .map(str::to_string)
        .collect();
    let args = proxy_args(&words);
    let upstream = upstream();
    let mut backend = PipeBackend {
        env: MapEnv::new(),
        transport: Arc::clone(&upstream),
    };
    let status = serve(&args, &mut backend);
    let deletes = upstream.count(toolkit_aws::Method::Delete);
    eprintln!("RESULT case={case} deletes={deletes}");
    std::process::exit(i32::from(status));
}

struct Run {
    child: Child,
    lines: Receiver<String>,
    stderr: Option<std::process::ChildStderr>,
}

fn spawn(case: &str, args: &[&str]) -> Run {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", ENTRY, "--nocapture", "--test-threads", "1", "-q"])
        .env_clear()
        .env(CASE, case)
        .env(ARGS, args.join("\n"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            // libtest's own banner is not a JSON-RPC line.
            if line.starts_with('{') || line.starts_with('[') {
                let _ = tx.send(line);
            }
        }
    });
    let stderr = child.stderr.take();
    Run {
        child,
        lines: rx,
        stderr,
    }
}

impl Run {
    fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn next(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(20))
            .expect("a stdout line")
    }

    /// Close stdin, wait, and return (exit code, remaining stdout lines, stderr).
    fn finish(mut self) -> (Option<i32>, Vec<String>, String) {
        drop(self.child.stdin.take());
        let status = self.child.wait().unwrap();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.stderr.take() {
            pipe.read_to_string(&mut stderr).unwrap();
        }
        let rest = self.lines.try_iter().collect();
        (status.code(), rest, stderr)
    }
}

const ENDPOINT: &str = "https://aws-mcp.us-east-1.api.aws/mcp";

fn rpc(line: &str) -> Json {
    parse_json(line).unwrap_or_else(|_| panic!("one JSON-RPC message per line: {line}"))
}

#[test]
fn a_session_over_real_pipes_ends_with_one_delete_and_status_0() {
    let mut run = spawn("roundtrip", &["mcp-proxy", ENDPOINT, "--skip-auth"]);
    run.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}"#);
    let line = rpc(&run.next());
    assert_eq!(line.get("id"), Some(&Json::Int(1)));
    run.send(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{}}}"#,
    );
    let line = rpc(&run.next());
    assert_eq!(line.get("id"), Some(&Json::Int(2)));
    let (code, rest, stderr) = run.finish();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(rest.is_empty(), "{rest:?}");
    assert!(
        stderr.contains("RESULT case=roundtrip deletes=1"),
        "{stderr}"
    );
}

#[test]
fn a_closed_stdout_ends_the_proxy_with_status_1() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", ENTRY, "--nocapture", "--test-threads", "1", "-q"])
        .env_clear()
        .env(CASE, "stdout-closed")
        .env(ARGS, ["mcp-proxy", ENDPOINT].join("\n"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Wait until libtest has printed its banner (the test is running), then
    // close the read end: the proxy's next stdout write fails with EPIPE.
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut banner = String::new();
    while !banner.contains("running 1 test") {
        let mut line = String::new();
        assert!(stdout.read_line(&mut line).unwrap() > 0, "banner");
        banner.push_str(&line);
    }
    drop(stdout);
    // stdin stays open (taken out of `child`, so `wait` does not close it):
    // the exit must come from the failed stdout write, not from EOF.
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();
    let status = child.wait().unwrap();
    drop(stdin);
    assert_eq!(status.code(), Some(1));
}

#[test]
fn a_refused_start_writes_one_fixed_line_and_nothing_to_stdout() {
    // No region anywhere: the scope step refuses before anything runs.
    let run = spawn(
        "refused",
        &[
            "mcp-proxy",
            "https://connect.aidevops.us-east-1.api.aws/mcp",
            "--service",
            "aidevops",
        ],
    );
    let (code, rest, stderr) = run.finish();
    assert_eq!(code, Some(2), "{stderr}");
    assert!(rest.is_empty());
    assert!(
        stderr.contains("toolkit mcp-proxy: no signing region: pass --region or set AWS_REGION\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"));
}

#[test]
fn t_leak_no_canary_reaches_stderr_or_a_proxy_generated_line() {
    let mut run = spawn(
        "leak",
        &[
            "mcp-proxy",
            ENDPOINT,
            "--log-level",
            "DEBUG",
            "--header",
            "X-Amzn-AgentAccess-Streaming-Session-Url=HEADER-LEAK-CANARY",
        ],
    );
    let mut lines = Vec::new();
    for (id, tool) in [
        "echo",
        "client-error",
        "server-error",
        "idle",
        "malformed",
        "broken",
    ]
    .iter()
    .enumerate()
    {
        run.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{tool}","arguments":{{}}}}}}"#
        ));
        lines.push(run.next());
    }
    let (code, rest, stderr) = run.finish();
    assert_eq!(code, Some(0), "{stderr}");
    lines.extend(rest);
    for line in &lines {
        rpc(line);
        for canary in CANARIES {
            assert!(!line.contains(canary), "{canary} in stdout: {line}");
        }
    }
    for canary in CANARIES {
        assert!(!stderr.contains(canary), "{canary} in stderr: {stderr}");
    }
    // DEBUG shows the failures, by kind only.
    assert!(
        stderr.contains("toolkit mcp-proxy: http-status status=400"),
        "{stderr}"
    );
    assert!(
        stderr.contains("toolkit mcp-proxy: upstream-message-malformed"),
        "{stderr}"
    );
}
