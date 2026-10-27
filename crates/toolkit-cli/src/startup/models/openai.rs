//! Deterministic OpenAI-to-Bedrock model/path recommendation
//! (`openai_model_recommendation.py`).
//!
//! Factual baseline: an internal OpenAI-to-Bedrock migration reference (2026-06
//! live probes and its recorded feature-status notes). The engine is offline.
//! Rankings are only produced where the catalog provides evidence; otherwise
//! the engine returns an explicit decision or a provisional limitation rather
//! than inventing a comparative order.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::LazyLock;

use fancy_regex::Regex;
use toolkit_core::Json;

use super::anthropic::{
    group, invocation_requires_cris, profile_verification,
    resolve_invocation_model_id as resolve_profile,
};
use super::catalog::{Catalog, Model, PathConfig};
use super::findings::{
    self, candidate, compatibility, delta, deltas_json, finding, findings_json, impact, object,
    optional_text, rollout, strings, text, Delta, Finding, BLOCKS, TUNE,
};
use super::request::{Requirements, Source, Workload};
use super::{ModelError, Res};
use crate::safe_text::safe_text;

/// The display name the catalog gives GPT-6 Astra; its evidence is path-level
/// and several rules are specific to it.
const ASTRA_NAME: &str = "OpenAI GPT-6 Astra (on Bedrock)";
const ASTRA_KEY: &str = "openai_gpt_6_astra";

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("a valid built-in pattern")
}

// Reasoning generation: gpt-5 / gpt-6 variants and o1/o3/o4.
static REASONING_PATTERNS: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        pattern(r"(?i)(^|[^a-z0-9])gpt[-_ ]?[56](\.\d+)?([^0-9]|$)"),
        pattern(r"(?i)(^|[^a-z0-9])o[134](-mini|-pro|-preview)?([^a-z0-9]|$)"),
    ]
});
// Legacy generation: gpt-4*, gpt-3.5 / gpt-35.
static LEGACY_PATTERNS: LazyLock<[Regex; 5]> = LazyLock::new(|| {
    [
        pattern(r"(?i)gpt[-_ ]?4o"),
        pattern(r"(?i)gpt[-_ ]?4(?!\d)"),
        pattern(r"(?i)gpt[-_ ]?4[-_ ]?32k"),
        pattern(r"(?i)gpt[-_ ]?3\.?5"),
        pattern(r"(?i)gpt[-_ ]?35"),
    ]
});
static OPENAI_VERSION: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)gpt[-_ ]?(\d+(?:\.\d+)?)"));

/// `detect_family`: `reasoning`, `legacy` or `unknown`. Opaque deployment names
/// stay `unknown`, so the engine never infers a model family from an alias.
pub(super) fn detect_family(model_id: &str) -> &'static str {
    let name = python_strip(model_id);
    if name.is_empty() {
        return "unknown";
    }
    if REASONING_PATTERNS
        .iter()
        .any(|expression| expression.is_match(name).unwrap_or(false))
    {
        return "reasoning";
    }
    if LEGACY_PATTERNS
        .iter()
        .any(|expression| expression.is_match(name).unwrap_or(false))
    {
        return "legacy";
    }
    "unknown"
}

fn python_strip(text: &str) -> &str {
    text.trim_matches(crate::startup::text::is_space)
}

fn primary_source_id(source: &Source) -> &str {
    source.model_ids.first().map_or("", String::as_str)
}

/// `_source_family`: reasoning if any id is reasoning, else legacy if any is
/// legacy, else unknown.
fn source_family(source: &Source) -> &'static str {
    let families: Vec<&'static str> = if source.model_ids.is_empty() {
        vec![detect_family("")]
    } else {
        source
            .model_ids
            .iter()
            .map(|id| detect_family(id))
            .collect()
    };
    if families.contains(&"reasoning") {
        "reasoning"
    } else if families.contains(&"legacy") {
        "legacy"
    } else {
        "unknown"
    }
}

// Hosted tools and modalities that need re-platforming, not a model swap.
const HOSTED_TOOL_IMPACTS: [(&str, &str, &str); 8] = [
    (
        "web_search",
        "Hosted web search does not execute live on Bedrock Mantle (verified passthrough: \
no query issued, no citations).",
        "Implement a client-side tool loop, or expose a server-side tool (MCP/Lambda) through \
Bedrock AgentCore Gateway; use Knowledge Bases for document retrieval.",
    ),
    (
        "file_search_retrieval",
        "Hosted file search / retrieval is not a drop-in on Bedrock.",
        "Re-platform retrieval onto Bedrock Knowledge Bases or application-owned retrieval.",
    ),
    (
        "files_api",
        "The OpenAI Files API and vector stores do not port directly.",
        "Redesign data ingestion and retrieval; use Knowledge Bases or application storage.",
    ),
    (
        "vector_stores",
        "OpenAI vector stores are not a Bedrock primitive.",
        "Move vectors to a Bedrock-supported store and retrieve from the application.",
    ),
    (
        "assistants_threads",
        "Assistants/Threads hosted state is not available on Bedrock.",
        "Redesign agent state, tools, and memory (e.g. Bedrock AgentCore).",
    ),
    (
        "audio_modality",
        "Audio is not a single-call text-model capability on Bedrock.",
        "Split into a verified STT/TTS service path (Amazon Transcribe / Polly).",
    ),
    (
        "embeddings",
        "Embeddings require a separate Bedrock embedding model, not the text model.",
        "Select a sourced Bedrock embedding model (e.g. Titan/Cohere) as a separate workload.",
    ),
    (
        "images",
        "Image generation/editing is a separate capability, not a text-model swap.",
        "Select a sourced Bedrock image model/service as a separate workload.",
    ),
];

// Converse tier map: a governance workload keeps its capability tier when
// moving from the (Mantle-only) GPT-5.x family to Claude on runtime Converse.
//   GPT-5.6 Sol (frontier)      -> Claude Opus 5.5
//   GPT-5.6 Terra / 5.5 / 5.4   -> Claude Sonnet 5 (also the default tier)
//   GPT-5.6 Luna (fast/low-cost)-> Claude Haiku 4.5
const CONVERSE_TIER_DEFAULT: &str = "anthropic_claude_sonnet_5";
const CONVERSE_TIER_ORDER: [&str; 3] = [
    "anthropic_claude_sonnet_5",
    "anthropic_claude_opus_5_5",
    "anthropic_claude_haiku_4_5",
];

/// `_converse_tier_for_source`.
pub(super) fn converse_tier_for_source(source: &Source) -> &'static str {
    let id = primary_source_id(source).to_lowercase();
    if id.contains("5.6-sol") || id.contains("6-astra") {
        "anthropic_claude_opus_5_5"
    } else if id.contains("5.6-luna") {
        "anthropic_claude_haiku_4_5"
    } else {
        // 5.6-terra, 5.5, 5.4, legacy, and unknown sources all map to the
        // balanced tier.
        CONVERSE_TIER_DEFAULT
    }
}

/// Recognized source identities and their catalog keys.
pub(super) const SOURCE_MODEL_KEYS: [(&str, &str); 7] = [
    ("gpt-5.6-sol", "openai_gpt_5_6_sol"),
    ("gpt-5.6-terra", "openai_gpt_5_6_terra"),
    ("gpt-5.6-luna", "openai_gpt_5_6_luna"),
    ("gpt-6-astra", "openai_gpt_6_astra"),
    ("gpt-6-sol", "openai_gpt_6_sol"),
    ("gpt-6-luna", "openai_gpt_6_luna"),
    ("gpt-6.1-sol", "openai_gpt_6_1_sol"),
];

