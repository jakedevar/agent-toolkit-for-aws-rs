//! A small backtracking regular-expression matcher for JSON Schema `pattern`
//! keywords.
//!
//! The Python validator relied on `re.search` through `jsonschema`. The
//! toolkit has no regex dependency, so this module implements the subset the
//! shipped schemas need, with Python semantics for it:
//!
//! - literals, `.` (any character except `\n`), `^`, `$` (end of text or just
//!   before a final `\n`, as in Python), alternation `|`;
//! - capturing and non-capturing groups `( )` / `(?: )` and the look-aheads
//!   `(?= )` / `(?! )`;
//! - classes `[a-z0-9.-]` / `[^...]` with ranges and escaped punctuation;
//! - greedy quantifiers `*`, `+`, `?`, `{n}`, `{n,}`, `{,m}`, `{n,m}`;
//! - escapes of punctuation (`\.`, `\$`, `\{` ...) and `\n \t \r \f \v`.
//!
//! Everything else (lazy or possessive quantifiers, back-references, shorthand
//! classes such as `\d`, flags, named groups, look-behind) is rejected when the
//! pattern is compiled, so an unsupported schema fails loudly instead of being
//! mis-evaluated. The matcher has a recursion budget; a pattern that exceeds it
//! reports an error instead of overflowing the stack.

use std::cell::Cell;

/// Largest recursion depth the matcher uses before it gives up.
const MAX_DEPTH: usize = 1000;

/// Marker for "no upper bound" in a quantifier.
const UNBOUNDED: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    Group(Vec<Vec<Node>>),
    Look {
        negate: bool,
        alternatives: Vec<Vec<Node>>,
    },
    Repeat {
        node: Box<Node>,
        min: usize,
        max: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Class {
    negate: bool,
    ranges: Vec<(char, char)>,
}

impl Class {
    fn matches(&self, c: char) -> bool {
        self.ranges.iter().any(|&(low, high)| low <= c && c <= high) != self.negate
    }
}

/// A compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Regex {
    alternatives: Vec<Vec<Node>>,
}

impl Regex {
    /// Compiles `pattern`.
    ///
    /// # Errors
    ///
    /// Returns a description of the first unsupported or malformed construct.
    pub(crate) fn new(pattern: &str) -> Result<Self, String> {
        let mut parser = Parser {
            chars: pattern.chars().collect(),
            pos: 0,
        };
        let alternatives = parser.alternatives()?;
        if parser.pos < parser.chars.len() {
            return Err("unbalanced parenthesis".to_string());
        }
        Ok(Self { alternatives })
    }

