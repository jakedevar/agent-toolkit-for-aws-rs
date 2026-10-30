//! Offline tests of the tunnel commands: scripted seams and recordings only. No
//! socket, no subprocess, no AWS or Redis/Valkey client, no wall clock. The
//! tests are named after the functions of the three Python scripts they port
//! (`find_tunnel_hosts`, `check_instance_running`, `validate_port`,
//! `check_port_available`, `wait_for_port`, `start_tunnel`, `check_dns`,
//! `test_connection`, `get_iam_auth_token`).

#![allow(clippy::items_after_statements)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use toolkit_aws::{AwsError, Secret, ServiceError};
use toolkit_core::{Json, ToolkitError};

use super::clean::{clean, SecretGate};
use super::connect::{self, is_private, ConnectArgs};
use super::discover::{self, DiscoverArgs};
use super::recorded::Recording;
use super::seams::{
    CacheConnection, CacheEnvironment, CacheErrorKind, ConnectPlan, IamTokenRequest, Operation,
    ProcessEvent, Resolution, Seam, SeamError, ServerInfo, TunnelAws, TunnelHost, TunnelProcess,
    UnavailableAws, UnavailableCache, UnavailableHost,
};
use super::start::{self, StartArgs};
use super::{is_host, is_instance_id, is_region};
use crate::Output;

// ---------------------------------------------------------------- harness

type Log = Rc<RefCell<Vec<String>>>;

fn document(text: &str) -> Json {
    toolkit_core::parse_json(text).expect("fixture JSON")
}

struct Captured {
    result: Result<(), ToolkitError>,
    status: u8,
    stdout: String,
    stderr: String,
}

fn capture(run: impl FnOnce(&mut Output<'_>) -> Result<(), ToolkitError>) -> Captured {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let (result, status) = {
        let mut out = Output::with_diagnostics(&mut stdout, &mut stderr);
        let result = run(&mut out);
        (result, out.exit_status())
    };
    Captured {
        result,
        status,
        stdout: String::from_utf8(stdout).expect("utf-8 stdout"),
        stderr: String::from_utf8(stderr).expect("utf-8 stderr"),
    }
}