fn is_identity_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// `(?<![a-z0-9])identity(?![a-z0-9])` somewhere in `haystack`.
fn contains_identity(haystack: &str, identity: &str) -> bool {
    haystack.match_indices(identity).any(|(start, found)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + found.len()..].chars().next();
        !before.is_some_and(is_identity_char) && !after.is_some_and(is_identity_char)
    })
}

/// `_same_model_catalog_key`: the exact catalog key of a recognized source
/// model identity.
pub(super) fn same_model_catalog_key(source: &Source) -> Option<&'static str> {
    let id: String = primary_source_id(source)
        .to_lowercase()
        .chars()
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect();
    let mut identities = SOURCE_MODEL_KEYS;
    identities.sort_by_key(|(identity, _)| std::cmp::Reverse(identity.len()));
    identities
        .iter()
        .find(|(identity, _)| contains_identity(&id, identity))
        .map(|(_, key)| *key)
}

/// `_converse_candidate_order`: the same model first, then the tier map.
pub(super) fn converse_candidate_order(
    source: &Source,
    requirements: Option<&Requirements>,
) -> Vec<&'static str> {
    let tier = if requirements.is_some_and(|req| req.priority == "quality") {
        "anthropic_claude_opus_5_5"
    } else {
        converse_tier_for_source(source)
    };
    let mut order = vec![tier];
    order.extend(
        CONVERSE_TIER_ORDER
            .iter()
            .copied()
            .filter(|key| *key != tier),
    );
    if let Some(same) = same_model_catalog_key(source) {
        // Same model outranks any cross-family tier: keeping the model removes
        // prompt-adaptation and behavior-delta risk entirely.
        order.insert(0, same);
    }
    order
}

/// `_api_continuity`: required | preferred | `not_required` | unknown. Never
/// reuses Anthropic's `preserve_messages_api`.
fn api_continuity(requirements: &Requirements) -> &str {
    if let Some(explicit) = requirements.api_continuity.as_deref() {
        return explicit;
    }
    match requirements.preserve_openai_api {
        Some(true) => "required",
        Some(false) => "not_required",
        None => "unknown",
    }
}

/// `_mantle_path`.
fn mantle_path(source: &Source, requirements: &Requirements) -> &'static str {
    let same = same_model_catalog_key(source);
    if source.api_surface == "chat_completions"
        && matches!(api_continuity(requirements), "required" | "preferred")
        && same.is_some_and(|key| key.starts_with("openai_gpt_6"))
    {
        "mantle_openai_chat"
    } else {
        "mantle_openai_responses"
    }
}

/// `_mantle_candidate_order`: the exact source model, then catalog order.
fn mantle_candidate_order(catalog: &Catalog, source: &Source) -> Vec<String> {
    let same = same_model_catalog_key(source);
    let mut keys: Vec<String> = same.map(str::to_string).into_iter().collect();
    keys.extend(
        catalog
            .models
            .iter()
            .map(|(key, _)| key.clone())
            .filter(|key| Some(key.as_str()) != same),
    );
    keys
}

/// `_hard_profile_requirement`: whether runtime selection must prove a specific
/// CRIS profile is callable.
fn hard_profile_requirement(requirements: &Requirements) -> bool {
    let residency = requirements.data_residency.as_deref();
    requirements.inference_profile_id.is_some()
        || residency == Some("global_allowed")
        || (residency == Some("geo_required") && requirements.cris_geography.is_some())
}

fn runtime_required(requirements: &Requirements) -> bool {
    !requirements.governance.is_empty() || requirements.multi_model_converse == Some(true)
}

/// A catalog model chosen for a path (capabilities replaced by the path's own
/// evidence when it has any).
pub(super) struct Hit {
    key: String,
    model: Model,
    path_config: PathConfig,
}

/// `_catalog_model_for_path`: the first available catalog model for `path`
/// whose capability evidence covers the required/detected text features. A
/// candidate without evidence is skipped in favor of a later probed one; when
/// none covers the requirements the first available model is returned with its
/// unmet capabilities so the caller can fail closed.
fn catalog_model_for_path(
    catalog: &Catalog,
    path: &str,
    detected: &[String],
    requirements: &Requirements,
    candidate_order: Option<&[String]>,
    region: &str,
) -> (Option<Hit>, Vec<String>) {
    let keys: Vec<String> = candidate_order.map_or_else(
        || catalog.models.iter().map(|(key, _)| key.clone()).collect(),
        <[String]>::to_vec,
    );
    let mut first: Option<Hit> = None;
    let mut first_unmet = Vec::new();
    for key in keys {
        let Some(model) = catalog.model(&key) else {
            continue;
        };
        let Some(path_config) = model.path(path) else {
            continue;
        };
        if !path_config.available {
            continue;
        }
        let mut model = model.clone();
        if let Some(capabilities) = &path_config.capabilities {
            model.capabilities.clone_from(capabilities);
        }
        if path.starts_with("mantle_") && !region.is_empty() && region != "unknown" {
            if let Some((commercial, govcloud)) = &model.region_availability {
                let listed = commercial
                    .iter()
                    .chain(govcloud)
                    .any(|known| known == region);
                if !listed {
                    continue;
                }
            }
        }
        if path == "runtime_converse"
            && path_config
                .inference_profile_regions
                .as_ref()
                .is_none_or(Vec::is_empty)
            && path_config.requires_cris
            && hard_profile_requirement(requirements)
            && resolve_invocation_model_id(
                &path_config.model_id,
                true,
                requirements,
                Some(&model),
                region,
            )
            .is_none()
        {
            continue;
        }
        let unmet = unsupported_required_capabilities(&model, detected, requirements);
        let hit = Hit {
            key,
            model,
            path_config: path_config.clone(),
        };
        if unmet.is_empty() {
            return (Some(hit), Vec::new());
        }
        if first.is_none() {
            first = Some(hit);
            first_unmet = unmet;
        }
    }
    (first, first_unmet)
}

/// `_resolve_invocation_model_id` of the `OpenAI` engine: a model with
/// inference profiles resolves like an Anthropic one; otherwise the stated
/// profile or residency posture builds the id.
pub(super) fn resolve_invocation_model_id(
    model_id: &str,
    requires_cris: bool,
    requirements: &Requirements,
    model: Option<&Model>,
    region: &str,
) -> Option<String> {
    if model.is_some_and(|model| !model.inference_profiles.is_empty()) {
        return resolve_profile(model_id, requires_cris, requirements, model, region);
    }
    if !requires_cris {
        return Some(model_id.to_string());
    }
    if let Some(explicit) = &requirements.inference_profile_id {
        return Some(explicit.clone());
    }
    match requirements.data_residency.as_deref().unwrap_or("unknown") {
        "global_allowed" => Some(format!("global.{model_id}")),
        "geo_required" => requirements
            .cris_geography
            .as_ref()
            .map(|geography| format!("{geography}.{model_id}")),
        _ => None,
    }
}

fn model_identity(
    key: &str,
    model: &Model,
    path_config: &PathConfig,
    invocation: Option<&str>,
) -> Json {
    object(vec![
        ("model_key", text(key)),
        ("display_name", text(&model.display_name)),
        ("family", text(&model.family)),
        ("version", text(&model.version)),
        ("context_window", model.context_window.json()),
        ("output_token_ceiling", model.output_token_ceiling.json()),
        ("path_model_id", text(&path_config.model_id)),
        (
            "requires_cris",
            Json::Bool(invocation_requires_cris(path_config, invocation)),
        ),
    ])
}

