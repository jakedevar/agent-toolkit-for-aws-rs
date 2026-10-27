//! Offline tests for `asm exec`: every collaborator is a fake, so nothing
//! reaches AWS, DNS, the local agent or the real environment. Secret values are
//! synthetic and every scenario asserts that they appear only in what the
//! launcher is handed.

use std::cell::RefCell;
use std::io;

use super::*;
use toolkit_aws::testing::{FakeSigner, FixedResolver, MockTransport};
use toolkit_aws::{AwsError, MapEnv};

use super::process::ExitCode;

const VALUE: &str = "synthetic-secret-Zq9x";
const CLEAN_MCP_REPLY: &str = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;

/// What the launcher was asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Launch {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

struct FakeLauncher {
    launches: RefCell<Vec<Launch>>,
    outcome: RefCell<Option<io::Result<ExitCode>>>,
}

impl FakeLauncher {
    fn exits(code: u8) -> Self {
        Self {
            launches: RefCell::new(Vec::new()),
            outcome: RefCell::new(Some(Ok(ExitCode(code)))),
        }
    }

    fn fails(kind: io::ErrorKind) -> Self {
        Self {
            launches: RefCell::new(Vec::new()),
            outcome: RefCell::new(Some(Err(io::Error::from(kind)))),
        }
    }
}

impl Launcher for FakeLauncher {
    fn run(
        &self,
        program: &OsString,
        args: &[OsString],
        env: &[(OsString, OsString)],
    ) -> io::Result<ExitCode> {
        self.launches.borrow_mut().push(Launch {
            program: program.clone(),
            args: args.to_vec(),
            env: env.to_vec(),
        });
        self.outcome.borrow_mut().take().unwrap_or(Ok(ExitCode(0)))
    }
}

struct Outcome {
    status: u8,
    stdout: String,
    stderr: String,
}

struct World {
    env: MapEnv,
    process_env: Vec<(OsString, OsString)>,
    launcher: FakeLauncher,
    sma: MockTransport,
    mcp: MockTransport,
    signer: FakeSigner,
    resolver: FixedResolver,
}

impl World {
    fn new() -> Self {
        Self {
            env: MapEnv::new(),
            process_env: Vec::new(),
            launcher: FakeLauncher::exits(0),
            sma: MockTransport::new(),
            mcp: MockTransport::new(),
            signer: FakeSigner::new(),
            resolver: FixedResolver::new().answer_any(&["3.5.140.2"]),
        }
    }

    fn setting(mut self, name: &str, value: &str) -> Self {
        self.env = self.env.with(name, value);
        self
    }

    fn exported(mut self, name: &str, value: &str) -> Self {
        self.process_env
            .push((OsString::from(name), OsString::from(value)));
        self
    }

    fn agent(mut self, transport: MockTransport) -> Self {
        self.sma = transport;
        self
    }

    fn mcp(mut self, transport: MockTransport) -> Self {
        self.mcp = transport;
        self
    }

    fn launcher(mut self, launcher: FakeLauncher) -> Self {
        self.launcher = launcher;
        self
    }

    fn run(&self, args: &[&str]) -> Outcome {
        let deps = Deps {
            env: &self.env,
            process_env: self.process_env.clone(),
            launcher: &self.launcher,
            sma_transport: &self.sma,
            mcp_transport: &self.mcp,
            signer: &self.signer,
            resolver: &self.resolver,
        };
        let words: Vec<String> = args.iter().map(ToString::to_string).collect();
        let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let status = {
            let mut output = Output::with_diagnostics(&mut out, &mut err);
            run_with(&deps, &words, &mut output);
            output.finish().unwrap();
            output.exit_status()
        };
        Outcome {
            status,
            stdout: String::from_utf8(out).unwrap(),
            stderr: String::from_utf8(err).unwrap(),
        }
    }

    fn launches(&self) -> Vec<Launch> {
        self.launcher.launches.borrow().clone()
    }
}

fn json_string(value: &str) -> String {
    Json::string(value).to_compact_string()
}

