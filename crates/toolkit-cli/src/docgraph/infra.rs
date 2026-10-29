//! Infrastructure checks of the review (`run_infra_checks` of `wa_review.py`).
//!
//! Every AWS call goes through an [`AwsClient`]. The check ids, pillars,
//! thresholds, statuses, label and detail texts are those of the script, in
//! the same order. Two deliberate differences, recorded in
//! `docs/migration/docgraph.md`: a failed call names its error **class** (the
//! stable code and, for a service error, its sanitized error code) and never
//! the service's message, and a response member of the wrong type fails the
//! check that reads it (`invalid response`) instead of raising a traceback.

use std::collections::HashSet;

use toolkit_aws::recorded::{
    optional_list, optional_object, optional_object_list, optional_string_list, optional_text,
    MemberError,
};
use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::access::{optional_bool, optional_int, optional_number};
use super::checks::{Findings, Status};
use super::client::{AwsClient, Service};
use super::snapshot::group;
use super::tables::{connection_limit, ram_gib, GRAVITON_FAMILIES};
use crate::safe_text::{safe_path, safe_prefix};

const PAGE_LIMIT: usize = 1000;
const SEVEN_DAYS: i64 = 7 * 86_400;
const THIRTEEN_DAYS: i64 = 13 * 86_400;
const GIB: f64 = 1_073_741_824.0;
const MIB: f64 = 1_048_576.0;

/// Why one call or one read of a response failed.
enum Fail {
    Aws(AwsError),
    Member(MemberError),
    Shape(&'static str),
}

impl From<AwsError> for Fail {
    fn from(error: AwsError) -> Self {
        Self::Aws(error)
    }
}

impl From<MemberError> for Fail {
    fn from(error: MemberError) -> Self {
        Self::Member(error)
    }
}

impl Fail {
    /// The error class, never the service's message or a response value.
    fn describe(&self) -> String {
        match self {
            Self::Aws(error) => {
                let mut text = error.code().to_string();
                if let AwsError::Service(service) = error {
                    if let Some(code) = service.code().filter(|code| {
                        !code.is_empty()
                            && code.len() <= 64
                            && code
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    }) {
                        text.push_str(" (");
                        text.push_str(code);
                        text.push(')');
                    }
                }
                text
            }
            Self::Member(error) => format!("invalid response ({error})"),
            Self::Shape(what) => format!("invalid response ({what})"),
        }
    }
}

/// The time window of one run, as the strings the requests carry.
struct Window {
    start: String,
    end: String,
}

fn call(
    client: &mut dyn AwsClient,
    service: Service,
    operation: &str,
    request: &Json,
) -> Result<Json, Fail> {
    Ok(client.call(service, operation, request)?)
}

fn text(value: &str) -> Json {
    Json::string(value)
}

/// Walks the pages of a paginated operation. `visit` returns `true` to stop
/// early. A repeated token or more than [`PAGE_LIMIT`] pages is a failure.
fn pages(
    client: &mut dyn AwsClient,
    service: Service,
    operation: &str,
    base: &[(&str, Json)],
    request_token: &str,
    response_token: &str,
    mut visit: impl FnMut(&Json) -> Result<bool, Fail>,
) -> Result<(), Fail> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut token: Option<String> = None;
    for _ in 0..PAGE_LIMIT {
        let mut members: Vec<(String, Json)> = base
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        if let Some(token) = &token {
            members.push((request_token.to_string(), Json::string(token.clone())));
        }
        let response = call(client, service, operation, &Json::Object(members))?;
        if visit(&response)? {
            return Ok(());
        }
        match toolkit_aws::next_token_from(&response, response_token)? {
            None => return Ok(()),
            Some(next) => {
                if !seen.insert(next.clone()) {
                    return Err(AwsError::pagination("a repeated pagination token").into());
                }
                token = Some(next);
            }
        }
    }
    Err(AwsError::pagination("too many pages").into())
}

/// The statistic values of the datapoints of one metric, one row per
/// datapoint with a value per requested statistic.
fn metric(
    client: &mut dyn AwsClient,
    window: &Window,
    instance: &str,
    name: &str,
    period: i64,
    statistics: &[&str],
) -> Result<Vec<Vec<f64>>, Fail> {
    let request = Json::object(vec![
        ("Namespace", text("AWS/DocDB")),
        ("MetricName", text(name)),
        (
            "Dimensions",
            Json::Array(vec![Json::object(vec![
                ("Name", text("DBInstanceIdentifier")),
                ("Value", text(instance)),
            ])]),
        ),
        ("StartTime", text(&window.start)),
        ("EndTime", text(&window.end)),
        ("Period", Json::Int(period)),
        (
            "Statistics",
            Json::Array(statistics.iter().map(|stat| text(stat)).collect()),
        ),
    ]);
    let response = call(client, Service::CloudWatch, "GetMetricStatistics", &request)?;
    let mut rows = Vec::new();
    for point in optional_object_list(&response, "Datapoints")? {
        let mut row = Vec::with_capacity(statistics.len());
        for stat in statistics {
            row.push(
                optional_number(point, stat)?
                    .ok_or(Fail::Shape("a datapoint lacks a requested statistic"))?,
            );
        }
        rows.push(row);
    }
    Ok(rows)
}

