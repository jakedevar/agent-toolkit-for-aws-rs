//! Redacted error contract for the AWS boundary.
//!
//! [`AwsError`] is the one error type of this crate. Every message is passed
//! through [`redact_text`] when it is built and again when it is displayed, so
//! a credential that slips into an SDK or HTTP message never reaches stderr.
//! [`AwsError::is_retryable`] drives [`crate::retry`]; [`render_failure`]
//! reproduces the human-readable `Failed to <action>` blocks of the Python
//! `di_error_translation` module; the `From<AwsError> for ToolkitError`
//! conversion maps onto the shared CLI exit-status contract.

use std::fmt;

use toolkit_core::ToolkitError;

use crate::redact::{
    is_secret_name, redact_text, sanitize_path, snippet, write_scrubbed_debug,
    write_scrubbed_display, Scrubbed, REDACTED,
};

/// Longest response message kept in a [`ServiceError`].
pub const MAX_MESSAGE_CHARS: usize = 1024;

/// Error codes AWS services use for throttling (retryable).
const THROTTLING_CODES: &[&str] = &[
    "Throttling",
    "ThrottlingException",
    "ThrottledException",
    "RequestThrottledException",
    "TooManyRequestsException",
    "ProvisionedThroughputExceededException",
    "TransactionInProgressException",
    "RequestLimitExceeded",
    "BandwidthLimitExceeded",
    "LimitExceededException",
    "RequestThrottled",
    "SlowDown",
    "PriorRequestNotComplete",
    "EC2ThrottledException",
];

/// Error codes AWS services use for transient server-side failures (retryable).
const TRANSIENT_CODES: &[&str] = &[
    "RequestTimeout",
    "RequestTimeoutException",
    "InternalError",
    "InternalFailure",
    "ServiceUnavailable",
    "ServiceUnavailableException",
];

/// Which timeout fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPhase {
    /// Establishing the connection.
    Connect,
    /// Waiting for response bytes.
    Read,
}

impl fmt::Display for TimeoutPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect",
            Self::Read => "read",
        })
    }
}

/// An HTTP-level error response from a service.
///
/// The request path is stored sanitized and the message redacted and
/// truncated; the raw response body is never kept. `Debug` and `Display`
/// scrub every field again, so no constructor path can print a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceError {
    method: String,
    path: String,
    status: u16,
    code: Option<String>,
    message: String,
    request_id: Option<String>,
}

impl ServiceError {
    /// Build a service error, sanitizing the path and redacting the message.
    #[must_use]
    pub fn new(
        method: &str,
        path: &str,
        status: u16,
        code: Option<&str>,
        message: &str,
        request_id: Option<&str>,
    ) -> Self {
        Self {
            // Scrub before upper-casing too: some recognizers are case-sensitive.
            method: snippet(&redact_text(method).to_ascii_uppercase(), 16),
            // Scrub first: `<id>` rewriting can remove a long label (such as
            // `x-amz-security-token`) and would leave its value behind.
            path: sanitize_path(&redact_text(path)),
            status,
            code: code.filter(|c| !c.is_empty()).map(|c| snippet(c, 128)),
            message: snippet(message, MAX_MESSAGE_CHARS),
            request_id: request_id
                .filter(|r| !r.is_empty())
                .map(|r| snippet(r, 128)),
        }
    }

    /// HTTP method, upper case.
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Sanitized request path (`<id>` for id-like segments, no query).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Structured error code such as `ThrottlingException`, when present.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Redacted, truncated service message (may be empty).
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Service request id, when the response carried one.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    fn is_throttling(&self) -> bool {
        self.status == 429
            || self
                .code
                .as_deref()
                .is_some_and(|code| THROTTLING_CODES.contains(&code))
    }

    fn is_transient(&self) -> bool {
        matches!(self.status, 500 | 502 | 503 | 504)
            || self
                .code
                .as_deref()
                .is_some_and(|code| TRANSIENT_CODES.contains(&code))
    }
}

impl fmt::Display for ServiceError {
    /// `<METHOD> <sanitized path> failed (<status>)[: <code>]`; like the Python
    /// `ApiError`, the message is not part of the one-line form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_display(f, |f| {
            write!(
                f,
                "{} {} failed ({})",
                Scrubbed(&self.method),
                Scrubbed(&sanitize_path(&self.path)),
                self.status
            )?;
            if let Some(code) = &self.code {
                write!(f, ": {}", Scrubbed(code))?;
            }
            Ok(())
        })
    }
}

impl fmt::Debug for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            f.debug_struct("ServiceError")
                .field("method", &Scrubbed(&self.method))
                .field("path", &Scrubbed(&sanitize_path(&self.path)))
                .field("status", &self.status)
                .field("code", &self.code.as_deref().map(Scrubbed))
                .field("message", &Scrubbed(&self.message))
                .field("request_id", &self.request_id.as_deref().map(Scrubbed))
                .finish()
        })
    }
}

