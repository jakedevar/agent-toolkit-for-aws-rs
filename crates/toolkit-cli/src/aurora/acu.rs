//! Aurora Serverless v2 ACU estimate and provisioned-versus-serverless costs
//! (`toolkit aurora acu`).

use std::collections::BTreeMap;

use toolkit_core::{Json, ToolkitError};

use crate::manifests::py_float_repr;
use crate::{Invocation, Output};

use super::api::{
    attribute, ensure_available, on_demand_dimensions, parse_bulk_csv, parse_price_item, usd_price,
    ApiResult, AwsApi,
};
use super::args::{
    float_value, format_value, instance_value, int_value, parse_py_float, region_value, required,
    Format,
};
use super::pyfmt::{
    dumps, fmt_f, get_f64, get_str, py_max, py_min, round_num, round_to, title_case, Num,
};
use super::{
    acu_regions, location, Engine, Tool, HOURS_PER_MONTH, IO_OPT_COMPUTE_MULTIPLIER,
    STATIC_ACU_PRICE_IO_OPTIMIZED, STATIC_ACU_PRICE_STANDARD, STATIC_INSTANCES,
    STATIC_STORAGE_IO_OPT_PER_GIB, STATIC_STORAGE_STANDARD_PER_GIB,
};

const ACU_MIN: f64 = 0.5;
const ACU_MAX: i64 = 256;
const GIB_PER_ACU: f64 = 2.0;
const STATIC_LAST_UPDATED: &str = "2026-03-28";

/// One instance class: CPU count, memory and hourly price.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct InstanceSpec {
    pub vcpus: i64,
    pub memory_gib: Num,
    pub price: f64,
}

/// Active prices, instance catalog and where they came from.
#[derive(Debug, Clone)]
pub(crate) struct AcuPricing {
    pub acu_standard: f64,
    /// Tracked like the Python global; no calculation reads it.
    #[allow(dead_code)]
    pub acu_io_optimized: f64,
    pub storage_standard: f64,
    /// Tracked like the Python global; no calculation reads it.
    #[allow(dead_code)]
    pub storage_io_opt: f64,
    pub instances: BTreeMap<String, InstanceSpec>,
    pub source: Json,
}

fn static_specs() -> BTreeMap<String, InstanceSpec> {
    STATIC_INSTANCES
        .iter()
        .map(|(name, vcpus, memory, price)| {
            (
                (*name).to_string(),
                InstanceSpec {
                    vcpus: *vcpus,
                    memory_gib: Num::Int(*memory),
                    price: *price,
                },
            )
        })
        .collect()
}

impl AcuPricing {
    /// The built-in us-east-1 defaults.
    pub(crate) fn static_defaults() -> Self {
        Self {
            acu_standard: STATIC_ACU_PRICE_STANDARD,
            acu_io_optimized: STATIC_ACU_PRICE_IO_OPTIMIZED,
            storage_standard: STATIC_STORAGE_STANDARD_PER_GIB,
            storage_io_opt: STATIC_STORAGE_IO_OPT_PER_GIB,
            instances: static_specs(),
            source: Json::object(vec![
                ("source", Json::string("static_fallback")),
                ("region", Json::string("us-east-1")),
                ("last_updated", Json::string(STATIC_LAST_UPDATED)),
                ("details", Json::string("Built-in us-east-1 defaults")),
            ]),
        }
    }
}

// ---------------------------------------------------------------------------
// Live fetchers (payload parsing; the transport is `AwsApi`)
// ---------------------------------------------------------------------------

fn fetch_instance_pricing(
    api: &dyn AwsApi,
    engine: Engine,
    region: &str,
) -> ApiResult<BTreeMap<String, f64>> {
    ensure_available(api)?;
    let place = location(Tool::Acu, region).ok_or_else(|| {
        format!(
            "Unknown region '{region}'. Supported: {}",
            acu_regions().join(", ")
        )
    })?;
    let filters = [
        ("databaseEngine", engine.pricing_name()),
        ("location", place),
        ("deploymentOption", "Single-AZ"),
        ("termType", "OnDemand"),
    ];
    let mut prices = BTreeMap::new();
    for raw in api.pricing_products(&filters)? {
        let item = parse_price_item(&raw)?;
        let instance_type = attribute(&item, "instanceType");
        if !instance_type.starts_with("db.") {
            continue;
        }
        if attribute(&item, "usagetype").contains("IOOptimized") {
            continue;
        }
        for dimension in on_demand_dimensions(&item) {
            let Ok(price) = usd_price(dimension) else {
                continue;
            };
            if price > 0.0 {
                prices.insert(instance_type.to_string(), price);
            }
        }
    }
    Ok(prices)
}