fn exec(args: &[&str]) -> (u8, String, String) {
    let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = crate::run(&os_args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

struct TempFile(PathBuf);

impl TempFile {
    fn new(contents: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-elasticache-tunnel-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("write fixture");
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("utf-8 path")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn error_code(error: &ToolkitError) -> &'static str {
    error.code()
}

// --------------------------------------------------------------- AWS mock

#[derive(Default)]
struct ScriptedAws {
    steps: VecDeque<(Operation, Result<Json, SeamError>)>,
    seen: Vec<(Operation, Json)>,
}

impl ScriptedAws {
    fn new(steps: Vec<(Operation, Result<Json, SeamError>)>) -> Self {
        Self {
            steps: steps.into(),
            seen: Vec::new(),
        }
    }
}

impl TunnelAws for ScriptedAws {
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, SeamError> {
        self.seen.push((operation, request.clone()));
        let (expected, outcome) = self.steps.pop_front().expect("an unscripted AWS call");
        assert_eq!(expected, operation, "calls out of order");
        outcome
    }

    fn unconsumed(&self) -> usize {
        self.steps.len()
    }
}

fn denied(code: &str) -> SeamError {
    SeamError::Aws(AwsError::service(ServiceError::new(
        "POST",
        "/",
        400,
        Some(code),
        "not allowed",
        None,
    )))
}

fn instance(id: &str, name: &str, kind: &str) -> String {
    let tags = if name.is_empty() {
        String::new()
    } else {
        format!(
            r#","Tags":[{{"Key":"Env","Value":"x"}},{{"Key":"Name","Value":{}}}]"#,
            serde_text(name)
        )
    };
    format!(
        r#"{{"InstanceId":"{id}","InstanceType":"{kind}","Placement":{{"AvailabilityZone":"us-east-1a"}},"PrivateIpAddress":"10.0.0.7"{tags}}}"#
    )
}

/// A JSON string literal for `text` (quotes and control characters escaped).
fn serde_text(text: &str) -> String {
    format!("\"{}\"", toolkit_core::escape_json_string(text))
}

fn instances_page(instances: &[String], token: Option<&str>) -> Json {
    let next = token.map_or(String::new(), |token| format!(r#","NextToken":"{token}""#));
    document(&format!(
        r#"{{"Reservations":[{{"Instances":[{}]}}]{next}}}"#,
        instances.join(",")
    ))
}

fn managed_page(ids: &[&str]) -> Json {
    let list: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"InstanceId":"{id}"}}"#))
        .collect();
    document(&format!(
        r#"{{"InstanceInformationList":[{}]}}"#,
        list.join(",")
    ))
}

fn discover_args() -> DiscoverArgs<'static> {
    DiscoverArgs {
        vpc_id: "vpc-0abc1234",
        region: "us-east-1",
    }
}

fn run_discover(aws: &mut ScriptedAws) -> Captured {
    capture(|out| discover::run(&discover_args(), aws, out))
}

// ------------------------------------------------- find_tunnel_hosts (py)

#[test]
fn find_tunnel_hosts_puts_ssm_managed_instances_first_then_sorts_by_type() {
    let mut aws = ScriptedAws::new(vec![
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[
                    instance("i-0000000a", "plain", "t3.micro"),
                    instance("i-0000000b", "big", "m5.large"),
                    instance("i-0000000c", "small", "c5.large"),
                    instance("i-0000000d", "tie", "c5.large"),
                ],
                None,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(managed_page(&["i-0000000b", "i-0000000c", "i-0000000d"])),
        ),
    ]);
    let mut lines = Vec::new();
    let found = discover::find_tunnel_hosts(&discover_args(), &mut aws, &mut lines)
        .expect("hosts are found");
    let order: Vec<(&str, bool)> = found.iter().map(|i| (i.id.as_str(), i.ssm)).collect();
    assert_eq!(
        order,
        [
            ("i-0000000c", true),
            ("i-0000000d", true),
            ("i-0000000b", true),
            ("i-0000000a", false)
        ],
        "SSM first, then by type; a tie keeps the response order"
    );
}

#[test]
fn find_tunnel_hosts_recommends_the_first_ssm_instance_by_name_or_type() {
    let mut aws = ScriptedAws::new(vec![
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[
                    instance("i-0000000a", "", "a1.large"),
                    instance("i-0000000b", "jump", "m5.large"),
                    instance("i-0000000c", "", "t3.micro"),
                ],
                None,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(managed_page(&["i-0000000a", "i-0000000b"])),
        ),
    ]);
    let run = run_discover(&mut aws);
    assert!(run.result.is_ok());
    assert_eq!(run.status, 0);
    assert!(run.stdout.starts_with(
        "Scanning VPC vpc-0abc1234 in us-east-1 for SSM-managed instances ...\n\nFound 2 SSM-managed instance(s) (ready for tunnel, $0 extra cost):\n\n"
    ));
    assert!(
        run.stdout.contains("Recommended: i-0000000a (a1.large)\n"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("    --instance-id i-0000000a \\\n"));
    assert!(run
        .stdout
        .contains("--cache-port 6380 \\\n    --local-port 6380 \\\n    --region us-east-1\n"));
    assert!(run
        .stdout
        .contains("\n(1 additional instance(s) without SSM agent not shown)\n"));
    let request = &aws.seen[0].1.to_compact_string();
    assert!(
        request.contains(r#""Name":"vpc-id","Values":["vpc-0abc1234"]"#),
        "{request}"
    );
    assert!(request.contains(r#""Name":"instance-state-name","Values":["running"]"#));
}

#[test]
fn find_tunnel_hosts_without_a_running_instance_prints_the_options_and_fails() {
    let mut aws = ScriptedAws::new(vec![(
        Operation::DescribeInstances,
        Ok(instances_page(&[], None)),
    )]);
    let run = run_discover(&mut aws);
    assert!(run.result.is_ok());
    assert_eq!(run.status, 1);
    assert!(run
        .stdout
        .contains("\nNo running EC2 instances found in this VPC.\n\nOptions:\n"));
    assert!(run.stdout.contains("t4g.nano (~$3/month)"));
    assert!(run.stdout.contains("(no tunnel needed)"));
    assert_eq!(aws.seen.len(), 1, "no SSM query without instances");
}

#[test]
fn find_tunnel_hosts_without_ssm_agents_explains_how_to_get_one() {
    let mut aws = ScriptedAws::new(vec![
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[instance("i-0000000a", "plain", "t3.micro")],
                None,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(managed_page(&[])),
        ),
    ]);
    let run = run_discover(&mut aws);
    assert_eq!(run.status, 0);
    assert!(run
        .stdout
        .contains("Found 1 instance(s) but none have SSM agent:\n\n"));
    assert!(run
        .stdout
        .contains("  1. Install SSM agent on one of these instances (free):\n"));
    assert!(!run.stdout.contains("Recommended:"));
    assert!(!run.stdout.contains("additional instance"));
}

#[test]
fn find_tunnel_hosts_lists_everything_when_ssm_is_access_denied() {
    for code in [
        "AccessDeniedException",
        "AccessDenied",
        "UnauthorizedAccess",
    ] {
        let mut aws = ScriptedAws::new(vec![
            (
                Operation::DescribeInstances,
                Ok(instances_page(
                    &[instance("i-0000000a", "plain", "t3.micro")],
                    None,
                )),
            ),
            (Operation::DescribeInstanceInformation, Err(denied(code))),
        ]);
        let run = run_discover(&mut aws);
        assert_eq!(run.status, 0, "{code}");
        assert!(run.stdout.contains(
            "ERROR: Access denied querying SSM. Ensure your IAM role/user has ssm:DescribeInstanceInformation permission.\nListing all instances -- SSM status unknown.\n\n"
        ), "{}", run.stdout);
        assert!(run
            .stdout
            .contains("Found 1 instance(s) but none have SSM agent"));
    }
}

#[test]
fn find_tunnel_hosts_warns_when_ssm_fails_for_another_reason() {
    let mut aws = ScriptedAws::new(vec![
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[instance("i-0000000a", "plain", "t3.micro")],
                None,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Err(denied("ThrottlingException")),
        ),
    ]);
    let run = run_discover(&mut aws);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout.contains("Warning: could not query SSM: "),
        "{}",
        run.stdout
    );
    assert!(run
        .stdout
        .contains("Listing all instances -- SSM status unknown.\n"));
}

#[test]
fn find_tunnel_hosts_reports_an_ec2_failure_and_exits_1() {
    let mut aws = ScriptedAws::new(vec![(
        Operation::DescribeInstances,
        Err(denied("UnauthorizedAccess")),
    )]);
    let run = run_discover(&mut aws);
    assert!(run.result.is_ok());
    assert_eq!(run.status, 1);
    assert!(run.stdout.ends_with(
        "ERROR: Access denied. Ensure your IAM role/user has ec2:DescribeInstances and ssm:DescribeInstanceInformation permissions.\n"
    ), "{}", run.stdout);
    let mut aws = ScriptedAws::new(vec![(
        Operation::DescribeInstances,
        Err(denied("RequestLimitExceeded")),
    )]);
    let run = run_discover(&mut aws);
    assert_eq!(run.status, 1);
    assert!(run.stdout.contains("\nERROR: "));
}

#[test]
fn find_tunnel_hosts_walks_every_page_of_both_calls() {
    let mut aws = ScriptedAws::new(vec![
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[instance("i-0000000a", "one", "t3.micro")],
                Some("tok-1"),
            )),
        ),
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[instance("i-0000000b", "two", "t3.small")],
                None,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(document(
                r#"{"InstanceInformationList":[{"InstanceId":"i-0000000a"}],"NextToken":"tok-2"}"#,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(managed_page(&["i-0000000b"])),
        ),
    ]);
    let mut lines = Vec::new();
    let found = discover::find_tunnel_hosts(&discover_args(), &mut aws, &mut lines).expect("found");
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|i| i.ssm));
    assert!(aws.seen[1]
        .1
        .to_compact_string()
        .contains(r#""NextToken":"tok-1""#));
    assert!(aws.seen[3]
        .1
        .to_compact_string()
        .contains(r#""NextToken":"tok-2""#));
}

#[test]
fn find_tunnel_hosts_cleans_instance_text_from_the_responses() {
    let hostile = "evil\u{1b}[31m\r\nForged: line AKIAIOSFODNN7EXAMPLE";
    let long = "n".repeat(500);
    let mut aws = ScriptedAws::new(vec![
        (
            Operation::DescribeInstances,
            Ok(instances_page(
                &[
                    instance("i-0000000a", hostile, "t3.micro"),
                    instance("i-0000000b", &long, "t3.small"),
                ],
                None,
            )),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(managed_page(&["i-0000000a", "i-0000000b"])),
        ),
    ]);
    let run = run_discover(&mut aws);
    assert!(
        !run.stdout.contains('\u{1b}') && !run.stdout.contains('\r'),
        "{:?}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("AKIAIOSFODNN7EXAMPLE"),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains(&long), "a very long name is bounded");
    assert!(
        run.stdout.contains("evil\\x1b[31m\\x0d\\x0aForged: line"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.lines().all(|line| !line.starts_with("Forged")));
}

#[test]
fn find_tunnel_hosts_rejects_wrong_typed_or_null_response_members() {
    for page in [
        r#"{"Reservations":null}"#,
        r#"{"Reservations":[{"Instances":null}]}"#,
        r#"{"Reservations":[{"Instances":[{"InstanceId":"i-0000000a"}]}]}"#,
        r#"{"Reservations":[{"Instances":[{"InstanceId":7,"InstanceType":"t","Placement":{"AvailabilityZone":"a"}}]}]}"#,
        r#"{"Reservations":[],"NextToken":5}"#,
    ] {
        let mut aws = ScriptedAws::new(vec![(Operation::DescribeInstances, Ok(document(page)))]);
        let run = run_discover(&mut aws);
        assert_eq!(run.status, 1, "{page}");
        assert!(run.stdout.contains("ERROR: "), "{page}: {}", run.stdout);
        assert!(
            !run.stdout.contains("No running EC2 instances"),
            "null is not an empty discovery: {page}"
        );
    }
}

#[test]
fn find_tunnel_hosts_without_a_recording_refuses_with_a_typed_error() {
    let mut aws = UnavailableAws;
    let run = capture(|out| discover::run(&discover_args(), &mut aws, out));
    let error = run.result.expect_err("no AWS in this build");
    assert_eq!(error_code(&error), "elasticache_seam_unavailable");
    assert!(error
        .message()
        .contains("AWS API access is not available in this build"));
    assert!(error.message().contains("--recorded"));
    assert_eq!(run.status, 0);
}

// ------------------------------------------------------ start mocks (py)

struct MockProcess {
    settle_exit: Option<i32>,
    output: String,
    events: VecDeque<ProcessEvent>,
    exit: i32,
    graceful: bool,
    fail_next: Option<SeamError>,
    log: Log,
}

impl TunnelProcess for MockProcess {
    fn exited_after(&mut self, settle: Duration) -> Result<Option<i32>, SeamError> {
        self.log
            .borrow_mut()
            .push(format!("settle {}s", settle.as_secs()));
        Ok(self.settle_exit)
    }

    fn drain_output(&mut self) -> Result<String, SeamError> {
        self.log.borrow_mut().push("drain".to_string());
        Ok(self.output.clone())
    }

    fn next_event(&mut self, idle: Duration) -> Result<ProcessEvent, SeamError> {
        self.log
            .borrow_mut()
            .push(format!("event {}s", idle.as_secs()));
        if let Some(error) = self.fail_next.take() {
            return Err(error);
        }
        Ok(self.events.pop_front().unwrap_or(ProcessEvent::Eof))
    }

    fn terminate(&mut self) {
        self.log.borrow_mut().push("terminate".to_string());
    }

    fn wait(&mut self, timeout: Duration) -> Result<Option<i32>, SeamError> {
        self.log
            .borrow_mut()
            .push(format!("wait {}s", timeout.as_secs()));
        Ok(self.graceful.then_some(self.exit))
    }

    fn kill(&mut self) {
        self.log.borrow_mut().push("kill".to_string());
    }

    fn wait_exit(&mut self) -> Result<i32, SeamError> {
        self.log.borrow_mut().push("wait-exit".to_string());
        Ok(self.exit)
    }
}

struct MockHost {
    programs: Vec<&'static str>,
    busy: bool,
    checks: VecDeque<bool>,
    process: Option<MockProcess>,
    spawned: Option<Vec<String>>,
    log: Log,
}

impl MockHost {
    fn ready(log: &Log) -> Self {
        Self {
            programs: vec!["aws", "session-manager-plugin"],
            busy: false,
            checks: VecDeque::from([true]),
            process: Some(MockProcess {
                settle_exit: None,
                output: String::new(),
                events: VecDeque::new(),
                exit: 0,
                graceful: true,
                fail_next: None,
                log: Rc::clone(log),
            }),
            spawned: None,
            log: Rc::clone(log),
        }
    }

    fn process(&mut self) -> &mut MockProcess {
        self.process.as_mut().expect("process not yet spawned")
    }
}

impl TunnelHost for MockHost {
    fn program_on_path(&mut self, program: &str) -> Result<bool, SeamError> {
        self.log.borrow_mut().push(format!("which {program}"));
        Ok(self.programs.contains(&program))
    }

    fn local_port_free(&mut self, port: u16) -> Result<bool, SeamError> {
        self.log.borrow_mut().push(format!("bind {port}"));
        Ok(!self.busy)
    }

    fn port_accepts(&mut self, port: u16, timeout: Duration) -> Result<bool, SeamError> {
        self.log
            .borrow_mut()
            .push(format!("probe {port} {}s", timeout.as_secs()));
        Ok(self.checks.pop_front().expect("an unscripted probe"))
    }

    fn spawn(&mut self, command: &[String]) -> Result<Box<dyn TunnelProcess>, SeamError> {
        self.log.borrow_mut().push("spawn".to_string());
        self.spawned = Some(command.to_vec());
        Ok(Box::new(self.process.take().expect("one spawn")))
    }

    fn unconsumed(&self) -> usize {
        self.checks.len()
    }
}

const INSTANCE: &str = "i-0abc12345def";
const CACHE: &str = "my-cache.serverless.use1.cache.amazonaws.com";

fn start_args() -> StartArgs<'static> {
    StartArgs {
        instance_id: INSTANCE,
        cache_host: CACHE,
        cache_port: 6379,
        local_port: 6379,
        region: "us-west-2",
        profile: None,
    }
}

fn running() -> Json {
    document(r#"{"InstanceStatuses":[{"InstanceState":{"Name":"running"}}]}"#)
}

fn online() -> Json {
    document(r#"{"InstanceInformationList":[{"PingStatus":"Online"}]}"#)
}

fn healthy_aws() -> ScriptedAws {
    ScriptedAws::new(vec![
        (Operation::DescribeInstanceStatus, Ok(running())),
        (Operation::DescribeInstanceInformation, Ok(online())),
    ])
}

fn run_start(args: &StartArgs<'_>, aws: &mut ScriptedAws, host: &mut MockHost) -> Captured {
    capture(|out| start::run(args, aws, host, out))
}

fn logged(log: &Log) -> Vec<String> {
    log.borrow().clone()
}

// ------------------------------------------------------ validate_port (py)

#[test]
fn validate_port_rejects_out_of_range_ports_before_touching_any_seam() {
    for (cache_port, local_port, label, shown) in [
        (0, 6379, "--cache-port", 0),
        (65536, 6379, "--cache-port", 65536),
        (6379, 0, "--local-port", 0),
        (6379, -1, "--local-port", -1),
        (6379, 70000, "--local-port", 70000),
    ] {
        let log: Log = Log::default();
        let mut host = MockHost::ready(&log);
        let mut aws = ScriptedAws::default();
        let args = StartArgs {
            cache_port,
            local_port,
            ..start_args()
        };
        let run = run_start(&args, &mut aws, &mut host);
        assert!(run.result.is_ok());
        assert_eq!(run.status, 1);
        assert_eq!(
            run.stdout,
            format!("Pre-flight checks:\n\n  [FAIL] {label} must be between 1 and 65535 (got {shown})\n")
        );
        assert!(logged(&log).is_empty(), "no seam call: {:?}", logged(&log));
    }
}

#[test]
fn validate_port_accepts_the_bounds() {
    for port in [1_i64, 65535] {
        let log: Log = Log::default();
        let mut host = MockHost::ready(&log);
        host.busy = true;
        let args = StartArgs {
            cache_port: port,
            local_port: port,
            ..start_args()
        };
        let run = run_start(&args, &mut ScriptedAws::default(), &mut host);
        assert!(run.stdout.contains(&format!(
            "  [OK]   Port numbers are valid (local={port}, remote={port})"
        )));
    }
}

// ------------------------------------------------ check_port_available (py)

#[test]
fn check_port_available_stops_the_run_when_the_local_port_is_in_use() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.busy = true;
    let run = run_start(&start_args(), &mut ScriptedAws::default(), &mut host);
    assert_eq!(run.status, 1);
    assert!(run.stdout.ends_with(
        "  [FAIL] Local port 6379 is already in use.\n         Another process is listening on 127.0.0.1:6379.\n         Stop that process first, or use --local-port to pick a different port.\n"
    ), "{}", run.stdout);
    assert_eq!(
        logged(&log),
        ["bind 6379"],
        "nothing after the failed probe"
    );
}

#[test]
fn check_aws_cli_and_check_ssm_plugin_stop_the_run_when_missing() {
    for (missing, expected_line, expected_log) in [
        (
            "aws",
            "  [FAIL] AWS CLI not found on PATH.\n",
            vec!["bind 6379", "which aws"],
        ),
        (
            "session-manager-plugin",
            "  [FAIL] Session Manager plugin not found on PATH.\n",
            vec!["bind 6379", "which aws", "which session-manager-plugin"],
        ),
    ] {
        let log: Log = Log::default();
        let mut host = MockHost::ready(&log);
        host.programs.retain(|name| *name != missing);
        let run = run_start(&start_args(), &mut ScriptedAws::default(), &mut host);
        assert_eq!(run.status, 1, "{missing}");
        assert!(run.stdout.contains(expected_line), "{}", run.stdout);
        assert!(run
            .stdout
            .contains("         Install: https://docs.aws.amazon.com/"));
        assert_eq!(logged(&log), expected_log);
    }
}

// ------------------------------------------- check_instance_running (py)

#[test]
fn check_instance_running_fails_for_a_stopped_or_unknown_instance() {
    for response in [
        r#"{"InstanceStatuses":[{"InstanceState":{"Name":"stopped"}}]}"#,
        r#"{"InstanceStatuses":[]}"#,
        r"{}",
    ] {
        let log: Log = Log::default();
        let mut host = MockHost::ready(&log);
        let mut aws = ScriptedAws::new(vec![(
            Operation::DescribeInstanceStatus,
            Ok(document(response)),
        )]);
        let run = run_start(&start_args(), &mut aws, &mut host);
        assert_eq!(run.status, 1, "{response}");
        assert!(run
            .stdout
            .contains("  [FAIL] Instance i-0abc12345def is not in 'running' state.\n"));
        assert!(!logged(&log).contains(&"spawn".to_string()));
    }
}

#[test]
fn check_instance_running_sends_the_request_of_the_script() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    let mut aws = healthy_aws();
    let _ = run_start(&start_args(), &mut aws, &mut host);
    assert_eq!(aws.seen[0].0, Operation::DescribeInstanceStatus);
    assert_eq!(
        aws.seen[0].1.to_compact_string(),
        r#"{"InstanceIds":["i-0abc12345def"],"IncludeAllInstances":true}"#
    );
    assert_eq!(aws.seen[1].0, Operation::DescribeInstanceInformation);
    assert_eq!(
        aws.seen[1].1.to_compact_string(),
        r#"{"Filters":[{"Key":"InstanceIds","Values":["i-0abc12345def"]}]}"#
    );
}

#[test]
fn check_instance_running_treats_an_aws_failure_as_not_running_with_a_warning() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    let mut aws = ScriptedAws::new(vec![(
        Operation::DescribeInstanceStatus,
        Err(denied("AccessDenied")),
    )]);
    let run = run_start(&start_args(), &mut aws, &mut host);
    assert_eq!(run.status, 1);
    assert!(
        run.stdout
            .contains("Warning: could not verify instance state: "),
        "{}",
        run.stdout
    );
    assert!(run
        .stdout
        .contains("[FAIL] Instance i-0abc12345def is not in 'running' state."));
}

#[test]
fn a_malformed_instance_status_is_a_warning_not_an_empty_answer() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    let mut aws = ScriptedAws::new(vec![(
        Operation::DescribeInstanceStatus,
        Ok(document(r#"{"InstanceStatuses":null}"#)),
    )]);
    let run = run_start(&start_args(), &mut aws, &mut host);
    assert!(
        run.stdout
            .contains("Warning: could not verify instance state: "),
        "{}",
        run.stdout
    );
    assert_eq!(run.status, 1);
}

// -------------------------------------- check_instance_ssm_status (py)

#[test]
fn check_instance_ssm_status_warns_and_proceeds_when_not_online() {
    for response in [
        Ok(document(
            r#"{"InstanceInformationList":[{"PingStatus":"ConnectionLost"}]}"#,
        )),
        Ok(document(r#"{"InstanceInformationList":[]}"#)),
        Err(denied("AccessDenied")),
    ] {
        let failed = response.is_err();
        let log: Log = Log::default();
        let mut host = MockHost::ready(&log);
        let mut aws = ScriptedAws::new(vec![
            (Operation::DescribeInstanceStatus, Ok(running())),
            (Operation::DescribeInstanceInformation, response),
        ]);
        let run = run_start(&start_args(), &mut aws, &mut host);
        assert_eq!(run.status, 0);
        assert!(run
            .stdout
            .contains("  [WARN] Instance i-0abc12345def is not reporting to SSM as Online.\n"));
        assert!(
            run.stdout
                .contains("         Proceeding anyway ...\n\n\nStarting tunnel:"),
            "{}",
            run.stdout
        );
        assert_eq!(
            run.stdout
                .contains("Warning: could not verify SSM status: "),
            failed
        );
        assert!(
            logged(&log).contains(&"spawn".to_string()),
            "the tunnel is still started"
        );
    }
}

// ------------------------------------------------------- start_tunnel (py)

#[test]
fn start_tunnel_builds_the_ssm_session_command_of_the_script() {
    let command = start::tunnel_command(&start_args());
    assert_eq!(
        command,
        [
            "aws",
            "ssm",
            "start-session",
            "--target",
            "i-0abc12345def",
            "--document-name",
            "AWS-StartPortForwardingSessionToRemoteHost",
            "--parameters",
            r#"{"host": ["my-cache.serverless.use1.cache.amazonaws.com"], "portNumber": ["6379"], "localPortNumber": ["6379"]}"#,
            "--region",
            "us-west-2",
        ]
    );
    let with_profile = StartArgs {
        profile: Some("dev"),
        cache_port: 6380,
        local_port: 7000,
        ..start_args()
    };
    let command = start::tunnel_command(&with_profile);
    assert_eq!(&command[command.len() - 2..], ["--profile", "dev"]);
    assert!(command[8].contains(r#""portNumber": ["6380"], "localPortNumber": ["7000"]"#));
}

#[test]
fn start_tunnel_echoes_the_command_redacted_escaped_and_quoted() {
    let hostile = vec![
        "aws".to_string(),
        "--profile".to_string(),
        "dev\u{1b}[0m\nAKIAIOSFODNN7EXAMPLE x".to_string(),
        "{\"a\": [\"b c\"]}".to_string(),
    ];
    let echoed = start::echo_command(&hostile);
    assert!(!echoed.contains('\u{1b}') && !echoed.contains('\n'));
    assert!(!echoed.contains("AKIAIOSFODNN7EXAMPLE"), "{echoed}");
    assert!(echoed.starts_with("aws --profile 'dev\\x1b[0m"), "{echoed}");
    assert!(echoed.ends_with("'{\"a\": [\"b c\"]}'"), "{echoed}");
}

#[test]
fn start_tunnel_prints_the_banner_and_runs_the_lifecycle_with_the_script_timeouts() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().events =
        VecDeque::from([ProcessEvent::Line("Starting session\r\n".to_string())]);
    let mut aws = healthy_aws();
    let run = run_start(&start_args(), &mut aws, &mut host);
    assert!(run.result.is_ok(), "{:?}", run.result);
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Pre-flight checks:\n\n\
         \x20 [OK]   Port numbers are valid (local=6379, remote=6379)\n\
         \x20 [OK]   Local port 6379 is available\n\
         \x20 [OK]   AWS CLI found\n\
         \x20 [OK]   Session Manager plugin found\n\
         \x20 [OK]   Instance i-0abc12345def is running\n\
         \x20 [OK]   Instance i-0abc12345def SSM agent is online\n\n\
         Starting tunnel: 127.0.0.1:6379 -> my-cache.serverless.use1.cache.amazonaws.com:6379\n\
         \x20 via SSM instance: i-0abc12345def\n\
         \x20 region: us-west-2\n\n\
         Tunnel command:\n\
         \x20 aws ssm start-session --target i-0abc12345def --document-name AWS-StartPortForwardingSessionToRemoteHost --parameters '{\"host\": [\"my-cache.serverless.use1.cache.amazonaws.com\"], \"portNumber\": [\"6379\"], \"localPortNumber\": [\"6379\"]}' --region us-west-2\n\n\
         Tunnel is UP. Local port 127.0.0.1:6379 is accepting connections.\n\n\
         Note: ElastiCache Serverless requires TLS. Your client must connect\n\
         \x20 with TLS enabled and SNI set to my-cache.serverless.use1.cache.amazonaws.com\n\
         \x20 (the tunnel forwards raw TCP; TLS negotiation happens end-to-end).\n\n\
         Test with:\n\
         \x20 toolkit elasticache test-connection 127.0.0.1 --port 6379 \\\n\
         \x20   --tunnel-mode --server-name my-cache.serverless.use1.cache.amazonaws.com \\\n\
         \x20   --username <user> --password <password>\n\n\
         Press Ctrl+C to stop the tunnel.\n\
         \x20 [SSM] Starting session\n"
    );
    assert_eq!(
        logged(&log),
        [
            "bind 6379",
            "which aws",
            "which session-manager-plugin",
            "spawn",
            "settle 2s",
            "probe 6379 15s",
            "event 30s",
            "event 30s",
            "wait-exit"
        ]
    );
    assert_eq!(host.spawned.expect("spawned")[0], "aws");
}

#[test]
fn start_tunnel_notes_serverless_tls_only_for_a_serverless_host() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    let args = StartArgs {
        cache_host: "node.abc.0001.use1.cache.amazonaws.com",
        ..start_args()
    };
    let run = run_start(&args, &mut healthy_aws(), &mut host);
    assert!(run.stdout.contains("Tunnel is UP."));
    assert!(!run.stdout.contains("ElastiCache Serverless requires TLS"));
}

#[test]
fn start_tunnel_exits_1_when_the_process_dies_during_the_settle_time() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().settle_exit = Some(254);
    host.process().output =
        "SessionManagerPlugin not found\u{1b}[1m AKIAIOSFODNN7EXAMPLE".to_string();
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 1);
    assert!(run.stdout.ends_with(
        "Tunnel process exited immediately (code 254):\n\nSessionManagerPlugin not found\\x1b[1m [REDACTED]\n"
    ) || run.stdout.contains("Tunnel process exited immediately (code 254):\n\nSessionManagerPlugin not found\\x1b[1m"), "{}", run.stdout);
    assert!(!run.stdout.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(
        !logged(&log).iter().any(|entry| entry.starts_with("probe")),
        "no port probe after an early exit"
    );
}

#[test]
fn wait_for_port_warns_when_the_tunnel_is_not_up_after_the_first_probe() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.checks = VecDeque::from([false]);
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 0);
    assert!(run.stdout.contains(
        "Warning: local port 6379 not yet responding after 15s.\nThe tunnel may still be starting. Check the output below.\n\n"
    ));
    assert!(!run.stdout.contains("Tunnel is UP"));
    assert!(logged(&log).contains(&"probe 6379 15s".to_string()));
}

#[test]
fn wait_for_port_health_check_warns_each_time_an_idle_tunnel_stops_answering() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.checks = VecDeque::from([true, true, false, false]);
    host.process().events =
        VecDeque::from([ProcessEvent::Idle, ProcessEvent::Idle, ProcessEvent::Idle]);
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 0);
    let warning = "\n  [WARN] Tunnel port 6379 is no longer responding.\n         The SSM session may have stalled. Press Ctrl+C to stop.\n";
    assert_eq!(run.stdout.matches(warning).count(), 2, "{}", run.stdout);
    let probes: Vec<String> = logged(&log)
        .into_iter()
        .filter(|e| e.starts_with("probe"))
        .collect();
    assert_eq!(
        probes,
        [
            "probe 6379 15s",
            "probe 6379 3s",
            "probe 6379 3s",
            "probe 6379 3s"
        ]
    );
}

