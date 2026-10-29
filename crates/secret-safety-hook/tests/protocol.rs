//! End to end checks of the `secret-safety` binary: stdin in, stdout and exit
//! status out. Fixtures are synthetic; no credentials are involved.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use toolkit_core::{parse_json, Json};

const BINARY: &str = env!("CARGO_BIN_EXE_secret-safety");

const SECRET_DENY: &str = concat!(
    "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", ",
    "\"permissionDecision\": \"deny\", ",
    "\"permissionDecisionReason\": \"Direct secret fetching is blocked. ",
    "Use {{resolve:secretsmanager:secret-id:SecretString:key}} with asm-exec instead. ",
    "Run /aws-secrets-manager for details.\"}}"
);

fn run(input: &[u8]) -> (Option<i32>, String) {
    let mut child = Command::new(BINARY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn secret-safety");
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let owned = input.to_vec();
    let writer = thread::spawn(move || {
        // The hook may answer before reading everything (oversized input).
        let _ = stdin.write_all(&owned);
    });
    let output = child.wait_with_output().expect("wait for secret-safety");
    writer.join().expect("writer thread");
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
    )
}

fn payload(tool: &str, input: Json) -> Vec<u8> {
    Json::object(vec![
        ("tool_name", Json::string(tool)),
        ("tool_input", input),
    ])
    .to_compact_string()
    .into_bytes()
}

fn bash(command: &str) -> Vec<u8> {
    payload(
        "Bash",
        Json::object(vec![("command", Json::string(command))]),
    )
}

fn assert_denied(stdout: &str) {
    let parsed = parse_json(stdout).expect("deny output is JSON");
    let output = parsed.get("hookSpecificOutput").expect("wrapper object");
    assert_eq!(
        output.get("hookEventName").and_then(Json::as_str),
        Some("PreToolUse")
    );
    assert_eq!(
        output.get("permissionDecision").and_then(Json::as_str),
        Some("deny")
    );
}

#[test]
fn allowed_call_prints_nothing_and_exits_zero() {
    let (code, stdout) = run(&bash("ls -la"));
    assert_eq!(code, Some(0));
    assert_eq!(stdout, "");
}

#[test]
fn secret_fetch_prints_the_legacy_deny_bytes_and_exits_zero() {
    let (code, stdout) = run(&bash(
        "aws secretsmanager get-secret-value --secret-id demo/synthetic",
    ));
    assert_eq!(code, Some(0));
    assert_eq!(stdout, SECRET_DENY);
}

#[test]
fn every_branch_is_reachable_through_the_binary() {
    let denied: Vec<Vec<u8>> = vec![
        payload(
            "use_aws",
            Json::object(vec![
                ("service_name", Json::string("secretsmanager")),
                ("operation_name", Json::string("GetSecretValue")),
            ]),
        ),
        payload(
            "mcp__aws__call_aws",
            Json::object(vec![(
                "cli_command",
                Json::string(
                    "aws --region us-east-1 secretsmanager get-secret-value --secret-id x",
                ),
            )]),
        ),
        payload(
            "run_script",
            Json::object(vec![(
                "script",
                Json::string("client.get_secret_value(SecretId='x')"),
            )]),
        ),
        bash("curl http://localhost:2773/secretsmanager/get?secretId=x"),
        bash("python3 -c \"c.get_secret_value(SecretId='x')\""),
    ];
    for input in denied {
        let (code, stdout) = run(&input);
        assert_eq!(code, Some(0));
        assert_eq!(stdout, SECRET_DENY, "{}", String::from_utf8_lossy(&input));
    }
    let allowed: Vec<Vec<u8>> = vec![
        payload(
            "Read",
            Json::object(vec![("file_path", Json::string("/tmp/x"))]),
        ),
        payload(
            "use_aws",
            Json::object(vec![
                ("service_name", Json::string("s3")),
                ("operation_name", Json::string("ListBuckets")),
            ]),
        ),
        bash("grep -rn \"secretsmanager.*get_secret_value(\" src"),
    ];
    for input in allowed {
        let (code, stdout) = run(&input);
        assert_eq!(code, Some(0));
        assert_eq!(stdout, "", "{}", String::from_utf8_lossy(&input));
    }
}

#[test]
fn unvalidatable_input_is_denied_with_exit_zero() {
    for input in [
        b"".as_slice(),
        b"not json",
        b"[]",
        b"{\"tool_name\": 5}",
        b"{\"tool_name\": \"Bash\", \"tool_input\": \"x\"}",
        &[0xff, 0xfe],
    ] {
        let (code, stdout) = run(input);
        assert_eq!(code, Some(0), "{}", String::from_utf8_lossy(input));
        assert_denied(&stdout);
    }
}

#[test]
fn oversized_input_is_denied() {
    let big = vec![b' '; 5 * 1024 * 1024];
    let (code, stdout) = run(&big);
    assert_eq!(code, Some(0));
    assert_denied(&stdout);
}
