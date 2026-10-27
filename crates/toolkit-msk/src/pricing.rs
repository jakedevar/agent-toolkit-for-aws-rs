//! Pricing sources for the sizing calculator.
//!
//! Two interchangeable pathways exist, selected with `--pricing-source`:
//!
//! - `api`: broker, storage and data-transfer prices come from the AWS Pricing
//!   API (`GetProducts`) through the [`PricingApi`] trait. The lookup logic
//!   ([`fetch_pricing`]) is a faithful port and is tested with injected
//!   clients; the shipped CLI links no AWS SDK (service SDKs stay optional),
//!   so its client always reports the pathway as unavailable.
//! - `static`: prices come from the embedded [`STATIC_BROKER_PRICE_PER_HR`]
//!   and [`STATIC_SHARED_PRICING`] tables for [`STATIC_PRICING_REGION`] only.
//!
//! `auto` tries `api` and falls back to `static` only when the API pathway as
//! a whole is unusable, never when one specific SKU is missing.

use std::collections::BTreeMap;
use std::fmt;

use toolkit_core::Json;

use crate::pyfmt::{format_g, py_str_repr};
use crate::sizing::{classify, BrokerClass, BrokerGroup, INSTANCE_SPECS};

/// Region the embedded static table covers.
pub const STATIC_PRICING_REGION: &str = "us-east-1";
/// Date the static table was captured.
pub const STATIC_PRICING_CAPTURED: &str = "2026-10-05";
/// Published one-way cross-AZ rate, used when the Pricing API lacks the SKU.
pub const FALLBACK_CROSS_AZ_COST_PER_GB_ONE_WAY: f64 = 0.01;

/// Selectable `--pricing-source` values.
pub const PRICING_SOURCES: &[&str] = &["auto", "api", "static"];

/// Embedded on-demand broker prices (USD/hr) for [`STATIC_PRICING_REGION`].
pub const STATIC_BROKER_PRICE_PER_HR: &[(&str, f64)] = &[
    ("kafka.m5.large", 0.21),
    ("kafka.m5.xlarge", 0.42),
    ("kafka.m5.2xlarge", 0.84),
    ("kafka.m5.4xlarge", 1.68),
    ("kafka.m5.8xlarge", 3.36),
    ("kafka.m5.12xlarge", 5.04),
    ("kafka.m5.16xlarge", 6.73),
    ("kafka.m5.24xlarge", 10.08),
    ("kafka.m7g.large", 0.204),
    ("kafka.m7g.xlarge", 0.408),
    ("kafka.m7g.2xlarge", 0.816),
    ("kafka.m7g.4xlarge", 1.632),
    ("kafka.m7g.8xlarge", 3.264),
    ("kafka.m7g.12xlarge", 4.896),
    ("kafka.m7g.16xlarge", 6.528),
    ("kafka.m7g.large (Express)", 0.408),
    ("kafka.m7g.xlarge (Express)", 0.816),
    ("kafka.m7g.2xlarge (Express)", 1.632),
    ("kafka.m7g.4xlarge (Express)", 3.264),
    ("kafka.m7g.8xlarge (Express)", 6.528),
    ("kafka.m7g.12xlarge (Express)", 9.792),
    ("kafka.m7g.16xlarge (Express)", 13.056),
];

/// Shared (non-per-instance) static dimensions. The cross-AZ rate is already
/// doubled because EC2 Regional data transfer bills both sides of an AZ
/// boundary.
pub struct SharedPricing {
    /// EBS (GP2) storage, USD per GiB-month.
    pub ebs_cost_per_gb_month: f64,
    /// Tiered Storage, USD per GiB-month.
    pub tiered_storage_cost_per_gb_month: f64,
    /// Express storage, USD per GiB-month.
    pub express_storage_cost_per_gb_month: f64,
    /// Express data-in, USD per GiB.
    pub express_data_in_per_gb: f64,
    /// Cross-AZ transfer, USD per GiB (both directions).
    pub cross_az_cost_per_gb: f64,
    /// Provisioned Storage Throughput, USD per MiB/s-month.
    pub pst_cost_per_mbs_month: f64,
}

/// The static shared table.
pub const STATIC_SHARED_PRICING: SharedPricing = SharedPricing {
    ebs_cost_per_gb_month: 0.10,
    tiered_storage_cost_per_gb_month: 0.06,
    express_storage_cost_per_gb_month: 0.10,
    express_data_in_per_gb: 0.01,
    cross_az_cost_per_gb: FALLBACK_CROSS_AZ_COST_PER_GB_ONE_WAY * 2.0,
    pst_cost_per_mbs_month: 0.08,
};

