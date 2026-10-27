//! Replay of a recorded caller identity (`--recorded FILE`).
//!
//! ```json
//! {"calls": [
//!   {"service": "sts", "operation": "GetCallerIdentity", "request": {},
//!    "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}},
//!   {"service": "iam", "operation": "ListAccountAliases", "request": {},
//!    "response": {"AccountAliases": ["acme-sandbox"]}}
//! ]}
//! ```
//!
//! Only those two calls exist. An `error` step (`kind` `client`, `connect` or
//! `no_credentials`, with `code` and `message`) replays a failure after the
//! same request check as a response step. Every optional member is absent or of
//! its type; a present member of another type, `null` included, rejects the
//! recording (`toolkit_aws::recorded`, #4727). A call with no step left, a
//! request that differs from the recorded one and a step no call used all fail.

use toolkit_aws::recorded::{
    optional_text, step_fields, KeyOrder, MemberError, RecordedOutcome, ReplayError, Step,
    StepQueue, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::identity::IdentityClient;

const CALLERS: &str = "sts:GetCallerIdentity";
const ALIASES: &str = "iam:ListAccountAliases";

/// Replays the steps of a recorded document.
pub(crate) struct RecordedClient {
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
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        if document.get("calls").is_none() {
            return Err("the recorded document needs a \"calls\" array".to_string());
        }
        let calls = toolkit_aws::recorded::optional_list(document, "calls")
            .map_err(|error| quoted(&error))?;
        let mut queue = StepQueue::new(KeyOrder::Ignored);
        for (index, call) in calls.iter().enumerate() {
            let (key, step) = parse_call(index, call)?;
            queue.push(key, step);
        }
        Ok(Self { queue })
    }

    fn replay(&mut self, key: &str) -> Result<Json, AwsError> {
        match self
            .queue
            .replay(&key.to_string(), &Json::Object(Vec::new()))
        {
            Ok(outcome) => outcome,
            Err(ReplayError::Exhausted) => Err(AwsError::connect(format!(
                "no recorded response left for {key}"
            ))),
            Err(ReplayError::RequestMismatch) => Err(AwsError::connect(format!(
                "the recorded request of {key} differs from the request made"
            ))),
        }
    }
}

fn parse_call(index: usize, call: &Json) -> Result<(String, Step), String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = optional_text(call, "service")
        .map_err(|error| bad(&quoted(&error)))?
        .unwrap_or_default();
    let operation = optional_text(call, "operation")
        .map_err(|error| bad(&quoted(&error)))?
        .unwrap_or_default();
    let key = format!("{service}:{operation}");
    if key != CALLERS && key != ALIASES {
        return Err(bad(
            "\"service\" and \"operation\" must be sts GetCallerIdentity or iam ListAccountAliases",
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
    let step = match fields.outcome {
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
    };
    Ok((key, step))
}

impl IdentityClient for RecordedClient {
    fn get_caller_identity(&mut self) -> Result<Json, AwsError> {
        self.replay(CALLERS)
    }

    fn list_account_aliases(&mut self) -> Result<Json, AwsError> {
        self.replay(ALIASES)
    }

    fn unconsumed(&self) -> usize {
        self.queue.unconsumed()
    }
}
