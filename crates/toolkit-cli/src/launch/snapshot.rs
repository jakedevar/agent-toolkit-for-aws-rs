//! The `launch download-snapshot` and `launch apply-snapshot` routes (L6,
//! R-AR-EXT and R-AR-APPLY; the cutover of `SKILL.md` step 8A and step C is
//! L8).
//!

use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

use serde_json::Value;
#[cfg(all(unix, feature = "aws-sdk"))]
use toolkit_aws::retry::RetryPolicy;
use toolkit_aws::stream::StreamingTransport;
use toolkit_aws::transport::HttpRequest;
use toolkit_core::ToolkitError;

use super::LaunchError;
use crate::{Invocation, Output};

/// The fixed refusal of a build without the `aws-sdk` feature (Q20).
pub const NO_NETWORK_MESSAGE: &str =
    "This toolkit build has no network support; install a build with the aws-sdk feature.";
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
    super::api::validate_launch_id(launch_id)
        .map_err(|error| LaunchError::Config(error.to_string()))?;
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
    let lookup = || {
        network.download_url(
            &dir,
            &base,
            launch_id,
            region.region,
            env.var(config::ENV_BASE_URL).is_some_and(|s| !s.is_empty()),
        )
    };
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
        _region: &str,
        _override_base: bool,
    ) -> Result<Option<String>, LaunchError> {
        Err(LaunchError::Download(NO_NETWORK_MESSAGE.to_string()))
    }

    #[cfg(feature = "aws-sdk")]
    fn download_url(
        &self,
        dir: &Path,
        base: &str,
        launch_id: &str,
        region: &str,
        override_base: bool,
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
        super::api::Client {
            transport: &self.transport,
            base,
            region,
            token: &token,
            clock: &SystemClock,
            sleeper: &ThreadSleeper,
            jitter: &jitter,
            policy: RetryPolicy::default(),
        }
        .download_url(launch_id, override_base)
        .map_err(|error| LaunchError::Download(error.to_string()))
    }
}

#[cfg(test)]
mod tests_snapshot_cmd;
