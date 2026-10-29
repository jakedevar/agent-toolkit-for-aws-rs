//! The launch command routes of L5: the offline commands (`resolve-region`,
//! `session-status`, `sign-out`) and the entry points of the network commands
//! in [`super::flow`]. Success prints Python `json.dumps(value, indent=2)`,
//! failure `{"error": message}` on stderr with exit 1 (section 1.3).
use super::{
    config::{self, RegionInputs},
    flow, state,
    text::{fail, ok_text},
};
use crate::{Invocation, Output};
use toolkit_aws::{Env, ProcessEnv, SystemResolver};
use toolkit_core::{Json, ToolkitError};

fn emit(output: &mut Output<'_>, value: &Json) -> Result<(), ToolkitError> {
    output.line(&ok_text(value))
}

#[allow(clippy::unnecessary_wraps)]
fn failure(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    fail(output, message);
    Ok(())
}

pub(crate) fn resolve_region(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let env = ProcessEnv;
    let dir = match state::state_dir(&env) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    let home = env.var("HOME").map(std::path::PathBuf::from);
    let region = match config::resolve_region(&RegionInputs {
        explicit: invocation.value("--region"),
        aws_mcp_url: invocation.value("--aws-mcp-url"),
        env: &env,
        state_dir: Some(&dir),
        home: home.as_deref(),
    }) {
        Ok(value) => value,
        Err(error) => return failure(output, error.message()),
    };
    let base = match env.var(config::ENV_BASE_URL).filter(|s| !s.is_empty()) {
        Some(value) => config::validate_base_url(&value, &SystemResolver),
        None => config::base_url_for_region(region.region),
    };
    let base = match base {
        Ok(value) => value,
        Err(error) => return failure(output, error.message()),
    };
    let other: Vec<Json> = config::SUPPORTED_REGIONS
        .iter()
        .filter(|(name, _)| *name != region.region)
        .map(|(name, display)| {
            Json::object(vec![
                ("region", Json::string(*name)),
                ("regionName", Json::string(*display)),
            ])
        })
        .collect();
    emit(
        output,
        &Json::object(vec![
            ("region", Json::string(region.region)),
            ("regionName", Json::string(region.region_name())),
            ("source", Json::string(region.source.as_str())),
            ("sourceLabel", Json::string(region.source.label())),
            ("baseUrl", Json::string(base)),
            ("otherRegions", Json::Array(other)),
        ]),
    )
}

/// `session-status` at wall-clock second `now` (Python `session_status`).
fn session_value(session: Option<&state::StoredSession>, now: i64) -> Json {
    let Some(session) = session else {
        return Json::object(vec![("authenticated", Json::Bool(false))]);
    };
    Json::object(vec![
        (
            "authenticated",
            Json::Bool(session.token_expires_at > now.saturating_add(300)),
        ),
        (
            "tokenExpiresInSecs",
            Json::Int(session.token_expires_at.saturating_sub(now).max(0)),
        ),
        (
            "sessionExpiresInSecs",
            Json::Int(session.client_expires_at.saturating_sub(now).max(0)),
        ),
        ("canRefresh", Json::Bool(session.client_expires_at > now)),
    ])
}

pub(crate) fn session_status(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let dir = match state::state_dir(&ProcessEnv) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    let session = state::load_session(&dir);
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return failure(output, "The system clock is unavailable.");
    };
    let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
    emit(output, &session_value(session.as_ref(), now))
}

pub(crate) fn sign_out(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let dir = match state::state_dir(&ProcessEnv) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    match state::remove_state_file(&dir, state::SESSION_FILE_NAME) {
        Ok(removed) => emit(
            output,
            &Json::object(vec![
                ("signedOut", Json::Bool(true)),
                ("sessionRemoved", Json::Bool(removed)),
            ]),
        ),
        Err(_) => failure(output, "Could not remove the session file."),
    }
}

