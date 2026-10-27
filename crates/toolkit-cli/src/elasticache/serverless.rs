//! `toolkit elasticache estimate-serverless` (port of
//! `serverless_estimator.py`): what provisioned clusters would cost on
//! ElastiCache Serverless.
//!
//! Two ECPU modes, both heuristic approximations (AWS publishes no exact
//! formula):
//!
//! - **simple**: every command costs `max(1, avg_payload_kb)` ECPUs;
//! - **detailed**: per-command stats are classified (see
//!   [`super::classifier`]) and the higher of the CPU and the network
//!   component is billed.
//!
//! Bursty workloads are modeled with a daily sine wave sampled at 24 points.

use std::collections::HashMap;

use toolkit_core::Json;

use crate::aurora::api::parse_csv_rows;
use crate::aurora::args::{parse_py_float, parse_py_int};
use crate::aurora::pyfmt::{fmt_f, fmt_grouped, round_num, round_to, Num};

use super::classifier::{estimate_ecpus_from_commandstats, CommandStat};
use super::pricing::{PricingLoader, HOURS_PER_MONTH};
use super::text::{
    js, kv, pad_left, pad_right, py_sum, quoted, round_int, show, trunc_int, RoundError,
};

/// Samples of the daily sine wave (one per hour).
const SIN_SAMPLES: usize = 24;

/// Columns of the CSV report.
pub const CSV_FIELDS: &[&str] = &[
    "cluster_name",
    "region",
    "engine",
    "instance_type",
    "node_count",
    "primary_nodes",
    "current_monthly_cost",
    "cost_source",
    "avg_memory_gb",
    "billing_memory_gb",
    "monthly_gb_hours",
    "storage_cost",
    "monthly_ecpus",
    "ecpu_cost",
    "ecpu_mode",
    "serverless_total",
    "diff",
    "diff_pct",
    "notes",
];

/// One provisioned cluster read from the input CSV.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Name used to join the commandstats rows.
    pub cluster_name: String,
    /// Node type, empty when unknown.
    pub instance_type: String,
    /// AWS region.
    pub region: String,
    /// `valkey`, `redis` or `memcached`.
    pub engine: String,
    /// Nodes in the cluster.
    pub node_count: i64,
    /// Primary nodes (heuristic when not given).
    pub primary_nodes: i64,
    /// Average dataset memory in GB.
    pub avg_memory_gb: f64,
    /// Average commands per day.
    pub daily_commands: f64,
    /// Known monthly cost; `0` means "price it from the list".
    pub current_monthly_cost: f64,
    /// Average payload in KB (`1.0` default).
    pub avg_payload_kb: f64,
    /// Peak commands per day (bursty model).
    pub peak_commands: f64,
    /// Minimum commands per day (bursty model).
    pub min_commands: f64,
    /// Peak-to-average command ratio (bursty model).
    pub peak_to_avg_ratio: f64,
    /// Peak memory in GB (bursty model).
    pub peak_memory_gb: f64,
    /// Minimum memory in GB (bursty model).
    pub min_memory_gb: f64,
}

/// Per-command stats of one cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStats {
    /// Whether the numbers were divided by `uptime_days`.
    pub normalized: bool,
    /// Commands in file order.
    pub commands: Vec<(String, CommandStat)>,
}

fn row_text<'a>(row: &'a HashMap<String, String>, key: &str, default: &'a str) -> &'a str {
    row.get(key).map_or(default, String::as_str)
}

fn int_field(
    row: &HashMap<String, String>,
    key: &str,
    default: i64,
    number: usize,
) -> Result<i64, String> {
    match row.get(key) {
        None => Ok(default),
        Some(text) => parse_py_int(text).ok_or_else(|| {
            format!(
                "row {number}: invalid literal for int() with base 10: {}",
                quoted(text)
            )
        }),
    }
}

fn parse_float(text: &str, number: usize) -> Result<f64, String> {
    parse_py_float(text).ok_or_else(|| {
        format!(
            "row {number}: could not convert string to float: {}",
            quoted(text)
        )
    })
}

