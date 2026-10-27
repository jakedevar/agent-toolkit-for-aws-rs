//! Location sum type for dynamic instrumentation (`di_location.py`).
//!
//! A location is where in customer code an instrumentation configuration
//! applies. [`CodeLocation`] (language, file, class, method, line) is used
//! by BREAKPOINT and PROBE, [`HashLocation`] refers to an existing
//! configuration by its 16-character hash and [`UnknownLocation`] is the
//! forward-compatible fallback for a response union that matches nothing.
//!
//! Code-location fields hold JSON values (`null` is "absent"): a response
//! may carry any type, and the legacy renderers print whatever arrives with
//! `str()`. Input parsers always produce strings.

use toolkit_core::Json;

use super::pytext::{get_members, members, safe_json, safe_text};
use super::validation::{canonical_language, validate_location_inputs};
use crate::dynamodb::pyfmt;

const KNOWN_CODE_FIELDS: [&str; 6] = [
    "Language",
    "FilePath",
    "CodeUnit",
    "ClassName",
    "MethodName",
    "LineNumber",
];

/// A code-based instrumentation target (BREAKPOINT or PROBE).
///
/// Which fields identify the target depends on the language: Java uses
/// `code_unit` + `class_name` + `method_name`, Python `code_unit` +
/// `method_name` (`class_name` optional), JavaScript `file_path` +
/// `line_number`. A `line_number` makes any target line-level.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeLocation {
    /// `Language`.
    pub language: Json,
    /// `FilePath`.
    pub file_path: Json,
    /// `CodeUnit` (module or package).
    pub code_unit: Json,
    /// `ClassName`.
    pub class_name: Json,
    /// `MethodName`.
    pub method_name: Json,
    /// `LineNumber`.
    pub line_number: Json,
    /// Response fields outside the known set, rendered sorted by key.
    pub extra_fields: Vec<(String, Json)>,
}

fn opt_json(value: Option<&str>) -> Json {
    value.map_or(Json::Null, Json::string)
}

impl CodeLocation {
    /// Builds a location from request inputs.
    #[must_use]
    pub fn from_inputs(inputs: &LocationInputs<'_>, language: &str, file_path: &str) -> Self {
        Self {
            language: Json::string(language),
            file_path: Json::string(file_path),
            code_unit: opt_json(inputs.code_unit),
            class_name: opt_json(inputs.class_name),
            method_name: opt_json(inputs.method_name),
            line_number: inputs.line_number.map_or(Json::Null, Json::Int),
            extra_fields: Vec::new(),
        }
    }

    /// A one-line human description of the code target.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut target = if pyfmt::truthy(&self.file_path) {
            safe_json(&self.file_path)
        } else {
            "N/A".to_string()
        };
        if pyfmt::truthy(&self.class_name) {
            target.push_str(" :: ");
            target.push_str(&safe_json(&self.class_name));
        }
        if pyfmt::truthy(&self.method_name) {
            target.push('.');
            target.push_str(&safe_json(&self.method_name));
        }
        if self.line_number != Json::Null {
            target.push_str(":L");
            target.push_str(&safe_json(&self.line_number));
        }
        target
    }

    /// The breakpoint granularity.
    #[must_use]
    pub fn level(&self) -> String {
        if self.line_number == Json::Null {
            "FUNCTION/METHOD-LEVEL".to_string()
        } else {
            format!("LINE-LEVEL (L{})", safe_json(&self.line_number))
        }
    }

    /// The code location as labeled detail lines.
    ///
    /// Present-but-empty fields (`Language=""`) still render as
    /// `- Language: ` so a missing API field produces a visible blank.
    #[must_use]
    pub fn format_details(&self, location_hash: Option<&str>) -> String {
        let mut lines = vec!["- LocationKind: CODE".to_string()];
        if let Some(hash) = location_hash.filter(|hash| !hash.is_empty()) {
            lines.push(format!("- LocationHash: {}", safe_text(hash)));
        }
        let ordered = [
            ("Language", &self.language),
            ("File Path", &self.file_path),
            ("Code Unit", &self.code_unit),
            ("Class Name", &self.class_name),
            ("Method Name", &self.method_name),
            ("Line Number", &self.line_number),
        ];
        for (label, value) in ordered {
            if *value != Json::Null {
                lines.push(format!("- {label}: {}", safe_json(value)));
            }
        }
        let mut extras: Vec<&(String, Json)> = self.extra_fields.iter().collect();
        extras.sort_by(|left, right| left.0.cmp(&right.0));
        for (key, value) in extras {
            lines.push(format!("- {}: {}", safe_text(key), safe_json(value)));
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }

    /// The `CodeLocation` payload shared by create requests and lookups.
    #[must_use]
    pub fn to_code_location_payload(&self) -> Json {
        let mut payload: Vec<(String, Json)> = vec![
            ("Language".to_string(), self.language.clone()),
            ("FilePath".to_string(), self.file_path.clone()),
        ];
        let optional = [
            ("CodeUnit", &self.code_unit),
            ("ClassName", &self.class_name),
            ("MethodName", &self.method_name),
        ];
        for (key, value) in optional {
            if pyfmt::truthy(value) {
                payload.push((key.to_string(), value.clone()));
            }
        }
        if self.line_number != Json::Null {
            payload.push(("LineNumber".to_string(), self.line_number.clone()));
        }
        Json::Object(payload)
    }

    /// The create-request payload `{"CodeLocation": {...}}`.
    #[must_use]
    pub fn to_api_payload(&self) -> Json {
        Json::Object(vec![(
            "CodeLocation".to_string(),
            self.to_code_location_payload(),
        )])
    }

    /// The lookup identifier payload `{"CodeLocation": {...}}`.
    #[must_use]
    pub fn to_identifier(&self) -> Json {
        self.to_api_payload()
    }
}

