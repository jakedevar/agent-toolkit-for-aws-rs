//! Regression tests for #4634 (caller text must not inject terminal controls
//! or forge report lines into diagnostics) and #4635 (`UtcTime::from_civil`
//! rejects an out-of-range microsecond). Fixtures are literals.

use toolkit_core::{parse_json, Json};

use super::errors::{render_client_error, render_failure, translate_aws_error, AwsFailure, Prose};
use super::location::{location_from_response, render_location_block};
use super::pytext::{safe_block, safe_json, safe_opt, safe_text};
use super::timefmt::{Civil, UtcTime};
use super::validation::{
    format_code_location_troubleshooting, normalize_instrumentation_type, validate_capture_names,
    validate_location_inputs, validate_snapshot_signal,
};

/// A hostile value: ESC `[2J` clears a terminal, the newline forges a line.
const HOSTILE: &str = "REVIEW_SENTINEL\u{1b}[2J\nFORGED_REPORT";
/// What a diagnostic must show instead.
const ESCAPED: &str = "REVIEW_SENTINEL\\x1b[2J\\nFORGED_REPORT";

/// No control character other than the newline survives, and no line of the
/// text begins with the forged report marker.
fn assert_safe(text: &str) {
    for c in text.chars() {
        assert!(
            c == '\n' || !c.is_control(),
            "raw control {c:?} in {text:?}"
        );
    }
    for line in text.lines() {
        assert!(
            !line.starts_with("FORGED_REPORT"),
            "forged line in {text:?}"
        );
    }
}

fn json(text: &str) -> Json {
    parse_json(text).expect("valid fixture json")
}

// ───────────────────────────── helpers ─────────────────────────────

#[test]
fn safe_text_escapes_controls_and_leaves_ordinary_text_alone() {
    assert_eq!(safe_text(HOSTILE), ESCAPED);
    assert_eq!(safe_text("a\r\tb\u{7f}c\u{85}d"), "a\\r\\tb\\x7fc\\x85d");
    assert_eq!(
        safe_text("x\u{2028}y\u{202e}z\u{feff}"),
        "x\\u2028y\\u202ez\\ufeff"
    );
    assert_eq!(safe_text("naïve — 日本語 👍"), "naïve — 日本語 👍");
    assert_eq!(safe_text("C:\\path\\n"), "C:\\path\\n");
    assert_eq!(safe_opt(None), "None");
    assert_eq!(safe_opt(Some(HOSTILE)), ESCAPED);
    assert_eq!(safe_json(&Json::string(HOSTILE)), ESCAPED);
    assert_eq!(safe_json(&Json::Int(7)), "7");
}

#[test]
fn safe_block_keeps_layout_but_escapes_terminal_controls() {
    assert_eq!(safe_block("a\n\tb"), "a\n\tb");
    assert_eq!(safe_block("a\u{1b}[2J\r\nb\u{9b}"), "a\\x1b[2J\\r\nb\\x9b");
}

// ───────────────────────────── validation ─────────────────────────────

#[test]
fn validation_messages_never_echo_raw_controls() {
    let (_, error) = normalize_instrumentation_type(Some(HOSTILE));
    assert_eq!(
        error.expect("rejected"),
        format!(
            "ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: {ESCAPED})"
        )
    );
    assert_eq!(
        validate_snapshot_signal(Some(HOSTILE)).expect("rejected"),
        format!("ERROR: signal_type must be SNAPSHOT for this API (received: {ESCAPED})")
    );
    assert_eq!(
        validate_capture_names(HOSTILE, Some(&[])).expect("rejected"),
        format!(
            "ERROR: {ESCAPED} must contain at least one name if provided. Omit it to capture none."
        )
    );
    assert_eq!(
        validate_capture_names(HOSTILE, Some(&["*".to_string()])).expect("rejected"),
        format!(
            "ERROR: {ESCAPED} does not support the wildcard \"*\". \
             List explicit names, or omit it to capture none."
        )
    );
}

