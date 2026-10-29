//! The three commands end to end: routing, argument rules, recorded input
//! and the text they print.

use super::*;

const PINNED: &str = "2026-01-31T12:30:00Z";

#[test]
fn the_commands_are_registered_strict_and_not_passthrough() {
    let mut seen = 0;
    for spec in crate::COMMANDS {
        if spec.path.first() == Some(&"marketplace") {
            seen += 1;
            assert!(spec.strict_values, "{:?}", spec.path);
            assert!(!spec.passthrough, "{:?}", spec.path);
        }
    }
    assert_eq!(seen, 3);
    let help = toolkit(&["help"]);
    assert!(help.stdout.contains("toolkit marketplace handler NAME"));
    assert!(help
        .stdout
        .contains("never calls\n                BatchMeterUsage"));
}

#[test]
fn handler_arguments_are_checked() {
    let none = toolkit(&["marketplace", "handler"]);
    assert_eq!(none.status, 2);
    assert!(none
        .stderr
        .contains("a handler name is required: discoverer, aggregator"));
    let unknown = toolkit(&["marketplace", "handler", "nope"]);
    assert_eq!(unknown.status, 2);
    assert!(unknown.stderr.contains("the handler must be one of"));
    let two = toolkit(&["marketplace", "handler", "cleanup", "extra"]);
    assert_eq!(two.status, 2);
    let bad_now = toolkit(&["marketplace", "handler", "cleanup", "--now", "yesterday"]);
    assert_eq!(bad_now.status, 2);
    let bad_seed = toolkit(&["marketplace", "handler", "cleanup", "--seed", "-1"]);
    assert_eq!(bad_seed.status, 2);
}

#[test]
fn a_handler_runs_over_a_config_an_event_and_a_recording() {
    let config = TempFile::new("config", r#"{"SUBSCRIBERS_TABLE": "subs"}"#);
    let items = format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}"}}]}}"#
    );
    let recording = TempFile::new(
        "rec",
        &format!(r#"{{"calls": [{}, {}]}}"#, query(&items), update()),
    );
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "deprovision-cleanup",
        "--config",
        &config.path(),
        "--recorded",
        &recording.path(),
        "--now",
        PINNED,
    ]);
    assert_eq!(cli.status, 0, "{}", cli.stderr);
    assert_eq!(cli.stdout, "{\n  \"finalized\": 1\n}\n");
    assert!(cli.stderr.contains(
        "INFO Finalized deprovisioning->inactive for licenseArn=****c123 (window elapsed)"
    ));
    assert!(!cli.stderr.contains("no live AWS client"));
}

