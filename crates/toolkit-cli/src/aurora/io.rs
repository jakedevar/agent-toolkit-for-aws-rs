//! Aurora I/O-Optimized versus Standard cost comparison
//! (`toolkit aurora io`).
//!
//! I/O-Optimized is recommended when it lowers the total monthly cost: the
//! eliminated I/O charges must exceed the compute plus storage premium. The
//! "I/O is at least 25% of spend" figure is reported as a rule of thumb only;
//! the recommendation follows the computed dollar savings.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_core::{Json, ToolkitError};

use crate::{Invocation, Output};

use super::api::{
    attribute, aurora_cluster_ids, on_demand_dimensions, parse_price_item, usd_price, AwsApi,
};
use super::args::{
    float_value, format_value, instance_value, int_value, region_value, reject_unrecognized,
    required, usage, Format,
};
use super::pyfmt::{dumps, fmt_f, get_f64, get_str, py_max, round_num, round_to, truthy, Num};
use super::{
    location, Engine, Tool, HOURS_PER_MONTH, IO_OPT_COMPUTE_MULTIPLIER, STATIC_INSTANCES,
    STATIC_STORAGE_IO_OPT_PER_GIB, STATIC_STORAGE_STANDARD_PER_GIB,
};

/// Static I/O price, USD per million requests (Standard only).
const STATIC_IO_COST_PER_MILLION: f64 = 0.20;
/// Rule-of-thumb share of spend that I/O should reach.
const IO_OPT_BREAKEVEN_PCT: f64 = 25.0;
/// Largest `timedelta(days=...)` Python accepts.
const MAX_LOOKBACK_DAYS: i64 = 999_999_999;
/// 0001-01-01T00:00:00Z and 9999-12-31T23:59:59Z as epoch seconds.
const MIN_EPOCH: i64 = -62_135_596_800;
const MAX_EPOCH: i64 = 253_402_300_799;

/// Active prices and where they came from.
#[derive(Debug, Clone)]
pub(crate) struct IoPricing {
    pub instance_prices: BTreeMap<String, f64>,
    pub storage_standard: f64,
    pub storage_io_opt: f64,
    pub io_per_million: f64,
    pub source: Json,
}

impl IoPricing {
    /// The built-in us-east-1 defaults.
    pub(crate) fn static_defaults() -> Self {
        Self {
            instance_prices: STATIC_INSTANCES
                .iter()
                .map(|(name, _, _, price)| ((*name).to_string(), *price))
                .collect(),
            storage_standard: STATIC_STORAGE_STANDARD_PER_GIB,
            storage_io_opt: STATIC_STORAGE_IO_OPT_PER_GIB,
            io_per_million: STATIC_IO_COST_PER_MILLION,
            source: Json::object(vec![
                ("source", Json::string("static_fallback")),
                ("region", Json::string("us-east-1")),
            ]),
        }
    }
}

fn static_source(region: &str, key: &str, text: String) -> Json {
    Json::object(vec![
        ("source", Json::string("static_fallback")),
        ("region", Json::string(region)),
        (key, Json::String(text)),
    ])
}

/// Reads instance, storage and I/O prices into `state`; returns how many
/// instance prices were found. `state` keeps whatever was read before a
/// failure, like the module globals did.
fn refresh_live(
    api: &dyn AwsApi,
    engine: Engine,
    place: &str,
    state: &mut IoPricing,
) -> Result<usize, String> {
    let mut live_instances = 0usize;
    let instance_filters = [
        ("databaseEngine", engine.pricing_name()),
        ("location", place),
        ("deploymentOption", "Single-AZ"),
        ("termType", "OnDemand"),
    ];
    for raw in api.pricing_products(&instance_filters)? {
        let item = parse_price_item(&raw)?;
        let instance_type = attribute(&item, "instanceType");
        if !instance_type.starts_with("db.") {
            continue;
        }
        if attribute(&item, "usagetype").contains("IOOptimized") {
            continue;
        }
        for dimension in on_demand_dimensions(&item) {
            let Ok(price) = usd_price(dimension) else {
                continue;
            };
            if price > 0.0 {
                state
                    .instance_prices
                    .insert(instance_type.to_string(), price);
                live_instances += 1;
            }
        }
    }

    let storage_filters = [
        ("databaseEngine", engine.pricing_name()),
        ("location", place),
        ("productFamily", "Database Storage"),
        ("termType", "OnDemand"),
    ];
    for raw in api.pricing_products(&storage_filters)? {
        let item = parse_price_item(&raw)?;
        let usage = attribute(&item, "usagetype");
        for dimension in on_demand_dimensions(&item) {
            let Ok(price) = usd_price(dimension) else {
                continue;
            };
            if price <= 0.0 {
                continue;
            }
            if usage.contains("IOOptimized") {
                state.storage_io_opt = price;
            } else if usage.contains("Aurora:StorageUsage") {
                state.storage_standard = price;
            } else if usage.contains("Aurora:StorageIOUsage") {
                // Per request to per million requests.
                state.io_per_million = price * 1_000_000.0;
            }
        }
    }
    Ok(live_instances)
}

