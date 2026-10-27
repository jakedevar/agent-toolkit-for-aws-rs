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
//!
//! The audit is fail-closed: a `PASS` needs positive evidence (an exactly
//! matching record, usable identifiers, every member cluster and security
//! group read). Anything missing, malformed or unmatched is a not-inspected
//! `ERROR` finding instead; the table of `PASS` paths is in
//! `docs/migration/elasticache.md`.

use std::net::{Ipv4Addr, Ipv6Addr};

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

fn describe_text(value: Option<&Json>, default: &str) -> String {
    value.map_or_else(|| default.to_string(), |item| show(&py_str(item)))
}

/// The member `key` unless it is absent or `null`.
fn present<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value
        .get(key)
        .filter(|member| !matches!(member, Json::Null))
}

/// A usable identifier: a string holding at least one non-whitespace character.
/// Anything else (absent, `null`, empty, blank, another type) identifies nothing.
fn identifier(value: &Json) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

/// The `ERROR` finding for something the audit could not inspect.
fn not_inspected(check: impl Into<String>, why: &str) -> Finding {
    Finding::new(check, "ERROR", format!("Not inspected: {why}"))
}

/// Whether a value is a finite number greater than zero.
fn positive_number(value: &Json) -> bool {
    match value {
        Json::Int(n) => *n > 0,
        Json::Float(n) => n.is_finite() && *n > 0.0,
        _ => false,
    }
}

/// How a `SnapshotRetentionLimit` reads.
enum Retention {
    /// A whole number of days, at least one.
    Days,
    /// Absent, `null`, zero, negative or not a number: no backups (the
    /// script's `0 or not set`).
    Off,
    /// A positive number that is not a whole number of days.
    Malformed,
}

fn retention_of(value: &Json) -> Retention {
    match value {
        Json::Int(n) if *n > 0 => Retention::Days,
        Json::Float(n) if *n > 0.0 => Retention::Malformed,
        _ => Retention::Off,
    }
}

/// The recorded tags, or why the tag list could not be read. An absent or
/// `null` list is no tags (the script's answer to a failed tag call).
fn tags_of(describe: &Json) -> Result<Json, &'static str> {
    let list = match present(describe, "tags") {
        None => return Ok(Json::Object(Vec::new())),
        Some(Json::Array(list)) => list,
        Some(_) => return Err("the tag list is not a list"),
    };
    let mut tags: Vec<(String, Json)> = Vec::new();
    for tag in list {
        match (
            tag.get("Key").and_then(Json::as_str),
            tag.get("Value")
                .filter(|value| matches!(value, Json::String(_))),
        ) {
            (Some(key), Some(value)) => set_member(&mut tags, key, value.clone()),
            _ => return Err("a tag entry is not an object with a string Key and a string Value"),
        }
    }
    Ok(Json::Object(tags))
}

/// Adds the tag check and the `tags` info member. A recommended tag counts as
/// present only with a non-blank value.
fn audit_tags(findings: &mut Vec<Finding>, info: &mut Vec<(String, Json)>, describe: &Json) {
    let tags = match tags_of(describe) {
        Ok(tags) => tags,
        Err(why) => {
            findings.push(not_inspected("Resource tags", why));
            info.push(kv("tags", Json::Object(Vec::new())));
            return;
        }
    };
    let recommended = ["Environment", "managed_by", "Application", "Owner"];
    let missing: Vec<&str> = recommended
        .iter()
        .copied()
        .filter(|name| tags.get(name).and_then(identifier).is_none())
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
    info.push(kv("tags", tags));
}

/// A canonical prefix length: decimal digits, no sign, no leading zero, at most `max`.
fn prefix_length(text: &str, max: u8) -> Option<u8> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    canonical
        .then(|| text.parse::<u8>().ok())
        .flatten()
        .filter(|n| *n <= max)
}

