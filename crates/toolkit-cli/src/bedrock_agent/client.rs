//! The client seam of `toolkit bedrock-agent fetch`.
//!
//! The Python script called `boto3` clients (`sts`, `bedrock-agent`, `iam`,
//! `s3`). Here every call is one [`Service`] and `PascalCase` API operation with
//! the request the script built as keyword arguments, handed to an
//! [`AgentClient`]. Tests inject scripted clients; the command wires either the
//! [`RecordedClient`](super::recorded::RecordedClient) (`--recorded FILE`) or
//! [`NoLiveClient`], which sends nothing, reads no credentials and fails every
//! call. A live client belongs behind the `toolkit-aws` transport/signer seams
//! and is a follow-up (see `docs/migration/bedrock-fetch.md`).

use toolkit_aws::AwsError;
use toolkit_core::Json;

/// The AWS services the discovery reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// `sts`
    Sts,
    /// `bedrock-agent`
    BedrockAgent,
    /// `iam`
    Iam,
    /// `s3`
    S3,
}

impl Service {
    /// The service name used in a recorded-response file.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sts => "sts",
            Self::BedrockAgent => "bedrock-agent",
            Self::Iam => "iam",
            Self::S3 => "s3",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [Self::Sts, Self::BedrockAgent, Self::Iam, Self::S3]
            .into_iter()
            .find(|service| service.name() == name)
    }
}

/// Sends one operation and returns the response as a JSON object.
///
/// A service error response is `AwsError::Service` (its code drives the
/// tolerated-error rule); anything else (`Connect`, `NoCredentials`, ...) is a
/// transport failure that ends the run, like a non-`ClientError` exception.
pub(crate) trait AgentClient {
    /// # Errors
    ///
    /// The failure of the call.
    fn call(&mut self, service: Service, operation: &str, request: &Json)
        -> Result<Json, AwsError>;

    /// Whether the client knows `operation` (`hasattr(client, "<method>")` in
    /// the script: an SDK too old for multi-agent collaboration lacks it).
    fn supports(&self, _service: Service, _operation: &str) -> bool {
        true
    }
}

/// The client of a run without `--recorded`: nothing is sent.
pub(crate) struct NoLiveClient;

impl AgentClient for NoLiveClient {
    fn call(
        &mut self,
        _service: Service,
        _operation: &str,
        _request: &Json,
    ) -> Result<Json, AwsError> {
        Err(AwsError::connect(
            "this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        ))
    }
}
