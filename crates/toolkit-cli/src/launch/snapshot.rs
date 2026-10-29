//! The `launch download-snapshot` and `launch apply-snapshot` routes (L6,
//! R-AR-EXT and R-AR-APPLY; the cutover of `SKILL.md` step 8A and step C is
//! L8).
//!
//! `download-snapshot` needs one Launch API call, `GetLaunch
//! include=download_url`. Until L5's API client lands, [`get_download_url`]
//! is that one operation over the bounded runner of R-API-BODY: the
//! percent-encoded id path, the repeated `include` query member, a bearer
//! token, the 64 MiB JSON cap, 120 s attempts within a 360 s deadline. Its
//! failures name only the HTTP status. The default build has no network
//! transport and refuses with the Q20 message before any request.

use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

use serde_json::Value;
use toolkit_aws::retry::{Jitter, RetryPolicy, Sleeper};
use toolkit_aws::stream::StreamingTransport;
use toolkit_aws::transport::{percent_encode, Headers, HttpRequest, Method, Timeouts};
use toolkit_aws::Secret;
use toolkit_core::ToolkitError;

use super::http::{self, MonotonicClock, Operation};
use super::LaunchError;
use crate::{Invocation, Output};

/// The fixed refusal of a build without the `aws-sdk` feature (Q20).
pub const NO_NETWORK_MESSAGE: &str =
    "This toolkit build has no network support; install a build with the aws-sdk feature.";
/// The Launch API operation deadline (R-API-BODY).
pub const API_SECONDS: u64 = 360;
/// The per-attempt limit.
pub const ATTEMPT_SECONDS: u64 = 120;
/// Longest launch id (botocore `launchIdentifier`).
pub const MAX_LAUNCH_ID: usize = 2048;

/// The local launch id check, before any request.
///
/// # Errors
/// A fixed message for an empty or overlong id.
pub fn validate_launch_id(launch_id: &str) -> Result<(), LaunchError> {
    let length = launch_id.chars().count();
    if length == 0 || length > MAX_LAUNCH_ID {
        return Err(LaunchError::Config(format!(
            "Invalid launch id: it must be 1 to {MAX_LAUNCH_ID} characters."
        )));
    }
    Ok(())
}

/// The retry seams of [`get_download_url`].
pub struct Runner<'a> {
    /// Monotonic time.
    pub clock: &'a dyn MonotonicClock,
    /// Backoff sleeps.
    pub sleeper: &'a dyn Sleeper,
    /// Backoff jitter.
    pub jitter: &'a dyn Jitter,
    /// The shared retry policy.
    pub policy: RetryPolicy,
}

/// `GET <base>/api/launches/<id>?include=download_url`: the launch's
/// `downloadUrl`, or `None` when it has none yet.
///
/// # Errors
/// [`LaunchError::Download`] naming only the status or the failure class.
pub fn get_download_url(
    transport: &dyn StreamingTransport,
    base: &str,
    launch_id: &str,
    token: &Secret,
    runner: &Runner<'_>,
) -> Result<Option<String>, LaunchError> {
    let failed = |text: &str| LaunchError::Download(format!("The launch lookup failed: {text}."));
    let mut headers = Headers::new();
    headers
        .insert("authorization", &format!("Bearer {}", token.expose()))
        .map_err(|_| failed("the session token is not usable"))?;
    headers
        .insert("accept", "application/json")
        .map_err(|_| failed("invalid request"))?;
    let timeout = std::time::Duration::from_secs(ATTEMPT_SECONDS);
    let request = HttpRequest {
        method: Method::Get,
        url: format!(
            "{}/api/launches/{}?include=download_url",
            base.trim_end_matches('/'),
            percent_encode(launch_id)
        ),
        headers,
        body: Vec::new(),
        timeouts: Timeouts::new(timeout, timeout).map_err(|_| failed("invalid request"))?,
    };
    let deadline = runner.clock.now() + std::time::Duration::from_secs(API_SECONDS);
    let response = http::run(
        transport,
        &request,
        Operation::ApiJson,
        deadline,
        timeout,
        false,
        runner.clock,
        runner.sleeper,
        runner.jitter,
        &runner.policy,
    )
    .map_err(|error| {
        failed(match error {
            http::BodyError::Deadline => "the request timed out",
            http::BodyError::TooLarge => "the response is too large",
            http::BodyError::InvalidResponse | http::BodyError::Allocation => {
                "the response is invalid"
            }
            http::BodyError::Transport { .. } => "the service could not be reached",
        })
    })?;
    if !(200..300).contains(&response.status) {
        return Err(LaunchError::Download(format!(
            "The launch lookup failed with HTTP status {}.",
            response.status
        )));
    }
    let value: Value =
        serde_json::from_slice(&response.body).map_err(|_| failed("the response is invalid"))?;
    let Some(object) = value.as_object() else {
        return Err(failed("the response is invalid"));
    };
    match object.get("downloadUrl") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(url)) if url.is_empty() => Ok(None),
        Some(Value::String(url)) => Ok(Some(url.clone())),
        Some(_) => Err(failed("the response is invalid")),
    }
}

