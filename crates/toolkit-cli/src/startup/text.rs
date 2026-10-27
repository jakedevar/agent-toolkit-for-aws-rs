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
        (0..=chars.len()).any(|start| matches_at(self.toks, &chars, start))
    }
}

/// Python `\s` for `str`: Unicode whitespace plus `\x1c`-`\x1f`.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python `\w` for `str`: alphanumerics and the underscore.
pub(crate) fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

fn matches_at(toks: &[Tok], chars: &[char], pos: usize) -> bool {
    let Some((tok, rest)) = toks.split_first() else {
        return true;
    };
    match *tok {
        Tok::Lit(text) => {
            let mut at = pos;
            for expected in text.chars() {
                match chars.get(at) {
                    Some(&actual) if fold(actual) == fold(expected) => at += 1,
                    _ => return false,
                }
            }
            matches_at(rest, chars, at)
        }
        Tok::Space1 | Tok::Space0 => {
            let minimum = usize::from(matches!(tok, Tok::Space1));
            let run = chars[pos..].iter().take_while(|c| is_space(**c)).count();
            (minimum..=run)
                .rev()
                .any(|taken| matches_at(rest, chars, pos + taken))
        }
        Tok::Boundary => {
            let before = pos > 0 && is_word(chars[pos - 1]);
            let after = chars.get(pos).is_some_and(|c| is_word(*c));
            before != after && matches_at(rest, chars, pos)
        }
        Tok::OneOf(set) => {
            chars.get(pos).is_some_and(|c| set.contains(*c)) && matches_at(rest, chars, pos + 1)
        }
        Tok::Opt(optional) => {
            (chars.get(pos).is_some_and(|c| fold(*c) == fold(optional))
                && matches_at(rest, chars, pos + 1))
                || matches_at(rest, chars, pos)
        }
    }
}
