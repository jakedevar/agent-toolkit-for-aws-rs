//! Typed readers for optional members of a response or a snapshot.
//!
//! They sit on the shared `toolkit_aws::recorded::optional_member`: an
//! **absent** member takes the caller's default, a **present** member of
//! another type, `null` included, is a [`MemberError`] that names the member
//! and both types and never the value.

use toolkit_aws::recorded::{optional_member, MemberError};
use toolkit_core::Json;

/// A JSON number read as `f64`.
pub(super) fn number(value: &Json) -> Option<f64> {
    match value {
        Json::Int(n) => Some(*n as f64),
        Json::Float(n) => Some(*n),
        _ => None,
    }
}

/// An optional boolean member.
pub(super) fn optional_bool(object: &Json, key: &str) -> Result<Option<bool>, MemberError> {
    optional_member(object, key, "a boolean", |member| match member {
        Json::Bool(flag) => Some(*flag),
        _ => None,
    })
}

/// An optional number member (an integer or a float).
pub(super) fn optional_number(object: &Json, key: &str) -> Result<Option<f64>, MemberError> {
    optional_member(object, key, "a number", number)
}

/// An optional integer member.
pub(super) fn optional_int(object: &Json, key: &str) -> Result<Option<i64>, MemberError> {
    optional_member(object, key, "an integer", |member| match member {
        Json::Int(n) => Some(*n),
        _ => None,
    })
}
