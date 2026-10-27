//! `filter_deployable_models.py`: constraint matching and the deterministic
//! newest-first order.

use toolkit_core::Json;

use super::normalize::{list_field, text_field};

/// Constraint keys, in the order the Python script lists them.
pub(super) const VALID_KEYS: [&str; 13] = [
    "task",
    "data_type",
    "input_modality",
    "output_modality",
    "license",
    "size",
    "context_window",
    "language",
    "model_type",
    "provider",
    "bedrock",
    "customization",
    "name",
];

/// Constraints in first-seen key order; the values of one key are OR-ed, the
/// keys are AND-ed.
pub(super) type Constraints = Vec<(String, Vec<String>)>;

/// Adds `value` under `key`, keeping first-seen key order.
pub(super) fn add_constraint(constraints: &mut Constraints, key: &str, value: String) {
    if let Some((_, values)) = constraints.iter_mut().find(|(name, _)| name == key) {
        values.push(value);
    } else {
        constraints.push((key.to_string(), vec![value]));
    }
}

/// The constraints as the JSON object `filters_applied` prints.
pub(super) fn constraints_json(constraints: &Constraints) -> Json {
    Json::Object(
        constraints
            .iter()
            .map(|(key, values)| {
                (
                    key.clone(),
                    Json::Array(values.iter().map(|v| Json::string(v.as_str())).collect()),
                )
            })
            .collect(),
    )
}

/// Rejects a `bedrock` / `customization` value other than `true` or `false`.
/// The Python script ignored such a constraint and kept every model; here an
/// ambiguous request fails closed.
fn check_boolean_constraints(constraints: &Constraints) -> Result<(), String> {
    for (key, values) in constraints {
        if matches!(key.as_str(), "bedrock" | "customization")
            && values.last().is_some_and(|v| v != "true" && v != "false")
        {
            return Err(format!("constraint {key} must be true or false"));
        }
    }
    Ok(())
}

fn any_contains(values: &[String], items: &[&str]) -> bool {
    values.iter().any(|v| {
        items
            .iter()
            .any(|item| item.to_lowercase().contains(v.as_str()))
    })
}

fn flag_value(model: &Json, field: &str) -> Result<Option<bool>, String> {
    match model.get(field) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(format!("{field} must be true or false")),
    }
}

/// Whether `model` matches every constraint. A model without the metadata a
/// constraint filters on is excluded, like one that positively mismatches.
///
/// # Errors
///
/// A description when a member the constraints read has the wrong type.
pub(super) fn matches(model: &Json, constraints: &Constraints) -> Result<bool, String> {
    for (key, values) in constraints {
        let lists = match key.as_str() {
            "task" => Some("tasks"),
            "data_type" => Some("data_types"),
            "input_modality" => Some("input_modalities"),
            "output_modality" => Some("output_modalities"),
            "language" => Some("languages"),
            _ => None,
        };
        if let Some(field) = lists {
            let Some(items) = list_field(model, field)? else {
                return Ok(false);
            };
            if !any_contains(values, &items) {
                return Ok(false);
            }
            continue;
        }
        match key.as_str() {
            "license" | "provider" => {
                let field = key.as_str();
                let Some(text) = text_field(model, field)? else {
                    return Ok(false);
                };
                if !any_contains(values, &[text]) {
                    return Ok(false);
                }
            }
            "size" | "context_window" | "model_type" => {
                let Some(text) = text_field(model, key)? else {
                    return Ok(false);
                };
                let text = text.to_lowercase();
                if !values.contains(&text) {
                    return Ok(false);
                }
            }
            "bedrock" | "customization" => {
                let field = if key == "bedrock" {
                    "bedrock_eligible"
                } else {
                    "customization_eligible"
                };
                let Some(eligible) = flag_value(model, field)? else {
                    return Ok(false);
                };
                // The last value wins: true/false is binary.
                match values.last().map(String::as_str) {
                    Some("true") if !eligible => return Ok(false),
                    Some("false") if eligible => return Ok(false),
                    _ => {}
                }
            }
            "name" => {
                let name = text_field(model, "name")?.unwrap_or("").to_lowercase();
                if !values.iter().any(|v| name.contains(v.as_str())) {
                    return Ok(false);
                }
            }
            _ => {}
        }
    }
    Ok(true)
}

fn name_key(model: &Json) -> String {
    model
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_lowercase()
}

/// The models sorted newest first: dated models by `original_creation_time`
/// descending, then undated ones; ties and the undated group by name,
/// case-insensitive ascending. The input is not changed.
///
/// # Errors
///
/// A description when a creation time is not a string.
pub(super) fn sort_models_newest_first(models: &[Json]) -> Result<Vec<Json>, String> {
    let mut dated: Vec<(&Json, &str)> = Vec::new();
    let mut undated: Vec<&Json> = Vec::new();
    for model in models {
        match model.get("original_creation_time") {
            None | Some(Json::Null) => undated.push(model),
            Some(Json::String(time)) => dated.push((model, time)),
            Some(_) => return Err("original_creation_time must be a string".to_string()),
        }
    }
    // Two stable passes: the name tie-break first, then time descending.
    dated.sort_by_key(|(model, _)| name_key(model));
    dated.sort_by(|left, right| right.1.cmp(left.1));
    undated.sort_by_key(|model| name_key(model));
    Ok(dated
        .into_iter()
        .map(|(model, _)| model)
        .chain(undated)
        .cloned()
        .collect())
}

/// Applies every constraint and returns the deterministic order.
///
/// # Errors
///
/// A description (with the model's position) for a mistyped member or an
/// ambiguous boolean constraint.
pub(super) fn filter_models(
    models: &[Json],
    constraints: &Constraints,
) -> Result<Vec<Json>, String> {
    check_boolean_constraints(constraints)?;
    let mut kept = Vec::new();
    for (index, model) in models.iter().enumerate() {
        if matches(model, constraints).map_err(|message| format!("model {index}: {message}"))? {
            kept.push(model.clone());
        }
    }
    sort_models_newest_first(&kept)
}
