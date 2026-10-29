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

/// The escaping of [`repr`] (with `quote`) or [`shown`] (without), applied
/// to `value` as it is: no redaction, bounding or surrounding quotes. The
/// output sink registers a live secret in each of these forms (#5426).
#[must_use]
pub(crate) fn escaped(value: &str, quote: Option<char>) -> String {
    let mut out = String::with_capacity(value.len());
    push_escaped(&mut out, value, quote);
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

/// The success line of a launch command: Python
/// `json.dumps(value, indent=2, default=str)` (`ensure_ascii`, Python float
/// `repr`, the value's own member order).
#[must_use]
pub fn ok_text(value: &toolkit_core::Json) -> String {
    crate::aurora::pyfmt::dumps(value)
}

/// The failure line of a launch command: Python
/// `json.dumps({"error": message})` (compact default separators `", "` and
/// `": "`, `ensure_ascii`).
#[must_use]
pub fn error_text(message: &str) -> String {
    format!(
        "{{\"error\": {}}}",
        crate::aurora::pyfmt::dumps(&toolkit_core::Json::String(message.to_owned()))
    )
}

/// Write [`error_text`] to the diagnostic stream and set exit status 1.
pub fn fail(output: &mut crate::Output<'_>, message: &str) {
    output.diagnostic(&error_text(message));
    output.set_exit_status(1);
}
