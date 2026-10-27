//! The dated per-provider model catalogs (`references/models/*.json`).
//!
//! `load_catalog` and `load_openai_catalog` of the Python orchestrator checked
//! a handful of keys and left every other access to crash with a `KeyError`.
//! The port reads each catalog into typed values once and reports, with the
//! field and the escaped file name, anything the engines would need but the
//! file does not have. Model, path and region names are data of the file and
//! are printed through `safe_text`.

use std::path::Path;

use toolkit_core::{parse_json, Json};

use super::{ModelError, Res};
use crate::safe_text::{safe_path, safe_text};
use crate::startup::read_text;

/// A numeric catalog limit: a positive integer or the string `"unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Limit {
    Known(i64),
    Unknown,
}

impl Limit {
    pub(crate) fn json(self) -> Json {
        match self {
            Self::Known(number) => Json::Int(number),
            Self::Unknown => Json::string("unknown"),
        }
    }

    /// Whether a hard requirement of `need` tokens exceeds this limit. An
    /// unknown limit cannot confirm a positive need.
    pub(crate) fn below(self, need: i64) -> bool {
        match self {
            Self::Known(number) => number < need,
            Self::Unknown => need > 0,
        }
    }
}

/// One `models.<key>.paths.<path>` entry.
#[derive(Debug, Clone, Default)]
pub(crate) struct PathConfig {
    pub(crate) available: bool,
    pub(crate) model_id: String,
    pub(crate) requires_cris: bool,
    /// `regions` (Anthropic catalog).
    pub(crate) regions: Option<Vec<String>>,
    /// `supported_regions` (`OpenAI` catalog).
    pub(crate) supported_regions: Option<Vec<String>>,
    pub(crate) inference_profile_regions: Option<Vec<(String, Vec<String>)>>,
    /// Path-level capability evidence that replaces the model's.
    pub(crate) capabilities: Option<Vec<String>>,
}

/// One `models.<key>` entry.
#[derive(Debug, Clone)]
pub(crate) struct Model {
    pub(crate) display_name: String,
    pub(crate) family: String,
    pub(crate) version: String,
    pub(crate) generation: Option<String>,
    pub(crate) context_window: Limit,
    pub(crate) output_token_ceiling: Limit,
    pub(crate) capabilities: Vec<String>,
    pub(crate) paths: Vec<(String, PathConfig)>,
    pub(crate) inference_profiles: Vec<(String, Vec<String>)>,
    pub(crate) runtime_in_region_regions: Vec<String>,
    /// `region_availability.commercial` and `.govcloud`, when the object is
    /// present and not empty.
    pub(crate) region_availability: Option<(Vec<String>, Vec<String>)>,
    pub(crate) adaptive_thinking_only: bool,
    pub(crate) batch_supported: Option<bool>,
    pub(crate) forced_tool_choice_supported: Option<bool>,
    pub(crate) sampling_parameters_supported: Option<bool>,
}

impl Model {
    pub(crate) fn path(&self, name: &str) -> Option<&PathConfig> {
        self.paths
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, config)| config)
    }

    pub(crate) fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|known| known == name)
    }
}

/// A loaded catalog.
#[derive(Debug, Clone)]
pub(crate) struct Catalog {
    pub(crate) provider: String,
    pub(crate) verified_at: String,
    pub(crate) verified_region: String,
    pub(crate) source: Option<String>,
    pub(crate) models: Vec<(String, Model)>,
}

impl Catalog {
    pub(crate) fn model(&self, key: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, model)| model)
    }

    /// The model `key` of a selection table, or the error for a catalog that
    /// lacks it.
    pub(crate) fn require(&self, key: &str) -> Res<&Model> {
        self.model(key).ok_or_else(|| {
            ModelError::new(format!(
                "catalog lacks the model {} the selection tables name",
                safe_text(key)
            ))
        })
    }
}

/// Reads the Anthropic catalog at `path`.
///
/// # Errors
///
/// An unreadable, linked or malformed file, or a missing required field.
pub(crate) fn load_anthropic_catalog(path: &Path) -> Res<Catalog> {
    load(path, false)
}

/// Reads the OpenAI/Bedrock path catalog at `path`, whose models also carry a
/// `generation`.
///
/// # Errors
///
/// As [`load_anthropic_catalog`].
pub(crate) fn load_openai_catalog(path: &Path) -> Res<Catalog> {
    load(path, true)
}

