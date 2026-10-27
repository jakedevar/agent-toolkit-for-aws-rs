//! The HCL reader of the Terraform policy validator (port of the lexer and
//! attribute readers of `validate-terraform-policy.py`).
//!
//! HCL is scanned once ([`lex`]) into per-character brace depth, "is real
//! code" and "is comment" arrays. Only braces that are real block delimiters
//! count: a `{` or `}` inside a comment, a quoted string (including a string
//! nested in an interpolation) or a heredoc body is not a delimiter, and an
//! attribute that only appears commented out is not set. Every reader works on
//! character slices because the Python script indexed by code point.
//!
//! The regular expressions of the script are exact hand-written matchers; each
//! function names the expression it replaces.

use crate::manifests::{is_py_space, py_strip};
use crate::startup::text::{fold, is_space, is_word};

/// Per-character lexical facts of one HCL text.
pub(crate) struct Lexed {
    pub(crate) depths: Vec<i64>,
    pub(crate) codes: Vec<bool>,
    pub(crate) comments: Vec<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    LineComment,
    BlockComment,
    Str,
    Heredoc,
}

/// `<<(-?)\s*([A-Za-z_][A-Za-z0-9_]*)` at `at`: the end of the opener, whether
/// it is the indented `<<-` form and the terminator tag.
fn heredoc_open(block: &[char], at: usize) -> Option<(usize, bool, String)> {
    let mut pos = at + 2;
    let dash = block.get(pos) == Some(&'-');
    if dash {
        pos += 1;
    }
    pos = skip_spaces(block, pos);
    let first = *block.get(pos)?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut end = pos;
    while block
        .get(end)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
    {
        end += 1;
    }
    Some((end, dash, block[pos..end].iter().collect()))
}

/// Scans HCL once. Interpolations are opaque string content, tracked so a
/// quote inside `${...}` opens a nested string instead of ending the outer
/// one.
#[allow(clippy::too_many_lines)]
pub(crate) fn lex(block: &[char]) -> Lexed {
    let n = block.len();
    let mut lexed = Lexed {
        depths: vec![0; n],
        codes: vec![false; n],
        comments: vec![false; n],
    };
    let mut depth = 0_i64;
    let mut state = State::Code;
    let mut tag = String::new();
    let mut tag_indent_ok = false;
    let mut interp = 0_usize;
    let mut inner_str = false;
    let mut i = 0;
    while i < n {
        let ch = block[i];
        let next = block.get(i + 1).copied();
        if state == State::Code {
            if ch == '#' || (ch == '/' && next == Some('/')) {
                lexed.depths[i] = depth;
                lexed.comments[i] = true;
                state = State::LineComment;
                i += 1;
                continue;
            }
            if ch == '/' && next == Some('*') {
                lexed.depths[i] = depth;
                lexed.depths[i + 1] = depth;
                lexed.comments[i] = true;
                lexed.comments[i + 1] = true;
                state = State::BlockComment;
                i += 2;
                continue;
            }
            if ch == '"' {
                lexed.depths[i] = depth;
                state = State::Str;
                interp = 0;
                inner_str = false;
                i += 1;
                continue;
            }
            if ch == '<' && next == Some('<') {
                if let Some((end, dash, name)) = heredoc_open(block, i) {
                    for slot in &mut lexed.depths[i..end] {
                        *slot = depth;
                    }
                    tag = name;
                    // Only `<<-TAG` permits an indented terminator.
                    tag_indent_ok = dash;
                    state = State::Heredoc;
                    i = end;
                    continue;
                }
            }
            lexed.depths[i] = depth;
            lexed.codes[i] = true;
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
            }
            i += 1;
            continue;
        }

        lexed.depths[i] = depth;
        match state {
            State::LineComment => {
                if ch == '\n' {
                    state = State::Code;
                } else {
                    lexed.comments[i] = true;
                }
                i += 1;
            }
            State::BlockComment => {
                lexed.comments[i] = true;
                if ch == '*' && next == Some('/') {
                    lexed.depths[i + 1] = depth;
                    lexed.comments[i + 1] = true;
                    state = State::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::Str => {
                if ch == '\\' && i + 1 < n {
                    lexed.depths[i + 1] = depth;
                    i += 2;
                } else if ch == '$' && next == Some('{') && !inner_str {
                    lexed.depths[i + 1] = depth;
                    interp += 1;
                    i += 2;
                } else if interp > 0 && ch == '"' {
                    inner_str = !inner_str;
                    i += 1;
                } else if interp > 0 && ch == '}' && !inner_str {
                    interp -= 1;
                    i += 1;
                } else {
                    if ch == '"' && interp == 0 {
                        state = State::Code;
                    }
                    i += 1;
                }
            }
            // The body is literal until a line holding only the tag.
            State::Heredoc | State::Code => {
                let line_end = block[i..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(n, |offset| i + offset + 1);
                for slot in &mut lexed.depths[i..line_end] {
                    *slot = depth;
                }
                let line: String = block[i..line_end].iter().collect();
                let terminator = if tag_indent_ok {
                    py_strip(&line)
                } else {
                    line.trim_end_matches(is_py_space)
                };
                if terminator == tag {
                    state = State::Code;
                }
                i = line_end;
            }
        }
    }
    lexed
}

fn skip_spaces(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).copied().is_some_and(is_space) {
        at += 1;
    }
    at
}

/// Exact `word` at `at`.
fn starts_with(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

/// Case-insensitive `word` at `at`.
fn starts_with_folded(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset).is_some_and(|d| fold(*d) == fold(c)))
}

