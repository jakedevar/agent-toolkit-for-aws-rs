#![allow(
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]
//! Offline fixtures for the handler: valid and malformed events, the error
//! paths and the output schema (log lines, EMF metrics, X-Ray subsegments,
//! idempotency records, API response).

mod support;

use std::sync::Arc;

use powertools_handler::idempotency::{self, Record, STATUS_COMPLETED, STATUS_IN_PROGRESS};
use powertools_handler::logger::format_timestamp;
use powertools_handler::metrics::{MetricUnit, Metrics, MetricsError, MAX_METRICS_PER_BLOB};
use powertools_handler::{
    HandlerError, HandlerSettings, Idempotency, IdempotencyConfig, IdempotencyError, Level, Logger,
    Tracer,
};
use serde_json::{json, Value};
use support::{context, rig, rig_with, rig_with_settings, Call, FixedClock, MemorySink, NOW_MS};

fn order_event(body: &str) -> Value {
    json!({
        "body": body,
        "headers": { "Authorization": "Bearer secret-token", "Cookie": "session=secret-cookie" },
    })
}

fn key_of(event_body: &str) -> String {
    format!(
        "orders-fn.handler#{}",
        idempotency::hash(&json!(event_body))
    )
}

#[tokio::test]
async fn valid_event_returns_the_api_response() {
    let rig = rig();
    let response = rig
        .handler
        .handle(order_event("{\"order\":1}"), &context())
        .await
        .unwrap();
    assert_eq!(response["statusCode"], 200);
    assert_eq!(response["headers"]["Content-Type"], "application/json");
    assert_eq!(
        response["headers"]["Access-Control-Allow-Origin"],
        "https://your-domain.example"
    );
    // `body` is JSON text, not a nested object.
    assert_eq!(response["body"], "{\"message\":\"success\"}");
    let keys: Vec<&str> = response
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    // Field order is the handler's own (insertion order), in every build
    // scope: the crate selects `preserve_order` itself (#5394).
    assert_eq!(keys, ["statusCode", "headers", "body"]);
    let header_keys: Vec<&str> = response["headers"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(header_keys, ["Content-Type", "Access-Control-Allow-Origin"]);
}

#[tokio::test]
async fn the_event_is_never_logged() {
    let rig = rig();
    rig.handler
        .handle(order_event("{\"order\":1}"), &context())
        .await
        .unwrap();
    for line in rig.sink.lines() {
        assert!(!line.contains("secret"), "event leaked into: {line}");
        assert!(!line.contains("\"order\""), "event leaked into: {line}");
    }
}

#[tokio::test]
async fn log_lines_keep_the_powertools_layout() {
    let rig = rig();
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    let line = rig
        .sink
        .lines()
        .into_iter()
        .find(|l| l.contains("Processing request"))
        .expect("the handler logs the start of processing");
    // Field order is part of the format: parse the keys in order of appearance.
    let mut positions: Vec<(usize, &str)> = [
        "level",
        "location",
        "message",
        "timestamp",
        "service",
        "cold_start",
        "function_name",
        "function_memory_size",
        "function_arn",
        "function_request_id",
        "xray_trace_id",
    ]
    .iter()
    .map(|k| (line.find(&format!("\"{k}\":")).expect(k), *k))
    .collect();
    let in_order = positions.clone();
    positions.sort_unstable();
    assert_eq!(positions, in_order, "fields out of order in {line}");

    let log: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(log["level"], "INFO");
    assert!(log["location"].as_str().unwrap().contains("lib.rs:"));
    assert_eq!(log["message"], "Processing request");
    assert_eq!(log["timestamp"], "2023-11-14T22:13:20.123Z");
    assert_eq!(log["service"], "orders");
    assert_eq!(log["cold_start"], true);
    assert_eq!(log["function_name"], "orders-fn");
    assert_eq!(log["function_memory_size"], 256);
    assert_eq!(
        log["function_arn"],
        "arn:aws:lambda:us-east-1:123456789012:function:orders-fn"
    );
    assert_eq!(log["function_request_id"], "req-0001");
    assert_eq!(log["xray_trace_id"], "1-5759e988-bd862e3fe1be46a994272793");
}

#[tokio::test]
async fn cold_start_is_true_only_for_the_first_invocation() {
    let rig = rig();
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    rig.handler
        .handle(order_event("b"), &context())
        .await
        .unwrap();
    let flags: Vec<Value> = rig
        .sink
        .logs()
        .into_iter()
        .map(|l| l["cold_start"].clone())
        .collect();
    assert_eq!(flags, vec![json!(true), json!(false)]);
    let cold_starts = rig
        .sink
        .metrics()
        .into_iter()
        .filter(|m| m.get("ColdStart").is_some())
        .count();
    assert_eq!(cold_starts, 1);
}

#[tokio::test]
async fn metrics_are_embedded_metric_format_lines() {
    let rig = rig();
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    let lines = rig.sink.lines();
    let blobs = rig.sink.metrics();
    assert_eq!(blobs.len(), 2, "cold start blob then request blob");
    for line in lines.iter().filter(|l| l.contains("_aws")) {
        assert!(
            line.starts_with("{\"_aws\":"),
            "EMF needs _aws first: {line}"
        );
    }

    let cold = &blobs[0];
    assert_eq!(cold["ColdStart"], json!([1.0]));
    assert_eq!(cold["function_name"], "orders-fn");
    assert_eq!(cold["service"], "orders");
    assert_eq!(
        cold["_aws"]["CloudWatchMetrics"][0]["Dimensions"],
        json!([["function_name", "service"]])
    );
    assert_eq!(
        cold["_aws"]["CloudWatchMetrics"][0]["Metrics"],
        json!([{ "Name": "ColdStart", "Unit": "Count" }])
    );

    let request = &blobs[1];
    assert_eq!(request["RequestsProcessed"], json!([1.0]));
    assert_eq!(request["service"], "orders");
    assert_eq!(request["_aws"]["Timestamp"], NOW_MS);
    assert_eq!(
        request["_aws"]["CloudWatchMetrics"][0]["Namespace"],
        "MyApp"
    );
    assert_eq!(
        request["_aws"]["CloudWatchMetrics"][0]["Dimensions"],
        json!([["service"]])
    );
    assert_eq!(
        request["_aws"]["CloudWatchMetrics"][0]["Metrics"],
        json!([{ "Name": "RequestsProcessed", "Unit": "Count" }])
    );
}

#[tokio::test]
async fn repeating_a_completed_request_returns_the_stored_response() {
    let rig = rig();
    let first = rig
        .handler
        .handle(order_event("same"), &context())
        .await
        .unwrap();
    let second = rig
        .handler
        .handle(order_event("same"), &context())
        .await
        .unwrap();
    assert_eq!(first, second);
    let processed = rig
        .sink
        .logs()
        .iter()
        .filter(|l| l["message"] == "Processing request")
        .count();
    assert_eq!(
        processed, 1,
        "the duplicate must not run the business logic"
    );
    let counted = rig
        .sink
        .metrics()
        .iter()
        .filter(|m| m.get("RequestsProcessed").is_some())
        .count();
    assert_eq!(counted, 1);
    // The record keeps the response as JSON text, for an hour.
    let record = rig.store.record(&key_of("same")).unwrap();
    assert_eq!(record.status, STATUS_COMPLETED);
    assert_eq!(record.expiry_seconds, NOW_MS / 1000 + 3600);
    let stored: Value = serde_json::from_str(record.response.as_deref().unwrap()).unwrap();
    assert_eq!(stored, first);
}

#[tokio::test]
async fn a_different_body_is_a_different_request() {
    let rig = rig();
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    rig.handler
        .handle(order_event("b"), &context())
        .await
        .unwrap();
    assert_eq!(rig.store.keys().len(), 2);
}

#[tokio::test]
async fn a_record_expires_after_the_configured_time() {
    let rig = rig();
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    rig.clock.set(NOW_MS + 3_601_000);
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    let processed = rig
        .sink
        .logs()
        .iter()
        .filter(|l| l["message"] == "Processing request")
        .count();
    assert_eq!(processed, 2);
}

#[tokio::test]
async fn a_running_duplicate_is_refused() {
    let rig = rig();
    rig.store.insert(Record {
        key: key_of("busy"),
        status: STATUS_IN_PROGRESS.to_string(),
        expiry_seconds: NOW_MS / 1000 + 3600,
        in_progress_expiry_ms: Some(NOW_MS + 10_000),
        response: None,
    });
    let error = rig
        .handler
        .handle(order_event("busy"), &context())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            HandlerError::Idempotency(IdempotencyError::AlreadyInProgress(_))
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "Execution already in progress with idempotency key: {}",
            key_of("busy")
        )
    );
    // The holder's record is left alone.
    assert_eq!(
        rig.store.record(&key_of("busy")).unwrap().status,
        STATUS_IN_PROGRESS
    );
}

