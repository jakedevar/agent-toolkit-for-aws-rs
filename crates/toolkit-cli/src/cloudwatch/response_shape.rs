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

use super::timefmt::time_from_json;

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
    /// A timestamp: an ISO 8601 string or epoch seconds in range. Any other
    /// string is not a timestamp (#4381 repair 3), so a renderer never
    /// prints one as a time.
    Timestamp,
    /// A list of objects whose members are all strings (attribute-filter
    /// groups). Their member names are data, so an error names the item.
    TextMapList,
    /// An object whose listed members have the listed shapes; other members
    /// are not read and not checked.
    Object(&'static [Field]),
    /// A list of objects, each with the listed member shapes.
    ObjectList(&'static [Field]),
    /// A list of strings.
    StringList,
    /// A list whose items the consumer checks itself.
    List,
    /// Logs Insights result rows: a list of rows, each a list of objects with
    /// the listed members. Only the list is checked here; the consumer checks
    /// each row itself and gives a malformed one its own diagnostic (#4750).
    Rows(&'static [Field]),
    /// An object whose listed members are either wrapped in the `wrapper`
    /// object or written flat at its top (an API union such as `Location`
    /// with `CodeLocation`); the wrapped form wins when both are present.
    Union {
        /// The wrapper member.
        wrapper: &'static str,
        /// The members, in either form.
        members: &'static [Field],
    },
    /// A member read as data and passed on as it is, of any type, `null`
    /// included (an Omni span column: an SQL value the evaluator or the
    /// dataset receives unchanged, as in Python). Never rejected; listed so
    /// the read set stays complete (#4381 repair 4).
    Data,
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
            time_from_json(member).is_ok()
        })
        .map_err(wrap),
        Shape::TextMapList => {
            let items = optional_object_list(object, key).map_err(wrap)?;
            for (index, item) in items.iter().enumerate() {
                // recorded-shape: `optional_object_list` already rejected every item that is not an object.
                let Json::Object(members) = item else {
                    continue;
                };
                if let Some((_, value)) = members
                    .iter()
                    .find(|(_, value)| !matches!(value, Json::String(_)))
                {
                    return Err(ShapeError {
                        path: format!("{}[{index}]", join(prefix, key)),
                        expected: "an object of strings",
                        found: value.type_name(),
                    });
                }
            }
            Ok(())
        }
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
        Shape::Union { wrapper, members } => match optional_object(object, key).map_err(wrap)? {
            Some(inner) => {
                let at = join(prefix, key);
                check_member(inner, wrapper, Shape::Object(members), &at)?;
                check_at(inner, members, &at)
            }
            None => Ok(()),
        },
        Shape::StringList => optional_string_list(object, key).map(drop).map_err(wrap),
        Shape::List | Shape::Rows(_) => optional_list(object, key).map(drop).map_err(wrap),
        Shape::Data => Ok(()),
    }
}

/// The [`Shape::Data`] fields of an Omni span row: the `base` columns, then
/// the alias of each `(attribute key, alias)` column, so the row read set is
/// derived from the same column table the SQL is built from.
///
/// # Panics
///
/// At compile time, when `N` is not `B + A`.
#[must_use]
pub const fn data_columns<const B: usize, const A: usize, const N: usize>(
    base: [&'static str; B],
    attributes: [(&'static str, &'static str); A],
) -> [Field; N] {
    assert!(
        B + A == N,
        "N must be the number of base and attribute columns"
    );
    let mut out: [Field; N] = [("", Shape::Data); N];
    let mut index = 0;
    while index < B {
        out[index] = (base[index], Shape::Data);
        index += 1;
    }
    let mut column = 0;
    while column < A {
        out[B + column] = (attributes[column].1, Shape::Data);
        column += 1;
    }
    out
}

/// Which form of a [`Shape::Union`] a member path is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionSide {
    /// The path of the union object, such as `Configuration.Location`.
    pub at: String,
    /// Its wrapper member, such as `CodeLocation`.
    pub wrapper: &'static str,
    /// True for the flat form (members at the top of the union), false for
    /// the wrapped one (the wrapper object and the members inside it).
    pub flat: bool,
}

/// One member path a table reads: `a.b` for an object member, `a[*]` for
/// the items of a list, `a[*][*]` for the fields of a result row.
#[derive(Debug, Clone)]
pub struct MemberPath {
    /// The generic path.
    pub path: String,
    /// The member's shape.
    pub shape: Shape,
    /// The union form the path belongs to, when it is inside a union.
    pub union: Option<UnionSide>,
}

/// Every member path `fields` reads, depth first: the single source of the
/// response read set the mutation harness must cover (#4381 repair 4). A
/// union yields its wrapped and its flat form; the members of an attribute
/// filter group are data keys, so the path stops at the group.
#[must_use]
pub fn member_paths(fields: &[Field]) -> Vec<MemberPath> {
    let mut out = Vec::new();
    paths_at(fields, "", None, &mut out);
    out
}

fn paths_at(fields: &[Field], prefix: &str, union: Option<&UnionSide>, out: &mut Vec<MemberPath>) {
    for (key, shape) in fields {
        let path = join(prefix, key);
        let mut push = |path: String, shape: Shape| {
            out.push(MemberPath {
                path,
                shape,
                union: union.cloned(),
            });
        };
        push(path.clone(), *shape);
        match *shape {
            Shape::Object(inner) => paths_at(inner, &path, union, out),
            Shape::ObjectList(inner) => {
                let items = format!("{path}[*]");
                push(items.clone(), Shape::Object(inner));
                paths_at(inner, &items, union, out);
            }
            Shape::Rows(inner) => {
                let rows = format!("{path}[*]");
                push(rows.clone(), Shape::ObjectList(inner));
                let cells = format!("{rows}[*]");
                push(cells.clone(), Shape::Object(inner));
                paths_at(inner, &cells, union, out);
            }
            Shape::StringList => push(format!("{path}[*]"), Shape::Text),
            Shape::TextMapList => push(format!("{path}[*]"), Shape::Object(&[])),
            Shape::Union { wrapper, members } => {
                let wrapped = UnionSide {
                    at: path.clone(),
                    wrapper,
                    flat: false,
                };
                let flat = UnionSide {
                    flat: true,
                    ..wrapped.clone()
                };
                paths_at(
                    &[(wrapper, Shape::Object(members))],
                    &path,
                    Some(&wrapped),
                    out,
                );
                paths_at(members, &path, Some(&flat), out);
            }
            Shape::Text
            | Shape::NullableText
            | Shape::Int
            | Shape::NullableNumber
            | Shape::Bool
            | Shape::Timestamp
            | Shape::List
            | Shape::Data => {}
        }
    }
}
