//! Port of `amazon-dynamodb/scripts/calculate_costs.py` (slice `dynamo-cost`,
//! Issue #4389).
//!
//! Reads the structured data model (`tables`, entities, `access_patterns`),
//! computes RRU/WRU consumption and monthly on-demand costs and renders the
//! `cost_report.md` text. Every formula, constant, rounding step and message
//! of the Python source is kept; the module doc comments below name the
//! Python function each Rust item mirrors. The model is read as dynamic JSON
//! with Python's `dict.get` semantics (defaults, truthiness), because the
//! published model schema (`references/cost-model-schema.md`) is loose; a
//! field of the wrong type is a clear model error instead of a traceback.
//!
//! No AWS call, pricing API or other side effect happens here: the functions
//! are pure over the supplied JSON.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use toolkit_core::{Json, ToolkitError};

use super::pyfmt;

/// Result alias for model reading and pricing.
pub type Res<T> = Result<T, ToolkitError>;

/// A model-content error (exit status 1).
pub fn invalid(message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime("dynamodb_model_invalid", message)
}

// ---------------------------------------------------------------------------
// Pricing constants (DynamoDB Standard table class, on-demand public rates).
// ---------------------------------------------------------------------------
pub const SECONDS_PER_MONTH: f64 = 2_592_000.0;
pub const SECONDS_PER_DAY: f64 = 86_400.0;
pub const DEFAULT_RETENTION_DAYS: f64 = 30.0;
pub const WRU_PRICE: f64 = 0.625 / 1_000_000.0;
pub const RRU_PRICE: f64 = 0.125 / 1_000_000.0;
pub const STORAGE_PRICE_PER_GB_MONTH: f64 = 0.25;
pub const BYTES_PER_GB: f64 = 1_000_000_000.0;
pub const RCU_SIZE_BYTES: f64 = 4096.0;
pub const WCU_SIZE_BYTES: f64 = 1024.0;
pub const PER_ITEM_STORAGE_OVERHEAD: f64 = 100.0;
pub const GLOBAL_TABLES_OVERHEAD: f64 = 48.0;
pub const PAGE_CAP_KB: i64 = 900;
pub const VECTOR_WRITE_PRICE_PER_GB: f64 = 0.52;
pub const VECTOR_SEARCH_PRICE_PER_GB: f64 = 0.002;
pub const IA_REQUEST_MULTIPLIER: f64 = 1.25;
pub const BYTES_PER_F32: f64 = 4.0;
pub const VECTOR_METERING_MIN_BYTES: f64 = 1024.0;
pub const VECTOR_KEY_OVERHEAD_BYTES: f64 = 35.0;
pub const BYTES_PER_BASE_TABLE_NUMBER: f64 = 5.5;
pub const VECTOR_RETURN_BYTES_KEYS_ONLY: f64 = 89.0;
pub const VECTOR_RETURN_BYTES_INCLUDE_BASE: f64 = 89.0;
pub const VECTOR_TRAVERSAL_BYTES_PER_DIM_UPPER: f64 = 20.7;
pub const VECTOR_SPEND_GATE_SAFETY: f64 = 1.5;
pub const MAX_VECTOR_INDEXES_PER_TABLE: usize = 5;
pub const MAX_VECTOR_DIMENSIONS: i64 = 4096;
pub const MAX_VECTOR_TOP_K: i64 = 100;
pub const VECTOR_SEARCH_OP: &str = "SearchVectors";

/// What a `SearchVectors` row shows in the Monthly Cost column: never a dollar
/// amount (the examined-bytes term is deliberately unpriced).
pub const VECTOR_SEARCH_CELL: &str = "not priced²";

const DISCLAIMER: &str = concat!(
    "> **Disclaimer:** This estimate covers **read/write request costs** and ",
    "**storage costs** only, at DynamoDB Standard table class on-demand **full ",
    "public rates** (the 25 GB storage free tier is NOT applied — it is ",
    "account-wide and often already used). **Rates vary by region** — confirm ",
    "the figure against the AWS DynamoDB pricing page (or `aws pricing ",
    "get-products --service-code AmazonDynamoDB`) for the region you will ",
    "deploy in before quoting it. The headline figure ",
    "is **peak-sustained** — it assumes every pattern runs at its declared ",
    "`peak_rps` continuously, 24/7 for a month (the worst case). Real on-demand ",
    "spend tracks actual request volume; set `avg_rps` on patterns to also get ",
    "an expected average-volume figure. For up-to-date pricing, refer to ",
    "the [Amazon DynamoDB Pricing](https://aws.amazon.com/dynamodb/pricing/) page."
);

const GSI_FOOTNOTE: &str = concat!(
    "¹ **GSI additional writes** — When a table write changes attributes ",
    "projected into a GSI, DynamoDB performs an additional write to that index. ",
    "The additional write is sized by the projected attributes (not the base ",
    "item), which is why KEYS_ONLY / INCLUDE projections are cheaper. ",
    "[Learn more](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/GSI.html",
    "#GSI.ThroughputConsiderations.Writes)"
);

const VECTOR_SEARCH_FOOTNOTE: &str = concat!(
    "² **Vector search is not priced here, and is NOT $0** — `SearchVectors` consumes no ",
    "RCU/WCU; it bills on vector bytes examined plus bytes returned, at ",
    "$0.002/GB. The examined fraction is not derivable from a ",
    "design (measured to vary by an order of magnitude across index configurations), so no ",
    "figure is emitted rather than a wrong one. **This means the headline total above ",
    "excludes vector search cost.** See *Vector Index Capacity* below for the measured ",
    "drivers and how to obtain the real number."
);

// ---------------------------------------------------------------------------
// Operation sets.
// ---------------------------------------------------------------------------
pub fn is_write_op(op: &str) -> bool {
    matches!(
        op,
        "PutItem" | "UpdateItem" | "DeleteItem" | "BatchWriteItem" | "TransactWriteItems"
    )
}

fn is_transactional_op(op: &str) -> bool {
    matches!(op, "TransactGetItems" | "TransactWriteItems")
}

fn default_write_action(op: &str) -> &'static str {
    match op {
        "PutItem" => "create",
        "UpdateItem" => "update",
        "DeleteItem" => "delete",
        _ => "mixed",
    }
}

fn create_ratio(action: &str) -> f64 {
    match action {
        "create" => 1.0,
        "update" | "delete" => 0.0,
        _ => 0.3,
    }
}

fn projection_write_ratio(ptype: &str) -> f64 {
    match ptype {
        "INCLUDE" => 0.3,
        "KEYS_ONLY" => 0.1,
        _ => 1.0,
    }
}

fn projection_storage_ratio(ptype: &str) -> f64 {
    projection_write_ratio(ptype)
}

// ---------------------------------------------------------------------------
// Typed access to the dynamic model.
// ---------------------------------------------------------------------------
fn as_number(value: &Json) -> Option<f64> {
    match value {
        Json::Int(number) => Some(*number as f64),
        Json::Float(number) => Some(*number),
        Json::Bool(flag) => Some(f64::from(u8::from(*flag))),
        _ => None,
    }
}

/// Python `int(value)` for the JSON value space; `None` where Python raises.
/// Booleans count as 0/1 here (callers that reject them check first).
pub fn py_int(value: &Json) -> Option<i64> {
    match value {
        Json::Int(number) => Some(*number),
        Json::Float(number) if number.is_finite() => Some(number.trunc() as i64),
        Json::Bool(flag) => Some(i64::from(*flag)),
        Json::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn number(obj: &Json, key: &str, default: f64) -> Res<f64> {
    match obj.get(key) {
        None => Ok(default),
        Some(value) => as_number(value).ok_or_else(|| {
            invalid(format!(
                "`{key}` must be a number, got {}",
                pyfmt::repr(value)
            ))
        }),
    }
}

/// A string field; absent and `null` read as `None`.
fn string_field<'a>(obj: &'a Json, key: &str) -> Res<Option<&'a str>> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) => Ok(Some(text.as_str())),
        Some(other) => Err(invalid(format!(
            "`{key}` must be a string, got {}",
            pyfmt::repr(other)
        ))),
    }
}

/// An array field; absent and `null` read as the empty list.
fn array<'a>(obj: &'a Json, key: &str) -> Res<&'a [Json]> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Array(items)) => Ok(items.as_slice()),
        Some(other) => Err(invalid(format!(
            "`{key}` must be a list, got {}",
            pyfmt::repr(other)
        ))),
    }
}

fn truthy_field(obj: &Json, key: &str) -> bool {
    obj.get(key).is_some_and(pyfmt::truthy)
}

/// `int(value or default)` for an optional field.
fn int_or(value: Option<&Json>, default: i64, what: &str) -> Res<i64> {
    match value {
        Some(found) if pyfmt::truthy(found) => py_int(found).ok_or_else(|| {
            invalid(format!(
                "`{what}` must be an integer, got {}",
                pyfmt::repr(found)
            ))
        }),
        _ => Ok(default),
    }
}

fn operation(ap: &Json) -> Res<&str> {
    match ap.get("operation") {
        Some(Json::String(op)) => Ok(op.as_str()),
        Some(other) => Err(invalid(format!(
            "access pattern `operation` must be a string, got {}",
            pyfmt::repr(other)
        ))),
        None => Err(invalid("access pattern is missing `operation`")),
    }
}

fn table_name(table: &Json) -> Res<&str> {
    match table.get("table_name") {
        Some(Json::String(name)) => Ok(name.as_str()),
        _ => Err(invalid("a table is missing its string `table_name`")),
    }
}

fn strings_of(items: &[Json]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            Json::String(text) => text.clone(),
            other => pyfmt::repr(other),
        })
        .collect()
}

fn name_of(value: Option<&Json>) -> String {
    value.map_or_else(|| "None".to_string(), pyfmt::text)
}

