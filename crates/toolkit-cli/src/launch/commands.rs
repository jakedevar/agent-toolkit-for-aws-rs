//! Offline Launch commands.
use super::{
    config::{self, RegionInputs},
    state,
};
use crate::{Invocation, Output};
use toolkit_aws::{Env, ProcessEnv, SystemResolver};
use toolkit_core::ToolkitError;

fn emit(output: &mut Output<'_>, value: &serde_json::Value) -> Result<(), ToolkitError> {
    output.line(&serde_json::to_string_pretty(value).expect("JSON value serializes"))
}
#[allow(clippy::unnecessary_wraps)]
fn failure(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    output.diagnostic(&serde_json::json!({"error": message}).to_string());
    output.set_exit_status(1);
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
    let other: Vec<_> = config::SUPPORTED_REGIONS
        .iter()
        .filter(|(name, _)| *name != region.region)
        .map(|(name, display)| serde_json::json!({"region":name,"regionName":display}))
        .collect();
    emit(
        output,
        &serde_json::json!({"region":region.region,"regionName":region.region_name(),"source":region.source.as_str(),"sourceLabel":region.source.label(),"baseUrl":base,"otherRegions":other}),
    )
}
pub(crate) fn session_status(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let dir = match state::state_dir(&ProcessEnv) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    let Some(session) = state::load_session(&dir) else {
        return emit(output, &serde_json::json!({"authenticated":false}));
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return failure(output, "The system clock is unavailable.");
    };
    let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
    emit(
        output,
        &serde_json::json!({"authenticated":session.token_expires_at > now.saturating_add(300),"tokenExpiresInSecs":session.token_expires_at.saturating_sub(now).max(0),"sessionExpiresInSecs":session.client_expires_at.saturating_sub(now).max(0),"canRefresh":session.client_expires_at > now}),
    )
}
pub(crate) fn sign_out(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let dir = match state::state_dir(&ProcessEnv) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    match state::remove_state_file(&dir, state::SESSION_FILE_NAME) {
        Ok(removed) => emit(
            output,
            &serde_json::json!({"signedOut":true,"sessionRemoved":removed}),
        ),
        Err(_) => failure(output, "Could not remove the session file."),
    }
}
