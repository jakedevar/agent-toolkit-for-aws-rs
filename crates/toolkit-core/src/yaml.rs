//! A strict, documented YAML *subset* that parses into [`Json`].
//!
//! This is not a YAML engine. It reads the plain configuration-style documents
//! the toolkit needs and rejects everything it does not understand with a
//! line-numbered [`ParseError`] instead of guessing.
//!
//! Supported:
//!
//! - One document, with an optional leading `---` line and an optional `...`
//!   end line (only comments or blanks may follow it). Comments (`# ...`) and
//!   blank lines anywhere.
//! - Block mappings and block sequences, nested by indentation with spaces. A
//!   sequence may sit at the same indent as its parent key. `- key: value`
//!   starts a mapping inside a sequence item. Mapping keys are always
//!   strings.
//! - Plain scalars (typed: `null`/`~`/empty, `true`/`false`, JSON-grammar
//!   integers and floats, otherwise strings), single-quoted scalars (`''` is
//!   a quote) and double-quoted scalars with the escapes `\\ \" \/ \n \t \r
//!   \0 \b \f \xXX \uXXXX`. Quoted scalars are always strings.
//! - Block scalars `|`, `>`, `|-` and `>-`. Folded (`>`) joins lines with a
//!   space and a blank line becomes a newline; more-indented lines are
//!   folded like any other line.
//! - Flow collections on one line: `[a, "b", 1]`, `{k: v}` (nested allowed),
//!   with an optional trailing comma. Flow keys and flow plain scalars follow
//!   the same start rules as block ones (see below); quoted forms are always
//!   accepted as literal text.
//!
//! Rejected with an error: tab indentation, anchors/aliases/tags (`&`, `*`,
//! `!`), directives, complex keys (`? `), duplicate keys, multiple documents,
//! multi-line plain or quoted scalars, block-scalar indentation/`+` chomping
//! indicators, `key: - item` on one line and nesting deeper than
//! [`MAX_YAML_DEPTH`]. The anchor/alias/tag rule and the reserved-indicator
//! rule (`%`, `@`, `` ` ``) also apply to plain keys and plain scalars inside
//! flow collections (`{&a k: v}`, `{*a: v}`, `{!t k: v}`, `[]` are errors;
//! `{"&a k": v}` is the literal key `&a k`). The depth budget is a single
//! one for the whole document: block mappings, block sequences (including
//! same-indent sequences) and flow collections all count towards at most
//! [`MAX_YAML_DEPTH`] container levels. YAML 1.1 booleans (`yes`/`no`/`on`/`off`), numbers with
//! leading zeros, `+` signs, hex/octal and `.inf`/`.nan` stay strings.

use crate::error::ParseError;
use crate::json::{parse_json, Json};

/// Maximum block or flow nesting accepted by [`parse_yaml`].
pub const MAX_YAML_DEPTH: usize = 64;

struct Line<'a> {
    number: usize,
    indent: usize,
    text: &'a str,
    raw: &'a str,
}

impl Line<'_> {
    fn is_significant(&self) -> bool {
        !self.text.is_empty() && !self.text.starts_with('#')
    }
}

fn error(number: usize, message: &str) -> ParseError {
    ParseError {
        format: "YAML",
        line: number,
        column: 0,
        message: message.to_string(),
    }
}

/// Parses one YAML document (see the module documentation for the supported
/// subset). An empty document is [`Json::Null`].
///
/// # Errors
///
/// Returns a [`ParseError`] naming the 1-based line of the first unsupported
/// or malformed construct.
pub fn parse_yaml(input: &str) -> Result<Json, ParseError> {
    let content = input.strip_prefix('\u{feff}').unwrap_or(input);
    let lines = split_lines(content)?;
    let mut parser = Parser { lines, index: 0 };
    let Some(first) = parser.next_significant() else {
        return Ok(Json::Null);
    };
    let indent = parser.lines[first].indent;
    let value = parser.parse_block(indent, 0)?;
    if let Some(extra) = parser.next_significant() {
        return Err(error(
            parser.lines[extra].number,
            "unexpected content (inconsistent indentation)",
        ));
    }
    Ok(value)
}

fn split_lines(content: &str) -> Result<Vec<Line<'_>>, ParseError> {
    let mut lines = Vec::new();
    let mut started = false;
    let mut ended = false;
    for (index, raw) in content.lines().enumerate() {
        let number = index + 1;
        let body = raw.trim_start_matches(' ');
        let indent = raw.len() - body.len();
        let text = body.trim_end();
        if body.starts_with('\t') && !text.is_empty() {
            return Err(error(number, "tabs are not allowed for indentation"));
        }
        let significant = !text.is_empty() && !text.starts_with('#');
        if ended {
            if significant {
                return Err(error(number, "content after the document end marker"));
            }
            continue;
        }
        if indent == 0 && text == "..." {
            ended = true;
            continue;
        }
        if indent == 0 && (text == "---" || text.starts_with("--- ")) {
            if started {
                return Err(error(number, "multiple documents are not supported"));
            }
            if text != "---" {
                return Err(error(
                    number,
                    "content on the document start line is not supported",
                ));
            }
            started = true;
            continue;
        }
        if significant {
            started = true;
        }
        lines.push(Line {
            number,
            indent,
            text,
            raw,
        });
    }
    Ok(lines)
}

