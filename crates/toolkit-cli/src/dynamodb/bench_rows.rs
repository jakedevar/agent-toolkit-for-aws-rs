//! Aggregation half of `benchmark_model.py` (Issue #4390): `_percentile` and
//! `_aggregate`, turning the per-call rows and the Lambda's exact tallies into
//! the per-pattern blocks of `perf_summary.json`.
//!
//! The rows are typed ([`BenchRow`]): the orchestrator validates every member
//! of a row it receives, so nothing here reads a response shape. Sums use the
//! interpreter's `sum()` rule ([`py_sum`]) so the figures equal the script's.

use std::collections::BTreeMap;

use toolkit_core::Json;

use super::bench::{self, items, objects, Res};
use super::bench_fmt::{die, py_sum, Tally};
use super::cost::is_write_op;
use super::pyfmt;

/// One per-call row of the Lambda response, validated.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchRow {
    /// The row exactly as the Lambda returned it (written to the raw file).
    pub json: Json,
    /// `pattern_id`.
    pub pattern_id: Option<String>,
    /// `phase` (`settle`, `seed`, `warmup`, `measure`).
    pub phase: Option<String>,
    /// `latency_ms`, when set.
    pub latency_ms: Option<f64>,
    /// `consumed_cu`, when set.
    pub consumed_cu: Option<f64>,
    /// `gsi_cu`: capacity per GSI, in response order.
    pub gsi_cu: Vec<(String, f64)>,
    /// `throttled`.
    pub throttled: bool,
    /// `error` (an empty string is no error).
    pub error: Option<String>,
    /// `key_idx`, when set.
    pub key_idx: Option<i64>,
    /// `vector_search_bytes`, when set.
    pub vector_search_bytes: Option<f64>,
    /// `vector_write_bytes`: bytes per vector index, in response order.
    pub vector_write_bytes: Vec<(String, f64)>,
}

/// The Lambda's uncapped tallies of one `(pattern_id, phase)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExactAgg {
    /// Calls made.
    pub calls: i64,
    /// Throttled calls.
    pub throttles: i64,
    /// Non-throttle errors.
    pub errors: i64,
    /// Error codes.
    pub error_codes: Tally,
    /// Transact* per-item cancellation reasons.
    pub cancellation_reason_codes: Tally,
}

/// The tallies keyed by `(pattern_id, phase)`.
pub type ExactCounts = BTreeMap<(String, String), ExactAgg>;

/// `_percentile(values, q)`: `statistics.quantiles(n=100, method="inclusive")`
/// cut `q`; `None` for no data and the value itself for a single one.
#[must_use]
pub fn percentile(values: &[f64], q: usize) -> Option<f64> {
    match values {
        [] => None,
        [only] => Some(*only),
        _ => {
            let mut data = values.to_vec();
            data.sort_by(f64::total_cmp);
            let n: usize = 100;
            let m = data.len() - 1;
            // Cuts are 1..n-1; `idx = max(0, min(len - 1, q - 1))`.
            let cut = q.saturating_sub(1).min(n - 2) + 1;
            let (j, delta) = (cut * m / n, cut * m % n);
            #[allow(clippy::cast_precision_loss)]
            let value = (data[j] * (n - delta) as f64 + data[j + 1] * delta as f64) / n as f64;
            Some(value)
        }
    }
}

fn opt(value: Option<f64>) -> Json {
    value.map_or(Json::Null, Json::Float)
}

fn pairs_json(pairs: &[(String, f64)]) -> Json {
    Json::Object(
        pairs
            .iter()
            .map(|(name, value)| (name.clone(), Json::Float(*value)))
            .collect(),
    )
}

fn add_float(pairs: &mut Vec<(String, f64)>, name: &str, value: f64) {
    if let Some(slot) = pairs.iter_mut().find(|(existing, _)| existing == name) {
        slot.1 += value;
    } else {
        pairs.push((name.to_string(), value));
    }
}

