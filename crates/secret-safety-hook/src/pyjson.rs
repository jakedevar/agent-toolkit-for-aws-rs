//! Python-compatible serialization of the tool input.
//!
//! The legacy hook scanned `json.dumps(tool_input)`: ASCII only (everything
//! outside printable ASCII is `\uXXXX` escaped) with `", "` and `": "`
//! separators. The scanners still run on that exact text, in addition to the
//! raw strings, so the Rust hook can never match less than the legacy one.

use std::fmt::Write as _;

use toolkit_core::Json;

/// Serializes like `json.dumps(value)` with default arguments.
pub(crate) fn python_dumps(value: &Json) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(_) | Json::Float(_) => out.push_str(&value.to_compact_string()),
        Json::String(text) => write_string(text, out),
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            out.push('{');
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_string(key, out);
                out.push_str(": ");
                write_value(item, out);
            }
            out.push('}');
        }
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buffer = [0u16; 2];
                let length = c.encode_utf16(&mut buffer).len();
                for unit in &buffer[..length] {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_json_dumps() {
        let value = Json::object(vec![
            (
                "a",
                Json::Array(vec![Json::Int(1), Json::string("\u{e9}\u{1f600}\n")]),
            ),
            ("b", Json::Null),
            ("c", Json::Bool(true)),
            ("d", Json::Bool(false)),
            ("e", Json::object(vec![])),
            ("f", Json::Array(vec![])),
        ]);
        assert_eq!(
            python_dumps(&value),
            r#"{"a": [1, "\u00e9\ud83d\ude00\n"], "b": null, "c": true, "d": false, "e": {}, "f": []}"#
        );
    }

    #[test]
    fn escapes_quotes_backslashes_and_controls() {
        let value = Json::string("a\"b\\c\td\u{7f}\u{1}\u{8}\u{c}\r");
        assert_eq!(python_dumps(&value), r#""a\"b\\c\td\u007f\u0001\b\f\r""#);
    }

    #[test]
    fn non_ascii_whitespace_is_hidden_like_in_the_legacy_text() {
        let text = python_dumps(&Json::string("a\u{a0}b"));
        assert_eq!(text, r#""a\u00a0b""#);
    }
}
