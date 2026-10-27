//! MSK Express compatibility assessment (port of `compatibility.py`).
//!
//! A pure file processor: a discovery `cluster-config.json` goes in and a
//! five-pillar verdict document comes out (`topology`, `kafka_version`, `configs`,
//! auth, quotas). No live cluster or AWS call is involved, so results are
//! deterministic and replayable from fixtures.
//!
//! The discovery contract carries full Kafka config dumps. Values equal to the
//! Apache default for the source version produce no evidence; only divergences
//! are evaluated against the Express read/write, read-only, range and
//! forced-value sets. Every threshold cites one of the AWS public pages listed
//! in the legacy script (Express broker overview, read/write and read-only
//! configurations, broker quotas and best practices).

use toolkit_core::Json;

use crate::pyfmt::{format_fixed, py_int_parse, py_repr, py_str, py_str_repr, round_to};

/// How serious a finding or a pillar is. Ordered `Info < Advisory < ActionRequired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Nothing to do.
    Info,
    /// Review recommended.
    Advisory,
    /// Blocks a migration as-is.
    ActionRequired,
}

impl Severity {
    /// The published verdict spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Advisory => "ADVISORY",
            Self::ActionRequired => "ACTION_REQUIRED",
        }
    }

    /// The worse of two severities.
    #[must_use]
    pub fn worst(self, other: Self) -> Self {
        self.max(other)
    }
}

/// One finding with its own severity.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// Stable finding code, for example `AZ_COUNT_NOT_3`.
    pub code: &'static str,
    /// Severity of this finding alone.
    pub severity: Severity,
    /// Customer-facing explanation.
    pub detail: String,
    /// Additional structured fields, in output order.
    pub extra: Vec<(&'static str, Json)>,
}

impl Evidence {
    fn new(code: &'static str, severity: Severity, detail: impl Into<String>) -> Self {
        Self {
            code,
            severity,
            detail: detail.into(),
            extra: Vec::new(),
        }
    }

    fn with(mut self, key: &'static str, value: Json) -> Self {
        self.extra.push((key, value));
        self
    }

    fn to_json(&self) -> Json {
        let mut pairs = vec![
            ("code".to_string(), Json::string(self.code)),
            ("severity".to_string(), Json::string(self.severity.as_str())),
            ("detail".to_string(), Json::string(self.detail.clone())),
        ];
        for (key, value) in &self.extra {
            pairs.push(((*key).to_string(), value.clone()));
        }
        Json::Object(pairs)
    }
}

type Pillar = (Severity, Vec<Evidence>);

/// A shared `null` for lookups of absent members.
static NULL: Json = Json::Null;

/// MSK Express deploys across exactly this many AZs.
pub const EXPRESS_AZ_COUNT: i128 = 3;
/// Express needs at least this many brokers (RF=3 across 3 AZs).
pub const EXPRESS_TARGET_MIN_BROKERS: i128 = 3;
/// Kafka major.minor versions Express supports.
pub const EXPRESS_SUPPORTED_VERSIONS: &[(u64, u64)] = &[(3, 6), (3, 8), (3, 9), (4, 2)];
/// Oldest source version MSK Replicator can replicate from.
pub const MSK_REPLICATOR_MIN_SOURCE_VERSION: &[u64] = &[2, 8, 1];

/// Express read/write broker configs.
pub const EXPRESS_BROKER_RW: &[&str] = &[
    "advertised.listeners",
    "allow.everyone.if.no.acl.found",
    "auto.create.topics.enable",
    "compression.type",
    "connections.max.idle.ms",
    "delete.topic.enable",
    "group.initial.rebalance.delay.ms",
    "group.max.session.timeout.ms",
    "leader.imbalance.per.broker.percentage",
    "log.cleaner.delete.retention.ms",
    "log.cleaner.max.compaction.lag.ms",
    "log.cleaner.min.compaction.lag.ms",
    "log.cleanup.policy",
    "log.message.timestamp.after.max.ms",
    "log.message.timestamp.before.max.ms",
    "log.message.timestamp.type",
    "log.retention.bytes",
    "log.retention.ms",
    "max.connection.creation.rate",
    "max.connections",
    "max.connections.per.ip",
    "max.connections.per.ip.overrides",
    "max.incremental.fetch.session.cache.slots",
    "message.max.bytes",
    "num.partitions",
    "offsets.retention.minutes",
    "producer.id.expiration.ms",
    "replica.fetch.max.bytes",
    "replica.selector.class",
    "socket.receive.buffer.bytes",
    "socket.request.max.bytes",
    "socket.send.buffer.bytes",
    "transaction.max.timeout.ms",
    "transactional.id.expiration.ms",
];

/// Express read-only broker configs.
pub const EXPRESS_BROKER_RO: &[&str] = &[
    "broker.id",
    "broker.rack",
    "default.replication.factor",
    "fetch.max.bytes",
    "group.max.size",
    "inter.broker.listener.name",
    "inter.broker.protocol.version",
    "listeners",
    "log.message.format.version",
    "min.insync.replicas",
    "num.io.threads",
    "num.network.threads",
    "replica.fetch.response.max.bytes",
    "request.timeout.ms",
    "transaction.state.log.min.isr",
    "transaction.state.log.replication.factor",
    "unclean.leader.election.enable",
];

/// One day in milliseconds.
pub const ONE_DAY_MS: i128 = 24 * 60 * 60 * 1000;

/// Inclusive bounds (`None` = open) Express enforces on broker configs.
pub const EXPRESS_BROKER_RANGES: &[(&str, (Option<i128>, Option<i128>))] =
    &[("log.cleaner.max.compaction.lag.ms", (Some(ONE_DAY_MS), None))];

/// A value Express forces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forced {
    /// Integer value.
    Int(i64),
    /// String value.
    Str(&'static str),
}

impl Forced {
    fn json(self) -> Json {
        match self {
            Self::Int(value) => Json::Int(value),
            Self::Str(value) => Json::string(value),
        }
    }
}

/// Forced broker values (read-only configurations page).
pub const EXPRESS_BROKER_FORCED: &[(&str, Forced)] = &[
    ("default.replication.factor", Forced::Int(3)),
    ("min.insync.replicas", Forced::Int(2)),
    ("transaction.state.log.min.isr", Forced::Int(2)),
    ("unclean.leader.election.enable", Forced::Str("false")),
];

/// Express read/write topic configs.
pub const EXPRESS_TOPIC_RW: &[&str] = &[
    "cleanup.policy",
    "compression.type",
    "delete.retention.ms",
    "max.compaction.lag.ms",
    "max.message.bytes",
    "message.timestamp.after.max.ms",
    "message.timestamp.before.max.ms",
    "message.timestamp.type",
    "min.compaction.lag.ms",
    "retention.bytes",
    "retention.ms",
];

/// Inclusive bounds Express enforces on topic configs.
pub const EXPRESS_TOPIC_RANGES: &[(&str, (Option<i128>, Option<i128>))] =
    &[("max.compaction.lag.ms", (Some(ONE_DAY_MS), None))];

/// Forced topic values.
pub const TOPIC_FORCED: &[(&str, Forced)] = &[
    ("min.insync.replicas", Forced::Int(2)),
    ("unclean.leader.election.enable", Forced::Str("false")),
];

/// Express creates every topic with this replication factor.
pub const EXPRESS_TOPIC_FORCED_RF: i128 = 3;

/// Apache Kafka broker defaults for the configs that are evaluated, in
/// normalized form (`str(value).strip().lower()`). The legacy script keeps one
/// identical table per supported version (3.6, 3.8, 3.9) plus a generic one;
/// they never diverged, so a single table serves every version.
pub const BROKER_DEFAULTS: &[(&str, &str)] = &[
    ("auto.create.topics.enable", "true"),
    ("compression.type", "producer"),
    ("connections.max.idle.ms", "600000"),
    ("delete.topic.enable", "true"),
    ("group.initial.rebalance.delay.ms", "3000"),
    ("group.max.session.timeout.ms", "1800000"),
    ("leader.imbalance.per.broker.percentage", "10"),
    ("log.cleaner.delete.retention.ms", "86400000"),
    ("log.cleaner.max.compaction.lag.ms", "9223372036854775807"),
    ("log.cleaner.min.compaction.lag.ms", "0"),
    ("log.cleanup.policy", "delete"),
    ("log.message.timestamp.after.max.ms", "9223372036854775807"),
    ("log.message.timestamp.before.max.ms", "9223372036854775807"),
    ("log.message.timestamp.type", "createtime"),
    ("log.retention.bytes", "-1"),
    ("log.retention.ms", "-1"),
    ("max.connection.creation.rate", "2147483647"),
    ("max.connections", "2147483647"),
    ("max.connections.per.ip", "2147483647"),
    ("max.incremental.fetch.session.cache.slots", "1000"),
    ("message.max.bytes", "1048588"),
    ("num.partitions", "1"),
    ("offsets.retention.minutes", "10080"),
    ("producer.id.expiration.ms", "86400000"),
    ("replica.fetch.max.bytes", "1048576"),
    ("socket.receive.buffer.bytes", "102400"),
    ("socket.request.max.bytes", "104857600"),
    ("socket.send.buffer.bytes", "102400"),
    ("transaction.max.timeout.ms", "900000"),
    ("transactional.id.expiration.ms", "604800000"),
    ("default.replication.factor", "1"),
    ("min.insync.replicas", "1"),
    ("transaction.state.log.min.isr", "2"),
    ("unclean.leader.election.enable", "false"),
    ("num.io.threads", "8"),
    ("num.network.threads", "3"),
    ("allow.everyone.if.no.acl.found", "false"),
];

/// Apache Kafka topic defaults, normalized like [`BROKER_DEFAULTS`].
pub const TOPIC_DEFAULTS: &[(&str, &str)] = &[
    ("cleanup.policy", "delete"),
    ("compression.type", "producer"),
    ("delete.retention.ms", "86400000"),
    ("max.compaction.lag.ms", "9223372036854775807"),
    ("max.message.bytes", "1048588"),
    ("message.timestamp.after.max.ms", "9223372036854775807"),
    ("message.timestamp.before.max.ms", "9223372036854775807"),
    ("message.timestamp.type", "createtime"),
    ("min.compaction.lag.ms", "0"),
    ("retention.bytes", "-1"),
    ("retention.ms", "604800000"),
    ("min.insync.replicas", "1"),
    ("unclean.leader.election.enable", "false"),
];

/// Allowed `security.encryption_in_transit` values (sorted).
pub const ENCRYPTION_VALUES: &[&str] = &["PLAINTEXT", "TLS", "TLS_PLAINTEXT", "UNKNOWN"];
/// Allowed `security.authentication` values (sorted).
pub const AUTHENTICATION_VALUES: &[&str] = &[
    "OTHER",
    "SASL_IAM",
    "SASL_OAUTHBEARER",
    "SASL_SCRAM",
    "TLS",
    "UNAUTHENTICATED",
    "UNKNOWN",
];
/// Mechanisms that resolve to an IAM principal and share the IAM connection cap.
pub const IAM_AUTHENTICATION_VALUES: &[&str] = &["SASL_IAM", "SASL_OAUTHBEARER"];
/// Mechanisms that require TLS in transit.
pub const TLS_REQUIRED_AUTH_VALUES: &[&str] = &["TLS", "SASL_SCRAM", "SASL_IAM"];

/// Peak ingress limit per broker (m7g.16xlarge), MB per second.
pub const EXPRESS_MAX_INGRESS_MAX_QUOTA_PER_BROKER_MBPS: i64 = 750;
/// Peak egress limit per broker (m7g.16xlarge), MB per second.
pub const EXPRESS_MAX_EGRESS_MAX_QUOTA_PER_BROKER_MBPS: i64 = 1875;
/// Partition cap per broker.
pub const EXPRESS_MAX_PARTITIONS_PER_BROKER: i64 = 32_000;
/// IAM connection cap per broker.
pub const EXPRESS_MAX_IAM_CONNS_PER_BROKER: i64 = 3_000;
/// Per-partition throughput where Express begins to throttle, MB/s.
pub const EXPRESS_PARTITION_THROUGHPUT_MBPS: i64 = 15;

fn json_int(value: i128) -> Json {
    i64::try_from(value).map_or(Json::Float(value as f64), Json::Int)
}

fn list_of_str(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|value| Json::string(*value)).collect())
}

