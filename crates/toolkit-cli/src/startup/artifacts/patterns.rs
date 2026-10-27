//! Hand-written ports of the small regular expressions and `fnmatch` calls in
//! `validate-artifacts.py`, /// Port of `_JOINED_SET_RE.search(comment)`, ignoring case:
///
/// ```text
/// ["'`]?\+["'`]?[\s-]*joined|joined\s+(?:by|with|on)\s+["'`]?\+
/// ``` Python's Unicode `str` semantics.
use std::collections::BTreeSet;

use crate::manifests::{is_py_space, py_strip};
use crate::startup::text::{fold, is_word};

/// `re.match(r"^<.*>$", text)`: `.` excludes a newline and `$` also matches
/// before one trailing newline.
pub(crate) fn is_placeholder(text: &str) -> bool {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let chars: Vec<char> = body.chars().collect();
    chars.len() >= 2
        && chars[0] == '<'
        && chars[chars.len() - 1] == '>'
        && !chars[1..chars.len() - 1].contains(&'\n')
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-' | '/')
}

/// `_ENUM_TOKEN_RE.match(text)`: `^[A-Za-z0-9_.:+\-/]+$` (a trailing newline
/// is tolerated by `$`).
pub(crate) fn is_enum_token(text: &str) -> bool {
    let body = text.strip_suffix('\n').unwrap_or(text);
    !body.is_empty() && body.chars().all(is_token_char)
}

/// `_enum_from_string`: `a|b|c` where every member is a token.
pub(crate) fn enum_from_string(text: &str) -> Option<BTreeSet<String>> {
    if !text.contains('|') {
        return None;
    }
    let tokens: Vec<&str> = text.split('|').map(py_strip).collect();
    (tokens.len() >= 2 && tokens.iter().all(|t| is_enum_token(t)))
        .then(|| tokens.into_iter().map(str::to_string).collect())
}

/// `_enum_token`: one member of a comment enum, quotes removed.
fn enum_token(raw: &str) -> Option<String> {
    let stripped = py_strip(raw);
    let chars: Vec<char> = stripped.chars().collect();
    let inner: String =
        if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && "\"'`".contains(chars[0]) {
            chars[1..chars.len() - 1].iter().collect()
        } else {
            stripped.to_string()
        };
    is_enum_token(&inner).then_some(inner)
}

/// `_enum_from_comment`.
pub(crate) fn enum_from_comment(comment: &str) -> Option<BTreeSet<String>> {
    let chars: Vec<char> = comment.chars().collect();
    let group = search_comment_enum(&chars)?;
    let tokens: Vec<Option<String>> = group.split('|').map(enum_token).collect();
    (tokens.len() >= 2 && tokens.iter().all(Option::is_some))
        .then(|| tokens.into_iter().flatten().collect())
}

/// `_COMMENT_ENUM_RE.search(comment).group(1)`:
/// `(?:^|[—:\-]\s*)(ALT(?:\s*\|\s*ALT)+)\s*(?:$|[—(\-])`.
fn search_comment_enum(chars: &[char]) -> Option<String> {
    for start in 0..=chars.len() {
        let mut firsts = Vec::new();
        if start == 0 {
            firsts.push(0);
        }
        if matches!(chars.get(start), Some('\u{2014}' | ':' | '-')) {
            firsts.push(skip_space(chars, start + 1));
        }
        for first in firsts {
            for end in alt_ends(chars, first) {
                if let Some(group_end) = extend_enum(chars, end, 0) {
                    return Some(chars[first..group_end].iter().collect());
                }
            }
        }
    }
    None
}

fn skip_space(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).is_some_and(|c| is_py_space(*c)) {
        at += 1;
    }
    at
}

/// Possible ends of one `ALT`, most preferred first.
fn alt_ends(chars: &[char], at: usize) -> Vec<usize> {
    let Some(&c) = chars.get(at) else {
        return Vec::new();
    };
    if matches!(c, '`' | '"' | '\'') {
        let run = chars[at + 1..].iter().take_while(|x| **x != c).count();
        if run >= 1 && chars.get(at + 1 + run) == Some(&c) {
            return vec![at + run + 2];
        }
        // A quote is not a token character, so no bare-token fallback.
        return Vec::new();
    }
    let run = chars[at..]
        .iter()
        .take_while(|x| is_token_char(**x))
        .count();
    (1..=run).rev().map(|taken| at + taken).collect()
}

/// After an `ALT` ending at `end` with `count` repeats already matched: more
/// repeats first, then the tail `\s*(?:$|[—(\-])`. Returns the end of group 1.
fn extend_enum(chars: &[char], end: usize, count: usize) -> Option<usize> {
    let pipe = skip_space(chars, end);
    if chars.get(pipe) == Some(&'|') {
        let next = skip_space(chars, pipe + 1);
        for alt_end in alt_ends(chars, next) {
            if let Some(found) = extend_enum(chars, alt_end, count + 1) {
                return Some(found);
            }
        }
    }
    if count == 0 {
        return None;
    }
    (end..=skip_space(chars, end))
        .rev()
        .any(|t| t == chars.len() || matches!(chars.get(t), Some('\u{2014}' | '(' | '-')))
        .then_some(end)
}

fn starts_with_ci(chars: &[char], at: usize, word: &str) -> Option<usize> {
    let len = word.chars().count();
    let slice = chars.get(at..at + len)?;
    slice
        .iter()
        .zip(word.chars())
        .all(|(a, b)| fold(*a) == fold(b))
        .then_some(at + len)
}

