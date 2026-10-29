//! x402 challenge extraction from a 402 response (R-CH, x402 S1 #5118).
//!
//! 1. Header first: `payment-required`, else `x-payment-required` (names are
//!    case-insensitive; repeated field lines are joined with `", "`, as
//!    httpx's `headers.get` joins them). An empty value is falsy, so the
//!    fallback header is read.
//! 2. The header as strict base64 (including D35), then UTF-8,
//!    then JSON; accepted when it is an object with a truthy `accepts`.
//! 3. Otherwise the raw header text as JSON, with the same rule.
//! 4. Otherwise the body, decoded as lossy UTF-8 (Q11), as JSON: a parse
//!    failure is "no parseable x402 challenge"; an object with a truthy
//!    `accepts`, or whose `challenge` object has one, is the challenge;
//!    anything else is "missing the required 'accepts' array".
//!
//! Header JSON is bounded to [`MAX_HEADER_JSON_BYTES`]; a longer candidate
//! counts as unparseable. The body is bounded by the merchant body cap
//! (S3). Duplicate object keys and `NaN`/`Infinity` literals never parse
//! (D1, through `toolkit_core::json::parse_json`).

use toolkit_core::encoding::base64_standard_decode;
use toolkit_core::{parse_json, Json};

use super::policy::PolicyError;
use super::pyval::truthy;

/// Largest decoded header JSON read as a challenge.
pub const MAX_HEADER_JSON_BYTES: usize = 64 * 1024;

const NOT_PARSEABLE: &str = "402 response contains no parseable x402 challenge.";
const NO_ACCEPTS: &str = "402 challenge is missing the required 'accepts' array.";

/// Every field line named `name` (ASCII case-insensitive), joined by `", "`;
/// `None` when there is none.
fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    let values: Vec<&str> = headers
        .iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// Whether `value` is an object with a truthy `accepts`.
// recorded-shape: merchant challenge is not an AWS response.
fn carries_accepts(value: &Json) -> bool {
    matches!(value, Json::Object(_)) && value.get("accepts").is_some_and(truthy)
}

/// `text` as a challenge, when it is bounded JSON carrying `accepts`.
fn header_candidate(text: &str) -> Option<Json> {
    if text.len() > MAX_HEADER_JSON_BYTES {
        return None;
    }
    parse_json(text).ok().filter(carries_accepts)
}

fn from_header(header: &str) -> Option<Json> {
    base64_standard_decode(header)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| header_candidate(&text))
        .or_else(|| header_candidate(header))
}

/// The x402 challenge of a 402 response.
///
/// # Errors
///
/// [`PolicyError::Refused`] with the legacy text when neither the headers
/// nor the body carry a challenge.
pub fn extract(headers: &[(String, String)], body: &[u8]) -> Result<Json, PolicyError> {
    let non_empty = |name: &str| header_value(headers, name).filter(|value| !value.is_empty());
    if let Some(header) = non_empty("payment-required").or_else(|| non_empty("x-payment-required"))
    {
        if let Some(challenge) = from_header(&header) {
            return Ok(challenge);
        }
    }
    let text = String::from_utf8_lossy(body);
    let parsed = parse_json(&text).map_err(|_| PolicyError::Refused(NOT_PARSEABLE.to_owned()))?;
    if carries_accepts(&parsed) {
        return Ok(parsed);
    }
    if let Some(inner) = parsed.get("challenge") {
        if carries_accepts(inner) {
            return Ok(inner.clone());
        }
    }
    Err(PolicyError::Refused(NO_ACCEPTS.to_owned()))
}
