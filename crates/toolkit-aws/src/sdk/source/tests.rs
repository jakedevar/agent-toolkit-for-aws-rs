//! T-ABS (with the #5013 recorder-backed ECS and IMDS canary fallbacks and
//! the R-SRC mutation check), T-PROV destinations and T-LOGIN (#4413 S1).
//!
//! Each case runs the **real** aws-config providers in a child process over a
//! cleared environment and synthetic profile files. The only provider-plane
//! HTTP client is the recorder, injected through the R-PROV seam: it answers
//! from fixtures or refuses with a dispatch failure, and records method, host
//! and path only. Nothing leaves the process.

use std::collections::hash_map::RandomState;
use std::collections::VecDeque;
use std::fmt;
use std::fmt::Write as _;
use std::hash::{BuildHasher, Hasher};
use std::process::Command;
use std::sync::{Arc, Mutex};

use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{
    HttpRequest as SdkHttpRequest, HttpResponse as SdkHttpResponse,
};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;

use super::*;
use crate::sdk::test_support::{child_case, child_command, report, run, Child, TempDir};

const ENTRY: &str = "sdk::source::tests::child_entry";
const CANARY: &str = "AKIAFALLBACKCANARY0";
const BARE_CANARY: &str = "barecredentialcanaryfortest";
const IMDS: &str = "169.254.169.254";
const ECS_HOST: &str = "169.254.170.2";
const LOGIN_ARN: &str = "arn:aws:iam::012345678901:user/Admin";

#[test]
fn required_credentials_rebuild_expired_and_never_use_unsigned_fallback() {
    use std::time::{Duration, UNIX_EPOCH};

    let now = UNIX_EPOCH + Duration::from_secs(100);
    let expired = Credentials::new("canary-key", "canary-secret", None, Some(now)).unwrap();
    let fresh = Credentials::new("fresh-key", "fresh-secret", None, None).unwrap();
    let mut calls = Vec::new();
    let result = required_credentials(
        || now,
        |rebuild| {
            calls.push(rebuild);
            RequestCredentials::Sign(if rebuild {
                fresh.clone()
            } else {
                expired.clone()
            })
        },
    )
    .unwrap();
    assert_eq!(result, fresh);
    assert_eq!(calls, [false, true]);

    let mut count = 0;
    let error = required_credentials(
        || now,
        |_| {
            count += 1;
            RequestCredentials::Sign(expired.clone())
        },
    )
    .unwrap_err();
    assert!(matches!(error, AwsError::CredentialsExpired));
    assert_eq!(count, 2);
    assert!(!format!("{error:?} {error}").contains("canary"));

    let mut ticks = [now - Duration::from_secs(1), now].into_iter();
    let error = required_credentials(
        || ticks.next().unwrap(),
        |_| RequestCredentials::Sign(expired.clone()),
    )
    .unwrap_err();
    assert!(matches!(error, AwsError::CredentialsExpired));

    let error = required_credentials(|| now, |_| RequestCredentials::Unsigned).unwrap_err();
    assert!(matches!(error, AwsError::NoCredentials));
    let error = required_credentials(
        || now,
        |_| RequestCredentials::Refused(CredentialRefusal::ProviderFailed),
    )
    .unwrap_err();
    assert!(matches!(error, AwsError::CredentialProvider { .. }));
    assert!(!format!("{error:?} {error}").contains("canary"));
}

// --- The recorder: the T-PROV provider-plane HTTP client. ---

#[derive(Clone)]
enum Reply {
    Ok(u16, Vec<(&'static str, &'static str)>, String),
    Dispatch,
    Timeout,
    Canary,
    /// Never answers within the test: the provider is stalled.
    Stall,
}

/// Scripted replies by `"METHOD host/path"`.
type Routes = Arc<Mutex<Vec<(String, VecDeque<Reply>)>>>;

#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<String>>>,
    routes: Routes,
}

impl fmt::Debug for Recorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Recorder")
    }
}

