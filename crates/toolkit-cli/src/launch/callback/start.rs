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
use toolkit_core::{Json, ToolkitError};

/// The `auth-start` success line for an authenticated session (section 1.3).
fn started(reused: bool, base: &str) -> String {
    crate::launch::text::ok_text(&Json::object(vec![
        ("authenticated", Json::Bool(true)),
        ("reusedCachedSession", Json::Bool(reused)),
        ("baseUrl", Json::string(base)),
    ]))
}

/// Reuse a cached session, otherwise launch an isolated callback helper.
#[allow(clippy::unnecessary_wraps)]
pub fn command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    crate::launch::sink::guard(output);
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
        super::pending::cleanup_stale(&dir, now);
        let session = state::load_session(&dir);
        if session
            .as_ref()
            .is_some_and(|s| s.token_expires_at > now.saturating_add(300))
        {
            return output.line(&started(true, &base));
        }
        let client = match obtain_client(&env, &dir, session.as_ref(), now, output) {
            Ok(StartClient::Refreshed) => return output.line(&started(false, &base)),
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
        output.line(&crate::launch::text::ok_text(&Json::object(vec![
            ("authenticated", Json::Bool(false)),
            (
                "signInUrl",
                Json::string(format!(
                    "https://oidc.us-east-1.amazonaws.com/authorize?{query}"
                )),
            ),
            ("pid", Json::Int(i64::from(pid))),
            ("port", Json::Int(i64::from(port))),
            ("baseUrl", Json::string(base)),
        ])))
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
    _output: &mut Output<'_>,
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
    output: &mut Output<'_>,
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
    register_client(&oidc, env, now, output).map(StartClient::Interactive)
}

/// Interactive client registration. For a `LAUNCH_WITH_AWS_IDC_ISSUER_URL`
/// override the `Using non-default IdC issuer URL: <shown>` warning goes to
/// stderr through the guarded output before the request (design section 1.4
/// item 10 and R-TOK-OIDC; #5504).
#[cfg(any(test, feature = "aws-sdk"))]
fn register_client(
    oidc: &oidc::Oidc<'_>,
    env: &dyn Env,
    now: i64,
    output: &mut Output<'_>,
) -> Result<Client, &'static str> {
    let (issuer, warning) =
        config::resolve_issuer_url(env).map_err(|_| "Invalid sign-in issuer.")?;
    if let Some(warning) = warning {
        output.diagnostic(&warning);
    }
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
    .map_err(|_| "Could not register the sign-in client.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::http::SystemClock;
    use toolkit_aws::env::MapEnv;
    use toolkit_aws::retry::{NoJitter, RetryPolicy};
    use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};

    const ISSUER: &str = "https://my-org.awsapps.com/start";

    /// Run `register_client` behind the stored-session sink with a scripted
    /// registration response; return the result, stdout, stderr, exit status
    /// and the request bodies.
    fn register(
        env: &MapEnv,
        status: u16,
    ) -> (
        Result<Client, &'static str>,
        String,
        String,
        u8,
        Vec<String>,
    ) {
        let fake = FakeStreamingTransport::new().respond(
            status,
            &[],
            vec![FakeChunk::Data(
                br#"{"clientId":"id","clientSecret":"FAKE-L7-CLIENT-SECRET"}"#.to_vec(),
            )],
        );
        let sleeper = RecordingSleeper::new();
        let oidc = oidc::Oidc {
            transport: &fake,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (result, status) = {
            let mut output = Output::with_diagnostics(&mut out, &mut err);
            crate::launch::sink::guard_in(None, &mut output);
            let result = register_client(&oidc, env, 0, &mut output);
            (result, output.exit_status())
        };
        let bodies = fake
            .requests()
            .iter()
            .map(|request| String::from_utf8(request.body.clone()).unwrap())
            .collect();
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            status,
            bodies,
        )
    }

    /// R-TOK-OIDC (#5504): an issuer override prints the documented warning on
    /// stderr, once, on registration (also when registration then fails); the
    /// default issuer prints nothing; a refused override prints nothing and
    /// sends no request; stdout stays empty.
    #[test]
    fn t_url_issuer_override_warning_is_emitted_on_registration() {
        let warning = format!("Using non-default IdC issuer URL: {ISSUER}\n");
        let custom = MapEnv::new().with(config::ENV_IDC_ISSUER_URL, ISSUER);

        let (result, stdout, stderr, status, bodies) = register(&custom, 200);
        assert_eq!(result.expect("registered").id, "id");
        assert_eq!(
            (stdout.as_str(), stderr.as_str(), status),
            ("", warning.as_str(), 0)
        );
        assert_eq!(bodies.len(), 1);
        assert!(
            bodies[0].contains(&format!(r#""issuerUrl":"{ISSUER}""#)),
            "{}",
            bodies[0]
        );

        let (result, stdout, stderr, _, bodies) = register(&custom, 503);
        assert_eq!(result.err(), Some("Could not register the sign-in client."));
        assert_eq!((stdout.as_str(), stderr.as_str()), ("", warning.as_str()));
        assert_eq!(bodies.len(), 1);

        let (result, stdout, stderr, status, bodies) = register(&MapEnv::new(), 200);
        assert!(result.is_ok());
        assert_eq!((stdout.as_str(), stderr.as_str(), status), ("", "", 0));
        assert!(bodies[0].contains(&format!(r#""issuerUrl":"{}""#, config::IDC_ISSUER_URL)));

        let refused = MapEnv::new().with(
            config::ENV_IDC_ISSUER_URL,
            "http://my-org.awsapps.com/start",
        );
        let (result, stdout, stderr, _, bodies) = register(&refused, 200);
        assert_eq!(result.err(), Some("Invalid sign-in issuer."));
        assert_eq!((stdout.as_str(), stderr.as_str()), ("", ""));
        assert!(bodies.is_empty());
    }
}