struct Parser<'a> {
    lines: Vec<Line<'a>>,
    index: usize,
}

impl<'a> Parser<'a> {
    fn next_significant(&self) -> Option<usize> {
        (self.index..self.lines.len()).find(|&position| self.lines[position].is_significant())
    }

    /// Enforces the single document-wide nesting budget: `depth` is the
    /// zero-based level of the container about to be parsed, counting block
    /// and flow containers (and same-indent sequences) alike, so at most
    /// [`MAX_YAML_DEPTH`] container levels are accepted.
    fn check_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth >= MAX_YAML_DEPTH {
            let number = self
                .next_significant()
                .map_or(0, |position| self.lines[position].number);
            return Err(error(number, "nesting is too deep"));
        }
        Ok(())
    }

    fn parse_block(&mut self, indent: usize, depth: usize) -> Result<Json, ParseError> {
        let Some(position) = self.next_significant() else {
            return Ok(Json::Null);
        };
        let number = self.lines[position].number;
        let text = self.lines[position].text;
        if is_sequence_item(text) {
            return self.parse_sequence(indent, depth);
        }
        if split_entry(text, number)?.is_some() {
            return self.parse_mapping(indent, depth);
        }
        self.index = position + 1;
        self.scalar_value(text, indent, number, depth)
    }

    fn parse_sequence(&mut self, indent: usize, depth: usize) -> Result<Json, ParseError> {
        self.check_depth(depth)?;
        let mut items = Vec::new();
        while let Some(position) = self.next_significant() {
            let number = self.lines[position].number;
            let line_indent = self.lines[position].indent;
            let text: &'a str = self.lines[position].text;
            if line_indent < indent {
                break;
            }
            if line_indent > indent {
                return Err(error(number, "unexpected indentation"));
            }
            if !is_sequence_item(text) {
                break;
            }
            let after = strip_leading_comment(text[1..].trim_start());
            if after.is_empty() {
                self.index = position + 1;
                items.push(self.nested_value(indent, false, depth)?);
                continue;
            }
            let offset = text.len() - after.len();
            if is_sequence_item(after) || split_entry(after, number)?.is_some() {
                // Treat the item's content as if it started its own line at
                // the column where it really begins.
                self.lines[position].indent = indent + offset;
                self.lines[position].text = after;
                self.index = position;
                items.push(self.parse_block(indent + offset, depth + 1)?);
            } else {
                self.index = position + 1;
                items.push(self.scalar_value(after, indent, number, depth + 1)?);
            }
        }
        Ok(Json::Array(items))
    }

    fn parse_mapping(&mut self, indent: usize, depth: usize) -> Result<Json, ParseError> {
        self.check_depth(depth)?;
        let mut pairs: Vec<(String, Json)> = Vec::new();
        while let Some(position) = self.next_significant() {
            let number = self.lines[position].number;
            let line_indent = self.lines[position].indent;
            let text: &'a str = self.lines[position].text;
            if line_indent < indent {
                break;
            }
            if line_indent > indent {
                return Err(error(
                    number,
                    "unexpected indentation (multi-line plain scalars are not supported)",
                ));
            }
            if is_sequence_item(text) {
                return Err(error(number, "unexpected sequence entry inside a mapping"));
            }
            let Some((key, rest)) = split_entry(text, number)? else {
                return Err(error(number, "expected a 'key: value' mapping entry"));
            };
            if pairs.iter().any(|(existing, _)| *existing == key) {
                return Err(error(number, &format!("duplicate mapping key '{key}'")));
            }
            self.index = position + 1;
            let rest = strip_leading_comment(rest);
            let value = if rest.is_empty() {
                self.nested_value(indent, true, depth)?
            } else {
                self.scalar_value(rest, indent, number, depth + 1)?
            };
            pairs.push((key, value));
        }
        Ok(Json::Object(pairs))
    }

    /// The value after an empty `key:` or `-`: a more-indented block, or (for
    /// keys only) a sequence at the key's own indent, otherwise null.
    fn nested_value(
        &mut self,
        parent_indent: usize,
        allow_same_indent_sequence: bool,
        depth: usize,
    ) -> Result<Json, ParseError> {
        let Some(position) = self.next_significant() else {
            return Ok(Json::Null);
        };
        let line_indent = self.lines[position].indent;
        if line_indent > parent_indent {
            return self.parse_block(line_indent, depth + 1);
        }
        if allow_same_indent_sequence
            && line_indent == parent_indent
            && is_sequence_item(self.lines[position].text)
        {
            return self.parse_sequence(parent_indent, depth + 1);
        }
        Ok(Json::Null)
    }

    /// Parses the scalar or flow collection at the start of `text`.
    /// `container_depth` is the level a flow collection starting here would
    /// occupy in the document-wide nesting budget.
    fn scalar_value(
        &mut self,
        text: &str,
        parent_indent: usize,
        number: usize,
        container_depth: usize,
    ) -> Result<Json, ParseError> {
        let Some(first) = text.chars().next() else {
            return Ok(Json::Null);
        };
        match first {
            '|' | '>' => self.block_scalar(text, parent_indent, number),
            '"' | '\'' => {
                let (value, used) = parse_quoted(text, number)?;
                expect_end(&text[used..], number, "quoted scalar")?;
                Ok(Json::String(value))
            }
            '[' | '{' => {
                let mut flow = Flow {
                    chars: text.chars().collect(),
                    pos: 0,
                    number,
                };
                let value = flow.value(container_depth)?;
                flow.finish()?;
                Ok(value)
            }
            _ => {
                reject_indicator(first, number)?;
                plain_scalar(text, number)
            }
        }
    }

    fn block_scalar(
        &mut self,
        header: &str,
        parent_indent: usize,
        number: usize,
    ) -> Result<Json, ParseError> {
        let (folded, strip) = match strip_trailing_comment(header) {
            "|" => (false, false),
            "|-" => (false, true),
            ">" => (true, false),
            ">-" => (true, true),
            _ => {
                return Err(error(
                    number,
                    "unsupported block scalar header (only |, >, |- and >- are supported)",
                ));
            }
        };
        let mut content_indent: Option<usize> = None;
        let mut collected: Vec<&str> = Vec::new();
        let mut position = self.index;
        let mut end = self.index;
        while position < self.lines.len() {
            let (is_blank, indent, line_number, raw) = {
                let line = &self.lines[position];
                (line.text.is_empty(), line.indent, line.number, line.raw)
            };
            if is_blank {
                collected.push("");
                position += 1;
                continue;
            }
            if indent <= parent_indent {
                break;
            }
            let base = *content_indent.get_or_insert(indent);
            if indent < base {
                return Err(error(line_number, "inconsistent block scalar indentation"));
            }
            collected.push(&raw[base..]);
            position += 1;
            end = position;
        }
        self.index = end;
        while collected.last() == Some(&"") {
            collected.pop();
        }
        let mut value = if folded {
            fold(&collected)
        } else {
            collected.join("\n")
        };
        if !strip && !collected.is_empty() {
            value.push('\n');
        }
        Ok(Json::String(value))
    }
}

