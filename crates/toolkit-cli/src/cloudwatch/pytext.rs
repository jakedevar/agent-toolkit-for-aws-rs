//! Python-compatible text helpers shared by the CloudWatch ports.
//!
//! The legacy modules build their output with f-strings, `str.strip()`,
//! `dict.get(key, default)` and `json.dumps(indent=2)`. The helpers here
//! reproduce exactly those renderings for the JSON value space; nothing is a
//! general Python emulation. Value formatting (`str`/`repr`) is reused from
//! the shared [`crate::dynamodb::pyfmt`] module.

use std::fmt::Write as _;

use toolkit_core::Json;

use crate::dynamodb::pyfmt;

/// `str.isspace()` for one character: Unicode white space plus the four
/// ASCII separators `\x1c`-`\x1f` that Python also treats as white space.
#[must_use]
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
#[must_use]
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.rstrip()` with no argument.
#[must_use]
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// `format(value, ",")` for a non-negative integer.
#[must_use]
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `f"{value}"` of an optional string: `None` renders as `None`.
#[must_use]
pub fn opt_text(value: Option<&str>) -> &str {
    value.unwrap_or("None")
}

/// `f"{value}"` of an optional integer: `None` renders as `None`.
#[must_use]
pub fn opt_int(value: Option<i64>) -> String {
    value.map_or_else(|| "None".to_string(), |number| number.to_string())
}

/// `data.get(key, default)` rendered with `str()`: the default only applies
/// when the key is absent, a JSON `null` renders as `None`.
#[must_use]
pub fn get_text(data: &Json, key: &str, default: &str) -> String {
    data.get(key)
        .map_or_else(|| default.to_string(), pyfmt::text)
}

/// Python truthiness of `data.get(key)`.
#[must_use]
pub fn get_truthy(data: &Json, key: &str) -> bool {
    data.get(key).is_some_and(pyfmt::truthy)
}

/// `str(data.get(key))` when truthy, else `None` (the `if value:` idiom).
#[must_use]
pub fn get_truthy_text(data: &Json, key: &str) -> Option<String> {
    data.get(key)
        .filter(|value| pyfmt::truthy(value))
        .map(pyfmt::text)
}

/// The items of `data.get(key, [])` when it is a JSON array; anything else
/// reads as an empty list.
#[must_use]
pub fn get_array<'a>(data: &'a Json, key: &str) -> &'a [Json] {
    match data.get(key) {
        Some(Json::Array(items)) => items,
        _ => &[],
    }
}

/// The members of an object value, if it is one.
#[must_use]
pub fn members(value: &Json) -> Option<&[(String, Json)]> {
    match value {
        Json::Object(pairs) => Some(pairs),
        _ => None,
    }
}

/// `isinstance(value, dict)` members of `data.get(key)`.
#[must_use]
pub fn get_members<'a>(data: &'a Json, key: &str) -> Option<&'a [(String, Json)]> {
    data.get(key).and_then(members)
}

/// Whether `data.get(key)` is present and not `null` (`is not None`).
#[must_use]
pub fn has_value(data: &Json, key: &str) -> bool {
    !matches!(data.get(key), None | Some(Json::Null))
}

fn push_ascii_string(out: &mut String, text: &str) {
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
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn push_indent(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    for _ in 0..indent * level {
        out.push(' ');
    }
}

fn write_value(out: &mut String, value: &Json, indent: usize, level: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) if number.is_nan() => out.push_str("NaN"),
        Json::Float(number) if number.is_infinite() => {
            out.push_str(if *number < 0.0 {
                "-Infinity"
            } else {
                "Infinity"
            });
        }
        Json::Float(number) => out.push_str(&pyfmt::float_repr(*number)),
        Json::String(text) => push_ascii_string(out, text),
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
                push_indent(out, indent, level + 1);
                write_value(out, item, indent, level + 1);
            }
            push_indent(out, indent, level);
            out.push(']');
        }
        Json::Object(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_indent(out, indent, level + 1);
                push_ascii_string(out, key);
                out.push_str(": ");
                write_value(out, item, indent, level + 1);
            }
            push_indent(out, indent, level);
            out.push('}');
        }
    }
}

/// `json.dumps(value, indent=2)`: insertion-ordered keys, `ensure_ascii`,
/// `", "`-free item separators (a bare `,` before the newline) and `": "`
/// key separators; empty containers stay `[]` / `{}`.
#[must_use]
pub fn dumps_indent2(value: &Json) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 2, 0);
    out
}

/// Builds a JSON object from owned keys, keeping their order.
#[must_use]
pub fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::object(pairs)
}

/// A JSON string array.
#[must_use]
pub fn string_array(items: &[String]) -> Json {
    Json::Array(
        items
            .iter()
            .map(|item| Json::string(item.as_str()))
            .collect(),
    )
}

/// The clone of `data.get(key)`, `null` when absent.
#[must_use]
pub fn clone_or_null(data: &Json, key: &str) -> Json {
    data.get(key).cloned().unwrap_or(Json::Null)
}

/// True for a character that must never reach a terminal or a report line
/// unescaped: C0 and C1 controls (ESC, CR, BEL, DEL, NEL, ...), the Unicode
/// line/paragraph separators, and the invisible direction-control and BOM
/// characters that can reorder or hide text (joiners stay, for emoji).
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}'
                | '\u{200e}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

fn escape_unsafe(text: &str, keep_layout: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\t' if keep_layout => out.push(c),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_unsafe_char(c) => {
                let code = u32::from(c);
                let _ = if code <= 0xff {
                    write!(out, "\\x{code:02x}")
                } else if code <= 0xffff {
                    write!(out, "\\u{code:04x}")
                } else {
                    write!(out, "\\U{code:08x}")
                };
            }
            c => out.push(c),
        }
    }
    out
}

/// A caller-derived value made safe to embed in one diagnostic line: every
/// control (newline, carriage return, tab, ESC, ...) and invisible format
/// character is replaced by a visible Python-style escape (`\n`, `\x1b`,
/// `\uXXXX`). Ordinary text, including non-ASCII letters, is unchanged, and
/// the backslash is left alone.
#[must_use]
pub fn safe_text(text: &str) -> String {
    escape_unsafe(text, false)
}

/// Like [`safe_text`] but keeps newlines and tabs, for multi-line text the
/// toolkit itself authors (bullets, trailers, whole reports): terminal
/// controls are still escaped, so the block cannot clear or rewrite the
/// terminal. Never use it for caller-supplied text: that goes through
/// [`safe_text`], so it cannot add a line (#4652, #4653).
#[must_use]
pub fn safe_block(text: &str) -> String {
    escape_unsafe(text, true)
}

/// [`safe_text`] of an optional string, `None` rendering as `None`.
#[must_use]
pub fn safe_opt(value: Option<&str>) -> String {
    safe_text(opt_text(value))
}

/// [`safe_text`] of `str(value)` for a JSON value.
#[must_use]
pub fn safe_json(value: &Json) -> String {
    safe_text(&pyfmt::text(value))
}
