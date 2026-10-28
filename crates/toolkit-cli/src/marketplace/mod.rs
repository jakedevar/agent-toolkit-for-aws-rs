//! AWS Marketplace metering, subscription and cleanup helpers (#4396, slice
//! `marketplace`).
//!
//! | Command | Replaces |
//! | --- | --- |
//! | `toolkit marketplace handler NAME` | the Lambda handlers `discoverer.py`, `aggregator.py`, `cleanup.py`, `submitter.py`, `expiry.py` (with `metering_core.py`), `subscription.py`, `deprovision_cleanup.py`, `register.py` and `test_event_publisher.py` |
//! | `toolkit marketplace query-metering` | `query_metering.py` |
//! | `toolkit marketplace test-events` | the command line of `test_event_publisher.py` |
//!
//! No command opens a socket, reads a credential, runs Python or calls AWS
//! Marketplace. Every AWS interaction (DynamoDB, SQS, `BatchMeterUsage`,
//! `ResolveCustomer`, EventBridge, CloudTrail) is a request handed to an
//! [`client::AwsClient`] with a recorded implementation (`--recorded FILE`) and
//! a "no live client" implementation that sends nothing. A live binding is a
//! follow-up. Parity evidence and the intentional changes are in
//! `docs/migration/marketplace.md`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::too_many_lines
)]

use std::path::Path;

use toolkit_aws::{is_valid_region, redact_text};
use toolkit_core::{parse_json, Json, ToolkitError};

use crate::ai_data::parse_failure_text;
use crate::aurora::args::usage;
use crate::cloudwatch::timefmt::{parse_iso_timestamp, UtcTime};
use crate::safe_text::safe_path;
use crate::startup::read_text;
use crate::{CommandSpec, Invocation, Output};

mod base;
mod client;
mod clock;
mod core;
mod ddb;
mod events;
mod pipeline;
mod query;
mod submitter;
mod subscribers;
mod web;

#[cfg(test)]
mod tests;

use base::{shown, Config, Ctx, Failure, Logger};
use client::{AwsClient, NoLiveClient, RecordedClient};

/// `marketplace handler`.
pub const HANDLER: CommandSpec = CommandSpec {
    path: &["marketplace", "handler"],
    flags: &[],
    value_flags: &["--event", "--config", "--recorded", "--now", "--seed"],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 1,
    passthrough: false,
    handler: handler_command,
};

/// `marketplace query-metering`.
pub const QUERY_METERING: CommandSpec = CommandSpec {
    path: &["marketplace", "query-metering"],
    flags: &[],
    value_flags: &[
        "--region",
        "--days",
        "--start-date",
        "--end-date",
        "--customer-id",
        "--status",
        "--dimension",
        "--metering-record-id",
        "--product-code",
        "--deployment-product-code",
        "--query-type",
        "--top-n",
        "--limit",
        "--max-pages",
        "--recorded",
        "--now",
    ],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 0,
    passthrough: false,
    handler: query_metering_command,
};

/// `marketplace test-events`.
pub const TEST_EVENTS: CommandSpec = CommandSpec {
    path: &["marketplace", "test-events"],
    flags: &["--dry-print"],
    value_flags: &[
        "--scenario",
        "--count",
        "--product-code",
        "--test-accounts",
        "--region",
        "--event-bus-name",
        "--event-source",
        "--recorded",
        "--now",
        "--seed",
    ],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 0,
    passthrough: false,
    handler: test_events_command,
};

/// Registration of the `marketplace` slice: its routes and help text (see
/// `crate::registry`). Added after the registry, so both ranks are
/// [`crate::registry::NEW_RANK`].
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "marketplace",
    commands: &[HANDLER, QUERY_METERING, TEST_EVENTS],
    synopsis: r"  toolkit marketplace handler NAME [--event FILE] [--config FILE] [--recorded FILE]
      [--now TS] [--seed N]
  toolkit marketplace query-metering --region R [--days N] [--start-date D] [--end-date D]
      [--customer-id ID] [--status S] [--dimension D] [--metering-record-id ID]
      [--product-code C] [--deployment-product-code C] [--query-type TYPE]
      [--top-n N] [--limit N] [--max-pages N] [--recorded FILE] [--now TS]
  toolkit marketplace test-events --product-code C --test-accounts A,B --event-source SRC
      [--scenario S] [--count N] [--region R] [--event-bus-name B] [--dry-print]
      [--recorded FILE] [--now TS] [--seed N]
