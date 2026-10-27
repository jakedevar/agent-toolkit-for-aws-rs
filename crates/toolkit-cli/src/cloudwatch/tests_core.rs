//! Tests for the value-level modules: constants, timestamps, region,
//! validation, locations, captures, error text and snapshot parsing. All
//! fixtures are literals; nothing touches the network or the clock.

use toolkit_core::{parse_json, Json};

use super::capture::{capture_from_response, Capture, CaptureLimits};
use super::consts::resolve_snapshot_log_group;
use super::errors::{render_client_error, render_failure, translate_aws_error, AwsFailure, Prose};
use super::location::{
    is_missing_identifier_error, location_from_response, parse_create_inputs, parse_lookup_inputs,
    render_location_block, Location, LocationInputs, ResolvedLocation, CODE_LOOKUP_UNSUPPORTED,
    MISSING_LOCATION_IDENTIFIER,
};
use super::pytext::{dumps_indent2, group_thousands, is_space, rstrip, strip};
use super::region::resolve_region;
use super::snapshot_parsing::{
    escape_logs_insights_regex, escape_logs_insights_string, parse_snapshot_fields,
    preview_captured_value,
};
use super::timefmt::{format_timestamp, parse_iso_timestamp, time_from_json, Civil, UtcTime};
use super::validation::{
    canonical_language, format_code_location_troubleshooting, is_valid_location_hash,
    normalize_instrumentation_type, validate_capture_names, validate_location_inputs,
    validate_probe_constraints, validate_snapshot_signal,
};

fn json(text: &str) -> Json {
    parse_json(text).expect("valid fixture json")
}

fn compact(value: &Json) -> String {
    value.to_compact_string()
}

// ───────────────────────────── constants ─────────────────────────────

#[test]
fn snapshot_log_group_substitutes_the_service_once() {
    assert_eq!(
        resolve_snapshot_log_group("checkout"),
        "/aws/service-events/checkout"
    );
    // A brace-looking service name is data, never re-expanded.
    assert_eq!(
        resolve_snapshot_log_group("a{service_name}b"),
        "/aws/service-events/a{service_name}b"
    );
}

// ───────────────────────────── python text ─────────────────────────────

#[test]
fn strip_uses_python_whitespace() {
    assert_eq!(strip("  x \t\n"), "x");
    assert_eq!(strip("\u{1c}x\u{1f}"), "x");
    assert_eq!(rstrip(" x \n"), " x");
    assert!(is_space('\u{a0}'));
    assert!(!is_space('x'));
}

#[test]
fn group_thousands_matches_python_format() {
    assert_eq!(group_thousands(0), "0");
    assert_eq!(group_thousands(999), "999");
    assert_eq!(group_thousands(1_000), "1,000");
    assert_eq!(group_thousands(10_241), "10,241");
    assert_eq!(group_thousands(1_234_567), "1,234,567");
}

