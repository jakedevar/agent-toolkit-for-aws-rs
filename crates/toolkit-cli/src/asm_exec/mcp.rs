//! Backend 2: the AWS MCP endpoint (Streamable HTTP, SigV4-signed).
//!
//! Three JSON-RPC POSTs, as in the Python: `initialize` (its `Mcp-Session-Id`
//! header is kept), `notifications/initialized`, and `tools/call` of
//! `aws___run_script` with a short script that calls
//! `call_boto3('secretsmanager', 'GetSecretValue', ...)` server-side. The
//! `SecretString` comes back inside the `run_script` envelope.
//!
//! Everything goes through the `toolkit-aws` seams: the endpoint is validated
//! by `validate_endpoint` (https, AWS domains only, public DNS answers),
//! requests are signed by an injected [`Signer`] and sent by an injected
//! [`Transport`]. A failure is returned as a one-line reason; it never contains
//! the secret, and the caller scrubs it again before printing.

use std::cell::OnceCell;

use toolkit_aws::env::non_empty;
use toolkit_aws::{
    is_valid_region, validate_endpoint, AwsError, Endpoint, EndpointPolicy, Env, Headers,
    HostResolver, HttpRequest, Method, Secret, Signer, SigningScope, Timeouts, Transport,
    DEFAULT_REGION,
};
use toolkit_core::{parse_json, Json};

use crate::manifests::py_repr_str;

/// Name of the setting that overrides the endpoint.
pub const ENDPOINT_SETTING: &str = "ASM_EXEC_MCP_ENDPOINT";

/// The endpoint used when the setting is absent.
pub const DEFAULT_ENDPOINT: &str = "https://aws-mcp.us-east-1.api.aws/mcp";

/// Protocol version announced in `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Tool the script runs through.
const TOOL_NAME: &str = "aws___run_script";

/// Deepest nesting followed when looking for the `SecretString`.
const MAX_SEARCH_DEPTH: usize = 32;

/// Keys under which `run_script` nests the API response, in search order.
const NESTING_KEYS: &[&str] = &["return_value", "result", "results", "output", "stdout"];

/// Service and region a request to `host` is signed for (`mcp-proxy-for-aws-cli`
/// rules): `service.region.api.aws` gives `(service, region)`; a
/// `bedrock-agentcore` host under `amazonaws.com` gives that service and the
/// region before it; anything else gives the first label and no region (the
/// caller uses `AWS_REGION`, `AWS_DEFAULT_REGION` or `us-east-1`).
#[must_use]
pub fn signing_service_region(host: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = host.split('.').collect();
    match parts.as_slice() {
        [_, .., "bedrock-agentcore", region, "amazonaws", "com"] => {
            ("bedrock-agentcore".to_string(), Some((*region).to_string()))
        }
        [service, region, "api", "aws"] => ((*service).to_string(), Some((*region).to_string())),
        _ => (
            parts.first().copied().unwrap_or("aws-mcp").to_string(),
            None,
        ),
    }
}

/// A validated endpoint and the scope requests to it are signed for.
struct Prepared {
    endpoint: Endpoint,
    scope: SigningScope,
}

/// The MCP resolver.
pub struct McpBackend<'a> {
    endpoint_url: String,
    timeouts: Timeouts,
    timeout_seconds: f64,
    env: &'a dyn Env,
    resolver: &'a dyn HostResolver,
    transport: &'a dyn Transport,
    signer: &'a dyn Signer,
    prepared: OnceCell<Result<Prepared, String>>,
}

impl std::fmt::Debug for McpBackend<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpBackend")
            .field("timeout_seconds", &self.timeout_seconds)
            .finish_non_exhaustive()
    }
}

impl<'a> McpBackend<'a> {
    /// A backend for `endpoint_url` with a per-request timeout.
    #[must_use]
    pub fn new(
        endpoint_url: &str,
        timeouts: Timeouts,
        timeout_seconds: f64,
        env: &'a dyn Env,
        resolver: &'a dyn HostResolver,
        transport: &'a dyn Transport,
        signer: &'a dyn Signer,
    ) -> Self {
        Self {
            endpoint_url: endpoint_url.to_string(),
            timeouts,
            timeout_seconds,
            env,
            resolver,
            transport,
            signer,
            prepared: OnceCell::new(),
        }
    }

    fn prepared(&self) -> Result<&Prepared, String> {
        self.prepared
            .get_or_init(|| self.prepare())
            .as_ref()
            .map_err(Clone::clone)
    }

