//! `toolkit rds commitment` (port of `rds-oss/scripts/rds_commitment_pricing_analyzer.py`).
//!
//! Read-only Reserved Instance and Database Savings Plan estimator for RDS
//! `MySQL`, `MariaDB` and `PostgreSQL`. No purchase API exists in the [`client`]
//! seam. Rates come from an [`AwsRead`] client: `--recorded FILE` replays
//! recorded responses, otherwise [`NoLiveClient`] sends nothing and the
//! analysis falls back to the bundled on-demand table, exactly as the script
//! behaved without `boto3`.
//!
//! The script swallowed every failure of the rate fetches and returned no
//! offerings; that is preserved (a connection failure yields the on-demand
//! recommendation) and the arithmetic, rounding and JSON are the script's.

use toolkit_aws::{collect_pages, next_token_from, AwsError, Page, PageLimits};
use toolkit_core::Json;

use toolkit_aws::recorded;

use super::members::{response_reason, text_member};
use crate::aurora::api::json_float;
use crate::safe_text::redacted_value;
use crate::aurora::pyfmt::{round_num, round_to, Num};

pub(crate) mod client;

use client::{AwsRead, Service};

/// Hours in the billing month the script uses.
const HOURS_PER_MONTH: f64 = 730.0;
/// A reserved term longer than this many seconds is the 3-year term.
const THREE_YEAR_SECONDS: f64 = 94_000_000.0;
const MULTI_AZ_MULTIPLIER: f64 = 2.0;

/// (engine, `ProductDescription`) of the supported engines.
const ENGINE_PRODUCTS: &[(&str, &str)] = &[
    ("mysql", "MySQL"),
    ("mariadb", "MariaDB"),
    ("postgres", "PostgreSQL"),
];

/// us-east-1 on-demand USD/hour. Other regions differ (see the notes).
const STATIC_INSTANCE_PRICES: &[(&str, f64)] = &[
    ("db.t3.medium", 0.068),
    ("db.t3.large", 0.136),
    ("db.t4g.medium", 0.065),
    ("db.t4g.large", 0.129),
    ("db.m5.large", 0.171),
    ("db.m5.xlarge", 0.342),
    ("db.m5.2xlarge", 0.684),
    ("db.m6g.large", 0.154),
    ("db.m6g.xlarge", 0.308),
    ("db.m6g.2xlarge", 0.616),
    ("db.m7g.large", 0.162),
    ("db.m7g.xlarge", 0.324),
    ("db.m7g.2xlarge", 0.648),
    ("db.r5.large", 0.240),
    ("db.r5.xlarge", 0.480),
    ("db.r5.2xlarge", 0.960),
    ("db.r5.4xlarge", 1.920),
    ("db.r5.8xlarge", 3.840),
    ("db.r6g.large", 0.218),
    ("db.r6g.xlarge", 0.435),
    ("db.r6g.2xlarge", 0.870),
    ("db.r6g.4xlarge", 1.740),
    ("db.r6g.8xlarge", 3.480),
    ("db.r7g.large", 0.228),
    ("db.r7g.xlarge", 0.456),
    ("db.r7g.2xlarge", 0.912),
    ("db.r7g.4xlarge", 1.824),
    ("db.r7g.8xlarge", 3.648),
    ("db.r8g.large", 0.240),
    ("db.r8g.xlarge", 0.480),
    ("db.r8g.2xlarge", 0.960),
    ("db.r8g.4xlarge", 1.920),
    ("db.r8g.8xlarge", 3.840),
];

/// Families the Database Savings Plan covers, sorted as the script prints them.
const DSP_ELIGIBLE_FAMILIES: &[&str] = &[
    "c7g", "c7i", "m7g", "m7i", "r7g", "r7i", "r8g", "r8gd", "x8g",
];

/// Engines accepted by `offline --engine`.
pub(crate) const ENGINES: &[&str] = &["mysql", "mariadb", "postgres"];

fn product_description(engine: &str) -> &'static str {
    ENGINE_PRODUCTS
        .iter()
        .find(|(name, _)| *name == engine)
        .map_or("MySQL", |(_, product)| *product)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RiOffering {
    pub(crate) term_years: i64,
    pub(crate) payment_option: String,
    pub(crate) effective_hourly: f64,
    pub(crate) upfront_cost: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DspRate {
    pub(crate) term_years: i64,
    pub(crate) payment_option: String,
    pub(crate) rate_per_hour: f64,
}

/// `dsp_rates`: instance class to its rates, in first-seen order.
pub(crate) type DspRates = Vec<(String, Vec<DspRate>)>;

fn term_years_of(seconds: f64) -> i64 {
    if seconds > THREE_YEAR_SECONDS {
        3
    } else {
        1
    }
}

/// `re.match(r"db\.([a-z0-9]+)\.", instance_type)`, group 1, or `""`.
pub(crate) fn family_of(instance_type: &str) -> &str {
    let Some(rest) = instance_type.strip_prefix("db.") else {
        return "";
    };
    let end = rest
        .bytes()
        .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit()))
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