#[test]
fn start_tunnel_prefixes_and_cleans_every_ssm_output_line() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().events = VecDeque::from([
        ProcessEvent::Line("plain\n".to_string()),
        ProcessEvent::Line("forged\u{1b}]0;title\u{7}\u{202e}\n".to_string()),
        ProcessEvent::Line(format!("{}\n", "z".repeat(1000))),
        ProcessEvent::Line("key AKIAIOSFODNN7EXAMPLE\n".to_string()),
    ]);
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    let ssm: Vec<&str> = run
        .stdout
        .lines()
        .filter(|l| l.starts_with("  [SSM] "))
        .collect();
    assert_eq!(ssm.len(), 4, "{}", run.stdout);
    assert_eq!(ssm[0], "  [SSM] plain");
    assert!(ssm[1].contains("\\x1b]0;title\\x07\\u202e"), "{}", ssm[1]);
    assert!(ssm[2].len() < 260 && ssm[2].ends_with("..."), "{}", ssm[2]);
    assert!(!ssm[3].contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(!run.stdout.contains('\u{1b}') && !run.stdout.contains('\u{7}'));
}

#[test]
fn start_tunnel_reports_a_nonzero_exit_of_the_process_after_the_output_ends() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().exit = 3;
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 1);
    assert!(
        run.stdout.ends_with("\nTunnel exited with code 3\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn start_tunnel_on_ctrl_c_terminates_gracefully_without_a_kill() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().events = VecDeque::from([ProcessEvent::Interrupted]);
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .ends_with("\n\n\nShutting down tunnel ...\nTunnel closed.\n"),
        "{:?}",
        run.stdout
    );
    let entries = logged(&log);
    assert_eq!(
        &entries[entries.len() - 3..],
        ["event 30s", "terminate", "wait 5s"]
    );
    assert!(!entries.contains(&"kill".to_string()));
}

#[test]
fn start_tunnel_on_ctrl_c_kills_a_process_that_ignores_sigterm() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().graceful = false;
    host.process().events = VecDeque::from([ProcessEvent::Interrupted]);
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 0);
    assert!(run.stdout.ends_with("Tunnel closed.\n"));
    let entries = logged(&log);
    assert_eq!(
        &entries[entries.len() - 3..],
        ["terminate", "wait 5s", "kill"]
    );
}

#[test]
fn start_tunnel_kills_the_process_and_exits_1_on_an_unexpected_aws_error() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().fail_next = Some(denied("AccessDenied"));
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    assert_eq!(run.status, 1);
    assert!(run.stdout.contains("\n\nError: "), "{}", run.stdout);
    assert_eq!(logged(&log).last().map(String::as_str), Some("kill"));
}

#[test]
fn start_tunnel_kills_the_process_and_stops_on_a_recording_mismatch() {
    let log: Log = Log::default();
    let mut host = MockHost::ready(&log);
    host.process().fail_next = Some(SeamError::Replay("the recording ran out".to_string()));
    let run = run_start(&start_args(), &mut healthy_aws(), &mut host);
    let error = run.result.expect_err("a replay failure is fatal");
    assert_eq!(error_code(&error), "elasticache_recording_mismatch");
    assert_eq!(logged(&log).last().map(String::as_str), Some("kill"));
}

#[test]
fn start_tunnel_without_a_recording_refuses_every_seam_with_a_typed_error() {
    let mut host = UnavailableHost;
    let mut aws = UnavailableAws;
    let run = capture(|out| start::run(&start_args(), &mut aws, &mut host, out));
    let error = run.result.expect_err("no local port probe in this build");
    assert_eq!(error_code(&error), "elasticache_seam_unavailable");
    assert!(error
        .message()
        .contains("the local port probe is not available in this build"));
    assert!(run
        .stdout
        .starts_with("Pre-flight checks:\n\n  [OK]   Port numbers are valid"));

    let seams: Vec<SeamError> = vec![
        host.program_on_path("aws").expect_err("refused"),
        host.port_accepts(1, Duration::from_secs(1))
            .expect_err("refused"),
        host.spawn(&[]).err().expect("refused"),
        aws.call(Operation::DescribeInstances, &Json::Null)
            .expect_err("refused"),
    ];
    assert_eq!(
        seams,
        [
            SeamError::Unavailable(Seam::Program),
            SeamError::Unavailable(Seam::PortConnect),
            SeamError::Unavailable(Seam::TunnelProcess),
            SeamError::Unavailable(Seam::Aws),
        ]
    );
}

// ------------------------------------------------------ test_connection

struct MockConnection {
    ping: Result<f64, CacheErrorKind>,
    info: Option<ServerInfo>,
    log: Log,
}

impl CacheConnection for MockConnection {
    fn ping(&mut self) -> Result<f64, CacheErrorKind> {
        self.log.borrow_mut().push("ping".to_string());
        self.ping
    }

    fn info_server(&mut self) -> Option<ServerInfo> {
        self.log.borrow_mut().push("info".to_string());
        self.info.clone()
    }

    fn close(&mut self) {
        self.log.borrow_mut().push("close".to_string());
    }
}

struct MockCache {
    resolution: Resolution,
    init_error: Option<CacheErrorKind>,
    ping: Result<f64, CacheErrorKind>,
    info: Option<ServerInfo>,
    iam: Option<&'static str>,
    plans: Vec<ConnectPlan>,
    iam_requests: Vec<IamTokenRequest>,
    resolved: Vec<String>,
    log: Log,
}

impl MockCache {
    fn healthy() -> Self {
        Self {
            resolution: Resolution::Address("54.1.2.3".parse().expect("address")),
            init_error: None,
            ping: Ok(1.26),
            info: Some(ServerInfo {
                version: "7.1.0".to_string(),
                mode: "standalone".to_string(),
            }),
            iam: Some("TOKEN-SECRET-VALUE"),
            plans: Vec::new(),
            iam_requests: Vec::new(),
            resolved: Vec::new(),
            log: Log::default(),
        }
    }
}

impl CacheEnvironment for MockCache {
    fn resolve(&mut self, host: &str) -> Result<Resolution, SeamError> {
        self.resolved.push(host.to_string());
        Ok(self.resolution)
    }

    fn iam_token(&mut self, request: &IamTokenRequest) -> Result<Secret, SeamError> {
        self.iam_requests.push(request.clone());
        self.iam
            .map(Secret::new)
            .ok_or(SeamError::Unavailable(Seam::IamSigner))
    }

    fn connect(
        &mut self,
        plan: &ConnectPlan,
    ) -> Result<Result<Box<dyn CacheConnection>, CacheErrorKind>, SeamError> {
        self.plans.push(plan.clone());
        if let Some(kind) = self.init_error.take() {
            return Ok(Err(kind));
        }
        Ok(Ok(Box::new(MockConnection {
            ping: self.ping,
            info: self.info.clone(),
            log: Rc::clone(&self.log),
        })))
    }