    /// Python's `re.search(pattern, text) is not None`.
    ///
    /// # Errors
    ///
    /// Returns an error when the recursion budget is exhausted.
    pub(crate) fn is_match(&self, text: &str) -> Result<bool, String> {
        let chars: Vec<char> = text.chars().collect();
        let matcher = Matcher {
            text: &chars,
            depth: Cell::new(0),
            exceeded: Cell::new(false),
        };
        for start in 0..=chars.len() {
            let found = self
                .alternatives
                .iter()
                .any(|alternative| matcher.sequence(alternative, start, &mut |_| true));
            if found {
                return Ok(true);
            }
            if matcher.exceeded.get() {
                return Err("pattern is too complex for this input".to_string());
            }
        }
        Ok(false)
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// Parses `a|b|c` up to the end of the input or an unconsumed `)`.
    fn alternatives(&mut self) -> Result<Vec<Vec<Node>>, String> {
        let mut alternatives: Vec<Vec<Node>> = vec![Vec::new()];
        loop {
            match self.peek() {
                None | Some(')') => break,
                Some('|') => {
                    self.pos += 1;
                    alternatives.push(Vec::new());
                }
                Some(_) => {
                    let atom = self.atom()?;
                    let node = self.quantified(atom)?;
                    if let Some(current) = alternatives.last_mut() {
                        current.push(node);
                    }
                }
            }
        }
        Ok(alternatives)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let Some(c) = self.bump() else {
            return Err("unexpected end of pattern".to_string());
        };
        match c {
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '.' => Ok(Node::Any),
            '(' => self.group(),
            '[' => self.class(),
            '\\' => self.escape(),
            '*' | '+' | '?' => Err(format!("nothing to repeat before '{c}'")),
            other => Ok(Node::Char(other)),
        }
    }

    fn group(&mut self) -> Result<Node, String> {
        let mut look: Option<bool> = None;
        if self.peek() == Some('?') {
            self.pos += 1;
            match self.bump() {
                Some(':') => {}
                Some('=') => look = Some(false),
                Some('!') => look = Some(true),
                _ => return Err("unsupported group extension".to_string()),
            }
        }
        let alternatives = self.alternatives()?;
        if self.bump() != Some(')') {
            return Err("missing closing parenthesis".to_string());
        }
        Ok(match look {
            Some(negate) => Node::Look {
                negate,
                alternatives,
            },
            None => Node::Group(alternatives),
        })
    }

    fn escape(&mut self) -> Result<Node, String> {
        let Some(c) = self.bump() else {
            return Err("pattern ends with a backslash".to_string());
        };
        Ok(Node::Char(Self::escaped_char(c)?))
    }

    fn escaped_char(c: char) -> Result<char, String> {
        match c {
            'n' => Ok('\n'),
            't' => Ok('\t'),
            'r' => Ok('\r'),
            'f' => Ok('\u{c}'),
            'v' => Ok('\u{b}'),
            c if c.is_ascii_alphanumeric() => Err(format!("unsupported escape '\\{c}'")),
            other => Ok(other),
        }
    }

    /// One character inside `[...]`, resolving a backslash escape.
    fn class_char(&mut self) -> Result<char, String> {
        match self.bump() {
            None => Err("missing closing bracket".to_string()),
            Some('\\') => match self.bump() {
                Some(escaped) => Self::escaped_char(escaped),
                None => Err("pattern ends with a backslash".to_string()),
            },
            Some(c) => Ok(c),
        }
    }

    fn class(&mut self) -> Result<Node, String> {
        let negate = self.peek() == Some('^');
        if negate {
            self.pos += 1;
        }
        let mut ranges: Vec<(char, char)> = Vec::new();
        let mut first = true;
        loop {
            if self.peek() == Some(']') && !first {
                self.pos += 1;
                break;
            }
            first = false;
            let low = self.class_char()?;
            // A range needs a following `-` that is not the last class character.
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|&n| n != ']') {
                self.pos += 1;
                let high = self.class_char()?;
                if high < low {
                    return Err("bad character range".to_string());
                }
                ranges.push((low, high));
            } else {
                ranges.push((low, low));
            }
        }
        Ok(Node::Class(Class { negate, ranges }))
    }

    /// Applies a following quantifier to `atom`, if there is one.
    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let bounds = match self.peek() {
            Some('*') => {
                self.pos += 1;
                Some((0, UNBOUNDED))
            }
            Some('+') => {
                self.pos += 1;
                Some((1, UNBOUNDED))
            }
            Some('?') => {
                self.pos += 1;
                Some((0, 1))
            }
            Some('{') => self.braces(),
            _ => None,
        };
        let Some((min, max)) = bounds else {
            return Ok(atom);
        };
        if matches!(self.peek(), Some('?' | '+')) {
            return Err("lazy and possessive quantifiers are not supported".to_string());
        }
        if min > max {
            return Err("quantifier minimum is larger than its maximum".to_string());
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
        })
    }

    /// Parses `{n}`, `{n,}`, `{,m}` or `{n,m}`; anything else leaves the `{`
    /// to be read as a literal (Python does the same).
    fn braces(&mut self) -> Option<(usize, usize)> {
        let start = self.pos;
        self.pos += 1;
        let low = self.number();
        let (min, max) = if self.peek() == Some(',') {
            self.pos += 1;
            let high = self.number();
            (low.unwrap_or(0), high.unwrap_or(UNBOUNDED))
        } else {
            let Some(n) = low else {
                self.pos = start;
                return None;
            };
            (n, n)
        };
        if self.peek() == Some('}') {
            self.pos += 1;
            Some((min, max))
        } else {
            self.pos = start;
            None
        }
    }

    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }
}

