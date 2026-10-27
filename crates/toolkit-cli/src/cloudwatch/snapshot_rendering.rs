//! Formatting of snapshot tool responses (`di_snapshot_rendering.py`).
//!
//! Both renderers return `json.dumps(..., indent=2)` text: key order,
//! `ensure_ascii` escaping and the failure shapes (`ERROR`, `TIMEOUT`, an
//! unexpected non-`Complete` status) are the legacy ones. The Logs Insights
//! query itself (`_execute_cloudwatch_query`) is the live client's job
//! (#4381); these renderers consume its result shape, [`QueryResult`].

use toolkit_core::{parse_json, Json};

use super::consts::resolve_snapshot_log_group;
use super::pytext::{dumps_indent2, group_thousands, string_array};
use super::snapshot_parsing::parse_snapshot_fields;

/// Raw snapshots larger than this are replaced with the parsed summary.
pub const RAW_SNAPSHOT_SIZE_THRESHOLD: usize = 10 * 1024;

/// The dict `_execute_cloudwatch_query` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    /// `Complete`, `Failed`, `Cancelled`, `Error` or `Polling Timeout`.
    pub status: String,
    /// `queryId` (`null` when the query never started).
    pub query_id: Json,
    /// `error`, set for `Error` and `Polling Timeout`.
    pub error: Option<String>,
    /// One `{field: value}` object per result row.
    pub results: Vec<Json>,
    /// `messages` from the Logs API.
    pub messages: Vec<Json>,
}

impl QueryResult {
    /// Reads the JSON shape of a query result.
    ///
    /// # Errors
    ///
    /// A description when the value is not an object with a string
    /// `status`, or when `results` / `messages` are present but not arrays.
    pub fn from_json(value: &Json) -> Result<Self, String> {
        let Json::Object(_) = value else {
            return Err("query_result must be a JSON object".to_string());
        };
        let status = match value.get("status") {
            Some(Json::String(status)) => status.clone(),
            _ => return Err("query_result.status must be a string".to_string()),
        };
        let list = |key: &str| -> Result<Vec<Json>, String> {
            match value.get(key) {
                None | Some(Json::Null) => Ok(Vec::new()),
                // recorded-shape: caller input of `cloudwatch render` (#4380); not a response.
                Some(Json::Array(items)) => Ok(items.clone()),
                Some(_) => Err(format!("query_result.{key} must be an array")),
            }
        };
        Ok(Self {
            status,
            query_id: value.get("queryId").cloned().unwrap_or(Json::Null),
            error: match value.get("error") {
                Some(Json::String(text)) => Some(text.clone()),
                _ => None,
            },
            results: list("results")?,
            messages: list("messages")?,
        })
    }
}

/// Inputs of the snapshot-search renderer that are not part of the query
/// result.
#[derive(Debug, Clone, Copy)]
pub struct SearchContext<'a> {
    /// `service_name`.
    pub service_name: &'a str,
    /// `environment`.
    pub environment: &'a str,
    /// `location_hash`.
    pub location_hash: &'a str,
    /// `custom_filters` (empty when none were given).
    pub custom_filters: &'a [String],
    /// `start_time_utc`.
    pub start_time_utc: &'a str,
    /// `end_time_utc`.
    pub end_time_utc: &'a str,
    /// `start_epoch`.
    pub start_epoch: i64,
    /// `end_epoch`.
    pub end_epoch: i64,
    /// `query_string`.
    pub query_string: &'a str,
}

/// Inputs of the sample-snapshot renderer that are not part of the query
/// result.
#[derive(Debug, Clone, Copy)]
pub struct SampleContext<'a> {
    /// `service_name`.
    pub service_name: &'a str,
    /// `environment`.
    pub environment: &'a str,
    /// `location_hash`.
    pub location_hash: &'a str,
    /// `start_time_utc`.
    pub start_time_utc: &'a str,
    /// `end_time_utc`.
    pub end_time_utc: &'a str,
    /// `max_timeout` in seconds.
    pub max_timeout: i64,
    /// `query_string`.
    pub query_string: &'a str,
    /// `include_raw`.
    pub include_raw: bool,
}

fn messages_json(query_result: &QueryResult) -> Json {
    Json::Array(query_result.messages.clone())
}

/// Parses a `@message` the way the renderers do: anything that is not a
/// JSON object reads as an empty snapshot.
fn snapshot_object(message: Option<&Json>) -> Json {
    match message {
        Some(Json::String(text)) => match parse_json(text) {
            // recorded-shape: the snapshot body a log event carries (#4380); not a response member.
            Ok(parsed @ Json::Object(_)) => parsed,
            _ => Json::Object(Vec::new()),
        },
        _ => Json::Object(Vec::new()),
    }
}

