//! The verification of `startup verify-model-path`: `verify_workload` and
//! `verify_recommendation` of `verify_model_path.py`.
//!
//! The recommendation has already passed the `model-recommendation` schema, so
//! its fields have the schema's types. A probe never substitutes another
//! model: it calls the recorded `invocation_model_id` or reports why it could
//! not. Every error a probe returns is redacted and escaped before it is
//! stored (AC3), because a client error can carry an endpoint URL or a bearer
//! token.

use std::collections::BTreeSet;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_aws::redact_text;
use toolkit_core::{Json, ToolkitError};

use super::probe::{
    converse_request, invoke_request, mantle_request, openai_chat_request,
    openai_responses_request, profile_request, Operation, ProbeClient, ProbeError,
};
use crate::safe_text::{safe_path, safe_text};

/// The Opus 5.5 path model whose application inference profiles are verified
/// before they are invoked.
const APPLICATION_PROFILE_MODEL: &str = "anthropic.claude-opus-5-5";

/// Marker of an application inference profile ARN.
const APPLICATION_PROFILE_MARKER: &str = ":application-inference-profile/";

/// Longest stored probe error message, in characters of the escaped text.
const MAX_ERROR_CHARS: usize = 512;

/// A recommendation that cannot be verified (a request problem, not a probe
/// result). The text names fields and counts, never an input value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelPathError(String);

impl ModelPathError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ModelPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ModelPathError> for ToolkitError {
    fn from(error: ModelPathError) -> Self {
        Self::runtime("startup_model_path_invalid", error.0)
    }
}

/// `datetime.now(utc).isoformat()`: seconds precision when the microseconds
/// are zero, `+00:00` offset.
pub(super) fn utc_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_utc(
        i64::try_from(now.as_secs()).unwrap_or(i64::MAX / 2),
        now.subsec_micros(),
    )
}

/// ISO 8601 text of a Unix time in UTC (proleptic Gregorian calendar).
pub(super) fn format_utc(seconds: i64, micros: u32) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let time = format!(
        "{:02}:{:02}:{:02}",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    );
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!("{year:04}-{month:02}-{day:02}T{time}{fraction}+00:00")
}

fn text_field<'a>(object: &'a Json, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Json::as_str)
}

