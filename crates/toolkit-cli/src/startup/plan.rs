//! `toolkit startup emit-plan --migration-dir DIR`: copies the already
//! validated cost figures of a finished migration run into `plan.json` for the
//! AWS Startups Migrate web import (port of `emit-plan-json.py`).
//!
//! Nothing is generated: every field is copied from a validated artifact or
//! omitted. The command is fail-open by design. A missing or unreadable input
//! leaves the migration successful, writes no file, prints a `PLAN_SKIP`
//! status line and exits 0, so a missed handoff never costs a customer their
//! migration result. Only a usage error exits non-zero.
//!
//! Cost routes: a run costs its work as infra (`estimation-infra.json`),
//! billing-only (`estimation-billing.json`) and/or AI (`estimation-ai.json`).
//! Infra or billing alone gives `INFRA_ONLY`; a base route plus AI gives
//! `FULL` (the two figures summed). A standalone AI-only run is deferred.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use toolkit_core::{neutralize_line, Json, ToolkitError};

use super::artifacts::pyjson::{dumps, loads};
use super::{is_regular_file, is_regular_file_under, read_text, read_text_under};
use crate::manifests::{is_falsy, is_py_space, py_repr_opt, py_strip};
use crate::{Invocation, Output};

/// Registration of the `startup-plan` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-plan",
    commands: &[crate::CommandSpec {
        path: &["startup", "emit-plan"],
        flags: &[],
        value_flags: &["--migration-dir", "--plugin-root", "--plugin-json"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: crate::startup::emit_plan_command,
    }],
    synopsis: r"  toolkit startup emit-plan --migration-dir DIR [--plugin-root DIR] [--plugin-json FILE]
",
    description: r"  startup emit-plan
                Copy the validated cost figures of a finished GCP or Heroku
                migration run in --migration-dir DIR into plan.json for the
                AWS Startups Migrate web import (producerVersion from
                --plugin-json FILE, default: .claude-plugin/plugin.json of
                --plugin-root DIR). Fail-open: any missing or unreadable input
                prints `PLAN_SKIP | reason=...`, writes no file and exits 0;
                success prints `PLAN_OK | path=... | platform=... | scope=...`.
",
    exit_notes: "",
    doc: "docs/migration/startup-validators.md",
    synopsis_rank: 80,
    description_rank: 70,
};

/// Highest `plan.json` schema the web import page understands.
const SCHEMA_VERSION: i64 = 1;

/// USD cap of the strict web import contract (inclusive).
const MAX_USD_AMOUNT: i64 = 100_000_000;
/// The same cap as a float, for comparisons against float amounts.
const MAX_USD_AMOUNT_F: f64 = 100_000_000.0;
/// At most this many service items are emitted.
const MAX_SERVICE_ITEMS: usize = 100;
/// Longest `serviceName`, counted in UTF-16 code units like the JS schema.
const MAX_SERVICE_NAME_LEN: usize = 128;

/// Naming of the write temp files, shared by the writer and the orphan sweep.
const TEMP_PREFIX: &str = ".plan-";
const TEMP_SUFFIX: &str = ".json.tmp";
/// A real write takes milliseconds, so an older temp is an orphan of a crashed
/// run and safe to reclaim.
const ORPHAN_TEMP_AGE: Duration = Duration::from_secs(3600);

/// Why the plan is not written.
enum Stop {
    /// A deterministic disqualification: a stale `plan.json` is dropped.
    Skip(String),
    /// An unclassifiable input fault: a previous `plan.json` is kept.
    Fail(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Fail(message)
    }
}

type Step<T> = Result<T, Stop>;

fn skip<T>(reason: impl Into<String>) -> Step<T> {
    Err(Stop::Skip(reason.into()))
}

/// `owning_skill` (telemetry id) to `sourcePlatform`. `LLM_TO_BEDROCK` and
/// `AZURE_TO_AWS` are intentionally absent (no importable mapping yet).
fn platform_for(skill: &str) -> Option<&'static str> {
    match skill {
        "GCP_TO_AWS" => Some("GCP"),
        "HEROKU_TO_AWS" => Some("HEROKU"),
        _ => None,
    }
}