#[test]
fn dumps_indent2_matches_json_dumps() {
    let value = json(r#"{"a":[],"b":{},"c":[1,2.5,"é\n"],"d":{"e":null,"f":true}}"#);
    let expected = "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    2.5,\n    \"\\u00e9\\n\"\n  ],\n  \"d\": {\n    \"e\": null,\n    \"f\": true\n  }\n}";
    assert_eq!(dumps_indent2(&value), expected);
    assert_eq!(dumps_indent2(&Json::Float(1.0)), "1.0");
    assert_eq!(dumps_indent2(&Json::string("\u{7f}")), "\"\\u007f\"");
}

// ───────────────────────────── timestamps ─────────────────────────────

#[test]
fn format_timestamp_passes_strings_through_and_defaults_blanks() {
    assert_eq!(format_timestamp(None, "N/A"), "N/A");
    assert_eq!(format_timestamp(Some(&Json::Null), "N/A"), "N/A");
    assert_eq!(format_timestamp(Some(&Json::string("")), "Never"), "Never");
    assert_eq!(
        format_timestamp(Some(&Json::string("2025-02-03T18:42:00Z")), "N/A"),
        "2025-02-03T18:42:00Z"
    );
    assert_eq!(format_timestamp(Some(&Json::Int(5)), "N/A"), "5");
    assert_eq!(format_timestamp(Some(&Json::Float(1.5)), "N/A"), "1.5");
}

#[test]
fn iso_parsing_reads_z_offsets_and_naive_values_as_utc() {
    for text in [
        "2025-02-03T18:42:00Z",
        "2025-02-03T18:42:00",
        "2025-02-03T18:42:00+00:00",
        "2025-02-03 18:42:00Z",
        "2025-02-03T10:42:00-08:00",
        "2025-02-04T00:12:00+05:30",
        "20250203T184200Z",
    ] {
        let parsed = parse_iso_timestamp(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        assert_eq!(parsed.epoch_seconds(), 1_738_608_120, "{text}");
        assert_eq!(parsed.format_seconds_z(), "2025-02-03T18:42:00Z", "{text}");
    }
}

#[test]
fn iso_parsing_keeps_microseconds_and_date_only_values() {
    let fractional = parse_iso_timestamp("2025-02-03T18:42:00.5Z").expect("fraction");
    assert_eq!(fractional.isoformat_z(), "2025-02-03T18:42:00.500000Z");
    assert_eq!(fractional.format_seconds_z(), "2025-02-03T18:42:00Z");
    let long = parse_iso_timestamp("2025-02-03T18:42:00.1234567Z").expect("long fraction");
    assert_eq!(long.isoformat_z(), "2025-02-03T18:42:00.123456Z");
    let date = parse_iso_timestamp("2025-02-03").expect("date");
    assert_eq!(date.format_seconds_z(), "2025-02-03T00:00:00Z");
    assert_eq!(
        parse_iso_timestamp("2025-02-03T18:42Z")
            .expect("minutes only")
            .format_seconds_z(),
        "2025-02-03T18:42:00Z"
    );
}

#[test]
fn iso_parsing_reports_python_error_texts() {
    let cases = [
        ("not-a-date", "Invalid isoformat string: 'not-a-date'"),
        ("", "Invalid isoformat string: ''"),
        ("garbageZ", "Invalid isoformat string: 'garbage+00:00'"),
        ("2025-02-03T", "Invalid isoformat string: '2025-02-03T'"),
        ("2025-13-01T00:00:00Z", "month must be in 1..12"),
        ("2025-02-30T00:00:00Z", "day is out of range for month"),
        ("2023-02-29", "day is out of range for month"),
        ("2025-02-03T25:00:00Z", "hour must be in 0..23"),
        ("2025-02-03T00:61:00Z", "minute must be in 0..59"),
        ("2025-02-03T00:00:60Z", "second must be in 0..59"),
        ("0000-01-01", "year 0 is out of range"),
    ];
    for (text, expected) in cases {
        assert_eq!(
            parse_iso_timestamp(text).expect_err(text),
            expected,
            "{text}"
        );
    }
    assert!(parse_iso_timestamp("2025-02-03T18:42:00+24:00")
        .expect_err("offset")
        .starts_with("offset must be a timedelta strictly between"));
    // Non-ASCII input is never a valid ISO timestamp.
    assert!(parse_iso_timestamp("2025-02-03T18:42:00\u{ff1a}").is_err());
}

#[test]
fn utc_time_calendar_and_range() {
    let epoch = UtcTime::from_epoch_micros(0).expect("epoch");
    assert_eq!(epoch.format_seconds_z(), "1970-01-01T00:00:00Z");
    let before = UtcTime::from_epoch_micros(-1_000_000).expect("before epoch");
    assert_eq!(before.format_seconds_z(), "1969-12-31T23:59:59Z");
    // `int(timestamp())` truncates toward zero.
    assert_eq!(
        UtcTime::from_epoch_micros(-1_500_000)
            .expect("negative")
            .epoch_seconds(),
        -1
    );
    let leap = UtcTime::from_civil(Civil {
        year: 2024,
        month: 2,
        day: 29,
        hour: 23,
        minute: 59,
        second: 59,
        micro: 0,
    })
    .expect("leap day");
    assert_eq!(leap.format_seconds_z(), "2024-02-29T23:59:59Z");
    let end = UtcTime::from_epoch_micros(253_402_300_799_999_999).expect("max");
    assert_eq!(end.format_seconds_z(), "9999-12-31T23:59:59Z");
    assert!(end.checked_add_micros(1).is_none());
    assert!(UtcTime::from_epoch_micros(-62_135_596_800_000_001).is_none());
    assert!(UtcTime::from_epoch_seconds(f64::NAN).is_none());
    let moved = leap.checked_add_micros(1_000_000).expect("one second");
    assert_eq!(moved.format_seconds_z(), "2024-03-01T00:00:00Z");
}

#[test]
fn time_from_json_accepts_strings_and_epoch_seconds() {
    let from_string = time_from_json(&Json::string("2025-02-03T18:42:00Z")).expect("string");
    let from_int = time_from_json(&Json::Int(1_738_608_120)).expect("int");
    let from_float = time_from_json(&Json::Float(1_738_608_120.0)).expect("float");
    assert_eq!(from_string, from_int);
    assert_eq!(from_int, from_float);
    assert!(time_from_json(&Json::Bool(true)).is_err());
    assert!(time_from_json(&Json::Int(i64::MAX)).is_err());
    // The value comes from a response: the error names its type only (#4784).
    let error = time_from_json(&Json::string("SYNTHETIC_TIME_SENTINEL")).expect_err("not ISO");
    assert!(!error.contains("SYNTHETIC"), "{error}");
}

// ───────────────────────────── region ─────────────────────────────

fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    }
}

#[test]
fn region_precedence_is_flag_then_region_then_default_region_then_us_east_1() {
    let both = env_of(&[
        ("AWS_REGION", "eu-west-1"),
        ("AWS_DEFAULT_REGION", "ap-south-1"),
    ]);
    assert_eq!(resolve_region(Some("us-west-2"), &both), "us-west-2");
    assert_eq!(resolve_region(None, &both), "eu-west-1");
    let default_only = env_of(&[("AWS_DEFAULT_REGION", "ap-south-1")]);
    assert_eq!(resolve_region(None, &default_only), "ap-south-1");
    assert_eq!(resolve_region(None, &env_of(&[])), "us-east-1");
}

#[test]
fn region_treats_empty_values_as_unset_at_every_level() {
    let env = env_of(&[("AWS_REGION", ""), ("AWS_DEFAULT_REGION", "ap-south-1")]);
    assert_eq!(resolve_region(Some(""), &env), "ap-south-1");
    let empty = env_of(&[("AWS_REGION", ""), ("AWS_DEFAULT_REGION", "")]);
    assert_eq!(resolve_region(None, &empty), "us-east-1");
}

// ───────────────────────────── validation ─────────────────────────────

#[test]
fn canonical_language_maps_any_casing() {
    assert_eq!(canonical_language(Some("python")), Some("Python"));
    assert_eq!(canonical_language(Some(" JAVA ")), Some("Java"));
    assert_eq!(canonical_language(Some("JavaScript")), Some("Javascript"));
    assert_eq!(canonical_language(Some("go")), None);
    assert_eq!(canonical_language(Some("")), None);
    assert_eq!(canonical_language(None), None);
}

