//! `toolkit elasticache dashboard` (port of `generate_dashboards.py`): emits a
//! CloudFormation template with a CloudWatch dashboard and alarms for a
//! serverless cache or a node-based replication group. Nothing here calls AWS.
//!
//! Thresholds keep the Python `int` / `float` distinction because it shows in
//! the template (`80` for a default, `80.0` for `--hit-rate-threshold 80`).

use toolkit_core::{Json, ToolkitError};

use crate::aurora::pyfmt::Num;

use super::text::{dumps_inline, js, kv, set_member, trunc_int};

/// Deployment type of the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheType {
    /// A serverless cache (`ServerlessCacheName` dimension).
    Serverless,
    /// A node-based replication group.
    NodeBased,
}

/// Everything [`generate_template`] needs. A threshold of `None` disables its
/// alarm.
#[derive(Debug, Clone)]
pub(crate) struct TemplateOptions {
    pub cache_type: CacheType,
    pub identifier: String,
    pub region: String,
    pub sns_topic: Option<String>,
    pub include_dashboard: bool,
    pub include_alarms: bool,
    pub max_storage_gb: Option<f64>,
    pub storage_alarm_pct: Num,
    pub memory_threshold: Option<Num>,
    pub engine_cpu_threshold: Option<Num>,
    pub replication_lag_threshold: Option<Num>,
    pub hit_rate_threshold: Option<Num>,
    pub throttle_threshold: Option<Num>,
    pub read_latency_threshold: Option<Num>,
    pub write_latency_threshold: Option<Num>,
    pub ecpu_threshold: Option<Num>,
    pub evictions_threshold: Option<Num>,
    pub new_connections_threshold: Option<Num>,
    pub node_ids: Option<Vec<String>>,
}

fn strings(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|value| js(value)).collect())
}

fn metric(name: &str, dim_name: &str, dim_value: &str) -> Json {
    strings(&["AWS/ElastiCache", name, dim_name, dim_value])
}

fn widget(title: &str, region: &str, metrics: Vec<Json>, period: i64, stat: &str) -> Json {
    Json::Object(vec![
        kv("type", js("metric")),
        kv(
            "properties",
            Json::Object(vec![
                kv("title", js(title)),
                kv("region", js(region)),
                kv("metrics", Json::Array(metrics)),
                kv("period", Json::Int(period)),
                kv("stat", js(stat)),
                kv("view", js("timeSeries")),
            ]),
        ),
        kv("width", Json::Int(12)),
        kv("height", Json::Int(6)),
    ])
}

fn serverless_widget(
    title: &str,
    region: &str,
    cache_name: &str,
    names: &[&str],
    period: i64,
    stat: &str,
) -> Json {
    let metrics = names
        .iter()
        .map(|name| metric(name, "ServerlessCacheName", cache_name))
        .collect();
    widget(title, region, metrics, period, stat)
}

/// CloudWatch dashboard body for a serverless cache.
#[must_use]
pub fn serverless_dashboard_body(cache_name: &str, region: &str) -> Json {
    let w = |title: &str, names: &[&str], period: i64, stat: &str| {
        serverless_widget(title, region, cache_name, names, period, stat)
    };
    Json::Object(vec![kv(
        "widgets",
        Json::Array(vec![
            w("Cache Hit Rate", &["CacheHitRate"], 300, "Average"),
            w(
                "ECPU Consumption",
                &["ElastiCacheProcessingUnits"],
                60,
                "Average",
            ),
            w("Throttled Commands", &["ThrottledCmds"], 60, "Sum"),
            w(
                "Read / Write Latency",
                &[
                    "SuccessfulReadRequestLatency",
                    "SuccessfulWriteRequestLatency",
                ],
                60,
                "Average",
            ),
            w("Current Connections", &["CurrConnections"], 300, "Average"),
            w("New Connections", &["NewConnections"], 300, "Sum"),
            w(
                "Bytes Used For Cache",
                &["BytesUsedForCache"],
                300,
                "Average",
            ),
            w(
                "Command-Family Breakdown",
                &[
                    "StringBasedCmds",
                    "HashBasedCmds",
                    "SortedSetBasedCmds",
                    "StreamBasedCmds",
                    "PubSubBasedCmds",
                ],
                300,
                "Sum",
            ),
            w("Total Commands Count", &["TotalCmdsCount"], 300, "Sum"),
            w(
                "Item Counts",
                &["CurrVolatileItems", "CurrItems"],
                300,
                "Average",
            ),
        ]),
    )])
}