#[test]
fn location_input_errors_and_troubleshooting_escape_every_received_field() {
    let message = validate_location_inputs(
        HOSTILE,
        HOSTILE,
        Some(HOSTILE),
        Some(HOSTILE),
        Some(HOSTILE),
        Some(0),
    )
    .expect("invalid inputs");
    assert_safe(&message);
    assert!(message.contains(&format!(
        "language must be Python, Java, or JavaScript (received: {ESCAPED})."
    )));
    let lines: Vec<&str> = message.lines().collect();
    for field in [
        "language",
        "file_path",
        "code_unit",
        "class_name",
        "method_name",
    ] {
        let expected = format!("- {field}={ESCAPED}");
        assert!(lines.contains(&expected.as_str()), "{field}: {message}");
    }
    assert!(lines.contains(&"- line_number=0"));

    let direct = format_code_location_troubleshooting(
        Some(HOSTILE),
        Some(HOSTILE),
        None,
        Some(HOSTILE),
        None,
        Some(3),
    );
    assert_safe(&direct);
    assert!(direct.contains(&format!("- file_path={ESCAPED}")));
    assert!(direct.contains("- code_unit=None"));
}

#[test]
fn java_suggestion_escapes_the_split_class_name() {
    let message = validate_location_inputs(
        "Java",
        "/a/B.java",
        None,
        Some("com.x\u{1b}\nFORGED_REPORT.Order"),
        Some("run"),
        None,
    )
    .expect("invalid inputs");
    assert_safe(&message);
    assert!(
        message.contains("Use code_unit=\"com.x\\x1b\\nFORGED_REPORT\" and class_name=\"Order\".")
    );
}

// ───────────────────────────── error translation ─────────────────────────────

