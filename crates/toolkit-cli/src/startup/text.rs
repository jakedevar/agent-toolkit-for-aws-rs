//! A tiny pattern matcher for the handful of case-insensitive phrase checks the
//! startup validators make, so no second regular-expression engine is added.
//!
//! The Python scripts used `re` with `IGNORECASE` on patterns built only from
//! literals, `\s+`, `\s*`, `\b`, one-of character classes and optional
//! characters. [`Pattern`] models exactly those pieces with Python's `str`
//! semantics for whitespace (`\s`), word characters (`\w`) and case folding;
//! anything else is deliberately not representable.

/// One element of a [`Pattern`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum Tok {
    /// Literal text, compared case-insensitively.
    Lit(&'static str),
    /// `\s+`
    Space1,
    /// `\s*`
    Space0,
    /// `\b`
    Boundary,
    /// A character class such as `["']`.
    OneOf(&'static str),
    /// An optional single character such as `s?` (case-insensitive).
    Opt(char),
    /// A group of literal alternatives such as `(?:method|request)`, tried in
    /// order (case-insensitive); an empty alternative makes the group optional.
    Either(&'static [&'static str]),
}

/// A compiled phrase pattern plus the Python source it stands for (the
/// validators print that source in their messages).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pattern {
    source: &'static str,
    toks: &'static [Tok],
}

impl Pattern {
    pub(crate) const fn new(source: &'static str, toks: &'static [Tok]) -> Self {
        Self { source, toks }
    }

    /// The regular expression text the Python script printed for this check.
    pub(crate) fn source(&self) -> &'static str {
        self.source
    }

    /// `re.search`: whether the pattern matches anywhere in `text`.
    pub(crate) fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        (0..=chars.len()).any(|start| end_at(self.toks, &chars, start).is_some())
    }

    /// `re.finditer`: the start offsets (in characters) of the non-overlapping
    /// matches, scanning left to right.
    pub(crate) fn find_starts(&self, text: &str) -> Vec<usize> {
        let chars: Vec<char> = text.chars().collect();
        let mut starts = Vec::new();
        let mut pos = 0;
        while pos <= chars.len() {
            if let Some(end) = end_at(self.toks, &chars, pos) {
                starts.push(pos);
                pos = end.max(pos + 1);
            } else {
                pos += 1;
            }
        }
        starts
    }
}

/// Python `\s` for `str`: Unicode whitespace plus `\x1c`-`\x1f`.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python `\w` for `str`: `str.isalnum()` characters (letters, `L*`, and
/// numbers, `N*`) and the underscore. Combining marks, join controls and
/// connector punctuation other than `_` are NOT word characters, although
/// Rust's `char::is_alphanumeric` (Unicode `Alphabetic`, which includes the
/// combining vowel signs) and the regex engines' Unicode `\w` count some of
/// them (#4585).
pub(crate) fn is_word(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    // Every Python word character is `Alphabetic` or `Numeric` to Rust; only
    // that rare non-ASCII subset needs the exact general-category check.
    c.is_alphanumeric() && is_letter_or_number(c)
}

/// `\p{L}` or `\p{N}`, which is Python's `isalpha() or isnumeric()`.
fn is_letter_or_number(c: char) -> bool {
    static CLASS: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(r"\A[\p{L}\p{N}]\z").expect("a valid category class")
    });
    let mut buffer = [0_u8; 4];
    CLASS.is_match(c.encode_utf8(&mut buffer)).unwrap_or(false)
}

/// Simple case folding as used by `re.IGNORECASE`, including the extra
/// equivalences Python adds (dotted/dotless i, long s, Kelvin sign).
pub(crate) fn fold(c: char) -> char {
    match c {
        '\u{130}' | '\u{131}' => 'i',
        '\u{17f}' => 's',
        '\u{212a}' => 'k',
        _ => {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(single), None) => single,
                _ => c,
            }
        }
    }
}

/// Where the first successful (backtracking) match of `toks` at `pos` ends.
fn end_at(toks: &[Tok], chars: &[char], pos: usize) -> Option<usize> {
    let Some((tok, rest)) = toks.split_first() else {
        return Some(pos);
    };
    match *tok {
        Tok::Lit(text) => literal_end(text, chars, pos).and_then(|at| end_at(rest, chars, at)),
        Tok::Space1 | Tok::Space0 => {
            let minimum = usize::from(matches!(tok, Tok::Space1));
            let run = chars[pos..].iter().take_while(|c| is_space(**c)).count();
            (minimum..=run)
                .rev()
                .find_map(|taken| end_at(rest, chars, pos + taken))
        }
        Tok::Boundary => {
            let before = pos > 0 && is_word(chars[pos - 1]);
            let after = chars.get(pos).is_some_and(|c| is_word(*c));
            if before == after {
                None
            } else {
                end_at(rest, chars, pos)
            }
        }
        Tok::OneOf(set) => {
            if chars.get(pos).is_some_and(|c| set.contains(*c)) {
                end_at(rest, chars, pos + 1)
            } else {
                None
            }
        }
        Tok::Opt(optional) => {
            let taken = chars.get(pos).is_some_and(|c| fold(*c) == fold(optional));
            taken
                .then(|| end_at(rest, chars, pos + 1))
                .flatten()
                .or_else(|| end_at(rest, chars, pos))
        }
        Tok::Either(alternatives) => alternatives.iter().find_map(|alternative| {
            literal_end(alternative, chars, pos).and_then(|at| end_at(rest, chars, at))
        }),
    }
}

/// The position after case-insensitive `text` at `pos`, if it is there.
fn literal_end(text: &str, chars: &[char], pos: usize) -> Option<usize> {
    let mut at = pos;
    for expected in text.chars() {
        match chars.get(at) {
            Some(&actual) if fold(actual) == fold(expected) => at += 1,
            _ => return None,
        }
    }
    Some(at)
}
