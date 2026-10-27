//! CloudWatch Logs Insights query helper for the snapshot tools
//! (`di_snapshot_queries.py` and the `di_logs_client.py` allowlist).
//!
//! `_execute_cloudwatch_query` starts a query and polls `GetQueryResults`
//! once a second until it settles or `max_timeout` seconds pass. The Logs
//! proxy of the Python code allowed exactly `start_query` and
//! `get_query_results`; here the closed [`LogsOp`] enum is that allowlist.
//! Time is injected ([`Pacer`]) so the polling loop is deterministic: a
//! replay never sleeps, it counts the seconds it would have slept.

use toolkit_core::Json;

use super::aws_client::{failure_of, failure_text, is_client_error, AwsCalls, Service};
use super::errors::AwsFailure;
use super::pytext::{get_array, get_text};
use super::snapshot_rendering::QueryResult;
use crate::dynamodb::pyfmt;

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

fn call(client: &mut dyn AwsCalls, op: LogsOp, request: &Json) -> Result<Json, AwsFailure> {
    match client.call(Service::Logs, op.api_name(), request) {
        Ok(response @ Json::Object(_)) => Ok(response),
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
/// value. `None` for a row of another shape.
fn result_row(line: &Json) -> Option<Json> {
    let Json::Array(fields) = line else {
        return None;
    };
    let mut row: Vec<(String, Json)> = Vec::with_capacity(fields.len());
    for field in fields {
        if !matches!(field, Json::Object(_)) {
            return None;
        }
        let name = match field.get("field") {
            None => String::new(),
            Some(Json::String(name)) => name.clone(),
            Some(_) => return None,
        };
        let value = field
            .get("value")
            .cloned()
            .unwrap_or_else(|| Json::String(String::new()));
        match row.iter_mut().find(|(key, _)| *key == name) {
            Some((_, slot)) => *slot = value,
            None => row.push((name, value)),
        }
    }
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

    let query_id = start_response.get("queryId").cloned().unwrap_or(Json::Null);
    if !pyfmt::truthy(&query_id) {
        return error_result(
            format!(
                "start_query did not return a queryId (response: {})",
                pyfmt::text(&start_response)
            ),
            None,
        );
    }

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
            let mut results = Vec::new();
            for line in get_array(&response, "results") {
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
                messages: get_array(&response, "messages").to_vec(),
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
