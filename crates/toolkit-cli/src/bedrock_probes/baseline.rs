//! Source-model baseline runner (`source_baseline.py`).
//!
//! Re-runs each golden prompt against the customer's source model and records
//! one JSON row per prompt:
//! `{"id": ..., "source_response": "<text or empty>", "status": "live |
//! http_<code>: <reason> | error: <type>: <message>"}`.
//!
//! Partial resume: a prompt whose id already has a `live` row in the output is
//! skipped (calling the provider again would double-spend the user's budget);
//! failed rows are retried.
//!
//! Security: the API key is read from the env file into this module only and
//! sent only as an auth header to its own provider's official endpoint
//! (`api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`),
//! never to another host, never as a URL query parameter, never to stdout or
//! the output rows. Failure text is redacted against the known key values
//! first, then truncated and escaped.
//!
//! Nothing here sends anything: requests go through an [`HttpClient`]
//! (`http.rs`), a recording or the fail-closed no-live client.

use std::io::Read as _;
use std::path::Path;

use toolkit_aws::recorded::{optional_list, optional_object_list, MemberError};
use toolkit_core::{open_regular_file, Json, PathError};

use super::diag;
use super::http::{HttpClient, HttpFailure};
use crate::bedrock_resolve::image::{base64_encode, data_url, mime_type};
use crate::bedrock_resolve::source::redact;
use crate::startup::artifacts::pyjson::{dumps, loads};

/// The token cap of every request; it matches the Bedrock eval side.
pub(crate) const MAX_TOKENS: i64 = 4096;
/// Most bytes of an HTTP error body that are read.
pub(crate) const ERROR_BODY_CAP: usize = 2048;
/// Statuses whose body is never shown: an auth endpoint can echo the key.
const AUTH_CODES: [u16; 2] = [401, 403];
/// Largest image file a request is built from.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Most characters of failure text a row keeps after escaping.
const STATUS_MAX_CHARS: usize = 600;

const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const GEMINI_URL_PREFIX: &str = "https://generativelanguage.googleapis.com/v1beta/models/";
/// The only hosts a key is ever sent to (the tests pin every builder to them).
#[cfg(test)]
pub(crate) const OFFICIAL_URL_PREFIXES: [&str; 3] = [
    "https://api.openai.com/",
    "https://api.anthropic.com/",
    "https://generativelanguage.googleapis.com/",
];

/// One provider request: the URL, the headers (the key is only here) and the
/// JSON body.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Json,
}

/// An image part of a request: the path (its extension names the format) and
/// the file bytes.
pub(crate) type Image<'a> = Option<(&'a str, &'a [u8])>;

fn header(name: &str, value: String) -> (String, String) {
    (name.to_string(), value)
}

fn message(role: &str, content: Json) -> Json {
    Json::object(vec![("role", Json::string(role)), ("content", content)])
}

/// A `chat/completions` request. gpt-5.x rejects `max_tokens` (HTTP 400
/// `unsupported_parameter`) and requires `max_completion_tokens`; the newer
/// name is accepted by every current model, so it is sent unconditionally.
///
/// # Errors
///
/// An unsupported image extension (the Python `ValueError`).
pub(crate) fn build_openai_request(
    model: &str,
    system: &str,
    user_text: &str,
    image: Image<'_>,
    api_key: &str,
) -> Result<Request, String> {
    let content = match image {
        None => Json::string(user_text),
        Some((path, raw)) => Json::Array(vec![
            Json::object(vec![
                ("type", Json::string("text")),
                ("text", Json::string(user_text)),
            ]),
            Json::object(vec![
                ("type", Json::string("image_url")),
                (
                    "image_url",
                    Json::object(vec![
                        ("url", Json::string(data_url(path, raw)?)),
                        ("detail", Json::string("auto")),
                    ]),
                ),
            ]),
        ]),
    };
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(message("system", Json::string(system)));
    }
    messages.push(message("user", content));
    Ok(Request {
        url: OPENAI_URL.to_string(),
        headers: vec![
            header("Authorization", format!("Bearer {api_key}")),
            header("Content-Type", "application/json".to_string()),
        ],
        body: Json::object(vec![
            ("model", Json::string(model)),
            ("messages", Json::Array(messages)),
            ("max_completion_tokens", Json::Int(MAX_TOKENS)),
        ]),
    })
}