fn type_size(type_name: &str) -> i64 {
    match type_name {
        "N" => 8,
        "BOOL" | "NULL" => 1,
        "B" => 256,
        "L" | "M" | "SS" | "NS" | "BS" => 200,
        _ => 100,
    }
}

fn attr_type_size(attr: &Json) -> i64 {
    attr.get("type")
        .and_then(Json::as_str)
        .map_or(100, type_size)
}

// ---------------------------------------------------------------------------
// Core capacity-consumption formulas (verified against live DynamoDB).
// ---------------------------------------------------------------------------

/// WRUs for a standard write: `ceil(size / 1024)`, minimum 1.
pub fn wru_for_item(item_size_bytes: f64) -> f64 {
    if item_size_bytes <= 0.0 {
        return 1.0;
    }
    (item_size_bytes / WCU_SIZE_BYTES).ceil()
}

/// RRUs reading one item: strong `ceil(size / 4096)` (min 1), eventual half.
pub fn rru_for_item(item_size_bytes: f64, strong: bool) -> f64 {
    let strong_rru = (item_size_bytes.max(1.0) / RCU_SIZE_BYTES).ceil().max(1.0);
    if strong {
        strong_rru
    } else {
        strong_rru / 2.0
    }
}

/// RRUs for a Query/Scan: the TOTAL bytes are rounded up to 4 KB, then halved
/// for eventual consistency.
pub fn rru_for_query(total_bytes_read: f64, strong: bool) -> f64 {
    rru_for_item(total_bytes_read, strong)
}

/// BatchGetItem rounds every item, then sums.
pub fn rru_for_batch_read(item_sizes: &[f64], strong: bool) -> f64 {
    item_sizes
        .iter()
        .fold(0.0, |total, size| total + rru_for_item(*size, strong))
}

/// BatchWriteItem rounds every item, then sums.
pub fn wru_for_batch_write(item_sizes: &[f64]) -> f64 {
    item_sizes.iter().map(|size| wru_for_item(*size)).sum()
}

/// WRU of the GSI write that a base-table write amplifies into, sized by the
/// PROJECTED attributes only.
pub fn gsi_write_wru(base_item_size_bytes: f64, projection_type: &str) -> f64 {
    let projected = (base_item_size_bytes * projection_write_ratio(projection_type))
        .trunc()
        .max(1.0);
    wru_for_item(projected)
}

/// Per-pattern capacity: only reads OR writes are non-zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Capacity {
    pub op: String,
    pub rcus: f64,
    pub wcus: f64,
    pub strong: bool,
    pub transactional: bool,
    pub notes: Vec<String>,
}

/// The per-item sizes of a multi-item call: the explicit `item_sizes` list or
/// `items_per_request` copies of `estimated_item_size_bytes`.
enum ItemSizes {
    Explicit(Vec<f64>),
    Repeated { size: f64, count: f64 },
}

impl ItemSizes {
    fn read_rru(&self, strong: bool) -> f64 {
        match self {
            Self::Explicit(sizes) => rru_for_batch_read(sizes, strong),
            // Half-integral addends: repeated addition equals the product.
            Self::Repeated { size, count } => rru_for_item(*size, strong) * count,
        }
    }

    fn write_wru(&self) -> f64 {
        match self {
            Self::Explicit(sizes) => wru_for_batch_write(sizes),
            Self::Repeated { size, count } => wru_for_item(*size) * count,
        }
    }
}

fn item_sizes(ap: &Json, size: f64, items: f64) -> Res<ItemSizes> {
    if let Some(explicit) = ap.get("item_sizes").filter(|value| pyfmt::truthy(value)) {
        let Json::Array(entries) = explicit else {
            return Err(invalid(format!(
                "`item_sizes` must be a list of numbers, got {}",
                pyfmt::repr(explicit)
            )));
        };
        let mut sizes = Vec::with_capacity(entries.len());
        for entry in entries {
            sizes.push(as_number(entry).ok_or_else(|| {
                invalid(format!(
                    "`item_sizes` entries must be numbers, got {}",
                    pyfmt::repr(entry)
                ))
            })?);
        }
        return Ok(ItemSizes::Explicit(sizes));
    }
    if items.fract() != 0.0 {
        return Err(invalid(format!(
            "`items_per_request` must be an integer, got {}",
            pyfmt::float_repr(items)
        )));
    }
    Ok(ItemSizes::Repeated {
        size,
        count: items.max(0.0),
    })
}

/// `calc_pattern_capacity`.
pub fn calc_pattern_capacity(ap: &Json) -> Res<Capacity> {
    let op = operation(ap)?;
    let size = number(ap, "estimated_item_size_bytes", 1024.0)?;
    let items = number(ap, "items_per_request", 1.0)?;
    let strong = ap
        .get("consistency")
        .and_then(Json::as_str)
        .unwrap_or("eventual")
        == "strong";
    let transactional = is_transactional_op(op);
    let mut notes = Vec::new();
    let mut rcus = 0.0;
    let mut wcus = 0.0;

    match op {
        "GetItem" => {
            rcus = rru_for_item(size, strong);
            if truthy_field(ap, "non_existent_rate") {
                notes.push("Non-existent items still consume the minimum RRU.".to_string());
            }
        }
        "Query" => {
            rcus = rru_for_query(size * items, strong);
            if truthy_field(ap, "filter") {
                notes.push("FilterExpression does not reduce cost.".to_string());
            }
            if truthy_field(ap, "projection") {
                notes.push("ProjectionExpression does not reduce cost.".to_string());
            }
        }
        "Scan" => {
            rcus = rru_for_query(size * items, strong);
            notes.push("Scan reads every evaluated item regardless of filter.".to_string());
        }
        "BatchGetItem" => {
            rcus = item_sizes(ap, size, items)?.read_rru(strong);
        }
        "TransactGetItems" => {
            rcus = item_sizes(ap, size, items)?.read_rru(true) * 2.0;
            notes.push("Transactional reads consume 2× RRUs.".to_string());
        }
        "PutItem" => {
            // Replacement costs the LARGER of before/after.
            let before = number(ap, "previous_item_size_bytes", 0.0)?;
            wcus = wru_for_item(size.max(before));
        }
        "UpdateItem" => {
            let before = number(ap, "previous_item_size_bytes", size)?;
            wcus = wru_for_item(size.max(before));
        }
        "DeleteItem" => {
            wcus = wru_for_item(size);
        }
        "BatchWriteItem" => {
            wcus = item_sizes(ap, size, items)?.write_wru();
        }
        "TransactWriteItems" => {
            // The transactional multiplier applies to the base table only.
            wcus = item_sizes(ap, size, items)?.write_wru() * 2.0;
            notes.push(
                "Transactional writes: 2× multiplier on base table; GSI amplification remains 1× per GSI write."
                    .to_string(),
            );
        }
        _ => {}
    }

    if let Some(rate) = ap.get("conditional_fail_rate") {
        if pyfmt::truthy(rate) && is_write_op(op) {
            notes.push(format!(
                "Conditional failures (rate={}) still consume the same WRUs.",
                pyfmt::text(rate)
            ));
        }
    }

    Ok(Capacity {
        op: op.to_string(),
        rcus,
        wcus,
        strong,
        transactional,
        notes,
    })
}

// ---------------------------------------------------------------------------
// Model loading helpers.
// ---------------------------------------------------------------------------

/// `{entity_name: {attr_name: size_bytes}}`, entity order preserved.
pub type EntitySizes = Vec<(String, Vec<(String, i64)>)>;

fn upsert<V>(entries: &mut Vec<(String, V)>, key: &str, value: V) {
    if let Some(slot) = entries.iter_mut().find(|(name, _)| name == key) {
        slot.1 = value;
    } else {
        entries.push((key.to_string(), value));
    }
}

/// `_build_entity_attr_sizes`: heuristic attribute sizes (type size plus 10
/// bytes of attribute-name overhead).
pub fn build_entity_attr_sizes(tables: &[Json]) -> Res<EntitySizes> {
    let mut result: EntitySizes = Vec::new();
    for table in tables {
        for entity in array(table, "entities")? {
            let Some(entity_name) = entity.get("entity_name").and_then(Json::as_str) else {
                return Err(invalid("an entity is missing its string `entity_name`"));
            };
            let mut sizes: Vec<(String, i64)> = Vec::new();
            for attr in array(entity, "attributes")? {
                let Some(attr_name) = attr.get("name").and_then(Json::as_str) else {
                    return Err(invalid(format!(
                        "an attribute of entity `{entity_name}` is missing its string `name`"
                    )));
                };
                upsert(&mut sizes, attr_name, attr_type_size(attr) + 10);
            }
            upsert(&mut result, entity_name, sizes);
        }
    }
    Ok(result)
}

/// `_cap_items_per_request`: cap Query/Scan `items_per_request` at the 900 KB
/// page limit using the projected attribute sizes.
pub fn cap_items_per_request(ap: &Json, entity_sizes: &EntitySizes) -> Res<f64> {
    let items = number(ap, "items_per_request", 1.0)?;
    if !matches!(operation(ap)?, "Query" | "Scan") {
        return Ok(items);
    }
    let Some(projection) = ap.get("projection").filter(|value| pyfmt::truthy(value)) else {
        return Ok(items);
    };
    let Json::Array(attributes) = projection else {
        return Err(invalid(format!(
            "`projection` must be a list of attribute names, got {}",
            pyfmt::repr(projection)
        )));
    };
    let mut projected_bytes: i64 = 0;
    for attr in attributes {
        let found = attr.as_str().and_then(|name| {
            entity_sizes.iter().find_map(|(_, sizes)| {
                sizes
                    .iter()
                    .find(|(attr_name, _)| attr_name == name)
                    .map(|(_, size)| *size)
            })
        });
        // Default fallback per attribute.
        projected_bytes += found.unwrap_or(100);
    }
    if projected_bytes <= 0 {
        return Ok(items);
    }
    let items_per_page = (PAGE_CAP_KB * 1024 / projected_bytes).max(1);
    Ok(items.min(items_per_page as f64))
}

