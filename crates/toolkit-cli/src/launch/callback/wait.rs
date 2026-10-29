//! Parent side of the pending-record handshake.
use crate::{Invocation, Output};
use toolkit_core::ToolkitError;

/// Wait without signalling a PID: only lock custody can establish finality.
#[allow(clippy::unnecessary_wraps)]
pub fn command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    #[cfg(unix)]
    {
        use super::{
            pending::{finish_if_final, Observation},
            WaitDecision,
        };
        use crate::launch::{
            config::{self, RegionInputs},
            state,
        };
        use toolkit_aws::{Env, ProcessEnv, SystemResolver};
        let Some(pid) = invocation
            .positional(0)
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|pid| *pid != 0)
        else {
            return failure(output, "Missing required argument for auth-wait");
        };
        let env = ProcessEnv;
        let Ok(dir) = state::state_dir(&env) else {
            return failure(output, "Could not locate sign-in state.");
        };
        let home = env.var("HOME").map(std::path::PathBuf::from);
        let inputs = RegionInputs {
            explicit: invocation.value("--region"),
            aws_mcp_url: invocation.value("--aws-mcp-url"),
            env: &env,
            state_dir: Some(&dir),
            home: home.as_deref(),
        };
        let region = match config::resolve_region(&inputs) {
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
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|v| i64::try_from(v.as_secs()).ok())
                .unwrap_or(0);
            let (observation, reason) = finish_if_final(&dir, pid, now);
            match observation {
                Observation::Missing => return failure(output, &format!("No sign-in is in progress for pid {pid}. Run auth-start again.")),
                Observation::Decision(WaitDecision::Authenticated) => return output.line(&serde_json::json!({"authenticated":true,"baseUrl":base}).to_string()),
                Observation::Decision(WaitDecision::Poll) => std::thread::sleep(std::time::Duration::from_secs(1)),
                Observation::Decision(WaitDecision::HelperBlocked) => return failure(output, "Sign-in has not finished: the sign-in helper is still blocked. Run session-status in a minute; if it does not report a session, run auth-start again."),
                Observation::Decision(WaitDecision::TimedOut) => return failure(output, "Sign-in did not finish in time. Run auth-start again."),
                Observation::Decision(WaitDecision::Ended) => return failure(output, "Sign-in ended without completing authentication. Run auth-start again."),
                Observation::Decision(WaitDecision::Failed) => return failure(output, match reason.as_deref() {
                    Some("authorization_denied") => "Sign-in was denied. Run auth-start again.",
                    Some("invalid_code") => "Sign-in returned an invalid authorization code. Run auth-start again.",
                    Some("transport_unavailable") => "The sign-in service transport is unavailable.",
                    _ => "Sign-in failed. Run auth-start again.",
                }),
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = invocation;
        failure(output, "This command is not supported on this platform.")
    }
}

#[allow(clippy::unnecessary_wraps)]
pub(super) fn failure(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    output.diagnostic(&serde_json::json!({"error":message}).to_string());
    output.set_exit_status(1);
    Ok(())
}
