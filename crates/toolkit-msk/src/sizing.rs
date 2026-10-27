//! MSK broker sizing (port of `msk_sizing.py`).
//!
//! # Unit convention
//!
//! All throughput is in **MiB/s** and all storage in **GiB**; conversions use
//! the binary factor 1024 (`MiB/s × 3600 / 1024 → GiB/h`). Field names ending
//! in `_mbs` keep the legacy spelling but mean MiB/s.
//!
//! Every formula keeps the legacy operation order so floating point results
//! are bit-identical to the Python script.

use crate::pricing::Pricing;
use crate::pyfmt::py_float_repr;

/// Sizing assumes 3 AZs because Express only supports 3 AZ configurations.
pub const NUM_AZS: i64 = 3;
/// Hours per month used for hourly prices.
pub const HOURS_PER_MONTH: f64 = 730.0;
/// EBS maximum per MSK broker (16 TiB), in GiB.
pub const MAX_EBS_GB_PER_BROKER: f64 = 16_384.0;
/// Default per-cluster broker quota (MSK Provisioned, `KRaft`).
pub const DEFAULT_BROKER_QUOTA: i64 = 60;
/// EBS volumes are provisioned with a 50% utilization buffer.
pub const EBS_HEADROOM_FACTOR: f64 = 2.0;
/// EBS write I/O per unit ingress without Tiered Storage
/// (1.5 ingress + 1.5 replication-in + 0.5 lagging + 3.0 rebalancing).
pub const STORAGE_IO_FACTOR_BASE: f64 = 6.5;
/// Extra EBS I/O when Tiered Storage is enabled.
pub const STORAGE_IO_FACTOR_TS_ADD: f64 = 1.5;
/// Static outbound NIC multiplier without Tiered Storage
/// (2.2 replication-out + 0.5 lagging + 3.0 rebalancing).
pub const NETWORK_IO_BASE: f64 = 5.7;
/// Extra NIC traffic when Tiered Storage is enabled.
pub const NETWORK_IO_TS_ADD: f64 = 1.5;
/// Consumer traffic scales by `NUM_AZS / (NUM_AZS - 1)` with one AZ down.
pub const AZ_SCALE_FACTOR: f64 = 1.5;
/// Express egress capacity = `ingress_mbs` × this factor.
pub const EXPRESS_EGRESS_FACTOR: f64 = 2.5;

/// Physical capacity limits of one instance type. The Pricing API does not
/// expose these, so they stay hardcoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Capacity {
    /// Standard (M5 / M7g).
    Standard {
        /// Maximum provisionable EBS throughput per broker (MiB/s); also the
        /// documented PST cap for the instance.
        ebs_throughput_mbs: f64,
        /// NIC bandwidth available to the broker (MiB/s).
        network_throughput_mbs: f64,
    },
    /// Express (M7g).
    Express {
        /// Maximum producer throughput per broker (MiB/s).
        ingress_mbs: f64,
    },
}

/// One entry of the instance table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstanceSpec {
    /// Table key, for example `kafka.m5.large` or `kafka.m7g.large (Express)`.
    pub name: &'static str,
    /// Throughput limits.
    pub capacity: Capacity,
    /// Recommended maximum partitions per broker (leaders + followers).
    pub rec_partitions: i64,
    /// Hard maximum partitions per broker.
    pub max_partitions: i64,
}

const fn standard(
    name: &'static str,
    ebs_throughput_mbs: f64,
    network_throughput_mbs: f64,
    rec_partitions: i64,
    max_partitions: i64,
) -> InstanceSpec {
    InstanceSpec {
        name,
        capacity: Capacity::Standard {
            ebs_throughput_mbs,
            network_throughput_mbs,
        },
        rec_partitions,
        max_partitions,
    }
}

const fn express(
    name: &'static str,
    ingress_mbs: f64,
    rec_partitions: i64,
    max_partitions: i64,
) -> InstanceSpec {
    InstanceSpec {
        name,
        capacity: Capacity::Express { ingress_mbs },
        rec_partitions,
        max_partitions,
    }
}

/// The instance table, in the legacy iteration order (report order).
pub const INSTANCE_SPECS: &[InstanceSpec] = &[
    standard("kafka.m5.large", 81.0, 96.0, 1000, 1500),
    standard("kafka.m5.xlarge", 144.0, 160.0, 1000, 1500),
    standard("kafka.m5.2xlarge", 250.0, 320.0, 2000, 3000),
    standard("kafka.m5.4xlarge", 593.75, 640.0, 4000, 6000),
    standard("kafka.m5.8xlarge", 850.0, 1280.0, 4000, 6000),
    standard("kafka.m5.12xlarge", 1000.0, 1536.0, 4000, 6000),
    standard("kafka.m5.16xlarge", 1000.0, 2560.0, 4000, 6000),
    standard("kafka.m5.24xlarge", 1000.0, 3200.0, 4000, 4000),
    standard("kafka.m7g.large", 78.75, 117.0, 1000, 1500),
    standard("kafka.m7g.xlarge", 156.25, 234.0, 1000, 1500),
    standard("kafka.m7g.2xlarge", 312.5, 469.0, 2000, 3000),
    standard("kafka.m7g.4xlarge", 625.0, 937.0, 4000, 6000),
    standard("kafka.m7g.8xlarge", 1000.0, 1875.0, 4000, 6000),
    standard("kafka.m7g.12xlarge", 1000.0, 2812.0, 4000, 6000),
    standard("kafka.m7g.16xlarge", 1000.0, 3750.0, 4000, 6000),
    express("kafka.m7g.large (Express)", 15.625, 1000, 1500),
    express("kafka.m7g.xlarge (Express)", 31.25, 1000, 2000),
    express("kafka.m7g.2xlarge (Express)", 62.5, 2500, 4000),
    express("kafka.m7g.4xlarge (Express)", 125.0, 6000, 8000),
    express("kafka.m7g.8xlarge (Express)", 250.0, 12_000, 16_000),
    express("kafka.m7g.12xlarge (Express)", 375.0, 16_000, 24_000),
    express("kafka.m7g.16xlarge (Express)", 500.0, 20_000, 32_000),
];

