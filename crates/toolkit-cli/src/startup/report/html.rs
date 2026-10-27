//! A port of `CPython`'s `html.parser.HTMLParser` (with `convert_charrefs=True`)
//! and `html.unescape`, the tokenizer behind the report validator.
//!
//! The report validator never asks for the character-reference callbacks: with
//! `convert_charrefs=True` the text between tags is decoded as a whole before
//! it reaches the handler. The tokenizer follows the module of Python 3.14
//! (`goahead`, the tolerant start/end tag grammar, comment, bogus comment,
//! declaration and CDATA/RCDATA handling); the regular expressions of the
//! original are written out as scanners over the characters of the document.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::entities::HTML5;

/// One attribute: lower-cased name and its value (`None` when it has none).
pub(super) type Attr = (String, Option<String>);

/// The overridable callbacks of `HTMLParser`.
pub(super) trait Handler {
    /// `handle_starttag`; `text` is `get_starttag_text()`.
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], text: &str);
    /// `handle_startendtag`; `text` is `get_starttag_text()`.
    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], text: &str);
    /// `handle_endtag`.
    fn end_tag(&mut self, tag: &str);
    /// `handle_data`.
    fn data(&mut self, data: &str);
    /// `handle_comment`.
    fn comment(&mut self, _data: &str) {}
}

/// `dict(attrs).get(name)`: the last attribute of that name.
pub(super) fn attr_lookup<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a Option<String>> {
    attrs
        .iter()
        .rev()
        .find(|(attr_name, _)| attr_name == name)
        .map(|(_, value)| value)
}

/// `"hidden" in dict(attrs)`.
pub(super) fn has_attr(attrs: &[Attr], name: &str) -> bool {
    attrs.iter().any(|(attr_name, _)| attr_name == name)
}

const CDATA_CONTENT_ELEMENTS: [&str; 6] =
    ["script", "style", "xmp", "iframe", "noembed", "noframes"];
const RCDATA_CONTENT_ELEMENTS: [&str; 2] = ["textarea", "title"];

/// `[\t\n\r\f ]`.
fn is_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\x0c' | ' ')
}

/// A character that ends a tag name (`[\t\n\r\f />]`).
fn ends_tag_name(c: char) -> bool {
    is_space(c) || c == '/' || c == '>'
}

/// A character that ends an attribute name (`[\t\n\r\f /=>]`).
fn ends_attribute_name(c: char) -> bool {
    is_space(c) || matches!(c, '/' | '=' | '>')
}

/// Feeds `html` to a fresh parser, then closes it (`feed(html)`; `close()`).
pub(super) fn parse<H: Handler>(html: &str, handler: &mut H) {
    let mut parser = Parser {
        raw: html.chars().collect(),
        cdata: None,
        escapable: true,
        handler,
    };
    parser.goahead(false);
    parser.goahead(true);
}

struct Parser<'h, H: Handler> {
    raw: Vec<char>,
    /// `cdata_elem`: the element whose content is raw text.
    cdata: Option<String>,
    escapable: bool,
    handler: &'h mut H,
}

/// The pieces of a matched attribute (`attrfind_tolerant`).
struct AttrMatch {
    name: String,
    /// The `=value` group was present.
    has_value: bool,
    value: String,
    end: usize,
}

