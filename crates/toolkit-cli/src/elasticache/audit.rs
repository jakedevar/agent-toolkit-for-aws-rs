//! `toolkit elasticache audit` (port of `security_audit.py`): reports the
//! security and operational posture of a serverless cache or a node-based
//! replication group (TLS, auth, encryption at rest, Multi-AZ, failover,
//! backups, security groups, tags).
//!
//! The Python script called `describe_*` through `boto3`. This build has no
//! AWS client, so the audit runs over the recorded responses in a **describe
//! file** (see `docs/migration/elasticache.md`):
//!
//! ```json
//! {
//!   "serverless_caches": [ {"ServerlessCacheName": "c", "...": "..."} ],
//!   "replication_groups": [ {"ReplicationGroupId": "g", "MemberClusters": ["g-001"]} ],
//!   "cache_clusters": { "g-001": {"Engine": "valkey", "SecurityGroups": []} },
//!   "security_groups": [ {"GroupId": "sg-1", "IpPermissions": []} ],
//!   "security_groups_error": "text",
//!   "tags": [ {"Key": "Owner", "Value": "me"} ]
//! }
//! ```
//!
//! The members are the `ServerlessCaches`, `ReplicationGroups`,
//! `CacheClusters` (by cluster id), `SecurityGroups` and `TagList` of the API
//! responses. Nothing is written and no value of the file other than the
//! audited identifiers and the findings is echoed.

use toolkit_core::Json;

use crate::aurora::pyfmt::truthy;

use super::text::{js, kv, py_str, set_member, show};

/// One check outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Name of the check.
    pub check: String,
    /// `PASS`, `WARN`, `FAIL` or `ERROR`.
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

/// The result of an audit.
#[derive(Debug, Clone, PartialEq)]
pub enum Audit {
    /// The resource does not exist in the describe file.
    NotFound(String),
    /// What was learned and the checks.
    Report {
        /// Facts about the resource, in insertion order.
        info: Vec<(String, Json)>,
        /// The checks.
        findings: Vec<Finding>,
    },
}

impl Audit {
    /// The object `--json` prints.
    #[must_use]
    pub fn to_json(&self) -> Json {
        match self {
            Self::NotFound(message) => Json::Object(vec![kv("error", js(message))]),
            Self::Report { info, findings } => Json::Object(vec![
                kv("info", Json::Object(info.clone())),
                kv(
                    "findings",
                    Json::Array(findings.iter().map(Finding::to_json).collect()),
                ),
            ]),
        }
    }

    /// Whether the command exits with status 1.
    #[must_use]
    pub fn failed(&self) -> bool {
        match self {
            Self::NotFound(_) => true,
            Self::Report { findings, .. } => findings.iter().any(|f| f.status == "FAIL"),
        }
    }
}

fn str_of<'a>(value: &'a Json, key: &str, default: &'a str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap_or(default)
}

fn items(value: Option<&Json>) -> &[Json] {
    match value {
        Some(Json::Array(list)) => list,
        _ => &[],
    }
}

fn value_or(value: &Json, key: &str, default: Json) -> Json {
    value.get(key).cloned().unwrap_or(default)
}

fn number_of(value: Option<&Json>) -> f64 {
    match value {
        Some(Json::Int(n)) => *n as f64,
        Some(Json::Float(n)) => *n,
        _ => 0.0,
    }
}

fn describe_text(value: Option<&Json>, default: &str) -> String {
    value.map_or_else(|| default.to_string(), |item| show(&py_str(item)))
}

fn tags_of(describe: &Json) -> Json {
    let mut tags: Vec<(String, Json)> = Vec::new();
    for tag in items(describe.get("tags")) {
        if let (Some(key), Some(value)) = (tag.get("Key"), tag.get("Value")) {
            set_member(&mut tags, &py_str(key), value.clone());
        }
    }
    Json::Object(tags)
}

