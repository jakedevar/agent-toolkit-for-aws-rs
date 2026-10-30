//! OIDC registration and token lifecycle. The service endpoint is fixed.
use super::{
    http::{self, MonotonicClock},
    state::StoredSession,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use toolkit_aws::{
    retry::{Jitter, RetryPolicy, Sleeper},
    stream::StreamingTransport,
    transport::{Headers, HttpRequest, Method, Timeouts},
    Secret,
};

const ENDPOINT: &str = "https://oidc.us-east-1.amazonaws.com";
const MAX_LIFE: i64 = 90 * 24 * 3600;
const DEFAULT_LIFE: i64 = 28_800;
pub const SESSION_EXPIRED: &str =
    "Session expired or not authenticated. Run auth-start to sign in.";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OidcError {
    InvalidResponse,
    Unavailable,
    Service { status: u16, code: &'static str },
    SessionExpired,
}
impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidResponse => f.write_str("The service returned an invalid response."),
            Self::Unavailable => f.write_str("Could not reach the sign-in service."),
            Self::Service { status, code } => write!(f, "OIDC request failed ({status}): {code}"),
            Self::SessionExpired => f.write_str(SESSION_EXPIRED),
        }
    }
}
#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: Secret,
    pub expires_at: i64,
    pub scopes: Vec<String>,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &"[REDACTED]")
            .field("secret", &self.secret)
            .field("expires_at", &self.expires_at)
            .field("scopes", &"[REDACTED]")
            .finish()
    }
}
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: Secret,
    pub refresh: Secret,
    pub expires_at: i64,
}

/// PKCE material and callback state, generated independently from the OS RNG.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: Secret,
    pub state: Secret,
    pub challenge: String,
}
pub fn generate_pkce(random: &dyn super::clock::Random) -> Result<Pkce, super::LaunchError> {
    let mut bytes = [0_u8; 32];
    random.fill(&mut bytes)?;
    let verifier = toolkit_core::base64url_nopad(&bytes);
    let challenge = toolkit_core::base64url_nopad(&toolkit_core::sha256(verifier.as_bytes()));
    random.fill(&mut bytes)?;
    Ok(Pkce {
        verifier: Secret::new(verifier),
        state: Secret::new(toolkit_core::base64url_nopad(&bytes)),
        challenge,
    })
}

