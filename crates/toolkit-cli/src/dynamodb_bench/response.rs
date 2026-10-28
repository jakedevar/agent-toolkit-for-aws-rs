//! The benchmark Lambda's response, validated.
//!
//! `benchmark_model.py` read the response with `resp.get(...)` and trusted
//! every shape. Here each member is read through the shared accessors of
//! `toolkit_aws::recorded` (or the two marked helpers of [`super::shape`]) and
//! a member of the wrong type, `null` included, rejects the response. The only
//! nullable members are the ones the Lambda itself emits as `null`: the
//! optional numbers and the error code of a row.
//!
//! Diagnostics name the member and the row index, never a value.

use toolkit_aws::recorded::{
    optional_list, optional_member, optional_object, optional_text, MemberError,
};
use toolkit_core::Json;

use super::shape::{list_items, map_entries};
use crate::dynamodb::bench_fmt::Tally;
use crate::dynamodb::bench_rows::{BenchRow, ExactAgg};

/// The members of a `seed_verification` entry that the handler emits and the
/// report reads.
const SEED_MEMBERS: [&str; 5] = [
    "expected",
    "actual",
    "sampled",
    "passed",
    "seed_shortfall_ratio",
];

/// One `(pattern_id, phase)` tally of `exact_counts`.
pub(super) struct ExactCount {
    pub(super) pattern_id: String,
    pub(super) phase: String,
    pub(super) agg: ExactAgg,
}

/// The members of a response the orchestrator uses.
pub(super) struct LambdaResponse {
    pub(super) raw_rows: Vec<BenchRow>,
    /// `phases_run`, when the response has it.
    pub(super) phases_run: Option<Vec<String>>,
    /// `seed_verification` (an object of objects, empty when absent).
    pub(super) seed_verification: Json,
    pub(super) seed_verification_failed: bool,
    pub(super) handler_error: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) key_sampling: Option<String>,
    pub(super) items_per_partition: Option<i64>,
    pub(super) measurement_tainted: Vec<(String, String)>,
    pub(super) measured_patterns: Vec<String>,
    pub(super) exact_counts: Vec<ExactCount>,
}

type Parsed<T> = Result<T, String>;

