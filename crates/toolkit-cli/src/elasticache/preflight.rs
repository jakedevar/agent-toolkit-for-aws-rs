//! `toolkit elasticache preflight` (port of `migration_preflight.py`): is a
//! Redis/Valkey source ready to migrate to ElastiCache?
//!
//! The Python script connected to the source with the `valkey` client. This
//! build has no network client, so the checks run over a **snapshot**: a JSON
//! file holding what the script read from the server. A live connection is
//! reported exactly like a connection failure of the script
//! (see `docs/migration/elasticache.md`).
//!
//! Snapshot shape (every key optional unless noted):
//!
//! ```json
//! {
//!   "info": {"redis_version": "7.2.4", "used_memory": 1048576, "db0": {"keys": 10}},
//!   "modules": [["name", "search", "ver", 20000]],
//!   "commandstats": {"cmdstat_get": {"calls": 5, "usec": 9}},
//!   "config": {"maxmemory-policy": "allkeys-lru"},
//!   "sample_keys": [{"key": "k", "type": "hash", "memory_usage": 2097152}],
//!   "memory_usage_supported": true,
//!   "connection_error": "text", "auth_error": "text", "info_error": "text"
//! }
//! ```
//!
//! `info` is required unless one of the three `*_error` keys is given. A
//! `null` for `modules`, `commandstats` or `config` means the server refused
//! that command. The key names of `sample_keys` are never printed.

use toolkit_core::{Json, ToolkitError};

use crate::aurora::args::{parse_py_float, parse_py_int};
use crate::aurora::pyfmt::{round_to, truthy, Num};
use crate::manifests::py_repr;
use crate::safe_text::redacted_value;

use super::text::{group_int, js, kv, py_str, set_member, show};

/// Commands that are restricted or modified on ElastiCache.
pub const ELASTICACHE_RESTRICTED_COMMANDS: &[&str] = &[
    "BGREWRITEAOF",
    "BGSAVE",
    "CONFIG",
    "DEBUG",
    "MIGRATE",
    "REPLICAOF",
    "SAVE",
    "SHUTDOWN",
    "SLAVEOF",
    "SYNC",
];

/// Known modules: (module name, product name, ElastiCache availability, note).
const MODULE_COMPATIBILITY: &[(&str, &str, bool, &str)] = &[
    (
        "search",
        "RediSearch",
        false,
        "Not available on ElastiCache. Valkey 8.2 provides native vector search (FT.* commands) on node-based clusters.",
    ),
    (
        "ReJSON",
        "RedisJSON",
        false,
        "The ReJSON module cannot be loaded on ElastiCache. However, ElastiCache provides native JSON support (JSON.* commands) in Valkey 7.2+ and Redis OSS 6.2.6+. Verify command compatibility before migration.",
    ),
    (
        "timeseries",
        "RedisTimeSeries",
        false,
        "Not available on ElastiCache. Consider CloudWatch or Timestream.",
    ),
    (
        "bf",
        "RedisBloom",
        false,
        "No (module) / Yes (native BF.* commands in Valkey 8.1+).",
    ),
    (
        "graph",
        "RedisGraph",
        false,
        "Deprecated. Consider Neptune or OpenSearch.",
    ),
    (
        "ai",
        "RedisAI",
        false,
        "Not available on ElastiCache. Use SageMaker for ML inference.",
    ),
];

const EXTENDED_SUPPORT_DETAIL: &str = "Year 1-2 premium: 80% surcharge. Year 3 premium: 160% surcharge. Run `toolkit elasticache price --extended-support` to see costs.";

/// One check outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Name of the check.
    pub check: String,
    /// `PASS`, `WARN`, `FAIL`, `INFO` or `ERROR`.
    pub status: &'static str,
    /// What was found.
    pub detail: String,
}

