//! The queue-driven pipeline Lambdas: `discoverer.py`, `aggregator.py`,
//! `cleanup.py` and `expiry.py`.

use toolkit_aws::recorded::optional_list;
use toolkit_core::{parse_json, Json};

use super::base::{shown, Ctx, Failure};
use super::client::Service;
use super::clock::{add_hours, floor_hour, hour_bucket};
use super::core::{
    self, agg_sort_key, audit_item, collect_group, emf_metric, finalize_allocations,
    get_pending_groups, get_pending_groups_for_license, hour_bucket_for_offset,
    is_previous_month_bucket, mask, previous_month_grace_open, read_group_rows,
    update_rows_bounded, validate_group, Group, PendingGroup, CLEANUP_KEYS_PER_MESSAGE,
    STATUS_REJECTED,
};
use super::ddb::{self, required_text, Query, Update};
use crate::cloudwatch::timefmt::{parse_iso_timestamp, UtcTime};
use crate::dynamodb::pyfmt::{dumps, text};

/// The age-out sweep stops after this many empty buckets in a row.
const AGEOUT_EMPTY_STREAK_STOP: u32 = 6;
/// How far back the sweeps look.
const MAX_HOURS_BACK: i64 = 24 * 400;
/// Age-out shards.
const AGEOUT_SHARDS: i64 = 7;
/// A deprovisioning license's current hour is flushed this long before its
/// window closes.
const CURRENT_HOUR_LEAD_MICROS: i64 = 10 * 60 * 1_000_000;

/// A `Result` of a handler: the value the Lambda returns.
pub(super) type Handled = Result<Json, Failure>;

fn invalid(error: impl std::fmt::Display) -> Failure {
    Failure::message(format!("invalid event: {error}"))
}

/// The `Records` of an SQS event.
fn sqs_records(event: &Json) -> Result<&[Json], Failure> {
    optional_list(event, "Records").map_err(invalid)
}

fn message_id(record: &Json) -> Result<String, Failure> {
    required_text(record, "messageId")
}

/// `json.loads(record["body"])`.
fn message_body(record: &Json) -> Result<Json, Failure> {
    let body = required_text(record, "body")?;
    parse_json(&body).map_err(|_| Failure::message("the message body is not valid JSON"))
}

fn batch_failures(ids: Vec<String>) -> Json {
    let failures = ids
        .into_iter()
        .map(|id| Json::object(vec![("itemIdentifier", Json::string(id))]))
        .collect();
    Json::object(vec![("batchItemFailures", Json::Array(failures))])
}

fn integer(value: i64) -> Json {
    Json::Int(value)
}

/// `SendMessageBatch` of already built bodies, ten entries per call. A
/// partial `Failed` result is a failure (those groups would silently not be
/// metered).
fn send_batches(
    ctx: &mut Ctx<'_>,
    queue_url: &str,
    bodies: &[Json],
    failure_label: &str,
) -> Result<(), Failure> {
    for chunk in bodies.chunks(10) {
        let entries: Vec<Json> = chunk
            .iter()
            .enumerate()
            .map(|(index, body)| {
                Json::object(vec![
                    ("Id", Json::string(index.to_string())),
                    ("MessageBody", Json::string(dumps(body, false))),
                ])
            })
            .collect();
        let request = Json::object(vec![
            ("QueueUrl", Json::string(queue_url)),
            ("Entries", Json::Array(entries)),
        ]);
        let response = ctx
            .client
            .call(Service::Sqs, "SendMessageBatch", &request)?;
        let failed = optional_list(&response, "Failed").map_err(|error| {
            Failure::message(format!("invalid SendMessageBatch response: {error}"))
        })?;
        if !failed.is_empty() {
            let codes: Vec<String> = failed
                .iter()
                .map(|entry| {
                    shown(
                        entry
                            .get("Code")
                            .and_then(Json::as_str)
                            .unwrap_or("unknown"),
                    )
                })
                .collect();
            return Err(Failure::message(format!(
                "SQS SendMessageBatch had {} {failure_label}failure(s): {}",
                failed.len(),
                codes.join(", ")
            )));
        }
    }
    Ok(())
}

// ---- discoverer --------------------------------------------------------------

