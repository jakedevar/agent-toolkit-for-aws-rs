//! `submitter.py`: reads the pending aggregated records, shapes the
//! `BatchMeterUsage` batches, submits them (or only logs them in `dry-run`)
//! and writes the outcome back to `aggregated_usage`.

use toolkit_core::Json;

use super::base::{shown, Ctx, Failure};
use super::clock::{add_hours, epoch, floor_hour, hour_bucket, parse_hour_bucket};
use super::core::{
    self, deprovisioning_licenses, emf_metric, in_quantity_domain, mask, quantity_reason,
    strict_int, submit_records, AggKey, Outcome, Outcomes, BATCH_SIZE,
};
use super::ddb::{self, query_each, required_text, Flow, Query, Update};
use super::pipeline::Handled;
use crate::dynamodb::pyfmt::text;

const STATUS_DRY_RUN: &str = "DryRunSubmitted";

/// A pending aggregated record by its key, in read order.
#[derive(Default)]
struct Pending {
    entries: Vec<(AggKey, Json)>,
}

impl Pending {
    fn contains(&self, key: &AggKey) -> bool {
        self.entries.iter().any(|(known, _)| known == key)
    }

    fn set(&mut self, key: AggKey, item: Json) {
        match self.entries.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = item,
            None => self.entries.push((key, item)),
        }
    }

    fn get(&self, key: &AggKey) -> Option<&Json> {
        self.entries
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, item)| item)
    }
}

fn item_key(item: &Json) -> Result<AggKey, Failure> {
    Ok((
        required_text(item, "licenseArn")?,
        required_text(item, "account_dimension_hour")?,
    ))
}

/// `submitter.handler`.
pub(super) fn submitter(ctx: &mut Ctx<'_>, _event: &Json) -> Handled {
    ctx.require_core()?;
    ctx.aggregated_table()?;
    let dry_run = ctx.metering_mode() == "dry-run";

    // Deprovisioning-first is best effort: a failed lookup must not block the
    // ordinary backlog.
    let deprovisioning: Vec<String> = match deprovisioning_licenses(ctx) {
        Ok(found) => found.into_iter().map(|(arn, _)| arn).collect(),
        Err(failure) => {
            ctx.log.warning(&format!(
                "deprovisioning_licenses() lookup failed ({failure}); submitting ordinary backlog only \
                 (deprovisioning records still submit via the regular pass)"
            ));
            Vec::new()
        }
    };

    let mut pending = Pending::default();
    if !deprovisioning.is_empty() {
        read_deprovisioning_pending(ctx, &deprovisioning, &mut pending)?;
    }
    read_pending_aggregated(ctx, &mut pending)?;
    if pending.entries.is_empty() {
        ctx.log.info("No pending aggregated usage to submit");
        return Ok(Json::object(vec![("submitted", Json::Int(0))]));
    }

    let (ca_batches, legacy_batches) = build_batches(&pending, &deprovisioning)?;
    let mut outcomes = Outcomes::default();
    let mut unprocessed_remaining = false;
    for (records, keys) in &ca_batches {
        unprocessed_remaining |= send(ctx, dry_run, records, keys, &mut outcomes, false)?;
    }
    for (records, keys) in &legacy_batches {
        unprocessed_remaining |= send(ctx, dry_run, records, keys, &mut outcomes, true)?;
    }

    for (key, outcome) in outcomes.entries() {
        let item = pending
            .get(key)
            .ok_or_else(|| Failure::message("an outcome has no pending record"))?;
        write_back(ctx, item, outcome)?;
    }

    let licenses: Vec<&str> = {
        let mut seen: Vec<&str> = Vec::new();
        for ((license, _), _) in outcomes.entries() {
            if !seen.contains(&license.as_str()) {
                seen.push(license);
            }
        }
        seen
    };
    ctx.log.info(&format!(
        "Submitted {} aggregated record(s) across {} license(s)",
        outcomes.entries().len(),
        licenses.len()
    ));
    if unprocessed_remaining {
        return Err(Failure::message(
            "One or more UsageRecords remained unprocessed after retry; their aggregated_usage \
             meteringPending markers were kept for re-submission.",
        ));
    }
    Ok(Json::object(vec![(
        "submitted",
        Json::Int(i64::try_from(outcomes.entries().len()).unwrap_or(i64::MAX)),
    )]))
}