/// A healthy agent: a ping, then one `{"SecretString": ...}` answer per value.
fn agent_with(values: &[&str]) -> MockTransport {
    values.iter().fold(
        MockTransport::new().respond(200, "ok"),
        |transport, value| {
            transport.respond(
                200,
                &format!(r#"{{"SecretString":{}}}"#, json_string(value)),
            )
        },
    )
}

/// MCP replies for one secret: initialize, initialized, tools/call.
fn mcp_with(value: &str) -> MockTransport {
    let envelope = format!(
        r#"{{"status":"success","return_value":{{"SecretString":{}}}}}"#,
        json_string(value)
    );
    let reply = format!(
        r#"{{"result":{{"content":[{{"type":"text","text":{}}}]}}}}"#,
        json_string(&envelope)
    );
    MockTransport::new()
        .respond(200, CLEAN_MCP_REPLY)
        .respond(202, "")
        .respond(200, &reply)
}

fn os(text: &str) -> OsString {
    OsString::from(text)
}

fn assert_clean(outcome: &Outcome, secrets: &[&str]) {
    for secret in secrets {
        assert!(
            !outcome.stdout.contains(secret),
            "stdout leaked: {}",
            outcome.stdout
        );
        assert!(
            !outcome.stderr.contains(secret),
            "stderr leaked: {}",
            outcome.stderr
        );
    }
    assert_eq!(outcome.stdout, "", "asm exec writes nothing to stdout");
}

#[test]
fn no_command_prints_the_usage_line_and_exits_one() {
    for args in [&[][..], &["--"][..]] {
        let world = World::new();
        let outcome = world.run(args);
        assert_eq!(outcome.status, 1);
        assert_eq!(outcome.stderr, "Usage: asm-exec <command> [args...]\n");
        assert_eq!(outcome.stdout, "");
        assert!(world.launches().is_empty());
    }
}

#[test]
fn only_one_leading_double_dash_is_dropped_and_other_words_pass_verbatim() {
    let world = World::new().launcher(FakeLauncher::exits(0));
    let outcome = world.run(&["--", "echo", "--", "-n", "plain"]);
    assert_eq!(outcome.status, 0);
    let launches = world.launches();
    let launch = &launches[0];
    assert_eq!(launch.program, os("echo"));
    assert_eq!(launch.args, [os("--"), os("-n"), os("plain")]);

    let world = World::new();
    world.run(&["echo", "--", "x"]);
    assert_eq!(world.launches()[0].args, [os("--"), os("x")]);
}

#[test]
fn a_command_without_references_runs_without_any_lookup() {
    let world = World::new()
        .launcher(FakeLauncher::exits(7))
        .exported("PATH", "/usr/bin")
        .exported("HOME", "/home/x");
    let outcome = world.run(&["tool", "a", "{{resolve:other:thing}}"]);
    assert_eq!(outcome.status, 7, "the child's status is the exit status");
    assert_eq!(outcome.stderr, "");
    assert!(world.sma.requests().is_empty());
    assert!(world.mcp.requests().is_empty());
    let launches = world.launches();
    let launch = &launches[0];
    assert_eq!(launch.args, [os("a"), os("{{resolve:other:thing}}")]);
    assert_eq!(
        launch.env,
        [(os("PATH"), os("/usr/bin")), (os("HOME"), os("/home/x"))]
    );
}

#[test]
fn references_in_arguments_and_the_environment_reach_only_the_child() {
    let world = World::new()
        .agent(agent_with(&[VALUE, "second-synthetic", VALUE]))
        .exported("DB_PASSWORD", "{{resolve:secretsmanager:prod/db}}")
        .exported("PLAIN", "unchanged");
    let outcome = world.run(&[
        "psql",
        "password={{resolve:secretsmanager:prod/db}} x",
        "-p{{resolve:secretsmanager:other}}",
    ]);
    assert_eq!(outcome.status, 0);
    assert_clean(&outcome, &[VALUE, "second-synthetic"]);
    assert_eq!(outcome.stderr, "");
    let launches = world.launches();
    let launch = &launches[0];
    assert_eq!(launch.program, os("psql"));
    assert_eq!(
        launch.args,
        [os(&format!("password={VALUE} x")), os("-psecond-synthetic")]
    );
    assert_eq!(
        launch.env,
        [
            (os("DB_PASSWORD"), os(VALUE)),
            (os("PLAIN"), os("unchanged"))
        ]
    );
}

#[test]
fn environment_values_are_resolved_after_arguments_and_names_are_kept() {
    let world = World::new()
        .agent(agent_with(&[VALUE]))
        .exported("TOKEN", "Bearer {{resolve:secretsmanager:api}}")
        .exported("PLAIN", "unchanged");
    let outcome = world.run(&["env"]);
    assert_eq!(outcome.status, 0);
    assert_clean(&outcome, &[VALUE]);
    let launches = world.launches();
    let launch = &launches[0];
    assert_eq!(
        launch.env,
        [
            (os("TOKEN"), os(&format!("Bearer {VALUE}"))),
            (os("PLAIN"), os("unchanged")),
        ]
    );
}

#[test]
fn the_agent_is_pinged_once_and_each_reference_is_fetched_separately() {
    let world = World::new().agent(agent_with(&["one", "two", "one"]));
    world.run(&[
        "t",
        "{{resolve:secretsmanager:a}}",
        "{{resolve:secretsmanager:b}}{{resolve:secretsmanager:a}}",
    ]);
    let urls: Vec<String> = world.sma.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        urls,
        [
            "http://localhost:2773/ping",
            "http://localhost:2773/secretsmanager/get?secretId=a&versionStage=AWSCURRENT",
            "http://localhost:2773/secretsmanager/get?secretId=b&versionStage=AWSCURRENT",
            "http://localhost:2773/secretsmanager/get?secretId=a&versionStage=AWSCURRENT",
        ]
    );
    assert_eq!(world.launches()[0].args, [os("one"), os("twoone")]);
}

