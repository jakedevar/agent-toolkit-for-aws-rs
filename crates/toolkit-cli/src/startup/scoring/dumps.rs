//! `json.dumps(value, indent=2)` as the scoring scripts print and write it:
//! two-space indent, `", "`-free separators (`,` at line end, `": "` between
//! key and value), insertion-ordered keys, `ensure_ascii=True` and Python's
//! float `repr`.

use std::fmt::Write as _;

use toolkit_core::Json;

use crate::dynamodb::pyfmt::float_repr;

/// Renders `value` like Python's `json.dumps(value, indent=2)` (no trailing
/// newline).
pub(crate) fn dumps_indent2(value: &Json) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out
}

fn newline(out: &mut String, level: usize) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Json, level: usize) {
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
        Json::Float(number) => out.push_str(&float_repr(*number)),
        Json::String(text) => write_string(out, text),
        Json::Array(items) if items.is_empty() => out.push_str("[]"),
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write_value(out, item, level + 1);
            }
            newline(out, level);
            out.push(']');
        }
        Json::Object(pairs) if pairs.is_empty() => out.push_str("{}"),
        Json::Object(pairs) => {
            out.push('{');
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, item, level + 1);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

/// A JSON string with everything outside space..`~` escaped, non-BMP
/// characters as surrogate pairs.
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
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}
