//! Escaping of untrusted text for diagnostics (#4639).
//!
//! Argument values, file names and payload fields are attacker-controlled. A
//! diagnostic that echoes them raw can forge extra report lines (CR/LF), drive
//! the terminal (ESC and other C0/C1 controls, BEL), reorder what an operator
//! reads (bidirectional overrides such as U+202E) or leak payload contents.
//! Slices #4602, #4625 and #4634 each failed review on that defect and each
//! repair invented its own escaping; this module is the one shared helper.
//!
//! Prefer **not echoing** untrusted text at all: report the field, location and
//! type instead (the #4602 repair). When a value must appear, pass it through
//! [`quote_untrusted`]. The result is a double-quoted string that
//!
//! - contains no control character, no line or paragraph separator, no
//!   bidirectional or other invisible formatting character and no byte-order
//!   mark: each is written as an escape (`\n`, `\r`, `\t` or `\uXXXX`), as
//!   are the backslash and the double quote, so the quoting cannot be broken;
//! - is bounded: only the first [`DEFAULT_MAX_CHARS`] characters are kept
//!   (see [`quote_untrusted_bounded`]) and a `...[N more chars]` marker records
//!   how many were dropped;
//! - leaves ordinary printable text, including non-ASCII letters, unchanged.
//!
//! This is escaping, not redaction. It does not recognise secrets; text that
//! may hold credentials must be redacted first (see `toolkit_aws::redact`) or
//! left out of the diagnostic.

use std::borrow::Cow;
use std::fmt::Write as _;

/// Characters kept by [`quote_untrusted`] before the truncation marker.
pub const DEFAULT_MAX_CHARS: usize = 120;

/// Whether `c` must never reach a terminal or a log line unescaped.
///
/// True for every control character (C0, DEL and C1, which includes ESC, BEL,
/// CR and LF), the Unicode line and paragraph separators, and the invisible
/// formatting characters that can reorder or hide text: the bidirectional
/// marks, embeddings, overrides and isolates (U+061C, U+200E, U+200F,
/// U+202A-U+202E, U+2066-U+2069), the zero-width and joiner characters
/// (U+200B-U+200D, U+2060-U+2064), the soft hyphen (U+00AD), the interlinear annotation marks
/// (U+FFF9-U+FFFB) and the byte-order mark (U+FEFF).
#[must_use]
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

/// Escape `text` for a diagnostic and bound it to `max_chars` characters,
/// without surrounding quotes.
///
/// Escapes `\`, `"`, newline, carriage return and tab as `\\`, `\"`, `\n`,
/// `\r` and `\t`; every other [`is_unsafe_char`] character as `\uXXXX`
/// (uppercase hexadecimal; every such character is in the Basic Multilingual
/// Plane, so the escape is also valid inside a JSON string). If `text` has more than
/// `max_chars` characters, the rest is replaced by `...[N more chars]` where
/// `N` is the number of characters dropped (counted before escaping).
#[must_use]
pub fn escape_untrusted(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars.saturating_mul(2)) + 24);
    let mut chars = text.chars();
    for c in chars.by_ref().take(max_chars) {
        push_escaped(&mut out, c);
    }
    let dropped = chars.count();
    if dropped > 0 {
        let _ = write!(out, "...[{dropped} more chars]");
    }
    out
}

/// [`escape_untrusted`] inside double quotes, bounded to `max_chars`.
#[must_use]
pub fn quote_untrusted_bounded(text: &str, max_chars: usize) -> String {
    format!("\"{}\"", escape_untrusted(text, max_chars))
}

/// The helper every diagnostic should use for untrusted text: escaped, quoted
/// and bounded to [`DEFAULT_MAX_CHARS`] characters.
#[must_use]
pub fn quote_untrusted(text: &str) -> String {
    quote_untrusted_bounded(text, DEFAULT_MAX_CHARS)
}