#[test]
fn a_resolved_value_is_never_scanned_for_references() {
    let world = World::new().agent(agent_with(&["{{resolve:secretsmanager:other}}"]));
    world.run(&["t", "{{resolve:secretsmanager:a}}"]);
    assert_eq!(
        world.launches()[0].args,
        [os("{{resolve:secretsmanager:other}}")]
    );
    assert_eq!(world.sma.requests().len(), 2, "ping and one get");
}

#[test]
fn json_keys_select_members_and_non_strings_are_dumped_like_python() {
    let secret = r#"{"user":"u","pass":"p w","n":5,"b":true,"z":null,"o":{"k":[1,2.5]},"f":1e16}"#;
    let refs = [
        ("user", "u"),
        ("pass", "p w"),
        ("n", "5"),
        ("b", "true"),
        ("z", "null"),
        ("o", r#"{"k": [1, 2.5]}"#),
        ("f", "1e+16"),
    ];
    for (key, expected) in refs {
        let world = World::new().agent(agent_with(&[secret]));
        let outcome = world.run(&[
            "t",
            &format!("{{{{resolve:secretsmanager:db:SecretString:{key}}}}}"),
        ]);
        assert_eq!(outcome.status, 0, "{key}: {}", outcome.stderr);
        assert_eq!(world.launches()[0].args, [os(expected)], "{key}");
    }
}

#[test]
fn a_missing_json_key_or_unparseable_secret_fails_without_printing_the_value() {
    for secret in [
        r#"{"user":"u"}"#,
        "plain-not-json-synthetic",
        "[1,2]",
        r#""str""#,
    ] {
        let world = World::new().agent(agent_with(&[secret]));
        let outcome = world.run(&["t", "{{resolve:secretsmanager:db:SecretString:nope}}"]);
        assert_eq!(outcome.status, 1, "{secret}");
        assert_eq!(
            outcome.stderr,
            "asm-exec: ERROR: JSON key 'nope' not found in: db\n"
        );
        assert_clean(&outcome, &[secret]);
        assert!(world.launches().is_empty());
    }
}

#[test]
fn only_secret_string_is_supported_and_nothing_is_contacted() {
    let world = World::new().agent(agent_with(&[VALUE]));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:n:SecretBinary}}"]);
    assert_eq!(outcome.status, 1);
    assert_eq!(
        outcome.stderr,
        "asm-exec: ERROR: Only SecretString is supported, got: SecretBinary\n"
    );
    assert!(world.sma.requests().is_empty());
    assert!(world.launches().is_empty());
}

#[test]
fn an_unresolvable_reference_stops_everything_before_the_command_starts() {
    // Agent down, MCP unreachable.
    let world = World::new()
        .agent(MockTransport::new().fail(AwsError::connect("refused")))
        .mcp(MockTransport::new().fail(AwsError::connect("no route")));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:prod/db}}"]);
    assert_eq!(outcome.status, 1);
    assert_eq!(
        outcome.stderr,
        "asm-exec: MCP resolution failed: cannot reach MCP endpoint: no route\n\
         asm-exec: ERROR: Failed to resolve: prod/db\n"
    );
    assert!(world.launches().is_empty());
}