fn event_integer(event: &Json, key: &str) -> Result<i64, Failure> {
    let value = event
        .get(key)
        .ok_or_else(|| Failure::message(format!("the event has no {key}")))?;
    let parsed = match value {
        Json::Int(number) => Some(*number),
        #[allow(clippy::cast_possible_truncation)]
        Json::Float(number) if number.is_finite() => Some(number.trunc() as i64),
        Json::Bool(flag) => Some(i64::from(*flag)),
        Json::String(text) => super::base::py_int(text),
        _ => None,
    };
    parsed.ok_or_else(|| Failure::message(format!("{key} must be an integer")))
}

/// `discoverer.handler`: `meter`, `ageout` or `flush-deprovisioning`.
pub(super) fn discoverer(ctx: &mut Ctx<'_>, event: &Json) -> Handled {
    ctx.require_core()?;
    match event.get("mode").and_then(Json::as_str) {
        Some("meter") => handle_meter(ctx, event_integer(event, "hourOffset")?),
        Some("ageout") => handle_ageout(ctx, event_integer(event, "shard")?),
        Some("flush-deprovisioning") => handle_flush_deprovisioning(ctx),
        _ => Err(Failure::message(
            "Discoverer invoked without a valid mode/offset (mode must be meter, ageout or flush-deprovisioning)",
        )),
    }
}

fn work_message(group: &PendingGroup, bucket: &str) -> Json {
    Json::object(vec![
        ("licenseArn", Json::string(group.0.clone())),
        ("customerAWSAccountId", Json::string(group.1.clone())),
        ("dimension", Json::string(group.2.clone())),
        ("hourBucket", Json::string(bucket)),
    ])
}

fn enqueue_work(
    ctx: &mut Ctx<'_>,
    groups: &[PendingGroup],
    bucket: &str,
    queue_url: &str,
) -> Result<i64, Failure> {
    let bodies: Vec<Json> = groups
        .iter()
        .map(|group| work_message(group, bucket))
        .collect();
    send_batches(ctx, queue_url, &bodies, "")?;
    Ok(i64::try_from(bodies.len()).unwrap_or(i64::MAX))
}

fn handle_meter(ctx: &mut Ctx<'_>, hour_offset: i64) -> Handled {
    if !(1..=23).contains(&hour_offset) {
        return Err(Failure::message(format!(
            "hourOffset out of range: {hour_offset}"
        )));
    }
    let queue = ctx.config.text("WORK_QUEUE_URL").to_string();
    if queue.is_empty() {
        return Err(Failure::message("WORK_QUEUE_URL not configured"));
    }
    let bucket = hour_bucket_for_offset(ctx.now, hour_offset);
    let configured = ctx.config.integer("METERING_LOCK_HOURS", 1)?;
    let lock = core::effective_lock_hours(&bucket, ctx.now, configured);
    if hour_offset < lock {
        ctx.log.info(&format!(
            "hourOffset {hour_offset} < effective lock {lock} for bucket {}: still open for late usage; no-op",
            shown(&bucket)
        ));
        return Ok(Json::object(vec![
            ("hourOffset", integer(hour_offset)),
            ("lock", integer(lock)),
            ("enqueued", integer(0)),
            ("noop", Json::Bool(true)),
        ]));
    }
    let groups = get_pending_groups(ctx, &bucket)?;
    if groups.is_empty() {
        ctx.log.info(&format!(
            "No pending usage for hour bucket {} (offset {hour_offset})",
            shown(&bucket)
        ));
        return Ok(Json::object(vec![
            ("hourBucket", Json::string(bucket)),
            ("enqueued", integer(0)),
        ]));
    }
    let enqueued = enqueue_work(ctx, &groups, &bucket, &queue)?;
    ctx.log.info(&format!(
        "Enqueued {enqueued} group(s) for hour bucket {}",
        shown(&bucket)
    ));
    Ok(Json::object(vec![
        ("hourBucket", Json::string(bucket)),
        ("enqueued", integer(enqueued)),
    ]))
}

/// `_parse_iso`: an ISO-8601 timestamp, or `None` when absent or unparseable
/// (the current hour is then conservatively not flushed).
fn parse_iso(value: Option<&str>) -> Option<UtcTime> {
    value
        .filter(|text| !text.is_empty())
        .and_then(|text| parse_iso_timestamp(text).ok())
}