    fn prepare(&self) -> Result<Prepared, String> {
        let endpoint = validate_endpoint(
            &self.endpoint_url,
            ENDPOINT_SETTING,
            &EndpointPolicy::aws_public(),
            self.resolver,
        )
        .map_err(|error| error.to_string())?;
        let (service, region) = signing_service_region(endpoint.host());
        let region = region
            .or_else(|| non_empty(self.env, "AWS_REGION"))
            .or_else(|| non_empty(self.env, "AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| DEFAULT_REGION.to_string());
        if !is_valid_region(&region) {
            return Err("the signing region is not a valid AWS region name".to_string());
        }
        Ok(Prepared {
            endpoint,
            scope: SigningScope { region, service },
        })
    }

    /// Resolve one secret. `Err` is the one-line cause for `asm-exec: MCP
    /// resolution failed: <cause>`.
    ///
    /// # Errors
    /// The cause, with the same classes as the Python: configuration,
    /// credentials, timeout, unreachable endpoint, HTTP status, JSON-RPC
    /// rejection, a `run_script` error, or a response without a `SecretString`.
    pub fn resolve(
        &self,
        secret_id: &str,
        version_stage: &str,
        region: Option<&str>,
    ) -> Result<Secret, String> {
        let prepared = self.prepared()?;

        let initialize = Json::object(vec![
            ("jsonrpc", Json::string("2.0")),
            ("id", Json::Int(1)),
            ("method", Json::string("initialize")),
            (
                "params",
                Json::object(vec![
                    ("protocolVersion", Json::string(PROTOCOL_VERSION)),
                    (
                        "clientInfo",
                        Json::object(vec![
                            ("name", Json::string("asm-exec")),
                            ("version", Json::string("1.0.0")),
                        ]),
                    ),
                    ("capabilities", Json::object(vec![])),
                ]),
            ),
        ]);
        let (reply, session) = self.post(prepared, &initialize, None)?;
        if let Some(message) = rpc_error(&reply) {
            return Err(format!("RuntimeError: MCP initialize rejected: {message}"));
        }

        let initialized = Json::object(vec![
            ("jsonrpc", Json::string("2.0")),
            ("method", Json::string("notifications/initialized")),
        ]);
        self.post(prepared, &initialized, session.as_deref())?;

        let call = Json::object(vec![
            ("jsonrpc", Json::string("2.0")),
            ("id", Json::Int(2)),
            ("method", Json::string("tools/call")),
            (
                "params",
                Json::object(vec![
                    ("name", Json::string(TOOL_NAME)),
                    (
                        "arguments",
                        Json::object(vec![(
                            "code",
                            Json::string(script(secret_id, version_stage, region)),
                        )]),
                    ),
                ]),
            ),
        ]);
        let (reply, _) = self.post(prepared, &call, session.as_deref())?;
        if let Some(message) = rpc_error(&reply) {
            return Err(format!(
                "RuntimeError: {TOOL_NAME} call rejected: {message}"
            ));
        }
        found_secret(&reply)
    }

    /// One signed POST; returns the parsed reply and its session id.
    fn post(
        &self,
        prepared: &Prepared,
        payload: &Json,
        session: Option<&str>,
    ) -> Result<(Json, Option<String>), String> {
        let mut headers = Headers::new();
        let mut set = |name: &str, value: &str| {
            headers
                .insert(name, value)
                .map_err(|error| error.to_string())
        };
        set("Content-Type", "application/json")?;
        set("Accept", "application/json, text/event-stream")?;
        set("User-Agent", "ASMExecWrapper/1.0.0")?;
        if let Some(id) = session {
            set("Mcp-Session-Id", id)?;
        }
        let mut request = HttpRequest {
            method: Method::Post,
            url: prepared.endpoint.url(),
            headers,
            body: payload.to_compact_string().into_bytes(),
            timeouts: self.timeouts,
        };
        self.signer
            .sign(&mut request, &prepared.scope)
            .map_err(|error| match error {
                AwsError::NoCredentials => {
                    "RuntimeError: no AWS credentials available for MCP signing".to_string()
                }
                other => other.to_string(),
            })?;
        let response = self.transport.send(&request).map_err(|error| match error {
            AwsError::Timeout { message, .. } => format!(
                "timed out after {:?}s -- raise ASM_EXEC_MCP_TIMEOUT if the server-side call \
                 needs longer ({message})",
                self.timeout_seconds
            ),
            AwsError::Connect { message } => format!("cannot reach MCP endpoint: {message}"),
            other => other.to_string(),
        })?;
        if response.status >= 300 {
            return Err(format!(
                "HTTP {} from MCP endpoint ({})",
                response.status,
                http_reason(response.status)
            ));
        }
        let session_out = response.headers.get("mcp-session-id").map(str::to_string);
        let reply = if response.body.is_empty() {
            Json::object(vec![])
        } else {
            let text = response.body_text().ok_or_else(|| {
                "UnicodeDecodeError: the MCP response is not valid UTF-8".to_string()
            })?;
            parse_json(text)
                .map_err(|_| "JSONDecodeError: the MCP response is not valid JSON".to_string())?
        };
        Ok((reply, session_out))
    }
}

/// The server-side script. Values are Python string literals, so quotes and
/// metacharacters in the secret id, stage or region cannot leave their literal;
/// no shell is involved.
fn script(secret_id: &str, version_stage: &str, region: Option<&str>) -> String {
    use std::fmt::Write as _;
    let mut call = format!(
        "service_name='secretsmanager', operation_name='GetSecretValue', \
         params={{'SecretId': {}, 'VersionStage': {}}}",
        py_repr_str(secret_id),
        py_repr_str(version_stage)
    );
    if let Some(region) = region.filter(|region| !region.is_empty()) {
        let _ = write!(call, ", region_name={}", py_repr_str(region));
    }
    format!("result = await call_boto3({call})\nresult")
}

/// Standard reason phrase for the statuses worth naming.
fn http_reason(status: u16) -> &'static str {
    match status {
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// Python truthiness of a JSON value.
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null | Json::Bool(false) => false,
        Json::Int(number) => *number != 0,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(pairs) => !pairs.is_empty(),
        Json::Bool(true) => true,
    }
}

/// A JSON value as the Python's `%s` would show it: strings as is, the rest as
/// compact JSON.
fn display_text(value: &Json) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_compact_string(), str::to_string)
}

