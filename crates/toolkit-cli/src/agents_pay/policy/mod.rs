//! The x402 payment policy gate (x402 S1, #5118).
//!
//! Every decision is made here, from the operator's policy, before anything
//! can spend. [`authorize`] runs the legacy `authorize_payment` checks in
//! their order and texts, then the Rust additions, and only then builds the
//! one [`Approved`] a spending call accepts (R-SPEND-ORDER):
//!
//! 1. R-ORIGIN ([`assert_public_https_url`]): https, a host, no userinfo,
//!    every resolved address public (`toolkit_aws::endpoint::is_non_public`,
//!    D8); then the optional `allowed_origins` pin.
//! 2. The challenge must be an object.
//! 3. R-POL ([`select_accept_entry`]): the policy shape, then the first
//!    `accepts` entry that passes every check; a uniform refusal otherwise
//!    ([`UNIFORM_REFUSAL`], which names no field and no value).
//! 4. D4: only `eip155:*` networks are paid.
//! 5. R-VER ([`x402_version`]), the vetted entry, R-RESBIND
//!    ([`validated_resource`]), R-TOK ([`derive_client_token`]).
//! 6. The four resources must be set (the legacy `_require_resource` texts).
//!
//! Nothing here performs I/O except the injected `HostResolver`, and nothing
//! here reads a clock or a random source (R-TOK-NOFRESH).

use std::fmt;

use toolkit_aws::endpoint::{is_non_public, HostResolver};
use toolkit_core::{sha256_hex, Json};

use super::amount::{exceeds_cap, validated_amount_units, CapDecimal};
use super::pyval::{py_int, py_str, truthy};
use super::urlparse::{urlparse, ParsedUrl};
use crate::safe_text::safe_text;

mod approved;
mod fixed;

pub use approved::{Approved, PaymentIds};
pub use fixed::FixedResolver;

/// The refusal when no `accepts` entry satisfies the policy. It is the same
/// for every reason, so a publisher cannot probe the policy (T-POL-UNI).
pub const UNIFORM_REFUSAL: &str = "No payment option in this challenge satisfies the configured \
                                   policy (scheme, network, asset, recipient, and amount ceiling \
                                   are all enforced). Refusing to pay.";

const NON_EVM: &str = "Payment network family is not supported by this runtime.";
const NOT_OBJECT: &str = "Payment challenge is not a JSON object.";
const NO_ENTRIES: &str = "Payment challenge has no accepts entries.";
const RESOURCE_MISMATCH: &str = "Challenge resource.url does not match the requested URL.";
const NO_CAP: &str = "Payment policy has no per-payment ceiling ('max_per_payment_usd'). \
                      Refusing to pay: the session budget is cumulative and does not bound a \
                      single transaction.";
const ASSETS_SHAPE: &str = "Payment policy 'allowed_assets' must map networks to lists of strings.";

/// Fields an `accepts` entry needs before it can be eligible.
const REQUIRED_FIELDS: [&str; 4] = ["scheme", "network", "asset", "payTo"];
/// D11: the largest `extra` object, serialized compactly.
const MAX_EXTRA_BYTES: usize = 4096;
/// D11: the `maxTimeoutSeconds` range.
const TIMEOUT_RANGE: std::ops::RangeInclusive<i64> = 1..=3600;

/// A refusal of the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// A refusal with its fixed text (caller values in it are escaped).
    Refused(String),
    /// Python raised `ValueError` (a URL, a port or `int()`); the flow
    /// reports the generic class-name text (R-ERR).
    ValueError,
}

impl PolicyError {
    /// The model-facing reason.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Refused(text) => text.clone(),
            Self::ValueError => "ValueError during payment flow.".to_owned(),
        }
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason())
    }
}

impl std::error::Error for PolicyError {}

fn refused(text: impl Into<String>) -> PolicyError {
    PolicyError::Refused(text.into())
}

/// The resolved resource identifiers (config first, then environment; S2
/// resolves them). An empty value counts as missing.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Resources {
    /// `payment_manager_arn` / `PAYMENT_MANAGER_ARN`.
    pub payment_manager_arn: Option<String>,
    /// `payment_instrument_id` / `PAYMENT_INSTRUMENT_ID`.
    pub payment_instrument_id: Option<String>,
    /// `payment_session_id` / `PAYMENT_SESSION_ID`: a spending credential.
    pub payment_session_id: Option<String>,
    /// `user_id` / `PAYMENT_USER_ID`.
    pub user_id: Option<String>,
}

