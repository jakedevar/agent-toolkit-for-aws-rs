//! Attributes of a migration telemetry event, derived from the run's own
//! artifacts (`metric_emission/migration_attributes.py`).
//!
//! Every value is a lookup over files the migration skills already write into
//! `.migration/<id>/`: no inference, no free text. Unknown or unmodelled values
//! drop the attribute, never the event, because the service rejects a whole
//! event on one bad enum member. Phase facts attach only to the
//! `PHASE_COMPLETED` event of the phase that produced them; `complexityTier` is
//! the exception, riding every Design-onward and terminal event.
//!
//! The Python code works on loosely typed `dict`s and relies on `or {}`, `.get`
//! and `str()` behaving the same for every JSON shape; here those semantics are
//! spelled out ([`or_obj`], [`py_str`], [`Abort`]). An artifact of a shape that
//! made the Python raise makes [`derive_attributes`] return [`Abort`], which the
//! caller turns into "no attributes", never a lost event.

use std::path::Path;
use std::sync::OnceLock;

use fancy_regex::Regex;
use toolkit_core::{read_regular_file, Json, MAX_TEXT_FILE_BYTES};

use crate::manifests::{is_falsy, py_repr, py_strip};
use crate::startup::artifacts::pyjson;

/// The Python code would have raised on this input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Abort;

pub(super) type R<T> = Result<T, Abort>;

/// One migration event found by the diff, before it becomes the
/// `migrationActivity` union member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Event {
    pub name: &'static str,
    pub phase: Option<String>,
    pub status: Option<&'static str>,
    /// The state-file key of a `PHASE_COMPLETED` event (original case).
    pub key: Option<String>,
    pub failure_reason: Option<&'static str>,
    pub run_mode: Option<&'static str>,
}

impl Event {
    pub(super) fn named(name: &'static str) -> Self {
        Self {
            name,
            phase: None,
            status: None,
            key: None,
            failure_reason: None,
            run_mode: None,
        }
    }
}

pub(super) const PHASES: [&str; 7] = [
    "DISCOVER", "CLARIFY", "DESIGN", "ESTIMATE", "WORKSHOP", "GENERATE", "FEEDBACK",
];
pub(super) const RESOLVED_STATUS: [(&str, &str); 4] = [
    ("completed", "SUCCESS"),
    ("skipped", "SKIPPED"),
    ("not_applicable", "NOT_APPLICABLE"),
    ("failed", "FAILED"),
];
pub(super) const RUN_MODE: [(&str, &str); 2] = [
    ("decide", "DECIDE"),
    ("decide_and_execute", "DECIDE_AND_EXECUTE"),
];
/// Mirrors the `reason=` constants of the interpreter's `GATE_FAIL` line.
pub(super) const FAILURE_REASON: [(&str, &str); 3] = [
    ("missing", "MISSING"),
    ("invalid", "INVALID"),
    ("stale_downstream", "STALE_DOWNSTREAM"),
];