/// Only 4xlarge and larger support Provisioned Storage Throughput (PST).
pub const PST_ELIGIBLE: &[&str] = &[
    "kafka.m5.4xlarge",
    "kafka.m5.8xlarge",
    "kafka.m5.12xlarge",
    "kafka.m5.16xlarge",
    "kafka.m7g.4xlarge",
    "kafka.m7g.8xlarge",
    "kafka.m7g.12xlarge",
    "kafka.m7g.16xlarge",
    "kafka.m5.24xlarge",
];

/// Whether `instance_type` supports Provisioned Storage Throughput.
#[must_use]
pub fn is_pst_eligible(instance_type: &str) -> bool {
    PST_ELIGIBLE.contains(&instance_type)
}

/// Recommendation class of an instance type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerClass {
    /// `kafka.m5.*` Standard.
    M5Standard,
    /// `kafka.m7g.*` Standard.
    M7gStandard,
    /// `* (Express)`.
    Express,
}

impl BrokerClass {
    /// Report label of the class.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::M5Standard => "Standard M5",
            Self::M7gStandard => "Standard M7g",
            Self::Express => "Express M7g",
        }
    }
}

/// Groups instance types into the three recommendation classes; `None` for an
/// unrecognized name (never produced by [`INSTANCE_SPECS`]).
#[must_use]
pub fn classify(instance_type: &str) -> Option<BrokerClass> {
    if instance_type.contains("Express") {
        Some(BrokerClass::Express)
    } else if instance_type.starts_with("kafka.m7g.") {
        Some(BrokerClass::M7gStandard)
    } else if instance_type.starts_with("kafka.m5.") {
        Some(BrokerClass::M5Standard)
    } else {
        None
    }
}

/// A `--broker-classes` selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerGroup {
    /// M5 and M7g Standard brokers.
    Standard,
    /// Express brokers.
    Express,
}

impl BrokerGroup {
    /// The recommendation classes the group covers.
    #[must_use]
    pub fn classes(self) -> &'static [BrokerClass] {
        match self {
            Self::Standard => &[BrokerClass::M5Standard, BrokerClass::M7gStandard],
            Self::Express => &[BrokerClass::Express],
        }
    }
}

/// Parses a `--broker-classes` value into an ordered, de-duplicated list.
///
/// # Errors
///
/// Returns the legacy message for an unknown class or an empty selection.
pub fn parse_broker_classes(value: &str) -> Result<Vec<BrokerGroup>, String> {
    let mut selected: Vec<BrokerGroup> = Vec::new();
    for raw in value.split(',') {
        let trimmed = raw.trim();
        let group = match trimmed.to_lowercase().as_str() {
            "" => continue,
            "standard" => BrokerGroup::Standard,
            "express" => BrokerGroup::Express,
            _ => {
                return Err(format!(
                    "invalid broker class {}; choose from: express, standard \
                     (comma-separate to select both)",
                    crate::pyfmt::py_str_repr(trimmed)
                ));
            }
        };
        if !selected.contains(&group) {
            selected.push(group);
        }
    }
    if selected.is_empty() {
        return Err("--broker-classes requires at least one of: express, standard".to_string());
    }
    Ok(selected)
}

/// Parses a `--discount-pct` value as a percentage in `[0, 100)`.
///
/// # Errors
///
/// Returns the legacy message for a non-number or an out-of-range value.
pub fn parse_discount_pct(value: &str) -> Result<f64, String> {
    let Some(pct) = crate::pyfmt::py_float_parse(value) else {
        return Err(format!(
            "invalid discount percentage {}; expected a number",
            crate::pyfmt::py_str_repr(value)
        ));
    };
    if !(0.0..100.0).contains(&pct) {
        return Err(format!(
            "discount percentage must be >= 0 and < 100; got {value}"
        ));
    }
    Ok(pct)
}

/// Converts a PPA / EDP discount percentage into a cost multiplier applied to
/// every cost dimension.
///
/// # Errors
///
/// Returns the legacy message when the percentage is outside `[0, 100)`.
pub fn discount_multiplier(discount_pct: f64) -> Result<f64, String> {
    if !(0.0..100.0).contains(&discount_pct) {
        return Err(format!(
            "discount_pct must be >= 0 and < 100; got {}",
            py_float_repr(discount_pct)
        ));
    }
    Ok(1.0 - discount_pct / 100.0)
}

/// Workload description for a sizing run.
#[derive(Debug, Clone, PartialEq)]
pub struct SizingInputs {
    /// Average producer throughput (MiB/s).
    pub avg_data_in_mbs: f64,
    /// Peak producer throughput (MiB/s).
    pub peak_data_in_mbs: f64,
    /// Average consumer throughput (MiB/s).
    pub avg_data_out_mbs: f64,
    /// Peak consumer throughput (MiB/s).
    pub peak_data_out_mbs: f64,
    /// Total partitions including replicas.
    pub num_partitions: i64,
    /// Kafka replication factor (Standard: 2 or 3; Express: always 3).
    pub replication_factor: i64,
    /// Total data retention (hours).
    pub retention_hours: i64,
    /// Primary (EBS) retention (hours); the remainder goes to Tiered Storage.
    pub primary_retention_hours: i64,
    /// Maximum fraction of broker capacity to use (Standard).
    pub utilization_standard: f64,
    /// Maximum fraction of broker capacity to use (Express).
    pub utilization_express: f64,
    /// Provisioned storage throughput per broker (MiB/s); 250–1000.
    pub pst_per_broker_mbs: Option<f64>,
    /// Use the hard maximum partition limit instead of the recommended one.
    pub use_max_partitions: bool,
    /// When false, cross-AZ cost includes consumer fetch traffic.
    pub rack_affined_consumers: bool,
}

