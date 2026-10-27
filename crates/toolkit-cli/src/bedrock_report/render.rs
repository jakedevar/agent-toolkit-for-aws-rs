//! `toolkit bedrock-report render`: the in-chat summary of a finished Execute
//! run (`render_report.py`).
//!
//! The summary is built from the phase-result files (`--phase-results`) or one
//! legacy payload file, and the newest `MIGRATION_REPORT_*.md` of the
//! repository is copied to the results directory. Every value taken from those
//! files is printed through `safe_text`, so a result file cannot forge summary
//! lines or drive the terminal (#4602).

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use toolkit_core::{
    parse_json, read_regular_file, write_regular_file, Json, ToolkitError, MAX_TEXT_FILE_BYTES,
};
use toolkit_msk::pyfmt::py_str;

use super::{path_reason, usage};
use crate::ai_data::parse_failure_text;
use crate::bedrock_resolve::identity::is_decimal;
use crate::safe_text::{safe_path, safe_text};
use crate::startup::text::is_space;
use crate::{Invocation, Output};

const REPORT_PREFIX: &str = "MIGRATION_REPORT_";
const REPORT_SUFFIX: &str = ".md";
const NO_REPORT: &str = "(see repository root: MIGRATION_REPORT_*.md)";

/// The largest report the command copies.
pub(super) const REPORT_COPY_LIMIT: u64 = 16 * 1024 * 1024;

pub(super) fn command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let payload_file = invocation.positional(0).filter(|path| !path.is_empty());
    let phase_results = invocation
        .value("--phase-results")
        .filter(|path| !path.is_empty());
    if payload_file.is_some() == phase_results.is_some() {
        return Err(usage(
            "provide exactly one of: payload_json, --phase-results",
        ));
    }
    let payload = if let Some(dir) = phase_results {
        let Some(repo) = invocation.value("--repo") else {
            return Err(usage("--phase-results requires --repo"));
        };
        load_phase_results(Path::new(dir), repo, invocation.value("--date-suffix"))
    } else {
        let file = payload_file.unwrap_or_default();
        match load_payload(file) {
            Ok(payload) => payload,
            Err(message) => {
                output.diagnostic(&format!(
                    "render_report: cannot read payload JSON: {message}"
                ));
                output.set_exit_status(1);
                return Ok(());
            }
        }
    };
    if let Some(report) = find_report_path(&payload) {
        let copied = results_dir(invocation.value("--results-dir"))
            .and_then(|dir| copy_report(&report, &dir));
        if let Err(message) = copied {
            output.diagnostic(&format!("render_report: cannot copy the report: {message}"));
            output.set_exit_status(1);
            return Ok(());
        }
    }
    output.line(&summarize(&payload))
}

fn load_payload(file: &str) -> Result<Json, String> {
    let text = read_regular_file(Path::new(file), MAX_TEXT_FILE_BYTES)
        .map_err(|error| path_reason(&error))?;
    let payload = parse_json(&text).map_err(|error| parse_failure_text(&error))?;
    if matches!(payload, Json::Object(_)) {
        Ok(payload)
    } else {
        Err("the payload must be a JSON object".to_string())
    }
}