/// Where each platform's source-monthly baseline lives, as `(container, field)`.
fn source_field(platform: &str) -> Option<(&'static str, &'static str)> {
    match platform {
        "GCP" => Some(("current_costs", "gcp_monthly")),
        "HEROKU" => Some(("cost_comparison", "heroku_monthly_baseline")),
        "AZURE" => Some(("current_costs", "azure_monthly")),
        _ => None,
    }
}

/// The scopes the import handler accepts per platform.
fn valid_scope(platform: &str, scope: &str) -> bool {
    match platform {
        "GCP" | "AZURE" => matches!(scope, "INFRA_ONLY" | "FULL"),
        "HEROKU" => scope == "INFRA_ONLY",
        "OPENAI" => scope == "AI_ONLY",
        _ => false,
    }
}

/// The plugin's `pricing_source.status` values mapped to the web enum.
fn pricing_enum(status: &str) -> Option<&'static str> {
    match status {
        "cached" => Some("CACHED"),
        "cached_fallback" => Some("CACHED_FALLBACK"),
        "cached_stale" => Some("CACHED_STALE"),
        "live" => Some("LIVE"),
        "unavailable" => Some("UNAVAILABLE"),
        _ => None,
    }
}

/// A real, finite, non-negative JSON number. A JSON boolean is not one.
fn is_amount(value: &Json) -> bool {
    match value {
        Json::Int(number) => *number >= 0,
        Json::Float(number) => number.is_finite() && *number >= 0.0,
        _ => false,
    }
}

fn within_cap(value: &Json) -> bool {
    match value {
        Json::Int(number) => *number <= MAX_USD_AMOUNT,
        Json::Float(number) => *number <= MAX_USD_AMOUNT_F,
        _ => false,
    }
}

/// A non-negative number within the USD cap: safe to copy into the optional
/// cost fields, which are dropped (not failed) when over the cap.
fn is_usable_amount(value: &Json) -> bool {
    is_amount(value) && within_cap(value)
}

/// `a + b` with Python's int/float rules (an `i64` overflow continues as a
/// float, like Python's unbounded integers continue past the cap anyway).
#[allow(clippy::cast_precision_loss)]
fn add_amounts(left: &Json, right: &Json) -> Json {
    let float = |value: &Json| match value {
        Json::Int(number) => *number as f64,
        Json::Float(number) => *number,
        _ => 0.0,
    };
    if let (Json::Int(a), Json::Int(b)) = (left, right) {
        if let Some(sum) = a.checked_add(*b) {
            return Json::Int(sum);
        }
    }
    Json::Float(float(left) + float(right))
}

/// Length as the JS schema counts it (UTF-16 code units).
fn import_str_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The pricing source enum for a route's `pricing_source`: an object with a
/// `status` (infra) or a plain string (billing/AI). A `cached` status flagged
/// stale becomes `CACHED_STALE`.
fn pricing_source(raw: Option<&Json>) -> Step<Option<&'static str>> {
    let status = match raw {
        Some(object @ Json::Object(_)) => {
            let status = object.get("status");
            if status.and_then(Json::as_str) == Some("cached")
                && object
                    .get("fallback_staleness")
                    .and_then(|staleness| staleness.get("is_stale"))
                    == Some(&Json::Bool(true))
            {
                return Ok(Some("CACHED_STALE"));
            }
            status
        }
        Some(text @ Json::String(_)) => Some(text),
        _ => return Ok(None),
    };
    match status {
        // `dict.get(unhashable)` raises in Python; the run fails open.
        Some(Json::Array(_)) => Err(Stop::Fail("unhashable type: 'list'".to_string())),
        Some(Json::Object(_)) => Err(Stop::Fail("unhashable type: 'dict'".to_string())),
        Some(Json::String(text)) => Ok(pricing_enum(text)),
        _ => Ok(None),
    }
}

fn is_ascii_digit(chars: &[char], at: usize) -> bool {
    chars.get(at).is_some_and(char::is_ascii_digit)
}

fn skip_spaces(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).copied().is_some_and(is_py_space) {
        at += 1;
    }
    at
}

