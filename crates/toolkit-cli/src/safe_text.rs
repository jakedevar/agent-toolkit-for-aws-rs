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
//!    Python's `repr` does (`\x1b`, `‮`, `\U000e0001`).
//!
//! Redacting first is the point: a cut made first can drop the `@host` that
//! marks URL userinfo or split an access-key id, so the redactor no longer
//! recognizes what is left of the credential (#4907, #4908, #4939, #4940).
//! Text without credential-shaped content and within the bound is returned
//! unchanged, so ordinary output is byte-identical.
//!
//! [`CallerText`] applies the same rule at the output sinks to every
//! command-line value, so a slice that echoes a value raw (or cut before
//! redaction) still cannot print its credential.

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

/// [`safe_text`] between single quotes, for a name inside a sentence.
pub(crate) fn safe_quoted(text: &str) -> String {
    format!("'{}'", safe_text(text).replace('\'', "\\'"))
}

/// Length in bytes of the longest common prefix of `a` and `b` (a character
/// boundary of both).
fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = 0;
    for (x, y) in a.chars().zip(b.chars()) {
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

/// The pieces of `original` that the redactor hid in `redacted`: the whole
/// changed span (between the longest common prefix and suffix), and, when
/// several values were hidden, each one on its own (the text kept between the
/// [`REDACTED`] marks is copied from `original` in order).
fn hidden_pieces<'a>(original: &'a str, redacted: &str) -> Vec<&'a str> {
    let prefix = common_prefix_len(original, redacted);
    let suffix = common_suffix_len(&original[prefix..], &redacted[prefix..]);
    let mut pieces = vec![&original[prefix..original.len() - suffix]];
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
        pieces.push(&original[start..end]);
        at = end;
    }
    pieces
}