/// `_resolve_write_action`.
pub fn resolve_write_action(ap: &Json, requirements: Option<&Json>) -> Res<String> {
    if let Some(action) = ap.get("write_action").filter(|value| pyfmt::truthy(value)) {
        return Ok(pyfmt::text(action));
    }
    if let Some(requirements) = requirements {
        let source = ap.get("source_requirement");
        let key = source.map_or_else(String::new, pyfmt::text);
        if let Some(entry) = requirements
            .get("updates")
            .and_then(|updates| updates.get(&key))
        {
            if let Some(action) = entry
                .get("write_action")
                .filter(|value| pyfmt::truthy(value))
            {
                return Ok(pyfmt::text(action));
            }
        }
    }
    Ok(default_write_action(operation(ap)?).to_string())
}

/// `_resolve_retention_days`.
pub fn resolve_retention_days(ap: &Json, tables: &[Json], requirements: Option<&Json>) -> Res<f64> {
    // A per-pattern value is the most specific signal.
    if let Some(days) = ap.get("retention_days") {
        return py_int(days).map(|value| value as f64).ok_or_else(|| {
            invalid(format!(
                "`retention_days` must be an integer, got {}",
                pyfmt::repr(days)
            ))
        });
    }
    let Some(requirements) = requirements else {
        return Ok(DEFAULT_RETENTION_DAYS);
    };
    let wanted = string_field(ap, "table")?.unwrap_or("");
    let mut entity_name: Option<&Json> = None;
    for table in tables {
        if table_name(table)? == wanted {
            if let Some(first) = array(table, "entities")?.first() {
                entity_name = first.get("entity_name");
            }
            break;
        }
    }
    if let Some(Json::String(entity)) = entity_name.filter(|value| pyfmt::truthy(value)) {
        let retention = requirements
            .get("entities")
            .and_then(|entities| entities.get(entity))
            .and_then(|definition| definition.get("retention_days"))
            .filter(|value| !matches!(value, Json::Null));
        if let Some(value) = retention {
            return as_number(value).ok_or_else(|| {
                invalid(format!(
                    "requirements `retention_days` must be a number, got {}",
                    pyfmt::repr(value)
                ))
            });
        }
    }
    let default = requirements
        .get("metadata")
        .and_then(|metadata| metadata.get("retention_days_default"))
        .filter(|value| !matches!(value, Json::Null));
    match default {
        Some(value) => as_number(value).ok_or_else(|| {
            invalid(format!(
                "requirements `retention_days_default` must be a number, got {}",
                pyfmt::repr(value)
            ))
        }),
        None => Ok(DEFAULT_RETENTION_DAYS),
    }
}

/// `_fmt`: dollars with two decimals.
pub fn fmt_money(cost: f64) -> String {
    format!("${cost:.2}")
}

/// `_padded_table`: a Markdown table padded to the widest cell (in
/// characters) of every column.
pub fn padded_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }
    let pad = |text: &str, width: usize| {
        let missing = width.saturating_sub(text.chars().count());
        format!("{text}{}", " ".repeat(missing))
    };
    let mut lines = Vec::with_capacity(rows.len() + 2);
    let header_cells: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(index, header)| pad(header, widths[index]))
        .collect();
    lines.push(format!("| {} |", header_cells.join(" | ")));
    let rules: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    lines.push(format!("| {} |", rules.join(" | ")));
    for row in rows {
        let cells: Vec<String> = (0..headers.len())
            .map(|index| pad(row.get(index).map_or("", String::as_str), widths[index]))
            .collect();
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// GSI membership and amplification.
// ---------------------------------------------------------------------------
type AttrSet = HashSet<String>;

fn entity_key(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => format!("\u{0}{}", pyfmt::repr(other)),
    }
}

/// `_entity_attr_names`: `{entity_name: attribute names}` for one table.
fn entity_attr_names(table_def: &Json) -> Res<Vec<(String, Rc<AttrSet>)>> {
    let mut out: Vec<(String, Rc<AttrSet>)> = Vec::new();
    for entity in array(table_def, "entities")? {
        let Some(name) = entity
            .get("entity_name")
            .filter(|value| pyfmt::truthy(value))
        else {
            continue;
        };
        let mut attrs = AttrSet::new();
        for attr in array(entity, "attributes")? {
            if let Some(Json::String(attr_name)) = attr.get("name") {
                if !attr_name.is_empty() {
                    attrs.insert(attr_name.clone());
                }
            }
        }
        upsert(&mut out, &entity_key(name), Rc::new(attrs));
    }
    Ok(out)
}

fn key_name(first: Option<&Json>, second: Option<&Json>) -> Option<String> {
    first
        .filter(|value| pyfmt::truthy(value))
        .or_else(|| second.filter(|value| pyfmt::truthy(value)))
        .map(entity_key)
}

/// `_gsi_key_attrs`: `(partition key, sort key)`, tolerating spellings.
fn gsi_key_attrs(gsi: &Json) -> (Option<String>, Option<String>) {
    (
        key_name(gsi.get("partition_key"), gsi.get("hash_key")),
        key_name(gsi.get("sort_key"), gsi.get("range_key")),
    )
}

/// `_table_key_attrs`: the table's own PK/SK names, carried by every item.
fn table_key_attrs(table_def: &Json) -> AttrSet {
    let mut keys = AttrSet::new();
    if let Some(schema) = table_def
        .get("key_schema")
        .filter(|value| pyfmt::truthy(value))
    {
        for field in ["partition_key", "sort_key"] {
            if let Some(Json::String(name)) = schema.get(field) {
                if !name.is_empty() {
                    keys.insert(name.clone());
                }
            }
        }
    }
    keys
}

/// `_item_carries_gsi_key`: an item is indexed by a GSI only if it carries the
/// GSI partition key AND, for a composite GSI, the sort key.
fn item_carries_gsi_key(attr_names: &AttrSet, gsi: &Json, table_keys: &AttrSet) -> bool {
    let (pk, sk) = gsi_key_attrs(gsi);
    let carried = |name: &String| attr_names.contains(name) || table_keys.contains(name);
    match pk {
        Some(pk) if carried(&pk) => {}
        _ => return false,
    }
    sk.is_none_or(|sk| carried(&sk))
}

/// `_gsi_membership_determinable`: can the model say which items belong to
/// the GSI? Otherwise callers fall back to the charge-everything bound.
fn gsi_membership_determinable(table_def: &Json, gsi: &Json) -> Res<bool> {
    let (pk, sk) = gsi_key_attrs(gsi);
    let Some(pk) = pk else {
        return Ok(false);
    };
    let mut declared = table_key_attrs(table_def);
    for entity in array(table_def, "entities")? {
        for attr in array(entity, "attributes")? {
            if let Some(Json::String(name)) = attr.get("name") {
                if !name.is_empty() {
                    declared.insert(name.clone());
                }
            }
        }
    }
    Ok(declared.contains(&pk) && sk.is_none_or(|sk| declared.contains(&sk)))
}

/// `_projection_type`.
fn projection_type(gsi: &Json) -> Res<String> {
    let Some(projection) = gsi.get("projection").filter(|value| pyfmt::truthy(value)) else {
        return Ok("ALL".to_string());
    };
    if !matches!(projection, Json::Object(_)) {
        return Err(invalid(format!(
            "GSI `projection` must be an object, got {}",
            pyfmt::repr(projection)
        )));
    }
    Ok(match projection.get("type") {
        Some(Json::String(kind)) => kind.to_uppercase(),
        _ => "ALL".to_string(),
    })
}

fn attributes_written(ap: &Json) -> Res<Option<Vec<String>>> {
    match ap.get("attributes_written") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Array(items)) => Ok(Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect(),
        )),
        Some(other) => Err(invalid(format!(
            "`attributes_written` must be a list, got {}",
            pyfmt::repr(other)
        ))),
    }
}

/// `_resolve_written_items`: `(attribute-name set, repeat count)` per written
/// entity, plus whether the entities could be identified at all.
fn resolve_written_items(ap: &Json, table_def: &Json) -> Res<(Vec<(Rc<AttrSet>, i64)>, bool)> {
    let entity_attrs = entity_attr_names(table_def)?;
    let items_per = int_or(ap.get("items_per_request"), 1, "items_per_request")?;

    if let Some(written) = ap
        .get("entities_written")
        .filter(|value| pyfmt::truthy(value))
    {
        let Json::Array(entries) = written else {
            return Err(invalid(format!(
                "`entities_written` must be a list, got {}",
                pyfmt::repr(written)
            )));
        };
        let mut flat = Vec::new();
        for entry in entries {
            let (name, count) = if matches!(entry, Json::Object(_)) {
                let name = entry
                    .get("entity")
                    .filter(|value| pyfmt::truthy(value))
                    .or_else(|| entry.get("entity_name"));
                (
                    name.map(entity_key),
                    int_or(entry.get("count"), 1, "count")?,
                )
            } else {
                (Some(entity_key(entry)), 1)
            };
            // An undeclared entity contributes items that carry no known key,
            // i.e. amplify nowhere: a visibly low number, not the full fan-out.
            let attrs = name
                .and_then(|wanted| {
                    entity_attrs
                        .iter()
                        .find(|(entity, _)| *entity == wanted)
                        .map(|(_, attrs)| Rc::clone(attrs))
                })
                .unwrap_or_default();
            flat.push((attrs, count.max(1)));
        }
        return Ok((flat, true));
    }

    if entity_attrs.len() == 1 {
        let only = Rc::clone(&entity_attrs[0].1);
        return Ok((vec![(only, items_per.max(0))], true));
    }
    Ok((Vec::new(), false))
}

