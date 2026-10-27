//! Shared error contract for every `toolkit` command.
//!
//! A [`ToolkitError`] carries a [`ErrorKind`] (which fixes the process exit
//! status), a stable machine-readable `code` and a human message. Domain
//! errors convert into it with `From`, so command handlers return one error
//! type and the CLI maps it to stderr text and an exit code in one place.

use std::fmt;

use crate::json::Json;
use crate::paths::PathError;
use crate::schema::Violation;
use crate::skills::SkillsError;

/// Broad error class; decides the process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The command line was wrong. Exit status 2.
    Usage,
    /// The command was valid but could not be completed. Exit status 1.
    Runtime,
}

impl ErrorKind {
    /// Process exit status for this class of error.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::Runtime => 1,
        }
    }

    /// Stable lowercase label used in structured output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Runtime => "runtime",
        }
    }
}

/// The one error type returned by command handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolkitError {
    kind: ErrorKind,
    code: &'static str,
    message: String,
    exit_override: Option<u8>,
}

impl ToolkitError {
    /// A command-line (usage) error, exit status 2.
    #[must_use]
    pub fn usage(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Usage,
            code,
            message: message.into(),
            exit_override: None,
        }
    }

    /// A runtime error, exit status 1.
    #[must_use]
    pub fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Runtime,
            code,
            message: message.into(),
            exit_override: None,
        }
    }

    /// Replaces the process exit status for a command that must keep a
    /// published non-default status (for example a legacy status 4 for a
    /// refused deploy). `0` is ignored: an error never exits successfully.
    #[must_use]
    pub fn with_exit_code(mut self, status: u8) -> Self {
        if status != 0 {
            self.exit_override = Some(status);
        }
        self
    }

    /// The error class.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Stable machine-readable code such as `root_not_found`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Human-readable message without the `error: ` prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Process exit status.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.exit_override.unwrap_or_else(|| self.kind.exit_code())
    }

    /// Structured form: `{"error": {"kind", "code", "message"}}`.
    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::Object(vec![(
            "error".to_string(),
            Json::Object(vec![
                ("kind".to_string(), Json::string(self.kind.label())),
                ("code".to_string(), Json::string(self.code)),
                ("message".to_string(), Json::string(self.message.as_str())),
            ]),
        )])
    }
}

impl fmt::Display for ToolkitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolkitError {}

/// Failure to parse a JSON or YAML document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// `"JSON"` or `"YAML"`.
    pub format: &'static str,
    /// 1-based line of the problem.
    pub line: usize,
    /// 1-based column, or 0 when the parser does not report one.
    pub column: usize,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.column == 0 {
            write!(
                f,
                "{} parse error at line {}: {}",
                self.format, self.line, self.message
            )
        } else {
            write!(
                f,
                "{} parse error at line {}, column {}: {}",
                self.format, self.line, self.column, self.message
            )
        }
    }
}

impl std::error::Error for ParseError {}

impl From<ParseError> for ToolkitError {
    fn from(error: ParseError) -> Self {
        Self::runtime("parse_error", error.to_string())
    }
}

impl From<SkillsError> for ToolkitError {
    fn from(error: SkillsError) -> Self {
        let code = match &error {
            SkillsError::RootNotFound(_) => "root_not_found",
            SkillsError::RootNotDirectory(_) => "root_not_directory",
            SkillsError::Io(..) => "io_error",
            SkillsError::SkillFile(_) => "skill_file_unreadable",
        };
        Self::runtime(code, error.to_string())
    }
}

impl From<PathError> for ToolkitError {
    fn from(error: PathError) -> Self {
        match &error {
            PathError::Invalid(_) => Self::usage("path_invalid", error.to_string()),
            PathError::NotFound(_) => Self::runtime("path_not_found", error.to_string()),
            PathError::Symlink(_) => Self::runtime("path_symlink", error.to_string()),
            PathError::NotRegularFile(_) => Self::runtime("path_not_regular", error.to_string()),
            PathError::TooLarge { .. } => Self::runtime("path_too_large", error.to_string()),
            PathError::NotUtf8(_) => Self::runtime("path_not_utf8", error.to_string()),
            PathError::Changed(_) => Self::runtime("path_changed", error.to_string()),
            PathError::Io(..) | PathError::WriteFailed(..) => {
                Self::runtime("io_error", error.to_string())
            }
        }
    }
}