",
    description: r"  marketplace   AWS Marketplace metering helpers (#4396). handler runs one metering
                Lambda (discoverer, aggregator, cleanup, submitter, expiry, subscription,
                deprovision-cleanup, register, test-event-publisher) over an --event, a
                --config of its environment and the --recorded AWS responses;
                query-metering summarizes CloudTrail metering records; test-events
                builds (and with --recorded replays the publication of) simulated
                subscription events. This build has no AWS client and never calls
                BatchMeterUsage. See docs/migration/marketplace.md.
",
    exit_notes: "",
    doc: "docs/migration/marketplace.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// A caller path as the slice shows it: redacted, then control, bidirectional
/// and invisible characters escaped. A path is never shortened.
fn shown_path(path: &str) -> String {
    safe_path(&redact_text(path))
}

/// Reads and parses a JSON file named by the caller. The error names the file
/// as [`shown_path`] does and the failure, never a value of the document.
fn read_document(path: &str) -> Result<Json, String> {
    let label = shown_path(path);
    let text = read_text(Path::new(path), &label)?;
    parse_json(&text)
        .map_err(|error| format!("{label}: invalid JSON ({})", parse_failure_text(&error)))
}

fn system_now() -> UtcTime {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    UtcTime::from_epoch_seconds(seconds)
        .or_else(|| UtcTime::from_epoch_micros(0))
        .expect("the epoch is a valid time")
}

fn now_from(invocation: &Invocation) -> Result<UtcTime, ToolkitError> {
    match invocation.value("--now") {
        None => Ok(system_now()),
        Some(text) => parse_iso_timestamp(text)
            .map_err(|_| usage("--now must be an ISO timestamp such as 2026-01-31T12:00:00Z")),
    }
}

fn seed_from(invocation: &Invocation, now: UtcTime) -> Result<u64, ToolkitError> {
    match invocation.value("--seed") {
        None => Ok(u64::try_from(now.epoch_micros()).unwrap_or(0) ^ u64::from(std::process::id())),
        Some(text) => text
            .parse::<u64>()
            .map_err(|_| usage("--seed must be a non-negative integer")),
    }
}

/// The recorded client of `--recorded FILE`, if given.
fn recorded_client(invocation: &Invocation) -> Result<Option<RecordedClient>, ToolkitError> {
    let Some(path) = invocation.value("--recorded") else {
        return Ok(None);
    };
    let document = read_document(path)
        .map_err(|reason| ToolkitError::runtime("marketplace_recorded", reason))?;
    RecordedClient::from_document(&document)
        .map(Some)
        .map_err(|reason| {
            ToolkitError::runtime(
                "marketplace_recorded",
                format!("{}: {reason}", shown_path(path)),
            )
        })
}

fn unused_steps(client: Option<&RecordedClient>) -> Result<(), ToolkitError> {
    match client.map(RecordedClient::unconsumed) {
        Some(unused) if unused > 0 => Err(ToolkitError::runtime(
            "marketplace_recorded",
            format!("{unused} recorded call(s) were not used"),
        )),
        _ => Ok(()),
    }
}

fn emit_log(out: &mut Output<'_>, log: &Logger) {
    for line in log.lines() {
        out.diagnostic(line);
    }
}

fn integer_option(invocation: &Invocation, flag: &str, default: i64) -> Result<i64, ToolkitError> {
    match invocation.value(flag) {
        None => Ok(default),
        Some(text) => base::py_int(text).ok_or_else(|| usage(format!("{flag} must be an integer"))),
    }
}