/// `_gsi_index_writes_for_item`: index writes ONE item costs against a GSI
/// (0, 1 or 2).
fn gsi_index_writes_for_item(
    item_attrs: &AttrSet,
    gsi: &Json,
    op: &str,
    written: Option<&[String]>,
    table_keys: &AttrSet,
) -> Res<i64> {
    if !item_carries_gsi_key(item_attrs, gsi, table_keys) {
        return Ok(0);
    }
    if op == "UpdateItem" {
        if let Some(written) = written {
            let ptype = projection_type(gsi)?;
            let (pk, sk) = gsi_key_attrs(gsi);
            let key_moved = written
                .iter()
                .any(|name| pk.as_deref() == Some(name) || sk.as_deref() == Some(name));
            if key_moved {
                // Old index entry deleted + new one inserted.
                return Ok(2);
            }
            match ptype.as_str() {
                "ALL" => return Ok(1),
                "INCLUDE" => {
                    let projection = gsi.get("projection");
                    let included = ["attributes", "non_key_attributes", "NonKeyAttributes"]
                        .iter()
                        .filter_map(|field| projection.and_then(|p| p.get(field)))
                        .find(|value| pyfmt::truthy(value));
                    let included = match included {
                        Some(Json::Array(items)) => strings_of(items),
                        _ => Vec::new(),
                    };
                    return Ok(i64::from(
                        written.iter().any(|name| included.contains(name)),
                    ));
                }
                "KEYS_ONLY" => return Ok(0),
                _ => {}
            }
        }
    }
    Ok(1)
}

/// `calc_gsi_amplification_wru`: per-request WRU consumed by GSI
/// amplification, with the explanation strings used in the report.
pub fn calc_gsi_amplification_wru(ap: &Json, table_def: &Json) -> Res<(f64, Vec<String>)> {
    let op = operation(ap)?;
    if !is_write_op(op) {
        return Ok((0.0, Vec::new()));
    }
    let gsis = array(table_def, "gsis")?;
    if gsis.is_empty() {
        return Ok((0.0, Vec::new()));
    }
    let size = number(ap, "estimated_item_size_bytes", 1024.0)?;
    let written = attributes_written(ap)?;
    let written = written.as_deref();
    let items_per = int_or(ap.get("items_per_request"), 1, "items_per_request")?;

    let (written_items, resolved) = resolve_written_items(ap, table_def)?;
    let table_keys = table_key_attrs(table_def);
    let mut total = 0.0_f64;

    if resolved {
        let mut per_gsi_writes: Vec<(String, i64)> = Vec::new();
        let mut indeterminate: Vec<String> = Vec::new();
        let mut member_but_gated: Vec<String> = Vec::new();
        for gsi in gsis {
            let name = gsi
                .get("index_name")
                .map_or_else(|| "?".to_string(), pyfmt::text);
            let determinable = gsi_membership_determinable(table_def, gsi)?;
            let mut gated_member = false;
            let ptype = projection_type(gsi)?;
            for (item_attrs, count) in &written_items {
                let writes = if determinable {
                    let is_member = item_carries_gsi_key(item_attrs, gsi, &table_keys);
                    let writes =
                        gsi_index_writes_for_item(item_attrs, gsi, op, written, &table_keys)?;
                    if is_member && writes == 0 {
                        gated_member = true;
                    }
                    writes
                } else {
                    // Indeterminate membership: conservative unless the
                    // UpdateItem projection gate provably excludes it.
                    let (pk, sk) = gsi_key_attrs(gsi);
                    let synthetic: AttrSet = [pk, sk].into_iter().flatten().collect();
                    gsi_index_writes_for_item(&synthetic, gsi, op, written, &table_keys)?
                };
                if writes != 0 {
                    total += gsi_write_wru(size, &ptype) * writes as f64 * *count as f64;
                    match per_gsi_writes.iter_mut().find(|(known, _)| *known == name) {
                        Some(slot) => slot.1 += writes * count,
                        None => per_gsi_writes.push((name.clone(), writes * count)),
                    }
                }
            }
            if !determinable && per_gsi_writes.iter().any(|(known, _)| *known == name) {
                indeterminate.push(name.clone());
            }
            if gated_member && !per_gsi_writes.iter().any(|(known, _)| *known == name) {
                member_but_gated.push(name);
            }
        }
        let details = if per_gsi_writes.is_empty() {
            if member_but_gated.is_empty() {
                vec!["no GSI amplification — written item(s) carry no GSI key".to_string()]
            } else {
                vec![format!(
                    "no GSI amplification — {op} touched no attribute projected into {} (projection gate)",
                    member_but_gated.join(", ")
                )]
            }
        } else {
            per_gsi_writes
                .iter()
                .map(|(name, writes)| {
                    format!(
                        "{name} (×{writes} index write{}{})",
                        if *writes == 1 { "" } else { "s" },
                        if indeterminate.contains(name) {
                            ", membership unverified — GSI key not declared on any entity"
                        } else {
                            ""
                        }
                    )
                })
                .collect()
        };
        return Ok((total, details));
    }

    // Unresolved: multi-entity table without `entities_written`. Keep the
    // conservative UPPER BOUND (every item is a member of every GSI).
    for gsi in gsis {
        let (pk, sk) = gsi_key_attrs(gsi);
        let synthetic: AttrSet = [pk, sk].into_iter().flatten().collect();
        let writes = gsi_index_writes_for_item(&synthetic, gsi, op, written, &table_keys)?;
        total += gsi_write_wru(size, &projection_type(gsi)?) * writes as f64;
    }
    if matches!(op, "BatchWriteItem" | "TransactWriteItems") {
        total *= items_per as f64;
    }
    let table = string_field(ap, "table")?.unwrap_or("?");
    Ok((
        total,
        vec![format!(
            "UPPER BOUND — '{table}' has multiple entities and this pattern has no `entities_written`, so every written item is charged against every GSI. Declare `entities_written` to bill only the GSIs each written entity is a member of."
        )],
    ))
}

// ---------------------------------------------------------------------------
// Vector index costs.
// ---------------------------------------------------------------------------
fn vector_indexes(table_def: Option<&Json>) -> Res<&[Json]> {
    match table_def {
        Some(table) => array(table, "vector_indexes"),
        None => Ok(&[]),
    }
}

/// `_ia_multiplier`: Standard-IA is 125% on both vector request dimensions.
/// "INFREQUENT" is the reliable token (the real enum value
/// `STANDARD_INFREQUENT_ACCESS` does not contain "IA" by luck of spelling).
fn ia_multiplier(table_def: Option<&Json>) -> Res<f64> {
    let class = match table_def.and_then(|table| table.get("table_class")) {
        Some(value) if pyfmt::truthy(value) => match value {
            Json::String(text) => text.to_uppercase(),
            other => {
                return Err(invalid(format!(
                    "`table_class` must be a string, got {}",
                    pyfmt::repr(other)
                )))
            }
        },
        _ => "STANDARD".to_string(),
    };
    Ok(if class.contains("INFREQUENT") || class.contains("IA") {
        IA_REQUEST_MULTIPLIER
    } else {
        1.0
    })
}

/// `_vector_projection`: `(type, attributes)`, accepting the bare-string
/// shorthand `"projection": "KEYS_ONLY"`.
fn vector_projection(vi: &Json) -> Res<(String, Vec<String>)> {
    let Some(projection) = vi.get("projection").filter(|value| pyfmt::truthy(value)) else {
        return Ok(("ALL".to_string(), Vec::new()));
    };
    match projection {
        Json::String(kind) => Ok((kind.to_uppercase(), Vec::new())),
        Json::Object(_) => {
            let kind = match projection.get("type").filter(|value| pyfmt::truthy(value)) {
                Some(Json::String(kind)) => kind.to_uppercase(),
                Some(other) => {
                    return Err(invalid(format!(
                        "vector index projection `type` must be a string, got {}",
                        pyfmt::repr(other)
                    )))
                }
                None => "ALL".to_string(),
            };
            Ok((kind, strings_of(array(projection, "attributes")?)))
        }
        other => Err(invalid(format!(
            "vector index `projection` must be a string or object, got {}",
            pyfmt::repr(other)
        ))),
    }
}

fn dimensions(vi: &Json) -> Res<i64> {
    int_or(vi.get("dimensions"), 0, "dimensions")
}

/// `_named_attr_bytes`: summed declared size of the named attributes, and
/// whether all of them were declared (undeclared ones default to 100 B).
fn named_attr_bytes(table_def: &Json, names: &[String]) -> Res<(f64, bool)> {
    let mut declared: HashMap<&str, f64> = HashMap::new();
    for entity in array(table_def, "entities")? {
        for attr in array(entity, "attributes")? {
            if let Some(Json::String(name)) = attr.get("name") {
                if !name.is_empty() {
                    declared.insert(name.as_str(), attr_type_size(attr) as f64);
                }
            }
        }
    }
    let mut total = 0.0;
    let mut all_declared = true;
    for name in names {
        if let Some(size) = declared.get(name.as_str()) {
            total += size;
        } else {
            total += 100.0;
            all_declared = false;
        }
    }
    Ok((total, all_declared))
}