impl Finding {
    fn new(check: impl Into<String>, status: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            status,
            detail: detail.into(),
        }
    }

    fn to_json(&self) -> Json {
        Json::Object(vec![
            kv("check", js(&self.check)),
            kv("status", js(self.status)),
            kv("detail", js(&self.detail)),
        ])
    }
}

/// The result of [`run_preflight`].
#[derive(Debug, Clone, PartialEq)]
pub struct Preflight {
    /// Set when the source could not be reached.
    pub error: Option<String>,
    /// What was learned about the source, in insertion order.
    pub info: Vec<(String, Json)>,
    /// The checks.
    pub findings: Vec<Finding>,
}

impl Preflight {
    /// The object `--json` prints (`error` first when there is one).
    #[must_use]
    pub fn to_json(&self) -> Json {
        let mut members = Vec::new();
        if let Some(error) = &self.error {
            members.push(kv("error", js(error)));
        }
        members.push(kv("info", Json::Object(self.info.clone())));
        members.push(kv(
            "findings",
            Json::Array(self.findings.iter().map(Finding::to_json).collect()),
        ));
        Json::Object(members)
    }

    /// Number of findings with `status`.
    #[must_use]
    pub fn count(&self, status: &str) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.status == status)
            .count()
    }

    /// Whether the command exits with status 1.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.error.is_some() || self.count("FAIL") > 0
    }
}

fn number(value: Option<&Json>, default: f64) -> f64 {
    match value {
        Some(Json::Int(n)) => *n as f64,
        Some(Json::Float(n)) => *n,
        Some(Json::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Json::String(text)) => parse_py_float(text).unwrap_or(default),
        _ => default,
    }
}

fn calls_of(entry: Option<&Json>) -> i64 {
    number(entry.and_then(|stat| stat.get("calls")), 0.0) as i64
}

fn version_tuple(version: &str) -> (i64, i64) {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        match (parse_py_int(parts[0]), parse_py_int(parts[1])) {
            (Some(major), Some(minor)) => (major, minor),
            _ => (0, 0),
        }
    } else {
        (parse_py_int(parts[0]).unwrap_or(0), 0)
    }
}

// recorded-shape: one MODULE LIST entry of the Redis/Valkey snapshot file, not an AWS
// response; every JSON type renders as the script printed it.
fn module_name(entry: &Json) -> String {
    match entry {
        Json::Array(items) if items.len() > 1 => py_str(&items[1]),
        Json::Object(_) => entry.get("name").map_or_else(|| py_repr(entry), py_str),
        other => py_str(other),
    }
}

fn engine_findings(version: &str) -> Vec<Finding> {
    let shown = show(version);
    let extended = Finding::new(
        "Extended Support",
        "FAIL",
        format!("Redis {shown} is enrolled in Extended Support. {EXTENDED_SUPPORT_DETAIL}"),
    );
    if version_tuple(version) >= (7, 0) {
        vec![Finding::new(
            "Engine version",
            "PASS",
            format!("Redis {shown} can migrate to Valkey 7.2 (direct in-place switch) or Redis OSS 7+ on ElastiCache"),
        )]
    } else if version_tuple(version) >= (6, 0) {
        vec![Finding::new(
            "Engine version",
            "WARN",
            format!("Redis {shown} standard support ends January 31, 2027. Extended Support charges begin February 1, 2027. Recommend upgrading to Valkey before then."),
        )]
    } else if version_tuple(version) >= (5, 0) {
        vec![
            Finding::new(
                "Engine version",
                "FAIL",
                format!("Redis {shown} standard support ended January 31, 2026. Extended Support charges are now active (since Feb 1, 2026). Upgrade to Valkey to eliminate surcharges."),
            ),
            extended,
        ]
    } else {
        vec![
            Finding::new(
                "Engine version",
                "FAIL",
                format!("Redis {shown} standard support ended January 31, 2026. Extended Support charges are now active. Upgrade to Valkey to eliminate surcharges."),
            ),
            extended,
        ]
    }
}