impl Recorder {
    /// Answer `"METHOD host/path"` with `replies` in turn (the last repeats).
    fn route(self, key: &str, replies: Vec<Reply>) -> Self {
        self.routes
            .lock()
            .unwrap()
            .push((key.to_string(), replies.into()));
        self
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl HttpConnector for Recorder {
    fn call(&self, request: SdkHttpRequest) -> HttpConnectorFuture {
        let uri: http_1x_uri::Uri = request.uri().parse().expect("request URI");
        let key = format!("{} {}{}", request.method(), uri.host(), uri.path());
        self.seen.lock().unwrap().push(key.clone());
        let reply = {
            let mut routes = self.routes.lock().unwrap();
            routes
                .iter_mut()
                .find(|(route, _)| *route == key)
                .and_then(|(_, replies)| {
                    if replies.len() > 1 {
                        replies.pop_front()
                    } else {
                        replies.front().cloned()
                    }
                })
        };
        let result = match reply {
            Some(Reply::Ok(status, headers, body)) => {
                let mut response = SdkHttpResponse::new(
                    StatusCode::try_from(status).expect("status"),
                    SdkBody::from(body),
                );
                for (name, value) in headers {
                    response.headers_mut().insert(name, value);
                }
                Ok(response)
            }
            Some(Reply::Timeout) => Err(ConnectorError::timeout("recorder timeout".into())),
            Some(Reply::Canary) => Err(ConnectorError::io(BARE_CANARY.into())),
            Some(Reply::Stall) => {
                return HttpConnectorFuture::new(async {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    Err(ConnectorError::io("recorder stalled".into()))
                });
            }
            Some(Reply::Dispatch) | None => {
                Err(ConnectorError::io("recorder refused the request".into()))
            }
        };
        HttpConnectorFuture::ready(result)
    }
}

impl HttpClient for Recorder {
    fn http_connector(
        &self,
        _settings: &HttpConnectorSettings,
        _components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}

/// Minimal URI parsing for the recorder key (`scheme://host[:port]/path`).
mod http_1x_uri {
    pub struct Uri {
        host: String,
        path: String,
    }

    impl std::str::FromStr for Uri {
        type Err = ();

        fn from_str(value: &str) -> Result<Self, ()> {
            let rest = value.split_once("://").ok_or(())?.1;
            let (authority, path) = match rest.find('/') {
                Some(index) => (&rest[..index], &rest[index..]),
                None => (rest, "/"),
            };
            let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
            let path = path.split('?').next().unwrap_or("/");
            Ok(Self {
                host: host.to_string(),
                path: path.to_string(),
            })
        }
    }

    impl Uri {
        pub fn host(&self) -> &str {
            &self.host
        }

        pub fn path(&self) -> &str {
            &self.path
        }
    }
}

fn credentials_json(key: &str) -> String {
    format!(
        "{{\"Code\":\"Success\",\"Type\":\"AWS-HMAC\",\"AccessKeyId\":\"{key}\",\
         \"SecretAccessKey\":\"fallback-secret\",\"Token\":\"fallback-token\",\
         \"Expiration\":\"2099-01-01T00:00:00Z\",\"LastUpdated\":\"2026-01-01T00:00:00Z\"}}"
    )
}

/// The recorder for this child, from its fixture variables.
fn recorder() -> Recorder {
    let mut recorder = Recorder::default();
    let fixture = |name: &str| std::env::var(name).ok();
    if fixture("FIXTURE_ECS").is_some() {
        recorder = recorder.route(
            &format!("GET {ECS_HOST}/fallback-creds"),
            vec![Reply::Ok(200, vec![], credentials_json(CANARY))],
        );
    }
    match fixture("FIXTURE_IMDS").as_deref() {
        Some("ok") => {
            recorder = recorder
                .route(
                    &format!("PUT {IMDS}/latest/api/token"),
                    vec![Reply::Ok(
                        200,
                        vec![("x-aws-ec2-metadata-token-ttl-seconds", "21600")],
                        "imds-token".to_string(),
                    )],
                )
                .route(
                    &format!("GET {IMDS}/latest/meta-data/iam/security-credentials/"),
                    vec![Reply::Ok(200, vec![], "fallback-role".to_string())],
                )
                .route(
                    &format!("GET {IMDS}/latest/meta-data/iam/security-credentials/fallback-role"),
                    vec![Reply::Ok(200, vec![], credentials_json(CANARY))],
                );
        }
        Some("refuse") => {
            recorder = recorder.route(
                &format!("PUT {IMDS}/latest/api/token"),
                vec![Reply::Dispatch],
            );
        }
        _ => {}
    }
    if fixture("FIXTURE_STS").as_deref() == Some("timeout") {
        recorder = recorder.route("POST sts.us-east-1.amazonaws.com/", vec![Reply::Timeout]);
    }
    if fixture("FIXTURE_STS").as_deref() == Some("stall") {
        recorder = recorder.route("POST sts.us-east-1.amazonaws.com/", vec![Reply::Stall]);
    }
    if fixture("FIXTURE_STS").as_deref() == Some("canary") {
        recorder = recorder.route("POST sts.us-east-1.amazonaws.com/", vec![Reply::Canary]);
    }
    match fixture("FIXTURE_SIGNIN").as_deref() {
        Some("ok") => {
            recorder = recorder.route(
                "POST us-east-1.signin.aws.amazon.com/v1/token",
                vec![Reply::Ok(
                    200,
                    vec![("content-type", "application/json")],
                    "{\"accessToken\":{\"accessKeyId\":\"AKIAREFRESHEDKEY0\",\
                     \"secretAccessKey\":\"refreshed-secret\",\"sessionToken\":\"refreshed-token\"},\
                     \"tokenType\":\"aws_sigv4\",\"expiresIn\":900,\"refreshToken\":\"refresh-2\"}"
                        .to_string(),
                )],
            );
        }
        Some("stall") => {
            recorder = recorder.route(
                "POST us-east-1.signin.aws.amazon.com/v1/token",
                vec![Reply::Stall],
            );
        }
        Some("fail") => {
            recorder = recorder.route(
                "POST us-east-1.signin.aws.amazon.com/v1/token",
                vec![
                    Reply::Ok(
                        400,
                        vec![("content-type", "application/json")],
                        "{\"error\":\"invalid_request\"}".to_string(),
                    ),
                    Reply::Dispatch,
                ],
            );
        }
        _ => {}
    }
    recorder
}

fn describe(outcome: &RequestCredentials) -> String {
    match outcome {
        RequestCredentials::Sign(_) => "sign".to_string(),
        RequestCredentials::Unsigned => "unsigned".to_string(),
        RequestCredentials::Refused(refusal) => format!("refused:{refusal:?}"),
    }
}

fn report_outcome(suffix: &str, outcome: &RequestCredentials) {
    report(&format!("decision{suffix}"), describe(outcome));
    if let RequestCredentials::Sign(credentials) = outcome {
        report(
            &format!("key{suffix}"),
            credentials.access_key_id().expose(),
        );
    }
}

fn rewrite(path_var: &str, contents_var: &str) {
    let path = std::env::var(path_var).expect("hook path");
    let contents = std::env::var(contents_var).unwrap_or_default();
    std::fs::write(path, contents).expect("hook rewrite");
}

/// The child: one guarded resolution (or two, for R-STICKY and recovery).
#[test]
fn child_entry() {
    let Some(case) = child_case() else {
        return;
    };
    assert_eq!(case, "resolve");
    let recorder = recorder();
    let runtime = SdkRuntime::new().expect("runtime");
    let cli_profile = std::env::var("CLI_PROFILE").ok();
    let skip_auth = std::env::var("SKIP_AUTH").is_ok();
    let mut guarded = GuardedCredentials::new(
        &runtime,
        "us-east-1",
        cli_profile.as_deref(),
        ProviderHttp::injected(recorder.clone()),
    )
    .with_build_recording();
    if std::env::var("MUTATE").is_ok() {
        guarded = guarded.with_selector(|_| Selection::Build(SourceKind::DefaultChain));
    }
    if std::env::var("HOOK_CONFIG").is_ok() {
        guarded =
            guarded.with_between_attempts(Box::new(|| rewrite("AWS_CONFIG_FILE", "HOOK_CONFIG")));
    }
    if let Ok(budget) = std::env::var("BOUNDED_MS") {
        let budget = std::time::Duration::from_millis(budget.parse().expect("BOUNDED_MS"));
        let start = Instant::now();
        let loaded = CredentialsProvider::load(&guarded.until(start + budget));
        report("elapsed_ms", start.elapsed().as_millis().to_string());
        match loaded {
            Ok(credentials) => {
                report("decision", "sign");
                report("key", credentials.access_key_id().expose());
            }
            Err(error) => {
                report("decision", error.code());
                report("diagnostic", format!("{error:?} {error}"));
            }
        }
    } else if std::env::var("REQUIRE_CREDENTIALS").is_ok() {
        match CredentialsProvider::load(&guarded) {
            Ok(_) => report("decision", "sign"),
            Err(error) => {
                report("decision", error.code());
                report("diagnostic", format!("{error:?} {error}"));
            }
        }
    } else {
        let first = guarded.resolve(skip_auth, false);
        report_outcome("", &first);
    }
    if std::env::var("THEN_CONFIG").is_ok() {
        rewrite("AWS_CONFIG_FILE", "THEN_CONFIG");
        report_outcome("2", &guarded.resolve(skip_auth, false));
    }
    if std::env::var("THEN_CACHE").is_ok() {
        std::fs::copy(
            std::env::var("THEN_CACHE").unwrap(),
            std::env::var("LOGIN_CACHE_FILE").unwrap(),
        )
        .expect("rewrite login cache");
        report_outcome("2", &guarded.resolve(skip_auth, false));
    }
    let log: Vec<String> = guarded
        .build_log()
        .iter()
        .map(ToString::to_string)
        .collect();
    report("log", log.join(","));
    report("dest", recorder.seen().join("|"));
}

// --- The parent side. ---

/// A synthetic launch environment.
struct Scenario {
    tmp: TempDir,
    vars: Vec<(String, String)>,
}

impl Scenario {
    fn new(config: &str, credentials: &str) -> Self {
        let tmp = TempDir::new("abs");
        let home = tmp.dir("home");
        let config_path = tmp.write("config", config);
        let credentials_path = tmp.write("credentials", credentials);
        let mut scenario = Self {
            tmp,
            vars: Vec::new(),
        };
        scenario.set("HOME", &home.display().to_string());
        scenario.set("AWS_CONFIG_FILE", &config_path.display().to_string());
        scenario.set(
            "AWS_SHARED_CREDENTIALS_FILE",
            &credentials_path.display().to_string(),
        );
        scenario.set("AWS_EC2_METADATA_DISABLED", "true");
        scenario
    }

    fn set(&mut self, name: &str, value: &str) {
        self.vars.retain(|(existing, _)| existing != name);
        self.vars.push((name.to_string(), value.to_string()));
    }

    fn unset(&mut self, name: &str) {
        self.vars.retain(|(existing, _)| existing != name);
    }

    fn with(mut self, name: &str, value: &str) -> Self {
        self.set(name, value);
        self
    }

    fn ecs_fallback(self) -> Self {
        self.with("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", "/fallback-creds")
            .with("FIXTURE_ECS", "1")
    }

    fn imds_fallback(mut self) -> Self {
        self.unset("AWS_EC2_METADATA_DISABLED");
        self.with("FIXTURE_IMDS", "ok")
    }

    fn run(&self) -> Child {
        let mut command: Command = child_command(ENTRY, "resolve");
        for (name, value) in &self.vars {
            command.env(name, value);
        }
        let outcome = run(&mut command);
        outcome.assert_ok();
        outcome
    }
}

/// Run with and without `--skip-auth`.
fn both(scenario: Scenario) -> [Child; 2] {
    let without = scenario.run();
    let with = scenario.with("SKIP_AUTH", "1").run();
    [without, with]
}

/// Run with and without `--skip-auth`, each on fresh files (for scenarios
/// whose hooks rewrite them).
fn both_fresh(make: impl Fn() -> Scenario) -> [Child; 2] {
    [make().run(), make().with("SKIP_AUTH", "1").run()]
}

fn assert_no_canary(outcome: &Child) {
    assert!(!outcome.stdout.contains(CANARY), "{}", outcome.stdout);
    assert!(!outcome.stderr.contains(CANARY), "{}", outcome.stderr);
}

#[test]
fn genuine_absence_is_unsigned_only_with_skip_auth() {
    for config in ["", "[default]\nregion = us-west-2\n"] {
        let [without, with] = both(Scenario::new(config, ""));
        assert_eq!(without.result("decision"), "refused:NoCredentials");
        assert_eq!(with.result("decision"), "unsigned");
        for outcome in [&without, &with] {
            assert_eq!(outcome.result("log"), "DefaultChain");
            assert_eq!(outcome.result("dest"), "", "metadata disabled: no request");
        }
    }
}

#[test]
fn required_guarded_source_refuses_fallback_and_hides_bare_canaries() {
    let role = "[default]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = base\n\
                [profile base]\naws_access_key_id = AKIABASE0\naws_secret_access_key = s\n";
    let process =
        format!("[default]\ncredential_process = sh -c 'echo {BARE_CANARY} >&2; exit 1'\n");
    let scenarios = [
        (Scenario::new("", ""), "aws_no_credentials", ""),
        (
            Scenario::new("", "").with("AWS_ACCESS_KEY_ID", "partial"),
            "aws_invalid_config",
            "",
        ),
        (
            Scenario::new("", "")
                .with("AWS_PROFILE", "missing")
                .ecs_fallback(),
            "aws_invalid_config",
            "",
        ),
        (
            Scenario::new(&process, "").ecs_fallback(),
            "aws_credential_provider",
            "",
        ),
        (
            Scenario::new(role, "")
                .ecs_fallback()
                .with("FIXTURE_STS", "canary"),
            "aws_credential_provider",
            "sts",
        ),
    ];
    for (scenario, expected, destination) in scenarios {
        let outcome = scenario
            .with("REQUIRE_CREDENTIALS", "1")
            .with("RUST_LOG", "trace")
            .run();
        assert_eq!(outcome.result("decision"), expected);
        assert!(!outcome.stdout.contains(BARE_CANARY));
        assert!(!outcome.stderr.contains(BARE_CANARY));
        assert_no_canary(&outcome);
        let destinations = outcome.result("dest");
        if destination.is_empty() {
            assert!(destinations.is_empty(), "{destinations}");
        } else {
            assert!(!destinations.is_empty());
            assert!(destinations
                .split('|')
                .all(|request| request == "POST sts.us-east-1.amazonaws.com/"));
        }
    }
}

#[test]
fn required_guarded_source_recovers_on_configured_rebuild() {
    let valid = "[default]\naws_access_key_id = AKIAFIXEDKEY0\naws_secret_access_key = s\n";
    let outcome = Scenario::new(&format!("[default]\n{SSO_INCOMPLETE}{SSO_SESSION}"), "")
        .with("HOOK_CONFIG", valid)
        .with("REQUIRE_CREDENTIALS", "1")
        .run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("log"), "Profile,Profile");
    assert_eq!(outcome.result("dest"), "");
    assert_no_canary(&outcome);
}

#[test]
fn an_unreachable_imds_is_not_loaded_and_only_its_token_was_requested() {
    let mut scenario = Scenario::new("", "").with("FIXTURE_IMDS", "refuse");
    scenario.unset("AWS_EC2_METADATA_DISABLED");
    let outcome = scenario.with("SKIP_AUTH", "1").run();
    assert_eq!(outcome.result("decision"), "unsigned");
    let dest = outcome.result("dest");
    assert!(!dest.is_empty());
    for request in dest.split('|') {
        assert_eq!(request, format!("PUT {IMDS}/latest/api/token"));
    }
}

#[test]
fn control_cases_prove_each_fallback_fixture_works() {
    // IMDS fixture under an Absent configuration.
    let outcome = Scenario::new("", "").imds_fallback().run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), CANARY);
    assert_eq!(outcome.result("log"), "DefaultChain");
    assert!(outcome
        .result("dest")
        .contains("PUT 169.254.169.254/latest/api/token"));
    // ECS fixture: the container variable alone is Configured row 4.
    let outcome = Scenario::new("", "").ecs_fallback().run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), CANARY);
    assert_eq!(outcome.result("log"), "Ecs");
    assert_eq!(
        outcome.result("dest"),
        format!("GET {ECS_HOST}/fallback-creds")
    );
}

