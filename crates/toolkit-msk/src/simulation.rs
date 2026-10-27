//! Deterministic compute and guardrail engine for the MSK Express load-test
//! simulation (port of `simulation_load_test_config.py`).
//!
//! Three actions:
//!
//! - [`compute`]: given cluster sizing (instance type, broker count) derive
//!   cluster capacity, the auto-sized client fleet and per-test default
//!   parameters and bounds.
//! - [`render_template`]: bake the sizing and the computed fleet into a copy of
//!   the `CloudFormation` template as parameter `Default:` values.
//! - [`validate`]: given a test type and a proposed config return PASS/REJECT
//!   plus inline warnings.
//!
//! Throughput is MB/s as in the legacy script (the Express quota tables are
//! published in MB/s); the rendered template carries the same numbers.

use toolkit_core::Json;

use crate::pyfmt::{py_float_repr, py_repr, py_str_repr, round_to};

/// The static `CloudFormation` template (the skill's `assets/simulation-stack.yaml`),
/// compiled in so the tool needs no skill directory at run time.
pub const SOURCE_TEMPLATE: &str = include_str!(
    "../../../skills/specialized-skills/analytics-skills/migrate-to-msk/assets/simulation-stack.yaml"
);

/// Express per-broker INGRESS throttle limits in MB/s: `(sustained, max_quota)`.
/// Sustained is the recommended threshold (no degradation up to there); the max
/// quota is the hard ceiling (the cluster throttles traffic beyond it).
pub const PER_BROKER_INGRESS: &[(&str, f64, f64)] = &[
    ("express.m7g.large", 15.6, 23.4),
    ("express.m7g.xlarge", 31.2, 46.8),
    ("express.m7g.2xlarge", 62.5, 93.7),
    ("express.m7g.4xlarge", 124.9, 187.5),
    ("express.m7g.8xlarge", 250.0, 375.0),
    ("express.m7g.12xlarge", 375.0, 562.5),
    ("express.m7g.16xlarge", 500.0, 750.0),
];

/// The only valid Express version and metadata-mode combinations, as the exact
/// `CloudFormation` `KafkaVersion` strings the template accepts.
pub const SUPPORTED_KAFKA_VERSIONS: [&str; 3] = ["3.6.0", "3.8.x", "3.9.x.kraft"];

/// Client fleet instance type: one size for all clusters, only the count varies.
pub const FLEET_INSTANCE_TYPE: &str = "c5.2xlarge";
/// Conservative per-node producer throughput target, MB/s.
pub const FLEET_NODE_THROUGHPUT_MBPS: i64 = 400;

/// Largest producer fleet [`compute`] accepts: the biggest count whose
/// throughput (count x [`FLEET_NODE_THROUGHPUT_MBPS`]) still fits an `i64`.
pub const MAX_FLEET_PRODUCERS: i64 = i64::MAX / FLEET_NODE_THROUGHPUT_MBPS;

/// Default instance type of the `compute` and `validate` actions.
pub const DEFAULT_INSTANCE_TYPE: &str = "express.m7g.4xlarge";
/// Default broker count of the `compute` and `validate` actions.
pub const DEFAULT_BROKER_COUNT: i64 = 12;

/// Derived cluster capacity, client fleet and per-test defaults and bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct Caps {
    /// Express instance type.
    pub instance_type: String,
    /// Broker count (a multiple of 3).
    pub broker_count: i64,
    /// Cluster sustained ingress, MB/s (one decimal).
    pub cluster_sustained_ingress_mbps: f64,
    /// Cluster maximum ingress quota, MB/s (one decimal).
    pub cluster_max_ingress_mbps: f64,
    /// Producer fleet size.
    pub fleet_producer_count: i64,
    /// Consumer fleet size.
    pub fleet_consumer_count: i64,
    /// Maximum fleet throughput, MB/s.
    pub fleet_max_mbps: i64,
    /// Default target throughput for the broker restart test, MB/s.
    pub default_target_throughput_mbps: i64,
}

/// Python `round(x)` for a float: round half to even, as an integer.
fn round_half_even(value: f64) -> i64 {
    value.round_ties_even() as i64
}

/// Derives all values for `instance_type` and `broker_count`.
///
/// # Errors
///
/// Returns the legacy message for an unknown instance type, a broker count
/// that is not a multiple of 3 (or below 3), an unsupported `kafka_version`
/// (only checked when supplied; 3.9 is `KRaft`-only on Express) or a broker
/// count so large that the fleet size or its throughput does not fit an `i64`
/// (Python's unbounded integers have no such limit; this is an intentional
/// change recorded in `docs/migration/msk-contracts.md`).
pub fn compute(
    instance_type: &str,
    broker_count: i64,
    kafka_version: Option<&str>,
) -> Result<Caps, String> {
    let Some((_, per_broker_sustained, per_broker_max)) = PER_BROKER_INGRESS
        .iter()
        .find(|(name, _, _)| *name == instance_type)
    else {
        let valid = Json::Array(
            PER_BROKER_INGRESS
                .iter()
                .map(|(name, _, _)| Json::string(*name))
                .collect(),
        );
        return Err(format!(
            "unknown instance_type {}; valid: {}",
            py_str_repr(instance_type),
            py_repr(&valid)
        ));
    };
    if broker_count < 3 || broker_count % 3 != 0 {
        return Err("broker_count must be a multiple of 3 and >= 3".to_string());
    }
    // Only validated when supplied: the sizing math does not need it, but
    // rejecting an unsupported value keeps a freeform answer from reaching
    // create-stack. 3.9 ZooKeeper (e.g. "3.9.x") is intentionally not in the set.
    if let Some(version) = kafka_version {
        if !SUPPORTED_KAFKA_VERSIONS.contains(&version) {
            let valid = Json::Array(
                SUPPORTED_KAFKA_VERSIONS
                    .iter()
                    .map(|version| Json::string(*version))
                    .collect(),
            );
            return Err(format!(
                "unsupported kafka_version {}; valid: {} (3.9 is KRaft-only on Express)",
                py_str_repr(version),
                py_repr(&valid)
            ));
        }
    }

    let count = broker_count as f64;
    let cluster_sustained_ingress = round_to(per_broker_sustained * count, 1);
    let cluster_max_ingress = round_to(per_broker_max * count, 1);
    // Provision 1.5x the cluster's MAXIMUM ingress quota so the fleet can drive
    // the cluster to its throttle ceiling with headroom and never be the
    // bottleneck, even when deliberately probing past the limit.
    // Python ints do not overflow; i64 does. Refuse a broker count whose fleet
    // (and the MB/s it drives) is not representable instead of saturating the
    // float-to-integer cast or wrapping the product (#4541).
    let fleet_float = (cluster_max_ingress * 1.5 / FLEET_NODE_THROUGHPUT_MBPS as f64).ceil();
    if fleet_float.is_nan() || fleet_float > MAX_FLEET_PRODUCERS as f64 {
        return Err(format!(
            "broker_count {broker_count} is too large: the producer fleet would exceed \
             {MAX_FLEET_PRODUCERS} nodes"
        ));
    }
    let fleet_producer_count = std::cmp::max(2, fleet_float as i64);
    let Some(fleet_max_mbps) = fleet_producer_count.checked_mul(FLEET_NODE_THROUGHPUT_MBPS) else {
        return Err(format!(
            "broker_count {broker_count} is too large: the fleet throughput is not representable"
        ));
    };
    Ok(Caps {
        instance_type: instance_type.to_string(),
        broker_count,
        cluster_sustained_ingress_mbps: cluster_sustained_ingress,
        cluster_max_ingress_mbps: cluster_max_ingress,
        fleet_producer_count,
        fleet_consumer_count: fleet_producer_count,
        fleet_max_mbps,
        default_target_throughput_mbps: round_half_even(cluster_sustained_ingress * 0.8),
    })
}