/// `re.search(r"InstanceUsage:db\.(\w+)\.(\w+)", usage)`: `db.<family>.<size>`.
fn instance_usage_class(usage: &str) -> Option<String> {
    const NEEDLE: &str = "InstanceUsage:db.";
    let mut from = 0;
    while let Some(found) = usage[from..].find(NEEDLE) {
        let start = from + found + NEEDLE.len();
        let rest = &usage[start..];
        let family_len: usize = rest
            .chars()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum();
        if family_len > 0 && rest[family_len..].starts_with('.') {
            let after = &rest[family_len + 1..];
            let size_len: usize = after
                .chars()
                .take_while(|c| is_word(*c))
                .map(char::len_utf8)
                .sum();
            if size_len > 0 {
                return Some(format!("db.{}.{}", &rest[..family_len], &after[..size_len]));
            }
        }
        from += found + 1;
    }
    None
}

/// The on-demand USD/hour of `instance_type` (0 when no price is bundled) and
/// the warning the script raised for a non-`us-east-1` region.
pub(crate) fn on_demand_price(
    instance_type: &str,
    region: &str,
    multi_az: bool,
) -> (f64, Option<String>) {
    let mut price = STATIC_INSTANCE_PRICES
        .iter()
        .find(|(name, _)| *name == instance_type)
        .map_or(0.0, |(_, price)| *price);
    let warning = (region != "us-east-1" && price > 0.0).then(|| {
        format!(
            "On-demand price for {instance_type} is based on us-east-1. Actual price in \
             {region} may differ."
        )
    });
    if multi_az {
        price *= MULTI_AZ_MULTIPLIER;
    }
    (price, warning)
}

type FetchResult<T> = Result<T, String>;

fn string_member<'j>(object: &'j Json, key: &str) -> FetchResult<&'j str> {
    text_member(object, key).map(Option::unwrap_or_default)
}

fn number_member(object: &Json, key: &str) -> FetchResult<f64> {
    recorded::optional_member(object, key, "a number", |member| {
        Num::from_json(member).map(Num::value)
    })
    .map(Option::unwrap_or_default)
    .map_err(|error| response_reason(&error, "a number"))
}

fn float_member(object: &Json, key: &str) -> FetchResult<f64> {
    match object.get(key) {
        None => Ok(0.0),
        Some(member) => json_float(member),
    }
}

fn items<'j>(response: &'j Json, key: &str) -> FetchResult<&'j [Json]> {
    recorded::optional_list(response, key).map_err(|error| response_reason(&error, "an array"))
}

fn parse_offering(offering: &Json, instance_type: &str) -> FetchResult<Option<RiOffering>> {
    if string_member(offering, "DBInstanceClass")? != instance_type {
        return Ok(None);
    }
    let term_years = term_years_of(number_member(offering, "Duration")?);
    let payment_option = string_member(offering, "OfferingType")?.to_string();
    let fixed = float_member(offering, "FixedPrice")?;
    let mut recurring = 0.0;
    for charge in items(offering, "RecurringCharges")? {
        recurring += float_member(charge, "RecurringChargeAmount")?;
    }
    #[allow(clippy::cast_precision_loss)]
    let term_hours = (term_years * 365 * 24) as f64;
    let effective = fixed / term_hours + recurring;
    Ok(Some(RiOffering {
        term_years,
        payment_option,
        effective_hourly: round_to(effective, 6),
        upfront_cost: round_to(fixed, 2),
    }))
}

fn aws_failure(error: &AwsError) -> String {
    error.to_string()
}