fn context() -> Vec<(&'static str, Json)> {
    vec![
        ("service\u{1b}[2J\nFORGED_REPORT", Json::string(HOSTILE)),
        (
            "nested",
            json(r#"["a\u001b[2J\nFORGED_REPORT", {"k\n": "v\r"}]"#),
        ),
    ]
}

#[test]
fn every_failure_template_escapes_caller_text() {
    let attempted = context();
    let failures = [
        AwsFailure::Client {
            code: Some(HOSTILE.to_string()),
            message: Some(HOSTILE.to_string()),
            display: String::new(),
        },
        AwsFailure::Client {
            code: None,
            message: None,
            display: HOSTILE.to_string(),
        },
        AwsFailure::EndpointConnection(HOSTILE.to_string()),
        AwsFailure::Timeout(HOSTILE.to_string()),
        AwsFailure::Credentials {
            type_name: HOSTILE.to_string(),
            message: HOSTILE.to_string(),
        },
        AwsFailure::Other(HOSTILE.to_string()),
    ];
    for failure in &failures {
        let rendered = translate_aws_error(failure, HOSTILE, &attempted);
        assert_safe(&rendered);
        assert!(
            rendered.starts_with(&format!("Failed to {ESCAPED}\n")),
            "{rendered}"
        );
        assert!(rendered.contains(ESCAPED));
        assert!(rendered.contains(&format!("- service\\x1b[2J\\nFORGED_REPORT: {ESCAPED}")));
        assert!(rendered.contains("- nested: ['a\\x1b[2J\\nFORGED_REPORT', {'k\\n': 'v\\r'}]"));
    }
}

#[test]
fn client_error_prose_escapes_terminal_controls_but_keeps_its_layout() {
    let failure = AwsFailure::Client {
        code: Some("ValidationException".to_string()),
        message: Some(HOSTILE.to_string()),
        display: String::new(),
    };
    let prose = Prose {
        attempted_label: Some("TRIED\u{1b}[2J:"),
        possible_causes: &["cause\u{1b}[2J"],
        troubleshooting: &["step\u{7}"],
        trailer: Some("LOCATION\u{1b}[2J\nsecond line"),
    };
    let attempted = context();
    for rendered in [
        render_client_error(&failure, HOSTILE, &attempted, &prose),
        render_failure(&failure, HOSTILE, &attempted, &prose),
    ] {
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
        assert!(rendered.contains("TRIED\\x1b[2J:\n"));
        assert!(rendered.contains("1. cause\\x1b[2J\n"));
        assert!(rendered.contains("1. step\\x07"));
        assert!(rendered.ends_with("\n\nLOCATION\\x1b[2J\nsecond line"));
        assert!(rendered.contains(&format!("Error: ValidationException - {ESCAPED}\n")));
    }
}

// ───────────────────────────── location blocks ─────────────────────────────

#[test]
fn location_blocks_escape_values_keys_and_hashes() {
    let code = location_from_response(&json(
        r#"{"CodeLocation": {"Language": "Py\u001b[2J\nFORGED_REPORT",
            "FilePath": "/a\r.py", "ClassName": "C\u0007",
            "Weird\u001b[2J\nFORGED_REPORT": "v\nFORGED_REPORT"}}"#,
    ));
    let rendered = render_location_block(&code, Some("ab\u{1b}[2J\nFORGED_REPORT"));
    assert_safe(&rendered);
    assert!(rendered.contains("- LocationHash: ab\\x1b[2J\\nFORGED_REPORT\n"));
    assert!(rendered.contains("- Language: Py\\x1b[2J\\nFORGED_REPORT\n"));
    assert!(rendered.contains("- Weird\\x1b[2J\\nFORGED_REPORT: v\\nFORGED_REPORT\n"));
    assert!(rendered.contains("/a\\r.py"));

    let unknown = location_from_response(&json(r#"{"K\u001b\nFORGED_REPORT": "v\u001b"}"#));
    let rendered = render_location_block(&unknown, None);
    assert_safe(&rendered);
    assert!(rendered.contains("- K\\x1b\\nFORGED_REPORT: v\\x1b\n"));

    let hash = location_from_response(&json(r#"{"LocationHash": "0123456789abcdef"}"#));
    assert_safe(&render_location_block(&hash, None));
}

// ───────────────────────────── #4635: microseconds ─────────────────────────────

fn at(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64, micro: i64) -> Civil {
    Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
        micro,
    }
}

#[test]
fn from_civil_rejects_a_microsecond_outside_0_to_999999() {
    let endpoints = [
        (at(1, 1, 1, 0, 0, 0, 0), -62_135_596_800_000_000_i64),
        (at(9999, 12, 31, 23, 59, 59, 0), 253_402_300_799_000_000_i64),
    ];
    for (base, base_micros) in endpoints {
        for micro in [-1, 1_000_000, i64::MIN, i64::MAX, i64::MAX - 999_999] {
            assert_eq!(
                UtcTime::from_civil(Civil { micro, ..base }),
                Err("microsecond must be in 0..999999".to_string()),
                "micro {micro} at {base:?}"
            );
        }
        for micro in [0, 1, 999_999] {
            let time = UtcTime::from_civil(Civil { micro, ..base }).expect("in range");
            assert_eq!(time.epoch_micros(), base_micros + micro);
        }
    }
    let last = UtcTime::from_civil(at(9999, 12, 31, 23, 59, 59, 999_999)).expect("last instant");
    assert_eq!(last.epoch_micros(), 253_402_300_799_999_999);
    assert_eq!(last.format_seconds_z(), "9999-12-31T23:59:59Z");
    let first = UtcTime::from_civil(at(1, 1, 1, 0, 0, 0, 0)).expect("first instant");
    assert_eq!(first.epoch_micros(), -62_135_596_800_000_000);
}

#[test]
fn from_civil_checks_the_microsecond_after_the_other_fields() {
    // Python's `datetime(...)` reports the first offending field in order.
    assert_eq!(
        UtcTime::from_civil(at(2025, 2, 3, 4, 5, 60, 1_000_000)),
        Err("second must be in 0..59".to_string())
    );
}
