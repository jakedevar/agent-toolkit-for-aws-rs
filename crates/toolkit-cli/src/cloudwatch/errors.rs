//! Human-readable failure text for AWS API errors (`di_error_translation.py`).
//!
//! The Python module translated `botocore` exceptions. The Rust port works
//! on [`AwsFailure`], the typed description of the same exception classes,
//! so it needs no SDK: the client layer (#4381) maps its SDK errors onto it.
//! The section headers (`Error:`, `ATTEMPTED PARAMETERS:`,
//! `POSSIBLE CAUSES:`, `TROUBLESHOOTING:`) are unchanged.

use toolkit_core::Json;

use super::pytext::{rstrip, safe_block, safe_json, safe_text};

/// The exception classes the legacy translator told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwsFailure {
    /// A service-side `ClientError`.
    Client {
        /// `Error.Code` (absent or empty reads as `ClientError`).
        code: Option<String>,
        /// `Error.Message` (absent or empty falls back to `display`).
        message: Option<String>,
        /// `str(exc)`, used when the response carries no message.
        display: String,
    },
    /// `EndpointConnectionError` with its `str(exc)`.
    EndpointConnection(String),
    /// `ReadTimeoutError` / `ConnectTimeoutError` with its `str(exc)`.
    Timeout(String),
    /// `NoCredentialsError` / `PartialCredentialsError`: the class name and
    /// `str(exc)`.
    Credentials {
        /// `type(exc).__name__`.
        type_name: String,
        /// `str(exc)`.
        message: String,
    },
    /// Anything else, with its `str(exc)`.
    Other(String),
}

/// The attempted-parameters context: ordered `(name, value)` pairs.
pub type Context<'a> = &'a [(&'a str, Json)];

fn is_blank(value: &Json) -> bool {
    matches!(value, Json::Null) || matches!(value, Json::String(text) if text.is_empty())
}

fn format_block(label: &str, context: Context<'_>) -> String {
    let lines: Vec<String> = context
        .iter()
        .filter(|(_, value)| !is_blank(value))
        .map(|(key, value)| format!("- {}: {}", safe_text(key), safe_json(value)))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!("\n{}\n{}\n", safe_block(label), lines.join("\n"))
}

fn format_numbered_section(label: &str, items: &[&str]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let body: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, safe_block(item)))
        .collect();
    format!("\n{}\n{}\n", safe_block(label), body.join("\n"))
}

fn client_error_body(code: Option<&str>, message: Option<&str>, display: &str) -> (String, String) {
    let code = code
        .filter(|text| !text.is_empty())
        .unwrap_or("ClientError");
    let message = message.filter(|text| !text.is_empty()).unwrap_or(display);
    (safe_text(code), safe_text(message))
}

/// The tailored prose a call site supplies for a `ClientError`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Prose<'a> {
    /// Label of the attempted-values block (default `ATTEMPTED PARAMETERS:`).
    pub attempted_label: Option<&'a str>,
    /// `POSSIBLE CAUSES:` bullets.
    pub possible_causes: &'a [&'a str],
    /// `TROUBLESHOOTING:` bullets.
    pub troubleshooting: &'a [&'a str],
    /// A tool-specific footer (for example the location troubleshooting).
    pub trailer: Option<&'a str>,
}

impl Prose<'_> {
    /// True when the call site supplied any tailored prose.
    #[must_use]
    pub fn is_tailored(&self) -> bool {
        !self.possible_causes.is_empty()
            || !self.troubleshooting.is_empty()
            || self.trailer.is_some_and(|trailer| !trailer.is_empty())
    }
}