#[test]
fn instrumentation_type_is_normalized_and_rejected_with_the_received_text() {
    assert_eq!(
        normalize_instrumentation_type(Some(" probe ")),
        ("PROBE".to_string(), None)
    );
    assert_eq!(
        normalize_instrumentation_type(Some("breakpoint")),
        ("BREAKPOINT".to_string(), None)
    );
    let (normalized, error) = normalize_instrumentation_type(Some("trace"));
    assert_eq!(normalized, "TRACE");
    assert_eq!(
        error.as_deref(),
        Some("ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: trace)")
    );
    let (normalized, error) = normalize_instrumentation_type(None);
    assert_eq!(normalized, "");
    assert_eq!(
        error.as_deref(),
        Some("ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: None)")
    );
    let (_, error) = normalize_instrumentation_type(Some(""));
    assert_eq!(
        error.as_deref(),
        Some("ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: )")
    );
}

#[test]
fn capture_names_reject_empty_lists_and_the_wildcard() {
    assert_eq!(validate_capture_names("capture_arguments", None), None);
    assert_eq!(
        validate_capture_names("capture_arguments", Some(&["a".to_string()])),
        None
    );
    assert_eq!(
        validate_capture_names("capture_arguments", Some(&[])).as_deref(),
        Some("ERROR: capture_arguments must contain at least one name if provided. Omit it to capture none.")
    );
    assert_eq!(
        validate_capture_names("capture_locals", Some(&["a".to_string(), "*".to_string()]))
            .as_deref(),
        Some("ERROR: capture_locals does not support the wildcard \"*\". List explicit names, or omit it to capture none.")
    );
}

#[test]
fn probe_constraints_cover_javascript_and_line_numbers() {
    assert_eq!(
        validate_probe_constraints("BREAKPOINT", Some("javascript"), Some(3)),
        None
    );
    assert_eq!(
        validate_probe_constraints("PROBE", Some("python"), None),
        None
    );
    assert_eq!(
        validate_probe_constraints("PROBE", Some(" JavaScript "), None).as_deref(),
        Some("ERROR: PROBE is not supported for JavaScript. Use instrumentation_type=BREAKPOINT for JavaScript targets.")
    );
    assert_eq!(
        validate_probe_constraints("PROBE", Some("java"), Some(10)).as_deref(),
        Some("ERROR: PROBE does not support line_number (the SDKs ignore it). Omit line_number for PROBE \u{2014} it is method/function-level only.")
    );
}

#[test]
fn location_hash_must_be_sixteen_lowercase_hex_characters() {
    assert!(is_valid_location_hash(Some("0123456789abcdef")));
    assert!(!is_valid_location_hash(Some("0123456789ABCDEF")));
    assert!(!is_valid_location_hash(Some("0123456789abcde")));
    assert!(!is_valid_location_hash(Some("0123456789abcdef0")));
    assert!(!is_valid_location_hash(Some("0123456789abcde\"")));
    assert!(!is_valid_location_hash(Some("0123456789abcdeg")));
    assert!(!is_valid_location_hash(Some("")));
    assert!(!is_valid_location_hash(None));
}

#[test]
fn snapshot_signal_is_case_insensitive_but_exact() {
    assert_eq!(validate_snapshot_signal(Some("snapshot")), None);
    assert_eq!(validate_snapshot_signal(Some(" SNAPSHOT ")), None);
    assert_eq!(
        validate_snapshot_signal(Some("TRACE")).as_deref(),
        Some("ERROR: signal_type must be SNAPSHOT for this API (received: TRACE)")
    );
    assert_eq!(
        validate_snapshot_signal(None).as_deref(),
        Some("ERROR: signal_type must be SNAPSHOT for this API (received: None)")
    );
}

#[test]
fn java_location_errors_number_every_problem_and_suggest_a_split() {
    let message = validate_location_inputs(
        "Java",
        "Order.java",
        None,
        Some("com.example.OrderContext"),
        None,
        None,
    )
    .expect("invalid location");
    let head = "Invalid breakpoint location inputs:\n\
1. Java requires code_unit (the package name, e.g. com.amazon.sampleapp).\n\
2. Java requires method_name.\n\
3. For Java, class_name must be simple (e.g., \"OrderContext\"), not fully qualified (e.g., \"com.example.OrderContext\").\n\
\n\
Suggestions:\n\
1. Use code_unit=\"com.example\" and class_name=\"OrderContext\".\n\
\n\
CODE LOCATION TROUBLESHOOTING:\n";
    assert!(message.starts_with(head), "{message}");
    assert!(message.contains(
        "- Breakpoint level: FUNCTION/METHOD-level (line_number omitted).\n- Java rules:\n"
    ));
    assert!(message.ends_with(
        "LOCATION INPUTS RECEIVED:\n- language=Java\n- file_path=Order.java\n- code_unit=None\n- class_name=com.example.OrderContext\n- method_name=None\n- line_number=None"
    ));
}

#[test]
fn python_and_javascript_location_rules() {
    let python = validate_location_inputs("python", "a.py", Some("a.py"), None, None, None)
        .expect("python errors");
    assert!(
        python.contains("1. Python requires method_name.\n"),
        "{python}"
    );
    assert!(python.contains(
        "Suggestions:\n1. Python code_unit should be a module path (e.g., services.billing), not a .py filename.\n"
    ));
    assert!(python.contains("- Python rules:\n"));

    let javascript = validate_location_inputs("JavaScript", "app.js", None, None, None, None)
        .expect("js errors");
    assert!(javascript
        .contains("1. JavaScript requires line_number (>= 1); it binds by file and line.\n"));

    let zero = validate_location_inputs("JavaScript", "app.js", None, None, None, Some(0))
        .expect("zero line");
    assert!(
        zero.contains("1. line_number must be >= 1 (received: 0).\n"),
        "{zero}"
    );

    let unknown = validate_location_inputs("go", "main.go", None, None, None, None)
        .expect("unknown language");
    assert!(unknown.contains("1. language must be Python, Java, or JavaScript (received: go).\n"));

    let blank = validate_location_inputs("Python", "  ", Some("m"), None, Some("f"), None)
        .expect("blank path");
    assert!(blank.contains("1. file_path is required and must be non-empty.\n"));

    assert_eq!(
        validate_location_inputs("Python", "a.py", Some("m"), None, Some("f"), None),
        None
    );
    assert_eq!(
        validate_location_inputs(
            "java",
            "A.java",
            Some("com.x"),
            Some("A"),
            Some("m"),
            Some(3)
        ),
        None
    );
}

