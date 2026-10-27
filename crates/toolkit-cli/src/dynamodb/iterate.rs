//! Port of `amazon-dynamodb/scripts/iterate_design.py` (slice `dynamo-cost`,
//! Issue #4389): ONE round of the human-driven DynamoDB design loop, then
//! STOP.
//!
//! Round: optionally apply a user-agreed change to the design JSON, decide
//! reuse-vs-redeploy from a schema fingerprint, (gated) deploy or reuse,
//! benchmark, generate the report and findings, refresh the calculator cost
//! report, append one entry to `loop_state.json` and print a compact summary
//! plus a STOP marker. The command never starts a second round, never chooses
//! a change of its own and never tears anything down.
//!
//! The Python script drove deploy, benchmark and report generation by running
//! sibling Python scripts as subprocesses. Here every pipeline step is a
//! `toolkit dynamodb <step>` command dispatched through the shared command
//! table ([`RoundEnv::run_step`]); the cost calculator is this slice's own
//! `dynamodb calculate-costs`. The deploy, benchmark and report commands
//! belong to the `dynamo-deploy` (#4391) and `dynamo-bench` (#4390) slices
//! and are not part of this build, so a round that needs them stops with the
//! `dynamodb_step_unavailable` error before any AWS action — the safety gates
//! (`--yes-deploy`, `--skip-deploy`, `--allow-spend`) are evaluated first and
//! forwarded unchanged to those commands once they are registered.

use std::collections::BTreeSet;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use toolkit_core::{sha256_hex, Json, ToolkitError};

use super::cost::{self, Res};
use super::{load_json, pyfmt, required_option, write_text};
use crate::{route, Invocation, Output, Routed, COMMANDS};

/// Artifact names written beside the design JSON.
pub const PERF_RAW: &str = "perf_raw.jsonl";
pub const PERF_SUMMARY: &str = "perf_summary.json";
pub const PERF_REPORT: &str = "performance_report.md";
pub const DESIGN_FINDINGS: &str = "design_findings.json";
pub const COST_REPORT: &str = "cost_report.md";

const OPS_HELP: &str = concat!(
    "In the --apply-change file, each entry in \"ops\" needs ",
    "{\"op\": \"add\"|\"replace\"|\"remove\", \"path\": \"/json/pointer\", \"value\": ...} ",
    "(value omitted for remove). Example: ",
    "{\"op\": \"replace\", \"path\": \"/tables/0/gsis/1/projection/type\", \"value\": \"INCLUDE\"}."
);

/// An `_die`: message plus the published exit status (default 2).
fn die(message: impl Into<String>, status: u8) -> ToolkitError {
    ToolkitError::runtime("dynamodb_iterate_failed", message).with_exit_code(status)
}

/// A failure of a user-supplied change or file that is not a `_die` (the
/// Python source raised and exited 1).
fn failure(message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime("dynamodb_iterate_failed", message)
}

// ---------------------------------------------------------------------------
// Environment (clock, ids, pipeline steps).
// ---------------------------------------------------------------------------

/// Everything a round needs from the outside world, injectable for tests.
pub trait RoundEnv {
    /// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
    fn now_iso(&mut self) -> String;
    /// A fresh 8-hex-digit loop id.
    fn new_loop_id(&mut self) -> String;
    /// Runs one pipeline step: `argv` is the `toolkit` command line without
    /// the program name (for example `["dynamodb", "calculate-costs", ...]`).
    /// The step writes its stdout lines to `out`.
    fn run_step(&mut self, argv: &[String], out: &mut Output<'_>) -> Result<(), ToolkitError>;
}

/// The real environment: system clock and the shared command table.
pub struct SystemEnv;

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub fn utc_iso(epoch_seconds: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let rem = epoch_seconds.rem_euclid(86_400);
    // Civil-from-days (proleptic Gregorian), after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub(crate) fn dispatch_step(argv: &[String], out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let registered = COMMANDS.iter().any(|spec| {
        argv.len() >= spec.path.len()
            && spec
                .path
                .iter()
                .zip(argv)
                .all(|(expected, actual)| *expected == actual.as_str())
    });
    if !registered {
        return Err(ToolkitError::runtime(
            "dynamodb_step_unavailable",
            format!(
                "pipeline step `toolkit {}` is not available in this build (its Rust port belongs to another migration slice: deploy #4391, benchmark and perf report #4390); nothing was deployed or benchmarked",
                argv.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
            ),
        ));
    }
    match route(COMMANDS, argv)? {
        Routed::Run(spec, invocation) => (spec.handler)(&invocation, out),
        Routed::Help | Routed::Version => Err(failure("pipeline step resolved to help")),
    }
}

impl RoundEnv for SystemEnv {
    fn now_iso(&mut self) -> String {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
            .unwrap_or(0);
        utc_iso(seconds)
    }

    fn new_loop_id(&mut self) -> String {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        format!("{:08x}", hasher.finish() as u32)
    }

    fn run_step(&mut self, argv: &[String], out: &mut Output<'_>) -> Result<(), ToolkitError> {
        dispatch_step(argv, out)
    }
}

// ---------------------------------------------------------------------------
// Small JSON helpers.
// ---------------------------------------------------------------------------
fn set_key(pairs: &mut Vec<(String, Json)>, key: &str, value: Json) {
    if let Some(slot) = pairs.iter_mut().find(|(name, _)| name == key) {
        slot.1 = value;
    } else {
        pairs.push((key.to_string(), value));
    }
}

fn object_pairs(value: &Json) -> &[(String, Json)] {
    match value {
        Json::Object(pairs) => pairs.as_slice(),
        _ => &[],
    }
}

fn numeric(value: &Json) -> Option<f64> {
    match value {
        Json::Int(number) => Some(*number as f64),
        Json::Float(number) => Some(*number),
        Json::Bool(flag) => Some(f64::from(u8::from(*flag))),
        _ => None,
    }
}

