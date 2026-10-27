//! Normalization of `SageMaker` Hub content into the skill's model shape
//! (`get_deployable_models.py`: `normalize_size_bucket`, `normalize_hub_content`,
//! `list_available_values`).
//!
//! A model is a JSON object whose members keep the insertion order of the
//! Python dict, so `json.dumps` of the result is byte-identical.

use toolkit_aws::recorded::optional_string_list;
use toolkit_core::{parse_json, Json};
use toolkit_msk::pyfmt::py_float_parse;

use super::timeutil::epoch_to_iso;
use crate::dynamodb::pyfmt;
use crate::manifests::py_strip;

/// Canonical, non-overlapping size buckets (`SIZE_BUCKETS`).
pub(super) const SIZE_BUCKETS: [&str; 5] = ["<=10b", "11b-70b", "71b-100b", ">100b", "unknown"];

/// The keyword that marks a model deployable from the Bedrock console.
const BEDROCK_KEYWORD: &str = "@capability:bedrock_console";

/// The keyword that marks a model as fine-tunable.
pub(super) const CUSTOMIZATION_KEYWORD: &str = "@capability:customization";

fn bucket_for(billions: f64) -> &'static str {
    if billions <= 10.0 {
        "<=10b"
    } else if billions <= 70.0 {
        "11b-70b"
    } else if billions <= 100.0 {
        "71b-100b"
    } else {
        ">100b"
    }
}

/// `_parse_billions`: `7b`, `550b` or `10` as a number of billions.
fn parse_billions(token: &str) -> Option<f64> {
    if token.is_empty() {
        return None;
    }
    let lowered = py_strip(token).to_lowercase();
    py_float_parse(py_strip(lowered.trim_end_matches('b')))
}

/// Maps a raw `@model-size` value to a canonical bucket: exact counts and the
/// Hub's range labels are classified by their upper bound, `>Nb` is the
/// largest bucket, anything unparseable is `unknown`.
pub(super) fn normalize_size_bucket(raw: Option<&str>) -> &'static str {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return "unknown";
    };
    let value = py_strip(raw).to_lowercase();
    if value == "unknown" {
        return "unknown";
    }
    if value.starts_with('>') {
        return ">100b";
    }
    if let Some(rest) = value.strip_prefix('<') {
        return parse_billions(rest).map_or("unknown", bucket_for);
    }
    if let Some((_, upper)) = value.split_once('-') {
        return parse_billions(upper).map_or("unknown", bucket_for);
    }
    parse_billions(&value).map_or("unknown", bucket_for)
}

/// An insertion-ordered model under construction.
struct Entry(Vec<(String, Json)>);

impl Entry {
    /// `entry[key] = value`: an existing key keeps its position.
    fn set(&mut self, key: &str, value: Json) {
        if let Some(slot) = self.0.iter_mut().find(|(name, _)| name == key) {
            slot.1 = value;
        } else {
            self.0.push((key.to_string(), value));
        }
    }

    /// `entry.setdefault(key, []).append(item)`.
    // recorded-shape: the normalized model entry this command is building, not a response.
    fn push_item(&mut self, key: &str, item: &str) {
        if let Some((_, Json::Array(items))) = self.0.iter_mut().find(|(name, _)| name == key) {
            items.push(Json::string(item));
        } else {
            self.0
                .push((key.to_string(), Json::Array(vec![Json::string(item)])));
        }
    }
}

/// The list-valued keywords: prefix and the model field they append to.
const LIST_KEYWORDS: [(&str, &str); 5] = [
    ("@data-type:", "data_types"),
    ("@input-modality:", "input_modalities"),
    ("@output-modality:", "output_modalities"),
    ("@language:", "languages"),
    ("@task:", "tasks"),
];

/// The scalar keywords: prefix and the model field they set.
const SCALAR_KEYWORDS: [(&str, &str); 5] = [
    ("@license:", "license"),
    ("@context-window:", "context_window"),
    ("@model-type:", "model_type"),
    ("@framework:", "framework"),
    ("@provider:", "provider"),
];

