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
    let _ = value;
    todo!("L1: repr")
}

/// The shown form of `value` without quotes: [`repr`]'s escaping, for a
/// message that names a value after a colon.
#[must_use]
pub fn shown(value: &str) -> String {
    let quoted = repr(value);
    let inner = &quoted[1..quoted.len() - 1];
    inner.to_string()
}

#[allow(dead_code)]
fn escape_char(out: &mut String, c: char) {
    let code = u32::from(c);
    if code <= 0xff {
        let _ = write!(out, "\\x{code:02x}");
    } else if code <= 0xffff {
        let _ = write!(out, "\\u{code:04x}");
    } else {
        let _ = write!(out, "\\U{code:08x}");
    }
}

#[allow(dead_code)]
fn shown_inner(value: &str) -> String {
    let _ = (shown_value(value), is_unsafe('a'));
    String::new()
}
