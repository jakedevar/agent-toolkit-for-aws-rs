//! Printing untrusted text in diagnostics (#4602, #4941).
//!
//! Values read from input files (JSON keys, strings, file names) and values
//! given on the command line (paths above all) must not reach a terminal or a
//! log as raw bytes: a carriage return, an escape sequence, a bidirectional
//! override or a line separator can forge report lines or drive the terminal,
//! an unbounded value can flood the output, and a credential-shaped value
//! (URL userinfo, `api_key=...`, an access-key id) must not be echoed at all.
//!
//! Every helper here applies one pipeline, in this order (#4941):
//!
//! 1. **redact** the whole value with the shared `toolkit_aws::redact_text`;
//! 2. **bound** the redacted text on a character boundary
//!    ([`MAX_PATH_CHARS`] for a caller path or value, [`MAX_SHOWN_CHARS`] for
//!    other text), marking a cut with `...`;
//! 3. **escape** control, bidirectional and invisible characters the way
//!    Python's `repr` does (`\x1b`, `\u202e`, `\U000e0001`).
//!
//! Redacting first is the point: a cut made first can drop the `@host` that
//! marks URL userinfo or split an access-key id, so the redactor no longer
//! recognizes what is left of the credential (#4907, #4908, #4939, #4940).
//! Text without credential-shaped content and within the bound is returned
//! unchanged, so ordinary output is byte-identical.
//!
//! Redaction happens at the value, before it is formatted (#4971 review):
//! a renderer that quotes or escapes on its own (a Python `repr`, a quoted
//! name, a JSON string) is given [`shown_value`] (redacted, then bounded) and
//! a value written into stdout data (a JSON report, a template) is given
//! [`redacted_value`] (redacted whole, never cut). Stdout is never rewritten
//! after it is formatted, so its JSON stays valid and its typed fields keep
//! their schema (#4981). These helpers also hide what another argument of the
//! run taught ([`CallerText::enter_run`]): a flag repeated with the bare
//! credential is an ordinary value to the redactor, not to this run.
//!
//! [`CallerText`] is a stderr safety net behind that rule, never the only
//! protection: a diagnostic that still echoes a command-line value raw (or
//! cut before redaction) does not print its credential. It finds every match
//! against the original text before replacing any (#4978), ignoring the
//! backslashes a renderer inserts (#4979), and hides every learned piece
//! whatever its length (#4980). Its work is linear in the text and the learned
//! values, however much a credential overlaps itself, and capped: past
//! [`MAX_MATCH_WORK`] steps or [`MAX_SPANS`] matches it hides every line of the
//! diagnostic instead (#5012).
//!
//! A piece of one to three characters matches as a plain substring, so a run
//! that passes such a credential may see ordinary letters of its diagnostics
//! hidden. That is the chosen trade-off: the mask never leaks, and a document
//! with a schema is kept valid by masking its caller strings where they are
//! built ([`redacted_value`], [`Output::json_diagnostic`](crate::Output)), not
//! by weakening the mask.
//!
//! A list value is redacted whole before it is split (#5008): a comma inside
//! URL userinfo would otherwise cut the credential from its `@host`.

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::rc::Rc;

use toolkit_aws::{redact_text, REDACTED};
use toolkit_core::{escape_untrusted, is_unsafe_char, neutralize_line, neutralize_text};

/// Longest text, in characters, that [`safe_text`] prints before `...`.
pub(crate) const MAX_SHOWN_CHARS: usize = 120;

/// Longest caller path or command-line value, in characters, that
/// [`safe_path`] and [`CallerText`] print before `...` (#4941). It is counted
/// on the redacted text, before escaping.
pub(crate) const MAX_PATH_CHARS: usize = 200;

/// The mark that ends a bounded text.
const ELLIPSIS: &str = "...";

/// A credential echoed in part (cut by a slice before redaction) is
/// recognized from this many leading characters on.
const MIN_PARTIAL_CHARS: usize = 8;

/// Whether `c` must be escaped before it is printed: control characters
/// (C0, DEL, C1) and the invisible format, separator and tag characters that
/// reorder or hide text.
pub(crate) fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{34f}'
                | '\u{61c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

fn push_escaped(out: &mut String, c: char) {
    use std::fmt::Write as _;

    let code = u32::from(c);
    let _ = if code <= 0xff {
        write!(out, "\\x{code:02x}")
    } else if code <= 0xffff {
        write!(out, "\\u{code:04x}")
    } else {
        write!(out, "\\U{code:08x}")
    };
}

/// Step 3: `text` with every [`is_unsafe`] character escaped, nothing cut.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe(c) {
            push_escaped(&mut out, c);
        } else {
            out.push(c);
        }
    }
    out
}

/// Step 2: the first `count` characters of `text`, then [`ELLIPSIS`] when
/// anything was cut. Never splits a character.
fn bounded(text: &str, count: usize) -> Cow<'_, str> {
    match text.char_indices().nth(count) {
        Some((end, _)) => Cow::Owned(format!("{}{ELLIPSIS}", &text[..end])),
        None => Cow::Borrowed(text),
    }
}

/// The whole pipeline: redact all of `text`, bound the result to `count`
/// characters, escape what is left.
fn redact_bound_escape(text: &str, count: usize) -> String {
    escaped(&bounded(&redacted(text), count))
}

/// `text` made safe to print: redacted, at most [`MAX_SHOWN_CHARS`]
/// characters shown (then `...`), unsafe characters escaped. Text without
/// credential-shaped content or unsafe characters and within the bound is
/// returned unchanged.
pub(crate) fn safe_text(text: &str) -> String {
    redact_bound_escape(text, MAX_SHOWN_CHARS)
}

/// A caller path (or other caller value) made safe to print: redacted in full,
/// at most [`MAX_PATH_CHARS`] characters shown (then `...`), unsafe
/// characters escaped (#4941). Only the displayed text changes: read and write
/// the file through the path exactly as given. An ordinary path of at most
/// [`MAX_PATH_CHARS`] characters is returned unchanged.
pub(crate) fn safe_path(text: &str) -> String {
    redact_bound_escape(text, MAX_PATH_CHARS)
}

/// The first `count` characters of the redacted `text`, escaped, with no
/// mark: Python's `text[:count]` for a slice whose source cuts text silently.
/// The whole text is redacted before the cut.
pub(crate) fn safe_prefix(text: &str, count: usize) -> String {
    escaped(&redacted(text).chars().take(count).collect::<String>())
}

/// Steps 1 and 2 for a renderer that escapes on its own (a Python `repr`, a
/// quoted name): `text` redacted in full, then bounded to [`MAX_PATH_CHARS`]
/// characters (#4979). Pass the result, never the raw value, to the renderer.
/// An ordinary value within the bound is returned unchanged.
pub(crate) fn shown_value(text: &str) -> Cow<'_, str> {
    match redacted_value(text) {
        Cow::Borrowed(same) => bounded(same, MAX_PATH_CHARS),
        Cow::Owned(redacted) => Cow::Owned(bounded(&redacted, MAX_PATH_CHARS).into_owned()),
    }
}

/// [`shown_value`] of a caller path, for a message that embeds it before a
/// sink escapes it (`not found: {}`).
pub(crate) fn shown_path(path: &std::path::Path) -> String {
    shown_value(&path.display().to_string()).into_owned()
}

/// Step 1 only, for a command-line value written into stdout *data* (a JSON
/// report member, a generated template): `text` redacted in full and never
/// cut, so a legitimate long value (an SNS topic ARN) stays whole (#4981). The
/// serializer escapes it. An ordinary value is returned unchanged.
pub(crate) fn redacted_value(text: &str) -> Cow<'_, str> {
    let redacted = redacted(text);
    if redacted == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(redacted)
    }
}

thread_local! {
    /// The command-line values of the run on this thread.
    static RUN_CALLER: RefCell<Option<Rc<CallerText>>> = const { RefCell::new(None) };
}

/// Restores the previous run's values when a [`CallerText::enter_run`] ends.
#[must_use]
pub(crate) struct RunScope(Option<Rc<CallerText>>);

impl Drop for RunScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        RUN_CALLER.with_borrow_mut(|current| *current = previous);
    }
}

/// `text` as the shared redactor shows it, then with every piece another
/// argument of this run taught hidden too. Step 1 of every helper here.
fn redacted(text: &str) -> String {
    let redacted = redact_text(text);
    RUN_CALLER.with_borrow(|caller| match caller {
        Some(caller) => caller.hide_pieces(&redacted).into_owned(),
        None => redacted,
    })
}

/// [`safe_text`] between single quotes, for a name inside a sentence.
pub(crate) fn safe_quoted(text: &str) -> String {
    format!("'{}'", safe_text(text).replace('\'', "\\'"))
}

/// Most matcher steps the stderr net may spend on one diagnostic (#5012).
/// A diagnostic whose matching could take more is not scanned: every line of
/// it is hidden instead (fail closed). Ordinary diagnostics are far below it:
/// it allows, for instance, 64 learned fragments over a 256 KiB text.
const MAX_MATCH_WORK: usize = 1 << 26;

/// Most matched spans the stderr net retains for one diagnostic: pieces of one
/// character can match at every byte, for each piece. Past it every line is
/// hidden, as past [`MAX_MATCH_WORK`].
const MAX_SPANS: usize = 1 << 20;

