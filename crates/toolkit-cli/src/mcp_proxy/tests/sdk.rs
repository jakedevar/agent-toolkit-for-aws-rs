//! aws-sdk suites of S3 (#4413):
//!
//! - T-SIG-2: a proxied `initialize` signed for `aws-mcp`/`us-east-1` gives a
//!   byte-exact `Authorization` (the fixture was derived independently with
//!   `openssl dgst -mac HMAC` over the canonical request; the same pipeline
//!   reproduces AWS's `get-vanilla` answer);
//! - T-SIG-4 with the real signer: the signature re-verifies over the sent,
//!   post-`_meta` body;
//! - end to end in child processes through the production [`SdkBackend`]
//!   (real roots from `SSL_CERT_FILE`, real stdin isolation, S1's guarded
//!   credentials over the real aws-config providers), with only the MCP
//!   transport, the start-up DNS answer and the provider-plane client (the
//!   recorder) replaced: T-ABS (#5013 cases with valid recorder-backed
//!   fallbacks), T-PROV, T-LEAK-SDK under `RUST_LOG` and T-LEAK-GATE.

use std::collections::hash_map::RandomState;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toolkit_aws::diag::Diag;
use toolkit_aws::sdk::recorder::{ProviderRecorder, RecordedReply};
use toolkit_aws::sdk::SigV4Signer;
use toolkit_aws::{AwsError, Credentials, CredentialsProvider, HostResolver, Signer};

use super::harness::{
    config, ok_result, request, result, start_with, tool_call, Reply, Sink, Upstream,
};
use crate::mcp_proxy::cli::parse;
use crate::mcp_proxy::command::{serve, SdkBackend};
use crate::mcp_proxy::scope::USER_AGENT;
use crate::mcp_proxy::{CredentialSource, Resolved, SignWith, SourceAuth};
use crate::{route, Routed, COMMANDS};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

fn fixed_clock() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_440_938_160) // 2015-08-30T12:36:00Z
}

struct Static(Credentials);

impl CredentialsProvider for Static {
    fn name(&self) -> &'static str {
        "static"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        Ok(self.0.clone())
    }
}

struct Example;

impl CredentialSource for Example {
    fn resolve(&self, _skip: bool, _rebuild: bool) -> Resolved {
        Resolved::Sign(Credentials::new("AKIDEXAMPLE", SECRET, None, None).unwrap())
    }

    fn end_session(&self) {}
}

fn fixed_sigv4() -> SignWith {
    Box::new(|credentials, request, scope| {
        SigV4Signer::new(Static(credentials.clone()))
            .with_clock(fixed_clock)
            .sign(request, scope)
    })
}

