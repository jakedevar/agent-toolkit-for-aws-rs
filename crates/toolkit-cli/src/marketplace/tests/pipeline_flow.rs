//! The discoverer, aggregator, cleanup and expiry Lambdas.

use super::*;

fn pending_items(count: usize) -> String {
    let items: Vec<String> = (0..count)
        .map(|n| {
            format!(
                r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}", "dimension": "D{n}", "meteringPending": "2026-01-31T11"}}"#
            )
        })
        .collect();
    format!(r#"{{"Items": [{}]}}"#, items.join(","))
}

fn sqs_ok() -> String {
    step("sqs", "SendMessageBatch", None, "{}")
}

#[test]
fn meter_enqueues_one_message_per_group() {
    let event = r#"{"mode": "meter", "hourOffset": 1}"#;
    let calls = [
        step(
            "dynamodb",
            "Query",
            Some(
                r#"{"TableName": "usage", "IndexName": "metering_pending", "KeyConditionExpression": "meteringPending = :hb", "ExpressionAttributeValues": {":hb": "2026-01-31T11"}}"#,
            ),
            &pending_items(2),
        ),
        sqs_ok(),
    ];
    let ran = run("discoverer", event, &calls);
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"hourBucket":"2026-01-31T11","enqueued":2}"#
    );
    let sends = ran.requests("sqs:SendMessageBatch");
    assert_eq!(sends.len(), 1);
    assert_eq!(
        sends[0].to_compact_string(),
        format!(
            r#"{{"QueueUrl":"https://sqs.test/work","Entries":[{{"Id":"0","MessageBody":"{{\"licenseArn\": \"{LICENSE}\", \"customerAWSAccountId\": \"{ACCOUNT}\", \"dimension\": \"D0\", \"hourBucket\": \"2026-01-31T11\"}}"}},{{"Id":"1","MessageBody":"{{\"licenseArn\": \"{LICENSE}\", \"customerAWSAccountId\": \"{ACCOUNT}\", \"dimension\": \"D1\", \"hourBucket\": \"2026-01-31T11\"}}"}}]}}"#
        )
    );
    assert_eq!(ran.unused, 0);
}

#[test]
fn meter_pages_dedupes_and_batches_by_ten() {
    // 25 distinct groups over two pages, one repeated: 25 messages in 3 batches.
    let first = pending_items(13).replace("]}", r#"], "LastEvaluatedKey": {"licenseArn": "k"}}"#);
    let calls = [
        query(&first),
        query(&pending_items(13)),
        sqs_ok(),
        sqs_ok(),
        sqs_ok(),
    ];
    let ran = run(
        "discoverer",
        r#"{"mode": "meter", "hourOffset": 1}"#,
        &calls,
    );
    assert_eq!(ran.ok().get("enqueued"), Some(&Json::Int(13)));
    // The second page repeated the same 13 groups: de-duplicated to 13, so 2 batches.
    assert_eq!(ran.requests("sqs:SendMessageBatch").len(), 2);
    assert_eq!(ran.unused, 1, "the third batch step is unused");
    let queries = ran.requests("dynamodb:Query");
    assert_eq!(
        queries[1]
            .get("ExclusiveStartKey")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{"licenseArn":"k"}"#)
    );
}

#[test]
fn twenty_five_groups_make_three_batches() {
    let calls = [query(&pending_items(25)), sqs_ok(), sqs_ok(), sqs_ok()];
    let ran = run(
        "discoverer",
        r#"{"mode": "meter", "hourOffset": 2}"#,
        &calls,
    );
    assert_eq!(ran.ok().get("enqueued"), Some(&Json::Int(25)));
    assert_eq!(ran.requests("sqs:SendMessageBatch").len(), 3);
}

#[test]
fn meter_is_a_noop_while_the_hour_is_locked() {
    let ran = run_with(
        "discoverer",
        &[
            ("SUBSCRIBERS_TABLE", "s"),
            ("PRODUCT_CODE", "p"),
            ("WORK_QUEUE_URL", "q"),
            ("METERING_LOCK_HOURS", "3"),
        ],
        r#"{"mode": "meter", "hourOffset": 2}"#,
        &[],
        NOW,
    );
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"hourOffset":2,"lock":3,"enqueued":0,"noop":true}"#
    );
    assert!(ran.sent.is_empty());
    // Month end: from 03:00 UTC on the 1st a previous-month bucket has lock 0.
    let ran = run_with(
        "discoverer",
        &[
            ("SUBSCRIBERS_TABLE", "s"),
            ("PRODUCT_CODE", "p"),
            ("WORK_QUEUE_URL", "q"),
            ("USAGE_TABLE", "u"),
            ("METERING_LOCK_HOURS", "5"),
        ],
        r#"{"mode": "meter", "hourOffset": 4}"#,
        &[query(r#"{"Items": []}"#)],
        "2026-02-01T03:30:00Z",
    );
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"hourBucket":"2026-01-31T23","enqueued":0}"#
    );
}

