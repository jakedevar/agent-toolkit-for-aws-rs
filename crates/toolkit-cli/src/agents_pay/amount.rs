//! Amounts and the per-payment cap (R-AMT, x402 S1 #5118).
//!
//! No float, decimal or big-integer type is involved: every comparison is a
//! comparison of ASCII digit strings.
//!
//! - **Challenge amounts** ([`base_units`]) are base units written as ASCII
//!   digits only (D5), greater than zero, kept verbatim (leading zeros
//!   included), as the legacy gate kept them.
//! - **Aliases** ([`validated_amount_units`]): `amount` and
//!   `maxAmountRequired` must agree through Python `str()` when both are set.
//! - **The cap** ([`CapDecimal`], R-AMT-REP) is parsed once into
//!   `value = D x 10^E` from the finite Python `Decimal` grammar: surrounding
//!   whitespace, one sign, `I`, `I.F`, `.F` or `I.`, an optional exponent, and
//!   `_` between two digits. `Infinity`, `NaN`, `sNaN`, a written exponent
//!   beyond +-1000 and a mantissa of more than 1000 digits are refused (D6).
//! - **The comparison** ([`exceeds_cap`], R-AMT-CMP): `A x 10^-6 > D x 10^E`
//!   becomes `A > D x 10^(E+6)`, with both sides padded into integers.
//! - **`amount_usd`** ([`usd_text`]) renders `Decimal(units) / 10**6` under
//!   Python's default context, as `str()` prints it (Q10).

use std::cmp::Ordering;

use toolkit_core::Json;

use super::pyval::{is_decimal_space, py_str, strip_digit_underscores};

/// USDC base-unit decimals, used for every asset as in the legacy gate (Q9).
pub const USDC_DECIMALS: u32 = 6;

/// Largest written exponent magnitude and mantissa length accepted (D6).
const MAX_CAP_EXPONENT: u32 = 1000;
const MAX_CAP_DIGITS: usize = 1000;

/// Python `Decimal`'s default context precision.
const DECIMAL_PRECISION: usize = 28;

/// Why a challenge amount was refused; each has the legacy text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountError {
    /// `amount` and `maxAmountRequired` differ.
    Conflicting,
    /// Neither alias is set.
    Missing,
    /// Not ASCII digits (D5), or an array, object, float or boolean.
    NotPlainInteger,
    /// Zero.
    NotPositive,
}

impl AmountError {
    /// The legacy refusal text.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Conflicting => "Payment challenge contains conflicting amount fields.",
            Self::Missing => "Payment challenge has no amount.",
            Self::NotPlainInteger => {
                "Payment challenge amount must be a plain integer in base units."
            }
            Self::NotPositive => "Payment challenge amount must be positive.",
        }
    }
}

/// `text` as base units: ASCII digits only, greater than zero.
///
/// # Errors
///
/// [`AmountError::NotPlainInteger`] or [`AmountError::NotPositive`].
pub fn base_units(text: &str) -> Result<&str, AmountError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmountError::NotPlainInteger);
    }
    if text.bytes().all(|b| b == b'0') {
        return Err(AmountError::NotPositive);
    }
    Ok(text)
}

/// The one canonical amount text of an `accepts` entry: `str(amount)`, else
/// `str(maxAmountRequired)`, refusing conflicting aliases.
///
/// # Errors
///
/// An [`AmountError`] with the legacy text.
pub fn validated_amount_units(entry: &Json) -> Result<String, AmountError> {
    let set = |key: &str| entry.get(key).filter(|value| !matches!(value, Json::Null));
    let amount = set("amount");
    let maximum = set("maxAmountRequired");
    if let (Some(amount), Some(maximum)) = (amount, maximum) {
        let (Some(amount), Some(maximum)) = (py_str(amount), py_str(maximum)) else {
            return Err(AmountError::NotPlainInteger);
        };
        if amount != maximum {
            return Err(AmountError::Conflicting);
        }
    }
    let resolved = amount.or(maximum).ok_or(AmountError::Missing)?;
    let text = py_str(resolved).ok_or(AmountError::NotPlainInteger)?;
    base_units(&text)?;
    Ok(text)
}

