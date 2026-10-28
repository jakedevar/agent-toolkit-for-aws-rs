//! Score CloudWatch Omni agent traces on demand with AgentCore evaluators
//! (`cloudwatch-omni/evaluate_traces.py`).
//!
//! Given trace ids and evaluator ids, resolve each trace's session, fetch the
//! session's spans from `traces.default` via Omni SQL, rebuild them into the
//! `sessionSpans` shape the scoring API needs (every span's `kind` coerced to
//! a bare OTel string) and score every (trace, evaluator) pair, fetching each
//! unique trace once. The result is one JSON receipt: per-pair score, label
//! and error code, and the judge's explanation only when asked for. Raw spans
//! never leave the process.
//!
//! All AWS calls go through an [`AwsCalls`] client. This build wires only the
//! recorded replay and a client that sends nothing; each score is also written
//! back as a `gen_ai.evaluation.result` record through the same client, which
//! is why a replay needs the `logs` entries too (or `--no-writeback`).

use std::collections::{HashMap, HashSet};

use toolkit_aws::recorded::{optional_object_list, optional_text};
use toolkit_core::Json;

use super::aws_client::{
    client_error_code, failure_of, failure_text, is_client_error, AwsCalls, Service,
};
use super::errors::AwsFailure;
use super::logs_query::Pacer;
use super::omni_sql::{omni_query, EmptyPage, SPAN_ROW_LIMIT};
use super::pytext::strip;
use super::response_shape::{check, data_columns, Field, Shape};
use super::timefmt::UtcTime;
use crate::dynamodb::pyfmt;

/// The evaluator used when none is given.
pub const DEFAULT_EVALUATOR: &str = "Builtin.Helpfulness";
/// The evaluation levels.
pub const LEVELS: [&str; 3] = ["TRACE", "TOOL_CALL", "SESSION"];
/// Unique traces fetched per run.
pub const MAX_TRACES: usize = 50;
/// Distinct evaluators per run.
pub const MAX_EVALUATORS: usize = 10;
/// Total (trace, evaluator) pairs per run.
pub const MAX_PAIRS: usize = 100;
/// Hard ceiling on `Evaluate` calls per run (maps to the ~100 evaluations per
/// minute account quota).
pub const MAX_EVALUATE_CALLS: i64 = 100;
/// Scoring service cap on ids per call (`TOOL_CALL` chunking).
pub const MAX_TARGET_IDS: usize = 10;
/// Evaluators that need ground truth (an expected tool trajectory) and so
/// cannot score a raw trace. The evaluator APIs expose no flag for this, so
/// the list is maintained by hand.
pub const GROUND_TRUTH_REQUIRED: [&str; 3] = [
    "Builtin.TrajectoryExactOrderMatch",
    "Builtin.TrajectoryInOrderMatch",
    "Builtin.TrajectoryAnyOrderMatch",
];
/// Prefix of the log group the scores are written back to.
pub const EVAL_RESULTS_LOG_GROUP_PREFIX: &str = "/aws/cloudwatch/evaluations/results/";
const EVAL_RESULTS_LOG_STREAM: &str = "on-demand";
const EVAL_RESULT_RECORD_NAME: &str = "gen_ai.evaluation.result";
/// Retention of a results log group this run creates.
pub const DEFAULT_RESULTS_RETENTION_DAYS: i64 = 30;
/// The only `retentionInDays` values CloudWatch Logs accepts.
pub const RETENTION_DAYS_ALLOWED: [i64; 22] = [
    1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922,
    3288, 3653,
];
const MAX_LOG_GROUP_LEN: usize = 512;
const ACCESS_DENIED_CODES: [&str; 4] = [
    "AccessDenied",
    "AccessDeniedException",
    "UnauthorizedException",
    "AuthorizationError",
];
const UNKNOWN_SERVICE: &str = "default";
const POLL_HINT: &str = "narrow --window-days to speed it up";

const DEFAULT_KIND: &str = "UNSPECIFIED";
const KIND_BY_NUMBER: [&str; 6] = [
    "UNSPECIFIED",
    "INTERNAL",
    "SERVER",
    "CLIENT",
    "PRODUCER",
    "CONSUMER",
];

/// Span attribute columns to select from `traces.default`: the OTLP attribute
/// key and its SQL alias. Omni SQL has no wildcard projection, so only known
/// keys can be fetched; extend this list for custom evaluators that read
/// other attributes.
const ATTR_COLS: [(&str, &str); 22] = [
    ("openinference.span.kind", "oi_span_kind"),
    ("aws.genai.span_kind", "aws_genai_span_kind"),
    ("gen_ai.operation.name", "gen_ai_op_name"),
    ("input.value", "input_value"),
    ("output.value", "output_value"),
    ("gen_ai.input_value", "gen_ai_input_value"),
    ("gen_ai.output_value", "gen_ai_output_value"),
    ("gen_ai.input.messages", "gen_ai_input_messages"),
    ("gen_ai.output.messages", "gen_ai_output_messages"),
    ("llm.input_messages", "llm_input_messages"),
    ("llm.output_messages", "llm_output_messages"),
    ("tool.name", "tool_name"),
    ("gen_ai.tool.name", "gen_ai_tool_name"),
    ("gen_ai.tool.call.id", "gen_ai_tool_call_id"),
    ("session.id", "session_id_attr"),
    ("gen_ai.request.model", "gen_ai_req_model"),
    ("gen_ai.response.model", "gen_ai_resp_model"),
    ("gen_ai.system", "gen_ai_system"),
    ("gen_ai.usage.input_tokens", "usage_input_tokens"),
    ("gen_ai.usage.output_tokens", "usage_output_tokens"),
    ("llm.token_count.prompt", "prompt_tokens"),
    ("llm.token_count.completion", "completion_tokens"),
];