fn column(rows: &[Vec<f64>], index: usize) -> Vec<f64> {
    rows.iter().map(|row| row[index]).collect()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn min_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::INFINITY, f64::min)
}

fn max_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

struct Cluster {
    retention: i64,
    deletion_protection: bool,
    engine_version: String,
    parameter_group: String,
    security_groups: Vec<String>,
    encrypted: bool,
    log_exports: Vec<String>,
    subnet_group: String,
    maintenance_window: String,
    arn: Option<String>,
    storage_type: String,
}

fn string_or(object: &Json, key: &str, default: &str) -> Result<String, MemberError> {
    Ok(optional_text(object, key)?.unwrap_or(default).to_string())
}

impl Cluster {
    fn read(cluster: &Json) -> Result<Self, MemberError> {
        let mut security_groups = Vec::new();
        for group in optional_object_list(cluster, "VpcSecurityGroups")? {
            security_groups.push(string_or(group, "VpcSecurityGroupId", "")?);
        }
        Ok(Self {
            retention: optional_int(cluster, "BackupRetentionPeriod")?.unwrap_or(1),
            deletion_protection: optional_bool(cluster, "DeletionProtection")?.unwrap_or(false),
            engine_version: string_or(cluster, "EngineVersion", "unknown")?,
            parameter_group: string_or(cluster, "DBClusterParameterGroup", "")?,
            security_groups,
            encrypted: optional_bool(cluster, "StorageEncrypted")?.unwrap_or(false),
            log_exports: optional_string_list(cluster, "EnabledCloudwatchLogsExports")?
                .into_iter()
                .map(str::to_string)
                .collect(),
            subnet_group: string_or(cluster, "DBSubnetGroup", "")?,
            maintenance_window: string_or(cluster, "PreferredMaintenanceWindow", "not set")?,
            arn: optional_text(cluster, "DBClusterArn")?.map(str::to_string),
            storage_type: string_or(cluster, "StorageType", "standard")?,
        })
    }
}

struct Instance {
    id: String,
    class: String,
    zone: String,
    writer: bool,
}

impl Instance {
    fn read(instance: &Json) -> Result<Self, Fail> {
        let required = |key: &str| {
            optional_text(instance, key)?
                .map(str::to_string)
                .ok_or(Fail::Shape("an instance lacks its identifier or class"))
        };
        Ok(Self {
            id: required("DBInstanceIdentifier")?,
            class: required("DBInstanceClass")?,
            zone: string_or(instance, "AvailabilityZone", "")?,
            writer: optional_bool(instance, "IsClusterWriter")?.unwrap_or(false),
        })
    }
}

fn status_of(pass: bool, otherwise: Status) -> Status {
    if pass {
        Status::Pass
    } else {
        otherwise
    }
}

fn detail_if(condition: bool, text: &str) -> String {
    (if condition { text } else { "" }).to_string()
}

/// The infrastructure checks for `cluster_id`, in the order of the script.
/// `now` is the instant of the run (`datetime.now(timezone.utc)`), already
/// formatted; `window` strings are derived from it by the caller.
pub(super) fn run_infra_checks(
    client: &mut dyn AwsClient,
    cluster_id: &str,
    now_seconds: i64,
) -> Findings {
    let mut findings = Findings::default();
    let iso = |seconds: i64| crate::dynamodb::iterate::utc_iso(seconds);
    let cluster_json = match describe_cluster(client, cluster_id) {
        Ok(cluster) => cluster,
        Err(fail) => {
            findings.add(
                "Other",
                "ERR",
                format!("Cannot describe cluster: {}", fail.describe()),
                Status::Fail,
                "",
            );
            return findings;
        }
    };
    let cluster = match Cluster::read(&cluster_json) {
        Ok(cluster) => cluster,
        Err(error) => {
            findings.add(
                "Other",
                "ERR",
                format!(
                    "Cannot describe cluster: {}",
                    Fail::Member(error).describe()
                ),
                Status::Fail,
                "",
            );
            return findings;
        }
    };
    let instances = match describe_instances(client, cluster_id) {
        Ok(instances) => instances,
        Err(fail) => {
            findings.add(
                "Other",
                "ERR",
                format!("Cannot describe instances: {}", fail.describe()),
                Status::Fail,
                "",
            );
            Vec::new()
        }
    };

    reliability(
        client,
        &mut findings,
        cluster_id,
        &cluster,
        &instances,
        now_seconds,
    );
    security(client, &mut findings, cluster_id, &cluster);
    operations(client, &mut findings, cluster_id, &cluster);
    cost(client, &mut findings, &cluster);

    let window = Window {
        start: iso(now_seconds - SEVEN_DAYS),
        end: iso(now_seconds),
    };
    for instance in &instances {
        instance_checks(client, &mut findings, &window, instance);
    }
    findings
}

