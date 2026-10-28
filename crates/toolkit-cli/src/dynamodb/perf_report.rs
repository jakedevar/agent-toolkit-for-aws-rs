//! Analysis half of `generate_perf_report.py` (Issue #4390): merging the
//! benchmark summary with the calculator, extracting the design signals and
//! classifying them into findings. The Markdown text is rendered by
//! [`super::perf_render`].
//!
//! No AWS call and no side effect: the inputs are the design JSON and the
//! `perf_summary.json` a benchmark run wrote, both read as files. The expected
//! numbers come from [`super::cost`], the same calculator the cost report uses.

use std::collections::BTreeMap;

use toolkit_core::{Json, ToolkitError};

use super::bench::{items, objects, Res};
use super::bench_fmt::{diag, diag_bounded, die, py_sum, Num, Tally};
use super::cost::{
    self, EntitySizes, BYTES_PER_GB, PAGE_CAP_KB, SECONDS_PER_MONTH, VECTOR_SEARCH_PRICE_PER_GB,
    VECTOR_WRITE_PRICE_PER_GB,
};
use super::pyfmt;

/// 10% deviation triggers flagging.
pub const TOLERANCE: f64 = 0.10;
/// Above this non-throttle error rate a pattern is structurally broken.
pub const ERROR_RATE_HIGH: f64 = 0.5;
/// `cc.PAGE_CAP_KB * 1024`.
pub const PAGE_CAP_BYTES: f64 = (PAGE_CAP_KB * 1024) as f64;

/// Error and cancellation codes that signal a benchmark artifact rather than a
/// design defect (contention on the small synthetic key space).
pub const ARTIFACT_ERROR_CODES: &[&str] = &[
    "TransactionConflict",
    "ConditionalCheckFailed",
    "ConditionalCheckFailedException",
];

/// The read operations of the calculator (`cc.READ_OPS`).
#[must_use]
pub fn is_read_op(op: &str) -> bool {
    matches!(
        op,
        "GetItem" | "Query" | "Scan" | "BatchGetItem" | "TransactGetItems"
    )
}

fn is_artifact_code(code: &str) -> bool {
    ARTIFACT_ERROR_CODES.contains(&code)
}

/// `_classify_error_codes`: `(kind, dominant_code, reason_histogram)` where the
/// kind is `artifact` or `structural`.
#[must_use]
pub fn classify_error_codes(error_codes: &Tally, reasons: &Tally) -> (&'static str, String, Tally) {
    if !reasons.is_empty() {
        let dominant = reasons.top().unwrap_or("unknown").to_string();
        let kind = if reasons.0.iter().all(|(code, _)| is_artifact_code(code)) {
            "artifact"
        } else {
            "structural"
        };
        return (kind, dominant, reasons.clone());
    }
    if error_codes.is_empty() {
        return ("structural", "unknown".to_string(), Tally::default());
    }
    let dominant = error_codes.top().unwrap_or("unknown").to_string();
    let kind = if error_codes.0.iter().all(|(code, _)| is_artifact_code(code)) {
        "artifact"
    } else {
        "structural"
    };
    (kind, dominant, Tally::default())
}

/// The hot/cold partition record of a representative-mode pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyDist {
    /// Distinct partitions hit.
    pub n_distinct_keys: Option<Json>,
    /// The hottest partition's share of the calls.
    pub top_key_share: Option<f64>,
    /// Standard deviation over the mean of the per-key counts.
    pub stddev_over_mean: f64,
    /// Hot-partition p99.
    pub hot_partition_p99_ms: Option<f64>,
    /// Cold-partition p99.
    pub cold_partition_p99_ms: Option<f64>,
}

/// The warmup block of a pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct ColdStart {
    /// Warmup p50.
    pub warmup_p50_ms: Option<f64>,
    /// Warmup p99.
    pub warmup_p99_ms: Option<f64>,
    /// Throttles during warmup.
    pub warmup_throttles: Json,
    /// Warmup p99 above twice the steady p99.
    pub cold_start_elevated: bool,
}

/// One pattern: what the run observed against what the calculator expected.
#[derive(Debug, Clone)]
pub struct Row<'a> {
    pub pattern_id: String,
    pub op: String,
    pub table: String,
    pub index: Option<String>,
    pub table_index: String,
    pub declared_peak_rps: Num,
    pub bench_rps: f64,
    pub observed_cu: f64,
    pub expected_cu: f64,
    pub delta_pct: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub throttles: i64,
    pub errors: i64,
    pub error_rate: f64,
    pub error_codes: Tally,
    pub cancellation_reason_codes: Tally,
    pub call_count: i64,
    pub key_distribution: Option<KeyDist>,
    pub extrapolated_monthly: f64,
    pub expected_monthly: f64,
    pub amplification_ratio: f64,
    pub observed_vector_search_bytes: f64,
    pub observed_vector_write_bytes_by_index: Vec<(String, f64)>,
    pub observed_vector_write_bytes: f64,
    pub vector_monthly: f64,
    pub cold_start: ColdStart,
    pub measurement_tainted: bool,
    pub consistency: String,
    pub items_per_request: Num,
    pub estimated_item_size_bytes: Num,
    pub td: Option<&'a Json>,
}

