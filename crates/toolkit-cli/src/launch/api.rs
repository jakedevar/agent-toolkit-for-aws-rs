//! Launch API client.

use serde_json::Value;

/// Convert an untrusted service error code to a locally owned constant.
#[must_use]
pub fn error_code(header: Option<&str>, body: &[u8]) -> &'static str {
    const CODES: &[&str] = &[
        "ConflictException", "InternalServerException", "ResourceNotFoundException",
        "ServiceUnavailableException", "ThrottlingException", "ValidationException",
        "AccessDeniedException", "UnauthorizedException", "UnrecognizedClientException",
        "ExpiredTokenException", "ForbiddenException", "BadRequestException", "NotFoundException",
        "TooManyRequestsException", "LimitExceededException", "ServiceQuotaExceededException",
        "RequestTimeoutException", "InternalFailure", "ServiceUnavailable",
    ];
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let candidate = header.or_else(|| {
        parsed.as_ref().and_then(|value| {
            ["__type", "code", "Code", "errorCode"]
                .iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
        })
    });
    let normalized = candidate.unwrap_or("").rsplit('#').next().unwrap_or("")
        .split(':').next().unwrap_or("");
    CODES.iter().copied().find(|code| *code == normalized).unwrap_or("UnrecognizedError")
}

#[cfg(test)]
#[path = "api/tests_code.rs"]
mod tests_code;

#[cfg(test)]
#[path = "api/tests_ops.rs"]
mod tests_ops;

use std::time::Duration;
use toolkit_aws::retry::{Jitter, RetryPolicy, Sleeper};
use toolkit_aws::stream::StreamingTransport;
use toolkit_aws::transport::{percent_encode, Headers, HttpRequest, Method, Timeouts};
use toolkit_aws::Secret;
use super::http::{self, MonotonicClock, Operation};

#[derive(Clone, Copy)]
pub(crate) enum ApiOperation { CreateUploadUrl, CreateLaunch, GetLaunch, ListLaunches, StartExecution, Delete, Put }

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiError {
    InvalidResponse,
    Unreachable,
    InvalidRequest,
    Status { status: u16, code: &'static str },
    PutStatus(u16),
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidResponse => f.write_str("The Launch with AWS service returned an invalid response."),
            Self::Unreachable => f.write_str("Could not reach Launch with AWS."),
            Self::InvalidRequest => f.write_str("The operation failed. Re-run with logging enabled for details."),
            Self::Status { status, code } => write!(f, "Launch API request failed ({status}): {code}"),
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
impl Client<'_> {
    pub fn call(&self, operation: ApiOperation, id: &str, include: &[&str], body: Option<Value>) -> Result<Value, ApiError> {
        let (method, path, stream_op) = match operation {
            ApiOperation::CreateUploadUrl => (Method::Post, "/api/uploads".to_owned(), Operation::ApiJson),
            ApiOperation::CreateLaunch => (Method::Post, "/api/launches".to_owned(), Operation::ApiJson),
            ApiOperation::GetLaunch => (Method::Get, format!("/api/launches/{}", percent_encode(id)), Operation::ApiJson),
            ApiOperation::ListLaunches => (Method::Get, "/api/launches".to_owned(), Operation::ApiJson),
            ApiOperation::StartExecution => (Method::Post, format!("/api/launches/{}/start", percent_encode(id)), Operation::ApiJson),
            ApiOperation::Delete => (Method::Delete, format!("/api/launches/{}", percent_encode(id)), Operation::Delete),
            ApiOperation::Put => (Method::Put, String::new(), Operation::Put),
        };
        let mut url = format!("{}{}", self.base.trim_end_matches('/'), path);
        for (index, value) in include.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str("include=");
            url.push_str(&percent_encode(value));
        }
        let mut headers = Headers::new();
        if !matches!(operation, ApiOperation::Put) {
            headers.insert("authorization", &format!("Bearer {}", self.token.expose())).map_err(|_| ApiError::InvalidRequest)?;
            headers.insert("content-type", "application/json").map_err(|_| ApiError::InvalidRequest)?;
        }
        let timeout = Duration::from_secs(120);
        let request = HttpRequest { method, url, headers, body: body.map_or_else(Vec::new, |v| v.to_string().into_bytes()), timeouts: Timeouts::new(timeout, timeout).map_err(|_| ApiError::InvalidRequest)? };
        let put = matches!(operation, ApiOperation::Put);
        let deadline = self.clock.now() + Duration::from_secs(if put { 300 } else { 360 });
        let response = http::run(self.transport, &request, stream_op, deadline, if put { Duration::from_secs(300) } else { timeout }, put, self.clock, self.sleeper, self.jitter, &self.policy).map_err(|e| match e {
            http::BodyError::Deadline | http::BodyError::Transport { .. } => ApiError::Unreachable,
            _ => ApiError::InvalidResponse,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(if put { ApiError::PutStatus(response.status) } else { ApiError::Status { status: response.status, code: error_code(response.error_header.as_deref(), &response.body) } });
        }
        if put { return Ok(Value::Null); }
        if matches!(operation, ApiOperation::Delete) {
            return Ok(serde_json::json!({"deleted":true,"id":crate::safe_text::redacted_value(id),"region":self.region}));
        }
        let result: Value = serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidResponse)?;
        if !result.is_object() { return Err(ApiError::InvalidResponse); }
        Ok(result)
    }
}