#[test]
fn troubleshooting_notes_the_line_level_and_language_hints() {
    let java = format_code_location_troubleshooting(
        Some("Java"),
        Some("A.java"),
        Some("com.x"),
        Some("A"),
        Some("m"),
        Some(12),
    );
    assert!(java.contains(
        "- Breakpoint level: LINE-LEVEL (L12).\n  * NOTE: target an executable statement."
    ));
    let javascript = format_code_location_troubleshooting(
        Some("javascript"),
        Some("a.js"),
        None,
        None,
        None,
        Some(4),
    );
    assert!(javascript.contains("slides to the next parseable line"));
    assert!(javascript.contains("- JavaScript rules:\n"));
    let other = format_code_location_troubleshooting(None, None, None, None, None, None);
    assert!(other.ends_with("- language=None\n- file_path=None\n- code_unit=None\n- class_name=None\n- method_name=None\n- line_number=None"));
    assert!(!other.contains("rules:"));
}

// ───────────────────────────── locations ─────────────────────────────

fn python_inputs() -> LocationInputs<'static> {
    LocationInputs {
        language: Some("python"),
        file_path: Some("/app/billing.py"),
        code_unit: Some("services.billing"),
        class_name: Some("Billing"),
        method_name: Some("charge"),
        line_number: None,
        ..LocationInputs::default()
    }
}

#[test]
fn create_inputs_require_language_and_file_path() {
    let expected = "ERROR: BREAKPOINT/PROBE require language and file_path.\nExample: language=\"Python\", file_path=\"/app/handler.py\"";
    assert_eq!(
        parse_create_inputs(&LocationInputs::default()),
        Err(expected.to_string())
    );
    let missing_path = LocationInputs {
        language: Some("Python"),
        file_path: Some(""),
        ..LocationInputs::default()
    };
    assert_eq!(
        parse_create_inputs(&missing_path),
        Err(expected.to_string())
    );
}

#[test]
fn create_inputs_surface_validation_errors_verbatim() {
    let inputs = LocationInputs {
        language: Some("Python"),
        file_path: Some("/a.py"),
        ..LocationInputs::default()
    };
    let error = parse_create_inputs(&inputs).expect_err("python needs code_unit and method_name");
    assert!(error.starts_with("Invalid breakpoint location inputs:\n1. Python requires code_unit"));
}

#[test]
fn create_inputs_canonicalize_the_language_and_build_the_payload() {
    let location = parse_create_inputs(&python_inputs()).expect("valid python location");
    assert_eq!(
        compact(&location.to_api_payload().expect("create payload")),
        r#"{"CodeLocation":{"Language":"Python","FilePath":"/app/billing.py","CodeUnit":"services.billing","ClassName":"Billing","MethodName":"charge"}}"#
    );
    assert_eq!(
        location.to_identifier(),
        location.to_api_payload().expect("payload")
    );
    assert_eq!(location.describe(), "/app/billing.py :: Billing.charge");

    let line_level = LocationInputs {
        language: Some("JAVASCRIPT"),
        file_path: Some("/app/app.js"),
        line_number: Some(12),
        ..LocationInputs::default()
    };
    let location = parse_create_inputs(&line_level).expect("valid js location");
    assert_eq!(
        compact(&location.to_api_payload().expect("payload")),
        r#"{"CodeLocation":{"Language":"Javascript","FilePath":"/app/app.js","LineNumber":12}}"#
    );
    assert_eq!(location.describe(), "/app/app.js:L12");
}

#[test]
fn lookup_prefers_the_hash_and_supports_code_locations() {
    let both = LocationInputs {
        location_hash: Some("0123456789abcdef"),
        ..python_inputs()
    };
    let hash = parse_lookup_inputs(&both, true).expect("hash wins");
    assert!(matches!(hash, ResolvedLocation::Hash(_)));
    assert_eq!(hash.describe(), "LocationHash 0123456789abcdef");
    assert_eq!(
        compact(&hash.to_identifier()),
        r#"{"LocationHash":"0123456789abcdef"}"#
    );
    // A hash cannot describe a new configuration.
    assert_eq!(hash.to_api_payload(), None);

    let code = parse_lookup_inputs(&python_inputs(), true).expect("code lookup");
    assert!(matches!(code, ResolvedLocation::Code(_)));
    // Lookup does not validate the per-language fields, only create does.
    let sparse = LocationInputs {
        language: Some("python"),
        file_path: Some("/a.py"),
        ..LocationInputs::default()
    };
    assert!(parse_lookup_inputs(&sparse, true).is_ok());
}

#[test]
fn lookup_errors_are_distinct() {
    assert_eq!(
        parse_lookup_inputs(&python_inputs(), false),
        Err(CODE_LOOKUP_UNSUPPORTED.to_string())
    );
    let error = parse_lookup_inputs(&LocationInputs::default(), true).expect_err("nothing given");
    assert_eq!(error, MISSING_LOCATION_IDENTIFIER);
    assert_eq!(
        error,
        "missing location identifier input. Provide location_hash OR language+file_path (code location)."
    );
    assert!(is_missing_identifier_error(&error));
    assert!(!is_missing_identifier_error(CODE_LOOKUP_UNSUPPORTED));
}