fn describe_cluster(client: &mut dyn AwsClient, cluster_id: &str) -> Result<Json, Fail> {
    let request = Json::object(vec![("DBClusterIdentifier", text(cluster_id))]);
    let response = call(client, Service::Docdb, "DescribeDBClusters", &request)?;
    optional_object_list(&response, "DBClusters")?
        .first()
        .cloned()
        .ok_or(Fail::Shape("no cluster in the response"))
}

fn describe_instances(client: &mut dyn AwsClient, cluster_id: &str) -> Result<Vec<Instance>, Fail> {
    let request = Json::object(vec![(
        "Filters",
        Json::Array(vec![Json::object(vec![
            ("Name", text("db-cluster-id")),
            ("Values", Json::Array(vec![text(cluster_id)])),
        ])]),
    )]);
    let response = call(client, Service::Docdb, "DescribeDBInstances", &request)?;
    optional_object_list(&response, "DBInstances")?
        .iter()
        .map(Instance::read)
        .collect()
}

fn reliability(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    cluster_id: &str,
    cluster: &Cluster,
    instances: &[Instance],
    now_seconds: i64,
) {
    let pillar = "Reliability";
    let retention = cluster.retention;
    findings.add(
        pillar,
        "REL1",
        format!("Backup retention period ({retention} days)"),
        if retention >= 7 {
            Status::Pass
        } else if retention >= 3 {
            Status::Warn
        } else {
            Status::Fail
        },
        detail_if(retention < 7, "Recommended: 7+ days for production"),
    );
    let protected = cluster.deletion_protection;
    findings.add(
        pillar,
        "REL2",
        format!(
            "Deletion protection ({})",
            if protected { "enabled" } else { "disabled" }
        ),
        status_of(protected, Status::Fail),
        detail_if(
            !protected,
            "Enable deletion protection for production clusters",
        ),
    );
    let count = instances.len();
    findings.add(
        pillar,
        "REL5a",
        format!("Instance count ({count})"),
        status_of(count >= 2, Status::Fail),
        detail_if(count < 2, "Minimum 2 instances required for auto failover"),
    );
    let zones: HashSet<&str> = instances.iter().map(|i| i.zone.as_str()).collect();
    findings.add(
        pillar,
        "REL5b",
        format!("Instances across {} AZ(s)", zones.len()),
        status_of(zones.len() >= 2, Status::Fail),
        detail_if(zones.len() < 2, "Single AZ -- no failover protection"),
    );
    let version = safe_path(&cluster.engine_version);
    let major = if cluster.engine_version == "unknown" {
        ""
    } else {
        cluster.engine_version.split('.').next().unwrap_or("")
    };
    match major {
        "3" | "4" => findings.add(
            pillar,
            "REL6",
            format!("Engine version {version} (approaching or past end-of-life)"),
            Status::Fail,
            "Upgrade to DocumentDB 5.0 or 8.0",
        ),
        "5" => findings.add(
            pillar,
            "REL6",
            format!("Engine version {version}"),
            Status::Pass,
            "Consider upgrading to 8.0 for Zstandard compression and Query Planner v3",
        ),
        _ => findings.add(
            pillar,
            "REL6",
            format!("Engine version {version}"),
            Status::Pass,
            "",
        ),
    }
    failover_events(client, findings, cluster_id, now_seconds);
}

/// One recent event: the message and the joined categories.
struct Event {
    message: String,
    categories: String,
}