/// Whether `text` is a well-formed `address/prefix` CIDR of the given family.
fn parsed_cidr(text: &str, ipv6: bool) -> Option<u8> {
    let (address, prefix) = text.split_once('/')?;
    if ipv6 {
        address.parse::<Ipv6Addr>().ok()?;
        prefix_length(prefix, 128)
    } else {
        address.parse::<Ipv4Addr>().ok()?;
        prefix_length(prefix, 32)
    }
}

/// Whether a range entry is an object whose required CIDR member is a well-formed string.
fn range_inspectable(range: &Json, key: &str, ipv6: bool) -> bool {
    matches!(range, Json::Object(_))
        && range
            .get(key)
            .and_then(Json::as_str)
            .is_some_and(|cidr| parsed_cidr(cidr, ipv6).is_some())
}

/// Whether every entry of a rule's range list (absent means none) is inspectable.
fn ranges_inspectable(rule: &Json, list: &str, key: &str, ipv6: bool) -> bool {
    match rule.get(list) {
        None => true,
        Some(Json::Array(ranges)) => ranges
            .iter()
            .all(|range| range_inspectable(range, key, ipv6)),
        Some(_) => false,
    }
}

/// Whether a recorded security group carries a complete, correctly typed rule list:
/// every rule an object and every IPv4/IPv6 range entry an object with a well-formed CIDR.
fn rules_inspectable(group: &Json) -> bool {
    let Some(Json::Array(rules)) = group.get("IpPermissions") else {
        return false;
    };
    rules.iter().all(|rule| {
        matches!(rule, Json::Object(_))
            && ranges_inspectable(rule, "IpRanges", "CidrIp", false)
            && ranges_inspectable(rule, "Ipv6Ranges", "CidrIpv6", true)
    })
}

/// The `GroupId` of a recorded security group, when it is a usable identifier
/// (a missing, `null`, empty or non-string id matches no reference).
fn group_id(group: &Json) -> Option<&str> {
    group.get("GroupId").and_then(identifier)
}

/// The security groups a resource references, and an `ERROR` finding for every
/// reference (or member cluster) that could not be read.
#[derive(Default)]
struct GroupRefs {
    /// Usable group ids, without duplicates.
    ids: Vec<String>,
    /// Not-inspected findings of the discovery.
    errors: Vec<Finding>,
}

impl GroupRefs {
    fn add(&mut self, id: &str) {
        if !self.ids.iter().any(|known| known == id) {
            self.ids.push(id.to_string());
        }
    }

    fn fail(&mut self, check: impl Into<String>, why: &str) {
        self.errors.push(not_inspected(check, why));
    }
}