fn audit_tags(findings: &mut Vec<Finding>, tags: &Json) {
    let recommended = ["Environment", "managed_by", "Application", "Owner"];
    let missing: Vec<&str> = recommended
        .iter()
        .copied()
        .filter(|name| tags.get(name).is_none())
        .collect();
    if missing.is_empty() {
        findings.push(Finding::new(
            "Resource tags",
            "PASS",
            "All recommended tags present",
        ));
    } else {
        findings.push(Finding::new(
            "Resource tags",
            "WARN",
            format!("Missing recommended tags: {}", missing.join(", ")),
        ));
    }
}

fn audit_security_groups(describe: &Json, sg_ids: &[String]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if sg_ids.is_empty() {
        findings.push(Finding::new(
            "Security groups",
            "WARN",
            "No security groups found",
        ));
        return findings;
    }
    if let Some(message) = describe.get("security_groups_error").and_then(Json::as_str) {
        findings.push(Finding::new(
            "Security groups",
            "ERROR",
            format!("Could not inspect: {}", show(message)),
        ));
        return findings;
    }

    for group in items(describe.get("security_groups")) {
        if !sg_ids.iter().any(|id| id == str_of(group, "GroupId", "")) {
            continue;
        }
        let sg_id = show(str_of(group, "GroupId", ""));
        for rule in items(group.get("IpPermissions")) {
            let from_port = describe_text(rule.get("FromPort"), "0");
            let to_port = describe_text(rule.get("ToPort"), "0");
            for range in items(rule.get("IpRanges")) {
                let cidr = str_of(range, "CidrIp", "");
                let shown = show(cidr);
                if cidr == "0.0.0.0/0" {
                    findings.push(Finding::new(
                        format!("Security group {sg_id}"),
                        "WARN",
                        format!("Inbound rule allows 0.0.0.0/0 on ports {from_port}-{to_port} -- no public endpoint exists, but 0.0.0.0/0 violates least-privilege"),
                    ));
                } else if cidr.ends_with("/0") || cidr.ends_with("/8") {
                    findings.push(Finding::new(
                        format!("Security group {sg_id}"),
                        "WARN",
                        format!("Inbound rule allows {shown} on ports {from_port}-{to_port} -- very broad CIDR"),
                    ));
                }
            }
            for range in items(rule.get("Ipv6Ranges")) {
                if str_of(range, "CidrIpv6", "") == "::/0" {
                    findings.push(Finding::new(
                        format!("Security group {sg_id}"),
                        "WARN",
                        format!("Inbound rule allows ::/0 (IPv6) on ports {from_port}-{to_port} -- no public endpoint exists, but ::/0 violates least-privilege"),
                    ));
                }
            }
        }
    }

    if !findings
        .iter()
        .any(|finding| finding.check.starts_with("Security group"))
    {
        let shown: Vec<String> = sg_ids.iter().map(|id| show(id)).collect();
        findings.push(Finding::new(
            "Security groups",
            "PASS",
            format!("No overly permissive rules in {}", shown.join(", ")),
        ));
    }
    findings
}