fn member(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

fn text(object: &Json, key: &str) -> Parsed<Option<String>> {
    optional_text(object, key)
        .map(|value| value.map(str::to_string))
        .map_err(|error| member(&error))
}

fn required_text(object: &Json, key: &str, context: &str) -> Parsed<String> {
    text(object, key)?.ok_or_else(|| format!("{context}: \"{key}\" is required"))
}

fn flag(object: &Json, key: &str) -> Parsed<bool> {
    optional_member(object, key, "a boolean", |value| match value {
        Json::Bool(flag) => Some(*flag),
        _ => None,
    })
    .map(|value| value.unwrap_or(false))
    .map_err(|error| member(&error))
}

fn integer(object: &Json, key: &str) -> Parsed<Option<i64>> {
    optional_member(object, key, "an integer", |value| match value {
        Json::Int(number) => Some(*number),
        _ => None,
    })
    .map_err(|error| member(&error))
}

/// One table of a `seed_verification` map, checked member by member (review
/// #4954, Issue #4958). The handler's `verify_seed` emits `expected` and
/// `actual` as counts, `sampled` and `passed` as booleans and
/// `seed_shortfall_ratio` as a number. The Python truthiness rules (`passed`
/// read as true for a string, or when absent) are not kept: a present member of
/// another type, `null` included, is refused, and so is a missing `passed`,
/// because nothing may read as `ok` without an explicit `true`. The reason
/// names the member, never a value or the table. `generate-perf-report` applies
/// the same check to a hand-fed summary.
///
/// # Errors
///
/// The reason the entry is refused.
pub(crate) fn check_seed_entry(entry: &Json) -> Parsed<()> {
    if map_entries(entry).is_none() {
        return Err("a `seed_verification` entry must be an object".to_string());
    }
    let describe = |error: &MemberError| format!("a `seed_verification` entry: {}", member(error));
    let boolean = |key: &str| {
        optional_member(entry, key, "a boolean", |value| match value {
            Json::Bool(flag) => Some(*flag),
            _ => None,
        })
        .map_err(|error| describe(&error))
    };
    let count = |key: &str| {
        optional_member(entry, key, "an integer", |value| match value {
            Json::Int(number) => Some(*number),
            _ => None,
        })
        .map_err(|error| describe(&error))
    };
    if boolean("passed")?.is_none() {
        return Err("a `seed_verification` entry: \"passed\" is required (a boolean)".to_string());
    }
    boolean("sampled")?;
    count("expected")?;
    count("actual")?;
    optional_member(entry, "seed_shortfall_ratio", "a number", number_of)
        .map_err(|error| describe(&error))?;
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
/// A number the Lambda may emit as `null` (`latency_ms`, `consumed_cu`, ...).
fn nullable_number(object: &Json, key: &str) -> Parsed<Option<f64>> {
    optional_member(object, key, "a number or null", |value| match value {
        Json::Null => Some(None),
        Json::Int(number) => Some(Some(*number as f64)),
        Json::Float(number) => Some(Some(*number)),
        _ => None,
    })
    .map(Option::flatten)
    .map_err(|error| member(&error))
}

#[allow(clippy::cast_precision_loss)]
fn number_of(value: &Json) -> Option<f64> {
    match value {
        Json::Int(number) => Some(*number as f64),
        Json::Float(number) => Some(*number),
        _ => None,
    }
}

/// An object member whose entries are all numbers, in response order.
fn number_map(object: &Json, key: &str, nullable: bool) -> Parsed<Vec<(String, f64)>> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    if nullable && matches!(value, Json::Null) {
        return Ok(Vec::new());
    }
    let wrong = || {
        format!(
            "\"{key}\" must be an object of numbers when present (it is {})",
            value.type_name()
        )
    };
    let entries = map_entries(value).ok_or_else(wrong)?;
    entries
        .iter()
        .map(|(name, item)| number_of(item).map(|n| (name.clone(), n)).ok_or_else(wrong))
        .collect()
}

fn count_map(object: &Json, key: &str) -> Parsed<Tally> {
    let Some(value) = object.get(key) else {
        return Ok(Tally::default());
    };
    let wrong = || {
        format!(
            "\"{key}\" must be an object of integer counts when present (it is {})",
            value.type_name()
        )
    };
    let entries = map_entries(value).ok_or_else(wrong)?;
    let mut tally = Tally::default();
    for (name, item) in entries {
        let Json::Int(count) = item else {
            return Err(wrong());
        };
        tally.add(name, *count);
    }
    Ok(tally)
}

fn string_list(object: &Json, key: &str) -> Parsed<Vec<String>> {
    Ok(toolkit_aws::recorded::optional_string_list(object, key)
        .map_err(|error| member(&error))?
        .into_iter()
        .map(str::to_string)
        .collect())
}

fn parse_row(index: usize, row: &Json) -> Parsed<BenchRow> {
    let bad = |what: String| format!("raw_rows[{index}]: {what}");
    if map_entries(row).is_none() {
        return Err(bad("must be an object".to_string()));
    }
    let error = optional_member(row, "error", "a string or null", |value| match value {
        Json::Null => Some(None),
        Json::String(code) => Some((!code.is_empty()).then(|| code.clone())),
        _ => None,
    })
    .map(Option::flatten)
    .map_err(|e| bad(member(&e)))?;
    let throttled = optional_member(row, "throttled", "a boolean or null", |value| match value {
        Json::Null => Some(false),
        Json::Bool(flag) => Some(*flag),
        _ => None,
    })
    .map(|value| value.unwrap_or(false))
    .map_err(|e| bad(member(&e)))?;
    let key_idx = optional_member(row, "key_idx", "an integer or null", |value| match value {
        Json::Null => Some(None),
        Json::Int(number) => Some(Some(*number)),
        _ => None,
    })
    .map(Option::flatten)
    .map_err(|e| bad(member(&e)))?;
    Ok(BenchRow {
        json: row.clone(),
        pattern_id: text(row, "pattern_id").map_err(&bad)?,
        phase: text(row, "phase").map_err(&bad)?,
        latency_ms: nullable_number(row, "latency_ms").map_err(&bad)?,
        consumed_cu: nullable_number(row, "consumed_cu").map_err(&bad)?,
        gsi_cu: number_map(row, "gsi_cu", true).map_err(&bad)?,
        throttled,
        error,
        key_idx,
        vector_search_bytes: nullable_number(row, "vector_search_bytes").map_err(&bad)?,
        vector_write_bytes: number_map(row, "vector_write_bytes", true).map_err(&bad)?,
    })
}

fn parse_exact(index: usize, entry: &Json) -> Parsed<ExactCount> {
    let bad = |what: String| format!("exact_counts[{index}]: {what}");
    if map_entries(entry).is_none() {
        return Err(bad("must be an object".to_string()));
    }
    Ok(ExactCount {
        pattern_id: required_text(entry, "pattern_id", &format!("exact_counts[{index}]"))?,
        phase: required_text(entry, "phase", &format!("exact_counts[{index}]"))?,
        agg: ExactAgg {
            calls: integer(entry, "calls").map_err(&bad)?.unwrap_or(0),
            throttles: integer(entry, "throttles").map_err(&bad)?.unwrap_or(0),
            errors: integer(entry, "errors").map_err(&bad)?.unwrap_or(0),
            error_codes: count_map(entry, "error_codes").map_err(&bad)?,
            cancellation_reason_codes: count_map(entry, "cancellation_reason_codes")
                .map_err(&bad)?,
        },
    })
}

/// Validates the payload the Lambda returned.
///
/// # Errors
///
/// A reason naming the member (and the row index), never a value.
pub(super) fn parse(payload: &Json) -> Parsed<LambdaResponse> {
    if map_entries(payload).is_none() {
        return Err("the Lambda payload must be a JSON object".to_string());
    }
    let mut raw_rows = Vec::new();
    for (index, row) in optional_list(payload, "raw_rows")
        .map_err(|e| member(&e))?
        .iter()
        .enumerate()
    {
        raw_rows.push(parse_row(index, row)?);
    }
    let phases_run = match payload.get("phases_run") {
        None => None,
        Some(_) => Some(string_list(payload, "phases_run")?),
    };
    let seed_verification =
        match optional_object(payload, "seed_verification").map_err(|e| member(&e))? {
            None => Json::Object(Vec::new()),
            Some(object) => {
                let mut tables = Vec::new();
                for (table, entry) in map_entries(object).unwrap_or_default() {
                    if map_entries(entry).is_none() {
                        return Err("\"seed_verification\" must map tables to objects".to_string());
                    }
                    check_seed_entry(entry)?;
                    // Only the five members `verify_seed` emits are kept.
                    let kept: Vec<(String, Json)> = map_entries(entry)
                        .unwrap_or_default()
                        .iter()
                        .filter(|(key, _)| SEED_MEMBERS.contains(&key.as_str()))
                        .cloned()
                        .collect();
                    tables.push((table.clone(), Json::Object(kept)));
                }
                Json::Object(tables)
            }
        };
    let mut measurement_tainted = Vec::new();
    if let Some(object) = optional_object(payload, "measurement_tainted").map_err(|e| member(&e))? {
        for (pid, reason) in map_entries(object).unwrap_or_default() {
            let Json::String(reason) = reason else {
                return Err("\"measurement_tainted\" must map pattern ids to strings".to_string());
            };
            measurement_tainted.push((pid.clone(), reason.clone()));
        }
    }
    let measured_patterns = match optional_object(payload, "coverage").map_err(|e| member(&e))? {
        None => Vec::new(),
        Some(coverage) => string_list(coverage, "measured_patterns")?,
    };
    let mut exact_counts = Vec::new();
    for (index, entry) in match payload.get("exact_counts") {
        None => &[],
        Some(value) => list_items(value).ok_or_else(|| {
            format!(
                "\"exact_counts\" must be a list when present (it is {})",
                value.type_name()
            )
        })?,
    }
    .iter()
    .enumerate()
    {
        exact_counts.push(parse_exact(index, entry)?);
    }
    Ok(LambdaResponse {
        raw_rows,
        phases_run,
        seed_verification,
        seed_verification_failed: flag(payload, "seed_verification_failed")?,
        handler_error: text(payload, "handler_error")?.filter(|message| !message.is_empty()),
        mode: text(payload, "mode")?,
        key_sampling: text(payload, "key_sampling")?,
        items_per_partition: integer(payload, "items_per_partition")?,
        measurement_tainted,
        measured_patterns,
        exact_counts,
    })
}
