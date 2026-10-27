use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::{dumps, j, Spy};
use crate::ai_models::client::Operation;
use crate::ai_models::diagnostics::{collect_endpoint_diagnostics, TimeRange};
use crate::ai_models::hub::ModelsError;
use crate::cloudwatch::timefmt::{parse_iso_timestamp, UtcTime};

fn at(text: &str) -> UtcTime {
    parse_iso_timestamp(text).expect("timestamp")
}

fn now() -> UtcTime {
    at("2024-03-15T15:00:00Z")
}

fn service(message: &str) -> AwsError {
    AwsError::service(ServiceError::new(
        "POST",
        "/",
        400,
        Some("ValidationException"),
        message,
        None,
    ))
}

const ENDPOINT: &str = r#"{"EndpointStatus": "InService", "CreationTime": 1710000000,
    "LastModifiedTime": "2024-03-10 00:00:00+00:00",
    "ProductionVariants": [{"VariantName": "AllTraffic", "InstanceType": "ml.g5.xlarge",
                            "CurrentInstanceCount": 1, "CurrentWeight": 1.0}]}"#;

fn run(answers: Vec<Result<Json, AwsError>>, range: TimeRange) -> (Json, Spy) {
    let mut spy = Spy::new(answers);
    let result =
        collect_endpoint_diagnostics(&mut spy, "my-endpoint", range, now()).expect("collects");
    (result, spy)
}

#[allow(clippy::unnecessary_wraps)]
fn ok(text: &str) -> Result<Json, AwsError> {
    Ok(j(text))
}

fn member<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or_else(|| panic!("no {key}"))
}