    fn client_name(&self) -> &'static str {
        "mock-client"
    }

    fn unconsumed(&self) -> usize {
        0
    }
}

fn connect_args(host: &str) -> ConnectArgs<'_> {
    ConnectArgs {
        host,
        port: 6379,
        timeout_secs: 5.0,
        no_tls: false,
        tunnel_mode: false,
        server_name: None,
        username: Some("app"),
        password: Some("hunter2-PASSWORD"),
        iam_auth: false,
        iam_user: None,
        cache_name: None,
        serverless: false,
        region: "us-east-1",
    }
}

fn run_connect(args: &ConnectArgs<'_>, cache: &mut MockCache) -> Captured {
    capture(|out| connect::run(args, cache, out))
}

const NODE_HOST: &str = "node.abc.0001.use1.cache.amazonaws.com";

#[test]
fn test_connection_reports_ping_server_tls_and_success() {
    let mut cache = MockCache::healthy();
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert!(run.result.is_ok());
    assert_eq!(run.status, 0);
    assert_eq!(
        run.stdout,
        "Connecting to node.abc.0001.use1.cache.amazonaws.com:6379 (TLS, user=app) via mock-client ...\n\
         \x20 PING: OK (1.3 ms)\n\
         \x20 Server: engine version unknown, mode=unknown\n\
         \x20 TLS: enabled (in-transit encryption active)\n\
         \x20 Result: CONNECTION SUCCESSFUL\n"
    );
    assert_eq!(run.stderr, "");
    assert_eq!(logged(&cache.log), ["ping", "info", "close"]);
}

#[test]
fn test_connection_never_prints_the_password_or_token() {
    let secret = "hunter2-PASSWORD";
    let mut outcomes = vec![Ok(1.0)];
    outcomes.extend(ALL_KINDS.map(Err));
    for failure in outcomes {
        let mut cache = MockCache::healthy();
        cache.ping = failure;
        let run = run_connect(&connect_args(NODE_HOST), &mut cache);
        assert!(!run.stdout.contains(secret) && !run.stderr.contains(secret));
        let plan = cache.plans.first().expect("a connect plan");
        assert!(plan.password.is_some());
        assert!(!format!("{plan:?}").contains(secret), "{plan:?}");
    }
    let args = ConnectArgs {
        username: None,
        password: Some(secret),
        ..connect_args(NODE_HOST)
    };
    let run = run_connect(&args, &mut MockCache::healthy());
    assert!(run.stdout.contains("(TLS, password-auth)"));
    assert!(!run.stdout.contains(secret));
    let args = ConnectArgs {
        username: None,
        password: None,
        ..connect_args(NODE_HOST)
    };
    assert!(run_connect(&args, &mut MockCache::healthy())
        .stdout
        .contains("(TLS, no-auth)"));
    // An empty value is no credential (`if password:`).
    let args = ConnectArgs {
        username: Some(""),
        password: Some(""),
        ..connect_args(NODE_HOST)
    };
    assert!(run_connect(&args, &mut MockCache::healthy())
        .stdout
        .contains("(TLS, no-auth)"));
}

#[test]
fn test_connection_an_other_failure_prints_only_its_fixed_text() {
    let mut cache = MockCache::healthy();
    cache.ping = Err(CacheErrorKind::Other);
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr,
        "  PING FAILED: client error (details withheld)\n"
    );
}

#[test]
fn test_connection_authentication_failure_hints_at_the_rbac_user() {
    let mut cache = MockCache::healthy();
    cache.ping = Err(CacheErrorKind::AuthenticationFailed);
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr,
        "  PING FAILED: Authentication error -- authentication failed\n  Hint: Check username/password or ensure the RBAC user exists and is active.\n"
    );
    assert!(!run.stdout.contains("CONNECTION SUCCESSFUL"));
    assert!(!logged(&cache.log).contains(&"info".to_string()));
}

#[test]
fn test_connection_connection_failures_pick_the_tls_or_network_hint() {
    let hint = |kind: CacheErrorKind, no_tls: bool| {
        let mut cache = MockCache::healthy();
        cache.ping = Err(kind);
        let args = ConnectArgs {
            no_tls,
            ..connect_args(NODE_HOST)
        };
        let run = run_connect(&args, &mut cache);
        assert_eq!(run.status, 1);
        run.stderr.lines().nth(1).expect("a hint").to_string()
    };
    assert_eq!(
        hint(CacheErrorKind::TlsHandshakeFailed, false),
        "  Hint: TLS handshake failed. Verify the endpoint supports in-transit encryption."
    );
    assert_eq!(
        hint(CacheErrorKind::TlsHandshakeFailed, true),
        "  Hint: This endpoint may require TLS. Retry without --no-tls (TLS is default)."
    );
    for kind in [
        CacheErrorKind::ConnectionRefused,
        CacheErrorKind::Timeout,
        CacheErrorKind::DnsFailed,
    ] {
        assert_eq!(hint(kind, false), "  Hint: Check security group rules, VPC placement, and whether you need a tunnel or jump host.");
    }
}

#[test]
fn test_connection_missing_info_is_a_warning_and_plaintext_is_flagged() {
    let mut cache = MockCache::healthy();
    cache.info = None;
    let args = ConnectArgs {
        no_tls: true,
        ..connect_args(NODE_HOST)
    };
    let run = run_connect(&args, &mut cache);
    assert_eq!(run.status, 0);
    assert!(run.stdout.starts_with(
        "Connecting to node.abc.0001.use1.cache.amazonaws.com:6379 (plaintext, user=app)"
    ));
    assert_eq!(
        run.stderr,
        "  Server info: not available (ACL may restrict INFO command)\n  TLS: disabled -- consider enabling in-transit encryption for production use\n"
    );
    assert!(run.stdout.ends_with("  Result: CONNECTION SUCCESSFUL\n"));
}

#[test]
fn test_connection_client_init_failure_exits_1() {
    let mut cache = MockCache::healthy();
    cache.init_error = Some(CacheErrorKind::Other);
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr,
        "  Client init FAILED: client error (details withheld)\n"
    );
}

#[test]
fn test_connection_tunnel_mode_verifies_the_certificate_but_not_the_host_name() {
    let mut cache = MockCache::healthy();
    let args = ConnectArgs {
        tunnel_mode: true,
        server_name: Some(CACHE),
        port: 6380,
        ..connect_args("127.0.0.1")
    };
    let run = run_connect(&args, &mut cache);
    assert_eq!(run.status, 0);
    let plan = &cache.plans[0];
    assert!(plan.tls && !plan.check_hostname);
    assert_eq!(plan.server_name.as_deref(), Some(CACHE));
    assert!(run.stdout.contains("(TLS tunnel-mode, user=app)"));
    assert!(run
        .stdout
        .contains("  Tunnel mode: hostname verification disabled, target=127.0.0.1:6380\n"));
    assert_eq!(
        run.stderr,
        "  WARNING: tunnel mode is for LOCAL DEVELOPMENT ONLY\n"
    );
    assert!(cache.resolved.is_empty(), "127.0.0.1 is never resolved");
}

#[test]
fn test_connection_a_serverless_endpoint_uses_a_cluster_client_except_through_a_tunnel() {
    let mut cache = MockCache::healthy();
    run_connect(&connect_args(CACHE), &mut cache);
    assert!(
        cache.plans[0].cluster_client,
        "the host name says serverless"
    );
    assert!(cache.plans[0].check_hostname);

    let mut cache = MockCache::healthy();
    let args = ConnectArgs {
        serverless: true,
        ..connect_args(NODE_HOST)
    };
    run_connect(&args, &mut cache);
    assert!(cache.plans[0].cluster_client, "--serverless");

    let mut cache = MockCache::healthy();
    let args = ConnectArgs {
        serverless: true,
        tunnel_mode: true,
        ..connect_args("127.0.0.1")
    };
    let run = run_connect(&args, &mut cache);
    assert!(
        !cache.plans[0].cluster_client,
        "cluster discovery returns unreachable VPC addresses"
    );
    assert!(run.stdout.contains(
        "  Note: using standalone client through tunnel (cluster discovery\n  would return unreachable VPC IPs). PING validates connectivity.\n"
    ));

    let mut cache = MockCache::healthy();
    run_connect(&connect_args(NODE_HOST), &mut cache);
    assert!(!cache.plans[0].cluster_client);
}

#[test]
fn test_connection_does_not_send_the_sni_name_without_tunnel_mode() {
    let mut cache = MockCache::healthy();
    let args = ConnectArgs {
        server_name: Some(CACHE),
        ..connect_args(NODE_HOST)
    };
    run_connect(&args, &mut cache);
    assert_eq!(cache.plans[0].server_name, None);
}

#[test]
fn test_connection_tunnel_mode_without_tls_is_a_usage_error() {
    let args = ConnectArgs {
        tunnel_mode: true,
        no_tls: true,
        ..connect_args("127.0.0.1")
    };
    let run = run_connect(&args, &mut MockCache::healthy());
    let error = run.result.expect_err("refused");
    assert_eq!(error.exit_code(), 2);
    assert_eq!(
        error.message(),
        "--tunnel-mode requires TLS (cannot combine with --no-tls)"
    );
}

#[test]
fn check_dns_warns_about_private_addresses_unknown_hosts_and_skips_loopback() {
    let mut cache = MockCache::healthy();
    cache.resolution = Resolution::Address("10.0.1.5".parse().expect("address"));
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert!(run.stderr.starts_with(
        "  WARNING: node.abc.0001.use1.cache.amazonaws.com resolves to private IP 10.0.1.5.\n  If you are not in the VPC, this connection will time out.\n  Use --tunnel-mode with `toolkit elasticache start-tunnel` for local development.\n\n"
    ), "{}", run.stderr);
    assert_eq!(run.status, 0, "a warning does not stop the run");

    let mut cache = MockCache::healthy();
    cache.resolution = Resolution::NotFound;
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert!(run.stderr.starts_with(
        "  WARNING: DNS lookup failed for node.abc.0001.use1.cache.amazonaws.com. The endpoint may be wrong,\n  or you may not have DNS access to the VPC.\n\n"
    ));

    for host in ["127.0.0.1", "localhost", "::1"] {
        let mut cache = MockCache::healthy();
        let run = run_connect(&connect_args(host), &mut cache);
        assert!(cache.resolved.is_empty(), "{host}");
        assert!(!run.stderr.contains("WARNING"), "{host}");
    }
    let mut cache = MockCache::healthy();
    assert!(!run_connect(&connect_args(NODE_HOST), &mut cache)
        .stderr
        .contains("WARNING"));
}

#[test]
fn check_dns_private_address_ranges_follow_python_ipaddress() {
    let private = |text: &str| is_private(text.parse::<IpAddr>().expect("address"));
    for address in [
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "127.0.0.1",
        "169.254.1.1",
        "0.0.0.0",
        "198.18.0.1",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:10.0.0.1",
    ] {
        assert!(private(address), "{address}");
    }
    for address in [
        "8.8.8.8",
        "54.1.2.3",
        "172.32.0.1",
        "2001:4860:4860::8888",
        "::ffff:8.8.8.8",
    ] {
        assert!(!private(address), "{address}");
    }
}

// ----------------------------------------------- get_iam_auth_token (py)

#[test]
fn get_iam_auth_token_signs_for_the_lowercased_cache_and_sets_the_user() {
    let mut cache = MockCache::healthy();
    let args = ConnectArgs {
        iam_auth: true,
        iam_user: Some("iam-user"),
        cache_name: Some("My-Cache"),
        username: Some("ignored"),
        password: Some("ignored-too"),
        serverless: true,
        ..connect_args(CACHE)
    };
    let run = run_connect(&args, &mut cache);
    assert_eq!(run.status, 0);
    assert_eq!(
        cache.iam_requests,
        [IamTokenRequest {
            cache_name: "my-cache".to_string(),
            iam_user: "iam-user".to_string(),
            region: "us-east-1".to_string(),
            serverless: true,
        }]
    );
    assert!(run.stdout.starts_with(
        "Generating IAM auth token for user=iam-user, cache=My-Cache ...\n  IAM token generated (valid for 15 minutes)\nConnecting to "
    ), "{}", run.stdout);
    assert!(run.stdout.contains("(TLS, user=iam-user)"));
    assert!(
        !run.stdout.contains("TOKEN-SECRET-VALUE") && !run.stderr.contains("TOKEN-SECRET-VALUE")
    );
    let plan = &cache.plans[0];
    assert_eq!(plan.username.as_deref(), Some("iam-user"));
    assert_eq!(
        plan.password.as_ref().map(Secret::expose),
        Some("TOKEN-SECRET-VALUE")
    );
}

#[test]
fn get_iam_auth_token_requires_user_and_cache_name() {
    for (user, cache_name) in [
        (None, Some("c")),
        (Some("u"), None),
        (Some(""), Some("c")),
        (None, None),
    ] {
        let args = ConnectArgs {
            iam_auth: true,
            iam_user: user,
            cache_name,
            ..connect_args(CACHE)
        };
        let run = run_connect(&args, &mut MockCache::healthy());
        let error = run.result.expect_err("refused");
        assert_eq!(error.exit_code(), 2);
        assert_eq!(
            error.message(),
            "--iam-auth requires --iam-user and --cache-name"
        );
        assert_eq!(run.stdout, "");
    }
}

