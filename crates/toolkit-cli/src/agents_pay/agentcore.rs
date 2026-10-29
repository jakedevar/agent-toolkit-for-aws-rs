//! Safe `AgentCore` error classification and endpoint construction.

use crate::agents_pay::policy::Approved;
use toolkit_aws::{AwsError, Endpoint};
use toolkit_core::parse_json;
use toolkit_core::Json;

/// Pinned `AgentCore` payment service-model excerpt used for response contracts.
pub const PAYMENT_SERVICE_MODEL: &str = include_str!("model/agentcore-payments.json");

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

/// A fixed, value-free exception class for the payment flow.
#[must_use]
pub fn aws_error_class(error: &AwsError) -> &'static str {
    use toolkit_aws::error::TimeoutPhase;
    match error {
        AwsError::Connect { .. } => "ConnectError",
        AwsError::Protocol => "RemoteProtocolError",
        AwsError::Timeout {
            phase: TimeoutPhase::Connect,
            ..
        } => "ConnectTimeout",
        AwsError::Timeout {
            phase: TimeoutPhase::Read | TimeoutPhase::Deadline,
            ..
        } => "ReadTimeout",
        AwsError::NoCredentials => "NoCredentialsError",
        AwsError::Service(_) => "ClientError",
        AwsError::InvalidConfig { .. }
        | AwsError::CredentialArgument { .. }
        | AwsError::PartialCredentials { .. }
        | AwsError::CredentialsExpired
        | AwsError::CredentialProvider { .. }
        | AwsError::Pagination { .. }
        | AwsError::InvalidResponse { .. } => "RuntimeError",
    }
}

/// Build the standard `AgentCore` endpoint without reading endpoint overrides.
///
/// # Errors
/// Returns an invalid configuration error for an invalid region.
pub fn endpoint(region: &str) -> Result<String, AwsError> {
    Endpoint::for_service("bedrock-agentcore", region).map(|endpoint| endpoint.url())
}

// recorded-shape: approved merchant challenge is policy input, not an AWS response.
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
pub struct ProcessPaymentRequest {
    body: Vec<u8>,
    manager: String,
    session: String,
    instrument: String,
    user: String,
}

impl ProcessPaymentRequest {
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

fn get_body(manager: &str, session: &str) -> String {
    Json::object(vec![
        ("paymentManagerArn", Json::string(manager)),
        ("paymentSessionId", Json::string(session)),
    ])
    .to_compact_string()
}

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

fn agent_name(runtime: bool) -> &'static str {
    if runtime {
        "openclaw-aws-agents-pay"
    } else {
        "aws-agents-pay"
    }
}