/// `float(row.get(key, default))`: a present value must parse.
fn float_field(
    row: &HashMap<String, String>,
    key: &str,
    default: f64,
    number: usize,
) -> Result<f64, String> {
    match row.get(key) {
        None => Ok(default),
        Some(text) => parse_float(text, number),
    }
}

/// `float(row.get(key, default) or default)`: an empty value is the default.
fn float_or(
    row: &HashMap<String, String>,
    key: &str,
    default: f64,
    number: usize,
) -> Result<f64, String> {
    match row.get(key) {
        Some(text) if !text.is_empty() => parse_float(text, number),
        _ => Ok(default),
    }
}

fn required<'a>(
    row: &'a HashMap<String, String>,
    key: &str,
    number: usize,
) -> Result<&'a str, String> {
    row.get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("row {number}: missing the required column '{key}'"))
}

/// Loads the cluster CSV.
///
/// Required columns: `cluster_name`; `node_count`, `avg_memory_gb` and
/// `daily_commands` default to `1`, `0` and `0` when the column is absent.
/// See the migration notes for the optional columns.
///
/// # Errors
///
/// A missing `cluster_name` column or a number that does not parse.
pub fn load_clusters(text: &str) -> Result<Vec<Cluster>, String> {
    let mut clusters = Vec::new();
    for (index, row) in parse_csv_rows(text).iter().enumerate() {
        let number = index + 1;
        let node_count = int_field(row, "node_count", 1, number)?;
        let mut primary_nodes = int_field(row, "primary_nodes", 0, number)?;
        if primary_nodes == 0 {
            // A rough heuristic: half of the nodes are primaries. Supply the
            // primary_nodes column for an accurate count.
            primary_nodes = std::cmp::max(1, node_count.div_euclid(2));
        }
        clusters.push(Cluster {
            cluster_name: required(row, "cluster_name", number)?.trim().to_string(),
            instance_type: row_text(row, "instance_type", "").trim().to_string(),
            region: row_text(row, "region", "us-east-1").trim().to_string(),
            engine: row_text(row, "engine", "valkey").trim().to_lowercase(),
            node_count,
            primary_nodes,
            avg_memory_gb: float_field(row, "avg_memory_gb", 0.0, number)?,
            daily_commands: float_field(row, "daily_commands", 0.0, number)?,
            current_monthly_cost: float_or(row, "current_monthly_cost", 0.0, number)?,
            avg_payload_kb: float_or(row, "avg_payload_kb", 1.0, number)?,
            peak_commands: float_or(row, "peak_commands", 0.0, number)?,
            min_commands: float_or(row, "min_commands", 0.0, number)?,
            peak_to_avg_ratio: float_or(row, "peak_to_avg_ratio", 0.0, number)?,
            peak_memory_gb: float_or(row, "peak_memory_gb", 0.0, number)?,
            min_memory_gb: float_or(row, "min_memory_gb", 0.0, number)?,
        });
    }
    Ok(clusters)
}

/// Parses a commandstats CSV (`cluster_name,command,calls,usec[,uptime_days]`)
/// into per-cluster stats in order of first appearance. With `uptime_days`
/// the numbers are divided by it to get a daily rate.
///
/// # Errors
///
/// A missing column, a number that does not parse or a daily rate that is not
/// a finite integer.
pub fn parse_commandstats_file(text: &str) -> Result<Vec<(String, CommandStats)>, String> {
    let mut result: Vec<(String, CommandStats)> = Vec::new();
    for (index, row) in parse_csv_rows(text).iter().enumerate() {
        let number = index + 1;
        let cluster = required(row, "cluster_name", number)?.trim().to_string();
        let command = required(row, "command", number)?.trim().to_lowercase();
        let mut calls = int_field(row, "calls", 0, number)?;
        let mut usec = int_field(row, "usec", 0, number)?;
        let uptime_raw = row_text(row, "uptime_days", "").trim();
        let uptime_days = if uptime_raw.is_empty() {
            None
        } else {
            Some(parse_float(uptime_raw, number)?)
        };
        let mut normalized = false;
        if let Some(uptime) = uptime_days.filter(|days| *days > 0.0) {
            let daily = |count: i64| -> Result<i64, String> {
                trunc_int(count as f64 / uptime)
                    .map_err(|error: RoundError| format!("row {number}: {}", error.message()))
            };
            calls = daily(calls)?;
            usec = daily(usec)?;
            normalized = true;
        }
        let position = result.iter().position(|(name, _)| *name == cluster);
        let entry = match position {
            Some(at) => &mut result[at].1,
            None => {
                result.push((
                    cluster,
                    CommandStats {
                        normalized,
                        commands: Vec::new(),
                    },
                ));
                let last = result.len() - 1;
                &mut result[last].1
            }
        };
        if let Some(slot) = entry.commands.iter_mut().find(|(name, _)| *name == command) {
            slot.1 = CommandStat { calls, usec };
        } else {
            entry.commands.push((command, CommandStat { calls, usec }));
        }
        if normalized {
            entry.normalized = true;
        }
    }
    Ok(result)
}

