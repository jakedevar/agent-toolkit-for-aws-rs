//! Python-compatible `json.loads` / `json.dumps` for the artifact validator.
//!
//! `validate-artifacts.py` reads contract examples and run artifacts with
//! `json.loads`, which is more lenient than the strict reader in
//! `toolkit-core`: a duplicated object key keeps its first position and its
//! last value, and `NaN`, `Infinity` and `-Infinity` are numbers. Its error
//! messages (`Expecting ',' delimiter`, ...) and line numbers are part of the
//! `SCHEMA_PARSE` findings, so this reader reproduces them. Two deliberate
//! differences are recorded in `docs/migration/startup-validators.md`: an
//! integer outside the `i64` range becomes a float, and a lone surrogate
//! escape becomes U+FFFD (a Rust `String` cannot hold one).

use std::fmt::Write as _;

use toolkit_core::Json;

use crate::manifests::py_float_repr;

/// Deepest array/object nesting read; Python raises `RecursionError` far
/// below a stack overflow, this reader reports an error instead.
const MAX_DEPTH: usize = 500;

/// A `json.JSONDecodeError`: the bare message and the 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsonError {
    pub(crate) msg: String,
    pub(crate) pos: usize,
    pub(crate) lineno: usize,
    pub(crate) colno: usize,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: line {} column {} (char {})",
            self.msg, self.lineno, self.colno, self.pos
        )
    }
}

struct Reader {
    chars: Vec<char>,
}

type Step<T> = Result<T, (String, usize)>;

/// `json.loads(text)`.
pub(crate) fn loads(text: &str) -> Result<Json, JsonError> {
    let reader = Reader {
        chars: text.chars().collect(),
    };
    reader.document().map_err(|(msg, pos)| {
        let before = &reader.chars[..pos.min(reader.chars.len())];
        let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
        let colno = before
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(pos + 1, |p| pos - p);
        JsonError {
            msg,
            pos,
            lineno,
            colno,
        }
    })
}