#[tokio::test]
async fn an_invocation_that_timed_out_does_not_block_the_retry() {
    let rig = rig();
    rig.store.insert(Record {
        key: key_of("stuck"),
        status: STATUS_IN_PROGRESS.to_string(),
        expiry_seconds: NOW_MS / 1000 + 3600,
        in_progress_expiry_ms: Some(NOW_MS - 1),
        response: None,
    });
    let response = rig
        .handler
        .handle(order_event("stuck"), &context())
        .await
        .unwrap();
    assert_eq!(response["statusCode"], 200);
    assert_eq!(
        rig.store.record(&key_of("stuck")).unwrap().status,
        STATUS_COMPLETED
    );
}

#[tokio::test]
async fn the_lock_expires_when_the_invocation_would_time_out() {
    // A failing completion leaves the lock record behind: it must carry the
    // invocation deadline so that a timed-out holder does not block retries.
    let ctx = context();
    let rig = rig_with(IdempotencyConfig::for_body(), false);
    rig.store.fail_complete("boom");
    let _ = rig.handler.handle(order_event("a"), &ctx).await;
    let record = rig.store.record(&key_of("a")).unwrap();
    assert_eq!(record.status, STATUS_IN_PROGRESS);
    assert_eq!(record.in_progress_expiry_ms, Some(ctx.deadline_ms));
}