/// Pricing for one region from either pathway. Instance types with no price
/// are absent and excluded from sizing.
#[derive(Debug, Clone, PartialEq)]
pub struct Pricing {
    /// Region the prices are for.
    pub region: String,
    /// Broker USD/hr keyed by instance table name, in table order.
    pub broker_price_per_hr: Vec<(&'static str, f64)>,
    /// EBS (GP2) storage, USD per GiB-month.
    pub ebs_cost_per_gb_month: f64,
    /// Tiered Storage, USD per GiB-month.
    pub tiered_storage_cost_per_gb_month: f64,
    /// Express storage, USD per GiB-month.
    pub express_storage_cost_per_gb_month: f64,
    /// Express data-in, USD per GiB.
    pub express_data_in_per_gb: f64,
    /// Cross-AZ transfer, USD per GiB (both directions).
    pub cross_az_cost_per_gb: f64,
    /// Provisioned Storage Throughput, USD per MiB/s-month.
    pub pst_cost_per_mbs_month: f64,
    /// `"api"` or `"static"`.
    pub source: &'static str,
}

impl Pricing {
    /// Broker price (USD/hr) for an instance type, when one is known.
    #[must_use]
    pub fn broker_price_per_hr(&self, instance_type: &str) -> Option<f64> {
        self.broker_price_per_hr
            .iter()
            .find(|(name, _)| *name == instance_type)
            .map(|(_, price)| *price)
    }
}

/// A pricing-resolution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricingError {
    /// Any other pricing failure (bad source name, unsupported static region).
    General(String),
    /// The Pricing API pathway as a whole is unusable: missing or expired
    /// credentials, no `pricing:GetProducts` permission, throttling, an
    /// unreachable endpoint, or no SDK in this build. `auto` recovers from
    /// this by falling back to static pricing.
    Unavailable(String),
    /// The API works but one specific price could not be resolved (no
    /// products, or no `OnDemand` price dimension).
    Dimension(String),
}

impl PricingError {
    /// The failure message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::General(message) | Self::Unavailable(message) | Self::Dimension(message) => {
                message
            }
        }
    }
}

impl fmt::Display for PricingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PricingError {}

/// Client for the AWS Price List `GetProducts` operation.
///
/// Implementations return every product of every page, parsed from the
/// `PriceList` JSON strings. They map credential, permission, throttling and
/// connectivity failures to [`PricingError::Unavailable`] and everything else
/// to [`PricingError::Dimension`].
pub trait PricingApi {
    /// Runs `GetProducts` for `service_code` with `TERM_MATCH` filters.
    ///
    /// # Errors
    ///
    /// See the trait documentation.
    fn get_products(
        &self,
        service_code: &str,
        filters: &[(&str, &str)],
    ) -> Result<Vec<Json>, PricingError>;
}

/// The client linked into the shipped CLI: no AWS SDK is part of this build.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnlinkedPricingApi;

impl PricingApi for UnlinkedPricingApi {
    fn get_products(
        &self,
        _service_code: &str,
        _filters: &[(&str, &str)],
    ) -> Result<Vec<Json>, PricingError> {
        Err(PricingError::Unavailable(format!(
            "this toolkit build has no AWS Pricing API client, so live pricing cannot be \
             fetched. Re-run with --pricing-source static to use the embedded \
             {STATIC_PRICING_REGION} rates."
        )))
    }
}

fn selected_classes(broker_classes: &[BrokerGroup]) -> (bool, bool) {
    let classes: Vec<BrokerClass> = broker_classes
        .iter()
        .flat_map(|group| group.classes().iter().copied())
        .collect();
    let need_standard =
        classes.contains(&BrokerClass::M5Standard) || classes.contains(&BrokerClass::M7gStandard);
    let need_express = classes.contains(&BrokerClass::Express);
    (need_standard, need_express)
}

fn skipped_by_selection(instance_type: &str, need_standard: bool, need_express: bool) -> bool {
    match classify(instance_type) {
        Some(BrokerClass::Express) => !need_express,
        Some(BrokerClass::M5Standard | BrokerClass::M7gStandard) => !need_standard,
        None => false,
    }
}

/// Maps an instance table key to the Pricing API `computeFamily` attribute:
/// `kafka.m5.large` → `m5.large`, `kafka.m7g.large (Express)` →
/// `express.m7g.large`.
#[must_use]
pub fn compute_family(instance_type: &str) -> String {
    let suffix = " (Express)";
    if let Some(base) = instance_type.strip_suffix(suffix) {
        let base = base.strip_prefix("kafka.").unwrap_or(base);
        return format!("express.{base}");
    }
    instance_type
        .strip_prefix("kafka.")
        .unwrap_or(instance_type)
        .to_string()
}