impl SizingInputs {
    /// Validates the workload.
    ///
    /// The legacy checks run first and keep their messages. Two additions
    /// replace crashes of the Python script: non-finite numbers and a
    /// utilization that is not strictly positive are rejected with a message
    /// instead of producing `nan` results or a `ZeroDivisionError`.
    ///
    /// # Errors
    ///
    /// Returns the first violated rule.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("avg_data_in_mbs", self.avg_data_in_mbs),
            ("peak_data_in_mbs", self.peak_data_in_mbs),
            ("avg_data_out_mbs", self.avg_data_out_mbs),
            ("peak_data_out_mbs", self.peak_data_out_mbs),
        ] {
            if !value.is_finite() {
                return Err(format!(
                    "{name} must be a finite number; got {}",
                    py_float_repr(value)
                ));
            }
            if value <= 0.0 {
                return Err(format!("{name} must be > 0; got {}", py_float_repr(value)));
            }
        }
        if self.peak_data_in_mbs < self.avg_data_in_mbs {
            return Err(format!(
                "peak_data_in_mbs ({}) must be >= avg_data_in_mbs ({})",
                py_float_repr(self.peak_data_in_mbs),
                py_float_repr(self.avg_data_in_mbs)
            ));
        }
        if self.peak_data_out_mbs < self.avg_data_out_mbs {
            return Err(format!(
                "peak_data_out_mbs ({}) must be >= avg_data_out_mbs ({})",
                py_float_repr(self.peak_data_out_mbs),
                py_float_repr(self.avg_data_out_mbs)
            ));
        }
        if self.replication_factor != 2 && self.replication_factor != 3 {
            return Err(format!(
                "replication_factor must be 2 or 3; got {}. \
                 Express always uses RF=3 internally regardless of this value.",
                self.replication_factor
            ));
        }
        if self.primary_retention_hours > self.retention_hours {
            return Err(format!(
                "primary_retention_hours ({}) must be <= retention_hours ({})",
                self.primary_retention_hours, self.retention_hours
            ));
        }
        if let Some(pst) = self.pst_per_broker_mbs {
            if !(250.0..=1000.0).contains(&pst) {
                return Err(format!(
                    "pst_per_broker_mbs must be between 250 and 1000 MiB/s; got {}",
                    py_float_repr(pst)
                ));
            }
        }
        for (name, value) in [
            ("utilization_standard", self.utilization_standard),
            ("utilization_express", self.utilization_express),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!(
                    "{name} must be a finite number > 0; got {}",
                    py_float_repr(value)
                ));
            }
        }
        Ok(())
    }
}

/// Per-constraint detail for a sizing result.
#[derive(Debug, Clone, PartialEq)]
pub struct BottleneckDetail {
    /// Constraint name: `ingress`, `egress`, `partitions`, `storage` or `pst`.
    pub name: &'static str,
    /// Brokers this constraint requires (a multiple of 3).
    pub brokers_needed: i64,
    /// Demand placed on the constraint.
    pub demand: f64,
    /// Capacity of one broker for this constraint.
    pub per_broker_capacity: f64,
    /// Unit description.
    pub unit: String,
}

/// Sizing and monthly cost for one instance type.
#[derive(Debug, Clone, PartialEq)]
pub struct SizingResult {
    /// Instance table key.
    pub instance_type: &'static str,
    /// Required broker count (maximum over all constraints).
    pub broker_count: i64,
    /// Name of the first constraint that produced `broker_count`.
    pub bottleneck: &'static str,
    /// Broker hours.
    pub monthly_broker_cost: f64,
    /// EBS (Standard) or Express storage cost.
    pub monthly_ebs_cost: f64,
    /// Tiered Storage cost.
    pub monthly_ts_cost: f64,
    /// Express data-in cost.
    pub monthly_data_in_cost: f64,
    /// Cross-AZ transfer cost.
    pub monthly_cross_az_cost: f64,
    /// Provisioned Storage Throughput cost.
    pub monthly_pst_cost: f64,
    /// Sum of all cost dimensions.
    pub total_monthly_cost: f64,
    /// Details per constraint, in evaluation order.
    pub bottleneck_details: Vec<BottleneckDetail>,
}

/// Minimum broker count to serve `demand`, rounded up to a multiple of
/// [`NUM_AZS`].
#[must_use]
pub fn brokers_for(demand: f64, per_broker_capacity: f64) -> i64 {
    let raw = (demand / per_broker_capacity).ceil();
    let groups = (raw / 3.0).ceil();
    groups as i64 * NUM_AZS
}

fn partition_limit(spec: &InstanceSpec, use_max: bool) -> i64 {
    if use_max {
        spec.max_partitions
    } else {
        spec.rec_partitions
    }
}

fn partition_unit(use_max: bool) -> String {
    format!("partitions ({})", if use_max { "max" } else { "rec" })
}

fn cross_az_cost(inputs: &SizingInputs, pricing: &Pricing, discount: f64) -> f64 {
    let mut cross_az_mbs = inputs.avg_data_in_mbs * 2.0 / 3.0;
    if !inputs.rack_affined_consumers {
        cross_az_mbs += inputs.avg_data_out_mbs * 2.0 / 3.0;
    }
    let cross_az_gb_mo = cross_az_mbs * 3600.0 * HOURS_PER_MONTH / 1024.0;
    cross_az_gb_mo * pricing.cross_az_cost_per_gb * discount
}

/// Picks the first detail with the maximum broker count, like Python's `max`.
fn pick_bottleneck(details: &[BottleneckDetail]) -> (i64, &'static str) {
    let mut best: Option<&BottleneckDetail> = None;
    for detail in details {
        if best.is_none_or(|current| detail.brokers_needed > current.brokers_needed) {
            best = Some(detail);
        }
    }
    best.map_or((0, "ingress"), |detail| {
        (detail.brokers_needed, detail.name)
    })
}