/// The command-line values of one run, as the output sinks must show them
/// (#4639, #4941).
///
/// [`run`](crate::run) builds it from the arguments and both the final
/// `error:` report and [`Output::diagnostic`](crate::Output::diagnostic) pass
/// their text through [`CallerText::diagnostic`]; stdout lines go through
/// [`CallerText::stdout`]. A value is *known* when it holds a newline, is
/// credential-shaped (the shared redactor changes it) or is longer than
/// [`MAX_PATH_CHARS`] characters. Ordinary values are not known, so text that
/// echoes them stays byte-identical.
#[derive(Debug, Default, Clone)]
pub(crate) struct CallerText {
    /// `(value, how it is shown)`, longest value first: a known value echoed
    /// verbatim in a diagnostic is replaced by its redacted, bounded,
    /// single-line rendering.
    shown: Vec<(String, String)>,
    /// What the redactor hid in a credential-shaped value, in each escaped
    /// spelling, longest first: hidden wherever it appears in any output, in
    /// full or cut after at least [`MIN_PARTIAL_CHARS`] characters.
    fragments: Vec<String>,
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
                        .push((arg.clone(), neutralize_line(arg).into_owned()));
                }
            } else {
                caller.learn(arg);
            }
            if let Some((_, value)) = arg.split_once('=') {
                caller.learn(value);
            }
        }
        // Longest first, so a value that contains another is replaced whole.
        caller.shown.sort_by_key(|(value, _)| Reverse(value.len()));
        caller.fragments.sort_by_key(|piece| Reverse(piece.len()));
        caller.fragments.dedup();
        caller
    }

    fn learn(&mut self, value: &str) {
        let redacted = redact_text(value);
        let credential = redacted != value;
        if credential || value.contains('\n') || value.chars().count() > MAX_PATH_CHARS {
            let shown = neutralize_line(&bounded(&redacted, MAX_PATH_CHARS)).into_owned();
            self.shown.push((value.to_string(), shown));
        }
        if !credential {
            return;
        }
        for piece in hidden_pieces(value, &redacted) {
            let spellings = [
                piece.to_string(),
                neutralize_line(piece).into_owned(),
                escape_untrusted(piece, usize::MAX),
                escaped(piece),
            ];
            for spelling in spellings {
                if spelling.chars().count() >= MIN_FRAGMENT_CHARS
                    && !self.fragments.contains(&spelling)
                {
                    self.fragments.push(spelling);
                }
            }
        }
    }

    /// `text` with every credential fragment of a command-line value, whole
    /// or cut after at least [`MIN_PARTIAL_CHARS`] characters, replaced by
    /// [`REDACTED`]. Text without one is returned unchanged.
    fn hide_fragments<'t>(&self, text: Cow<'t, str>) -> Cow<'t, str> {
        let mut current = text;
        for fragment in &self.fragments {
            let need = fragment.chars().count().min(MIN_PARTIAL_CHARS);
            let anchor_end = fragment
                .char_indices()
                .nth(need)
                .map_or(fragment.len(), |(end, _)| end);
            let anchor = &fragment[..anchor_end];
            if !current.contains(anchor) {
                continue;
            }
            let mut out = String::with_capacity(current.len());
            let mut rest: &str = &current;
            while let Some(found) = rest.find(anchor) {
                out.push_str(&rest[..found]);
                out.push_str(REDACTED);
                let tail = &rest[found..];
                rest = &tail[common_prefix_len(tail, fragment)..];
            }
            out.push_str(rest);
            current = Cow::Owned(out);
        }
        current
    }

    /// A diagnostic or error report made safe for stderr: each known value
    /// echoed verbatim is shown redacted, bounded to [`MAX_PATH_CHARS`]
    /// characters and on one line; any remaining credential fragment is
    /// hidden; then every unsafe character but the newline is escaped
    /// (`toolkit_core::neutralize_text`). Text that echoes no known value and
    /// holds no unsafe character is returned unchanged.
    pub(crate) fn diagnostic<'t>(&self, text: &'t str) -> Cow<'t, str> {
        let mut current = Cow::Borrowed(text);
        for (value, shown) in &self.shown {
            if current.contains(value.as_str()) {
                current = Cow::Owned(current.replace(value.as_str(), shown));
            }
        }
        let current = self.hide_fragments(current);
        if current.chars().any(|c| c != '\n' && is_unsafe_char(c)) {
            Cow::Owned(neutralize_text(&current).into_owned())
        } else {
            current
        }
    }

    /// A stdout line with every credential fragment of a command-line value
    /// hidden. Nothing else changes: stdout is the command's data.
    pub(crate) fn stdout<'t>(&self, text: &'t str) -> Cow<'t, str> {
        self.hide_fragments(Cow::Borrowed(text))
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
            assert!(matches!(known.stdout(text), Cow::Borrowed(_)), "{text}");
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
    fn a_credential_value_echoed_raw_or_cut_is_hidden_at_every_sink() {
        let url = userinfo(0);
        let past = userinfo(250);
        let known = caller(&["seed", "--recorded", &url, &format!("--out={past}")]);
        let shown = known.diagnostic(&format!("error: cannot read {url}: missing"));
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
            assert_no_secret(&known.stdout(&text));
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
        assert!(matches!(known.stdout(&long), Cow::Borrowed(_)));
    }

    #[test]
    fn each_hidden_value_of_an_argument_is_a_fragment() {
        let value = "a/password=FAKE_SECRET_ONE b/token=FAKE_SECRET_TWO/c";
        let redacted = toolkit_aws::redact_text(value);
        let pieces = hidden_pieces(value, &redacted);
        assert!(!pieces.is_empty(), "{redacted}");
        let known = caller(&["x", value]);
        assert_no_secret(&known.stdout("only FAKE_SECRET_TWO here"));
        assert_no_secret(&known.stdout("only FAKE_SECRET_ONE here"));
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
    fn quoted_text_escapes_the_quote() {
        assert_eq!(safe_quoted("it's"), "'it\\'s'");
        assert_eq!(safe_quoted("a\nb"), "'a\\x0ab'");
    }
}