fn fetch_instance_pricing_bulk(
    api: &dyn AwsApi,
    engine: Engine,
    region: &str,
) -> ApiResult<BTreeMap<String, f64>> {
    let raw = api.bulk_pricing_csv(region)?;
    let mut prices = BTreeMap::new();
    for row in parse_bulk_csv(&raw) {
        let field = |name: &str| row.get(name).map(String::as_str);
        if field("Database Engine") != Some(engine.pricing_name()) {
            continue;
        }
        if field("Deployment Option") != Some("Single-AZ") {
            continue;
        }
        // On-demand hourly rows only: the CSV also carries Reserved and
        // fixed-fee rows that would otherwise overwrite the hourly price.
        if field("TermType") != Some("OnDemand") || field("Unit") != Some("Hrs") {
            continue;
        }
        let instance_type = field("Instance Type").unwrap_or("");
        if !instance_type.starts_with("db.") {
            continue;
        }
        if field("usageType").unwrap_or("").contains("IOOptimized") {
            continue;
        }
        let Some(price) = field("PricePerUnit").and_then(parse_py_float) else {
            continue;
        };
        if price > 0.0 {
            prices.insert(instance_type.to_string(), price);
        }
    }
    Ok(prices)
}

#[derive(Debug, Default)]
struct AcuStoragePrices {
    acu_standard: Option<f64>,
    acu_io_optimized: Option<f64>,
    storage_standard: Option<f64>,
    storage_io_optimized: Option<f64>,
}

fn fetch_acu_and_storage_pricing(
    api: &dyn AwsApi,
    engine: Engine,
    region: &str,
) -> ApiResult<AcuStoragePrices> {
    ensure_available(api)?;
    let place = location(Tool::Acu, region).ok_or_else(|| format!("Unknown region '{region}'"))?;
    let mut result = AcuStoragePrices::default();

    let acu_filters = [
        ("databaseEngine", engine.pricing_name()),
        ("location", place),
        ("termType", "OnDemand"),
    ];
    for raw in api.pricing_products(&acu_filters)? {
        let item = parse_price_item(&raw)?;
        let usage = attribute(&item, "usagetype");
        if usage.contains("ServerlessV2Usage") && !usage.contains("IOOptimized") {
            for dimension in on_demand_dimensions(&item) {
                let price = usd_price(dimension)?;
                if price > 0.0 {
                    result.acu_standard = Some(price);
                    result.acu_io_optimized = Some(round_to(price * IO_OPT_COMPUTE_MULTIPLIER, 4));
                }
            }
        }
    }

    let storage_filters = [
        ("databaseEngine", engine.pricing_name()),
        ("location", place),
        ("termType", "OnDemand"),
        ("productFamily", "Database Storage"),
    ];
    for raw in api.pricing_products(&storage_filters)? {
        let item = parse_price_item(&raw)?;
        let usage = attribute(&item, "usagetype");
        for dimension in on_demand_dimensions(&item) {
            let price = usd_price(dimension)?;
            if price <= 0.0 {
                continue;
            }
            if usage.contains("IOOptimized") {
                result.storage_io_optimized = Some(price);
            } else if usage.contains("Aurora:StorageUsage") {
                result.storage_standard = Some(price);
            }
        }
    }
    Ok(result)
}

