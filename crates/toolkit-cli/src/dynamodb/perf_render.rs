//! Markdown rendering of `generate_perf_report.py` (`_render_report`,
//! Issue #4390). The text is the script's, line for line; the only deliberate
//! difference is that text taken from the design or the summary is passed
//! through `neutralize_line`, so a newline or a terminal control character in a
//! name cannot forge a report line (see the migration document).

use toolkit_core::{neutralize_line, Json};

use super::bench::{cfg_f64, Res};
use super::bench_fmt::{commas, fixed, general, percent, py_sum, signed, Num};
use super::cost::{
    self, BYTES_PER_GB, SECONDS_PER_MONTH, VECTOR_METERING_MIN_BYTES, VECTOR_SEARCH_PRICE_PER_GB,
    VECTOR_WRITE_PRICE_PER_GB,
};
use super::perf_report::{Analysis, Finding, Row, ERROR_RATE_HIGH, TOLERANCE};
use super::pyfmt;

fn safe(text: &str) -> String {
    neutralize_line(text).into_owned()
}

/// `_fmt_delta`.
fn fmt_delta(observed: f64, expected: f64) -> String {
    if expected == 0.0 {
        return "—".to_string();
    }
    format!("{}%", signed(((observed - expected) / expected) * 100.0, 1))
}

/// `_fmt_money`.
#[must_use]
pub fn fmt_money(value: f64) -> String {
    if value >= 100.0 {
        format!("${}", commas(value, 0))
    } else {
        format!("${}", commas(value, 2))
    }
}

fn fmt_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |v| fixed(v, 1))
}

/// `_hot_cold_ratio`.
fn hot_cold_ratio(row: &Row<'_>) -> Option<f64> {
    let kd = row.key_distribution.as_ref()?;
    let (hot, cold) = (kd.hot_partition_p99_ms?, kd.cold_partition_p99_ms?);
    (cold > 0.0).then(|| hot / cold)
}

/// `a or b` for two optional JSON members.
fn first_truthy<'a>(first: Option<&'a Json>, second: Option<&'a Json>) -> Option<&'a Json> {
    [first, second]
        .into_iter()
        .flatten()
        .find(|value| pyfmt::truthy(value))
}

fn member_text(parent: &Json, key: &str, fallback: &str) -> String {
    match parent.get(key).filter(|value| pyfmt::truthy(value)) {
        Some(value) => safe(&pyfmt::text(value)),
        None => fallback.to_string(),
    }
}

fn text_or(parent: &Json, key: &str, default: &str) -> String {
    parent
        .get(key)
        .map_or_else(|| default.to_string(), |value| safe(&pyfmt::text(value)))
}

fn truthy_member(parent: &Json, key: &str) -> bool {
    parent.get(key).is_some_and(pyfmt::truthy)
}

fn object_or_empty(parent: &Json, key: &str) -> Json {
    match parent.get(key) {
        Some(value) if pyfmt::truthy(value) => value.clone(),
        _ => Json::Object(Vec::new()),
    }
}

fn joined(ids: &[&str]) -> String {
    ids.iter().map(|id| safe(id)).collect::<Vec<_>>().join(", ")
}

struct Doc(Vec<String>);

impl Doc {
    fn a(&mut self, text: impl Into<String>) {
        self.0.push(text.into());
    }
}

