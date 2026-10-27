//! Offline tests for the credential chain. The AWS CLI is a scripted fake; the
//! key material below is synthetic.

use std::cell::RefCell;
use std::collections::VecDeque;

use super::*;
use toolkit_aws::{MapEnv, SigningScope};

type Script = VecDeque<Result<CommandOutput, RunFailure>>;

/// Replays scripted outcomes and records the command lines it was asked for.
#[derive(Default)]
struct FakeRunner {
    script: RefCell<Script>,
    calls: RefCell<Vec<String>>,
}

impl FakeRunner {
    fn with(outcomes: Vec<Result<CommandOutput, RunFailure>>) -> Rc<Self> {
        Rc::new(Self {
            script: RefCell::new(outcomes.into()),
            calls: RefCell::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for FakeRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, RunFailure> {
        assert_eq!(timeout, CLI_TIMEOUT);
        self.calls
            .borrow_mut()
            .push(format!("{program} {}", args.join(" ")));
        self.script
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(RunFailure::NotStarted))
    }
}

fn ok(stdout: &str) -> Result<CommandOutput, RunFailure> {
    Ok(CommandOutput {
        success: true,
        stdout: stdout.to_string(),
    })
}

fn exit_failure() -> Result<CommandOutput, RunFailure> {
    Ok(CommandOutput {
        success: false,
        stdout: String::new(),
    })
}

fn env(pairs: &[(&str, &str)]) -> Rc<dyn Env> {
    let mut map = MapEnv::new();
    for (name, value) in pairs {
        map = map.with(name, value);
    }
    Rc::new(map)
}

fn runner_dyn(runner: &Rc<FakeRunner>) -> Rc<dyn CommandRunner> {
    Rc::clone(runner) as Rc<dyn CommandRunner>
}

#[test]
fn environment_credentials_win_and_start_no_process() {
    let runner = FakeRunner::with(vec![]);
    let chain = credential_chain(
        &env(&[
            ("AWS_ACCESS_KEY_ID", "AKIDSYNTHETIC"),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-secret"),
            ("AWS_SESSION_TOKEN", "synthetic-token"),
        ]),
        &runner_dyn(&runner),
    );
    let credentials = chain.load().unwrap();
    assert_eq!(credentials.access_key_id().expose(), "AKIDSYNTHETIC");
    assert_eq!(credentials.secret_access_key().expose(), "synthetic-secret");
    assert_eq!(
        credentials.session_token().unwrap().expose(),
        "synthetic-token"
    );
    assert!(runner.calls().is_empty());
    assert_eq!(
        chain.names(),
        [
            "environment",
            "aws-cli-export-credentials",
            "aws-cli-configure-get"
        ]
    );
}

#[test]
fn an_access_key_without_a_secret_is_partial_and_stops_the_chain() {
    let runner = FakeRunner::with(vec![ok(
        "export AWS_ACCESS_KEY_ID=A\nexport AWS_SECRET_ACCESS_KEY=B",
    )]);
    let chain = credential_chain(
        &env(&[("AWS_ACCESS_KEY_ID", "AKIDSYNTHETIC")]),
        &runner_dyn(&runner),
    );
    let error = chain.load().unwrap_err();
    assert_eq!(error.code(), "aws_partial_credentials");
    assert!(runner.calls().is_empty());
}

#[test]
fn an_empty_access_key_in_the_environment_falls_through() {
    let runner = FakeRunner::with(vec![ok("AWS_ACCESS_KEY_ID=A\nAWS_SECRET_ACCESS_KEY=B\n")]);
    let chain = credential_chain(&env(&[("AWS_ACCESS_KEY_ID", "")]), &runner_dyn(&runner));
    assert_eq!(chain.load().unwrap().access_key_id().expose(), "A");
}

#[test]
fn cli_v2_export_is_parsed_with_or_without_export_prefix() {
    let runner = FakeRunner::with(vec![ok(
        "export AWS_ACCESS_KEY_ID=ASIASYNTHETIC\nAWS_SECRET_ACCESS_KEY=synthetic-secret\nexport AWS_SESSION_TOKEN=synthetic=token==\nAWS_CREDENTIAL_EXPIRATION=2030-01-01T00:00:00Z\nnoise line\n",
    )]);
    let chain = credential_chain(&env(&[]), &runner_dyn(&runner));
    let credentials = chain.load().unwrap();
    assert_eq!(credentials.access_key_id().expose(), "ASIASYNTHETIC");
    assert_eq!(credentials.secret_access_key().expose(), "synthetic-secret");
    // Only the first `=` splits a line.
    assert_eq!(
        credentials.session_token().unwrap().expose(),
        "synthetic=token=="
    );
    assert_eq!(
        runner.calls(),
        ["aws configure export-credentials --format env"]
    );
}

#[test]
fn cli_v1_static_keys_are_read_one_setting_at_a_time() {
    let runner = FakeRunner::with(vec![
        exit_failure(),
        ok("AKIDSYNTHETIC\n"),
        ok("  synthetic-secret \n"),
        exit_failure(),
    ]);
    let chain = credential_chain(&env(&[]), &runner_dyn(&runner));
    let credentials = chain.load().unwrap();
    assert_eq!(credentials.access_key_id().expose(), "AKIDSYNTHETIC");
    assert_eq!(credentials.secret_access_key().expose(), "synthetic-secret");
    assert!(credentials.session_token().is_none());
    assert_eq!(
        runner.calls(),
        [
            "aws configure export-credentials --format env",
            "aws configure get aws_access_key_id",
            "aws configure get aws_secret_access_key",
            "aws configure get aws_session_token",
        ]
    );
}

#[test]
fn missing_cli_timeouts_and_empty_answers_mean_no_credentials() {
    for script in [
        vec![Err(RunFailure::NotStarted), Err(RunFailure::NotStarted)],
        vec![Err(RunFailure::TimedOut), Err(RunFailure::TimedOut)],
        vec![ok("garbage"), ok("")],
        vec![exit_failure(), exit_failure()],
        // CLI v1 found a key but then timed out reading the secret: abandoned.
        vec![
            exit_failure(),
            ok("AKIDSYNTHETIC"),
            Err(RunFailure::TimedOut),
        ],
    ] {
        let runner = FakeRunner::with(script);
        let chain = credential_chain(&env(&[]), &runner_dyn(&runner));
        assert_eq!(chain.load().unwrap_err().code(), "aws_no_credentials");
    }
}

#[test]
fn credential_and_output_types_never_print_values() {
    let runner = FakeRunner::with(vec![ok(
        "AWS_ACCESS_KEY_ID=ASIASYNTHETIC\nAWS_SECRET_ACCESS_KEY=synthetic-secret\n",
    )]);
    let chain = credential_chain(&env(&[]), &runner_dyn(&runner));
    let credentials = chain.load().unwrap();
    for text in [
        format!("{credentials:?}"),
        format!("{chain:?}"),
        format!("{:?}", ok("synthetic-secret")),
    ] {
        assert!(!text.contains("synthetic-secret"), "{text}");
        assert!(!text.contains("ASIASYNTHETIC"), "{text}");
    }
}

fn scope() -> SigningScope {
    SigningScope {
        region: "us-east-1".to_string(),
        service: "aws-mcp".to_string(),
    }
}

fn request() -> HttpRequest {
    HttpRequest {
        method: toolkit_aws::Method::Post,
        url: "https://aws-mcp.us-east-1.api.aws/mcp".to_string(),
        headers: toolkit_aws::Headers::new(),
        body: Vec::new(),
        timeouts: toolkit_aws::Timeouts::default(),
    }
}

#[test]
fn the_unavailable_signer_reports_missing_credentials_first_then_fails_closed() {
    let runner = FakeRunner::with(vec![]);
    let none = credential_chain(&env(&[]), &runner_dyn(&runner));
    let signer = UnavailableSigner::new(&none);
    let mut unsigned = request();
    assert_eq!(
        signer.sign(&mut unsigned, &scope()).unwrap_err(),
        AwsError::NoCredentials
    );

    let some = credential_chain(
        &env(&[
            ("AWS_ACCESS_KEY_ID", "AKIDSYNTHETIC"),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-secret"),
        ]),
        &runner_dyn(&runner),
    );
    let signer = UnavailableSigner::new(&some);
    let error = signer.sign(&mut unsigned, &scope()).unwrap_err();
    assert_eq!(error.code(), "aws_credential_provider");
    assert!(
        error.to_string().contains("not available in this build"),
        "{error}"
    );
    // Nothing was added to the request: no signature, no credential header.
    assert!(unsigned.headers.is_empty());
    assert!(!error.to_string().contains("synthetic-secret"));
}

#[test]
fn the_unavailable_transport_always_fails() {
    let error = UnavailableTransport.send(&request()).unwrap_err();
    assert_eq!(error.code(), "aws_connect");
    assert!(
        error.to_string().contains("not available in this build"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn the_system_runner_captures_stdout_reports_status_and_times_out() {
    let runner = SystemRunner;
    let output = runner
        .run("/bin/sh", &["-c", "printf abc"], Duration::from_secs(5))
        .unwrap();
    assert!(output.success);
    assert_eq!(output.stdout, "abc");

    let failed = runner
        .run(
            "/bin/sh",
            &["-c", "printf out; exit 3"],
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(!failed.success);

    assert_eq!(
        runner.run(
            "/nonexistent/asm-exec-test-program",
            &[],
            Duration::from_secs(5)
        ),
        Err(RunFailure::NotStarted)
    );
    assert_eq!(
        runner.run("/bin/sh", &["-c", "sleep 30"], Duration::from_millis(200)),
        Err(RunFailure::TimedOut)
    );
}
