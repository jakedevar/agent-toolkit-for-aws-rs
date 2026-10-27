//! The AWS client seam of the model tools.
//!
//! The Python scripts called `boto3` `sagemaker`, `cloudwatch` and `logs`
//! clients. Here every call is one [`Operation`] with the request the Python
//! code passed as keyword arguments, handed to a [`SageMakerClient`] that
//! answers with the response as JSON or a [`toolkit_aws::AwsError`].
//!
//! - [`NoLiveClient`] is the client of this build: nothing is sent and no
//!   credential is read; every call fails with a configuration error.
//! - [`RecordedClient`] replays recorded responses from a JSON document
//!   (`--recorded FILE`), checks each request against the recorded one when the
//!   step carries it, and fails closed when the recording runs out. It is how
//!   the commands run offline, deterministically and without paid calls.
//!
//! A live binding (the `aws-sdk-sagemaker` client behind `toolkit-aws`'s `sdk`
//! feature) is a follow-up; see `docs/migration/ai-models.md`.
//!
//! Recorded document:
//!
//! ```json
//! {"calls": {"ListHubContents": [
//!   {"request": {"HubName": "h", "HubContentType": "Model", "MaxResults": 100},
//!    "response": {"HubContentSummaries": [], "NextToken": "t"}},
//!   {"error": {"kind": "service", "code": "AccessDeniedException",
//!              "message": "denied", "status": 400}}]}}
//! ```
//!
//! `request` is optional. `error.kind` is `service` (default), `no_credentials`
//! or `connect`.

use std::collections::{BTreeMap, VecDeque};

use toolkit_aws::{AwsError, ServiceError};
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
    const ALL: [Self; 6] = [
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
pub(super) struct NoLiveClient;

impl SageMakerClient for NoLiveClient {
    fn call(&mut self, _operation: Operation, _request: &Json) -> Result<Json, AwsError> {
        Err(AwsError::invalid_config(
            "client",
            "this build has no live AWS client and sent nothing; replay recorded responses \
             with --recorded FILE",
        ))
    }
}

enum Step {
    Response {
        request: Option<Json>,
        response: Json,
    },
    Failure(AwsError),
}

/// Replays the recorded responses of a JSON document, in order per operation.
pub(super) struct RecordedClient {
    calls: BTreeMap<Operation, VecDeque<Step>>,
}

fn object_keys<'a>(
    value: &'a Json,
    allowed: &[&str],
    what: &str,
) -> Result<&'a [(String, Json)], String> {
    let Json::Object(members) = value else {
        return Err(format!("{what} must be an object"));
    };
    if members
        .iter()
        .any(|(key, _)| !allowed.contains(&key.as_str()))
    {
        return Err(format!(
            "{what} may only contain {}",
            allowed
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(members)
}

fn parse_failure(value: &Json, at: &str) -> Result<AwsError, String> {
    let members = object_keys(value, &["kind", "code", "message", "status"], at)?;
    let text = |key: &str| -> Result<Option<&str>, String> {
        match members.iter().find(|(name, _)| name == key) {
            None | Some((_, Json::Null)) => Ok(None),
            Some((_, Json::String(text))) => Ok(Some(text.as_str())),
            Some(_) => Err(format!("{at}.{key} must be a string")),
        }
    };
    let message = text("message")?.unwrap_or("");
    match text("kind")?.unwrap_or("service") {
        "service" => {
            let status = match members.iter().find(|(name, _)| name == "status") {
                None => 400,
                Some((_, Json::Int(status))) if (100..=599).contains(status) => {
                    u16::try_from(*status).map_err(|_| format!("{at}.status is out of range"))?
                }
                Some(_) => return Err(format!("{at}.status must be an HTTP status code")),
            };
            Ok(AwsError::service(ServiceError::new(
                "POST",
                "/",
                status,
                text("code")?,
                message,
                None,
            )))
        }
        "no_credentials" => Ok(AwsError::NoCredentials),
        "connect" => Ok(AwsError::connect(message)),
        _ => Err(format!(
            "{at}.kind must be service, no_credentials or connect"
        )),
    }
}

fn parse_step(value: &Json, at: &str) -> Result<Step, String> {
    let members = object_keys(value, &["request", "response", "error"], at)?;
    let find = |key: &str| members.iter().find(|(name, _)| name == key).map(|(_, v)| v);
    match (find("response"), find("error")) {
        (Some(response @ Json::Object(_)), None) => Ok(Step::Response {
            request: find("request").cloned(),
            response: response.clone(),
        }),
        (None, Some(error)) => Ok(Step::Failure(parse_failure(error, &format!("{at}.error"))?)),
        _ => Err(format!(
            "{at} needs exactly one of `response` (an object) or `error`"
        )),
    }
}

impl RecordedClient {
    /// Reads a recorded document.
    ///
    /// # Errors
    ///
    /// A description of the first malformed part. Values of the document are
    /// never echoed; positions are.
    pub(super) fn from_json(document: &Json) -> Result<Self, String> {
        let top = object_keys(document, &["calls"], "the recorded input")?;
        let Some((_, calls)) = top.first() else {
            return Err("the recorded input needs a `calls` object".to_string());
        };
        let names: Vec<&str> = Operation::ALL.iter().map(|op| op.name()).collect();
        let by_operation = object_keys(calls, &names, "`calls`")?;
        let mut parsed = BTreeMap::new();
        for (name, steps) in by_operation {
            let Some(operation) = Operation::ALL.iter().find(|op| op.name() == name) else {
                continue;
            };
            let Json::Array(steps) = steps else {
                return Err(format!("calls.{name} must be a list of steps"));
            };
            let queue = steps
                .iter()
                .enumerate()
                .map(|(index, step)| parse_step(step, &format!("calls.{name}[{index}]")))
                .collect::<Result<VecDeque<_>, _>>()?;
            parsed.insert(*operation, queue);
        }
        Ok(Self { calls: parsed })
    }
}

impl SageMakerClient for RecordedClient {
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, AwsError> {
        let Some(step) = self.calls.get_mut(&operation).and_then(VecDeque::pop_front) else {
            return Err(AwsError::invalid_response(format!(
                "the recorded input has no further response for {}",
                operation.name()
            )));
        };
        match step {
            Step::Failure(error) => Err(error),
            Step::Response {
                request: expected,
                response,
            } => {
                if expected
                    .as_ref()
                    .is_some_and(|expected| expected != request)
                {
                    return Err(AwsError::invalid_response(format!(
                        "the {} request differs from the recorded request",
                        operation.name()
                    )));
                }
                Ok(response)
            }
        }
    }
}