/// `_vector_item_bytes`: bytes one item contributes to one vector index.
///
/// CONVENTION (kept from Python): `estimated_item_size_bytes` EXCLUDES the
/// vector attributes; the ALL branch adds the declared size to this index's
/// own `dimensions x 4`.
pub fn vector_item_bytes(vi: &Json, table_def: &Json) -> Res<i64> {
    let dims = dimensions(vi)?;
    let vector_bytes = dims as f64 * BYTES_PER_F32;
    let (ptype, named) = vector_projection(vi)?;

    if ptype == "KEYS_ONLY" {
        return Ok((vector_bytes + VECTOR_KEY_OVERHEAD_BYTES).trunc() as i64);
    }
    if ptype == "INCLUDE" {
        let (extra, _) = named_attr_bytes(table_def, &named)?;
        return Ok((vector_bytes + VECTOR_KEY_OVERHEAD_BYTES + extra).trunc() as i64);
    }
    // ALL: the whole rest of the item rides along on every vector write.
    let entities = array(table_def, "entities")?;
    let non_vector = if entities.is_empty() {
        1024.0
    } else {
        let mut sum = 0.0;
        for entity in entities {
            sum += number(entity, "estimated_item_size_bytes", 1024.0)?;
        }
        sum / entities.len() as f64
    };
    // Other vector attributes on the table are also copied, in base-table form.
    let own_attr = vi.get("vector_attribute");
    let mut others = 0.0;
    for other in array(table_def, "vector_indexes")? {
        let attr = other.get("vector_attribute");
        if attr.is_some_and(pyfmt::truthy) && attr != own_attr {
            others += dimensions(other)? as f64 * BYTES_PER_BASE_TABLE_NUMBER;
        }
    }
    Ok((vector_bytes + VECTOR_KEY_OVERHEAD_BYTES + non_vector + others).trunc() as i64)
}

/// `_vector_indexed_item_count`: `(item count, declared explicitly?)`.
fn vector_indexed_item_count(vi: &Json, table_def: &Json) -> Res<(i64, bool)> {
    if let Some(declared) = vi
        .get("estimated_indexed_items")
        .filter(|v| !matches!(v, Json::Null))
    {
        let count = py_int(declared).ok_or_else(|| {
            invalid(format!(
                "`estimated_indexed_items` must be an integer, got {}",
                pyfmt::repr(declared)
            ))
        })?;
        return Ok((count, true));
    }
    let mut total = 0.0;
    for entity in array(table_def, "entities")? {
        total += number(entity, "estimated_item_count", 100_000.0)?;
    }
    let count = if total == 0.0 {
        100_000
    } else {
        total.trunc() as i64
    };
    Ok((count, false))
}

/// `vector_write_bytes_per_call`: bytes replicated into a table's vector
/// indexes by one write call. The 1 KB minimum applies ONCE per request.
pub fn vector_write_bytes_per_call(ap: &Json, table_def: &Json) -> Res<(f64, Vec<String>)> {
    let indexes = array(table_def, "vector_indexes")?;
    if indexes.is_empty() {
        return Ok((0.0, Vec::new()));
    }
    let written = attributes_written(ap)?;
    let mut details = Vec::new();
    let mut raw_bytes = 0.0;
    for vi in indexes {
        let attr = match vi.get("vector_attribute") {
            Some(Json::String(name)) => name.as_str(),
            _ => "",
        };
        let touched = written
            .as_ref()
            .is_none_or(|names| names.iter().any(|name| name == attr));
        let index_name = name_of(vi.get("index_name"));
        if !touched {
            details.push(format!("{index_name}: not touched by this write"));
            continue;
        }
        let bytes = vector_item_bytes(vi, table_def)? as f64;
        raw_bytes += bytes;
        let note = if written.is_some() {
            ""
        } else {
            " (upper bound: attributes_written absent)"
        };
        details.push(format!(
            "{index_name}: {} B{note}",
            pyfmt::int_commas(bytes)
        ));
    }
    if raw_bytes <= 0.0 {
        return Ok((0.0, details));
    }
    Ok((raw_bytes.max(VECTOR_METERING_MIN_BYTES), details))
}

/// `_vector_return_bytes_per_result`: bytes returned per search result,
/// driven by the index projection.
fn vector_return_bytes_per_result(vi: &Json, table_def: &Json) -> Res<(f64, String)> {
    let (ptype, named) = vector_projection(vi)?;
    if ptype == "KEYS_ONLY" {
        return Ok((
            VECTOR_RETURN_BYTES_KEYS_ONLY,
            "KEYS_ONLY, keys only (~89 B measured)".to_string(),
        ));
    }
    if ptype == "INCLUDE" {
        let (extra, all_declared) = named_attr_bytes(table_def, &named)?;
        let note = if all_declared {
            ""
        } else {
            " (some sizes defaulted to 100 B — declare them to tighten)"
        };
        return Ok((
            VECTOR_RETURN_BYTES_INCLUDE_BASE + extra,
            format!("INCLUDE, keys + {} attribute(s){note}", named.len()),
        ));
    }
    Ok((
        vector_item_bytes(vi, table_def)? as f64,
        "ALL, the whole projected item is returned per result".to_string(),
    ))
}

/// `vector_search_bytes_per_call`: bytes RETURNED by one SearchVectors call,
/// the soundly measurable part (a driver, not a cost).
pub fn vector_search_bytes_per_call(ap: &Json, vi: &Json, table_def: &Json) -> Res<(f64, String)> {
    let top_k = int_or(ap.get("top_k"), 10, "top_k")?;
    let (per_result, basis) = vector_return_bytes_per_result(vi, table_def)?;
    Ok((
        top_k as f64 * per_result,
        format!(
            "TopK {top_k} x {} B/result — {basis}",
            pyfmt::commas(per_result, 0)
        ),
    ))
}

/// `vector_search_bytes_spend_gate_upper`: a deliberately HIGH byte estimate
/// for one SearchVectors call, for the pre-spend gate only (never a reported
/// cost).
pub fn vector_search_bytes_spend_gate_upper(ap: &Json, vi: &Json, table_def: &Json) -> Res<f64> {
    let (returned, _) = vector_search_bytes_per_call(ap, vi, table_def)?;
    let dims = dimensions(vi)? as f64;
    let traversal = dims * VECTOR_TRAVERSAL_BYTES_PER_DIM_UPPER;
    let bound = (returned + traversal) * VECTOR_SPEND_GATE_SAFETY;
    Ok(VECTOR_METERING_MIN_BYTES.max(bound))
}

/// `_as_int`: `int(value)` or `None`; booleans are rejected on purpose.
fn as_int(value: &Json) -> Option<i64> {
    if matches!(value, Json::Bool(_)) {
        return None;
    }
    py_int(value)
}

fn sorted_keys(value: &Json) -> Vec<String> {
    let mut keys: Vec<String> = match value {
        Json::Object(pairs) => pairs.iter().map(|(key, _)| key.clone()).collect(),
        _ => Vec::new(),
    };
    keys.sort();
    keys
}

/// `validate_vector_model`: hard validation of service constraints, so an
/// impossible design fails loudly instead of being mispriced.
pub fn validate_vector_model(tables: &[Json], access_patterns: &[Json]) -> Res<Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    for table in tables {
        let indexes = array(table, "vector_indexes")?;
        if indexes.is_empty() {
            continue;
        }
        let tname = table
            .get("table_name")
            .map_or_else(|| "<unnamed>".to_string(), pyfmt::text);

        if indexes.len() > MAX_VECTOR_INDEXES_PER_TABLE {
            errors.push(format!(
                "{tname}: {} vector indexes exceeds the per-table limit of {MAX_VECTOR_INDEXES_PER_TABLE}",
                indexes.len()
            ));
        }
        if truthy_field(table, "provisioned_capacity") {
            errors.push(format!(
                "{tname}: vector indexes require on-demand capacity; this table declares provisioned_capacity"
            ));
        }

        let mut by_attr: Vec<(Json, BTreeSet<i64>)> = Vec::new();
        for vi in indexes {
            let name = vi
                .get("index_name")
                .map_or_else(|| "<unnamed>".to_string(), pyfmt::text);
            // `index_name` is what a SearchVectors `index` resolves against.
            if !truthy_field(vi, "index_name") {
                errors.push(format!(
                    "{tname}: a vector index is missing `index_name` (found keys: {}) — the field is `index_name`, not `name`",
                    pyfmt::repr_strings(&sorted_keys(vi))
                ));
            }
            let dims = vi.get("dimensions");
            let dims_int = dims.and_then(as_int);
            if !dims_int.is_some_and(|value| (1..=MAX_VECTOR_DIMENSIONS).contains(&value)) {
                errors.push(format!(
                    "{tname}.{name}: dimensions must be an integer 1-{MAX_VECTOR_DIMENSIONS}, got {}",
                    dims.map_or_else(|| "None".to_string(), pyfmt::repr)
                ));
            }
            let function = match vi.get("distance_function") {
                Some(Json::String(text)) => text.to_uppercase(),
                _ => String::new(),
            };
            if !matches!(function.as_str(), "COSINE" | "EUCLIDEAN" | "DOT_PRODUCT") {
                errors.push(format!(
                    "{tname}.{name}: distance_function must be COSINE, EUCLIDEAN or DOT_PRODUCT, got {}",
                    vi.get("distance_function")
                        .map_or_else(|| "None".to_string(), pyfmt::repr)
                ));
            }
            if !truthy_field(vi, "vector_attribute") {
                errors.push(format!("{tname}.{name}: vector_attribute is required"));
            }
            let attr_key = vi
                .get("vector_attribute")
                .filter(|value| pyfmt::truthy(value))
                .cloned()
                .unwrap_or_else(|| Json::string(""));
            let dim_value = dims_int.unwrap_or(0);
            match by_attr.iter_mut().find(|(known, _)| *known == attr_key) {
                Some((_, set)) => {
                    set.insert(dim_value);
                }
                None => by_attr.push((attr_key, BTreeSet::from([dim_value]))),
            }
        }

        for (attr, dimset) in &by_attr {
            if dimset.len() > 1 {
                let listed: Vec<String> = dimset.iter().map(ToString::to_string).collect();
                errors.push(format!(
                    "{tname}: indexes on attribute {} declare differing dimensions [{}] — DynamoDB rejects this ('Attributes cannot be redefined')",
                    pyfmt::repr(attr),
                    listed.join(", ")
                ));
            }
        }
    }

    // Only NAMED indexes are resolvable targets: an unnamed index and a
    // pattern without `index` must not match each other.
    let mut index_names: Vec<(Option<&Json>, &Json)> = Vec::new();
    for table in tables {
        for vi in array(table, "vector_indexes")? {
            if let Some(name) = vi.get("index_name").filter(|value| pyfmt::truthy(value)) {
                index_names.push((table.get("table_name"), name));
            }
        }
    }
    for ap in access_patterns {
        if ap.get("operation").and_then(Json::as_str) != Some(VECTOR_SEARCH_OP) {
            continue;
        }
        let pid = ap
            .get("pattern_id")
            .map_or_else(|| "<unnamed>".to_string(), pyfmt::text);
        let top_k = ap.get("top_k");
        let top_k_int = top_k.map_or(Some(10), as_int);
        if !top_k_int.is_some_and(|value| (1..=MAX_VECTOR_TOP_K).contains(&value)) {
            errors.push(format!(
                "{pid}: top_k must be an integer 1-{MAX_VECTOR_TOP_K}, got {}",
                top_k.map_or_else(|| "10".to_string(), pyfmt::repr)
            ));
        }
        if !truthy_field(ap, "table") {
            errors.push(format!(
                "{pid}: operation SearchVectors has no `table` (found keys: {}) — a vector index belongs to a table, so the pattern must name it just as every other access pattern does",
                pyfmt::repr_strings(&sorted_keys(ap))
            ));
            continue;
        }
        if !truthy_field(ap, "index") {
            errors.push(format!(
                "{pid}: operation SearchVectors has no `index` (found keys: {}) — the field is `index`, not `vector_index`; it must name an entry in the table's vector_indexes",
                pyfmt::repr_strings(&sorted_keys(ap))
            ));
            continue;
        }
        let key = (ap.get("table"), ap.get("index"));
        if !index_names
            .iter()
            .any(|(table, index)| *table == key.0 && Some(*index) == key.1)
        {
            errors.push(format!(
                "{pid}: operation SearchVectors targets {}.{}, which is not declared in that table's vector_indexes",
                name_of(key.0),
                name_of(key.1)
            ));
        }
    }
    Ok(errors)
}

