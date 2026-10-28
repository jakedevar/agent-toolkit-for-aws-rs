//! Python-compatible numbers, text and JSON for the benchmark slice (#4390).
//!
//! `benchmark_model.py` and `generate_perf_report.py` print figures with
//! `f"{x:.3f}"`, `f"{x:+.1f}"`, `f"{x:.2g}"`, `f"{x:.0%}"`, `sum()` and
//! `json.dumps(indent=2)`. Reports and summaries are compared byte for byte, so
//! this module reproduces exactly those renderings, plus the diagnostics rule
//! of the migration (caller text is redacted, then truncated, then escaped).

use std::fmt::Write as _;

use toolkit_aws::redact_text;
use toolkit_core::diag::DEFAULT_MAX_CHARS;
use toolkit_core::{quote_untrusted_bounded, Json, ToolkitError};

use super::pyfmt;

/// A design, config or summary problem: exit status 2, like the script's `_die`.
pub fn die(message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime("dynamodb_bench_invalid", message).with_exit_code(2)
}

/// Caller-derived text for a diagnostic: redacted first, then truncated, then
/// escaped to one quoted line (`toolkit_aws::redact_text` and
/// `toolkit_core::quote_untrusted_bounded`, in that order).
#[must_use]
pub fn diag(text: &str) -> String {
    diag_bounded(text, DEFAULT_MAX_CHARS)
}

/// [`diag`] with an explicit character bound.
#[must_use]
pub fn diag_bounded(text: &str, max_chars: usize) -> String {
    quote_untrusted_bounded(&redact_text(text), max_chars)
}

/// `[` + each item through [`diag`] + `]`, the shape of a Python list of names.
#[must_use]
pub fn diag_list<S: AsRef<str>>(items: &[S]) -> String {
    let parts: Vec<String> = items.iter().map(|item| diag(item.as_ref())).collect();
    format!("[{}]", parts.join(", "))
}

/// A JSON number kept as Python kept it: an `int` stays exact, a `float` is a
/// float. `int * int` is an `int`, anything else a `float`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Num {
    /// A Python `int`.
    Int(i64),
    /// A Python `float`.
    Float(f64),
}

impl Num {
    /// The value as a float.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(value) => value as f64,
            Self::Float(value) => value,
        }
    }

    /// A JSON number (`bool`, strings and the rest are not numbers here).
    #[must_use]
    pub fn from_json(value: &Json) -> Option<Self> {
        match value {
            Json::Int(number) => Some(Self::Int(*number)),
            Json::Float(number) => Some(Self::Float(*number)),
            _ => None,
        }
    }

    /// The JSON value.
    #[must_use]
    pub fn to_json(self) -> Json {
        match self {
            Self::Int(value) => Json::Int(value),
            Self::Float(value) => Json::Float(value),
        }
    }

    /// `a * b` with Python's int/float rule.
    #[must_use]
    pub fn times(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_mul(b)
                .map_or_else(|| Self::Float(self.as_f64() * other.as_f64()), Self::Int),
            _ => Self::Float(self.as_f64() * other.as_f64()),
        }
    }

    /// `str(value)`.
    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Int(value) => value.to_string(),
            Self::Float(value) => pyfmt::float_repr(value),
        }
    }

    /// `format(value, ",")`.
    #[must_use]
    pub fn commas(self) -> String {
        match self {
            Self::Int(value) => toolkit_msk::pyfmt::format_int_grouped(value),
            Self::Float(value) => {
                // `format(float, ",")` is the shortest repr with grouped integer digits.
                let repr = pyfmt::float_repr(value);
                group_repr(&repr)
            }
        }
    }
}

/// Groups the integer digits of a float `repr` (`1000.5` becomes `1,000.5`);
/// an exponent form is left alone.
fn group_repr(repr: &str) -> String {
    if repr.contains('e') || repr.contains("inf") || repr.contains("nan") {
        return repr.to_string();
    }
    let (sign, unsigned) = match repr.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", repr),
    };
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let mut grouped = String::new();
    for (index, digit) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

fn non_finite(value: f64) -> Option<String> {
    if value.is_nan() {
        Some("nan".to_string())
    } else if value.is_infinite() {
        Some(if value < 0.0 { "-inf" } else { "inf" }.to_string())
    } else {
        None
    }
}

/// `format(value, ".Nf")`.
#[must_use]
pub fn fixed(value: f64, decimals: usize) -> String {
    non_finite(value).unwrap_or_else(|| format!("{value:.decimals$}"))
}

/// `format(value, "+.Nf")`.
#[must_use]
pub fn signed(value: f64, decimals: usize) -> String {
    non_finite(value).map_or_else(
        || format!("{value:+.decimals$}"),
        |text| {
            if value.is_sign_negative() {
                text
            } else {
                format!("+{text}")
            }
        },
    )
}

