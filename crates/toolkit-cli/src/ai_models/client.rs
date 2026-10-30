//! The AWS client seam of the model tools.
//!
//! The Python scripts called `boto3` `sagemaker`, `cloudwatch` and `logs`
//! clients. Here every call is one [`Operation`] with the request the Python
//! code passed as keyword arguments, handed to a [`SageMakerClient`] that
//! answers with the response as JSON or a [`toolkit_aws::AwsError`].
//!
//! - [`NoLiveClient`] is the SDK-free build's client: nothing is sent and no
//!   credential is read; every call fails with a configuration error.
//! - [`super::recorded::RecordedClient`] replays recorded responses from a JSON
//!   document (`--recorded FILE`): see that module for the document and its
//!   validation. It is how the commands run offline, deterministically and
//!   without paid calls.
//!
//! Feature `ai-models-live` supplies a third implementation using the service
//! SDKs over toolkit-aws's `sdk` foundations; see `docs/migration/ai-models.md`.

use toolkit_aws::AwsError;
use toolkit_core::Json;

/// One call a tool makes against the target account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Operation {
    /// `sagemaker` `ListHubContents`.
    ListHubContents,
    /// `sagemaker` `DescribeHubContent`.
    DescribeHubContent,
    /// `sagemaker` `Search` with `Resource=HubContent`.
    Search,
    /// `sagemaker` `DescribeEndpoint`.
    DescribeEndpoint,
    /// `cloudwatch` `GetMetricData`.
    GetMetricData,
    /// `logs` `FilterLogEvents`.
    FilterLogEvents,
}

impl Operation {
    pub(super) const ALL: [Self; 6] = [
        Self::ListHubContents,
        Self::DescribeHubContent,
        Self::Search,
        Self::DescribeEndpoint,
        Self::GetMetricData,
        Self::FilterLogEvents,
    ];

    /// The API operation name, also the key of a recorded document.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::ListHubContents => "ListHubContents",
            Self::DescribeHubContent => "DescribeHubContent",
            Self::Search => "Search",
            Self::DescribeEndpoint => "DescribeEndpoint",
            Self::GetMetricData => "GetMetricData",
            Self::FilterLogEvents => "FilterLogEvents",
        }
    }
}

/// Sends one operation and returns the response object.
pub(super) trait SageMakerClient {
    /// # Errors
    ///
    /// The failure of the call (access denied, throttling, a missing model...).
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, AwsError>;

    /// Whether this client can issue `Search(Resource="HubContent")`: the
    /// counterpart of the Python `boto3`/`botocore` minimum-version gate. A
    /// live binding returns the reason when its SDK build lacks `HubContent`
    /// Search; the caller then uses the complete `ListHubContents` fallback.
    ///
    /// # Errors
    ///
    /// The reason Search cannot be used.
    fn check_search_support(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The client of this build: nothing is sent.
#[cfg_attr(feature = "ai-models-live", allow(dead_code))]
pub(super) struct NoLiveClient;

impl SageMakerClient for NoLiveClient {
    fn call(&mut self, _operation: Operation, _request: &Json) -> Result<Json, AwsError> {
        Err(AwsError::invalid_config(
            "client",
            "this build has no live AWS client and sent nothing: built without ai-models-live; replay recorded responses \
             with --recorded FILE",
        ))
    }
}