/// CloudWatch dashboard body for a node-based replication group. With
/// `node_ids` the metrics are per node (`CacheClusterId`); without, they fall
/// back to `ReplicationGroupId`, which only exists for a subset of metrics.
#[must_use]
pub fn node_based_dashboard_body(rg_id: &str, region: &str, node_ids: Option<&[String]>) -> Json {
    let widgets = match node_ids.filter(|ids| !ids.is_empty()) {
        Some(ids) => {
            let w = |title: &str, names: &[&str], period: i64, stat: &str| {
                let metrics = names
                    .iter()
                    .flat_map(|name| ids.iter().map(move |id| metric(name, "CacheClusterId", id)))
                    .collect();
                widget(title, region, metrics, period, stat)
            };
            vec![
                w("Cache Hit Rate", &["CacheHitRate"], 300, "Average"),
                w(
                    "Engine CPU Utilization",
                    &["EngineCPUUtilization", "CPUUtilization"],
                    60,
                    "Average",
                ),
                w(
                    "Database Memory Usage",
                    &["DatabaseMemoryUsagePercentage"],
                    300,
                    "Average",
                ),
                w(
                    "Read / Write Latency",
                    &[
                        "SuccessfulReadRequestLatency",
                        "SuccessfulWriteRequestLatency",
                    ],
                    60,
                    "Average",
                ),
                w("Current Connections", &["CurrConnections"], 300, "Average"),
                w("Replication Lag", &["ReplicationLag"], 60, "Average"),
                w(
                    "Network Bytes In/Out",
                    &["NetworkBytesIn", "NetworkBytesOut"],
                    300,
                    "Sum",
                ),
                w("Evictions", &["Evictions"], 300, "Sum"),
                w(
                    "Command-Family Breakdown",
                    &[
                        "StringBasedCmds",
                        "HashBasedCmds",
                        "SortedSetBasedCmds",
                        "StreamBasedCmds",
                        "SearchBasedCmds",
                    ],
                    300,
                    "Sum",
                ),
            ]
        }
        None => {
            let w = |title: &str, names: &[&str], period: i64, stat: &str| {
                let metrics = names
                    .iter()
                    .map(|name| metric(name, "ReplicationGroupId", rg_id))
                    .collect();
                widget(title, region, metrics, period, stat)
            };
            vec![
                w("Cache Hit Rate", &["CacheHitRate"], 300, "Average"),
                w(
                    "Engine CPU Utilization",
                    &["EngineCPUUtilization", "CPUUtilization"],
                    60,
                    "Average",
                ),
                w(
                    "Database Memory Usage",
                    &["DatabaseMemoryUsagePercentage"],
                    300,
                    "Average",
                ),
                w(
                    "Read / Write Latency",
                    &[
                        "SuccessfulReadRequestLatency",
                        "SuccessfulWriteRequestLatency",
                    ],
                    60,
                    "Average",
                ),
                w("Current Connections", &["CurrConnections"], 300, "Average"),
                w("Replication Lag", &["ReplicationLag"], 60, "Average"),
                w("Evictions", &["Evictions"], 300, "Sum"),
            ]
        }
    };
    Json::Object(vec![kv("widgets", Json::Array(widgets))])
}

/// One CloudWatch alarm resource.
struct AlarmSpec<'a> {
    name: String,
    description: String,
    metric: &'a str,
    dimension: (&'a str, &'a str),
    threshold: Num,
    comparison: &'a str,
    period: i64,
    eval_periods: i64,
    datapoints: i64,
    stat: &'a str,
}