fn try_fetch_ri(
    client: &mut dyn AwsRead,
    instance_type: &str,
    engine: &str,
    region: &str,
    multi_az: bool,
) -> FetchResult<Vec<RiOffering>> {
    let product = product_description(engine);
    let pages = collect_pages(&PageLimits::default(), |token| {
        let mut request = vec![
            ("DBInstanceClass", Json::string(instance_type)),
            ("ProductDescription", Json::string(product)),
            ("MultiAZ", Json::Bool(multi_az)),
        ];
        if let Some(marker) = token {
            request.push(("Marker", Json::string(marker)));
        }
        let response = client.call(
            Service::Rds,
            region,
            "DescribeReservedDBInstancesOfferings",
            &Json::object(request),
        )?;
        let offerings = items(&response, "ReservedDBInstancesOfferings")
            .map_err(AwsError::invalid_response)?
            .to_vec();
        Ok(Page {
            items: offerings,
            next_token: next_token_from(&response, "Marker")?,
        })
    })
    .map_err(|error| aws_failure(&error))?;
    let mut results = Vec::new();
    for offering in &pages {
        if let Some(parsed) = parse_offering(offering, instance_type)? {
            results.push(parsed);
        }
    }
    // Deduplicate on (term, payment option, effective rate), keeping the first.
    let mut seen: Vec<(i64, String, u64)> = Vec::new();
    let mut deduped = Vec::new();
    for offering in results {
        let key = (
            offering.term_years,
            offering.payment_option.clone(),
            offering.effective_hourly.to_bits(),
        );
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        deduped.push(offering);
    }
    Ok(deduped)
}

/// The Reserved Instance offerings of one class. Every failure yields no
/// offerings, like the script's `except Exception: return []`.
pub(crate) fn fetch_ri_offerings(
    client: &mut dyn AwsRead,
    instance_type: &str,
    engine: &str,
    region: &str,
    multi_az: bool,
) -> Vec<RiOffering> {
    try_fetch_ri(client, instance_type, engine, region, multi_az).unwrap_or_default()
}

fn dsp_request(region: &str, product: &str, token: Option<&str>) -> Json {
    let mut request = vec![
        (
            "savingsPlanTypes",
            Json::Array(vec![Json::string("Database")]),
        ),
        ("products", Json::Array(vec![Json::string("RDS")])),
        ("serviceCodes", Json::Array(vec![Json::string("AmazonRDS")])),
        (
            "filters",
            Json::Array(vec![
                Json::object(vec![
                    ("name", Json::string("region")),
                    ("values", Json::Array(vec![Json::string(region)])),
                ]),
                Json::object(vec![
                    ("name", Json::string("productDescription")),
                    ("values", Json::Array(vec![Json::string(product)])),
                ]),
            ]),
        ),
        ("maxResults", Json::Int(1000)),
    ];
    if let Some(token) = token {
        request.push(("nextToken", Json::string(token)));
    }
    Json::object(request)
}

fn parse_rate(entry: &Json) -> FetchResult<Option<(String, DspRate)>> {
    let empty = Json::Object(Vec::new());
    let offering = recorded::optional_object(entry, "savingsPlanOffering")
        .map_err(|error| response_reason(&error, "an object"))?
        .unwrap_or(&empty);
    let term_years = term_years_of(number_member(offering, "durationSeconds")?);
    let payment_option = string_member(offering, "paymentOption")?.to_string();
    // `float(rate)` that fails skips the entry; so does a rate that is not positive.
    let rate = match entry.get("rate") {
        None => 0.0,
        Some(value) => match json_float(value) {
            Ok(rate) => rate,
            Err(_) => return Ok(None),
        },
    };
    if rate <= 0.0 {
        return Ok(None);
    }
    let usage = string_member(entry, "usageType")?;
    // Usage types may carry a region prefix (`USE2-InstanceUsage:db.r7g.2xlarge`),
    // so the pattern is searched anywhere in the string.
    let Some(class) = instance_usage_class(usage) else {
        return Ok(None);
    };
    Ok(Some((
        class,
        DspRate {
            term_years,
            payment_option,
            rate_per_hour: round_to(rate, 6),
        },
    )))
}

/// The Database Savings Plan rates by instance class. A failed fetch yields
/// no rates; a malformed entry ends the walk and keeps the rates read so far,
/// like the script's `except Exception: pass`.
pub(crate) fn fetch_dsp_rates(client: &mut dyn AwsRead, engine: &str, region: &str) -> DspRates {
    let product = product_description(engine);
    let fetched = collect_pages(&PageLimits::default(), |token| {
        let response = client.call(
            Service::SavingsPlans,
            "us-east-1",
            "DescribeSavingsPlansOfferingRates",
            &dsp_request(region, product, token),
        )?;
        Ok(Page {
            items: items(&response, "searchResults")
                .map_err(AwsError::invalid_response)?
                .to_vec(),
            next_token: next_token_from(&response, "nextToken")?,
        })
    });
    let Ok(entries) = fetched else {
        return Vec::new();
    };
    let mut result: DspRates = Vec::new();
    for entry in &entries {
        match parse_rate(entry) {
            Ok(None) => {}
            Ok(Some((class, rate))) => match result.iter_mut().find(|(name, _)| *name == class) {
                Some((_, rates)) => rates.push(rate),
                None => result.push((class, vec![rate])),
            },
            Err(_) => break,
        }
    }
    result
}