/// The hot-partition histogram of one pattern (`key_distribution`).
fn key_distribution(measure: &[&BenchRow]) -> Option<Json> {
    let keyed: Vec<(i64, &BenchRow)> = measure
        .iter()
        .filter_map(|row| row.key_idx.map(|idx| (idx, *row)))
        .collect();
    if keyed.is_empty() {
        return None;
    }
    let mut counts: Vec<(i64, i64)> = Vec::new();
    for (idx, _) in &keyed {
        if let Some(slot) = counts.iter_mut().find(|(key, _)| key == idx) {
            slot.1 += 1;
        } else {
            counts.push((*idx, 1));
        }
    }
    let n = keyed.len() as f64;
    let distinct = counts.len() as f64;
    let mean_f = n / distinct;
    let squares: Vec<f64> = counts
        .iter()
        .map(|(_, freq)| {
            let diff = *freq as f64 - mean_f;
            diff * diff
        })
        .collect();
    let var = py_sum(&squares) / distinct;
    let stddev = var.powf(0.5);
    let mut hot = counts[0];
    for entry in &counts {
        if entry.1 > hot.1 {
            hot = *entry;
        }
    }
    let hot_lat: Vec<f64> = keyed
        .iter()
        .filter(|(idx, _)| *idx == hot.0)
        .filter_map(|(_, row)| row.latency_ms)
        .collect();
    let cold_lat: Vec<f64> = keyed
        .iter()
        .filter(|(idx, _)| *idx != hot.0)
        .filter_map(|(_, row)| row.latency_ms)
        .collect();
    let sorted = |values: Vec<f64>| {
        let mut values = values;
        values.sort_by(f64::total_cmp);
        values
    };
    Some(Json::object(vec![
        ("n_distinct_keys", Json::Int(counts.len() as i64)),
        ("top_key_share", Json::Float(hot.1 as f64 / n)),
        (
            "stddev_over_mean",
            Json::Float(if mean_f > 0.0 { stddev / mean_f } else { 0.0 }),
        ),
        (
            "hot_partition_p99_ms",
            opt(percentile(&sorted(hot_lat), 99)),
        ),
        (
            "cold_partition_p99_ms",
            opt(percentile(&sorted(cold_lat), 99)),
        ),
    ]))
}

/// One aggregated pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternAggregate {
    /// `pattern_id` of the design pattern, as written.
    pub pattern_id: Json,
    /// The id as text, for matching the Lambda's taint map.
    pub id_text: Option<String>,
    /// `bench_rps`.
    pub bench_rps: f64,
    /// `steady_state` block.
    pub steady_state: Json,
    /// `cold_start` block.
    pub cold_start: Json,
}

impl PatternAggregate {
    /// The `patterns[]` entry of the summary, `tainted` marking a pattern the
    /// Lambda flagged (with its reason).
    #[must_use]
    pub fn to_json(&self, tainted: Option<&str>) -> Json {
        let mut pairs = vec![
            ("pattern_id".to_string(), self.pattern_id.clone()),
            ("bench_rps".to_string(), Json::Float(self.bench_rps)),
            ("steady_state".to_string(), self.steady_state.clone()),
            ("cold_start".to_string(), self.cold_start.clone()),
            (
                "measurement_tainted".to_string(),
                Json::Bool(tainted.is_some()),
            ),
        ];
        if let Some(reason) = tainted {
            pairs.push(("tainted_reason".to_string(), Json::string(reason)));
        }
        Json::Object(pairs)
    }
}