const PRICING_SOURCE: [(&str, &str); 5] = [
    ("live", "LIVE"),
    ("cached", "CACHED"),
    ("cached_fallback", "CACHED_FALLBACK"),
    ("cached_stale", "CACHED_STALE"),
    ("unavailable", "UNAVAILABLE"),
];
const RECOMMENDATION_OUTCOME: [(&str, &str); 5] = [
    ("go", "GO"),
    ("conditional_go", "CONDITIONAL_GO"),
    ("defer", "DEFER"),
    ("defer_for_evidence", "DEFER"),
    ("stay", "STAY"),
];
const CLARIFY_MODE: [(&str, &str); 3] = [
    ("wizard", "WIZARD"),
    ("full", "FULL"),
    ("fast_path", "FAST"),
];
/// `estimated_from_token_volume` is the AI route (it prices tokens, not
/// infrastructure). `preferences` is handled by [`preference_spend_basis`],
/// since a clarify answer the skill filled in itself is not the customer's
/// figure.
const SPEND_BASIS: [(&str, &str); 11] = [
    ("billing_data", "BILLING_DATA"),
    ("inventory_estimate", "INVENTORY_ESTIMATE"),
    ("live_prices_plus_cache", "LIVE_PRICES_PLUS_CACHE"),
    ("pricing_cache", "PRICING_CACHE"),
    ("user_provided", "USER_PROVIDED"),
    ("unavailable", "UNAVAILABLE"),
    ("estimated_from_token_volume", "TOKEN_VOLUME_ESTIMATE"),
    // azure-to-aws's baseline rungs: a stated figure, a Cost Management export
    // or RDfA consumption data, or a figure derived from SKUs.
    ("user_stated", "USER_PROVIDED"),
    ("cost_management_export", "BILLING_DATA"),
    ("consumption_data", "BILLING_DATA"),
    ("derived_from_skus", "INVENTORY_ESTIMATE"),
];
const AI_SOURCE: [(&str, &str); 5] = [
    ("openai", "OPENAI"),
    ("anthropic", "ANTHROPIC"),
    ("gemini", "GCP"),
    ("azure_openai", "AZURE"),
    ("other", "OTHER"),
];
const COMPLEXITY_TIER: [&str; 3] = ["SMALL", "MEDIUM", "LARGE"];
/// The discover preview's coarser signal, used before a tiered artifact exists.
const COMPLEXITY_SIGNAL: [(&str, &str); 3] = [
    ("likely_simple", "SMALL"),
    ("standard", "MEDIUM"),
    ("complex", "LARGE"),
];
const RECOMMENDATION_CONFIDENCE: [&str; 3] = ["LOW", "MEDIUM", "HIGH"];
const VALIDATION_STATUS: [&str; 3] = ["PASSED", "PASSED_DEGRADED_OFFLINE", "FAILED"];
const COMPLIANCE_ACCEPTED: [&str; 11] = [
    "NONE", "UNKNOWN", "HIPAA", "PCI", "SOC2", "GDPR", "CCPA", "FEDRAMP", "PCI_DSS", "SOC_2",
    "FED_RAMP",
];
const COMPLIANCE_ALIAS: [(&str, &str); 3] = [
    ("PCI_DSS", "PCI"),
    ("SOC_2", "SOC2"),
    ("FED_RAMP", "FEDRAMP"),
];
const AVAILABILITY: [&str; 4] = ["SINGLE_AZ", "MULTI_AZ", "MULTI_AZ_HA", "MULTI_REGION"];
const CUTOVER_STRATEGY: [&str; 4] = [
    "MAINTENANCE_WINDOW_WEEKLY",
    "MAINTENANCE_WINDOW_MONTHLY",
    "FLEXIBLE",
    "ZERO_DOWNTIME",
];
/// gcp's clarify writes `write-heavy-global` for the write-heavy answer.
const DATABASE_TRAFFIC_ACCEPTED: [&str; 4] =
    ["STEADY", "READ_HEAVY", "WRITE_HEAVY", "WRITE_HEAVY_GLOBAL"];
const COMPUTE_POSTURE: [&str; 6] = [
    "EKS_MANAGED",
    "EKS_OR_ECS",
    "ECS_FARGATE",
    "EKS",
    "ECS",
    "ELASTIC_BEANSTALK",
];
const TARGET_REGION: [&str; 17] = [
    "US_EAST_1",
    "US_EAST_2",
    "US_WEST_1",
    "US_WEST_2",
    "CA_CENTRAL_1",
    "EU_WEST_1",
    "EU_WEST_2",
    "EU_WEST_3",
    "EU_CENTRAL_1",
    "EU_NORTH_1",
    "AP_SOUTH_1",
    "AP_SOUTHEAST_1",
    "AP_SOUTHEAST_2",
    "AP_NORTHEAST_1",
    "AP_NORTHEAST_2",
    "AP_NORTHEAST_3",
    "SA_EAST_1",
];
/// The skills write `db_size` as a human range (`<10GB`, `10-100GB`).
const DB_SIZE: [(&str, &str); 5] = [
    ("<10gb", "DB_UNDER_10GB"),
    ("10-100gb", "DB_10_100GB"),
    ("100-500gb", "DB_100_500GB"),
    (">500gb", "DB_OVER_500GB"),
    ("unknown", "UNKNOWN"),
];
const PROJECTED_COST_MAX: f64 = 10_000_000.0;
const RESOURCE_COUNT_MAX: usize = 10_000;

const AI_TYPE_WORDS: [&str; 14] = [
    "vertex",
    "aiplatform",
    "notebooks",
    "discovery_engine",
    "automl",
    "ml_engine",
    "dialogflow",
    "document_ai",
    "cognitive",
    "machinelearning",
    "machine_learning",
    "bedrock",
    "sagemaker",
    "comprehend",
];
const DB_TYPE_WORDS: [&str; 17] = [
    "sql",
    "postgres",
    "mysql",
    "mongo",
    "redis",
    "firestore",
    "spanner",
    "bigtable",
    "datastore",
    "memorystore",
    "alloydb",
    "cosmos",
    "rds",
    "aurora",
    "dynamo",
    "elasticache",
    "documentdb",
];