struct Reader {
    file: String,
}

impl Reader {
    fn fail(&self, message: impl AsRef<str>) -> ModelError {
        ModelError::new(format!("{}: {}", self.file, message.as_ref()))
    }

    fn string(&self, object: &Json, key: &str, owner: &str) -> Res<String> {
        match object.get(key) {
            Some(Json::String(text)) => Ok(text.clone()),
            Some(_) => Err(self.fail(format!("{owner} \"{key}\" must be a string"))),
            None => Err(self.fail(format!("{owner} lacks \"{key}\""))),
        }
    }

    fn optional_string(&self, object: &Json, key: &str, owner: &str) -> Res<Option<String>> {
        match object.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(Json::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(self.fail(format!("{owner} \"{key}\" must be a string or null"))),
        }
    }

    fn strings(&self, value: &Json, what: &str) -> Res<Vec<String>> {
        let Json::Array(items) = value else {
            return Err(self.fail(format!("{what} must be an array")));
        };
        items
            .iter()
            .map(|item| match item {
                Json::String(text) => Ok(text.clone()),
                _ => Err(self.fail(format!("{what} must hold only strings"))),
            })
            .collect()
    }

    fn optional_strings(&self, object: &Json, key: &str, owner: &str) -> Res<Option<Vec<String>>> {
        object
            .get(key)
            .map(|value| self.strings(value, &format!("{owner} \"{key}\"")))
            .transpose()
    }

    fn flag(&self, object: &Json, key: &str, owner: &str) -> Res<Option<bool>> {
        match object.get(key) {
            None => Ok(None),
            Some(Json::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => Err(self.fail(format!("{owner} \"{key}\" must be a boolean"))),
        }
    }

    fn members<'a>(&self, value: &'a Json, what: &str) -> Res<&'a [(String, Json)]> {
        let Json::Object(pairs) = value else {
            return Err(self.fail(format!("{what} must be an object")));
        };
        Ok(pairs)
    }

    fn lists(&self, value: &Json, what: &str) -> Res<Vec<(String, Vec<String>)>> {
        self.members(value, what)?
            .iter()
            .map(|(key, list)| {
                Ok((
                    key.clone(),
                    self.strings(list, &format!("{what} {}", safe_text(key)))?,
                ))
            })
            .collect()
    }

    fn limit(&self, model: &Json, key: &str, owner: &str) -> Res<Limit> {
        match model.get(key) {
            Some(Json::Int(number)) if *number > 0 => Ok(Limit::Known(*number)),
            Some(Json::String(text)) if text == "unknown" => Ok(Limit::Unknown),
            Some(_) => Err(self.fail(format!(
                "{owner} {key} must be a positive int or \"unknown\""
            ))),
            None => Err(self.fail(format!("{owner} missing {key}"))),
        }
    }

    fn path_config(&self, value: &Json, owner: &str) -> Res<PathConfig> {
        self.members(value, owner)?;
        let available = matches!(value.get("available"), Some(Json::Bool(true)));
        let model_id = if available || value.get("model_id").is_some() {
            self.string(value, "model_id", owner)?
        } else {
            String::new()
        };
        let requires_cris = if available || value.get("requires_cris").is_some() {
            self.flag(value, "requires_cris", owner)?
                .ok_or_else(|| self.fail(format!("{owner} lacks \"requires_cris\"")))?
        } else {
            false
        };
        let profile_regions = value
            .get("inference_profile_regions")
            .map(|regions| self.lists(regions, &format!("{owner} \"inference_profile_regions\"")))
            .transpose()?;
        Ok(PathConfig {
            available,
            model_id,
            requires_cris,
            regions: self.optional_strings(value, "regions", owner)?,
            supported_regions: self.optional_strings(value, "supported_regions", owner)?,
            inference_profile_regions: profile_regions,
            capabilities: self.optional_strings(value, "capabilities", owner)?,
        })
    }

    fn model(&self, key: &str, value: &Json, openai: bool) -> Res<Model> {
        let owner = format!("model {}", safe_text(key));
        self.members(value, &owner)?;
        let mut required = vec![
            "display_name",
            "family",
            "version",
            "context_window",
            "output_token_ceiling",
            "capabilities",
            "paths",
        ];
        if openai {
            required.insert(2, "generation");
        }
        for field in required {
            if value.get(field).is_none() {
                return Err(self.fail(format!("{} missing {field}", safe_text(key))));
            }
        }
        let paths = value.get("paths").unwrap_or(&Json::Null);
        let paths = self
            .members(paths, &format!("{owner} \"paths\""))?
            .iter()
            .map(|(name, config)| {
                Ok((
                    name.clone(),
                    self.path_config(config, &format!("{owner} path {}", safe_text(name)))?,
                ))
            })
            .collect::<Res<Vec<_>>>()?;
        let availability = match value.get("region_availability") {
            Some(object @ Json::Object(pairs)) if !pairs.is_empty() => {
                let part = |key: &str| -> Res<Vec<String>> {
                    object
                        .get(key)
                        .map(|list| {
                            self.strings(list, &format!("{owner} region_availability {key}"))
                        })
                        .transpose()
                        .map(Option::unwrap_or_default)
                };
                Some((part("commercial")?, part("govcloud")?))
            }
            None | Some(Json::Null | Json::Object(_)) => None,
            Some(_) => {
                return Err(self.fail(format!("{owner} \"region_availability\" must be an object")))
            }
        };
        Ok(Model {
            display_name: self.string(value, "display_name", &owner)?,
            family: self.string(value, "family", &owner)?,
            version: self.string(value, "version", &owner)?,
            generation: self.optional_string(value, "generation", &owner)?,
            context_window: self.limit(value, "context_window", &owner)?,
            output_token_ceiling: self.limit(value, "output_token_ceiling", &owner)?,
            capabilities: value
                .get("capabilities")
                .map(|list| self.strings(list, &format!("{owner} \"capabilities\"")))
                .transpose()?
                .unwrap_or_default(),
            paths,
            inference_profiles: value
                .get("inference_profiles")
                .map(|map| self.lists(map, &format!("{owner} \"inference_profiles\"")))
                .transpose()?
                .unwrap_or_default(),
            runtime_in_region_regions: self
                .optional_strings(value, "runtime_in_region_regions", &owner)?
                .unwrap_or_default(),
            region_availability: availability,
            adaptive_thinking_only: self
                .flag(value, "adaptive_thinking_only", &owner)?
                .unwrap_or(false),
            batch_supported: self.flag(value, "batch_supported", &owner)?,
            forced_tool_choice_supported: self.flag(
                value,
                "forced_tool_choice_supported",
                &owner,
            )?,
            sampling_parameters_supported: self.flag(
                value,
                "sampling_parameters_supported",
                &owner,
            )?,
        })
    }
}