/// `format(value, ".0%")`-style: `value * 100`, then `decimals` fixed digits.
#[must_use]
pub fn percent(value: f64, decimals: usize) -> String {
    format!("{}%", fixed(value * 100.0, decimals))
}

/// `format(value, ",.Nf")`.
#[must_use]
pub fn commas(value: f64, decimals: usize) -> String {
    non_finite(value).unwrap_or_else(|| pyfmt::commas(value, decimals))
}

/// `format(value, ".{precision}g")`: `precision` significant digits, trailing
/// zeros removed, exponent form from `10**precision` (so `1500.0` with
/// precision 2 is `1.5e+03`, as in Python).
#[must_use]
pub fn general(value: f64, precision: usize) -> String {
    if let Some(text) = non_finite(value) {
        return text;
    }
    let precision = precision.max(1);
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let scientific = format!("{value:.digits$e}", digits = precision - 1);
    let (mantissa, exponent_text) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let exponent: i32 = exponent_text.parse().unwrap_or(0);
    let strip = |text: &str| -> String {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            text.to_string()
        }
    };
    let limit = i32::try_from(precision).unwrap_or(i32::MAX);
    if (-4..limit).contains(&exponent) {
        let decimals = usize::try_from(limit - 1 - exponent).unwrap_or(0);
        strip(&format!("{value:.decimals$}"))
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", strip(mantissa), exponent.abs())
    }
}

/// Python's builtin `sum()` over floats: Neumaier-compensated since CPython
/// 3.12 (earlier interpreters add left to right; the results can differ in the
/// last bit, see the migration document).
#[must_use]
pub fn py_sum(values: &[f64]) -> f64 {
    let mut total = 0.0_f64;
    let mut carry = 0.0_f64;
    for &x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            carry += (total - t) + x;
        } else {
            carry += (x - t) + total;
        }
        total = t;
    }
    if carry != 0.0 && carry.is_finite() {
        total += carry;
    }
    total
}

/// Python `a // b` for floats (floor division with CPython's correction).
#[must_use]
pub fn py_floordiv(a: f64, b: f64) -> f64 {
    let modulo = a % b;
    let mut div = (a - modulo) / b;
    if modulo != 0.0 && ((b < 0.0) != (modulo < 0.0)) {
        div -= 1.0;
    }
    if div != 0.0 {
        let floor = div.floor();
        if div - floor > 0.5 {
            floor + 1.0
        } else {
            floor
        }
    } else if (a / b).is_sign_negative() {
        -0.0
    } else {
        0.0
    }
}

/// `max(a, b)` with Python's rule (the first argument wins a tie).
#[must_use]
pub fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// `min(a, b)` with Python's rule (the first argument wins a tie).
#[must_use]
pub fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// Counts that keep first-seen order, like a Python `dict` of tallies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally(pub Vec<(String, i64)>);

impl Tally {
    /// Adds `count` to `key`, keeping the position of an existing key.
    pub fn add(&mut self, key: &str, count: i64) {
        if let Some(slot) = self.0.iter_mut().find(|(name, _)| name == key) {
            slot.1 += count;
        } else {
            self.0.push((key.to_string(), count));
        }
    }

    /// Whether nothing was counted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `max(d, key=d.get)`: the first key with the largest count.
    #[must_use]
    pub fn top(&self) -> Option<&str> {
        let mut best: Option<&(String, i64)> = None;
        for entry in &self.0 {
            if best.is_none_or(|current| entry.1 > current.1) {
                best = Some(entry);
            }
        }
        best.map(|(name, _)| name.as_str())
    }

    /// The tally as a JSON object.
    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::Object(
            self.0
                .iter()
                .map(|(name, count)| (name.clone(), Json::Int(*count)))
                .collect(),
        )
    }
}

/// `json.dumps(value, indent=2)` (ASCII output, Python float `repr`).
#[must_use]
pub fn dumps_indent(value: &Json) -> String {
    let mut out = String::new();
    write_indented(&mut out, value, 0);
    out
}

fn push_pad(out: &mut String, level: usize) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_indented(out: &mut String, value: &Json, level: usize) {
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_pad(out, level + 1);
                write_indented(out, item, level + 1);
            }
            push_pad(out, level);
            out.push(']');
        }
        Json::Object(pairs) if !pairs.is_empty() => {
            out.push('{');
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_pad(out, level + 1);
                out.push_str(&pyfmt::dumps(&Json::String(key.clone()), false));
                out.push_str(": ");
                write_indented(out, item, level + 1);
            }
            push_pad(out, level);
            out.push('}');
        }
        other => {
            let _ = write!(out, "{}", pyfmt::dumps(other, false));
        }
    }
}

