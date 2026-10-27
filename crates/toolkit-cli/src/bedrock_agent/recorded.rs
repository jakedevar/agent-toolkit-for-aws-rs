//! Replay of recorded AWS responses (`--recorded FILE`).
//!
//! The file is one JSON object:
//!
//! ```json
//! {
//!   "unsupported": ["ListAgentCollaborators"],
//!   "calls": [
//!     {"service": "sts", "operation": "GetCallerIdentity", "request": {},
//!      "response": {"Account": "111122223333", "Arn": "arn:aws:iam::..."}},
//!     {"service": "bedrock-agent", "operation": "GetAgentAlias",
//!      "request": {"agentId": "A", "agentAliasId": "X"},
//!      "error": {"code": "ResourceNotFoundException", "message": "no alias"}}
//!   ]
//! }
//! ```
//!
//! A call matches the entry with the same service, operation and request
//! (objects compare without regard to key order, so a paginated request must
//! carry the recorded `nextToken`). `error.kind` is `client` (default, a
//! service error response), `connect` or `no_credentials` (a transport
//! failure that ends the run). Every optional member (`request`, `unsupported`,
//! `error.kind`, `error.code`, `error.message`) is absent or of its type; a
//! present member of another type, `null` included, rejects the recording. An
//! unmatched call fails with a `Connect` error
//! that names the service and operation only, never a request value.
//!
//! The step members and the request comparison follow the shared rules of
//! `toolkit_aws::recorded` (#4727); the reasons keep this command's wording.

use toolkit_aws::recorded::{
    optional_member, optional_text, step_fields, KeyOrder, MemberError, RecordedOutcome, Step,
    StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::client::{AgentClient, Service};

/// JSON equality that ignores object key order (the shared helper; the tests
/// import it from here).
#[cfg(test)]
pub(crate) use toolkit_aws::recorded::same_json;

struct Entry {
    service: Service,
    operation: String,
    /// The recorded request (`{}` when absent) and the outcome.
    step: Step,
}

/// Replays the entries of a recorded-response file.
pub(crate) struct RecordedClient {
    entries: Vec<Entry>,
    unsupported: Vec<String>,
}

/// `"<key>" must be <expected> when present (it is <found>)`.
fn quoted(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

impl RecordedClient {
    /// Builds a client from a parsed recorded-response document.
    ///
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        // recorded-shape: required member; absent, null or another type rejects the recording.
        let Some(Json::Array(calls)) = document.get("calls") else {
            return Err("the recorded responses need a \"calls\" array".to_string());
        };
        let mut entries = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            entries.push(parse_entry(index, call)?);
        }
        let unsupported =
            optional_member(document, "unsupported", "an array", |member| match member {
                // recorded-shape: the `unsupported` member of the recording, read through the shared
                // `optional_member` (absent is none, null or another type is rejected).
                Json::Array(names) => Some(names),
                _ => None,
            })
            .map_err(|error| quoted(&error))?
            .map(|names| {
                names
                    .iter()
                    .map(|name| {
                        name.as_str().map(str::to_string).ok_or_else(|| {
                            "\"unsupported\" must be an array of operation names".to_string()
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            entries,
            unsupported,
        })
    }
}

fn parse_entry(index: usize, call: &Json) -> Result<Entry, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = call
        .get("service")
        .and_then(Json::as_str)
        .and_then(Service::from_name)
        .ok_or_else(|| bad("\"service\" must be sts, bedrock-agent, iam or s3"))?;
    let operation = call
        .get("operation")
        .and_then(Json::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| bad("\"operation\" must be a non-empty string"))?
        .to_string();
    let fields = step_fields(call).map_err(|error| match error {
        StepShapeError::Request(member) => bad(&quoted(&member)),
        StepShapeError::Outcome | StepShapeError::ErrorNotObject => {
            bad("exactly one of \"response\" (an object) and \"error\" (an object) is required")
        }
    })?;
    let request = fields
        .request
        .cloned()
        .unwrap_or_else(|| Json::Object(Vec::new()));
    let step = match fields.outcome {
        RecordedOutcome::Response(response) => Step::response(Some(request), response.clone()),
        RecordedOutcome::Error(error) => {
            // Every optional member is validated whatever the kind, so a
            // wrong-typed one is rejected instead of defaulted or dropped.
            let in_error = |member: MemberError| bad(&format!("error {}", quoted(&member)));
            let message = optional_text(error, "message").map_err(in_error)?;
            let code = optional_text(error, "code").map_err(in_error)?;
            let kind = optional_text(error, "kind").map_err(in_error)?;
            let message = message.unwrap_or_default();
            let failure = match kind.unwrap_or("client") {
                "client" => AwsError::Service(Box::new(ServiceError::new(
                    "POST",
                    "/",
                    400,
                    Some(code.unwrap_or_default()),
                    message,
                    None,
                ))),
                "connect" => AwsError::connect(message),
                "no_credentials" => AwsError::NoCredentials,
                _ => {
                    return Err(bad(
                        "\"error.kind\" must be client, connect or no_credentials",
                    ))
                }
            };
            Step::failure(Some(request), failure)
        }
    };
    Ok(Entry {
        service,
        operation,
        step,
    })
}

impl AgentClient for RecordedClient {
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, AwsError> {
        let entry = self.entries.iter().find(|entry| {
            entry.service == service
                && entry.operation == operation
                && entry.step.matches(request, KeyOrder::Ignored)
        });
        match entry {
            Some(entry) => entry.step.outcome(),
            None => Err(AwsError::connect(format!(
                "no recorded response for {} {operation} with this request",
                service.name()
            ))),
        }
    }

    fn supports(&self, _service: Service, operation: &str) -> bool {
        !self.unsupported.iter().any(|name| name == operation)
    }
}