const MISSING_SOURCE: &str =
    "role_arn = arn:aws:iam::123456789012:role/r\nsource_profile = missing\n";
const SSO_INCOMPLETE: &str = "sso_session = dev\n";
const SSO_SESSION: &str =
    "[sso-session dev]\nsso_region = us-east-1\nsso_start_url = https://example.awsapps.com/start\n";

/// The #5013 scenarios: (label, config, selection variables).
/// One #5013 scenario: label, config text, selection variables.
type Case5013 = (String, String, Vec<(&'static str, &'static str)>);

fn cases_5013() -> Vec<Case5013> {
    let mut cases = Vec::new();
    for (label, body, extra) in [
        ("a", MISSING_SOURCE, ""),
        ("b", SSO_INCOMPLETE, SSO_SESSION),
    ] {
        let config = format!("[profile dev]\n{body}{extra}");
        cases.push((
            format!("5013-{label} --profile"),
            config.clone(),
            vec![("CLI_PROFILE", "dev")],
        ));
        cases.push((
            format!("5013-{label} AWS_PROFILE"),
            config,
            vec![("AWS_PROFILE", "dev")],
        ));
    }
    cases.push((
        "5013-c implicit default".to_string(),
        format!("[default]\n{MISSING_SOURCE}"),
        Vec::new(),
    ));
    cases
}