fn to_string(chars: &[char]) -> String {
    chars.iter().collect()
}

/// The block that opens at `open_brace` (braces included) and the index after
/// its closing brace; the rest of the text when it never closes.
pub(crate) fn extract_braced_block(
    content: &[char],
    open_brace: usize,
    lexed: Option<&Lexed>,
) -> (Vec<char>, usize) {
    let own;
    let lexed = if let Some(lexed) = lexed {
        lexed
    } else {
        own = lex(content);
        &own
    };
    let tail = || content[open_brace.min(content.len())..].to_vec();
    if open_brace >= lexed.codes.len() || !lexed.codes[open_brace] {
        return (tail(), content.len());
    }
    let target = lexed.depths[open_brace] + 1;
    (open_brace + 1..content.len())
        .find(|&idx| lexed.codes[idx] && content[idx] == '}' && lexed.depths[idx] == target)
        .map_or_else(
            || (tail(), content.len()),
            |idx| (content[open_brace..=idx].to_vec(), idx + 1),
        )
}

/// One `resource "<type>" "<name>" {` header.
struct ResourceHeader {
    start: usize,
    brace: usize,
    kind: String,
    name: String,
}

/// `resource\s+"([a-zA-Z0-9_]+)"\s+"([^"]+)"\s*\{` at `at`.
fn resource_header(content: &[char], at: usize) -> Option<ResourceHeader> {
    if !starts_with(content, at, "resource") {
        return None;
    }
    let mut pos = at + "resource".len();
    let after_spaces = skip_spaces(content, pos);
    if after_spaces == pos || content.get(after_spaces) != Some(&'"') {
        return None;
    }
    pos = after_spaces + 1;
    let kind_start = pos;
    while content
        .get(pos)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
    {
        pos += 1;
    }
    if pos == kind_start || content.get(pos) != Some(&'"') {
        return None;
    }
    let kind = to_string(&content[kind_start..pos]);
    let after_kind = pos + 1;
    let name_open = skip_spaces(content, after_kind);
    if name_open == after_kind || content.get(name_open) != Some(&'"') {
        return None;
    }
    let name_start = name_open + 1;
    let name_end = name_start + content[name_start..].iter().position(|c| *c == '"')?;
    if name_end == name_start {
        return None;
    }
    let brace = skip_spaces(content, name_end + 1);
    if content.get(brace) != Some(&'{') {
        return None;
    }
    Some(ResourceHeader {
        start: at,
        brace,
        kind,
        name: to_string(&content[name_start..name_end]),
    })
}

/// A resource block: its name, body (braces included) and 1-based line.
pub(crate) struct Block {
    pub(crate) name: String,
    pub(crate) body: Vec<char>,
    pub(crate) line: usize,
}