fn failover_events(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    cluster_id: &str,
    now_seconds: i64,
) {
    let pillar = "Reliability";
    let iso = |seconds: i64| crate::dynamodb::iterate::utc_iso(seconds);
    let base = [
        ("SourceIdentifier", text(cluster_id)),
        ("SourceType", text("db-cluster")),
        ("StartTime", text(&iso(now_seconds - THIRTEEN_DAYS))),
        ("EndTime", text(&iso(now_seconds))),
    ];
    let mut events: Vec<Event> = Vec::new();
    let walked = pages(
        client,
        Service::Docdb,
        "DescribeEvents",
        &base,
        "Marker",
        "Marker",
        |page| {
            for event in optional_object_list(page, "Events")? {
                events.push(Event {
                    message: string_or(event, "Message", "")?,
                    categories: optional_string_list(event, "EventCategories")?.join(","),
                });
            }
            Ok(false)
        },
    );
    if let Err(fail) = walked {
        findings.add(
            pillar,
            "REL7",
            format!("Cannot check events: {}", fail.describe()),
            Status::Warn,
            "",
        );
        return;
    }
    let failovers: Vec<&Event> = events
        .iter()
        .filter(|event| {
            event.message.to_lowercase().contains("failover")
                || event.categories.to_lowercase().contains("failover")
        })
        .collect();
    if let Some(last) = failovers.last() {
        findings.add(
            pillar,
            "REL7",
            format!("{} failover event(s) in last 13 days", failovers.len()),
            Status::Warn,
            format!("Most recent: {}", safe_prefix(&last.message, 120)),
        );
    } else {
        findings.add(
            pillar,
            "REL7",
            "No failover events in last 13 days",
            Status::Pass,
            "",
        );
    }
}

fn security(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    cluster_id: &str,
    cluster: &Cluster,
) {
    let pillar = "Security";
    let encrypted = cluster.encrypted;
    findings.add(
        pillar,
        "SEC1a",
        format!(
            "Encryption at rest ({})",
            if encrypted { "enabled" } else { "disabled" }
        ),
        status_of(encrypted, Status::Fail),
        detail_if(
            !encrypted,
            "Enable encryption at rest (requires new cluster)",
        ),
    );

    let mut tls_value = "unknown".to_string();
    if !cluster.parameter_group.is_empty() {
        match tls_parameters(client, &cluster.parameter_group) {
            Ok(parameters) => {
                for (name, value) in parameters {
                    if name == "tls" {
                        tls_value = value.unwrap_or_else(|| "enabled".to_string());
                    } else if name == "tls_version" {
                        let version = value.unwrap_or_default();
                        let label = format!("TLS minimum version: {}", safe_path(&version));
                        if !version.is_empty()
                            && version.contains("1.2")
                            && !version.contains("1.0")
                            && !version.contains("1.1")
                        {
                            findings.add(pillar, "SEC6", label, Status::Pass, "");
                        } else if !version.is_empty() {
                            findings.add(
                                pillar,
                                "SEC6",
                                label,
                                Status::Warn,
                                "Set tls_version to TLSv1.2 to disable older protocols",
                            );
                        }
                    }
                }
            }
            Err(fail) => findings.add(
                pillar,
                "SEC6",
                format!("Cannot check TLS parameters: {}", fail.describe()),
                Status::Warn,
                "",
            ),
        }
    }
    if !findings.has("SEC6") {
        findings.add(
            pillar,
            "SEC6",
            "TLS minimum version: unknown",
            Status::Warn,
            "Could not determine tls_version parameter",
        );
    }
    let tls_label = format!("TLS ({})", safe_path(&tls_value));
    let (tls_status, tls_detail) = match tls_value.as_str() {
        "enabled" => (Status::Pass, ""),
        "unknown" => (Status::Warn, "Could not determine TLS status"),
        _ => (Status::Fail, "TLS should be enabled"),
    };
    findings.add(pillar, "SEC1b", tls_label, tls_status, tls_detail);

    security_groups(client, findings, cluster);

    let audit = cluster.log_exports.iter().any(|log| log == "audit");
    findings.add(
        pillar,
        "SEC5",
        format!(
            "Audit logging ({})",
            if audit { "enabled" } else { "disabled" }
        ),
        status_of(audit, Status::Warn),
        detail_if(!audit, "Enable audit logging for compliance"),
    );

    secrets(client, findings, cluster_id);
}

/// `(ParameterName, ParameterValue)` of the parameters named `tls` and
/// `tls_version`, in response order.
fn tls_parameters(
    client: &mut dyn AwsClient,
    group: &str,
) -> Result<Vec<(String, Option<String>)>, Fail> {
    let request = Json::object(vec![("DBClusterParameterGroupName", text(group))]);
    let response = call(
        client,
        Service::Docdb,
        "DescribeDBClusterParameters",
        &request,
    )?;
    let mut out = Vec::new();
    for parameter in optional_object_list(&response, "Parameters")? {
        let name = optional_text(parameter, "ParameterName")?.unwrap_or_default();
        if name == "tls" || name == "tls_version" {
            out.push((
                name.to_string(),
                optional_text(parameter, "ParameterValue")?.map(str::to_string),
            ));
        }
    }
    Ok(out)
}