fn handle_flush_deprovisioning(ctx: &mut Ctx<'_>) -> Handled {
    let queue = ctx.config.text("DEPROVISION_WORK_QUEUE_URL").to_string();
    if queue.is_empty() {
        return Err(Failure::message(
            "DEPROVISION_WORK_QUEUE_URL not configured",
        ));
    }
    let deprovisioning = core::deprovisioning_licenses(ctx)?;
    if deprovisioning.is_empty() {
        ctx.log
            .info("flush-deprovisioning: no licenses in a deprovisioning window");
        return Ok(Json::object(vec![
            ("deprovisioningLicenses", integer(0)),
            ("enqueued", integer(0)),
        ]));
    }
    let now = ctx.now;
    let current_hour: Vec<&str> = deprovisioning
        .iter()
        .filter(|(_, expiry)| {
            parse_iso(expiry.as_deref())
                .and_then(|expiry| expiry.checked_add_micros(-CURRENT_HOUR_LEAD_MICROS))
                .is_some_and(|threshold| now >= threshold)
        })
        .map(|(arn, _)| arn.as_str())
        .collect();
    let mut total = 0_i64;
    for hour_offset in 0..24 {
        let bucket = hour_bucket_for_offset(now, hour_offset);
        let licenses: Vec<&str> = if hour_offset == 0 {
            current_hour.clone()
        } else {
            deprovisioning.iter().map(|(arn, _)| arn.as_str()).collect()
        };
        if licenses.is_empty() {
            continue;
        }
        let mut groups: Vec<PendingGroup> = Vec::new();
        for license in licenses {
            groups.extend(get_pending_groups_for_license(ctx, &bucket, license)?);
        }
        if !groups.is_empty() {
            total += enqueue_work(ctx, &groups, &bucket, &queue)?;
        }
    }
    ctx.log.info(&format!(
        "flush-deprovisioning: enqueued {total} group(s) across {} deprovisioning license(s) \
         ({} also current-hour-flushed) (lock bypassed)",
        deprovisioning.len(),
        current_hour.len()
    ));
    Ok(Json::object(vec![
        (
            "deprovisioningLicenses",
            integer(i64::try_from(deprovisioning.len()).unwrap_or(i64::MAX)),
        ),
        ("enqueued", integer(total)),
    ]))
}

fn handle_ageout(ctx: &mut Ctx<'_>, shard: i64) -> Handled {
    if !(0..AGEOUT_SHARDS).contains(&shard) {
        return Err(Failure::message(format!("shard out of range: {shard}")));
    }
    let now = ctx.now;
    let base = floor_hour(now);
    let mut hours_back = 24_i64;
    let mut empty_streak = 0_u32;
    let mut aged = 0_i64;
    while hours_back <= MAX_HOURS_BACK && empty_streak < AGEOUT_EMPTY_STREAK_STOP {
        if hours_back % AGEOUT_SHARDS != shard {
            hours_back += 1;
            continue;
        }
        let bucket = hour_bucket(add_hours(base, -hours_back));
        hours_back += 1;
        if previous_month_grace_open(now) && is_previous_month_bucket(&bucket, now) {
            continue;
        }
        let pending = get_pending_groups(ctx, &bucket)?;
        if pending.is_empty() {
            empty_streak += 1;
            continue;
        }
        empty_streak = 0;
        aged += expire_raw_bucket(ctx, &pending, &bucket)?;
    }
    ctx.log
        .info(&format!("Age-out shard {shard}: expired {aged} row(s)"));
    Ok(Json::object(vec![
        ("shard", integer(shard)),
        ("expired", integer(aged)),
    ]))
}

