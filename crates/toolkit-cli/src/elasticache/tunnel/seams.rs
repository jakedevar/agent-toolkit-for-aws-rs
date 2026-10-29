//! The seams of the tunnel commands: everything that would touch the network,
//! start a subprocess or talk to Redis/Valkey.
//!
//! This build opens no socket, starts no process and connects to no cache. Each
//! of those actions sits behind a trait here. Without `--recorded FILE` the
//! implementation is an `Unavailable*` type whose every method refuses with
//! [`SeamError::Unavailable`] (a typed error naming the seam, never a value of
//! the request); with a recording the `recorded` module replays it. A live
//! implementation is a follow-up (docs/migration/elasticache-tunnel.md).

use std::time::Duration;

use toolkit_aws::paginate::{collect_pages, next_token_from, Page, PageLimits};
use toolkit_aws::recorded::object_members;
use toolkit_aws::AwsError;
use toolkit_core::{Json, ToolkitError};

use super::clean::clean;

/// The part of the outside world a seam stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seam {
    /// AWS API calls (EC2, SSM).
    Aws,
    /// The local port probe (a bind on `127.0.0.1`).
    LocalPort,
    /// Looking a program up on `PATH`.
    Program,
    /// Spawning and supervising the `aws ssm start-session` process.
    TunnelProcess,
    /// Connecting to the tunnel endpoint to see whether it accepts.
    PortConnect,
    /// DNS resolution of the cache host.
    Dns,
    /// The Redis/Valkey client.
    CacheClient,
    /// The IAM auth token signer (needs AWS credentials).
    IamSigner,
}

impl Seam {
    fn describe(self) -> &'static str {
        match self {
            Self::Aws => "AWS API access",
            Self::LocalPort => "the local port probe",
            Self::Program => "the PATH lookup of the AWS CLI and the Session Manager plugin",
            Self::TunnelProcess => "starting the SSM tunnel process",
            Self::PortConnect => "the connection probe of the tunnel port",
            Self::Dns => "DNS resolution",
            Self::CacheClient => "the Redis/Valkey client",
            Self::IamSigner => "IAM auth token signing",
        }
    }
}

/// Why a seam call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SeamError {
    /// No recording was given and this build has no live implementation.
    Unavailable(Seam),
    /// The recording has no answer for this call, or its expected request (or
    /// command) differs from the call made. Carries no recorded value.
    Replay(String),
    /// An AWS call failed (recorded or live).
    Aws(AwsError),
}

impl From<AwsError> for SeamError {
    fn from(error: AwsError) -> Self {
        Self::Aws(error)
    }
}

impl From<SeamError> for ToolkitError {
    fn from(error: SeamError) -> Self {
        match error {
            SeamError::Unavailable(seam) => ToolkitError::runtime(
                "elasticache_seam_unavailable",
                format!(
                    "{} is not available in this build: it opens no sockets, starts no \
                     processes and makes no AWS or Redis/Valkey connections; pass --recorded \
                     FILE to replay a recorded run (docs/migration/elasticache-tunnel.md)",
                    seam.describe()
                ),
            ),
            SeamError::Replay(reason) => {
                ToolkitError::runtime("elasticache_recording_mismatch", clean(&reason))
            }
            SeamError::Aws(error) => {
                ToolkitError::runtime("elasticache_aws_error", clean(&error.to_string()))
            }
        }
    }
}

/// The AWS operations of the tunnel commands.
#[allow(clippy::enum_variant_names)] // the three names are the AWS operation names
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Operation {
    /// `ec2:DescribeInstances`.
    DescribeInstances,
    /// `ec2:DescribeInstanceStatus`.
    DescribeInstanceStatus,
    /// `ssm:DescribeInstanceInformation`.
    DescribeInstanceInformation,
}

impl Operation {
    /// `service:Operation`, the key of a recorded call.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::DescribeInstances => "ec2:DescribeInstances",
            Self::DescribeInstanceStatus => "ec2:DescribeInstanceStatus",
            Self::DescribeInstanceInformation => "ssm:DescribeInstanceInformation",
        }
    }

    /// The operation of a `service:Operation` key.
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        [
            Self::DescribeInstances,
            Self::DescribeInstanceStatus,
            Self::DescribeInstanceInformation,
        ]
        .into_iter()
        .find(|operation| operation.key() == key)
    }
}

/// AWS calls (the `describe_*` calls of the scripts).
pub(crate) trait TunnelAws {
    /// One API call.
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, SeamError>;
    /// Recorded steps a run never used (always 0 for a live client).
    fn unconsumed(&self) -> usize;
}

/// The AWS seam of a run without a recording.
pub(crate) struct UnavailableAws;

impl TunnelAws for UnavailableAws {
    fn call(&mut self, _operation: Operation, _request: &Json) -> Result<Json, SeamError> {
        Err(SeamError::Unavailable(Seam::Aws))
    }

    fn unconsumed(&self) -> usize {
        0
    }
}

