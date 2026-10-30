//! T-BR-ERR-BIN and T-BR-D32 / T-AC3-INH-BR (`OpenClaw` B2, #5443; design
//! sections 4.7 and 4.8): the real `toolkit agents-pay bridge` binary of a
//! verification build, which has no payment support.
//!
//! The route reads its stdin request in every build (the G1c refinement of
//! design 4.8), so the AC3 assertions here are: at most 131073 stdin bytes
//! read, the R-BR-D32 document for a valid service request, the pure
//! `sanitize_output` still answered, zero connections to a loopback
//! recorder named by every endpoint variable, an empty stderr, and `HOME`
//! and the working directory byte- and mtime-identical.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Seek, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

const D32: &str = "{\"ok\":false,\"error\":\"payment support unavailable\"}\n";
const INPUT: &str = "{\"ok\":false,\"error\":\"invalid bridge request\"}\n";

fn contract(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/aws-agents/skills/agents-pay/packages/openclaw/test/fixtures/bridge-contract")
        .join(name)
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(label: &str) -> Scratch {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "agents-pay-bridge-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    Scratch(root)
}

/// A stdin file holding `bytes`, and a handle sharing its file offset.
fn stdin_file(dir: &Path, name: &str, bytes: &[u8]) -> (fs::File, fs::File) {
    let path = dir.join(name);
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    let file = fs::File::open(&path).unwrap();
    let shared = file.try_clone().unwrap();
    (file, shared)
}

fn bridge(stdin: Stdio, configure: impl FnOnce(&mut Command)) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolkit"));
    command.args(["agents-pay", "bridge"]).stdin(stdin);
    configure(&mut command);
    command.output().unwrap()
}

fn piped(bytes: &[u8]) -> Output {
    let dir = scratch("stdin");
    let (file, _) = stdin_file(&dir.0, "request.json", bytes);
    bridge(Stdio::from(file), |_| {})
}

fn assert_frame(out: &Output, code: i32, stdout: &str) {
    assert_eq!(out.status.code(), Some(code), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), stdout);
    assert!(
        out.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn t_br_err_bin_invalid_stdin_fails_without_diagnostics() {
    assert_frame(&piped(br#"{"operation":"not-supported"}"#), 2, INPUT);
    assert_frame(&piped(b""), 2, INPUT);
    assert_frame(&piped(b"\xef\xbb\xbf{}"), 2, INPUT);
    assert_frame(
        &piped(&fs::read(contract("legacy_process_payment.request.json")).unwrap()),
        2,
        INPUT,
    );
    let null = bridge(Stdio::null(), |_| {});
    assert_frame(&null, 2, INPUT);
}

#[test]
fn t_br_err_bin_valid_service_requests_get_the_d32_document() {
    for name in ["get_payment_session", "process_payment"] {
        let request = fs::read(contract(&format!("{name}.request.json"))).unwrap();
        assert_frame(&piped(&request), 1, D32);
    }
}

#[test]
fn t_br_err_bin_sanitize_output_runs_without_payment_support() {
    let text = fs::read_to_string(contract("final-output/registry-cases.json")).unwrap();
    let fixture = toolkit_core::parse_json(&text).unwrap();
    let Some(toolkit_core::Json::Array(cases)) = fixture.get("cases") else {
        panic!("cases")
    };
    for case in cases {
        let request = case.get("request").unwrap().to_compact_string();
        let out = piped(request.as_bytes());
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(out.stderr.is_empty());
        let frame = String::from_utf8(out.stdout).unwrap();
        assert!(frame.ends_with('\n') && frame.is_ascii());
        let document = toolkit_core::parse_json(frame.trim_end()).unwrap();
        assert_eq!(document.get("ok"), Some(&toolkit_core::Json::Bool(true)));
        let result = document.get("result").unwrap();
        let expected = case.get("expected").unwrap();
        let text_of = |value: &toolkit_core::Json| {
            value.get("content").and_then(|c| match c {
                toolkit_core::Json::Array(items) => items
                    .first()
                    .and_then(|item| item.get("text"))
                    .and_then(toolkit_core::Json::as_str)
                    .map(str::to_owned),
                _ => None,
            })
        };
        assert_eq!(text_of(result), text_of(expected), "{frame}");
    }
}

/// `OpenClaw` B4 (#4411, R-OC-FINAL): `tool-cases.json` holds the exact
/// `sanitize_output` requests the TypeScript paid tool builds (the JS suite
/// asserts it builds them byte for byte) and the frame for each. The real
/// binary must answer each with exactly that frame, so the JS tests that
/// replay these frames never stand in for a matcher of their own.
#[test]
fn t_oc_final_tool_cases_answer_with_the_pinned_frames() {
    use toolkit_core::Json;
    let text = fs::read_to_string(contract("final-output/tool-cases.json")).unwrap();
    let fixture = toolkit_core::parse_json(&text).unwrap();
    let Some(Json::Array(cases)) = fixture.get("cases") else {
        panic!("cases")
    };
    let mut outcomes = BTreeMap::new();
    for case in cases {
        let name = case.get("name").and_then(Json::as_str).unwrap();
        let outcome = case.get("outcome").and_then(Json::as_str).unwrap();
        let request = case.get("request").unwrap();
        let frame = case.get("frame").and_then(Json::as_str).unwrap();
        let out = piped(request.to_compact_string().as_bytes());
        assert_eq!(out.status.code(), Some(0), "{name}: {out:?}");
        assert!(out.stderr.is_empty(), "{name}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), frame, "{name}");

        let Some(Json::Array(secrets)) = request.get("secrets") else {
            panic!("{name}: secrets")
        };
        assert_eq!(secrets.len(), 3, "{name}");
        for secret in secrets {
            let secret = secret.as_str().unwrap();
            assert!(
                !frame.contains(secret),
                "{name}: a secret reached the frame"
            );
        }
        let document = toolkit_core::parse_json(frame.trim_end()).unwrap();
        let result = document.get("result").unwrap();
        let details = result.get("details").unwrap();
        match outcome {
            "clean" => assert_eq!(Some(result), request.get("candidate"), "{name}"),
            "redact" => {
                assert_eq!(details.get("redacted"), Some(&Json::Bool(true)), "{name}");
                assert_eq!(details.get("paid"), Some(&Json::Bool(true)), "{name}");
            }
            "refuse" => assert_eq!(
                details.to_compact_string(),
                r#"{"paid":false,"reason":"Payment flow failed safely without exposing provider or proof details","refused":true}"#,
                "{name}"
            ),
            other => panic!("{name}: outcome {other}"),
        }
        *outcomes.entry(outcome.to_owned()).or_insert(0) += 1;
    }
    let counts: Vec<(&str, i32)> = outcomes.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(counts, [("clean", 4), ("redact", 2), ("refuse", 2)]);
}

#[test]
fn t_br_err_bin_argv_is_refused_by_the_router() {
    for extra in ["positional", "--payload", "--profile"] {
        let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["agents-pay", "bridge", extra])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{extra}");
        assert!(out.stdout.is_empty(), "{extra}");
    }
}

fn write(path: &Path, text: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn hostile_home(root: &Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &home.join(".agents-pay/config.json"),
        r#"{"resources":{"payment_manager_arn":"arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/prod-abcdefghij","payment_session_id":"payment-session-PRODabcdefghijk"},"policy":{"max_per_payment_usd":"5","allowed_networks":["eip155:8453"],"allow_any_recipient":true}}"#,
        0o600,
    );
    write(
        &home.join(".x402/config.json"),
        r#"{"maxPaymentAmountAtomic":"999999999","allowAnyRecipient":true}"#,
        0o600,
    );
    write(
        &home.join(".aws/credentials"),
        "[hostile]\naws_access_key_id = AKIAZZZZZZZZZZZZZZZZ\naws_secret_access_key = hostileSecretKeyhostileSecretKeyhostile0\n",
        0o600,
    );
    (home, cwd)
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut found = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path.clone());
                found.insert(path, (Vec::new(), meta.modified().unwrap()));
            } else {
                found.insert(
                    path.clone(),
                    (fs::read(&path).unwrap(), meta.modified().unwrap()),
                );
            }
        }
    }
    found
}

