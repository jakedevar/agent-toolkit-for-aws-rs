//! The per-unit driver (`score_units.py`): scores every `agent_session` unit
//! of an `answers.json`, loads the run's verification evidence and mirrors the
//! primary unit's result at the top level.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use toolkit_core::{parse_json, Json};

use super::evidence::is_run_materialized_evidence;
use super::value::{dict_get, members, required, Res};
use super::{engine, shown, ScoreError};
use crate::startup::read_text;

/// File name of the evidence artifact beside `answers.json`.
pub(crate) const RUN_EVIDENCE_FILENAME: &str = "current-run-verifications.json";

/// Loads the evidence created for this run beside `answers_path`, or `None`
/// when absent. A link is refused.
///
/// # Errors
///
/// An unreadable or invalid artifact, or one whose `run_id` is not the name of
/// the run directory (the directory holding `answers.json`).
pub(crate) fn load_run_evidence(answers_path: &Path) -> Res<Option<Json>> {
    let run_dir = answers_path.parent().unwrap_or_else(|| Path::new(""));
    let evidence_path = run_dir.join(RUN_EVIDENCE_FILENAME);
    let label = shown(&evidence_path.display().to_string());
    match fs::symlink_metadata(&evidence_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ScoreError::new(format!("cannot read {label}: {error}")));
        }
    }
    let text = read_text(&evidence_path, &label).map_err(ScoreError::new)?;
    let evidence = parse_json(&text)
        .map_err(|error| ScoreError::new(format!("{label}: invalid JSON ({error})")))?;
    if !is_run_materialized_evidence(&evidence) {
        return Err(ScoreError::new(format!(
            "{label}: invalid run-materialized evidence"
        )));
    }
    let run_name = run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if evidence.get("run_id").and_then(Json::as_str) != Some(run_name) {
        return Err(ScoreError::new(format!(
            "{label}: run_id must match the answers.json run directory"
        )));
    }
    Ok(Some(evidence))
}

/// The answers of one unit: the system answers (minus the run-evidence and
/// provenance metadata) overlaid with the unit's own (minus its class and
/// provenance). A unit's `null` stays in the merge: scoring ignores it, which
/// also hides the system value.
fn unit_answers(system: &[(String, Json)], unit: &[(String, Json)]) -> Json {
    let mut merged: BTreeMap<&str, &Json> = BTreeMap::new();
    for (name, value) in system {
        if name != "current_run_verifications" && name != "provenance" {
            merged.insert(name, value);
        }
    }
    for (name, value) in unit {
        if name != "workload_class" && name != "provenance" {
            merged.insert(name, value);
        }
    }
    Json::Object(
        merged
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect(),
    )
}

/// Scores every `agent_session` unit of `answers` and mirrors the primary
/// unit's result (or the first unit's) at the top level next to `units`.
///
/// # Errors
///
/// A document of the wrong shape, a scoring failure, or no `agent_session`
/// unit to mirror.
pub(crate) fn score_units(
    answers: &Json,
    profiles: &[Json],
    run_evidence: Option<&Json>,
) -> Res<Json> {
    members(answers, "answers.json")?;
    let system = members(required(answers, "system", "answers.json")?, "\"system\"")?;
    let units = members(required(answers, "units", "answers.json")?, "\"units\"")?;
    let entry_point = answers
        .get("entry_point")
        .cloned()
        .unwrap_or_else(|| Json::string("build_scratch"));

    let mut results: Vec<(String, Json)> = Vec::new();
    for (unit_id, info) in units {
        let info_members = members(info, "a unit entry")?;
        if info.get("workload_class").and_then(Json::as_str) != Some("agent_session") {
            continue;
        }
        let input = Json::object(vec![
            ("entry_point", entry_point.clone()),
            ("answers", unit_answers(system, info_members)),
        ]);
        results.push((
            unit_id.clone(),
            engine::score(&input, profiles, run_evidence)?,
        ));
    }

    let results = Json::Object(results);
    let mirror = match answers.get("primary_unit") {
        Some(primary) => dict_get(&results, primary)?,
        None => None,
    };
    let mirror = match mirror {
        Some(mirror) => mirror,
        None => match &results {
            Json::Object(pairs) => pairs.first().map(|(_, first)| first),
            _ => None,
        }
        .ok_or_else(|| ScoreError::new("answers.json has no agent_session unit to score"))?,
    };
    let mut out = vec![("units".to_string(), results.clone())];
    if let Json::Object(pairs) = mirror {
        out.extend(pairs.iter().cloned());
    }
    Ok(Json::Object(out))
}