fn best_ri(offerings: &[RiOffering], term_years: i64) -> Option<&RiOffering> {
    offerings
        .iter()
        .filter(|offering| offering.term_years == term_years)
        .fold(None, |best: Option<&RiOffering>, candidate| match best {
            Some(current) if candidate.effective_hourly >= current.effective_hourly => {
                Some(current)
            }
            _ => Some(candidate),
        })
}

fn best_dsp(rates: &[DspRate], term_years: i64) -> Option<&DspRate> {
    rates
        .iter()
        .filter(|rate| rate.term_years == term_years)
        .fold(None, |best: Option<&DspRate>, candidate| match best {
            Some(current) if candidate.rate_per_hour >= current.rate_per_hour => Some(current),
            _ => Some(candidate),
        })
}

fn percent(savings: f64, baseline: f64) -> Num {
    if baseline > 0.0 {
        Num::Float(savings / baseline * 100.0)
    } else {
        Num::Int(0)
    }
}

fn money(value: f64, digits: usize) -> Json {
    Json::Float(round_to(value, digits))
}

fn share(savings: f64, baseline: f64) -> Json {
    round_num(percent(savings, baseline), 1).to_json()
}

/// A workload to price.
pub(crate) struct Workload<'a> {
    pub(crate) instance_type: &'a str,
    pub(crate) engine: &'a str,
    pub(crate) num_instances: i64,
    pub(crate) region: &'a str,
    pub(crate) multi_az: bool,
}

/// The comparison document and the warnings the script raised.
pub(crate) struct Comparison {
    pub(crate) document: Json,
    pub(crate) warnings: Vec<String>,
}