/// Renders the snapshot-search response as JSON text.
#[must_use]
pub fn render_search_snapshots_for_status_event_output(
    context: &SearchContext<'_>,
    query_result: &QueryResult,
) -> String {
    let log_group_name = resolve_snapshot_log_group(context.service_name);
    let filters = string_array(context.custom_filters);
    let service = Json::string(context.service_name);
    let environment = Json::string(context.environment);
    let log_group = Json::string(log_group_name);
    let hash = Json::string(context.location_hash);
    let query_string = Json::string(context.query_string);

    if query_result.status == "Error" {
        return dumps_indent2(&Json::object(vec![
            ("status", Json::string("ERROR")),
            ("service_name", service),
            ("environment", environment),
            ("log_group_name", log_group),
            ("location_hash", hash),
            ("custom_filters", filters),
            ("start_time_utc", Json::string(context.start_time_utc)),
            ("end_time_utc", Json::string(context.end_time_utc)),
            ("query_string", query_string),
            (
                "error",
                Json::string(query_result.error.as_deref().unwrap_or("Unknown error")),
            ),
        ]));
    }

    if query_result.status == "Polling Timeout" {
        return dumps_indent2(&Json::object(vec![
            ("queryId", query_result.query_id.clone()),
            ("status", Json::string("TIMEOUT")),
            ("log_group_name", log_group),
            ("service_name", service),
            ("environment", environment),
            ("location_hash", hash),
            ("custom_filters", filters),
            ("start_time_utc", Json::string(context.start_time_utc)),
            ("end_time_utc", Json::string(context.end_time_utc)),
            ("query_string", query_string),
            (
                "message",
                Json::string(
                    "Query did not complete within the requested timeout. \
                     Use get-query-results with the returned queryId to retry.",
                ),
            ),
        ]));
    }

    if query_result.status != "Complete" {
        // Failed/Cancelled (or any unexpected non-Complete) status: surface
        // it instead of an empty-but-success-shaped response that would be
        // indistinguishable from "completed, zero snapshots".
        return dumps_indent2(&Json::object(vec![
            ("status", Json::string(query_result.status.as_str())),
            ("queryId", query_result.query_id.clone()),
            ("service_name", service),
            ("environment", environment),
            ("log_group_name", log_group),
            ("location_hash", hash),
            ("query_string", query_string),
            ("messages", messages_json(query_result)),
        ]));
    }

    let summaries: Vec<Json> = query_result
        .results
        .iter()
        .map(|result| {
            let snapshot = snapshot_object(result.get("@message"));
            let attributes = match snapshot.get("attributes") {
                // recorded-shape: the snapshot body a log event carries (#4380); not a response member.
                Some(attributes @ Json::Object(_)) => attributes.clone(),
                _ => Json::Object(Vec::new()),
            };
            Json::object(vec![
                (
                    "@timestamp",
                    result.get("@timestamp").cloned().unwrap_or(Json::Null),
                ),
                (
                    "snapshot_id",
                    attributes
                        .get("aws.di.snapshot_id")
                        .cloned()
                        .unwrap_or(Json::Null),
                ),
                (
                    "location_hash",
                    attributes
                        .get("aws.di.location_hash")
                        .cloned()
                        .unwrap_or(Json::Null),
                ),
                (
                    "traceId",
                    snapshot.get("traceId").cloned().unwrap_or(Json::Null),
                ),
                (
                    "spanId",
                    snapshot.get("spanId").cloned().unwrap_or(Json::Null),
                ),
            ])
        })
        .collect();

    dumps_indent2(&Json::object(vec![
        ("queryId", query_result.query_id.clone()),
        ("status", Json::string(query_result.status.as_str())),
        ("log_group_name", log_group),
        ("service_name", service),
        ("environment", environment),
        ("location_hash", hash),
        ("custom_filters", filters),
        ("start_time_utc", Json::string(context.start_time_utc)),
        ("end_time_utc", Json::string(context.end_time_utc)),
        ("start_epoch", Json::Int(context.start_epoch)),
        ("end_epoch", Json::Int(context.end_epoch)),
        ("query_string", query_string),
        ("messages", messages_json(query_result)),
        ("snapshot_summaries", Json::Array(summaries)),
        ("results", Json::Array(query_result.results.clone())),
    ]))
}

