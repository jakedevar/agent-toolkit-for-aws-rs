//! Python value semantics for the JSON space the scoring engine reasons about:
//! equality across numbers and booleans, `in` on lists, strings and objects,
//! and ordered maps with overwrite-in-place like a `dict`.

use std::cmp::Ordering;

use toolkit_core::Json;

use super::ScoreError;

/// Result of a scoring step.
pub(super) type Res<T> = Result<T, ScoreError>;

/// Stand-in for a missing answer (`answers.get(name)` is `None`).
pub(super) static NULL: Json = Json::Null;

/// A score: a Python `int` or `float`.
#[derive(Clone, Copy, Debug)]
pub(super) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// A JSON number or boolean read as a Python number (`True` is `1`).
    pub(super) fn from_json(value: &Json) -> Option<Self> {
        match value {
            Json::Bool(flag) => Some(Self::Int(i64::from(*flag))),
            Json::Int(number) => Some(Self::Int(*number)),
            Json::Float(number) => Some(Self::Float(*number)),
            _ => None,
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn to_f64(self) -> f64 {
        match self {
            Self::Int(number) => number as f64,
            Self::Float(number) => number,
        }
    }

    /// `self + other`; integer sums that overflow `i64` are reported.
    pub(super) fn add(self, other: Self) -> Res<Self> {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => left
                .checked_add(right)
                .map(Self::Int)
                .ok_or_else(|| ScoreError::new("a runtime score is too large")),
            (left, right) => Ok(Self::Float(left.to_f64() + right.to_f64())),
        }
    }

    /// `self - amount` for a small non-negative `amount`.
    #[allow(clippy::cast_precision_loss)]
    pub(super) fn minus(self, amount: i64) -> Self {
        match self {
            Self::Int(number) => Self::Int(number.saturating_sub(amount)),
            Self::Float(number) => Self::Float(number - amount as f64),
        }
    }

    pub(super) fn to_json(self) -> Json {
        match self {
            Self::Int(number) => Json::Int(number),
            Self::Float(number) => Json::Float(number),
        }
    }
}

impl PartialEq for Num {
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

impl PartialOrd for Num {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => Some(left.cmp(right)),
            _ => self.to_f64().partial_cmp(&other.to_f64()),
        }
    }
}

// Python `==` for JSON values (numbers and booleans compare by value, objects
// ignore member order) is shared with the manifest validator.
pub(super) use crate::manifests::py_eq;

fn unhashable() -> ScoreError {
    ScoreError::new("a list or object cannot be used as a lookup key")
}

/// Python `item in container` for a JSON container.
///
/// # Errors
///
/// Where Python raises `TypeError`: a non-string item searched in a string, an
/// unhashable item searched in an object, or a container that is not one.
pub(super) fn py_contains(container: &Json, item: &Json) -> Res<bool> {
    match container {
        Json::Array(items) => Ok(items.iter().any(|candidate| py_eq(candidate, item))),
        Json::String(text) => match item {
            Json::String(part) => Ok(text.contains(part.as_str())),
            _ => Err(ScoreError::new("a text value can only contain text")),
        },
        Json::Object(members) => match item {
            Json::String(key) => Ok(members.iter().any(|(name, _)| name == key)),
            Json::Array(_) | Json::Object(_) => Err(unhashable()),
            _ => Ok(false),
        },
        _ => Err(ScoreError::new(
            "a value that is not a collection was searched",
        )),
    }
}

/// Looks `key` up in an object the way `dict.get` does for a JSON-derived key:
/// strings are looked up, numbers and `null` never match, lists and objects
/// are unhashable.
pub(super) fn dict_get<'a>(object: &'a Json, key: &Json) -> Res<Option<&'a Json>> {
    match key {
        Json::String(name) => Ok(object.get(name)),
        Json::Array(_) | Json::Object(_) => Err(unhashable()),
        _ => Ok(None),
    }
}

/// `object[key]` with a `KeyError` that names the field and its owner.
pub(super) fn required<'a>(object: &'a Json, key: &str, owner: &str) -> Res<&'a Json> {
    object
        .get(key)
        .ok_or_else(|| ScoreError::new(format!("{owner} lacks \"{key}\"")))
}

/// The members of a JSON object.
pub(super) fn members<'a>(value: &'a Json, what: &str) -> Res<&'a [(String, Json)]> {
    match value {
        Json::Object(pairs) => Ok(pairs),
        _ => Err(ScoreError::new(format!(
            "{what} must be a JSON object, found {}",
            value.type_name()
        ))),
    }
}

/// `object.get(key, [])` iterated: absent is empty, anything but an array is
/// reported.
pub(super) fn array_or_empty<'a>(object: &'a Json, key: &str, owner: &str) -> Res<&'a [Json]> {
    match object.get(key) {
        None => Ok(&[]),
        Some(Json::Array(items)) => Ok(items),
        Some(other) => Err(ScoreError::new(format!(
            "{owner} \"{key}\" must be an array, found {}",
            other.type_name()
        ))),
    }
}

/// An insertion-ordered map where setting an existing key keeps its position,
/// like a `dict`.
#[derive(Debug)]
pub(super) struct OrderedMap<V> {
    items: Vec<(String, V)>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<V> OrderedMap<V> {
    pub(super) fn set(&mut self, key: &str, value: V) {
        match self.items.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = value,
            None => self.items.push((key.to_string(), value)),
        }
    }

    pub(super) fn contains(&self, key: &str) -> bool {
        self.items.iter().any(|(name, _)| name == key)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.items
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}