#[test]
fn t_ac3_inh_br_bridge_route_as_a_child_under_a_hostile_environment() {
    let root = scratch("ac3");
    let (home, cwd) = hostile_home(&root.0);
    let inputs = root.0.join("inputs");
    fs::create_dir_all(&inputs).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let recorder = format!("http://{}", listener.local_addr().unwrap());
    let session = fs::read(contract("get_payment_session.request.json")).unwrap();
    let payment = fs::read(contract("process_payment.request.json")).unwrap();
    let mut flood = payment.clone();
    flood.resize(1 << 20, b' ');
    let cases: Vec<(&str, Vec<u8>, i32, &str)> = vec![
        ("session", session, 1, D32),
        ("payment", payment, 1, D32),
        ("1 MiB stream", flood, 2, INPUT),
    ];
    let before = snapshot(&root.0);
    for (label, bytes, code, stdout) in cases {
        let (file, mut shared) = stdin_file(&inputs, label, &bytes);
        let out = bridge(Stdio::from(file), |command| {
            command
                .current_dir(&cwd)
                .env("HOME", &home)
                .env("AWS_PROFILE", "hostile")
                .env("AWS_ACCESS_KEY_ID", "AKIAYYYYYYYYYYYYYYYY")
                .env(
                    "AWS_SECRET_ACCESS_KEY",
                    "hostileEnvSecrethostileEnvSecrethostile0",
                )
                .env("AWS_REGION", "ap-south-1")
                .env("AWS_DEFAULT_REGION", "ap-south-1")
                .env("PAYMENT_SESSION_ID", "payment-session-PRODabcdefghijk")
                .env("PAYMENT_MANAGER_ARN", "arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/prod-abcdefghij")
                .env("AWS_ENDPOINT_URL", &recorder)
                .env("AWS_ENDPOINT_URL_BEDROCK_AGENTCORE", &recorder)
                .env("AWS_EC2_METADATA_SERVICE_ENDPOINT", &recorder)
                .env("AWS_CONTAINER_CREDENTIALS_FULL_URI", &recorder)
                .env("HTTPS_PROXY", &recorder)
                .env("AGENTS_PAY_CONFIG", home.join(".agents-pay/config.json"));
        });
        assert_frame(&out, code, stdout);
        let read = shared.stream_position().unwrap();
        assert!(read <= 131_073, "{label}: read {read} stdin bytes");
        if code == 1 {
            assert_eq!(read, bytes.len() as u64, "{label}: the whole request");
        }
        match listener.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            other => panic!("{label}: the loopback recorder saw a connection: {other:?}"),
        }
    }
    let mut after = snapshot(&root.0);
    after.retain(|path, _| !path.starts_with(&inputs));
    let mut expected = before;
    expected.retain(|path, _| !path.starts_with(&inputs));
    assert_eq!(after, expected, "a file changed or was created");
}