/// In `dry-run` the whole pipeline runs but `BatchMeterUsage` is never
/// called: every record is logged (masked), counted and stamped
/// `DryRunSubmitted`.
fn send(
    ctx: &mut Ctx<'_>,
    dry_run: bool,
    records: &[Json],
    keys: &[AggKey],
    outcomes: &mut Outcomes,
    use_product_code: bool,
) -> Result<bool, Failure> {
    if !dry_run {
        return submit_records(ctx, records, keys, outcomes, use_product_code);
    }
    for (record, key) in records.iter().zip(keys) {
        outcomes.set(
            key.clone(),
            Outcome {
                metering_record_id: None,
                status: STATUS_DRY_RUN.to_string(),
                reason: None,
            },
        );
        ctx.log.info(&format!(
            "DRY-RUN (MeteringMode=dry-run) - would submit UsageRecord: license={} account={} \
             dimension={} quantity={} (use_product_code={}); NOT calling BatchMeterUsage",
            shown(&mask(&ddb::text_or_empty(record, "LicenseArn")?)),
            shown(&mask(&ddb::text_or_empty(record, "CustomerAWSAccountId")?)),
            shown(&ddb::text_or_empty(record, "Dimension")?),
            record
                .get("Quantity")
                .map_or_else(|| "None".to_string(), text),
            if use_product_code { "True" } else { "False" }
        ));
    }
    emf_metric(
        ctx,
        "DryRunSubmitted",
        i64::try_from(records.len()).unwrap_or(i64::MAX),
        None,
        None,
    );
    Ok(false)
}

/// The pending aggregated records of the deprovisioning set over now-23h ..
/// now (the in-progress hour too), all of them, not subject to the run cap.
fn read_deprovisioning_pending(
    ctx: &mut Ctx<'_>,
    deprovisioning: &[String],
    pending: &mut Pending,
) -> Result<(), Failure> {
    let table = ctx.aggregated_table()?.to_string();
    let base = floor_hour(ctx.now);
    for hours_back in (0..=23).rev() {
        let bucket = hour_bucket(add_hours(base, -hours_back));
        for license in deprovisioning {
            let query = Query::new(
                &table,
                Some("metering_pending"),
                "meteringPending = :hb AND licenseArn = :la",
                vec![
                    (":hb", Json::string(bucket.clone())),
                    (":la", Json::string(license.clone())),
                ],
            );
            query_each(ctx.client, &query, |item| {
                pending.set(item_key(item)?, item.clone());
                Ok(Flow::Continue)
            })?;
        }
    }
    Ok(())
}

/// The pending aggregated records of the submittable window now-23h ..
/// now-1h, oldest first, up to `MAX_RECORDS_PER_RUN` in total.
fn read_pending_aggregated(ctx: &mut Ctx<'_>, pending: &mut Pending) -> Result<(), Failure> {
    let table = ctx.aggregated_table()?.to_string();
    let cap = ctx.config.integer("MAX_RECORDS_PER_RUN", 50_000)?;
    let cap = usize::try_from(cap).unwrap_or(0);
    let base = floor_hour(ctx.now);
    for hours_back in (1..=23).rev() {
        let bucket = hour_bucket(add_hours(base, -hours_back));
        let query = Query::new(
            &table,
            Some("metering_pending"),
            "meteringPending = :hb",
            vec![(":hb", Json::string(bucket))],
        );
        let mut full = false;
        query_each(ctx.client, &query, |item| {
            let key = item_key(item)?;
            if !pending.contains(&key) {
                pending.set(key, item.clone());
            }
            if pending.entries.len() >= cap {
                full = true;
                return Ok(Flow::Stop);
            }
            Ok(Flow::Continue)
        })?;
        if full {
            return Ok(());
        }
    }
    Ok(())
}

type Batch = (Vec<Json>, Vec<AggKey>);

/// `_build_batches`: the API records, deprovisioning licenses first and the
/// read order kept within each tier, split into Concurrent Agreements
/// (`LicenseArn`) and legacy (`ProductCode`) batches of at most 25.
fn build_batches(
    pending: &Pending,
    deprovisioning: &[String],
) -> Result<(Vec<Batch>, Vec<Batch>), Failure> {
    let mut ordered: Vec<&(AggKey, Json)> = pending.entries.iter().collect();
    ordered.sort_by_key(|((license, _), _)| u8::from(!deprovisioning.contains(license)));
    let mut ca: Batch = (Vec::new(), Vec::new());
    let mut legacy: Batch = (Vec::new(), Vec::new());
    for (key, item) in ordered {
        let license_arn = required_text(item, "licenseArn")?;
        let account_id = ddb::text_or_empty(item, "customerAWSAccountId")?;
        let dimension = required_text(item, "dimension")?;
        let quantity = stored_quantity(item.get("quantity"))?;
        let bucket = required_text(item, "hourBucket")?;
        let hour_ts = parse_hour_bucket(&bucket)
            .ok_or_else(|| Failure::message("hourBucket must look like 2026-01-31T12"))?;
        let mut record = vec![
            ("Timestamp", Json::Int(epoch(hour_ts))),
            ("Dimension", Json::string(dimension)),
            ("Quantity", Json::Int(quantity)),
        ];
        if let Some(allocations) = item
            .get("usageAllocations")
            .filter(|v| crate::dynamodb::pyfmt::truthy(v))
        {
            record.push(("UsageAllocations", api_allocations(allocations)?));
        }
        if license_arn.starts_with("arn:") {
            record.push(("CustomerAWSAccountId", Json::string(account_id)));
            record.push(("LicenseArn", Json::string(license_arn)));
            ca.0.push(Json::object(record));
            ca.1.push(key.clone());
        } else {
            record.push((
                "CustomerIdentifier",
                Json::string(ddb::text_or_empty(item, "customerIdentifier")?),
            ));
            legacy.0.push(Json::object(record));
            legacy.1.push(key.clone());
        }
    }
    Ok((chunk(ca), chunk(legacy)))
}

