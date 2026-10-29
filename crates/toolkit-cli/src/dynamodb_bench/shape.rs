//! The only reads of JSON container shapes in this slice.
//!
//! Every other file of the module reads response members through
//! `toolkit_aws::recorded` accessors or through these two functions. Each is
//! marked: the container it opens is a member of the benchmark Lambda's own
//! response, and the caller validates every item, rejecting any other shape.

use toolkit_core::Json;

/// The items of a list member.
// recorded-shape: lambda response list member, every item is validated by the caller, other shapes are rejected.
pub(super) fn list_items(value: &Json) -> Option<&[Json]> {
    match value {
        Json::Array(items) => Some(items.as_slice()),
        _ => None,
    }
}

/// The entries of an object member, in response order.
// recorded-shape: lambda response map member, every entry is validated by the caller, other shapes are rejected.
pub(super) fn map_entries(value: &Json) -> Option<&[(String, Json)]> {
    match value {
        Json::Object(entries) => Some(entries.as_slice()),
        _ => None,
    }
}
