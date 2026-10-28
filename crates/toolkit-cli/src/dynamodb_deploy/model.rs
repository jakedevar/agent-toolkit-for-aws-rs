//! Reads of the design model and the benchmark config.
//!
//! Both files are caller input, not AWS responses. The scripts read them with
//! Python's `value or default`: an absent member, `null`, `false`, `0`, `""`,
//! `[]` and `{}` all mean "not set". These accessors keep that rule and reject
//! only a truthy member of the wrong type, which the scripts either crashed on
//! or passed to AWS to be refused. Every read of a JSON shape in this module is
//! marked: none of it touches a recorded response.

use toolkit_core::{quote_untrusted, Json, ToolkitError};

use crate::dynamodb::pyfmt;

/// A design or config problem: exit status 2, like the script's `_die`.
pub(crate) fn die(message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime("dynamodb_deploy_invalid", message).with_exit_code(2)
}

/// `value or default` for a member that must be an array.
pub(crate) fn list<'a>(parent: &'a Json, key: &str) -> Result<&'a [Json], ToolkitError> {
    match parent.get(key) {
        None => Ok(&[]),
        Some(value) if !pyfmt::truthy(value) => Ok(&[]),
        // recorded-shape: design member, Python `or []` semantics, not a response.
        Some(Json::Array(items)) => Ok(items.as_slice()),
        Some(other) => Err(die(format!(
            "\"{key}\" must be an array (it is {})",
            other.type_name()
        ))),
    }
}

/// `value or {}` for a member that must be an object: `None` when not set.
pub(crate) fn object<'a>(parent: &'a Json, key: &str) -> Result<Option<&'a Json>, ToolkitError> {
    match parent.get(key) {
        None => Ok(None),
        Some(value) if !pyfmt::truthy(value) => Ok(None),
        // recorded-shape: design member, Python `or {}` semantics, not a response.
        Some(value @ Json::Object(_)) => Ok(Some(value)),
        Some(other) => Err(die(format!(
            "\"{key}\" must be an object (it is {})",
            other.type_name()
        ))),
    }
}

/// `value or None` for a member that must be a string: `None` when not set.
pub(crate) fn text<'a>(parent: &'a Json, key: &str) -> Result<Option<&'a str>, ToolkitError> {
    match parent.get(key) {
        None => Ok(None),
        Some(value) if !pyfmt::truthy(value) => Ok(None),
        Some(Json::String(text)) => Ok(Some(text.as_str())),
        Some(other) => Err(die(format!(
            "\"{key}\" must be a string (it is {})",
            other.type_name()
        ))),
    }
}

/// The first of `keys` that is set (`a.get(x) or a.get(y)`), as a string.
pub(crate) fn first_text<'a>(
    parent: &'a Json,
    keys: &[&str],
) -> Result<Option<&'a str>, ToolkitError> {
    for key in keys {
        if let Some(found) = text(parent, key)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Every item of `items` must be an object (`what` names the list).
pub(crate) fn require_objects(items: &[Json], what: &str) -> Result<(), ToolkitError> {
    for (index, item) in items.iter().enumerate() {
        // recorded-shape: design list item, a non-object item is rejected, not a response.
        if !matches!(item, Json::Object(_)) {
            return Err(die(format!(
                "{what}[{index}] must be an object (it is {})",
                item.type_name()
            )));
        }
    }
    Ok(())
}

/// A set list of strings (`a.get(x) or []`, each item a string).
pub(crate) fn strings(parent: &Json, key: &str) -> Result<Vec<String>, ToolkitError> {
    list(parent, key)?
        .iter()
        .enumerate()
        .map(|(index, item)| match item {
            Json::String(text) => Ok(text.clone()),
            other => Err(die(format!(
                "\"{key}\"[{index}] must be a string (it is {})",
                other.type_name()
            ))),
        })
        .collect()
}

/// Python `int(value)` for the JSON values the scripts accept: a whole number,
/// a float (truncated), a boolean or a numeric string. `None` otherwise.
// A float beyond `i128` saturates, which every caller range-checks and rejects.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn py_int(value: &Json) -> Option<i128> {
    match value {
        Json::Int(number) => Some(i128::from(*number)),
        Json::Bool(flag) => Some(i128::from(*flag)),
        Json::Float(number) if number.is_finite() => Some(number.trunc() as i128),
        Json::String(text) => text.trim().parse::<i128>().ok(),
        _ => None,
    }
}

/// `repr(member)` of an optional member, escaped for a diagnostic.
pub(crate) fn show(value: Option<&Json>) -> String {
    match value {
        None => "None".to_string(),
        Some(value) => quote_untrusted(&pyfmt::text(value)),
    }
}

/// A name for a diagnostic: the string, or `?` when it is not set.
pub(crate) fn name_or_unknown(parent: &Json, key: &str) -> String {
    match parent.get(key) {
        Some(Json::String(text)) if !text.is_empty() => quote_untrusted(text),
        _ => "?".to_string(),
    }
}
