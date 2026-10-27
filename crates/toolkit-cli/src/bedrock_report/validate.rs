//! `toolkit bedrock-report validate`: the deterministic gate for phase-result
//! files and the run-context comparison (`validate_result.py`).
//!
//! stdout and exit status follow the script: `RESULT=valid CONTROL=...` (0),
//! `RESULT=invalid` plus one line per finding (1), `RESULT=error ...` (2) and
//! `RUN_CONTEXT=match` (0) / `RUN_CONTEXT=mismatch` plus `MISMATCH` lines (1) /
//! `RUN_CONTEXT=error ...` (2). Findings never repeat a value of the checked
//! file (#4602): they name the path, the JSON type and the allowed values.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use toolkit_core::{parse_json, read_regular_file, Json, ToolkitError, MAX_TEXT_FILE_BYTES};
use toolkit_msk::pyfmt::{ascii_escape, py_float_repr, py_str};

use super::identity::{same_model, strip};
use super::{path_reason, usage};
use crate::ai_data::parse_failure_text;
use crate::safe_text::{safe_path, safe_text};
use crate::spec::engine::{self, Issue, Segment};
use crate::{Invocation, Output};

/// The phase-result schemas, in the order the script lists them.
pub(super) const SCHEMA_NAMES: [&str; 5] = [
    "analysis",
    "ingestion",
    "eval",
    "rewrite",
    "delta-decisions",
];

const SCHEMA_SOURCES: [(&str, &str); 5] = [
    (
        "analysis",
        include_str!(
            "../../../../plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/schemas/analysis.json"
        ),
    ),
    (
        "ingestion",
        include_str!(
            "../../../../plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/schemas/ingestion.json"
        ),
    ),
    (
        "eval",
        include_str!(
            "../../../../plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/schemas/eval.json"
        ),
    ),
    (
        "rewrite",
        include_str!(
            "../../../../plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/schemas/rewrite.json"
        ),
    ),
    (
        "delta-decisions",
        include_str!(
            "../../../../plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/schemas/delta-decisions.json"
        ),
    ),
];

/// The evaluation contract the current plugin enforces on a resumed run.
pub(super) const EVALUATION_CONTRACT_VERSION: i64 = 3;

/// Run metadata, not run identity: excluded from the comparison.
const COMPARE_EXCLUDED_FIELDS: [&str; 1] = ["report_date_suffix"];

/// Fields whose values must never be printed side by side.
const REDACTED_FIELDS: [&str; 1] = ["source_key_sha256"];

/// What a command prints on stdout and the status it exits with.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Report {
    pub(super) lines: Vec<String>,
    pub(super) status: u8,
}

impl Report {
    fn new(status: u8, lines: Vec<String>) -> Self {
        Self { lines, status }
    }

    fn single(status: u8, line: String) -> Self {
        Self::new(status, vec![line])
    }
}

pub(super) fn command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let report = match (
        invocation.value("--schema"),
        invocation.value("--check-run-context"),
    ) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(usage(
                "provide exactly one of: --schema, --check-run-context",
            ));
        }
        (Some(name), None) => {
            if !SCHEMA_NAMES.contains(&name) {
                return Err(usage(&format!(
                    "--schema must be one of: {}",
                    SCHEMA_NAMES.join(", ")
                )));
            }
            let Some(file) = invocation.positional(0) else {
                return Err(usage("--schema requires a phase-result file argument"));
            };
            phase_report(name, file)
        }
        (None, Some(saved)) => {
            let Some(current) = invocation.value("--current") else {
                return Err(usage("--check-run-context requires --current"));
            };
            context_report(saved, current)
        }
    };
    for line in &report.lines {
        output.line(line)?;
    }
    output.set_exit_status(report.status);
    Ok(())
}

/// Reads and parses a JSON file; the error text names the (escaped) path.
fn load_json(path: &str) -> Result<Json, String> {
    let shown = safe_path(path);
    let text = read_regular_file(Path::new(path), MAX_TEXT_FILE_BYTES)
        .map_err(|error| format!("cannot read {shown}: {}", path_reason(&error)))?;
    parse_json(&text)
        .map_err(|error| format!("not valid JSON: {shown}: {}", parse_failure_text(&error)))
}

/// The compiled-in schema `name` as a JSON document.
pub(super) fn load_schema(name: &str) -> Result<Json, String> {
    let Some((_, source)) = SCHEMA_SOURCES.iter().find(|(known, _)| *known == name) else {
        return Err(format!("unknown schema {}", safe_text(name)));
    };
    parse_json(source).map_err(|error| parse_failure_text(&error))
}

/// Findings of `data` against `schema`, without any value of `data`.
fn findings(schema: &Json, data: &Json) -> Result<Vec<Issue>, String> {
    engine::compile(schema.clone())?.validate_redacted(data)
}

