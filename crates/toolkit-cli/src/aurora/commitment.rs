//! Reserved Instance and Database Savings Plan estimates
//! (`toolkit aurora commitment`). Read-only: no purchase API exists here.

use std::collections::BTreeMap;

use toolkit_core::{Json, ToolkitError};

use crate::{Invocation, Output};

use super::api::{
    attribute, aurora_cluster_ids, json_float, on_demand_dimensions, parse_price_item, usd_price,
    AwsApi,
};
use super::args::{
    float_value, format_value, int_value, region_value, reject_unrecognized, usage, Format,
};
use super::pyfmt::{dumps, fmt_f, fmt_grouped, get_f64, get_str, round_num, round_to, truthy, Num};
use super::{
    location, static_price, Engine, Tool, HOURS_PER_MONTH, IO_OPT_COMPUTE_MULTIPLIER,
    STATIC_ACU_PRICE_IO_OPTIMIZED, STATIC_ACU_PRICE_STANDARD,
};

/// Duration above which an offering counts as a 3-year term (seconds).
const THREE_YEAR_SECONDS: f64 = 94_000_000.0;

/// Families the Database Savings Plan covers.
const DSP_ELIGIBLE_FAMILIES: &[&str] = &[
    "r7g", "r7i", "r8g", "r8gd", "m7g", "m7i", "c7g", "c7i", "x8g",
];

/// Abbreviated Savings Plans sizes and their instance-size names.
const DSP_SIZE_MAP: &[(&str, &str)] = &[
    ("micro", "micro"),
    ("small", "small"),
    ("medium", "medium"),
    ("large", "large"),
    ("xl", "xlarge"),
    ("2xl", "2xlarge"),
    ("4xl", "4xlarge"),
    ("8xl", "8xlarge"),
    ("12xl", "12xlarge"),
    ("16xl", "16xlarge"),
    ("24xl", "24xlarge"),
    ("48xl", "48xlarge"),
];

/// One Reserved Instance offering.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RiOffering {
    pub term_years: i64,
    pub payment_option: String,
    /// `upfront / term_hours + recurring`.
    pub effective_hourly: f64,
    pub upfront_cost: f64,
}

/// One Database Savings Plan rate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DspRate {
    pub term_years: i64,
    pub payment_option: String,
    pub rate_per_hour: f64,
}

/// Savings Plan rates keyed by instance class or `ServerlessV2`.
pub(crate) type DspRates = BTreeMap<String, Vec<DspRate>>;

