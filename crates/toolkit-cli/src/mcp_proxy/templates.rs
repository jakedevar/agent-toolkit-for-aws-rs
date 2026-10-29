//! Proxy-generated JSON-RPC messages: fixed templates plus, at most, the
//! client's own id and a bounded tool name. No upstream body, header,
//! credential or error `Display` is ever placed in one, so every control line
//! fits [`MAX_CONTROL_LINE`].
//!
//! For `tools/call`, failures of the call itself (HTTP status, credentials,
//! transport, timeout, no valid response) are a `CallToolResult` with
//! `isError: true`, as upstream's `ToolErrorMiddleware` produces. Failures of
//! the proxy's own bounds and of the session (too many requests, budget,
//! response too large, session expired or refused) are JSON-RPC `-32603`
//! errors for every method (section 4.5).

use toolkit_core::{escape_json_string, Json};

use super::frame::to_line;
use super::limits::MAX_CONTROL_LINE;

/// Escaped bytes of a tool name quoted in a template.
const MAX_TOOL_NAME_ESCAPED: usize = 384;

/// JSON-RPC parse error.
pub(crate) const PARSE_ERROR: i64 = -32700;
/// JSON-RPC invalid request.
pub(crate) const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC internal error.
pub(crate) const INTERNAL_ERROR: i64 = -32603;

const CREDENTIALS_TAIL: &str = "Please refresh your credentials and retry. The proxy will \
automatically use the new credentials on the next request.";

/// Why a request failed. Each maps to one fixed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Signing failed, or the server answered 401/403.
    Credentials,
    /// The server answered with this HTTP status (and no JSON-RPC error).
    Http(u16),
    /// The endpoint could not be reached.
    Connect,
    /// The endpoint stopped sending (idle-read or connect timeout).
    Unresponsive,
    /// The stream ended without a valid response to this request.
    NoResponse,
    /// The request's own deadline passed.
    TimedOut { seconds: u64 },
    /// A message passed its size bound.
    TooLarge,
    /// The R-MEM budget could not be reserved.
    Budget,
    /// More than the concurrency cap.
    TooManyRequests,
    /// The server no longer knows the session (404).
    SessionExpired,
    /// The server sent an invalid session id or protocol version.
    SessionRefused,
}

impl Failure {
    /// Whether, for `tools/call`, this is a tool result rather than a
    /// JSON-RPC error.
    fn is_tool_failure(self) -> bool {
        matches!(
            self,
            Self::Credentials
                | Self::Http(_)
                | Self::Connect
                | Self::Unresponsive
                | Self::NoResponse
                | Self::TimedOut { .. }
        )
    }

    /// The reason inside "Tool call '<name>' failed: <reason>. Please retry."
    fn tool_reason(self) -> String {
        match self {
            Self::Http(status) => http_text(status),
            Self::Connect => "the MCP endpoint could not be reached".to_string(),
            Self::Unresponsive => "the MCP endpoint did not respond in time".to_string(),
            Self::NoResponse => "no valid response from the MCP server".to_string(),
            Self::TimedOut { seconds } => format!("timed out after {seconds} seconds"),
            _ => self.rpc_message(),
        }
    }

    /// The JSON-RPC error message.
    fn rpc_message(self) -> String {
        match self {
            Self::Credentials => {
                format!("AWS credentials are expired or invalid. {CREDENTIALS_TAIL}")
            }
            Self::Http(status) => format!("upstream {}", http_text(status)),
            Self::Connect => "the MCP endpoint could not be reached".to_string(),
            Self::Unresponsive => "the MCP endpoint did not respond in time".to_string(),
            Self::NoResponse => "no valid response from the MCP server".to_string(),
            Self::TimedOut { seconds } => format!("request timed out after {seconds} seconds"),
            Self::TooLarge => "response too large".to_string(),
            Self::Budget => "proxy memory budget exhausted".to_string(),
            Self::TooManyRequests => "too many concurrent requests".to_string(),
            Self::SessionExpired => "MCP session expired; reconnect the MCP server".to_string(),
            Self::SessionRefused => "the MCP server returned an invalid session".to_string(),
        }
    }
}

