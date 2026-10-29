//! Launch API client.

use serde_json::Value;

/// Convert an untrusted service error code to a locally owned constant.
#[must_use]
pub fn error_code(header: Option<&str>, body: &[u8]) -> &'static str {
    const CODES: &[&str] = &[
        "ConflictException",
        "InternalServerException",
        "ResourceNotFoundException",
        "ServiceUnavailableException",
        "ThrottlingException",
        "ValidationException",
        "AccessDeniedException",
        "UnauthorizedException",
        "UnrecognizedClientException",
        "ExpiredTokenException",
        "ForbiddenException",
        "BadRequestException",
        "NotFoundException",
        "TooManyRequestsException",
        "LimitExceededException",
        "ServiceQuotaExceededException",
        "RequestTimeoutException",
        "InternalFailure",
        "ServiceUnavailable",
    ];
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let candidate = header.or_else(|| {
        parsed.as_ref().and_then(|value| {
            ["__type", "code", "Code", "errorCode"]
                .iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
        })
    });
    let normalized = candidate
        .unwrap_or("")
        .rsplit('#')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    CODES
        .iter()
        .copied()
        .find(|code| *code == normalized)
        .unwrap_or("UnrecognizedError")
}

#[cfg(test)]
#[path = "api/tests_code.rs"]
mod tests_code;

#[cfg(test)]
#[path = "api/tests_ops.rs"]
mod tests_ops;

use super::http::{self, MonotonicClock, Operation};
use std::time::Duration;
use toolkit_aws::retry::{Jitter, RetryPolicy, Sleeper};
use toolkit_aws::stream::StreamingTransport;
use toolkit_aws::transport::{percent_encode, Headers, HttpRequest, Method, Timeouts};
use toolkit_aws::Secret;

#[derive(Clone, Copy)]
pub(crate) enum ApiOperation {
    CreateUploadUrl,
    CreateLaunch,
    GetLaunch,
    ListLaunches,
    StartExecution,
    Delete,
    #[cfg(test)]
    Put,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiError {
    InvalidResponse,
    Unreachable,
    InvalidRequest,
    InvalidLaunchId,
    Status {
        status: u16,
        code: &'static str,
    },
    Lookup {
        status: u16,
        code: &'static str,
        hint: &'static str,
    },
    PutStatus(u16),
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidResponse => {
                f.write_str("The Launch with AWS service returned an invalid response.")
            }
            Self::Unreachable => f.write_str("Could not reach Launch with AWS."),
            Self::InvalidRequest => {
                f.write_str("The operation failed. Re-run with logging enabled for details.")
            }
            Self::InvalidLaunchId => {
                f.write_str("Invalid launch id: it must be 1 to 2048 characters.")
            }
            Self::Status { status, code } => {
                write!(f, "Launch API request failed ({status}): {code}")
            }
            Self::Lookup { status, code, hint } => {
                write!(f, "GET /api/launches/<id> failed ({status}): {code}{hint}")
            }
            Self::PutStatus(status) => write!(f, "PUT (presigned S3 upload) failed ({status})"),
        }
    }
}

