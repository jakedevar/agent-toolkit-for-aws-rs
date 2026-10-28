//! Turn CloudWatch Omni traces into an AgentCore evaluation dataset
//! (`cloudwatch-omni/capture_dataset_from_traces.py`).
//!
//! For each trace the spans are queried from `traces.default` via Omni SQL
//! and reshaped into one `AGENTCORE_EVALUATION_PREDEFINED_V1` example (root
//! span `COALESCE` for input and output, ordered `TOOL` name trajectory with
//! adjacent duplicates collapsed, `scenario_id` and metadata provenance),
//! validated, and written into a new dataset (`create`) or an existing one
//! (`add`). Only a compact JSON receipt is returned; raw spans never leave the
//! process.
//!
//! Creating or extending a dataset is a real resource write. [`run`] refuses
//! to write unless the caller passes `confirmed` (`--i-have-confirmed`), which
//! asserts that the user confirmed the write; the refusal happens before any
//! client call.

use std::collections::HashSet;

use toolkit_aws::next_token_from;
use toolkit_aws::recorded::{
    optional_member, optional_object_list, optional_string_list, optional_text, MemberError,
};
use toolkit_core::Json;

use super::aws_client::{failure_of, failure_text, AwsCalls, Service};
use super::errors::AwsFailure;
use super::evaluate::is_trace_id;
use super::logs_query::Pacer;
use super::omni_sql::{omni_query, EmptyPage, SPAN_ROW_LIMIT};
use super::pytext::strip;
use super::response_shape::{check, Field, Shape};
use crate::dynamodb::pyfmt;

const INPUT_KEYS: [&str; 2] = ["input.value", "gen_ai.input_value"];
const OUTPUT_KEYS: [&str; 2] = ["output.value", "gen_ai.output_value"];
/// The dataset schema type.
pub const SCHEMA_TYPE: &str = "AGENTCORE_EVALUATION_PREDEFINED_V1";
/// Traces captured per write batch; the excess is reported, never dropped
/// silently.
pub const MAX_TRACES: usize = 25;
/// The service caps one serialized example at 1 MB.
pub const MAX_EXAMPLE_BYTES: usize = 1_000_000;
/// A dataset holds at most this many examples.
pub const MAX_DATASET_EXAMPLES: i64 = 1000;
const POLL_HINT: &str = "reduce --trace-ids or narrow --window-days";

/// The span attribute columns the capture selects.
const ATTR_COLS: [(&str, &str); 8] = [
    ("openinference.span.kind", "oi_span_kind"),
    ("aws.genai.span_kind", "aws_genai_span_kind"),
    ("tool.name", "tool_name"),
    ("input.value", "input_value"),
    ("output.value", "output_value"),
    ("gen_ai.input_value", "gen_ai_input_value"),
    ("gen_ai.output_value", "gen_ai_output_value"),
    ("session.id", "session_id_attr"),
];

/// `--mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Write a new dataset.
    Create,
    /// Add examples to an existing dataset.
    Add,
}

/// The arguments of a run.
#[derive(Debug, Clone)]
pub struct Options {
    /// `--trace-ids`.
    pub trace_ids: String,
    /// `--mode`.
    pub mode: Mode,
    /// `--dataset-name`.
    pub dataset_name: Option<String>,
    /// `--dataset-id`.
    pub dataset_id: Option<String>,
    /// `--description`.
    pub description: Option<String>,
    /// `--window-days`.
    pub window_days: i64,
    /// `--i-have-confirmed`.
    pub confirmed: bool,
}

fn error_receipt(message: impl Into<String>) -> Json {
    Json::object(vec![("error", Json::string(message.into()))])
}

fn error_with(message: impl Into<String>, key: &str, value: Json) -> Json {
    Json::Object(vec![
        ("error".to_string(), Json::string(message.into())),
        (key.to_string(), value),
    ])
}

