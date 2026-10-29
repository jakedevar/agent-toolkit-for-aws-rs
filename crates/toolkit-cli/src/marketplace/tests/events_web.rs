//! `test_event_publisher.py` and the web helpers of `register.py`.

use super::*;
use crate::marketplace::events::{
    build_batch, build_event, dumps_indented_ascii, require_event_source, Rng, SCENARIOS,
};
use crate::marketplace::web::{b64decode, form_values, parse_qs};

fn accounts() -> Vec<String> {
    vec!["111122223333".to_string(), "444455556666".to_string()]
}

fn event(scenario: &str, seed: u64) -> Json {
    build_event(
        scenario,
        &accounts(),
        "prod123",
        at(NOW),
        "beta.agreement-marketplace",
        &mut Rng::new(seed),
    )
    .expect("event")
}

fn detail_of(event: &Json) -> Json {
    j(event
        .get("Detail")
        .and_then(Json::as_str)
        .expect("Detail is a JSON string"))
}

#[test]
fn parse_qs_follows_urllib() {
    let form = parse_qs("a=1&b=%41%C3%A9+x&a=2&empty=&novalue&&=nameless&bad=%zz%4&utf=%ff");
    assert_eq!(form_values(&form, "a"), ["1".to_string(), "2".to_string()]);
    assert_eq!(form_values(&form, "b"), ["Aé x".to_string()]);
    assert!(
        form_values(&form, "empty").is_empty(),
        "blank values are dropped"
    );
    assert!(form_values(&form, "novalue").is_empty());
    assert_eq!(form_values(&form, ""), ["nameless".to_string()]);
    assert_eq!(form_values(&form, "bad"), ["%zz%4".to_string()]);
    assert_eq!(form_values(&form, "utf"), ["\u{fffd}".to_string()]);
    assert!(form_values(&form, "missing").is_empty());
}

#[test]
fn base64_follows_b64decode() {
    assert_eq!(b64decode("aGVsbG8=").expect("decodes"), b"hello");
    assert_eq!(
        b64decode("aGVsbG8").expect_err("padding"),
        "Incorrect padding"
    );
    assert_eq!(
        b64decode("aGVs bG8=\n").expect("whitespace is discarded"),
        b"hello"
    );
    assert_eq!(b64decode("").expect("empty"), b"");
    assert_eq!(
        b64decode("a").expect_err("a lone sextet"),
        "Invalid base64-encoded string"
    );
    assert_eq!(b64decode("TWE=").expect("two bytes"), b"Ma");
    assert_eq!(b64decode("TQ==").expect("one byte"), b"M");
}

#[test]
fn only_a_stage_scoped_source_is_accepted() {
    assert_eq!(
        require_event_source("beta.agreement-marketplace"),
        Ok("beta.agreement-marketplace")
    );
    assert!(require_event_source("")
        .expect_err("empty")
        .contains("there is no default"));
    let reserved = require_event_source("aws.agreement-marketplace").expect_err("reserved");
    assert!(reserved.contains("reserved 'aws.' prefix"));
    assert!(
        !reserved.contains("aws.agreement-marketplace"),
        "the rejected value is not echoed"
    );
    assert!(
        build_event("new", &[], "p", at(NOW), "beta.x", &mut Rng::new(1))
            .expect_err("no accounts")
            .contains("must be non-empty")
    );
    assert!(build_event("new", &accounts(), "p", at(NOW), "aws.x", &mut Rng::new(1)).is_err());
}