/// The JSON-RPC error message of a reply, if it has an `error` object.
fn rpc_error(reply: &Json) -> Option<String> {
    let error = reply.get("error")?;
    if !matches!(error, Json::Object(_)) {
        return None;
    }
    Some(
        error
            .get("message")
            .filter(|message| truthy(message))
            .map_or_else(|| error.to_compact_string(), display_text),
    )
}

/// `SecretString` search of one object: the member itself, else the nesting
/// keys in order (a JSON-text value is parsed first).
fn extract_secret_string(payload: &Json, depth: usize) -> Option<String> {
    if depth > MAX_SEARCH_DEPTH || !matches!(payload, Json::Object(_)) {
        return None;
    }
    if let Some(value) = payload.get("SecretString") {
        return value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string);
    }
    for key in NESTING_KEYS {
        let Some(nested) = payload.get(key) else {
            continue;
        };
        let found = if let Some(text) = nested.as_str() {
            match parse_json(text) {
                Ok(parsed) => extract_secret_string(&parsed, depth.saturating_add(1)),
                Err(_) => continue,
            }
        } else {
            extract_secret_string(nested, depth.saturating_add(1))
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The `tools/call` reply to a secret, or the cause of its absence.
fn found_secret(reply: &Json) -> Result<Secret, String> {
    let result = reply
        .get("result")
        .cloned()
        .unwrap_or_else(|| Json::object(vec![]));
    let mut envelopes: Vec<Json> = Vec::new();
    match result.get("content") {
        Some(Json::Array(items)) => {
            for item in items {
                let is_text = item.get("type").and_then(Json::as_str) == Some("text");
                let parsed = item
                    .get("text")
                    .and_then(Json::as_str)
                    .and_then(|text| parse_json(text).ok());
                if let (true, Some(parsed)) = (is_text, parsed) {
                    envelopes.push(parsed);
                }
            }
        }
        Some(_) => {}
        None if matches!(result, Json::Object(_)) => envelopes.push(result.clone()),
        None => {}
    }
    for envelope in &envelopes {
        if envelope.get("status").and_then(Json::as_str) == Some("error") {
            let cause = ["error", "stderr"]
                .iter()
                .filter_map(|key| envelope.get(key))
                .find(|value| truthy(value))
                .map_or_else(|| "unknown error".to_string(), display_text);
            return Err(format!(
                "RuntimeError: run_script reported an error: {cause}"
            ));
        }
        if let Some(found) = extract_secret_string(envelope, 0) {
            return Ok(Secret::new(found));
        }
    }
    Err(
        "RuntimeError: no SecretString in run_script response (check \
         secretsmanager:GetSecretValue permission and the secret id/stage)"
            .to_string(),
    )
}

#[cfg(test)]
mod tests;