/// Prices a workload against on-demand, Reserved Instances and Database
/// Savings Plans. `dsp_rates` of `None` fetches them with `client`.
#[allow(clippy::too_many_lines)]
pub(crate) fn build_comparison(
    client: &mut dyn AwsRead,
    workload: &Workload<'_>,
    dsp_rates: Option<DspRates>,
) -> Comparison {
    let Workload {
        instance_type,
        engine,
        num_instances,
        region,
        multi_az,
    } = *workload;
    let dsp_rates = dsp_rates.unwrap_or_else(|| fetch_dsp_rates(client, engine, region));

    let family = family_of(instance_type);
    let (od_hourly, warning) = on_demand_price(instance_type, region, multi_az);
    #[allow(clippy::cast_precision_loss)]
    let instances = num_instances as f64;
    let od_monthly = od_hourly * HOURS_PER_MONTH * instances;

    let offerings = fetch_ri_offerings(client, instance_type, engine, region, multi_az);
    let ri_1yr = best_ri(&offerings, 1);
    let ri_3yr = best_ri(&offerings, 3);
    let ri_monthly =
        |ri: Option<&RiOffering>| ri.map(|ri| ri.effective_hourly * HOURS_PER_MONTH * instances);
    let ri_1yr_monthly = ri_monthly(ri_1yr);
    let ri_3yr_monthly = ri_monthly(ri_3yr);

    let class_rates: &[DspRate] = dsp_rates
        .iter()
        .find(|(name, _)| name == instance_type)
        .map_or(&[], |(_, rates)| rates.as_slice());
    let dsp_1yr = best_dsp(class_rates, 1);
    let dsp_3yr = best_dsp(class_rates, 3);
    // Multi-AZ consumes twice the compute hours the savings plan must cover.
    let az_multiplier = if multi_az { MULTI_AZ_MULTIPLIER } else { 1.0 };
    let dsp_monthly = |dsp: Option<&DspRate>| {
        dsp.map(|dsp| dsp.rate_per_hour * HOURS_PER_MONTH * instances * az_multiplier)
    };
    let dsp_1yr_monthly = dsp_monthly(dsp_1yr);
    let dsp_3yr_monthly = dsp_monthly(dsp_3yr);

    let dsp_eligible = DSP_ELIGIBLE_FAMILIES.contains(&family);
    let mut notes: Vec<String> = Vec::new();
    if od_hourly == 0.0 {
        notes.push(format!(
            "No static on-demand price is bundled for {instance_type}, so the offline \
             baseline is $0 and savings cannot be computed. Run against a live instance \
             (no 'offline' subcommand) or supply pricing to get accurate figures."
        ));
    }
    if !dsp_eligible {
        notes.push(format!(
            "Database Savings Plans do not cover the {family} family. Eligible families: {}.",
            DSP_ELIGIBLE_FAMILIES.join(", ")
        ));
    }
    if multi_az {
        notes.push(
            "Multi-AZ pricing applied. Multi-AZ RIs are separate offerings from Single-AZ. \
             Ensure you purchase the correct deployment type."
                .to_string(),
        );
    }
    if region != "us-east-1" && od_hourly > 0.0 {
        notes.push(format!(
            "On-demand baseline for {instance_type} uses us-east-1 static pricing; actual \
             {region} pricing may differ by 10-20%, so savings percentages are approximate. \
             Provide live pricing or run in us-east-1 for exact figures."
        ));
    }

    let format_ri = |ri: Option<&RiOffering>, monthly: Option<f64>| -> Json {
        let (Some(ri), Some(monthly)) = (ri, monthly) else {
            return Json::Null;
        };
        let savings = od_monthly - monthly;
        Json::object(vec![
            ("term_years", Json::Int(ri.term_years)),
            ("payment_option", Json::string(ri.payment_option.clone())),
            (
                "effective_hourly_per_instance",
                money(ri.effective_hourly, 4),
            ),
            ("upfront_total", money(ri.upfront_cost * instances, 2)),
            ("monthly", money(monthly, 2)),
            ("savings_monthly", money(savings, 2)),
            ("savings_pct", share(savings, od_monthly)),
        ])
    };
    let format_dsp = |dsp: Option<&DspRate>, monthly: Option<f64>| -> Json {
        let (Some(dsp), Some(monthly)) = (dsp, monthly) else {
            return Json::Null;
        };
        let savings = od_monthly - monthly;
        Json::object(vec![
            ("term_years", Json::Int(dsp.term_years)),
            ("payment_option", Json::string(dsp.payment_option.clone())),
            ("rate_per_hour", money(dsp.rate_per_hour, 4)),
            ("monthly", money(monthly, 2)),
            ("savings_monthly", money(savings, 2)),
            ("savings_pct", share(savings, od_monthly)),
        ])
    };

    let mut options: Vec<(&str, f64)> = Vec::new();
    for (label, monthly) in [
        ("1yr RI", ri_1yr_monthly),
        ("3yr RI", ri_3yr_monthly),
        ("1yr DSP", dsp_1yr_monthly),
        ("3yr DSP", dsp_3yr_monthly),
    ] {
        if let Some(monthly) = monthly {
            options.push((label, monthly));
        }
    }
    let best = options
        .iter()
        .fold(None, |best: Option<&(&str, f64)>, candidate| match best {
            Some(current) if candidate.1 >= current.1 => Some(current),
            _ => Some(candidate),
        });
    let recommendation = match best {
        Some((label, cost)) => {
            let savings = od_monthly - cost;
            Json::object(vec![
                ("best_option", Json::string(*label)),
                ("best_monthly_cost", money(*cost, 2)),
                ("savings_vs_on_demand", money(savings, 2)),
                ("savings_pct", share(savings, od_monthly)),
            ])
        }
        None => Json::object(vec![
            ("best_option", Json::string("on_demand")),
            ("reason", Json::string("No RI or DSP offerings found.")),
        ]),
    };

    let document = Json::object(vec![
        ("engine", Json::string(engine)),
        ("instance_type", Json::string(instance_type)),
        ("num_instances", Json::Int(num_instances)),
        ("multi_az", Json::Bool(multi_az)),
        (
            "on_demand",
            Json::object(vec![
                ("hourly", money(od_hourly, 4)),
                ("monthly", money(od_monthly, 2)),
            ]),
        ),
        ("ri_1yr", format_ri(ri_1yr, ri_1yr_monthly)),
        ("ri_3yr", format_ri(ri_3yr, ri_3yr_monthly)),
        ("dsp_1yr", format_dsp(dsp_1yr, dsp_1yr_monthly)),
        ("dsp_3yr", format_dsp(dsp_3yr, dsp_3yr_monthly)),
        ("recommendation", recommendation),
        (
            "notes",
            Json::Array(notes.into_iter().map(Json::String).collect()),
        ),
    ]);
    Comparison {
        document,
        warnings: warning.into_iter().collect(),
    }
}