const CODES: &[&str] = &[
    "AccessDeniedException",
    "AuthorizationPendingException",
    "ExpiredTokenException",
    "InternalServerException",
    "InvalidClientException",
    "InvalidClientMetadataException",
    "InvalidGrantException",
    "InvalidRedirectUriException",
    "InvalidRequestException",
    "InvalidRequestRegionException",
    "InvalidScopeException",
    "SlowDownException",
    "UnauthorizedClientException",
    "UnsupportedGrantTypeException",
    "invalid_request",
    "invalid_client",
    "invalid_grant",
    "unauthorized_client",
    "unsupported_grant_type",
    "invalid_scope",
];
pub fn error_code(header: Option<&str>, body: &[u8]) -> &'static str {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let candidate = header.or_else(|| {
        parsed.as_ref().and_then(|v| {
            ["__type", "code", "Code", "errorCode", "error"]
                .iter()
                .find_map(|key| v.get(key).and_then(Value::as_str))
        })
    });
    let normalized = candidate
        .unwrap_or("")
        .rsplit('#')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    CODES
        .iter()
        .copied()
        .find(|code| *code == normalized)
        .unwrap_or("UnrecognizedError")
}
pub fn scrub(message: &str, secrets: &[&Secret]) -> String {
    if secrets
        .iter()
        .any(|s| s.expose().len() >= 8 && message.contains(s.expose()))
    {
        "The operation failed. Re-run with logging enabled for details.".to_string()
    } else {
        message.to_string()
    }
}
pub struct Oidc<'a> {
    pub transport: &'a dyn StreamingTransport,
    pub clock: &'a dyn MonotonicClock,
    pub sleeper: &'a dyn Sleeper,
    pub jitter: &'a dyn Jitter,
    pub policy: RetryPolicy,
}
impl Oidc<'_> {
    /// Return a cached access token, or refresh and atomically persist it.
    pub fn access_token(
        &self,
        dir: &std::path::Path,
        random: &dyn super::clock::Random,
        now: i64,
        deadline: Instant,
    ) -> Result<Secret, OidcError> {
        let mut session = super::state::load_session(dir).ok_or(OidcError::SessionExpired)?;
        if session.token_expires_at > now.saturating_add(300) {
            return Ok(session.access_token);
        }
        if session.client_expires_at <= now {
            return Err(OidcError::SessionExpired);
        }
        let token = self.refresh(&session, now, deadline)?;
        session.access_token = token.access.clone();
        session.refresh_token = token.refresh;
        session.token_expires_at = token.expires_at;
        super::state::save_session(dir, &session, random).map_err(|_| OidcError::SessionExpired)?;
        Ok(token.access)
    }
    fn call(
        &self,
        path: &str,
        body: &Value,
        deadline: Instant,
        single_use: bool,
    ) -> Result<Value, OidcError> {
        let mut headers = Headers::new();
        headers
            .insert("content-type", "application/json")
            .map_err(|_| OidcError::InvalidResponse)?;
        let timeout = Duration::from_secs(30);
        let request = HttpRequest {
            method: Method::Post,
            url: format!("{ENDPOINT}{path}"),
            headers,
            body: serde_json::to_vec(body).map_err(|_| OidcError::InvalidResponse)?,
            timeouts: Timeouts::new(timeout, timeout).map_err(|_| OidcError::InvalidResponse)?,
        };
        let response = http::run(
            self.transport,
            &request,
            http::Operation::Oidc,
            deadline,
            timeout,
            single_use,
            self.clock,
            self.sleeper,
            self.jitter,
            &self.policy,
        )
        .map_err(|e| match e {
            http::BodyError::InvalidResponse
            | http::BodyError::TooLarge
            | http::BodyError::Allocation => OidcError::InvalidResponse,
            _ => OidcError::Unavailable,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(OidcError::Service {
                status: response.status,
                code: error_code(response.error_header.as_deref(), &response.body),
            });
        }
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| OidcError::InvalidResponse)?;
        if !value.is_object() {
            return Err(OidcError::InvalidResponse);
        }
        Ok(value)
    }
    pub fn register(
        &self,
        issuer: &str,
        scopes: &[String],
        now: i64,
        deadline: Instant,
    ) -> Result<Client, OidcError> {
        let deadline = deadline.min(self.clock.now() + Duration::from_secs(30));
        let v = self.call("/client/register", &json!({"clientName":"Launch with AWS Agent Skill", "clientType":"public", "grantTypes":["authorization_code","refresh_token"], "scopes":scopes, "issuerUrl":issuer, "redirectUris":["http://127.0.0.1"]}), deadline, false)?;
        let id = required(&v, "clientId")?.to_owned();
        let secret = Secret::new(required(&v, "clientSecret")?);
        let cap = now.saturating_add(MAX_LIFE);
        let expiry = v
            .get("clientSecretExpiresAt")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        Ok(Client {
            id,
            secret,
            expires_at: if expiry == 0 { cap } else { expiry.min(cap) },
            scopes: scopes.to_vec(),
        })
    }
    pub fn exchange(
        &self,
        client: &Client,
        code: &Secret,
        verifier: &Secret,
        redirect: &str,
        now: i64,
        deadline: Instant,
    ) -> Result<Tokens, OidcError> {
        let v = self.call("/token", &json!({"clientId":client.id,"clientSecret":client.secret.expose(),"grantType":"authorization_code","code":code.expose(),"codeVerifier":verifier.expose(),"redirectUri":redirect}), deadline, true)?;
        tokens(&v, None, now)
    }
    pub fn refresh(
        &self,
        session: &StoredSession,
        now: i64,
        deadline: Instant,
    ) -> Result<Tokens, OidcError> {
        let deadline = deadline.min(self.clock.now() + Duration::from_secs(30));
        let result = self.call("/token", &json!({"clientId":session.client_id,"clientSecret":session.client_secret.expose(),"grantType":"refresh_token","refreshToken":session.refresh_token.expose()}), deadline, false).and_then(|v| tokens(&v, Some(&session.refresh_token), now));
        result.map_err(|_| OidcError::SessionExpired)
    }
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str, OidcError> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(OidcError::InvalidResponse)
}
fn tokens(v: &Value, old: Option<&Secret>, now: i64) -> Result<Tokens, OidcError> {
    let access = Secret::new(required(v, "accessToken")?);
    let refresh = v
        .get("refreshToken")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(Secret::new)
        .or_else(|| old.cloned())
        .ok_or(OidcError::InvalidResponse)?;
    let life = match v.get("expiresIn") {
        Some(value) if value.as_u64().is_some_and(|n| n > MAX_LIFE as u64) => MAX_LIFE,
        value => value
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_LIFE)
            .min(MAX_LIFE),
    };
    Ok(Tokens {
        access,
        refresh,
        expires_at: now.saturating_add(life),
    })
}