/// `HubContentSearchKeywords` of a Hub record: absent is empty; a present one
/// must be a list of strings (`null` is rejected).
///
/// # Errors
///
/// A description when the member is not a list of strings.
pub(super) fn keywords_of(record: &Json) -> Result<Vec<&str>, String> {
    optional_string_list(record, "HubContentSearchKeywords").map_err(String::from)
}

/// `_document_scalar`: one lower-cased string of a top-level field of the
/// record's `HubContentDocument`, or `None`.
// recorded-shape: port of `_document_scalar`: the embedded HubContentDocument is free-form
// text the service does not type, any shape but an object or list of strings yields None.
fn document_scalar(record: &Json, field: &str) -> Option<String> {
    let document = record.get("HubContentDocument")?.as_str()?;
    if document.is_empty() {
        return None;
    }
    let parsed = parse_json(document).ok()?;
    let Json::Object(_) = parsed else {
        return None;
    };
    let value = match parsed.get(field)? {
        Json::Array(items) => items.iter().find_map(|item| match item {
            Json::String(text) if !text.is_empty() => Some(text.as_str()),
            _ => None,
        }),
        Json::String(text) if !text.is_empty() => Some(text.as_str()),
        _ => None,
    }?;
    Some(value.to_lowercase())
}

#[allow(clippy::cast_precision_loss)]
/// The `original_creation_time` text of a Hub record, if it has a truthy one:
/// strings pass through, epoch seconds become `isoformat()` of the UTC instant.
// recorded-shape: Python-truthiness port of `OriginalCreationTime`; every JSON type is
// classified here (none is defaulted), the value is not a typed response list.
fn original_creation_time(record: &Json) -> Option<String> {
    match record.get("OriginalCreationTime")? {
        Json::Null | Json::Bool(false) | Json::Int(0) => None,
        Json::String(text) if text.is_empty() => None,
        Json::Array(items) if items.is_empty() => None,
        Json::Object(members) if members.is_empty() => None,
        Json::Float(seconds) if *seconds == 0.0 => None,
        Json::String(text) => Some(text.clone()),
        Json::Int(seconds) => Some(epoch_to_iso(*seconds as f64)),
        Json::Float(seconds) => Some(epoch_to_iso(*seconds)),
        other => Some(pyfmt::text(other)),
    }
}

/// Converts a List or Search `HubContent` record to the skill's normalized shape.
///
/// # Errors
///
/// A description when the record's keywords are not a list of strings.
pub(super) fn normalize_hub_content(record: &Json) -> Result<Json, String> {
    let keywords = keywords_of(record)?;
    let mut entry = Entry(vec![(
        "name".to_string(),
        record.get("HubContentName").cloned().unwrap_or(Json::Null),
    )]);
    for keyword in &keywords {
        if let Some(raw_size) = keyword.strip_prefix("@model-size:") {
            // The raw value is kept for display; the bucket is what filters use.
            entry.set("size_raw", Json::string(raw_size));
            entry.set("size", Json::string(normalize_size_bucket(Some(raw_size))));
        } else if let Some((prefix, field)) = LIST_KEYWORDS
            .iter()
            .find(|(prefix, _)| keyword.starts_with(prefix))
        {
            entry.push_item(field, &keyword[prefix.len()..]);
        } else if let Some((prefix, field)) = SCALAR_KEYWORDS
            .iter()
            .find(|(prefix, _)| keyword.starts_with(prefix))
        {
            entry.set(field, Json::string(&keyword[prefix.len()..]));
        }
    }
    entry.set(
        "bedrock_eligible",
        Json::Bool(keywords.contains(&BEDROCK_KEYWORD)),
    );
    entry.set(
        "customization_eligible",
        Json::Bool(keywords.contains(&CUSTOMIZATION_KEYWORD)),
    );

    // A keyword, when present, is authoritative; the document only fills what
    // the Hub's 50-keyword cap dropped.
    if !keywords
        .iter()
        .any(|keyword| keyword.starts_with("@model-size:"))
    {
        if let Some(value) = document_scalar(record, "ModelSize") {
            entry.set("size_raw", Json::string(value.as_str()));
            entry.set(
                "size",
                Json::string(normalize_size_bucket(Some(value.as_str()))),
            );
        }
    }
    if !keywords
        .iter()
        .any(|keyword| keyword.starts_with("@context-window:"))
    {
        if let Some(value) = document_scalar(record, "ContextWindow") {
            entry.set("context_window", Json::string(value));
        }
    }
    if let Some(time) = original_creation_time(record) {
        entry.set("original_creation_time", Json::string(time));
    }
    Ok(Json::Object(entry.0))
}

