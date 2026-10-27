//! `toolkit startup model-pricing`: every default Anthropic candidate model
//! must have a numeric, Active price row in each vendored Bedrock pricing
//! cache (port of `validate-model-pricing.py`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, read_regular_file, resolve_within, Json, ToolkitError};

use super::text::{fold, is_word};
use super::MAX_ARTIFACT_BYTES;
use crate::manifests::{py_repr, py_splitlines, py_strip, read_failure, universal_newlines};
use crate::{Invocation, Output};

const SELECTOR: &str = "skills/agent-advisor/scripts/model_recommendation.py";
const MODELS_DIR: &str = "skills/agent-advisor/references/models";
const CLOUDS: [&str; 2] = ["gcp-to-aws", "azure-to-aws"];
const SUCCESS: &str =
    "Model pricing coverage: all Anthropic candidates have numeric Active cache rows.";
const CATALOG_PREFIX: &str = "DEFAULT_CATALOG = MODELS_DIR / \"";
const REGION_PREFIXES: [&str; 5] = ["us.", "eu.", "au.", "jp.", "global."];

/// Handler for `startup model-pricing [--plugin-root DIR]` (default `.`).
///
/// # Errors
///
/// A runtime error (exit status 1) for an unreadable or malformed input and
/// for any candidate without a numeric Active price row.
pub fn pricing_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let root = PathBuf::from(invocation.value("--plugin-root").unwrap_or("."));
    let errors = validate_model_pricing(&root)
        .map_err(|message| ToolkitError::runtime("pricing_input_invalid", message))?;
    if errors.is_empty() {
        return output.line(SUCCESS);
    }
    for error in &errors {
        output.diagnostic(error);
    }
    Err(ToolkitError::runtime(
        "pricing_coverage_failed",
        format!("{} model pricing problem(s)", errors.len()),
    ))
}

/// Returns the findings, or the message of the failure that aborted the run
/// (the Python `except (OSError, ValueError, KeyError)` path, which discards
/// findings gathered so far).
pub(crate) fn validate_model_pricing(plugin_root: &Path) -> Result<Vec<String>, String> {
    let selector = read_in(plugin_root, SELECTOR)?;
    let name = default_catalog_name(&selector)
        .ok_or_else(|| "Cannot locate the default Anthropic catalog.".to_string())?;
    let catalog_rel = format!("{MODELS_DIR}/{name}");
    let catalog = parse_json(&read_in(plugin_root, &catalog_rel)?)
        .map_err(|error| format!("{catalog_rel}: {error}"))?;

    let mut errors = Vec::new();
    for cloud in CLOUDS {
        let cache_rel = format!("skills/{cloud}/references/vendored/ai/bedrock-pricing-cache.md");
        let priced = priced_models(&read_in(plugin_root, &cache_rel)?);
        let models = match index(&catalog, "models")? {
            Json::Object(models) => models,
            other => {
                return Err(format!(
                    "catalog 'models' is {}, not an object",
                    other.type_name()
                ))
            }
        };
        for (_, model) in models {
            let paths = index(model, "paths")?;
            let converse = index(paths, "runtime_converse")?;
            let model_id = index(converse, "model_id")?;
            let listed = match model_id {
                Json::String(text) => priced.contains(text.as_str()),
                Json::Array(_) | Json::Object(_) => {
                    return Err("unhashable model_id in the catalog".to_string())
                }
                _ => false,
            };
            if !listed {
                let shown = model_id
                    .as_str()
                    .map_or_else(|| py_repr(model_id), str::to_string);
                errors.push(format!(
                    "{cloud}: candidate {shown} has no numeric Active price row."
                ));
            }
        }
    }
    Ok(errors)
}

fn read_in(root: &Path, relative: &str) -> Result<String, String> {
    resolve_within(root, relative)
        .and_then(|path| read_regular_file(&path, MAX_ARTIFACT_BYTES))
        .map(|text| universal_newlines(&text))
        .map_err(|error| format!("cannot read {relative}: {}", read_failure(&error)))
}

/// `value[key]`: a missing key is Python's `KeyError` text (`'key'`), a
/// non-object is a shape error.
fn index<'a>(value: &'a Json, key: &str) -> Result<&'a Json, String> {
    match value {
        Json::Object(_) => value.get(key).ok_or_else(|| format!("'{key}'")),
        other => Err(format!(
            "catalog value for '{key}' is {}, not an object",
            other.type_name()
        )),
    }
}

/// The first line of `selector` that is
/// `DEFAULT_CATALOG = MODELS_DIR / "<name>"`, with a non-empty `<name>`.
pub(crate) fn default_catalog_name(selector: &str) -> Option<String> {
    std::iter::once(0)
        .chain(selector.match_indices('\n').map(|(index, _)| index + 1))
        .find_map(|start| {
            let after = selector[start..].strip_prefix(CATALOG_PREFIX)?;
            let end = after.find('"').filter(|end| *end > 0)?;
            Some(after[..end].to_string())
        })
}

/// Model ids (region prefix removed) with an Active status and positive,
/// finite input and output prices in the markdown table of one cache.
fn priced_models(cache: &str) -> HashSet<String> {
    let mut priced = HashSet::new();
    for line in py_splitlines(cache) {
        if !line.starts_with('|') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        let row: Vec<&str> = parts[1..parts.len() - 1]
            .iter()
            .map(|cell| py_strip(cell))
            .collect();
        if row.len() < 8 || !starts_with_active(row[7]) {
            continue;
        }
        if positive_rate(row[3]) && positive_rate(row[4]) {
            priced.insert(strip_region(row[1]).to_string());
        }
    }
    priced
}

/// `re.match(r"active\b", status, re.IGNORECASE)`.
pub(crate) fn starts_with_active(status: &str) -> bool {
    let mut chars = status.chars();
    for expected in "active".chars() {
        match chars.next() {
            Some(actual) if fold(actual) == expected => {}
            _ => return false,
        }
    }
    !chars.next().is_some_and(is_word)
}

/// `re.sub(r"^(us|eu|au|jp|global)\.", "", model_id)`.
pub(crate) fn strip_region(model_id: &str) -> &str {
    REGION_PREFIXES
        .iter()
        .find_map(|prefix| model_id.strip_prefix(prefix))
        .unwrap_or(model_id)
}

/// `math.isfinite(float(text)) and float(text) > 0`, where `float` rejects
/// unparsable text (the Python `except ValueError: continue`). Underscores
/// are accepted between digits, like Python.
pub(crate) fn positive_rate(text: &str) -> bool {
    let bytes = text.as_bytes();
    let underscores_ok = bytes.iter().enumerate().all(|(position, byte)| {
        *byte != b'_'
            || (position > 0
                && bytes[position - 1].is_ascii_digit()
                && bytes.get(position + 1).is_some_and(u8::is_ascii_digit))
    });
    underscores_ok
        && text
            .replace('_', "")
            .parse::<f64>()
            .is_ok_and(|rate| rate.is_finite() && rate > 0.0)
}
