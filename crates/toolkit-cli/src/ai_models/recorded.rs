//! Replay of recorded AWS calls (`--recorded FILE`).
//!
//! The document:
//!
//! ```json
//! {"calls": {"ListHubContents": [
//!   {"request": {"HubName": "h", "HubContentType": "Model", "MaxResults": 100},
//!    "response": {"HubContentSummaries": [], "NextToken": "t"}},
//!   {"request": {"HubName": "h"},
//!    "error": {"kind": "service", "code": "AccessDeniedException",
//!              "message": "denied", "status": 400}}]}}
//! ```
//!
//! Every step has exactly one of `response` (an object) and `error` (an
//! object), and an optional `request` (an object). The recording is a contract:
//! each optional member (`request`, `error.kind`, `error.code`,
//! `error.message`, `error.status`) is absent or of its type, whatever the
//! step's kind; a present member of another type, `null` included, rejects the
//! whole recording before anything runs. A step's `request`, when present, is
//! compared with the request made **before** either outcome is replayed, so an
//! error step asserts its request exactly as a response step does; a mismatch
//! is a runtime data error, never a tolerated service error. Reasons name
//! positions and rules, never values.
//!
//! `error.kind` is `service` (default), `no_credentials` or `connect`.
//!
//! The step members, the request check and the replay order are the shared
//! rules of `toolkit_aws::recorded` (#4727); the reasons keep this command's
//! wording.

use std::collections::VecDeque;

use toolkit_aws::recorded::{
    optional_member, optional_text, step_fields, KeyOrder, MemberError, RecordedOutcome,
    ReplayError, Step, StepQueue, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::client::{Operation, SageMakerClient};

/// Replays the recorded steps of a JSON document, in order per operation.
pub(super) struct RecordedClient {
    calls: StepQueue<Operation>,
}

// recorded-shape: the recording document being validated: a required object, every other
// shape is rejected, and no member is defaulted.
fn object_keys<'a>(
    value: &'a Json,
    allowed: &[&str],
    what: &str,
) -> Result<&'a [(String, Json)], String> {
    let Json::Object(members) = value else {
        return Err(format!("{what} must be an object"));
    };
    if members
        .iter()
        .any(|(key, _)| !allowed.contains(&key.as_str()))
    {
        return Err(format!(
            "{what} may only contain {}",
            allowed
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(members)
}

/// `error.status`: absent is 400; a present one is an HTTP status code.
fn optional_status(error: &Json) -> Result<Option<u16>, MemberError> {
    optional_member(
        error,
        "status",
        "an HTTP status code",
        |member| match member {
            Json::Int(status) if (100..=599).contains(status) => u16::try_from(*status).ok(),
            _ => None,
        },
    )
}

/// An `error` object. Every optional member is validated whatever the kind, so
/// a wrong-typed one is rejected instead of defaulted or dropped.
fn parse_failure(error: &Json, at: &str) -> Result<AwsError, String> {
    object_keys(error, &["kind", "code", "message", "status"], at)?;
    let in_error = |reason: MemberError| format!("{at}.{reason}");
    let kind = optional_text(error, "kind").map_err(in_error)?;
    let code = optional_text(error, "code").map_err(in_error)?;
    let message = optional_text(error, "message").map_err(in_error)?;
    let status = optional_status(error).map_err(in_error)?;
    let message = message.unwrap_or("");
    match kind.unwrap_or("service") {
        "service" => Ok(AwsError::service(ServiceError::new(
            "POST",
            "/",
            status.unwrap_or(400),
            code,
            message,
            None,
        ))),
        "no_credentials" => Ok(AwsError::NoCredentials),
        "connect" => Ok(AwsError::connect(message)),
        _ => Err(format!(
            "{at}.kind must be service, no_credentials or connect"
        )),
    }
}

fn parse_step(value: &Json, at: &str) -> Result<Step, String> {
    object_keys(value, &["request", "response", "error"], at)?;
    let fields = step_fields(value).map_err(|error| match error {
        StepShapeError::Request(member) => format!("{at}.{member}"),
        StepShapeError::Outcome => {
            format!("{at} needs exactly one of `response` (an object) or `error`")
        }
        StepShapeError::ErrorNotObject => format!("{at}.error must be an object"),
    })?;
    let expected = fields.request.cloned();
    Ok(match fields.outcome {
        RecordedOutcome::Response(response) => Step::response(expected, response.clone()),
        RecordedOutcome::Error(error) => {
            Step::failure(expected, parse_failure(error, &format!("{at}.error"))?)
        }
    })
}

impl RecordedClient {
    /// Reads a recorded document.
    ///
    /// # Errors
    ///
    /// A description of the first malformed part. Values of the document are
    /// never echoed; positions are.
    pub(super) fn from_json(document: &Json) -> Result<Self, String> {
        let top = object_keys(document, &["calls"], "the recorded input")?;
        let Some((_, calls)) = top.first() else {
            return Err("the recorded input needs a `calls` object".to_string());
        };
        let names: Vec<&str> = Operation::ALL.iter().map(|op| op.name()).collect();
        let by_operation = object_keys(calls, &names, "`calls`")?;
        // The request of a recording this command already pins is compared
        // member by member, in order.
        let mut parsed = StepQueue::new(KeyOrder::Significant);
        for (name, steps) in by_operation {
            let Some(operation) = Operation::ALL.iter().find(|op| op.name() == name) else {
                continue;
            };
            // recorded-shape: a required list of the recording being validated; every other shape
            // is rejected with an error naming the operation.
            let Json::Array(steps) = steps else {
                return Err(format!("calls.{name} must be a list of steps"));
            };
            let queue = steps
                .iter()
                .enumerate()
                .map(|(index, step)| parse_step(step, &format!("calls.{name}[{index}]")))
                .collect::<Result<VecDeque<_>, _>>()?;
            parsed.insert(*operation, queue);
        }
        Ok(Self { calls: parsed })
    }
}

impl SageMakerClient for RecordedClient {
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, AwsError> {
        // The expected request is checked before either outcome is replayed.
        match self.calls.replay(&operation, request) {
            Ok(outcome) => outcome,
            Err(ReplayError::Exhausted) => Err(AwsError::invalid_response(format!(
                "the recorded input has no further response for {}",
                operation.name()
            ))),
            Err(ReplayError::RequestMismatch) => Err(AwsError::invalid_response(format!(
                "the {} request differs from the recorded request",
                operation.name()
            ))),
        }
    }
}