/// Make `line` safe to write to stderr or a log as ONE line (newlines too), changing nothing
/// else: every [`is_unsafe_char`] character (newline, carriage return and tab
/// included) becomes `\n`, `\r`, `\t` or `\uXXXX`, while backslashes, quotes and
/// ordinary text stay as they are and nothing is truncated. Text that is
/// already safe is returned borrowed.
///
/// This is the last-line defence at the output sinks (`Output::diagnostic` and
/// the final `error:` report in `toolkit-cli`, which use [`neutralize_text`]), so an echo a slice forgot to
/// quote still cannot forge a line or drive the terminal. It is not a
/// substitute for [`quote_untrusted`] at the call site: the result is not
/// quoted, so an escaped value is not distinguishable from one that contained a
/// literal backslash sequence.
#[must_use]
pub fn neutralize_line(line: &str) -> Cow<'_, str> {
    neutralize(line, false)
}

/// Like [`neutralize_line`] but newlines stay as line separators, for text that
/// is legitimately several lines (a multi-line warning block). Every other
/// unsafe character, carriage return and tab included, is escaped. A newline
/// that came from untrusted text is NOT neutralised here, so call sites that
/// echo such text into a multi-line block must quote it with
/// [`quote_untrusted`].
#[must_use]
pub fn neutralize_text(text: &str) -> Cow<'_, str> {
    neutralize(text, true)
}

/// [`neutralize_text`] that also escapes the newlines of the values in
/// `untrusted` wherever they appear verbatim in `text`.
///
/// Some diagnostics are legitimately multi-line, so [`neutralize_text`] keeps
/// newlines; but a caller-supplied value that contains a newline and is echoed
/// into such a message would forge a line. The command line is the one source of
/// untrusted text every handler sees, so the CLI passes its argument values
/// here: each occurrence of a listed value is written with its newlines (and
/// every other unsafe character) escaped, then the rest of `text` is neutralised
/// as by [`neutralize_text`]. Values without a newline are ignored.
#[must_use]
pub fn neutralize_text_knowing<'a>(text: &'a str, untrusted: &[String]) -> Cow<'a, str> {
    let mut values: Vec<&str> = untrusted
        .iter()
        .map(String::as_str)
        .filter(|value| value.contains('\n'))
        .collect();
    // Longest first, so a value that contains another is replaced as a whole.
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    let mut current = Cow::Borrowed(text);
    for value in values {
        if current.contains(value) {
            let replaced = current.replace(value, &neutralize_line(value));
            current = Cow::Owned(replaced);
        }
    }
    if current.chars().any(|c| c != '\n' && is_unsafe_char(c)) {
        Cow::Owned(neutralize_text(&current).into_owned())
    } else {
        current
    }
}