const KEYLESS_EVENTS: &[(&str, &str)] = &[
    ("empty object", "{}"),
    ("null event", "null"),
    ("array event", "[]"),
    ("string event", "\"text\""),
    ("number event", "42"),
    ("null body", "{\"body\":null}"),
    ("empty body", "{\"body\":\"\"}"),
];

#[tokio::test]
async fn an_event_without_a_key_skips_idempotency_with_a_warning() {
    for (name, text) in KEYLESS_EVENTS {
        let rig = rig();
        let event: Value = serde_json::from_str(text).unwrap();
        let response = rig.handler.handle(event, &context()).await.unwrap();
        assert_eq!(response["statusCode"], 200, "{name}");
        assert!(rig.store.calls().is_empty(), "{name} touched the store");
        let warnings: Vec<Value> = rig
            .sink
            .logs()
            .into_iter()
            .filter(|l| l["level"] == "WARNING")
            .collect();
        assert_eq!(warnings.len(), 1, "{name}");
        assert_eq!(
            warnings[0]["message"],
            "No idempotency key value found. Skipping persistence layer and validation operations."
        );
    }
}

// The Powertools rule (`BasePersistenceLayer.is_missing_idempotency_key`):
// None and the empty string are missing; a list or dict is missing when every
// item (for a dict: every KEY, never its values) is None; numbers and booleans
// are never missing, so 0 and false identify a request.
const MISSING_BODIES: &[(&str, &str)] = &[
    ("absent", "{}"),
    ("null", "{\"body\":null}"),
    ("empty string", "{\"body\":\"\"}"),
    ("empty list", "{\"body\":[]}"),
    ("empty object", "{\"body\":{}}"),
    ("all-null list", "{\"body\":[null,null]}"),
];