/// An existing configuration referenced by its 16-character location hash.
///
/// Lookup-only: a hash cannot describe a new configuration, so it has no
/// create payload, and it has no fields beyond itself, so it has no detail
/// lines. Both are absent from the Rust type (the legacy methods only raised
/// `NotImplementedError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashLocation {
    /// The location hash.
    pub location_hash: String,
}

impl HashLocation {
    /// A one-line description naming the location hash.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("LocationHash {}", safe_text(&self.location_hash))
    }

    /// The `{"LocationHash": ...}` lookup identifier payload.
    #[must_use]
    pub fn to_identifier(&self) -> Json {
        Json::Object(vec![(
            "LocationHash".to_string(),
            Json::string(self.location_hash.as_str()),
        )])
    }
}

/// A location union the API returned that matches no known variant.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownLocation {
    /// The raw members of the union.
    pub raw: Vec<(String, Json)>,
}

impl UnknownLocation {
    /// `"N/A"`: an unknown location has no describable target.
    #[must_use]
    pub fn describe(&self) -> String {
        "N/A".to_string()
    }

    /// The raw fields as detail lines.
    #[must_use]
    pub fn format_details(&self, location_hash: Option<&str>) -> String {
        let mut lines = vec!["- LocationKind: UNKNOWN".to_string()];
        if let Some(hash) = location_hash.filter(|hash| !hash.is_empty()) {
            lines.push(format!("- LocationHash: {}", safe_text(hash)));
        }
        if self.raw.is_empty() {
            lines.push("- Location payload could not be parsed.".to_string());
        } else {
            let mut ordered: Vec<&(String, Json)> = self.raw.iter().collect();
            ordered.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, value) in ordered {
                lines.push(format!("- {}: {}", safe_text(key), safe_json(value)));
            }
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

/// Any location a renderer can meet.
#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    /// A code target.
    Code(CodeLocation),
    /// An existing configuration by hash.
    Hash(HashLocation),
    /// A response union that matched no variant.
    Unknown(UnknownLocation),
}

impl Location {
    /// A one-line human description.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Code(location) => location.describe(),
            Self::Hash(location) => location.describe(),
            Self::Unknown(location) => location.describe(),
        }
    }

    /// The granularity line, when the variant has one.
    #[must_use]
    pub fn level(&self) -> Option<String> {
        match self {
            Self::Code(location) => Some(location.level()),
            Self::Hash(_) | Self::Unknown(_) => None,
        }
    }
}

/// A location resolved from caller inputs. Unlike [`Location`] it never
/// holds an unknown variant, so `to_identifier` / `to_api_payload` need no
/// fallback.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedLocation {
    /// A code target.
    Code(CodeLocation),
    /// An existing configuration by hash.
    Hash(HashLocation),
}

impl ResolvedLocation {
    /// A one-line human description.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Code(location) => location.describe(),
            Self::Hash(location) => location.describe(),
        }
    }

    /// The lookup identifier payload.
    #[must_use]
    pub fn to_identifier(&self) -> Json {
        match self {
            Self::Code(location) => location.to_identifier(),
            Self::Hash(location) => location.to_identifier(),
        }
    }

    /// The create payload; `None` for a hash, which cannot describe a new
    /// configuration (the legacy method raised `NotImplementedError`).
    #[must_use]
    pub fn to_api_payload(&self) -> Option<Json> {
        match self {
            Self::Code(location) => Some(location.to_api_payload()),
            Self::Hash(_) => None,
        }
    }

    /// Widens to the renderer-facing [`Location`].
    #[must_use]
    pub fn into_location(self) -> Location {
        match self {
            Self::Code(location) => Location::Code(location),
            Self::Hash(location) => Location::Hash(location),
        }
    }
}

/// The raw location-related tool inputs of a create or lookup call.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocationInputs<'a> {
    /// `location_hash` (lookup only).
    pub location_hash: Option<&'a str>,
    /// `language`.
    pub language: Option<&'a str>,
    /// `file_path`.
    pub file_path: Option<&'a str>,
    /// `code_unit`.
    pub code_unit: Option<&'a str>,
    /// `class_name`.
    pub class_name: Option<&'a str>,
    /// `method_name`.
    pub method_name: Option<&'a str>,
    /// `line_number`.
    pub line_number: Option<i64>,
}