/// What one security group allows from anywhere.
fn open_ranges(group: &Json) -> Result<Vec<&'static str>, Fail> {
    let mut open = Vec::new();
    for rule in optional_object_list(group, "IpPermissions")? {
        for range in optional_object_list(rule, "IpRanges")? {
            if optional_text(range, "CidrIp")? == Some("0.0.0.0/0") {
                open.push("0.0.0.0/0");
            }
        }
        for range in optional_object_list(rule, "Ipv6Ranges")? {
            if optional_text(range, "CidrIpv6")? == Some("::/0") {
                open.push("::/0");
            }
        }
    }
    Ok(open)
}

fn security_groups(client: &mut dyn AwsClient, findings: &mut Findings, cluster: &Cluster) {
    let pillar = "Security";
    let mut open = false;
    let mut checked = 0_usize;
    for id in cluster.security_groups.iter().filter(|id| !id.is_empty()) {
        let request = Json::object(vec![("GroupIds", Json::Array(vec![text(id)]))]);
        let shown = safe_path(id);
        let described = call(client, Service::Ec2, "DescribeSecurityGroups", &request)
            .and_then(|response| {
                optional_object_list(&response, "SecurityGroups")?
                    .first()
                    .cloned()
                    .ok_or(Fail::Shape("no security group in the response"))
            })
            .and_then(|group| open_ranges(&group));
        match described {
            Ok(ranges) => {
                checked += 1;
                for range in ranges {
                    open = true;
                    findings.add(
                        pillar,
                        "SEC2",
                        format!("Security group {shown} open to {range}"),
                        Status::Fail,
                        "Restrict to specific CIDR ranges",
                    );
                }
            }
            Err(fail) => findings.add(
                pillar,
                "SEC2",
                format!("Cannot check SG {shown}: {}", fail.describe()),
                Status::Warn,
                "",
            ),
        }
    }
    if !open && checked > 0 {
        findings.add(
            pillar,
            "SEC2",
            format!("Security groups properly restricted ({checked} checked)"),
            Status::Pass,
            "",
        );
    }
}