fn object_of<'a>(value: &'a Json, what: &str) -> Result<&'a [(String, Json)], String> {
    match value {
        Json::Object(pairs) => Ok(pairs),
        _ => Err(format!("{what} must be an object")),
    }
}

/// Python truthiness of a JSON value.
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Int(number) => *number != 0,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(pairs) => !pairs.is_empty(),
    }
}

/// `value.get(key)` where a missing key and an explicit `null` both read as
/// absent (Python `None`).
fn get_present<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.get(key).filter(|found| !matches!(found, Json::Null))
}

/// Python `int(value)` for a JSON value.
fn py_int(value: &Json, what: &str) -> Result<i128, String> {
    match value {
        Json::Int(number) => Ok(i128::from(*number)),
        Json::Bool(flag) => Ok(i128::from(*flag)),
        Json::Float(number) if number.is_finite() => Ok(number.trunc() as i128),
        Json::Float(_) => Err(format!("{what}: cannot convert a non-finite float to integer")),
        Json::String(text) => py_int_parse(text).ok_or_else(|| {
            format!(
                "{what}: invalid literal for int() with base 10: {}",
                py_str_repr(text)
            )
        }),
        other => Err(format!(
            "{what}: int() argument must be a string or a number, not {}",
            other.type_name()
        )),
    }
}

/// `str(value).strip().lower()` with Python's JSON-bool spelling.
fn normalize_value(value: &Json) -> String {
    match value {
        Json::Bool(true) => "true".to_string(),
        Json::Bool(false) => "false".to_string(),
        other => py_str(other).trim().to_lowercase(),
    }
}

fn is_default(key: &str, value: &Json, defaults: &[(&str, &str)]) -> bool {
    // A config without a known default cannot be told apart, so the rules
    // still fire for it.
    defaults
        .iter()
        .find(|(name, _)| *name == key)
        .is_some_and(|(_, default)| normalize_value(value) == *default)
}

fn coerce_int(value: &Json) -> Option<i128> {
    match value {
        Json::Int(number) => Some(i128::from(*number)),
        Json::String(text) => py_int_parse(text),
        _ => None,
    }
}

struct RangeProblem {
    config_key: String,
    detail: String,
    observed: Json,
    limit: Json,
}

/// Evidence-style problem if `value` is not an integer within `bounds`.
/// `subject` names whose setting it is ("Your cluster" or "Topic 'name'").
fn check_range(
    key: &str,
    value: &Json,
    bounds: (Option<i128>, Option<i128>),
    subject: &str,
) -> Option<RangeProblem> {
    let (lo, hi) = bounds;
    let lo_text = lo.map_or_else(|| "INT_MIN".to_string(), |bound| bound.to_string());
    let hi_text = hi.map_or_else(|| "INT_MAX".to_string(), |bound| bound.to_string());
    let limit = Json::Array(vec![
        lo.map_or(Json::Null, json_int),
        hi.map_or(Json::Null, json_int),
    ]);
    let Some(integer) = coerce_int(value) else {
        return Some(RangeProblem {
            config_key: key.to_string(),
            detail: format!(
                "MSK Express expects {key} to be an integer within [{lo_text}, {hi_text}]. \
                 {subject} sets it to {}, which is not a valid integer, so this \
                 configuration can't be migrated as-is.",
                py_repr(value)
            ),
            observed: value.clone(),
            limit,
        });
    };
    if lo.is_some_and(|bound| integer < bound) || hi.is_some_and(|bound| integer > bound) {
        return Some(RangeProblem {
            config_key: key.to_string(),
            detail: format!(
                "MSK Express accepts {key} only within [{lo_text}, {hi_text}]. \
                 {subject} sets it to {}, which is outside that range, so this \
                 configuration can't be migrated as-is.",
                py_repr(value)
            ),
            observed: json_int(integer),
            limit,
        });
    }
    None
}

/// Parses `3.6`, `3.6.0`, `3.6.1` into its numeric parts. `None` input means
/// the version is unknown (the discovery contract allows `null`).
///
/// # Errors
///
/// Returns the legacy message for text that does not start with
/// `<digits>.<digits>`.
pub fn parse_version(raw: Option<&str>) -> Result<Option<Vec<u64>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim_start();
    let mut parts: Vec<u64> = Vec::new();
    let mut rest = trimmed;
    for index in 0..3 {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            break;
        }
        parts.push(rest[..digits_end].parse().unwrap_or(u64::MAX));
        rest = &rest[digits_end..];
        if index < 2 && rest.starts_with('.') {
            rest = &rest[1..];
        } else {
            break;
        }
    }
    // The pattern requires at least major.minor; a trailing "." that is not
    // followed by digits ends the match after the previous number.
    if parts.len() < 2 {
        return Err(format!("Unparseable Kafka version: {}", py_str_repr(raw)));
    }
    Ok(Some(parts))
}

fn version_of(cfg: &Json) -> Result<(Json, Option<Vec<u64>>), String> {
    let raw = cfg
        .get("kafka")
        .and_then(|kafka| kafka.get("version"))
        .cloned()
        .unwrap_or(Json::Null);
    let parsed = match &raw {
        Json::Null => None,
        Json::String(text) => parse_version(Some(text))?,
        other => {
            return Err(format!(
                "kafka.version must be a string or null, not {}",
                other.type_name()
            ))
        }
    };
    Ok((raw, parsed))
}

// ---------------------------------------------------------------------------
// Pillar 1 - Topology
// ---------------------------------------------------------------------------

fn assess_topology(cfg: &Json) -> Result<Pillar, String> {
    let mut evidence = Vec::new();
    let mut verdict = Severity::Info;
    let topology = cfg.get("topology").unwrap_or(&NULL);
    let broker_count = py_int(
        topology.get("num_brokers").unwrap_or(&NULL),
        "topology.num_brokers",
    )?;
    let az = get_present(topology, "num_azs");
    let is_zookeeper = cfg
        .get("kafka")
        .and_then(|kafka| kafka.get("coordination_mechanism"))
        .and_then(Json::as_str)
        == Some("ZooKeeper");
    let (_, parsed) = version_of(cfg)?;
    let version = if let Some(version) = parsed {
        version
    } else {
        evidence.push(Evidence::new(
            "VERSION_UNKNOWN",
            Severity::Advisory,
            "We couldn't determine your cluster's Apache Kafka version, so \
             we can't check the KRaft-on-3.9 transition or the supported \
             version set. Provide `kafka.version` for a complete \
             assessment.",
        ));
        verdict = verdict.worst(Severity::Advisory);
        // Neutral placeholder; skips the version-specific check below.
        vec![0, 0]
    };

    match az {
        None => {
            evidence.push(Evidence::new(
                "AZ_COUNT_UNKNOWN",
                Severity::Advisory,
                "We couldn't determine how many Availability Zones your cluster \
                 uses. MSK Express always deploys across 3 Availability Zones, \
                 so your MSK Express cluster will use 3 regardless.",
            ));
            verdict = verdict.worst(Severity::Advisory);
        }
        Some(az) => {
            let observed = py_int(az, "topology.num_azs")?;
            if observed != EXPRESS_AZ_COUNT {
                evidence.push(
                    Evidence::new(
                        "AZ_COUNT_NOT_3",
                        Severity::Advisory,
                        format!(
                            "Your cluster spans {} Availability Zone(s). MSK Express \
                             always deploys across {EXPRESS_AZ_COUNT} Availability Zones, \
                             so your MSK Express cluster will use {EXPRESS_AZ_COUNT} \
                             regardless.",
                            py_str(az)
                        ),
                    )
                    .with("observed", json_int(observed))
                    .with("required", json_int(EXPRESS_AZ_COUNT)),
                );
                verdict = verdict.worst(Severity::Advisory);
            }
        }
    }

    // Express requires at least 3 brokers (RF=3 across 3 AZs). The target
    // broker count is sized by the workbook, so this is a note, not a check.
    if broker_count < EXPRESS_TARGET_MIN_BROKERS {
        evidence.push(
            Evidence::new(
                "BROKER_COUNT_LT_3",
                Severity::Advisory,
                format!(
                    "Your cluster has {broker_count} broker(s). MSK Express uses a minimum \
                     of {EXPRESS_TARGET_MIN_BROKERS} brokers across 3 Availability \
                     Zones. The sizing workbook helps you choose your MSK Express \
                     broker count based on your throughput, so it isn't carried \
                     over from your current cluster."
                ),
            )
            .with("observed", json_int(broker_count))
            .with("required", json_int(EXPRESS_TARGET_MIN_BROKERS)),
        );
        verdict = verdict.worst(Severity::Advisory);
    }

    if version[..2] == [3, 9] && is_zookeeper {
        evidence.push(Evidence::new(
            "KRAFT_REQUIRED_FOR_VERSION",
            Severity::Advisory,
            "Your cluster runs Kafka 3.9 with ZooKeeper. On Kafka 3.9, MSK \
             Express runs in KRaft mode and is provisioned that way \
             automatically; no action needed. If any of your tooling talks \
             to ZooKeeper directly, see the Apache Kafka KRaft migration \
             documentation: \
             https://kafka.apache.org/37/operations/kraft/#zookeeper-to-kraft-migration",
        ));
        verdict = verdict.worst(Severity::Advisory);
    }

    Ok((verdict, evidence))
}

// ---------------------------------------------------------------------------
// Pillar 2 - Kafka version
// ---------------------------------------------------------------------------

fn assess_kafka_version(cfg: &Json) -> Result<Pillar, String> {
    let (raw, parsed) = version_of(cfg)?;
    let supported: Vec<String> = EXPRESS_SUPPORTED_VERSIONS
        .iter()
        .map(|(major, minor)| format!("{major}.{minor}"))
        .collect();
    let supported_text = supported.join(", ");
    let supported_json = Json::Array(supported.iter().map(Json::string).collect());

    let Some(parsed) = parsed else {
        return Ok((
            Severity::Advisory,
            vec![Evidence::new(
                "VERSION_UNKNOWN",
                Severity::Advisory,
                format!(
                    "We couldn't determine your cluster's Apache Kafka version, so \
                     we can't confirm it's in the set MSK Express supports ({supported_text}). \
                     Provide `kafka.version` for a complete assessment."
                ),
            )
            .with("supported", supported_json)],
        ));
    };

    let raw_text = py_str(&raw);
    let major_minor = (parsed[0], parsed[1]);
    if EXPRESS_SUPPORTED_VERSIONS.contains(&major_minor) {
        return Ok((
            Severity::Info,
            vec![Evidence::new(
                "VERSION_SUPPORTED",
                Severity::Info,
                format!(
                    "Your cluster runs Apache Kafka {raw_text}, which MSK Express \
                     supports ({supported_text}). Your workload will run on the \
                     same Kafka version after migrating."
                ),
            )],
        ));
    }

    let mut message = format!(
        "MSK Express supports Apache Kafka {supported_text}. Your cluster \
         runs {raw_text}, so after migrating your workload will run on a new \
         Kafka version. Confirm your client libraries and applications are \
         compatible with the version you choose for Express \u{2014} Kafka clients \
         are generally compatible across minor versions, but we recommend \
         validating in a test environment before migrating. See the Apache \
         Kafka upgrade notes at https://kafka.apache.org/documentation/#upgrade \
         for details."
    );
    if parsed.as_slice() < MSK_REPLICATOR_MIN_SOURCE_VERSION {
        message.push_str(
            " If intending to migrate your data along with your cluster, \
             please note that MSK Replicator can only copy data from \
             clusters running Apache Kafka 2.8.1 or later. Since your \
             cluster is older, you would need to set up a MirrorMaker 2 \
             based solution instead.",
        );
    }
    Ok((
        Severity::Advisory,
        vec![
            Evidence::new("VERSION_NOT_IN_EXPRESS_SET", Severity::Advisory, message)
                .with("observed", raw)
                .with("supported", supported_json),
        ],
    ))
}

// ---------------------------------------------------------------------------
// Pillar 3 - Configs
// ---------------------------------------------------------------------------

fn lookup<'a, T>(table: &'a [(&str, T)], key: &str) -> Option<&'a T> {
    table.iter().find(|(name, _)| *name == key).map(|(_, v)| v)
}

fn config_map<'a>(value: Option<&'a Json>, what: &str) -> Result<&'a [(String, Json)], String> {
    match value {
        Some(found) if truthy(found) => object_of(found, what),
        _ => Ok(&[]),
    }
}