/// Parses raw `INFO commandstats` text
/// (`cmdstat_get:calls=1000,usec=1500,usec_per_call=1.50,...`).
#[must_use]
pub fn parse_commandstats_info(text: &str) -> Vec<(String, CommandStat)> {
    let mut result: Vec<(String, CommandStat)> = Vec::new();
    for line in text.trim().lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("cmdstat_") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(rest) = rest[name.len()..].strip_prefix(":calls=") else {
            continue;
        };
        let calls_text: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let Some(rest) = rest[calls_text.len()..].strip_prefix(",usec=") else {
            continue;
        };
        let usec_text: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let (Ok(calls), Ok(usec)) = (calls_text.parse::<i64>(), usec_text.parse::<i64>()) {
            let command = name.to_lowercase();
            if let Some(slot) = result.iter_mut().find(|(known, _)| *known == command) {
                slot.1 = CommandStat { calls, usec };
            } else {
                result.push((command, CommandStat { calls, usec }));
            }
        }
    }
    result
}

/// `sample(t) = min + (max - min) * (1 + sin(2 pi t / n)) / 2`, written as the
/// script does: `midpoint + amplitude * sin(...)`.
fn sine_wave_samples(min_val: f64, max_val: f64) -> Vec<f64> {
    let amplitude = (max_val - min_val) / 2.0;
    let midpoint = (max_val + min_val) / 2.0;
    (0..SIN_SAMPLES)
        .map(|index| {
            midpoint
                + amplitude
                    * (2.0 * std::f64::consts::PI * (index as f64) / (SIN_SAMPLES as f64)).sin()
        })
        .collect()
}

/// Cost multiplier for a bursty workload: the sine-wave average over the
/// customer-supplied average. `1.0` when no burst is described or the
/// workload is flat.
#[must_use]
pub fn compute_burst_multiplier(
    avg_val: f64,
    min_val: Option<f64>,
    max_val: Option<f64>,
    peak_to_avg: Option<f64>,
) -> f64 {
    let (mut min_val, mut max_val) = (min_val, max_val);
    if let Some(ratio) = peak_to_avg.filter(|ratio| *ratio > 1.0) {
        max_val = Some(avg_val * ratio);
        let floor = avg_val * (2.0 - ratio);
        min_val = Some(if floor > 0.0 { floor } else { 0.0 });
    }
    let (Some(min_val), Some(max_val)) = (min_val, max_val) else {
        return 1.0;
    };
    if max_val <= min_val || max_val <= 0.0 || avg_val <= 0.0 {
        return 1.0;
    }
    let samples = sine_wave_samples(min_val, max_val);
    let sine_avg = py_sum(&samples) / (samples.len() as f64);
    sine_avg / avg_val
}

/// `Some(value)` unless it is zero (Python `value or None`).
fn nonzero(value: f64) -> Option<f64> {
    if value == 0.0 {
        None
    } else {
        Some(value)
    }
}

fn min_storage_gb(engine: &str) -> f64 {
    if engine == "valkey" {
        0.1
    } else {
        1.0
    }
}

