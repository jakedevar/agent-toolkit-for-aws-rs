//! Python-compatible text and number formatting.
//!
//! The legacy MSK scripts print figures with Python's `str(float)`,
//! `{:,.2f}`, `{:g}`, `repr()` and `json.dumps(indent=2)`. Reports and JSON
//! documents are customer-facing and compared byte for byte, so this module
//! reproduces those renderings (round-half-even on the exact binary value,
//! shortest round-trip floats, ASCII-only JSON). Everything here is pure.

use std::fmt::Write as _;

use toolkit_core::Json;

/// Python `repr(float)`: the shortest round-trip digits, positional notation
/// for decimal exponents in `[-4, 16)` and `d.ddde+XX` outside it.
#[must_use]
pub fn py_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return String::from(if value > 0.0 { "inf" } else { "-inf" });
    }
    let scientific = format!("{value:e}");
    let (negative, unsigned) = match scientific.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, scientific.as_str()),
    };
    let (mantissa, exponent_text) = unsigned.split_once('e').unwrap_or((unsigned, "0"));
    let exponent: i32 = exponent_text.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = if (-4..16).contains(&exponent) {
        if exponent >= 0 {
            let integer_len = usize::try_from(exponent).unwrap_or(0) + 1;
            if digits.len() <= integer_len {
                format!("{digits}{}.0", "0".repeat(integer_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..integer_len], &digits[integer_len..])
            }
        } else {
            let zeros = usize::try_from(-exponent - 1).unwrap_or(0);
            format!("0.{}{digits}", "0".repeat(zeros))
        }
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        let magnitude = exponent.unsigned_abs();
        if digits.len() == 1 {
            format!("{digits}e{sign}{magnitude:02}")
        } else {
            format!("{}.{}e{sign}{magnitude:02}", &digits[..1], &digits[1..])
        }
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

fn strip_trailing_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text.to_string()
    }
}

/// Python `format(value, "g")` (six significant digits, trailing zeros removed).
#[must_use]
pub fn format_g(value: f64) -> String {
    if !value.is_finite() {
        return py_float_repr(value);
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent_text) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let exponent: i32 = exponent_text.parse().unwrap_or(0);
    if (-4..6).contains(&exponent) {
        let decimals = usize::try_from(5 - exponent).unwrap_or(0);
        strip_trailing_zeros(&format!("{value:.decimals$}"))
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        let magnitude = exponent.unsigned_abs();
        format!("{}e{sign}{magnitude:02}", strip_trailing_zeros(mantissa))
    }
}

fn group_thousands(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Python `format(value, ",.Nf")` when `grouped`, `".Nf"` otherwise.
#[must_use]
pub fn format_fixed(value: f64, decimals: usize, grouped: bool) -> String {
    if !value.is_finite() {
        return py_float_repr(value);
    }
    let raw = format!("{value:.decimals$}");
    let (sign, unsigned) = match raw.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", raw.as_str()),
    };
    let (integer, fraction) = match unsigned.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (unsigned, None),
    };
    let integer = if grouped {
        group_thousands(integer)
    } else {
        integer.to_string()
    };
    match fraction {
        Some(fraction) => format!("{sign}{integer}.{fraction}"),
        None => format!("{sign}{integer}"),
    }
}

/// Python `format(value, ",")` for an integer.
#[must_use]
pub fn format_int_grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let grouped = group_thousands(&digits);
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// Left-aligns `text` in a field of `width` characters (`{:<width}`).
#[must_use]
pub fn pad_right(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(len)))
}

/// Right-aligns `text` in a field of `width` characters (`{:>width}`).
#[must_use]
pub fn pad_left(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{}{text}", " ".repeat(width.saturating_sub(len)))
}

/// Python `round(value, decimals)`: round-half-even on the exact binary value.
#[must_use]
pub fn round_to(value: f64, decimals: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{value:.decimals$}").parse().unwrap_or(value)
}

/// Python `int(text)` for a decimal string: surrounding whitespace, an
/// optional sign and single underscores between digits are accepted.
#[must_use]
pub fn py_int_parse(text: &str) -> Option<i128> {
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(10)?;
        value = value.checked_mul(10)?.checked_add(i128::from(digit))?;
    }
    Some(if negative { -value } else { value })
}