fn required_flags(invocation: &Invocation, flags: &[&str]) -> Result<(), ToolkitError> {
    let missing: Vec<&str> = flags
        .iter()
        .copied()
        .filter(|flag| invocation.value(flag).is_none())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(usage(format!(
            "the following arguments are required: {}",
            missing.join(", ")
        )))
    }
}

const HANDLERS: [&str; 9] = [
    "discoverer",
    "aggregator",
    "cleanup",
    "submitter",
    "expiry",
    "subscription",
    "deprovision-cleanup",
    "register",
    "test-event-publisher",
];

/// Runs one handler against the context.
fn dispatch(name: &str, ctx: &mut Ctx<'_>, event: &Json, seed: u64) -> pipeline::Handled {
    match name {
        "discoverer" => pipeline::discoverer(ctx, event),
        "aggregator" => pipeline::aggregator(ctx, event),
        "cleanup" => pipeline::cleanup(ctx, event),
        "submitter" => submitter::submitter(ctx, event),
        "expiry" => pipeline::expiry(ctx, event),
        "subscription" => subscribers::subscription(ctx, event),
        "deprovision-cleanup" => subscribers::deprovision_cleanup(ctx, event),
        "register" => subscribers::register(ctx, event),
        _ => events::test_event_publisher(ctx, event, seed),
    }
}

/// Handler of `toolkit marketplace handler`.
pub fn handler_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let Some(name) = invocation.positional(0) else {
        return Err(usage(format!(
            "a handler name is required: {}",
            HANDLERS.join(", ")
        )));
    };
    if !HANDLERS.contains(&name) {
        return Err(usage(format!(
            "the handler must be one of: {}",
            HANDLERS.join(", ")
        )));
    }
    let config = match invocation.value("--config") {
        None => Config::default(),
        Some(path) => {
            let document = read_document(path)
                .map_err(|reason| ToolkitError::runtime("marketplace_config", reason))?;
            Config::from_document(&document).map_err(|reason| {
                ToolkitError::runtime(
                    "marketplace_config",
                    format!("{}: {reason}", shown_path(path)),
                )
            })?
        }
    };
    let event = match invocation.value("--event") {
        None => Json::Object(Vec::new()),
        Some(path) => read_document(path)
            .map_err(|reason| ToolkitError::runtime("marketplace_event", reason))?,
    };
    let now = now_from(invocation)?;
    let seed = seed_from(invocation, now)?;
    let mut recorded = recorded_client(invocation)?;
    if recorded.is_none() {
        out.diagnostic(
            "note: this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        );
    }
    let mut no_live = NoLiveClient;
    let client: &mut dyn AwsClient = match recorded.as_mut() {
        Some(recorded) => recorded,
        None => &mut no_live,
    };
    let mut log = Logger::default();
    let mut ctx = Ctx {
        client,
        config: &config,
        log: &mut log,
        now,
    };
    let result = dispatch(name, &mut ctx, &event, seed);
    emit_log(out, &log);
    match result {
        Ok(value) => {
            out.line(&events::dumps_indented_ascii(&value))?;
            unused_steps(recorded.as_ref())
        }
        Err(failure) => Err(ToolkitError::runtime(
            "marketplace_handler",
            format!("{name} failed: {failure}"),
        )),
    }
}

const QUERY_TYPES: [&str; 5] = [
    "detail",
    "summary",
    "count",
    "list_failures",
    "top_customers",
];

fn text_option(invocation: &Invocation, flag: &str) -> Option<String> {
    invocation.value(flag).map(str::to_string)
}