fn require_number(value: &Json, what: &str) -> Res<f64> {
    numeric(value).ok_or_else(|| {
        failure(format!(
            "{what} must be a number, got {}",
            pyfmt::repr(value)
        ))
    })
}

/// Python `round(value, digits)`: an int stays an int, a float is rounded.
fn round_json(value: &Json, digits: usize, what: &str) -> Res<Json> {
    match value {
        Json::Int(_) => Ok(value.clone()),
        Json::Bool(flag) => Ok(Json::Int(i64::from(*flag))),
        Json::Float(number) => Ok(Json::Float(pyfmt::round(*number, digits))),
        other => Err(failure(format!(
            "{what} must be a number, got {}",
            pyfmt::repr(other)
        ))),
    }
}

fn subtract(left: &Json, right: &Json, what: &str) -> Res<Json> {
    if let (Json::Int(a), Json::Int(b)) = (left, right) {
        if let Some(difference) = a.checked_sub(*b) {
            return Ok(Json::Int(difference));
        }
    }
    Ok(Json::Float(
        require_number(left, what)? - require_number(right, what)?,
    ))
}

fn object_or_empty(value: Option<&Json>) -> Json {
    match value {
        Some(found) if pyfmt::truthy(found) => found.clone(),
        _ => Json::Object(Vec::new()),
    }
}

fn load_or(path: &Path, default: Option<Json>) -> Res<Json> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match default {
            Some(value) => Ok(value),
            None => Err(die(
                format!("file not found: {}", pyfmt::normalized_path(path)),
                2,
            )),
        },
        _ => load_json(path),
    }
}

// ---------------------------------------------------------------------------
// Mode presets (the values `benchmark_model.py` resolves; mirrored here so the
// config recorded in loop_state matches what the benchmark will compute).
// ---------------------------------------------------------------------------
fn preset_pairs(mode: &str) -> Vec<(&'static str, Json)> {
    match mode {
        "quick" => vec![
            ("table_settle_seconds", Json::Int(10)),
            ("warmup_seconds", Json::Int(3)),
            ("duration_seconds", Json::Int(15)),
            ("ramp_seconds", Json::Int(3)),
            ("seed_items_per_table", Json::Int(100)),
        ],
        "representative" => vec![
            ("table_settle_seconds", Json::Int(45)),
            ("warmup_seconds", Json::Int(15)),
            ("duration_seconds", Json::Int(120)),
            ("ramp_seconds", Json::Int(20)),
            ("seed_items_per_table", Json::Int(2000)),
            ("items_per_partition", Json::Int(40)),
            ("scale_factor", Json::Float(0.15)),
            ("max_rps_per_pattern", Json::Int(1800)),
            ("min_rps_per_pattern", Json::Int(5)),
            ("concurrency_per_pattern", Json::Int(32)),
            ("read_pattern_key_sampling", Json::string("zipf")),
            ("zipf_s", Json::Float(1.1)),
        ],
        _ => Vec::new(),
    }
}

/// `_resolve_config`: fill the mode preset into the config; explicit values
/// always win.
pub fn resolve_config(cfg: &Json) -> Json {
    let mode = match cfg.get("mode") {
        None | Some(Json::Null) => return cfg.clone(),
        Some(Json::String(mode)) if mode == "standard" => return cfg.clone(),
        Some(Json::String(mode)) => mode.as_str(),
        Some(_) => "",
    };
    let mut merged = object_pairs(cfg).to_vec();
    for (key, value) in preset_pairs(mode) {
        if cfg.get(key).is_none() {
            merged.push((key.to_string(), value));
        }
    }
    Json::Object(merged)
}

// ---------------------------------------------------------------------------
// Apply a user-agreed change.
// ---------------------------------------------------------------------------

/// `_deep_merge`: JSON merge-patch where a `null` removes the key.
fn deep_merge(base: Json, patch: &Json) -> Json {
    match (base, patch) {
        (Json::Object(mut pairs), Json::Object(patch_pairs)) => {
            for (key, value) in patch_pairs {
                if matches!(value, Json::Null) {
                    pairs.retain(|(name, _)| name != key);
                    continue;
                }
                let existing = pairs
                    .iter()
                    .position(|(name, _)| name == key)
                    .map(|index| std::mem::replace(&mut pairs[index].1, Json::Null));
                let merged = deep_merge(existing.unwrap_or(Json::Null), value);
                set_key(&mut pairs, key, merged);
            }
            Json::Object(pairs)
        }
        (_, patch) => patch.clone(),
    }
}

fn pointer_tokens(path: &str) -> Vec<&str> {
    path.split('/').filter(|token| !token.is_empty()).collect()
}

/// Python list indexing: negative indices count from the end.
fn list_index(len: usize, index: i64) -> Option<usize> {
    let length = i64::try_from(len).ok()?;
    let resolved = if index < 0 { index + length } else { index };
    if (0..length).contains(&resolved) {
        usize::try_from(resolved).ok()
    } else {
        None
    }
}

fn parse_index(token: &str, path: &str) -> Res<i64> {
    token.trim().parse::<i64>().map_err(|_| {
        failure(format!(
            "--apply-change: {token:?} in path {path:?} is not a list index"
        ))
    })
}

fn navigate<'a>(root: &'a mut Json, tokens: &[&str], path: &str) -> Res<&'a mut Json> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Json::Array(items) => {
                let index = parse_index(token, path)?;
                let Some(position) = list_index(items.len(), index) else {
                    return Err(failure(format!(
                        "--apply-change: index {token} is out of range in path {path:?}"
                    )));
                };
                &mut items[position]
            }
            Json::Object(pairs) => {
                match pairs.iter_mut().find(|(name, _)| name.as_str() == *token) {
                    Some((_, value)) => value,
                    None => {
                        return Err(failure(format!(
                            "--apply-change: key {token:?} not found in path {path:?}"
                        )))
                    }
                }
            }
            _ => {
                return Err(failure(format!(
                    "--apply-change: cannot descend into a scalar at {token:?} in path {path:?}"
                )))
            }
        };
    }
    Ok(current)
}