/// The plain columns a span row is read for: those `fetch_spans` selects
/// and [`row_to_span`] reads, and the `session_id` of the session lookup.
const BASE_COLUMNS: [&str; 11] = [
    "traceId",
    "spanId",
    "parentSpanId",
    "name",
    "kind",
    "startTimeUnixNano",
    "endTimeUnixNano",
    "svc_name",
    "scope_name",
    "status_code",
    "session_id",
];

const SPAN_ROW_FIELDS: [Field; 33] = data_columns(BASE_COLUMNS, ATTR_COLS);

/// Every column of an Omni row the evaluation reads (#4381 repair 4): the
/// base columns and every [`ATTR_COLS`] alias. A column is data passed to
/// the evaluator, so it has no type check.
pub const SPAN_ROW: &[Field] = &SPAN_ROW_FIELDS;

/// The evaluation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// One evaluate call per (trace, evaluator).
    Trace,
    /// Tool spans chunked into at most [`MAX_TARGET_IDS`] ids per call.
    ToolCall,
    /// One evaluate call per (session, evaluator).
    Session,
}

impl Level {
    /// The level named `name` (one of [`LEVELS`]).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "TRACE" => Some(Self::Trace),
            "TOOL_CALL" => Some(Self::ToolCall),
            "SESSION" => Some(Self::Session),
            _ => None,
        }
    }

    /// The level's name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::ToolCall => "TOOL_CALL",
            Self::Session => "SESSION",
        }
    }
}

/// The caps of a run (the module constants of the script, injectable for
/// tests).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Unique traces.
    pub max_traces: usize,
    /// Distinct evaluators.
    pub max_evaluators: usize,
    /// (trace, evaluator) pairs.
    pub max_pairs: usize,
    /// `Evaluate` calls.
    pub max_evaluate_calls: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_traces: MAX_TRACES,
            max_evaluators: MAX_EVALUATORS,
            max_pairs: MAX_PAIRS,
            max_evaluate_calls: MAX_EVALUATE_CALLS,
        }
    }
}

/// The arguments of a run (`argparse` namespace of the script).
#[derive(Debug, Clone)]
pub struct Options {
    /// `--trace-ids`.
    pub trace_ids: Option<String>,
    /// `--trace-id`.
    pub trace_id: Option<String>,
    /// `--session-id`.
    pub session_id: Option<String>,
    /// `--evaluator-ids`.
    pub evaluator_ids: Option<String>,
    /// `--evaluator-id`.
    pub evaluator_id: Option<String>,
    /// `--level`.
    pub level: Level,
    /// `--window-days`.
    pub window_days: i64,
    /// `--include-explanations`.
    pub include_explanations: bool,
    /// `--no-writeback`.
    pub no_writeback: bool,
    /// `--retention-days`.
    pub retention_days: i64,
    /// `--kms-key-id`.
    pub kms_key_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            trace_ids: None,
            trace_id: None,
            session_id: None,
            evaluator_ids: None,
            evaluator_id: None,
            level: Level::Trace,
            window_days: 30,
            include_explanations: false,
            no_writeback: false,
            retention_days: DEFAULT_RESULTS_RETENTION_DAYS,
            kms_key_id: None,
        }
    }
}

/// `{"error": message}`, the failure receipt (`_die`).
fn error_receipt(message: impl Into<String>) -> Json {
    Json::object(vec![("error", Json::string(message.into()))])
}

/// Flattens comma-separated id arguments into a de-duplicated,
/// order-preserving list. `lower` normalises trace ids (Omni SQL `traceId`
/// values are lowercase hex); evaluator ids keep their casing.
fn split_ids(values: &[Option<&str>], lower: bool) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for value in values.iter().flatten().filter(|text| !text.is_empty()) {
        for piece in value.split(',') {
            let piece = strip(piece);
            if piece.is_empty() {
                continue;
            }
            let piece = if lower {
                piece.to_lowercase()
            } else {
                piece.to_string()
            };
            if !ids.contains(&piece) {
                ids.push(piece);
            }
        }
    }
    ids
}

