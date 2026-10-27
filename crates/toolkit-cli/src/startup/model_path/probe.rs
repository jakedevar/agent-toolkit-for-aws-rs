//! The probe client seam of `startup verify-model-path`.
//!
//! The Python script built a `boto3`, `anthropic` or `openai` client on demand
//! and called one method on it. Here every call is one [`Operation`] with the
//! request the Python code passed as keyword arguments, handed to a
//! [`ProbeClient`]. Tests inject a recording client; the build wires
//! [`NoLiveClient`], which sends nothing and fails every operation with a
//! `RuntimeError` (the exception the Python raised for a missing package), so a
//! default run never reaches the network or reads credentials.

use std::fmt;

use toolkit_core::Json;

/// The prompt of every probe.
pub(super) const PROMPT: &str = "Reply with exactly: ok";

/// Tokens a probe may generate.
const MAX_TOKENS: i64 = 8;

/// One call the verifier makes against the target account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    /// `client.messages.create(...)` on the Anthropic Bedrock Mantle client.
    MantleMessages,
    /// `client.responses.create(...)` on the Mantle `OpenAI` endpoint.
    OpenAiResponses,
    /// `client.chat.completions.create(...)` on the Mantle `OpenAI` endpoint.
    OpenAiChat,
    /// `bedrock-runtime` `Converse`.
    Converse,
    /// `bedrock-runtime` `InvokeModel`.
    InvokeModel,
    /// `bedrock` `GetInferenceProfile`.
    GetInferenceProfile,
}

impl Operation {
    fn describe(self) -> &'static str {
        match self {
            Self::MantleMessages => "Mantle verification",
            Self::OpenAiResponses => "Mantle Responses verification",
            Self::OpenAiChat => "Mantle Chat Completions verification",
            Self::Converse | Self::InvokeModel => "Runtime verification",
            Self::GetInferenceProfile => "Inference profile verification",
        }
    }
}

/// A failed call: the exception class name and message of the Python client.
/// The message may carry endpoints or tokens; it is redacted before it is
/// stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeError {
    pub(crate) kind: String,
    pub(crate) message: String,
}

impl ProbeError {
    pub(crate) fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

/// Sends one operation to the target account in `region` and returns the
/// response as a JSON object.
pub(crate) trait ProbeClient {
    /// # Errors
    ///
    /// The failure of the call (access denied, throttling, a missing model...).
    fn call(
        &mut self,
        operation: Operation,
        region: &str,
        request: &Json,
    ) -> Result<Json, ProbeError>;
}

/// The client of this build: no live transport is bound, nothing is sent.
pub(crate) struct NoLiveClient;

impl ProbeClient for NoLiveClient {
    fn call(
        &mut self,
        operation: Operation,
        _region: &str,
        _request: &Json,
    ) -> Result<Json, ProbeError> {
        Err(ProbeError::new(
            "RuntimeError",
            format!(
                "{} requires a live Bedrock client, which this build does not include",
                operation.describe()
            ),
        ))
    }
}

fn user_message(content: Json) -> Json {
    Json::object(vec![("role", Json::string("user")), ("content", content)])
}

fn prompt_messages() -> Json {
    Json::Array(vec![user_message(Json::string(PROMPT))])
}

/// `client.messages.create(model, max_tokens, messages)`.
pub(super) fn mantle_request(model_id: &str) -> Json {
    Json::object(vec![
        ("model", Json::string(model_id)),
        ("max_tokens", Json::Int(MAX_TOKENS)),
        ("messages", prompt_messages()),
    ])
}

/// `client.responses.create(model, input)`.
pub(super) fn openai_responses_request(model_id: &str) -> Json {
    Json::object(vec![
        ("model", Json::string(model_id)),
        ("input", Json::string(PROMPT)),
    ])
}

/// `client.chat.completions.create(model, max_completion_tokens, messages)`.
pub(super) fn openai_chat_request(model_id: &str) -> Json {
    Json::object(vec![
        ("model", Json::string(model_id)),
        ("max_completion_tokens", Json::Int(MAX_TOKENS)),
        ("messages", prompt_messages()),
    ])
}

/// `client.converse(modelId, messages, inferenceConfig)`.
pub(super) fn converse_request(model_id: &str) -> Json {
    Json::object(vec![
        ("modelId", Json::string(model_id)),
        (
            "messages",
            Json::Array(vec![user_message(Json::Array(vec![Json::object(vec![(
                "text",
                Json::string(PROMPT),
            )])]))]),
        ),
        (
            "inferenceConfig",
            Json::object(vec![("maxTokens", Json::Int(MAX_TOKENS))]),
        ),
    ])
}

/// `client.invoke_model(modelId, contentType, accept, body)`; `body` is the
/// native Bedrock request as one line of compact JSON.
pub(super) fn invoke_request(model_id: &str) -> Json {
    let body = Json::object(vec![
        ("anthropic_version", Json::string("bedrock-2023-05-31")),
        ("max_tokens", Json::Int(MAX_TOKENS)),
        ("messages", prompt_messages()),
    ]);
    Json::object(vec![
        ("modelId", Json::string(model_id)),
        ("contentType", Json::string("application/json")),
        ("accept", Json::string("application/json")),
        ("body", Json::string(body.to_compact_string())),
    ])
}

/// `client.get_inference_profile(inferenceProfileIdentifier=...)`.
pub(super) fn profile_request(identifier: &str) -> Json {
    Json::object(vec![(
        "inferenceProfileIdentifier",
        Json::string(identifier),
    )])
}