/// `{"applied": true, "files": N, "directories": M, "into": "<path>"}`.
#[must_use]
pub fn render_applied(files: usize, directories: usize, into: &Path) -> String {
    format!(
        "{{\"applied\":true,\"files\":{files},\"directories\":{directories},\"into\":{}}}",
        Value::String(into.to_string_lossy().into_owned())
    )
}

#[allow(clippy::unnecessary_wraps)]
fn failure(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    output.diagnostic(&serde_json::json!({ "error": message }).to_string());
    output.set_exit_status(1);
    Ok(())
}

/// `toolkit launch apply-snapshot <snapshot-dir> [--into DIR]`.
#[allow(clippy::unnecessary_wraps)]
pub fn apply_command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let Some(snapshot) = invocation.positional(0) else {
        return failure(output, "Missing required argument for apply-snapshot");
    };
    #[cfg(unix)]
    {
        use super::archive::apply::{
            apply_snapshot, ApplyLimits, ApplyOptions, MetaRules, NoApplyHook,
        };
        use super::archive::git::GitRunner;
        use super::clock::OsRandom;
        let into = Path::new(invocation.value("--into").unwrap_or("."));
        let git = GitRunner::system();
        let mut hook = NoApplyHook;
        let mut options = ApplyOptions {
            git: &git,
            env: &toolkit_aws::ProcessEnv,
            limits: ApplyLimits::PRODUCTION,
            rules: MetaRules::PRODUCTION,
            hook: &mut hook,
            random: &OsRandom,
        };
        match apply_snapshot(Path::new(snapshot), into, &mut options) {
            Ok(applied) => output.line(&render_applied(
                applied.files,
                applied.directories,
                &applied.into,
            )),
            Err(error) => failure(output, error.message()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = snapshot;
        failure(output, super::PLATFORM_MESSAGE)
    }
}

/// `toolkit launch download-snapshot <launch-id> [--output DIR] [--region R]`.
#[allow(clippy::unnecessary_wraps)]
pub fn download_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(launch_id) = invocation.positional(0) else {
        return failure(output, "Missing required argument for download-snapshot");
    };
    #[cfg(unix)]
    {
        match download(invocation, launch_id) {
            Ok(text) => output.line(&text),
            Err(error) => failure(output, error.message()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (invocation, launch_id);
        failure(output, super::PLATFORM_MESSAGE)
    }
}

#[cfg(unix)]
fn download(invocation: &Invocation, launch_id: &str) -> Result<String, LaunchError> {
    use super::archive::extract::NoExtractHook;
    use super::archive::plan::MAX_SNAPSHOT_NODES;
    use super::archive::ArchiveLimits;
    use super::clock::OsRandom;
    use super::config::{self, RegionInputs};
    use super::download::{
        download_snapshot, render_downloaded, DownloadDeps, DownloadRequest, DOWNLOAD_SECONDS,
    };
    use super::http::DOWNLOAD_CAP;
    use toolkit_aws::{Env, ProcessEnv, SystemResolver};
    validate_launch_id(launch_id)?;
    let env = ProcessEnv;
    let dir = super::state::state_dir(&env)?;
    let home = env.var("HOME").map(std::path::PathBuf::from);
    let region = config::resolve_region(&RegionInputs {
        explicit: invocation.value("--region"),
        aws_mcp_url: None,
        env: &env,
        state_dir: Some(&dir),
        home: home.as_deref(),
    })?;
    let base = match env.var(config::ENV_BASE_URL).filter(|s| !s.is_empty()) {
        Some(value) => config::validate_base_url(&value, &SystemResolver)?,
        None => config::base_url_for_region(region.region)?,
    };
    let output_path = invocation.value("--output").map(Path::new);
    let network = Network::open()?;
    let lookup = || network.download_url(&dir, &base, launch_id);
    let temp = std::env::temp_dir();
    let mut hook = NoExtractHook;
    let mut deps = DownloadDeps {
        download_url: &lookup,
        transport: network.transport(),
        resolver: &SystemResolver,
        random: &OsRandom,
        hook: &mut hook,
        temp_dir: &temp,
        limits: ArchiveLimits::PRODUCTION,
        node_budget: MAX_SNAPSHOT_NODES,
        download_cap: DOWNLOAD_CAP,
        download_time: Duration::from_secs(DOWNLOAD_SECONDS),
    };
    let downloaded = download_snapshot(
        &DownloadRequest {
            launch_id,
            output: output_path,
            region: region.region,
        },
        &mut deps,
    )?;
    Ok(render_downloaded(&downloaded))
}

/// The production network: the SDK transport with the `aws-sdk` feature,
/// otherwise a transport that refuses every request with the Q20 message.
#[cfg(unix)]
struct Network {
    #[cfg(feature = "aws-sdk")]
    transport: toolkit_aws::sdk::SdkTransport,
    #[cfg(feature = "aws-sdk")]
    _runtime: toolkit_aws::sdk::SdkRuntime,
    #[cfg(not(feature = "aws-sdk"))]
    transport: Unavailable,
}

/// The default build's transport.
#[cfg(not(feature = "aws-sdk"))]
struct Unavailable;

#[cfg(not(feature = "aws-sdk"))]
impl StreamingTransport for Unavailable {
    fn open<'a>(
        &'a self,
        _request: &HttpRequest,
        _limits: &toolkit_aws::stream::StreamLimits,
    ) -> Result<Box<dyn toolkit_aws::stream::ResponseStream + 'a>, toolkit_aws::AwsError> {
        Err(toolkit_aws::AwsError::connect(NO_NETWORK_MESSAGE))
    }
}