/// The estimate for one cluster, rounded the way the script rounds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterResult {
    pub cluster_name: String,
    pub region: String,
    pub engine: String,
    pub instance_type: String,
    pub node_count: i64,
    pub primary_nodes: i64,
    pub current_monthly_cost: Num,
    pub cost_source: &'static str,
    pub avg_memory_gb: f64,
    pub billing_memory_gb: f64,
    pub monthly_gb_hours: f64,
    pub storage_rate_per_gb_hour: f64,
    pub storage_cost: f64,
    pub monthly_ecpus: i64,
    pub ecpu_rate_per_million: f64,
    pub ecpu_cost: f64,
    pub ecpu_mode: &'static str,
    pub fixed_ecpus: Option<i64>,
    pub nonfixed_ecpus: Option<i64>,
    pub internal_calls_excluded: Option<i64>,
    pub serverless_total: f64,
    pub diff: Option<f64>,
    pub diff_pct: Option<f64>,
    pub notes: String,
}

fn opt_int(value: Option<i64>) -> Json {
    value.map_or(Json::Null, Json::Int)
}

fn opt_float(value: Option<f64>) -> Json {
    value.map_or(Json::Null, Json::Float)
}

impl ClusterResult {
    /// The JSON object (`--json`), keys in the script's order.
    pub(crate) fn to_json(&self) -> Json {
        Json::Object(vec![
            kv("cluster_name", js(&self.cluster_name)),
            kv("region", js(&self.region)),
            kv("engine", js(&self.engine)),
            kv("instance_type", js(&self.instance_type)),
            kv("node_count", Json::Int(self.node_count)),
            kv("primary_nodes", Json::Int(self.primary_nodes)),
            kv("current_monthly_cost", self.current_monthly_cost.to_json()),
            kv("cost_source", js(self.cost_source)),
            kv("avg_memory_gb", Json::Float(self.avg_memory_gb)),
            kv("billing_memory_gb", Json::Float(self.billing_memory_gb)),
            kv("monthly_gb_hours", Json::Float(self.monthly_gb_hours)),
            kv(
                "storage_rate_per_gb_hour",
                Json::Float(self.storage_rate_per_gb_hour),
            ),
            kv("storage_cost", Json::Float(self.storage_cost)),
            kv("monthly_ecpus", Json::Int(self.monthly_ecpus)),
            kv(
                "ecpu_rate_per_million",
                Json::Float(self.ecpu_rate_per_million),
            ),
            kv("ecpu_cost", Json::Float(self.ecpu_cost)),
            kv("ecpu_mode", js(self.ecpu_mode)),
            kv("fixed_ecpus", opt_int(self.fixed_ecpus)),
            kv("nonfixed_ecpus", opt_int(self.nonfixed_ecpus)),
            kv(
                "internal_calls_excluded",
                opt_int(self.internal_calls_excluded),
            ),
            kv("serverless_total", Json::Float(self.serverless_total)),
            kv("diff", opt_float(self.diff)),
            kv("diff_pct", opt_float(self.diff_pct)),
            kv("notes", js(&self.notes)),
        ])
    }

    /// The cells of one CSV row, in [`CSV_FIELDS`] order.
    pub(crate) fn csv_cells(&self) -> Vec<String> {
        let float = |value: f64| Num::Float(value).py_str();
        let optional = |value: Option<f64>| value.map_or_else(String::new, float);
        vec![
            self.cluster_name.clone(),
            self.region.clone(),
            self.engine.clone(),
            self.instance_type.clone(),
            self.node_count.to_string(),
            self.primary_nodes.to_string(),
            self.current_monthly_cost.py_str(),
            self.cost_source.to_string(),
            float(self.avg_memory_gb),
            float(self.billing_memory_gb),
            float(self.monthly_gb_hours),
            float(self.storage_cost),
            self.monthly_ecpus.to_string(),
            float(self.ecpu_cost),
            self.ecpu_mode.to_string(),
            float(self.serverless_total),
            optional(self.diff),
            optional(self.diff_pct),
            self.notes.clone(),
        ]
    }
}

fn convert(error: RoundError) -> String {
    error.message().to_string()
}

