//! Replay of a recorded run (`--recorded FILE`) through the tunnel seams.
//!
//! One JSON object; each command accepts only the members it reads:
//!
//! ```json
//! {
//!   "calls": [
//!     {"service": "ec2", "operation": "DescribeInstanceStatus",
//!      "request": {"InstanceIds": ["i-0abc12345"], "IncludeAllInstances": true},
//!      "response": {"InstanceStatuses": [{"InstanceState": {"Name": "running"}}]}}
//!   ],
//!   "host": {"programs": ["aws", "session-manager-plugin"], "busy_ports": [],
//!            "port_checks": [true]},
//!   "tunnel": {"command": ["aws", "ssm", "start-session", "..."],
//!              "settle_exit_code": null, "exit_output": "",
//!              "events": [{"line": "Starting session"}, {"idle": true}, {"interrupt": true}],
//!              "exit_code": 0, "stops_gracefully": true},
//!   "cache": {"address": "10.0.1.5", "iam_token": true,
//!             "connect": {"request": {"host": "..."}, "init_error": null},
//!             "ping_ms": 1.5, "ping_error": {"kind": "connection", "message": "..."},
//!             "server": {"version": "7.1.0", "mode": "standalone"}}
//! }
//! ```
//!
//! - `calls` are `ec2`/`ssm` steps (`DescribeInstances`, `DescribeInstanceStatus`,
//!   `DescribeInstanceInformation`) replayed in order per operation by the
//!   shared `toolkit_aws::recorded::StepQueue`; the request of every step,
//!   `error` steps included, is compared with the request made before the
//!   outcome is replayed. An `error` step has `kind` `client` (default),
//!   `connect` or `no_credentials`, `code` and `message`.
//! - `host.programs` are the programs on `PATH`; `busy_ports` are the local
//!   ports in use; `port_checks` answer each connection probe in order.
//! - `tunnel` scripts the process: `settle_exit_code` is its exit code after the
//!   2 s settle (`null`/absent: still running), `events` its output, idle
//!   periods and a Ctrl+C, `exit_code` its exit once the output ends,
//!   `stops_gracefully` whether it stops within the 5 s grace period after
//!   SIGTERM. `command`, when given, must equal the command the run builds.
//! - `cache` scripts `test-connection`; `connect.request` (when given) is the
//!   connect plan without the credentials. A run that builds a client needs
//!   exactly one of `ping_ms` and `ping_error`. Each of `address`, `iam_token`,
//!   `connect`, the PING outcome and `server` is consumed by its own call, and
//!   one no call used fails the run.
//!
//! Every optional member is absent or of its type (`null` rejects the
//! recording) except where this list says `null`. A call, probe or process
//! with nothing recorded, a request that differs, and a recorded step or event
//! no run used all fail. A recording holds no credential: a password or token
//! never appears in it and is never compared.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::net::IpAddr;
use std::rc::Rc;
use std::time::Duration;

