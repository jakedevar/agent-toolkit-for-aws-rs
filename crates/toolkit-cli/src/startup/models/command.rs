//! The `startup model-recommend` handler (`model_recommendation.py main`).

use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, write_regular_file, Json, ToolkitError};

use super::catalog::{load_anthropic_catalog, load_openai_catalog, read_json_file, Catalog};
use super::request::Input;
use super::{recommend, Res, DEFAULT_CATALOG, OPENAI_CATALOG};
use crate::manifests::{py_eq, read_failure};
use crate::safe_text::safe_path;
use crate::spec::engine::{self, escaped_path};
use crate::startup::read_text;
use crate::startup::scoring::dumps_indent2;
use crate::{Invocation, Output};

/// The shipped contract of the input document.
const INPUT_SCHEMA: &str = include_str!(
    "../../../../../plugins/aws-startup-advisor/skills/agent-advisor/scripts/schemas/model-recommendation-input.json"
);

/// The shipped contract of the result document.
const OUTPUT_SCHEMA: &str = include_str!(
    "../../../../../plugins/aws-startup-advisor/skills/agent-advisor/scripts/schemas/model-recommendation.json"
);

/// Name of the result file written beside the input.
const RESULT_FILENAME: &str = "model-recommendation.json";

/// Name of the probe-result file that goes stale when the invocation contract
/// changes.
const VERIFICATION_FILENAME: &str = "model-verification.json";

/// Findings printed before the rest are summarized.
const MAX_FINDINGS: usize = 20;

fn models_dir(invocation: &Invocation) -> PathBuf {
    Path::new(invocation.value("--plugin-root").unwrap_or("."))
        .join("skills")
        .join("agent-advisor")
        .join("references")
        .join("models")
}

fn catalog_path(invocation: &Invocation, flag: &str, default: &str) -> PathBuf {
    invocation
        .value(flag)
        .map_or_else(|| models_dir(invocation).join(default), PathBuf::from)
}

/// Checks `document` against a shipped schema; every finding is printed as a
/// `  - <path>: <message>` line that names the field and the rule but never a
/// value of the document, and the call fails with `summary`.
fn check_schema(
    schema: &str,
    document: &Json,
    summary: &str,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let compiled = parse_json(schema)
        .map_err(|error| error.to_string())
        .and_then(engine::compile)
        .map_err(|reason| {
            ToolkitError::runtime(
                "startup_models_schema",
                format!("the bundled schema cannot be used: {reason}"),
            )
        })?;
    let issues = compiled
        .validate_redacted(document)
        .map_err(|reason| ToolkitError::runtime("startup_models_schema", reason))?;
    if issues.is_empty() {
        return Ok(());
    }
    for issue in issues.iter().take(MAX_FINDINGS) {
        output.diagnostic(&format!(
            "  - {}: {}",
            escaped_path(&issue.path),
            issue.message
        ));
    }
    if issues.len() > MAX_FINDINGS {
        output.diagnostic(&format!("  - ... and {} more", issues.len() - MAX_FINDINGS));
    }
    Err(ToolkitError::runtime("startup_models_invalid", summary))
}

