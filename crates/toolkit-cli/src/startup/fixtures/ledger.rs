//! The assertion ledger and the Python-compatible views the fixture asserters
//! share (#4408).
//!
//! Every `check_expected_*.py` program is a checklist: it appends one message
//! per failed assertion to a module-level list, optionally notes a few facts,
//! and ends with one `PASS` line or a `FAIL` block and exit status 1. The
//! [`Ledger`] is that list. Run artifacts are read with Python's `dict.get`
//! semantics through the free functions below (an absent key, or a key of a
//! value that is not an object, is `None`), and the committed expectation
//! document through [`Spec`], which records a missing or wrongly typed key as a
//! failure instead of raising `KeyError` (a fixture that cannot be evaluated
//! must never pass).

use std::cell::RefCell;

use toolkit_core::Json;

use crate::manifests::{is_falsy, py_eq, py_repr};

/// `None`, for the views below.
pub(super) static NULL: Json = Json::Null;

/// Longest message the report echoes. The views below return the full,
/// Python-compatible text (`str(v)`, `repr(v)`) because assertions compare and
/// search it; only the finished message is bounded (#4746), so a run value
/// can never be cut short before an assertion looks at it.
const MESSAGE_CHARS: usize = 1200;

/// Failed assertions and notes, in the order the program found them.
#[derive(Default)]
pub(in crate::startup) struct Ledger {
    fails: RefCell<Vec<String>>,
    notes: RefCell<Vec<String>>,
}

impl Ledger {
    /// `check(cond, msg)`: records `msg` unless `cond` holds.
    pub(super) fn check(&self, cond: bool, msg: impl Into<String>) {
        if !cond {
            self.fail(msg);
        }
    }

    /// `FAILS.append(msg)`.
    pub(super) fn fail(&self, msg: impl Into<String>) {
        self.fails.borrow_mut().push(bounded(&msg.into()));
    }

    /// `NOTES.append(msg)`.
    pub(super) fn note(&self, msg: impl Into<String>) {
        self.notes.borrow_mut().push(bounded(&msg.into()));
    }

    /// Checks the shape of a design document named `label` (#4778). This is
    /// the one reader every design consumer goes through.
    ///
    /// The root must be an object. A supplied `services` must be a list whose
    /// elements are objects. An absent `services` keeps the legacy
    /// empty-list default. A supplied `null`, any other type, or a non-object
    /// element is a named failure: it never reads as an empty list or as a
    /// service with blank fields. Returns whether the shape held.
    pub(super) fn design_shape(&self, design: &Json, label: &str) -> bool {
        let Json::Object(_) = design else {
            self.fail(format!("{label} is {}, not an object", type_name(design)));
            return false;
        };
        let Some(services) = design.get("services") else {
            return true;
        };
        let Json::Array(entries) = services else {
            self.fail(format!(
                "{label} services is {}, not a list",
                type_name(services)
            ));
            return false;
        };
        let mut ok = true;
        for (index, entry) in entries.iter().enumerate() {
            if !matches!(entry, Json::Object(_)) {
                self.fail(format!(
                    "{label} services[{index}] is {}, not an object",
                    type_name(entry)
                ));
                ok = false;
            }
        }
        ok
    }

    pub(super) fn fails(&self) -> Vec<String> {
        self.fails.borrow().clone()
    }

    pub(super) fn notes(&self) -> Vec<String> {
        self.notes.borrow().clone()
    }

    /// A [`Spec`] over the expectation document `value`, named `label` in the
    /// failures it records.
    pub(super) fn spec<'a>(&'a self, value: &'a Json, label: &str) -> Spec<'a> {
        Spec {
            value,
            ledger: self,
            at: label.to_string(),
            absent: false,
        }
    }
}

/// `dict.get(key)`: `None` for an absent key and for a non-object.
pub(super) fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or(&NULL)
}