fn neutralize(text: &str, keep_newlines: bool) -> Cow<'_, str> {
    let unsafe_here = |c: char| is_unsafe_char(c) && !(keep_newlines && c == '\n');
    if !text.chars().any(unsafe_here) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        match c {
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\n' if !keep_newlines => out.push_str("\\n"),
            c if unsafe_here(c) => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '\\' => out.push_str("\\\\"),
        '"' => out.push_str("\\\""),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if is_unsafe_char(c) => {
            let _ = write!(out, "\\u{:04X}", u32::from(c));
        }
        c => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_only_quoted() {
        assert_eq!(quote_untrusted("hello world"), "\"hello world\"");
        assert_eq!(quote_untrusted(""), "\"\"");
        assert_eq!(
            quote_untrusted("caf\u{e9} \u{65e5}\u{672c}"),
            "\"caf\u{e9} \u{65e5}\u{672c}\""
        );
    }

    #[test]
    fn terminal_and_line_controls_are_escaped() {
        assert_eq!(
            quote_untrusted("a\u{1b}[31mred\u{7}\r\nnext\tx"),
            "\"a\\u001B[31mred\\u0007\\r\\nnext\\tx\""
        );
        assert_eq!(
            quote_untrusted("\u{0}\u{7f}\u{9b}"),
            "\"\\u0000\\u007F\\u009B\""
        );
        assert_eq!(
            quote_untrusted("\u{85}\u{2028}\u{2029}"),
            "\"\\u0085\\u2028\\u2029\""
        );
    }

    #[test]
    fn bidi_and_invisible_formatting_characters_are_escaped() {
        assert_eq!(quote_untrusted("a\u{202e}b"), "\"a\\u202Eb\"");
        for c in [
            '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}', '\u{200b}', '\u{200d}',
            '\u{2060}', '\u{feff}', '\u{ad}',
        ] {
            let quoted = quote_untrusted(&format!("x{c}y"));
            assert!(
                !quoted.chars().any(is_unsafe_char),
                "{:04X} leaked: {quoted:?}",
                u32::from(c)
            );
            assert!(quoted.contains("\\u"), "{:04X}: {quoted:?}", u32::from(c));
        }
    }

    #[test]
    fn quote_and_backslash_cannot_break_out_of_the_quoting() {
        assert_eq!(quote_untrusted(r#"a"b\c"#), r#""a\"b\\c""#);
        // An already-escaped looking value stays distinguishable from a real escape.
        assert_eq!(quote_untrusted(r"\n"), r#""\\n""#);
    }

    #[test]
    fn output_never_contains_an_unsafe_character_for_every_scalar_value() {
        let mut every = String::new();
        for code in 0..=0x2_ffff_u32 {
            if let Some(c) = char::from_u32(code) {
                every.push(c);
            }
        }
        let escaped = escape_untrusted(&every, usize::MAX);
        assert!(!escaped.chars().any(is_unsafe_char));
        assert!(!escaped.contains('\n') && !escaped.contains('\r'));
    }

    #[test]
    fn long_text_is_bounded_with_a_marker_counting_dropped_characters() {
        let long = "x".repeat(DEFAULT_MAX_CHARS + 5);
        let quoted = quote_untrusted(&long);
        assert_eq!(
            quoted,
            format!("\"{}...[5 more chars]\"", "x".repeat(DEFAULT_MAX_CHARS))
        );
        let exact = "y".repeat(DEFAULT_MAX_CHARS);
        assert_eq!(quote_untrusted(&exact), format!("\"{exact}\""));
        assert_eq!(
            quote_untrusted_bounded("abcdef", 3),
            "\"abc...[3 more chars]\""
        );
        assert_eq!(quote_untrusted_bounded("abc", 0), "\"...[3 more chars]\"");
    }

    #[test]
    fn bound_counts_characters_not_bytes_and_never_splits_one() {
        let text = "\u{1f600}".repeat(4);
        assert_eq!(
            quote_untrusted_bounded(&text, 2),
            "\"\u{1f600}\u{1f600}...[2 more chars]\""
        );
    }

    #[test]
    fn escape_bound_applies_before_escaping_so_output_stays_proportional() {
        let hostile = "\u{1b}".repeat(10_000);
        let escaped = escape_untrusted(&hostile, 10);
        assert_eq!(
            escaped,
            format!("{}...[9990 more chars]", "\\u001B".repeat(10))
        );
    }

    #[test]
    fn neutralize_line_escapes_only_unsafe_characters() {
        assert!(matches!(
            neutralize_line("plain \\ \"text\""),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            neutralize_line("a\u{1b}[0m\r\nb\tc\u{202e}d \\ \"q\""),
            "a\\u001B[0m\\r\\nb\\tc\\u202Ed \\ \"q\""
        );
        let all: String = (0..=0x2_ffff_u32).filter_map(char::from_u32).collect();
        assert!(!neutralize_line(&all).chars().any(is_unsafe_char));
        assert_eq!(neutralize_text("a\nb\r\u{1b}c"), "a\nb\\r\\u001Bc");
        assert!(matches!(neutralize_text("one\ntwo\n"), Cow::Borrowed(_)));
    }

    #[test]
    fn known_values_have_their_newlines_escaped_inside_multiline_text() {
        let value = "a\nerror: forged".to_string();
        assert_eq!(
            neutralize_text_knowing(
                "error: bad a\nerror: forged here\nreal second line",
                &[value]
            ),
            "error: bad a\\nerror: forged here\nreal second line"
        );
        assert!(matches!(
            neutralize_text_knowing("x\ny", &["no newline".to_string(), String::new()]),
            Cow::Borrowed(_)
        ));
        let nested = ["a\nb".to_string(), "a\nb\nc".to_string()];
        assert_eq!(neutralize_text_knowing("[a\nb\nc]", &nested), "[a\\nb\\nc]");
    }

    #[test]
    fn hostile_input_keeps_a_secret_marker_only_as_plain_text() {
        // The helper escapes; it does not redact. Callers must not pass secrets.
        let quoted = quote_untrusted("\u{1b}[2JSECRET-4639\u{202e}\r");
        assert_eq!(quoted, "\"\\u001B[2JSECRET-4639\\u202E\\r\"");
    }
}