fn is_json_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Reader {
    fn document(&self) -> Step<Json> {
        if self.chars.first() == Some(&'\u{feff}') {
            return Err((
                "Unexpected UTF-8 BOM (decode using utf-8-sig)".to_string(),
                0,
            ));
        }
        let start = self.skip_space(0);
        let (value, end) = self.value(start, 0)?;
        let end = self.skip_space(end);
        if end != self.chars.len() {
            return Err(("Extra data".to_string(), end));
        }
        Ok(value)
    }

    fn skip_space(&self, mut at: usize) -> usize {
        while self.chars.get(at).is_some_and(|c| is_json_space(*c)) {
            at += 1;
        }
        at
    }

    fn starts_with(&self, at: usize, word: &str) -> bool {
        word.chars()
            .enumerate()
            .all(|(offset, c)| self.chars.get(at + offset) == Some(&c))
    }

    fn value(&self, at: usize, depth: usize) -> Step<(Json, usize)> {
        let expecting = || ("Expecting value".to_string(), at);
        let Some(&c) = self.chars.get(at) else {
            return Err(expecting());
        };
        match c {
            '"' => self
                .string(at + 1)
                .map(|(text, end)| (Json::String(text), end)),
            '{' => self.object(at + 1, depth),
            '[' => self.array(at + 1, depth),
            'n' if self.starts_with(at, "null") => Ok((Json::Null, at + 4)),
            't' if self.starts_with(at, "true") => Ok((Json::Bool(true), at + 4)),
            'f' if self.starts_with(at, "false") => Ok((Json::Bool(false), at + 5)),
            'N' if self.starts_with(at, "NaN") => Ok((Json::Float(f64::NAN), at + 3)),
            'I' if self.starts_with(at, "Infinity") => Ok((Json::Float(f64::INFINITY), at + 8)),
            '-' if self.starts_with(at, "-Infinity") => {
                Ok((Json::Float(f64::NEG_INFINITY), at + 9))
            }
            _ => self.number(at).ok_or_else(expecting),
        }
    }

    fn number(&self, at: usize) -> Option<(Json, usize)> {
        let mut end = at;
        if self.chars.get(end) == Some(&'-') {
            end += 1;
        }
        match self.chars.get(end) {
            Some('0') => end += 1,
            Some(c) if ('1'..='9').contains(c) => {
                while self.chars.get(end).is_some_and(char::is_ascii_digit) {
                    end += 1;
                }
            }
            _ => return None,
        }
        let mut is_float = false;
        if self.chars.get(end) == Some(&'.')
            && self.chars.get(end + 1).is_some_and(char::is_ascii_digit)
        {
            end += 1;
            while self.chars.get(end).is_some_and(char::is_ascii_digit) {
                end += 1;
            }
            is_float = true;
        }
        if matches!(self.chars.get(end), Some('e' | 'E')) {
            let mut look = end + 1;
            if matches!(self.chars.get(look), Some('+' | '-')) {
                look += 1;
            }
            if self.chars.get(look).is_some_and(char::is_ascii_digit) {
                while self.chars.get(look).is_some_and(char::is_ascii_digit) {
                    look += 1;
                }
                end = look;
                is_float = true;
            }
        }
        let text: String = self.chars[at..end].iter().collect();
        let value = if is_float {
            Json::Float(text.parse::<f64>().ok()?)
        } else {
            text.parse::<i64>()
                .map_or_else(
                    |_| text.parse::<f64>().map(Json::Float),
                    |n| Ok(Json::Int(n)),
                )
                .ok()?
        };
        Some((value, end))
    }

    /// `at` is just after the opening quote.
    fn string(&self, at: usize) -> Step<(String, usize)> {
        let begin = at - 1;
        let unterminated = || ("Unterminated string starting at".to_string(), begin);
        let mut out = String::new();
        let mut pos = at;
        loop {
            let c = *self.chars.get(pos).ok_or_else(unterminated)?;
            match c {
                '"' => return Ok((out, pos + 1)),
                '\\' => {
                    let escape = *self.chars.get(pos + 1).ok_or_else(unterminated)?;
                    pos += 2;
                    let plain = match escape {
                        '"' => Some('"'),
                        '\\' => Some('\\'),
                        '/' => Some('/'),
                        'b' => Some('\u{8}'),
                        'f' => Some('\u{c}'),
                        'n' => Some('\n'),
                        'r' => Some('\r'),
                        't' => Some('\t'),
                        'u' => None,
                        _ => return Err(("Invalid \\escape".to_string(), pos - 2)),
                    };
                    if let Some(plain) = plain {
                        out.push(plain);
                        continue;
                    }
                    let (unit, next) = self.hex4(pos, pos - 2)?;
                    pos = next;
                    if (0xd800..0xdc00).contains(&unit)
                        && self.chars.get(pos) == Some(&'\\')
                        && self.chars.get(pos + 1) == Some(&'u')
                    {
                        if let Ok((low, after)) = self.hex4(pos + 2, pos) {
                            if (0xdc00..0xe000).contains(&low) {
                                let scalar = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                                out.push(char::from_u32(scalar).unwrap_or('\u{fffd}'));
                                pos = after;
                                continue;
                            }
                        }
                    }
                    out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
                }
                c if u32::from(c) < 0x20 => {
                    return Err(("Invalid control character at".to_string(), pos));
                }
                c => {
                    out.push(c);
                    pos += 1;
                }
            }
        }
    }

    fn hex4(&self, at: usize, error_at: usize) -> Step<(u32, usize)> {
        let invalid = || ("Invalid \\uXXXX escape".to_string(), error_at);
        let mut unit = 0_u32;
        for offset in 0..4 {
            let digit = self
                .chars
                .get(at + offset)
                .and_then(|c| c.to_digit(16))
                .ok_or_else(invalid)?;
            unit = unit * 16 + digit;
        }
        Ok((unit, at + 4))
    }

    fn array(&self, at: usize, depth: usize) -> Step<(Json, usize)> {
        if depth >= MAX_DEPTH {
            return Err(("maximum recursion depth exceeded".to_string(), at));
        }
        let mut items = Vec::new();
        let mut pos = self.skip_space(at);
        if self.chars.get(pos) == Some(&']') {
            return Ok((Json::Array(items), pos + 1));
        }
        loop {
            let (value, end) = self.value(pos, depth + 1)?;
            items.push(value);
            pos = self.skip_space(end);
            match self.chars.get(pos) {
                Some(']') => return Ok((Json::Array(items), pos + 1)),
                Some(',') => pos = self.skip_space(pos + 1),
                _ => return Err(("Expecting ',' delimiter".to_string(), pos)),
            }
        }
    }

    fn object(&self, at: usize, depth: usize) -> Step<(Json, usize)> {
        if depth >= MAX_DEPTH {
            return Err(("maximum recursion depth exceeded".to_string(), at));
        }
        let mut members: Vec<(String, Json)> = Vec::new();
        let mut pos = self.skip_space(at);
        if self.chars.get(pos) == Some(&'}') {
            return Ok((Json::Object(members), pos + 1));
        }
        loop {
            if self.chars.get(pos) != Some(&'"') {
                return Err((
                    "Expecting property name enclosed in double quotes".to_string(),
                    pos,
                ));
            }
            let (key, end) = self.string(pos + 1)?;
            pos = self.skip_space(end);
            if self.chars.get(pos) != Some(&':') {
                return Err(("Expecting ':' delimiter".to_string(), pos));
            }
            pos = self.skip_space(pos + 1);
            let (value, end) = self.value(pos, depth + 1)?;
            // dict(pairs): the first position, the last value.
            if let Some(slot) = members.iter_mut().find(|slot| slot.0 == key) {
                slot.1 = value;
            } else {
                members.push((key, value));
            }
            pos = self.skip_space(end);
            match self.chars.get(pos) {
                Some('}') => return Ok((Json::Object(members), pos + 1)),
                Some(',') => pos = self.skip_space(pos + 1),
                _ => return Err(("Expecting ',' delimiter".to_string(), pos)),
            }
        }
    }
}