/// Builds [`Pricing`] from the embedded tables, with no AWS calls.
///
/// Warnings (never produced for the shipped table) are appended to `warnings`.
///
/// # Errors
///
/// Refuses any region other than [`STATIC_PRICING_REGION`]: applying its rates
/// elsewhere would produce confidently wrong dollar figures.
pub fn static_pricing(
    region: &str,
    broker_classes: &[BrokerGroup],
    warnings: &mut Vec<String>,
) -> Result<Pricing, PricingError> {
    if region != STATIC_PRICING_REGION {
        return Err(PricingError::General(format!(
            "static pricing covers only {STATIC_PRICING_REGION}, but --region {region} \
             was requested. {region} rates differ, so reusing {STATIC_PRICING_REGION} \
             figures would be wrong. Re-run with --pricing-source api (requires AWS \
             credentials with pricing:GetProducts), or size against \
             --region {STATIC_PRICING_REGION}."
        )));
    }

    let (need_standard, need_express) = selected_classes(broker_classes);
    let mut broker_price_per_hr = Vec::new();
    for spec in INSTANCE_SPECS {
        if skipped_by_selection(spec.name, need_standard, need_express) {
            continue;
        }
        let price = STATIC_BROKER_PRICE_PER_HR
            .iter()
            .find(|(name, _)| *name == spec.name)
            .map(|(_, price)| *price);
        match price {
            Some(price) => broker_price_per_hr.push((spec.name, price)),
            None => warnings.push(format!(
                "no static pricing recorded for {}; excluding from sizing",
                spec.name
            )),
        }
    }

    // Zero out the dimensions the selected broker classes cannot incur,
    // mirroring fetch_pricing() so both pathways yield identical shapes.
    let shared = &STATIC_SHARED_PRICING;
    Ok(Pricing {
        region: region.to_string(),
        broker_price_per_hr,
        ebs_cost_per_gb_month: if need_standard {
            shared.ebs_cost_per_gb_month
        } else {
            0.0
        },
        tiered_storage_cost_per_gb_month: if need_standard {
            shared.tiered_storage_cost_per_gb_month
        } else {
            0.0
        },
        express_storage_cost_per_gb_month: if need_express {
            shared.express_storage_cost_per_gb_month
        } else {
            0.0
        },
        express_data_in_per_gb: if need_express {
            shared.express_data_in_per_gb
        } else {
            0.0
        },
        cross_az_cost_per_gb: shared.cross_az_cost_per_gb,
        pst_cost_per_mbs_month: if need_standard {
            shared.pst_cost_per_mbs_month
        } else {
            0.0
        },
        source: "static",
    })
}

fn object_values(value: &Json) -> &[(String, Json)] {
    match value {
        Json::Object(pairs) => pairs,
        _ => &[],
    }
}

/// First `OnDemand` price dimension's USD price of a Pricing API product.
fn price_per_unit_usd(product: &Json) -> Result<f64, PricingError> {
    let first_dimension = product
        .get("terms")
        .and_then(|terms| terms.get("OnDemand"))
        .into_iter()
        .flat_map(|on_demand| object_values(on_demand).iter())
        .filter_map(|(_, term)| term.get("priceDimensions"))
        .flat_map(|dimensions| object_values(dimensions).iter())
        .map(|(_, dimension)| dimension)
        .next();
    let Some(dimension) = first_dimension else {
        let sku = product
            .get("product")
            .and_then(|inner| inner.get("sku"))
            .and_then(Json::as_str)
            .unwrap_or("unknown");
        return Err(PricingError::Dimension(format!(
            "no OnDemand priceDimensions found in pricing product (sku={sku})"
        )));
    };
    match dimension
        .get("pricePerUnit")
        .and_then(|price| price.get("USD"))
    {
        Some(Json::String(text)) => text.trim().parse().map_err(|_| {
            PricingError::Dimension(format!(
                "unparseable USD price {} in pricing product",
                py_str_repr(text)
            ))
        }),
        Some(Json::Int(number)) => Ok(*number as f64),
        Some(Json::Float(number)) => Ok(*number),
        _ => Err(PricingError::Dimension(
            "pricing product has a price dimension without pricePerUnit.USD".to_string(),
        )),
    }
}

fn filters_repr(filters: &[(&str, &str)]) -> String {
    let inner: Vec<String> = filters
        .iter()
        .map(|(field, value)| format!("({}, {})", py_str_repr(field), py_str_repr(value)))
        .collect();
    format!("[{}]", inner.join(", "))
}

fn fetch_single_price(
    api: &dyn PricingApi,
    service_code: &str,
    filters: &[(&str, &str)],
    context: &str,
) -> Result<f64, PricingError> {
    let products = api.get_products(service_code, filters)?;
    let Some(first) = products.first() else {
        return Err(PricingError::Dimension(format!(
            "No pricing found for {context} (service={service_code}, filters={}). \
             Check that the region and dimension are correct/available.",
            filters_repr(filters)
        )));
    };
    price_per_unit_usd(first)
}