fn repr_opt(value: Option<&Json>) -> String {
    value.map_or_else(|| "None".to_string(), pyfmt::repr)
}

fn unsupported_op(op: &Json, path: &Json) -> ToolkitError {
    die(
        format!(
            "--apply-change: unsupported op {} at path {}. {OPS_HELP}",
            repr_opt(op.get("op")),
            pyfmt::repr(path)
        ),
        2,
    )
}

fn op_value(op: &Json) -> Res<Json> {
    op.get("value")
        .cloned()
        .ok_or_else(|| failure("--apply-change: the op has no \"value\""))
}

/// `_apply_op`: one RFC 6902-lite operation (`add`, `replace`, `remove`).
fn apply_op(model: &mut Json, op: &Json) -> Res<()> {
    if !matches!(op, Json::Object(_)) {
        return Err(failure(format!(
            "--apply-change: each op must be an object, got {}",
            pyfmt::repr(op)
        )));
    }
    let path_value = op.get("path").cloned().unwrap_or_else(|| Json::string(""));
    let Json::String(path) = &path_value else {
        return Err(failure(format!(
            "--apply-change: op path must be a string, got {}",
            pyfmt::repr(&path_value)
        )));
    };
    let kind = op.get("op").and_then(Json::as_str);
    let tokens = pointer_tokens(path);
    let Some((last, parents)) = tokens.split_last() else {
        return Err(die(
            format!(
                "--apply-change op is missing a non-empty \"path\": {}. {OPS_HELP}",
                pyfmt::repr(op)
            ),
            2,
        ));
    };
    let parent = navigate(model, parents, path)?;
    match parent {
        Json::Array(items) => {
            if *last == "-" {
                if kind != Some("add") {
                    return Err(die(
                        format!(
                            "--apply-change: a trailing '-' in path {} appends to a list and is only valid for op \"add\", not {}. {OPS_HELP}",
                            pyfmt::repr(&path_value),
                            repr_opt(op.get("op"))
                        ),
                        2,
                    ));
                }
                items.push(op_value(op)?);
                return Ok(());
            }
            let index = parse_index(last, path)?;
            let length = items.len();
            match kind {
                Some("remove") => {
                    let Some(position) = list_index(length, index) else {
                        return Err(failure(format!(
                            "--apply-change: cannot remove index {last}: out of range in path {path:?}"
                        )));
                    };
                    let _ = items.remove(position);
                }
                Some("add") => {
                    let value = op_value(op)?;
                    // list.insert clamps the position instead of failing.
                    let resolved = if index < 0 {
                        (index + i64::try_from(length).unwrap_or(i64::MAX)).max(0)
                    } else {
                        index
                    };
                    let position = usize::try_from(resolved).unwrap_or(length).min(length);
                    items.insert(position, value);
                }
                Some("replace") => {
                    let value = op_value(op)?;
                    let Some(position) = list_index(length, index) else {
                        return Err(failure(format!(
                            "--apply-change: cannot replace index {last}: out of range in path {path:?}"
                        )));
                    };
                    items[position] = value;
                }
                _ => return Err(unsupported_op(op, &path_value)),
            }
            Ok(())
        }
        Json::Object(pairs) => match kind {
            Some("remove") => {
                pairs.retain(|(name, _)| name.as_str() != *last);
                Ok(())
            }
            Some("add" | "replace") => {
                let value = op_value(op)?;
                set_key(pairs, last, value);
                Ok(())
            }
            _ => Err(unsupported_op(op, &path_value)),
        },
        _ => Err(failure(format!(
            "--apply-change: cannot modify a scalar parent in path {path:?}"
        ))),
    }
}

/// `_apply_change`: a `{"merge": {...}}` merge-patch or an `{"ops": [...]}`
/// op list. Applies only what it is handed.
pub fn apply_change(model: Json, change: &Json) -> Res<Json> {
    if !matches!(change, Json::Object(_)) {
        return Err(failure(
            "the --apply-change file must contain a JSON object with a \"merge\" object or an \"ops\" list",
        ));
    }
    if let Some(patch) = change.get("merge") {
        return Ok(deep_merge(model, patch));
    }
    if let Some(ops) = change.get("ops") {
        let Json::Array(ops) = ops else {
            return Err(failure(format!(
                "--apply-change: \"ops\" must be a list, got {}",
                pyfmt::repr(ops)
            )));
        };
        let mut model = model;
        for op in ops {
            apply_op(&mut model, op)?;
        }
        return Ok(model);
    }
    let keys: Vec<&str> = object_pairs(change)
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    Err(die(
        format!(
            "--apply-change file must contain a top-level \"merge\" object (partial design to deep-merge) or an \"ops\" list (JSON-patch-lite). Got keys: {}. {OPS_HELP}",
            if keys.is_empty() {
                "<none>".to_string()
            } else {
                keys.join(", ")
            }
        ),
        2,
    ))
}

// ---------------------------------------------------------------------------
// Schema fingerprint.
// ---------------------------------------------------------------------------
fn sort_key(value: &Json, field: &str) -> Res<String> {
    match value.get(field) {
        None => Ok(String::new()),
        Some(Json::String(text)) => Ok(text.clone()),
        Some(other) => Err(failure(format!(
            "`{field}` must be a string to fingerprint the schema, got {}",
            pyfmt::repr(other)
        ))),
    }
}

fn clone_or_null(value: Option<&Json>) -> Json {
    value.cloned().unwrap_or(Json::Null)
}

fn array_items<'a>(value: Option<&'a Json>, what: &str) -> Res<&'a [Json]> {
    match value {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Array(items)) => Ok(items.as_slice()),
        Some(other) => Err(failure(format!(
            "`{what}` must be a list, got {}",
            pyfmt::repr(other)
        ))),
    }
}