fn alarm(spec: &AlarmSpec<'_>, sns_topic: Option<&str>) -> Json {
    let mut properties = vec![
        kv("AlarmName", js(&spec.name)),
        kv("AlarmDescription", js(&spec.description)),
        kv("Namespace", js("AWS/ElastiCache")),
        kv("MetricName", js(spec.metric)),
        kv(
            "Dimensions",
            Json::Array(vec![Json::Object(vec![
                kv("Name", js(spec.dimension.0)),
                kv("Value", js(spec.dimension.1)),
            ])]),
        ),
        kv("Threshold", spec.threshold.to_json()),
        kv("ComparisonOperator", js(spec.comparison)),
        kv("Period", Json::Int(spec.period)),
        kv("EvaluationPeriods", Json::Int(spec.eval_periods)),
        kv("DatapointsToAlarm", Json::Int(spec.datapoints)),
        kv("Statistic", js(spec.stat)),
        kv("TreatMissingData", js("notBreaching")),
    ];
    if sns_topic.is_some_and(|topic| !topic.is_empty()) {
        let reference = Json::Array(vec![Json::Object(vec![kv("Ref", js("SNSTopicARN"))])]);
        properties.push(kv("AlarmActions", reference.clone()));
        properties.push(kv("OKActions", reference));
    }
    Json::Object(vec![
        kv("Type", js("AWS::CloudWatch::Alarm")),
        kv("Properties", Json::Object(properties)),
    ])
}

/// `re.sub(r"[^a-zA-Z0-9]", "", text)`.
fn alphanumeric(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).collect()
}

fn serverless_alarms(options: &TemplateOptions) -> Result<Vec<(String, Json)>, ToolkitError> {
    let name = options.identifier.as_str();
    let sns = options.sns_topic.as_deref();
    let dim = ("ServerlessCacheName", name);
    let mut alarms: Vec<(String, Json)> = Vec::new();
    let mut add = |key: &str, spec: AlarmSpec<'_>| {
        set_member(&mut alarms, key, alarm(&spec, sns));
    };

    if let Some(threshold) = options.hit_rate_threshold {
        add(
            "CacheHitRateAlarm",
            AlarmSpec {
                name: format!("{name}-low-hit-rate"),
                description: format!("Cache hit rate below {}% for {name}", threshold.py_str()),
                metric: "CacheHitRate",
                dimension: dim,
                threshold,
                comparison: "LessThanThreshold",
                period: 300,
                eval_periods: 6,
                datapoints: 4,
                stat: "Average",
            },
        );
    }
    if let Some(threshold) = options.throttle_threshold {
        add(
            "ThrottledCmdsAlarm",
            AlarmSpec {
                name: format!("{name}-throttled-cmds"),
                description: format!("Throttled commands above {} on {name}", threshold.py_str()),
                metric: "ThrottledCmds",
                dimension: dim,
                threshold,
                comparison: "GreaterThanThreshold",
                period: 60,
                eval_periods: 3,
                datapoints: 2,
                stat: "Sum",
            },
        );
    }
    if let Some(max_storage_gb) = options.max_storage_gb {
        let bytes = trunc_int(
            max_storage_gb * (options.storage_alarm_pct.value() / 100.0) * 1_073_741_824.0,
        )
        .map_err(|error| ToolkitError::runtime("threshold_invalid", error.message()))?;
        add(
            "StorageLimitAlarm",
            AlarmSpec {
                name: format!("{name}-storage-approaching-limit"),
                description: format!(
                    "Storage approaching {}% of configured limit ({} GB) on {name}",
                    options.storage_alarm_pct.py_str(),
                    Num::Float(max_storage_gb).py_str()
                ),
                metric: "BytesUsedForCache",
                dimension: dim,
                threshold: Num::Int(bytes),
                comparison: "GreaterThanThreshold",
                period: 300,
                eval_periods: 3,
                datapoints: 2,
                stat: "Maximum",
            },
        );
    }
    if let Some(threshold) = options.read_latency_threshold {
        add(
            "ReadLatencyAlarm",
            AlarmSpec {
                name: format!("{name}-high-read-latency"),
                description: format!("Read latency above {}us on {name}", threshold.py_str()),
                metric: "SuccessfulReadRequestLatency",
                dimension: dim,
                threshold,
                comparison: "GreaterThanThreshold",
                period: 60,
                eval_periods: 5,
                datapoints: 3,
                stat: "Average",
            },
        );
    }
    if let Some(threshold) = options.write_latency_threshold {
        add(
            "WriteLatencyAlarm",
            AlarmSpec {
                name: format!("{name}-high-write-latency"),
                description: format!("Write latency above {}us on {name}", threshold.py_str()),
                metric: "SuccessfulWriteRequestLatency",
                dimension: dim,
                threshold,
                comparison: "GreaterThanThreshold",
                period: 60,
                eval_periods: 5,
                datapoints: 3,
                stat: "Average",
            },
        );
    }
    if let Some(threshold) = options.ecpu_threshold {
        add(
            "ECPUSpikeAlarm",
            AlarmSpec {
                name: format!("{name}-ecpu-spike"),
                description: format!("ECPU consumption above {} on {name}", threshold.py_str()),
                metric: "ElastiCacheProcessingUnits",
                dimension: dim,
                threshold,
                comparison: "GreaterThanThreshold",
                period: 300,
                eval_periods: 3,
                datapoints: 2,
                stat: "Sum",
            },
        );
    }
    if let Some(threshold) = options.evictions_threshold {
        add(
            "EvictionsAlarm",
            AlarmSpec {
                name: format!("{name}-evictions"),
                description: format!("Evictions above {} on {name}", threshold.py_str()),
                metric: "Evictions",
                dimension: dim,
                threshold,
                comparison: "GreaterThanThreshold",
                period: 300,
                eval_periods: 3,
                datapoints: 2,
                stat: "Sum",
            },
        );
    }
    Ok(alarms)
}

