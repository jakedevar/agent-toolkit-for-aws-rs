//! Fail-closed accessors for the members of a recorded-input document.
//!
//! Every recording this slice reads (`--recorded FILE`) follows one rule, the
//! same one `bedrock-agent fetch` applies (#4712, #4713): an *optional* member
//! that is absent takes its default; one that is present must be of the
//! expected type, and `null` is a wrong type, not an absent member. A wrong
//! type rejects the whole recording. The reason names the member and the type
//! it should have, never the value.

use toolkit_core::Json;

/// The one accessor of an optional member: absent is `Ok(None)`; a present
/// member must satisfy `expect` or the recording is rejected.
pub(crate) fn optional_member<'j, T>(
    object: &'j Json,
    key: &str,
    expected: &str,
    expect: impl FnOnce(&'j Json) -> Option<T>,
) -> Result<Option<T>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(member) => expect(member).map(Some).ok_or_else(|| {
            format!(
                "\"{key}\" must be {expected} when present (it is {})",
                member.type_name()
            )
        }),
    }
}

pub(crate) fn optional_string<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, String> {
    optional_member(object, key, "a string", Json::as_str)
}

pub(crate) fn optional_bool(object: &Json, key: &str) -> Result<Option<bool>, String> {
    optional_member(object, key, "a boolean", |member| match member {
        Json::Bool(flag) => Some(*flag),
        _ => None,
    })
}

pub(crate) fn optional_int(object: &Json, key: &str) -> Result<Option<i64>, String> {
    optional_member(object, key, "an integer", |member| match member {
        Json::Int(number) => Some(*number),
        _ => None,
    })
}

/// A finite, non-negative number (integer or float).
pub(crate) fn optional_seconds(object: &Json, key: &str) -> Result<Option<f64>, String> {
    optional_member(
        object,
        key,
        "a finite non-negative number",
        |member| match member {
            #[allow(clippy::cast_precision_loss)]
            Json::Int(number) if *number >= 0 => Some(*number as f64),
            Json::Float(number) if number.is_finite() && *number >= 0.0 => Some(*number),
            _ => None,
        },
    )
}

pub(crate) fn optional_object<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, String> {
    optional_member(object, key, "an object", |member| {
        matches!(member, Json::Object(_)).then_some(member)
    })
}

pub(crate) fn optional_array<'j>(
    object: &'j Json,
    key: &str,
) -> Result<Option<&'j Vec<Json>>, String> {
    optional_member(object, key, "an array", |member| match member {
        Json::Array(items) => Some(items),
        _ => None,
    })
}

/// JSON equality that ignores object key order.
pub(crate) fn same_json(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(other, _)| other == key)
                        .is_some_and(|(_, other)| same_json(value, other))
                })
        }
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_json(x, y))
        }
        _ => left == right,
    }
}

/// Python `type(value).__name__` of a decoded JSON value.
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