const KEYED_BODIES: &[(&str, &str)] = &[
    ("object with a null value", "{\"body\":{\"x\":null}}"),
    (
        "object with only null values",
        "{\"body\":{\"x\":null,\"y\":null}}",
    ),
    ("object with an empty object value", "{\"body\":{\"x\":{}}}"),
    ("object with a value", "{\"body\":{\"x\":1}}"),
    ("list with one non-null item", "{\"body\":[null,1]}"),
    ("list of empty lists", "{\"body\":[[]]}"),
    ("zero", "{\"body\":0}"),
    ("zero float", "{\"body\":0.0}"),
    ("false", "{\"body\":false}"),
    ("true", "{\"body\":true}"),
    ("whitespace string", "{\"body\":\" \"}"),
    ("string zero", "{\"body\":\"0\"}"),
];

fn raising() -> IdempotencyConfig {
    IdempotencyConfig {
        raise_on_no_idempotency_key: true,
        ..IdempotencyConfig::for_body()
    }
}

#[tokio::test]
async fn values_that_identify_a_request_are_persisted_and_deduplicated() {
    for (name, text) in KEYED_BODIES {
        let event: Value = serde_json::from_str(text).unwrap();
        for config in [IdempotencyConfig::for_body(), raising()] {
            let rig = rig_with(config, false);
            let first = rig
                .handler
                .handle(event.clone(), &context())
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let second = rig.handler.handle(event.clone(), &context()).await.unwrap();
            assert_eq!(first, second, "{name}");
            assert_eq!(rig.store.keys().len(), 1, "{name} must be persisted");
            let ran = rig
                .sink
                .logs()
                .iter()
                .filter(|l| l["message"] == "Processing request")
                .count();
            assert_eq!(ran, 1, "{name}: the duplicate must not run the function");
            assert!(
                rig.sink.logs().iter().all(|l| l["level"] != "WARNING"),
                "{name}: no missing-key warning"
            );
        }
    }
}

#[tokio::test]
async fn values_that_do_not_identify_a_request_skip_idempotency_or_fail() {
    for (name, text) in MISSING_BODIES {
        let event: Value = serde_json::from_str(text).unwrap();

        let skipping = rig();
        skipping
            .handler
            .handle(event.clone(), &context())
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            skipping.store.calls().is_empty(),
            "{name} touched the store"
        );
        let warnings = skipping
            .sink
            .logs()
            .iter()
            .filter(|l| l["level"] == "WARNING")
            .count();
        assert_eq!(warnings, 1, "{name}");

        let failing = rig_with(raising(), false);
        let error = failing.handler.handle(event, &context()).await.unwrap_err();
        assert!(
            matches!(
                error,
                HandlerError::Idempotency(IdempotencyError::MissingKey)
            ),
            "{name}: {error}"
        );
        assert!(failing.store.calls().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn the_whole_event_follows_the_same_rule() {
    let config = |raise| IdempotencyConfig {
        event_key: None,
        raise_on_no_idempotency_key: raise,
        ..IdempotencyConfig::for_body()
    };
    // `{"x": null}` is a non-empty dict: its key is not None.
    let rig = rig_with(config(true), false);
    let event = json!({"x": null});
    rig.handler.handle(event.clone(), &context()).await.unwrap();
    rig.handler.handle(event, &context()).await.unwrap();
    assert_eq!(rig.store.keys().len(), 1);
    // The empty event is missing.
    let rig = rig_with(config(true), false);
    assert!(rig.handler.handle(json!({}), &context()).await.is_err());
}

#[tokio::test]
async fn a_missing_key_can_be_an_error() {
    let config = IdempotencyConfig {
        raise_on_no_idempotency_key: true,
        ..IdempotencyConfig::for_body()
    };
    let rig = rig_with(config, true);
    let error = rig.handler.handle(json!({}), &context()).await.unwrap_err();
    assert!(matches!(
        error,
        HandlerError::Idempotency(IdempotencyError::MissingKey)
    ));
}

#[tokio::test]
async fn store_failures_surface_and_do_not_run_the_function() {
    let rig = rig();
    rig.store.fail_put("throttled");
    let error = rig
        .handler
        .handle(order_event("a"), &context())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("throttled"),
        "unexpected message: {error}"
    );
    assert!(rig
        .sink
        .logs()
        .iter()
        .all(|l| l["message"] != "Processing request"));
}

#[tokio::test]
async fn a_record_that_vanishes_is_retried_then_reported() {
    let rig = rig();
    rig.store.insert(Record {
        key: key_of("a"),
        status: STATUS_IN_PROGRESS.to_string(),
        expiry_seconds: NOW_MS / 1000 + 3600,
        in_progress_expiry_ms: Some(NOW_MS + 5_000),
        response: None,
    });
    rig.store.hide_records();
    let error = rig
        .handler
        .handle(order_event("a"), &context())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        HandlerError::Idempotency(IdempotencyError::InconsistentState(_))
    ));
    let puts = rig
        .store
        .calls()
        .iter()
        .filter(|c| matches!(c, Call::Put(_)))
        .count();
    assert_eq!(puts, 2, "two attempts, as in Powertools");
}

