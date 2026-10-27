//! Timestream for `InfluxDB` helpers (ports of `timestream-influxdb/scripts/input_validator.py`
//! and `instance_types.py`).
//!
//! - `validate_all`: the eight input rules checked before an AWS CLI call.
//! - [`INSTANCE_TYPES`], [`list_types`] and [`recommend`]: the instance type
//!   table and its sizing rule. The thresholds are published behaviour; the
//!   boundary tests pin each one.
//!
//! Intentional differences (`docs/migration/relational.md`): Python's
//! `re.match(... $)` also accepted a value followed by one newline and its `\d`
//! matched non-ASCII digits; both are rejected here because the values go
//! into AWS CLI calls.

use toolkit_core::escape_untrusted;

/// The longest value shown in a failure line.
const SHOWN_CHARS: usize = 120;

/// (name, vCPU, memory GiB, network), in size order. The order is the sizing
/// ladder `recommend` walks.
pub(crate) const INSTANCE_TYPES: &[(&str, u32, u32, &str)] = &[
    ("db.influx.medium", 1, 8, "Up to 10 Gbps"),
    ("db.influx.large", 2, 16, "Up to 10 Gbps"),
    ("db.influx.xlarge", 4, 32, "Up to 10 Gbps"),
    ("db.influx.2xlarge", 8, 64, "Up to 10 Gbps"),
    ("db.influx.4xlarge", 16, 128, "Up to 10 Gbps"),
    ("db.influx.8xlarge", 32, 256, "10 Gbps"),
    ("db.influx.12xlarge", 48, 384, "12 Gbps"),
    ("db.influx.16xlarge", 64, 512, "20 Gbps"),
    ("db.influx.24xlarge", 96, 768, "25 Gbps"),
];

#[derive(Clone, Copy)]
enum Rule {
    /// A letter, then letters, digits or hyphens, `1 + max_tail` characters at most.
    Name {
        max_tail: usize,
    },
    InstanceType,
    Region,
    /// `{prefix}-` and 8 to 17 lowercase hex digits.
    Id {
        prefix: &'static str,
    },
    StorageGb,
}

/// (key, rule, message) in the order of the Python `VALIDATORS` table.
const VALIDATORS: &[(&str, Rule, &str)] = &[
    (
        "instance_name",
        Rule::Name { max_tail: 62 },
        "Must start with letter, alphanumeric/hyphens, max 63 chars",
    ),
    (
        "instance_type",
        Rule::InstanceType,
        "Must be db.influx.{medium|large|xlarge|Nxlarge}",
    ),
    (
        "region",
        Rule::Region,
        "Must be valid AWS region format (e.g., us-east-1)",
    ),
    ("vpc_id", Rule::Id { prefix: "vpc" }, "Must be vpc-{hex}"),
    (
        "subnet_id",
        Rule::Id { prefix: "subnet" },
        "Must be subnet-{hex}",
    ),
    ("sg_id", Rule::Id { prefix: "sg" }, "Must be sg-{hex}"),
    (
        "snapshot_id",
        Rule::Name { max_tail: 254 },
        "Must start with letter, max 255 chars",
    ),
    ("storage_gb", Rule::StorageGb, "Must be 20-16384"),
];

fn is_name(value: &str, max_tail: usize) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphabetic()
        && bytes.len() <= 1 + max_tail
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