impl fmt::Debug for Resources {
    /// Presence only: the identifiers stay out of logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set = |value: Option<&str>| value.is_some_and(|v| !v.is_empty());
        f.debug_struct("Resources")
            .field(
                "payment_manager_arn",
                &set(self.payment_manager_arn.as_deref()),
            )
            .field(
                "payment_instrument_id",
                &set(self.payment_instrument_id.as_deref()),
            )
            .field(
                "payment_session_id",
                &set(self.payment_session_id.as_deref()),
            )
            .field("user_id", &set(self.user_id.as_deref()))
            .finish()
    }
}

/// One payment decision's inputs.
#[derive(Clone, Copy)]
pub struct PaymentRequest<'a> {
    /// The URL the caller asked to fetch.
    pub url: &'a str,
    /// The merchant's challenge (R-CH output).
    pub challenge: &'a Json,
    /// The operator's policy object.
    pub policy: &'a Json,
    /// The resolved resources.
    pub resources: &'a Resources,
    /// The caller's purchase discriminator (`--purchase-id`).
    pub purchase_id: Option<&'a str>,
}

/// The full gate: every check, then the one [`Approved`].
///
/// # Errors
///
/// The first refusal, in the legacy order (module documentation).
pub fn authorize(
    request: &PaymentRequest<'_>,
    resolver: &dyn HostResolver,
) -> Result<Approved, PolicyError> {
    let origin = assert_public_https_url(request.url, resolver)?;
    check_allowed_origins(request.policy, &origin)?;
    if !matches!(request.challenge, Json::Object(_)) {
        return Err(refused(NOT_OBJECT));
    }
    let entry = select_accept_entry(request.challenge, request.policy)?;
    let network = entry.get("network").and_then(py_str).unwrap_or_default();
    if !network.starts_with("eip155:") {
        return Err(refused(NON_EVM));
    }
    let amount = validated_amount_units(entry).map_err(|error| refused(error.message()))?;
    let version = x402_version(request.challenge)?;
    let vetted = vetted_entry(entry, &amount, version);
    let resource = validated_resource(request.challenge, request.url)?;
    let session = request
        .resources
        .payment_session_id
        .as_deref()
        .unwrap_or("");
    let token = derive_client_token(session, request.url, &vetted, request.purchase_id)?;
    let ids = required_ids(request.resources)?;
    Ok(Approved::new(vetted, amount, version, resource, token, ids))
}

/// R-ORIGIN: require https, a host, no userinfo and only public addresses;
/// return the canonical origin. An address literal is judged without a
/// lookup, as `getaddrinfo` returns it unchanged.
///
/// # Errors
///
/// The legacy texts, or [`PolicyError::ValueError`] where `urlparse` or the
/// port raises.
pub fn assert_public_https_url(
    url: &str,
    resolver: &dyn HostResolver,
) -> Result<String, PolicyError> {
    let parsed = urlparse(url).map_err(|_| PolicyError::ValueError)?;
    if parsed.scheme() != "https" {
        return Err(refused("Only https:// payment URLs are allowed."));
    }
    let Some(host) = parsed.hostname() else {
        return Err(refused("Payment URL has no host."));
    };
    if parsed.has_credentials() {
        return Err(refused(
            "Payment URL must not contain embedded credentials.",
        ));
    }
    parsed.port().map_err(|_| PolicyError::ValueError)?;
    let addresses = match parsed.address_literal() {
        Some(address) => vec![address],
        None => resolver
            .resolve(&host)
            .map_err(|_| refused("Payment URL hostname does not resolve."))?,
    };
    if addresses.is_empty() {
        return Err(refused("Payment URL hostname does not resolve."));
    }
    if addresses.into_iter().any(is_non_public) {
        return Err(refused("Payment URL resolves to a non-public address."));
    }
    parsed
        .canonical_origin()
        .map_err(|_| PolicyError::ValueError)
}