#[test]
fn metric_lines_go_to_the_log_stream_and_the_result_to_stdout() {
    let config = TempFile::new(
        "config",
        r#"{"SUBSCRIBERS_TABLE": "s", "PRODUCT_CODE": "p", "USAGE_TABLE": "u", "AGGREGATED_USAGE_TABLE": "a"}"#,
    );
    let row = format!(
        r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId_dimension_timestamp": "{ACCOUNT}#D#2026-01-30T12:00:01", "meteringPending": "x"}}"#
    );
    let mut calls = vec![
        query(&format!(
            r#"{{"Items": [{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}", "dimension": "D"}}]}}"#
        )),
        query(&format!(r#"{{"Items": [{row}]}}"#)),
        update(),
    ];
    calls.extend((0..6).map(|_| query(r#"{"Items": []}"#)));
    let recording = TempFile::new("rec", &format!(r#"{{"calls": [{}]}}"#, calls.join(",")));
    let event = TempFile::new("event", r#"{"mode": "ageout", "shard": 3}"#);
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "discoverer",
        "--config",
        &config.path(),
        "--event",
        &event.path(),
        "--recorded",
        &recording.path(),
        "--now",
        PINNED,
    ]);
    assert_eq!(cli.status, 0, "{}", cli.stderr);
    assert_eq!(
        j(&cli.stdout).to_compact_string(),
        r#"{"shard":3,"expired":1}"#
    );
    assert!(cli
        .stderr
        .lines()
        .any(|l| l.starts_with(r#"{"_aws": {"Timestamp": 1769862600000"#)
            && l.contains("UsageAggregationExpired")));
    assert!(cli
        .stderr
        .lines()
        .any(|l| l.starts_with("WARNING AGE-OUT:")));
    assert!(
        !cli.stdout.contains("_aws"),
        "only the handler result is on stdout"
    );
}

#[test]
fn emf_diagnostics_on_stderr_never_echo_caller_text() {
    // A rejected group emits `UsageRecordRejected` with the configured product
    // code, mode and namespace, all of them caller text.
    let hostile = core_logic::hostile();
    let mut members: Vec<(String, Json)> = CFG
        .iter()
        .filter(|(name, _)| *name != "PRODUCT_CODE")
        .map(|(name, value)| ((*name).to_string(), Json::string(*value)))
        .collect();
    for name in ["PRODUCT_CODE", "METERING_MODE", "METRIC_NAMESPACE"] {
        members.push((name.to_string(), Json::string(hostile.clone())));
    }
    let config = TempFile::new("config", &Json::Object(members).to_compact_string());
    let row = format!(
        r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId_dimension_timestamp": "{ACCOUNT}#Requests#2026-01-31T11:00:01", "meteringPending": "2026-01-31T11", "quantity": -1}}"#
    );
    let body = format!(
        r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}", "dimension": "Requests", "hourBucket": "2026-01-31T11"}}"#
    );
    let event = TempFile::new(
        "event",
        &format!(
            r#"{{"Records": [{{"messageId": "m1", "body": {}}}]}}"#,
            Json::string(body).to_compact_string()
        ),
    );
    let calls = [query(&format!(r#"{{"Items": [{row}]}}"#)), update()];
    let recording = TempFile::new("rec", &format!(r#"{{"calls": [{}]}}"#, calls.join(",")));
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "aggregator",
        "--config",
        &config.path(),
        "--event",
        &event.path(),
        "--recorded",
        &recording.path(),
        "--now",
        PINNED,
    ]);
    assert_eq!(cli.status, 0, "{}", cli.stderr);
    assert!(
        !cli.stderr
            .to_lowercase()
            .contains("fake-bearer-4396-emf-sentinel"),
        "{}",
        cli.stderr
    );
    let metrics: Vec<&str> = cli.stderr.lines().filter(|l| l.starts_with('{')).collect();
    assert!(!metrics.is_empty(), "{}", cli.stderr);
    for line in metrics {
        core_logic::assert_clean_emf(line);
    }
    assert!(cli.stderr.contains("UsageRecordRejected"));
}

#[test]
fn without_a_recording_nothing_is_sent_and_the_run_fails() {
    let config = TempFile::new("config", r#"{"SUBSCRIBERS_TABLE": "subs"}"#);
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "deprovision-cleanup",
        "--config",
        &config.path(),
    ]);
    assert_eq!(cli.status, 1);
    assert!(cli
        .stderr
        .contains("note: this build includes no live AWS client; pass --recorded FILE"));
    assert!(cli
        .stderr
        .contains("error: deprovision-cleanup failed: aws_connect"));
    assert!(cli.stdout.is_empty());
}

#[test]
fn a_handler_failure_names_a_class_and_never_a_response_value() {
    let config = TempFile::new("config", r#"{"SUBSCRIBERS_TABLE": "subs"}"#);
    let recording = TempFile::new(
        "rec",
        &format!(
            r#"{{"calls": [{}]}}"#,
            fail("dynamodb", "Query", "AccessDeniedException")
        ),
    );
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "deprovision-cleanup",
        "--config",
        &config.path(),
        "--recorded",
        &recording.path(),
        "--now",
        PINNED,
    ]);
    assert_eq!(cli.status, 1);
    assert!(cli
        .stderr
        .contains("error: deprovision-cleanup failed: aws_service_error (AccessDeniedException)"));
    assert!(!cli.stderr.contains(SECRET) && !cli.stdout.contains(SECRET));
}

#[test]
fn a_recording_with_unused_steps_is_rejected_after_the_run() {
    let config = TempFile::new("config", r#"{"SUBSCRIBERS_TABLE": "subs"}"#);
    let recording = TempFile::new(
        "rec",
        &format!(
            r#"{{"calls": [{}, {}]}}"#,
            query(r#"{"Items": []}"#),
            query(r#"{"Items": []}"#)
        ),
    );
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "deprovision-cleanup",
        "--config",
        &config.path(),
        "--recorded",
        &recording.path(),
        "--now",
        PINNED,
    ]);
    assert_eq!(cli.status, 1);
    assert!(cli.stderr.contains("1 recorded call(s) were not used"));
}

#[test]
fn malformed_recordings_name_the_entry_and_member_never_a_value() {
    let config = TempFile::new("config", r#"{"SUBSCRIBERS_TABLE": "subs"}"#);
    let bad = [
        (r#"{"calls": 5}"#, "\"calls\" must be an array"),
        (r#"{}"#, "need a \"calls\" array"),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "response": {}}]}"#,
            "recorded call 0: \"service\" must be",
        ),
        (
            r#"{"calls": [{"service": "sqs", "operation": "", "response": {}}]}"#,
            "\"operation\" must be a non-empty string",
        ),
        (
            r#"{"calls": [{"service": "sqs", "operation": "X"}]}"#,
            "exactly one of \"response\"",
        ),
        (
            r#"{"calls": [{"service": "sqs", "operation": "X", "request": null, "response": {}}]}"#,
            "\"request\" must be an object when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "sqs", "operation": "X", "error": {"code": 5}}]}"#,
            "error \"code\" must be a string when present (it is integer)",
        ),
        (
            r#"{"calls": [{"service": "sqs", "operation": "X", "error": {"kind": "weird"}}]}"#,
            "\"error.kind\" must be client, connect or no_credentials",
        ),
    ];
    for (document, reason) in bad {
        let recording = TempFile::new("rec", document);
        let cli = toolkit(&[
            "marketplace",
            "handler",
            "cleanup",
            "--config",
            &config.path(),
            "--recorded",
            &recording.path(),
        ]);
        assert_eq!(cli.status, 1, "{document}");
        assert!(cli.stderr.contains(reason), "{document}: {}", cli.stderr);
    }
    // The three error kinds replay as failures of their class.
    for (kind, class) in [
        ("connect", "aws_connect"),
        ("no_credentials", "aws_no_credentials"),
        ("client", "aws_service_error"),
    ] {
        let document = format!(
            r#"{{"calls": [{{"service": "dynamodb", "operation": "Query", "error": {{"kind": "{kind}"}}}}]}}"#
        );
        let recording = TempFile::new("rec", &document);
        let cli = toolkit(&[
            "marketplace",
            "handler",
            "deprovision-cleanup",
            "--config",
            &config.path(),
            "--recorded",
            &recording.path(),
        ]);
        assert!(
            cli.stderr.contains(&format!("failed: {class}")),
            "{kind}: {}",
            cli.stderr
        );
    }
}

#[test]
fn config_and_event_files_are_checked() {
    let not_strings = TempFile::new("config", r#"{"SUBSCRIBERS_TABLE": 5}"#);
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "cleanup",
        "--config",
        &not_strings.path(),
    ]);
    assert_eq!(cli.status, 1);
    assert!(
        cli.stderr
            .contains("configuration member \"SUBSCRIBERS_TABLE\"")
            || cli
                .stderr
                .contains("configuration member SUBSCRIBERS_TABLE")
    );
    assert!(cli.stderr.contains("must be a string (it is integer)"));
    let list = TempFile::new("config", "[1]");
    assert!(toolkit(&[
        "marketplace",
        "handler",
        "cleanup",
        "--config",
        &list.path()
    ])
    .stderr
    .contains("must be a JSON object of strings"));
    let broken = TempFile::new("event", "{oops");
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "cleanup",
        "--event",
        &broken.path(),
    ]);
    assert_eq!(cli.status, 1);
    assert!(cli.stderr.contains("invalid JSON"));
}

#[test]
fn caller_paths_are_redacted_before_they_are_shown() {
    let cli = toolkit(&[
        "marketplace",
        "handler",
        "cleanup",
        "--event",
        "/nonexistent/AKIAIOSFODNN7EXAMPLE/event.json",
    ]);
    assert_eq!(cli.status, 1);
    assert!(cli.stderr.contains("cannot read"), "{}", cli.stderr);
    assert!(
        !cli.stderr.contains("AKIAIOSFODNN7EXAMPLE"),
        "{}",
        cli.stderr
    );
    let escaped = toolkit(&[
        "marketplace",
        "handler",
        "cleanup",
        "--event",
        "/nonexistent/a\u{1b}[31mb.json",
    ]);
    assert!(!escaped.stderr.contains('\u{1b}'));
}

// ---- test-events ------------------------------------------------------------------

fn events_words<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = vec![
        "marketplace",
        "test-events",
        "--product-code",
        "prod123",
        "--test-accounts",
        "111122223333, 444455556666",
        "--event-source",
        "beta.agreement-marketplace",
        "--now",
        PINNED,
        "--seed",
        "11",
    ];
    words.extend_from_slice(extra);
    words
}

#[test]
fn dry_print_prints_the_events_without_publishing() {
    let cli = toolkit(&events_words(&["--dry-print", "--scenario", "duplicate"]));
    assert_eq!(cli.status, 0, "{}", cli.stderr);
    let printed = j(&cli.stdout);
    let text = printed.to_compact_string();
    assert_eq!(text.matches(r#""DetailType":"License Updated""#).count(), 2);
    assert!(cli
        .stdout
        .starts_with("[\n  {\n    \"Source\": \"beta.agreement-marketplace\""));
    assert!(
        !cli.stderr.contains("no live AWS client"),
        "a dry print needs no client"
    );
    // The same seed prints the same events.
    assert_eq!(
        toolkit(&events_words(&["--dry-print", "--scenario", "duplicate"])).stdout,
        cli.stdout
    );
}

#[test]
fn test_events_publishes_only_through_a_recording() {
    let recording = TempFile::new(
        "rec",
        &format!(
            r#"{{"calls": [{}]}}"#,
            step("events", "PutEvents", None, "{}")
        ),
    );
    let path = recording.path();
    let cli = toolkit(&events_words(&[
        "--scenario",
        "duplicate",
        "--event-bus-name",
        "nonprod-bus",
        "--region",
        "us-east-1",
        "--recorded",
        path.as_str(),
    ]));
    assert_eq!(cli.status, 0, "{}", cli.stderr);
    assert_eq!(cli.stdout, "Published 2 'duplicate' test event(s).\n");
    let live = toolkit(&events_words(&[]));
    assert_eq!(live.status, 1);
    assert!(live.stderr.contains("publishing failed: aws_connect"));
    assert!(live.stdout.is_empty());
    let failing = TempFile::new(
        "rec",
        &format!(
            r#"{{"calls": [{}]}}"#,
            step("events", "PutEvents", None, r#"{"FailedEntryCount": 1}"#)
        ),
    );
    let failing_path = failing.path();
    let failed = toolkit(&events_words(&["--recorded", failing_path.as_str()]));
    assert_eq!(failed.status, 1);
    assert!(failed
        .stderr
        .contains("PutEvents reported 1 failed entr(ies)"));
}

#[test]
fn test_events_refuses_unsafe_or_incomplete_arguments() {
    let missing = toolkit(&["marketplace", "test-events"]);
    assert_eq!(missing.status, 2);
    assert!(missing.stderr.contains(
        "the following arguments are required: --product-code, --test-accounts, --event-source"
    ));
    let reserved = toolkit(&[
        "marketplace",
        "test-events",
        "--product-code",
        "p",
        "--test-accounts",
        "1",
        "--event-source",
        "aws.agreement-marketplace",
    ]);
    assert_eq!(reserved.status, 2);
    assert!(reserved.stderr.contains("reserved 'aws.' prefix"));
    assert_eq!(toolkit(&events_words(&["--scenario", "nope"])).status, 2);
    assert_eq!(toolkit(&events_words(&["--count", "many"])).status, 2);
    assert_eq!(toolkit(&events_words(&["--region", "us east"])).status, 2);
    assert_eq!(
        toolkit(&events_words(&["--event-bus-name", "bad name!"])).status,
        2
    );
    let no_accounts = toolkit(&[
        "marketplace",
        "test-events",
        "--product-code",
        "p",
        "--test-accounts",
        " , ",
        "--event-source",
        "beta.x",
        "--dry-print",
    ]);
    assert_eq!(no_accounts.status, 1);
    assert!(no_accounts.stderr.contains("must be non-empty"));
}

#[test]
fn query_metering_arguments_are_checked() {
    let no_region = toolkit(&["marketplace", "query-metering"]);
    assert_eq!(no_region.status, 2);
    assert!(no_region
        .stderr
        .contains("the following arguments are required: --region"));
    assert_eq!(
        toolkit(&["marketplace", "query-metering", "--region", "mars"]).status,
        2
    );
    let bad_type = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--query-type",
        "all",
    ]);
    assert_eq!(bad_type.status, 2);
    assert!(bad_type.stderr.contains(
        "--query-type must be one of: detail, summary, count, list_failures, top_customers"
    ));
    assert_eq!(
        toolkit(&[
            "marketplace",
            "query-metering",
            "--region",
            "us-east-1",
            "--days",
            "x"
        ])
        .status,
        2
    );
    // Without a recording the unreachable service fails the query.
    let live = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
    ]);
    assert_eq!(live.status, 1);
    assert!(live.stderr.contains("aws_connect"));
    assert!(live.stdout.is_empty());
}

/// Integration round 20 (#5019): the extended diag contract found
/// `--test-accounts` split before redaction and `--event-source` and
/// `--product-code` written raw into the printed events. Each is now
/// redacted whole first; the ordinary dry print is unchanged (see
/// `dry_print_prints_the_events_without_publishing`).
#[test]
fn test_events_values_are_redacted_whole_before_they_reach_the_events() {
    let list = "111122223333,https://user:FAKE_SECRET_head,FAKE_SECRET_tail@example.invalid/x";
    let mut words = events_words(&["--dry-print", "--scenario", "duplicate"]);
    let at = words
        .iter()
        .position(|word| *word == "--test-accounts")
        .expect("accounts flag");
    words[at + 1] = list;
    let cli = toolkit(&words);
    assert_eq!(cli.status, 0, "{}", cli.stderr);
    assert!(!cli.stdout.contains("FAKE_SECRET"), "{}", cli.stdout);
    assert!(!cli.stderr.contains("FAKE_SECRET"), "{}", cli.stderr);
    let code = toolkit(&[
        "marketplace",
        "test-events",
        "--product-code",
        "password=Zq9",
        "--test-accounts",
        "111122223333",
        "--event-source",
        "beta.agreement-marketplace",
        "--now",
        PINNED,
        "--seed",
        "11",
        "--dry-print",
    ]);
    assert!(!code.stdout.contains("Zq9"), "{}", code.stdout);
    assert!(!code.stderr.contains("Zq9"), "{}", code.stderr);
}