/// Best-effort live pricing; any failure keeps the static defaults.
pub(crate) fn refresh_pricing(api: &dyn AwsApi, engine: Engine, region: &str) -> IoPricing {
    let mut state = IoPricing::static_defaults();
    let Some(place) = location(Tool::Io, region) else {
        state.source = static_source(
            region,
            "note",
            format!("Region {region} not mapped; using us-east-1 defaults"),
        );
        return state;
    };
    if !api.available() {
        state.source = static_source(region, "note", "AWS SDK not available".to_string());
        return state;
    }
    match refresh_live(api, engine, place, &mut state) {
        Ok(live_instances) => {
            state.source = Json::object(vec![
                ("source", Json::string("live")),
                ("region", Json::string(region)),
                (
                    "live_instances",
                    Json::Int(i64::try_from(live_instances).unwrap_or(i64::MAX)),
                ),
                ("storage_standard", Json::Float(state.storage_standard)),
                ("storage_io_opt", Json::Float(state.storage_io_opt)),
                ("io_per_million", Json::Float(state.io_per_million)),
            ]);
        }
        Err(error) => state.source = static_source(region, "error", error),
    }
    state
}

// ---------------------------------------------------------------------------
// Core calculation
// ---------------------------------------------------------------------------

/// Standard versus I/O-Optimized comparison and recommendation.
pub(crate) fn compute_comparison(
    pricing: &IoPricing,
    compute_monthly: f64,
    storage_gib: f64,
    monthly_io_millions: f64,
) -> Json {
    let storage_std = storage_gib * pricing.storage_standard;
    let io_cost = monthly_io_millions * pricing.io_per_million;
    let total_std = compute_monthly + storage_std + io_cost;

    let compute_io_opt = compute_monthly * IO_OPT_COMPUTE_MULTIPLIER;
    let storage_io_opt = storage_gib * pricing.storage_io_opt;
    let total_io_opt = compute_io_opt + storage_io_opt;

    let io_pct = if total_std > 0.0 {
        Num::Float(io_cost / total_std * 100.0)
    } else {
        Num::Int(0)
    };
    let savings = total_std - total_io_opt;

    // The recommendation follows the actual dollar savings, not the 25%
    // heuristic: near break-even the two diverge.
    let pct = io_pct.value();
    let threshold_note = format!(
        "I/O is {}% of total cost ({}{}% rule-of-thumb).",
        fmt_f(pct, 0),
        if pct >= IO_OPT_BREAKEVEN_PCT {
            "≥"
        } else {
            "below "
        },
        fmt_f(IO_OPT_BREAKEVEN_PCT, 0)
    );
    let (recommendation, reason) = if savings > 0.0 {
        (
            "io_optimized",
            format!(
                "{threshold_note} I/O-Optimized saves ${}/mo.",
                fmt_f(savings, 0)
            ),
        )
    } else {
        (
            "standard",
            format!(
                "{threshold_note} I/O-Optimized would cost ${}/mo more.",
                fmt_f(savings.abs(), 0)
            ),
        )
    };

    let money = |value: f64| Json::Float(round_to(value, 2));
    Json::object(vec![
        (
            "standard",
            Json::object(vec![
                ("compute_monthly", money(compute_monthly)),
                ("storage_monthly", money(storage_std)),
                ("io_monthly", money(io_cost)),
                ("total_monthly", money(total_std)),
            ]),
        ),
        (
            "io_optimized",
            Json::object(vec![
                ("compute_monthly", money(compute_io_opt)),
                ("storage_monthly", money(storage_io_opt)),
                ("io_monthly", Json::Float(0.0)),
                ("total_monthly", money(total_io_opt)),
            ]),
        ),
        (
            "monthly_io_millions",
            Json::Float(round_to(monthly_io_millions, 1)),
        ),
        ("storage_gib", Json::Float(round_to(storage_gib, 1))),
        ("io_cost_pct_of_total", round_num(io_pct, 1).to_json()),
        ("savings_with_io_opt", money(savings)),
        ("recommendation", Json::string(recommendation)),
        ("reason", Json::string(reason)),
    ])
}