/// `_numeric_requirement_conflict`: an unknown catalog limit cannot satisfy a
/// hard numeric need.
fn numeric_requirement_conflict(model: &Model, requirements: &Requirements) -> Vec<String> {
    let mut conflicts = Vec::new();
    for (need, limit, label) in [
        (
            requirements.min_context_tokens,
            model.context_window,
            "context window",
        ),
        (
            requirements.expected_output_tokens,
            model.output_token_ceiling,
            "output ceiling",
        ),
    ] {
        let Some(need) = need.filter(|need| *need != 0) else {
            continue;
        };
        match limit {
            super::catalog::Limit::Unknown => conflicts.push(format!(
                "{label} for {} is unknown in the catalog; a required {need}-token need cannot be confirmed",
                model.display_name
            )),
            super::catalog::Limit::Known(have) if have < need => conflicts.push(format!(
                "{label} for {} ({have}) is below the required {need}",
                model.display_name
            )),
            super::catalog::Limit::Known(_) => {}
        }
    }
    conflicts
}

/// `_openai_version`: the numeric version (`5`, `5.4`) an id names.
fn openai_version(id: &str) -> Option<String> {
    OPENAI_VERSION
        .captures(id)
        .ok()
        .flatten()
        .map(|found| group(&found, 1).to_string())
}

/// `_version_tuple` of the `OpenAI` engine: `major[.minor]`.
fn version_tuple(value: &str) -> Option<(i64, i64)> {
    let mut parts = value.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = match parts.next() {
        Some(part) => part.parse().ok()?,
        None => 0,
    };
    Some((major, minor))
}

/// `_target_accepts_sampling`: GPT-5.2+ re-accepts sampling on Responses.
/// `None` when the target is not an `OpenAI` model of known version.
fn target_accepts_sampling(target: Option<&Model>) -> Option<bool> {
    let target = target?;
    if target.generation.as_deref() == Some("bedrock_native") || target.display_name == ASTRA_NAME {
        return None;
    }
    version_tuple(&target.version).map(|version| version >= (5, 2))
}

struct SourceAnalysis {
    detected_version: Option<String>,
    target_version: Option<String>,
    version_changed: Option<bool>,
    source_family: &'static str,
    source_api_surface: String,
    model_generation_changes: Option<bool>,
}

impl SourceAnalysis {
    fn json(&self) -> Json {
        object(vec![
            (
                "detected_version",
                optional_text(self.detected_version.as_deref()),
            ),
            (
                "target_version",
                optional_text(self.target_version.as_deref()),
            ),
            (
                "version_changed",
                self.version_changed.map_or(Json::Null, Json::Bool),
            ),
            ("source_family", text(self.source_family)),
            ("source_api_surface", text(&self.source_api_surface)),
            (
                "model_generation_changes",
                self.model_generation_changes.map_or(Json::Null, Json::Bool),
            ),
        ])
    }
}

/// `_source_analysis`: source facts plus, once a target is selected, the
/// target-derived version and generation change flags.
fn source_analysis(source: &Source, target: Option<&Model>) -> SourceAnalysis {
    let source_id = primary_source_id(source);
    let family = source_family(source);
    let (target_version, version_changed, generation_changes) = match target {
        None => (None, None, None),
        Some(target) if target.generation.as_deref() == Some("bedrock_native") => {
            // An OpenAI source -> a Bedrock-native model is always a model change.
            (Some(target.version.clone()), Some(true), Some(true))
        }
        Some(target) => {
            let changed = match openai_version(source_id) {
                Some(version) if !target.version.is_empty() => Some(version != target.version),
                // o-series / opaque source vs a specific GPT-5.x target: a
                // move, but the exact source version is unknown.
                _ => matches!(family, "legacy" | "reasoning").then_some(true),
            };
            let generation =
                family == "legacy" && target.generation.as_deref() == Some("reasoning");
            (Some(target.version.clone()), changed, Some(generation))
        }
    };
    SourceAnalysis {
        detected_version: (!source_id.is_empty()).then(|| source_id.to_string()),
        target_version,
        version_changed,
        source_family: family,
        source_api_surface: source.api_surface.clone(),
        model_generation_changes: generation_changes,
    }
}

/// `_reasoning_findings`: version- and surface-specific parameter findings,
/// derived from the SELECTED target model and path, never from the source id
/// and never Anthropic's blanket sampling-removal rule.
#[allow(clippy::too_many_lines)]
fn reasoning_findings(
    source: &Source,
    target: &Model,
    path: &str,
    detected: &[String],
) -> (Vec<Finding>, Vec<Finding>, Vec<Delta>) {
    let mut blocks = Vec::new();
    let mut tuning = Vec::new();
    let mut deltas = Vec::new();
    let family = source_family(source);
    let source_id = primary_source_id(source);
    let target_name = target.display_name.as_str();
    let sampling_detected = detected.iter().any(|feature| feature == "sampling_params");

    if target.generation.as_deref() == Some("reasoning") {
        tuning.push(finding(
            "reasoning_token_headroom",
            TUNE,
            "Reasoning models spend hidden thinking tokens against the output budget; \
too-small caps return status:incomplete.",
            "Size the output cap for reasoning output. The reference uses ~2.5x the legacy \
budget plus a 4096 floor as a STARTING heuristic, not a guaranteed value; \
tune from measured output distributions.",
        ));
    }
    if family == "legacy" && target.generation.as_deref() == Some("reasoning") {
        deltas.push(delta(
            "model_generation_hop",
            "version",
            format!(
                "Source '{source_id}' is a GPT-4.x/legacy model migrating to a reasoning target \
({target_name}): reasoning controls, token budget, and prompt behavior change."
            ),
        ));
    }

    // Sampling acceptance is a property of the TARGET model and the selected path.
    if target.sampling_parameters_supported == Some(false) {
        let remediation =
            "Remove temperature/top_p/top_k from requests to this target; do not rescale them \
or disable thinking. Route user-visible controls through the existing \
parameter_removed confirmation before changing them.";
        let sampled = finding(
            "sampling_parameters_removed",
            if sampling_detected { BLOCKS } else { TUNE },
            format!(
                "{target_name} does not support sampling controls with its required thinking mode."
            ),
            remediation,
        );
        if sampled.tag == BLOCKS {
            blocks.push(sampled);
        } else {
            tuning.push(sampled);
        }
        if sampling_detected {
            deltas.push(delta("sampling_parameters_removed", "feature", remediation));
        }
    } else if path == "runtime_converse" {
        tuning.push(finding(
            "sampling_via_converse",
            TUNE,
            format!(
                "On runtime Converse, sampling is set through inferenceConfig \
(temperature/topP) on {target_name}, not OpenAI SDK kwargs."
            ),
            "Move temperature/top_p into Converse inferenceConfig; penalties/logprobs/stop \
are not universally supported and must be verified.",
        ));
    } else {
        match target_accepts_sampling(Some(target)) {
            Some(true) => {
                tuning.push(finding(
                    "sampling_params_accepted",
                    TUNE,
                    format!(
                        "{target_name} (GPT-5.2+) accepts temperature and top_p on the Responses \
surface; do not strip them."
                    ),
                    "Keep temperature/top_p and calibrate against a golden set.",
                ));
                tuning.push(finding(
                    "sampling_penalties_rejected",
                    TUNE,
                    "frequency_penalty and presence_penalty were rejected as SDK kwargs on the \
verified GPT-5.4 Responses probe.",
                    "Remove frequency_penalty/presence_penalty. logprobs, logit_bias, and stop \
were NOT part of that probe and are version/endpoint-specific — verify each \
for the exact target model rather than assuming acceptance or rejection.",
                ));
            }
            Some(false) => tuning.push(finding(
                "sampling_params_version_specific",
                TUNE,
                format!(
                    "{target_name} predates GPT-5.2 and may reject sampling parameters on \
Responses; behavior is version-specific."
                ),
                "Verify sampling acceptance for the exact target model and surface.",
            )),
            None if target.display_name == ASTRA_NAME => tuning.push(finding(
                "sampling_params_unverified",
                TUNE,
                "Astra's model card does not specify accepted sampling parameters.",
                "Probe sampling and reasoning settings on the selected API; do not \
inherit GPT-5.x parameter acceptance or rejection rules.",
            )),
            None => {}
        }
    }
    (blocks, tuning, deltas)
}

