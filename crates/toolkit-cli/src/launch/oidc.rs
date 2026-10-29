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
#[derive(Debug, Clone)]
pub struct Client {
    pub id: String,
    pub secret: Secret,
    pub expires_at: i64,
    pub scopes: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: Secret,
    pub refresh: Secret,
    pub expires_at: i64,
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
    fn call(
        &self,
        path: &str,
        body: Value,
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
            body: serde_json::to_vec(&body).map_err(|_| OidcError::InvalidResponse)?,
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
        let v = self.call("/client/register", json!({"clientName":"Launch with AWS Agent Skill", "clientType":"public", "grantTypes":["authorization_code","refresh_token"], "scopes":scopes, "issuerUrl":issuer, "redirectUris":["http://127.0.0.1"]}), deadline, false)?;
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
        let v = self.call("/token", json!({"clientId":client.id,"clientSecret":client.secret.expose(),"grantType":"authorization_code","code":code.expose(),"codeVerifier":verifier.expose(),"redirectUri":redirect}), deadline, true)?;
        tokens(&v, None, now)
    }
    pub fn refresh(
        &self,
        session: &StoredSession,
        now: i64,
        deadline: Instant,
    ) -> Result<Tokens, OidcError> {
        let result = self.call("/token", json!({"clientId":session.client_id,"clientSecret":session.client_secret.expose(),"grantType":"refresh_token","refreshToken":session.refresh_token.expose()}), deadline, false).and_then(|v| tokens(&v, Some(&session.refresh_token), now));
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
    let life = v
        .get("expiresIn")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LIFE)
        .min(MAX_LIFE);
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
    }
    #[test]
    fn error_codes_never_echo_secrets_or_descriptions() {
        for key in ["__type", "code", "Code", "errorCode", "error"] {
            let body =
                json!({key:"FAKE-CLIENT-SECRET", "error_description":"FAKE-ACCESS"}).to_string();
            assert_eq!(error_code(None, body.as_bytes()), "UnrecognizedError");
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
}
