//! Typed JSON values with a strict parser and a deterministic writer.
//!
//! The workspace deliberately has no JSON dependency. [`Json`] is a small
//! typed value (objects keep insertion order, so output is deterministic),
//! [`parse_json`] is a strict RFC 8259 reader and the writers emit either a
//! compact or a two-space pretty-printed form.
//!
//! Parser contract (each point has a negative test):
//!
//! - Exactly one value; any trailing characters are an error.
//! - Duplicate object keys, trailing commas, leading zeros, unescaped control
//!   characters, lone surrogate escapes, a leading byte-order mark and
//!   nesting deeper than [`MAX_JSON_DEPTH`] are rejected.
//! - Integers that fit `i64` are [`Json::Int`]; every other number is a
//!   finite [`Json::Float`] (an integer outside the `i64` range is therefore
//!   read as a float and may lose precision). Numbers that overflow `f64` are
//!   rejected.
//! - Non-finite floats cannot be written in JSON; the writers emit `null` for
//!   them, like `JSON.stringify`.

use std::fmt::Write as _;

use crate::error::ParseError;
use crate::skills::SkillEntry;

/// Maximum array/object nesting accepted by [`parse_json`].
pub const MAX_JSON_DEPTH: usize = 128;

/// A typed JSON value. Objects are ordered key/value lists.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A number written without fraction or exponent that fits `i64`.
    Int(i64),
    /// Any other number.
    Float(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object; key order is insertion order and keys are unique when parsed.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Builds a [`Json::String`].
    #[must_use]
    pub fn string(value: impl Into<String>) -> Self {
        Self::String(value.into())
    }

    /// Builds a [`Json::Object`] from key/value pairs, keeping their order.
    #[must_use]
    pub fn object(pairs: Vec<(&str, Json)>) -> Self {
        Self::Object(
            pairs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// Looks up an object member by key; `None` for non-objects.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(pairs) => pairs
                .iter()
                .find(|(name, _)| name.as_str() == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The string content of a [`Json::String`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Lower-case JSON type name used in schema messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Int(_) => "integer",
            Self::Float(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    /// Single-line form without insignificant whitespace.
    #[must_use]
    pub fn to_compact_string(&self) -> String {
        let mut out = String::new();
        self.write_to(&mut out, false, 0);
        out
    }

    /// Two-space indented form; empty arrays and objects stay `[]` / `{}`.
    #[must_use]
    pub fn to_pretty_string(&self) -> String {
        let mut out = String::new();
        self.write_to(&mut out, true, 0);
        out
    }

    fn write_to(&self, out: &mut String, pretty: bool, level: usize) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(true) => out.push_str("true"),
            Self::Bool(false) => out.push_str("false"),
            Self::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Self::Float(value) => {
                if value.is_finite() {
                    // Debug keeps a fraction or exponent, so a float never
                    // reads back as an integer.
                    let _ = write!(out, "{value:?}");
                } else {
                    out.push_str("null");
                }
            }
            Self::String(value) => push_quoted(out, value),
            Self::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    newline_indent(out, pretty, level + 1);
                    item.write_to(out, pretty, level + 1);
                }
                newline_indent(out, pretty, level);
                out.push(']');
            }
            Self::Object(pairs) => {
                if pairs.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (index, (key, value)) in pairs.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    newline_indent(out, pretty, level + 1);
                    push_quoted(out, key);
                    out.push(':');
                    if pretty {
                        out.push(' ');
                    }
                    value.write_to(out, pretty, level + 1);
                }
                newline_indent(out, pretty, level);
                out.push('}');
            }
        }
    }
}

fn newline_indent(out: &mut String, pretty: bool, level: usize) {
    if pretty {
        out.push('\n');
        for _ in 0..level {
            out.push_str("  ");
        }
    }
}

fn push_quoted(out: &mut String, value: &str) {
    out.push('"');
    out.push_str(&escape_json_string(value));
    out.push('"');
}

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
            c if u32::from(c) < 0x20 => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(c));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// Serializes skill entries as a pretty-printed JSON array of objects with
/// `name`, `description` and `path` fields, in the order supplied (discovery
/// order is already the documented sorted order). An empty list is `[]`.
#[must_use]
pub fn skills_to_json(entries: &[SkillEntry]) -> String {
    Json::Array(entries.iter().map(SkillEntry::to_json).collect()).to_pretty_string()
}