fn calculator_failure(error: &ToolkitError) -> ToolkitError {
    die(format!(
        "the calculator refused the design: {}",
        diag_bounded(error.message(), 240)
    ))
}

fn need<'a>(object: &'a Json, key: &str, what: &str) -> Res<&'a Json> {
    object
        .get(key)
        .ok_or_else(|| die(format!("{what} is missing `{key}`")))
}

fn as_number(value: &Json, key: &str, what: &str) -> Res<f64> {
    match value {
        Json::Int(number) => Ok(*number as f64),
        Json::Float(number) => Ok(*number),
        _ => Err(die(format!("{what} `{key}` must be a number"))),
    }
}

fn need_f64(object: &Json, key: &str, what: &str) -> Res<f64> {
    as_number(need(object, key, what)?, key, what)
}

fn opt_f64(object: &Json, key: &str, what: &str) -> Res<Option<f64>> {
    match need(object, key, what)? {
        Json::Null => Ok(None),
        other => as_number(other, key, what).map(Some),
    }
}

/// `object.get(key, default)` as an optional float: absent is `default`.
fn f64_or(object: &Json, key: &str, default: f64, what: &str) -> Res<f64> {
    match object.get(key) {
        None => Ok(default),
        Some(other) => as_number(other, key, what),
    }
}

fn i64_or(object: &Json, key: &str, default: i64, what: &str) -> Res<i64> {
    match object.get(key) {
        None => Ok(default),
        Some(Json::Int(number)) => Ok(*number),
        Some(_) => Err(die(format!("{what} `{key}` must be an integer"))),
    }
}

/// `member or {}` as a tally of integer counts.
fn tally_of(object: &Json, key: &str, what: &str) -> Res<Tally> {
    let mut tally = Tally::default();
    match object.get(key) {
        None | Some(Json::Null) => {}
        Some(Json::Object(pairs)) => {
            for (name, count) in pairs {
                let Json::Int(count) = count else {
                    return Err(die(format!(
                        "{what} `{key}` must map names to integer counts"
                    )));
                };
                tally.add(name, *count);
            }
        }
        Some(_) => return Err(die(format!("{what} `{key}` must be an object"))),
    }
    Ok(tally)
}

fn float_pairs(object: &Json, key: &str, what: &str) -> Res<Vec<(String, f64)>> {
    let mut out = Vec::new();
    match object.get(key) {
        None | Some(Json::Null) => {}
        Some(Json::Object(pairs)) => {
            for (name, value) in pairs {
                out.push((name.clone(), as_number(value, key, what)?));
            }
        }
        Some(_) => return Err(die(format!("{what} `{key}` must be an object"))),
    }
    Ok(out)
}

fn key_distribution(steady: &Json, what: &str) -> Res<Option<KeyDist>> {
    let Some(raw) = steady.get("key_distribution") else {
        return Ok(None);
    };
    if !pyfmt::truthy(raw) {
        return Ok(None);
    }
    let maybe = |key: &str| -> Res<Option<f64>> {
        match raw.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(other) => as_number(other, key, what).map(Some),
        }
    };
    Ok(Some(KeyDist {
        n_distinct_keys: raw.get("n_distinct_keys").cloned(),
        top_key_share: maybe("top_key_share")?,
        stddev_over_mean: f64_or(raw, "stddev_over_mean", 0.0, what)?,
        hot_partition_p99_ms: maybe("hot_partition_p99_ms")?,
        cold_partition_p99_ms: maybe("cold_partition_p99_ms")?,
    }))
}