fn secrets(client: &mut dyn AwsClient, findings: &mut Findings, cluster_id: &str) {
    let needle = cluster_id.to_lowercase();
    let mut found = false;
    let walked = pages(
        client,
        Service::SecretsManager,
        "ListSecrets",
        &[],
        "NextToken",
        "NextToken",
        |page| {
            for secret in optional_object_list(page, "SecretList")? {
                let name = string_or(secret, "Name", "")?.to_lowercase();
                let description = string_or(secret, "Description", "")?.to_lowercase();
                if name.contains(&needle) || description.contains(&needle) {
                    found = true;
                    break;
                }
            }
            Ok(found)
        },
    );
    match walked {
        Ok(()) => findings.add(
            "Security",
            "SEC3",
            format!(
                "Secrets Manager {} this cluster",
                if found {
                    "references"
                } else {
                    "does not reference"
                }
            ),
            status_of(found, Status::Warn),
            detail_if(!found, "Store credentials in Secrets Manager"),
        ),
        Err(fail) => findings.add(
            "Security",
            "SEC3",
            format!("Cannot check Secrets Manager: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }
}

fn subnet_zones(client: &mut dyn AwsClient, name: &str) -> Result<usize, Fail> {
    let request = Json::object(vec![("DBSubnetGroupName", text(name))]);
    let response = call(client, Service::Docdb, "DescribeDBSubnetGroups", &request)?;
    let group = optional_object_list(&response, "DBSubnetGroups")?
        .first()
        .ok_or(Fail::Shape("no subnet group in the response"))?;
    let mut zones: HashSet<&str> = HashSet::new();
    for subnet in optional_object_list(group, "Subnets")? {
        let zone = optional_object(subnet, "SubnetAvailabilityZone")?
            .ok_or(Fail::Shape("a subnet lacks its availability zone"))?;
        zones.insert(
            optional_text(zone, "Name")?
                .ok_or(Fail::Shape("an availability zone lacks its name"))?,
        );
    }
    Ok(zones.len())
}

fn operations(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    cluster_id: &str,
    cluster: &Cluster,
) {
    let pillar = "Operational Excellence";
    if !cluster.subnet_group.is_empty() {
        match subnet_zones(client, &cluster.subnet_group) {
            Ok(zones) => findings.add(
                pillar,
                "OPS2",
                format!("Subnet group spans {zones} AZ(s)"),
                status_of(zones >= 3, Status::Warn),
                detail_if(zones < 3, "Recommended: 3 AZs for failover flexibility"),
            ),
            Err(fail) => findings.add(
                pillar,
                "OPS2",
                format!("Cannot check subnet group: {}", fail.describe()),
                Status::Warn,
                "",
            ),
        }
    }
    let profiler = cluster.log_exports.iter().any(|log| log == "profiler");
    findings.add(
        pillar,
        "OPS5a",
        format!(
            "Profiler logging ({})",
            if profiler { "enabled" } else { "disabled" }
        ),
        status_of(profiler, Status::Warn),
        detail_if(!profiler, "Enable profiler for slow query analysis"),
    );
    let default_group = cluster.parameter_group.starts_with("default.");
    findings.add(
        pillar,
        "OPS5c",
        format!("Parameter group: {}", safe_path(&cluster.parameter_group)),
        if default_group {
            Status::Warn
        } else {
            Status::Pass
        },
        detail_if(
            default_group,
            "Use a custom parameter group for workload-specific tuning",
        ),
    );
    findings.add(
        pillar,
        "OPS7",
        format!(
            "Maintenance window: {}",
            safe_path(&cluster.maintenance_window)
        ),
        Status::Info,
        "Verify this window aligns with your lowest-traffic period",
    );
    let request = Json::object(vec![("AlarmNamePrefix", text(cluster_id))]);
    let alarms = call(client, Service::CloudWatch, "DescribeAlarms", &request)
        .and_then(|response| Ok(optional_object_list(&response, "MetricAlarms")?.len()));
    match alarms {
        Ok(count) => findings.add(
            pillar,
            "OPS5b",
            format!("CloudWatch alarms ({count} configured)"),
            if count >= 3 {
                Status::Pass
            } else if count > 0 {
                Status::Warn
            } else {
                Status::Fail
            },
            detail_if(
                count < 3,
                "Recommended: alarms for CPU, FreeableMemory, DatabaseConnections",
            ),
        ),
        Err(fail) => findings.add(
            pillar,
            "OPS5b",
            format!("Cannot check alarms: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }
}

fn cost(client: &mut dyn AwsClient, findings: &mut Findings, cluster: &Cluster) {
    let pillar = "Cost Optimization";
    let tags = match &cluster.arn {
        None => Err(Fail::Shape("the cluster lacks its ARN")),
        Some(arn) => {
            let request = Json::object(vec![("ResourceName", text(arn))]);
            call(client, Service::Docdb, "ListTagsForResource", &request)
                .and_then(|response| Ok(optional_list(&response, "TagList")?.len()))
        }
    };
    match tags {
        Ok(count) => findings.add(
            pillar,
            "COST6",
            format!("Cost allocation tags ({count} tags)"),
            status_of(count >= 2, Status::Warn),
            detail_if(count < 2, "Add cost allocation tags for expense tracking"),
        ),
        Err(fail) => findings.add(
            pillar,
            "COST6",
            format!("Cannot check tags: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }
    let io_optimized = cluster.storage_type == "iopt1";
    findings.add(
        pillar,
        "COST7",
        format!("Storage type: {}", safe_path(&cluster.storage_type)),
        Status::Info,
        if io_optimized {
            "I/O-Optimized active -- no per-I/O charges"
        } else {
            "Evaluate I/O-Optimized for write-heavy workloads"
        },
    );
}

/// Python `f"{value:,.0f}"`.
fn commas_0f(value: f64) -> String {
    let digits = format!("{:.0}", value.abs());
    group(&digits, value < 0.0 && digits != "0")
}

/// Truncating `int(value)`.
fn trunc(value: f64) -> i64 {
    value as i64
}

#[allow(clippy::too_many_lines)]
fn instance_checks(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    window: &Window,
    instance: &Instance,
) {
    let iid = safe_path(&instance.id);
    let class = safe_path(&instance.class);
    let family = instance
        .class
        .strip_prefix("db.")
        .map_or("", |rest| rest.split('.').next().unwrap_or(""));

    cpu(client, findings, window, instance, &iid, &class);

    let graviton = GRAVITON_FAMILIES.contains(&family);
    findings.add(
        "Sustainability",
        "SUST1",
        format!(
            "{iid} {} Graviton ({class})",
            if graviton { "uses" } else { "does not use" }
        ),
        status_of(graviton, Status::Warn),
        detail_if(
            !graviton,
            "Migrate to Graviton (r6g/r8g) for better price-performance",
        ),
    );

    let perf = "Performance Efficiency";
    let id = instance.id.as_str();

    // Buffer cache hit ratio
    match metric(
        client,
        window,
        id,
        "BufferCacheHitRatio",
        3600,
        &["Average"],
    ) {
        Ok(rows) if !rows.is_empty() => {
            let avg = mean(&column(&rows, 0));
            findings.add(
                perf,
                "PERF6",
                format!("Buffer cache hit ratio for {iid} ({avg:.1}%)"),
                if avg >= 99.0 {
                    Status::Pass
                } else if avg >= 95.0 {
                    Status::Warn
                } else {
                    Status::Fail
                },
                detail_if(avg < 95.0, "Working set may not fit in memory"),
            );
        }
        Ok(_) => {}
        Err(fail) => findings.add(
            perf,
            "PERF6",
            format!("Cannot check cache for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }

    // Connections vs limits
    match metric(
        client,
        window,
        id,
        "DatabaseConnections",
        3600,
        &["Maximum"],
    ) {
        Ok(rows) if !rows.is_empty() => {
            let peak = max_of(&column(&rows, 0));
            match connection_limit(&instance.class) {
                Some(limit) if limit > 0 => {
                    let pct = peak / limit as f64 * 100.0;
                    findings.add(
                        perf,
                        "PERF5",
                        format!(
                            "Peak connections for {iid} ({}/{limit} = {pct:.0}%)",
                            trunc(peak)
                        ),
                        if pct < 70.0 {
                            Status::Pass
                        } else if pct < 90.0 {
                            Status::Warn
                        } else {
                            Status::Fail
                        },
                        detail_if(pct >= 70.0, "Consider upsizing or connection pooling"),
                    );
                }
                _ => findings.add(
                    perf,
                    "PERF5",
                    format!("Connection limit unknown for {iid} ({class})"),
                    Status::Warn,
                    "Instance type not in lookup table",
                ),
            }
        }
        Ok(_) => {}
        Err(fail) => findings.add(
            perf,
            "PERF5",
            format!("Cannot check connections for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }

    if !instance.writer {
        idle_reader(client, findings, window, id, &iid);
    }

    // FreeableMemory
    if let Some(ram) = ram_gib(&instance.class) {
        match metric(client, window, id, "FreeableMemory", 3600, &["Minimum"]) {
            Ok(rows) if !rows.is_empty() => {
                let least = min_of(&column(&rows, 0));
                let free_pct = least / (ram as f64 * GIB) * 100.0;
                findings.add(
                    perf,
                    "PERF11",
                    format!(
                        "FreeableMemory min for {iid}: {:.1} GiB ({free_pct:.0}%)",
                        least / GIB
                    ),
                    if free_pct < 5.0 {
                        Status::Fail
                    } else if free_pct < 10.0 {
                        Status::Warn
                    } else {
                        Status::Pass
                    },
                    detail_if(free_pct < 10.0, "Instance under memory pressure"),
                );
            }
            Ok(_) => {}
            Err(fail) => findings.add(
                perf,
                "PERF11",
                format!("Cannot check memory for {iid}: {}", fail.describe()),
                Status::Warn,
                "",
            ),
        }
    } else {
        findings.add(
            perf,
            "PERF11",
            format!("Unknown instance type {class} -- cannot check FreeableMemory"),
            Status::Warn,
            "",
        );
    }

    // SwapUsage
    match metric(client, window, id, "SwapUsage", 3600, &["Maximum"]) {
        Ok(rows) if !rows.is_empty() => {
            let peak = max_of(&column(&rows, 0));
            if peak > 0.0 {
                findings.add(
                    perf,
                    "PERF12",
                    format!("SwapUsage max for {iid}: {:.0} MB", peak / MIB),
                    Status::Fail,
                    "Instance is swapping -- critically undersized",
                );
            } else {
                findings.add(
                    perf,
                    "PERF12",
                    format!("No swap on {iid}"),
                    Status::Pass,
                    "",
                );
            }
        }
        Ok(_) => {}
        Err(fail) => findings.add(
            perf,
            "PERF12",
            format!("Cannot check swap for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }

    // DiskQueueDepth
    match metric(client, window, id, "DiskQueueDepth", 3600, &["Average"]) {
        Ok(rows) if !rows.is_empty() => {
            let avg = mean(&column(&rows, 0));
            findings.add(
                perf,
                "PERF13",
                format!("DiskQueueDepth avg for {iid}: {avg:.1}"),
                if avg > 5.0 {
                    Status::Warn
                } else {
                    Status::Pass
                },
                detail_if(
                    avg > 5.0,
                    "I/O backing up -- evaluate I/O-Optimized or upsizing",
                ),
            );
        }
        Ok(_) => {}
        Err(fail) => findings.add(
            perf,
            "PERF13",
            format!("Cannot check DiskQueueDepth for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }

    // IndexBufferCacheHitRatio
    match metric(
        client,
        window,
        id,
        "IndexBufferCacheHitRatio",
        3600,
        &["Average"],
    ) {
        Ok(rows) if !rows.is_empty() => {
            let avg = mean(&column(&rows, 0));
            findings.add(
                perf,
                "PERF14",
                format!("IndexBufferCacheHitRatio for {iid}: {avg:.1}%"),
                if avg >= 99.0 {
                    Status::Pass
                } else if avg >= 95.0 {
                    Status::Warn
                } else {
                    Status::Fail
                },
                detail_if(avg < 95.0, "Indexes do not fit in memory"),
            );
        }
        Ok(_) => {}
        Err(fail) => findings.add(
            perf,
            "PERF14",
            format!("Cannot check index cache for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }

    // DatabaseCursorsTimedOut
    match metric(
        client,
        window,
        id,
        "DatabaseCursorsTimedOut",
        86_400,
        &["Sum"],
    ) {
        Ok(rows) => {
            let total: f64 = column(&rows, 0).iter().sum();
            if total > 0.0 {
                findings.add(
                    "Reliability",
                    "REL8",
                    format!(
                        "{} cursor(s) timed out on {iid} in last 7 days",
                        trunc(total)
                    ),
                    Status::Warn,
                    "Application may not be closing cursors properly",
                );
            } else {
                findings.add(
                    "Reliability",
                    "REL8",
                    format!("No cursor timeouts on {iid}"),
                    Status::Pass,
                    "",
                );
            }
        }
        Err(fail) => findings.add(
            "Reliability",
            "REL8",
            format!("Cannot check cursors for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }

    // AvailableMVCCIds (writer only)
    if instance.writer {
        match metric(client, window, id, "AvailableMVCCIds", 3600, &["Minimum"]) {
            Ok(rows) if !rows.is_empty() => {
                let least = min_of(&column(&rows, 0));
                let pct = least / 1_400_000_000.0 * 100.0;
                findings.add(
                    "Reliability",
                    "REL9",
                    format!("AvailableMVCCIds min: {} ({pct:.0}%)", commas_0f(least)),
                    if pct < 25.0 {
                        Status::Fail
                    } else if pct < 50.0 {
                        Status::Warn
                    } else {
                        Status::Pass
                    },
                    detail_if(
                        pct < 50.0,
                        "MVCC ID exhaustion risk -- investigate long-running transactions",
                    ),
                );
            }
            Ok(_) => {}
            Err(fail) => findings.add(
                "Reliability",
                "REL9",
                format!("Cannot check MVCCIds: {}", fail.describe()),
                Status::Warn,
                "",
            ),
        }
    }
}

/// CPU, using the hourly Maximum for the P95 to capture peak usage.
fn cpu(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    window: &Window,
    instance: &Instance,
    iid: &str,
    class: &str,
) {
    let pillar = "Cost Optimization";
    match metric(
        client,
        window,
        &instance.id,
        "CPUUtilization",
        3600,
        &["Average", "Maximum"],
    ) {
        Ok(rows) if !rows.is_empty() => {
            let average = mean(&column(&rows, 0));
            let mut maxima = column(&rows, 1);
            maxima.sort_by(f64::total_cmp);
            let p95 = maxima[((maxima.len() as f64 * 0.95) as usize).min(maxima.len() - 1)];
            let low = p95 < 10.0;
            findings.add(
                pillar,
                "COST1",
                format!("CPU for {iid} (avg {average:.1}%, P95 {p95:.1}%)"),
                if low { Status::Warn } else { Status::Pass },
                if low {
                    format!("Instance {class} may be oversized")
                } else {
                    String::new()
                },
            );
        }
        Ok(_) => {}
        Err(fail) => findings.add(
            pillar,
            "COST1",
            format!("Cannot check CPU for {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }
}

fn idle_reader(
    client: &mut dyn AwsClient,
    findings: &mut Findings,
    window: &Window,
    id: &str,
    iid: &str,
) {
    let pillar = "Cost Optimization";
    let rows = metric(
        client,
        window,
        id,
        "DatabaseConnections",
        3600,
        &["Average"],
    )
    .and_then(|connections| {
        metric(client, window, id, "ReadIOPS", 3600, &["Average"]).map(|iops| (connections, iops))
    });
    match rows {
        Ok((connections, iops)) => {
            let avg = |rows: &[Vec<f64>]| {
                if rows.is_empty() {
                    0.0
                } else {
                    mean(&column(rows, 0))
                }
            };
            let (conn, io) = (avg(&connections), avg(&iops));
            if conn < 2.0 && io < 5.0 {
                findings.add(
                    pillar,
                    "COST9",
                    format!("Reader {iid} appears idle (avg {conn:.0} conn, {io:.0} ReadIOPS)"),
                    Status::Warn,
                    "Consider removing this replica to reduce cost",
                );
            } else {
                findings.add(
                    pillar,
                    "COST9",
                    format!("Reader {iid} is active (avg {conn:.0} conn, {io:.0} ReadIOPS)"),
                    Status::Pass,
                    "",
                );
            }
        }
        Err(fail) => findings.add(
            pillar,
            "COST9",
            format!("Cannot check reader {iid}: {}", fail.describe()),
            Status::Warn,
            "",
        ),
    }
}