/// A string member of a model: `None` when absent or null.
///
/// # Errors
///
/// A description when the member is not a string.
pub(super) fn text_field<'a>(model: &'a Json, field: &str) -> Result<Option<&'a str>, String> {
    match model.get(field) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{field} must be a string")),
    }
}

/// A list-of-strings member of a model: `None` when absent or null.
///
/// # Errors
///
/// A description when the member is not a list of strings.
pub(super) fn list_field<'a>(model: &'a Json, field: &str) -> Result<Option<Vec<&'a str>>, String> {
    match model.get(field) {
        None | Some(Json::Null) => Ok(None),
        // recorded-shape: a normalized model member (`null` is its documented
        // absent value), not a response.
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| format!("{field} must be a list of strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{field} must be a list of strings")),
    }
}

/// `list_available_values`: every distinct value of each metadata field,
/// sorted.
///
/// # Errors
///
/// A description (with the model's position) for a mistyped member.
pub(super) fn list_available_values(models: &[Json]) -> Result<Json, String> {
    const LISTS: [(&str, &str); 7] = [
        ("tasks", "tasks"),
        ("data_types", "data_types"),
        ("input_modalities", "input_modalities"),
        ("output_modalities", "output_modalities"),
        ("languages", "languages"),
        ("sizes", "size"),
        ("licenses", "license"),
    ];
    const SCALARS: [(&str, &str); 4] = [
        ("context_windows", "context_window"),
        ("model_types", "model_type"),
        ("providers", "provider"),
        ("frameworks", "framework"),
    ];
    let mut sets: Vec<(&str, std::collections::BTreeSet<String>)> = [
        "tasks",
        "data_types",
        "input_modalities",
        "output_modalities",
        "sizes",
        "licenses",
        "languages",
        "context_windows",
        "model_types",
        "providers",
        "frameworks",
    ]
    .iter()
    .map(|name| (*name, std::collections::BTreeSet::new()))
    .collect();
    for (index, model) in models.iter().enumerate() {
        let context = |message: String| format!("model {index}: {message}");
        for (output, field) in LISTS {
            let values: Vec<&str> = if matches!(output, "sizes" | "licenses") {
                text_field(model, field)
                    .map_err(context)?
                    .into_iter()
                    .collect()
            } else {
                list_field(model, field)
                    .map_err(context)?
                    .unwrap_or_default()
            };
            if let Some((_, set)) = sets.iter_mut().find(|(name, _)| *name == output) {
                set.extend(values.into_iter().map(str::to_string));
            }
        }
        for (output, field) in SCALARS {
            if let Some(value) = text_field(model, field).map_err(context)? {
                if let Some((_, set)) = sets.iter_mut().find(|(name, _)| *name == output) {
                    set.insert(value.to_string());
                }
            }
        }
    }
    Ok(Json::Object(
        sets.into_iter()
            .map(|(name, set)| {
                (
                    name.to_string(),
                    Json::Array(set.into_iter().map(Json::String).collect()),
                )
            })
            .collect(),
    ))
}