fn assess_broker_configs(
    cfg: &Json,
    verdict: &mut Severity,
    evidence: &mut Vec<Evidence>,
) -> Result<(), String> {
    for (key, val) in config_map(cfg.get("broker_configs"), "broker_configs")? {
        let key = key.as_str();
        if is_default(key, val, BROKER_DEFAULTS) {
            continue;
        }

        if let Some(bounds) = lookup(EXPRESS_BROKER_RANGES, key) {
            if let Some(problem) = check_range(key, val, *bounds, "Your cluster") {
                evidence.push(
                    Evidence::new(
                        "BROKER_CONFIG_OUT_OF_RANGE",
                        Severity::ActionRequired,
                        problem.detail,
                    )
                    .with("config_key", Json::string(problem.config_key))
                    .with("observed", problem.observed)
                    .with("limit", problem.limit),
                );
                *verdict = verdict.worst(Severity::ActionRequired);
                continue;
            }
        }

        if let Some(forced) = lookup(EXPRESS_BROKER_FORCED, key) {
            let forced_json = forced.json();
            if normalize_value(val) != normalize_value(&forced_json) {
                let forced_repr = py_repr(&forced_json);
                let val_repr = py_repr(val);
                evidence.push(
                    Evidence::new(
                        "BROKER_CONFIG_FORCED_VALUE",
                        Severity::Advisory,
                        format!(
                            "MSK Express manages {key} for you and sets it to \
                             {forced_repr} to keep your cluster highly available and \
                             durable; your current value of {val_repr} won't apply. We \
                             recommend confirming your workload behaves as expected \
                             with {key}={forced_repr} in a test environment to ensure \
                             a smooth migration."
                        ),
                    )
                    .with("config_key", Json::string(key))
                    .with("observed", val.clone())
                    .with("enforced", forced_json),
                );
                *verdict = verdict.worst(Severity::Advisory);
            }
        } else if EXPRESS_BROKER_RO.contains(&key) {
            evidence.push(
                Evidence::new(
                    "BROKER_CONFIG_READ_ONLY",
                    Severity::Advisory,
                    format!(
                        "MSK Express manages {key} for you so you don't have to \
                         tune it (for thread-related settings it's sized \
                         automatically from the broker instance type); your current \
                         value won't apply. No action needed."
                    ),
                )
                .with("config_key", Json::string(key))
                .with("observed", val.clone()),
            );
            *verdict = verdict.worst(Severity::Advisory);
        } else if !EXPRESS_BROKER_RW.contains(&key) {
            evidence.push(
                Evidence::new(
                    "BROKER_CONFIG_NOT_EXPOSED",
                    Severity::Advisory,
                    format!(
                        "{key} isn't a configurable property on MSK Express \u{2014} \
                         Express manages it internally, and the behavior may differ \
                         from your current value of {}. We recommend \
                         validating in a test environment to ensure a smooth \
                         migration.",
                        py_repr(val)
                    ),
                )
                .with("config_key", Json::string(key))
                .with("observed", val.clone()),
            );
            *verdict = verdict.worst(Severity::Advisory);
        }
        // Otherwise the key is Express read/write and within range: INFO.
    }
    Ok(())
}

fn assess_topic_configs(
    cfg: &Json,
    verdict: &mut Severity,
    evidence: &mut Vec<Evidence>,
) -> Result<(), String> {
    let topics: &[Json] = match cfg.get("topics") {
        None => &[],
        Some(Json::Array(items)) => items,
        Some(_) => return Err("topics must be a list".to_string()),
    };
    for (index, topic) in topics.iter().enumerate() {
        let what = format!("topics[{index}]");
        object_of(topic, &what)?;
        let name = topic
            .get("name")
            .ok_or_else(|| format!("{what} is missing required field 'name'"))?;
        let rf = py_int(
            topic
                .get("replication_factor")
                .ok_or_else(|| format!("{what} is missing required field 'replication_factor'"))?,
            &format!("{what}.replication_factor"),
        )?;
        let name_repr = py_repr(name);

        if rf != EXPRESS_TOPIC_FORCED_RF {
            evidence.push(
                Evidence::new(
                    "TOPIC_RF_NOT_3",
                    Severity::Advisory,
                    format!(
                        "Topic {name_repr} uses replication factor {rf}. MSK Express \
                         creates every topic with replication factor 3 for \
                         durability, so on MSK Express this topic will use 3. No \
                         action needed."
                    ),
                )
                .with("topic", name.clone())
                .with("observed", json_int(rf))
                .with("enforced", json_int(EXPRESS_TOPIC_FORCED_RF)),
            );
            *verdict = verdict.worst(Severity::Advisory);
        }

        for (key, val) in config_map(topic.get("configs"), &format!("{what}.configs"))? {
            let key = key.as_str();
            if is_default(key, val, TOPIC_DEFAULTS) {
                continue;
            }

            if let Some(bounds) = lookup(EXPRESS_TOPIC_RANGES, key) {
                let subject = format!("Topic {name_repr}");
                if let Some(problem) = check_range(key, val, *bounds, &subject) {
                    evidence.push(
                        Evidence::new(
                            "TOPIC_CONFIG_OUT_OF_RANGE",
                            Severity::ActionRequired,
                            problem.detail,
                        )
                        .with("topic", name.clone())
                        .with("config_key", Json::string(problem.config_key))
                        .with("observed", problem.observed)
                        .with("limit", problem.limit),
                    );
                    *verdict = verdict.worst(Severity::ActionRequired);
                    continue;
                }
            }

            if let Some(forced) = lookup(TOPIC_FORCED, key) {
                let forced_json = forced.json();
                if normalize_value(val) != normalize_value(&forced_json) {
                    let forced_repr = py_repr(&forced_json);
                    let val_repr = py_repr(val);
                    evidence.push(
                        Evidence::new(
                            "TOPIC_CONFIG_FORCED_VALUE",
                            Severity::Advisory,
                            format!(
                                "Topic {name_repr} sets {key}={val_repr}. MSK Express \
                                 fixes this at {forced_repr} for every topic to keep \
                                 your data durable, so your current value won't \
                                 apply. We recommend confirming this topic behaves \
                                 as expected with {key}={forced_repr} in a test \
                                 environment to ensure a smooth migration."
                            ),
                        )
                        .with("topic", name.clone())
                        .with("config_key", Json::string(key))
                        .with("observed", val.clone())
                        .with("enforced", forced_json),
                    );
                    *verdict = verdict.worst(Severity::Advisory);
                }
            } else if !EXPRESS_TOPIC_RW.contains(&key) {
                evidence.push(
                    Evidence::new(
                        "TOPIC_CONFIG_NOT_EXPOSED",
                        Severity::Advisory,
                        format!(
                            "Topic {name_repr} sets {key}={}, which isn't a \
                             configurable topic property on MSK Express \u{2014} Express \
                             uses the broker default instead. We recommend \
                             validating that the default works for this topic in a \
                             test environment to ensure a smooth migration.",
                            py_repr(val)
                        ),
                    )
                    .with("topic", name.clone())
                    .with("config_key", Json::string(key))
                    .with("observed", val.clone()),
                );
                *verdict = verdict.worst(Severity::Advisory);
            }
        }
    }
    Ok(())
}

fn assess_configs(cfg: &Json) -> Result<Pillar, String> {
    let mut evidence = Vec::new();
    let mut verdict = Severity::Info;
    let (_, parsed) = version_of(cfg)?;
    if parsed.is_none() {
        evidence.push(Evidence::new(
            "VERSION_UNKNOWN",
            Severity::Advisory,
            "We couldn't determine your cluster's Apache Kafka version, so \
             config defaults are compared against a generic baseline \
             instead of your source version's actual defaults. Some \
             config divergences may be under- or over-reported as a \
             result. Provide `kafka.version` for a complete assessment.",
        ));
        verdict = verdict.worst(Severity::Advisory);
    }
    assess_broker_configs(cfg, &mut verdict, &mut evidence)?;
    assess_topic_configs(cfg, &mut verdict, &mut evidence)?;
    Ok((verdict, evidence))
}

// ---------------------------------------------------------------------------
// Pillar 4 - Auth
// ---------------------------------------------------------------------------

fn security_text(cfg: &Json, key: &str) -> Result<Option<String>, String> {
    let Some(security) = cfg.get("security").filter(|value| truthy(value)) else {
        return Ok(None);
    };
    object_of(security, "security")?;
    Ok(security
        .get(key)
        .and_then(Json::as_str)
        .map(str::to_string))
}

fn assess_auth(cfg: &Json) -> Result<Pillar, String> {
    let mut evidence = Vec::new();
    let mut verdict = Severity::Info;
    // A missing or blank field is UNKNOWN rather than a hard failure, so an
    // absent security block yields a calm advisory instead of a false
    // "not TLS" finding.
    let encryption = security_text(cfg, "encryption_in_transit")?
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "UNKNOWN".to_string());
    let authentication = security_text(cfg, "authentication")?
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "UNKNOWN".to_string());

    // MSK Express supports unauthenticated, TLS, SASL/SCRAM and IAM. A custom
    // OAuth provider behind SASL_OAUTHBEARER is not the AWS IAM path, and
    // OTHER mechanisms are unsupported. UNKNOWN cannot be confirmed.
    match authentication.as_str() {
        "SASL_OAUTHBEARER" => {
            evidence.push(
                Evidence::new(
                    "AUTH_OAUTHBEARER_NOT_SUPPORTED",
                    Severity::ActionRequired,
                    "Your cluster uses SASL/OAUTHBEARER. MSK Express accepts \
                     OAUTHBEARER only as a transport for AWS IAM tokens (using the \
                     AWS MSK IAM signer libraries), so a custom OAuth identity \
                     provider isn't supported. Move your clients to IAM, \
                     SASL/SCRAM, or TLS to ensure a smooth migration.",
                )
                .with("observed", Json::string(authentication.clone())),
            );
            verdict = verdict.worst(Severity::ActionRequired);
        }
        "OTHER" => {
            evidence.push(
                Evidence::new(
                    "AUTH_MECHANISM_NOT_SUPPORTED",
                    Severity::ActionRequired,
                    "Your cluster uses an authentication mechanism MSK Express \
                     doesn't support (for example SASL/GSSAPI/Kerberos, SASL/PLAIN, \
                     or a custom callback handler). MSK Express accepts \
                     unauthenticated, TLS, SASL/SCRAM, and IAM \u{2014} move your clients \
                     to one of these to ensure a smooth migration.",
                )
                .with("observed", Json::string(authentication.clone())),
            );
            verdict = verdict.worst(Severity::ActionRequired);
        }
        "UNKNOWN" => {
            evidence.push(
                Evidence::new(
                    "AUTH_UNKNOWN",
                    Severity::Advisory,
                    "We couldn't determine your cluster's authentication \
                     mechanism. Confirm it's one MSK Express supports \u{2014} one of \
                     unauthenticated, TLS, SASL/SCRAM, or IAM \u{2014} to ensure a smooth \
                     migration. MSK Express doesn't support SASL/GSSAPI/Kerberos, \
                     SASL/PLAIN, or custom mechanisms.",
                )
                .with("observed", Json::string(authentication.clone())),
            );
            verdict = verdict.worst(Severity::Advisory);
        }
        _ => {}
    }

    // Express requires TLS for every authenticated mechanism; plaintext is only
    // possible with unauthenticated access.
    if encryption == "UNKNOWN" {
        evidence.push(
            Evidence::new(
                "ENCRYPTION_UNKNOWN",
                Severity::Advisory,
                "We couldn't determine your cluster's encryption in transit. \
                 MSK Express requires TLS for authenticated clients (TLS, \
                 SASL/SCRAM, or IAM) and supports PLAINTEXT only for \
                 unauthenticated clients. Please confirm your clients can \
                 connect over TLS, if they are authenticated, to ensure a \
                 smooth migration.",
            )
            .with("observed", Json::string(encryption.clone())),
        );
        verdict = verdict.worst(Severity::Advisory);
    } else if encryption != "TLS" && TLS_REQUIRED_AUTH_VALUES.contains(&authentication.as_str()) {
        evidence.push(
            Evidence::new(
                "ENCRYPTION_NOT_TLS",
                Severity::ActionRequired,
                format!(
                    "Your cluster uses {} for encryption in transit, \
                     but MSK Express requires TLS for authenticated clients (TLS, \
                     SASL/SCRAM, IAM). Update your clients to connect over TLS to \
                     ensure a smooth migration.",
                    py_str_repr(&encryption)
                ),
            )
            .with("observed", Json::string(encryption.clone())),
        );
        verdict = verdict.worst(Severity::ActionRequired);
    }

    Ok((verdict, evidence))
}