fn audit_security_groups(describe: &Json, refs: GroupRefs) -> Vec<Finding> {
    let GroupRefs {
        ids: sg_ids,
        errors,
    } = refs;
    let mut findings = errors;
    if sg_ids.is_empty() {
        // Nothing referenced is a warning only when every reference was read.
        if findings.is_empty() {
            findings.push(Finding::new(
                "Security groups",
                "WARN",
                "No security groups found",
            ));
        }
        return findings;
    }
    match describe.get("security_groups_error") {
        None | Some(Json::Null) => {}
        Some(error) => {
            let detail = error.as_str().map_or_else(
                || "the recorded security group error is not a string".to_string(),
                show,
            );
            findings.push(Finding::new(
                "Security groups",
                "ERROR",
                format!("Could not inspect: {detail}"),
            ));
            return findings;
        }
    }
    // A group nobody described was not inspected: never report it as safe.
    let Some(Json::Array(records)) = describe.get("security_groups") else {
        let shown: Vec<String> = sg_ids.iter().map(|id| show(id)).collect();
        findings.push(Finding::new(
            "Security groups",
            "ERROR",
            format!(
                "Could not inspect: no security group records were supplied for {}",
                shown.join(", ")
            ),
        ));
        return findings;
    };

    for id in &sg_ids {
        let matching: Vec<&Json> = records
            .iter()
            .filter(|group| group_id(group) == Some(id.as_str()))
            .collect();
        if matching.is_empty() {
            findings.push(not_inspected(
                format!("Security group {}", show(id)),
                "no record for this security group was supplied",
            ));
        } else if !matching.iter().all(|group| rules_inspectable(group)) {
            findings.push(not_inspected(
                format!("Security group {}", show(id)),
                "IpPermissions is missing or malformed",
            ));
        }
    }

    for group in records {
        let Some(id) = group_id(group) else {
            continue;
        };
        if !sg_ids.iter().any(|wanted| wanted == id) || !rules_inspectable(group) {
            continue;
        }
        let sg_id = show(id);
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
                let cidr = str_of(range, "CidrIpv6", "");
                if cidr == "::/0" {
                    findings.push(Finding::new(
                        format!("Security group {sg_id}"),
                        "WARN",
                        format!("Inbound rule allows ::/0 (IPv6) on ports {from_port}-{to_port} -- no public endpoint exists, but ::/0 violates least-privilege"),
                    ));
                } else if parsed_cidr(cidr, true) == Some(0) {
                    // Another spelling of the whole IPv6 space (for example 0::/0).
                    findings.push(Finding::new(
                        format!("Security group {sg_id}"),
                        "WARN",
                        format!("Inbound rule allows {} (IPv6) on ports {from_port}-{to_port} -- covers the whole IPv6 space, like ::/0", show(cidr)),
                    ));
                }
            }
        }
    }

    // PASS only when every reference was read, every group inspected and nothing flagged.
    if findings.is_empty() {
        let shown: Vec<String> = sg_ids.iter().map(|id| show(id)).collect();
        findings.push(Finding::new(
            "Security groups",
            "PASS",
            format!("No overly permissive rules in {}", shown.join(", ")),
        ));
    }
    findings
}

/// The one recorded resource whose `key` is exactly `id`, or why there is none:
/// no record, a record without that identifier, or more than one record.
fn resource_record<'a>(
    describe: &'a Json,
    list: &str,
    key: &str,
    id: &str,
) -> Result<&'a Json, &'static str> {
    if id.trim().is_empty() {
        return Err("not found");
    }
    let mut matching = items(describe.get(list))
        .iter()
        .filter(|record| record.get(key).and_then(identifier) == Some(id));
    let Some(record) = matching.next() else {
        return Err("not found");
    };
    if matching.next().is_some() {
        return Err("is recorded more than once");
    }
    Ok(record)
}

/// The security groups a serverless cache references.
fn serverless_group_refs(cache: &Json) -> GroupRefs {
    let mut refs = GroupRefs::default();
    match present(cache, "SecurityGroupIds") {
        None => {}
        Some(Json::Array(list)) => {
            for (index, entry) in list.iter().enumerate() {
                match identifier(entry) {
                    Some(id) => refs.add(id),
                    None => refs.fail(
                        "Security groups",
                        &format!(
                            "SecurityGroupIds entry {} is not a non-empty string",
                            index + 1
                        ),
                    ),
                }
            }
        }
        Some(_) => refs.fail("Security groups", "SecurityGroupIds is not a list"),
    }
    refs
}