#[test]
fn a_later_environment_failure_also_prevents_the_launch() {
    let world = World::new()
        .agent(agent_with(&[VALUE, VALUE]))
        .exported("OK", "{{resolve:secretsmanager:a}}")
        .exported("BAD", "{{resolve:secretsmanager:b}}")
        .mcp(MockTransport::new().fail(AwsError::connect("no route")));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:a}}"]);
    assert_eq!(outcome.status, 1);
    assert!(
        outcome
            .stderr
            .ends_with("asm-exec: ERROR: Failed to resolve: b\n"),
        "{}",
        outcome.stderr
    );
    assert_clean(&outcome, &[VALUE]);
    assert!(world.launches().is_empty());
}

#[test]
fn diagnostics_are_scrubbed_before_printing() {
    let world = World::new()
        .agent(MockTransport::new().fail(AwsError::connect("refused")))
        .mcp(MockTransport::new().respond(
            200,
            r#"{"error":{"message":"denied for password=hunter2zzz"}}"#,
        ));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:password=hunter2yyy}}"]);
    assert_eq!(outcome.status, 1);
    assert!(!outcome.stderr.contains("hunter2"), "{}", outcome.stderr);
    assert!(outcome.stderr.contains("[REDACTED]"), "{}", outcome.stderr);
    assert!(
        outcome.stderr.contains("Failed to resolve"),
        "{}",
        outcome.stderr
    );
    // One line per message: control characters in remote text are flattened.
    let world = World::new()
        .agent(MockTransport::new().fail(AwsError::connect("refused")))
        .mcp(MockTransport::new().respond(
            200,
            "{\"error\":{\"message\":\"line1\\nline2\\u001b[31m\"}}",
        ));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:x}}"]);
    assert_eq!(outcome.stderr.lines().count(), 2, "{:?}", outcome.stderr);
    assert!(!outcome.stderr.contains('\u{1b}'));
}

#[test]
fn a_down_agent_falls_back_to_the_mcp_endpoint() {
    for ping in [
        MockTransport::new().respond(500, ""),
        MockTransport::new().fail(AwsError::connect("refused")),
    ] {
        let world = World::new().agent(ping).mcp(mcp_with(VALUE));
        let outcome = world.run(&["t", "{{resolve:secretsmanager:prod/db}}"]);
        assert_eq!(outcome.status, 0, "{}", outcome.stderr);
        assert_clean(&outcome, &[VALUE]);
        assert_eq!(world.launches()[0].args, [os(VALUE)]);
        assert_eq!(world.sma.requests().len(), 1, "only the ping");
        assert_eq!(world.mcp.requests().len(), 3);
    }
}