fn scenario_5013(config: &str, vars: &[(&str, &str)], fallback: &str) -> Scenario {
    let mut scenario = Scenario::new(config, "");
    for (name, value) in vars {
        scenario.set(name, value);
    }
    if fallback == "ecs" {
        scenario.ecs_fallback()
    } else {
        scenario.imds_fallback()
    }
}

#[test]
fn issue_5013_a_configured_source_never_falls_through_to_a_fallback_identity() {
    for (label, config, vars) in cases_5013() {
        for fallback in ["ecs", "imds"] {
            for outcome in both(scenario_5013(&config, &vars, fallback)) {
                assert_eq!(
                    outcome.result("decision"),
                    "refused:ConfiguredSourceNotLoaded",
                    "{label} {fallback}"
                );
                // The load and the D1 rebuild, both the profile provider.
                assert_eq!(
                    outcome.result("log"),
                    "Profile,Profile",
                    "{label} {fallback}"
                );
                // No fallback destination, no STS, no SSO: nothing at all.
                assert_eq!(outcome.result("dest"), "", "{label} {fallback}");
                assert_no_canary(&outcome);
            }
        }
    }
}

#[test]
fn issue_5013_mutation_the_cases_detect_a_default_chain_selector() {
    // With the revision-2 defect (the default chain for a Configured class),
    // the same scenarios reach the fallback and sign with its identity.
    for (label, config, vars) in cases_5013() {
        for fallback in ["ecs", "imds"] {
            let outcome = scenario_5013(&config, &vars, fallback)
                .with("MUTATE", "1")
                .with("SKIP_AUTH", "1")
                .run();
            assert_eq!(outcome.result("decision"), "sign", "{label} {fallback}");
            assert_eq!(outcome.result("key"), CANARY, "{label} {fallback}");
            assert_eq!(outcome.result("log"), "DefaultChain", "{label} {fallback}");
            assert!(!outcome.result("dest").is_empty(), "{label} {fallback}");
        }
    }
}