fn http_text(status: u16) -> String {
    match reason_phrase(status) {
        "" => format!("HTTP {status}"),
        reason => format!("HTTP {status} {reason}"),
    }
}

/// The standard reason phrase for the statuses worth naming.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// `name` cut to whole characters whose escaped form fits the bound.
fn bounded_name(name: &str) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in name.chars() {
        let cost = escape_json_string(c.encode_utf8(&mut [0; 4])).len();
        if used + cost > MAX_TOOL_NAME_ESCAPED {
            break;
        }
        used += cost;
        out.push(c);
    }
    out
}

/// The failure message for request `id`; `tool` is the `tools/call` name.
pub(crate) fn failure(id: &Json, tool: Option<&str>, failure: Failure) -> String {
    match tool {
        Some(name) if failure.is_tool_failure() => {
            let name = bounded_name(name);
            let text = if failure == Failure::Credentials {
                format!("Tool call '{name}' failed due to expired or invalid AWS credentials. {CREDENTIALS_TAIL}")
            } else {
                format!(
                    "Tool call '{name}' failed: {}. Please retry.",
                    failure.tool_reason()
                )
            };
            tool_error(id, &text)
        }
        _ => rpc_error(id, INTERNAL_ERROR, &failure.rpc_message()),
    }
}

/// A JSON-RPC error response.
pub(crate) fn rpc_error(id: &Json, code: i64, message: &str) -> String {
    let line = to_line(&Json::object(vec![
        ("jsonrpc", Json::string("2.0")),
        ("id", id.clone()),
        (
            "error",
            Json::object(vec![
                ("code", Json::Int(code)),
                ("message", Json::string(message)),
            ]),
        ),
    ]));
    debug_assert!(line.len() <= MAX_CONTROL_LINE);
    line
}

/// A `CallToolResult` with `isError: true`.
fn tool_error(id: &Json, text: &str) -> String {
    let line = to_line(&Json::object(vec![
        ("jsonrpc", Json::string("2.0")),
        ("id", id.clone()),
        (
            "result",
            Json::object(vec![
                (
                    "content",
                    Json::Array(vec![Json::object(vec![
                        ("type", Json::string("text")),
                        ("text", Json::string(text)),
                    ])]),
                ),
                ("isError", Json::Bool(true)),
            ]),
        ),
    ]));
    debug_assert!(line.len() <= MAX_CONTROL_LINE);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Failure; 11] = [
        Failure::Credentials,
        Failure::Http(599),
        Failure::Connect,
        Failure::Unresponsive,
        Failure::NoResponse,
        Failure::TimedOut { seconds: 3600 },
        Failure::TooLarge,
        Failure::Budget,
        Failure::TooManyRequests,
        Failure::SessionExpired,
        Failure::SessionRefused,
    ];

    #[test]
    fn the_worst_case_line_fits_the_control_bound() {
        let id = Json::string(format!("{}ab", "\u{1}".repeat(42))); // 252 + 2 + quotes
        assert_eq!(id.to_compact_string().len(), 256);
        let name = "\u{1}".repeat(1000);
        for failure_kind in ALL {
            for tool in [None, Some(name.as_str())] {
                let line = failure(&id, tool, failure_kind);
                assert!(
                    line.len() <= MAX_CONTROL_LINE,
                    "{} bytes: {line}",
                    line.len()
                );
                assert!(toolkit_core::parse_json(&line).is_ok());
            }
        }
    }

    #[test]
    fn tool_failures_are_results_and_proxy_failures_are_errors() {
        let id = Json::Int(1);
        let line = failure(&id, Some("echo"), Failure::Http(503));
        assert_eq!(
            line,
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"Tool call 'echo' failed: HTTP 503 Service Unavailable. Please retry."}],"isError":true}}"#
        );
        let line = failure(&id, Some("echo"), Failure::TooManyRequests);
        assert_eq!(
            line,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"too many concurrent requests"}}"#
        );
        assert_eq!(
            failure(&id, None, Failure::Http(418)),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"upstream HTTP 418"}}"#
        );
    }
}