#[cfg(test)]
mod tests {
    use super::super::http::SystemClock;
    use super::*;
    use toolkit_aws::retry::NoJitter;
    use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};
    fn response(body: &str) -> FakeChunk {
        FakeChunk::Data(body.as_bytes().to_vec())
    }
    #[test]
    fn registration_and_exchange_use_fixed_endpoint_and_exact_bodies() {
        let fake = FakeStreamingTransport::new().respond(200, &[], vec![response(r#"{"clientId":"id","clientSecret":"FAKE-CLIENT-SECRET","clientSecretExpiresAt":99999999}"#)]).respond(200, &[], vec![response(r#"{"accessToken":"FAKE-ACCESS","refreshToken":"FAKE-REFRESH","expiresIn":999999999}"#)]);
        let clock = SystemClock;
        let sleeper = RecordingSleeper::new();
        let oidc = Oidc {
            transport: &fake,
            clock: &clock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let scopes = vec!["launch:access".to_string()];
        let client = oidc
            .register("https://example.awsapps.com/start", &scopes, 100, deadline)
            .unwrap();
        assert_eq!(client.expires_at, 100 + MAX_LIFE);
        let token = oidc
            .exchange(
                &client,
                &Secret::new("FAKE-CODE"),
                &Secret::new("FAKE-VERIFIER"),
                "http://127.0.0.1:1234",
                100,
                deadline,
            )
            .unwrap();
        assert_eq!(token.expires_at, 100 + MAX_LIFE);
        let requests = fake.requests();
        assert_eq!(requests[0].url, format!("{ENDPOINT}/client/register"));
        assert_eq!(requests[1].url, format!("{ENDPOINT}/token"));
        assert!(requests
            .iter()
            .all(|r| r.headers.get("authorization").is_none()));
        let registration: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            registration,
            json!({"clientName":"Launch with AWS Agent Skill", "clientType":"public", "grantTypes":["authorization_code","refresh_token"], "scopes":["launch:access"], "issuerUrl":"https://example.awsapps.com/start", "redirectUris":["http://127.0.0.1"]})
        );
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(
            body,
            json!({"clientId":"id","clientSecret":"FAKE-CLIENT-SECRET","grantType":"authorization_code","code":"FAKE-CODE","codeVerifier":"FAKE-VERIFIER","redirectUri":"http://127.0.0.1:1234"})
        );
        for shown in [
            format!("{client:?}"),
            format!("{token:?}"),
            format!("{requests:?}"),
        ] {
            assert!(!shown.contains("FAKE-"));
        }
    }
    #[test]
    fn token_lifetimes_missing_values_and_refresh_fallback() {
        for expiry in [json!(null), json!(0), json!(-5)] {
            let v = json!({"accessToken":"a", "expiresIn":expiry});
            let old = Secret::new("old-refresh");
            let t = tokens(&v, Some(&old), 42).unwrap();
            assert_eq!(t.expires_at, 42 + DEFAULT_LIFE);
            assert_eq!(t.refresh, old);
            assert!(tokens(&v, None, 42).is_err());
        }
        assert!(tokens(&json!({"refreshToken":"r"}), None, 42).is_err());
        assert_eq!(
            tokens(
                &json!({"accessToken":"a", "refreshToken":"r", "expiresIn":u64::MAX}),
                None,
                42
            )
            .unwrap()
            .expires_at,
            42 + MAX_LIFE
        );
    }
    #[test]
    fn pkce_and_state_are_independent_and_debug_hides_them() {
        let random = super::super::clock::SeededRandom::new(314);
        let pkce = generate_pkce(&random).unwrap();
        assert_eq!(pkce.verifier.expose().len(), 43);
        assert_eq!(pkce.state.expose().len(), 43);
        assert_ne!(pkce.verifier.expose(), pkce.state.expose());
        assert_eq!(
            pkce.challenge,
            toolkit_core::base64url_nopad(&toolkit_core::sha256(pkce.verifier.expose().as_bytes()))
        );
        let shown = format!("{pkce:?}");
        assert!(!shown.contains(pkce.verifier.expose()));
        assert!(!shown.contains(pkce.state.expose()));
    }
    #[test]
    fn error_codes_never_echo_secrets_or_descriptions() {
        for secret in [
            "FAKE-CLIENT-SECRET",
            "FAKE-ACCESS",
            "FAKE-REFRESH",
            "FAKE-VERIFIER",
            "FAKE-CODE",
            "FAKE-STATE",
        ] {
            assert_eq!(error_code(Some(secret), b"{}"), "UnrecognizedError");
            for key in ["__type", "code", "Code", "errorCode", "error"] {
                let body = json!({key:secret, "error_description":"FAKE-ACCESS"}).to_string();
                assert_eq!(error_code(None, body.as_bytes()), "UnrecognizedError");
            }
        }
        assert_eq!(
            error_code(Some("x#InvalidGrantException:detail"), b"{}"),
            "InvalidGrantException"
        );
        assert_eq!(error_code(Some("FAKE-ACCESS"), b"{}"), "UnrecognizedError");
        assert!(
            !scrub("failure FAKE-ACCESS", &[&Secret::new("FAKE-ACCESS")]).contains("FAKE-ACCESS")
        );
    }

    #[test]
    fn service_errors_do_not_expose_request_id_or_long_secret_codes() {
        let secrets = [
            "FAKE-ACCESS",
            "FAKE-REFRESH",
            "FAKE-CLIENT-SECRET",
            "FAKE-VERIFIER",
            "FAKE-CODE",
        ];
        for secret in secrets {
            let code = format!("{}{secret}", "InvalidGrantException".repeat(6));
            let fake = FakeStreamingTransport::new().respond(
                400,
                &[("x-amzn-errortype", &code), ("x-amzn-requestid", secret)],
                vec![response(&json!({"error_description": secret}).to_string())],
            );
            let sleeper = RecordingSleeper::new();
            let oidc = Oidc {
                transport: &fake,
                clock: &SystemClock,
                sleeper: &sleeper,
                jitter: &NoJitter,
                policy: RetryPolicy::none(),
            };
            let error = oidc
                .register(
                    "https://example.awsapps.com/start",
                    &[],
                    0,
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap_err();
            assert_eq!(
                error,
                OidcError::Service {
                    status: 400,
                    code: "UnrecognizedError"
                }
            );
            let rendered = format!("{error} {error:?} {:?}", fake.requests());
            for hidden in secrets {
                assert!(!rendered.contains(hidden));
            }
        }
    }

    #[test]
    fn oidc_error_results_hide_each_seeded_secret_in_header_and_body() {
        let secrets = [
            "FAKE-ACCESS",
            "FAKE-REFRESH",
            "FAKE-CLIENT-SECRET",
            "FAKE-VERIFIER",
            "FAKE-CODE",
        ];
        for secret in secrets {
            for key in ["__type", "code", "Code", "errorCode", "error"] {
                for in_header in [false, true] {
                    let headers = if in_header {
                        vec![("x-amzn-errortype", secret), ("x-amzn-requestid", secret)]
                    } else {
                        vec![("x-amzn-requestid", secret)]
                    };
                    let body = json!({key: secret, "error_description": secret}).to_string();
                    let fake =
                        FakeStreamingTransport::new().respond(400, &headers, vec![response(&body)]);
                    let sleeper = RecordingSleeper::new();
                    let oidc = Oidc {
                        transport: &fake,
                        clock: &SystemClock,
                        sleeper: &sleeper,
                        jitter: &NoJitter,
                        policy: RetryPolicy::none(),
                    };
                    let err = oidc
                        .register(
                            "https://example.awsapps.com/start",
                            &[],
                            0,
                            Instant::now() + Duration::from_secs(30),
                        )
                        .unwrap_err();
                    assert_eq!(
                        err,
                        OidcError::Service {
                            status: 400,
                            code: "UnrecognizedError"
                        }
                    );
                    let rendered = format!("{err} {err:?}");
                    for hidden in secrets {
                        assert!(!rendered.contains(hidden));
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_registration_and_exchange_responses_are_rejected() {
        let client = Client {
            id: "id".into(),
            secret: Secret::new("FAKE-CLIENT-SECRET"),
            expires_at: 1,
            scopes: vec![],
        };
        for body in [
            "null",
            "[]",
            "{",
            "{}",
            r#"{"clientId":"id"}"#,
            r#"{"clientId":"","clientSecret":"s"}"#,
        ] {
            let fake = FakeStreamingTransport::new().respond(200, &[], vec![response(body)]);
            let sleeper = RecordingSleeper::new();
            let oidc = Oidc {
                transport: &fake,
                clock: &SystemClock,
                sleeper: &sleeper,
                jitter: &NoJitter,
                policy: RetryPolicy::none(),
            };
            assert_eq!(
                oidc.register("issuer", &[], 0, Instant::now() + Duration::from_secs(30))
                    .unwrap_err(),
                OidcError::InvalidResponse
            );
        }
        for body in [
            "null",
            "[]",
            "{",
            "{}",
            r#"{"accessToken":"a"}"#,
            r#"{"accessToken":"","refreshToken":"r"}"#,
        ] {
            let fake = FakeStreamingTransport::new().respond(200, &[], vec![response(body)]);
            let sleeper = RecordingSleeper::new();
            let oidc = Oidc {
                transport: &fake,
                clock: &SystemClock,
                sleeper: &sleeper,
                jitter: &NoJitter,
                policy: RetryPolicy::none(),
            };
            assert_eq!(
                oidc.exchange(
                    &client,
                    &Secret::new("FAKE-CODE"),
                    &Secret::new("FAKE-VERIFIER"),
                    "http://127.0.0.1",
                    0,
                    Instant::now() + Duration::from_secs(30)
                )
                .unwrap_err(),
                OidcError::InvalidResponse
            );
        }
    }

    #[test]
    fn oversized_exchange_fails_without_a_session_and_is_not_retried() {
        let fake = FakeStreamingTransport::new()
            .respond(
                200,
                &[],
                vec![FakeChunk::Data(vec![
                    b'x';
                    super::super::http::SMALL_CAP + 1
                ])],
            )
            .respond(
                200,
                &[],
                vec![response(r#"{"accessToken":"a","refreshToken":"r"}"#)],
            );
        let sleeper = RecordingSleeper::new();
        let oidc = Oidc {
            transport: &fake,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::default(),
        };
        let client = Client {
            id: "id".into(),
            secret: Secret::new("FAKE-CLIENT-SECRET"),
            expires_at: 100,
            scopes: vec![],
        };
        let scratch = super::super::tests_support::Scratch::new("oversized-exchange");
        assert_eq!(
            oidc.exchange(
                &client,
                &Secret::new("FAKE-CODE"),
                &Secret::new("FAKE-VERIFIER"),
                "http://127.0.0.1",
                0,
                Instant::now() + Duration::from_secs(30)
            )
            .unwrap_err(),
            OidcError::InvalidResponse
        );
        assert!(super::super::state::load_session(scratch.path()).is_none());
        assert_eq!(fake.requests().len(), 1);
        assert!(sleeper.sleeps().is_empty());
    }

    /// T-OIDC single use (#5504): with retries enabled, a code exchange that
    /// receives an HTTP 503 makes exactly one request, never sleeps and fails;
    /// the queued success is never consumed.
    #[test]
    fn code_exchange_503_with_default_retries_makes_exactly_one_request() {
        let fake = FakeStreamingTransport::new()
            .respond(503, &[], vec![])
            .respond(
                200,
                &[],
                vec![response(
                    r#"{"accessToken":"FAKE-L7-ACCESS","refreshToken":"FAKE-L7-REFRESH"}"#,
                )],
            );
        let sleeper = RecordingSleeper::new();
        let policy = RetryPolicy::default();
        assert!(policy.total_attempts() > 1, "retries are enabled");
        let oidc = Oidc {
            transport: &fake,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy,
        };
        let client = Client {
            id: "id".into(),
            secret: Secret::new("FAKE-L7-CLIENT"),
            expires_at: 100,
            scopes: vec![],
        };
        let error = oidc
            .exchange(
                &client,
                &Secret::new("FAKE-L7-CODE"),
                &Secret::new("FAKE-L7-VERIFIER"),
                "http://127.0.0.1",
                0,
                Instant::now() + Duration::from_secs(30),
            )
            .expect_err("a single-use code is not retried");
        assert!(
            matches!(error, OidcError::Service { status: 503, .. }),
            "{error:?}"
        );
        assert_eq!(fake.requests().len(), 1);
        assert!(sleeper.sleeps().is_empty());
        // Control: the same 503-then-success script is retried by register.
        let fake = FakeStreamingTransport::new()
            .respond(503, &[], vec![])
            .respond(
                200,
                &[],
                vec![response(r#"{"clientId":"id","clientSecret":"s"}"#)],
            );
        let oidc = Oidc {
            transport: &fake,
            ..oidc
        };
        oidc.register(
            "https://view.awsapps.com/start",
            &[],
            0,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("register retries the 503");
        assert_eq!(fake.requests().len(), 2);
    }

    #[test]
    fn aws_endpoint_overrides_in_injected_environment_do_not_change_oidc_target() {
        let env = toolkit_aws::env::MapEnv::new()
            .with("AWS_ENDPOINT_URL", "https://attacker.example")
            .with("AWS_ENDPOINT_URL_SSO_OIDC", "https://attacker.example");
        let (issuer, _) = super::super::config::resolve_issuer_url(&env).unwrap();
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[],
            vec![response(r#"{"clientId":"id","clientSecret":"s"}"#)],
        );
        let sleeper = RecordingSleeper::new();
        let oidc = Oidc {
            transport: &fake,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        oidc.register(&issuer, &[], 0, Instant::now() + Duration::from_secs(30))
            .unwrap();
        assert_eq!(
            fake.requests()[0].url,
            format!("{ENDPOINT}/client/register")
        );
    }

    #[test]
    fn secret_holding_l3_values_have_safe_debug_output() {
        let session = StoredSession {
            client_id: "id".into(),
            client_secret: Secret::new("FAKE-CLIENT-SECRET"),
            client_expires_at: 10,
            authorize_endpoint: format!("{ENDPOINT}/authorize"),
            token_endpoint: format!("{ENDPOINT}/token"),
            scopes: vec![],
            access_token: Secret::new("FAKE-ACCESS"),
            refresh_token: Secret::new("FAKE-REFRESH"),
            token_expires_at: 10,
        };
        let pkce = Pkce {
            verifier: Secret::new("FAKE-VERIFIER"),
            state: Secret::new("FAKE-STATE"),
            challenge: "challenge".into(),
        };
        let response = super::super::http::Response {
            status: 400,
            body: b"FAKE-CODE".to_vec(),
            error_header: Some("FAKE-ACCESS".into()),
            collector_capacity: 9,
            collector_peak: 9,
        };
        let shown = format!("{session:?} {pkce:?} {response:?}");
        for secret in [
            "FAKE-CLIENT-SECRET",
            "FAKE-ACCESS",
            "FAKE-REFRESH",
            "FAKE-VERIFIER",
            "FAKE-STATE",
            "FAKE-CODE",
        ] {
            assert!(!shown.contains(secret));
        }
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::super::http::SystemClock;
    use super::*;
    use toolkit_aws::retry::NoJitter;
    use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};
    #[test]
    fn refresh_keeps_old_refresh_token_and_failure_is_generic() {
        let session = StoredSession {
            client_id: "id".into(),
            client_secret: Secret::new("FAKE-CLIENT-SECRET"),
            client_expires_at: 5000,
            authorize_endpoint: format!("{ENDPOINT}/authorize"),
            token_endpoint: format!("{ENDPOINT}/token"),
            scopes: vec![],
            access_token: Secret::new("FAKE-ACCESS"),
            refresh_token: Secret::new("FAKE-REFRESH"),
            token_expires_at: 0,
        };
        let fake = FakeStreamingTransport::new()
            .respond(
                200,
                &[],
                vec![FakeChunk::Data(br#"{"accessToken":"new"}"#.to_vec())],
            )
            .respond(
                400,
                &[],
                vec![FakeChunk::Data(
                    br#"{"error":"FAKE-CLIENT-SECRET","error_description":"FAKE-REFRESH"}"#
                        .to_vec(),
                )],
            );
        let sleeper = RecordingSleeper::new();
        let oidc = Oidc {
            transport: &fake,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let token = oidc.refresh(&session, 100, deadline).unwrap();
        assert_eq!(token.refresh, session.refresh_token);
        let error = oidc.refresh(&session, 100, deadline).unwrap_err();
        assert_eq!(error, OidcError::SessionExpired);
        assert!(!format!("{error:?} {error}").contains("FAKE-"));
        let requests = fake.requests();
        let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            request,
            json!({"clientId":"id","clientSecret":"FAKE-CLIENT-SECRET","grantType":"refresh_token","refreshToken":"FAKE-REFRESH"})
        );
    }

    #[test]
    fn cached_and_refreshed_access_are_persisted_with_expiry_buffer() {
        let scratch = super::super::tests_support::Scratch::new("oidc-session");
        let random = super::super::clock::SeededRandom::new(9);
        let mut session = StoredSession {
            client_id: "id".into(),
            client_secret: Secret::new("FAKE-CLIENT-SECRET"),
            client_expires_at: 5000,
            authorize_endpoint: format!("{ENDPOINT}/authorize"),
            token_endpoint: format!("{ENDPOINT}/token"),
            scopes: vec![],
            access_token: Secret::new("FAKE-ACCESS"),
            refresh_token: Secret::new("FAKE-REFRESH"),
            token_expires_at: 401,
        };
        super::super::state::save_session(scratch.path(), &session, &random).unwrap();
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[],
            vec![FakeChunk::Data(
                br#"{"accessToken":"FAKE-NEW-ACCESS","expiresIn":3600}"#.to_vec(),
            )],
        );
        let sleeper = RecordingSleeper::new();
        let oidc = Oidc {
            transport: &fake,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        assert_eq!(
            oidc.access_token(scratch.path(), &random, 100, deadline)
                .unwrap(),
            session.access_token
        );
        assert!(fake.requests().is_empty());
        session.token_expires_at = 400;
        super::super::state::save_session(scratch.path(), &session, &random).unwrap();
        assert_eq!(
            oidc.access_token(scratch.path(), &random, 100, deadline)
                .unwrap()
                .expose(),
            "FAKE-NEW-ACCESS"
        );
        let saved = super::super::state::load_session(scratch.path()).unwrap();
        assert_eq!(saved.token_expires_at, 3700);
        assert_eq!(saved.refresh_token, session.refresh_token);
        assert_eq!(fake.requests().len(), 1);
    }
}
