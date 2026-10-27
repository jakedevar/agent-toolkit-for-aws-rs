//! The hook's verdict and its wire form.

use toolkit_core::escape_json_string;

/// Reason shown when a call would fetch a secret value directly. Byte for byte
/// the legacy `DENY_MSG`.
pub const SECRET_FETCH_MESSAGE: &str = "Direct secret fetching is blocked. Use {{resolve:secretsmanager:secret-id:SecretString:key}} with asm-exec instead. Run /aws-secrets-manager for details.";

/// Why a call was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// A detection rule matched: the call would fetch a secret value.
    SecretFetch,
    /// The hook input could not be validated (not UTF-8, not JSON, wrong shape).
    MalformedInput,
    /// The hook input exceeded the size cap.
    InputTooLarge,
    /// Evaluation did not finish inside its time budget.
    DeadlineExceeded,
    /// The evaluator failed unexpectedly (panic, thread failure).
    InternalFailure,
}

impl DenyReason {
    /// Human readable reason placed in `permissionDecisionReason`.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::SecretFetch => SECRET_FETCH_MESSAGE,
            Self::MalformedInput => {
                "Secret-safety hook blocked this call because its input could not be validated."
            }
            Self::InputTooLarge => {
                "Secret-safety hook blocked this call because its input is too large to validate."
            }
            Self::DeadlineExceeded => {
                "Secret-safety hook blocked this call because validation exceeded its time limit."
            }
            Self::InternalFailure => {
                "Secret-safety hook blocked this call because validation failed unexpectedly."
            }
        }
    }
}

/// The verdict for one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// No output, exit status 0.
    Allow,
    /// A deny object on stdout, exit status 0.
    Deny(DenyReason),
}

impl Decision {
    /// Standard output for this verdict. Allow is empty; deny is the
    /// `PreToolUse` object with the spacing of Python's `json.dump`, so the
    /// bytes match the legacy hook.
    #[must_use]
    pub fn render(self) -> String {
        match self {
            Self::Allow => String::new(),
            Self::Deny(reason) => format!(
                "{{\"hookSpecificOutput\": {{\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"deny\", \"permissionDecisionReason\": \"{}\"}}}}",
                escape_json_string(reason.message())
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::{parse_json, Json};

    #[test]
    fn allow_is_empty_output() {
        assert_eq!(Decision::Allow.render(), "");
    }

    #[test]
    fn secret_fetch_deny_is_byte_identical_to_the_legacy_hook() {
        let expected = concat!(
            "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", ",
            "\"permissionDecision\": \"deny\", ",
            "\"permissionDecisionReason\": \"Direct secret fetching is blocked. ",
            "Use {{resolve:secretsmanager:secret-id:SecretString:key}} with asm-exec instead. ",
            "Run /aws-secrets-manager for details.\"}}"
        );
        assert_eq!(Decision::Deny(DenyReason::SecretFetch).render(), expected);
    }

    #[test]
    fn every_deny_reason_renders_valid_json_with_the_deny_protocol() {
        for reason in [
            DenyReason::SecretFetch,
            DenyReason::MalformedInput,
            DenyReason::InputTooLarge,
            DenyReason::DeadlineExceeded,
            DenyReason::InternalFailure,
        ] {
            let rendered = Decision::Deny(reason).render();
            let parsed = parse_json(&rendered).expect("deny output is JSON");
            let output = parsed.get("hookSpecificOutput").expect("wrapper object");
            assert_eq!(
                output.get("hookEventName").and_then(Json::as_str),
                Some("PreToolUse")
            );
            assert_eq!(
                output.get("permissionDecision").and_then(Json::as_str),
                Some("deny")
            );
            assert_eq!(
                output
                    .get("permissionDecisionReason")
                    .and_then(Json::as_str),
                Some(reason.message())
            );
        }
    }
}