// ---------------------------------------------------------------------------
// Per-pattern monthly cost.
// ---------------------------------------------------------------------------

/// `pattern_monthly_cost`'s result for one access pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternCost {
    /// The effective pattern (after the page-size cap).
    pub ap: Json,
    pub cap: Capacity,
    pub base_cost: f64,
    pub gsi_amp_wru: f64,
    pub gsi_amp_details: Vec<String>,
    pub gsi_amp_cost: f64,
    pub vector_write_bytes: f64,
    pub vector_write_details: Vec<String>,
    pub vector_write_cost: f64,
    pub vector_search_bytes: f64,
    pub vector_search_basis: String,
    pub vector_search_cost: f64,
    pub is_vector_search: bool,
    pub total_cost: f64,
    pub expected_cost: f64,
}

fn with_items_per_request(ap: &Json, items: f64) -> Json {
    let value = if items.fract() == 0.0 && items.abs() < 9.0e15 {
        Json::Int(items as i64)
    } else {
        Json::Float(items)
    };
    match ap {
        Json::Object(pairs) => {
            let mut pairs = pairs.clone();
            upsert(&mut pairs, "items_per_request", value);
            Json::Object(pairs)
        }
        other => other.clone(),
    }
}

fn vector_search_cap(ap: &Json, notes: Vec<String>) -> Capacity {
    Capacity {
        op: ap
            .get("operation")
            .and_then(Json::as_str)
            .unwrap_or(VECTOR_SEARCH_OP)
            .to_string(),
        rcus: 0.0,
        wcus: 0.0,
        strong: false,
        transactional: false,
        notes,
    }
}

fn zero_cost(ap: &Json, cap: Capacity) -> PatternCost {
    PatternCost {
        ap: ap.clone(),
        cap,
        base_cost: 0.0,
        gsi_amp_wru: 0.0,
        gsi_amp_details: Vec::new(),
        gsi_amp_cost: 0.0,
        vector_write_bytes: 0.0,
        vector_write_details: Vec::new(),
        vector_write_cost: 0.0,
        vector_search_bytes: 0.0,
        vector_search_basis: String::new(),
        vector_search_cost: 0.0,
        is_vector_search: false,
        total_cost: 0.0,
        expected_cost: 0.0,
    }
}

/// `_search_vectors_monthly_cost`: search is billed on bytes examined, which a
/// design cannot determine, so only the returned-bytes driver is reported and
/// the dollar figure is deliberately zero ("not priced", never "free").
fn search_vectors_monthly_cost(ap: &Json, table_def: Option<&Json>) -> Res<PatternCost> {
    let wanted = ap.get("index");
    let found = vector_indexes(table_def)?
        .iter()
        .find(|vi| vi.get("index_name") == wanted);
    let (Some(vi), Some(table_def)) = (found, table_def) else {
        let mut result = zero_cost(
            ap,
            vector_search_cap(
                ap,
                vec![
                    "SearchVectors target index not found in the model — cost not estimated"
                        .to_string(),
                ],
            ),
        );
        result.vector_search_basis = "target index not declared".to_string();
        result.is_vector_search = true;
        return Ok(result);
    };
    let (per_call, basis) = vector_search_bytes_per_call(ap, vi, table_def)?;
    let mut result = zero_cost(
        ap,
        vector_search_cap(
            ap,
            vec!["SearchVectors consumes no RCU/WCU — billed on vector bytes examined".to_string()],
        ),
    );
    result.vector_search_bytes = per_call;
    result.vector_search_basis = basis;
    result.is_vector_search = true;
    Ok(result)
}

/// `pattern_monthly_cost`: monthly cost and capacity detail of one access
/// pattern. No AWS call, no side effect.
pub fn pattern_monthly_cost(
    ap: &Json,
    table_def: Option<&Json>,
    entity_sizes: Option<&EntitySizes>,
) -> Res<PatternCost> {
    let op = operation(ap)?;
    if op == VECTOR_SEARCH_OP {
        return search_vectors_monthly_cost(ap, table_def);
    }

    let mut effective = ap.clone();
    if matches!(op, "Query" | "Scan") {
        if let Some(sizes) = entity_sizes {
            let capped = cap_items_per_request(ap, sizes)?;
            effective = with_items_per_request(ap, capped);
        }
    }
    let ap = &effective;

    let cap = calc_pattern_capacity(ap)?;
    let rps = number(ap, "peak_rps", 0.0)?;
    // `avg_rps` defaults to the peak, so a model without it is unchanged.
    let avg_rps = number(ap, "avg_rps", rps)?;

    let per_op_cu_cost = cap.rcus * RRU_PRICE + cap.wcus * WRU_PRICE;
    let mut base_cost = per_op_cu_cost * rps * SECONDS_PER_MONTH;
    let mut expected_base_cost = per_op_cu_cost * avg_rps * SECONDS_PER_MONTH;

    let table_def = table_def.filter(|table| pyfmt::truthy(table));
    let mut gsi_amp_wru = 0.0;
    let mut gsi_amp_details = Vec::new();
    if is_write_op(op) {
        if let Some(table) = table_def {
            (gsi_amp_wru, gsi_amp_details) = calc_gsi_amplification_wru(ap, table)?;
        }
    }
    let mut gsi_amp_cost = gsi_amp_wru * WRU_PRICE * rps * SECONDS_PER_MONTH;
    let mut expected_gsi_amp_cost = gsi_amp_wru * WRU_PRICE * avg_rps * SECONDS_PER_MONTH;

    let cond_fail_rate = number(ap, "conditional_fail_rate", 0.0)?;
    if cond_fail_rate != 0.0 && is_write_op(op) {
        let multiplier = 1.0 + cond_fail_rate;
        base_cost *= multiplier;
        gsi_amp_cost *= multiplier;
        expected_base_cost *= multiplier;
        expected_gsi_amp_cost *= multiplier;
    }

    // Vector write capacity is billed in bytes at a different rate from WCU,
    // so it is a separate line rather than folded in.
    let mut vec_write_bytes = 0.0;
    let mut vec_write_details = Vec::new();
    let mut vec_write_cost = 0.0;
    let mut expected_vec_write_cost = 0.0;
    if is_write_op(op) {
        if let Some(table) = table_def {
            if !array(table, "vector_indexes")?.is_empty() {
                (vec_write_bytes, vec_write_details) = vector_write_bytes_per_call(ap, table)?;
                let rate = (VECTOR_WRITE_PRICE_PER_GB / BYTES_PER_GB) * ia_multiplier(Some(table))?;
                vec_write_cost = vec_write_bytes * rate * rps * SECONDS_PER_MONTH;
                expected_vec_write_cost = vec_write_bytes * rate * avg_rps * SECONDS_PER_MONTH;
                if cond_fail_rate != 0.0 {
                    vec_write_cost *= 1.0 + cond_fail_rate;
                    expected_vec_write_cost *= 1.0 + cond_fail_rate;
                }
            }
        }
    }

    Ok(PatternCost {
        ap: effective,
        cap,
        base_cost,
        gsi_amp_wru,
        gsi_amp_details,
        gsi_amp_cost,
        vector_write_bytes: vec_write_bytes,
        vector_write_details: vec_write_details,
        vector_write_cost: vec_write_cost,
        vector_search_bytes: 0.0,
        vector_search_basis: String::new(),
        vector_search_cost: 0.0,
        is_vector_search: false,
        total_cost: base_cost + gsi_amp_cost + vec_write_cost,
        expected_cost: expected_base_cost + expected_gsi_amp_cost + expected_vec_write_cost,
    })
}

// ---------------------------------------------------------------------------
// Storage.
// ---------------------------------------------------------------------------