/// `text` with every non-empty line replaced by [`REDACTED`], line breaks
/// kept: what a diagnostic past [`MAX_MATCH_WORK`] prints.
fn hidden_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| if line.is_empty() { "" } else { REDACTED })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Calls `found(at, len)`, in increasing `at`, for every byte position `at`
/// of `text` where `text[at..]` and `pattern` share a non-empty prefix:
/// `len` is the length of that common prefix in bytes, on a character
/// boundary. Linear work in `pattern` plus `text` (the Z algorithm over
/// `pattern`, a separator byte that UTF-8 never holds, and `text`), however
/// much the occurrences overlap (#5012).
fn prefix_matches(pattern: &str, text: &str, mut found: impl FnMut(usize, usize)) {
    let (head, tail) = (pattern.as_bytes(), text.as_bytes());
    let width = head.len();
    if width == 0 {
        return;
    }
    let total = width + 1 + tail.len();
    let byte = |index: usize| match index.cmp(&width) {
        std::cmp::Ordering::Less => head[index],
        std::cmp::Ordering::Equal => 0xff,
        std::cmp::Ordering::Greater => tail[index - width - 1],
    };
    // Z values are only ever looked up inside the pattern: a match never
    // crosses the separator, so `pos - left < right - left <= width`.
    let mut zeta = vec![0_usize; width + 1];
    let (mut left, mut right) = (0_usize, 0_usize);
    for pos in 1..total {
        let mut run = if pos < right {
            (right - pos).min(zeta[pos - left])
        } else {
            0
        };
        while pos + run < total && byte(run) == byte(pos + run) {
            tick(1);
            run += 1;
        }
        tick(1);
        if pos + run > right {
            left = pos;
            right = pos + run;
        }
        if pos <= width {
            zeta[pos] = run;
        } else if run > 0 {
            let at = pos - width - 1;
            let mut len = run;
            while !text.is_char_boundary(at + len) {
                len -= 1;
            }
            if len > 0 {
                found(at, len);
            }
        }
    }
}

// Count of elementary matcher steps, so tests can prove the stderr net's work
// bound deterministically instead of timing it (#5012). Compiled out of
// non-test builds.
#[cfg(test)]
thread_local! {
    static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn tick(steps: usize) {
    WORK.with(|work| work.set(work.get() + steps));
}

#[cfg(not(test))]
#[inline]
fn tick(_steps: usize) {}

/// `text` with every backslash removed, for matching: a renderer that escapes
/// a value (a Python `repr`'s `\'`, JSON's `\"`, a doubled `\\`) inserts
/// backslashes at any depth, so no fixed list of spellings is complete (#4979).
/// Removing them from the text and from each learned piece makes every
/// insertion the same spelling. Maps a match back to the original bytes.
struct BackslashFree<'t> {
    text: Cow<'t, str>,
    /// For each byte of `text`, where it is in the original; `None` when the
    /// text holds no backslash.
    source: Option<Vec<usize>>,
}

impl<'t> BackslashFree<'t> {
    fn new(original: &'t str) -> Self {
        tick(original.len());
        if !original.contains('\\') {
            return Self {
                text: Cow::Borrowed(original),
                source: None,
            };
        }
        let mut text = String::with_capacity(original.len());
        let mut source = Vec::with_capacity(original.len());
        for (at, c) in original.char_indices() {
            if c != '\\' {
                text.push(c);
                source.extend(at..at + c.len_utf8());
            }
        }
        Self {
            text: Cow::Owned(text),
            source: Some(source),
        }
    }

    /// The bytes of the original that `start..end` of [`Self::text`] came
    /// from. A backslash is ASCII, so both ends stay on character boundaries.
    fn original(&self, start: usize, end: usize) -> (usize, usize) {
        match &self.source {
            Some(source) => (source[start], source[end - 1] + 1),
            None => (start, end),
        }
    }
}

/// `text` as it is matched: see [`BackslashFree`].
fn without_backslashes(text: &str) -> String {
    text.replace('\\', "")
}

/// Length in bytes of the longest common prefix of `a` and `b` (a character
/// boundary of both).
fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = 0;
    for (x, y) in a.chars().zip(b.chars()) {
        tick(1);
        if x != y {
            break;
        }
        len += x.len_utf8();
    }
    len
}

/// Length in bytes of the longest common suffix of `a` and `b`.
fn common_suffix_len(a: &str, b: &str) -> usize {
    let mut len = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        len += x.len_utf8();
    }
    len
}

/// The byte ranges of `original` that the redactor hid in `redacted`: the
/// whole changed span (between the longest common prefix and suffix), and,
/// when several values were hidden, each one on its own (the text kept
/// between the [`REDACTED`] marks is matched in `original` in order).
fn hidden_ranges(original: &str, redacted: &str) -> Vec<(usize, usize)> {
    let prefix = common_prefix_len(original, redacted);
    let suffix = common_suffix_len(&original[prefix..], &redacted[prefix..]);
    let mut ranges = vec![(prefix, original.len() - suffix)];
    let kept: Vec<&str> = redacted.split(REDACTED).collect();
    let mut at = 0;
    for (index, pair) in kept.windows(2).enumerate() {
        let (before, after) = (pair[0], pair[1]);
        if !original[at..].starts_with(before) {
            break;
        }
        let start = at + before.len();
        let end = if index + 2 == kept.len() {
            match original.len().checked_sub(after.len()) {
                Some(end) if end >= start && original.ends_with(after) => end,
                _ => break,
            }
        } else if after.is_empty() {
            break;
        } else {
            match original[start..].find(after) {
                Some(offset) => start + offset,
                None => break,
            }
        };
        ranges.push((start, end));
        at = end;
    }
    ranges
}

/// Where the label of a hidden credential starts, if one stands right before
/// byte `piece_start` of `value`: a run of key characters (`password`,
/// `x-api-key`, `auth.token`) followed by at most four separator characters
/// (`=`, `:`, `=>`, quotes, spaces).
fn label_start(value: &str, piece_start: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut at = piece_start;
    let mut separators = 0;
    while at > 0
        && separators < 4
        && matches!(bytes[at - 1], b'=' | b':' | b'>' | b'"' | b'\'' | b' ')
    {
        at -= 1;
        separators += 1;
    }
    if separators == 0 {
        return None;
    }
    let key_end = at;
    while at > 0
        && (bytes[at - 1].is_ascii_alphanumeric() || matches!(bytes[at - 1], b'_' | b'-' | b'.'))
    {
        at -= 1;
    }
    (at < key_end).then_some(at)
}

/// Whether `c` separates two credentials inside a hidden piece (`user:password`,
/// `Bearer value`). A quote does not: it is part of a password, and splitting
/// there would teach one-letter parts that hide a diagnostic's ordinary words.
fn is_piece_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ':' | ',' | ';' | '@')
}

/// The spellings under which a slice may echo `text`: as given, on one line
/// (`neutralize_line`) and escaped (`escape_untrusted`, Python `repr` style).
fn spellings(text: &str) -> [String; 4] {
    [
        text.to_string(),
        neutralize_line(text).into_owned(),
        escape_untrusted(text, usize::MAX),
        escaped(text),
    ]
}

/// One learned credential piece: match without backslashes unless that view
/// would be empty (#5620), in which case retain the exact spelling. Hidden
/// wherever at least its first `min_bytes` bytes appear, as far as matching
/// continues.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Fragment {
    text: String,
    min_bytes: usize,
    /// An all-backslash piece has no normalized spelling. Match its original
    /// bytes instead, including overlapping occurrences in escaped echoes.
    exact: bool,
}

/// What replaces one matched span of a diagnostic.
#[derive(Debug, Clone, Copy)]
enum Replacement<'c> {
    /// A known value echoed whole: its redacted, bounded, single-line form.
    /// `redacted` tells whether that form already hides every credential.
    Shown { shown: &'c str, redacted: bool },
    /// A credential fragment, or a union of overlapping spans: [`REDACTED`].
    Hidden,
}

impl Replacement<'_> {
    /// A known value that hides no credential (a newline or overlong one).
    fn is_ordinary(self) -> bool {
        matches!(
            self,
            Self::Shown {
                redacted: false,
                ..
            }
        )
    }
}

/// The command-line values of one run, as the stderr sinks must show them
/// (#4639, #4941).
///
/// [`run`](crate::run) builds it from the arguments, and both the final
/// `error:` report and [`Output::diagnostic`](crate::Output::diagnostic) pass
/// their text through [`CallerText::diagnostic`]. Stdout does not: values are
/// redacted where stdout data is built ([`redacted_value`]). A value is
/// *known* when it holds a newline, is credential-shaped (the shared redactor
/// changes it) or is longer than [`MAX_PATH_CHARS`] characters. Ordinary
/// values are not known, so text that echoes them stays byte-identical.
///
/// This is a safety net behind the value-level rule, not a replacement for
/// it: a renderer that changes a value's spelling (a `repr` that escapes a
/// quote, #4979) must be handed [`shown_value`] instead.
#[derive(Debug, Default, Clone)]
pub(crate) struct CallerText {
    /// `(value, how it is shown, whether that hides a credential)`: a known
    /// value echoed verbatim in a diagnostic is replaced by its redacted,
    /// bounded, single-line rendering.
    shown: Vec<(String, String, bool)>,
    /// What the redactor hid in a credential-shaped value, in each spelling,
    /// whatever its length (#4980): the hidden piece alone and each part of it
    /// (matched whole, or from [`MIN_PARTIAL_CHARS`] characters on when cut) and
    /// the piece with the label in front of it (matched from the label and the
    /// first hidden character on).
    fragments: Vec<Fragment>,
    /// The fragments already learned, so each is added once.
    learned: HashSet<Fragment>,
}

impl CallerText {
    /// The known values of `args`: each argument and the value half of a
    /// `NAME=VALUE` argument. The flag word of `--flag=VALUE` is only escaped
    /// when it holds a newline (as before #4941); its value half is learned on
    /// its own.
    pub(crate) fn from_args(args: &[String]) -> Self {
        let mut caller = Self::default();
        for arg in args {
            if arg.starts_with("--") {
                if arg.contains('\n') {
                    caller
                        .shown
                        .push((arg.clone(), neutralize_line(arg).into_owned(), false));
                }
            } else {
                caller.learn(arg);
            }
            if let Some((_, value)) = arg.split_once('=') {
                caller.learn(value);
            }
        }
        caller
    }