/// Resolve the runtime region in documented precedence order.
#[must_use]
pub fn runtime_region(
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

fn has_required(shape_name: &str, value: &Json) -> bool {
    let Ok(model) = parse_json(PAYMENT_SERVICE_MODEL) else {
        return false;
    };
    let Some(shape) = model.get("shapes").and_then(|s| s.get(shape_name)) else {
        return false;
    };
    let Ok(required) = toolkit_aws::recorded::optional_string_list(shape, "required") else {
        return false;
    };
    !required.is_empty() && required.iter().all(|key| value.get(key).is_some())
}

// Validate the model closure for wallet details, including nested unions and
// the constraints on their strings. Unknown structure members are ignored,
// but a union must contain exactly one known arm.
// recorded-shape: model-driven validation rejects every mismatched required shape and union.
fn valid_wallet_shape(model: &Json, name: &str, value: &Json) -> bool {
    let Some(shape) = model.get("shapes").and_then(|shapes| shapes.get(name)) else {
        return false;
    };
    let bound = |key: &str| match shape.get(key) {
        Some(Json::Int(n)) => usize::try_from(*n).ok(),
        _ => None,
    };
    let within = |n: usize| {
        bound("min").is_none_or(|min| n >= min) && bound("max").is_none_or(|max| n <= max)
    };
    match (shape.get("type").and_then(Json::as_str), value) {
        (Some("string"), Json::String(text)) => {
            if !within(text.chars().count()) {
                return false;
            }
            if let Some(Json::Array(values)) = shape.get("enum") {
                if !values.iter().any(|value| value.as_str() == Some(text)) {
                    return false;
                }
            }
            shape
                .get("pattern")
                .and_then(Json::as_str)
                .is_none_or(|pattern| {
                    fancy_regex::Regex::new(&format!(r"\A(?:{pattern})\z"))
                        .is_ok_and(|regex| regex.is_match(text).unwrap_or(false))
                })
        }
        (Some("list"), Json::Array(values)) => {
            let Some(member) = shape
                .get("member")
                .and_then(|m| m.get("shape"))
                .and_then(Json::as_str)
            else {
                return false;
            };
            within(values.len())
                && values
                    .iter()
                    .all(|value| valid_wallet_shape(model, member, value))
        }
        (Some("structure"), Json::Object(entries)) => {
            let Some(Json::Object(members)) = shape.get("members") else {
                return false;
            };
            if matches!(shape.get("union"), Some(Json::Bool(true)))
                && (entries.len() != 1 || !members.iter().any(|(key, _)| key == &entries[0].0))
            {
                return false;
            }
            if let Some(Json::Array(required)) = shape.get("required") {
                if !required
                    .iter()
                    .all(|key| key.as_str().is_some_and(|key| value.get(key).is_some()))
                {
                    return false;
                }
            }
            members.iter().all(|(key, member)| {
                value.get(key).is_none_or(|nested| {
                    member
                        .get("shape")
                        .and_then(Json::as_str)
                        .is_some_and(|name| valid_wallet_shape(model, name, nested))
                })
            })
        }
        _ => false,
    }
}

// recorded-shape: payment payload is required as an object and all other shapes are rejected.
fn valid_process_response(body: &str, manager: &str, session: &str, instrument: &str) -> bool {
    if body.len() > 65_536 {
        return false;
    }
    let Ok(value) = parse_json(body) else {
        return false;
    };
    if !has_required("ProcessPaymentResponse", &value) {
        return false;
    }
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
        || !text("processPaymentId").is_some_and(|id| {
            parse_json(PAYMENT_SERVICE_MODEL).is_ok_and(|model| {
                valid_wallet_shape(&model, "ProcessPaymentId", &Json::string(id))
            })
        })
    {
        return false;
    }
    for key in ["createdAt", "updatedAt"] {
        if !matches!(
            toolkit_aws::recorded::optional_member(&value, key, "a timestamp", |member| {
                crate::cloudwatch::timefmt::time_from_json(member).ok()
            }),
            Ok(Some(_))
        ) {
            return false;
        }
    }
    let Ok(Some(output)) = toolkit_aws::recorded::optional_object(&value, "paymentOutput") else {
        return false;
    };
    let Ok(Some(crypto)) = toolkit_aws::recorded::optional_object(output, "cryptoX402") else {
        return false;
    };
    if !matches!(output, Json::Object(entries) if entries.len() == 1 && entries[0].0 == "cryptoX402")
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

fn valid_session_response(body: &str, manager: &str, session: Option<&str>, user: &str) -> bool {
    use toolkit_aws::recorded::{optional_member, optional_object, optional_text};
    if body.len() > 65_536 {
        return false;
    }
    let Ok(root) = parse_json(body) else {
        return false;
    };
    let Ok(Some(value)) = optional_object(&root, "paymentSession") else {
        return false;
    };
    if !has_required("PaymentSession", value) {
        return false;
    }
    let Ok(model) = parse_json(PAYMENT_SERVICE_MODEL) else {
        return false;
    };
    for (key, shape) in [
        ("paymentSessionId", "PaymentSessionId"),
        ("paymentManagerArn", "PaymentManagerArn"),
        ("userId", "UserId"),
    ] {
        if !value
            .get(key)
            .is_some_and(|v| valid_wallet_shape(&model, shape, v))
        {
            return false;
        }
    }
    let text = |key| optional_text(value, key).ok().flatten();
    if text("paymentManagerArn") != Some(manager)
        || text("userId") != Some(user)
        || text("paymentSessionId").is_none()
        || session.is_some_and(|id| text("paymentSessionId") != Some(id))
        || !matches!(
            optional_member(value, "expiryTimeInMinutes", "an integer", |v| match v {
                Json::Int(n) => i32::try_from(*n).ok(),
                _ => None,
            }),
            Ok(Some(_))
        )
    {
        return false;
    }
    let timestamp = |object: &Json, key: &str| {
        optional_member(object, key, "a timestamp", |v| {
            crate::cloudwatch::timefmt::time_from_json(v).ok()
        })
    };
    if !matches!(timestamp(value, "createdAt"), Ok(Some(_)))
        || !matches!(timestamp(value, "updatedAt"), Ok(Some(_)))
    {
        return false;
    }
    let amount = |object: &Json, key: &str, required: bool| match optional_object(object, key) {
        Ok(None) => !required,
        Ok(Some(v)) => {
            matches!(optional_text(v, "value"), Ok(Some(_)))
                && matches!(optional_text(v, "currency"), Ok(Some("USD")))
        }
        Err(_) => false,
    };
    match optional_object(value, "limits") {
        Ok(Some(limits)) if !amount(limits, "maxSpendAmount", true) => return false,
        Err(_) => return false,
        _ => {}
    }
    match optional_object(value, "availableLimits") {
        Ok(Some(limits))
            if !amount(limits, "availableSpendAmount", false)
                || timestamp(limits, "updatedAt").is_err() =>
        {
            return false
        }
        Err(_) => return false,
        _ => {}
    }
    true
}
#[derive(Debug, PartialEq, Eq)]
pub struct SessionStatus {
    /// Computed session state.
    pub status: &'static str,
    /// Whether the session can spend at the supplied time.
    pub usable: bool,
    /// Configured budget, if present.
    pub budget_usd: Option<String>,
    /// Difference between budget and available balance, if known.
    pub spent_usd: Option<String>,
}

/// Compute status using the service model's timestamps and available balance.
#[must_use]
pub fn session_status(value: &Json, now_micros: i64) -> SessionStatus {
    use crate::agents_pay::amount::CapDecimal;
    use toolkit_aws::recorded::{optional_member, optional_object, optional_text};
    let amount = |container: &str, member: &str| {
        optional_object(value, container)
            .ok()
            .flatten()
            .and_then(|v| optional_object(v, member).ok().flatten())
            .and_then(|v| optional_text(v, "value").ok().flatten())
    };
    let budget = amount("limits", "maxSpendAmount");
    let available = amount("availableLimits", "availableSpendAmount");
    let parsed_budget = budget.and_then(|v| CapDecimal::parse(v).ok());
    let parsed_available = available.and_then(|v| CapDecimal::parse(v).ok());
    let spent_usd = parsed_budget
        .as_ref()
        .zip(parsed_available.as_ref())
        .map(|(a, b)| a.subtract(b));
    let created = optional_member(value, "createdAt", "a timestamp", |v| {
        crate::cloudwatch::timefmt::time_from_json(v).ok()
    })
    .ok()
    .flatten();
    let duration = optional_member(value, "expiryTimeInMinutes", "an integer", |v| match v {
        Json::Int(n) if *n > 0 => n.checked_mul(60_000_000),
        _ => None,
    })
    .ok()
    .flatten();
    let expiry = created
        .zip(duration)
        .and_then(|(time, duration)| time.epoch_micros().checked_add(duration));
    let (status, usable) = match expiry {
        None => ("unknown", false),
        Some(end) if end <= now_micros => ("EXPIRED", false),
        Some(_)
            if parsed_available
                .as_ref()
                .is_some_and(CapDecimal::is_positive) =>
        {
            ("ACTIVE", true)
        }
        Some(_) => ("DRAINED", false),
    };
    SessionStatus {
        status,
        usable,
        budget_usd: budget.map(str::to_owned),
        spent_usd,
    }
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
    fn seeded_error_values_never_escape_allowlist() {
        for secret in [
            "FAKE-EMAIL@example.invalid",
            "FAKE-SESSION-ID",
            "FAKE-PROOF-SIGNATURE",
            "FAKE-ACCESS-KEY",
            "FAKE-CLIENT-TOKEN",
        ] {
            for header in [
                secret.to_owned(),
                format!("prefix#{secret}:suffix"),
                format!("ValidationException:{secret}"),
            ] {
                let code = error_code(Some(&header), "{}");
                assert!(!code.contains(secret));
                assert!(code == "UnrecognizedError" || code == "ValidationException");
            }
            for key in ["__type", "code", "Code", "message"] {
                let body = Json::object(vec![(key, Json::string(secret))]).to_compact_string();
                let code = error_code(None, &body);
                assert_eq!(code, "UnrecognizedError");
                assert!(!code.contains(secret));
            }
        }
    }

    #[test]
    fn aws_endpoint_is_fixed_to_the_service_and_region() {
        assert_eq!(
            endpoint("us-east-1").unwrap(),
            "https://bedrock-agentcore.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn aws_error_classes_are_fixed_and_never_print_input() {
        use toolkit_aws::error::TimeoutPhase;
        let secret = "FAKE-SEEDED-SECRET";
        for (error, class) in [
            (AwsError::connect(secret), "ConnectError"),
            (
                AwsError::timeout(TimeoutPhase::Connect, secret),
                "ConnectTimeout",
            ),
            (AwsError::timeout(TimeoutPhase::Read, secret), "ReadTimeout"),
            (
                AwsError::timeout(TimeoutPhase::Deadline, secret),
                "ReadTimeout",
            ),
            (AwsError::NoCredentials, "NoCredentialsError"),
            (AwsError::invalid_config("fixture", secret), "RuntimeError"),
        ] {
            assert_eq!(aws_error_class(&error), class);
            assert!(!aws_error_class(&error).contains(secret));
        }
    }

    #[test]
    fn session_status_uses_deadline_and_exact_available_balance() {
        let session = parse_json(r#"{"createdAt":1000,"expiryTimeInMinutes":15,"limits":{"maxSpendAmount":{"value":"10.00","currency":"USD"}},"availableLimits":{"availableSpendAmount":{"value":"3.25","currency":"USD"}}}"#).unwrap();
        let active = session_status(&session, 1_899_000_000);
        assert_eq!(
            active,
            SessionStatus {
                status: "ACTIVE",
                usable: true,
                budget_usd: Some("10.00".into()),
                spent_usd: Some("6.75".into())
            }
        );
        assert_eq!(session_status(&session, 1_900_000_000).status, "EXPIRED");
        let drained = parse_json(&session.to_compact_string().replace("3.25", "0.00")).unwrap();
        assert_eq!(session_status(&drained, 1_899_000_000).status, "DRAINED");
        let missing = parse_json(
            &session
                .to_compact_string()
                .replace(r#""createdAt":1000,"#, ""),
        )
        .unwrap();
        assert!(!session_status(&missing, 1_000_000_000).usable);
    }

    #[test]
    fn session_status_handles_expiry_and_balance_boundaries() {
        let make = |balance: &str| {
            parse_json(&format!(r#"{{"createdAt":1000,"expiryTimeInMinutes":15,"limits":{{"maxSpendAmount":{{"value":"10.00"}}}},"availableLimits":{{"availableSpendAmount":{{"value":"{balance}"}}}}}}"#)).unwrap()
        };
        let session = make("0.01");
        assert_eq!(session_status(&session, 1_899_999_999).status, "ACTIVE");
        assert_eq!(session_status(&session, 1_900_000_000).status, "EXPIRED");
        for balance in ["0", "-0.00", "-1", "not-a-number"] {
            let status = session_status(&make(balance), 1_000_000_000);
            assert_eq!(status.status, "DRAINED");
            assert!(!status.usable);
        }
        let missing = parse_json(r#"{"expiryTimeInMinutes":15}"#).unwrap();
        assert_eq!(session_status(&missing, 0).status, "unknown");
        assert!(!session_status(&missing, 0).usable);
        let overflow =
            parse_json(r#"{"createdAt":1,"expiryTimeInMinutes":9223372036854775807}"#).unwrap();
        assert_eq!(session_status(&overflow, 0).status, "unknown");
    }
}

#[cfg(test)]
mod operation_contracts {
    use super::*;
    use crate::agents_pay::policy::{authorize, FixedResolver, PaymentRequest, Resources};
    use toolkit_core::parse_json;

    pub(super) fn approved() -> crate::agents_pay::policy::Approved {
        approved_for(
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
        )
    }

    pub(super) fn approved_for(manager: &str) -> crate::agents_pay::policy::Approved {
        let policy = parse_json(r#"{"max_per_payment_usd":"0.10","allowed_networks":["eip155:84532"],"allowed_assets":{"eip155:84532":["0xa"]},"allowed_recipients":["0xb"]}"#).unwrap();
        let challenge = parse_json(r#"{"x402Version":2,"accepts":[{"scheme":"exact","network":"eip155:84532","asset":"0xA","payTo":"0xB","amount":"100000"}]}"#).unwrap();
        let resources = Resources {
            payment_manager_arn: Some(manager.into()),
            payment_instrument_id: Some("pi".into()),
            payment_session_id: Some("payment-session-TEST12345678901".into()),
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
                r#"{{"paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","paymentInput":{{"cryptoX402":{{"version":"2","payload":{{"scheme":"exact","network":"eip155:84532","asset":"0xA","payTo":"0xB","amount":"100000"}}}}}},"clientToken":"{token}"}}"#
            )
        );
    }

    #[test]
    fn t_aws_other_request_bodies_and_agent_names() {
        assert_eq!(
            get_body(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "payment-session-TEST12345678901"
            ),
            r#"{"paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901"}"#
        );
        assert_eq!(
            session_body(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "10",
                15,
                "token"
            ),
            r#"{"paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","limits":{"maxSpendAmount":{"value":"10","currency":"USD"}},"expiryTimeInMinutes":15,"clientToken":"token"}"#
        );
        assert_eq!(
            instrument_body(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "connector",
                "ETHEREUM",
                "user@example.com",
                "token"
            ),
            r#"{"paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentConnectorId":"connector","paymentInstrumentType":"EMBEDDED_CRYPTO_WALLET","paymentInstrumentDetails":{"embeddedCryptoWallet":{"network":"ETHEREUM","linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]}},"clientToken":"token"}"#
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
        let ok = r#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
        assert!(valid_process_response(
            ok,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
        assert!(!valid_process_response(
            &ok.replace("PROOF_GENERATED", "OTHER"),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
        assert!(!valid_process_response(
            ok,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "other",
            "pi"
        ));
        assert!(!valid_process_response(
            &ok.replace(r#"{"signature":"secret"}"#, "[]"),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
        assert!(!valid_process_response(
            &ok.replace(r#","createdAt":1"#, ""),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
        let huge = "x".repeat(16_385);
        assert!(!valid_process_response(
            &ok.replace("secret", &huge),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
    }

    #[test]
    fn process_response_accepts_iso8601_model_timestamps() {
        let body = r#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":"2026-10-28T12:00:00Z","updatedAt":"2026-10-28T12:00:00Z"}"#;
        assert!(valid_process_response(
            body,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
        assert!(!valid_process_response(
            &body.replace("2026-10-28T12:00:00Z", "not-a-timestamp"),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
    }

    #[test]
    fn process_response_rejects_multiple_or_unknown_union_arms() {
        let body = r#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{}},"future":{}},"createdAt":1,"updatedAt":1}"#;
        assert!(!valid_process_response(
            body,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
    }

    #[test]
    fn process_response_rejects_malformed_process_identifier() {
        let good = r#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{}}},"createdAt":1,"updatedAt":1}"#;
        assert!(valid_process_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
        assert!(!valid_process_response(
            &good.replace("12345678-abcd-1234-abcd-123456789abc", "id"),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            "payment-session-TEST12345678901",
            "pi"
        ));
    }

    #[test]
    fn session_response_rejects_integer_outside_model_range() {
        let good = r#"{"paymentSession":{"paymentSessionId":"payment-session-TEST12345678901","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","userId":"user","expiryTimeInMinutes":60,"createdAt":1,"updatedAt":1}}"#;
        assert!(valid_session_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("payment-session-TEST12345678901"),
            "user"
        ));
        for n in ["2147483648", "-2147483649"] {
            assert!(!valid_session_response(
                &good.replace(":60", &format!(":{n}")),
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                Some("payment-session-TEST12345678901"),
                "user"
            ));
        }
    }

    #[test]
    fn session_response_rejects_non_model_resource_identifiers_even_when_echoed() {
        let manager =
            "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/test-abcdefghij";
        let session = "payment-session-TEST12345678901";
        let good = format!(
            r#"{{"paymentSession":{{"paymentSessionId":"{session}","paymentManagerArn":"{manager}","userId":"user","expiryTimeInMinutes":60,"createdAt":1,"updatedAt":1}}}}"#
        );
        assert!(valid_session_response(
            &good,
            manager,
            Some(session),
            "user"
        ));
        assert!(!valid_session_response(
            &good.replace(session, "ps"),
            manager,
            Some("ps"),
            "user"
        ));
        assert!(!valid_session_response(
            &good.replace(manager, "arn"),
            "arn",
            Some(session),
            "user"
        ));
    }

    #[test]
    fn session_response_requires_model_members_and_matching_resources() {
        let good = r#"{"paymentSession":{"paymentSessionId":"payment-session-TEST12345678901","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","userId":"user","expiryTimeInMinutes":60,"createdAt":"2026-10-28T12:00:00Z","updatedAt":"2026-10-28T12:00:00Z","limits":{"maxSpendAmount":{"value":"10","currency":"USD"}},"availableLimits":{"availableSpendAmount":{"value":"9","currency":"USD"},"updatedAt":"2026-10-28T12:00:00Z"}}}"#;
        assert!(valid_session_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("payment-session-TEST12345678901"),
            "user"
        ));
        assert!(valid_session_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            None,
            "user"
        ));
        assert!(!valid_session_response(
            good,
            "wrong",
            Some("payment-session-TEST12345678901"),
            "user"
        ));
        assert!(!valid_session_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("other"),
            "user"
        ));
        assert!(!valid_session_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("payment-session-TEST12345678901"),
            "other"
        ));
        for fragment in [
            r#""paymentSessionId":"payment-session-TEST12345678901","#,
            r#""userId":"user","#,
            r#""updatedAt":"2026-10-28T12:00:00Z","#,
        ] {
            assert!(!valid_session_response(
                &good.replacen(fragment, "", 1),
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                Some("payment-session-TEST12345678901"),
                "user"
            ));
        }
        assert!(!valid_session_response(
            &good.replace(
                r#""expiryTimeInMinutes":60"#,
                r#""expiryTimeInMinutes":null"#
            ),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("payment-session-TEST12345678901"),
            "user"
        ));
        assert!(!valid_session_response(
            &good.replace(
                r#""limits":{"maxSpendAmount":{"value":"10","currency":"USD"}}"#,
                r#""limits":null"#
            ),
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("payment-session-TEST12345678901"),
            "user"
        ));
    }

    #[test]
    fn session_response_rejects_unknown_currency_in_either_amount() {
        let good = r#"{"paymentSession":{"paymentSessionId":"payment-session-TEST12345678901","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","userId":"user","expiryTimeInMinutes":60,"createdAt":1,"updatedAt":1,"limits":{"maxSpendAmount":{"value":"10","currency":"USD"}},"availableLimits":{"availableSpendAmount":{"value":"9","currency":"USD"}}}}"#;
        assert!(valid_session_response(
            good,
            "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
            Some("payment-session-TEST12345678901"),
            "user"
        ));
        for bad in [
            good.replacen("\"currency\":\"USD\"", "\"currency\":\"EUR\"", 1),
            good.replace(
                "\"value\":\"9\",\"currency\":\"USD\"",
                "\"value\":\"9\",\"currency\":\"EUR\"",
            ),
        ] {
            assert!(!valid_session_response(
                &bad,
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                Some("payment-session-TEST12345678901"),
                "user"
            ));
        }
    }
}

/// `AgentCore` call failure, containing no server-supplied text.
#[derive(Debug, PartialEq, Eq)]
pub enum PaymentError {
    /// A service error, reduced to a fixed code.
    Service(&'static str),
    /// The service returned an invalid response.
    InvalidResponse,
    /// The transport or authentication failed.
    Transport(PaymentErrorClass),
}

impl PaymentError {
    /// Fixed, value-free class used by the generic payment-flow error text.
    #[must_use]
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Service(_) => "ClientError",
            Self::InvalidResponse => "RuntimeError",
            Self::Transport(class) => match class {
                PaymentErrorClass::ConnectError => "ConnectError",
                PaymentErrorClass::ConnectTimeout => "ConnectTimeout",
                PaymentErrorClass::ReadTimeout => "ReadTimeout",
                PaymentErrorClass::RemoteProtocolError => "RemoteProtocolError",
                PaymentErrorClass::NoCredentialsError => "NoCredentialsError",
                PaymentErrorClass::ClientError => "ClientError",
                PaymentErrorClass::RuntimeError => "RuntimeError",
            },
        }
    }
}

/// Value-free classes used by the generic payment-flow error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentErrorClass {
    /// Connection or TLS failure.
    ConnectError,
    /// Connection timed out.
    ConnectTimeout,
    /// Idle read or operation deadline elapsed.
    ReadTimeout,
    /// Malformed HTTP response.
    RemoteProtocolError,
    /// No credentials were found.
    NoCredentialsError,
    /// AWS service error.
    ClientError,
    /// Other internal failure.
    RuntimeError,
}

fn transport_error(error: &AwsError) -> PaymentError {
    use toolkit_aws::error::TimeoutPhase;
    let class = match error {
        AwsError::Connect { .. } => PaymentErrorClass::ConnectError,
        AwsError::Protocol => PaymentErrorClass::RemoteProtocolError,
        AwsError::Timeout {
            phase: TimeoutPhase::Connect,
            ..
        } => PaymentErrorClass::ConnectTimeout,
        AwsError::Timeout {
            phase: TimeoutPhase::Read | TimeoutPhase::Deadline,
            ..
        } => PaymentErrorClass::ReadTimeout,
        AwsError::NoCredentials => PaymentErrorClass::NoCredentialsError,
        AwsError::Service(_) => PaymentErrorClass::ClientError,
        AwsError::InvalidConfig { .. }
        | AwsError::CredentialArgument { .. }
        | AwsError::PartialCredentials { .. }
        | AwsError::CredentialsExpired
        | AwsError::CredentialProvider { .. }
        | AwsError::Pagination { .. }
        | AwsError::InvalidResponse { .. } => PaymentErrorClass::RuntimeError,
    };
    PaymentError::Transport(class)
}

/// An adapter over a caller-supplied transport and signer.
pub struct PaymentClient<'a> {
    transport: &'a dyn toolkit_aws::StreamingTransport,
    signer: &'a dyn toolkit_aws::Signer,
    region: String,
    clock: &'a dyn PaymentClock,
    jitter: Option<&'a dyn toolkit_aws::Jitter>,
    operation_deadline: Option<std::time::Instant>,
}

trait PaymentClock {
    fn now(&self) -> std::time::Instant;
    fn sleep(&self, duration: std::time::Duration);
}

struct SystemPaymentClock;

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
pub struct PaymentProof(Json);

impl std::fmt::Debug for PaymentProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PaymentProof([REDACTED])")
    }
}

impl PaymentProof {
    /// Inspect the sensitive document for verification. Never log this value.
    #[must_use]
    pub fn payload(&self) -> &Json {
        &self.0
    }
}

/// A service proof bound to its approved terms; it still needs verification.
pub struct Authorized {
    proof: PaymentProof,
    amount: String,
    version: i64,
    recipient: String,
}
impl Authorized {
    /// Consume this authorization for proof verification.
    #[must_use]
    pub fn into_parts(self) -> (PaymentProof, String, i64, String) {
        (self.proof, self.amount, self.version, self.recipient)
    }
}
/// An in-memory payment fake limited to the fixture account.
#[derive(Default)]
pub struct FakePaymentsApi {
    calls: std::cell::Cell<usize>,
    proof: std::cell::RefCell<Option<Json>>,
    idempotency: std::cell::RefCell<std::collections::HashMap<String, (Vec<u8>, Json)>>,
    authorizations: std::cell::Cell<usize>,
    session: Option<Json>,
    instrument: Option<Json>,
    session_reads: std::cell::Cell<usize>,
    session_creates: std::cell::Cell<usize>,
    instrument_creates: std::cell::Cell<usize>,
}
impl FakePaymentsApi {
    /// Return a supplied proof document for fixture-account requests.
    #[must_use]
    pub fn with_proof(proof: Json) -> Self {
        Self {
            proof: std::cell::RefCell::new(Some(proof)),
            ..Self::default()
        }
    }
    /// Supply a deterministic session response for read and create calls.
    #[must_use]
    pub fn with_session(session: Json) -> Self {
        Self {
            session: Some(session),
            ..Self::default()
        }
    }
    /// Supply a deterministic instrument response.
    #[must_use]
    pub fn with_instrument(mut self, instrument: Json) -> Self {
        self.instrument = Some(instrument);
        self
    }
    /// Number of session reads observed by this fake.
    #[must_use]
    pub fn session_calls(&self) -> usize {
        self.session_reads.get()
    }
    /// Number of session creations observed by this fake.
    #[must_use]
    pub fn session_create_calls(&self) -> usize {
        self.session_creates.get()
    }
    /// Number of instrument creations observed by this fake.
    #[must_use]
    pub fn instrument_create_calls(&self) -> usize {
        self.instrument_creates.get()
    }
    /// Number of logical calls observed by this fake.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.get()
    }
    /// Set the proof produced by the next new authorization.
    pub fn set_proof(&self, proof: Json) {
        *self.proof.borrow_mut() = Some(proof);
    }
    /// Count new authorizations, excluding idempotent replays.
    #[must_use]
    pub fn authorizations(&self) -> usize {
        self.authorizations.get()
    }
    /// Simulate expiry of the service's idempotency retention window.
    pub fn expire_idempotency_records(&self) {
        self.idempotency.borrow_mut().clear();
    }
}
/// Payment operation interface; requests can only originate from `Approved`.
pub trait PaymentsApi {
    /// Read a session. Returns a sanitized error on failure.
    fn get_session(&self, manager: &str, session: &str, user: &str) -> Result<Json, PaymentError>;
    /// Create a session. Returns a sanitized error on failure.
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError>;
    /// Create an instrument. Returns a sanitized error on failure.
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError>;
    /// Execute a single logical payment request.
    ///
    /// # Errors
    /// Returns a sanitized transport, service, or response failure.
    fn process_payment(
        &self,
        request: &ProcessPaymentRequest,
    ) -> Result<PaymentProof, PaymentError>;
}
fn assert_fixture_account(manager: &str) {
    assert!(
        manager.starts_with("arn:") && manager.split(':').nth(4) == Some("111111111111"),
        "fake payments require fixture account"
    );
}
impl PaymentsApi for FakePaymentsApi {
    fn get_session(
        &self,
        manager: &str,
        _session: &str,
        _user: &str,
    ) -> Result<Json, PaymentError> {
        assert_fixture_account(manager);
        self.session_reads.set(self.session_reads.get() + 1);
        self.session.clone().ok_or(PaymentError::InvalidResponse)
    }
    fn create_session(
        &self,
        manager: &str,
        _user: &str,
        _budget: &str,
        _expiry: i64,
        _token: &str,
    ) -> Result<Json, PaymentError> {
        assert_fixture_account(manager);
        self.session_creates.set(self.session_creates.get() + 1);
        self.session.clone().ok_or(PaymentError::InvalidResponse)
    }
    fn create_instrument(
        &self,
        manager: &str,
        _user: &str,
        _connector: &str,
        _network: &str,
        _email: &str,
        _token: &str,
    ) -> Result<Json, PaymentError> {
        assert_fixture_account(manager);
        self.instrument_creates
            .set(self.instrument_creates.get() + 1);
        self.instrument.clone().ok_or(PaymentError::InvalidResponse)
    }
    fn process_payment(
        &self,
        request: &ProcessPaymentRequest,
    ) -> Result<PaymentProof, PaymentError> {
        assert_fixture_account(&request.manager);
        self.calls.set(self.calls.get() + 1);
        let body = std::str::from_utf8(&request.body).map_err(|_| PaymentError::InvalidResponse)?;
        let parsed = parse_json(body).map_err(|_| PaymentError::InvalidResponse)?;
        let token = parsed
            .get("clientToken")
            .and_then(Json::as_str)
            .ok_or(PaymentError::InvalidResponse)?;
        let mut records = self.idempotency.borrow_mut();
        if let Some((previous_body, proof)) = records.get(token) {
            if previous_body != &request.body {
                return Err(PaymentError::Service("ConflictException"));
            }
            return Ok(PaymentProof(proof.clone()));
        }
        let proof = self
            .proof
            .borrow()
            .clone()
            .unwrap_or_else(|| Json::object(vec![]));
        records.insert(token.to_owned(), (request.body.clone(), proof.clone()));
        self.authorizations.set(self.authorizations.get() + 1);
        Ok(PaymentProof(proof))
    }
}
#[allow(clippy::needless_pass_by_value)] // Consuming Approved permits only one logical spend.
/// Consume an approval once and bind the returned proof to its approved terms.
///
/// ```compile_fail,E0382
/// use toolkit_cli::agents_pay::{agentcore::{process_payment, PaymentsApi}, policy::Approved};
/// fn twice(api: &dyn PaymentsApi, approved: Approved) {
///     let _ = process_payment(api, approved);
///     let _ = process_payment(api, approved);
/// }
/// ```
///
/// # Errors
/// Returns a sanitized failure if the request or response is invalid.
pub fn process_payment(
    api: &dyn PaymentsApi,
    approved: Approved,
) -> Result<Authorized, PaymentError> {
    let recipient = toolkit_aws::recorded::optional_text(approved.entry(), "payTo")
        .map_err(|_| PaymentError::InvalidResponse)?
        .ok_or(PaymentError::InvalidResponse)?
        .to_owned();
    let amount = approved.amount().to_owned();
    let version = approved.version();
    let request = ProcessPaymentRequest::from_approved(&approved);
    let proof = api.process_payment(&request)?;
    Ok(Authorized {
        proof,
        amount,
        version,
        recipient,
    })
}

impl<'a> PaymentClient<'a> {
    /// Check the configured identity with the standard regional STS endpoint.
    ///
    /// # Errors
    /// Returns a sanitized failure for an unsuccessful or malformed response.
    pub fn sts_check(&self) -> Result<(), PaymentError> {
        const STS_NS: &str = "https://sts.amazonaws.com/doc/2011-06-15/";
        let mut headers = toolkit_aws::Headers::new();
        headers
            .insert("content-type", "application/x-www-form-urlencoded")
            .map_err(|error| transport_error(&error))?;
        let url = toolkit_aws::Endpoint::for_service("sts", &self.region)
            .map_err(|error| transport_error(&error))?
            .url();
        let body = self.send(
            &format!("{url}/"),
            "sts",
            headers,
            b"Action=GetCallerIdentity&Version=2011-06-15".to_vec(),
            200,
            self.operation_deadline,
        )?;
        let doc = roxmltree::Document::parse(&body).map_err(|_| PaymentError::InvalidResponse)?;
        let root = doc.root_element();
        if root.tag_name().name() != "GetCallerIdentityResponse"
            || root.tag_name().namespace() != Some(STS_NS)
        {
            return Err(PaymentError::InvalidResponse);
        }
        let mut results = root.children().filter(|node| {
            node.is_element()
                && node.tag_name().name() == "GetCallerIdentityResult"
                && node.tag_name().namespace() == Some(STS_NS)
        });
        if results.next().is_none() || results.next().is_some() {
            return Err(PaymentError::InvalidResponse);
        }
        Ok(())
    }
    #[cfg(test)]
    fn process_payment_until(
        &self,
        approved: Approved,
        deadline: std::time::Instant,
    ) -> Result<PaymentProof, PaymentError> {
        self.process_payment_inner(approved, Some(deadline))
    }
    /// Read a payment session.
    ///
    /// # Errors
    /// Returns a sanitized failure on transport or invalid response.
    pub fn get_session(
        &self,
        manager: &str,
        session: &str,
        user: &str,
    ) -> Result<Json, PaymentError> {
        let body = self.call(
            "getPaymentSession",
            user,
            true,
            get_body(manager, session).into_bytes(),
            200,
            self.operation_deadline,
        )?;
        if !valid_session_response(&body, manager, Some(session), user) {
            return Err(PaymentError::InvalidResponse);
        }
        parse_json(&body).map_err(|_| PaymentError::InvalidResponse)
    }
    /// Create a payment session using the supplied idempotency token.
    ///
    /// # Errors
    /// Returns a sanitized failure on invalid input, transport, or response.
    pub fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError> {
        if !(15..=480).contains(&expiry) {
            return Err(PaymentError::InvalidResponse);
        }
        let body = self.call(
            "createPaymentSession",
            user,
            false,
            session_body(manager, budget, expiry, token).into_bytes(),
            201,
            self.operation_deadline,
        )?;
        if !valid_session_response(&body, manager, None, user) {
            return Err(PaymentError::InvalidResponse);
        }
        parse_json(&body).map_err(|_| PaymentError::InvalidResponse)
    }
    /// Create a wallet instrument using the supplied idempotency token.
    ///
    /// # Errors
    /// Returns a sanitized failure on invalid input, transport, or response.
    pub fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError> {
        if !matches!(network, "ETHEREUM" | "SOLANA") {
            return Err(PaymentError::InvalidResponse);
        }
        let body = self.call(
            "createPaymentInstrument",
            user,
            false,
            instrument_body(manager, connector, network, email, token).into_bytes(),
            201,
            self.operation_deadline,
        )?;
        let value = parse_json(&body).map_err(|_| PaymentError::InvalidResponse)?;
        let instrument = toolkit_aws::recorded::optional_object(&value, "paymentInstrument")
            .map_err(|_| PaymentError::InvalidResponse)?
            .ok_or(PaymentError::InvalidResponse)?;
        let text = |key| {
            toolkit_aws::recorded::optional_text(instrument, key)
                .ok()
                .flatten()
        };
        if !has_required("PaymentInstrument", instrument)
            || text("paymentInstrumentId").is_none()
            || !matches!(
                text("status"),
                Some("INITIATED" | "ACTIVE" | "FAILED" | "DELETED" | "BLOCKED")
            )
            || text("paymentManagerArn") != Some(manager)
            || text("paymentConnectorId") != Some(connector)
            || text("userId") != Some(user)
            || text("paymentInstrumentType") != Some("EMBEDDED_CRYPTO_WALLET")
        {
            return Err(PaymentError::InvalidResponse);
        }
        for key in ["createdAt", "updatedAt"] {
            if !matches!(
                toolkit_aws::recorded::optional_member(instrument, key, "a timestamp", |v| {
                    crate::cloudwatch::timefmt::time_from_json(v).ok()
                }),
                Ok(Some(_))
            ) {
                return Err(PaymentError::InvalidResponse);
            }
        }
        let details =
            toolkit_aws::recorded::optional_object(instrument, "paymentInstrumentDetails")
                .map_err(|_| PaymentError::InvalidResponse)?
                .ok_or(PaymentError::InvalidResponse)?;
        let wallet = toolkit_aws::recorded::optional_object(details, "embeddedCryptoWallet")
            .map_err(|_| PaymentError::InvalidResponse)?
            .ok_or(PaymentError::InvalidResponse)?;
        let model = parse_json(PAYMENT_SERVICE_MODEL).map_err(|_| PaymentError::InvalidResponse)?;
        if !valid_wallet_shape(&model, "PaymentInstrumentDetails", details)
            || toolkit_aws::recorded::optional_text(wallet, "network")
                .ok()
                .flatten()
                != Some(network)
        {
            return Err(PaymentError::InvalidResponse);
        }
        Ok(value)
    }
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
            jitter: None,
            operation_deadline: None,
        })
    }

    /// Bound the logical payment call by the enclosing operation deadline.
    #[must_use]
    pub fn with_operation_deadline(mut self, deadline: std::time::Instant) -> Self {
        self.operation_deadline = Some(deadline);
        self
    }

    /// Send one approved payment.
    ///
    /// # Errors
    /// Refuses invalid responses and transport or service failures.
    #[allow(clippy::needless_pass_by_value)] // Consuming Approved is a security boundary.
    #[cfg(test)]
    fn process_payment(&self, approved: Approved) -> Result<PaymentProof, PaymentError> {
        self.process_payment_inner(approved, None)
    }

    #[cfg(test)]
    #[allow(clippy::needless_pass_by_value)]
    fn process_payment_inner(
        &self,
        approved: Approved,
        deadline: Option<std::time::Instant>,
    ) -> Result<PaymentProof, PaymentError> {
        let payment = ProcessPaymentRequest::from_approved(&approved);
        self.process_request(&payment, deadline)
    }

    fn process_request(
        &self,
        payment: &ProcessPaymentRequest,
        deadline: Option<std::time::Instant>,
    ) -> Result<PaymentProof, PaymentError> {
        let body = self.call(
            "processPayment",
            &payment.user,
            true,
            payment.body.clone(),
            200,
            deadline,
        )?;
        if !valid_process_response(
            &body,
            &payment.manager,
            &payment.session,
            &payment.instrument,
        ) {
            return Err(PaymentError::InvalidResponse);
        }
        let value = parse_json(&body).map_err(|_| PaymentError::InvalidResponse)?;
        let payload = value
            .get("paymentOutput")
            .and_then(|v| v.get("cryptoX402"))
            .and_then(|v| v.get("payload"))
            .ok_or(PaymentError::InvalidResponse)?;
        let payload = match payload {
            Json::String(text) => parse_json(text).map_err(|_| PaymentError::InvalidResponse)?,
            _ => payload.clone(),
        };
        Ok(PaymentProof(payload))
    }

    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        clippy::needless_pass_by_value
    )]
    fn call(
        &self,
        path: &str,
        user: &str,
        runtime: bool,
        request_body: Vec<u8>,
        expected: u16,
        operation_deadline: Option<std::time::Instant>,
    ) -> Result<String, PaymentError> {
        let mut headers = toolkit_aws::Headers::new();
        headers
            .insert_sensitive("x-amzn-bedrock-agentcore-payments-user-id", user)
            .map_err(|error| transport_error(&error))?;
        headers
            .insert(
                "x-amzn-bedrock-agentcore-payments-agent-name",
                agent_name(runtime),
            )
            .map_err(|error| transport_error(&error))?;
        headers
            .insert("content-type", "application/json")
            .map_err(|error| transport_error(&error))?;
        let url = format!(
            "{}/payments/{path}",
            endpoint(&self.region).map_err(|error| transport_error(&error))?
        );
        self.send(
            &url,
            "bedrock-agentcore",
            headers,
            request_body,
            expected,
            operation_deadline,
        )
    }

    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        clippy::needless_pass_by_value
    )]
    fn send(
        &self,
        url: &str,
        service: &str,
        headers: toolkit_aws::Headers,
        request_body: Vec<u8>,
        expected: u16,
        operation_deadline: Option<std::time::Instant>,
    ) -> Result<String, PaymentError> {
        use std::time::Duration;
        use toolkit_aws::{
            FullJitter, HttpRequest, Method, RetryPolicy, SigningScope, StreamLimits, Timeouts,
        };
        let limit = self.clock.now() + Duration::from_secs(60);
        let deadline = operation_deadline.map_or(limit, |cutoff| cutoff.min(limit));
        let policy = RetryPolicy::default();
        let jitter = FullJitter::new();
        for attempt in 1..=policy.total_attempts() {
            let remaining = deadline.saturating_duration_since(self.clock.now());
            if remaining.is_zero() {
                return Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout));
            }
            let timeout = remaining.min(Duration::from_secs(30));
            let mut request = HttpRequest {
                method: Method::Post,
                url: url.to_owned(),
                headers: headers.clone(),
                body: request_body.clone(),
                timeouts: Timeouts::new(timeout, timeout)
                    .map_err(|error| transport_error(&error))?,
            };
            self.signer
                .sign(
                    &mut request,
                    &SigningScope {
                        region: self.region.clone(),
                        service: service.into(),
                    },
                )
                .map_err(|error| transport_error(&error))?;
            if self.clock.now() >= deadline {
                return Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout));
            }
            let limits = StreamLimits::new(65_536, 8_192)
                .map_err(|error| transport_error(&error))?
                .with_deadline(deadline);
            let outcome = self.transport.open(&request, &limits);
            let error = match outcome {
                Ok(mut stream) => 'response: {
                    let status = stream.status();
                    let error_header = stream.headers().get("x-amzn-ErrorType").map(str::to_owned);
                    if status < 400 && status != expected {
                        return Err(PaymentError::InvalidResponse);
                    }
                    let length = match stream.headers().get("content-length") {
                        Some(value)
                            if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            match value.parse::<u64>() {
                                Ok(n) if n <= 65_536 => Some(
                                    usize::try_from(n)
                                        .map_err(|_| PaymentError::InvalidResponse)?,
                                ),
                                Ok(_) | Err(_) if status >= 400 => {
                                    break 'response PaymentError::Service(error_code(
                                        error_header.as_deref(),
                                        "",
                                    ));
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
                            return Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout));
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
                                break 'response PaymentError::Transport(
                                    PaymentErrorClass::ReadTimeout,
                                );
                            }
                            Err(toolkit_aws::StreamError::Failed(error))
                                if error.is_retryable() =>
                            {
                                break 'response transport_error(&error);
                            }
                            Err(toolkit_aws::StreamError::Failed(error)) => {
                                return Err(transport_error(&error));
                            }
                            Err(toolkit_aws::StreamError::TooLarge) if status >= 400 => {
                                break 'response PaymentError::Service(error_code(
                                    error_header.as_deref(),
                                    "",
                                ));
                            }
                            Err(toolkit_aws::StreamError::TooLarge) => {
                                return Err(PaymentError::InvalidResponse);
                            }
                            Err(toolkit_aws::StreamError::DeadlineExceeded) => {
                                return Err(PaymentError::Transport(
                                    PaymentErrorClass::ReadTimeout,
                                ));
                            }
                        }
                    }
                    let body =
                        std::str::from_utf8(&bytes).map_err(|_| PaymentError::InvalidResponse)?;
                    if status == expected {
                        return Ok(body.to_owned());
                    }
                    PaymentError::Service(error_code(error_header.as_deref(), body))
                }
                Err(error) if !error.is_retryable() => return Err(transport_error(&error)),
                Err(error) => transport_error(&error),
            };
            let retry = match &error {
                PaymentError::Service(code) => is_retryable(code),
                PaymentError::Transport(_) => true,
                PaymentError::InvalidResponse => false,
            };
            if !retry || attempt == policy.total_attempts() {
                return Err(error);
            }
            let delay = policy.delay(attempt, self.jitter.unwrap_or(&jitter));
            if delay >= deadline.saturating_duration_since(self.clock.now()) {
                return Err(error);
            }
            self.clock.sleep(delay);
        }
        Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout))
    }
}