/// `_gsi_membership_byte_fraction`: the fraction of the base table's BYTES a
/// GSI actually indexes (1.0 when membership cannot be determined).
fn gsi_membership_byte_fraction(table_def: &Json, gsi: &Json) -> Res<f64> {
    let entities = array(table_def, "entities")?;
    if entities.is_empty() || !gsi_membership_determinable(table_def, gsi)? {
        return Ok(1.0);
    }
    let table_keys = table_key_attrs(table_def);
    let mut total = 0.0;
    let mut member = 0.0;
    for entity in entities {
        let count = number(entity, "estimated_item_count", 100_000.0)?;
        let size = number(entity, "estimated_item_size_bytes", 1024.0)?;
        let bytes = count * size;
        total += bytes;
        let mut attrs = AttrSet::new();
        for attr in array(entity, "attributes")? {
            if let Some(Json::String(name)) = attr.get("name") {
                if !name.is_empty() {
                    attrs.insert(name.clone());
                }
            }
        }
        if item_carries_gsi_key(&attrs, gsi, &table_keys) {
            member += bytes;
        }
    }
    if total <= 0.0 {
        return Ok(1.0);
    }
    Ok(member / total)
}

/// `_storage_cost`: storage rows and the total dollars for a per-table byte
/// map. All storage is billed at the full public rate (the 25 GB free tier is
/// deliberately NOT applied).
pub fn storage_cost(
    tables: &[Json],
    by_table: &HashMap<String, f64>,
) -> Res<(Vec<Vec<String>>, f64)> {
    let mut rows = Vec::new();
    let mut total = 0.0;
    for table in tables {
        let tname = table_name(table)?;
        let mut total_bytes = by_table.get(tname).copied().unwrap_or(0.0);
        // Fallback: no write pattern hit this table, so use entity counts.
        if total_bytes == 0.0 {
            for entity in array(table, "entities")? {
                let item_size = number(entity, "estimated_item_size_bytes", 1024.0)?;
                let item_count = number(entity, "estimated_item_count", 100_000.0)?;
                let mut overhead = PER_ITEM_STORAGE_OVERHEAD;
                if truthy_field(table, "global_tables") {
                    overhead += GLOBAL_TABLES_OVERHEAD;
                }
                total_bytes += item_count * (item_size + overhead);
            }
        }

        let gb = total_bytes / BYTES_PER_GB;
        let cost = gb * STORAGE_PRICE_PER_GB_MONTH;
        rows.push(vec![
            tname.to_string(),
            "Table".to_string(),
            format!("{gb:.2}"),
            fmt_money(cost),
        ]);
        total += cost;

        for gsi in array(table, "gsis")? {
            let ratio = projection_storage_ratio(&projection_type(gsi)?);
            // A GSI stores only the items that carry its key.
            let membership = gsi_membership_byte_fraction(table, gsi)?;
            let gsi_gb = gb * ratio * membership;
            let gsi_cost = gsi_gb * STORAGE_PRICE_PER_GB_MONTH;
            let Some(index_name) = gsi.get("index_name") else {
                return Err(invalid(format!(
                    "a GSI of table `{tname}` is missing `index_name`"
                )));
            };
            rows.push(vec![
                pyfmt::text(index_name),
                "GSI".to_string(),
                format!("{gsi_gb:.2}"),
                fmt_money(gsi_cost),
            ]);
            total += gsi_cost;
        }

        // Vector index storage: sized from item COUNT, priced at the same
        // per-GB rate as table storage (there is no separate usage type).
        for vi in array(table, "vector_indexes")? {
            let (items, declared) = vector_indexed_item_count(vi, table)?;
            let per_item = vector_item_bytes(vi, table)?;
            let vector_gb = (items as f64 * per_item as f64) / BYTES_PER_GB;
            let vector_cost = vector_gb * STORAGE_PRICE_PER_GB_MONTH;
            let label = if declared {
                "Vector index"
            } else {
                "Vector index*"
            };
            rows.push(vec![
                vi.get("index_name")
                    .map_or_else(|| "<unnamed>".to_string(), pyfmt::text),
                label.to_string(),
                format!("{vector_gb:.2}"),
                fmt_money(vector_cost),
            ]);
            total += vector_cost;
        }
    }
    Ok((rows, total))
}

// ---------------------------------------------------------------------------
// Report.
// ---------------------------------------------------------------------------
struct PatternRow {
    pattern_id: String,
    op: String,
    table: String,
    index: Option<String>,
    rps: f64,
    rcus: f64,
    wcus: f64,
    strong: bool,
    transactional: bool,
    gsi_amp_cost: f64,
    total_cost: f64,
    expected_cost: f64,
    base_cost: f64,
    vector_write_bytes: f64,
    vector_write_cost: f64,
    vector_write_details: Vec<String>,
    is_vector_search: bool,
    vector_search_bytes: f64,
    vector_search_basis: String,
}

fn string_or_empty(value: Option<&Json>) -> String {
    value.map_or_else(String::new, pyfmt::text)
}

