//! Launch half of R-API-CODE: untrusted error codes never become diagnostics.
use super::error_code;

#[test]
fn t_code_secrets_in_header_and_body_are_unrecognized() {
    for secret in ["FAKE-ACCESS-TOKEN", "FAKE-REFRESH-TOKEN", "FAKE-CLIENT-SECRET", "FAKE-VERIFIER", "FAKE-CODE", "FAKE-STATE"] {
        assert_eq!(error_code(Some(secret), b"{}"), "UnrecognizedError");
        for key in ["__type", "code", "Code", "errorCode"] {
            let body = serde_json::json!({key: secret}).to_string();
            assert_eq!(error_code(None, body.as_bytes()), "UnrecognizedError");
        }
    }
}

#[test]
fn t_code_allowlisted_decorations_and_header_precedence() {
    assert_eq!(error_code(Some("com.amazon.launch#ResourceNotFoundException"), b"{}"), "ResourceNotFoundException");
    assert_eq!(error_code(Some("ResourceNotFoundException:http://internal/"), b"{}"), "ResourceNotFoundException");
    assert_eq!(error_code(Some("unknown"), br#"{"code":"ValidationException"}"#), "UnrecognizedError");
    for code in ["ConflictException", "InternalServerException", "ResourceNotFoundException", "ServiceUnavailableException", "ThrottlingException", "ValidationException", "AccessDeniedException", "UnauthorizedException", "UnrecognizedClientException", "ExpiredTokenException", "ForbiddenException", "BadRequestException", "NotFoundException", "TooManyRequestsException", "LimitExceededException", "ServiceQuotaExceededException", "RequestTimeoutException", "InternalFailure", "ServiceUnavailable"] {
        assert_eq!(error_code(Some(code), b"{}"), code);
    }
}

#[test]
fn t_code_long_prefix_and_unrelated_fields_are_never_echoed() {
    let code = format!("{}FAKE-ACCESS-TOKEN", "ValidationException".repeat(7));
    assert_eq!(error_code(Some(&code), b"{}"), "UnrecognizedError");
    assert_eq!(error_code(None, br#"{"message":"FAKE-ACCESS-TOKEN","requestId":"FAKE-ACCESS-TOKEN"}"#), "UnrecognizedError");
}
