//! The shape of the response members a CloudWatch operation consumes
//! (#4749, #4750, #4755, #4758), checked through the shared
//! `toolkit_aws::recorded` accessors before any result is formed.
//!
//! The rule is the one of #4727: an **absent** member takes its default; a
//! **present** member of another type, `null` included, makes the whole
//! response invalid. A malformed response therefore never reads as an empty
//! listing, a successful deletion, a write count or a score count. The
//! renderers of #4380 stay tolerant (they also render documents that never
//! came from a response); every response reaches them only after its
//! operation's shape has been checked here.
//!
//! A [`ShapeError`] names the member path and both types, never a value.

use std::fmt;

use toolkit_aws::recorded::{
    nullable_text, optional_list, optional_member, optional_object, optional_object_list,
    optional_string_list, optional_text, MemberError,
};
use toolkit_core::Json;

/// The expected type of one member.
#[derive(Debug, Clone, Copy)]
pub enum Shape {
    /// A string.
    Text,
    /// A string or `null` (the explicit opt-in for a display-only token whose
    /// `null` means "no further page").
    NullableText,
    /// An integer.
    Int,
    /// A number or `null` (the explicit opt-in for a score an error row
    /// leaves out).
    NullableNumber,
    /// A boolean.
    Bool,
    /// A timestamp: an ISO string or epoch seconds.
    Timestamp,
    /// An object whose listed members have the listed shapes; other members
    /// are not read and not checked.
    Object(&'static [Field]),
    /// A list of objects, each with the listed member shapes.
    ObjectList(&'static [Field]),
    /// A list of strings.
    StringList,
    /// A list whose items the consumer checks itself.
    List,
}

/// One member name and its shape.
pub type Field = (&'static str, Shape);

/// A present member of the wrong type, by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeError {
    path: String,
    expected: &'static str,
    found: &'static str,
}

impl ShapeError {
    /// The member path, such as `Configuration.Location`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    fn at(prefix: &str, error: &MemberError) -> Self {
        Self {
            path: join(prefix, error.key()),
            expected: error.expected(),
            found: error.found(),
        }
    }
}

/// ``the member `<path>` of the response is not <expected> (it is <found>)``.
impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the member `{}` of the response is not {} (it is {})",
            self.path, self.expected, self.found
        )
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Checks the listed members of `object` (itself an object).
///
/// # Errors
///
/// The first present member, in field order and depth first, of the wrong
/// type.
pub fn check(object: &Json, fields: &[Field]) -> Result<(), ShapeError> {
    check_at(object, fields, "")
}

fn check_at(object: &Json, fields: &[Field], prefix: &str) -> Result<(), ShapeError> {
    for (key, shape) in fields {
        check_member(object, key, *shape, prefix)?;
    }
    Ok(())
}

/// A present member must satisfy `accepts`.
fn typed(
    object: &Json,
    key: &str,
    expected: &'static str,
    accepts: fn(&Json) -> bool,
) -> Result<(), MemberError> {
    optional_member(object, key, expected, |member| {
        accepts(member).then_some(())
    })
    .map(drop)
}

fn check_member(object: &Json, key: &str, shape: Shape, prefix: &str) -> Result<(), ShapeError> {
    let wrap = |error: MemberError| ShapeError::at(prefix, &error);
    match shape {
        Shape::Text => optional_text(object, key).map(drop).map_err(wrap),
        Shape::NullableText => nullable_text(object, key).map(drop).map_err(wrap),
        Shape::Int => typed(object, key, "an integer", |member| {
            matches!(member, Json::Int(_))
        })
        .map_err(wrap),
        Shape::NullableNumber => typed(object, key, "a number or null", |member| {
            matches!(member, Json::Int(_) | Json::Float(_) | Json::Null)
        })
        .map_err(wrap),
        Shape::Bool => typed(object, key, "a boolean", |member| {
            matches!(member, Json::Bool(_))
        })
        .map_err(wrap),
        Shape::Timestamp => typed(object, key, "a timestamp", |member| {
            matches!(member, Json::String(_) | Json::Int(_) | Json::Float(_))
        })
        .map_err(wrap),
        Shape::Object(fields) => match optional_object(object, key).map_err(wrap)? {
            Some(inner) => check_at(inner, fields, &join(prefix, key)),
            None => Ok(()),
        },
        Shape::ObjectList(fields) => {
            let items = optional_object_list(object, key).map_err(wrap)?;
            for (index, item) in items.iter().enumerate() {
                check_at(item, fields, &format!("{}[{index}]", join(prefix, key)))?;
            }
            Ok(())
        }
        Shape::StringList => optional_string_list(object, key).map(drop).map_err(wrap),
        Shape::List => optional_list(object, key).map(drop).map_err(wrap),
    }
}