fn ids(request: &Json) -> Vec<String> {
    let Some(Json::Array(queries)) = request.get("MetricDataQueries") else {
        panic!("queries")
    };
    queries
        .iter()
        .map(|q| member(q, "Id").as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_healthy_endpoint_produces_the_documented_requests_and_result() {
    let (result, spy) = run(
        vec![
            ok(ENDPOINT),
            ok(
                r#"{"MetricDataResults": [{"Id": "invocations", "Values": [1.0, 2.0]}, {"Id": "cpu_util"}]}"#,
            ),
            ok(r#"{"events": [{"message": "hello", "timestamp": 1710514500123}]}"#),
        ],
        TimeRange::Last(5),
    );
    let ops: Vec<Operation> = spy.seen.iter().map(|(op, _)| *op).collect();
    assert_eq!(
        ops,
        [
            Operation::DescribeEndpoint,
            Operation::GetMetricData,
            Operation::FilterLogEvents
        ]
    );
    assert_eq!(spy.seen[0].1, j(r#"{"EndpointName": "my-endpoint"}"#));

    let metrics = &spy.seen[1].1;
    assert_eq!(
        member(metrics, "StartTime").as_str(),
        Some("2024-03-15T14:55:00+00:00")
    );
    assert_eq!(
        member(metrics, "EndTime").as_str(),
        Some("2024-03-15T15:00:00+00:00")
    );
    assert_eq!(
        ids(metrics),
        [
            "invocations",
            "errors_4xx",
            "errors_5xx",
            "invocation_model_errors",
            "model_latency_avg",
            "model_latency_p99",
            "overhead_latency",
            "cpu_util",
            "memory_util",
            "gpu_util",
            "gpu_memory_util"
        ]
    );
    let Some(Json::Array(queries)) = metrics.get("MetricDataQueries") else {
        panic!("queries")
    };
    assert_eq!(
        dumps(&queries[5]),
        concat!(
            r#"{"Id": "model_latency_p99", "MetricStat": {"Metric": {"Namespace": "AWS/SageMaker", "#,
            r#""MetricName": "ModelLatency", "Dimensions": [{"Name": "EndpointName", "Value": "my-endpoint"}, "#,
            r#"{"Name": "VariantName", "Value": "AllTraffic"}]}, "Period": 60, "Stat": "p99"}}"#
        )
    );
    assert!(dumps(&queries[7])
        .contains(r#""Namespace": "/aws/sagemaker/Endpoints", "MetricName": "CPUUtilization""#));

    // 15 trailing minutes of logs, one page of at most 100 events.
    assert_eq!(
        spy.seen[2].1,
        j(
            r#"{"logGroupName": "/aws/sagemaker/Endpoints/my-endpoint", "startTime": 1710513900000,
              "endTime": 1710514800000, "limit": 100}"#
        )
    );
    assert_eq!(
        dumps(&result),
        concat!(
            r#"{"endpoint_status": {"status": "InService", "failure_reason": null, "#,
            r#""creation_time": "2024-03-09 16:00:00+00:00", "last_modified_time": "2024-03-10 00:00:00+00:00", "#,
            r#""production_variants": [{"name": "AllTraffic", "instance_type": "ml.g5.xlarge", "#,
            r#""current_instance_count": 1, "current_weight": 1.0}]}, "#,
            r#""metrics": {"invocations": [1.0, 2.0], "cpu_util": []}, "#,
            r#""recent_logs": [{"message": "hello", "timestamp": "2024-03-15T14:55:00.123000+00:00"}], "#,
            r#""errors": [], "reference_docs": {"metrics_definitions": "#,
            r#""https://docs.aws.amazon.com/sagemaker/latest/dg/monitoring-cloudwatch.html", "troubleshooting": "#,
            r#""https://docs.aws.amazon.com/sagemaker/latest/dg/deploy-model-troubleshoot.html"}}"#
        )
    );
}

#[test]
fn without_a_variant_name_metrics_default_to_alltraffic_and_say_so() {
    let (result, spy) = run(
        vec![ok(r#"{"EndpointStatus": "Creating"}"#), ok("{}"), ok("{}")],
        TimeRange::Last(5),
    );
    assert_eq!(ids(&spy.seen[1].1).len(), 7);
    assert!(dumps(&spy.seen[1].1).contains(r#""Value": "AllTraffic""#));
    assert_eq!(
        dumps(member(&result, "errors")),
        concat!(
            r#"["Variant name unavailable from DescribeEndpoint; defaulting to 'AllTraffic' for metrics", "#,
            r#""Variant name unavailable; skipped instance-level metrics (CPU/Memory/GPU)"]"#
        )
    );
    assert_eq!(dumps(member(&result, "metrics")), "{}");
    assert_eq!(dumps(member(&result, "recent_logs")), "[]");
}

#[test]
fn a_failed_describe_is_recorded_and_the_other_steps_still_run() {
    let (result, spy) = run(
        vec![Err(service("Could not find endpoint")), ok("{}"), ok("{}")],
        TimeRange::Last(5),
    );
    assert_eq!(spy.seen.len(), 3);
    assert_eq!(dumps(member(&result, "endpoint_status")), "{}");
    assert_eq!(
        dumps(member(&result, "errors")),
        concat!(
            r#"["DescribeEndpoint failed: Could not find endpoint", "#,
            r#""Variant name unavailable from DescribeEndpoint; defaulting to 'AllTraffic' for metrics", "#,
            r#""Variant name unavailable; skipped instance-level metrics (CPU/Memory/GPU)"]"#
        )
    );
}

#[test]
fn more_than_one_variant_is_reported_with_the_one_measured() {
    let (result, _) = run(
        vec![
            ok(r#"{"ProductionVariants": [{"VariantName": "A"}, {"VariantName": "B"}, {}]}"#),
            ok("{}"),
            ok("{}"),
        ],
        TimeRange::Last(5),
    );
    assert_eq!(
        dumps(member(&result, "errors")),
        concat!(
            r#"["Multiple variants detected: ['A', 'B', None]. Metrics shown are for 'A' only. "#,
            r#"Ask the user which variant to diagnose if needed."]"#
        )
    );
}

#[test]
fn metric_and_log_failures_are_recorded_without_stopping_the_run() {
    let (result, _) = run(
        vec![
            ok(ENDPOINT),
            Err(service("metrics denied")),
            Err(service("no log group")),
        ],
        TimeRange::Last(5),
    );
    assert_eq!(
        dumps(member(&result, "errors")),
        r#"["GetMetricData failed: metrics denied", "FilterLogEvents failed: no log group"]"#
    );
    assert_eq!(dumps(member(&result, "metrics")), "{}");
}

#[test]
fn a_failure_that_is_not_a_service_error_stops_the_run() {
    for (answers, label) in [
        (vec![Err(AwsError::NoCredentials)], "describe"),
        (
            vec![ok(ENDPOINT), Err(AwsError::connect("refused"))],
            "metrics",
        ),
        (
            vec![ok(ENDPOINT), ok("{}"), Err(AwsError::NoCredentials)],
            "logs",
        ),
    ] {
        let mut spy = Spy::new(answers);
        let result =
            collect_endpoint_diagnostics(&mut spy, "my-endpoint", TimeRange::Last(5), now());
        assert!(matches!(result, Err(ModelsError::Aws(_))), "{label}");
    }
}

#[test]
fn an_absolute_window_is_respected_and_logs_are_capped_to_fifteen_minutes() {
    let cases = [
        // (start, end, expected log startTime in ms)
        (
            "2024-03-15T14:30:00Z",
            Some("2024-03-15T15:00:00Z"),
            1_710_513_900_000_i64,
        ),
        (
            "2024-03-15T14:50:00Z",
            Some("2024-03-15T15:00:00Z"),
            1_710_514_200_000,
        ),
        ("2024-03-15T14:50:00Z", None, 1_710_514_200_000),
    ];
    for (start, end, log_start) in cases {
        let range = TimeRange::Between {
            start: at(start),
            end: end.map(at),
        };
        let (_, spy) = run(vec![ok(ENDPOINT), ok("{}"), ok("{}")], range);
        let metrics = &spy.seen[1].1;
        assert!(member(metrics, "StartTime")
            .as_str()
            .unwrap()
            .starts_with(&start[..19]));
        assert_eq!(
            member(metrics, "EndTime").as_str(),
            Some("2024-03-15T15:00:00+00:00")
        );
        assert_eq!(
            member(&spy.seen[2].1, "startTime"),
            &Json::Int(log_start),
            "{start}"
        );
    }
}

#[test]
fn a_long_relative_window_still_reads_fifteen_minutes_of_logs() {
    let (_, spy) = run(vec![ok(ENDPOINT), ok("{}"), ok("{}")], TimeRange::Last(60));
    assert_eq!(
        member(&spy.seen[1].1, "StartTime").as_str(),
        Some("2024-03-15T14:00:00+00:00")
    );
    assert_eq!(
        member(&spy.seen[2].1, "startTime"),
        &Json::Int(1_710_513_900_000)
    );
}

#[test]
fn log_text_stays_data_and_leaves_as_escapes() {
    let (result, _) = run(
        vec![
            ok(ENDPOINT),
            ok("{}"),
            ok(
                r#"{"events": [{"message": "a\u001b[31m\u202e\nerror: injected"}, {"message": null}]}"#,
            ),
        ],
        TimeRange::Last(5),
    );
    let text = dumps(&result);
    assert!(
        text.contains(r#""message": "a\u001b[31m\u202e\nerror: injected""#),
        "{text}"
    );
    assert!(
        text.contains(r#""message": null, "timestamp": "1970-01-01T00:00:00+00:00""#),
        "{text}"
    );
    assert!(text.is_ascii() && !text.contains('\n') && !text.contains('\u{1b}'));
}

#[test]
fn a_missing_creation_time_prints_none_like_python() {
    let (result, _) = run(
        vec![
            ok(r#"{"EndpointStatus": "Failed", "FailureReason": "boom"}"#),
            ok("{}"),
            ok("{}"),
        ],
        TimeRange::Last(5),
    );
    let status = member(&result, "endpoint_status");
    assert_eq!(member(status, "creation_time"), &Json::string("None"));
    assert_eq!(member(status, "failure_reason"), &Json::string("boom"));
}

#[test]
fn invalid_input_stops_before_any_call() {
    let mut spy = Spy::new(vec![]);
    let name = collect_endpoint_diagnostics(&mut spy, "bad name\u{1b}", TimeRange::Last(5), now());
    match name {
        Err(ModelsError::Data(message)) => {
            assert!(message.starts_with("invalid endpoint name"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    let backwards = TimeRange::Between {
        start: at("2024-03-15T16:00:00Z"),
        end: Some(at("2024-03-15T15:00:00Z")),
    };
    let window = collect_endpoint_diagnostics(&mut spy, "e", backwards, now());
    assert!(
        matches!(window, Err(ModelsError::Data(m)) if m == "the time window must start before it ends")
    );
    let huge = collect_endpoint_diagnostics(&mut spy, "e", TimeRange::Last(i64::MAX), now());
    assert!(matches!(huge, Err(ModelsError::Data(m)) if m == "the time window is out of range"));
    assert!(spy.seen.is_empty());
}

#[test]
fn malformed_responses_are_errors() {
    for answers in [
        vec![ok(r#"{"ProductionVariants": "x"}"#)],
        vec![
            ok(ENDPOINT),
            ok(r#"{"MetricDataResults": [{"Values": []}]}"#),
        ],
        vec![
            ok(ENDPOINT),
            ok("{}"),
            ok(r#"{"events": [{"timestamp": "x"}]}"#),
        ],
    ] {
        let mut spy = Spy::new(answers);
        let result = collect_endpoint_diagnostics(&mut spy, "e", TimeRange::Last(5), now());
        assert!(matches!(result, Err(ModelsError::Data(_))), "{result:?}");
    }
}
