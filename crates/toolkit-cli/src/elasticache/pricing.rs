//! ElastiCache price tables (port of `pricing.py`).
//!
//! One loader serves the calculator and the serverless estimator. The Python
//! fetched the public AWS Bulk Pricing index over HTTPS and cached it; this build
//! reads the same data from a local file instead:
//!
//! - [`PricingLoader::from_csv_text`]: the optional local pricing CSV.
//! - [`PricingLoader::from_bulk_pricing`]: the AWS bulk pricing JSON
//!   (`products` / `terms.OnDemand` / `terms.Reserved`).
//! - [`PricingLoader::from_cache`]: the compact table the Python cache held
//!   (`node_pricing`, `serverless_storage`, ...), so a cache written by the old
//!   script keeps working.
//!
//! Lookups return `Err(message)` where the Python raised `ValueError`.

use std::collections::{BTreeSet, HashMap};

use toolkit_core::{Json, ToolkitError};

use crate::aurora::api::parse_csv_rows;
use crate::aurora::args::parse_py_float;
use crate::aurora::pyfmt::{get_f64, round_to, Num};

use super::text::show;

/// Hours in the billing month the scripts use.
pub const HOURS_PER_MONTH: f64 = 730.0;

/// Upper bound for a pricing file (the bulk index is tens of megabytes).
pub const MAX_PRICING_BYTES: u64 = 256 * 1024 * 1024;

/// The note printed when the CSV loader is used.
pub const CSV_NOTE: &str = "Note: CSV pricing does not include Reserved Instance or Extended Support data. Use --pricing-json with the AWS bulk pricing file for full coverage.";

/// Result of a lookup that can miss (Python `ValueError`).
pub type Lookup<T> = Result<T, String>;

/// Where the prices come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingKind {
    /// A pricing CSV (`--pricing-csv` / `--pricing`).
    Csv,
    /// A bulk pricing JSON or compact cache (`--pricing-json`).
    Json,
}

/// Hourly and upfront components of a reserved price.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReservedRate {
    /// `$` per hour.
    pub hourly: f64,
    /// Lump sum.
    pub upfront: f64,
}

/// One reserved purchase option of a node, with its effective monthly cost.
#[derive(Debug, Clone, PartialEq)]
pub struct ReservedOption {
    /// `1yr` or `3yr`.
    pub lease_length: String,
    /// `No Upfront`, `Partial Upfront` or `All Upfront`.
    pub purchase_option: String,
    /// `$` per hour.
    pub hourly: f64,
    /// Lump sum.
    pub upfront: f64,
    /// `round(upfront / term_months + hourly * 730, 2)`.
    pub effective_monthly: f64,
}

type ReservedKey = (String, String, String, String, String);

/// The loaded price tables.
#[derive(Debug, Default)]
pub struct PricingLoader {
    serverless_storage: HashMap<(String, String), f64>,
    serverless_ecpu: HashMap<(String, String), f64>,
    node_od: HashMap<(String, String, String), f64>,
    extended_support: HashMap<(String, String, String, String), f64>,
    reserved_index: HashMap<ReservedKey, usize>,
    reserved: Vec<(ReservedKey, ReservedRate)>,
    node_memory: HashMap<String, f64>,
}

fn clean(parts: &[&str]) -> bool {
    !parts.iter().any(|part| part.contains('|'))
}

fn text_of<'a>(value: &'a Json, key: &str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap_or("")
}

// recorded-shape: a price table file the user passes (the AWS bulk pricing offer file
// or the old script's cache, `--pricing-json`), not a recorded API response.
fn members(value: Option<&Json>) -> &[(String, Json)] {
    match value {
        Some(Json::Object(items)) => items,
        _ => &[],
    }
}

/// `float(dim.get("pricePerUnit", {}).get("USD", "0"))`.
fn price_of(dim: &Json) -> Option<f64> {
    match dim.get("pricePerUnit").and_then(|price| price.get("USD")) {
        None => Some(0.0),
        Some(Json::String(text)) => parse_py_float(text),
        Some(Json::Int(number)) => Some(*number as f64),
        Some(Json::Float(number)) => Some(*number),
        Some(Json::Bool(flag)) => Some(f64::from(u8::from(*flag))),
        Some(_) => None,
    }
}

fn od_price_with_unit(sku: &str, od_terms: Option<&Json>) -> Option<(f64, String)> {
    let sku_terms = od_terms.and_then(|terms| terms.get(sku));
    for (_, offer) in members(sku_terms) {
        for (_, dim) in members(offer.get("priceDimensions")) {
            if let Some(price) = price_of(dim) {
                return Some((price, text_of(dim, "unit").to_string()));
            }
        }
    }
    None
}