fn node_based_alarms(options: &TemplateOptions) -> Vec<(String, Json)> {
    let sns = options.sns_topic.as_deref();
    let targets: Vec<(&str, &str)> = match options.node_ids.as_deref().filter(|ids| !ids.is_empty())
    {
        Some(ids) => ids
            .iter()
            .map(|id| ("CacheClusterId", id.as_str()))
            .collect(),
        None => vec![("ReplicationGroupId", options.identifier.as_str())],
    };
    let mut alarms: Vec<(String, Json)> = Vec::new();

    for (dim_name, dim_value) in targets {
        let dim = (dim_name, dim_value);
        let safe = alphanumeric(dim_value);
        let mut add = |prefix: &str, spec: AlarmSpec<'_>| {
            set_member(&mut alarms, &format!("{prefix}{safe}"), alarm(&spec, sns));
        };
        if let Some(threshold) = options.hit_rate_threshold {
            add(
                "CacheHitRateAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-low-hit-rate"),
                    description: format!(
                        "Cache hit rate below {}% on {dim_value}",
                        threshold.py_str()
                    ),
                    metric: "CacheHitRate",
                    dimension: dim,
                    threshold,
                    comparison: "LessThanThreshold",
                    period: 300,
                    eval_periods: 6,
                    datapoints: 4,
                    stat: "Average",
                },
            );
        }
        if let Some(threshold) = options.engine_cpu_threshold {
            add(
                "EngineCPUAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-high-engine-cpu"),
                    description: format!("Engine CPU above {}% on {dim_value}", threshold.py_str()),
                    metric: "EngineCPUUtilization",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 60,
                    eval_periods: 5,
                    datapoints: 3,
                    stat: "Maximum",
                },
            );
        }
        if let Some(threshold) = options.memory_threshold {
            add(
                "MemoryAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-high-memory"),
                    description: format!(
                        "Memory usage above {}% on {dim_value}",
                        threshold.py_str()
                    ),
                    metric: "DatabaseMemoryUsagePercentage",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 60,
                    eval_periods: 5,
                    datapoints: 3,
                    stat: "Maximum",
                },
            );
        }
        if let Some(threshold) = options.replication_lag_threshold {
            add(
                "ReplicationLagAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-high-replication-lag"),
                    description: format!(
                        "Replication lag above {}s on {dim_value}",
                        threshold.py_str()
                    ),
                    metric: "ReplicationLag",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 60,
                    eval_periods: 5,
                    datapoints: 3,
                    stat: "Maximum",
                },
            );
        }
        if let Some(threshold) = options.read_latency_threshold {
            add(
                "ReadLatencyAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-high-read-latency"),
                    description: format!(
                        "Read latency above {}us on {dim_value}",
                        threshold.py_str()
                    ),
                    metric: "SuccessfulReadRequestLatency",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 60,
                    eval_periods: 5,
                    datapoints: 3,
                    stat: "Average",
                },
            );
        }
        if let Some(threshold) = options.write_latency_threshold {
            add(
                "WriteLatencyAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-high-write-latency"),
                    description: format!(
                        "Write latency above {}us on {dim_value}",
                        threshold.py_str()
                    ),
                    metric: "SuccessfulWriteRequestLatency",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 60,
                    eval_periods: 5,
                    datapoints: 3,
                    stat: "Average",
                },
            );
        }
        if let Some(threshold) = options.evictions_threshold {
            add(
                "EvictionsAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-evictions"),
                    description: format!("Evictions above {} on {dim_value}", threshold.py_str()),
                    metric: "Evictions",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 300,
                    eval_periods: 3,
                    datapoints: 2,
                    stat: "Sum",
                },
            );
        }
        if let Some(threshold) = options.new_connections_threshold {
            add(
                "NewConnectionsAlarm",
                AlarmSpec {
                    name: format!("{dim_value}-new-connections"),
                    description: format!(
                        "New connections above {}/min on {dim_value}",
                        threshold.py_str()
                    ),
                    metric: "NewConnections",
                    dimension: dim,
                    threshold,
                    comparison: "GreaterThanThreshold",
                    period: 60,
                    eval_periods: 3,
                    datapoints: 2,
                    stat: "Sum",
                },
            );
        }
    }
    alarms
}