/// Handler of `toolkit marketplace query-metering`.
pub fn query_metering_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    required_flags(invocation, &["--region"])?;
    let region = invocation.value("--region").unwrap_or_default();
    if !is_valid_region(region) {
        return Err(usage("--region must look like us-east-1"));
    }
    let query_type = invocation.value("--query-type").unwrap_or("summary");
    if !QUERY_TYPES.contains(&query_type) {
        return Err(usage(format!(
            "--query-type must be one of: {}",
            QUERY_TYPES.join(", ")
        )));
    }
    let args = query::QueryArgs {
        region: region.to_string(),
        days: integer_option(invocation, "--days", 7)?,
        start_date: text_option(invocation, "--start-date"),
        end_date: text_option(invocation, "--end-date"),
        customer_id: text_option(invocation, "--customer-id"),
        status: text_option(invocation, "--status"),
        dimension: text_option(invocation, "--dimension"),
        metering_record_id: text_option(invocation, "--metering-record-id"),
        product_code: text_option(invocation, "--product-code"),
        deployment_product_code: text_option(invocation, "--deployment-product-code"),
        query_type: query_type.to_string(),
        top_n: integer_option(invocation, "--top-n", 10)?,
        limit: integer_option(invocation, "--limit", 50)?,
        max_pages: integer_option(invocation, "--max-pages", 20)?,
    };
    let now = now_from(invocation)?;
    let mut recorded = recorded_client(invocation)?;
    if recorded.is_none() {
        out.diagnostic(
            "note: this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        );
    }
    let mut no_live = NoLiveClient;
    let client: &mut dyn AwsClient = match recorded.as_mut() {
        Some(recorded) => recorded,
        None => &mut no_live,
    };
    run_query(&args, now, client, out)?;
    unused_steps(recorded.as_ref())
}

fn query_failure(failure: &Failure) -> ToolkitError {
    ToolkitError::runtime("marketplace_query", failure.to_string())
}

fn run_query(
    args: &query::QueryArgs,
    now: UtcTime,
    client: &mut dyn AwsClient,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let fail = |message: String| ToolkitError::runtime("marketplace_query", message);
    let day_micros = 86_400 * 1_000_000_i64;
    let start_requested = match args.start_date.as_deref().filter(|date| !date.is_empty()) {
        Some(date) => clock::parse_date(date)
            .ok_or_else(|| fail("--start-date must be YYYY-MM-DD".to_string()))?,
        None => args
            .days
            .checked_mul(day_micros)
            .and_then(|delta| now.checked_add_micros(-delta))
            .ok_or_else(|| fail("--days is out of range".to_string()))?,
    };
    let end = match args.end_date.as_deref().filter(|date| !date.is_empty()) {
        Some(date) => clock::parse_date(date)
            .and_then(|midnight| midnight.checked_add_micros(day_micros - 1_000_000))
            .ok_or_else(|| fail("--end-date must be YYYY-MM-DD".to_string()))?,
        None => now,
    };
    if start_requested > end {
        return Err(fail(format!(
            "Error: start_time ({}) is after end_time ({}). Provide --start-date when using a past --end-date.",
            query::date_text(start_requested),
            query::date_text(end)
        )));
    }
    let boundary = query::ninety_days_ago(now);
    if end < boundary {
        out.line(&events::dumps_indented_ascii(&query::out_of_scope(end)))?;
        return Ok(());
    }
    let mut start = start_requested;
    let mut clipped = false;
    if start < boundary {
        clipped = true;
        out.diagnostic(
            "NOTE: CloudTrail Event History retains only 90 days; the query covers the in-window portion of the requested range (from 90 days ago). For the older portion, use Seller Reports / SDDS (see references/seller-reports.md).",
        );
        start = boundary;
    }
    if start > end {
        return Err(fail(format!(
            "Error: CloudTrail Event History retention limit reached - after capping the start date to 90 days ago ({}) it is later than the requested end date ({}). Use a more recent --end-date, or use Seller Reports / SDDS for data older than 90 days (see references/seller-reports.md).",
            query::date_text(start),
            query::date_text(end)
        )));
    }
    out.diagnostic("Querying CloudTrail Event History");
    out.diagnostic(&format!("  Region: {}", args.region));
    out.diagnostic(&format!(
        "  Range: {} to {}",
        query::minute_text(start),
        query::minute_text(end)
    ));

    let (events, stats) = query::lookup_events(
        client,
        start.epoch_seconds(),
        end.epoch_seconds(),
        args.max_pages,
    )
    .map_err(|failure| query_failure(&failure))?;
    let mut records = Vec::new();
    for (time, event) in &events {
        records.extend(
            query::extract_records(time, event).map_err(|failure| query_failure(&failure))?,
        );
    }
    out.diagnostic(&format!(
        "Processed {} CloudTrail events \u{2192} {} metering records",
        events.len(),
        records.len()
    ));
    if stats.truncated {
        out.diagnostic(&format!(
            "WARNING: hit the --max-pages cap ({} pages) with more events available; this result is a PARTIAL sample of the window. Narrow the range / add filters (--customer-id/--dimension/--status) or raise --max-pages for a complete answer, or use Seller Reports for an authoritative total.",
            args.max_pages
        ));
    }
    if args
        .product_code
        .as_deref()
        .is_some_and(|code| !code.is_empty())
    {
        let mapped =
            query::apply_deployment_product(&mut records, args.deployment_product_code.as_deref());
        if !mapped && !records.is_empty() {
            return out.line(&events::dumps_indented_ascii(&query::no_mapping_refusal()));
        }
    }
    let filtered: Vec<_> = records
        .iter()
        .filter(|record| query::matches_filters(record, args))
        .cloned()
        .collect();
    out.diagnostic(&format!("After filters: {} records", filtered.len()));
    let output = query::aggregate(&filtered, &args.query_type, args.top_n, args.limit);
    let window = query::Window {
        start,
        end,
        clipped,
        requested_start: start_requested,
    };
    let output = query::completeness(&output, args, &window, &stats, events.len(), records.len());
    out.line(&events::dumps_indented_ascii(&output))
}