struct Matcher<'t> {
    text: &'t [char],
    depth: Cell<usize>,
    exceeded: Cell<bool>,
}

type Continuation<'k> = &'k mut dyn FnMut(usize) -> bool;

impl Matcher<'_> {
    fn at_end(&self, pos: usize) -> bool {
        pos == self.text.len() || (pos + 1 == self.text.len() && self.text[pos] == '\n')
    }

    /// Matches `nodes` at `pos` and calls `k` with the end position of each
    /// way the sequence can match until `k` accepts one.
    fn sequence(&self, nodes: &[Node], pos: usize, k: Continuation<'_>) -> bool {
        if self.depth.get() >= MAX_DEPTH {
            self.exceeded.set(true);
            return false;
        }
        self.depth.set(self.depth.get() + 1);
        let matched = self.step(nodes, pos, k);
        self.depth.set(self.depth.get() - 1);
        matched
    }

    fn single(node: &Node, c: char) -> bool {
        match node {
            Node::Char(expected) => *expected == c,
            Node::Any => c != '\n',
            Node::Class(class) => class.matches(c),
            _ => false,
        }
    }

    fn step(&self, nodes: &[Node], pos: usize, k: Continuation<'_>) -> bool {
        let Some((node, rest)) = nodes.split_first() else {
            return k(pos);
        };
        match node {
            Node::Char(_) | Node::Any | Node::Class(_) => {
                self.text.get(pos).is_some_and(|&c| Self::single(node, c))
                    && self.sequence(rest, pos + 1, k)
            }
            Node::Start => pos == 0 && self.sequence(rest, pos, k),
            Node::End => self.at_end(pos) && self.sequence(rest, pos, k),
            Node::Group(alternatives) => alternatives.iter().any(|alternative| {
                self.sequence(alternative, pos, &mut |end| self.sequence(rest, end, k))
            }),
            Node::Look {
                negate,
                alternatives,
            } => {
                let found = alternatives
                    .iter()
                    .any(|alternative| self.sequence(alternative, pos, &mut |_| true));
                found != *negate && self.sequence(rest, pos, k)
            }
            Node::Repeat { node, min, max } => {
                if matches!(**node, Node::Char(_) | Node::Any | Node::Class(_)) {
                    self.repeat_single(node, *min, *max, pos, rest, k)
                } else {
                    self.repeat_general(node, *min, *max, 0, pos, rest, k)
                }
            }
        }
    }

    /// Greedy repeat of a one-character node without recursion per character.
    fn repeat_single(
        &self,
        node: &Node,
        min: usize,
        max: usize,
        pos: usize,
        rest: &[Node],
        k: Continuation<'_>,
    ) -> bool {
        let mut count = 0;
        while count < max
            && self
                .text
                .get(pos + count)
                .is_some_and(|&c| Self::single(node, c))
        {
            count += 1;
        }
        if count < min {
            return false;
        }
        loop {
            if self.sequence(rest, pos + count, k) {
                return true;
            }
            if count == min {
                return false;
            }
            count -= 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat_general(
        &self,
        node: &Node,
        min: usize,
        max: usize,
        count: usize,
        pos: usize,
        rest: &[Node],
        k: Continuation<'_>,
    ) -> bool {
        if count < max {
            let one = std::slice::from_ref(node);
            let matched = self.sequence(one, pos, &mut |end| {
                if end == pos && count >= min {
                    return false;
                }
                self.repeat_general(node, min, max, count + 1, end, rest, k)
            });
            if matched {
                return true;
            }
        }
        count >= min && self.sequence(rest, pos, k)
    }
}

#[cfg(test)]
mod tests {
    use super::Regex;

    fn search(pattern: &str, text: &str) -> bool {
        Regex::new(pattern)
            .expect("pattern compiles")
            .is_match(text)
            .expect("input within budget")
    }

    const NAME: &str = r"^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$";
    const CWD: &str = r"^(?:\./|\$\{PLUGIN_ROOT\}(?:/|$)|\$\{PLUGIN_DATA\}(?:/|$))";

    #[test]
    fn name_pattern_accepts_plugin_names() {
        for good in ["a", "aws-core", "a1", "x.y-z", "9", "a.b.c", "abc\n"] {
            assert!(search(NAME, good), "{good:?}");
        }
    }

    #[test]
    fn name_pattern_rejects_malformed_names() {
        for bad in [
            "", "-a", "a-", ".a", "a.", "a--b", "a..b", "A", "a_b", "a b", "ab\n\n",
        ] {
            assert!(!search(NAME, bad), "{bad:?}");
        }
    }

    #[test]
    fn cwd_pattern_accepts_plugin_relative_and_placeholder_roots() {
        for good in [
            "./",
            "./x",
            "${PLUGIN_ROOT}",
            "${PLUGIN_ROOT}/x",
            "${PLUGIN_DATA}",
            "${PLUGIN_DATA}/d",
        ] {
            assert!(search(CWD, good), "{good:?}");
        }
    }

    #[test]
    fn cwd_pattern_rejects_other_prefixes() {
        for bad in [
            "",
            "/abs",
            "x",
            "../x",
            ".",
            "${PLUGIN_ROOTX}",
            "${PLUGIN_ROOT}x",
            " ./x",
        ] {
            assert!(!search(CWD, bad), "{bad:?}");
        }
    }

    #[test]
    fn search_is_unanchored_unless_anchored() {
        assert!(search("b+", "aabbc"));
        assert!(!search("^b+", "aabbc"));
        assert!(search("c$", "aabbc"));
        assert!(search("c$", "aabbc\n"));
        assert!(!search("c$", "aabbc\n\n"));
    }

    #[test]
    fn quantifier_forms_are_supported() {
        assert!(search("^a{2}$", "aa"));
        assert!(!search("^a{2}$", "aaa"));
        assert!(search("^a{2,}$", "aaaa"));
        assert!(search("^a{,2}$", "aa"));
        assert!(!search("^a{,2}$", "aaa"));
        assert!(search("^a{1,3}b$", "aab"));
        assert!(search("^(ab)+$", "ababab"));
        assert!(!search("^(ab)+$", "ababa"));
        assert!(search("^(a|b)*c?$", "abba"));
        // A brace that is not a quantifier is a literal.
        assert!(search("^a{x}$", "a{x}"));
    }

    #[test]
    fn classes_support_ranges_negation_and_literal_edges() {
        assert!(search("^[a-c]$", "b"));
        assert!(!search("^[a-c]$", "d"));
        assert!(search("^[^a-c]$", "d"));
        assert!(search("^[]a]$", "]"));
        assert!(search("^[a-]$", "-"));
        assert!(search(r"^[\.\-]$", "."));
        assert!(search("^[.]$", "."));
        assert!(!search("^[.]$", "x"));
    }

    #[test]
    fn dot_does_not_match_newline() {
        assert!(search("^.$", "x"));
        assert!(!search("^.$", "\n"));
    }

    #[test]
    fn lookaheads_are_zero_width() {
        assert!(search("^(?=ab)a", "ab"));
        assert!(!search("^(?=ab)a", "ac"));
        assert!(search("^(?!ab)a", "ac"));
        assert!(!search("^(?!ab)a", "ab"));
    }

    #[test]
    fn unsupported_constructs_are_rejected_when_compiling() {
        for pattern in [
            r"\d", r"\w+", "a*?", "a++", "(?<n>a)", "(?i)a", "(a", "a)", "[a", "[z-a]", "*a", r"\",
            r"\1", "a{3,2}",
        ] {
            assert!(Regex::new(pattern).is_err(), "{pattern:?}");
        }
    }

    #[test]
    fn long_inputs_do_not_overflow_the_stack() {
        let long = "a".repeat(100_000);
        assert!(search("^[a-z]*$", &long));
        // Group repetition recurses per iteration; the budget turns that into
        // an error rather than a crash.
        let regex = Regex::new("^(ab)*$").expect("compiles");
        let result = regex.is_match(&"ab".repeat(50_000));
        assert!(result.is_err());
        assert!(search("^(ab)*$", &"ab".repeat(100)));
    }
}