fn fold(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut previous_was_text = false;
    for line in lines {
        if line.is_empty() {
            out.push('\n');
            previous_was_text = false;
        } else {
            if previous_was_text {
                out.push(' ');
            }
            out.push_str(line);
            previous_was_text = true;
        }
    }
    out
}

fn is_sequence_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn strip_leading_comment(text: &str) -> &str {
    if text.starts_with('#') {
        ""
    } else {
        text
    }
}

fn strip_trailing_comment(text: &str) -> &str {
    match text.find(" #") {
        Some(position) => text[..position].trim_end(),
        None => text,
    }
}

/// Splits `key: rest` for a mapping entry; `None` when the line is not one.
fn split_entry(text: &str, number: usize) -> Result<Option<(String, &str)>, ParseError> {
    if text == "?" || text.starts_with("? ") {
        return Err(error(number, "complex mapping keys are not supported"));
    }
    match text.chars().next() {
        Some('"' | '\'') => {
            let Ok((key, used)) = parse_quoted(text, number) else {
                return Ok(None);
            };
            if let Some(after) = text[used..].trim_start().strip_prefix(':') {
                if after.is_empty() || after.starts_with(' ') {
                    return Ok(Some((key, after.trim_start())));
                }
            }
            Ok(None)
        }
        Some('[' | '{' | '&' | '*' | '!' | '|' | '>' | '#' | '%' | '@' | '`') | None => Ok(None),
        Some(_) => {
            let bytes = text.as_bytes();
            for (position, byte) in bytes.iter().enumerate() {
                if *byte == b':' && (position + 1 == bytes.len() || bytes[position + 1] == b' ') {
                    let key = text[..position].trim_end();
                    if key.is_empty() {
                        return Err(error(number, "empty mapping key"));
                    }
                    if key.contains(" #") {
                        return Ok(None);
                    }
                    return Ok(Some((key.to_string(), text[position + 1..].trim_start())));
                }
            }
            Ok(None)
        }
    }
}

/// The one rule for how a plain scalar or plain key may begin, shared by block
/// and flow contexts: anchors, aliases, tags and reserved indicators are
/// errors (quote the text to use it literally).
fn reject_indicator(first: char, number: usize) -> Result<(), ParseError> {
    match first {
        '&' | '*' | '!' => Err(error(number, "anchors, aliases and tags are not supported")),
        '%' | '@' | '`' => Err(error(
            number,
            "a scalar cannot start with a reserved indicator",
        )),
        _ => Ok(()),
    }
}

fn plain_scalar(text: &str, number: usize) -> Result<Json, ParseError> {
    if is_sequence_item(text) {
        return Err(error(
            number,
            "block sequence entries are not allowed on a key line",
        ));
    }
    if text == "?" || text.starts_with("? ") {
        return Err(error(number, "complex mapping keys are not supported"));
    }
    let content = strip_trailing_comment(text);
    if content.contains(": ") || content.ends_with(':') {
        return Err(error(
            number,
            "a mapping value is not allowed inside a plain scalar",
        ));
    }
    Ok(resolve_plain(content))
}