/// The one error type of the AWS boundary.
///
/// The variants carry public strings, so a caller can build one by hand with
/// a raw secret. `Display` and `Debug` therefore scrub every string field
/// through [`redact_text`] again; the constructors scrub once more up front.
#[derive(Clone, PartialEq, Eq)]
pub enum AwsError {
    /// A region, endpoint, profile, timeout or request setting is invalid.
    InvalidConfig {
        /// Setting name, such as `region` or `AWS_ENDPOINT_URL`.
        field: &'static str,
        /// Why it was rejected (never the raw rejected value).
        reason: String,
    },
    /// A credential-bearing command-line flag was supplied.
    CredentialArgument {
        /// The flag name only, without any value.
        flag: String,
    },
    /// The provider chain found no credentials.
    NoCredentials,
    /// A provider found some but not all credential parts.
    PartialCredentials {
        /// Name of the missing part.
        missing: &'static str,
    },
    /// The credentials are past their expiry.
    CredentialsExpired,
    /// A credential provider failed for another reason.
    CredentialProvider {
        /// Provider name.
        provider: &'static str,
        /// Redacted failure message.
        message: String,
    },
    /// The endpoint could not be reached.
    Connect {
        /// Redacted failure message.
        message: String,
    },
    /// A connect or read timeout fired.
    Timeout {
        /// Which timeout.
        phase: TimeoutPhase,
        /// Redacted failure message.
        message: String,
    },
    /// The service returned an HTTP error response.
    Service(Box<ServiceError>),
    /// A paginated walk was malformed or exceeded its limits.
    Pagination {
        /// What went wrong.
        reason: String,
    },
    /// A success response could not be understood (for example invalid JSON).
    InvalidResponse {
        /// What was wrong with the response.
        reason: String,
    },
}

impl AwsError {
    /// An invalid-setting error; `reason` is redacted.
    #[must_use]
    pub fn invalid_config(field: &'static str, reason: impl AsRef<str>) -> Self {
        Self::InvalidConfig {
            field,
            reason: redact_text(reason.as_ref()),
        }
    }

    /// A connection failure; `message` is redacted.
    #[must_use]
    pub fn connect(message: impl AsRef<str>) -> Self {
        Self::Connect {
            message: redact_text(message.as_ref()),
        }
    }

    /// A timeout; `message` is redacted.
    #[must_use]
    pub fn timeout(phase: TimeoutPhase, message: impl AsRef<str>) -> Self {
        Self::Timeout {
            phase,
            message: redact_text(message.as_ref()),
        }
    }

    /// A credential provider failure; `message` is redacted.
    #[must_use]
    pub fn provider(provider: &'static str, message: impl AsRef<str>) -> Self {
        Self::CredentialProvider {
            provider,
            message: redact_text(message.as_ref()),
        }
    }

    /// A service error response (boxed to keep `Result<_, AwsError>` small).
    #[must_use]
    pub fn service(error: ServiceError) -> Self {
        Self::Service(Box::new(error))
    }

    /// A pagination failure; `reason` is redacted.
    #[must_use]
    pub fn pagination(reason: impl AsRef<str>) -> Self {
        Self::Pagination {
            reason: redact_text(reason.as_ref()),
        }
    }

    /// A response that could not be understood; `reason` is redacted.
    #[must_use]
    pub fn invalid_response(reason: impl AsRef<str>) -> Self {
        Self::InvalidResponse {
            reason: redact_text(reason.as_ref()),
        }
    }

    /// Stable machine-readable code (also the `ToolkitError` code).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig { .. } => "aws_invalid_config",
            Self::CredentialArgument { .. } => "aws_credential_argument",
            Self::NoCredentials => "aws_no_credentials",
            Self::PartialCredentials { .. } => "aws_partial_credentials",
            Self::CredentialsExpired => "aws_credentials_expired",
            Self::CredentialProvider { .. } => "aws_credential_provider",
            Self::Connect { .. } => "aws_connect",
            Self::Timeout { .. } => "aws_timeout",
            Self::Service(_) => "aws_service_error",
            Self::Pagination { .. } => "aws_pagination",
            Self::InvalidResponse { .. } => "aws_invalid_response",
        }
    }

    /// Whether the failure is a service throttle.
    #[must_use]
    pub fn is_throttling(&self) -> bool {
        matches!(self, Self::Service(service) if service.is_throttling())
    }

    /// Whether retrying the same request can succeed: throttling, HTTP
    /// 429/500/502/503/504, transient service codes, timeouts and connection
    /// failures. Credential, validation, configuration and pagination errors
    /// are never retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Connect { .. } | Self::Timeout { .. } => true,
            Self::Service(service) => service.is_throttling() || service.is_transient(),
            _ => false,
        }
    }
}

impl fmt::Display for AwsError {
    /// Each field is scrubbed, then the whole line is scrubbed again.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_display(f, |f| self.display_body(f))
    }
}