/// How much `CloudWatch` history backs a result.
pub(crate) fn data_quality_tag(days: i64) -> &'static str {
    if days < 3 {
        "insufficient"
    } else if days < 7 {
        "short"
    } else if days < 14 {
        "adequate"
    } else {
        "good"
    }
}

/// Analysis of user-supplied numbers; an unknown class yields an `error`
/// object (exit status 0, as before).
pub(crate) fn analyze_offline(
    pricing: &IoPricing,
    instance: &str,
    num_instances: i64,
    storage_gib: f64,
    monthly_io_millions: f64,
) -> Json {
    let Some(price) = pricing.instance_prices.get(instance) else {
        let supported: Vec<&str> = pricing.instance_prices.keys().map(String::as_str).collect();
        return Json::object(vec![(
            "error",
            Json::string(format!(
                "Unknown instance type: {instance}. Supported: {}",
                supported.join(", ")
            )),
        )]);
    };
    let compute_monthly = price * HOURS_PER_MONTH * num_instances as f64;
    let comparison = compute_comparison(pricing, compute_monthly, storage_gib, monthly_io_millions);
    let mut members = vec![
        ("cluster_id".to_string(), Json::string("offline-input")),
        ("instance_type".to_string(), Json::string(instance)),
        ("num_instances".to_string(), Json::Int(num_instances)),
        ("data_quality".to_string(), Json::string("user_supplied")),
    ];
    if let Json::Object(more) = comparison {
        members.extend(more);
    }
    Json::Object(members)
}

// ---------------------------------------------------------------------------
// Live cluster analysis
// ---------------------------------------------------------------------------

fn aws_error(message: String) -> ToolkitError {
    ToolkitError::runtime("aws_error", message)
}

/// `int(value)` for a finite float within `i64`.
fn truncate_to_i64(value: f64) -> Result<i64, ToolkitError> {
    if value.is_nan() {
        return Err(ToolkitError::runtime(
            "number_out_of_range",
            "cannot convert float NaN to integer",
        ));
    }
    if !value.is_finite() || value.abs() >= 9.2e18 {
        return Err(ToolkitError::runtime(
            "number_out_of_range",
            "observed I/O total is out of range",
        ));
    }
    Ok(value.trunc() as i64)
}

/// Whole hours since the epoch, as seconds (`datetime.replace(minute=0, ...)`).
pub(crate) fn floor_to_hour(epoch_seconds: i64) -> i64 {
    epoch_seconds - epoch_seconds.rem_euclid(3600)
}

/// The `CloudWatch` window `(start, end)` for `days` of history ending at the
/// current hour; magnitudes Python's `timedelta` or `datetime` cannot hold are
/// runtime errors.
pub(crate) fn lookback_window(now_epoch: i64, days: i64) -> Result<(i64, i64), ToolkitError> {
    let out_of_range = |message: String| ToolkitError::runtime("days_out_of_range", message);
    if days.unsigned_abs() > MAX_LOOKBACK_DAYS.unsigned_abs() {
        return Err(out_of_range(format!(
            "days={days}; must have magnitude <= {MAX_LOOKBACK_DAYS}"
        )));
    }
    let end = floor_to_hour(now_epoch);
    let start = days
        .checked_mul(86_400)
        .and_then(|span| end.checked_sub(span))
        .filter(|start| (MIN_EPOCH..=MAX_EPOCH).contains(start))
        .ok_or_else(|| out_of_range("date value out of range".to_string()))?;
    Ok((start, end))
}

