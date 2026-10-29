//! GraphSON v2 for the Gremlin plane (R-DG-05, R-DG-07; S3, #5515): the
//! request message the drivers send and a small typed-value decoder for the
//! response batches. Everything outside the supported set fails closed.
//!
//! Supported response values: JSON null, booleans, strings and numbers; plain
//! JSON arrays (lists) and objects (maps, GraphSON v2's form); the typed
//! wrappers `g:Int32`, `g:Int64`, `g:Date`, `g:Timestamp` (integers),
//! `g:Float`, `g:Double` (finite numbers), `g:UUID` (string), `g:List` and
//! `g:Set` (arrays); and a top-level `g:Traverser`, expanded `bulk` times.
//! Every other type (elements, paths, `g:Map`, GraphBinary, non-finite
//! doubles) is an error, and no type name or value is echoed.
//!
//! Bounds, each checked before the allocation it guards:
//! - [`MAX_DEPTH`] containers enclose a value at most (a typed scalar wrapper
//!   is not a container, a typed list is one). The raw text is scanned for
//!   JSON nesting first, so the tree is never built past the limit.
//! - [`MAX_RESULTS`] decoded results per operation, counting every bulk copy;
//!   a bulk is refused before any copy is made, as is a bulk whose copies
//!   would hold more than [`MAX_EXPANDED_BYTES`].

use toolkit_core::json::{parse_json_with_limits, JsonLimits};
use toolkit_core::Json;

use crate::error::{AwsError, ServiceError};

/// The most containers that may enclose a decoded value.
pub const MAX_DEPTH: usize = 64;
/// The most decoded results of one operation, bulk copies included.
pub const MAX_RESULTS: usize = 100_000;
/// The most compact-JSON bytes the bulk copies of one operation may hold.
pub const MAX_EXPANDED_BYTES: usize = 16 * 1024 * 1024;
/// The serializer the request names and the response must use.
pub const MIME: &str = "application/vnd.gremlin-v2.0+json";

/// JSON nesting of the deepest message the decoder can accept: the envelope
/// (message, `result`, `data`), a traverser wrapper and its value object,
/// each of [`MAX_DEPTH`] containers as a typed wrapper plus its array, and an
/// innermost typed scalar wrapper.
const MAX_JSON_NESTING: usize = 3 + 2 + 2 * MAX_DEPTH + 1;

fn malformed(reason: &str) -> AwsError {
    AwsError::invalid_response(format!("the Gremlin response {reason}"))
}

fn too_deep() -> AwsError {
    malformed("nests values deeper than the limit of 64")
}

fn too_many() -> AwsError {
    malformed("holds more than the limit of 100000 results")
}

/// The outcome of one response message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// 206: more batches follow.
    Partial,
    /// 200 or 204: the operation is complete.
    Complete,
}

/// The running result of one operation across its batches.
#[derive(Debug, Default)]
pub(crate) struct Results {
    pub(crate) items: Vec<Json>,
    expanded_bytes: usize,
}

