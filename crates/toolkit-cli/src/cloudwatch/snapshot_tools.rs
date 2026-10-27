//! Operation entrypoints for CloudWatch snapshot search and sampling
//! (`di_snapshot_tools.py`), and the failure classification of the host
//! script (`di_snapshots._is_failure`).
//!
//! Both tools return text: either a bare `ERROR: ...` line for a
//! deterministic input failure, or the JSON the renderers produce, whose
//! inner `status` carries an AWS-side failure.

use toolkit_core::{parse_json, Json, ToolkitError};

use super::aws_client::AwsCalls;
use super::cli::Args;
use super::consts::resolve_snapshot_log_group;
use super::logs_query::{execute_cloudwatch_query, Pacer};
use super::pytext::{safe_text, strip};
use super::snapshot_parsing::escape_logs_insights_string;
use super::snapshot_rendering::{
    render_get_sample_snapshot_for_breakpoint_output,
    render_search_snapshots_for_status_event_output, SampleContext, SearchContext,
};
use super::timefmt::{parse_iso_timestamp, UtcTime};
use super::validation::is_valid_location_hash;
use crate::dynamodb::pyfmt;

const HASH_ERROR: &str = "ERROR: location_hash must be a 16-character hex string";
const TIMESTAMP_ERROR: &str =
    "ERROR: status_timestamp must be ISO 8601 format like \"2025-02-03T18:42:00Z\"";
const OUT_OF_RANGE: &str = "ERROR: the search window is outside the supported date range";

/// The inner `status` values that make a snapshot result a failure.
const QUERY_FAILURE_STATUSES: [&str; 5] =
    ["ERROR", "TIMEOUT", "POLLING TIMEOUT", "FAILED", "CANCELLED"];

/// `_is_failure`: a bare `ERROR...` line, or a JSON object whose `status`
/// is one of the AWS-side failure statuses.
#[must_use]
pub fn is_failure(result: &str) -> bool {
    if result.trim_start().starts_with("ERROR") {
        return true;
    }
    match parse_json(result) {
        Ok(data @ Json::Object(_)) => {
            let status = data.get("status").map_or_else(String::new, pyfmt::text);
            QUERY_FAILURE_STATUSES.contains(&strip(&status).to_uppercase().as_str())
        }
        _ => false,
    }
}

/// `_build_base_filters`: the resource-matching Logs Insights filter shared by
/// both tools. All three values are escaped for a double-quoted string
/// literal, so a caller-supplied quote cannot break out of it and inject query
/// syntax.
fn build_base_filters(location_hash: &str, service_name: &str, environment: &str) -> String {
    let location_hash = escape_logs_insights_string(location_hash);
    let service_name = escape_logs_insights_string(service_name);
    let environment = escape_logs_insights_string(environment);
    format!(
        "attributes.aws.di.location_hash = \"{location_hash}\"\
         \x20and (resource.attributes.service.name = \"{service_name}\"\
         \x20     or not ispresent(resource.attributes.service.name))\
         \x20and (resource.attributes.deployment.environment = \"{environment}\"\
         \x20     or resource.attributes.deployment.environment.name = \"{environment}\"\
         \x20     or not ispresent(resource.attributes.deployment.environment))"
    )
}

/// `int(value)` for the JSON values Python's `int()` accepts without error:
/// integers, booleans, finite floats (truncated), and decimal strings.
fn py_int(value: &Json) -> Option<i64> {
    match value {
        Json::Int(number) => Some(*number),
        Json::Bool(flag) => Some(i64::from(*flag)),
        #[allow(clippy::cast_possible_truncation)]
        Json::Float(number) if number.is_finite() && number.abs() < 9.0e18 => {
            Some(number.trunc() as i64)
        }
        Json::String(text) => {
            let text = strip(text);
            let (sign, digits) = match text.as_bytes().first() {
                Some(b'-') => (-1_i64, &text[1..]),
                Some(b'+') => (1, &text[1..]),
                _ => (1, text),
            };
            let well_formed = !digits.is_empty()
                && !digits.starts_with('_')
                && !digits.ends_with('_')
                && !digits.contains("__")
                && digits
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'_');
            if !well_formed {
                return None;
            }
            let digits: String = digits.chars().filter(|c| *c != '_').collect();
            digits.parse::<i64>().ok().map(|number| sign * number)
        }
        _ => None,
    }
}

/// `int(start_time_utc.timestamp())`: whole seconds, truncated toward zero.
fn epoch_seconds(time: UtcTime) -> i64 {
    time.epoch_micros() / 1_000_000
}

fn shift(time: UtcTime, seconds: i64) -> Option<UtcTime> {
    seconds
        .checked_mul(1_000_000)
        .and_then(|delta| time.checked_add_micros(delta))
}

/// The custom filter fragments: a bare string is a one-element list.
fn custom_filters(args: &Args) -> Result<Vec<String>, ToolkitError> {
    match args.present("custom_filters") {
        None => Ok(Vec::new()),
        Some(Json::String(text)) => Ok(vec![text.clone()]),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| match item {
                Json::String(text) => Ok(text.clone()),
                other => Err(args.mistyped("custom_filters", "an array of strings", other)),
            })
            .collect(),
        Some(other) => Err(args.mistyped("custom_filters", "an array of strings", other)),
    }
}