#[test]
fn invalid_configurations_build_no_provider() {
    let static_default =
        "[default]\naws_access_key_id = AKIASTATICDEFAULT0\naws_secret_access_key = s\n";
    let scenarios = [
        Scenario::new("[profile real]\nregion = x\n", "").with("CLI_PROFILE", "typo"),
        Scenario::new("[profile real]\nregion = x\n", "").with("AWS_PROFILE", "typo"),
        Scenario::new(static_default, "").with("AWS_ACCESS_KEY_ID", "AKIAPARTIAL0"),
        Scenario::new("", "").with("AWS_SESSION_TOKEN", "token"),
        Scenario::new("", "").with("AWS_SECRET_ACCESS_KEY", "secret"),
        Scenario::new("", "").with("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/r"),
        Scenario::new("", "").with("AWS_CONFIG_FILE", "/nonexistent/toolkit/config"),
    ];
    for scenario in scenarios {
        for outcome in both(scenario.ecs_fallback()) {
            assert!(
                outcome
                    .result("decision")
                    .starts_with("refused:InvalidConfiguration"),
                "{}",
                outcome.stdout
            );
            assert_eq!(outcome.result("log"), "");
            assert_eq!(outcome.result("dest"), "");
            assert_no_canary(&outcome);
        }
    }
}

