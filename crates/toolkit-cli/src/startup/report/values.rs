//! Python-flavoured reads of the parsed estimation and design artifacts:
//! truthiness, `dict.get` chains and the `float()`, `int()` and `str()`
//! conversions the validator applies to JSON values.

use toolkit_core::Json;

use crate::manifests::{is_falsy, py_float_repr, py_strip};

/// `bool(value)` for an optional value (`None` is falsy).
pub(super) fn truthy(value: Option<&Json>) -> bool {
    value.is_some_and(|inner| !is_falsy(inner))
}

/// `value.get(a, {}).get(b, {})...`: the member at the end of the path, or
/// `None` as soon as a step is not an object holding the key.
pub(super) fn chain<'a>(value: &'a Json, path: &[&str]) -> Option<&'a Json> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}

/// `float(value)`: numbers, booleans and numeric strings convert; anything
/// else (and a string that is not a number) is `None`.
#[allow(clippy::cast_precision_loss)]
pub(super) fn py_float(value: &Json) -> Option<f64> {
    match value {
        Json::Int(number) => Some(*number as f64),
        Json::Float(number) => Some(*number),
        Json::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Json::String(text) => {
            let trimmed = py_strip(text);
            if trimmed.is_empty() || trimmed.contains(['_', ' ']) {
                return None;
            }
            trimmed.parse::<f64>().ok()
        }
        _ => None,
    }
}

/// `int(value)`: integers, booleans, finite floats (truncated) and integer
/// strings convert; anything else is `None`.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn py_int(value: &Json) -> Option<i64> {
    match value {
        Json::Int(number) => Some(*number),
        Json::Bool(flag) => Some(i64::from(*flag)),
        Json::Float(number) => {
            let whole = number.trunc();
            // The i64 range is [-2^63, 2^63); both bounds are exact in f64.
            if whole.is_finite()
                && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&whole)
            {
                Some(whole as i64)
            } else {
                None
            }
        }
        Json::String(text) => py_strip(text).parse::<i64>().ok(),
        _ => None,
    }
}

/// `str(value)`.
pub(super) fn py_str(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(number) => number.to_string(),
        Json::Float(number) => py_float_repr(*number),
        Json::String(text) => text.clone(),
        other => crate::manifests::py_repr(other),
    }
}

/// `str(value or "")`.
pub(super) fn py_str_or_empty(value: Option<&Json>) -> String {
    match value {
        Some(inner) if !is_falsy(inner) => py_str(inner),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_follow_python() {
        assert_eq!(py_float(&Json::string(" 12.5 ")), Some(12.5));
        assert_eq!(py_float(&Json::Bool(true)), Some(1.0));
        assert_eq!(py_float(&Json::string("abc")), None);
        assert_eq!(py_float(&Json::Array(vec![])), None);
        assert_eq!(py_int(&Json::Float(2.9)), Some(2));
        assert_eq!(py_int(&Json::Float(-2.9)), Some(-2));
        assert_eq!(py_int(&Json::Float(f64::NAN)), None);
        assert_eq!(py_int(&Json::string("7")), Some(7));
        assert_eq!(py_int(&Json::string("7.5")), None);
        assert_eq!(py_int(&Json::Null), None);
    }

    #[test]
    fn chains_stop_at_a_non_object() {
        let value = Json::object(vec![(
            "a",
            Json::object(vec![("b", Json::Int(1)), ("c", Json::Null)]),
        )]);
        assert_eq!(chain(&value, &["a", "b"]), Some(&Json::Int(1)));
        assert_eq!(chain(&value, &["a", "c", "d"]), None);
        assert_eq!(chain(&value, &["x"]), None);
        assert!(truthy(chain(&value, &["a", "b"])));
        assert!(!truthy(chain(&value, &["a", "c"])));
        assert!(!truthy(None));
    }

    #[test]
    fn strings_render_like_python() {
        assert_eq!(py_str(&Json::Bool(true)), "True");
        assert_eq!(py_str(&Json::Null), "None");
        assert_eq!(py_str(&Json::Float(1.0)), "1.0");
        assert_eq!(py_str_or_empty(None), "");
        assert_eq!(py_str_or_empty(Some(&Json::Int(0))), "");
        assert_eq!(py_str_or_empty(Some(&Json::string("x"))), "x");
    }
}