/// `request` with `NextToken` set (the `boto3` paginator does the same).
fn with_token(request: &Json, token: Option<&str>) -> Json {
    let mut members: Vec<(String, Json)> = object_members(request)
        .map(<[(String, Json)]>::to_vec)
        .unwrap_or_default();
    if let Some(token) = token {
        members.push(("NextToken".to_string(), Json::string(token)));
    }
    Json::Object(members)
}

/// Walks every page of `operation` (a `boto3` paginator): `parse` turns one
/// response into items. A seam failure other than an AWS error ends the walk
/// as it is, so it is never mistaken for a service error.
pub(crate) fn paged<T: Clone>(
    aws: &mut dyn TunnelAws,
    operation: Operation,
    request: &Json,
    parse: impl Fn(&Json) -> Result<Vec<T>, String>,
) -> Result<Vec<T>, SeamError> {
    match paged_partial(aws, operation, request, parse)? {
        (items, None) => Ok(items),
        (_, Some(error)) => Err(SeamError::Aws(error)),
    }
}

/// [`paged`] that keeps the items of the pages read before a service error:
/// `(items, None)` for a whole walk, `(items so far, Some(error))` when a
/// later page failed (as `ssm_managed` in `find_tunnel_host.py`, which lives
/// outside its `try`). Only an AWS error is kept; any other seam failure is
/// still `Err`.
pub(crate) fn paged_partial<T: Clone>(
    aws: &mut dyn TunnelAws,
    operation: Operation,
    request: &Json,
    parse: impl Fn(&Json) -> Result<Vec<T>, String>,
) -> Result<(Vec<T>, Option<AwsError>), SeamError> {
    let mut fatal: Option<SeamError> = None;
    let mut kept: Vec<T> = Vec::new();
    let result = collect_pages(&PageLimits::default(), |token| {
        let response = match aws.call(operation, &with_token(request, token)) {
            Ok(response) => response,
            Err(SeamError::Aws(error)) => return Err(error),
            Err(other) => {
                fatal = Some(other);
                return Err(AwsError::connect("seam failure"));
            }
        };
        let items = parse(&response).map_err(AwsError::invalid_response)?;
        let next_token = next_token_from(&response, "NextToken")?;
        kept.extend(items.iter().cloned());
        Ok(Page { items, next_token })
    });
    match (fatal, result) {
        (Some(fatal), _) => Err(fatal),
        (None, Ok(items)) => Ok((items, None)),
        (None, Err(error)) => Ok((kept, Some(error))),
    }
}

/// Local host facts and the tunnel process.
pub(crate) trait TunnelHost {
    /// Whether `program` is on `PATH` (`shutil.which`).
    fn program_on_path(&mut self, program: &str) -> Result<bool, SeamError>;
    /// Whether nothing is bound to `127.0.0.1:port` (`check_port_available`).
    fn local_port_free(&mut self, port: u16) -> Result<bool, SeamError>;
    /// Whether `127.0.0.1:port` accepts a connection within `timeout`
    /// (`wait_for_port`).
    fn port_accepts(&mut self, port: u16, timeout: Duration) -> Result<bool, SeamError>;
    /// Starts the tunnel process; stdout and stderr are one stream.
    fn spawn(&mut self, command: &[String]) -> Result<Box<dyn TunnelProcess>, SeamError>;
    /// Recorded facts a run never used (always 0 for a live host).
    fn unconsumed(&self) -> usize;
}

/// What the supervising loop sees of the tunnel process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessEvent {
    /// One line of the process output.
    Line(String),
    /// No output for the health-check interval.
    Idle,
    /// The output stream closed.
    Eof,
    /// The user interrupted the run (Ctrl+C).
    Interrupted,
}

/// A running tunnel process.
pub(crate) trait TunnelProcess {
    /// Waits `settle`, then returns the exit code if the process has exited.
    fn exited_after(&mut self, settle: Duration) -> Result<Option<i32>, SeamError>;
    /// The rest of the output of an exited process.
    fn drain_output(&mut self) -> Result<String, SeamError>;
    /// The next event, waiting at most `idle` for output.
    fn next_event(&mut self, idle: Duration) -> Result<ProcessEvent, SeamError>;
    /// Asks the process to stop (`SIGTERM`).
    fn terminate(&mut self);
    /// Waits `timeout` for the exit; `None` when it did not exit.
    fn wait(&mut self, timeout: Duration) -> Result<Option<i32>, SeamError>;
    /// Stops the process for good (`SIGKILL`).
    fn kill(&mut self);
    /// Waits for the exit with no bound and returns its code.
    fn wait_exit(&mut self) -> Result<i32, SeamError>;
}

/// The host seam of a run without a recording.
pub(crate) struct UnavailableHost;

impl TunnelHost for UnavailableHost {
    fn program_on_path(&mut self, _program: &str) -> Result<bool, SeamError> {
        Err(SeamError::Unavailable(Seam::Program))
    }

    fn local_port_free(&mut self, _port: u16) -> Result<bool, SeamError> {
        Err(SeamError::Unavailable(Seam::LocalPort))
    }

    fn port_accepts(&mut self, _port: u16, _timeout: Duration) -> Result<bool, SeamError> {
        Err(SeamError::Unavailable(Seam::PortConnect))
    }

