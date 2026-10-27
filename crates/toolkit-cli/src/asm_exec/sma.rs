//! Backend 1: the AWS Secrets Manager Agent on the loopback interface.
//!
//! `GET <base>/ping` (1 s) decides once per process whether the agent is
//! there; `GET <base>/secretsmanager/get?secretId=..&versionStage=..` (5 s)
//! fetches one secret. Any failure on the way is "not resolved here" and the
//! caller falls through to the next backend, exactly like the Python. The agent
//! is authenticated with the `X-Aws-Parameters-Secrets-Token` header carrying
//! the session token; it is attached only to a request for a loopback URL.

use std::cell::Cell;
use std::time::Duration;

use toolkit_aws::transport::percent_encode;
use toolkit_aws::{Headers, HttpRequest, Method, Secret, Timeouts, Transport};
use toolkit_core::parse_json;

const PING_TIMEOUT: Duration = Duration::from_secs(1);
const GET_TIMEOUT: Duration = Duration::from_secs(5);

/// Header carrying the agent's SSRF token.
const TOKEN_HEADER: &str = "X-Aws-Parameters-Secrets-Token";

/// The agent client. `Debug` shows nothing but whether a token is configured.
pub struct SmaBackend<'a> {
    base_url: String,
    token: Option<Secret>,
    transport: &'a dyn Transport,
    available: Cell<Option<bool>>,
}

impl std::fmt::Debug for SmaBackend<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmaBackend")
            .field("token_configured", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> SmaBackend<'a> {
    /// `base_url` is a validated loopback base (see `LoopbackEndpoint`);
    /// `token` is the session token, empty meaning none.
    #[must_use]
    pub fn new(base_url: String, token: &str, transport: &'a dyn Transport) -> Self {
        Self {
            base_url,
            token: (!token.is_empty()).then(|| Secret::new(token)),
            transport,
            available: Cell::new(None),
        }
    }

    fn timeouts(duration: Duration) -> Timeouts {
        // Both constants are within the accepted range; fall back to the
        // default rather than panic if that ever changes.
        Timeouts::new(duration, duration).unwrap_or_default()
    }

    /// Whether the agent answered `/ping` with a 2xx status; asked once.
    #[must_use]
    pub fn is_available(&self) -> bool {
        if let Some(known) = self.available.get() {
            return known;
        }
        let request = HttpRequest {
            method: Method::Get,
            url: format!("{}/ping", self.base_url),
            headers: Headers::new(),
            body: Vec::new(),
            timeouts: Self::timeouts(PING_TIMEOUT),
        };
        let up = self
            .transport
            .send(&request)
            .is_ok_and(|response| (200..300).contains(&response.status));
        self.available.set(Some(up));
        up
    }

    /// The `SecretString` of `secret_id` at `version_stage`, or `None` when
    /// the agent cannot supply a non-empty string (error status, transport
    /// failure, malformed or non-object body, missing or non-string member).
    #[must_use]
    pub fn get(&self, secret_id: &str, version_stage: &str) -> Option<Secret> {
        let mut headers = Headers::new();
        if let Some(token) = &self.token {
            headers.insert(TOKEN_HEADER, token.expose()).ok()?;
        }
        let request = HttpRequest {
            method: Method::Get,
            url: format!(
                "{}/secretsmanager/get?secretId={}&versionStage={}",
                self.base_url,
                percent_encode(secret_id),
                percent_encode(version_stage)
            ),
            headers,
            body: Vec::new(),
            timeouts: Self::timeouts(GET_TIMEOUT),
        };
        let response = self.transport.send(&request).ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        let json = parse_json(response.body_text()?).ok()?;
        let value = json.get("SecretString")?.as_str()?;
        (!value.is_empty()).then(|| Secret::new(value))
    }
}

#[cfg(test)]
mod tests;
