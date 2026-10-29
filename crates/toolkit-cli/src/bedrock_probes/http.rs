//! The HTTP seam of `toolkit bedrock-probes source-baseline`.
//!
//! The Python script sent each golden prompt to the customer's source provider
//! with `urllib` (60 s timeout). Here a request is a URL, its headers (the API
//! key travels only there) and a JSON body handed to an [`HttpClient`]. Tests
//! inject scripted clients; the command wires the [`RecordedHttp`] client
//! (`--recorded FILE`) or [`NoLiveHttp`], which sends nothing and fails every
//! request. A live client is a follow-up (see `docs/migration/bedrock-probes.md`).
//!
//! The recorded file is one JSON object:
//!
//! ```json
//! {
//!   "calls": [
//!     {"url": "https://api.openai.com/v1/chat/completions",
//!      "request": {"model": "m", "messages": []},
//!      "response": {"choices": [{"message": {"content": "42.00"}}]}},
//!     {"url": "https://api.openai.com/v1/chat/completions",
//!      "error": {"status": 400, "reason": "Bad Request",
//!                "body": "{\"error\": {\"message\": \"Unsupported parameter\"}}"}},
//!     {"url": "https://api.anthropic.com/v1/messages",
//!      "error": {"kind": "transport", "type": "URLError", "message": "timed out"}}
//!   ]
//! }
//! ```
//!
//! A call matches the first entry with the same URL and request (objects
//! compare without regard to key order; an entry without `request` matches any
//! body). `response` is the provider's JSON reply. `error.kind` is `http`
//! (default: `status` is required, `reason` and `body` are optional; `body` is
//! the raw error text or any JSON value) or `transport` (`type` defaults to
//! `URLError`, `message` to the empty text). Every optional member is absent or
//! of its type; a present member of another type, `null` included, rejects the
//! recording. Headers are never recorded: a replay cannot show where a key
//! went, which is why the request builders are tested directly.

use toolkit_aws::recorded::{
    optional_list, optional_member, optional_text, step_fields, KeyOrder, MemberError,
    RecordedOutcome, StepShapeError,
};
use toolkit_core::Json;

use crate::startup::artifacts::pyjson::dumps;

/// Why a request produced no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpFailure {
    /// The build has no live client: nothing was sent.
    NoLive,
    /// The recording has no entry for this URL and body.
    Unrecorded,
    /// An HTTP error status with its reason phrase and (at most the first
    /// part of) the raw body.
    Status {
        code: u16,
        reason: String,
        body: Vec<u8>,
    },
    /// A transport failure: the exception class and message.
    Transport { kind: String, message: String },
}

/// Sends one JSON request and returns the provider's JSON reply.
pub(crate) trait HttpClient {
    /// # Errors
    ///
    /// The failure of the request.
    fn send(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        body: &Json,
    ) -> Result<Json, HttpFailure>;
}

/// The client of a run without `--recorded`: nothing is sent.
pub(crate) struct NoLiveHttp;

impl HttpClient for NoLiveHttp {
    fn send(
        &mut self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &Json,
    ) -> Result<Json, HttpFailure> {
        Err(HttpFailure::NoLive)
    }
}

struct Entry {
    url: String,
    /// The expected request body; `None` accepts any body.
    request: Option<Json>,
    /// The reply or the failure to replay.
    outcome: Result<Json, HttpFailure>,
}

/// Replays the entries of a recorded-response file.
pub(crate) struct RecordedHttp {
    entries: Vec<Entry>,
}

/// `"<key>" must be <expected> when present (it is <found>)`.
fn quoted(error: &MemberError) -> String {
    format!(
        "\"{}\" must be {} when present (it is {})",
        error.key(),
        error.expected(),
        error.found()
    )
}

impl RecordedHttp {
    /// Builds a client from a parsed recorded-response document.
    ///
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        let calls = optional_list(document, "calls").map_err(|error| quoted(&error))?;
        let mut entries = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            entries.push(parse_entry(index, call)?);
        }
        Ok(Self { entries })
    }
}

fn parse_entry(index: usize, call: &Json) -> Result<Entry, String> {
    let bad = |what: &str| format!("recorded call {index}: {what}");
    let url = call
        .get("url")
        .and_then(Json::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| bad("\"url\" must be a non-empty string"))?
        .to_string();
    let fields = step_fields(call).map_err(|error| match error {
        StepShapeError::Request(member) => bad(&quoted(&member)),
        StepShapeError::Outcome | StepShapeError::ErrorNotObject => {
            bad("exactly one of \"response\" (an object) and \"error\" (an object) is required")
        }
    })?;
    let request = fields.request.cloned();
    match fields.outcome {
        RecordedOutcome::Response(response) => Ok(Entry {
            url,
            request,
            outcome: Ok(response.clone()),
        }),
        RecordedOutcome::Error(error) => {
            let in_error = |member: MemberError| bad(&format!("error {}", quoted(&member)));
            let kind = optional_text(error, "kind").map_err(in_error)?;
            let reason = optional_text(error, "reason").map_err(in_error)?;
            let kind_type = optional_text(error, "type").map_err(in_error)?;
            let message = optional_text(error, "message").map_err(in_error)?;
            let status = optional_member(error, "status", "an integer", |member| match member {
                Json::Int(status) => u16::try_from(*status)
                    .ok()
                    .filter(|s| (100..=599).contains(s)),
                _ => None,
            })
            .map_err(in_error)?;
            // The body is the raw error text, or any JSON value (kept as its
            // compact text, as the provider would have sent it).
            let body = optional_member(error, "body", "text or JSON", |member| match member {
                Json::String(text) => Some(text.clone()),
                other => Some(dumps(other, None, false)),
            })
            .map_err(in_error)?;
            let failure = match kind.unwrap_or("http") {
                "http" => HttpFailure::Status {
                    code: status.ok_or_else(|| {
                        bad("an http \"error\" needs an integer \"status\" (100-599)")
                    })?,
                    reason: reason.unwrap_or_default().to_string(),
                    body: body.unwrap_or_default().into_bytes(),
                },
                "transport" => HttpFailure::Transport {
                    kind: kind_type.unwrap_or("URLError").to_string(),
                    message: message.unwrap_or_default().to_string(),
                },
                _ => return Err(bad("\"error.kind\" must be http or transport")),
            };
            Ok(Entry {
                url,
                request,
                outcome: Err(failure),
            })
        }
    }
}

impl HttpClient for RecordedHttp {
    fn send(
        &mut self,
        url: &str,
        _headers: &[(String, String)],
        body: &Json,
    ) -> Result<Json, HttpFailure> {
        let entry = self
            .entries
            .iter()
            .find(|entry| {
                entry.url == url
                    && entry
                        .request
                        .as_ref()
                        .is_none_or(|expected| KeyOrder::Ignored.same(expected, body))
            })
            .ok_or(HttpFailure::Unrecorded)?;
        entry.outcome.clone()
    }
}