/// `_merge_rows`: one [`Row`] per summary pattern that the design declares.
pub fn merge_rows<'a>(model: &'a Json, summary: &Json) -> Res<Vec<Row<'a>>> {
    let tables = items(model, "tables", "the design")?;
    let mut table_map: BTreeMap<&str, &Json> = BTreeMap::new();
    for table in objects(tables, "table")? {
        let Some(name) = table.get("table_name").and_then(Json::as_str) else {
            return Err(die("a table is missing its string `table_name`"));
        };
        table_map.insert(name, table);
    }
    let entity_sizes: EntitySizes =
        cost::build_entity_attr_sizes(tables).map_err(|e| calculator_failure(&e))?;
    let mut ap_map: BTreeMap<&str, &Json> = BTreeMap::new();
    for ap in objects(
        items(model, "access_patterns", "the design")?,
        "access pattern",
    )? {
        let Some(pid) = ap.get("pattern_id").and_then(Json::as_str) else {
            return Err(die("an access pattern is missing its string `pattern_id`"));
        };
        ap_map.insert(pid, ap);
    }

    let mut rows = Vec::new();
    for p in objects(
        items(summary, "patterns", "the summary")?,
        "summary pattern",
    )? {
        let Some(pid) = p.get("pattern_id").and_then(Json::as_str) else {
            return Err(die("a summary pattern is missing its string `pattern_id`"));
        };
        let Some(&ap) = ap_map.get(pid) else {
            continue;
        };
        if !pyfmt::truthy(ap) {
            continue;
        }
        let what = format!("the summary of pattern {}", diag(pid));
        let table = ap.get("table").and_then(Json::as_str).ok_or_else(|| {
            die(format!(
                "pattern {} is missing its string `table`",
                diag(pid)
            ))
        })?;
        let op = ap.get("operation").and_then(Json::as_str).ok_or_else(|| {
            die(format!(
                "pattern {} is missing its string `operation`",
                diag(pid)
            ))
        })?;
        let td = table_map.get(table).copied();
        let pc = cost::pattern_monthly_cost(ap, td, Some(&entity_sizes))
            .map_err(|e| calculator_failure(&e))?;
        let expected_cu = pc.cap.rcus + pc.cap.wcus;

        let steady = need(p, "steady_state", &what)?;
        let observed_cu = need_f64(steady, "mean_observed_cu", &what)?;
        let declared = need(ap, "peak_rps", "a design pattern")?;
        let declared_rps = Num::from_json(declared)
            .ok_or_else(|| die(format!("pattern {} `peak_rps` must be a number", diag(pid))))?;
        let unit_price = if cost::is_write_op(op) {
            cost::WRU_PRICE
        } else {
            cost::RRU_PRICE
        };
        let mut extrapolated = observed_cu * declared_rps.as_f64() * SECONDS_PER_MONTH * unit_price;
        let expected_monthly = pc.total_cost;

        let obs_search_bytes = match steady.get("mean_vector_search_bytes") {
            Some(value) if pyfmt::truthy(value) => {
                as_number(value, "mean_vector_search_bytes", &what)?
            }
            _ => 0.0,
        };
        let by_index = float_pairs(steady, "mean_vector_write_bytes_by_index", &what)?;
        let obs_write_bytes = py_sum(&by_index.iter().map(|(_, v)| *v).collect::<Vec<_>>());
        let calls_per_month = declared_rps.as_f64() * SECONDS_PER_MONTH;
        let vector_monthly = (obs_search_bytes / BYTES_PER_GB * VECTOR_SEARCH_PRICE_PER_GB
            + obs_write_bytes / BYTES_PER_GB * VECTOR_WRITE_PRICE_PER_GB)
            * calls_per_month;
        extrapolated += vector_monthly;

        let delta_pct = (expected_cu > 0.0).then(|| (observed_cu - expected_cu) / expected_cu);
        let index = ap
            .get("index")
            .filter(|value| pyfmt::truthy(value))
            .map(pyfmt::text);
        let table_index = match &index {
            Some(name) => format!("{table}/{name}"),
            None => table.to_string(),
        };
        let cold = need(p, "cold_start", &what)?;
        let consistency = match ap.get("consistency") {
            None => "eventual".to_string(),
            Some(value) => pyfmt::text(value),
        };
        let number_or = |key: &str, default: Num| -> Res<Num> {
            match ap.get(key) {
                None => Ok(default),
                Some(value) => Num::from_json(value)
                    .ok_or_else(|| die(format!("pattern {} `{key}` must be a number", diag(pid)))),
            }
        };
        let rate_what = format!("{what} (steady_state)");
        rows.push(Row {
            pattern_id: pid.to_string(),
            op: op.to_string(),
            table: table.to_string(),
            index,
            table_index,
            declared_peak_rps: declared_rps,
            bench_rps: need_f64(p, "bench_rps", &what)?,
            observed_cu,
            expected_cu,
            delta_pct,
            p50_ms: opt_f64(steady, "p50_ms", &rate_what)?,
            p95_ms: opt_f64(steady, "p95_ms", &rate_what)?,
            p99_ms: opt_f64(steady, "p99_ms", &rate_what)?,
            throttles: match need(steady, "throttles", &rate_what)? {
                Json::Int(number) => *number,
                _ => return Err(die(format!("{rate_what} `throttles` must be an integer"))),
            },
            errors: i64_or(steady, "errors", 0, &rate_what)?,
            error_rate: f64_or(steady, "error_rate", 0.0, &rate_what)?,
            error_codes: tally_of(steady, "error_codes", &rate_what)?,
            cancellation_reason_codes: tally_of(steady, "cancellation_reason_codes", &rate_what)?,
            call_count: match need(steady, "call_count", &rate_what)? {
                Json::Int(number) => *number,
                _ => return Err(die(format!("{rate_what} `call_count` must be an integer"))),
            },
            key_distribution: key_distribution(steady, &rate_what)?,
            extrapolated_monthly: extrapolated,
            expected_monthly,
            amplification_ratio: need_f64(steady, "amplification_ratio", &rate_what)?,
            observed_vector_search_bytes: obs_search_bytes,
            observed_vector_write_bytes_by_index: by_index,
            observed_vector_write_bytes: obs_write_bytes,
            vector_monthly,
            cold_start: ColdStart {
                warmup_p50_ms: opt_f64(cold, "warmup_p50_ms", &what)?,
                warmup_p99_ms: opt_f64(cold, "warmup_p99_ms", &what)?,
                warmup_throttles: cold
                    .get("warmup_throttles")
                    .cloned()
                    .unwrap_or(Json::Int(0)),
                cold_start_elevated: cold.get("cold_start_elevated").is_some_and(pyfmt::truthy),
            },
            measurement_tainted: p.get("measurement_tainted").is_some_and(pyfmt::truthy),
            consistency,
            items_per_request: number_or("items_per_request", Num::Int(1))?,
            estimated_item_size_bytes: number_or("estimated_item_size_bytes", Num::Int(1024))?,
            td,
        });
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// A pattern that dominates the extrapolated bill (> 20%).
#[derive(Debug, Clone, PartialEq)]
pub struct Dominant {
    pub pattern_id: String,
    pub monthly: f64,
    pub share: f64,
    pub op: String,
    pub consistency: String,
    pub throttles: i64,
    pub delta_pct: Option<f64>,
}

/// A pattern with steady-state throttles.
#[derive(Debug, Clone, PartialEq)]
pub struct Throttled {
    pub pattern_id: String,
    pub throttles: i64,
}

/// A write whose GSI amplification exceeds what the projection implies.
#[derive(Debug, Clone, PartialEq)]
pub struct HighAmp {
    pub pattern_id: String,
    pub gsi_name: String,
    pub projection: String,
    pub observed_amp: f64,
    pub projection_implied_amp: f64,
}

/// A strong read that is more than 10% of the bill.
#[derive(Debug, Clone, PartialEq)]
pub struct StrongRead {
    pub pattern_id: String,
    pub extrapolated_monthly: f64,
    pub share: f64,
}

/// A Query whose page approaches the 900 KB cap.
#[derive(Debug, Clone, PartialEq)]
pub struct PageCap {
    pub pattern_id: String,
    pub approx_page_bytes: Num,
}

/// A pattern whose warmup tail exceeds twice the steady tail.
#[derive(Debug, Clone, PartialEq)]
pub struct ColdElevated {
    pub pattern_id: String,
    pub warmup_p99: Option<f64>,
    pub steady_p99: Option<f64>,
}

/// A pattern with skewed keys and a distressed hot partition.
#[derive(Debug, Clone, PartialEq)]
pub struct KeySkew {
    pub pattern_id: String,
    pub stddev_over_mean: f64,
    pub top_key_share: Option<f64>,
    pub throttles: i64,
    pub hot_partition_p99_ms: Option<f64>,
    pub cold_partition_p99_ms: Option<f64>,
    pub evidence_kind: &'static str,
}

/// A pattern that failed most of its calls.
#[derive(Debug, Clone, PartialEq)]
pub struct HighError {
    pub pattern_id: String,
    pub error_rate: f64,
    pub errors: i64,
    pub call_count: i64,
    pub top_error_code: String,
    pub error_codes: Tally,
    pub error_kind: &'static str,
    pub dominant_code: String,
    pub cancellation_reason_codes: Tally,
}

/// A pattern with a few non-throttle errors.
#[derive(Debug, Clone, PartialEq)]
pub struct MinorError {
    pub pattern_id: String,
    pub error_rate: f64,
    pub errors: i64,
    pub call_count: i64,
    pub top_error_code: String,
}

/// A pattern whose observed capacity differs from the calculator's by > 10%.
#[derive(Debug, Clone, PartialEq)]
pub struct LargeDelta {
    pub pattern_id: String,
    pub delta_pct: f64,
    pub likely_cause: &'static str,
}

/// The design signals of a run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signals {
    pub dominant_cost_patterns: Vec<Dominant>,
    pub persistent_throttles: Vec<Throttled>,
    pub high_gsi_amplification: Vec<HighAmp>,
    pub strong_read_overhead: Vec<StrongRead>,
    pub page_cap_hits: Vec<PageCap>,
    pub cold_start_elevated_patterns: Vec<ColdElevated>,
    pub key_skew_patterns: Vec<KeySkew>,
    pub high_error_rate_patterns: Vec<HighError>,
    pub minor_error_patterns: Vec<MinorError>,
    pub large_expected_observed_delta_patterns: Vec<LargeDelta>,
}