/// A Messages API request; the system prompt is a top-level member.
///
/// # Errors
///
/// An unsupported image extension.
pub(crate) fn build_anthropic_request(
    model: &str,
    system: &str,
    user_text: &str,
    image: Image<'_>,
    api_key: &str,
) -> Result<Request, String> {
    let content = match image {
        None => Json::string(user_text),
        Some((path, raw)) => Json::Array(vec![
            Json::object(vec![
                ("type", Json::string("image")),
                (
                    "source",
                    Json::object(vec![
                        ("type", Json::string("base64")),
                        ("media_type", Json::string(mime_type(path)?)),
                        ("data", Json::string(base64_encode(raw))),
                    ]),
                ),
            ]),
            Json::object(vec![
                ("type", Json::string("text")),
                ("text", Json::string(user_text)),
            ]),
        ]),
    };
    let mut body = vec![
        ("model", Json::string(model)),
        ("max_tokens", Json::Int(MAX_TOKENS)),
        ("messages", Json::Array(vec![message("user", content)])),
    ];
    if !system.is_empty() {
        body.push(("system", Json::string(system)));
    }
    Ok(Request {
        url: ANTHROPIC_URL.to_string(),
        headers: vec![
            header("x-api-key", api_key.to_string()),
            header("anthropic-version", "2023-06-01".to_string()),
            header("Content-Type", "application/json".to_string()),
        ],
        body: Json::object(body),
    })
}

/// Whether `model` may be part of the Gemini URL path: letters, digits, `.`,
/// `_` and `-` only, so it cannot add a path, a query, a fragment or a host.
pub(crate) fn is_gemini_model_id(model: &str) -> bool {
    !model.is_empty()
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A `generateContent` request. The key travels as a header, never as a query
/// parameter: URLs end up in logs. `systemInstruction` mirrors how the
/// customer's app passes system prompts (folding it into the user turn would
/// change behavior).
///
/// # Errors
///
/// An unsupported image extension, or a model id that is not a safe path
/// segment.
pub(crate) fn build_gemini_request(
    model: &str,
    system: &str,
    user_text: &str,
    image: Image<'_>,
    api_key: &str,
) -> Result<Request, String> {
    if !is_gemini_model_id(model) {
        return Err("the model id is not valid for the Gemini endpoint".to_string());
    }
    let mut parts = vec![Json::object(vec![("text", Json::string(user_text))])];
    if let Some((path, raw)) = image {
        parts.push(Json::object(vec![(
            "inlineData",
            Json::object(vec![
                ("mimeType", Json::string(mime_type(path)?)),
                ("data", Json::string(base64_encode(raw))),
            ]),
        )]));
    }
    let mut body = vec![
        (
            "contents",
            Json::Array(vec![Json::object(vec![("parts", Json::Array(parts))])]),
        ),
        (
            "generationConfig",
            Json::object(vec![("maxOutputTokens", Json::Int(MAX_TOKENS))]),
        ),
    ];
    if !system.is_empty() {
        body.push((
            "systemInstruction",
            Json::object(vec![(
                "parts",
                Json::Array(vec![Json::object(vec![("text", Json::string(system))])]),
            )]),
        ));
    }
    Ok(Request {
        url: format!("{GEMINI_URL_PREFIX}{model}:generateContent"),
        headers: vec![
            header("x-goog-api-key", api_key.to_string()),
            header("Content-Type", "application/json".to_string()),
        ],
        body: Json::object(body),
    })
}

/// The provider of an env-file key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provider {
    OpenAi,
    Anthropic,
    Gemini,
}