// ---------------------------------------------------------------------------
// Pillar 5 - Quotas
// ---------------------------------------------------------------------------

/// A numeric metric: the original JSON value and its `f64` reading.
fn metric(metrics: &Json, key: &str) -> Result<Option<(Json, f64)>, String> {
    match metrics.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(value @ Json::Int(number)) => Ok(Some((value.clone(), *number as f64))),
        Some(value @ Json::Float(number)) => Ok(Some((value.clone(), *number))),
        Some(other) => Err(format!(
            "metrics.{key} must be a number or null, not {}",
            other.type_name()
        )),
    }
}

fn over_limit_evidence(
    code: &'static str,
    observed: Json,
    limit: i64,
    detail: impl FnOnce(&str) -> String,
) -> Evidence {
    let shown = py_str(&observed);
    Evidence::new(code, Severity::Advisory, detail(&shown))
        .with("observed", observed)
        .with("limit", Json::Int(limit))
}

fn assess_quotas(cfg: &Json) -> Result<Pillar, String> {
    let mut evidence = Vec::new();
    let mut verdict = Severity::Info;
    let Some(metrics) = get_present(cfg, "metrics") else {
        evidence.push(Evidence::new(
            "METRICS_MISSING",
            Severity::Advisory,
            "We don't have utilization metrics for your cluster, so we \
             can't check your peak throughput, partitions, and connections \
             against the MSK Express per-broker limits. The sizing estimate \
             falls back to topology only. We recommend editing the sizing \
             sheet manually once these metrics are available and reviewing \
             the Express broker quotas to confirm your workload fits.",
        ));
        return Ok((Severity::Advisory, evidence));
    };
    object_of(metrics, "metrics")?;

    let ingress = metric(metrics, "peak_bytes_in_per_broker_mbps")?;
    if let Some((json, number)) = &ingress {
        if *number > EXPRESS_MAX_INGRESS_MAX_QUOTA_PER_BROKER_MBPS as f64 {
            evidence.push(over_limit_evidence(
                "INGRESS_OVER_MAX_BROKER",
                json.clone(),
                EXPRESS_MAX_INGRESS_MAX_QUOTA_PER_BROKER_MBPS,
                |shown| {
                    format!(
                        "Your peak ingress of {shown} MBps per broker is more than any \
                         single MSK Express broker can absorb (the maximum is \
                         {EXPRESS_MAX_INGRESS_MAX_QUOTA_PER_BROKER_MBPS} MBps, on \
                         m7g.16xlarge). Spread the load across more brokers so each \
                         carries less ingress. Note that the sizing workbook accounts \
                         for this already and can help you select the right MSK Express \
                         broker instance type."
                    )
                },
            ));
            verdict = verdict.worst(Severity::Advisory);
        }
    }

    if let Some((json, number)) = metric(metrics, "peak_bytes_out_per_broker_mbps")? {
        if number > EXPRESS_MAX_EGRESS_MAX_QUOTA_PER_BROKER_MBPS as f64 {
            evidence.push(over_limit_evidence(
                "EGRESS_OVER_MAX_BROKER",
                json,
                EXPRESS_MAX_EGRESS_MAX_QUOTA_PER_BROKER_MBPS,
                |shown| {
                    format!(
                        "Your peak egress of {shown} MBps per broker is more than any \
                         single MSK Express broker can absorb (the maximum is \
                         {EXPRESS_MAX_EGRESS_MAX_QUOTA_PER_BROKER_MBPS} MBps, on \
                         m7g.16xlarge). Spread the load across more brokers so each \
                         carries less egress. Note that the sizing workbook accounts \
                         for this already and can help you select the right MSK Express \
                         broker instance type."
                    )
                },
            ));
            verdict = verdict.worst(Severity::Advisory);
        }
    }

    if let Some((json, number)) = metric(metrics, "peak_partitions_per_broker")? {
        if number > EXPRESS_MAX_PARTITIONS_PER_BROKER as f64 {
            evidence.push(over_limit_evidence(
                "PARTITIONS_OVER_MAX_BROKER",
                json,
                EXPRESS_MAX_PARTITIONS_PER_BROKER,
                |shown| {
                    format!(
                        "Your peak of {shown} partitions per broker is above the MSK \
                         Express maximum ({EXPRESS_MAX_PARTITIONS_PER_BROKER}, on \
                         m7g.16xlarge). Adding brokers spreads the partitions so each \
                         one carries fewer. Note that the sizing workbook accounts for \
                         this already and can help you select the right MSK Express \
                         broker instance type."
                    )
                },
            ));
            verdict = verdict.worst(Severity::Advisory);
        }
    }

    let authentication = cfg
        .get("security")
        .filter(|value| truthy(value))
        .and_then(|security| security.get("authentication"))
        .and_then(Json::as_str);
    let connections = if authentication.is_some_and(|auth| IAM_AUTHENTICATION_VALUES.contains(&auth)) {
        metric(metrics, "peak_connections_per_broker")?
    } else {
        None
    };
    if let Some((json, number)) = connections {
        if number > EXPRESS_MAX_IAM_CONNS_PER_BROKER as f64 {
            evidence.push(over_limit_evidence(
                "CONNECTIONS_OVER_IAM_LIMIT",
                json,
                EXPRESS_MAX_IAM_CONNS_PER_BROKER,
                |shown| {
                    format!(
                        "Your peak of {shown} connections per broker is above the MSK \
                         Express IAM limit of {EXPRESS_MAX_IAM_CONNS_PER_BROKER} per \
                         broker. You can spread the connections across more brokers, or \
                         use a non-IAM authentication mechanism, which MSK Express \
                         doesn't cap (monitor CPU and memory instead)."
                    )
                },
            ));
            verdict = verdict.worst(Severity::Advisory);
        }
    }

    // Per-partition throughput approximation.
    let broker_count = py_int(
        cfg.get("topology")
            .and_then(|topology| topology.get("num_brokers"))
            .unwrap_or(&NULL),
        "topology.num_brokers",
    )?;
    let mut total_partitions: i128 = 0;
    if let Some(Json::Array(topics)) = cfg.get("topics") {
        for (index, topic) in topics.iter().enumerate() {
            let what = format!("topics[{index}]");
            total_partitions += py_int(
                topic
                    .get("num_partitions")
                    .ok_or_else(|| format!("{what} is missing required field 'num_partitions'"))?,
                &format!("{what}.num_partitions"),
            )?;
        }
    }
    if let (Some((_, number)), true) = (&ingress, total_partitions > 0) {
        let per_partition = (*number * broker_count as f64) / total_partitions as f64;
        if per_partition > EXPRESS_PARTITION_THROUGHPUT_MBPS as f64 {
            evidence.push(
                Evidence::new(
                    "PARTITION_THROUGHPUT_OVER_LIMIT",
                    Severity::Advisory,
                    format!(
                        "Your average per-partition throughput of \
                         {} MB/s is above the MSK Express limit \
                         of {EXPRESS_PARTITION_THROUGHPUT_MBPS} MB/s, where Express \
                         begins to throttle. Individual hot partitions can throttle \
                         your clients. We recommend spreading the busiest topics \
                         across more partitions before migrating.",
                        format_fixed(per_partition, 1, false)
                    ),
                )
                .with("observed", Json::Float(round_to(per_partition, 1)))
                .with("limit", Json::Int(EXPRESS_PARTITION_THROUGHPUT_MBPS)),
            );
            verdict = verdict.worst(Severity::Advisory);
        }
    }

    Ok((verdict, evidence))
}

// ---------------------------------------------------------------------------
// Input validation and orchestration
// ---------------------------------------------------------------------------

fn missing_fields(block: &Json, required: &[&str]) -> Option<Json> {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|key| block.get(key).is_none())
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(list_of_str(&missing))
    }
}

fn check_enum(
    security: &Json,
    key: &str,
    allowed: &'static [&'static str],
) -> Result<(), String> {
    match security.get(key) {
        None | Some(Json::Null) => Ok(()),
        Some(Json::String(text)) if allowed.contains(&text.as_str()) => Ok(()),
        Some(other) => Err(format!(
            "security.{key}={} is not in the allowed enum {}",
            py_repr(other),
            py_repr(&list_of_str(allowed))
        )),
    }
}

/// Validates the discovery contract: required fields, closed enums for the
/// security block and the target block's cost assumptions.
///
/// # Errors
///
/// Returns the first violated rule with the legacy message.
pub fn validate_input(cfg: &Json) -> Result<(), String> {
    object_of(cfg, "discovery contract")?;
    if let Some(missing) = missing_fields(cfg, &["cluster_name", "kafka", "topology", "topics"]) {
        return Err(format!(
            "discovery contract missing top-level fields: {}",
            py_repr(&missing)
        ));
    }
    let kafka = cfg.get("kafka").unwrap_or(&NULL);
    object_of(kafka, "kafka")?;
    if let Some(missing) = missing_fields(kafka, &["version"]) {
        return Err(format!(
            "kafka block missing required fields: {}",
            py_repr(&missing)
        ));
    }
    let topology = cfg.get("topology").unwrap_or(&NULL);
    object_of(topology, "topology")?;
    if let Some(missing) = missing_fields(topology, &["num_brokers"]) {
        return Err(format!(
            "topology block missing required fields: {}",
            py_repr(&missing)
        ));
    }
    if !matches!(cfg.get("topics"), Some(Json::Array(_))) {
        return Err("topics must be a list".to_string());
    }

    // Both security fields are optional, but any value supplied must be from
    // the closed enum.
    if let Some(security) = get_present(cfg, "security") {
        object_of(security, "security")?;
        check_enum(security, "encryption_in_transit", ENCRYPTION_VALUES)?;
        check_enum(security, "authentication", AUTHENTICATION_VALUES)?;
    }

    // The target block carries sizing cost assumptions, not compatibility
    // inputs; validate it so a malformed contract fails before sizing.
    if let Some(target) = get_present(cfg, "target") {
        object_of(target, "target")?;
        if let Some(rack) = get_present(target, "rack_affined_consumers") {
            if !matches!(rack, Json::Bool(_)) {
                return Err(format!(
                    "target.rack_affined_consumers must be true, false, or null; got {}",
                    py_repr(rack)
                ));
            }
        }
        if let Some(discount) = get_present(target, "pricing_discount_pct") {
            let number = match discount {
                Json::Int(number) => *number as f64,
                Json::Float(number) => *number,
                other => {
                    return Err(format!(
                        "target.pricing_discount_pct must be a number or null; got {}",
                        py_repr(other)
                    ))
                }
            };
            if !(0.0..100.0).contains(&number) {
                return Err(format!(
                    "target.pricing_discount_pct must be >= 0 and < 100; got {}",
                    py_repr(discount)
                ));
            }
        }
    }
    Ok(())
}

/// Runs all five pillars and returns the compatibility document.
///
/// `assessed_at` is the `YYYY-MM-DDTHH:MM:SSZ` timestamp to record; callers
/// supply it so the assessment itself stays deterministic.
///
/// # Errors
///
/// Returns a message when the contract is invalid or a value has an
/// unusable type.
pub fn assess(cfg: &Json, assessed_at: &str) -> Result<Json, String> {
    validate_input(cfg)?;
    let pillars: [(&str, fn(&Json) -> Result<Pillar, String>); 5] = [
        ("topology", assess_topology),
        ("kafka_version", assess_kafka_version),
        ("configs", assess_configs),
        ("auth", assess_auth),
        ("quotas", assess_quotas),
    ];

    let mut pillar_docs = Vec::new();
    let mut overall = Severity::Info;
    let (mut action_required, mut advisory, mut info) = (Vec::new(), Vec::new(), Vec::new());
    for (name, assess_pillar) in pillars {
        let (verdict, evidence) = assess_pillar(cfg)?;
        overall = overall.worst(verdict);
        // Bucket each finding by its own severity, not the pillar's verdict.
        for finding in &evidence {
            let bucket = match finding.severity {
                Severity::ActionRequired => &mut action_required,
                Severity::Advisory => &mut advisory,
                Severity::Info => &mut info,
            };
            bucket.push(Json::string(finding.code));
        }
        pillar_docs.push((
            name.to_string(),
            Json::object(vec![
                ("verdict", Json::string(verdict.as_str())),
                (
                    "evidence",
                    Json::Array(evidence.iter().map(Evidence::to_json).collect()),
                ),
            ]),
        ));
    }

    Ok(Json::object(vec![
        (
            "cluster_name",
            cfg.get("cluster_name").cloned().unwrap_or(Json::Null),
        ),
        ("assessed_at", Json::string(assessed_at)),
        ("overall", Json::string(overall.as_str())),
        ("pillars", Json::Object(pillar_docs)),
        (
            "summary",
            Json::object(vec![
                ("action_required_codes", Json::Array(action_required)),
                ("advisory_codes", Json::Array(advisory)),
                ("info_codes", Json::Array(info)),
            ]),
        ),
    ]))
}

