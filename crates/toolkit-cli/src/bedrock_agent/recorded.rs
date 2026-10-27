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
//! present member of another type, `null` included, rejects the recording. An unmatched call fails with a `Connect` error
//! that names the service and operation only, never a request value.

use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::client::{AgentClient, Service};

enum Outcome {
    Response(Json),
    Service { code: String, message: String },
    Connect(String),
    NoCredentials,
}

struct Entry {
    service: Service,
    operation: String,
    request: Json,
    outcome: Outcome,
}

/// Replays the entries of a recorded-response file.
pub(crate) struct RecordedClient {
    entries: Vec<Entry>,
    unsupported: Vec<String>,
}

/// The one accessor of an *optional* recorded member: absent is `Ok(None)`; a
/// present member must be of the expected type (`null` is not absent) or the
/// recording is rejected. The reason names the member and its expected type,
/// never the value.
pub(crate) fn optional_member<'j, T>(
    object: &'j Json,
    key: &str,
    expected: &str,
    expect: impl FnOnce(&'j Json) -> Option<T>,
) -> Result<Option<T>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(member) => expect(member).map(Some).ok_or_else(|| {
            format!(
                "\"{key}\" must be {expected} when present (it is {})",
                member.type_name()
            )
        }),
    }
}

pub(crate) fn optional_string<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, String> {
    optional_member(object, key, "a string", Json::as_str)
}

pub(crate) fn optional_object<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, String> {
    optional_member(object, key, "an object", |member| {
        matches!(member, Json::Object(_)).then_some(member)
    })
}

pub(crate) fn optional_array<'j>(
    object: &'j Json,
    key: &str,
) -> Result<Option<&'j Vec<Json>>, String> {
    optional_member(object, key, "an array", |member| match member {
        Json::Array(items) => Some(items),
        _ => None,
    })
}

/// JSON equality that ignores object key order.
pub(crate) fn same_json(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(other, _)| other == key)
                        .is_some_and(|(_, other)| same_json(value, other))
                })
        }
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_json(x, y))
        }
        _ => left == right,
    }
}

impl RecordedClient {
    /// Builds a client from a parsed recorded-response document.
    ///
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        let Some(Json::Array(calls)) = document.get("calls") else {
            return Err("the recorded responses need a \"calls\" array".to_string());
        };
        let mut entries = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            entries.push(parse_entry(index, call)?);
        }
        let unsupported = optional_array(document, "unsupported")?
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
                "no_credentials" => Outcome::NoCredentials,
                _ => {
                    return Err(bad(
                        "\"error.kind\" must be client, connect or no_credentials",
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
                && same_json(&entry.request, request)
        });
        match entry.map(|entry| &entry.outcome) {
            Some(Outcome::Response(response)) => Ok(response.clone()),
            Some(Outcome::Service { code, message }) => Err(AwsError::Service(Box::new(
                ServiceError::new("POST", "/", 400, Some(code), message, None),
            ))),
            Some(Outcome::Connect(message)) => Err(AwsError::connect(message)),
            Some(Outcome::NoCredentials) => Err(AwsError::NoCredentials),
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