impl Provider {
    pub(crate) fn from_key_name(key: &str) -> Option<Self> {
        match key {
            "OPENAI_API_KEY" => Some(Self::OpenAi),
            "ANTHROPIC_API_KEY" => Some(Self::Anthropic),
            "GEMINI_API_KEY" => Some(Self::Gemini),
            _ => None,
        }
    }

    fn build(
        self,
        model: &str,
        system: &str,
        user_text: &str,
        image: Image<'_>,
        api_key: &str,
    ) -> Result<Request, String> {
        match self {
            Self::OpenAi => build_openai_request(model, system, user_text, image, api_key),
            Self::Anthropic => build_anthropic_request(model, system, user_text, image, api_key),
            Self::Gemini => build_gemini_request(model, system, user_text, image, api_key),
        }
    }

    fn extract(self, response: &Json) -> Result<String, String> {
        match self {
            Self::OpenAi => extract_openai_text(response),
            Self::Anthropic => extract_anthropic_text(response),
            Self::Gemini => extract_gemini_text(response),
        }
    }
}

fn first_item(list: Result<&[Json], MemberError>) -> Option<&Json> {
    list.ok().and_then(<[Json]>::first)
}

fn text_of(value: Option<&Json>) -> Option<String> {
    value.and_then(Json::as_str).map(str::to_string)
}

/// The text of the first `OpenAI` choice. The message content must be text: a
/// `null` content (a refusal, a tool call) is not a baseline response.
pub(crate) fn extract_openai_text(response: &Json) -> Result<String, String> {
    let choice = first_item(optional_list(response, "choices"));
    text_of(
        choice
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content")),
    )
    .ok_or_else(|| "the response has no choices[0].message.content text".to_string())
}

/// The text of an Anthropic response, without treating thinking blocks as the
/// answer; an incomplete answer (truncated, refused, a tool call) is refused.
pub(crate) fn extract_anthropic_text(response: &Json) -> Result<String, String> {
    let blocks = optional_object_list(response, "content")
        .map_err(|error| format!("the response {error}"))?;
    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(Json::as_str) == Some("text") {
            let piece = block
                .get("text")
                .and_then(Json::as_str)
                .ok_or_else(|| "a text block of the response has no text".to_string())?;
            text.push_str(piece);
        }
    }
    let stop_reason = match response.get("stop_reason") {
        None => "unknown".to_string(),
        Some(Json::String(reason)) => reason.clone(),
        Some(other) => other.to_compact_string(),
    };
    if text.is_empty()
        || matches!(
            stop_reason.as_str(),
            "max_tokens" | "model_context_window_exceeded" | "refusal" | "tool_use"
        )
    {
        return Err(format!(
            "No complete Anthropic text response (stop_reason={})",
            diag::text(&stop_reason)
        ));
    }
    Ok(text)
}

/// The text of the first part of the first Gemini candidate.
pub(crate) fn extract_gemini_text(response: &Json) -> Result<String, String> {
    let candidate = first_item(optional_list(response, "candidates"));
    let content = candidate.and_then(|candidate| candidate.get("content"));
    let part = content.and_then(|content| first_item(optional_list(content, "parts")));
    text_of(part.and_then(|part| part.get("text")))
        .ok_or_else(|| "the response has no candidates[0].content.parts[0].text".to_string())
}

fn py_truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Int(number) => *number != 0,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        other => other.to_compact_string().len() > 2,
    }
}

fn py_str(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::String(text) => text.clone(),
        other => other.to_compact_string(),
    }
}