fn is_instance_type(value: &str) -> bool {
    let Some(size) = value.strip_prefix("db.influx.") else {
        return false;
    };
    matches!(size, "medium" | "large" | "xlarge")
        || size
            .strip_suffix("xlarge")
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn is_region(value: &str) -> bool {
    let mut parts = value.split('-');
    let (Some(first), Some(middle), Some(last), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    first.len() == 2
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && !middle.is_empty()
        && middle.bytes().all(|b| b.is_ascii_lowercase())
        && last.len() == 1
        && last.bytes().all(|b| b.is_ascii_digit())
}

fn is_id(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hex| {
            (8..=17).contains(&hex.len())
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Python `int(text)` for the storage size: surrounding whitespace, a sign and
/// single underscores between digits are accepted; larger magnitudes than
/// `i64` are out of range anyway.
fn is_storage_gb(value: &str) -> bool {
    crate::aurora::args::parse_py_int(value).is_some_and(|gb| (20..=16384).contains(&gb))
}

fn passes(rule: Rule, value: &str) -> bool {
    match rule {
        Rule::Name { max_tail } => is_name(value, max_tail),
        Rule::InstanceType => is_instance_type(value),
        Rule::Region => is_region(value),
        Rule::Id { prefix } => is_id(value, prefix),
        Rule::StorageGb => is_storage_gb(value),
    }
}

/// `(ok, message)` of one key. An unknown key has no validator and passes.
pub(crate) fn validate(key: &str, value: &str) -> (bool, &'static str) {
    match VALIDATORS.iter().find(|(name, ..)| *name == key) {
        None => (true, "No validator"),
        Some((_, rule, message)) => (passes(*rule, value), message),
    }
}

/// The report of `validate_all` and whether every input passed. Keys repeated
/// in `pairs` keep the position of the first and the value of the last, like
/// the `dict` the script built.
pub(crate) fn validate_all(pairs: &[(String, String)]) -> (Vec<String>, bool) {
    let mut unique: Vec<(&str, &str)> = Vec::new();
    for (key, value) in pairs {
        match unique.iter_mut().find(|(seen, _)| seen == key) {
            Some(slot) => slot.1 = value,
            None => unique.push((key, value)),
        }
    }
    let errors: Vec<String> = unique
        .iter()
        .filter_map(|(key, value)| {
            let (ok, message) = validate(key, value);
            (!ok).then(|| {
                format!(
                    "  {}={} \u{2014} {message}",
                    escape_untrusted(key, SHOWN_CHARS),
                    escape_untrusted(value, SHOWN_CHARS)
                )
            })
        })
        .collect();
    if errors.is_empty() {
        return (vec!["All inputs valid.".to_string()], true);
    }
    let mut lines = vec!["Validation FAILED:".to_string()];
    lines.extend(errors);
    (lines, false)
}

/// The instance type table, formatted like the script's `list_types`.
pub(crate) fn list_types() -> Vec<String> {
    let mut lines = vec![
        format!(
            "{:<25} {:>5} {:>13} {:<15}",
            "Instance Type", "vCPU", "Memory (GiB)", "Network"
        ),
        "-".repeat(60),
    ];
    for (name, vcpu, memory, network) in INSTANCE_TYPES {
        lines.push(format!("{name:<25} {vcpu:>5} {memory:>13} {network:<15}"));
    }
    lines
}

/// The recommended instance type for a workload.
pub(crate) fn recommended_type(
    write_rate_per_sec: i64,
    query_concurrency: i64,
    data_retention_days: i64,
) -> (&'static str, u32, u32) {
    let base = if write_rate_per_sec < 1000 && query_concurrency < 5 {
        "db.influx.medium"
    } else if write_rate_per_sec < 10000 && query_concurrency < 20 {
        "db.influx.large"
    } else if write_rate_per_sec < 50000 && query_concurrency < 50 {
        "db.influx.xlarge"
    } else if write_rate_per_sec < 100_000 {
        "db.influx.2xlarge"
    } else {
        "db.influx.4xlarge"
    };
    let last = INSTANCE_TYPES.len() - 1;
    let mut index = INSTANCE_TYPES
        .iter()
        .position(|(name, ..)| *name == base)
        .unwrap_or(0);
    // High-retention workloads add storage and memory pressure.
    if data_retention_days > 365 {
        index = (index + 2).min(last);
    } else if data_retention_days > 90 {
        index = (index + 1).min(last);
    }
    let (name, vcpu, memory, _) = INSTANCE_TYPES[index];
    (name, vcpu, memory)
}

/// The three lines of `recommend`.
pub(crate) fn recommend(
    write_rate_per_sec: i64,
    query_concurrency: i64,
    data_retention_days: i64,
) -> Vec<String> {
    let (name, vcpu, memory) =
        recommended_type(write_rate_per_sec, query_concurrency, data_retention_days);
    vec![
        format!("Recommended: {name} ({vcpu} vCPU, {memory} GiB)"),
        format!(
            "  Based on: {write_rate_per_sec} writes/s, {query_concurrency} concurrent \
             queries, {data_retention_days}d retention"
        ),
        "  Note: This is a starting point. Monitor CloudWatch metrics and adjust.".to_string(),
    ]
}
