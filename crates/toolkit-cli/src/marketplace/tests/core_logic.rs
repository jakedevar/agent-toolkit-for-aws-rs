//! `metering_core.py`: validation, windows, audit stamps, folding, EMF and the
//! submit retry/bisect rules.

use toolkit_core::Json;

use super::*;
use crate::marketplace::base::{Config, Ctx, Logger};
use crate::marketplace::client::RecordedClient;
use crate::marketplace::core::{
    self, completed_hour_buckets, effective_lock_hours, emf_metric, fold_rows,
    hour_bucket_for_offset, is_previous_month_bucket, mask, previous_month_grace_open,
    quantity_reason, strict_int, validate_group, validate_sort_key, with_audit_timestamps, AggKey,
    Outcomes,
};

#[test]
fn sort_keys_must_be_second_precision_of_the_group() {
    let ok = format!("{ACCOUNT}#Requests#2026-01-31T11:05:07");
    assert_eq!(validate_sort_key(&ok, ACCOUNT, "Requests"), None);
    let cases = [
        ("", "MalformedSortKey"),
        ("only#two", "MalformedSortKey"),
        (
            "111122223333#Requests#2026-01-31T11:05:07.123",
            "MalformedTimestamp",
        ),
        ("111122223333#Requests#2026-01-31T11", "MalformedTimestamp"),
        (
            "111122223333#Requests#2026-1-31T11:05:07",
            "MalformedTimestamp",
        ),
        (
            "111122223333#Requests#2026-01-31T11:05:60",
            "MalformedTimestamp",
        ),
        (
            "111122223333#Requests#2026-02-30T11:05:07",
            "MalformedTimestamp",
        ),
        (
            "111122223333#Requests#0000-01-31T11:05:07",
            "MalformedTimestamp",
        ),
        (
            "999999999999#Requests#2026-01-31T11:05:07",
            "SortKeyMismatch",
        ),
        ("111122223333#Other#2026-01-31T11:05:07", "SortKeyMismatch"),
    ];
    for (key, reason) in cases {
        assert_eq!(
            validate_sort_key(key, ACCOUNT, "Requests"),
            Some(reason),
            "{key}"
        );
    }
    // A dimension may hold '#'.
    let with_hash = format!("{ACCOUNT}#GB#Hours#2026-01-31T11:05:07");
    assert_eq!(validate_sort_key(&with_hash, ACCOUNT, "GB#Hours"), None);
}

#[test]
fn hour_buckets_run_from_now_minus_23_to_now_minus_1() {
    let buckets = completed_hour_buckets(at(NOW));
    assert_eq!(buckets.len(), 23);
    assert_eq!(buckets[0], "2026-01-30T13");
    assert_eq!(buckets[22], "2026-01-31T11");
    assert_eq!(hour_bucket_for_offset(at(NOW), 1), "2026-01-31T11");
    assert_eq!(hour_bucket_for_offset(at(NOW), 0), "2026-01-31T12");
    assert_eq!(hour_bucket_for_offset(at(NOW), 13), "2026-01-30T23");
}

#[test]
fn the_lock_is_month_aware_and_clamped() {
    let prev = "2026-01-31T23";
    let current = "2026-02-01T00";
    // From 03:00 UTC on the 1st until the 06:00 cutoff a previous-month bucket has lock 0.
    assert_eq!(effective_lock_hours(prev, at("2026-02-01T03:00:00Z"), 4), 0);
    assert_eq!(effective_lock_hours(prev, at("2026-02-01T05:59:59Z"), 4), 0);
    // Before 03:00 and at/after 06:00 the configured lock applies.
    assert_eq!(effective_lock_hours(prev, at("2026-02-01T02:59:59Z"), 4), 4);
    assert_eq!(effective_lock_hours(prev, at("2026-02-01T06:00:00Z"), 4), 4);
    // A current-month bucket never drops.
    assert_eq!(
        effective_lock_hours(current, at("2026-02-01T04:00:00Z"), 4),
        4
    );
    // The configured lock is clamped to 1..=20.
    assert_eq!(
        effective_lock_hours(current, at("2026-02-10T04:00:00Z"), 0),
        1
    );
    assert_eq!(
        effective_lock_hours(current, at("2026-02-10T04:00:00Z"), -5),
        1
    );
    assert_eq!(
        effective_lock_hours(current, at("2026-02-10T04:00:00Z"), 99),
        20
    );
    assert!(is_previous_month_bucket(prev, at("2026-02-01T00:10:00Z")));
    assert!(!is_previous_month_bucket(
        current,
        at("2026-02-01T00:10:00Z")
    ));
    assert!(is_previous_month_bucket(
        "2025-12-31T23",
        at("2026-01-01T00:10:00Z")
    ));
    assert!(previous_month_grace_open(at("2026-02-01T05:59:59Z")));
    assert!(!previous_month_grace_open(at("2026-02-01T06:00:00Z")));
    assert!(!previous_month_grace_open(at("2026-02-02T01:00:00Z")));
}