/// Year tiers named by an Extended Support `usagetype`.
fn extended_support_years(usagetype: &str) -> Vec<&'static str> {
    let lowered = usagetype.to_lowercase();
    if lowered.contains("yr1_yr2") {
        return vec!["1", "2"];
    }
    if lowered.contains("yr3") {
        return vec!["3"];
    }
    if lowered.contains("yr1") && !lowered.contains("yr2") {
        return vec!["1"];
    }
    if lowered.contains("yr2") && !lowered.contains("yr1") {
        return vec!["2"];
    }
    if lowered.replace(['-', '_'], "").contains("extendedsupport") {
        return vec!["1"];
    }
    Vec::new()
}

/// Engine names of the bulk file normalized to `valkey`, `redis`, `memcached`.
fn normalize_engine(engine: &str) -> String {
    let lowered = engine.to_lowercase();
    let lowered = lowered.trim();
    if lowered.contains("valkey") {
        "valkey".to_string()
    } else if lowered.contains("redis") {
        "redis".to_string()
    } else if lowered.contains("memcached") {
        "memcached".to_string()
    } else {
        lowered.to_string()
    }
}

/// `"103.68 GiB"` to `103.68`; `0.0` when absent or unparseable.
fn parse_memory_gib(text: &str) -> f64 {
    text.split_whitespace()
        .next()
        .and_then(parse_py_float)
        .unwrap_or(0.0)
}

fn effective_monthly(lease: &str, rate: ReservedRate) -> f64 {
    let term_months = if lease == "1yr" { 12.0 } else { 36.0 };
    round_to(
        (rate.upfront / term_months) + (rate.hourly * HOURS_PER_MONTH),
        2,
    )
}

impl PricingLoader {
    fn insert_node(&mut self, region: &str, itype: &str, engine: &str, price: f64) {
        if clean(&[region, itype, engine]) {
            self.node_od.insert(
                (region.to_string(), itype.to_string(), engine.to_string()),
                price,
            );
        }
    }

    fn insert_storage(&mut self, region: &str, engine: &str, price: f64) {
        if clean(&[region, engine]) {
            self.serverless_storage
                .insert((region.to_string(), engine.to_string()), price);
        }
    }

    fn insert_ecpu(&mut self, region: &str, engine: &str, price: f64) {
        if clean(&[region, engine]) {
            self.serverless_ecpu
                .insert((region.to_string(), engine.to_string()), price);
        }
    }

    fn insert_extended(&mut self, region: &str, itype: &str, engine: &str, year: &str, price: f64) {
        if clean(&[region, itype, engine, year]) {
            self.extended_support.insert(
                (
                    region.to_string(),
                    itype.to_string(),
                    engine.to_string(),
                    year.to_string(),
                ),
                price,
            );
        }
    }

    fn insert_reserved(&mut self, key: ReservedKey, rate: ReservedRate) {
        let parts = [
            key.0.as_str(),
            key.1.as_str(),
            key.2.as_str(),
            key.3.as_str(),
            key.4.as_str(),
        ];
        if !clean(&parts) {
            return;
        }
        if let Some(&index) = self.reserved_index.get(&key) {
            self.reserved[index].1 = rate;
        } else {
            self.reserved_index.insert(key.clone(), self.reserved.len());
            self.reserved.push((key, rate));
        }
    }

    /// Prices from a local CSV (`csv.DictReader` columns `Product Family`,
    /// `Region Code`, `Cache Engine`, `TermType`, `PricePerUnit`, `Instance
    /// Type`, `Unit`, `Operation`). Only on-demand node and serverless rows are
    /// read.
    #[must_use]
    pub fn from_csv_text(text: &str) -> Self {
        let mut loader = Self::default();
        for row in parse_csv_rows(text) {
            let get = |key: &str| row.get(key).map_or("", String::as_str);
            let family = get("Product Family");
            let region = get("Region Code");
            let engine = get("Cache Engine").to_lowercase();
            let term = get("TermType");
            let raw_price = row.get("PricePerUnit").map_or("0", String::as_str);
            let Some(price) = parse_py_float(raw_price) else {
                continue;
            };
            if price <= 0.0 || region.is_empty() {
                continue;
            }
            if family == "Cache Instance" && term == "OnDemand" {
                let itype = get("Instance Type");
                if !itype.is_empty() && get("Unit") == "Hrs" {
                    loader.insert_node(region, itype, &engine, price);
                }
            } else if family == "ElastiCache Serverless" {
                let unit = get("Unit");
                let operation = row
                    .get("Operation")
                    .or_else(|| row.get("operation"))
                    .map_or("", String::as_str);
                if operation.contains("Snapshot") || unit == "GB-months" {
                    continue;
                }
                let unit_lower = unit.to_lowercase();
                if unit_lower.contains("gb-hour") {
                    loader.insert_storage(region, &engine, price);
                } else if unit_lower.contains("processingunit") {
                    loader.insert_ecpu(region, &engine, price);
                }
            }
        }
        loader
    }

