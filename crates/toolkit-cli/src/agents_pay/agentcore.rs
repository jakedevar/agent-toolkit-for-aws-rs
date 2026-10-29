//! Safe AgentCore error classification and endpoint construction.

use toolkit_aws::{AwsError, Endpoint};
use toolkit_core::parse_json;

const CODES: &[&str] = &[
    "AccessDeniedException",
    "ConflictException",
    "InternalServerException",
    "ResourceNotFoundException",
    "ServiceQuotaExceededException",
    "SubscriptionRequiredException",
    "ThrottlingException",
    "ValidationException",
    "ServiceUnavailableException",
    "UnauthorizedException",
    "UnrecognizedClientException",
    "ExpiredTokenException",
    "ForbiddenException",
    "BadRequestException",
    "NotFoundException",
    "TooManyRequestsException",
    "LimitExceededException",
    "RequestTimeoutException",
    "InternalFailure",
    "ServiceUnavailable",
];

/// Classify a server error without returning any server-supplied text.
#[must_use]
pub fn error_code(header: Option<&str>, body: &str) -> &'static str {
    let parsed = parse_json(body).ok();
    let candidate = header.or_else(|| {
        parsed.as_ref().and_then(|json| {
            ["__type", "code", "Code"]
                .iter()
                .find_map(|key| json.get(key).and_then(|v| v.as_str()))
        })
    });
    let Some(candidate) = candidate else {
        return "UnrecognizedError";
    };
    let candidate = candidate
        .rsplit('#')
        .next()
        .unwrap_or(candidate)
        .split(':')
        .next()
        .unwrap_or(candidate);
    CODES
        .iter()
        .copied()
        .find(|code| *code == candidate)
        .unwrap_or("UnrecognizedError")
}

/// Whether a service error code is eligible for retry. A conflict is final.
#[must_use]
pub fn is_retryable(code: &str) -> bool {
    matches!(
        code,
        "ThrottlingException"
            | "TooManyRequestsException"
            | "RequestTimeoutException"
            | "InternalFailure"
            | "InternalServerException"
            | "ServiceUnavailableException"
            | "ServiceUnavailable"
    )
}

/// Build the standard AgentCore endpoint without reading endpoint overrides.
///
/// # Errors
/// Returns an invalid configuration error for an invalid region.
pub fn endpoint(region: &str) -> Result<String, AwsError> {
    Endpoint::for_service("bedrock-agentcore", region).map(|endpoint| endpoint.url())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_error_codes_are_allowlisted_and_conflict_is_not_retryable() {
        assert_eq!(
            error_code(Some("aws#ValidationException:secret"), ""),
            "ValidationException"
        );
        assert_eq!(
            error_code(
                None,
                r#"{"__type":"aws#ThrottlingException","message":"secret"}"#
            ),
            "ThrottlingException"
        );
        assert_eq!(
            error_code(Some("secret"), r#"{"code":"ValidationException"}"#),
            "UnrecognizedError"
        );
        assert!(!is_retryable("ConflictException"));
        assert!(is_retryable("ThrottlingException"));
    }

    #[test]
    fn aws_endpoint_is_fixed_to_the_service_and_region() {
        assert_eq!(
            endpoint("us-east-1").unwrap(),
            "https://bedrock-agentcore.us-east-1.amazonaws.com"
        );
    }
}
