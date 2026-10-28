//! Planning half of `benchmark_model.py` (Issue #4390): mode presets, design
//! validation, the invocation split, the vector-index metadata attached to
//! each pattern, the bench rate and the pre-spend estimate.
//!
//! Everything here is pure: no AWS call, no clock, no file. The orchestration
//! that invokes the benchmark Lambda through a client lives in
//! `crate::dynamodb_bench`; the row aggregation is in [`super::bench_rows`].
//!
//! Python reads the design and the config with `dict.get` and `or`; this port
//! keeps those rules (absent, `null`, `0`, `""`, `[]` and `{}` mean "not set")
//! and refuses a truthy value of the wrong type with a message, where the
//! script crashed with a traceback.

use std::collections::BTreeMap;

use toolkit_core::{Json, ToolkitError};

use super::bench_fmt::{diag, diag_list, die, fixed, py_floordiv, py_max, py_min};
use super::cost::{self, EntitySizes};
use super::pyfmt;
use crate::Output;

/// Result alias of this module.
pub type Res<T> = Result<T, ToolkitError>;

/// `quick` preset (`_QUICK_PRESET`).
pub const QUICK_PRESET: &[(&str, PresetValue)] = &[
    ("table_settle_seconds", PresetValue::Int(10)),
    ("warmup_seconds", PresetValue::Int(3)),
    ("duration_seconds", PresetValue::Int(15)),
    ("ramp_seconds", PresetValue::Int(3)),
    ("seed_items_per_table", PresetValue::Int(100)),
];

/// `representative` preset (`_REPRESENTATIVE_PRESET`).
pub const REPRESENTATIVE_PRESET: &[(&str, PresetValue)] = &[
    ("table_settle_seconds", PresetValue::Int(45)),
    ("warmup_seconds", PresetValue::Int(15)),
    ("duration_seconds", PresetValue::Int(120)),
    ("ramp_seconds", PresetValue::Int(20)),
    ("seed_items_per_table", PresetValue::Int(2000)),
    ("items_per_partition", PresetValue::Int(40)),
    ("scale_factor", PresetValue::Float(0.15)),
    ("max_rps_per_pattern", PresetValue::Int(1800)),
    ("min_rps_per_pattern", PresetValue::Int(5)),
    ("concurrency_per_pattern", PresetValue::Int(32)),
    ("read_pattern_key_sampling", PresetValue::Text("zipf")),
    ("zipf_s", PresetValue::Float(1.1)),
];

/// One preset value.
#[derive(Debug, Clone, Copy)]
pub enum PresetValue {
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Text(&'static str),
}

impl PresetValue {
    fn to_json(self) -> Json {
        match self {
            Self::Int(value) => Json::Int(value),
            Self::Float(value) => Json::Float(value),
            Self::Text(value) => Json::string(value),
        }
    }
}

/// The write operations of the spend estimate (`calculate_costs.WRITE_OPS`).
pub use super::cost::is_write_op;

// ---------------------------------------------------------------------------
// Dynamic reads (Python `dict.get` / `or` semantics).
// ---------------------------------------------------------------------------

/// `parent.get(key) or []`: an array member, or empty when the member is not
/// set. A truthy member of another type is refused.
pub fn items<'a>(parent: &'a Json, key: &str, what: &str) -> Res<&'a [Json]> {
    match parent.get(key) {
        None => Ok(&[]),
        Some(value) if !pyfmt::truthy(value) => Ok(&[]),
        Some(Json::Array(list)) => Ok(list.as_slice()),
        Some(other) => Err(die(format!(
            "{what} `{key}` must be a list (it is {})",
            other.type_name()
        ))),
    }
}

/// Every item of `list` must be an object.
pub fn objects<'a>(list: &'a [Json], what: &str) -> Res<Vec<&'a Json>> {
    list.iter()
        .map(|item| match item {
            Json::Object(_) => Ok(item),
            other => Err(die(format!(
                "each {what} must be an object (one is {})",
                other.type_name()
            ))),
        })
        .collect()
}

/// `float(cfg.get(key, default))`.
pub fn cfg_f64(cfg: &Json, key: &str, default: f64) -> Res<f64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(Json::Int(number)) => Ok(*number as f64),
        Some(Json::Float(number)) => Ok(*number),
        Some(Json::Bool(flag)) => Ok(f64::from(u8::from(*flag))),
        Some(Json::String(text)) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| die(format!("the benchmark config `{key}` must be a number"))),
        Some(other) => Err(die(format!(
            "the benchmark config `{key}` must be a number (it is {})",
            other.type_name()
        ))),
    }
}

/// `int(cfg.get(key, default))`.
pub fn cfg_i64(cfg: &Json, key: &str, default: i64) -> Res<i64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(value) => cost::py_int(value).ok_or_else(|| {
            die(format!(
                "the benchmark config `{key}` must be an integer (it is {})",
                value.type_name()
            ))
        }),
    }
}

/// `float(pattern.get("peak_rps", 0) or 0)`.
fn peak_rps(pattern: &Json) -> Res<f64> {
    match pattern.get("peak_rps") {
        None => Ok(0.0),
        Some(value) if !pyfmt::truthy(value) => Ok(0.0),
        Some(Json::Int(number)) => Ok(*number as f64),
        Some(Json::Float(number)) => Ok(*number),
        Some(Json::Bool(_)) => Ok(1.0),
        Some(Json::String(text)) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| die("a pattern `peak_rps` must be a number")),
        Some(other) => Err(die(format!(
            "a pattern `peak_rps` must be a number (it is {})",
            other.type_name()
        ))),
    }
}