/// `value.get(a).get(b)...` along `path` (every step an object member).
pub(super) fn path<'a>(value: &'a Json, keys: &[&str]) -> &'a Json {
    let mut node = value;
    for key in keys {
        node = get(node, key);
    }
    node
}

/// A dotted path (`a.b.c`) walked the way `cur.get(seg) if isinstance(cur,
/// dict) else None` does.
pub(super) fn dotted<'a>(value: &'a Json, dotted: &str) -> &'a Json {
    let mut node = value;
    for part in dotted.split('.') {
        node = get(node, part);
    }
    node
}

/// The members of a JSON array; empty for anything else.
pub(super) fn items(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        _ => &[],
    }
}

/// The array members that are objects (`isinstance(e, dict)`).
pub(super) fn objs(value: &Json) -> impl Iterator<Item = &Json> {
    items(value)
        .iter()
        .filter(|item| matches!(item, Json::Object(_)))
}

/// The members of an object; empty for anything else.
pub(super) fn members(value: &Json) -> &[(String, Json)] {
    match value {
        Json::Object(members) => members,
        _ => &[],
    }
}

/// Python truthiness.
pub(super) fn truthy(value: &Json) -> bool {
    !is_falsy(value)
}

/// `value.get(key) or default` for an array: the array, else empty.
pub(super) fn list<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    items(get(value, key))
}

/// The string content, if the value is a string.
pub(super) fn text(value: &Json) -> Option<&str> {
    value.as_str()
}

/// `isinstance(v, (int, float)) and not isinstance(v, bool)` as a float.
#[allow(clippy::cast_precision_loss)]
pub(super) fn number(value: &Json) -> Option<f64> {
    match value {
        Json::Int(n) => Some(*n as f64),
        Json::Float(n) => Some(*n),
        _ => None,
    }
}

/// `isinstance(v, (int, float))`: a Python `bool` is an `int`.
#[allow(clippy::cast_precision_loss)]
pub(super) fn number_or_bool(value: &Json) -> Option<f64> {
    match value {
        Json::Bool(flag) => Some(f64::from(u8::from(*flag))),
        other => number(other),
    }
}

/// Python `==` of two values.
pub(super) fn eq(left: &Json, right: &Json) -> bool {
    py_eq(left, right)
}

/// Python `==` against a string literal.
pub(super) fn eq_str(value: &Json, expected: &str) -> bool {
    value.as_str() == Some(expected)
}

/// `str(value)`: a string as itself, anything else as its `repr`. Never
/// shortened; a message is bounded when it is recorded.
pub(super) fn ps(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => rp(other),
    }
}

/// `repr(value)`, in full.
pub(super) fn rp(value: &Json) -> String {
    py_repr(value)
}

/// `repr` of a string.
pub(super) fn rs(text: &str) -> String {
    crate::manifests::py_repr_str(text)
}

/// At most [`MESSAGE_CHARS`] characters of a recorded message: run artifacts
/// are caller text, so a report echoes only a bounded prefix of them.
fn bounded(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(MESSAGE_CHARS).collect();
    let dropped = chars.count();
    if dropped == 0 {
        head
    } else {
        format!("{head}...[{dropped} more chars]")
    }
}

/// `repr` of a list of strings.
pub(super) fn rl<S: AsRef<str>>(items: &[S]) -> String {
    let inner: Vec<String> = items
        .iter()
        .map(|item| crate::manifests::py_repr_str(item.as_ref()))
        .collect();
    format!("[{}]", inner.join(", "))
}

/// `json.dumps(value)` (ASCII output).
pub(super) fn dumps(value: &Json) -> String {
    crate::startup::artifacts::pyjson::dumps(value, None, false)
}

/// `json.dumps(value, ensure_ascii=False)`.
pub(super) fn dumps_unicode(value: &Json) -> String {
    crate::startup::artifacts::pyjson::dumps_unicode(value)
}

