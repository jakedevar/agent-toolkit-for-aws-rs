//! Custom metric validation (`validate_custom_metrics.py`).
//!
//! The Python script validated a Bedrock LLM-as-Judge custom metric array with
//! pydantic models. This module re-implements those models by hand: the same
//! fields, the same coercions that matter for JSON input and the same custom
//! rules and messages, reported in pydantic's `N validation errors for ...`
//! layout (without its `input_value` repr and documentation URL; see
//! `docs/migration/ai-data.md`).

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use toolkit_core::{
    parse_json, read_regular_file, Json, PathError, ToolkitError, MAX_TEXT_FILE_BYTES,
};
use toolkit_msk::pyfmt::py_float_parse;

use super::{parse_failure_text, py_type_name};

/// Printed (on stdout, exit 1) when no argument is given.
pub(crate) const USAGE_LINE: &str = "Usage: toolkit ai-data validate-metrics '<json>' | file.json";
/// Most metric definitions accepted in one array.
pub(crate) const MAX_METRICS: usize = 10;
/// Longest `definition` of a rating scale entry, in characters.
pub(crate) const MAX_DEFINITION_CHARS: usize = 100;
/// Longest `instructions` text, in characters.
pub(crate) const MAX_INSTRUCTIONS_CHARS: usize = 5000;

/// One pydantic-style field error.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldError {
    location: Vec<String>,
    kind: &'static str,
    message: String,
    input_type: &'static str,
}

fn type_error(
    location: &[String],
    field: &str,
    kind: &'static str,
    message: &str,
    value: &Json,
) -> FieldError {
    FieldError {
        location: child(location, field),
        kind,
        message: message.to_string(),
        input_type: py_type_name(value),
    }
}

fn child(location: &[String], part: &str) -> Vec<String> {
    let mut next = location.to_vec();
    next.push(part.to_string());
    next
}

fn missing(location: &[String], field: &str) -> FieldError {
    FieldError {
        location: child(location, field),
        kind: "missing",
        message: "Field required".to_string(),
        input_type: "dict",
    }
}

fn not_a_model(location: &[String], model: &str, value: &Json) -> FieldError {
    FieldError {
        location: location.to_vec(),
        kind: "model_type",
        message: format!("Input should be a valid dictionary or instance of {model}"),
        input_type: py_type_name(value),
    }
}

fn value_error(location: &[String], message: &str) -> FieldError {
    FieldError {
        location: location.to_vec(),
        kind: "value_error",
        message: format!("Value error, {message}"),
        input_type: "dict",
    }
}

/// A required `str` field: absent or not a string is an error.
fn required_string<'a>(
    object: &'a Json,
    location: &[String],
    field: &str,
    errors: &mut Vec<FieldError>,
) -> Option<&'a str> {
    match object.get(field) {
        None => {
            errors.push(missing(location, field));
            None
        }
        Some(Json::String(text)) => Some(text.as_str()),
        Some(other) => {
            errors.push(type_error(
                location,
                field,
                "string_type",
                "Input should be a valid string",
                other,
            ));
            None
        }
    }
}

/// An `Optional[str]` field. Returns whether it holds a string.
fn optional_string(
    object: &Json,
    location: &[String],
    field: &str,
    errors: &mut Vec<FieldError>,
) -> bool {
    match object.get(field) {
        None | Some(Json::Null) => false,
        Some(Json::String(_)) => true,
        Some(other) => {
            errors.push(type_error(
                location,
                field,
                "string_type",
                "Input should be a valid string",
                other,
            ));
            false
        }
    }
}

/// An `Optional[float]` field with pydantic's lax coercion: numbers and
/// booleans are accepted, a string only when `float()` parses it. Returns
/// whether it holds a float.
fn optional_float(
    object: &Json,
    location: &[String],
    field: &str,
    errors: &mut Vec<FieldError>,
) -> bool {
    match object.get(field) {
        None | Some(Json::Null) => false,
        Some(Json::Int(_) | Json::Float(_) | Json::Bool(_)) => true,
        Some(Json::String(text)) => {
            if py_float_parse(text).is_some() {
                true
            } else {
                errors.push(type_error(
                    location,
                    field,
                    "float_parsing",
                    "Input should be a valid number, unable to parse string as a number",
                    &Json::string(text.as_str()),
                ));
                false
            }
        }
        Some(other) => {
            errors.push(type_error(
                location,
                field,
                "float_type",
                "Input should be a valid number",
                other,
            ));
            false
        }
    }
}

/// Which value type a rating scale entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleKind {
    Float,
    String,
}

/// `RatingValue`: exactly one of `floatValue` or `stringValue`.
fn validate_rating_value(
    value: &Json,
    location: &[String],
    errors: &mut Vec<FieldError>,
) -> Option<ScaleKind> {
    if !matches!(value, Json::Object(_)) {
        errors.push(not_a_model(location, "RatingValue", value));
        return None;
    }
    let before = errors.len();
    let has_float = optional_float(value, location, "floatValue", errors);
    let has_string = optional_string(value, location, "stringValue", errors);
    if errors.len() != before {
        return None;
    }
    if has_float == has_string {
        errors.push(value_error(
            location,
            "Exactly one of 'floatValue' or 'stringValue' must be set.",
        ));
        return None;
    }
    Some(if has_float {
        ScaleKind::Float
    } else {
        ScaleKind::String
    })
}