/// A preflight that stops at an error result.
fn failure(info: Vec<(String, Json)>, error: String, detail: &str) -> Preflight {
    Preflight {
        error: Some(error),
        info,
        findings: vec![Finding::new("Connectivity", "FAIL", detail)],
    }
}

fn text_member<'a>(snapshot: &'a Json, key: &str) -> Option<&'a str> {
    snapshot.get(key).and_then(Json::as_str)
}

/// Runs the checks over a snapshot (`None` is a live connection, which this
/// build cannot open).
///
/// # Errors
///
/// A snapshot without an `info` object.
pub fn run_preflight(
    host: &str,
    port: i64,
    snapshot: Option<&Json>,
) -> Result<Preflight, ToolkitError> {
    // The host is caller text: redacted where the data is built, never after
    // serialization, so the JSON keeps its schema (#4981).
    let mut info = vec![
        kv("host", js(&redacted_value(host))),
        kv("port", Json::Int(port)),
    ];
    let shown_host = show(host);

    let Some(snapshot) = snapshot else {
        let reason =
            "live Redis/Valkey connections are not available in this build; pass --snapshot FILE";
        return Ok(failure(
            info,
            format!("Cannot connect to {shown_host}:{port}: {reason}"),
            reason,
        ));
    };
    if let Some(message) = text_member(snapshot, "connection_error") {
        let shown = show(message);
        return Ok(failure(
            info,
            format!("Cannot connect to {shown_host}:{port}: {shown}"),
            &shown,
        ));
    }
    if let Some(message) = text_member(snapshot, "auth_error") {
        let shown = show(message);
        return Ok(failure(
            info,
            format!("Authentication failed: {shown}"),
            &format!("Auth failed: {shown}"),
        ));
    }

    let mut findings = vec![Finding::new(
        "Connectivity",
        "PASS",
        format!("Connected to {shown_host}:{port}"),
    )];
    if let Some(message) = text_member(snapshot, "info_error") {
        findings.push(Finding::new(
            "Server info",
            "ERROR",
            format!("Could not retrieve INFO: {}", show(message)),
        ));
        return Ok(Preflight {
            error: None,
            info,
            findings,
        });
    }
    let server_info = match snapshot.get("info") {
        // recorded-shape: the required INFO reply of the Redis/Valkey snapshot file, not an
        // AWS response; any shape but an object is refused.
        Some(object @ Json::Object(_)) => object,
        _ => {
            return Err(ToolkitError::runtime(
                "snapshot_invalid",
                "the snapshot must hold an \"info\" object",
            ));
        }
    };

    // Engine and version.
    let redis_version = server_info
        .get("redis_version")
        .map_or_else(|| "unknown".to_string(), py_str);
    set_member(&mut info, "redis_version", js(&redis_version));
    set_member(
        &mut info,
        "os",
        server_info
            .get("os")
            .cloned()
            .unwrap_or_else(|| js("unknown")),
    );
    let uptime = number(server_info.get("uptime_in_seconds"), 0.0);
    set_member(
        &mut info,
        "uptime_days",
        Json::Float(round_to(uptime / 86400.0, 1)),
    );
    findings.extend(engine_findings(&redis_version));

    // Cluster mode.
    let cluster_enabled = truthy(server_info.get("cluster_enabled"));
    set_member(&mut info, "cluster_mode", Json::Bool(cluster_enabled));
    findings.push(if cluster_enabled {
        Finding::new(
            "Cluster mode",
            "INFO",
            "Cluster mode enabled — target must also be cluster mode enabled",
        )
    } else {
        Finding::new(
            "Cluster mode",
            "INFO",
            "Standalone or replica mode — can target cluster mode disabled or enabled",
        )
    });

    // Connected replicas.
    let replicas = server_info
        .get("connected_slaves")
        .cloned()
        .unwrap_or(Json::Int(0));
    findings.push(Finding::new(
        "Replication",
        "INFO",
        format!("{} connected replica(s)", show(&py_str(&replicas))),
    ));
    set_member(&mut info, "connected_replicas", replicas);

    // Memory.
    let used_memory_mb = round_to(number(server_info.get("used_memory"), 0.0) / 1_048_576.0, 1);
    let used_memory_gb = round_to(used_memory_mb / 1024.0, 2);
    let peak_memory_mb = round_to(
        number(server_info.get("used_memory_peak"), 0.0) / 1_048_576.0,
        1,
    );
    set_member(&mut info, "used_memory_mb", Json::Float(used_memory_mb));
    set_member(&mut info, "used_memory_gb", Json::Float(used_memory_gb));
    set_member(&mut info, "peak_memory_mb", Json::Float(peak_memory_mb));
    findings.push(Finding::new(
        "Memory usage",
        "INFO",
        format!(
            "Current: {} MB ({} GB), Peak: {} MB",
            Num::Float(used_memory_mb).py_str(),
            Num::Float(used_memory_gb).py_str(),
            Num::Float(peak_memory_mb).py_str()
        ),
    ));

    key_count_findings(server_info, &mut info, &mut findings);

    // Persistence.
    let rdb_enabled = number(server_info.get("rdb_last_save_time"), 0.0) > 0.0;
    let aof_enabled = number(server_info.get("aof_enabled"), 0.0) == 1.0;
    set_member(&mut info, "rdb_enabled", Json::Bool(rdb_enabled));
    set_member(&mut info, "aof_enabled", Json::Bool(aof_enabled));
    let mut persistence: Vec<&str> = Vec::new();
    if rdb_enabled {
        persistence.push("RDB snapshots active");
    }
    if aof_enabled {
        persistence.push("AOF enabled");
    }
    if persistence.is_empty() {
        persistence.push("No persistence");
    }
    findings.push(Finding::new("Persistence", "INFO", persistence.join(", ")));

    module_findings(snapshot, &mut info, &mut findings);
    command_findings(snapshot, &mut info, &mut findings);
    large_key_findings(snapshot, &mut findings);
    eviction_findings(host, snapshot, &mut info, &mut findings);

    findings.push(Finding::new(
        "Data tiering advisory",
        "INFO",
        "If targeting r6gd node types (data tiering), note: online migration is not supported for r6gd clusters. Use backup/restore instead. Data tiering only supports volatile-lru, allkeys-lru, volatile-lfu, allkeys-lfu, and noeviction eviction policies.",
    ));
    findings.push(Finding::new(
        "Sizing recommendation",
        "INFO",
        format!(
            "Source dataset: {} GB. Use `toolkit elasticache price` to estimate serverless and node-based costs for your workload.",
            Num::Float(used_memory_gb).py_str()
        ),
    ));

    Ok(Preflight {
        error: None,
        info,
        findings,
    })
}