impl<H: Handler> Parser<'_, H> {
    fn len(&self) -> usize {
        self.raw.len()
    }

    /// `rawdata[start:stop]` with Python's clamping.
    fn slice(&self, start: usize, stop: usize) -> String {
        let stop = stop.min(self.len());
        let start = start.min(stop);
        self.raw[start..stop].iter().collect()
    }

    fn at(&self, index: usize) -> Option<char> {
        self.raw.get(index).copied()
    }

    fn starts_with(&self, prefix: &str, index: usize) -> bool {
        let mut position = index;
        for expected in prefix.chars() {
            if self.at(position) != Some(expected) {
                return false;
            }
            position += 1;
        }
        true
    }

    fn find_char(&self, wanted: char, from: usize) -> Option<usize> {
        (from..self.len()).find(|&index| self.raw[index] == wanted)
    }

    fn find_str(&self, needle: &str, from: usize) -> Option<usize> {
        (from..self.len()).find(|&index| self.starts_with(needle, index))
    }

    /// `starttagopen`: `<[a-zA-Z]`.
    fn start_tag_open(&self, index: usize) -> bool {
        self.at(index) == Some('<') && self.at(index + 1).is_some_and(|c| c.is_ascii_alphabetic())
    }

    /// `endtagopen`: `</[a-zA-Z]`.
    fn end_tag_open(&self, index: usize) -> bool {
        self.at(index) == Some('<')
            && self.at(index + 1) == Some('/')
            && self.at(index + 2).is_some_and(|c| c.is_ascii_alphabetic())
    }

    /// Hands text to `handle_data`, decoded unless the content is raw text.
    fn emit_data(&mut self, text: &str) {
        if self.escapable {
            let decoded = unescape(text);
            self.handler.data(&decoded);
        } else {
            self.handler.data(text);
        }
    }

    /// `self.interesting.search(rawdata, i)` while a CDATA/RCDATA element is
    /// open: the position of its end tag (or the end of the text for
    /// `plaintext`).
    fn interesting_search(&self, from: usize, element: &str) -> Option<usize> {
        if element == "plaintext" {
            return Some(self.len());
        }
        let name: Vec<char> = element.chars().collect();
        (from..self.len()).find(|&index| {
            if self.at(index) != Some('<') || self.at(index + 1) != Some('/') {
                return false;
            }
            for (offset, expected) in name.iter().enumerate() {
                match self.at(index + 2 + offset) {
                    Some(c) if c.to_ascii_lowercase() == *expected => {}
                    _ => return false,
                }
            }
            self.at(index + 2 + name.len())
                .is_some_and(|c| is_space(c) || c == '/' || c == '>')
        })
    }

    #[allow(clippy::too_many_lines)]
    fn goahead(&mut self, end: bool) {
        let n = self.len();
        let mut i = 0usize;
        while i < n {
            let j = match self.cdata.clone() {
                None => {
                    if let Some(position) = self.find_char('<', i) {
                        position
                    } else {
                        // A character reference may be cut in half at the end
                        // of the text: wait for the rest of it.
                        let from = i.max(n.saturating_sub(34));
                        let ampersand = (from..n).rev().find(|&index| self.raw[index] == '&');
                        if let Some(position) = ampersand {
                            let closed = self.raw[position..]
                                .iter()
                                .any(|&c| is_space(c) || c == ';');
                            if !closed {
                                break;
                            }
                        }
                        n
                    }
                }
                Some(element) => match self.interesting_search(i, &element) {
                    Some(position) => position,
                    None => break,
                },
            };
            if i < j {
                let text = self.slice(i, j);
                self.emit_data(&text);
            }
            i = j;
            if i == n {
                break;
            }
            // Here `rawdata[i]` is `<`.
            let parsed: Option<usize> = if self.start_tag_open(i) {
                self.parse_starttag(i)
            } else if self.starts_with("</", i) {
                self.parse_endtag(i)
            } else if self.starts_with("<!--", i) {
                self.parse_comment(i)
            } else if self.starts_with("<?", i) {
                self.parse_pi(i)
            } else if self.starts_with("<!", i) {
                self.parse_html_declaration(i)
            } else if i + 1 < n || end {
                self.handler.data("<");
                Some(i + 1)
            } else {
                break;
            };
            let Some(next) = parsed else {
                if !end {
                    break;
                }
                self.flush_incomplete(i);
                i = n;
                continue;
            };
            i = next;
        }
        if end && i < n {
            let text = self.slice(i, n);
            self.emit_data(&text);
            i = n;
        }
        self.raw = self.raw.split_off(i.min(n));
    }

    /// The tail of `goahead` for a construct that never terminated.
    fn flush_incomplete(&mut self, i: usize) {
        let n = self.len();
        // An unterminated start tag, CDATA section, declaration or processing
        // instruction is dropped.
        if self.start_tag_open(i)
            || self.starts_with("<![CDATA[", i)
            || self.is_doctype(i)
            || self.starts_with("<?", i)
        {
            return;
        }
        if self.starts_with("</", i) {
            if i + 2 == n {
                self.handler.data("</");
            } else if !self.end_tag_open(i) {
                let text = self.slice(i + 2, n);
                self.handler.comment(&text);
            }
        } else if self.starts_with("<!--", i) {
            let mut stop = n;
            for suffix in ["--!", "--", "-"] {
                let length = suffix.len();
                if n >= i + 4 + length && self.starts_with(suffix, n - length) {
                    stop -= length;
                    break;
                }
            }
            let text = self.slice(i + 4, stop);
            self.handler.comment(&text);
        } else if self.starts_with("<!", i) {
            let text = self.slice(i + 2, n);
            self.handler.comment(&text);
        }
    }

    fn is_doctype(&self, index: usize) -> bool {
        self.slice(index, index + 9).to_lowercase() == "<!doctype"
    }

    fn parse_html_declaration(&mut self, i: usize) -> Option<usize> {
        if self.starts_with("<!--", i) {
            self.parse_comment(i)
        } else if self.starts_with("<![CDATA[", i) {
            let close = self.find_str("]]>", i + 9)?;
            Some(close + 3)
        } else if self.is_doctype(i) {
            let close = self.find_char('>', i + 9)?;
            Some(close + 1)
        } else {
            self.parse_bogus_comment(i)
        }
    }

    fn parse_comment(&mut self, i: usize) -> Option<usize> {
        // An empty comment is abruptly closed by the first ">" or "->".
        let abrupt = match (self.at(i + 4), self.at(i + 5)) {
            (Some('>'), _) => Some(i + 5),
            (Some('-'), Some('>')) => Some(i + 6),
            _ => None,
        };
        if let Some(stop) = abrupt {
            self.handler.comment("");
            return Some(stop);
        }
        // `--!?>`: the first close at or after the comment text.
        let mut index = i + 4;
        while index < self.len() {
            if self.starts_with("--!>", index) {
                let text = self.slice(i + 4, index);
                self.handler.comment(&text);
                return Some(index + 4);
            }
            if self.starts_with("-->", index) {
                let text = self.slice(i + 4, index);
                self.handler.comment(&text);
                return Some(index + 3);
            }
            index += 1;
        }
        None
    }

    fn parse_bogus_comment(&mut self, i: usize) -> Option<usize> {
        let close = self.find_char('>', i + 2)?;
        let text = self.slice(i + 2, close);
        self.handler.comment(&text);
        Some(close + 1)
    }

    fn parse_pi(&mut self, i: usize) -> Option<usize> {
        let close = self.find_char('>', i + 2)?;
        Some(close + 1)
    }

    /// `[\t\n\r\f ]|/(?!>)` repeated from `index`.
    fn skip_tag_gap(&self, mut index: usize) -> usize {
        loop {
            match self.at(index) {
                Some(c) if is_space(c) => index += 1,
                Some('/') if self.at(index + 1) != Some('>') => index += 1,
                _ => return index,
            }
        }
    }

    /// `tagfind_tolerant` at `index`: the tag name and the end of the match.
    fn tag_name_at(&self, index: usize) -> (String, usize) {
        let mut end = index + 1;
        while self.at(end).is_some_and(|c| !ends_tag_name(c)) {
            end += 1;
        }
        let name = self.slice(index, end);
        (name, self.skip_tag_gap(end))
    }

    /// `'[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\f ]*` at `index`: the end of the
    /// value, or `None` when a quote is never closed.
    fn value_at(&self, index: usize) -> Option<usize> {
        match self.at(index) {
            Some(quote @ ('\'' | '"')) => {
                let close = self.find_char(quote, index + 1)?;
                Some(close + 1)
            }
            _ => {
                let mut end = index;
                while self.at(end).is_some_and(|c| c != '>' && !is_space(c)) {
                    end += 1;
                }
                Some(end)
            }
        }
    }

    /// The optional `[\t\n\r\f ]*=[\t\n\r\f ]*(value)` group at `index`: the
    /// span of the value, or `None` when the group does not match.
    fn value_group(&self, index: usize) -> Option<(usize, usize)> {
        let mut position = index;
        while self.at(position).is_some_and(is_space) {
            position += 1;
        }
        if self.at(position) != Some('=') {
            return None;
        }
        position += 1;
        let after_equals = position;
        while self.at(position).is_some_and(is_space) {
            position += 1;
        }
        if let Some(end) = self.value_at(position) {
            return Some((position, end));
        }
        // An unclosed quote: the whitespace gives one character back, which
        // lets the bare-value branch match an empty value.
        if position > after_equals {
            return Some((position - 1, position - 1));
        }
        None
    }

    /// Whether an attribute name may start at `index` (the look-behind and
    /// the first character class).
    fn attribute_starts(&self, index: usize) -> bool {
        let Some(previous) = index.checked_sub(1).and_then(|p| self.at(p)) else {
            return false;
        };
        let lookbehind = matches!(previous, '\'' | '"' | '/') || is_space(previous);
        lookbehind && self.at(index).is_some_and(|c| !ends_tag_name(c))
    }

    /// `locatetagend.match(rawdata, start)`: the end of the tag.
    fn locate_tag_end(&self, start: usize) -> usize {
        let mut position = start + 1;
        while self.at(position).is_some_and(|c| !ends_tag_name(c)) {
            position += 1;
        }
        while self.at(position).is_some_and(|c| is_space(c) || c == '/') {
            position += 1;
        }
        while self.attribute_starts(position) {
            position += 1;
            while self.at(position).is_some_and(|c| !ends_attribute_name(c)) {
                position += 1;
            }
            if let Some((_, end)) = self.value_group(position) {
                position = end;
            }
            while self.at(position).is_some_and(|c| is_space(c) || c == '/') {
                position += 1;
            }
        }
        if self.at(position) == Some('>') {
            position += 1;
        }
        position
    }

    /// `attrfind_tolerant.match(rawdata, index)`.
    fn attribute_at(&self, index: usize) -> Option<AttrMatch> {
        if !self.attribute_starts(index) {
            return None;
        }
        let mut name_end = index + 1;
        while self.at(name_end).is_some_and(|c| !ends_attribute_name(c)) {
            name_end += 1;
        }
        let name = self.slice(index, name_end);
        let (has_value, value, value_end) = match self.value_group(name_end) {
            Some((start, stop)) => (true, self.slice(start, stop), stop),
            None => (false, String::new(), name_end),
        };
        Some(AttrMatch {
            name,
            has_value,
            value,
            end: self.skip_tag_gap(value_end),
        })
    }

    fn parse_starttag(&mut self, i: usize) -> Option<usize> {
        // `check_for_whole_start_tag`
        let located = self.locate_tag_end(i + 1);
        if self.at(located - 1) != Some('>') {
            return None;
        }
        let endpos = located;
        let text = self.slice(i, endpos);

        let mut attrs: Vec<Attr> = Vec::new();
        let (name, mut k) = self.tag_name_at(i + 1);
        let tag = name.to_lowercase();
        while k < endpos {
            let Some(found) = self.attribute_at(k) else {
                break;
            };
            let value = if found.has_value {
                let mut value = found.value;
                let quoted = value.chars().next().zip(value.chars().last());
                if let Some((first, last)) = quoted {
                    if first == last
                        && (first == '\'' || first == '"')
                        && value.chars().count() >= 2
                    {
                        value = value
                            .chars()
                            .skip(1)
                            .take(value.chars().count() - 2)
                            .collect();
                    }
                }
                if !value.is_empty() {
                    value = unescape_attribute_value(&value);
                }
                Some(value)
            } else {
                None
            };
            attrs.push((found.name.to_lowercase(), value));
            k = found.end;
        }

        let tail = self.slice(k, endpos);
        let tail = tail.trim_matches(crate::manifests::is_py_space);
        if tail != ">" && tail != "/>" {
            let raw = self.slice(i, endpos);
            self.handler.data(&raw);
            return Some(endpos);
        }
        if tail.ends_with("/>") {
            self.handler.start_end_tag(&tag, &attrs, &text);
        } else {
            self.handler.start_tag(&tag, &attrs, &text);
            if CDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) || tag == "plaintext" {
                self.cdata = Some(tag);
                self.escapable = false;
            } else if RCDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) {
                self.cdata = Some(tag);
                self.escapable = true;
            }
        }
        Some(endpos)
    }

    fn parse_endtag(&mut self, i: usize) -> Option<usize> {
        self.find_char('>', i + 2)?;
        if !self.end_tag_open(i) {
            if self.at(i + 2) == Some('>') {
                // `</>` is ignored.
                return Some(i + 3);
            }
            return self.parse_bogus_comment(i);
        }
        let located = self.locate_tag_end(i + 2);
        if self.at(located - 1) != Some('>') {
            return None;
        }
        let (name, _) = self.tag_name_at(i + 2);
        self.handler.end_tag(&name.to_lowercase());
        self.cdata = None;
        self.escapable = true;
        Some(located)
    }
}

