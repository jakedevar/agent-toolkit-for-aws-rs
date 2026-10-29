//! The payment flow: `fetch`, session `status` and the browser handle
//! (design sections 2-5; legacy `x402_fetch.py`).
//!
//! Order (R-SPEND-ORDER): config, origin, method, probe, challenge, the
//! policy gate, then exactly one `ProcessPayment` (R-PP-ONCE), R-PP-VERIFY,
//! and replays of the same header bytes (R-RETRY). Every refusal before the
//! gate approves makes zero authorizations (T-ORDER-PRE); every failure after
//! it makes no further one (T-ORDER-POST). Deadline admission (R-DNS-ADMIT)
//! runs around every lookup and before every network step. Every document
//! goes through the known-secret scrub (R-SCRUB) before it is returned.

use std::time::{Duration, Instant};

use toolkit_core::Json;

use super::agentcore::{self, PaymentError};
use super::config::{resolve_resource, Tunables};
use super::deps::Deps;
use super::net::{method_refusal, MerchantRequest, MerchantResponse, ProofHeader};
use super::policy::{
    assert_public_https_url, authorize, check_allowed_origins, PaymentRequest, PolicyError,
    Resources,
};
use super::proof;
use super::urlparse::urlparse;
use super::vault::{Vault, HANDLE_TTL};

/// The paid body is cut to this many characters (`return_body`).
const BODY_CHARS: usize = 10_240;
/// D17: the reported content type is bounded.
const CONTENT_TYPE_CHARS: usize = 256;
/// R-SCRUB searches for secrets of at least this many bytes.
const MIN_SECRET_BYTES: usize = 8;
/// R-SCRUB also refuses a document holding any piece of a secret this long.
const SECRET_WINDOW: usize = 32;
/// D13: the pause between merchant replays.
const REPLAY_PAUSE: Duration = Duration::from_secs(1);

const SHAPE: &str = "Payment header generation returned an unexpected shape.";
const WITHHELD: &str = "Body withheld: paid publisher content is untrusted and must not enter the payment-capable model context. Use a separate no-payment/no-network analysis context if summarisation is required. To opt in, set 'return_body: true' in the policy section of the config file.";
const NO_SESSION: &str = "No payment session is configured.";
const NEW_SESSION: &str =
    "An operator must run: agents_pay_admin.py new-session (requires a human at a terminal).";
const MINT_BUDGET: &str = "An operator must run: agents_pay_admin.py new-session (requires a human at a terminal). The agent cannot mint budget.";
const CHECK_SESSION: &str = "An operator must check PAYMENT_SESSION_ID and AWS credentials.";
const BROWSER_NEXT: &str = "Call attach_browser_payment(handle, url) to get the header to set on the browser, then navigate. The handle is single-use and expires.";

/// R-ERR: every internal failure of the flow, each mapped to the fixed
/// class name Python would have printed. Exhaustive, so a new variant
/// forces a choice (T-ERR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowError {
    /// Connect or TLS failure.
    Connect,
    /// Connect timeout.
    ConnectTimeout,
    /// Idle or absolute deadline, or the operation deadline.
    Timeout,
    /// Malformed HTTP.
    Protocol,
    /// Any `AgentCore` error response.
    Service,
    /// No credentials.
    NoCredentials,
    /// Python `int()` failure.
    Value,
    /// A JSON failure where Python would raise.
    Json,
    /// Anything else.
    Runtime,
}