/// `RatingScaleEntry`: a `definition` of at most 100 characters and a value.
fn validate_scale_entry(
    entry: &Json,
    location: &[String],
    errors: &mut Vec<FieldError>,
) -> Option<ScaleKind> {
    if !matches!(entry, Json::Object(_)) {
        errors.push(not_a_model(location, "RatingScaleEntry", entry));
        return None;
    }
    if let Some(definition) = required_string(entry, location, "definition", errors) {
        let length = definition.chars().count();
        if length > MAX_DEFINITION_CHARS {
            errors.push(value_error(
                &child(location, "definition"),
                &format!("Definition exceeds {MAX_DEFINITION_CHARS} chars ({length})."),
            ));
        }
    }
    match entry.get("value") {
        None => {
            errors.push(missing(location, "value"));
            None
        }
        Some(value) => validate_rating_value(value, &child(location, "value"), errors),
    }
}

/// `CustomMetricDefinition`: name, instructions and an optional rating scale,
/// then the two model-level rules (run only when every field is valid).
fn validate_definition(definition: &Json, location: &[String], errors: &mut Vec<FieldError>) {
    if !matches!(definition, Json::Object(_)) {
        errors.push(not_a_model(location, "CustomMetricDefinition", definition));
        return;
    }
    let before = errors.len();
    required_string(definition, location, "name", errors);
    let instructions = required_string(definition, location, "instructions", errors);

    let mut kinds: Vec<ScaleKind> = Vec::new();
    match definition.get("ratingScale") {
        None | Some(Json::Null) => {}
        Some(Json::Array(entries)) => {
            let scale_location = child(location, "ratingScale");
            for (index, entry) in entries.iter().enumerate() {
                let entry_location = child(&scale_location, &index.to_string());
                if let Some(kind) = validate_scale_entry(entry, &entry_location, errors) {
                    kinds.push(kind);
                }
            }
        }
        Some(other) => errors.push(type_error(
            location,
            "ratingScale",
            "list_type",
            "Input should be a valid list",
            other,
        )),
    }
    if errors.len() != before {
        return;
    }
    let Some(instructions) = instructions else {
        return;
    };

    let length = instructions.chars().count();
    if length > MAX_INSTRUCTIONS_CHARS {
        errors.push(value_error(
            location,
            &format!("Instructions exceed {MAX_INSTRUCTIONS_CHARS} char limit ({length})."),
        ));
        return;
    }
    if !instructions.contains("{{prediction}}") && !instructions.contains("{{prompt}}") {
        errors.push(value_error(
            location,
            "Instructions must contain at least {{prompt}} or {{prediction}}.",
        ));
        return;
    }
    let has_float = kinds.contains(&ScaleKind::Float);
    let has_string = kinds.contains(&ScaleKind::String);
    if has_float && has_string {
        errors.push(value_error(
            location,
            "ratingScale mixes float and string values. Use one type.",
        ));
    }
}

/// `CustomMetric`: the one wrapper field `customMetricDefinition`.
fn validate_metric(item: &Json) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if !matches!(item, Json::Object(_)) {
        errors.push(not_a_model(&[], "CustomMetric", item));
        return errors;
    }
    match item.get("customMetricDefinition") {
        None => errors.push(missing(&[], "customMetricDefinition")),
        Some(definition) => validate_definition(
            definition,
            &["customMetricDefinition".to_string()],
            &mut errors,
        ),
    }
    errors
}

/// pydantic's `str(ValidationError)` layout for one metric.
fn format_errors(errors: &[FieldError]) -> String {
    let count = errors.len();
    let plural = if count == 1 { "" } else { "s" };
    let mut text = format!("{count} validation error{plural} for CustomMetric");
    for error in errors {
        if !error.location.is_empty() {
            text.push('\n');
            text.push_str(&error.location.join("."));
        }
        let _ = write!(
            text,
            "\n  {} [type={}, input_type={}]",
            error.message, error.kind, error.input_type
        );
    }
    text
}

/// Validates a JSON string of custom metrics. Returns the number of metrics
/// when valid, otherwise the error lines in order.
pub(crate) fn validate(raw: &str) -> Result<usize, Vec<String>> {
    let data = match parse_json(raw) {
        Ok(data) => data,
        Err(error) => {
            return Err(vec![format!(
                "Invalid JSON: {}",
                parse_failure_text(&error)
            )])
        }
    };
    let Json::Array(items) = data else {
        return Err(vec![
            "Must be a JSON array of metric definitions.".to_string()
        ]);
    };
    if items.is_empty() {
        return Err(vec![
            "Array is empty \u{2014} need at least one metric.".to_string()
        ]);
    }
    if items.len() > MAX_METRICS {
        return Err(vec![format!(
            "Too many metrics ({}). Maximum is {MAX_METRICS}.",
            items.len()
        )]);
    }
    let mut failures = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let errors = validate_metric(item);
        if !errors.is_empty() {
            failures.push(format!("Metric [{index}]: {}", format_errors(&errors)));
        }
    }
    if failures.is_empty() {
        Ok(items.len())
    } else {
        Err(failures)
    }
}

/// The text to validate for the command argument: the content of the regular
/// file it names, or the argument itself when no such file exists (the script
/// treated a missing file or a directory as inline JSON).
pub(crate) fn read_input(argument: &str) -> Result<String, ToolkitError> {
    match read_regular_file(Path::new(argument), MAX_TEXT_FILE_BYTES) {
        Ok(text) => Ok(text),
        Err(PathError::NotFound(_) | PathError::NotRegularFile(_)) => Ok(argument.to_string()),
        // Inline JSON is usually too long or too odd to be a file name.
        Err(PathError::Io(error, _))
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput
                    | io::ErrorKind::InvalidFilename
                    | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(argument.to_string())
        }
        Err(error) => Err(ToolkitError::runtime("input_unreadable", error.to_string())),
    }
}
