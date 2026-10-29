//! Safe `AgentCore` error classification and endpoint construction.

use crate::agents_pay::policy::Approved;
use toolkit_aws::{AwsError, Endpoint};
use toolkit_core::parse_json;
use toolkit_core::Json;

const CODES: &[&str] = &[
    "AccessDeniedException",
    "ConflictException",
    "InternalServerException",
    "ResourceNotFoundException",
    "ServiceQuotaExceededException",
    "SubscriptionRequiredException",
    "ThrottlingException",
    "ValidationException",
    "ServiceUnavailableException",
    "UnauthorizedException",
    "UnrecognizedClientException",
    "ExpiredTokenException",
    "ForbiddenException",
    "BadRequestException",
    "NotFoundException",
    "TooManyRequestsException",
    "LimitExceededException",
    "RequestTimeoutException",
    "InternalFailure",
    "ServiceUnavailable",
];

/// Classify a server error without returning any server-supplied text.
#[must_use]
pub fn error_code(header: Option<&str>, body: &str) -> &'static str {
    let parsed = parse_json(body).ok();
    let candidate = header.or_else(|| {
        parsed.as_ref().and_then(|json| {
            ["__type", "code", "Code"]
                .iter()
                .find_map(|key| json.get(key).and_then(|v| v.as_str()))
        })
    });
    let Some(candidate) = candidate else {
        return "UnrecognizedError";
    };
    let candidate = candidate
        .rsplit('#')
        .next()
        .unwrap_or(candidate)
        .split(':')
        .next()
        .unwrap_or(candidate);
    CODES
        .iter()
        .copied()
        .find(|code| *code == candidate)
        .unwrap_or("UnrecognizedError")
}

/// Whether a service error code is eligible for retry. A conflict is final.
#[must_use]
pub fn is_retryable(code: &str) -> bool {
    matches!(
        code,
        "ThrottlingException"
            | "TooManyRequestsException"
            | "RequestTimeoutException"
            | "InternalFailure"
            | "InternalServerException"
            | "ServiceUnavailableException"
            | "ServiceUnavailable"
    )
}

/// Build the standard `AgentCore` endpoint without reading endpoint overrides.
///
/// # Errors
/// Returns an invalid configuration error for an invalid region.
pub fn endpoint(region: &str) -> Result<String, AwsError> {
    Endpoint::for_service("bedrock-agentcore", region).map(|endpoint| endpoint.url())
}

fn process_body(approved: &Approved) -> String {
    let mut payload = match approved.entry() {
        Json::Object(entries) => entries.clone(),
        _ => unreachable!("policy only approves objects"),
    };
    payload.retain(|(key, _)| key != "maxAmountRequired" && key != "amount");
    payload.push(("amount".into(), Json::string(approved.amount())));
    Json::object(vec![
        ("paymentManagerArn", Json::string(approved.manager_arn())),
        ("paymentSessionId", Json::string(approved.session_id())),
        (
            "paymentInstrumentId",
            Json::string(approved.instrument_id()),
        ),
        ("paymentType", Json::string("CRYPTO_X402")),
        (
            "paymentInput",
            Json::object(vec![(
                "cryptoX402",
                Json::object(vec![
                    ("version", Json::string(approved.version().to_string())),
                    ("payload", Json::Object(payload)),
                ]),
            )]),
        ),
        ("clientToken", Json::string(approved.token())),
    ])
    .to_compact_string()
}

/// Immutable payment request derived solely from an approved selection.
///
/// The constructor is private to this module.
///
/// ```compile_fail
/// use toolkit_cli::agents_pay::{agentcore::ProcessPaymentRequest, policy::Approved};
/// fn forge(approved: &Approved) { let _ = ProcessPaymentRequest::from_approved(approved); }
/// ```
#[allow(dead_code)] // The production adapter will consume these fields in S4.
pub struct ProcessPaymentRequest {
    body: Vec<u8>,
    manager: String,
    session: String,
    instrument: String,
    user: String,
}

impl ProcessPaymentRequest {
    #[allow(dead_code)] // Used by the test prototype until the production adapter is complete.
    fn from_approved(approved: &Approved) -> Self {
        Self {
            body: process_body(approved).into_bytes(),
            manager: approved.manager_arn().into(),
            session: approved.session_id().into(),
            instrument: approved.instrument_id().into(),
            user: approved.user_id().into(),
        }
    }
}

