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
//! failure that ends the run). An unmatched call fails with a `Connect` error
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

fn string_member<'a>(object: &'a Json, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Json::as_str)
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
        let unsupported = match document.get("unsupported") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Array(names)) => names
                .iter()
                .map(|name| {
                    name.as_str().map(str::to_string).ok_or_else(|| {
                        "\"unsupported\" must be an array of operation names".to_string()
                    })
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("\"unsupported\" must be an array".to_string()),
        };
        Ok(Self {
            entries,
            unsupported,
        })
    }
}

fn parse_entry(index: usize, call: &Json) -> Result<Entry, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = string_member(call, "service")
        .and_then(Service::from_name)
        .ok_or_else(|| bad("\"service\" must be sts, bedrock-agent, iam or s3"))?;
    let operation = string_member(call, "operation")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| bad("\"operation\" must be a non-empty string"))?
        .to_string();
    let request = match call.get("request") {
        None | Some(Json::Null) => Json::Object(Vec::new()),
        Some(request @ Json::Object(_)) => request.clone(),
        Some(_) => return Err(bad("\"request\" must be an object")),
    };
    let outcome = match (call.get("response"), call.get("error")) {
        (Some(response @ Json::Object(_)), None) => Outcome::Response(response.clone()),
        (None, Some(error @ Json::Object(_))) => {
            let message = string_member(error, "message")
                .unwrap_or_default()
                .to_string();
            match string_member(error, "kind").unwrap_or("client") {
                "client" => Outcome::Service {
                    code: string_member(error, "code").unwrap_or_default().to_string(),
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