/// `_schema_fingerprint`: a short stable hash of everything that requires a
/// physical redeploy when it changes (key schema, GSIs, stream config).
/// Request volume, item size and consistency changes do NOT affect it.
pub fn schema_fingerprint(model: &Json) -> Res<String> {
    let mut tables: Vec<(String, &Json)> = Vec::new();
    for table in array_items(model.get("tables"), "tables")? {
        tables.push((sort_key(table, "table_name")?, table));
    }
    tables.sort_by(|left, right| left.0.cmp(&right.0));

    let mut signature = Vec::new();
    for (_, table) in tables {
        let key_schema = object_or_empty(table.get("key_schema"));
        let mut gsis: Vec<(String, &Json)> = Vec::new();
        for gsi in array_items(table.get("gsis"), "gsis")? {
            gsis.push((sort_key(gsi, "index_name")?, gsi));
        }
        gsis.sort_by(|left, right| left.0.cmp(&right.0));

        let mut gsi_signature = Vec::new();
        for (_, gsi) in gsis {
            let projection = object_or_empty(gsi.get("projection"));
            let kind = match projection.get("type").filter(|value| pyfmt::truthy(value)) {
                Some(Json::String(text)) => text.to_uppercase(),
                Some(other) => {
                    return Err(failure(format!(
                        "GSI projection `type` must be a string, got {}",
                        pyfmt::repr(other)
                    )))
                }
                None => "ALL".to_string(),
            };
            let attributes = ["attributes", "non_key_attributes", "NonKeyAttributes"]
                .iter()
                .filter_map(|field| projection.get(field))
                .find(|value| pyfmt::truthy(value));
            let mut names: Vec<String> = Vec::new();
            if let Some(attributes) = attributes {
                for item in array_items(Some(attributes), "projection attributes")? {
                    let Json::String(name) = item else {
                        return Err(failure(format!(
                            "projection attributes must be strings, got {}",
                            pyfmt::repr(item)
                        )));
                    };
                    names.push(name.clone());
                }
            }
            names.sort();
            gsi_signature.push(Json::Array(vec![
                clone_or_null(gsi.get("index_name")),
                clone_or_null(gsi.get("partition_key")),
                clone_or_null(gsi.get("sort_key")),
                Json::String(kind),
                Json::Array(names.into_iter().map(Json::String).collect()),
            ]));
        }
        let streams = object_or_empty(table.get("streams"));
        signature.push(Json::Array(vec![
            clone_or_null(table.get("table_name")),
            clone_or_null(key_schema.get("partition_key")),
            clone_or_null(key_schema.get("sort_key")),
            Json::Array(gsi_signature),
            Json::Bool(streams.get("enabled").is_some_and(pyfmt::truthy)),
            clone_or_null(streams.get("view_type")),
        ]));
    }
    let blob = pyfmt::dumps(&Json::Array(signature), true);
    Ok(sha256_hex(blob.as_bytes())[..12].to_string())
}

// ---------------------------------------------------------------------------
// Headline metrics and deltas.
// ---------------------------------------------------------------------------

/// The calculator's monthly total (no AWS): the sum of every pattern's
/// monthly cost, or `None` when the design cannot be priced.
fn calculator_monthly(model: &Json) -> Option<f64> {
    let tables = match model.get("tables") {
        Some(Json::Array(items)) => items.as_slice(),
        _ => &[],
    };
    // A table without `table_name` made the Python table map raise; the
    // headline then recorded no calculator figure.
    if tables.iter().any(|table| table.get("table_name").is_none()) {
        return None;
    }
    let sizes = cost::build_entity_attr_sizes(tables).ok()?;
    let mut total = 0.0;
    let patterns = match model.get("access_patterns") {
        Some(Json::Array(items)) => items.as_slice(),
        _ => &[],
    };
    for ap in patterns {
        let wanted = ap.get("table").and_then(Json::as_str).unwrap_or("");
        let table_def = tables
            .iter()
            .rev()
            .find(|table| table.get("table_name").and_then(Json::as_str) == Some(wanted));
        total += cost::pattern_monthly_cost(ap, table_def, Some(&sizes))
            .ok()?
            .total_cost;
    }
    Some(total)
}

/// `_headline_from_artifacts`: the small numeric snapshot recorded per round,
/// from the compact artifacts only (never the large raw file).
fn headline_from_artifacts(model: &Json, workdir: &Path, have_bench: bool) -> Res<Json> {
    let mut extrapolated = Json::Null;
    let calculator = match calculator_monthly(model) {
        Some(total) => Json::Float(pyfmt::round(total, 2)),
        None => Json::Null,
    };
    let mut throttles: Vec<(String, Json)> = Vec::new();
    let mut p99: Vec<(String, Json)> = Vec::new();
    let mut key_share: Vec<(String, Json)> = Vec::new();
    let mut max_amp = Json::Null;

    if have_bench {
        let summary = load_or(&workdir.join(PERF_SUMMARY), Some(Json::Object(Vec::new())))?;
        let findings = load_or(
            &workdir.join(DESIGN_FINDINGS),
            Some(Json::Object(Vec::new())),
        )?;

        // Total extrapolated monthly: monthly / share of any cost driver.
        for driver in array_items(findings.get("top_cost_drivers"), "top_cost_drivers")? {
            if let Some(share) = driver.get("share").filter(|value| pyfmt::truthy(value)) {
                let monthly = driver
                    .get("monthly")
                    .ok_or_else(|| failure("a top_cost_drivers entry is missing `monthly`"))?;
                let ratio = require_number(monthly, "monthly")? / require_number(share, "share")?;
                extrapolated = Json::Float(pyfmt::round(ratio, 2));
                break;
            }
        }

        for pattern in array_items(summary.get("patterns"), "patterns")? {
            let pid = pattern
                .get("pattern_id")
                .map(pyfmt::text)
                .ok_or_else(|| failure("a perf summary pattern is missing `pattern_id`"))?;
            let steady = object_or_empty(pattern.get("steady_state"));
            if let Some(value) = steady.get("throttles").filter(|v| pyfmt::truthy(v)) {
                set_key(&mut throttles, &pid, value.clone());
            }
            if let Some(value) = steady.get("p99_ms").filter(|v| !matches!(v, Json::Null)) {
                set_key(&mut p99, &pid, round_json(value, 1, "p99_ms")?);
            }
            if let Some(amp) = steady
                .get("amplification_ratio")
                .filter(|v| !matches!(v, Json::Null))
            {
                let replace = match numeric(&max_amp) {
                    None => true,
                    Some(current) => require_number(amp, "amplification_ratio")? > current,
                };
                if replace {
                    max_amp = amp.clone();
                }
            }
            let distribution = object_or_empty(steady.get("key_distribution"));
            if let Some(share) = distribution
                .get("top_key_share")
                .filter(|v| !matches!(v, Json::Null))
            {
                set_key(&mut key_share, &pid, round_json(share, 3, "top_key_share")?);
            }
        }
        if !matches!(max_amp, Json::Null) {
            max_amp = round_json(&max_amp, 3, "amplification_ratio")?;
        }
    }

    Ok(Json::object(vec![
        ("extrapolated_monthly_usd", extrapolated),
        ("calculator_monthly_usd", calculator),
        ("hot_pattern_throttles", Json::Object(throttles)),
        ("p99_ms_by_pattern", Json::Object(p99)),
        ("max_gsi_amplification", max_amp),
        ("top_key_share_by_pattern", Json::Object(key_share)),
    ]))
}

