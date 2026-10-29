//! The result type of the review and its report icons. Response and caller
//! text is shown through the shared `crate::safe_text` helpers (redact, bound,
//! escape; #4907, #4908, #4941).

use toolkit_core::Json;

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