fn load(path: &Path, openai: bool) -> Res<Catalog> {
    let reader = Reader {
        file: safe_path(&path.display().to_string()),
    };
    let document = read_json_file(path)?;
    if !matches!(document, Json::Object(_)) {
        return Err(reader.fail("the catalog must be a JSON object"));
    }
    let missing: Vec<&str> = [
        "schema_version",
        "provider",
        "verified_at",
        "verified_region",
        "paths",
        "models",
    ]
    .into_iter()
    .filter(|key| document.get(key).is_none())
    .collect();
    if !missing.is_empty() {
        return Err(reader.fail(format!("missing required keys {}", missing.join(", "))));
    }
    let models = reader
        .members(document.get("models").unwrap_or(&Json::Null), "\"models\"")?
        .iter()
        .map(|(key, value)| Ok((key.clone(), reader.model(key, value, openai)?)))
        .collect::<Res<Vec<_>>>()?;
    Ok(Catalog {
        provider: reader.string(&document, "provider", "the catalog")?,
        verified_at: reader.string(&document, "verified_at", "the catalog")?,
        verified_region: reader.string(&document, "verified_region", "the catalog")?,
        source: reader.optional_string(&document, "source", "the catalog")?,
        models,
    })
}

/// Reads and parses a JSON input file; the error text names the file through
/// `safe_path` and never repeats a value of the document.
///
/// # Errors
///
/// An unreadable, linked, oversized or invalid file.
pub(crate) fn read_json_file(path: &Path) -> Res<Json> {
    let label = safe_path(&path.display().to_string());
    let text = read_text(path, &label).map_err(ModelError::new)?;
    parse_json(&text).map_err(|error| ModelError::new(format!("{label}: invalid JSON ({error})")))
}
