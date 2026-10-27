//! Replay of recorded AWS responses (`--recorded FILE`).
//!
//! The file is one JSON object:
//!
//! ```json
//! {
//!   "calls": [
//!     {"service": "application-signals",
//!      "operation": "ListInstrumentationConfigurations",
//!      "request": {"Service": "svc", "Environment": "prod", "InstrumentationType": "BREAKPOINT"},
//!      "response": {"Configurations": []}},
//!     {"service": "logs", "operation": "GetQueryResults",
//!      "request": {"queryId": "q-1"},
//!      "error": {"code": "ThrottlingException", "message": "slow down"}}
//!   ]
//! }
//! ```
//!
//! A call matches the entries with the same service and operation whose recorded
//! `request` is contained in the actual request: every recorded member must be
//! present with an equal value (objects compare without regard to key order and
//! may hold more members than recorded, so a fixture can leave out spans and
//! generated client tokens; arrays and scalars must be equal, so a paginated
//! request must carry the recorded token). Matching entries are consumed in
//! file order (list the most specific entry first) and the last one repeats, so
//! a polled operation can record `Running` and then `Complete`, and a fixed
//! answer needs one entry. `error.kind` is `client`
//! (default, a service error response), `connect`, `timeout` or
//! `no_credentials` (a transport failure). Every optional member (`request`,
//! `error.kind`, `error.code`, `error.message`) is absent or of its type; a
//! present member of another type, `null` included, rejects the recording
//! (the shared `optional_member` rule of #4712/#4713). An unmatched call fails
//! with a `Connect` error that names the service and operation only, never a
//! request value.

use toolkit_aws::error::TimeoutPhase;
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::aws_client::{AwsCalls, Service};
use toolkit_aws::recorded::{same_json, MemberError};

/// `"<key>" must be <expected> when present (it is <found>)`.
fn quoted(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

fn optional_string<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, String> {
    toolkit_aws::recorded::optional_text(object, key).map_err(|error| quoted(&error))
}

fn optional_object<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, String> {
    toolkit_aws::recorded::optional_object(object, key).map_err(|error| quoted(&error))
}

/// True when every member of the recorded `expected` is in `actual` with an
/// equal value (recursively for objects; arrays and scalars compare equal).
fn contains_json(expected: &Json, actual: &Json) -> bool {
    match (expected, actual) {
        (Json::Object(wanted), Json::Object(_)) => wanted.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|found| contains_json(value, found))
        }),
        _ => same_json(expected, actual),
    }
}

enum Outcome {
    Response(Json),
    Service { code: String, message: String },
    Connect(String),
    Timeout(String),
    NoCredentials,
}

struct Entry {
    service: Service,
    operation: String,
    request: Json,
    outcome: Outcome,
    consumed: bool,
}

/// Replays the entries of a recorded-response file.
pub struct RecordedClient {
    entries: Vec<Entry>,
}

impl RecordedClient {
    /// Builds a client from a parsed recorded-response document.
    ///
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub fn from_document(document: &Json) -> Result<Self, String> {
        if !matches!(document, Json::Object(_)) {
            return Err("the recorded responses must be a JSON object".to_string());
        }
        let Some(Json::Array(calls)) = document.get("calls") else {
            return Err("the recorded responses need a \"calls\" array".to_string());
        };
        let mut entries = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            entries.push(parse_entry(index, call)?);
        }
        Ok(Self { entries })
    }
}

fn parse_entry(index: usize, call: &Json) -> Result<Entry, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    if !matches!(call, Json::Object(_)) {
        return Err(bad("must be an object"));
    }
    let service = call
        .get("service")
        .and_then(Json::as_str)
        .and_then(Service::from_name)
        .ok_or_else(|| {
            bad(
                "\"service\" must be application-signals, logs, cloudwatchomni, \
                 bedrock-agentcore or bedrock-agentcore-control",
            )
        })?;
    let operation = call
        .get("operation")
        .and_then(Json::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| bad("\"operation\" must be a non-empty string"))?
        .to_string();
    let request = optional_object(call, "request")
        .map_err(|reason| bad(&reason))?
        .cloned()
        .unwrap_or_else(|| Json::Object(Vec::new()));
    let outcome = match (call.get("response"), call.get("error")) {
        (Some(response @ Json::Object(_)), None) => Outcome::Response(response.clone()),
        (None, Some(error @ Json::Object(_))) => {
            // Every optional member is validated whatever the kind, so a
            // wrong-typed one is rejected instead of defaulted or dropped.
            let in_error = |reason: String| bad(&format!("error {reason}"));
            let message = optional_string(error, "message").map_err(in_error)?;
            let code = optional_string(error, "code").map_err(in_error)?;
            let kind = optional_string(error, "kind").map_err(in_error)?;
            let message = message.unwrap_or_default().to_string();
            match kind.unwrap_or("client") {
                "client" => Outcome::Service {
                    code: code.unwrap_or_default().to_string(),
                    message,
                },
                "connect" => Outcome::Connect(message),
                "timeout" => Outcome::Timeout(message),
                "no_credentials" => Outcome::NoCredentials,
                _ => {
                    return Err(bad(
                        "\"error.kind\" must be client, connect, timeout or no_credentials",
                    ))
                }
            }
        }
        _ => {
            return Err(bad(
                "exactly one of \"response\" (an object) and \"error\" (an object) is required",
            ))
        }
    };
    Ok(Entry {
        service,
        operation,
        request,
        outcome,
        consumed: false,
    })
}

impl AwsCalls for RecordedClient {
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, AwsError> {
        let matches = |entry: &Entry| {
            entry.service == service
                && entry.operation == operation
                && contains_json(&entry.request, request)
        };
        let position = self
            .entries
            .iter()
            .position(|entry| matches(entry) && !entry.consumed)
            .or_else(|| self.entries.iter().rposition(matches));
        let Some(position) = position else {
            return Err(AwsError::connect(format!(
                "no recorded response for {} {operation} with this request",
                service.name()
            )));
        };
        let entry = &mut self.entries[position];
        entry.consumed = true;
        match &entry.outcome {
            Outcome::Response(response) => Ok(response.clone()),
            Outcome::Service { code, message } => Err(AwsError::Service(Box::new(
                ServiceError::new("POST", "/", 400, Some(code), message, None),
            ))),
            Outcome::Connect(message) => Err(AwsError::connect(message)),
            Outcome::Timeout(message) => Err(AwsError::timeout(TimeoutPhase::Read, message)),
            Outcome::NoCredentials => Err(AwsError::NoCredentials),
        }
    }
}