fn pattern_text(pattern: &Json, key: &str, default: &str) -> String {
    pattern
        .get(key)
        .map_or_else(|| default.to_string(), pyfmt::text)
}

fn pattern_id(pattern: &Json) -> String {
    diag(&pattern_text(pattern, "pattern_id", "?"))
}

fn upsert(pairs: &mut Vec<(String, Json)>, key: &str, value: Json) {
    if let Some(slot) = pairs.iter_mut().find(|(name, _)| name == key) {
        slot.1 = value;
    } else {
        pairs.push((key.to_string(), value));
    }
}

// ---------------------------------------------------------------------------
// _apply_mode_preset
// ---------------------------------------------------------------------------

/// `_apply_mode_preset`: overlays the `quick` or `representative` preset on
/// the config (explicit values win), refuses an unknown mode, and prints the
/// mode banner on stdout.
pub fn apply_mode_preset(cfg: &Json, out: &mut Output<'_>) -> Res<Json> {
    if !matches!(cfg, Json::Object(_)) {
        return Err(die("the benchmark config must be a JSON object"));
    }
    let mode = match cfg.get("mode") {
        None | Some(Json::Null) => return Ok(cfg.clone()),
        Some(Json::String(text)) if text == "standard" => return Ok(cfg.clone()),
        Some(Json::String(text)) => text.as_str(),
        Some(other) => {
            return Err(unknown_mode(&pyfmt::text(other)));
        }
    };
    let preset = match mode {
        "quick" => QUICK_PRESET,
        "representative" => REPRESENTATIVE_PRESET,
        other => return Err(unknown_mode(other)),
    };
    let Json::Object(original) = cfg else {
        return Err(die("the benchmark config must be a JSON object"));
    };
    let mut merged = original.clone();
    for (key, value) in preset {
        if cfg.get(key).is_none() {
            merged.push(((*key).to_string(), value.to_json()));
        }
    }
    let merged = Json::Object(merged);
    // Ordinary numbers keep their Python rendering; every other value is caller
    // text (not yet validated) and goes through redact -> bound -> escape.
    let show = |key: &str, default: &str| -> String {
        merged
            .get(key)
            .map_or_else(|| default.to_string(), banner_value)
    };
    if mode == "quick" {
        out.line(&format!(
            "Quick mode: running a short per-pattern window (settle={}s, warmup={}s, \
             duration={}s, seed={} items). Percentiles and extrapolation are less stable than \
             the standard mode — treat this run as a smoke test, not a cost-validation result.",
            show("table_settle_seconds", "None"),
            show("warmup_seconds", "None"),
            show("duration_seconds", "None"),
            show("seed_items_per_table", "None"),
        ))?;
    } else {
        out.line(&format!(
            "Representative mode: scale ~{}× declared peak, zipf hot-key sampling ON, capped at \
             {} RPS/pattern, {} items/partition. Surfaces hot-partition throttling, \
             throttle-under-load, GSI amplification at volume, and Query-at-realistic-cardinality. \
             ONE in-region Lambda (32 threads) tops out near 1500–2000 RPS/pattern — \
             representative mode is BOUNDED and does NOT prove the design sustains declared peak \
             RPS. Per-op cost extrapolation stays linear and valid; throttle/latency numbers are \
             load-risk signals, not capacity-sustain proof — never extrapolate them linearly.",
            show("scale_factor", "0.15"),
            show("max_rps_per_pattern", "1800"),
            show("items_per_partition", "40"),
        ))?;
    }
    Ok(merged)
}

/// One config value inside a mode banner. Short numbers, booleans and null print
/// as Python printed them; anything else is caller text and is redacted, bounded
/// and escaped onto one line.
fn banner_value(value: &Json) -> String {
    let text = pyfmt::text(value);
    match value {
        Json::Int(_) | Json::Float(_) | Json::Bool(_) | Json::Null
            if text.chars().count() <= 40 =>
        {
            text
        }
        _ => diag(&text),
    }
}

fn unknown_mode(mode: &str) -> ToolkitError {
    die(format!(
        "unknown benchmark mode {}. Valid values: \"quick\", \"representative\", \"standard\" \
         (default).",
        diag(mode)
    ))
}

// ---------------------------------------------------------------------------
// _validate
// ---------------------------------------------------------------------------

fn names_of(list: Option<&Json>, key: &str) -> Res<Vec<String>> {
    let Some(table) = list else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in items(table, key, "a table")? {
        if !matches!(entry, Json::Object(_)) {
            return Err(die(format!(
                "each `{key}` entry of a table must be an object"
            )));
        }
        if let Some(Json::String(name)) = entry.get("index_name") {
            names.push(name.clone());
        }
    }
    Ok(names)
}

fn sorted_truthy(mut names: Vec<String>) -> Vec<String> {
    names.retain(|name| !name.is_empty());
    names.sort();
    names.dedup();
    names
}

/// The tables of a design by name (a later table of the same name wins, like
/// the dict comprehension).
pub fn tables_by_name(model: &Json) -> Res<BTreeMap<String, &Json>> {
    let mut map = BTreeMap::new();
    for table in objects(items(model, "tables", "the design")?, "table")? {
        if let Some(Json::String(name)) = table.get("table_name") {
            map.insert(name.clone(), table);
        }
    }
    Ok(map)
}

