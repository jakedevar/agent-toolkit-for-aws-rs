//! Replay of recorded provider responses (`--recorded FILE`) for `preflight`.
//!
//! The file is one JSON object:
//!
//! ```json
//! {
//!   "calls": [
//!     {"service": "bedrock-runtime", "operation": "Converse",
//!      "request": {"modelId": "amazon.nova-lite-v1:0"},
//!      "response": {}},
//!     {"service": "bedrock-runtime", "operation": "InvokeModel",
//!      "error": {"code": "AccessDeniedException", "message": "no access"}},
//!     {"service": "service-quotas", "operation": "ListServiceQuotas",
//!      "request": {"ServiceCode": "bedrock"},
//!      "response": {"Quotas": [{"QuotaName": "...", "Value": 50.0}]}},
//!     {"service": "bedrock-mantle", "operation": "Responses",
//!      "error": {"status": 403, "message": "not authorized"}}
//!   ]
//! }
//! ```
//!
//! `service` is `bedrock-runtime` (`Converse`, `InvokeModel`), `service-quotas`
//! (`ListServiceQuotas`), `bedrock-mantle` (`Responses`) or
//! `bedrock-runtime-openai` (`Responses`, `ChatCompletions`). A call matches
//! the first entry with the same service, operation and request (objects
//! compare without regard to key order; an entry without `request` matches any
//! request, so a paginated listing must record `NextToken` to be told apart).
//! The request of a failure entry is compared before the failure is replayed,
//! like a response entry.
//!
//! `error.kind` is `client` (default: a service error response with `code`,
//! `message` and, for the two `OpenAI` surfaces, the HTTP `status`, which is then
//! required), `connect` (a transport failure: `EndpointConnectionError` /
//! `APIConnectionError`) or `no_credentials` (`NoCredentialsError`). Every
//! optional member is absent or of its type; a present member of another type,
//! `null` included, rejects the recording. An unrecorded call fails with
//! [`ProbeError::Unrecorded`], which names the service and operation only,
//! never a request value.
//!
//! The step members and the request comparison follow the shared rules of
//! `toolkit_aws::recorded` (#4727); the reasons keep this command's wording.

use toolkit_aws::recorded::{
    optional_list, optional_member, optional_text, step_fields, KeyOrder, MemberError,
    RecordedOutcome, Step, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::client::{ProbeClient, ProbeError, Service};

/// The operations each service of a recording may name.
fn operation_known(service: Service, operation: &str) -> bool {
    match service {
        Service::BedrockRuntime => matches!(operation, "Converse" | "InvokeModel"),
        Service::Quotas => operation == "ListServiceQuotas",
        Service::Mantle => operation == "Responses",
        Service::RuntimeOpenai => matches!(operation, "Responses" | "ChatCompletions"),
    }
}

struct Entry {
    service: Service,
    operation: String,
    step: Step,
}

/// Replays the entries of a recorded-response file.
pub(crate) struct RecordedClient {
    entries: Vec<Entry>,
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
        let calls = optional_list(document, "calls").map_err(|error| quoted(&error))?;
        let mut entries = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            entries.push(parse_entry(index, call)?);
        }
        Ok(Self { entries })
    }
}

/// The HTTP status of a recorded `OpenAI`-surface error.
fn status_member(error: &Json) -> Result<Option<u16>, MemberError> {
    optional_member(error, "status", "an integer", |member| match member {
        Json::Int(status) => u16::try_from(*status)
            .ok()
            .filter(|s| (100..=599).contains(s)),
        _ => None,
    })
}

fn parse_entry(index: usize, call: &Json) -> Result<Entry, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = call
        .get("service")
        .and_then(Json::as_str)
        .and_then(Service::from_name)
        .ok_or_else(|| {
            bad("\"service\" must be bedrock-runtime, service-quotas, bedrock-mantle or bedrock-runtime-openai")
        })?;
    let operation = call
        .get("operation")
        .and_then(Json::as_str)
        .filter(|name| operation_known(service, name))
        .ok_or_else(|| bad("\"operation\" is not an operation of this service"))?
        .to_string();
    let fields = step_fields(call).map_err(|error| match error {
        StepShapeError::Request(member) => bad(&quoted(&member)),
        StepShapeError::Outcome | StepShapeError::ErrorNotObject => {
            bad("exactly one of \"response\" (an object) and \"error\" (an object) is required")
        }
    })?;
    let request = fields.request.cloned();
    let step = match fields.outcome {
        RecordedOutcome::Response(response) => Step::response(request, response.clone()),
        RecordedOutcome::Error(error) => {
            // Every optional member is validated whatever the kind, so a
            // wrong-typed one is rejected instead of defaulted or dropped.
            let in_error = |member: MemberError| bad(&format!("error {}", quoted(&member)));
            let message = optional_text(error, "message").map_err(in_error)?;
            let code = optional_text(error, "code").map_err(in_error)?;
            let kind = optional_text(error, "kind").map_err(in_error)?;
            let status = status_member(error).map_err(in_error)?;
            let message = message.unwrap_or_default();
            let failure = match kind.unwrap_or("client") {
                "client" => {
                    if service.reports_status() && status.is_none() {
                        return Err(bad(
                            "an OpenAI-surface \"error\" needs an integer HTTP \"status\" (100-599)",
                        ));
                    }
                    AwsError::Service(Box::new(ServiceError::new(
                        "POST",
                        "/",
                        status.unwrap_or(400),
                        code,
                        message,
                        None,
                    )))
                }
                "connect" => AwsError::connect(message),
                "no_credentials" => AwsError::NoCredentials,
                _ => {
                    return Err(bad(
                        "\"error.kind\" must be client, connect or no_credentials",
                    ));
                }
            };
            Step::failure(request, failure)
        }
    };
    Ok(Entry {
        service,
        operation,
        step,
    })
}

impl ProbeClient for RecordedClient {
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, ProbeError> {
        let entry = self.entries.iter().find(|entry| {
            entry.service == service
                && entry.operation == operation
                && entry.step.matches(request, KeyOrder::Ignored)
        });
        match entry {
            Some(entry) => entry.step.outcome().map_err(ProbeError::Aws),
            None => Err(ProbeError::Unrecorded {
                service,
                operation: operation.to_string(),
            }),
        }
    }
}