impl fmt::Debug for AwsError {
    /// Same redaction contract as `Display`, whatever constructor was used.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| self.debug_body(f))
    }
}

impl AwsError {
    fn display_body(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(f, "invalid {}: {}", Scrubbed(field), Scrubbed(reason))
            }
            Self::CredentialArgument { flag } => write!(
                f,
                "{} is not accepted: credentials come only from the standard AWS provider \
                 chain (environment, shared config or profile, SSO, container or instance \
                 role); set AWS_PROFILE to choose a profile",
                snippet(flag, 64)
            ),
            Self::NoCredentials => f.write_str("Unable to locate credentials"),
            Self::PartialCredentials { missing } => {
                write!(
                    f,
                    "Partial credentials found: missing {}",
                    Scrubbed(missing)
                )
            }
            Self::CredentialsExpired => f.write_str("The AWS credentials have expired"),
            Self::CredentialProvider { provider, message } => write!(
                f,
                "credential provider {} failed: {}",
                Scrubbed(provider),
                Scrubbed(message)
            ),
            Self::Connect { message } => {
                write!(
                    f,
                    "could not connect to the endpoint: {}",
                    redact_text(message)
                )
            }
            Self::Timeout { phase, message } => {
                write!(f, "{phase} timeout: {}", redact_text(message))
            }
            Self::Service(service) => fmt::Display::fmt(service, f),
            Self::Pagination { reason } => {
                write!(f, "pagination failed: {}", redact_text(reason))
            }
            Self::InvalidResponse { reason } => {
                write!(f, "invalid response: {}", redact_text(reason))
            }
        }
    }

    fn debug_body(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => f
                .debug_struct("InvalidConfig")
                .field("field", &Scrubbed(field))
                .field("reason", &Scrubbed(reason))
                .finish(),
            Self::CredentialArgument { flag } => f
                .debug_struct("CredentialArgument")
                .field("flag", &Scrubbed(&snippet(flag, 64)))
                .finish(),
            Self::NoCredentials => f.write_str("NoCredentials"),
            Self::PartialCredentials { missing } => f
                .debug_struct("PartialCredentials")
                .field("missing", &Scrubbed(missing))
                .finish(),
            Self::CredentialsExpired => f.write_str("CredentialsExpired"),
            Self::CredentialProvider { provider, message } => f
                .debug_struct("CredentialProvider")
                .field("provider", &Scrubbed(provider))
                .field("message", &Scrubbed(message))
                .finish(),
            Self::Connect { message } => f
                .debug_struct("Connect")
                .field("message", &Scrubbed(message))
                .finish(),
            Self::Timeout { phase, message } => f
                .debug_struct("Timeout")
                .field("phase", phase)
                .field("message", &Scrubbed(message))
                .finish(),
            Self::Service(service) => f.debug_tuple("Service").field(service).finish(),
            Self::Pagination { reason } => f
                .debug_struct("Pagination")
                .field("reason", &Scrubbed(reason))
                .finish(),
            Self::InvalidResponse { reason } => f
                .debug_struct("InvalidResponse")
                .field("reason", &Scrubbed(reason))
                .finish(),
        }
    }
}

impl std::error::Error for AwsError {}

impl From<AwsError> for ToolkitError {
    /// Credential-argument mistakes are usage errors (exit 2); everything else
    /// is a runtime error (exit 1). The message is the redacted `Display`.
    fn from(error: AwsError) -> Self {
        let message = error.to_string();
        if matches!(error, AwsError::CredentialArgument { .. }) {
            Self::usage(error.code(), message)
        } else {
            Self::runtime(error.code(), message)
        }
    }
}

/// Tool-specific wording for [`render_failure_with`].
#[derive(Clone, Copy)]
pub struct Guidance<'a> {
    /// Numbered `POSSIBLE CAUSES:` entries.
    pub possible_causes: &'a [&'a str],
    /// Numbered `TROUBLESHOOTING:` entries.
    pub troubleshooting: &'a [&'a str],
}

impl fmt::Debug for Guidance<'_> {
    /// Every entry is scrubbed, like the rendered block.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            let causes: Vec<Scrubbed<'_>> =
                self.possible_causes.iter().copied().map(Scrubbed).collect();
            let steps: Vec<Scrubbed<'_>> =
                self.troubleshooting.iter().copied().map(Scrubbed).collect();
            f.debug_struct("Guidance")
                .field("possible_causes", &causes)
                .field("troubleshooting", &steps)
                .finish()
        })
    }
}

const DEFAULT_CAUSES: &[&str] = &[
    "Invalid input parameters (validation error)",
    "Resource not found, already exists, or scoped to a different account",
    "Insufficient IAM permissions",
    "Service-side throttling or transient error",
];

