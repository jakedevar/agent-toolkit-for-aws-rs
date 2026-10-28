//! Bedrock fail-fast preflight (`preflight_bedrock.py`): authorization,
//! region and model availability, quota.
//!
//! Always a JSON verdict, including for missing credentials or a bad region,
//! so the caller can parse the verdict instead of reading a trace. On failure
//! the top level carries `reason`, `detail` and `failing_models` lifted from the
//! first failing model. Chat models default to Converse; `--runtime-api`
//! preserves an explicit proprietary GPT Chat or Responses selection. Embedding
//! models (which do not speak Converse) go through `InvokeModel` with their
//! family's request body. Bare `OpenAI` proprietary GPT ids are mantle-served
//! and probed through the `OpenAI` Responses API; GPT-6 Astra (`us.`/`global.`)
//! and GPT-5.6 (`us.`/`in.`/`global.`) CRIS ids are `bedrock-runtime` targets
//! that take the selected runtime API probe.
//!
//! Nothing here talks to a network: every call goes through a
//! [`ProbeClient`] (see `client.rs`). The probe request shapes are the ones the
//! Python script sent, so a recording pins the wire contract.

use toolkit_aws::next_token_from;
use toolkit_aws::recorded::{optional_member, optional_object_list, optional_text};
use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::client::{ProbeClient, ProbeError, Service};
use super::diag;
use crate::bedrock_resolve::identity::strip;
use crate::startup::artifacts::pyjson::dumps;

/// Reason of a probe that could not run because this build has no live client.
pub(crate) const REASON_NO_LIVE: &str = "live_client_unavailable";
/// Reason of a probe the recording has no entry for.
pub(crate) const REASON_UNRECORDED: &str = "unrecorded_call";

const MODEL_ACCESS_DETAIL_TAIL: &str =
    ". Enable it in the Bedrock console (Model access page); this is separate from IAM.";
const PROBE_COST_NOTE: &str =
    "Minimal model/API permission probes; output budgets and cost vary by API.";
const OPUS_5_5: &str = "anthropic.claude-opus-5-5";
/// Most pages of the quota listing one run reads (a stuck listing fails soft).
const MAX_QUOTA_PAGES: usize = 100;

/// The outcome of one probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub ok: bool,
    pub reason: &'static str,
    pub detail: String,
}