/// `_render_report`.
#[allow(clippy::too_many_lines)]
pub fn render_report(model: &Json, summary: &Json, analysis: &Analysis<'_>) -> Res<String> {
    let rows = &analysis.rows;
    let signals = &analysis.signals;
    let findings = &analysis.findings;
    let mf = object_or_empty(summary, "manifest");
    let cfg = object_or_empty(summary, "config");
    let mode = first_truthy(summary.get("mode"), cfg.get("mode"));
    let is_representative = mode.and_then(Json::as_str) == Some("representative");
    let account = member_text(&mf, "account", "<unknown>");
    let region = member_text(&mf, "region", "<unknown>");
    let prefix = member_text(&mf, "prefix", "<unknown>");
    let run_id = member_text(&mf, "run_id", "<unknown>");
    let completed_at = member_text(summary, "benchmark_completed_at", "<not recorded>");
    let wall_seconds = summary
        .get("benchmark_wall_seconds")
        .filter(|v| !matches!(v, Json::Null));
    let total_rows = summary
        .get("total_rows")
        .filter(|v| !matches!(v, Json::Null));

    let total_extrapolated = py_sum(
        &rows
            .iter()
            .map(|r| r.extrapolated_monthly)
            .collect::<Vec<_>>(),
    );
    let total_expected = py_sum(&rows.iter().map(|r| r.expected_monthly).collect::<Vec<_>>());
    let delta = (total_expected > 0.0)
        .then(|| (total_extrapolated - total_expected) / total_expected * 100.0);

    let vector_search_monthly = py_sum(
        &rows
            .iter()
            .map(|r| {
                r.observed_vector_search_bytes / BYTES_PER_GB
                    * VECTOR_SEARCH_PRICE_PER_GB
                    * r.declared_peak_rps.as_f64()
                    * SECONDS_PER_MONTH
            })
            .collect::<Vec<_>>(),
    );

    let mut bench_cost = 0.0_f64;
    for r in rows {
        let unit = if cost::is_write_op(&r.op) {
            cost::WRU_PRICE
        } else {
            cost::RRU_PRICE
        };
        bench_cost += r.observed_cu * r.call_count as f64 * unit;
        bench_cost += (r.observed_vector_search_bytes / BYTES_PER_GB * VECTOR_SEARCH_PRICE_PER_GB
            + r.observed_vector_write_bytes / BYTES_PER_GB * VECTOR_WRITE_PRICE_PER_GB)
            * r.call_count as f64;
    }

    let bench_rates: Vec<f64> = rows.iter().map(|r| r.bench_rps).collect();
    let min_rps = cfg_f64(&cfg, "min_rps_per_pattern", 1.0)?;
    let floor_bound =
        !bench_rates.is_empty() && bench_rates.iter().all(|b| (b - min_rps).abs() < 1e-6);
    let rate_str = if bench_rates.is_empty() {
        "unknown rate".to_string()
    } else {
        let lo = bench_rates.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = bench_rates
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        if (hi - lo).abs() < 1e-6 {
            format!("{} rps/pattern", general(lo, 2))
        } else {
            format!("{}–{} rps/pattern", general(lo, 2), general(hi, 2))
        }
    };
    let scale_clause = if floor_bound {
        format!(
            "drove {rate_str} — the `min_rps_per_pattern` floor, BELOW the configured \
             scale_factor, so this is a UNIT-COST sample, not a load test: it validates per-op \
             cost and unloaded latency only, NOT load, throttling, or behaviour at your declared \
             peak"
        )
    } else {
        format!(
            "drove {rate_str} (a fraction of declared peak) for {}s/pattern",
            text_or(&cfg, "duration_seconds", "90")
        )
    };

    let mut doc = Doc(Vec::new());
    doc.a("# DynamoDB Live Performance Report\n");

    let cov = object_or_empty(summary, "coverage");
    let all_zero = !rows.is_empty() && rows.iter().all(|r| r.call_count == 0);
    if truthy_member(&cov, "coverage_incomplete")
        || truthy_member(&cov, "missing_patterns")
        || all_zero
    {
        let missing: Vec<&str> = match cov.get("missing_patterns") {
            Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
            _ => Vec::new(),
        };
        let mut text = String::from(
            "> ⚠️ **INCOMPLETE COVERAGE — do not trust these numbers as a full result.** ",
        );
        if !missing.is_empty() {
            text.push_str("Patterns with no measurement: ");
            text.push_str(&joined(&missing));
            text.push_str(". ");
        }
        if all_zero {
            text.push_str("Every measured pattern recorded zero calls. ");
        }
        text.push_str(
            "The benchmark did not measure every declared pattern (a killed/backgrounded run, a \
             seed failure, or a hand-fed summary). Re-run to completion before drawing \
             conclusions.\n",
        );
        doc.a(text);
    }

    doc.a(format!(
        "> **Disclaimer:** This report measures a scaled-down benchmark that {scale_clause}, \
         against real AWS resources deployed in {account}/{region}. Capacity numbers are live \
         observations from ReturnConsumedCapacity. Monthly-cost figures extrapolate linearly: \
         observed per-op capacity × declared peak RPS × on-demand unit price (full public rate; \
         rates vary by region — confirm against the AWS DynamoDB pricing page for your region). \
         This benchmark does NOT prove the design sustains declared peak RPS — it validates \
         per-op unit cost, latency, and GSI amplification shape. Not measured: stream consumers, \
         TTL sweep, autoscaling, cross-region replication, long-tail bursts, any non-DDB \
         services in the design.\n"
    ));
    if is_representative {
        doc.a(format!(
            "> **Representative mode:** this run drove ~{}× declared peak with **zipf hot-key \
             sampling** and **realistic item-collection cardinality** ({} items/partition) to \
             surface SCALE risk — hot-partition throttling, throttle-under-load, GSI \
             amplification at volume, and Query-at-cardinality. **Throttle and latency numbers \
             here are load-risk signals collected under deliberate skew at bounded scale — they \
             scale NONLINEARLY with skew and must NOT be linearly extrapolated to peak.** The \
             cost figures above remain valid: they extrapolate per-op capacity (scale-invariant) \
             against declared peak, not the driven bench RPS. One in-region Lambda tops out near \
             1,500–2,000 RPS/pattern, so this surfaces hot-partition risk at bounded cost; it \
             does not prove sustained-peak capacity.\n",
            text_or(&cfg, "scale_factor", "0.15"),
            text_or(&cfg, "items_per_partition", "40"),
        ));
    }

    let delta_text = delta.map_or_else(|| "—".to_string(), |d| format!("{}%", signed(d, 1)));
    doc.a(format!(
        "**Extrapolated Monthly Cost (measurement-based): {}**  *(from steady-state measurement only)*",
        fmt_money(total_extrapolated)
    ));
    doc.a(format!(
        "**Calculator Monthly Cost (expected):              {}**",
        fmt_money(total_expected)
    ));
    doc.a(format!(
        "**Delta:                                           {delta_text}**"
    ));
    if delta.is_some() && total_expected > 0.0 && vector_search_monthly / total_expected > 0.01 {
        doc.a(format!(
            "\n*Of the measured figure, {}/month is vector **search** cost. The calculator \
             deliberately does not price search, so that amount is missing from the expected \
             column by design — it accounts for {} points of the delta above and is not a model \
             error. See **Vector capacity (measured)** below.*",
            fmt_money(vector_search_monthly),
            signed(vector_search_monthly / total_expected * 100.0, 1),
        ));
    }
    doc.a(format!(
        "**This benchmark consumed: ~{} in actual AWS charges** *(seed + warmup + measurement)*\n",
        fmt_money(bench_cost)
    ));

    doc.a("| Source                  | Measured Monthly | Calculator | Δ% |");
    doc.a("| ----------------------- | ---------------- | ---------- | -- |");
    doc.a("| Storage (not measured)  | —                | —          | —  |");
    doc.a(format!(
        "| Read/write requests     | {} | {} | {} |\n",
        pad_right(&fmt_money(total_extrapolated), 16),
        pad_right(&fmt_money(total_expected), 10),
        pad_right(&delta_text, 2)
    ));

    // Deployment.
    doc.a("## Deployment\n");
    doc.a(format!(
        "Account: {account}   Region: {region}   Run ID: {run_id}   "
    ));
    doc.a(format!("Resource prefix: `{prefix}`  "));
    let mut window = vec![format!("completed {completed_at} UTC")];
    if let Some(seconds) = wall_seconds {
        window.push(format!("{}s wall", safe(&pyfmt::text(seconds))));
    }
    if let Some(count) = total_rows {
        window.push(format!("{} measured rows", safe(&pyfmt::text(count))));
    }
    doc.a(format!("Window: {}.  ", window.join(", ")));
    let tables = super::bench::items(model, "tables", "the design")?;
    let mut gsi_count = 0_usize;
    for table in tables {
        gsi_count += super::bench::items(table, "gsis", "a table")?.len();
    }
    doc.a(format!(
        "Resources: {} tables, {gsi_count} GSIs. Manifest: created_resources.json.  ",
        tables.len()
    ));
    doc.a("Teardown: teardown.sh (run manually; the skill does NOT auto-delete).\n");

    // Storage.
    doc.a("## Storage (not benchmarked)\n");
    doc.a(
        "Storage is not measured in a short-window benchmark. See `cost_report.md` for the \
         storage breakdown from the calculator.\n",
    );

    // Seed verification.
    if let Some(Json::Object(sv_map)) = summary
        .get("seed_verification")
        .filter(|v| pyfmt::truthy(v))
    {
        doc.a("## Seed verification\n");
        doc.a("| Table | Expected | Seeded (observed) | Status |");
        doc.a("| --- | --- | --- | --- |");
        for (tname, sv) in sv_map {
            let exp = sv.get("expected").filter(|v| !matches!(v, Json::Null));
            let act = sv.get("actual").filter(|v| !matches!(v, Json::Null));
            let sampled = truthy_member(sv, "sampled");
            let passed = sv.get("passed").is_none_or(pyfmt::truthy);
            let status = if passed {
                if sampled {
                    "ok (sampled to cap)"
                } else {
                    "ok"
                }
            } else if sampled {
                "below cap (sampled)"
            } else {
                "SHORTFALL"
            };
            let act_cell = act.map_or_else(
                || "—".to_string(),
                |value| {
                    format!(
                        "{}{}",
                        safe(&pyfmt::text(value)),
                        if sampled { "+" } else { "" }
                    )
                },
            );
            doc.a(format!(
                "| `{}` | {} | {act_cell} | {status} |",
                safe(tname),
                exp.map_or_else(|| "—".to_string(), |v| safe(&pyfmt::text(v)))
            ));
        }
        doc.a("");
    }

    // Access pattern measurements.
    doc.a("## Access Pattern Measurements\n");
    doc.a("Steady-state only (warmup excluded). See \"Cold start\" below for warmup numbers.\n");
    doc.a(
        "| Pattern | Operation | Table/Index | Peak RPS | Bench RPS | Observed RCU/WCU | Expected \
         RCU/WCU | Δ | p50 ms | p99 ms | Throttles | Errors | Extrapolated Monthly |",
    );
    doc.a("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    let concurrency = match cfg.get("concurrency_per_pattern") {
        Some(value) if pyfmt::truthy(value) => {
            cfg_f64(&cfg, "concurrency_per_pattern", 32.0)? as i64
        }
        _ => 32,
    };
    let saturated_row = |r: &Row<'_>| -> bool {
        let Some(p50) = r.p50_ms.filter(|p| *p != 0.0) else {
            return false;
        };
        if r.bench_rps == 0.0 {
            return false;
        }
        r.bench_rps * (p50 / 1000.0) >= 0.9 * concurrency as f64
    };
    let saturated: Vec<&str> = rows
        .iter()
        .filter(|r| saturated_row(r))
        .map(|r| r.pattern_id.as_str())
        .collect();
    for r in rows {
        let err_cell = if r.errors != 0 {
            let mut cell = format!("{} ({})", r.errors, percent(r.error_rate, 0));
            if r.error_rate >= ERROR_RATE_HIGH {
                cell.push_str(" *");
            }
            cell
        } else {
            "0".to_string()
        };
        let p99_cell = format!(
            "{}{}",
            fmt_ms(r.p99_ms),
            if saturated_row(r) { " †" } else { "" }
        );
        doc.a(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            safe(&r.pattern_id),
            safe(&r.op),
            safe(&r.table_index),
            r.declared_peak_rps.text(),
            fixed(r.bench_rps, 2),
            fixed(r.observed_cu, 3),
            fixed(r.expected_cu, 3),
            fmt_delta(r.observed_cu, r.expected_cu),
            fmt_ms(r.p50_ms),
            p99_cell,
            r.throttles,
            err_cell,
            fmt_money(r.extrapolated_monthly)
        ));
    }
    doc.a("");
    if rows.iter().any(|r| r.error_rate >= ERROR_RATE_HIGH) {
        doc.a(
            "> `*` = this pattern errored on most/all calls (non-throttle). Its Observed \
             RCU/WCU, latency, and Extrapolated Monthly are **not meaningful** — the operation \
             failed. See **Correctness** below.\n",
        );
    }
    if !saturated.is_empty() {
        doc.a(format!(
            "> `†` = **driver-saturated p99 — not a DynamoDB latency.** At the driven rate, the \
             in-flight request count (rps × p50) meets or exceeds the {concurrency} benchmark \
             driver threads, so these requests queue client-side waiting for a worker/connection \
             and the **p99 reflects the single-Lambda load generator, not the service**. p50 is \
             still representative; the true service tail is closer to the p99 of the low-rate \
             patterns. To measure an un-saturated tail at this rate, raise \
             `concurrency_per_pattern` or drive the pattern at a lower rps. Affected: {}.\n",
            joined(&saturated)
        ));
    }

    // Correctness.
    let high_err = &signals.high_error_rate_patterns;
    let minor_err = &signals.minor_error_patterns;
    let struct_err: Vec<_> = high_err
        .iter()
        .filter(|e| e.error_kind != "artifact")
        .collect();
    let artifact_err: Vec<_> = high_err
        .iter()
        .filter(|e| e.error_kind == "artifact")
        .collect();
    if !high_err.is_empty() || !minor_err.is_empty() {
        doc.a("## Correctness (operation errors)\n");
        let err_table = |doc: &mut Doc, entries: &[&super::perf_report::HighError]| {
            doc.a("| Pattern | Op | Error rate | Top reason | Errors / calls |");
            doc.a("| --- | --- | --- | --- | --- |");
            for e in entries {
                let op = rows
                    .iter()
                    .find(|x| x.pattern_id == e.pattern_id)
                    .map_or("?", |r| r.op.as_str());
                let shown = if e.dominant_code.is_empty() {
                    &e.top_error_code
                } else {
                    &e.dominant_code
                };
                doc.a(format!(
                    "| {} | {} | {} | `{}` | {}/{} |",
                    safe(&e.pattern_id),
                    safe(op),
                    percent(e.error_rate, 0),
                    safe(shown),
                    e.errors,
                    e.call_count
                ));
            }
            doc.a("");
        };
        if !struct_err.is_empty() {
            doc.a(
                "**These patterns FAILED on most/all calls (non-throttle errors).** Their cost \
                 and latency numbers above are not meaningful until the operation succeeds. This \
                 is a structural problem in the design or the access-pattern JSON, not a \
                 capacity issue:\n",
            );
            err_table(&mut doc, &struct_err);
            doc.a(
                "Likely causes by error code: `ValidationException` → a Query/GSI names an index \
                 that doesn't exist, an operation references an undefined attribute, or a \
                 batch/transact built duplicate/unseeded keys; `ResourceNotFoundException` → the \
                 pattern's table isn't in the design; `AccessDeniedException` → the benchmark \
                 role lacks the action. Fix the structural cause in `dynamodb_data_model.json` \
                 and re-run; do not interpret the cost numbers for these patterns until they \
                 succeed.\n",
            );
        }
        if !artifact_err.is_empty() {
            doc.a(
                "**Benchmark artifact — NOT a design defect.** These patterns showed a high \
                 error rate, but the failures are `TransactionConflict` / condition-check \
                 rejections, which come from the benchmark driving many concurrent writes \
                 against a small synthetic key pool (`seed_items_per_table`), not from anything \
                 wrong with the schema or the access-pattern JSON. Under real traffic with \
                 unique IDs these do not occur. The successful calls' per-op cost/latency are \
                 still representative — do **not** change the design on account of this error \
                 rate:\n",
            );
            err_table(&mut doc, &artifact_err);
            for e in &artifact_err {
                if !e.cancellation_reason_codes.is_empty() {
                    let mut hist: Vec<&(String, i64)> =
                        e.cancellation_reason_codes.0.iter().collect();
                    hist.sort_by(|a, b| b.1.cmp(&a.1));
                    let parts: Vec<String> = hist
                        .iter()
                        .map(|(code, count)| format!("`{}` ×{count}", safe(code)))
                        .collect();
                    doc.a(format!(
                        "- {} cancellation reasons (per-item, observed): {}.",
                        safe(&e.pattern_id),
                        parts.join(", ")
                    ));
                }
            }
            doc.a(
                "To drive a clean error rate, raise `seed_items_per_table` well above the write \
                 RPS for these patterns, or lower the driven rate, then re-run.\n",
            );
        }
        if !minor_err.is_empty() {
            let parts: Vec<String> = minor_err
                .iter()
                .map(|e| {
                    format!(
                        "{} ({}/{}, `{}`)",
                        safe(&e.pattern_id),
                        e.errors,
                        e.call_count,
                        safe(&e.top_error_code)
                    )
                })
                .collect();
            doc.a(format!(
                "Patterns with a LOW but non-zero error rate (transient or partial — below the \
                 structural threshold, cost numbers still usable): {}.\n",
                parts.join(", ")
            ));
        }
    }

    // Load-risk signals.
    if is_representative {
        doc.a("## Load-risk signals (representative mode only)\n");
        doc.a(
            "Collected under zipf hot-key sampling at bounded scale. **These scale nonlinearly \
             with key skew and must not be linearly extrapolated to peak** — they characterize \
             hot-partition and throttle RISK, not sustained-peak capacity.\n",
        );
        doc.a("| Pattern | Throttles | p99 ms | Observed amp | Top-key share | Distinct keys |");
        doc.a("| --- | --- | --- | --- | --- | --- |");
        for r in rows {
            let kd = r.key_distribution.as_ref();
            let tks = kd.and_then(|k| k.top_key_share);
            let ndk = kd.and_then(|k| k.n_distinct_keys.as_ref());
            doc.a(format!(
                "| {} | {} | {} | {}× | {} | {} |",
                safe(&r.pattern_id),
                r.throttles,
                fmt_ms(r.p99_ms),
                fixed(r.amplification_ratio, 2),
                tks.map_or_else(|| "—".to_string(), |v| percent(v, 0)),
                ndk.filter(|v| !matches!(v, Json::Null))
                    .map_or_else(|| "—".to_string(), |v| safe(&pyfmt::text(v)))
            ));
        }
        doc.a("");
        let skew_pids: Vec<&str> = signals
            .key_skew_patterns
            .iter()
            .map(|k| k.pattern_id.as_str())
            .collect();
        let throttled: Vec<&Row<'_>> = rows.iter().filter(|r| r.throttles > 0).collect();
        let lat_only: Vec<&Row<'_>> = rows
            .iter()
            .filter(|r| skew_pids.contains(&r.pattern_id.as_str()) && r.throttles == 0)
            .collect();
        let throttled_skew: Vec<&&Row<'_>> = throttled
            .iter()
            .filter(|r| hot_cold_ratio(r).is_some_and(|ratio| ratio >= 1.3))
            .collect();
        let throttled_starved: Vec<&&Row<'_>> = throttled
            .iter()
            .filter(|r| !hot_cold_ratio(r).is_some_and(|ratio| ratio >= 1.3))
            .collect();
        let ids = |rows: &[&&Row<'_>]| -> String {
            rows.iter()
                .map(|r| safe(&r.pattern_id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !throttled_skew.is_empty() {
            doc.a(format!(
                "Patterns with steady-state throttles AND a hot partition p99 materially above \
                 the cold partitions' — a hot partition is at the per-partition ceiling \
                 (Mechanics #3, ~1000 WCU / 3000 RCU). Write-shard the partition key (hash \
                 suffix) or re-aggregate: {}.\n",
                ids(&throttled_skew)
            ));
        }
        if !throttled_starved.is_empty() {
            doc.a(format!(
                "Patterns that throttled with hot-partition p99 ≈ cold-partition p99 — this is \
                 **uniform capacity starvation, NOT key skew**: the whole table/GSI was \
                 under-provisioned for the driven load, so every partition throttled equally. \
                 This run did **not** isolate a hot-partition effect for these — do not \
                 attribute it to the partition key. To test skew specifically, re-run with \
                 capacity set *above* uniform demand so only a genuinely hot partition \
                 throttles: {}.\n",
                ids(&throttled_starved)
            ));
        }
        if !lat_only.is_empty() {
            doc.a(format!(
                "Patterns whose hot partition shows **elevated tail latency** (no throttles) \
                 under skew — on an on-demand table this is adaptive capacity absorbing a hot \
                 key as latency rather than rejecting it. It signals the same partition-key \
                 concentration (Mechanics #3) and the same fix (write-shard / re-aggregate); on \
                 a PROVISIONED table the identical skew would throttle. Watch: {}.\n",
                lat_only
                    .iter()
                    .map(|r| safe(&r.pattern_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if throttled.is_empty() && lat_only.is_empty() {
            doc.a(
                "No hot-partition distress (throttles or elevated tail latency) observed under \
                 skew at this scale. (Absence at bounded scale is not proof of headroom at peak; \
                 a provisioned table would throttle sooner than on-demand, which absorbs hot \
                 keys via adaptive capacity.)\n",
            );
        }
    }

    // Cold start.
    doc.a("## Cold start\n");
    doc.a(format!(
        "Settle window: {}s after CreateTable before the first measurement call. Warmup window: \
         {}s per pattern, excluded from percentiles above.\n",
        text_or(&cfg, "table_settle_seconds", "30"),
        text_or(&cfg, "warmup_seconds", "10")
    ));
    doc.a("| Pattern | Warmup p50 ms | Warmup p99 ms | Steady p99 ms | Warmup throttles | Elevated? |");
    doc.a("| --- | --- | --- | --- | --- | --- |");
    for r in rows {
        let cs = &r.cold_start;
        doc.a(format!(
            "| {} | {} | {} | {} | {} | {} |",
            safe(&r.pattern_id),
            fmt_ms(cs.warmup_p50_ms),
            fmt_ms(cs.warmup_p99_ms),
            fmt_ms(r.p99_ms),
            safe(&pyfmt::text(&cs.warmup_throttles)),
            if cs.cold_start_elevated { "yes" } else { "no" }
        ));
    }
    doc.a("");
    doc.a(
        "A pattern is flagged \"Elevated\" when warmup p99 > 2× steady-state p99 — a signal that \
         callers hitting this pattern immediately after deploy will see materially worse latency \
         than the steady-state numbers suggest.\n",
    );

    // Supporting services.
    doc.a("## Supporting services (designed, not benchmarked)\n");
    let mut streams_tables: Vec<String> = Vec::new();
    for table in tables {
        let streams = table.get("streams").filter(|v| pyfmt::truthy(v));
        if streams.is_some_and(|s| truthy_member(s, "enabled")) {
            let Some(Json::String(name)) = table.get("table_name") else {
                return Err(super::bench_fmt::die(
                    "a table is missing its string `table_name`",
                ));
            };
            streams_tables.push(name.clone());
        }
    }
    if streams_tables.is_empty() {
        doc.a("The design references no non-DDB services that require mention here.\n");
    } else {
        for tn in &streams_tables {
            doc.a(format!(
                "- Stream on table `{}`: configured in design; consumers (Lambda / EventBridge \
                 Pipe / Kinesis) not deployed.",
                safe(tn)
            ));
        }
    }
    doc.a("");

    // Axiom findings.
    doc.a("## Axiom Findings\n");
    doc.a("**Validated by this run**\n");
    doc.a(
        "- Mechanics #18: RCU/WCU formulas reproduced within tolerance for patterns where `|Δ| ≤ \
         10%`. See Access Pattern Measurements table.",
    );
    doc.a(
        "- Mechanics #15 (eventual vs strong 2:1): observed where both modes are present in the \
         design; see per-pattern deltas.",
    );
    doc.a("");
    doc.a("**Deviations**\n");
    let error_pids: Vec<&str> = high_err.iter().map(|e| e.pattern_id.as_str()).collect();
    let mut artifact_pids: Vec<&str> = Vec::new();
    for e in high_err.iter().filter(|e| e.error_kind == "artifact") {
        if !artifact_pids.contains(&e.pattern_id.as_str()) {
            artifact_pids.push(&e.pattern_id);
        }
    }
    let mut all_error_pids: Vec<&str> = Vec::new();
    for pid in &error_pids {
        if !all_error_pids.contains(pid) {
            all_error_pids.push(pid);
        }
    }
    let structural_count = all_error_pids
        .iter()
        .filter(|pid| !artifact_pids.contains(pid))
        .count();
    let mut any_dev = false;
    let mut throttled_excluded = 0_usize;
    for r in rows {
        if error_pids.contains(&r.pattern_id.as_str()) {
            continue;
        }
        if r.throttles > 0 {
            throttled_excluded += 1;
            continue;
        }
        if r.delta_pct.is_some_and(|d| d.abs() > TOLERANCE) {
            let likely = signals
                .large_expected_observed_delta_patterns
                .iter()
                .find(|d| d.pattern_id == r.pattern_id)
                .map_or("other", |d| d.likely_cause);
            doc.a(format!(
                "- {}: observed {} vs expected {} (Δ {}). Likely: {likely}.",
                safe(&r.pattern_id),
                fixed(r.observed_cu, 3),
                fixed(r.expected_cu, 3),
                fmt_delta(r.observed_cu, r.expected_cu)
            ));
            any_dev = true;
        }
    }
    if structural_count > 0 {
        doc.a(format!(
            "- ({structural_count} pattern(s) excluded here — they FAILED most/all calls and are \
             reported under **Correctness** above, not as cost deviations.)"
        ));
    }
    if !artifact_pids.is_empty() {
        doc.a(format!(
            "- ({} pattern(s) excluded here — their high error rate is a benchmark artifact \
             (contention on the synthetic key space), not a cost or design issue; see \
             **Correctness** above.)",
            artifact_pids.len()
        ));
    }
    if throttled_excluded > 0 {
        doc.a(format!(
            "- ({throttled_excluded} pattern(s) excluded here — they THROTTLED and are reported \
             under **Load-risk signals** above, not as cost deviations.)"
        ));
    }
    if !any_dev {
        doc.a("- None: every pattern within ±10% of calculator prediction.");
    }
    doc.a("");
    doc.a("**Not validated by this run**\n");
    if is_representative {
        doc.a(
            "- Mechanics #3 (per-partition ceilings 1000 WCU / 3000 RCU): PROBED under zipf skew \
             at bounded scale — see Load-risk signals. Throttles indicate a hot partition near \
             its ceiling ONLY when the hot partition's p99 is materially above the cold \
             partitions'; throttles with hot p99 ≈ cold p99 are uniform capacity starvation, not \
             skew, and do not isolate a hot-partition effect. Absence of throttles is not proof \
             of headroom at full peak.",
        );
    } else {
        doc.a(
            "- Mechanics #3 (per-partition ceilings 1000 WCU / 3000 RCU): bench did not push to \
             ceilings by design.",
        );
    }
    doc.a(
        "- Mechanics #12 (TTL eventual delete): sweep cadence is hours; short window cannot \
         observe.",
    );
    doc.a(
        "- Data Modeling #3, #5 (Streams/PITR/recovery granularity): configuration-level, not \
         traffic-observable.",
    );
    doc.a("- Data Modeling #13 (Global Tables LWW / MRSC): single-region deploy.");
    doc.a("- Patterns #1 (idempotency middleware): application layer, not DDB alone.");
    doc.a("- Integration #1 (consumer idempotency): consumers not deployed.\n");

    // Vector capacity.
    let vector_rows: Vec<&Row<'_>> = rows
        .iter()
        .filter(|r| r.observed_vector_search_bytes != 0.0 || r.observed_vector_write_bytes != 0.0)
        .collect();
    if !vector_rows.is_empty() {
        doc.a("## Vector capacity (measured)\n");
        doc.a(
            "Vector indexes are metered in **bytes**, not RCU/WCU, so none of this appears in \
             the capacity columns above — a `SearchVectors` pattern legitimately consumes 0 CU. \
             These are observed `VectorSearchRequestBytes` and `VectorWriteRequestBytes` per \
             call, taken from `ConsumedCapacity` on this deployment.\n",
        );
        doc.a(format!(
            "`calculate_costs.py` prices vector **storage and writes** but deliberately declines \
             to price **search**: the share of an index that approximate-nearest-neighbour \
             search examines varied by roughly 10x across configurations in testing, so a \
             predicted figure would be confidently wrong. The numbers below close that gap by \
             measurement — per-call bytes observed on this design, at its real dimensions and \
             projection, multiplied by declared peak. Search is billed at ${}/GB and writes at \
             ${}/GB.\n",
            pyfmt::float_repr(VECTOR_SEARCH_PRICE_PER_GB),
            pyfmt::float_repr(VECTOR_WRITE_PRICE_PER_GB)
        ));
        doc.a(
            "| Pattern | Op | Index | Search B/call | Write B/call | Declared peak rps | Vector \
             $/mo |",
        );
        doc.a("| --- | --- | --- | --- | --- | --- | --- |");
        for r in &vector_rows {
            let wb = &r.observed_vector_write_bytes_by_index;
            let idx = match &r.index {
                Some(name) => name.clone(),
                None if !wb.is_empty() => {
                    let mut names: Vec<&str> = wb.iter().map(|(n, _)| n.as_str()).collect();
                    names.sort_unstable();
                    names.join(", ")
                }
                None => "—".to_string(),
            };
            doc.a(format!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                safe(&r.pattern_id),
                safe(&r.op),
                safe(&idx),
                commas(r.observed_vector_search_bytes, 0),
                commas(r.observed_vector_write_bytes, 0),
                r.declared_peak_rps.commas(),
                fmt_money(r.vector_monthly)
            ));
        }
        doc.a("");
        let vector_total = py_sum(
            &vector_rows
                .iter()
                .map(|r| r.vector_monthly)
                .collect::<Vec<_>>(),
        );
        doc.a(format!(
            "**Vector capacity total: {}/month** at declared peak, already included in the \
             extrapolated monthly figures above. Vector index *storage* is not in this total — \
             it rolls into table storage, which the cost report covers.\n",
            fmt_money(vector_total)
        ));
        let min_b = VECTOR_METERING_MIN_BYTES;
        if vector_rows.iter().any(|r| {
            (0.0 < r.observed_vector_search_bytes && r.observed_vector_search_bytes <= min_b)
                || (0.0 < r.observed_vector_write_bytes && r.observed_vector_write_bytes <= min_b)
        }) {
            doc.a(format!(
                "At least one figure is at or below the {}-byte metering minimum, so it reflects \
                 the floor rather than the vector's real size. Low-dimension indexes do not \
                 meter proportionally cheaper; scaling these numbers up by dimension count will \
                 overstate cost until the floor is cleared (~{} dimensions).\n",
                Num::Int(min_b as i64).commas(),
                (min_b as i64) / 4
            ));
        }
    }

    // Cost-estimate validation.
    doc.a("## Cost-estimate validation\n");
    if any_dev {
        doc.a(
            "See **Deviations** above: at least one pattern diverges from the calculator by more \
             than 10%. Investigate per-pattern as called out.\n",
        );
    } else if structural_count > 0 {
        doc.a(format!(
            "Cost validation is INCONCLUSIVE for {structural_count} pattern(s) that errored on \
             most/all calls — their observed capacity is 0 because the operation failed, not \
             because the design is cheap (see **Correctness** above). Fix those patterns and \
             re-run before trusting their cost numbers. The patterns that DID succeed reproduce \
             the calculator within tolerance.\n"
        ));
    } else if !artifact_pids.is_empty() {
        doc.a(format!(
            "Cost numbers are trustworthy. {} pattern(s) showed a high error rate, but it is a \
             benchmark artifact (key-space contention, see **Correctness** above), not a design \
             issue — the successful calls reproduce the calculator within tolerance.\n",
            artifact_pids.len()
        ));
    } else {
        doc.a(
            "Calculator unit-cost formulas reproduce live DynamoDB billing within tolerance for \
             this design; any remaining cost risk lies in the RPS / item-size assumptions fed to \
             the calculator.\n",
        );
    }

    if let Some(top) = signals.dominant_cost_patterns.first() {
        let delta_bit = rows
            .iter()
            .find(|r| r.pattern_id == top.pattern_id)
            .and_then(|r| r.delta_pct)
            .map_or_else(String::new, |d| {
                format!(
                    " (its observed vs expected Δ here is {}%)",
                    signed(d * 100.0, 0)
                )
            });
        doc.a(format!(
            "Cost concentration: `{}` drives ~{} of the estimate, so the headline is dominated \
             by that one pattern's item-size and RPS inputs{delta_bit} — a misstatement there \
             moves the whole number roughly in proportion. Validate those inputs before quoting \
             the figure.\n",
            safe(&top.pattern_id),
            percent(top.share, 0)
        ));
    }

    // Design reflection.
    doc.a("## Design reflection\n");
    doc.a(format!(
        "Authored by the agent from `design_findings.json` ({} findings extracted) and the \
         axioms.\n",
        findings.len()
    ));
    let by_category = |category: &str| -> Vec<&Finding> {
        findings.iter().filter(|f| f.category == category).collect()
    };
    let input_acc = by_category("input-accuracy");
    let design_f = by_category("design");
    let correctness_f = by_category("correctness");

    doc.a("### Correctness findings (fix before reading cost numbers)\n");
    if correctness_f.is_empty() {
        doc.a("- None — every pattern completed its calls without structural errors.");
    }
    for finding in &correctness_f {
        let ev = &finding.evidence;
        if finding.signal == "seed_shortfall" {
            doc.a(format!(
                "- **seed_shortfall** on table `{}`: severity={}. Seeded {} of {} expected \
                 items. Measurements for patterns on this table ran against far less data than \
                 declared (hot-key skew and item-collection cardinality are both wrong) — fix \
                 the seed (or seed volume) and re-run before trusting any number on this table.",
                member_text_or_none(ev, "table"),
                finding.severity,
                member_text_or_none(ev, "actual"),
                member_text_or_none(ev, "expected"),
            ));
            continue;
        }
        let pid_list = joined(
            &finding
                .pattern_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let error_rate = match ev.get("error_rate") {
            Some(Json::Float(v)) => *v,
            Some(Json::Int(v)) => *v as f64,
            _ => 0.0,
        };
        doc.a(format!(
            "- **{}** on {pid_list}: severity={}. Error rate {}, top code `{}` ({}/{} calls). \
             Cost/latency for this pattern are meaningless until it succeeds — fix the \
             structural cause in the design JSON and re-run.",
            finding.signal,
            finding.severity,
            percent(error_rate, 0),
            ev.get("top_error_code")
                .map_or_else(|| "unknown".to_string(), |v| safe(&pyfmt::text(v))),
            ev.get("errors")
                .map_or_else(|| "?".to_string(), pyfmt::text),
            ev.get("call_count")
                .map_or_else(|| "?".to_string(), pyfmt::text),
        ));
    }
    doc.a("");

    doc.a("### Input-accuracy findings (update the inputs, not the design)\n");
    if input_acc.is_empty() {
        doc.a("- None — declared inputs reproduce observed capacity within tolerance.");
    }
    for finding in &input_acc {
        let Some(pid) = finding.pattern_ids.first() else {
            continue;
        };
        let Some(row) = rows.iter().find(|x| &x.pattern_id == pid) else {
            continue;
        };
        doc.a(format!(
            "- {}: observed {}, expected {}. Likely cause: {}. Proposed: update JSON and re-run \
             calculator only.",
            safe(pid),
            fixed(row.observed_cu, 3),
            fixed(row.expected_cu, 3),
            finding
                .evidence
                .get("likely_cause")
                .map_or_else(|| "unknown".to_string(), |v| safe(&pyfmt::text(v)))
        ));
    }
    doc.a("");

    doc.a("### Design findings (the structure itself is the source)\n");
    if design_f.is_empty() {
        doc.a("Measurements support the current design. No alternative is argued for by this run.");
    }
    for finding in &design_f {
        let pid_list = joined(
            &finding
                .pattern_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        doc.a(format!(
            "- **{}** on {pid_list}: severity={}. Axioms: {}. Evidence: {}. See SKILL.md `## Live \
             validation` for the axiom-indexed alternative-design menu and how to express it as \
             a JSON diff.",
            finding.signal,
            finding.severity,
            finding.axioms.join(", "),
            pyfmt::dumps(&finding.evidence, false)
        ));
    }
    doc.a("");

    if !design_f.is_empty() || !correctness_f.is_empty() {
        doc.a("## Iteration offer\n");
        doc.a(
            "- **Calculator-only re-eval** — update the JSON with the proposed changes from the \
             Design findings section above and re-run `scripts/calculate_costs.py`. No AWS \
             calls.",
        );
        doc.a(
            "- **Full re-eval** — calculator + a second live validation against the revised \
             design. Requires re-consenting to AWS deployment and running the prior `teardown.sh` \
             first unless the change is additive-only (e.g. adding a GSI).",
        );
        doc.a(
            "- **No changes** — record the decision as a deviation per Artifact #5 with your \
             stated reason.\n",
        );
    }
    Ok(doc.0.join("\n"))
}

fn member_text_or_none(evidence: &Json, key: &str) -> String {
    evidence
        .get(key)
        .map_or_else(|| "None".to_string(), |value| safe(&pyfmt::text(value)))
}

fn pad_right(text: &str, width: usize) -> String {
    toolkit_msk::pyfmt::pad_right(text, width)
}