/// Fetches on-demand pricing for `region`, scoped to the selected broker
/// classes. Broker prices come from one `GetProducts` call per region and are
/// matched locally by `computeFamily`.
///
/// # Errors
///
/// Propagates [`PricingError::Unavailable`] from the client and returns
/// [`PricingError::Dimension`] when a required price is unpublished (except
/// cross-AZ transfer, which falls back to the published rate with a warning).
pub fn fetch_pricing(
    api: &dyn PricingApi,
    region: &str,
    broker_classes: &[BrokerGroup],
    warnings: &mut Vec<String>,
) -> Result<Pricing, PricingError> {
    let (need_standard, need_express) = selected_classes(broker_classes);

    let mut broker_price_per_hr = Vec::new();
    if need_standard || need_express {
        let products = api.get_products(
            "AmazonMSK",
            &[("regionCode", region), ("operation", "RunBroker")],
        )?;
        let mut price_by_compute_family: BTreeMap<String, f64> = BTreeMap::new();
        for product in &products {
            let attributes = product
                .get("product")
                .and_then(|inner| inner.get("attributes"));
            let Some(attributes) = attributes else {
                return Err(PricingError::Dimension(
                    "pricing product has no product.attributes".to_string(),
                ));
            };
            if let Some(family) = attributes
                .get("computeFamily")
                .and_then(Json::as_str)
                .filter(|family| !family.is_empty())
            {
                price_by_compute_family.insert(family.to_string(), price_per_unit_usd(product)?);
            }
        }
        for spec in INSTANCE_SPECS {
            if skipped_by_selection(spec.name, need_standard, need_express) {
                continue;
            }
            let family = compute_family(spec.name);
            match price_by_compute_family.get(&family) {
                Some(price) => broker_price_per_hr.push((spec.name, *price)),
                None => warnings.push(format!(
                    "no {region} pricing found for {} (computeFamily={family}); \
                     excluding from sizing",
                    spec.name
                )),
            }
        }
    }

    let (mut ebs_cost, mut tiered_cost, mut pst_cost) = (0.0, 0.0, 0.0);
    let (mut express_storage_cost, mut express_data_in) = (0.0, 0.0);
    if need_standard {
        ebs_cost = fetch_single_price(
            api,
            "AmazonMSK",
            &[
                ("regionCode", region),
                ("operation", "RunVolume"),
                ("storageFamily", "GP2"),
            ],
            "EBS (GP2) storage",
        )?;
        tiered_cost = fetch_single_price(
            api,
            "AmazonMSK",
            &[
                ("regionCode", region),
                ("operation", "TieredStorage"),
                ("storageFamily", "Tiered storage"),
            ],
            "Tiered Storage",
        )?;
        pst_cost = fetch_single_price(
            api,
            "AmazonMSK",
            &[
                ("regionCode", region),
                ("operation", "RunVolume"),
                ("storageFamily", "General"),
            ],
            "Provisioned Storage Throughput",
        )?;
    }
    if need_express {
        express_storage_cost = fetch_single_price(
            api,
            "AmazonMSK",
            &[
                ("regionCode", region),
                ("operation", "Storage"),
                ("storageFamily", "Express storage"),
            ],
            "Express storage",
        )?;
        express_data_in = fetch_single_price(
            api,
            "AmazonMSK",
            &[("regionCode", region), ("operation", "Data-In")],
            "Express data-in",
        )?;
    }

    // EC2 "Regional" data transfer is billed per GB on both the sending and
    // receiving side of an AZ boundary, so one GB crossing AZs costs twice the
    // published one-way rate.
    let cross_az_one_way = match fetch_single_price(
        api,
        "AmazonEC2",
        &[
            ("productFamily", "Data Transfer"),
            ("fromRegionCode", region),
            ("transferType", "IntraRegion"),
        ],
        "cross-AZ data transfer",
    ) {
        Ok(price) => price,
        // Only PricingError::Dimension is recovered: a credentials or
        // permissions failure must never be reported as the published rate.
        Err(PricingError::Dimension(_)) => {
            warnings.push(format!(
                "no {region} cross-AZ data-transfer pricing published via the Pricing API; \
                 falling back to the standard ${}/GB one-way rate documented at \
                 https://aws.amazon.com/ec2/pricing/on-demand/",
                format_g(FALLBACK_CROSS_AZ_COST_PER_GB_ONE_WAY)
            ));
            FALLBACK_CROSS_AZ_COST_PER_GB_ONE_WAY
        }
        Err(other) => return Err(other),
    };

    Ok(Pricing {
        region: region.to_string(),
        broker_price_per_hr,
        ebs_cost_per_gb_month: ebs_cost,
        tiered_storage_cost_per_gb_month: tiered_cost,
        express_storage_cost_per_gb_month: express_storage_cost,
        express_data_in_per_gb: express_data_in,
        cross_az_cost_per_gb: cross_az_one_way * 2.0,
        pst_cost_per_mbs_month: pst_cost,
        source: "api",
    })
}

/// Returns pricing for `region` using the requested pathway.
///
/// `api` and `static` force one pathway. `auto` prefers the live API and falls
/// back to the embedded table only when the API pathway is unusable.
///
/// # Errors
///
/// Returns a [`PricingError`] for an unknown source, an API failure, or an
/// unsupported static region.
pub fn resolve_pricing(
    api: &dyn PricingApi,
    region: &str,
    broker_classes: &[BrokerGroup],
    pricing_source: &str,
    warnings: &mut Vec<String>,
) -> Result<Pricing, PricingError> {
    match pricing_source {
        "static" => static_pricing(region, broker_classes, warnings),
        "api" => fetch_pricing(api, region, broker_classes, warnings),
        "auto" => match fetch_pricing(api, region, broker_classes, warnings) {
            Err(PricingError::Unavailable(message)) => {
                if region != STATIC_PRICING_REGION {
                    return Err(PricingError::General(format!(
                        "{message}\n\nFalling back to static pricing is not possible for \
                         --region {region}: the embedded table covers only \
                         {STATIC_PRICING_REGION}. Supply AWS credentials with \
                         pricing:GetProducts to price {region}, or size against \
                         --region {STATIC_PRICING_REGION} with --pricing-source static."
                    )));
                }
                warnings.push(format!(
                    "the AWS Pricing API is unavailable ({message}).\n\
                     WARNING: falling back to embedded {STATIC_PRICING_REGION} pricing captured \
                     on {STATIC_PRICING_CAPTURED}; rates may be stale. Re-run with \
                     --pricing-source api once credentials are available to confirm."
                ));
                static_pricing(region, broker_classes, warnings)
            }
            other => other,
        },
        unknown => Err(PricingError::General(format!(
            "unknown pricing source {}; choose from: {}",
            py_str_repr(unknown),
            PRICING_SOURCES.join(", ")
        ))),
    }
}

