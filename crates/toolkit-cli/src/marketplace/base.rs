//! Shared plumbing of the `marketplace` slice: the failure type, the log
//! sink, the Lambda environment (`--config`) and the text rules.

use std::fmt;

use toolkit_aws::recorded::object_members;
use toolkit_aws::{redact_text, AwsError};
use toolkit_core::{escape_untrusted, neutralize_line, Json};

use super::client::AwsClient;
use super::core::EmfMetric;
use crate::cloudwatch::timefmt::UtcTime;

/// Characters of caller text a diagnostic shows.
pub(super) const SHOWN_CHARS: usize = 120;

/// Caller or response text as a diagnostic shows it: redacted first, then
/// truncated, then escaped on a single line (shared `toolkit_aws` redactor and
/// `toolkit_core` escaping).
pub(super) fn shown(text: &str) -> String {
    escape_untrusted(&redact_text(text), SHOWN_CHARS)
}

/// Why a handler or a command failed. Display never carries a response or
/// recording value, only a class and a code.
#[derive(Debug)]
pub(super) enum Failure {
    /// An AWS call failed (or the recording had no answer for it).
    Aws(AwsError),
    /// Anything else, already safe to show.
    Message(String),
}

impl Failure {
    pub(super) fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

impl From<AwsError> for Failure {
    fn from(error: AwsError) -> Self {
        Self::Aws(error)
    }
}

/// The service error code of a failed call, when it has one.
pub(super) fn service_code(error: &AwsError) -> Option<&str> {
    match error {
        AwsError::Service(service) => service.code(),
        _ => None,
    }
}

/// `aws_service_error (Code)` or the class (`aws_connect`) of a failure.
pub(super) fn aws_class(error: &AwsError) -> String {
    match service_code(error) {
        Some(code) => format!("{} ({})", error.code(), shown(code)),
        None => error.code().to_string(),
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aws(error) => f.write_str(&aws_class(error)),
            Self::Message(text) => f.write_str(text),
        }
    }
}

/// The log lines of a run (the CloudWatch Logs of the Lambda): `LEVEL text`
/// lines and EMF metric lines, written to the diagnostics stream.
#[derive(Debug, Default)]
pub(super) struct Logger {
    records: Vec<LogRecord>,
}

/// Preserve a metric's schema and caller fields until the output sink can
/// mask those fields. A serialized metric must never use the text log path.
#[derive(Debug)]
pub(super) enum LogRecord {
    Text(String),
    Metric(EmfMetric),
}

impl Logger {
    fn push(&mut self, level: &str, text: &str) {
        let safe = neutralize_line(&redact_text(text)).into_owned();
        self.records
            .push(LogRecord::Text(format!("{level} {safe}")));
    }

    pub(super) fn info(&mut self, text: &str) {
        self.push("INFO", text);
    }

    pub(super) fn warning(&mut self, text: &str) {
        self.push("WARNING", text);
    }

    pub(super) fn error(&mut self, text: &str) {
        self.push("ERROR", text);
    }

    /// An EMF record, still carrying typed values and separate caller fields.
    pub(super) fn metric(&mut self, metric: EmfMetric) {
        self.records.push(LogRecord::Metric(metric));
    }

    pub(super) fn records(&self) -> &[LogRecord] {
        &self.records
    }

    #[cfg(test)]
    pub(super) fn lines(&self) -> Vec<String> {
        self.records
            .iter()
            .map(|record| match record {
                LogRecord::Text(line) => line.clone(),
                LogRecord::Metric(metric) => metric.render(&str::to_string),
            })
            .collect()
    }
}

/// `int(text)` as Python reads it: surrounding whitespace, one sign, digits
/// with single underscores between them.
pub(super) fn py_int(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let plain: String = digits.chars().filter(|c| *c != '_').collect();
    let value = plain.parse::<i64>().ok()?;
    Some(if negative { -value } else { value })
}