/// A string-list field of the `verification` object.
fn string_list(object: &Json, key: &str) -> Vec<String> {
    match object.get(key) {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// The `error` object of a verification: the exception type and a redacted,
/// escaped, single-line message.
fn error_object(kind: &str, message: &str) -> Json {
    Json::object(vec![
        ("type", Json::string(safe_text(&redact_text(kind)))),
        ("message", Json::string(stored_message(message))),
    ])
}

/// `message` with credential-looking content redacted first (so truncation can
/// never half-cut a secret), then control and bidirectional characters escaped
/// and the length bounded.
fn stored_message(message: &str) -> String {
    let redacted = redact_text(message);
    let mut shown = String::new();
    let mut length = 0;
    for c in redacted.chars() {
        let piece = safe_path(c.encode_utf8(&mut [0; 4]));
        length += piece.chars().count();
        if length > MAX_ERROR_CHARS {
            shown.push_str("...");
            break;
        }
        shown.push_str(&piece);
    }
    shown
}

fn fail(result: &mut [(String, Json)], error: &ProbeError) {
    set(result, "status", Json::string("failed"));
    set(result, "error", error_object(&error.kind, &error.message));
}

fn set(result: &mut [(String, Json)], key: &str, value: Json) {
    if let Some(slot) = result.iter_mut().find(|(name, _)| name == key) {
        slot.1 = value;
    }
}

fn value_error(message: &str) -> ProbeError {
    ProbeError::new("ValueError", message)
}

/// The set of model ARNs an inference profile resolves to, which must be an
/// active profile of the selected model.
fn destinations(profile: &Json, path_model_id: &str) -> Result<BTreeSet<String>, ProbeError> {
    if !matches!(profile, Json::Object(_)) {
        return Err(ProbeError::new(
            "TypeError",
            "an inference profile response must be an object",
        ));
    }
    let mut models = BTreeSet::new();
    match profile.get("models") {
        None => {}
        Some(Json::Array(items)) => {
            for item in items {
                match item.get("modelArn").and_then(Json::as_str) {
                    Some(arn) => {
                        models.insert(arn.to_string());
                    }
                    None => return Err(ProbeError::new("KeyError", "'modelArn'")),
                }
            }
        }
        Some(_) => {
            return Err(ProbeError::new(
                "TypeError",
                "an inference profile's models must be a list",
            ))
        }
    }
    if text_field(profile, "status") != Some("ACTIVE") || models.is_empty() {
        return Err(value_error(
            "Inference profile has no active model destinations",
        ));
    }
    let expected = format!("foundation-model/{path_model_id}");
    if models
        .iter()
        .any(|arn| arn.splitn(6, ':').last() != Some(expected.as_str()))
    {
        return Err(value_error(
            "Inference profile does not match the selected model",
        ));
    }
    Ok(models)
}

/// `_verify_application_profile`: the application profile resolves to the
/// selected model, and either to the single in-region model destination the
/// recommendation allows or to the same destinations as one of the allowed
/// system profiles.
fn verify_application_profile(
    client: &mut dyn ProbeClient,
    region: &str,
    model_id: &str,
    path_model_id: &str,
    allowed_profiles: &[String],
    allowed_in_region: &[String],
) -> Result<(), ProbeError> {
    let profile = client.call(
        Operation::GetInferenceProfile,
        region,
        &profile_request(model_id),
    )?;
    let actual = destinations(&profile, path_model_id)?;
    if actual.len() == 1 && actual.iter().all(|arn| allowed_in_region.contains(arn)) {
        return Ok(());
    }
    for reference_id in allowed_profiles {
        let reference = client
            .call(
                Operation::GetInferenceProfile,
                region,
                &profile_request(reference_id),
            )
            .and_then(|reference| destinations(&reference, path_model_id));
        if reference.is_ok_and(|reference| reference == actual) {
            return Ok(());
        }
    }
    Err(value_error(
        "Application profile destinations do not match a verified allowed inference profile",
    ))
}

/// `_response_model_id`: the `model` of the response, else its `modelId`.
fn response_model_id(response: &Json) -> Json {
    let pick = |key: &str| {
        response
            .get(key)
            .filter(|value| matches!(value, Json::String(text) if !text.is_empty()))
    };
    pick("model")
        .or_else(|| pick("modelId"))
        .cloned()
        .unwrap_or(Json::Null)
}

/// The probe of `path` against `model_id`.
fn probe(
    client: &mut dyn ProbeClient,
    path: Option<&str>,
    region: &str,
    model_id: &str,
) -> Result<Json, ProbeError> {
    let (operation, request) = match path {
        Some("mantle_messages") => (Operation::MantleMessages, mantle_request(model_id)),
        Some("mantle_openai_responses") => (
            Operation::OpenAiResponses,
            openai_responses_request(model_id),
        ),
        Some("mantle_openai_chat") => (Operation::OpenAiChat, openai_chat_request(model_id)),
        Some("runtime_converse") => (Operation::Converse, converse_request(model_id)),
        Some("runtime_invoke") => (Operation::InvokeModel, invoke_request(model_id)),
        other => {
            return Err(value_error(&format!(
                "verification is not implemented for API path: {}",
                safe_text(other.unwrap_or("None"))
            )))
        }
    };
    client.call(operation, region, &request)
}

/// `_base_result`: the not-yet-probed verification of one workload.
fn base_result(workload_id: &str, recommendation: &Json) -> Vec<(String, Json)> {
    let identity = recommendation.get("model_identity");
    let verification = recommendation.get("verification");
    let field = |owner: Option<&Json>, key: &str| {
        owner
            .and_then(|owner| owner.get(key))
            .cloned()
            .unwrap_or(Json::Null)
    };
    [
        ("workload_id", Json::string(workload_id)),
        (
            "decision_status",
            field(Some(recommendation), "decision_status"),
        ),
        ("api_path", field(Some(recommendation), "api_path")),
        ("path_model_id", field(identity, "path_model_id")),
        (
            "invocation_model_id",
            field(Some(recommendation), "invocation_model_id"),
        ),
        ("region", field(verification, "region")),
        ("status", Json::string("not_run")),
        ("checked_at", Json::Null),
        ("response_model_id", Json::Null),
        ("error", Json::Null),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

/// `verify_workload`: probes one workload and returns its verification record.
pub(super) fn verify_workload(
    workload_id: &str,
    recommendation: &Json,
    client: &mut dyn ProbeClient,
    checked_at: &str,
) -> Json {
    let mut result = base_result(workload_id, recommendation);
    if text_field(recommendation, "decision_status") != Some("recommended") {
        set(&mut result, "status", Json::string("not_applicable"));
        return Json::Object(result);
    }
    set(&mut result, "checked_at", Json::string(checked_at));
    let Some(model_id) =
        text_field(recommendation, "invocation_model_id").filter(|id| !id.is_empty())
    else {
        set(&mut result, "status", Json::string("needs_resolution"));
        set(
            &mut result,
            "error",
            error_object(
                "UnresolvedInvocationModelId",
                "Resolve an account-invocable inference profile ID before probing; the verifier \
                 will not substitute a model ID.",
            ),
        );
        return Json::Object(result);
    };
    let verification = recommendation.get("verification").unwrap_or(&Json::Null);
    let region = text_field(verification, "region").unwrap_or_default();
    let path_model_id = recommendation
        .get("model_identity")
        .and_then(|identity| text_field(identity, "path_model_id"))
        .unwrap_or_default();
    let path = text_field(recommendation, "api_path");

    let outcome = (|| {
        if model_id.contains(APPLICATION_PROFILE_MARKER)
            && path_model_id == APPLICATION_PROFILE_MODEL
        {
            let allowed = string_list(verification, "allowed_inference_profiles");
            let in_region = string_list(verification, "allowed_in_region_model_arns");
            if allowed.is_empty() && in_region.is_empty() {
                return Err(value_error(
                    "Application profile requires verified model and residency constraints",
                ));
            }
            verify_application_profile(
                client,
                region,
                model_id,
                path_model_id,
                &allowed,
                &in_region,
            )?;
        }
        probe(client, path, region, model_id)
    })();
    match outcome {
        Ok(response) => {
            set(&mut result, "status", Json::string("passed"));
            set(
                &mut result,
                "response_model_id",
                response_model_id(&response),
            );
        }
        Err(error) => fail(&mut result, &error),
    }
    Json::Object(result)
}

/// `verify_recommendation`: the verification document for the selected
/// workloads (all of them when `workload_ids` is empty), in the order of the
/// recommendation.
///
/// # Errors
///
/// A [`ModelPathError`] when a selected workload is not in the recommendation
/// or the workloads are not an object.
pub(super) fn verify_recommendation(
    recommendation: &Json,
    workload_ids: &[String],
    client: &mut dyn ProbeClient,
    now: Option<&str>,
) -> Result<Json, ModelPathError> {
    let Some(Json::Object(workloads)) = recommendation.get("workloads") else {
        return Err(ModelPathError::new(
            "the recommendation \"workloads\" must be a JSON object",
        ));
    };
    let unknown = workload_ids
        .iter()
        .filter(|id| !workloads.iter().any(|(name, _)| name == *id))
        .count();
    if unknown > 0 {
        let known: Vec<String> = workloads.iter().map(|(name, _)| safe_text(name)).collect();
        return Err(ModelPathError::new(format!(
            "workload not found in recommendation: {unknown} selected workload id(s) are unknown; \
             the recommendation has: {}",
            known.join(", ")
        )));
    }
    let generated_at = now.map_or_else(utc_now, str::to_string);
    let mut verified = Vec::new();
    for (workload_id, workload) in workloads {
        if workload_ids.is_empty() || workload_ids.contains(workload_id) {
            verified.push((
                workload_id.clone(),
                verify_workload(workload_id, workload, client, &generated_at),
            ));
        }
    }
    Ok(Json::object(vec![
        ("schema_version", Json::Int(1)),
        (
            "recommendation_schema_version",
            recommendation
                .get("schema_version")
                .cloned()
                .unwrap_or(Json::Null),
        ),
        ("generated_at", Json::string(generated_at)),
        ("workloads", Json::Object(verified)),
    ]))
}