// recorded-shape: the `db*` keyspace sections of the INFO reply in the Redis/Valkey
// snapshot file, not an AWS response; a section of another type counts no keys.
fn key_count_findings(
    server_info: &Json,
    info: &mut Vec<(String, Json)>,
    findings: &mut Vec<Finding>,
) {
    let Json::Object(members) = server_info else {
        return;
    };
    let databases: Vec<&Json> = members
        .iter()
        .filter(|(key, _)| key.starts_with("db"))
        .map(|(_, value)| value)
        .collect();
    let mut total_keys: i64 = 0;
    for database in &databases {
        if !matches!(database, Json::Object(_)) {
            continue;
        }
        match database.get("keys") {
            None => {}
            Some(Json::Int(keys)) => total_keys = total_keys.saturating_add(*keys),
            Some(_) => {
                set_member(info, "total_keys", js("unknown"));
                return;
            }
        }
    }
    set_member(info, "total_keys", Json::Int(total_keys));
    set_member(
        info,
        "databases_in_use",
        Json::Int(i64::try_from(databases.len()).unwrap_or(i64::MAX)),
    );
    findings.push(Finding::new(
        "Key count",
        "INFO",
        format!(
            "{} keys across {} database(s)",
            group_int(total_keys),
            databases.len()
        ),
    ));
    if databases.len() > 1 {
        findings.push(Finding::new(
            "Multiple databases",
            "WARN",
            format!(
                "{} databases in use — ElastiCache cluster mode uses only db0. Plan key migration.",
                databases.len()
            ),
        ));
    }
}