fn starts_with(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

/// `\d+(?:\.\d+)?` at `at`: the digits and the end position.
fn number(chars: &[char], at: usize) -> Option<(String, usize)> {
    if !is_ascii_digit(chars, at) {
        return None;
    }
    let mut end = at;
    while is_ascii_digit(chars, end) {
        end += 1;
    }
    if chars.get(end) == Some(&'.') && is_ascii_digit(chars, end + 1) {
        end += 1;
        while is_ascii_digit(chars, end) {
            end += 1;
        }
    }
    Some((chars[at..end].iter().collect(), end))
}

/// `(?!\s*(?:on|of|for)\s+the\b)` at `at`: a scoping preposition makes the
/// figure a per-line one, not the run's overall band.
fn is_scoped(chars: &[char], at: usize) -> bool {
    let start = skip_spaces(chars, at);
    ["on", "of", "for"].iter().any(|word| {
        if !starts_with(chars, start, word) {
            return false;
        }
        let after_word = start + word.len();
        let after_spaces = skip_spaces(chars, after_word);
        after_spaces > after_word
            && starts_with(chars, after_spaces, "the")
            && !chars
                .get(after_spaces + 3)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
    })
}

/// The pattern `(?:±|\+/-)\s*(\d+(?:\.\d+)?)\s*(?:-\s*(\d+(?:\.\d+)?))?\s*%`
/// anchored at `start`, with the scoping lookahead after the percent sign.
fn band_at(chars: &[char], start: usize) -> Option<(String, Option<String>)> {
    let after_sign = match chars.get(start)? {
        '±' => start + 1,
        '+' if starts_with(chars, start, "+/-") => start + 3,
        _ => return None,
    };
    let (low, end) = number(chars, skip_spaces(chars, after_sign))?;
    let mut pos = skip_spaces(chars, end);
    let mut high = None;
    if chars.get(pos) == Some(&'-') {
        if let Some((digits, end)) = number(chars, skip_spaces(chars, pos + 1)) {
            high = Some(digits);
            pos = skip_spaces(chars, end);
        }
    }
    if chars.get(pos) != Some(&'%') || is_scoped(chars, pos + 1) {
        return None;
    }
    Some((low, high))
}

/// `round(float(text))` as Python computes it (ties to even).
fn rounded(digits: &str) -> Step<f64> {
    let value: f64 = digits.parse().unwrap_or(f64::INFINITY);
    if value.is_finite() {
        Ok(value.round_ties_even())
    } else {
        Err(Stop::Fail(
            "cannot convert float infinity to integer".to_string(),
        ))
    }
}

/// `0 <= value <= 100` as an integer percent.
#[allow(clippy::cast_possible_truncation)]
fn percent(value: f64) -> Option<i64> {
    (0.0..=100.0).contains(&value).then_some(value as i64)
}

/// The `cost.accuracy` object for a route, or `None` when not derivable. Only
/// an explicitly declared, sign-prefixed overall band (`±5-10%`, `+/-30%`) that
/// is not scoped to a sub-component counts.
fn accuracy(data: &Json) -> Step<Option<Json>> {
    let Some(Json::String(text)) = data.get("accuracy_confidence") else {
        return Ok(None);
    };
    let chars: Vec<char> = text.chars().collect();
    let Some((low, high)) = (0..chars.len()).find_map(|start| band_at(&chars, start)) else {
        return Ok(None);
    };
    let mut low = rounded(&low)?;
    let mut high = match high {
        Some(digits) => rounded(&digits)?,
        None => low,
    };
    if low > high {
        std::mem::swap(&mut low, &mut high);
    }
    let (Some(low), Some(high)) = (percent(low), percent(high)) else {
        return Ok(None);
    };
    let mut members = vec![
        ("minPercent", Json::Int(low)),
        ("maxPercent", Json::Int(high)),
    ];
    // pricing_source is top-level (infra object / AI string) or under metadata
    // (billing).
    let mut raw = data.get("pricing_source").filter(|v| **v != Json::Null);
    if raw.is_none() {
        raw = data
            .get("metadata")
            .filter(|metadata| matches!(metadata, Json::Object(_)))
            .and_then(|metadata| metadata.get("pricing_source"))
            .filter(|v| **v != Json::Null);
    }
    if let Some(source) = pricing_source(raw)? {
        members.push(("pricingSource", Json::string(source)));
    }
    Ok(Some(Json::object(members)))
}

fn percent_of(band: &Json, key: &str) -> i64 {
    match band.get(key) {
        Some(Json::Int(value)) => *value,
        _ => 0,
    }
}

/// The LOOSER of two declared bands, carried whole (never a synthesized
/// min/max); `pricingSource` is kept only when both routes declare the same.
fn looser_accuracy(first: &Json, second: &Json) -> Json {
    let key = |band: &Json| {
        (
            percent_of(band, "maxPercent"),
            percent_of(band, "minPercent"),
        )
    };
    let looser = if key(first) >= key(second) {
        first
    } else {
        second
    };
    let mut members = vec![
        ("minPercent", Json::Int(percent_of(looser, "minPercent"))),
        ("maxPercent", Json::Int(percent_of(looser, "maxPercent"))),
    ];
    if let Some(source) = first.get("pricingSource") {
        if second.get("pricingSource") == Some(source) {
            members.push(("pricingSource", source.clone()));
        }
    }
    Json::object(members)
}

/// The infra estimate's monthly source-platform baseline. Reads only the
/// platform's own field; an explicitly non-USD currency omits the figure.
fn source_monthly(data: &Json, platform: &str) -> Option<Json> {
    let (container_key, field) = source_field(platform)?;
    let container = data
        .get(container_key)
        .filter(|value| matches!(value, Json::Object(_)))?;
    if let Some(Json::String(currency)) = container.get("currency") {
        let currency = py_strip(currency).to_uppercase();
        if !currency.is_empty() && currency != "USD" {
            return None;
        }
    }
    container
        .get(field)
        .filter(|value| is_usable_amount(value))
        .cloned()
}

/// One service line item, or `None` when the name is unusable (blank, a
/// rollup, or longer than the contract allows).
fn service_item(name: &str, monthly: &Json) -> Option<Json> {
    let name = py_strip(name);
    if name.is_empty()
        || name.to_lowercase() == "total"
        || import_str_len(name) > MAX_SERVICE_NAME_LEN
    {
        return None;
    }
    Some(Json::object(vec![
        ("serviceName", Json::string(name)),
        ("monthlyCost", monthly.clone()),
        ("classification", Json::string("INFRASTRUCTURE")),
    ]))
}

/// One `projected_costs.breakdown` entry: a bare monthly number or an object
/// carrying the figure under `monthly`, `mid` or `right_sized_monthly`.
fn breakdown_item(key: &str, entry: &Json) -> Option<Json> {
    // Drop the aggregate row (any casing) so the total is never a service.
    if py_strip(key).to_lowercase() == "total" {
        return None;
    }
    if is_usable_amount(entry) {
        return service_item(key, entry);
    }
    if !matches!(entry, Json::Object(_)) {
        return None;
    }
    // Excluded rows (Azure's no-rate / no-quantity / partial-rate lines and
    // priced floors) are skipped before any figure is read.
    if entry.get("excluded_from_total") == Some(&Json::Bool(true))
        || entry.get("exclusion_reason").is_some_and(|v| !is_falsy(v))
    {
        return None;
    }
    // The first PRESENT key is validated; never substitute another figure when
    // the primary one is out of range.
    let monthly = ["monthly", "mid", "right_sized_monthly"]
        .iter()
        .find_map(|name| entry.get(name))?;
    if !is_usable_amount(monthly) {
        return None;
    }
    let label = entry
        .get("service")
        .filter(|value| !is_falsy(value))
        .or_else(|| entry.get("aws_service"));
    let name = match label {
        Some(Json::String(text)) if !py_strip(text).is_empty() => text.as_str(),
        _ => key,
    };
    service_item(name, monthly)
}

/// Per-service items from `projected_costs.breakdown`; all of them are
/// `INFRASTRUCTURE` (the single AI line of a combined run is added later).
fn service_items(projected: &Json) -> Vec<Json> {
    let Some(Json::Object(breakdown)) = projected.get("breakdown") else {
        return Vec::new();
    };
    breakdown
        .iter()
        .filter_map(|(key, entry)| breakdown_item(key, entry))
        .collect()
}

/// Per-service items from a billing estimate's `aws_projection.services[]`.
fn billing_service_items(projection: Option<&Json>) -> Vec<Json> {
    let Some(Json::Array(services)) = projection.and_then(|value| value.get("services")) else {
        return Vec::new();
    };
    services
        .iter()
        .filter_map(|service| {
            let cost = service.get("aws_mid").filter(|v| is_usable_amount(v))?;
            let name = service.get("aws_target").and_then(Json::as_str)?;
            service_item(name, cost)
        })
        .collect()
}

/// A cost artifact when present: `None` when absent, a skip when present but
/// not a JSON object, a failure when unreadable or invalid JSON.
fn load_route(dir: &Path, name: &str) -> Step<Option<Json>> {
    let path = dir.join(name);
    if !is_regular_file(&path)? {
        return Ok(None);
    }
    let data = loads(&read_text(&path, name)?).map_err(|error| Stop::Fail(error.to_string()))?;
    if matches!(data, Json::Object(_)) {
        Ok(Some(data))
    } else {
        skip(format!("{name} is not a JSON object"))
    }
}

/// AWS figure, optional source baseline and service items of one base route.
struct Route {
    aws: Json,
    source: Option<Json>,
    items: Vec<Json>,
}

fn infra_route(data: &Json, platform: &str) -> Step<Route> {
    let projected = data.get("projected_costs");
    let Some(aws) = projected
        .and_then(|value| value.get("aws_monthly_balanced"))
        .filter(|value| is_amount(value))
    else {
        return skip("no usable projected_costs.aws_monthly_balanced");
    };
    Ok(Route {
        aws: aws.clone(),
        source: source_monthly(data, platform),
        items: projected.map(service_items).unwrap_or_default(),
    })
}

fn billing_route(data: &Json) -> Step<Route> {
    let comparison = data.get("cost_comparison");
    let Some(aws) = comparison
        .and_then(|value| value.get("aws_monthly_mid"))
        .filter(|value| is_amount(value))
    else {
        return skip("no usable cost_comparison.aws_monthly_mid");
    };
    Ok(Route {
        aws: aws.clone(),
        source: comparison
            .and_then(|value| value.get("gcp_monthly"))
            .filter(|value| is_usable_amount(value))
            .cloned(),
        items: billing_service_items(data.get("aws_projection")),
    })
}

/// The AI estimate's Bedrock monthly figure. Its source baseline is
/// intentionally not read: it is not comparable with the infra baseline.
fn ai_route(data: &Json) -> Step<Json> {
    match data
        .get("cost_comparison")
        .and_then(|value| value.get("projected_bedrock_monthly"))
        .filter(|value| is_amount(value))
    {
        Some(aws) => Ok(aws.clone()),
        None => skip("no usable cost_comparison.projected_bedrock_monthly"),
    }
}

/// The telemetry ids a skill may legitimately carry; only these are echoed.
const KNOWN_OWNING_SKILLS: [&str; 4] = [
    "GCP_TO_AWS",
    "HEROKU_TO_AWS",
    "LLM_TO_BEDROCK",
    "AZURE_TO_AWS",
];

/// How a skip reason names `owning_skill`. The status file is untrusted payload,
/// so its value is never echoed: a recognised telemetry id appears as its
/// Python-style repr, an absent key as `None`, anything else only by its JSON
/// type (#4639).
fn owning_skill_shown(value: Option<&Json>) -> String {
    match value {
        None | Some(Json::Null) => "None".to_string(),
        Some(Json::String(text)) if KNOWN_OWNING_SKILLS.contains(&text.as_str()) => {
            py_repr_opt(value)
        }
        Some(Json::String(_)) => "(unrecognised string)".to_string(),
        Some(Json::Array(_)) => "(a list)".to_string(),
        Some(Json::Object(_)) => "(a dict)".to_string(),
        Some(_) => "(a number or boolean)".to_string(),
    }
}

/// `run_id` and `sourcePlatform` of `.phase-status.json`.
fn read_status(dir: &Path) -> Step<(String, &'static str)> {
    let path = dir.join(".phase-status.json");
    if !is_regular_file(&path)? {
        return skip("no .phase-status.json in migration dir");
    }
    let status = loads(&read_text(&path, ".phase-status.json")?)
        .map_err(|error| Stop::Fail(error.to_string()))?;
    if !matches!(status, Json::Object(_)) {
        return skip(".phase-status.json is not a JSON object");
    }
    let owning_skill = status.get("owning_skill");
    let Some(platform) = owning_skill.and_then(Json::as_str).and_then(platform_for) else {
        return skip(format!(
            "owning_skill {} has no web handoff yet",
            owning_skill_shown(owning_skill)
        ));
    };
    // runId carries the attribution the handoff exists for; a numeric or blank
    // one would be rejected by the strict web schema.
    let run_id = match status.get("run_id") {
        Some(Json::String(text)) if !py_strip(text).is_empty() => py_strip(text),
        _ => return skip("no usable run_id in .phase-status.json"),
    };
    // A UUID-shaped run_id is canonicalized to lowercase (macOS `uuidgen`).
    let run_id = if is_uuid(run_id) {
        run_id.to_lowercase()
    } else {
        run_id.to_string()
    };
    Ok((run_id, platform))
}

/// `\A[0-9a-fA-F]{8}-...-[0-9a-fA-F]{12}\Z`.
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The scope, basis and totals decided by which routes are present.
struct Costs {
    scope: &'static str,
    basis: &'static str,
    aws: Json,
    source: Option<Json>,
    items: Vec<Json>,
}

/// The base (non-AI) route: infra takes precedence over billing, because both
/// files coexist after a billing-only run re-enters with Terraform.
fn base_route(
    infra: Option<&Json>,
    billing: Option<&Json>,
    platform: &str,
) -> Step<Option<(Route, &'static str)>> {
    if let Some(data) = infra {
        return Ok(Some((infra_route(data, platform)?, "BALANCED")));
    }
    if let Some(data) = billing {
        return Ok(Some((billing_route(data)?, "BILLING_MID")));
    }
    Ok(None)
}

fn combine(
    infra: Option<&Json>,
    billing: Option<&Json>,
    ai: Option<&Json>,
    platform: &str,
) -> Step<Costs> {
    let base = base_route(infra, billing, platform)?;
    let ai_aws = ai.map(ai_route).transpose()?;
    let costs = match (base, ai_aws) {
        (None, None) => return skip("no cost estimate to hand off"),
        // The contract only accepts AI_ONLY with sourcePlatform OPENAI, and the
        // LLM-to-Bedrock path persists no cost file: defer.
        (None, Some(_)) => return skip("AI-only run has no valid web mapping yet (deferred)"),
        (Some((route, basis)), None) => Costs {
            scope: "INFRA_ONLY",
            basis,
            aws: route.aws,
            source: route.source,
            items: route.items,
        },
        (Some((route, _)), Some(ai_aws)) => {
            // The AWS figures are summed and one Bedrock line added. The
            // infra and AI-spend baselines are not comparable, so sourceMonthly
            // is omitted on a combined run.
            let mut items = route.items;
            items.push(Json::object(vec![
                ("serviceName", Json::string("Amazon Bedrock")),
                ("monthlyCost", ai_aws.clone()),
                ("classification", Json::string("AI_ML")),
            ]));
            Costs {
                scope: "FULL",
                basis: if infra.is_some() {
                    "INFRA_PLUS_AI_SUM"
                } else {
                    "BILLING_MID_PLUS_AI_SUM"
                },
                aws: add_amounts(&route.aws, &ai_aws),
                source: None,
                items,
            }
        }
    };
    // Never emit a (platform, scope) the import handler rejects.
    if !valid_scope(platform, costs.scope) {
        return skip(format!(
            "{platform} + {} is not an importable combination",
            costs.scope
        ));
    }
    if !within_cap(&costs.aws) {
        return skip("total AWS monthly exceeds the supported maximum");
    }
    Ok(costs)
}

/// `cost.accuracy`: optional for infra-only/billing-only, REQUIRED for a FULL
/// plan, which carries the looser of the two declared bands.
fn plan_accuracy(costs: &Costs, base_data: Option<&Json>, ai: Option<&Json>) -> Step<Option<Json>> {
    let base = base_data.map(accuracy).transpose()?.flatten();
    if costs.scope != "FULL" {
        return Ok(base);
    }
    let ai_band = ai.map(accuracy).transpose()?.flatten();
    match (base, ai_band) {
        (Some(base), Some(ai_band)) => Ok(Some(looser_accuracy(&base, &ai_band))),
        _ => skip("FULL run needs a usable accuracy band from both the base and AI estimates"),
    }
}

/// The plugin version for `producerVersion`; any fault reading the manifest
/// just omits it.
fn producer_version(plugin_root: &Path, plugin_json: &Path) -> Option<String> {
    if !is_regular_file_under(plugin_root, plugin_json).ok()? {
        return None;
    }
    let text = read_text_under(plugin_root, plugin_json, "plugin.json").ok()?;
    match loads(&text).ok()?.get("version") {
        Some(Json::String(version)) if !version.is_empty() => Some(version.clone()),
        _ => None,
    }
}

/// The plan from validated artifacts, with its platform and scope, or the
/// reason to fail open.
fn build_plan(
    dir: &Path,
    plugin_root: &Path,
    plugin_json: &Path,
) -> Step<(Json, &'static str, &'static str)> {
    let (run_id, platform) = read_status(dir)?;
    // infra and billing are mutually exclusive (billing is the no-IaC
    // fallback); AI runs independently and may accompany either.
    let infra = load_route(dir, "estimation-infra.json")?;
    let billing = load_route(dir, "estimation-billing.json")?;
    let ai = load_route(dir, "estimation-ai.json")?;
    let costs = combine(infra.as_ref(), billing.as_ref(), ai.as_ref(), platform)?;
    let accuracy = plan_accuracy(&costs, infra.as_ref().or(billing.as_ref()), ai.as_ref())?;

    let mut cost = vec![
        ("awsMonthly", costs.aws.clone()),
        ("awsMonthlyBasis", Json::string(costs.basis)),
    ];
    if let Some(source) = &costs.source {
        cost.push(("sourceMonthly", source.clone()));
    }
    if let Some(accuracy) = accuracy {
        cost.push(("accuracy", accuracy));
    }
    // The contract caps the list; more items have no meaningful subset, so the
    // optional field is omitted rather than rejecting the whole handoff.
    if !costs.items.is_empty() && costs.items.len() <= MAX_SERVICE_ITEMS {
        cost.push(("awsServiceItems", Json::Array(costs.items.clone())));
    }
    let mut plan = vec![
        ("schemaVersion", Json::Int(SCHEMA_VERSION)),
        ("sourcePlatform", Json::string(platform)),
        ("scope", Json::string(costs.scope)),
        ("runId", Json::string(run_id)),
        ("cost", Json::object(cost)),
    ];
    // The web contract names the field `producerVersion`, not `pluginVersion`;
    // it is optional, so an unreadable manifest only omits it.
    if let Some(version) = producer_version(plugin_root, plugin_json) {
        plan.push(("producerVersion", Json::string(version)));
    }
    Ok((Json::object(plan), platform, costs.scope))
}

/// Best-effort removal of write temps abandoned by a crashed earlier run. The
/// age gate keeps a concurrent run's in-flight temp from being deleted.
fn sweep_orphan_temps(dir: &Path) {
    let Some(cutoff) = SystemTime::now().checked_sub(ORPHAN_TEMP_AGE) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.len() < TEMP_PREFIX.len() + TEMP_SUFFIX.len()
            || !name.starts_with(TEMP_PREFIX)
            || !name.ends_with(TEMP_SUFFIX)
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A unique, exclusively created temp file with owner-only permissions in
/// `dir`: concurrent reruns never share a temp, and a pre-existing temp
/// symlink is never followed.
fn create_temp(dir: &Path) -> io::Result<(fs::File, PathBuf)> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..100 {
        let token = RandomState::new().build_hasher().finish();
        let path = dir.join(format!("{TEMP_PREFIX}{token:016x}{TEMP_SUFFIX}"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // plan.json holds the customer's cost figures; keep the private mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Writes `plan.json` atomically: unique temp in the same directory, then a
/// rename, so a crash only leaves an orphan temp, never a truncated plan.
fn write_plan(plan: &Json, out_path: &Path) -> io::Result<()> {
    let dir = out_path.parent().unwrap_or_else(|| Path::new("."));
    let (mut file, temp) = create_temp(dir)?;
    let body = dumps(plan, Some(2), false) + "\n";
    let written = file.write_all(body.as_bytes()).and_then(|()| file.flush());
    drop(file);
    let result = written.and_then(|()| fs::rename(&temp, out_path));
    if result.is_err() {
        // Only this invocation's own temp is removed, never a good handoff.
        let _ = fs::remove_file(&temp);
    }
    result
}

/// `pathlib`-style normalisation: no `.` components (except a lone one) and
/// no repeated or trailing separators.
fn normalized(text: &str) -> PathBuf {
    let path: PathBuf = Path::new(text)
        .components()
        .filter(|part| !matches!(part, Component::CurDir))
        .collect();
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path
    }
}

/// Handler for `startup emit-plan --migration-dir DIR [--plugin-root DIR]
/// [--plugin-json FILE]`.
///
/// # Errors
///
/// A usage error (exit status 2) without `--migration-dir`, or a runtime error
/// when the status line cannot be written. Every input fault fails open: it
/// prints `PLAN_SKIP | reason=...` and succeeds.
pub fn emit_plan_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(dir) = invocation.value("--migration-dir") else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "the following arguments are required: --migration-dir",
        ));
    };
    let dir = normalized(dir);
    // `(root, plugin.json)`: links below the plugin root are refused (#4584); an
    // explicit `--plugin-json` is the caller's own choice, so only its last
    // component is checked.
    let (plugin_root, plugin_json) = invocation.value("--plugin-json").map_or_else(
        || {
            let root = normalized(invocation.value("--plugin-root").unwrap_or("."));
            let json = root.join(".claude-plugin").join("plugin.json");
            (root, json)
        },
        |explicit| {
            let json = normalized(explicit);
            (
                json.parent().map(Path::to_path_buf).unwrap_or_default(),
                json,
            )
        },
    );
    let out_path = dir.join("plan.json");

    // Reclaim temps abandoned by a crashed earlier run on EVERY invocation, so
    // a directory that keeps failing open still gets cleaned up.
    sweep_orphan_temps(&dir);

    match build_plan(&dir, &plugin_root, &plugin_json) {
        Err(Stop::Skip(reason)) => {
            // This run should have NO plan, so drop a stale one.
            let _ = fs::remove_file(&out_path);
            status_line(output, &format!("PLAN_SKIP | reason={reason}"))
        }
        Err(Stop::Fail(error)) => {
            // An unclassifiable fault: a previous plan may still be valid.
            status_line(
                output,
                &format!("PLAN_SKIP | reason=unreadable input: {error}"),
            )
        }
        Ok((plan, platform, scope)) => match write_plan(&plan, &out_path) {
            Ok(()) => status_line(
                output,
                &format!(
                    "PLAN_OK | path={} | platform={platform} | scope={scope}",
                    out_path.display()
                ),
            ),
            Err(error) => status_line(
                output,
                &format!("PLAN_SKIP | reason=could not write plan.json: {error}"),
            ),
        },
    }
}

/// Writes one `PLAN_*` status line to stdout. The line carries the caller's
/// directory path and OS error text, so it is escaped as ONE line: control
/// characters (newline and carriage return included, so no argument can forge a
/// second status line), bidirectional overrides and other invisible characters
/// become `\n`, `\r`, `\t` or `\uXXXX` (#4639). Ordinary text is unchanged.
fn status_line(output: &mut Output<'_>, line: &str) -> Result<(), ToolkitError> {
    output.line(&neutralize_line(line))
}

#[cfg(test)]
mod tests;
