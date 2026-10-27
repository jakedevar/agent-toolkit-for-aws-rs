//! Python-compatible text formatting for the DynamoDB slice.
//!
//! The legacy scripts print values with Python's `repr`/`str`, `round`,
//! `format(x, ",.2f")` and `json.dumps`. Those renderings reach published
//! output (the cost report, error messages, the loop summary) and the schema
//! fingerprint, so this module reproduces them exactly for the JSON value
//! space. Nothing here is a general Python emulation.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use toolkit_core::Json;

/// Python truthiness of a JSON value.
pub fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Int(number) => *number != 0,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(pairs) => !pairs.is_empty(),
    }
}

/// `repr(float)`: shortest round-trip digits, exponent form only below
/// `1e-4` or from `1e16`, two-digit signed exponent.
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        let word = if value < 0.0 { "-inf" } else { "inf" };
        return word.to_string();
    }
    if value == 0.0 {
        let word = if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        };
        return word.to_string();
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if (-4..16).contains(&exponent) {
        let mut out = String::from(sign);
        if exponent >= 0 {
            let int_len = usize::try_from(exponent).unwrap_or(0) + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                out.push_str(&"0".repeat(int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat(usize::try_from(-exponent - 1).unwrap_or(0)));
            out.push_str(&digits);
        }
        out
    } else {
        let mut out = String::from(sign);
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(
            out,
            "e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        );
        out
    }
}

fn string_repr(text: &str) -> String {
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
            c if u32::from(c) < 0x20 || (0x7f..=0x9f).contains(&u32::from(c)) => {
                let _ = write!(out, "\\x{:02x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr(value)` for a JSON value (`None`, `True`, `'text'`, `[1, 2]`,
/// `{'k': 1}`).
pub fn repr(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(number) => number.to_string(),
        Json::Float(number) => float_repr(*number),
        Json::String(text) => string_repr(text),
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Json::Object(pairs) => {
            let parts: Vec<String> = pairs
                .iter()
                .map(|(key, item)| format!("{}: {}", string_repr(key), repr(item)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `str(value)`: a string is shown bare, everything else as its `repr`.
pub fn text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => repr(other),
    }
}

/// `repr` of a list of strings.
pub fn repr_strings(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|item| string_repr(item)).collect();
    format!("[{}]", parts.join(", "))
}

/// `repr` of a single string.
pub fn repr_str(item: &str) -> String {
    string_repr(item)
}

/// Python `round(value, digits)` for floats: correctly rounded decimal, ties
/// to even on the exact binary value.
pub fn round(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}").parse().unwrap_or(value)
}

/// `format(value, f",.{decimals}f")`: fixed decimals with thousands separators.
pub fn commas(value: f64, decimals: usize) -> String {
    let fixed = format!("{value:.decimals$}");
    let (sign, unsigned) = match fixed.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", fixed.as_str()),
    };
    let (int_part, fraction) = match unsigned.split_once('.') {
        Some((int_part, fraction)) => (int_part, Some(fraction)),
        None => (unsigned, None),
    };
    let mut grouped = String::new();
    for (index, digit) in int_part.chars().enumerate() {
        if index > 0 && (int_part.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

/// `format(value, ",")` for an integer-valued number.
pub fn int_commas(value: f64) -> String {
    commas(value.trunc(), 0)
}

/// A path as Python's `str(Path(...))` shows it: no `.` components, no
/// repeated separators, no trailing separator.
pub fn normalized_path(path: &Path) -> String {
    let rebuilt: PathBuf = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    if rebuilt.as_os_str().is_empty() && !path.as_os_str().is_empty() {
        return ".".to_string();
    }
    rebuilt.display().to_string()
}

fn push_ascii_json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) || c == '\u{7f}' => out.push(c),
            c => {
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// `json.dumps(value, sort_keys=...)` with Python's default separators
/// (`", "` and `": "`) and `ensure_ascii=True`.
pub fn dumps(value: &Json, sort_keys: bool) -> String {
    let mut out = String::new();
    write_dumps(&mut out, value, sort_keys);
    out
}

fn write_dumps(out: &mut String, value: &Json, sort_keys: bool) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) if number.is_nan() => out.push_str("NaN"),
        Json::Float(number) if number.is_infinite() => {
            out.push_str(if *number < 0.0 {
                "-Infinity"
            } else {
                "Infinity"
            });
        }
        Json::Float(number) => out.push_str(&float_repr(*number)),
        Json::String(text) => push_ascii_json_string(out, text),
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_dumps(out, item, sort_keys);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            let mut ordered: Vec<&(String, Json)> = pairs.iter().collect();
            if sort_keys {
                ordered.sort_by(|left, right| left.0.cmp(&right.0));
            }
            out.push('{');
            for (index, (key, item)) in ordered.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                push_ascii_json_string(out, key);
                out.push_str(": ");
                write_dumps(out, item, sort_keys);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    fn json(text: &str) -> Json {
        parse_json(text).expect("valid fixture json")
    }

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (0.1, "0.1"),
            (1.0, "1.0"),
            (100.0, "100.0"),
            (1234.5, "1234.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e300, "1.5e+300"),
            (-2.5, "-2.5"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (123_456_789.123, "123456789.123"),
            (2.0f64.powi(60), "1.152921504606847e+18"),
        ];
        for (value, expected) in cases {
            assert_eq!(float_repr(value), expected, "{value:e}");
        }
        assert_eq!(float_repr(f64::NAN), "nan");
        assert_eq!(float_repr(f64::INFINITY), "inf");
        assert_eq!(float_repr(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn repr_and_str_follow_python() {
        assert_eq!(repr(&json("null")), "None");
        assert_eq!(repr(&json("true")), "True");
        assert_eq!(repr(&json("false")), "False");
        assert_eq!(repr(&json("[1, 2.5, \"a\"]")), "[1, 2.5, 'a']");
        assert_eq!(repr(&json("{\"k\": [null]}")), "{'k': [None]}");
        assert_eq!(repr(&json("\"it's\"")), "\"it's\"");
        assert_eq!(
            repr(&json("\"say \\\"hi\\\" it's\"")),
            "'say \"hi\" it\\'s'"
        );
        assert_eq!(
            repr(&json("\"a\\\\b\\nc\\td\\u0001\"")),
            "'a\\\\b\\nc\\td\\x01'"
        );
        assert_eq!(text(&json("\"bare\"")), "bare");
        assert_eq!(text(&json("[\"x\"]")), "['x']");
        assert_eq!(text(&json("null")), "None");
        assert_eq!(
            repr_strings(&["a".to_string(), "b".to_string()]),
            "['a', 'b']"
        );
        assert_eq!(repr_str("1k"), "'1k'");
    }

    #[test]
    fn truthiness_matches_python() {
        for (source, expected) in [
            ("null", false),
            ("false", false),
            ("true", true),
            ("0", false),
            ("0.0", false),
            ("2", true),
            ("\"\"", false),
            ("\"x\"", true),
            ("[]", false),
            ("[0]", true),
            ("{}", false),
            ("{\"a\": 1}", true),
        ] {
            assert_eq!(truthy(&json(source)), expected, "{source}");
        }
    }

    #[test]
    fn rounding_and_grouping_follow_python() {
        assert_eq!(round(2.675, 2), 2.67, "binary 2.675 is below the tie");
        assert_eq!(round(0.125, 2), 0.12, "exact ties go to even");
        assert_eq!(round(0.375, 2), 0.38);
        assert_eq!(round(1.005, 2), 1.0);
        assert_eq!(round(-1.25, 1), -1.2);
        assert_eq!(format!("{:.2}", 0.125), "0.12");
        assert_eq!(format!("{:.0}", 2.5), "2");
        assert_eq!(commas(1_234_567.891, 2), "1,234,567.89");
        assert_eq!(commas(999.5, 0), "1,000");
        assert_eq!(commas(1000.0, 1), "1,000.0");
        assert_eq!(commas(-12_345.6, 1), "-12,345.6");
        assert_eq!(commas(0.5, 2), "0.50");
        assert_eq!(int_commas(4105.0), "4,105");
        assert_eq!(int_commas(1_000_000.0), "1,000,000");
        assert_eq!(int_commas(12.0), "12");
    }

    #[test]
    fn paths_print_like_python() {
        assert_eq!(normalized_path(Path::new("./a//b/")), "a/b");
        assert_eq!(normalized_path(Path::new("x.json")), "x.json");
        assert_eq!(normalized_path(Path::new("/a/./b")), "/a/b");
        assert_eq!(normalized_path(Path::new("")), "");
        assert_eq!(normalized_path(Path::new(".")), ".");
        assert_eq!(normalized_path(Path::new("././x")), "x");
    }

    #[test]
    fn dumps_uses_python_separators_ascii_escapes_and_sorting() {
        let value = json("{\"b\": [1, 2.0, null, true], \"a\": \"é😀\\n\\u007f\"}");
        assert_eq!(
            dumps(&value, false),
            "{\"b\": [1, 2.0, null, true], \"a\": \"\\u00e9\\ud83d\\ude00\\n\u{7f}\"}"
        );
        assert_eq!(
            dumps(&value, true),
            "{\"a\": \"\\u00e9\\ud83d\\ude00\\n\u{7f}\", \"b\": [1, 2.0, null, true]}"
        );
        assert_eq!(dumps(&json("[]"), false), "[]");
        assert_eq!(dumps(&json("{}"), true), "{}");
        assert_eq!(dumps(&Json::Float(f64::NAN), false), "NaN");
        assert_eq!(dumps(&Json::Float(f64::NEG_INFINITY), false), "-Infinity");
    }
}
