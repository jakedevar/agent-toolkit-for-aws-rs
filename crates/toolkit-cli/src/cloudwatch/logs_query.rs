//! CloudWatch Logs Insights query helper for the snapshot tools
//! (`di_snapshot_queries.py` and the `di_logs_client.py` allowlist).
//!
//! `_execute_cloudwatch_query` starts a query and polls `GetQueryResults`
//! once a second until it settles or `max_timeout` seconds pass. The Logs
//! proxy of the Python code allowed exactly `start_query` and
//! `get_query_results`; here the closed [`LogsOp`] enum is that allowlist.
//! Time is injected ([`Pacer`]) so the polling loop is deterministic: a
//! replay never sleeps, it counts the seconds it would have slept.

use toolkit_aws::recorded::{optional_list, optional_text};
use toolkit_core::Json;

use super::aws_client::{failure_of, failure_text, is_client_error, AwsCalls, Service};
use super::errors::AwsFailure;
use super::pytext::get_text;
use super::response_shape::{check, Field, Shape};
use super::snapshot_rendering::QueryResult;

/// The CloudWatch Logs operations the snapshot tools may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogsOp {
    /// `StartQuery`.
    StartQuery,
    /// `GetQueryResults`.
    GetQueryResults,
}

impl LogsOp {
    /// The whole allowlist.
    pub const ALL: [LogsOp; 2] = [Self::StartQuery, Self::GetQueryResults];

    /// The API operation name.
    #[must_use]
    pub fn api_name(self) -> &'static str {
        match self {
            Self::StartQuery => "StartQuery",
            Self::GetQueryResults => "GetQueryResults",
        }
    }
}

/// A clock and a sleeper.
pub trait Pacer {
    /// Seconds since an arbitrary start.
    fn now(&mut self) -> f64;
    /// Waits for `seconds`.
    fn sleep(&mut self, seconds: f64);
}

/// A pacer that never waits: `sleep` advances its own clock. Replays use it,
/// so the `Polling Timeout` path takes no wall-clock time.
#[derive(Debug, Default, Clone, Copy)]
pub struct VirtualPacer {
    elapsed: f64,
}

impl Pacer for VirtualPacer {
    fn now(&mut self) -> f64 {
        self.elapsed
    }

    fn sleep(&mut self, seconds: f64) {
        self.elapsed += seconds;
    }
}

/// The `StartQuery` members read: a `null` or wrong-typed `queryId` is an
/// invalid response, an absent or empty one the legacy "did not return a
/// queryId" error.
const START_QUERY: &[Field] = &[("queryId", Shape::Text)];

/// The `GetQueryResults` members read (#4750): a `results` or `messages` that
/// is `null` or not a list is an invalid response, never an empty result.
const QUERY_RESULTS: &[Field] = &[
    ("status", Shape::Text),
    ("results", Shape::List),
    ("messages", Shape::List),
];

impl LogsOp {
    fn response_fields(self) -> &'static [Field] {
        match self {
            Self::StartQuery => START_QUERY,
            Self::GetQueryResults => QUERY_RESULTS,
        }
    }
}

fn call(client: &mut dyn AwsCalls, op: LogsOp, request: &Json) -> Result<Json, AwsFailure> {
    match client.call(Service::Logs, op.api_name(), request) {
        // recorded-shape: the response envelope must be an object; every
        // other shape is refused, and its members are checked here.
        Ok(response @ Json::Object(_)) => match check(&response, op.response_fields()) {
            Ok(()) => Ok(response),
            Err(shape) => Err(AwsFailure::Other(format!(
                "the {} response is invalid: {shape}",
                op.api_name()
            ))),
        },
        Ok(other) => Err(AwsFailure::Other(format!(
            "the {} response is not an object (it is {})",
            op.api_name(),
            other.type_name()
        ))),
        Err(error) => Err(failure_of(op.api_name(), &error)),
    }
}

fn error_result(error: String, query_id: Option<&Json>) -> QueryResult {
    QueryResult {
        status: "Error".to_string(),
        query_id: query_id.cloned().unwrap_or(Json::Null),
        error: Some(error),
        results: Vec::new(),
        messages: Vec::new(),
    }
}

/// `f"{prefix}: {exc}"` for a `ClientError`, `str(exc)` for any other class.
fn failure_message(prefix: &str, failure: &AwsFailure) -> String {
    if is_client_error(failure) {
        format!("{prefix}: {}", failure_text(failure))
    } else {
        failure_text(failure).to_string()
    }
}