/// A bounded, credential-free extract of the provider's error body.
///
/// The evaluator contract needs the HTTP 400 body's message and param to tell
/// a request-shape bug from a quota or auth problem: the reason alone is
/// usually just `Bad Request`, and the body is irrecoverable after this
/// process exits. Auth errors (401/403) return no detail at all: their
/// classification uses the status code alone, and an auth endpoint can echo the
/// submitted credential in its body. Every other path is redacted against the
/// known key values, the non-JSON fallback included.
pub(crate) fn error_detail(code: u16, body: &[u8], secrets: &[String]) -> String {
    if AUTH_CODES.contains(&code) {
        return String::new();
    }
    let raw = String::from_utf8_lossy(&body[..body.len().min(ERROR_BODY_CAP)]).into_owned();
    if let Ok(document) = loads(&raw) {
        if document.type_name() == "object" {
            // `err = data.get("error", {})`; only a mapping is read.
            let err = document.get("error");
            if err.is_none_or(|err| err.type_name() == "object") {
                let msg = err.and_then(|err| err.get("message"));
                let param = err
                    .and_then(|err| err.get("param"))
                    .filter(|p| py_truthy(p));
                if let Some(param) = param {
                    let shown = msg.map_or_else(String::new, py_str);
                    return redact(&format!("{shown} (param: {})", py_str(param)), secrets);
                }
                match msg {
                    None => return String::new(),
                    Some(Json::String(text)) => return redact(text, secrets),
                    // A message that is not text cannot be redacted: the
                    // script falls back to the raw body.
                    Some(_) => {}
                }
            }
        }
    }
    redact(&raw.chars().take(200).collect::<String>(), secrets)
}

/// The failure text a row keeps: redacted against the key values, truncated
/// and escaped on one line, redacted again.
fn row_text(text: &str, secrets: &[String]) -> String {
    redact(
        &diag::bounded(&redact(text, secrets), STATUS_MAX_CHARS),
        secrets,
    )
}

/// `status` of a row for a failed request.
fn failure_status(failure: &HttpFailure, secrets: &[String]) -> String {
    match failure {
        HttpFailure::NoLive => {
            "error: ConnectionError: this build includes no live HTTP client; pass --recorded FILE to replay recorded responses".to_string()
        }
        HttpFailure::Unrecorded => {
            "error: ConnectionError: no recorded response for this request".to_string()
        }
        HttpFailure::Status { code, reason, body } => {
            let detail = error_detail(*code, body, secrets);
            let mut status = format!("http_{code}: {}", row_text(reason, secrets));
            if !detail.is_empty() {
                status.push_str(" — ");
                status.push_str(&row_text(&detail, secrets));
            }
            status
        }
        HttpFailure::Transport { kind, message } => format!(
            "error: {}: {}",
            row_text(kind, secrets),
            row_text(message, secrets)
        ),
    }
}

/// `error: <type>: <message>`, redacted and escaped.
fn error_status(kind: &str, message: &str, secrets: &[String]) -> String {
    format!("error: {kind}: {}", row_text(message, secrets))
}

