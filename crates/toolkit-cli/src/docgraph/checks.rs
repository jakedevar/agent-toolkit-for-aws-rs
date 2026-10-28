//! The result type of the review and the text rules for its labels.

use toolkit_aws::redact_text;
use toolkit_core::Json;

use crate::safe_text::safe_path;

/// Status of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Pass,
    Warn,
    Fail,
    Info,
}

impl Status {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Info => "info",
        }
    }
}

/// One finding: the dict `_add` appended in the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Check {
    pub(super) pillar: &'static str,
    pub(super) id: &'static str,
    pub(super) label: String,
    pub(super) status: Status,
    pub(super) detail: String,
}

impl Check {
    pub(super) fn to_json(&self) -> Json {
        Json::object(vec![
            ("pillar", Json::string(self.pillar)),
            ("id", Json::string(self.id)),
            ("label", Json::string(self.label.clone())),
            ("status", Json::string(self.status.as_str())),
            ("detail", Json::string(self.detail.clone())),
        ])
    }
}

/// The ordered findings of a run.
#[derive(Debug, Default)]
pub(super) struct Findings {
    pub(super) checks: Vec<Check>,
}

impl Findings {
    pub(super) fn add(
        &mut self,
        pillar: &'static str,
        id: &'static str,
        label: impl Into<String>,
        status: Status,
        detail: impl Into<String>,
    ) {
        self.checks.push(Check {
            pillar,
            id,
            label: label.into(),
            status,
            detail: detail.into(),
        });
    }

    pub(super) fn has(&self, id: &str) -> bool {
        self.checks.iter().any(|check| check.id == id)
    }
}

/// Text taken from a response or a snapshot, made safe for a label: credential
/// shaped text goes through the shared redactor, then control, bidirectional
/// and invisible characters are escaped.
pub(super) fn clean(text: &str) -> String {
    safe_path(&redact_text(text))
}

/// [`clean`] bounded to the first `count` characters (`text[:count]`): the
/// whole text is redacted first, then cut, then escaped, so a credential that
/// crosses the bound (URL userinfo whose `@host` lies beyond it, an access-key
/// id cut in half) is still recognized (#4908).
pub(super) fn clean_prefix(text: &str, count: usize) -> String {
    safe_path(&first_chars(&redact_text(text), count))
}

/// The first `count` characters (`text[:count]`).
pub(super) fn first_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

/// `status` -> `[PASS]`-style icon of the Markdown report.
pub(super) fn icon(status: &str) -> String {
    match status {
        "pass" => "[PASS]".to_string(),
        "warn" => "[WARN]".to_string(),
        "fail" => "[FAIL]".to_string(),
        "info" => "[INFO]".to_string(),
        other => other.to_string(),
    }
}