#[test]
fn code_location_details_render_present_fields_in_order() {
    let location = location_from_response(&json(
        r#"{"CodeLocation":{"Language":"Python","FilePath":"/app/billing.py","CodeUnit":"services.billing","MethodName":"charge"}}"#,
    ));
    let Location::Code(code) = &location else {
        panic!("expected a code location, got {location:?}");
    };
    assert_eq!(
        code.format_details(Some("0123456789abcdef")),
        "- LocationKind: CODE\n- LocationHash: 0123456789abcdef\n- Language: Python\n- File Path: /app/billing.py\n- Code Unit: services.billing\n- Method Name: charge\n"
    );
    assert_eq!(code.format_details(None).lines().count(), 5);
    assert_eq!(
        render_location_block(&location, None),
        "- LocationKind: CODE\n- Language: Python\n- File Path: /app/billing.py\n- Code Unit: services.billing\n- Method Name: charge\n\nINSTRUMENTATION:\n- Level: FUNCTION/METHOD-LEVEL\n"
    );
}

#[test]
fn code_location_response_keeps_blank_and_extra_fields() {
    // A bare (unwrapped) location with a missing Language renders a blank
    // `Language`, and unknown members are listed sorted by key.
    let location = location_from_response(&json(
        r#"{"FilePath":"x.py","Zeta":1,"Alpha":"a","LineNumber":9}"#,
    ));
    let Location::Code(code) = &location else {
        panic!("expected a code location");
    };
    assert_eq!(
        code.format_details(None),
        "- LocationKind: CODE\n- Language: \n- File Path: x.py\n- Line Number: 9\n- Alpha: a\n- Zeta: 1\n"
    );
    assert_eq!(code.level(), "LINE-LEVEL (L9)");
    assert_eq!(code.describe(), "x.py:L9");
    assert_eq!(location.level().as_deref(), Some("LINE-LEVEL (L9)"));
    assert!(render_location_block(&location, None)
        .ends_with("INSTRUMENTATION:\n- Level: LINE-LEVEL (L9)\n"));
}

