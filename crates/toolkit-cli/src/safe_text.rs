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
//! their schema (#4981).
//!
//! [`CallerText`] is a stderr safety net behind that rule, never the only
//! protection: a diagnostic that still echoes a command-line value raw (or
//! cut before redaction) does not print its credential. It finds every match
//! against the original text before replacing any (#4978), and hides a
//! complete labelled credential whatever its length (#4980).

use std::borrow::Cow;
use std::cmp::Reverse;

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

/// A credential piece shorter than this is not searched for in output: it
/// would match ordinary words.
const MIN_FRAGMENT_CHARS: usize = 4;

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
    escaped(&bounded(&redact_text(text), count))
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
    let redacted = redact_text(text);
    escaped(&redacted.chars().take(count).collect::<String>())
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
    let redacted = redact_text(text);
    if redacted == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(redacted)
    }
}

/// [`safe_text`] between single quotes, for a name inside a sentence.
pub(crate) fn safe_quoted(text: &str) -> String {
    format!("'{}'", safe_text(text).replace('\'', "\\'"))
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

/// One learned piece of a command-line credential, in one spelling: hidden
/// wherever at least its first `min_bytes` bytes appear, and as far as the
/// output keeps matching it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fragment {
    text: String,
    min_bytes: usize,
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
    /// What the redactor hid in a credential-shaped value, in each spelling:
    /// the hidden piece alone (at least [`MIN_FRAGMENT_CHARS`] characters,
    /// matched from [`MIN_PARTIAL_CHARS`] on) and, whatever its length, the
    /// piece with the label in front of it (matched from the label and the
    /// first hidden character on, #4980).
    fragments: Vec<Fragment>,
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
            if piece.chars().count() >= MIN_FRAGMENT_CHARS {
                for spelling in spellings(piece) {
                    let min_bytes = spelling
                        .char_indices()
                        .nth(MIN_PARTIAL_CHARS)
                        .map_or(spelling.len(), |(at, _)| at);
                    self.add(spelling, min_bytes);
                }
            }
            if let Some(label) = label_start(value, start) {
                for (whole, label) in spellings(&value[label..end])
                    .into_iter()
                    .zip(spellings(&value[label..start]))
                {
                    let min_bytes = whole
                        .strip_prefix(label.as_str())
                        .and_then(|rest| rest.chars().next())
                        .map_or(whole.len(), |first| label.len() + first.len_utf8());
                    self.add(whole, min_bytes);
                }
            }
        }
    }

    fn add(&mut self, text: String, min_bytes: usize) {
        let fragment = Fragment { text, min_bytes };
        if fragment.min_bytes > 0 && !self.fragments.contains(&fragment) {
            self.fragments.push(fragment);
        }
    }

    /// Every span of `text` to replace, found in the original text before any
    /// replacement (#4978): each verbatim echo of a known value, and each
    /// place a credential fragment appears, whole or cut.
    fn spans(&self, text: &str) -> Vec<(usize, usize, Replacement<'_>)> {
        let mut spans = Vec::new();
        for (value, shown, redacted) in &self.shown {
            for (at, _) in text.match_indices(value.as_str()) {
                let replacement = Replacement::Shown {
                    shown,
                    redacted: *redacted,
                };
                spans.push((at, at + value.len(), replacement));
            }
        }
        for fragment in &self.fragments {
            let anchor = &fragment.text[..fragment.min_bytes];
            let mut from = 0;
            while let Some(offset) = text[from..].find(anchor) {
                let at = from + offset;
                let len = common_prefix_len(&text[at..], &fragment.text);
                spans.push((at, at + len, Replacement::Hidden));
                // Overlapping occurrences too: the next character on.
                from = at + text[at..].chars().next().map_or(1, char::len_utf8);
            }
        }
        spans
    }

    /// `text` with every span of [`CallerText::spans`] replaced. A span inside
    /// a known value's rendering that already hides its credential is dropped;
    /// any other overlap is merged and hidden as one [`REDACTED`].
    fn replace_spans<'t>(&self, text: &'t str) -> Cow<'t, str> {
        let mut spans = self.spans(text);
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

    #[test]
    fn quoted_text_escapes_the_quote() {
        assert_eq!(safe_quoted("it's"), "'it\\'s'");
        assert_eq!(safe_quoted("a\nb"), "'a\\x0ab'");
    }
}