/// `_validate`: refuses a design the benchmark cannot run (no patterns, a
/// pattern without a peak rate, a missing table or an index of the wrong
/// family).
pub fn validate(model: &Json) -> Res<()> {
    if !matches!(model, Json::Object(_)) {
        return Err(die("the design JSON must be an object"));
    }
    let aps = objects(
        items(model, "access_patterns", "the design")?,
        "access pattern",
    )?;
    if aps.is_empty() {
        return Err(die("access_patterns empty — refusing (Mechanics #2)."));
    }
    for ap in &aps {
        if !ap.get("peak_rps").is_some_and(pyfmt::truthy) {
            return Err(die(format!(
                "pattern {} has missing or zero peak_rps — refusing per Mechanics #2.",
                pattern_id(ap)
            )));
        }
    }
    let tables = tables_by_name(model)?;
    for ap in &aps {
        let pid = pattern_id(ap);
        let tn = ap.get("table");
        let Some(tn) = tn.filter(|value| pyfmt::truthy(value)) else {
            return Err(die(format!(
                "pattern {pid} has no \"table\" — every pattern must name the table it runs \
                 against."
            )));
        };
        let table_text = pyfmt::text(tn);
        let td = match tn {
            Json::String(name) => tables.get(name).copied(),
            _ => None,
        };
        let Some(td) = td else {
            let defined: Vec<&String> = tables.keys().collect();
            return Err(die(format!(
                "pattern {pid} references table {}, not defined in tables[]. Defined tables: {}.",
                diag(&table_text),
                diag_list(&defined)
            )));
        };
        let idx = ap.get("index").filter(|value| pyfmt::truthy(value));
        let op = ap.get("operation");
        let op_text = op.map_or_else(|| "None".to_string(), pyfmt::text);
        let gsi_names = names_of(Some(td), "gsis")?;
        let vec_names = names_of(Some(td), "vector_indexes")?;
        let idx_name = idx.and_then(Json::as_str);
        let in_names =
            |names: &[String]| idx_name.is_some_and(|name| names.iter().any(|n| n == name));

        if op.and_then(Json::as_str) == Some("SearchVectors") {
            let Some(idx) = idx else {
                return Err(die(format!(
                    "pattern {pid} is a SearchVectors but names no \"index\"."
                )));
            };
            if !in_names(&vec_names) {
                return Err(die(format!(
                    "pattern {pid} searches vector index {} on table {}, but that table defines \
                     no such vector index. Defined vector indexes: {}. Fix the design JSON.",
                    diag(&pyfmt::text(idx)),
                    diag(&table_text),
                    diag_list(&sorted_truthy(vec_names))
                )));
            }
        } else if let Some(idx) = idx {
            let idx_text = diag(&pyfmt::text(idx));
            if in_names(&vec_names) {
                return Err(die(format!(
                    "pattern {pid} targets {idx_text} on table {} with operation {}, but \
                     {idx_text} is a VECTOR index. Query and Scan are rejected against one \
                     ('Query operation not supported on this index type'). Use operation \
                     \"SearchVectors\".",
                    diag(&table_text),
                    diag(&op_text)
                )));
            }
            if !in_names(&gsi_names) {
                return Err(die(format!(
                    "pattern {pid} uses index {idx_text} on table {}, but that table defines no \
                     such GSI. Defined GSIs: {}. A Query/Scan against a non-existent index fails \
                     every call — fix the design JSON.",
                    diag(&table_text),
                    diag_list(&sorted_truthy(gsi_names))
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// _compute_split
// ---------------------------------------------------------------------------

/// The seed wall-clock estimate shared by the split and the wall-clock banner:
/// `int(30 + (seed_items / 25.0) * 0.03 * max(1, n_patterns))`.
pub fn seed_seconds(cfg: &Json, n_patterns: usize) -> Res<i64> {
    let seed_items = cfg_i64(cfg, "seed_items_per_table", 500)?;
    let scale = n_patterns.max(1) as f64;
    Ok((30.0 + (seed_items as f64 / 25.0) * 0.03 * scale) as i64)
}

/// `_compute_split`: `(invocations_total, usable_seconds)`. `-1` means that
/// even one invocation's settle + seed + warmup overhead exceeds the usable
/// Lambda timeout.
pub fn compute_split(cfg: &Json, n_patterns: usize) -> Res<(i64, f64)> {
    let settle = cfg_i64(cfg, "table_settle_seconds", 30)?;
    let seed = seed_seconds(cfg, n_patterns)?;
    let warmup = cfg_i64(cfg, "warmup_seconds", 10)?;
    let duration = cfg_i64(cfg, "duration_seconds", 90)?;
    let aggregation_margin: i64 = 20;
    let lambda_timeout = cfg_i64(cfg, "lambda_timeout_seconds", 900)?;
    let usable = lambda_timeout as f64 * 0.9;
    let patterns = i64::try_from(n_patterns).unwrap_or(i64::MAX);

    let first_overhead = settle + seed + patterns * warmup + aggregation_margin;
    let subsequent_overhead = aggregation_margin;
    let total_measure_time = patterns * duration;

    if (first_overhead + total_measure_time) as f64 <= usable {
        return Ok((1, usable));
    }
    let first_slice_budget = py_max(0.0, usable - first_overhead as f64);
    let subsequent_slice_budget = py_max(0.0, usable - subsequent_overhead as f64);
    let per_invocation_measure_budget = py_min(first_slice_budget, subsequent_slice_budget);
    if per_invocation_measure_budget <= 0.0 {
        return Ok((-1, usable));
    }
    let slice_per_pattern =
        (py_floordiv(per_invocation_measure_budget, patterns as f64) as i64).max(1);
    let per_invocation_total_measure = slice_per_pattern * patterns;
    // -(-a // b): integer ceiling division.
    let ceiling = (total_measure_time + per_invocation_total_measure - 1)
        .div_euclid(per_invocation_total_measure);
    Ok((ceiling.max(2), usable))
}

// ---------------------------------------------------------------------------
// _attach_vector_index_meta
// ---------------------------------------------------------------------------

/// `_attach_vector_index_meta`: resolves each SearchVectors pattern's vector
/// index onto the pattern (`vector_index` member), the metadata the Lambda
/// driver needs.
pub fn attach_vector_index_meta(model: &Json) -> Res<Vec<Json>> {
    let tables = tables_by_name(model)?;
    let mut out = Vec::new();
    for ap in items(model, "access_patterns", "the design")? {
        if ap.get("operation").and_then(Json::as_str) != Some("SearchVectors") {
            out.push(ap.clone());
            continue;
        }
        let td = ap
            .get("table")
            .and_then(Json::as_str)
            .and_then(|name| tables.get(name).copied());
        let empty = Json::Object(Vec::new());
        let td_json = td.unwrap_or(&empty);
        let wanted = ap.get("index");
        let mut found: Option<&Json> = None;
        for vi in items(td_json, "vector_indexes", "a table")? {
            if vi.get("index_name") == wanted && wanted.is_some() {
                found = Some(vi);
                break;
            }
        }
        let Some(vi) = found else {
            return Err(die(format!(
                "access pattern {} searches vector index {} on table {}, which declares no such \
                 vector index. Fix the model before benchmarking — otherwise every call fails \
                 and the run reports the pattern as free.",
                diag(&pattern_text(ap, "pattern_id", "None")),
                diag(&pattern_text(ap, "index", "None")),
                diag(&pattern_text(ap, "table", "None")),
            )));
        };
        let schema = vi.get("search_schema").filter(|v| pyfmt::truthy(v));
        let pk = schema.and_then(|s| s.get("partition_key"));
        let mut pk_type = Json::string("S");
        for ent in items(td_json, "entities", "a table")? {
            for attr in items(ent, "attributes", "an entity")? {
                if attr.get("name") == pk && pk.is_some() {
                    pk_type = attr
                        .get("type")
                        .cloned()
                        .unwrap_or_else(|| Json::string("S"));
                }
            }
        }
        for attr in items(td_json, "attribute_definitions", "a table")? {
            let name = [attr.get("attribute_name"), attr.get("name")]
                .into_iter()
                .flatten()
                .find(|value| pyfmt::truthy(value))
                .or_else(|| attr.get("name"));
            if name == pk && pk.is_some() {
                let candidate = [attr.get("attribute_type"), attr.get("type")]
                    .into_iter()
                    .flatten()
                    .find(|value| pyfmt::truthy(value));
                if let Some(found_type) = candidate {
                    pk_type = found_type.clone();
                }
            }
        }
        let filters = match schema.and_then(|s| s.get("inline_filters")) {
            Some(value) if pyfmt::truthy(value) => match value {
                Json::Array(list) => Json::Array(list.clone()),
                _ => return Err(die("a search_schema `inline_filters` must be a list")),
            },
            _ => Json::Array(Vec::new()),
        };
        let mut pairs = match ap {
            Json::Object(pairs) => pairs.clone(),
            _ => Vec::new(),
        };
        upsert(
            &mut pairs,
            "vector_index",
            Json::object(vec![
                (
                    "index_name",
                    vi.get("index_name").cloned().unwrap_or(Json::Null),
                ),
                (
                    "vector_attribute",
                    vi.get("vector_attribute").cloned().unwrap_or(Json::Null),
                ),
                (
                    "dimensions",
                    vi.get("dimensions").cloned().unwrap_or(Json::Null),
                ),
                ("partition_key", pk.cloned().unwrap_or(Json::Null)),
                ("pk_type", pk_type),
                ("inline_filters", filters),
            ]),
        );
        out.push(Json::Object(pairs));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// _bench_rps and the pre-spend estimate
// ---------------------------------------------------------------------------

/// `_bench_rps`: the rate the benchmark drives a pattern at,
/// `max(min_rps, min(max_rps, peak_rps * scale_factor))`.
pub fn bench_rps(pattern: &Json, cfg: &Json) -> Res<f64> {
    let declared = peak_rps(pattern)?;
    let scaled = declared * cfg_f64(cfg, "scale_factor", 0.01)?;
    Ok(py_max(
        cfg_f64(cfg, "min_rps_per_pattern", 1.0)?,
        py_min(cfg_f64(cfg, "max_rps_per_pattern", 50.0)?, scaled),
    ))
}

/// One pattern's share of the estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternSpend {
    /// The pattern id.
    pub pattern_id: Json,
    /// The driven rate.
    pub bench_rps: f64,
    /// `bench_rps * (warmup + duration)`.
    pub calls: f64,
    /// Estimated dollars for the pattern (capacity plus vector bytes).
    pub cost: f64,
    /// Vector bytes dollars, when any.
    pub vector_cost: f64,
}

/// `_estimate_bench_spend`: the AWS charge a run will incur, before running it.
#[derive(Debug, Clone, PartialEq)]
pub struct Spend {
    /// Driven load plus seeding.
    pub total_usd: f64,
    /// Σ patterns of calls × per-call capacity × unit price (+ vector bytes).
    pub driven_usd: f64,
    /// Seed writes.
    pub seed_usd: f64,
    /// Vector bytes in either part (zero for a design without vectors).
    pub vector_usd: f64,
    /// Per-pattern detail.
    pub per_pattern: Vec<PatternSpend>,
    /// `warmup + duration`.
    pub window_seconds: f64,
}

/// `_estimate_bench_spend`.
pub fn estimate_bench_spend(model: &Json, cfg: &Json) -> Res<Spend> {
    let aps = items(model, "access_patterns", "the design")?;
    let tables = items(model, "tables", "the design")?;
    let mut table_map: BTreeMap<String, &Json> = BTreeMap::new();
    for table in objects(tables, "table")? {
        let Some(Json::String(name)) = table.get("table_name") else {
            return Err(die("a table is missing its string `table_name`"));
        };
        table_map.insert(name.clone(), table);
    }
    let sizes: EntitySizes = cost::build_entity_attr_sizes(tables)?;

    let warmup = cfg_f64(cfg, "warmup_seconds", 10.0)?;
    let duration = cfg_f64(cfg, "duration_seconds", 90.0)?;
    let window = warmup + duration;
    let empty = Json::Object(Vec::new());

    let mut driven_cost = 0.0_f64;
    let mut vector_cost = 0.0_f64;
    let mut per_pattern = Vec::new();
    for ap in aps {
        let rps = bench_rps(ap, cfg)?;
        let calls = rps * window;
        let op = pattern_text(ap, "operation", "GetItem");
        let is_write = is_write_op(&op);
        let td: Option<&Json> = ap
            .get("table")
            .and_then(Json::as_str)
            .and_then(|name| table_map.get(name).copied());
        let cu_per_call = match cost::pattern_monthly_cost(ap, td, Some(&sizes)) {
            Ok(pc) => pc.cap.rcus + pc.cap.wcus,
            Err(_) => 1.0,
        };
        let unit = if is_write {
            cost::WRU_PRICE
        } else {
            cost::RRU_PRICE
        };
        let td_or_empty = td.unwrap_or(&empty);
        let vector_indexes = items(td_or_empty, "vector_indexes", "a table")?;
        let mut named_indexes: Vec<(&str, &Json)> = Vec::new();
        for vi in vector_indexes {
            let Some(Json::String(name)) = vi.get("index_name") else {
                return Err(die("a vector index is missing its string `index_name`"));
            };
            named_indexes.push((name.as_str(), vi));
        }
        let mut v_per_call = 0.0_f64;
        if op == cost::VECTOR_SEARCH_OP {
            let vi = ap
                .get("index")
                .and_then(Json::as_str)
                .and_then(|name| {
                    named_indexes
                        .iter()
                        .rev()
                        .find(|(candidate, _)| *candidate == name)
                        .map(|(_, vi)| *vi)
                })
                .filter(|vi| pyfmt::truthy(vi));
            if let Some(vi) = vi {
                v_per_call = cost::vector_search_bytes_spend_gate_upper(ap, vi, td_or_empty)
                    .map_or(0.0, |bytes| {
                        bytes / cost::BYTES_PER_GB * cost::VECTOR_SEARCH_PRICE_PER_GB
                    });
            }
        } else if is_write && !named_indexes.is_empty() {
            v_per_call = cost::vector_write_bytes_per_call(ap, td_or_empty)
                .map_or(0.0, |(wb, _)| {
                    wb / cost::BYTES_PER_GB * cost::VECTOR_WRITE_PRICE_PER_GB
                });
        }
        let v_cost = calls * v_per_call;
        vector_cost += v_cost;
        let cost_here = calls * cu_per_call * unit + v_cost;
        driven_cost += cost_here;
        per_pattern.push(PatternSpend {
            pattern_id: ap.get("pattern_id").cloned().unwrap_or(Json::Null),
            bench_rps: rps,
            calls,
            cost: cost_here,
            vector_cost: v_cost,
        });
    }

    // Seeding: run_seed seeds PER PATTERN and prices every pattern at the
    // largest declared item size on its table.
    let seed_items = cfg_i64(cfg, "seed_items_per_table", 500)?;
    let mut table_max_size: Vec<(String, i64)> = Vec::new();
    for ap in aps {
        let Some(tn) = ap.get("table").filter(|v| pyfmt::truthy(v)) else {
            continue;
        };
        let name = pyfmt::text(tn);
        let size = match ap.get("estimated_item_size_bytes") {
            None => 1024,
            Some(value) => cost::py_int(value)
                .ok_or_else(|| die("a pattern `estimated_item_size_bytes` must be an integer"))?,
        };
        if let Some(slot) = table_max_size.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = slot.1.max(size);
        } else {
            table_max_size.push((name, 0_i64.max(size)));
        }
    }
    let mut seed_cost = 0.0_f64;
    for ap in aps {
        let Some(tn) = ap.get("table").filter(|v| pyfmt::truthy(v)) else {
            continue;
        };
        let name = pyfmt::text(tn);
        let max_size = table_max_size
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0, |(_, size)| *size);
        // -(-size // 1024): ceiling KB, at least 1.
        let item_kb = (-((-max_size).div_euclid(1024))).max(1);
        seed_cost += seed_items as f64 * item_kb as f64 * cost::WRU_PRICE;
        let td = tn.as_str().and_then(|n| table_map.get(n).copied());
        let td_or_empty = td.unwrap_or(&empty);
        let vis = items(td_or_empty, "vector_indexes", "a table")?;
        if td.is_some() && !vis.is_empty() {
            let written: Vec<Json> = vis
                .iter()
                .filter_map(|vi| vi.get("vector_attribute").filter(|v| pyfmt::truthy(v)))
                .cloned()
                .collect();
            let write_ap = Json::object(vec![
                ("operation", Json::string("PutItem")),
                ("estimated_item_size_bytes", Json::Int(max_size)),
                ("attributes_written", Json::Array(written)),
            ]);
            if let Ok((wb, _)) = cost::vector_write_bytes_per_call(&write_ap, td_or_empty) {
                let v =
                    seed_items as f64 * wb / cost::BYTES_PER_GB * cost::VECTOR_WRITE_PRICE_PER_GB;
                seed_cost += v;
                vector_cost += v;
            }
        }
    }

    Ok(Spend {
        total_usd: driven_cost + seed_cost,
        driven_usd: driven_cost,
        seed_usd: seed_cost,
        vector_usd: vector_cost,
        per_pattern,
        window_seconds: window,
    })
}

/// `Estimated wall-clock` figures of the foreground banner: settle, seed,
/// warmup, duration and the total seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClock {
    /// `table_settle_seconds`.
    pub settle: i64,
    /// Seed estimate.
    pub seed: i64,
    /// `warmup_seconds`.
    pub warmup: i64,
    /// `duration_seconds`.
    pub duration: i64,
    /// `settle + seed + n × (warmup + duration) + invocations × 20`.
    pub total: i64,
}

/// The upfront wall-clock estimate (`_est_s`).
pub fn wall_clock(cfg: &Json, n_patterns: usize, invocations_total: i64) -> Res<WallClock> {
    let settle = cfg_i64(cfg, "table_settle_seconds", 30)?;
    let seed = seed_seconds(cfg, n_patterns)?;
    let warmup = cfg_i64(cfg, "warmup_seconds", 10)?;
    let duration = cfg_i64(cfg, "duration_seconds", 90)?;
    let patterns = i64::try_from(n_patterns).unwrap_or(i64::MAX);
    Ok(WallClock {
        settle,
        seed,
        warmup,
        duration,
        total: settle + seed + patterns * (warmup + duration) + invocations_total * 20,
    })
}

/// The three banner lines about the spend estimate and the guardrail, as the
/// script prints them (`~$x.xxx`).
#[must_use]
pub fn spend_line(spend: &Spend, guardrail: f64) -> String {
    format!(
        "Estimated AWS spend for this run: ~${} (driven load ~${} + seeding ~${}). Guardrail: ${}.",
        fixed(spend.total_usd, 3),
        fixed(spend.driven_usd, 3),
        fixed(spend.seed_usd, 3),
        fixed(guardrail, 2)
    )
}

/// The refusal text of the cost guardrail (exit status 3).
#[must_use]
pub fn guardrail_refusal(spend: &Spend, guardrail: f64) -> String {
    format!(
        "estimated spend ${} exceeds the cost guardrail ${}. Lower scale_factor / \
         duration_seconds / max_rps_per_pattern, raise cost_guardrail_usd in \
         benchmark_config.json, or re-run with --allow-spend to proceed after acknowledging the \
         charge.",
        fixed(spend.total_usd, 3),
        fixed(guardrail, 2)
    )
}

/// `{:.0f}` minutes of the wall-clock banner.
#[must_use]
pub fn minutes(seconds: i64) -> String {
    fixed(seconds as f64 / 60.0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    fn j(text: &str) -> Json {
        parse_json(text).expect("fixture json")
    }

    const MODEL: &str = r#"{
      "tables": [{"table_name": "Orders",
        "entities": [{"entity_name": "Order", "estimated_item_size_bytes": 1024,
                      "attributes": [{"name": "pk", "type": "S"}]}],
        "gsis": [{"index_name": "ByCustomer"}],
        "vector_indexes": [{"index_name": "Embed", "vector_attribute": "vec", "dimensions": 8,
                            "search_schema": {"partition_key": "pk", "inline_filters": ["a"]}}]}],
      "access_patterns": [
        {"pattern_id": "Get", "operation": "GetItem", "table": "Orders", "peak_rps": 100},
        {"pattern_id": "Put", "operation": "PutItem", "table": "Orders", "peak_rps": 50}]}"#;

    const PLAIN: &str = r#"{
      "tables": [{"table_name": "Orders",
        "entities": [{"entity_name": "Order", "estimated_item_size_bytes": 1024,
                      "attributes": [{"name": "pk", "type": "S"}]}]}],
      "access_patterns": [
        {"pattern_id": "Get", "operation": "GetItem", "table": "Orders", "peak_rps": 100},
        {"pattern_id": "Put", "operation": "PutItem", "table": "Orders", "peak_rps": 50}]}"#;

    fn refusal(model: &str) -> String {
        validate(&j(model))
            .expect_err("refused")
            .message()
            .to_string()
    }

    #[test]
    fn split_fits_one_invocation_or_divides_the_measurement() {
        let (one, usable) = compute_split(&j("{}"), 2).expect("split");
        assert_eq!((one, usable), (1, 810.0));
        // 31 patterns at the defaults: first overhead 408 s, 2790 s of measurement,
        // slice floor(402 / 31) = 12 s per pattern, ceil(2790 / 372) = 8 invocations.
        assert_eq!(compute_split(&j("{}"), 31).expect("split").0, 8);
        let hopeless = j(r#"{"warmup_seconds": 100}"#);
        assert_eq!(compute_split(&hopeless, 10).expect("split").0, -1);
        let two = j(
            r#"{"lambda_timeout_seconds": 100, "duration_seconds": 30, "warmup_seconds": 1, "table_settle_seconds": 1, "seed_items_per_table": 25}"#,
        );
        // usable 90 s; first overhead 1 + 30 + 2 + 20 = 53; measure 60.
        assert_eq!(compute_split(&two, 2).expect("split").0, 2);
        assert!(compute_split(&j(r#"{"warmup_seconds": null}"#), 2).is_err());
        assert!(compute_split(&j(r#"{"warmup_seconds": "x"}"#), 2).is_err());
    }

    #[test]
    fn seed_seconds_scale_with_the_pattern_count() {
        assert_eq!(seed_seconds(&j("{}"), 2).expect("seed"), 31);
        assert_eq!(seed_seconds(&j("{}"), 0).expect("seed"), 30);
        assert_eq!(
            seed_seconds(&j(r#"{"seed_items_per_table": 2000}"#), 10).expect("seed"),
            54
        );
    }

    #[test]
    fn bench_rate_is_clamped_between_the_floor_and_the_ceiling() {
        let ap = j(r#"{"peak_rps": 100}"#);
        assert_eq!(bench_rps(&ap, &j("{}")).expect("rps"), 1.0);
        assert_eq!(
            bench_rps(&ap, &j(r#"{"scale_factor": 0.2}"#)).expect("rps"),
            20.0
        );
        assert_eq!(
            bench_rps(&ap, &j(r#"{"scale_factor": 2, "max_rps_per_pattern": 50}"#)).expect("rps"),
            50.0
        );
        assert_eq!(
            bench_rps(
                &j(r#"{"peak_rps": 1}"#),
                &j(r#"{"min_rps_per_pattern": 5}"#)
            )
            .expect("rps"),
            5.0
        );
        assert!(bench_rps(&j(r#"{"peak_rps": [1]}"#), &j("{}")).is_err());
    }

    #[test]
    fn validation_refuses_what_would_fail_every_call() {
        assert!(validate(&j(MODEL)).is_ok());
        assert!(refusal(r#"{"access_patterns": []}"#).starts_with("access_patterns empty"));
        assert!(
            refusal(r#"{"access_patterns": [{"pattern_id": "X", "peak_rps": 0}]}"#)
                .contains("has missing or zero peak_rps")
        );
        assert!(
            refusal(r#"{"access_patterns": [{"pattern_id": "X", "peak_rps": 1}]}"#)
                .contains("has no \"table\"")
        );
        let missing = refusal(
            r#"{"tables": [{"table_name": "A"}], "access_patterns": [{"pattern_id": "X", "peak_rps": 1, "table": "B"}]}"#,
        );
        assert!(missing.contains("not defined in tables[]"), "{missing}");
        assert!(missing.contains("Defined tables: [\"A\"]"), "{missing}");
        let vector_on_gsi = |op: &str, index: &str| {
            refusal(&format!(
                r#"{{"tables": [{{"table_name": "T", "gsis": [{{"index_name": "G"}}], "vector_indexes": [{{"index_name": "V"}}]}}],
                   "access_patterns": [{{"pattern_id": "X", "peak_rps": 1, "table": "T", "operation": "{op}", "index": "{index}"}}]}}"#
            ))
        };
        assert!(vector_on_gsi("SearchVectors", "G").contains("defines no such vector index"));
        assert!(vector_on_gsi("Query", "V").contains("is a VECTOR index"));
        assert!(vector_on_gsi("Query", "Z").contains("defines no such GSI"));
        let no_index = refusal(
            r#"{"tables": [{"table_name": "T"}], "access_patterns": [{"pattern_id": "X", "peak_rps": 1, "table": "T", "operation": "SearchVectors"}]}"#,
        );
        assert!(no_index.contains("names no \"index\""), "{no_index}");
    }

    #[test]
    fn validation_messages_escape_and_redact_caller_text() {
        let message = refusal(
            "{\"access_patterns\": [{\"pattern_id\": \"AKIAIOSFODNN7EXAMPLE\\nforged\", \"peak_rps\": 0}]}",
        );
        assert!(!message.contains("AKIAIOSFODNN7EXAMPLE"), "{message}");
        assert!(!message.contains('\n'), "{message}");
    }

    #[test]
    fn vector_metadata_is_resolved_onto_search_patterns() {
        let model = j(r#"{
          "tables": [{"table_name": "T",
            "entities": [{"attributes": [{"name": "pk", "type": "N"}]}],
            "attribute_definitions": [{"attribute_name": "pk", "attribute_type": "B"}],
            "vector_indexes": [{"index_name": "V", "vector_attribute": "vec", "dimensions": 4,
                                "search_schema": {"partition_key": "pk", "inline_filters": ["f"]}}]}],
          "access_patterns": [
            {"pattern_id": "Get", "operation": "GetItem", "table": "T", "peak_rps": 1},
            {"pattern_id": "S", "operation": "SearchVectors", "table": "T", "index": "V", "peak_rps": 1}]}"#);
        let patterns = attach_vector_index_meta(&model).expect("meta");
        assert!(patterns[0].get("vector_index").is_none());
        let vi = patterns[1].get("vector_index").expect("vector_index");
        assert_eq!(vi.get("pk_type"), Some(&Json::string("B")));
        assert_eq!(vi.get("dimensions"), Some(&Json::Int(4)));
        assert_eq!(vi.get("partition_key"), Some(&Json::string("pk")));
        assert_eq!(
            vi.get("inline_filters").map(Json::to_compact_string),
            Some(r#"["f"]"#.to_string())
        );
    }

    #[test]
    fn spend_estimate_prices_the_window_and_the_seed() {
        let model = j(PLAIN);
        let spend = estimate_bench_spend(
            &model,
            &j(r#"{"warmup_seconds": 3, "duration_seconds": 15, "seed_items_per_table": 100}"#),
        )
        .expect("spend");
        assert_eq!(spend.window_seconds, 18.0);
        // Get: 18 calls x 0.5 RRU; Put: 18 calls x 1 WRU. Seed: 2 patterns x 100 x 1 KB.
        let driven = 18.0 * 0.5 * 0.125e-6 + 18.0 * 1.0 * 0.625e-6;
        assert!(
            (spend.driven_usd - driven).abs() < 1e-12,
            "{}",
            spend.driven_usd
        );
        let seed = 2.0 * 100.0 * 0.625e-6;
        assert!(spend.seed_usd >= seed, "{}", spend.seed_usd);
        assert!((spend.total_usd - spend.driven_usd - spend.seed_usd).abs() < 1e-15);
        assert_eq!(spend.per_pattern.len(), 2);
    }

    #[test]
    fn spend_gate_counts_vector_bytes_for_search_and_seed() {
        let model = j(r#"{
          "tables": [{"table_name": "T",
            "entities": [{"entity_name": "E", "estimated_item_size_bytes": 100, "attributes": []}],
            "vector_indexes": [{"index_name": "V", "vector_attribute": "vec", "dimensions": 1024,
                                "projection": {"type": "KEYS_ONLY"}}]}],
          "access_patterns": [
            {"pattern_id": "S", "operation": "SearchVectors", "table": "T", "index": "V", "peak_rps": 100, "top_k": 10},
            {"pattern_id": "P", "operation": "PutItem", "table": "T", "peak_rps": 10, "attributes_written": ["vec"]}]}"#);
        let spend = estimate_bench_spend(&model, &j("{}")).expect("spend");
        assert!(spend.vector_usd > 0.0);
        let search = &spend.per_pattern[0];
        assert!(search.vector_cost > 0.0, "search must not estimate at $0");
        assert!(spend.per_pattern[1].vector_cost > 0.0);
    }

    #[test]
    fn mode_presets_fill_only_what_the_config_leaves_out() {
        let mut sink: Vec<u8> = Vec::new();
        let mut out = Output::new(&mut sink);
        let quick = apply_mode_preset(&j(r#"{"mode": "quick", "duration_seconds": 40}"#), &mut out)
            .expect("quick");
        assert_eq!(quick.get("duration_seconds"), Some(&Json::Int(40)));
        assert_eq!(quick.get("warmup_seconds"), Some(&Json::Int(3)));
        let representative = apply_mode_preset(&j(r#"{"mode": "representative"}"#), &mut out)
            .expect("representative");
        assert_eq!(representative.get("scale_factor"), Some(&Json::Float(0.15)));
        assert_eq!(
            representative.get("read_pattern_key_sampling"),
            Some(&Json::string("zipf"))
        );
        let standard = j(r#"{"mode": "standard", "x": 1}"#);
        assert_eq!(
            apply_mode_preset(&standard, &mut out).expect("standard"),
            standard
        );
        let none = j(r#"{"mode": null}"#);
        assert_eq!(apply_mode_preset(&none, &mut out).expect("null"), none);
        for bad in [
            r#"{"mode": "quik"}"#,
            r#"{"mode": 5}"#,
            r#"{"mode": ["quick"]}"#,
            "[]",
        ] {
            assert!(apply_mode_preset(&j(bad), &mut out).is_err(), "{bad}");
        }
        drop(out);
        let text = String::from_utf8(sink).expect("utf-8");
        assert!(text.contains("Quick mode: running a short per-pattern window (settle=10s, warmup=3s, duration=40s, seed=100 items)."));
        assert!(text.contains("Representative mode: scale ~0.15× declared peak"));
    }

    #[test]
    fn guardrail_texts_use_python_precision() {
        let spend = Spend {
            total_usd: 1.23456,
            driven_usd: 1.0,
            seed_usd: 0.23456,
            vector_usd: 0.0,
            per_pattern: Vec::new(),
            window_seconds: 1.0,
        };
        assert_eq!(
            spend_line(&spend, 0.5),
            "Estimated AWS spend for this run: ~$1.235 (driven load ~$1.000 + seeding ~$0.235). Guardrail: $0.50."
        );
        assert!(guardrail_refusal(&spend, 0.5)
            .starts_with("estimated spend $1.235 exceeds the cost guardrail $0.50."));
        assert_eq!(minutes(96), "2");
        assert_eq!(
            wall_clock(&j("{}"), 2, 1).expect("wall").total,
            30 + 31 + 2 * 100 + 20
        );
    }
}
