//! `documentdb wa-review`: infrastructure checks, database checks, report and
//! the command, over recordings built here.

use super::{find, json, review, run, summary, TempDir, NOW, SECRET};
use crate::docgraph::checks::Status;
use crate::docgraph::client::RecordedClient;
use crate::docgraph::dbchecks::run_db_checks;
use crate::docgraph::snapshot::read_analysis;
use crate::docgraph::{report, tables};

const START: &str = "2026-01-24T12:00:00Z";

/// What a recording of a cluster contains; the default is a healthy cluster.
pub(super) struct Cluster {
    retention: i64,
    deletion_protection: bool,
    encrypted: bool,
    engine: &'static str,
    parameter_group: &'static str,
    tls: &'static str,
    tls_version: &'static str,
    logs: &'static str,
    open_ranges: &'static str,
    secret_name: &'static str,
    alarms: usize,
    tags: usize,
    zones: &'static str,
    instances: Vec<(&'static str, &'static str, &'static str, bool)>,
    cpu: (f64, f64),
    connections_max: f64,
    freeable_gib: f64,
    swap_max: f64,
    mvcc_min: f64,
    sg_failure: Option<&'static str>,
}

impl Default for Cluster {
    fn default() -> Self {
        Self {
            retention: 7,
            deletion_protection: true,
            encrypted: true,
            engine: "5.0.0",
            parameter_group: "prod-params",
            tls: "enabled",
            tls_version: "TLSv1.2",
            logs: r#"["audit","profiler"]"#,
            open_ranges: r#"[{"CidrIp":"10.0.0.0/16"}]"#,
            secret_name: "prod-docdb/credentials",
            alarms: 3,
            tags: 2,
            zones: r#"["a","b","c"]"#,
            instances: vec![
                ("i-writer", "db.r6g.large", "us-east-1a", true),
                ("i-reader", "db.r6g.large", "us-east-1b", false),
            ],
            cpu: (40.0, 60.0),
            connections_max: 100.0,
            freeable_gib: 8.0,
            swap_max: 0.0,
            mvcc_min: 1_400_000_000.0,
            sg_failure: None,
        }
    }
}

pub(super) fn call(
    service: &str,
    operation: &str,
    request: Option<&str>,
    response: &str,
) -> String {
    let request = request.map_or(String::new(), |r| format!(r#""request": {r},"#));
    format!(
        r#"{{"service": "{service}", "operation": "{operation}", {request} "response": {response}}}"#
    )
}

fn points(statistic: &str, values: &[f64]) -> String {
    let rows: Vec<String> = values
        .iter()
        .map(|v| format!(r#"{{"{statistic}": {v:?}}}"#))
        .collect();
    format!(r#"{{"Datapoints": [{}]}}"#, rows.join(","))
}

fn metric(iid: &str, name: &str, period: i64, stats: &[&str], response: &str) -> String {
    let statistics: Vec<String> = stats.iter().map(|s| format!(r#""{s}""#)).collect();
    let request = format!(
        r#"{{"Namespace":"AWS/DocDB","MetricName":"{name}","Dimensions":[{{"Name":"DBInstanceIdentifier","Value":"{iid}"}}],"StartTime":"{START}","EndTime":"{NOW}","Period":{period},"Statistics":[{}]}}"#,
        statistics.join(",")
    );
    call(
        "cloudwatch",
        "GetMetricStatistics",
        Some(&request),
        response,
    )
}

impl Cluster {
    pub(super) fn recording(&self) -> String {
        let mut calls = Vec::new();
        let cluster = format!(
            r#"{{"DBClusters": [{{"BackupRetentionPeriod": {}, "DeletionProtection": {}, "StorageEncrypted": {},
              "EngineVersion": "{}", "DBClusterParameterGroup": "{}", "VpcSecurityGroups": [{{"VpcSecurityGroupId": "sg-0abc"}}],
              "EnabledCloudwatchLogsExports": {}, "DBSubnetGroup": "sng", "PreferredMaintenanceWindow": "sun:05:00-sun:06:00",
              "DBClusterArn": "arn:aws:rds:us-east-1:111122223333:cluster:prod-docdb", "StorageType": "iopt1"}}]}}"#,
            self.retention,
            self.deletion_protection,
            self.encrypted,
            self.engine,
            self.parameter_group,
            self.logs
        );
        calls.push(call(
            "docdb",
            "DescribeDBClusters",
            Some(r#"{"DBClusterIdentifier": "prod-docdb"}"#),
            &cluster,
        ));
        let instances: Vec<String> = self
            .instances
            .iter()
            .map(|(id, class, zone, writer)| {
                format!(
                    r#"{{"DBInstanceIdentifier": "{id}", "DBInstanceClass": "{class}", "AvailabilityZone": "{zone}", "IsClusterWriter": {writer}}}"#
                )
            })
            .collect();
        calls.push(call(
            "docdb",
            "DescribeDBInstances",
            Some(r#"{"Filters": [{"Name": "db-cluster-id", "Values": ["prod-docdb"]}]}"#),
            &format!(r#"{{"DBInstances": [{}]}}"#, instances.join(",")),
        ));
        calls.push(call("docdb", "DescribeEvents", None, r#"{"Events": []}"#));
        calls.push(call(
            "docdb",
            "DescribeDBClusterParameters",
            None,
            &format!(
                r#"{{"Parameters": [{{"ParameterName": "tls", "ParameterValue": "{}"}}, {{"ParameterName": "tls_version", "ParameterValue": "{}"}}, {{"ParameterName": "other", "ParameterValue": "x"}}]}}"#,
                self.tls, self.tls_version
            ),
        ));
        if let Some(code) = self.sg_failure {
            calls.push(format!(
                r#"{{"service": "ec2", "operation": "DescribeSecurityGroups", "error": {{"code": "{code}", "message": "{SECRET}"}}}}"#
            ));
        } else {
            calls.push(call(
                "ec2",
                "DescribeSecurityGroups",
                Some(r#"{"GroupIds": ["sg-0abc"]}"#),
                &format!(
                    r#"{{"SecurityGroups": [{{"IpPermissions": [{{"IpRanges": {}, "Ipv6Ranges": []}}]}}]}}"#,
                    self.open_ranges
                ),
            ));
        }
        calls.push(call(
            "secretsmanager",
            "ListSecrets",
            None,
            &format!(r#"{{"SecretList": [{{"Name": "{}"}}]}}"#, self.secret_name),
        ));
        let subnets: Vec<String> = serde_zones(self.zones)
            .iter()
            .map(|z| format!(r#"{{"SubnetAvailabilityZone": {{"Name": "{z}"}}}}"#))
            .collect();
        calls.push(call(
            "docdb",
            "DescribeDBSubnetGroups",
            None,
            &format!(
                r#"{{"DBSubnetGroups": [{{"Subnets": [{}]}}]}}"#,
                subnets.join(",")
            ),
        ));
        let alarms: Vec<&str> = vec!["{}"; self.alarms];
        calls.push(call(
            "cloudwatch",
            "DescribeAlarms",
            None,
            &format!(r#"{{"MetricAlarms": [{}]}}"#, alarms.join(",")),
        ));
        let tags: Vec<&str> = vec!["{}"; self.tags];
        calls.push(call(
            "docdb",
            "ListTagsForResource",
            None,
            &format!(r#"{{"TagList": [{}]}}"#, tags.join(",")),
        ));
        for (id, _, _, writer) in &self.instances {
            let ram = self.freeable_gib * 1_073_741_824.0;
            calls.push(metric(
                id,
                "CPUUtilization",
                3600,
                &["Average", "Maximum"],
                &format!(
                    r#"{{"Datapoints": [{{"Average": {:?}, "Maximum": {:?}}}]}}"#,
                    self.cpu.0, self.cpu.1
                ),
            ));
            calls.push(metric(
                id,
                "BufferCacheHitRatio",
                3600,
                &["Average"],
                &points("Average", &[99.5]),
            ));
            calls.push(metric(
                id,
                "DatabaseConnections",
                3600,
                &["Maximum"],
                &points("Maximum", &[self.connections_max]),
            ));
            calls.push(metric(
                id,
                "DatabaseConnections",
                3600,
                &["Average"],
                &points("Average", &[10.0]),
            ));
            calls.push(metric(
                id,
                "ReadIOPS",
                3600,
                &["Average"],
                &points("Average", &[100.0]),
            ));
            calls.push(metric(
                id,
                "FreeableMemory",
                3600,
                &["Minimum"],
                &points("Minimum", &[ram]),
            ));
            calls.push(metric(
                id,
                "SwapUsage",
                3600,
                &["Maximum"],
                &points("Maximum", &[self.swap_max]),
            ));
            calls.push(metric(
                id,
                "DiskQueueDepth",
                3600,
                &["Average"],
                &points("Average", &[0.5]),
            ));
            calls.push(metric(
                id,
                "IndexBufferCacheHitRatio",
                3600,
                &["Average"],
                &points("Average", &[99.9]),
            ));
            calls.push(metric(
                id,
                "DatabaseCursorsTimedOut",
                86400,
                &["Sum"],
                r#"{"Datapoints": []}"#,
            ));
            if *writer {
                calls.push(metric(
                    id,
                    "AvailableMVCCIds",
                    3600,
                    &["Minimum"],
                    &points("Minimum", &[self.mvcc_min]),
                ));
            }
        }
        format!(r#"{{"calls": [{}]}}"#, calls.join(",\n"))
    }
}

fn serde_zones(text: &str) -> Vec<String> {
    text.trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|zone| zone.trim().trim_matches('"').to_string())
        .collect()
}

fn ids(findings: &crate::docgraph::checks::Findings) -> Vec<String> {
    summary(findings)
}

#[test]
fn a_healthy_cluster_passes_in_the_order_of_the_script() {
    let findings = review(&Cluster::default().recording());
    let writer: Vec<&str> = vec![
        "COST1", "SUST1", "PERF6", "PERF5", "PERF11", "PERF12", "PERF13", "PERF14", "REL8", "REL9",
    ];
    let reader: Vec<&str> = vec![
        "COST1", "SUST1", "PERF6", "PERF5", "COST9", "PERF11", "PERF12", "PERF13", "PERF14", "REL8",
    ];
    let mut expected: Vec<&str> = vec![
        "REL1", "REL2", "REL5a", "REL5b", "REL6", "REL7", "SEC1a", "SEC6", "SEC1b", "SEC2", "SEC5",
        "SEC3", "OPS2", "OPS5a", "OPS5c", "OPS7", "OPS5b", "COST6", "COST7",
    ];
    expected.extend(&writer);
    expected.extend(&reader);
    let got = ids(&findings);
    let got_ids: Vec<&str> = got.iter().map(|s| s.split(':').next().unwrap()).collect();
    assert_eq!(got_ids, expected);
    for check in &findings.checks {
        let allowed = if check.id == "OPS7" || check.id == "COST7" {
            Status::Info
        } else {
            Status::Pass
        };
        assert_eq!(check.status, allowed, "{} {}", check.id, check.label);
    }
    assert_eq!(
        find(&findings, "REL1")[0].label,
        "Backup retention period (7 days)"
    );
    assert_eq!(
        find(&findings, "COST1")[0].label,
        "CPU for i-writer (avg 40.0%, P95 60.0%)"
    );
    assert_eq!(
        find(&findings, "PERF5")[0].label,
        "Peak connections for i-writer (100/3400 = 3%)"
    );
    assert_eq!(
        find(&findings, "PERF11")[0].label,
        "FreeableMemory min for i-writer: 8.0 GiB (50%)"
    );
    assert_eq!(
        find(&findings, "REL9")[0].label,
        "AvailableMVCCIds min: 1,400,000,000 (100%)"
    );
    assert_eq!(
        find(&findings, "SEC6")[0].label,
        "TLS minimum version: TLSv1.2"
    );
    assert_eq!(
        find(&findings, "COST9")[0].label,
        "Reader i-reader is active (avg 10 conn, 100 ReadIOPS)"
    );
}

#[test]
fn a_degraded_cluster_reports_each_finding() {
    let cluster = Cluster {
        retention: 2,
        deletion_protection: false,
        encrypted: false,
        engine: "4.0.0",
        parameter_group: "default.docdb5.0",
        tls: "disabled",
        tls_version: "TLSv1",
        logs: "[]",
        open_ranges: r#"[{"CidrIp":"0.0.0.0/0"},{"CidrIp":"10.0.0.0/8"}]"#,
        secret_name: "unrelated",
        alarms: 0,
        tags: 1,
        zones: r#"["a","b"]"#,
        instances: vec![("i-only", "db.t3.medium", "us-east-1a", true)],
        cpu: (2.0, 8.0),
        connections_max: 950.0,
        freeable_gib: 0.1,
        swap_max: 5_242_880.0,
        mvcc_min: 100_000_000.0,
        sg_failure: None,
    };
    let findings = review(&cluster.recording());
    let status = |id: &str| find(&findings, id)[0].status;
    assert_eq!(status("REL1"), Status::Fail);
    assert_eq!(status("REL2"), Status::Fail);
    assert_eq!(status("REL5a"), Status::Fail);
    assert_eq!(status("REL5b"), Status::Fail);
    assert_eq!(status("REL6"), Status::Fail);
    assert_eq!(status("SEC1a"), Status::Fail);
    assert_eq!(status("SEC6"), Status::Warn);
    assert_eq!(status("SEC1b"), Status::Fail);
    assert_eq!(find(&findings, "SEC2").len(), 1);
    assert_eq!(
        find(&findings, "SEC2")[0].label,
        "Security group sg-0abc open to 0.0.0.0/0"
    );
    assert_eq!(status("SEC5"), Status::Warn);
    assert_eq!(status("SEC3"), Status::Warn);
    assert_eq!(status("OPS2"), Status::Warn);
    assert_eq!(status("OPS5c"), Status::Warn);
    assert_eq!(status("OPS5b"), Status::Fail);
    assert_eq!(status("COST6"), Status::Warn);
    assert_eq!(status("COST1"), Status::Warn);
    assert_eq!(
        find(&findings, "COST1")[0].detail,
        "Instance db.t3.medium may be oversized"
    );
    assert_eq!(status("PERF5"), Status::Fail);
    assert_eq!(status("PERF11"), Status::Fail);
    assert_eq!(status("PERF12"), Status::Fail);
    assert_eq!(
        find(&findings, "PERF12")[0].label,
        "SwapUsage max for i-only: 5 MB"
    );
    assert_eq!(status("REL9"), Status::Fail);
    assert_eq!(
        find(&findings, "REL6")[0].label,
        "Engine version 4.0.0 (approaching or past end-of-life)"
    );
    assert!(
        find(&findings, "COST9").is_empty(),
        "the only instance is the writer"
    );
}

#[test]
fn both_open_ranges_are_reported_and_a_failed_lookup_is_a_warning() {
    let both = Cluster {
        open_ranges: r#"[{"CidrIp":"0.0.0.0/0"}]"#,
        ..Cluster::default()
    };
    let text = both.recording().replace(
        r#""Ipv6Ranges": []"#,
        r#""Ipv6Ranges": [{"CidrIpv6": "::/0"}]"#,
    );
    let findings = review(&text);
    let labels: Vec<&str> = find(&findings, "SEC2")
        .iter()
        .map(|c| c.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Security group sg-0abc open to 0.0.0.0/0",
            "Security group sg-0abc open to ::/0"
        ]
    );
    let denied = Cluster {
        sg_failure: Some("UnauthorizedOperation"),
        ..Cluster::default()
    };
    let findings = review(&denied.recording());
    let sec2 = find(&findings, "SEC2");
    assert_eq!(sec2.len(), 1);
    assert_eq!(sec2[0].status, Status::Warn);
    assert_eq!(
        sec2[0].label,
        "Cannot check SG sg-0abc: aws_service_error (UnauthorizedOperation)"
    );
}

#[test]
fn a_service_failure_names_its_class_and_never_the_message() {
    let failing = format!(
        r#"{{"calls": [{{"service": "docdb", "operation": "DescribeDBClusters",
           "error": {{"code": "AccessDenied", "message": "{SECRET}"}}}}]}}"#
    );
    let findings = review(&failing);
    assert_eq!(summary(&findings), ["ERR:fail"]);
    let label = &findings.checks[0].label;
    assert_eq!(
        label,
        "Cannot describe cluster: aws_service_error (AccessDenied)"
    );
    assert!(!label.contains(SECRET));
    assert_eq!(findings.checks[0].pillar, "Other");

    let hostile_code = format!(
        r#"{{"calls": [{{"service": "docdb", "operation": "DescribeDBClusters",
           "error": {{"code": "{SECRET}", "message": "m"}}}}]}}"#
    );
    let findings = review(&hostile_code);
    assert_eq!(
        findings.checks[0].label,
        "Cannot describe cluster: aws_service_error"
    );
}

#[test]
fn an_unrecorded_call_is_a_warning_for_its_check_only() {
    let text = Cluster::default().recording();
    let without_alarms = text.replace("DescribeAlarms", "DescribeAlarmsX");
    let findings = review(&without_alarms);
    assert_eq!(find(&findings, "OPS5b")[0].status, Status::Warn);
    assert_eq!(
        find(&findings, "OPS5b")[0].label,
        "Cannot check alarms: aws_connect"
    );
    assert_eq!(find(&findings, "REL1")[0].status, Status::Pass);
}

#[test]
fn wrong_typed_response_members_fail_the_check_that_reads_them() {
    for (needle, replacement, id) in [
        (
            r#""BackupRetentionPeriod": 7"#,
            r#""BackupRetentionPeriod": "7""#,
            "ERR",
        ),
        (
            r#""DeletionProtection": true"#,
            r#""DeletionProtection": null"#,
            "ERR",
        ),
        (
            r#""EnabledCloudwatchLogsExports": ["audit","profiler"]"#,
            r#""EnabledCloudwatchLogsExports": null"#,
            "ERR",
        ),
    ] {
        let text = Cluster::default().recording().replace(needle, replacement);
        let findings = review(&text);
        assert_eq!(summary(&findings), [format!("{id}:fail")], "{needle}");
        assert!(findings.checks[0].label.contains("invalid response"));
        assert!(!findings.checks[0].label.contains(SECRET));
    }
    let text = Cluster::default()
        .recording()
        .replace(r#"{"Events": []}"#, r#"{"Events": null}"#);
    let findings = review(&text);
    assert_eq!(find(&findings, "REL7")[0].status, Status::Warn);
    assert!(find(&findings, "REL7")[0]
        .label
        .starts_with("Cannot check events: invalid response"));
    let text = Cluster::default()
        .recording()
        .replace(r#""MetricAlarms": [{},{},{}]"#, r#""MetricAlarms": null"#);
    assert_eq!(find(&review(&text), "OPS5b")[0].status, Status::Warn);
}

#[test]
fn failover_events_are_counted_across_pages_and_the_last_is_shown_clean() {
    let first = r#"{"Events": [{"Message": "Started failover", "EventCategories": ["failover"]}], "Marker": "page2"}"#;
    let second = format!(
        r#"{{"Events": [{{"Message": "Ordinary event"}}, {{"Message": "x\u001b[31mfail\nover {SECRET}", "EventCategories": ["Failover"]}}]}}"#
    );
    let text = Cluster::default().recording().replace(
        &call("docdb", "DescribeEvents", None, r#"{"Events": []}"#),
        &format!(
            "{},\n{}",
            call(
                "docdb",
                "DescribeEvents",
                Some(
                    r#"{"SourceIdentifier":"prod-docdb","SourceType":"db-cluster","StartTime":"2026-01-18T12:00:00Z","EndTime":"2026-01-31T12:00:00Z"}"#
                ),
                first
            ),
            call(
                "docdb",
                "DescribeEvents",
                Some(
                    r#"{"SourceIdentifier":"prod-docdb","SourceType":"db-cluster","StartTime":"2026-01-18T12:00:00Z","EndTime":"2026-01-31T12:00:00Z","Marker":"page2"}"#
                ),
                &second
            )
        ),
    );
    let findings = review(&text);
    let rel7 = &find(&findings, "REL7")[0];
    assert_eq!(rel7.status, Status::Warn);
    assert_eq!(rel7.label, "2 failover event(s) in last 13 days");
    assert!(
        rel7.detail
            .starts_with("Most recent: x\\x1b[31mfail\\x0aover "),
        "{}",
        rel7.detail
    );
    assert!(
        !rel7.detail.contains(SECRET),
        "credential-shaped text is redacted"
    );
    assert!(!rel7.detail.chars().any(char::is_control));

    let looping = Cluster::default().recording().replace(
        &call("docdb", "DescribeEvents", None, r#"{"Events": []}"#),
        &call(
            "docdb",
            "DescribeEvents",
            None,
            r#"{"Events": [], "Marker": "same"}"#,
        ),
    );
    let findings = review(&looping);
    assert_eq!(find(&findings, "REL7")[0].status, Status::Warn);
    assert_eq!(
        find(&findings, "REL7")[0].label,
        "Cannot check events: aws_pagination"
    );
}

#[test]
fn metric_thresholds_and_the_idle_reader() {
    let low_cpu = Cluster {
        cpu: (1.0, 3.0),
        ..Cluster::default()
    };
    assert_eq!(
        find(&review(&low_cpu.recording()), "COST1")[0].status,
        Status::Warn
    );

    let busy = Cluster {
        connections_max: 2_700.0,
        ..Cluster::default()
    };
    let findings = review(&busy.recording());
    assert_eq!(find(&findings, "PERF5")[0].status, Status::Warn);
    assert_eq!(
        find(&findings, "PERF5")[0].label,
        "Peak connections for i-writer (2700/3400 = 79%)"
    );

    let unknown_class = Cluster {
        instances: vec![
            ("i-x", "db.x9.large", "us-east-1a", true),
            ("i-y", "db.x9.large", "us-east-1b", false),
        ],
        ..Cluster::default()
    };
    let findings = review(&unknown_class.recording());
    assert_eq!(find(&findings, "PERF11").len(), 2);
    assert_eq!(
        find(&findings, "PERF11")[0].label,
        "Unknown instance type db.x9.large -- cannot check FreeableMemory"
    );
    assert_eq!(
        find(&findings, "PERF5")[0].label,
        "Connection limit unknown for i-x (db.x9.large)"
    );
    assert_eq!(find(&findings, "SUST1")[0].status, Status::Warn);

    let idle = Cluster::default().recording().replace(
        r#""Statistics":["Average"]}, "response": {"Datapoints": [{"Average": 100.0}]}"#,
        r#""Statistics":["Average"]}, "response": {"Datapoints": [{"Average": 1.0}]}"#,
    );
    let idle = idle.replace(r#"{"Average": 10.0}"#, r#"{"Average": 0.0}"#);
    let findings = review(&idle);
    let cost9 = &find(&findings, "COST9")[0];
    assert_eq!(cost9.status, Status::Warn);
    assert_eq!(
        cost9.label,
        "Reader i-reader appears idle (avg 0 conn, 1 ReadIOPS)"
    );
}

#[test]
fn p95_uses_the_hourly_maximum_with_the_index_of_the_script() {
    // 20 hourly maxima: int(20 * 0.95) = 19 is the largest value.
    let maxima: Vec<String> = (1..=20)
        .map(|n| format!(r#"{{"Average": 1.0, "Maximum": {n}.0}}"#))
        .collect();
    let text = Cluster::default().recording().replace(
        r#"{"Datapoints": [{"Average": 40.0, "Maximum": 60.0}]}"#,
        &format!(r#"{{"Datapoints": [{}]}}"#, maxima.join(",")),
    );
    let findings = review(&text);
    assert_eq!(
        find(&findings, "COST1")[0].label,
        "CPU for i-writer (avg 1.0%, P95 20.0%)"
    );
}

#[test]
fn the_instance_class_tables_match_the_script() {
    for (class, limit, ram) in [
        ("db.t3.medium", 1000, 4),
        ("db.t4g.medium", 1000, 4),
        ("db.r5.large", 3400, 16),
        ("db.r6g.large", 3400, 16),
        ("db.r6gd.xlarge", 7000, 32),
        ("db.r8g.2xlarge", 14200, 64),
        ("db.r5.4xlarge", 28400, 128),
        ("db.r6g.8xlarge", 60000, 256),
        ("db.r6g.12xlarge", 60000, 384),
        ("db.r8g.16xlarge", 60000, 512),
        ("db.r5.24xlarge", 60000, 768),
    ] {
        assert_eq!(tables::connection_limit(class), Some(limit), "{class}");
        assert_eq!(tables::ram_gib(class), Some(ram), "{class}");
    }
    for class in [
        "db.r6g.24xlarge",
        "db.r7g.large",
        "db.t3.large",
        "r5.large",
        "db.r5",
        "db.r5.large.x",
        "",
    ] {
        assert_eq!(tables::connection_limit(class), None, "{class}");
        assert_eq!(tables::ram_gib(class), None, "{class}");
    }
}

const ANALYSIS: &str = r#"{
  "app": {
    "orders": {
      "count": 250000, "size": 1000, "avgObjSize": 9000.5,
      "indexes": [
        {"name": "_id_", "size": 100, "ordered_fields": ["_id"]},
        {"name": "a_1", "size": 2000, "ordered_fields": ["a"], "usage": {"potential_unused": true}},
        {"name": "a_1_b_1", "size": 3000, "ordered_fields": ["a", "b"], "cardinality": {"is_low": true}},
        {"name": "ttl", "size": 10, "ordered_fields": ["t"], "expireAfterSeconds": 0}
      ],
      "compression": {"enabled": false},
      "unusedStorageSize": {"unusedBytes": 500, "unusedPercent": 45.4}
    },
    "bare": {"count": 200000, "size": 0, "avgObjSize": 10, "indexes": [{"name": "_id_", "size": 5, "ordered_fields": ["_id"]}], "compression": {"enabled": true}},
    "broken": {"error": "ignored"},
    "notadict": 5
  },
  "other": [1, 2],
  "empty": {}
}"#;

fn db_findings(text: &str) -> crate::docgraph::checks::Findings {
    let collections = read_analysis(&json(text))
        .expect("valid")
        .expect("some data");
    run_db_checks(&collections)
}

#[test]
fn database_checks_follow_the_script() {
    let findings = db_findings(ANALYSIS);
    let order: Vec<String> = summary(&findings);
    assert_eq!(
        order,
        [
            "PERF1:warn",
            "PERF1b:warn",
            "PERF1c:warn",
            "COST3:warn",
            "COST4:pass",
            "PERF8:warn",
            "PERF9:warn",
            "PERF10:pass",
            "SUST2:warn",
            "PERF15:warn",
            "PERF16:warn"
        ]
    );
    let get = |id: &str| find(&findings, id)[0];
    assert_eq!(
        get("PERF1").label,
        "1 collection(s) with avg doc size > 8 KB"
    );
    assert_eq!(get("PERF1").detail, "app.orders (9,000.5 bytes)");
    assert_eq!(
        get("PERF1b").label,
        "1 redundant index(es) (prefix subsets)"
    );
    assert_eq!(get("PERF1c").detail, "app.orders.a_1_b_1");
    assert_eq!(get("COST3").label, "1 unused index(es) of 5 total");
    assert_eq!(get("COST4").label, "TTL indexes on 1 collection(s)");
    assert_eq!(get("COST4").detail, "app.orders");
    assert_eq!(get("PERF8").label, "Index-to-data ratio: 512%");
    assert_eq!(get("PERF8").detail, "Indexes exceed 50% of data size");
    assert_eq!(get("PERF9").detail, "Run compact command. app.orders (45%)");
    assert_eq!(
        get("SUST2").label,
        "Compression disabled on 1 collection(s)"
    );
    assert_eq!(get("SUST2").detail, "app.orders");
    assert_eq!(
        get("PERF15").label,
        "1 large collection(s) with no secondary indexes"
    );
    assert_eq!(get("PERF15").detail, "app.bare (200,000 docs)");
    assert_eq!(get("PERF16").detail, "app.orders (index 5.1x data)");
}

#[test]
fn a_clean_analysis_passes_every_database_check_and_lists_are_capped_at_five() {
    let clean = r#"{"d": {"c": {"count": 5, "size": 100, "avgObjSize": 10,
        "indexes": [{"name": "_id_", "size": 10, "ordered_fields": ["_id"]}, {"name": "x", "size": 10, "ordered_fields": ["x"], "expireAfterSeconds": 60}],
        "compression": {"enabled": true}, "unusedStorageSize": {"unusedPercent": 1}}}}"#;
    let findings = db_findings(clean);
    assert!(
        findings.checks.iter().all(|c| c.status == Status::Pass),
        "{:?}",
        summary(&findings)
    );
    assert_eq!(
        find(&findings, "PERF8")[0].label,
        "Index-to-data ratio: 20%"
    );

    let many: Vec<String> = (1..=7)
        .map(|n| format!(r#""c{n}": {{"compression": {{"enabled": false}}}}"#))
        .collect();
    let findings = db_findings(&format!(r#"{{"d": {{{}}}}}"#, many.join(",")));
    let sust = &find(&findings, "SUST2")[0];
    assert_eq!(sust.label, "Compression disabled on 7 collection(s)");
    assert_eq!(sust.detail, "d.c1, d.c2, d.c3, d.c4, d.c5");
}

#[test]
fn an_analysis_member_of_the_wrong_type_is_rejected_without_naming_a_value() {
    for bad in [
        r#"{"d": {"c": {"count": "many"}}}"#,
        r#"{"d": {"c": {"indexes": null}}}"#,
        r#"{"d": {"c": {"indexes": [1]}}}"#,
        r#"{"d": {"c": {"indexes": [{"ordered_fields": "ab"}]}}}"#,
        r#"{"d": {"c": {"compression": []}}}"#,
        r#"{"d": {"c": {"unusedStorageSize": {"unusedPercent": "x"}}}}"#,
        r#"{"d": {"c": {"indexes": [{"name": 5}]}}}"#,
    ] {
        let reason = read_analysis(&json(bad)).expect_err(bad);
        assert!(
            reason.starts_with("analysis data, database 1, collection 1: "),
            "{reason}"
        );
        assert!(
            !reason.contains("many") && !reason.contains(" d ") && !reason.contains('c')
                || reason.contains("collection")
        );
    }
    assert!(read_analysis(&json("[]")).is_err());
    assert!(read_analysis(&json("5")).is_err());
    assert!(read_analysis(&json("{}"))
        .expect("empty is allowed")
        .is_none());
    let secret = format!(r#"{{"{SECRET}": {{"{SECRET}": {{"count": "{SECRET}"}}}}}}"#);
    let reason = read_analysis(&json(&secret)).expect_err("wrong type");
    assert!(!reason.contains(SECRET), "{reason}");
}

#[test]
fn recordings_with_wrong_shapes_are_rejected_without_echoing_values() {
    for (text, expected) in [
        (r#"{}"#, "need a \"calls\""),
        (r#"{"calls": null}"#, "must be an array"),
        (
            r#"{"calls": [{"service": "s3", "operation": "X", "response": {}}]}"#,
            "recorded call 0: \"service\" must be",
        ),
        (
            r#"{"calls": [{"service": "ec2", "operation": "", "response": {}}]}"#,
            "\"operation\" must be a non-empty string",
        ),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "request": null, "response": {}}]}"#,
            "\"request\" must be an object when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "response": {}, "error": {}}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "response": []}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "error": {"message": 5}}]}"#,
            "error \"message\" must be a string when present (it is integer)",
        ),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "error": {"kind": "weird"}}]}"#,
            "\"error.kind\" must be",
        ),
    ] {
        let reason = RecordedClient::from_document(&json(text))
            .err()
            .expect(text);
        assert!(reason.contains(expected), "{reason} / {expected}");
    }
    let hostile = format!(
        r#"{{"calls": [{{"service": "{SECRET}", "operation": "{SECRET}", "request": "{SECRET}", "response": {{}}}}]}}"#
    );
    let reason = RecordedClient::from_document(&json(&hostile))
        .err()
        .expect("rejected");
    assert!(!reason.contains(SECRET), "{reason}");
}

#[test]
fn the_report_matches_the_script_layout() {
    let findings = review(&Cluster::default().recording());
    let mut checks = findings.checks;
    checks.push(crate::docgraph::checks::Check {
        pillar: "Other",
        id: "ERR",
        label: "Cannot describe instances: aws_connect".to_string(),
        status: Status::Fail,
        detail: String::new(),
    });
    let text = report::results_json(&checks[..2], "prod-docdb", "2026-01-31T12:00:00Z");
    assert_eq!(
        text,
        r#"{
  "cluster": "prod-docdb",
  "timestamp": "2026-01-31T12:00:00Z",
  "checks": [
    {
      "pillar": "Reliability",
      "id": "REL1",
      "label": "Backup retention period (7 days)",
      "status": "pass",
      "detail": ""
    },
    {
      "pillar": "Reliability",
      "id": "REL2",
      "label": "Deletion protection (enabled)",
      "status": "pass",
      "detail": ""
    }
  ]
}"#
    );
    let md = report::markdown(&checks, "prod-docdb", "2026-01-31 12:00");
    assert!(md.starts_with(
        "# Well-Architected Review: prod-docdb\n\n**Date:** 2026-01-31 12:00 UTC\n\n**Summary:** "
    ));
    assert!(md.contains("\n## Reliability\n\n| Status | Check | Detail |\n|--------|-------|--------|\n| [PASS] | Backup retention period (7 days) |  |\n"));
    assert!(md.contains("| [INFO] | Maintenance window: sun:05:00-sun:06:00 | Verify this window aligns with your lowest-traffic period |"));
    assert!(
        !md.contains("Other"),
        "the Other pillar is counted but not listed"
    );
    assert!(md.ends_with("|\n"));
    let [pass, warn, fail, info] = report::counts(&checks);
    assert_eq!((warn, fail, info), (0, 1, 2));
    assert!(pass > 30);
    let piped = crate::docgraph::checks::Check {
        pillar: "Security",
        id: "SEC5",
        label: "a|b".to_string(),
        status: Status::Warn,
        detail: "c|d".to_string(),
    };
    assert!(report::markdown(&[piped], "c", "d").contains("| [WARN] | a/b | c/d |"));
}

#[test]
fn the_command_writes_both_files_and_prints_the_script_summary() {
    let dir = TempDir::new("review");
    let recorded = dir.write("recorded.json", &Cluster::default().recording());
    let analysis = dir.write("analysis.json", ANALYSIS);
    let out = dir.join("out");
    let out_text = out.to_string_lossy().into_owned();
    let ran = run(&[
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod-docdb",
        "--region",
        "us-east-1",
        "--recorded",
        &recorded,
        "--analysis-data",
        &analysis,
        "--output",
        &out_text,
        "--now",
        NOW,
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert_eq!(
        lines[0],
        "Running Well-Architected Review for prod-docdb in us-east-1..."
    );
    assert_eq!(lines[1], "  Running infrastructure checks (AWS APIs)...");
    assert_eq!(lines[2], "  Infrastructure: 39 checks completed");
    assert!(lines[3].starts_with("  Loading database stats from "));
    assert_eq!(lines[4], "  Running database-level checks...");
    assert_eq!(lines[5], "  Database: 11 checks completed");
    assert_eq!(lines[6], "");
    assert_eq!(
        lines[7],
        format!("  Results: {out_text}/wa_review_results.json")
    );
    assert_eq!(
        lines[8],
        format!("  Report:  {out_text}/wa_review_report.md")
    );
    assert_eq!(lines[9], "");
    assert!(
        lines[10].starts_with("  Total: 50 checks -- "),
        "{}",
        lines[10]
    );
    let results = std::fs::read_to_string(out.join("wa_review_results.json")).expect("results");
    assert!(results.starts_with("{\n  \"cluster\": \"prod-docdb\",\n  \"timestamp\": \"2026-01-31T12:00:00Z\",\n  \"checks\": [\n    {\n"));
    let md = std::fs::read_to_string(out.join("wa_review_report.md")).expect("report");
    assert!(md.contains("**Date:** 2026-01-31 12:00 UTC"));
    assert!(md.contains("## Sustainability"));
}

#[test]
fn without_a_recording_the_run_fails_before_writing_the_report() {
    let dir = TempDir::new("nolive");
    let out_text = dir.join("out").to_string_lossy().into_owned();
    let ran = run(&[
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod",
        "--region",
        "us-east-1",
        "--output",
        &out_text,
        "--now",
        NOW,
    ]);
    assert_eq!(ran.status, 1, "{}", ran.stderr);
    assert!(ran.stdout.is_empty());
    assert!(ran.stderr.contains(if cfg!(feature = "docgraph-live") {
        "live data requires explicit --live"
    } else {
        "built without docgraph-live"
    }));
    assert!(!dir.join("out").exists());
}

#[test]
fn the_command_rejects_bad_arguments_before_any_call() {
    for words in [
        vec!["documentdb", "wa-review"],
        vec!["documentdb", "wa-review", "--cluster-id", "a"],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "1bad",
            "--region",
            "us-east-1",
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "a\nb",
            "--region",
            "us-east-1",
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "a",
            "--region",
            "nowhere",
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "a",
            "--region",
            "us-east-1",
            "--now",
            "yesterday",
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "a",
            "--region",
            "us-east-1",
            "--uri",
            "mongodb://u:p@h",
        ],
    ] {
        let ran = run(&words);
        assert_eq!(ran.status, 2, "{words:?}: {}", ran.stderr);
        assert!(ran.stdout.is_empty(), "{words:?}");
        assert!(
            !ran.stderr.contains("mongodb://"),
            "no uri option exists, its value is never echoed"
        );
    }
}

#[test]
fn a_bad_recording_or_analysis_file_is_one_line_and_names_no_value() {
    let dir = TempDir::new("badfiles");
    let bad = dir.write(
        "bad.json",
        &format!(r#"{{"calls": [{{"service": "{SECRET}"}}]}}"#),
    );
    let ran = run(&[
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod",
        "--region",
        "us-east-1",
        "--recorded",
        &bad,
        "--now",
        NOW,
    ]);
    assert_eq!(ran.status, 1);
    assert!(!ran.stderr.contains(SECRET) && !ran.stdout.contains(SECRET));
    let analysis = dir.write(
        "a.json",
        &format!(r#"{{"d": {{"c": {{"count": "{SECRET}"}}}}}}"#),
    );
    let recorded = dir.write("r.json", &Cluster::default().recording());
    let ran = run(&[
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod-docdb",
        "--region",
        "us-east-1",
        "--recorded",
        &recorded,
        "--analysis-data",
        &analysis,
        "--output",
        &dir.join("o").to_string_lossy(),
        "--now",
        NOW,
    ]);
    assert_eq!(ran.status, 1);
    assert!(ran.stderr.contains("analysis data, database 1, collection 1: count must be a number when present (it is string)"), "{}", ran.stderr);
    assert!(!ran.stderr.contains(SECRET) && !ran.stdout.contains(SECRET));
}

// ------------------------------------------------- #5344 private report files

const RESULTS_NAME: &str = "wa_review_results.json";
const REPORT_NAME: &str = "wa_review_report.md";

/// `wa-review` arguments with `--analysis-data` (R-DG-08, #5344).
fn private_review_args<'a>(recorded: &'a str, analysis: &'a str, out: &'a str) -> Vec<&'a str> {
    vec![
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod-docdb",
        "--region",
        "us-east-1",
        "--recorded",
        recorded,
        "--analysis-data",
        analysis,
        "--output",
        out,
        "--now",
        NOW,
    ]
}

#[cfg(unix)]
#[test]
fn r_dg_08_analysis_data_refuses_an_existing_report_and_leaves_it_unchanged() {
    let dir = TempDir::new("private-exists");
    let recorded = dir.write("recorded.json", &Cluster::default().recording());
    let analysis = dir.write("analysis.json", ANALYSIS);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    for (name, other) in [(REPORT_NAME, RESULTS_NAME), (RESULTS_NAME, REPORT_NAME)] {
        let existing = out.join(name);
        std::fs::write(&existing, "keep me\n").expect("existing file");
        let out_text = out.to_string_lossy().into_owned();
        let ran = run(&private_review_args(&recorded, &analysis, &out_text));
        assert_eq!(ran.status, 1, "{name}: {}", ran.stdout);
        assert!(ran.stderr.contains(name), "{name}: {}", ran.stderr);
        assert_eq!(
            std::fs::read_to_string(&existing).expect("existing file"),
            "keep me\n",
            "{name} was modified"
        );
        std::fs::remove_file(&existing).expect("cleanup");
        let _ = std::fs::remove_file(out.join(other));
    }
}

#[cfg(unix)]
#[test]
fn r_dg_08_analysis_data_refuses_a_symlink_report_and_leaves_its_target_untouched() {
    let dir = TempDir::new("private-symlink");
    let recorded = dir.write("recorded.json", &Cluster::default().recording());
    let analysis = dir.write("analysis.json", ANALYSIS);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    let target = dir.join("target.txt");
    std::fs::write(&target, "target\n").expect("target");
    let dangling = dir.join("absent-target.txt");
    for (name, pointee) in [(RESULTS_NAME, &target), (REPORT_NAME, &dangling)] {
        let link = out.join(name);
        std::os::unix::fs::symlink(pointee, &link).expect("symlink");
        let out_text = out.to_string_lossy().into_owned();
        let ran = run(&private_review_args(&recorded, &analysis, &out_text));
        assert_eq!(ran.status, 1, "{name}: {}", ran.stdout);
        assert!(ran.stderr.contains(name), "{name}: {}", ran.stderr);
        assert_eq!(
            std::fs::read_to_string(&target).expect("target"),
            "target\n"
        );
        assert!(!dangling.exists(), "{name}: the link was followed");
        assert!(std::fs::symlink_metadata(&link)
            .expect("link")
            .file_type()
            .is_symlink());
        std::fs::remove_file(&link).expect("cleanup");
        let _ = std::fs::remove_file(out.join(RESULTS_NAME));
        let _ = std::fs::remove_file(out.join(REPORT_NAME));
    }
}

#[test]
fn r_dg_08_without_analysis_data_an_existing_report_is_still_overwritten() {
    let dir = TempDir::new("private-absent");
    let recorded = dir.write("recorded.json", &Cluster::default().recording());
    let out = dir.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    std::fs::write(out.join(REPORT_NAME), "old\n").expect("existing file");
    let out_text = out.to_string_lossy().into_owned();
    let ran = run(&[
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod-docdb",
        "--region",
        "us-east-1",
        "--recorded",
        &recorded,
        "--output",
        &out_text,
        "--now",
        NOW,
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let md = std::fs::read_to_string(out.join(REPORT_NAME)).expect("report");
    assert!(md.contains("## Sustainability"), "{md}");
}