/// Audits a serverless cache.
#[must_use]
pub fn audit_serverless(describe: &Json, cache_name: &str) -> Audit {
    let cache = match resource_record(
        describe,
        "serverless_caches",
        "ServerlessCacheName",
        cache_name,
    ) {
        Ok(cache) => cache,
        Err(why) => {
            return Audit::NotFound(format!("Serverless cache '{}' {why}", show(cache_name)));
        }
    };

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
    match cache.get("UserGroupId") {
        Some(group) if identifier(group).is_some() => {
            findings.push(Finding::new(
                "Authentication",
                "PASS",
                format!("RBAC user group: {}", show(&py_str(group))),
            ));
            info.push(kv("auth_mode", js("RBAC")));
            info.push(kv("user_group", group.clone()));
        }
        Some(group) if truthy(Some(group)) => {
            findings.push(not_inspected(
                "Authentication",
                "UserGroupId is not a non-empty string",
            ));
            info.push(kv("auth_mode", js("unknown")));
        }
        _ => {
            findings.push(Finding::new(
                "Authentication",
                "WARN",
                "No user group attached — using default user only",
            ));
            info.push(kv("auth_mode", js("default-user-only")));
        }
    }

    // Encryption at rest (always on; a named key must be a usable id).
    info.push(kv("encryption_at_rest", Json::Bool(true)));
    match cache.get("KmsKeyId") {
        Some(key) if identifier(key).is_some() => {
            findings.push(Finding::new(
                "Encryption at rest",
                "PASS",
                format!("KMS key: {}", show(&py_str(key))),
            ));
            info.push(kv("kms_key", key.clone()));
        }
        Some(key) if truthy(Some(key)) => {
            findings.push(not_inspected(
                "Encryption at rest",
                "KmsKeyId is not a non-empty string",
            ));
        }
        _ => {
            findings.push(Finding::new(
                "Encryption at rest",
                "PASS",
                "Always enabled on serverless (using AWS-managed key)",
            ));
        }
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
    match retention_of(&retention) {
        Retention::Days => {
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
        }
        Retention::Malformed => {
            info.push(kv("daily_backups", Json::Bool(false)));
            findings.push(not_inspected(
                "Automatic backups",
                "SnapshotRetentionLimit is not a whole number of days",
            ));
        }
        Retention::Off => {
            info.push(kv("daily_backups", Json::Bool(false)));
            findings.push(Finding::new(
                "Automatic backups",
                "FAIL",
                "No automatic backups configured, SnapshotRetentionLimit is 0 or not set",
            ));
        }
    }

    // Usage limits (cost control).
    let empty = Json::Object(Vec::new());
    let limits = cache.get("CacheUsageLimits").unwrap_or(&empty);
    let data_limit = limits.get("DataStorage").unwrap_or(&empty);
    let ecpu_limit = limits.get("ECPUPerSecond").unwrap_or(&empty);
    match data_limit.get("Maximum") {
        Some(maximum) if positive_number(maximum) => {
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
        }
        Some(maximum) if truthy(Some(maximum)) => findings.push(not_inspected(
            "Data storage limit",
            "CacheUsageLimits.DataStorage.Maximum is not a positive number",
        )),
        _ => findings.push(Finding::new(
            "Data storage limit",
            "WARN",
            "No data storage limit set — costs could grow unbounded",
        )),
    }
    match ecpu_limit.get("Maximum") {
        Some(maximum) if positive_number(maximum) => {
            info.push(kv("max_ecpu_per_sec", maximum.clone()));
            findings.push(Finding::new(
                "ECPU limit",
                "PASS",
                format!("{} ECPUs/sec max", show(&py_str(maximum))),
            ));
        }
        Some(maximum) if truthy(Some(maximum)) => findings.push(not_inspected(
            "ECPU limit",
            "CacheUsageLimits.ECPUPerSecond.Maximum is not a positive number",
        )),
        _ => findings.push(Finding::new(
            "ECPU limit",
            "WARN",
            "No ECPU limit set — costs could grow unbounded",
        )),
    }

    // Security groups.
    let refs = serverless_group_refs(cache);
    let sg_ids = refs.ids.clone();
    findings.extend(audit_security_groups(describe, refs));
    info.push(kv(
        "security_groups",
        Json::Array(sg_ids.iter().map(|id| js(id)).collect()),
    ));

    // Tags.
    audit_tags(&mut findings, &mut info, describe);

    Audit::Report { info, findings }
}

/// The recorded `describe_cache_clusters` answer for `id` (an entry holding an
/// `error` stands for a failed call). Only the engine details read this.
fn cluster_of<'a>(describe: &'a Json, id: &str) -> Option<&'a Json> {
    describe
        .get("cache_clusters")
        .and_then(|all| all.get(id))
        .filter(|cluster| cluster.get("error").is_none())
}