/// Renders `pricing` as the table-refresh literals printed by
/// `--emit-static-pricing`. `captured` is the capture date (`YYYY-MM-DD`).
#[must_use]
pub fn format_static_pricing_literals(pricing: &Pricing, captured: &str) -> String {
    let mut lines = vec![
        format!("STATIC_PRICING_REGION = \"{}\"", pricing.region),
        format!("STATIC_PRICING_CAPTURED = \"{captured}\""),
        String::new(),
        "STATIC_BROKER_PRICE_PER_HR = {".to_string(),
    ];
    for spec in INSTANCE_SPECS {
        if let Some(price) = pricing.broker_price_per_hr(spec.name) {
            lines.push(format!("    \"{}\": {},", spec.name, format_g(price)));
        }
    }
    lines.push("}".to_string());
    lines.push(String::new());
    lines.push("STATIC_SHARED_PRICING = {".to_string());
    for (key, value) in [
        ("ebs_cost_per_gb_month", pricing.ebs_cost_per_gb_month),
        (
            "tiered_storage_cost_per_gb_month",
            pricing.tiered_storage_cost_per_gb_month,
        ),
        (
            "express_storage_cost_per_gb_month",
            pricing.express_storage_cost_per_gb_month,
        ),
        ("express_data_in_per_gb", pricing.express_data_in_per_gb),
        ("cross_az_cost_per_gb", pricing.cross_az_cost_per_gb),
        ("pst_cost_per_mbs_month", pricing.pst_cost_per_mbs_month),
    ] {
        lines.push(format!("    \"{key}\": {},", format_g(value)));
    }
    lines.push("}".to_string());
    lines.join("\n")
}