/// One result row: `{field.get("field", ""): field.get("value", "")}` in
/// order, a repeated field name keeping its first position and the last
/// value. `None` for a row of another shape (a row that is not a list of
/// objects, or a `field`/`value` that is present and not a string).
fn result_row(line: &Json) -> Option<Json> {
    let Json::Array(fields) = line else {
        return None;
    };
    let mut row: Vec<(String, Json)> = Vec::with_capacity(fields.len());
    for field in fields {
        if !matches!(field, Json::Object(_)) {
            return None;
        }
        // Absent `field` / `value` read as `""`; `null` or another type makes
        // the row (and so the result) invalid (#4750).
        let name = optional_text(field, "field")
            .ok()?
            .unwrap_or_default()
            .to_string();
        let value = Json::string(optional_text(field, "value").ok()?.unwrap_or_default());
        match row.iter_mut().find(|(key, _)| *key == name) {
            Some((_, slot)) => *slot = value,
            None => row.push((name, value)),
        }
    }
    // recorded-shape: builds the row object; it reads no member.
    Some(Json::Object(row))
}

/// Executes a CloudWatch Logs Insights query and polls for results.
///
/// Never fails: every failure is a [`QueryResult`] with status `Error` (a
/// start or poll failure, or a response of an unexpected shape) or
/// `Polling Timeout`, exactly the dict the Python helper returns.
pub fn execute_cloudwatch_query(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    query_string: &str,
    start_epoch: i64,
    end_epoch: i64,
    log_group_name: &str,
    max_timeout: i64,
) -> QueryResult {
    let request = Json::object(vec![
        ("logGroupName", Json::string(log_group_name)),
        ("startTime", Json::Int(start_epoch)),
        ("endTime", Json::Int(end_epoch)),
        ("queryString", Json::string(query_string)),
    ]);
    let start_response = match call(client, LogsOp::StartQuery, &request) {
        Ok(response) => response,
        Err(failure) => {
            return error_result(failure_message("Failed to start query", &failure), None);
        }
    };

    // `call` has checked `queryId` already; read it through the shared
    // accessor. An absent or empty id is the legacy "did not return a
    // queryId" error, without the response dump the Python helper appended:
    // the response may carry anything, and a diagnostic never echoes it
    // (#4784).
    let query_id = match optional_text(&start_response, "queryId") {
        Ok(Some(id)) if !id.is_empty() => Json::string(id),
        Ok(_) => {
            return error_result("start_query did not return a queryId".to_string(), None);
        }
        Err(error) => {
            return error_result(format!("the StartQuery response is invalid: {error}"), None);
        }
    };

    #[allow(clippy::cast_precision_loss)]
    let timeout = max_timeout as f64;
    let poll_start = pacer.now();
    while poll_start + timeout > pacer.now() {
        let poll = Json::object(vec![("queryId", query_id.clone())]);
        let response = match call(client, LogsOp::GetQueryResults, &poll) {
            Ok(response) => response,
            Err(failure) => {
                return error_result(
                    failure_message("Failed to get results", &failure),
                    Some(&query_id),
                );
            }
        };

        let status = get_text(&response, "status", "Unknown");
        if matches!(status.as_str(), "Complete" | "Failed" | "Cancelled") {
            // `call` has checked both lists already; read them through the
            // shared accessor, and never default a malformed one.
            let (lines, messages) = match (
                optional_list(&response, "results"),
                optional_list(&response, "messages"),
            ) {
                (Ok(lines), Ok(messages)) => (lines, messages),
                (Err(error), _) | (_, Err(error)) => {
                    return error_result(
                        format!("the GetQueryResults response is invalid: {error}"),
                        Some(&query_id),
                    );
                }
            };
            let mut results = Vec::new();
            for line in lines {
                let Some(row) = result_row(line) else {
                    return error_result(
                        "get_query_results returned a result row of an unexpected shape"
                            .to_string(),
                        Some(&query_id),
                    );
                };
                results.push(row);
            }
            return QueryResult {
                status,
                query_id,
                error: None,
                results,
                messages: messages.to_vec(),
            };
        }

        pacer.sleep(1.0);
    }

    QueryResult {
        status: "Polling Timeout".to_string(),
        query_id,
        error: Some(format!(
            "Query did not complete within {max_timeout} seconds."
        )),
        results: Vec::new(),
        messages: Vec::new(),
    }
}