#[test]
fn events_have_the_marketplace_shape() {
    for scenario in SCENARIOS {
        let value = event(scenario, 3);
        assert_eq!(
            value.get("Source").and_then(Json::as_str),
            Some("beta.agreement-marketplace")
        );
        assert_eq!(value.get("scenario").and_then(Json::as_str), Some(scenario));
        assert_eq!(
            value.get("time").and_then(Json::as_str),
            Some("2026-01-31T12:30:00Z")
        );
        let detail = detail_of(&value);
        assert_eq!(
            detail
                .get("product")
                .and_then(|p| p.get("code"))
                .and_then(Json::as_str),
            Some("prod123")
        );
        assert_eq!(
            detail
                .get("agreement")
                .and_then(|a| a.get("endTime"))
                .and_then(Json::as_str),
            Some("2027-01-31T12:30:00Z")
        );
        if scenario != "malformed" {
            let account = detail
                .get("acceptor")
                .and_then(|a| a.get("accountId"))
                .and_then(Json::as_str)
                .expect("account");
            assert!(accounts().iter().any(|a| a == account));
            let arn = detail
                .get("license")
                .and_then(|l| l.get("arn"))
                .and_then(Json::as_str)
                .expect("arn");
            assert!(arn.starts_with(&format!("arn:aws:license-manager::{account}:license:l-")));
            assert_eq!(
                arn.len(),
                format!("arn:aws:license-manager::{account}:license:l-").len() + 16
            );
        }
    }
    let new = event("new", 3);
    assert_eq!(
        new.get("DetailType").and_then(Json::as_str),
        Some("License Updated")
    );
    let entitlement = detail_of(&new)
        .get("entitlement")
        .cloned()
        .expect("entitlement");
    assert_eq!(
        entitlement.get("status").and_then(Json::as_str),
        Some("active")
    );
    assert!(
        ["Requests", "GBHours", "Users", "DataProcessedGB"].contains(
            &entitlement
                .get("dimension")
                .and_then(Json::as_str)
                .expect("dimension")
        )
    );
    let deprovision = event("deprovision", 3);
    assert_eq!(
        deprovision.get("DetailType").and_then(Json::as_str),
        Some("License Deprovisioned")
    );
    assert_eq!(
        detail_of(&deprovision)
            .get("entitlement")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{"status":"deprovisioning"}"#)
    );
    assert_eq!(
        event("agreement-ended", 3)
            .get("DetailType")
            .and_then(Json::as_str),
        Some("Purchase Agreement Ended")
    );
    assert!(detail_of(&event("agreement-ended", 3))
        .get("entitlement")
        .is_none());
    assert_eq!(
        event("agreement-amended", 3)
            .get("DetailType")
            .and_then(Json::as_str),
        Some("Purchase Agreement Amended")
    );
    for compound in [
        "multi-region",
        "multi-dimension",
        "burst",
        "duplicate",
        "month-boundary",
    ] {
        assert_eq!(
            event(compound, 3).get("DetailType").and_then(Json::as_str),
            Some("License Updated")
        );
    }
    // The detail is serialized with Python's separators.
    assert!(new
        .get("Detail")
        .and_then(Json::as_str)
        .expect("detail")
        .starts_with(r#"{"acceptor": {"accountId": ""#));
}

#[test]
fn a_malformed_event_drops_the_license_and_corrupts_the_account() {
    let detail = detail_of(&event("malformed", 9));
    assert_eq!(
        detail
            .get("license")
            .map(Json::to_compact_string)
            .as_deref(),
        Some("{}")
    );
    assert_eq!(
        detail
            .get("acceptor")
            .and_then(|a| a.get("accountId"))
            .and_then(Json::as_str),
        Some("not-an-account-id")
    );
}

#[test]
fn the_generator_is_deterministic_per_seed() {
    assert_eq!(event("new", 5), event("new", 5));
    assert_ne!(event("new", 5), event("new", 6));
}

#[test]
fn batches_follow_the_scenario() {
    let batch = |scenario: &str, count: i64| {
        build_batch(
            scenario,
            &accounts(),
            "p",
            count,
            at(NOW),
            "beta.x",
            &mut Rng::new(1),
        )
        .expect("batch")
    };
    assert_eq!(batch("new", 3).len(), 3);
    assert_eq!(batch("new", 0).len(), 0);
    assert_eq!(batch("new", -2).len(), 0);
    assert_eq!(batch("burst", 1).len(), 50);
    assert_eq!(batch("burst", 4).len(), 4);
    let duplicate = batch("duplicate", 1);
    assert_eq!(duplicate.len(), 2);
    assert_eq!(duplicate[0], duplicate[1]);
    assert_eq!(batch("duplicate", 0).len(), 0);
}

fn publisher_config(mode: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        ("METERING_MODE", mode),
        ("TEST_ACCOUNT_ALLOWLIST", "111122223333, 444455556666,"),
        ("EVENT_SOURCE", " beta.agreement-marketplace "),
        ("EVENT_BUS_NAME", "nonprod-bus"),
    ]
}

fn put_events(response: &str) -> String {
    step("events", "PutEvents", None, response)
}

#[test]
fn the_publisher_runs_only_in_a_dry_run_stage() {
    for mode in ["live", ""] {
        let ran = run_with(
            "test-event-publisher",
            &publisher_config(mode),
            r#"{"productCode": "p"}"#,
            &[],
            NOW,
        );
        assert_eq!(
            ran.err(),
            "test_event_publisher may run ONLY in a dry-run (non-prod) stage"
        );
    }
    let unset = run_with(
        "test-event-publisher",
        &[],
        r#"{"productCode": "p"}"#,
        &[],
        NOW,
    );
    assert!(unset.err().contains("dry-run"));
    assert!(unset.sent.is_empty());
}

#[test]
fn the_publisher_needs_a_product_code_accounts_and_a_stage_source() {
    let cfg = publisher_config("Dry-Run");
    let missing = run_with(
        "test-event-publisher",
        &cfg,
        r#"{"scenario": "new"}"#,
        &[],
        NOW,
    );
    assert!(missing
        .err()
        .starts_with("productCode is required in the invocation payload"));
    let empty_product = run_with(
        "test-event-publisher",
        &cfg,
        r#"{"productCode": ""}"#,
        &[],
        NOW,
    );
    assert!(empty_product.err().starts_with("productCode is required"));
    let mut no_source = cfg.clone();
    no_source.retain(|(k, _)| *k != "EVENT_SOURCE");
    assert!(run_with(
        "test-event-publisher",
        &no_source,
        r#"{"productCode": "p"}"#,
        &[],
        NOW
    )
    .err()
    .contains("there is no default"));
    let mut aws_source = cfg.clone();
    aws_source.retain(|(k, _)| *k != "EVENT_SOURCE");
    aws_source.push(("EVENT_SOURCE", "aws.agreement-marketplace"));
    assert!(run_with(
        "test-event-publisher",
        &aws_source,
        r#"{"productCode": "p"}"#,
        &[],
        NOW
    )
    .err()
    .contains("reserved"));
    let mut no_accounts = cfg;
    no_accounts.retain(|(k, _)| *k != "TEST_ACCOUNT_ALLOWLIST");
    let ran = run_with(
        "test-event-publisher",
        &no_accounts,
        r#"{"productCode": "p"}"#,
        &[],
        NOW,
    );
    assert!(ran.err().contains("must be non-empty"));
    assert!(
        ran.sent.is_empty(),
        "nothing is published without an allow-list"
    );
}

#[test]
fn the_publisher_sends_events_in_chunks_of_ten_to_the_configured_bus() {
    let cfg = publisher_config("dry-run");
    let calls = [
        put_events("{}"),
        put_events(r#"{"FailedEntryCount": 0}"#),
        put_events("{}"),
    ];
    let ran = run_with(
        "test-event-publisher",
        &cfg,
        r#"{"productCode": "p1", "scenario": "burst", "count": "25"}"#,
        &calls,
        NOW,
    );
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"scenario":"burst","productCode":"p1","published":25}"#
    );
    let puts = ran.requests("events:PutEvents");
    assert_eq!(puts.len(), 3);
    let sizes: Vec<usize> = puts
        .iter()
        .map(|p| p.to_compact_string().matches(r#""Source""#).count())
        .collect();
    assert_eq!(sizes, vec![10, 10, 5]);
    let first = puts[0].to_compact_string();
    assert!(first.contains(r#""EventBusName":"nonprod-bus""#));
    assert!(first.contains(r#""Source":"beta.agreement-marketplace""#));
    // Only Source, DetailType, Detail and the bus go to EventBridge.
    assert!(!first.contains("scenario") && !first.contains(r#""time""#));
}

#[test]
fn a_failed_entry_is_an_error() {
    let cfg = publisher_config("dry-run");
    let ran = run_with(
        "test-event-publisher",
        &cfg,
        r#"{"productCode": "p"}"#,
        &[put_events(r#"{"FailedEntryCount": 2}"#)],
        NOW,
    );
    assert_eq!(ran.err(), "PutEvents reported 2 failed entr(ies)");
    let wrong = run_with(
        "test-event-publisher",
        &cfg,
        r#"{"productCode": "p"}"#,
        &[put_events(r#"{"FailedEntryCount": "0"}"#)],
        NOW,
    );
    assert!(wrong
        .err()
        .contains("FailedEntryCount must be an integer when present (it is string)"));
    let denied = run_with(
        "test-event-publisher",
        &cfg,
        r#"{"productCode": "p"}"#,
        &[fail("events", "PutEvents", "AccessDeniedException")],
        NOW,
    );
    assert_eq!(denied.err(), "aws_service_error (AccessDeniedException)");
}

#[test]
fn json_dumps_indent_escapes_non_ascii() {
    let value = j(r#"{"a": ["é", "\ud83d\ude00"], "b": {}, "c": []}"#);
    assert_eq!(
        dumps_indented_ascii(&value),
        "{\n  \"a\": [\n    \"\\u00e9\",\n    \"\\ud83d\\ude00\"\n  ],\n  \"b\": {},\n  \"c\": []\n}"
    );
}
