//! Printing untrusted text in diagnostics (#4602).
//!
//! Values read from input files (JSON keys, strings, file names) must not
//! reach a terminal or a log as raw bytes: a carriage return, an escape
//! sequence, a bidirectional override or a line separator can forge report
//! lines or drive the terminal, and an unbounded value can flood the output.
//! [`safe_text`] keeps ordinary text unchanged and escapes the rest the way
//! Python's `repr` does (`\x1b`, `\u202e`, `\U000e0001`).

/// Longest text, in characters, that [`safe_text`] prints before `...`.
pub(crate) const MAX_SHOWN_CHARS: usize = 120;

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

/// `text` made safe to print: unsafe characters escaped, at most
/// [`MAX_SHOWN_CHARS`] characters shown (then `...`). Text without unsafe
/// characters and within the bound is returned unchanged.
pub(crate) fn safe_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_SHOWN_CHARS * 2));
    for (index, c) in text.chars().enumerate() {
        if index == MAX_SHOWN_CHARS {
            out.push_str("...");
            break;
        }
        if is_unsafe(c) {
            push_escaped(&mut out, c);
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` with unsafe characters escaped and no length bound, for a file path
/// (the file name at the end must stay visible).
pub(crate) fn safe_path(text: &str) -> String {
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

/// [`safe_text`] between single quotes, for a name inside a sentence.
pub(crate) fn safe_quoted(text: &str) -> String {
    format!("'{}'", safe_text(text).replace('\'', "\\'"))
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
    fn paths_are_escaped_but_never_shortened() {
        let long = format!("{}/report\n.md", "d".repeat(MAX_SHOWN_CHARS * 2));
        let shown = safe_path(&long);
        assert!(shown.ends_with("/report\\x0a.md"));
        assert_eq!(shown.chars().count(), long.chars().count() + 3);
    }

    /// The #4941 bound of a shown caller path, in characters.
    const MAX_PATH_CHARS: usize = 200;

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
        let ending = format!("/{}/api_key=FAKE_SECRET", "d".repeat(277));
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