/// Where a route records the source-platform spend, in preference order.
const SOURCE_SPEND_KEYS: [&str; 18] = [
    "gcp_monthly_spend",
    "gcp_monthly",
    "gcp_monthly_usd",
    "gcp_monthly_baseline",
    "current_gcp_monthly",
    "total_monthly_spend",
    "gcp_total_monthly",
    "total_monthly",
    "gcp_monthly_ai_spend",
    "total_current_ai_monthly",
    "heroku_monthly",
    "heroku_monthly_estimated",
    "heroku_monthly_baseline",
    "current_heroku_monthly",
    "azure_monthly",
    "azure_monthly_spend",
    "azure_monthly_baseline",
    "current_azure_monthly",
];

/// The inventory file a migration skill writes and the provider it implies.
#[derive(Clone, Copy)]
struct InventorySpec {
    inventory: &'static str,
    provider: &'static str,
}

fn skill_inventory(skill: &str) -> Option<InventorySpec> {
    let (inventory, provider) = match skill {
        "AZURE_TO_AWS" => ("azure-resource-inventory.json", "AZURE"),
        "GCP_TO_AWS" => ("gcp-resource-inventory.json", "GCP"),
        "HEROKU_TO_AWS" => ("heroku-resource-inventory.json", "HEROKU"),
        _ => return None,
    };
    Some(InventorySpec {
        inventory,
        provider,
    })
}

const ESTIMATE_FILES: [&str; 3] = [
    "estimation-infra.json",
    "estimation-ai.json",
    "estimation-billing.json",
];
const DESIGN_ONWARD: [&str; 5] = ["DESIGN", "ESTIMATE", "WORKSHOP", "GENERATE", "FEEDBACK"];

// ------------------------------------------------------------ Python helpers

static EMPTY: Json = Json::Object(Vec::new());

/// `value or {}` followed by `.get(...)`: absent, null and every falsy value
/// read as an empty object, an object as itself, any other truthy value
/// raises.
pub(super) fn or_obj(value: Option<&Json>) -> R<&Json> {
    match value {
        None => Ok(&EMPTY),
        Some(v) if is_falsy(v) => Ok(&EMPTY),
        Some(v @ Json::Object(_)) => Ok(v),
        Some(_) => Err(Abort),
    }
}

/// `dict.get(key)` with a JSON null reading as `None`.
pub(super) fn get<'a>(object: &'a Json, key: &str) -> Option<&'a Json> {
    object.get(key).filter(|value| !matches!(value, Json::Null))
}

/// Python `str(value)`: a string as itself, anything else as its `repr`.
pub(super) fn py_str(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// `read_json(path)`: the parsed file, or `None` when absent, unreadable
/// (symbolic links and oversized files included) or unparseable. A file that
/// holds `null` is `None` too, as in Python.
pub(super) fn read_json(path: &Path) -> Option<Json> {
    let text = read_regular_file(path, MAX_TEXT_FILE_BYTES).ok()?;
    pyjson::loads(&text)
        .ok()
        .filter(|value| !matches!(value, Json::Null))
}

/// Look `value` up case-insensitively; only the table's own keys count.
pub(super) fn map_enum(
    table: &[(&str, &'static str)],
    value: Option<&Json>,
) -> Option<&'static str> {
    let key = py_str(value.filter(|v| !matches!(v, Json::Null))?).to_lowercase();
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, mapped)| *mapped)
}

/// Normalise a skill-written value (`multi-az-ha`, `us-east-1`) to
/// `UPPER_SNAKE` and admit it only if the model declares it.
pub(super) fn to_enum(members: &[&str], value: Option<&Json>) -> Option<String> {
    let text = py_str(value.filter(|v| !matches!(v, Json::Null))?);
    let upper = py_strip(&text).to_uppercase();
    let mut key = String::new();
    let mut in_gap = false;
    for c in upper.chars() {
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            key.push(c);
            in_gap = false;
        } else if !in_gap {
            key.push('_');
            in_gap = true;
        }
    }
    let key = key.trim_matches('_');
    members.contains(&key).then(|| key.to_string())
}

/// Python `float(text)`: surrounding whitespace, single underscores between
/// digits, `inf` and `nan` are accepted.
pub(super) fn py_float(text: &str) -> Option<f64> {
    let text = py_strip(text);
    let bytes = text.as_bytes();
    let mut cleaned = String::with_capacity(text.len());
    for (index, c) in text.char_indices() {
        if c == '_' {
            let digit_before = index > 0 && bytes[index - 1].is_ascii_digit();
            let digit_after = bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
            if !(digit_before && digit_after) {
                return None;
            }
        } else {
            cleaned.push(c);
        }
    }
    cleaned.parse::<f64>().ok()
}

