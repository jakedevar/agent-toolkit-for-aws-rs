//! Output records shared by both provider engines: findings, migration deltas
//! and the small JSON builders of the recommendation contract. Key order is the
//! order of the Python dict literals, because the result file is compared
//! byte for byte.

use toolkit_core::Json;

/// A `[BLOCKS]` or `[TUNE]` finding.
#[derive(Debug, Clone)]
pub(crate) struct Finding {
    pub(crate) code: &'static str,
    pub(crate) tag: &'static str,
    pub(crate) message: String,
    pub(crate) remediation: String,
}

pub(crate) const BLOCKS: &str = "[BLOCKS]";
pub(crate) const TUNE: &str = "[TUNE]";

pub(crate) fn finding(
    code: &'static str,
    tag: &'static str,
    message: impl Into<String>,
    remediation: impl Into<String>,
) -> Finding {
    Finding {
        code,
        tag,
        message: message.into(),
        remediation: remediation.into(),
    }
}

impl Finding {
    pub(crate) fn json(&self) -> Json {
        object(vec![
            ("code", text(self.code)),
            ("tag", text(self.tag)),
            ("message", text(&self.message)),
            ("remediation", text(&self.remediation)),
        ])
    }
}

/// A migration delta.
#[derive(Debug, Clone)]
pub(crate) struct Delta {
    pub(crate) code: &'static str,
    pub(crate) category: &'static str,
    pub(crate) description: String,
}

pub(crate) fn delta(
    code: &'static str,
    category: &'static str,
    description: impl Into<String>,
) -> Delta {
    Delta {
        code,
        category,
        description: description.into(),
    }
}

impl Delta {
    pub(crate) fn json(&self) -> Json {
        object(vec![
            ("code", text(self.code)),
            ("category", text(self.category)),
            ("description", text(&self.description)),
        ])
    }
}

pub(crate) fn text(value: &str) -> Json {
    Json::string(value)
}

pub(crate) fn optional_text(value: Option<&str>) -> Json {
    value.map_or(Json::Null, Json::string)
}

pub(crate) fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::object(pairs)
}

pub(crate) fn strings<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Json {
    Json::Array(items.into_iter().map(|item| text(item.as_ref())).collect())
}

pub(crate) fn findings_json(items: &[Finding]) -> Json {
    Json::Array(items.iter().map(Finding::json).collect())
}

pub(crate) fn deltas_json(items: &[Delta]) -> Json {
    Json::Array(items.iter().map(Delta::json).collect())
}

/// `{"mode": ..., "gates": [...]}` evaluation block.
pub(crate) fn evaluation(trajectory: bool, gates: &[&str]) -> Json {
    object(vec![
        (
            "mode",
            text(if trajectory { "trajectory" } else { "prompt" }),
        ),
        ("gates", strings(gates)),
    ])
}

/// `{"strategy": ..., "gate": ...}` rollout block.
pub(crate) fn rollout(strategy: &str, gate: &str) -> Json {
    object(vec![("strategy", text(strategy)), ("gate", text(gate))])
}

/// The `compatibility` block.
pub(crate) fn compatibility(
    native: &[String],
    portable: &[String],
    rearchitecture: &[String],
    unsupported: &[String],
) -> Json {
    object(vec![
        ("native", strings(native)),
        ("portable", strings(portable)),
        ("rearchitecture", strings(rearchitecture)),
        ("unsupported", strings(unsupported)),
    ])
}

/// One `architecture_impacts` entry.
pub(crate) fn impact(feature: &str, impact: &str, recommendation: &str) -> Json {
    object(vec![
        ("feature", text(feature)),
        ("impact", text(impact)),
        ("recommendation", text(recommendation)),
    ])
}

/// A `decision_options` / `alternatives` candidate.
pub(crate) fn candidate(
    model_key: &str,
    model: &str,
    api_path: &str,
    invocation_model_id: Option<&str>,
    requires_cris: bool,
    reason: &str,
) -> Json {
    object(vec![
        ("model_key", text(model_key)),
        ("model", text(model)),
        ("api_path", text(api_path)),
        ("invocation_model_id", optional_text(invocation_model_id)),
        ("requires_cris", Json::Bool(requires_cris)),
        ("reason", text(reason)),
    ])
}

/// The `source_analysis` block of the Anthropic engine.
pub(crate) fn source_analysis(
    detected: Option<&str>,
    target: Option<&str>,
    changed: Option<bool>,
) -> Json {
    object(vec![
        ("detected_version", optional_text(detected)),
        ("target_version", optional_text(target)),
        ("version_changed", changed.map_or(Json::Null, Json::Bool)),
    ])
}