/// An image file of a dataset record: its bytes, or the row status of the
/// failure (the exception class and text the script would have recorded).
fn read_image(path: &str, secrets: &[String]) -> Result<Vec<u8>, String> {
    let failed = |kind: &str, text: String| error_status(kind, &text, secrets);
    let (file, metadata) = open_regular_file(Path::new(path)).map_err(|error| match error {
        PathError::NotFound(_) => failed(
            "FileNotFoundError",
            format!("[Errno 2] No such file or directory: '{path}'"),
        ),
        PathError::Symlink(_) => failed(
            "OSError",
            format!("refusing to read a symbolic link: '{path}'"),
        ),
        PathError::NotRegularFile(_) => failed("OSError", format!("not a regular file: '{path}'")),
        _ => failed("OSError", format!("cannot read '{path}'")),
    })?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(failed(
            "OSError",
            format!("larger than {MAX_IMAGE_BYTES} bytes: '{path}'"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failed("OSError", format!("cannot read '{path}'")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IMAGE_BYTES {
        return Err(failed(
            "OSError",
            format!("larger than {MAX_IMAGE_BYTES} bytes: '{path}'"),
        ));
    }
    Ok(bytes)
}

/// The text member `name` of a dataset record: absent and `null` are empty.
fn optional_prompt_text<'a>(record: &'a Json, name: &str) -> Result<&'a str, String> {
    match record.get(name) {
        None | Some(Json::Null) => Ok(""),
        Some(Json::String(text)) => Ok(text),
        Some(_) => Err(format!("error: TypeError: {name} must be a string")),
    }
}

/// One prompt: the response text, or the `status` of the row that records the
/// failure.
fn fetch_one(
    record: &Json,
    provider: Provider,
    model: &str,
    api_key: &str,
    secrets: &[String],
    client: &mut dyn HttpClient,
) -> Result<String, String> {
    let image_path = optional_prompt_text(record, "image_path")?;
    let raw = if image_path.is_empty() {
        None
    } else {
        Some(read_image(image_path, secrets)?)
    };
    let system = optional_prompt_text(record, "system_prompt")?;
    let user_text = match record.get("user_prompt") {
        None => return Err("error: KeyError: 'user_prompt'".to_string()),
        Some(Json::String(text)) => text,
        Some(_) => return Err("error: TypeError: user_prompt must be a string".to_string()),
    };
    let image = raw.as_deref().map(|bytes| (image_path, bytes));
    let request = provider
        .build(model, system, user_text, image, api_key)
        .map_err(|text| error_status("ValueError", &text, secrets))?;
    let reply = client
        .send(&request.url, &request.headers, &request.body)
        .map_err(|failure| failure_status(&failure, secrets))?;
    provider
        .extract(&reply)
        .map_err(|text| error_status("ValueError", &text, secrets))
}

/// The prompts of a golden dataset (JSON lines; blank lines skipped). Every
/// record needs an `id` that is a scalar.
///
/// # Errors
///
/// A line that is not JSON or has no scalar `id` (its number only, no text).
pub(crate) fn parse_dataset(text: &str) -> Result<Vec<Json>, String> {
    parse_lines(text, "dataset")
}

fn parse_lines(text: &str, what: &str) -> Result<Vec<Json>, String> {
    let mut rows = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let number = index + 1;
        let row = loads(line).map_err(|_| format!("{what} line {number} is not valid JSON"))?;
        match row.get("id") {
            Some(id) if !matches!(id.type_name(), "array" | "object") => rows.push(row),
            _ => return Err(format!("{what} line {number} has no scalar \"id\"")),
        }
    }
    Ok(rows)
}

/// The rows of an earlier run that are `live` (kept as they are).
///
/// # Errors
///
/// A line that is not JSON or has no scalar `id`.
pub(crate) fn parse_live_rows(text: &str) -> Result<Vec<Json>, String> {
    Ok(parse_lines(text, "output")?
        .into_iter()
        .filter(|row| row.get("status").and_then(Json::as_str) == Some("live"))
        .collect())
}

/// What one run needs.
pub(crate) struct Run<'a> {
    pub provider: Provider,
    pub model: &'a str,
    pub api_key: &'a str,
    /// Every key value of the env file (redacted from every row).
    pub secrets: &'a [String],
}

/// The result rows: the kept live rows, then one row per fetched prompt.
pub(crate) fn run(
    config: &Run<'_>,
    kept: Vec<Json>,
    prompts: &[Json],
    client: &mut dyn HttpClient,
) -> Vec<Json> {
    let done: Vec<Json> = kept
        .iter()
        .filter_map(|row| row.get("id").cloned())
        .collect();
    let mut rows = kept;
    for record in prompts {
        let id = record.get("id").cloned().unwrap_or(Json::Null);
        if done.contains(&id) {
            continue;
        }
        let (response, status) = match fetch_one(
            record,
            config.provider,
            config.model,
            config.api_key,
            config.secrets,
            client,
        ) {
            Ok(text) => (redact(&text, config.secrets), "live".to_string()),
            Err(status) => (String::new(), status),
        };
        rows.push(Json::object(vec![
            ("id", id),
            ("source_response", Json::string(response)),
            ("status", Json::string(status)),
        ]));
    }
    rows
}

/// The output file text: one `json.dumps(row)` line per row.
pub(crate) fn rows_text(rows: &[Json]) -> String {
    rows.iter()
        .map(|row| dumps(row, None, false) + "\n")
        .collect()
}