fn number_of(value: &Json) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    match value {
        Json::Int(number) => Some(*number as f64),
        Json::Float(number) => Some(*number),
        _ => None,
    }
}

/// `x > 0` for a value read with default `0`: raises for anything that is not
/// a number or boolean.
fn gt_zero(value: Option<&Json>) -> R<bool> {
    match value {
        None => Ok(false),
        Some(Json::Bool(flag)) => Ok(*flag),
        Some(other) => number_of(other).map(|n| n > 0.0).ok_or(Abort),
    }
}

// ---------------------------------------------------------------- conversions

pub(super) fn to_pricing_source(raw: Option<&Json>) -> R<Option<&'static str>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let object = matches!(raw, Json::Object(_));
    let mapped = map_enum(
        &PRICING_SOURCE,
        if object {
            get(raw, "status")
        } else {
            Some(raw)
        },
    );
    let Some(mapped) = mapped else {
        return Ok(None);
    };
    let stale = object
        && get(or_obj(get(raw, "fallback_staleness"))?, "is_stale") == Some(&Json::Bool(true));
    Ok(Some(if mapped == "CACHED" && stale {
        "CACHED_STALE"
    } else {
        mapped
    }))
}

/// A number from a JSON number or a numeric string; `None` for anything else.
fn numeric(value: Option<&Json>) -> Option<f64> {
    match value? {
        Json::String(text) => py_float(text),
        other => number_of(other),
    }
}

pub(super) fn to_spend_band(amount: Option<&Json>) -> Option<&'static str> {
    let amount = numeric(amount)?;
    if amount.is_nan() || amount < 0.0 {
        return None;
    }
    Some(if amount < 100.0 {
        "UNDER_100"
    } else if amount < 1000.0 {
        "FROM_100_TO_1K"
    } else if amount < 10000.0 {
        "FROM_1K_TO_10K"
    } else {
        "OVER_10K"
    })
}

/// Integer USD within the model's range; anything else is omitted rather than
/// clamped, since a clamped cost would read as a real figure.
pub(super) fn to_projected_usd(value: Option<&Json>) -> Option<i64> {
    let amount = numeric(value)?;
    if amount.is_nan() || !(0.0..=PROJECTED_COST_MAX).contains(&amount) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(amount.round_ties_even() as i64)
}

fn accuracy_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"±?\s*([0-9]+)(?:\s*-\s*([0-9]+))?\s*%").expect("static pattern")
    })
}

/// `"±5-10%"`, `"±20-30%+"`, `"±5% (billing)"`: the upper bound decides the band.
pub(super) fn to_accuracy_band(text: Option<&Json>) -> Option<&'static str> {
    let Some(Json::String(text)) = text else {
        return None;
    };
    let captures = accuracy_pattern().captures(text).ok().flatten()?;
    let digits = captures.get(2).or_else(|| captures.get(1))?.as_str();
    // Longer than u64 is far past every threshold.
    let upper = digits.parse::<u64>().unwrap_or(u64::MAX);
    Some(if upper <= 10 {
        "HIGH"
    } else if upper <= 20 {
        "MEDIUM"
    } else {
        "LOW"
    })
}

/// `"100%"`, `"97%"`, `100`: `FULL` at 100, `NEAR_COMPLETE` from 90, `PARTIAL` below.
pub(super) fn to_coverage(value: Option<&Json>) -> Option<&'static str> {
    let value = value.filter(|v| !matches!(v, Json::Null))?;
    if value == &Json::String(String::new()) {
        return None;
    }
    let number = match number_of(value) {
        Some(number) => number,
        None => py_float(&py_str(value).replace('%', ""))?,
    };
    Some(if number >= 100.0 {
        "FULL"
    } else if number >= 90.0 {
        "NEAR_COMPLETE"
    } else {
        "PARTIAL"
    })
}

/// A design constraint is an object with the interpreted value under `value`
/// (gcp) or `default` (heroku's `compute_target`); older artifacts hold the
/// bare value. Heroku records region, compliance and availability as plain
/// values under `global` instead.
fn constraint_value<'a>(preferences: &'a Json, key: &str) -> R<Option<&'a Json>> {
    if !matches!(preferences, Json::Object(_)) {
        return Ok(None);
    }
    let mut raw = get(or_obj(preferences.get("design_constraints"))?, key);
    if raw.is_none() {
        raw = get(or_obj(preferences.get("global"))?, key);
    }
    Ok(match raw {
        Some(object @ Json::Object(_)) => object.get("value").or_else(|| object.get("default")),
        other => other,
    })
}