    fn spawn(&mut self, _command: &[String]) -> Result<Box<dyn TunnelProcess>, SeamError> {
        Err(SeamError::Unavailable(Seam::TunnelProcess))
    }

    fn unconsumed(&self) -> usize {
        0
    }
}

/// What a lookup of the cache host found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// The host resolved to this address.
    Address(std::net::IpAddr),
    /// The lookup failed (`socket.gaierror`).
    NotFound,
}

/// What the cache client is asked to connect with. The password is a
/// [`toolkit_aws::Secret`]: it has no `Display` and `Debug` hides it.
#[derive(Debug, Clone)]
pub(crate) struct ConnectPlan {
    pub host: String,
    pub port: u16,
    pub timeout_secs: f64,
    pub tls: bool,
    /// Tunnel mode: the certificate is verified against the CA bundle but the
    /// host name is not.
    pub check_hostname: bool,
    pub server_name: Option<String>,
    /// A cluster-mode client (serverless endpoint, not through a tunnel).
    pub cluster_client: bool,
    pub username: Option<String>,
    pub password: Option<toolkit_aws::Secret>,
}

/// Why a cache client call failed: a closed set. The client's own error text
/// is never carried, printed or recorded (PM ruling for #5004, #5005, #5007);
/// each kind prints [`CacheErrorKind::text`] and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheErrorKind {
    /// The server refused the credentials (`AuthenticationError`).
    AuthenticationFailed,
    /// The connection was refused or reset.
    ConnectionRefused,
    /// The TLS handshake failed.
    TlsHandshakeFailed,
    /// The call timed out.
    Timeout,
    /// The host name did not resolve.
    DnsFailed,
    /// Anything else.
    Other,
}

impl CacheErrorKind {
    /// Every kind, in the order the recording format lists them.
    pub(crate) const ALL: [Self; 6] = [
        Self::AuthenticationFailed,
        Self::ConnectionRefused,
        Self::TlsHandshakeFailed,
        Self::Timeout,
        Self::DnsFailed,
        Self::Other,
    ];

    /// The name of the kind in a recording.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::ConnectionRefused => "connection_refused",
            Self::TlsHandshakeFailed => "tls_handshake_failed",
            Self::Timeout => "timeout",
            Self::DnsFailed => "dns_failed",
            Self::Other => "other",
        }
    }

    /// The kind named `key` in a recording.
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// The fixed, first-party text printed for the kind.
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication failed",
            Self::ConnectionRefused => "connection refused",
            Self::TlsHandshakeFailed => "TLS handshake failed",
            Self::Timeout => "timed out",
            Self::DnsFailed => "host name lookup failed",
            Self::Other => "client error (details withheld)",
        }
    }
}

/// The `INFO server` fields the script prints. The text is the server's own:
/// it is printed only through [`super::clean::SecretGate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerInfo {
    pub version: String,
    pub mode: String,
}

/// An open cache connection.
pub(crate) trait CacheConnection {
    /// `PING`; the round trip in milliseconds.
    fn ping(&mut self) -> Result<f64, CacheErrorKind>;
    /// `INFO server`; `None` when the ACL refuses it.
    fn info_server(&mut self) -> Option<ServerInfo>;
    /// Closes the connection.
    fn close(&mut self);
}

/// The IAM token request (`get_iam_auth_token`). The cache name is already
/// lower-cased, as the script does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IamTokenRequest {
    pub cache_name: String,
    pub iam_user: String,
    pub region: String,
    pub serverless: bool,
}

/// DNS, the cache client and the IAM signer.
pub(crate) trait CacheEnvironment {
    /// `socket.gethostbyname`.
    fn resolve(&mut self, host: &str) -> Result<Resolution, SeamError>;
    /// A short-lived IAM auth token (15 minutes).
    fn iam_token(&mut self, request: &IamTokenRequest) -> Result<toolkit_aws::Secret, SeamError>;
    /// Builds the client. `Err(kind)` is the script's `Client init FAILED`.
    fn connect(
        &mut self,
        plan: &ConnectPlan,
    ) -> Result<Result<Box<dyn CacheConnection>, CacheErrorKind>, SeamError>;
    /// The client name printed in the connecting line.
    fn client_name(&self) -> &'static str;
    /// Recorded facts a run never used.
    fn unconsumed(&self) -> usize;
}

/// The cache environment of a run without a recording.
pub(crate) struct UnavailableCache;

impl CacheEnvironment for UnavailableCache {
    fn resolve(&mut self, _host: &str) -> Result<Resolution, SeamError> {
        Err(SeamError::Unavailable(Seam::Dns))
    }

    fn iam_token(&mut self, _request: &IamTokenRequest) -> Result<toolkit_aws::Secret, SeamError> {
        Err(SeamError::Unavailable(Seam::IamSigner))
    }

    fn connect(
        &mut self,
        _plan: &ConnectPlan,
    ) -> Result<Result<Box<dyn CacheConnection>, CacheErrorKind>, SeamError> {
        Err(SeamError::Unavailable(Seam::CacheClient))
    }

    fn client_name(&self) -> &'static str {
        "valkey"
    }

    fn unconsumed(&self) -> usize {
        0
    }
}