impl FlowError {
    /// The fixed class name (R-ERR table).
    #[must_use]
    pub fn class_name(self) -> &'static str {
        match self {
            Self::Connect => "ConnectError",
            Self::ConnectTimeout => "ConnectTimeout",
            Self::Timeout => "ReadTimeout",
            Self::Protocol => "RemoteProtocolError",
            Self::Service => "ClientError",
            Self::NoCredentials => "NoCredentialsError",
            Self::Value => "ValueError",
            Self::Json => "JSONDecodeError",
            Self::Runtime => "RuntimeError",
        }
    }

    /// `"{name} during payment flow."`
    #[must_use]
    pub fn during_flow(self) -> String {
        format!("{} during payment flow.", self.class_name())
    }

    /// `"{name} while reading session status."`
    #[must_use]
    pub fn during_status(self) -> String {
        format!("{} while reading session status.", self.class_name())
    }

    /// The class of an `AgentCore` failure.
    #[must_use]
    pub fn from_payment(error: &PaymentError) -> Self {
        use agentcore::PaymentErrorClass as C;
        match error {
            PaymentError::Service(_) => Self::Service,
            PaymentError::InvalidResponse => Self::Runtime,
            PaymentError::Transport(class) => match class {
                C::ConnectError => Self::Connect,
                C::ConnectTimeout => Self::ConnectTimeout,
                C::ReadTimeout => Self::Timeout,
                C::RemoteProtocolError => Self::Protocol,
                C::NoCredentialsError => Self::NoCredentials,
                C::ClientError => Self::Service,
                C::RuntimeError => Self::Runtime,
            },
        }
    }
}

/// A model-facing refusal: a fixed text, possibly with bounded caller values.
struct Refusal(String);

impl From<PolicyError> for Refusal {
    fn from(error: PolicyError) -> Self {
        Self(error.reason())
    }
}

fn timeout() -> Refusal {
    Refusal(FlowError::Timeout.during_flow())
}

fn refusal_doc(reason: &str) -> Json {
    Json::object(vec![
        ("paid", Json::Bool(false)),
        ("refused", Json::Bool(true)),
        ("reason", Json::string(reason)),
    ])
}

/// One document as Python's `json.dumps` writes it (`, `/`: ` separators,
/// ASCII only).
#[must_use]
pub fn render(document: &Json) -> String {
    crate::asm_exec::reference::python_json_dumps(document)
}

/// The fetch and browser-handle exit status: 2 when `refused` is true.
#[must_use]
pub fn exit_code(document: &Json) -> u8 {
    if document.get("refused") == Some(&Json::Bool(true)) {
        2
    } else {
        0
    }
}

/// The `--status` exit status: 0 only when the session is usable.
#[must_use]
pub fn status_exit_code(document: &Json) -> u8 {
    if document.get("usable") == Some(&Json::Bool(true)) {
        0
    } else {
        2
    }
}

/// The invocation's bounds and secrets.
struct Run<'d, 'a> {
    deps: &'d Deps<'a>,
    policy: Json,
    tunables: Tunables,
    deadline: Instant,
    /// Live secrets of this invocation, for R-SCRUB.
    secrets: Vec<String>,
}

impl<'d, 'a> Run<'d, 'a> {
    fn start(deps: &'d Deps<'a>) -> Result<Self, Refusal> {
        let start = deps.clock.now();
        let tunables = Tunables::from_env(&|name| deps.config.env(name));
        let policy = deps.config.runtime_config().map_err(Refusal)?;
        Ok(Self {
            deadline: operation_deadline(start, &tunables),
            deps,
            policy,
            tunables,
            secrets: Vec::new(),
        })
    }

    fn admitted(&self) -> Result<(), Refusal> {
        if self.deps.clock.now() >= self.deadline {
            Err(timeout())
        } else {
            Ok(())
        }
    }

    /// R-DNS-ADMIT around a step that may block in a lookup.
    fn around<T>(&self, step: impl FnOnce() -> T) -> Result<T, Refusal> {
        self.admitted()?;
        let value = step();
        self.admitted()?;
        Ok(value)
    }

    fn env(&self, name: &str) -> Option<String> {
        self.deps.config.env(name)
    }

    fn resource(&self, key: &str) -> Result<Option<String>, Refusal> {
        resolve_resource(&self.policy, key, &|name| self.env(name)).map_err(Refusal)
    }

    fn timeout_per_request(&self) -> Duration {
        Duration::from_secs_f64(self.tunables.timeout_seconds)
    }