impl Verdict {
    fn new(ok: bool, reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            ok,
            reason,
            detail: detail.into(),
        }
    }

    fn fail(reason: &'static str, detail: impl Into<String>) -> Self {
        Self::new(false, reason, detail)
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// Maps a service error code to a structured verdict (pure). The message is
/// caller-derived text: redacted, truncated and escaped on one line.
pub(crate) fn classify_invoke_error(code: &str, message: &str) -> Verdict {
    let shown = diag::text(message);
    match code {
        // Bedrock raises AccessDeniedException for two distinct problems: model
        // access not enabled in the console (fixed there, not in IAM) and an
        // IAM principal without bedrock:InvokeModel. The message tells them apart.
        "AccessDeniedException" => {
            let lowered = message.to_lowercase();
            if contains_any(
                &lowered,
                &[
                    "model access",
                    "access to the model",
                    "use the model",
                    "model is not",
                ],
            ) {
                Verdict::fail(
                    "model_access",
                    format!(
                        "Bedrock model access not enabled for this model — {shown}{MODEL_ACCESS_DETAIL_TAIL}"
                    ),
                )
            } else {
                Verdict::fail(
                    "authz",
                    format!(
                        "IAM denies bedrock:InvokeModel — {shown}. Grant bedrock:InvokeModel to this principal."
                    ),
                )
            }
        }
        "ValidationException" | "ResourceNotFoundException" => Verdict::fail(
            "model_unavailable",
            format!(
                "Model not available in this region — {shown}. Try a cross-region inference profile (e.g. us.<model-id>)."
            ),
        ),
        // Far enough to be throttled means authorized.
        "ThrottlingException" | "ServiceQuotaExceededException" => Verdict::new(
            true,
            "throttled_ok",
            "Authorized (probe throttled, which still proves access).",
        ),
        "UnrecognizedClientException"
        | "InvalidSignatureException"
        | "ExpiredTokenException"
        | "ExpiredToken" => Verdict::fail(
            "credentials",
            format!(
                "AWS credentials invalid or expired — {shown}. Run 'aws configure', refresh your SSO session, or set AWS_PROFILE."
            ),
        ),
        _ => Verdict::fail("unknown", format!("{}: {shown}", diag::text(code))),
    }
}

/// Embedding models do not speak the Converse API (pure).
pub(crate) fn is_embedding_model(model_id: &str) -> bool {
    model_id.to_lowercase().contains("embed")
}

/// Ids probed through the mantle Responses API rather than Converse or
/// `InvokeModel` (pure). Matches the bare proprietary GPT ids (`openai.gpt-5*`,
/// `openai.gpt-6*`: GPT-5.5/5.4/5.6 and GPT-6 Astra/Sol/Luna/6.1 Sol), which
/// are mantle-served. Deliberately not the gpt-oss ids (Converse-capable on
/// `bedrock-runtime`) nor the GPT-5.6/GPT-6 CRIS profile ids (`us.`/`in.`/
/// `global.` prefixed): those are runtime targets where Converse is supported,
/// so falling through to the standard probe is the correct behavior.
pub(crate) fn is_mantle_model(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    (id.starts_with("openai.gpt-5") || id.starts_with("openai.gpt-6")) && !id.contains("oss")
}

/// Resolves a missing legacy API path once for evaluator and rewriter
/// dispatch. Explicit plans retain their chosen API. Legacy bare proprietary
/// GPT targets use Mantle Responses; runtime targets keep the Converse
/// default. A mixed legacy target set needs an explicit plan rather than an
/// unsafe guess.
///
/// # Errors
///
/// An unknown runtime API, an empty target list without a plan path, or a mixed
/// Mantle and runtime target list without a plan path.
pub(crate) fn normalize_api_path(
    plan_path: Option<&str>,
    model_ids: &[String],
    runtime_api: Option<&str>,
) -> Result<String, String> {
    if plan_path == Some("mantle")
        && !model_ids.is_empty()
        && model_ids.iter().all(|id| is_mantle_model(id))
    {
        return Ok("mantle_openai_responses".to_string());
    }
    if plan_path == Some("runtime_openai_cris") {
        let path = match runtime_api {
            None | Some("converse") => "runtime_openai_cris",
            Some("responses") => "runtime_openai_responses",
            Some("chat_completions") => "runtime_openai_chat",
            Some(_) => {
                return Err("Unknown runtime API; resolve the saved migration plan".to_string());
            }
        };
        return Ok(path.to_string());
    }
    if let Some(path) = plan_path.filter(|path| !path.is_empty()) {
        return Ok(path.to_string());
    }
    if model_ids.is_empty() {
        return Err("No validated target model IDs to resolve an API path".to_string());
    }
    let mantle = model_ids.iter().filter(|id| is_mantle_model(id)).count();
    if mantle == model_ids.len() {
        Ok("mantle_openai_responses".to_string())
    } else if mantle > 0 {
        Err("Mixed Mantle and runtime targets require an explicit migration plan".to_string())
    } else {
        Ok("converse".to_string())
    }
}

/// Maps an HTTP status of the mantle endpoint to a verdict (pure). Mirrors
/// [`classify_invoke_error`] for the mantle surface, where the remedies differ:
/// IAM needs `bedrock-mantle:*` actions (not `bedrock:InvokeModel`), and mantle
/// has no cross-region form, though for Astra and GPT-5.6 a `bedrock-runtime`
/// CRIS id can cover the region instead.
pub(crate) fn classify_mantle_error(status: Option<u16>, message: &str) -> Verdict {
    let shown = diag::text(message);
    match status {
        Some(401 | 403) => {
            let lowered = message.to_lowercase();
            if contains_any(
                &lowered,
                &[
                    "model access",
                    "access to the model",
                    "use the model",
                    "not authorized to use",
                ],
            ) {
                Verdict::fail(
                    "model_access",
                    format!(
                        "Bedrock model access not enabled for this model — {shown}{MODEL_ACCESS_DETAIL_TAIL}"
                    ),
                )
            } else {
                Verdict::fail(
                    "authz",
                    format!(
                        "IAM denies mantle inference — {shown}. Grant bedrock-mantle actions (e.g. the AmazonBedrockMantleInferenceAccess managed policy); bedrock:InvokeModel does NOT authorize these models."
                    ),
                )
            }
        }
        Some(404) => Verdict::fail(
            "model_unavailable",
            format!(
                "Model not available at this mantle endpoint/region — {shown}. Mantle is in-region only; for GPT-5.6 try a bedrock-runtime CRIS id (us./in./global. prefixed); for GPT-6 Astra use us-east-1/us-west-2 mantle or a supported us./global. runtime CRIS path. For GPT-5.5/5.4 switch to a supported region or a different model."
            ),
        ),
        // Reaching a token-per-minute ceiling still proves authorization.
        Some(429) => Verdict::new(
            true,
            "throttled_ok",
            "Authorized (probe throttled on a TPM quota, which still proves access).",
        ),
        other => Verdict::fail(
            "unknown",
            format!(
                "{}: {shown}",
                other.map_or_else(|| "None".to_string(), |status| status.to_string())
            ),
        ),
    }
}

/// The minimal valid request body of an embedding model family (pure).
fn embed_request_body(model_id: &str) -> Option<Json> {
    let parts: Vec<&str> = model_id.split('.').collect();
    let vendor = match parts.as_slice() {
        [first, second, ..] if matches!(*first, "us" | "eu" | "apac" | "global") => *second,
        [first, ..] => *first,
        [] => "",
    };
    match vendor {
        "amazon" => Some(Json::object(vec![("inputText", Json::string("ping"))])),
        "cohere" => Some(Json::object(vec![
            ("texts", Json::Array(vec![Json::string("ping")])),
            ("input_type", Json::string("search_document")),
        ])),
        _ => None,
    }
}

fn no_live_verdict() -> Verdict {
    Verdict::fail(
        REASON_NO_LIVE,
        "This build includes no live AWS or provider client, so access was NOT verified; preflight fails closed. Pass --recorded FILE to replay recorded responses.",
    )
}

fn unrecorded_verdict(service: Service, operation: &str) -> Verdict {
    Verdict::fail(
        REASON_UNRECORDED,
        format!(
            "No recorded response for {} {operation} with this request, so access was NOT verified; preflight fails closed.",
            service.name()
        ),
    )
}

/// The transport failure kinds a verdict names, as the exception class and the
/// message the SDK would have shown.
fn transport_failure(error: &AwsError) -> Option<(&'static str, String)> {
    match error {
        AwsError::NoCredentials => Some((
            "NoCredentialsError",
            "Unable to locate credentials".to_string(),
        )),
        AwsError::Connect { message } => Some(("EndpointConnectionError", message.clone())),
        AwsError::Service(_) => None,
        other => Some(("BotoCoreError", other.to_string())),
    }
}

/// `str(ClientError)` when the response carries no message.
fn client_error_text(code: &str, operation: &str) -> String {
    format!("An error occurred ({code}) when calling the {operation} operation: ")
}

/// The verdict of a failed `bedrock-runtime` call: a service error is
/// classified by its code; a transport failure is a configuration problem on
/// the caller's machine, not a Bedrock verdict.
fn boto_failure_verdict(error: &ProbeError, operation: &str) -> Verdict {
    match error {
        ProbeError::NoLive => no_live_verdict(),
        ProbeError::Unrecorded {
            service,
            operation: unrecorded,
        } => unrecorded_verdict(*service, unrecorded),
        ProbeError::Aws(AwsError::Service(failure)) => {
            let code = failure.code().unwrap_or("Unknown");
            let message = if failure.message().is_empty() {
                client_error_text(code, operation)
            } else {
                failure.message().to_string()
            };
            classify_invoke_error(code, &message)
        }
        ProbeError::Aws(other) => {
            let (kind, message) =
                transport_failure(other).unwrap_or(("BotoCoreError", String::new()));
            Verdict::fail(
                "credentials",
                format!(
                    "{kind}: {}. Run 'aws configure', refresh your SSO session, or check the region name.",
                    diag::text(&message)
                ),
            )
        }
    }
}

/// Real minimal probe: Converse for chat models, `InvokeModel` for embeddings.
pub(crate) fn probe_model(client: &mut dyn ProbeClient, region: &str, model_id: &str) -> Verdict {
    if model_id == OPUS_5_5 && region != "eu-west-2" {
        return Verdict::fail(
            "inference_profile_required",
            "Opus 5.5 runtime in-region access is documented only in eu-west-2 (verified 2026-10-03). Elsewhere, use a supported Geo/Global profile for Converse, or verify a supported Mantle Messages path separately.",
        );
    }
    let (operation, request, success) = if is_embedding_model(model_id) {
        let Some(body) = embed_request_body(model_id) else {
            // An unknown embedding family: a wrong body would produce a
            // ValidationException indistinguishable from an availability
            // problem, so pass with an explicit caveat.
            return Verdict::new(
                true,
                "embedding_unprobed",
                "Embedding model from an unrecognized family — access not verified by preflight; confirm in the Bedrock console.",
            );
        };
        (
            "InvokeModel",
            Json::object(vec![
                ("modelId", Json::string(model_id)),
                ("body", Json::string(dumps(&body, None, false))),
                ("contentType", Json::string("application/json")),
                ("accept", Json::string("application/json")),
            ]),
            "InvokeModel (embedding) authorized.",
        )
    } else {
        (
            "Converse",
            Json::object(vec![
                ("modelId", Json::string(model_id)),
                (
                    "messages",
                    Json::Array(vec![Json::object(vec![
                        ("role", Json::string("user")),
                        (
                            "content",
                            Json::Array(vec![Json::object(vec![("text", Json::string("ping"))])]),
                        ),
                    ])]),
                ),
                (
                    "inferenceConfig",
                    Json::object(vec![("maxTokens", Json::Int(1))]),
                ),
            ]),
            "InvokeModel authorized.",
        )
    };
    match client.call(Service::BedrockRuntime, operation, &request) {
        Ok(_) => Verdict::new(true, "ok", success),
        Err(error) => boto_failure_verdict(&error, operation),
    }
}

/// The HTTP status and message of a failed `OpenAI`-surface call, or the
/// verdict of a failure that has no status.
enum SurfaceFailure {
    /// A service error response with its HTTP status.
    Status(u16, String),
    /// A transport failure: the exception class and message.
    Transport(&'static str, String),
    /// No call was made or recorded.
    Verdict(Verdict),
}

fn surface_failure(error: &ProbeError) -> SurfaceFailure {
    match error {
        ProbeError::NoLive => SurfaceFailure::Verdict(no_live_verdict()),
        ProbeError::Unrecorded { service, operation } => {
            SurfaceFailure::Verdict(unrecorded_verdict(*service, operation))
        }
        ProbeError::Aws(AwsError::Service(failure)) => {
            SurfaceFailure::Status(failure.status(), failure.message().to_string())
        }
        ProbeError::Aws(AwsError::Connect { message }) => {
            SurfaceFailure::Transport("APIConnectionError", message.clone())
        }
        ProbeError::Aws(other) => {
            let (kind, message) =
                transport_failure(other).unwrap_or(("BotoCoreError", String::new()));
            SurfaceFailure::Transport(kind, message)
        }
    }
}

/// The endpoint of the runtime `OpenAI`-compatible API for a region.
pub(crate) fn runtime_openai_url(region: &str, api_surface: &str) -> String {
    let tail = if api_surface == "chat_completions" {
        "chat/completions"
    } else {
        "responses"
    };
    format!("https://bedrock-runtime.{region}.amazonaws.com/openai/v1/{tail}")
}

/// Probes the selected runtime Chat or Responses API, not a Converse
/// substitute.
pub(crate) fn probe_runtime_openai_model(
    client: &mut dyn ProbeClient,
    model_id: &str,
    region: &str,
    api_surface: &str,
) -> Verdict {
    let (operation, body) = if api_surface == "chat_completions" {
        (
            "ChatCompletions",
            Json::object(vec![
                ("model", Json::string(model_id)),
                (
                    "messages",
                    Json::Array(vec![Json::object(vec![
                        ("role", Json::string("user")),
                        ("content", Json::string("ping")),
                    ])]),
                ),
            ]),
        )
    } else {
        (
            "Responses",
            Json::object(vec![
                ("model", Json::string(model_id)),
                ("input", Json::string("ping")),
                ("max_output_tokens", Json::Int(16)),
            ]),
        )
    };
    let request = Json::object(vec![
        ("url", Json::string(runtime_openai_url(region, api_surface))),
        ("body", body),
    ]);
    match client.call(Service::RuntimeOpenai, operation, &request) {
        Ok(_) => Verdict::new(true, "ok", format!("Runtime {api_surface} API authorized.")),
        Err(error) => match surface_failure(&error) {
            SurfaceFailure::Verdict(verdict) => verdict,
            SurfaceFailure::Status(status, message) => {
                let code = match status {
                    401 => "UnrecognizedClientException",
                    403 => "AccessDeniedException",
                    404 => "ResourceNotFoundException",
                    429 => "ThrottlingException",
                    _ => "unknown",
                };
                classify_invoke_error(code, &message)
            }
            SurfaceFailure::Transport(_, message) => classify_invoke_error("unknown", &message),
        },
    }
}

/// Real minimal probe against the mantle Responses API. A missing client
/// fails CLOSED: returning ok would turn a fail-fast preflight into an
/// unconditional green light with endpoint, model and IAM access never
/// checked.
pub(crate) fn probe_mantle_model(
    client: &mut dyn ProbeClient,
    model_id: &str,
    region: &str,
) -> Verdict {
    let request = Json::object(vec![
        ("region", Json::string(region)),
        (
            "body",
            Json::object(vec![
                ("model", Json::string(model_id)),
                ("input", Json::string("ping")),
                ("max_output_tokens", Json::Int(16)),
                ("store", Json::Bool(false)),
            ]),
        ),
    ]);
    match client.call(Service::Mantle, "Responses", &request) {
        Ok(_) => Verdict::new(true, "ok", "Mantle Responses API authorized."),
        Err(error) => match surface_failure(&error) {
            SurfaceFailure::Verdict(verdict) => verdict,
            SurfaceFailure::Status(status, message) => {
                classify_mantle_error(Some(status), &message)
            }
            SurfaceFailure::Transport(kind, message) => Verdict::fail(
                "credentials",
                format!(
                    "{kind}: {}. Check AWS credentials, the region name, and network access to the bedrock-mantle endpoint.",
                    diag::text(&message)
                ),
            ),
        },
    }
}

/// One model's entry of the report.
#[derive(Debug, Clone)]
pub(crate) struct ModelResult {
    pub verdict: Verdict,
    pub model_id: String,
    pub rpm_quota: Option<i64>,
    pub quota_note: Option<String>,
    pub quota_warning: Option<String>,
}

impl ModelResult {
    fn to_json(&self) -> Json {
        let mut members = vec![
            ("ok", Json::Bool(self.verdict.ok)),
            ("reason", Json::string(self.verdict.reason)),
            ("detail", Json::string(self.verdict.detail.clone())),
            ("model_id", Json::string(self.model_id.clone())),
            ("rpm_quota", self.rpm_quota.map_or(Json::Null, Json::Int)),
        ];
        if let Some(note) = &self.quota_note {
            members.push(("quota_note", Json::string(note.clone())));
        }
        if let Some(warning) = &self.quota_warning {
            members.push(("quota_warning", Json::string(warning.clone())));
        }
        Json::object(members)
    }
}

/// The first failing model's reason and detail, lifted to the top level so the
/// orchestrator can branch on a single `reason` (its documented contract), and
/// every failing model id (pure).
pub(crate) fn aggregate_failure(results: &[ModelResult]) -> Vec<(&'static str, Json)> {
    let failing: Vec<&ModelResult> = results.iter().filter(|result| !result.verdict.ok).collect();
    let Some(first) = failing.first() else {
        return Vec::new();
    };
    vec![
        ("reason", Json::string(first.verdict.reason)),
        ("detail", Json::string(first.verdict.detail.clone())),
        (
            "failing_models",
            Json::Array(
                failing
                    .iter()
                    .map(|result| Json::string(result.model_id.clone()))
                    .collect(),
            ),
        ),
    ]
}

fn quota_value(quota: &Json) -> Result<i64, String> {
    optional_member(quota, "Value", "a number", |member| match member {
        Json::Int(number) => Some(*number),
        // `int(float)` truncates toward zero; a float beyond the integer range
        // saturates.
        #[allow(clippy::cast_possible_truncation)]
        Json::Float(number) if number.is_finite() => Some(number.trunc() as i64),
        _ => None,
    })
    .map(Option::unwrap_or_default)
    .map_err(String::from)
}

/// Best-effort on-demand requests-per-minute quota for a model from a
/// pre-fetched quota list; `None` when nothing matches. Quota names follow
/// `On-demand model inference requests per minute for <Model Display Name>`, so
/// a per-minute request quota matches when its name shares a token with the
/// model id (`claude`, `nova`, `titan`).
///
/// # Errors
///
/// A quota whose `QuotaName` is not a string or whose `Value` is not a number.
pub(crate) fn quota_rpm(quotas: &[Json], model_id: &str) -> Result<Option<i64>, String> {
    let lowered = model_id.to_lowercase().replace(':', ".");
    let mut tokens: Vec<&str> = Vec::new();
    for part in lowered.split('.').filter(|part| !part.is_empty()) {
        for piece in part.split('-') {
            if !piece.is_empty() && !piece.chars().all(char::is_numeric) && !tokens.contains(&piece)
            {
                tokens.push(piece);
            }
        }
    }
    let mut lowest: Option<i64> = None;
    for quota in quotas {
        let name = optional_text(quota, "QuotaName")
            .map_err(String::from)?
            .unwrap_or_default()
            .to_lowercase();
        if !name.contains("per minute") || !name.contains("request") {
            continue;
        }
        if !tokens.iter().any(|token| name.contains(token)) {
            continue;
        }
        let value = quota_value(quota)?;
        lowest = Some(lowest.map_or(value, |known| known.min(value)));
    }
    Ok(lowest)
}

/// Fetches every Bedrock service quota once. A failed call (a service error,
/// a transport failure, no live client, an unrecorded call) or a listing that
/// never ends gives an empty list: quotas are best effort.
///
/// # Errors
///
/// A response whose `Quotas` member is present and not a list of objects, or
/// whose `NextToken` is not a string: a malformed response is an invalid
/// response, never an empty listing.
fn fetch_bedrock_quotas(client: &mut dyn ProbeClient) -> Result<Vec<Json>, String> {
    let mut quotas: Vec<Json> = Vec::new();
    let mut token: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..MAX_QUOTA_PAGES {
        let mut members = vec![("ServiceCode", Json::string("bedrock"))];
        if let Some(next) = &token {
            members.push(("NextToken", Json::string(next.clone())));
        }
        let Ok(page) = client.call(Service::Quotas, "ListServiceQuotas", &Json::object(members))
        else {
            return Ok(Vec::new());
        };
        quotas.extend(
            optional_object_list(&page, "Quotas")
                .map_err(|error| format!("invalid ListServiceQuotas response: {error}"))?
                .iter()
                .cloned(),
        );
        token = next_token_from(&page, "NextToken")
            .map_err(|error| format!("invalid ListServiceQuotas response: {error}"))?;
        match &token {
            None => return Ok(quotas),
            Some(next) if seen.contains(next) => return Ok(Vec::new()),
            Some(next) => seen.push(next.clone()),
        }
    }
    Ok(Vec::new())
}

/// The model ids of a `--models` value: split on commas, stripped, empty
/// entries dropped.
pub(crate) fn split_models(models: &str) -> Vec<String> {
    models
        .split(',')
        .map(strip)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

/// botocore's region-name check (`^[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?$`,
/// without the trailing-newline allowance of Python's `$`).
fn is_valid_region_name(region: &str) -> bool {
    let bytes = region.as_bytes();
    let edge = |b: u8| b.is_ascii_alphanumeric();
    !bytes.is_empty()
        && bytes.len() <= 63
        && edge(bytes[0])
        && edge(bytes[bytes.len() - 1])
        && bytes.iter().all(|b| edge(*b) || *b == b'-')
}

/// The options of one preflight run.
pub(crate) struct Options<'a> {
    pub region: &'a str,
    pub models: &'a str,
    pub dataset_size: i64,
    pub runtime_api: &'a str,
}

const MANTLE_QUOTA_NOTE: &str = "Mantle quotas are per-model input/output tokens per minute; there is no RPM quota. Pace on token throughput, and note that prompt-cached input tokens are exempt from the input-TPM quota.";
const ASTRA_RUNTIME_QUOTA_NOTE: &str = "Astra runtime TPM usage = input tokens + 10 * output tokens. Check the selected CRIS profile's token quota; an RPM check alone does not establish throughput capacity.";

fn early_failure(region: &str, reason: &'static str, detail: String) -> Json {
    Json::object(vec![
        ("ok", Json::Bool(false)),
        ("region", Json::string(diag::text(region))),
        ("models", Json::Array(Vec::new())),
        ("reason", Json::string(reason)),
        ("detail", Json::string(detail)),
    ])
}

/// One model's probe, quota note and pacing warning.
fn probe_one(
    client: &mut dyn ProbeClient,
    options: &Options<'_>,
    quotas: &[Json],
    model_id: &str,
) -> Result<ModelResult, String> {
    if is_mantle_model(model_id) {
        let verdict = probe_mantle_model(client, model_id, options.region);
        // Mantle enforces per-model input/output TPM quotas and has no RPM
        // quota, so an RPM-derived pacing warning would be meaningless here.
        let note = if model_id.to_lowercase() == "openai.gpt-6-astra" {
            format!(
                "Verify Astra's mantle quotas in the target account and {}. Do not assume GPT-5.6's cached-input quota exemption or RPM rules apply; Astra's documented 10x output burndown is runtime-specific.",
                options.region
            )
        } else {
            MANTLE_QUOTA_NOTE.to_string()
        };
        return Ok(ModelResult {
            verdict,
            model_id: model_id.to_string(),
            rpm_quota: None,
            quota_note: Some(note),
            quota_warning: None,
        });
    }
    let verdict = if options.runtime_api != "converse"
        && model_id.contains(".openai.gpt-")
        && !model_id.contains("gpt-oss")
    {
        probe_runtime_openai_model(client, model_id, options.region, options.runtime_api)
    } else {
        probe_model(client, options.region, model_id)
    };
    let rpm = quota_rpm(quotas, model_id)?;
    let tail = model_id.rsplit('/').next().unwrap_or(model_id);
    let quota_note = matches!(tail, "us.openai.gpt-6-astra" | "global.openai.gpt-6-astra")
        .then(|| ASTRA_RUNTIME_QUOTA_NOTE.to_string());
    let quota_warning = rpm.filter(|rpm| options.dataset_size > *rpm).map(|rpm| {
        format!(
            "Dataset ({}) exceeds ~{rpm} RPM quota — Eval will pace with backoff and may be slow.",
            options.dataset_size
        )
    });
    Ok(ModelResult {
        verdict,
        model_id: model_id.to_string(),
        rpm_quota: rpm,
        quota_note,
        quota_warning,
    })
}

/// Runs the preflight and returns the report and whether every model passed.
///
/// # Errors
///
/// A recorded quota listing that is malformed (see [`fetch_bedrock_quotas`]).
pub(crate) fn run(
    options: &Options<'_>,
    client: &mut dyn ProbeClient,
) -> Result<(Json, bool), String> {
    let model_ids = split_models(options.models);
    if model_ids.is_empty() {
        return Ok((
            early_failure(
                options.region,
                "no_models",
                "--models resolved to an empty list; nothing to probe.".to_string(),
            ),
            false,
        ));
    }
    if !is_valid_region_name(options.region) {
        let detail = format!(
            "InvalidRegionError: Provided region_name '{}' doesn't match a supported format.",
            diag::text(options.region)
        );
        return Ok((early_failure(options.region, "credentials", detail), false));
    }
    let quotas = fetch_bedrock_quotas(client)?;
    let mut results: Vec<ModelResult> = Vec::new();
    for model_id in &model_ids {
        results.push(probe_one(client, options, &quotas, model_id)?);
    }
    let all_ok = results.iter().all(|result| result.verdict.ok);
    let mut report = vec![
        ("ok", Json::Bool(all_ok)),
        ("region", Json::string(options.region)),
        ("probe_cost_note", Json::string(PROBE_COST_NOTE)),
        (
            "models",
            Json::Array(results.iter().map(ModelResult::to_json).collect()),
        ),
    ];
    report.extend(aggregate_failure(&results));
    Ok((Json::object(report), all_ok))
}
