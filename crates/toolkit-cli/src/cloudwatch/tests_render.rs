//! Tests for the renderers, the status assessment and the
//! `toolkit cloudwatch` command surface. Fixtures are literals; the clock is
//! never read except by the one CLI test that deliberately leaves `now`
//! unset and asserts only what does not depend on it.

use std::ffi::OsString;

use toolkit_core::{parse_json, Json};

use super::capture::CaptureLimits;
use super::crud_rendering::{
    format_batch_delete_response, render_create_success_message, render_get_instrumentation_output,
    render_list_instrumentations_output, CreateSummary,
};
use super::location::{location_from_response, parse_create_inputs, LocationInputs};
use super::pytext::group_thousands;
use super::snapshot_rendering::{
    render_get_sample_snapshot_for_breakpoint_output,
    render_search_snapshots_for_status_event_output, QueryResult, SampleContext, SearchContext,
};
use super::status_assessment::{assess, StatusCheckResult, TimeWindow, Verdict};
use super::status_rendering::{
    render_consolidated_active_status_output, render_consolidated_error_or_pending_status_output,
    render_consolidated_ready_status_output,
    render_get_instrumentation_configuration_status_output, render_status_assessment,
    StatusSubject,
};
use super::timefmt::{parse_iso_timestamp, UtcTime};

fn json(text: &str) -> Json {
    parse_json(text).expect("valid fixture json")
}

fn at(text: &str) -> UtcTime {
    parse_iso_timestamp(text).expect("valid fixture timestamp")
}

// ───────────────────────────── snapshot rendering ─────────────────────────────

fn query_result(text: &str) -> QueryResult {
    QueryResult::from_json(&json(text)).expect("valid query result")
}

fn search_context(filters: &[String]) -> SearchContext<'_> {
    SearchContext {
        service_name: "svc",
        environment: "prod",
        location_hash: "0123456789abcdef",
        custom_filters: filters,
        start_time_utc: "2025-02-03T18:41:55Z",
        end_time_utc: "2025-02-03T18:43:00Z",
        start_epoch: 1_738_608_115,
        end_epoch: 1_738_608_180,
        query_string: "fields @timestamp",
    }
}

fn sample_context(include_raw: bool) -> SampleContext<'static> {
    SampleContext {
        service_name: "svc",
        environment: "prod",
        location_hash: "0123456789abcdef",
        start_time_utc: "2025-02-03T18:41:30Z",
        end_time_utc: "2025-02-03T18:44:00Z",
        max_timeout: 30,
        query_string: "fields @timestamp",
        include_raw,
    }
}

#[test]
fn query_results_are_read_from_json() {
    let result = query_result(
        r#"{"status":"Complete","queryId":"q1","results":[{"@message":"{}"}],"messages":["m"]}"#,
    );
    assert_eq!(result.status, "Complete");
    assert_eq!(result.query_id, Json::string("q1"));
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.messages, vec![Json::string("m")]);
    assert_eq!(result.error, None);

    let error = query_result(r#"{"status":"Error","error":"boom"}"#);
    assert_eq!(error.error.as_deref(), Some("boom"));
    assert_eq!(error.query_id, Json::Null);

    for bad in [
        "[]",
        "{}",
        r#"{"status":1}"#,
        r#"{"status":"x","results":3}"#,
    ] {
        assert!(QueryResult::from_json(&json(bad)).is_err(), "{bad}");
    }
}