/// The members of a result that fix the invocation (`_invocation_contract`),
/// or `None` for a document that does not have the result shape.
fn invocation_contract(result: &Json) -> Option<Json> {
    let Json::Object(workloads) = result.get("workloads")? else {
        return None;
    };
    let mut contract = Vec::new();
    for (id, workload) in workloads {
        let verification = workload.get("verification")?;
        let sorted_list = |key: &str| -> Option<Json> {
            let mut items = match verification.get(key) {
                None => Vec::new(),
                Some(Json::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()?,
                Some(_) => return None,
            };
            items.sort();
            Some(Json::Array(items.into_iter().map(Json::String).collect()))
        };
        let field = |key: &str| workload.get(key).cloned().unwrap_or(Json::Null);
        contract.push((
            id.clone(),
            Json::object(vec![
                ("decision_status", field("decision_status")),
                (
                    "model",
                    workload
                        .get("model_identity")
                        .and_then(|identity| identity.get("path_model_id"))
                        .cloned()
                        .unwrap_or(Json::Null),
                ),
                ("api_path", field("api_path")),
                ("invocation_model_id", field("invocation_model_id")),
                (
                    "region",
                    verification.get("region").cloned().unwrap_or(Json::Null),
                ),
                (
                    "allowed_inference_profiles",
                    sorted_list("allowed_inference_profiles")?,
                ),
                (
                    "allowed_in_region_model_arns",
                    sorted_list("allowed_in_region_model_arns")?,
                ),
            ]),
        ));
    }
    Some(Json::Object(contract))
}

/// Removes the stale probe result beside `output` when the invocation contract
/// differs from the one the previous result recorded.
fn drop_stale_verification(output_path: &Path, result: &Json) -> Result<(), ToolkitError> {
    let previous = read_text(output_path, "the previous result")
        .ok()
        .and_then(|text| parse_json(&text).ok())
        .and_then(|document| invocation_contract(&document));
    let current = invocation_contract(result);
    let unchanged = match (&previous, &current) {
        (Some(before), Some(after)) => py_eq(before, after),
        _ => false,
    };
    if unchanged {
        return Ok(());
    }
    let stale = output_path.with_file_name(VERIFICATION_FILENAME);
    match fs::remove_file(&stale) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ToolkitError::runtime(
            "startup_models_write",
            format!(
                "cannot remove {}: {}",
                safe_path(&stale.display().to_string()),
                error
            ),
        )),
    }
}

/// Handler for `startup model-recommend INPUT.json`: validates the input
/// contract, recommends a model and API path per workload, validates the
/// result contract, writes `model-recommendation.json` (or `--output`) and
/// prints `RESULT=ok WORKLOADS=<n> SCHEMA_VALIDATED=yes`. A stale
/// `model-verification.json` beside the result is removed when the invocation
/// contract changed.
///
/// # Errors
///
/// A usage error (exit status 2) without the input file; a runtime error (exit
/// status 1) when the input or a catalog cannot be read, is malformed or does
/// not satisfy its contract, no catalog entry satisfies a workload, or the
/// result cannot be written.
pub fn model_recommend_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(input_arg) = invocation.positional(0) else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "usage: toolkit startup model-recommend INPUT.json [--output FILE] [--catalog FILE] \
[--openai-catalog FILE] [--plugin-root DIR]",
        ));
    };
    let input_path = Path::new(input_arg);
    let document = read_json_file(input_path)?;
    check_schema(
        INPUT_SCHEMA,
        &document,
        &format!(
            "{}: the input does not satisfy the model-recommendation-input schema",
            safe_path(input_arg)
        ),
        output,
    )?;
    let input = Input::from_json(&document)?;
    let catalog = load_anthropic_catalog(&catalog_path(invocation, "--catalog", DEFAULT_CATALOG))?;
    let openai_path = catalog_path(invocation, "--openai-catalog", OPENAI_CATALOG);
    let result = recommend(&input, &catalog, &mut || -> Res<Catalog> {
        load_openai_catalog(&openai_path)
    })?;
    check_schema(
        OUTPUT_SCHEMA,
        &result,
        "internal error: the recommendation does not satisfy the model-recommendation schema",
        output,
    )?;
    let output_path = invocation.value("--output").map_or_else(
        || {
            input_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(RESULT_FILENAME)
        },
        PathBuf::from,
    );
    drop_stale_verification(&output_path, &result)?;
    write_regular_file(&output_path, &format!("{}\n", dumps_indent2(&result))).map_err(
        |error| {
            ToolkitError::runtime(
                "startup_models_write",
                format!(
                    "cannot write {}: {}",
                    safe_path(&output_path.display().to_string()),
                    read_failure(&error)
                ),
            )
        },
    )?;
    output.line(&format!(
        "RESULT=ok WORKLOADS={} SCHEMA_VALIDATED=yes",
        input.workloads.len()
    ))
}

/// The schema findings of a result document as printed lines, for the tests of
/// the output contract.
#[cfg(test)]
pub(crate) fn check_schema_for_tests(result: &Json) -> Vec<String> {
    let mut sink = Vec::new();
    let mut diagnostics = Vec::new();
    let mut output = Output::with_diagnostics(&mut sink, &mut diagnostics);
    let _ = check_schema(OUTPUT_SCHEMA, result, "invalid", &mut output);
    String::from_utf8(diagnostics)
        .expect("utf-8 diagnostics")
        .lines()
        .map(str::to_string)
        .collect()
}