fn total_extrapolated(rows: &[Row<'_>]) -> f64 {
    py_sum(
        &rows
            .iter()
            .map(|r| r.extrapolated_monthly)
            .collect::<Vec<_>>(),
    )
}

/// Projection-implied GSI write amplification.
fn implied_amplification(projection: &str) -> f64 {
    match projection {
        "INCLUDE" => 0.3,
        "KEYS_ONLY" => 0.1,
        _ => 1.0,
    }
}

/// `_extract_signals`.
pub fn extract_signals(rows: &[Row<'_>]) -> Res<Signals> {
    let total_monthly = total_extrapolated(rows);
    let mut signals = Signals::default();

    if total_monthly > 0.0 {
        for r in rows {
            let share = r.extrapolated_monthly / total_monthly;
            if share > 0.20 {
                signals.dominant_cost_patterns.push(Dominant {
                    pattern_id: r.pattern_id.clone(),
                    monthly: r.extrapolated_monthly,
                    share,
                    op: r.op.clone(),
                    consistency: r.consistency.clone(),
                    throttles: r.throttles,
                    delta_pct: r.delta_pct,
                });
            }
        }
    }
    signals
        .dominant_cost_patterns
        .sort_by(|a, b| b.share.total_cmp(&a.share));

    signals.persistent_throttles = rows
        .iter()
        .filter(|r| r.throttles > 0 && !r.measurement_tainted)
        .map(|r| Throttled {
            pattern_id: r.pattern_id.clone(),
            throttles: r.throttles,
        })
        .collect();

    for r in rows {
        let Some(td) = r.td.filter(|t| pyfmt::truthy(t)) else {
            continue;
        };
        let gsis = items(td, "gsis", "a table")?;
        if !cost::is_write_op(&r.op) || gsis.is_empty() {
            continue;
        }
        let obs_amp = r.amplification_ratio;
        for g in gsis {
            let Some(Json::String(gsi_name)) = g.get("index_name") else {
                return Err(die("a GSI is missing its string `index_name`"));
            };
            let projection = match g.get("projection").filter(|v| pyfmt::truthy(v)) {
                None => "ALL".to_string(),
                Some(projection) => match projection.get("type") {
                    None => "ALL".to_string(),
                    Some(Json::String(kind)) => kind.to_uppercase(),
                    Some(_) => return Err(die("a GSI projection `type` must be a string")),
                },
            };
            let implied = implied_amplification(&projection);
            if obs_amp > implied * 1.15 && obs_amp > 0.01 {
                signals.high_gsi_amplification.push(HighAmp {
                    pattern_id: r.pattern_id.clone(),
                    gsi_name: gsi_name.clone(),
                    projection,
                    observed_amp: obs_amp,
                    projection_implied_amp: implied,
                });
            }
        }
    }

    for r in rows {
        if !is_read_op(&r.op) || r.consistency != "strong" {
            continue;
        }
        let share = if total_monthly > 0.0 {
            r.extrapolated_monthly / total_monthly
        } else {
            0.0
        };
        if share > 0.10 {
            signals.strong_read_overhead.push(StrongRead {
                pattern_id: r.pattern_id.clone(),
                extrapolated_monthly: r.extrapolated_monthly,
                share,
            });
        }
    }

    for r in rows {
        if r.op == "Query" {
            let approx = r.items_per_request.times(r.estimated_item_size_bytes);
            if approx.as_f64() >= PAGE_CAP_BYTES * 0.9 {
                signals.page_cap_hits.push(PageCap {
                    pattern_id: r.pattern_id.clone(),
                    approx_page_bytes: approx,
                });
            }
        }
    }

    signals.cold_start_elevated_patterns = rows
        .iter()
        .filter(|r| r.cold_start.cold_start_elevated)
        .map(|r| ColdElevated {
            pattern_id: r.pattern_id.clone(),
            warmup_p99: r.cold_start.warmup_p99_ms,
            steady_p99: r.p99_ms,
        })
        .collect();

    for r in rows {
        let Some(kd) = &r.key_distribution else {
            continue;
        };
        if kd.stddev_over_mean <= 0.5 {
            continue;
        }
        let elevated = match (kd.hot_partition_p99_ms, kd.cold_partition_p99_ms) {
            (Some(hot), Some(cold)) => cold > 0.0 && hot > 1.8 * cold,
            _ => false,
        };
        if r.throttles > 0 || elevated {
            signals.key_skew_patterns.push(KeySkew {
                pattern_id: r.pattern_id.clone(),
                stddev_over_mean: kd.stddev_over_mean,
                top_key_share: kd.top_key_share,
                throttles: r.throttles,
                hot_partition_p99_ms: kd.hot_partition_p99_ms,
                cold_partition_p99_ms: kd.cold_partition_p99_ms,
                evidence_kind: if r.throttles > 0 {
                    "throttles"
                } else {
                    "elevated_latency"
                },
            });
        }
    }

    let mut error_pids: Vec<&str> = Vec::new();
    for r in rows {
        if r.error_rate >= ERROR_RATE_HIGH && r.call_count > 0 {
            error_pids.push(&r.pattern_id);
            let top_code = r.error_codes.top().unwrap_or("unknown").to_string();
            let (kind, dominant, hist) =
                classify_error_codes(&r.error_codes, &r.cancellation_reason_codes);
            signals.high_error_rate_patterns.push(HighError {
                pattern_id: r.pattern_id.clone(),
                error_rate: r.error_rate,
                errors: r.errors,
                call_count: r.call_count,
                top_error_code: top_code,
                error_codes: r.error_codes.clone(),
                error_kind: kind,
                dominant_code: dominant,
                cancellation_reason_codes: hist,
            });
        }
    }

    for r in rows {
        if !error_pids.contains(&r.pattern_id.as_str()) && r.errors > 0 {
            signals.minor_error_patterns.push(MinorError {
                pattern_id: r.pattern_id.clone(),
                error_rate: r.error_rate,
                errors: r.errors,
                call_count: r.call_count,
                top_error_code: r.error_codes.top().unwrap_or("unknown").to_string(),
            });
        }
    }

    for r in rows {
        let Some(delta) = r.delta_pct else {
            continue;
        };
        if error_pids.contains(&r.pattern_id.as_str()) || r.throttles > 0 {
            continue;
        }
        if delta.abs() > TOLERANCE {
            let likely = if cost::is_write_op(&r.op) {
                if delta < 0.0 {
                    "item_size_off"
                } else {
                    "conditional_fail_rate_or_projection"
                }
            } else {
                "RPS_or_consistency_off"
            };
            signals
                .large_expected_observed_delta_patterns
                .push(LargeDelta {
                    pattern_id: r.pattern_id.clone(),
                    delta_pct: delta,
                    likely_cause: likely,
                });
        }
    }
    Ok(signals)
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// One classified finding (`design_findings.json` `classified_findings[]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub id: String,
    pub category: &'static str,
    pub signal: &'static str,
    pub pattern_ids: Vec<String>,
    pub evidence: Json,
    pub axioms: Vec<&'static str>,
    pub severity: &'static str,
}

impl Finding {
    /// The finding as written to `design_findings.json`.
    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::object(vec![
            ("id", Json::string(self.id.clone())),
            ("category", Json::string(self.category)),
            ("signal", Json::string(self.signal)),
            (
                "pattern_ids",
                Json::Array(
                    self.pattern_ids
                        .iter()
                        .map(|p| Json::string(p.clone()))
                        .collect(),
                ),
            ),
            ("evidence", self.evidence.clone()),
            (
                "axioms",
                Json::Array(self.axioms.iter().map(|a| Json::string(*a)).collect()),
            ),
            ("severity", Json::string(self.severity)),
        ])
    }
}

fn opt_json(value: Option<f64>) -> Json {
    value.map_or(Json::Null, Json::Float)
}

fn interpretation(is_artifact: bool, dominant: &str) -> String {
    if is_artifact {
        let reason = if dominant.is_empty() {
            "TransactionConflict"
        } else {
            dominant
        };
        format!(
            "Most calls for this pattern were rejected with `{reason}`. This is almost certainly \
             a BENCHMARK ARTIFACT, not a design defect: the load generator drives many \
             concurrent writes against a small synthetic key pool (`seed_items_per_table`), so \
             the same items collide far more often than they would under real traffic with \
             unique IDs. The cost/latency numbers from the calls that DID succeed are still \
             representative of per-op cost. To drive this pattern to a clean error rate, raise \
             `seed_items_per_table` well above the write RPS, or lower the driven rate — do NOT \
             change the design on account of this error rate alone. Confirm against the real \
             cancellation-reason histogram in the Correctness section."
        )
    } else {
        "Most/all calls for this pattern FAILED (non-throttle). Observed capacity is 0 because \
         the operation errored, not because the design is cheap. Common causes: Query/GSI names \
         an index that does not exist on the table; an operation references an attribute the \
         table never defines; a batch/transact request built duplicate or unseeded keys; or the \
         Lambda role lacks the action. Fix the structural cause and re-run — the cost/latency \
         numbers for this pattern are not meaningful until it succeeds."
            .to_string()
    }
}

/// `_classify_findings`.
#[must_use]
pub fn classify_findings(signals: &Signals) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let mut push = |category: &'static str,
                    signal: &'static str,
                    pid: &str,
                    evidence: Json,
                    axioms: Vec<&'static str>,
                    severity: &'static str| {
        let id = format!("finding-{}", out.len() + 1);
        out.push(Finding {
            id,
            category,
            signal,
            pattern_ids: vec![pid.to_string()],
            evidence,
            axioms,
            severity,
        });
    };

    for d in &signals.dominant_cost_patterns {
        let is_write = cost::is_write_op(&d.op);
        let is_txn = d.op == "TransactWriteItems" || (is_write && d.consistency == "transactional");
        let axioms = if is_txn {
            vec!["Mechanics #18", "Mechanics #1"]
        } else if is_write {
            vec!["Mechanics #1", "Mechanics #8"]
        } else {
            vec!["Mechanics #1", "Mechanics #7", "Integration #8"]
        };
        let live = d.throttles > 0 || d.delta_pct.is_some_and(|delta| delta.abs() > TOLERANCE);
        let severity = if d.share > 0.4 && live {
            "high"
        } else {
            "medium"
        };
        push(
            "design",
            "dominant_cost_patterns",
            &d.pattern_id,
            Json::object(vec![
                ("monthly", Json::Float(d.monthly)),
                ("share", Json::Float(d.share)),
            ]),
            axioms,
            severity,
        );
    }
    for t in &signals.persistent_throttles {
        push(
            "design",
            "persistent_throttles",
            &t.pattern_id,
            Json::object(vec![("throttles", Json::Int(t.throttles))]),
            vec!["Mechanics #3"],
            "high",
        );
    }
    for h in &signals.high_gsi_amplification {
        push(
            "design",
            "high_gsi_amplification",
            &h.pattern_id,
            Json::object(vec![
                ("observed_amp", Json::Float(h.observed_amp)),
                (
                    "projection_implied_amp",
                    Json::Float(h.projection_implied_amp),
                ),
                ("projection", Json::string(h.projection.clone())),
                ("gsi_name", Json::string(h.gsi_name.clone())),
            ]),
            vec!["Mechanics #6", "Mechanics #7", "Mechanics #8"],
            if h.observed_amp > 2.0 * h.projection_implied_amp {
                "high"
            } else {
                "medium"
            },
        );
    }
    for p in &signals.page_cap_hits {
        push(
            "design",
            "page_cap_hits",
            &p.pattern_id,
            Json::object(vec![("approx_page_bytes", p.approx_page_bytes.to_json())]),
            vec!["Mechanics #17"],
            "medium",
        );
    }
    for s in &signals.strong_read_overhead {
        push(
            "design",
            "strong_read_overhead",
            &s.pattern_id,
            Json::object(vec![
                ("extrapolated_monthly", Json::Float(s.extrapolated_monthly)),
                ("share", Json::Float(s.share)),
            ]),
            vec!["Mechanics #15"],
            "medium",
        );
    }
    for c in &signals.cold_start_elevated_patterns {
        push(
            "design",
            "cold_start_elevated_patterns",
            &c.pattern_id,
            Json::object(vec![
                ("warmup_p99", opt_json(c.warmup_p99)),
                ("steady_p99", opt_json(c.steady_p99)),
            ]),
            vec!["Mechanics #19"],
            "low",
        );
    }
    for k in &signals.key_skew_patterns {
        push(
            "design",
            "key_skew_patterns",
            &k.pattern_id,
            Json::object(vec![
                ("stddev_over_mean", Json::Float(k.stddev_over_mean)),
                ("top_key_share", opt_json(k.top_key_share)),
                ("throttles", Json::Int(k.throttles)),
                ("hot_partition_p99_ms", opt_json(k.hot_partition_p99_ms)),
                ("cold_partition_p99_ms", opt_json(k.cold_partition_p99_ms)),
                ("evidence_kind", Json::string(k.evidence_kind)),
            ]),
            vec!["Mechanics #3"],
            if k.throttles > 0 { "high" } else { "medium" },
        );
    }
    for e in &signals.high_error_rate_patterns {
        let artifact = e.error_kind == "artifact";
        push(
            "correctness",
            if artifact {
                "pattern_artifact_error_rate"
            } else {
                "pattern_high_error_rate"
            },
            &e.pattern_id,
            Json::object(vec![
                ("error_rate", Json::Float(e.error_rate)),
                ("errors", Json::Int(e.errors)),
                ("call_count", Json::Int(e.call_count)),
                ("top_error_code", Json::string(e.top_error_code.clone())),
                ("error_codes", e.error_codes.to_json()),
                ("error_kind", Json::string(e.error_kind)),
                (
                    "cancellation_reason_codes",
                    e.cancellation_reason_codes.to_json(),
                ),
                (
                    "interpretation",
                    Json::string(interpretation(artifact, &e.dominant_code)),
                ),
            ]),
            vec!["Mechanics #2"],
            if artifact { "medium" } else { "high" },
        );
    }
    for e in &signals.minor_error_patterns {
        push(
            "correctness",
            "pattern_minor_errors",
            &e.pattern_id,
            Json::object(vec![
                ("error_rate", Json::Float(e.error_rate)),
                ("errors", Json::Int(e.errors)),
                ("call_count", Json::Int(e.call_count)),
                ("top_error_code", Json::string(e.top_error_code.clone())),
            ]),
            vec!["Mechanics #2"],
            "low",
        );
    }
    for d in &signals.large_expected_observed_delta_patterns {
        push(
            "input-accuracy",
            "large_expected_observed_delta_patterns",
            &d.pattern_id,
            Json::object(vec![
                ("delta_pct", Json::Float(d.delta_pct)),
                ("likely_cause", Json::string(d.likely_cause)),
            ]),
            vec!["Mechanics #2", "Mechanics #18"],
            "medium",
        );
    }
    out
}

