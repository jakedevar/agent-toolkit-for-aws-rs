//! Parent-side cached sign-in and helper launch.
use super::{
    spawn::{self, HelperConfig},
    wait::failure,
};
use crate::launch::{
    clock::OsRandom,
    config::{self, RegionInputs},
    oidc::{self, Client},
    state,
};
use crate::{Invocation, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use toolkit_aws::{Env, ProcessEnv, SystemResolver};
use toolkit_core::ToolkitError;

/// Reuse a cached session, otherwise launch an isolated callback helper.
#[allow(clippy::unnecessary_wraps)]
pub fn command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    #[cfg(unix)]
    {
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
        let Some(now) = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
        else {
            return failure(output, "The system clock is unavailable.");
        };
        let session = state::load_session(&dir);
        if session
            .as_ref()
            .is_some_and(|s| s.token_expires_at > now.saturating_add(300))
        {
            return output.line(&serde_json::json!({"authenticated":true,"reusedCachedSession":true,"baseUrl":base}).to_string());
        }
        let client = match obtain_client(&env, &dir, session.as_ref(), now) {
            Ok(StartClient::Refreshed) => return output.line(&serde_json::json!({"authenticated":true,"reusedCachedSession":false,"baseUrl":base}).to_string()),
            Ok(StartClient::Interactive(client)) => client,
            Err(message) => return failure(output, message),
        };
        let Ok(pkce) = oidc::generate_pkce(&OsRandom) else {
            return failure(
                output,
                "The operating system random number generator is unavailable.",
            );
        };
        let config = HelperConfig {
            client_id: client.id.clone(),
            client_secret: client.secret,
            client_expires_at: client.expires_at,
            scopes: client.scopes.clone(),
            code_verifier: pkce.verifier,
            state: pkce.state,
            deadline: now.saturating_add(600),
        };
        let Ok((pid, port)) = spawn::spawn(&config) else {
            return failure(output, "The sign-in helper failed to start.");
        };
        let redirect = format!("http://127.0.0.1:{port}");
        let query = [
            ("response_type", "code"),
            ("client_id", client.id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("state", config.state.expose()),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("scope", &client.scopes.join(" ")),
        ]
        .into_iter()
        .map(|(key, value)| format!("{key}={}", form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
        output.line(&serde_json::json!({"authenticated":false,"signInUrl":format!("https://oidc.us-east-1.amazonaws.com/authorize?{query}"),"pid":pid,"port":port,"baseUrl":base}).to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = invocation;
        failure(output, "This command is not supported on this platform.")
    }
}

fn form_encode(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

enum StartClient {
    Refreshed,
    Interactive(Client),
}

#[cfg(not(feature = "aws-sdk"))]
fn obtain_client(
    _env: &dyn Env,
    _dir: &std::path::Path,
    session: Option<&state::StoredSession>,
    now: i64,
) -> Result<StartClient, &'static str> {
    let Some(session) = session.filter(|s| s.client_expires_at > now) else {
        return Err("The sign-in service transport is unavailable.");
    };
    Ok(StartClient::Interactive(Client {
        id: session.client_id.clone(),
        secret: session.client_secret.clone(),
        expires_at: session.client_expires_at,
        scopes: session.scopes.clone(),
    }))
}

#[cfg(feature = "aws-sdk")]
fn obtain_client(
    env: &dyn Env,
    dir: &std::path::Path,
    session: Option<&state::StoredSession>,
    now: i64,
) -> Result<StartClient, &'static str> {
    use crate::launch::http::SystemClock;
    use toolkit_aws::{
        retry::{FullJitter, RetryPolicy, ThreadSleeper},
        sdk::{roots::ValidatedRoots, SdkRuntime, SdkTransport},
    };
    let roots = ValidatedRoots::from_process()
        .map_err(|_| "The sign-in service transport is unavailable.")?;
    let runtime = SdkRuntime::new().map_err(|_| "The sign-in service transport is unavailable.")?;
    let transport = SdkTransport::new(&runtime, &roots)
        .map_err(|_| "The sign-in service transport is unavailable.")?;
    let oidc = oidc::Oidc {
        transport: &transport,
        clock: &SystemClock,
        sleeper: &ThreadSleeper,
        jitter: &FullJitter::new(),
        policy: RetryPolicy::default(),
    };
    if let Some(session) = session.filter(|s| s.client_expires_at > now) {
        if oidc
            .access_token(
                dir,
                &OsRandom,
                now,
                std::time::Instant::now() + std::time::Duration::from_secs(30),
            )
            .is_ok()
        {
            return Ok(StartClient::Refreshed);
        }
        return Ok(StartClient::Interactive(Client {
            id: session.client_id.clone(),
            secret: session.client_secret.clone(),
            expires_at: session.client_expires_at,
            scopes: session.scopes.clone(),
        }));
    }
    let (issuer, _) = config::resolve_issuer_url(env).map_err(|_| "Invalid sign-in issuer.")?;
    let scopes = env
        .var(config::ENV_SCOPES)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || vec!["launch:access".to_owned()],
            |s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            },
        );
    oidc.register(
        &issuer,
        &scopes,
        now,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
    .map(StartClient::Interactive)
    .map_err(|_| "Could not register the sign-in client.")
}
