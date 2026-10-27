//! Offline tests for reference scanning, splitting and the JSON renderer.

use super::*;
use toolkit_core::parse_json;

type Answers<'a> = &'a [(&'a str, &'a str)];

fn sub(
    input: &str,
    answers: Answers<'_>,
) -> (Result<String, SubstituteError<String>>, Vec<String>) {
    let mut seen: Vec<String> = Vec::new();
    let result = substitute(input.as_bytes(), &mut |reference| {
        seen.push(reference.to_string());
        answers
            .iter()
            .find(|(name, _)| *name == reference)
            .map(|(_, value)| value.as_bytes().to_vec())
            .ok_or_else(|| format!("no answer for {reference}"))
    });
    (result.map(|bytes| String::from_utf8(bytes).unwrap()), seen)
}

#[test]
fn text_without_references_is_returned_unchanged_and_never_resolved() {
    for text in [
        "",
        "plain",
        "{{resolve:other:a}}",
        "{{resolve:secretsmanager:}}",
        "{{resolve:secretsmanager:a}",
        "{{resolve:secretsmanager:a}b}}",
        "{resolve:secretsmanager:a}",
        "resolve:secretsmanager:a}}",
    ] {
        assert!(!contains_reference(text.as_bytes()), "{text}");
        let (result, seen) = sub(text, &[]);
        assert_eq!(result.unwrap(), text);
        assert!(seen.is_empty(), "{text}");
    }
}

#[test]
fn references_are_replaced_in_place_left_to_right() {
    let (result, seen) = sub(
        "a {{resolve:secretsmanager:one}} b {{resolve:secretsmanager:two:SecretString:k}}!",
        &[("one", "1"), ("two:SecretString:k", "2")],
    );
    assert_eq!(result.unwrap(), "a 1 b 2!");
    assert_eq!(seen, ["one", "two:SecretString:k"]);

    let (adjacent, _) = sub(
        "{{resolve:secretsmanager:a}}{{resolve:secretsmanager:b}}",
        &[("a", "A"), ("b", "B")],
    );
    assert_eq!(adjacent.unwrap(), "AB");
}

#[test]
fn a_replacement_is_never_scanned_again() {
    let (result, seen) = sub(
        "{{resolve:secretsmanager:a}}",
        &[("a", "{{resolve:secretsmanager:b}}"), ("b", "LEAK")],
    );
    assert_eq!(result.unwrap(), "{{resolve:secretsmanager:b}}");
    assert_eq!(seen, ["a"]);
}

#[test]
fn the_match_starts_at_the_last_opening_brace_like_the_regex() {
    let (result, _) = sub("{{{resolve:secretsmanager:a}}", &[("a", "V")]);
    assert_eq!(result.unwrap(), "{V");
    // `[^}]+` stops at the first `}`; a lone `}` ends that attempt but a later
    // reference still matches.
    let (result, seen) = sub(
        "{{resolve:secretsmanager:x}y {{resolve:secretsmanager:a}}",
        &[("a", "V")],
    );
    assert_eq!(result.unwrap(), "{{resolve:secretsmanager:x}y V");
    assert_eq!(seen, ["a"]);
}

#[test]
fn a_reference_may_contain_newlines_and_colons() {
    let (result, seen) = sub("{{resolve:secretsmanager:a\nb:c}}", &[("a\nb:c", "V")]);
    assert_eq!(result.unwrap(), "V");
    assert_eq!(seen, ["a\nb:c"]);
}

#[test]
fn the_first_resolver_failure_stops_substitution() {
    let (result, seen) = sub(
        "{{resolve:secretsmanager:a}} {{resolve:secretsmanager:b}}",
        &[("b", "B")],
    );
    assert_eq!(
        result.unwrap_err(),
        SubstituteError::Resolver("no answer for a".to_string())
    );
    assert_eq!(seen, ["a"]);
}

#[test]
fn non_utf8_bytes_pass_through_but_a_non_utf8_reference_is_refused() {
    let mut input = vec![0xff];
    input.extend_from_slice(b"{{resolve:secretsmanager:a}}");
    input.push(0xfe);
    let out = substitute::<String>(&input, &mut |_| Ok(b"V".to_vec())).unwrap();
    assert_eq!(out, [0xff, b'V', 0xfe]);

    let mut bad = b"{{resolve:secretsmanager:".to_vec();
    bad.push(0xff);
    bad.extend_from_slice(b"}}");
    assert_eq!(
        substitute::<String>(&bad, &mut |_| Ok(Vec::new())).unwrap_err(),
        SubstituteError::NotUtf8
    );
}