/// The seed-shortfall findings of the summary's `seed_verification` map: a
/// table that was counted to exhaustion and still fell short.
pub fn seed_shortfall_findings(summary: &Json) -> Res<Vec<Finding>> {
    let mut out = Vec::new();
    match summary.get("seed_verification") {
        None | Some(Json::Null) => {}
        Some(Json::Object(tables)) => {
            for (name, sv) in tables {
                if !matches!(sv, Json::Object(_)) {
                    return Err(die("each `seed_verification` entry must be an object"));
                }
                let passed = sv.get("passed").is_none_or(pyfmt::truthy);
                let sampled = sv.get("sampled").is_some_and(pyfmt::truthy);
                if !passed && !sampled {
                    out.push(Finding {
                        id: format!("seed-{name}"),
                        category: "correctness",
                        signal: "seed_shortfall",
                        pattern_ids: Vec::new(),
                        evidence: Json::object(vec![
                            ("table", Json::string(name.clone())),
                            (
                                "expected",
                                sv.get("expected").cloned().unwrap_or(Json::Null),
                            ),
                            ("actual", sv.get("actual").cloned().unwrap_or(Json::Null)),
                            (
                                "seed_shortfall_ratio",
                                sv.get("seed_shortfall_ratio")
                                    .cloned()
                                    .unwrap_or(Json::Null),
                            ),
                        ]),
                        axioms: vec!["Mechanics #2"],
                        severity: "high",
                    });
                }
            }
        }
        Some(_) => return Err(die("the summary `seed_verification` must be an object")),
    }
    Ok(out)
}