impl std::fmt::Debug for ProcessPaymentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProcessPaymentRequest([REDACTED])")
    }
}

#[cfg(test)]
fn get_body(manager: &str, session: &str) -> String {
    Json::object(vec![
        ("paymentManagerArn", Json::string(manager)),
        ("paymentSessionId", Json::string(session)),
    ])
    .to_compact_string()
}

#[cfg(test)]
fn session_body(manager: &str, budget: &str, expiry: i64, token: &str) -> String {
    Json::object(vec![
        ("paymentManagerArn", Json::string(manager)),
        (
            "limits",
            Json::object(vec![(
                "maxSpendAmount",
                Json::object(vec![
                    ("value", Json::string(budget)),
                    ("currency", Json::string("USD")),
                ]),
            )]),
        ),
        ("expiryTimeInMinutes", Json::Int(expiry)),
        ("clientToken", Json::string(token)),
    ])
    .to_compact_string()
}

#[cfg(test)]
fn instrument_body(
    manager: &str,
    connector: &str,
    network: &str,
    email: &str,
    token: &str,
) -> String {
    Json::object(vec![
        ("paymentManagerArn", Json::string(manager)),
        ("paymentConnectorId", Json::string(connector)),
        (
            "paymentInstrumentType",
            Json::string("EMBEDDED_CRYPTO_WALLET"),
        ),
        (
            "paymentInstrumentDetails",
            Json::object(vec![(
                "embeddedCryptoWallet",
                Json::object(vec![
                    ("network", Json::string(network)),
                    (
                        "linkedAccounts",
                        Json::Array(vec![Json::object(vec![(
                            "email",
                            Json::object(vec![("emailAddress", Json::string(email))]),
                        )])]),
                    ),
                ]),
            )]),
        ),
        ("clientToken", Json::string(token)),
    ])
    .to_compact_string()
}

#[cfg(test)]
fn agent_name(runtime: bool) -> &'static str {
    if runtime {
        "openclaw-aws-agents-pay"
    } else {
        "aws-agents-pay"
    }
}

#[cfg(test)]
fn runtime_region(
    config: Option<&str>,
    env: Option<&str>,
    arn: &str,
    profile: Option<&str>,
) -> Option<String> {
    let arn_region = arn
        .strip_prefix("arn:")
        .and_then(|s| s.split(':').nth(2))
        .filter(|s| !s.is_empty());
    let region = config
        .filter(|s| !s.is_empty())
        .or(env.filter(|s| !s.is_empty()))
        .or(arn_region)
        .or(profile)?;
    toolkit_aws::is_valid_region(region).then(|| region.to_owned())
}