fn resolve_plain(text: &str) -> Json {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Json::Null,
        "true" | "True" | "TRUE" => return Json::Bool(true),
        "false" | "False" | "FALSE" => return Json::Bool(false),
        _ => {}
    }
    if matches!(text.as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
        if let Ok(number @ (Json::Int(_) | Json::Float(_))) = parse_json(text) {
            return number;
        }
    }
    Json::String(text.to_string())
}

/// Accepts only whitespace and a `#` comment (after at least one space).
fn expect_end(rest: &str, number: usize, what: &str) -> Result<(), ParseError> {
    let trimmed = rest.trim_start();
    if trimmed.is_empty() || (trimmed.len() < rest.len() && trimmed.starts_with('#')) {
        Ok(())
    } else {
        Err(error(number, &format!("unexpected text after {what}")))
    }
}

/// Parses a quoted scalar at the start of `text`; returns the value and the
/// number of bytes consumed (including both quotes).
fn parse_quoted(text: &str, number: usize) -> Result<(String, usize), ParseError> {
    let mut chars = text.char_indices();
    let Some((_, quote)) = chars.next() else {
        return Err(error(number, "empty scalar"));
    };
    let mut value = String::new();
    while let Some((offset, c)) = chars.next() {
        if c == quote {
            if quote == '\'' && text[offset + 1..].starts_with('\'') {
                chars.next();
                value.push('\'');
                continue;
            }
            return Ok((value, offset + 1));
        }
        if quote == '"' && c == '\\' {
            let Some((_, escape)) = chars.next() else {
                break;
            };
            match escape {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'n' => value.push('\n'),
                't' => value.push('\t'),
                'r' => value.push('\r'),
                '0' => value.push('\0'),
                'b' => value.push('\u{08}'),
                'f' => value.push('\u{0C}'),
                'x' => value.push(read_hex(&mut chars, 2, number)?),
                'u' => value.push(read_hex(&mut chars, 4, number)?),
                other => {
                    return Err(error(
                        number,
                        &format!("unsupported escape sequence '\\{other}'"),
                    ));
                }
            }
            continue;
        }
        value.push(c);
    }
    Err(error(
        number,
        "unterminated quoted scalar (multi-line quoted scalars are not supported)",
    ))
}

fn read_hex(
    chars: &mut std::str::CharIndices<'_>,
    digits: usize,
    number: usize,
) -> Result<char, ParseError> {
    let mut code = 0u32;
    for _ in 0..digits {
        let Some(digit) = chars.next().and_then(|(_, c)| c.to_digit(16)) else {
            return Err(error(number, "invalid hexadecimal escape"));
        };
        code = code * 16 + digit;
    }
    char::from_u32(code).ok_or_else(|| error(number, "invalid unicode escape"))
}

/// One-line flow collection reader (`[...]` and `{...}`).
struct Flow {
    chars: Vec<char>,
    pos: usize,
    number: usize,
}

