//! T-AC3-INH (x402 S5 #5352, design section 7 G1c) for the `fetch` routes.
//!
//! Every route runs once in process through `toolkit_cli::run` and once as
//! the built binary under a hostile, non-fixture environment whose endpoint
//! and credential variables point at a loopback recorder owned by this test.
//! A verification build has no payment support (D32), so each route must
//! print the one D32 document, accept zero connections and leave every file
//! alone, whatever else argv says.
//! Runs in the default and the `aws-sdk` verification builds.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use toolkit_cli::agents_pay::deps::NO_PAYMENT_SUPPORT;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("agents-pay-ac3-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    Scratch(root)
}

fn write(path: &Path, text: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A hostile home: a valid non-fixture payment config and AWS profile.
fn hostile_home(root: &Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &home.join(".agents-pay/config.json"),
        r#"{"resources":{"payment_manager_arn":"arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/prod-abcdefghij","payment_instrument_id":"payment-instrument-PRODabcdefghijk","payment_session_id":"payment-session-PRODabcdefghijk","user_id":"agents-pay-a1b2c3d4e5f6"},"policy":{"max_per_payment_usd":"5","allowed_networks":["eip155:8453"],"allow_any_recipient":true}}"#,
        0o600,
    );
    fs::set_permissions(home.join(".agents-pay"), fs::Permissions::from_mode(0o700)).unwrap();
    write(
        &home.join(".aws/config"),
        "[default]\nregion = us-east-1\n[profile hostile]\nregion = us-east-1\n",
        0o600,
    );
    write(
        &home.join(".aws/credentials"),
        "[hostile]\naws_access_key_id = AKIAZZZZZZZZZZZZZZZZ\naws_secret_access_key = hostileSecretKeyhostileSecretKeyhostile0\n",
        0o600,
    );
    write(
        &root.join("project/agentcore/.cli/deployed-state.json"),
        r#"{"targets":{"default":{"resources":{"payments":[{"paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/prod-abcdefghij"}]}}}}"#,
        0o644,
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

fn assert_no_connection(listener: &TcpListener) {
    match listener.accept() {
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        other => panic!("the loopback recorder saw a connection: {other:?}"),
    }
}

const ROUTES: &[&[&str]] = &[
    &["agents-pay", "fetch", "https://merchant.example/paid"],
    &[
        "agents-pay",
        "fetch",
        "https://merchant.example/paid",
        "--method",
        "HEAD",
        "--purchase-id",
        "order-1",
    ],
    &["agents-pay", "fetch", "--status"],
    &[
        "agents-pay",
        "fetch",
        "--browser-handle",
        "https://merchant.example/paid",
    ],
    // D32 wins over the handler's own usage errors: argv is never parsed.
    &["agents-pay", "fetch"],
    &[
        "agents-pay",
        "fetch",
        "https://merchant.example/paid",
        "--method",
        "POST",
    ],
    &["agents-pay", "fetch", "--status", "--method", "PUT"],
];

/// The one document every `fetch` form prints in a build without the live
/// feature (D32), `--status` included.
fn expected(_route: &[&str]) -> String {
    format!("{{\"paid\": false, \"refused\": true, \"reason\": \"{NO_PAYMENT_SUPPORT}\"}}\n")
}

#[test]
fn t_ac3_inh_fetch_routes_in_process_make_no_connection_and_touch_no_file() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for route in ROUTES {
        let args: Vec<OsString> = route.iter().map(OsString::from).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = toolkit_cli::run(&args, &mut out, &mut err);
        assert_eq!(code, 2, "{route:?}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            expected(route),
            "{route:?}"
        );
        assert!(err.is_empty(), "{route:?}");
        assert_no_connection(&listener);
    }
}

#[test]
fn t_ac3_inh_fetch_routes_as_a_child_under_a_hostile_environment() {
    let root = scratch();
    let (home, cwd) = hostile_home(&root.0);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let recorder = format!("http://{}", listener.local_addr().unwrap());
    let before = snapshot(&root.0);
    for route in ROUTES {
        let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(*route)
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("AWS_PROFILE", "hostile")
            .env("AWS_ACCESS_KEY_ID", "AKIAYYYYYYYYYYYYYYYY")
            .env(
                "AWS_SECRET_ACCESS_KEY",
                "hostileEnvSecrethostileEnvSecrethostile0",
            )
            .env("AWS_REGION", "us-east-1")
            .env(
                "PAYMENT_MANAGER_ARN",
                "arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/prod-abcdefghij",
            )
            .env(
                "PAYMENT_INSTRUMENT_ID",
                "payment-instrument-PRODabcdefghijk",
            )
            .env("PAYMENT_SESSION_ID", "payment-session-PRODabcdefghijk")
            .env("PAYMENT_USER_ID", "agents-pay-a1b2c3d4e5f6")
            .env("AWS_ENDPOINT_URL", &recorder)
            .env("AWS_ENDPOINT_URL_BEDROCK_AGENTCORE", &recorder)
            .env("AWS_EC2_METADATA_SERVICE_ENDPOINT", &recorder)
            .env("AWS_CONTAINER_CREDENTIALS_FULL_URI", &recorder)
            .env("AGENTCORE_PROJECT_DIR", root.0.join("project"))
            .env("AGENTS_PAY_CONFIG", home.join(".agents-pay/config.json"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{route:?}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            expected(route),
            "{route:?}"
        );
        assert!(out.stderr.is_empty(), "{route:?}");
        assert_no_connection(&listener);
    }
    assert_eq!(snapshot(&root.0), before, "a file changed or was created");
}