#[test]
fn meter_refuses_bad_input_without_echoing_it() {
    assert!(
        run("discoverer", r#"{"mode": "meter", "hourOffset": 0}"#, &[])
            .err()
            .contains("hourOffset out of range: 0")
    );
    assert!(
        run("discoverer", r#"{"mode": "meter", "hourOffset": 24}"#, &[])
            .err()
            .contains("out of range")
    );
    assert!(run("discoverer", r#"{"mode": "meter"}"#, &[])
        .err()
        .contains("no hourOffset"));
    assert!(
        run("discoverer", r#"{"mode": "meter", "hourOffset": "x"}"#, &[])
            .err()
            .contains("must be an integer")
    );
    assert!(run("discoverer", r#"{"mode": "ageout", "shard": 7}"#, &[])
        .err()
        .contains("shard out of range: 7"));
    let error = run("discoverer", &format!(r#"{{"mode": "{SECRET}"}}"#), &[]).err();
    assert!(error.starts_with("Discoverer invoked without a valid mode/offset"));
    assert!(!error.contains(SECRET), "{error}");
    let missing_queue = run_with(
        "discoverer",
        &[("SUBSCRIBERS_TABLE", "s"), ("PRODUCT_CODE", "p")],
        r#"{"mode": "meter", "hourOffset": 1}"#,
        &[],
        NOW,
    );
    assert_eq!(missing_queue.err(), "WORK_QUEUE_URL not configured");
    let missing_env = run_with(
        "discoverer",
        &[],
        r#"{"mode": "meter", "hourOffset": 1}"#,
        &[],
        NOW,
    );
    assert_eq!(missing_env.err(), "SUBSCRIBERS_TABLE is not configured");
}

#[test]
fn a_failed_enqueue_is_never_swallowed() {
    let calls = [
        query(&pending_items(2)),
        step(
            "sqs",
            "SendMessageBatch",
            None,
            &format!(
                r#"{{"Failed": [{{"Id": "0", "Code": "InternalError", "Message": "{SECRET}"}}]}}"#
            ),
        ),
    ];
    let ran = run(
        "discoverer",
        r#"{"mode": "meter", "hourOffset": 1}"#,
        &calls,
    );
    let error = ran.err();
    assert_eq!(
        error,
        "SQS SendMessageBatch had 1 failure(s): InternalError"
    );
    // A failed GSI query fails the invocation too.
    let ran = run(
        "discoverer",
        r#"{"mode": "meter", "hourOffset": 1}"#,
        &[fail("dynamodb", "Query", "AccessDeniedException")],
    );
    assert_eq!(ran.err(), "aws_service_error (AccessDeniedException)");
    // A null Items is an invalid response, not an empty discovery.
    let ran = run(
        "discoverer",
        r#"{"mode": "meter", "hourOffset": 1}"#,
        &[query(r#"{"Items": null}"#)],
    );
    assert!(ran
        .err()
        .contains("Items must be a list of objects when present (it is null)"));
}

#[test]
fn flush_deprovisioning_bypasses_the_lock_and_gates_the_current_hour() {
    let near = "2026-01-31T12:25:00Z";
    let far = "2026-01-31T13:00:00Z";
    let subscribers = query(&format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "deprovisioningExpiry": "{near}"}}, {{"licenseArn": "arn:aws:license-manager:us-east-1:111122223333:license:l-far", "deprovisioningExpiry": "{far}"}}]}}"#
    ));
    // 24 buckets: the current hour only for the near-expiry license (1 query), the other 23 for both (2 each).
    let mut calls = vec![subscribers];
    let mut expected_groups = 0;
    for hour_offset in 0..24 {
        let licenses = if hour_offset == 0 { 1 } else { 2 };
        for _ in 0..licenses {
            calls.push(query(if hour_offset == 5 && expected_groups == 0 {
                expected_groups += 1;
                r#"{"Items": [{"licenseArn": "x", "customerAWSAccountId": "a", "dimension": "d"}]}"#
            } else {
                r#"{"Items": []}"#
            }));
        }
    }
    calls.push(sqs_ok());
    let ran = run("discoverer", r#"{"mode": "flush-deprovisioning"}"#, &calls);
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"deprovisioningLicenses":2,"enqueued":1}"#
    );
    let sends = ran.requests("sqs:SendMessageBatch");
    assert_eq!(
        sends[0].get("QueueUrl").and_then(Json::as_str),
        Some("https://sqs.test/depwork")
    );
    assert_eq!(ran.unused, 0);
    let queries = ran.requests("dynamodb:Query");
    // The sweep queries per license: meteringPending = :hb AND licenseArn = :la.
    assert_eq!(
        queries[1]
            .get("KeyConditionExpression")
            .and_then(Json::as_str),
        Some("meteringPending = :hb AND licenseArn = :la")
    );
    assert!(ran.logged("(1 also current-hour-flushed)"));

    let none = run(
        "discoverer",
        r#"{"mode": "flush-deprovisioning"}"#,
        &[query(r#"{"Items": []}"#)],
    );
    assert_eq!(
        none.ok().to_compact_string(),
        r#"{"deprovisioningLicenses":0,"enqueued":0}"#
    );
    let unconfigured = run_with(
        "discoverer",
        &[("SUBSCRIBERS_TABLE", "s"), ("PRODUCT_CODE", "p")],
        r#"{"mode": "flush-deprovisioning"}"#,
        &[],
        NOW,
    );
    assert_eq!(
        unconfigured.err(),
        "DEPROVISION_WORK_QUEUE_URL not configured"
    );
}

#[test]
fn an_unparseable_expiry_does_not_flush_the_current_hour() {
    let subscribers = query(&format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "deprovisioningExpiry": "not a time"}}]}}"#
    ));
    // Only the 23 completed hours are swept (one license).
    let mut calls = vec![subscribers];
    calls.extend((0..23).map(|_| query(r#"{"Items": []}"#)));
    let ran = run("discoverer", r#"{"mode": "flush-deprovisioning"}"#, &calls);
    assert_eq!(ran.ok().get("enqueued"), Some(&Json::Int(0)));
    assert!(ran.logged("(0 also current-hour-flushed)"));
    assert_eq!(ran.unused, 0);
}

#[test]
fn ageout_expires_stale_rows_and_stops_after_six_empty_buckets() {
    // Shard 3 starts at hours_back 24; the first bucket holds one stale row.
    let row = format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "customerAWSAccountId_dimension_timestamp": "{ACCOUNT}#D0#2026-01-30T12:00:01", "meteringPending": "2026-01-30T12"}}]}}"#
    );
    let mut calls = vec![query(&pending_items(1)), query(&row), update()];
    calls.extend((0..6).map(|_| query(r#"{"Items": []}"#)));
    let ran = run("discoverer", r#"{"mode": "ageout", "shard": 3}"#, &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"shard":3,"expired":1}"#);
    let updates = ran.requests("dynamodb:UpdateItem");
    assert_eq!(
        updates[0].get("UpdateExpression").and_then(Json::as_str),
        Some("REMOVE meteringPending SET meteringStatus = :st, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        updates[0]
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{":st":"AggregationExpired",":now":"2026-01-31T12:30:00Z"}"#)
    );
    assert!(ran.logged("AGE-OUT: dropping pending usage older than 24h (unbillable)"));
    assert!(ran
        .log
        .iter()
        .any(|l| l.contains(r#""UsageAggregationExpired": 1"#)));
    assert_eq!(ran.unused, 0);
}

#[test]
fn ageout_skips_previous_month_buckets_while_the_grace_is_open() {
    // 05:30 on the 1st: every older bucket is a previous-month one, so nothing is read.
    let ran = run_with(
        "discoverer",
        &CFG,
        r#"{"mode": "ageout", "shard": 0}"#,
        &[],
        "2026-02-01T05:30:00Z",
    );
    assert_eq!(ran.ok().to_compact_string(), r#"{"shard":0,"expired":0}"#);
    assert!(ran.sent.is_empty());
}

// ---- aggregator --------------------------------------------------------------

fn sqs_record(id: &str, body: &str) -> String {
    format!(
        r#"{{"messageId": "{id}", "body": {}}}"#,
        Json::string(body).to_compact_string()
    )
}

fn group_message(dimension: &str, bucket: &str) -> String {
    format!(
        r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}", "dimension": "{dimension}", "hourBucket": "{bucket}"}}"#
    )
}

fn raw_row(dimension: &str, second: u32, quantity: &str, bucket: &str) -> String {
    format!(
        r#"{{"licenseArn": "{LICENSE}", "customerAWSAccountId_dimension_timestamp": "{ACCOUNT}#{dimension}#{bucket}:00:{second:02}", "meteringPending": "{bucket}", "quantity": {quantity}}}"#
    )
}

fn event_of(records: &[String]) -> String {
    format!(r#"{{"Records": [{}]}}"#, records.join(","))
}

#[test]
fn a_valid_group_is_committed_once_and_cleanup_is_enqueued() {
    let rows = format!(
        r#"{{"Items": [{}, {}]}}"#,
        raw_row("Requests", 1, "3", "2026-01-31T11"),
        raw_row("Requests", 2, "4", "2026-01-31T11")
    );
    let calls = [
        query(&rows),
        step("dynamodb", "PutItem", None, "{}"),
        sqs_ok(),
    ];
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run("aggregator", &event, &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let put = ran.requests("dynamodb:PutItem")[0];
    assert_eq!(
        put.to_compact_string(),
        format!(
            r#"{{"TableName":"agg","Item":{{"licenseArn":"{LICENSE}","account_dimension_hour":"{ACCOUNT}#Requests#2026-01-31T11","customerAWSAccountId":"{ACCOUNT}","dimension":"Requests","hourBucket":"2026-01-31T11","quantity":7,"meteringPending":"2026-01-31T11","createdAt":"2026-01-31T12:30:00Z","updatedAt":"2026-01-31T12:30:00Z"}},"ConditionExpression":"attribute_not_exists(licenseArn)"}}"#
        )
    );
    let send = ran.requests("sqs:SendMessageBatch")[0];
    let body = send
        .get("Entries")
        .map(Json::to_compact_string)
        .unwrap_or_default();
    assert!(
        body.contains(
            r#"\"meteringStatus\": \"Aggregated\", \"totalQuantity\": 7, \"recordCount\": 2"#
        ),
        "{body}"
    );
    assert_eq!(
        send.get("QueueUrl").and_then(Json::as_str),
        Some("https://sqs.test/cleanup")
    );
}

#[test]
fn the_query_reads_the_group_by_sort_key_prefix() {
    let calls = [query(r#"{"Items": []}"#)];
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run("aggregator", &event, &calls);
    let request = ran.requests("dynamodb:Query")[0];
    assert_eq!(
        request.get("KeyConditionExpression").and_then(Json::as_str),
        Some("licenseArn = :la AND begins_with(customerAWSAccountId_dimension_timestamp, :pfx)")
    );
    assert_eq!(
        request
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string),
        Some(format!(
            r#"{{":la":"{LICENSE}",":pfx":"{ACCOUNT}#Requests#2026-01-31T11"}}"#
        ))
    );
    // No pending rows: a no-op.
    assert!(ran.requests("dynamodb:PutItem").is_empty());
    assert!(ran.logged("No pending rows for group"));
}

#[test]
fn a_duplicate_visit_writes_nothing_and_enqueues_no_cleanup() {
    let rows = format!(
        r#"{{"Items": [{}]}}"#,
        raw_row("Requests", 1, "3", "2026-01-31T11")
    );
    let calls = [
        query(&rows),
        fail("dynamodb", "PutItem", "ConditionalCheckFailedException"),
    ];
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run("aggregator", &event, &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    assert!(ran.requests("sqs:SendMessageBatch").is_empty());
    assert!(ran.logged("already exists (idempotent)"));
}

#[test]
fn an_invalid_group_is_rejected_client_side() {
    let rows = format!(
        r#"{{"Items": [{}, {}]}}"#,
        raw_row("Requests", 1, "3", "2026-01-31T11"),
        raw_row("Requests", 2, r#""1.5""#, "2026-01-31T11")
    );
    let calls = [query(&rows), update(), update()];
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run("aggregator", &event, &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let updates = ran.requests("dynamodb:UpdateItem");
    assert_eq!(updates.len(), 2, "every raw row is finalized");
    assert_eq!(
        updates[0].get("UpdateExpression").and_then(Json::as_str),
        Some("REMOVE meteringPending SET meteringStatus = :st, meteringStatusReason = :rsn, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        updates[0]
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(
            r#"{":st":"RejectedClientSide",":rsn":"NonIntegerQuantity",":now":"2026-01-31T12:30:00Z"}"#
        )
    );
    assert!(
        ran.requests("dynamodb:PutItem").is_empty(),
        "a rejected group is never aggregated"
    );
    assert!(ran
        .log
        .iter()
        .any(|l| l.contains(r#""UsageRecordRejected": 1"#)));
    assert!(ran.logged("Rejecting usage group client-side"));
}

/// One tagged raw row of `quantity` with the given allocations.
fn allocated_row(quantity: i64, allocations: &[(i64, &str)]) -> String {
    let list: Vec<String> = allocations
        .iter()
        .map(|(allocated, value)| {
            format!(
                r#"{{"AllocatedUsageQuantity": {allocated}, "Tags": [{{"Key": "team", "Value": "{value}"}}]}}"#
            )
        })
        .collect();
    let row = raw_row("Requests", 1, &quantity.to_string(), "2026-01-31T11");
    format!(
        r#"{}, "usageAllocations": [{}]}}"#,
        row.trim_end_matches('}'),
        list.join(",")
    )
}

fn aggregate_one(row: &str) -> Ran {
    let calls = [query(&format!(r#"{{"Items": [{row}]}}"#)), update()];
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    run("aggregator", &event, &calls)
}

fn assert_rejected_with(ran: &Ran, reason: &str) {
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    assert!(
        ran.requests("dynamodb:PutItem").is_empty(),
        "an overflowing group is never aggregated"
    );
    assert!(
        ran.requests("sqs:SendMessageBatch").is_empty(),
        "and never reaches cleanup"
    );
    let updates = ran.requests("dynamodb:UpdateItem");
    assert_eq!(updates.len(), 1);
    let values = updates[0]
        .get("ExpressionAttributeValues")
        .map(Json::to_compact_string)
        .unwrap_or_default();
    assert!(
        values.contains(r#"":st":"RejectedClientSide""#) && values.contains(reason),
        "{values}"
    );
    assert_eq!(ran.unused, 0);
}

#[test]
fn overflowing_allocations_on_one_tag_set_reject_the_group() {
    let row = allocated_row(i64::MAX, &[(i64::MAX, "a"), (1, "a")]);
    assert_rejected_with(&aggregate_one(&row), "QuantityOutOfRange");
}

#[test]
fn overflowing_allocation_totals_across_tag_sets_reject_the_group() {
    let row = allocated_row(i64::MAX, &[(i64::MAX, "a"), (1, "b")]);
    assert_rejected_with(&aggregate_one(&row), "AllocationSumMismatch");
}

#[test]
fn zero_quantity_is_left_pending_except_at_the_oldest_edge() {
    let zero = |bucket: &str| format!(r#"{{"Items": [{}]}}"#, raw_row("Requests", 1, "0", bucket));
    // In-window: left pending, nothing written.
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run("aggregator", &event, &[query(&zero("2026-01-31T11"))]);
    assert!(ran.requests("dynamodb:PutItem").is_empty());
    assert!(ran.logged("In-window zero-quantity group left pending"));
    // now = 12:30, so the oldest edge is 2026-01-30T13: aggregated as quantity 0.
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-30T13"),
    )]);
    let ran = run(
        "aggregator",
        &event,
        &[
            query(&zero("2026-01-30T13")),
            step("dynamodb", "PutItem", None, "{}"),
            sqs_ok(),
        ],
    );
    assert_eq!(ran.requests("dynamodb:PutItem").len(), 1);
    assert!(ran.requests("dynamodb:PutItem")[0]
        .to_compact_string()
        .contains(r#""quantity":0"#));
}

#[test]
fn a_failing_message_is_reported_alone() {
    let rows = format!(
        r#"{{"Items": [{}]}}"#,
        raw_row("Requests", 1, "3", "2026-01-31T11")
    );
    let event = event_of(&[
        sqs_record("bad-json", "{not json"),
        sqs_record("good", &group_message("Requests", "2026-01-31T11")),
        sqs_record("no-field", r#"{"licenseArn": "x"}"#),
    ]);
    let calls = [
        query(&rows),
        step("dynamodb", "PutItem", None, "{}"),
        sqs_ok(),
    ];
    let ran = run("aggregator", &event, &calls);
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"batchItemFailures":[{"itemIdentifier":"bad-json"},{"itemIdentifier":"no-field"}]}"#
    );
    // A cleanup enqueue failure fails the message, never the whole batch.
    let calls = [
        query(&rows),
        step("dynamodb", "PutItem", None, "{}"),
        step(
            "sqs",
            "SendMessageBatch",
            None,
            r#"{"Failed": [{"Id": "0"}]}"#,
        ),
    ];
    let event = event_of(&[sqs_record(
        "lost",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run("aggregator", &event, &calls);
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"batchItemFailures":[{"itemIdentifier":"lost"}]}"#
    );
    assert!(ran.logged("SQS SendMessageBatch had 1 cleanup failure(s)"));
}

#[test]
fn the_aggregator_needs_its_environment() {
    let cfg: Vec<(&str, &str)> = CFG
        .iter()
        .copied()
        .filter(|(k, _)| *k != "CLEANUP_QUEUE_URL")
        .collect();
    let ran = run_with("aggregator", &cfg, &event_of(&[]), &[], NOW);
    assert_eq!(ran.err(), "CLEANUP_QUEUE_URL not configured");
    let cfg: Vec<(&str, &str)> = CFG
        .iter()
        .copied()
        .filter(|(k, _)| *k != "AGGREGATED_USAGE_TABLE")
        .collect();
    let ran = run_with("aggregator", &cfg, &event_of(&[]), &[], NOW);
    assert_eq!(ran.err(), "AGGREGATED_USAGE_TABLE is not configured");
}

#[test]
fn rows_over_a_hundred_keys_are_split_across_cleanup_messages() {
    // Distinct sort keys: the minute varies, the second repeats every 60 rows.
    let rows: Vec<String> = (0..205_u32)
        .map(|n| {
            raw_row("Requests", n % 60, "1", "2026-01-31T11")
                .replace(":00:", &format!(":{:02}:", n / 60))
        })
        .collect();
    let items = format!(r#"{{"Items": [{}]}}"#, rows.join(","));
    let event = event_of(&[sqs_record(
        "m1",
        &group_message("Requests", "2026-01-31T11"),
    )]);
    let ran = run(
        "aggregator",
        &event,
        &[
            query(&items),
            step("dynamodb", "PutItem", None, "{}"),
            sqs_ok(),
        ],
    );
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let send = ran.requests("sqs:SendMessageBatch")[0];
    let entries = send
        .get("Entries")
        .map(Json::to_compact_string)
        .unwrap_or_default();
    // 205 keys: messages of 100, 100 and 5, each carrying the group total.
    assert_eq!(entries.matches(r#""Id""#).count(), 3);
    assert_eq!(entries.matches(r#"totalQuantity\": 205"#).count(), 3);
}

// ---- cleanup ------------------------------------------------------------------

#[test]
fn cleanup_stamps_the_aggregated_status_when_it_knows_the_sum() {
    let body = format!(
        r#"{{"licenseArn": "{LICENSE}", "rowKeys": ["k1", "", "k2"], "meteringStatus": "Aggregated", "totalQuantity": 7, "recordCount": 2}}"#
    );
    let ran = run(
        "cleanup",
        &event_of(&[sqs_record("m1", &body)]),
        &[update(), update()],
    );
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let updates = ran.requests("dynamodb:UpdateItem");
    assert_eq!(updates.len(), 2, "the empty key is skipped");
    assert_eq!(
        updates[0]
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(
            r#"{":st":"Aggregated",":rsn":"Aggregated total quantity 7 from 2 raw usage records",":now":"2026-01-31T12:30:00Z"}"#
        )
    );
    assert!(ran.logged("Finalized 2 raw row(s) as Aggregated"));
}

#[test]
fn cleanup_without_a_status_only_clears_the_pending_marker() {
    let body = format!(r#"{{"licenseArn": "{LICENSE}", "rowKeys": ["k1"]}}"#);
    let ran = run(
        "cleanup",
        &event_of(&[sqs_record("m1", &body)]),
        &[update()],
    );
    let updates = ran.requests("dynamodb:UpdateItem");
    assert_eq!(
        updates[0].get("UpdateExpression").and_then(Json::as_str),
        Some("SET createdAt = if_not_exists(createdAt, :now), updatedAt = :now REMOVE meteringPending")
    );
    assert_eq!(
        updates[0]
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{":now":"2026-01-31T12:30:00Z"}"#)
    );
    assert!(ran.logged("Cleared meteringPending on 1 raw row(s) (no status)"));
    // No keys: nothing to do.
    let none = run(
        "cleanup",
        &event_of(&[sqs_record(
            "m2",
            &format!(r#"{{"licenseArn": "{LICENSE}", "rowKeys": []}}"#),
        )]),
        &[],
    );
    assert!(none.sent.is_empty());
}

#[test]
fn cleanup_fails_the_message_when_a_row_cannot_be_written() {
    let body = format!(r#"{{"licenseArn": "{LICENSE}", "rowKeys": ["k1", "k2"]}}"#);
    let calls = [
        fail("dynamodb", "UpdateItem", "InternalServerError"),
        update(),
    ];
    let ran = run("cleanup", &event_of(&[sqs_record("m1", &body)]), &calls);
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"batchItemFailures":[{"itemIdentifier":"m1"}]}"#
    );
    assert!(ran.logged("Cleanup failed for a message: 1/2 UpdateItem(s) failed"));
    assert!(!ran.log.iter().any(|l| l.contains(SECRET)));
}

// ---- expiry -------------------------------------------------------------------

#[test]
fn expiry_expires_pending_aggregated_records_past_the_window() {
    // now = Jan 31 12:30: no previous-month in-window bucket; the sweep starts at 24h.
    let item = format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "account_dimension_hour": "{ACCOUNT}#R#2026-01-30T12"}}]}}"#
    );
    let mut calls = vec![query(&item), update()];
    calls.extend((0..6).map(|_| query(r#"{"Items": []}"#)));
    let ran = run("expiry", "{}", &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"expired":1}"#);
    let update = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        update
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{":st":"SubmissionExpired",":now":"2026-01-31T12:30:00Z"}"#)
    );
    assert_eq!(update.get("TableName").and_then(Json::as_str), Some("agg"));
    assert!(ran.logged("AGGREGATED EXPIRY: unbillable pending aggregated record"));
    assert!(ran
        .log
        .iter()
        .any(|l| l.contains(r#""UsageSubmissionExpired": 1"#)));
    assert!(ran.logged("Expired 1 aggregated record(s) past the submittable window"));
    assert_eq!(ran.unused, 0);
    let first = ran.requests("dynamodb:Query")[0];
    assert_eq!(
        first
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{":hb":"2026-01-30T12"}"#)
    );
}

#[test]
fn expiry_expires_previous_month_buckets_in_window_once_the_grace_closed() {
    // 07:30 on the 1st: in-window hours_back 8..=23 are January buckets (16 queries), then 6 empty.
    let mut calls: Vec<String> = (0..16).map(|_| query(r#"{"Items": []}"#)).collect();
    calls.extend((0..6).map(|_| query(r#"{"Items": []}"#)));
    let ran = run_with("expiry", &CFG, "{}", &calls, "2026-02-01T07:30:00Z");
    assert_eq!(ran.ok().to_compact_string(), r#"{"expired":0}"#);
    assert_eq!(ran.requests("dynamodb:Query").len(), 22);
    assert_eq!(ran.unused, 0);
    assert!(ran.logged("No aggregated records to expire"));
}

#[test]
fn expiry_leaves_previous_month_records_alone_while_the_grace_is_open() {
    let ran = run_with("expiry", &CFG, "{}", &[], "2026-02-01T05:30:00Z");
    assert_eq!(ran.ok().to_compact_string(), r#"{"expired":0}"#);
    assert!(ran.sent.is_empty());
}

#[test]
fn a_total_beyond_two_pow_53_minus_1_rejects_the_group_at_aggregation() {
    // The domain bound of a metering quantity (#4991): exactly 2^53 - 1 stays
    // valid at the submitter, so nothing larger may be aggregated for it.
    let safe_max = 9_007_199_254_740_991_i64;
    let row = allocated_row(safe_max + 1, &[(safe_max + 1, "a")]);
    assert_rejected_with(&aggregate_one(&row), "QuantityOutOfRange");
    let split = allocated_row(safe_max, &[(safe_max, "a"), (1, "a")]);
    assert_rejected_with(&aggregate_one(&split), "QuantityOutOfRange");
}
