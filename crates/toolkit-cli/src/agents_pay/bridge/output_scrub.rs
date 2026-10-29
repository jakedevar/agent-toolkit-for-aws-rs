//! The pure final model-output scrub (`OpenClaw` R-OC-FINAL, #5160; design
//! section 5.1 and D-OC19).
//!
//! Input: a `sanitize_output` request B1's [`super::protocol`] accepted, so
//! the candidate is one text item whose parsed text equals the object
//! `details`, and the registry is a list of strings. The matcher is the
//! shared `safe_text` one ([`FinalSecrets`]), with its bounded decoding and
//! work budget. Nothing here reads a file, a variable, a clock or a socket.
//!
//! - No hit anywhere: the candidate is returned unchanged, text bytes
//!   included.
//! - Hits only in the opt-in body (`details.body`, a top-level string):
//!   every matching span becomes `[REDACTED]`, `redacted: true` is
//!   appended, and the text becomes the compact rendering of the same
//!   details through the shared emitter.
//! - A hit anywhere else (a key, any other member, a nested `body`, a text
//!   hit the body does not explain), a candidate that already says
//!   `redacted` beside a body hit, a hit left after rendering, or an
//!   exhausted work budget: the whole result is the fixed refusal, in both
//!   channels. There is no proof exemption.
//!
//! The complete encoded frame is checked again before it is returned; a
//! frame over the cap is the fixed `AgentCore` failure (R-BR-OUT-CAP).

use toolkit_core::Json;

use super::protocol::BridgeError;
use super::{render, Frame};
use crate::aurora::pyfmt::dumps_compact;
use crate::safe_text::{FinalSecrets, FINAL_MATCH_WORK};

/// The fixed generic refusal reason of a paid-tool result.
pub const FAILED_SAFELY: &str =
    "Payment flow failed safely without exposing provider or proof details";

/// Sanitize one validated `sanitize_output` request into a complete frame.
#[must_use]
pub fn sanitize_frame(request: &Json) -> Frame {
    sanitize_frame_within(request, FINAL_MATCH_WORK)
}

/// [`sanitize_frame`] with an explicit matcher work budget: the default is
/// [`FINAL_MATCH_WORK`]; a smaller one exercises the fail-closed path.
#[must_use]
pub fn sanitize_frame_within(request: &Json, budget: usize) -> Frame {
    let secrets: Vec<&str> = match request.get("secrets") {
        Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
        _ => return render::error_frame(BridgeError::Input),
    };
    // A registry entry under 8 bytes cannot be matched safely: malformed.
    let Some(matcher) = FinalSecrets::new(&secrets) else {
        return render::error_frame(BridgeError::Input);
    };
    let Some(candidate) = request.get("candidate") else {
        return render::error_frame(BridgeError::Input);
    };
    let mut budget = budget;
    let result = sanitized(candidate, &matcher, &mut budget).unwrap_or_else(refusal);
    let frame = render::frame(&envelope(result));
    if frame.1 != 0 {
        return frame;
    }
    let clean = std::str::from_utf8(&frame.0)
        .ok()
        .and_then(|text| matcher.found_in(text, &mut budget))
        == Some(false);
    if clean {
        frame
    } else {
        refusal_frame(&matcher)
    }
}

fn envelope(result: Json) -> Json {
    Json::object(vec![("ok", Json::Bool(true)), ("result", result)])
}

/// The fixed refusal wrapper, in both channels.
fn refusal() -> Json {
    let details = Json::object(vec![
        ("paid", Json::Bool(false)),
        ("reason", Json::string(FAILED_SAFELY)),
        ("refused", Json::Bool(true)),
    ]);
    Json::object(vec![
        (
            "content",
            Json::Array(vec![Json::object(vec![
                ("type", Json::string("text")),
                ("text", Json::string(dumps_compact(&details))),
            ])]),
        ),
        ("details", details),
    ])
}

/// The refusal frame, itself checked (with a fresh budget: it holds only
/// fixed text); a registry that matches the fixed text is a bridge failure.
fn refusal_frame(matcher: &FinalSecrets) -> Frame {
    let frame = render::frame(&envelope(refusal()));
    let mut budget = FINAL_MATCH_WORK;
    let clean = std::str::from_utf8(&frame.0)
        .ok()
        .and_then(|text| matcher.found_in(text, &mut budget))
        == Some(false);
    if clean && frame.1 == 0 {
        frame
    } else {
        render::error_frame(BridgeError::AgentCore)
    }
}

/// Whether any key or string of `value` holds a secret; `None` past the
/// budget.
fn tree_hit(value: &Json, matcher: &FinalSecrets, budget: &mut usize) -> Option<bool> {
    match value {
        Json::String(text) => matcher.found_in(text, budget),
        Json::Array(items) => {
            for item in items {
                if tree_hit(item, matcher, budget)? {
                    return Some(true);
                }
            }
            Some(false)
        }
        Json::Object(members) => {
            for (key, member) in members {
                if matcher.found_in(key, budget)? || tree_hit(member, matcher, budget)? {
                    return Some(true);
                }
            }
            Some(false)
        }
        _ => Some(false),
    }
}

/// `value` with its member `key` set to `member`.
fn replaced(value: &Json, key: &str, member: &Json) -> Json {
    match value {
        Json::Object(members) => Json::Object(
            members
                .iter()
                .map(|(name, old)| {
                    let new = if name == key {
                        member.clone()
                    } else {
                        old.clone()
                    };
                    (name.clone(), new)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The sanitized result, or `None` for the fixed refusal.
fn sanitized(candidate: &Json, matcher: &FinalSecrets, budget: &mut usize) -> Option<Json> {
    let Some(Json::Object(members)) = candidate.get("details") else {
        return None;
    };
    let Some(Json::Array(content)) = candidate.get("content") else {
        return None;
    };
    let item = content.first()?;
    let text = item.get("text").and_then(Json::as_str)?;
    let mut body = None;
    for (key, value) in members {
        if matcher.found_in(key, budget)? {
            return None;
        }
        match (key.as_str(), value) {
            ("body", Json::String(body_text)) => {
                if matcher.found_in(body_text, budget)? {
                    body = Some(body_text);
                }
            }
            _ => {
                if tree_hit(value, matcher, budget)? {
                    return None;
                }
            }
        }
    }
    let text_hit = matcher.found_in(text, budget)?;
    let Some(body) = body else {
        // A text hit no details value explains is outside the body.
        return (!text_hit).then(|| candidate.clone());
    };
    // Ambiguous: the candidate already carries the flag the scrub adds.
    if members.iter().any(|(key, _)| key == "redacted") {
        return None;
    }
    let (redacted, _) = matcher.redact(body, budget)?;
    let mut new_members: Vec<(String, Json)> = members
        .iter()
        .map(|(key, value)| {
            if key == "body" {
                (key.clone(), Json::string(redacted.clone()))
            } else {
                (key.clone(), value.clone())
            }
        })
        .collect();
    new_members.push(("redacted".to_owned(), Json::Bool(true)));
    let details = Json::Object(new_members);
    let new_text = dumps_compact(&details);
    if tree_hit(&details, matcher, budget)? || matcher.found_in(&new_text, budget)? {
        return None;
    }
    let item = replaced(item, "text", &Json::string(new_text));
    Some(replaced(
        &replaced(candidate, "content", &Json::Array(vec![item])),
        "details",
        &details,
    ))
}