/// `str(e)` of a failed API call, redacted.
fn call_failure_text(operation: &str, error: &AwsError) -> String {
    let text = match error {
        AwsError::Service(service) => format!(
            "An error occurred ({}) when calling the {operation} operation: {}",
            service.code().unwrap_or("Unknown"),
            service.message()
        ),
        other => other.to_string(),
    };
    toolkit_aws::redact_text(&text)
}

// recorded-shape: appends to the output document this command builds, not a response.
fn set_member(document: &mut Json, key: &str, value: Json) {
    if let Json::Object(members) = document {
        members.push((key.to_string(), value));
    }
}

fn error_document(instance_id: &str, reason: String) -> Json {
    Json::object(vec![
        // Caller text: redacted where the document is built (#4941, #4981).
        ("instance_id", Json::string(redacted_value(instance_id))),
        ("error", Json::String(reason)),
    ])
}

/// The facts the live analysis reads from `describe_db_instances`.
struct Described {
    engine: String,
    instance_type: String,
    multi_az: bool,
    replicas: usize,
    engine_version: String,
}

fn describe(instance: &Json) -> FetchResult<Described> {
    // recorded-shape: each required `DBInstances` entry must be an object; any other shape fails the fetch.
    if !matches!(instance, Json::Object(_)) {
        return Err(format!(
            "\"DBInstances\" entries must be objects (it is {})",
            instance.type_name()
        ));
    }
    let multi_az =
        recorded::optional_member(instance, "MultiAZ", "a boolean", |member| match member {
            Json::Bool(flag) => Some(*flag),
            _ => None,
        })
        .map_err(|error| response_reason(&error, "a boolean"))?
        .unwrap_or(false);
    Ok(Described {
        engine: string_member(instance, "Engine")?.to_string(),
        instance_type: string_member(instance, "DBInstanceClass")?.to_string(),
        multi_az,
        replicas: items(instance, "ReadReplicaDBInstanceIdentifiers")?.len(),
        engine_version: string_member(instance, "EngineVersion")?.to_string(),
    })
}

/// Analyzes one live instance. A failed `describe_db_instances` is reported in
/// the document (`{"instance_id": ..., "error": ...}`), like the script.
pub(crate) fn analyze_instance_live(
    client: &mut dyn AwsRead,
    instance_id: &str,
    region: &str,
) -> Comparison {
    let request = Json::object(vec![("DBInstanceIdentifier", Json::string(instance_id))]);
    let response = match client.call(Service::Rds, region, "DescribeDBInstances", &request) {
        Ok(response) => response,
        Err(error) => {
            return Comparison {
                document: error_document(
                    instance_id,
                    call_failure_text("DescribeDBInstances", &error),
                ),
                warnings: Vec::new(),
            }
        }
    };
    let described = items(&response, "DBInstances").and_then(|instances| match instances.first() {
        None => Ok(None),
        Some(instance) => describe(instance).map(Some),
    });
    let described = match described {
        Ok(Some(described)) => described,
        Ok(None) => {
            return Comparison {
                document: error_document(instance_id, "instance not found".to_string()),
                warnings: Vec::new(),
            }
        }
        Err(reason) => {
            return Comparison {
                document: error_document(
                    instance_id,
                    format!("invalid DescribeDBInstances response: {reason}"),
                ),
                warnings: Vec::new(),
            }
        }
    };
    let mut comparison = build_comparison(
        client,
        &Workload {
            instance_type: &described.instance_type,
            engine: &described.engine,
            num_instances: 1,
            region,
            multi_az: described.multi_az,
        },
        None,
    );
    set_member(
        &mut comparison.document,
        "instance_id",
        Json::string(redacted_value(instance_id)),
    );
    set_member(
        &mut comparison.document,
        "engine_version",
        Json::String(described.engine_version),
    );
    if described.replicas > 0 {
        push_note(
            &mut comparison.document,
            format!(
                "Instance has {} read replica(s). Consider separate RI/DSP for each replica \
                 (Single-AZ pricing).",
                described.replicas
            ),
        );
    }
    comparison
}

/// Appends to the document's `notes` array.
// recorded-shape: appends to the output document this command builds, not a response.
fn push_note(document: &mut Json, note: String) {
    if let Json::Object(members) = document {
        if let Some((_, Json::Array(notes))) = members.iter_mut().find(|(key, _)| key == "notes") {
            notes.push(Json::String(note));
        }
    }
}
