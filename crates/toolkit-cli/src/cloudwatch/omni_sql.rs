//! Omni SQL span queries, shared by the trace evaluation and the dataset
//! capture helpers (`_omni_query` / `fetch_spans_by_trace` in
//! `cloudwatch-omni/`).
//!
//! A query is a session, a started query, a completion poll and a paginated
//! row fetch; the session is always stopped afterwards, whatever happened.
//! The pagination reuses `toolkit_aws::collect_pages` (opaque tokens, a
//! repeated token or a runaway walk is an error); the row cap is the scripts'
//! `SPAN_ROW_LIMIT + 1` probe: once more than `SPAN_ROW_LIMIT` rows are
//! collected no further page is requested.

use toolkit_aws::recorded::{optional_object_list, optional_text};
use toolkit_aws::{collect_pages, next_token_from, AwsError, Page, PageLimits};
use toolkit_core::Json;

use super::aws_client::{failure_of, failure_text, AwsCalls, Service};
use super::logs_query::Pacer;

/// Fetch `LIMIT + 1` rows so an exactly-full result is detectable as truncated.
pub const SPAN_ROW_LIMIT: usize = 1000;

/// Polls of a query before giving up (each preceded by a 2 s wait).
pub const QUERY_POLL_ATTEMPTS: usize = 150;

/// What to do when a page comes back empty but carries a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyPage {
    /// An empty page ends the walk (the evaluation script).
    Stop,
    /// Follow the token anyway (the dataset script).
    Follow,
}

fn call(
    client: &mut dyn AwsCalls,
    operation: &str,
    request: &Json,
) -> Result<Json, super::errors::AwsFailure> {
    match client.call(Service::CloudWatchOmni, operation, request) {
        // recorded-shape: the response envelope must be an object; every
        // other shape is refused, and the members are read by the shared
        // accessors.
        Ok(response @ Json::Object(_)) => Ok(response),
        Ok(other) => Err(super::errors::AwsFailure::Other(format!(
            "the {operation} response is not an object (it is {})",
            other.type_name()
        ))),
        Err(error) => Err(failure_of(operation, &error)),
    }
}

/// A required string member of a response, or the failure text: absent or
/// empty is "has no", `null` or another type an invalid response.
fn response_id(response: &Json, key: &str, operation: &str) -> Result<String, String> {
    optional_text(response, key)
        .map_err(|error| format!("Omni {operation} failed: the response is invalid: {error}"))?
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Omni {operation} failed: the response has no {key}"))
}

/// The query statuses a diagnostic may name. Any other status text is a
/// response value, which a diagnostic never echoes (#4784).
const KNOWN_STATUSES: [&str; 6] = [
    "Scheduled",
    "Running",
    "Complete",
    "Failed",
    "Cancelled",
    "Timeout",
];

/// The status as an incomplete-query diagnostic shows it: a known status by
/// name, `None` when there was none, and never an unrecognised value.
fn shown_status(status: Option<&str>) -> &'static str {
    match status {
        None => "None",
        Some(status) => KNOWN_STATUSES
            .iter()
            .find(|known| **known == status)
            .copied()
            .unwrap_or("<unrecognised>"),
    }
}

fn query_rows(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    session_id: &str,
    sql: &str,
    incomplete_hint: &str,
    empty_page: EmptyPage,
) -> Result<Vec<Json>, String> {
    let started = call(
        client,
        "StartTelemetryQuery",
        &Json::object(vec![
            ("sessionId", Json::string(session_id)),
            ("queryString", Json::string(sql)),
        ]),
    )
    .map_err(|failure| {
        format!(
            "Omni start_telemetry_query failed: {}",
            failure_text(&failure)
        )
    })?;
    let query_id = response_id(&started, "queryId", "start_telemetry_query")?;

    // Phase 1: poll for completion (status only; rows are not stable until
    // Complete).
    let mut status: Option<String> = None;
    for _ in 0..QUERY_POLL_ATTEMPTS {
        pacer.sleep(2.0);
        let poll = call(
            client,
            "GetTelemetryQueryResults",
            &Json::object(vec![
                ("queryId", Json::string(query_id.as_str())),
                ("maxResults", Json::Int(1)),
            ]),
        )
        .map_err(|failure| {
            format!(
                "Omni get_telemetry_query_results failed: {}",
                failure_text(&failure)
            )
        })?;
        status = optional_text(&poll, "status")
            .map_err(|error| {
                format!("Omni get_telemetry_query_results failed: the response is invalid: {error}")
            })?
            .map(str::to_string);
        if matches!(status.as_deref(), Some("Complete" | "Failed" | "Cancelled")) {
            break;
        }
    }
    if status.as_deref() != Some("Complete") {
        return Err(format!(
            "Omni SQL query did not complete (status={}); {incomplete_hint}",
            shown_status(status.as_deref())
        ));
    }

    // Phase 2: paginate to collect rows (the cap is 1000 per page).
    let mut collected = 0_usize;
    let mut last_failure: Option<String> = None;
    let outcome = collect_pages(&PageLimits::default(), |token| {
        let mut request = vec![
            ("queryId", Json::string(query_id.as_str())),
            ("maxResults", Json::Int(1000)),
        ];
        if let Some(token) = token {
            request.push(("nextToken", Json::string(token)));
        }
        let page = call(client, "GetTelemetryQueryResults", &Json::object(request)).map_err(
            |failure| {
                last_failure = Some(format!(
                    "Omni get_telemetry_query_results (fetch) failed: {}",
                    failure_text(&failure)
                ));
                AwsError::invalid_response("the page could not be fetched")
            },
        )?;
        // Absent and `[]` are no rows; `null`, a non-list or a non-object row
        // is an invalid response, never an empty result (#4727).
        let rows = optional_object_list(&page, "rows")
            .map_err(|error| AwsError::invalid_response(error.to_string()))?
            .to_vec();
        let mut next_token = next_token_from(&page, "nextToken")?;
        collected += rows.len();
        if collected > SPAN_ROW_LIMIT || (empty_page == EmptyPage::Stop && rows.is_empty()) {
            next_token = None;
        }
        Ok(Page {
            items: rows,
            next_token,
        })
    });
    outcome.map_err(|error| {
        last_failure
            .unwrap_or_else(|| format!("Omni get_telemetry_query_results (fetch) failed: {error}"))
    })
}

/// Runs an Omni SQL query and collects its rows (at most one page past the
/// row cap). The error is the receipt text of the failing step.
///
/// # Errors
///
/// The message of the first failing step (session, start, poll, fetch) or the
/// `did not complete` message; the session is stopped in every case.
pub fn omni_query(
    client: &mut dyn AwsCalls,
    pacer: &mut dyn Pacer,
    session_name: &str,
    sql: &str,
    incomplete_hint: &str,
    empty_page: EmptyPage,
) -> Result<Vec<Json>, String> {
    let session = call(
        client,
        "StartTelemetryQuerySession",
        &Json::object(vec![("sessionName", Json::string(session_name))]),
    )
    .map_err(|failure| {
        format!(
            "Omni start_telemetry_query_session failed: {}",
            failure_text(&failure)
        )
    })?;
    let session_id = response_id(&session, "sessionId", "start_telemetry_query_session")?;

    let rows = query_rows(client, pacer, &session_id, sql, incomplete_hint, empty_page);
    // Best effort, like the `except Exception: pass` of the scripts.
    let _ = client.call(
        Service::CloudWatchOmni,
        "StopTelemetryQuerySession",
        &Json::object(vec![("sessionId", Json::string(session_id.as_str()))]),
    );
    rows
}
