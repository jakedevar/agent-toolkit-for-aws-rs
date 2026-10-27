//! `toolkit elasticache price` (port of `price_calculator.py`): serverless and
//! node-based cost estimates, reserved-node options and Extended Support
//! surcharges from the shared price tables.

use toolkit_core::Json;

use crate::aurora::pyfmt::{fmt_grouped, round_to};

use super::pricing::{PricingLoader, HOURS_PER_MONTH};
use super::text::{js, kv, pad_left, pad_right, round_int, show, RoundError};

/// The one-line disclaimer inside the JSON estimates.
pub const JSON_DISCLAIMER: &str =
    "Estimates only. Verify at https://aws.amazon.com/elasticache/pricing/";

/// The indented disclaimer closing the text reports.
pub const PRICING_DISCLAIMER: &str =
    "  Estimates only. Verify current pricing at https://aws.amazon.com/elasticache/pricing/";

const DIVISION_BY_ZERO: &str = "float division by zero";

fn rounded(value: f64, digits: usize) -> Json {
    Json::Float(round_to(value, digits))
}

/// `(lease, purchase option)` of an `--ri-term` choice.
#[must_use]
pub fn ri_term_parts(term: &str) -> Option<(&'static str, &'static str)> {
    match term {
        "1yr_no_upfront" => Some(("1yr", "No Upfront")),
        "1yr_partial_upfront" => Some(("1yr", "Partial Upfront")),
        "1yr_all_upfront" => Some(("1yr", "All Upfront")),
        "3yr_no_upfront" => Some(("3yr", "No Upfront")),
        "3yr_partial_upfront" => Some(("3yr", "Partial Upfront")),
        "3yr_all_upfront" => Some(("3yr", "All Upfront")),
        _ => None,
    }
}

/// Serverless estimate as the JSON object the script printed.
///
/// # Errors
///
/// A missing storage or ECPU price (Python `ValueError`).
pub fn estimate_serverless(
    loader: &PricingLoader,
    engine: &str,
    region: &str,
    data_gb: f64,
    ecpu_millions_per_month: f64,
    avg_payload_kb: f64,
) -> Result<Vec<(String, Json)>, String> {
    let storage_rate = loader.get_serverless_storage_rate(region, engine)?;
    let ecpu_rate = loader.get_serverless_ecpu_rate(region, engine)?;
    let min_gb = if engine == "valkey" { 0.1 } else { 1.0 };
    let billing_gb = if min_gb > data_gb { min_gb } else { data_gb };
    let storage = billing_gb * storage_rate * HOURS_PER_MONTH;
    let payload_scale = if avg_payload_kb > 1.0 {
        avg_payload_kb
    } else {
        1.0
    };
    let scaled_ecpus = ecpu_millions_per_month * payload_scale;
    let compute = scaled_ecpus * 1_000_000.0 * ecpu_rate;
    let mut result = vec![
        kv("model", js("serverless")),
        kv("engine", js(engine)),
        kv("region", js(region)),
        kv("data_gb", Json::Float(data_gb)),
        kv("ecpu_millions", Json::Float(ecpu_millions_per_month)),
        kv("storage_monthly", rounded(storage, 2)),
        kv("compute_monthly", rounded(compute, 2)),
        kv("total_monthly", rounded(storage + compute, 2)),
    ];
    if avg_payload_kb != 1.0 {
        result.push(kv("avg_payload_kb", Json::Float(avg_payload_kb)));
        result.push(kv("ecpu_millions_scaled", rounded(scaled_ecpus, 1)));
    }
    Ok(result)
}