    fn request(
        &self,
        url: &str,
        method: &str,
        proof: Option<&ProofHeader>,
    ) -> Result<MerchantResponse, Refusal> {
        self.admitted()?;
        self.deps
            .merchant
            .request(&MerchantRequest {
                url,
                method,
                proof,
                body_cap: u64::try_from(self.tunables.max_body_bytes).unwrap_or(u64::MAX),
                timeout: self.timeout_per_request(),
                operation_deadline: self.deadline,
            })
            .map_err(|error| Refusal(error.0))
    }

    /// The pre-network checks: R-ORIGIN with admission, `allowed_origins`.
    fn origin(&self, url: &str) -> Result<String, Refusal> {
        let origin = self.around(|| assert_public_https_url(url, self.deps.resolver))??;
        check_allowed_origins(&self.policy, &origin)?;
        Ok(origin)
    }

    fn add_secret(&mut self, secret: &str) {
        if secret.len() >= MIN_SECRET_BYTES && !self.secrets.iter().any(|known| known == secret) {
            self.secrets.push(secret.to_owned());
        }
    }
}

// The bounds are small and positive; truncation is the legacy `int()`.
#[allow(clippy::cast_precision_loss)]
fn operation_deadline(start: Instant, tunables: &Tunables) -> Instant {
    // R-NET: (2 + attempts) x 3 x T + 60 s covers the probe, the replays and
    // the AWS call.
    let seconds =
        (2.0 + tunables.max_payment_attempts as f64) * 3.0 * tunables.timeout_seconds + 60.0;
    start + Duration::from_secs_f64(seconds)
}

/// The authorized, verified result of the 402 path.
struct Paid {
    header: ProofHeader,
    receipt: Json,
    origin: String,
}

/// From the probe's 402 through the gate, `ProcessPayment` and R-PP-VERIFY.
fn pay(
    run: &mut Run<'_, '_>,
    url: &str,
    origin: String,
    probe: &MerchantResponse,
    purchase_id: Option<&str>,
) -> Result<Paid, Refusal> {
    let headers: Vec<(String, String)> = probe
        .headers
        .entries()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    let challenge = super::challenge::extract(&headers, &probe.body)?;
    let resources = Resources {
        payment_manager_arn: run.resource("payment_manager_arn")?,
        payment_instrument_id: run.resource("payment_instrument_id")?,
        payment_session_id: run.resource("payment_session_id")?,
        user_id: run.resource("user_id")?,
    };
    let approved = run.around(|| {
        authorize(
            &PaymentRequest {
                url,
                challenge: &challenge,
                policy: &run.policy,
                resources: &resources,
                purchase_id,
            },
            run.deps.resolver,
        )
    })??;
    let region = run_region(run, approved.manager_arn())?;
    let path = urlparse(url)
        .map(|p| p.path().to_owned())
        .unwrap_or_default();
    let receipt = Json::object(vec![
        ("amount_usd", Json::string(approved.amount_usd())),
        (
            "network",
            approved
                .entry()
                .get("network")
                .cloned()
                .unwrap_or(Json::Null),
        ),
        ("resource", Json::string(format!("{origin}{path}"))),
    ]);
    let manager = approved.manager_arn().to_owned();
    let session = approved.session_id().to_owned();
    run.add_secret(&session);
    run.admitted()?;
    let api = run
        .deps
        .payments
        .open(&region, &manager, run.deadline)
        .map_err(Refusal)?;
    let authorized = agentcore::process_payment(api.as_ref(), approved).map_err(|error| {
        Refusal(match error {
            PaymentError::InvalidResponse => SHAPE.to_owned(),
            other => FlowError::from_payment(&other).during_flow(),
        })
    })?;
    let verified = proof::verify(authorized, run.deps.clock)
        .map_err(|error| Refusal(error.reason().to_owned()))?;
    let header = verified.header();
    run.add_secret(header.value());
    if let Some(signature) = verified.signature() {
        run.add_secret(signature);
    }
    Ok(Paid {
        header,
        receipt,
        origin,
    })
}