/// An explicit empty list is the customer's confirmed "no requirements", which
/// the model spells NONE; only an absent answer is omitted.
pub(super) fn to_compliance_list(raw: Option<&Json>) -> Option<Vec<String>> {
    let raw = raw.filter(|v| !matches!(v, Json::Null))?;
    if matches!(raw, Json::Array(items) if items.is_empty()) {
        return Some(vec!["NONE".to_string()]);
    }
    let values: Vec<&Json> = match raw {
        Json::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if let Some(key) = to_enum(&COMPLIANCE_ACCEPTED, Some(value)) {
            let member = COMPLIANCE_ALIAS
                .iter()
                .find(|(alias, _)| *alias == key)
                .map_or(key, |(_, member)| (*member).to_string());
            if !out.contains(&member) {
                out.push(member);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// An estimate that cites `preferences` for the source spend is only as good
/// as the clarify answer behind it: the customer's own answer is `USER_PROVIDED`,
/// one the skill filled in itself is `DEFAULTED`, one extracted from the
/// customer's billing export is `BILLING_DATA`, anything without provenance is
/// left out.
fn preference_spend_basis(run_dir: &Path) -> R<Option<&'static str>> {
    let preferences = read_json(&run_dir.join("preferences.json"));
    let preferences = or_obj(preferences.as_ref())?;
    let spend = get(
        or_obj(preferences.get("design_constraints"))?,
        "gcp_monthly_spend",
    );
    let Some(spend @ Json::Object(_)) = spend.filter(|v| !is_falsy(v)) else {
        return Ok(None);
    };
    let source = get(spend, "source")
        .filter(|v| !is_falsy(v))
        .map_or_else(String::new, py_str);
    let chosen_by = get(spend, "chosen_by");
    let is = |wanted: &str| chosen_by == Some(&Json::string(wanted));
    Ok(if is("default") || source.starts_with("default") {
        Some("DEFAULTED")
    } else if is("user") {
        Some("USER_PROVIDED")
    } else if is("extracted") && source.starts_with("billing") {
        Some("BILLING_DATA")
    } else {
        None
    })
}

/// The resource types as one lower-cased string. gcp writes `type`, heroku
/// `resource_type` (with the add-on service under `config`), azure the
/// canonical ARM type under `azure_type`.
fn resource_types(resources: &[Json]) -> String {
    let mut parts = Vec::new();
    for resource in resources {
        if !matches!(resource, Json::Object(_)) {
            continue;
        }
        let kind = ["type", "resource_type", "azure_type"]
            .iter()
            .filter_map(|key| get(resource, key))
            .find(|value| !is_falsy(value))
            .map_or_else(String::new, py_str);
        let addon = match resource.get("config") {
            Some(config @ Json::Object(_)) => get(config, "addon_service")
                .filter(|value| !is_falsy(value))
                .map_or_else(String::new, py_str),
            _ => String::new(),
        };
        parts.push(format!("{kind} {addon}"));
    }
    parts.join(" ").to_lowercase()
}

fn types_match_ai(types: &str) -> bool {
    AI_TYPE_WORDS.iter().any(|word| types.contains(word))
}

fn types_match_db(types: &str) -> bool {
    DB_TYPE_WORDS.iter().any(|word| types.contains(word))
}

/// The migration's source: the skill's inventory file names it; an AI-only run
/// has none and takes the provider from its workload profile.
fn source_provider(run_dir: &Path, spec: Option<InventorySpec>) -> R<Option<&'static str>> {
    if let Some(spec) = spec {
        if run_dir.join(spec.inventory).exists() {
            return Ok(Some(spec.provider));
        }
    }
    let profile = read_json(&run_dir.join("ai-workload-profile.json"));
    let summary = or_obj(or_obj(profile.as_ref())?.get("summary"))?;
    Ok(map_enum(&AI_SOURCE, summary.get("ai_source")))
}

/// Where a route records the source-platform cost, in preference order: the
/// infra and AI routes use `current_costs`, the billing-only route
/// `gcp_baseline`, and heroku's assembler also rolls it into
/// `financial_summary`.
fn cost_containers(estimate: &Json) -> Vec<&Json> {
    [
        "current_costs",
        "gcp_baseline",
        "cost_comparison",
        "financial_summary",
    ]
    .iter()
    .filter_map(|key| estimate.get(key))
    .filter(|value| matches!(value, Json::Object(_)))
    .collect()
}

/// The three cost tiers are written flat (`aws_monthly_*`), with older shapes
/// nesting them under `option_*`/`tiers`.
fn projected_cost(estimate: &Json, tier: &str, option: &str) -> R<Option<i64>> {
    let costs = or_obj(estimate.get("projected_costs"))?;
    let mut value = get(costs, &format!("aws_monthly_{tier}"));
    if value.is_none() {
        value = match costs.get(option) {
            Some(nested @ Json::Object(_)) => get(nested, "aws_monthly"),
            _ => None,
        };
    }
    if value.is_none() {
        let tiers = or_obj(costs.get("tiers"))?;
        value = match tiers.get(tier) {
            Some(nested @ Json::Object(_)) => get(nested, "monthly"),
            _ => None,
        };
    }
    Ok(to_projected_usd(value))
}

/// Written by generate (gcp) or estimate (heroku); before either is complete,
/// the discover preview's coarser signal. A confirmed re-entry resets
/// downstream phases to pending but leaves their artifacts on disk, so an
/// artifact counts only while its phase is completed.
fn complexity_tier(run_dir: &Path, status: &Json) -> R<Option<String>> {
    let phases = or_obj(status.get("phases"))?;
    let completed = |name: &str| phases.get(name) == Some(&Json::string("completed"));
    let mut tiered: Vec<&str> = Vec::new();
    if completed("generate") {
        tiered.extend([
            "generation-infra.json",
            "generation-billing.json",
            "generation-ai.json",
        ]);
    }
    if completed("estimate") {
        tiered.push("estimation-infra.json");
    }
    for name in tiered {
        let artifact = read_json(&run_dir.join(name));
        let artifact = or_obj(artifact.as_ref())?;
        let mut raw = get(artifact, "complexity_tier");
        if raw.is_none() {
            raw = get(
                or_obj(artifact.get("estimation_summary"))?,
                "complexity_tier",
            );
        }
        if let Some(tier) = to_enum(&COMPLEXITY_TIER, raw) {
            return Ok(Some(tier));
        }
    }
    let preview = read_json(&run_dir.join("migration-preview.json"));
    let preview = or_obj(preview.as_ref())?;
    Ok(map_enum(&COMPLEXITY_SIGNAL, preview.get("complexity_signal")).map(str::to_string))
}

// ------------------------------------------------------------- the attributes

/// An insertion-ordered attribute map (a Python `dict`): setting an existing
/// key keeps its position.
#[derive(Default)]
struct Attrs(Vec<(String, Json)>);

impl Attrs {
    fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(name, _)| name == key)
    }

    fn set(&mut self, key: &str, value: Json) {
        match self.0.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key.to_string(), value)),
        }
    }

    fn set_text(&mut self, key: &str, value: &str) {
        self.set(key, Json::string(value));
    }
}