fn chat_to_responses_deltas() -> Vec<Delta> {
    vec![
        delta(
            "chat_to_responses_request",
            "path",
            "Chat Completions -> Responses: messages->input, system message->instructions, \
max_tokens->max_output_tokens.",
        ),
        delta(
            "chat_to_responses_response",
            "path",
            "Read model output from output_text instead of the chat message content.",
        ),
        delta(
            "chat_to_responses_tools",
            "feature",
            "Tool results move to function_call_output on the Responses surface.",
        ),
        delta(
            "chat_to_responses_state",
            "path",
            "Replayed multi-turn context uses previous_response_id where appropriate, or \
application-owned state.",
        ),
    ]
}

/// The set of detected plus critical features.
fn needed_features(detected: &[String], requirements: &Requirements) -> BTreeSet<String> {
    detected
        .iter()
        .chain(&requirements.critical_features)
        .cloned()
        .collect()
}

/// `_feature_findings`: structured output, `n`, tools, state, and hosted
/// tool/modality impacts. Path-aware: Mantle Responses keeps verified
/// capabilities (typed `responses.parse` output, hosted `store=True`
/// continuation) that a Bedrock-native Converse target does not.
fn feature_findings(
    detected: &[String],
    requirements: &Requirements,
    path: &str,
    target: Option<&Model>,
) -> (Vec<Finding>, Vec<Delta>, Vec<Json>) {
    let features = needed_features(detected, requirements);
    let is_responses = path == "mantle_openai_responses";
    let is_chat = path == "mantle_openai_chat";
    let astra = target.is_some_and(|model| model.display_name == ASTRA_NAME);
    let mut tuning = Vec::new();
    let mut deltas = Vec::new();

    if features.contains("structured_output_json") {
        if path == "runtime_converse" && astra {
            deltas.push(delta(
                "structured_output_text_format",
                "feature",
                "Astra runtime JSON Schema output is documented for non-streaming calls. \
Set outputConfig.textFormat.type=json_schema and put name plus schema \
(encoded as a JSON string) in outputConfig.textFormat.structure.jsonSchema. \
Also set additionalModelRequestFields.text.format.strict=true. Use an \
object schema with all fields required and additionalProperties=false; \
validate the schema and check refusal/incomplete responses before parsing.",
            ));
        } else if is_responses {
            // Verified on Mantle: responses.parse(text_format=Model) returns a typed object.
            deltas.push(delta(
                "structured_output_text_format",
                "feature",
                "On Mantle Responses, keep typed structured output: \
client.responses.parse(..., text_format=Model) is verified, or use raw \
text.format json_schema.",
            ));
        } else {
            deltas.push(delta(
                "structured_output_text_format",
                "feature",
                "For the selected non-Responses path, enforce the schema through that \
path's supported request shape plus application validation; the OpenAI \
Responses parse() helper is not available.",
            ));
        }
    }
    if requirements.uses_n == Some(true) || features.contains("multiple_candidates_n") {
        if is_chat {
            if astra {
                tuning.push(finding(
                    "multiple_candidates_n_unverified",
                    TUNE,
                    "Astra Chat Completions support for n has not been verified.",
                    "Probe n on the selected model and endpoint before preserving it; \
if unsupported, request multiple candidates with repeated calls.",
                ));
            }
        } else {
            deltas.push(delta(
                "responses_no_n",
                "feature",
                "The Responses API does not support n; request multiple candidates with \
repeated calls.",
            ));
        }
    }
    if features.contains("tool_or_function_calling") {
        let description = if is_chat {
            "On Mantle Chat Completions, preserve assistant tool_calls and continue each \
result as a role: tool message with the matching tool_call_id; the \
application executes the tool."
        } else {
            "Tool calls continue via function_call_output on Responses; the application \
executes the tool."
        };
        deltas.push(delta("tool_result_shape", "feature", description));
    }
    if features.contains("conversation_state") || requirements.uses_hosted_state == Some(true) {
        let description = if astra {
            "Astra's hosted state behavior has not been probed on this API. Verify \
retention and continuation semantics; use application-owned history \
until those requirements are confirmed."
        } else if is_responses {
            "Two verified modes on Mantle Responses: server-hosted continuation \
(store=True + previous_response_id) or manual replay (store=False with \
application-owned history). Retention/compliance/availability are \
verification questions, not a lost capability."
        } else if is_chat {
            "Mantle Chat Completions uses application-owned message history; replay \
the prior user, assistant, tool-call, and tool-result messages on each \
request."
        } else {
            "This path does not use hosted Responses state; carry \
conversation history in an application-owned store."
        };
        deltas.push(delta("conversation_state_ownership", "path", description));
    }
    let mut impacted: Vec<&(&str, &str, &str)> = HOSTED_TOOL_IMPACTS
        .iter()
        .filter(|(feature, _, _)| features.contains(*feature))
        .collect();
    impacted.sort_by_key(|entry| entry.0);
    let impacts = impacted
        .into_iter()
        .map(|(feature, description, recommendation)| impact(feature, description, recommendation))
        .collect();
    (tuning, deltas, impacts)
}

/// Features that map to portable request-shape changes on any path.
const PORTABLE_FEATURES: [&str; 3] = ["max_tokens", "sampling_params", "sampling_params_accepted"];

/// Separate-capability modalities that need their own target, not the text
/// model: `(capability, service, evidence)`.
const SEPARATE_MODALITY_TARGETS: [(&str, &str, &str); 3] = [
    (
        "embeddings",
        "Amazon Bedrock embedding model (e.g. Titan/Cohere families)",
        "05_migrating_for_real.ipynb maps embeddings to a separate Bedrock embedding model",
    ),
    (
        "images",
        "Amazon Bedrock image model/service (e.g. Titan Image / Nova / Stability)",
        "03_reasoning_api_migration.ipynb maps images to a separate Bedrock image model",
    ),
    (
        "audio_modality",
        "Amazon Transcribe / Polly (STT/TTS)",
        "feature-catalog + notebooks map audio to Transcribe/Polly, not a text model",
    ),
];