fn expire_raw_bucket(
    ctx: &mut Ctx<'_>,
    pending: &[PendingGroup],
    bucket: &str,
) -> Result<i64, Failure> {
    let table = ctx.usage_table()?.to_string();
    let mut aged = 0_i64;
    for (license_arn, account_id, dimension) in pending {
        for row in read_group_rows(ctx, license_arn, account_id, dimension, bucket)? {
            if row.get("meteringPending").is_none() {
                continue;
            }
            let sort_key = ddb::text_or_empty(&row, "customerAWSAccountId_dimension_timestamp")?;
            ctx.log.warning(&format!(
                "AGE-OUT: dropping pending usage older than 24h (unbillable) license={} account={} dim={} hour={}",
                shown(&mask(license_arn)),
                shown(&mask(account_id)),
                shown(dimension),
                shown(bucket)
            ));
            emf_metric(ctx, "UsageAggregationExpired", 1, None, None);
            let (expression, values) = core::with_audit_timestamps(
                "REMOVE meteringPending SET meteringStatus = :st",
                vec![(":st".to_string(), Json::string("AggregationExpired"))],
                ctx.now,
            );
            let update = Update {
                table: &table,
                key: vec![
                    ("licenseArn", Json::string(license_arn.clone())),
                    (
                        "customerAWSAccountId_dimension_timestamp",
                        Json::string(sort_key),
                    ),
                ],
                expression: &expression,
                condition: None,
                values: Some(values),
                names: None,
            };
            ddb::update_item(ctx.client, &update)?;
            aged += 1;
        }
    }
    Ok(aged)
}

// ---- aggregator ---------------------------------------------------------------

/// `aggregator.handler`: one SQS message is one group.
pub(super) fn aggregator(ctx: &mut Ctx<'_>, event: &Json) -> Handled {
    ctx.require_core()?;
    ctx.aggregated_table()?;
    if ctx.config.text("CLEANUP_QUEUE_URL").is_empty() {
        return Err(Failure::message("CLEANUP_QUEUE_URL not configured"));
    }
    let mut failures = Vec::new();
    for record in sqs_records(event)? {
        let outcome = message_body(record).and_then(|body| aggregate_message(ctx, &body));
        if let Err(failure) = outcome {
            ctx.log
                .error(&format!("Aggregation failed for a group: {failure}"));
            failures.push(message_id(record)?);
        }
    }
    Ok(batch_failures(failures))
}

fn aggregate_message(ctx: &mut Ctx<'_>, message: &Json) -> Result<(), Failure> {
    let license_arn = required_text(message, "licenseArn")?;
    let account_id = required_text(message, "customerAWSAccountId")?;
    let dimension = required_text(message, "dimension")?;
    let bucket = required_text(message, "hourBucket")?;

    let group = collect_group(ctx, &license_arn, &account_id, &dimension, &bucket)?;
    if group.row_keys.is_empty() {
        ctx.log.info(&format!(
            "No pending rows for group license={} dim={} hour={}; skipping",
            shown(&mask(&license_arn)),
            shown(&dimension),
            shown(&bucket)
        ));
        return Ok(());
    }
    let oldest_edge = add_hours(floor_hour(ctx.now), -23);
    if group.quantity == 0 && group.reject_reason.is_none() && group.hour_ts > oldest_edge {
        ctx.log.info(&format!(
            "In-window zero-quantity group left pending license={} dim={} hour={}",
            shown(&mask(&license_arn)),
            shown(&dimension),
            shown(&bucket)
        ));
        return Ok(());
    }
    if let Some(reason) = validate_group(&group, ctx.now) {
        return reject(ctx, &group, reason);
    }
    if write_aggregated(ctx, &group)? {
        enqueue_cleanup(ctx, &group)
    } else {
        ctx.log.info(&format!(
            "Aggregated record already exists (idempotent) for license={} dim={} hour={}; \
             NOT re-enqueuing cleanup (inflation guard). Any still-pending rows self-heal on the next scheduled re-visit.",
            shown(&mask(&license_arn)),
            shown(&dimension),
            shown(&bucket)
        ));
        Ok(())
    }
}