/// `^db\.([a-z0-9]+)\.` family of an instance class (`""` when it does not
/// match).
pub(crate) fn family_from_instance(instance_type: &str) -> &str {
    let Some(rest) = instance_type.strip_prefix("db.") else {
        return "";
    };
    let end = rest
        .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .unwrap_or(rest.len());
    if end > 0 && rest[end..].starts_with('.') {
        &rest[..end]
    } else {
        ""
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `^InstanceUsage:db\.(\w+)\.(\w+)` → (family, size).
fn parse_instance_usage(usage: &str) -> Option<(String, String)> {
    let rest = usage.strip_prefix("InstanceUsage:db.")?;
    let family_end = rest.find(|c: char| !is_word(c))?;
    if family_end == 0 || !rest[family_end..].starts_with('.') {
        return None;
    }
    let after = &rest[family_end + 1..];
    let size_end = after.find(|c: char| !is_word(c)).unwrap_or(after.len());
    if size_end == 0 {
        return None;
    }
    Some((
        rest[..family_end].to_string(),
        after[..size_end].to_string(),
    ))
}

fn term_years_from(seconds: f64) -> i64 {
    if seconds > THREE_YEAR_SECONDS {
        3
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Fetchers (payload parsing; the transport is `AwsApi`)
// ---------------------------------------------------------------------------

/// On-demand hourly price: the Pricing API when usable, else the static table
/// (`0.0` for an unknown class).
pub(crate) fn get_on_demand_price(
    api: &dyn AwsApi,
    engine: Engine,
    instance_type: &str,
    region: &str,
) -> f64 {
    let fallback = static_price(instance_type).unwrap_or(0.0);
    if !api.available() {
        return fallback;
    }
    let Some(place) = location(Tool::Commitment, region) else {
        return fallback;
    };
    let filters = [
        ("databaseEngine", engine.pricing_name()),
        ("location", place),
        ("instanceType", instance_type),
        ("deploymentOption", "Single-AZ"),
        ("termType", "OnDemand"),
    ];
    let Ok(items) = api.pricing_products(&filters) else {
        return fallback;
    };
    for raw in items {
        let Ok(item) = parse_price_item(&raw) else {
            return fallback;
        };
        if attribute(&item, "usagetype").contains("IOOptimized") {
            continue;
        }
        for dimension in on_demand_dimensions(&item) {
            if let Ok(price) = usd_price(dimension) {
                if price > 0.0 {
                    return price;
                }
            }
        }
    }
    fallback
}

/// One RDS reserved-offering record; `Err` when a field cannot be read the
/// way the Python did (that ends the offering list).
fn parse_offering(offering: &Json, instance_type: &str) -> Result<Option<RiOffering>, String> {
    let class = offering
        .get("DBInstanceClass")
        .and_then(Json::as_str)
        .unwrap_or("");
    if class != instance_type {
        return Ok(None);
    }
    let duration = match offering.get("Duration") {
        None => 0.0,
        Some(value) => Num::from_json(value)
            .map(Num::value)
            .ok_or_else(|| "'>' not supported for Duration".to_string())?,
    };
    let term_years = term_years_from(duration);
    let payment = offering
        .get("OfferingType")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let fixed = match offering.get("FixedPrice") {
        None => 0.0,
        Some(value) => json_float(value)?,
    };
    let mut recurring = 0.0;
    if let Some(Json::Array(charges)) = offering.get("RecurringCharges") {
        for charge in charges {
            recurring += match charge.get("RecurringChargeAmount") {
                None => 0.0,
                Some(value) => json_float(value)?,
            };
        }
    }
    let term_hours = (term_years * 365 * 24) as f64;
    let effective = fixed / term_hours + recurring;
    Ok(Some(RiOffering {
        term_years,
        payment_option: payment,
        effective_hourly: round_to(effective, 6),
        upfront_cost: round_to(fixed, 2),
    }))
}

/// Reserved Instance offerings for a class; empty when the SDK is missing or
/// the call fails.
pub(crate) fn fetch_ri_offerings(
    api: &dyn AwsApi,
    engine: Engine,
    instance_type: &str,
    region: &str,
) -> Vec<RiOffering> {
    if !api.available() {
        return Vec::new();
    }
    let Ok(records) = api.reserved_offerings(region, instance_type, engine.rds_engine()) else {
        return Vec::new();
    };
    let mut results: Vec<RiOffering> = Vec::new();
    for record in &records {
        match parse_offering(record, instance_type) {
            Ok(Some(offering)) => results.push(offering),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    // Deduplicate: the same offering can be listed more than once.
    let mut deduped: Vec<RiOffering> = Vec::new();
    for offering in results {
        let seen = deduped.iter().any(|kept| {
            kept.term_years == offering.term_years
                && kept.payment_option == offering.payment_option
                && kept.effective_hourly == offering.effective_hourly
        });
        if !seen {
            deduped.push(offering);
        }
    }
    deduped
}

/// Database Savings Plan rates for the region; empty without the SDK.
pub(crate) fn fetch_dsp_rates(api: &dyn AwsApi, engine: Engine, region: &str) -> DspRates {
    let mut result = DspRates::new();
    if !api.available() {
        return result;
    }
    let Ok(entries) = api.savings_plan_rates(region, engine.pricing_name()) else {
        return result;
    };
    for entry in &entries {
        let offering = entry.get("savingsPlanOffering");
        let duration = match offering.and_then(|value| value.get("durationSeconds")) {
            None => 0.0,
            Some(value) => match Num::from_json(value) {
                Some(number) => number.value(),
                None => break,
            },
        };
        let term_years = term_years_from(duration);
        let payment = offering
            .and_then(|value| value.get("paymentOption"))
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let rate = match entry.get("rate") {
            None => 0.0,
            Some(value) => match json_float(value) {
                Ok(rate) => rate,
                Err(_) => continue,
            },
        };
        if rate <= 0.0 {
            continue;
        }
        let usage = entry.get("usageType").and_then(Json::as_str).unwrap_or("");
        let unit = entry.get("unit").and_then(Json::as_str).unwrap_or("");
        // Standard pricing only, for consistency with the main comparison.
        if usage.contains("IOOptimized") {
            continue;
        }
        let key = if unit == "ACU-Hr" && usage.contains("ServerlessV2") {
            "ServerlessV2".to_string()
        } else {
            let Some((family, short_size)) = parse_instance_usage(usage) else {
                continue;
            };
            let size = DSP_SIZE_MAP
                .iter()
                .find(|(short, _)| *short == short_size)
                .map_or(short_size.as_str(), |(_, long)| *long);
            format!("db.{family}.{size}")
        };
        let existing = result.entry(key).or_default();
        if !existing
            .iter()
            .any(|kept| kept.term_years == term_years && kept.payment_option == payment)
        {
            existing.push(DspRate {
                term_years,
                payment_option: payment,
                rate_per_hour: round_to(rate, 6),
            });
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Best-of selection
// ---------------------------------------------------------------------------

/// Cheapest effective hourly rate for the term (the first one on a tie).
pub(crate) fn best_ri(offerings: &[RiOffering], term_years: i64) -> Option<&RiOffering> {
    let mut best: Option<&RiOffering> = None;
    for offering in offerings.iter().filter(|o| o.term_years == term_years) {
        if best.is_none_or(|current| offering.effective_hourly < current.effective_hourly) {
            best = Some(offering);
        }
    }
    best
}

/// Cheapest 1-year Savings Plan rate (the first one on a tie).
pub(crate) fn best_dsp(rates: &[DspRate]) -> Option<&DspRate> {
    let mut best: Option<&DspRate> = None;
    for rate in rates.iter().filter(|r| r.term_years == 1) {
        if best.is_none_or(|current| rate.rate_per_hour < current.rate_per_hour) {
            best = Some(rate);
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Comparison builder
// ---------------------------------------------------------------------------

/// What `build_comparison` prices.
#[derive(Debug, Clone)]
pub(crate) struct Workload<'a> {
    pub instance_type: &'a str,
    pub num_instances: i64,
    pub io_optimized: bool,
    pub serverless: bool,
    pub avg_acu: f64,
}

/// Savings of `monthly` against `od_monthly` and the percentage (an integer
/// `0` when there is no on-demand cost).
fn savings_of(od_monthly: f64, monthly: f64) -> (f64, Num) {
    let savings = od_monthly - monthly;
    let pct = if od_monthly > 0.0 {
        Num::Float(savings / od_monthly * 100.0)
    } else {
        Num::Int(0)
    };
    (savings, pct)
}

fn format_ri(
    offering: Option<&RiOffering>,
    monthly: Option<f64>,
    od_monthly: f64,
    instances: i64,
) -> Json {
    let (Some(ri), Some(monthly)) = (offering, monthly) else {
        return Json::Null;
    };
    let (savings, pct) = savings_of(od_monthly, monthly);
    Json::object(vec![
        ("term_years", Json::Int(ri.term_years)),
        ("payment_option", Json::string(ri.payment_option.clone())),
        (
            "effective_hourly_per_instance",
            Json::Float(round_to(ri.effective_hourly, 4)),
        ),
        (
            "upfront_total",
            Json::Float(round_to(ri.upfront_cost * instances as f64, 2)),
        ),
        ("monthly", Json::Float(round_to(monthly, 2))),
        ("savings_monthly", Json::Float(round_to(savings, 2))),
        ("savings_pct", round_num(pct, 1).to_json()),
    ])
}

fn format_dsp(dsp: Option<&DspRate>, units: f64, od_monthly: f64) -> Json {
    let Some(rate) = dsp else {
        return Json::Null;
    };
    let monthly = rate.rate_per_hour * HOURS_PER_MONTH * units;
    let (savings, pct) = savings_of(od_monthly, monthly);
    Json::object(vec![
        ("term_years", Json::Int(rate.term_years)),
        ("payment_option", Json::string(rate.payment_option.clone())),
        (
            "rate_per_hour",
            Json::Float(round_to(rate.rate_per_hour, 4)),
        ),
        ("monthly", Json::Float(round_to(monthly, 2))),
        ("savings_monthly", Json::Float(round_to(savings, 2))),
        ("savings_pct", round_num(pct, 1).to_json()),
    ])
}

fn recommend_provisioned(
    od: f64,
    ri_1yr: Option<f64>,
    ri_3yr: Option<f64>,
    dsp: Option<f64>,
    dsp_eligible: bool,
    io_optimized: bool,
) -> Json {
    let mut options: Vec<(&str, f64)> = Vec::new();
    if let Some(cost) = ri_1yr {
        options.push(("1yr RI", cost));
    }
    if let Some(cost) = ri_3yr {
        options.push(("3yr RI", cost));
    }
    if let Some(cost) = dsp {
        options.push(("1yr DSP", cost));
    }
    let Some(mut best) = options.first().copied() else {
        return Json::object(vec![
            ("best_option", Json::string("on_demand")),
            (
                "reason",
                Json::string("No RI or DSP offerings available for this instance type/region."),
            ),
        ]);
    };
    for option in &options[1..] {
        if option.1 < best.1 {
            best = *option;
        }
    }
    let (label, cost) = best;
    let (savings, pct) = savings_of(od, cost);

    let mut reasons = vec![format!(
        "{label} is the lowest-cost option, saving ${}/mo ({}%) vs on-demand.",
        fmt_f(savings, 0),
        fmt_f(pct.value(), 0)
    )];
    match label {
        "3yr RI" => reasons.push(
            "Best fit for steady 24/7 workloads you're confident will stay on this instance \
             family for 3 years."
                .to_string(),
        ),
        "1yr DSP" => {
            reasons.push(
                "Offers flexibility — covers any eligible Aurora instance family in the \
                 account, including future upgrades."
                    .to_string(),
            );
            if io_optimized {
                reasons.push(
                    "DSP is particularly attractive for I/O-Optimized clusters since it covers \
                     the full rate."
                        .to_string(),
                );
            }
        }
        _ => reasons.push(
            "Shorter commitment than 3yr, useful when instance-family migration is on the \
             horizon."
                .to_string(),
        ),
    }
    if !dsp_eligible && dsp.is_none() {
        reasons.push(
            "DSP is not available for this instance family, so RI is the only commitment option."
                .to_string(),
        );
    }
    Json::object(vec![
        ("best_option", Json::string(label)),
        ("best_monthly_cost", Json::Float(round_to(cost, 2))),
        ("savings_vs_on_demand", Json::Float(round_to(savings, 2))),
        ("savings_pct", round_num(pct, 1).to_json()),
        ("reason", Json::string(reasons.join(" "))),
    ])
}

fn recommend_serverless(dsp_savings: Option<f64>, od_monthly: f64) -> Json {
    let Some(savings) = dsp_savings else {
        return Json::object(vec![
            ("best_option", Json::string("on_demand")),
            (
                "reason",
                Json::string(
                    "No Database Savings Plan rates available for Serverless v2 ACU in this \
                     region.",
                ),
            ),
        ]);
    };
    if savings <= 0.0 {
        return Json::object(vec![
            ("best_option", Json::string("on_demand")),
            (
                "reason",
                Json::string("DSP would not save money at the specified average ACU."),
            ),
        ]);
    }
    let pct = if od_monthly > 0.0 {
        Num::Float(savings / od_monthly * 100.0)
    } else {
        Num::Int(0)
    };
    Json::object(vec![
        ("best_option", Json::string("1yr DSP")),
        ("savings_vs_on_demand", Json::Float(round_to(savings, 2))),
        ("savings_pct", round_num(pct, 1).to_json()),
        (
            "reason",
            Json::string(format!(
                "1yr DSP saves ${}/mo ({}%). Size the commitment to your steady baseline ACU — \
                 DSP bills the committed $/hr continuously, even during idle periods.",
                fmt_f(savings, 0),
                fmt_f(pct.value(), 0)
            )),
        ),
    ])
}

/// Compares on-demand, Reserved Instances and Database Savings Plans for one
/// workload. `dsp_rates` of `None` fetches them.
pub(crate) fn build_comparison(
    api: &dyn AwsApi,
    engine: Engine,
    region: &str,
    workload: &Workload<'_>,
    dsp_rates: Option<&DspRates>,
) -> Json {
    let fetched;
    let dsp_rates = if let Some(rates) = dsp_rates {
        rates
    } else {
        fetched = fetch_dsp_rates(api, engine, region);
        &fetched
    };

    if workload.serverless {
        let od_hourly = if workload.io_optimized {
            STATIC_ACU_PRICE_IO_OPTIMIZED
        } else {
            STATIC_ACU_PRICE_STANDARD
        };
        // Serverless v2 prices the average ACU continuously, not instances.
        let units = workload.avg_acu;
        let od_monthly = od_hourly * units * HOURS_PER_MONTH;
        let dsp_entry = dsp_rates
            .get("ServerlessV2")
            .and_then(|rates| best_dsp(rates));
        let dsp_monthly = dsp_entry.map(|rate| rate.rate_per_hour * units * HOURS_PER_MONTH);
        let dsp_savings = dsp_monthly.map(|monthly| od_monthly - monthly);
        return Json::object(vec![
            ("workload_type", Json::string("serverless_v2")),
            ("avg_acu", Json::Float(workload.avg_acu)),
            ("io_optimized", Json::Bool(workload.io_optimized)),
            (
                "on_demand",
                Json::object(vec![
                    ("hourly", Json::Float(od_hourly)),
                    ("monthly", Json::Float(round_to(od_monthly, 2))),
                ]),
            ),
            ("ri_1yr", Json::Null),
            ("ri_3yr", Json::Null),
            ("dsp_1yr", format_dsp(dsp_entry, units, od_monthly)),
            (
                "recommendation",
                recommend_serverless(dsp_savings, od_monthly),
            ),
            (
                "notes",
                Json::Array(vec![
                    Json::string("Reserved Instances do not apply to Aurora Serverless v2."),
                    Json::string(
                        "DSP covers ACU-hours but bills the committed $/hr continuously, \
                         even during auto-pause. Only commit to the steady baseline ACU.",
                    ),
                ]),
            ),
        ]);
    }

    let instance_type = workload.instance_type;
    let instances = workload.num_instances as f64;
    let family = family_from_instance(instance_type);
    let mut od_hourly = get_on_demand_price(api, engine, instance_type, region);
    if workload.io_optimized {
        od_hourly *= IO_OPT_COMPUTE_MULTIPLIER;
    }
    let od_monthly = od_hourly * HOURS_PER_MONTH * instances;

    let offerings = fetch_ri_offerings(api, engine, instance_type, region);
    let ri_1yr = best_ri(&offerings, 1);
    let ri_3yr = best_ri(&offerings, 3);

    // An I/O-Optimized instance is fully covered by Reserved Instances but
    // consumes 30% more normalized units per hour, so the effective RI cost is
    // the Standard-normalized rate times 1.30.
    let units = if workload.io_optimized {
        IO_OPT_COMPUTE_MULTIPLIER
    } else {
        1.0
    };
    let ri_monthly = |offering: Option<&RiOffering>| {
        offering.map(|ri| ri.effective_hourly * units * HOURS_PER_MONTH * instances)
    };
    let ri_1yr_monthly = ri_monthly(ri_1yr);
    let ri_3yr_monthly = ri_monthly(ri_3yr);

    let dsp_entry = dsp_rates
        .get(instance_type)
        .and_then(|rates| best_dsp(rates));
    let dsp_monthly = dsp_entry.map(|rate| rate.rate_per_hour * HOURS_PER_MONTH * instances);

    let dsp_eligible = DSP_ELIGIBLE_FAMILIES.contains(&family);
    let mut notes: Vec<Json> = Vec::new();
    if !dsp_eligible {
        notes.push(Json::string(format!(
            "Database Savings Plans do not cover the {family} family. \
             DSP requires r7g, r8g, r7i, or newer-gen Aurora-compatible families."
        )));
    }
    if workload.io_optimized {
        notes.push(Json::string(
            "Cluster is I/O-Optimized (30% compute premium). Both RI and DSP cover the \
             full I/O-Optimized instance price — I/O-Optimized consumes 30% more \
             normalized units per hour, so an RI draws down capacity at 1.30× (buy ~30% \
             more normalized RI units to fully cover the fleet); no portion is on-demand.",
        ));
    }

    Json::object(vec![
        ("workload_type", Json::string("provisioned")),
        ("instance_type", Json::string(instance_type)),
        ("num_instances", Json::Int(workload.num_instances)),
        ("io_optimized", Json::Bool(workload.io_optimized)),
        (
            "on_demand",
            Json::object(vec![
                ("hourly", Json::Float(round_to(od_hourly, 4))),
                ("monthly", Json::Float(round_to(od_monthly, 2))),
            ]),
        ),
        (
            "ri_1yr",
            format_ri(ri_1yr, ri_1yr_monthly, od_monthly, workload.num_instances),
        ),
        (
            "ri_3yr",
            format_ri(ri_3yr, ri_3yr_monthly, od_monthly, workload.num_instances),
        ),
        ("dsp_1yr", format_dsp(dsp_entry, instances, od_monthly)),
        (
            "recommendation",
            recommend_provisioned(
                od_monthly,
                ri_1yr_monthly,
                ri_3yr_monthly,
                dsp_monthly,
                dsp_eligible,
                workload.io_optimized,
            ),
        ),
        ("notes", Json::Array(notes)),
    ])
}

// ---------------------------------------------------------------------------
// Live cluster analysis
// ---------------------------------------------------------------------------

fn error_result(cluster_id: &str, message: &str) -> Json {
    Json::object(vec![
        ("cluster_id", Json::string(cluster_id)),
        ("error", Json::string(message)),
    ])
}

/// Prices every instance class of one cluster.
pub(crate) fn analyze_cluster_live(
    api: &dyn AwsApi,
    engine: Engine,
    cluster_id: &str,
    region: &str,
) -> Json {
    let cluster = match api.describe_cluster(region, cluster_id) {
        Err(message) => return error_result(cluster_id, &message),
        Ok(None) => return error_result(cluster_id, "cluster not found"),
        Ok(Some(cluster)) => cluster,
    };
    let io_optimized = cluster.storage_type == "aurora-iopt1";

    // Skip clusters with no instances: there is no compute to price.
    if cluster.members.is_empty() {
        return Json::object(vec![
            ("cluster_id", Json::string(cluster_id)),
            ("engine", Json::string(cluster.engine.clone())),
            (
                "engine_version",
                Json::string(cluster.engine_version.clone()),
            ),
            ("storage_type", Json::string(cluster.storage_type.clone())),
            ("skipped", Json::Bool(true)),
            ("reason", Json::string(engine.commitment_empty_reason())),
        ]);
    }

    let mut type_counts: Vec<(String, i64)> = Vec::new();
    let mut serverless_instances: i64 = 0;
    for member in &cluster.members {
        let Ok(classes) = api.instance_classes(region, member) else {
            continue;
        };
        for class in classes {
            if class == "db.serverless" {
                serverless_instances += 1;
            } else if let Some(slot) = type_counts.iter_mut().find(|(name, _)| *name == class) {
                slot.1 += 1;
            } else {
                type_counts.push((class, 1));
            }
        }
    }

    // Fetch Savings Plan rates once for the whole cluster.
    let dsp_rates = fetch_dsp_rates(api, engine, region);
    let mut workloads: Vec<Json> = Vec::new();
    for (instance_type, count) in &type_counts {
        workloads.push(build_comparison(
            api,
            engine,
            region,
            &Workload {
                instance_type,
                num_instances: *count,
                io_optimized,
                serverless: false,
                avg_acu: 0.0,
            },
            Some(&dsp_rates),
        ));
    }
    if serverless_instances > 0 {
        // Without observed ACU metrics serverless cannot be priced exactly.
        workloads.push(Json::object(vec![
            ("workload_type", Json::string("serverless_v2")),
            ("instance_count", Json::Int(serverless_instances)),
            (
                "note",
                Json::string(
                    "Serverless v2 instances detected. Re-run with 'offline --serverless \
                     --avg-acu <N>' using your observed average ACU (from CloudWatch \
                     ServerlessDatabaseCapacity metric) for a precise DSP estimate.",
                ),
            ),
            ("io_optimized", Json::Bool(io_optimized)),
        ]));
    }

    let mut mix: Vec<(String, Json)> = type_counts
        .iter()
        .map(|(name, count)| (name.clone(), Json::Int(*count)))
        .collect();
    if let Some(slot) = mix.iter_mut().find(|(name, _)| name == "serverless") {
        slot.1 = Json::Int(serverless_instances);
    } else {
        mix.push(("serverless".to_string(), Json::Int(serverless_instances)));
    }
    Json::object(vec![
        ("cluster_id", Json::string(cluster_id)),
        ("engine", Json::string(cluster.engine.clone())),
        ("storage_type", Json::string(cluster.storage_type.clone())),
        ("io_optimized", Json::Bool(io_optimized)),
        ("instance_mix", Json::Object(mix)),
        ("workloads", Json::Array(workloads)),
    ])
}

// ---------------------------------------------------------------------------
// Table output
// ---------------------------------------------------------------------------

/// `str(bool)` as Python prints it.
fn py_bool(value: Option<&Json>) -> &'static str {
    if truthy(value) {
        "True"
    } else {
        "False"
    }
}

/// The text table of one workload comparison.
pub(crate) fn format_table_single(result: &Json) -> String {
    let empty = Json::Object(Vec::new());
    let mut lines: Vec<String> = vec!["=".repeat(72)];
    if get_str(result, "workload_type", "") == "serverless_v2" {
        lines.push("Aurora Serverless v2 Commitment Pricing".to_string());
        lines.push(format!(
            "  Avg ACU: {}  I/O-Optimized: {}",
            fmt_f(get_f64(result, "avg_acu", 0.0), 1),
            py_bool(result.get("io_optimized"))
        ));
    } else {
        let count = result
            .get("num_instances")
            .and_then(Num::from_json)
            .map_or_else(|| "1".to_string(), Num::py_str);
        lines.push(format!(
            "Aurora Commitment Pricing — {count}× {}",
            get_str(result, "instance_type", "?")
        ));
        if truthy(result.get("io_optimized")) {
            lines.push("  Storage: I/O-Optimized (30% compute premium applied)".to_string());
        }
    }
    lines.push("=".repeat(72));
    let od_monthly = result
        .get("on_demand")
        .map_or(0.0, |od| get_f64(od, "monthly", 0.0));
    lines.push(String::new());
    lines.push(format!(
        "  {:<28} {:>12} {:>12} {:>12}  Term",
        "Option", "Monthly", "Savings", "Upfront"
    ));
    lines.push(format!("  {}", "-".repeat(70)));
    lines.push(format!(
        "  {:<28} ${:>11} {:>12} {:>12}  —",
        "On-Demand",
        fmt_grouped(od_monthly, 0),
        "—",
        "$0"
    ));
    for (key, label, term_hint) in [
        ("ri_1yr", "1yr RI", "1 year"),
        ("ri_3yr", "3yr RI", "3 years"),
        ("dsp_1yr", "1yr DSP", "1 year"),
    ] {
        let Some(entry) = result.get(key).filter(|entry| truthy(Some(*entry))) else {
            continue;
        };
        let payment = get_str(entry, "payment_option", "");
        let display = if payment.is_empty() {
            label.to_string()
        } else {
            format!("{label} ({payment})")
        };
        let monthly = get_f64(entry, "monthly", 0.0);
        let savings = get_f64(entry, "savings_monthly", 0.0);
        let pct = get_f64(entry, "savings_pct", 0.0);
        let upfront = get_f64(entry, "upfront_total", 0.0);
        let savings_text = if savings == 0.0 {
            "—".to_string()
        } else {
            format!("${} ({}%)", fmt_grouped(savings, 0), fmt_f(pct, 0))
        };
        let upfront_text = if upfront == 0.0 {
            "$0".to_string()
        } else {
            format!("${}", fmt_grouped(upfront, 0))
        };
        lines.push(format!(
            "  {display:<28} ${:>11} {savings_text:>12} {upfront_text:>12}  {term_hint}",
            fmt_grouped(monthly, 0)
        ));
    }
    let recommendation = result.get("recommendation").unwrap_or(&empty);
    lines.push(String::new());
    lines.push(format!(
        "  Recommendation: {}",
        get_str(recommendation, "best_option", "?")
    ));
    lines.push(format!("  {}", get_str(recommendation, "reason", "")));
    if let Some(Json::Array(notes)) = result.get("notes") {
        if !notes.is_empty() {
            lines.push(String::new());
            for note in notes {
                lines.push(format!("  Note: {}", note.as_str().unwrap_or("")));
            }
        }
    }
    lines.push("=".repeat(72));
    lines.join("\n")
}

/// The text of one analyzed cluster.
pub(crate) fn format_cluster(result: &Json) -> String {
    let mut lines = vec![format!(
        "Cluster: {}  ({})  storage_type={}",
        get_str(result, "cluster_id", "?"),
        get_str(result, "engine", "?"),
        get_str(result, "storage_type", "?")
    )];
    if let Some(Json::Array(workloads)) = result.get("workloads") {
        for workload in workloads {
            if get_str(workload, "workload_type", "") == "serverless_v2"
                && workload.get("note").is_some()
            {
                lines.push(String::new());
                let count = workload
                    .get("instance_count")
                    .and_then(Num::from_json)
                    .map_or_else(|| "0".to_string(), Num::py_str);
                lines.push(format!("  [Serverless v2 — {count} instance(s)]"));
                lines.push(format!("  {}", get_str(workload, "note", "")));
                continue;
            }
            lines.push(String::new());
            lines.push(format_table_single(workload));
        }
    }
    lines.join("\n")
}

/// The text of a whole-region analysis.
pub(crate) fn format_fleet(output: &Json) -> String {
    let empty_list: Vec<Json> = Vec::new();
    let clusters = match output.get("clusters") {
        Some(Json::Array(items)) => items,
        _ => &empty_list,
    };
    let mut lines = vec![
        format!("Region: {}", get_str(output, "region", "")),
        String::new(),
    ];
    let mut total_od = 0.0;
    let mut total_best = 0.0;
    for cluster in clusters {
        if cluster.get("error").is_some() {
            lines.push(format!(
                "{}: ERROR {}",
                get_str(cluster, "cluster_id", ""),
                get_str(cluster, "error", "")
            ));
            continue;
        }
        if let Some(Json::Array(workloads)) = cluster.get("workloads") {
            for workload in workloads {
                if get_str(workload, "workload_type", "") != "provisioned" {
                    continue;
                }
                let od = workload
                    .get("on_demand")
                    .map_or(0.0, |value| get_f64(value, "monthly", 0.0));
                let best = workload
                    .get("recommendation")
                    .map_or(od, |value| get_f64(value, "best_monthly_cost", od));
                total_od += od;
                total_best += best;
            }
        }
    }
    lines.push(format!(
        "  Fleet monthly on-demand:    ${}",
        fmt_grouped(total_od, 0)
    ));
    lines.push(format!(
        "  With best commitments:      ${}",
        fmt_grouped(total_best, 0)
    ));
    let savings = total_od - total_best;
    let pct = if total_od > 0.0 {
        savings / total_od * 100.0
    } else {
        0.0
    };
    lines.push(format!(
        "  Fleet savings opportunity:  ${}/mo ({}%)",
        fmt_grouped(savings, 0),
        fmt_f(pct, 0)
    ));
    lines.push(String::new());
    for cluster in clusters {
        if cluster.get("error").is_some() {
            continue;
        }
        lines.push(String::new());
        lines.push(format_cluster(cluster));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

const OFFLINE_ONLY_FLAGS: &[&str] = &[
    "--instance",
    "--num-instances",
    "--io-optimized",
    "--serverless",
    "--avg-acu",
];

/// `toolkit aurora commitment [offline]`.
pub(crate) fn command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    api: &dyn AwsApi,
) -> Result<(), ToolkitError> {
    let engine = Engine::from_invocation(invocation)?;
    let region = region_value(invocation);
    let format = format_value(invocation)?;
    let offline = match invocation.positional(0) {
        None => false,
        Some("offline") => true,
        Some(other) => {
            return Err(usage(format!(
                "argument mode: invalid choice: {} (choose from 'offline')",
                crate::manifests::py_repr_str(&crate::safe_text::shown_value(other))
            )))
        }
    };

    if offline {
        let num_instances = int_value(invocation, "--num-instances")?.unwrap_or(1);
        let avg_acu = float_value(invocation, "--avg-acu")?.unwrap_or(0.0);
        let io_optimized = invocation.has_flag("--io-optimized");
        let result = if invocation.has_flag("--serverless") {
            if avg_acu <= 0.0 {
                return Err(usage("--serverless requires --avg-acu > 0"));
            }
            build_comparison(
                api,
                engine,
                &region,
                &Workload {
                    instance_type: "",
                    num_instances: 0,
                    io_optimized,
                    serverless: true,
                    avg_acu,
                },
                None,
            )
        } else {
            let Some(instance) = invocation
                .value("--instance")
                .filter(|name| !name.is_empty())
            else {
                return Err(usage("offline mode requires --instance (or --serverless)"));
            };
            build_comparison(
                api,
                engine,
                &region,
                &Workload {
                    instance_type: instance,
                    num_instances,
                    io_optimized,
                    serverless: false,
                    avg_acu: 0.0,
                },
                None,
            )
        };
        return match format {
            Format::Json => output.line(&dumps(&result)),
            Format::Table => output.line(&format_table_single(&result)),
        };
    }

    reject_unrecognized(invocation, OFFLINE_ONLY_FLAGS)?;
    if !api.available() {
        return Err(usage(
            "AWS SDK access is not available in this build; use the 'offline' subcommand.",
        ));
    }
    let runtime = |message: String| ToolkitError::runtime("aws_error", message);
    if invocation.has_flag("--all") {
        let ids = aurora_cluster_ids(api, &region).map_err(runtime)?;
        let results: Vec<Json> = ids
            .iter()
            .map(|id| analyze_cluster_live(api, engine, id, &region))
            .collect();
        let result = Json::object(vec![
            ("region", Json::string(region.clone())),
            (
                "cluster_count",
                Json::Int(i64::try_from(results.len()).unwrap_or(i64::MAX)),
            ),
            ("clusters", Json::Array(results)),
        ]);
        return match format {
            Format::Json => output.line(&dumps(&result)),
            Format::Table => output.line(&format_fleet(&result)),
        };
    }
    if let Some(cluster) = invocation
        .value("--cluster")
        .filter(|name| !name.is_empty())
    {
        let result = analyze_cluster_live(api, engine, cluster, &region);
        return match format {
            Format::Json => output.line(&dumps(&result)),
            Format::Table => output.line(&format_cluster(&result)),
        };
    }
    output.line(crate::USAGE)
}