/// The error text for a lookup with neither a hash nor a code location.
pub const MISSING_LOCATION_IDENTIFIER: &str = "missing location identifier input. Provide location_hash OR language+file_path (code location).";

/// The error text for a lookup that must not use a code location.
pub const CODE_LOOKUP_UNSUPPORTED: &str =
    "code location lookup is not supported for this operation.";

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Parses create-call inputs into a code location.
///
/// A hash is not accepted: a configuration cannot be created from an
/// existing hash. The legacy `normalized_type` argument was unused and is
/// not carried over.
///
/// # Errors
///
/// The text the operation renders verbatim back to the caller.
pub fn parse_create_inputs(inputs: &LocationInputs<'_>) -> Result<ResolvedLocation, String> {
    let (Some(language), Some(file_path)) =
        (non_empty(inputs.language), non_empty(inputs.file_path))
    else {
        return Err("ERROR: BREAKPOINT/PROBE require language and file_path.\n\
             Example: language=\"Python\", file_path=\"/app/handler.py\""
            .to_string());
    };

    if let Some(message) = validate_location_inputs(
        language,
        file_path,
        inputs.code_unit,
        inputs.class_name,
        inputs.method_name,
        inputs.line_number,
    ) {
        return Err(message);
    }

    let language = canonical_language(Some(language)).unwrap_or(language);
    Ok(ResolvedLocation::Code(CodeLocation::from_inputs(
        inputs, language, file_path,
    )))
}

/// Parses lookup inputs: a location hash wins over a code location.
///
/// # Errors
///
/// [`CODE_LOOKUP_UNSUPPORTED`] when a code location is given but not
/// allowed, or [`MISSING_LOCATION_IDENTIFIER`] when nothing identifies the
/// target.
pub fn parse_lookup_inputs(
    inputs: &LocationInputs<'_>,
    allow_code_location_lookup: bool,
) -> Result<ResolvedLocation, String> {
    if let Some(hash) = non_empty(inputs.location_hash) {
        return Ok(ResolvedLocation::Hash(HashLocation {
            location_hash: hash.to_string(),
        }));
    }

    if let (Some(language), Some(file_path)) =
        (non_empty(inputs.language), non_empty(inputs.file_path))
    {
        if !allow_code_location_lookup {
            return Err(CODE_LOOKUP_UNSUPPORTED.to_string());
        }
        let language = canonical_language(Some(language)).unwrap_or(language);
        return Ok(ResolvedLocation::Code(CodeLocation::from_inputs(
            inputs, language, file_path,
        )));
    }

    Err(MISSING_LOCATION_IDENTIFIER.to_string())
}

fn code_location_from_members(payload: &Json, members: &[(String, Json)]) -> CodeLocation {
    let extra_fields = members
        .iter()
        .filter(|(key, _)| !KNOWN_CODE_FIELDS.contains(&key.as_str()))
        .cloned()
        .collect();
    CodeLocation {
        language: payload
            .get("Language")
            .cloned()
            .unwrap_or_else(|| Json::string("")),
        file_path: payload
            .get("FilePath")
            .cloned()
            .unwrap_or_else(|| Json::string("")),
        code_unit: payload.get("CodeUnit").cloned().unwrap_or(Json::Null),
        class_name: payload.get("ClassName").cloned().unwrap_or(Json::Null),
        method_name: payload.get("MethodName").cloned().unwrap_or(Json::Null),
        line_number: payload.get("LineNumber").cloned().unwrap_or(Json::Null),
        extra_fields,
    }
}

/// Parses a `Location` union returned by the API.
///
/// Returns [`Location::Unknown`] when the value has no recognized variant,
/// which keeps response rendering forward-compatible.
#[must_use]
pub fn location_from_response(union: &Json) -> Location {
    let Some(pairs) = members(union) else {
        return Location::Unknown(UnknownLocation { raw: Vec::new() });
    };

    if let Some(code_members) = get_members(union, "CodeLocation") {
        let code = union.get("CodeLocation").unwrap_or(&Json::Null);
        return Location::Code(code_location_from_members(code, code_members));
    }

    if union.get("Language").is_some() || union.get("FilePath").is_some() {
        return Location::Code(code_location_from_members(union, pairs));
    }

    Location::Unknown(UnknownLocation {
        raw: pairs.to_vec(),
    })
}

/// Renders the standard LOCATION block plus the optional INSTRUMENTATION
/// level line.
#[must_use]
pub fn render_location_block(location: &Location, location_hash: Option<&str>) -> String {
    match location {
        Location::Hash(hash) => {
            format!(
                "- LocationKind: HASH\n- LocationHash: {}\n",
                safe_text(&hash.location_hash)
            )
        }
        Location::Code(code) => {
            let mut output = code.format_details(location_hash);
            output.push_str("\nINSTRUMENTATION:\n");
            output.push_str(&format!("- Level: {}\n", code.level()));
            output
        }
        Location::Unknown(unknown) => unknown.format_details(location_hash),
    }
}

/// True when `error` is the "no identifier given" lookup error that the
/// operations replace with their usage help.
#[must_use]
pub fn is_missing_identifier_error(error: &str) -> bool {
    error.contains("missing location identifier input")
}