    /// Only the #4639 newline rule, for a passthrough command (`asm exec`):
    /// its words are another program's arguments, which may hold secret
    /// *references* such as `{{resolve:secretsmanager:prod/db}}` that the
    /// shared redactor would take for a labelled credential, and the command
    /// redacts resolved values itself. Each value with a newline is escaped
    /// onto one line wherever a diagnostic echoes it; nothing is redacted or
    /// bounded.
    pub(crate) fn newlines_only(args: &[String]) -> Self {
        let mut caller = Self::default();
        for arg in args {
            let value = arg.split_once('=').map(|(_, value)| value);
            for text in std::iter::once(arg.as_str()).chain(value) {
                if text.contains('\n') {
                    caller.shown.push((
                        text.to_string(),
                        neutralize_line(text).into_owned(),
                        false,
                    ));
                }
            }
        }
        caller
    }

    fn learn(&mut self, value: &str) {
        let redacted = redact_text(value);
        let credential = redacted != value;
        if credential || value.contains('\n') || value.chars().count() > MAX_PATH_CHARS {
            let shown = neutralize_line(&bounded(&redacted, MAX_PATH_CHARS)).into_owned();
            self.shown.push((value.to_string(), shown, credential));
        }
        if !credential {
            return;
        }
        for (start, end) in hidden_ranges(value, &redacted) {
            let piece = &value[start..end];
            if piece.is_empty() {
                continue;
            }
            self.learn_piece(piece);
            // `user:password` hides as one piece, and either half can be
            // echoed on its own.
            for part in piece.split(is_piece_separator) {
                self.learn_piece(part);
            }
            if let Some(label) = label_start(value, start) {
                for (whole, label) in spellings(&value[label..end])
                    .into_iter()
                    .zip(spellings(&value[label..start]))
                {
                    let (whole, label) = (without_backslashes(&whole), without_backslashes(&label));
                    let min_bytes = whole
                        .strip_prefix(label.as_str())
                        .and_then(|rest| rest.chars().next())
                        .map_or(whole.len(), |first| label.len() + first.len_utf8());
                    self.add(whole, min_bytes, false);
                }
            }
        }
    }

    fn learn_piece(&mut self, piece: &str) {
        for spelling in spellings(piece) {
            let text = without_backslashes(&spelling);
            let exact = text.is_empty() && !spelling.is_empty();
            let text = if exact { spelling } else { text };
            let min_bytes = text
                .char_indices()
                .nth(MIN_PARTIAL_CHARS)
                .map_or(text.len(), |(at, _)| at);
            self.add(text, min_bytes, exact);
        }
    }

    fn add(&mut self, text: String, min_bytes: usize, exact: bool) {
        let fragment = Fragment {
            text,
            min_bytes,
            exact,
        };
        // A set, not a scan of the list: many credential arguments must not
        // cost quadratic work to learn (#5012).
        if fragment.min_bytes > 0 && self.learned.insert(fragment.clone()) {
            self.fragments.push(fragment);
        }
    }

    /// Upper bound, in matcher steps, of [`CallerText::spans`] on a text of
    /// `len` bytes: each known value is searched once (a linear substring
    /// search), each fragment is pre-checked by a linear search for its
    /// anchor and then matched by [`prefix_matches`] in at most
    /// `2 * (fragment + 1 + text)` steps, after one pass that removes the
    /// text's backslashes. Exact fragments use the original text, whose length
    /// is the same upper bound, under the same work and span limits (#5620).
    fn work_bound(&self, len: usize) -> usize {
        let known: usize = self
            .shown
            .iter()
            .map(|(value, _, _)| len.saturating_add(value.len()))
            .fold(0, usize::saturating_add);
        let fragments: usize = self
            .fragments
            .iter()
            .map(|fragment| {
                len.saturating_mul(3)
                    .saturating_add(fragment.text.len().saturating_mul(2))
                    .saturating_add(2)
            })
            .fold(0, usize::saturating_add);
        known.saturating_add(fragments).saturating_add(len)
    }

    /// Every span of `text` to replace, found in the original text before any
    /// replacement (#4978): each verbatim echo of a known value, and each
    /// place a credential fragment appears, whole or cut, backslashes aside.
    /// Touching or overlapping occurrences of one fragment are merged as they
    /// are found, in work linear in the text and the fragment (#5012). `None`
    /// past [`MAX_SPANS`].
    fn spans(&self, text: &str) -> Option<Vec<(usize, usize, Replacement<'_>)>> {
        let mut spans = Vec::new();
        for (value, shown, redacted) in &self.shown {
            tick(text.len() + value.len());
            for (at, _) in text.match_indices(value.as_str()) {
                let replacement = Replacement::Shown {
                    shown,
                    redacted: *redacted,
                };
                spans.push((at, at + value.len(), replacement));
            }
        }
        self.piece_spans(text, &mut spans).then_some(spans)
    }

    /// Adds where each fragment appears in `text`; false past [`MAX_SPANS`].
    fn piece_spans<'c>(
        &'c self,
        text: &str,
        spans: &mut Vec<(usize, usize, Replacement<'c>)>,
    ) -> bool {
        if self.fragments.is_empty() {
            return spans.len() <= MAX_SPANS;
        }
        let view = BackslashFree::new(text);
        for fragment in &self.fragments {
            let matched = if fragment.exact { text } else { &view.text };
            tick(matched.len());
            if !matched.contains(&fragment.text[..fragment.min_bytes]) {
                continue;
            }
            let mut current: Option<(usize, usize)> = None;
            let mut flush = |span: (usize, usize)| {
                let (start, end) = if fragment.exact {
                    span
                } else {
                    view.original(span.0, span.1)
                };
                spans.push((start, end, Replacement::Hidden));
            };
            prefix_matches(&fragment.text, matched, |at, len| {
                if len < fragment.min_bytes {
                    return;
                }
                let end = at + len;
                match current.as_mut() {
                    Some(span) if at <= span.1 => span.1 = span.1.max(end),
                    _ => {
                        if let Some(span) = current.replace((at, end)) {
                            flush(span);
                        }
                    }
                }
            });
            if let Some(span) = current {
                flush(span);
            }
            if spans.len() > MAX_SPANS {
                return false;
            }
        }
        spans.len() <= MAX_SPANS
    }

    /// `text` with `spans` replaced. A span inside a known value's rendering
    /// that already hides its credential is dropped; any other overlap is
    /// merged and hidden as one [`REDACTED`].
    fn splice<'t>(text: &'t str, mut spans: Vec<(usize, usize, Replacement<'_>)>) -> Cow<'t, str> {
        if spans.is_empty() {
            return Cow::Borrowed(text);
        }
        spans.sort_by_key(|&(start, end, _)| (start, Reverse(end)));
        let mut merged: Vec<(usize, usize, Replacement<'_>)> = Vec::new();
        for (start, end, replacement) in spans {
            match merged.last_mut() {
                Some(last) if start < last.1 => {
                    // A span inside one that already hides every credential,
                    // or an ordinary known value inside another, adds nothing.
                    let inside = end <= last.1;
                    let covered = !last.2.is_ordinary() || replacement.is_ordinary();
                    if !(inside && covered) {
                        last.1 = last.1.max(end);
                        last.2 = Replacement::Hidden;
                    }
                }
                _ => merged.push((start, end, replacement)),
            }
        }
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        for (start, end, replacement) in merged {
            out.push_str(&text[copied..start]);
            out.push_str(match replacement {
                Replacement::Shown { shown, .. } => shown,
                Replacement::Hidden => REDACTED,
            });
            copied = end;
        }
        out.push_str(&text[copied..]);
        Cow::Owned(out)
    }

    /// `text` with every span of [`CallerText::spans`] replaced. When matching
    /// could take more than [`MAX_MATCH_WORK`] steps the text is not scanned at
    /// all, and past [`MAX_SPANS`] matches nothing of it is kept: every line of
    /// it is hidden (fail closed, #5012).
    fn replace_spans<'t>(&self, text: &'t str) -> Cow<'t, str> {
        if self.work_bound(text.len()) > MAX_MATCH_WORK {
            return Cow::Owned(hidden_lines(text));
        }
        match self.spans(text) {
            Some(spans) => Self::splice(text, spans),
            None => Cow::Owned(hidden_lines(text)),
        }
    }

    /// `text`, a caller value about to be formatted, with each learned piece
    /// hidden. Unlike [`CallerText::replace_spans`] a known value is not
    /// rewritten: the redactor already handled it and stdout data is never cut.
    /// Past the work or span bound the value is one [`REDACTED`].
    fn hide_pieces<'t>(&self, text: &'t str) -> Cow<'t, str> {
        if self.fragments.is_empty() {
            return Cow::Borrowed(text);
        }
        if self.work_bound(text.len()) > MAX_MATCH_WORK {
            return Cow::Borrowed(REDACTED);
        }
        let mut spans = Vec::new();
        if self.piece_spans(text, &mut spans) {
            Self::splice(text, spans)
        } else {
            Cow::Borrowed(REDACTED)
        }
    }

    /// Makes the value helpers of this module hide what these arguments taught
    /// on this thread until the returned scope ends: [`crate::run`] enters it
    /// for the whole run.
    pub(crate) fn enter_run(&self) -> RunScope {
        let current = Rc::new(self.clone());
        RunScope(RUN_CALLER.with_borrow_mut(|slot| slot.replace(current)))
    }

    /// `text` with what the stderr net hides, for a message that is about to be
    /// put inside a structured document: no unsafe character is escaped here.
    pub(crate) fn message_value<'t>(&self, text: &'t str) -> Cow<'t, str> {
        self.replace_spans(text)
    }

    /// A diagnostic or error report made safe for stderr: each known value
    /// echoed verbatim is shown redacted, bounded to [`MAX_PATH_CHARS`]
    /// characters and on one line; any remaining credential fragment is
    /// hidden; then every unsafe character but the newline is escaped
    /// (`toolkit_core::neutralize_text`). Text that echoes no known value and
    /// holds no unsafe character is returned unchanged.
    pub(crate) fn diagnostic<'t>(&self, text: &'t str) -> Cow<'t, str> {
        let current = self.replace_spans(text);
        if current.chars().any(|c| c != '\n' && is_unsafe_char(c)) {
            Cow::Owned(neutralize_text(&current).into_owned())
        } else {
            current
        }
    }
}