/// The `field_documentation` block of a successful sample response.
const FIELD_DOCUMENTATION: [(&str, &str); 17] = [
    (
        "attributes.aws.di.snapshot_id",
        "Unique snapshot identifier (UUID v4).",
    ),
    (
        "timeUnixNano",
        "Snapshot timestamp in nanoseconds since Unix epoch.",
    ),
    (
        "attributes.aws.di.duration_ms",
        "Function execution duration in milliseconds. \
         Present for method-level breakpoints only; absent for line-level.",
    ),
    (
        "resource.attributes.service.name",
        "Service name from OTel resource.",
    ),
    (
        "resource.attributes.deployment.environment",
        "Deployment environment from OTel resource (legacy semconv key used by the Python agent \
         and the Java agent's fallback path). Filter on both this key and \
         resource.attributes.deployment.environment.name to cover every agent path.",
    ),
    (
        "resource.attributes.deployment.environment.name",
        "Deployment environment under the modern semconv key. The Java agent emits this via \
         OTel autoconfiguration / OTEL_RESOURCE_ATTRIBUTES.",
    ),
    (
        "attributes.aws.di.location_hash",
        "Breakpoint identifier. Use in filters: attributes.aws.di.location_hash = \"<value>\"",
    ),
    (
        "attributes.aws.di.*",
        "Breakpoint location metadata: code_unit, class_name, method_name, file_path, \
         instrumentation_level, instrumentation_type.",
    ),
    (
        "traceId",
        "OpenTelemetry trace ID (hex, 32 chars). Use to filter snapshots from the same request: \
         traceId = \"<value>\"",
    ),
    (
        "spanId",
        "OpenTelemetry span ID (hex, 16 chars). Use with traceId for precise span correlation.",
    ),
    (
        "body.stack",
        "Call stack frames (file_path, function, line_number), top to bottom. \
         First few frames are DI internals; application frames follow after.",
    ),
    (
        "body.captures.entry.arguments.<name>",
        "Input arguments at function entry (method-level only). \
         Filter: @message like /\"arguments\"/ and @message like /\"<name>\"/",
    ),
    (
        "body.captures.entry.locals.<name>",
        "Local variables at function entry (method-level only).",
    ),
    (
        "body.captures.return.return_value",
        "Function return value (method-level only). \
         Filter: @message like /\"return_value\"/ and @message like /\"<value>\"/",
    ),
    (
        "body.captures.return.arguments.<name>",
        "Arguments at function exit. Compare with entry arguments to detect mutation.",
    ),
    (
        "body.captures.return.locals.<name>",
        "Local variables at function exit (method-level only).",
    ),
    (
        "body.captures.return.throwable",
        "Exception info if function threw: type, message, stacktrace.",
    ),
];

const LINE_LOCALS_DOCUMENTATION: (&str, &str) = (
    "body.captures.lines.<line>.locals.<name>",
    "Local variables at a specific line (line-level only). \
     Filter: @message like /\"locals\"/ and @message like /\"<name>\"/",
);

const CAPTURED_VALUE_DOCUMENTATION: (&str, &str) = (
    "CapturedValue shapes",
    "Each captured value has 'type' and one of: \
     'value' (string representation for primitives/strings/numbers), \
     'fields' (map of field name to CapturedValue, for objects/structs), \
     'elements' (array of CapturedValue, for lists/arrays), \
     'entries' (array of {key: CapturedValue, value: CapturedValue}, for maps/dicts), \
     'is_null': true (for null values), \
     'not_captured_reason' \u{2014} the literal is agent-specific: Python emits lowercase \
     camelCase (depth, fieldCount, timeout); Java emits uppercase enum names \
     (DEPTH, TIMEOUT). Match both forms when filtering. \
     Oversize collections/maps are signaled via 'truncated: true' plus 'size' (original element count), \
     not via a not_captured_reason.",
);

fn field_documentation() -> Json {
    let mut entries: Vec<(String, Json)> = FIELD_DOCUMENTATION
        .iter()
        .map(|(key, text)| ((*key).to_string(), Json::string(*text)))
        .collect();
    entries.push((
        LINE_LOCALS_DOCUMENTATION.0.to_string(),
        Json::string(LINE_LOCALS_DOCUMENTATION.1),
    ));
    entries.push((
        CAPTURED_VALUE_DOCUMENTATION.0.to_string(),
        Json::string(CAPTURED_VALUE_DOCUMENTATION.1),
    ));
    Json::Object(entries)
}