/// Calculates sizing for all Standard (M5 / M7g) instance types that have a
/// price in `pricing`.
///
/// # Errors
///
/// Returns the legacy message when `discount_pct` is outside `[0, 100)`.
pub fn calculate_standard_sizing(
    inputs: &SizingInputs,
    pricing: &Pricing,
    discount_pct: f64,
) -> Result<Vec<SizingResult>, String> {
    let discount = discount_multiplier(discount_pct)?;
    let mut results = Vec::new();

    let ebs_gb_data = inputs.avg_data_in_mbs
        * inputs.primary_retention_hours as f64
        * 3600.0
        * inputs.replication_factor as f64
        / 1024.0;
    let ebs_gb = ebs_gb_data * EBS_HEADROOM_FACTOR;

    let ts_in_use = inputs.retention_hours > inputs.primary_retention_hours;
    let storage_factor = STORAGE_IO_FACTOR_BASE
        + if ts_in_use {
            STORAGE_IO_FACTOR_TS_ADD
        } else {
            0.0
        };
    let network_base = NETWORK_IO_BASE + if ts_in_use { NETWORK_IO_TS_ADD } else { 0.0 };

    let ts_gb = if ts_in_use {
        inputs.avg_data_in_mbs
            * (inputs.retention_hours - inputs.primary_retention_hours) as f64
            * 3600.0
            / 1024.0
    } else {
        0.0
    };

    let monthly_ebs_cost = ebs_gb * pricing.ebs_cost_per_gb_month * discount;
    let monthly_ts_cost = ts_gb * pricing.tiered_storage_cost_per_gb_month * discount;
    let pst_mbs_per_broker = inputs.pst_per_broker_mbs.unwrap_or(0.0);
    let monthly_cross_az_cost = cross_az_cost(inputs, pricing, discount);

    let fan_out = inputs.peak_data_out_mbs / inputs.peak_data_in_mbs;
    let network_factor = fan_out * AZ_SCALE_FACTOR + network_base;

    for spec in INSTANCE_SPECS {
        let Capacity::Standard {
            ebs_throughput_mbs,
            network_throughput_mbs,
        } = spec.capacity
        else {
            continue;
        };
        let Some(price_per_hr) = pricing.broker_price_per_hr(spec.name) else {
            continue;
        };

        let util = inputs.utilization_standard;

        // Available ingress per broker is the minimum of what EBS writes and
        // NIC bandwidth can sustain, across all concurrent I/O operations.
        let storage_limit = ebs_throughput_mbs / storage_factor;
        let network_limit = network_throughput_mbs / network_factor;
        let ingress_per_broker = storage_limit.min(network_limit);

        let ingress_capacity_per_broker = ingress_per_broker * util;
        let egress_capacity_per_broker = fan_out * ingress_per_broker * util;
        let limit = partition_limit(spec, inputs.use_max_partitions);

        let mut details = vec![
            BottleneckDetail {
                name: "ingress",
                brokers_needed: brokers_for(inputs.peak_data_in_mbs, ingress_capacity_per_broker),
                demand: inputs.peak_data_in_mbs,
                per_broker_capacity: ingress_capacity_per_broker,
                unit: "MiB/s peak ingress (after util)".to_string(),
            },
            BottleneckDetail {
                name: "egress",
                brokers_needed: brokers_for(inputs.peak_data_out_mbs, egress_capacity_per_broker),
                demand: inputs.peak_data_out_mbs,
                per_broker_capacity: egress_capacity_per_broker,
                unit: "MiB/s peak egress (after util, fan-out)".to_string(),
            },
            BottleneckDetail {
                name: "partitions",
                brokers_needed: brokers_for(inputs.num_partitions as f64, limit as f64),
                demand: inputs.num_partitions as f64,
                per_broker_capacity: limit as f64,
                unit: partition_unit(inputs.use_max_partitions),
            },
            BottleneckDetail {
                name: "storage",
                brokers_needed: brokers_for(ebs_gb, MAX_EBS_GB_PER_BROKER),
                demand: ebs_gb,
                per_broker_capacity: MAX_EBS_GB_PER_BROKER,
                unit: "GiB EBS primary".to_string(),
            },
        ];

        if let Some(requested) = inputs.pst_per_broker_mbs {
            if is_pst_eligible(spec.name) {
                let effective_pst = requested.min(ebs_throughput_mbs);
                details.push(BottleneckDetail {
                    name: "pst",
                    brokers_needed: brokers_for(inputs.avg_data_out_mbs, effective_pst),
                    demand: inputs.avg_data_out_mbs,
                    per_broker_capacity: effective_pst,
                    unit: "MiB/s avg egress (capacity = PST cap, clamped to instance EBS max)"
                        .to_string(),
                });
            }
        }

        let (broker_count, bottleneck) = pick_bottleneck(&details);
        let monthly_broker_cost = broker_count as f64 * price_per_hr * HOURS_PER_MONTH * discount;

        let monthly_pst_cost = if pst_mbs_per_broker > 0.0 && is_pst_eligible(spec.name) {
            let effective_pst_mbs = pst_mbs_per_broker.min(ebs_throughput_mbs);
            broker_count as f64 * effective_pst_mbs * pricing.pst_cost_per_mbs_month * discount
        } else {
            0.0
        };

        let total = monthly_broker_cost
            + monthly_ebs_cost
            + monthly_ts_cost
            + monthly_cross_az_cost
            + monthly_pst_cost;

        results.push(SizingResult {
            instance_type: spec.name,
            broker_count,
            bottleneck,
            monthly_broker_cost,
            monthly_ebs_cost,
            monthly_ts_cost,
            monthly_data_in_cost: 0.0,
            monthly_cross_az_cost,
            monthly_pst_cost,
            total_monthly_cost: total,
            bottleneck_details: details,
        });
    }

    Ok(results)
}