/// `--results-dir`, or `~/saws-migrate-results`.
fn results_dir(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(dir) = explicit {
        return Ok(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join("saws-migrate-results"))
        .ok_or_else(|| "the home directory is unknown; pass --results-dir".to_string())
}

// ---- payload assembly -----------------------------------------------------------------

/// One phase-result file, or `None` when it is missing, unreadable, not JSON
/// or a blocked/partial control-state file (which is not a payload).
fn read_phase_file(dir: &Path, name: &str) -> Option<Json> {
    let text = read_regular_file(&dir.join(name), MAX_TEXT_FILE_BYTES).ok()?;
    let data = parse_json(&text).ok()?;
    if data.get("blocked").is_some() || data.get("partial").is_some() {
        return None;
    }
    Some(data)
}

/// Assembles the `summarize` payload from the phase-result files. Missing or
/// control-state files degrade to empty values.
pub(super) fn load_phase_results(dir: &Path, repo: &str, date_suffix: Option<&str>) -> Json {
    let or_empty = |name: &str| read_phase_file(dir, name).unwrap_or(Json::Object(Vec::new()));
    Json::object(vec![
        ("rewrite", or_empty("rewrite.json")),
        ("evalRes", or_empty("eval.json")),
        (
            "deltaDecisions",
            read_phase_file(dir, "delta-decisions.json").unwrap_or(Json::Null),
        ),
        ("repo", Json::string(repo)),
        (
            "reportDateSuffix",
            date_suffix.map_or(Json::Null, Json::string),
        ),
    ])
}

// ---- report lookup and copy ------------------------------------------------------------

/// A regular file that is not a symbolic link.
fn is_plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// `MIGRATION_REPORT_<suffix>.md` of the repository when it exists, otherwise
/// the last in name order of `MIGRATION_REPORT_*.md`. A suffix with a path
/// separator never names a report (it could reach a file outside the
/// repository), and only regular files that are not links qualify.
pub(super) fn find_report_path(payload: &Json) -> Option<PathBuf> {
    let repo = match payload.get("repo") {
        Some(Json::String(repo)) if !repo.is_empty() => Path::new(repo.as_str()),
        _ => return None,
    };
    if let Some(Json::String(suffix)) = payload.get("reportDateSuffix") {
        if !suffix.is_empty() && !suffix.contains(['/', '\\', '\0']) {
            let candidate = repo.join(format!("{REPORT_PREFIX}{suffix}{REPORT_SUFFIX}"));
            if is_plain_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    let mut names: Vec<String> = fs::read_dir(repo)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(REPORT_PREFIX) && name.ends_with(REPORT_SUFFIX))
        .filter(|name| name.len() >= REPORT_PREFIX.len() + REPORT_SUFFIX.len())
        .filter(|name| is_plain_file(&repo.join(name)))
        .collect();
    names.sort();
    names.pop().map(|name| repo.join(name))
}

/// The path as `pathlib` prints it: no `.` components, no repeated separators.
pub(super) fn display_path(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        if component != Component::CurDir {
            out.push(component.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        ".".to_string()
    } else {
        out.to_string_lossy().into_owned()
    }
}

/// Copies the report (content only) into `dest_dir`, creating it. Reads and
/// writes refuse symbolic links.
pub(super) fn copy_report(report: &Path, dest_dir: &Path) -> Result<(), String> {
    let Some(name) = report.file_name() else {
        return Err("the report has no file name".to_string());
    };
    let content = read_regular_file(report, REPORT_COPY_LIMIT).map_err(|error| {
        format!(
            "cannot read {}: {}",
            safe_path(&report.to_string_lossy()),
            path_reason(&error)
        )
    })?;
    fs::create_dir_all(dest_dir).map_err(|error| {
        format!(
            "cannot create {}: {error}",
            safe_path(&dest_dir.to_string_lossy())
        )
    })?;
    let target = dest_dir.join(name);
    write_regular_file(&target, &content).map_err(|error| {
        format!(
            "cannot write {}: {}",
            safe_path(&target.to_string_lossy()),
            path_reason(&error)
        )
    })
}

// ---- the summary --------------------------------------------------------------------------

/// `- Prompt eval pass rate: ...`, always `N/A` for a zero-golden-case run.
fn pass_rate_line(eval: Option<&Json>, notes: &str) -> String {
    if notes.contains("no_golden_cases: true") {
        return "- Prompt eval pass rate: N/A (no golden cases — quality scoring skipped)"
            .to_string();
    }
    let Some(percent) = eval
        .and_then(|eval| eval.get("pass_rate"))
        .and_then(percent_text)
    else {
        return "- Prompt eval pass rate: (unavailable)".to_string();
    };
    let count = |key: &str| {
        eval.and_then(|eval| eval.get(key))
            .map_or_else(|| "0".to_string(), |value| safe_text(&py_str(value)))
    };
    let mut line = format!(
        "- Prompt eval pass rate: {percent}% ({} cases, {} failures)",
        count("total_cases"),
        count("failures")
    );
    if let Some(coverage) = partial_coverage(notes) {
        let _ = write!(
            line,
            " — partial coverage {} (throttled)",
            safe_text(&coverage)
        );
    }
    line
}

/// `round(pass_rate * 100)` for a JSON number (a boolean is an integer, as in
/// Python); `None` for any other value.
fn percent_text(value: &Json) -> Option<String> {
    match value {
        Json::Bool(flag) => Some((i128::from(*flag) * 100).to_string()),
        Json::Int(number) => Some((i128::from(*number) * 100).to_string()),
        Json::Float(number) => {
            let rounded = (number * 100.0).round_ties_even();
            Some(if rounded.abs() < 0.5 {
                "0".to_string()
            } else {
                format!("{rounded:.0}")
            })
        }
        _ => None,
    }
}

/// `re.search(r"partial_coverage: (\S+)", notes)`.
fn partial_coverage(notes: &str) -> Option<String> {
    const KEY: &str = "partial_coverage: ";
    let mut from = 0;
    while let Some(found) = notes[from..].find(KEY) {
        let start = from + found + KEY.len();
        let word: String = notes[start..]
            .chars()
            .take_while(|c| !is_space(*c))
            .collect();
        if !word.is_empty() {
            return Some(word);
        }
        from += found + 1;
    }
    None
}

fn digits_end(chars: &[char], start: usize) -> usize {
    start
        + chars[start..]
            .iter()
            .take_while(|c| is_decimal(**c))
            .count()
}

fn spaces_end(chars: &[char], start: usize) -> usize {
    start + chars[start..].iter().take_while(|c| is_space(**c)).count()
}

/// `(\d+)\s*/\s*(\d+)\s+passing` matched at `start`.
fn counts_at(chars: &[char], start: usize) -> Option<(String, String)> {
    let first_end = digits_end(chars, start);
    let slash = spaces_end(chars, first_end);
    if chars.get(slash) != Some(&'/') {
        return None;
    }
    let second_start = spaces_end(chars, slash + 1);
    let second_end = digits_end(chars, second_start);
    if second_end == second_start {
        return None;
    }
    let word = spaces_end(chars, second_end);
    let passing: String = chars[word..].iter().take("passing".len()).collect();
    if word == second_end || passing != "passing" {
        return None;
    }
    Some((
        chars[start..first_end].iter().collect(),
        chars[second_start..second_end].iter().collect(),
    ))
}

/// The first `N/M passing` of the rewriter's free-text notes.
fn passing_counts(notes: &str) -> Option<(String, String)> {
    let chars: Vec<char> = notes.chars().collect();
    (0..chars.len())
        .filter(|&index| is_decimal(chars[index]) && (index == 0 || !is_decimal(chars[index - 1])))
        .find_map(|index| counts_at(&chars, index))
}

/// The summary text. Values that are missing or of an unexpected type degrade
/// to the same placeholders the script prints.
pub(super) fn summarize(payload: &Json) -> String {
    let rewrite = payload.get("rewrite");
    let eval = payload.get("evalRes");
    let rewrite_member = |key: &str| rewrite.and_then(|rewrite| rewrite.get(key));
    let notes_of = |member: Option<&Json>| match member {
        Some(Json::String(notes)) => notes.clone(),
        _ => String::new(),
    };
    let eval_notes = notes_of(eval.and_then(|eval| eval.get("notes")));

    let branch = rewrite_member("branch_name")
        .map_or_else(|| "(none)".to_string(), |value| safe_text(&py_str(value)));
    let files = match rewrite_member("files_changed") {
        Some(Json::Array(files)) => files.len(),
        _ => 0,
    };
    let tests = passing_counts(&notes_of(rewrite_member("notes"))).map(|(passed, total)| {
        format!(
            "- Tests: {}/{} passing",
            safe_text(&passed),
            safe_text(&total)
        )
    });
    let deltas = match payload.get("deltaDecisions") {
        Some(Json::Array(deltas)) if !deltas.is_empty() => Some(format!(
            "- Behavior-delta decisions applied: {}",
            deltas.len()
        )),
        _ => None,
    };
    let report = find_report_path(payload).map_or_else(
        || NO_REPORT.to_string(),
        |path| safe_path(&display_path(&path)),
    );

    let mut lines = vec![
        "AI Migration Complete!".to_string(),
        format!("- Branch: {branch}"),
        pass_rate_line(eval, &eval_notes),
        format!("- Files modified: {files} files"),
    ];
    lines.extend(tests);
    lines.extend(deltas);
    lines.push(format!("- Report: {report}"));
    lines.join("\n")
}