const DEFAULT_TROUBLESHOOTING: &[&str] = &[
    "Re-read the error message above for the specific failure cause",
    "Verify the identifiers and parameters passed in the request",
    "Verify credentials map to an account/region with access",
];

/// Render the human-readable failure block for `error` with the default
/// wording. See [`render_failure_with`].
#[must_use]
pub fn render_failure(action: &str, error: &AwsError, attempted: &[(&str, &str)]) -> String {
    render_failure_with(action, error, attempted, None)
}

/// Render the human-readable failure block, the Rust form of the Python
/// `translate_aws_error`: `Failed to <action>`, an `Error:` line, an optional
/// `ATTEMPTED PARAMETERS:` block (empty values skipped, values redacted) and
/// `POSSIBLE CAUSES:` / `TROUBLESHOOTING:` sections matched to the failure.
/// `guidance` replaces the wording used for service errors only.
#[must_use]
pub fn render_failure_with(
    action: &str,
    error: &AwsError,
    attempted: &[(&str, &str)],
    guidance: Option<&Guidance<'_>>,
) -> String {
    let attempted = attempted_block(attempted);
    let head = format!("Failed to {}\n\n", redact_text(action));
    let block = match error {
        AwsError::Service(service) => {
            let code = redact_text(service.code().unwrap_or("ClientError"));
            let message = if service.message().is_empty() {
                error.to_string()
            } else {
                redact_text(service.message())
            };
            let (causes, steps) = guidance.map_or((DEFAULT_CAUSES, DEFAULT_TROUBLESHOOTING), |g| {
                (g.possible_causes, g.troubleshooting)
            });
            format!(
                "{head}Error: {code} - {message}\n{attempted}{}{}",
                numbered("POSSIBLE CAUSES:", causes),
                numbered("TROUBLESHOOTING:", steps)
            )
        }
        AwsError::Connect { .. } => format!(
            "{head}Error: EndpointConnectionError - {error}\n{attempted}{}",
            numbered(
                "TROUBLESHOOTING:",
                &[
                    "Check network connectivity to the AWS endpoint",
                    "Verify AWS region resolution (AWS_REGION env var or profile)",
                ]
            )
        ),
        AwsError::Timeout { .. } => format!(
            "{head}Error: TimeoutError - {error}\n{attempted}{}",
            numbered(
                "TROUBLESHOOTING:",
                &[
                    "Retry the request — the AWS endpoint did not respond within the socket timeout",
                    "Check network connectivity",
                ]
            )
        ),
        AwsError::NoCredentials
        | AwsError::PartialCredentials { .. }
        | AwsError::CredentialsExpired => {
            let name = match error {
                AwsError::NoCredentials => "NoCredentialsError",
                AwsError::PartialCredentials { .. } => "PartialCredentialsError",
                _ => "ExpiredCredentialsError",
            };
            format!(
                "{head}Error: {name} - {error}\n{attempted}{}",
                numbered(
                    "TROUBLESHOOTING:",
                    &[
                        "Verify AWS credentials with: aws configure list",
                        "Set AWS_PROFILE or supply credentials via env vars",
                    ]
                )
            )
        }
        _ => format!("{head}Unexpected error: {error}\n{attempted}"),
    };
    // Every piece is already scrubbed; scrubbing the assembled block makes the
    // whole output stable under a second pass and hides anything a piece
    // boundary might have split.
    redact_text(&block)
}

fn attempted_block(attempted: &[(&str, &str)]) -> String {
    let lines: Vec<String> = attempted
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| {
            let shown = if is_secret_name(key) {
                REDACTED.to_string()
            } else {
                snippet(value, 512)
            };
            format!("- {}: {shown}", snippet(key, 64))
        })
        .collect();
    if lines.is_empty() {
        String::new()
    } else {
        format!("\nATTEMPTED PARAMETERS:\n{}\n", lines.join("\n"))
    }
}