/// The runtime region chain: config, `AWS_REGION`, the manager ARN, the
/// profile (D19).
fn run_region(run: &Run<'_, '_>, manager: &str) -> Result<String, Refusal> {
    let configured = run.resource("region")?;
    let profile = run.deps.payments.profile_region();
    agentcore::runtime_region(configured.as_deref(), None, manager, profile.as_deref())
        .ok_or_else(|| Refusal(FlowError::Runtime.during_flow()))
}

/// The metadata of a merchant response, with the opt-in body (R-RED 4).
fn safe_content(run: &Run<'_, '_>, response: &MerchantResponse) -> Vec<(String, Json)> {
    let content_type: String = response
        .headers
        .get("content-type")
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .chars()
        .take(CONTENT_TYPE_CHARS)
        .collect();
    let mut members = vec![
        (
            "content_type".to_owned(),
            Json::string(if content_type.is_empty() {
                "unknown".to_owned()
            } else {
                content_type
            }),
        ),
        (
            "body_sha256".to_owned(),
            Json::string(toolkit_core::sha256_hex(&response.body)),
        ),
        (
            "bytes".to_owned(),
            Json::Int(i64::try_from(response.body.len()).unwrap_or(i64::MAX)),
        ),
    ];
    if run.policy.get("return_body") == Some(&Json::Bool(true)) {
        // R-SCRUB: replace every live secret in the whole decoded body
        // before it is cut, so a cut cannot leave part of one behind.
        let mut text = String::from_utf8_lossy(&response.body).into_owned();
        let mut redacted = false;
        for secret in &run.secrets {
            if text.contains(secret.as_str()) {
                text = text.replace(secret.as_str(), "[REDACTED]");
                redacted = true;
            }
        }
        let body: String = text.chars().take(BODY_CHARS).collect();
        members.push(("body_returned".to_owned(), Json::Bool(true)));
        members.push(("body".to_owned(), Json::string(body)));
        members.push((
            "truncated".to_owned(),
            Json::Bool(response.body.len() > BODY_CHARS),
        ));
        if redacted {
            members.push(("redacted".to_owned(), Json::Bool(true)));
        }
    } else {
        members.push(("body_returned".to_owned(), Json::Bool(false)));
        members.push(("note".to_owned(), Json::string(WITHHELD)));
    }
    members
}

fn document(head: Vec<(&str, Json)>, tail: Vec<(String, Json)>) -> Json {
    let mut members: Vec<(String, Json)> = head
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    members.extend(tail);
    Json::Object(members)
}

/// R-SCRUB: a live secret anywhere in the rendered document (outside the
/// already-redacted body) replaces the whole document with the generic text.
/// The comparison ignores ASCII case: the content type is lower-cased, and a
/// lower-cased proof still carries the proof.
/// Whether `text` holds `secret`, or a piece of it: a cut (the D17 bound on
/// the content type) or a partial echo still carries part of the proof, so
/// every window of [`SECRET_WINDOW`] bytes at a stride of 8 is searched.
fn leaks(text: &str, secret: &str) -> bool {
    if secret.len() < MIN_SECRET_BYTES {
        return false;
    }
    if secret.len() <= SECRET_WINDOW {
        return text.contains(secret);
    }
    (0..=secret.len() - SECRET_WINDOW)
        .step_by(8)
        .chain(std::iter::once(secret.len() - SECRET_WINDOW))
        .filter_map(|start| secret.get(start..start + SECRET_WINDOW))
        .any(|window| text.contains(window))
}

fn scrubbed(document: Json, secrets: &[String]) -> Json {
    let text = render(&document).to_ascii_lowercase();
    if secrets
        .iter()
        .any(|secret| leaks(&text, &secret.to_ascii_lowercase()))
    {
        refusal_doc(&FlowError::Runtime.during_flow())
    } else {
        document
    }
}