use toolkit_aws::error::TimeoutPhase;
use toolkit_aws::recorded::{
    object_members, optional_list, optional_member, optional_object, optional_object_list,
    optional_string_list, optional_text, same_json, step_fields, KeyOrder, MemberError,
    RecordedOutcome, ReplayError, Step, StepQueue, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::seams::{
    CacheConnection, CacheEnvironment, ConnectPlan, IamTokenRequest, Operation, PingFailure,
    ProcessEvent, Resolution, Seam, SeamError, ServerInfo, TunnelAws, TunnelHost, TunnelProcess,
};

fn quoted(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

fn only_members(object: &Json, allowed: &[&str], what: &str) -> Result<(), String> {
    let members = object_members(object).ok_or_else(|| format!("{what} must be an object"))?;
    match members
        .iter()
        .find(|(key, _)| !allowed.contains(&key.as_str()))
    {
        // The member name is the recording's own text: only its length and the
        // list of the accepted names are reported.
        Some((key, _)) => Err(format!(
            "{what} has a member this command does not read ({} characters); accepted: {}",
            key.chars().count(),
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

fn int_member(object: &Json, key: &str) -> Result<Option<i64>, String> {
    optional_member(object, key, "an integer", |member| match member {
        Json::Int(value) => Some(*value),
        _ => None,
    })
    .map_err(|error| quoted(&error))
}

fn bool_member(object: &Json, key: &str) -> Result<Option<bool>, String> {
    optional_member(object, key, "a boolean", |member| match member {
        Json::Bool(value) => Some(*value),
        _ => None,
    })
    .map_err(|error| quoted(&error))
}

fn number_member(object: &Json, key: &str) -> Result<Option<f64>, String> {
    optional_member(object, key, "a number", |member| match member {
        Json::Int(value) => Some(*value as f64),
        Json::Float(value) => Some(*value),
        _ => None,
    })
    .map_err(|error| quoted(&error))
}

fn text_member<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, String> {
    optional_text(object, key).map_err(|error| quoted(&error))
}

fn object_member<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, String> {
    optional_object(object, key).map_err(|error| quoted(&error))
}

fn exit_code(object: &Json, key: &str) -> Result<Option<i32>, String> {
    match int_member(object, key)? {
        None => Ok(None),
        Some(value) => i32::try_from(value)
            .map(Some)
            .map_err(|_| format!("\"{key}\" is out of range")),
    }
}

/// A recording, parsed once; the members a command does not accept are
/// refused by name list.
pub(super) struct Recording {
    pub aws: RecordedAws,
    pub host: RecordedHost,
    pub cache: RecordedCache,
}

impl Recording {
    /// # Errors
    ///
    /// A reason naming the member and the wrong type, never a value.
    pub(super) fn from_document(document: &Json, accepted: &[&str]) -> Result<Self, String> {
        only_members(document, accepted, "the recorded document")?;
        Ok(Self {
            aws: RecordedAws::from_document(document)?,
            host: RecordedHost::from_document(document)?,
            cache: RecordedCache::from_document(document)?,
        })
    }
}

// ------------------------------------------------------------------ AWS

/// Replays the `calls` of a recording.
pub(super) struct RecordedAws {
    queue: StepQueue<&'static str>,
}

impl RecordedAws {
    fn from_document(document: &Json) -> Result<Self, String> {
        let calls = optional_object_list(document, "calls").map_err(|error| quoted(&error))?;
        let mut queue = StepQueue::new(KeyOrder::Ignored);
        for (index, call) in calls.iter().enumerate() {
            let bad = |what: &str| format!("recorded call {index}: {what}");
            let service = text_member(call, "service")
                .map_err(|e| bad(&e))?
                .unwrap_or("");
            let name = text_member(call, "operation")
                .map_err(|e| bad(&e))?
                .unwrap_or("");
            let operation = Operation::from_key(&format!("{service}:{name}")).ok_or_else(|| {
                bad("\"service\" and \"operation\" must be ec2 DescribeInstances, ec2 DescribeInstanceStatus or ssm DescribeInstanceInformation")
            })?;
            let fields = step_fields(call).map_err(|error| match error {
                StepShapeError::Request(member) => bad(&quoted(&member)),
                StepShapeError::Outcome | StepShapeError::ErrorNotObject => bad(
                    "exactly one of \"response\" (an object) and \"error\" (an object) is required",
                ),
            })?;
            let request = fields.request.cloned();
            let step = match fields.outcome {
                RecordedOutcome::Response(response) => Step::response(request, response.clone()),
                RecordedOutcome::Error(error) => {
                    let message = text_member(error, "message").map_err(|e| bad(&e))?;
                    let code = text_member(error, "code").map_err(|e| bad(&e))?;
                    let kind = text_member(error, "kind").map_err(|e| bad(&e))?;
                    let message = message.unwrap_or_default();
                    let failure =
                        match kind.unwrap_or("client") {
                            "client" => AwsError::Service(Box::new(ServiceError::new(
                                "POST",
                                "/",
                                400,
                                Some(code.unwrap_or_default()),
                                message,
                                None,
                            ))),
                            "connect" => AwsError::connect(message),
                            "timeout" => AwsError::timeout(TimeoutPhase::Read, message),
                            "no_credentials" => AwsError::NoCredentials,
                            _ => return Err(bad(
                                "\"error.kind\" must be client, connect, timeout or no_credentials",
                            )),
                        };
                    Step::failure(request, failure)
                }
            };
            queue.push(operation.key(), step);
        }
        Ok(Self { queue })
    }
}

impl TunnelAws for RecordedAws {
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, SeamError> {
        match self.queue.replay(&operation.key(), request) {
            Ok(outcome) => outcome.map_err(SeamError::Aws),
            Err(ReplayError::Exhausted) => Err(SeamError::Replay(format!(
                "no recorded step left for {}",
                operation.key()
            ))),
            Err(ReplayError::RequestMismatch) => Err(SeamError::Replay(format!(
                "the recorded request of {} differs from the request made",
                operation.key()
            ))),
        }
    }

    fn unconsumed(&self) -> usize {
        self.queue.unconsumed()
    }
}

// ----------------------------------------------------------------- host

struct RecordedTunnel {
    command: Option<Vec<String>>,
    settle_exit: Option<i32>,
    exit_output: String,
    events: VecDeque<ProcessEvent>,
    exit_code: i32,
    stops_gracefully: bool,
}

/// Replays `host` and `tunnel`.
pub(super) struct RecordedHost {
    programs: Vec<String>,
    busy_ports: Vec<i64>,
    port_checks: VecDeque<bool>,
    tunnel: Option<RecordedTunnel>,
    spawned: bool,
    events_left: Rc<Cell<usize>>,
}

impl RecordedHost {
    fn from_document(document: &Json) -> Result<Self, String> {
        let mut host = Self {
            programs: Vec::new(),
            busy_ports: Vec::new(),
            port_checks: VecDeque::new(),
            tunnel: None,
            spawned: false,
            events_left: Rc::new(Cell::new(0)),
        };
        if let Some(section) = object_member(document, "host")? {
            only_members(
                section,
                &["programs", "busy_ports", "port_checks"],
                "\"host\"",
            )?;
            host.programs = optional_string_list(section, "programs")
                .map_err(|error| quoted(&error))?
                .into_iter()
                .map(str::to_string)
                .collect();
            for port in optional_list(section, "busy_ports").map_err(|e| quoted(&e))? {
                match port {
                    Json::Int(value) => host.busy_ports.push(*value),
                    _ => return Err("\"busy_ports\" must be a list of integers".to_string()),
                }
            }
            for check in optional_list(section, "port_checks").map_err(|e| quoted(&e))? {
                match check {
                    Json::Bool(value) => host.port_checks.push_back(*value),
                    _ => return Err("\"port_checks\" must be a list of booleans".to_string()),
                }
            }
        }
        if let Some(section) = object_member(document, "tunnel")? {
            host.tunnel = Some(parse_tunnel(section)?);
        }
        Ok(host)
    }
}

fn parse_tunnel(section: &Json) -> Result<RecordedTunnel, String> {
    only_members(
        section,
        &[
            "command",
            "settle_exit_code",
            "exit_output",
            "events",
            "exit_code",
            "stops_gracefully",
        ],
        "\"tunnel\"",
    )?;
    let command = match section.get("command") {
        None => None,
        Some(_) => Some(
            optional_string_list(section, "command")
                .map_err(|error| quoted(&error))?
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
    };
    let mut events = VecDeque::new();
    for (index, event) in optional_object_list(section, "events")
        .map_err(|error| quoted(&error))?
        .iter()
        .enumerate()
    {
        let bad = |what: &str| format!("tunnel event {index}: {what}");
        only_members(event, &["line", "idle", "interrupt"], "an event")?;
        let line = text_member(event, "line").map_err(|e| bad(&e))?;
        let idle = bool_member(event, "idle").map_err(|e| bad(&e))?;
        let interrupt = bool_member(event, "interrupt").map_err(|e| bad(&e))?;
        events.push_back(match (line, idle, interrupt) {
            (Some(line), None, None) => ProcessEvent::Line(line.to_string()),
            (None, Some(true), None) => ProcessEvent::Idle,
            (None, None, Some(true)) => ProcessEvent::Interrupted,
            _ => {
                return Err(bad(
                    "exactly one of \"line\", \"idle\": true and \"interrupt\": true",
                ))
            }
        });
    }
    Ok(RecordedTunnel {
        command,
        settle_exit: match section.get("settle_exit_code") {
            Some(Json::Null) | None => None,
            Some(_) => exit_code(section, "settle_exit_code")?,
        },
        exit_output: text_member(section, "exit_output")?
            .unwrap_or_default()
            .to_string(),
        events,
        exit_code: exit_code(section, "exit_code")?.unwrap_or(0),
        stops_gracefully: bool_member(section, "stops_gracefully")?.unwrap_or(true),
    })
}

struct RecordedProcess {
    settle_exit: Option<i32>,
    exit_output: String,
    events: VecDeque<ProcessEvent>,
    exit_code: i32,
    stops_gracefully: bool,
    left: Rc<Cell<usize>>,
}

impl TunnelProcess for RecordedProcess {
    fn exited_after(&mut self, _settle: Duration) -> Result<Option<i32>, SeamError> {
        Ok(self.settle_exit)
    }

    fn drain_output(&mut self) -> Result<String, SeamError> {
        Ok(self.exit_output.clone())
    }

    fn next_event(&mut self, _idle: Duration) -> Result<ProcessEvent, SeamError> {
        let event = self.events.pop_front().unwrap_or(ProcessEvent::Eof);
        self.left.set(self.events.len());
        Ok(event)
    }

    fn terminate(&mut self) {}

    fn wait(&mut self, _timeout: Duration) -> Result<Option<i32>, SeamError> {
        Ok(self.stops_gracefully.then_some(self.exit_code))
    }

    fn kill(&mut self) {}

    fn wait_exit(&mut self) -> Result<i32, SeamError> {
        Ok(self.exit_code)
    }
}

impl TunnelHost for RecordedHost {
    fn program_on_path(&mut self, program: &str) -> Result<bool, SeamError> {
        Ok(self.programs.iter().any(|name| name == program))
    }

    fn local_port_free(&mut self, port: u16) -> Result<bool, SeamError> {
        Ok(!self.busy_ports.contains(&i64::from(port)))
    }

    fn port_accepts(&mut self, _port: u16, _timeout: Duration) -> Result<bool, SeamError> {
        self.port_checks
            .pop_front()
            .ok_or_else(|| SeamError::Replay("no recorded port check left".to_string()))
    }

    fn spawn(&mut self, command: &[String]) -> Result<Box<dyn TunnelProcess>, SeamError> {
        let Some(tunnel) = self.tunnel.take() else {
            return Err(SeamError::Replay("no recorded tunnel process".to_string()));
        };
        self.spawned = true;
        if tunnel
            .command
            .as_deref()
            .is_some_and(|expected| expected != command)
        {
            return Err(SeamError::Replay(
                "the recorded tunnel command differs from the command built".to_string(),
            ));
        }
        self.events_left.set(tunnel.events.len());
        Ok(Box::new(RecordedProcess {
            settle_exit: tunnel.settle_exit,
            exit_output: tunnel.exit_output,
            events: tunnel.events,
            exit_code: tunnel.exit_code,
            stops_gracefully: tunnel.stops_gracefully,
            left: Rc::clone(&self.events_left),
        }))
    }

    fn unconsumed(&self) -> usize {
        self.port_checks.len()
            + self.events_left.get()
            + usize::from(self.tunnel.is_some() && !self.spawned)
    }
}

// ---------------------------------------------------------------- cache

/// The one recorded result of `PING`.
enum PingOutcome {
    Pong(f64),
    Failed(PingFailure),
}

/// The recorded cache steps no call has used yet. Each is taken by the call it
/// answers (DNS, the signer, the connect, `PING`, `INFO`), on the environment
/// and on the connection it returns alike, so a step a run never reached is
/// counted (#4988).
#[derive(Default)]
struct Pending {
    address: Option<IpAddr>,
    iam_token: bool,
    expected_plan: Option<Json>,
    init_error: Option<String>,
    ping: Option<PingOutcome>,
    server: Option<ServerInfo>,
}

impl Pending {
    fn unused(&self) -> usize {
        usize::from(self.address.is_some())
            + usize::from(self.iam_token)
            + usize::from(self.expected_plan.is_some())
            + usize::from(self.init_error.is_some())
            + usize::from(self.ping.is_some())
            + usize::from(self.server.is_some())
    }
}

/// Replays `cache`.
pub(super) struct RecordedCache {
    present: bool,
    pending: Rc<RefCell<Pending>>,
}

impl RecordedCache {
    fn from_document(document: &Json) -> Result<Self, String> {
        let mut cache = Self {
            present: false,
            pending: Rc::new(RefCell::new(Pending::default())),
        };
        let Some(section) = object_member(document, "cache")? else {
            return Ok(cache);
        };
        only_members(
            section,
            &[
                "address",
                "iam_token",
                "connect",
                "ping_ms",
                "ping_error",
                "server",
            ],
            "\"cache\"",
        )?;
        cache.present = true;
        let mut pending = Pending {
            address: match text_member(section, "address")? {
                None => None,
                Some(text) => Some(
                    text.parse::<IpAddr>()
                        .map_err(|_| "\"address\" must be an IP address".to_string())?,
                ),
            },
            iam_token: bool_member(section, "iam_token")?.unwrap_or(false),
            ..Pending::default()
        };
        if let Some(connect) = object_member(section, "connect")? {
            only_members(connect, &["request", "init_error"], "\"connect\"")?;
            pending.expected_plan = object_member(connect, "request")?.cloned();
            pending.init_error = match connect.get("init_error") {
                Some(Json::Null) | None => None,
                Some(_) => text_member(connect, "init_error")?.map(str::to_string),
            };
        }
        let failure = match object_member(section, "ping_error")? {
            None => None,
            Some(error) => {
                only_members(error, &["kind", "message"], "\"ping_error\"")?;
                let message = text_member(error, "message")?
                    .unwrap_or_default()
                    .to_string();
                Some(match text_member(error, "kind")?.unwrap_or("other") {
                    "authentication" => PingFailure::Authentication(message),
                    "connection" => PingFailure::Connection(message),
                    "other" => PingFailure::Other(message),
                    _ => {
                        return Err(
                            "\"ping_error.kind\" must be authentication, connection or other"
                                .to_string(),
                        )
                    }
                })
            }
        };
        let ping_ms = number_member(section, "ping_ms")?;
        if ping_ms.is_some_and(|ms| !ms.is_finite() || ms < 0.0) {
            return Err("\"ping_ms\" must be a finite, non-negative number".to_string());
        }
        pending.ping = match (ping_ms, failure) {
            (Some(_), Some(_)) => {
                return Err(
                    "\"cache\" has both \"ping_ms\" and \"ping_error\"; record exactly one PING outcome"
                        .to_string(),
                )
            }
            (Some(ms), None) => Some(PingOutcome::Pong(ms)),
            (None, Some(failure)) => Some(PingOutcome::Failed(failure)),
            (None, None) => None,
        };
        pending.server = match object_member(section, "server")? {
            None => None,
            Some(server) => {
                only_members(server, &["version", "mode"], "\"server\"")?;
                Some(ServerInfo {
                    version: text_member(server, "version")?
                        .unwrap_or("unknown")
                        .to_string(),
                    mode: text_member(server, "mode")?
                        .unwrap_or("unknown")
                        .to_string(),
                })
            }
        };
        cache.pending = Rc::new(RefCell::new(pending));
        Ok(cache)
    }
}

/// The connect plan as a JSON object, credentials left out: the kind of auth
/// only.
pub(super) fn plan_request(plan: &ConnectPlan) -> Json {
    let auth = match (&plan.username, &plan.password) {
        (Some(_), _) => "user",
        (None, Some(_)) => "password",
        (None, None) => "none",
    };
    let mut members = vec![
        ("host", Json::string(&plan.host)),
        ("port", Json::Int(i64::from(plan.port))),
        ("tls", Json::Bool(plan.tls)),
        ("check_hostname", Json::Bool(plan.check_hostname)),
        ("cluster_client", Json::Bool(plan.cluster_client)),
        ("timeout_secs", Json::Float(plan.timeout_secs)),
        ("auth", Json::string(auth)),
    ];
    if let Some(name) = &plan.server_name {
        members.push(("server_name", Json::string(name)));
    }
    if let Some(user) = &plan.username {
        members.push(("username", Json::string(user)));
    }
    Json::object(members)
}

struct RecordedConnection {
    pending: Rc<RefCell<Pending>>,
}

impl CacheConnection for RecordedConnection {
    fn ping(&mut self) -> Result<f64, PingFailure> {
        match self.pending.borrow_mut().ping.take() {
            Some(PingOutcome::Pong(ms)) => Ok(ms),
            Some(PingOutcome::Failed(failure)) => Err(failure),
            // `connect` checks that the outcome is recorded; a second PING
            // has none.
            None => Err(PingFailure::Other(
                "no recorded PING outcome is left".to_string(),
            )),
        }
    }

    fn info_server(&mut self) -> Option<ServerInfo> {
        self.pending.borrow_mut().server.take()
    }

    fn close(&mut self) {}
}

impl CacheEnvironment for RecordedCache {
    fn resolve(&mut self, _host: &str) -> Result<Resolution, SeamError> {
        if !self.present {
            return Err(SeamError::Replay("no recorded cache".to_string()));
        }
        Ok(self
            .pending
            .borrow_mut()
            .address
            .take()
            .map_or(Resolution::NotFound, Resolution::Address))
    }

    fn iam_token(&mut self, _request: &IamTokenRequest) -> Result<toolkit_aws::Secret, SeamError> {
        let mut pending = self.pending.borrow_mut();
        if std::mem::take(&mut pending.iam_token) {
            // A placeholder: the recording holds no token and the replay
            // client never reads it.
            Ok(toolkit_aws::Secret::new("recorded-token"))
        } else {
            Err(SeamError::Unavailable(Seam::IamSigner))
        }
    }

    fn connect(
        &mut self,
        plan: &ConnectPlan,
    ) -> Result<Result<Box<dyn CacheConnection>, String>, SeamError> {
        if !self.present {
            return Err(SeamError::Replay("no recorded cache".to_string()));
        }
        let mut pending = self.pending.borrow_mut();
        if let Some(expected) = pending.expected_plan.take() {
            if !same_json(&expected, &plan_request(plan)) {
                return Err(SeamError::Replay(
                    "the recorded connect plan differs from the plan built".to_string(),
                ));
            }
        }
        if let Some(message) = pending.init_error.take() {
            return Ok(Err(message));
        }
        // A client that is built is pinged: the outcome must be recorded, never
        // assumed.
        if pending.ping.is_none() {
            return Err(SeamError::Replay(
                "the recorded cache has no PING outcome: record exactly one of \"ping_ms\" and \"ping_error\""
                    .to_string(),
            ));
        }
        drop(pending);
        Ok(Ok(Box::new(RecordedConnection {
            pending: Rc::clone(&self.pending),
        })))
    }

    fn client_name(&self) -> &'static str {
        "recorded replay"
    }

    fn unconsumed(&self) -> usize {
        self.pending.borrow().unused()
    }
}