/// The cap spelling is outside the finite decimal grammar (or D6 bounds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapError;

/// A decimal number as `digits x 10^exponent` (R-AMT-REP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapDecimal {
    negative: bool,
    /// ASCII digits without leading zeros; `"0"` for zero.
    digits: String,
    exponent: i32,
}

impl CapDecimal {
    /// Parse a cap written as a string.
    ///
    /// # Errors
    ///
    /// [`CapError`] for anything outside the finite grammar or the D6 bounds.
    pub fn parse(text: &str) -> Result<Self, CapError> {
        let trimmed = text.trim_matches(is_decimal_space);
        let (negative, body) = match trimmed.as_bytes().first() {
            Some(b'-') => (true, &trimmed[1..]),
            Some(b'+') => (false, &trimmed[1..]),
            _ => (false, trimmed),
        };
        let (mantissa, written_exponent) = match body.find(['e', 'E']) {
            Some(index) => (&body[..index], Some(&body[index + 1..])),
            None => (body, None),
        };
        let exponent = match written_exponent {
            Some(text) => parse_exponent(text)?,
            None => 0,
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let whole = digits_of(whole)?;
        let fraction = digits_of(fraction)?;
        if whole.is_empty() && fraction.is_empty() {
            return Err(CapError);
        }
        if whole.len() + fraction.len() > MAX_CAP_DIGITS {
            return Err(CapError);
        }
        let joined = format!("{whole}{fraction}");
        let significant = joined.trim_start_matches('0');
        let digits = if significant.is_empty() {
            "0".to_owned()
        } else {
            significant.to_owned()
        };
        let fraction_len = i32::try_from(fraction.len()).map_err(|_| CapError)?;
        Ok(Self {
            negative,
            digits,
            exponent: exponent - fraction_len,
        })
    }

    /// Parse a cap from the policy JSON: a string as [`CapDecimal::parse`],
    /// a number through its value (an integer in decimal, a float through its
    /// shortest round-trip digits, which is the value Python's `str()` gives).
    ///
    /// # Errors
    ///
    /// [`CapError`] for any other JSON type or an invalid spelling.
    pub fn from_json(value: &Json) -> Result<Self, CapError> {
        match value {
            Json::String(text) => Self::parse(text),
            Json::Int(number) => Self::parse(&number.to_string()),
            Json::Float(number) if number.is_finite() => Self::parse(&format!("{number:e}")),
            _ => Err(CapError),
        }
    }

    /// The coefficient digits, without leading zeros (`"0"` for zero).
    #[must_use]
    pub fn digits(&self) -> &str {
        &self.digits
    }

    /// The power of ten the digits are scaled by.
    #[must_use]
    pub fn exponent(&self) -> i32 {
        self.exponent
    }

    /// Whether a `-` sign was written.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// Greater than zero (the legacy `cap <= 0` refusal is its negation).
    #[must_use]
    pub fn is_positive(&self) -> bool {
        !self.negative && self.digits != "0"
    }
}

/// `text` with `_` between digits removed; every other character must be an
/// ASCII digit. An empty part stays empty.
fn digits_of(text: &str) -> Result<String, CapError> {
    strip_digit_underscores(text).ok_or(CapError)
}

/// The exponent after `e`: one optional sign and ASCII digits (with `_`
/// between digits), at most [`MAX_CAP_EXPONENT`] in magnitude.
fn parse_exponent(text: &str) -> Result<i32, CapError> {
    let (negative, body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let digits = digits_of(body)?;
    if digits.is_empty() {
        return Err(CapError);
    }
    let significant = digits.trim_start_matches('0');
    if significant.len() > 4 {
        return Err(CapError);
    }
    let magnitude: u32 = if significant.is_empty() {
        0
    } else {
        significant.parse().map_err(|_| CapError)?
    };
    if magnitude > MAX_CAP_EXPONENT {
        return Err(CapError);
    }
    let value = i32::try_from(magnitude).map_err(|_| CapError)?;
    Ok(if negative { -value } else { value })
}

/// Compare two ASCII digit strings as integers.
fn compare_digits(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

/// Whether `units` base units are above `cap` dollars (R-AMT-CMP). Equal is
/// allowed. `units` must already satisfy [`base_units`]; a cap that is not
/// positive is exceeded by everything (fail closed).
#[must_use]
pub fn exceeds_cap(units: &str, cap: &CapDecimal) -> bool {
    if !cap.is_positive() {
        return true;
    }
    let shift = i64::from(cap.exponent) + i64::from(USDC_DECIMALS);
    // |shift| <= 2006 by the D6 bounds, so the padding is small.
    let pad = "0".repeat(usize::try_from(shift.unsigned_abs()).unwrap_or(0));
    let ordering = if shift >= 0 {
        compare_digits(units, &format!("{}{pad}", cap.digits))
    } else {
        compare_digits(&format!("{units}{pad}"), &cap.digits)
    };
    ordering == Ordering::Greater
}

/// `str(Decimal(units) / Decimal(10) ** 6)` under Python's default context:
/// an exact quotient takes the exponent closest to 0 that keeps it exact,
/// a quotient past 28 significant digits is rounded half-even, and the text
/// is `Decimal.__str__` of the result.
#[must_use]
pub fn usd_text(units: &str) -> String {
    let significant = units.trim_start_matches('0');
    if significant.is_empty() {
        return "0".to_owned();
    }
    let mut digits = significant.to_owned();
    let mut exponent = -i64::from(USDC_DECIMALS);
    while exponent < 0 && digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
        exponent += 1;
    }
    if digits.len() > DECIMAL_PRECISION {
        let dropped = digits.len() - DECIMAL_PRECISION;
        let (kept, rest) = digits.split_at(DECIMAL_PRECISION);
        let mut kept = kept.to_owned();
        if rounds_up(&kept, rest) {
            kept = increment(&kept);
        }
        exponent += i64::try_from(dropped).unwrap_or(i64::MAX);
        if kept.len() > DECIMAL_PRECISION {
            kept.pop();
            exponent += 1;
        }
        digits = kept;
    }
    decimal_str(&digits, exponent)
}

/// Round-half-even decision for dropping `rest` after `kept`.
fn rounds_up(kept: &str, rest: &str) -> bool {
    let mut rest_digits = rest.bytes();
    match rest_digits.next() {
        Some(b'6'..=b'9') => true,
        Some(b'5') => {
            rest_digits.any(|b| b != b'0')
                || kept
                    .as_bytes()
                    .last()
                    .is_some_and(|last| (last - b'0') % 2 == 1)
        }
        _ => false,
    }
}

/// `digits + 1` as a digit string (one longer on carry out).
fn increment(digits: &str) -> String {
    let mut bytes = digits.as_bytes().to_vec();
    for byte in bytes.iter_mut().rev() {
        if *byte == b'9' {
            *byte = b'0';
        } else {
            *byte += 1;
            return String::from_utf8(bytes).unwrap_or_default();
        }
    }
    let mut out = String::with_capacity(bytes.len() + 1);
    out.push('1');
    out.push_str(&String::from_utf8(bytes).unwrap_or_default());
    out
}

/// Python `Decimal.__str__` (to-scientific-string) of `digits x 10^exponent`.
fn decimal_str(digits: &str, exponent: i64) -> String {
    let count = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let adjusted = exponent + count - 1;
    if exponent <= 0 && adjusted >= -6 {
        if exponent == 0 {
            return digits.to_owned();
        }
        let point = count + exponent;
        if point > 0 {
            let (whole, fraction) = digits.split_at(usize::try_from(point).unwrap_or(0));
            return format!("{whole}.{fraction}");
        }
        let zeros = "0".repeat(usize::try_from(-point).unwrap_or(0));
        return format!("0.{zeros}{digits}");
    }
    let (first, rest) = digits.split_at(1);
    let mantissa = if rest.is_empty() {
        first.to_owned()
    } else {
        format!("{first}.{rest}")
    };
    let sign = if adjusted < 0 { '-' } else { '+' };
    format!("{mantissa}E{sign}{}", adjusted.unsigned_abs())
}