/// Analyzes one cluster from instance prices and `CloudWatch` I/O and size.
pub(crate) fn analyze_cluster_live(
    api: &dyn AwsApi,
    engine: Engine,
    pricing: &IoPricing,
    cluster_id: &str,
    region: &str,
    days: i64,
    now_epoch: i64,
) -> Result<Json, ToolkitError> {
    let Some(cluster) = api
        .describe_cluster(region, cluster_id)
        .map_err(aws_error)?
    else {
        return Ok(Json::object(vec![
            ("cluster_id", Json::string(cluster_id)),
            ("error", Json::string("cluster not found")),
        ]));
    };

    // Skip clusters with no instances: there is no compute to compare.
    if cluster.members.is_empty() {
        return Ok(Json::object(vec![
            ("cluster_id", Json::string(cluster_id)),
            ("engine", Json::string(cluster.engine.clone())),
            (
                "engine_version",
                Json::string(cluster.engine_version.clone()),
            ),
            (
                "current_storage_type",
                Json::string(cluster.storage_type.clone()),
            ),
            ("skipped", Json::Bool(true)),
            ("reason", Json::string(engine.io_empty_reason())),
        ]));
    }

    let mut compute_monthly = 0.0;
    let mut instance_summary: Vec<Json> = Vec::new();
    let mut compute_warnings: Vec<String> = Vec::new();
    for member in &cluster.members {
        let classes = match api.instance_classes(region, member) {
            Ok(classes) => classes,
            Err(message) => {
                instance_summary.push(Json::object(vec![
                    ("id", Json::string(member.clone())),
                    ("error", Json::string(message)),
                ]));
                continue;
            }
        };
        for class in classes {
            // Serverless v2 has no fixed hourly rate; counting it at $0 would
            // understate compute, so it is excluded and the estimate flagged.
            if class == "db.serverless" {
                instance_summary.push(Json::object(vec![
                    ("id", Json::string(member.clone())),
                    ("type", Json::string(class)),
                    ("note", Json::string("serverless_excluded")),
                ]));
                compute_warnings.push(format!(
                    "{member} is Aurora Serverless v2 (db.serverless) — its ACU-based compute \
                     cost is not included (it has no fixed hourly rate); the Standard vs \
                     I/O-Optimized compute figures below cover provisioned instances only."
                ));
                continue;
            }
            let price = pricing.instance_prices.get(&class).copied().unwrap_or(0.0);
            if price == 0.0 {
                // Unknown or unpriced class: never add $0 silently.
                compute_warnings.push(format!(
                    "{member} ({class}) has no known hourly price in the static/live table — \
                     excluded from the compute estimate; results are partial."
                ));
                instance_summary.push(Json::object(vec![
                    ("id", Json::string(member.clone())),
                    ("type", Json::string(class)),
                    ("note", Json::string("unknown_price")),
                ]));
                continue;
            }
            compute_monthly += price * HOURS_PER_MONTH;
            instance_summary.push(Json::object(vec![
                ("id", Json::string(member.clone())),
                ("type", Json::string(class)),
                ("price_hr", Json::Float(price)),
            ]));
        }
    }

    let window = lookback_window(now_epoch, days)?;
    let observed_hours = days
        .checked_mul(24)
        .ok_or_else(|| ToolkitError::runtime("days_out_of_range", "days out of range"))?;

    let sum_metric = |metric: &str| -> Result<f64, ToolkitError> {
        let values = api
            .metric_values(region, cluster_id, metric, "Sum", window)
            .map_err(aws_error)?;
        Ok(py_sum(&values))
    };
    let read_io = sum_metric("VolumeReadIOPs")?;
    let write_io = sum_metric("VolumeWriteIOPs")?;
    let total_io = read_io + write_io;
    // Extrapolate to a 730-hour month.
    let monthly_io = if observed_hours > 0 {
        total_io / observed_hours as f64 * HOURS_PER_MONTH
    } else {
        0.0
    };
    let monthly_io_millions = monthly_io / 1_000_000.0;

    let average_bytes = {
        let values = api
            .metric_values(region, cluster_id, "VolumeBytesUsed", "Average", window)
            .map_err(aws_error)?;
        if values.is_empty() {
            0.0
        } else {
            py_sum(&values) / values.len() as f64
        }
    };
    // Aurora bills actual usage; there is no fixed minimum.
    let storage_gib = average_bytes / 1_073_741_824.0;

    let comparison = compute_comparison(pricing, compute_monthly, storage_gib, monthly_io_millions);
    let mut members = vec![
        ("cluster_id".to_string(), Json::string(cluster_id)),
        ("engine".to_string(), Json::string(cluster.engine.clone())),
        (
            "current_storage_type".to_string(),
            Json::string(cluster.storage_type.clone()),
        ),
        ("instances".to_string(), Json::Array(instance_summary)),
        ("lookback_days".to_string(), Json::Int(days)),
        (
            "data_quality".to_string(),
            Json::string(data_quality_tag(days)),
        ),
        (
            "observed_io_total".to_string(),
            Json::Int(truncate_to_i64(total_io)?),
        ),
    ];
    if let Json::Object(more) = comparison {
        members.extend(more);
    }
    if !compute_warnings.is_empty() {
        members.push(("compute_partial".to_string(), Json::Bool(true)));
        members.push((
            "compute_warnings".to_string(),
            Json::Array(compute_warnings.into_iter().map(Json::String).collect()),
        ));
    }
    Ok(Json::Object(members))
}