/// Calculates sizing for all Express (M7g) instance types that have a price
/// in `pricing`.
///
/// # Errors
///
/// Returns the legacy message when `discount_pct` is outside `[0, 100)`.
pub fn calculate_express_sizing(
    inputs: &SizingInputs,
    pricing: &Pricing,
    discount_pct: f64,
) -> Result<Vec<SizingResult>, String> {
    let discount = discount_multiplier(discount_pct)?;
    let mut results = Vec::new();

    let monthly_cross_az_cost = cross_az_cost(inputs, pricing, discount);

    let data_in_gb_mo = inputs.avg_data_in_mbs * 3600.0 * HOURS_PER_MONTH / 1024.0;
    let monthly_data_in_cost = data_in_gb_mo * pricing.express_data_in_per_gb * discount;

    let express_storage_gb =
        inputs.avg_data_in_mbs * inputs.retention_hours as f64 * 3600.0 / 1024.0;
    let monthly_express_storage_cost =
        express_storage_gb * pricing.express_storage_cost_per_gb_month * discount;

    for spec in INSTANCE_SPECS {
        let Capacity::Express { ingress_mbs } = spec.capacity else {
            continue;
        };
        let Some(price_per_hr) = pricing.broker_price_per_hr(spec.name) else {
            continue;
        };

        let util = inputs.utilization_express;
        let ingress_capacity_per_broker = ingress_mbs * util;
        let egress_capacity_per_broker = ingress_mbs * EXPRESS_EGRESS_FACTOR;
        let limit = partition_limit(spec, inputs.use_max_partitions);

        let details = vec![
            BottleneckDetail {
                name: "ingress",
                brokers_needed: brokers_for(inputs.peak_data_in_mbs, ingress_capacity_per_broker),
                demand: inputs.peak_data_in_mbs,
                per_broker_capacity: ingress_capacity_per_broker,
                unit: "MiB/s peak ingress (after util)".to_string(),
            },
            BottleneckDetail {
                name: "egress",
                brokers_needed: brokers_for(inputs.peak_data_out_mbs, egress_capacity_per_broker),
                demand: inputs.peak_data_out_mbs,
                per_broker_capacity: egress_capacity_per_broker,
                unit: "MiB/s peak egress".to_string(),
            },
            BottleneckDetail {
                name: "partitions",
                brokers_needed: brokers_for(inputs.num_partitions as f64, limit as f64),
                demand: inputs.num_partitions as f64,
                per_broker_capacity: limit as f64,
                unit: partition_unit(inputs.use_max_partitions),
            },
        ];

        let (broker_count, bottleneck) = pick_bottleneck(&details);
        let monthly_broker_cost = broker_count as f64 * price_per_hr * HOURS_PER_MONTH * discount;

        results.push(SizingResult {
            instance_type: spec.name,
            broker_count,
            bottleneck,
            monthly_broker_cost,
            monthly_ebs_cost: monthly_express_storage_cost,
            monthly_ts_cost: 0.0,
            monthly_data_in_cost,
            monthly_cross_az_cost,
            monthly_pst_cost: 0.0,
            total_monthly_cost: monthly_broker_cost
                + monthly_express_storage_cost
                + monthly_data_in_cost
                + monthly_cross_az_cost,
            bottleneck_details: details,
        });
    }

    Ok(results)
}