#[test]
fn many_unterminated_openers_are_scanned_in_linear_time() {
    let mut input = "{{resolve:secretsmanager:a".repeat(20_000);
    input.push('}');
    assert!(!contains_reference(input.as_bytes()));
    let (result, seen) = sub(&input, &[]);
    assert_eq!(result.unwrap(), input);
    assert!(seen.is_empty());
}

#[test]
fn plain_references_split_into_id_type_key_and_stage() {
    let plain = Reference::parse("prod/db");
    assert_eq!(plain.secret_id, "prod/db");
    assert_eq!(plain.field_type, "SecretString");
    assert_eq!(plain.json_key, None);
    assert_eq!(plain.version_stage, "AWSCURRENT");
    assert_eq!(plain.arn_region(), None);

    let keyed = Reference::parse("prod/db:SecretString:password");
    assert_eq!(keyed.json_key.as_deref(), Some("password"));

    let staged = Reference::parse("n:SecretString:k:AWSPREVIOUS");
    assert_eq!(staged.version_stage, "AWSPREVIOUS");

    // The stage takes everything after the third colon.
    assert_eq!(
        Reference::parse("n:SecretString:k:a:b").version_stage,
        "a:b"
    );

    // An empty key means "the whole value"; an empty stage stays empty.
    let empty_key = Reference::parse("n:SecretString::AWSPREVIOUS");
    assert_eq!(empty_key.json_key, None);
    assert_eq!(empty_key.version_stage, "AWSPREVIOUS");
    assert_eq!(Reference::parse("n:SecretString:k:").version_stage, "");

    assert_eq!(
        Reference::parse("n:SecretBinary").field_type,
        "SecretBinary"
    );
}

#[test]
fn arns_keep_their_first_seven_segments_as_the_secret_id() {
    let arn = "arn:aws:secretsmanager:us-west-2:123456789012:secret:prod/db-AbCdEf";
    let bare = Reference::parse(arn);
    assert_eq!(bare.secret_id, arn);
    assert_eq!(bare.field_type, "SecretString");
    assert_eq!(bare.arn_region(), Some("us-west-2"));

    let full = Reference::parse(&format!("{arn}:SecretString:user:AWSPREVIOUS:ignored"));
    assert_eq!(full.secret_id, arn);
    assert_eq!(full.json_key.as_deref(), Some("user"));
    assert_eq!(full.version_stage, "AWSPREVIOUS");

    let short = Reference::parse("arn:aws:secretsmanager:eu-1");
    assert_eq!(short.secret_id, "arn:aws:secretsmanager:eu-1");
    assert_eq!(short.arn_region(), Some("eu-1"));

    let no_region = Reference::parse("arn:aws:secretsmanager::123456789012:secret:n");
    assert_eq!(no_region.arn_region(), None);
}

#[test]
fn json_dumps_matches_python_formatting() {
    let value =
        parse_json(r#"{"a":1,"b":[true,null,"é\n\"q\"\\"],"c":1.5,"d":{"x":"y"},"e":[],"f":{}}"#)
            .unwrap();
    assert_eq!(
        python_json_dumps(&value),
        r#"{"a": 1, "b": [true, null, "é\n\"q\"\\"], "c": 1.5, "d": {"x": "y"}, "e": [], "f": {}}"#
    );
    assert_eq!(python_json_dumps(&Json::Bool(false)), "false");
    assert_eq!(python_json_dumps(&Json::Float(100.0)), "100.0");
    assert_eq!(python_json_dumps(&Json::Float(1e16)), "1e+16");
    assert_eq!(python_json_dumps(&Json::Float(1.5e-7)), "1.5e-07");
    assert_eq!(python_json_dumps(&Json::Int(-7)), "-7");
    // Control characters, DEL and astral characters become \u escapes.
    assert_eq!(
        python_json_dumps(&Json::string("\u{1}\u{7f}😀")),
        "\"\\u0001\\u007f\\ud83d\\ude00\""
    );
}