/// Adds the member-cluster ids of `list` (absent or `null` is none) to
/// `members`: plain strings, or objects carrying the id under `key`. An entry
/// naming no usable id, or a `list` that is not a list, is recorded in `refs`.
fn add_member_ids(
    members: &mut Vec<String>,
    refs: &mut GroupRefs,
    list: Option<&Json>,
    key: Option<&str>,
    what: &str,
) {
    match list {
        None | Some(Json::Null) => {}
        Some(Json::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                let id = match key {
                    None => identifier(entry),
                    Some(key) => entry.get(key).and_then(identifier),
                };
                match id {
                    Some(id) => {
                        if !members.iter().any(|known| known == id) {
                            members.push(id.to_string());
                        }
                    }
                    None => refs.fail(
                        "Security groups",
                        &format!("{what} entry {} names no member cluster", index + 1),
                    ),
                }
            }
        }
        Some(_) => refs.fail("Security groups", &format!("{what} is not a list")),
    }
}

/// The security groups of every member cluster of a replication group. The
/// members are the union of `MemberClusters` and the `NodeGroupMembers` of
/// every node group; each must have a readable recorded cluster with a
/// `SecurityGroups` list, or the discovery records why not.
fn replication_group_refs(describe: &Json, group: &Json) -> GroupRefs {
    let mut refs = GroupRefs::default();
    let mut members: Vec<String> = Vec::new();
    add_member_ids(
        &mut members,
        &mut refs,
        group.get("MemberClusters"),
        None,
        "MemberClusters",
    );
    match present(group, "NodeGroups") {
        None => {}
        Some(Json::Array(node_groups)) => {
            for (index, node_group) in node_groups.iter().enumerate() {
                let what = format!("NodeGroups entry {}", index + 1);
                if matches!(node_group, Json::Object(_)) {
                    add_member_ids(
                        &mut members,
                        &mut refs,
                        node_group.get("NodeGroupMembers"),
                        Some("CacheClusterId"),
                        &format!("{what} NodeGroupMembers"),
                    );
                } else {
                    refs.fail("Security groups", &format!("{what} is not an object"));
                }
            }
        }
        Some(_) => refs.fail("Security groups", "NodeGroups is not a list"),
    }
    members.sort();

    let clusters = describe.get("cache_clusters");
    for id in &members {
        let check = format!("Security groups of cluster {}", show(id));
        let Some(cluster) = clusters.and_then(|all| all.get(id)) else {
            refs.fail(
                check,
                "no describe_cache_clusters record for this member was supplied",
            );
            continue;
        };
        if !matches!(cluster, Json::Object(_)) {
            refs.fail(check, "the recorded cluster is not an object");
            continue;
        }
        if let Some(error) = cluster.get("error") {
            refs.fail(
                check,
                &format!(
                    "describe_cache_clusters failed: {}",
                    describe_text(Some(error), "")
                ),
            );
            continue;
        }
        match cluster.get("SecurityGroups") {
            Some(Json::Array(list)) => {
                for (index, entry) in list.iter().enumerate() {
                    match entry.get("SecurityGroupId").and_then(identifier) {
                        Some(sg) => refs.add(sg),
                        None => refs.fail(
                            check.clone(),
                            &format!(
                                "SecurityGroups entry {} has no non-empty string SecurityGroupId",
                                index + 1
                            ),
                        ),
                    }
                }
            }
            None | Some(Json::Null) => {
                refs.fail(check, "the recorded cluster has no SecurityGroups list");
            }
            Some(_) => refs.fail(check, "SecurityGroups is not a list"),
        }
    }
    refs.ids.sort();
    refs
}

