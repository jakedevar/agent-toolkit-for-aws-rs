//! Caller and archive values inside launch messages (R-TOK-RED, #4941).
//!
//! The Python formatted such values with `{value!r}`. [`repr`] keeps that
//! shape for a value that has first been through `safe_text::shown_value`
//! (redacted whole, then bounded), and also escapes the bidirectional and
//! invisible characters Python's `repr` would print raw, so an archive entry
//! name or an environment value cannot forge or hide message text.

use std::fmt::Write as _;

use crate::safe_text::{is_unsafe, shown_value};

/// Python `repr(str)` of the shown form of `value`: redacted, bounded, quoted
/// with `'` (or `"` when the text holds `'` and no `"`), with `\\`, the quote,
/// `\n`, `\r`, `\t` and every unsafe character escaped.
#[must_use]
pub fn repr(value: &str) -> String {
    let shown = shown_value(value);
    let quote = if shown.contains('\'') && !shown.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(shown.len() + 2);
    out.push(quote);
    push_escaped(&mut out, &shown, Some(quote));
    out.push(quote);
    out
}

/// The shown form of `value` without quotes: redacted, bounded, with `\\`,
/// `\n`, `\r`, `\t` and every unsafe character escaped, for a message that
/// names a value after a colon.
#[must_use]
pub fn shown(value: &str) -> String {
    let shown = shown_value(value);
    let mut out = String::with_capacity(shown.len());
    push_escaped(&mut out, &shown, None);
    out
}

fn push_escaped(out: &mut String, text: &str, quote: Option<char>) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if Some(c) == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_unsafe(c) => {
                let code = u32::from(c);
                let _ = if code <= 0xff {
                    write!(out, "\\x{code:02x}")
                } else if code <= 0xffff {
                    write!(out, "\\u{code:04x}")
                } else {
                    write!(out, "\\U{code:08x}")
                };
            }
            c => out.push(c),
        }
    }
}
