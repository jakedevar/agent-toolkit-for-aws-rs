//! A small wrapper over `fancy-regex`, the engine that stands in for Python's
//! `re` (look-behind, look-ahead and back-references are used by the report
//! validator's patterns, which the `regex` crate cannot express).
//!
//! Offsets are byte offsets into the searched text. A search that the engine
//! cannot complete reports "no match".

use fancy_regex::{Regex, RegexBuilder};

/// A byte span `(start, end)` of a match.
pub(super) type Span = (usize, usize);

/// Capture groups of one match: group 0 is the whole match.
pub(super) type Groups = Vec<Option<Span>>;

/// The body of Python's `str` word class, `\w`: letters, numbers and `_`
/// (`str.isalnum()` plus the underscore). The engine's own `\w` and `\b` also
/// count combining marks, join controls and connector punctuation, which lets
/// `TODO` followed by U+0301 slip past `\bTODO\b` (#4585).
const WORD_CLASS: &str = r"\p{L}\p{N}_";

/// Rewrites Python `re` word escapes into explicit classes and look-arounds
/// over [`WORD_CLASS`], so the patterns keep their Python meaning.
///
/// `\b` is a position between a word and a non-word character (or the text
/// edge); `\w` becomes the class and `\W` its complement outside a bracket
/// class. Escapes inside `[...]` keep their position (`[\w'-]`).
fn python_word_semantics(pattern: &str) -> String {
    let word = format!("[{WORD_CLASS}]");
    let boundary = format!("(?:(?<={word})(?!{word})|(?<!{word})(?={word}))");
    let mut out = String::with_capacity(pattern.len() + 64);
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(next) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match (next, in_class) {
                    ('b', false) => out.push_str(&boundary),
                    ('w', false) => out.push_str(&word),
                    ('w', true) => out.push_str(WORD_CLASS),
                    ('W', false) => {
                        out.push_str(&format!("[^{WORD_CLASS}]"));
                    }
                    ('B', false) => {
                        out.push_str(&format!("(?:(?<={word})(?={word})|(?<!{word})(?!{word}))"));
                    }
                    ('b' | 'W' | 'B', true) => {
                        panic!("a word escape inside a bracket class is not supported");
                    }
                    _ => {
                        out.push('\\');
                        out.push(next);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push(c);
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// A compiled pattern.
pub(super) struct Rx(Regex);

/// Compiles a pattern once per call site and returns a `&'static Rx`.
macro_rules! rx {
    ($pattern:expr) => {{
        static CELL: std::sync::LazyLock<$crate::startup::report::rx::Rx> =
            std::sync::LazyLock::new(|| $crate::startup::report::rx::Rx::new($pattern));
        &*CELL
    }};
}
pub(super) use rx;

impl Rx {
    /// Compiles `pattern`; the patterns are literals of this crate.
    pub(super) fn new(pattern: &str) -> Self {
        let regex = RegexBuilder::new(&python_word_semantics(pattern))
            .backtrack_limit(usize::MAX)
            .build()
            .expect("the report validator patterns are valid");
        Self(regex)
    }

    /// `re.search(...) is not None`.
    pub(super) fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text).unwrap_or(false)
    }

    /// `re.search`: the span of the first match.
    pub(super) fn find(&self, text: &str) -> Option<Span> {
        self.0
            .find(text)
            .ok()
            .flatten()
            .map(|found| (found.start(), found.end()))
    }

    /// `re.finditer`: the spans of every non-overlapping match.
    pub(super) fn find_all(&self, text: &str) -> Vec<Span> {
        self.0
            .find_iter(text)
            .filter_map(Result::ok)
            .map(|found| (found.start(), found.end()))
            .collect()
    }

    /// `re.search` with the groups of the first match.
    pub(super) fn captures(&self, text: &str) -> Option<Groups> {
        let captures = self.0.captures(text).ok().flatten()?;
        Some(
            (0..captures.len())
                .map(|index| {
                    captures
                        .get(index)
                        .map(|found| (found.start(), found.end()))
                })
                .collect(),
        )
    }

    /// `re.finditer` with the groups of every match.
    pub(super) fn captures_all(&self, text: &str) -> Vec<Groups> {
        self.0
            .captures_iter(text)
            .filter_map(Result::ok)
            .map(|captures| {
                (0..captures.len())
                    .map(|index| {
                        captures
                            .get(index)
                            .map(|found| (found.start(), found.end()))
                    })
                    .collect()
            })
            .collect()
    }
}

/// The text of group `index`, or the empty string when it did not take part.
pub(super) fn group<'a>(text: &'a str, groups: &Groups, index: usize) -> &'a str {
    groups
        .get(index)
        .copied()
        .flatten()
        .map_or("", |(start, end)| &text[start..end])
}