#[test]
fn search_error_status_renders_the_error_shape() {
    let text = render_search_snapshots_for_status_event_output(
        &search_context(&[]),
        &query_result(r#"{"status":"Error","error":"Failed to start query: boom","results":[]}"#),
    );
    assert_eq!(
        text,
        "{\n  \"status\": \"ERROR\",\n  \"service_name\": \"svc\",\n  \"environment\": \"prod\",\n  \"log_group_name\": \"/aws/service-events/svc\",\n  \"location_hash\": \"0123456789abcdef\",\n  \"custom_filters\": [],\n  \"start_time_utc\": \"2025-02-03T18:41:55Z\",\n  \"end_time_utc\": \"2025-02-03T18:43:00Z\",\n  \"query_string\": \"fields @timestamp\",\n  \"error\": \"Failed to start query: boom\"\n}"
    );
    // A missing error text reads `Unknown error`.
    let unknown = render_search_snapshots_for_status_event_output(
        &search_context(&[]),
        &query_result(r#"{"status":"Error"}"#),
    );
    assert!(unknown.ends_with("\"error\": \"Unknown error\"\n}"));
}

#[test]
fn search_timeout_and_other_failures_are_never_success_shaped() {
    let filters = vec!["@message like /ORD-1/".to_string()];
    let timeout = render_search_snapshots_for_status_event_output(
        &search_context(&filters),
        &query_result(r#"{"status":"Polling Timeout","queryId":"q9","results":[],"error":"x"}"#),
    );
    let parsed = json(&timeout);
    assert_eq!(parsed.get("status"), Some(&Json::string("TIMEOUT")));
    assert_eq!(parsed.get("queryId"), Some(&Json::string("q9")));
    assert_eq!(
        parsed.get("custom_filters"),
        Some(&Json::Array(vec![Json::string("@message like /ORD-1/")]))
    );
    assert_eq!(
        parsed.get("message"),
        Some(&Json::string(
            "Query did not complete within the requested timeout. Use get-query-results with the returned queryId to retry."
        ))
    );
    assert!(timeout.starts_with("{\n  \"queryId\": \"q9\",\n  \"status\": \"TIMEOUT\",\n"));

    for status in ["Failed", "Cancelled", "Running"] {
        let text = render_search_snapshots_for_status_event_output(
            &search_context(&[]),
            &query_result(&format!(
                r#"{{"status":"{status}","queryId":"q1","results":[{{"@message":"{{}}"}}],"messages":[{{"message":"m"}}]}}"#
            )),
        );
        let parsed = json(&text);
        assert_eq!(parsed.get("status"), Some(&Json::string(status)));
        assert_eq!(parsed.get("results"), None, "no results on a failure shape");
        assert_eq!(parsed.get("messages"), Some(&json(r#"[{"message":"m"}]"#)));
        assert!(text.starts_with(&format!(
            "{{\n  \"status\": \"{status}\",\n  \"queryId\": \"q1\",\n"
        )));
    }
}

#[test]
fn search_success_summarizes_each_record() {
    let result = query_result(
        r#"{"status":"Complete","queryId":"q1","results":[
            {"@timestamp":"2025-02-03 18:42:01.000","@message":"{\"traceId\":\"t1\",\"spanId\":\"s1\",\"attributes\":{\"aws.di.snapshot_id\":\"snap-1\",\"aws.di.location_hash\":\"0123456789abcdef\"},\"note\":\"café\"}"},
            {"@timestamp":"2025-02-03 18:42:02.000","@message":"not json"},
            {"@timestamp":"2025-02-03 18:42:03.000","@message":"[1]"}
        ]}"#,
    );
    let text = render_search_snapshots_for_status_event_output(&search_context(&[]), &result);
    let parsed = json(&text);
    assert_eq!(parsed.get("status"), Some(&Json::string("Complete")));
    assert_eq!(parsed.get("start_epoch"), Some(&Json::Int(1_738_608_115)));
    assert_eq!(parsed.get("end_epoch"), Some(&Json::Int(1_738_608_180)));
    assert_eq!(parsed.get("custom_filters"), Some(&Json::Array(Vec::new())));
    assert_eq!(parsed.get("messages"), Some(&Json::Array(Vec::new())));
    assert_eq!(
        parsed.get("snapshot_summaries"),
        Some(&json(
            r#"[
                {"@timestamp":"2025-02-03 18:42:01.000","snapshot_id":"snap-1","location_hash":"0123456789abcdef","traceId":"t1","spanId":"s1"},
                {"@timestamp":"2025-02-03 18:42:02.000","snapshot_id":null,"location_hash":null,"traceId":null,"spanId":null},
                {"@timestamp":"2025-02-03 18:42:03.000","snapshot_id":null,"location_hash":null,"traceId":null,"spanId":null}
            ]"#
        ))
    );
    assert_eq!(
        parsed.get("results").and_then(|r| match r {
            Json::Array(items) => Some(items.len()),
            _ => None,
        }),
        Some(3)
    );
    // Key order is the legacy order, and output is ASCII-only.
    let order: Vec<&str> = match &parsed {
        Json::Object(pairs) => pairs.iter().map(|(key, _)| key.as_str()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(
        order,
        [
            "queryId",
            "status",
            "log_group_name",
            "service_name",
            "environment",
            "location_hash",
            "custom_filters",
            "start_time_utc",
            "end_time_utc",
            "start_epoch",
            "end_epoch",
            "query_string",
            "messages",
            "snapshot_summaries",
            "results"
        ]
    );
    assert!(text.contains("caf\\u00e9"));
    assert!(text.is_ascii());
}

#[test]
fn sample_failure_shapes() {
    let error = render_get_sample_snapshot_for_breakpoint_output(
        &sample_context(false),
        &query_result(r#"{"status":"Error","error":"Failed to get results: x","results":[]}"#),
    );
    assert_eq!(
        error,
        "{\n  \"status\": \"ERROR\",\n  \"service_name\": \"svc\",\n  \"environment\": \"prod\",\n  \"log_group_name\": \"/aws/service-events/svc\",\n  \"location_hash\": \"0123456789abcdef\",\n  \"error\": \"Failed to get results: x\",\n  \"query_string\": \"fields @timestamp\"\n}"
    );

    let timeout = render_get_sample_snapshot_for_breakpoint_output(
        &sample_context(false),
        &query_result(r#"{"status":"Polling Timeout","queryId":"q2"}"#),
    );
    let parsed = json(&timeout);
    assert_eq!(parsed.get("status"), Some(&Json::string("TIMEOUT")));
    assert_eq!(
        parsed.get("message"),
        Some(&Json::string("Query did not complete within 30 seconds."))
    );

    let cancelled = render_get_sample_snapshot_for_breakpoint_output(
        &sample_context(false),
        &query_result(r#"{"status":"Cancelled","queryId":"q3","messages":[]}"#),
    );
    let parsed = json(&cancelled);
    assert_eq!(parsed.get("status"), Some(&Json::string("Cancelled")));
    assert_eq!(parsed.get("sample_snapshot"), None);
}

#[test]
fn sample_with_no_results_explains_what_to_try() {
    let text = render_get_sample_snapshot_for_breakpoint_output(
        &sample_context(false),
        &query_result(r#"{"status":"Complete","queryId":"q1","results":[]}"#),
    );
    let parsed = json(&text);
    assert_eq!(
        parsed.get("status"),
        Some(&Json::string("NO_SNAPSHOTS_FOUND"))
    );
    assert_eq!(
        parsed.get("time_range"),
        Some(&json(
            r#"{"start":"2025-02-03T18:41:30Z","end":"2025-02-03T18:44:00Z"}"#
        ))
    );
    let message = parsed
        .get("message")
        .and_then(Json::as_str)
        .expect("message");
    assert!(message.starts_with("No snapshots found in this window. Suggestions: (1) Try an older ACTIVE event timestamp \u{2014} older events"));
    assert!(message.ends_with(
        "(3) Verify the breakpoint is still ACTIVE and not DISABLED from max_hits exhaustion."
    ));
}

#[test]
fn small_samples_are_returned_whole_with_field_documentation() {
    let message = r#"{"traceId":"t1","attributes":{"aws.di.snapshot_id":"snap-1"}}"#;
    let result = Json::object(vec![
        ("status", Json::string("Complete")),
        ("queryId", Json::string("q1")),
        (
            "results",
            Json::Array(vec![Json::object(vec![
                ("@timestamp", Json::string("2025-02-03 18:42:01.000")),
                ("@message", Json::string(message)),
            ])]),
        ),
        ("messages", Json::Array(vec![Json::string("m")])),
    ]);
    let text = render_get_sample_snapshot_for_breakpoint_output(
        &sample_context(false),
        &QueryResult::from_json(&result).expect("query result"),
    );
    let parsed = json(&text);
    assert_eq!(parsed.get("status"), Some(&Json::string("SUCCESS")));
    assert_eq!(parsed.get("note"), None);
    assert_eq!(parsed.get("sample_snapshot"), Some(&json(message)));
    assert_eq!(
        parsed.get("cloudwatch_timestamp"),
        Some(&Json::string("2025-02-03 18:42:01.000"))
    );
    assert_eq!(
        parsed.get("messages"),
        Some(&Json::Array(vec![Json::string("m")]))
    );
    // recorded-shape: test assertion over a document the test built.
    let Some(Json::Object(documentation)) = parsed.get("field_documentation") else {
        panic!("field_documentation missing");
    };
    assert_eq!(documentation.len(), 19);
    assert_eq!(documentation[0].0, "attributes.aws.di.snapshot_id");
    assert_eq!(
        documentation[17].0,
        "body.captures.lines.<line>.locals.<name>"
    );
    assert_eq!(documentation[18].0, "CapturedValue shapes");
    let order: Vec<&str> = match &parsed {
        Json::Object(pairs) => pairs.iter().map(|(key, _)| key.as_str()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(
        order,
        [
            "status",
            "queryId",
            "service_name",
            "environment",
            "log_group_name",
            "location_hash",
            "time_range",
            "cloudwatch_timestamp",
            "sample_snapshot",
            "field_documentation",
            "messages"
        ]
    );
}

fn large_sample(include_raw: bool) -> (String, usize) {
    let padding = "x".repeat(11_000);
    let message = format!(
        r#"{{"traceId":"t1","attributes":{{"aws.di.snapshot_id":"snap-1","aws.di.location_hash":"0123456789abcdef","aws.di.class_name":"Billing"}},"body":{{"stack":[{{"file_path":"a.py","function":"f","line_number":3}}]}},"pad":"{padding}"}}"#
    );
    let size = message.len();
    let result = Json::object(vec![
        ("status", Json::string("Complete")),
        ("queryId", Json::string("q1")),
        (
            "results",
            Json::Array(vec![Json::object(vec![
                ("@timestamp", Json::string("2025-02-03 18:42:01.000")),
                ("@message", Json::string(message)),
            ])]),
        ),
    ]);
    let text = render_get_sample_snapshot_for_breakpoint_output(
        &sample_context(include_raw),
        &QueryResult::from_json(&result).expect("query result"),
    );
    (text, size)
}

#[test]
fn oversize_samples_are_replaced_by_the_parsed_summary_unless_raw_is_requested() {
    let (text, size) = large_sample(false);
    assert!(size > 10 * 1024);
    let parsed = json(&text);
    assert_eq!(
        parsed.get("note").and_then(Json::as_str),
        Some(
            format!(
                "Raw snapshot was {} bytes and has been replaced with a compact parsed summary. To get the full raw snapshot, call this tool again with include_raw=True.",
                group_thousands(size as u64)
            )
            .as_str()
        )
    );
    let sample = parsed.get("sample_snapshot").expect("sample_snapshot");
    assert_eq!(sample.get("snapshot_id"), Some(&Json::string("snap-1")));
    assert_eq!(sample.get("stack_frame_count"), Some(&Json::Int(1)));
    assert_eq!(sample.get("raw_snapshot"), None, "the raw copy is dropped");
    assert!(!text.contains("xxxxxxxx"), "padding must not be echoed");

    let (text, _) = large_sample(true);
    let parsed = json(&text);
    assert_eq!(parsed.get("note"), None);
    let sample = parsed.get("sample_snapshot").expect("sample_snapshot");
    assert_eq!(sample.get("traceId"), Some(&Json::string("t1")));
    assert!(text.contains("xxxxxxxx"));
}

// ───────────────────────────── status assessment ─────────────────────────────

fn events(times: &[&str]) -> Vec<Json> {
    times
        .iter()
        .map(|time| Json::object(vec![("Time", Json::string(*time))]))
        .collect()
}

fn found(times: &[&str]) -> StatusCheckResult {
    StatusCheckResult {
        has_events: !times.is_empty(),
        events: events(times),
        error: None,
    }
}

type Call = (String, String, String);

fn run_assess(
    created: &str,
    start: &str,
    end: &str,
    answers: &[(&str, StatusCheckResult)],
) -> (Verdict, TimeWindow, Vec<Call>) {
    let mut calls: Vec<Call> = Vec::new();
    let (verdict, window) = assess(at(created), at(start), at(end), &mut |status, from, to| {
        calls.push((
            status.to_string(),
            from.format_seconds_z(),
            to.format_seconds_z(),
        ));
        answers
            .iter()
            .find(|(name, _)| *name == status)
            .map(|(_, result)| result.clone())
            .unwrap_or_default()
    });
    (verdict, window, calls)
}

fn call(status: &str, from: &str, to: &str) -> Call {
    (status.to_string(), from.to_string(), to.to_string())
}

#[test]
fn active_wins_and_its_window_is_clamped_to_creation() {
    let (verdict, window, calls) = run_assess(
        "2025-02-03T10:00:00Z",
        "2025-02-03T09:00:00Z",
        "2025-02-03T11:00:00Z",
        &[("ACTIVE", found(&["2025-02-03T10:05:00Z"]))],
    );
    assert!(matches!(verdict, Verdict::Active { .. }));
    assert_eq!(
        window,
        TimeWindow {
            created_at: "2025-02-03T10:00:00Z".to_string(),
            requested_start: "2025-02-03T09:00:00Z".to_string(),
            active_query_start: "2025-02-03T10:00:00Z".to_string(),
            query_end: "2025-02-03T11:00:00Z".to_string(),
        }
    );
    assert_eq!(
        calls,
        vec![call(
            "ACTIVE",
            "2025-02-03T10:00:00Z",
            "2025-02-03T11:00:00Z"
        )]
    );
}

#[test]
fn a_requested_start_after_creation_is_not_clamped() {
    let (_, window, calls) = run_assess(
        "2025-02-03T08:00:00Z",
        "2025-02-03T09:00:00Z",
        "2025-02-03T11:00:00Z",
        &[("ACTIVE", found(&["2025-02-03T10:05:00Z"]))],
    );
    assert_eq!(window.active_query_start, "2025-02-03T09:00:00Z");
    assert_eq!(calls[0].1, "2025-02-03T09:00:00Z");
}

#[test]
fn ready_is_checked_over_the_full_requested_window() {
    let (verdict, _, calls) = run_assess(
        "2025-02-03T10:00:00Z",
        "2025-02-03T09:00:00Z",
        "2025-02-03T11:00:00Z",
        &[("READY", found(&["2025-02-03T10:01:00Z"]))],
    );
    assert!(matches!(verdict, Verdict::Ready { .. }));
    assert_eq!(
        calls,
        vec![
            call("ACTIVE", "2025-02-03T10:00:00Z", "2025-02-03T11:00:00Z"),
            call("READY", "2025-02-03T09:00:00Z", "2025-02-03T11:00:00Z"),
        ]
    );
}

#[test]
fn error_decides_between_error_and_pending() {
    let (verdict, _, calls) = run_assess(
        "2025-02-03T10:00:00Z",
        "2025-02-03T09:00:00Z",
        "2025-02-03T11:00:00Z",
        &[],
    );
    let Verdict::ErrorOrPending { error, .. } = verdict else {
        panic!("expected ErrorOrPending");
    };
    assert!(!error.has_events);
    assert_eq!(
        calls
            .iter()
            .map(|(status, _, _)| status.as_str())
            .collect::<Vec<_>>(),
        ["ACTIVE", "READY", "ERROR"]
    );
}

#[test]
fn an_empty_active_window_skips_the_active_query() {
    let (verdict, window, calls) = run_assess(
        "2025-02-03T12:00:00Z",
        "2025-02-03T09:00:00Z",
        "2025-02-03T11:00:00Z",
        &[],
    );
    assert_eq!(window.active_query_start, "2025-02-03T12:00:00Z");
    let Verdict::ErrorOrPending { active, .. } = verdict else {
        panic!("expected ErrorOrPending");
    };
    assert!(!active.has_events);
    assert_eq!(
        active.error.as_deref(),
        Some("Skipped: ACTIVE query window is empty after applying created_at clamp (start=2025-02-03T12:00:00Z, end=2025-02-03T11:00:00Z)")
    );
    assert_eq!(
        calls
            .iter()
            .map(|(status, _, _)| status.as_str())
            .collect::<Vec<_>>(),
        ["READY", "ERROR"]
    );
}

// ───────────────────────────── status rendering ─────────────────────────────

fn subject() -> StatusSubject<'static> {
    StatusSubject {
        location_hash: "0123456789abcdef",
        service: "checkout",
        environment: "prod",
        normalized_type: "BREAKPOINT",
    }
}

fn window() -> TimeWindow {
    TimeWindow {
        created_at: "2025-02-03T10:00:00Z".to_string(),
        requested_start: "2025-02-03T09:00:00Z".to_string(),
        active_query_start: "2025-02-03T10:00:00Z".to_string(),
        query_end: "2025-02-03T11:00:00Z".to_string(),
    }
}

const ACTIVE_HEADER: &str = "CONSOLIDATED STATUS CHECK\n\
\n\
INSTRUMENTATION INFO:\n\
- LocationHash: 0123456789abcdef\n\
- Service: checkout\n\
- Environment: prod\n\
- Type: BREAKPOINT\n\
\n\
TIME RANGE:\n\
- Created At: 2025-02-03T10:00:00Z\n\
- Requested Start: 2025-02-03T09:00:00Z\n\
- ACTIVE Query Start: 2025-02-03T10:00:00Z\n\
- Query End: 2025-02-03T11:00:00Z\n\
\n";

#[test]
fn active_status_lists_snapshot_timestamps_oldest_first() {
    let text = render_consolidated_active_status_output(
        &subject(),
        &window(),
        &found(&["2025-02-03T10:05:00Z", "2025-02-03T10:06:00Z"]),
    );
    let expected = format!(
        "{ACTIVE_HEADER}ACTIVE STATUS:\n\
- Time Window: 2025-02-03T10:00:00Z to 2025-02-03T11:00:00Z\n\
- Confirmed: YES (2 event(s))\n\
\x20 - Event 1: 2025-02-03T10:05:00Z\n\
\x20 - Event 2: 2025-02-03T10:06:00Z\n\
\n\
SNAPSHOT QUERY TIP: Try these timestamps with search_snapshots_for_status_event\n\
\x20 (log group: \"/aws/service-events/checkout\")\n\
\x20 Oldest first \u{2014} older events are more likely to have snapshots ingested:\n\
\x20 - status_timestamp=\"2025-02-03T10:06:00Z\" (oldest, try first)\n\
\x20 - status_timestamp=\"2025-02-03T10:05:00Z\" (most recent)\n\
\n\
OVERALL STATUS: ACTIVE \u{2713} (breakpoint is being hit)\n"
    );
    assert_eq!(text, expected);
}

#[test]
fn active_status_truncates_long_event_lists() {
    let times = [
        "2025-02-03T10:01:00Z",
        "2025-02-03T10:02:00Z",
        "2025-02-03T10:03:00Z",
        "2025-02-03T10:04:00Z",
        "2025-02-03T10:05:00Z",
    ];
    let text = render_consolidated_active_status_output(&subject(), &window(), &found(&times));
    assert!(text.contains("- Confirmed: YES (5 event(s))\n  - Event 1: 2025-02-03T10:01:00Z\n  - Event 2: 2025-02-03T10:02:00Z\n  - Event 3: 2025-02-03T10:03:00Z\n  - ... and 2 more\n"));
    // The tip covers the first five events, newest of those last.
    assert!(text.contains("(oldest, try first)\n"));
    assert!(text.contains("  - status_timestamp=\"2025-02-03T10:03:00Z\"\n"));
    assert!(text.contains("  - status_timestamp=\"2025-02-03T10:01:00Z\" (most recent)\n"));
    // A single event is the oldest and never also "most recent".
    let single = render_consolidated_active_status_output(
        &subject(),
        &window(),
        &found(&["2025-02-03T10:05:00Z"]),
    );
    assert!(single.contains("  - status_timestamp=\"2025-02-03T10:05:00Z\" (oldest, try first)\n"));
    assert!(!single.contains("(most recent)"));
}

#[test]
fn unconfirmed_active_says_so() {
    let text = render_consolidated_active_status_output(&subject(), &window(), &found(&[]));
    assert_eq!(
        text,
        format!(
            "{ACTIVE_HEADER}ACTIVE STATUS:\n- Time Window: 2025-02-03T10:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: NO (no ACTIVE events found)\n\nOVERALL STATUS: ACTIVE not confirmed yet\n"
        )
    );
}

#[test]
fn sections_report_skipped_and_failed_checks() {
    let skipped = StatusCheckResult {
        error: Some("Skipped: window is empty".to_string()),
        ..StatusCheckResult::default()
    };
    let text = render_consolidated_active_status_output(&subject(), &window(), &skipped);
    assert!(text.contains("- Check Skipped: Skipped: window is empty\n"));
    assert!(text.ends_with("\nOVERALL STATUS: ACTIVE not confirmed yet\n"));
    let failed = StatusCheckResult {
        error: Some("API error: throttled".to_string()),
        ..StatusCheckResult::default()
    };
    let text = render_consolidated_active_status_output(&subject(), &window(), &failed);
    assert!(text.contains("- Check Failed: API error: throttled\n"));
    assert!(!text.contains("Confirmed:"));
}

#[test]
fn ready_status_replaces_the_active_verdict_line() {
    let text = render_consolidated_ready_status_output(
        &subject(),
        &window(),
        &found(&[]),
        &found(&["2025-02-03T09:30:00Z"]),
    );
    assert_eq!(
        text,
        format!(
            "{ACTIVE_HEADER}ACTIVE STATUS:\n- Time Window: 2025-02-03T10:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: NO (no ACTIVE events found)\n\nREADY STATUS:\n- Time Window: 2025-02-03T09:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: YES (1 event(s))\n  - Event 1: 2025-02-03T09:30:00Z\n\nOVERALL STATUS: READY (waiting for traffic)\n"
        )
    );
    assert!(!text.contains("ACTIVE not confirmed yet"));
}

fn error_events(causes: &[&str]) -> StatusCheckResult {
    StatusCheckResult {
        has_events: !causes.is_empty(),
        events: causes
            .iter()
            .map(|cause| {
                Json::object(vec![
                    ("Time", Json::string("2025-02-03T09:45:00Z")),
                    ("ErrorCause", Json::string(*cause)),
                ])
            })
            .collect(),
        error: None,
    }
}

#[test]
fn error_status_explains_the_known_causes() {
    let cases = [
        ("FILE_NOT_FOUND", "- Verify file_path is correct\n"),
        (
            "METHOD_NOT_FOUND",
            "- Verify method_name and code_unit are correct\n- Check if the function is loaded at runtime\n",
        ),
        (
            "LINE_NOT_EXECUTABLE",
            "- Verify line_number points to executable code (not comment/blank/declaration)\n",
        ),
        ("SOMETHING_NEW", "- Check instrumentation configuration for SOMETHING_NEW\n"),
    ];
    for (cause, advice) in cases {
        let text = render_consolidated_error_or_pending_status_output(
            &subject(),
            &window(),
            &found(&[]),
            &found(&[]),
            &error_events(&[cause]),
        );
        let expected = format!(
            "{ACTIVE_HEADER}ACTIVE STATUS:\n- Time Window: 2025-02-03T10:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: NO (no ACTIVE events found)\n\n\nREADY STATUS:\n- Time Window: 2025-02-03T09:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: NO (no READY events found)\n\nERROR STATUS:\n- Time Window: 2025-02-03T09:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: YES (1 event(s))\n  - Event 1: 2025-02-03T09:45:00Z | ErrorCause: {cause}\n\nOVERALL STATUS: ERROR ({cause})\n\nTROUBLESHOOTING:\n{advice}"
        );
        assert_eq!(text, expected, "{cause}");
    }
}

#[test]
fn pending_status_waits_for_events() {
    let text = render_consolidated_error_or_pending_status_output(
        &subject(),
        &window(),
        &found(&[]),
        &found(&[]),
        &found(&[]),
    );
    assert!(text.ends_with(
        "\nOVERALL STATUS: PENDING (no ACTIVE, READY, or ERROR events yet - wait longer or check configuration)\n\nNOTE: Status events can take 1-2 minutes to appear after creation.\n"
    ));
    assert!(text.contains("ERROR STATUS:\n- Time Window: 2025-02-03T09:00:00Z to 2025-02-03T11:00:00Z\n- Confirmed: NO (no ERROR events found)\n"));
}

#[test]
fn missing_error_cause_reads_unknown() {
    let bare = StatusCheckResult {
        has_events: true,
        events: vec![Json::object(vec![(
            "Time",
            Json::string("2025-02-03T09:45:00Z"),
        )])],
        error: None,
    };
    let text = render_consolidated_error_or_pending_status_output(
        &subject(),
        &window(),
        &found(&[]),
        &found(&[]),
        &bare,
    );
    assert!(text.contains("  - Event 1: 2025-02-03T09:45:00Z | ErrorCause: Unknown\n"));
    assert!(text.contains("OVERALL STATUS: ERROR (Unknown)\n\nTROUBLESHOOTING:\n- Check instrumentation configuration for Unknown\n"));
}

#[test]
fn verdicts_dispatch_to_their_renderers() {
    let active = found(&["2025-02-03T10:05:00Z"]);
    let ready = found(&["2025-02-03T09:30:00Z"]);
    let nothing = found(&[]);
    assert_eq!(
        render_status_assessment(
            &Verdict::Active {
                active: active.clone()
            },
            &subject(),
            &window()
        ),
        render_consolidated_active_status_output(&subject(), &window(), &active)
    );
    assert_eq!(
        render_status_assessment(
            &Verdict::Ready {
                active: nothing.clone(),
                ready: ready.clone()
            },
            &subject(),
            &window()
        ),
        render_consolidated_ready_status_output(&subject(), &window(), &nothing, &ready)
    );
    assert_eq!(
        render_status_assessment(
            &Verdict::ErrorOrPending {
                active: nothing.clone(),
                ready: nothing.clone(),
                error: nothing.clone()
            },
            &subject(),
            &window()
        ),
        render_consolidated_error_or_pending_status_output(
            &subject(),
            &window(),
            &nothing,
            &nothing,
            &nothing
        )
    );
}

const HISTORY: &str = r#"{"Service":"svc2","Status":"ACTIVE","Events":[{"Time":"2025-02-03T10:05:00Z"},{"Time":"2025-02-03T10:06:00Z","ErrorCause":"X"}],"Location":{"CodeLocation":{"Language":"Python","FilePath":"/a.py","CodeUnit":"m","MethodName":"f"}},"LocationHash":"0123456789abcdef","NextToken":"tok"}"#;

#[test]
fn status_history_confirms_when_events_are_present() {
    let text = render_get_instrumentation_configuration_status_output(
        &json(HISTORY),
        "BREAKPOINT",
        "svc",
        "prod",
        "ACTIVE",
    );
    assert_eq!(
        text,
        "INSTRUMENTATION STATUS\n\
\n\
TYPE: BREAKPOINT\n\
SERVICE: svc2\n\
ENVIRONMENT: prod\n\
SIGNAL TYPE: SNAPSHOT\n\
REQUESTED STATUS FILTER: ACTIVE\n\
CURRENT STATUS: ACTIVE\n\
\n\
LOCATION:\n\
- LocationKind: CODE\n\
- LocationHash: 0123456789abcdef\n\
- Language: Python\n\
- File Path: /a.py\n\
- Code Unit: m\n\
- Method Name: f\n\
\n\
INSTRUMENTATION:\n\
- Level: FUNCTION/METHOD-LEVEL\n\
- Events Returned: 2\n\
- Status Confirmation: CONFIRMED (ACTIVE events present)\n\
- Interpretation Rule: Do not treat CURRENT STATUS as confirmed unless STATUS EVENTS contain entries.\n\
\n\
STATUS EVENTS:\n\
- Event 1: 2025-02-03T10:05:00Z\n\
- Event 2: 2025-02-03T10:06:00Z | ErrorCause: X\n\
\n\
PAGINATION: More results available. Use next_token=\"tok\" to retrieve next page."
    );
}

#[test]
fn status_history_without_events_says_not_confirmed() {
    let text = render_get_instrumentation_configuration_status_output(
        &json(r#"{"Events":[]}"#),
        "PROBE",
        "svc",
        "prod",
        "ACTIVE",
    );
    assert!(text.starts_with("INSTRUMENTATION STATUS\n\nTYPE: PROBE\nSERVICE: svc\nENVIRONMENT: prod\nSIGNAL TYPE: SNAPSHOT\nREQUESTED STATUS FILTER: ACTIVE\nCURRENT STATUS: N/A\n\nLOCATION:\n- LocationKind: UNKNOWN\n- Location payload could not be parsed.\n- Events Returned: 0\n"));
    assert!(text.contains("- Status Confirmation: NOT CONFIRMED (no ACTIVE events)\n"));
    assert!(text.contains("- ACTIVE Clarification: Breakpoint is not confirmed as hit yet."));
    assert!(text.ends_with("\nSTATUS EVENTS:\n- No ACTIVE status events found\n"));

    // The clarification is specific to ACTIVE.
    let ready = render_get_instrumentation_configuration_status_output(
        &json("{}"),
        "PROBE",
        "svc",
        "prod",
        "READY",
    );
    assert!(!ready.contains("ACTIVE Clarification"));
    assert!(ready.ends_with("- No READY status events found\n"));
}

// ───────────────────────────── crud rendering ─────────────────────────────

fn billing_location() -> super::location::Location {
    parse_create_inputs(&LocationInputs {
        language: Some("python"),
        file_path: Some("/app/billing.py"),
        code_unit: Some("services.billing"),
        class_name: Some("Billing"),
        method_name: Some("charge"),
        ..LocationInputs::default()
    })
    .expect("valid location")
    .into_location()
}

fn summary<'a>(
    location: &'a super::location::Location,
    limits: &'a CaptureLimits,
    arguments: Option<&'a [String]>,
) -> CreateSummary<'a> {
    CreateSummary {
        normalized_type: "BREAKPOINT",
        service: "svc",
        environment: "prod",
        location,
        ttl_hours: Some(24),
        capture_arguments: arguments,
        capture_locals: None,
        is_line_level: false,
        capture_return: Some(true),
        capture_stack_trace: Some(true),
        limits,
        attribute_filters: &[],
        now: at("2030-01-01T00:00:00Z"),
    }
}

#[test]
fn create_success_describes_the_configuration() {
    let location = billing_location();
    let limits = CaptureLimits::from_values([Some(5), None, None, None, None, None, None, None]);
    let arguments = vec!["amount".to_string()];
    let text = render_create_success_message(
        &json(
            r#"{"LocationHash":"0123456789abcdef","ARN":"arn:aws:x","CreatedAt":"2025-02-03T10:00:00Z","ExpiresAt":"2025-02-04T10:00:00Z"}"#,
        ),
        &summary(&location, &limits, Some(&arguments)),
    );
    assert_eq!(
        text,
        "Successfully created BREAKPOINT instrumentation\n\
\n\
INSTRUMENTATION CREATED:\n\
- Type: BREAKPOINT\n\
- Service: svc\n\
- Environment: prod\n\
- SignalType: SNAPSHOT\n\
- ARN: arn:aws:x\n\
- CreatedAt: 2025-02-03T10:00:00Z\n\
- Expires: 2025-02-04T10:00:00Z (requested 24 hours)\n\
\n\
LOCATION:\n\
- LocationKind: CODE\n\
- LocationHash: 0123456789abcdef\n\
- Language: Python\n\
- File Path: /app/billing.py\n\
- Code Unit: services.billing\n\
- Class Name: Billing\n\
- Method Name: charge\n\
\n\
INSTRUMENTATION:\n\
- Level: FUNCTION/METHOD-LEVEL\n\
\n\
CAPTURE CONFIGURATION:\n\
- Arguments: amount\n\
- Return Values: Enabled\n\
- Stack Traces: Enabled\n\
\n\
CAPTURE LIMITS:\n\
- Max Hits: 5\n\
\n\
NOTE: Allow ~1-2 min before this configuration reports READY. Status checks immediately after creation may return no events yet \u{2014} wait and re-check rather than recreating.\n\
\n\
TIP: Use this LocationHash to delete: delete_instrumentation(location_hash=\"0123456789abcdef\")"
    );
}

#[test]
fn create_success_falls_back_when_the_response_is_sparse() {
    let location = billing_location();
    let limits = CaptureLimits::unset();
    let mut sparse = summary(&location, &limits, None);
    sparse.ttl_hours = Some(1);
    sparse.normalized_type = "PROBE";
    sparse.capture_return = None;
    sparse.capture_stack_trace = None;
    let attribute_filters = [json(r#"{"service.version":"1.2.0"}"#), json("{}")];
    sparse.attribute_filters = &attribute_filters;
    let text = render_create_success_message(&json("{}"), &sparse);
    assert!(text.contains(
        "- ARN: N/A\n- CreatedAt: 2030-01-01T00:00:00Z\n- Expires: Never (unless deleted)\n"
    ));
    assert!(text.contains("- LocationKind: CODE\n- LocationHash: N/A\n"));
    assert!(text.contains("CAPTURE CONFIGURATION:\n- Arguments: (none)\n- Return Values: Disabled\n- Stack Traces: Disabled\n\nATTRIBUTE FILTERS: 2 filter group(s) applied\n"));
    assert!(!text.contains("CAPTURE LIMITS"));
    assert!(text.contains("NOTE: Allow ~10-12 min before"));
    assert!(text.ends_with("delete_instrumentation(location_hash=\"N/A\")"));

    // One hour is singular, and the suffix needs a requested ttl.
    let text =
        render_create_success_message(&json(r#"{"ExpiresAt":"2025-02-04T10:00:00Z"}"#), &sparse);
    assert!(text.contains("- Expires: 2025-02-04T10:00:00Z (requested 1 hour)\n"));
    sparse.ttl_hours = None;
    let text =
        render_create_success_message(&json(r#"{"ExpiresAt":"2025-02-04T10:00:00Z"}"#), &sparse);
    assert!(text.contains("- Expires: 2025-02-04T10:00:00Z\n"));
}

#[test]
fn line_level_create_shows_locals_instead_of_arguments() {
    let location = billing_location();
    let limits = CaptureLimits::unset();
    let locals = vec!["total".to_string(), "tax".to_string()];
    let mut line_level = summary(&location, &limits, None);
    line_level.is_line_level = true;
    line_level.capture_locals = Some(&locals);
    let text =
        render_create_success_message(&json(r#"{"LocationHash":"0123456789abcdef"}"#), &line_level);
    assert!(text.contains(
        "CAPTURE CONFIGURATION:\n- Local Variables: total, tax\n- Stack Traces: Enabled\n"
    ));
    assert!(!text.contains("- Arguments:"));
    assert!(!text.contains("- Return Values:"));
}

#[test]
fn list_output_for_no_results_and_one_configuration() {
    assert_eq!(
        render_list_instrumentations_output(&json("{}"), "BREAKPOINT", "svc", "prod"),
        "No active BREAKPOINT instrumentations found\n\nService: svc\nEnvironment: prod\n\nTIP: Use create_instrumentation to add instrumentations."
    );
    let data = json(
        r#"{"SyncedAt":"2025-02-03T10:00:00Z","LatestConfigurations":[{"LocationHash":"0123456789abcdef","Location":{"CodeLocation":{"Language":"Java","FilePath":"A.java","CodeUnit":"com.x","ClassName":"A","MethodName":"m","LineNumber":7}},"CaptureConfiguration":{"CodeCapture":{"CaptureReturn":true,"CaptureStackTrace":false,"CaptureArguments":["a","b"],"CaptureLocals":[],"CaptureLimits":{"MaxHits":3,"MaxStringLength":100,"MaxStackFrames":9}}},"CreatedAt":"2025-02-03T09:00:00Z","Description":"d","ARN":"arn1"}],"NextToken":"n1"}"#,
    );
    let rule = "=".repeat(60);
    let expected = format!(
        "Active BREAKPOINT Instrumentations (1 found)\n\
\n\
Service: svc\n\
Environment: prod\n\
Synced At: 2025-02-03T10:00:00Z\n\
\n\
{rule}\n\
INSTRUMENTATION #1\n\
{rule}\n\
LOCATION:\n\
- LocationKind: CODE\n\
- LocationHash: 0123456789abcdef\n\
- Language: Java\n\
- File Path: A.java\n\
- Code Unit: com.x\n\
- Class Name: A\n\
- Method Name: m\n\
- Line Number: 7\n\
\n\
INSTRUMENTATION:\n\
- Level: LINE-LEVEL (L7)\n\
\n\
CAPTURE SETTINGS:\n\
- Return: Enabled\n\
- Stack Traces: Disabled\n\
- Arguments: a, b\n\
- Locals: (empty list)\n\
- Limits: MaxHits=3, MaxStringLen=100\n\
\n\
TIMING:\n\
- Created: 2025-02-03T09:00:00Z\n\
- Expires: Never\n\
\n\
Description: d\n\
ARN: arn1\n\
\n\
\n\
PAGINATION: More results available. Use next_token=\"n1\" to retrieve next page."
    );
    assert_eq!(
        render_list_instrumentations_output(&data, "BREAKPOINT", "svc", "prod"),
        expected
    );
}

#[test]
fn list_output_handles_unparseable_captures_and_missing_fields() {
    let text = render_list_instrumentations_output(
        &json(
            r#"{"LatestConfigurations":[{"Location":{},"CaptureConfiguration":{}},{"CaptureConfiguration":{"CodeCapture":{}}}]}"#,
        ),
        "PROBE",
        "svc",
        "prod",
    );
    assert!(text.starts_with("Active PROBE Instrumentations (2 found)\n\nService: svc\nEnvironment: prod\nSynced At: N/A\n\n"));
    assert!(text.contains("- LocationKind: UNKNOWN\n- Location payload could not be parsed.\n\nCAPTURE SETTINGS:\n- Capture payload could not be parsed.\n\nTIMING:\n- Created: N/A\n- Expires: Never\n\nDescription: N/A\nARN: N/A\n\n"));
    assert!(text.contains("INSTRUMENTATION #2\n"));
    assert!(text.contains("- Return: Disabled\n- Stack Traces: Disabled\n- Arguments: (not set)\n- Locals: (not set)\n"));
    assert!(!text.contains("- Limits:"));
    assert!(!text.contains("PAGINATION"));
}

#[test]
fn get_output_lists_capture_limits_and_filters() {
    let config = json(
        r#"{"InstrumentationType":"BREAKPOINT","LocationHash":"0123456789abcdef","Location":{"CodeLocation":{"Language":"Python","FilePath":"/a.py","CodeUnit":"m","MethodName":"f"}},"CaptureConfiguration":{"CodeCapture":{"CaptureReturn":false,"CaptureStackTrace":true,"CaptureLimits":{"MaxHits":2,"MaxFieldsPerObject":4}}},"AttributeFilters":[{"service.version":"1.2.0"}],"Description":"desc","CreatedAt":"2025-02-03T09:00:00Z","ExpiresAt":"2025-02-04T09:00:00Z","ARN":"arn1"}"#,
    );
    assert_eq!(
        render_get_instrumentation_output(&config, "svc", "prod"),
        "INSTRUMENTATION CONFIGURATION\n\
\n\
TYPE: BREAKPOINT\n\
SERVICE: svc\n\
ENVIRONMENT: prod\n\
SIGNAL TYPE: SNAPSHOT\n\
\n\
LOCATION:\n\
- LocationKind: CODE\n\
- LocationHash: 0123456789abcdef\n\
- Language: Python\n\
- File Path: /a.py\n\
- Code Unit: m\n\
- Method Name: f\n\
\n\
INSTRUMENTATION:\n\
- Level: FUNCTION/METHOD-LEVEL\n\
\n\
CAPTURE CONFIGURATION:\n\
- Return Values: Disabled\n\
- Stack Traces: Enabled\n\
- Arguments: (not set)\n\
- Local Variables: (not set)\n\
\n\
CAPTURE LIMITS:\n\
- Max Hits: 2\n\
- Max Fields Per Object: 4\n\
\n\
ATTRIBUTE FILTERS: 1 filter group(s)\n\
\x20 Group 1: {'service.version': '1.2.0'}\n\
\n\
METADATA:\n\
- Description: desc\n\
- Created: 2025-02-03T09:00:00Z\n\
- Expires: 2025-02-04T09:00:00Z\n\
- ARN: arn1\n"
    );
}

#[test]
fn get_output_for_a_bare_configuration() {
    let text = render_get_instrumentation_output(
        &json(r#"{"CaptureConfiguration":{"x":1}}"#),
        "svc",
        "prod",
    );
    assert!(text.starts_with("INSTRUMENTATION CONFIGURATION\n\nTYPE: N/A\nSERVICE: svc\nENVIRONMENT: prod\nSIGNAL TYPE: SNAPSHOT\n\nLOCATION:\n- LocationKind: UNKNOWN\n- Location payload could not be parsed.\n\nCAPTURE CONFIGURATION:\n- Capture payload could not be parsed.\n\nMETADATA:\n- Description: N/A\n- Created: N/A\n- Expires: Never\n- ARN: N/A\n"));
    assert!(!text.contains("ATTRIBUTE FILTERS"));
}

#[test]
fn batch_delete_summarizes_successes_and_errors() {
    let data = json(
        r#"{"DeletedCount":2,"SuccessfulDeletions":[{"ResourceArn":"arn:1"},{"SignalType":"SNAPSHOT","LocationHash":"0123456789abcdef"},{}],"Errors":[{"ResourceArn":"arn:2","Code":"NotFound","Message":"gone"},{}]}"#,
    );
    assert_eq!(
        format_batch_delete_response("Scope", &data, "BREAKPOINT", Some("svc"), Some("prod")),
        "BATCH DELETE COMPLETED\n\
\n\
Mode: Scope\n\
InstrumentationType: BREAKPOINT\n\
DeletedCount: 2\n\
SuccessfulDeletions: 3\n\
Errors: 2\n\
Service: svc\n\
Environment: prod\n\
\n\
SUCCESSFUL DELETIONS:\n\
- Item 1: ResourceArn=arn:1\n\
- Item 2: SignalType=SNAPSHOT | LocationHash=0123456789abcdef\n\
- Item 3: SignalType=N/A | LocationHash=N/A\n\
\n\
DELETE ERRORS:\n\
- Item 1: ResourceArn=arn:2 | Code=NotFound | Message=gone\n\
- Item 2: ResourceArn=N/A | Code=N/A | Message=N/A\n"
    );
    assert_eq!(
        format_batch_delete_response("ResourceArns", &json("{}"), "PROBE", None, Some("")),
        "BATCH DELETE COMPLETED\n\nMode: ResourceArns\nInstrumentationType: PROBE\nDeletedCount: 0\nSuccessfulDeletions: 0\nErrors: 0\n"
    );
}

#[test]
fn locations_round_trip_through_the_renderers() {
    // A created code location renders the same as the API echoing it back.
    let created = billing_location();
    let echoed = location_from_response(&json(
        r#"{"CodeLocation":{"Language":"Python","FilePath":"/app/billing.py","CodeUnit":"services.billing","ClassName":"Billing","MethodName":"charge"}}"#,
    ));
    assert_eq!(created, echoed);
}

// ───────────────────────────── command surface ─────────────────────────────

fn run_cli(args: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = crate::run(&args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

#[test]
fn render_location_prints_the_block() {
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "location",
        "--json",
        r#"{"location":{"CodeLocation":{"Language":"Python","FilePath":"/a.py"}},"location_hash":"0123456789abcdef"}"#,
    ]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(err, "");
    assert_eq!(
        out,
        "- LocationKind: CODE\n- LocationHash: 0123456789abcdef\n- Language: Python\n- File Path: /a.py\n\nINSTRUMENTATION:\n- Level: FUNCTION/METHOD-LEVEL\n\n"
    );
}

#[test]
fn render_accepts_value_flags_in_both_spellings() {
    let json_text = r#"{"location":{"Foo":"bar"}}"#;
    let (status, spaced, _) = run_cli(&["cloudwatch", "render", "location", "--json", json_text]);
    assert_eq!(status, 0);
    let (status, joined, _) = run_cli(&[
        "cloudwatch",
        "render",
        "location",
        &format!("--json={json_text}"),
    ]);
    assert_eq!(status, 0);
    assert_eq!(spaced, joined);
    assert_eq!(spaced, "- LocationKind: UNKNOWN\n- Foo: bar\n\n");
}

#[test]
fn render_reads_arguments_from_a_file() {
    let path = std::env::temp_dir().join(format!(
        "toolkit-cloudwatch-args-{}-{:?}.json",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(
        &path,
        r#"{"data":{"Events":[]},"normalized_type":"PROBE","service":"svc","environment":"prod","requested_status":"READY"}"#,
    )
    .expect("write arguments file");
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "status-history",
        "--json-file",
        path.to_str().expect("utf-8 path"),
    ]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("REQUESTED STATUS FILTER: READY\n"));
    assert!(out.ends_with("- No READY status events found\n\n"));
}

#[test]
fn render_argument_errors_are_usage_errors() {
    let cases: [(&[&str], &str); 7] = [
        (&["cloudwatch", "render"], "an op is required"),
        (
            &["cloudwatch", "render", "bogus", "--json", "{}"],
            "invalid choice: 'bogus'",
        ),
        (
            &["cloudwatch", "render", "location"],
            "the op's arguments are required",
        ),
        (
            &[
                "cloudwatch",
                "render",
                "location",
                "--json",
                "{}",
                "--json-file",
                "x.json",
            ],
            "exactly one of --json or --json-file",
        ),
        (
            &["cloudwatch", "render", "location", "--json", "{oops"],
            "arguments are not valid JSON",
        ),
        (
            &["cloudwatch", "render", "location", "--json", "[1]"],
            "arguments must be a JSON object",
        ),
        (
            &[
                "cloudwatch",
                "render",
                "location",
                "--json-file",
                "/nonexistent/args.json",
            ],
            "--json-file could not be read",
        ),
    ];
    for (args, message) in cases {
        let (status, out, err) = run_cli(args);
        assert_eq!(status, 2, "{args:?}: {err}");
        assert_eq!(out, "", "{args:?}");
        assert!(err.contains(message), "{args:?}: {err}");
    }
}

#[test]
fn strict_value_flags_reject_flag_looking_values() {
    let (status, out, err) =
        run_cli(&["cloudwatch", "render", "location", "--json", "--json-file"]);
    assert_eq!(status, 2);
    assert_eq!(out, "");
    assert!(err.starts_with("error: invalid arguments\n"), "{err}");
    let (status, _, _) = run_cli(&["cloudwatch", "region", "--region", ""]);
    assert_eq!(status, 2);
}

#[test]
fn mistyped_and_unknown_arguments_keep_the_legacy_exit_status_two() {
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "location",
        "--json",
        r#"{"location":{},"extra":1}"#,
    ]);
    assert_eq!(status, 2);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "error: invalid arguments for op 'location': got an unexpected argument 'extra'\n"
    );

    let (status, _, err) = run_cli(&[
        "cloudwatch",
        "render",
        "list",
        "--json",
        r#"{"data":{},"normalized_type":"BREAKPOINT","service":5,"environment":"prod"}"#,
    ]);
    assert_eq!(status, 2);
    assert_eq!(
        err,
        "error: invalid arguments for op 'list': argument 'service' must be a string, got integer\n"
    );

    let (status, _, err) = run_cli(&[
        "cloudwatch",
        "render",
        "get",
        "--json",
        r#"{"config":{},"service":"svc"}"#,
    ]);
    assert_eq!(status, 2);
    assert_eq!(
        err,
        "error: invalid arguments for op 'get': missing required argument 'environment'\n"
    );
}

#[test]
fn status_assessment_replays_recorded_checks() {
    let arguments = r#"{
        "created_at":"2025-02-03T10:00:00Z","requested_start":"2025-02-03T09:00:00Z","query_end":"2025-02-03T11:00:00Z",
        "location_hash":"0123456789abcdef","service":"checkout","environment":"prod","normalized_type":"BREAKPOINT",
        "checks":{"ACTIVE":{"events":[{"Time":"2025-02-03T10:05:00Z"},{"Time":"2025-02-03T10:06:00Z"}]}}
    }"#;
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "status-assessment",
        "--json",
        arguments,
    ]);
    assert_eq!(status, 0, "{err}");
    let expected = render_consolidated_active_status_output(
        &subject(),
        &window(),
        &found(&["2025-02-03T10:05:00Z", "2025-02-03T10:06:00Z"]),
    );
    assert_eq!(out, format!("{expected}\n"));
}

#[test]
fn status_assessment_reports_missing_checks_and_bad_windows() {
    let base = |checks: &str, end: &str| {
        format!(
            r#"{{"created_at":"2025-02-03T10:00:00Z","requested_start":"2025-02-03T09:00:00Z","query_end":"{end}","location_hash":"0123456789abcdef","service":"checkout","environment":"prod","normalized_type":"BREAKPOINT","checks":{checks}}}"#
        )
    };
    // READY is needed after an unconfirmed ACTIVE but was not recorded.
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "status-assessment",
        "--json",
        &base(r#"{"ACTIVE":{}}"#, "2025-02-03T11:00:00Z"),
    ]);
    assert_eq!(status, 2);
    assert_eq!(out, "");
    assert!(err.contains("no 'READY' entry"), "{err}");

    // end_time must be later than start_time: the operation's own failure.
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "status-assessment",
        "--json",
        &base("{}", "2025-02-03T09:00:00Z"),
    ]);
    assert_eq!(status, 1);
    assert_eq!(out, "ERROR: end_time must be later than start_time\n");
    assert_eq!(err, "");

    let (status, _, err) = run_cli(&[
        "cloudwatch",
        "render",
        "status-assessment",
        "--json",
        &base("{}", "later"),
    ]);
    assert_eq!(status, 2);
    assert!(
        err.contains("argument 'query_end': Invalid isoformat string: 'later'"),
        "{err}"
    );
}

#[test]
fn snapshot_renderers_and_fields_run_offline() {
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "snapshot-search",
        "--json",
        r#"{"query_result":{"status":"Error","error":"boom"},"service_name":"svc","environment":"prod","location_hash":"0123456789abcdef","start_time_utc":"a","end_time_utc":"b","start_epoch":1,"end_epoch":2,"query_string":"q"}"#,
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("\"status\": \"ERROR\""));
    assert!(out.ends_with("\"error\": \"boom\"\n}\n"));

    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "snapshot-sample",
        "--json",
        r#"{"query_result":{"status":"Complete","queryId":"q","results":[]},"service_name":"svc","environment":"prod","location_hash":"0123456789abcdef","start_time_utc":"a","end_time_utc":"b","query_string":"q"}"#,
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("\"status\": \"NO_SNAPSHOTS_FOUND\""));

    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "snapshot-fields",
        "--json",
        r#"{"result":{"@timestamp":"t","@message":"{\"traceId\":\"t1\"}"}}"#,
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("\"trace\": {\n    \"traceId\": \"t1\",\n    \"spanId\": null\n  }"));

    let (status, _, err) = run_cli(&[
        "cloudwatch",
        "render",
        "snapshot-search",
        "--json",
        r#"{"query_result":{"status":1},"service_name":"s","environment":"e","location_hash":"h","start_time_utc":"a","end_time_utc":"b","start_epoch":1,"end_epoch":2,"query_string":"q"}"#,
    ]);
    assert_eq!(status, 2);
    assert!(
        err.contains("query_result.status must be a string"),
        "{err}"
    );
}

#[test]
fn create_success_renders_through_the_command() {
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "render",
        "create-success",
        "--json",
        r#"{"response":{"LocationHash":"0123456789abcdef","ARN":"arn:x","CreatedAt":"2025-02-03T10:00:00Z"},"normalized_type":"BREAKPOINT","service":"svc","environment":"prod","location":{"CodeLocation":{"Language":"Python","FilePath":"/a.py","CodeUnit":"m","MethodName":"f"}},"capture_arguments":["a","b"],"capture_return":true,"capture_stack_trace":false,"limits":{"MaxHits":3},"ttl_hours":2,"now":"2030-01-01T00:00:00Z"}"#,
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("- CreatedAt: 2025-02-03T10:00:00Z\n- Expires: Never (unless deleted)\n"));
    assert!(out.contains("CAPTURE CONFIGURATION:\n- Arguments: a, b\n- Return Values: Enabled\n- Stack Traces: Disabled\n\nCAPTURE LIMITS:\n- Max Hits: 3\n"));
}

#[test]
fn failure_renderer_mirrors_the_gateway_dispatch() {
    let base = r#""failure":{"kind":"client_error","code":"Throttling","message":"slow down"},"action":"get instrumentation","attempted":{"Target":"a.py"}"#;
    let (status, canned, err) = run_cli(&[
        "cloudwatch",
        "render",
        "failure",
        "--json",
        &format!("{{{base}}}"),
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(canned.contains("Error: Throttling - slow down\n\nATTEMPTED PARAMETERS:\n- Target: a.py\n\nPOSSIBLE CAUSES:\n1. Invalid input parameters"));

    let (status, tailored, err) = run_cli(&[
        "cloudwatch",
        "render",
        "failure",
        "--json",
        &format!(
            r#"{{{base},"attempted_label":"ATTEMPTED TO RETRIEVE:","possible_causes":["one"],"trailer":"TRAILER"}}"#
        ),
    ]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        tailored,
        "Failed to get instrumentation\n\nError: Throttling - slow down\n\nATTEMPTED TO RETRIEVE:\n- Target: a.py\n\nPOSSIBLE CAUSES:\n1. one\n\nTRAILER\n"
    );

    let (status, _, err) = run_cli(&[
        "cloudwatch",
        "render",
        "failure",
        "--json",
        r#"{"failure":{"kind":"mystery"},"action":"x"}"#,
    ]);
    assert_eq!(status, 2);
    assert!(
        err.contains("argument 'failure.kind' must be one of"),
        "{err}"
    );
}

#[test]
fn validate_reports_failures_on_stdout_with_status_one() {
    let (status, out, err) = run_cli(&[
        "cloudwatch",
        "validate",
        "location-hash",
        "--json",
        r#"{"location_hash":"NOT-A-HASH"}"#,
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        out,
        "ERROR: location_hash must be a 16-character hex string\n"
    );
    assert_eq!(err, "");

    let (status, out, _) = run_cli(&[
        "cloudwatch",
        "validate",
        "location-hash",
        "--json",
        r#"{"location_hash":"0123456789abcdef"}"#,
    ]);
    assert_eq!((status, out.as_str()), (0, "OK\n"));
}

#[test]
fn validate_checks_cover_the_input_helpers() {
    let run = |check: &str, arguments: &str| {
        run_cli(&["cloudwatch", "validate", check, "--json", arguments])
    };

    let (status, out, _) = run(
        "instrumentation-type",
        r#"{"instrumentation_type":" probe "}"#,
    );
    assert_eq!((status, out.as_str()), (0, "PROBE\n"));
    let (status, out, _) = run(
        "instrumentation-type",
        r#"{"instrumentation_type":"trace"}"#,
    );
    assert_eq!(status, 1);
    assert_eq!(
        out,
        "ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: trace)\n"
    );
    let (status, out, _) = run("instrumentation-type", "{}");
    assert_eq!(status, 1);
    assert!(out.ends_with("(received: None)\n"));

    let (status, out, _) = run(
        "capture-names",
        r#"{"field_name":"capture_locals","names":["*"]}"#,
    );
    assert_eq!(status, 1);
    assert!(out.starts_with("ERROR: capture_locals does not support the wildcard"));
    let (status, out, _) = run("capture-names", r#"{"field_name":"capture_locals"}"#);
    assert_eq!((status, out.as_str()), (0, "OK\n"));

    let (status, out, _) = run(
        "probe",
        r#"{"instrumentation_type":"PROBE","language":"JavaScript"}"#,
    );
    assert_eq!(status, 1);
    assert!(out.starts_with("ERROR: PROBE is not supported for JavaScript."));
    let (status, out, _) = run(
        "probe",
        r#"{"instrumentation_type":"BREAKPOINT","language":"JavaScript","line_number":4}"#,
    );
    assert_eq!((status, out.as_str()), (0, "OK\n"));
    let (status, out, _) = run("probe", r#"{"instrumentation_type":"nope"}"#);
    assert_eq!(status, 1);
    assert!(out.starts_with("ERROR: instrumentation_type must be one of"));

    let (status, out, _) = run("snapshot-signal", r#"{"signal_type":"TRACE"}"#);
    assert_eq!(status, 1);
    assert_eq!(
        out,
        "ERROR: signal_type must be SNAPSHOT for this API (received: TRACE)\n"
    );
    let (status, out, _) = run("snapshot-signal", r#"{"signal_type":"snapshot"}"#);
    assert_eq!((status, out.as_str()), (0, "OK\n"));
}

#[test]
fn validate_location_inputs_prints_the_payload_or_the_legacy_error() {
    let run = |arguments: &str| {
        run_cli(&[
            "cloudwatch",
            "validate",
            "location-inputs",
            "--json",
            arguments,
        ])
    };

    let (status, out, err) = run(
        r#"{"language":"java","file_path":"A.java","code_unit":"com.x","class_name":"A","method_name":"m","line_number":7}"#,
    );
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        out,
        "{\n  \"CodeLocation\": {\n    \"Language\": \"Java\",\n    \"FilePath\": \"A.java\",\n    \"CodeUnit\": \"com.x\",\n    \"ClassName\": \"A\",\n    \"MethodName\": \"m\",\n    \"LineNumber\": 7\n  }\n}\n"
    );

    let (status, out, _) = run(r#"{"language":"Python"}"#);
    assert_eq!(status, 1);
    assert!(out.starts_with("ERROR: BREAKPOINT/PROBE require language and file_path.\n"));

    let (status, out, _) = run(r#"{"language":"Python","file_path":"/a.py"}"#);
    assert_eq!(status, 1);
    assert!(out.starts_with("Invalid breakpoint location inputs:\n1. Python requires code_unit"));

    let (status, out, _) = run(r#"{"mode":"lookup","location_hash":"0123456789abcdef"}"#);
    assert_eq!(status, 0);
    assert_eq!(out, "{\n  \"LocationHash\": \"0123456789abcdef\"\n}\n");

    let (status, out, _) = run(r#"{"mode":"lookup"}"#);
    assert_eq!(status, 1);
    assert_eq!(
        out,
        "ERROR: missing location identifier input. Provide location_hash OR language+file_path (code location).\n"
    );

    let (status, out, _) = run(
        r#"{"mode":"lookup","language":"Python","file_path":"/a.py","allow_code_location_lookup":false}"#,
    );
    assert_eq!(status, 1);
    assert_eq!(
        out,
        "ERROR: code location lookup is not supported for this operation.\n"
    );

    let (status, _, err) = run(r#"{"mode":"delete"}"#);
    assert_eq!(status, 2);
    assert!(
        err.contains("argument 'mode' must be create or lookup"),
        "{err}"
    );
}

#[test]
fn region_command_resolves_the_flag_first() {
    let (status, out, err) = run_cli(&["cloudwatch", "region", "--region", "eu-west-1"]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(out, "eu-west-1\n");
    let (status, out, _) = run_cli(&["cloudwatch", "region", "--region=ap-south-1"]);
    assert_eq!((status, out.as_str()), (0, "ap-south-1\n"));
}