/// Audits a serverless cache.
#[must_use]
pub fn audit_serverless(describe: &Json, cache_name: &str) -> Audit {
    let not_found =
        || Audit::NotFound(format!("Serverless cache '{}' not found", show(cache_name)));
    let Some(cache) = items(describe.get("serverless_caches")).first() else {
        return not_found();
    };
    if let Some(name) = cache.get("ServerlessCacheName").and_then(Json::as_str) {
        if name != cache_name {
            return not_found();
        }
    }

    let mut findings = Vec::new();
    let mut info = vec![
        kv("cache_type", js("serverless")),
        kv("cache_name", js(cache_name)),
        kv("engine", value_or(cache, "Engine", js("unknown"))),
        kv(
            "engine_version",
            value_or(cache, "MajorEngineVersion", js("unknown")),
        ),
        kv("status", value_or(cache, "Status", js("unknown"))),
        kv("arn", value_or(cache, "ARN", js("unknown"))),
    ];

    // TLS is always on for serverless.
    info.push(kv("tls_enabled", Json::Bool(true)));
    findings.push(Finding::new(
        "TLS in-transit encryption",
        "PASS",
        "Always enabled on serverless",
    ));

    // Serverless requires RBAC.
    if let Some(group) = cache.get("UserGroupId").filter(|g| truthy(Some(*g))) {
        findings.push(Finding::new(
            "Authentication",
            "PASS",
            format!("RBAC user group: {}", show(&py_str(group))),
        ));
        info.push(kv("auth_mode", js("RBAC")));
        info.push(kv("user_group", group.clone()));
    } else {
        findings.push(Finding::new(
            "Authentication",
            "WARN",
            "No user group attached — using default user only",
        ));
        info.push(kv("auth_mode", js("default-user-only")));
    }

    // Encryption at rest.
    if let Some(key) = cache.get("KmsKeyId").filter(|k| truthy(Some(*k))) {
        findings.push(Finding::new(
            "Encryption at rest",
            "PASS",
            format!("KMS key: {}", show(&py_str(key))),
        ));
        info.push(kv("encryption_at_rest", Json::Bool(true)));
        info.push(kv("kms_key", key.clone()));
    } else {
        findings.push(Finding::new(
            "Encryption at rest",
            "PASS",
            "Always enabled on serverless (using AWS-managed key)",
        ));
        info.push(kv("encryption_at_rest", Json::Bool(true)));
    }

    // Multi-AZ is always on.
    info.push(kv("multi_az", Json::Bool(true)));
    findings.push(Finding::new(
        "Multi-AZ",
        "PASS",
        "Always enabled on serverless",
    ));

    // Backups.
    let retention = value_or(cache, "SnapshotRetentionLimit", Json::Int(0));
    info.push(kv("snapshot_retention_days", retention.clone()));
    if number_of(Some(&retention)) > 0.0 {
        let snapshot_time = describe_text(cache.get("DailySnapshotTime"), "not set");
        info.push(kv("daily_backups", Json::Bool(true)));
        findings.push(Finding::new(
            "Automatic backups",
            "PASS",
            format!(
                "Retention: {} days, daily snapshot time: {snapshot_time}",
                show(&py_str(&retention))
            ),
        ));
    } else {
        info.push(kv("daily_backups", Json::Bool(false)));
        findings.push(Finding::new(
            "Automatic backups",
            "FAIL",
            "No automatic backups configured, SnapshotRetentionLimit is 0 or not set",
        ));
    }

    // Usage limits (cost control).
    let empty = Json::Object(Vec::new());
    let limits = cache.get("CacheUsageLimits").unwrap_or(&empty);
    let data_limit = limits.get("DataStorage").unwrap_or(&empty);
    let ecpu_limit = limits.get("ECPUPerSecond").unwrap_or(&empty);
    if let Some(maximum) = data_limit.get("Maximum").filter(|m| truthy(Some(*m))) {
        info.push(kv("max_data_gb", maximum.clone()));
        findings.push(Finding::new(
            "Data storage limit",
            "PASS",
            format!(
                "{} {} max",
                show(&py_str(maximum)),
                describe_text(data_limit.get("Unit"), "GB")
            ),
        ));
    } else {
        findings.push(Finding::new(
            "Data storage limit",
            "WARN",
            "No data storage limit set — costs could grow unbounded",
        ));
    }
    if let Some(maximum) = ecpu_limit.get("Maximum").filter(|m| truthy(Some(*m))) {
        info.push(kv("max_ecpu_per_sec", maximum.clone()));
        findings.push(Finding::new(
            "ECPU limit",
            "PASS",
            format!("{} ECPUs/sec max", show(&py_str(maximum))),
        ));
    } else {
        findings.push(Finding::new(
            "ECPU limit",
            "WARN",
            "No ECPU limit set — costs could grow unbounded",
        ));
    }

    // Security groups.
    let sg_ids: Vec<String> = items(cache.get("SecurityGroupIds"))
        .iter()
        .map(py_str)
        .collect();
    findings.extend(audit_security_groups(describe, &sg_ids));
    info.push(kv(
        "security_groups",
        Json::Array(sg_ids.iter().map(|id| js(id)).collect()),
    ));

    // Tags.
    let tags = tags_of(describe);
    audit_tags(&mut findings, &tags);
    info.push(kv("tags", tags));

    Audit::Report { info, findings }
}