fn module_findings(snapshot: &Json, info: &mut Vec<(String, Json)>, findings: &mut Vec<Finding>) {
    let modules = match snapshot.get("modules") {
        Some(Json::Null) => {
            set_member(info, "modules", Json::Array(Vec::new()));
            findings.push(Finding::new(
                "Modules",
                "INFO",
                "MODULE LIST not available (may be restricted or old version)",
            ));
            return;
        }
        // recorded-shape: the MODULE LIST reply of the Redis/Valkey snapshot file, not an
        // AWS response; `null` is the documented refused command.
        Some(Json::Array(items)) => items.iter().map(module_name).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    set_member(
        info,
        "modules",
        Json::Array(modules.iter().map(|name| js(name)).collect()),
    );
    if modules.is_empty() {
        findings.push(Finding::new("Modules", "PASS", "No modules loaded"));
        return;
    }
    for name in &modules {
        match MODULE_COMPATIBILITY
            .iter()
            .find(|(known, _, _, _)| *known == name.as_str())
        {
            Some((_, product, available, note)) => findings.push(Finding::new(
                format!("Module: {product}"),
                if *available { "INFO" } else { "FAIL" },
                *note,
            )),
            None => {
                let shown = show(name);
                findings.push(Finding::new(
                    format!("Module: {shown}"),
                    "WARN",
                    format!("Unknown module '{shown}' — verify compatibility with ElastiCache"),
                ));
            }
        }
    }
}

/// Restricted-command and Lua findings.
fn command_findings(snapshot: &Json, info: &mut Vec<(String, Json)>, findings: &mut Vec<Finding>) {
    let stats = match snapshot.get("commandstats") {
        Some(Json::Null) => {
            findings.push(Finding::new(
                "Restricted command usage",
                "INFO",
                "INFO COMMANDSTATS not available (may be restricted by ACL or unsupported on this engine version)",
            ));
            None
        }
        // recorded-shape: the INFO COMMANDSTATS reply of the Redis/Valkey snapshot file,
        // not an AWS response; `null` is the documented refused command.
        Some(object @ Json::Object(_)) => Some(object),
        _ => None,
    };
    if let Some(stats) = stats {
        for command in ELASTICACHE_RESTRICTED_COMMANDS {
            let key = format!("cmdstat_{}", command.to_lowercase().replace(' ', "|"));
            let calls = calls_of(stats.get(&key));
            if stats.get(&key).is_some() && calls > 0 {
                findings.push(Finding::new(
                    "Restricted command usage",
                    "WARN",
                    format!("Command {command} called {calls} times. This command is restricted on ElastiCache. Review usage before migration."),
                ));
            }
        }
    }
    let eval_calls: i64 = stats.map_or(0, |all| {
        calls_of(all.get("cmdstat_eval")) + calls_of(all.get("cmdstat_evalsha"))
    });
    let lua_detected = eval_calls > 0;
    if lua_detected {
        findings.push(Finding::new(
            "Lua scripts",
            "WARN",
            format!("Lua scripts detected ({eval_calls} eval calls). Test all Lua scripts against the target engine. Scripts using module commands or hardcoded key names may break."),
        ));
    }
    set_member(info, "lua_scripts_detected", Json::Bool(lua_detected));
}

fn large_key_findings(snapshot: &Json, findings: &mut Vec<Finding>) {
    // recorded-shape: the sampled keys of the Redis/Valkey snapshot file, not an AWS
    // response; a list that is absent or of another type samples nothing.
    let Some(Json::Array(samples)) = snapshot.get("sample_keys") else {
        return;
    };
    let supported = !matches!(
        snapshot.get("memory_usage_supported"),
        Some(Json::Bool(false))
    );
    let mut large: Vec<(String, f64)> = Vec::new();
    for sample in samples.iter().take(20) {
        if !supported {
            findings.push(Finding::new(
                "Large key sampling",
                "INFO",
                "MEMORY USAGE command not available, skipping large key check",
            ));
            break;
        }
        let usage = number(sample.get("memory_usage"), 0.0);
        if usage > 1_048_576.0 {
            let key_present = text_member(sample, "key").is_some_and(|key| !key.is_empty());
            let key_type = if key_present {
                text_member(sample, "type").unwrap_or("unknown")
            } else {
                "unknown"
            };
            large.push((show(key_type), round_to(usage / 1_048_576.0, 2)));
        }
    }
    for (key_type, size_mb) in large {
        findings.push(Finding::new(
            "Large key sampling",
            "WARN",
            format!(
                "Large key detected ({key_type}, {}MB). Large keys can cause replication timeouts and event loop blocking during migration.",
                Num::Float(size_mb).py_str()
            ),
        ));
    }
}

fn eviction_findings(
    host: &str,
    snapshot: &Json,
    info: &mut Vec<(String, Json)>,
    findings: &mut Vec<Finding>,
) {
    // CONFIG is restricted on ElastiCache endpoints, so it is not consulted.
    if host.ends_with(".cache.amazonaws.com") {
        set_member(info, "maxmemory_policy", js("unknown (ElastiCache source)"));
        findings.push(Finding::new(
            "Eviction policy",
            "INFO",
            "CONFIG command is restricted on ElastiCache. Check the eviction policy via: aws elasticache describe-cache-parameters --cache-parameter-group-name <param-group> --query \"Parameters[?ParameterName=='maxmemory-policy']\"",
        ));
        return;
    }
    // recorded-shape: the CONFIG GET reply of the Redis/Valkey snapshot file, not an
    // AWS response; `null` is the documented refused command.
    let Some(config @ Json::Object(_)) = snapshot.get("config") else {
        set_member(info, "maxmemory_policy", js("unknown"));
        return;
    };
    let policy = config
        .get("maxmemory-policy")
        .map_or_else(|| "unknown".to_string(), py_str);
    set_member(info, "maxmemory_policy", js(&policy));
    let shown = show(&policy);
    if policy == "noeviction" {
        findings.push(Finding::new(
            "Eviction policy",
            "WARN",
            format!("Policy is '{shown}' -- writes will fail when memory is full. Consider volatile-lru or allkeys-lru."),
        ));
    } else {
        findings.push(Finding::new(
            "Eviction policy",
            "INFO",
            format!("Policy: {shown}"),
        ));
    }
}

fn status_rank(status: &str) -> u8 {
    match status {
        "FAIL" => 0,
        "WARN" => 1,
        "ERROR" => 2,
        "PASS" => 3,
        "INFO" => 4,
        _ => 5,
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "PASS" => "[OK]  ",
        "FAIL" => "[FAIL]",
        "WARN" => "[WARN]",
        "INFO" => "[INFO]",
        "ERROR" => "[ERR] ",
        _ => "[?]   ",
    }
}

fn info_text(info: &[(String, Json)], key: &str, default: &str) -> String {
    info.iter()
        .find(|(name, _)| name == key)
        .map_or_else(|| default.to_string(), |(_, value)| show(&py_str(value)))
}

/// The human-readable report.
#[must_use]
pub fn format_report(result: &Preflight) -> String {
    let host = info_text(&result.info, "host", "?");
    let port = info_text(&result.info, "port", "?");
    if let Some(error) = &result.error {
        let mut lines = vec![format!("ERROR: {error}")];
        if !result.info.is_empty() {
            lines.push(format!("  Host: {host}:{port}"));
        }
        return lines.join("\n");
    }

    let info = &result.info;
    let cluster_mode = info
        .iter()
        .find(|(name, _)| name == "cluster_mode")
        .is_some_and(|(_, value)| truthy(Some(value)));
    let modules = match info.iter().find(|(name, _)| name == "modules") {
        // recorded-shape: the `modules` member of the preflight result this command built
        // from the snapshot, not an AWS response.
        Some((_, Json::Array(items))) if !items.is_empty() => items
            .iter()
            .map(|item| show(&py_str(item)))
            .collect::<Vec<_>>()
            .join(", "),
        _ => "none".to_string(),
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push("=".repeat(72));
    lines.push("ElastiCache Migration Preflight Report".to_string());
    lines.push("=".repeat(72));
    lines.push(String::new());
    lines.push(format!("  Source:          {host}:{port}"));
    lines.push(format!(
        "  Redis version:   {}",
        info_text(info, "redis_version", "unknown")
    ));
    lines.push(format!(
        "  OS:              {}",
        info_text(info, "os", "unknown")
    ));
    lines.push(format!(
        "  Uptime:          {} days",
        info_text(info, "uptime_days", "?")
    ));
    lines.push(format!(
        "  Cluster mode:    {}",
        if cluster_mode { "enabled" } else { "disabled" }
    ));
    lines.push(format!(
        "  Memory:          {} MB ({} GB)",
        info_text(info, "used_memory_mb", "?"),
        info_text(info, "used_memory_gb", "?")
    ));
    lines.push(format!(
        "  Keys:            {}",
        info_text(info, "total_keys", "?")
    ));
    lines.push(format!(
        "  Replicas:        {}",
        info_text(info, "connected_replicas", "?")
    ));
    lines.push(format!("  Modules:         {modules}"));
    lines.push(String::new());

    let fail_count = result.count("FAIL");
    let warn_count = result.count("WARN");
    let pass_count = result.count("PASS");
    if fail_count == 0 {
        lines.push(format!(
            "  Verdict: READY TO MIGRATE ({pass_count} passed, {warn_count} warnings)"
        ));
    } else {
        lines.push(format!(
            "  Verdict: BLOCKERS FOUND ({fail_count} failures, {warn_count} warnings)"
        ));
    }
    lines.push(String::new());
    lines.push("-".repeat(72));

    let mut sorted: Vec<&Finding> = result.findings.iter().collect();
    sorted.sort_by_key(|finding| status_rank(finding.status));
    for finding in &sorted {
        lines.push(format!(
            "  {} {}",
            status_icon(finding.status),
            finding.check
        ));
        lines.push(format!("         {}", finding.detail));
        lines.push(String::new());
    }

    if fail_count > 0 {
        lines.push("=".repeat(72));
        lines.push("MIGRATION BLOCKERS".to_string());
        lines.push("=".repeat(72));
        lines.push(String::new());
        for finding in sorted.iter().filter(|finding| finding.status == "FAIL") {
            lines.push(format!("  - {}: {}", finding.check, finding.detail));
        }
        lines.push(String::new());
        lines.push("  Resolve blockers before starting migration.".to_string());
        lines.push("  See references/migration/instructions.md for guidance.".to_string());
    }

    lines.push(String::new());
    lines.push("  Next steps:".to_string());
    lines.push("  1. Resolve any blockers above".to_string());
    lines.push("  2. Run: toolkit elasticache price to estimate target cost".to_string());
    lines.push(
        "  3. Use the AWS CLI test-migration command to validate connectivity to the target cluster"
            .to_string(),
    );
    lines.push(
        "  4. See references/migration/instructions.md for the full migration workflow".to_string(),
    );
    lines.push(String::new());
    lines.join("\n")
}