/// `json.dumps(value, indent=indent, sort_keys=sort_keys)` with the default
/// `ensure_ascii=True`.
pub(crate) fn dumps(value: &Json, indent: Option<usize>, sort_keys: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, indent, sort_keys, 0);
    out
}

fn write_value(out: &mut String, value: &Json, indent: Option<usize>, sort: bool, level: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) => {
            let text = if number.is_nan() {
                "NaN".to_string()
            } else if number.is_infinite() {
                let word = if *number > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                word.to_string()
            } else {
                py_float_repr(*number)
            };
            out.push_str(&text);
        }
        Json::String(text) => write_string(out, text),
        Json::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                separator(out, indent, level + 1, index == 0);
                write_value(out, item, indent, sort, level + 1);
            }
            close(out, indent, level, ']');
        }
        Json::Object(members) => {
            if members.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut ordered: Vec<&(String, Json)> = members.iter().collect();
            if sort {
                ordered.sort_by(|a, b| a.0.cmp(&b.0));
            }
            out.push('{');
            for (index, (key, member)) in ordered.into_iter().enumerate() {
                separator(out, indent, level + 1, index == 0);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, member, indent, sort, level + 1);
            }
            close(out, indent, level, '}');
        }
    }
}

fn separator(out: &mut String, indent: Option<usize>, level: usize, first: bool) {
    match indent {
        Some(width) => {
            if !first {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(width * level));
        }
        None => {
            if !first {
                out.push_str(", ");
            }
        }
    }
}

fn close(out: &mut String, indent: Option<usize>, level: usize, bracket: char) {
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * level));
    }
    out.push(bracket);
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
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (' '..='\u{7e}').contains(&c) || c == '\u{7f}' => out.push(c),
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
