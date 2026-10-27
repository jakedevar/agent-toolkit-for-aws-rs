//! The typed view of a recommendation input (`model-recommendation-input.json`).
//!
//! The document is checked against the shipped JSON Schema before it gets
//! here; this module only reads the checked shape, and still reports a
//! mismatch (naming the field) rather than assuming it.

use toolkit_core::Json;

use super::{ModelError, Res};

/// A workload `source`. `json` is the original object, which the result echoes
/// with its member order.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub(crate) json: Json,
    pub(crate) provider: String,
    pub(crate) model_ids: Vec<String>,
    pub(crate) api_surface: String,
}

/// A workload's `requirements` (every optional key is `None` when absent).
#[derive(Debug, Clone, Default)]
pub(crate) struct Requirements {
    pub(crate) priority: String,
    pub(crate) critical_features: Vec<String>,
    pub(crate) preserve_messages_api: Option<bool>,
    pub(crate) newest_anthropic_betas: Option<bool>,
    pub(crate) api_continuity: Option<String>,
    pub(crate) preserve_openai_api: Option<bool>,
    pub(crate) uses_n: Option<bool>,
    pub(crate) uses_hosted_state: Option<bool>,
    pub(crate) governance: Vec<String>,
    pub(crate) multi_model_converse: Option<bool>,
    pub(crate) requires_native_payload: Option<bool>,
    pub(crate) preferred_api_path: Option<String>,
    pub(crate) min_context_tokens: Option<i64>,
    pub(crate) expected_output_tokens: Option<i64>,
    pub(crate) thinking_enabled: Option<bool>,
    pub(crate) data_residency: Option<String>,
    pub(crate) cris_geography: Option<String>,
    pub(crate) inference_profile_id: Option<String>,
}

impl Requirements {
    pub(crate) fn has_critical(&self, feature: &str) -> bool {
        self.critical_features.iter().any(|known| known == feature)
    }
}

/// One workload.
#[derive(Debug, Clone)]
pub(crate) struct Workload {
    pub(crate) id: String,
    pub(crate) source: Source,
    pub(crate) requirements: Requirements,
    pub(crate) detected_features: Vec<String>,
    pub(crate) feature_status: Vec<(String, String)>,
}

/// The whole input.
#[derive(Debug, Clone)]
pub(crate) struct Input {
    pub(crate) region: String,
    pub(crate) primary_unit: String,
    pub(crate) workloads: Vec<Workload>,
}

fn mismatch(field: &str) -> ModelError {
    ModelError::new(format!(
        "the input field {field} does not have its schema shape"
    ))
}

fn string(object: &Json, key: &str, owner: &str) -> Res<String> {
    match object.get(key) {
        Some(Json::String(text)) => Ok(text.clone()),
        _ => Err(mismatch(&format!("{owner}.{key}"))),
    }
}

fn optional_string(object: &Json, key: &str, owner: &str) -> Res<Option<String>> {
    match object.get(key) {
        None => Ok(None),
        Some(Json::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(mismatch(&format!("{owner}.{key}"))),
    }
}

fn flag(object: &Json, key: &str, owner: &str) -> Res<Option<bool>> {
    match object.get(key) {
        None => Ok(None),
        Some(Json::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(mismatch(&format!("{owner}.{key}"))),
    }
}

/// A JSON integer, or a float with an integral value (the schema's `integer`).
fn integer(object: &Json, key: &str, owner: &str) -> Res<Option<i64>> {
    match object.get(key) {
        None => Ok(None),
        Some(Json::Int(number)) => Ok(Some(*number)),
        #[allow(clippy::cast_possible_truncation)]
        Some(Json::Float(number)) if number.fract() == 0.0 && number.abs() < 9.0e15 => {
            Ok(Some(*number as i64))
        }
        Some(_) => Err(mismatch(&format!("{owner}.{key}"))),
    }
}

fn strings(object: &Json, key: &str, owner: &str) -> Res<Vec<String>> {
    match object.get(key) {
        None => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| match item {
                Json::String(text) => Ok(text.clone()),
                _ => Err(mismatch(&format!("{owner}.{key}"))),
            })
            .collect(),
        Some(_) => Err(mismatch(&format!("{owner}.{key}"))),
    }
}

fn source(value: &Json, owner: &str) -> Res<Source> {
    let owner = format!("{owner}.source");
    Ok(Source {
        json: value.clone(),
        provider: string(value, "provider", &owner)?,
        model_ids: strings(value, "model_ids", &owner)?,
        api_surface: string(value, "api_surface", &owner)?,
    })
}

fn requirements(value: &Json, owner: &str) -> Res<Requirements> {
    let owner = format!("{owner}.requirements");
    Ok(Requirements {
        priority: string(value, "priority", &owner)?,
        critical_features: strings(value, "critical_features", &owner)?,
        preserve_messages_api: flag(value, "preserve_messages_api", &owner)?,
        newest_anthropic_betas: flag(value, "newest_anthropic_betas", &owner)?,
        api_continuity: optional_string(value, "api_continuity", &owner)?,
        preserve_openai_api: flag(value, "preserve_openai_api", &owner)?,
        uses_n: flag(value, "uses_n", &owner)?,
        uses_hosted_state: flag(value, "uses_hosted_state", &owner)?,
        governance: strings(value, "governance", &owner)?,
        multi_model_converse: flag(value, "multi_model_converse", &owner)?,
        requires_native_payload: flag(value, "requires_native_payload", &owner)?,
        preferred_api_path: optional_string(value, "preferred_api_path", &owner)?,
        min_context_tokens: integer(value, "min_context_tokens", &owner)?,
        expected_output_tokens: integer(value, "expected_output_tokens", &owner)?,
        thinking_enabled: flag(value, "thinking_enabled", &owner)?,
        data_residency: optional_string(value, "data_residency", &owner)?,
        cris_geography: optional_string(value, "cris_geography", &owner)?,
        inference_profile_id: optional_string(value, "inference_profile_id", &owner)?,
    })
}

fn workload(value: &Json, index: usize) -> Res<Workload> {
    let owner = format!("workloads[{index}]");
    let feature_status = match value.get("feature_status") {
        None => Vec::new(),
        Some(Json::Object(pairs)) => pairs
            .iter()
            .map(|(feature, status)| match status {
                Json::String(text) => Ok((feature.clone(), text.clone())),
                _ => Err(mismatch(&format!("{owner}.feature_status"))),
            })
            .collect::<Res<Vec<_>>>()?,
        Some(_) => return Err(mismatch(&format!("{owner}.feature_status"))),
    };
    Ok(Workload {
        id: string(value, "workload_id", &owner)?,
        source: source(value.get("source").ok_or_else(|| mismatch(&owner))?, &owner)?,
        requirements: requirements(
            value.get("requirements").ok_or_else(|| mismatch(&owner))?,
            &owner,
        )?,
        detected_features: strings(value, "detected_features", &owner)?,
        feature_status,
    })
}

impl Input {
    /// Reads a document that already passed the input schema.
    ///
    /// # Errors
    ///
    /// A field that does not have the shape the schema promises.
    pub(crate) fn from_json(document: &Json) -> Res<Self> {
        let workloads = match document.get("workloads") {
            Some(Json::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(index, item)| workload(item, index))
                .collect::<Res<Vec<_>>>()?,
            _ => return Err(mismatch("workloads")),
        };
        Ok(Self {
            region: string(document, "region", "the input")?,
            primary_unit: string(document, "primary_unit", "the input")?,
            workloads,
        })
    }
}
