//! `collect_diagnostics.py`: endpoint status, `CloudWatch` metrics and recent
//! container logs of one `SageMaker` endpoint.
//!
//! Same requests, same warnings and the same result object as the Python
//! function. A service error of one step is recorded in `errors` and the next
//! step still runs; any other failure (no credentials, no connection, a
//! malformed response) stops the run, where Python raised.

use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::client::{Operation, SageMakerClient};
use super::hub::{validate_endpoint_name, ModelsError};
use super::timeutil::{iso_utc, timestamp_text};
use crate::cloudwatch::timefmt::UtcTime;
use crate::dynamodb::pyfmt;

/// Length of the log window, in minutes.
const LOG_WINDOW_MINUTES: i64 = 15;

/// Namespace of the invocation metrics.
const SAGEMAKER: &str = "AWS/SageMaker";

/// Namespace of the instance-level metrics.
const INSTANCE: &str = "/aws/sagemaker/Endpoints";

/// Logs returned at most (one page, no pagination).
const LOG_EVENT_LIMIT: i64 = 100;

const MICROS_PER_MINUTE: i64 = 60_000_000;

/// The window metrics and logs are read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeRange {
    /// The last `minutes` minutes before now.
    Last(i64),
    /// An absolute window; `end` defaults to now.
    Between {
        start: UtcTime,
        end: Option<UtcTime>,
    },
}

impl TimeRange {
    fn resolve(self, now: UtcTime) -> Result<(UtcTime, UtcTime), String> {
        match self {
            Self::Between { start, end } => Ok((start, end.unwrap_or(now))),
            Self::Last(minutes) => {
                let start = minutes
                    .checked_mul(MICROS_PER_MINUTE)
                    .and_then(|micros| now.checked_add_micros(-micros))
                    .ok_or_else(|| "the time window is out of range".to_string())?;
                Ok((start, now))
            }
        }
    }
}

fn metric_query(id: &str, namespace: &str, metric: &str, dimensions: &Json, stat: &str) -> Json {
    Json::object(vec![
        ("Id", Json::string(id)),
        (
            "MetricStat",
            Json::object(vec![
                (
                    "Metric",
                    Json::object(vec![
                        ("Namespace", Json::string(namespace)),
                        ("MetricName", Json::string(metric)),
                        ("Dimensions", dimensions.clone()),
                    ]),
                ),
                ("Period", Json::Int(60)),
                ("Stat", Json::string(stat)),
            ]),
        ),
    ])
}

fn dimensions(endpoint: &str, variant: &str) -> Json {
    Json::Array(vec![
        Json::object(vec![
            ("Name", Json::string("EndpointName")),
            ("Value", Json::string(endpoint)),
        ]),
        Json::object(vec![
            ("Name", Json::string("VariantName")),
            ("Value", Json::string(variant)),
        ]),
    ])
}

/// A service error becomes a recorded line; every other failure stops the run.
fn recorded(error: AwsError, step: &str, errors: &mut Vec<Json>) -> Result<(), ModelsError> {
    match error {
        AwsError::Service(service) => {
            errors.push(Json::String(format!(
                "{step} failed: {}",
                service.message()
            )));
            Ok(())
        }
        other => Err(ModelsError::Aws(other)),
    }
}

fn describe_endpoint(
    client: &mut dyn SageMakerClient,
    endpoint: &str,
    errors: &mut Vec<Json>,
) -> Result<(Json, String, Vec<Json>), ModelsError> {
    let request = Json::object(vec![("EndpointName", Json::string(endpoint))]);
    let response = match client.call(Operation::DescribeEndpoint, &request) {
        Ok(response) => response,
        Err(error) => {
            recorded(error, "DescribeEndpoint", errors)?;
            return Ok((Json::Object(Vec::new()), String::new(), Vec::new()));
        }
    };
    let get = |key: &str| response.get(key).cloned().unwrap_or(Json::Null);
    let raw_variants = match response.get("ProductionVariants") {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::Array(items)) if items.iter().all(|item| matches!(item, Json::Object(_))) => {
            items.clone()
        }
        Some(_) => {
            return Err(ModelsError::Data(
                "ProductionVariants must be a list of objects".to_string(),
            ))
        }
    };
    let variants: Vec<Json> = raw_variants
        .iter()
        .map(|variant| {
            let member = |key: &str| variant.get(key).cloned().unwrap_or(Json::Null);
            Json::object(vec![
                ("name", member("VariantName")),
                ("instance_type", member("InstanceType")),
                ("current_instance_count", member("CurrentInstanceCount")),
                ("current_weight", member("CurrentWeight")),
            ])
        })
        .collect();
    let first_variant = raw_variants
        .first()
        .and_then(|variant| variant.get("VariantName"))
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let status = Json::object(vec![
        ("status", get("EndpointStatus")),
        ("failure_reason", get("FailureReason")),
        (
            "creation_time",
            Json::String(timestamp_text(response.get("CreationTime"))),
        ),
        (
            "last_modified_time",
            Json::String(timestamp_text(response.get("LastModifiedTime"))),
        ),
        ("production_variants", Json::Array(variants.clone())),
    ]);
    Ok((status, first_variant, variants))
}