/// `strict_int(value) or 0` for a stored quantity or allocation, held to the
/// metering domain `|q| <= MAX_QUANTITY` (#4991). A missing or non-integer
/// member is the script's `0`; an integer outside the domain, however it is
/// written (an integer beyond 64 bits, a float the reader rounded onto an `i64`
/// boundary, or simply larger than `2^53 - 1`), fails the run before anything is
/// built, submitted or finalized, because `0` would silently drop the usage and
/// a rounded value would bill the wrong one.
fn stored_quantity(value: Option<&Json>) -> Result<i64, Failure> {
    let Some(value) = value else {
        return Ok(0);
    };
    let out_of_range = || {
        Failure::message(
            "a stored usage quantity is outside the metering range of 2^53 - 1 \
             (QuantityOutOfRange); nothing was submitted and the pending markers were kept",
        )
    };
    match strict_int(value) {
        Some(quantity) if in_quantity_domain(quantity) => Ok(quantity),
        Some(_) => Err(out_of_range()),
        None if quantity_reason(value) == "QuantityOutOfRange" => Err(out_of_range()),
        None => Ok(0),
    }
}

fn chunk((records, keys): Batch) -> Vec<Batch> {
    records
        .chunks(BATCH_SIZE)
        .zip(keys.chunks(BATCH_SIZE))
        .map(|(records, keys)| (records.to_vec(), keys.to_vec()))
        .collect()
}

/// `_to_api_allocations`: the stored allocations as API integers and tags.
fn api_allocations(allocations: &Json) -> Result<Json, Failure> {
    let list = toolkit_aws::recorded::list_value(allocations)
        .ok_or_else(|| Failure::message("usageAllocations must be a list"))?;
    let mut out = Vec::with_capacity(list.len());
    for allocation in list {
        let quantity = stored_quantity(allocation.get("AllocatedUsageQuantity"))?;
        let mut tags = Vec::new();
        for tag in toolkit_aws::recorded::optional_list(allocation, "Tags")
            .map_err(|error| Failure::message(format!("invalid item: {error}")))?
        {
            tags.push(Json::object(vec![
                ("Key", Json::string(required_text(tag, "Key")?)),
                ("Value", Json::string(required_text(tag, "Value")?)),
            ]));
        }
        out.push(Json::object(vec![
            ("AllocatedUsageQuantity", Json::Int(quantity)),
            ("Tags", Json::Array(tags)),
        ]));
    }
    Ok(Json::Array(out))
}

/// `_write_back`: the metering record id, status (and reason) on the
/// aggregated record, `meteringPending` removed. A `ttl` is set on a terminal
/// `Success` row only when retention is configured.
fn write_back(ctx: &mut Ctx<'_>, item: &Json, outcome: &Outcome) -> Result<(), Failure> {
    let table = ctx.aggregated_table()?.to_string();
    let ttl_days = ctx.config.integer("AGGREGATED_USAGE_TTL_DAYS", 0)?;
    let mut expression = "REMOVE meteringPending SET meteringStatus = :st".to_string();
    let mut values = vec![(":st".to_string(), Json::string(outcome.status.clone()))];
    if let Some(id) = outcome
        .metering_record_id
        .as_ref()
        .filter(|id| !id.is_empty())
    {
        expression.push_str(", meteringRecordId = :mid");
        values.push((":mid".to_string(), Json::string(id.clone())));
    }
    if let Some(reason) = outcome.reason.as_ref().filter(|reason| !reason.is_empty()) {
        expression.push_str(", meteringStatusReason = :rsn");
        values.push((":rsn".to_string(), Json::string(reason.clone())));
    }
    let with_ttl = ttl_days > 0 && outcome.status == "Success";
    if with_ttl {
        expression.push_str(", #ttl = :ttl");
        let expires = epoch(ctx.now).saturating_add(ttl_days.saturating_mul(86_400));
        values.push((":ttl".to_string(), Json::Int(expires)));
    }
    let (expression, values) = core::with_audit_timestamps(&expression, values, ctx.now);
    let update = Update {
        table: &table,
        key: vec![
            (
                "licenseArn",
                Json::string(required_text(item, "licenseArn")?),
            ),
            (
                "account_dimension_hour",
                Json::string(required_text(item, "account_dimension_hour")?),
            ),
        ],
        expression: &expression,
        condition: None,
        values: Some(values),
        names: with_ttl.then(|| Json::object(vec![("#ttl", Json::string("ttl"))])),
    };
    ddb::update_item(ctx.client, &update)?;
    Ok(())
}