/// Audits a node-based replication group.
#[must_use]
pub fn audit_replication_group(describe: &Json, rg_id: &str) -> Audit {
    let group = match resource_record(describe, "replication_groups", "ReplicationGroupId", rg_id) {
        Ok(group) => group,
        Err(why) => return Audit::NotFound(format!("Replication group '{}' {why}", show(rg_id))),
    };

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

    // Engine version from the first member cluster (display only).
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

    // TLS: only the boolean `true` passes; another truthy value is unreadable.
    let tls_value = group.get("TransitEncryptionEnabled");
    let tls = matches!(tls_value, Some(Json::Bool(true)));
    info.push(kv("tls_enabled", Json::Bool(tls)));
    findings.push(if tls {
        Finding::new("TLS in-transit encryption", "PASS", "Enabled")
    } else if truthy(tls_value) {
        not_inspected(
            "TLS in-transit encryption",
            "TransitEncryptionEnabled is not a boolean",
        )
    } else {
        Finding::new(
            "TLS in-transit encryption",
            "FAIL",
            "Not enabled — data in transit is unencrypted",
        )
    });

    // Encryption at rest.
    let at_rest_value = group.get("AtRestEncryptionEnabled");
    let at_rest = matches!(at_rest_value, Some(Json::Bool(true)));
    info.push(kv("encryption_at_rest", Json::Bool(at_rest)));
    if at_rest {
        match present(group, "KmsKeyId") {
            None => {
                findings.push(Finding::new(
                    "Encryption at rest",
                    "PASS",
                    "Enabled (key: AWS-managed)",
                ));
                info.push(kv("kms_key", js("AWS-managed")));
            }
            Some(key) => match identifier(key) {
                Some(name) => {
                    findings.push(Finding::new(
                        "Encryption at rest",
                        "PASS",
                        format!("Enabled (key: {})", show(name)),
                    ));
                    info.push(kv("kms_key", key.clone()));
                }
                None => findings.push(not_inspected(
                    "Encryption at rest",
                    "KmsKeyId is not a non-empty string",
                )),
            },
        }
    } else if truthy(at_rest_value) {
        findings.push(not_inspected(
            "Encryption at rest",
            "AtRestEncryptionEnabled is not a boolean",
        ));
    } else {
        findings.push(Finding::new(
            "Encryption at rest",
            "FAIL",
            "Not enabled — data at rest is unencrypted",
        ));
    }

    // Authentication.
    match group.get("UserGroupIds") {
        Some(Json::Array(user_groups)) if !user_groups.is_empty() => {
            let names: Option<Vec<String>> = user_groups
                .iter()
                .map(|g| identifier(g).map(show))
                .collect();
            if let Some(names) = names {
                info.push(kv("auth_mode", js("RBAC")));
                info.push(kv("user_groups", Json::Array(user_groups.clone())));
                findings.push(Finding::new(
                    "Authentication",
                    "PASS",
                    format!("RBAC user groups: {}", names.join(", ")),
                ));
            } else {
                info.push(kv("auth_mode", js("unknown")));
                findings.push(not_inspected(
                    "Authentication",
                    "UserGroupIds holds an entry that is not a non-empty string",
                ));
            }
        }
        Some(value) if !matches!(value, Json::Array(_)) && truthy(Some(value)) => {
            info.push(kv("auth_mode", js("unknown")));
            findings.push(not_inspected(
                "Authentication",
                "UserGroupIds is not a list",
            ));
        }
        _ => {
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
        }
    }

    // Multi-AZ and automatic failover: only the exact string "enabled" passes.
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
    findings.push(match retention_of(&retention) {
        Retention::Days => Finding::new(
            "Automatic backups",
            "PASS",
            format!("Retention: {} days", show(&py_str(&retention))),
        ),
        Retention::Malformed => not_inspected(
            "Automatic backups",
            "SnapshotRetentionLimit is not a whole number of days",
        ),
        Retention::Off => Finding::new(
            "Automatic backups",
            "FAIL",
            "No automatic backups — data loss risk on failure",
        ),
    });

    // Security groups of every member cluster (sorted, deduplicated).
    let refs = replication_group_refs(describe, group);
    let sg_ids = refs.ids.clone();
    findings.extend(audit_security_groups(describe, refs));
    info.push(kv(
        "security_groups",
        Json::Array(sg_ids.iter().map(|id| js(id)).collect()),
    ));

    // Tags.
    audit_tags(&mut findings, &mut info, describe);

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
    let mut lines: Vec<String> = vec![
        "=".repeat(72),
        "ElastiCache Security Audit Report".to_string(),
        "=".repeat(72),
        String::new(),
    ];

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