/// `_JOINED_SET_RE.search(comment)`:
/// `["'`]?\+["'`]?[\s-]*joined|joined\s+(?:by|with|on)\s+["'`]?\+`, ignore case.
pub(crate) fn mentions_joined_set(comment: &str) -> bool {
    let chars: Vec<char> = comment.chars().collect();
    let quote = |c: Option<&char>| matches!(c, Some('"' | '\'' | '`'));
    (0..chars.len()).any(|at| {
        // First alternative, anchored at the `+`.
        if chars[at] == '+' {
            let mut look = at + 1;
            if quote(chars.get(look)) {
                look += 1;
            }
            while chars
                .get(look)
                .is_some_and(|c| is_py_space(*c) || *c == '-')
            {
                look += 1;
            }
            if starts_with_ci(&chars, look, "joined").is_some() {
                return true;
            }
        }
        // Second alternative.
        let Some(after) = starts_with_ci(&chars, at, "joined") else {
            return false;
        };
        let spaced = skip_space(&chars, after);
        if spaced == after {
            return false;
        }
        ["by", "with", "on"].iter().any(|word| {
            let Some(end) = starts_with_ci(&chars, spaced, word) else {
                return false;
            };
            let mut look = skip_space(&chars, end);
            if look == end {
                return false;
            }
            if quote(chars.get(look)) {
                look += 1;
            }
            chars.get(look) == Some(&'+')
        })
    })
}

/// `_heading_token`'s final check: `^[A-Za-z_][A-Za-z0-9_.\[\]]*$`.
fn is_key_path(token: &str) -> bool {
    let body = token.strip_suffix('\n').unwrap_or(token);
    let mut chars = body.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'))
}

/// `_heading_token`: the key path a section heading documents.
pub(crate) fn heading_token(heading: &str) -> Option<String> {
    let chars: Vec<char> = heading.chars().collect();
    let mut found: Option<String> = None;
    for (at, c) in chars.iter().enumerate() {
        if *c != '`' {
            continue;
        }
        let run = chars[at + 1..].iter().take_while(|x| **x != '`').count();
        if run >= 1 && chars.get(at + 1 + run) == Some(&'`') {
            found = Some(chars[at + 1..at + 1 + run].iter().collect());
            break;
        }
    }
    let token = found.unwrap_or_else(|| heading.split(' ').next().unwrap_or("").to_string());
    let token = py_strip(&token);
    is_key_path(token).then(|| token.to_string())
}

/// `_heading_matches`: an exact match, or a prefix match ending on a word
/// boundary.
pub(crate) fn heading_matches(heading: &str, wanted: &str) -> bool {
    let normalise = |text: &str| py_strip(&text.replace('`', "")).to_lowercase();
    let h = normalise(heading);
    let w = normalise(wanted);
    if h == w {
        return true;
    }
    let Some(rest) = h.strip_prefix(w.as_str()) else {
        return false;
    };
    rest.chars().next().is_some_and(|next| !is_word(next))
}

/// One piece of a compiled `fnmatch` pattern.
enum Piece {
    Literal(char),
    Any,
    AnyRun,
    Class {
        negated: bool,
        members: Vec<(char, char)>,
    },
    Never,
}

fn compile_glob(pattern: &str) -> Vec<Piece> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut pieces = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        at += 1;
        match c {
            '*' => {
                if !matches!(pieces.last(), Some(Piece::AnyRun)) {
                    pieces.push(Piece::AnyRun);
                }
            }
            '?' => pieces.push(Piece::Any),
            '[' => {
                let mut close = at;
                if chars.get(close) == Some(&'!') {
                    close += 1;
                }
                if chars.get(close) == Some(&']') {
                    close += 1;
                }
                while close < chars.len() && chars[close] != ']' {
                    close += 1;
                }
                if close >= chars.len() {
                    pieces.push(Piece::Literal('['));
                    continue;
                }
                let mut stuff = &chars[at..close];
                at = close + 1;
                let negated = stuff.first() == Some(&'!');
                if negated {
                    stuff = &stuff[1..];
                }
                if stuff.is_empty() && !negated {
                    pieces.push(Piece::Never);
                    continue;
                }
                let mut members = Vec::new();
                let mut index = 0;
                while index < stuff.len() {
                    if index + 2 < stuff.len() && stuff[index + 1] == '-' {
                        members.push((stuff[index], stuff[index + 2]));
                        index += 3;
                    } else {
                        members.push((stuff[index], stuff[index]));
                        index += 1;
                    }
                }
                pieces.push(Piece::Class { negated, members });
            }
            other => pieces.push(Piece::Literal(other)),
        }
    }
    pieces
}

fn glob_here(pieces: &[Piece], text: &[char]) -> bool {
    let Some((piece, rest)) = pieces.split_first() else {
        return text.is_empty();
    };
    match piece {
        Piece::AnyRun => (0..=text.len()).any(|taken| glob_here(rest, &text[taken..])),
        Piece::Never => false,
        _ => {
            let Some((&c, tail)) = text.split_first() else {
                return false;
            };
            let ok = match piece {
                Piece::Literal(literal) => *literal == c,
                Piece::Any => true,
                Piece::Class { negated, members } => {
                    members.iter().any(|(low, high)| *low <= c && c <= *high) != *negated
                }
                Piece::AnyRun | Piece::Never => false,
            };
            ok && glob_here(rest, tail)
        }
    }
}

/// `fnmatch.fnmatch(name, pattern)` on a POSIX system (case-sensitive,
/// `*` matches any character including `/`).
pub(crate) fn fnmatch(name: &str, pattern: &str) -> bool {
    let text: Vec<char> = name.chars().collect();
    glob_here(&compile_glob(pattern), &text)
}
