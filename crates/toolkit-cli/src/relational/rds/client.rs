//! The client seam of `toolkit rds commitment`.
//!
//! The Python script called `boto3` clients (`rds` and `savingsplans`). Here
//! every call is one [`Service`], a `PascalCase` operation and the request the
//! script built as keyword arguments, handed to an [`AwsRead`]. Tests inject
//! scripted clients; the command wires the [`RecordedClient`]
//! (`--recorded FILE`) or [`NoLiveClient`], which sends nothing, reads no
//! credentials and fails every call, like a machine without `boto3`. A live
//! client behind the `toolkit-aws` transport and signer is a follow-up
//! (`docs/migration/relational.md`). No purchase operation exists in
//! [`Service`]'s vocabulary: the analyzer is read-only by construction.

use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use crate::relational::members::{optional_object, optional_string, same_json};

/// The AWS services the analyzer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// `rds`
    Rds,
    /// `savingsplans`
    SavingsPlans,
}

impl Service {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Rds => "rds",
            Self::SavingsPlans => "savingsplans",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [Self::Rds, Self::SavingsPlans]
            .into_iter()
            .find(|service| service.name() == name)
    }
}

/// Sends one read operation and returns the response as a JSON object.
///
/// A service error response is `AwsError::Service`; anything else is a
/// transport failure.
pub(crate) trait AwsRead {
    /// # Errors
    ///
    /// The failure of the call.
    fn call(
        &mut self,
        service: Service,
        region: &str,
        operation: &str,
        request: &Json,
    ) -> Result<Json, AwsError>;
}

/// The client of a run without `--recorded`: nothing is sent.
pub(crate) struct NoLiveClient;

impl AwsRead for NoLiveClient {
    fn call(
        &mut self,
        _service: Service,
        _region: &str,
        _operation: &str,
        _request: &Json,
    ) -> Result<Json, AwsError> {
        Err(AwsError::connect(
            "this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        ))
    }
}

enum Outcome {
    Response(Json),
    Service { code: String, message: String },
    Connect(String),
    NoCredentials,
}

struct Entry {
    service: Service,
    region: Option<String>,
    operation: String,
    request: Json,
    outcome: Outcome,
}

/// Replays the entries of a recorded-response file:
///
/// ```json
/// {"calls": [
///   {"service": "rds", "region": "us-east-1",
///    "operation": "DescribeReservedDBInstancesOfferings",
///    "request": {"DBInstanceClass": "db.r7g.2xlarge", "ProductDescription": "MySQL",
///                "MultiAZ": false},
///    "response": {"ReservedDBInstancesOfferings": []}},
///   {"service": "rds", "operation": "DescribeDBInstances",
///    "request": {"DBInstanceIdentifier": "prod"},
///    "error": {"code": "DBInstanceNotFound", "message": "no such instance"}}
/// ]}
/// ```
///
/// A call matches the entry with the same service, operation and request
/// (objects compare without regard to key order, so a paginated request must
/// carry the recorded token) and, when the entry has a `region`, the same
/// region. `request`, `region`, `error.kind`, `error.code` and `error.message`
/// are optional; a present member of another type, `null` included, rejects
/// the recording. `error.kind` is `client` (default, a service error
/// response), `connect` or `no_credentials`. An unmatched call fails with a
/// `Connect` error that names the service and operation only.
pub(crate) struct RecordedClient {
    entries: Vec<Entry>,
}

impl RecordedClient {
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
        Ok(Self { entries })
    }
}

fn parse_entry(index: usize, call: &Json) -> Result<Entry, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let service = call
        .get("service")
        .and_then(Json::as_str)
        .and_then(Service::from_name)
        .ok_or_else(|| bad("\"service\" must be rds or savingsplans"))?;
    let operation = call
        .get("operation")
        .and_then(Json::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| bad("\"operation\" must be a non-empty string"))?
        .to_string();
    let region = optional_string(call, "region")
        .map_err(|reason| bad(&reason))?
        .map(str::to_string);
    let request = optional_object(call, "request")
        .map_err(|reason| bad(&reason))?
        .cloned()
        .unwrap_or_else(|| Json::Object(Vec::new()));
    let outcome = match (call.get("response"), call.get("error")) {
        (Some(response @ Json::Object(_)), None) => Outcome::Response(response.clone()),
        (None, Some(error @ Json::Object(_))) => {
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
        region,
        operation,
        request,
        outcome,
    })
}

impl AwsRead for RecordedClient {
    fn call(
        &mut self,
        service: Service,
        region: &str,
        operation: &str,
        request: &Json,
    ) -> Result<Json, AwsError> {
        let entry = self.entries.iter().find(|entry| {
            entry.service == service
                && entry.operation == operation
                && entry
                    .region
                    .as_deref()
                    .is_none_or(|recorded| recorded == region)
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
}
