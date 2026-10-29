//! Bounded merchant requests over an injected streaming transport.
//!
//! The live transport must use the guarded resolver and payment roots. System
//! DNS is blocking; deadline admission is repeated after each lookup.

use std::time::{Duration, Instant};
use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::error::{AwsError, TimeoutPhase};
use toolkit_aws::stream::{StreamError, StreamLimits, StreamingTransport};
use toolkit_aws::transport::{Headers, HttpRequest, Method, Timeouts};

use super::policy::{assert_public_https_url, PolicyError};
use crate::safe_text::shown_value;

/// An injectable monotonic clock for operation deadline admission.
pub trait Clock {
    /// Current monotonic time.
    fn now(&self) -> Instant;
}
/// The process monotonic clock.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A bounded merchant response. Redirect locations are deliberately omitted.
#[derive(Clone, PartialEq, Eq)]
pub struct MerchantResponse {
    /// HTTP status, including redirects.
    pub status: u16,
    /// Response headers except Location.
    pub headers: Headers,
    /// Raw bytes, never decompressed.
    pub body: Vec<u8>,
}
impl std::fmt::Debug for MerchantResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MerchantResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// A model-facing refusal, containing fixed or sanitized text only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetError(pub String);
impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for NetError {}
fn timeout() -> NetError {
    NetError("ReadTimeout during payment flow.".into())
}
fn aws_error(error: &AwsError) -> NetError {
    let name = match error {
        AwsError::Timeout {
            phase: TimeoutPhase::Connect,
            ..
        } => "ConnectTimeout",
        AwsError::Timeout { .. } => "ReadTimeout",
        AwsError::Connect { .. } => "ConnectError",
        _ => "RuntimeError",
    };
    NetError(format!("{name} during payment flow."))
}
fn safe_method(method: &str) -> bool {
    method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD")
}

/// One request's limits and operation deadline.
pub struct MerchantRequest<'a> {
    /// Merchant URL.
    pub url: &'a str,
    /// GET or HEAD, case insensitive.
    pub method: &'a str,
    /// Optional validated payment proof header (name, value).
    pub proof: Option<(&'a str, &'a str)>,
    /// Total body cap in bytes.
    pub body_cap: u64,
    /// Per-request idle timeout.
    pub timeout: Duration,
    /// Deadline shared with the payment flow.
    pub operation_deadline: Instant,
}

/// Merchant HTTP seam for the flow.
pub trait MerchantHttp {
    /// Send one bounded request.
    fn request(&self, request: &MerchantRequest<'_>) -> Result<MerchantResponse, NetError>;
}

/// Adapter over a prebuilt, guarded transport.
pub struct StreamingMerchant<T, R, C> {
    /// No-proxy, guarded transport supplied by the wiring layer.
    pub transport: T,
    /// Origin pre-check resolver.
    pub resolver: R,
    /// Monotonic clock.
    pub clock: C,
}
impl<T: StreamingTransport, R: HostResolver, C: Clock> MerchantHttp for StreamingMerchant<T, R, C> {
    fn request(&self, input: &MerchantRequest<'_>) -> Result<MerchantResponse, NetError> {
        if !safe_method(input.method) {
            return Err(NetError(format!("Method {} is not allowed; this skill fetches paid content with GET or HEAD. A request body would let the agent send data to an arbitrary origin, which the policy gate does not validate.", shown_value(&input.method.to_uppercase()))));
        }
        if self.clock.now() >= input.operation_deadline {
            return Err(timeout());
        }
        let origin = assert_public_https_url(input.url, &self.resolver);
        if self.clock.now() >= input.operation_deadline {
            return Err(timeout());
        }
        origin.map_err(|e: PolicyError| NetError(e.reason()))?;
        let now = self.clock.now();
        if now >= input.operation_deadline {
            return Err(timeout());
        }
        let request_deadline = now
            .checked_add(input.timeout.saturating_mul(3))
            .unwrap_or(input.operation_deadline)
            .min(input.operation_deadline);
        let mut headers = Headers::new();
        headers
            .insert("accept", "application/json, text/plain")
            .map_err(|e| aws_error(&e))?;
        headers
            .insert("accept-encoding", "identity")
            .map_err(|e| aws_error(&e))?;
        headers
            .insert(
                "user-agent",
                concat!("aws-agents-pay/", env!("CARGO_PKG_VERSION")),
            )
            .map_err(|e| aws_error(&e))?;
        if let Some((name, value)) = input.proof {
            if !name.eq_ignore_ascii_case("payment-signature")
                && !name.eq_ignore_ascii_case("x-payment")
            {
                return Err(NetError("RuntimeError during payment flow.".into()));
            }
            headers
                .insert_sensitive(name, value)
                .map_err(|e| aws_error(&e))?;
        }
        let request = HttpRequest {
            method: if input.method.eq_ignore_ascii_case("GET") {
                Method::Get
            } else {
                Method::Head
            },
            url: input.url.to_owned(),
            headers,
            body: Vec::new(),
            timeouts: Timeouts::new(input.timeout.min(Duration::from_secs(5)), input.timeout)
                .map_err(|e| aws_error(&e))?,
        };
        let limits = StreamLimits::new(input.body_cap, 64 * 1024)
            .map_err(|e| aws_error(&e))?
            .with_deadline(request_deadline);
        if self.clock.now() >= input.operation_deadline {
            return Err(timeout());
        }
        let mut stream = self
            .transport
            .open(&request, &limits)
            .map_err(|e| aws_error(&e))?;
        let status = stream.status();
        let mut response_headers = Headers::new();
        for (name, value) in stream.headers().entries() {
            if name != "location" {
                response_headers
                    .insert(name, value)
                    .map_err(|e| aws_error(&e))?;
            }
        }
        stream.narrow_body_limit(input.body_cap);
        if let Some(value) = stream.headers().get("content-length") {
            if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
                let digits = value.trim_start_matches('0');
                let cap = input.body_cap.to_string();
                if digits.len() > cap.len() || (digits.len() == cap.len() && digits > cap.as_str())
                {
                    return Err(NetError(format!(
                        "Response declares {} bytes, over the {} limit.",
                        shown_value(value),
                        input.body_cap
                    )));
                }
            }
        }
        let mut body = Vec::new();
        loop {
            match stream.next_chunk() {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(StreamError::TooLarge) => {
                    return Err(NetError(format!(
                        "Response exceeded {} bytes; refusing to buffer more.",
                        input.body_cap
                    )))
                }
                Err(StreamError::IdleTimeout | StreamError::DeadlineExceeded) => {
                    return Err(timeout())
                }
                Err(StreamError::Failed(e)) => return Err(aws_error(&e)),
            }
        }
        Ok(MerchantResponse {
            status,
            headers: response_headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests;