/// The `datetime.isoformat()` of a UTC instant: microseconds only when set.
#[must_use]
pub fn isoformat_utc(seconds: i64, micros: u32) -> String {
    let base = super::iterate::utc_iso(seconds);
    let stem = base.trim_end_matches('Z');
    if micros == 0 {
        format!("{stem}+00:00")
    } else {
        format!("{stem}.{micros:06}+00:00")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_format_matches_python() {
        assert_eq!(general(1.0, 2), "1");
        assert_eq!(general(5.0, 2), "5");
        assert_eq!(general(0.01, 2), "0.01");
        assert_eq!(general(1500.0, 2), "1.5e+03");
        assert_eq!(general(1800.0, 2), "1.8e+03");
        assert_eq!(general(50.0, 2), "50");
        assert_eq!(general(100.0, 2), "1e+02");
        assert_eq!(general(2.5, 2), "2.5");
        assert_eq!(general(0.125, 2), "0.12");
        assert_eq!(general(99.5, 2), "1e+02");
        assert_eq!(general(0.00001234, 2), "1.2e-05");
        assert_eq!(general(0.0, 2), "0");
    }

    #[test]
    fn signed_percent_and_fixed_follow_python() {
        assert_eq!(signed(5.04, 1), "+5.0");
        assert_eq!(signed(-0.04, 1), "-0.0");
        assert_eq!(signed(0.0, 1), "+0.0");
        assert_eq!(percent(0.5, 0), "50%");
        assert_eq!(percent(0.125, 0), "12%");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(commas(1234567.891, 2), "1,234,567.89");
        assert_eq!(fixed(f64::INFINITY, 2), "inf");
    }

    #[test]
    fn numbers_keep_the_python_int_float_split() {
        assert_eq!(Num::Int(3).times(Num::Int(4)), Num::Int(12));
        assert_eq!(Num::Int(3).times(Num::Float(0.5)), Num::Float(1.5));
        assert_eq!(Num::Int(1000).commas(), "1,000");
        assert_eq!(Num::Float(1000.5).commas(), "1,000.5");
        assert_eq!(Num::Float(100.0).text(), "100.0");
        assert_eq!(Num::Int(-1234).commas(), "-1,234");
    }

    #[test]
    fn neumaier_sum_matches_cpython() {
        // 0.1 added ten times is 0.9999999999999999 naively and 1.0 compensated.
        let tenths = vec![0.1; 10];
        assert_eq!(py_sum(&tenths), 1.0);
        assert_eq!(py_sum(&[]), 0.0);
        assert_eq!(py_sum(&[1.0, 1e100, 1.0, -1e100]), 2.0);
    }

    #[test]
    fn float_floor_division_is_exact() {
        assert_eq!(py_floordiv(810.0, 3.0), 270.0);
        assert_eq!(py_floordiv(7.0, 2.0), 3.0);
        assert_eq!(py_floordiv(0.3, 0.1), 2.0);
        assert_eq!(py_floordiv(-7.0, 2.0), -4.0);
    }

    #[test]
    fn tally_keeps_first_seen_order_and_first_maximum() {
        let mut tally = Tally::default();
        tally.add("b", 2);
        tally.add("a", 2);
        tally.add("b", 0);
        assert_eq!(tally.top(), Some("b"));
        assert_eq!(tally.to_json().to_compact_string(), r#"{"b":2,"a":2}"#);
    }

    #[test]
    fn indented_dumps_follows_json_dumps() {
        let value = Json::object(vec![
            ("a", Json::Array(vec![Json::Int(1), Json::Float(1e16)])),
            ("b", Json::Object(Vec::new())),
            ("c", Json::string("é")),
            ("d", Json::Array(Vec::new())),
        ]);
        assert_eq!(
            dumps_indent(&value),
            "{\n  \"a\": [\n    1,\n    1e+16\n  ],\n  \"b\": {},\n  \"c\": \"\\u00e9\",\n  \"d\": []\n}"
        );
    }

    #[test]
    fn isoformat_has_microseconds_only_when_set() {
        assert_eq!(isoformat_utc(0, 0), "1970-01-01T00:00:00+00:00");
        assert_eq!(isoformat_utc(1, 5), "1970-01-01T00:00:01.000005+00:00");
    }

    #[test]
    fn diagnostics_redact_then_escape() {
        let shown = diag("AKIAIOSFODNN7EXAMPLE\nforged: line");
        assert!(!shown.contains("AKIAIOSFODNN7EXAMPLE"), "{shown}");
        assert!(!shown.contains('\n'));
        assert!(shown.starts_with('"') && shown.ends_with('"'));
    }
}