/// `_additional_targets`: an explicit per-modality contract whose candidate
/// stays unresolved, since the reference names a service family, not a
/// verified model id.
fn additional_targets(detected: &[String], requirements: &Requirements) -> Json {
    let features = needed_features(detected, requirements);
    let mut found: Vec<&(&str, &str, &str)> = SEPARATE_MODALITY_TARGETS
        .iter()
        .filter(|(capability, _, _)| features.contains(*capability))
        .collect();
    found.sort_by(|left, right| left.0.cmp(right.0));
    Json::Array(
        found
            .into_iter()
            .map(|(capability, service, evidence)| {
                object(vec![
                    ("capability", text(capability)),
                    ("status", text("unresolved")),
                    ("candidate", Json::Null),
                    ("service", text(service)),
                    (
                        "evidence",
                        text(&format!(
                            "{evidence}; select and verify a specific model/service in the target account"
                        )),
                    ),
                ])
            })
            .collect(),
    )
}

/// `_feature_assessment`: `feature_status` is authoritative for
/// detected/absent/unknown, so an explicit `unknown` is preserved.
fn feature_assessment(workload: &Workload) -> BTreeMap<String, String> {
    let mut assessment = BTreeMap::new();
    for feature in &workload.detected_features {
        assessment.insert(feature.clone(), "detected".to_string());
    }
    for (feature, status) in &workload.feature_status {
        assessment.insert(feature.clone(), status.clone());
    }
    assessment
}

fn assessment_json(assessment: &BTreeMap<String, String>) -> Json {
    Json::Object(
        assessment
            .iter()
            .map(|(feature, status)| (feature.clone(), text(status)))
            .collect(),
    )
}

/// Features whose merged status is `detected`, sorted.
fn detected_features(assessment: &BTreeMap<String, String>) -> Vec<String> {
    assessment
        .iter()
        .filter(|(_, status)| status.as_str() == "detected")
        .map(|(feature, _)| feature.clone())
        .collect()
}

/// `_unknown_required_features`: required features whose status is unknown.
fn unknown_required_features(
    assessment: &BTreeMap<String, String>,
    requirements: &Requirements,
) -> Vec<String> {
    let required: BTreeSet<&String> = requirements.critical_features.iter().collect();
    required
        .into_iter()
        .filter(|feature| assessment.get(*feature).map(String::as_str) == Some("unknown"))
        .cloned()
        .collect()
}

const CHECKABLE_FEATURES: [&str; 5] = [
    "tool_or_function_calling",
    "structured_output_json",
    "streaming",
    "image_input_vision",
    "reasoning",
];

/// The text-model features among `features` a catalog can evidence.
fn checkable_features(model: &Model, features: &BTreeSet<String>) -> BTreeSet<String> {
    let mut checkable: BTreeSet<String> = features
        .iter()
        .filter(|feature| CHECKABLE_FEATURES.contains(&feature.as_str()))
        .cloned()
        .collect();
    if model.display_name == ASTRA_NAME && features.contains("prompt_caching") {
        checkable.insert("prompt_caching".to_string());
    }
    checkable
}

/// `_unsupported_required_capabilities`: required/detected text-model features
/// the model's catalog capabilities do not evidence.
fn unsupported_required_capabilities(
    model: &Model,
    detected: &[String],
    requirements: &Requirements,
) -> Vec<String> {
    let features = needed_features(detected, requirements);
    checkable_features(model, &features)
        .into_iter()
        .filter(|feature| !model.has_capability(feature))
        .collect()
}

/// `_compatibility`: `native` holds only features the selected target's
/// catalog evidences; needed but unevidenced ones go to `unsupported`.
fn compatibility_block(
    detected: &[String],
    requirements: &Requirements,
    target: Option<&Model>,
) -> Json {
    let features = needed_features(detected, requirements);
    let (native, unsupported) = target.map_or((Vec::new(), Vec::new()), |model| {
        let text_features = checkable_features(model, &features);
        (
            text_features
                .iter()
                .filter(|feature| model.has_capability(feature))
                .cloned()
                .collect(),
            text_features
                .iter()
                .filter(|feature| !model.has_capability(feature))
                .cloned()
                .collect(),
        )
    });
    let rearchitecture: Vec<String> = features
        .iter()
        .filter(|feature| {
            HOSTED_TOOL_IMPACTS
                .iter()
                .any(|(known, _, _)| known == feature)
        })
        .cloned()
        .collect();
    let portable: Vec<String> = features
        .iter()
        .filter(|feature| PORTABLE_FEATURES.contains(&feature.as_str()))
        .cloned()
        .collect();
    compatibility(&native, &portable, &rearchitecture, &unsupported)
}

fn evaluation(detected: &[String], requirements: &Requirements) -> Json {
    let features = needed_features(detected, requirements);
    let trajectory = [
        "tool_or_function_calling",
        "assistants_threads",
        "web_search",
        "file_search_retrieval",
    ]
    .iter()
    .any(|feature| features.contains(*feature))
        || requirements.has_critical("agentic");
    let mut gates = vec![
        "Build a deterministic discovery inventory of clients, APIs, model IDs, and features.",
        "Compare viable path/model candidates on a representative golden set.",
        "Size quota for peak traffic and reasoning-output tokens.",
        "Fail on refusal mishandling, truncation, or invalid structured output.",
    ];
    if trajectory {
        gates.extend([
            "Verify correct tool selection, valid tool arguments, and tool-result continuation.",
            "Verify loop termination and structured-output validity.",
        ]);
    }
    findings::evaluation(trajectory, &gates)
}

/// `_verification`: an unselected target is `not_applicable`; a selected one is
/// a provisional, not-yet-probed claim.
fn verification(
    region: &str,
    catalog: &Catalog,
    selection: Option<(&Hit, &str, Option<&str>, &Requirements)>,
) -> Json {
    let Some((hit, path, invocation, requirements)) = selection else {
        return object(vec![
            ("region", text(region)),
            ("catalog_verified_at", text(&catalog.verified_at)),
            ("verified_at", Json::Null),
            ("probe_status", text("not_applicable")),
            ("availability_claim", text("not_selected")),
            ("invocation_model_id", Json::Null),
            (
                "required_checks",
                strings(["Resolve the model/path decision before running an availability probe."]),
            ),
        ]);
    };
    let path_model_id = hit
        .model
        .path(path)
        .map_or("", |config| config.model_id.as_str());
    let requires_cris = hit.path_config.requires_cris && invocation != Some(path_model_id);
    let mut checks = vec![
        "Probe the selected model through the selected API path in the target account and region.",
        "Verify path-specific IAM, model access, and quota before code rewrite or POC generation.",
    ];
    if requires_cris {
        checks.insert(
            1,
            "Resolve and probe a Global or geography-scoped CRIS inference profile.",
        );
    }
    let mut members = vec![
        ("region", text(region)),
        ("catalog_verified_at", text(&catalog.verified_at)),
        ("verified_at", Json::Null),
        ("probe_status", text("not_run")),
        ("availability_claim", text("provisional")),
        ("invocation_model_id", optional_text(invocation)),
        ("required_checks", strings(checks)),
    ];
    members.extend(profile_verification(
        path_model_id,
        requires_cris,
        Some(&hit.model),
        region,
        requirements,
    ));
    object(members)
}

