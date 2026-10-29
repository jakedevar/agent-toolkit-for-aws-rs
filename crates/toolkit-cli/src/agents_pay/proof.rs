//! R-PP-VERIFY, the bounded `validAfter` wait and the R-HDR proof header.
//!
//! [`verify`] is the only consumer of an [`Authorized`] (R-SPEND-ORDER rule
//! 5, #5332), and [`VerifiedProof::header`] is the only producer of a
//! [`ProofHeader`]: a proof whose terms differ from the approved ones can
//! never reach a merchant.
//!
//! ```compile_fail
//! use toolkit_cli::agents_pay::{agentcore::Authorized, proof::VerifiedProof};
//! // A replay header cannot be built from an unverified authorization.
//! fn forge(authorized: &Authorized) { let _ = VerifiedProof::header(authorized); }
//! ```
//!
//! ```compile_fail
//! use toolkit_cli::agents_pay::proof::VerifiedProof;
//! use toolkit_core::Json;
//! fn forge(payload: Json) -> VerifiedProof {
//!     VerifiedProof { payload, entry: Json::Null, resource: None, version: 2 }
//! }
//! ```

use std::time::Duration;

use toolkit_core::Json;

use super::agentcore::Authorized;
use super::deps::PayClock;
use super::net::ProofHeader;

/// The proof's terms differ from the approved ones (D12).
pub const MISMATCH: &str = "Signed payment does not match the approved terms.";
/// `validAfter` is more than 15 s away, or the proof expires before then.
pub const OUTSIDE_WINDOW: &str = "Signed payment is not valid within the bounded retry window.";

/// The longest `validAfter` wait (Q5).
const MAX_WAIT_SECONDS: i64 = 15;

/// Why a proof was dropped unsent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofError {
    /// [`MISMATCH`].
    Mismatch,
    /// [`OUTSIDE_WINDOW`].
    OutsideWindow,
}

impl ProofError {
    /// The model-facing text.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Mismatch => MISMATCH,
            Self::OutsideWindow => OUTSIDE_WINDOW,
        }
    }
}

/// A proof checked against the approved terms; the only source of a replay
/// header.
pub struct VerifiedProof {
    payload: Json,
    entry: Json,
    resource: Option<Json>,
    version: i64,
}

impl std::fmt::Debug for VerifiedProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerifiedProof([REDACTED])")
    }
}

/// An unsigned decimal integer from a JSON string of ASCII digits or a
/// non-negative JSON integer.
// recorded-shape: the signed authorization is checked field by field and refused otherwise.
fn unsigned(value: Option<&Json>) -> Option<i64> {
    match value? {
        Json::Int(n) if *n >= 0 => Some(*n),
        Json::String(text)
            if !text.is_empty() && text.len() <= 18 && text.bytes().all(|b| b.is_ascii_digit()) =>
        {
            text.parse().ok()
        }
        _ => None,
    }
}

/// The exact decimal text of an authorization amount.
fn amount_text(value: Option<&Json>) -> Option<String> {
    match value? {
        Json::Int(n) if *n >= 0 => Some(n.to_string()),
        Json::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// R-PP-VERIFY: `payload.authorization.to` equals the approved `payTo`
/// (ASCII case-insensitive), `value` equals the canonical amount, and
/// `validBefore` lies in the future on `clock`. A future `validAfter` is
/// waited for until one second after it, only when that is at most 15 s
/// away and the authorization is still valid then.
///
/// # Errors
/// [`ProofError`]; the proof is dropped unsent.
// recorded-shape: the signed authorization is checked field by field and refused otherwise.
pub fn verify(authorized: Authorized, clock: &dyn PayClock) -> Result<VerifiedProof, ProofError> {
    let parts = authorized.into_parts();
    let payload = parts.proof.payload();
    let Some(authorization @ Json::Object(_)) = payload.get("authorization") else {
        return Err(ProofError::Mismatch);
    };
    let to = authorization.get("to").and_then(Json::as_str);
    if !to.is_some_and(|to| to.eq_ignore_ascii_case(&parts.recipient)) {
        return Err(ProofError::Mismatch);
    }
    if amount_text(authorization.get("value")).as_deref() != Some(parts.amount.as_str()) {
        return Err(ProofError::Mismatch);
    }
    let Some(valid_before) = unsigned(authorization.get("validBefore")) else {
        return Err(ProofError::Mismatch);
    };
    let valid_after = match authorization.get("validAfter") {
        None => 0,
        some => unsigned(some).ok_or(ProofError::Mismatch)?,
    };
    let now = clock.unix_micros().div_euclid(1_000_000);
    if valid_before <= now {
        return Err(ProofError::Mismatch);
    }
    if valid_after > now {
        let wait = valid_after.saturating_add(1).saturating_sub(now);
        if wait > MAX_WAIT_SECONDS || valid_before <= now.saturating_add(wait) {
            return Err(ProofError::OutsideWindow);
        }
        clock.sleep(Duration::from_secs(wait.unsigned_abs()));
    }
    Ok(VerifiedProof {
        payload: payload.clone(),
        entry: parts.entry,
        resource: parts.resource,
        version: parts.version,
    })
}

impl VerifiedProof {
    /// R-HDR: version 1 sends `X-PAYMENT` with
    /// `{"x402Version":1,"scheme":"exact","network":…,"payload":…}`; any
    /// other version sends `PAYMENT-SIGNATURE` with
    /// `{"x402Version":2,"accepted":<vetted entry>,"payload":…,"resource":…}`
    /// (`resource` only when present). Both are standard padded base64 of
    /// compact JSON; the signed payload is copied as returned.
    #[must_use]
    pub fn header(&self) -> ProofHeader {
        let (name, document) = if self.version == 1 {
            let network = self.entry.get("network").cloned().unwrap_or(Json::Null);
            (
                "X-PAYMENT",
                Json::object(vec![
                    ("x402Version", Json::Int(1)),
                    ("scheme", Json::string("exact")),
                    ("network", network),
                    ("payload", self.payload.clone()),
                ]),
            )
        } else {
            let mut members = vec![
                ("x402Version", Json::Int(2)),
                ("accepted", self.entry.clone()),
                ("payload", self.payload.clone()),
            ];
            if let Some(resource) = &self.resource {
                members.push(("resource", resource.clone()));
            }
            ("PAYMENT-SIGNATURE", Json::object(members))
        };
        let value =
            toolkit_core::encoding::base64_standard(document.to_compact_string().as_bytes());
        ProofHeader::new(name.to_owned(), value)
    }

    /// The signed payload's `signature`, a live secret for R-SCRUB.
    #[must_use]
    pub(in crate::agents_pay) fn signature(&self) -> Option<&str> {
        self.payload.get("signature").and_then(Json::as_str)
    }
}
