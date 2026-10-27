//! Runtime scoring of the agent-advisor skill (#4400, slice `startup-scoring`).
//!
//! Ports `scripts/scoring.py` (the deterministic, registry-driven runtime
//! scoring engine) and `scripts/score_units.py` (the per-unit driver) of
//! `plugins/aws-startup-advisor/skills/agent-advisor`:
//!
//! | Command | Python original |
//! | --- | --- |
//! | `startup score ANSWERS` | `scoring.py ANSWERS` |
//! | `startup score-units ANSWERS` | `score_units.py ANSWERS` |
//!
//! The engine is pure: an answers document plus the runtime profiles in
//! `references/runtimes/*.json` produce one recommendation object. Every
//! value is handled as a [`Json`] value with Python's equality, membership
//! and truthiness, because the profiles are data files whose shape the Python
//! code never validated beyond four required keys. Where Python would crash
//! with a traceback on a malformed profile, answers file or evidence file, the
//! port reports a one-line error (exit status 1) that names the field and the
//! file but never echoes an input value. Every deliberate difference is in
//! `docs/migration/startup-scoring.md`.

use std::fmt::{self, Write as _};

use toolkit_core::ToolkitError;

mod command;
mod dumps;
mod engine;
mod evidence;
mod profiles;
mod units;
mod value;

#[cfg(test)]
pub(in crate::startup) mod fixtures;
#[cfg(test)]
mod locks;
#[cfg(test)]
mod tests;

pub use command::{score_command, score_units_command};

pub(crate) use engine::score;
pub(crate) use profiles::load_profiles;

/// Score of a dimension a profile does not rate (`NEUTRAL_SCORE`).
pub(crate) const NEUTRAL_SCORE: i64 = 2;

/// Scoring dimensions, in the order the Python engine sums them.
pub(crate) const DIMENSIONS: [&str; 15] = [
    "session_duration",
    "traffic_pattern",
    "platform_fit",
    "session_state",
    "ops_preference",
    "isolation",
    "memory_needs",
    "multi_agent",
    "framework",
    "existing_cluster",
    "multi_cloud",
    "idle_resume",
    "compute_tier",
    "launch_concurrency",
    "instance_type_requirement",
];

/// Legal answer values per scoring dimension (`LEGAL_VALUES`); the registry tests
/// hold every profile to them.
#[cfg(test)]
pub(crate) const LEGAL_VALUES: [(&str, &[&str]); 15] = [
    (
        "session_duration",
        &["under_15min", "15min_to_8hr", "over_8hr", "unknown"],
    ),
    ("traffic_pattern", &["bursty", "steady", "idle", "unknown"]),
    ("platform_fit", &["ecs", "eks", "lambda", "none", "unknown"]),
    (
        "session_state",
        &["stateless", "stateful", "hitl", "unknown"],
    ),
    (
        "ops_preference",
        &["minimal", "moderate", "full_control", "unknown"],
    ),
    (
        "isolation",
        &["required", "nice_to_have", "not_needed", "unknown"],
    ),
    (
        "memory_needs",
        &["cross_session", "session_only", "none", "unknown"],
    ),
    ("multi_agent", &["yes", "no", "unknown"]),
    (
        "framework",
        &[
            "strands",
            "langgraph",
            "crewai",
            "custom",
            "none",
            "unknown",
        ],
    ),
    ("existing_cluster", &["eks", "ecs", "none", "unknown"]),
    ("multi_cloud", &["yes", "no", "unknown"]),
    (
        "idle_resume",
        &["process_level", "filesystem", "none", "unknown"],
    ),
    (
        "compute_tier",
        &["light", "heavy_non_gpu", "gpu", "unknown"],
    ),
    (
        "launch_concurrency",
        &["high", "moderate", "low", "unknown"],
    ),
    ("instance_type_requirement", &["yes", "no", "unknown"]),
];

/// The legal values of `dimension`, if it is a scoring dimension.
#[cfg(test)]
pub(crate) fn legal_values(dimension: &str) -> Option<&'static [&'static str]> {
    LEGAL_VALUES
        .iter()
        .find(|(name, _)| *name == dimension)
        .map(|(_, values)| *values)
}

/// A scoring, profile or evidence problem. The text names fields and files,
/// never an input value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScoreError(String);

impl ScoreError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ScoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ScoreError> for ToolkitError {
    fn from(error: ScoreError) -> Self {
        Self::runtime("startup_scoring_invalid", error.0)
    }
}

/// Text that is safe to print: control characters, escapes, bidirectional
/// controls and other non-printing characters of an untrusted name or path are
/// written as `\u{..}` so they cannot forge or hide report lines (#4602).
pub(crate) fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let plain = c == ' ' || c.is_ascii_graphic() || (!c.is_ascii() && c.is_alphanumeric());
        if plain {
            out.push(c);
        } else {
            let _ = write!(out, "\\u{{{:04x}}}", u32::from(c));
        }
    }
    out
}