/// Instance classes the account can order, with EC2 CPU and memory
/// (`(name, vcpus, memory_gib)`; classes missing either are dropped).
fn fetch_instance_catalog(
    api: &dyn AwsApi,
    engine: Engine,
    region: &str,
) -> ApiResult<Vec<(String, i64, f64)>> {
    ensure_available(api)?;
    let mut names: Vec<String> = Vec::new();
    if let Ok(classes) = api.orderable_instance_classes(region, engine.rds_engine()) {
        for name in classes {
            if name.starts_with("db.") && name != "db.serverless" && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut specs: BTreeMap<String, (i64, f64)> =
        names.iter().map(|name| (name.clone(), (0, 0.0))).collect();
    let record = |specs: &mut BTreeMap<String, (i64, f64)>, types: Vec<super::api::Ec2Type>| {
        for instance in types {
            if let Some(slot) = specs.get_mut(&format!("db.{}", instance.name)) {
                *slot = (instance.vcpus, instance.memory_mib / 1024.0);
            }
        }
    };
    let ec2_names: Vec<String> = names
        .iter()
        .map(|name| name.replacen("db.", "", 1))
        .collect();
    for batch in ec2_names.chunks(100) {
        match api.ec2_instance_types(region, batch) {
            Ok(types) => record(&mut specs, types),
            Err(_) => {
                // Retry one by one for families EC2 does not know about.
                for single in batch {
                    if let Ok(types) = api.ec2_instance_types(region, std::slice::from_ref(single))
                    {
                        record(&mut specs, types);
                    }
                }
            }
        }
    }
    Ok(names
        .into_iter()
        .filter_map(|name| {
            let (vcpus, memory) = specs.get(&name).copied()?;
            (vcpus > 0 && memory > 0.0).then_some((name, vcpus, memory))
        })
        .collect())
}

/// Refreshes prices and the catalog from the transport; any gap keeps the
/// static default and is reported in `pricing_source`.
pub(crate) fn refresh_pricing(api: &dyn AwsApi, engine: Engine, region: &str) -> AcuPricing {
    let mut state = AcuPricing::static_defaults();
    let mut errors: Vec<String> = Vec::new();
    let mut live_instances = 0usize;
    let mut live_prices = 0usize;
    let mut live_acu = false;

    let catalog = match fetch_instance_catalog(api, engine, region) {
        Ok(catalog) => {
            live_instances = catalog.len();
            catalog
        }
        Err(error) => {
            errors.push(format!("Instance catalog: {error}"));
            Vec::new()
        }
    };

    let mut instance_prices: BTreeMap<String, f64> = BTreeMap::new();
    match fetch_instance_pricing(api, engine, region) {
        Ok(prices) => {
            live_prices = prices.len();
            instance_prices = prices;
        }
        Err(error) => {
            errors.push(format!("Instance pricing (API): {error}"));
            // Fallback: the public bulk pricing CSV needs no credentials.
            match fetch_instance_pricing_bulk(api, engine, region) {
                Ok(prices) => {
                    live_prices = prices.len();
                    instance_prices = prices;
                    if live_prices > 0 {
                        if let Some(last) = errors.last_mut() {
                            last.push_str(" [recovered via bulk pricing CSV]");
                        }
                    }
                }
                Err(second) => errors.push(format!("Instance pricing (bulk CSV): {second}")),
            }
        }
    }

    match fetch_acu_and_storage_pricing(api, engine, region) {
        Ok(found) => {
            if let Some(price) = found.acu_standard {
                state.acu_standard = price;
                state.acu_io_optimized = found
                    .acu_io_optimized
                    .unwrap_or_else(|| round_to(price * IO_OPT_COMPUTE_MULTIPLIER, 4));
                live_acu = true;
            }
            if let Some(price) = found.storage_standard {
                state.storage_standard = price;
            }
            if let Some(price) = found.storage_io_optimized {
                state.storage_io_opt = price;
            }
        }
        Err(error) => errors.push(format!("ACU/storage pricing: {error}")),
    }

    // Merge: static, then the API catalog, then live prices.
    let statics = static_specs();
    let mut merged = statics.clone();
    for (name, vcpus, memory) in &catalog {
        let mut price = instance_prices.get(name).copied().unwrap_or(0.0);
        if price == 0.0 {
            if let Some(known) = statics.get(name) {
                price = known.price;
            }
        }
        merged.insert(
            name.clone(),
            InstanceSpec {
                vcpus: *vcpus,
                memory_gib: Num::Float(*memory),
                price,
            },
        );
    }
    for (name, known) in &statics {
        if !catalog.iter().any(|(entry, ..)| entry == name) {
            if let Some(price) = instance_prices.get(name) {
                merged.insert(
                    name.clone(),
                    InstanceSpec {
                        price: *price,
                        ..*known
                    },
                );
            }
        }
    }
    state.instances = merged;

    let (source, details) = if errors.is_empty() {
        (
            "live",
            format!(
                "Live AWS APIs ({region}): {live_instances} instance types, \
                 {live_prices} prices, ACU=${}/hr",
                py_float_repr(state.acu_standard)
            ),
        )
    } else if live_prices > 0 || live_acu {
        (
            "partial_live",
            format!(
                "Partial live data ({region}): {live_instances} instances, \
                 {live_prices} prices. Gaps filled from static defaults. \
                 Errors: {}",
                errors.join("; ")
            ),
        )
    } else {
        state = AcuPricing::static_defaults();
        (
            "static_fallback",
            format!(
                "Static fallback (us-east-1, {STATIC_LAST_UPDATED}). \
                 Live fetch failed: {}",
                errors.join("; ")
            ),
        )
    };

    let mut members = vec![
        ("source", Json::string(source)),
        ("region", Json::string(region)),
        (
            "last_updated",
            Json::string(if source == "static_fallback" {
                STATIC_LAST_UPDATED
            } else {
                "now"
            }),
        ),
        ("details", Json::string(details)),
        (
            "instance_count",
            Json::Int(i64::try_from(state.instances.len()).unwrap_or(i64::MAX)),
        ),
        ("acu_price_standard", Json::Float(state.acu_standard)),
        (
            "storage_price_standard",
            Json::Float(state.storage_standard),
        ),
    ];
    if !errors.is_empty() {
        members.push((
            "errors",
            Json::Array(
                errors
                    .iter()
                    .map(|error| Json::string(error.clone()))
                    .collect(),
            ),
        ));
    }
    state.source = Json::object(members);
    state
}

// ---------------------------------------------------------------------------
// Core calculations
// ---------------------------------------------------------------------------

fn conversion_error(value: f64) -> ToolkitError {
    let what = if value.is_nan() { "NaN" } else { "infinity" };
    ToolkitError::runtime(
        "number_out_of_range",
        format!("cannot convert float {what} to integer"),
    )
}

/// `math.ceil(value * 2) / 2`: the next half ACU. Python raises for a value
/// that overflows or is NaN; that is a runtime error here.
pub(crate) fn round_up_to_half(value: f64) -> Result<f64, ToolkitError> {
    let doubled = value * 2.0;
    if !doubled.is_finite() {
        return Err(conversion_error(doubled));
    }
    let ceiled = doubled.ceil();
    // Python's int ceiling has no negative zero.
    Ok(if ceiled == 0.0 { 0.0 } else { ceiled / 2.0 })
}

/// ACU-to-CPU ratio of an instance family (`r`/`x` 4, `m`/`t` 2, `c` 1,
/// anything else 4).
pub(crate) fn family_ratio(instance_type: &str) -> i64 {
    let letter = instance_type
        .strip_prefix("db.")
        .and_then(|rest| rest.chars().next())
        .filter(char::is_ascii_lowercase);
    match letter {
        Some('m' | 't') => 2,
        Some('c') => 1,
        _ => 4,
    }
}

fn instance_spec(pricing: &AcuPricing, instance_type: &str) -> Result<InstanceSpec, ToolkitError> {
    pricing
        .instances
        .get(instance_type)
        .copied()
        .ok_or_else(|| {
            let supported: Vec<&str> = pricing.instances.keys().map(String::as_str).collect();
            ToolkitError::runtime(
                "unknown_instance_type",
                format!(
                    "Unknown instance type: {instance_type}. Supported: {}",
                    supported.join(", ")
                ),
            )
        })
}

/// Result of [`estimate_acu`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AcuEstimate {
    pub typical: Num,
    pub peak: f64,
    pub avg: f64,
    /// Fields of the Python result dict that `run_estimate` does not print.
    #[allow(dead_code)]
    pub raw_typical: f64,
    #[allow(dead_code)]
    pub raw_peak: f64,
    #[allow(dead_code)]
    pub family_ratio: i64,
    pub exceeds_capacity: bool,
}

/// Estimates the ACU a workload needs.
pub(crate) fn estimate_acu(
    cpu_p95: f64,
    cpu_max: f64,
    vcpus: i64,
    instance_type: &str,
    cpu_avg: f64,
) -> Result<AcuEstimate, ToolkitError> {
    let ratio = family_ratio(instance_type);
    let vcpu_count = vcpus as f64;
    let ratio_value = ratio as f64;

    // Typical ACU: weighted 95/5 blend of P95 and maximum CPU.
    let weighted_cpu = (cpu_p95 * 0.95 + cpu_max * 0.05) / 100.0;
    let raw_typical = weighted_cpu * vcpu_count * ratio_value;
    let typical = py_max(
        Num::Float(ACU_MIN),
        py_min(
            Num::Float(round_up_to_half(raw_typical)?),
            Num::Int(ACU_MAX),
        ),
    );

    let raw_peak = (cpu_max / 100.0) * vcpu_count * ratio_value;
    let peak = round_up_to_half(raw_peak)?;

    let avg = if cpu_avg > 0.0 {
        round_up_to_half((cpu_avg / 100.0) * vcpu_count * ratio_value)?
    } else {
        // Without an average, assume 60% of P95.
        round_up_to_half((cpu_p95 * 0.6 / 100.0) * vcpu_count * ratio_value)?
    };

    let ceiling = ACU_MAX as f64;
    Ok(AcuEstimate {
        typical,
        peak,
        avg,
        raw_typical: round_to(raw_typical, 2),
        raw_peak: round_to(raw_peak, 2),
        family_ratio: ratio,
        exceeds_capacity: raw_typical > ceiling || raw_peak > ceiling,
    })
}

/// Result of [`recommend_min_max`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MinMax {
    pub recommended_min: Num,
    pub recommended_max: Num,
    pub connection_floor_acu: f64,
    pub memory_floor_acu: Num,
    pub memory_advisory: Option<String>,
    pub exceeds_capacity: bool,
}