/// Handler of `toolkit marketplace test-events`.
pub fn test_events_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    required_flags(
        invocation,
        &["--product-code", "--test-accounts", "--event-source"],
    )?;
    let scenario = invocation.value("--scenario").unwrap_or("new");
    if !events::SCENARIOS.contains(&scenario) {
        return Err(usage(format!(
            "--scenario must be one of: {}",
            events::SCENARIOS.join(", ")
        )));
    }
    let count = integer_option(invocation, "--count", 1)?;
    let product_code = invocation.value("--product-code").unwrap_or_default();
    let source = invocation.value("--event-source").unwrap_or_default();
    events::require_event_source(source).map_err(usage)?;
    let accounts: Vec<String> = invocation
        .value("--test-accounts")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|account| !account.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(region) = invocation.value("--region") {
        if !is_valid_region(region) {
            return Err(usage("--region must look like us-east-1"));
        }
    }
    let bus = invocation.value("--event-bus-name");
    if let Some(bus) = bus {
        let valid = !bus.is_empty()
            && bus.len() <= 256
            && bus
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'));
        if !valid {
            return Err(usage(
                "--event-bus-name must hold letters, digits and / . - _ (256 characters at most)",
            ));
        }
    }
    let now = now_from(invocation)?;
    let mut rng = events::Rng::new(seed_from(invocation, now)?);
    let batch = events::build_batch(
        scenario,
        &accounts,
        product_code,
        count,
        now,
        source,
        &mut rng,
    )
    .map_err(|reason| ToolkitError::runtime("marketplace_events", reason))?;
    if invocation.has_flag("--dry-print") {
        return out.line(&events::dumps_indented_ascii(&Json::Array(batch)));
    }
    let mut recorded = recorded_client(invocation)?;
    if recorded.is_none() {
        out.diagnostic(
            "note: this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        );
    }
    let mut no_live = NoLiveClient;
    let client: &mut dyn AwsClient = match recorded.as_mut() {
        Some(recorded) => recorded,
        None => &mut no_live,
    };
    let config = Config::default();
    let mut log = Logger::default();
    let mut ctx = Ctx {
        client,
        config: &config,
        log: &mut log,
        now,
    };
    let sent = events::publish(&mut ctx, &batch, bus).map_err(|failure| {
        ToolkitError::runtime(
            "marketplace_events",
            format!("publishing failed: {}", shown(&failure.to_string())),
        )
    })?;
    out.line(&format!("Published {sent} '{scenario}' test event(s)."))?;
    unused_steps(recorded.as_ref())
}