/// `$`, `$.key` and `$[0]` style path of a finding; keys are escaped.
fn path_text(path: &[Segment]) -> String {
    let mut out = String::from("$");
    for segment in path {
        match segment {
            Segment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
            Segment::Key(key) => {
                out.push('.');
                out.push_str(&safe_text(key));
            }
        }
    }
    out
}

/// For `oneOf` schemas the findings are reported against the branch the user
/// most likely meant: the control-state branch when the document carries a
/// `blocked` or `partial` key, otherwise the payload branch.
fn error_schema<'a>(schema: &'a Json, data: &Json) -> &'a Json {
    let Some(Json::Array(branches)) = schema.get("oneOf") else {
        return schema;
    };
    let Some(payload_branch) = branches.first() else {
        return schema;
    };
    let key = if data.get("blocked").is_some() {
        Some("blocked")
    } else if data.get("partial").is_some() {
        Some("partial")
    } else {
        None
    };
    match key {
        None => payload_branch,
        Some(key) => branches
            .iter()
            .find(|branch| {
                branch
                    .get("properties")
                    .is_some_and(|properties| properties.get(key).is_some())
            })
            .unwrap_or(schema),
    }
}

/// Control state of a schema-valid result.
#[derive(Debug, PartialEq, Eq)]
enum Control {
    Ok,
    Blocked(String),
    Partial(String, String),
}

fn control_state(data: &Json) -> Control {
    if let Some(blocked) = data.get("blocked") {
        return Control::Blocked(blocked.get("reason").map_or_else(String::new, py_str));
    }
    if let Some(partial) = data.get("partial") {
        let part = |key: &str| partial.get(key).map_or_else(|| "0".to_string(), py_str);
        return Control::Partial(part("completed"), part("total"));
    }
    Control::Ok
}

/// `same_model_family: true` is only valid when every `"source -> target"`
/// pair of `target_models` names the same model.
fn identity_holds(data: &Json) -> bool {
    let provider = data
        .get("source_provider")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let Some(Json::Array(targets)) = data.get("target_models") else {
        return false;
    };
    !targets.is_empty()
        && targets.iter().all(|entry| {
            entry
                .as_str()
                .and_then(|pair| pair.split_once("->"))
                .is_some_and(|(source, target)| same_model(provider, strip(source), strip(target)))
        })
}

fn valid_report(name: &str, data: &Json) -> Report {
    let control = control_state(data);
    if name == "analysis"
        && control == Control::Ok
        && data.get("same_model_family") == Some(&Json::Bool(true))
        && !identity_holds(data)
    {
        return Report::new(
            1,
            vec![
                "RESULT=invalid".to_string(),
                "$.same_model_family: true requires every source/target identity to match"
                    .to_string(),
            ],
        );
    }
    let line = match control {
        Control::Ok => "RESULT=valid CONTROL=ok".to_string(),
        Control::Blocked(reason) => {
            format!("RESULT=valid CONTROL=blocked REASON={}", safe_text(&reason))
        }
        Control::Partial(completed, total) => format!(
            "RESULT=valid CONTROL=partial COMPLETED={} TOTAL={}",
            safe_text(&completed),
            safe_text(&total)
        ),
    };
    Report::single(0, line)
}

/// `validate_phase`: the report for `--schema NAME FILE`.
pub(super) fn phase_report(name: &str, file: &str) -> Report {
    let data = match load_json(file) {
        Ok(data) => data,
        Err(message) => return Report::single(2, format!("RESULT=error {message}")),
    };
    let schema = match load_schema(name) {
        Ok(schema) => schema,
        Err(message) => {
            return Report::single(2, format!("RESULT=error schema load failed: {message}"));
        }
    };
    let evaluation_failed = |reason: &str| {
        Report::single(
            2,
            format!(
                "RESULT=error schema evaluation failed: {}",
                safe_text(reason)
            ),
        )
    };
    match findings(&schema, &data) {
        Ok(list) if list.is_empty() => return valid_report(name, &data),
        Ok(_) => {}
        Err(reason) => return evaluation_failed(&reason),
    }
    match findings(error_schema(&schema, &data), &data) {
        Ok(list) => {
            let mut lines = vec!["RESULT=invalid".to_string()];
            lines.extend(
                list.iter()
                    .map(|issue| format!("{}: {}", path_text(&issue.path), issue.message)),
            );
            Report::new(1, lines)
        }
        Err(reason) => evaluation_failed(&reason),
    }
}

// ---- run-context comparison ---------------------------------------------------------