/// The optional origin pin: when `allowed_origins` is a non-empty list, the
/// lower-cased origin must equal one entry, lower-cased with trailing `/`
/// removed.
///
/// # Errors
///
/// A non-list value, or an origin outside the list (escaped in the text).
pub fn check_allowed_origins(policy: &Json, origin: &str) -> Result<(), PolicyError> {
    let allowed = policy_list(policy, "allowed_origins")?;
    if allowed.is_empty() {
        return Ok(());
    }
    let wanted = origin.to_lowercase();
    let pinned = allowed
        .iter()
        .flatten()
        .any(|entry| entry.to_lowercase().trim_end_matches('/') == wanted);
    if pinned {
        Ok(())
    } else {
        Err(refused(format!(
            "Origin {} is not in the configured allowed_origins.",
            safe_text(origin)
        )))
    }
}

/// A list-valued policy key, items through Python `str()` (`None` for an
/// array or object item, which can never match). Absent or `null` is empty.
fn policy_list(policy: &Json, key: &str) -> Result<Vec<Option<String>>, PolicyError> {
    match policy.get(key) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(items)) => Ok(items.iter().map(py_str).collect()),
        Some(_) => Err(refused(format!("Payment policy '{key}' must be a list."))),
    }
}

/// The per-payment cap under `max_per_payment_usd`, else the earlier
/// `max_amount_usd` (by presence).
fn per_payment_cap(policy: &Json) -> Result<CapDecimal, PolicyError> {
    let (key, value) = ["max_per_payment_usd", "max_amount_usd"]
        .into_iter()
        .find_map(|key| policy.get(key).map(|value| (key, value)))
        .ok_or_else(|| refused(NO_CAP))?;
    let cap = CapDecimal::from_json(value)
        .map_err(|_| refused(format!("Payment policy '{key}' is not a decimal number.")))?;
    if !cap.is_positive() {
        return Err(refused(format!(
            "Payment policy '{key}' must be greater than zero."
        )));
    }
    Ok(cap)
}

/// The validated policy an entry is checked against.
struct Rules<'a> {
    networks: Vec<Option<String>>,
    schemes: Vec<Option<String>>,
    allow_any_recipient: bool,
    recipients: Vec<Option<String>>,
    assets: Option<&'a Json>,
    cap: CapDecimal,
}

impl<'a> Rules<'a> {
    /// The policy shape checks, in the legacy order and texts.
    fn from_policy(policy: &'a Json) -> Result<Self, PolicyError> {
        let networks = policy_list(policy, "allowed_networks")?;
        let mut schemes = policy_list(policy, "allowed_schemes")?;
        if policy.get("allowed_schemes").is_none() {
            schemes = vec![Some("exact".to_owned())];
        }
        let allow_any_recipient = match policy.get("allow_any_recipient") {
            None => false,
            Some(Json::Bool(flag)) => *flag,
            Some(_) => {
                return Err(refused(
                    "Payment policy 'allow_any_recipient' must be a boolean.",
                ))
            }
        };
        if policy.get("allow_any_recipient").is_some() && policy.get("allowed_recipients").is_some()
        {
            return Err(refused(
                "Payment policy 'allow_any_recipient' and 'allowed_recipients' are mutually \
                 exclusive.",
            ));
        }
        let recipients = policy_list(policy, "allowed_recipients")?
            .into_iter()
            .map(|recipient| recipient.map(|r| r.to_lowercase()))
            .collect::<Vec<_>>();
        let assets = match policy.get("allowed_assets") {
            Some(value) if truthy(value) => {
                if !matches!(value, Json::Object(_)) {
                    return Err(refused(
                        "Payment policy 'allowed_assets' must be an object.",
                    ));
                }
                Some(value)
            }
            _ => None,
        };
        let cap = per_payment_cap(policy)?;
        if networks.is_empty() {
            return Err(refused("Payment policy allows no networks."));
        }
        if schemes.is_empty() {
            return Err(refused("Payment policy allows no schemes."));
        }
        if !allow_any_recipient && recipients.is_empty() {
            return Err(refused("Payment policy allows no recipients."));
        }
        Ok(Self {
            networks,
            schemes,
            allow_any_recipient,
            recipients,
            assets,
            cap,
        })
    }