/// `math.isclose(a, b, abs_tol=abs_tol)` with the default `rel_tol` of 1e-9.
#[allow(clippy::float_cmp)]
pub(super) fn isclose(a: f64, b: f64, abs_tol: f64) -> bool {
    if a == b {
        return true;
    }
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    let diff = (a - b).abs();
    diff <= (1e-9 * a.abs().max(b.abs())).max(abs_tol)
}

/// `round(value, 2)` as the message text Python prints.
pub(super) fn r2(value: f64) -> String {
    toolkit_msk::pyfmt::py_float_repr(toolkit_msk::pyfmt::round_to(value, 2))
}

/// `str.strip()` with Python's whitespace.
pub(super) fn strip(text: &str) -> &str {
    crate::manifests::py_strip(text)
}

/// `type(value).__name__`.
pub(super) fn type_name(value: &Json) -> &'static str {
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

/// A typed window on the committed expectation document. A missing key, or a
/// value of the wrong type, is recorded as a failure (the Python program would
/// have raised) and reads as an empty value.
#[derive(Clone)]
pub(in crate::startup) struct Spec<'a> {
    value: &'a Json,
    ledger: &'a Ledger,
    at: String,
    /// The key was missing (already recorded); typed reads stay quiet.
    absent: bool,
}

impl<'a> Spec<'a> {
    fn malformed(&self, what: &str) {
        if self.absent {
            return;
        }
        self.ledger.fail(format!(
            "expectation document is malformed: {} {what}",
            self.at
        ));
    }