/// Python `float(text)`: surrounding whitespace and single underscores between
/// digits are accepted. `nan` and `inf` parse; callers decide whether they are
/// acceptable.
#[must_use]
pub fn py_float_parse(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.contains('_') {
        let chars: Vec<char> = trimmed.chars().collect();
        for (index, c) in chars.iter().enumerate() {
            if *c == '_' {
                let before = index.checked_sub(1).and_then(|i| chars.get(i));
                let after = chars.get(index + 1);
                let digit = |c: Option<&char>| c.is_some_and(char::is_ascii_digit);
                if !digit(before) || !digit(after) {
                    return None;
                }
            }
        }
        return trimmed.replace('_', "").parse().ok();
    }
    trimmed.parse().ok()
}

/// Python `repr(str)`.
#[must_use]
pub fn py_str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if u32::from(c) < 0x20
                || u32::from(c) == 0x7f
                || (0x80..=0xa0).contains(&u32::from(c))
                || u32::from(c) == 0xad =>
            {
                let _ = write!(out, "\\x{:02x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr()` of the Python value a JSON document loads as.
#[must_use]
pub fn py_repr(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(number) => number.to_string(),
        Json::Float(number) => py_float_repr(*number),
        Json::String(text) => py_str_repr(text),
        Json::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Json::Object(pairs) => {
            let inner: Vec<String> = pairs
                .iter()
                .map(|(key, item)| format!("{}: {}", py_str_repr(key), py_repr(item)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python `str()` of the Python value a JSON document loads as.
#[must_use]
pub fn py_str(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// Escapes every non-ASCII character as `\uXXXX` (surrogate pairs above the
/// BMP), like `json.dumps(ensure_ascii=True)`. Applied to serialized JSON,
/// where non-ASCII characters only occur inside strings.
#[must_use]
pub fn ascii_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buffer = [0_u16; 2];
            for unit in c.encode_utf16(&mut buffer).iter() {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// `json.dumps(value, indent=2)`: two-space pretty printing, ASCII only.
#[must_use]
pub fn json_dumps(value: &Json) -> String {
    ascii_escape(&value.to_pretty_string())
}

/// `json.dumps({"error": message})` with Python's default separators.
#[must_use]
pub fn error_json(message: &str) -> String {
    ascii_escape(&format!(
        "{{\"error\": \"{}\"}}",
        toolkit_core::escape_json_string(message)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (10.0, "10.0"),
            (0.5, "0.5"),
            (187.2, "187.2"),
            (1499.0, "1499.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (123_456_789_012_345.0, "123456789012345.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.25e20, "1.25e+20"),
            (1.5e300, "1.5e+300"),
            (-22.5, "-22.5"),
            (0.1 + 0.2, "0.30000000000000004"),
        ];
        for (value, expected) in cases {
            assert_eq!(py_float_repr(value), expected, "{value:?}");
        }
        assert_eq!(py_float_repr(f64::INFINITY), "inf");
        assert_eq!(py_float_repr(f64::NEG_INFINITY), "-inf");
        assert_eq!(py_float_repr(f64::NAN), "nan");
    }

    #[test]
    fn general_format_matches_python() {
        let cases = [
            (0.0, "0"),
            (15.0, "15"),
            (12.5, "12.5"),
            (2.0, "2"),
            (0.01, "0.01"),
            (6.73, "6.73"),
            (0.000_123_456_789, "0.000123457"),
            (0.00001, "1e-05"),
            (1_234_567.0, "1.23457e+06"),
            (100_000.0, "100000"),
            (999_999.0, "999999"),
            (0.204, "0.204"),
            (13.056, "13.056"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_g(value), expected, "{value:?}");
        }
    }

    #[test]
    fn fixed_format_groups_and_rounds_half_even() {
        assert_eq!(format_fixed(1_234_567.891, 2, true), "1,234,567.89");
        assert_eq!(format_fixed(999.0, 2, true), "999.00");
        assert_eq!(format_fixed(1000.0, 2, true), "1,000.00");
        assert_eq!(format_fixed(0.125, 2, true), "0.12");
        assert_eq!(format_fixed(0.375, 2, true), "0.38");
        assert_eq!(format_fixed(-1234.5, 1, true), "-1,234.5");
        assert_eq!(format_fixed(5.0, 0, false), "5");
        assert_eq!(format_fixed(2.5, 0, false), "2");
        assert_eq!(format_int_grouped(1_234_567), "1,234,567");
        assert_eq!(format_int_grouped(-1000), "-1,000");
        assert_eq!(format_int_grouped(12), "12");
    }

    #[test]
    fn padding_counts_characters() {
        assert_eq!(pad_right("ab", 4), "ab  ");
        assert_eq!(pad_left("ab", 4), "  ab");
        assert_eq!(pad_left("abcd", 2), "abcd");
        assert_eq!(pad_right("é", 3), "é  ");
    }

    #[test]
    fn round_to_is_half_even_on_binary_value() {
        assert_eq!(round_to(0.125, 2), 0.12);
        assert_eq!(round_to(2.675, 2), 2.67);
        assert_eq!(round_to(187.2, 1), 187.2);
        assert_eq!(round_to(22.46, 1), 22.5);
    }

    #[test]
    fn int_parse_follows_python_int() {
        assert_eq!(py_int_parse("12"), Some(12));
        assert_eq!(py_int_parse(" -12 "), Some(-12));
        assert_eq!(py_int_parse("+7"), Some(7));
        assert_eq!(py_int_parse("1_000"), Some(1000));
        assert_eq!(py_int_parse("007"), Some(7));
        for bad in ["", "-", "1__0", "_1", "1_", "1.5", "0x10", "1e3", "abc"] {
            assert_eq!(py_int_parse(bad), None, "{bad:?}");
        }
        assert_eq!(py_int_parse(&"9".repeat(60)), None);
    }

    #[test]
    fn float_parse_follows_python_float() {
        assert_eq!(py_float_parse("1.5"), Some(1.5));
        assert_eq!(py_float_parse(" 2 "), Some(2.0));
        assert_eq!(py_float_parse("1_0.5"), Some(10.5));
        assert_eq!(py_float_parse("1e3"), Some(1000.0));
        assert!(py_float_parse("nan").is_some_and(f64::is_nan));
        for bad in ["", "abc", "1_", "_1", "1__0", "1._5", "0x1"] {
            assert_eq!(py_float_parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn repr_matches_python_for_json_values() {
        assert_eq!(py_str_repr("abc"), "'abc'");
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_str_repr("a\\b\n"), "'a\\\\b\\n'");
        assert_eq!(py_str_repr("\u{1}\u{7f}"), "'\\x01\\x7f'");
        assert_eq!(py_str_repr("é—"), "'é—'");
        let value = Json::object(vec![
            ("a", Json::Int(1)),
            ("b", Json::Array(vec![Json::Null, Json::Bool(true)])),
            ("c", Json::Float(2.0)),
        ]);
        assert_eq!(py_repr(&value), "{'a': 1, 'b': [None, True], 'c': 2.0}");
        assert_eq!(py_repr(&Json::Bool(false)), "False");
        assert_eq!(py_str(&Json::string("x")), "x");
        assert_eq!(py_str(&Json::Null), "None");
        assert_eq!(py_str(&Json::Int(-3)), "-3");
    }

    #[test]
    fn json_dumps_is_ascii_and_indented() {
        let value = Json::object(vec![
            ("text", Json::string("a — b é 😀")),
            ("list", Json::Array(vec![Json::Int(1)])),
            ("empty", Json::Array(vec![])),
        ]);
        assert_eq!(
            json_dumps(&value),
            "{\n  \"text\": \"a \\u2014 b \\u00e9 \\ud83d\\ude00\",\n  \"list\": [\n    1\n  ],\n  \"empty\": []\n}"
        );
        assert_eq!(
            error_json("bad \"x\" —"),
            "{\"error\": \"bad \\\"x\\\" \\u2014\"}"
        );
    }
}