/// Parses one JSON document.
///
/// # Errors
///
/// Returns a [`ParseError`] with a 1-based line and column when the input is
/// not exactly one valid JSON value (see the module documentation for the
/// strictness rules).
pub fn parse_json(input: &str) -> Result<Json, ParseError> {
    let mut parser = Parser {
        src: input,
        bytes: input.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("unexpected characters after the JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> ParseError {
        let end = self.pos.min(self.src.len());
        let mut line = 1;
        let mut column = 1;
        for (offset, c) in self.src.char_indices() {
            if offset >= end {
                break;
            }
            if c == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        ParseError {
            format: "JSON",
            line,
            column,
            message: message.to_string(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", char::from(byte))))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, ParseError> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.error("nesting is too deep"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, ParseError> {
        if self
            .src
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(word))
        {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.pos += 1;
        let mut pairs: Vec<(String, Json)> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(pairs));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string object key"));
            }
            let key_start = self.pos;
            let key = self.string()?;
            if pairs.iter().any(|(existing, _)| *existing == key) {
                self.pos = key_start;
                return Err(self.error("duplicate object key"));
            }
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            pairs.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(pairs));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // The scan stops only at ASCII bytes (or the end), so both ends
            // of the slice are character boundaries.
            out.push_str(&self.src[start..self.pos]);
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return Err(self.error("unescaped control character in string")),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), ParseError> {
        let Some(byte) = self.peek() else {
            return Err(self.error("unterminated escape sequence"));
        };
        self.pos += 1;
        match byte {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{08}'),
            b'f' => out.push('\u{0C}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let first = self.hex4()?;
                let code = if (0xD800..0xDC00).contains(&first) {
                    let has_pair = self
                        .src
                        .get(self.pos..)
                        .is_some_and(|rest| rest.starts_with("\\u"));
                    if !has_pair {
                        return Err(self.error("unpaired surrogate escape"));
                    }
                    self.pos += 2;
                    let second = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&second) {
                        return Err(self.error("invalid low surrogate escape"));
                    }
                    0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                } else if (0xDC00..0xE000).contains(&first) {
                    return Err(self.error("unpaired surrogate escape"));
                } else {
                    first
                };
                let Some(c) = char::from_u32(code) else {
                    return Err(self.error("invalid unicode escape"));
                };
                out.push(c);
            }
            _ => {
                self.pos -= 1;
                return Err(self.error("invalid escape sequence"));
            }
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(digit) = self.peek().and_then(|byte| char::from(byte).to_digit(16)) else {
                return Err(self.error("invalid \\u escape"));
            };
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Result<Json, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.error("invalid number")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.error("invalid number: digits required after '.'"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("invalid number: digits required in exponent"));
            }
        }
        let text = &self.src[start..self.pos];
        if !is_float {
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Json::Int(value));
            }
        }
        match text.parse::<f64>() {
            Ok(value) if value.is_finite() => Ok(Json::Float(value)),
            _ => Err(self.error("number is out of range")),
        }
    }
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

    fn parse_error(input: &str) -> ParseError {
        parse_json(input).expect_err("input must be rejected")
    }

    #[test]
    fn empty_list_is_bare_array() {
        assert_eq!(skills_to_json(&[]), "[]");
    }

    #[test]
    fn entries_serialize_in_supplied_order_with_exact_shape() {
        let json = skills_to_json(&[entry("a", "first", "x/a"), entry("b", "", "y/b")]);
        assert_eq!(
            json,
            "[\n  {\n    \"name\": \"a\",\n    \"description\": \"first\",\n    \"path\": \"x/a\"\n  },\n  {\n    \"name\": \"b\",\n    \"description\": \"\",\n    \"path\": \"y/b\"\n  }\n]"
        );
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
        let parsed = parse_json(&json).expect("round trip");
        let Json::Array(items) = &parsed else {
            panic!("expected an array, got {}", parsed.type_name());
        };
        assert_eq!(
            items[0].get("description").and_then(Json::as_str),
            Some("line one\nline two")
        );
    }

    #[test]
    fn writers_are_deterministic_and_keep_insertion_order() {
        let value = Json::object(vec![
            ("zebra", Json::Int(1)),
            ("apple", Json::Array(vec![Json::Null, Json::Bool(true)])),
            ("empty", Json::object(vec![])),
        ]);
        assert_eq!(
            value.to_compact_string(),
            "{\"zebra\":1,\"apple\":[null,true],\"empty\":{}}"
        );
        assert_eq!(
            value.to_pretty_string(),
            "{\n  \"zebra\": 1,\n  \"apple\": [\n    null,\n    true\n  ],\n  \"empty\": {}\n}"
        );
        assert_eq!(value.to_pretty_string(), value.to_pretty_string());
    }

    #[test]
    fn floats_keep_a_fraction_and_non_finite_become_null() {
        assert_eq!(Json::Float(1.0).to_compact_string(), "1.0");
        assert_eq!(Json::Float(0.5).to_compact_string(), "0.5");
        assert_eq!(Json::Float(f64::NAN).to_compact_string(), "null");
        assert_eq!(Json::Float(f64::INFINITY).to_compact_string(), "null");
        assert_eq!(
            parse_json("1.0").expect("float"),
            Json::Float(1.0),
            "a float does not collapse to an integer"
        );
    }

    #[test]
    fn parse_round_trips_every_value_type() {
        let text = "{\"a\":[1,-2,3.5,1e3,true,false,null],\"b\":{\"c\":\"d\"},\"e\":\"\"}";
        let value = parse_json(text).expect("valid json");
        assert_eq!(value.to_compact_string(), text.replace("1e3", "1000.0"));
        assert_eq!(
            parse_json(&value.to_pretty_string()).expect("pretty"),
            value
        );
        assert_eq!(
            value
                .get("b")
                .and_then(|b| b.get("c"))
                .and_then(Json::as_str),
            Some("d")
        );
        assert_eq!(value.get("missing"), None);
    }

    #[test]
    fn whitespace_around_the_value_is_accepted() {
        assert_eq!(
            parse_json(" \t\r\n[ 1 , 2 ]\n").expect("valid"),
            Json::Array(vec![Json::Int(1), Json::Int(2)])
        );
    }

    #[test]
    fn unicode_escapes_and_surrogate_pairs_decode() {
        assert_eq!(
            parse_json("\"\\u00e9\\ud83d\\ude00\\n\\/\"").expect("valid"),
            Json::string("é😀\n/")
        );
        assert_eq!(parse_json("\"é😀\"").expect("valid"), Json::string("é😀"));
    }

    #[test]
    fn integer_overflow_becomes_float_and_float_overflow_is_rejected() {
        assert_eq!(
            parse_json("9223372036854775807").expect("max"),
            Json::Int(i64::MAX)
        );
        assert!(matches!(
            parse_json("9223372036854775808").expect("float"),
            Json::Float(_)
        ));
        assert_eq!(parse_error("1e999").message, "number is out of range");
    }

    #[test]
    fn malformed_documents_are_rejected_with_positions() {
        for bad in [
            "",
            "   ",
            "[1,]",
            "{\"a\":1,}",
            "[1 2]",
            "{\"a\" 1}",
            "{a:1}",
            "'x'",
            "nul",
            "truex",
            "01",
            "-",
            "1.",
            "1e",
            ".5",
            "+1",
            "[",
            "{",
            "\"abc",
            "\"\\x41\"",
            "\"\\u12\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"\\ud800\\u0041\"",
            "\"a\u{1}b\"",
            "[] []",
            "\u{feff}[]",
            "NaN",
            "[1]]",
        ] {
            let error = parse_error(bad);
            assert_eq!(error.format, "JSON", "input {bad:?}");
            assert!(error.line >= 1 && error.column >= 1, "input {bad:?}");
        }
    }

    #[test]
    fn duplicate_keys_are_rejected_at_the_second_key() {
        let error = parse_error("{\n  \"a\": 1,\n  \"a\": 2\n}");
        assert_eq!(error.message, "duplicate object key");
        assert_eq!(error.line, 3);
        assert_eq!(error.column, 3);
    }

    #[test]
    fn depth_limit_is_enforced() {
        let ok = format!(
            "{}{}",
            "[".repeat(MAX_JSON_DEPTH),
            "]".repeat(MAX_JSON_DEPTH)
        );
        assert!(parse_json(&ok).is_ok());
        let deep = format!(
            "{}{}",
            "[".repeat(MAX_JSON_DEPTH + 2),
            "]".repeat(MAX_JSON_DEPTH + 2)
        );
        assert_eq!(parse_error(&deep).message, "nesting is too deep");
    }
}