/// Estimates the serverless cost of one cluster. `warnings` receives the lines
/// the script printed on stderr.
///
/// # Errors
///
/// A missing serverless price or a total that is not a finite number.
pub(crate) fn estimate_cluster(
    cluster: &Cluster,
    pricing: &PricingLoader,
    commandstats: Option<&CommandStats>,
    warnings: &mut Vec<String>,
) -> Result<ClusterResult, String> {
    let region = cluster.region.as_str();
    let engine = cluster.engine.as_str();
    let node_count = cluster.node_count;
    let instance_type = cluster.instance_type.as_str();

    // Current provisioned cost.
    let (current_cost, cost_source) = if cluster.current_monthly_cost > 0.0 {
        (Num::Float(cluster.current_monthly_cost), "provided")
    } else if instance_type.is_empty() {
        (Num::Int(0), "unknown")
    } else {
        match pricing.get_node_hourly_rate(region, instance_type, engine) {
            Ok(hourly) => (
                Num::Float(hourly * HOURS_PER_MONTH * (node_count as f64)),
                "list_price",
            ),
            Err(_) => (Num::Int(0), "unknown"),
        }
    };

    // Serverless storage.
    let avg_memory_gb = cluster.avg_memory_gb;
    let min_gb = min_storage_gb(engine);
    let billing_memory_gb = if min_gb > avg_memory_gb {
        min_gb
    } else {
        avg_memory_gb
    };
    let used_minimum = avg_memory_gb < min_gb;
    let storage_burst = compute_burst_multiplier(
        avg_memory_gb,
        nonzero(cluster.min_memory_gb),
        nonzero(cluster.peak_memory_gb),
        None,
    );
    let adjusted_memory_gb = billing_memory_gb * storage_burst;
    let monthly_gb_hours = adjusted_memory_gb * HOURS_PER_MONTH;
    let storage_rate = pricing.get_serverless_storage_rate(region, engine)?;
    let storage_cost = monthly_gb_hours * storage_rate;

    // Serverless ECPUs.
    let ecpu_rate = pricing.get_serverless_ecpu_rate(region, engine)?;
    let ecpu_burst = compute_burst_multiplier(
        cluster.daily_commands,
        nonzero(cluster.min_commands),
        nonzero(cluster.peak_commands),
        nonzero(cluster.peak_to_avg_ratio),
    );

    let mut stats = commandstats;
    if stats.is_some() && engine == "memcached" {
        warnings.push(format!(
            "WARNING: commandstats mode uses Redis/Valkey INFO format, which is incompatible with Memcached. ECPU estimates for Memcached cluster '{}' will be inaccurate. Falling back to simple estimation.",
            show(&cluster.cluster_name)
        ));
        stats = None;
    }

    let avg_payload_kb = if cluster.avg_payload_kb == 0.0 {
        1.0
    } else {
        cluster.avg_payload_kb
    };
    let ecpu_per_request = if avg_payload_kb > 1.0 {
        avg_payload_kb
    } else {
        1.0
    };

    let mut normalized = true;
    let (monthly_ecpus, ecpu_mode, fixed_ecpus, nonfixed_ecpus, internal_calls) =
        if let Some(stats) = stats {
            normalized = stats.normalized;
            let estimate = estimate_ecpus_from_commandstats(&stats.commands).map_err(convert)?;
            let daily_cpu_ecpus = estimate.total_ecpus as f64;
            // Serverless bills the higher of vCPU time and data transferred.
            let daily_net_ecpus = cluster.daily_commands * ecpu_per_request;
            let daily_ecpus = if daily_net_ecpus > daily_cpu_ecpus {
                daily_net_ecpus
            } else {
                daily_cpu_ecpus
            };
            (
                daily_ecpus * 30.0 * ecpu_burst,
                "detailed",
                Some(round_int(estimate.fixed_ecpus as f64 * 30.0).map_err(convert)?),
                Some(round_int(estimate.nonfixed_ecpus as f64 * 30.0).map_err(convert)?),
                Some(round_int(estimate.internal_calls as f64 * 30.0).map_err(convert)?),
            )
        } else {
            (
                cluster.daily_commands * 30.0 * ecpu_per_request * ecpu_burst,
                "simple",
                None,
                None,
                None,
            )
        };

    let ecpu_cost = monthly_ecpus * ecpu_rate;
    let total_serverless = storage_cost + ecpu_cost;

    // Comparison.
    let current_value = current_cost.value();
    let diff = if current_value > 0.0 {
        Some(total_serverless - current_value)
    } else {
        None
    };
    let diff_pct = diff.map(|change| change / current_value * 100.0);

    // Notes.
    let mut notes: Vec<String> = Vec::new();
    if used_minimum {
        notes.push(format!(
            "Min storage applied ({} GB)",
            Num::Float(min_gb).py_str()
        ));
    }
    if cost_source == "list_price" {
        notes.push("Current cost: on-demand list price".to_string());
    }
    if cost_source == "unknown" {
        notes.push(
            "Current cost unknown - provide current_monthly_cost or instance_type".to_string(),
        );
    }
    if ecpu_mode == "simple" {
        if avg_payload_kb > 1.0 {
            notes.push(format!(
                "ECPU: {} ECPUs per request ({} KB avg payload)",
                Num::Float(round_to(ecpu_per_request, 1)).py_str(),
                Num::Float(round_to(avg_payload_kb, 1)).py_str()
            ));
        } else {
            notes.push("ECPU: 1 cmd = 1 ECPU (provide avg_payload_kb for better accuracy)".into());
        }
    }
    if stats.is_some() && !normalized {
        notes.push(
            "Commandstats not normalized by uptime; provide uptime_days column for accuracy"
                .to_string(),
        );
    }
    if ecpu_burst > 1.0 {
        notes.push(format!(
            "Burst adjustment applied: {}x (peak/avg commands)",
            fmt_f(ecpu_burst, 2)
        ));
    }
    if storage_burst > 1.0 {
        notes.push(format!(
            "Burst adjustment applied: {}x (peak/avg memory)",
            fmt_f(storage_burst, 2)
        ));
    }
    if engine == "redis" || engine == "valkey" {
        notes.push("Serverless requires cluster-mode-enabled clients and TLS; Global Data Store and Data Tiering are not supported".to_string());
    }

    Ok(ClusterResult {
        cluster_name: cluster.cluster_name.clone(),
        region: region.to_string(),
        engine: engine.to_string(),
        instance_type: instance_type.to_string(),
        node_count,
        primary_nodes: cluster.primary_nodes,
        current_monthly_cost: round_num(current_cost, 2),
        cost_source,
        avg_memory_gb: round_to(avg_memory_gb, 4),
        billing_memory_gb: round_to(billing_memory_gb, 4),
        monthly_gb_hours: round_to(monthly_gb_hours, 2),
        storage_rate_per_gb_hour: storage_rate,
        storage_cost: round_to(storage_cost, 2),
        monthly_ecpus: round_int(monthly_ecpus).map_err(convert)?,
        ecpu_rate_per_million: round_to(ecpu_rate * 1_000_000.0, 4),
        ecpu_cost: round_to(ecpu_cost, 2),
        ecpu_mode,
        fixed_ecpus,
        nonfixed_ecpus,
        internal_calls_excluded: internal_calls,
        serverless_total: round_to(total_serverless, 2),
        diff: diff.map(|change| round_to(change, 2)),
        diff_pct: diff_pct.map(|pct| round_to(pct, 1)),
        notes: notes.join("; "),
    })
}