/// The recorded `describe_cache_clusters` answer for `id` (an entry holding an
/// `error` stands for a failed call).
fn cluster_of<'a>(describe: &'a Json, id: &str) -> Option<&'a Json> {
    describe
        .get("cache_clusters")
        .and_then(|all| all.get(id))
        .filter(|cluster| cluster.get("error").is_none())
}

/// Audits a node-based replication group.
#[must_use]
pub fn audit_replication_group(describe: &Json, rg_id: &str) -> Audit {
    let not_found = || Audit::NotFound(format!("Replication group '{}' not found", show(rg_id)));
    let Some(group) = items(describe.get("replication_groups")).first() else {
        return not_found();
    };
    if let Some(name) = group.get("ReplicationGroupId").and_then(Json::as_str) {
        if name != rg_id {
            return not_found();
        }
    }

    let mut findings = Vec::new();
    let node_groups = items(group.get("NodeGroups"));
    let mut info = vec![
        kv("cache_type", js("node-based")),
        kv("replication_group_id", js(rg_id)),
        kv("engine", js("unknown")),
        kv("status", value_or(group, "Status", js("unknown"))),
        kv("arn", value_or(group, "ARN", js("unknown"))),
        kv(
            "cluster_enabled",
            value_or(group, "ClusterEnabled", Json::Bool(false)),
        ),
        kv(
            "num_node_groups",
            Json::Int(i64::try_from(node_groups.len()).unwrap_or(i64::MAX)),
        ),
    ];

    // Engine version from the first member cluster.
    if let Some(first) = items(group.get("MemberClusters")).first() {
        match cluster_of(describe, &py_str(first)) {
            Some(cluster) => {
                set_member(
                    &mut info,
                    "engine",
                    value_or(cluster, "Engine", js("unknown")),
                );
                info.push(kv(
                    "engine_version",
                    value_or(cluster, "EngineVersion", js("unknown")),
                ));
                info.push(kv(
                    "node_type",
                    value_or(cluster, "CacheNodeType", js("unknown")),
                ));
            }
            None => {
                info.push(kv("engine_version", js("unknown")));
                info.push(kv("node_type", js("unknown")));
            }
        }
    }

    // TLS.
    let tls = truthy(group.get("TransitEncryptionEnabled"));
    info.push(kv("tls_enabled", Json::Bool(tls)));
    findings.push(if tls {
        Finding::new("TLS in-transit encryption", "PASS", "Enabled")
    } else {
        Finding::new(
            "TLS in-transit encryption",
            "FAIL",
            "Not enabled — data in transit is unencrypted",
        )
    });

    // Encryption at rest.
    let at_rest = truthy(group.get("AtRestEncryptionEnabled"));
    info.push(kv("encryption_at_rest", Json::Bool(at_rest)));
    if at_rest {
        let kms = describe_text(group.get("KmsKeyId"), "AWS-managed");
        findings.push(Finding::new(
            "Encryption at rest",
            "PASS",
            format!("Enabled (key: {kms})"),
        ));
        info.push(kv(
            "kms_key",
            value_or(group, "KmsKeyId", js("AWS-managed")),
        ));
    } else {
        findings.push(Finding::new(
            "Encryption at rest",
            "FAIL",
            "Not enabled — data at rest is unencrypted",
        ));
    }

    // Authentication.
    let user_groups: Vec<Json> = items(group.get("UserGroupIds")).to_vec();
    if user_groups.is_empty() {
        if truthy(group.get("AuthTokenEnabled")) {
            info.push(kv("auth_mode", js("AUTH-token")));
            findings.push(Finding::new(
                "Authentication",
                "WARN",
                "Using legacy AUTH token — consider migrating to RBAC",
            ));
        } else {
            info.push(kv("auth_mode", js("none")));
            findings.push(Finding::new(
                "Authentication",
                "FAIL",
                "No authentication configured — any VPC client can access data",
            ));
        }
    } else {
        let names: Vec<String> = user_groups.iter().map(|g| show(&py_str(g))).collect();
        info.push(kv("auth_mode", js("RBAC")));
        info.push(kv("user_groups", Json::Array(user_groups)));
        findings.push(Finding::new(
            "Authentication",
            "PASS",
            format!("RBAC user groups: {}", names.join(", ")),
        ));
    }

    // Multi-AZ and automatic failover.
    let multi_az = str_of(group, "MultiAZ", "disabled") == "enabled";
    info.push(kv("multi_az", Json::Bool(multi_az)));
    findings.push(if multi_az {
        Finding::new("Multi-AZ", "PASS", "Enabled")
    } else {
        Finding::new("Multi-AZ", "WARN", "Not enabled — single-AZ failure risk")
    });
    let failover = str_of(group, "AutomaticFailover", "disabled") == "enabled";
    info.push(kv("automatic_failover", Json::Bool(failover)));
    findings.push(if failover {
        Finding::new("Automatic failover", "PASS", "Enabled")
    } else {
        Finding::new(
            "Automatic failover",
            "WARN",
            "Not enabled — manual intervention needed on primary failure",
        )
    });

    // Backups.
    let retention = value_or(group, "SnapshotRetentionLimit", Json::Int(0));
    info.push(kv("snapshot_retention_days", retention.clone()));
    if number_of(Some(&retention)) > 0.0 {
        findings.push(Finding::new(
            "Automatic backups",
            "PASS",
            format!("Retention: {} days", show(&py_str(&retention))),
        ));
    } else {
        findings.push(Finding::new(
            "Automatic backups",
            "FAIL",
            "No automatic backups — data loss risk on failure",
        ));
    }

    // Security groups of the member clusters (sorted, deduplicated).
    let mut sg_ids: Vec<String> = Vec::new();
    for node_group in node_groups {
        for member in items(node_group.get("NodeGroupMembers")) {
            let Some(id) = member.get("CacheClusterId") else {
                continue;
            };
            let Some(cluster) = cluster_of(describe, &py_str(id)) else {
                continue;
            };
            for group_ref in items(cluster.get("SecurityGroups")) {
                if let Some(sg) = group_ref.get("SecurityGroupId") {
                    let sg = py_str(sg);
                    if !sg_ids.contains(&sg) {
                        sg_ids.push(sg);
                    }
                }
            }
        }
    }
    sg_ids.sort();
    findings.extend(audit_security_groups(describe, &sg_ids));
    info.push(kv(
        "security_groups",
        Json::Array(sg_ids.iter().map(|id| js(id)).collect()),
    ));

    // Tags.
    let tags = tags_of(describe);
    audit_tags(&mut findings, &tags);
    info.push(kv("tags", tags));

    Audit::Report { info, findings }
}