/// Gremlin bindings in GraphSON v2: integers as `g:Int32` when they fit, else
/// `g:Int64`, other numbers as `g:Double`, containers recursively.
fn encode_binding(value: &Json) -> Json {
    match value {
        Json::Int(n) => Json::object(vec![
            (
                "@type",
                Json::string(if i32::try_from(*n).is_ok() {
                    "g:Int32"
                } else {
                    "g:Int64"
                }),
            ),
            ("@value", Json::Int(*n)),
        ]),
        Json::Float(n) => Json::object(vec![
            ("@type", Json::string("g:Double")),
            ("@value", Json::Float(*n)),
        ]),
        Json::Array(items) => Json::Array(items.iter().map(encode_binding).collect()),
        Json::Object(pairs) => Json::Object(
            pairs
                .iter()
                .map(|(key, item)| (key.clone(), encode_binding(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The binary frame payload of one `eval` request, as the GraphSON v2 drivers
/// send it: a length-prefixed MIME type, then the JSON message. Traversal
/// source `g`; the statement and bindings are sent as given, never
/// interpolated.
pub(crate) fn request_payload(request_id: &str, gremlin: &str, bindings: Option<&Json>) -> Vec<u8> {
    let mut args = vec![
        ("gremlin", Json::string(gremlin)),
        ("language", Json::string("gremlin-groovy")),
        ("aliases", Json::object(vec![("g", Json::string("g"))])),
    ];
    if let Some(bindings) = bindings {
        args.push(("bindings", encode_binding(bindings)));
    }
    let message = Json::object(vec![
        ("requestId", Json::string(request_id)),
        ("op", Json::string("eval")),
        ("processor", Json::string("")),
        ("args", Json::object(args)),
    ]);
    let mut payload = Vec::with_capacity(1 + MIME.len());
    payload.push(u8::try_from(MIME.len()).unwrap_or(u8::MAX));
    payload.extend_from_slice(MIME.as_bytes());
    payload.extend_from_slice(message.to_compact_string().as_bytes());
    payload
}

/// JSON nesting of `text`, stopping as soon as it passes `limit` (so a deep
/// message is refused before any tree is built).
fn nesting_within(text: &[u8], limit: usize) -> bool {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in text {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > limit {
                    return false;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

fn member<'a>(pairs: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    pairs
        .iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

/// The `@type`/`@value` of a typed wrapper; `None` for a plain map. An object
/// naming `@type` with any other shape is malformed.
fn typed(pairs: &[(String, Json)]) -> Result<Option<(&str, &Json)>, AwsError> {
    let Some(kind) = member(pairs, "@type") else {
        return Ok(None);
    };
    let (Some(kind), Some(value), 2) = (kind.as_str(), member(pairs, "@value"), pairs.len()) else {
        return Err(malformed("holds a malformed typed value"));
    };
    Ok(Some((kind, value)))
}

fn unsupported() -> AwsError {
    malformed("holds an unsupported GraphSON type")
}

/// Decode a value enclosed by `depth` containers.
fn decode(value: &Json, depth: usize) -> Result<Json, AwsError> {
    let inner = depth + 1;
    match value {
        Json::Array(items) => {
            if inner > MAX_DEPTH {
                return Err(too_deep());
            }
            items
                .iter()
                .map(|item| decode(item, inner))
                .collect::<Result<_, _>>()
                .map(Json::Array)
        }
        Json::Object(pairs) => match typed(pairs)? {
            None => {
                if inner > MAX_DEPTH {
                    return Err(too_deep());
                }
                pairs
                    .iter()
                    .map(|(key, item)| Ok((key.clone(), decode(item, inner)?)))
                    .collect::<Result<_, AwsError>>()
                    .map(Json::Object)
            }
            Some((kind, value)) => decode_typed(kind, value, depth),
        },
        scalar => Ok(scalar.clone()),
    }
}

fn decode_typed(kind: &str, value: &Json, depth: usize) -> Result<Json, AwsError> {
    match (kind, value) {
        ("g:Int32", Json::Int(n)) if i32::try_from(*n).is_ok() => Ok(Json::Int(*n)),
        ("g:Int64" | "g:Date" | "g:Timestamp", Json::Int(n)) => Ok(Json::Int(*n)),
        ("g:Float" | "g:Double", Json::Int(n)) =>
        {
            #[allow(clippy::cast_precision_loss)]
            Ok(Json::Float(*n as f64))
        }
        ("g:Float" | "g:Double", Json::Float(n)) if n.is_finite() => Ok(Json::Float(*n)),
        ("g:UUID", Json::String(text)) => Ok(Json::String(text.clone())),
        ("g:List" | "g:Set", Json::Array(_)) => decode(value, depth),
        _ => Err(unsupported()),
    }
}

/// A positive bulk count.
fn bulk(value: &Json) -> Result<usize, AwsError> {
    let count = match value {
        Json::Int(n) => *n,
        Json::Object(pairs) => match typed(pairs)? {
            Some(("g:Int64" | "g:Int32", Json::Int(n))) => *n,
            _ => return Err(malformed("holds a malformed traverser bulk")),
        },
        _ => return Err(malformed("holds a malformed traverser bulk")),
    };
    usize::try_from(count)
        .ok()
        .filter(|count| *count > 0)
        .ok_or_else(|| malformed("holds a malformed traverser bulk"))
}

impl Results {
    /// Room for `count` more results of `weight` bytes each, checked before
    /// any of them is allocated.
    fn admit(&self, count: usize, weight: usize) -> Result<(), AwsError> {
        if self
            .items
            .len()
            .checked_add(count)
            .is_none_or(|total| total > MAX_RESULTS)
        {
            return Err(too_many());
        }
        let bytes = count
            .checked_mul(weight.max(1))
            .and_then(|bytes| bytes.checked_add(self.expanded_bytes));
        if bytes.is_none_or(|bytes| bytes > MAX_EXPANDED_BYTES) {
            return Err(malformed(
                "expands to more than the limit of 16 MiB of results",
            ));
        }
        Ok(())
    }

    fn push(&mut self, item: &Json) -> Result<(), AwsError> {
        if let Json::Object(pairs) = item {
            if let Some(("g:Traverser", Json::Object(fields))) = typed(pairs)? {
                let (Some(count), Some(value), 2) = (
                    member(fields, "bulk"),
                    member(fields, "value"),
                    fields.len(),
                ) else {
                    return Err(malformed("holds a malformed traverser"));
                };
                let count = bulk(count)?;
                // The count is checked before the value is even decoded.
                self.admit(count, 1)?;
                let value = decode(value, 0)?;
                let weight = value.to_compact_string().len();
                self.admit(count, weight)?;
                self.items.reserve(count);
                for _ in 1..count {
                    self.items.push(value.clone());
                }
                self.items.push(value);
                self.expanded_bytes += count * weight.max(1);
                return Ok(());
            }
        }
        self.admit(1, 1)?;
        let value = decode(item, 0)?;
        let weight = value.to_compact_string().len();
        self.admit(1, weight)?;
        self.items.push(value);
        self.expanded_bytes += weight.max(1);
        Ok(())
    }

    /// Decode one response message for `request_id` and add its results.
    ///
    /// # Errors
    /// `InvalidResponse` for malformed JSON or GraphSON, a foreign request
    /// id, an unexpected status or a broken bound; `Service` (status only, no
    /// server text) for a Gremlin error status.
    pub(crate) fn add_message(
        &mut self,
        text: &[u8],
        request_id: &str,
    ) -> Result<Status, AwsError> {
        if !nesting_within(text, MAX_JSON_NESTING) {
            return Err(too_deep());
        }
        let text = std::str::from_utf8(text).map_err(|_| malformed("is not UTF-8 text"))?;
        let limits = JsonLimits {
            max_depth: MAX_JSON_NESTING,
            ..JsonLimits::default()
        };
        let message =
            parse_json_with_limits(text, &limits).map_err(|_| malformed("is not valid JSON"))?;
        // recorded-shape: a live Gremlin response envelope; every other shape is rejected
        let Json::Object(envelope) = &message else {
            return Err(malformed("is not a JSON object"));
        };
        if member(envelope, "requestId").and_then(Json::as_str) != Some(request_id) {
            return Err(malformed("names another request"));
        }
        let code = member(envelope, "status")
            .and_then(|status| status.get("code"))
            .and_then(|code| match code {
                Json::Int(n) => u16::try_from(*n).ok(),
                _ => None,
            })
            .ok_or_else(|| malformed("has no status code"))?;
        let data = member(envelope, "result").and_then(|result| result.get("data"));
        match code {
            200 | 206 => {
                // recorded-shape: a live Gremlin batch; a non-list result is rejected
                let Some(Json::Array(items)) = data else {
                    return Err(malformed("has no result list"));
                };
                for item in items {
                    self.push(item)?;
                }
                Ok(if code == 206 {
                    Status::Partial
                } else {
                    Status::Complete
                })
            }
            204 => match data {
                None | Some(Json::Null) => Ok(Status::Complete),
                Some(Json::Array(items)) if items.is_empty() => Ok(Status::Complete),
                Some(_) => Err(malformed("has results with status 204")),
            },
            // Gremlin error statuses (401 unauthorized, 407 authenticate,
            // 498/499 malformed or invalid request, 5xx server errors): the
            // status only; the server's message is never forwarded.
            401 | 407 | 498..=599 => Err(AwsError::Service(Box::new(ServiceError::new(
                "GET", "/gremlin", code, None, "", None,
            )))),
            _ => Err(malformed("has an unexpected status")),
        }
    }
}
