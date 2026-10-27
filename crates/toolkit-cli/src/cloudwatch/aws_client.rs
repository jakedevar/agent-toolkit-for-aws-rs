//! The AWS client seam of the CloudWatch operations (#4381).
//!
//! The Python scripts called `boto3` clients (`application-signals`, `logs`,
//! `cloudwatchomni`, `bedrock-agentcore`, `bedrock-agentcore-control`). Here
//! every call is one [`Service`] and `PascalCase` API operation with the
//! request the script built as keyword arguments, handed to an [`AwsCalls`]
//! implementation. The commands wire either the
//! [`RecordedClient`](super::recorded::RecordedClient) (`--recorded FILE`,
//! deterministic replay) or [`NoLiveClient`], which sends nothing, reads no
//! credentials and fails every call. A live client behind the `toolkit-aws`
//! transport and signer seams is a follow-up (`docs/migration/cw-api.md`).

use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::errors::AwsFailure;

/// The AWS services the CloudWatch operations call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `application-signals`: the instrumentation configuration API.
    ApplicationSignals,
    /// `logs`: CloudWatch Logs.
    Logs,
    /// `cloudwatchomni`: Omni SQL span queries.
    CloudWatchOmni,
    /// `bedrock-agentcore`: the data plane (`evaluate`).
    BedrockAgentCore,
    /// `bedrock-agentcore-control`: datasets.
    BedrockAgentCoreControl,
}

impl Service {
    /// Every service, in the order a recorded file may name them.
    pub const ALL: [Service; 5] = [
        Self::ApplicationSignals,
        Self::Logs,
        Self::CloudWatchOmni,
        Self::BedrockAgentCore,
        Self::BedrockAgentCoreControl,
    ];

    /// The boto3 service name, used in a recorded-response file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ApplicationSignals => "application-signals",
            Self::Logs => "logs",
            Self::CloudWatchOmni => "cloudwatchomni",
            Self::BedrockAgentCore => "bedrock-agentcore",
            Self::BedrockAgentCoreControl => "bedrock-agentcore-control",
        }
    }

    /// The service named `name`, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.name() == name)
    }
}

/// Sends one operation and returns the response as a JSON object.
///
/// A service error response is `AwsError::Service` (its code drives the
/// tolerated-error rules); anything else (`Connect`, `NoCredentials`, ...) is a
/// transport failure, like the `BotoCoreError` family.
pub trait AwsCalls {
    /// # Errors
    ///
    /// The failure of the call.
    fn call(&mut self, service: Service, operation: &str, request: &Json)
        -> Result<Json, AwsError>;
}

/// The client of a run without `--recorded`: nothing is sent and no
/// credential is read.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLiveClient;

/// The message of every call that `NoLiveClient` refuses.
pub const NO_LIVE_CLIENT_MESSAGE: &str =
    "this build includes no live AWS client; pass --recorded FILE to replay recorded responses";

impl AwsCalls for NoLiveClient {
    fn call(
        &mut self,
        _service: Service,
        _operation: &str,
        _request: &Json,
    ) -> Result<Json, AwsError> {
        Err(AwsError::connect(NO_LIVE_CLIENT_MESSAGE))
    }
}

/// `str(botocore.exceptions.ClientError)` for a service error.
fn client_error_text(operation: &str, code: Option<&str>, message: &str) -> String {
    format!(
        "An error occurred ({}) when calling the {operation} operation: {message}",
        code.unwrap_or("Unknown")
    )
}

/// Maps an SDK failure onto the exception classes the legacy translator told
/// apart. `operation` is the API operation that failed (it is part of
/// `str(ClientError)`).
#[must_use]
pub fn failure_of(operation: &str, error: &AwsError) -> AwsFailure {
    match error {
        AwsError::Service(service) => AwsFailure::Client {
            code: service.code().map(str::to_string),
            message: Some(service.message().to_string()).filter(|text| !text.is_empty()),
            display: client_error_text(operation, service.code(), service.message()),
        },
        AwsError::NoCredentials => AwsFailure::Credentials {
            type_name: "NoCredentialsError".to_string(),
            message: error.to_string(),
        },
        AwsError::PartialCredentials { .. } => AwsFailure::Credentials {
            type_name: "PartialCredentialsError".to_string(),
            message: error.to_string(),
        },
        AwsError::Connect { message } => AwsFailure::EndpointConnection(message.clone()),
        AwsError::Timeout { message, .. } => AwsFailure::Timeout(message.clone()),
        other => AwsFailure::Other(other.to_string()),
    }
}

/// `str(exc)` of a failure: the text an f-string interpolating the caught
/// exception would show.
#[must_use]
pub fn failure_text(failure: &AwsFailure) -> &str {
    match failure {
        AwsFailure::Client { display, .. } => display,
        AwsFailure::EndpointConnection(text)
        | AwsFailure::Timeout(text)
        | AwsFailure::Other(text) => text,
        AwsFailure::Credentials { message, .. } => message,
    }
}

/// True for a failure of the `ClientError` class (a service-side error).
#[must_use]
pub fn is_client_error(failure: &AwsFailure) -> bool {
    matches!(failure, AwsFailure::Client { .. })
}

/// The `Error.Code` of a client failure, `""` for any other class (the
/// `_err_code` helper of the evaluation script).
#[must_use]
pub fn client_error_code(failure: &AwsFailure) -> &str {
    match failure {
        AwsFailure::Client { code, .. } => code.as_deref().unwrap_or(""),
        _ => "",
    }
}