/// Picks the cheapest result per class whose broker count fits within
/// `broker_quota`; `None` when no instance of the class fits. The returned
/// list has the three classes in report order.
#[must_use]
pub fn recommend_per_class(
    results: &[SizingResult],
    broker_quota: i64,
) -> Vec<(BrokerClass, Option<&SizingResult>)> {
    [
        BrokerClass::M5Standard,
        BrokerClass::M7gStandard,
        BrokerClass::Express,
    ]
    .into_iter()
    .map(|class| {
        let mut best: Option<&SizingResult> = None;
        for result in results {
            if classify(result.instance_type) != Some(class) || result.broker_count > broker_quota {
                continue;
            }
            if best.is_none_or(|current| result.total_monthly_cost < current.total_monthly_cost) {
                best = Some(result);
            }
        }
        (class, best)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::static_pricing;

    fn approx(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    fn workload() -> SizingInputs {
        SizingInputs {
            avg_data_in_mbs: 10.0,
            peak_data_in_mbs: 20.0,
            avg_data_out_mbs: 30.0,
            peak_data_out_mbs: 40.0,
            num_partitions: 3000,
            replication_factor: 3,
            retention_hours: 168,
            primary_retention_hours: 24,
            utilization_standard: 0.50,
            utilization_express: 0.75,
            pst_per_broker_mbs: None,
            use_max_partitions: false,
            rack_affined_consumers: true,
        }
    }

    fn pricing() -> Pricing {
        static_pricing(
            "us-east-1",
            &[BrokerGroup::Standard, BrokerGroup::Express],
            &mut Vec::new(),
        )
        .expect("static pricing")
    }

    fn standard(inputs: &SizingInputs, discount: f64) -> Vec<SizingResult> {
        calculate_standard_sizing(inputs, &pricing(), discount).expect("standard sizing")
    }

    fn find<'a>(results: &'a [SizingResult], name: &str) -> &'a SizingResult {
        results
            .iter()
            .find(|result| result.instance_type == name)
            .unwrap_or_else(|| panic!("no result for {name}"))
    }

    #[test]
    fn brokers_are_rounded_up_to_a_multiple_of_three_azs() {
        assert_eq!(brokers_for(10.0, 5.0), 3);
        assert_eq!(brokers_for(15.0, 5.0), 3);
        assert_eq!(brokers_for(16.0, 5.0), 6);
        assert_eq!(brokers_for(20.0, 5.0), 6);
        assert_eq!(brokers_for(21.0, 5.0), 9);
        assert_eq!(brokers_for(1.0, 1000.0), 3);
        assert_eq!(brokers_for(0.0, 5.0), 0);
    }

    #[test]
    fn standard_sizing_matches_the_hand_computed_worked_example() {
        let results = standard(&workload(), 0.0);
        // 15 Standard instance types, in table order.
        assert_eq!(results.len(), 15);
        assert_eq!(results[0].instance_type, "kafka.m5.large");
        assert_eq!(results[14].instance_type, "kafka.m7g.16xlarge");

        let m5 = find(&results, "kafka.m5.large");
        // Tiered Storage in use: storage factor 8.0, network factor
        // 2 * 1.5 + 7.2 = 10.2; NIC 96 / 10.2 beats EBS 81 / 8; x 0.5 util.
        assert_eq!(m5.broker_count, 6);
        assert_eq!(m5.bottleneck, "ingress");
        let ingress = &m5.bottleneck_details[0];
        assert_eq!((ingress.name, ingress.brokers_needed), ("ingress", 6));
        approx(ingress.per_broker_capacity, 96.0 / 10.2 * 0.5);
        approx(ingress.demand, 20.0);
        assert_eq!(ingress.unit, "MiB/s peak ingress (after util)");
        let names: Vec<&str> = m5.bottleneck_details.iter().map(|d| d.name).collect();
        assert_eq!(names, ["ingress", "egress", "partitions", "storage"]);
        assert_eq!(m5.bottleneck_details[1].brokers_needed, 6);
        assert_eq!(m5.bottleneck_details[2].brokers_needed, 3);
        assert_eq!(m5.bottleneck_details[2].unit, "partitions (rec)");
        assert_eq!(m5.bottleneck_details[3].brokers_needed, 3);
        approx(m5.bottleneck_details[3].demand, 5062.5);
        approx(m5.monthly_broker_cost, 6.0 * 0.21 * 730.0);
        approx(m5.monthly_ebs_cost, 506.25);
        approx(m5.monthly_ts_cost, 303.75);
        approx(m5.monthly_cross_az_cost, 342.1875);
        approx(m5.monthly_pst_cost, 0.0);
        approx(m5.monthly_data_in_cost, 0.0);
        approx(m5.total_monthly_cost, 919.8 + 506.25 + 303.75 + 342.1875);
    }

    #[test]
    fn without_tiered_storage_the_io_factors_are_lower() {
        let mut inputs = workload();
        inputs.primary_retention_hours = inputs.retention_hours;
        let results = standard(&inputs, 0.0);
        let m5 = find(&results, "kafka.m5.large");
        // network factor 3.0 + 5.7 = 8.7 -> 96 / 8.7 * 0.5 = 5.517 -> 3.625 -> 4 -> 6
        assert_eq!(m5.broker_count, 6);
        approx(
            m5.bottleneck_details[0].per_broker_capacity,
            96.0 / 8.7 * 0.5,
        );
        approx(m5.monthly_ts_cost, 0.0);
        // EBS holds the full retention: 10 * 168 * 3600 * 3 / 1024 * 2.
        approx(
            m5.bottleneck_details[3].demand,
            10.0 * 168.0 * 3600.0 * 3.0 / 1024.0 * 2.0,
        );
    }

    #[test]
    fn partition_limits_switch_between_recommended_and_maximum() {
        let mut inputs = workload();
        inputs.num_partitions = 12_000;
        let results = standard(&inputs, 0.0);
        let m5 = find(&results, "kafka.m5.large");
        assert_eq!((m5.broker_count, m5.bottleneck), (12, "partitions"));

        inputs.use_max_partitions = true;
        let results = standard(&inputs, 0.0);
        let m5 = find(&results, "kafka.m5.large");
        // 12000 / 1500 = 8 -> 9 brokers, still above the ingress need of 6.
        assert_eq!((m5.broker_count, m5.bottleneck), (9, "partitions"));
        assert_eq!(m5.bottleneck_details[2].unit, "partitions (max)");
        assert_eq!(m5.bottleneck_details[2].per_broker_capacity, 1500.0);
    }

    #[test]
    fn provisioned_storage_throughput_is_sized_clamped_and_priced() {
        let mut inputs = workload();
        inputs.pst_per_broker_mbs = Some(500.0);
        let results = standard(&inputs, 0.0);

        let big = find(&results, "kafka.m5.4xlarge");
        assert_eq!(big.broker_count, 3);
        let pst = big.bottleneck_details.last().expect("pst detail");
        assert_eq!((pst.name, pst.brokers_needed), ("pst", 3));
        assert_eq!(pst.per_broker_capacity, 500.0);
        assert_eq!(pst.demand, 30.0);
        approx(big.monthly_pst_cost, 3.0 * 500.0 * 0.08);
        approx(
            big.total_monthly_cost,
            big.monthly_broker_cost
                + big.monthly_ebs_cost
                + big.monthly_ts_cost
                + big.monthly_cross_az_cost
                + 120.0,
        );

        // Not PST eligible: no detail and no cost.
        let small = find(&results, "kafka.m5.2xlarge");
        assert!(small.bottleneck_details.iter().all(|d| d.name != "pst"));
        approx(small.monthly_pst_cost, 0.0);

        // The request is clamped to the instance's EBS maximum (593.75).
        inputs.pst_per_broker_mbs = Some(1000.0);
        let results = standard(&inputs, 0.0);
        let big = find(&results, "kafka.m5.4xlarge");
        assert_eq!(
            big.bottleneck_details
                .last()
                .expect("pst")
                .per_broker_capacity,
            593.75
        );
        approx(big.monthly_pst_cost, 3.0 * 593.75 * 0.08);
    }

    #[test]
    fn discount_applies_to_every_cost_dimension() {
        let mut inputs = workload();
        inputs.pst_per_broker_mbs = Some(500.0);
        let list = standard(&inputs, 0.0);
        let discounted = standard(&inputs, 20.0);
        let (a, b) = (
            find(&list, "kafka.m5.4xlarge"),
            find(&discounted, "kafka.m5.4xlarge"),
        );
        approx(b.monthly_broker_cost, a.monthly_broker_cost * 0.8);
        approx(b.monthly_ebs_cost, a.monthly_ebs_cost * 0.8);
        approx(b.monthly_ts_cost, a.monthly_ts_cost * 0.8);
        approx(b.monthly_cross_az_cost, a.monthly_cross_az_cost * 0.8);
        approx(b.monthly_pst_cost, a.monthly_pst_cost * 0.8);
        assert_eq!(
            a.broker_count, b.broker_count,
            "discount never changes sizing"
        );

        approx(discount_multiplier(0.0).expect("ok"), 1.0);
        approx(discount_multiplier(15.0).expect("ok"), 0.85);
        assert_eq!(
            discount_multiplier(100.0),
            Err("discount_pct must be >= 0 and < 100; got 100.0".to_string())
        );
        assert_eq!(
            discount_multiplier(-1.0),
            Err("discount_pct must be >= 0 and < 100; got -1.0".to_string())
        );
        assert!(calculate_standard_sizing(&workload(), &pricing(), 100.0).is_err());
        assert!(calculate_express_sizing(&workload(), &pricing(), -5.0).is_err());
    }

    #[test]
    fn consumer_rack_awareness_only_changes_the_cross_az_cost() {
        let mut inputs = workload();
        let affined = standard(&inputs, 0.0);
        inputs.rack_affined_consumers = false;
        let across = standard(&inputs, 0.0);
        // (10 + 30) * 2/3 MiB/s -> 68437.5 GiB/month at $0.02.
        approx(
            find(&across, "kafka.m5.large").monthly_cross_az_cost,
            1368.75,
        );
        approx(
            find(&affined, "kafka.m5.large").monthly_cross_az_cost,
            342.1875,
        );
        assert_eq!(
            find(&across, "kafka.m5.large").broker_count,
            find(&affined, "kafka.m5.large").broker_count
        );
    }

    #[test]
    fn express_sizing_uses_ingress_egress_and_partition_limits() {
        let results =
            calculate_express_sizing(&workload(), &pricing(), 0.0).expect("express sizing");
        assert_eq!(results.len(), 7);
        let large = &results[0];
        assert_eq!(large.instance_type, "kafka.m7g.large (Express)");
        // ingress 15.625 * 0.75 = 11.71875 -> 1.7 -> 2 -> 3; egress capacity
        // 15.625 * 2.5 -> 1.02 -> 2 -> 3; partitions 3000 / 1000 = 3.
        assert_eq!(large.broker_count, 3);
        assert_eq!(large.bottleneck, "ingress");
        assert_eq!(large.bottleneck_details.len(), 3);
        approx(large.bottleneck_details[1].per_broker_capacity, 39.0625);
        approx(large.monthly_broker_cost, 3.0 * 0.408 * 730.0);
        approx(
            large.monthly_ebs_cost,
            10.0 * 168.0 * 3600.0 / 1024.0 * 0.10,
        );
        approx(
            large.monthly_data_in_cost,
            10.0 * 3600.0 * 730.0 / 1024.0 * 0.01,
        );
        approx(large.monthly_cross_az_cost, 342.1875);
        approx(large.monthly_ts_cost, 0.0);
        approx(large.monthly_pst_cost, 0.0);
        approx(
            large.total_monthly_cost,
            large.monthly_broker_cost
                + large.monthly_ebs_cost
                + large.monthly_data_in_cost
                + large.monthly_cross_az_cost,
        );
    }

    #[test]
    fn express_utilization_changes_the_ingress_requirement() {
        let mut inputs = workload();
        inputs.peak_data_in_mbs = 60.0;
        inputs.utilization_express = 0.5;
        let results = calculate_express_sizing(&inputs, &pricing(), 0.0).expect("sizing");
        let large = &results[0];
        // 60 / (15.625 * 0.5) = 7.68 -> 8 -> 9 brokers.
        assert_eq!(large.bottleneck_details[0].brokers_needed, 9);
        assert_eq!((large.broker_count, large.bottleneck), (9, "ingress"));
    }

    #[test]
    fn instances_without_a_price_are_excluded() {
        let mut priced = pricing();
        priced
            .broker_price_per_hr
            .retain(|(name, _)| *name != "kafka.m5.large");
        let results = calculate_standard_sizing(&workload(), &priced, 0.0).expect("sizing");
        assert_eq!(results.len(), 14);
        assert!(results.iter().all(|r| r.instance_type != "kafka.m5.large"));
    }

    #[test]
    fn recommendations_pick_the_cheapest_instance_within_the_quota() {
        let result = |name: &'static str, brokers: i64, total: f64| SizingResult {
            instance_type: name,
            broker_count: brokers,
            bottleneck: "ingress",
            monthly_broker_cost: 0.0,
            monthly_ebs_cost: 0.0,
            monthly_ts_cost: 0.0,
            monthly_data_in_cost: 0.0,
            monthly_cross_az_cost: 0.0,
            monthly_pst_cost: 0.0,
            total_monthly_cost: total,
            bottleneck_details: Vec::new(),
        };
        let results = vec![
            result("kafka.m5.large", 63, 100.0),
            result("kafka.m5.xlarge", 30, 300.0),
            result("kafka.m5.2xlarge", 15, 200.0),
            result("kafka.m7g.large", 61, 50.0),
            result("kafka.m7g.xlarge (Express)", 60, 900.0),
            result("kafka.m7g.2xlarge (Express)", 30, 900.0),
        ];
        let recommendations = recommend_per_class(&results, 60);
        let picks: Vec<(BrokerClass, Option<&str>)> = recommendations
            .iter()
            .map(|(class, pick)| (*class, pick.map(|r| r.instance_type)))
            .collect();
        assert_eq!(
            picks,
            [
                // m5.large exceeds the quota, so the cheapest eligible is 2xlarge.
                (BrokerClass::M5Standard, Some("kafka.m5.2xlarge")),
                // Nothing of the class fits within 60 brokers.
                (BrokerClass::M7gStandard, None),
                // Equal cost: the first in table order wins, like Python's min.
                (BrokerClass::Express, Some("kafka.m7g.xlarge (Express)")),
            ]
        );
        let relaxed = recommend_per_class(&results, 100);
        assert_eq!(
            relaxed[1].1.map(|r| r.instance_type),
            Some("kafka.m7g.large")
        );
    }

    #[test]
    fn inputs_validation_keeps_the_legacy_messages() {
        let check = |mutate: &dyn Fn(&mut SizingInputs)| {
            let mut inputs = workload();
            mutate(&mut inputs);
            inputs.validate().expect_err("must be rejected")
        };
        assert!(workload().validate().is_ok());
        assert_eq!(
            check(&|i| i.avg_data_in_mbs = 0.0),
            "avg_data_in_mbs must be > 0; got 0.0"
        );
        assert_eq!(
            check(&|i| i.peak_data_out_mbs = -2.5),
            "peak_data_out_mbs must be > 0; got -2.5"
        );
        assert_eq!(
            check(&|i| i.peak_data_in_mbs = 5.0),
            "peak_data_in_mbs (5.0) must be >= avg_data_in_mbs (10.0)"
        );
        assert_eq!(
            check(&|i| i.peak_data_out_mbs = 29.0),
            "peak_data_out_mbs (29.0) must be >= avg_data_out_mbs (30.0)"
        );
        assert_eq!(
            check(&|i| i.replication_factor = 4),
            "replication_factor must be 2 or 3; got 4. \
             Express always uses RF=3 internally regardless of this value."
        );
        assert_eq!(
            check(&|i| i.primary_retention_hours = 200),
            "primary_retention_hours (200) must be <= retention_hours (168)"
        );
        assert_eq!(
            check(&|i| i.pst_per_broker_mbs = Some(249.0)),
            "pst_per_broker_mbs must be between 250 and 1000 MiB/s; got 249.0"
        );
        assert_eq!(
            check(&|i| i.pst_per_broker_mbs = Some(1000.5)),
            "pst_per_broker_mbs must be between 250 and 1000 MiB/s; got 1000.5"
        );
        // Boundaries are accepted.
        let mut edge = workload();
        edge.pst_per_broker_mbs = Some(250.0);
        assert!(edge.validate().is_ok());
        edge.pst_per_broker_mbs = Some(1000.0);
        edge.replication_factor = 2;
        edge.peak_data_in_mbs = edge.avg_data_in_mbs;
        edge.primary_retention_hours = edge.retention_hours;
        assert!(edge.validate().is_ok());
    }

    #[test]
    fn inputs_validation_replaces_python_crashes_with_messages() {
        // Intentional change: the script crashed (ZeroDivisionError) or
        // produced nan results for these.
        let check = |mutate: &dyn Fn(&mut SizingInputs)| {
            let mut inputs = workload();
            mutate(&mut inputs);
            inputs.validate().expect_err("must be rejected")
        };
        assert_eq!(
            check(&|i| i.avg_data_in_mbs = f64::NAN),
            "avg_data_in_mbs must be a finite number; got nan"
        );
        assert_eq!(
            check(&|i| i.peak_data_in_mbs = f64::INFINITY),
            "peak_data_in_mbs must be a finite number; got inf"
        );
        assert_eq!(
            check(&|i| i.utilization_standard = 0.0),
            "utilization_standard must be a finite number > 0; got 0.0"
        );
        assert_eq!(
            check(&|i| i.utilization_express = -0.1),
            "utilization_express must be a finite number > 0; got -0.1"
        );
    }

    #[test]
    fn broker_class_selection_parses_like_the_legacy_option() {
        assert_eq!(
            parse_broker_classes("standard"),
            Ok(vec![BrokerGroup::Standard])
        );
        assert_eq!(
            parse_broker_classes("express"),
            Ok(vec![BrokerGroup::Express])
        );
        assert_eq!(
            parse_broker_classes(" Express , standard,express,,"),
            Ok(vec![BrokerGroup::Express, BrokerGroup::Standard])
        );
        assert_eq!(
            parse_broker_classes("standard,express"),
            Ok(vec![BrokerGroup::Standard, BrokerGroup::Express])
        );
        assert_eq!(
            parse_broker_classes(""),
            Err("--broker-classes requires at least one of: express, standard".to_string())
        );
        assert_eq!(
            parse_broker_classes(" , "),
            Err("--broker-classes requires at least one of: express, standard".to_string())
        );
        assert_eq!(
            parse_broker_classes("standard, Gold"),
            Err(
                "invalid broker class 'Gold'; choose from: express, standard \
                 (comma-separate to select both)"
                    .to_string()
            )
        );
    }

    #[test]
    fn discount_option_parses_like_the_legacy_option() {
        assert_eq!(parse_discount_pct("0"), Ok(0.0));
        assert_eq!(parse_discount_pct("15"), Ok(15.0));
        assert_eq!(parse_discount_pct(" 99.99 "), Ok(99.99));
        assert_eq!(
            parse_discount_pct("100"),
            Err("discount percentage must be >= 0 and < 100; got 100".to_string())
        );
        assert_eq!(
            parse_discount_pct("-0.5"),
            Err("discount percentage must be >= 0 and < 100; got -0.5".to_string())
        );
        assert_eq!(
            parse_discount_pct("nan"),
            Err("discount percentage must be >= 0 and < 100; got nan".to_string())
        );
        assert_eq!(
            parse_discount_pct("ten"),
            Err("invalid discount percentage 'ten'; expected a number".to_string())
        );
    }

    #[test]
    fn instance_table_is_complete_and_consistent() {
        assert_eq!(INSTANCE_SPECS.len(), 22);
        let mut names: Vec<&str> = INSTANCE_SPECS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 22, "names are unique");
        for spec in INSTANCE_SPECS {
            assert!(spec.rec_partitions <= spec.max_partitions, "{}", spec.name);
            assert!(classify(spec.name).is_some(), "{}", spec.name);
            assert_eq!(
                matches!(spec.capacity, Capacity::Express { .. }),
                spec.name.contains("Express")
            );
        }
        assert_eq!(PST_ELIGIBLE.len(), 9);
        for name in PST_ELIGIBLE {
            assert!(names.contains(name), "{name} is in the table");
            assert!(!name.contains("Express"));
        }
        assert!(is_pst_eligible("kafka.m7g.4xlarge"));
        assert!(!is_pst_eligible("kafka.m7g.2xlarge"));
        assert!(!is_pst_eligible("kafka.m7g.4xlarge (Express)"));
        // Spot values from the published capacity tables.
        let m5_24 = INSTANCE_SPECS
            .iter()
            .find(|s| s.name == "kafka.m5.24xlarge")
            .expect("row");
        assert_eq!((m5_24.rec_partitions, m5_24.max_partitions), (4000, 4000));
        assert_eq!(
            m5_24.capacity,
            Capacity::Standard {
                ebs_throughput_mbs: 1000.0,
                network_throughput_mbs: 3200.0
            }
        );
    }

    #[test]
    fn classification_groups_instances() {
        assert_eq!(classify("kafka.m5.large"), Some(BrokerClass::M5Standard));
        assert_eq!(classify("kafka.m7g.large"), Some(BrokerClass::M7gStandard));
        assert_eq!(
            classify("kafka.m7g.large (Express)"),
            Some(BrokerClass::Express)
        );
        assert_eq!(classify("kafka.t3.small"), None);
        assert_eq!(BrokerClass::M5Standard.label(), "Standard M5");
        assert_eq!(BrokerGroup::Standard.classes().len(), 2);
        assert_eq!(BrokerGroup::Express.classes(), [BrokerClass::Express]);
    }
}
