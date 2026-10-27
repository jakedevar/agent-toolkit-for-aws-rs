//! JSONC to JSON: comment stripping with key attribution, and the four
//! regular-expression rewrites `validate-artifacts.py` applies to a contract
//! example block. Every rewrite is a hand-written matcher with the exact
//! semantics of the Python pattern (Unicode `\w` and `\s`, `re.M` anchors), so
//! no second regular-expression engine is needed.

use std::collections::HashMap;

use crate::manifests::{is_py_space, py_splitlines, py_strip};
use crate::startup::text::is_word;

/// The result of [`strip_jsonc`].
pub(crate) struct Stripped {
    pub(crate) json: String,
    /// `(0-based line, comment text)` in line order.
    pub(crate) comments: Vec<(usize, String)>,
    /// The key that opens on a line, by 0-based line.
    pub(crate) keys: HashMap<usize, String>,
}

/// Port of `strip_jsonc`.
pub(crate) fn strip_jsonc(text: &str) -> Stripped {
    let mut out_lines: Vec<String> = Vec::new();
    let mut comments = Vec::new();
    let mut keys = HashMap::new();
    for (index, raw) in py_splitlines(text).into_iter().enumerate() {
        let mut line: Vec<char> = raw.chars().collect();
        if let Some(at) = comment_start(&line) {
            let comment: String = line[at + 2..].iter().collect();
            comments.push((index, py_strip(&comment).to_string()));
            line.truncate(at);
            while line.last().is_some_and(|c| is_py_space(*c)) {
                line.pop();
            }
        }
        if let Some(key) = attributed_key(&line) {
            keys.insert(index, key);
        }
        out_lines.push(line.into_iter().collect());
    }
    let joined = out_lines.join("\n");
    let joined = replace_ellipsis(&joined);
    let joined = replace_bool_alternation(&joined);
    Stripped {
        json: remove_trailing_commas(&joined),
        comments,
        keys,
    }
}

/// Index of the first `//` that is outside a string.
fn comment_start(line: &[char]) -> Option<usize> {
    let mut in_string = false;
    let mut escaped = false;
    let mut at = 0;
    while at < line.len() {
        let c = line[at];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '/' && line.get(at + 1) == Some(&'/') {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// The key a trailing comment annotates: the last key opened at the nesting
/// depth the line ends on, or one level shallower when its value is still open.
fn attributed_key(line: &[char]) -> Option<String> {
    let mut depth: i64 = 0;
    let mut last_at_depth: HashMap<i64, String> = HashMap::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut key_end: Option<usize> = None;
    let mut string_open = 0;
    let mut pos = 0;
    while pos < line.len() {
        let c = line[pos];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                key_end = Some(pos);
            }
            pos += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                string_open = pos;
            }
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ':' if key_end.is_some() => {
                if let Some(end) = key_end {
                    let between: String = line[end + 1..pos].iter().collect();
                    if py_strip(&between).is_empty() {
                        last_at_depth.insert(depth, line[string_open + 1..end].iter().collect());
                    }
                }
                key_end = None;
            }
            c if !is_py_space(c) => key_end = None,
            _ => {}
        }
        pos += 1;
    }
    last_at_depth
        .get(&depth)
        .or_else(|| last_at_depth.get(&(depth - 1)))
        .cloned()
}

/// `(?<!["\w.])\.\.\.(?!["\w.])` replaced by `"..."`.
fn replace_ellipsis(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let blocked = |c: char| c == '"' || c == '.' || is_word(c);
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        let hit = chars.get(at..at + 3).is_some_and(|s| s == ['.', '.', '.'])
            && !(at > 0 && blocked(chars[at - 1]))
            && !chars.get(at + 3).is_some_and(|c| blocked(*c));
        if hit {
            out.push_str("\"...\"");
            at += 3;
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

/// `(?<!["\w])(?:true|false)\|(?:true|false)(?!["\w])` replaced by `true`.
fn replace_bool_alternation(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let blocked = |c: char| c == '"' || is_word(c);
    let word_at = |at: usize| -> Option<usize> {
        ["true", "false"].iter().find_map(|word| {
            let len = word.chars().count();
            let slice = chars.get(at..at + len)?;
            slice.iter().copied().eq(word.chars()).then_some(len)
        })
    };
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        let matched = (!(at > 0 && blocked(chars[at - 1])))
            .then(|| word_at(at))
            .flatten()
            .and_then(|first| (chars.get(at + first) == Some(&'|')).then_some(at + first + 1))
            .and_then(|second| word_at(second).map(|len| second + len))
            .filter(|end| !chars.get(*end).is_some_and(|c| blocked(*c)));
        if let Some(end) = matched {
            out.push_str("true");
            at = end;
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

/// `,(\s*[}\]])` replaced by the captured group.
pub(crate) fn remove_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == ',' {
            let mut look = at + 1;
            while chars.get(look).is_some_and(|c| is_py_space(*c)) {
                look += 1;
            }
            if matches!(chars.get(look), Some('}' | ']')) {
                out.extend(&chars[at + 1..=look]);
                at = look + 1;
                continue;
            }
        }
        out.push(chars[at]);
        at += 1;
    }
    out
}

/// `re.compile(r'^(\s*)"\.\.\."\s*,?\s*$', re.M).sub(r'\1"...": "..."', text)`.
pub(crate) fn replace_lone_ellipsis(text: &str) -> String {
    const TOKEN: [char; 5] = ['"', '.', '.', '.', '"'];
    let chars: Vec<char> = text.chars().collect();
    let at_line_end = |at: usize| at == chars.len() || chars[at] == '\n';
    let skip_space = |mut at: usize| {
        while chars.get(at).is_some_and(|c| is_py_space(*c)) {
            at += 1;
        }
        at
    };
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        if chars.get(at..at + 5).is_some_and(|s| s == TOKEN) {
            let line_start = chars[..at]
                .iter()
                .rposition(|c| *c == '\n')
                .map_or(0, |p| p + 1);
            let indented = chars[line_start..at].iter().all(|c| is_py_space(*c));
            if let Some(end) = indented
                .then(|| lone_end(&chars, at + 5, &skip_space, &at_line_end))
                .flatten()
            {
                out.push_str("\"...\": \"...\"");
                at = end;
                continue;
            }
        }
        out.push(chars[at]);
        at += 1;
    }
    out
}

/// End of `\s*,?\s*$` from `from`, trying the greedy choices first.
fn lone_end(
    chars: &[char],
    from: usize,
    skip_space: &dyn Fn(usize) -> usize,
    at_line_end: &dyn Fn(usize) -> bool,
) -> Option<usize> {
    for first in (from..=skip_space(from)).rev() {
        let after_comma = (chars.get(first) == Some(&',')).then_some(first + 1);
        for start in after_comma.into_iter().chain(std::iter::once(first)) {
            if let Some(end) = (start..=skip_space(start)).rev().find(|k| at_line_end(*k)) {
                return Some(end);
            }
        }
    }
    None
}