/// The conditional `PutItem` into `aggregated_usage` (the idempotency commit
/// point): `true` when written, `false` when the record already existed.
fn write_aggregated(ctx: &mut Ctx<'_>, group: &Group) -> Result<bool, Failure> {
    let table = ctx.aggregated_table()?.to_string();
    let mut members = vec![
        (
            "licenseArn".to_string(),
            Json::string(group.license_arn.clone()),
        ),
        (
            "account_dimension_hour".to_string(),
            Json::string(agg_sort_key(
                &group.account_id,
                &group.dimension,
                &group.hour_bucket,
            )),
        ),
        (
            "customerAWSAccountId".to_string(),
            Json::string(group.account_id.clone()),
        ),
        (
            "dimension".to_string(),
            Json::string(group.dimension.clone()),
        ),
        (
            "hourBucket".to_string(),
            Json::string(group.hour_bucket.clone()),
        ),
        ("quantity".to_string(), Json::Int(group.quantity)),
        (
            "meteringPending".to_string(),
            Json::string(group.hour_bucket.clone()),
        ),
    ];
    let allocations = finalize_allocations(group);
    if !allocations.is_empty() {
        members.push(("usageAllocations".to_string(), Json::Array(allocations)));
    }
    if !group.customer_identifier.is_empty() {
        members.push((
            "customerIdentifier".to_string(),
            Json::string(group.customer_identifier.clone()),
        ));
    }
    let request = Json::object(vec![
        ("TableName", Json::string(table)),
        ("Item", audit_item(members, ctx.now)),
        (
            "ConditionExpression",
            Json::string("attribute_not_exists(licenseArn)"),
        ),
    ]);
    match ctx.client.call(Service::DynamoDb, "PutItem", &request) {
        Ok(_) => Ok(true),
        Err(error)
            if super::base::service_code(&error) == Some("ConditionalCheckFailedException") =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// Rejects a group client-side: terminal status and reason on every raw row,
/// `meteringPending` cleared, a warning and the `UsageRecordRejected` metric.
fn reject(ctx: &mut Ctx<'_>, group: &Group, reason: &str) -> Result<(), Failure> {
    ctx.log.warning(&format!(
        "Rejecting usage group client-side (license={} dim={} hour={}): {}",
        shown(&mask(&group.license_arn)),
        shown(&group.dimension),
        shown(&group.hour_bucket),
        shown(reason)
    ));
    emf_metric(ctx, "UsageRecordRejected", 1, Some(reason), None);
    update_rows_bounded(
        ctx,
        &group.license_arn,
        &group.row_keys,
        "REMOVE meteringPending SET meteringStatus = :st, meteringStatusReason = :rsn",
        &[
            (":st".to_string(), Json::string(STATUS_REJECTED)),
            (":rsn".to_string(), Json::string(reason)),
        ],
    )
}

/// Cleanup messages of at most 100 raw row keys each, carrying the
/// authoritative `Aggregated` status with the sum and count.
fn enqueue_cleanup(ctx: &mut Ctx<'_>, group: &Group) -> Result<(), Failure> {
    let queue = ctx.config.text("CLEANUP_QUEUE_URL").to_string();
    let mut bodies = Vec::new();
    for chunk in group.row_keys.chunks(CLEANUP_KEYS_PER_MESSAGE) {
        bodies.push(Json::object(vec![
            ("licenseArn", Json::string(group.license_arn.clone())),
            (
                "rowKeys",
                Json::Array(chunk.iter().cloned().map(Json::string).collect()),
            ),
            ("meteringStatus", Json::string("Aggregated")),
            ("totalQuantity", Json::Int(group.quantity)),
            (
                "recordCount",
                Json::Int(i64::try_from(group.row_keys.len()).unwrap_or(i64::MAX)),
            ),
        ]));
    }
    send_batches(ctx, &queue, &bodies, "cleanup ")
}

// ---- cleanup ------------------------------------------------------------------

/// `cleanup.handler`: clears `meteringPending` on the raw rows of a message.
pub(super) fn cleanup(ctx: &mut Ctx<'_>, event: &Json) -> Handled {
    ctx.require_core()?;
    let mut failures = Vec::new();
    for record in sqs_records(event)? {
        let outcome = message_body(record).and_then(|body| cleanup_message(ctx, &body));
        if let Err(failure) = outcome {
            ctx.log
                .error(&format!("Cleanup failed for a message: {failure}"));
            failures.push(message_id(record)?);
        }
    }
    Ok(batch_failures(failures))
}

fn cleanup_message(ctx: &mut Ctx<'_>, message: &Json) -> Result<(), Failure> {
    let license_arn = required_text(message, "licenseArn")?;
    let row_keys = optional_list(message, "rowKeys").map_err(invalid)?;
    let mut keys = Vec::new();
    for key in row_keys {
        let Some(text) = key.as_str() else {
            if crate::dynamodb::pyfmt::truthy(key) {
                return Err(Failure::message("rowKeys must hold strings"));
            }
            continue;
        };
        if !text.is_empty() {
            keys.push(text.to_string());
        }
    }
    if keys.is_empty() {
        return Ok(());
    }
    let masked = shown(&mask(&license_arn));
    if message.get("meteringStatus").and_then(Json::as_str) == Some("Aggregated") {
        let total = message
            .get("totalQuantity")
            .map_or_else(|| "0".to_string(), text);
        let count = message
            .get("recordCount")
            .map_or_else(|| keys.len().to_string(), text);
        let reason = format!("Aggregated total quantity {total} from {count} raw usage records");
        update_rows_bounded(
            ctx,
            &license_arn,
            &keys,
            "REMOVE meteringPending SET meteringStatus = :st, meteringStatusReason = :rsn",
            &[
                (":st".to_string(), Json::string("Aggregated")),
                (":rsn".to_string(), Json::string(reason.clone())),
            ],
        )?;
        ctx.log.info(&format!(
            "Finalized {} raw row(s) as Aggregated ({}) for license={masked}",
            keys.len(),
            shown(&reason)
        ));
    } else {
        update_rows_bounded(ctx, &license_arn, &keys, "REMOVE meteringPending", &[])?;
        ctx.log.info(&format!(
            "Cleared meteringPending on {} raw row(s) (no status) for license={masked}",
            keys.len()
        ));
    }
    Ok(())
}

// ---- expiry -------------------------------------------------------------------

/// `expiry.handler`: terminally expires pending aggregated records that can no
/// longer be metered.
pub(super) fn expiry(ctx: &mut Ctx<'_>, _event: &Json) -> Handled {
    ctx.require_core()?;
    ctx.aggregated_table()?;
    let now = ctx.now;
    let base = floor_hour(now);
    let grace_open = previous_month_grace_open(now);
    let mut aged = 0_i64;
    if !grace_open {
        for hours_back in 1..24 {
            let bucket = hour_bucket(add_hours(base, -hours_back));
            if is_previous_month_bucket(&bucket, now) {
                aged += expire_aggregated_bucket(ctx, &bucket)?;
            }
        }
    }
    let mut hours_back = 24_i64;
    let mut empty_streak = 0_u32;
    while hours_back <= MAX_HOURS_BACK && empty_streak < AGEOUT_EMPTY_STREAK_STOP {
        let bucket = hour_bucket(add_hours(base, -hours_back));
        hours_back += 1;
        if is_previous_month_bucket(&bucket, now) && grace_open {
            continue;
        }
        let found = expire_aggregated_bucket(ctx, &bucket)?;
        aged += found;
        empty_streak = if found > 0 { 0 } else { empty_streak + 1 };
    }
    if aged > 0 {
        ctx.log.error(&format!(
            "Expired {aged} aggregated record(s) past the submittable window"
        ));
    } else {
        ctx.log.info("No aggregated records to expire");
    }
    Ok(Json::object(vec![("expired", integer(aged))]))
}

fn expire_aggregated_bucket(ctx: &mut Ctx<'_>, bucket: &str) -> Result<i64, Failure> {
    let table = ctx.aggregated_table()?.to_string();
    let query = Query::new(
        &table,
        Some("metering_pending"),
        "meteringPending = :hb",
        vec![(":hb", Json::string(bucket))],
    );
    // Collect first: the walk holds the client, the update needs it too.
    let items = ddb::query_all(ctx.client, &query)?;
    let mut expired = 0_i64;
    for item in items {
        let license_arn = required_text(&item, "licenseArn")?;
        let sort_key = required_text(&item, "account_dimension_hour")?;
        emf_metric(ctx, "UsageSubmissionExpired", 1, None, None);
        let (expression, values) = core::with_audit_timestamps(
            "REMOVE meteringPending SET meteringStatus = :st",
            vec![(":st".to_string(), Json::string("SubmissionExpired"))],
            ctx.now,
        );
        let update = Update {
            table: &table,
            key: vec![
                ("licenseArn", Json::string(license_arn.clone())),
                ("account_dimension_hour", Json::string(sort_key)),
            ],
            expression: &expression,
            condition: None,
            values: Some(values),
            names: None,
        };
        ddb::update_item(ctx.client, &update)?;
        ctx.log.warning(&format!(
            "AGGREGATED EXPIRY: unbillable pending aggregated record license={} hour={}",
            shown(&mask(&license_arn)),
            shown(bucket)
        ));
        expired += 1;
    }
    Ok(expired)
}