#[tokio::test]
async fn a_failing_function_frees_the_key_for_the_retry() {
    let clock = Arc::new(FixedClock::at(NOW_MS));
    let store = support::MemoryStore::default();
    let sink = Arc::new(MemorySink::default());
    let logger = Logger::new("orders", Level::Info, sink, clock.clone());
    let idempotency = Idempotency::new(store.clone(), IdempotencyConfig::for_body(), clock);
    let event = order_event("a");

    let error = idempotency
        .run(&event, &context(), &logger, || async {
            Err::<Value, _>("downstream failed".into())
        })
        .await
        .unwrap_err();
    assert!(matches!(error, IdempotencyError::Function(_)));
    assert_eq!(error.to_string(), "downstream failed");
    assert!(
        store.record(&key_of("a")).is_none(),
        "record must be removed"
    );
    assert_eq!(
        store.calls(),
        vec![Call::Put(key_of("a")), Call::Delete(key_of("a"))]
    );

    let retried = idempotency
        .run(&event, &context(), &logger, || async {
            Ok(json!({"ok": true}))
        })
        .await
        .unwrap();
    assert_eq!(retried, json!({"ok": true}));
}

#[tokio::test]
async fn a_failing_cleanup_is_reported_as_a_store_error() {
    let clock = Arc::new(FixedClock::at(NOW_MS));
    let store = support::MemoryStore::default();
    store.fail_delete("cannot delete");
    let logger = Logger::new(
        "orders",
        Level::Info,
        Arc::new(MemorySink::default()),
        clock.clone(),
    );
    let idempotency = Idempotency::new(store, IdempotencyConfig::for_body(), clock);
    let error = idempotency
        .run(&order_event("a"), &context(), &logger, || async {
            Err::<Value, _>("downstream failed".into())
        })
        .await
        .unwrap_err();
    assert!(
        matches!(error, IdempotencyError::Persistence(_)),
        "{error:?}"
    );
}

