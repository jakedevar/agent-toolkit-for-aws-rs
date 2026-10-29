//! Integer literals beyond 64 bits, judged by their text (#4984).
//!
//! The workspace JSON reader turns an integer literal it cannot hold in an
//! `i64` into a float. The float nearest `-9223372036854775809` is exactly
//! `-9223372036854775808`, so once parsed the literal is indistinguishable
//! from a legitimate `i64::MIN` and a range check on the value accepts it. The
//! script keeps such an integer exactly (Python integers are unbounded), so it
//! is out of range, never `i64::MIN`.
//!
//! [`parse_recorded`] settles that before any rounding can happen: it rewrites
//! each integer literal that does not fit an `i64` into the string of the same
//! digits, and the existing string path (`strict_int`, `quantity_reason`)
//! already reports such a string as `QuantityOutOfRange`. A literal that fits,
//! a float form (`.` or an exponent) and everything inside a string are left
//! exactly as they are, so `i64::MIN` and `i64::MAX` stay valid integers. The
//! shared reader in `toolkit-core` is unchanged.

use std::borrow::Cow;

use toolkit_core::{parse_json, Json, ParseError};

/// Parses `text` with every out-of-range integer literal kept as digits.
pub(super) fn parse_recorded(text: &str) -> Result<Json, ParseError> {
    parse_json(&quote_wide_integers(text))
}

/// `text` with each JSON integer literal that does not fit an `i64` wrapped in
/// quotes. Text that is not JSON passes through to the parser's own error.
fn quote_wide_integers(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => pos = end_of_string(bytes, pos),
            b'-' | b'0'..=b'9' => {
                let end = pos
                    + bytes[pos..]
                        .iter()
                        .take_while(|b| matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
                        .count();
                let token = &text[pos..end];
                if is_wide_integer(token) {
                    out.push_str(&text[copied..pos]);
                    out.push('"');
                    out.push_str(token);
                    out.push('"');
                    copied = end;
                }
                pos = end;
            }
            _ => pos += 1,
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// The position after the string literal that opens at `start`.
fn end_of_string(bytes: &[u8], start: usize) -> usize {
    let mut pos = start + 1;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => pos += 2,
            b'"' => return pos + 1,
            _ => pos += 1,
        }
    }
    bytes.len()
}

/// A well-formed JSON integer literal (no fraction, no exponent, no leading
/// zero) that an `i64` cannot hold.
fn is_wide_integer(token: &str) -> bool {
    let digits = token.strip_prefix('-').unwrap_or(token);
    let well_formed = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    well_formed && token.parse::<i64>().is_err()
}