fn fetch_inner(
    run: &mut Run<'_, '_>,
    url: &str,
    purchase_id: Option<&str>,
    method: &str,
) -> Result<Json, Refusal> {
    let origin = run.origin(url)?;
    if let Some(refusal) = method_refusal(method) {
        return Err(Refusal(refusal));
    }
    let probe = run.request(url, method, None)?;
    if probe.status != 402 {
        return Ok(document(
            vec![
                ("status_code", Json::Int(i64::from(probe.status))),
                ("paid", Json::Bool(false)),
                ("untrusted", Json::Bool(true)),
            ],
            safe_content(run, &probe),
        ));
    }
    let paid = pay(run, url, origin, &probe, purchase_id)?;
    let attempts = run.tunables.max_payment_attempts;
    let mut last = None;
    for attempt in 1..=attempts {
        if attempt > 1 {
            // D13, inside the operation deadline.
            if run.deps.clock.now() + REPLAY_PAUSE >= run.deadline {
                return Err(timeout());
            }
            run.deps.clock.sleep(REPLAY_PAUSE);
        }
        let response = run.request(url, method, Some(&paid.header))?;
        let settled = response.status != 402;
        last = Some(response);
        if settled {
            break;
        }
    }
    let Some(response) = last.filter(|response| response.status != 402) else {
        let attempts = i64::try_from(attempts).unwrap_or(i64::MAX);
        return Ok(Json::object(vec![
            ("paid", Json::Bool(false)),
            ("refused", Json::Bool(false)),
            ("status_code", Json::Int(402)),
            ("attempts", Json::Int(attempts)),
            (
                "reason",
                Json::string(format!(
                    "Paid and replayed {attempts} times but the merchant still returns 402 \u{2014} usually transient on-chain settlement. The same authorization was replayed each time, so there is no double charge. Retry shortly, or raise X402_MAX_PAYMENT_ATTEMPTS."
                )),
            ),
        ]));
    };
    Ok(document(
        vec![
            ("status_code", Json::Int(i64::from(response.status))),
            ("paid", Json::Bool((200..300).contains(&response.status))),
            ("untrusted", Json::Bool(true)),
            ("receipt", paid.receipt.clone()),
        ],
        safe_content(run, &response),
    ))
}

/// `toolkit agents-pay fetch URL`: fetch, paying only when the policy allows
/// it. Returns the one model-facing document.
#[must_use]
pub fn fetch(deps: &Deps<'_>, url: &str, purchase_id: Option<&str>, method: &str) -> Json {
    let mut run = match Run::start(deps) {
        Ok(run) => run,
        Err(Refusal(reason)) => return refusal_doc(&reason),
    };
    let outcome = fetch_inner(&mut run, url, purchase_id, method);
    let document = match outcome {
        Ok(document) => document,
        Err(Refusal(reason)) => refusal_doc(&reason),
    };
    scrubbed(document, &run.secrets)
}

fn browser_inner(
    run: &mut Run<'_, '_>,
    vault: &Vault,
    url: &str,
    purchase_id: Option<&str>,
) -> Result<Json, Refusal> {
    let origin = run.origin(url)?;
    let probe = run.request(url, "GET", None)?;
    if probe.status != 402 {
        return Err(Refusal(format!(
            "URL returned {}, not 402. No payment needed.",
            probe.status
        )));
    }
    let paid = pay(run, url, origin, &probe, purchase_id)?;
    let path = urlparse(url)
        .map(|parsed| parsed.path().to_owned())
        .unwrap_or_default();
    let path = if path.is_empty() {
        "/".to_owned()
    } else {
        path
    };
    let handle = vault
        .deposit(paid.header, &paid.origin, &path, run.deps.clock.now())
        .map_err(|error| Refusal(error.reason().to_owned()))?;
    Ok(Json::object(vec![
        ("paid", Json::Bool(true)),
        ("handle", Json::string(handle)),
        (
            "expires_in_seconds",
            Json::Int(i64::try_from(HANDLE_TTL.as_secs()).unwrap_or(90)),
        ),
        ("receipt", paid.receipt),
        ("next_step", Json::string(BROWSER_NEXT)),
    ]))
}

