//! Hand-rolled JSON emission for skill entries.
//!
//! The bootstrap slice has no JSON dependency: this module emits a small,
//! stable document shape with RFC 8259 string escaping. Parsing is not
//! provided; tests assert on the exact serialized shape.

use std::fmt::Write as _;

use crate::skills::SkillEntry;

/// Escapes a string for inclusion in a JSON document.
#[must_use]
pub fn escape_json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for unit in value.chars() {
        match unit {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0C}' => escaped.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(escaped, "\\u{:04x}", c as u32);
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// Serializes skill entries as a pretty-printed JSON array with `name`,
/// `description` and `path` fields, in the order supplied (discovery order is
/// already the documented sorted order).
#[must_use]
pub fn skills_to_json(entries: &[SkillEntry]) -> String {
    if entries.is_empty() {
        return "[]".to_string();
    }
    let mut document = String::from("[\n");
    for (index, entry) in entries.iter().enumerate() {
        let _ = write!(
            document,
            "  {{\n    \"name\": \"{}\",\n    \"description\": \"{}\",\n    \"path\": \"{}\"\n  }}",
            escape_json_string(&entry.name),
            escape_json_string(&entry.description),
            escape_json_string(&entry.path)
        );
        if index + 1 < entries.len() {
            document.push(',');
        }
        document.push('\n');
    }
    document.push(']');
    document
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str, path: &str) -> SkillEntry {
        SkillEntry {
            name: name.to_string(),
            description: description.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn empty_list_is_bare_array() {
        assert_eq!(skills_to_json(&[]), "[]");
    }

    #[test]
    fn entries_serialize_in_supplied_order() {
        let json = skills_to_json(&[entry("a", "first", "x/a"), entry("b", "", "y/b")]);
        let a = json.find("\"name\": \"a\"").expect("a present");
        let b = json.find("\"name\": \"b\"").expect("b present");
        assert!(a < b);
        assert!(json.starts_with("[\n  {\n"));
        assert!(json.ends_with(']'));
        assert!(json.contains("\"path\": \"x/a\""));
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(
            escape_json_string("q\"ote\\slash\nline\ttab"),
            "q\\\"ote\\\\slash\\nline\\ttab"
        );
        assert_eq!(escape_json_string("\u{1}"), "\\u0001");
        assert_eq!(escape_json_string("é"), "é");
    }

    #[test]
    fn descriptions_with_newlines_stay_valid_json() {
        let json = skills_to_json(&[entry("s", "line one\nline two", "p")]);
        assert!(json.contains("\"description\": \"line one\\nline two\""));
        assert!(!json.contains("line one\nline two\""));
    }
}