pub(crate) struct Client<'a> {
    pub transport: &'a dyn StreamingTransport,
    pub base: &'a str,
    pub region: &'a str,
    pub token: &'a Secret,
    pub clock: &'a dyn MonotonicClock,
    pub sleeper: &'a dyn Sleeper,
    pub jitter: &'a dyn Jitter,
    pub policy: RetryPolicy,
}
pub(crate) fn validate_launch_id(id: &str) -> Result<(), ApiError> {
    if (1..=2048).contains(&id.chars().count()) {
        Ok(())
    } else {
        Err(ApiError::InvalidLaunchId)
    }
}
impl Client<'_> {
    pub fn download_url(&self, id: &str, override_base: bool) -> Result<Option<String>, ApiError> {
        let result = self.call(ApiOperation::GetLaunch, id, &["download_url"], None).map_err(|error| match error {
            ApiError::Status { status, code } => {
                let hint = if status == 401 { " Hint: the backend rejected the Bearer token. Ensure you signed in successfully." }
                else if status == 404 && !override_base && self.region == "us-east-1" { " Hint: no launch with this ID exists in us-east-1. A launch exists only in the region it was created in; retry with --region set to that region (one of: us-east-1, eu-central-1)." }
                else if status == 404 && !override_base && self.region == "eu-central-1" { " Hint: no launch with this ID exists in eu-central-1. A launch exists only in the region it was created in; retry with --region set to that region (one of: us-east-1, eu-central-1)." }
                else { "" };
                ApiError::Lookup { status, code, hint }
            },
            other => other,
        })?;
        let launch = result
            .get("launch")
            .and_then(Value::as_object)
            .ok_or(ApiError::InvalidResponse)?;
        match launch.get("downloadUrl") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(url)) if url.is_empty() => Ok(None),
            Some(Value::String(url)) => Ok(Some(url.clone())),
            _ => Err(ApiError::InvalidResponse),
        }
    }
    pub fn call(
        &self,
        operation: ApiOperation,
        id: &str,
        include: &[&str],
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        if matches!(
            operation,
            ApiOperation::GetLaunch | ApiOperation::Delete | ApiOperation::StartExecution
        ) {
            validate_launch_id(id)?;
        }
        let retry = matches!(
            operation,
            ApiOperation::CreateUploadUrl
                | ApiOperation::CreateLaunch
                | ApiOperation::StartExecution
        ) && body
            .as_ref()
            .and_then(|v| v.get("clientToken"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let (method, path, stream_op) = match operation {
            ApiOperation::CreateUploadUrl => {
                (Method::Post, "/api/uploads".to_owned(), Operation::ApiJson)
            }
            ApiOperation::CreateLaunch => {
                (Method::Post, "/api/launches".to_owned(), Operation::ApiJson)
            }
            ApiOperation::GetLaunch => (
                Method::Get,
                format!("/api/launches/{}", percent_encode(id)),
                Operation::ApiJson,
            ),
            ApiOperation::ListLaunches => {
                (Method::Get, "/api/launches".to_owned(), Operation::ApiJson)
            }
            ApiOperation::StartExecution => (
                Method::Post,
                format!("/api/launches/{}/start", percent_encode(id)),
                Operation::ApiJson,
            ),
            ApiOperation::Delete => (
                Method::Delete,
                format!("/api/launches/{}", percent_encode(id)),
                Operation::Delete,
            ),
            #[cfg(test)]
            ApiOperation::Put => (Method::Put, String::new(), Operation::Put),
        };
        let mut url = format!("{}{}", self.base.trim_end_matches('/'), path);
        for (index, value) in include.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str("include=");
            url.push_str(&percent_encode(value));
        }
        let mut headers = Headers::new();
        {
            headers
                .insert("authorization", &format!("Bearer {}", self.token.expose()))
                .map_err(|_| ApiError::InvalidRequest)?;
            headers
                .insert("content-type", "application/json")
                .map_err(|_| ApiError::InvalidRequest)?;
        }
        let timeout = Duration::from_secs(120);
        let request = HttpRequest {
            method,
            url,
            headers,
            body: body.map_or_else(Vec::new, |v| v.to_string().into_bytes()),
            timeouts: Timeouts::new(timeout, timeout).map_err(|_| ApiError::InvalidRequest)?,
        };
        let put = stream_op == Operation::Put;
        let deadline = self.clock.now() + Duration::from_secs(if put { 300 } else { 360 });
        let response = http::run(
            self.transport,
            &request,
            stream_op,
            deadline,
            if put {
                Duration::from_secs(300)
            } else {
                timeout
            },
            !retry,
            self.clock,
            self.sleeper,
            self.jitter,
            &self.policy,
        )
        .map_err(|e| match e {
            http::BodyError::Deadline | http::BodyError::Transport { .. } => ApiError::Unreachable,
            _ => ApiError::InvalidResponse,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(if put {
                ApiError::PutStatus(response.status)
            } else {
                ApiError::Status {
                    status: response.status,
                    code: error_code(response.error_header.as_deref(), &response.body),
                }
            });
        }
        if put {
            return Ok(Value::Null);
        }
        if matches!(operation, ApiOperation::Delete) {
            return Ok(
                serde_json::json!({"deleted":true,"id":crate::safe_text::redacted_value(id),"region":self.region}),
            );
        }
        let shape = match operation {
            ApiOperation::CreateUploadUrl => "CreateUploadUrlOutput",
            ApiOperation::CreateLaunch => "CreateLaunchOutput",
            ApiOperation::GetLaunch => "GetLaunchOutput",
            ApiOperation::ListLaunches => "ListLaunchesOutput",
            ApiOperation::StartExecution => "StartLaunchExecutionOutput",
            ApiOperation::Delete => unreachable!(),
            #[cfg(test)]
            ApiOperation::Put => unreachable!(),
        };
        let body = std::str::from_utf8(&response.body).map_err(|_| ApiError::InvalidResponse)?;
        let shaped = super::shape::shape(shape, body).map_err(|_| ApiError::InvalidResponse)?;
        serde_json::from_str(&shaped).map_err(|_| ApiError::InvalidResponse)
    }

    #[cfg(unix)]
    pub fn put_archive(
        &self,
        url: &str,
        archive: &[u8],
        resolver: &dyn toolkit_aws::endpoint::HostResolver,
    ) -> Result<(), ApiError> {
        super::download::check_download_url(url, resolver).map_err(|_| ApiError::InvalidRequest)?;
        let authority = url
            .split_once("://")
            .ok_or(ApiError::InvalidRequest)?
            .1
            .split(['/', '?', '#'])
            .next()
            .ok_or(ApiError::InvalidRequest)?;
        let host = authority
            .split(':')
            .next()
            .ok_or(ApiError::InvalidRequest)?
            .to_ascii_lowercase();
        if host.len() <= ".amazonaws.com".len() || !host.ends_with(".amazonaws.com") {
            return Err(ApiError::InvalidRequest);
        }
        let mut headers = Headers::new();
        headers
            .insert("content-type", "application/zip")
            .map_err(|_| ApiError::InvalidRequest)?;
        let timeout = Duration::from_secs(300);
        let request = HttpRequest {
            method: Method::Put,
            url: url.to_owned(),
            headers,
            body: archive.to_vec(),
            timeouts: Timeouts::new(timeout, timeout).map_err(|_| ApiError::InvalidRequest)?,
        };
        let response = http::collect_response(
            self.transport,
            &request,
            Operation::Put,
            self.clock.now() + timeout,
        )
        .map_err(|e| match e {
            http::BodyError::Deadline | http::BodyError::Transport { .. } => ApiError::Unreachable,
            _ => ApiError::InvalidResponse,
        })?;
        if (200..300).contains(&response.status) {
            Ok(())
        } else {
            Err(ApiError::PutStatus(response.status))
        }
    }
}

#[cfg(test)]
#[path = "api/tests_api.rs"]
mod tests_api;