#[cfg(unix)]
impl Network {
    #[cfg(feature = "aws-sdk")]
    fn open() -> Result<Self, LaunchError> {
        use toolkit_aws::sdk::{roots::ValidatedRoots, SdkRuntime, SdkTransport};
        let unavailable =
            || LaunchError::Download("The network transport is unavailable.".to_string());
        let roots = ValidatedRoots::from_process().map_err(|_| unavailable())?;
        let runtime = SdkRuntime::new().map_err(|_| unavailable())?;
        let transport = SdkTransport::new(&runtime, &roots).map_err(|_| unavailable())?;
        Ok(Self {
            transport,
            _runtime: runtime,
        })
    }

    #[cfg(not(feature = "aws-sdk"))]
    #[allow(clippy::unnecessary_wraps)]
    fn open() -> Result<Self, LaunchError> {
        Ok(Self {
            transport: Unavailable,
        })
    }

    fn transport(&self) -> &dyn StreamingTransport {
        &self.transport
    }

    #[cfg(not(feature = "aws-sdk"))]
    #[allow(clippy::unused_self)]
    fn download_url(
        &self,
        _dir: &Path,
        _base: &str,
        _launch_id: &str,
    ) -> Result<Option<String>, LaunchError> {
        Err(LaunchError::Download(NO_NETWORK_MESSAGE.to_string()))
    }

    #[cfg(feature = "aws-sdk")]
    fn download_url(
        &self,
        dir: &Path,
        base: &str,
        launch_id: &str,
    ) -> Result<Option<String>, LaunchError> {
        use super::clock::OsRandom;
        use super::http::SystemClock;
        use super::oidc::Oidc;
        use toolkit_aws::retry::{FullJitter, ThreadSleeper};
        let jitter = FullJitter::new();
        let oidc = Oidc {
            transport: &self.transport,
            clock: &SystemClock,
            sleeper: &ThreadSleeper,
            jitter: &jitter,
            policy: RetryPolicy::default(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
            .ok_or_else(|| LaunchError::Config("The system clock is unavailable.".to_string()))?;
        let token = oidc
            .access_token(
                dir,
                &OsRandom,
                now,
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .map_err(|error| LaunchError::Download(error.to_string()))?;
        get_download_url(
            &self.transport,
            base,
            launch_id,
            &token,
            &Runner {
                clock: &SystemClock,
                sleeper: &ThreadSleeper,
                jitter: &jitter,
                policy: RetryPolicy::default(),
            },
        )
    }
}

#[cfg(test)]
mod tests_snapshot_cmd;
