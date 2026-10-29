//! Launch API client.

use serde_json::Value;

/// Convert an untrusted service error code to a locally owned constant.
#[must_use]
pub fn error_code(header: Option<&str>, body: &[u8]) -> &'static str {
    const CODES: &[&str] = &[
        "ConflictException", "InternalServerException", "ResourceNotFoundException",
        "ServiceUnavailableException", "ThrottlingException", "ValidationException",
        "AccessDeniedException", "UnauthorizedException", "UnrecognizedClientException",
        "ExpiredTokenException", "ForbiddenException", "BadRequestException", "NotFoundException",
        "TooManyRequestsException", "LimitExceededException", "ServiceQuotaExceededException",
        "RequestTimeoutException", "InternalFailure", "ServiceUnavailable",
    ];
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let candidate = header.or_else(|| {
        parsed.as_ref().and_then(|value| {
            ["__type", "code", "Code", "errorCode"]
                .iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
        })
    });
    let normalized = candidate.unwrap_or("").rsplit('#').next().unwrap_or("")
        .split(':').next().unwrap_or("");
    CODES.iter().copied().find(|code| *code == normalized).unwrap_or("UnrecognizedError")
}

#[cfg(test)]
#[path = "api/tests_code.rs"]
mod tests_code;