/// `search_snapshots_for_status_event`.
pub(super) fn search_snapshots(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    args: &Args,
) -> Result<String, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let location_hash = args.req_str("location_hash")?;
    let status_timestamp = args.req_str("status_timestamp")?;
    let max_timeout = args.opt_i64("max_timeout")?.unwrap_or(30);
    let filters = custom_filters(args)?;
    let start_time = args.opt_str("start_time")?;
    let end_time = args.opt_str("end_time")?;
    let limit = match args.explicit("limit") {
        None => Some(10),
        Some(value) => py_int(value),
    };

    if !is_valid_location_hash(Some(location_hash)) {
        return Ok(HASH_ERROR.to_string());
    }
    let Some(limit) = limit else {
        return Ok("ERROR: limit must be an integer".to_string());
    };
    let Ok(event_time) = parse_iso_timestamp(status_timestamp) else {
        return Ok(TIMESTAMP_ERROR.to_string());
    };

    // Both bounds or neither, so the window is never half-specified.
    let (start_time_dt, end_time_dt) = match (start_time, end_time) {
        (None, Some(_)) | (Some(_), None) => {
            return Ok("ERROR: start_time and end_time must be provided together \
                       (both ISO 8601), or both omitted to use the status_timestamp-anchored \
                       window"
                .to_string());
        }
        (Some(start), Some(end)) => {
            let (Ok(window_start), Ok(window_end)) =
                (parse_iso_timestamp(start), parse_iso_timestamp(end))
            else {
                return Ok(
                    "ERROR: start_time and end_time must be ISO 8601 format like \
                           \"2025-02-03T18:42:00Z\""
                        .to_string(),
                );
            };
            if window_end <= window_start {
                return Ok("ERROR: end_time must be after start_time".to_string());
            }
            (window_start, window_end)
        }
        (None, None) => match (shift(event_time, -5), shift(event_time, 60)) {
            (Some(start), Some(end)) => (start, end),
            _ => return Ok(OUT_OF_RANGE.to_string()),
        },
    };
    let start_epoch = epoch_seconds(start_time_dt);
    let end_epoch = epoch_seconds(end_time_dt);

    let mut base_filters = build_base_filters(location_hash, service, environment);
    for custom_filter in &filters {
        let custom_filter = strip(custom_filter);
        if custom_filter.is_empty() {
            continue;
        }
        // Custom filters are raw Logs Insights fragments the caller appends on
        // purpose, so they pass through unescaped. Reject only the realistic
        // corruption vector: an unbalanced double quote that would leak into
        // (or truncate) the rest of the query.
        let quotes = custom_filter.matches('"').count();
        let escaped_quotes = custom_filter.matches("\\\"").count();
        if !quotes.abs_diff(escaped_quotes).is_multiple_of(2) {
            return Ok(format!(
                "ERROR: custom_filters has unbalanced quotes: {}",
                safe_text(&pyfmt::repr_str(custom_filter))
            ));
        }
        base_filters.push_str(&format!(" and {custom_filter}"));
    }

    let query_string = format!(
        "fields @timestamp, @message\n| filter {base_filters}\n| sort @timestamp asc\n| limit {limit}"
    );
    let query_result = execute_cloudwatch_query(
        client,
        pacer,
        &query_string,
        start_epoch,
        end_epoch,
        &resolve_snapshot_log_group(service),
        max_timeout,
    );

    let start_time_utc = start_time_dt.isoformat_z();
    let end_time_utc = end_time_dt.isoformat_z();
    Ok(render_search_snapshots_for_status_event_output(
        &SearchContext {
            service_name: service,
            environment,
            location_hash,
            custom_filters: &filters,
            start_time_utc: &start_time_utc,
            end_time_utc: &end_time_utc,
            start_epoch,
            end_epoch,
            query_string: &query_string,
        },
        &query_result,
    ))
}

/// `get_sample_snapshot_for_breakpoint`.
pub(super) fn sample_snapshot(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    args: &Args,
) -> Result<String, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let location_hash = args.req_str("location_hash")?;
    let status_timestamp = args.req_str("status_timestamp")?;
    let max_timeout = args.opt_i64("max_timeout")?.unwrap_or(30);
    let include_raw = args.opt_bool("include_raw")?.unwrap_or(false);

    if !is_valid_location_hash(Some(location_hash)) {
        return Ok(HASH_ERROR.to_string());
    }
    let Ok(event_time) = parse_iso_timestamp(status_timestamp) else {
        return Ok(TIMESTAMP_ERROR.to_string());
    };
    let (Some(start_time), Some(end_time)) = (shift(event_time, -30), shift(event_time, 90)) else {
        return Ok(OUT_OF_RANGE.to_string());
    };
    let start_epoch = epoch_seconds(start_time);
    let end_epoch = epoch_seconds(end_time);

    let query_string = format!(
        "fields @timestamp, @message\n| filter {}\n| sort @timestamp desc\n| limit 1",
        build_base_filters(location_hash, service, environment)
    );
    let query_result = execute_cloudwatch_query(
        client,
        pacer,
        &query_string,
        start_epoch,
        end_epoch,
        &resolve_snapshot_log_group(service),
        max_timeout,
    );

    let start_time_utc = start_time.isoformat_z();
    let end_time_utc = end_time.isoformat_z();
    Ok(render_get_sample_snapshot_for_breakpoint_output(
        &SampleContext {
            service_name: service,
            environment,
            location_hash,
            start_time_utc: &start_time_utc,
            end_time_utc: &end_time_utc,
            max_timeout,
            query_string: &query_string,
            include_raw,
        },
        &query_result,
    ))
}