/// Output file name `compatibility.<cluster_name>.json`.
///
/// # Errors
///
/// The legacy script interpolated the name unchecked, so a name such as
/// `../x` wrote outside the output directory. Names containing a path
/// separator, a NUL byte or only dots are refused.
pub fn output_file_name(cluster_name: &Json) -> Result<String, String> {
    let name = py_str(cluster_name);
    if name.is_empty()
        || name.contains(['/', '\\', '\0'])
        || name.chars().all(|c| c == '.')
        || name.chars().any(char::is_control)
    {
        return Err(format!(
            "cluster_name {} cannot be used in an output file name \
             (it must be non-empty and contain no path separators or control characters)",
            py_str_repr(&name)
        ));
    }
    Ok(format!("compatibility.{name}.json"))
}

/// Days since 1970-01-01 → (year, month, day) in the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `YYYY-MM-DD` (UTC) for Unix seconds.
#[must_use]
pub fn format_utc_date(unix_seconds: i64) -> String {
    let (year, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC) for Unix seconds.
#[must_use]
pub fn format_utc_timestamp(unix_seconds: i64) -> String {
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        format_utc_date(unix_seconds),
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    const AT: &str = "2026-01-01T00:00:00Z";

    /// Builds a discovery document from defaults plus overrides. An override
    /// value of `-` removes the key.
    fn config(overrides: &[(&str, &str)]) -> Json {
        let mut fields: Vec<(String, String)> = vec![
            ("cluster_name".into(), "\"demo\"".into()),
            (
                "kafka".into(),
                "{\"version\": \"3.6.0\", \"coordination_mechanism\": \"KRaft\"}".into(),
            ),
            ("topology".into(), "{\"num_brokers\": 3, \"num_azs\": 3}".into()),
            ("topics".into(), "[]".into()),
        ];
        for (key, value) in overrides {
            fields.retain(|(existing, _)| existing != key);
            if *value != "-" {
                fields.push(((*key).to_string(), (*value).to_string()));
            }
        }
        let body: Vec<String> = fields
            .iter()
            .map(|(key, value)| format!("\"{key}\": {value}"))
            .collect();
        parse_json(&format!("{{{}}}", body.join(", "))).expect("valid fixture JSON")
    }

    fn assessed(overrides: &[(&str, &str)]) -> Json {
        assess(&config(overrides), AT).expect("assessment")
    }

    fn pillar<'a>(doc: &'a Json, name: &str) -> &'a Json {
        doc.get("pillars")
            .and_then(|pillars| pillars.get(name))
            .unwrap_or_else(|| panic!("pillar {name}"))
    }

    fn verdict(doc: &Json, name: &str) -> String {
        pillar(doc, name)
            .get("verdict")
            .and_then(Json::as_str)
            .expect("verdict")
            .to_string()
    }

    fn evidence<'a>(doc: &'a Json, name: &str) -> &'a [Json] {
        match pillar(doc, name).get("evidence") {
            Some(Json::Array(items)) => items,
            _ => panic!("evidence array"),
        }
    }

    fn codes(doc: &Json, name: &str) -> Vec<(String, String)> {
        evidence(doc, name)
            .iter()
            .map(|item| {
                (
                    item.get("code").and_then(Json::as_str).expect("code").to_string(),
                    item.get("severity").and_then(Json::as_str).expect("severity").to_string(),
                )
            })
            .collect()
    }

    fn code_list(doc: &Json, name: &str) -> Vec<String> {
        codes(doc, name).into_iter().map(|(code, _)| code).collect()
    }

    fn finding<'a>(doc: &'a Json, name: &str, code: &str) -> &'a Json {
        evidence(doc, name)
            .iter()
            .find(|item| item.get("code").and_then(Json::as_str) == Some(code))
            .unwrap_or_else(|| panic!("no {code} finding in {name}"))
    }

    fn detail(doc: &Json, name: &str, code: &str) -> String {
        finding(doc, name, code)
            .get("detail")
            .and_then(Json::as_str)
            .expect("detail")
            .to_string()
    }

    fn error_of(overrides: &[(&str, &str)]) -> String {
        assess(&config(overrides), AT).expect_err("must be rejected")
    }

    // --- orchestration -------------------------------------------------

    #[test]
    fn a_clean_cluster_is_info_everywhere_except_missing_metrics() {
        let doc = assessed(&[
            ("security", "{\"encryption_in_transit\": \"TLS\", \"authentication\": \"SASL_IAM\"}"),
            ("metrics", "{}"),
        ]);
        assert_eq!(doc.get("overall").and_then(Json::as_str), Some("INFO"));
        for name in ["topology", "configs", "auth", "quotas"] {
            assert_eq!(verdict(&doc, name), "INFO", "{name}");
            assert!(evidence(&doc, name).is_empty(), "{name}");
        }
        assert_eq!(code_list(&doc, "kafka_version"), ["VERSION_SUPPORTED"]);
        assert_eq!(verdict(&doc, "kafka_version"), "INFO");
        let summary = doc.get("summary").expect("summary");
        assert_eq!(
            summary.get("info_codes"),
            Some(&Json::Array(vec![Json::string("VERSION_SUPPORTED")]))
        );
        assert_eq!(summary.get("advisory_codes"), Some(&Json::Array(vec![])));
        assert_eq!(summary.get("action_required_codes"), Some(&Json::Array(vec![])));
    }

    #[test]
    fn the_document_has_the_published_shape_and_key_order() {
        let doc = assessed(&[]);
        let Json::Object(pairs) = &doc else {
            panic!("object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["cluster_name", "assessed_at", "overall", "pillars", "summary"]);
        assert_eq!(doc.get("cluster_name").and_then(Json::as_str), Some("demo"));
        assert_eq!(doc.get("assessed_at").and_then(Json::as_str), Some(AT));
        let Some(Json::Object(pillars)) = doc.get("pillars") else {
            panic!("pillars");
        };
        let names: Vec<&str> = pillars.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(names, ["topology", "kafka_version", "configs", "auth", "quotas"]);
        let first = finding(&assessed(&[("topology", "{\"num_brokers\": 1, \"num_azs\": 3}")]), "topology", "BROKER_COUNT_LT_3").clone();
        let Json::Object(fields) = first else {
            panic!("evidence object");
        };
        let order: Vec<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(order, ["code", "severity", "detail", "observed", "required"]);
    }

    #[test]
    fn overall_is_the_worst_verdict_and_summary_buckets_per_finding() {
        let doc = assessed(&[
            ("topology", "{\"num_brokers\": 2, \"num_azs\": 3}"),
            (
                "security",
                "{\"encryption_in_transit\": \"PLAINTEXT\", \"authentication\": \"SASL_SCRAM\"}",
            ),
        ]);
        assert_eq!(doc.get("overall").and_then(Json::as_str), Some("ACTION_REQUIRED"));
        let summary = doc.get("summary").expect("summary");
        let list = |key: &str| match summary.get(key) {
            Some(Json::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>(),
            _ => panic!("summary list"),
        };
        assert_eq!(list("action_required_codes"), ["ENCRYPTION_NOT_TLS"]);
        assert_eq!(
            list("advisory_codes"),
            ["BROKER_COUNT_LT_3", "METRICS_MISSING"]
        );
        assert_eq!(list("info_codes"), ["VERSION_SUPPORTED"]);
    }

    // --- version parsing -----------------------------------------------

    #[test]
    fn versions_parse_like_the_legacy_regex() {
        assert_eq!(parse_version(None), Ok(None));
        assert_eq!(parse_version(Some("3.6")), Ok(Some(vec![3, 6])));
        assert_eq!(parse_version(Some("3.6.0")), Ok(Some(vec![3, 6, 0])));
        assert_eq!(parse_version(Some("  3.9.12-rc1")), Ok(Some(vec![3, 9, 12])));
        assert_eq!(parse_version(Some("3.6.x")), Ok(Some(vec![3, 6])));
        assert_eq!(parse_version(Some("3.6.1.7")), Ok(Some(vec![3, 6, 1])));
        for bad in ["", "3", "3.", "abc", "v3.6", ".6", "3.x"] {
            assert_eq!(
                parse_version(Some(bad)),
                Err(format!("Unparseable Kafka version: '{bad}'")),
                "{bad:?}"
            );
        }
    }

    // --- pillar 1: topology ----------------------------------------------

    #[test]
    fn topology_az_count_rules() {
        let doc = assessed(&[("topology", "{\"num_brokers\": 3, \"num_azs\": 2}")]);
        assert_eq!(verdict(&doc, "topology"), "ADVISORY");
        let item = finding(&doc, "topology", "AZ_COUNT_NOT_3");
        assert_eq!(item.get("observed"), Some(&Json::Int(2)));
        assert_eq!(item.get("required"), Some(&Json::Int(3)));
        assert_eq!(
            detail(&doc, "topology", "AZ_COUNT_NOT_3"),
            "Your cluster spans 2 Availability Zone(s). MSK Express always deploys across 3 \
             Availability Zones, so your MSK Express cluster will use 3 regardless."
        );

        for az in ["3", "\"3\"", "3.0"] {
            let doc = assessed(&[("topology", &format!("{{\"num_brokers\": 3, \"num_azs\": {az}}}"))]);
            assert!(evidence(&doc, "topology").is_empty(), "az {az}");
        }
        for topology in ["{\"num_brokers\": 3}", "{\"num_brokers\": 3, \"num_azs\": null}"] {
            let doc = assessed(&[("topology", topology)]);
            assert_eq!(code_list(&doc, "topology"), ["AZ_COUNT_UNKNOWN"], "{topology}");
            assert_eq!(verdict(&doc, "topology"), "ADVISORY");
        }
    }

    #[test]
    fn topology_broker_floor_is_a_note_not_a_failure() {
        for (brokers, expected) in [(1, true), (2, true), (3, false), (6, false)] {
            let doc = assessed(&[("topology", &format!("{{\"num_brokers\": {brokers}, \"num_azs\": 3}}"))]);
            assert_eq!(
                code_list(&doc, "topology").contains(&"BROKER_COUNT_LT_3".to_string()),
                expected,
                "{brokers} brokers"
            );
        }
        let doc = assessed(&[("topology", "{\"num_brokers\": 2, \"num_azs\": 3}")]);
        let item = finding(&doc, "topology", "BROKER_COUNT_LT_3");
        assert_eq!(item.get("observed"), Some(&Json::Int(2)));
        assert!(detail(&doc, "topology", "BROKER_COUNT_LT_3").starts_with("Your cluster has 2 broker(s). "));
        // num_brokers may be a numeric string, like int() accepts.
        let doc = assessed(&[("topology", "{\"num_brokers\": \"2\", \"num_azs\": 3}")]);
        assert_eq!(code_list(&doc, "topology"), ["BROKER_COUNT_LT_3"]);
    }

    #[test]
    fn kraft_note_only_for_zookeeper_on_three_nine() {
        for (version, coordination, expected) in [
            ("3.9.0", "ZooKeeper", true),
            ("3.9", "ZooKeeper", true),
            ("3.9.0", "KRaft", false),
            ("3.8.1", "ZooKeeper", false),
            ("3.6.0", "ZooKeeper", false),
            ("4.2.0", "ZooKeeper", false),
        ] {
            let kafka = format!(
                "{{\"version\": \"{version}\", \"coordination_mechanism\": \"{coordination}\"}}"
            );
            let doc = assessed(&[("kafka", &kafka)]);
            assert_eq!(
                code_list(&doc, "topology").contains(&"KRAFT_REQUIRED_FOR_VERSION".to_string()),
                expected,
                "{version} {coordination}"
            );
        }
        let doc = assessed(&[("kafka", "{\"version\": \"3.9.0\"}")]);
        assert!(evidence(&doc, "topology").is_empty(), "no coordination field");
    }

    #[test]
    fn unknown_version_is_reported_by_three_pillars() {
        let doc = assessed(&[("kafka", "{\"version\": null}")]);
        for name in ["topology", "kafka_version", "configs"] {
            assert_eq!(code_list(&doc, name), ["VERSION_UNKNOWN"], "{name}");
            assert_eq!(verdict(&doc, name), "ADVISORY", "{name}");
        }
        let item = finding(&doc, "kafka_version", "VERSION_UNKNOWN");
        assert_eq!(
            item.get("supported"),
            Some(&Json::Array(
                ["3.6", "3.8", "3.9", "4.2"].iter().map(|v| Json::string(*v)).collect()
            ))
        );
        assert!(detail(&doc, "configs", "VERSION_UNKNOWN").contains("generic baseline"));
    }

    // --- pillar 2: kafka version -----------------------------------------

    #[test]
    fn supported_versions_are_info() {
        for version in ["3.6", "3.6.0", "3.8.1", "3.9.0", "4.2.0", "3.9.x"] {
            let doc = assessed(&[("kafka", &format!("{{\"version\": \"{version}\"}}"))]);
            assert_eq!(code_list(&doc, "kafka_version"), ["VERSION_SUPPORTED"], "{version}");
            assert_eq!(verdict(&doc, "kafka_version"), "INFO");
        }
        let doc = assessed(&[]);
        assert_eq!(
            detail(&doc, "kafka_version", "VERSION_SUPPORTED"),
            "Your cluster runs Apache Kafka 3.6.0, which MSK Express supports (3.6, 3.8, 3.9, \
             4.2). Your workload will run on the same Kafka version after migrating."
        );
    }

    #[test]
    fn unsupported_versions_are_advisory_and_old_ones_mention_the_replicator() {
        for (version, replicator) in [
            ("3.7.0", false),
            ("3.5.1", false),
            ("4.0.0", false),
            ("2.8.1", false),
            ("2.9", false),
            ("2.8.0", true),
            ("2.8", true),
            ("2.7.2", true),
            ("1.1.1", true),
        ] {
            let doc = assessed(&[("kafka", &format!("{{\"version\": \"{version}\"}}"))]);
            assert_eq!(verdict(&doc, "kafka_version"), "ADVISORY", "{version}");
            let item = finding(&doc, "kafka_version", "VERSION_NOT_IN_EXPRESS_SET");
            assert_eq!(item.get("observed").and_then(Json::as_str), Some(version));
            let text = detail(&doc, "kafka_version", "VERSION_NOT_IN_EXPRESS_SET");
            assert!(text.starts_with("MSK Express supports Apache Kafka 3.6, 3.8, 3.9, 4.2. Your cluster runs "));
            assert_eq!(text.contains("MSK Replicator can only copy data"), replicator, "{version}");
        }
    }

    #[test]
    fn unusable_versions_are_errors() {
        assert_eq!(
            error_of(&[("kafka", "{\"version\": \"banana\"}")]),
            "Unparseable Kafka version: 'banana'"
        );
        assert_eq!(
            error_of(&[("kafka", "{\"version\": 3.6}")]),
            "kafka.version must be a string or null, not number"
        );
    }

    // --- pillar 3: configs -----------------------------------------------

    fn broker(configs: &str) -> Json {
        assessed(&[("broker_configs", configs)])
    }

    #[test]
    fn values_equal_to_the_apache_default_produce_no_evidence() {
        let doc = broker(
            "{\"num.partitions\": \"1\", \"compression.type\": \"Producer\", \
             \"unclean.leader.election.enable\": false, \"auto.create.topics.enable\": true, \
             \"log.retention.ms\": -1, \"log.cleaner.max.compaction.lag.ms\": 9223372036854775807, \
             \"min.insync.replicas\": 1, \"default.replication.factor\": 1, \
             \"log.message.timestamp.type\": \"createtime\", \"num.network.threads\": \" 3 \"}",
        );
        assert!(evidence(&doc, "configs").is_empty(), "{:?}", evidence(&doc, "configs"));
        assert_eq!(verdict(&doc, "configs"), "INFO");
    }

    #[test]
    fn read_write_configs_with_custom_values_are_fine() {
        let doc = broker(
            "{\"num.partitions\": 8, \"advertised.listeners\": \"PLAINTEXT://h:9092\", \
             \"log.retention.ms\": 3600000, \"max.connections.per.ip.overrides\": \"a:1\"}",
        );
        assert!(evidence(&doc, "configs").is_empty());
    }

    #[test]
    fn broker_range_rule_flags_small_and_non_integer_compaction_lag() {
        let key = "log.cleaner.max.compaction.lag.ms";
        for (value, flagged) in [
            ("86400000", false),
            ("86400001", false),
            ("\"90000000\"", false),
            ("86399999", true),
            ("1000", true),
            ("\"1000\"", true),
            ("0", true),
            ("-5", true),
        ] {
            let doc = broker(&format!("{{\"{key}\": {value}}}"));
            assert_eq!(
                code_list(&doc, "configs").contains(&"BROKER_CONFIG_OUT_OF_RANGE".to_string()),
                flagged,
                "{value}"
            );
        }
        let doc = broker(&format!("{{\"{key}\": \"1000\"}}"));
        assert_eq!(verdict(&doc, "configs"), "ACTION_REQUIRED");
        let item = finding(&doc, "configs", "BROKER_CONFIG_OUT_OF_RANGE");
        assert_eq!(item.get("severity").and_then(Json::as_str), Some("ACTION_REQUIRED"));
        assert_eq!(item.get("config_key").and_then(Json::as_str), Some(key));
        assert_eq!(item.get("observed"), Some(&Json::Int(1000)));
        assert_eq!(
            item.get("limit"),
            Some(&Json::Array(vec![Json::Int(86_400_000), Json::Null]))
        );
        assert_eq!(
            detail(&doc, "configs", "BROKER_CONFIG_OUT_OF_RANGE"),
            format!(
                "MSK Express accepts {key} only within [86400000, INT_MAX]. Your cluster sets it \
                 to '1000', which is outside that range, so this configuration can't be migrated \
                 as-is."
            )
        );
    }

    #[test]
    fn broker_range_rule_flags_values_that_are_not_integers() {
        let key = "log.cleaner.max.compaction.lag.ms";
        for (value, shown) in [("\"abc\"", "'abc'"), ("true", "True"), ("3.5", "3.5"), ("null", "None")] {
            let doc = broker(&format!("{{\"{key}\": {value}}}"));
            assert_eq!(verdict(&doc, "configs"), "ACTION_REQUIRED", "{value}");
            assert_eq!(
                detail(&doc, "configs", "BROKER_CONFIG_OUT_OF_RANGE"),
                format!(
                    "MSK Express expects {key} to be an integer within [86400000, INT_MAX]. Your \
                     cluster sets it to {shown}, which is not a valid integer, so this \
                     configuration can't be migrated as-is."
                ),
                "{value}"
            );
        }
        let doc = broker(&format!("{{\"{key}\": \"abc\"}}"));
        assert_eq!(
            finding(&doc, "configs", "BROKER_CONFIG_OUT_OF_RANGE").get("observed"),
            Some(&Json::string("abc"))
        );
    }

    #[test]
    fn forced_broker_values_are_advisories_unless_they_already_match() {
        let doc = broker("{\"default.replication.factor\": 2}");
        assert_eq!(verdict(&doc, "configs"), "ADVISORY");
        let item = finding(&doc, "configs", "BROKER_CONFIG_FORCED_VALUE");
        assert_eq!(item.get("observed"), Some(&Json::Int(2)));
        assert_eq!(item.get("enforced"), Some(&Json::Int(3)));
        assert_eq!(
            detail(&doc, "configs", "BROKER_CONFIG_FORCED_VALUE"),
            "MSK Express manages default.replication.factor for you and sets it to 3 to keep your \
             cluster highly available and durable; your current value of 2 won't apply. We \
             recommend confirming your workload behaves as expected with \
             default.replication.factor=3 in a test environment to ensure a smooth migration."
        );

        // Equal to the forced value (after normalization): nothing to report.
        for configs in [
            "{\"default.replication.factor\": 3}",
            "{\"default.replication.factor\": \"3\"}",
            "{\"min.insync.replicas\": 2}",
            "{\"transaction.state.log.min.isr\": \"2\"}",
            // Equal to the Apache default (1): filtered before the forced rule.
            "{\"min.insync.replicas\": 1}",
            "{\"unclean.leader.election.enable\": false}",
        ] {
            assert!(evidence(&broker(configs), "configs").is_empty(), "{configs}");
        }

        let doc = broker("{\"unclean.leader.election.enable\": true, \"min.insync.replicas\": 3}");
        assert_eq!(
            code_list(&doc, "configs"),
            ["BROKER_CONFIG_FORCED_VALUE", "BROKER_CONFIG_FORCED_VALUE"]
        );
        let text = detail(&doc, "configs", "BROKER_CONFIG_FORCED_VALUE");
        assert!(text.contains("sets it to 'false'"), "{text}");
        assert!(text.contains("your current value of True won't apply"), "{text}");
        assert_eq!(
            finding(&doc, "configs", "BROKER_CONFIG_FORCED_VALUE").get("enforced"),
            Some(&Json::string("false"))
        );
    }

    #[test]
    fn read_only_and_unexposed_broker_configs_are_advisories() {
        let doc = broker("{\"broker.id\": 7, \"num.io.threads\": 16, \"num.network.threads\": 3}");
        assert_eq!(
            code_list(&doc, "configs"),
            ["BROKER_CONFIG_READ_ONLY", "BROKER_CONFIG_READ_ONLY"],
            "num.network.threads=3 is the default"
        );
        assert_eq!(
            detail(&doc, "configs", "BROKER_CONFIG_READ_ONLY"),
            "MSK Express manages broker.id for you so you don't have to tune it (for \
             thread-related settings it's sized automatically from the broker instance type); \
             your current value won't apply. No action needed."
        );

        let doc = broker("{\"log.dirs\": \"/data\"}");
        assert_eq!(code_list(&doc, "configs"), ["BROKER_CONFIG_NOT_EXPOSED"]);
        assert_eq!(
            detail(&doc, "configs", "BROKER_CONFIG_NOT_EXPOSED"),
            "log.dirs isn't a configurable property on MSK Express \u{2014} Express manages it \
             internally, and the behavior may differ from your current value of '/data'. We \
             recommend validating in a test environment to ensure a smooth migration."
        );
        assert_eq!(verdict(&doc, "configs"), "ADVISORY");
        assert_eq!(
            finding(&doc, "configs", "BROKER_CONFIG_NOT_EXPOSED").get("observed"),
            Some(&Json::string("/data"))
        );
    }

    #[test]
    fn broker_config_dump_shapes() {
        for configs in ["null", "{}", "[]", "0", "\"\""] {
            assert!(evidence(&broker(configs), "configs").is_empty(), "{configs}");
        }
        let doc = assessed(&[("broker_configs", "-")]);
        assert!(evidence(&doc, "configs").is_empty());
        assert_eq!(error_of(&[("broker_configs", "5")]), "broker_configs must be an object");
        assert_eq!(error_of(&[("broker_configs", "[1]")]), "broker_configs must be an object");
        // Evidence follows the dump's key order.
        let doc = broker("{\"log.dirs\": \"a\", \"broker.id\": 1, \"listeners\": \"x\"}");
        let keys: Vec<String> = evidence(&doc, "configs")
            .iter()
            .map(|item| item.get("config_key").and_then(Json::as_str).expect("key").to_string())
            .collect();
        assert_eq!(keys, ["log.dirs", "broker.id", "listeners"]);
    }

    fn topics(text: &str) -> Json {
        assessed(&[("topics", text)])
    }

    #[test]
    fn topics_with_other_replication_factors_are_noted() {
        let doc = topics("[{\"name\": \"orders\", \"replication_factor\": 2}]");
        assert_eq!(code_list(&doc, "configs"), ["TOPIC_RF_NOT_3"]);
        let item = finding(&doc, "configs", "TOPIC_RF_NOT_3");
        assert_eq!(item.get("topic"), Some(&Json::string("orders")));
        assert_eq!(item.get("observed"), Some(&Json::Int(2)));
        assert_eq!(item.get("enforced"), Some(&Json::Int(3)));
        assert_eq!(
            detail(&doc, "configs", "TOPIC_RF_NOT_3"),
            "Topic 'orders' uses replication factor 2. MSK Express creates every topic with \
             replication factor 3 for durability, so on MSK Express this topic will use 3. No \
             action needed."
        );
        for factor in ["3", "\"3\""] {
            let doc = topics(&format!("[{{\"name\": \"t\", \"replication_factor\": {factor}}}]"));
            assert!(evidence(&doc, "configs").is_empty(), "rf {factor}");
        }
    }

    #[test]
    fn topic_range_rule() {
        let make = |value: &str| {
            topics(&format!(
                "[{{\"name\": \"t\", \"replication_factor\": 3, \"configs\": \
                 {{\"max.compaction.lag.ms\": {value}}}}}]"
            ))
        };
        let doc = make("\"5\"");
        assert_eq!(verdict(&doc, "configs"), "ACTION_REQUIRED");
        let item = finding(&doc, "configs", "TOPIC_CONFIG_OUT_OF_RANGE");
        assert_eq!(item.get("topic"), Some(&Json::string("t")));
        assert_eq!(item.get("observed"), Some(&Json::Int(5)));
        assert_eq!(
            detail(&doc, "configs", "TOPIC_CONFIG_OUT_OF_RANGE"),
            "MSK Express accepts max.compaction.lag.ms only within [86400000, INT_MAX]. Topic 't' \
             sets it to '5', which is outside that range, so this configuration can't be \
             migrated as-is."
        );
        assert!(evidence(&make("86400000"), "configs").is_empty());
        assert!(evidence(&make("9223372036854775807"), "configs").is_empty(), "Apache default");
        assert_eq!(verdict(&make("\"soon\""), "configs"), "ACTION_REQUIRED");
    }

    #[test]
    fn topic_forced_and_unexposed_configs() {
        let make = |configs: &str| {
            topics(&format!(
                "[{{\"name\": \"t\", \"replication_factor\": 3, \"configs\": {configs}}}]"
            ))
        };
        let doc = make("{\"min.insync.replicas\": \"3\", \"segment.bytes\": 1024, \"retention.ms\": 1000}");
        assert_eq!(
            code_list(&doc, "configs"),
            ["TOPIC_CONFIG_FORCED_VALUE", "TOPIC_CONFIG_NOT_EXPOSED"]
        );
        assert_eq!(
            detail(&doc, "configs", "TOPIC_CONFIG_FORCED_VALUE"),
            "Topic 't' sets min.insync.replicas='3'. MSK Express fixes this at 2 for every topic \
             to keep your data durable, so your current value won't apply. We recommend \
             confirming this topic behaves as expected with min.insync.replicas=2 in a test \
             environment to ensure a smooth migration."
        );
        assert_eq!(
            detail(&doc, "configs", "TOPIC_CONFIG_NOT_EXPOSED"),
            "Topic 't' sets segment.bytes=1024, which isn't a configurable topic property on MSK \
             Express \u{2014} Express uses the broker default instead. We recommend validating \
             that the default works for this topic in a test environment to ensure a smooth \
             migration."
        );
        let item = finding(&doc, "configs", "TOPIC_CONFIG_FORCED_VALUE");
        assert_eq!(item.get("enforced"), Some(&Json::Int(2)));
        assert_eq!(item.get("observed"), Some(&Json::string("3")));

        for configs in [
            "{\"min.insync.replicas\": 1}",
            "{\"min.insync.replicas\": 2}",
            "{\"unclean.leader.election.enable\": \"false\"}",
            "{\"retention.ms\": 1000, \"cleanup.policy\": \"compact\"}",
            "null",
            "{}",
        ] {
            assert!(evidence(&make(configs), "configs").is_empty(), "{configs}");
        }
        let doc = make("{\"unclean.leader.election.enable\": \"true\"}");
        assert_eq!(code_list(&doc, "configs"), ["TOPIC_CONFIG_FORCED_VALUE"]);
        assert_eq!(
            finding(&doc, "configs", "TOPIC_CONFIG_FORCED_VALUE").get("enforced"),
            Some(&Json::string("false"))
        );
    }

    #[test]
    fn malformed_topics_are_errors() {
        assert_eq!(
            error_of(&[("topics", "[{\"replication_factor\": 3}]")]),
            "topics[0] is missing required field 'name'"
        );
        assert_eq!(
            error_of(&[("topics", "[{\"name\": \"t\"}]")]),
            "topics[0] is missing required field 'replication_factor'"
        );
        assert_eq!(
            error_of(&[("topics", "[{\"name\": \"t\", \"replication_factor\": \"x\"}]")]),
            "topics[0].replication_factor: invalid literal for int() with base 10: 'x'"
        );
        assert_eq!(error_of(&[("topics", "[5]")]), "topics[0] must be an object");
        assert_eq!(
            error_of(&[("topics", "[{\"name\": \"t\", \"replication_factor\": 3, \"configs\": 5}]")]),
            "topics[0].configs must be an object"
        );
    }

    // --- pillar 4: auth ----------------------------------------------------

    fn security(encryption: &str, authentication: &str) -> Json {
        assessed(&[(
            "security",
            &format!(
                "{{\"encryption_in_transit\": {encryption}, \"authentication\": {authentication}}}"
            ),
        )])
    }

    #[test]
    fn supported_authentication_over_tls_is_info() {
        for auth in ["UNAUTHENTICATED", "TLS", "SASL_SCRAM", "SASL_IAM"] {
            let doc = security("\"TLS\"", &format!("\"{auth}\""));
            assert!(evidence(&doc, "auth").is_empty(), "{auth}");
            assert_eq!(verdict(&doc, "auth"), "INFO");
        }
        // Plaintext without authentication is allowed.
        for encryption in ["\"PLAINTEXT\"", "\"TLS_PLAINTEXT\""] {
            let doc = security(encryption, "\"UNAUTHENTICATED\"");
            assert!(evidence(&doc, "auth").is_empty(), "{encryption}");
        }
    }

    #[test]
    fn unsupported_mechanisms_require_action() {
        let doc = security("\"TLS\"", "\"SASL_OAUTHBEARER\"");
        assert_eq!(code_list(&doc, "auth"), ["AUTH_OAUTHBEARER_NOT_SUPPORTED"]);
        assert_eq!(verdict(&doc, "auth"), "ACTION_REQUIRED");
        assert_eq!(
            finding(&doc, "auth", "AUTH_OAUTHBEARER_NOT_SUPPORTED").get("observed"),
            Some(&Json::string("SASL_OAUTHBEARER"))
        );
        assert!(detail(&doc, "auth", "AUTH_OAUTHBEARER_NOT_SUPPORTED")
            .starts_with("Your cluster uses SASL/OAUTHBEARER. MSK Express accepts OAUTHBEARER only"));

        let doc = security("\"TLS\"", "\"OTHER\"");
        assert_eq!(code_list(&doc, "auth"), ["AUTH_MECHANISM_NOT_SUPPORTED"]);
        assert_eq!(verdict(&doc, "auth"), "ACTION_REQUIRED");
        // OTHER with plaintext is subsumed: no extra encryption finding.
        let doc = security("\"PLAINTEXT\"", "\"OTHER\"");
        assert_eq!(code_list(&doc, "auth"), ["AUTH_MECHANISM_NOT_SUPPORTED"]);
    }

    #[test]
    fn unknown_authentication_or_encryption_is_an_advisory() {
        let doc = security("\"TLS\"", "\"UNKNOWN\"");
        assert_eq!(code_list(&doc, "auth"), ["AUTH_UNKNOWN"]);
        assert_eq!(verdict(&doc, "auth"), "ADVISORY");

        let doc = security("\"UNKNOWN\"", "\"SASL_SCRAM\"");
        assert_eq!(code_list(&doc, "auth"), ["ENCRYPTION_UNKNOWN"]);
        assert_eq!(verdict(&doc, "auth"), "ADVISORY");

        // A missing, null or empty security block reads as UNKNOWN twice.
        for overrides in [
            vec![("security", "-")],
            vec![("security", "null")],
            vec![("security", "{}")],
            vec![("security", "{\"encryption_in_transit\": null, \"authentication\": null}")],
        ] {
            let doc = assessed(&overrides);
            assert_eq!(
                code_list(&doc, "auth"),
                ["AUTH_UNKNOWN", "ENCRYPTION_UNKNOWN"],
                "{overrides:?}"
            );
            assert_eq!(verdict(&doc, "auth"), "ADVISORY");
        }
    }

    #[test]
    fn authenticated_clients_must_use_tls() {
        for (encryption, auth) in [
            ("PLAINTEXT", "SASL_SCRAM"),
            ("PLAINTEXT", "SASL_IAM"),
            ("PLAINTEXT", "TLS"),
            ("TLS_PLAINTEXT", "SASL_SCRAM"),
        ] {
            let doc = security(&format!("\"{encryption}\""), &format!("\"{auth}\""));
            assert_eq!(code_list(&doc, "auth"), ["ENCRYPTION_NOT_TLS"], "{encryption} {auth}");
            assert_eq!(verdict(&doc, "auth"), "ACTION_REQUIRED");
            assert_eq!(
                finding(&doc, "auth", "ENCRYPTION_NOT_TLS").get("observed"),
                Some(&Json::string(encryption))
            );
        }
        let doc = security("\"PLAINTEXT\"", "\"SASL_SCRAM\"");
        assert_eq!(
            detail(&doc, "auth", "ENCRYPTION_NOT_TLS"),
            "Your cluster uses 'PLAINTEXT' for encryption in transit, but MSK Express requires \
             TLS for authenticated clients (TLS, SASL/SCRAM, IAM). Update your clients to \
             connect over TLS to ensure a smooth migration."
        );
        // OAUTHBEARER over plaintext only reports the mechanism.
        let doc = security("\"PLAINTEXT\"", "\"SASL_OAUTHBEARER\"");
        assert_eq!(code_list(&doc, "auth"), ["AUTH_OAUTHBEARER_NOT_SUPPORTED"]);
    }

    #[test]
    fn security_enums_are_closed() {
        assert_eq!(
            error_of(&[("security", "{\"encryption_in_transit\": \"SSL\"}")]),
            "security.encryption_in_transit='SSL' is not in the allowed enum ['PLAINTEXT', 'TLS', \
             'TLS_PLAINTEXT', 'UNKNOWN']"
        );
        assert_eq!(
            error_of(&[("security", "{\"authentication\": \"kerberos\"}")]),
            "security.authentication='kerberos' is not in the allowed enum ['OTHER', 'SASL_IAM', \
             'SASL_OAUTHBEARER', 'SASL_SCRAM', 'TLS', 'UNAUTHENTICATED', 'UNKNOWN']"
        );
        assert_eq!(
            error_of(&[("security", "{\"authentication\": \"\"}")]),
            "security.authentication='' is not in the allowed enum ['OTHER', 'SASL_IAM', \
             'SASL_OAUTHBEARER', 'SASL_SCRAM', 'TLS', 'UNAUTHENTICATED', 'UNKNOWN']"
        );
        assert_eq!(
            error_of(&[("security", "{\"authentication\": 5}")]),
            "security.authentication=5 is not in the allowed enum ['OTHER', 'SASL_IAM', \
             'SASL_OAUTHBEARER', 'SASL_SCRAM', 'TLS', 'UNAUTHENTICATED', 'UNKNOWN']"
        );
        assert_eq!(error_of(&[("security", "[1]")]), "security must be an object");
    }

    // --- pillar 5: quotas --------------------------------------------------

    fn metrics(fields: &str) -> Json {
        assessed(&[("metrics", &format!("{{{fields}}}"))])
    }

    #[test]
    fn missing_metrics_is_an_advisory() {
        for overrides in [vec![], vec![("metrics", "null")]] {
            let doc = assessed(&overrides);
            assert_eq!(code_list(&doc, "quotas"), ["METRICS_MISSING"]);
            assert_eq!(verdict(&doc, "quotas"), "ADVISORY");
        }
        let doc = assessed(&[]);
        assert!(detail(&doc, "quotas", "METRICS_MISSING")
            .starts_with("We don't have utilization metrics for your cluster, so we can't check"));
    }

    #[test]
    fn per_broker_limits_are_exclusive_upper_bounds() {
        for (field, code, limit) in [
            ("peak_bytes_in_per_broker_mbps", "INGRESS_OVER_MAX_BROKER", 750),
            ("peak_bytes_out_per_broker_mbps", "EGRESS_OVER_MAX_BROKER", 1875),
            ("peak_partitions_per_broker", "PARTITIONS_OVER_MAX_BROKER", 32_000),
        ] {
            let at_limit = metrics(&format!("\"{field}\": {limit}"));
            assert!(
                !code_list(&at_limit, "quotas").contains(&code.to_string()),
                "{field} at the limit"
            );
            let over = metrics(&format!("\"{field}\": {}", limit + 1));
            assert_eq!(verdict(&over, "quotas"), "ADVISORY", "{field}");
            let item = finding(&over, "quotas", code);
            assert_eq!(item.get("observed"), Some(&Json::Int(limit + 1)));
            assert_eq!(item.get("limit"), Some(&Json::Int(limit)));
            assert_eq!(item.get("severity").and_then(Json::as_str), Some("ADVISORY"));
        }
        let doc = metrics("\"peak_bytes_in_per_broker_mbps\": 800.5");
        assert_eq!(
            detail(&doc, "quotas", "INGRESS_OVER_MAX_BROKER"),
            "Your peak ingress of 800.5 MBps per broker is more than any single MSK Express \
             broker can absorb (the maximum is 750 MBps, on m7g.16xlarge). Spread the load \
             across more brokers so each carries less ingress. Note that the sizing workbook \
             accounts for this already and can help you select the right MSK Express broker \
             instance type."
        );
        assert_eq!(
            finding(&doc, "quotas", "INGRESS_OVER_MAX_BROKER").get("observed"),
            Some(&Json::Float(800.5))
        );
        let doc = metrics("\"peak_bytes_out_per_broker_mbps\": 2000, \"peak_partitions_per_broker\": 40000");
        assert_eq!(
            code_list(&doc, "quotas"),
            ["EGRESS_OVER_MAX_BROKER", "PARTITIONS_OVER_MAX_BROKER"]
        );
        // null metrics are skipped.
        assert!(evidence(&metrics("\"peak_bytes_in_per_broker_mbps\": null"), "quotas").is_empty());
    }

    #[test]
    fn iam_connection_cap_applies_to_iam_style_authentication_only() {
        let make = |auth: &str, connections: i64| {
            assessed(&[
                (
                    "security",
                    &format!("{{\"encryption_in_transit\": \"TLS\", \"authentication\": \"{auth}\"}}"),
                ),
                ("metrics", &format!("{{\"peak_connections_per_broker\": {connections}}}")),
            ])
        };
        for auth in ["SASL_IAM", "SASL_OAUTHBEARER"] {
            assert!(evidence(&make(auth, 3000), "quotas").is_empty(), "{auth} at the cap");
            let doc = make(auth, 3001);
            assert_eq!(code_list(&doc, "quotas"), ["CONNECTIONS_OVER_IAM_LIMIT"], "{auth}");
            assert_eq!(
                finding(&doc, "quotas", "CONNECTIONS_OVER_IAM_LIMIT").get("limit"),
                Some(&Json::Int(3000))
            );
        }
        for auth in ["SASL_SCRAM", "TLS", "UNAUTHENTICATED"] {
            assert!(evidence(&make(auth, 50_000), "quotas").is_empty(), "{auth}");
        }
        // No security block: not IAM.
        let doc = assessed(&[("metrics", "{\"peak_connections_per_broker\": 50000}")]);
        assert!(evidence(&doc, "quotas").is_empty());
        assert_eq!(
            detail(&make("SASL_IAM", 3001), "quotas", "CONNECTIONS_OVER_IAM_LIMIT"),
            "Your peak of 3001 connections per broker is above the MSK Express IAM limit of 3000 \
             per broker. You can spread the connections across more brokers, or use a non-IAM \
             authentication mechanism, which MSK Express doesn't cap (monitor CPU and memory \
             instead)."
        );
    }

    #[test]
    fn per_partition_throughput_uses_peak_ingress_times_brokers_over_partitions() {
        let make = |ingress: &str, brokers: i64, partitions: i64| {
            assessed(&[
                ("topology", &format!("{{\"num_brokers\": {brokers}, \"num_azs\": 3}}")),
                (
                    "topics",
                    &format!(
                        "[{{\"name\": \"a\", \"replication_factor\": 3, \"num_partitions\": {}}}, \
                         {{\"name\": \"b\", \"replication_factor\": 3, \"num_partitions\": \"{}\"}}]",
                        partitions - 1,
                        1
                    ),
                ),
                ("metrics", &format!("{{\"peak_bytes_in_per_broker_mbps\": {ingress}}}")),
            ])
        };
        // 100 * 3 / 10 = 30.0 MB/s > 15.
        let doc = make("100", 3, 10);
        assert_eq!(code_list(&doc, "quotas"), ["PARTITION_THROUGHPUT_OVER_LIMIT"]);
        let item = finding(&doc, "quotas", "PARTITION_THROUGHPUT_OVER_LIMIT");
        assert_eq!(item.get("observed"), Some(&Json::Float(30.0)));
        assert_eq!(item.get("limit"), Some(&Json::Int(15)));
        assert_eq!(
            detail(&doc, "quotas", "PARTITION_THROUGHPUT_OVER_LIMIT"),
            "Your average per-partition throughput of 30.0 MB/s is above the MSK Express limit of \
             15 MB/s, where Express begins to throttle. Individual hot partitions can throttle \
             your clients. We recommend spreading the busiest topics across more partitions \
             before migrating."
        );
        // Exactly the limit: 50 * 3 / 10 = 15.0 is not above it.
        assert!(evidence(&make("50", 3, 10), "quotas").is_empty());
        // Rounded to one decimal: 7.7 * 3 / 1 = 23.1.
        let doc = make("7.7", 3, 1);
        let item = finding(&doc, "quotas", "PARTITION_THROUGHPUT_OVER_LIMIT");
        assert_eq!(item.get("observed"), Some(&Json::Float(23.1)));
        assert!(detail(&doc, "quotas", "PARTITION_THROUGHPUT_OVER_LIMIT").contains("throughput of 23.1 MB/s"));
    }

    #[test]
    fn per_partition_throughput_needs_partitions_and_ingress() {
        let doc = assessed(&[("metrics", "{\"peak_bytes_in_per_broker_mbps\": 700}")]);
        assert!(evidence(&doc, "quotas").is_empty(), "no topics, no partitions");
        let doc = assessed(&[
            (
                "topics",
                "[{\"name\": \"a\", \"replication_factor\": 3, \"num_partitions\": 1}]",
            ),
            ("metrics", "{}"),
        ]);
        assert!(evidence(&doc, "quotas").is_empty(), "no ingress metric");
    }

    #[test]
    fn malformed_metrics_and_partition_counts_are_errors() {
        assert_eq!(
            error_of(&[("metrics", "{\"peak_bytes_in_per_broker_mbps\": \"fast\"}")]),
            "metrics.peak_bytes_in_per_broker_mbps must be a number or null, not string"
        );
        assert_eq!(error_of(&[("metrics", "[1]")]), "metrics must be an object");
        assert_eq!(
            error_of(&[
                ("topics", "[{\"name\": \"a\", \"replication_factor\": 3}]"),
                ("metrics", "{}"),
            ]),
            "topics[0] is missing required field 'num_partitions'"
        );
        assert_eq!(
            error_of(&[("topology", "{\"num_brokers\": null}")]),
            "topology.num_brokers: int() argument must be a string or a number, not null"
        );
    }

    // --- input validation --------------------------------------------------

    #[test]
    fn required_contract_fields() {
        let incomplete = parse_json("{\"cluster_name\": \"x\"}").expect("json");
        assert_eq!(
            validate_input(&incomplete),
            Err("discovery contract missing top-level fields: ['kafka', 'topology', 'topics']".to_string())
        );
        assert_eq!(
            error_of(&[("kafka", "{}")]),
            "kafka block missing required fields: ['version']"
        );
        assert_eq!(
            error_of(&[("topology", "{}")]),
            "topology block missing required fields: ['num_brokers']"
        );
        assert_eq!(error_of(&[("topics", "{}")]), "topics must be a list");
        assert_eq!(error_of(&[("topics", "null")]), "topics must be a list");
        assert_eq!(
            validate_input(&Json::Array(vec![])),
            Err("discovery contract must be an object".to_string())
        );
        assert_eq!(error_of(&[("kafka", "\"3.6\"")]), "kafka must be an object");
        assert_eq!(error_of(&[("topology", "[]")]), "topology must be an object");
        assert!(validate_input(&config(&[])).is_ok());
    }

    #[test]
    fn target_block_is_validated_but_unused() {
        for target in [
            "{}",
            "{\"rack_affined_consumers\": true, \"pricing_discount_pct\": 15}",
            "{\"rack_affined_consumers\": null, \"pricing_discount_pct\": null}",
            "{\"pricing_discount_pct\": 0}",
            "{\"pricing_discount_pct\": 99.5}",
            "null",
        ] {
            assert!(assess(&config(&[("target", target)]), AT).is_ok(), "{target}");
        }
        assert_eq!(
            error_of(&[("target", "{\"rack_affined_consumers\": \"yes\"}")]),
            "target.rack_affined_consumers must be true, false, or null; got 'yes'"
        );
        assert_eq!(
            error_of(&[("target", "{\"rack_affined_consumers\": 1}")]),
            "target.rack_affined_consumers must be true, false, or null; got 1"
        );
        assert_eq!(
            error_of(&[("target", "{\"pricing_discount_pct\": \"5\"}")]),
            "target.pricing_discount_pct must be a number or null; got '5'"
        );
        assert_eq!(
            error_of(&[("target", "{\"pricing_discount_pct\": true}")]),
            "target.pricing_discount_pct must be a number or null; got True"
        );
        assert_eq!(
            error_of(&[("target", "{\"pricing_discount_pct\": 100}")]),
            "target.pricing_discount_pct must be >= 0 and < 100; got 100"
        );
        assert_eq!(
            error_of(&[("target", "{\"pricing_discount_pct\": -0.5}")]),
            "target.pricing_discount_pct must be >= 0 and < 100; got -0.5"
        );
        assert_eq!(error_of(&[("target", "[]")]), "target must be an object");
    }

    // --- helpers -----------------------------------------------------------

    #[test]
    fn severity_ordering_and_names() {
        assert!(Severity::Info < Severity::Advisory);
        assert!(Severity::Advisory < Severity::ActionRequired);
        assert_eq!(Severity::Info.worst(Severity::ActionRequired), Severity::ActionRequired);
        assert_eq!(Severity::Advisory.worst(Severity::Info), Severity::Advisory);
        assert_eq!(Severity::Info.as_str(), "INFO");
        assert_eq!(Severity::Advisory.as_str(), "ADVISORY");
        assert_eq!(Severity::ActionRequired.as_str(), "ACTION_REQUIRED");
    }

    #[test]
    fn output_names_must_stay_inside_the_output_directory() {
        assert_eq!(
            output_file_name(&Json::string("orders-prod")),
            Ok("compatibility.orders-prod.json".to_string())
        );
        assert_eq!(
            output_file_name(&Json::string(".hidden name")),
            Ok("compatibility..hidden name.json".to_string())
        );
        assert_eq!(output_file_name(&Json::Int(42)), Ok("compatibility.42.json".to_string()));
        for bad in ["", ".", "..", "a/b", "../x", "a\\b", "x\ny", "nul\u{0}"] {
            let Err(message) = output_file_name(&Json::string(bad)) else {
                panic!("{bad:?} must be refused");
            };
            assert!(message.contains("cannot be used in an output file name"), "{message}");
        }
    }

    #[test]
    fn utc_timestamps_match_strftime() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc_timestamp(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(format_utc_timestamp(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_utc_timestamp(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(format_utc_timestamp(4_102_444_800), "2100-01-01T00:00:00Z");
        assert_eq!(format_utc_timestamp(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_utc_date(86_399), "1970-01-01");
        assert_eq!(format_utc_date(86_400), "1970-01-02");
        assert_eq!(format_utc_date(951_782_400), "2000-02-29");
    }
}