/// `_compute_deltas`. Key order inside the per-pattern maps is sorted (the
/// Python source iterated hash-randomized sets).
pub fn compute_deltas(cur: &Json, prev: Option<&Json>) -> Res<Json> {
    let Some(prev) = prev.filter(|value| pyfmt::truthy(value)) else {
        return Ok(Json::object(vec![
            ("monthly_usd_pct", Json::Null),
            ("throttle_delta", Json::Object(Vec::new())),
            ("p99_delta_ms", Json::Object(Vec::new())),
            ("gsi_amp_delta", Json::Null),
        ]));
    };

    let previous_monthly = prev.get("extrapolated_monthly_usd");
    let current_monthly = cur.get("extrapolated_monthly_usd");
    let monthly_pct = match (previous_monthly, current_monthly) {
        (Some(before), Some(now)) if pyfmt::truthy(before) && !matches!(now, Json::Null) => {
            let before = require_number(before, "extrapolated_monthly_usd")?;
            let now = require_number(now, "extrapolated_monthly_usd")?;
            Json::Float(pyfmt::round((now - before) / before * 100.0, 1))
        }
        _ => Json::Null,
    };

    let current_throttles = object_or_empty(cur.get("hot_pattern_throttles"));
    let previous_throttles = object_or_empty(prev.get("hot_pattern_throttles"));
    let throttle_keys: BTreeSet<&str> = object_pairs(&current_throttles)
        .iter()
        .chain(object_pairs(&previous_throttles))
        .map(|(key, _)| key.as_str())
        .collect();
    let mut throttle_delta = Vec::new();
    for key in throttle_keys {
        let zero = Json::Int(0);
        let now = current_throttles.get(key).unwrap_or(&zero);
        let before = previous_throttles.get(key).unwrap_or(&zero);
        throttle_delta.push((key.to_string(), subtract(now, before, "throttles")?));
    }

    let current_p99 = object_or_empty(cur.get("p99_ms_by_pattern"));
    let previous_p99 = object_or_empty(prev.get("p99_ms_by_pattern"));
    let shared: BTreeSet<&str> = object_pairs(&current_p99)
        .iter()
        .filter(|(key, _)| previous_p99.get(key).is_some())
        .map(|(key, _)| key.as_str())
        .collect();
    let mut p99_delta = Vec::new();
    for key in shared {
        let (Some(now), Some(before)) = (current_p99.get(key), previous_p99.get(key)) else {
            continue;
        };
        let difference = subtract(now, before, "p99_ms")?;
        p99_delta.push((key.to_string(), round_json(&difference, 1, "p99_ms")?));
    }

    let gsi_amp_delta = match (
        prev.get("max_gsi_amplification")
            .filter(|v| !matches!(v, Json::Null)),
        cur.get("max_gsi_amplification")
            .filter(|v| !matches!(v, Json::Null)),
    ) {
        (Some(before), Some(now)) => {
            let difference = subtract(now, before, "max_gsi_amplification")?;
            round_json(&difference, 3, "max_gsi_amplification")?
        }
        _ => Json::Null,
    };

    Ok(Json::object(vec![
        ("monthly_usd_pct", monthly_pct),
        ("throttle_delta", Json::Object(throttle_delta)),
        ("p99_delta_ms", Json::Object(p99_delta)),
        ("gsi_amp_delta", gsi_amp_delta),
    ]))
}

// ---------------------------------------------------------------------------
// The round.
// ---------------------------------------------------------------------------

/// Inputs of `dynamodb iterate-design` (the Python argparse namespace).
#[derive(Debug, Clone)]
pub struct IterateArgs {
    pub model: PathBuf,
    pub config: PathBuf,
    pub loop_state: PathBuf,
    pub manifest: PathBuf,
    pub apply_change: Option<PathBuf>,
    pub mode: String,
    pub timestamp: Option<String>,
    pub yes_deploy: bool,
    pub allow_spend: bool,
    pub skip_deploy: bool,
    pub calculator_only: bool,
    pub dry_run: bool,
}