/// Node-based on-demand estimate (with an optional reserved comparison).
///
/// A missing on-demand price is reported inside the result as `{"error": ...}`
/// like the script; the `Err` case is a failure the script did not catch.
///
/// # Errors
///
/// Division by zero or an infinite discount (zero nodes).
pub fn estimate_node_based(
    loader: &PricingLoader,
    engine: &str,
    region: &str,
    node_type: &str,
    node_count: i64,
    ri_term: Option<&str>,
) -> Result<Vec<(String, Json)>, String> {
    let hourly = match loader.get_node_hourly_rate(region, node_type, engine) {
        Ok(rate) => rate,
        Err(message) => return Ok(vec![kv("error", Json::String(message))]),
    };
    let count = node_count as f64;
    let monthly_per_node = hourly * HOURS_PER_MONTH;
    let total = monthly_per_node * count;
    let memory = loader.get_node_memory_gb(node_type);

    let mut result = vec![
        kv("model", js("node-based (on-demand)")),
        kv("engine", js(engine)),
        kv("region", js(region)),
        kv("node_type", js(node_type)),
        kv("node_count", Json::Int(node_count)),
        kv(
            "memory_per_node_gb",
            if memory > 0.0 {
                Json::Float(memory)
            } else {
                js("unknown")
            },
        ),
        kv(
            "total_memory_gb",
            if memory > 0.0 {
                rounded(memory * count, 1)
            } else {
                js("unknown")
            },
        ),
        kv("per_node_monthly", rounded(monthly_per_node, 2)),
        kv("total_monthly", rounded(total, 2)),
    ];

    let Some((lease, purchase)) = ri_term.and_then(ri_term_parts) else {
        return Ok(result);
    };
    let Ok(eff_monthly) =
        loader.get_reserved_effective_monthly(region, node_type, engine, lease, purchase)
    else {
        return Ok(result);
    };
    let ri_total = eff_monthly * count;
    if total == 0.0 {
        return Err(DIVISION_BY_ZERO.to_string());
    }
    match round_int((1.0 - ri_total / total) * 100.0) {
        Ok(discount_pct) => {
            result.push(kv(
                "reserved",
                Json::Object(vec![
                    kv("term", js(ri_term.unwrap_or_default())),
                    kv("discount_pct", Json::Int(discount_pct)),
                    kv("per_node_monthly", Json::Float(eff_monthly)),
                    kv("total_monthly", rounded(ri_total, 2)),
                    kv("annual_savings", rounded((total - ri_total) * 12.0, 2)),
                    kv("source", js("live")),
                ]),
            ));
            Ok(result)
        }
        Err(RoundError::Nan) => Ok(result),
        Err(other) => Err(other.message().to_string()),
    }
}

/// The reserved-options report (`--show-ri-options`).
///
/// # Errors
///
/// A missing on-demand price or a zero-node on-demand total.
pub fn format_ri_options(
    loader: &PricingLoader,
    engine: &str,
    region: &str,
    node_type: &str,
    node_count: i64,
) -> Result<String, String> {
    let options = loader.list_reserved_options(region, node_type, engine);
    let hourly = loader.get_node_hourly_rate(region, node_type, engine)?;
    let count = node_count as f64;
    let od_monthly = hourly * HOURS_PER_MONTH * count;

    let mut lines: Vec<String> = Vec::new();
    lines.push("=".repeat(68));
    lines.push("Reserved Pricing Options (live pricing)".to_string());
    lines.push("=".repeat(68));
    lines.push(String::new());
    lines.push(format!(
        "  Node type: {}  x {}  ({} engine)",
        show(node_type),
        node_count,
        show(engine)
    ));
    lines.push(format!(
        "  On-demand: ${}/month",
        fmt_grouped(od_monthly, 2)
    ));
    lines.push(String::new());

    if options.is_empty() {
        lines.push("  No reserved pricing options found for this configuration.".to_string());
        lines.push(String::new());
        lines.push(PRICING_DISCLAIMER.to_string());
        lines.push(String::new());
        return Ok(lines.join("\n"));
    }

    lines.push(format!(
        "  {} {} {} {} {}",
        pad_right("Term", 10),
        pad_right("Purchase Option", 18),
        pad_left("Monthly", 12),
        pad_left("Upfront", 10),
        pad_left("Savings", 10)
    ));
    lines.push(format!(
        "  {} {} {} {} {}",
        "-".repeat(10),
        "-".repeat(18),
        "-".repeat(12),
        "-".repeat(10),
        "-".repeat(10)
    ));

    for option in &options {
        let eff_total = option.effective_monthly * count;
        if od_monthly == 0.0 {
            return Err(DIVISION_BY_ZERO.to_string());
        }
        let savings_pct = round_int((1.0 - eff_total / od_monthly) * 100.0)
            .map_err(|error| error.message().to_string())?;
        let upfront_total = option.upfront * count;
        lines.push(format!(
            "  {} {} ${} ${} {}%",
            pad_right(&show(&option.lease_length), 10),
            pad_right(&show(&option.purchase_option), 18),
            pad_left(&fmt_grouped(eff_total, 2), 9),
            pad_left(&fmt_grouped(upfront_total, 0), 8),
            pad_left(&savings_pct.to_string(), 8)
        ));
    }

    lines.push(String::new());
    lines.push(PRICING_DISCLAIMER.to_string());
    lines.push(String::new());
    Ok(lines.join("\n"))
}

/// Extended Support cost of one year tier.
#[derive(Debug, Clone, PartialEq)]
pub struct YearCost {
    /// Year after end of standard support (1, 2 or 3).
    pub year: i64,
    /// Surcharge per node-hour.
    pub per_node_hourly: f64,
    /// Surcharge for all nodes per month.
    pub total_monthly: f64,
    /// Surcharge for all nodes per year.
    pub total_annual: f64,
}

/// Extended Support estimate for a node type.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedSupport {
    /// Engine as requested.
    pub engine: String,
    /// Region as requested.
    pub region: String,
    /// Node type as requested.
    pub node_type: String,
    /// Number of nodes.
    pub node_count: i64,
    /// One entry per published year tier.
    pub years: Vec<YearCost>,
    /// Why there are no years, when no surcharge data was loaded.
    pub note: Option<String>,
    /// On-demand cost without the surcharge.
    pub base_monthly: Option<f64>,
    /// Base cost plus the first listed year's surcharge.
    pub total_with_surcharge_monthly: Option<f64>,
    /// Advice printed under the table.
    pub recommendation: Option<String>,
}