#[test]
fn get_iam_auth_token_without_a_signer_is_a_typed_refusal() {
    let mut cache = MockCache::healthy();
    cache.iam = None;
    let args = ConnectArgs {
        iam_auth: true,
        iam_user: Some("u"),
        cache_name: Some("c"),
        ..connect_args(CACHE)
    };
    let run = run_connect(&args, &mut cache);
    let error = run.result.expect_err("no signer");
    assert_eq!(error_code(&error), "elasticache_seam_unavailable");
    assert!(error
        .message()
        .contains("IAM auth token signing is not available in this build"));
}

#[test]
fn test_connection_without_a_recording_refuses_before_connecting() {
    let mut cache = UnavailableCache;
    let run = capture(|out| connect::run(&connect_args(NODE_HOST), &mut cache, out));
    let error = run.result.expect_err("no DNS or client in this build");
    assert_eq!(error_code(&error), "elasticache_seam_unavailable");
    assert!(error.message().contains("DNS resolution is not available"));
    assert_eq!(run.stdout, "");
    let error = cache
        .connect(&ConnectPlan {
            host: NODE_HOST.to_string(),
            port: 1,
            timeout_secs: 1.0,
            tls: true,
            check_hostname: true,
            server_name: None,
            cluster_client: false,
            username: None,
            password: None,
        })
        .err()
        .expect("refused");
    assert_eq!(error, SeamError::Unavailable(Seam::CacheClient));
}

// ------------------------------------------------------------- cleaning

#[test]
fn caller_text_is_redacted_then_bounded_then_escaped() {
    assert_eq!(clean("plain text"), "plain text");
    assert_eq!(
        clean("a\u{1b}[0mb\r\nc\u{202e}d"),
        "a\\x1b[0mb\\x0d\\x0ac\\u202ed"
    );
    assert!(!clean("key AKIAIOSFODNN7EXAMPLE here").contains("AKIAIOSFODNN7EXAMPLE"));
    let long = format!("{}{}", "a".repeat(190), "AKIAIOSFODNN7EXAMPLE");
    let shown = clean(&long);
    assert!(
        !shown.contains("AKIAIOSFODNN7"),
        "a credential across the bound is redacted first: {shown}"
    );
    assert!(shown.chars().count() <= 203 + 12, "{shown}");
    // The bound is the shared safe_path bound (#4941, round 20): 200
    // characters of the redacted text, cut before escaping.
    let escapes = format!("{}{}", "\u{1b}".repeat(210), "x");
    assert_eq!(
        clean(&escapes),
        format!("{}...", "\\x1b".repeat(200)),
        "escaping happens after the bound"
    );
    let exact = "a".repeat(200);
    assert_eq!(clean(&exact), exact, "a text at the bound is not cut");
}

// --------------------------------------------------- argument validation

#[test]
fn identifiers_are_validated_before_any_seam_is_used() {
    assert!(is_instance_id("i-0abc12345def"));
    assert!(is_instance_id("mi-0123456789abcdef0"));
    for bad in [
        "",
        "i-",
        "i-XYZ",
        "i-0abc1234567890abcd",
        "--target",
        "i-0abc 1234",
        "vpc-0abc1234",
    ] {
        assert!(!is_instance_id(bad), "{bad}");
    }
    assert!(is_host(CACHE) && is_host("127.0.0.1") && is_host("::1"));
    for bad in ["", "-x", "a b", "a;b", "a/b", "a\nb", &"h".repeat(254)] {
        assert!(!is_host(bad), "{bad:?}");
    }
    assert!(is_region("us-east-1") && is_region("us-gov-west-1"));
    for bad in ["", "US-EAST-1", "1x", "us east", &"r".repeat(33)] {
        assert!(!is_region(bad), "{bad:?}");
    }
}

#[test]
fn the_commands_refuse_bad_arguments_with_exit_2_and_never_echo_the_value() {
    let secret = "Zq9-hostile-value";
    for args in [
        vec!["elasticache", "find-tunnel-host"],
        vec!["elasticache", "find-tunnel-host", "--vpc-id", secret],
        vec![
            "elasticache",
            "find-tunnel-host",
            "--vpc-id",
            "vpc-0abc1234",
            "--region",
            "NOT A REGION",
        ],
        vec![
            "elasticache",
            "find-tunnel-host",
            "--vpc-id",
            "vpc-0abc1234",
            "--profile",
            "bad\u{7}profile",
        ],
        vec!["elasticache", "start-tunnel", "--cache-host", CACHE],
        vec![
            "elasticache",
            "start-tunnel",
            "--instance-id",
            secret,
            "--cache-host",
            CACHE,
        ],
        vec![
            "elasticache",
            "start-tunnel",
            "--instance-id",
            INSTANCE,
            "--cache-host",
            "bad host",
        ],
        vec![
            "elasticache",
            "start-tunnel",
            "--instance-id",
            INSTANCE,
            "--cache-host",
            CACHE,
            "--cache-port",
            "abc",
        ],
        vec!["elasticache", "test-connection"],
        vec!["elasticache", "test-connection", "bad host"],
        vec!["elasticache", "test-connection", NODE_HOST, "--port", "0"],
        vec![
            "elasticache",
            "test-connection",
            NODE_HOST,
            "--port",
            "65536",
        ],
        vec![
            "elasticache",
            "test-connection",
            NODE_HOST,
            "--timeout",
            "0",
        ],
        vec![
            "elasticache",
            "test-connection",
            NODE_HOST,
            "--timeout",
            "nan",
        ],
        vec![
            "elasticache",
            "test-connection",
            NODE_HOST,
            "--server-name",
            "bad name",
        ],
        vec![
            "elasticache",
            "test-connection",
            NODE_HOST,
            "--tunnel-mode",
            "--no-tls",
        ],
        vec!["elasticache", "test-connection", NODE_HOST, "--iam-auth"],
    ] {
        let (status, stdout, stderr) = exec(&args);
        assert_eq!(status, 2, "{args:?}: {stderr}");
        assert_eq!(stdout, "", "{args:?}");
        assert!(!stderr.contains(secret), "{args:?}: {stderr}");
    }
}

#[test]
fn a_bare_dash_is_refused_for_every_value_flag_and_the_positional() {
    // No audited Python line reads stdin for any of these arguments (#4780):
    // `-` is a refused value everywhere, and `--flag=-` too.
    let value_flags: [(&str, &[&str]); 3] = [
        ("find-tunnel-host", super::FIND_TUNNEL_HOST_VALUE_FLAGS),
        ("start-tunnel", super::START_TUNNEL_VALUE_FLAGS),
        ("test-connection", super::TEST_CONNECTION_VALUE_FLAGS),
    ];
    for (command, flags) in value_flags {
        for flag in flags {
            let (status, stdout, _) = exec(&["elasticache", command, flag, "-"]);
            assert_eq!((status, stdout.as_str()), (2, ""), "{command} {flag} -");
            let attached = format!("{flag}=-");
            let (status, stdout, _) = exec(&["elasticache", command, &attached]);
            assert_eq!((status, stdout.as_str()), (2, ""), "{command} {attached}");
        }
    }
    let (status, stdout, _) = exec(&["elasticache", "test-connection", "-"]);
    assert_eq!((status, stdout.as_str()), (2, ""), "positional host");
    let (status, _, _) = exec(&["elasticache", "test-connection", "-x"]);
    assert_eq!(status, 2);
}

#[test]
fn the_recorded_flag_name_is_a_single_shared_constant_per_command_family() {
    for flags in [
        super::FIND_TUNNEL_HOST_VALUE_FLAGS,
        super::START_TUNNEL_VALUE_FLAGS,
        super::TEST_CONNECTION_VALUE_FLAGS,
    ] {
        assert_eq!(
            flags.iter().filter(|flag| **flag == "--recorded").count(),
            1
        );
    }
}

// ------------------------------------------------------------ CLI replay

fn write_recording(text: &str) -> TempFile {
    TempFile::new(text)
}

#[test]
fn the_commands_refuse_without_a_recording_and_open_nothing() {
    for args in [
        vec![
            "elasticache",
            "find-tunnel-host",
            "--vpc-id",
            "vpc-0abc1234",
        ],
        vec![
            "elasticache",
            "start-tunnel",
            "--instance-id",
            INSTANCE,
            "--cache-host",
            CACHE,
        ],
        vec![
            "elasticache",
            "test-connection",
            NODE_HOST,
            "--password",
            "hunter2-PASSWORD",
        ],
    ] {
        let (status, _, stderr) = exec(&args);
        assert_eq!(status, 1, "{args:?}");
        assert!(stderr.contains("not available in this build"), "{stderr}");
        assert!(stderr.contains("--recorded FILE"), "{stderr}");
        assert!(!stderr.contains("hunter2-PASSWORD"));
    }
}

#[test]
fn find_tunnel_host_replays_a_recording_end_to_end() {
    let recording = write_recording(&format!(
        r#"{{"calls":[
          {{"service":"ec2","operation":"DescribeInstances",
            "request":{{"Filters":[{{"Name":"vpc-id","Values":["vpc-0abc1234"]}},{{"Name":"instance-state-name","Values":["running"]}}]}},
            "response":{}}},
          {{"service":"ssm","operation":"DescribeInstanceInformation",
            "request":{{"Filters":[{{"Key":"InstanceIds","Values":["i-0000000a"]}}]}},
            "response":{{"InstanceInformationList":[{{"InstanceId":"i-0000000a"}}]}}}}
        ]}}"#,
        instances_page(&[instance("i-0000000a", "jump", "t3.micro")], None).to_compact_string()
    ));
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "find-tunnel-host",
        "--vpc-id",
        "vpc-0abc1234",
        "--region",
        "eu-west-1",
        "--profile",
        "dev",
        "--recorded",
        recording.path(),
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    assert!(stdout.contains("Scanning VPC vpc-0abc1234 in eu-west-1"));
    assert!(stdout.contains("Recommended: i-0000000a (jump)\n"));
}

#[test]
fn a_recording_with_an_unused_step_or_a_different_request_fails() {
    let unused = write_recording(
        r#"{"calls":[
          {"service":"ec2","operation":"DescribeInstances","response":{"Reservations":[]}},
          {"service":"ssm","operation":"DescribeInstanceInformation","response":{}}
        ]}"#,
    );
    let (status, _, stderr) = exec(&[
        "elasticache",
        "find-tunnel-host",
        "--vpc-id",
        "vpc-0abc1234",
        "--recorded",
        unused.path(),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("never used by this run"), "{stderr}");

    let other = write_recording(
        r#"{"calls":[{"service":"ec2","operation":"DescribeInstances",
            "request":{"Filters":[]},"error":{"code":"AccessDenied","message":"no"}}]}"#,
    );
    let (status, _, stderr) = exec(&[
        "elasticache",
        "find-tunnel-host",
        "--vpc-id",
        "vpc-0abc1234",
        "--recorded",
        other.path(),
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("differs from the request made"),
        "the request of an error step is compared first: {stderr}"
    );
}

#[test]
fn a_recording_with_a_wrong_typed_member_is_rejected_by_name() {
    for (text, needle) in [
        (r"[]", "the recorded document must be an object"),
        (
            r#"{"calls":null}"#,
            "\"calls\" must be a list of objects when present (it is null)",
        ),
        (
            r#"{"calls":[{"service":"ec2","operation":"DescribeInstances","request":null,"response":{}}]}"#,
            "recorded call 0: \"request\" must be an object when present (it is null)",
        ),
        (
            r#"{"calls":[{"service":"ec2","operation":"DescribeInstances"}]}"#,
            "exactly one of \"response\"",
        ),
        (
            r#"{"calls":[{"service":"s3","operation":"ListBuckets","response":{}}]}"#,
            "must be ec2 DescribeInstances",
        ),
        (
            r#"{"calls":[{"service":"ec2","operation":"DescribeInstances","error":{"kind":7},"response":{}}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls":[{"service":"ec2","operation":"DescribeInstances","error":{"kind":"weird"}}]}"#,
            "\"error.kind\" must be client, connect, timeout or no_credentials",
        ),
        (
            r#"{"host":{}}"#,
            "has a member this command does not read (4 characters); accepted: calls",
        ),
        (
            r#"{"calls":[],"extra-secret-member":1}"#,
            "has a member this command does not read (19 characters)",
        ),
    ] {
        let recording = write_recording(text);
        let (status, _, stderr) = exec(&[
            "elasticache",
            "find-tunnel-host",
            "--vpc-id",
            "vpc-0abc1234",
            "--recorded",
            recording.path(),
        ]);
        assert_eq!(status, 1, "{text}");
        assert!(stderr.contains(needle), "{text}: {stderr}");
        assert!(
            !stderr.contains("extra-secret-member"),
            "a member name of the recording is not echoed: {stderr}"
        );
    }
}

