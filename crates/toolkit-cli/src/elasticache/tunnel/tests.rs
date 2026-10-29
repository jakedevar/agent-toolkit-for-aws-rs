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

use super::clean::{clean, clean_to};
use super::connect::{self, is_private, ConnectArgs};
use super::discover::{self, DiscoverArgs};
use super::recorded::Recording;
use super::seams::{
    CacheConnection, CacheEnvironment, ConnectPlan, IamTokenRequest, Operation, PingFailure,
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
    ping: Result<f64, PingFailure>,
    info: Option<ServerInfo>,
    log: Log,
}

impl CacheConnection for MockConnection {
    fn ping(&mut self) -> Result<f64, PingFailure> {
        self.log.borrow_mut().push("ping".to_string());
        self.ping.clone()
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
    init_error: Option<String>,
    ping: Result<f64, PingFailure>,
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
    ) -> Result<Result<Box<dyn CacheConnection>, String>, SeamError> {
        self.plans.push(plan.clone());
        if let Some(message) = self.init_error.take() {
            return Ok(Err(message));
        }
        Ok(Ok(Box::new(MockConnection {
            ping: self.ping.clone(),
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
         \x20 Server: engine version 7.1.0, mode=standalone\n\
         \x20 TLS: enabled (in-transit encryption active)\n\
         \x20 Result: CONNECTION SUCCESSFUL\n"
    );
    assert_eq!(run.stderr, "");
    assert_eq!(logged(&cache.log), ["ping", "info", "close"]);
}

#[test]
fn test_connection_never_prints_the_password_or_token() {
    let secret = "hunter2-PASSWORD";
    for failure in [
        Ok(1.0),
        Err(PingFailure::Authentication("bad credentials".to_string())),
        Err(PingFailure::Connection("SSL: handshake".to_string())),
        Err(PingFailure::Other("boom".to_string())),
    ] {
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
fn test_connection_an_error_message_with_a_credential_is_redacted() {
    let mut cache = MockCache::healthy();
    cache.ping = Err(PingFailure::Other(
        "denied for AKIAIOSFODNN7EXAMPLE\u{1b}[0m".to_string(),
    ));
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert_eq!(run.status, 1);
    assert!(
        !run.stderr.contains("AKIAIOSFODNN7EXAMPLE") && !run.stderr.contains('\u{1b}'),
        "{}",
        run.stderr
    );
    assert!(run.stderr.starts_with("  PING FAILED: "));
}

#[test]
fn test_connection_authentication_failure_hints_at_the_rbac_user() {
    let mut cache = MockCache::healthy();
    cache.ping = Err(PingFailure::Authentication("WRONGPASS".to_string()));
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert_eq!(run.status, 1);
    assert_eq!(
        run.stderr,
        "  PING FAILED: Authentication error -- WRONGPASS\n  Hint: Check username/password or ensure the RBAC user exists and is active.\n"
    );
    assert!(!run.stdout.contains("CONNECTION SUCCESSFUL"));
    assert!(!logged(&cache.log).contains(&"info".to_string()));
}

#[test]
fn test_connection_connection_failures_pick_the_tls_or_network_hint() {
    let hint = |message: &str, no_tls: bool| {
        let mut cache = MockCache::healthy();
        cache.ping = Err(PingFailure::Connection(message.to_string()));
        let args = ConnectArgs {
            no_tls,
            ..connect_args(NODE_HOST)
        };
        let run = run_connect(&args, &mut cache);
        assert_eq!(run.status, 1);
        run.stderr.lines().nth(1).expect("a hint").to_string()
    };
    assert_eq!(
        hint("SSL handshake failed", false),
        "  Hint: TLS handshake failed. Verify the endpoint supports in-transit encryption."
    );
    assert_eq!(
        hint("tls error", false),
        "  Hint: TLS handshake failed. Verify the endpoint supports in-transit encryption."
    );
    assert_eq!(
        hint("SSL handshake failed", true),
        "  Hint: This endpoint may require TLS. Retry without --no-tls (TLS is default)."
    );
    assert_eq!(hint("timed out", false), "  Hint: Check security group rules, VPC placement, and whether you need a tunnel or jump host.");
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
    cache.init_error = Some("bad config".to_string());
    let run = run_connect(&connect_args(NODE_HOST), &mut cache);
    assert_eq!(run.status, 1);
    assert_eq!(run.stderr, "  Client init FAILED: bad config\n");
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
    let escapes = format!("{}{}", "\u{1b}".repeat(10), "x");
    assert_eq!(
        clean_to(&escapes, 5),
        "\\x1b\\x1b\\x1b\\x1b\\x1b...",
        "escaping happens after the bound"
    );
    assert_eq!(clean_to("abc", 3), "abc");
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
    assert!(stdout.contains("  PING: OK (2.0 ms)\n  Server: engine version 7.1.0, mode=cluster\n"));
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
        r#"{"cache":{"address":"10.0.0.9","ping_error":{"kind":"authentication","message":"WRONGPASS"}}}"#,
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
    assert!(stderr.contains("Authentication error -- WRONGPASS"));
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

// ------------------------------------------- #4986 secrets in client text

/// Percent-encoding of every byte but the unreserved ones (RFC 3986), the
/// way a client puts a credential into a URL.
fn percent_encoded(text: &str, upper: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else if upper {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push_str(&format!("%{byte:02x}"));
        }
    }
    out
}

fn hex_encoded(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// The renderings of a credential in client error text that a scrubber must
/// know: raw, quoted, escaped for JSON, percent-encoded, hex.
fn renderings(secret: &str) -> Vec<String> {
    vec![
        secret.to_string(),
        format!("{secret:?}"),
        toolkit_core::escape_json_string(secret),
        percent_encoded(secret, true),
        percent_encoded(secret, false),
        hex_encoded(secret),
    ]
}

/// Every error text a cache client can hand back, with `form` inside it.
fn client_errors(form: &str) -> Vec<(&'static str, String)> {
    vec![
        ("init", format!("cannot build client for {form} here")),
        ("authentication", format!("WRONGPASS {form} rejected")),
        ("connection", format!("Error connecting with {form}")),
        (
            "connection (tls)",
            format!("SSL handshake with {form} failed"),
        ),
        ("other", format!("unexpected {form} reply")),
    ]
}

fn failing_cache(kind: &str, message: String) -> MockCache {
    let mut cache = MockCache::healthy();
    match kind {
        "init" => cache.init_error = Some(message),
        "authentication" => cache.ping = Err(PingFailure::Authentication(message)),
        "connection" | "connection (tls)" => cache.ping = Err(PingFailure::Connection(message)),
        _ => cache.ping = Err(PingFailure::Other(message)),
    }
    cache
}

fn assert_no_leak(run: &Captured, forms: &[String], context: &str) {
    let all = format!("{}{}", run.stdout, run.stderr);
    for form in forms {
        assert!(
            !all.contains(form.as_str()),
            "{context}: {form:?} leaked into:\n{all}"
        );
    }
    if let Err(error) = &run.result {
        let text = format!("{error:?}{error}");
        for form in forms {
            assert!(
                !text.contains(form.as_str()),
                "{context}: error leaked {form:?}"
            );
        }
    }
}

#[test]
fn test_connection_removes_the_active_password_from_every_client_error() {
    for password in [
        "hunter2-PASSWORD",
        "p@ss/w0rd+=&\"q",
        "a b\\c'd",
        "0123456789abcdef0123456789",
    ] {
        let forms = renderings(password);
        for form in &forms {
            for (kind, message) in client_errors(form) {
                let mut cache = failing_cache(kind, message);
                let args = ConnectArgs {
                    password: Some(password),
                    ..connect_args(NODE_HOST)
                };
                let run = run_connect(&args, &mut cache);
                assert_eq!(run.status, 1, "{kind}");
                assert!(run.stderr.contains("[REDACTED]"), "{kind}: {}", run.stderr);
                assert_no_leak(&run, &forms, kind);
            }
        }
    }
}

#[test]
fn test_connection_the_password_in_the_password_auth_flow_is_scrubbed_too() {
    let password = "unlabelled-S3CRET-0042";
    for (kind, message) in client_errors(password) {
        let mut cache = failing_cache(kind, message);
        let args = ConnectArgs {
            username: None,
            password: Some(password),
            ..connect_args(NODE_HOST)
        };
        let run = run_connect(&args, &mut cache);
        assert_eq!(run.status, 1, "{kind}");
        assert_no_leak(&run, &renderings(password), kind);
    }
}

#[test]
fn test_connection_scrubs_before_bounding_and_escaping() {
    let password = "hunter2-PASSWORD";
    // The bound (200 characters) would cut the password in two and leak the
    // front half.
    let near_the_bound = format!("{}{password}{}", "x".repeat(196), "y".repeat(50));
    // A control character inside the password is escaped by the last step.
    let hostile = "tab\there-9X\u{7}";
    let with_controls = format!("a\u{1b}[31m{hostile}\r\nForged: line {hostile}");
    for (secret, text) in [(password, near_the_bound), (hostile, with_controls)] {
        for (kind, _) in client_errors("") {
            let mut cache = failing_cache(kind, text.clone());
            let args = ConnectArgs {
                password: Some(secret),
                ..connect_args(NODE_HOST)
            };
            let run = run_connect(&args, &mut cache);
            let mut forms = renderings(secret);
            forms.push(secret[..5].to_string());
            forms.push(crate::safe_text::safe_path(secret));
            assert_no_leak(&run, &forms, kind);
            assert!(!run.stderr.contains('\u{1b}'), "{}", run.stderr);
        }
    }
}

#[test]
fn test_connection_the_tls_hint_does_not_depend_on_the_password() {
    // A password "tls" must not flip the hint of an unrelated failure.
    let mut cache = MockCache::healthy();
    cache.ping = Err(PingFailure::Connection("refused for tls".to_string()));
    let args = ConnectArgs {
        password: Some("tls"),
        no_tls: true,
        ..connect_args(NODE_HOST)
    };
    let run = run_connect(&args, &mut cache);
    assert!(
        run.stderr.contains("Hint: Check security group rules"),
        "{}",
        run.stderr
    );
}

#[test]
fn test_connection_removes_server_info_that_echoes_the_password() {
    let password = "unlabelled-S3CRET-0042";
    let mut cache = MockCache::healthy();
    cache.info = Some(ServerInfo {
        version: format!("7.1 {password}"),
        mode: percent_encoded(password, true),
    });
    let args = ConnectArgs {
        password: Some(password),
        ..connect_args(NODE_HOST)
    };
    let run = run_connect(&args, &mut cache);
    assert_eq!(run.status, 0);
    assert_no_leak(&run, &renderings(password), "server info");
}

#[test]
fn test_connection_removes_the_iam_token_from_every_client_error() {
    let token =
        "mycache.use1.cache.amazonaws.com/?Action=connect&User=iamuser&Zq9=Wd8zXk33&Ab=Cd/Ef+Gh";
    let supplied = "also-supplied-PW-77";
    let mut forms = renderings(token);
    forms.extend(renderings(supplied));
    for form in forms.clone() {
        for (kind, message) in client_errors(&form) {
            let mut cache = failing_cache(kind, message);
            cache.iam = Some(token);
            let args = ConnectArgs {
                username: None,
                password: Some(supplied),
                iam_auth: true,
                iam_user: Some("iam-user"),
                cache_name: Some("My-Cache"),
                ..connect_args(NODE_HOST)
            };
            let run = run_connect(&args, &mut cache);
            assert_eq!(run.status, 1, "{kind}");
            assert_eq!(cache.iam_requests.len(), 1, "{kind}");
            assert_no_leak(&run, &forms, kind);
            assert!(run.stderr.contains("[REDACTED]"), "{kind}: {}", run.stderr);
        }
    }
}

#[test]
fn test_connection_cli_replay_never_prints_a_password_a_recording_echoes() {
    let password = "unlabelled-S3CRET-0042";
    for recording in [
        format!(r#"{{"cache":{{"connect":{{"init_error":"cannot use {password}"}}}}}}"#),
        format!(
            r#"{{"cache":{{"ping_error":{{"kind":"authentication","message":"WRONGPASS {password}"}}}}}}"#
        ),
        format!(
            r#"{{"cache":{{"ping_error":{{"kind":"connection","message":"closed {password}"}}}}}}"#
        ),
        format!(r#"{{"cache":{{"ping_error":{{"message":"odd {password}"}}}}}}"#),
    ] {
        let file = write_recording(&recording);
        let (status, stdout, stderr) = exec(&[
            "elasticache",
            "test-connection",
            "127.0.0.1",
            "--password",
            password,
            "--recorded",
            file.path(),
        ]);
        assert_eq!(status, 1, "{recording}");
        assert!(
            !format!("{stdout}{stderr}").contains(password),
            "{recording}: {stdout}{stderr}"
        );
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
            r#"{"cache":{"ping_ms":1.0,"ping_error":{"message":"x"}}}"#,
            "both",
        ),
        (
            r#"{"cache":{"ping_ms":1.0,"ping_error":{"kind":"connection","message":"x"}}}"#,
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
        r#"{"cache":{"ping_error":{"kind":"connection","message":"reset"}}}"#,
        &[],
    );
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(stderr.contains("PING FAILED: Connection error -- reset"));

    // A client that never gets built never reaches PING.
    let (status, stdout, stderr) =
        replay_connection(r#"{"cache":{"connect":{"init_error":"no client"}}}"#, &[]);
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(
        stderr.starts_with("  Client init FAILED: no client\n"),
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
        r#"{"cache":{"connect":{"init_error":"no client"},"ping_ms":1.0}}"#,
        &[],
        "ping after init_error",
    );
    assert_unused(
        r#"{"cache":{"connect":{"init_error":"no client"},"server":{"version":"7"}}}"#,
        &[],
        "info after init_error",
    );
    assert_unused(
        r#"{"cache":{"ping_error":{"message":"reset"},"server":{"version":"7"}}}"#,
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
    assert!(stdout.contains("Server: engine version 7.1, mode=cluster"));

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

fn two_instances() -> Result<Json, SeamError> {
    Ok(instances_page(
        &[
            instance("i-0000000b", "plain", "c5.large"),
            instance("i-0000000a", "known-ready", "t3.micro"),
        ],
        None,
    ))
}

fn first_managed_page() -> Result<Json, SeamError> {
    Ok(document(
        r#"{"InstanceInformationList":[{"InstanceId":"i-0000000a"}],"NextToken":"page-2"}"#,
    ))
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
            (Operation::DescribeInstances, two_instances()),
            (Operation::DescribeInstanceInformation, first_managed_page()),
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
        (Operation::DescribeInstances, two_instances()),
        (Operation::DescribeInstanceInformation, first_managed_page()),
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
        (Operation::DescribeInstances, two_instances()),
        (Operation::DescribeInstanceInformation, first_managed_page()),
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
        (Operation::DescribeInstances, two_instances()),
        (Operation::DescribeInstanceInformation, first_managed_page()),
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