/// Renders a tool-tailored failure block for a `ClientError`: `Failed to
/// {action}`, an `Error:` line, the attempted-values block and numbered
/// `POSSIBLE CAUSES` / `TROUBLESHOOTING` sections.
#[must_use]
pub fn render_client_error(
    failure: &AwsFailure,
    action: &str,
    attempted: Context<'_>,
    prose: &Prose<'_>,
) -> String {
    let AwsFailure::Client {
        code,
        message,
        display,
    } = failure
    else {
        return translate_aws_error(failure, action, attempted);
    };
    let (code, message) = client_error_body(code.as_deref(), message.as_deref(), display);
    let action = safe_text(action);
    let label = prose.attempted_label.unwrap_or("ATTEMPTED PARAMETERS:");
    let sections = [
        format!("Failed to {action}\n"),
        format!("\nError: {code} - {message}\n"),
        format_block(label, attempted),
        format_numbered_section("POSSIBLE CAUSES:", prose.possible_causes),
        format_numbered_section("TROUBLESHOOTING:", prose.troubleshooting),
    ];
    let joined = sections.concat();
    let body = rstrip(&joined);
    match prose.trailer.filter(|trailer| !trailer.is_empty()) {
        Some(trailer) => format!("{body}\n\n{}", safe_block(trailer)),
        None => body.to_string(),
    }
}

/// Renders a human-readable failure block for an AWS API exception.
///
/// Starts with `Failed to {action}` and includes an `Error:` line, an
/// `ATTEMPTED PARAMETERS:` block when `context` has values, and the
/// `POSSIBLE CAUSES` / `TROUBLESHOOTING` sections tuned to the failure type.
#[must_use]
pub fn translate_aws_error(failure: &AwsFailure, action: &str, context: Context<'_>) -> String {
    let action = safe_text(action);
    let attempted = format_block("ATTEMPTED PARAMETERS:", context);
    match failure {
        AwsFailure::Client {
            code,
            message,
            display,
        } => {
            let (code, message) = client_error_body(code.as_deref(), message.as_deref(), display);
            format!(
                "Failed to {action}\n\n\
                 Error: {code} - {message}\n\
                 {attempted}\
                 \nPOSSIBLE CAUSES:\n\
                 1. Invalid input parameters (validation error)\n\
                 2. Resource not found, already exists, or scoped to a different account\n\
                 3. Insufficient IAM permissions\n\
                 4. Service-side throttling or transient error\n\
                 \nTROUBLESHOOTING:\n\
                 1. Re-read the error message above for the specific failure cause\n\
                 2. Verify service, environment, and instrumentation_type identifiers\n\
                 3. Verify credentials map to an account/region with access\n"
            )
        }
        AwsFailure::EndpointConnection(text) => format!(
            "Failed to {action}\n\n\
             Error: EndpointConnectionError - {}\n\
             {attempted}\
             \nTROUBLESHOOTING:\n\
             1. Check network connectivity to the AWS endpoint\n\
             2. Verify AWS region resolution (AWS_REGION env var or profile)\n",
            safe_text(text)
        ),
        AwsFailure::Timeout(text) => format!(
            "Failed to {action}\n\n\
             Error: TimeoutError - {}\n\
             {attempted}\
             \nTROUBLESHOOTING:\n\
             1. Retry the request \u{2014} the AWS endpoint did not respond within the socket timeout\n\
             2. Check network connectivity\n",
            safe_text(text)
        ),
        AwsFailure::Credentials { type_name, message } => format!(
            "Failed to {action}\n\n\
             Error: {} - {}\n\
             {attempted}\
             \nTROUBLESHOOTING:\n\
             1. Verify AWS credentials: aws configure list\n\
             2. Set AWS_PROFILE or supply credentials via env vars\n",
            safe_text(type_name),
            safe_text(message)
        ),
        AwsFailure::Other(text) => {
            let text = safe_text(text);
            format!("Failed to {action}\n\nUnexpected error: {text}\n{attempted}")
        }
    }
}

/// Renders a gateway failure with the right template (`di_gateway.render_error`).
///
/// Call sites that supply tailored prose and a `ClientError` get
/// [`render_client_error`]; everything else - including every non-client
/// failure - falls through to [`translate_aws_error`], which carries its own
/// canned bullets per failure type.
#[must_use]
pub fn render_failure(
    failure: &AwsFailure,
    action: &str,
    attempted: Context<'_>,
    prose: &Prose<'_>,
) -> String {
    if matches!(failure, AwsFailure::Client { .. }) && prose.is_tailored() {
        render_client_error(failure, action, attempted, prose)
    } else {
        translate_aws_error(failure, action, attempted)
    }
}