#[test]
fn the_event_key_is_a_dotted_path_and_hashing_ignores_key_order() {
    let event = json!({"requestContext": {"requestId": "r-1"}, "body": "x"});
    assert_eq!(
        idempotency::select(&event, "requestContext.requestId"),
        Some(&json!("r-1"))
    );
    assert_eq!(idempotency::select(&event, "requestContext.missing"), None);
    assert_eq!(idempotency::select(&event, "body.deeper"), None);
    assert_eq!(
        idempotency::hash(&json!({"a": 1, "b": [1, {"d": 4, "c": 3}]})),
        idempotency::hash(&json!({"b": [1, {"c": 3, "d": 4}], "a": 1}))
    );
    assert_ne!(
        idempotency::hash(&json!("a")),
        idempotency::hash(&json!("b"))
    );
    // SHA-256 of the canonical JSON text.
    assert_eq!(
        idempotency::hash(&json!("a")),
        "ac8d8342bbb2362d13f0a559a3621bb407011368895164b628a54f7fc33fc43c"
    );
    assert_eq!(
        idempotency::hash(&json!({"b": [1, {"d": 4, "c": 3}], "a": 1})),
        "81915f15f3a959467c4eea07e805ce3dce7c1c88ee57dac68e32303c29c3d772"
    );
}

#[tokio::test]
async fn the_whole_event_is_hashed_without_an_event_key() {
    let config = IdempotencyConfig {
        event_key: None,
        ..IdempotencyConfig::for_body()
    };
    let rig = rig_with(config, false);
    for event in [json!({"x": 1}), json!({"x": 1}), json!({"x": 2})] {
        rig.handler.handle(event, &context()).await.unwrap();
    }
    assert_eq!(rig.store.keys().len(), 2);
    let key = format!("orders-fn.handler#{}", idempotency::hash(&json!({"x": 1})));
    assert!(rig.store.record(&key).is_some());
}

// ---- metrics ----

fn metrics(namespace: Option<&str>) -> (Metrics, Arc<MemorySink>) {
    let sink = Arc::new(MemorySink::default());
    let metrics = Metrics::new(
        namespace,
        Some("orders"),
        sink.clone(),
        Arc::new(FixedClock::at(NOW_MS)),
    );
    (metrics, sink)
}

#[test]
fn flushing_without_a_namespace_is_an_error() {
    let (mut metrics, sink) = metrics(None);
    metrics.add_metric("RequestsProcessed", MetricUnit::Count, 1.0);
    assert_eq!(metrics.flush(), Err(MetricsError::MissingNamespace));
    assert!(sink.lines().is_empty());
    assert!(MetricsError::MissingNamespace
        .to_string()
        .contains("namespace"));
}

#[test]
fn flushing_nothing_writes_nothing() {
    let (mut metrics, sink) = metrics(Some("MyApp"));
    metrics.flush().unwrap();
    assert!(sink.lines().is_empty());
}

#[test]
fn a_repeated_metric_becomes_an_array_and_units_are_cloudwatch_names() {
    let (mut metrics, sink) = metrics(Some("MyApp"));
    metrics.add_metric("Latency", MetricUnit::Milliseconds, 5.0);
    metrics.add_metric("Latency", MetricUnit::Milliseconds, 7.5);
    metrics.add_metric("Throughput", MetricUnit::BytesPerSecond, 2.0);
    metrics.flush().unwrap();
    let blob = &sink.metrics()[0];
    assert_eq!(blob["Latency"], json!([5.0, 7.5]));
    assert_eq!(blob["Throughput"], json!([2.0]));
    assert_eq!(
        blob["_aws"]["CloudWatchMetrics"][0]["Metrics"],
        json!([
            { "Name": "Latency", "Unit": "Milliseconds" },
            { "Name": "Throughput", "Unit": "Bytes/Second" },
        ])
    );
    assert_eq!(MetricUnit::NoUnit.as_str(), "None");
}

#[test]
fn the_hundred_and_first_metric_flushes_a_full_blob() {
    let (mut metrics, sink) = metrics(Some("MyApp"));
    for index in 0..=MAX_METRICS_PER_BLOB {
        metrics.add_metric(&format!("m{index}"), MetricUnit::Count, 1.0);
    }
    assert_eq!(sink.metrics().len(), 1, "the first 100 are flushed");
    metrics.flush().unwrap();
    let blobs = sink.metrics();
    assert_eq!(blobs.len(), 2);
    assert_eq!(
        blobs[0]["_aws"]["CloudWatchMetrics"][0]["Metrics"]
            .as_array()
            .unwrap()
            .len(),
        MAX_METRICS_PER_BLOB
    );
    assert!(blobs[1].get("m100").is_some());
}