/// Every block of `resource_type`; a commented-out declaration is skipped.
pub(crate) fn extract_blocks(content: &[char], resource_type: &str) -> Vec<Block> {
    let lexed = lex(content);
    let mut blocks = Vec::new();
    let mut pos = 0;
    while pos < content.len() {
        // `finditer`: headers do not overlap; scanning resumes after a match.
        let Some(header) = resource_header(content, pos) else {
            pos += 1;
            continue;
        };
        pos = header.brace + 1;
        if header.kind != resource_type || !lexed.codes[header.start] {
            continue;
        }
        let (body, _) = extract_braced_block(content, header.brace, Some(&lexed));
        let line = content[..header.start]
            .iter()
            .filter(|c| **c == '\n')
            .count()
            + 1;
        blocks.push(Block {
            name: header.name,
            body,
            line,
        });
    }
    blocks
}

/// Bodies of `block_type { ... }` blocks declared in `body`
/// (`<block_type>\s*\{`, outside comments).
pub(crate) fn own_blocks(body: &[char], block_type: &str) -> Vec<Vec<char>> {
    let lexed = lex(body);
    let mut found = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        if starts_with(body, pos, block_type) {
            let brace = skip_spaces(body, pos + block_type.chars().count());
            if body.get(brace) == Some(&'{') {
                let start = pos;
                pos = brace + 1;
                if lexed.codes[start] {
                    found.push(extract_braced_block(body, brace, Some(&lexed)).0);
                }
                continue;
            }
        }
        pos += 1;
    }
    found
}

/// What follows `<attr>\s*=` in an attribute pattern.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// Nothing: the assignment itself.
    Presence,
    /// `\s*"([^"]*)"`
    Quoted,
    /// `\s*(true|false)\b`, matched case-insensitively with the name.
    Bool,
    /// `\s*(?:(\d+)|"(\d+)")`
    Int,
}

/// The value an attribute pattern captured.
enum Captured {
    Presence,
    Text(String),
}

/// `^\s*<attr>\s*=<tail>` starting at the line start `at`: the capture and the
/// end of the match.
fn attr_at(block: &[char], at: usize, attr: &str, tail: Tail) -> Option<(Captured, usize)> {
    let name = skip_spaces(block, at);
    let ci = tail == Tail::Bool;
    let matches = if ci {
        starts_with_folded(block, name, attr)
    } else {
        starts_with(block, name, attr)
    };
    if !matches {
        return None;
    }
    let eq = skip_spaces(block, name + attr.chars().count());
    if block.get(eq) != Some(&'=') {
        return None;
    }
    let value = skip_spaces(block, eq + 1);
    let digits_end = |from: usize| {
        let mut end = from;
        while block.get(end).is_some_and(char::is_ascii_digit) {
            end += 1;
        }
        end
    };
    match tail {
        Tail::Presence => Some((Captured::Presence, eq + 1)),
        Tail::Quoted => {
            if block.get(value) != Some(&'"') {
                return None;
            }
            let close = value + 1 + block[value + 1..].iter().position(|c| *c == '"')?;
            Some((
                Captured::Text(to_string(&block[value + 1..close])),
                close + 1,
            ))
        }
        Tail::Bool => ["true", "false"].into_iter().find_map(|word| {
            let end = value + word.len();
            (starts_with_folded(block, value, word)
                && !block.get(end).copied().is_some_and(is_word))
            .then(|| (Captured::Text(word.to_string()), end))
        }),
        Tail::Int => {
            let end = digits_end(value);
            if end > value {
                return Some((Captured::Text(to_string(&block[value..end])), end));
            }
            if block.get(value) != Some(&'"') {
                return None;
            }
            let end = digits_end(value + 1);
            (end > value + 1 && block.get(end) == Some(&'"'))
                .then(|| (Captured::Text(to_string(&block[value + 1..end])), end + 1))
        }
    }
}

/// The first match of the attribute pattern that sits among the block's OWN
/// attributes: at the body's own brace depth (1 for a braced body, 0 for a
/// bare fragment) and not commented out. A nested block's `port` is therefore
/// never read as the resource's.
fn first_own_attr(block: &[char], attr: &str, tail: Tail) -> Option<Captured> {
    let base = i64::from(
        block
            .iter()
            .find(|c| !is_py_space(**c))
            .is_some_and(|c| *c == '{'),
    );
    let lexed = lex(block);
    let mut pos = 0;
    while pos < block.len() {
        // `^` in multiline mode: only at the start of the text or after a newline.
        if pos != 0 && block[pos - 1] != '\n' {
            pos += 1;
            continue;
        }
        let Some((captured, end)) = attr_at(block, pos, attr, tail) else {
            pos += 1;
            continue;
        };
        if lexed.codes[pos] && lexed.depths[pos] == base {
            return Some(captured);
        }
        pos = end.max(pos + 1);
    }
    None
}