#[test]
fn configured_provider_failures_are_terminal_after_one_rebuild() {
    let cases = [
        // `sso_session` naming a missing section: InvalidConfiguration, level 2.
        ("[default]\nsso_session = absent\n", "refused:ProviderFailed"),
        // credential_process exits 1.
        ("[default]\ncredential_process = sh -c 'exit 1'\n", "refused:ProviderFailed"),
        // credential_process returns an Expiration in the past.
        (
            "[default]\ncredential_process = printf '{\"Version\":1,\"AccessKeyId\":\"AKIAEXPIRED0\",\"SecretAccessKey\":\"s\",\"SessionToken\":\"t\",\"Expiration\":\"2000-01-01T00:00:00Z\"}'\n",
            "refused:ProviderFailed",
        ),
    ];
    for (config, expected) in cases {
        for outcome in both(Scenario::new(config, "").ecs_fallback()) {
            assert_eq!(outcome.result("decision"), expected, "{config}");
            assert_eq!(outcome.result("log"), "Profile,Profile", "{config}");
            assert_eq!(outcome.result("dest"), "", "{config}");
            assert!(!outcome.stdout.contains("AKIAEXPIRED0"));
            assert_no_canary(&outcome);
        }
    }
}

#[test]
fn an_assume_role_whose_sts_call_times_out_fails_and_calls_only_sts() {
    let config = "[default]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = base\n\
                  [profile base]\naws_access_key_id = AKIABASE0\naws_secret_access_key = s\n";
    for outcome in both(
        Scenario::new(config, "")
            .ecs_fallback()
            .with("FIXTURE_STS", "timeout"),
    ) {
        assert_eq!(outcome.result("decision"), "refused:ProviderFailed");
        assert_eq!(outcome.result("log"), "Profile,Profile");
        let dest = outcome.result("dest");
        assert!(!dest.is_empty());
        for request in dest.split('|') {
            assert_eq!(request, "POST sts.us-east-1.amazonaws.com/", "{dest}");
        }
        assert_no_canary(&outcome);
    }
}

#[test]
fn source_order_follows_the_sdk_chain() {
    let static_default =
        "[default]\naws_access_key_id = AKIAPROFILEKEY0\naws_secret_access_key = s\n";
    let outcome = Scenario::new(static_default, "")
        .with("AWS_ACCESS_KEY_ID", "AKIAENVKEY0")
        .with("AWS_SECRET_ACCESS_KEY", "env-secret")
        .run();
    assert_eq!(outcome.result("key"), "AKIAENVKEY0");
    assert_eq!(outcome.result("log"), "Env");
    assert_eq!(outcome.result("dest"), "");

    let scenario = Scenario::new(static_default, "");
    let token = scenario.tmp.write("token", "web-identity-token");
    let outcome = scenario
        .with("AWS_WEB_IDENTITY_TOKEN_FILE", &token.display().to_string())
        .with("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/web")
        .run();
    assert_eq!(outcome.result("key"), "AKIAPROFILEKEY0");
    assert_eq!(outcome.result("log"), "Profile");
    assert_eq!(outcome.result("dest"), "", "no AssumeRoleWithWebIdentity");
}

#[test]
fn precedence_across_the_load_and_the_rebuild() {
    // Configured provider error, then the rebuild finds Absent: fails closed.
    for outcome in both_fresh(|| {
        Scenario::new("[default]\ncredential_process = sh -c 'exit 1'\n", "")
            .with("HOOK_CONFIG", "")
    }) {
        assert_eq!(outcome.result("decision"), "refused:ProviderFailed");
        assert_eq!(outcome.result("log"), "Profile,DefaultChain");
    }
    // Configured NotLoaded, then the rebuild succeeds: signed.
    let fixed = "[default]\naws_access_key_id = AKIAFIXEDKEY0\naws_secret_access_key = s\n";
    let outcome = Scenario::new(&format!("[default]\n{SSO_INCOMPLETE}{SSO_SESSION}"), "")
        .with("HOOK_CONFIG", fixed)
        .run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), "AKIAFIXEDKEY0");
    assert_eq!(outcome.result("log"), "Profile,Profile");
}

#[test]
fn a_signed_session_never_goes_anonymous() {
    let config = "[default]\naws_access_key_id = AKIASTICKY0\naws_secret_access_key = s\n";
    let outcome = Scenario::new(config, "")
        .with("SKIP_AUTH", "1")
        .with("THEN_CONFIG", "")
        .run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(
        outcome.result("decision2"),
        "refused:SignedSessionLostCredentials"
    );
}

// --- T-LOGIN. ---

/// A P-256 key generated now, as the SEC1 PEM the login cache holds.
fn dpop_key_pem() -> String {
    loop {
        let mut bytes = [0_u8; 32];
        for chunk in bytes.chunks_mut(8) {
            let random = RandomState::new().build_hasher().finish();
            chunk.copy_from_slice(&random.to_le_bytes());
        }
        if let Ok(key) = p256::SecretKey::from_slice(&bytes) {
            return key
                .to_sec1_pem(p256::pkcs8::LineEnding::LF)
                .expect("PEM")
                .to_string();
        }
    }
}

fn login_cache(key_id: &str, expires_at: &str, dpop: &str) -> String {
    let dpop = dpop.replace('\n', "\\n");
    format!(
        "{{\"accessToken\":{{\"accessKeyId\":\"{key_id}\",\"secretAccessKey\":\"login-secret\",\
         \"sessionToken\":\"login-token\",\"accountId\":\"012345678901\",\"expiresAt\":\"{expires_at}\"}},\
         \"tokenType\":\"aws_sigv4\",\"refreshToken\":\"refresh-1\",\
         \"clientId\":\"arn:aws:signin:::devtools/same-device\",\"dpopKey\":\"{dpop}\"}}"
    )
}