/// Flattens nested JSON into `{path: leaf}`; empty containers become sentinel
/// leaves so "present but empty" differs from "absent".
fn flatten(value: &Json, prefix: &str, out: &mut BTreeMap<String, Json>) {
    match value {
        Json::Object(members) if members.is_empty() => {
            out.insert(prefix.to_string(), Json::string("__empty_object__"));
        }
        Json::Object(members) => {
            for (key, member) in members {
                flatten(member, &format!("{prefix}.{key}"), out);
            }
        }
        Json::Array(items) if items.is_empty() => {
            out.insert(prefix.to_string(), Json::string("__empty_array__"));
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                flatten(item, &format!("{prefix}[{index}]"), out);
            }
        }
        scalar => {
            out.insert(prefix.to_string(), scalar.clone());
        }
    }
}

/// `'$.log_files[0].sha256'` -> `log_files`.
fn top_key(path: &str) -> String {
    path.chars()
        .skip(2)
        .take_while(|c| !matches!(c, '.' | '['))
        .collect()
}

fn flat(value: &Json) -> BTreeMap<String, Json> {
    let mut out = BTreeMap::new();
    flatten(value, "$", &mut out);
    out.retain(|path, _| !COMPARE_EXCLUDED_FIELDS.contains(&top_key(path).as_str()));
    out
}

#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

fn as_number(value: &Json) -> Option<Num> {
    match value {
        Json::Bool(flag) => Some(Num::Int(i64::from(*flag))),
        Json::Int(number) => Some(Num::Int(*number)),
        Json::Float(number) => Some(Num::Float(*number)),
        _ => None,
    }
}

fn same_float(left: f64, right: f64) -> bool {
    left.partial_cmp(&right) == Some(Ordering::Equal)
}

fn float_is_int(float: f64, int: i64) -> bool {
    same_float(float.fract(), 0.0) && format!("{float:.0}").parse::<i64>() == Ok(int)
}

fn numbers_equal(left: Num, right: Num) -> bool {
    match (left, right) {
        (Num::Int(a), Num::Int(b)) => a == b,
        (Num::Float(a), Num::Float(b)) => same_float(a, b),
        (Num::Int(int), Num::Float(float)) | (Num::Float(float), Num::Int(int)) => {
            float_is_int(float, int)
        }
    }
}

/// Python `==` on two JSON leaves (`1 == 1.0 == True`, `"1" != 1`).
fn leaves_equal(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Null, Json::Null) => true,
        (Json::String(a), Json::String(b)) => a == b,
        _ => match (as_number(left), as_number(right)) {
            (Some(a), Some(b)) => numbers_equal(a, b),
            _ => false,
        },
    }
}

/// `json.dumps(leaf)`: ASCII only, Python float spelling.
fn dump(value: &Json) -> String {
    match value {
        Json::Float(number) => py_float_repr(*number),
        other => ascii_escape(&other.to_compact_string()).replace('\u{7f}', "\\u007f"),
    }
}

/// The `MISMATCH` lines (empty means match). The current evaluation contract is
/// enforced even when `current` omits or changes its version.
pub(super) fn compare_run_contexts(saved: &Json, current: &[(String, Json)]) -> Vec<String> {
    let mut members = current.to_vec();
    let version = Json::Int(EVALUATION_CONTRACT_VERSION);
    match members
        .iter_mut()
        .find(|(key, _)| key == "evaluation_contract_version")
    {
        Some(slot) => slot.1 = version,
        None => members.push(("evaluation_contract_version".to_string(), version)),
    }
    let flat_saved = flat(saved);
    let flat_current = flat(&Json::Object(members));
    let absent = Json::string("<absent>");
    let paths: BTreeSet<&String> = flat_saved.keys().chain(flat_current.keys()).collect();
    let mut lines = Vec::new();
    for path in paths {
        let saved_value = flat_saved.get(path).unwrap_or(&absent);
        let current_value = flat_current.get(path).unwrap_or(&absent);
        if leaves_equal(saved_value, current_value) {
            continue;
        }
        let shown = safe_text(path);
        if REDACTED_FIELDS.contains(&top_key(path).as_str()) {
            lines.push(format!("MISMATCH {shown} differs"));
        } else {
            lines.push(format!(
                "MISMATCH {shown} saved={} current={}",
                dump(saved_value),
                dump(current_value)
            ));
        }
    }
    lines
}

/// `check_run_context`: the report for `--check-run-context SAVED --current CURRENT`.
pub(super) fn context_report(saved_path: &str, current_path: &str) -> Report {
    let error = |message: String| Report::single(2, format!("RUN_CONTEXT=error {message}"));
    let saved = match load_json(saved_path) {
        Ok(saved) => saved,
        Err(message) => return error(message),
    };
    let current = match load_json(current_path) {
        Ok(current) => current,
        Err(message) => return error(message),
    };
    let Json::Object(members) = &current else {
        return error("the current context must be a JSON object".to_string());
    };
    let mismatches = compare_run_contexts(&saved, members);
    if mismatches.is_empty() {
        return Report::single(0, "RUN_CONTEXT=match".to_string());
    }
    let mut lines = vec!["RUN_CONTEXT=mismatch".to_string()];
    lines.extend(mismatches);
    Report::new(1, lines)
}
