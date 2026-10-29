//! Text, number and JSON helpers shared by the ElastiCache commands.
//!
//! The scripts print through Python `round`, `int`, `sum`, `str.format` and
//! `json.dumps`; the pieces that differ from Rust defaults live here once.
//! Everything that echoes a value taken from an input file or the command line
//! into a report goes through [`show`], so a control character or a very long
//! value can never forge a line or a terminal sequence (#4602).

use std::fmt::Write as _;

use toolkit_core::Json;

use crate::aurora::pyfmt::dumps;
use crate::manifests::py_repr;

/// Longest value [`show`] prints before it elides the rest.
const MAX_SHOWN_CHARS: usize = 120;

/// Why a float cannot become an integer (Python `ValueError` / `OverflowError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundError {
    /// `round(nan)` / `int(nan)`.
    Nan,
    /// An infinity or a magnitude beyond `i64`.
    Overflow,
}

impl RoundError {
    /// The message the failing conversion reports.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Nan => "cannot convert float NaN to integer",
            Self::Overflow => "cannot convert float infinity or an out-of-range value to integer",
        }
    }
}

fn float_to_int(value: f64) -> Result<i64, RoundError> {
    if value.is_nan() {
        return Err(RoundError::Nan);
    }
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value) {
        return Err(RoundError::Overflow);
    }
    Ok(value as i64)
}

/// Python `round(value)`: the nearest integer, ties to even.
pub fn round_int(value: f64) -> Result<i64, RoundError> {
    if value.is_nan() {
        return Err(RoundError::Nan);
    }
    float_to_int(value.round_ties_even())
}

/// Python `int(value)`: truncation toward zero.
pub fn trunc_int(value: f64) -> Result<i64, RoundError> {
    float_to_int(value.trunc())
}

/// `text` with control characters and bidirectional overrides written as
/// `\xNN` / `\uNNNN` escapes, never truncated.
#[must_use]
pub fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            let _ = write!(out, "\\x{:02x}", u32::from(c));
        } else if matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` safe to print in a report or diagnostic: redacted in full first
/// (`toolkit_aws::redact_text`, #4941), then control characters are escaped
/// and a value longer than 120 characters is elided. Text without
/// credential-shaped content is shown as before.
#[must_use]
pub fn show(text: &str) -> String {
    let escaped = escape_controls(&toolkit_aws::redact_text(text));
    if escaped.chars().count() <= MAX_SHOWN_CHARS {
        return escaped;
    }
    let mut shortened: String = escaped.chars().take(MAX_SHOWN_CHARS).collect();
    shortened.push_str("...");
    shortened
}

/// [`show`] in single quotes, like Python `repr` of a plain string.
#[must_use]
pub fn quoted(text: &str) -> String {
    format!("'{}'", show(text).replace('\'', "\\'"))
}

/// Python `str(value)` of a JSON value: a string as itself, anything else as
/// its `repr`. Strings are returned as they are; callers escape them.
#[must_use]
pub fn py_str(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// Pads `text` with spaces on the right to `width` characters.
#[must_use]
pub fn pad_right(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    for _ in text.chars().count()..width {
        out.push(' ');
    }
    out
}

/// Pads `text` with spaces on the left to `width` characters.
#[must_use]
pub fn pad_left(text: &str, width: usize) -> String {
    let mut out = String::new();
    for _ in text.chars().count()..width {
        out.push(' ');
    }
    out.push_str(text);
    out
}

/// Python `f"{value:,}"` for an integer.
#[must_use]
pub fn group_int(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Python `sum()` of floats (compensated summation, as CPython 3.12+ does).
#[must_use]
pub fn py_sum(values: &[f64]) -> f64 {
    let mut result = 0.0_f64;
    let mut compensation = 0.0_f64;
    for &item in values {
        let total = result + item;
        if result.abs() >= item.abs() {
            compensation += (result - total) + item;
        } else {
            compensation += (item - total) + result;
        }
        result = total;
    }
    if compensation != 0.0 && compensation.is_finite() {
        result += compensation;
    }
    result
}

/// Python `os.path.splitext` on a `/`-separated path.
#[must_use]
pub fn splitext(path: &str) -> (&str, &str) {
    let base_start = path.rfind('/').map_or(0, |index| index + 1);
    let base = &path[base_start..];
    let stripped = base.trim_start_matches('.');
    let leading = base.len() - stripped.len();
    match stripped.rfind('.') {
        Some(index) => {
            let split = base_start + leading + index;
            (&path[..split], &path[split..])
        }
        None => (path, ""),
    }
}

/// `text` quoted for a POSIX shell when it contains anything but plain
/// path characters (controls are escaped first, so the hint stays one line).
#[must_use]
pub fn shell_quote(text: &str) -> String {
    let safe = escape_controls(text);
    if !safe.is_empty()
        && safe
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        return safe;
    }
    format!("'{}'", safe.replace('\'', "'\\''"))
}

/// A `(key, value)` member for [`Json::Object`].
#[must_use]
pub fn kv(key: &str, value: Json) -> (String, Json) {
    (key.to_string(), value)
}

/// A JSON string.
#[must_use]
pub fn js(text: &str) -> Json {
    Json::String(text.to_string())
}

/// Sets `key` in an ordered member list: replaces the value in place when the
/// key exists (Python dict assignment), appends otherwise.
pub fn set_member(members: &mut Vec<(String, Json)>, key: &str, value: Json) {
    if let Some(slot) = members.iter_mut().find(|(name, _)| name == key) {
        slot.1 = value;
    } else {
        members.push((key.to_string(), value));
    }
}

/// Python `json.dumps(value)` with the default separators (`", "` and `": "`).
#[must_use]
pub fn dumps_inline(value: &Json) -> String {
    let mut out = String::new();
    write_inline(&mut out, value);
    out
}

// recorded-shape: a total writer of any JSON value into its inline display form;
// every type is rendered, no member is read, typed or defaulted.
fn write_inline(out: &mut String, value: &Json) {
    match value {
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_inline(out, item);
            }
            out.push(']');
        }
        Json::Object(members) => {
            out.push('{');
            for (index, (key, member)) in members.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&dumps(&Json::String(key.clone())));
                out.push_str(": ");
                write_inline(out, member);
            }
            out.push('}');
        }
        scalar => out.push_str(&dumps(scalar)),
    }
}