impl PaymentsApi for PaymentClient<'_> {
    fn get_session(&self, manager: &str, session: &str, user: &str) -> Result<Json, PaymentError> {
        PaymentClient::get_session(self, manager, session, user)
    }
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError> {
        PaymentClient::create_session(self, manager, user, budget, expiry, token)
    }
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError> {
        PaymentClient::create_instrument(self, manager, user, connector, network, email, token)
    }
    fn process_payment(
        &self,
        request: &ProcessPaymentRequest,
    ) -> Result<PaymentProof, PaymentError> {
        self.process_request(request, self.operation_deadline)
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

    #[test]
    fn fake_session_read_is_scripted_and_records_calls() {
        let manager = "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/test";
        let fixture = Json::object(vec![("marker", Json::string("fixture"))]);
        let api = FakePaymentsApi::with_session(fixture.clone());
        let payments: &dyn PaymentsApi = &api;
        assert_eq!(
            payments
                .get_session(manager, "payment-session-TEST12345678901", "user")
                .unwrap(),
            fixture
        );
        assert_eq!(api.session_calls(), 1);
        assert_eq!(
            payments
                .create_session(manager, "user", "10", 15, "token")
                .unwrap(),
            fixture
        );
        assert_eq!(api.session_create_calls(), 1);
        assert_eq!(
            payments.create_instrument(
                manager,
                "user",
                "connector",
                "ETHEREUM",
                "test@example.invalid",
                "token"
            ),
            Err(PaymentError::InvalidResponse)
        );
        assert_eq!(api.instrument_create_calls(), 1);
        let instrument = Json::object(vec![("id", Json::string("instrument"))]);
        let api = api.with_instrument(instrument.clone());
        assert_eq!(
            api.create_instrument(
                manager,
                "user",
                "connector",
                "ETHEREUM",
                "test@example.invalid",
                "token"
            )
            .unwrap(),
            instrument
        );
        assert_eq!(api.instrument_create_calls(), 2);
    }

    #[test]
    fn fake_replays_matching_token_and_expires_idempotency_record() {
        let manager = "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/test";
        let request = ProcessPaymentRequest::from_approved(
            &super::operation_contracts::approved_for(manager),
        );
        let first = Json::object(vec![("proof", Json::string("first"))]);
        let second = Json::object(vec![("proof", Json::string("second"))]);
        let api = FakePaymentsApi::with_proof(first.clone());
        assert_eq!(api.process_payment(&request).unwrap().payload(), &first);
        api.set_proof(second.clone());
        assert_eq!(api.process_payment(&request).unwrap().payload(), &first);
        assert_eq!(api.calls(), 2);
        assert_eq!(api.authorizations(), 1);
        let mut conflicting = ProcessPaymentRequest::from_approved(
            &super::operation_contracts::approved_for(manager),
        );
        conflicting.body.push(b' ');
        assert_eq!(
            api.process_payment(&conflicting).err(),
            Some(PaymentError::Service("ConflictException"))
        );
        assert_eq!(api.authorizations(), 1);
        api.expire_idempotency_records();
        assert_eq!(api.process_payment(&request).unwrap().payload(), &second);
        assert_eq!(api.authorizations(), 2);
    }

    #[test]
    fn fake_admin_and_reads_reject_other_accounts_before_recording() {
        let api = FakePaymentsApi::default();
        let manager = "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test";
        for operation in 0..3 {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match operation {
                    0 => api.get_session(manager, "session", "user"),
                    1 => api.create_session(manager, "user", "10", 15, "token"),
                    _ => api.create_instrument(
                        manager,
                        "user",
                        "connector",
                        "ETHEREUM",
                        "test@example.invalid",
                        "token",
                    ),
                }));
            assert!(result.is_err());
        }
        assert_eq!(
            (
                api.session_calls(),
                api.session_create_calls(),
                api.instrument_create_calls()
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn payment_error_class_mapping_is_exhaustive_and_value_free() {
        for (error, expected) in [
            (PaymentError::Service("ValidationException"), "ClientError"),
            (PaymentError::InvalidResponse, "RuntimeError"),
            (
                PaymentError::Transport(PaymentErrorClass::ConnectError),
                "ConnectError",
            ),
            (
                PaymentError::Transport(PaymentErrorClass::ConnectTimeout),
                "ConnectTimeout",
            ),
            (
                PaymentError::Transport(PaymentErrorClass::ReadTimeout),
                "ReadTimeout",
            ),
            (
                PaymentError::Transport(PaymentErrorClass::RemoteProtocolError),
                "RemoteProtocolError",
            ),
            (
                PaymentError::Transport(PaymentErrorClass::NoCredentialsError),
                "NoCredentialsError",
            ),
            (
                PaymentError::Transport(PaymentErrorClass::ClientError),
                "ClientError",
            ),
            (
                PaymentError::Transport(PaymentErrorClass::RuntimeError),
                "RuntimeError",
            ),
        ] {
            assert_eq!(error.class_name(), expected);
        }
    }

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
    fn transport_preserves_credential_error_class_without_leaking_details() {
        let transport = FakeStreamingTransport::new().open_error(AwsError::NoCredentials);
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        let error = client
            .get_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "session",
                "user",
            )
            .unwrap_err();
        assert_eq!(format!("{error:?}"), "Transport(NoCredentialsError)");
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn malformed_http_is_not_retried_and_retains_its_class() {
        let transport = FakeStreamingTransport::new().open_error(AwsError::Protocol);
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.get_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "session",
                "user"
            ),
            Err(PaymentError::Transport(
                PaymentErrorClass::RemoteProtocolError
            ))
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn sts_identity_has_fixed_endpoint_signing_and_valid_response() {
        let body = br#"<GetCallerIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><GetCallerIdentityResult><Arn>arn:aws:iam::111111111111:role/test</Arn><UserId>TEST</UserId><Account>111111111111</Account></GetCallerIdentityResult><ResponseMetadata><RequestId>test</RequestId></ResponseMetadata></GetCallerIdentityResponse>"#;
        let transport =
            FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::data(body)]);
        let signer = toolkit_aws::testing::FakeSigner::new();
        let client = PaymentClient::new("us-east-1", &transport, &signer).unwrap();
        assert_eq!(client.sts_check(), Ok(()));
        let calls = transport.requests();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, "https://sts.us-east-1.amazonaws.com/");
        assert_eq!(
            calls[0].body,
            b"Action=GetCallerIdentity&Version=2011-06-15"
        );
        assert_eq!(
            calls[0].headers.get("authorization"),
            Some("FAKE-SIGNED us-east-1/sts")
        );
        assert_eq!(
            calls[0].headers.get("content-type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            calls[0]
                .headers
                .get("x-amzn-bedrock-agentcore-payments-user-id"),
            None
        );
    }

    #[test]
    fn sts_identity_rejects_malformed_or_wrong_response() {
        for body in [
            "<GetCallerIdentityResponse>",
            "<GetCallerIdentityResponse><GetCallerIdentityResult/></GetCallerIdentityResponse>",
            "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"/>",
            "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult/><GetCallerIdentityResult/></GetCallerIdentityResponse>",
        ] {
            let transport = FakeStreamingTransport::new().respond(
                200,
                &[],
                vec![FakeChunk::data(body.as_bytes())],
            );
            let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
            assert_eq!(client.sts_check(), Err(PaymentError::InvalidResponse));
        }
    }

    #[test]
    #[should_panic(expected = "fake payments require fixture account")]
    fn fake_payments_rejects_non_fixture_manager_account() {
        let api = FakePaymentsApi {
            calls: Cell::new(0),
            ..FakePaymentsApi::default()
        };
        let request = ProcessPaymentRequest::from_approved(&super::operation_contracts::approved());
        let _ = api.process_payment(&request);
    }

    #[test]
    fn approved_payment_is_consumed_once_and_retains_verified_terms() {
        let api = FakePaymentsApi {
            calls: Cell::new(0),
            ..FakePaymentsApi::default()
        };
        let authorized = process_payment(
            &api,
            super::operation_contracts::approved_for(
                "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/test",
            ),
        )
        .unwrap();
        assert_eq!(api.calls.get(), 1);
        assert_eq!(authorized.amount, "100000");
        assert_eq!(authorized.version, 2);
        assert_eq!(authorized.recipient, "0xB");
        assert!(matches!(authorized.proof.payload(), Json::Object(_)));
        let request = ProcessPaymentRequest::from_approved(&super::operation_contracts::approved());
        let shown = format!("{request:?}");
        assert!(!shown.contains("100000"));
        assert!(!shown.contains("user"));
        assert!(!shown.contains("0xB"));
    }

    #[test]
    fn fake_returns_configured_proof_for_fixture_approval() {
        let proof = parse_json(r#"{"authorization":{"to":"0xB","value":"100000"}}"#).unwrap();
        let api = FakePaymentsApi::with_proof(proof.clone());
        let approved = super::operation_contracts::approved_for(
            "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/test",
        );
        let authorized = process_payment(&api, approved).unwrap();
        assert_eq!(authorized.proof.payload(), &proof);
        assert_eq!(api.calls(), 1);
    }

    #[test]
    fn other_payment_operations_have_golden_wires() {
        let session = br#"{"paymentSession":{"paymentSessionId":"payment-session-TEST12345678901","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","userId":"user","expiryTimeInMinutes":60,"createdAt":1,"updatedAt":1}}"#;
        let instrument = br#"{"paymentInstrument":{"paymentInstrumentId":"pi","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentConnectorId":"connector","userId":"user","paymentInstrumentType":"EMBEDDED_CRYPTO_WALLET","paymentInstrumentDetails":{"embeddedCryptoWallet":{"network":"ETHEREUM","linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]}},"createdAt":1,"status":"ACTIVE","updatedAt":1}}"#;
        let transport = FakeStreamingTransport::new()
            .respond(200, &[], vec![FakeChunk::data(session)])
            .respond(201, &[], vec![FakeChunk::data(session)])
            .respond(201, &[], vec![FakeChunk::data(instrument)]);
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert!(client
            .get_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "payment-session-TEST12345678901",
                "user"
            )
            .is_ok());
        assert!(client
            .create_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "10",
                60,
                "token"
            )
            .is_ok());
        assert!(client
            .create_instrument(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "connector",
                "ETHEREUM",
                "user@example.com",
                "token"
            )
            .is_ok());
        let requests = transport.requests();
        let expected = [
            (
                "getPaymentSession",
                "openclaw-aws-agents-pay",
                get_body("arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij", "payment-session-TEST12345678901"),
            ),
            (
                "createPaymentSession",
                "aws-agents-pay",
                session_body("arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij", "10", 60, "token"),
            ),
            (
                "createPaymentInstrument",
                "aws-agents-pay",
                instrument_body("arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij", "connector", "ETHEREUM", "user@example.com", "token"),
            ),
        ];
        assert_eq!(requests.len(), expected.len());
        for (request, (path, agent, body)) in requests.iter().zip(expected) {
            assert_eq!(
                request.url,
                format!("https://bedrock-agentcore.us-east-1.amazonaws.com/payments/{path}")
            );
            assert_eq!(
                request
                    .headers
                    .get("x-amzn-bedrock-agentcore-payments-user-id"),
                Some("user")
            );
            assert_eq!(
                request
                    .headers
                    .get("x-amzn-bedrock-agentcore-payments-agent-name"),
                Some(agent)
            );
            assert_eq!(request.body, body.as_bytes());
        }
    }

    #[test]
    fn admin_rejects_invalid_session_and_wallet_inputs_before_transport() {
        let transport = FakeStreamingTransport::new();
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.create_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "10",
                14,
                "token"
            ),
            Err(PaymentError::InvalidResponse)
        );
        assert_eq!(
            client.create_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "10",
                481,
                "token"
            ),
            Err(PaymentError::InvalidResponse)
        );
        assert_eq!(
            client.create_instrument(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "connector",
                "OTHER",
                "user@example.com",
                "token"
            ),
            Err(PaymentError::InvalidResponse)
        );
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn instrument_response_rejects_invalid_nested_and_required_members() {
        let good = r#"{"paymentInstrument":{"paymentInstrumentId":"pi","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentConnectorId":"connector","userId":"user","paymentInstrumentType":"EMBEDDED_CRYPTO_WALLET","paymentInstrumentDetails":{"embeddedCryptoWallet":{"network":"ETHEREUM","linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]}},"createdAt":1,"status":"ACTIVE","updatedAt":1}}"#;
        for bad in [
            good.replace(
                r#""paymentInstrumentId":"pi""#,
                r#""paymentInstrumentId":null"#,
            ),
            good.replace(r#""createdAt":1"#, r#""createdAt":"invalid""#),
            good.replace(r#""status":"ACTIVE""#, r#""status":null"#),
            good.replace(r#""network":"ETHEREUM""#, r#""network":null"#),
            good.replace(
                r#","linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]"#,
                "",
            ),
            good.replace(
                r#""linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]"#,
                r#""linkedAccounts":null"#,
            ),
        ] {
            let transport = FakeStreamingTransport::new().respond(
                201,
                &[],
                vec![FakeChunk::data(bad.as_bytes())],
            );
            let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
            assert_eq!(
                client.create_instrument(
                    "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                    "user",
                    "connector",
                    "ETHEREUM",
                    "user@example.com",
                    "token"
                ),
                Err(PaymentError::InvalidResponse)
            );
        }
    }

    #[test]
    fn instrument_response_rejects_invalid_unions_and_enums() {
        let good = r#"{"paymentInstrument":{"paymentInstrumentId":"pi","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentConnectorId":"connector","userId":"user","paymentInstrumentType":"EMBEDDED_CRYPTO_WALLET","paymentInstrumentDetails":{"embeddedCryptoWallet":{"network":"ETHEREUM","linkedAccounts":[{"email":{"emailAddress":"user@example.com"}}]}},"createdAt":1,"status":"ACTIVE","updatedAt":1}}"#;
        for bad in [
            good.replace("\"status\":\"ACTIVE\"", "\"status\":\"OTHER\""),
            good.replace("{\"email\":{\"emailAddress\":\"user@example.com\"}}", "{}"),
            good.replace("{\"email\":{\"emailAddress\":\"user@example.com\"}}", "{\"email\":{}}"),
            good.replace("{\"email\":{\"emailAddress\":\"user@example.com\"}}", "{\"email\":{\"emailAddress\":\"x\"},\"sms\":{\"phoneNumber\":\"+123\"}}"),
            good.replace("[{\"email\":{\"emailAddress\":\"user@example.com\"}}]", "[{\"email\":{\"emailAddress\":\"user@example.com\"}},{\"sms\":{\"phoneNumber\":\"+123\"}}]"),
            good.replace("{\"email\":{\"emailAddress\":\"user@example.com\"}}", "{\"oAuth2\":{\"google\":{},\"apple\":{}}}"),
        ] {
            let transport = FakeStreamingTransport::new().respond(201, &[], vec![FakeChunk::data(bad.as_bytes())]);
            let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
            assert_eq!(client.create_instrument("arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij", "user", "connector", "ETHEREUM", "user@example.com", "token"), Err(PaymentError::InvalidResponse), "{bad}");
        }
    }

    #[test]
    fn wallet_model_accepts_each_valid_linked_account_variant() {
        let model = parse_json(PAYMENT_SERVICE_MODEL).unwrap();
        for accounts in [
            "[]",
            r#"[{"email":{"emailAddress":"user@example.com"}}]"#,
            r#"[{"sms":{"phoneNumber":"+123456789"}}]"#,
            r#"[{"developerJwt":{"kid":"key-1","sub":"subject"}}]"#,
            r#"[{"oAuth2":{"google":{"sub":"subject"}}}]"#,
        ] {
            let wallet = parse_json(&format!(
                r#"{{"network":"ETHEREUM","linkedAccounts":{accounts}}}"#
            ))
            .unwrap();
            assert!(
                valid_wallet_shape(&model, "EmbeddedCryptoWallet", &wallet),
                "{accounts}"
            );
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
            Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout))
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn operation_deadline_bounds_open_and_rejects_expired_admission() {
        let now = Instant::now();
        let clock = FakeClock(Cell::new(now));
        let expired_transport = FakeStreamingTransport::new();
        let mut expired = PaymentClient::new("us-east-1", &expired_transport, &NoSigner).unwrap();
        expired.clock = &clock;
        assert_eq!(
            expired.process_payment_until(super::operation_contracts::approved(), now),
            Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout))
        );
        assert!(expired_transport.requests().is_empty());
        let stalled_transport =
            FakeStreamingTransport::new().respond(200, &[], vec![FakeChunk::Stall]);
        let mut stalled = PaymentClient::new("us-east-1", &stalled_transport, &NoSigner).unwrap();
        stalled.clock = &clock;
        let cutoff = now + Duration::from_secs(1);
        assert_eq!(
            stalled.process_payment_until(super::operation_contracts::approved(), cutoff),
            Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout))
        );
        assert_eq!(stalled_transport.requests().len(), 1);
        assert_eq!(stalled_transport.open_limits()[0].deadline(), Some(cutoff));
    }

    #[test]
    fn operation_deadline_rejects_expired_session_and_admin_calls() {
        let transport = FakeStreamingTransport::new();
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner)
            .unwrap()
            .with_operation_deadline(Instant::now().checked_sub(Duration::from_secs(1)).unwrap());
        let expected = Err(PaymentError::Transport(PaymentErrorClass::ReadTimeout));
        assert_eq!(
            client.get_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "payment-session-TEST12345678901",
                "user"
            ),
            expected
        );
        assert_eq!(
            client.create_session(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "1",
                15,
                "token"
            ),
            expected
        );
        assert_eq!(
            client.create_instrument(
                "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij",
                "user",
                "connector",
                "ETHEREUM",
                "a@example.com",
                "token"
            ),
            expected
        );
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn retry_backoff_that_reaches_deadline_is_not_slept() {
        let now = Instant::now();
        let clock = FakeClock(Cell::new(now));
        let transport = FakeStreamingTransport::new().open_error(AwsError::connect("offline"));
        let mut client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        client.clock = &clock;
        client.jitter = Some(&toolkit_aws::NoJitter);
        assert_eq!(
            client.process_payment_until(
                super::operation_contracts::approved(),
                now + Duration::from_secs(2)
            ),
            Err(PaymentError::Transport(PaymentErrorClass::ConnectError))
        );
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(clock.now(), now);
    }

    #[test]
    fn read_timeout_retries_identical_payment_bytes() {
        let response = br#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
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
        let response = br#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
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
    fn streamed_oversized_error_keeps_allowlisted_header() {
        let transport = FakeStreamingTransport::new().respond(
            409,
            &[("x-amzn-ErrorType", "ConflictException")],
            vec![FakeChunk::data(&vec![b'x'; 65_537])],
        );
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.process_payment(super::operation_contracts::approved()),
            Err(PaymentError::Service("ConflictException"))
        );
        assert_eq!(transport.requests().len(), 1);
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
    fn duplicate_length_marker_is_refused_before_pulling() {
        let transport = FakeStreamingTransport::new().respond(
            200,
            &[("content-length", "invalid-duplicate")],
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
        let response = br#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
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
        let response = br#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{"signature":"secret"}}},"createdAt":1,"updatedAt":1}"#;
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
    fn unexpected_success_status_is_an_invalid_response() {
        let response = br#"{"processPaymentId":"12345678-abcd-1234-abcd-123456789abc","paymentManagerArn":"arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/test-abcdefghij","paymentSessionId":"payment-session-TEST12345678901","paymentInstrumentId":"pi","paymentType":"CRYPTO_X402","status":"PROOF_GENERATED","paymentOutput":{"cryptoX402":{"version":"2","payload":{}}},"createdAt":1,"updatedAt":1}"#;
        let transport =
            FakeStreamingTransport::new().respond(201, &[], vec![FakeChunk::data(response)]);
        let client = PaymentClient::new("us-east-1", &transport, &NoSigner).unwrap();
        assert_eq!(
            client.process_payment(super::operation_contracts::approved()),
            Err(PaymentError::InvalidResponse)
        );
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.frames_pulled(), 0);
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