/// `_decision_options`: the two-sided option set, the selected Mantle API
/// against runtime Converse governance.
#[allow(clippy::too_many_lines)]
fn decision_options(catalog: &Catalog, workload: &Workload, region: &str) -> Vec<Json> {
    let requirements = &workload.requirements;
    let source = &workload.source;
    let assessment = feature_assessment(workload);
    let detected = detected_features(&assessment);
    let astra = same_model_catalog_key(source) == Some(ASTRA_KEY);
    let mut mantle_path_name = mantle_path(source, requirements);
    if astra {
        match requirements.preferred_api_path.as_deref() {
            Some(preferred @ ("mantle_openai_chat" | "mantle_openai_responses")) => {
                mantle_path_name = if preferred == "mantle_openai_chat" {
                    "mantle_openai_chat"
                } else {
                    "mantle_openai_responses"
                };
            }
            _ if source.api_surface == "chat_completions" => {
                mantle_path_name = "mantle_openai_chat";
            }
            _ => {}
        }
        // A decision may offer changing an incompatible Chat preference to the
        // documented Responses caching path; it must not silently select it.
        if needed_features(&detected, requirements).contains("prompt_caching") {
            mantle_path_name = mantle_path(source, requirements);
        }
    }
    let first_order: Vec<String> = if astra {
        vec![ASTRA_KEY.to_string()]
    } else {
        mantle_candidate_order(catalog, source)
    };
    let (mut mantle, _) = catalog_model_for_path(
        catalog,
        mantle_path_name,
        &detected,
        requirements,
        Some(&first_order),
        region,
    );
    let mut requires_region_change = false;
    if mantle.is_none() && region != "unknown" {
        let order = mantle_candidate_order(catalog, source);
        (mantle, _) = catalog_model_for_path(
            catalog,
            mantle_path_name,
            &detected,
            requirements,
            Some(&order),
            "unknown",
        );
        requires_region_change = mantle.is_some();
    }
    let mut options = Vec::new();
    if let Some(hit) = mantle {
        let mut reason = if mantle_path_name == "mantle_openai_chat" {
            "Preserves the OpenAI SDK and Chat Completions surface; gives up \
runtime-only Bedrock governance."
        } else {
            "Preserves the OpenAI SDK and Responses surface; gives up \
runtime-only Bedrock governance."
        }
        .to_string();
        if requires_region_change {
            let supported: BTreeSet<&String> = hit
                .model
                .region_availability
                .iter()
                .flat_map(|(commercial, govcloud)| commercial.iter().chain(govcloud))
                .collect();
            let label = if supported.is_empty() {
                "a catalog-supported region".to_string()
            } else {
                supported
                    .into_iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = write!(
                reason,
                " Requires changing from {region} to a catalog-supported Mantle region ({label})."
            );
        }
        let invocation = resolve_invocation_model_id(
            &hit.path_config.model_id,
            hit.path_config.requires_cris,
            requirements,
            Some(&hit.model),
            region,
        );
        options.push(candidate(
            &hit.key,
            &hit.path_config.model_id,
            mantle_path_name,
            invocation.as_deref(),
            hit.path_config.requires_cris,
            &reason,
        ));
    }
    let order: Vec<String> = converse_candidate_order(source, Some(requirements))
        .into_iter()
        .map(str::to_string)
        .collect();
    let (runtime, _) = catalog_model_for_path(
        catalog,
        "runtime_converse",
        &detected,
        requirements,
        Some(&order),
        region,
    );
    if let Some(hit) = runtime {
        let invocation = resolve_invocation_model_id(
            &hit.path_config.model_id,
            hit.path_config.requires_cris,
            requirements,
            Some(&hit.model),
            region,
        );
        let reason = if Some(hit.key.as_str()) == same_model_catalog_key(source) {
            "SAME-MODEL governance path: this OpenAI target runs on bedrock-runtime via a \
CRIS id — Guardrails (Converse API only) and invocation logging without a \
model change. Check the model's region/profile matrix and dated prices."
        } else {
            "Uses Bedrock-native Converse request/response shapes and a \
cross-family model; requires rewriting the OpenAI integration."
        };
        options.push(candidate(
            &hit.key,
            &hit.path_config.model_id,
            "runtime_converse",
            invocation.as_deref(),
            invocation_requires_cris(&hit.path_config, invocation.as_deref()),
            reason,
        ));
    }
    options
}

/// The common head of a recommendation (`_base`).
fn base(workload: &Workload, status: &str) -> Vec<(&'static str, Json)> {
    vec![
        ("workload_id", text(&workload.id)),
        ("provider_module", text("openai")),
        ("decision_status", text(status)),
        ("source", workload.source.json.clone()),
    ]
}

/// The state `_unresolved` of the Python engine closed over.
struct Context<'a> {
    workload: &'a Workload,
    region: &'a str,
    catalog: &'a Catalog,
    assessment: &'a BTreeMap<String, String>,
    detected: &'a [String],
}

impl Context<'_> {
    /// The shared `decision_required` assembly (no target selected).
    fn unresolved(
        &self,
        head: &str,
        reasons: Vec<String>,
        block: &Finding,
        compat_path: &str,
    ) -> Json {
        let requirements = &self.workload.requirements;
        let mut rationale = vec![head.to_string()];
        rationale.extend(reasons);
        let impacts = feature_findings(self.detected, requirements, compat_path, None).2;
        let mut members = base(self.workload, "decision_required");
        members.extend([
            (
                "source_analysis",
                source_analysis(&self.workload.source, None).json(),
            ),
            ("feature_assessment", assessment_json(self.assessment)),
            ("primary_model", Json::Null),
            ("model_identity", Json::Null),
            ("api_path", Json::Null),
            ("invocation_model_id", Json::Null),
            (
                "decision_options",
                Json::Array(decision_options(self.catalog, self.workload, self.region)),
            ),
            ("alternatives", Json::Array(Vec::new())),
            ("rationale", strings(&rationale)),
            ("blocks", findings_json(std::slice::from_ref(block))),
            ("tuning", Json::Array(Vec::new())),
            (
                "compatibility",
                compatibility_block(self.detected, requirements, None),
            ),
            ("architecture_impacts", Json::Array(impacts)),
            (
                "additional_targets",
                additional_targets(self.detected, requirements),
            ),
            ("migration_deltas", Json::Array(Vec::new())),
            ("evaluation", evaluation(self.detected, requirements)),
            (
                "rollout",
                rollout(
                    "decision_required",
                    "Resolve the open decision before implementation.",
                ),
            ),
            (
                "verification",
                verification(self.region, self.catalog, None),
            ),
        ]);
        object(members)
    }
}