#[test]
fn start_tunnel_replays_a_recorded_session_end_to_end() {
    let recording = write_recording(&format!(
        r#"{{
          "calls":[
            {{"service":"ec2","operation":"DescribeInstanceStatus",
              "request":{{"InstanceIds":["{INSTANCE}"],"IncludeAllInstances":true}},
              "response":{{"InstanceStatuses":[{{"InstanceState":{{"Name":"running"}}}}]}}}},
            {{"service":"ssm","operation":"DescribeInstanceInformation",
              "response":{{"InstanceInformationList":[{{"PingStatus":"Online"}}]}}}}
          ],
          "host":{{"programs":["aws","session-manager-plugin"],"busy_ports":[7000],"port_checks":[true]}},
          "tunnel":{{"events":[{{"line":"Starting session\n"}},{{"interrupt":true}}],"stops_gracefully":false}}
        }}"#
    ));
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "start-tunnel",
        "--instance-id",
        INSTANCE,
        "--cache-host",
        CACHE,
        "--region",
        "us-west-2",
        "--recorded",
        recording.path(),
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    assert!(stdout.contains("Tunnel is UP."));
    assert!(stdout.contains("  [SSM] Starting session\n"));
    assert!(stdout.ends_with("\n\n\nShutting down tunnel ...\nTunnel closed.\n"));

    let busy = write_recording(r#"{"host":{"busy_ports":[6379]}}"#);
    let (status, stdout, _) = exec(&[
        "elasticache",
        "start-tunnel",
        "--instance-id",
        INSTANCE,
        "--cache-host",
        CACHE,
        "--recorded",
        busy.path(),
    ]);
    assert_eq!(status, 1);
    assert!(stdout.contains("[FAIL] Local port 6379 is already in use."));

    let wrong_command = write_recording(
        r#"{"calls":[
            {"service":"ec2","operation":"DescribeInstanceStatus","response":{"InstanceStatuses":[{"InstanceState":{"Name":"running"}}]}},
            {"service":"ssm","operation":"DescribeInstanceInformation","response":{"InstanceInformationList":[{"PingStatus":"Online"}]}}],
           "host":{"programs":["aws","session-manager-plugin"]},
           "tunnel":{"command":["aws","other"]}}"#,
    );
    let (status, _, stderr) = exec(&[
        "elasticache",
        "start-tunnel",
        "--instance-id",
        INSTANCE,
        "--cache-host",
        CACHE,
        "--recorded",
        wrong_command.path(),
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("recorded tunnel command differs"),
        "{stderr}"
    );
}

#[test]
fn start_tunnel_recording_with_unused_events_fails() {
    let recording = write_recording(
        r#"{
          "calls":[
            {"service":"ec2","operation":"DescribeInstanceStatus","response":{"InstanceStatuses":[{"InstanceState":{"Name":"running"}}]}},
            {"service":"ssm","operation":"DescribeInstanceInformation","response":{"InstanceInformationList":[{"PingStatus":"Online"}]}}
          ],
          "host":{"programs":["aws","session-manager-plugin"],"port_checks":[true,false]},
          "tunnel":{"events":[{"interrupt":true},{"line":"never read"}]}
        }"#,
    );
    let (status, _, stderr) = exec(&[
        "elasticache",
        "start-tunnel",
        "--instance-id",
        INSTANCE,
        "--cache-host",
        CACHE,
        "--recorded",
        recording.path(),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("never used by this run"), "{stderr}");
}

#[test]
fn test_connection_replays_a_recorded_connection_without_the_password() {
    let recording = write_recording(
        r#"{"cache":{"address":"54.1.2.3",
            "connect":{"request":{"host":"node.abc.0001.use1.cache.amazonaws.com","port":6379,"tls":true,
              "check_hostname":true,"cluster_client":false,"timeout_secs":5.0,"auth":"user","username":"app"}},
            "ping_ms":2.04,"server":{"version":"7.1.0","mode":"cluster"}}}"#,
    );
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "test-connection",
        NODE_HOST,
        "--username",
        "app",
        "--password",
        "hunter2-PASSWORD",
        "--recorded",
        recording.path(),
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    assert!(
        stdout.contains("  PING: OK (2.0 ms)\n  Server: engine version unknown, mode=unknown\n")
    );
    assert!(!stdout.contains("hunter2-PASSWORD"));

    let (status, _, stderr) = exec(&[
        "elasticache",
        "test-connection",
        NODE_HOST,
        "--no-tls",
        "--recorded",
        recording.path(),
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("recorded connect plan differs from the plan built"),
        "{stderr}"
    );
    assert!(!stderr.contains("hunter2"));

    let failing = write_recording(
        r#"{"cache":{"address":"10.0.0.9","ping_error":{"kind":"authentication_failed"}}}"#,
    );
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "test-connection",
        NODE_HOST,
        "--password",
        "hunter2-PASSWORD",
        "--recorded",
        failing.path(),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("resolves to private IP 10.0.0.9."));
    assert!(stderr.contains("Authentication error -- authentication failed"));
    assert!(!format!("{stdout}{stderr}").contains("hunter2"));
}

