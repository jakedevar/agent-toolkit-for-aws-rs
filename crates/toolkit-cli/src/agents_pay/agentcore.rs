//! AgentCore payments adapter contract tests and implementation.

pub fn error_code(_header: Option<&str>, _body: &str) -> &'static str {
    "UnrecognizedError"
}

pub fn is_retryable(_code: &str) -> bool {
    false
}

pub fn endpoint(_region: &str) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_error_codes_are_allowlisted_and_conflict_is_not_retryable() {
        assert_eq!(error_code(Some("aws#ValidationException:secret"), ""), "ValidationException");
        assert_eq!(error_code(None, r#"{"__type":"aws#ThrottlingException","message":"secret"}"#), "ThrottlingException");
        assert_eq!(error_code(Some("secret"), r#"{"code":"ValidationException"}"#), "UnrecognizedError");
        assert!(!is_retryable("ConflictException"));
        assert!(is_retryable("ThrottlingException"));
    }

    #[test]
    fn aws_endpoint_is_fixed_to_the_service_and_region() {
        assert_eq!(endpoint("us-east-1"), "https://bedrock-agentcore.us-east-1.amazonaws.com");
    }
}