impl Flow {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(' ') {
            self.pos += 1;
        }
    }

    fn fail(&self, message: &str) -> ParseError {
        error(self.number, message)
    }

    fn finish(&mut self) -> Result<(), ParseError> {
        self.skip_spaces();
        match self.peek() {
            None => Ok(()),
            Some('#') if self.pos > 0 && self.chars[self.pos - 1] == ' ' => Ok(()),
            Some(_) => Err(self.fail("unexpected text after a flow collection")),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.skip_spaces();
        match self.peek() {
            Some('[') => self.sequence(depth),
            Some('{') => self.mapping(depth),
            Some('"' | '\'') => self.quoted().map(Json::String),
            Some(_) => {
                let text = self.plain(false);
                if text.is_empty() {
                    return Err(self.fail("empty flow entry"));
                }
                self.check_plain_start(&text)?;
                if text.contains(": ") {
                    return Err(self.fail("a mapping value is not allowed inside a plain scalar"));
                }
                Ok(resolve_plain(&text))
            }
            None => Err(self.fail("unexpected end of a flow collection")),
        }
    }

    fn quoted(&mut self) -> Result<String, ParseError> {
        let rest: String = self.chars[self.pos..].iter().collect();
        let (value, used) = parse_quoted(&rest, self.number)?;
        self.pos += rest[..used].chars().count();
        Ok(value)
    }

    /// Reads a plain scalar up to `,`, `]` or `}` (and `:` when reading a key).
    fn plain(&mut self, is_key: bool) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if matches!(c, ',' | ']' | '}') || (is_key && c == ':') {
                break;
            }
            self.pos += 1;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    }

    fn check_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth >= MAX_YAML_DEPTH {
            return Err(self.fail("nesting is too deep"));
        }
        Ok(())
    }

    /// Applies the block-context rules for how a plain scalar or key may begin.
    fn check_plain_start(&self, text: &str) -> Result<(), ParseError> {
        let first = text.chars().next().unwrap_or(' ');
        reject_indicator(first, self.number)?;
        if matches!(first, '|' | '>') {
            return Err(self.fail("block scalars are not allowed inside a flow collection"));
        }
        if text == "?" || text.starts_with("? ") {
            return Err(self.fail("complex mapping keys are not supported"));
        }
        Ok(())
    }

    fn sequence(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.check_depth(depth)?;
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_spaces();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Ok(Json::Array(items));
            }
            items.push(self.value(depth + 1)?);
            self.skip_spaces();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.fail("expected ',' or ']' in a flow sequence")),
            }
        }
    }

    fn mapping(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.check_depth(depth)?;
        self.pos += 1;
        let mut pairs: Vec<(String, Json)> = Vec::new();
        loop {
            self.skip_spaces();
            if self.peek() == Some('}') {
                self.pos += 1;
                return Ok(Json::Object(pairs));
            }
            let quoted_key = matches!(self.peek(), Some('"' | '\''));
            let key = if quoted_key {
                self.quoted()?
            } else {
                self.plain(true)
            };
            if key.is_empty() {
                return Err(self.fail("empty key in a flow mapping"));
            }
            if !quoted_key {
                self.check_plain_start(&key)?;
            }
            if pairs.iter().any(|(existing, _)| *existing == key) {
                return Err(self.fail(&format!("duplicate mapping key '{key}'")));
            }
            self.skip_spaces();
            if self.peek() != Some(':') {
                return Err(self.fail("expected ':' after a flow mapping key"));
            }
            self.pos += 1;
            self.skip_spaces();
            let value = if matches!(self.peek(), Some(',' | '}')) {
                Json::Null
            } else {
                self.value(depth + 1)?
            };
            pairs.push((key, value));
            self.skip_spaces();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(Json::Object(pairs));
                }
                _ => return Err(self.fail("expected ',' or '}' in a flow mapping")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(text: &str) -> Json {
        parse_yaml(text).expect("fixture must parse")
    }

    fn json(text: &str) -> Json {
        parse_json(text).expect("expected value must be valid json")
    }

    fn rejected(text: &str) -> ParseError {
        parse_yaml(text).expect_err("fixture must be rejected")
    }

    #[test]
    fn nested_mappings_and_sequences_parse() {
        let doc = "\
# a comment
name: demo
version: 3
tags:
  - a
  - b
nested:
  inner:
    flag: true
  list:
  - 1
  - 2.5
";
        assert_eq!(
            yaml(doc),
            json(
                "{\"name\":\"demo\",\"version\":3,\"tags\":[\"a\",\"b\"],\
                 \"nested\":{\"inner\":{\"flag\":true},\"list\":[1,2.5]}}"
            )
        );
    }

    #[test]
    fn sequences_of_mappings_and_nested_sequences_parse() {
        let doc = "\
- name: one
  value: 1
- name: two
  extra:
    - x
- - a
  - b
- plain
";
        assert_eq!(
            yaml(doc),
            json(
                "[{\"name\":\"one\",\"value\":1},{\"name\":\"two\",\"extra\":[\"x\"]},\
                 [\"a\",\"b\"],\"plain\"]"
            )
        );
    }

    #[test]
    fn same_indent_sequence_under_a_key_does_not_swallow_siblings() {
        let doc = "items:\n- a\n- b\nafter: 1\n";
        assert_eq!(yaml(doc), json("{\"items\":[\"a\",\"b\"],\"after\":1}"));
    }

    #[test]
    fn key_order_follows_the_document() {
        let value = yaml("z: 1\na: 2\nm: 3\n");
        assert_eq!(value.to_compact_string(), "{\"z\":1,\"a\":2,\"m\":3}");
    }

    #[test]
    fn scalars_are_typed_conservatively() {
        let doc = "\
nothing:
tilde: ~
nul: null
yes_word: yes
off_word: off
t: true
f: False
int: -12
float: 1.5e2
zero: 0
leading_zero: 007
hex: 0x1F
plus: +5
inf: .inf
word: hello world
url: http://example.com/x
";
        let value = yaml(doc);
        let expect = |key: &str, expected: Json| {
            assert_eq!(value.get(key), Some(&expected), "key {key}");
        };
        expect("nothing", Json::Null);
        expect("tilde", Json::Null);
        expect("nul", Json::Null);
        expect("yes_word", Json::string("yes"));
        expect("off_word", Json::string("off"));
        expect("t", Json::Bool(true));
        expect("f", Json::Bool(false));
        expect("int", Json::Int(-12));
        expect("float", Json::Float(150.0));
        expect("zero", Json::Int(0));
        expect("leading_zero", Json::string("007"));
        expect("hex", Json::string("0x1F"));
        expect("plus", Json::string("+5"));
        expect("inf", Json::string(".inf"));
        expect("word", Json::string("hello world"));
        expect("url", Json::string("http://example.com/x"));
    }

    #[test]
    fn quoted_scalars_are_always_strings_and_unescape() {
        let doc = "\
a: \"1\"
b: 'it''s'
c: \"tab\\there\\u00e9\\x41\"
d: 'true'
\"quoted key\": v
e: \"x\" # trailing comment
";
        let value = yaml(doc);
        assert_eq!(value.get("a"), Some(&Json::string("1")));
        assert_eq!(value.get("b"), Some(&Json::string("it's")));
        assert_eq!(value.get("c"), Some(&Json::string("tab\there\u{e9}A")));
        assert_eq!(value.get("d"), Some(&Json::string("true")));
        assert_eq!(value.get("quoted key"), Some(&Json::string("v")));
        assert_eq!(value.get("e"), Some(&Json::string("x")));
    }

    #[test]
    fn comments_after_plain_values_are_stripped() {
        let value = yaml("a: value # note\nb: c#not-a-comment\n");
        assert_eq!(value.get("a"), Some(&Json::string("value")));
        assert_eq!(value.get("b"), Some(&Json::string("c#not-a-comment")));
    }

    #[test]
    fn block_scalars_literal_folded_and_strip() {
        let doc = "\
lit: |
  line one
    indented
  line three
fold: >
  folded
  text

  new paragraph
strip: |-
  no newline
folded_strip: >-
  a
  b
empty: |
after: 1
";
        let value = yaml(doc);
        assert_eq!(
            value.get("lit"),
            Some(&Json::string("line one\n  indented\nline three\n"))
        );
        assert_eq!(
            value.get("fold"),
            Some(&Json::string("folded text\nnew paragraph\n"))
        );
        assert_eq!(value.get("strip"), Some(&Json::string("no newline")));
        assert_eq!(value.get("folded_strip"), Some(&Json::string("a b")));
        assert_eq!(value.get("empty"), Some(&Json::string("")));
        assert_eq!(value.get("after"), Some(&Json::Int(1)));
    }

    #[test]
    fn block_scalar_in_sequence_items_and_comment_lines_are_content() {
        let doc = "- |\n  # not a comment\n  body\n- next\n";
        assert_eq!(yaml(doc), json("[\"# not a comment\\nbody\\n\",\"next\"]"));
    }

    #[test]
    fn flow_collections_parse() {
        let value =
            yaml("a: [1, \"two\", three]\nb: {k: v, n: 2}\nc: []\nd: {}\ne: [[1], {x: [y]},]\n");
        assert_eq!(value.get("a"), Some(&json("[1,\"two\",\"three\"]")));
        assert_eq!(value.get("b"), Some(&json("{\"k\":\"v\",\"n\":2}")));
        assert_eq!(value.get("c"), Some(&json("[]")));
        assert_eq!(value.get("d"), Some(&json("{}")));
        assert_eq!(value.get("e"), Some(&json("[[1],{\"x\":[\"y\"]}]")));
    }

    #[test]
    fn document_markers_bom_and_empty_documents() {
        assert_eq!(yaml("---\na: 1\n"), json("{\"a\":1}"));
        assert_eq!(yaml("a: 1\n...\n# trailing\n"), json("{\"a\":1}"));
        assert_eq!(yaml("\u{feff}a: 1\r\nb: 2\r\n"), json("{\"a\":1,\"b\":2}"));
        assert_eq!(yaml(""), Json::Null);
        assert_eq!(yaml("# only a comment\n\n"), Json::Null);
        assert_eq!(yaml("just a scalar\n"), Json::string("just a scalar"));
        assert_eq!(yaml("42\n"), Json::Int(42));
    }

    #[test]
    fn unsupported_constructs_are_rejected_with_line_numbers() {
        let cases: [(&str, usize, &str); 18] = [
            ("a:\n\tb: 1\n", 2, "tabs"),
            ("a: &anchor 1\n", 1, "anchors"),
            ("a: *alias\n", 1, "anchors"),
            ("a: !!str 1\n", 1, "anchors"),
            ("a: 1\na: 2\n", 2, "duplicate"),
            ("a: 1\n---\nb: 2\n", 2, "multiple documents"),
            ("--- text\n", 1, "document start"),
            ("a: 1\n...\nb: 2\n", 3, "end marker"),
            ("a: \"open\n", 1, "unterminated"),
            ("a: 'open\n", 1, "unterminated"),
            ("a: \"\\q\"\n", 1, "escape"),
            ("a: - b\n", 1, "block sequence"),
            ("a: b: c\n", 1, "mapping value"),
            ("a: |+\n  x\n", 1, "block scalar header"),
            ("a: |2\n  x\n", 1, "block scalar header"),
            ("? complex\n: x\n", 1, "complex"),
            ("a: [1, 2\n", 1, "flow"),
            ("a: [1,,2]\n", 1, "empty flow entry"),
        ];
        for (text, line, needle) in cases {
            let error = rejected(text);
            assert_eq!(error.format, "YAML", "input {text:?}");
            assert_eq!(error.line, line, "input {text:?}: {error}");
            assert!(error.message.contains(needle), "input {text:?}: {error}");
        }
    }

    #[test]
    fn structural_errors_are_rejected() {
        // Over-indented continuation of a plain scalar.
        assert_eq!(rejected("a: one\n  two\n").line, 2);
        // A sibling dedented to an inconsistent level.
        assert_eq!(rejected("a:\n    b: 1\n  c: 2\n").line, 3);
        // A key line without a colon inside a mapping.
        assert_eq!(rejected("a: 1\nplain line\n").line, 2);
        // A sequence entry mixed into a mapping at the same level.
        assert_eq!(rejected("a: 1\n- b\n").line, 2);
        // Inconsistent block scalar indentation.
        assert_eq!(rejected("a: |\n    deep\n  shallow\n").line, 3);
        // Text after a quoted scalar or a flow collection.
        assert!(rejected("a: \"x\" y\n")
            .message
            .contains("after quoted scalar"));
        assert!(rejected("a: [1] x\n")
            .message
            .contains("after a flow collection"));
    }

    #[test]
    fn nesting_depth_is_bounded() {
        let mut deep = String::new();
        for level in 0..=MAX_YAML_DEPTH + 2 {
            deep.push_str(&" ".repeat(level));
            deep.push_str("k:\n");
        }
        deep.push_str(&" ".repeat(MAX_YAML_DEPTH + 3));
        deep.push_str("leaf: 1\n");
        assert_eq!(rejected(&deep).message, "nesting is too deep");
        let flow = format!(
            "a: {}{}\n",
            "[".repeat(MAX_YAML_DEPTH + 2),
            "]".repeat(MAX_YAML_DEPTH + 2)
        );
        assert_eq!(rejected(&flow).message, "nesting is too deep");
    }

    /// `block` nested block mappings (`k:` lines) followed by `flow` nested
    /// flow arrays holding `0`; the document-wide level count is the sum.
    fn mixed_depth_fixture(block: usize, flow: usize) -> String {
        let mut text = String::new();
        for level in 0..block {
            text.push_str(&" ".repeat(level));
            text.push_str("k:\n");
        }
        text.push_str(&" ".repeat(block));
        text.push_str(&"[".repeat(flow));
        text.push('0');
        text.push_str(&"]".repeat(flow));
        text.push('\n');
        text
    }

    /// `levels` sequence-of-mapping pairs joined by same-indent sequences
    /// (two containers per level). `tail` appends one more same-indent
    /// sequence item under the last key when true, otherwise the last key
    /// holds a scalar.
    fn same_indent_fixture(levels: usize, tail: bool) -> String {
        let mut text = String::new();
        for level in 0..levels {
            text.push_str(&" ".repeat(2 * level));
            if level + 1 == levels && !tail {
                text.push_str("- k: 1\n");
            } else {
                text.push_str("- k:\n");
            }
        }
        if tail {
            text.push_str(&" ".repeat(2 * levels));
            text.push_str("- 1\n");
        }
        text
    }

    fn container_depth(value: &Json) -> usize {
        match value {
            Json::Array(items) => 1 + items.iter().map(container_depth).max().unwrap_or(0),
            Json::Object(pairs) => {
                1 + pairs
                    .iter()
                    .map(|(_, v)| container_depth(v))
                    .max()
                    .unwrap_or(0)
            }
            _ => 0,
        }
    }

    #[test]
    fn one_depth_budget_spans_block_and_flow_containers() {
        // The reviewer's fixture: 40 block levels + 40 flow levels = 80 > 64.
        let error = rejected(&mixed_depth_fixture(40, 40));
        assert_eq!(error.message, "nesting is too deep");
        // Exactly the limit is accepted; one more level is rejected.
        let at_limit = yaml(&mixed_depth_fixture(40, MAX_YAML_DEPTH - 40));
        assert_eq!(container_depth(&at_limit), MAX_YAML_DEPTH);
        let over = rejected(&mixed_depth_fixture(40, MAX_YAML_DEPTH - 40 + 1));
        assert_eq!(over.message, "nesting is too deep");
        assert_eq!(over.line, 41);
        // Pure block and pure flow documents use the same limit.
        let block_only = yaml(&mixed_depth_fixture(MAX_YAML_DEPTH, 0));
        assert_eq!(container_depth(&block_only), MAX_YAML_DEPTH);
        assert_eq!(
            rejected(&mixed_depth_fixture(MAX_YAML_DEPTH + 1, 0)).message,
            "nesting is too deep"
        );
        let flow_only = format!(
            "a: {}0{}\n",
            "[".repeat(MAX_YAML_DEPTH - 1),
            "]".repeat(MAX_YAML_DEPTH - 1)
        );
        assert_eq!(container_depth(&yaml(&flow_only)), MAX_YAML_DEPTH);
        let flow_over = format!(
            "a: {}0{}\n",
            "[".repeat(MAX_YAML_DEPTH),
            "]".repeat(MAX_YAML_DEPTH)
        );
        assert_eq!(rejected(&flow_over).message, "nesting is too deep");
    }

    #[test]
    fn flow_collections_in_sequence_items_and_roots_count_toward_the_budget() {
        let nested = |blocks: usize, flow: usize| {
            let mut text = String::new();
            for level in 0..blocks {
                text.push_str(&" ".repeat(2 * level));
                text.push_str("-\n");
            }
            text.push_str(&" ".repeat(2 * blocks));
            text.push_str("- ");
            text.push_str(&"[".repeat(flow));
            text.push('0');
            text.push_str(&"]".repeat(flow));
            text.push('\n');
            text
        };
        // `blocks` outer sequences + the sequence holding the flow item + flow.
        let ok = yaml(&nested(30, MAX_YAML_DEPTH - 31));
        assert_eq!(container_depth(&ok), MAX_YAML_DEPTH);
        assert_eq!(
            rejected(&nested(30, MAX_YAML_DEPTH - 30)).message,
            "nesting is too deep"
        );
        // A bare root flow collection starts at level one.
        let root = format!(
            "{}0{}\n",
            "[".repeat(MAX_YAML_DEPTH),
            "]".repeat(MAX_YAML_DEPTH)
        );
        assert_eq!(container_depth(&yaml(&root)), MAX_YAML_DEPTH);
        let root_over = format!(
            "{}0{}\n",
            "[".repeat(MAX_YAML_DEPTH + 1),
            "]".repeat(MAX_YAML_DEPTH + 1)
        );
        assert_eq!(rejected(&root_over).message, "nesting is too deep");
    }

    #[test]
    fn same_indent_sequences_count_toward_the_budget() {
        let at_limit = yaml(&same_indent_fixture(MAX_YAML_DEPTH / 2, false));
        assert_eq!(container_depth(&at_limit), MAX_YAML_DEPTH);
        let over = rejected(&same_indent_fixture(MAX_YAML_DEPTH / 2, true));
        assert_eq!(over.message, "nesting is too deep");
        assert_eq!(over.line, MAX_YAML_DEPTH / 2 + 1);
        assert_eq!(
            rejected(&same_indent_fixture(MAX_YAML_DEPTH / 2 + 1, false)).message,
            "nesting is too deep"
        );
    }

    #[test]
    fn unsupported_syntax_in_flow_keys_and_scalars_is_rejected() {
        let cases: [(&str, usize, &str); 12] = [
            ("ok: 1\nx: {&a name: value}\n", 2, "anchors"),
            ("ok: 1\nx: {*alias: value}\n", 2, "anchors"),
            ("ok: 1\nx: {!tag name: value}\n", 2, "anchors"),
            ("ok: 1\nx: {a: 1, &b k: 2}\n", 2, "anchors"),
            ("ok: 1\nx: [@bad]\n", 2, "reserved"),
            ("ok: 1\nx: [%bad]\n", 2, "reserved"),
            ("ok: 1\nx: [a, `bad]\n", 2, "reserved"),
            ("ok: 1\nx: {@k: v}\n", 2, "reserved"),
            ("ok: 1\nx: {k: &a v}\n", 2, "anchors"),
            ("ok: 1\nx: [|]\n", 2, "block scalars"),
            ("ok: 1\nx: {? a: b}\n", 2, "complex"),
            ("- [1, *a]\n", 1, "anchors"),
        ];
        for (text, line, needle) in cases {
            let error = rejected(text);
            assert_eq!(error.line, line, "input {text:?}: {error}");
            assert!(error.message.contains(needle), "input {text:?}: {error}");
        }
    }

    #[test]
    fn quoted_flow_keys_and_scalars_may_hold_indicator_text() {
        let value = yaml(
            "x: {\"&a name\": 1, '*alias': 2, \"!tag\": 3, \"@k\": 4}\ny: [\"@bad\", '%x', \"`t\", '|', \"&a\"]\n",
        );
        assert_eq!(
            value.get("x"),
            Some(&json("{\"&a name\":1,\"*alias\":2,\"!tag\":3,\"@k\":4}"))
        );
        assert_eq!(
            value.get("y"),
            Some(&json("[\"@bad\",\"%x\",\"`t\",\"|\",\"&a\"]"))
        );
        // Indicator characters inside (not at the start of) plain text are fine.
        let inner = yaml("z: [a&b, c*d, e!f, g@h]\nw: {k&1: v*2}\n");
        assert_eq!(
            inner.get("z"),
            Some(&json("[\"a&b\",\"c*d\",\"e!f\",\"g@h\"]"))
        );
        assert_eq!(inner.get("w"), Some(&json("{\"k&1\":\"v*2\"}")));
    }

    #[test]
    fn yaml_documents_validate_against_a_schema() {
        use crate::schema::{validate, ObjectSchema, Schema};
        let schema: Schema = ObjectSchema::new()
            .required("name", Schema::String)
            .required("replicas", Schema::Integer)
            .optional("ports", Schema::Array(Box::new(Schema::Integer)))
            .into();
        let good = yaml("name: web\nreplicas: 2\nports: [80, 443]\n");
        assert!(validate(&schema, &good).is_ok());
        let bad = yaml("name: web\nreplicas: two\nports: [80, http]\n");
        let violations = validate(&schema, &bad).expect_err("must fail");
        let paths: Vec<&str> = violations.iter().map(|v| v.path.as_str()).collect();
        assert_eq!(paths, vec!["$.replicas", "$.ports[1]"]);
    }
}