/// `html.unescape`.
pub(super) fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '&' {
            if let Some(end) = charref_end(&chars, index) {
                let group: String = chars[index + 1..end].iter().collect();
                out.push_str(&replace_charref(&group));
                index = end;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// The end of `&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\f <&#;]{1,32};?)`
/// starting at the `&` at `start`.
fn charref_end(chars: &[char], start: usize) -> Option<usize> {
    let at = |index: usize| chars.get(index).copied();
    let mut position = start + 1;
    if at(position) == Some('#') {
        let digits_from = position + 1;
        let mut end = digits_from;
        while at(end).is_some_and(|c| c.is_ascii_digit()) {
            end += 1;
        }
        if end > digits_from {
            return Some(end + usize::from(at(end) == Some(';')));
        }
        if matches!(at(digits_from), Some('x' | 'X')) {
            let hex_from = digits_from + 1;
            let mut end = hex_from;
            while at(end).is_some_and(|c| c.is_ascii_hexdigit()) {
                end += 1;
            }
            if end > hex_from {
                return Some(end + usize::from(at(end) == Some(';')));
            }
        }
        return None;
    }
    let name_char = |c: char| !matches!(c, '\t' | '\n' | '\x0c' | ' ' | '<' | '&' | '#' | ';');
    let name_from = position;
    while position < chars.len() && position - name_from < 32 && name_char(chars[position]) {
        position += 1;
    }
    if position == name_from {
        return None;
    }
    Some(position + usize::from(at(position) == Some(';')))
}

/// Numeric value of the digits of a character reference, saturating above the
/// last code point.
fn saturating_number(digits: &str, radix: u32) -> u32 {
    let mut value: u32 = 0;
    for c in digits.chars() {
        let digit = c.to_digit(radix).unwrap_or(0);
        value = value
            .saturating_mul(radix)
            .saturating_add(digit)
            .min(0x11_0000);
    }
    value
}

/// The replacement for one character reference (`_replace_charref`).
fn replace_charref(group: &str) -> String {
    if let Some(rest) = group.strip_prefix('#') {
        let number = match rest.strip_prefix(['x', 'X']) {
            Some(hex) => saturating_number(hex.trim_end_matches(';'), 16),
            None => saturating_number(rest.trim_end_matches(';'), 10),
        };
        return numeric_charref(number);
    }
    let table = entity_table();
    if let Some(value) = table.get(group) {
        return (*value).to_string();
    }
    // The longest matching name, as the standard defines it.
    let chars: Vec<char> = group.chars().collect();
    for x in (2..chars.len()).rev() {
        let prefix: String = chars[..x].iter().collect();
        if let Some(value) = table.get(prefix.as_str()) {
            let rest: String = chars[x..].iter().collect();
            return format!("{value}{rest}");
        }
    }
    format!("&{group}")
}

fn numeric_charref(number: u32) -> String {
    const WINDOWS_1252: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}',
        '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}',
        '\u{178}',
    ];
    match number {
        0 => return "\u{fffd}".to_string(),
        0x0d => return "\r".to_string(),
        0x80..=0x9f => {
            let index = usize::try_from(number - 0x80).unwrap_or(0);
            return WINDOWS_1252[index].to_string();
        }
        _ => {}
    }
    if (0xd800..=0xdfff).contains(&number) || number > 0x10_ffff {
        return "\u{fffd}".to_string();
    }
    let invalid = matches!(number, 0x1..=0x8 | 0xb | 0xe..=0x1f | 0x7f | 0xfdd0..=0xfdef)
        || (number & 0xfffe) == 0xfffe;
    if invalid {
        return String::new();
    }
    char::from_u32(number).map(String::from).unwrap_or_default()
}