fn stamped(expression: &str) -> (String, String) {
    let (expression, values) = with_audit_timestamps(
        expression,
        vec![(":st".to_string(), Json::string("x"))],
        at(NOW),
    );
    (expression, values.to_compact_string())
}

#[test]
fn audit_stamps_go_inside_the_set_clause() {
    const STAMPS: &str = "createdAt = if_not_exists(createdAt, :now), updatedAt = :now";
    let (expr, values) = stamped("REMOVE meteringPending SET meteringStatus = :st");
    assert_eq!(
        expr,
        format!("REMOVE meteringPending SET meteringStatus = :st, {STAMPS}")
    );
    assert_eq!(values, r#"{":st":"x",":now":"2026-01-31T12:30:00Z"}"#);
    // A REMOVE-only expression gets a SET in front.
    assert_eq!(
        stamped("REMOVE meteringPending").0,
        format!("SET {STAMPS} REMOVE meteringPending")
    );
    // A SET followed by ADD gets the stamps spliced before the ADD clause.
    assert_eq!(
        stamped("SET a = :st ADD s :r").0,
        format!("SET a = :st, {STAMPS} ADD s :r")
    );
    assert_eq!(stamped("SET a = :st").0, format!("SET a = :st, {STAMPS}"));
    // A caller :now is replaced, never duplicated.
    let (_, values) = with_audit_timestamps(
        "SET a = :now",
        vec![(":now".to_string(), Json::string("old"))],
        at(NOW),
    );
    assert_eq!(
        values.to_compact_string(),
        r#"{":now":"2026-01-31T12:30:00Z"}"#
    );
}

#[test]
fn strict_int_accepts_only_exact_integers() {
    let some = [
        ("5", 5),
        ("-7", -7),
        (" 12 ", 12),
        ("5.0", 5),
        ("1e3", 1000),
        ("1_000", 1000),
        ("0.00", 0),
        ("+3", 3),
        ("120E-1", 12),
    ];
    for (text, value) in some {
        assert_eq!(strict_int(&Json::string(text)), Some(value), "{text}");
    }
    for text in [
        "5.5", "abc", "", "NaN", "Infinity", "1e", ".", "1__0", "_1", "0x10", "1.5e0",
    ] {
        assert_eq!(strict_int(&Json::string(text)), None, "{text}");
    }
    assert_eq!(strict_int(&Json::Int(9)), Some(9));
    assert_eq!(strict_int(&Json::Float(4.0)), Some(4));
    assert_eq!(strict_int(&Json::Float(4.5)), None);
    assert_eq!(strict_int(&Json::Bool(true)), None);
    assert_eq!(strict_int(&Json::Null), None);
    assert_eq!(strict_int(&j("[1]")), None);
}

#[test]
fn quantity_reasons_classify_why_a_value_failed() {
    assert_eq!(quantity_reason(&Json::string("5.5")), "NonIntegerQuantity");
    assert_eq!(quantity_reason(&Json::string("NaN")), "NonIntegerQuantity");
    assert_eq!(quantity_reason(&Json::string("abc")), "NonNumericQuantity");
    assert_eq!(quantity_reason(&Json::Bool(true)), "NonNumericQuantity");
    assert_eq!(quantity_reason(&Json::Null), "NonNumericQuantity");
    assert_eq!(quantity_reason(&Json::Float(2.5)), "NonIntegerQuantity");
    // Beyond i64 is out of range here (Python had unbounded ints).
    assert_eq!(
        quantity_reason(&Json::string("99999999999999999999")),
        "QuantityOutOfRange"
    );
    assert_eq!(strict_int(&Json::string("99999999999999999999")), None);
    assert_eq!(
        strict_int(&Json::string("9223372036854775807")),
        Some(i64::MAX)
    );
}

#[test]
fn mask_keeps_three_characters_at_each_end() {
    assert_eq!(mask("123456"), "***");
    assert_eq!(mask(""), "***");
    assert_eq!(mask("1234567"), "123***567");
    assert_eq!(mask(LICENSE), "arn***123");
}

fn row(sort_key: &str, quantity: &str, extra: &str) -> Json {
    j(&format!(
        r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId_dimension_timestamp": "{sort_key}",
            "meteringPending": "2026-01-31T11", "quantity": {quantity} {extra}}}"#
    ))
}