#[test]
fn recording_parse_errors_never_carry_a_value() {
    let secret = "SECRET-RECORDED-VALUE";
    for text in [
        format!(r#"{{"host":{{"programs":[1,"{secret}"]}}}}"#),
        format!(r#"{{"host":{{"busy_ports":["{secret}"]}}}}"#),
        format!(r#"{{"host":{{"port_checks":["{secret}"]}}}}"#),
        format!(r#"{{"tunnel":{{"events":[{{"line":7,"x":"{secret}"}}]}}}}"#),
        format!(r#"{{"tunnel":{{"events":[{{"idle":false}}],"exit_code":"{secret}"}}}}"#),
        format!(r#"{{"cache":{{"address":"{secret}"}}}}"#),
    ] {
        let error = Recording::from_document(&document(&text), &["host", "tunnel", "cache"])
            .err()
            .expect("rejected");
        assert!(!error.contains(secret), "{text}: {error}");
    }
}

// ------------------- #4986 #5004 #5005 #5007 injected-client typed diagnostics

/// Every kind a client can fail with, and the fixed text it prints.
const ALL_KINDS: [CacheErrorKind; 6] = CacheErrorKind::ALL;

/// A password with the characters the reviewers' reproductions used.
const FORM_PASSWORD: &str = "marmalade zqx742qz/synthetic+value";

fn failing_cache(kind: &str, error: CacheErrorKind) -> MockCache {
    let mut cache = MockCache::healthy();
    if kind == "init" {
        cache.init_error = Some(error);
    } else {
        cache.ping = Err(error);
    }
    cache
}

fn assert_no_secret(run: &Captured, parts: &[&str], context: &str) {
    let all = format!("{}{}", run.stdout, run.stderr);
    for part in parts {
        assert!(
            !all.contains(part),
            "{context}: {part:?} leaked into:\n{all}"
        );
    }
    if let Err(error) = &run.result {
        let text = format!("{error:?}{error}");
        for part in parts {
            assert!(!text.contains(part), "{context}: error leaked {part:?}");
        }
    }
}

#[test]
fn a_client_error_kind_prints_only_its_fixed_text_in_every_auth_flow() {
    let token =
        "mycache.use1.cache.amazonaws.com/?Action=connect&User=iamuser&X-Amz-Signature=Wd8zXk33";
    let parts = [
        "marmalade",
        "zqx742qz",
        "synthetic",
        "Wd8zXk33",
        "iamuser",
        "mycache.use1",
    ];
    for kind in ALL_KINDS {
        for stage in ["init", "ping"] {
            for flow in ["rbac", "password", "iam"] {
                let mut cache = failing_cache(stage, kind);
                cache.iam = Some(token);
                let args = ConnectArgs {
                    username: (flow == "rbac").then_some("app"),
                    password: Some(FORM_PASSWORD),
                    iam_auth: flow == "iam",
                    iam_user: Some("iam-user"),
                    cache_name: Some("My-Cache"),
                    ..connect_args(NODE_HOST)
                };
                let context = format!("{stage}/{flow}/{kind:?}");
                let run = run_connect(&args, &mut cache);
                assert_eq!(run.status, 1, "{context}");
                assert_no_secret(&run, &parts, &context);
                let first = run.stderr.lines().next().expect("a diagnostic");
                assert!(first.ends_with(kind.text()), "{context}: {first}");
                if stage == "init" {
                    assert_eq!(
                        run.stderr,
                        format!("  Client init FAILED: {}\n", kind.text())
                    );
                }
            }
        }
    }
}

#[test]
fn the_text_of_a_kind_is_first_party_and_pinned() {
    let texts: Vec<(&str, &str)> = ALL_KINDS
        .iter()
        .map(|kind| (kind.key(), kind.text()))
        .collect();
    assert_eq!(
        texts,
        [
            ("authentication_failed", "authentication failed"),
            ("connection_refused", "connection refused"),
            ("tls_handshake_failed", "TLS handshake failed"),
            ("timeout", "timed out"),
            ("dns_failed", "host name lookup failed"),
            ("other", "client error (details withheld)"),
        ]
    );
    for kind in ALL_KINDS {
        assert_eq!(CacheErrorKind::from_key(kind.key()), Some(kind));
    }
    assert_eq!(CacheErrorKind::from_key("authentication"), None);
    assert_eq!(CacheErrorKind::from_key(""), None);
}

#[test]
fn an_injected_server_info_is_printed_through_the_gate_only() {
    let hostile = ServerInfo {
        version: format!("7.1 {FORM_PASSWORD}"),
        mode: "marmalade+zqx742qz%2Fsynthetic%2Bvalue".to_string(),
    };
    let mut cache = MockCache::healthy();
    cache.info = Some(hostile);
    let args = ConnectArgs {
        password: Some(FORM_PASSWORD),
        ..connect_args(NODE_HOST)
    };
    let run = capture(|out| connect::run(&args, &mut cache, out));
    assert_eq!(run.status, 0);
    assert!(
        run.stdout
            .contains("  Server: engine version unknown, mode=unknown\n"),
        "{}",
        run.stdout
    );
    assert_no_secret(&run, &["marmalade", "zqx742qz", "synthetic"], "server info");
}

// ----------------------------------------------- the SecretGate itself

#[test]
fn the_gate_withholds_authenticated_metadata_without_scanning_secrets_or_values() {
    let gate = SecretGate::new(&["pre-rec", "recorded-token", ""]);
    assert_eq!(gate.version("7.1.0"), "unknown");
    // Overlapping exact values: either one anywhere in the value refuses it.
    assert_eq!(gate.version("pre-recorded-token"), "unknown");
    assert_eq!(gate.version("7-pre-rec"), "unknown");
    assert_eq!(gate.version("x-recorded-token"), "unknown");
    assert_eq!(gate.mode("cluster"), "unknown");

    let self_overlap = SecretGate::new(&["ababababababa"]);
    assert_eq!(self_overlap.version("ababababababababa"), "unknown");
    assert_eq!(self_overlap.version("abababababab"), "unknown");

    // Empty passwords are not credentials; no-auth metadata is still useful.
    assert_eq!(SecretGate::new(&[""]).version("7.1.0"), "7.1.0");

    // Credential length and the number of possible renderings have no effect:
    // construction checks only emptiness and stores one boolean.
    let long_secret = "a".repeat(1025);
    assert_eq!(
        SecretGate::new(&[long_secret.as_str()]).version("aaaa"),
        "unknown"
    );
    let near_match = format!("{}b", "a".repeat(5000));
    let big_value = "a".repeat(1 << 20);
    let gate = SecretGate::new(&[near_match.as_str()]);
    assert_eq!(gate.version(&big_value), "unknown");
    assert_eq!(gate.mode(&big_value), "unknown");
    let no_auth = SecretGate::new(&[]);
    assert_eq!(no_auth.version(&big_value), "unknown");
    assert_eq!(no_auth.mode(&big_value), "unknown");
}

#[test]
fn the_gate_allowlist_for_a_version_is_exact() {
    let gate = SecretGate::new(&[]);
    let max = format!("a{}", "-".repeat(31));
    assert_eq!(max.len(), 32);
    assert_eq!(gate.version(&max), max);
    assert_eq!(gate.version(&format!("{max}x")), "unknown");
    for ok in ["7", "7.1", "7.2.4-rc1+b5", "a", "Z.-+9"] {
        assert_eq!(gate.version(ok), ok, "{ok}");
    }
    for bad in [
        "",
        ".7",
        "-7",
        "+7",
        "7 1",
        "7_1",
        "7/1",
        "7:1",
        "7\n",
        "\u{e9}",
        "7\u{200b}",
    ] {
        assert_eq!(gate.version(bad), "unknown", "{bad:?}");
    }
    for mode in ["standalone", "cluster", "sentinel"] {
        assert_eq!(gate.mode(mode), mode);
    }
    for bad in ["", "Cluster", "cluster ", "master", "7.1"] {
        assert_eq!(gate.mode(bad), "unknown", "{bad:?}");
    }
}

// ---------------------------------------------- #4987 the PING outcome

fn replay_connection(recording: &str, extra: &[&str]) -> (u8, String, String) {
    let file = write_recording(recording);
    let mut args = vec!["elasticache", "test-connection", "127.0.0.1"];
    args.extend_from_slice(extra);
    args.push("--recorded");
    args.push(file.path());
    exec(&args)
}

#[test]
fn a_recorded_cache_needs_exactly_one_ping_outcome() {
    for (recording, why) in [
        (r#"{"cache":{}}"#, "no outcome"),
        (r#"{"cache":{"server":{"version":"7"}}}"#, "only server"),
        (
            r#"{"cache":{"ping_ms":1.0,"ping_error":{"kind":"other"}}}"#,
            "both",
        ),
        (
            r#"{"cache":{"ping_ms":1.0,"ping_error":{"kind":"connection_refused"}}}"#,
            "both, explicit kind",
        ),
    ] {
        let (status, stdout, stderr) = replay_connection(recording, &[]);
        assert_eq!(status, 1, "{why}: {stdout}{stderr}");
        assert!(
            !stdout.contains("PING: OK") && !stdout.contains("CONNECTION SUCCESSFUL"),
            "{why}: success without a recorded outcome:\n{stdout}"
        );
        assert!(
            stderr.contains("ping_ms") && stderr.contains("ping_error"),
            "{why}: {stderr}"
        );
        assert!(!stderr.contains("PING FAILED"), "{why}: {stderr}");
    }
}

#[test]
fn a_recorded_ping_time_must_be_a_finite_non_negative_number() {
    for bad in ["-1.0", "\"fast\"", "null"] {
        let recording = format!(r#"{{"cache":{{"ping_ms":{bad}}}}}"#);
        let (status, stdout, stderr) = replay_connection(&recording, &[]);
        assert_eq!(status, 1, "{bad}: {stdout}{stderr}");
        assert!(!stdout.contains("CONNECTION SUCCESSFUL"), "{bad}");
    }
}

#[test]
fn a_recorded_explicit_ping_outcome_replays_and_an_init_error_needs_none() {
    let (status, stdout, stderr) = replay_connection(r#"{"cache":{"ping_ms":2.5}}"#, &[]);
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert!(stderr.contains("Server info: not available"), "{stderr}");
    assert!(stdout.contains("  PING: OK (2.5 ms)\n"));

    let (status, stdout, stderr) = replay_connection(
        r#"{"cache":{"ping_error":{"kind":"connection_refused"}}}"#,
        &[],
    );
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(stderr.contains("PING FAILED: Connection error -- connection refused"));

    // A client that never gets built never reaches PING.
    let (status, stdout, stderr) =
        replay_connection(r#"{"cache":{"connect":{"init_error":"other"}}}"#, &[]);
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(
        stderr.starts_with("  Client init FAILED: client error (details withheld)\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("recorded"), "{stderr}");
}

// ----------------------------------------- #4988 unused cache operations

fn assert_unused(recording: &str, extra: &[&str], why: &str) {
    let (status, stdout, stderr) = replay_connection(recording, extra);
    assert_eq!(status, 1, "{why}: {stdout}{stderr}");
    assert!(stderr.contains("never used by this run"), "{why}: {stderr}");
}

#[test]
fn a_recorded_cache_step_no_call_used_fails_the_run() {
    // The recorded signer result: IAM was not requested.
    assert_unused(
        r#"{"cache":{"iam_token":true,"ping_ms":1.0}}"#,
        &[],
        "unused signer",
    );
    // The explicit DNS answer: a loopback host skips the lookup.
    assert_unused(
        r#"{"cache":{"address":"10.0.0.9","ping_ms":1.0}}"#,
        &[],
        "unused dns",
    );
    // The reviewed reproduction: both at once.
    assert_unused(
        r#"{"cache":{"address":"10.0.0.9","iam_token":true,"ping_ms":1.0}}"#,
        &[],
        "unused dns and signer",
    );
}

#[test]
fn a_recorded_result_after_an_earlier_failure_is_unused() {
    assert_unused(
        r#"{"cache":{"connect":{"init_error":"other"},"ping_ms":1.0}}"#,
        &[],
        "ping after init_error",
    );
    assert_unused(
        r#"{"cache":{"connect":{"init_error":"other"},"server":{"version":"7"}}}"#,
        &[],
        "info after init_error",
    );
    assert_unused(
        r#"{"cache":{"ping_error":{"kind":"other"},"server":{"version":"7"}}}"#,
        &[],
        "info after a failed ping",
    );
}

#[test]
fn a_recorded_cache_with_every_step_used_succeeds() {
    let (status, stdout, stderr) = replay_connection(
        r#"{"cache":{"iam_token":true,"ping_ms":1.0,"server":{"version":"7.1","mode":"cluster"}}}"#,
        &["--iam-auth", "--iam-user", "u", "--cache-name", "c"],
    );
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    assert!(stdout.contains("IAM token generated"));
    assert!(stdout.contains("Server: engine version unknown, mode=unknown"));

    // Optional metadata may be absent: no server, no address.
    let file = write_recording(r#"{"cache":{"address":"54.1.2.3","ping_ms":1.0}}"#);
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "test-connection",
        NODE_HOST,
        "--recorded",
        file.path(),
    ]);
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert!(stdout.contains("Result: CONNECTION SUCCESSFUL"));
}

// ------------------------------------- #4989 partial SSM pagination

fn two_instances() -> Json {
    instances_page(
        &[
            instance("i-0000000b", "plain", "c5.large"),
            instance("i-0000000a", "known-ready", "t3.micro"),
        ],
        None,
    )
}

fn first_managed_page() -> Json {
    document(r#"{"InstanceInformationList":[{"InstanceId":"i-0000000a"}],"NextToken":"page-2"}"#)
}

#[test]
fn find_tunnel_hosts_keeps_the_hosts_of_earlier_ssm_pages_when_a_later_page_fails() {
    for (error, expected) in [
        (
            denied("ThrottlingException"),
            "Warning: could not query SSM: ",
        ),
        (
            denied("AccessDeniedException"),
            "ERROR: Access denied querying SSM. Ensure your IAM role/user has ssm:DescribeInstanceInformation permission.\n",
        ),
        (
            denied("UnauthorizedAccess"),
            "ERROR: Access denied querying SSM.",
        ),
    ] {
        let mut aws = ScriptedAws::new(vec![
            (Operation::DescribeInstances, Ok(two_instances())),
            (Operation::DescribeInstanceInformation, Ok(first_managed_page())),
            (Operation::DescribeInstanceInformation, Err(error)),
        ]);
        let run = run_discover(&mut aws);
        assert!(run.result.is_ok(), "{expected}");
        assert_eq!(run.status, 0, "{expected}: {}", run.stdout);
        assert!(run.stdout.contains(expected), "{expected}: {}", run.stdout);
        assert!(
            run.stdout.contains("Listing all instances -- SSM status unknown.\n\n"),
            "{}",
            run.stdout
        );
        assert!(
            run.stdout.contains("Found 1 SSM-managed instance(s)"),
            "{}",
            run.stdout
        );
        assert!(
            run.stdout.contains("Recommended: i-0000000a (known-ready)\n"),
            "{}",
            run.stdout
        );
        assert!(!run.stdout.contains("none have SSM agent"), "{}", run.stdout);
        assert!(
            run.stdout
                .contains("(1 additional instance(s) without SSM agent not shown)"),
            "{}",
            run.stdout
        );
        assert_eq!(aws.unconsumed(), 0);
    }
}

#[test]
fn find_tunnel_hosts_orders_the_hosts_kept_from_a_partial_ssm_walk() {
    let mut aws = ScriptedAws::new(vec![
        (Operation::DescribeInstances, Ok(two_instances())),
        (
            Operation::DescribeInstanceInformation,
            Ok(first_managed_page()),
        ),
        (
            Operation::DescribeInstanceInformation,
            Err(denied("ThrottlingException")),
        ),
    ]);
    let mut lines = Vec::new();
    let found = discover::find_tunnel_hosts(&discover_args(), &mut aws, &mut lines).expect("found");
    let order: Vec<(&str, bool)> = found.iter().map(|i| (i.id.as_str(), i.ssm)).collect();
    assert_eq!(
        order,
        [("i-0000000a", true), ("i-0000000b", false)],
        "the host of the first page is SSM-managed, so it sorts first"
    );
    assert!(aws.seen[2]
        .1
        .to_compact_string()
        .contains(r#""NextToken":"page-2""#));
}

#[test]
fn find_tunnel_hosts_keeps_earlier_pages_when_a_later_ssm_page_is_malformed() {
    let mut aws = ScriptedAws::new(vec![
        (Operation::DescribeInstances, Ok(two_instances())),
        (
            Operation::DescribeInstanceInformation,
            Ok(first_managed_page()),
        ),
        (
            Operation::DescribeInstanceInformation,
            Ok(document(r#"{"InstanceInformationList":[{"NoId":1}]}"#)),
        ),
    ]);
    let run = run_discover(&mut aws);
    assert_eq!(run.status, 0, "{}", run.stdout);
    assert!(run.stdout.contains("Warning: could not query SSM: "));
    assert!(run
        .stdout
        .contains("Recommended: i-0000000a (known-ready)\n"));
}

#[test]
fn find_tunnel_hosts_a_replay_mismatch_on_a_later_ssm_page_stays_fatal() {
    let mut aws = ScriptedAws::new(vec![
        (Operation::DescribeInstances, Ok(two_instances())),
        (
            Operation::DescribeInstanceInformation,
            Ok(first_managed_page()),
        ),
        (
            Operation::DescribeInstanceInformation,
            Err(SeamError::Replay("the request differs".to_string())),
        ),
    ]);
    let run = run_discover(&mut aws);
    assert!(run.result.is_err(), "{}", run.stdout);
    assert!(!run.stdout.contains("Recommended:"), "{}", run.stdout);
    assert!(!run.stdout.contains("Warning: could not query SSM"));
}

// ------------------------- #5004 #5005 #5007: typed cache diagnostics (PM ruling)
//
// Cache failures carry no client diagnostic string. A failure is a kind from
// a closed set with fixed first-party text; the version
// and mode of the server are printed only without credentials and when they
// match a strict allowlist.

const KINDS: [(&str, &str); 6] = [
    ("authentication_failed", "authentication failed"),
    ("connection_refused", "connection refused"),
    ("tls_handshake_failed", "TLS handshake failed"),
    ("timeout", "timed out"),
    ("dns_failed", "host name lookup failed"),
    ("other", "client error (details withheld)"),
];

const SG_HINT: &str =
    "  Hint: Check security group rules, VPC placement, and whether you need a tunnel or jump host.\n";

#[test]
fn every_client_init_failure_kind_prints_its_fixed_text() {
    for (kind, text) in KINDS {
        let (status, stdout, stderr) = replay_connection(
            &format!(r#"{{"cache":{{"connect":{{"init_error":"{kind}"}}}}}}"#),
            &[],
        );
        assert_eq!(status, 1, "{kind}: {stdout}{stderr}");
        assert_eq!(stderr, format!("  Client init FAILED: {text}\n"), "{kind}");
    }
}

#[test]
fn every_ping_failure_kind_prints_its_fixed_text_and_hint() {
    let rbac = "  Hint: Check username/password or ensure the RBAC user exists and is active.\n";
    let tls = "  Hint: TLS handshake failed. Verify the endpoint supports in-transit encryption.\n";
    let expected = [
        (
            "authentication_failed",
            format!("  PING FAILED: Authentication error -- authentication failed\n{rbac}"),
        ),
        (
            "connection_refused",
            format!("  PING FAILED: Connection error -- connection refused\n{SG_HINT}"),
        ),
        (
            "tls_handshake_failed",
            format!("  PING FAILED: Connection error -- TLS handshake failed\n{tls}"),
        ),
        (
            "timeout",
            format!("  PING FAILED: Connection error -- timed out\n{SG_HINT}"),
        ),
        (
            "dns_failed",
            format!("  PING FAILED: Connection error -- host name lookup failed\n{SG_HINT}"),
        ),
        (
            "other",
            "  PING FAILED: client error (details withheld)\n".to_string(),
        ),
    ];
    for (kind, stderr_expected) in expected {
        let (status, stdout, stderr) = replay_connection(
            &format!(r#"{{"cache":{{"ping_error":{{"kind":"{kind}"}}}}}}"#),
            &[],
        );
        assert_eq!(status, 1, "{kind}: {stdout}{stderr}");
        assert_eq!(stderr, stderr_expected, "{kind}");
        assert!(!stdout.contains("CONNECTION SUCCESSFUL"), "{kind}");
    }
}

#[test]
fn the_tls_hint_follows_the_typed_kind_and_the_tls_setting() {
    let (_, _, with_tls) = replay_connection(
        r#"{"cache":{"ping_error":{"kind":"tls_handshake_failed"}}}"#,
        &[],
    );
    assert!(
        with_tls.contains("Hint: TLS handshake failed. Verify"),
        "{with_tls}"
    );
    let (_, _, plaintext) = replay_connection(
        r#"{"cache":{"ping_error":{"kind":"tls_handshake_failed"}}}"#,
        &["--no-tls"],
    );
    assert!(
        plaintext.contains("Hint: This endpoint may require TLS. Retry without --no-tls"),
        "{plaintext}"
    );
    // A password that spells "tls" steers nothing: only the kind picks the hint.
    let (_, _, other) = replay_connection(
        r#"{"cache":{"ping_error":{"kind":"connection_refused"}}}"#,
        &["--password", "tls"],
    );
    assert!(other.ends_with(SG_HINT), "{other}");
}

#[test]
fn a_recording_refuses_free_text_error_and_unknown_kinds_without_echoing_them() {
    let secret = "marmalade zqx742qz/synthetic+value";
    let echo = "marmalade";
    for recording in [
        // Free text as the init error or the PING message is no longer a recording.
        format!(r#"{{"cache":{{"connect":{{"init_error":"rejected {secret}"}}}}}}"#),
        format!(r#"{{"cache":{{"ping_error":{{"kind":"other","message":"{secret}"}}}}}}"#),
        r#"{"cache":{"ping_error":{"kind":"authentication_failed","message":"x"}}}"#.to_string(),
        // An unknown or old-style kind, a missing kind.
        format!(r#"{{"cache":{{"ping_error":{{"kind":"{secret}"}}}}}}"#),
        r#"{"cache":{"ping_error":{"kind":"authentication"}}}"#.to_string(),
        r#"{"cache":{"ping_error":{}}}"#.to_string(),
        format!(r#"{{"cache":{{"connect":{{"init_error":"{secret}"}}}}}}"#),
    ] {
        let (status, stdout, stderr) = replay_connection(&recording, &["--password", secret]);
        assert_eq!(status, 1, "{recording}");
        let all = format!("{stdout}{stderr}");
        assert!(!all.contains(echo), "{recording}: {all}");
        assert!(!all.contains("zqx742qz"), "{recording}: {all}");
        assert!(!all.contains("PING FAILED"), "{recording}: {all}");
        assert!(!all.contains("Client init FAILED"), "{recording}: {all}");
    }
}

/// #5365: an echo guard fails only when the program echoes the secret, never
/// because of the host's temp path. The refusal names the recording path, so a
/// path made of digits and hex letters (a job id, a pid) is in the output.
#[test]
fn an_echo_guard_ignores_a_recording_path_holding_digits_and_hex() {
    let canary = "zqx742qz";
    let secret = format!("marmalade {canary}/synthetic+value");
    let path = std::env::temp_dir().join(format!(
        "toolkit-elasticache-742-baba-{}.json",
        std::process::id()
    ));
    let recording = format!(r#"{{"cache":{{"ping_error":{{"kind":"{secret}"}}}}}}"#);
    fs::write(&path, recording).expect("write fixture");
    let file = TempFile(path);
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "test-connection",
        "127.0.0.1",
        "--password",
        &secret,
        "--recorded",
        file.path(),
    ]);
    assert_eq!(status, 1, "{stdout}{stderr}");
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("742-baba"),
        "the refusal no longer names the path, so this test proves nothing: {all}"
    );
    assert!(!all.contains("marmalade"), "{all}");
    assert!(!all.contains(canary), "{all}");
}

/// The text of a server field that the allowlist must refuse.
const UNKNOWN: &str = "unknown";

fn server_line(version: &str, mode: &str, extra: &[&str]) -> (u8, String, String) {
    let recording = format!(
        r#"{{"cache":{{"ping_ms":1.0,"server":{{"version":{},"mode":{}}}}}}}"#,
        Json::string(version).to_compact_string(),
        Json::string(mode).to_compact_string(),
    );
    replay_connection(&recording, extra)
}

#[test]
fn authenticated_server_metadata_never_echoes_allowlisted_credential_encodings() {
    // Nearby #5004 inputs: these encodings pass the version allowlist and
    // contain no literal occurrence of the password. No rendering enumeration
    // can prove a server value is unrelated to a credential.
    for (password, version) in [
        ("marmalade 742", "marmalade+742"),
        ("a b", "7a+b9"),
        ("synthetic", "c3ludGhldGlj"), // base64, no padding needed
        ("synthetic", "73796e746865746963"), // hexadecimal
        ("7.1.0", "Ny4xLjA"),          // unpadded base64url
        ("a a", "a+a+a"),              // overlapping encoded password occurrences
        ("a a a", "a+a+a+a"),          // self-overlapping encoded occurrences
    ] {
        let (status, stdout, stderr) = server_line(version, "cluster", &["--password", password]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert!(
            !stdout.contains(version),
            "encoded credential {version:?} leaked: {stdout}"
        );
        assert!(stdout.contains("Server: engine version unknown, mode=unknown\n"));
        assert!(stdout.contains("Result: CONNECTION SUCCESSFUL"));
    }
}

#[test]
fn injected_iam_metadata_never_echoes_an_encoded_active_token() {
    let mut cache = MockCache::healthy();
    cache.iam = Some("synthetic");
    cache.info = Some(ServerInfo {
        version: "c3ludGhldGlj".to_string(),
        mode: "cluster".to_string(),
    });
    let args = ConnectArgs {
        password: None,
        iam_auth: true,
        iam_user: Some("u"),
        cache_name: Some("c"),
        ..connect_args("127.0.0.1")
    };
    let run = run_connect(&args, &mut cache);
    assert!(run.result.is_ok());
    assert_eq!(run.status, 0);
    assert_no_secret(&run, &["synthetic", "c3ludGhldGlj"], "active IAM token");
    assert_eq!(logged(&cache.log), ["ping", "info", "close"]);
}

#[test]
fn a_supplied_password_still_withholds_metadata_after_iam_replaces_it() {
    // The active token is empty in this injected client, so only the supplied
    // credential can cause the gate to withhold this form-urlencoded echo.
    let mut cache = MockCache::healthy();
    cache.iam = Some("");
    cache.info = Some(ServerInfo {
        version: "supplied+password".to_string(),
        mode: "cluster".to_string(),
    });
    let args = ConnectArgs {
        password: Some("supplied password"),
        iam_auth: true,
        iam_user: Some("u"),
        cache_name: Some("c"),
        ..connect_args("127.0.0.1")
    };
    let run = run_connect(&args, &mut cache);
    assert!(run.result.is_ok());
    assert_eq!(run.status, 0);
    assert_no_secret(
        &run,
        &["supplied password", "supplied+password"],
        "supplied password",
    );
}

#[test]
fn reviewed_free_text_errors_and_overlap_inputs_are_refused_without_echoing() {
    // The exact original #4986/#5004/#5007 reproductions, including the
    // supplied-password/active-token overlap. They now fail recording parsing.
    for (recording, extra, absent) in [
        (
            r#"{"cache":{"connect":{"init_error":"rejected synthetic-unlabelled"}}}"#,
            vec!["--password", "synthetic-unlabelled"],
            "synthetic-unlabelled",
        ),
        (
            r#"{"cache":{"ping_error":{"kind":"other","message":"rejected synthetic-unlabelled"}}}"#,
            vec!["--password", "synthetic-unlabelled"],
            "synthetic-unlabelled",
        ),
        (
            r#"{"cache":{"connect":{"init_error":"rejected marmalade+742%2Fsynthetic%2Bvalue"}}}"#,
            vec!["--password", "marmalade 742/synthetic+value"],
            "marmalade+742%2Fsynthetic%2Bvalue",
        ),
        (
            r#"{"cache":{"ping_error":{"kind":"authentication","message":"rejected marmalade+742%2Fsynthetic%2Bvalue"}}}"#,
            vec!["--password", "marmalade 742/synthetic+value"],
            "marmalade+742%2Fsynthetic%2Bvalue",
        ),
        (
            r#"{"cache":{"iam_token":true,"ping_error":{"kind":"other","message":"rejected pre-recorded-token"}}}"#,
            vec![
                "--iam-auth",
                "--iam-user",
                "u",
                "--cache-name",
                "c",
                "--password",
                "pre-rec",
            ],
            "orded-token",
        ),
        (
            r#"{"cache":{"connect":{"init_error":"rejected ababababababababa"}}}"#,
            vec!["--password", "ababababababa"],
            "ababababababababa",
        ),
    ] {
        let (status, stdout, stderr) = replay_connection(recording, &extra);
        assert_eq!(status, 1);
        assert!(stdout.is_empty(), "{stdout}");
        assert!(!stderr.contains(absent), "{stderr}");
        assert!(!stderr.contains("Client init FAILED") && !stderr.contains("PING FAILED"));
    }
}

#[test]
fn the_server_version_prints_only_when_it_matches_the_allowlist() {
    let longest = format!("7{}", "9".repeat(31));
    for version in [
        "7.1.0",
        "255.255.255",
        "7.2.4-rc1+build5",
        "V",
        "0",
        longest.as_str(),
    ] {
        let (status, stdout, stderr) = server_line(version, "standalone", &[]);
        assert_eq!(status, 0, "{version}: {stderr}");
        assert!(
            stdout.contains(&format!(
                "  Server: engine version {version}, mode=standalone\n"
            )),
            "{version}: {stdout}"
        );
    }
    let too_long = format!("7{}", "9".repeat(32));
    for version in [
        "",
        too_long.as_str(),
        ".1.0",
        "-1",
        "+x",
        "7 1",
        "7.1\n0",
        "7.1\u{1b}[31m",
        "7.1_0",
        "7.1/0",
        "7%2F1",
        "7.1.0;rm",
        "\u{e9}7",
        "<b>7</b>",
        "7.1.0 (build 5)",
    ] {
        let (status, stdout, _) = server_line(version, "standalone", &[]);
        assert_eq!(status, 0, "{version:?}");
        assert!(
            stdout.contains(&format!(
                "  Server: engine version {UNKNOWN}, mode=standalone\n"
            )),
            "{version:?}: {stdout}"
        );
        assert!(
            !stdout.contains(version) || version.is_empty(),
            "{version:?}: {stdout}"
        );
    }
}

#[test]
fn the_server_mode_prints_only_a_known_mode() {
    for mode in ["standalone", "cluster", "sentinel"] {
        let (_, stdout, _) = server_line("7.1.0", mode, &[]);
        assert!(
            stdout.contains(&format!("version 7.1.0, mode={mode}\n")),
            "{mode}: {stdout}"
        );
    }
    for mode in [
        "",
        "Standalone",
        "cluster\n",
        "unexpected",
        "7.1.0",
        "stand alone",
    ] {
        let (_, stdout, _) = server_line("7.1.0", mode, &[]);
        assert!(
            stdout.contains(&format!("version 7.1.0, mode={UNKNOWN}\n")),
            "{mode:?}: {stdout}"
        );
    }
}

#[test]
fn authenticated_server_values_including_overlaps_and_near_matches_are_withheld() {
    // Each value passes the pattern, including overlaps and near matches.
    let long_a = "a".repeat(30);
    let long_secret = format!("{long_a}b");
    for (version, secret, why) in [
        ("7.1.0-Zx9kq2", "Zx9kq2", "raw suffix"),
        ("Zx9kq2", "Zx9kq2", "the whole value"),
        ("7.Zx9kq2.0", "Zx9kq2", "inside"),
        (
            "pre-recorded-token",
            "pre-rec",
            "overlaps the IAM placeholder",
        ),
        ("ababababababababa", "ababababababa", "self-overlapping"),
        ("9.9.9", "9", "a one-character secret"),
        (
            long_a.as_str(),
            long_secret.as_str(),
            "near match, secret longer: not held",
        ),
    ] {
        let (status, stdout, stderr) = server_line(version, "standalone", &["--password", secret]);
        assert_eq!(status, 0, "{why}: {stderr}");
        assert!(
            stdout.contains(&format!("engine version {UNKNOWN}, mode={UNKNOWN}")),
            "{why}: {stdout}"
        );
        assert!(
            !stdout.contains(&format!("version {version}")),
            "{why}: {stdout}"
        );
    }
}

#[test]
fn the_active_iam_token_is_a_secret_of_the_server_gate() {
    // The replay signer hands out the token `recorded-token`.
    let (status, stdout, stderr) = replay_connection(
        r#"{"cache":{"iam_token":true,"ping_ms":1.0,"server":{"version":"7.recorded-token.1","mode":"cluster"}}}"#,
        &["--iam-auth", "--iam-user", "u", "--cache-name", "c"],
    );
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains("engine version unknown, mode=unknown"),
        "{stdout}"
    );
    assert!(!stdout.contains("recorded-token"), "{stdout}");
}

#[test]
fn server_text_in_reviewed_encodings_never_shows_the_password() {
    // #5004: form-urlencoded; #5007: overlapping and self-overlapping.
    let password = "marmalade zqx742qz/synthetic+value";
    for version in [
        "marmalade+zqx742qz%2Fsynthetic%2Bvalue",
        "marmalade%20zqx742qz%2Fsynthetic%2Bvalue",
        "marmalade zqx742qz/synthetic+value",
    ] {
        let (status, stdout, stderr) =
            server_line(version, "standalone", &["--password", password]);
        assert_eq!(status, 0, "{version}: {stderr}");
        let all = format!("{stdout}{stderr}");
        for part in ["marmalade", "zqx742qz", "synthetic", "%2F", "%2B"] {
            assert!(!all.contains(part), "{version}: {part:?} leaked: {all}");
        }
    }
    for (password, text) in [
        ("qzqzqzqzqzqzq", "qzqzqzqzqzqzqzqzq"),
        ("pre-rec", "pre-recorded-token"),
    ] {
        let (_, stdout, stderr) = server_line(text, "cluster", &["--password", password]);
        let all = format!("{stdout}{stderr}");
        assert!(
            !all.contains("zqzq") && !all.contains("orded-token"),
            "{text}: {all}"
        );
    }
}

#[test]
fn a_very_large_server_value_is_unknown_and_never_scanned_whole() {
    // #5005: a long near-match input. With a credential the gate checks only
    // its boolean state, without inspecting the server value.
    let value = "a".repeat(1_048_576);
    let secret = format!("{}b", "a".repeat(5000));
    let (status, stdout, stderr) = server_line(&value, "standalone", &["--password", &secret]);
    assert_eq!(status, 0, "{stderr}");
    assert!(
        stdout.contains(&format!(
            "  Server: engine version {UNKNOWN}, mode={UNKNOWN}\n"
        )),
        "{}",
        &stdout[..stdout.len().min(300)]
    );
    assert!(stdout.len() < 400, "{}", stdout.len());
}