    /// The lower-cased assets allowed on `network` (D3: only strings).
    fn assets_for(&self, network: &str) -> Result<Vec<String>, PolicyError> {
        let Some(map) = self.assets else {
            return Ok(Vec::new());
        };
        match map.get(network) {
            None => Ok(Vec::new()),
            Some(Json::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_lowercase)
                        .ok_or_else(|| refused(ASSETS_SHAPE))
                })
                .collect(),
            Some(_) => Err(refused(ASSETS_SHAPE)),
        }
    }

    /// Whether `entry` passes every check. Only a policy error (D3) is an
    /// `Err`; every challenge failure is a skip.
    fn eligible(&self, entry: &Json) -> Result<bool, PolicyError> {
        if !matches!(entry, Json::Object(_)) {
            return Ok(false);
        }
        let missing = REQUIRED_FIELDS.iter().any(|field| match entry.get(field) {
            None | Some(Json::Null) => true,
            Some(Json::String(text)) => text.is_empty(),
            Some(_) => false,
        });
        if missing {
            return Ok(false);
        }
        let field = |key: &str| entry.get(key).and_then(py_str);
        let Some(scheme) = field("scheme") else {
            return Ok(false);
        };
        if !contains(&self.schemes, &scheme) {
            return Ok(false);
        }
        let Some(network) = field("network") else {
            return Ok(false);
        };
        if !contains(&self.networks, &network) {
            return Ok(false);
        }
        let Some(asset) = field("asset") else {
            return Ok(false);
        };
        if !self.assets_for(&network)?.contains(&asset.to_lowercase()) {
            return Ok(false);
        }
        let Some(pay_to) = field("payTo") else {
            return Ok(false);
        };
        if !self.allow_any_recipient && !contains(&self.recipients, &pay_to.to_lowercase()) {
            return Ok(false);
        }
        if !bounded_members(entry) {
            return Ok(false);
        }
        let Ok(units) = validated_amount_units(entry) else {
            return Ok(false);
        };
        Ok(!exceeds_cap(&units, &self.cap))
    }
}

fn contains(list: &[Option<String>], value: &str) -> bool {
    list.iter().flatten().any(|item| item == value)
}

/// D11: `maxTimeoutSeconds`, when present, is an integer in 1..=3600, and
/// `extra`, when present, is an object of at most 4096 bytes.
fn bounded_members(entry: &Json) -> bool {
    if let Some(timeout) = entry.get("maxTimeoutSeconds") {
        if !matches!(timeout, Json::Int(seconds) if TIMEOUT_RANGE.contains(seconds)) {
            return false;
        }
    }
    if let Some(extra) = entry.get("extra") {
        let small =
            matches!(extra, Json::Object(_)) && extra.to_compact_string().len() <= MAX_EXTRA_BYTES;
        if !small {
            return false;
        }
    }
    true
}

/// R-POL: the first `accepts` entry that satisfies the policy.
///
/// # Errors
///
/// A policy-shape text, "no accepts entries", or [`UNIFORM_REFUSAL`].
pub fn select_accept_entry<'c>(
    challenge: &'c Json,
    policy: &Json,
) -> Result<&'c Json, PolicyError> {
    let accepts = match challenge.get("accepts") {
        Some(Json::Array(items)) if !items.is_empty() => items,
        _ => return Err(refused(NO_ENTRIES)),
    };
    let rules = Rules::from_policy(policy)?;
    for entry in accepts {
        if rules.eligible(entry)? {
            return Ok(entry);
        }
    }
    Err(refused(UNIFORM_REFUSAL))
}

/// R-VER: `int(challenge.x402Version or challenge.version or 1)`.
///
/// # Errors
///
/// [`PolicyError::ValueError`] where Python's `int()` raises, or outside
/// `i64` (D7).
pub fn x402_version(challenge: &Json) -> Result<i64, PolicyError> {
    let chosen = ["x402Version", "version"]
        .into_iter()
        .filter_map(|key| challenge.get(key))
        .find(|value| truthy(value));
    match chosen {
        None => Ok(1),
        Some(value) => py_int(value).map_err(|_| PolicyError::ValueError),
    }
}

/// The selected entry in its key order, the aliases removed and the
/// canonical amount appended under the version's key.
fn vetted_entry(entry: &Json, amount: &str, version: i64) -> Json {
    let Json::Object(members) = entry else {
        return entry.clone();
    };
    let mut members: Vec<(String, Json)> = members
        .iter()
        .filter(|(key, _)| key != "amount" && key != "maxAmountRequired")
        .cloned()
        .collect();
    let key = if version == 1 {
        "maxAmountRequired"
    } else {
        "amount"
    };
    members.push((key.to_owned(), Json::string(amount)));
    Json::Object(members)
}