fn discover(run_dir: &Path, spec: Option<InventorySpec>, attributes: &mut Attrs) -> R<()> {
    let inventory = spec.and_then(|spec| read_json(&run_dir.join(spec.inventory)));
    let resources = match &inventory {
        Some(list @ Json::Array(_)) => Some(list),
        Some(object @ Json::Object(_)) => object.get("resources"),
        _ => None,
    };
    // App-code AI detection lives only here; it is what lets an AI workload
    // with no AI resource (an SDK called from application code) report hasAi.
    let profile = read_json(&run_dir.join("ai-workload-profile.json"));
    let profile_object = or_obj(profile.as_ref())?;
    let ai_from_profile = match profile_object.get("models") {
        Some(Json::Array(models)) => !models.is_empty(),
        _ => gt_zero(or_obj(profile_object.get("summary"))?.get("total_models_detected"))?,
    };
    if let Some(Json::Array(items)) = resources {
        attributes.set(
            "resourceCount",
            Json::Int(i64::try_from(items.len().min(RESOURCE_COUNT_MAX)).unwrap_or(i64::MAX)),
        );
        let types = resource_types(items);
        attributes.set("hasDatabase", Json::Bool(types_match_db(&types)));
        attributes.set(
            "hasAi",
            Json::Bool(types_match_ai(&types) || ai_from_profile),
        );
    } else if profile.is_some() {
        attributes.set("hasAi", Json::Bool(ai_from_profile));
    }
    let coverage = match &inventory {
        Some(object @ Json::Object(_)) => to_coverage(get(
            or_obj(object.get("summary"))?,
            "classification_coverage",
        )),
        _ => None,
    };
    if let Some(coverage) = coverage {
        attributes.set_text("classificationCoverage", coverage);
    }
    Ok(())
}