/// `calculate_and_report`: the full `cost_report.md` text (no trailing
/// newline).
pub fn calculate_and_report(model: &Json, requirements: Option<&Json>) -> Res<String> {
    let tables = array(model, "tables")?;
    let access_patterns = array(model, "access_patterns")?;
    let requirements = requirements.filter(|value| pyfmt::truthy(value));

    let mut table_map: Vec<(&str, &Json)> = Vec::new();
    for table in tables {
        let name = table_name(table)?;
        match table_map.iter_mut().find(|(known, _)| *known == name) {
            Some(slot) => slot.1 = table,
            None => table_map.push((name, table)),
        }
    }
    let lookup = |name: &str| {
        table_map
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, t)| *t)
    };
    let entity_sizes = build_entity_attr_sizes(tables)?;

    let mut results: Vec<PatternRow> = Vec::new();
    for ap in access_patterns {
        let table_def = lookup(string_field(ap, "table")?.unwrap_or(""));
        let cost = pattern_monthly_cost(ap, table_def, Some(&entity_sizes))?;
        let effective = &cost.ap;
        let Some(pattern_id) = effective.get("pattern_id") else {
            return Err(invalid("an access pattern is missing `pattern_id`"));
        };
        results.push(PatternRow {
            pattern_id: pyfmt::text(pattern_id),
            op: operation(effective)?.to_string(),
            table: string_or_empty(effective.get("table")),
            index: effective
                .get("index")
                .filter(|value| pyfmt::truthy(value))
                .map(pyfmt::text),
            rps: number(effective, "peak_rps", 0.0)?,
            rcus: cost.cap.rcus,
            wcus: cost.cap.wcus,
            strong: cost.cap.strong,
            transactional: cost.cap.transactional,
            gsi_amp_cost: cost.gsi_amp_cost,
            total_cost: cost.total_cost,
            expected_cost: cost.expected_cost,
            base_cost: cost.base_cost,
            vector_write_bytes: cost.vector_write_bytes,
            vector_write_cost: cost.vector_write_cost,
            vector_write_details: cost.vector_write_details.clone(),
            is_vector_search: cost.is_vector_search,
            vector_search_bytes: cost.vector_search_bytes,
            vector_search_basis: cost.vector_search_basis.clone(),
        });
    }

    // Storage estimates.
    let mut storage_by_table: HashMap<String, f64> = HashMap::new();
    // Parallel accumulation at avg_rps so the expected headline uses
    // average-rate storage growth, not peak.
    let mut expected_storage_by_table: HashMap<String, f64> = HashMap::new();
    for ap in access_patterns {
        if !is_write_op(operation(ap)?) {
            continue;
        }
        let write_action = resolve_write_action(ap, requirements)?;
        let ratio = create_ratio(&write_action);
        if ratio <= 0.0 {
            continue;
        }
        let rps = number(ap, "peak_rps", 0.0)?;
        if rps <= 0.0 {
            continue;
        }
        let avg_rps = number(ap, "avg_rps", rps)?;

        let raw_item_size = number(ap, "estimated_item_size_bytes", 1024.0)?;
        let mut overhead = PER_ITEM_STORAGE_OVERHEAD;
        let name = string_field(ap, "table")?.unwrap_or("");
        if lookup(name).is_some_and(|table| truthy_field(table, "global_tables")) {
            overhead += GLOBAL_TABLES_OVERHEAD;
        }
        let item_size = raw_item_size + overhead;

        let retention_days = resolve_retention_days(ap, tables, requirements)?;
        let create_rate = rps * ratio;
        let steady_state_bytes = item_size * create_rate * SECONDS_PER_DAY * retention_days;
        *storage_by_table.entry(name.to_string()).or_insert(0.0) += steady_state_bytes;
        let expected_bytes = item_size * (avg_rps * ratio) * SECONDS_PER_DAY * retention_days;
        *expected_storage_by_table
            .entry(name.to_string())
            .or_insert(0.0) += expected_bytes;
    }

    let (storage_rows, storage_total) = storage_cost(tables, &storage_by_table)?;
    let (_, expected_storage_total) = storage_cost(tables, &expected_storage_by_table)?;

    let rw_total: f64 = results.iter().map(|r| r.total_cost).sum();
    let total = storage_total + rw_total;
    // Two headlines only when some pattern sets `avg_rps` (key presence).
    let any_avg_rps = access_patterns.iter().any(|ap| ap.get("avg_rps").is_some());
    let expected_rw_total: f64 = results.iter().map(|r| r.expected_cost).sum();
    let expected_total = expected_storage_total + expected_rw_total;

    // Highest cost first; equal costs keep model order (stable sort).
    results.sort_by(|left, right| {
        right
            .total_cost
            .partial_cmp(&left.total_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // If a single pattern drives >30% of the RW total, item-size drift on it
    // could move the headline materially: say where to scrutinise inputs.
    let mut concentration_notes = Vec::new();
    if rw_total > 0.0 {
        for row in &results {
            let share = row.total_cost / rw_total;
            if share > 0.30 {
                concentration_notes.push(format!(
                    "- `{}` contributes {:.0}% of read/write cost. A 50% error in `estimated_item_size_bytes` on this pattern would move the headline number by ~{:.0}%. Revisit its attribute walkthrough if you haven't already, or rely on live validation to confirm the number.",
                    row.pattern_id,
                    share * 100.0,
                    share * 50.0
                ));
            }
        }
    }

    let mut lines: Vec<String> = vec![
        "# DynamoDB Cost Report".to_string(),
        String::new(),
        DISCLAIMER.to_string(),
        String::new(),
    ];
    if any_avg_rps {
        lines.push(format!(
            "**Peak-Sustained Monthly Cost: {}**  *(every pattern at its declared `peak_rps`, sustained 24/7 — the worst case)*",
            fmt_money(total)
        ));
        lines.push(String::new());
        lines.push(format!(
            "**Expected Monthly Cost (your stated average volume): {}**  *(patterns driven at `avg_rps`; on-demand bills per request, so this is the realistic figure)*",
            fmt_money(expected_total)
        ));
        lines.push(String::new());
        lines.push(padded_table(
            &["Source", "Peak-Sustained", "Expected"],
            &[
                vec![
                    "Storage".to_string(),
                    fmt_money(storage_total),
                    fmt_money(expected_storage_total),
                ],
                vec![
                    "Read and write requests".to_string(),
                    fmt_money(rw_total),
                    fmt_money(expected_rw_total),
                ],
            ],
        ));
    } else {
        lines.push(format!(
            "**Peak-Sustained Monthly Cost: {}**  *(every pattern at its declared `peak_rps`, sustained 24/7; set `avg_rps` on patterns for a realistic average-volume figure)*",
            fmt_money(total)
        ));
        lines.push(String::new());
        lines.push(padded_table(
            &["Source", "Monthly Cost"],
            &[
                vec!["Storage".to_string(), fmt_money(storage_total)],
                vec!["Read and write requests".to_string(), fmt_money(rw_total)],
            ],
        ));
    }
    if !concentration_notes.is_empty() {
        lines.push(String::new());
        lines.push(
            "> **Cost concentration — item-size sensitivity.** One or more patterns drive a disproportionate share of the estimate. The calculator formulas are empirically verified; the remaining risk is in the inputs — particularly `estimated_item_size_bytes`. If any of these patterns' item sizes were guessed without an attribute walkthrough, the headline number could be off by tens of percent."
                .to_string(),
        );
        lines.push(String::new());
        lines.extend(concentration_notes);
    }
    lines.push(String::new());
    lines.push("## Storage Costs".to_string());
    lines.push(String::new());
    lines.push(format!("**Monthly Cost:** {}", fmt_money(storage_total)));
    lines.push(String::new());
    lines.push(
        "*Priced at the full public Standard rate ($0.25/GB-month). The 25 GB free tier is not applied — it is account-wide and often already consumed by other tables.*"
            .to_string(),
    );
    lines.push(String::new());
    lines.push(padded_table(
        &["Resource", "Type", "Storage (GB)", "Monthly Cost"],
        &storage_rows,
    ));
    lines.push(String::new());
    lines.push("## Access Pattern Costs".to_string());
    lines.push(String::new());
    lines.push(format!("**Monthly Cost:** {}", fmt_money(rw_total)));
    lines.push(String::new());

    let mut has_gsi_footnote = false;
    let mut has_vector_search_footnote = false;
    let mut detail_rows: Vec<Vec<String>> = Vec::new();
    for row in &results {
        let mut target = row.table.clone();
        if let Some(index) = &row.index {
            target.push_str(" / ");
            target.push_str(index);
        }
        let units = if row.wcus > 0.0 { row.wcus } else { row.rcus };
        let mut flag = String::new();
        if row.transactional {
            flag.push_str(" (txn)");
        }
        if row.strong && row.wcus == 0.0 {
            flag.push_str(" (strong)");
        }
        // SearchVectors consumes no RCU/WCU AND its capacity is deliberately
        // not priced: "$0.00" would state that search is free. Render the
        // carve-out and footnote it instead.
        let cell = if row.is_vector_search {
            VECTOR_SEARCH_CELL.to_string()
        } else {
            fmt_money(row.base_cost)
        };
        detail_rows.push(vec![
            row.pattern_id.clone(),
            format!("{}{flag}", row.op),
            target,
            format!("{:.1}", row.rps),
            format!("{units:.2}"),
            cell,
        ]);
        if row.is_vector_search {
            has_vector_search_footnote = true;
        }
        if row.gsi_amp_cost > 0.0 {
            detail_rows.push(vec![
                format!("{}¹", row.pattern_id),
                "GSI writes".to_string(),
                row.table.clone(),
                format!("{:.1}", row.rps),
                "-".to_string(),
                fmt_money(row.gsi_amp_cost),
            ]);
            has_gsi_footnote = true;
        }
    }
    lines.push(padded_table(
        &[
            "Pattern",
            "Operation",
            "Table/Index",
            "Peak RPS",
            "RRU/WRU",
            "Monthly Cost",
        ],
        &detail_rows,
    ));

    if has_gsi_footnote {
        lines.push(String::new());
        lines.push(GSI_FOOTNOTE.to_string());
    }
    if has_vector_search_footnote {
        lines.push(String::new());
        lines.push(VECTOR_SEARCH_FOOTNOTE.to_string());
    }
    lines.extend(vector_report_section(&results));

    Ok(lines.join("\n"))
}

/// `_vector_report_section`: what was priced and what deliberately was not.
fn vector_report_section(results: &[PatternRow]) -> Vec<String> {
    let searches: Vec<&PatternRow> = results.iter().filter(|r| r.is_vector_search).collect();
    let writes: Vec<&PatternRow> = results
        .iter()
        .filter(|r| r.vector_write_bytes != 0.0 && !r.is_vector_search)
        .collect();
    if searches.is_empty() && writes.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<String> = vec![
        String::new(),
        "## Vector Index Capacity".to_string(),
        String::new(),
    ];

    if !writes.is_empty() {
        out.push(
            "Vector **write** capacity is metered in bytes at $0.52/GB, separately from base-table WCU, and **is included** in the headline above. A 1 KB minimum applies per request (not per index), so low-dimension vectors do not meter proportionally lower."
                .to_string(),
        );
        out.push(String::new());
        let rows: Vec<Vec<String>> = writes
            .iter()
            .map(|r| {
                let detail = if r.vector_write_details.is_empty() {
                    "-".to_string()
                } else {
                    r.vector_write_details.join("; ")
                };
                vec![
                    r.pattern_id.clone(),
                    pyfmt::commas(r.vector_write_bytes, 0),
                    fmt_money(r.vector_write_cost),
                    detail,
                ]
            })
            .collect();
        out.push(padded_table(
            &["Pattern", "Bytes/write", "Monthly Cost", "Per-index detail"],
            &rows,
        ));
        out.push(String::new());
    }

    if !searches.is_empty() {
        out.push(
            "Vector **search** capacity is **not priced here**, and that is deliberate. It is metered on the vector data a search examines inside the index plus the data returned, and measurement showed the examined fraction varies by an order of magnitude with index configuration — 12.8% of all vector bytes on a 60-item unpartitioned index against 1.3% on a 200-item partitioned one. A calibration fitted across configurations came out 51-69% low when tested at 256 / 1536 / 3072 dimensions, which is exactly where real embedding models sit. A confidently wrong figure is worse than none."
                .to_string(),
        );
        out.push(String::new());
        out.push("What IS measured, and what you can act on:".to_string());
        out.push(String::new());
        out.push(
            "- **`TopK` is exactly linear.** Halving `TopK` halves the returned-data term."
                .to_string(),
        );
        out.push(
            "- **The projection is a ~170x multiplier** on bytes returned per result: ~89 B for `KEYS_ONLY`, ~98 B for a narrow `INCLUDE`, and the whole projected item for `ALL`. A `TopK=100` search on an `ALL` index measured **1.52 MB**; the same search on `KEYS_ONLY` measured **19 KB**."
                .to_string(),
        );
        out.push(
            "- **Dimensions scale it linearly** at a fixed index configuration (~31 B per dimension measured at 256 / 1536 / 3072)."
                .to_string(),
        );
        out.push(
            "- **Index size is not a linear driver.** ANN prunes: within one index, going from 10 to 200 vectors in the searched partition barely moved the figure. That is not the same as \"population never matters\" — an EMPTY 1024-dim index measured ~2x a populated one, so do not extrapolate this shape down to a sparse or freshly-created index."
                .to_string(),
        );
        out.push(String::new());
        out.push(
            "The returned-data component below is the part that follows soundly from your design. Treat it as a **lower bound on bytes**, not a cost."
                .to_string(),
        );
        out.push(String::new());
        let rows: Vec<Vec<String>> = searches
            .iter()
            .map(|r| {
                let basis = if r.vector_search_basis.is_empty() {
                    "-".to_string()
                } else {
                    r.vector_search_basis.clone()
                };
                vec![
                    r.pattern_id.clone(),
                    format!("{} KB", pyfmt::commas(r.vector_search_bytes / 1_000.0, 1)),
                    basis,
                ]
            })
            .collect();
        out.push(padded_table(
            &["Pattern", "Returned bytes/search", "Basis"],
            &rows,
        ));
        out.push(String::new());
        out.push(format!(
            "**Get the real number before you quote one.** Vector search bills at ${VECTOR_SEARCH_PRICE_PER_GB:.3}/GB. Set `ReturnConsumedCapacity` on your `SearchVectors` calls and read `VectorSearchRequestBytes`, or chart the `VectorSearchRequestBytes` CloudWatch metric (dimensioned by `TableName` and `VectorIndexName`). A live validation run measures it against your own data and reports the observed value."
        ));
        out.push(String::new());
        out.push(
            "**The cheapest lever is the projection.** If search cost turns out to matter, narrowing an `ALL` vector index to `KEYS_ONLY` or a tight `INCLUDE` and hydrating the rest with `BatchGetItem` cuts search bytes by orders of magnitude. The projection is immutable, so that is a new-index migration."
                .to_string(),
        );
    }
    out
}
