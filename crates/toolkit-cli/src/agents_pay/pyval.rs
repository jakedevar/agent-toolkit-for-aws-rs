//! Python value semantics over [`Json`] (x402 S1, #5118).
//!
//! The legacy gate compared challenge and policy values through Python's
//! `str()`, branched on truthiness and read the version with `int()`. The
//! token bytes and every allowlist comparison depend on those renderings, so
//! they are reproduced here, on ASCII:
//!
//! - [`truthy`]: `null`, `false`, `0`, `0.0`, `""`, `[]` and `{}` are false.
//! - [`py_str`]: a string as is, an integer in decimal, `True`/`False`,
//!   `None`, and a float in Python's shortest `repr`. An array or object has
//!   no rendering (D2: such a field makes an entry ineligible).
//! - [`py_int`]: `int()` of a JSON value. Booleans are 0/1, a float is
//!   truncated toward zero, a string takes surrounding whitespace, one sign,
//!   ASCII digits and single `_` between digits. Anything else, and any
//!   value outside `i64` (D7), raises `ValueError`.

use std::num::FpCategory;

use toolkit_core::Json;

/// Python raised `ValueError` (or would have raised another error that this
/// port reports as `ValueError`, R-ERR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyValueError;

/// Python's truthiness of a JSON value.
#[must_use]
pub fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Int(number) => *number != 0,
        Json::Float(number) => number.classify() != FpCategory::Zero,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(members) => !members.is_empty(),
    }
}

/// Python's `str()` of a JSON scalar; `None` for an array or object (D2).
#[must_use]
pub fn py_str(value: &Json) -> Option<String> {
    let text = match value {
        Json::Null => "None".to_owned(),
        Json::Bool(true) => "True".to_owned(),
        Json::Bool(false) => "False".to_owned(),
        Json::Int(number) => number.to_string(),
        Json::Float(number) => float_repr(*number),
        Json::String(text) => text.clone(),
        Json::Array(_) | Json::Object(_) => return None,
    };
    Some(text)
}

/// Python's `repr()` of a float: the shortest digits that round-trip, in
/// positional form when the decimal point falls within 16 digits of the
/// first digit, otherwise `d.ddde±XX`.
#[must_use]
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value.classify() == FpCategory::Zero {
        return format!("{sign}0.0");
    }
    // `{:e}` is the shortest round-trip form, `d[.ddd]e<exp>`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    // value = 0.<digits> x 10^point
    let point = exponent + 1;
    if point <= -4 || point > 16 {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{first}{fraction}e{exp_sign}{:02}", exponent.abs());
    }
    let count = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    if point <= 0 {
        let zeros = "0".repeat(usize::try_from(-point).unwrap_or(0));
        format!("{sign}0.{zeros}{digits}")
    } else if point >= count {
        let zeros = "0".repeat(usize::try_from(point - count).unwrap_or(0));
        format!("{sign}{digits}{zeros}.0")
    } else {
        let (whole, fraction) = digits.split_at(usize::try_from(point).unwrap_or(0));
        format!("{sign}{whole}.{fraction}")
    }
}

/// The ASCII whitespace accepted by Python's `Decimal()`.
pub(crate) fn is_decimal_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{1c}'..='\u{1f}' | ' '
    )
}

/// ASCII digits with each `_` between two digits removed; `None` for any
/// other character or a misplaced `_` (PEP 515).
pub(crate) fn strip_digit_underscores(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    for (index, &byte) in bytes.iter().enumerate() {
        if byte.is_ascii_digit() {
            out.push(char::from(byte));
            continue;
        }
        let between_digits = byte == b'_'
            && index > 0
            && bytes[index - 1].is_ascii_digit()
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
        if !between_digits {
            return None;
        }
    }
    Some(out)
}

/// Python `int(text)` in base 10 on ASCII; `None` where Python raises (or
/// where the value leaves `i64`, D7).
fn int_from_str(text: &str) -> Option<i64> {
    let trimmed = text.trim_matches(|c| matches!(c, '\t'..='\r' | ' '));
    let (negative, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digits = strip_digit_underscores(body)?;
    if digits.is_empty() {
        return None;
    }
    let significant = digits.trim_start_matches('0');
    if significant.len() > 19 {
        return None;
    }
    let magnitude = if significant.is_empty() {
        0
    } else {
        significant.parse::<u64>().ok()?
    };
    let value = if negative {
        -i128::from(magnitude)
    } else {
        i128::from(magnitude)
    };
    i64::try_from(value).ok()
}

/// `2^63` as a float: the first value above the `i64` range.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// Python `int(value)` for a JSON value (R-VER).
///
/// # Errors
///
/// [`PyValueError`] where Python raises, and for values outside `i64` (D7).
pub fn py_int(value: &Json) -> Result<i64, PyValueError> {
    match value {
        Json::Bool(flag) => Ok(i64::from(*flag)),
        Json::Int(number) => Ok(*number),
        Json::Float(number) => {
            let truncated = number.trunc();
            if !truncated.is_finite() || !(-I64_LIMIT..I64_LIMIT).contains(&truncated) {
                return Err(PyValueError);
            }
            // In range by the check above, so the conversion is exact.
            #[allow(clippy::cast_possible_truncation)]
            let whole = truncated as i64;
            Ok(whole)
        }
        Json::String(text) => int_from_str(text).ok_or(PyValueError),
        Json::Null | Json::Array(_) | Json::Object(_) => Err(PyValueError),
    }
}
