//! The AWS client seam of `toolkit documentdb wa-review`.
//!
//! The Python script called `boto3` clients (`docdb`, `cloudwatch`, `ec2`,
//! `secretsmanager`). Here every call is one [`Service`] and `PascalCase` API
//! operation with the request the script built, handed to an [`AwsClient`].
//! The command wires either [`RecordedClient`] (`--recorded FILE`) or
//! [`NoLiveClient`], which sends nothing and reads no credentials. A live
//! client behind the `toolkit-aws` transport is a follow-up
//! (`docs/migration/docgraph.md`).
//!
//! # Recorded file
//!
//! ```json
//! {"calls": [
//!   {"service": "docdb", "operation": "DescribeDBClusters",
//!    "request": {"DBClusterIdentifier": "prod"},
//!    "response": {"DBClusters": [{"BackupRetentionPeriod": 7}]}},
//!   {"service": "cloudwatch", "operation": "GetMetricStatistics",
//!    "response": {"Datapoints": []}},
//!   {"service": "ec2", "operation": "DescribeSecurityGroups",
//!    "error": {"code": "UnauthorizedOperation", "message": "no"}}
//! ]}
//! ```
//!
//! A call matches the first entry with the same service and operation whose
//! recorded `request` equals the request made (objects compare without regard
//! to key order). An entry **without** a `request` answers any request of its
//! operation, so a recording that does not care about the exact metric window
//! stays short. `error.kind` is `client` (default, a service error response),
//! `connect` or `no_credentials`. Every optional member is absent or of its
//! type; a present member of another type, `null` included, rejects the
//! recording. A call with no matching entry fails like an unreachable
//! service and names the service and operation only, never a request value.

use toolkit_aws::recorded::{
    optional_text, step_fields, KeyOrder, MemberError, RecordedOutcome, Step, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

/// The AWS services the review reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// `docdb`
    Docdb,
    /// `cloudwatch`
    CloudWatch,
    /// `ec2`
    Ec2,
    /// `secretsmanager`
    SecretsManager,
}

impl Service {
    /// The service name of a recorded file.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Docdb => "docdb",
            Self::CloudWatch => "cloudwatch",
            Self::Ec2 => "ec2",
            Self::SecretsManager => "secretsmanager",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [
            Self::Docdb,
            Self::CloudWatch,
            Self::Ec2,
            Self::SecretsManager,
        ]
        .into_iter()
        .find(|service| service.name() == name)
    }
}

/// Sends one operation and returns the response object.
///
/// A service error response is `AwsError::Service`; anything else is a
/// transport failure. The review turns either into a `warn` or `fail` check.
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

struct Entry {
    service: Service,
    operation: String,
    step: Step,
}

/// Replays the entries of a recorded file.
pub(crate) struct RecordedClient {
    entries: Vec<Entry>,
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
    /// Builds a client from a parsed recorded file.
    ///
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        let calls = match document.get("calls") {
            Some(calls) => super::snapshot::list(calls)
                .ok_or_else(|| "\"calls\" must be an array".to_string())?,
            None => return Err("the recorded responses need a \"calls\" array".to_string()),
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
    let service = call
        .get("service")
        .and_then(Json::as_str)
        .and_then(Service::from_name)
        .ok_or_else(|| bad("\"service\" must be docdb, cloudwatch, ec2 or secretsmanager"))?;
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
    Ok(Entry {
        service,
        operation,
        step,
    })
}

impl AwsClient for RecordedClient {
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
}