fn example_auth() -> SourceAuth {
    SourceAuth::new(
        Box::new(Example),
        fixed_sigv4(),
        toolkit_aws::SigningScope {
            region: "us-east-1".to_string(),
            service: "aws-mcp".to_string(),
        },
        false,
        Diag::new("mcp-proxy", Box::new(Sink::default())),
    )
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t-sig-2","version":"1"}}}"#;

#[test]
fn t_sig_2_a_proxied_initialize_has_the_recorded_authorization() {
    let upstream = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let mut cfg = config();
    cfg.url = "https://aws-mcp.us-east-1.api.aws/mcp".to_string();
    cfg.user_agent = Some(USER_AGENT.to_string());
    let mut h = start_with(cfg, upstream.clone(), Arc::new(example_auth()));
    h.send(INITIALIZE);
    h.next();
    let seen = upstream.seen_rpc("initialize")[0].clone();
    assert_eq!(seen.body_text(), INITIALIZE);
    assert_eq!(seen.header("x-amz-date"), Some("20150830T123600Z"));
    assert_eq!(
        seen.header("authorization"),
        Some(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/aws-mcp/aws4_request, \
             SignedHeaders=accept;content-type;host;x-amz-date, \
             Signature=aa66e3f4998328d91eab3f1b2a5b3682fe1876c7393abffa848c5ebbc509f37b"
        )
    );
    h.finish();
}

#[test]
fn t_sig_4_the_signature_covers_the_sent_post_meta_body() {
    let upstream = Upstream::new(|seen| Reply::json(&ok_result(seen)));
    let mut cfg = config();
    cfg.url = "https://aws-mcp.us-east-1.api.aws/mcp".to_string();
    cfg.metadata = vec![(
        "INSTALL_SOURCE".to_string(),
        "agent-toolkit-core".to_string(),
    )];
    let mut h = start_with(cfg, upstream.clone(), Arc::new(example_auth()));
    h.send(&tool_call(1, "echo"));
    h.next();
    let seen = upstream.seen_rpc("tools/call")[0].clone();
    assert!(seen
        .body_text()
        .contains(r#""_meta":{"INSTALL_SOURCE":"agent-toolkit-core"}"#));
    let mut headers = toolkit_aws::Headers::new();
    for (name, value) in seen.headers.entries() {
        if !matches!(name, "authorization" | "x-amz-date") {
            headers.insert(name, value).unwrap();
        }
    }
    let mut rebuilt = toolkit_aws::HttpRequest {
        method: seen.method,
        url: seen.url.clone(),
        headers,
        body: seen.body.clone(),
        timeouts: toolkit_aws::Timeouts::default(),
    };
    SigV4Signer::new(Static(
        Credentials::new("AKIDEXAMPLE", SECRET, None, None).unwrap(),
    ))
    .with_clock(fixed_clock)
    .sign(
        &mut rebuilt,
        &toolkit_aws::SigningScope {
            region: "us-east-1".to_string(),
            service: "aws-mcp".to_string(),
        },
    )
    .unwrap();
    assert_eq!(
        rebuilt.headers.get("authorization"),
        seen.header("authorization")
    );
    h.finish();
}

// --------------------------------------------- child processes, real SDK

const ENTRY: &str = "mcp_proxy::tests::sdk::child_entry";
const FIXTURE_ENTRY: &str = "mcp_proxy::tests::sdk::credential_process_fixture";
const FALLBACK: &str = "AKIAFALLBACKCANARY0";
const IMDS: &str = "169.254.169.254";
const ENDPOINT: &str = "https://aws-mcp.us-east-1.api.aws/mcp";

/// Every host resolves to one public address (the start-up check only; the
/// fake transport opens no socket).
struct PublicDns;

impl HostResolver for PublicDns {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        Ok(vec![IpAddr::from([52, 94, 0, 1])])
    }
}

fn credentials_json(key: &str) -> String {
    format!(
        "{{\"Code\":\"Success\",\"Type\":\"AWS-HMAC\",\"AccessKeyId\":\"{key}\",\
         \"SecretAccessKey\":\"fallback-secret\",\"Token\":\"fallback-token\",\
         \"Expiration\":\"2099-01-01T00:00:00Z\",\"LastUpdated\":\"2026-01-01T00:00:00Z\"}}"
    )
}

fn recorder() -> ProviderRecorder {
    let mut recorder = ProviderRecorder::new();
    if std::env::var("FIXTURE_ECS").is_ok() {
        recorder = recorder.route(
            "GET 169.254.170.2/fallback-creds",
            vec![RecordedReply::ok(200, &credentials_json(FALLBACK))],
        );
    }
    if std::env::var("FIXTURE_IMDS").is_ok() {
        recorder = recorder
            .route(
                &format!("PUT {IMDS}/latest/api/token"),
                vec![RecordedReply::ok(200, "imds-token")
                    .header("x-aws-ec2-metadata-token-ttl-seconds", "21600")],
            )
            .route(
                &format!("GET {IMDS}/latest/meta-data/iam/security-credentials/"),
                vec![RecordedReply::ok(200, "fallback-role")],
            )
            .route(
                &format!("GET {IMDS}/latest/meta-data/iam/security-credentials/fallback-role"),
                vec![RecordedReply::ok(200, &credentials_json(FALLBACK))],
            );
    }
    recorder
}

/// A `credential_process` fixture: print a bare canary on stderr, exit 1.
#[test]
fn credential_process_fixture() {
    let Ok(canary) = std::env::var("FIXTURE_PROCESS_CANARY") else {
        return;
    };
    eprintln!("{canary}");
    std::process::exit(1);
}

/// The child: the production start-up path over real pipes, then facts.
#[test]
fn child_entry() {
    let Ok(words) = std::env::var("MCP_PROXY_SDK_ARGS") else {
        return;
    };
    let words: Vec<String> = words.split('\n').map(str::to_string).collect();
    let Ok(Routed::Run(_, invocation)) = route(COMMANDS, &words) else {
        panic!("routed");
    };
    let args = parse(&invocation).expect("valid arguments");
    let posts = Arc::new(AtomicUsize::new(0));
    let signed: Arc<Mutex<Vec<String>>> = Arc::default();
    let upstream = {
        let (posts, signed) = (Arc::clone(&posts), Arc::clone(&signed));
        Upstream::new(move |seen| {
            if seen.method == toolkit_aws::Method::Post {
                let n = posts.fetch_add(1, Ordering::SeqCst);
                signed.lock().unwrap().push(
                    seen.header("authorization")
                        .and_then(|a| a.split("Credential=").nth(1))
                        .and_then(|c| c.split('/').next())
                        .unwrap_or("none")
                        .to_string(),
                );
                if n == 0 && std::env::var("FIXTURE_FIRST_403").is_ok() {
                    // Server-controlled values that must never reach a provider.
                    return Reply::status(403)
                        .header("aws-profile", "evil")
                        .header("location", "https://evil.example/creds");
                }
            }
            match seen.id() {
                Some(id) => Reply::sse(&[&result(
                    id,
                    r#"{"content":[{"type":"text","text":"AWS_PROFILE=evil role_arn=arn:aws:iam::1:role/x"}]}"#,
                )]),
                None => Reply::status(200),
            }
        })
    };
    let recorder = recorder();
    let mut backend = SdkBackend::new()
        .with_build_recording()
        .with_resolver(Arc::new(PublicDns))
        .with_transport(upstream)
        .with_provider_http(recorder.provider_http());
    let status = serve(&args, &mut backend);
    let log = backend
        .credentials()
        .map(|c| {
            c.build_log()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    eprintln!("RESULT status={status}");
    eprintln!("RESULT posts={}", posts.load(Ordering::SeqCst));
    eprintln!("RESULT signed={}", signed.lock().unwrap().join(","));
    eprintln!("RESULT build={log}");
    eprintln!("RESULT recorder={}", recorder.seen().join(","));
    eprintln!(
        "RESULT dispatcher={}",
        toolkit_aws::sdk::tracing_dispatcher_set()
    );
    std::process::exit(i32::from(status));
}

struct Outcome {
    code: Option<i32>,
    stdout: Vec<String>,
    stderr: String,
    results: BTreeMap<String, String>,
}

impl Outcome {
    fn get(&self, key: &str) -> &str {
        self.results
            .get(key)
            .map_or_else(|| panic!("no {key}:\n{}", self.stderr), String::as_str)
    }
}

struct Case {
    args: Vec<String>,
    env: Vec<(String, String)>,
    config: String,
    credentials: String,
}

impl Case {
    fn new(extra: &[&str]) -> Self {
        let mut args = vec!["mcp-proxy".to_string(), ENDPOINT.to_string()];
        args.extend(extra.iter().map(|s| (*s).to_string()));
        Self {
            args,
            env: vec![("AWS_EC2_METADATA_DISABLED".to_string(), "true".to_string())],
            config: String::new(),
            credentials: String::new(),
        }
    }

    fn env(mut self, name: &str, value: &str) -> Self {
        self.env.retain(|(n, _)| n != name);
        self.env.push((name.to_string(), value.to_string()));
        self
    }

    fn imds(mut self) -> Self {
        self.env.retain(|(n, _)| n != "AWS_EC2_METADATA_DISABLED");
        self.env("FIXTURE_IMDS", "1")
    }

    fn ecs(self) -> Self {
        self.env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", "/fallback-creds")
            .env("FIXTURE_ECS", "1")
    }

    fn config(mut self, text: &str) -> Self {
        self.config = text.to_string();
        self
    }

    fn credentials(mut self, text: &str) -> Self {
        self.credentials = text.to_string();
        self
    }

    fn run(&self, line: &str) -> Outcome {
        let tmp = std::env::temp_dir().join(format!(
            "toolkit-cli-mcp-sdk-{}-{}",
            std::process::id(),
            RandomState::new().build_hasher().finish()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let config = tmp.join("config");
        let credentials = tmp.join("credentials");
        std::fs::write(&config, &self.config).unwrap();
        std::fs::write(&credentials, &self.credentials).unwrap();
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/mcp_proxy/test-root.pem");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", ENTRY, "--nocapture", "--test-threads", "1", "-q"])
            .env_clear()
            .env("MCP_PROXY_SDK_ARGS", self.args.join("\n"))
            .env("HOME", &tmp)
            .env("AWS_CONFIG_FILE", &config)
            .env("AWS_SHARED_CREDENTIALS_FILE", &credentials)
            .env("SSL_CERT_FILE", &root)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in &self.env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut lines = Vec::new();
        // Wait for the one response, then close stdin.
        loop {
            let mut text = String::new();
            if stdout.read_line(&mut text).unwrap() == 0 {
                break;
            }
            let text = text.trim_end().to_string();
            if text.starts_with('{') {
                lines.push(text);
                break;
            }
        }
        drop(stdin);
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        lines.extend(
            rest.lines()
                .filter(|l| l.starts_with('{'))
                .map(str::to_string),
        );
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        let code = child.wait().unwrap().code();
        let _ = std::fs::remove_dir_all(&tmp);
        let results = stderr
            .lines()
            .filter_map(|l| l.strip_prefix("RESULT "))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Outcome {
            code,
            stdout: lines,
            stderr,
            results,
        }
    }
}

fn call() -> String {
    tool_call(1, "echo")
}

fn credentials_refused(outcome: &Outcome) {
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.stdout.len(), 1, "{:?}", outcome.stdout);
    assert!(
        outcome.stdout[0].contains("failed due to expired or invalid AWS credentials"),
        "{:?}",
        outcome.stdout
    );
    assert_eq!(outcome.get("posts"), "0", "no MCP transport call");
    for text in [outcome.stdout[0].as_str(), outcome.stderr.as_str()] {
        assert!(!text.contains(FALLBACK));
    }
    assert_eq!(outcome.get("dispatcher"), "false");
}

const MISSING_SOURCE: &str =
    "[profile dev]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = missing\n\
     [default]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = missing\n";
const SSO_INCOMPLETE: &str = "[profile dev]\nsso_session = dev\n[default]\nsso_session = dev\n\
     [sso-session dev]\nsso_region = us-east-1\nsso_start_url = https://example.awsapps.com/start\n";

#[test]
fn t_abs_genuine_absence_is_unsigned_only_with_skip_auth() {
    let outcome = Case::new(&["--skip-auth"]).run(&call());
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.get("posts"), "1");
    assert_eq!(outcome.get("signed"), "none");
    assert_eq!(outcome.get("build"), "DefaultChain");
    assert_eq!(outcome.get("recorder"), "");
    assert_eq!(outcome.get("dispatcher"), "false");
    // Without --skip-auth the same absence fails closed.
    let outcome = Case::new(&[]).run(&call());
    credentials_refused(&outcome);
}

#[test]
fn t_abs_controls_each_fallback_signs_on_its_own() {
    let outcome = Case::new(&["--skip-auth"]).ecs().run(&call());
    assert_eq!(outcome.get("signed"), FALLBACK, "{}", outcome.stderr);
    assert_eq!(outcome.get("build"), "Ecs");
    assert_eq!(outcome.get("recorder"), "GET 169.254.170.2/fallback-creds");
    let outcome = Case::new(&["--skip-auth"]).imds().run(&call());
    assert_eq!(outcome.get("signed"), FALLBACK, "{}", outcome.stderr);
    assert_eq!(outcome.get("build"), "DefaultChain");
}

#[test]
fn t_abs_5013_a_broken_source_profile_never_reaches_a_fallback() {
    for (selector, fallback) in [
        ("--profile", "ecs"),
        ("--profile", "imds"),
        ("env", "ecs"),
        ("default", "imds"),
    ] {
        for skip in [true, false] {
            let mut extra = Vec::new();
            if selector == "--profile" {
                extra.extend(["--profile", "dev"]);
            }
            if skip {
                extra.push("--skip-auth");
            }
            let mut case = Case::new(&extra).config(MISSING_SOURCE);
            if selector == "env" {
                case = case.env("AWS_PROFILE", "dev");
            }
            case = if fallback == "ecs" {
                case.ecs()
            } else {
                case.imds()
            };
            let outcome = case.run(&call());
            credentials_refused(&outcome);
            assert_eq!(
                outcome.get("build"),
                "Profile,Profile",
                "{selector} {fallback} {skip}"
            );
            assert_eq!(outcome.get("recorder"), "", "no request at all");
        }
    }
}

#[test]
fn t_abs_5013_b_an_incomplete_sso_profile_never_reaches_a_fallback() {
    for fallback in ["ecs", "imds"] {
        for skip in [true, false] {
            let extra: &[&str] = if skip {
                &["--profile", "dev", "--skip-auth"]
            } else {
                &["--profile", "dev"]
            };
            let case = Case::new(extra).config(SSO_INCOMPLETE);
            let case = if fallback == "ecs" {
                case.ecs()
            } else {
                case.imds()
            };
            let outcome = case.run(&call());
            credentials_refused(&outcome);
            assert_eq!(outcome.get("build"), "Profile,Profile");
            assert_eq!(outcome.get("recorder"), "");
        }
    }
}

#[test]
fn t_abs_typos_and_partial_configuration_build_nothing() {
    let profile = "[default]\naws_access_key_id = AKIDPROFILE0000000\naws_secret_access_key = s\n";
    for case in [
        Case::new(&["--skip-auth", "--profile", "typo"]).credentials(profile),
        Case::new(&["--skip-auth"])
            .env("AWS_PROFILE", "typo")
            .credentials(profile),
        Case::new(&["--skip-auth"])
            .env("AWS_ACCESS_KEY_ID", "AKIDENVONLY0000000")
            .credentials(profile),
        Case::new(&["--skip-auth"]).env("AWS_SESSION_TOKEN", "t"),
        Case::new(&["--skip-auth"]).env("AWS_ROLE_ARN", "arn:aws:iam::1:role/r"),
    ] {
        let outcome = case.run(&call());
        credentials_refused(&outcome);
        assert_eq!(outcome.get("build"), "");
    }
}

#[test]
fn t_abs_source_order_env_beats_profile() {
    let outcome = Case::new(&[])
        .env("AWS_ACCESS_KEY_ID", "AKIDENVPAIR0000000")
        .env("AWS_SECRET_ACCESS_KEY", "env-secret")
        .credentials(
            "[default]\naws_access_key_id = AKIDPROFILE0000000\naws_secret_access_key = s\n",
        )
        .run(&call());
    assert_eq!(
        outcome.get("signed"),
        "AKIDENVPAIR0000000",
        "{}",
        outcome.stderr
    );
    assert_eq!(outcome.get("build"), "Env");
    assert_eq!(
        outcome.get("recorder"),
        "",
        "T-PROV: no destination for env"
    );
}

#[test]
fn t_prov_traffic_never_reconfigures_the_provider_plane() {
    // The client's message and the server's 403 carry profile, region, role
    // and URL values; the D1 rebuild uses the startup profile only.
    let line = request(
        1,
        "tools/call",
        r#"{"name":"echo","arguments":{},"_meta":{"AWS_PROFILE":"evil","AWS_REGION":"eu-west-1","role_arn":"arn:aws:iam::1:role/x","url":"https://evil.example"}}"#,
    );
    let outcome = Case::new(&[])
        .env("FIXTURE_FIRST_403", "1")
        .credentials(
            "[default]\naws_access_key_id = AKIDPROVSTATIC0000\naws_secret_access_key = s\n\
                      [evil]\naws_access_key_id = AKIDEVIL0000000000\naws_secret_access_key = e\n",
        )
        .run(&line);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.get("posts"), "2", "the load and D1's one resend");
    assert_eq!(
        outcome.get("signed"),
        "AKIDPROVSTATIC0000,AKIDPROVSTATIC0000"
    );
    assert_eq!(outcome.get("build"), "Profile,Profile");
    assert_eq!(
        outcome.get("recorder"),
        "",
        "static profile: no destination"
    );
    assert!(
        !outcome.stderr.contains("evil") && !outcome.stdout.iter().any(|l| l.contains("AKIDEVIL"))
    );
}

fn canary() -> String {
    format!(
        "{:016x}{:016x}",
        RandomState::new().build_hasher().finish(),
        RandomState::new().build_hasher().finish()
    )
}

#[test]
fn t_leak_sdk_a_credential_process_stderr_canary_never_appears() {
    let exe = std::env::current_exe().unwrap();
    let process = format!("'{}' --exact {FIXTURE_ENTRY} --nocapture -q", exe.display());
    let direct = format!("[profile dev]\ncredential_process = {process}\n");
    let chained = format!(
        "[profile dev]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = base\n\
         [profile base]\ncredential_process = {process}\n"
    );
    for config in [direct, chained] {
        for rust_log in ["trace", "aws_config=trace", "debug"] {
            let secret = canary();
            let outcome = Case::new(&["--profile", "dev", "--log-level", "DEBUG", "--skip-auth"])
                .env("RUST_LOG", rust_log)
                .env("FIXTURE_PROCESS_CANARY", &secret)
                .config(&config)
                .run(&call());
            credentials_refused(&outcome);
            assert!(
                !outcome.stderr.contains(&secret),
                "{rust_log}: {}",
                outcome.stderr
            );
            assert!(!outcome.stdout.iter().any(|l| l.contains(&secret)));
            assert!(
                outcome.stderr.contains("credential-failure kind="),
                "{}",
                outcome.stderr
            );
            assert_eq!(outcome.get("dispatcher"), "false", "T-LEAK-GATE");
        }
    }
}

#[test]
fn unusable_roots_refuse_the_start_before_stdin_is_read() {
    let outcome = Case::new(&["--skip-auth"])
        .env("SSL_CERT_FILE", "/nonexistent/toolkit-mcp-proxy-roots.pem")
        .run(&call());
    assert_eq!(outcome.code, Some(2));
    assert!(outcome.stdout.is_empty());
    assert!(outcome.stderr.contains(
        "toolkit mcp-proxy: TLS trust roots are unusable (configured-location-error); \
         fix or unset SSL_CERT_FILE/SSL_CERT_DIR\n"
    ));
    assert_eq!(outcome.get("posts"), "0");
    assert!(!outcome.stderr.contains("panicked"));
}
