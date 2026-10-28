//! The AWS client seam of the `marketplace` slice.
//!
//! The Lambdas called `boto3` (DynamoDB resources, SQS, `meteringmarketplace`,
//! EventBridge, CloudTrail). Here every call is one [`Service`] and
//! `PascalCase` API operation with the request the script built, handed to an
//! [`AwsClient`]. The commands wire a [`RecordedClient`] (`--recorded FILE`) or
//! [`NoLiveClient`], which sends nothing and reads no credential. A live client
//! is a follow-up (`docs/migration/marketplace.md`).
//!
//! # Recorded file
//!
//! ```json
//! {"calls": [
//!   {"service": "dynamodb", "operation": "Query",
//!    "request": {"TableName": "usage", "IndexName": "metering_pending"},
//!    "response": {"Items": [], "LastEvaluatedKey": {"licenseArn": "x"}}},
//!   {"service": "dynamodb", "operation": "UpdateItem",
//!    "error": {"code": "ConditionalCheckFailedException"}}
//! ]}
//! ```
//!
//! Steps of one `service` and `operation` replay in order (a call past the
//! last one fails like an unreachable service), the recorded `request` of a
//! step (error steps too) is compared before its outcome, and a run that
//! leaves steps unused is rejected. `error.kind` is `client` (default, a
//! service error response), `connect` or `no_credentials`. Every optional
//! member is absent or of its type; a present member of another type, `null`
//! included, rejects the recording. DynamoDB items and expression values are
//! plain JSON, as the `boto3` resource layer shows them (a string set is
//! `{"SS": [..]}`).

use toolkit_aws::recorded::{
    list_value, optional_text, step_fields, KeyOrder, MemberError, RecordedOutcome, ReplayError,
    Step, StepQueue, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

/// The AWS services the slice calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// `dynamodb`
    DynamoDb,
    /// `sqs`
    Sqs,
    /// `meteringmarketplace`
    Metering,
    /// `events` (EventBridge)
    Events,
    /// `cloudtrail`
    CloudTrail,
}

impl Service {
    const ALL: [Self; 5] = [
        Self::DynamoDb,
        Self::Sqs,
        Self::Metering,
        Self::Events,
        Self::CloudTrail,
    ];

    /// The service name of a recorded file.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::DynamoDb => "dynamodb",
            Self::Sqs => "sqs",
            Self::Metering => "meteringmarketplace",
            Self::Events => "events",
            Self::CloudTrail => "cloudtrail",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.name() == name)
    }
}

/// Sends one operation and returns the response object.
///
/// A service error response is `AwsError::Service`; anything else is a
/// transport failure.
pub(crate) trait AwsClient {
    /// # Errors
    ///
    /// The failure of the call.
    fn call(&mut self, service: Service, operation: &str, request: &Json)
        -> Result<Json, AwsError>;
}

/// The client of a run without `--recorded`: nothing is sent.
pub(crate) struct NoLiveClient;

impl AwsClient for NoLiveClient {
    fn call(&mut self, _: Service, _: &str, _: &Json) -> Result<Json, AwsError> {
        Err(AwsError::connect(
            "this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        ))
    }
}

/// Replays the steps of a recorded file in order.
pub(crate) struct RecordedClient {
    queue: StepQueue<String>,
    #[cfg(test)]
    sent: Vec<(String, Json)>,
}

fn quoted(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

fn queue_key(service: Service, operation: &str) -> String {
    format!("{}:{operation}", service.name())
}

impl RecordedClient {
    /// Builds a client from a parsed recorded file.
    ///
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        let calls = match document.get("calls") {
            Some(calls) => {
                list_value(calls).ok_or_else(|| "\"calls\" must be an array".to_string())?
            }
            None => return Err("the recorded responses need a \"calls\" array".to_string()),
        };
        let mut queue = StepQueue::new(KeyOrder::Ignored);
        for (index, call) in calls.iter().enumerate() {
            let (key, step) = parse_entry(index, call)?;
            queue.push(key, step);
        }
        Ok(Self {
            queue,
            #[cfg(test)]
            sent: Vec::new(),
        })
    }

    /// How many recorded steps the run never used.
    pub(crate) fn unconsumed(&self) -> usize {
        self.queue.unconsumed()
    }

    /// Every request sent, in order, as `service:Operation` and the request.
    #[cfg(test)]
    pub(crate) fn sent(&self) -> &[(String, Json)] {
        &self.sent
    }
}

fn parse_entry(index: usize, call: &Json) -> Result<(String, Step), String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = call
        .get("service")
        .and_then(Json::as_str)
        .and_then(Service::from_name)
        .ok_or_else(|| {
            bad("\"service\" must be dynamodb, sqs, meteringmarketplace, events or cloudtrail")
        })?;
    let operation = call
        .get("operation")
        .and_then(Json::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| bad("\"operation\" must be a non-empty string"))?;
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
            Step::failure(request, failure)
        }
    };
    Ok((queue_key(service, operation), step))
}

impl AwsClient for RecordedClient {
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, AwsError> {
        let key = queue_key(service, operation);
        #[cfg(test)]
        self.sent.push((key.clone(), request.clone()));
        match self.queue.replay(&key, request) {
            Ok(outcome) => outcome,
            Err(ReplayError::Exhausted) => Err(AwsError::connect(format!(
                "no recorded response left for {} {operation}",
                service.name()
            ))),
            Err(ReplayError::RequestMismatch) => Err(AwsError::connect(format!(
                "the recorded request of {} {operation} differs from the request made",
                service.name()
            ))),
        }
    }
}