#[cfg(test)]
fn valid_process_response(body: &str, manager: &str, session: &str, instrument: &str) -> bool {
    if body.len() > 65_536 {
        return false;
    }
    let Ok(value) = parse_json(body) else {
        return false;
    };
    let text = |key| {
        toolkit_aws::recorded::optional_text(&value, key)
            .ok()
            .flatten()
    };
    if text("paymentManagerArn") != Some(manager)
        || text("paymentSessionId") != Some(session)
        || text("paymentInstrumentId") != Some(instrument)
        || text("paymentType") != Some("CRYPTO_X402")
        || text("status") != Some("PROOF_GENERATED")
        || text("processPaymentId").is_none()
    {
        return false;
    }
    for key in ["createdAt", "updatedAt"] {
        if !matches!(value.get(key), Some(Json::Int(_) | Json::Float(_))) {
            return false;
        }
    }
    let Ok(Some(output)) = toolkit_aws::recorded::optional_object(&value, "paymentOutput") else {
        return false;
    };
    let Ok(Some(crypto)) = toolkit_aws::recorded::optional_object(output, "cryptoX402") else {
        return false;
    };
    if output.get("mpp").is_some()
        || toolkit_aws::recorded::optional_text(crypto, "version")
            .ok()
            .flatten()
            .is_none()
    {
        return false;
    }
    let Some(payload) = crypto.get("payload") else {
        return false;
    };
    let parsed;
    let payload = if let Some(s) = payload.as_str() {
        if s.len() > 16_384 {
            return false;
        }
        parsed = match parse_json(s) {
            Ok(v) => v,
            Err(_) => return false,
        };
        &parsed
    } else {
        payload
    };
    matches!(payload, Json::Object(_)) && payload.to_compact_string().len() <= 16_384
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_error_codes_are_allowlisted_and_conflict_is_not_retryable() {
        assert_eq!(
            error_code(Some("aws#ValidationException:secret"), ""),
            "ValidationException"
        );
        assert_eq!(
            error_code(
                None,
                r#"{"__type":"aws#ThrottlingException","message":"secret"}"#
            ),
            "ThrottlingException"
        );
        assert_eq!(
            error_code(Some("secret"), r#"{"code":"ValidationException"}"#),
            "UnrecognizedError"
        );
        assert!(!is_retryable("ConflictException"));
        assert!(is_retryable("ThrottlingException"));
    }

    #[test]
    fn aws_endpoint_is_fixed_to_the_service_and_region() {
        assert_eq!(
            endpoint("us-east-1").unwrap(),
            "https://bedrock-agentcore.us-east-1.amazonaws.com"
        );
    }
}

#[cfg(test)]
mod operation_contracts {
    use super::*;
    use crate::agents_pay::policy::{authorize, FixedResolver, PaymentRequest, Resources};
    use toolkit_core::parse_json;

    pub(super) fn approved() -> crate::agents_pay::policy::Approved {
        let policy = parse_json(r#"{"max_per_payment_usd":"0.10","allowed_networks":["eip155:84532"],"allowed_assets":{"eip155:84532":["0xa"]},"allowed_recipients":["0xb"]}"#).unwrap();
        let challenge = parse_json(r#"{"x402Version":2,"accepts":[{"scheme":"exact","network":"eip155:84532","asset":"0xA","payTo":"0xB","amount":"100000"}]}"#).unwrap();
        let resources = Resources {
            payment_manager_arn: Some("arn".into()),
            payment_instrument_id: Some("pi".into()),
            payment_session_id: Some("ps".into()),
            user_id: Some("user".into()),
        };
        authorize(
            &PaymentRequest {
                url: "https://93.184.216.34/item",
                challenge: &challenge,
                policy: &policy,
                resources: &resources,
                purchase_id: None,
            },
            &FixedResolver::new(),
        )
        .unwrap()
    }

    #[test]
    fn t_aws_process_request_contains_only_approved_terms() {
        let approved = approved();
        let token = approved.token().to_owned();
        let body = process_body(&approved);
        assert_eq!(
            body,
            format!(
                r#"{{"paymentManagerArn":"arn","paymentSessionId":"ps","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","paymentInput":{{"cryptoX402":{{"version":"2","payload":{{"scheme":"exact","network":"eip155:84532","asset":"0xA","payTo":"0xB","amount":"100000"}}}}}},"clientToken":"{token}"}}"#
            )
        );
    }

    #[test]
    fn t_aws_other_request_bodies_and_agent_names() {
        assert_eq!(
            get_body("arn", "ps"),
            r#"{"paymentManagerArn":"arn","paymentSessionId":"ps"}"#
        );
        assert_eq!(
            session_body("arn", "10", 15, "token"),
            r#"{"paymentManagerArn":"arn","limits":{"maxSpendAmount":{"value":"10","currency":"USD"}},"expiryTimeInMinutes":15,"clientToken":"token"}"#
        );
        assert_eq!(
            instrument_body("arn", "connector", "ETHEREUM", "user@example.com", "token"),
            r#"{"paymentManagerArn":"arn","paymentConnectorId":"connector","paymentInstrumentType":"EMBEDDED_CRYPTO_WALLET","paymentInstrumentDetails":{"embeddedCryptoWallet":{"network":"ETHEREUM","linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]}},"clientToken":"token"}"#
        );
        assert_eq!(agent_name(true), "openclaw-aws-agents-pay");
        assert_eq!(agent_name(false), "aws-agents-pay");
    }

    #[test]
    fn d19_region_precedence_and_invalid_inputs() {
        let arn = "arn:aws:bedrock-agentcore:us-west-2:123:payment-manager/test";
        assert_eq!(
            runtime_region(Some("eu-west-1"), Some("us-east-1"), arn, Some("us-west-1")),
            Some("eu-west-1".into())
        );
        assert_eq!(
            runtime_region(None, Some("us-east-1"), arn, Some("us-west-1")),
            Some("us-east-1".into())
        );
        assert_eq!(
            runtime_region(None, None, arn, Some("us-west-1")),
            Some("us-west-2".into())
        );
        assert_eq!(
            runtime_region(None, None, "bad", Some("us-west-1")),
            Some("us-west-1".into())
        );
        assert_eq!(runtime_region(Some("bad/host"), None, arn, None), None);
    }

    #[test]
    fn process_response_requires_status_echo_and_object_payload() {
        let ok = r#"{"processPaymentId":"id","paymentManagerArn":"arn","paymentSessionId":"ps","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
        assert!(valid_process_response(ok, "arn", "ps", "pi"));
        assert!(!valid_process_response(
            &ok.replace("PROOF_GENERATED", "OTHER"),
            "arn",
            "ps",
            "pi"
        ));
        assert!(!valid_process_response(ok, "arn", "other", "pi"));
        assert!(!valid_process_response(
            &ok.replace(r#"{"signature":"secret"}"#, "[]"),
            "arn",
            "ps",
            "pi"
        ));
        assert!(!valid_process_response(
            &ok.replace(r#","createdAt":1"#, ""),
            "arn",
            "ps",
            "pi"
        ));
        let huge = "x".repeat(16_385);
        assert!(!valid_process_response(
            &ok.replace("secret", &huge),
            "arn",
            "ps",
            "pi"
        ));
    }
}

/// `AgentCore` call failure, containing no server-supplied text.
#[derive(Debug, PartialEq, Eq)]
#[cfg(test)]
pub enum PaymentError {
    /// A service error, reduced to a fixed code.
    Service(&'static str),
    /// The service returned an invalid response.
    InvalidResponse,
    /// The transport or authentication failed.
    Transport,
}

/// An adapter over a caller-supplied transport and signer.
#[cfg(test)]
pub struct PaymentClient<'a> {
    transport: &'a dyn toolkit_aws::StreamingTransport,
    signer: &'a dyn toolkit_aws::Signer,
    region: String,
    clock: &'a dyn PaymentClock,
}

#[cfg(test)]
trait PaymentClock {
    fn now(&self) -> std::time::Instant;
    fn sleep(&self, duration: std::time::Duration);
}

#[cfg(test)]
struct SystemPaymentClock;

#[cfg(test)]
impl PaymentClock for SystemPaymentClock {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
    fn sleep(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

/// A sensitive authorization returned by the service; it has not yet been
/// checked against the approved terms by the proof verifier.
#[derive(PartialEq)]
#[cfg(test)]
pub struct PaymentProof(Json);

#[cfg(test)]
impl std::fmt::Debug for PaymentProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PaymentProof([REDACTED])")
    }
}

#[cfg(test)]
impl PaymentProof {
    /// Inspect the sensitive document for verification. Never log this value.
    #[must_use]
    pub fn payload(&self) -> &Json {
        &self.0
    }
}

#[cfg(test)]
impl<'a> PaymentClient<'a> {
    /// Build an adapter using the conventional endpoint.
    ///
    /// # Errors
    /// Invalid region.
    pub fn new(
        region: &str,
        transport: &'a dyn toolkit_aws::StreamingTransport,
        signer: &'a dyn toolkit_aws::Signer,
    ) -> Result<Self, AwsError> {
        endpoint(region)?;
        Ok(Self {
            transport,
            signer,
            region: region.to_owned(),
            clock: &SystemPaymentClock,
        })
    }

    /// Send one approved payment.
    ///
    /// # Errors
    /// Refuses invalid responses and transport or service failures.
    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)] // Test prototype; consuming Approved is a security boundary.
    pub fn process_payment(&self, approved: Approved) -> Result<PaymentProof, PaymentError> {
        use std::time::Duration;
        use toolkit_aws::{
            FullJitter, Headers, HttpRequest, Method, RetryPolicy, SigningScope, StreamLimits,
            Timeouts,
        };
        let deadline = self.clock.now() + Duration::from_secs(60);
        let payment = ProcessPaymentRequest::from_approved(&approved);
        let mut headers = Headers::new();
        headers
            .insert_sensitive("x-amzn-bedrock-agentcore-payments-user-id", &payment.user)
            .map_err(|_| PaymentError::Transport)?;
        headers
            .insert(
                "x-amzn-bedrock-agentcore-payments-agent-name",
                "openclaw-aws-agents-pay",
            )
            .map_err(|_| PaymentError::Transport)?;
        headers
            .insert("content-type", "application/json")
            .map_err(|_| PaymentError::Transport)?;
        let policy = RetryPolicy::default();
        let jitter = FullJitter::new();
        for attempt in 1..=policy.total_attempts() {
            let remaining = deadline.saturating_duration_since(self.clock.now());
            if remaining.is_zero() {
                return Err(PaymentError::Transport);
            }
            let timeout = remaining.min(Duration::from_secs(30));
            let mut request = HttpRequest {
                method: Method::Post,
                url: format!(
                    "{}/payments/processPayment",
                    endpoint(&self.region).map_err(|_| PaymentError::Transport)?
                ),
                headers: headers.clone(),
                body: payment.body.clone(),
                timeouts: Timeouts::new(timeout, timeout).map_err(|_| PaymentError::Transport)?,
            };
            self.signer
                .sign(
                    &mut request,
                    &SigningScope {
                        region: self.region.clone(),
                        service: "bedrock-agentcore".into(),
                    },
                )
                .map_err(|_| PaymentError::Transport)?;
            if self.clock.now() >= deadline {
                return Err(PaymentError::Transport);
            }
            let limits = StreamLimits::new(65_536, 8_192)
                .map_err(|_| PaymentError::Transport)?
                .with_deadline(deadline);
            let outcome = self.transport.open(&request, &limits);
            let error = match outcome {
                Ok(mut stream) => 'response: {
                    let status = stream.status();
                    let error_header = stream.headers().get("x-amzn-ErrorType").map(str::to_owned);
                    if (300..400).contains(&status) {
                        return Err(PaymentError::InvalidResponse);
                    }
                    let length = match stream.headers().get("content-length") {
                        Some(value)
                            if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            match value.parse::<u64>() {
                                Ok(n) if n <= 65_536 => Some(n as usize),
                                Ok(_) | Err(_) if status >= 400 => {
                                    return Err(PaymentError::Service(error_code(
                                        error_header.as_deref(),
                                        "",
                                    )));
                                }
                                _ => return Err(PaymentError::InvalidResponse),
                            }
                        }
                        Some(_) => return Err(PaymentError::InvalidResponse),
                        None => None,
                    };
                    let mut bytes = Vec::new();
                    loop {
                        if self.clock.now() >= deadline {
                            return Err(PaymentError::Transport);
                        }
                        match stream.next_chunk() {
                            Ok(Some(chunk)) => {
                                let total = bytes.len().saturating_add(chunk.len());
                                if total > 65_536 || length.is_some_and(|len| total > len) {
                                    return Err(PaymentError::InvalidResponse);
                                }
                                bytes.extend_from_slice(&chunk);
                            }
                            Ok(None) => break,
                            Err(toolkit_aws::StreamError::IdleTimeout) => {
                                break 'response PaymentError::Transport;
                            }
                            Err(toolkit_aws::StreamError::Failed(error))
                                if error.is_retryable() =>
                            {
                                break 'response PaymentError::Transport;
                            }
                            Err(toolkit_aws::StreamError::TooLarge) => {
                                return Err(PaymentError::InvalidResponse);
                            }
                            Err(_) => return Err(PaymentError::Transport),
                        }
                    }
                    let body =
                        std::str::from_utf8(&bytes).map_err(|_| PaymentError::InvalidResponse)?;
                    if status == 200 {
                        if !valid_process_response(
                            body,
                            &payment.manager,
                            &payment.session,
                            &payment.instrument,
                        ) {
                            return Err(PaymentError::InvalidResponse);
                        }
                        let value = parse_json(body).map_err(|_| PaymentError::InvalidResponse)?;
                        let payload = value
                            .get("paymentOutput")
                            .and_then(|v| v.get("cryptoX402"))
                            .and_then(|v| v.get("payload"))
                            .ok_or(PaymentError::InvalidResponse)?;
                        let payload = match payload {
                            Json::String(text) => {
                                parse_json(text).map_err(|_| PaymentError::InvalidResponse)?
                            }
                            _ => payload.clone(),
                        };
                        return Ok(PaymentProof(payload));
                    }
                    PaymentError::Service(error_code(error_header.as_deref(), body))
                }
                Err(error) if !error.is_retryable() => return Err(PaymentError::Transport),
                Err(_) => PaymentError::Transport,
            };
            let retry = match &error {
                PaymentError::Service(code) => is_retryable(code),
                PaymentError::Transport => true,
                PaymentError::InvalidResponse => false,
            };
            if !retry || attempt == policy.total_attempts() {
                return Err(error);
            }
            let delay = policy.delay(attempt, &jitter);
            if delay >= deadline.saturating_duration_since(self.clock.now()) {
                return Err(error);
            }
            self.clock.sleep(delay);
        }
        Err(PaymentError::Transport)
    }
}

#[cfg(test)]
mod wire_contracts {
    use super::*;
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    use toolkit_aws::{
        testing::{FakeChunk, FakeStreamingTransport},
        NoSigner,
    };

    struct FakeClock(Cell<Instant>);
    impl PaymentClock for FakeClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }

    #[test]
    fn deadline_timeout_never_retries() {
        let transport = FakeStreamingTransport::new()
            .open_error(AwsError::timeout(
                toolkit_aws::error::TimeoutPhase::Deadline,
                "expired",
            ))
            .respond(200, &[], vec![]);
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        client.clock = &clock;
        assert_eq!(
            client.process_payment(super::operation_contracts::approved()),
            Err(PaymentError::Transport)
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn read_timeout_retries_identical_payment_bytes() {
        let response = br#"{"processPaymentId":"id","paymentManagerArn":"arn","paymentSessionId":"ps","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
        let transport = FakeStreamingTransport::new()
            .respond(200, &[], vec![FakeChunk::IdleTimeout])
            .respond(200, &[], vec![FakeChunk::data(response)]);
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        client.clock = &clock;
        assert!(client
            .process_payment(super::operation_contracts::approved())
            .is_ok());
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body, requests[1].body);
        assert_eq!(
            transport.open_limits()[0].deadline(),
            transport.open_limits()[1].deadline()
        );
    }

    #[test]
    fn oversized_error_uses_header_and_retries_without_reading_it() {
        let response = br#"{"processPaymentId":"id","paymentManagerArn":"arn","paymentSessionId":"ps","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
        let transport = FakeStreamingTransport::new()
            .respond(
                503,
                &[
                    ("content-length", "65537"),
                    ("x-amzn-ErrorType", "ThrottlingException"),
                ],
                vec![FakeChunk::data(b"must not read")],
            )
            .respond(200, &[], vec![FakeChunk::data(response)]);
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        client.clock = &clock;
        assert!(client
            .process_payment(super::operation_contracts::approved())
            .is_ok());
        assert_eq!(transport.requests().len(), 2);
        assert_eq!(transport.frames_pulled(), 1);
    }

    #[test]
    fn declared_oversize_is_refused_before_pulling() {
        let transport = FakeStreamingTransport::new().respond(
            200,
            &[("content-length", "65537")],
            vec![FakeChunk::data(b"x")],
        );
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.process_payment(super::operation_contracts::approved()),
            Err(PaymentError::InvalidResponse)
        );
        assert_eq!(transport.frames_pulled(), 0);
    }

    #[test]
    fn declared_short_length_is_not_trusted() {
        let response = br#"{"processPaymentId":"id","paymentManagerArn":"arn","paymentSessionId":"ps","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
        let transport = FakeStreamingTransport::new().respond(
            200,
            &[("content-length", "1")],
            vec![FakeChunk::data(response)],
        );
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.process_payment(super::operation_contracts::approved()),
            Err(PaymentError::InvalidResponse)
        );
    }

    #[test]
    fn t_aws_process_wire_and_response() {
        let response = br#"{"processPaymentId":"id","paymentManagerArn":"arn","paymentSessionId":"ps","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
        let transport =
            FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::data(response)]);
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        let result = client.process_payment(super::operation_contracts::approved());
        assert!(result.is_ok());
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert!(transport.open_limits()[0].deadline().is_some());
        assert_eq!(
            requests[0].url,
            "https://bedrock-agentcore.us-east-1.amazonaws.com/payments/processPayment"
        );
        assert_eq!(
            requests[0]
                .headers
                .get("x-amzn-bedrock-agentcore-payments-user-id"),
            Some("user")
        );
        assert_eq!(
            requests[0]
                .headers
                .get("x-amzn-bedrock-agentcore-payments-agent-name"),
            Some("openclaw-aws-agents-pay")
        );
        assert!(format!("{:?}", requests[0]).contains("[REDACTED]"));
    }

    #[test]
    fn conflict_does_not_mint_or_send_again_and_error_is_redacted() {
        let transport = FakeStreamingTransport::new().respond(
            409,
            &[("x-amzn-ErrorType", "ConflictException:secret")],
            vec![FakeChunk::data(br#"{"message":"secret"}"#)],
        );
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.process_payment(super::operation_contracts::approved()),
            Err(PaymentError::Service("ConflictException"))
        );
        assert_eq!(transport.requests().len(), 1);
    }
}