/// Python `sum` of floats: starts at `0`, adds left to right.
fn py_sum(values: &[f64]) -> f64 {
    let mut total = 0.0;
    for value in values {
        total += value;
    }
    total
}

fn sum_nums(values: impl Iterator<Item = Num>) -> Num {
    values.fold(Num::Int(0), Num::plus)
}

fn number_at(value: &Json, outer: &str, inner: &str) -> Num {
    value
        .get(outer)
        .and_then(|group| group.get(inner))
        .and_then(Num::from_json)
        .unwrap_or(Num::Int(0))
}

/// Fleet totals; errored and skipped clusters stay out of the dollar sums.
pub(crate) fn fleet_summary(results: &[Json]) -> Json {
    let analyzable: Vec<&Json> = results
        .iter()
        .filter(|entry| entry.get("error").is_none() && !truthy(entry.get("skipped")))
        .collect();
    let skipped: Vec<&Json> = results
        .iter()
        .filter(|entry| truthy(entry.get("skipped")))
        .collect();
    let total_std = sum_nums(
        analyzable
            .iter()
            .map(|entry| number_at(entry, "standard", "total_monthly")),
    );
    let total_io_opt = sum_nums(
        analyzable
            .iter()
            .map(|entry| number_at(entry, "io_optimized", "total_monthly")),
    );
    let switch_wins: Vec<Json> = analyzable
        .iter()
        .filter(|entry| get_str(entry, "recommendation", "") == "io_optimized")
        .map(|entry| Json::string(get_str(entry, "cluster_id", "")))
        .collect();
    let optimal = sum_nums(analyzable.iter().map(|entry| {
        py_max(
            Num::Int(0),
            entry
                .get("savings_with_io_opt")
                .and_then(Num::from_json)
                .unwrap_or(Num::Int(0)),
        )
    }));
    let count = |items: usize| Json::Int(i64::try_from(items).unwrap_or(i64::MAX));
    Json::object(vec![
        ("cluster_count", count(results.len())),
        ("analyzable_count", count(analyzable.len())),
        ("skipped_count", count(skipped.len())),
        (
            "skipped_clusters",
            Json::Array(
                skipped
                    .iter()
                    .map(|entry| {
                        Json::object(vec![
                            ("cluster_id", Json::string(get_str(entry, "cluster_id", ""))),
                            ("reason", Json::string(get_str(entry, "reason", ""))),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("clusters_that_should_switch", Json::Array(switch_wins)),
        (
            "current_monthly_total_standard",
            round_num(total_std, 2).to_json(),
        ),
        (
            "if_all_on_io_optimized_monthly",
            round_num(total_io_opt, 2).to_json(),
        ),
        ("optimal_savings_monthly", round_num(optimal, 2).to_json()),
    ])
}

// ---------------------------------------------------------------------------
// Table output
// ---------------------------------------------------------------------------

fn print_fleet(output: &mut Output<'_>, engine: Engine, result: &Json) -> Result<(), ToolkitError> {
    let empty = Json::Object(Vec::new());
    let summary = result.get("summary").unwrap_or(&empty);
    let count = |key: &str| {
        summary
            .get(key)
            .and_then(Num::from_json)
            .map_or_else(|| "0".to_string(), Num::py_str)
    };
    output.line(&format!(
        "Region: {}  Clusters: {} (analyzable: {}, skipped: {})",
        get_str(result, "region", ""),
        count("cluster_count"),
        count("analyzable_count"),
        count("skipped_count")
    ))?;
    output.line(&format!(
        "  Current (Standard):      ${}/mo",
        fmt_f(get_f64(summary, "current_monthly_total_standard", 0.0), 0)
    ))?;
    output.line(&format!(
        "  All on I/O-Optimized:    ${}/mo",
        fmt_f(get_f64(summary, "if_all_on_io_optimized_monthly", 0.0), 0)
    ))?;
    output.line(&format!(
        "  Optimal (switch winners): saves ${}/mo",
        fmt_f(get_f64(summary, "optimal_savings_monthly", 0.0), 0)
    ))?;
    let switching: Vec<&str> = match summary.get("clusters_that_should_switch") {
        Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    output.line(&format!(
        "  Clusters to switch: {}",
        if switching.is_empty() {
            "(none)".to_string()
        } else {
            switching.join(", ")
        }
    ))?;
    if let Some(Json::Array(skipped)) = summary.get("skipped_clusters") {
        if !skipped.is_empty() {
            output.line("  Skipped (not applicable):")?;
            for entry in skipped {
                let reason: String = get_str(entry, "reason", "").chars().take(80).collect();
                output.line(&format!(
                    "    - {}: {reason}",
                    get_str(entry, "cluster_id", "")
                ))?;
            }
        }
    }
    output.line("")?;
    output.line(&format!(
        "{:<30} {:>6} {:>10} {:>12} {:>14}",
        "Cluster", "I/O %", "Std $/mo", "IOOpt $/mo", "Rec"
    ))?;
    output.line(&"-".repeat(76))?;
    let (skip_cell, skip_width) = engine.io_skipped_cell();
    if let Some(Json::Array(clusters)) = result.get("clusters") {
        for entry in clusters {
            let id = get_str(entry, "cluster_id", "");
            if entry.get("error").is_some() {
                output.line(&format!("{id:<30} ERROR: {}", get_str(entry, "error", "")))?;
                continue;
            }
            if truthy(entry.get("skipped")) {
                output.line(&format!(
                    "{id:<30} {:>6} {:>10} {:>12} {skip_cell:>skip_width$}",
                    "—", "—", "—"
                ))?;
                continue;
            }
            output.line(&format!(
                "{id:<30} {:>5}% {:>10} {:>12} {:>14}",
                fmt_f(get_f64(entry, "io_cost_pct_of_total", 0.0), 0),
                fmt_f(number_at(entry, "standard", "total_monthly").value(), 0),
                fmt_f(number_at(entry, "io_optimized", "total_monthly").value(), 0),
                get_str(entry, "recommendation", "")
            ))?;
        }
    }
    Ok(())
}

fn print_single(output: &mut Output<'_>, result: &Json) -> Result<(), ToolkitError> {
    if truthy(result.get("skipped")) {
        output.line(&format!("Cluster: {}", get_str(result, "cluster_id", "?")))?;
        output.line(&format!(
            "  Engine: {} {}",
            get_str(result, "engine", "?"),
            get_str(result, "engine_version", "")
        ))?;
        output.line("  Status: SKIPPED — not applicable")?;
        return output.line(&format!("  {}", get_str(result, "reason", "")));
    }
    if result.get("standard").is_none() {
        // The Python script raised KeyError here; report the cause instead.
        return Err(ToolkitError::runtime(
            "analysis_failed",
            get_str(result, "error", "no comparison available").to_string(),
        ));
    }
    output.line(&format!(
        "Cluster: {}  ({} data)",
        get_str(result, "cluster_id", "?"),
        get_str(result, "data_quality", "?")
    ))?;
    output.line(&format!(
        "  Current storage type: {}",
        get_str(result, "current_storage_type", "?")
    ))?;
    output.line(&format!(
        "  Monthly I/O: {}M requests",
        fmt_f(get_f64(result, "monthly_io_millions", 0.0), 0)
    ))?;
    output.line(&format!(
        "  Storage: {} GiB",
        fmt_f(get_f64(result, "storage_gib", 0.0), 0)
    ))?;
    output.line("")?;
    let standard = result.get("standard").unwrap_or(result);
    let optimized = result.get("io_optimized").unwrap_or(result);
    output.line(&format!(
        "  {:<12} {:>12} {:>15}",
        "Component", "Standard", "I/O-Optimized"
    ))?;
    for (label, key) in [
        ("Compute", "compute_monthly"),
        ("Storage", "storage_monthly"),
        ("I/O", "io_monthly"),
        ("Total", "total_monthly"),
    ] {
        output.line(&format!(
            "  {label:<12} {:>12} {:>15}",
            fmt_f(get_f64(standard, key, 0.0), 0),
            fmt_f(get_f64(optimized, key, 0.0), 0)
        ))?;
    }
    output.line("")?;
    output.line(&format!(
        "  I/O cost: {}% of total",
        fmt_f(get_f64(result, "io_cost_pct_of_total", 0.0), 0)
    ))?;
    output.line(&format!(
        "  Recommendation: {}",
        get_str(result, "recommendation", "").to_uppercase()
    ))?;
    output.line(&format!("  {}", get_str(result, "reason", "")))
}

fn emit(
    output: &mut Output<'_>,
    engine: Engine,
    result: &Json,
    format: Format,
    fleet: bool,
) -> Result<(), ToolkitError> {
    match (format, fleet) {
        (Format::Json, _) => output.line(&dumps(result)),
        (Format::Table, true) => print_fleet(output, engine, result),
        (Format::Table, false) => print_single(output, result),
    }
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

const OFFLINE_ONLY_FLAGS: &[&str] = &[
    "--instance",
    "--num-instances",
    "--storage-gib",
    "--monthly-io-millions",
];

/// The current time as epoch seconds.
pub(crate) fn system_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

fn with_pricing_source(result: Json, pricing: &IoPricing) -> Json {
    match result {
        Json::Object(mut members) => {
            members.push(("pricing_source".to_string(), pricing.source.clone()));
            Json::Object(members)
        }
        other => other,
    }
}

/// `toolkit aurora io [offline]`.
pub(crate) fn command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    api: &dyn AwsApi,
    now_epoch: i64,
) -> Result<(), ToolkitError> {
    let engine = Engine::from_invocation(invocation)?;
    let region = region_value(invocation);
    let days = int_value(invocation, "--days")?.unwrap_or(14);
    let format = format_value(invocation)?;
    let offline = match invocation.positional(0) {
        None => false,
        Some("offline") => true,
        Some(other) => {
            return Err(usage(format!(
                "argument mode: invalid choice: {} (choose from 'offline')",
                crate::manifests::py_repr_str(&crate::safe_text::shown_value(other))
            )))
        }
    };

    if offline {
        let num_instances = int_value(invocation, "--num-instances")?.unwrap_or(1);
        let storage = float_value(invocation, "--storage-gib")?;
        let io_millions = float_value(invocation, "--monthly-io-millions")?;
        let mut missing = Vec::new();
        if invocation.value("--instance").is_none() {
            missing.push("--instance");
        }
        if storage.is_none() {
            missing.push("--storage-gib");
        }
        if io_millions.is_none() {
            missing.push("--monthly-io-millions");
        }
        required(&missing)?;
        let (Some(instance), Some(storage), Some(io_millions)) =
            (instance_value(invocation), storage, io_millions)
        else {
            return Ok(());
        };
        // Pricing is refreshed even offline so regional rates can apply.
        let pricing = refresh_pricing(api, engine, &region);
        let result = with_pricing_source(
            analyze_offline(&pricing, &instance, num_instances, storage, io_millions),
            &pricing,
        );
        return emit(output, engine, &result, format, false);
    }

    reject_unrecognized(invocation, OFFLINE_ONLY_FLAGS)?;
    if !api.available() {
        return Err(usage(
            "AWS SDK access is not available in this build; use the 'offline' subcommand.",
        ));
    }
    let pricing = refresh_pricing(api, engine, &region);

    if invocation.has_flag("--all") {
        let ids = aurora_cluster_ids(api, &region).map_err(aws_error)?;
        if ids.is_empty() {
            let result = Json::object(vec![
                ("status", Json::string("ok")),
                ("region", Json::string(region.clone())),
                ("clusters", Json::Array(Vec::new())),
            ]);
            return output.line(&dumps(&result));
        }
        let mut results = Vec::with_capacity(ids.len());
        for id in &ids {
            results.push(analyze_cluster_live(
                api, engine, &pricing, id, &region, days, now_epoch,
            )?);
        }
        let summary = fleet_summary(&results);
        let result = Json::object(vec![
            ("region", Json::string(region.clone())),
            ("pricing_source", pricing.source.clone()),
            ("summary", summary),
            ("clusters", Json::Array(results)),
        ]);
        return emit(output, engine, &result, format, true);
    }
    if let Some(cluster) = invocation
        .value("--cluster")
        .filter(|name| !name.is_empty())
    {
        let result =
            analyze_cluster_live(api, engine, &pricing, cluster, &region, days, now_epoch)?;
        return emit(
            output,
            engine,
            &with_pricing_source(result, &pricing),
            format,
            false,
        );
    }
    output.line(crate::USAGE)
}