/// The live secrets of one invocation, for a scrub that must not let any
/// piece of them out (x402 R-SCRUB, design D21; #5374, #5375).
///
/// It is [`CallerText`]'s fragment rule made total: a secret is recognized
/// from [`MIN_PARTIAL_CHARS`] bytes on, as a prefix (a cut) **and at every
/// other offset** (a partial or unaligned echo), ignoring ASCII case (a
/// lower-cased content type) and in each spelling a renderer may give it
/// ([`spellings`] plus any the caller adds, such as its JSON escape). Every
/// window of [`MIN_PARTIAL_CHARS`] bytes of every spelling is learned, so a
/// shown piece of at least that length always holds a learned window, at any
/// offset, and the matched spans cover it whole. Matching is one hash lookup
/// per text byte; a text whose work would pass [`MAX_MATCH_WORK`] is not
/// scanned and counts as a hit everywhere (fail closed).
#[derive(Debug, Default, Clone)]
pub(crate) struct KnownSecrets {
    windows: HashSet<[u8; MIN_PARTIAL_CHARS]>,
}

/// The ASCII-lower-cased window of `text` at `at`, if a whole window fits.
fn window_at(text: &[u8], at: usize) -> Option<[u8; MIN_PARTIAL_CHARS]> {
    let piece = text.get(at..at + MIN_PARTIAL_CHARS)?;
    let mut window = [0_u8; MIN_PARTIAL_CHARS];
    for (slot, byte) in window.iter_mut().zip(piece) {
        *slot = byte.to_ascii_lowercase();
    }
    Some(window)
}

impl KnownSecrets {
    /// Learn `secret` in every [`spellings`] form. A secret shorter than
    /// [`MIN_PARTIAL_CHARS`] bytes is not learned: it would match ordinary
    /// words.
    pub(crate) fn learn(&mut self, secret: &str) {
        if secret.len() < MIN_PARTIAL_CHARS {
            return;
        }
        for spelling in spellings(secret) {
            self.learn_spelling(&spelling);
        }
    }

    /// Learn one more spelling of a secret already learned (for example the
    /// JSON escape a document renderer gives it).
    pub(crate) fn learn_spelling(&mut self, spelling: &str) {
        let bytes = spelling.as_bytes();
        for at in 0..=bytes.len().saturating_sub(MIN_PARTIAL_CHARS) {
            if let Some(window) = window_at(bytes, at) {
                tick(1);
                self.windows.insert(window);
            }
        }
    }

    /// Whether nothing was learned.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// The merged byte spans of `text` that hold a learned piece, widened to
    /// character boundaries; `None` when matching would take more than
    /// `budget` steps.
    fn spans_within(&self, text: &str, budget: usize) -> Option<Vec<(usize, usize)>> {
        if self.windows.is_empty() {
            return Some(Vec::new());
        }
        if text.len().saturating_add(self.windows.len()) > budget {
            return None;
        }
        let bytes = text.as_bytes();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for at in 0..=bytes.len().saturating_sub(MIN_PARTIAL_CHARS) {
            tick(1);
            let Some(window) = window_at(bytes, at) else {
                break;
            };
            if !self.windows.contains(&window) {
                continue;
            }
            let end = at + MIN_PARTIAL_CHARS;
            match spans.last_mut() {
                Some(last) if at <= last.1 => last.1 = end,
                _ => spans.push((at, end)),
            }
        }
        for span in &mut spans {
            while !text.is_char_boundary(span.0) {
                span.0 -= 1;
            }
            while !text.is_char_boundary(span.1) {
                span.1 += 1;
            }
        }
        Some(spans)
    }

    /// `text` with every span holding a learned piece replaced by
    /// [`REDACTED`], and whether anything was replaced. Past
    /// [`MAX_MATCH_WORK`] the whole text is [`REDACTED`] (fail closed).
    pub(crate) fn redact<'t>(&self, text: &'t str) -> (Cow<'t, str>, bool) {
        self.redact_within(text, MAX_MATCH_WORK)
    }

    fn redact_within<'t>(&self, text: &'t str, budget: usize) -> (Cow<'t, str>, bool) {
        let Some(spans) = self.spans_within(text, budget) else {
            return (Cow::Borrowed(REDACTED), true);
        };
        if spans.is_empty() {
            return (Cow::Borrowed(text), false);
        }
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        for (start, end) in spans {
            out.push_str(&text[copied..start]);
            out.push_str(REDACTED);
            copied = end;
        }
        out.push_str(&text[copied..]);
        (Cow::Owned(out), true)
    }

    /// Whether `text` holds any learned piece. Past [`MAX_MATCH_WORK`] it
    /// counts as holding one (fail closed).
    pub(crate) fn found_in(&self, text: &str) -> bool {
        self.found_within(text, MAX_MATCH_WORK)
    }

    fn found_within(&self, text: &str, budget: usize) -> bool {
        self.spans_within(text, budget)
            .is_none_or(|spans| !spans.is_empty())
    }
}

/// The shortest known secret the final model-output scrub accepts (`OpenClaw`
/// R-OC-FINAL, design D-OC19): a shorter one would match ordinary words.
pub(crate) const FINAL_MIN_SECRET: usize = 8;

/// The shortest piece of a secret the final scrub recognizes when the whole
/// secret is not shown (D-OC19): 32 contiguous bytes of the secret itself,
/// so a public prefix (the `x402` header head, `0x`) is never a pattern.
pub(crate) const FINAL_MIN_FRAGMENT: usize = 32;

/// The final scrub's default work bound, the same as R-SCRUB's.
pub(crate) const FINAL_MATCH_WORK: usize = MAX_MATCH_WORK;

/// The known secrets of one `OpenClaw` paid-tool result, for the final
/// model-output scrub (R-OC-FINAL, #5160).
///
/// It is [`KnownSecrets`]' window rule with the D-OC19 thresholds: a secret
/// is found whole from [`FINAL_MIN_SECRET`] bytes, and in part only as
/// [`FINAL_MIN_FRAGMENT`] contiguous bytes of the secret, ignoring ASCII
/// case. Each text is searched as given and through one decoding layer of
/// each supported encoding ([`decoded_views`]): JSON string escapes, percent
/// escapes and standard or URL-safe base64 at each of the four alignments.
/// Every step is charged to a caller budget; past it the caller must fail
/// closed.
#[derive(Debug, Default, Clone)]
pub(crate) struct FinalSecrets {
    windows: HashSet<[u8; FINAL_MIN_FRAGMENT]>,
    short: Vec<Vec<u8>>,
}

/// One searchable form of a text: its bytes, and for each byte the span of
/// the source text it came from.
struct View {
    bytes: Vec<u8>,
    source: Vec<(usize, usize)>,
}

impl View {
    fn with_capacity(len: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(len),
            source: Vec::with_capacity(len),
        }
    }
    fn push(&mut self, byte: u8, from: usize, to: usize) {
        self.bytes.push(byte.to_ascii_lowercase());
        self.source.push((from, to));
    }
}

fn hex_value(byte: u8) -> Option<u32> {
    char::from(byte).to_digit(16)
}

/// `\uXXXX` at `at` (after the backslash at `at - 1` was seen), as a code unit.
fn json_unit(bytes: &[u8], at: usize) -> Option<u32> {
    let digits = bytes.get(at + 1..at + 5)?;
    if bytes.get(at) != Some(&b'u') {
        return None;
    }
    digits
        .iter()
        .try_fold(0_u32, |acc, &b| hex_value(b).map(|v| acc * 16 + v))
}

/// One layer of JSON string escapes undone (a lone surrogate stays as written).
fn json_view(text: &str) -> View {
    let bytes = text.as_bytes();
    let mut view = View::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\' {
            let simple = match bytes.get(at + 1) {
                Some(b'"') => Some(b'"'),
                Some(b'\\') => Some(b'\\'),
                Some(b'/') => Some(b'/'),
                Some(b'b') => Some(0x08),
                Some(b'f') => Some(0x0c),
                Some(b'n') => Some(b'\n'),
                Some(b'r') => Some(b'\r'),
                Some(b't') => Some(b'\t'),
                _ => None,
            };
            if let Some(byte) = simple {
                view.push(byte, at, at + 2);
                at += 2;
                continue;
            }
            if let Some(unit) = json_unit(bytes, at + 1) {
                let (scalar, end) = if (0xd800..0xdc00).contains(&unit) {
                    match bytes
                        .get(at + 6)
                        .filter(|b| **b == b'\\')
                        .and_then(|_| json_unit(bytes, at + 7))
                    {
                        Some(low) if (0xdc00..0xe000).contains(&low) => (
                            char::from_u32(0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00)),
                            at + 12,
                        ),
                        _ => (None, at + 6),
                    }
                } else {
                    (char::from_u32(unit), at + 6)
                };
                if let Some(scalar) = scalar {
                    let mut buffer = [0_u8; 4];
                    for byte in scalar.encode_utf8(&mut buffer).bytes() {
                        view.push(byte, at, end);
                    }
                    at = end;
                    continue;
                }
            }
        }
        view.push(bytes[at], at, at + 1);
        at += 1;
    }
    view
}