fn bounds(default: i64, min: i64, max: Option<i64>) -> Json {
    Json::object(vec![
        ("default", Json::Int(default)),
        ("min", Json::Int(min)),
        ("max", max.map_or(Json::Null, Json::Int)),
    ])
}

impl Caps {
    /// The `compute` document, with the legacy key order.
    #[must_use]
    pub fn to_json(&self) -> Json {
        let fleet_max = self.fleet_max_mbps;
        let producers = self.fleet_producer_count;
        let consumers = self.fleet_consumer_count;
        Json::object(vec![
            ("instance_type", Json::string(self.instance_type.clone())),
            ("broker_count", Json::Int(self.broker_count)),
            (
                "supported_kafka_versions",
                Json::Array(
                    SUPPORTED_KAFKA_VERSIONS
                        .iter()
                        .map(|version| Json::string(*version))
                        .collect(),
                ),
            ),
            (
                "cluster_sustained_ingress_mbps",
                Json::Float(self.cluster_sustained_ingress_mbps),
            ),
            (
                "cluster_max_ingress_mbps",
                Json::Float(self.cluster_max_ingress_mbps),
            ),
            ("fleet_instance_type", Json::string(FLEET_INSTANCE_TYPE)),
            (
                "fleet_instance_bandwidth_mbps",
                Json::Int(FLEET_NODE_THROUGHPUT_MBPS),
            ),
            ("fleet_producer_count", Json::Int(producers)),
            ("fleet_consumer_count", Json::Int(consumers)),
            ("fleet_max_mbps", Json::Int(fleet_max)),
            (
                "default_target_throughput_mbps",
                Json::Int(self.default_target_throughput_mbps),
            ),
            (
                "tests",
                Json::object(vec![
                    (
                        "e2e_latency",
                        Json::object(vec![
                            ("throughput_mbps", bounds(10, 1, Some(fleet_max))),
                            ("duration_minutes", bounds(10, 5, Some(120))),
                            ("num_producers", bounds(1, 1, Some(producers))),
                            ("num_consumers", bounds(1, 1, Some(consumers))),
                        ]),
                    ),
                    (
                        "broker_restart",
                        Json::object(vec![
                            (
                                "target_throughput_mbps",
                                bounds(self.default_target_throughput_mbps, 1, Some(fleet_max)),
                            ),
                            ("duration_minutes", bounds(15, 10, Some(120))),
                            // max = duration - 2
                            ("reboot_at_minute", bounds(5, 2, None)),
                            ("num_producers", bounds(producers, 1, Some(producers))),
                        ]),
                    ),
                ]),
            ),
        ])
    }
}

/// Sets the `Default:` of a top-level `CloudFormation` parameter in place.
///
/// The template is static and authored with each parameter at 2-space indent
/// and its properties at 4-space indent. The parameter block is located and its
/// existing `Default:` line rewritten (every baked parameter already declares
/// one), so template drift fails loudly instead of silently producing an
/// unsized stack.
///
/// # Errors
///
/// Returns the legacy message when the parameter or its `Default:` line is
/// missing.
pub fn set_param_default(
    template_text: &str,
    param_name: &str,
    value: &str,
) -> Result<String, String> {
    let lines: Vec<&str> = template_text.lines().collect();
    let header = format!("  {param_name}:");
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut replaced = false;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        out.push(line.to_string());
        index += 1;
        if line != header {
            continue;
        }
        // Walk this parameter's property lines (indented deeper than 2 spaces).
        let mut block_done = false;
        while index < lines.len() {
            let prop = lines[index];
            let stripped = prop.trim_start_matches(' ');
            let indent = prop.len() - stripped.len();
            // End of block: a non-blank line indented at most 2 spaces.
            if !prop.trim().is_empty() && indent <= 2 {
                break;
            }
            if stripped.starts_with("Default:") && !block_done {
                out.push(format!("{}Default: {value}", " ".repeat(indent)));
                replaced = true;
                block_done = true;
            } else {
                out.push(prop.to_string());
            }
            index += 1;
        }
    }
    if !replaced {
        return Err(format!(
            "could not set Default for parameter {}; the source template \
             may have drifted (expected a 2-space-indented block with a Default line)",
            py_str_repr(param_name)
        ));
    }
    let mut text = out.join("\n");
    if template_text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// The result of rendering the template.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// The filled template text.
    pub template: String,
    /// Derived capacities (fleet counts etc.).
    pub caps: Caps,
    /// The parameter defaults that were baked, in order.
    pub baked_parameters: Vec<(&'static str, Json)>,
}

