//! The one accessor family for *optional* members of recorded input and of AWS
//! responses.
//!
//! The rule is the same at every site: an **absent** member takes its default;
//! a **present** member of another type, `null` included, is rejected. The
//! reason names the member and its expected type, never the value, so a hostile
//! response or recording cannot reach the terminal through it.
//!
//! Legitimately nullable scalars (`FailureReason`, a token that ends paging)
//! are not read through here; only schema arrays, objects and the recorded
//! step members are.

use toolkit_core::Json;

/// An optional member that must satisfy `expect` when present.
///
/// # Errors
///
/// The member is present but `expect` refuses it (`null` is not absent).
pub(super) fn optional_member<'j, T>(
    object: &'j Json,
    key: &str,
    expected: &str,
    expect: impl FnOnce(&'j Json) -> Option<T>,
) -> Result<Option<T>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(member) => expect(member).map(Some).ok_or_else(|| {
            format!(
                "{key} must be {expected} when present (it is {})",
                member.type_name()
            )
        }),
    }
}

/// An optional string member.
///
/// # Errors
///
/// The member is present and not a string.
pub(super) fn optional_string<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, String> {
    optional_member(object, key, "a string", Json::as_str)
}

/// An optional object member.
///
/// # Errors
///
/// The member is present and not an object.
pub(super) fn optional_object<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, String> {
    optional_member(object, key, "an object", |member| {
        matches!(member, Json::Object(_)).then_some(member)
    })
}

/// An optional list member, empty when absent.
///
/// # Errors
///
/// The member is present and not a list.
pub(super) fn optional_list<'j>(object: &'j Json, key: &str) -> Result<&'j [Json], String> {
    optional_member(object, key, "a list", |member| match member {
        Json::Array(items) => Some(items.as_slice()),
        _ => None,
    })
    .map(Option::unwrap_or_default)
}

/// An optional list member whose items must all be objects, empty when absent.
///
/// # Errors
///
/// The member is present and not a list of objects.
pub(super) fn optional_object_list<'j>(object: &'j Json, key: &str) -> Result<&'j [Json], String> {
    optional_member(object, key, "a list of objects", |member| match member {
        Json::Array(items) if items.iter().all(|item| matches!(item, Json::Object(_))) => {
            Some(items.as_slice())
        }
        _ => None,
    })
    .map(Option::unwrap_or_default)
}

/// An optional list member whose items must all be strings, empty when absent.
///
/// # Errors
///
/// The member is present and not a list of strings.
pub(super) fn optional_string_list<'j>(
    object: &'j Json,
    key: &str,
) -> Result<Vec<&'j str>, String> {
    optional_member(object, key, "a list of strings", |member| match member {
        Json::Array(items) => items.iter().map(Json::as_str).collect::<Option<Vec<_>>>(),
        _ => None,
    })
    .map(Option::unwrap_or_default)
}
