//! The client seam of `toolkit bedrock-probes preflight`.
//!
//! The Python script called `boto3` (`bedrock-runtime` `converse` and
//! `invoke_model`, `service-quotas` `list_service_quotas`) and the `OpenAI`
//! SDK (the mantle Responses API and the runtime Chat/Responses APIs). Here
//! every call is one [`Service`] and operation with the request the script
//! built, handed to a [`ProbeClient`]. Tests inject scripted clients; the
//! command wires the [`RecordedClient`](super::recorded::RecordedClient)
//! (`--recorded FILE`) or [`NoLiveClient`], which sends nothing, reads no
//! credentials, generates no token and fails every call closed. A live client
//! belongs behind the `toolkit-aws` transport and signer seams and is a
//! follow-up (see `docs/migration/bedrock-probes.md`).

use toolkit_aws::AwsError;
use toolkit_core::Json;

/// The services a preflight probe talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// `bedrock-runtime` (`Converse`, `InvokeModel`).
    BedrockRuntime,
    /// `service-quotas` (`ListServiceQuotas`).
    Quotas,
    /// The `bedrock-mantle` `OpenAI`-compatible endpoint (`Responses`).
    Mantle,
    /// The `bedrock-runtime` `OpenAI`-compatible endpoint (`Responses`,
    /// `ChatCompletions`).
    RuntimeOpenai,
}

impl Service {
    /// The service name used in a recorded-response file.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::BedrockRuntime => "bedrock-runtime",
            Self::Quotas => "service-quotas",
            Self::Mantle => "bedrock-mantle",
            Self::RuntimeOpenai => "bedrock-runtime-openai",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [
            Self::BedrockRuntime,
            Self::Quotas,
            Self::Mantle,
            Self::RuntimeOpenai,
        ]
        .into_iter()
        .find(|service| service.name() == name)
    }

    /// Whether an error of this service carries an HTTP status the verdict
    /// reads (the `OpenAI` SDK surfaces; boto3 errors carry a code instead).
    pub(crate) fn reports_status(self) -> bool {
        matches!(self, Self::Mantle | Self::RuntimeOpenai)
    }
}

/// Why a call produced no response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeError {
    /// The build has no live client: nothing was sent.
    NoLive,
    /// The recording has no entry for this call and request.
    Unrecorded { service: Service, operation: String },
    /// A recorded failure (a service error response or a transport failure).
    Aws(AwsError),
}

/// Sends one operation and returns the response as a JSON object.
pub(crate) trait ProbeClient {
    /// # Errors
    ///
    /// The failure of the call.
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, ProbeError>;
}

/// The client of a run without `--recorded`: nothing is sent.
pub(crate) struct NoLiveClient;

impl ProbeClient for NoLiveClient {
    fn call(
        &mut self,
        _service: Service,
        _operation: &str,
        _request: &Json,
    ) -> Result<Json, ProbeError> {
        Err(ProbeError::NoLive)
    }
}