/// Renders the sample-snapshot response as JSON text.
#[must_use]
pub fn render_get_sample_snapshot_for_breakpoint_output(
    context: &SampleContext<'_>,
    query_result: &QueryResult,
) -> String {
    let log_group_name = resolve_snapshot_log_group(context.service_name);
    let service = Json::string(context.service_name);
    let environment = Json::string(context.environment);
    let log_group = Json::string(log_group_name);
    let hash = Json::string(context.location_hash);
    let query_string = Json::string(context.query_string);

    if query_result.status == "Error" {
        return dumps_indent2(&Json::object(vec![
            ("status", Json::string("ERROR")),
            ("service_name", service),
            ("environment", environment),
            ("log_group_name", log_group),
            ("location_hash", hash),
            (
                "error",
                Json::string(query_result.error.as_deref().unwrap_or("Unknown error")),
            ),
            ("query_string", query_string),
        ]));
    }

    if query_result.status == "Polling Timeout" {
        return dumps_indent2(&Json::object(vec![
            ("status", Json::string("TIMEOUT")),
            ("queryId", query_result.query_id.clone()),
            ("service_name", service),
            ("environment", environment),
            ("log_group_name", log_group),
            ("location_hash", hash),
            (
                "message",
                Json::string(format!(
                    "Query did not complete within {} seconds.",
                    context.max_timeout
                )),
            ),
            ("query_string", query_string),
        ]));
    }

    if query_result.status != "Complete" {
        return dumps_indent2(&Json::object(vec![
            ("status", Json::string(query_result.status.as_str())),
            ("queryId", query_result.query_id.clone()),
            ("service_name", service),
            ("environment", environment),
            ("log_group_name", log_group),
            ("location_hash", hash),
            ("query_string", query_string),
            ("messages", messages_json(query_result)),
        ]));
    }

    let time_range = Json::object(vec![
        ("start", Json::string(context.start_time_utc)),
        ("end", Json::string(context.end_time_utc)),
    ]);

    let Some(first) = query_result.results.first() else {
        return dumps_indent2(&Json::object(vec![
            ("status", Json::string("NO_SNAPSHOTS_FOUND")),
            ("queryId", query_result.query_id.clone()),
            ("service_name", service),
            ("environment", environment),
            ("log_group_name", log_group),
            ("location_hash", hash),
            ("time_range", time_range),
            (
                "message",
                Json::string(
                    "No snapshots found in this window. Suggestions: \
                     (1) Try an older ACTIVE event timestamp \u{2014} older events have had more time \
                     for CloudWatch Logs ingestion. \
                     (2) If all timestamps fail, wait 1-2 minutes for ingestion delay. \
                     (3) Verify the breakpoint is still ACTIVE and not DISABLED from max_hits exhaustion.",
                ),
            ),
            ("query_string", query_string),
        ]));
    };

    let raw_message = match first.get("@message") {
        Some(Json::String(text)) => text.as_str(),
        _ => "{}",
    };
    let raw_size = raw_message.len();
    let use_parsed = raw_size > RAW_SNAPSHOT_SIZE_THRESHOLD && !context.include_raw;

    let sample_snapshot = if use_parsed {
        let mut parsed = parse_snapshot_fields(first);
        if let Json::Object(pairs) = &mut parsed {
            pairs.retain(|(key, _)| key != "raw_snapshot");
        }
        parsed
    } else {
        parse_json(raw_message).unwrap_or_else(|_| Json::Object(Vec::new()))
    };

    let mut output: Vec<(String, Json)> = vec![
        ("status".to_string(), Json::string("SUCCESS")),
        ("queryId".to_string(), query_result.query_id.clone()),
        ("service_name".to_string(), service),
        ("environment".to_string(), environment),
        ("log_group_name".to_string(), log_group),
        ("location_hash".to_string(), hash),
        ("time_range".to_string(), time_range),
        (
            "cloudwatch_timestamp".to_string(),
            first.get("@timestamp").cloned().unwrap_or(Json::Null),
        ),
    ];

    if use_parsed {
        output.push((
            "note".to_string(),
            Json::string(format!(
                "Raw snapshot was {} bytes and has been replaced with a \
                 compact parsed summary. To get the full raw snapshot, call this tool \
                 again with include_raw=True.",
                group_thousands(u64::try_from(raw_size).unwrap_or(u64::MAX))
            )),
        ));
    }

    output.push(("sample_snapshot".to_string(), sample_snapshot));
    output.push(("field_documentation".to_string(), field_documentation()));
    output.push(("messages".to_string(), messages_json(query_result)));

    dumps_indent2(&Json::Object(output))
}
