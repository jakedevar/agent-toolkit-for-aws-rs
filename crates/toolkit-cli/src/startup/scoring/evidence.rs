//! Run-materialized verification evidence: the artifact contract and the
//! source-backed checks that decide whether a volatile fact is verified in the
//! current run (`is_run_materialized_evidence`, `_has_current_run_evidence`,
//! `_is_current_run_verified`).

use toolkit_core::Json;

use super::value::{dict_get, py_eq, Res};
use crate::manifests::py_strip;

pub(crate) const RUN_EVIDENCE_ARTIFACT_TYPE: &str = "agent-advisor.current-run-verifications";
pub(crate) const RUN_EVIDENCE_SCHEMA_VERSION: i64 = 1;

const ARTIFACT_KEYS: [&str; 4] = ["artifact_type", "schema_version", "run_id", "verifications"];

fn is_blank(text: &str) -> bool {
    py_strip(text).is_empty()
}

/// Whether `source` is a canonical public AWS documentation URL.
fn is_authoritative_aws_source(source: &str) -> bool {
    source.starts_with("https://aws.amazon.com/")
        || source.starts_with("https://docs.aws.amazon.com/")
}

/// Whether an artifact satisfies the run-evidence schema contract: exactly the
/// four keys, the pinned type and version, a non-blank run id and
/// `verifications` records limited to `status`, `source` and `value`.
///
/// An unhashable `status` makes the artifact invalid (Python raised).
pub(crate) fn is_run_materialized_evidence(evidence: &Json) -> bool {
    let Json::Object(pairs) = evidence else {
        return false;
    };
    if pairs.len() != ARTIFACT_KEYS.len() || ARTIFACT_KEYS.iter().any(|k| evidence.get(k).is_none())
    {
        return false;
    }
    let type_ok =
        evidence.get("artifact_type").and_then(Json::as_str) == Some(RUN_EVIDENCE_ARTIFACT_TYPE);
    let version_ok = evidence
        .get("schema_version")
        .is_some_and(|version| py_eq(version, &Json::Int(RUN_EVIDENCE_SCHEMA_VERSION)));
    let run_id_ok = evidence
        .get("run_id")
        .and_then(Json::as_str)
        .is_some_and(|run_id| !is_blank(run_id));
    let Some(Json::Object(records)) = evidence.get("verifications") else {
        return false;
    };
    type_ok && version_ok && run_id_ok && records.iter().all(|(_, record)| record_is_valid(record))
}

fn record_is_valid(record: &Json) -> bool {
    let Json::Object(fields) = record else {
        return false;
    };
    if !fields
        .iter()
        .all(|(name, _)| matches!(name.as_str(), "status" | "source" | "value"))
    {
        return false;
    }
    match record.get("status").and_then(Json::as_str) {
        Some("verified") => {
            record.get("source").and_then(Json::as_str).is_some()
                && record
                    .get("value")
                    .and_then(Json::as_str)
                    .is_some_and(|value| !is_blank(value))
        }
        Some("not_verified" | "failed") => true,
        _ => false,
    }
}

/// A source-backed, non-empty observation in a trusted artifact.
pub(super) fn has_current_run_evidence(record: Option<&Json>) -> bool {
    let Some(record) = record else {
        return false;
    };
    matches!(record, Json::Object(_))
        && record.get("status").and_then(Json::as_str) == Some("verified")
        && record
            .get("source")
            .and_then(Json::as_str)
            .is_some_and(is_authoritative_aws_source)
        && record
            .get("value")
            .and_then(Json::as_str)
            .is_some_and(|value| !is_blank(value))
}

/// Whether run-materialized evidence exactly verifies a constraint: the record
/// is source-backed, its source is one the constraint accepts and its value is
/// the expected one.
///
/// # Errors
///
/// When `key` is a list or object (an unhashable lookup in Python).
pub(super) fn is_current_run_verified(
    verifications: &Json,
    key: &Json,
    expected: Option<&Json>,
    sources: Option<&Json>,
) -> Res<bool> {
    let record = dict_get(verifications, key)?;
    let Some(record) = record.filter(|record| has_current_run_evidence(Some(record))) else {
        return Ok(false);
    };
    let Some(Json::String(expected)) = expected else {
        return Ok(false);
    };
    let Some(Json::Array(sources)) = sources else {
        return Ok(false);
    };
    if expected.is_empty() || sources.is_empty() {
        return Ok(false);
    }
    let source = record.get("source").and_then(Json::as_str);
    let value = record.get("value").and_then(Json::as_str);
    Ok(sources.iter().any(|accepted| accepted.as_str() == source) && value == Some(expected))
}