    /// Prices from the AWS bulk pricing JSON (`_parse_bulk_pricing`).
    #[must_use]
    pub fn from_bulk_pricing(data: &Json) -> Self {
        let mut loader = Self::default();
        let terms = data.get("terms");
        let od_terms = terms.and_then(|all| all.get("OnDemand"));
        let products = data.get("products");

        for (sku, product) in members(products) {
            let attrs = product.get("attributes");
            let attr = |key: &str| attrs.map_or("", |all| text_of(all, key));
            let family = text_of(product, "productFamily");
            let region = attr("regionCode");
            if region.is_empty() || attr("locationType") != "AWS Region" {
                continue;
            }
            let Some((price, unit)) = od_price_with_unit(sku, od_terms) else {
                continue;
            };
            if price <= 0.0 {
                continue;
            }
            if family == "Cache Instance" {
                let itype = attr("instanceType");
                let engine = normalize_engine(attr("cacheEngine"));
                let usagetype = attr("usagetype");
                if usagetype.contains("ExtendedSupport") {
                    if engine.is_empty() || itype.is_empty() || unit != "Hrs" {
                        continue;
                    }
                    for year in extended_support_years(usagetype) {
                        loader.insert_extended(region, itype, &engine, year, price);
                    }
                } else if !itype.is_empty() && !engine.is_empty() && unit == "Hrs" {
                    loader.insert_node(region, itype, &engine, price);
                    if !loader.node_memory.contains_key(itype) {
                        let memory = parse_memory_gib(attr("memory"));
                        if memory > 0.0 {
                            loader.node_memory.insert(itype.to_string(), memory);
                        }
                    }
                }
            } else if family == "ElastiCache Serverless" {
                let engine = normalize_engine(attr("cacheEngine"));
                let operation = attr("operation");
                let usagetype = attr("usagetype");
                if operation.contains("Snapshot") || usagetype.contains("Backup") {
                    continue;
                }
                if engine.is_empty() {
                    continue;
                }
                let unit_lower = unit.to_lowercase();
                if unit_lower.contains("gb-hour") || usagetype.contains("CachedData") {
                    loader.insert_storage(region, &engine, price);
                } else if unit_lower.contains("processingunit")
                    || usagetype.contains("ProcessingUnits")
                {
                    loader.insert_ecpu(region, &engine, price);
                }
            }
        }

        loader.load_reserved_terms(products, terms.and_then(|all| all.get("Reserved")));
        loader
    }

    fn load_reserved_terms(&mut self, products: Option<&Json>, reserved_terms: Option<&Json>) {
        const VALID: [&str; 3] = ["No Upfront", "Partial Upfront", "All Upfront"];
        for (sku, offers) in members(reserved_terms) {
            let Some(product) = products.and_then(|all| all.get(sku)) else {
                continue;
            };
            if text_of(product, "productFamily") != "Cache Instance" {
                continue;
            }
            let attrs = product.get("attributes");
            let attr = |key: &str| attrs.map_or("", |all| text_of(all, key));
            if attr("locationType") != "AWS Region" || attr("usagetype").contains("ExtendedSupport")
            {
                continue;
            }
            let region = attr("regionCode");
            let itype = attr("instanceType");
            let engine = normalize_engine(attr("cacheEngine"));
            if region.is_empty() || itype.is_empty() || engine.is_empty() {
                continue;
            }
            for (_, offer) in members(Some(offers)) {
                let term_attrs = offer.get("termAttributes");
                let term_attr = |key: &str| term_attrs.map_or("", |all| text_of(all, key));
                let purchase = term_attr("PurchaseOption");
                if !VALID.contains(&purchase) {
                    continue;
                }
                let lease = term_attr("LeaseContractLength");
                let mut hourly = 0.0;
                let mut upfront = 0.0;
                for (_, dim) in members(offer.get("priceDimensions")) {
                    let Some(price) = price_of(dim) else {
                        continue;
                    };
                    match text_of(dim, "unit") {
                        "Hrs" => hourly = price,
                        "Quantity" => upfront = price,
                        _ => {}
                    }
                }
                self.insert_reserved(
                    (
                        region.to_string(),
                        itype.to_string(),
                        engine.clone(),
                        lease.to_string(),
                        purchase.to_string(),
                    ),
                    ReservedRate { hourly, upfront },
                );
            }
        }
    }

