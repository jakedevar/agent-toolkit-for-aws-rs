//! Relational's rendering of the shared `toolkit_aws::recorded` accessors.
//!
//! Every recording this slice reads (`--recorded FILE`) follows the shared rule
//! (#4727): an *optional* member that is absent takes its default; one that is
//! present must be of the expected type, and `null` is a wrong type, not an
//! absent member. A wrong type rejects the whole recording. The accessors here
//! only delegate to [`recorded`] and render its [`MemberError`] in the wording
//! this slice already published (`"<key>" must be <expected> when present (it
//! is <found>)`): the member name, the expected type and the type found, never
//! a value (#4799).

use toolkit_aws::recorded::{self, MemberError};
use toolkit_core::Json;

/// `"<key>" must be <expected> when present (it is <found>)`.
pub(crate) fn when_present(error: &MemberError, expected: &str) -> String {
    format!(
        "\"{}\" must be {expected} when present (it is {})",
        error.key(),
        error.found()
    )
}

/// `"<key>" must be <expected> (it is <found>)`, the wording of the
/// `rds commitment` response members.
pub(crate) fn response_reason(error: &MemberError, expected: &str) -> String {
    format!(
        "\"{}\" must be {expected} (it is {})",
        error.key(),
        error.found()
    )
}

/// An optional string member ([`recorded::optional_text`]).
pub(crate) fn text_member<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, String> {
    recorded::optional_text(object, key).map_err(|error| when_present(&error, "a string"))
}

/// An optional object member ([`recorded::optional_object`]).
pub(crate) fn object_member<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, String> {
    recorded::optional_object(object, key).map_err(|error| when_present(&error, "an object"))
}

/// An optional array member ([`recorded::optional_list`]); `None` when absent,
/// so a caller can still tell an absent member from `[]`.
pub(crate) fn array_member<'j>(object: &'j Json, key: &str) -> Result<Option<&'j [Json]>, String> {
    let items =
        recorded::optional_list(object, key).map_err(|error| when_present(&error, "an array"))?;
    Ok(object.get(key).map(|_| items))
}

/// An optional boolean member ([`recorded::optional_member`]).
pub(crate) fn bool_member(object: &Json, key: &str) -> Result<Option<bool>, String> {
    recorded::optional_member(object, key, "a boolean", |member| match member {
        Json::Bool(flag) => Some(*flag),
        _ => None,
    })
    .map_err(|error| when_present(&error, "a boolean"))
}

/// An optional integer member ([`recorded::optional_member`]).
pub(crate) fn int_member(object: &Json, key: &str) -> Result<Option<i64>, String> {
    recorded::optional_member(object, key, "an integer", |member| match member {
        Json::Int(number) => Some(*number),
        _ => None,
    })
    .map_err(|error| when_present(&error, "an integer"))
}

/// An optional finite, non-negative number, integer or float
/// ([`recorded::optional_member`]).
pub(crate) fn seconds_member(object: &Json, key: &str) -> Result<Option<f64>, String> {
    let expected = "a finite non-negative number";
    recorded::optional_member(object, key, expected, |member| match member {
        #[allow(clippy::cast_precision_loss)]
        Json::Int(number) if *number >= 0 => Some(*number as f64),
        Json::Float(number) if number.is_finite() && *number >= 0.0 => Some(*number),
        _ => None,
    })
    .map_err(|error| when_present(&error, expected))
}

/// Python `type(value).__name__` of a decoded JSON value.
// recorded-shape: Python type names classify every JSON type on purpose; nothing is read or defaulted.
pub(crate) fn py_type_name(value: &Json) -> &'static str {
    match value {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Int(_) => "int",
        Json::Float(_) => "float",
        Json::String(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}