fn cache_name() -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(LOGIN_ARN.as_bytes());
    let hex = digest.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!("{hex}.json")
}

fn login_scenario(expires_at: &str, signin: &str) -> (Scenario, std::path::PathBuf) {
    let scenario = Scenario::new(&format!("[default]\nlogin_session = {LOGIN_ARN}\n"), "");
    let cache_dir = scenario.tmp.dir("login-cache");
    let cache = cache_dir.join(cache_name());
    std::fs::write(
        &cache,
        login_cache("AKIALOGINFIXTURE0", expires_at, &dpop_key_pem()),
    )
    .expect("cache");
    let scenario = scenario
        .with(
            "AWS_LOGIN_CACHE_DIRECTORY",
            &cache_dir.display().to_string(),
        )
        .with("LOGIN_CACHE_FILE", &cache.display().to_string())
        .with("FIXTURE_SIGNIN", signin);
    (scenario, cache)
}

#[test]
fn t_login_an_unexpired_cached_session_signs_without_calling_sign_in() {
    // This success is the proof that `credentials-login` is compiled in:
    // without it aws-config refuses a `login_session` profile.
    let (scenario, _) = login_scenario("2099-01-01T00:00:00Z", "none");
    let outcome = scenario.run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), "AKIALOGINFIXTURE0");
    assert_eq!(outcome.result("log"), "Profile");
    assert_eq!(outcome.result("dest"), "");
}

#[test]
fn t_login_an_expired_session_refreshes_through_sign_in_and_updates_the_cache() {
    let (scenario, cache) = login_scenario("2000-01-01T00:00:00Z", "ok");
    let outcome = scenario.run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), "AKIAREFRESHEDKEY0");
    assert_eq!(
        outcome.result("dest"),
        "POST us-east-1.signin.aws.amazon.com/v1/token"
    );
    let updated = std::fs::read_to_string(&cache).expect("cache");
    assert!(
        updated.contains("AKIAREFRESHEDKEY0"),
        "the cache file is updated"
    );
}

#[test]
fn t_login_a_refresh_failure_fails_closed_and_a_new_login_recovers() {
    let make = || {
        let (scenario, _) = login_scenario("2000-01-01T00:00:00Z", "fail");
        let fresh = scenario.tmp.write(
            "fresh.json",
            login_cache("AKIARELOGGEDIN0", "2099-01-01T00:00:00Z", &dpop_key_pem()),
        );
        scenario.with("THEN_CACHE", &fresh.display().to_string())
    };
    for outcome in both_fresh(make) {
        // Configured (login_session): never unsigned, even with --skip-auth.
        assert!(
            outcome.result("decision").starts_with("refused:"),
            "{}",
            outcome.stdout
        );
        let dest = outcome.result("dest");
        assert!(
            dest.starts_with("POST us-east-1.signin.aws.amazon.com/v1/token"),
            "{dest}"
        );
        // Simulated `aws login`: the next request succeeds.
        assert_eq!(outcome.result("decision2"), "sign");
        assert_eq!(outcome.result("key2"), "AKIARELOGGEDIN0");
    }
}

#[test]
fn the_provider_plane_client_trusts_exactly_the_validated_roots() {
    // T-TLS-20 (in process): both planes receive the same ValidatedRoots.
    let roots = ValidatedRoots::for_tests(vec![crate::sdk::test_support::Pki::new("p").ca_der]);
    let http = ProviderHttp::validated(&roots).expect("provider client");
    assert!(http.roots().expect("validated").same_allocation(&roots));
    let runtime = SdkRuntime::new().expect("runtime");
    let transport = crate::sdk::SdkTransport::new(&runtime, &roots).expect("transport");
    assert!(transport.roots().expect("roots").same_allocation(&roots));
    assert!(ProviderHttp::SdkDefault.roots().is_none());
    assert_eq!(format!("{http:?}"), "ProviderHttp::Validated");
}

/// #5112: the build history is a bounded, opt-in test instrument; the
/// production default keeps nothing.
#[test]
fn the_build_log_is_off_by_default_and_bounded_when_enabled() {
    let mut off = BuildLog::default();
    for _ in 0..1000 {
        off.record(SourceKind::DefaultChain);
    }
    assert!(off.entries().is_empty(), "production records nothing");
    let mut on = BuildLog::enabled();
    for _ in 0..1000 {
        on.record(SourceKind::Env);
    }
    assert_eq!(on.entries().len(), BUILD_LOG_CAP);
    let runtime = SdkRuntime::new().expect("runtime");
    let guarded = GuardedCredentials::new(
        &runtime,
        "us-east-1",
        None,
        ProviderHttp::injected(recorder()),
    );
    assert!(!guarded.lock().log.enabled);
    assert!(guarded.with_build_recording().lock().log.enabled);
}

// ---- R-DG-07: bounded credential resolution (#5366 PM ruling 1) ----

fn elapsed_ms(outcome: &Child) -> u64 {
    outcome.result("elapsed_ms").parse().expect("elapsed_ms")
}

