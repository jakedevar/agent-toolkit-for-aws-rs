//! Closed types for bare slots and consent assignments (design §4.4.1).
use toolkit_core::Json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotType {
    Consent,
    Flag,
    Integer { minimum: i64 },
    Metrics,
}

pub(crate) const TYPES: &[(&str, SlotType)] = &[
    ("ACCEPT_EULA", SlotType::Consent),
    ("CONCURRENCY", SlotType::Integer { minimum: 1 }),
    ("INPUT_TOKENS_MEAN", SlotType::Integer { minimum: 1 }),
    ("INPUT_TOKENS_STDDEV", SlotType::Integer { minimum: 0 }),
    ("METRICS_LIST", SlotType::Metrics),
    ("OUTPUT_TOKENS_MEAN", SlotType::Integer { minimum: 1 }),
    ("OUTPUT_TOKENS_STDDEV", SlotType::Integer { minimum: 0 }),
    ("REQUEST_COUNT", SlotType::Integer { minimum: 1 }),
    ("TRUE_OR_FALSE", SlotType::Flag),
];

pub(crate) fn slot_type(name: &str) -> Option<SlotType> {
    TYPES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, kind)| *kind)
}

/// Returns only a literal, never an error containing the supplied value.
#[allow(dead_code)] // Connected by the render slice.
pub(crate) fn literal(name: &str, value: &Json, consent: bool) -> Result<String, &'static str> {
    match (slot_type(name), value) {
        (Some(SlotType::Consent), Json::Bool(true)) if !consent => Err("consent_not_given"),
        (Some(SlotType::Consent | SlotType::Flag), Json::Bool(v)) => {
            Ok(if *v { "True" } else { "False" }.to_owned())
        }
        (Some(SlotType::Integer { minimum }), Json::Int(v))
            if *v >= minimum && *v <= i64::from(i32::MAX) =>
        {
            Ok(v.to_string())
        }
        (Some(SlotType::Metrics), Json::Array(values)) if values.len() <= 64 => {
            let mut names = std::collections::HashSet::new();
            let mut result = Vec::new();
            for value in values {
                let Json::String(name) = value else {
                    return Err("typed_value_invalid");
                };
                let mut chars = name.chars();
                if name.len() > 64
                    || !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                    || !chars.all(|c| c.is_ascii_alphanumeric())
                    || !names.insert(name)
                {
                    return Err("typed_value_invalid");
                }
                result.push(format!("\"{name}\""));
            }
            Ok(format!("[{}]", result.join(", ")))
        }
        _ => Err("typed_value_invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;
    #[test]
    fn ap_typ_01_02_03_04_consent_and_flag_are_strict() {
        assert_eq!(
            literal("ACCEPT_EULA", &Json::Bool(false), false).unwrap(),
            "False"
        );
        assert_eq!(
            literal("ACCEPT_EULA", &Json::Bool(true), false),
            Err("consent_not_given")
        );
        assert_eq!(
            literal("ACCEPT_EULA", &Json::Bool(true), true).unwrap(),
            "True"
        );
        for raw in [
            "\"true\"",
            "\"false\"",
            "1",
            "0",
            "null",
            "[]",
            "[true]",
            "[\"false\"]",
            "{}",
        ] {
            assert!(literal("ACCEPT_EULA", &parse_json(raw).unwrap(), true).is_err());
        }
        for raw in ["true", "false"] {
            assert!(literal("TRUE_OR_FALSE", &parse_json(raw).unwrap(), false).is_ok());
        }
        for raw in ["\"false\"", "1", "null", "[\"false\"]", "{}"] {
            assert!(literal("TRUE_OR_FALSE", &parse_json(raw).unwrap(), false).is_err());
        }
    }
    #[test]
    fn ap_typ_06_integer_ranges_and_token_kinds() {
        for (name, kind) in TYPES {
            if let SlotType::Integer { minimum } = kind {
                assert_eq!(
                    literal(name, &Json::Int(*minimum), false).unwrap(),
                    minimum.to_string()
                );
                assert!(literal(name, &Json::Int(minimum - 1), false).is_err());
                for raw in [
                    "-1",
                    "512.0",
                    "5.12e2",
                    "2147483648",
                    "true",
                    "\"512\"",
                    "null",
                    "[512]",
                ] {
                    assert!(
                        literal(name, &parse_json(raw).unwrap(), false).is_err(),
                        "{name}: {raw}"
                    );
                }
            }
        }
    }
    #[test]
    fn ap_typ_07_metric_list_domain() {
        assert_eq!(
            literal(
                "METRICS_LIST",
                &parse_json("[\"Correctness\",\"Faithfulness\"]").unwrap(),
                false
            )
            .unwrap(),
            "[\"Correctness\", \"Faithfulness\"]"
        );
        assert_eq!(
            literal("METRICS_LIST", &Json::Array(vec![]), false).unwrap(),
            "[]"
        );
        for raw in [
            "null",
            "\"Correctness\"",
            "[1]",
            "[\"\"]",
            "[\"a b\"]",
            "[\"X\",\"X\"]",
            "[[\"X\"]]",
        ] {
            assert!(literal("METRICS_LIST", &parse_json(raw).unwrap(), false).is_err());
        }
        let names = (0..65).map(|i| Json::string(format!("X{i}"))).collect();
        assert!(literal("METRICS_LIST", &Json::Array(names), false).is_err());
    }
}

#[cfg(test)]
mod scalar_tests {
    use super::*;
    #[test]
    fn ap_sub_10_12_scalar_quotes_backslashes_and_rejects_controls() {
        assert_eq!(
            quoted_literal(&Json::string("a'\\b"), '\'').unwrap(),
            "'a\\'\\\\b'"
        );
        assert_eq!(
            quoted_literal(&Json::string("a\"b"), '"').unwrap(),
            "\"a\\\"b\""
        );
        for text in ["a\nb", "a\tb", "\u{202e}", "\u{feff}"] {
            assert!(quoted_literal(&Json::string(text), '"').is_err());
        }
        assert!(quoted_literal(&Json::Int(1), '"').is_err());
        assert!(quoted_literal(&Json::string("x".repeat(4097)), '"').is_err());
    }
    #[test]
    fn ap_sec_01_02_03_secret_is_none_or_a_secrets_manager_arn() {
        assert_eq!(secret_literal(&Json::Null, '"').unwrap(), "None");
        assert_eq!(
            secret_literal(&Json::string("None"), '"').unwrap(),
            "\"None\""
        );
        assert!(secret_literal(
            &Json::string("arn:aws:secretsmanager:us-east-1:123456789012:secret:my-secret"),
            '"'
        )
        .is_ok());
        for text in [
            "token-value",
            "arn:aws:secretsmanager:us-east-1:123:secret:x",
            "arn:aws:s3:us-east-1:123456789012:secret:x",
            "arn:aws:secretsmanager:us-east-1:123456789012:secret:",
        ] {
            assert!(secret_literal(&Json::string(text), '"').is_err());
        }
    }
}

/// Escape a validated scalar as a complete Python string literal.
#[allow(dead_code)] // Wired into substitution by the renderer.
pub(crate) fn quoted_literal(value: &Json, quote: char) -> Result<String, &'static str> {
    let Json::String(text) = value else {
        return Err("string_value_invalid");
    };
    if !matches!(quote, '\'' | '"')
        || text.chars().count() > 4096
        || text.chars().any(toolkit_core::is_unsafe_char)
    {
        return Err("string_value_invalid");
    }
    let mut result = String::with_capacity(text.len() + 2);
    result.push(quote);
    for ch in text.chars() {
        if ch == quote || ch == '\\' {
            result.push('\\');
        }
        result.push(ch);
    }
    result.push(quote);
    Ok(result)
}

/// A secret is a reference or the None sentinel, never a literal token.
#[allow(dead_code)] // Wired into substitution by the renderer.
pub(crate) fn secret_literal(value: &Json, quote: char) -> Result<String, &'static str> {
    if matches!(value, Json::Null) {
        return Ok("None".to_owned());
    }
    let Json::String(text) = value else {
        return Err("secret_value_invalid");
    };
    if text != "None" && !secrets_manager_arn(text) {
        return Err("secret_value_invalid");
    }
    quoted_literal(value, quote).map_err(|_| "secret_value_invalid")
}

fn secrets_manager_arn(text: &str) -> bool {
    let mut parts = text.splitn(7, ':');
    let (
        Some("arn"),
        Some(partition),
        Some("secretsmanager"),
        Some(region),
        Some(account),
        Some("secret"),
        Some(resource),
    ) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    )
    else {
        return false;
    };
    partition
        .strip_prefix("aws")
        .is_some_and(|suffix| suffix.bytes().all(|c| c.is_ascii_lowercase() || c == b'-'))
        && !region.is_empty()
        && region
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && account.len() == 12
        && account.bytes().all(|c| c.is_ascii_digit())
        && !resource.is_empty()
}