    /// Prices from the compact table the Python cache stored.
    #[must_use]
    pub fn from_cache(data: &Json) -> Self {
        let mut loader = Self::default();
        let number = |value: &Json| Num::from_json(value).map(Num::value);
        for (key, price) in members(data.get("node_pricing")) {
            let parts: Vec<&str> = key.split('|').collect();
            if let (3, Some(price)) = (parts.len(), number(price)) {
                loader.insert_node(parts[0], parts[1], parts[2], price);
            }
        }
        for (key, price) in members(data.get("serverless_storage")) {
            let parts: Vec<&str> = key.split('|').collect();
            if let (2, Some(price)) = (parts.len(), number(price)) {
                loader.insert_storage(parts[0], parts[1], price);
            }
        }
        for (key, price) in members(data.get("serverless_ecpu")) {
            let parts: Vec<&str> = key.split('|').collect();
            if let (2, Some(price)) = (parts.len(), number(price)) {
                loader.insert_ecpu(parts[0], parts[1], price);
            }
        }
        for (key, price) in members(data.get("extended_support")) {
            let parts: Vec<&str> = key.split('|').collect();
            if let (4, Some(price)) = (parts.len(), number(price)) {
                loader.insert_extended(parts[0], parts[1], parts[2], parts[3], price);
            }
        }
        for (key, info) in members(data.get("reserved_pricing")) {
            let parts: Vec<&str> = key.split('|').collect();
            if parts.len() == 5 {
                let rate = ReservedRate {
                    hourly: get_f64(info, "hourly", 0.0),
                    upfront: get_f64(info, "upfront", 0.0),
                };
                let key = (
                    parts[0].to_string(),
                    parts[1].to_string(),
                    parts[2].to_string(),
                    parts[3].to_string(),
                    parts[4].to_string(),
                );
                loader.insert_reserved(key, rate);
            }
        }
        for (itype, memory) in members(data.get("node_memory")) {
            if let Some(memory) = number(memory) {
                loader.node_memory.insert(itype.clone(), memory);
            }
        }
        loader
    }

    /// Loads either JSON form: the bulk pricing index (has `products`) or the
    /// compact cache.
    #[must_use]
    pub fn from_json(data: &Json) -> Self {
        if data.get("products").is_some() {
            Self::from_bulk_pricing(data)
        } else {
            Self::from_cache(data)
        }
    }

    /// Reads a pricing file of the given kind through the shared path contract.
    ///
    /// # Errors
    ///
    /// A missing, unreadable, oversized or (for JSON) unparseable file.
    pub fn load_file(path: &std::path::Path, kind: PricingKind) -> Result<Self, ToolkitError> {
        let text = toolkit_core::read_regular_file(path, MAX_PRICING_BYTES)?;
        match kind {
            PricingKind::Csv => Ok(Self::from_csv_text(&text)),
            PricingKind::Json => Ok(Self::from_json(&toolkit_core::parse_json(&text)?)),
        }
    }

    /// Serverless storage rate in `$` per GB-hour.
    ///
    /// # Errors
    ///
    /// No rate for the region and engine.
    pub fn get_serverless_storage_rate(&self, region: &str, engine: &str) -> Lookup<f64> {
        let eng = engine.to_lowercase();
        match self
            .serverless_storage
            .get(&(region.to_string(), eng.clone()))
        {
            Some(rate) => Ok(*rate),
            None => Err(format!(
                "No serverless storage pricing found for region={}, engine={}. Available: {}",
                show(region),
                show(&eng),
                self.list_serverless_regions(&eng)
            )),
        }
    }

    /// Serverless price of one ECPU in `$`.
    ///
    /// # Errors
    ///
    /// No rate for the region and engine.
    pub fn get_serverless_ecpu_rate(&self, region: &str, engine: &str) -> Lookup<f64> {
        let eng = engine.to_lowercase();
        match self.serverless_ecpu.get(&(region.to_string(), eng.clone())) {
            Some(rate) => Ok(*rate),
            None => Err(format!(
                "No serverless ECPU pricing found for region={}, engine={}. Available: {}",
                show(region),
                show(&eng),
                self.list_serverless_regions(&eng)
            )),
        }
    }