#[test]
fn an_agent_that_has_no_value_falls_back_to_the_mcp_endpoint() {
    for get in [
        MockTransport::new().respond(200, "ok").respond(404, ""),
        MockTransport::new()
            .respond(200, "ok")
            .respond(200, r#"{"SecretString":""}"#),
        MockTransport::new()
            .respond(200, "ok")
            .respond(200, "not json"),
    ] {
        let world = World::new().agent(get).mcp(mcp_with(VALUE));
        let outcome = world.run(&["t", "{{resolve:secretsmanager:n}}"]);
        assert_eq!(outcome.status, 0, "{}", outcome.stderr);
        assert_eq!(world.launches()[0].args, [os(VALUE)]);
    }
}

#[test]
fn the_secrets_region_comes_from_the_arn_then_the_environment() {
    let arn = "arn:aws:secretsmanager:us-west-2:123456789012:secret:prod/db-AbCdEf";
    let world = World::new()
        .setting("AWS_REGION", "eu-west-1")
        .agent(MockTransport::new().respond(500, ""))
        .mcp(mcp_with(VALUE));
    world.run(&["t", &format!("{{{{resolve:secretsmanager:{arn}}}}}")]);
    let body = String::from_utf8(world.mcp.requests()[2].body.clone()).unwrap();
    assert!(body.contains("region_name='us-west-2'"), "{body}");
    assert!(body.contains(&format!("'SecretId': '{arn}'")), "{body}");

    let world = World::new()
        .setting("AWS_DEFAULT_REGION", "ap-south-1")
        .agent(MockTransport::new().respond(500, ""))
        .mcp(mcp_with(VALUE));
    world.run(&["t", "{{resolve:secretsmanager:plain}}"]);
    let body = String::from_utf8(world.mcp.requests()[2].body.clone()).unwrap();
    assert!(body.contains("region_name='ap-south-1'"), "{body}");

    let world = World::new()
        .agent(MockTransport::new().respond(500, ""))
        .mcp(mcp_with(VALUE));
    world.run(&["t", "{{resolve:secretsmanager:plain}}"]);
    let body = String::from_utf8(world.mcp.requests()[2].body.clone()).unwrap();
    assert!(!body.contains("region_name"), "{body}");
}

#[test]
fn the_agent_endpoint_and_token_follow_the_python_settings() {
    let world = World::new()
        .setting(
            "AWS_SECRETS_MANAGER_AGENT_ENDPOINT",
            "http://127.0.0.1:9999/",
        )
        .setting("AWS_SESSION_TOKEN", "session-token-1")
        .setting("AWS_TOKEN", "other-token")
        .agent(agent_with(&[VALUE]));
    world.run(&["t", "{{resolve:secretsmanager:n}}"]);
    let requests = world.sma.requests();
    assert_eq!(requests[0].url, "http://127.0.0.1:9999/ping");
    assert!(requests[0].headers.is_empty());
    assert_eq!(
        requests[1].headers.get("x-aws-parameters-secrets-token"),
        Some("session-token-1")
    );

    for (settings, expected) in [
        (
            vec![("AWS_TOKEN", "fallback-token")],
            Some("fallback-token"),
        ),
        (
            vec![("AWS_SESSION_TOKEN", ""), ("AWS_TOKEN", "fallback-token")],
            None,
        ),
        (vec![], None),
    ] {
        let mut world = World::new().agent(agent_with(&[VALUE]));
        for (name, value) in settings {
            world = world.setting(name, value);
        }
        world.run(&["t", "{{resolve:secretsmanager:n}}"]);
        assert_eq!(
            world.sma.requests()[1]
                .headers
                .get("x-aws-parameters-secrets-token"),
            expected
        );
    }
}

#[test]
fn an_agent_endpoint_that_is_not_loopback_is_never_contacted_and_never_gets_the_token() {
    for endpoint in [
        "http://169.254.169.254",
        "http://example.com:2773",
        "https://localhost:2773",
        "",
    ] {
        let world = World::new()
            .setting("AWS_SECRETS_MANAGER_AGENT_ENDPOINT", endpoint)
            .setting("AWS_SESSION_TOKEN", "session-token-1")
            .agent(agent_with(&["from-the-wrong-place"]))
            .mcp(mcp_with(VALUE));
        let outcome = world.run(&[
            "t",
            "{{resolve:secretsmanager:n}}",
            "{{resolve:secretsmanager:m}}",
        ]);
        assert!(world.sma.requests().is_empty(), "{endpoint}");
        assert!(
            outcome
                .stderr
                .starts_with("asm-exec: Secrets Manager Agent not used: "),
            "{}",
            outcome.stderr
        );
        assert_eq!(
            outcome.stderr.matches("not used").count(),
            1,
            "the notice is printed once"
        );
        for request in world.mcp.requests() {
            assert!(request
                .headers
                .get("x-aws-parameters-secrets-token")
                .is_none());
        }
    }
}

#[test]
fn the_mcp_endpoint_and_timeout_settings_apply_and_bad_ones_fail_closed() {
    let world = World::new()
        .setting(
            "ASM_EXEC_MCP_ENDPOINT",
            "https://aws-mcp.eu-west-1.api.aws/mcp",
        )
        .setting("ASM_EXEC_MCP_TIMEOUT", " 12.5 ")
        .agent(MockTransport::new().respond(500, ""))
        .mcp(mcp_with(VALUE));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:n}}"]);
    assert_eq!(outcome.status, 0, "{}", outcome.stderr);
    let requests = world.mcp.requests();
    let request = &requests[0];
    assert_eq!(request.url, "https://aws-mcp.eu-west-1.api.aws/mcp");
    assert_eq!(request.timeouts.read(), Duration::from_millis(12_500));
    assert_eq!(
        request.headers.get("authorization"),
        Some("FAKE-SIGNED eu-west-1/aws-mcp")
    );

    for bad in ["abc", "", "0", "-1", "nan", "inf", "3601", "1e999"] {
        let world = World::new().setting("ASM_EXEC_MCP_TIMEOUT", bad);
        let outcome = world.run(&["true"]);
        assert_eq!(outcome.status, 1, "{bad:?}");
        assert!(
            outcome.stderr.contains("ASM_EXEC_MCP_TIMEOUT"),
            "{}",
            outcome.stderr
        );
        assert!(world.launches().is_empty(), "{bad:?}");
    }

    let world = World::new()
        .setting(
            "ASM_EXEC_MCP_ENDPOINT",
            "http://aws-mcp.us-east-1.api.aws/mcp",
        )
        .agent(MockTransport::new().respond(500, ""));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:n}}"]);
    assert_eq!(outcome.status, 1);
    assert!(
        outcome
            .stderr
            .contains("MCP resolution failed: invalid ASM_EXEC_MCP_ENDPOINT"),
        "{}",
        outcome.stderr
    );
    assert!(world.mcp.requests().is_empty());
}

