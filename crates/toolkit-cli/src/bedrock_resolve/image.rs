//! Image-bearing request payloads for Converse, Responses and Chat Completions
//! (`image_input.py`).
//!
//! The extension is not the wire format (`.jpg` is `jpeg` for Converse and
//! `image/jpeg` for a data URL, never `image/jpg`), a Responses `input[]` item
//! is a user message wrapper and `detail` is always emitted on a Responses
//! image block. Every message is built in this one place.

use toolkit_core::Json;

use crate::safe_text::safe_text;

/// Accepted by `ResponseInputImageParam.detail` of the pinned `OpenAI` SDK.
pub(crate) const DETAIL_LEVELS: [&str; 4] = ["low", "high", "auto", "original"];
/// The SDK's documented default.
pub(crate) const DEFAULT_DETAIL: &str = "auto";

/// Formats Bedrock Converse accepts, keyed by the extensions mapping onto them.
const CONVERSE_FORMATS: [(&str, &str); 5] = [
    ("jpg", "jpeg"),
    ("jpeg", "jpeg"),
    ("png", "png"),
    ("gif", "gif"),
    ("webp", "webp"),
];

/// Python `Path(image_path).suffix.lstrip(".").lower()`.
fn extension(image_path: &str) -> String {
    // pathlib drops empty and `.` components; the name is the last one.
    let name = image_path
        .split('/')
        .rfind(|part| !part.is_empty() && *part != ".")
        .unwrap_or("");
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => {
            name[index..].trim_start_matches('.').to_lowercase()
        }
        _ => String::new(),
    }
}

/// Converse `image.format` for a file path. `.jpg` is `jpeg`: the extension
/// and the wire format differ and Converse rejects `jpg`.
///
/// # Errors
///
/// An unsupported extension is refused rather than guessed; the message
/// repeats the (escaped) extension and path.
pub(crate) fn converse_format(image_path: &str) -> Result<&'static str, String> {
    let ext = extension(image_path);
    CONVERSE_FORMATS
        .iter()
        .find(|(known, _)| *known == ext)
        .map(|(_, format)| *format)
        .ok_or_else(|| {
            format!(
                "unsupported image type '{}' for {}: Bedrock accepts ['gif', 'jpeg', 'png', 'webp']",
                safe_text(&ext),
                safe_text(image_path)
            )
        })
}

/// MIME type for a data URL, for example `.jpg` is `image/jpeg`.
pub(crate) fn mime_type(image_path: &str) -> Result<String, String> {
    converse_format(image_path).map(|format| format!("image/{format}"))
}

/// Standard base64 with padding (moved to `toolkit_core::encoding`, #5118).
pub(crate) use toolkit_core::encoding::base64_standard as base64_encode;

/// Base64 data URL for the Responses `input_image.image_url` field.
pub(crate) fn data_url(image_path: &str, raw: &[u8]) -> Result<String, String> {
    Ok(format!(
        "data:{};base64,{}",
        mime_type(image_path)?,
        base64_encode(raw)
    ))
}

const RAW_REQUIRED: &str = "raw image bytes are required when image_path is given";

/// The image part of a request: the path names the format, `raw` is the file.
fn image_parts<'a>(
    image_path: Option<&'a str>,
    raw: Option<&'a [u8]>,
) -> Result<Option<(&'a str, &'a [u8])>, String> {
    match image_path {
        None => Ok(None),
        Some(path) => raw
            .map(|bytes| Some((path, bytes)))
            .ok_or_else(|| RAW_REQUIRED.to_string()),
    }
}

fn message(content: Vec<Json>) -> Json {
    Json::object(vec![
        ("role", Json::string("user")),
        ("content", Json::Array(content)),
    ])
}

/// One Converse `messages[]` item; the image block precedes the text block.
/// JSON has no byte string, so `source.bytes` is `{"base64": "..."}`.
///
/// # Errors
///
/// Missing bytes for a given path, or an unsupported extension.
pub(crate) fn converse_message(
    prompt: &str,
    image_path: Option<&str>,
    raw: Option<&[u8]>,
) -> Result<Json, String> {
    let mut content = Vec::new();
    if let Some((path, bytes)) = image_parts(image_path, raw)? {
        content.push(Json::object(vec![(
            "image",
            Json::object(vec![
                ("format", Json::string(converse_format(path)?)),
                (
                    "source",
                    Json::object(vec![(
                        "bytes",
                        Json::object(vec![("base64", Json::string(base64_encode(bytes)))]),
                    )]),
                ),
            ]),
        )]));
    }
    content.push(Json::object(vec![("text", Json::string(prompt))]));
    Ok(message(content))
}

/// One Responses API `input[]` item, wrapped as a user message. `detail` is
/// always emitted on the image block because the SDK declares it required.
///
/// # Errors
///
/// An unknown `detail`, missing bytes for a given path, or an unsupported
/// extension (checked in that order).
pub(crate) fn responses_message(
    prompt: &str,
    image_path: Option<&str>,
    raw: Option<&[u8]>,
    detail: &str,
) -> Result<Json, String> {
    if !DETAIL_LEVELS.contains(&detail) {
        return Err(format!(
            "invalid image detail '{}': expected one of {:?}",
            safe_text(detail),
            DETAIL_LEVELS
        ));
    }
    let mut content = vec![Json::object(vec![
        ("type", Json::string("input_text")),
        ("text", Json::string(prompt)),
    ])];
    if let Some((path, bytes)) = image_parts(image_path, raw)? {
        content.push(Json::object(vec![
            ("type", Json::string("input_image")),
            ("image_url", Json::string(data_url(path, bytes)?)),
            ("detail", Json::string(detail)),
        ]));
    }
    Ok(message(content))
}

/// One Chat Completions message with the same image MIME normalization.
///
/// # Errors
///
/// Missing bytes for a given path, or an unsupported extension.
pub(crate) fn chat_message(
    prompt: &str,
    image_path: Option<&str>,
    raw: Option<&[u8]>,
) -> Result<Json, String> {
    let mut content = vec![Json::object(vec![
        ("type", Json::string("text")),
        ("text", Json::string(prompt)),
    ])];
    if let Some((path, bytes)) = image_parts(image_path, raw)? {
        content.push(Json::object(vec![
            ("type", Json::string("image_url")),
            (
                "image_url",
                Json::object(vec![("url", Json::string(data_url(path, bytes)?))]),
            ),
        ]));
    }
    Ok(message(content))
}