/// Builds the complete CloudFormation template.
///
/// # Errors
///
/// A storage alarm threshold that is not a finite integer.
pub(crate) fn generate_template(options: &TemplateOptions) -> Result<Json, ToolkitError> {
    let identifier = options.identifier.as_str();
    let mut resources: Vec<(String, Json)> = Vec::new();

    if options.include_dashboard {
        let body = match options.cache_type {
            CacheType::Serverless => serverless_dashboard_body(identifier, &options.region),
            CacheType::NodeBased => {
                node_based_dashboard_body(identifier, &options.region, options.node_ids.as_deref())
            }
        };
        resources.push(kv(
            &format!("{}Dashboard", alphanumeric(identifier)),
            Json::Object(vec![
                kv("Type", js("AWS::CloudWatch::Dashboard")),
                kv(
                    "Properties",
                    Json::Object(vec![
                        kv("DashboardName", js(&format!("ElastiCache-{identifier}"))),
                        kv("DashboardBody", js(&dumps_inline(&body))),
                    ]),
                ),
            ]),
        ));
    }

    if options.include_alarms {
        let alarms = match options.cache_type {
            CacheType::Serverless => serverless_alarms(options)?,
            CacheType::NodeBased => node_based_alarms(options),
        };
        for (key, value) in alarms {
            set_member(&mut resources, &key, value);
        }
    }

    let mut template = vec![
        kv("AWSTemplateFormatVersion", js("2010-09-09")),
        kv(
            "Description",
            js(&format!("ElastiCache observability stack for {identifier}")),
        ),
        kv("Resources", Json::Object(resources)),
    ];
    if let Some(topic) = options
        .sns_topic
        .as_deref()
        .filter(|topic| !topic.is_empty())
    {
        template.push(kv(
            "Parameters",
            Json::Object(vec![kv(
                "SNSTopicARN",
                Json::Object(vec![
                    kv("Type", js("String")),
                    kv("Default", js(topic)),
                    kv("Description", js("SNS topic ARN for alarm notifications")),
                ]),
            )]),
        ));
    }
    Ok(Json::Object(template))
}