fn key(second: u32) -> String {
    format!("{ACCOUNT}#Requests#2026-01-31T11:00:{second:02}")
}

fn fold(rows: &[Json]) -> core::Group {
    fold_rows(LICENSE, ACCOUNT, "Requests", "2026-01-31T11", rows).expect("fold")
}

#[test]
fn rows_fold_into_one_group() {
    let rows = vec![
        row(&key(1), "3", ""),
        row(&key(2), r#""4""#, ""),
        j(&format!(
            r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId_dimension_timestamp": "{}", "quantity": 100}}"#,
            key(3)
        )),
    ];
    let group = fold(&rows);
    // The third row has no meteringPending and is skipped entirely.
    assert_eq!(group.quantity, 7);
    assert_eq!(group.row_keys, vec![key(1), key(2)]);
    assert_eq!(group.reject_reason, None);
    assert_eq!(group.untagged_nonzero_rows, 2);
    assert_eq!(validate_group(&group, at(NOW)), None);
}

#[test]
fn the_first_reason_wins_and_rejected_rows_keep_their_keys() {
    let rows = vec![
        row(&key(1), "2", ""),
        row(&key(2), "-1", ""),
        row(&key(3), r#""x""#, ""),
        row("bad", "1", ""),
    ];
    let group = fold(&rows);
    assert_eq!(group.reject_reason, Some("NegativeQuantity"));
    assert_eq!(group.quantity, 2);
    assert_eq!(group.row_keys.len(), 4);
    assert_eq!(validate_group(&group, at(NOW)), Some("NegativeQuantity"));
    let bad_first = fold(&[row(&key(1), "1.5", ""), row(&key(2), "-1", "")]);
    assert_eq!(bad_first.reject_reason, Some("NonIntegerQuantity"));
    let overflow = fold(&[
        row(&key(1), "9223372036854775807", ""),
        row(&key(2), "1", ""),
    ]);
    assert_eq!(overflow.reject_reason, Some("QuantityOutOfRange"));
}

fn alloc(quantity: i64, tags: &str) -> String {
    format!(r#"{{"AllocatedUsageQuantity": {quantity}, "Tags": {tags}}}"#)
}

#[test]
fn allocations_merge_by_tag_set_and_must_sum_to_the_quantity() {
    let tagged = |second: u32, quantity: i64, allocations: &str| {
        row(
            &key(second),
            &quantity.to_string(),
            &format!(r#", "usageAllocations": [{allocations}]"#),
        )
    };
    let a = r#"[{"Key": "team", "Value": "a"}, {"Key": "env", "Value": "p"}]"#;
    let a_reordered = r#"[{"Key": "env", "Value": "p"}, {"Key": "team", "Value": "a"}]"#;
    let b = r#"[{"Key": "team", "Value": "b"}]"#;
    let group = fold(&[
        tagged(1, 5, &format!("{}, {}", alloc(3, a), alloc(2, b))),
        tagged(2, 4, &alloc(4, a_reordered)),
    ]);
    assert_eq!(group.quantity, 9);
    assert_eq!(group.allocations.len(), 2);
    assert_eq!(group.allocations[0].qty, 7);
    assert_eq!(validate_group(&group, at(NOW)), None);
    let finalized = core::finalize_allocations(&group);
    assert_eq!(finalized.len(), 2);
    assert_eq!(
        finalized[0].to_compact_string(),
        r#"{"AllocatedUsageQuantity":7,"Tags":[{"Key":"team","Value":"a"},{"Key":"env","Value":"p"}]}"#
    );

    let mismatch = fold(&[tagged(1, 5, &alloc(3, b))]);
    assert_eq!(
        validate_group(&mismatch, at(NOW)),
        Some("AllocationSumMismatch")
    );
    let mixed = fold(&[tagged(1, 2, &alloc(2, b)), row(&key(2), "1", "")]);
    assert_eq!(
        validate_group(&mixed, at(NOW)),
        Some("MixedAllocatedAndUnallocated")
    );
    // Untagged rows only: no allocation list is sent.
    let plain = fold(&[row(&key(1), "2", "")]);
    assert!(core::finalize_allocations(&plain).is_empty());
}

#[test]
fn malformed_allocations_are_reason_coded() {
    let reason = |extra: &str| fold(&[row(&key(1), "2", extra)]).reject_reason;
    assert_eq!(
        reason(r#", "usageAllocations": "x""#),
        Some("MalformedAllocations")
    );
    assert_eq!(
        reason(r#", "usageAllocations": {"a": 1}"#),
        Some("MalformedAllocations")
    );
    assert_eq!(
        reason(r#", "usageAllocations": [5]"#),
        Some("MalformedAllocations")
    );
    assert_eq!(
        reason(&format!(
            r#", "usageAllocations": [{}]"#,
            alloc(-1, r#"[{"Key":"k","Value":"v"}]"#)
        )),
        Some("NegativeOrNonIntegerAllocation")
    );
    assert_eq!(
        reason(r#", "usageAllocations": [{"Tags": [{"Key":"k","Value":"v"}]}]"#),
        Some("NegativeOrNonIntegerAllocation")
    );
    assert_eq!(
        reason(&format!(r#", "usageAllocations": [{}]"#, alloc(2, "[]"))),
        Some("MissingTagKeyOrValue")
    );
    assert_eq!(
        reason(&format!(r#", "usageAllocations": [{}]"#, alloc(2, "null"))),
        Some("MissingTagKeyOrValue")
    );
    assert_eq!(
        reason(&format!(
            r#", "usageAllocations": [{}]"#,
            alloc(2, r#"[{"Key":"k"}]"#)
        )),
        Some("MissingTagKeyOrValue")
    );
    assert_eq!(
        reason(&format!(
            r#", "usageAllocations": [{}]"#,
            alloc(2, r#"[{"Key":"","Value":"v"}]"#)
        )),
        Some("MissingTagKeyOrValue")
    );
    let six = (0..6)
        .map(|n| format!(r#"{{"Key":"k{n}","Value":"v"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        reason(&format!(
            r#", "usageAllocations": [{}]"#,
            alloc(2, &format!("[{six}]"))
        )),
        Some("TooManyTags")
    );
}

#[test]
fn groups_are_validated_in_the_scripts_order() {
    let group = |license: &str, account: &str, dimension: &str, bucket: &str| {
        let mut group = fold_rows(license, account, dimension, bucket, &[]).expect("fold");
        group.quantity = 1;
        group
    };
    let now = at(NOW);
    let valid = group(LICENSE, ACCOUNT, "Requests", "2026-01-31T11");
    assert_eq!(validate_group(&valid, now), None);
    let cases = [
        (
            group(LICENSE, ACCOUNT, "", "2026-01-31T11"),
            "MissingDimension",
        ),
        (
            group(LICENSE, ACCOUNT, "R", "2026-01-31T13"),
            "TimestampInFuture",
        ),
        (
            group(LICENSE, ACCOUNT, "R", "2026-01-30T12"),
            "TimestampOutOfWindow",
        ),
        (
            group("arn:aws:other", ACCOUNT, "R", "2026-01-31T11"),
            "InvalidLicenseArn",
        ),
        (
            group(LICENSE, "", "R", "2026-01-31T11"),
            "MissingCustomerAWSAccountId",
        ),
        (
            group("legacy-id", "", "R", "2026-01-31T11"),
            "MissingLegacyIdentifier",
        ),
    ];
    for (group, reason) in cases {
        assert_eq!(validate_group(&group, now), Some(reason));
    }
    // 24 hours back is still inside the window; the legacy identifier may be the account.
    assert_eq!(
        validate_group(&group(LICENSE, ACCOUNT, "R", "2026-01-30T13"), now),
        None
    );
    assert_eq!(
        validate_group(&group("legacy-id", ACCOUNT, "R", "2026-01-31T11"), now),
        None
    );
}

#[test]
fn license_arns_follow_the_script_regex() {
    let good = [
        LICENSE,
        "arn:aws-cn:license-manager::111122223333:license:l-1",
        "arn:aws:license-manager:eu-west-1:111122223333:license:a:b\n",
    ];
    for arn in good {
        let mut group = fold_rows(arn, ACCOUNT, "R", "2026-01-31T11", &[]).expect("fold");
        group.quantity = 1;
        assert_eq!(validate_group(&group, at(NOW)), None, "{arn}");
    }
    let bad = [
        "arn:aws:license-manager:us-east-1:11112222333:license:l-1",
        "arn:aws:license-manager:us-east-1:1111222233334:license:l-1",
        "arn:aws:license-manager:us-east-1:111122223333:license:",
        "arn:aws:license-manager:us-east-1:111122223333:license:\n",
        "arn:aws:license-manager:us-east-1:111122223333:license:a\nb",
        "arn:aws:license-manager:us-east-1:11112222333a:license:l-1",
        "arn:aws:licensemanager:us-east-1:111122223333:license:l-1",
        "arn:aws:license-manager:us:east:111122223333:license:l-1",
    ];
    for arn in bad {
        let mut group = fold_rows(arn, ACCOUNT, "R", "2026-01-31T11", &[]).expect("fold");
        group.quantity = 1;
        assert_eq!(
            validate_group(&group, at(NOW)),
            Some("InvalidLicenseArn"),
            "{arn:?}"
        );
    }
}

fn emf(name: &str, reason: Option<&str>, exception: Option<&str>) -> String {
    let mut log = Logger::default();
    let mut client = RecordedClient::from_document(&j(r#"{"calls": []}"#)).expect("recording");
    let config = Config::from_pairs(&[("PRODUCT_CODE", "prod123"), ("METERING_MODE", " Dry-Run ")]);
    let mut ctx = Ctx {
        client: &mut client,
        config: &config,
        log: &mut log,
        now: at(NOW),
    };
    emf_metric(&mut ctx, name, 2, reason, exception);
    log.lines()[0].clone()
}

#[test]
fn emf_lines_pick_their_dimension_set() {
    assert_eq!(
        emf("DuplicateRecord", None, None),
        r#"{"_aws": {"Timestamp": 1769862600000, "CloudWatchMetrics": [{"Namespace": "AwsMarketplace/Metering", "Dimensions": [["ProductCode"]], "Metrics": [{"Name": "DuplicateRecord", "Unit": "Count"}]}]}, "ProductCode": "prod123", "DuplicateRecord": 2}"#
    );
    assert_eq!(
        emf("UsageRecordRejected", Some("NegativeQuantity"), None),
        r#"{"_aws": {"Timestamp": 1769862600000, "CloudWatchMetrics": [{"Namespace": "AwsMarketplace/Metering", "Dimensions": [["Reason", "MeteringMode", "ProductCode"]], "Metrics": [{"Name": "UsageRecordRejected", "Unit": "Count"}]}]}, "Reason": "NegativeQuantity", "ProductCode": "prod123", "MeteringMode": "dry-run", "UsageRecordRejected": 2}"#
    );
    let exception = emf(
        "BatchMeterUsageException",
        None,
        Some("InvalidTagException"),
    );
    assert!(exception.contains(r#""Dimensions": [["Exception", "MeteringMode", "ProductCode"]]"#));
    assert!(exception.contains(r#""Exception": "InvalidTagException""#));
    // Without the reason or the exception the counter falls back to the bare set.
    assert!(emf("UsageRecordRejected", None, None).contains(r#""Dimensions": [["ProductCode"]]"#));
}

// ---- submit ----------------------------------------------------------------

fn usage_record(n: usize) -> Json {
    j(&format!(
        r#"{{"Timestamp": 1769857200, "Dimension": "D{n}", "Quantity": {n}, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}"#
    ))
}

fn agg_key(n: usize) -> AggKey {
    (LICENSE.to_string(), format!("{ACCOUNT}#D{n}#2026-01-31T11"))
}

fn result_for(n: usize, status: &str) -> String {
    format!(
        r#"{{"MeteringRecordId": "mrec-{n}", "Status": "{status}", "UsageRecord": {{"Timestamp": 1769857200, "Dimension": "D{n}", "Quantity": {n}, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}}}"#
    )
}

struct Submitted {
    pending: Result<bool, String>,
    outcomes: Outcomes,
    log: Vec<String>,
    sent: Vec<(String, Json)>,
    unused: usize,
}

fn submit(count: usize, calls: &[String], product: bool) -> Submitted {
    let document = j(&format!(r#"{{"calls": [{}]}}"#, calls.join(",")));
    let mut client = RecordedClient::from_document(&document).expect("recording");
    let config = Config::from_pairs(&[("PRODUCT_CODE", "prod123")]);
    let mut log = Logger::default();
    let mut outcomes = Outcomes::default();
    let records: Vec<Json> = (0..count).map(usage_record).collect();
    let keys: Vec<AggKey> = (0..count).map(agg_key).collect();
    let pending = {
        let mut ctx = Ctx {
            client: &mut client,
            config: &config,
            log: &mut log,
            now: at(NOW),
        };
        core::submit_records(&mut ctx, &records, &keys, &mut outcomes, product)
            .map_err(|f| f.to_string())
    };
    Submitted {
        pending,
        outcomes,
        log: log.lines().to_vec(),
        sent: client.sent().to_vec(),
        unused: client.unconsumed(),
    }
}

fn metering(response: &str) -> String {
    step("meteringmarketplace", "BatchMeterUsage", None, response)
}

fn metering_error(code: &str) -> String {
    fail("meteringmarketplace", "BatchMeterUsage", code)
}

#[test]
fn a_clean_batch_records_each_status() {
    let response = format!(
        r#"{{"Results": [{}, {}, {}]}}"#,
        result_for(0, "Success"),
        result_for(1, "DuplicateRecord"),
        result_for(2, "CustomerNotSubscribed")
    );
    let run = submit(3, &[metering(&response)], false);
    assert_eq!(run.pending, Ok(false));
    let statuses: Vec<(&str, Option<&str>)> = run
        .outcomes
        .entries()
        .iter()
        .map(|(_, o)| (o.status.as_str(), o.metering_record_id.as_deref()))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("Success", Some("mrec-0")),
            ("DuplicateRecord", Some("mrec-1")),
            ("CustomerNotSubscribed", Some("mrec-2"))
        ]
    );
    // The legacy path sends ProductCode, the Concurrent Agreements path does not.
    assert!(run.sent[0].1.get("ProductCode").is_none());
    assert!(run
        .log
        .iter()
        .any(|l| l.contains(r#""DuplicateRecord": 1"#)));
    assert!(run
        .log
        .iter()
        .any(|l| l.contains(r#""CustomerNotSubscribed": 1"#)));
    assert!(run
        .log
        .iter()
        .any(|l| l.contains("Metering status=CustomerNotSubscribed account=111***333")));
    let legacy = submit(
        1,
        &[metering(&format!(
            r#"{{"Results": [{}]}}"#,
            result_for(0, "Success")
        ))],
        true,
    );
    assert_eq!(
        legacy.sent[0].1.get("ProductCode").and_then(Json::as_str),
        Some("prod123")
    );
}

#[test]
fn unprocessed_records_are_retried_once() {
    let unprocessed = format!(
        r#"{{"Results": [{}], "UnprocessedRecords": [{}]}}"#,
        result_for(0, "Success"),
        usage_record(1).to_compact_string()
    );
    let second = format!(r#"{{"Results": [{}]}}"#, result_for(1, "Success"));
    let run = submit(2, &[metering(&unprocessed), metering(&second)], false);
    assert_eq!(run.pending, Ok(false));
    assert_eq!(run.outcomes.entries().len(), 2);
    assert_eq!(run.sent.len(), 2);
    // The retry carries only the unprocessed record.
    assert_eq!(
        run.sent[1]
            .1
            .get("UsageRecords")
            .map(Json::to_compact_string),
        Some(format!("[{}]", usage_record(1).to_compact_string()))
    );
    assert!(run
        .log
        .iter()
        .any(|l| l.contains("1 unprocessed record(s); retrying once")));

    // Still unprocessed after the retry: left pending for the next run.
    let still = format!(
        r#"{{"UnprocessedRecords": [{}]}}"#,
        usage_record(1).to_compact_string()
    );
    let run = submit(1, &[metering(&still), metering(&still)], false);
    assert_eq!(run.pending, Ok(true));
    assert!(run.outcomes.entries().is_empty());
    assert!(run
        .log
        .iter()
        .any(|l| l.contains(r#""UsageRecordUnprocessed": 1"#)));
    assert_eq!(run.unused, 0);
}

#[test]
fn transient_codes_propagate_and_stamp_nothing() {
    for code in [
        "ThrottlingException",
        "InternalServiceException",
        "ServiceUnavailable",
        "DisabledApiException",
    ] {
        let run = submit(2, &[metering_error(code)], false);
        let error = run.pending.expect_err("a transient error is raised");
        assert!(error.contains(code), "{error}");
        assert!(run.outcomes.entries().is_empty());
        assert_eq!(run.sent.len(), 1, "not bisected");
    }
    // A response without recorded steps is a connect failure: raised, never stamped.
    assert!(submit(1, &[], false).pending.is_err());
}

#[test]
fn a_poison_record_is_isolated_by_bisecting() {
    // Four records: the whole batch and the left half fail, then record 0 alone fails.
    let calls = [
        metering_error("InvalidUsageDimensionException"),
        metering_error("InvalidUsageDimensionException"),
        metering_error("InvalidUsageDimensionException"),
        metering(&format!(r#"{{"Results": [{}]}}"#, result_for(1, "Success"))),
        metering(&format!(
            r#"{{"Results": [{}, {}]}}"#,
            result_for(2, "Success"),
            result_for(3, "Success")
        )),
    ];
    let run = submit(4, &calls, false);
    assert_eq!(run.pending, Ok(false));
    let entries = run.outcomes.entries();
    assert_eq!(entries.len(), 4);
    let poison = entries
        .iter()
        .find(|(k, _)| *k == agg_key(0))
        .expect("poison");
    assert_eq!(poison.1.status, "Error");
    assert_eq!(
        poison.1.reason.as_deref(),
        Some("InvalidUsageDimensionException")
    );
    assert_eq!(poison.1.metering_record_id, None);
    assert!(
        entries
            .iter()
            .filter(|(_, o)| o.status == "Success")
            .count()
            == 3
    );
    assert!(run
        .log
        .iter()
        .any(|l| l.contains(r#""Exception": "InvalidUsageDimensionException""#)));
    assert_eq!(run.unused, 0);
}

#[test]
fn bisecting_a_full_failing_batch_reaches_every_record() {
    // 25 records that all fail: a full binary tree of 49 calls ends with 25 isolated records.
    let calls: Vec<String> = (0..49)
        .map(|_| metering_error("InvalidLicenseException"))
        .collect();
    let run = submit(25, &calls, false);
    assert_eq!(run.pending, Ok(false));
    assert_eq!(run.outcomes.entries().len(), 25);
    assert!(run
        .outcomes
        .entries()
        .iter()
        .all(|(_, o)| o.status == "Error"));
    assert_eq!(run.sent.len(), 49);
    assert_eq!(core::MAX_BISECT_DEPTH, 5);
    assert_eq!(1_u32 << core::MAX_BISECT_DEPTH, 32);
}

#[test]
fn an_empty_batch_sends_nothing() {
    let run = submit(0, &[], false);
    assert_eq!(run.pending, Ok(false));
    assert!(run.sent.is_empty());
}

// ---- bounded row writes --------------------------------------------------------

fn row_updates(
    calls: &[String],
    config: &[(&str, &str)],
    keys: &[String],
) -> (Result<(), String>, usize, usize) {
    let document = j(&format!(r#"{{"calls": [{}]}}"#, calls.join(",")));
    let mut client = RecordedClient::from_document(&document).expect("recording");
    let config = Config::from_pairs(config);
    let mut log = Logger::default();
    let result = {
        let mut ctx = Ctx {
            client: &mut client,
            config: &config,
            log: &mut log,
            now: at(NOW),
        };
        core::update_rows_bounded(&mut ctx, LICENSE, keys, "REMOVE meteringPending", &[])
            .map_err(|f| f.to_string())
    };
    (result, client.sent().len(), client.unconsumed())
}

fn throttle() -> String {
    fail(
        "dynamodb",
        "UpdateItem",
        "ProvisionedThroughputExceededException",
    )
}

#[test]
fn throttled_writes_retry_up_to_the_limit() {
    let keys = vec![key(1), String::new(), key(2)];
    let config = [("USAGE_TABLE", "usage"), ("CLEANUP_MAX_RETRIES", "2")];
    // key 1 throttled twice then fine, the empty key is skipped, key 2 fine.
    let ok = [throttle(), throttle(), update(), update()];
    let (result, sent, unused) = row_updates(&ok, &config, &keys);
    assert_eq!(result, Ok(()));
    assert_eq!((sent, unused), (4, 0));
    // Three throttles exceed two retries: that row fails after three calls.
    let calls = [throttle(), throttle(), throttle(), update()];
    let (result, sent, _) = row_updates(&calls, &config, &keys);
    let error = result.expect_err("the row fails");
    assert!(
        error.starts_with("1/3 UpdateItem(s) failed for license=arn***123; first error: "),
        "{error}"
    );
    assert!(error.contains("ProvisionedThroughputExceededException"));
    assert_eq!(sent, 4, "the other row is still tried");
}

#[test]
fn other_errors_fail_a_row_without_retry_and_never_leak_the_message() {
    let calls = [
        fail("dynamodb", "UpdateItem", "ValidationException"),
        update(),
    ];
    let (result, sent, _) = row_updates(&calls, &[("USAGE_TABLE", "usage")], &[key(1), key(2)]);
    let error = result.expect_err("one row fails");
    assert!(error.starts_with("1/2 UpdateItem(s) failed"));
    assert!(!error.contains(SECRET));
    assert_eq!(sent, 2);
}

#[test]
fn row_updates_carry_the_audit_stamps() {
    let document = j(&format!(r#"{{"calls": [{}]}}"#, update()));
    let mut client = RecordedClient::from_document(&document).expect("recording");
    let config = Config::from_pairs(&[("USAGE_TABLE", "usage")]);
    let mut log = Logger::default();
    let mut ctx = Ctx {
        client: &mut client,
        config: &config,
        log: &mut log,
        now: at(NOW),
    };
    core::update_rows_bounded(
        &mut ctx,
        LICENSE,
        &[key(1)],
        "REMOVE meteringPending SET meteringStatus = :st",
        &[(":st".to_string(), Json::string("Aggregated"))],
    )
    .expect("update");
    let request = client.sent()[0].1.clone();
    assert_eq!(
        request.to_compact_string(),
        format!(
            r#"{{"TableName":"usage","Key":{{"licenseArn":"{LICENSE}","customerAWSAccountId_dimension_timestamp":"{}"}},"UpdateExpression":"REMOVE meteringPending SET meteringStatus = :st, createdAt = if_not_exists(createdAt, :now), updatedAt = :now","ExpressionAttributeValues":{{":st":"Aggregated",":now":"2026-01-31T12:30:00Z"}}}}"#,
            key(1)
        )
    );
}