/// `toolkit agents-pay fetch --browser-handle URL`: pay and keep the proof
/// in `vault`, returning only an opaque single-use handle.
#[must_use]
pub fn prepare_browser_payment(
    deps: &Deps<'_>,
    vault: &Vault,
    url: &str,
    purchase_id: Option<&str>,
) -> Json {
    let mut run = match Run::start(deps) {
        Ok(run) => run,
        Err(Refusal(reason)) => return refusal_doc(&reason),
    };
    let document = match browser_inner(&mut run, vault, url, purchase_id) {
        Ok(document) => document,
        Err(Refusal(reason)) => refusal_doc(&reason),
    };
    scrubbed(document, &run.secrets)
}

fn status_failure(reason: &str) -> Json {
    Json::object(vec![
        ("usable", Json::Bool(false)),
        ("reason", Json::string(reason)),
        ("next_step", Json::string(CHECK_SESSION)),
    ])
}

/// `toolkit agents-pay fetch --status`: whether the configured session can
/// spend now (read-only; Q19 computation from the model members).
#[must_use]
pub fn status(deps: &Deps<'_>) -> Json {
    let start = deps.clock.now();
    let tunables = Tunables::from_env(&|name| deps.config.env(name));
    // Without a config file the environment is the fallback, as in Python.
    let policy = deps
        .config
        .runtime_config()
        .unwrap_or_else(|_| Json::object(vec![("_resources", Json::object(vec![]))]));
    let env = |name: &str| deps.config.env(name);
    let resource = |key: &str| resolve_resource(&policy, key, &env);
    let (Ok(session), Ok(manager), Ok(user), Ok(region)) = (
        resource("payment_session_id"),
        resource("payment_manager_arn"),
        resource("user_id"),
        resource("region"),
    ) else {
        return status_failure(&FlowError::Runtime.during_status());
    };
    let (Some(session), Some(manager)) = (
        session.filter(|s| !s.is_empty()),
        manager.filter(|m| !m.is_empty()),
    ) else {
        return Json::object(vec![
            ("usable", Json::Bool(false)),
            ("reason", Json::string(NO_SESSION)),
            ("next_step", Json::string(NEW_SESSION)),
        ]);
    };
    let Some(user) = user.filter(|u| !u.is_empty()) else {
        return status_failure(&FlowError::Runtime.during_status());
    };
    let profile = deps.payments.profile_region();
    let Some(region) =
        agentcore::runtime_region(region.as_deref(), None, &manager, profile.as_deref())
    else {
        return status_failure(&FlowError::Runtime.during_status());
    };
    let api = match deps
        .payments
        .open(&region, &manager, operation_deadline(start, &tunables))
    {
        Ok(api) => api,
        Err(reason) => return status_failure(&reason),
    };
    let value = match api.get_session(&manager, &session, &user) {
        Ok(value) => value,
        Err(error) => return status_failure(&FlowError::from_payment(&error).during_status()),
    };
    let Some(session_value) = value.get("paymentSession") else {
        return status_failure(&FlowError::Runtime.during_status());
    };
    let computed = agentcore::session_status(session_value, deps.clock.unix_micros());
    let mut members = vec![
        ("usable", Json::Bool(computed.usable)),
        ("status", Json::string(computed.status)),
    ];
    if let Some(budget) = computed.budget_usd {
        members.push(("budget_usd", Json::string(budget)));
    }
    if let Some(spent) = computed.spent_usd {
        members.push(("spent_usd", Json::string(spent)));
    }
    if !computed.usable {
        members.push(("next_step", Json::string(MINT_BUDGET)));
    }
    scrubbed(Json::object(members), &[session])
}

#[cfg(test)]
mod tests;