/// Recommends minimum and maximum ACU settings.
pub(crate) fn recommend_min_max(
    estimate: &AcuEstimate,
    connections: i64,
    working_set_gib: Num,
) -> Result<MinMax, ToolkitError> {
    // About 10 MB per connection. The product is exact in 128 bits, so a
    // huge connection count cannot overflow.
    let conn_mem_gib = (i128::from(connections) * 10) as f64 / 1024.0;
    let conn_acu = round_up_to_half(conn_mem_gib / GIB_PER_ACU)?;

    let working_set = working_set_gib.value();
    let mem_acu = if working_set > 0.0 {
        Num::Float(round_up_to_half(working_set / GIB_PER_ACU)?)
    } else {
        Num::Int(0)
    };

    // Min: average CPU plus the connection floor, uncapped first so a
    // baseline above the serverless limit can be detected.
    let raw_min = py_max(
        py_max(Num::Float(ACU_MIN), Num::Float(estimate.avg)),
        Num::Float(conn_acu),
    );

    // Max: peak plus 30% headroom, at least 1.5x typical, capped at the limit.
    let by_peak = round_up_to_half(estimate.peak * 1.3)?;
    let by_typical = round_up_to_half(estimate.typical.value() * 1.5)?;
    let recommended_max = py_min(
        Num::Float(py_max(Num::Float(by_peak), Num::Float(by_typical)).value()),
        Num::Int(ACU_MAX),
    );

    let ceiling = ACU_MAX as f64;
    let exceeds_capacity = raw_min.value() > ceiling || raw_min.value() > recommended_max.value();

    // Cap min at the limit and never above max.
    let recommended_min = py_min(py_min(raw_min, Num::Int(ACU_MAX)), recommended_max);

    let memory_advisory = (mem_acu.value() > recommended_min.value()).then(|| {
        format!(
            "Working set needs {} ACU ({} GiB / {} GiB per ACU). Your min ACU ({}) is below \
             this. Setting min to {} ACU keeps the working set cached and avoids \
             cold-cache I/O penalties on scale-up. Trade-off: higher baseline cost.",
            mem_acu.py_str(),
            fmt_f(working_set, 1),
            py_float_repr(GIB_PER_ACU),
            recommended_min.py_str(),
            mem_acu.py_str()
        )
    });

    Ok(MinMax {
        recommended_min,
        recommended_max,
        connection_floor_acu: conn_acu,
        memory_floor_acu: mem_acu,
        memory_advisory,
        exceeds_capacity,
    })
}