/// One layer of `%XX` escapes undone.
fn percent_view(text: &str) -> View {
    let bytes = text.as_bytes();
    let mut view = View::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if let (Some(high), Some(low)) = (
                bytes.get(at + 1).copied().and_then(hex_value),
                bytes.get(at + 2).copied().and_then(hex_value),
            ) {
                view.push(u8::try_from(high * 16 + low).unwrap_or(0), at, at + 3);
                at += 3;
                continue;
            }
        }
        view.push(bytes[at], at, at + 1);
        at += 1;
    }
    view
}

fn base64_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// One layer of base64 (standard or URL-safe) undone over every maximal run
/// of the alphabet, starting `offset` characters into each run. Runs are
/// joined by a 0xff byte, which no UTF-8 secret holds, so no match crosses
/// two runs. A byte's source is its whole quartet, `=` padding included.
fn base64_view(text: &str, offset: usize) -> View {
    let bytes = text.as_bytes();
    let mut view = View::with_capacity(bytes.len() / 4 * 3 + 1);
    let mut at = 0;
    while at < bytes.len() {
        if base64_value(bytes[at]).is_none() {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && base64_value(bytes[at]).is_some() {
            at += 1;
        }
        let mut end = at;
        while end < bytes.len() && bytes[end] == b'=' {
            end += 1;
        }
        let mut quartet = start + offset;
        while quartet + 2 <= at {
            let stop = (quartet + 4).min(at);
            let values: Vec<u32> = bytes[quartet..stop]
                .iter()
                .filter_map(|b| base64_value(*b))
                .collect();
            let source_end = if stop == at { end } else { stop };
            let word = values
                .iter()
                .chain(std::iter::repeat(&0))
                .take(4)
                .fold(0_u32, |acc, v| (acc << 6) | v);
            for index in 0..values.len() - 1 {
                let byte = u8::try_from((word >> (16 - 8 * index)) & 0xff).unwrap_or(0);
                view.push(byte, quartet, source_end);
            }
            quartet += 4;
        }
        view.bytes.push(0xff);
        view.source.push((end, end));
        at = end;
    }
    view
}

/// The literal text and every one-layer decoding of it (D-OC19).
fn decoded_views(text: &str) -> Vec<View> {
    let mut literal = View::with_capacity(text.len());
    for (at, byte) in text.bytes().enumerate() {
        literal.push(byte, at, at + 1);
    }
    let mut views = vec![literal, json_view(text), percent_view(text)];
    views.extend((0..4).map(|offset| base64_view(text, offset)));
    views
}

/// Takes `steps` from `budget`; false when it does not hold that many.
fn charge(budget: &mut usize, steps: usize) -> bool {
    match budget.checked_sub(steps) {
        Some(rest) => {
            *budget = rest;
            tick(steps);
            true
        }
        None => false,
    }
}

impl FinalSecrets {
    /// The matcher for `secrets`; `None` when one is shorter than
    /// [`FINAL_MIN_SECRET`] bytes (a registry it cannot honour).
    pub(crate) fn new(secrets: &[&str]) -> Option<Self> {
        let mut matcher = Self::default();
        for secret in secrets {
            let lower = secret.as_bytes().to_ascii_lowercase();
            if lower.len() < FINAL_MIN_SECRET {
                return None;
            }
            if lower.len() < FINAL_MIN_FRAGMENT {
                if !matcher.short.contains(&lower) {
                    matcher.short.push(lower);
                }
                continue;
            }
            for window in lower.windows(FINAL_MIN_FRAGMENT) {
                let mut key = [0_u8; FINAL_MIN_FRAGMENT];
                key.copy_from_slice(window);
                matcher.windows.insert(key);
            }
        }
        Some(matcher)
    }

    /// The merged byte spans of `text`, widened to character boundaries,
    /// whose literal or one-layer-decoded form holds a secret or a fragment;
    /// `None` when that would take more than `budget` steps (the budget is
    /// spent as the search goes).
    pub(crate) fn spans(&self, text: &str, budget: &mut usize) -> Option<Vec<(usize, usize)>> {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        if self.windows.is_empty() && self.short.is_empty() {
            return Some(spans);
        }
        if !charge(budget, text.len().saturating_mul(8)) {
            return None;
        }
        for view in decoded_views(text) {
            let bytes = &view.bytes;
            let mut hit = |from: usize, len: usize| {
                let start = view.source[from].0;
                let end = view.source[from + len - 1].1;
                spans.push((start, end));
            };
            if !self.windows.is_empty() {
                if !charge(budget, bytes.len()) {
                    return None;
                }
                for (at, window) in bytes.windows(FINAL_MIN_FRAGMENT).enumerate() {
                    if self.windows.contains(window) {
                        hit(at, FINAL_MIN_FRAGMENT);
                    }
                }
            }
            for secret in &self.short {
                if !charge(budget, bytes.len().saturating_mul(secret.len())) {
                    return None;
                }
                for (at, window) in bytes.windows(secret.len()).enumerate() {
                    if window == secret.as_slice() {
                        hit(at, secret.len());
                    }
                }
            }
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        for span in &mut merged {
            while !text.is_char_boundary(span.0) {
                span.0 -= 1;
            }
            while !text.is_char_boundary(span.1) {
                span.1 += 1;
            }
        }
        Some(merged)
    }

    /// Whether `text` holds a secret in any supported form; `None` past the
    /// budget.
    pub(crate) fn found_in(&self, text: &str, budget: &mut usize) -> Option<bool> {
        self.spans(text, budget).map(|spans| !spans.is_empty())
    }

    /// `text` with every span of [`FinalSecrets::spans`] replaced by
    /// [`REDACTED`], and whether anything was replaced; `None` past the
    /// budget.
    pub(crate) fn redact(&self, text: &str, budget: &mut usize) -> Option<(String, bool)> {
        let spans = self.spans(text, budget)?;
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        for &(start, end) in &spans {
            out.push_str(&text[copied..start]);
            out.push_str(REDACTED);
            copied = end;
        }
        out.push_str(&text[copied..]);
        Some((out, !spans.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_unchanged() {
        assert_eq!(
            safe_text("bedrock-migration/ü 日本"),
            "bedrock-migration/ü 日本"
        );
        assert_eq!(safe_text(""), "");
    }

    #[test]
    fn control_and_format_characters_are_escaped() {
        assert_eq!(
            safe_text("a\nb\r\u{1b}[31m\u{7}"),
            "a\\x0ab\\x0d\\x1b[31m\\x07"
        );
        assert_eq!(
            safe_text("x\u{202e}y\u{2028}z\u{7f}"),
            "x\\u202ey\\u2028z\\x7f"
        );
        assert_eq!(safe_text("\u{e0001}"), "\\U000e0001");
    }

    #[test]
    fn long_text_is_bounded_in_characters() {
        let long = "é".repeat(MAX_SHOWN_CHARS + 30);
        let shown = safe_text(&long);
        assert_eq!(shown.chars().count(), MAX_SHOWN_CHARS + 3);
        assert!(shown.ends_with("..."));
        let exact = "a".repeat(MAX_SHOWN_CHARS);
        assert_eq!(safe_text(&exact), exact);
    }

    #[test]
    fn paths_within_the_bound_are_escaped_but_not_shortened() {
        // Longer than the text bound, within the path bound (#4941 replaced
        // the unbounded path rule with the 200-character one).
        let long = format!("{}/report\n.md", "d".repeat(MAX_SHOWN_CHARS + 50));
        let shown = safe_path(&long);
        assert!(shown.ends_with("/report\\x0a.md"));
        assert_eq!(shown.chars().count(), long.chars().count() + 3);
    }

    #[test]
    fn a_silent_prefix_is_cut_after_redaction_without_a_mark() {
        assert_eq!(safe_prefix("failover now", 8), "failover");
        assert_eq!(safe_prefix("a\nb", 5), "a\\x0ab");
        let shown = safe_prefix(&userinfo(150), 120);
        assert!(shown.starts_with("https://[REDACTED]@"), "{shown}");
        assert_no_secret(&shown);
    }

    fn caller(args: &[&str]) -> CallerText {
        CallerText::from_args(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn ordinary_arguments_leave_every_sink_unchanged() {
        let known = caller(&["neptune", "seed", "--recorded", "/tmp/r.json", "--x=y"]);
        for text in ["error: /tmp/r.json: not found", "plain\nlines", "--x=y"] {
            assert!(matches!(known.diagnostic(text), Cow::Borrowed(_)), "{text}");
        }
        // Unsafe characters are still escaped, newlines kept (#4639).
        assert_eq!(known.diagnostic("a\u{1b}b\nc"), "a\\u001Bb\nc");
    }

    #[test]
    fn a_newline_value_is_escaped_as_before() {
        let known = caller(&["x", "a\nerror: forged", "--f=b\nc"]);
        assert_eq!(
            known.diagnostic("error: a\nerror: forged here\nnext"),
            "error: a\\nerror: forged here\nnext"
        );
        assert_eq!(known.diagnostic("bad --f=b\nc"), "bad --f=b\\nc");
        assert_eq!(known.diagnostic("bad b\nc"), "bad b\\nc");
    }

    #[test]
    fn a_credential_value_echoed_raw_or_cut_is_hidden_in_a_diagnostic() {
        let url = userinfo(0);
        let past = userinfo(250);
        let known = caller(&["seed", "--recorded", &url, &format!("--out={past}")]);
        let message = format!("error: cannot read {url}: missing");
        let shown = known.diagnostic(&message);
        assert_eq!(
            shown,
            "error: cannot read https://[REDACTED]@example.invalid/x.json: missing"
        );
        // A slice that cut the value before redacting it.
        let cut: String = past.chars().take(120).collect();
        for text in [
            format!("error: {cut}..."),
            format!("wrote {past}"),
            format!("normalized https:/{}", &past["https://".len()..]),
        ] {
            assert_no_secret(&known.diagnostic(&text));
        }
        // The whole attached argument (an unknown-flag report).
        assert_no_secret(&known.diagnostic(&format!("unknown --out={past}")));
        // An overlong ordinary value is bounded only where it is echoed whole.
        let long = format!("/{}", "d".repeat(MAX_PATH_CHARS + 20));
        let known = caller(&["x", &long]);
        assert_eq!(
            known.diagnostic(&format!("error: {long}")),
            format!("error: {}...", &long[..MAX_PATH_CHARS])
        );
    }

    #[test]
    fn a_passthrough_command_keeps_secret_references_and_only_escapes_newlines() {
        let args: Vec<String> = [
            "asm",
            "exec",
            "sh",
            "{{resolve:secretsmanager:prod/db}}",
            "a\nb",
        ]
        .map(String::from)
        .to_vec();
        let known = CallerText::newlines_only(&args);
        let failure = "asm-exec: ERROR: Failed to resolve: prod/db";
        assert!(matches!(known.diagnostic(failure), Cow::Borrowed(_)));
        assert_eq!(known.diagnostic("x a\nb"), "x a\\nb");
    }

    #[test]
    fn each_hidden_value_of_an_argument_is_a_fragment() {
        let value = "a/password=FAKE_SECRET_ONE b/token=FAKE_SECRET_TWO/c";
        let redacted = toolkit_aws::redact_text(value);
        let ranges = hidden_ranges(value, &redacted);
        assert!(ranges.len() > 1, "{redacted}");
        let known = caller(&["x", value]);
        assert_no_secret(&known.diagnostic("only FAKE_SECRET_TWO here"));
        assert_no_secret(&known.diagnostic("only FAKE_SECRET_ONE here"));
    }

    /// Every 8-character window of `piece` that `shown` still contains.
    fn surviving_windows(shown: &str, piece: &str) -> Vec<String> {
        let chars: Vec<char> = piece.chars().collect();
        chars
            .windows(8)
            .map(|window| window.iter().collect::<String>())
            .filter(|window| shown.contains(window.as_str()))
            .collect()
    }

    /// #4978 as a property: distinct credentials that share prefixes of
    /// several lengths, under different labels, in every order of three, each
    /// echoed whole, alone, cut and with its label, leave no 8-character
    /// window of any of them in a diagnostic.
    #[test]
    fn overlapping_credentials_are_all_hidden_whatever_their_order() {
        // Each tail past the shared prefix is longer than the window, so a
        // tail left behind by a consumed prefix is caught.
        let pieces = [
            "SHAREDPFX_ALPHA_LONGER_FIRSTTAIL1",
            "SHAREDPFX_ALPHA_SECONDTAIL2",
            "SHAREDPFX_THIRDTAIL3",
            "SHAREDP_FOURTHTAIL4",
            "SHAREDPFX_ALPHA_LONGER_FIFTHTAIL",
        ];
        let labels = ["password=", "token=", "secret=", "api_key=", "x/pwd="];
        let mut checked = 0;
        for a in 0..pieces.len() {
            for b in 0..pieces.len() {
                for c in 0..pieces.len() {
                    if a == b || b == c || a == c {
                        continue;
                    }
                    let chosen = [a, b, c];
                    let args: Vec<String> = std::iter::once("x".to_string())
                        .chain(
                            chosen
                                .iter()
                                .map(|&index| format!("{}{}", labels[index], pieces[index])),
                        )
                        .collect();
                    let known = CallerText::from_args(&args);
                    let mut text = String::from("error:");
                    for &index in chosen.iter().rev() {
                        let piece = pieces[index];
                        let arg = format!("{}{}", labels[index], piece);
                        let cut: String = piece.chars().take(9).collect();
                        let nearly: String =
                            piece.chars().take(piece.chars().count() - 2).collect();
                        let arg_cut: String = arg.chars().take(labels[index].len() + 10).collect();
                        for echo in [arg.as_str(), piece, &cut, &nearly, &arg_cut] {
                            text.push_str(" <");
                            text.push_str(echo);
                            text.push_str("...>");
                        }
                    }
                    let shown = known.diagnostic(&text);
                    for &index in &chosen {
                        let left = surviving_windows(&shown, pieces[index]);
                        assert!(
                            left.is_empty(),
                            "{left:?} of {:?} survived for {args:?}:\n{shown}",
                            pieces[index]
                        );
                    }
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 60);
    }

    /// #4980 at the stderr sink: a complete labelled credential is hidden
    /// whatever its length, whole or with only its first character echoed.
    #[test]
    fn a_short_labelled_credential_is_hidden_in_a_diagnostic() {
        for value in ["password=abc", "token=ab", "pwd=x", "a/secret=Qz"] {
            let known = caller(&["x", value]);
            let message = format!("cannot connect to {value}:6379");
            let shown = known.diagnostic(&message);
            assert!(!shown.contains(value), "{shown}");
            assert!(shown.contains(REDACTED), "{shown}");
        }
    }

    /// `https://user:FAKE_SECRET` + `pad` x characters + `@example.invalid/x.json`.
    fn userinfo(pad: usize) -> String {
        format!(
            "https://user:FAKE_SECRET{}@example.invalid/x.json",
            "x".repeat(pad)
        )
    }

    fn assert_no_secret(shown: &str) {
        assert!(
            !shown.contains("FAKE_SECRE"),
            "credential leaked: {shown:?}"
        );
        assert!(!shown.contains("user:"), "user name leaked: {shown:?}");
    }

    #[test]
    fn a_caller_path_is_redacted_before_it_is_shown() {
        let shown = safe_path(&userinfo(0));
        assert_eq!(shown, "https://[REDACTED]@example.invalid/x.json");
        let shown = safe_path("/data/password=FAKE_SECRET/x.json");
        assert!(shown.starts_with("/data/password=[REDACTED]"), "{shown}");
        assert_no_secret(&shown);
    }

    #[test]
    fn a_caller_path_is_redacted_in_full_before_it_is_bounded() {
        // The `@host` lies past the bound: cutting first would hide it from
        // the userinfo recognizer and print the password.
        for pad in [150, 190, 250, 5000] {
            let shown = safe_path(&userinfo(pad));
            assert_no_secret(&shown);
        }
        let text_shown = safe_text(&userinfo(150));
        assert_no_secret(&text_shown);
        // An access-key id that straddles the 200-character bound.
        let key = format!("/{}/AKIAQQQQQQQQQQQQQQQQ/x.json", "d".repeat(190));
        assert!(!safe_path(&key).contains("AKIAQQQQ"), "{}", safe_path(&key));
    }

    #[test]
    fn a_long_caller_path_is_bounded_on_a_character_boundary_then_escaped() {
        let long = format!("/{}", "d".repeat(MAX_PATH_CHARS + 99));
        let shown = safe_path(&long);
        assert_eq!(shown, format!("{}...", &long[..MAX_PATH_CHARS]));
        let wide = "é".repeat(MAX_PATH_CHARS + 1);
        assert_eq!(
            safe_path(&wide),
            format!("{}...", "é".repeat(MAX_PATH_CHARS))
        );
        // The bound counts characters of the redacted text, before escaping.
        let control = format!("{}\nrest", "a".repeat(MAX_PATH_CHARS - 1));
        assert_eq!(
            safe_path(&control),
            format!("{}\\x0a...", "a".repeat(MAX_PATH_CHARS - 1))
        );
        // A 300-character path ending in a credential.
        let ending = format!("/{}/api_key=FAKE_SECRET", "d".repeat(279));
        assert_eq!(ending.chars().count(), 300);
        assert_no_secret(&safe_path(&ending));
    }

    #[test]
    fn an_ordinary_path_up_to_the_bound_is_unchanged() {
        let exact = format!("/{}", "p".repeat(MAX_PATH_CHARS - 1));
        assert_eq!(safe_path(&exact), exact);
        assert_eq!(
            safe_path("./out/wa_review_results.json"),
            "./out/wa_review_results.json"
        );
        assert_eq!(safe_path("dir/ü 日本/x.md"), "dir/ü 日本/x.md");
    }

    #[test]
    fn a_value_is_redacted_before_a_renderer_quotes_it() {
        // #4979: the whole value is redacted before `repr` escapes its quote.
        let value = "password=ab'c\"FAKE_SECRET";
        assert_eq!(shown_value(value), "password=[REDACTED]");
        assert_eq!(redacted_value(value), "password=[REDACTED]");
        // #4980: a labelled credential of any length.
        for short in ["password=abc", "token=ab", "pwd=x"] {
            assert!(shown_value(short).ends_with("=[REDACTED]"), "{short}");
        }
        // Ordinary values are returned as given, quotes and all.
        for ordinary in ["ab'c\"d", "mysql", "6379", "", "us-east-1"] {
            assert!(matches!(shown_value(ordinary), Cow::Borrowed(_)));
            assert!(matches!(redacted_value(ordinary), Cow::Borrowed(_)));
        }
        // Only the display form is bounded; stdout data is never cut.
        let long = format!("arn:aws:sns:us-east-1:123456789012:{}", "t".repeat(256));
        assert_eq!(redacted_value(&long), long.as_str());
        assert_eq!(
            shown_value(&long),
            format!("{}...", &long[..MAX_PATH_CHARS])
        );
    }

    #[test]
    fn a_label_is_found_only_in_front_of_a_hidden_piece() {
        assert_eq!(label_start("password=abc", 9), Some(0));
        assert_eq!(label_start("a/x-api-key: abc", 13), Some(2));
        assert_eq!(label_start("https://user:pw@host", 8), None);
        assert_eq!(label_start("=abc", 1), None);
    }

    /// A non-credential value with a newline that contains a credential
    /// fragment is hidden whole, not shown with the fragment inside.
    #[test]
    fn a_fragment_inside_an_ordinary_known_value_is_not_shown() {
        let known = caller(&["x", "password=FRAGMENT_SECRET_1", "FRAGMENT_SECRET_1\nnext"]);
        let shown = known.diagnostic("error: FRAGMENT_SECRET_1\nnext");
        assert!(!shown.contains("FRAGMENT"), "{shown}");
        assert!(!shown.contains('\n'), "{shown}");
    }

    /// Matcher steps taken by `known.diagnostic(text)`, and its result.
    fn counted(known: &CallerText, text: &str) -> (String, usize) {
        WORK.with(|work| work.set(0));
        let shown = known.diagnostic(text).into_owned();
        (shown, WORK.with(std::cell::Cell::get))
    }

    /// #5012: a credential of one repeated character (or a short repeated
    /// pattern) taught by one occurrence of a flag and echoed whole by a
    /// parser error for another occurrence overlaps itself at every position.
    /// Matching it takes work linear in the text and the learned fragments,
    /// counted in matcher steps, not timed, and nothing of it is shown.
    #[test]
    fn a_self_overlapping_credential_is_matched_in_linear_work() {
        for n in [2_000_usize, 20_000] {
            for secret in ["a".repeat(n), "ab".repeat(n / 2), "aab".repeat(n / 3)] {
                // The msk size route of the finding: the first value is a
                // labelled credential, the last (ordinary) one is parsed and
                // quoted in full by the parser error.
                let first = format!("password={secret}");
                let known = caller(&[
                    "msk",
                    "size",
                    "--discount-pct",
                    &first,
                    "--discount-pct",
                    &secret,
                ]);
                let message = format!(
                    "error: argument --discount-pct: invalid float value: '{secret}'\n\
                     usage: toolkit msk size [...]\n"
                );
                let (shown, work) = counted(&known, &message);
                let window = &secret[..8];
                assert!(!shown.contains(window), "{n}: {shown:.300}");
                // Each learned fragment and known value may be compared with
                // the text a small constant number of times per byte.
                let learned: usize = known
                    .fragments
                    .iter()
                    .map(|fragment| fragment.text.len())
                    .chain(known.shown.iter().map(|(value, _, _)| value.len()))
                    .sum();
                let entries = known.fragments.len() + known.shown.len();
                let bound = 4 * (learned + entries * message.len());
                assert!(
                    work <= bound,
                    "{n}: {work} matcher steps exceed the linear bound {bound}"
                );
            }
        }
    }

    /// #5012: when the learned values are so many and the text so long that
    /// matching them could exceed the work budget, the net does not scan at
    /// all and fails closed: every line of the diagnostic is hidden.
    #[test]
    fn a_diagnostic_past_the_work_budget_hides_every_line() {
        let values: Vec<String> = (0..64)
            .map(|index| format!("password=BUDGET_SECRET_{index:03}_TAIL"))
            .collect();
        let mut args = vec!["x".to_string()];
        args.extend(values.iter().cloned());
        let known = CallerText::from_args(&args);
        let mut message = format!("error: {}\n", values[7]);
        message.push_str(&"x".repeat(300_000));
        message.push_str("\nlast line BUDGET_SECRET_012_TAIL\n");
        let (shown, work) = counted(&known, &message);
        assert_eq!(shown, format!("{REDACTED}\n{REDACTED}\n{REDACTED}\n"));
        assert!(work <= 1 << 26, "{work} matcher steps");
        // An ordinary diagnostic with the same learned values is matched.
        let (shown, _) = counted(&known, "error: cannot read BUDGET_SECRET_012_TAIL\n");
        assert_eq!(shown, format!("error: cannot read {REDACTED}\n"));
    }

    /// #4979, the net behind the value rule: a Python-style renderer given the
    /// raw value inserts backslashes that change a credential's spelling. Any
    /// backslash insertion is still the credential, at any escape depth.
    #[test]
    fn a_backslash_escape_of_a_credential_is_hidden_in_a_diagnostic() {
        let value = "password=ab'c\"FAKE_SECRET";
        let bare = "ab'c\"FAKE_SECRET";
        let known = caller(&["x", value]);
        for text in [
            // Python `repr` of the whole value and of the bare piece.
            "invalid float value: 'password=ab\\'c\"FAKE_SECRET'".to_string(),
            "invalid float value: 'ab\\'c\"FAKE_SECRET'".to_string(),
            // JSON and Rust `Debug` escape the double quote instead.
            "invalid: \"ab'c\\\"FAKE_SECRET\"".to_string(),
            // Both quotes, and a second layer of escaping.
            "invalid: 'ab\\'c\\\"FAKE_SECRET'".to_string(),
            "invalid: 'ab\\\\\\'c\\\\\\\"FAKE_SECRET'".to_string(),
            // A cut echo after the anchor.
            "invalid: 'ab\\'c\"FAKE_...'".to_string(),
            format!("raw {bare}"),
        ] {
            let shown = known.diagnostic(&text);
            for part in ["FAKE_", "ab\\'c", "ab'c"] {
                assert!(!shown.contains(part), "{part} of {text} in {shown}");
            }
            assert!(shown.contains(REDACTED), "{text} -> {shown}");
        }
        // A credential holding a backslash, echoed doubled by `repr`.
        let known = caller(&["x", "password=ab\\cFAKE_SECRET"]);
        let shown = known.diagnostic("bad 'ab\\\\cFAKE_SECRET'");
        assert!(!shown.contains("FAKE_"), "{shown}");
        // Backslashes around ordinary text stay exactly as they were.
        assert_eq!(known.diagnostic("C:\\dir\\file"), "C:\\dir\\file");
        assert_eq!(
            caller(&["x", "ab'c\"d"]).diagnostic("bad 'ab\\'c\"d'"),
            "bad 'ab\\'c\"d'"
        );
    }

    /// #4980: a piece of one, two or three characters is a credential too when
    /// it is echoed without its label. The mask is a plain substring match: it
    /// may hide an ordinary letter of the diagnostic of a run that passed such
    /// a credential, and it never leaves the piece visible.
    #[test]
    fn a_short_piece_is_hidden_without_its_label() {
        for (value, piece) in [
            ("password=Q", "Q"),
            ("token=Qz", "Qz"),
            ("password=Qz9", "Qz9"),
            ("https://Qz9@example.invalid/x", "Qz9"),
        ] {
            let known = caller(&["x", value]);
            for text in [
                format!("invalid value: '{piece}'"),
                format!("cannot connect to {piece}:6379"),
                format!("glued x{piece}y"),
            ] {
                let shown = known.diagnostic(&text);
                assert!(!shown.contains(piece), "{value}: {shown}");
                assert!(shown.contains(REDACTED), "{value}: {shown}");
            }
        }
        // The same words are untouched when no credential holds them.
        let known = caller(&["x", "password=Qz9"]);
        assert_eq!(known.diagnostic("plain words"), "plain words");
    }

    /// A long credential with a quote in it is one credential: its letters
    /// around the quote are not learned on their own, so the diagnostic's
    /// ordinary words survive.
    #[test]
    fn a_quote_does_not_split_a_credential() {
        let known = caller(&["x", "password=ab'c\"FAKE_SECRET"]);
        assert_eq!(known.diagnostic("invalid choice: x"), "invalid choice: x");
        let known = caller(&["x", "https://user:FAKE_SECRET@h.invalid/x"]);
        assert_eq!(known.diagnostic("invalid choice: x"), "invalid choice: x");
    }

    /// Userinfo hides `user:password` as one piece; each half is a credential
    /// when echoed on its own, whatever its length.
    #[test]
    fn each_part_of_a_hidden_piece_is_hidden_on_its_own() {
        for (value, parts) in [
            (
                "https://user:FAKE_SECRET@h.invalid/x",
                &["user", "FAKE_SECRET"][..],
            ),
            ("https://u:p@h.invalid/x", &["u:p"][..]),
            ("authorization: Bearer FAKE_SECRET", &["FAKE_SECRET"][..]),
        ] {
            let known = caller(&["x", value]);
            for part in parts {
                let text = format!("invalid value: '{part}'");
                let shown = known.diagnostic(&text);
                assert!(!shown.contains(part), "{value}: {shown}");
            }
        }
    }

    /// The schema-aware path: a value helper masks what another argument of
    /// the run taught, in caller text only, while its scope lasts. Stdout data
    /// is built from these helpers, so a typed field is never rewritten.
    #[test]
    fn value_helpers_hide_what_another_argument_taught() {
        let known = caller(&["--host", "token=FAKE_SECRET", "--host", "FAKE_SECRET"]);
        assert_eq!(redacted_value("FAKE_SECRET"), "FAKE_SECRET");
        {
            let _scope = known.enter_run();
            assert_eq!(redacted_value("FAKE_SECRET"), REDACTED);
            assert_eq!(shown_value("FAKE_SECRET"), REDACTED);
            assert_eq!(safe_text("FAKE_SECRET"), REDACTED);
            assert_eq!(safe_path("/x/FAKE_SECRET"), format!("/x/{REDACTED}"));
            assert_eq!(safe_prefix("FAKE_SECRET", 40), REDACTED);
            // What the redactor keeps stays, and an ordinary value is borrowed.
            assert_eq!(redacted_value("token=FAKE_SECRET"), "token=[REDACTED]");
            assert!(matches!(redacted_value("us-east-1"), Cow::Borrowed(_)));
            // Only caller values change: a long ordinary one is never cut.
            let long = "t".repeat(MAX_PATH_CHARS * 2);
            assert_eq!(redacted_value(&long), long.as_str());
            // A nested scope (a passthrough command) replaces the outer one.
            let passthrough = CallerText::default();
            {
                let _inner = passthrough.enter_run();
                assert_eq!(redacted_value("FAKE_SECRET"), "FAKE_SECRET");
            }
            assert_eq!(redacted_value("FAKE_SECRET"), REDACTED);
        }
        assert_eq!(redacted_value("FAKE_SECRET"), "FAKE_SECRET");
    }

    /// A value mask past the work budget fails closed, like the stderr net.
    #[test]
    fn a_value_past_the_work_budget_is_hidden() {
        let values: Vec<String> = (0..64)
            .map(|index| format!("password=BUDGET_SECRET_{index:03}_TAIL"))
            .collect();
        let known = CallerText::from_args(&values);
        let _scope = known.enter_run();
        // Below the redactor's own 256 KiB cap, so only the work budget hides it.
        let long = "x".repeat(250_000);
        assert_eq!(redacted_value(&long), REDACTED);
    }

    /// #5012 at its reachable size, in both argument orders: N = 60000, one
    /// repeated character, where the rescan took about 1.8 billion comparisons.
    #[test]
    fn the_reported_repeated_character_input_stays_linear() {
        let secret = "a".repeat(60_000);
        let first = format!("password={secret}");
        for (one, two) in [
            (first.as_str(), secret.as_str()),
            (secret.as_str(), first.as_str()),
        ] {
            let known = caller(&["msk", "size", "--discount-pct", one, "--discount-pct", two]);
            let message = format!("error: invalid discount percentage '{secret}'; a number\n");
            let (shown, work) = counted(&known, &message);
            assert!(!shown.contains("aaaaaaaa"), "{shown:.200}");
            assert!(work < 2_000_000, "{work} matcher steps");
        }
    }

    /// Hostile text for the backslash-insensitive view: a run of backslashes,
    /// a credential that is mostly backslashes, and backslashes between every
    /// character of a self-overlapping echo. Work stays linear and nothing of
    /// the credential is shown.
    #[test]
    fn backslashes_do_not_make_matching_quadratic() {
        for n in [4_000_usize, 40_000] {
            let secret = "ab".repeat(n / 2);
            let known = caller(&[
                "x",
                &format!("password={secret}"),
                &format!("token=\\\\{secret}"),
            ]);
            for message in [
                format!("{}{secret}", "\\".repeat(n)),
                secret.chars().flat_map(|c| ['\\', c]).collect::<String>(),
                "\\".repeat(n),
            ] {
                let (shown, work) = counted(&known, &message);
                assert!(!shown.contains("abababab"), "{shown:.200}");
                let learned: usize = known.fragments.iter().map(|f| f.text.len()).sum();
                let bound =
                    4 * (learned + (known.fragments.len() + known.shown.len() + 1) * message.len());
                assert!(work <= bound, "{n}: {work} steps exceed {bound}");
            }
        }
    }

    /// #5620: exact matching survives an empty normalized view, including
    /// renderers that double backslashes repeatedly. Separator-only pieces
    /// and mixed pieces must also retain their whole spelling.
    #[test]
    fn empty_normalized_credentials_and_separator_pieces_are_not_lost() {
        for secret in [
            "\\",
            "\\\\",
            "\\\\\\",
            "\\\\\\\\",
            "\\\\\\\\\\\\\\\\\\",
            ":",
            ",",
            ";",
            "@",
            "\\:\\",
            "\\Q\\",
        ] {
            // Comma/semicolon terminate an unquoted labelled value in the
            // shared redactor's grammar; quote them to supply a credential.
            let labelled = if matches!(secret, "," | ";") {
                format!("password=\"{secret}\"")
            } else {
                format!("password={secret}")
            };
            let known = caller(&["x", &labelled]);
            for depth in 0..5 {
                let echo = secret.replace('\\', &"\\".repeat(1 << depth));
                let message = format!("invalid '{echo}'");
                let shown = known.diagnostic(&message);
                assert!(
                    !shown.contains(&echo),
                    "{secret:?} at depth {depth}: {shown}"
                );
                let _scope = known.enter_run();
                assert!(!redacted_value(&echo).contains(&echo), "value {secret:?}");
            }
        }
    }

    #[test]
    fn exact_backslash_matching_is_linear_and_budgeted() {
        let secret = "\\".repeat(60_000);
        let known = caller(&["x", &format!("password={secret}")]);
        let echo = "\\".repeat(120_000);
        let (shown, work) = counted(&known, &echo);
        assert_eq!(shown, REDACTED);
        assert!(work < 2_000_000, "{work} matcher steps");
        assert!(work <= known.work_bound(echo.len()));
        let many: Vec<String> = (1..=128)
            .map(|n| format!("password={}", "\\".repeat(n)))
            .collect();
        let known = CallerText::from_args(&many);
        let long = "x".repeat(250_000);
        assert!(known.work_bound(long.len()) > MAX_MATCH_WORK);
        assert_eq!(known.diagnostic(&long), REDACTED);
        assert_eq!(known.hide_pieces(&long), REDACTED);
    }

    /// Short pieces match everywhere, so the spans of one diagnostic could
    /// grow with the text times the pieces: past [`MAX_SPANS`] the diagnostic
    /// is hidden instead of retaining them. Touching matches of one piece are
    /// one span.
    #[test]
    fn retained_spans_are_bounded() {
        let known = caller(&["x", "pwd=a", "pwd=b", "pwd=c", "pwd=d", "pwd=e"]);
        let run = "a".repeat(1_000);
        let spans = known.spans(&run).expect("within the bound");
        assert_eq!(spans.len(), 1);
        let text = "abcde.".repeat(MAX_SPANS / 5 + 1);
        assert_eq!(known.diagnostic(&text), REDACTED);
    }
    #[test]
    fn quoted_text_escapes_the_quote() {
        assert_eq!(safe_quoted("it's"), "'it\\'s'");
        assert_eq!(safe_quoted("a\nb"), "'a\\x0ab'");
    }

    /// A base64-shaped secret like an x402 proof header.
    const PROOF: &str = "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0ZWQiOnsic2NoZW1lIjoiZXhhY3Qi";

    fn learned(secrets: &[&str]) -> KnownSecrets {
        let mut known = KnownSecrets::default();
        for secret in secrets {
            known.learn(secret);
        }
        known
    }

    /// R-SCRUB (#5374, #5375): every piece of 8..=40 bytes at every offset,
    /// as given, upper-cased and lower-cased, is found and replaced whole
    /// with the text around it kept; a 7-byte piece is ordinary text.
    #[test]
    fn known_secrets_find_every_piece_at_every_offset() {
        let known = learned(&[PROOF]);
        for at in 0..PROOF.len() {
            for len in 7..=40 {
                let Some(piece) = PROOF.get(at..at + len) else {
                    continue;
                };
                for spelling in [
                    piece.to_owned(),
                    piece.to_ascii_uppercase(),
                    piece.to_ascii_lowercase(),
                ] {
                    let text = format!("<p>{spelling}</p>");
                    let (shown, redacted) = known.redact(&text);
                    if len < 8 {
                        assert!(!known.found_in(&text), "{at}+{len}");
                        assert_eq!((shown.as_ref(), redacted), (text.as_str(), false));
                    } else {
                        assert!(known.found_in(&text), "{at}+{len}");
                        assert_eq!(shown, format!("<p>{REDACTED}</p>"), "{at}+{len}");
                        assert!(redacted);
                    }
                }
            }
        }
    }

    #[test]
    fn known_secrets_find_whole_cut_and_repeated_secrets() {
        let known = learned(&[PROOF, "0xsig0001c4293dfd364640b9"]);
        let cut = &PROOF[..PROOF.len() - 3];
        let text = format!("a {PROOF} b {cut} c 0XSIG0001C4 d {}", &PROOF[20..]);
        let (shown, redacted) = known.redact(&text);
        assert!(redacted);
        assert_eq!(
            shown,
            format!("a {REDACTED} b {REDACTED} c {REDACTED} d {REDACTED}")
        );
        // Two secrets that touch become one span.
        let touching = format!("x{}{}y", &PROOF[..9], "0xsig0001");
        let (shown, _) = known.redact(&touching);
        assert_eq!(shown, format!("x{REDACTED}y"));
        // Ordinary text and a short secret are left alone.
        let mut short = KnownSecrets::default();
        short.learn("ps-1");
        assert!(short.is_empty());
        assert_eq!(known.redact("caf\u{e9} ok").0, "caf\u{e9} ok");
    }

    /// A secret that a renderer escapes is found in the escaped spelling,
    /// and a span next to multi-byte text stays on character boundaries.
    #[test]
    fn known_secrets_find_escaped_spellings() {
        let secret = "line-one\nline-two\u{1b}[31mSECRET-tail";
        let mut known = learned(&[secret]);
        known.learn_spelling("line-one\\nline-two\\u001b[31mSECRET-tail");
        for shown in [
            escaped(secret),
            escape_untrusted(secret, usize::MAX),
            neutralize_line(secret).into_owned(),
            "line-one\\nline-two\\u001b[31m".to_owned(),
        ] {
            assert!(known.found_in(&shown), "{shown}");
            let (redacted, hit) = known.redact(&shown);
            assert!(hit);
            assert!(!redacted.contains("line-one"), "{redacted}");
        }
        let text = format!("\u{e9}{}\u{1F600}", &PROOF[3..15]);
        let (shown, _) = learned(&[PROOF]).redact(&text);
        assert_eq!(shown, format!("\u{e9}{REDACTED}\u{1F600}"));
    }

    /// Matching is one step per text byte plus the learned windows; past
    /// the budget the text is not scanned and counts as a hit everywhere.
    #[test]
    fn known_secrets_fail_closed_past_the_work_budget() {
        let known = learned(&[PROOF]);
        let text = format!("{}{}", "x".repeat(4096), &PROOF[5..13]);
        WORK.with(|work| work.set(0));
        assert!(known.found_in(&text));
        let steps = WORK.with(std::cell::Cell::get);
        assert!(steps <= text.len() + 1, "{steps} steps");
        let budget = text.len();
        assert!(known.found_within("ordinary text", 1));
        assert!(known.found_within(&"x".repeat(budget), budget));
        assert_eq!(
            known.redact_within(&"y".repeat(budget), budget),
            (Cow::Borrowed(REDACTED), true)
        );
        assert!(!known.found_within("ordinary text", budget));
        assert!(!KnownSecrets::default().found_within(&"x".repeat(budget), 1));
    }
}