/// Bakes the customer's sizing and the computed fleet into `template_text`.
///
/// # Errors
///
/// Returns the [`compute`] errors (a `kafka_version` is mandatory here) or a
/// template drift error.
pub fn render_template(
    template_text: &str,
    instance_type: &str,
    broker_count: i64,
    kafka_version: &str,
) -> Result<Rendered, String> {
    let caps = compute(instance_type, broker_count, Some(kafka_version))?;
    let baked: Vec<(&'static str, Json)> = vec![
        ("InstanceType", Json::string(instance_type)),
        ("BrokerCount", Json::Int(broker_count)),
        ("KafkaVersion", Json::string(kafka_version)),
        ("ClientInstanceType", Json::string(FLEET_INSTANCE_TYPE)),
        ("ProducerCount", Json::Int(caps.fleet_producer_count)),
        ("ConsumerCount", Json::Int(caps.fleet_consumer_count)),
    ];
    let mut text = template_text.to_string();
    for (name, value) in &baked {
        let default = match value {
            Json::String(text) => text.clone(),
            other => other.to_compact_string(),
        };
        text = set_param_default(&text, name, &default)?;
    }
    Ok(Rendered {
        template: text,
        caps,
        baked_parameters: baked,
    })
}

/// The `render` result document for a template written to `output_path`.
#[must_use]
pub fn render_result_json(rendered: &Rendered, output_path: &str) -> Json {
    Json::object(vec![
        ("output_path", Json::string(output_path)),
        (
            "deploy_command",
            Json::string(format!(
                "aws cloudformation create-stack --stack-name msk-express-simulation \
                 --template-body file://{output_path} --capabilities CAPABILITY_IAM \
                 --region <region>"
            )),
        ),
        (
            "baked_parameters",
            Json::Object(
                rendered
                    .baked_parameters
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), value.clone()))
                    .collect(),
            ),
        ),
        (
            "fleet_producer_count",
            Json::Int(rendered.caps.fleet_producer_count),
        ),
        (
            "fleet_consumer_count",
            Json::Int(rendered.caps.fleet_consumer_count),
        ),
        ("fleet_instance_type", Json::string(FLEET_INSTANCE_TYPE)),
    ])
}

/// A number from a proposed test config, keeping Python's int/float
/// distinction for message text.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Num {
    Int(i64),
    /// An integer result that left the `i64` range (Python ints are unbounded),
    /// for example `duration_minutes - 2` at the signed minimum (#4540).
    Wide(i128),
    Float(f64),
}

impl Num {
    fn as_f64(self) -> f64 {
        match self {
            Self::Int(number) => number as f64,
            Self::Wide(number) => number as f64,
            Self::Float(number) => number,
        }
    }

    /// The exact value of an integer, so integer comparisons never round.
    fn as_i128(self) -> Option<i128> {
        match self {
            Self::Int(number) => Some(i128::from(number)),
            Self::Wide(number) => Some(number),
            Self::Float(_) => None,
        }
    }

    /// `self > other` like Python: exact between integers, `f64` otherwise.
    fn greater_than(self, other: Self) -> bool {
        match (self.as_i128(), other.as_i128()) {
            (Some(left), Some(right)) => left > right,
            _ => self.as_f64() > other.as_f64(),
        }
    }

    fn text(self) -> String {
        match self {
            Self::Int(number) => number.to_string(),
            Self::Wide(number) => number.to_string(),
            Self::Float(number) => py_float_repr(number),
        }
    }

    /// `self - amount` without overflow: an `i64` difference that does not fit
    /// widens to `i128` (an `i64` minus a small `amount` always fits there).
    fn minus_int(self, amount: i64) -> Self {
        match self {
            Self::Int(number) => Self::Wide(i128::from(number) - i128::from(amount)),
            Self::Wide(number) => Self::Wide(number - i128::from(amount)),
            Self::Float(number) => Self::Float(number - amount as f64),
        }
    }
}

fn config_number(config: &Json, key: &str) -> Result<Num, String> {
    match config.get(key) {
        None => Err(py_str_repr(key)),
        Some(Json::Int(number)) => Ok(Num::Int(*number)),
        Some(Json::Float(number)) => Ok(Num::Float(*number)),
        Some(other) => Err(format!("{key} must be a number, not {}", other.type_name())),
    }
}

fn check_range(name: &str, value: Num, lo: i64, hi: Option<Num>, errors: &mut Vec<String>) {
    let below = Num::Int(lo).greater_than(value);
    let above = hi.is_some_and(|bound| value.greater_than(bound));
    if below || above {
        errors.push(format!(
            "{name}={} is outside the allowed range [{lo}, {}]",
            value.text(),
            hi.map_or_else(|| "None".to_string(), Num::text)
        ));
    }
}

/// Tiered throughput warning. Returns true if a degradation or throttle
/// warning fired: above the max quota the cluster THROTTLES (hard ceiling),
/// above the sustained rate it DEGRADES (elevated latency) without throttling.
fn throughput_warnings(
    name: &str,
    throughput: Num,
    sustained: f64,
    max_quota: f64,
    warnings: &mut Vec<String>,
) -> bool {
    let tput = throughput.as_f64();
    let shown = throughput.text();
    if tput > max_quota {
        warnings.push(format!(
            "{name}={shown} MB/s exceeds the cluster's maximum ingress quota \
             ({} MB/s) \u{2014} MSK Express throttles read/write traffic beyond this, \
             so the cluster cannot actually sustain that rate.",
            py_float_repr(max_quota)
        ));
        return true;
    }
    if tput > sustained {
        warnings.push(format!(
            "{name}={shown} MB/s is above the cluster's sustained/recommended ingress \
             ({} MB/s); it can run but expect performance degradation \
             (elevated latency), with no throttling until the {} MB/s ceiling.",
            py_float_repr(sustained),
            py_float_repr(max_quota)
        ));
        return true;
    }
    false
}

/// Outcome of [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    /// Hard-rule violations; any entry means REJECT.
    pub errors: Vec<String>,
    /// Soft warnings the agent surfaces as one-sentence notes.
    pub warnings: Vec<String>,
}

impl Validation {
    /// `"REJECT"` when there are errors, otherwise `"PASS"`.
    #[must_use]
    pub fn decision(&self) -> &'static str {
        if self.errors.is_empty() {
            "PASS"
        } else {
            "REJECT"
        }
    }

    /// The `validate` document.
    #[must_use]
    pub fn to_json(&self) -> Json {
        let strings = |items: &[String]| Json::Array(items.iter().map(Json::string).collect());
        Json::object(vec![
            ("decision", Json::string(self.decision())),
            ("errors", strings(&self.errors)),
            ("warnings", strings(&self.warnings)),
        ])
    }
}