/// Describes where the printed prices came from, for the report banner.
#[must_use]
pub fn pricing_provenance(source: &str) -> String {
    if source == "static" {
        format!(
            "from rates embedded in this script, captured on {STATIC_PRICING_CAPTURED} \
             (NOT fetched live, and possibly stale \u{2014} re-run with\n\
             --pricing-source api to confirm against current published pricing)"
        )
    } else {
        "fetched live via the AWS Pricing API".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use toolkit_core::parse_json;

    type Handler = Box<dyn Fn(&str, &[(&str, &str)]) -> Result<Vec<Json>, PricingError>>;

    struct FakeApi {
        calls: RefCell<Vec<String>>,
        handler: Handler,
    }

    impl FakeApi {
        fn new(handler: Handler) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                handler,
            }
        }
    }

    impl PricingApi for FakeApi {
        fn get_products(
            &self,
            service_code: &str,
            filters: &[(&str, &str)],
        ) -> Result<Vec<Json>, PricingError> {
            let rendered: Vec<String> = filters.iter().map(|(k, v)| format!("{k}={v}")).collect();
            self.calls
                .borrow_mut()
                .push(format!("{service_code}:{}", rendered.join(",")));
            (self.handler)(service_code, filters)
        }
    }

    fn product(sku: &str, family: Option<&str>, usd: &str) -> Json {
        let attributes = family.map_or_else(String::new, |family| {
            format!("\"computeFamily\": \"{family}\"")
        });
        parse_json(&format!(
            "{{\"product\": {{\"sku\": \"{sku}\", \"attributes\": {{{attributes}}}}}, \
             \"terms\": {{\"OnDemand\": {{\"T1\": {{\"priceDimensions\": \
             {{\"D1\": {{\"pricePerUnit\": {{\"USD\": \"{usd}\"}}}}}}}}}}}}}}"
        ))
        .expect("valid product JSON")
    }

    fn has(filters: &[(&str, &str)], field: &str, value: &str) -> bool {
        filters.iter().any(|(k, v)| *k == field && *v == value)
    }

    /// An API publishing every price except `m7g.xlarge` brokers.
    fn full_api() -> FakeApi {
        FakeApi::new(Box::new(|service, filters| {
            if service == "AmazonEC2" {
                return Ok(vec![product("EC2", None, "0.0125000000")]);
            }
            if has(filters, "operation", "RunBroker") {
                let mut products = Vec::new();
                for (spec, family) in INSTANCE_SPECS.iter().map(|s| (s, compute_family(s.name))) {
                    if family != "m7g.xlarge" {
                        let price = format!("{:.4}", 1.0 + spec.rec_partitions as f64 / 10_000.0);
                        products.push(product(&family, Some(&family), &price));
                    }
                }
                // A product without a computeFamily is ignored.
                products.push(product("noise", None, "99"));
                return Ok(products);
            }
            let usd = if has(filters, "storageFamily", "GP2") {
                "0.1100000000"
            } else if has(filters, "operation", "TieredStorage") {
                "0.0700000000"
            } else if has(filters, "storageFamily", "General") {
                "0.0900000000"
            } else if has(filters, "storageFamily", "Express storage") {
                "0.1200000000"
            } else if has(filters, "operation", "Data-In") {
                "0.0200000000"
            } else {
                return Err(PricingError::Dimension("unexpected".to_string()));
            };
            Ok(vec![product("S", None, usd)])
        }))
    }

    const BOTH: [BrokerGroup; 2] = [BrokerGroup::Standard, BrokerGroup::Express];

    #[test]
    fn static_pricing_covers_every_instance_for_both_classes() {
        let mut warnings = Vec::new();
        let pricing = static_pricing("us-east-1", &BOTH, &mut warnings).expect("static");
        assert!(warnings.is_empty());
        assert_eq!(pricing.source, "static");
        assert_eq!(pricing.region, "us-east-1");
        assert_eq!(pricing.broker_price_per_hr.len(), 22);
        assert_eq!(pricing.broker_price_per_hr("kafka.m5.large"), Some(0.21));
        assert_eq!(
            pricing.broker_price_per_hr("kafka.m7g.16xlarge (Express)"),
            Some(13.056)
        );
        assert_eq!(pricing.broker_price_per_hr("kafka.nope"), None);
        assert_eq!(pricing.ebs_cost_per_gb_month, 0.10);
        assert_eq!(pricing.tiered_storage_cost_per_gb_month, 0.06);
        assert_eq!(pricing.express_storage_cost_per_gb_month, 0.10);
        assert_eq!(pricing.express_data_in_per_gb, 0.01);
        assert_eq!(pricing.cross_az_cost_per_gb, 0.02);
        assert_eq!(pricing.pst_cost_per_mbs_month, 0.08);
        // Every instance of the table has a static price.
        assert_eq!(STATIC_BROKER_PRICE_PER_HR.len(), INSTANCE_SPECS.len());
        for spec in INSTANCE_SPECS {
            assert!(
                pricing.broker_price_per_hr(spec.name).is_some(),
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn static_pricing_zeroes_dimensions_of_unselected_classes() {
        let standard =
            static_pricing("us-east-1", &[BrokerGroup::Standard], &mut Vec::new()).expect("static");
        assert_eq!(standard.broker_price_per_hr.len(), 15);
        assert_eq!(standard.express_storage_cost_per_gb_month, 0.0);
        assert_eq!(standard.express_data_in_per_gb, 0.0);
        assert_eq!(standard.ebs_cost_per_gb_month, 0.10);
        assert_eq!(standard.cross_az_cost_per_gb, 0.02);

        let express =
            static_pricing("us-east-1", &[BrokerGroup::Express], &mut Vec::new()).expect("static");
        assert_eq!(express.broker_price_per_hr.len(), 7);
        assert_eq!(express.ebs_cost_per_gb_month, 0.0);
        assert_eq!(express.tiered_storage_cost_per_gb_month, 0.0);
        assert_eq!(express.pst_cost_per_mbs_month, 0.0);
        assert_eq!(express.express_storage_cost_per_gb_month, 0.10);
        assert_eq!(express.express_data_in_per_gb, 0.01);
    }

    #[test]
    fn static_pricing_refuses_other_regions() {
        let Err(PricingError::General(message)) =
            static_pricing("eu-west-1", &BOTH, &mut Vec::new())
        else {
            panic!("expected a general pricing error");
        };
        assert_eq!(
            message,
            "static pricing covers only us-east-1, but --region eu-west-1 was requested. \
             eu-west-1 rates differ, so reusing us-east-1 figures would be wrong. Re-run with \
             --pricing-source api (requires AWS credentials with pricing:GetProducts), or size \
             against --region us-east-1."
        );
    }

    #[test]
    fn compute_family_maps_instance_names() {
        assert_eq!(compute_family("kafka.m5.large"), "m5.large");
        assert_eq!(compute_family("kafka.m7g.16xlarge"), "m7g.16xlarge");
        assert_eq!(
            compute_family("kafka.m7g.large (Express)"),
            "express.m7g.large"
        );
        assert_eq!(compute_family("custom"), "custom");
    }

    #[test]
    fn api_pricing_matches_brokers_by_compute_family() {
        let api = full_api();
        let mut warnings = Vec::new();
        let pricing = fetch_pricing(&api, "us-east-1", &BOTH, &mut warnings).expect("api pricing");
        assert_eq!(pricing.source, "api");
        // m7g.xlarge is unpublished: excluded with a warning.
        assert_eq!(pricing.broker_price_per_hr.len(), 21);
        assert_eq!(pricing.broker_price_per_hr("kafka.m7g.xlarge"), None);
        assert_eq!(
            warnings,
            [
                "no us-east-1 pricing found for kafka.m7g.xlarge (computeFamily=m7g.xlarge); \
              excluding from sizing"
            ]
        );
        assert_eq!(pricing.broker_price_per_hr("kafka.m5.large"), Some(1.1));
        assert_eq!(
            pricing.broker_price_per_hr("kafka.m7g.large (Express)"),
            Some(1.1)
        );
        assert_eq!(pricing.ebs_cost_per_gb_month, 0.11);
        assert_eq!(pricing.tiered_storage_cost_per_gb_month, 0.07);
        assert_eq!(pricing.pst_cost_per_mbs_month, 0.09);
        assert_eq!(pricing.express_storage_cost_per_gb_month, 0.12);
        assert_eq!(pricing.express_data_in_per_gb, 0.02);
        // EC2 Regional transfer bills both sides of an AZ boundary: 2x one way.
        assert_eq!(pricing.cross_az_cost_per_gb, 0.025);
        // One RunBroker call per region, not one per instance type.
        let calls = api.calls.borrow();
        assert_eq!(calls.iter().filter(|c| c.contains("RunBroker")).count(), 1);
        assert_eq!(
            calls[0],
            "AmazonMSK:regionCode=us-east-1,operation=RunBroker"
        );
    }

    #[test]
    fn api_pricing_only_queries_dimensions_of_the_selected_classes() {
        let api = full_api();
        let pricing = fetch_pricing(&api, "us-east-1", &[BrokerGroup::Express], &mut Vec::new())
            .expect("api pricing");
        assert_eq!(pricing.broker_price_per_hr.len(), 7);
        assert_eq!(pricing.ebs_cost_per_gb_month, 0.0);
        let calls = api.calls.borrow();
        assert!(calls
            .iter()
            .all(|c| !c.contains("GP2") && !c.contains("TieredStorage")));
        assert!(calls.iter().any(|c| c.contains("Express storage")));
        assert!(calls.iter().any(|c| c.contains("AmazonEC2")));

        let api = full_api();
        fetch_pricing(&api, "us-east-1", &[BrokerGroup::Standard], &mut Vec::new())
            .expect("api pricing");
        assert!(api.calls.borrow().iter().all(|c| !c.contains("Data-In")));
    }

    #[test]
    fn missing_cross_az_sku_falls_back_to_the_published_rate_with_a_warning() {
        let api = FakeApi::new(Box::new(|service, filters| {
            if service == "AmazonEC2" {
                return Ok(Vec::new());
            }
            if has(filters, "operation", "RunBroker") {
                return Ok(Vec::new());
            }
            Ok(vec![product("S", None, "0.5")])
        }));
        let mut warnings = Vec::new();
        let pricing = fetch_pricing(&api, "ap-south-2", &BOTH, &mut warnings).expect("pricing");
        assert_eq!(pricing.cross_az_cost_per_gb, 0.02);
        assert!(pricing.broker_price_per_hr.is_empty());
        assert_eq!(
            warnings.len(),
            23,
            "22 missing brokers plus the cross-AZ note"
        );
        assert_eq!(
            warnings.last().map(String::as_str),
            Some(
                "no ap-south-2 cross-AZ data-transfer pricing published via the Pricing API; \
                 falling back to the standard $0.01/GB one-way rate documented at \
                 https://aws.amazon.com/ec2/pricing/on-demand/"
            )
        );
    }

    #[test]
    fn credential_failures_are_never_reported_as_the_published_rate() {
        let api = FakeApi::new(Box::new(|service, filters| {
            if service == "AmazonEC2" {
                return Err(PricingError::Unavailable("AccessDenied".to_string()));
            }
            if has(filters, "operation", "RunBroker") {
                return Ok(Vec::new());
            }
            Ok(vec![product("S", None, "0.5")])
        }));
        let result = fetch_pricing(&api, "us-east-1", &BOTH, &mut Vec::new());
        assert_eq!(
            result,
            Err(PricingError::Unavailable("AccessDenied".to_string()))
        );
    }

    #[test]
    fn an_unpublished_required_price_is_a_dimension_error() {
        let api = FakeApi::new(Box::new(|_, filters| {
            if has(filters, "storageFamily", "GP2") {
                return Ok(Vec::new());
            }
            Ok(vec![product("S", Some("m5.large"), "0.5")])
        }));
        let Err(PricingError::Dimension(message)) =
            fetch_pricing(&api, "us-east-1", &[BrokerGroup::Standard], &mut Vec::new())
        else {
            panic!("expected a dimension error");
        };
        assert_eq!(
            message,
            "No pricing found for EBS (GP2) storage (service=AmazonMSK, filters=[('regionCode', \
             'us-east-1'), ('operation', 'RunVolume'), ('storageFamily', 'GP2')]). \
             Check that the region and dimension are correct/available."
        );
    }

    #[test]
    fn malformed_products_are_dimension_errors() {
        let bare = parse_json(r#"{"product": {"sku": "S9"}}"#).expect("json");
        assert_eq!(
            price_per_unit_usd(&bare),
            Err(PricingError::Dimension(
                "no OnDemand priceDimensions found in pricing product (sku=S9)".to_string()
            ))
        );
        let anonymous = parse_json("{}").expect("json");
        assert_eq!(
            price_per_unit_usd(&anonymous),
            Err(PricingError::Dimension(
                "no OnDemand priceDimensions found in pricing product (sku=unknown)".to_string()
            ))
        );
        let no_usd = parse_json(
            r#"{"terms": {"OnDemand": {"T": {"priceDimensions": {"D": {"pricePerUnit": {"EUR": "1"}}}}}}}"#,
        )
        .expect("json");
        assert!(matches!(
            price_per_unit_usd(&no_usd),
            Err(PricingError::Dimension(_))
        ));
        let bad_number = parse_json(
            r#"{"terms": {"OnDemand": {"T": {"priceDimensions": {"D": {"pricePerUnit": {"USD": "free"}}}}}}}"#,
        )
        .expect("json");
        assert_eq!(
            price_per_unit_usd(&bad_number),
            Err(PricingError::Dimension(
                "unparseable USD price 'free' in pricing product".to_string()
            ))
        );
        // A first term without dimensions is skipped in favour of the next one.
        let second = parse_json(
            r#"{"terms": {"OnDemand": {"A": {}, "B": {"priceDimensions": {"D": {"pricePerUnit": {"USD": "2.5"}}}}}}}"#,
        )
        .expect("json");
        assert_eq!(price_per_unit_usd(&second), Ok(2.5));

        let api = FakeApi::new(Box::new(|_, _| {
            Ok(vec![parse_json(r#"{"terms": {}}"#).expect("json")])
        }));
        assert!(matches!(
            fetch_pricing(&api, "us-east-1", &BOTH, &mut Vec::new()),
            Err(PricingError::Dimension(_))
        ));
    }

    #[test]
    fn resolve_auto_falls_back_only_when_the_api_pathway_is_unusable() {
        // No client in this build: us-east-1 falls back with two warning lines.
        let mut warnings = Vec::new();
        let pricing = resolve_pricing(
            &UnlinkedPricingApi,
            "us-east-1",
            &BOTH,
            "auto",
            &mut warnings,
        )
        .expect("fallback");
        assert_eq!(pricing.source, "static");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with(
            "the AWS Pricing API is unavailable (this toolkit build has no AWS Pricing API client"
        ));
        assert!(warnings[0].contains(
            ").\nWARNING: falling back to embedded us-east-1 pricing captured on 2026-10-05; \
             rates may be stale. Re-run with --pricing-source api once credentials are available to confirm."
        ));

        // Another region cannot fall back.
        let Err(PricingError::General(message)) = resolve_pricing(
            &UnlinkedPricingApi,
            "eu-west-1",
            &BOTH,
            "auto",
            &mut Vec::new(),
        ) else {
            panic!("expected a general error");
        };
        assert!(message.contains(
            "\n\nFalling back to static pricing is not possible for --region eu-west-1: the \
             embedded table covers only us-east-1. Supply AWS credentials with \
             pricing:GetProducts to price eu-west-1, or size against --region us-east-1 with \
             --pricing-source static."
        ));

        // A missing SKU is a data problem the static table cannot fix: no fallback.
        let api = FakeApi::new(Box::new(|_, _| Ok(Vec::new())));
        let result = resolve_pricing(
            &api,
            "us-east-1",
            &[BrokerGroup::Standard],
            "auto",
            &mut Vec::new(),
        );
        assert!(matches!(result, Err(PricingError::Dimension(_))));
    }

    #[test]
    fn resolve_forces_one_pathway_and_rejects_unknown_sources() {
        let api = full_api();
        let pricing =
            resolve_pricing(&api, "us-east-1", &BOTH, "api", &mut Vec::new()).expect("api");
        assert_eq!(pricing.source, "api");
        let pricing =
            resolve_pricing(&api, "us-east-1", &BOTH, "auto", &mut Vec::new()).expect("auto");
        assert_eq!(pricing.source, "api", "auto prefers a working API");
        let before = api.calls.borrow().len();
        let pricing =
            resolve_pricing(&api, "us-east-1", &BOTH, "static", &mut Vec::new()).expect("static");
        assert_eq!(pricing.source, "static");
        assert_eq!(
            api.calls.borrow().len(),
            before,
            "static never calls the API"
        );

        assert!(matches!(
            resolve_pricing(
                &UnlinkedPricingApi,
                "us-east-1",
                &BOTH,
                "api",
                &mut Vec::new()
            ),
            Err(PricingError::Unavailable(_))
        ));
        assert_eq!(
            resolve_pricing(&api, "us-east-1", &BOTH, "bogus", &mut Vec::new()),
            Err(PricingError::General(
                "unknown pricing source 'bogus'; choose from: auto, api, static".to_string()
            ))
        );
    }

    #[test]
    fn static_pricing_literals_round_trip_the_embedded_tables() {
        let pricing = static_pricing("us-east-1", &BOTH, &mut Vec::new()).expect("static");
        let text = format_static_pricing_literals(&pricing, "2026-11-01");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "STATIC_PRICING_REGION = \"us-east-1\"");
        assert_eq!(lines[1], "STATIC_PRICING_CAPTURED = \"2026-11-01\"");
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "STATIC_BROKER_PRICE_PER_HR = {");
        assert_eq!(lines[4], "    \"kafka.m5.large\": 0.21,");
        assert_eq!(lines[25], "    \"kafka.m7g.16xlarge (Express)\": 13.056,");
        assert_eq!(&lines[26..29], ["}", "", "STATIC_SHARED_PRICING = {"]);
        assert_eq!(
            &lines[29..],
            [
                "    \"ebs_cost_per_gb_month\": 0.1,",
                "    \"tiered_storage_cost_per_gb_month\": 0.06,",
                "    \"express_storage_cost_per_gb_month\": 0.1,",
                "    \"express_data_in_per_gb\": 0.01,",
                "    \"cross_az_cost_per_gb\": 0.02,",
                "    \"pst_cost_per_mbs_month\": 0.08,",
                "}",
            ]
        );
    }

    #[test]
    fn provenance_text_names_the_source() {
        assert_eq!(
            pricing_provenance("api"),
            "fetched live via the AWS Pricing API"
        );
        assert_eq!(
            pricing_provenance("static"),
            "from rates embedded in this script, captured on 2026-10-05 (NOT fetched live, and \
             possibly stale \u{2014} re-run with\n--pricing-source api to confirm against \
             current published pricing)"
        );
        assert_eq!(PRICING_SOURCES, ["auto", "api", "static"]);
        assert_eq!(
            UnlinkedPricingApi
                .get_products("x", &[])
                .map_err(|e| e.message().len() > 10),
            Err(true)
        );
        assert_eq!(PricingError::General("m".into()).to_string(), "m");
    }
}