/// `recommend_openai_workload`: a recommendation in the shared downstream
/// contract shape.
///
/// # Errors
///
/// A catalog that lacks a model the selection tables name, or has no available
/// model for a Converse path.
#[allow(clippy::too_many_lines)]
pub(super) fn recommend_workload(
    workload: &Workload,
    region: &str,
    catalog: &Catalog,
) -> Res<Json> {
    let source = &workload.source;
    let requirements = &workload.requirements;
    let surface = source.api_surface.as_str();
    let continuity_required = api_continuity(requirements) == "required";
    let runtime_needed = runtime_required(requirements);

    let assessment = feature_assessment(workload);
    // feature_status is authoritative: a feature is "effectively detected" only
    // if the merged assessment marks it detected, so compatibility, deltas and
    // targets never contradict the assessment.
    let detected = detected_features(&assessment);
    let unknown_required = unknown_required_features(&assessment, requirements);
    let context = Context {
        workload,
        region,
        catalog,
        assessment: &assessment,
        detected: &detected,
    };

    // Fail closed: a required feature with unknown status is not ready.
    if !unknown_required.is_empty() {
        let listed = unknown_required.join(", ");
        return Ok(context.unresolved(
            &format!("Required features are unresolved (status unknown): {listed}"),
            vec![
                "The scan did not confirm these required features as detected or absent, so \
readiness cannot be asserted."
                    .to_string(),
            ],
            &finding(
                "feature_scan_incomplete",
                BLOCKS,
                format!("Required features are unresolved (status unknown): {listed}"),
                "Scan the recorded source paths and mark each required feature detected or \
absent, then rerun Model Recommend.",
            ),
            "",
        ));
    }

    // Hard conflict: OpenAI continuity required AND runtime-only governance.
    if continuity_required && runtime_needed {
        let options = decision_options(catalog, workload, region);
        if options.len() < 2 {
            return Err(ModelError::new(format!(
                "catalog cannot supply both decision options for workload {}",
                safe_text(&workload.id)
            )));
        }
        return Ok(context.unresolved(
            "Required OpenAI API continuity conflicts with a runtime-only Bedrock \
governance requirement.",
            vec![
                "Mantle preserves the OpenAI SDK and the supported source API surface; runtime \
Converse provides Bedrock-native governance and keeps an exact same-model \
runtime candidate when the catalog provides one."
                    .to_string(),
            ],
            &finding(
                "model_path_decision_required",
                BLOCKS,
                "OpenAI API continuity and runtime-only governance cannot both be satisfied \
by one path.",
                "Choose the Mantle continuity option or the runtime governance option, \
update requirements, and rerun Model Recommend.",
            ),
            "",
        ));
    }

    // Select a path.
    let mut path: String;
    let mut candidate_order: Vec<String>;
    let mut rationale_head: String;
    if runtime_needed {
        path = "runtime_converse".to_string();
        candidate_order = converse_candidate_order(source, Some(requirements))
            .into_iter()
            .map(str::to_string)
            .collect();
        rationale_head = if let Some(same) = same_model_catalog_key(source) {
            format!(
                "Bedrock governance or multi-model requirements select runtime Converse; \
the source model itself ({}) runs there via a CRIS id, so the same-model \
candidate leads, with the Claude tier as the cross-family fallback.",
                catalog.require(same)?.display_name
            )
        } else {
            format!(
                "Bedrock governance or multi-model requirements select runtime Converse; \
the source tier maps to {} (capability-evidence fallback across Claude tiers). \
Sources without an exact runtime catalog identity require this family switch.",
                catalog
                    .require(converse_tier_for_source(source))?
                    .display_name
            )
        };
    } else {
        path = mantle_path(source, requirements).to_string();
        candidate_order = mantle_candidate_order(catalog, source);
        rationale_head = if path == "mantle_openai_chat" {
            "GPT-6 Chat Completions API continuity requires an in-region Mantle Chat \
Completions model."
        } else {
            "OpenAI source lands on Mantle Responses, with the exact source model \
evaluated before capability and numeric fallbacks."
        }
        .to_string();
    }

    // Astra's new paths must not inherit the older GPT-5.x defaults. Keep this
    // source on Astra and fail closed when the required path lacks evidence.
    if same_model_catalog_key(source) == Some(ASTRA_KEY) {
        candidate_order = vec![ASTRA_KEY.to_string()];
        if let Some(preferred) = requirements.preferred_api_path.as_deref() {
            let available = catalog
                .require(ASTRA_KEY)?
                .path(preferred)
                .is_some_and(|config| config.available);
            if !available
                || (runtime_needed && preferred != "runtime_converse")
                || (continuity_required && preferred == "runtime_converse")
            {
                return Ok(context.unresolved(
                    "The explicit Astra API preference conflicts with the available paths \
or required API/governance constraints.",
                    Vec::new(),
                    &finding(
                        "preferred_api_path_conflict",
                        BLOCKS,
                        format!("The requested {preferred} path cannot satisfy this workload."),
                        "Choose a supported API or revise the conflicting requirements, \
then rerun Model Recommend.",
                    ),
                    preferred,
                ));
            }
            path = preferred.to_string();
        } else if !runtime_needed && surface == "chat_completions" {
            path = "mantle_openai_chat".to_string();
        }
        rationale_head =
            "Keep GPT-6 Astra on Bedrock using the compatible explicit API preference, \
otherwise the source-compatible API or required runtime Converse path; \
apply Astra's own region and feature evidence."
                .to_string();
    }

    // Evidence-driven selection: the first candidate on the path whose catalog
    // capabilities cover the required/detected text features (a newer model
    // without feature-level evidence falls back to an older probed one). Fail
    // closed only when NO candidate on the path has the evidence.
    let (hit, unsupported) = catalog_model_for_path(
        catalog,
        &path,
        &detected,
        requirements,
        Some(&candidate_order),
        region,
    );
    let Some(hit) = hit else {
        if path.starts_with("mantle_") {
            return Ok(context.unresolved(
                &rationale_head,
                vec![format!(
                    "The catalog has no {path} candidate available in {region}; \
select another region or migration path."
                )],
                &finding(
                    "mantle_region_unavailable",
                    BLOCKS,
                    format!("No in-region {path} model is available in {region}."),
                    "Choose a region in the selected model's Mantle footprint, choose \
runtime Converse, or update the continuity requirement, then rerun \
Model Recommend.",
                ),
                &path,
            ));
        }
        return Err(ModelError::new(format!(
            "catalog has no available model for path {}",
            safe_text(&path)
        )));
    };
    let model_key = hit.key.as_str();
    let model = &hit.model;
    let path_config = &hit.path_config;
    if path == "mantle_openai_chat" {
        rationale_head = if Some(model_key) == same_model_catalog_key(source) {
            "GPT-6 Chat Completions source with API continuity lands on the exact \
same model through Mantle Chat Completions."
                .to_string()
        } else {
            format!(
                "The exact source model is unavailable in {region}; {} is the in-region \
Mantle Chat Completions fallback.",
                model.display_name
            )
        };
    }
    if !unsupported.is_empty() {
        let listed = unsupported.join(", ");
        return Ok(context.unresolved(
            &rationale_head,
            vec![format!(
                "No {path} candidate has catalog capability evidence for: {listed}"
            )],
            &finding(
                "unverified_capability",
                BLOCKS,
                format!("No {path} candidate is evidenced to support required features: {listed}"),
                "Add dated capability evidence for a candidate, choose a different path/model, \
or drop the requirement, then rerun Model Recommend.",
            ),
            &path,
        ));
    }

    let needed = needed_features(&detected, requirements);
    if model_key == ASTRA_KEY
        && path == "runtime_converse"
        && needed.contains("structured_output_json")
        && needed.contains("streaming")
    {
        return Ok(context.unresolved(
            &rationale_head,
            Vec::new(),
            &finding(
                "structured_output_streaming_unverified",
                BLOCKS,
                "Astra runtime JSON Schema output is documented only for non-streaming calls.",
                "Use non-streaming structured output or separate the streaming workload, \
then update requirements and rerun Model Recommend.",
            ),
            &path,
        ));
    }

    let numeric_conflicts = numeric_requirement_conflict(model, requirements);
    if !numeric_conflicts.is_empty() {
        return Ok(context.unresolved(
            &rationale_head,
            numeric_conflicts.clone(),
            &finding(
                "unverified_capacity",
                BLOCKS,
                numeric_conflicts.join("; "),
                "Source the missing capability data from a dated reference, or reduce the \
hard numeric requirement, then rerun Model Recommend.",
            ),
            &path,
        ));
    }

    let invocation = resolve_invocation_model_id(
        &path_config.model_id,
        path_config.requires_cris,
        requirements,
        Some(model),
        region,
    );
    if let Some(regions) = &path_config.supported_regions {
        if !regions.iter().any(|known| known == region) {
            return Ok(context.unresolved(
                &rationale_head,
                Vec::new(),
                &finding(
                    "model_region_unavailable",
                    BLOCKS,
                    format!(
                        "{} is not listed for {path} in {region}.",
                        model.display_name
                    ),
                    "Choose a supported region or a supported CRIS path with explicit \
data-residency consent, or evaluate a different model.",
                ),
                &path,
            ));
        }
    }
    if let Some(profile_regions) = &path_config.inference_profile_regions {
        if invocation.is_some() || hard_profile_requirement(requirements) {
            let profile_key = invocation.as_deref().and_then(|profile| {
                if profile.starts_with("arn:") {
                    arn_profile_key(profile, region)
                } else {
                    Some(profile.to_string())
                }
            });
            let callable = profile_key.is_some_and(|key| {
                profile_regions
                    .iter()
                    .find(|(name, _)| *name == key)
                    .is_some_and(|(_, regions)| regions.iter().any(|known| known == region))
            });
            if !callable {
                return Ok(context.unresolved(
                    &rationale_head,
                    Vec::new(),
                    &finding(
                        "inference_profile_unverified",
                        BLOCKS,
                        format!(
                            "{} has no catalog evidence for calls from {region}.",
                            invocation.as_deref().unwrap_or("None")
                        ),
                        "Select a cataloged profile available from this region, or verify an \
application inference profile in the target account before proceeding.",
                    ),
                    &path,
                ));
            }
        }
    }
    let analysis = source_analysis(source, Some(model));

    // Findings (target- and path-derived).
    let (mut blocks, mut tuning, reasoning_deltas) =
        reasoning_findings(source, model, &path, &detected);
    let (feature_tuning, feature_deltas, impacts) =
        feature_findings(&detected, requirements, &path, Some(model));
    tuning.extend(feature_tuning);
    let mut deltas = reasoning_deltas;
    if model.adaptive_thinking_only {
        let adaptive = finding(
            "adaptive_thinking_required",
            if requirements.thinking_enabled == Some(false) {
                BLOCKS
            } else {
                TUNE
            },
            "Opus 5.5 always uses adaptive thinking and does not support disabled thinking, manual budgets, or forced any/tool choice.",
            "Set output_config.effort as needed (default medium), and size max_tokens \
for thinking plus response text. Use auto/none tool choice; a hard forced-tool \
requirement needs a verified compatible target. Verify billed usage on the workload.",
        );
        if adaptive.tag == BLOCKS {
            blocks.push(adaptive);
        } else {
            tuning.push(adaptive);
        }
    }

    if path == "runtime_converse" && path_config.requires_cris && invocation.is_none() {
        // GPT-5.6 on bedrock-runtime is CRIS-only: there is no in-region
        // invocation form. A residency posture that forbids both Global and Geo
        // CRIS makes the same-model governance path unusable; say so explicitly
        // instead of shipping a recommendation with an unresolvable id.
        blocks.push(finding(
            "cris_residency_unresolved",
            BLOCKS,
            "This runtime path is CRIS-only, and the stated data-residency posture \
permits neither Global nor Geo cross-region inference, so no invocation \
id can be resolved.",
            "Relax residency to Global or a Geo CRIS geography, supply an explicit \
inference_profile_id, or take the Claude cross-family Converse path.",
        ));
    }

    if path == "mantle_openai_responses" && surface == "chat_completions" {
        deltas.extend(chat_to_responses_deltas());
        blocks.push(finding(
            "chat_completions_to_responses_required",
            BLOCKS,
            if model_key == ASTRA_KEY {
                "The selected Astra Responses path requires reshaping the Chat source."
            } else {
                "GPT-5.x on Mantle rejects Chat Completions; the source must reshape \
to the Responses API."
            },
            "Apply the request/response/tool/state reshape deltas before cutover.",
        ));
    }
    if path == "mantle_openai_chat" && surface == "responses" {
        deltas.extend([
            delta(
                "responses_to_chat_request",
                "path",
                "Map Responses input/instructions to Chat messages/system content and \
verify the selected model's output-budget parameters.",
            ),
            delta(
                "responses_to_chat_response",
                "path",
                "Read choices[0].message.content instead of output_text; preserve Chat \
messages and tool_call_id rather than Responses output items.",
            ),
        ]);
        blocks.push(finding(
            "responses_to_chat_completions_required",
            BLOCKS,
            "The selected Astra Chat path requires reshaping the Responses source.",
            "Apply the request/response/tool/state mapping before cutover.",
        ));
    }
    deltas.extend(feature_deltas);

    if path == "runtime_converse" {
        let same_model = Some(model_key) == same_model_catalog_key(source);
        deltas.push(delta(
            "openai_sdk_to_converse",
            "path",
            format!(
                "The OpenAI SDK integration is rewritten to boto3 bedrock-runtime Converse; {}",
                if same_model {
                    "the selected OpenAI model identity is preserved, while streaming moves \
to ConverseStream event shapes."
                } else {
                    "the selected model also changes families, and streaming moves to \
ConverseStream event shapes."
                }
            ),
        ));
    }

    let mut rationale = vec![rationale_head];
    if model.context_window == super::catalog::Limit::Unknown
        || model.output_token_ceiling == super::catalog::Limit::Unknown
    {
        rationale.push(format!(
            "{} path is evidenced by the reference, but its context/output \
limits are unknown in the catalog and must be probed.",
            model.display_name
        ));
    }
    let mut members = base(workload, "recommended");
    members.extend([
        ("source_analysis", analysis.json()),
        ("feature_assessment", assessment_json(&assessment)),
        ("primary_model", text(&path_config.model_id)),
        (
            "model_identity",
            model_identity(model_key, model, path_config, invocation.as_deref()),
        ),
        ("api_path", text(&path)),
        ("invocation_model_id", optional_text(invocation.as_deref())),
        ("decision_options", Json::Array(Vec::new())),
        ("alternatives", Json::Array(Vec::new())),
        ("rationale", strings(&rationale)),
        ("blocks", findings_json(&blocks)),
        ("tuning", findings_json(&tuning)),
        (
            "compatibility",
            compatibility_block(&detected, requirements, Some(model)),
        ),
        ("architecture_impacts", Json::Array(impacts)),
        (
            "additional_targets",
            additional_targets(&detected, requirements),
        ),
        ("migration_deltas", deltas_json(&deltas)),
        ("evaluation", evaluation(&detected, requirements)),
        (
            "rollout",
            rollout(
                "canary",
                "Compare source and target on the golden set before percentage rollout.",
            ),
        ),
        (
            "verification",
            verification(
                region,
                catalog,
                Some((&hit, &path, invocation.as_deref(), requirements)),
            ),
        ),
    ]);
    Ok(object(members))
}

/// The profile name of a same-account inference-profile ARN callable from
/// `region` (`arn:aws:bedrock:<region>:<12 digits>:inference-profile/<name>`).
fn arn_profile_key(arn: &str, region: &str) -> Option<String> {
    static EXPRESSION: LazyLock<Regex> = LazyLock::new(|| {
        pattern(r"\Aarn:aws:bedrock:([^:]+):[0-9]{12}:inference-profile/([^/]+)\z")
    });
    let found = EXPRESSION.captures(arn).ok().flatten()?;
    (group(&found, 1) == region).then(|| group(&found, 2).to_string())
}
