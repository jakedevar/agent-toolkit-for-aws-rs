//! The submitter Lambda: reads, batching, dry-run, outcomes and write-back.

use super::*;

const DEP_LICENSE: &str = "arn:aws:license-manager:us-east-1:111122223333:license:l-dep";

fn agg_item(license: &str, dimension: &str, bucket: &str, quantity: i64) -> String {
    format!(
        r#"{{"licenseArn": "{license}", "account_dimension_hour": "{ACCOUNT}#{dimension}#{bucket}", "customerAWSAccountId": "{ACCOUNT}", "dimension": "{dimension}", "hourBucket": "{bucket}", "quantity": {quantity}, "meteringPending": "{bucket}"}}"#
    )
}

fn items(list: &[String]) -> String {
    format!(r#"{{"Items": [{}]}}"#, list.join(","))
}

/// The 23 queries of the ordinary pass (oldest first); `at` maps a position
/// (0 is now-23h, 22 is now-1h) to its items.
fn ordinary_pass(at: &[(usize, Vec<String>)]) -> Vec<String> {
    (0..23)
        .map(|position| {
            let found = at.iter().find(|(p, _)| *p == position);
            query(&found.map_or_else(|| r#"{"Items": []}"#.to_string(), |(_, list)| items(list)))
        })
        .collect()
}

fn empty_subscribers() -> String {
    query(r#"{"Items": []}"#)
}

fn metering(response: &str) -> String {
    step("meteringmarketplace", "BatchMeterUsage", None, response)
}

fn config_with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut config: Vec<(&str, &str)> = CFG.to_vec();
    config.extend_from_slice(extra);
    config
}

fn result_record(dimension: &str, status: &str, id: &str) -> String {
    format!(
        r#"{{"MeteringRecordId": "{id}", "Status": "{status}", "UsageRecord": {{"Timestamp": 1769857200, "Dimension": "{dimension}", "Quantity": 5, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}}}"#
    )
}

#[test]
fn dry_run_never_calls_batch_meter_usage() {
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(
        22,
        vec![agg_item(LICENSE, "Requests", "2026-01-31T11", 5)],
    )]));
    calls.push(update());
    let config = config_with(&[("METERING_MODE", "dry-run")]);
    let ran = run_with("submitter", &config, "{}", &calls, NOW);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":1}"#);
    assert!(ran
        .requests("meteringmarketplace:BatchMeterUsage")
        .is_empty());
    let write = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        write.get("UpdateExpression").and_then(Json::as_str),
        Some("REMOVE meteringPending SET meteringStatus = :st, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        write
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{":st":"DryRunSubmitted",":now":"2026-01-31T12:30:00Z"}"#)
    );
    assert!(ran.logged("DRY-RUN (MeteringMode=dry-run) - would submit UsageRecord: license=arn***123 account=111***333 dimension=Requests quantity=5 (use_product_code=False); NOT calling BatchMeterUsage"));
    assert!(ran.log.iter().any(|l| l.contains(r#""DryRunSubmitted": 1"#)
        && l.contains(r#""Dimensions": [["ProductCode"]]"#)));
    assert_eq!(ran.unused, 0);
}

#[test]
fn a_live_run_submits_concurrent_agreement_and_legacy_records_separately() {
    let legacy = r#"{"licenseArn": "legacy-lic", "account_dimension_hour": "acct#L#2026-01-31T11", "customerIdentifier": "cust-1", "dimension": "L", "hourBucket": "2026-01-31T11", "quantity": "1", "meteringPending": "2026-01-31T11"}"#;
    let tagged = format!(
        r#"{{"licenseArn": "{LICENSE}", "account_dimension_hour": "{ACCOUNT}#T#2026-01-31T11", "customerAWSAccountId": "{ACCOUNT}", "dimension": "T", "hourBucket": "2026-01-31T11", "quantity": 5, "usageAllocations": [{{"AllocatedUsageQuantity": "5", "Tags": [{{"Key": "team", "Value": "a"}}]}}], "meteringPending": "2026-01-31T11"}}"#
    );
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(
        22,
        vec![
            agg_item(LICENSE, "Requests", "2026-01-31T11", 5),
            tagged,
            legacy.to_string(),
        ],
    )]));
    calls.push(metering(&format!(
        r#"{{"Results": [{}, {}]}}"#,
        result_record("Requests", "Success", "mrec-1"),
        result_record("T", "CustomerNotSubscribed", "mrec-2")
    )));
    calls.push(metering(
        r#"{"Results": [{"MeteringRecordId": "mrec-3", "Status": "Success", "UsageRecord": {"Timestamp": 1769857200, "Dimension": "L", "Quantity": 1, "CustomerIdentifier": "cust-1"}}]}"#,
    ));
    calls.extend([update(), update(), update()]);
    let config = config_with(&[("AGGREGATED_USAGE_TTL_DAYS", "2")]);
    let ran = run_with("submitter", &config, "{}", &calls, NOW);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":3}"#);

    let batches = ran.requests("meteringmarketplace:BatchMeterUsage");
    assert_eq!(batches.len(), 2);
    assert_eq!(
        batches[0].to_compact_string(),
        format!(
            r#"{{"UsageRecords":[{{"Timestamp":1769857200,"Dimension":"Requests","Quantity":5,"CustomerAWSAccountId":"{ACCOUNT}","LicenseArn":"{LICENSE}"}},{{"Timestamp":1769857200,"Dimension":"T","Quantity":5,"UsageAllocations":[{{"AllocatedUsageQuantity":5,"Tags":[{{"Key":"team","Value":"a"}}]}}],"CustomerAWSAccountId":"{ACCOUNT}","LicenseArn":"{LICENSE}"}}]}}"#
        )
    );
    assert_eq!(
        batches[1].to_compact_string(),
        r#"{"UsageRecords":[{"Timestamp":1769857200,"Dimension":"L","Quantity":1,"CustomerIdentifier":"cust-1"}],"ProductCode":"prod123"}"#
    );

    let writes = ran.requests("dynamodb:UpdateItem");
    assert_eq!(writes.len(), 3);
    // Success: the id and a ttl of now + 2 days.
    assert_eq!(
        writes[0].get("UpdateExpression").and_then(Json::as_str),
        Some("REMOVE meteringPending SET meteringStatus = :st, meteringRecordId = :mid, #ttl = :ttl, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        writes[0]
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(
            r#"{":st":"Success",":mid":"mrec-1",":ttl":1770035400,":now":"2026-01-31T12:30:00Z"}"#
        )
    );
    assert_eq!(
        writes[0]
            .get("ExpressionAttributeNames")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r##"{"#ttl":"ttl"}"##)
    );
    assert_eq!(
        writes[0].get("Key").map(Json::to_compact_string),
        Some(format!(
            r#"{{"licenseArn":"{LICENSE}","account_dimension_hour":"{ACCOUNT}#Requests#2026-01-31T11"}}"#
        ))
    );
    // A non-success status never gets a ttl.
    assert!(writes[1].get("ExpressionAttributeNames").is_none());
    assert!(writes[1]
        .get("ExpressionAttributeValues")
        .map(Json::to_compact_string)
        .is_some_and(|v| v.contains("CustomerNotSubscribed") && !v.contains("ttl")));
    assert!(ran.logged("Submitted 3 aggregated record(s) across 2 license(s)"));
    assert_eq!(ran.unused, 0);
}

#[test]
fn deprovisioning_licenses_go_first_and_batches_hold_at_most_25() {
    let ordinary: Vec<String> = (0..25)
        .map(|n| agg_item(LICENSE, &format!("D{n}"), "2026-01-31T11", 1))
        .collect();
    let subscribers = query(&format!(
        r#"{{"Items": [{{"licenseArn": "{DEP_LICENSE}", "deprovisioningExpiry": "2026-01-31T13:00:00Z"}}]}}"#
    ));
    let mut calls = vec![subscribers];
    // Deprovisioning pass: now-23h .. now (24 queries), the record sits at now-1h.
    for hours_back in (0..=23).rev() {
        calls.push(query(&if hours_back == 1 {
            items(&[agg_item(DEP_LICENSE, "Dep", "2026-01-31T11", 1)])
        } else {
            r#"{"Items": []}"#.to_string()
        }));
    }
    calls.extend(ordinary_pass(&[(22, ordinary)]));
    calls.extend([metering("{}"), metering("{}")]);
    let ran = run("submitter", "{}", &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":0}"#);
    let batches = ran.requests("meteringmarketplace:BatchMeterUsage");
    let records = |n: usize| {
        batches[n]
            .get("UsageRecords")
            .map(Json::to_compact_string)
            .unwrap_or_default()
    };
    assert_eq!(records(0).matches(r#""Timestamp""#).count(), 25);
    assert_eq!(records(1).matches(r#""Timestamp""#).count(), 1);
    assert!(records(0).starts_with(&format!(r#"[{{"Timestamp":1769857200,"Dimension":"Dep","Quantity":1,"CustomerAWSAccountId":"{ACCOUNT}","LicenseArn":"{DEP_LICENSE}"}}"#)));
    // The deprovisioning pass reads per license, the current hour included.
    let queries = ran.requests("dynamodb:Query");
    assert_eq!(
        queries[1]
            .get("KeyConditionExpression")
            .and_then(Json::as_str),
        Some("meteringPending = :hb AND licenseArn = :la")
    );
    assert_eq!(queries.len(), 1 + 24 + 23);
    assert_eq!(ran.unused, 0);
}

#[test]
fn the_per_run_cap_stops_reading() {
    let three = vec![
        agg_item(LICENSE, "A", "2026-01-30T13", 1),
        agg_item(LICENSE, "B", "2026-01-30T13", 1),
        agg_item(LICENSE, "C", "2026-01-30T13", 1),
    ];
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(0, three)]));
    calls.push(metering("{}"));
    let config = config_with(&[("MAX_RECORDS_PER_RUN", "2")]);
    let ran = run_with("submitter", &config, "{}", &calls, NOW);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":0}"#);
    assert_eq!(
        ran.requests("dynamodb:Query").len(),
        2,
        "no bucket is read after the cap"
    );
    let batch = ran.requests("meteringmarketplace:BatchMeterUsage")[0];
    assert_eq!(
        batch
            .get("UsageRecords")
            .map(Json::to_compact_string)
            .unwrap_or_default()
            .matches(r#""Timestamp""#)
            .count(),
        2
    );
}

#[test]
fn a_failed_deprovisioning_lookup_does_not_block_ordinary_metering() {
    let mut calls = vec![fail("dynamodb", "Query", "AccessDeniedException")];
    calls.extend(ordinary_pass(&[]));
    let ran = run("submitter", "{}", &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":0}"#);
    assert!(ran.logged(
        "deprovisioning_licenses() lookup failed (aws_service_error (AccessDeniedException))"
    ));
    assert!(ran.logged("No pending aggregated usage to submit"));
}

#[test]
fn a_poison_record_is_stamped_error_with_its_exception() {
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(
        22,
        vec![agg_item(LICENSE, "Bad", "2026-01-31T11", 3)],
    )]));
    calls.push(fail(
        "meteringmarketplace",
        "BatchMeterUsage",
        "InvalidUsageDimensionException",
    ));
    calls.push(update());
    let ran = run("submitter", "{}", &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":1}"#);
    let write = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        write.get("UpdateExpression").and_then(Json::as_str),
        Some("REMOVE meteringPending SET meteringStatus = :st, meteringStatusReason = :rsn, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        write
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(
            r#"{":st":"Error",":rsn":"InvalidUsageDimensionException",":now":"2026-01-31T12:30:00Z"}"#
        )
    );
}

#[test]
fn a_transient_error_fails_the_run_and_keeps_the_pending_markers() {
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(
        22,
        vec![agg_item(LICENSE, "Requests", "2026-01-31T11", 3)],
    )]));
    calls.push(fail(
        "meteringmarketplace",
        "BatchMeterUsage",
        "ThrottlingException",
    ));
    let ran = run("submitter", "{}", &calls);
    assert_eq!(ran.err(), "aws_service_error (ThrottlingException)");
    assert!(ran.requests("dynamodb:UpdateItem").is_empty());
}

#[test]
fn unprocessed_records_fail_the_run_after_the_write_back() {
    let unprocessed = format!(
        r#"{{"Results": [{}], "UnprocessedRecords": [{{"Timestamp": 1769857200, "Dimension": "B", "Quantity": 1, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}]}}"#,
        result_record("A", "Success", "mrec-a")
    );
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(
        22,
        vec![
            agg_item(LICENSE, "A", "2026-01-31T11", 5),
            agg_item(LICENSE, "B", "2026-01-31T11", 1),
        ],
    )]));
    calls.extend([metering(&unprocessed), metering(&unprocessed), update()]);
    let ran = run("submitter", "{}", &calls);
    assert!(ran
        .err()
        .starts_with("One or more UsageRecords remained unprocessed after retry"));
    // The record that did succeed was written back before the run failed.
    assert_eq!(ran.requests("dynamodb:UpdateItem").len(), 1);
}

#[test]
fn the_submitter_needs_the_aggregated_table() {
    let cfg: Vec<(&str, &str)> = CFG
        .iter()
        .copied()
        .filter(|(k, _)| *k != "AGGREGATED_USAGE_TABLE")
        .collect();
    let ran = run_with("submitter", &cfg, "{}", &[], NOW);
    assert_eq!(ran.err(), "AGGREGATED_USAGE_TABLE is not configured");
}

// ---- exact integers beyond the quantity domain fail closed (#4973) ------------

fn item_with(quantity: &str, allocations: &str) -> String {
    let allocations = if allocations.is_empty() {
        String::new()
    } else {
        format!(r#", "usageAllocations": [{allocations}]"#)
    };
    format!(
        r#"{{"licenseArn": "{LICENSE}", "account_dimension_hour": "{ACCOUNT}#Requests#2026-01-31T11", "customerAWSAccountId": "{ACCOUNT}", "dimension": "Requests", "hourBucket": "2026-01-31T11", "quantity": {quantity}{allocations}, "meteringPending": "2026-01-31T11"}}"#
    )
}

/// The reads of a run over `pending` items: nothing else is recorded, so any
/// `BatchMeterUsage` or write the run attempted would be refused and counted.
fn reads_only(pending: Vec<String>) -> Vec<String> {
    let mut calls = vec![empty_subscribers()];
    calls.extend(ordinary_pass(&[(22, pending)]));
    calls
}

fn assert_failed_closed(ran: &Ran) {
    let error = ran.err();
    assert!(error.contains("QuantityOutOfRange"), "{error}");
    assert!(
        ran.requests("meteringmarketplace:BatchMeterUsage")
            .is_empty(),
        "nothing is submitted, not even a substituted zero"
    );
    assert!(
        ran.requests("dynamodb:UpdateItem").is_empty(),
        "nothing is finalized, the pending marker stays"
    );
    assert_eq!(ran.unused, 0);
}

#[test]
fn a_stored_quantity_beyond_i64_fails_the_run_instead_of_becoming_zero() {
    for quantity in [
        r#""9223372036854775808""#,
        "9223372036854775808",
        "1e19",
        r#""-9223372036854775809""#,
        r#""18446744073709551616""#,
    ] {
        let calls = reads_only(vec![item_with(quantity, "")]);
        assert_failed_closed(&run("submitter", "{}", &calls));
        let dry = config_with(&[("METERING_MODE", "dry-run")]);
        assert_failed_closed(&run_with("submitter", &dry, "{}", &calls, NOW));
    }
}

#[test]
fn one_out_of_range_record_stops_the_valid_records_beside_it_too() {
    let calls = reads_only(vec![
        agg_item(LICENSE, "Fine", "2026-01-31T11", 5),
        item_with(r#""9223372036854775808""#, ""),
    ]);
    assert_failed_closed(&run("submitter", "{}", &calls));
}

#[test]
fn a_stored_allocation_beyond_i64_fails_the_run_instead_of_becoming_zero() {
    for allocated in [
        r#""9223372036854775808""#,
        "9223372036854775808",
        "1e19",
        r#""-9223372036854775809""#,
    ] {
        let allocations = format!(
            r#"{{"AllocatedUsageQuantity": {allocated}, "Tags": [{{"Key": "team", "Value": "a"}}]}}"#
        );
        let calls = reads_only(vec![item_with("5", &allocations)]);
        assert_failed_closed(&run("submitter", "{}", &calls));
    }
}

#[test]
fn the_largest_quantity_and_a_legitimate_zero_are_still_submitted() {
    let max = SAFE_MAX;
    let allocations = r#"{"AllocatedUsageQuantity": "0", "Tags": [{"Key": "team", "Value": "a"}]}, {"AllocatedUsageQuantity": 9007199254740991, "Tags": [{"Key": "team", "Value": "b"}]}"#;
    let mut calls = reads_only(vec![
        item_with(&max.to_string(), allocations),
        agg_item(LICENSE, "Zero", "2026-01-31T11", 0),
    ]);
    calls.push(metering(&format!(
        r#"{{"Results": [{{"MeteringRecordId": "mrec-max", "Status": "Success", "UsageRecord": {{"Timestamp": 1769857200, "Dimension": "Requests", "Quantity": {max}, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}}}, {{"MeteringRecordId": "mrec-zero", "Status": "Success", "UsageRecord": {{"Timestamp": 1769857200, "Dimension": "Zero", "Quantity": 0, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}}}]}}"#
    )));
    calls.extend([update(), update()]);
    let ran = run("submitter", "{}", &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":2}"#);
    let batch = ran.requests("meteringmarketplace:BatchMeterUsage")[0].to_compact_string();
    assert!(
        batch.contains(&format!(r#""Dimension":"Requests","Quantity":{max},"UsageAllocations":[{{"AllocatedUsageQuantity":0,"Tags":[{{"Key":"team","Value":"a"}}]}},{{"AllocatedUsageQuantity":{max},"Tags":[{{"Key":"team","Value":"b"}}]}}]"#)),
        "{batch}"
    );
    assert!(
        batch.contains(r#""Dimension":"Zero","Quantity":0"#),
        "{batch}"
    );
    assert_eq!(ran.requests("dynamodb:UpdateItem").len(), 2);
    assert_eq!(ran.unused, 0);
}

// ---- an integer literal beyond i64 is judged by its text (#4984) --------------------
//
// The JSON reader turns an integer literal it cannot hold in 64 bits into a
// float, and the float nearest `-9223372036854775809` is `-9223372036854775808`
// exactly. A range check on the parsed value therefore accepts the literal as
// `i64::MIN`; the range has to be judged on the literal itself.

/// Integer literals just outside, and well outside, the 64-bit range.
const WIDE_LITERALS: [&str; 6] = [
    "-9223372036854775809",
    "-9223372036854775810",
    "-9223372036854776832",
    "-99999999999999999999",
    "9223372036854775808",
    "99999999999999999999",
];

#[test]
fn a_literal_below_i64_min_is_not_billed_as_i64_min() {
    for literal in WIDE_LITERALS {
        let calls = reads_only(vec![item_with(literal, "")]);
        assert_failed_closed(&run("submitter", "{}", &calls));
        let dry = config_with(&[("METERING_MODE", "dry-run")]);
        assert_failed_closed(&run_with("submitter", &dry, "{}", &calls, NOW));
    }
}

#[test]
fn an_allocation_literal_below_i64_min_is_not_billed_as_i64_min() {
    for literal in WIDE_LITERALS {
        let allocations = format!(
            r#"{{"AllocatedUsageQuantity": {literal}, "Tags": [{{"Key": "team", "Value": "a"}}]}}"#
        );
        let calls = reads_only(vec![item_with("5", &allocations)]);
        assert_failed_closed(&run("submitter", "{}", &calls));
        let dry = config_with(&[("METERING_MODE", "dry-run")]);
        assert_failed_closed(&run_with("submitter", &dry, "{}", &calls, NOW));
    }
}

#[test]
fn a_wide_literal_stops_the_valid_neighbours_in_its_run_too() {
    let allocations = r#"{"AllocatedUsageQuantity": -9223372036854775809, "Tags": [{"Key": "team", "Value": "a"}]}"#;
    for pending in [
        vec![
            agg_item(LICENSE, "Fine", "2026-01-31T11", 5),
            item_with("-9223372036854775809", ""),
            agg_item(LICENSE, "AlsoFine", "2026-01-31T11", 7),
        ],
        vec![
            agg_item(LICENSE, "Fine", "2026-01-31T11", 5),
            item_with("5", allocations),
        ],
    ] {
        let calls = reads_only(pending);
        assert_failed_closed(&run("submitter", "{}", &calls));
        let dry = config_with(&[("METERING_MODE", "dry-run")]);
        assert_failed_closed(&run_with("submitter", &dry, "{}", &calls, NOW));
    }
}

#[test]
fn the_cli_fails_closed_on_a_literal_below_i64_min() {
    let config = TempFile::new(
        "config",
        &format!(
            "{{{}}}",
            CFG.iter()
                .map(|(key, value)| format!(r#""{key}": "{value}""#))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    for pending in [
        item_with("-9223372036854775809", ""),
        item_with(
            "5",
            r#"{"AllocatedUsageQuantity": -9223372036854775809, "Tags": [{"Key": "team", "Value": "a"}]}"#,
        ),
    ] {
        let mut pending_list = vec![agg_item(LICENSE, "Fine", "2026-01-31T11", 5)];
        pending_list.push(pending);
        let recording = TempFile::new(
            "rec",
            &format!(r#"{{"calls": [{}]}}"#, reads_only(pending_list).join(",")),
        );
        let cli = toolkit(&[
            "marketplace",
            "handler",
            "submitter",
            "--config",
            &config.path(),
            "--recorded",
            &recording.path(),
            "--now",
            NOW,
        ]);
        assert_eq!(cli.status, 1, "{}", cli.stderr);
        assert!(cli.stderr.contains("QuantityOutOfRange"), "{}", cli.stderr);
        assert!(
            cli.stderr.contains("nothing was submitted"),
            "the run stopped at the build, before any billing: {}",
            cli.stderr
        );
        assert!(cli.stdout.is_empty(), "{}", cli.stdout);
    }
}

#[test]
fn the_domain_boundaries_themselves_are_still_submitted_as_literals() {
    let min = -SAFE_MAX;
    let max = SAFE_MAX;
    let allocations = format!(
        r#"{{"AllocatedUsageQuantity": {min}, "Tags": [{{"Key": "team", "Value": "a"}}]}}, {{"AllocatedUsageQuantity": {max}, "Tags": [{{"Key": "team", "Value": "b"}}]}}"#
    );
    for quantity in [min, max] {
        let mut calls = reads_only(vec![item_with(&quantity.to_string(), &allocations)]);
        calls.push(metering(&format!(
            r#"{{"Results": [{{"MeteringRecordId": "mrec-edge", "Status": "Success", "UsageRecord": {{"Timestamp": 1769857200, "Dimension": "Requests", "Quantity": {quantity}, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}}}]}}"#
        )));
        calls.push(update());
        let ran = run("submitter", "{}", &calls);
        assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":1}"#);
        let batch = ran.requests("meteringmarketplace:BatchMeterUsage")[0].to_compact_string();
        assert!(
            batch.contains(&format!(
                r#""Quantity":{quantity},"UsageAllocations":[{{"AllocatedUsageQuantity":{min},"Tags":[{{"Key":"team","Value":"a"}}]}},{{"AllocatedUsageQuantity":{max},"Tags""#
            )),
            "the domain boundaries are legitimate integers and are sent unchanged: {batch}"
        );
        assert_eq!(ran.requests("dynamodb:UpdateItem").len(), 1);
        assert_eq!(ran.unused, 0);
    }
}

// ---- the metering quantity domain is |q| <= 2^53 - 1 (#4991, #4992) -------------------
//
// The JSON reader rounds what it cannot hold exactly to a float, so a value the
// reader may have rounded cannot be told from a legitimate one. A metering
// quantity is therefore valid only where an `f64` is exact: an integer with
// `|q| <= 2^53 - 1`. Anything else, however it is written, fails the whole run
// with QuantityOutOfRange before anything is submitted or written.

const SAFE_MAX: i64 = 9_007_199_254_740_991;

/// Quantities outside the domain, in every spelling the recording can use: the
/// integer forms either side of the bound and of `i64`, the float forms the
/// reader rounds onto `i64::MIN` (#4991) and the ones beyond `i64`.
const OUT_OF_DOMAIN: [&str; 21] = [
    "9007199254740992",
    "9007199254740993",
    "-9007199254740992",
    "9223372036854775807",
    "-9223372036854775808",
    "-9223372036854775809",
    "9223372036854775808",
    r#""9007199254740992""#,
    r#""-9007199254740992""#,
    r#""9223372036854775807""#,
    "9007199254740992.0",
    "-9007199254740992.0",
    "9.007199254740992e15",
    "-9223372036854775809.0",
    "-9.223372036854775809e18",
    "-92233720368547758090e-1",
    "-9223372036854775809e0",
    "9223372036854775808.0",
    "1e16",
    "1e19",
    "-1e19",
];

fn dry_config() -> Vec<(&'static str, &'static str)> {
    config_with(&[("METERING_MODE", "dry-run")])
}

#[test]
fn a_stored_quantity_outside_the_domain_fails_the_run_live_and_dry() {
    for literal in OUT_OF_DOMAIN {
        let calls = reads_only(vec![item_with(literal, "")]);
        assert_failed_closed(&run("submitter", "{}", &calls));
        assert_failed_closed(&run_with("submitter", &dry_config(), "{}", &calls, NOW));
    }
}

#[test]
fn an_allocated_quantity_outside_the_domain_fails_the_run_live_and_dry() {
    for literal in OUT_OF_DOMAIN {
        let allocations = format!(
            r#"{{"AllocatedUsageQuantity": {literal}, "Tags": [{{"Key": "team", "Value": "a"}}]}}"#
        );
        let calls = reads_only(vec![item_with("5", &allocations)]);
        assert_failed_closed(&run("submitter", "{}", &calls));
        assert_failed_closed(&run_with("submitter", &dry_config(), "{}", &calls, NOW));
    }
}

#[test]
fn a_quantity_outside_the_domain_stops_the_valid_neighbours_in_its_run_too() {
    for literal in ["-9223372036854775809.0", "9007199254740992", "1e19"] {
        let allocations = format!(
            r#"{{"AllocatedUsageQuantity": {literal}, "Tags": [{{"Key": "team", "Value": "a"}}]}}"#
        );
        for pending in [
            vec![
                agg_item(LICENSE, "Fine", "2026-01-31T11", 5),
                item_with(literal, ""),
                agg_item(LICENSE, "AlsoFine", "2026-01-31T11", 7),
            ],
            vec![
                agg_item(LICENSE, "Fine", "2026-01-31T11", 5),
                item_with("5", &allocations),
            ],
        ] {
            let calls = reads_only(pending);
            assert_failed_closed(&run("submitter", "{}", &calls));
            assert_failed_closed(&run_with("submitter", &dry_config(), "{}", &calls, NOW));
        }
    }
}

#[test]
fn the_largest_safe_quantity_is_submitted_at_both_sites() {
    for spelling in [
        SAFE_MAX.to_string(),
        format!(r#""{SAFE_MAX}""#),
        format!("{SAFE_MAX}.0"),
    ] {
        let allocations = format!(
            r#"{{"AllocatedUsageQuantity": {spelling}, "Tags": [{{"Key": "team", "Value": "a"}}]}}"#
        );
        let mut calls = reads_only(vec![item_with(&spelling, &allocations)]);
        calls.push(metering(&format!(
            r#"{{"Results": [{{"MeteringRecordId": "mrec-safe", "Status": "Success", "UsageRecord": {{"Timestamp": 1769857200, "Dimension": "Requests", "Quantity": {SAFE_MAX}, "CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}"}}}}]}}"#
        )));
        calls.push(update());
        let ran = run("submitter", "{}", &calls);
        assert_eq!(ran.ok().to_compact_string(), r#"{"submitted":1}"#);
        let batch = ran.requests("meteringmarketplace:BatchMeterUsage")[0].to_compact_string();
        assert!(
            batch.contains(&format!(
                r#""Quantity":{SAFE_MAX},"UsageAllocations":[{{"AllocatedUsageQuantity":{SAFE_MAX},"Tags""#
            )),
            "{batch}"
        );
        assert_eq!(ran.requests("dynamodb:UpdateItem").len(), 1);
        assert_eq!(ran.unused, 0);

        // Dry-run accepts it too: it logs the quantity and finalizes without billing.
        let mut dry_calls = reads_only(vec![item_with(&spelling, &allocations)]);
        dry_calls.push(update());
        let dry = run_with("submitter", &dry_config(), "{}", &dry_calls, NOW);
        assert_eq!(dry.ok().to_compact_string(), r#"{"submitted":1}"#);
        assert!(dry
            .requests("meteringmarketplace:BatchMeterUsage")
            .is_empty());
        assert!(dry.logged(&format!("quantity={SAFE_MAX} ")));
        assert_eq!(dry.unused, 0);
    }
}

fn submitter_cli(recording: &str) -> Cli {
    let config = TempFile::new(
        "config",
        &format!(
            "{{{}}}",
            CFG.iter()
                .map(|(key, value)| format!(r#""{key}": "{value}""#))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    let recording = TempFile::new("rec", recording);
    toolkit(&[
        "marketplace",
        "handler",
        "submitter",
        "--config",
        &config.path(),
        "--recorded",
        &recording.path(),
        "--now",
        NOW,
    ])
}

#[test]
fn the_cli_fails_closed_on_a_quantity_outside_the_domain() {
    for literal in [
        "-9223372036854775809.0",
        "-9.223372036854775809e18",
        "9007199254740992",
        "1e19",
    ] {
        for pending in [
            item_with(literal, ""),
            item_with(
                "5",
                &format!(
                    r#"{{"AllocatedUsageQuantity": {literal}, "Tags": [{{"Key": "team", "Value": "a"}}]}}"#
                ),
            ),
        ] {
            let list = vec![agg_item(LICENSE, "Fine", "2026-01-31T11", 5), pending];
            let cli = submitter_cli(&format!(
                r#"{{"calls": [{}]}}"#,
                reads_only(list).join(",")
            ));
            assert_eq!(cli.status, 1, "{literal}: {}", cli.stderr);
            assert!(cli.stderr.contains("QuantityOutOfRange"), "{}", cli.stderr);
            assert!(
                cli.stderr.contains("nothing was submitted"),
                "{literal}: {}",
                cli.stderr
            );
            assert!(cli.stdout.is_empty(), "{}", cli.stdout);
        }
    }
}

#[test]
fn an_unquoted_object_key_is_invalid_json_whatever_its_digits() {
    let calls = reads_only(vec![agg_item(LICENSE, "Fine", "2026-01-31T11", 5)]).join(",");
    for document in [
        format!(r#"{{1: 0, "calls": [{calls}]}}"#),
        format!(r#"{{9223372036854775808: 0, "calls": [{calls}]}}"#),
        format!(r#"{{-9223372036854775809: 0, "calls": [{calls}]}}"#),
        format!(r#"{{99999999999999999999: 0, "calls": [{calls}]}}"#),
        format!(r#"{{"calls": [{calls}], "extra": {{9223372036854775808: 1}}}}"#),
        format!(r#"{{"calls": [{calls}], "extra": [{{"a": {{-99999999999999999999: 1}}}}]}}"#),
    ] {
        let cli = submitter_cli(&document);
        assert_eq!(cli.status, 1, "{document}: {}", cli.stderr);
        assert!(cli.stderr.contains("invalid JSON"), "{}", cli.stderr);
        assert!(cli.stdout.is_empty(), "{}", cli.stdout);
    }
}

#[test]
fn the_recording_reader_rejects_an_unquoted_key_and_keeps_wide_literals_as_numbers() {
    for text in [
        r#"{9223372036854775808: 0, "calls": []}"#,
        r#"{-9223372036854775809: 0, "calls": []}"#,
        r#"{"calls": [], "nested": {99999999999999999999: 1}}"#,
    ] {
        let file = TempFile::new("bare-key", text);
        let error = super::super::read_recording(&file.path()).expect_err(text);
        assert!(error.contains("invalid JSON"), "{error}");
    }
    // A wide literal is a number the quantity checks judge, never a string.
    let file = TempFile::new("wide", r#"{"quantity": -9223372036854775809}"#);
    let parsed = super::super::read_recording(&file.path()).expect("valid JSON");
    let raw = parsed.get("quantity").expect("member");
    assert!(matches!(raw, Json::Float(_) | Json::Int(_)), "{raw:?}");
}