fn metric_queries(
    endpoint: &str,
    first_variant: &str,
    errors: &mut Vec<Json>,
) -> (Vec<Json>, String) {
    // Invocation metrics need the exact {EndpointName, VariantName} set.
    let variant_name = if first_variant.is_empty() {
        "AllTraffic".to_string()
    } else {
        first_variant.to_string()
    };
    let dims = dimensions(endpoint, &variant_name);
    if first_variant.is_empty() {
        errors.push(Json::string(
            "Variant name unavailable from DescribeEndpoint; defaulting to 'AllTraffic' for metrics",
        ));
    }
    let mut queries = vec![
        metric_query("invocations", SAGEMAKER, "Invocations", &dims, "Sum"),
        metric_query("errors_4xx", SAGEMAKER, "Invocation4XXErrors", &dims, "Sum"),
        metric_query("errors_5xx", SAGEMAKER, "Invocation5XXErrors", &dims, "Sum"),
        metric_query(
            "invocation_model_errors",
            SAGEMAKER,
            "InvocationModelErrors",
            &dims,
            "Sum",
        ),
        metric_query(
            "model_latency_avg",
            SAGEMAKER,
            "ModelLatency",
            &dims,
            "Average",
        ),
        metric_query("model_latency_p99", SAGEMAKER, "ModelLatency", &dims, "p99"),
        metric_query(
            "overhead_latency",
            SAGEMAKER,
            "OverheadLatency",
            &dims,
            "Average",
        ),
    ];
    if first_variant.is_empty() {
        errors.push(Json::string(
            "Variant name unavailable; skipped instance-level metrics (CPU/Memory/GPU)",
        ));
    } else {
        // Instance-level metrics use the endpoint-instance namespace.
        queries.extend([
            metric_query("cpu_util", INSTANCE, "CPUUtilization", &dims, "Average"),
            metric_query(
                "memory_util",
                INSTANCE,
                "MemoryUtilization",
                &dims,
                "Average",
            ),
            metric_query("gpu_util", INSTANCE, "GPUUtilization", &dims, "Average"),
            metric_query(
                "gpu_memory_util",
                INSTANCE,
                "GPUMemoryUtilization",
                &dims,
                "Average",
            ),
        ]);
    }
    (queries, variant_name)
}

fn collect_metrics(
    client: &mut dyn SageMakerClient,
    endpoint: &str,
    first_variant: &str,
    variants: &[Json],
    window: (UtcTime, UtcTime),
    errors: &mut Vec<Json>,
) -> Result<Json, ModelsError> {
    let (queries, variant_name) = metric_queries(endpoint, first_variant, errors);
    if variants.len() > 1 {
        let names = Json::Array(
            variants
                .iter()
                .map(|variant| variant.get("name").cloned().unwrap_or(Json::Null))
                .collect(),
        );
        // Metrics are only for the first variant: say so.
        errors.push(Json::String(format!(
            "Multiple variants detected: {}. Metrics shown are for '{variant_name}' only. Ask the \
             user which variant to diagnose if needed.",
            pyfmt::repr(&names)
        )));
    }
    let request = Json::object(vec![
        ("MetricDataQueries", Json::Array(queries)),
        ("StartTime", Json::String(iso_utc(window.0))),
        ("EndTime", Json::String(iso_utc(window.1))),
    ]);
    let mut metrics: Vec<(String, Json)> = Vec::new();
    match client.call(Operation::GetMetricData, &request) {
        Ok(response) => {
            let results = match response.get("MetricDataResults") {
                None | Some(Json::Null) => Vec::new(),
                Some(Json::Array(items)) => items.clone(),
                Some(_) => {
                    return Err(ModelsError::Data(
                        "MetricDataResults must be a list".to_string(),
                    ))
                }
            };
            for result in &results {
                let Some(id) = result.get("Id").and_then(Json::as_str) else {
                    return Err(ModelsError::Data(
                        "a metric result has no string Id".to_string(),
                    ));
                };
                let values = result
                    .get("Values")
                    .cloned()
                    .unwrap_or_else(|| Json::Array(Vec::new()));
                // A repeated id keeps its first position and the last value.
                if let Some(slot) = metrics.iter_mut().find(|(name, _)| name == id) {
                    slot.1 = values;
                } else {
                    metrics.push((id.to_string(), values));
                }
            }
        }
        Err(error) => recorded(error, "GetMetricData", errors)?,
    }
    Ok(Json::Object(metrics))
}