fn clarify(run_dir: &Path, attributes: &mut Attrs) -> R<()> {
    let preferences = read_json(&run_dir.join("preferences.json"));
    let preferences = or_obj(preferences.as_ref())?;
    let metadata = or_obj(preferences.get("metadata"))?;
    let clarify_mode = if metadata.get("migration_type") == Some(&Json::string("ai-only")) {
        Some("AI_ONLY")
    } else {
        map_enum(&CLARIFY_MODE, metadata.get("clarify_mode"))
    };
    if let Some(mode) = clarify_mode {
        attributes.set_text("clarifyMode", mode);
    }
    if let Some(compliance) = to_compliance_list(constraint_value(preferences, "compliance")?) {
        attributes.set(
            "compliance",
            Json::Array(compliance.into_iter().map(Json::String).collect()),
        );
    }
    if let Some(value) = to_enum(
        &AVAILABILITY,
        constraint_value(preferences, "availability")?,
    ) {
        attributes.set("availability", Json::String(value));
    }
    if let Some(value) = to_enum(
        &CUTOVER_STRATEGY,
        constraint_value(preferences, "cutover_strategy")?,
    ) {
        attributes.set("cutoverStrategy", Json::String(value));
    }
    let size = constraint_value(preferences, "db_size")?
        .filter(|value| !is_falsy(value))
        .map_or_else(String::new, py_str);
    let size: String = size.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(db_size) = map_enum(&DB_SIZE, Some(&Json::String(size))) {
        attributes.set_text("dbSize", db_size);
    }
    if let Some(key) = to_enum(
        &DATABASE_TRAFFIC_ACCEPTED,
        constraint_value(preferences, "database_traffic")?,
    ) {
        let member = if key == "WRITE_HEAVY_GLOBAL" {
            "WRITE_HEAVY".to_string()
        } else {
            key
        };
        attributes.set("databaseTraffic", Json::String(member));
    }
    // gcp asks about kubernetes, heroku about a compute target; same decision.
    let mut posture = constraint_value(preferences, "kubernetes")?;
    if posture.is_none() {
        posture = constraint_value(preferences, "compute_target")?;
    }
    if let Some(value) = to_enum(&COMPUTE_POSTURE, posture) {
        attributes.set("computePosture", Json::String(value));
    }
    if let Some(value) = to_enum(
        &TARGET_REGION,
        constraint_value(preferences, "target_region")?,
    ) {
        attributes.set("targetRegion", Json::String(value));
    }
    Ok(())
}

fn estimate_facts(estimate: &Json, attributes: &mut Attrs) -> R<()> {
    let recommendation = or_obj(estimate.get("recommendation"))?;
    if !attributes.has("recommendationOutcome") {
        if let Some(outcome) = map_enum(&RECOMMENDATION_OUTCOME, recommendation.get("outcome")) {
            attributes.set_text("recommendationOutcome", outcome);
        }
    }
    if !attributes.has("recommendationConfidence") {
        if let Some(value) = to_enum(&RECOMMENDATION_CONFIDENCE, recommendation.get("confidence")) {
            attributes.set("recommendationConfidence", Json::String(value));
        }
    }
    if !attributes.has("pricingSource") {
        // The billing-only route records provenance under metadata.
        let mut raw = get(estimate, "pricing_source");
        if raw.is_none() {
            raw = get(or_obj(estimate.get("metadata"))?, "pricing_source");
        }
        if raw.is_none() {
            raw = get(or_obj(estimate.get("projected_costs"))?, "pricing_source");
        }
        if let Some(pricing) = to_pricing_source(raw)? {
            attributes.set_text("pricingSource", pricing);
        }
    }
    if !attributes.has("estimateAccuracyBand") {
        let mut accuracy = get(or_obj(estimate.get("current_costs"))?, "accuracy");
        if accuracy.is_none() {
            accuracy = get(estimate, "accuracy_confidence");
        }
        if let Some(band) = to_accuracy_band(accuracy) {
            attributes.set_text("estimateAccuracyBand", band);
        }
    }
    if !attributes.has("billingDataAvailable") {
        let considerations = or_obj(estimate.get("migration_cost_considerations"))?;
        if let Some(Json::Bool(available)) = considerations.get("billing_data_available") {
            attributes.set("billingDataAvailable", Json::Bool(*available));
        }
    }
    if !attributes.has("awsProjectedCostBalanced") {
        for (tier, option, key) in [
            (
                "optimized",
                "option_c_optimized",
                "awsProjectedCostOptimized",
            ),
            ("balanced", "option_b_balanced", "awsProjectedCostBalanced"),
            ("premium", "option_a_premium", "awsProjectedCostPremium"),
        ] {
            if let Some(usd) = projected_cost(estimate, tier, option)? {
                attributes.set(key, Json::Int(usd));
            }
        }
    }
    Ok(())
}