/// R-RESBIND: when `challenge.resource` is an object with a string `url`,
/// its `scheme://netloc+path` (trailing `/` stripped) must equal the
/// requested URL's; the sanitized `{"url": ...}` is returned. Any other
/// shape is no resource.
///
/// # Errors
///
/// The mismatch text, or [`PolicyError::ValueError`] where `urlparse`
/// raises.
pub fn validated_resource(challenge: &Json, url: &str) -> Result<Option<Json>, PolicyError> {
    let Some(resource @ Json::Object(_)) = challenge.get("resource") else {
        return Ok(None);
    };
    let Some(Json::String(resource_url)) = resource.get("url") else {
        return Ok(None);
    };
    let requested = urlparse(url).map_err(|_| PolicyError::ValueError)?;
    let challenged = urlparse(resource_url).map_err(|_| PolicyError::ValueError)?;
    if requested.binding_base() != challenged.binding_base() {
        return Err(refused(RESOURCE_MISMATCH));
    }
    Ok(Some(Json::object(vec![(
        "url",
        Json::string(resource_url.clone()),
    )])))
}

/// `str(accept.get(key, ""))` for the token material.
fn material_field(accept: &Json, key: &str) -> Result<String, PolicyError> {
    match accept.get(key) {
        None => Ok(String::new()),
        Some(value) => py_str(value).ok_or_else(|| refused(UNIFORM_REFUSAL)),
    }
}

/// R-TOK: the idempotency token, byte-identical to the legacy
/// `derive_client_token`:
/// `sha256_hex(session \x1f origin \x1f path-or-"/" \x1f network \x1f
/// lower(asset) \x1f lower(payTo) \x1f canonical amount \x1f purchase_id)`.
/// No time, randomness or attempt counter enters it (R-TOK-NOFRESH).
///
/// # Errors
///
/// An amount refusal, or [`PolicyError::ValueError`] where `urlparse` or the
/// port raises.
pub fn derive_client_token(
    session: &str,
    url: &str,
    accept: &Json,
    purchase_id: Option<&str>,
) -> Result<String, PolicyError> {
    let parsed: ParsedUrl = urlparse(url).map_err(|_| PolicyError::ValueError)?;
    let origin = parsed
        .canonical_origin()
        .map_err(|_| PolicyError::ValueError)?;
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let network = material_field(accept, "network")?;
    let asset = material_field(accept, "asset")?.to_lowercase();
    let pay_to = material_field(accept, "payTo")?.to_lowercase();
    let amount = validated_amount_units(accept).map_err(|error| refused(error.message()))?;
    let material = [
        session,
        &origin,
        path,
        &network,
        &asset,
        &pay_to,
        &amount,
        purchase_id.unwrap_or(""),
    ]
    .join("\u{1f}");
    Ok(sha256_hex(material.as_bytes()))
}

/// The four resources, in the legacy `_require_resource` order and text.
fn required_ids(resources: &Resources) -> Result<PaymentIds, PolicyError> {
    let require = |value: Option<&str>, key: &str, env: &str| {
        value
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                refused(format!(
                    "Missing payment resource '{key}'. Add it to the config \
                     (agents_pay_admin.py init-config) or set {env}. Provision resources \
                     with the trusted setup path first."
                ))
            })
    };
    let manager_arn = require(
        resources.payment_manager_arn.as_deref(),
        "payment_manager_arn",
        "PAYMENT_MANAGER_ARN",
    )?;
    let instrument_id = require(
        resources.payment_instrument_id.as_deref(),
        "payment_instrument_id",
        "PAYMENT_INSTRUMENT_ID",
    )?;
    let session_id = require(
        resources.payment_session_id.as_deref(),
        "payment_session_id",
        "PAYMENT_SESSION_ID",
    )?;
    let user_id = require(resources.user_id.as_deref(), "user_id", "PAYMENT_USER_ID")?;
    Ok(PaymentIds {
        manager_arn,
        instrument_id,
        session_id,
        user_id,
    })
}