/// True if `attr` is assigned among the block's own attributes.
pub(crate) fn has_own_attr(block: &[char], attr: &str) -> bool {
    first_own_attr(block, attr, Tail::Presence).is_some()
}

/// A quoted string attribute, else a literal `true`/`false` (lower-cased).
pub(crate) fn attr_string(block: &[char], attr: &str) -> Option<String> {
    for tail in [Tail::Quoted, Tail::Bool] {
        if let Some(Captured::Text(text)) = first_own_attr(block, attr, tail) {
            return Some(if tail == Tail::Bool {
                text.to_lowercase()
            } else {
                text
            });
        }
    }
    None
}

/// An integer attribute in its bare (`443`) or quoted (`"443"`) form; a quoted
/// value must be entirely digits.
pub(crate) fn attr_int(block: &[char], attr: &str) -> Option<i128> {
    match first_own_attr(block, attr, Tail::Int)? {
        Captured::Text(digits) => Some(digits.parse().unwrap_or(i128::MAX)),
        Captured::Presence => None,
    }
}

/// `type` of the first `default_action { ... }` block, read by brace-depth
/// matching so a nested `redirect {}` / `forward {}` before it does not hide
/// it.
pub(crate) fn default_action_type(block: &[char]) -> Option<String> {
    let brace = (0..block.len()).find_map(|start| {
        if !starts_with(block, start, "default_action") {
            return None;
        }
        let brace = skip_spaces(block, start + "default_action".len());
        (block.get(brace) == Some(&'{')).then_some(brace)
    })?;
    let (body, _) = extract_braced_block(block, brace, None);
    // `^\s*type\s*=\s*"([^"]+)"` (multiline), first match in the whole body.
    (0..body.len()).find_map(|start| {
        if start != 0 && body[start - 1] != '\n' {
            return None;
        }
        let name = skip_spaces(&body, start);
        if !starts_with(&body, name, "type") {
            return None;
        }
        let eq = skip_spaces(&body, name + "type".len());
        if body.get(eq) != Some(&'=') {
            return None;
        }
        let open = skip_spaces(&body, eq + 1);
        if body.get(open) != Some(&'"') {
            return None;
        }
        let close = open + 1 + body[open + 1..].iter().position(|c| *c == '"')?;
        (close > open + 1).then(|| to_string(&body[open + 1..close]))
    })
}

/// Comment characters replaced with spaces (surviving tokens never fuse and
/// line anchors keep working), using the lexer's own definition of a comment.
pub(crate) fn strip_comments(text: &[char]) -> Vec<char> {
    let lexed = lex(text);
    text.iter()
        .zip(&lexed.comments)
        .map(|(c, comment)| if *comment { ' ' } else { *c })
        .collect()
}

/// The raw text inside a literal `attr = [ ... ]`
/// (`(?:^|\{)\s*<attr>\s*=\s*\[(.*?)\]`). The name must start a line or directly
/// follow a `{`, which keeps `cidr_blocks` from matching the tail of
/// `ipv6_cidr_blocks`.
pub(crate) fn attr_list_inner(block: &[char], attr: &str) -> Option<Vec<char>> {
    (0..block.len()).find_map(|start| {
        let line_start = start == 0 || block[start - 1] == '\n';
        let candidates = [
            line_start.then_some(start),
            (block[start] == '{').then_some(start + 1),
        ];
        candidates.into_iter().flatten().find_map(|from| {
            let name = skip_spaces(block, from);
            if !starts_with(block, name, attr) {
                return None;
            }
            let eq = skip_spaces(block, name + attr.chars().count());
            if block.get(eq) != Some(&'=') {
                return None;
            }
            let open = skip_spaces(block, eq + 1);
            if block.get(open) != Some(&'[') {
                return None;
            }
            let close = open + 1 + block[open + 1..].iter().position(|c| *c == ']')?;
            Some(block[open + 1..close].to_vec())
        })
    })
}
