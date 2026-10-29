//! Replay of recorded benchmark-Lambda invocations (`--recorded FILE`).
//!
//! ```json
//! {"calls": [
//!   {"service": "lambda", "operation": "Invoke",
//!    "request": {"FunctionName": "ddb-skill-bench-abc-bench", "Payload": {"invocation_index": 0}},
//!    "response": {"StatusCode": 200, "Payload": {"phases_run": ["measure"], "raw_rows": []}}},
//!   {"service": "lambda", "operation": "Invoke",
//!    "error": {"kind": "client", "code": "ResourceNotFoundException", "message": "no function"}}
//! ]}
//! ```
//!
//! One step per invocation, replayed in order. A recorded `request` is a
//! *contained* request (`KeyOrder::Contained`): every member written in the
//! recording must equal the member of the request the orchestrator builds, and
//! the real request may hold more (the payload carries the whole design, the
//! manifest and the config). An `error` step (`kind` `client`, `connect` or
//! `no_credentials`) replays a failure after the same request check. A call with
//! no step left, a request that differs and a step no call used all stop the run
//! (`toolkit_aws::recorded`, #4727).

use toolkit_aws::recorded::{
    optional_text, step_fields, KeyOrder, MemberError, RecordedOutcome, ReplayError, Step,
    StepQueue, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

const INVOKE: &str = "lambda:Invoke";

/// The one AWS call of the orchestrator: `lambda:Invoke` with
/// `InvocationType=RequestResponse`.
pub(super) trait LambdaClient {
    /// Invokes the function; `request` is `{FunctionName, InvocationType,
    /// Payload}` with the payload as a JSON document. The response is the
    /// `Invoke` response: `{StatusCode, Payload, FunctionError?}`.
    fn invoke(&mut self, request: &Json) -> Result<Json, AwsError>;
    /// Recorded steps no call used (always 0 for a client that records none).
    fn unconsumed(&self) -> usize {
        0
    }
}

const NO_LIVE: &str = "this build has no live AWS client (pass --recorded FILE to replay recorded \
Lambda invocations)";

/// The client of a build without live AWS access: every call fails.
pub(super) struct NoLiveClient;

impl LambdaClient for NoLiveClient {
    fn invoke(&mut self, _request: &Json) -> Result<Json, AwsError> {
        Err(AwsError::connect(NO_LIVE))
    }
}

/// Replays the steps of a recorded document.
pub(super) struct RecordedClient {
    queue: StepQueue<String>,
}

fn quoted(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

impl RecordedClient {
    /// Builds a client from a parsed recorded document.
    ///
    /// # Errors
    ///
    /// A reason naming the step index and the wrong member, never a value.
    pub(super) fn from_document(document: &Json) -> Result<Self, String> {
        if document.get("calls").is_none() {
            return Err("the recorded document needs a \"calls\" array".to_string());
        }
        let calls = toolkit_aws::recorded::optional_list(document, "calls")
            .map_err(|error| quoted(&error))?;
        let mut queue = StepQueue::new(KeyOrder::Contained);
        for (index, call) in calls.iter().enumerate() {
            queue.push(INVOKE.to_string(), parse_call(index, call)?);
        }
        Ok(Self { queue })
    }
}

fn parse_call(index: usize, call: &Json) -> Result<Step, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = optional_text(call, "service")
        .map_err(|error| bad(&quoted(&error)))?
        .unwrap_or_default();
    let operation = optional_text(call, "operation")
        .map_err(|error| bad(&quoted(&error)))?
        .unwrap_or_default();
    if format!("{service}:{operation}") != INVOKE {
        return Err(bad(
            "\"service\" and \"operation\" must be lambda Invoke (the only call of the orchestrator)",
        ));
    }
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
    Ok(match fields.outcome {
        RecordedOutcome::Response(response) => Step::response(Some(request), response.clone()),
        RecordedOutcome::Error(error) => {
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
    })
}

impl LambdaClient for RecordedClient {
    fn invoke(&mut self, request: &Json) -> Result<Json, AwsError> {
        match self.queue.replay(&INVOKE.to_string(), request) {
            Ok(outcome) => outcome,
            Err(ReplayError::Exhausted) => Err(AwsError::connect(
                "no recorded response left for lambda:Invoke",
            )),
            Err(ReplayError::RequestMismatch) => Err(AwsError::connect(
                "the recorded request of lambda:Invoke differs from the request made",
            )),
        }
    }

    fn unconsumed(&self) -> usize {
        self.queue.unconsumed()
    }
}