/// T-DG-07a provider phase: a stalled provider (an assume-role STS call, and
/// a login refresh, over injected provider HTTP) is abandoned at the
/// operation deadline with the fixed timeout class, no rebuild, no fallback.
#[test]
fn a_stalled_provider_is_abandoned_at_the_deadline() {
    let role = "[default]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = base\n\
                [profile base]\naws_access_key_id = AKIABASE0\naws_secret_access_key = s\n";
    let (login, _) = login_scenario("2000-01-01T00:00:00Z", "stall");
    let cases = [
        (
            Scenario::new(role, "")
                .ecs_fallback()
                .with("FIXTURE_STS", "stall"),
            "POST sts.us-east-1.amazonaws.com/",
        ),
        (login, "POST us-east-1.signin.aws.amazon.com/v1/token"),
    ];
    for (scenario, destination) in cases {
        let outcome = scenario
            .with("BOUNDED_MS", "300")
            .with("RUST_LOG", "trace")
            .run();
        assert_eq!(
            outcome.result("decision"),
            "aws_timeout",
            "{}",
            outcome.stdout
        );
        let elapsed = elapsed_ms(&outcome);
        assert!((250..2000).contains(&elapsed), "took {elapsed} ms");
        assert_eq!(
            outcome.result("log"),
            "Profile",
            "no rebuild after the deadline"
        );
        let dest = outcome.result("dest");
        assert!(
            !dest.is_empty() && dest.split('|').all(|r| r == destination),
            "{dest}"
        );
        assert!(outcome.result("diagnostic").contains("nothing was sent"));
        assert_no_canary(&outcome);
    }
}

/// The credential budget (10 s) and the operation deadline: the earlier
/// wins, and a deadline already past stops before any provider request.
#[test]
fn bounded_resolution_succeeds_in_budget_and_an_expired_deadline_sends_nothing() {
    let valid = "[default]\naws_access_key_id = AKIAFIXEDKEY0\naws_secret_access_key = s\n";
    let outcome = Scenario::new(valid, "").with("BOUNDED_MS", "5000").run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), "AKIAFIXEDKEY0");
    assert!(CREDENTIAL_BUDGET == std::time::Duration::from_secs(10));

    let role = "[default]\nrole_arn = arn:aws:iam::123456789012:role/r\nsource_profile = base\n\
                [profile base]\naws_access_key_id = AKIABASE0\naws_secret_access_key = s\n";
    let outcome = Scenario::new(role, "")
        .with("FIXTURE_STS", "stall")
        .with("BOUNDED_MS", "0")
        .run();
    assert_eq!(outcome.result("decision"), "aws_timeout");
    assert!(elapsed_ms(&outcome) < 1000);
    assert_eq!(
        outcome.result("dest"),
        "",
        "no provider request after the deadline"
    );
}

/// T-DG-02a through the real guarded source on the bounded path: an expired
/// login session refreshes through injected Sign-in HTTP, a refresh failure
/// fails closed with a fixed class (no unsigned fallback, no other identity).
#[test]
fn bounded_resolution_refreshes_expired_credentials_and_fails_closed() {
    let (refresh, _) = login_scenario("2000-01-01T00:00:00Z", "ok");
    let outcome = refresh.with("BOUNDED_MS", "5000").run();
    assert_eq!(outcome.result("decision"), "sign");
    assert_eq!(outcome.result("key"), "AKIAREFRESHEDKEY0");
    assert_eq!(
        outcome.result("dest"),
        "POST us-east-1.signin.aws.amazon.com/v1/token"
    );

    let (failing, _) = login_scenario("2000-01-01T00:00:00Z", "fail");
    let outcome = failing.ecs_fallback().with("BOUNDED_MS", "5000").run();
    assert_eq!(outcome.result("decision"), "aws_credential_provider");
    let dest = outcome.result("dest");
    assert!(
        dest.split('|')
            .all(|r| r == "POST us-east-1.signin.aws.amazon.com/v1/token"),
        "no fallback identity was requested: {dest}"
    );
    assert_no_canary(&outcome);

    let outcome = Scenario::new("", "").with("BOUNDED_MS", "5000").run();
    assert_eq!(outcome.result("decision"), "aws_no_credentials");
}

/// aws-config does not kill a `credential_process` child when its future is
/// dropped, so the bounded path refuses such a source before it is built:
/// no process starts, and the refusal is a fixed message.
#[test]
fn bounded_resolution_refuses_a_credential_process_before_it_runs() {
    let probe = TempDir::new("process-probe");
    let marker = probe.path().join("started");
    let process = format!(
        "[default]\ncredential_process = sh -c 'touch {}; sleep 3'\n",
        marker.display()
    );
    let start = std::time::Instant::now();
    let outcome = Scenario::new(&process, "")
        .ecs_fallback()
        .with("BOUNDED_MS", "300")
        .run();
    assert_eq!(outcome.result("decision"), "aws_invalid_config");
    assert!(outcome
        .result("diagnostic")
        .contains(UNBOUNDED_PROCESS_REFUSAL));
    assert!(!marker.exists(), "the credential_process must not start");
    assert_eq!(outcome.result("dest"), "", "and no fallback is requested");
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}