/// `spendBand` and `spendBasis` travel as a pair: a band without its basis is
/// indistinguishable from a measured figure. The basis comes from the cost
/// container's own `source`; when a route omits it, confirmed billing data is
/// the one case that can still be named. A basis alone is harmless and is
/// still reported.
fn spend_facts(run_dir: &Path, estimates: &[Json], attributes: &mut Attrs) -> R<()> {
    for estimate in estimates {
        let containers = cost_containers(estimate);
        let mut basis = None;
        for container in &containers {
            let source = get(container, "source");
            basis = if source == Some(&Json::string("preferences")) {
                preference_spend_basis(run_dir)?
            } else {
                map_enum(&SPEND_BASIS, source)
            };
            if basis.is_some() {
                break;
            }
        }
        if basis.is_none()
            && or_obj(estimate.get("migration_cost_considerations"))?.get("billing_data_available")
                == Some(&Json::Bool(true))
        {
            basis = Some("BILLING_DATA");
        }
        let band = containers.iter().find_map(|container| {
            SOURCE_SPEND_KEYS
                .iter()
                .find_map(|key| to_spend_band(container.get(key)))
        });
        if let (Some(basis), Some(band)) = (basis, band) {
            attributes.set_text("spendBand", band);
            attributes.set_text("spendBasis", basis);
            break;
        }
        if let Some(basis) = basis {
            if !attributes.has("spendBasis") {
                attributes.set_text("spendBasis", basis);
            }
        }
    }
    Ok(())
}

fn estimate(run_dir: &Path, attributes: &mut Attrs) -> R<()> {
    let estimates: Vec<Json> = ESTIMATE_FILES
        .iter()
        .filter_map(|name| read_json(&run_dir.join(name)))
        .filter(|value| matches!(value, Json::Object(_)))
        .collect();
    for item in &estimates {
        estimate_facts(item, attributes)?;
    }
    spend_facts(run_dir, &estimates, attributes)
}

/// The attributes object for `event`, or `None` when there is nothing to say.
///
/// # Errors
///
/// [`Abort`] when an artifact has a shape the Python code raised on.
pub(super) fn derive_attributes(
    run_dir: &Path,
    skill: &str,
    event: &Event,
    status: &Json,
) -> R<Option<Json>> {
    let mut attributes = Attrs::default();
    let spec = skill_inventory(skill);
    if let Some(provider) = source_provider(run_dir, spec)? {
        attributes.set_text("sourceProvider", provider);
    }
    let phase_event = event.name == "PHASE_COMPLETED";
    let phase = event.phase.as_deref();

    if phase_event && phase == Some("DISCOVER") {
        discover(run_dir, spec, &mut attributes)?;
    }
    if phase_event && phase == Some("CLARIFY") {
        clarify(run_dir, &mut attributes)?;
    }
    if phase_event && phase == Some("ESTIMATE") {
        estimate(run_dir, &mut attributes)?;
    }
    if phase_event && phase == Some("GENERATE") {
        let report = read_json(&run_dir.join("validation-report.json"));
        let report = or_obj(report.as_ref())?;
        if let Some(value) = to_enum(&VALIDATION_STATUS, report.get("status")) {
            attributes.set("validationStatus", Json::String(value));
        }
    }
    let from_design_onward = (phase_event || event.name == "GATE_FAILED")
        && phase.is_some_and(|phase| DESIGN_ONWARD.contains(&phase));
    if from_design_onward || event.name == "RUN_COMPLETED" {
        if let Some(tier) = complexity_tier(run_dir, status)? {
            attributes.set("complexityTier", Json::String(tier));
        }
    }
    if let Some(mode) = event.run_mode {
        attributes.set_text("runMode", mode);
    }
    if let Some(reason) = event.failure_reason {
        attributes.set_text("failureReason", reason);
    }
    Ok(if attributes.0.is_empty() {
        None
    } else {
        Some(Json::Object(attributes.0))
    })
}
