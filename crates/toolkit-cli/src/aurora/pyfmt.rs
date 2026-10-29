//! Python-compatible numbers, rounding, formatting and `json.dumps`.
//!
//! The Aurora analyzers print through Python `round`, `f"{x:.0f}"`, `str(float)`
//! and `json.dumps(..., indent=2)`. Their output is a published format, so the
//! pieces that differ from Rust defaults live here once:
//!
//! - [`Num`] keeps the Python `int` / `float` distinction (`round(0, 1)` is the
//!   integer `0`, `min(1.0, 256)` can return the integer `256`), because it
//!   shows in the JSON (`0` versus `0.0`).
//! - [`round_to`] is `round(x, n)`: the exact binary value rounded half to even.
//! - [`fmt_f`] / [`fmt_grouped`] are `:.Nf` / `:,.Nf` (`nan` and `inf` spelled
//!   like Python).
//! - [`dumps`] is `json.dumps(value, indent=2)` with `ensure_ascii`, Python float
//!   `repr` and `NaN` / `Infinity`.

use std::fmt::Write as _;

use toolkit_core::Json;

use crate::manifests::py_float_repr;

/// A Python number: an `int` (kept in `i64`) or a `float`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// The value as a float (Python converts an `int` operand the same way).
    pub(crate) fn value(self) -> f64 {
        match self {
            Self::Int(number) => number as f64,
            Self::Float(number) => number,
        }
    }

    /// The JSON number: `int` stays an integer, `float` stays a float.
    pub(crate) fn to_json(self) -> Json {
        match self {
            Self::Int(number) => Json::Int(number),
            Self::Float(number) => Json::Float(number),
        }
    }

    /// `str(number)`.
    pub(crate) fn py_str(self) -> String {
        match self {
            Self::Int(number) => number.to_string(),
            Self::Float(number) => py_float_repr(number),
        }
    }

    /// Python `a + b`: an int only when both are ints.
    pub(crate) fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => left
                .checked_add(right)
                .map_or_else(|| Self::Float(left as f64 + right as f64), Self::Int),
            (left, right) => Self::Float(left.value() + right.value()),
        }
    }

    /// The number of a JSON value (`None` for anything that is not a number).
    pub(crate) fn from_json(value: &Json) -> Option<Self> {
        match value {
            Json::Int(number) => Some(Self::Int(*number)),
            Json::Float(number) => Some(Self::Float(*number)),
            _ => None,
        }
    }
}

/// Python `min(first, second)`: the second operand only when strictly smaller.
pub(crate) fn py_min(first: Num, second: Num) -> Num {
    if second.value() < first.value() {
        second
    } else {
        first
    }
}

/// Python `max(first, second)`: the second operand only when strictly larger.
pub(crate) fn py_max(first: Num, second: Num) -> Num {
    if second.value() > first.value() {
        second
    } else {
        first
    }
}

/// Python `round(value, digits)` for a float: the exact binary value rounded
/// half to even (infinities and NaN are returned unchanged).
pub(crate) fn round_to(value: f64, digits: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{value:.digits$}").parse().unwrap_or(value)
}

/// Python `round(number, digits)`: an `int` stays an `int`.
pub(crate) fn round_num(number: Num, digits: usize) -> Num {
    match number {
        Num::Int(_) => number,
        Num::Float(value) => Num::Float(round_to(value, digits)),
    }
}

/// Python `f"{value:.{digits}f}"`.
pub(crate) fn fmt_f(value: f64, digits: usize) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        let text = if value > 0.0 { "inf" } else { "-inf" };
        text.to_string()
    } else {
        format!("{value:.digits$}")
    }
}

/// Python `f"{value:,.{digits}f}"` (thousands separators).
pub(crate) fn fmt_grouped(value: f64, digits: usize) -> String {
    let plain = fmt_f(value, digits);
    if !value.is_finite() {
        return plain;
    }
    let (sign, unsigned) = plain
        .strip_prefix('-')
        .map_or(("", plain.as_str()), |rest| ("-", rest));
    let (whole, fraction) = unsigned
        .split_once('.')
        .map_or((unsigned, String::new()), |(whole, fraction)| {
            (whole, format!(".{fraction}"))
        });
    // Walk the digits from the right, starting a new group after three.
    let mut reversed = String::new();
    let mut run = 0;
    for digit in whole.chars().rev() {
        if run == 3 {
            reversed.push(',');
            run = 0;
        }
        reversed.push(digit);
        run += 1;
    }
    let grouped: String = reversed.chars().rev().collect();
    format!("{sign}{grouped}{fraction}")
}

/// Python `str.title()` for the ASCII words used in source tags.
pub(crate) fn title_case(text: &str) -> String {
    let mut out = String::new();
    let mut start_of_word = true;
    for c in text.chars() {
        if c.is_alphabetic() {
            if start_of_word {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start_of_word = false;
        } else {
            out.push(c);
            start_of_word = true;
        }
    }
    out
}

/// Python `json.dumps(value, indent=2)` (default `ensure_ascii=True`).
pub(crate) fn dumps(value: &Json) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out
}

fn write_value(out: &mut String, value: &Json, level: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) => {
            if number.is_nan() {
                out.push_str("NaN");
            } else if number.is_infinite() {
                out.push_str(if *number > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                });
            } else {
                out.push_str(&py_float_repr(*number));
            }
        }
        Json::String(text) => write_string(out, text),
        Json::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                indent(out, level + 1);
                write_value(out, item, level + 1);
            }
            indent(out, level);
            out.push(']');
        }
        Json::Object(members) => {
            if members.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (index, (key, member)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                indent(out, level + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, member, level + 1);
            }
            indent(out, level);
            out.push('}');
        }
    }
}

fn indent(out: &mut String, level: usize) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// The numeric value of `key` in a JSON object, or `default` when absent or not
/// a number (Python `result.get(key, default)` on a numeric field).
pub(crate) fn get_f64(value: &Json, key: &str, default: f64) -> f64 {
    value
        .get(key)
        .and_then(Num::from_json)
        .map_or(default, Num::value)
}

/// The string at `key`, or `default`.
pub(crate) fn get_str<'a>(value: &'a Json, key: &str, default: &'a str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap_or(default)
}

/// Python truthiness of an optional JSON value (`None` is false).
pub(crate) fn truthy(value: Option<&Json>) -> bool {
    value.is_some_and(|json| match json {
        Json::Null | Json::Bool(false) | Json::Int(0) => false,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(members) => !members.is_empty(),
        Json::Bool(true) | Json::Int(_) => true,
    })
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    // T-BR-OUT-1: byte equality is separate from parsed-value equality.
    #[test]
    fn t_br_out_1_compact_ensure_ascii() {
        let value = Json::Array(vec![Json::String("é😀\u{7f}\0\n\t\u{8}\u{c}\r\"\\".into()), Json::Float(1.5)]);
        let encoded = dumps_compact(&value);
        assert_eq!(encoded.as_bytes(), br#"["\u00e9\ud83d\ude00\u007f\u0000\n\t\b\f\r\"\\",1.5]"#);
        assert_eq!(toolkit_core::json::parse_json(encoded.as_bytes()).unwrap(), value);
    }
}