/// Validates a proposed test config against `caps`.
///
/// # Errors
///
/// Returns the message of a missing key (the quoted key, like the legacy
/// `KeyError`) or a key whose value is not a number.
pub fn validate(test: &str, config: &Json, caps: &Caps) -> Result<Validation, String> {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let fleet_max = Num::Int(caps.fleet_max_mbps);
    let sustained = caps.cluster_sustained_ingress_mbps;
    let max_quota = caps.cluster_max_ingress_mbps;
    let fleet_bw = FLEET_NODE_THROUGHPUT_MBPS as f64;
    let fleet_producers = Num::Int(caps.fleet_producer_count);

    match test {
        "e2e_latency" => {
            let tput = config_number(config, "throughput_mbps")?;
            let dur = config_number(config, "duration_minutes")?;
            let prod = config_number(config, "num_producers")?;
            let cons = config_number(config, "num_consumers")?;
            check_range("throughput_mbps", tput, 1, Some(fleet_max), &mut errors);
            check_range("duration_minutes", dur, 5, Some(Num::Int(120)), &mut errors);
            check_range("num_producers", prod, 1, Some(fleet_producers), &mut errors);
            check_range(
                "num_consumers",
                cons,
                1,
                Some(Num::Int(caps.fleet_consumer_count)),
                &mut errors,
            );
            if !throughput_warnings("throughput_mbps", tput, sustained, max_quota, &mut warnings)
                && tput.as_f64() > sustained * 0.6
            {
                warnings.push(format!(
                    "throughput_mbps={} is above 60% of sustained ingress; high load \
                     can inflate latency. For a clean signal, consider <= {} MB/s.",
                    tput.text(),
                    round_half_even(sustained * 0.5)
                ));
            }
            push_producer_warning(prod, tput, fleet_bw, &mut warnings);
        }
        "broker_restart" => {
            let tput = config_number(config, "target_throughput_mbps")?;
            let dur = config_number(config, "duration_minutes")?;
            let reboot = config_number(config, "reboot_at_minute")?;
            let prod = config_number(config, "num_producers")?;
            check_range(
                "target_throughput_mbps",
                tput,
                1,
                Some(fleet_max),
                &mut errors,
            );
            check_range(
                "duration_minutes",
                dur,
                10,
                Some(Num::Int(120)),
                &mut errors,
            );
            check_range(
                "reboot_at_minute",
                reboot,
                2,
                Some(dur.minus_int(2)),
                &mut errors,
            );
            check_range("num_producers", prod, 1, Some(fleet_producers), &mut errors);
            if !throughput_warnings(
                "target_throughput_mbps",
                tput,
                sustained,
                max_quota,
                &mut warnings,
            ) && tput.as_f64() > sustained * 0.6
            {
                warnings.push(format!(
                    "target_throughput_mbps={} is above 60% of sustained ingress; \
                     broker-CPU headroom during the restart is limited, so recovery may take longer.",
                    tput.text()
                ));
            }
            push_producer_warning(prod, tput, fleet_bw, &mut warnings);
        }
        other => errors.push(format!(
            "unknown test {}; valid: e2e_latency, broker_restart",
            py_str_repr(other)
        )),
    }

    Ok(Validation { errors, warnings })
}