    /// `spec["key"]`: records a failure when the key is absent.
    pub(super) fn at(&self, key: &str) -> Spec<'a> {
        let child = format!("{}.{key}", self.at);
        if let Some(value) = self.value.get(key) {
            Spec {
                value,
                ledger: self.ledger,
                at: child,
                absent: false,
            }
        } else {
            self.ledger.fail(format!(
                "expectation document is malformed: {child} is missing"
            ));
            Spec {
                value: &NULL,
                ledger: self.ledger,
                at: child,
                absent: true,
            }
        }
    }

    /// `spec.get("key")`: silently `None` when absent.
    pub(super) fn get(&self, key: &str) -> Spec<'a> {
        Spec {
            value: self.value.get(key).unwrap_or(&NULL),
            ledger: self.ledger,
            at: format!("{}.{key}", self.at),
            absent: false,
        }
    }

    pub(super) fn json(&self) -> &'a Json {
        self.value
    }

    pub(super) fn truthy(&self) -> bool {
        truthy(self.value)
    }

    pub(super) fn is_null(&self) -> bool {
        matches!(self.value, Json::Null)
    }

    pub(super) fn has(&self, key: &str) -> bool {
        self.value.get(key).is_some()
    }

    /// The string value (`""` and a recorded failure when it is not a string).
    pub(super) fn str(&self) -> &'a str {
        if let Some(text) = self.value.as_str() {
            text
        } else {
            self.malformed("is not a string");
            ""
        }
    }

    pub(super) fn f64(&self) -> f64 {
        if let Some(n) = number(self.value) {
            n
        } else {
            self.malformed("is not a number");
            0.0
        }
    }

    pub(super) fn i64(&self) -> i64 {
        if let Json::Int(n) = self.value {
            *n
        } else {
            self.malformed("is not an integer");
            0
        }
    }

    /// The array members as specs (empty and a recorded failure otherwise).
    pub(super) fn arr(&self) -> Vec<Spec<'a>> {
        if let Json::Array(array) = self.value {
            array
                .iter()
                .enumerate()
                .map(|(index, value)| Spec {
                    value,
                    ledger: self.ledger,
                    at: format!("{}[{index}]", self.at),
                    absent: false,
                })
                .collect()
        } else {
            self.malformed("is not an array");
            Vec::new()
        }
    }

    /// The array members as specs; empty when the value is absent or null.
    pub(super) fn arr_or_empty(&self) -> Vec<Spec<'a>> {
        if matches!(self.value, Json::Null) {
            Vec::new()
        } else {
            self.arr()
        }
    }

    /// The object members as specs; empty when the value is absent or null.
    pub(super) fn entries_or_empty(&self) -> Vec<(&'a str, Spec<'a>)> {
        if matches!(self.value, Json::Null) {
            Vec::new()
        } else {
            self.entries()
        }
    }

    /// An array of strings.
    pub(super) fn strs(&self) -> Vec<&'a str> {
        self.arr().iter().map(Spec::str).collect()
    }

    /// The object members as `(key, spec)` pairs.
    pub(super) fn entries(&self) -> Vec<(&'a str, Spec<'a>)> {
        if let Json::Object(members) = self.value {
            members
                .iter()
                .map(|(key, value)| {
                    (
                        key.as_str(),
                        Spec {
                            value,
                            ledger: self.ledger,
                            at: format!("{}.{key}", self.at),
                            absent: false,
                        },
                    )
                })
                .collect()
        } else {
            self.malformed("is not an object");
            Vec::new()
        }
    }

    /// The object members whose key does not start with `_` (the documentation
    /// keys the expectation documents interleave with data).
    pub(super) fn real_entries(&self) -> Vec<(&'a str, Spec<'a>)> {
        self.entries()
            .into_iter()
            .filter(|(key, _)| !key.starts_with('_'))
            .collect()
    }

    /// `spec["why"]`-style documentation text; a missing note reads as empty
    /// rather than failing, because it only ends up in a message.
    pub(super) fn note(&self, key: &str) -> &'a str {
        self.value.get(key).and_then(Json::as_str).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    #[test]
    fn views_follow_dict_get_semantics() {
        let doc = parse_json(r#"{"a": {"b": [1, {"c": null}]}, "n": 3, "t": "x"}"#).unwrap();
        assert!(matches!(get(&doc, "zzz"), Json::Null));
        assert!(matches!(get(get(&doc, "n"), "k"), Json::Null));
        assert_eq!(items(path(&doc, &["a", "b"])).len(), 2);
        assert_eq!(objs(path(&doc, &["a", "b"])).count(), 1);
        assert!(items(get(&doc, "t")).is_empty());
        assert_eq!(dotted(&doc, "a.b").type_name(), "array");
        assert_eq!(number(&Json::Bool(true)), None);
        assert_eq!(number_or_bool(&Json::Bool(true)), Some(1.0));
    }

    #[test]
    fn a_missing_spec_key_is_a_recorded_failure() {
        let doc = parse_json(r#"{"a": [1, "x"], "s": "t"}"#).unwrap();
        let ledger = Ledger::default();
        let spec = ledger.spec(&doc, "exp");
        assert_eq!(spec.at("s").str(), "t");
        assert!(ledger.fails().is_empty());
        assert_eq!(spec.at("zzz").str(), "");
        assert_eq!(spec.at("s").i64(), 0);
        assert_eq!(spec.at("a").strs().len(), 2);
        let fails = ledger.fails();
        assert_eq!(fails.len(), 3, "{fails:?}");
        assert!(fails[0].contains("exp.zzz is missing"), "{fails:?}");
        assert!(fails[2].contains("exp.a[0] is not a string"), "{fails:?}");
    }

    #[test]
    fn shown_values_are_bounded_python_reprs() {
        assert_eq!(rp(&Json::Null), "None");
        assert_eq!(rp(&Json::Bool(true)), "True");
        assert_eq!(ps(&Json::string("plain")), "plain");
        let long = Json::string("x".repeat(500));
        assert_eq!(rp(&long).chars().count(), 502);
        let ledger = Ledger::default();
        let huge = Json::string("x".repeat(5000));
        ledger.fail(format!("{} tail", rp(&huge)));
        let shown = ledger.fails().remove(0);
        assert!(shown.ends_with("more chars]"), "{shown}");
        assert!(shown.chars().count() < MESSAGE_CHARS + 40);
        let long = Json::string("y".repeat(5000));
        ledger.fail(ps(&long));
        assert!(ledger.fails()[1].ends_with("more chars]"));
    }
}