fn request<'a>(invocation: &'a Invocation, command: &'a str) -> flow::Request<'a> {
    flow::Request {
        command,
        first: invocation.positional(0),
        second: invocation.positional(1),
        region: invocation.value("--region"),
        aws_mcp_url: invocation.value("--aws-mcp-url"),
    }
}

/// Print the outcome of a network command.
pub(crate) fn report(
    output: &mut Output<'_>,
    result: Result<Json, String>,
) -> Result<(), ToolkitError> {
    match result {
        Ok(value) => emit(output, &value),
        Err(message) => failure(output, &message),
    }
}

/// Dispatch a Launch API command: the local checks always run first, so a
/// build without network support still refuses bad arguments with the same
/// messages.
fn network_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    command: &str,
) -> Result<(), ToolkitError> {
    let request = request(invocation, command);
    if let Err(message) = flow::validate_local(&request) {
        return failure(output, &message);
    }
    #[cfg(feature = "aws-sdk")]
    {
        report(output, run_network(&request))
    }
    #[cfg(not(feature = "aws-sdk"))]
    {
        failure(output, super::snapshot::NO_NETWORK_MESSAGE)
    }
}

pub(crate) fn create_launch(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "create-launch")
}
pub(crate) fn get_launch(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "get-launch")
}
pub(crate) fn list_launches(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "list-launches")
}
pub(crate) fn delete_launch(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "delete-launch")
}
pub(crate) fn start_execution(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "start-launch-execution")
}
pub(crate) fn get_status(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "get-launch-status")
}
pub(crate) fn get_download_url(i: &Invocation, o: &mut Output<'_>) -> Result<(), ToolkitError> {
    network_command(i, o, "get-launch-download-url")
}

/// The production dependencies of [`flow::run`].
#[cfg(feature = "aws-sdk")]
fn run_network(request: &flow::Request<'_>) -> Result<Json, String> {
    use super::clock::OsRandom;
    use super::http::SystemClock;
    use toolkit_aws::retry::{FullJitter, RetryPolicy, ThreadSleeper};
    let env = ProcessEnv;
    let dir = state::state_dir(&env).map_err(|e| e.to_string())?;
    let home = env.var("HOME").map(std::path::PathBuf::from);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or("The system clock is unavailable.")?;
    let network = super::snapshot::Network::open().map_err(|e| e.to_string())?;
    let jitter = FullJitter::new();
    let archive = |source: &str, name: &str| -> Result<Vec<u8>, String> {
        #[cfg(unix)]
        {
            use super::archive::{
                git::GitRunner,
                select::{build_archive, BuildOptions, NoHook},
                skipped::SkippedBudget,
                ArchiveLimits,
            };
            let cwd = std::env::current_dir()
                .map_err(|_| "Could not determine the current directory.".to_owned())?;
            let temp = std::env::temp_dir();
            let git = GitRunner::system();
            build_archive(
                source,
                name,
                &BuildOptions {
                    git: &git,
                    hook: &NoHook,
                    limits: ArchiveLimits::PRODUCTION,
                    skipped: SkippedBudget::PRODUCTION,
                    home: home.as_deref(),
                    cwd: &cwd,
                    temp_parent: &temp,
                    random: &OsRandom,
                },
            )
            .map(|built| built.bytes)
            .map_err(|e| e.to_string())
        }
        #[cfg(not(unix))]
        {
            let _ = (source, name);
            Err(super::PLATFORM_MESSAGE.to_owned())
        }
    };
    flow::run(
        request,
        &flow::Deps {
            env: &env,
            state_dir: &dir,
            home: home.as_deref(),
            transport: network.transport(),
            resolver: &SystemResolver,
            random: &OsRandom,
            clock: &SystemClock,
            sleeper: &ThreadSleeper,
            jitter: &jitter,
            policy: RetryPolicy::default(),
            now,
            archive: &archive,
        },
    )
}

#[cfg(test)]
#[path = "commands/tests_offline.rs"]
mod tests_offline;