/// `^[0-9a-fA-F]{1,64}$`.
#[must_use]
pub fn is_trace_id(text: &str) -> bool {
    (1..=64).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `^[0-9A-Za-z._:\-]{1,128}$`. Validate (reject) rather than mangle, so a
/// malformed id never rewrites to a different real session's id.
#[must_use]
pub fn is_session_id(text: &str) -> bool {
    (1..=128).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// Coerces a raw span `kind` (numeric enum, `SPAN_KIND_*`, null, missing) to
/// a bare OTel `SpanKind` string. A boolean is not a numeric enum.
#[must_use]
pub fn normalize_span_kind(value: Option<&Json>) -> &'static str {
    let by_number = |number: i64| {
        usize::try_from(number)
            .ok()
            .and_then(|index| KIND_BY_NUMBER.get(index))
            .copied()
            .unwrap_or(DEFAULT_KIND)
    };
    match value {
        Some(Json::Int(number)) => by_number(*number),
        #[allow(clippy::cast_possible_truncation)]
        Some(Json::Float(number)) if number.is_finite() && number.fract() == 0.0 => {
            by_number(*number as i64)
        }
        Some(Json::String(text)) => {
            let text = strip(text);
            if text.is_empty() {
                return DEFAULT_KIND;
            }
            if text.bytes().all(|byte| byte.is_ascii_digit()) {
                return text.parse::<i64>().map_or(DEFAULT_KIND, by_number);
            }
            let upper = text.to_uppercase();
            let bare = ["SPAN_KIND_", "SPANKIND."]
                .iter()
                .find_map(|prefix| upper.strip_prefix(prefix))
                .unwrap_or(&upper);
            KIND_BY_NUMBER
                .iter()
                .copied()
                .find(|kind| *kind == bare)
                .unwrap_or(DEFAULT_KIND)
        }
        _ => DEFAULT_KIND,
    }
}

fn truthy(value: Option<&Json>) -> bool {
    value.is_some_and(pyfmt::truthy)
}

/// Reconstructs an OTLP-compatible span from an Omni SQL result row. Only
/// non-empty values are included in the attributes, so the evaluator gets a
/// clean document instead of one full of null keys.
#[must_use]
pub fn row_to_span(row: &Json) -> Json {
    let attributes: Vec<(String, Json)> = ATTR_COLS
        .iter()
        .filter_map(|(key, alias)| match row.get(alias) {
            None | Some(Json::Null) => None,
            Some(Json::String(text)) if text.is_empty() => None,
            Some(value) => Some(((*key).to_string(), value.clone())),
        })
        .collect();
    let get = |key: &str| row.get(key).cloned().unwrap_or(Json::Null);
    let name = match row.get("name") {
        Some(value) if pyfmt::truthy(value) => value.clone(),
        _ => Json::string(""),
    };
    let mut resource_attributes: Vec<(String, Json)> = Vec::new();
    if truthy(row.get("svc_name")) {
        resource_attributes.push(("service.name".to_string(), get("svc_name")));
    }
    let mut span: Vec<(String, Json)> = vec![
        ("traceId".to_string(), get("traceId")),
        ("spanId".to_string(), get("spanId")),
        ("name".to_string(), name),
        (
            "kind".to_string(),
            Json::string(normalize_span_kind(row.get("kind"))),
        ),
        ("attributes".to_string(), Json::Object(attributes)),
        (
            "resource".to_string(),
            Json::Object(vec![(
                "attributes".to_string(),
                Json::Object(resource_attributes),
            )]),
        ),
    ];
    for key in ["parentSpanId", "startTimeUnixNano", "endTimeUnixNano"] {
        if truthy(row.get(key)) {
            span.push((key.to_string(), get(key)));
        }
    }
    if truthy(row.get("scope_name")) {
        span.push((
            "scope".to_string(),
            Json::Object(vec![("name".to_string(), get("scope_name"))]),
        ));
    }
    if truthy(row.get("status_code")) {
        span.push((
            "status".to_string(),
            Json::Object(vec![("code".to_string(), get("status_code"))]),
        ));
    }
    Json::Object(span)
}

fn attr_select() -> String {
    ATTR_COLS
        .iter()
        .map(|(key, alias)| format!("attributes['{key}'] AS {alias}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A fetched set of spans and whether the row cap truncated it.
type Spans = (Vec<Json>, bool);

/// Fetches spans from `traces.default` for a `WHERE` clause.
fn fetch_spans(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    now: UtcTime,
    filter_clause: &str,
    window_days: i64,
) -> Result<Spans, String> {
    let sql = format!(
        "SELECT traceId, spanId, parentSpanId, name, kind, \
         startTimeUnixNano, endTimeUnixNano, \
         resource['attributes']['service.name'] AS svc_name, \
         scope['name'] AS scope_name, \
         status['code'] AS status_code, {} \
         FROM \"traces.default\" \
         WHERE {filter_clause} \
         AND \"@timestamp\" BETWEEN NOW() - INTERVAL '{window_days} days' AND NOW() \
         ORDER BY startTimeUnixNano ASC \
         LIMIT {}",
        attr_select(),
        SPAN_ROW_LIMIT + 1
    );
    let mut rows = omni_query(
        client,
        pacer,
        &session_name(now),
        &sql,
        POLL_HINT,
        EmptyPage::Stop,
    )?;
    let truncated = rows.len() > SPAN_ROW_LIMIT;
    if truncated {
        rows.truncate(SPAN_ROW_LIMIT);
    }
    let spans = rows
        .iter()
        .filter(|row| truthy(row.get("traceId")))
        .map(row_to_span)
        .collect();
    Ok((spans, truncated))
}

fn session_name(now: UtcTime) -> String {
    format!("eval-traces-{}", now.epoch_seconds())
}

/// Finds the trace's `session.id` via Omni SQL. Non-fatal on failure.
fn resolve_session_id(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    now: UtcTime,
    trace_id: &str,
    window_days: i64,
) -> Option<String> {
    let sql = format!(
        "SELECT attributes['session.id'] AS session_id \
         FROM \"traces.default\" \
         WHERE traceId = '{trace_id}' \
         AND attributes['session.id'] IS NOT NULL \
         AND \"@timestamp\" BETWEEN NOW() - INTERVAL '{window_days} days' AND NOW() \
         LIMIT 1"
    );
    let rows = omni_query(
        client,
        pacer,
        &session_name(now),
        &sql,
        POLL_HINT,
        EmptyPage::Stop,
    )
    .ok()?;
    rows.iter()
        .find_map(|row| row.get("session_id").and_then(Json::as_str))
        .filter(|session| !session.is_empty())
        .map(str::to_string)
}

/// The spans of one trace (or of its session) and how it was fetched.
struct Fetched {
    session_id: Option<String>,
    spans: Vec<Json>,
    truncated: bool,
}

/// Resolves one trace's session and fetches its spans once. The cache
/// de-duplicates by resolved session id: two trace ids in the same session
/// fetch that session's spans only once.
fn fetch_trace(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    now: UtcTime,
    trace_id: &str,
    window_days: i64,
    session_id: Option<&str>,
    cache: &mut HashMap<String, Spans>,
) -> Result<Fetched, String> {
    let resolved = match session_id.filter(|session| !session.is_empty()) {
        Some(session) => Some(session.to_string()),
        None => resolve_session_id(client, pacer, now, trace_id, window_days),
    };
    let session = resolved.filter(|session| is_session_id(session));
    let cache_key = session
        .clone()
        .unwrap_or_else(|| format!("trace:{trace_id}"));
    let (spans, truncated) = if let Some(found) = cache.get(&cache_key) {
        found.clone()
    } else {
        let filter_clause = match &session {
            Some(session) => format!("attributes['session.id'] = '{session}'"),
            None => format!("traceId = '{trace_id}'"),
        };
        let (spans, truncated) = fetch_spans(client, pacer, now, &filter_clause, window_days)?;
        if spans.is_empty() {
            return Err(format!("no spans found in the last {window_days} day(s)"));
        }
        cache.insert(cache_key, (spans.clone(), truncated));
        (spans, truncated)
    };
    if session.is_some()
        && truncated
        && !spans
            .iter()
            .any(|span| span.get("traceId").and_then(Json::as_str) == Some(trace_id))
    {
        return Err(format!(
            "session has more than {SPAN_ROW_LIMIT} spans and the target trace was not in the \
             fetched set; narrow --window-days, or the session is too large to score whole"
        ));
    }
    Ok(Fetched {
        session_id: session,
        spans,
        truncated,
    })
}

/// Span ids of `TOOL` spans (for `TOOL_CALL` level).
fn tool_span_ids(spans: &[Json]) -> Vec<String> {
    spans
        .iter()
        .filter_map(|span| {
            let attributes = span.get("attributes")?;
            let is_tool = attributes
                .get("openinference.span.kind")
                .and_then(Json::as_str)
                == Some("TOOL")
                || attributes
                    .get("gen_ai.operation.name")
                    .and_then(Json::as_str)
                    == Some("execute_tool");
            let span_id = span.get("spanId").and_then(Json::as_str)?;
            (is_tool && span_id.chars().count() == 16).then(|| span_id.to_string())
        })
        .collect()
}

/// The first `limit` characters of `text` (Python slicing counts code points).
fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn set(pairs: &mut Vec<(String, Json)>, key: &str, value: Json) {
    match pairs.iter_mut().find(|(existing, _)| existing == key) {
        Some((_, slot)) => *slot = value,
        None => pairs.push((key.to_string(), value)),
    }
}

/// Maps a raw evaluate response to compact rows; the raw envelope is not
/// returned. The judge's explanation quotes the agent's own inputs and
/// outputs, so it stays off the receipt unless asked for; it is kept on the
/// private `_explanation` key because the writeback record persists it.
/// One evaluation result: `value` is a number, or `null` for a result that
/// carries no score (an error row, the explicit nullable opt-in); every other
/// member read is a string when present.
const EVALUATION_RESULT: &[Field] = &[
    ("evaluatorId", Shape::Text),
    ("value", Shape::NullableNumber),
    ("label", Shape::Text),
    ("explanation", Shape::Text),
    ("errorCode", Shape::Text),
    ("errorMessage", Shape::Text),
];

/// The `Evaluate` response members read.
/// (Public for the read-set inventory, #4381 repair 4.)
pub const EVALUATE_RESPONSE: &[Field] =
    &[("evaluationResults", Shape::ObjectList(EVALUATION_RESULT))];

fn summarize(
    response: &Json,
    evaluator_id: &str,
    include_explanations: bool,
) -> Result<Vec<Json>, String> {
    // The whole response is checked before any row is built (#4758): a
    // present member of the wrong type, `null` included, rejects it, so a
    // malformed result is a score error and never a scored pair, a persisted
    // record or a count.
    if !matches!(response, Json::Object(_)) {
        return Err(format!(
            "the Evaluate response is not an object (it is {})",
            response.type_name()
        ));
    }
    check(response, EVALUATE_RESPONSE)
        .map_err(|error| format!("the Evaluate response is invalid: {error}"))?;
    let results = optional_object_list(response, "evaluationResults")
        .map_err(|error| format!("the Evaluate response is invalid: {error}"))?;
    let mut rows = Vec::with_capacity(results.len());
    for result in results {
        let explanation = optional_text(result, "explanation")
            .map_err(|error| format!("the Evaluate response is invalid: {error}"))?
            .unwrap_or_default()
            .to_string();
        let clipped = if explanation.chars().count() > 600 {
            format!("{}\u{2026}", clip(&explanation, 600))
        } else {
            explanation.clone()
        };
        let mut row: Vec<(String, Json)> = vec![
            (
                "evaluatorId".to_string(),
                result
                    .get("evaluatorId")
                    .cloned()
                    .unwrap_or_else(|| Json::string(evaluator_id)),
            ),
            (
                "value".to_string(),
                result.get("value").cloned().unwrap_or(Json::Null),
            ),
            (
                "label".to_string(),
                result.get("label").cloned().unwrap_or(Json::Null),
            ),
            ("_explanation".to_string(), Json::string(clipped.clone())),
        ];
        if include_explanations {
            row.push(("explanation".to_string(), Json::string(clipped)));
        } else {
            row.push((
                "hasExplanation".to_string(),
                Json::Bool(!explanation.is_empty()),
            ));
        }
        if let Some(code) = result.get("errorCode").filter(|code| pyfmt::truthy(code)) {
            row.push(("errorCode".to_string(), code.clone()));
            if let Some(message) = result
                .get("errorMessage")
                .filter(|message| pyfmt::truthy(message))
            {
                row.push((
                    "errorMessage".to_string(),
                    Json::string(clip(&pyfmt::text(message), 600)),
                ));
            }
        }
        rows.push(Json::Object(row));
    }
    Ok(rows)
}

/// Drops private (`_`-prefixed) keys: what the receipt may print, versus what
/// the writeback may read. The last step before printing.
fn public_rows(rows: &[Json]) -> Vec<Json> {
    rows.iter()
        .map(|row| match row {
            Json::Object(pairs) => Json::Object(
                pairs
                    .iter()
                    .filter(|(key, _)| !key.starts_with('_'))
                    .cloned()
                    .collect(),
            ),
            other => other.clone(),
        })
        .collect()
}

/// Sanitizes a service name into one log-group path segment.
fn log_group_segment(service: &str) -> String {
    let safe: String = strip(service)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/' | '#') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let segment = if safe.is_empty() {
        UNKNOWN_SERVICE.to_string()
    } else {
        safe
    };
    clip(
        &segment,
        MAX_LOG_GROUP_LEN - EVAL_RESULTS_LOG_GROUP_PREFIX.len(),
    )
}

/// The scored trace's `resource.attributes['service.name']`; its own span wins.
fn service_name(spans: &[Json], trace_id: &str) -> Option<String> {
    let mut fallback: Option<String> = None;
    for span in spans {
        let Some(name) = span
            .get("resource")
            .and_then(|resource| resource.get("attributes"))
            .and_then(|attributes| attributes.get("service.name"))
            .and_then(Json::as_str)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        if span.get("traceId").and_then(Json::as_str) == Some(trace_id) {
            return Some(name.to_string());
        }
        fallback.get_or_insert_with(|| name.to_string());
    }
    fallback
}

/// One `gen_ai.evaluation.result` record per scored row.
fn eval_result_records(
    rows: &[Json],
    trace_id: &str,
    session_id: Option<&str>,
    level: Level,
    service: &str,
    ts_ms: i64,
    partial: bool,
) -> Vec<Json> {
    rows.iter()
        .filter(|row| {
            !truthy(row.get("errorCode")) && !matches!(row.get("value"), None | Some(Json::Null))
        })
        .map(|row| {
            let mut attributes: Vec<(String, Json)> = vec![
                (
                    "gen_ai.evaluation.name".to_string(),
                    row.get("evaluatorId").cloned().unwrap_or(Json::Null),
                ),
                (
                    "gen_ai.evaluation.score.value".to_string(),
                    Json::string(row.get("value").map_or_else(String::new, pyfmt::text)),
                ),
                (
                    "aws.bedrock_agentcore.evaluation_level".to_string(),
                    Json::string(level.name()),
                ),
            ];
            if let Some(session) = session_id.filter(|session| !session.is_empty()) {
                set(&mut attributes, "session.id", Json::string(session));
            }
            if let Some(label) = row.get("label").filter(|label| **label != Json::Null) {
                set(
                    &mut attributes,
                    "gen_ai.evaluation.score.label",
                    label.clone(),
                );
            }
            if let Some(explanation) = row
                .get("_explanation")
                .filter(|explanation| pyfmt::truthy(explanation))
            {
                set(
                    &mut attributes,
                    "gen_ai.evaluation.explanation",
                    explanation.clone(),
                );
            }
            if partial {
                set(
                    &mut attributes,
                    "gen_ai.evaluation.partial",
                    Json::string("true"),
                );
            }
            Json::Object(vec![
                ("name".to_string(), Json::string(EVAL_RESULT_RECORD_NAME)),
                ("traceId".to_string(), Json::string(trace_id)),
                ("attributes".to_string(), Json::Object(attributes)),
                (
                    "resource".to_string(),
                    Json::Object(vec![(
                        "attributes".to_string(),
                        Json::Object(vec![("service.name".to_string(), Json::string(service))]),
                    )]),
                ),
                ("@timestamp".to_string(), Json::Int(ts_ms)),
            ])
        })
        .collect()
}

fn logs_call(client: &mut dyn AwsCalls, operation: &str, request: &Json) -> Result<(), AwsFailure> {
    client
        .call(Service::Logs, operation, request)
        .map(|_| ())
        .map_err(|error| failure_of(operation, &error))
}

/// What a writeback attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Writeback {
    /// The records were written.
    Persisted,
    /// Not written, and nothing worth telling the user (no permission, or no
    /// records), so the receipt never claims a score was persisted.
    Skipped,
    /// Not written, with the reason for the receipt.
    Failed(String),
}

fn persist_failure(failure: &AwsFailure) -> String {
    format!(
        "could not persist the score for later querying: {}",
        failure_text(failure)
    )
}

/// Creates the results log group and stream, retention-bounded and optionally
/// CMK-encrypted. Returns the outcome (`None` = ready) and a warning.
fn provision_group(
    client: &mut dyn AwsCalls,
    group: &str,
    retention_days: i64,
    kms_key_id: Option<&str>,
) -> (Option<Writeback>, Option<String>) {
    let mut warning = None;
    let mut request = vec![("logGroupName", Json::string(group))];
    if let Some(key) = kms_key_id {
        request.push(("kmsKeyId", Json::string(key)));
    }
    let created = match logs_call(client, "CreateLogGroup", &Json::object(request)) {
        Ok(()) => true,
        Err(failure) => {
            let code = client_error_code(&failure);
            if is_client_error(&failure) && ACCESS_DENIED_CODES.contains(&code) {
                return (Some(Writeback::Skipped), None);
            }
            if is_client_error(&failure) && code == "ResourceAlreadyExistsException" {
                false
            } else if is_client_error(&failure)
                && kms_key_id.is_some()
                && matches!(code, "InvalidParameterException" | "ValidationException")
            {
                return (
                    Some(Writeback::Failed(format!(
                        "could not create the results log group with kmsKeyId {} (does the \
                         key policy allow \
                         the CloudWatch Logs service principal to use it?): {}",
                        kms_key_id.unwrap_or_default(),
                        failure_text(&failure)
                    ))),
                    None,
                );
            } else {
                return (Some(Writeback::Failed(persist_failure(&failure))), None);
            }
        }
    };

    if created && retention_days != 0 {
        let request = Json::object(vec![
            ("logGroupName", Json::string(group)),
            ("retentionInDays", Json::Int(retention_days)),
        ]);
        if let Err(failure) = logs_call(client, "PutRetentionPolicy", &request) {
            warning = Some(format!(
                "created the results log group but could not set {retention_days}-day \
                 retention (needs \
                 logs:PutRetentionPolicy), so it keeps the account default retention \u{2014} \
                 which may be 'never expire', and these records carry the evaluator's \
                 explanation: {}",
                failure_text(&failure)
            ));
        }
    }

    let request = Json::object(vec![
        ("logGroupName", Json::string(group)),
        ("logStreamName", Json::string(EVAL_RESULTS_LOG_STREAM)),
    ]);
    if let Err(failure) = logs_call(client, "CreateLogStream", &request) {
        let code = client_error_code(&failure);
        if is_client_error(&failure) && ACCESS_DENIED_CODES.contains(&code) {
            return (Some(Writeback::Skipped), warning);
        }
        if !(is_client_error(&failure) && code == "ResourceAlreadyExistsException") {
            return (Some(Writeback::Failed(persist_failure(&failure))), warning);
        }
    }
    (None, warning)
}

/// Persists scores to the evaluation-results log group. Best effort: a
/// failure is a [`Writeback`] value, never an error.
fn write_back(
    client: &mut dyn AwsCalls,
    service: &str,
    records: &[Json],
    ts_ms: i64,
    retention_days: i64,
    kms_key_id: Option<&str>,
) -> (Writeback, Option<String>) {
    if records.is_empty() {
        return (Writeback::Skipped, None);
    }
    let group = format!(
        "{EVAL_RESULTS_LOG_GROUP_PREFIX}{}",
        log_group_segment(service)
    );
    let events: Vec<Json> = records
        .iter()
        .map(|record| {
            Json::object(vec![
                ("timestamp", Json::Int(ts_ms)),
                ("message", Json::string(pyfmt::dumps(record, false))),
            ])
        })
        .collect();
    let request = Json::object(vec![
        ("logGroupName", Json::string(group.as_str())),
        ("logStreamName", Json::string(EVAL_RESULTS_LOG_STREAM)),
        ("logEvents", Json::Array(events)),
    ]);
    let mut warning = None;
    for attempt in 1..=2 {
        match logs_call(client, "PutLogEvents", &request) {
            Ok(()) => return (Writeback::Persisted, warning),
            Err(failure) if is_client_error(&failure) => {
                let code = client_error_code(&failure);
                if ACCESS_DENIED_CODES.contains(&code) {
                    return (Writeback::Skipped, warning);
                }
                if code == "ResourceNotFoundException" && attempt == 1 {
                    let (outcome, provisioned) =
                        provision_group(client, &group, retention_days, kms_key_id);
                    warning = provisioned;
                    if let Some(outcome) = outcome {
                        return (outcome, warning);
                    }
                    continue;
                }
                return (Writeback::Failed(persist_failure(&failure)), warning);
            }
            Err(failure) => return (Writeback::Failed(persist_failure(&failure)), warning),
        }
    }
    (Writeback::Persisted, warning)
}

/// The result of scoring one pair.
struct PairScore {
    rows: Vec<Json>,
    errors: Vec<String>,
    calls_made: i64,
    note: Option<String>,
}

/// Scores one (trace, evaluator) pair against the AgentCore data plane.
fn score_pair(
    client: &mut dyn AwsCalls,
    trace_id: &str,
    spans: &[Json],
    evaluator_id: &str,
    level: Level,
    max_calls: i64,
    include_explanations: bool,
) -> PairScore {
    let evaluation_input = Json::object(vec![("sessionSpans", Json::Array(spans.to_vec()))]);
    let request = |target: Option<Json>| {
        let mut pairs = vec![
            ("evaluatorId", Json::string(evaluator_id)),
            ("evaluationInput", evaluation_input.clone()),
        ];
        if let Some(target) = target {
            pairs.push(("evaluationTarget", target));
        }
        Json::object(pairs)
    };
    let mut calls: Vec<Json> = Vec::new();
    let mut note = None;
    match level {
        Level::Session => calls.push(request(None)),
        Level::ToolCall => {
            let span_ids = tool_span_ids(spans);
            if span_ids.is_empty() {
                return PairScore {
                    rows: Vec::new(),
                    errors: vec!["TOOL_CALL requested but the trace has no tool spans".to_string()],
                    calls_made: 0,
                    note: None,
                };
            }
            for chunk in span_ids.chunks(MAX_TARGET_IDS) {
                calls.push(request(Some(Json::object(vec![(
                    "spanIds",
                    Json::Array(chunk.iter().map(|id| Json::string(id.as_str())).collect()),
                )]))));
            }
            let budget = usize::try_from(max_calls.max(0)).unwrap_or(0);
            if calls.len() > budget {
                note = Some(format!(
                    "tool-span scoring truncated to {budget} of {} chunks (evaluate-call budget)",
                    calls.len()
                ));
            }
        }
        Level::Trace => calls.push(request(Some(Json::object(vec![(
            "traceIds",
            Json::Array(vec![Json::string(trace_id)]),
        )])))),
    }
    calls.truncate(usize::try_from(max_calls.max(0)).unwrap_or(0));

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for call in &calls {
        let response = match client.call(Service::BedrockAgentCore, "Evaluate", call) {
            Ok(response) => response,
            Err(error) => {
                let failure = failure_of("Evaluate", &error);
                let mut message = failure_text(&failure).to_string();
                if message.contains("Unknown evaluator") && message.contains("provider") {
                    message.push_str(
                        " \u{2014} this evaluator is listed by list-evaluators/get-evaluator \
                         but not invocable by evaluate (a service-side discovery/evaluate \
                         mismatch, not a payload problem); pick a different evaluator",
                    );
                }
                errors.push(format!("evaluate failed: {message}"));
                continue;
            }
        };
        match summarize(&response, evaluator_id, include_explanations) {
            Ok(found) => rows.extend(found),
            Err(reason) => errors.push(format!("evaluate failed: {reason}")),
        }
    }
    PairScore {
        rows,
        errors,
        calls_made: i64::try_from(calls.len()).unwrap_or(i64::MAX),
        note,
    }
}

/// A row holds a score when its `value` is a number (zero included) and it
/// carries no `errorCode`. `summarize` has already rejected any other type.
fn is_scored(row: &Json) -> bool {
    matches!(row.get("value"), Some(Json::Int(_) | Json::Float(_))) && !truthy(row.get("errorCode"))
}

/// Everything `run` needs to keep between traces.
struct Totals {
    results: Vec<Json>,
    score_errors: Vec<Json>,
    notes: Vec<String>,
    pairs_scored: i64,
    persisted_traces: i64,
}

/// Runs the evaluation and returns the receipt, or the failure receipt
/// (`_die`).
///
/// # Errors
///
/// A failure receipt for invalid arguments, a ground-truth-only evaluator
/// list, or a run in which no pair produced a score.
#[allow(clippy::too_many_lines)]
pub fn run(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    options: &Options,
    limits: &Limits,
    now: UtcTime,
) -> Result<Json, Json> {
    let mut trace_ids = split_ids(
        &[options.trace_ids.as_deref(), options.trace_id.as_deref()],
        true,
    );
    if trace_ids.is_empty() {
        return Err(error_receipt(
            "no trace ids given (use --trace-id or --trace-ids)",
        ));
    }
    if let Some(bad) = trace_ids.iter().find(|id| !is_trace_id(id)) {
        return Err(error_receipt(format!(
            "invalid trace id (expected hex): {}",
            pyfmt::repr_str(bad)
        )));
    }
    let mut evaluator_ids = split_ids(
        &[
            options.evaluator_ids.as_deref(),
            options.evaluator_id.as_deref(),
        ],
        false,
    );
    if evaluator_ids.is_empty() {
        evaluator_ids.push(DEFAULT_EVALUATOR.to_string());
    }
    let session_arg = options.session_id.as_deref().filter(|id| !id.is_empty());
    if let Some(session) = session_arg {
        if trace_ids.len() > 1 {
            return Err(error_receipt(
                "--session-id applies to a single trace; omit it when scoring multiple traces",
            ));
        }
        if !is_session_id(session) {
            return Err(error_receipt(format!(
                "invalid session id: {}",
                pyfmt::repr_str(session)
            )));
        }
    }
    if options.retention_days != 0 && !RETENTION_DAYS_ALLOWED.contains(&options.retention_days) {
        let allowed = RETENTION_DAYS_ALLOWED
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(error_receipt(format!(
            "invalid --retention-days {}; CloudWatch Logs accepts one of [{allowed}] (or 0 to \
             leave the account default)",
            options.retention_days
        )));
    }

    let mut notes: Vec<String> = Vec::new();
    let ground_truth: Vec<String> = evaluator_ids
        .iter()
        .filter(|id| GROUND_TRUTH_REQUIRED.contains(&id.as_str()))
        .cloned()
        .collect();
    if !ground_truth.is_empty() {
        evaluator_ids.retain(|id| !GROUND_TRUTH_REQUIRED.contains(&id.as_str()));
        let message = format!(
            "evaluator(s) {} require ground truth (an expected tool trajectory), which a raw \
             trace does not carry \u{2014} on-demand scoring here supplies no reference inputs. \
             Use ground-truth-free evaluators (e.g. Builtin.ToolSelectionAccuracy for tool \
             quality), or evaluate against a dataset whose examples carry the expected \
             trajectory.",
            ground_truth.join(", ")
        );
        if evaluator_ids.is_empty() {
            return Err(Json::object(vec![
                ("error", Json::string(message)),
                (
                    "evaluatorIds",
                    Json::Array(
                        ground_truth
                            .iter()
                            .map(|id| Json::string(id.as_str()))
                            .collect(),
                    ),
                ),
            ]));
        }
        notes.push(format!("skipped {message}"));
    }

    if evaluator_ids.len() > limits.max_evaluators {
        let dropped = evaluator_ids.split_off(limits.max_evaluators);
        notes.push(format!(
            "capped to {} evaluators (\u{2264}{}-evaluator limit); NOT run: {}",
            evaluator_ids.len(),
            limits.max_evaluators,
            dropped.join(", ")
        ));
    }
    let evaluator_count = evaluator_ids.len();
    let allowed_traces = limits
        .max_traces
        .min((limits.max_pairs / evaluator_count).max(1));
    if trace_ids.len() > allowed_traces {
        let dropped = trace_ids.split_off(allowed_traces);
        notes.push(format!(
            "capped to {} traces (\u{2264}{}-trace / \u{2264}{}-pair limit at {evaluator_count} \
             evaluators); NOT scored: {}",
            trace_ids.len(),
            limits.max_traces,
            limits.max_pairs,
            dropped.join(", ")
        ));
    }

    let mut fetched: HashMap<String, Fetched> = HashMap::new();
    let mut fetch_errors: Vec<Json> = Vec::new();
    let mut span_cache: HashMap<String, Spans> = HashMap::new();
    let single_session = if trace_ids.len() == 1 {
        session_arg
    } else {
        None
    };
    for trace_id in &trace_ids {
        match fetch_trace(
            client,
            pacer,
            now,
            trace_id,
            options.window_days,
            single_session,
            &mut span_cache,
        ) {
            Ok(found) => {
                fetched.insert(trace_id.clone(), found);
            }
            Err(reason) => fetch_errors.push(Json::object(vec![
                ("traceId", Json::string(trace_id.as_str())),
                ("error", Json::string(reason)),
            ])),
        }
    }

    let mut totals = Totals {
        results: Vec::new(),
        score_errors: Vec::new(),
        notes,
        pairs_scored: 0,
        persisted_traces: 0,
    };
    let mut calls_budget = limits.max_evaluate_calls;
    let mut budget_hit = false;
    let mut scored_sessions: HashSet<(String, String)> = HashSet::new();
    let ts_ms = now.epoch_micros() / 1000;
    for trace_id in &trace_ids {
        if budget_hit {
            break;
        }
        let Some(found) = fetched.get(trace_id) else {
            continue;
        };
        let mut trace_rows: Vec<Json> = Vec::new();
        for evaluator_id in &evaluator_ids {
            if options.level == Level::Session {
                if let Some(session) = &found.session_id {
                    if !scored_sessions.insert((session.clone(), evaluator_id.clone())) {
                        continue;
                    }
                }
            }
            if calls_budget <= 0 {
                totals.notes.push(format!(
                    "evaluate-call budget ({}) reached; remaining (trace, evaluator) pairs not \
                     scored",
                    limits.max_evaluate_calls
                ));
                budget_hit = true;
                break;
            }
            let mut pair = score_pair(
                client,
                trace_id,
                &found.spans,
                evaluator_id,
                options.level,
                calls_budget,
                options.include_explanations,
            );
            calls_budget -= pair.calls_made;
            for row in &mut pair.rows {
                if let Json::Object(pairs) = row {
                    set(pairs, "traceId", Json::string(trace_id.as_str()));
                }
            }
            if pair.rows.iter().any(is_scored) {
                totals.pairs_scored += 1;
            }
            trace_rows.extend(pair.rows);
            for message in pair.errors {
                totals.score_errors.push(Json::object(vec![
                    ("traceId", Json::string(trace_id.as_str())),
                    ("evaluatorId", Json::string(evaluator_id.as_str())),
                    ("error", Json::string(message)),
                ]));
            }
            if let Some(note) = pair.note {
                totals
                    .notes
                    .push(format!("trace {trace_id} / {evaluator_id}: {note}"));
            }
        }

        if !trace_rows.is_empty() && !options.no_writeback {
            let service =
                service_name(&found.spans, trace_id).unwrap_or_else(|| UNKNOWN_SERVICE.to_string());
            let records = eval_result_records(
                &trace_rows,
                trace_id,
                found.session_id.as_deref(),
                options.level,
                &service,
                ts_ms,
                found.truncated,
            );
            if !records.is_empty() {
                let (outcome, warning) = write_back(
                    client,
                    &service,
                    &records,
                    ts_ms,
                    options.retention_days,
                    options.kms_key_id.as_deref(),
                );
                if let Some(warning) = warning {
                    if !totals.notes.contains(&warning) {
                        totals.notes.push(warning);
                    }
                }
                match outcome {
                    Writeback::Persisted => totals.persisted_traces += 1,
                    Writeback::Skipped => {}
                    Writeback::Failed(reason) => {
                        totals.notes.push(format!("trace {trace_id}: {reason}"));
                    }
                }
            }
        }
        if found.truncated {
            totals.notes.push(format!(
                "trace {trace_id}: span fetch hit the {SPAN_ROW_LIMIT}-row cap; scored on a \
                 partial span set"
            ));
        }
        totals.results.extend(trace_rows);
    }

    let evaluators_json = Json::Array(
        evaluator_ids
            .iter()
            .map(|id| Json::string(id.as_str()))
            .collect(),
    );
    if totals.results.is_empty() {
        return Err(Json::object(vec![
            (
                "error",
                Json::string("no (trace, evaluator) pair produced a score"),
            ),
            ("level", Json::string(options.level.name())),
            ("evaluators", evaluators_json),
            ("fetchErrors", Json::Array(fetch_errors)),
            ("scoreErrors", Json::Array(totals.score_errors)),
        ]));
    }

    let scored_trace_ids: HashSet<&str> = totals
        .results
        .iter()
        .filter(|row| is_scored(row))
        .filter_map(|row| row.get("traceId").and_then(Json::as_str))
        .collect();
    let mut receipt: Vec<(String, Json)> = vec![
        ("level".to_string(), Json::string(options.level.name())),
        ("evaluators".to_string(), evaluators_json),
        (
            "tracesFetched".to_string(),
            Json::Int(i64::try_from(fetched.len()).unwrap_or(i64::MAX)),
        ),
        (
            "tracesScored".to_string(),
            Json::Int(i64::try_from(scored_trace_ids.len()).unwrap_or(i64::MAX)),
        ),
        ("pairsScored".to_string(), Json::Int(totals.pairs_scored)),
        (
            "results".to_string(),
            Json::Array(public_rows(&totals.results)),
        ),
    ];
    if !options.no_writeback {
        receipt.push((
            "persistedTraces".to_string(),
            Json::Int(totals.persisted_traces),
        ));
        if totals.persisted_traces > 0 {
            receipt.push((
                "resultsLogGroupPrefix".to_string(),
                Json::string(EVAL_RESULTS_LOG_GROUP_PREFIX),
            ));
            receipt.push((
                "resultsRetentionDaysOnCreate".to_string(),
                if options.retention_days == 0 {
                    Json::string("account default")
                } else {
                    Json::Int(options.retention_days)
                },
            ));
            receipt.push((
                "resultsKmsKeyOnCreate".to_string(),
                Json::string(
                    options
                        .kms_key_id
                        .as_deref()
                        .filter(|key| !key.is_empty())
                        .unwrap_or("aws-owned"),
                ),
            ));
        }
    }
    if !totals.notes.is_empty() {
        receipt.push((
            "notes".to_string(),
            Json::Array(
                totals
                    .notes
                    .iter()
                    .map(|n| Json::string(n.as_str()))
                    .collect(),
            ),
        ));
    }
    if !fetch_errors.is_empty() {
        receipt.push(("fetchErrors".to_string(), Json::Array(fetch_errors)));
    }
    if !totals.score_errors.is_empty() {
        receipt.push(("scoreErrors".to_string(), Json::Array(totals.score_errors)));
    }
    Ok(Json::Object(receipt))
}