    /// On-demand hourly rate of a node.
    ///
    /// # Errors
    ///
    /// No rate for the region, instance type and engine.
    pub fn get_node_hourly_rate(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
    ) -> Lookup<f64> {
        let eng = engine.to_lowercase();
        let key = (region.to_string(), instance_type.to_string(), eng.clone());
        match self.node_od.get(&key) {
            Some(rate) => Ok(*rate),
            None => Err(format!(
                "No node pricing found for region={}, instance_type={}, engine={}",
                show(region),
                show(instance_type),
                show(&eng)
            )),
        }
    }

    /// Extended Support surcharge in `$` per node-hour for a year tier.
    ///
    /// # Errors
    ///
    /// No surcharge is published for the combination.
    pub fn get_extended_support_rate(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
        year: &str,
    ) -> Lookup<f64> {
        let eng = engine.to_lowercase();
        let key = (
            region.to_string(),
            instance_type.to_string(),
            eng.clone(),
            year.to_string(),
        );
        match self.extended_support.get(&key) {
            Some(rate) => Ok(*rate),
            None => Err(format!(
                "No Extended Support pricing found for region={}, instance_type={}, engine={}, year={}. This may mean Extended Support pricing is not published for this combination.",
                show(region),
                show(instance_type),
                show(&eng),
                show(year)
            )),
        }
    }

    /// Whether any Extended Support price was loaded.
    #[must_use]
    pub fn has_extended_support_pricing(&self) -> bool {
        !self.extended_support.is_empty()
    }

    /// Memory of a node type in GB (`0.0` when unknown).
    #[must_use]
    pub fn get_node_memory_gb(&self, instance_type: &str) -> f64 {
        self.node_memory.get(instance_type).copied().unwrap_or(0.0)
    }

    fn list_serverless_regions(&self, engine: &str) -> String {
        let regions: BTreeSet<&str> = self
            .serverless_storage
            .keys()
            .filter(|(_, name)| name == engine)
            .map(|(region, _)| region.as_str())
            .collect();
        let sorted: Vec<&str> = regions.into_iter().collect();
        if sorted.len() > 5 {
            return format!("{} and {} more", sorted[..5].join(", "), sorted.len() - 5);
        }
        if sorted.is_empty() {
            "none loaded".to_string()
        } else {
            sorted.join(", ")
        }
    }

    /// Reserved price of a node.
    ///
    /// # Errors
    ///
    /// No reserved price for the combination.
    pub fn get_reserved_rate(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
        lease_length: &str,
        purchase_option: &str,
    ) -> Lookup<ReservedRate> {
        let eng = engine.to_lowercase();
        let key = (
            region.to_string(),
            instance_type.to_string(),
            eng.clone(),
            lease_length.to_string(),
            purchase_option.to_string(),
        );
        match self.reserved_index.get(&key) {
            Some(&index) => Ok(self.reserved[index].1),
            None => Err(format!(
                "No reserved pricing found for region={}, instance_type={}, engine={}, lease={}, purchase={}",
                show(region),
                show(instance_type),
                show(&eng),
                show(lease_length),
                show(purchase_option)
            )),
        }
    }

    /// Effective monthly cost of a reserved node: the upfront amortized over
    /// the term plus the recurring hourly cost.
    ///
    /// # Errors
    ///
    /// No reserved price for the combination.
    pub fn get_reserved_effective_monthly(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
        lease_length: &str,
        purchase_option: &str,
    ) -> Lookup<f64> {
        let rate =
            self.get_reserved_rate(region, instance_type, engine, lease_length, purchase_option)?;
        Ok(effective_monthly(lease_length, rate))
    }

    /// Every reserved option of a node, cheapest effective monthly cost first
    /// (ties keep the order of the pricing file).
    #[must_use]
    pub fn list_reserved_options(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
    ) -> Vec<ReservedOption> {
        let eng = engine.to_lowercase();
        let mut options: Vec<ReservedOption> = self
            .reserved
            .iter()
            .filter(|((r, it, e, _, _), _)| r == region && it == instance_type && *e == eng)
            .map(|((_, _, _, lease, purchase), rate)| ReservedOption {
                lease_length: lease.clone(),
                purchase_option: purchase.clone(),
                hourly: rate.hourly,
                upfront: rate.upfront,
                effective_monthly: effective_monthly(lease, *rate),
            })
            .collect();
        options.sort_by(|a, b| a.effective_monthly.total_cmp(&b.effective_monthly));
        options
    }
}
