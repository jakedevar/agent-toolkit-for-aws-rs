//! Internal callback command, launched by auth-start with configuration on stdin.
use super::{
    lifecycle::{self, HelperResult},
    spawn::HelperConfig,
};
use crate::{Invocation, Output};
use std::io::{self, IsTerminal};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use toolkit_core::ToolkitError;

/// Internal route. Failures are intentionally silent, including invalid stdin.
pub fn callback(_invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    if io::stdin().is_terminal() {
        output.set_exit_status(1);
        return Ok(());
    }
    let config = match HelperConfig::read(io::stdin().lock()) {
        Ok(config) => config,
        Err(_) => {
            output.set_exit_status(1);
            return Ok(());
        }
    };
    let dir = match crate::launch::state::state_dir(&toolkit_aws::ProcessEnv) {
        Ok(dir) => dir,
        Err(_) => {
            output.set_exit_status(1);
            return Ok(());
        }
    };
    #[cfg(unix)]
    let result = lifecycle::run(
        &dir,
        config.state.expose(),
        config.deadline,
        |port| {
            output
                .line(&port.to_string())
                .and_then(|()| output.finish())
                .map_err(|_| io::Error::other("port announcement failed"))
        },
        |code, redirect, cutoff| exchange(&config, code, redirect, cutoff),
    );
    #[cfg(not(unix))]
    let result: io::Result<HelperResult> = Err(io::Error::other("unsupported platform"));
    if !matches!(result, Ok(HelperResult::Succeeded)) {
        output.set_exit_status(1);
    }
    Ok(())
}

#[cfg(not(feature = "aws-sdk"))]
fn exchange(
    _config: &HelperConfig,
    _code: &str,
    _redirect: &str,
    _cutoff: Instant,
) -> Result<Vec<u8>, &'static str> {
    Err("transport_unavailable")
}

#[cfg(feature = "aws-sdk")]
fn exchange(
    config: &HelperConfig,
    code: &str,
    redirect: &str,
    cutoff: Instant,
) -> Result<Vec<u8>, &'static str> {
    use crate::launch::{
        http::SystemClock,
        oidc::{Client, Oidc},
        state::StoredSession,
    };
    use toolkit_aws::{
        retry::{FullJitter, RetryPolicy, ThreadSleeper},
        sdk::{roots::ValidatedRoots, SdkRuntime, SdkTransport},
        Secret,
    };
    let roots = ValidatedRoots::from_process().map_err(|_| "transport_unavailable")?;
    let runtime = SdkRuntime::new().map_err(|_| "transport_unavailable")?;
    let transport = SdkTransport::new(&runtime, &roots).map_err(|_| "transport_unavailable")?;
    let client = Client {
        id: config.client_id.clone(),
        secret: config.client_secret.clone(),
        expires_at: config.client_expires_at,
        scopes: config.scopes.clone(),
    };
    let now = unix_time().ok_or("exchange_failed")?;
    let oidc = Oidc {
        transport: &transport,
        clock: &SystemClock,
        sleeper: &ThreadSleeper,
        jitter: &FullJitter::new(),
        policy: RetryPolicy::default(),
    };
    let tokens = oidc
        .exchange(
            &client,
            &Secret::new(code.to_owned()),
            &config.code_verifier,
            redirect,
            now,
            cutoff,
        )
        .map_err(|_| "exchange_failed")?;
    let session = StoredSession {
        client_id: client.id,
        client_secret: client.secret,
        client_expires_at: client.expires_at,
        authorize_endpoint: "https://oidc.us-east-1.amazonaws.com/authorize".into(),
        token_endpoint: "https://oidc.us-east-1.amazonaws.com/token".into(),
        scopes: client.scopes,
        access_token: tokens.access,
        refresh_token: tokens.refresh,
        token_expires_at: tokens.expires_at,
    };
    Ok(session.to_json_string().into_bytes())
}

#[cfg(feature = "aws-sdk")]
fn unix_time() -> Option<i64> {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}