#[test]
fn unknown_locations_list_their_raw_fields() {
    let unknown = location_from_response(&json(r#"{"Foo":"bar","Alpha":{"x":1}}"#));
    assert!(matches!(unknown, Location::Unknown(_)));
    assert_eq!(unknown.describe(), "N/A");
    assert_eq!(unknown.level(), None);
    assert_eq!(
        render_location_block(&unknown, Some("0123456789abcdef")),
        "- LocationKind: UNKNOWN\n- LocationHash: 0123456789abcdef\n- Alpha: {'x': 1}\n- Foo: bar\n"
    );
    for empty in [json("{}"), json("null"), json("[1]"), json("\"text\"")] {
        assert_eq!(
            render_location_block(&location_from_response(&empty), None),
            "- LocationKind: UNKNOWN\n- Location payload could not be parsed.\n"
        );
    }
}

#[test]
fn hash_locations_render_their_own_block() {
    let location = parse_lookup_inputs(
        &LocationInputs {
            location_hash: Some("0123456789abcdef"),
            ..LocationInputs::default()
        },
        true,
    )
    .expect("hash")
    .into_location();
    assert_eq!(
        render_location_block(&location, Some("ignored")),
        "- LocationKind: HASH\n- LocationHash: 0123456789abcdef\n"
    );
    assert_eq!(location.level(), None);
}

// ───────────────────────────── captures ─────────────────────────────

#[test]
fn capture_payload_keeps_omitted_and_empty_lists_apart() {
    let wire = json(
        r#"{"CodeCapture":{"CaptureReturn":true,"CaptureStackTrace":false,"CaptureLimits":{"MaxHits":5},"CaptureArguments":["a"],"CaptureLocals":[]}}"#,
    );
    let Capture::Code(capture) = capture_from_response(&wire) else {
        panic!("expected a code capture");
    };
    assert!(capture.capture_return);
    assert!(!capture.capture_stack_trace);
    assert_eq!(capture.capture_arguments, Some(vec!["a".to_string()]));
    assert_eq!(capture.capture_locals, Some(Vec::new()));
    // Round trip: parse -> payload reproduces the wire shape.
    assert_eq!(capture.to_api_payload(), wire);

    let omitted = json(r#"{"CodeCapture":{"CaptureReturn":false,"CaptureStackTrace":false}}"#);
    let Capture::Code(capture) = capture_from_response(&omitted) else {
        panic!("expected a code capture");
    };
    assert_eq!(capture.capture_arguments, None);
    assert_eq!(capture.capture_locals, None);
    assert_eq!(
        compact(&capture.to_api_payload()),
        r#"{"CodeCapture":{"CaptureReturn":false,"CaptureStackTrace":false,"CaptureLimits":{}}}"#
    );
}

#[test]
fn unwrapped_capture_shapes_are_still_code_captures() {
    let Capture::Code(capture) = capture_from_response(&json(r#"{"CaptureReturn":1}"#)) else {
        panic!("expected a code capture");
    };
    assert!(capture.capture_return);
    assert!(capture.limits.is_empty());
    // Presence of a hint key is enough, even when it holds `null`.
    assert!(matches!(
        capture_from_response(&json(r#"{"CaptureLimits":null}"#)),
        Capture::Code(_)
    ));
}

#[test]
fn unrecognized_captures_are_unknown() {
    assert!(matches!(
        capture_from_response(&json(r#"{"Other":1}"#)),
        Capture::Unknown(_)
    ));
    for value in [json("null"), json("[]"), json("\"x\"")] {
        let Capture::Unknown(unknown) = capture_from_response(&value) else {
            panic!("expected unknown for {value:?}");
        };
        assert!(unknown.raw.is_empty());
    }
}

#[test]
fn capture_limits_report_only_set_values() {
    let limits =
        CaptureLimits::from_values([Some(5), None, Some(8), None, None, None, None, Some(2)]);
    assert!(!limits.is_empty());
    assert_eq!(
        compact(&limits.to_api_payload()),
        r#"{"MaxHits":5,"MaxCollectionWidth":8,"MaxFieldsPerObject":2}"#
    );
    assert!(CaptureLimits::default().is_empty());
    assert_eq!(compact(&CaptureLimits::unset().to_api_payload()), "{}");
}

// ───────────────────────────── error text ─────────────────────────────

fn client(code: Option<&str>, message: Option<&str>) -> AwsFailure {
    AwsFailure::Client {
        code: code.map(str::to_string),
        message: message.map(str::to_string),
        display: "An error occurred (X) when calling the Op operation: shown".to_string(),
    }
}

#[test]
fn client_errors_get_the_canned_causes_and_steps() {
    let text = translate_aws_error(
        &client(Some("ValidationException"), Some("bad input")),
        "create BREAKPOINT instrumentation",
        &[
            ("Type", Json::string("BREAKPOINT")),
            ("Target", Json::string("")),
            ("Gap", Json::Null),
            ("Service", Json::string("svc")),
            ("Count", Json::Int(2)),
        ],
    );
    assert_eq!(
        text,
        "Failed to create BREAKPOINT instrumentation\n\
\n\
Error: ValidationException - bad input\n\
\n\
ATTEMPTED PARAMETERS:\n\
- Type: BREAKPOINT\n\
- Service: svc\n\
- Count: 2\n\
\n\
POSSIBLE CAUSES:\n\
1. Invalid input parameters (validation error)\n\
2. Resource not found, already exists, or scoped to a different account\n\
3. Insufficient IAM permissions\n\
4. Service-side throttling or transient error\n\
\n\
TROUBLESHOOTING:\n\
1. Re-read the error message above for the specific failure cause\n\
2. Verify service, environment, and instrumentation_type identifiers\n\
3. Verify credentials map to an account/region with access\n"
    );
}

#[test]
fn client_errors_fall_back_to_the_class_name_and_display_text() {
    let text = translate_aws_error(&client(None, Some("")), "list instrumentations", &[]);
    assert!(text.starts_with(
        "Failed to list instrumentations\n\nError: ClientError - An error occurred (X) when calling the Op operation: shown\n\nPOSSIBLE CAUSES:\n"
    ));
    assert!(!text.contains("ATTEMPTED"));
}

#[test]
fn transport_and_credential_failures_have_their_own_troubleshooting() {
    let context = [("Service", Json::string("svc"))];
    assert_eq!(
        translate_aws_error(&AwsFailure::EndpointConnection("no route".into()), "get x", &context),
        "Failed to get x\n\nError: EndpointConnectionError - no route\n\nATTEMPTED PARAMETERS:\n- Service: svc\n\nTROUBLESHOOTING:\n1. Check network connectivity to the AWS endpoint\n2. Verify AWS region resolution (AWS_REGION env var or profile)\n"
    );
    assert_eq!(
        translate_aws_error(&AwsFailure::Timeout("slow".into()), "get x", &[]),
        "Failed to get x\n\nError: TimeoutError - slow\n\nTROUBLESHOOTING:\n1. Retry the request \u{2014} the AWS endpoint did not respond within the socket timeout\n2. Check network connectivity\n"
    );
    assert_eq!(
        translate_aws_error(
            &AwsFailure::Credentials {
                type_name: "PartialCredentialsError".into(),
                message: "Partial credentials found".into(),
            },
            "get x",
            &[]
        ),
        "Failed to get x\n\nError: PartialCredentialsError - Partial credentials found\n\nTROUBLESHOOTING:\n1. Verify AWS credentials: aws configure list\n2. Set AWS_PROFILE or supply credentials via env vars\n"
    );
    assert_eq!(
        translate_aws_error(&AwsFailure::Other("weird".into()), "get x", &context),
        "Failed to get x\n\nUnexpected error: weird\n\nATTEMPTED PARAMETERS:\n- Service: svc\n"
    );
    assert_eq!(
        translate_aws_error(&AwsFailure::Other("weird".into()), "get x", &[]),
        "Failed to get x\n\nUnexpected error: weird\n"
    );
}

#[test]
fn tailored_client_errors_keep_the_call_sites_wording() {
    let prose = Prose {
        attempted_label: Some("ATTEMPTED TO RETRIEVE:"),
        possible_causes: &["one", "two"],
        troubleshooting: &["do"],
        trailer: Some("TRAILER"),
    };
    let failure = client(Some("ResourceNotFoundException"), Some("gone"));
    let expected = "Failed to get instrumentation\n\
\n\
Error: ResourceNotFoundException - gone\n\
\n\
ATTEMPTED TO RETRIEVE:\n\
- Target: a.py\n\
\n\
POSSIBLE CAUSES:\n\
1. one\n\
2. two\n\
\n\
TROUBLESHOOTING:\n\
1. do\n\
\n\
TRAILER";
    let attempted = [("Target", Json::string("a.py"))];
    assert_eq!(
        render_client_error(&failure, "get instrumentation", &attempted, &prose),
        expected
    );
    assert_eq!(
        render_failure(&failure, "get instrumentation", &attempted, &prose),
        expected
    );

    // Without a trailer the body is right-stripped and nothing is appended.
    let bare = Prose {
        possible_causes: &["one"],
        ..Prose::default()
    };
    let text = render_client_error(&failure, "get x", &[], &bare);
    assert_eq!(
        text,
        "Failed to get x\n\nError: ResourceNotFoundException - gone\n\nPOSSIBLE CAUSES:\n1. one"
    );
}

#[test]
fn failures_without_prose_or_of_other_kinds_use_the_canned_template() {
    let failure = client(Some("Throttling"), Some("slow down"));
    let canned = translate_aws_error(&failure, "get x", &[]);
    assert_eq!(
        render_failure(&failure, "get x", &[], &Prose::default()),
        canned
    );

    let prose = Prose {
        possible_causes: &["one"],
        trailer: Some("TRAILER"),
        ..Prose::default()
    };
    let timeout = AwsFailure::Timeout("slow".into());
    assert_eq!(
        render_failure(&timeout, "get x", &[], &prose),
        translate_aws_error(&timeout, "get x", &[])
    );
}

// ───────────────────────────── snapshot parsing ─────────────────────────────

#[test]
fn regex_escaping_follows_python_re_escape() {
    assert_eq!(
        escape_logs_insights_regex("a.b*c(d)/e"),
        "a\\.b\\*c\\(d\\)\\/e"
    );
    assert_eq!(escape_logs_insights_regex("x y-z_9"), "x\\ y\\-z_9");
    assert_eq!(
        escape_logs_insights_regex("[a]{1}|^$+?&~#\\"),
        "\\[a\\]\\{1\\}\\|\\^\\$\\+\\?\\&\\~\\#\\\\"
    );
    assert_eq!(escape_logs_insights_regex("tab\there"), "tab\\\there");
    assert_eq!(escape_logs_insights_regex("caf\u{e9}"), "caf\u{e9}");
}

#[test]
fn string_escaping_handles_backslashes_before_quotes() {
    assert_eq!(escape_logs_insights_string("a\"b\\c"), "a\\\"b\\\\c");
    // A trailing backslash-quote pair cannot terminate the literal.
    assert_eq!(escape_logs_insights_string("\\\""), "\\\\\\\"");
    assert_eq!(escape_logs_insights_string("plain"), "plain");
}

#[test]
fn captured_values_preview_by_shape() {
    let cases = [
        ("5", "5"),
        (
            r#"{"type":"int","value":"7"}"#,
            r#"{"type":"int","value":"7"}"#,
        ),
        (
            r#"{"type":"str","value":"x","truncated":true,"size":300,"extra":1}"#,
            r#"{"type":"str","value":"x","truncated":true,"size":300}"#,
        ),
        (
            r#"{"type":"str","value":"x","truncated":false}"#,
            r#"{"type":"str","value":"x"}"#,
        ),
        (
            r#"{"type":"Foo","is_null":true,"value":"ignored"}"#,
            r#"{"type":"Foo","is_null":true}"#,
        ),
        (
            r#"{"type":"Obj","not_captured_reason":"depth","value":"ignored"}"#,
            r#"{"type":"Obj","not_captured_reason":"depth"}"#,
        ),
        (
            r#"{"type":"list","elements":[{"type":"int","value":"1"},{"type":"int","value":"2"}]}"#,
            r#"{"type":"list","element_count":2,"first_element":{"type":"int","value":"1"}}"#,
        ),
        (
            r#"{"type":"list","elements":[]}"#,
            r#"{"type":"list","element_count":0}"#,
        ),
        (
            r#"{"type":"map","entries":[{},{}]}"#,
            r#"{"type":"map","entry_count":2}"#,
        ),
        (r#"{"type":"x"}"#, r#"{"type":"x"}"#),
        (r#"{"type":""}"#, r#"{"type":""}"#),
        ("{}", "{}"),
        (r#"{"other":1}"#, r#"{"other":1}"#),
    ];
    for (input, expected) in cases {
        assert_eq!(
            compact(&preview_captured_value(&json(input))),
            expected,
            "{input}"
        );
    }
}

#[test]
fn object_fields_expand_one_level() {
    let preview = preview_captured_value(&json(
        r#"{"type":"Order","size":6,"fields":{"id":{"type":"int","value":"1"},"n":{"is_null":true},"d":{"not_captured_reason":"depth"},"o":{"type":"Addr","fields":{}},"raw":5,"u":{}}}"#,
    ));
    assert_eq!(
        compact(&preview),
        r#"{"type":"Order","fields_preview":{"id":"1","n":null,"d":"<depth>","o":"<Addr>","raw":5,"u":"<object>"},"size":6}"#
    );
}

const SNAPSHOT_MESSAGE: &str = r#"{"traceId":"t1","spanId":"s1","timeUnixNano":"1738608120000000000","attributes":{"aws.di.snapshot_id":"snap-1","aws.di.location_hash":"0123456789abcdef","aws.di.class_name":"Billing","aws.di.method_name":"charge","aws.di.file_path":"/app/billing.py","aws.di.code_unit":"services.billing","aws.di.instrumentation_level":"METHOD","aws.di.instrumentation_type":"BREAKPOINT","aws.di.duration_ms":12},"body":{"stack":[{"file_path":"a.py","function":"f","line_number":3},"junk",{"file_path":"b.py","function":"g","line_number":9},{},{},{},{}],"captures":{"entry":{"arguments":{"amount":{"type":"int","value":"5"}},"locals":{}},"return":{"return_value":{"type":"str","value":"ok"},"throwable":{"type":"ValueError","message":"bad","stacktrace":[1,2]}},"lines":{"10":{"locals":{"x":{"type":"int","value":"1"}}},"2":{"arguments":{"a":{"type":"int","value":"2"}},"return_value":{"type":"int","value":"3"}},"-1":{"throwable":{"type":"E","message":"m"}},"bad":5}}}}"#;

fn result_of(message: &str) -> Json {
    Json::object(vec![
        ("@timestamp", Json::string("2025-02-03 18:42:01.000")),
        ("@message", Json::string(message)),
    ])
}

#[test]
fn snapshot_fields_summarize_an_otlp_record() {
    let parsed = parse_snapshot_fields(&result_of(SNAPSHOT_MESSAGE));
    let field = |name: &str| compact(parsed.get(name).unwrap_or_else(|| panic!("missing {name}")));

    assert_eq!(field("@timestamp"), "\"2025-02-03 18:42:01.000\"");
    assert_eq!(field("snapshot_id"), "\"snap-1\"");
    assert_eq!(field("timeUnixNano"), "\"1738608120000000000\"");
    assert_eq!(field("duration_ms"), "12");
    assert_eq!(field("location_hash"), "\"0123456789abcdef\"");
    assert_eq!(
        field("location"),
        r#"{"class_name":"Billing","method_name":"charge","file_path":"/app/billing.py","code_unit":"services.billing","instrumentation_level":"METHOD","instrumentation_type":"BREAKPOINT"}"#
    );
    assert_eq!(field("trace"), r#"{"traceId":"t1","spanId":"s1"}"#);
    // Only the first five frames count, and non-object frames are skipped
    // (but still counted in `stack_frame_count`).
    assert_eq!(
        field("stack_preview"),
        r#"[{"file_path":"a.py","function":"f","line_number":3},{"file_path":"b.py","function":"g","line_number":9},{"file_path":null,"function":null,"line_number":null},{"file_path":null,"function":null,"line_number":null}]"#
    );
    assert_eq!(field("stack_frame_count"), "7");
    assert_eq!(field("entry_argument_names"), r#"["amount"]"#);
    assert_eq!(
        field("entry_arguments"),
        r#"{"amount":{"type":"int","value":"5"}}"#
    );
    assert_eq!(field("entry_local_names"), "[]");
    assert_eq!(field("entry_locals"), "{}");
    assert_eq!(field("return_value"), r#"{"type":"str","value":"ok"}"#);
    assert_eq!(
        field("throwable"),
        r#"{"type":"ValueError","message":"bad","stacktrace_frame_count":2}"#
    );
    // Numeric line keys sort by value, the rest lexically after them.
    assert_eq!(field("line_numbers"), r#"["2","10","-1"]"#);
    assert_eq!(field("line_locals"), r#"{"10":["x"]}"#);
    assert_eq!(
        field("line_local_previews"),
        r#"{"10":{"x":{"type":"int","value":"1"}}}"#
    );
    assert_eq!(field("line_arguments"), r#"{"2":["a"]}"#);
    assert_eq!(
        field("line_argument_previews"),
        r#"{"2":{"a":{"type":"int","value":"2"}}}"#
    );
    assert_eq!(
        field("line_return_values"),
        r#"{"2":{"type":"int","value":"3"}}"#
    );
    assert_eq!(
        field("line_throwables"),
        r#"{"-1":{"type":"E","message":"m","stacktrace_frame_count":0}}"#
    );
    assert_eq!(
        parsed.get("raw_snapshot"),
        Some(&json(SNAPSHOT_MESSAGE)),
        "raw snapshot is kept for callers that drop it"
    );
}

#[test]
fn unparseable_snapshots_degrade_to_empty_fields() {
    for message in ["not json", "[1,2]", "\"text\"", ""] {
        let parsed = parse_snapshot_fields(&result_of(message));
        assert_eq!(
            parsed.get("@timestamp"),
            Some(&Json::string("2025-02-03 18:42:01.000"))
        );
        assert_eq!(parsed.get("snapshot_id"), Some(&Json::Null), "{message}");
        assert_eq!(parsed.get("return_value"), Some(&Json::Null));
        assert_eq!(parsed.get("throwable"), Some(&Json::Null));
        assert_eq!(parsed.get("line_numbers"), Some(&Json::Array(Vec::new())));
        assert_eq!(parsed.get("line_locals"), Some(&Json::Object(Vec::new())));
        assert_eq!(parsed.get("line_arguments"), Some(&Json::Null));
        assert_eq!(parsed.get("line_return_values"), Some(&Json::Null));
        assert_eq!(parsed.get("line_throwables"), Some(&Json::Null));
        assert_eq!(parsed.get("stack_frame_count"), Some(&Json::Int(0)));
        assert_eq!(parsed.get("raw_snapshot"), Some(&Json::Object(Vec::new())));
    }
    // A missing or non-string message reads the same way.
    let parsed = parse_snapshot_fields(&json(r#"{"@message":7}"#));
    assert_eq!(parsed.get("@timestamp"), Some(&Json::Null));
    assert_eq!(parsed.get("raw_snapshot"), Some(&Json::Object(Vec::new())));
}

#[test]
fn mistyped_sections_are_treated_as_empty() {
    let parsed = parse_snapshot_fields(&result_of(
        r#"{"attributes":[1],"body":{"stack":"x","captures":{"entry":5,"return":{"throwable":[],"return_value":null},"lines":{"3":{"locals":[],"arguments":{}}}}}}"#,
    ));
    assert_eq!(parsed.get("snapshot_id"), Some(&Json::Null));
    assert_eq!(parsed.get("stack_frame_count"), Some(&Json::Int(0)));
    assert_eq!(
        parsed.get("entry_argument_names"),
        Some(&Json::Array(Vec::new()))
    );
    assert_eq!(parsed.get("return_value"), Some(&Json::Null));
    assert_eq!(parsed.get("throwable"), Some(&Json::Null));
    assert_eq!(parsed.get("line_numbers"), Some(&Json::Array(Vec::new())));
}