fn push_producer_warning(prod: Num, tput: Num, fleet_bw: f64, warnings: &mut Vec<String>) {
    let needed = (tput.as_f64() / fleet_bw).ceil();
    if prod.as_f64() < needed {
        warnings.push(format!(
            "num_producers={} may not sustain {} MB/s; consider >= {} producers.",
            prod.text(),
            tput.text(),
            needed as i64
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    fn caps() -> Caps {
        compute(DEFAULT_INSTANCE_TYPE, DEFAULT_BROKER_COUNT, None).expect("default caps")
    }

    fn json(text: &str) -> Json {
        parse_json(text).expect("valid JSON")
    }

    fn check(test: &str, config: &str) -> Validation {
        validate(test, &json(config), &caps()).expect("validation runs")
    }

    fn e2e(throughput: &str, duration: &str, producers: &str, consumers: &str) -> Validation {
        check(
            "e2e_latency",
            &format!(
                "{{\"throughput_mbps\": {throughput}, \"duration_minutes\": {duration}, \
                 \"num_producers\": {producers}, \"num_consumers\": {consumers}}}"
            ),
        )
    }

    fn restart(target: &str, duration: &str, reboot: &str, producers: &str) -> Validation {
        check(
            "broker_restart",
            &format!(
                "{{\"target_throughput_mbps\": {target}, \"duration_minutes\": {duration}, \
                 \"reboot_at_minute\": {reboot}, \"num_producers\": {producers}}}"
            ),
        )
    }

    // --- compute -------------------------------------------------------

    #[test]
    fn default_sizing_matches_the_hand_computed_values() {
        let caps = caps();
        // 124.9 and 187.5 MB/s per broker x 12 brokers.
        assert_eq!(caps.cluster_sustained_ingress_mbps, 1498.8);
        assert_eq!(caps.cluster_max_ingress_mbps, 2250.0);
        // ceil(2250.0 * 1.5 / 400) = ceil(8.4375) = 9 producers = 3600 MB/s.
        assert_eq!(caps.fleet_producer_count, 9);
        assert_eq!(caps.fleet_consumer_count, 9);
        assert_eq!(caps.fleet_max_mbps, 3600);
        // round(1498.8 * 0.8) = round(1199.04)
        assert_eq!(caps.default_target_throughput_mbps, 1199);
    }

    #[test]
    fn small_clusters_get_the_two_node_fleet_floor() {
        let caps = compute("express.m7g.large", 3, None).expect("caps");
        assert_eq!(caps.cluster_sustained_ingress_mbps, 46.8);
        assert_eq!(caps.cluster_max_ingress_mbps, 70.2);
        assert_eq!(caps.fleet_producer_count, 2, "ceil(0.26) = 1, floored to 2");
        assert_eq!(caps.fleet_max_mbps, 800);
        assert_eq!(caps.default_target_throughput_mbps, 37);
    }

    #[test]
    fn large_clusters_scale_the_fleet() {
        let caps = compute("express.m7g.16xlarge", 60, None).expect("caps");
        assert_eq!(caps.cluster_sustained_ingress_mbps, 30_000.0);
        assert_eq!(caps.cluster_max_ingress_mbps, 45_000.0);
        // ceil(45000 * 1.5 / 400) = ceil(168.75)
        assert_eq!(caps.fleet_producer_count, 169);
        assert_eq!(caps.fleet_max_mbps, 67_600);
        assert_eq!(caps.default_target_throughput_mbps, 24_000);
    }

    #[test]
    fn every_instance_type_computes() {
        for (name, sustained, max_quota) in PER_BROKER_INGRESS {
            let caps = compute(name, 3, None).expect(name);
            assert!(caps.cluster_sustained_ingress_mbps > 0.0);
            assert!(caps.cluster_max_ingress_mbps > caps.cluster_sustained_ingress_mbps);
            assert!(sustained < max_quota, "{name}");
            assert!(caps.fleet_producer_count >= 2);
        }
        assert_eq!(PER_BROKER_INGRESS.len(), 7);
    }

    #[test]
    fn compute_rejects_bad_sizing_with_the_legacy_messages() {
        let Err(message) = compute("m5.large", 3, None) else {
            panic!("unknown instance type must fail");
        };
        assert_eq!(
            message,
            "unknown instance_type 'm5.large'; valid: ['express.m7g.large', 'express.m7g.xlarge', \
             'express.m7g.2xlarge', 'express.m7g.4xlarge', 'express.m7g.8xlarge', \
             'express.m7g.12xlarge', 'express.m7g.16xlarge']"
        );
        for count in [-3, 0, 1, 2, 4, 5, 10, 13] {
            assert_eq!(
                compute(DEFAULT_INSTANCE_TYPE, count, None),
                Err("broker_count must be a multiple of 3 and >= 3".to_string()),
                "{count}"
            );
        }
        for count in [3, 6, 15, 60] {
            assert!(
                compute(DEFAULT_INSTANCE_TYPE, count, None).is_ok(),
                "{count}"
            );
        }
    }

    #[test]
    fn kafka_version_is_only_checked_when_supplied() {
        for version in SUPPORTED_KAFKA_VERSIONS {
            assert!(
                compute(DEFAULT_INSTANCE_TYPE, 3, Some(version)).is_ok(),
                "{version}"
            );
        }
        for version in ["3.9.x", "3.9", "3.7.0", "", "3.9.x.KRAFT", "4.0"] {
            assert_eq!(
                compute(DEFAULT_INSTANCE_TYPE, 3, Some(version)),
                Err(format!(
                    "unsupported kafka_version '{version}'; valid: ['3.6.0', '3.8.x', '3.9.x.kraft'] \
                     (3.9 is KRaft-only on Express)"
                )),
                "{version:?}"
            );
        }
        assert!(compute(DEFAULT_INSTANCE_TYPE, 3, None).is_ok());
    }

    #[test]
    fn compute_document_has_the_published_shape() {
        let doc = caps().to_json();
        let Json::Object(pairs) = &doc else {
            panic!("object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "instance_type",
                "broker_count",
                "supported_kafka_versions",
                "cluster_sustained_ingress_mbps",
                "cluster_max_ingress_mbps",
                "fleet_instance_type",
                "fleet_instance_bandwidth_mbps",
                "fleet_producer_count",
                "fleet_consumer_count",
                "fleet_max_mbps",
                "default_target_throughput_mbps",
                "tests",
            ]
        );
        assert_eq!(
            doc.get("fleet_instance_type").and_then(Json::as_str),
            Some("c5.2xlarge")
        );
        assert_eq!(
            doc.get("fleet_instance_bandwidth_mbps"),
            Some(&Json::Int(400))
        );
        let tests = doc.get("tests").expect("tests");
        let bounds = |test: &str, key: &str| {
            tests
                .get(test)
                .and_then(|t| t.get(key))
                .cloned()
                .expect("bounds")
        };
        assert_eq!(
            bounds("e2e_latency", "throughput_mbps"),
            json("{\"default\": 10, \"min\": 1, \"max\": 3600}")
        );
        assert_eq!(
            bounds("e2e_latency", "duration_minutes"),
            json("{\"default\": 10, \"min\": 5, \"max\": 120}")
        );
        assert_eq!(
            bounds("e2e_latency", "num_producers"),
            json("{\"default\": 1, \"min\": 1, \"max\": 9}")
        );
        assert_eq!(
            bounds("e2e_latency", "num_consumers"),
            json("{\"default\": 1, \"min\": 1, \"max\": 9}")
        );
        assert_eq!(
            bounds("broker_restart", "target_throughput_mbps"),
            json("{\"default\": 1199, \"min\": 1, \"max\": 3600}")
        );
        assert_eq!(
            bounds("broker_restart", "duration_minutes"),
            json("{\"default\": 15, \"min\": 10, \"max\": 120}")
        );
        assert_eq!(
            bounds("broker_restart", "reboot_at_minute"),
            json("{\"default\": 5, \"min\": 2, \"max\": null}")
        );
        assert_eq!(
            bounds("broker_restart", "num_producers"),
            json("{\"default\": 9, \"min\": 1, \"max\": 9}")
        );
    }

    #[test]
    fn rounding_is_half_to_even_like_python() {
        assert_eq!(round_half_even(0.5), 0);
        assert_eq!(round_half_even(1.5), 2);
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(74.88), 75);
        assert_eq!(round_half_even(749.4), 749);
        assert_eq!(round_half_even(-1.5), -2);
    }

    // --- render --------------------------------------------------------

    const MINI: &str = "Parameters:\n  InstanceType:\n    Type: String\n    Default: old\n    Default: second\n  \
        BrokerCount:\n    Type: Number\n    # note\n      Default: 1\n\n    MinValue: 3\n  Other:\n    Default: untouched\nResources:\n  Foo:\n    Default: not-a-param\n";

    #[test]
    fn set_param_default_rewrites_only_the_first_default_of_the_named_block() {
        let text = set_param_default(MINI, "InstanceType", "new").expect("rewritten");
        assert!(text.contains(
            "  InstanceType:\n    Type: String\n    Default: new\n    Default: second\n"
        ));
        assert!(text.contains("Other:\n    Default: untouched"));
        assert!(text.contains("Foo:\n    Default: not-a-param"));
        assert!(text.ends_with('\n'), "trailing newline preserved");

        // The original indentation of the Default line is kept; blank lines
        // stay inside the block.
        let text = set_param_default(MINI, "BrokerCount", "9").expect("rewritten");
        assert!(text.contains("    # note\n      Default: 9\n\n    MinValue: 3\n"));
        assert!(text.contains("Default: old\n"));
        assert_eq!(text.lines().count(), MINI.lines().count());
    }

    #[test]
    fn set_param_default_keeps_a_missing_trailing_newline() {
        let source = "  P:\n    Default: 1";
        assert_eq!(
            set_param_default(source, "P", "2").expect("ok"),
            "  P:\n    Default: 2"
        );
        let source = "  P:\n    Default: 1\n";
        assert_eq!(
            set_param_default(source, "P", "2").expect("ok"),
            "  P:\n    Default: 2\n"
        );
    }

    #[test]
    fn template_drift_fails_loudly() {
        let drift = "could not set Default for parameter 'P'; the source template may have \
                     drifted (expected a 2-space-indented block with a Default line)";
        // Missing parameter, wrong header indentation, no Default line, and a
        // Default outside the block all count as drift.
        for source in [
            "  Q:\n    Default: 1\n",
            "    P:\n      Default: 1\n",
            "  P:\n    Type: String\n",
            "  P:\n    Type: String\nResources:\n    Default: 1\n",
            "",
        ] {
            assert_eq!(
                set_param_default(source, "P", "2"),
                Err(drift.to_string()),
                "{source:?}"
            );
        }
    }

    #[test]
    fn rendering_bakes_sizing_and_fleet_into_six_defaults() {
        let rendered =
            render_template(SOURCE_TEMPLATE, "express.m7g.8xlarge", 9, "3.8.x").expect("rendered");
        // 375.0 * 9 = 3375 MB/s max quota -> ceil(3375 * 1.5 / 400) = 13 producers.
        assert_eq!(rendered.caps.fleet_producer_count, 13);
        let before: Vec<&str> = SOURCE_TEMPLATE.lines().collect();
        let after: Vec<&str> = rendered.template.lines().collect();
        assert_eq!(before.len(), after.len());
        let changed: Vec<(usize, &str)> = after
            .iter()
            .enumerate()
            .filter(|(index, line)| **line != before[*index])
            .map(|(index, line)| (index, *line))
            .collect();
        assert_eq!(
            changed,
            [
                (11, "    Default: express.m7g.8xlarge"),
                (16, "    Default: 9"),
                (20, "    Default: 3.8.x"),
                (38, "    Default: 13"),
                (42, "    Default: 13"),
            ],
            "ClientInstanceType already defaults to c5.2xlarge"
        );
        assert!(rendered.template.ends_with('\n'));
        assert!(rendered
            .template
            .contains("Default: /aws/service/ami-amazon-linux-latest"));
        let names: Vec<&str> = rendered
            .baked_parameters
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            names,
            [
                "InstanceType",
                "BrokerCount",
                "KafkaVersion",
                "ClientInstanceType",
                "ProducerCount",
                "ConsumerCount"
            ]
        );
    }

    #[test]
    fn rendering_rejects_bad_sizing_and_a_drifted_template() {
        assert!(
            render_template(SOURCE_TEMPLATE, "express.m7g.4xlarge", 10, "3.9.x.kraft").is_err()
        );
        assert!(render_template(SOURCE_TEMPLATE, "x", 12, "3.9.x.kraft").is_err());
        assert_eq!(
            render_template(SOURCE_TEMPLATE, "express.m7g.4xlarge", 12, "3.9.x"),
            Err(
                "unsupported kafka_version '3.9.x'; valid: ['3.6.0', '3.8.x', '3.9.x.kraft'] \
                 (3.9 is KRaft-only on Express)"
                    .to_string()
            )
        );
        let drifted = SOURCE_TEMPLATE.replace("  ProducerCount:", "  ProducerCountX:");
        let Err(message) = render_template(&drifted, "express.m7g.4xlarge", 12, "3.9.x.kraft")
        else {
            panic!("drift must fail");
        };
        assert!(message.starts_with("could not set Default for parameter 'ProducerCount'"));
    }

    #[test]
    fn render_result_document_lists_the_baked_parameters() {
        let rendered = render_template(SOURCE_TEMPLATE, "express.m7g.4xlarge", 12, "3.9.x.kraft")
            .expect("rendered");
        let doc = render_result_json(&rendered, "/work/stack.yaml");
        assert_eq!(
            doc.get("output_path").and_then(Json::as_str),
            Some("/work/stack.yaml")
        );
        assert_eq!(
            doc.get("deploy_command").and_then(Json::as_str),
            Some(
                "aws cloudformation create-stack --stack-name msk-express-simulation \
                 --template-body file:///work/stack.yaml --capabilities CAPABILITY_IAM \
                 --region <region>"
            )
        );
        assert_eq!(
            doc.get("baked_parameters"),
            Some(&json(
                "{\"InstanceType\": \"express.m7g.4xlarge\", \"BrokerCount\": 12, \
                 \"KafkaVersion\": \"3.9.x.kraft\", \"ClientInstanceType\": \"c5.2xlarge\", \
                 \"ProducerCount\": 9, \"ConsumerCount\": 9}"
            ))
        );
        assert_eq!(doc.get("fleet_producer_count"), Some(&Json::Int(9)));
        assert_eq!(doc.get("fleet_consumer_count"), Some(&Json::Int(9)));
        assert_eq!(
            doc.get("fleet_instance_type").and_then(Json::as_str),
            Some("c5.2xlarge")
        );
    }

    // --- validate: e2e_latency ------------------------------------------

    #[test]
    fn e2e_defaults_pass_cleanly() {
        let result = e2e("10", "10", "1", "1");
        assert_eq!(result.decision(), "PASS");
        assert!(
            result.errors.is_empty() && result.warnings.is_empty(),
            "{result:?}"
        );
    }

    #[test]
    fn e2e_ranges_are_inclusive() {
        for (field, low_ok, low_bad, high_ok, high_bad, window) in [
            ("throughput_mbps", "1", "0", "3600", "3601", "[1, 3600]"),
            ("duration_minutes", "5", "4", "120", "121", "[5, 120]"),
            ("num_producers", "1", "0", "9", "10", "[1, 9]"),
            ("num_consumers", "1", "0", "9", "10", "[1, 9]"),
        ] {
            let with = |value: &str| match field {
                "throughput_mbps" => e2e(value, "10", "9", "1"),
                "duration_minutes" => e2e("10", value, "1", "1"),
                "num_producers" => e2e("10", "10", value, "1"),
                _ => e2e("10", "10", "1", value),
            };
            for ok in [low_ok, high_ok] {
                assert!(
                    with(ok).errors.is_empty(),
                    "{field}={ok}: {:?}",
                    with(ok).errors
                );
            }
            for bad in [low_bad, high_bad] {
                let result = with(bad);
                assert_eq!(result.decision(), "REJECT", "{field}={bad}");
                assert_eq!(
                    result.errors,
                    [format!(
                        "{field}={bad} is outside the allowed range {window}"
                    )]
                );
            }
        }
    }

    #[test]
    fn e2e_reports_every_violation_at_once() {
        let result = e2e("0", "4", "0", "99");
        assert_eq!(
            result.errors,
            [
                "throughput_mbps=0 is outside the allowed range [1, 3600]",
                "duration_minutes=4 is outside the allowed range [5, 120]",
                "num_producers=0 is outside the allowed range [1, 9]",
                "num_consumers=99 is outside the allowed range [1, 9]",
            ]
        );
    }

    #[test]
    fn e2e_throughput_warning_tiers() {
        // Sustained 1498.8, max quota 2250.0, 60% = 899.28.
        let sixty = |tput: &str| e2e(tput, "10", "9", "1").warnings;
        assert!(sixty("899").is_empty());
        assert_eq!(
            sixty("900"),
            [
                "throughput_mbps=900 is above 60% of sustained ingress; high load can inflate \
              latency. For a clean signal, consider <= 749 MB/s."
            ]
        );
        assert_eq!(sixty("899.3").len(), 1);
        // At the sustained rate there is no sustained warning, only the 60% note.
        assert_eq!(sixty("1498.8").len(), 1);
        assert_eq!(
            sixty("1500"),
            [
                "throughput_mbps=1500 MB/s is above the cluster's sustained/recommended ingress \
              (1498.8 MB/s); it can run but expect performance degradation (elevated latency), \
              with no throttling until the 2250.0 MB/s ceiling."
            ],
            "the degradation warning replaces the 60% note"
        );
        assert_eq!(
            sixty("2250").len(),
            1,
            "the quota itself is still only degradation"
        );
        assert!(sixty("2250")[0].contains("sustained/recommended"));
        assert_eq!(
            sixty("2251"),
            [
                "throughput_mbps=2251 MB/s exceeds the cluster's maximum ingress quota (2250.0 \
              MB/s) \u{2014} MSK Express throttles read/write traffic beyond this, so the \
              cluster cannot actually sustain that rate."
            ]
        );
    }

    #[test]
    fn e2e_producer_sufficiency_warning() {
        let warnings = |tput: &str, producers: &str| e2e(tput, "10", producers, "1").warnings;
        assert!(warnings("400", "1").is_empty(), "ceil(400 / 400) = 1");
        assert_eq!(
            warnings("10", "1").len(),
            0,
            "one producer carries a low rate"
        );
        assert_eq!(
            warnings("401", "1"),
            ["num_producers=1 may not sustain 401 MB/s; consider >= 2 producers."]
        );
        // The throughput warnings come first, then the producer note.
        let both = warnings("2000", "2");
        assert_eq!(both.len(), 2);
        assert!(both[0].contains("sustained/recommended"));
        assert_eq!(
            both[1],
            "num_producers=2 may not sustain 2000 MB/s; consider >= 5 producers."
        );
        // Float throughput keeps Python's rendering.
        let floats = warnings("12.5", "1");
        assert!(floats.is_empty());
        let floats = warnings("400.5", "1");
        assert_eq!(
            floats,
            ["num_producers=1 may not sustain 400.5 MB/s; consider >= 2 producers."]
        );
    }

    // --- validate: broker_restart -------------------------------------

    #[test]
    fn broker_restart_defaults_warn_about_headroom_only() {
        let result = restart("1199", "15", "5", "9");
        assert_eq!(result.decision(), "PASS");
        assert_eq!(
            result.warnings,
            ["target_throughput_mbps=1199 is above 60% of sustained ingress; broker-CPU headroom \
              during the restart is limited, so recovery may take longer."]
        );
        let result = restart("500", "15", "5", "2");
        assert!(
            result.errors.is_empty() && result.warnings.is_empty(),
            "{result:?}"
        );
    }

    #[test]
    fn broker_restart_ranges_and_reboot_window() {
        assert_eq!(
            restart("0", "9", "8", "0").errors,
            [
                "target_throughput_mbps=0 is outside the allowed range [1, 3600]",
                "duration_minutes=9 is outside the allowed range [10, 120]",
                "reboot_at_minute=8 is outside the allowed range [2, 7]",
                "num_producers=0 is outside the allowed range [1, 9]",
            ]
        );
        // The reboot must leave two minutes after it: max = duration - 2.
        assert!(restart("100", "15", "2", "1").errors.is_empty());
        assert!(restart("100", "15", "13", "1").errors.is_empty());
        assert_eq!(
            restart("100", "15", "14", "1").errors,
            ["reboot_at_minute=14 is outside the allowed range [2, 13]"]
        );
        assert_eq!(
            restart("100", "15", "1", "1").errors,
            ["reboot_at_minute=1 is outside the allowed range [2, 13]"]
        );
        assert!(restart("100", "120", "118", "1").errors.is_empty());
        assert_eq!(restart("100", "10", "8", "1").errors, Vec::<String>::new());
        assert_eq!(
            restart("100", "121", "5", "1").errors,
            ["duration_minutes=121 is outside the allowed range [10, 120]"]
        );
        // A fractional duration keeps a fractional window, like Python.
        assert_eq!(
            restart("100", "15.5", "13.6", "1").errors,
            ["reboot_at_minute=13.6 is outside the allowed range [2, 13.5]"]
        );
        assert!(restart("100", "15.5", "13.5", "1").errors.is_empty());
    }

    #[test]
    fn broker_restart_window_survives_the_signed_integer_boundaries() {
        // `duration - 2` leaves the i64 range at the minimum; Python's unbounded
        // ints print the exact result and the reboot is still out of range (#4540).
        let result = restart("1", "-9223372036854775808", "2", "1");
        assert_eq!(result.decision(), "REJECT");
        assert_eq!(
            result.errors,
            [
                "duration_minutes=-9223372036854775808 is outside the allowed range [10, 120]",
                "reboot_at_minute=2 is outside the allowed range [2, -9223372036854775810]",
            ]
        );
        // The neighbours of the boundary reject the same way, never wrap.
        for duration in ["-9223372036854775807", "-9223372036854775806", "-9223372036854775805"] {
            let result = restart("1", duration, "2", "1");
            assert_eq!(result.decision(), "REJECT", "{duration}");
            assert_eq!(result.errors.len(), 2, "{duration}");
        }
        // The maximum has no overflow (max - 2) and an equal reboot is rejected
        // by the exact integer comparison, not by float rounding.
        let result = restart("1", "9223372036854775807", "9223372036854775807", "1");
        assert_eq!(
            result.errors,
            [
                "duration_minutes=9223372036854775807 is outside the allowed range [10, 120]",
                "reboot_at_minute=9223372036854775807 is outside the allowed range \
                 [2, 9223372036854775805]",
            ]
        );
        // A reboot exactly at the exact integer limit passes that range check.
        let result = restart("1", "9223372036854775807", "9223372036854775805", "1");
        assert_eq!(
            result.errors,
            ["duration_minutes=9223372036854775807 is outside the allowed range [10, 120]"]
        );
    }

    #[test]
    fn integer_range_checks_do_not_round_through_floats() {
        // 2^53 + 1 is not representable as f64; the exact comparison still sees
        // it above the 120 minute ceiling and the throughput ceiling.
        let result = e2e("9007199254740993", "9007199254740993", "1", "1");
        assert_eq!(
            result.errors,
            [
                "throughput_mbps=9007199254740993 is outside the allowed range [1, 3600]",
                "duration_minutes=9007199254740993 is outside the allowed range [5, 120]",
            ]
        );
    }

    #[test]
    fn compute_refuses_a_fleet_that_does_not_fit_an_i64() {
        let name = "express.m7g.16xlarge";
        // 750 MB/s per broker x 1.5 / 400 = 2.8125 producers per broker.
        let ok = compute(name, 7_999_999_999_999_998, None).expect("fits");
        assert!(ok.fleet_producer_count > 22_000_000_000_000_000);
        assert!(ok.fleet_producer_count <= MAX_FLEET_PRODUCERS);
        assert_eq!(ok.fleet_max_mbps, ok.fleet_producer_count * 400);
        assert_eq!(ok.fleet_consumer_count, ok.fleet_producer_count);
        for count in [8_400_000_000_000_000, 9_223_372_036_854_775_806] {
            let Err(message) = compute(name, count, None) else {
                panic!("{count} must be refused");
            };
            assert_eq!(
                message,
                format!(
                    "broker_count {count} is too large: the producer fleet would exceed \
                     {MAX_FLEET_PRODUCERS} nodes"
                )
            );
        }
        // The fleet bound is exactly the largest count whose MB/s fits an i64.
        assert_eq!(MAX_FLEET_PRODUCERS, 23_058_430_092_136_939);
        assert!(MAX_FLEET_PRODUCERS.checked_mul(FLEET_NODE_THROUGHPUT_MBPS).is_some());
        assert!((MAX_FLEET_PRODUCERS + 1)
            .checked_mul(FLEET_NODE_THROUGHPUT_MBPS)
            .is_none());
        // Rendering goes through compute, so it refuses too.
        assert!(render_template("", name, 9_223_372_036_854_775_806, "3.8.x").is_err());
    }

    #[test]
    fn broker_restart_throughput_tiers_match_e2e() {
        let warnings = |tput: &str| restart(tput, "15", "5", "9").warnings;
        assert!(warnings("899").is_empty());
        assert_eq!(warnings("900").len(), 1);
        assert!(warnings("1500")[0].starts_with(
            "target_throughput_mbps=1500 MB/s is above the cluster's sustained/recommended ingress"
        ));
        assert!(warnings("2251")[0].starts_with(
            "target_throughput_mbps=2251 MB/s exceeds the cluster's maximum ingress quota"
        ));
    }

    // --- validate: input handling ---------------------------------------

    #[test]
    fn unknown_tests_are_rejected() {
        let result = validate("soak", &json("{}"), &caps()).expect("runs");
        assert_eq!(result.decision(), "REJECT");
        assert_eq!(
            result.errors,
            ["unknown test 'soak'; valid: e2e_latency, broker_restart"]
        );
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn missing_or_non_numeric_keys_are_errors() {
        assert_eq!(
            validate("e2e_latency", &json("{}"), &caps()),
            Err("'throughput_mbps'".to_string())
        );
        assert_eq!(
            validate(
                "e2e_latency",
                &json("{\"throughput_mbps\": 1, \"duration_minutes\": 5}"),
                &caps()
            ),
            Err("'num_producers'".to_string())
        );
        assert_eq!(
            validate(
                "broker_restart",
                &json("{\"target_throughput_mbps\": 1, \"duration_minutes\": 10, \"num_producers\": 1}"),
                &caps()
            ),
            Err("'reboot_at_minute'".to_string())
        );
        assert_eq!(
            validate(
                "e2e_latency",
                &json("{\"throughput_mbps\": \"10\", \"duration_minutes\": 5}"),
                &caps()
            ),
            Err("throughput_mbps must be a number, not string".to_string())
        );
        assert_eq!(
            validate("e2e_latency", &json("{\"throughput_mbps\": null}"), &caps()),
            Err("throughput_mbps must be a number, not null".to_string())
        );
    }

    #[test]
    fn validation_document_has_the_published_shape() {
        let pass = check("e2e_latency", "{\"throughput_mbps\": 10, \"duration_minutes\": 10, \"num_producers\": 1, \"num_consumers\": 1}");
        assert_eq!(
            pass.to_json(),
            json("{\"decision\": \"PASS\", \"errors\": [], \"warnings\": []}")
        );
        let reject = e2e("0", "10", "1", "1");
        let doc = reject.to_json();
        let Json::Object(pairs) = &doc else {
            panic!("object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["decision", "errors", "warnings"]);
        assert_eq!(doc.get("decision").and_then(Json::as_str), Some("REJECT"));
    }

    #[test]
    fn a_smaller_cluster_tightens_the_bounds() {
        let caps = compute("express.m7g.large", 3, None).expect("caps");
        // fleet max 800 MB/s, sustained 46.8, max quota 70.2.
        let config = json(
            "{\"throughput_mbps\": 801, \"duration_minutes\": 10, \"num_producers\": 3, \"num_consumers\": 1}",
        );
        let result = validate("e2e_latency", &config, &caps).expect("runs");
        assert_eq!(
            result.errors,
            [
                "throughput_mbps=801 is outside the allowed range [1, 800]",
                "num_producers=3 is outside the allowed range [1, 2]",
            ]
        );
        assert!(result.warnings[0].contains("maximum ingress quota (70.2 MB/s)"));
    }
}