/// Inputs of [`calculate_costs`].
#[derive(Debug, Clone)]
pub(crate) struct CostInput<'a> {
    pub typical_acu: Num,
    pub min_acu: Num,
    pub max_acu: Num,
    pub storage_gib: Num,
    pub provisioned_instance: &'a str,
    pub num_instances: i64,
    pub exceeds_capacity: bool,
}

/// Compares serverless against provisioned monthly cost.
pub(crate) fn calculate_costs(
    pricing: &AcuPricing,
    input: &CostInput<'_>,
) -> Result<Json, ToolkitError> {
    let spec = instance_spec(pricing, input.provisioned_instance)?;
    let instances = input.num_instances as f64;
    let storage = input.storage_gib.value();

    let prov_compute = spec.price * HOURS_PER_MONTH * instances;
    let prov_storage = storage * pricing.storage_standard;
    let prov_total = prov_compute + prov_storage;

    let sv_compute = input.typical_acu.value() * pricing.acu_standard * HOURS_PER_MONTH;
    let sv_storage = storage * pricing.storage_standard;
    let sv_total = sv_compute + sv_storage;

    let sv_low = input.min_acu.value() * pricing.acu_standard * HOURS_PER_MONTH + sv_storage;
    let sv_high = input.max_acu.value() * pricing.acu_standard * HOURS_PER_MONTH + sv_storage;

    let savings = prov_total - sv_total;
    let savings_pct = if prov_total > 0.0 {
        Num::Float(savings / prov_total * 100.0)
    } else {
        Num::Int(0)
    };
    let pct = savings_pct.value();

    let (recommendation, reason) = if input.exceeds_capacity {
        (
            "not_recommended",
            format!(
                "Workload's baseline/peak demand exceeds the {} ACU serverless \
                 maximum. Stay with provisioned or split across multiple serverless clusters.",
                fmt_f(ACU_MAX as f64, 0)
            ),
        )
    } else if pct > 10.0 && sv_high <= prov_total * 2.0 {
        (
            "recommended",
            format!(
                "Serverless saves ${}/mo ({}%) vs provisioned. Cost range: ${}–${}/mo.",
                fmt_f(savings, 0),
                fmt_f(pct, 0),
                fmt_f(sv_low, 0),
                fmt_f(sv_high, 0)
            ),
        )
    } else if pct > 10.0 {
        (
            "consider",
            format!(
                "Typical cost is lower (${} vs ${}/mo), but peak cost could reach ${}/mo. \
                 Variable workloads benefit; sustained peaks may not.",
                fmt_f(sv_total, 0),
                fmt_f(prov_total, 0),
                fmt_f(sv_high, 0)
            ),
        )
    } else if pct > -5.0 {
        (
            "consider",
            format!(
                "Similar cost (${} vs ${}/mo). Choose serverless for auto-scaling and zero \
                 management overhead.",
                fmt_f(sv_total, 0),
                fmt_f(prov_total, 0)
            ),
        )
    } else if pct > -30.0 {
        (
            "more_expensive",
            format!(
                "Serverless costs ${}/mo more than provisioned (${} vs ${}/mo).",
                fmt_f(savings.abs(), 0),
                fmt_f(sv_total, 0),
                fmt_f(prov_total, 0)
            ),
        )
    } else {
        (
            "not_recommended",
            format!(
                "Serverless at ${}/mo is {}% more expensive than provisioned at ${}/mo. \
                 Sustained workloads are cheaper on provisioned instances.",
                fmt_f(sv_total, 0),
                fmt_f(pct.abs(), 0),
                fmt_f(prov_total, 0)
            ),
        )
    };

    let money = |value: f64| Json::Float(round_to(value, 2));
    Ok(Json::object(vec![
        (
            "provisioned",
            Json::object(vec![
                ("instance_type", Json::string(input.provisioned_instance)),
                ("num_instances", Json::Int(input.num_instances)),
                ("compute_monthly", money(prov_compute)),
                ("storage_monthly", money(prov_storage)),
                ("total_monthly", money(prov_total)),
            ]),
        ),
        (
            "serverless",
            Json::object(vec![
                ("typical_acu", input.typical_acu.to_json()),
                ("compute_monthly", money(sv_compute)),
                ("storage_monthly", money(sv_storage)),
                ("total_monthly", money(sv_total)),
                (
                    "cost_range",
                    Json::object(vec![
                        ("low", money(sv_low)),
                        ("typical", money(sv_total)),
                        ("high", money(sv_high)),
                    ]),
                ),
            ]),
        ),
        ("savings_monthly", money(savings)),
        ("savings_pct", round_num(savings_pct, 1).to_json()),
        ("recommendation", Json::string(recommendation)),
        ("reason", Json::string(reason)),
    ]))
}