/// The text of a span attribute when it is a non-blank string, stripped.
fn attr_text(span: &Json, keys: &[&str]) -> Option<String> {
    let attributes = span.get("attributes")?;
    keys.iter().find_map(|key| {
        attributes
            .get(key)
            .and_then(Json::as_str)
            .map(strip)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

fn scan(spans: &[Json], keys: &[&str], reverse: bool) -> Option<String> {
    if reverse {
        spans.iter().rev().find_map(|span| attr_text(span, keys))
    } else {
        spans.iter().find_map(|span| attr_text(span, keys))
    }
}

fn has_parent(span: &Json) -> bool {
    span.get("parentSpanId").is_some_and(pyfmt::truthy)
}

fn span_kind(span: &Json) -> Option<&Json> {
    let attributes = span.get("attributes")?;
    ["openinference.span.kind", "aws.genai.span_kind"]
        .iter()
        .find_map(|key| attributes.get(key).filter(|value| pyfmt::truthy(value)))
}

fn ts_key(span: &Json) -> String {
    match span.get("ts") {
        None | Some(Json::Null) => String::new(),
        Some(Json::String(text)) => text.clone(),
        Some(other) => pyfmt::text(other),
    }
}

/// One `AGENTCORE_EVALUATION_PREDEFINED_V1` example from a trace's spans.
#[must_use]
pub fn to_example(trace_id: &str, spans: &[Json], partial: bool) -> Json {
    let mut spans = spans.to_vec();
    spans.sort_by_key(ts_key);
    let root = spans
        .iter()
        .find(|span| !has_parent(span))
        .or_else(|| spans.first());

    let input = root
        .and_then(|root| attr_text(root, &INPUT_KEYS))
        .or_else(|| scan(&spans, &INPUT_KEYS, false))
        .unwrap_or_default();
    let mut turn: Vec<(String, Json)> = vec![("input".to_string(), Json::string(input))];
    if let Some(output) = root
        .and_then(|root| attr_text(root, &OUTPUT_KEYS))
        .or_else(|| scan(&spans, &OUTPUT_KEYS, true))
    {
        turn.push(("expected_response".to_string(), Json::string(output)));
    }

    let session_id = spans.iter().find_map(|span| {
        span.get("attributes")
            .and_then(|attributes| attributes.get("session.id"))
            .filter(|value| pyfmt::truthy(value))
    });
    let mut metadata: Vec<(String, Json)> =
        vec![("sourceTraceId".to_string(), Json::string(trace_id))];
    if let Some(session_id) = session_id {
        metadata.push(("sessionId".to_string(), session_id.clone()));
    }
    if partial {
        metadata.push(("partial".to_string(), Json::Bool(true)));
    }
    let mut example: Vec<(String, Json)> = vec![
        ("scenario_id".to_string(), Json::string(trace_id)),
        ("turns".to_string(), Json::Array(vec![Json::Object(turn)])),
        ("metadata".to_string(), Json::Object(metadata)),
    ];

    let mut trajectory: Vec<Json> = Vec::new();
    for span in &spans {
        if span_kind(span).and_then(Json::as_str) != Some("TOOL") {
            continue;
        }
        let Some(name) = span
            .get("attributes")
            .and_then(|attributes| attributes.get("tool.name"))
            .filter(|name| pyfmt::truthy(name))
        else {
            continue;
        };
        if trajectory.last() != Some(name) {
            trajectory.push(name.clone());
        }
    }
    if !trajectory.is_empty() {
        example.push(("expected_trajectory".to_string(), Json::Array(trajectory)));
    }
    Json::Object(example)
}

/// The reason an example fails the PREDEFINED gate, if it does.
#[must_use]
pub fn invalid(example: &Json) -> Option<String> {
    let turns = match example.get("turns") {
        // recorded-shape: reads the example this command built, not a response.
        Some(Json::Array(turns)) if !turns.is_empty() => turns,
        _ => return Some("no turns".to_string()),
    };
    let input_ok = match turns[0].get("input") {
        Some(Json::String(text)) => !strip(text).is_empty(),
        // recorded-shape: reads the example this command built, not a response.
        Some(Json::Object(members)) => !members.is_empty(),
        _ => false,
    };
    if !input_ok {
        return Some("turn 1 missing a non-empty input".to_string());
    }
    // The service rejects the whole batch if any single example exceeds 1 MB.
    let size = pyfmt::dumps(example, false).len();
    if size > MAX_EXAMPLE_BYTES {
        return Some(format!(
            "example serializes to {size} bytes; the service caps one example at \
             {MAX_EXAMPLE_BYTES} bytes"
        ));
    }
    None
}

/// The fetched spans per trace in first-seen order, whether the row cap hit,
/// and the only possibly-cut (last returned) trace.
struct Fetch {
    by_trace: Vec<(String, Vec<Json>)>,
    truncated: bool,
    boundary: Option<String>,
}

/// Fetches spans for the trace ids via Omni SQL, sorted by trace id and start
/// time. Because the query sorts by trace id, a row-cap hit only ever cuts the
/// last returned trace; every lower-sorted trace is complete and every
/// higher-sorted requested trace was never reached.
fn fetch_spans_by_trace(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    trace_ids: &[String],
    window_days: i64,
) -> Result<Fetch, String> {
    let quoted = trace_ids
        .iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let columns = ATTR_COLS
        .iter()
        .map(|(key, alias)| format!("attributes['{key}'] AS {alias}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT traceId, spanId, parentSpanId, startTimeUnixNano, {columns} \
         FROM \"traces.default\" \
         WHERE traceId IN ({quoted}) \
         AND \"@timestamp\" BETWEEN NOW() - INTERVAL '{window_days} days' AND NOW() \
         ORDER BY traceId ASC, startTimeUnixNano ASC \
         LIMIT {}",
        SPAN_ROW_LIMIT + 1
    );
    let mut rows = omni_query(
        client,
        pacer,
        "capture-dataset",
        &sql,
        POLL_HINT,
        EmptyPage::Follow,
    )?;
    let truncated = rows.len() > SPAN_ROW_LIMIT;
    if truncated {
        rows.truncate(SPAN_ROW_LIMIT);
    }

    let mut by_trace: Vec<(String, Vec<Json>)> = Vec::new();
    for row in &rows {
        let Some(trace_id) = row
            .get("traceId")
            .and_then(Json::as_str)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let attributes: Vec<(String, Json)> = ATTR_COLS
            .iter()
            .filter_map(|(key, alias)| {
                row.get(alias)
                    .filter(|value| pyfmt::truthy(value))
                    .map(|value| ((*key).to_string(), value.clone()))
            })
            .collect();
        let span = Json::object(vec![
            ("spanId", row.get("spanId").cloned().unwrap_or(Json::Null)),
            (
                "parentSpanId",
                row.get("parentSpanId")
                    .filter(|value| pyfmt::truthy(value))
                    .cloned()
                    .unwrap_or(Json::Null),
            ),
            (
                "ts",
                row.get("startTimeUnixNano").cloned().unwrap_or(Json::Null),
            ),
            ("attributes", Json::Object(attributes)),
        ]);
        match by_trace.iter_mut().find(|(id, _)| id == trace_id) {
            Some((_, spans)) => spans.push(span),
            None => by_trace.push((trace_id.to_string(), vec![span])),
        }
    }
    let boundary = if truncated {
        by_trace.last().map(|(id, _)| id.clone())
    } else {
        None
    };
    Ok(Fetch {
        by_trace,
        truncated,
        boundary,
    })
}

/// The members of a `ListDatasets` item that are read.
const DATASET_SUMMARY: &[Field] = &[("datasetId", Shape::Text), ("datasetName", Shape::Text)];

/// The response members each control-plane operation reads (#4755): a
/// present `null` or wrong-typed one is an invalid response, so a malformed
/// write never becomes a receipt or a write count.
fn control_fields(operation: &str) -> &'static [Field] {
    match operation {
        "ListDatasets" => &[
            ("datasets", Shape::ObjectList(DATASET_SUMMARY)),
            ("nextToken", Shape::NullableText),
        ],
        "CreateDataset" => &[("datasetId", Shape::Text), ("status", Shape::Text)],
        "AddDatasetExamples" => &[("exampleIds", Shape::StringList)],
        _ => &[],
    }
}

fn control_call(
    client: &mut dyn AwsCalls,
    operation: &str,
    request: &Json,
) -> Result<Json, AwsFailure> {
    match client.call(Service::BedrockAgentCoreControl, operation, request) {
        // recorded-shape: the response envelope must be an object; every
        // other shape is refused, and its members are checked next.
        Ok(response @ Json::Object(_)) => match check(&response, control_fields(operation)) {
            Ok(()) => Ok(response),
            Err(shape) => Err(AwsFailure::Other(format!(
                "the {operation} response is invalid: {shape}"
            ))),
        },
        Ok(other) => Err(AwsFailure::Other(format!(
            "the {operation} response is not an object (it is {})",
            other.type_name()
        ))),
        Err(error) => Err(failure_of(operation, &error)),
    }
}

/// Resolves a dataset id from an id or a name, walking the listing.
///
/// # Errors
///
/// The failure text alone: the caller builds the receipt, so the conversion
/// errors it has already collected are never dropped (#4785).
fn resolve_dataset_id(client: &mut dyn AwsCalls, id_or_name: &str) -> Result<String, String> {
    let mut token: Option<String> = None;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        let request = match &token {
            Some(token) => Json::object(vec![("nextToken", Json::string(token.as_str()))]),
            None => Json::Object(Vec::new()),
        };
        let response = control_call(client, "ListDatasets", &request)
            .map_err(|failure| format!("list_datasets failed: {}", failure_text(&failure)))?;
        let invalid = |error: MemberError| {
            format!("list_datasets failed: the ListDatasets response is invalid: {error}")
        };
        let datasets = optional_object_list(&response, "datasets").map_err(invalid)?;
        for dataset in datasets {
            let id = optional_text(dataset, "datasetId").map_err(invalid)?;
            let name = optional_text(dataset, "datasetName").map_err(invalid)?;
            if id == Some(id_or_name) || name == Some(id_or_name) {
                if let Some(id) = id {
                    return Ok(id.to_string());
                }
            }
        }
        let next = next_token_from(&response, "nextToken")
            .map_err(|error| format!("list_datasets failed: {error}"))?;
        match next {
            Some(next) if seen.insert(next.clone()) => token = Some(next),
            _ => {
                return Err(format!("dataset {} not found", pyfmt::repr_str(id_or_name)));
            }
        }
    }
}

/// The dataset's current example count, or `None` when unknown. A failed
/// call must not block the write: the service enforces the cap anyway. A
/// response that is not an object, or whose `exampleCount` is present and
/// `null` or not an integer, is invalid (#4755) and stops the write.
fn count_examples(client: &mut dyn AwsCalls, dataset_id: &str) -> Result<Option<i64>, String> {
    let request = Json::object(vec![("datasetId", Json::string(dataset_id))]);
    let Ok(response) = client.call(Service::BedrockAgentCoreControl, "GetDataset", &request) else {
        return Ok(None);
    };
    if !matches!(response, Json::Object(_)) {
        return Err(format!(
            "get_dataset failed: the GetDataset response is not an object (it is {})",
            response.type_name()
        ));
    }
    optional_member(
        &response,
        "exampleCount",
        "an integer",
        |member| match member {
            Json::Int(count) => Some(*count),
            _ => None,
        },
    )
    .map_err(|error| format!("get_dataset failed: the GetDataset response is invalid: {error}"))
}

/// A random version-4 UUID for the idempotency `clientToken`.
fn uuid4() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut bytes = [0_u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let value = RandomState::new().build_hasher().finish();
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn truthy_text(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Builds the dataset examples from traces and writes them.
///
/// # Errors
///
/// A failure receipt: the confirmation refusal, invalid arguments, a failed
/// query or write, or a run that built no example.
#[allow(clippy::too_many_lines)]
pub fn run(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    options: &Options,
) -> Result<Json, Json> {
    // A create/add is a real resource write: refuse unless the caller asserts
    // that the user confirmed it. The refusal touches no AWS API.
    if !options.confirmed {
        return Err(error_with(
            "refusing to write: creating/extending a dataset is a real resource write. Confirm \
             with the user first, then re-run with --i-have-confirmed. Do not pass the flag on \
             a request that has not been confirmed.",
            "confirmationRequired",
            Json::Bool(true),
        ));
    }
    if options.mode == Mode::Create && truthy_text(options.dataset_name.as_deref()).is_none() {
        return Err(error_receipt(
            "--dataset-name is required for --mode create",
        ));
    }
    if options.mode == Mode::Add
        && truthy_text(options.dataset_id.as_deref()).is_none()
        && truthy_text(options.dataset_name.as_deref()).is_none()
    {
        return Err(error_receipt(
            "--dataset-id or --dataset-name is required for --mode add",
        ));
    }

    let mut unique: Vec<String> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    // Normalize to lowercase once: span trace ids are stored lowercase, the
    // filter is an exact string match and the boundary comparison relies on
    // the query's case-sensitive sort.
    for piece in options.trace_ids.split(',') {
        let id = strip(piece).to_lowercase();
        if id.is_empty() || unique.contains(&id) {
            continue;
        }
        if !is_trace_id(&id) {
            bad.push(id);
            continue;
        }
        unique.push(id);
    }
    if !bad.is_empty() {
        let shown: Vec<&str> = bad.iter().take(5).map(String::as_str).collect();
        return Err(error_receipt(format!(
            "invalid trace id(s) (expected hex): {}",
            shown.join(", ")
        )));
    }
    if unique.is_empty() {
        return Err(error_receipt("no valid trace ids provided"));
    }
    let mut notes: Vec<String> = Vec::new();
    if unique.len() > MAX_TRACES {
        notes.push(format!(
            "captured the first {MAX_TRACES} of {} traces; run again for the rest",
            unique.len()
        ));
        unique.truncate(MAX_TRACES);
    }

    let fetch =
        fetch_spans_by_trace(client, pacer, &unique, options.window_days).map_err(error_receipt)?;
    if fetch.truncated {
        notes.push(format!(
            "Omni SQL query hit the {SPAN_ROW_LIMIT}-row cap; only fully-fetched traces were \
             captured. The boundary trace is flagged metadata.partial=true; any trace reported \
             'row cap reached before this trace' was not fetched \u{2014} reduce --trace-ids or \
             narrow --window-days and re-run for those."
        ));
    }

    let mut examples: Vec<Json> = Vec::new();
    let mut conversion_errors: Vec<Json> = Vec::new();
    let conversion_error = |trace_id: &str, message: String| {
        Json::object(vec![
            ("traceId", Json::string(trace_id)),
            ("error", Json::string(message)),
        ])
    };
    for trace_id in &unique {
        let spans = fetch
            .by_trace
            .iter()
            .find(|(id, _)| id == trace_id)
            .map(|(_, spans)| spans.as_slice())
            .unwrap_or_default();
        if spans.is_empty() {
            // On truncation the query stops at the boundary (trace id
            // ascending), so a requested id that sorts after it was never
            // reached: report that distinctly from a genuine miss.
            let unreached = fetch.truncated
                && fetch
                    .boundary
                    .as_deref()
                    .is_some_and(|boundary| trace_id.as_str() > boundary);
            let message = if unreached {
                format!("row cap ({SPAN_ROW_LIMIT}) reached before this trace; not fetched")
            } else {
                "no spans found in window".to_string()
            };
            conversion_errors.push(conversion_error(trace_id, message));
            continue;
        }
        // Only the boundary trace can be mid-cut.
        let partial = fetch.truncated && fetch.boundary.as_deref() == Some(trace_id.as_str());
        let example = to_example(trace_id, spans, partial);
        if let Some(message) = invalid(&example) {
            conversion_errors.push(conversion_error(trace_id, message));
            continue;
        }
        examples.push(example);
    }

    if examples.is_empty() {
        return Err(error_with(
            "no examples could be built",
            "conversionErrors",
            Json::Array(conversion_errors),
        ));
    }

    let source = Json::object(vec![(
        "inlineExamples",
        Json::object(vec![("examples", Json::Array(examples.clone()))]),
    )]);
    let mut receipt: Vec<(String, Json)>;
    if options.mode == Mode::Create {
        let mut request = vec![
            (
                "datasetName",
                Json::string(truthy_text(options.dataset_name.as_deref()).unwrap_or_default()),
            ),
            ("schemaType", Json::string(SCHEMA_TYPE)),
            ("source", source),
            ("clientToken", Json::string(uuid4())),
        ];
        if let Some(description) = truthy_text(options.description.as_deref()) {
            request.push(("description", Json::string(description)));
        }
        let response =
            control_call(client, "CreateDataset", &Json::object(request)).map_err(|failure| {
                error_with(
                    format!("create_dataset failed: {}", failure_text(&failure)),
                    "conversionErrors",
                    Json::Array(conversion_errors.clone()),
                )
            })?;
        // `control_call` has checked both members; a created dataset needs
        // its id, so an absent or empty `datasetId` is an invalid response
        // too (#4755): no receipt and no write count without it.
        let invalid = |message: String| {
            error_with(
                format!("create_dataset failed: the CreateDataset response is invalid: {message}"),
                "conversionErrors",
                Json::Array(conversion_errors.clone()),
            )
        };
        let dataset_id = optional_text(&response, "datasetId")
            .map_err(|error| invalid(error.to_string()))?
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid("it has no datasetId".to_string()))?;
        let status = optional_text(&response, "status")
            .map_err(|error| invalid(error.to_string()))?
            .map_or(Json::Null, Json::string);
        receipt = vec![
            ("mode".to_string(), Json::string("create")),
            ("datasetId".to_string(), Json::string(dataset_id)),
            ("status".to_string(), status),
            (
                "examplesWritten".to_string(),
                Json::Int(i64::try_from(examples.len()).unwrap_or(i64::MAX)),
            ),
        ];
    } else {
        let wanted = truthy_text(options.dataset_id.as_deref())
            .or_else(|| truthy_text(options.dataset_name.as_deref()))
            .unwrap_or_default();
        // Every resolver failure (a service error, a malformed listing or
        // page, or no such dataset) keeps the conversion errors (#4785).
        let dataset_id = resolve_dataset_id(client, wanted).map_err(|message| {
            error_with(
                message,
                "conversionErrors",
                Json::Array(conversion_errors.clone()),
            )
        })?;
        let added = i64::try_from(examples.len()).unwrap_or(i64::MAX);
        let counted = count_examples(client, &dataset_id).map_err(|message| {
            error_with(
                message,
                "conversionErrors",
                Json::Array(conversion_errors.clone()),
            )
        })?;
        if let Some(existing) = counted {
            if existing + added > MAX_DATASET_EXAMPLES {
                return Err(error_with(
                    // The resolved id and the stored count are response
                    // values, so the refusal names neither (#4816): only
                    // this run's `added` and the fixed cap.
                    format!(
                        "the dataset is too full to take these examples: adding {added} would \
                         exceed the service cap of {MAX_DATASET_EXAMPLES} examples per dataset \
                         (the write would fail with 402 ServiceQuotaExceededException). Create \
                         a new dataset (--mode create) for the remainder."
                    ),
                    "conversionErrors",
                    Json::Array(conversion_errors),
                ));
            }
        }
        let request = Json::object(vec![
            ("datasetId", Json::string(dataset_id.as_str())),
            ("source", source),
            ("clientToken", Json::string(uuid4())),
        ]);
        let response = control_call(client, "AddDatasetExamples", &request).map_err(|failure| {
            error_with(
                format!("add_dataset_examples failed: {}", failure_text(&failure)),
                "conversionErrors",
                Json::Array(conversion_errors.clone()),
            )
        })?;
        // Absent or `[]` falls back to the number of examples sent (the
        // script's `len(ids) or len(examples)`); `null`, a non-list or a
        // non-string id is an invalid response, never a write count (#4755).
        let example_ids: Vec<Json> = optional_string_list(&response, "exampleIds")
            .map_err(|error| {
                error_with(
                    format!(
                        "add_dataset_examples failed: the AddDatasetExamples response is invalid: \
                         {error}"
                    ),
                    "conversionErrors",
                    Json::Array(conversion_errors.clone()),
                )
            })?
            .into_iter()
            .map(Json::string)
            .collect();
        let written = if example_ids.is_empty() {
            examples.len()
        } else {
            example_ids.len()
        };
        receipt = vec![
            ("mode".to_string(), Json::string("add")),
            ("datasetId".to_string(), Json::string(dataset_id)),
            (
                "examplesWritten".to_string(),
                Json::Int(i64::try_from(written).unwrap_or(i64::MAX)),
            ),
            ("exampleIds".to_string(), Json::Array(example_ids)),
        ];
    }
    if !conversion_errors.is_empty() {
        receipt.push((
            "conversionErrors".to_string(),
            Json::Array(conversion_errors),
        ));
    }
    if !notes.is_empty() {
        receipt.push((
            "notes".to_string(),
            Json::Array(notes.iter().map(|n| Json::string(n.as_str())).collect()),
        ));
    }
    Ok(Json::Object(receipt))
}