#[tokio::test]
async fn the_cold_start_metric_is_written_even_when_the_invocation_fails() {
    let rig = rig();
    rig.store.fail_put("throttled");
    let _ = rig.handler.handle(order_event("a"), &context()).await;
    let blobs = rig.sink.metrics();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0]["ColdStart"], json!([1.0]));
}

// Python `Metrics(namespace="MyApp")` adds the `service` dimension only when
// POWERTOOLS_SERVICE_NAME (or the `service` argument) is set; the Logger's
// "service_undefined" display default never reaches the metrics.
#[tokio::test]
async fn without_a_configured_service_the_metrics_have_no_service_dimension() {
    let rig = rig_with_settings(
        IdempotencyConfig::for_body(),
        false,
        HandlerSettings::default(),
    );
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    // The log lines still show the Logger's service.
    assert_eq!(rig.sink.logs()[0]["service"], "orders");
    let blobs = rig.sink.metrics();
    assert_eq!(blobs.len(), 2);

    let cold = &blobs[0];
    assert!(cold.get("service").is_none(), "{cold}");
    assert_eq!(cold["function_name"], "orders-fn");
    assert_eq!(
        cold["_aws"]["CloudWatchMetrics"][0]["Dimensions"],
        json!([["function_name"]])
    );

    let request = &blobs[1];
    assert!(request.get("service").is_none(), "{request}");
    assert_eq!(request["RequestsProcessed"], json!([1.0]));
    assert_eq!(
        request["_aws"]["CloudWatchMetrics"][0]["Dimensions"],
        json!([[]])
    );
}

#[test]
fn a_configured_service_is_a_dimension_and_an_unset_one_is_not() {
    let sink = Arc::new(MemorySink::default());
    let mut with = Metrics::new(
        Some("MyApp"),
        Some("billing"),
        sink.clone(),
        Arc::new(FixedClock::at(NOW_MS)),
    );
    with.add_metric("Orders", MetricUnit::Count, 3.0);
    with.flush().unwrap();
    let mut without = Metrics::new(
        Some("MyApp"),
        None,
        sink.clone(),
        Arc::new(FixedClock::at(NOW_MS)),
    );
    without.add_metric("Orders", MetricUnit::Count, 3.0);
    without.flush().unwrap();
    let blobs = sink.metrics();
    assert_eq!(blobs[0]["service"], "billing");
    assert_eq!(
        blobs[0]["_aws"]["CloudWatchMetrics"][0]["Dimensions"],
        json!([["service"]])
    );
    assert_eq!(blobs[0]["Orders"], json!([3.0]));
    assert!(blobs[1].get("service").is_none());
    assert_eq!(
        blobs[1]["_aws"]["CloudWatchMetrics"][0]["Dimensions"],
        json!([[]])
    );
}

// The metric values are written as Powertools writes them: a list of floats,
// after the dimension fields (top-level order is fixed by hand).
#[test]
fn the_emf_line_writes_dimensions_then_float_value_lists() {
    let (mut metrics, sink) = metrics(Some("MyApp"));
    metrics.add_metric("RequestsProcessed", MetricUnit::Count, 1.0);
    metrics.flush().unwrap();
    let line = &sink.lines()[0];
    assert!(line.starts_with("{\"_aws\":"), "{line}");
    assert!(
        line.ends_with(",\"service\":\"orders\",\"RequestsProcessed\":[1.0]}"),
        "{line}"
    );
}

// ---- logger ----