/// The workload description of `acu estimate`.
#[derive(Debug, Clone)]
pub(crate) struct EstimateInput {
    pub instance: String,
    pub cpu_p95: f64,
    pub cpu_max: f64,
    pub cpu_avg: Num,
    pub connections: i64,
    pub working_set: Num,
    pub storage: Num,
    pub num_instances: i64,
}

/// Full estimation: ACU sizing, min/max and the cost comparison.
pub(crate) fn run_estimate(
    pricing: &AcuPricing,
    input: &EstimateInput,
) -> Result<Json, ToolkitError> {
    let spec = instance_spec(pricing, &input.instance)?;
    let estimate = estimate_acu(
        input.cpu_p95,
        input.cpu_max,
        spec.vcpus,
        &input.instance,
        input.cpu_avg.value(),
    )?;
    let min_max = recommend_min_max(&estimate, input.connections, input.working_set)?;
    // The workload overflows serverless if either signal trips.
    let exceeds_capacity = estimate.exceeds_capacity || min_max.exceeds_capacity;
    let costs = calculate_costs(
        pricing,
        &CostInput {
            typical_acu: estimate.typical,
            min_acu: min_max.recommended_min,
            max_acu: min_max.recommended_max,
            storage_gib: input.storage,
            provisioned_instance: &input.instance,
            num_instances: input.num_instances,
            exceeds_capacity,
        },
    )?;
    Ok(Json::object(vec![
        (
            "input",
            Json::object(vec![
                ("instance_type", Json::string(input.instance.clone())),
                ("vcpus", Json::Int(spec.vcpus)),
                ("memory_gib", spec.memory_gib.to_json()),
                ("cpu_p95", Json::Float(input.cpu_p95)),
                ("cpu_max", Json::Float(input.cpu_max)),
                ("cpu_avg", input.cpu_avg.to_json()),
                ("connections", Json::Int(input.connections)),
                ("working_set_gib", input.working_set.to_json()),
                ("storage_gib", input.storage.to_json()),
                ("num_instances", Json::Int(input.num_instances)),
            ]),
        ),
        (
            "acu_settings",
            Json::object(vec![
                ("recommended_min", min_max.recommended_min.to_json()),
                ("recommended_max", min_max.recommended_max.to_json()),
                ("typical_acu", estimate.typical.to_json()),
                ("peak_acu", Json::Float(estimate.peak)),
                ("avg_acu", Json::Float(estimate.avg)),
                (
                    "connection_floor_acu",
                    Json::Float(min_max.connection_floor_acu),
                ),
                ("memory_floor_acu", min_max.memory_floor_acu.to_json()),
                (
                    "memory_advisory",
                    min_max.memory_advisory.map_or(Json::Null, Json::String),
                ),
                ("exceeds_capacity", Json::Bool(exceeds_capacity)),
            ]),
        ),
        ("cost_comparison", costs),
        ("pricing_source", pricing.source.clone()),
    ]))
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn money_text(value: &Json, key: &str) -> String {
    let text = value
        .get(key)
        .and_then(Num::from_json)
        .map_or_else(|| "0".to_string(), Num::py_str);
    format!("${text}")
}

/// The text table of an estimate result.
pub(crate) fn format_table(result: &Json) -> String {
    let empty = Json::Object(Vec::new());
    let source = result.get("pricing_source").unwrap_or(&empty);
    let tag = title_case(&get_str(source, "source", "static_fallback").replace('_', " "));
    let mut lines: Vec<String> = vec![
        "=".repeat(65),
        "  Aurora Serverless v2 — ACU Estimate & Cost Comparison".to_string(),
        format!("  Pricing: {tag} ({})", get_str(source, "region", "?")),
        "=".repeat(65),
    ];

    let acu = result.get("acu_settings").unwrap_or(&empty);
    lines.push(String::new());
    lines.push("  ACU Configuration".to_string());
    lines.push(format!("  {}", "-".repeat(45)));
    lines.push(format!(
        "  Recommended Min ACU:  {}",
        fmt_f(get_f64(acu, "recommended_min", 0.0), 1)
    ));
    lines.push(format!(
        "  Recommended Max ACU:  {}",
        fmt_f(get_f64(acu, "recommended_max", 0.0), 1)
    ));
    lines.push(format!(
        "  Typical ACU:          {}",
        fmt_f(get_f64(acu, "typical_acu", 0.0), 1)
    ));
    lines.push(format!(
        "  Peak ACU:             {}",
        fmt_f(get_f64(acu, "peak_acu", 0.0), 1)
    ));
    let connection_floor = get_f64(acu, "connection_floor_acu", 0.0);
    if connection_floor > 0.0 {
        lines.push(format!(
            "  Connection floor:     {} ACU",
            fmt_f(connection_floor, 1)
        ));
    }
    let memory_floor = get_f64(acu, "memory_floor_acu", 0.0);
    if memory_floor > 0.0 {
        lines.push(format!(
            "  Memory floor:         {} ACU (advisory)",
            fmt_f(memory_floor, 1)
        ));
    }
    if let Some(note) = acu.get("memory_advisory").and_then(Json::as_str) {
        if !note.is_empty() {
            lines.push(format!("  NOTE: {note}"));
        }
    }

    let costs = result.get("cost_comparison").unwrap_or(&empty);
    let prov = costs.get("provisioned").unwrap_or(&empty);
    let serverless = costs.get("serverless").unwrap_or(&empty);
    lines.push(String::new());
    lines.push("  Monthly Cost Comparison".to_string());
    lines.push(format!("  {}", "-".repeat(45)));
    lines.push(format!(
        "  {:30} {:>14} {:>14}",
        "", "Provisioned", "Serverless"
    ));
    for (label, key) in [
        ("Compute", "compute_monthly"),
        ("Storage", "storage_monthly"),
        ("Total", "total_monthly"),
    ] {
        lines.push(format!(
            "  {label:30} {:>14} {:>14}",
            money_text(prov, key),
            money_text(serverless, key)
        ));
    }
    lines.push(String::new());
    let range = serverless.get("cost_range").unwrap_or(&empty);
    lines.push(format!(
        "  Serverless cost range: ${} – ${}/mo",
        fmt_f(get_f64(range, "low", 0.0), 0),
        fmt_f(get_f64(range, "high", 0.0), 0)
    ));
    lines.push(format!(
        "  Savings: ${}/mo ({}%)",
        fmt_f(get_f64(costs, "savings_monthly", 0.0), 0),
        fmt_f(get_f64(costs, "savings_pct", 0.0), 0)
    ));

    lines.push(String::new());
    lines.push(format!(
        "  Recommendation: {}",
        get_str(costs, "recommendation", "").to_uppercase()
    ));
    lines.push(format!("  {}", get_str(costs, "reason", "")));
    lines.push(String::new());
    lines.push("=".repeat(65));
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Pricing for a command: the static defaults with `--offline`, otherwise a
/// refresh with a one-line status on stderr (except for `pricing-source`).
fn load_pricing(
    invocation: &Invocation,
    output: &mut Output<'_>,
    api: &dyn AwsApi,
    engine: Engine,
    quiet: bool,
) -> AcuPricing {
    if invocation.has_flag("--offline") {
        return AcuPricing::static_defaults();
    }
    let region = region_value(invocation);
    let pricing = refresh_pricing(api, engine, &region);
    if !quiet {
        let tag = match get_str(&pricing.source, "source", "") {
            "live" => "LIVE",
            "partial_live" => "PARTIAL",
            _ => "STATIC",
        };
        output.diagnostic(&format!(
            "[Pricing: {tag} — {region}, {} instances, ACU=${}/hr]",
            pricing.instances.len(),
            py_float_repr(pricing.acu_standard)
        ));
    }
    pricing
}

/// `toolkit aurora acu estimate`.
pub(crate) fn estimate_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    api: &dyn AwsApi,
) -> Result<(), ToolkitError> {
    let engine = if invocation.value("--engine").is_some() {
        Some(Engine::from_invocation(invocation)?)
    } else {
        None
    };
    let cpu_p95 = float_value(invocation, "--cpu-p95")?;
    let cpu_max = float_value(invocation, "--cpu-max")?;
    let cpu_avg = float_value(invocation, "--cpu-avg")?;
    let connections = int_value(invocation, "--connections")?;
    let working_set = float_value(invocation, "--working-set")?;
    let storage = float_value(invocation, "--storage")?;
    let num_instances = int_value(invocation, "--num-instances")?;
    let format = format_value(invocation)?;

    let mut missing = Vec::new();
    if engine.is_none() {
        missing.push("--engine");
    }
    if invocation.value("--instance").is_none() {
        missing.push("--instance");
    }
    if cpu_p95.is_none() {
        missing.push("--cpu-p95");
    }
    if cpu_max.is_none() {
        missing.push("--cpu-max");
    }
    required(&missing)?;
    let (Some(engine), Some(instance), Some(cpu_p95), Some(cpu_max)) =
        (engine, instance_value(invocation), cpu_p95, cpu_max)
    else {
        return Ok(());
    };

    let pricing = load_pricing(invocation, output, api, engine, false);
    let input = EstimateInput {
        instance: instance.to_string(),
        cpu_p95,
        cpu_max,
        cpu_avg: cpu_avg.map_or(Num::Int(0), Num::Float),
        connections: connections.unwrap_or(0),
        working_set: working_set.map_or(Num::Int(0), Num::Float),
        storage: storage.map_or(Num::Int(10), Num::Float),
        num_instances: num_instances.unwrap_or(1),
    };
    let result = run_estimate(&pricing, &input)?;
    match format {
        Format::Table => output.line(&format_table(&result)),
        Format::Json => output.line(&dumps(&result)),
    }
}

/// `toolkit aurora acu list-instances`.
pub(crate) fn list_instances_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    api: &dyn AwsApi,
) -> Result<(), ToolkitError> {
    let engine = Engine::from_invocation(invocation)?;
    let pricing = load_pricing(invocation, output, api, engine, false);
    output.line(&format!(
        "Pricing source: {} ({})",
        get_str(&pricing.source, "source", "?"),
        get_str(&pricing.source, "region", "?")
    ))?;
    output.line(&format!(
        "{:<25} {:>6} {:>8} {:>8} {:>10}",
        "Instance Type", "vCPUs", "Memory", "$/hr", "$/mo"
    ))?;
    output.line(&"-".repeat(62))?;
    for (name, spec) in &pricing.instances {
        output.line(&format!(
            "{name:<25} {:>6} {:>6} GiB {:>8} {:>10}",
            spec.vcpus,
            fmt_f(spec.memory_gib.value(), 0),
            fmt_f(spec.price, 3),
            fmt_f(spec.price * HOURS_PER_MONTH, 2)
        ))?;
    }
    output.line(&format!(
        "\nTotal: {} instance types",
        pricing.instances.len()
    ))
}

/// `toolkit aurora acu pricing-source`.
pub(crate) fn pricing_source_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    api: &dyn AwsApi,
) -> Result<(), ToolkitError> {
    let engine = Engine::from_invocation(invocation)?;
    let pricing = load_pricing(invocation, output, api, engine, true);
    output.line(&dumps(&pricing.source))
}