impl From<Vec<Violation>> for ToolkitError {
    fn from(violations: Vec<Violation>) -> Self {
        let details: Vec<String> = violations.iter().map(ToString::to_string).collect();
        Self::runtime(
            "schema_violation",
            format!("schema validation failed: {}", details.join("; ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse_json;
    use std::path::PathBuf;

    #[test]
    fn exit_codes_follow_the_documented_contract() {
        assert_eq!(ErrorKind::Usage.exit_code(), 2);
        assert_eq!(ErrorKind::Runtime.exit_code(), 1);
        assert_eq!(ToolkitError::usage("u", "bad").exit_code(), 2);
        assert_eq!(ToolkitError::runtime("r", "bad").exit_code(), 1);
    }

    #[test]
    fn exit_override_replaces_the_status_but_never_with_zero() {
        let error = ToolkitError::runtime("r", "bad").with_exit_code(4);
        assert_eq!(error.exit_code(), 4);
        assert_eq!(error.kind(), ErrorKind::Runtime);
        let zero = ToolkitError::runtime("r", "bad").with_exit_code(0);
        assert_eq!(zero.exit_code(), 1, "an error never exits with status 0");
        let usage = ToolkitError::usage("u", "bad").with_exit_code(3);
        assert_eq!(usage.exit_code(), 3);
    }

    #[test]
    fn display_is_the_bare_message() {
        let error = ToolkitError::runtime("code", "something broke");
        assert_eq!(error.to_string(), "something broke");
        assert_eq!(error.code(), "code");
        assert_eq!(error.message(), "something broke");
        assert_eq!(error.kind(), ErrorKind::Runtime);
    }

    #[test]
    fn structured_form_is_stable_and_parseable() {
        let error = ToolkitError::usage("invalid_arguments", "say \"hi\"");
        let text = error.to_json().to_compact_string();
        assert_eq!(
            text,
            "{\"error\":{\"kind\":\"usage\",\"code\":\"invalid_arguments\",\"message\":\"say \\\"hi\\\"\"}}"
        );
        assert_eq!(parse_json(&text).expect("valid json"), error.to_json());
    }

    #[test]
    fn skills_errors_map_to_distinct_runtime_codes() {
        let missing = ToolkitError::from(SkillsError::RootNotFound(PathBuf::from("x")));
        assert_eq!(missing.code(), "root_not_found");
        assert_eq!(missing.exit_code(), 1);
        assert!(missing.message().contains("root does not exist"));
        let file = ToolkitError::from(SkillsError::RootNotDirectory(PathBuf::from("x")));
        assert_eq!(file.code(), "root_not_directory");
        assert!(file.message().contains("root is not a directory"));
    }

    #[test]
    fn path_errors_split_usage_from_runtime() {
        let invalid = ToolkitError::from(PathError::Invalid("nope".to_string()));
        assert_eq!(invalid.kind(), ErrorKind::Usage);
        assert_eq!(invalid.code(), "path_invalid");
        let symlink = ToolkitError::from(PathError::Symlink(PathBuf::from("link")));
        assert_eq!(symlink.kind(), ErrorKind::Runtime);
        assert_eq!(symlink.code(), "path_symlink");
    }

    #[test]
    fn parse_errors_report_position() {
        let with_column = ParseError {
            format: "JSON",
            line: 3,
            column: 7,
            message: "bad".to_string(),
        };
        assert_eq!(
            with_column.to_string(),
            "JSON parse error at line 3, column 7: bad"
        );
        let without_column = ParseError {
            format: "YAML",
            line: 2,
            column: 0,
            message: "bad".to_string(),
        };
        assert_eq!(
            without_column.to_string(),
            "YAML parse error at line 2: bad"
        );
        let converted = ToolkitError::from(without_column);
        assert_eq!(converted.code(), "parse_error");
        assert_eq!(converted.exit_code(), 1);
    }

    #[test]
    fn schema_violations_join_into_one_message() {
        let error = ToolkitError::from(vec![
            Violation {
                path: "$.a".to_string(),
                message: "missing".to_string(),
            },
            Violation {
                path: "$.b".to_string(),
                message: "wrong".to_string(),
            },
        ]);
        assert_eq!(error.code(), "schema_violation");
        assert_eq!(
            error.message(),
            "schema validation failed: $.a: missing; $.b: wrong"
        );
    }
}