/// `_aggregate`: per-pattern steady-state and cold-start blocks. The exact
/// tallies, when present, replace the row-derived counts, so a down-sampled
/// measure window never under-reports throttles or errors.
pub fn aggregate(
    rows: &[BenchRow],
    patterns: &[Json],
    tables: &[Json],
    cfg: &Json,
    exact_counts: &ExactCounts,
) -> Res<Vec<PatternAggregate>> {
    let mut has_vector_indexes = false;
    for table in objects(tables, "table")? {
        for vi in items(table, "vector_indexes", "a table")? {
            if !matches!(vi.get("index_name"), Some(Json::String(_))) {
                return Err(die("a vector index is missing its string `index_name`"));
            }
            has_vector_indexes = true;
        }
        for gsi in items(table, "gsis", "a table")? {
            if !matches!(gsi.get("index_name"), Some(Json::String(_))) {
                return Err(die("a GSI is missing its string `index_name`"));
            }
        }
    }
    let key_sampling = match cfg.get("read_pattern_key_sampling") {
        Some(value) if pyfmt::truthy(value) => pyfmt::text(value).to_lowercase(),
        _ => "uniform".to_string(),
    };

    let mut out = Vec::new();
    for pattern in objects(patterns, "access pattern")? {
        let Some(pid_json) = pattern.get("pattern_id") else {
            return Err(die("an access pattern is missing its `pattern_id`"));
        };
        let Some(Json::String(operation)) = pattern.get("operation") else {
            return Err(die("an access pattern is missing its string `operation`"));
        };
        let pid = pid_json.as_str();
        let p_rows: Vec<&BenchRow> = rows
            .iter()
            .filter(|row| pid.is_some() && row.pattern_id.as_deref() == pid)
            .collect();
        let measure: Vec<&BenchRow> = p_rows
            .iter()
            .copied()
            .filter(|row| row.phase.as_deref() == Some("measure"))
            .collect();
        let warmup: Vec<&BenchRow> = p_rows
            .iter()
            .copied()
            .filter(|row| row.phase.as_deref() == Some("warmup"))
            .collect();
        let lat =
            |rs: &[&BenchRow]| -> Vec<f64> { rs.iter().filter_map(|r| r.latency_ms).collect() };
        let measure_lat = lat(&measure);
        let warmup_lat = lat(&warmup);

        let measure_cu: Vec<f64> = measure.iter().filter_map(|r| r.consumed_cu).collect();
        let mean_cu = if measure_cu.is_empty() {
            0.0
        } else {
            py_sum(&measure_cu) / measure_cu.len() as f64
        };

        let is_write = is_write_op(operation);
        let mut gsi_sum: Vec<(String, f64)> = Vec::new();
        let mut base_wcu_sum = 0.0_f64;
        let mut gsi_wcu_sum = 0.0_f64;
        for row in &measure {
            for (name, value) in &row.gsi_cu {
                add_float(&mut gsi_sum, name, *value);
                if is_write {
                    gsi_wcu_sum += value;
                }
            }
            if is_write {
                base_wcu_sum += row.consumed_cu.unwrap_or(0.0);
            }
        }
        let amp_ratio = if base_wcu_sum > 0.0 {
            gsi_wcu_sum / base_wcu_sum
        } else {
            0.0
        };

        let exact = pid.and_then(|id| exact_counts.get(&(id.to_string(), "measure".to_string())));
        let (exact_calls, exact_throttles) = exact.map_or_else(
            || {
                (
                    measure.len() as i64,
                    measure.iter().filter(|r| r.throttled).count() as i64,
                )
            },
            |ec| (ec.calls, ec.throttles),
        );
        let (exact_errors, error_codes) = if let Some(ec) = exact {
            (ec.errors, ec.error_codes.clone())
        } else {
            let mut codes = Tally::default();
            let mut errors = 0_i64;
            for row in &measure {
                if let (Some(code), false) = (&row.error, row.throttled) {
                    errors += 1;
                    codes.add(code, 1);
                }
            }
            (errors, codes)
        };
        let error_rate = if exact_calls == 0 {
            0.0
        } else {
            exact_errors as f64 / exact_calls as f64
        };
        let cancellation = exact
            .map(|ec| ec.cancellation_reason_codes.clone())
            .unwrap_or_default();

        let p50 = percentile(&measure_lat, 50);
        let p95 = percentile(&measure_lat, 95);
        let p99 = percentile(&measure_lat, 99);
        let mut steady: Vec<(String, Json)> = vec![
            ("call_count".to_string(), Json::Int(exact_calls)),
            ("rows_sampled".to_string(), Json::Int(measure.len() as i64)),
            ("p50_ms".to_string(), opt(p50)),
            ("p95_ms".to_string(), opt(p95)),
            ("p99_ms".to_string(), opt(p99)),
            ("mean_observed_cu".to_string(), Json::Float(mean_cu)),
            ("gsi_cu_by_index".to_string(), pairs_json(&gsi_sum)),
            ("amplification_ratio".to_string(), Json::Float(amp_ratio)),
            ("throttles".to_string(), Json::Int(exact_throttles)),
            ("errors".to_string(), Json::Int(exact_errors)),
            ("error_rate".to_string(), Json::Float(error_rate)),
            ("error_codes".to_string(), error_codes.to_json()),
            (
                "cancellation_reason_codes".to_string(),
                cancellation.to_json(),
            ),
        ];

        if has_vector_indexes {
            let vs_vals: Vec<f64> = measure
                .iter()
                .filter(|r| r.error.is_none())
                .filter_map(|r| r.vector_search_bytes)
                .collect();
            let mut vw_sum: Vec<(String, f64)> = Vec::new();
            let mut vw_calls: Vec<(String, i64)> = Vec::new();
            for row in measure.iter().filter(|r| r.error.is_none()) {
                for (name, value) in &row.vector_write_bytes {
                    add_float(&mut vw_sum, name, *value);
                    if let Some(slot) = vw_calls.iter_mut().find(|(n, _)| n == name) {
                        slot.1 += 1;
                    } else {
                        vw_calls.push((name.clone(), 1));
                    }
                }
            }
            let mean_vs = if vs_vals.is_empty() {
                0.0
            } else {
                py_sum(&vs_vals) / vs_vals.len() as f64
            };
            steady.push(("mean_vector_search_bytes".to_string(), Json::Float(mean_vs)));
            steady.push((
                "vector_search_bytes_total".to_string(),
                Json::Float(py_sum(&vs_vals)),
            ));
            steady.push((
                "vector_write_bytes_by_index".to_string(),
                pairs_json(&vw_sum),
            ));
            let means: Vec<(String, f64)> = vw_sum
                .iter()
                .filter_map(|(name, total)| {
                    vw_calls
                        .iter()
                        .find(|(n, _)| n == name)
                        .filter(|(_, count)| *count != 0)
                        .map(|(_, count)| (name.clone(), total / *count as f64))
                })
                .collect();
            steady.push((
                "mean_vector_write_bytes_by_index".to_string(),
                pairs_json(&means),
            ));
        }

        if key_sampling == "zipf" {
            if let Some(distribution) = key_distribution(&measure) {
                steady.push(("key_distribution".to_string(), distribution));
            }
        }

        let w_p50 = percentile(&warmup_lat, 50);
        let w_p99 = percentile(&warmup_lat, 99);
        let cold_start_elevated = match (w_p99, p99) {
            (Some(warm), Some(steady_p99)) if steady_p99 > 0.0 => warm > 2.0 * steady_p99,
            _ => false,
        };
        let cold = Json::object(vec![
            ("warmup_call_count", Json::Int(warmup.len() as i64)),
            ("warmup_p50_ms", opt(w_p50)),
            ("warmup_p99_ms", opt(w_p99)),
            (
                "warmup_throttles",
                Json::Int(warmup.iter().filter(|r| r.throttled).count() as i64),
            ),
            ("cold_start_elevated", Json::Bool(cold_start_elevated)),
        ]);

        out.push(PatternAggregate {
            pattern_id: pid_json.clone(),
            id_text: pid.map(str::to_string),
            bench_rps: bench::bench_rps(pattern, cfg)?,
            steady_state: Json::Object(steady),
            cold_start: cold,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: &str, phase: &str, latency: f64, cu: f64) -> BenchRow {
        BenchRow {
            json: Json::Null,
            pattern_id: Some(pid.to_string()),
            phase: Some(phase.to_string()),
            latency_ms: Some(latency),
            consumed_cu: Some(cu),
            gsi_cu: Vec::new(),
            throttled: false,
            error: None,
            key_idx: None,
            vector_search_bytes: None,
            vector_write_bytes: Vec::new(),
        }
    }

    #[test]
    fn percentile_is_the_inclusive_quantile_of_python() {
        // statistics.quantiles(range(1, 11), n=100, method="inclusive")
        let values: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&values, 50), Some(5.5));
        assert!((percentile(&values, 95).unwrap_or(0.0) - 9.55).abs() < 1e-12);
        assert!((percentile(&values, 99).unwrap_or(0.0) - 9.91).abs() < 1e-12);
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[4.0], 99), Some(4.0));
        assert_eq!(percentile(&[1.0, 3.0], 50), Some(2.0));
        // Unsorted input is sorted first.
        assert_eq!(percentile(&[3.0, 1.0], 50), Some(2.0));
    }

    #[test]
    fn exact_tallies_replace_the_row_counts() {
        let patterns = vec![Json::object(vec![
            ("pattern_id", Json::string("P")),
            ("operation", Json::string("GetItem")),
            ("peak_rps", Json::Int(100)),
        ])];
        let rows = vec![row("P", "measure", 2.0, 1.0), row("P", "measure", 4.0, 1.0)];
        let mut exact = ExactCounts::new();
        let mut agg = ExactAgg {
            calls: 500,
            throttles: 7,
            errors: 3,
            ..ExactAgg::default()
        };
        agg.error_codes.add("ValidationException", 3);
        exact.insert(("P".to_string(), "measure".to_string()), agg);
        let result =
            aggregate(&rows, &patterns, &[], &Json::Object(Vec::new()), &exact).expect("ok");
        let steady = &result[0].steady_state;
        assert_eq!(steady.get("call_count"), Some(&Json::Int(500)));
        assert_eq!(steady.get("rows_sampled"), Some(&Json::Int(2)));
        assert_eq!(steady.get("throttles"), Some(&Json::Int(7)));
        assert_eq!(steady.get("errors"), Some(&Json::Int(3)));
        assert_eq!(steady.get("error_rate"), Some(&Json::Float(0.006)));
        assert_eq!(steady.get("p50_ms"), Some(&Json::Float(3.0)));
        assert_eq!(steady.get("mean_observed_cu"), Some(&Json::Float(1.0)));
    }

    #[test]
    fn row_counts_are_the_fallback_and_exclude_throttles_from_errors() {
        let patterns = vec![Json::object(vec![
            ("pattern_id", Json::string("P")),
            ("operation", Json::string("PutItem")),
            ("peak_rps", Json::Int(100)),
        ])];
        let mut throttled = row("P", "measure", 1.0, 0.0);
        throttled.throttled = true;
        throttled.error = Some("ThrottlingException".to_string());
        let mut failed = row("P", "measure", 1.0, 0.0);
        failed.error = Some("ValidationException".to_string());
        let mut gsi = row("P", "measure", 1.0, 3.0);
        gsi.gsi_cu = vec![("ByCustomer".to_string(), 3.0)];
        let result = aggregate(
            &[throttled, failed, gsi],
            &patterns,
            &[],
            &Json::Object(Vec::new()),
            &ExactCounts::new(),
        )
        .expect("ok");
        let steady = &result[0].steady_state;
        assert_eq!(steady.get("call_count"), Some(&Json::Int(3)));
        assert_eq!(steady.get("throttles"), Some(&Json::Int(1)));
        assert_eq!(steady.get("errors"), Some(&Json::Int(1)));
        assert_eq!(
            steady.get("error_codes").map(Json::to_compact_string),
            Some(r#"{"ValidationException":1}"#.to_string())
        );
        assert_eq!(steady.get("amplification_ratio"), Some(&Json::Float(1.0)));
    }

    #[test]
    fn key_distribution_only_for_zipf_sampling() {
        let patterns = vec![Json::object(vec![
            ("pattern_id", Json::string("P")),
            ("operation", Json::string("GetItem")),
            ("peak_rps", Json::Int(100)),
        ])];
        let mut rows = Vec::new();
        for (idx, latency) in [(0, 9.0), (0, 8.0), (0, 10.0), (1, 2.0), (2, 3.0)] {
            let mut r = row("P", "measure", latency, 1.0);
            r.key_idx = Some(idx);
            rows.push(r);
        }
        let uniform = aggregate(
            &rows,
            &patterns,
            &[],
            &Json::Object(Vec::new()),
            &ExactCounts::new(),
        )
        .expect("uniform");
        assert!(uniform[0].steady_state.get("key_distribution").is_none());
        let zipf_cfg = Json::object(vec![("read_pattern_key_sampling", Json::string("ZIPF"))]);
        let zipf = aggregate(&rows, &patterns, &[], &zipf_cfg, &ExactCounts::new()).expect("zipf");
        let kd = zipf[0].steady_state.get("key_distribution").expect("kd");
        assert_eq!(kd.get("n_distinct_keys"), Some(&Json::Int(3)));
        assert_eq!(kd.get("top_key_share"), Some(&Json::Float(0.6)));
        assert_eq!(kd.get("hot_partition_p99_ms"), Some(&Json::Float(9.98)));
    }

    #[test]
    fn cold_start_is_elevated_above_twice_the_steady_p99() {
        let patterns = vec![Json::object(vec![
            ("pattern_id", Json::string("P")),
            ("operation", Json::string("GetItem")),
            ("peak_rps", Json::Int(100)),
        ])];
        let rows = vec![
            row("P", "warmup", 30.0, 1.0),
            row("P", "warmup", 40.0, 1.0),
            row("P", "measure", 5.0, 1.0),
            row("P", "measure", 6.0, 1.0),
        ];
        let result = aggregate(
            &rows,
            &patterns,
            &[],
            &Json::Object(Vec::new()),
            &ExactCounts::new(),
        )
        .expect("ok");
        assert_eq!(
            result[0].cold_start.get("cold_start_elevated"),
            Some(&Json::Bool(true))
        );
        assert_eq!(
            result[0].cold_start.get("warmup_call_count"),
            Some(&Json::Int(2))
        );
    }

    #[test]
    fn vector_blocks_appear_only_for_designs_with_vector_indexes() {
        let patterns = vec![Json::object(vec![
            ("pattern_id", Json::string("S")),
            ("operation", Json::string("SearchVectors")),
            ("peak_rps", Json::Int(10)),
        ])];
        let tables = vec![Json::object(vec![
            ("table_name", Json::string("T")),
            (
                "vector_indexes",
                Json::Array(vec![Json::object(vec![("index_name", Json::string("V"))])]),
            ),
        ])];
        let mut ok = row("S", "measure", 1.0, 0.0);
        ok.vector_search_bytes = Some(2048.0);
        ok.vector_write_bytes = vec![("V".to_string(), 1024.0)];
        let mut failed = row("S", "measure", 1.0, 0.0);
        failed.error = Some("ValidationException".to_string());
        failed.vector_search_bytes = Some(0.0);
        let result = aggregate(
            &[ok, failed],
            &patterns,
            &tables,
            &Json::Object(Vec::new()),
            &ExactCounts::new(),
        )
        .expect("ok");
        let steady = &result[0].steady_state;
        assert_eq!(
            steady.get("mean_vector_search_bytes"),
            Some(&Json::Float(2048.0))
        );
        assert_eq!(
            steady.get("vector_search_bytes_total"),
            Some(&Json::Float(2048.0))
        );
        assert_eq!(
            steady
                .get("mean_vector_write_bytes_by_index")
                .map(Json::to_compact_string),
            Some(r#"{"V":1024.0}"#.to_string())
        );
    }
}