/// `_is_significant`: something the agent must act on.
#[must_use]
pub fn is_significant(finding: &Finding) -> bool {
    if finding.signal == "dominant_cost_patterns" {
        return finding.severity == "high";
    }
    if finding.category == "design" {
        return true;
    }
    finding.category == "correctness" && finding.severity == "high"
}

/// One of the five top cost drivers.
#[derive(Debug, Clone, PartialEq)]
pub struct Driver {
    pub pattern_id: String,
    pub monthly: f64,
    pub share: f64,
}

/// The top five patterns by extrapolated monthly cost.
#[must_use]
pub fn top_drivers(rows: &[Row<'_>]) -> Vec<Driver> {
    let mut sorted: Vec<&Row<'_>> = rows.iter().collect();
    sorted.sort_by(|a, b| b.extrapolated_monthly.total_cmp(&a.extrapolated_monthly));
    let total = total_extrapolated(rows);
    sorted
        .into_iter()
        .take(5)
        .map(|r| Driver {
            pattern_id: r.pattern_id.clone(),
            monthly: r.extrapolated_monthly,
            share: if total == 0.0 {
                0.0
            } else {
                r.extrapolated_monthly / total
            },
        })
        .collect()
}

/// The machine-readable output (`design_findings.json`).
#[must_use]
pub fn findings_payload(
    findings: &[Finding],
    drivers: &[Driver],
    no_significant: bool,
    coverage_incomplete: bool,
) -> Json {
    Json::object(vec![
        (
            "classified_findings",
            Json::Array(findings.iter().map(Finding::to_json).collect()),
        ),
        (
            "top_cost_drivers",
            Json::Array(
                drivers
                    .iter()
                    .map(|d| {
                        Json::object(vec![
                            ("pattern_id", Json::string(d.pattern_id.clone())),
                            ("monthly", Json::Float(d.monthly)),
                            ("share", Json::Float(d.share)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("no_significant_findings", Json::Bool(no_significant)),
        ("coverage_incomplete", Json::Bool(coverage_incomplete)),
    ])
}

/// Whether the summary says the run did not measure every pattern.
#[must_use]
pub fn coverage_incomplete(summary: &Json) -> bool {
    let Some(cov) = summary.get("coverage").filter(|v| pyfmt::truthy(v)) else {
        return false;
    };
    cov.get("coverage_incomplete").is_some_and(pyfmt::truthy)
        || cov.get("missing_patterns").is_some_and(pyfmt::truthy)
}

/// The whole analysis of a design and a summary.
pub struct Analysis<'a> {
    pub rows: Vec<Row<'a>>,
    pub signals: Signals,
    pub findings: Vec<Finding>,
    pub drivers: Vec<Driver>,
    pub no_significant: bool,
    pub coverage_incomplete: bool,
}

/// `main()` of `generate_perf_report.py` up to the rendering.
pub fn analyze<'a>(model: &'a Json, summary: &Json) -> Res<Analysis<'a>> {
    if !matches!(model, Json::Object(_)) {
        return Err(die("the design JSON must be an object"));
    }
    if !matches!(summary, Json::Object(_)) {
        return Err(die("the summary JSON must be an object"));
    }
    let rows = merge_rows(model, summary)?;
    let signals = extract_signals(&rows)?;
    let mut findings = classify_findings(&signals);
    findings.extend(seed_shortfall_findings(summary)?);
    let drivers = top_drivers(&rows);
    let significant = findings.iter().any(is_significant);
    let incomplete = coverage_incomplete(summary);
    Ok(Analysis {
        rows,
        signals,
        findings,
        drivers,
        no_significant: !significant && !incomplete,
        coverage_incomplete: incomplete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_classification_needs_every_code_to_be_an_artifact() {
        let mut codes = Tally::default();
        codes.add("TransactionCanceledException", 10);
        let mut reasons = Tally::default();
        reasons.add("TransactionConflict", 9);
        let (kind, dominant, hist) = classify_error_codes(&codes, &reasons);
        assert_eq!(kind, "artifact");
        assert_eq!(dominant, "TransactionConflict");
        assert_eq!(hist, reasons);
        reasons.add("ValidationError", 1);
        assert_eq!(classify_error_codes(&codes, &reasons).0, "structural");
        let mut plain = Tally::default();
        plain.add("ConditionalCheckFailedException", 4);
        assert_eq!(
            classify_error_codes(&plain, &Tally::default()).0,
            "artifact"
        );
        plain.add("ValidationException", 1);
        assert_eq!(
            classify_error_codes(&plain, &Tally::default()).0,
            "structural"
        );
        let (kind, dominant, _) = classify_error_codes(&Tally::default(), &Tally::default());
        assert_eq!((kind, dominant.as_str()), ("structural", "unknown"));
    }

    #[test]
    fn read_ops_exclude_vector_search() {
        assert!(is_read_op("Query"));
        assert!(!is_read_op("SearchVectors"));
        assert!(!is_read_op("PutItem"));
    }

    #[test]
    fn significance_follows_the_script() {
        let base = |signal: &'static str, category: &'static str, severity: &'static str| Finding {
            id: "f".to_string(),
            category,
            signal,
            pattern_ids: Vec::new(),
            evidence: Json::Null,
            axioms: Vec::new(),
            severity,
        };
        assert!(!is_significant(&base(
            "dominant_cost_patterns",
            "design",
            "medium"
        )));
        assert!(is_significant(&base(
            "dominant_cost_patterns",
            "design",
            "high"
        )));
        assert!(is_significant(&base(
            "persistent_throttles",
            "design",
            "high"
        )));
        assert!(is_significant(&base("page_cap_hits", "design", "medium")));
        assert!(is_significant(&base(
            "pattern_high_error_rate",
            "correctness",
            "high"
        )));
        assert!(!is_significant(&base(
            "pattern_artifact_error_rate",
            "correctness",
            "medium"
        )));
        assert!(!is_significant(&base(
            "pattern_minor_errors",
            "correctness",
            "low"
        )));
        assert!(!is_significant(&base("x", "input-accuracy", "medium")));
    }
}