#[test]
fn log_levels_filter_and_parse() {
    let sink = Arc::new(MemorySink::default());
    let logger = Logger::new(
        "svc",
        Level::Warning,
        sink.clone(),
        Arc::new(FixedClock::at(0)),
    );
    logger.debug("d");
    logger.info("i");
    logger.warning("w");
    logger.error("e");
    let levels: Vec<Value> = sink
        .logs()
        .into_iter()
        .map(|l| l["level"].clone())
        .collect();
    assert_eq!(levels, vec![json!("WARNING"), json!("ERROR")]);

    assert_eq!(Level::parse("debug"), Some(Level::Debug));
    assert_eq!(Level::parse(" WARN "), Some(Level::Warning));
    assert_eq!(Level::parse("critical"), Some(Level::Critical));
    assert_eq!(Level::parse("loud"), None);
    assert!(Level::Debug < Level::Info);
}

#[test]
fn timestamps_are_rfc3339_in_utc() {
    assert_eq!(format_timestamp(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(format_timestamp(NOW_MS), "2023-11-14T22:13:20.123Z");
    assert_eq!(
        format_timestamp(1_709_164_800_000),
        "2024-02-29T00:00:00.000Z"
    );
    assert_eq!(
        format_timestamp(951_782_400_000),
        "2000-02-29T00:00:00.000Z"
    );
    assert_eq!(
        format_timestamp(4_102_444_799_999),
        "2099-12-31T23:59:59.999Z"
    );
}

// ---- tracer ----

#[tokio::test]
async fn subsegments_are_sent_under_the_function_segment() {
    let rig = rig();
    rig.handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    let docs = rig.segments.documents();
    assert_eq!(docs.len(), 2);
    let method = &docs[0];
    let handler = &docs[1];
    assert_eq!(handler["name"], "## handler");
    assert_eq!(method["name"], "### process");
    for doc in &docs {
        assert_eq!(doc["type"], "subsegment");
        assert_eq!(doc["trace_id"], "1-5759e988-bd862e3fe1be46a994272793");
        assert!(doc["end_time"].as_f64().unwrap() >= doc["start_time"].as_f64().unwrap());
        assert_eq!(doc["id"].as_str().unwrap().len(), 16);
        assert!(doc.get("fault").is_none());
    }
    assert_eq!(handler["parent_id"], "53995c3f42cd8ad8");
    assert_eq!(method["parent_id"], handler["id"]);
    assert_ne!(method["id"], handler["id"]);
    assert_eq!(
        handler["annotations"],
        json!({"ColdStart": true, "Service": "orders"})
    );
}

#[tokio::test]
async fn a_failed_invocation_marks_the_handler_subsegment_as_a_fault() {
    let rig = rig();
    rig.store.fail_put("throttled");
    let _ = rig.handler.handle(order_event("a"), &context()).await;
    let docs = rig.segments.documents();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["fault"], true);
    assert!(docs[0]["cause"]["exceptions"][0]["message"]
        .as_str()
        .unwrap()
        .contains("throttled"));
}

#[tokio::test]
async fn unsampled_untraced_or_unconfigured_invocations_send_nothing() {
    let rig = rig();
    let mut ctx = context();
    ctx.xray_trace_id = Some(
        "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=0".to_string(),
    );
    rig.handler.handle(order_event("a"), &ctx).await.unwrap();
    ctx.xray_trace_id = None;
    rig.handler.handle(order_event("b"), &ctx).await.unwrap();
    ctx.xray_trace_id = Some("not a trace header".to_string());
    rig.handler.handle(order_event("c"), &ctx).await.unwrap();
    assert!(rig.segments.documents().is_empty());

    let untraced = rig_with(IdempotencyConfig::for_body(), false);
    untraced
        .handler
        .handle(order_event("a"), &context())
        .await
        .unwrap();
    assert!(untraced.segments.documents().is_empty());

    // A tracer without a sink is also safe on its own.
    let tracer = Tracer::new(None, Arc::new(FixedClock::at(NOW_MS)));
    tracer.begin_invocation(Some(support::TRACE_HEADER), true, "svc");
    let value: Result<u8, String> = tracer.capture_method("m", async { Ok(1) }).await;
    assert_eq!(value, Ok(1));
    tracer.end_invocation(None);
}
