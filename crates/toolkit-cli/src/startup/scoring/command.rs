//! The `startup score` and `startup score-units` handlers.

use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, write_regular_file, Json, ToolkitError};

use super::dumps::dumps_indent2;
use super::profiles::GA_ONLY;
use super::units::{load_run_evidence, score_units};
use super::{load_profiles, score, shown, ScoreError};
use crate::startup::read_text;
use crate::{Invocation, Output};

/// Name of the file `startup score` writes beside the answers file.
const SCORING_RESULT_FILENAME: &str = "scoring-result.json";

/// The runtime registry: `--runtimes-dir`, or `references/runtimes` of the
/// agent-advisor skill below `--plugin-root` (default: the current directory).
fn runtimes_dir(invocation: &Invocation) -> PathBuf {
    if let Some(dir) = invocation.value("--runtimes-dir") {
        return PathBuf::from(dir);
    }
    Path::new(invocation.value("--plugin-root").unwrap_or("."))
        .join("skills")
        .join("agent-advisor")
        .join("references")
        .join("runtimes")
}

/// The required answers-file argument.
fn answers_argument(invocation: &Invocation, command: &str) -> Result<String, ToolkitError> {
    invocation.positional(0).map(str::to_string).ok_or_else(|| {
        ToolkitError::usage(
            "missing_argument",
            format!("usage: toolkit startup {command} ANSWERS.json [--plugin-root DIR]"),
        )
    })
}

/// Reads and parses a JSON input file, naming it safely on failure.
fn read_json(path: &Path) -> Result<Json, ToolkitError> {
    let label = shown(&path.display().to_string());
    let text = read_text(path, &label).map_err(ScoreError::new)?;
    parse_json(&text)
        .map_err(|error| ScoreError::new(format!("{label}: invalid JSON ({error})")).into())
}

/// Handler for `startup score ANSWERS.json`: scores one answers document
/// (`{"entry_point": ..., "answers": {...}}`), writes `scoring-result.json`
/// beside it and prints `RESULT=ok VERDICT=<verdict>`.
///
/// # Errors
///
/// A usage error (exit status 2) without the answers file; a runtime error
/// (exit status 1) when the registry or answers cannot be read, are malformed
/// or the result cannot be written.
pub fn score_command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let answers = answers_argument(invocation, "score")?;
    let answers_path = Path::new(&answers);
    let input = read_json(answers_path)?;
    let profiles = load_profiles(&runtimes_dir(invocation), &GA_ONLY)?;
    let result = score(&input, &profiles, None)?;
    let result_path = answers_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(SCORING_RESULT_FILENAME);
    write_regular_file(&result_path, &dumps_indent2(&result)).map_err(|error| {
        ToolkitError::runtime(
            "startup_scoring_write",
            format!(
                "cannot write {}: {}",
                shown(&result_path.display().to_string()),
                crate::manifests::read_failure(&error)
            ),
        )
    })?;
    let verdict = result
        .get("verdict")
        .and_then(Json::as_str)
        .unwrap_or_default();
    output.line(&format!("RESULT=ok VERDICT={}", shown(verdict)))
}

/// Handler for `startup score-units ANSWERS.json`: scores every
/// `agent_session` unit and prints the `{"units": ..., <primary mirror>}`
/// document.
///
/// # Errors
///
/// A usage error (exit status 2) without the answers file; a runtime error
/// (exit status 1) when the registry, answers or evidence cannot be read, are
/// malformed, or no unit can be scored.
pub fn score_units_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let answers = answers_argument(invocation, "score-units")?;
    let answers_path = Path::new(&answers);
    let document = read_json(answers_path)?;
    let evidence = load_run_evidence(answers_path)?;
    let profiles = load_profiles(&runtimes_dir(invocation), &GA_ONLY)?;
    let result = score_units(&document, &profiles, evidence.as_ref())?;
    output.line(&dumps_indent2(&result))
}