fn status_rank(status: &str) -> u8 {
    match status {
        "FAIL" => 0,
        "WARN" => 1,
        "ERROR" => 2,
        "INFO" => 3,
        "PASS" => 4,
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
pub fn format_report(result: &Audit) -> String {
    let (info, findings) = match result {
        Audit::NotFound(message) => return format!("ERROR: {message}"),
        Audit::Report { info, findings } => (info, findings),
    };
    let mut lines: Vec<String> = Vec::new();
    lines.push("=".repeat(72));
    lines.push("ElastiCache Security Audit Report".to_string());
    lines.push("=".repeat(72));
    lines.push(String::new());

    let cache_type = info_text(info, "cache_type", "?");
    lines.push(format!("  Cache type:      {cache_type}"));
    if cache_type == "serverless" {
        lines.push(format!(
            "  Cache name:      {}",
            info_text(info, "cache_name", "?")
        ));
    } else {
        lines.push(format!(
            "  Replication group: {}",
            info_text(info, "replication_group_id", "?")
        ));
        lines.push(format!(
            "  Node type:       {}",
            info_text(info, "node_type", "unknown")
        ));
        let cluster_enabled = info
            .iter()
            .find(|(name, _)| name == "cluster_enabled")
            .is_some_and(|(_, value)| truthy(Some(value)));
        lines.push(format!(
            "  Cluster mode:    {}",
            if cluster_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ));
        lines.push(format!(
            "  Node groups:     {}",
            info_text(info, "num_node_groups", "unknown")
        ));
    }
    lines.push(format!(
        "  Engine:          {} {}",
        info_text(info, "engine", "?"),
        info_text(info, "engine_version", "")
    ));
    lines.push(format!(
        "  Status:          {}",
        info_text(info, "status", "?")
    ));
    lines.push(format!(
        "  Auth mode:       {}",
        info_text(info, "auth_mode", "unknown")
    ));
    lines.push(String::new());

    let count = |status: &str| findings.iter().filter(|f| f.status == status).count();
    let (fail_count, warn_count, pass_count) = (count("FAIL"), count("WARN"), count("PASS"));
    lines.push(format!(
        "  Summary: {pass_count} passed, {warn_count} warnings, {fail_count} failures"
    ));
    lines.push(String::new());
    lines.push("-".repeat(72));

    let mut sorted: Vec<&Finding> = findings.iter().collect();
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
        lines.push("RECOMMENDED ACTIONS".to_string());
        lines.push("=".repeat(72));
        lines.push(String::new());
        for finding in sorted.iter().filter(|finding| finding.status == "FAIL") {
            let check = &finding.check;
            if check.contains("TLS") {
                lines.push(
                    "  - Enable in-transit encryption. For existing node-based clusters, use a"
                        .into(),
                );
                lines.push(
                    "    two-step process: first set transit-encryption-mode to 'preferred',"
                        .into(),
                );
                lines.push(
                    "    then after migrating all clients to TLS, set it to 'required'.".into(),
                );
            } else if check.contains("Encryption at rest") {
                lines.push(
                    "  - Enable at-rest encryption. For existing clusters, this requires".into(),
                );
                lines.push(
                    "    creating a new cluster with AtRestEncryptionEnabled=true and migrating."
                        .into(),
                );
            } else if check.contains("Authentication") {
                lines.push(
                    "  - Configure RBAC: create users, a user group, and attach it to the cache."
                        .into(),
                );
                lines
                    .push("    See references/setup/iam-policies.md for IAM auth patterns.".into());
            } else if check.to_lowercase().contains("backups") {
                lines
                    .push("  - Enable automatic backups: modify the replication group with".into());
                lines.push("    SnapshotRetentionLimit > 0 (recommended: 7 days).".into());
            } else if finding.detail.contains("0.0.0.0/0") {
                lines.push(format!(
                    "  - Restrict {check}: remove the 0.0.0.0/0 inbound rule and scope to"
                ));
                lines.push("    application security groups only.".into());
            }
            lines.push(String::new());
        }
    }

    lines.push("  Note: This is an informational review, not a security certification.".into());
    lines.push("  Consult your security team before production deployment.".into());
    lines.push(String::new());
    lines.join("\n")
}