/// The Lambda environment: `NAME -> value` read from `--config FILE`. The
/// process environment is never read, so a run depends on its inputs only.
#[derive(Debug, Default, Clone)]
pub(super) struct Config {
    vars: Vec<(String, String)>,
}

impl Config {
    /// Reads the members of a parsed `--config` document.
    pub(super) fn from_document(document: &Json) -> Result<Self, String> {
        let members = object_members(document)
            .ok_or_else(|| "the configuration must be a JSON object of strings".to_string())?;
        let mut vars = Vec::with_capacity(members.len());
        for (name, value) in members {
            let text = value.as_str().ok_or_else(|| {
                format!(
                    "configuration member {} must be a string (it is {})",
                    shown(name),
                    value.type_name()
                )
            })?;
            vars.push((name.clone(), text.to_string()));
        }
        Ok(Self { vars })
    }

    #[cfg(test)]
    pub(super) fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self {
            vars: pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    pub(super) fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    /// The value, or the empty string when unset (`os.environ.get(NAME, "")`).
    pub(super) fn text(&self, name: &str) -> &str {
        self.get(name).unwrap_or("")
    }

    /// `os.environ[NAME]`: unset is a failure (the `KeyError` at import).
    pub(super) fn require(&self, name: &str) -> Result<&str, Failure> {
        self.get(name)
            .ok_or_else(|| Failure::message(format!("{name} is not configured")))
    }

    /// `int(os.environ.get(NAME, default))`.
    pub(super) fn integer(&self, name: &str, default: i64) -> Result<i64, Failure> {
        match self.get(name) {
            None => Ok(default),
            Some(text) => {
                py_int(text).ok_or_else(|| Failure::message(format!("{name} must be an integer")))
            }
        }
    }

    /// `[s.strip() for s in value.split(",") if s.strip()]`.
    pub(super) fn list(&self, name: &str) -> Vec<String> {
        self.text(name)
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// What a handler works with: the client, the environment, the log and the
/// pinned instant.
pub(super) struct Ctx<'a> {
    pub(super) client: &'a mut dyn AwsClient,
    pub(super) config: &'a Config,
    pub(super) log: &'a mut Logger,
    pub(super) now: UtcTime,
}

impl Ctx<'_> {
    /// `PRODUCT_CODE` and `SUBSCRIBERS_TABLE` are read at import by
    /// `metering_core.py`, so every pipeline handler needs them.
    pub(super) fn require_core(&self) -> Result<(), Failure> {
        self.config.require("SUBSCRIBERS_TABLE")?;
        self.config.require("PRODUCT_CODE")?;
        self.config.integer("CLEANUP_MAX_WORKERS", 16)?;
        self.config.integer("CLEANUP_MAX_RETRIES", 5)?;
        Ok(())
    }

    pub(super) fn product_code(&self) -> &str {
        self.config.text("PRODUCT_CODE")
    }

    /// `METERING_MODE`, stripped and lower-cased; `live` when unset.
    pub(super) fn metering_mode(&self) -> String {
        match self.config.get("METERING_MODE") {
            None => "live".to_string(),
            Some(text) => text.trim().to_lowercase(),
        }
    }

    pub(super) fn metric_namespace(&self) -> &str {
        self.config
            .get("METRIC_NAMESPACE")
            .unwrap_or("AwsMarketplace/Metering")
    }

    pub(super) fn usage_table(&self) -> Result<&str, Failure> {
        self.config
            .get("USAGE_TABLE")
            .filter(|name| !name.is_empty())
            .ok_or_else(|| Failure::message("USAGE_TABLE is not configured"))
    }

    pub(super) fn aggregated_table(&self) -> Result<&str, Failure> {
        self.config
            .get("AGGREGATED_USAGE_TABLE")
            .filter(|name| !name.is_empty())
            .ok_or_else(|| Failure::message("AGGREGATED_USAGE_TABLE is not configured"))
    }

    pub(super) fn subscribers_table(&self) -> Result<&str, Failure> {
        self.config.require("SUBSCRIBERS_TABLE")
    }
}