fn collect_logs(
    client: &mut dyn SageMakerClient,
    endpoint: &str,
    time_range: TimeRange,
    window: (UtcTime, UtcTime),
    errors: &mut Vec<Json>,
) -> Result<Json, ModelsError> {
    let fifteen = LOG_WINDOW_MINUTES * MICROS_PER_MINUTE;
    let trailing = window
        .1
        .checked_add_micros(-fifteen)
        .ok_or_else(|| ModelsError::Data("the time window is out of range".to_string()))?;
    let log_start = match time_range {
        // An absolute window is respected, capped to the trailing 15 minutes.
        TimeRange::Between { .. } => window.0.max(trailing),
        // A relative window always reads the trailing 15 minutes.
        TimeRange::Last(_) => trailing,
    };
    let request = Json::object(vec![
        (
            "logGroupName",
            Json::String(format!("/aws/sagemaker/Endpoints/{endpoint}")),
        ),
        ("startTime", Json::Int(log_start.epoch_micros() / 1000)),
        ("endTime", Json::Int(window.1.epoch_micros() / 1000)),
        ("limit", Json::Int(LOG_EVENT_LIMIT)),
    ]);
    match client.call(Operation::FilterLogEvents, &request) {
        Ok(response) => {
            let events = match response.get("events") {
                None | Some(Json::Null) => Vec::new(),
                Some(Json::Array(events))
                    if events.iter().all(|e| matches!(e, Json::Object(_))) =>
                {
                    events.clone()
                }
                Some(_) => {
                    return Err(ModelsError::Data(
                        "events must be a list of objects".to_string(),
                    ))
                }
            };
            events
                .iter()
                .map(|event| {
                    let millis = match event.get("timestamp") {
                        None | Some(Json::Null) => 0,
                        Some(Json::Int(millis)) => *millis,
                        Some(_) => {
                            return Err(ModelsError::Data(
                                "an event timestamp must be an integer".to_string(),
                            ))
                        }
                    };
                    let time = millis
                        .checked_mul(1000)
                        .and_then(UtcTime::from_epoch_micros)
                        .ok_or_else(|| {
                            ModelsError::Data("an event timestamp is out of range".to_string())
                        })?;
                    Ok(Json::object(vec![
                        (
                            "message",
                            event.get("message").cloned().unwrap_or(Json::Null),
                        ),
                        ("timestamp", Json::String(iso_utc(time))),
                    ]))
                })
                .collect::<Result<Vec<_>, ModelsError>>()
                .map(Json::Array)
        }
        Err(error) => {
            recorded(error, "FilterLogEvents", errors)?;
            Ok(Json::Array(Vec::new()))
        }
    }
}

/// Collects endpoint status, `CloudWatch` metrics and recent logs.
///
/// # Errors
///
/// A usage-level description for an invalid endpoint name or window, or the
/// failure that stopped the run (see the module documentation).
pub(super) fn collect_endpoint_diagnostics(
    client: &mut dyn SageMakerClient,
    endpoint: &str,
    time_range: TimeRange,
    now: UtcTime,
) -> Result<Json, ModelsError> {
    validate_endpoint_name(endpoint)?;
    let window = time_range.resolve(now)?;
    if window.0 >= window.1 {
        return Err(ModelsError::Data(
            "the time window must start before it ends".to_string(),
        ));
    }
    let mut errors: Vec<Json> = Vec::new();
    let (endpoint_status, first_variant, variants) =
        describe_endpoint(client, endpoint, &mut errors)?;
    let metrics = collect_metrics(
        client,
        endpoint,
        &first_variant,
        &variants,
        window,
        &mut errors,
    )?;
    let recent_logs = collect_logs(client, endpoint, time_range, window, &mut errors)?;
    Ok(Json::object(vec![
        ("endpoint_status", endpoint_status),
        ("metrics", metrics),
        ("recent_logs", recent_logs),
        ("errors", Json::Array(errors)),
        (
            "reference_docs",
            Json::object(vec![
                (
                    "metrics_definitions",
                    Json::string("https://docs.aws.amazon.com/sagemaker/latest/dg/monitoring-cloudwatch.html"),
                ),
                (
                    "troubleshooting",
                    Json::string("https://docs.aws.amazon.com/sagemaker/latest/dg/deploy-model-troubleshoot.html"),
                ),
            ]),
        ),
    ]))
}