struct Round {
    model: Json,
    workdir: PathBuf,
    resolved_cfg: Json,
    overlay: PathBuf,
    loop_state: Json,
    manifest: Json,
    round_idx: usize,
    prev_round: Option<Json>,
    timestamp: String,
    fingerprint: String,
    prior_fingerprint: Option<Json>,
    have_manifest: bool,
    decision: String,
    applied_diff: Option<Json>,
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

/// `_run`: announce a step, run it, and turn a failure into the step's own
/// exit status.
fn run_step(env: &mut dyn RoundEnv, out: &mut Output<'_>, step: &str, argv: &[String]) -> Res<()> {
    out.line(&format!(
        "\n[iterate_design] {step}: toolkit {}",
        argv.join(" ")
    ))?;
    match env.run_step(argv, out) {
        Ok(()) => Ok(()),
        Err(error) => {
            out.diagnostic(&format!("error: {}", error.message()));
            let code = error.exit_code();
            Err(die(
                format!("step '{step}' failed (exit {code}). See output above."),
                code,
            ))
        }
    }
}

fn calculate_argv(model_path: &Path, output: &Path) -> Vec<String> {
    let mut argv = words(&["dynamodb", "calculate-costs", "--model"]);
    argv.push(path_text(model_path));
    argv.push("--output".to_string());
    argv.push(path_text(output));
    argv
}

fn rounds_of(loop_state: &Json) -> &[Json] {
    match loop_state.get("rounds") {
        Some(Json::Array(items)) => items.as_slice(),
        _ => &[],
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_round(
    round: &mut Round,
    ls_path: &Path,
    args: &IterateArgs,
    decision: &str,
    headline: &Json,
    signals: &[String],
) -> Res<()> {
    let prev_headline = round
        .prev_round
        .as_ref()
        .and_then(|previous| previous.get("headline"));
    let entry = Json::object(vec![
        (
            "round",
            Json::Int(i64::try_from(round.round_idx).unwrap_or(i64::MAX)),
        ),
        ("timestamp", Json::string(round.timestamp.as_str())),
        ("mode", Json::string(args.mode.as_str())),
        (
            "scale_factor",
            clone_or_null(round.resolved_cfg.get("scale_factor")),
        ),
        (
            "applied_diff",
            round.applied_diff.clone().unwrap_or(Json::Null),
        ),
        (
            "schema_fingerprint",
            Json::string(round.fingerprint.as_str()),
        ),
        ("deploy_decision", Json::string(decision)),
        ("headline", headline.clone()),
        ("delta_vs_prev", compute_deltas(headline, prev_headline)?),
        (
            "finding_signals",
            Json::Array(signals.iter().map(Json::string).collect()),
        ),
        ("user_decision", Json::Null),
    ]);
    let Json::Object(state) = &mut round.loop_state else {
        return Err(failure("the loop state must be a JSON object"));
    };
    match state.iter_mut().find(|(name, _)| name == "rounds") {
        Some((_, Json::Array(rounds))) => rounds.push(entry),
        _ => return Err(failure("the loop state has no `rounds` list")),
    }
    set_key(
        state,
        "current_schema_fingerprint",
        Json::string(round.fingerprint.as_str()),
    );
    if let Some(prefix) = round.manifest.get("prefix").filter(|v| pyfmt::truthy(v)) {
        set_key(state, "active_manifest", prefix.clone());
    }
    write_text(ls_path, &round.loop_state.to_pretty_string())
}

/// `_emit_summary`.
fn emit_summary(
    out: &mut Output<'_>,
    round: &Round,
    decision: &str,
    headline: &Json,
    signals: &[String],
    calc_only: bool,
) -> Res<()> {
    out.line(&format!("\n{}", "=".repeat(72)))?;
    out.line(&format!("ROUND {} SUMMARY", round.round_idx))?;
    out.line(&"=".repeat(72))?;
    out.line(&format!("  decision: {decision}"))?;
    if let Some(value) = headline
        .get("calculator_monthly_usd")
        .filter(|v| !matches!(v, Json::Null))
    {
        out.line(&format!(
            "  calculator monthly: ${}",
            pyfmt::commas(require_number(value, "calculator_monthly_usd")?, 2)
        ))?;
    }
    if let Some(value) = headline
        .get("extrapolated_monthly_usd")
        .filter(|v| !matches!(v, Json::Null))
    {
        out.line(&format!(
            "  measured-extrapolated monthly: ${}",
            pyfmt::commas(require_number(value, "extrapolated_monthly_usd")?, 2)
        ))?;
    }
    if !calc_only {
        match headline
            .get("hot_pattern_throttles")
            .filter(|v| pyfmt::truthy(v))
        {
            Some(throttles) => {
                out.line(&format!(
                    "  hot-pattern throttles: {}",
                    pyfmt::repr(throttles)
                ))?;
            }
            None => out.line("  hot-pattern throttles: none observed")?,
        }
        if let Some(amp) = headline
            .get("max_gsi_amplification")
            .filter(|v| !matches!(v, Json::Null))
        {
            out.line(&format!("  max GSI amplification: {}×", pyfmt::text(amp)))?;
        }
    }
    if let Some(previous) = &round.prev_round {
        let delta = compute_deltas(headline, previous.get("headline"))?;
        let previous_round = previous
            .get("round")
            .map(pyfmt::text)
            .ok_or_else(|| failure("the previous round entry has no `round`"))?;
        out.line(&format!(
            "  delta vs round {previous_round}: {}",
            pyfmt::dumps(&delta, false)
        ))?;
    }
    let shown = if signals.is_empty() {
        "none (clean)".to_string()
    } else {
        pyfmt::repr_strings(signals)
    };
    out.line(&format!("  design-finding signals: {shown}"))?;
    out.line(&format!(
        "  artifacts: {DESIGN_FINDINGS}, {COST_REPORT}{}",
        if calc_only {
            String::new()
        } else {
            format!(", {PERF_REPORT}")
        }
    ))?;
    out.line(&"=".repeat(72))?;
    out.line(&format!(
        "=== ROUND {} COMPLETE — handing back to user. The loop does not self-iterate. Review the findings, decide a change, and re-run for the next round. ===",
        round.round_idx
    ))
}

/// `_run_round_body`: deploy-or-reuse, benchmark, report, cost, record.
fn run_round_body(
    round: &mut Round,
    args: &IterateArgs,
    env: &mut dyn RoundEnv,
    out: &mut Output<'_>,
) -> Res<()> {
    // --- 4. Deploy (gated) or reuse ---
    if round.decision == "deploy" {
        if !args.yes_deploy {
            return Err(die(
                "this round requires a real AWS deploy (schema changed or no active deployment), but --yes-deploy was not passed. Re-run with --yes-deploy after confirming the target is a sandbox account. (Existing deployments from a prior round are NOT torn down automatically — run the prior teardown.sh first if the schema changed.)",
                4,
            ));
        }
        let schema_moved = round.prior_fingerprint.as_ref().is_some_and(|prior| {
            !matches!(prior, Json::Null) && *prior != Json::string(round.fingerprint.as_str())
        });
        if schema_moved && round.have_manifest {
            out.line("[iterate_design] NOTE: schema changed since the last deployment. The prior resources are NOT torn down automatically — run the prior teardown.sh to avoid orphans.")?;
        }
        let mut deploy_cfg_path = round.overlay.clone();
        if args.dry_run {
            // dry_run is read from the config by the deploy step; layer it on
            // the resolved overlay so the user's file is never mutated.
            let mut pairs = object_pairs(&round.resolved_cfg).to_vec();
            set_key(&mut pairs, "dry_run", Json::Bool(true));
            deploy_cfg_path = round.workdir.join(".iterate_dry_config.json");
            write_text(&deploy_cfg_path, &Json::Object(pairs).to_pretty_string())?;
        }
        let mut deploy = words(&["dynamodb", "deploy-model", "--model"]);
        deploy.push(path_text(&args.model));
        deploy.push("--config".to_string());
        deploy.push(path_text(&deploy_cfg_path));
        deploy.push("--manifest-out".to_string());
        deploy.push(path_text(&args.manifest));
        deploy.push("--yes-deploy".to_string());
        run_step(env, out, "deploy_model", &deploy)?;
        if args.dry_run {
            // Clean up the temporary dry-run config overlay.
            let _ = fs::remove_file(&deploy_cfg_path);
            out.line("[iterate_design] dry-run: deploy rehearsed, no resources created; skipping benchmark.")?;
            let headline = headline_from_artifacts(&round.model, &round.workdir, false)?;
            let decision = format!("{}(dry-run)", round.decision);
            finish_round(round, &args.loop_state, args, &decision, &headline, &[])?;
            return emit_summary(out, round, &decision, &headline, &[], false);
        }
        round.manifest = load_or(&args.manifest, None)?;
    } else {
        out.line(&format!(
            "[iterate_design] reusing existing deployment (prefix={}).",
            round
                .manifest
                .get("prefix")
                .map_or_else(|| "None".to_string(), pyfmt::text)
        ))?;
    }

    // --- 5. Benchmark (the cost guardrail lives inside the benchmark step) ---
    let mut bench = words(&["dynamodb", "benchmark-model", "--model"]);
    bench.push(path_text(&args.model));
    bench.push("--config".to_string());
    bench.push(path_text(&round.overlay));
    bench.push("--manifest".to_string());
    bench.push(path_text(&args.manifest));
    bench.push("--raw-out".to_string());
    bench.push(path_text(&round.workdir.join(PERF_RAW)));
    bench.push("--summary-out".to_string());
    bench.push(path_text(&round.workdir.join(PERF_SUMMARY)));
    if args.allow_spend {
        bench.push("--allow-spend".to_string());
    }
    run_step(env, out, "benchmark_model", &bench)?;

    // --- 6. Report + 7. cost report ---
    let mut report = words(&["dynamodb", "generate-perf-report", "--model"]);
    report.push(path_text(&args.model));
    report.push("--summary".to_string());
    report.push(path_text(&round.workdir.join(PERF_SUMMARY)));
    report.push("--output".to_string());
    report.push(path_text(&round.workdir.join(PERF_REPORT)));
    report.push("--findings-out".to_string());
    report.push(path_text(&round.workdir.join(DESIGN_FINDINGS)));
    run_step(env, out, "generate_perf_report", &report)?;
    run_step(
        env,
        out,
        "calculate_costs",
        &calculate_argv(&args.model, &round.workdir.join(COST_REPORT)),
    )?;

    let findings = load_or(
        &round.workdir.join(DESIGN_FINDINGS),
        Some(Json::Object(Vec::new())),
    )?;
    let mut signals: BTreeSet<String> = BTreeSet::new();
    for finding in array_items(findings.get("classified_findings"), "classified_findings")? {
        match finding.get("signal") {
            Some(Json::String(signal)) => {
                signals.insert(signal.clone());
            }
            _ => return Err(failure("a classified finding has no string `signal`")),
        }
    }
    let signals: Vec<String> = signals.into_iter().collect();
    let headline = headline_from_artifacts(&round.model, &round.workdir, true)?;

    // --- 8. Update loop_state + 9. emit summary + STOP ---
    let decision = round.decision.clone();
    finish_round(
        round,
        &args.loop_state,
        args,
        &decision,
        &headline,
        &signals,
    )?;
    emit_summary(out, round, &decision, &headline, &signals, false)
}

/// The `main()` of `iterate_design.py`: run exactly one round, then stop.
pub fn run_iterate_round(
    args: &IterateArgs,
    env: &mut dyn RoundEnv,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let model_path = args.model.as_path();
    let mut model = load_or(model_path, None)?;
    if !matches!(model, Json::Object(_)) {
        return Err(failure("the data model must be a JSON object"));
    }
    let workdir = fs::canonicalize(model_path)
        .ok()
        .and_then(|resolved| resolved.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut cfg = load_or(&args.config, None)?;
    let Json::Object(cfg_pairs) = &mut cfg else {
        return Err(failure("the benchmark config must be a JSON object"));
    };
    if !cfg_pairs.iter().any(|(name, _)| name == "mode") {
        cfg_pairs.push(("mode".to_string(), Json::string(args.mode.as_str())));
    }
    // Resolve the mode preset NOW and persist it to an overlay the steps read
    // from disk: passing the user's original config would silently drop
    // `--mode`, because the steps re-read the file.
    let resolved_cfg = resolve_config(&cfg);
    let overlay = workdir.join(".iterate_config.json");

    let timestamp = args.timestamp.clone().unwrap_or_else(|| env.now_iso());
    let default_state = Json::object(vec![
        ("loop_id", Json::String(env.new_loop_id())),
        (
            "model_path",
            Json::string(pyfmt::normalized_path(model_path)),
        ),
        ("created_at", Json::string(timestamp.as_str())),
        ("current_schema_fingerprint", Json::Null),
        ("active_manifest", Json::Null),
        ("rounds", Json::Array(Vec::new())),
    ]);
    let loop_state = load_or(&args.loop_state, Some(default_state))?;
    let manifest = load_or(&args.manifest, Some(Json::Object(Vec::new())))?;

    let round_idx = rounds_of(&loop_state).len();
    let prev_round = rounds_of(&loop_state).last().cloned();

    // --- 1. Apply the user-agreed change (if any) ---
    let mut applied_diff = None;
    if let Some(change_path) = &args.apply_change {
        let change = load_or(change_path, None)?;
        model = apply_change(model, &change)?;
        write_text(model_path, &model.to_pretty_string())?;
        let name = model_path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        applied_diff = Some(change);
        out.line(&format!("[iterate_design] applied change to {name}"))?;
    }

    // --- 2. Schema fingerprint + reuse-vs-redeploy decision ---
    let fingerprint = schema_fingerprint(&model)?;
    let prior_fingerprint = loop_state
        .get("current_schema_fingerprint")
        .filter(|value| !matches!(value, Json::Null))
        .cloned();
    let have_manifest = manifest.get("lambda").is_some_and(pyfmt::truthy);
    let current = Json::string(fingerprint.as_str());
    let schema_changed = prior_fingerprint
        .as_ref()
        .is_some_and(|prior| *prior != current);
    let prior_matches = prior_fingerprint.as_ref() == Some(&current);

    let decision = if args.calculator_only {
        "calculator-only"
    } else if schema_changed {
        "deploy"
    } else if have_manifest && prior_matches {
        "reuse"
    } else if have_manifest && prior_fingerprint.is_none() {
        // First round against a manifest the loop did not deploy: trust it;
        // the fingerprint computed now is stored at finish so the NEXT round
        // detects any schema change normally.
        "reuse"
    } else {
        "deploy"
    };

    let prior_text = prior_fingerprint
        .as_ref()
        .map_or_else(|| "None".to_string(), pyfmt::text);
    if decision == "deploy" && args.skip_deploy {
        return Err(die(
            format!(
                "--skip-deploy was passed but this round needs a redeploy (schema fingerprint changed: {prior_text} -> {fingerprint}). Re-run without --skip-deploy and with --yes-deploy to redeploy."
            ),
            2,
        ));
    }

    out.line(&format!(
        "[iterate_design] round {round_idx}: schema_fingerprint={fingerprint} (prior={prior_text}) -> decision={decision}"
    ))?;

    let mut round = Round {
        model,
        workdir,
        resolved_cfg,
        overlay,
        loop_state,
        manifest,
        round_idx,
        prev_round,
        timestamp,
        fingerprint,
        prior_fingerprint,
        have_manifest,
        decision: decision.to_string(),
        applied_diff,
    };

    // --- 3. Calculator-only fast path: re-cost, record, STOP ---
    // (No AWS, no benchmark, so no config overlay is needed.)
    if round.decision == "calculator-only" {
        let cost_out = round.workdir.join(COST_REPORT);
        run_step(
            env,
            out,
            "calculate_costs (calculator-only)",
            &calculate_argv(model_path, &cost_out),
        )?;
        let headline = headline_from_artifacts(&round.model, &round.workdir, false)?;
        let decision = round.decision.clone();
        finish_round(
            &mut round,
            &args.loop_state,
            args,
            &decision,
            &headline,
            &[],
        )?;
        return emit_summary(out, &round, &decision, &headline, &[], true);
    }

    write_text(&round.overlay, &round.resolved_cfg.to_pretty_string())?;
    let result = run_round_body(&mut round, args, env, out);
    // Clean up both transient overlays, even if a step failed mid-round.
    for transient in [
        round.overlay.clone(),
        round.workdir.join(".iterate_dry_config.json"),
    ] {
        if fs::symlink_metadata(&transient).is_ok() {
            let _ = fs::remove_file(&transient);
        }
    }
    result
}

/// Handler of `toolkit dynamodb iterate-design`.
pub fn iterate_design_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let args = IterateArgs {
        model: PathBuf::from(required_option(invocation, "--model")?),
        config: PathBuf::from(required_option(invocation, "--config")?),
        loop_state: PathBuf::from(required_option(invocation, "--loop-state")?),
        manifest: PathBuf::from(
            invocation
                .value("--manifest")
                .unwrap_or("created_resources.json"),
        ),
        apply_change: invocation.value("--apply-change").map(PathBuf::from),
        mode: invocation
            .value("--mode")
            .unwrap_or("representative")
            .to_string(),
        timestamp: invocation.value("--timestamp").map(ToString::to_string),
        yes_deploy: invocation.has_flag("--yes-deploy"),
        allow_spend: invocation.has_flag("--allow-spend"),
        skip_deploy: invocation.has_flag("--skip-deploy"),
        calculator_only: invocation.has_flag("--calculator-only"),
        dry_run: invocation.has_flag("--dry-run"),
    };
    run_iterate_round(&args, &mut SystemEnv, out)
}