/// Extended Support surcharge estimate (never fails; gaps are left out).
#[must_use]
pub fn estimate_extended_support(
    loader: &PricingLoader,
    engine: &str,
    region: &str,
    node_type: &str,
    node_count: i64,
) -> ExtendedSupport {
    let mut results = ExtendedSupport {
        engine: engine.to_string(),
        region: region.to_string(),
        node_type: node_type.to_string(),
        node_count,
        years: Vec::new(),
        note: None,
        base_monthly: None,
        total_with_surcharge_monthly: None,
        recommendation: None,
    };
    if !loader.has_extended_support_pricing() {
        results.note = Some(
            "Extended Support pricing not available in bulk pricing API. Check the ElastiCache pricing page."
                .to_string(),
        );
        return results;
    }
    let count = node_count as f64;
    for (year, number) in [("1", 1_i64), ("2", 2), ("3", 3)] {
        if let Ok(hourly) = loader.get_extended_support_rate(region, node_type, engine, year) {
            let monthly = hourly * HOURS_PER_MONTH * count;
            results.years.push(YearCost {
                year: number,
                per_node_hourly: hourly,
                total_monthly: round_to(monthly, 2),
                total_annual: round_to(monthly * 12.0, 2),
            });
        }
    }
    if let Some(first) = results.years.first() {
        let first_monthly = first.total_monthly;
        if let Ok(base_hourly) = loader.get_node_hourly_rate(region, node_type, engine) {
            let base_monthly = round_to(base_hourly * HOURS_PER_MONTH * count, 2);
            results.base_monthly = Some(base_monthly);
            results.total_with_surcharge_monthly = Some(round_to(base_monthly + first_monthly, 2));
        }
        results.recommendation = Some(
            "Migrating to Valkey eliminates Extended Support charges. Savings vs other engines: 20% lower on node-based, 33% lower on serverless."
                .to_string(),
        );
    }
    results
}

/// The human-readable Extended Support report.
#[must_use]
pub fn format_extended_support_report(results: &ExtendedSupport) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("=".repeat(60));
    lines.push("Extended Support Cost Estimate (live pricing)".to_string());
    lines.push("=".repeat(60));
    lines.push(String::new());
    lines.push(format!("  Engine: {}", show(&results.engine)));
    lines.push(format!("  Region: {}", show(&results.region)));
    lines.push(format!("  Node type: {}", show(&results.node_type)));
    lines.push(format!("  Nodes:  {}", results.node_count));
    lines.push(String::new());

    if results.years.is_empty() {
        lines.push(format!(
            "  {}",
            results
                .note
                .as_deref()
                .unwrap_or("No pricing data available.")
        ));
        lines.push(String::new());
        return lines.join("\n");
    }

    lines.push(format!(
        "  {} {} {}",
        pad_right("Year After EOL", 18),
        pad_left("Monthly Cost", 16),
        pad_left("Annual Cost", 16)
    ));
    lines.push(format!(
        "  {} {} {}",
        "-".repeat(18),
        "-".repeat(16),
        "-".repeat(16)
    ));
    for year in &results.years {
        lines.push(format!(
            "  Year {} ${} ${}",
            pad_right(&year.year.to_string(), 13),
            pad_left(&fmt_grouped(year.total_monthly, 2), 13),
            pad_left(&fmt_grouped(year.total_annual, 2), 13)
        ));
    }

    if let Some(base) = results.base_monthly.filter(|base| *base != 0.0) {
        lines.push(String::new());
        lines.push(format!(
            "  Base node cost:       ${}/month",
            pad_left(&fmt_grouped(base, 2), 13)
        ));
        let total = results.total_with_surcharge_monthly.unwrap_or(base);
        lines.push(format!(
            "  Total (base + Yr1):   ${}/month",
            pad_left(&fmt_grouped(total, 2), 13)
        ));
    }

    lines.push(String::new());
    if let Some(recommendation) = &results.recommendation {
        lines.push(format!("  {recommendation}"));
    }
    lines.push(String::new());
    lines.push(PRICING_DISCLAIMER.to_string());
    lines.push(String::new());
    lines.join("\n")
}

/// Monthly ECPUs, in millions, for an operations-per-second rate.
#[must_use]
pub fn estimate_ecpu_from_ops(ops_per_sec: f64, avg_ecpu_per_op: f64) -> f64 {
    let ecpu_per_month = ops_per_sec * avg_ecpu_per_op * 3600.0 * HOURS_PER_MONTH;
    round_to(ecpu_per_month / 1_000_000.0, 1)
}