#[test]
fn an_empty_secret_does_not_resolve() {
    let world = World::new()
        .agent(agent_with(&[""]))
        .mcp(MockTransport::new().fail(AwsError::connect("no route")));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:n}}"]);
    assert_eq!(outcome.status, 1);
    assert!(world.launches().is_empty());
}

#[test]
fn a_launch_failure_exits_one_and_names_only_the_unresolved_word() {
    let world = World::new()
        .agent(agent_with(&["synthetic-prog"]))
        .launcher(FakeLauncher::fails(io::ErrorKind::NotFound));
    let outcome = world.run(&["{{resolve:secretsmanager:cmd}}", "x"]);
    assert_eq!(outcome.status, 1);
    assert_clean(&outcome, &["synthetic-prog"]);
    assert!(
        outcome
            .stderr
            .starts_with("asm-exec: ERROR: Failed to run {{resolve:secretsmanager:cmd}}: "),
        "{}",
        outcome.stderr
    );

    let world = World::new().launcher(FakeLauncher::fails(io::ErrorKind::PermissionDenied));
    let outcome = world.run(&["./nope"]);
    assert_eq!(outcome.status, 1);
    assert!(
        outcome
            .stderr
            .starts_with("asm-exec: ERROR: Failed to run ./nope: "),
        "{}",
        outcome.stderr
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_environment_values_are_preserved_and_may_hold_references() {
    use std::os::unix::ffi::OsStringExt;
    let mut raw = vec![0xff, b'-'];
    raw.extend_from_slice(b"{{resolve:secretsmanager:n}}");
    raw.push(0xfe);
    let mut world = World::new().agent(agent_with(&["v"]));
    world
        .process_env
        .push((os("BINARY"), OsString::from_vec(vec![0xff, 0xfe])));
    world
        .process_env
        .push((os("MIXED"), OsString::from_vec(raw)));
    let outcome = world.run(&["t"]);
    assert_eq!(outcome.status, 0, "{}", outcome.stderr);
    let launches = world.launches();
    let env = &launches[0].env;
    assert_eq!(env[0].1, OsString::from_vec(vec![0xff, 0xfe]));
    assert_eq!(env[1].1, OsString::from_vec(vec![0xff, b'-', b'v', 0xfe]));
}

#[test]
fn a_reference_that_is_not_utf8_fails_closed() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut raw = b"{{resolve:secretsmanager:".to_vec();
        raw.push(0xff);
        raw.extend_from_slice(b"}}");
        let mut world = World::new().agent(agent_with(&["v"]));
        world.process_env.push((os("BAD"), OsString::from_vec(raw)));
        let outcome = world.run(&["t"]);
        assert_eq!(outcome.status, 1);
        assert_eq!(
            outcome.stderr,
            "asm-exec: ERROR: A reference is not valid UTF-8\n"
        );
        assert!(world.launches().is_empty());
    }
}

#[test]
fn a_secret_that_cannot_be_an_argument_stops_at_the_launcher_not_in_a_log() {
    // A NUL byte cannot be passed to a process; the real launcher refuses it
    // (see process tests). Here the fake accepts it and the secret is still
    // absent from every diagnostic.
    let world = World::new().agent(agent_with(&["a\0b"]));
    let outcome = world.run(&["t", "{{resolve:secretsmanager:n}}"]);
    assert_clean(&outcome, &["a\0b"]);
    assert_eq!(world.launches()[0].args, [os("a\0b")]);
}