fn entity_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| HTML5.iter().copied().collect())
}

/// `_unescape_attrvalue`: references in an attribute value, where a named one
/// is decoded only when it is an exact entity name not followed by `=`.
fn unescape_attribute_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let table = entity_table();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '&' {
            if let Some(end) = attribute_charref_end(&chars, index) {
                let reference: String = chars[index..end].iter().collect();
                let decode = if reference.starts_with("&#") {
                    true
                } else {
                    !reference.ends_with('=') && table.contains_key(&reference[1..])
                };
                if decode {
                    out.push_str(&unescape(&reference));
                } else {
                    out.push_str(&reference);
                }
                index = end;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// The end of `&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*)[;=]?`.
fn attribute_charref_end(chars: &[char], start: usize) -> Option<usize> {
    let at = |index: usize| chars.get(index).copied();
    let body_from = start + 1;
    let mut end;
    if at(body_from) == Some('#') {
        end = body_from + 1;
        while at(end).is_some_and(|c| c.is_ascii_digit()) {
            end += 1;
        }
        if end == body_from + 1 {
            if !matches!(at(body_from + 1), Some('x' | 'X')) {
                return None;
            }
            end = body_from + 2;
            while at(end).is_some_and(|c| c.is_ascii_hexdigit()) {
                end += 1;
            }
            if end == body_from + 2 {
                return None;
            }
        }
    } else if at(body_from).is_some_and(|c| c.is_ascii_alphabetic()) {
        end = body_from + 1;
        while at(end).is_some_and(|c| c.is_ascii_alphanumeric()) {
            end += 1;
        }
    } else {
        return None;
    }
    if matches!(at(end), Some(';' | '=')) {
        end += 1;
    }
    Some(end)
}
