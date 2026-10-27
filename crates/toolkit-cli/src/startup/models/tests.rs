//! Offline, deterministic tests of `toolkit startup model-recommend` (#4401).
//!
//! The Python tests (`test_model_recommendation.py`,
//! `test_openai_model_recommendation.py`) are ported scenario by scenario; the
//! catalogs, schemas and phase documents are the real files of the plugin tree.
//! The Python tests that exercised `jsonschema` now run the shipped schemas
//! through the shared schema engine inside the command, so every CLI test is
//! also a schema test.

use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, Json};

use super::catalog::Catalog;
use super::request::Input;
use super::{load_anthropic_catalog, load_openai_catalog, recommend, ModelError, Res};
use crate::startup::scoring::fixtures::real_skill;

mod anthropic;
mod cli;
mod hostile;
mod openai;

pub(super) const HOSTILE: &str = "\u{1b}[31mAKIAFAKESECRETMARKER\u{7}\r\nforged: ok\u{202e}";

pub(super) fn j(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

pub(super) fn models_dir() -> PathBuf {
    real_skill().join("references").join("models")
}

pub(super) fn plugin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

pub(super) fn anthropic_catalog() -> Catalog {
    load_anthropic_catalog(&models_dir().join(super::DEFAULT_CATALOG)).expect("real catalog")
}

pub(super) fn openai_catalog() -> Catalog {
    load_openai_catalog(&models_dir().join(super::OPENAI_CATALOG)).expect("real catalog")
}

/// Shallow-merges `overrides` into `base`; `source` and `requirements` merge
/// one level deeper like the Python `_workload` helpers.
pub(super) fn merged(base: &str, overrides: &str) -> Json {
    let mut workload = j(base);
    let Json::Object(changes) = j(overrides) else {
        panic!("overrides are an object");
    };
    for (key, value) in changes {
        let Json::Object(members) = &mut workload else {
            panic!("workload is an object");
        };
        let slot = members.iter_mut().find(|(name, _)| *name == key);
        match (slot, value) {
            (Some((_, Json::Object(existing))), Json::Object(extra))
                if key == "source" || key == "requirements" =>
            {
                for (inner, inner_value) in extra {
                    match existing.iter_mut().find(|(name, _)| *name == inner) {
                        Some(entry) => entry.1 = inner_value,
                        None => existing.push((inner, inner_value)),
                    }
                }
            }
            (Some(entry), value) => entry.1 = value,
            (None, value) => members.push((key, value)),
        }
    }
    workload
}

/// The input document around `workload` (region `us-east-1`).
pub(super) fn input_with(workload: &Json, region: &str) -> Json {
    let id = workload
        .get("workload_id")
        .and_then(Json::as_str)
        .expect("workload id")
        .to_string();
    Json::object(vec![
        ("schema_version", Json::Int(2)),
        ("region", Json::string(region)),
        ("primary_unit", Json::string(id)),
        ("workloads", Json::Array(vec![workload.clone()])),
    ])
}

pub(super) fn recommend_input(document: &Json, openai: bool) -> Res<Json> {
    let input = Input::from_json(document)?;
    let anthropic = anthropic_catalog();
    recommend(&input, &anthropic, &mut || -> Res<Catalog> {
        if openai {
            Ok(openai_catalog())
        } else {
            Err(ModelError::new("the OpenAI catalog was not expected"))
        }
    })
}

pub(super) fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing member {key}"))
}

pub(super) fn text<'a>(value: &'a Json, key: &str) -> &'a str {
    get(value, key)
        .as_str()
        .unwrap_or_else(|| panic!("{key} is not a string"))
}

pub(super) fn is_null(value: &Json, key: &str) -> bool {
    matches!(get(value, key), Json::Null)
}

/// The `code` members of a findings/deltas array.
pub(super) fn codes(value: &Json, key: &str) -> Vec<String> {
    match get(value, key) {
        Json::Array(items) => items
            .iter()
            .map(|item| text(item, "code").to_string())
            .collect(),
        other => panic!("{key} is a {}", other.type_name()),
    }
}

pub(super) fn has_code(value: &Json, key: &str, code: &str) -> bool {
    codes(value, key).iter().any(|known| known == code)
}

/// The finding or delta with `code`.
pub(super) fn entry<'a>(value: &'a Json, key: &str, code: &str) -> &'a Json {
    match get(value, key) {
        Json::Array(items) => items
            .iter()
            .find(|item| text(item, "code") == code)
            .unwrap_or_else(|| panic!("no {code} in {key}")),
        other => panic!("{key} is a {}", other.type_name()),
    }
}

pub(super) fn strings(value: &Json, key: &str) -> Vec<String> {
    match get(value, key) {
        Json::Array(items) => items
            .iter()
            .map(|item| item.as_str().expect("string item").to_string())
            .collect(),
        other => panic!("{key} is a {}", other.type_name()),
    }
}

pub(super) fn array_len(value: &Json, key: &str) -> usize {
    match get(value, key) {
        Json::Array(items) => items.len(),
        other => panic!("{key} is a {}", other.type_name()),
    }
}
