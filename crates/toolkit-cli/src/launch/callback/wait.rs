//! Parent side of the pending-record handshake.
use crate::{Invocation, Output};
use toolkit_core::ToolkitError;

/// Wait without signalling a PID: only lock custody can establish finality.
#[allow(clippy::unnecessary_wraps)]
pub fn command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    #[cfg(unix)]
    {
        use super::{
            pending::{Observation, WaitTracker},
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
        let mut tracker = WaitTracker::default();
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|v| i64::try_from(v.as_secs()).ok())
                .unwrap_or(0);
            let result = tracker.finish_after_report(&dir, pid, now, |observation, reason| match observation {
                Observation::Missing => report_failure(output, &format!("No sign-in is in progress for pid {pid}. Run auth-start again.")),
                Observation::Decision(WaitDecision::Authenticated) => {
                    let result = output.line(&serde_json::json!({"authenticated":true,"baseUrl":base}).to_string());
                    let delivered = result.is_ok() && !output.is_closed();
                    (Some(result), delivered)
                },
                Observation::Decision(WaitDecision::Poll) => (None, false),
                Observation::Decision(WaitDecision::HelperBlocked) => report_failure(output, "Sign-in has not finished: the sign-in helper is still blocked. Run session-status in a minute; if it does not report a session, run auth-start again."),
                Observation::Decision(WaitDecision::TimedOut) => report_failure(output, "Sign-in did not finish in time. Run auth-start again."),
                Observation::Decision(WaitDecision::Ended) => report_failure(output, "Sign-in ended without completing authentication. Run auth-start again."),
                Observation::Decision(WaitDecision::Failed) => report_failure(output, match reason.as_deref() {
                    Some("authorization_denied") => "Sign-in was denied. Run auth-start again.",
                    Some("invalid_code") => "Sign-in returned an invalid authorization code. Run auth-start again.",
                    Some("transport_unavailable") => "The sign-in service transport is unavailable.",
                    _ => "Sign-in failed. Run auth-start again.",
                }),
            });
            if let Some(result) = result {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = invocation;
        failure(output, "This command is not supported on this platform.")
    }
}

#[cfg(unix)]
pub(super) fn report_failure(
    output: &mut Output<'_>,
    message: &str,
) -> (Option<Result<(), ToolkitError>>, bool) {
    let text = serde_json::json!({"error":message}).to_string();
    let text = output.caller.diagnostic(&text);
    let delivered = output.diagnostics.as_mut().is_some_and(|writer| {
        writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .is_ok()
    });
    output.set_exit_status(1);
    (Some(Ok(())), delivered)
}

#[allow(clippy::unnecessary_wraps)]
pub(super) fn failure(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    output.diagnostic(&serde_json::json!({"error":message}).to_string());
    output.set_exit_status(1);
    Ok(())
}