fn csv_cell(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// The CSV report: a header and one row per result, `\r\n` terminated like
/// Python's `csv.DictWriter`.
pub(crate) fn csv_report(results: &[ClusterResult]) -> String {
    let mut out = CSV_FIELDS.join(",");
    out.push_str("\r\n");
    for result in results {
        let cells: Vec<String> = result
            .csv_cells()
            .iter()
            .map(|cell| csv_cell(cell))
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

/// `{:+.0f}`.
fn signed_whole(value: f64) -> String {
    let text = fmt_f(value, 0);
    if text.starts_with('-') {
        text
    } else {
        format!("+{text}")
    }
}

/// The console summary, one line per entry.
pub(crate) fn summary_lines(results: &[ClusterResult]) -> Vec<String> {
    let currents: Vec<f64> = results
        .iter()
        .map(|result| result.current_monthly_cost.value())
        .collect();
    let storage: Vec<f64> = results.iter().map(|result| result.storage_cost).collect();
    let ecpu: Vec<f64> = results.iter().map(|result| result.ecpu_cost).collect();
    let totals: Vec<f64> = results
        .iter()
        .map(|result| result.serverless_total)
        .collect();
    let total_current = py_sum(&currents);
    let total_sl = py_sum(&totals);
    let total_stor = py_sum(&storage);
    let total_ecpu = py_sum(&ecpu);
    let cheaper = results
        .iter()
        .filter(|result| result.diff.is_some_and(|diff| diff < 0.0))
        .count();
    let more_expensive = results
        .iter()
        .filter(|result| result.diff.is_some_and(|diff| diff > 0.0))
        .count();
    let money = |value: f64| pad_left(&fmt_grouped(value, 2), 12);

    let mut lines: Vec<String> = vec![
        String::new(),
        "=".repeat(65),
        "  ElastiCache Serverless Cost Estimate".to_string(),
        "=".repeat(65),
        format!("  Clusters analyzed:        {}", results.len()),
        format!(
            "  Total nodes:              {}",
            results.iter().map(|result| result.node_count).sum::<i64>()
        ),
        String::new(),
        format!(
            "  Current provisioned cost: ${} /month",
            money(total_current)
        ),
        format!("  Estimated serverless:     ${} /month", money(total_sl)),
        format!("    Data storage:           ${}", money(total_stor)),
        format!("    ECPUs:                  ${}", money(total_ecpu)),
        String::new(),
    ];
    if total_current > 0.0 {
        let savings = total_current - total_sl;
        let pct = savings / total_current * 100.0;
        lines.push(format!(
            "  Estimated savings:        ${} ({}%)",
            money(savings),
            fmt_f(pct, 1)
        ));
    }
    lines.push(format!("  Clusters cheaper on SL:   {cheaper}"));
    lines.push(format!("  Clusters more expensive:  {more_expensive}"));
    lines.push("=".repeat(65));
    lines.push(String::new());
    lines.push(format!(
        "  {} {} {} {}",
        pad_right("Cluster", 30),
        pad_left("Current", 10),
        pad_left("Serverless", 10),
        pad_left("Change", 8)
    ));
    lines.push(format!(
        "  {} {} {} {}",
        "-".repeat(30),
        "-".repeat(10),
        "-".repeat(10),
        "-".repeat(8)
    ));
    let mut ordered: Vec<&ClusterResult> = results.iter().collect();
    let sort_key = |result: &ClusterResult| {
        let diff = result.diff.unwrap_or(0.0);
        if diff == 0.0 {
            0.0
        } else {
            diff
        }
    };
    ordered.sort_by(|a, b| sort_key(a).total_cmp(&sort_key(b)));
    for result in ordered {
        let safe_name = show(&result.cluster_name);
        let name: String = safe_name.chars().take(30).collect();
        let current = format!("${}", fmt_grouped(result.current_monthly_cost.value(), 0));
        let serverless = format!("${}", fmt_grouped(result.serverless_total, 0));
        let change = result.diff_pct.map_or_else(
            || "N/A".to_string(),
            |pct| format!("{}%", signed_whole(pct)),
        );
        lines.push(format!(
            "  {} {} {} {}",
            pad_right(&name, 30),
            pad_left(&current, 10),
            pad_left(&serverless, 10),
            pad_left(&change, 8)
        ));
    }
    lines.push(String::new());
    lines
        .push("  ECPU estimates assume 1 KB avg payload unless avg_payload_kb is provided.".into());
    lines.push("  ECPUs scale linearly with payload size (e.g. 3 KB = 3x ECPUs).".to_string());
    lines.push(String::new());
    lines.push(
        "  IMPORTANT: These are approximate estimates. Actual serverless costs depend".to_string(),
    );
    lines.push(
        "  on real-time workload characteristics and may differ. For accurate billing,".to_string(),
    );
    lines.push("  use CloudWatch metrics on a running serverless cache.".to_string());
    lines.push("  See https://aws.amazon.com/elasticache/pricing/".to_string());
    lines.push(String::new());
    lines
}