fn numbered(label: &str, items: &[&str]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let body: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, redact_text(item)))
        .collect();
    format!("\n{label}\n{}\n", body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::ADVERSARIAL;
    use toolkit_core::ErrorKind;

    fn service(status: u16, code: Option<&str>) -> AwsError {
        AwsError::service(ServiceError::new(
            "post",
            "/api/launches/3f2504e0-4f89-41d3-9a0c-0305e82c3301?x=1",
            status,
            code,
            "boom",
            Some("req-1"),
        ))
    }

    #[test]
    fn service_error_display_is_method_sanitized_path_status_code() {
        let error = service(400, Some("ValidationException"));
        assert_eq!(
            error.to_string(),
            "POST /api/launches/<id> failed (400): ValidationException"
        );
        assert_eq!(
            service(502, None).to_string(),
            "POST /api/launches/<id> failed (502)"
        );
        let AwsError::Service(inner) = &error else {
            panic!("expected service error");
        };
        assert_eq!(inner.method(), "POST");
        assert_eq!(inner.status(), 400);
        assert_eq!(inner.request_id(), Some("req-1"));
        assert_eq!(inner.message(), "boom");
    }

    #[test]
    fn service_error_redacts_and_truncates_message() {
        let long = format!("token=abc {}", "x".repeat(2000));
        let error = ServiceError::new("GET", "/", 500, None, &long, None);
        assert!(error.message().starts_with("token=[REDACTED] x"));
        assert!(error.message().ends_with('…'));
        assert!(error.message().chars().count() <= MAX_MESSAGE_CHARS + 1);
    }

    #[test]
    fn retryability_matches_the_documented_classes() {
        for retryable in [
            service(429, None),
            service(400, Some("ThrottlingException")),
            service(400, Some("SlowDown")),
            service(500, None),
            service(502, None),
            service(503, None),
            service(504, None),
            service(400, Some("RequestTimeout")),
            AwsError::connect("refused"),
            AwsError::timeout(TimeoutPhase::Read, "slow"),
        ] {
            assert!(retryable.is_retryable(), "{retryable}");
        }
        for fatal in [
            service(400, Some("ValidationException")),
            service(403, Some("AccessDeniedException")),
            service(404, None),
            AwsError::NoCredentials,
            AwsError::CredentialsExpired,
            AwsError::PartialCredentials { missing: "secret" },
            AwsError::invalid_config("region", "bad"),
            AwsError::pagination("loop"),
            AwsError::provider("p", "x"),
        ] {
            assert!(!fatal.is_retryable(), "{fatal}");
        }
        assert!(service(400, Some("Throttling")).is_throttling());
        assert!(!service(500, None).is_throttling());
        assert!(!AwsError::NoCredentials.is_throttling());
    }

    #[test]
    fn display_redacts_even_hand_built_variants() {
        let error = AwsError::Connect {
            message: "failed with token=abc123".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "could not connect to the endpoint: failed with token=[REDACTED]"
        );
        let error = AwsError::CredentialProvider {
            provider: "env",
            message: "password: hunter2".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "credential provider env failed: password: [REDACTED]"
        );
        let built = AwsError::provider("env", "AKIAIOSFODNN7EXAMPLE rejected");
        assert_eq!(
            built.to_string(),
            "credential provider env failed: [REDACTED] rejected"
        );
    }

    /// Synthetic hostile messages: (text, marker that must never appear).
    const HOSTILE: &[(&str, &str)] = &[
        (
            "dial https://FAKE-USER:FAKE-URL-PASSWORD@logs.amazonaws.com failed",
            "FAKE-URL-PASSWORD",
        ),
        (r#"{"password":"aa\"FAKE-TAIL-LEAK"}"#, "FAKE-TAIL-LEAK"),
        (
            "{\"password\":\n\"FAKE-JSON-PASSWORD\"}",
            "FAKE-JSON-PASSWORD",
        ),
        ("token=FAKE-TOKEN-VALUE", "FAKE-TOKEN-VALUE"),
    ];

    /// Every variant that carries a raw string, built by hand (no constructor).
    fn hand_built(message: &str) -> Vec<AwsError> {
        vec![
            AwsError::InvalidConfig {
                field: "region",
                reason: message.to_string(),
            },
            AwsError::CredentialArgument {
                flag: message.to_string(),
            },
            AwsError::CredentialProvider {
                provider: "env",
                message: message.to_string(),
            },
            AwsError::Connect {
                message: message.to_string(),
            },
            AwsError::Timeout {
                phase: TimeoutPhase::Read,
                message: message.to_string(),
            },
            AwsError::Pagination {
                reason: message.to_string(),
            },
            AwsError::InvalidResponse {
                reason: message.to_string(),
            },
        ]
    }

    #[test]
    fn hostile_messages_never_reach_display_conversion_or_failure_blocks() {
        for &(text, leak) in HOSTILE {
            for error in hand_built(text).into_iter().chain([
                AwsError::connect(text),
                AwsError::provider("env", text),
                AwsError::timeout(TimeoutPhase::Connect, text),
                AwsError::invalid_config("endpoint", text),
                AwsError::pagination(text),
                AwsError::invalid_response(text),
            ]) {
                let shown = error.to_string();
                assert!(!shown.contains(leak), "{error:?}: {shown}");
                let converted: ToolkitError = error.clone().into();
                assert!(
                    !converted.message().contains(leak),
                    "{}",
                    converted.message()
                );
                let block = render_failure("do it", &error, &[("p", text)]);
                assert!(!block.contains(leak), "{block}");
                // The scrubbed text is stable under a second pass (a flag is
                // truncated to 64 characters, which can cut a redaction short).
                if !matches!(error, AwsError::CredentialArgument { .. }) {
                    assert_eq!(redact_text(&shown), shown);
                    assert_eq!(redact_text(converted.message()), converted.message());
                }
            }
        }
    }

    #[test]
    fn connect_message_with_url_userinfo_is_stripped_in_every_path() {
        let message = "proxy https://FAKE-USER:FAKE-URL-PASSWORD@logs.amazonaws.com refused";
        let error = AwsError::connect(message);
        let expected =
            "could not connect to the endpoint: proxy https://[REDACTED]@logs.amazonaws.com refused";
        assert_eq!(error.to_string(), expected);
        let converted: ToolkitError = error.clone().into();
        assert_eq!(converted.message(), expected);
        let block = render_failure("list", &error, &[]);
        assert!(block.contains("https://[REDACTED]@logs.amazonaws.com"));
        assert!(!block.contains("FAKE-URL-PASSWORD") && !block.contains("FAKE-USER"));
        let hand = AwsError::Connect {
            message: message.to_string(),
        };
        assert_eq!(hand.to_string(), expected);
    }

    #[test]
    fn non_json_service_bodies_are_scrubbed_in_message_display_and_block() {
        for &(body, leak) in HOSTILE {
            let service = ServiceError::new("post", "/x", 500, Some("Oops"), body, Some("r-1"));
            assert!(!service.message().contains(leak), "{}", service.message());
            let error = AwsError::service(service);
            assert!(!error.to_string().contains(leak));
            let block = render_failure("act", &error, &[]);
            assert!(!block.contains(leak), "{block}");
            assert!(!format!("{error:?}").contains(leak));
            // Message is already scrubbed: a second pass changes nothing.
            let AwsError::Service(inner) = &error else {
                panic!("expected service error");
            };
            assert_eq!(redact_text(inner.message()), inner.message());
        }
        let url = ServiceError::new(
            "GET",
            "/x",
            502,
            None,
            "bad gateway via http://FAKE-USER:FAKE-URL-PASSWORD@10.0.0.1:8080/",
            None,
        );
        assert_eq!(
            url.message(),
            "bad gateway via http://[REDACTED]@10.0.0.1:8080/"
        );
    }

    #[test]
    fn debug_never_shows_raw_secrets_for_direct_variants() {
        for &(text, leak) in HOSTILE {
            for error in hand_built(text) {
                let debug = format!("{error:?}");
                let pretty = format!("{error:#?}");
                assert!(!debug.contains(leak), "{debug}");
                assert!(!pretty.contains(leak), "{pretty}");
            }
        }
        let error = AwsError::Connect {
            message: "token=FAKE-TOKEN-VALUE".into(),
        };
        assert_eq!(
            format!("{error:?}"),
            r#"Connect { message: "token=[REDACTED]" }"#
        );
        assert_eq!(format!("{:?}", AwsError::NoCredentials), "NoCredentials");
    }

    #[test]
    fn debug_of_service_errors_is_scrubbed_even_through_a_box() {
        let error = AwsError::service(ServiceError::new(
            "POST",
            "/api/launches/3f2504e0-4f89-41d3-9a0c-0305e82c3301?token=FAKE-QUERY-TOKEN",
            400,
            Some("password=FAKE-CODE-VALUE"),
            "token=FAKE-TOKEN-VALUE",
            Some("secret=FAKE-REQUEST-ID"),
        ));
        let debug = format!("{error:?}");
        for leak in [
            "FAKE-QUERY-TOKEN",
            "FAKE-CODE-VALUE",
            "FAKE-TOKEN-VALUE",
            "FAKE-REQUEST-ID",
            "3f2504e0",
        ] {
            assert!(!debug.contains(leak), "{debug}");
        }
        assert!(debug.contains("/api/launches/<id>"));
    }

    #[test]
    fn guidance_text_is_scrubbed_too() {
        let guidance = Guidance {
            possible_causes: &["token=FAKE-CAUSE-VALUE"],
            troubleshooting: &["password: FAKE-STEP-VALUE"],
        };
        let text = render_failure_with("act", &service(400, Some("X")), &[], Some(&guidance));
        assert!(!text.contains("FAKE-"), "{text}");
    }

    #[test]
    fn codes_are_stable() {
        let codes = [
            (
                AwsError::invalid_config("region", "x"),
                "aws_invalid_config",
            ),
            (
                AwsError::CredentialArgument { flag: "--x".into() },
                "aws_credential_argument",
            ),
            (AwsError::NoCredentials, "aws_no_credentials"),
            (
                AwsError::PartialCredentials { missing: "m" },
                "aws_partial_credentials",
            ),
            (AwsError::CredentialsExpired, "aws_credentials_expired"),
            (AwsError::provider("p", "m"), "aws_credential_provider"),
            (AwsError::connect("m"), "aws_connect"),
            (AwsError::timeout(TimeoutPhase::Connect, "m"), "aws_timeout"),
            (service(400, None), "aws_service_error"),
            (AwsError::pagination("m"), "aws_pagination"),
            (AwsError::invalid_response("m"), "aws_invalid_response"),
        ];
        for (error, code) in codes {
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn toolkit_error_conversion_maps_exit_classes() {
        let usage: ToolkitError = AwsError::CredentialArgument {
            flag: "--secret-access-key".to_string(),
        }
        .into();
        assert_eq!(usage.kind(), ErrorKind::Usage);
        assert_eq!(usage.exit_code(), 2);
        assert_eq!(usage.code(), "aws_credential_argument");
        assert!(usage
            .message()
            .starts_with("--secret-access-key is not accepted"));

        let runtime: ToolkitError = service(500, None).into();
        assert_eq!(runtime.kind(), ErrorKind::Runtime);
        assert_eq!(runtime.exit_code(), 1);
        assert_eq!(runtime.code(), "aws_service_error");
    }

    #[test]
    fn service_failure_block_matches_the_python_layout() {
        let error = AwsError::service(ServiceError::new(
            "POST",
            "/x",
            400,
            Some("ValidationException"),
            "bad input",
            None,
        ));
        let text = render_failure(
            "create BREAKPOINT instrumentation",
            &error,
            &[
                ("service", "checkout"),
                ("empty", ""),
                ("region", "us-east-1"),
            ],
        );
        let expected = "\
Failed to create BREAKPOINT instrumentation

Error: ValidationException - bad input

ATTEMPTED PARAMETERS:
- service: checkout
- region: us-east-1

POSSIBLE CAUSES:
1. Invalid input parameters (validation error)
2. Resource not found, already exists, or scoped to a different account
3. Insufficient IAM permissions
4. Service-side throttling or transient error

TROUBLESHOOTING:
1. Re-read the error message above for the specific failure cause
2. Verify the identifiers and parameters passed in the request
3. Verify credentials map to an account/region with access
";
        assert_eq!(text, expected);
    }

    #[test]
    fn custom_guidance_replaces_service_wording_only() {
        let error = service(404, Some("ResourceNotFoundException"));
        let guidance = Guidance {
            possible_causes: &["gone"],
            troubleshooting: &["look again", "ask"],
        };
        let text = render_failure_with("get thing", &error, &[], Some(&guidance));
        assert_eq!(
            text,
            "Failed to get thing\n\nError: ResourceNotFoundException - boom\n\
             \nPOSSIBLE CAUSES:\n1. gone\n\nTROUBLESHOOTING:\n1. look again\n2. ask\n"
        );
        let connect = render_failure_with(
            "get thing",
            &AwsError::connect("down"),
            &[],
            Some(&guidance),
        );
        assert!(connect.contains("EndpointConnectionError"));
        assert!(!connect.contains("gone"));
    }

    #[test]
    fn transport_and_credential_blocks_have_their_own_troubleshooting() {
        let connect = render_failure("list", &AwsError::connect("refused"), &[]);
        assert_eq!(
            connect,
            "Failed to list\n\nError: EndpointConnectionError - could not connect to the endpoint: refused\n\
             \nTROUBLESHOOTING:\n1. Check network connectivity to the AWS endpoint\n\
             2. Verify AWS region resolution (AWS_REGION env var or profile)\n"
        );
        let timeout = render_failure("list", &AwsError::timeout(TimeoutPhase::Read, "slow"), &[]);
        assert!(timeout.contains("Error: TimeoutError - read timeout: slow"));
        assert!(timeout.contains("1. Retry the request"));
        let none = render_failure("list", &AwsError::NoCredentials, &[("a", "b")]);
        assert!(none.contains("Error: NoCredentialsError - Unable to locate credentials"));
        assert!(none.contains("ATTEMPTED PARAMETERS:\n- a: b\n"));
        assert!(none.contains("1. Verify AWS credentials with: aws configure list"));
        let partial = render_failure(
            "list",
            &AwsError::PartialCredentials {
                missing: "secret_access_key",
            },
            &[],
        );
        assert!(partial.contains("PartialCredentialsError"));
        let expired = render_failure("list", &AwsError::CredentialsExpired, &[]);
        assert!(expired.contains("ExpiredCredentialsError"));
    }

    #[test]
    fn unexpected_errors_and_redaction_in_failure_blocks() {
        let text = render_failure(
            "paginate",
            &AwsError::pagination("repeated token=abc"),
            &[("password", "hunter2")],
        );
        assert_eq!(
            text,
            "Failed to paginate\n\nUnexpected error: pagination failed: repeated token=[REDACTED]\n\
             \nATTEMPTED PARAMETERS:\n- password: [REDACTED]\n"
        );
    }

    /// Every error built from `text`: hand-built variants, constructors and a
    /// service error carrying `text` in every string field.
    fn every_error(text: &str) -> Vec<AwsError> {
        let mut errors = hand_built(text);
        errors.extend([
            AwsError::connect(text),
            AwsError::provider("env", text),
            AwsError::timeout(TimeoutPhase::Read, text),
            AwsError::invalid_config("endpoint", text),
            AwsError::pagination(text),
            AwsError::invalid_response(text),
            AwsError::service(ServiceError::new(
                text,
                text,
                500,
                Some(text),
                text,
                Some(text),
            )),
        ]);
        errors
    }

    #[test]
    fn adversarial_inputs_never_leak_through_any_error_path() {
        for &(text, _, leaks) in ADVERSARIAL {
            let causes = [text];
            let guidance = Guidance {
                possible_causes: &causes,
                troubleshooting: &causes,
            };
            for error in every_error(text) {
                let converted: ToolkitError = error.clone().into();
                let shown = [
                    error.to_string(),
                    format!("{error:?}"),
                    format!("{error:#?}"),
                    converted.message().to_string(),
                    render_failure(text, &error, &[("p", text), ("token", text)]),
                    render_failure_with(text, &error, &[("p", text)], Some(&guidance)),
                ];
                for out in &shown {
                    for leak in leaks {
                        assert!(!out.contains(leak), "{leak:?} leaked from {text:?}: {out}");
                    }
                    assert_eq!(&redact_text(out), out, "second pass changed {text:?}");
                }
            }
        }
    }

    #[test]
    fn adversarial_service_error_fields_are_scrubbed_at_construction() {
        for &(text, _, leaks) in ADVERSARIAL {
            let service = ServiceError::new("GET", "/x", 400, Some(text), text, Some(text));
            for field in [
                service.message(),
                service.code().unwrap_or_default(),
                service.request_id().unwrap_or_default(),
            ] {
                for leak in leaks {
                    assert!(
                        !field.contains(leak),
                        "{leak:?} leaked from {text:?}: {field}"
                    );
                }
                assert_eq!(redact_text(field), field, "unstable field for {text:?}");
            }
        }
    }

    #[test]
    fn reviewer_inputs_have_exact_display_text() {
        let error = AwsError::connect("dial https://u:FAKE'PW@logs.amazonaws.com/ failed");
        assert_eq!(
            error.to_string(),
            "could not connect to the endpoint: dial https://[REDACTED]@logs.amazonaws.com/ failed"
        );
        let error = AwsError::Pagination {
            reason: r#"{"password":"aa"tail" LEAK-REST"}"#.to_string(),
        };
        assert_eq!(
            error.to_string(),
            r#"pagination failed: {"password":"[REDACTED]"#
        );
        for (raw, shown) in [
            ("Bearer  FAKE-TWO-SPACE-TOKEN", "Bearer  [REDACTED]"),
            ("Basic  RkFLRTpNVUxUSVNQQUNF", "Basic  [REDACTED]"),
        ] {
            let error = AwsError::Connect {
                message: raw.to_string(),
            };
            assert_eq!(
                error.to_string(),
                format!("could not connect to the endpoint: {shown}")
            );
            assert_eq!(
                format!("{error:?}"),
                format!("Connect {{ message: \"{shown}\" }}")
            );
        }
    }

    #[test]
    fn overlapping_and_suffixed_label_inputs_have_exact_text() {
        // Re-review #4467 of 6ba2ac90: #4515 (overlap) and #4516 (label
        // inside a longer key), through constructor and hand-built variants.
        for (raw, shown) in [
            (
                "token=Bearer FAKE-OVERLAP-VALUE",
                "token=[REDACTED] [REDACTED]",
            ),
            (
                "password=Basic RkFLRTpPVkVSTEFQ",
                "password=[REDACTED] [REDACTED]",
            ),
            (
                "password_backup=FAKE-SUFFIX40 rest",
                "password_backup=[REDACTED] rest",
            ),
            ("my_Password_Old: FAKE-MID41", "my_Password_Old: [REDACTED]"),
        ] {
            let hand_built = AwsError::Connect {
                message: raw.to_string(),
            };
            for error in [AwsError::connect(raw), hand_built] {
                assert_eq!(
                    error.to_string(),
                    format!("could not connect to the endpoint: {shown}")
                );
                assert_eq!(
                    format!("{error:?}"),
                    format!("Connect {{ message: \"{shown}\" }}")
                );
                let converted: ToolkitError = error.into();
                assert!(converted.message().contains(shown), "{raw:?}");
            }
        }
    }

    #[test]
    fn guidance_debug_is_scrubbed() {
        let guidance = Guidance {
            possible_causes: &[
                "token=FAKE-CAUSE-VALUE",
                "see https://FAKEU:FAKEP@h.example/",
            ],
            troubleshooting: &["Bearer \t FAKE-STEP-BEARER"],
        };
        for shown in [format!("{guidance:?}"), format!("{guidance:#?}")] {
            assert!(!shown.contains("FAKE-"), "{shown}");
            assert!(
                !shown.contains("FAKEU") && !shown.contains("FAKEP"),
                "{shown}"
            );
            assert!(shown.contains("possible_causes"), "{shown}");
            assert_eq!(redact_text(&shown), shown);
        }
    }
}
