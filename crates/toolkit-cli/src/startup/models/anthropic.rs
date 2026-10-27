//! Deterministic Anthropic-to-Bedrock model/path recommendation
//! (`anthropic_model_recommendation.py`): joint (model, API path) filtering and
//! ranking over the dated catalog, Claude version-hop analysis, CRIS
//! resolution without guessing a geography and the shared recommendation
//! contract. Offline: no SDK, no network.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use fancy_regex::Regex;
use toolkit_core::Json;

use super::catalog::{Catalog, Model, PathConfig};
use super::findings::{
    self, candidate, compatibility, delta, deltas_json, finding, findings_json, impact, object,
    optional_text, rollout, strings, text, Delta, Finding, BLOCKS, TUNE,
};
use super::request::{Requirements, Source, Workload};
use super::{ModelError, Res};
use crate::safe_text::safe_text;

/// Every API path a requirement may name (`SUPPORTED_PATHS`).
pub(super) const SUPPORTED_PATHS: [&str; 5] = [
    "mantle_messages",
    "mantle_openai_chat",
    "mantle_openai_responses",
    "runtime_converse",
    "runtime_invoke",
];

/// Paths of an Anthropic Messages source (`ANTHROPIC_PATHS`).
const ANTHROPIC_PATHS: [&str; 3] = ["mantle_messages", "runtime_converse", "runtime_invoke"];

/// Sources this engine reasons about (`ANTHROPIC_POOL`). `none` and `unknown`
/// belong here: a source with no detected provider is treated as
/// Bedrock-native. Any other provider routed here gets a recommendation that
/// carries `provider_module_pending` and stays provisional.
pub(super) const ANTHROPIC_POOL: [&str; 3] = ["anthropic", "none", "unknown"];

/// New migrations use Opus 5.5; older Opus models are not automatic fallbacks.
pub(crate) const PRIORITY_ORDER: [(&str, [&str; 3]); 5] = [
    (
        "quality",
        ["claude_opus_5_5", "claude_sonnet_5", "claude_haiku_4_5"],
    ),
    (
        "balanced",
        ["claude_sonnet_5", "claude_opus_5_5", "claude_haiku_4_5"],
    ),
    (
        "speed",
        ["claude_haiku_4_5", "claude_sonnet_5", "claude_opus_5_5"],
    ),
    (
        "cost",
        ["claude_haiku_4_5", "claude_sonnet_5", "claude_opus_5_5"],
    ),
    (
        "unknown",
        ["claude_sonnet_5", "claude_opus_5_5", "claude_haiku_4_5"],
    ),
];

const AGENTIC_ORDER: [&str; 3] = ["claude_sonnet_5", "claude_opus_5_5", "claude_haiku_4_5"];

const MODEL_CAPABILITIES: [&str; 4] = ["extended_thinking", "long_context", "tool_use", "vision"];

pub(super) const VERSION_SCAN_FEATURES: [&str; 6] = [
    "assistant_prefill",
    "budget_tokens",
    "max_tokens_headroom",
    "refusal_handling",
    "sampling_parameters",
    "tokenizer_rebaseline",
];

const REARCHITECTURE_FEATURES: [&str; 7] = [
    "agent_infra",
    "conversation_state",
    "fallbacks",
    "files_api",
    "message_batches",
    "server_tools",
    "url_sources",
];

/// `(feature, code, message, remediation)` of the `[BLOCKS]` findings.
const BLOCK_FINDINGS: [(&str, &str, &str, &str); 11] = [
    (
        "budget_tokens",
        "budget_tokens_removed",
        "`budget_tokens` is rejected by current Claude targets.",
        "Replace it with adaptive thinking and an explicit effort setting.",
    ),
    (
        "sampling_parameters",
        "sampling_parameters_removed",
        "Legacy temperature/top_p/top_k controls are rejected by current Claude targets.",
        "Remove them and calibrate behavior with the golden set.",
    ),
    (
        "assistant_prefill",
        "assistant_prefill_removed",
        "Assistant-prefill structured output is rejected by current Claude targets.",
        "Use a forced tool without strict and validate the resulting schema.",
    ),
    (
        "server_tools",
        "server_tools_not_portable",
        "First-party server tools are not provided by the selected Bedrock path.",
        "Implement an application-owned tool loop or select an external service.",
    ),
    (
        "files_api",
        "files_api_not_portable",
        "The first-party Files API is not available on Bedrock.",
        "Inline supported content within the 20 MB request cap or add object retrieval.",
    ),
    (
        "url_sources",
        "url_sources_not_portable",
        "First-party URL image and document sources are not available on Bedrock.",
        "Fetch, validate, and inline the content from the application.",
    ),
    (
        "message_batches",
        "message_batches_not_portable",
        "Anthropic Message Batches do not port directly to Bedrock.",
        "Use CreateModelInvocationJob and redesign job submission and result handling.",
    ),
    (
        "models_api",
        "models_api_not_portable",
        "The first-party Models API is not available on Bedrock.",
        "Use Bedrock model discovery and keep runtime invocability as a separate probe.",
    ),
    (
        "fallbacks",
        "fallbacks_not_portable",
        "Server-side model fallbacks are not available on the selected Bedrock path.",
        "Implement explicit client-side routing, retry limits, and fallback observability.",
    ),
    (
        "conversation_state",
        "conversation_state_not_portable",
        "Server-side conversation state is not available on the selected Bedrock path.",
        "Persist and resend conversation history from an application-owned store.",
    ),
    (
        "agent_infra",
        "agent_infra_rearchitecture",
        "First-party Anthropic agent infrastructure is not a portable inference-call feature.",
        "Redesign Skills, MCP connectors, and managed-agent dependencies on AWS.",
    ),
];

/// `(feature, code, message, remediation)` of the `[TUNE]` findings.
const TUNE_FINDINGS: [(&str, &str, &str, &str); 4] = [
    (
        "refusal_handling",
        "refusal_handling",
        "Current Claude targets can return stop_reason=refusal.",
        "Add explicit refusal handling and include refusal cases in evaluation.",
    ),
    (
        "tokenizer_rebaseline",
        "tokenizer_rebaseline",
        "Token counts change across Claude versions and Bedrock paths.",
        "Re-baseline context sizing, usage accounting, and truncation alerts.",
    ),
    (
        "max_tokens_headroom",
        "max_tokens_headroom",
        "Thinking tokens share max_tokens and framework defaults can truncate output.",
        "Preserve source intent and tune from output distributions and thinking headroom.",
    ),
    (
        "prompt_caching",
        "prompt_cache_validation",
        "Prompt caching is supported but cache-hit behavior must not be assumed.",
        "Verify writes, reads, TTL, minimum-token thresholds, and hit telemetry.",
    ),
];

fn block_entry(
    feature: &str,
) -> Option<&'static (&'static str, &'static str, &'static str, &'static str)> {
    BLOCK_FINDINGS.iter().find(|entry| entry.0 == feature)
}

fn to_vec(set: &BTreeSet<String>) -> Vec<String> {
    set.iter().cloned().collect()
}

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("a valid built-in pattern")
}

static VERSION_PAIR: LazyLock<Regex> = LazyLock::new(|| pattern(r"([0-9]+)\.([0-9]+)"));
static SOURCE_PATTERNS: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        pattern(r"claude-(?:opus-|sonnet-|haiku-)?([0-9]+)[.-]([0-9]+)"),
        pattern(r"claude-([0-9]+)[.-]([0-9]+)-(?:opus|sonnet|haiku)"),
    ]
});
static SOURCE_MAJOR: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"claude-(?:opus|sonnet|haiku)-([0-9]+)(?:-latest)?(?=\n?\z)"));
static INFERENCE_PROFILE_ARN: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"\Aarn:(aws(?:-[a-z-]+)?):bedrock:([a-z0-9-]+):[0-9]{12}:(inference-profile|application-inference-profile)/([a-zA-Z0-9:.-]+)\z",
    )
});

/// Capture group `index` of a match (empty when the group did not take part).
pub(super) fn group<'t>(captures: &fancy_regex::Captures<'t, str>, index: usize) -> &'t str {
    captures.get(index).map_or("", |found| found.as_str())
}

/// `str(int(digits))` for a run of ASCII digits of any length.
fn decimal_text(digits: &str) -> String {
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }.to_string()
}

/// A version number component, saturating at `u64::MAX`.
fn component(digits: &str) -> u64 {
    digits.parse().unwrap_or(u64::MAX)
}

/// `_version_tuple`: the first `major.minor` pair of `value`.
pub(super) fn version_tuple(value: &str) -> Option<(u64, u64)> {
    let captures = VERSION_PAIR.captures(value).ok().flatten()?;
    Some((
        component(group(&captures, 1)),
        component(group(&captures, 2)),
    ))
}

/// `_source_version`: the Claude version a source model id names.
pub(super) fn source_version(source: &Source) -> Option<String> {
    for model_id in &source.model_ids {
        let normalized = model_id.to_lowercase().replace('_', "-");
        for expression in SOURCE_PATTERNS.iter() {
            if let Ok(Some(found)) = expression.captures(normalized.as_str()) {
                return Some(format!(
                    "{}.{}",
                    decimal_text(group(&found, 1)),
                    decimal_text(group(&found, 2))
                ));
            }
        }
        if let Ok(Some(major)) = SOURCE_MAJOR.captures(normalized.as_str()) {
            return Some(format!("{}.0", decimal_text(group(&major, 1))));
        }
    }
    None
}

fn candidate_order(requirements: &Requirements) -> (Vec<&'static str>, String) {
    if requirements.has_critical("agentic") {
        return (AGENTIC_ORDER.to_vec(), "agentic".to_string());
    }
    let priority = if requirements.priority.is_empty() {
        "unknown"
    } else {
        requirements.priority.as_str()
    };
    let order = PRIORITY_ORDER
        .iter()
        .find(|(name, _)| *name == priority)
        .map_or(PRIORITY_ORDER[4].1, |(_, order)| *order);
    (order.to_vec(), priority.to_string())
}

fn required_capabilities(requirements: &Requirements) -> BTreeSet<&'static str> {
    let mut capabilities = BTreeSet::new();
    for feature in &requirements.critical_features {
        let name = if feature == "multimodal" {
            "vision"
        } else {
            feature.as_str()
        };
        if let Some(known) = MODEL_CAPABILITIES.iter().find(|known| **known == name) {
            capabilities.insert(*known);
        }
    }
    if requirements.thinking_enabled == Some(true) {
        capabilities.insert("extended_thinking");
    }
    capabilities
}

/// The API-path decision of `_path_constraints`.
#[derive(Debug)]
pub(super) struct Constraints {
    pub(super) paths: Vec<String>,
    pub(super) conflicts: Vec<&'static str>,
    pub(super) option_paths: Vec<String>,
    pub(super) rationale: Vec<String>,
}

fn fixed(paths: &[&str], rationale: &str) -> Constraints {
    Constraints {
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        conflicts: Vec::new(),
        option_paths: Vec::new(),
        rationale: vec![rationale.to_string()],
    }
}

pub(super) fn path_constraints(workload: &Workload) -> Res<Constraints> {
    let source = &workload.source;
    let requirements = &workload.requirements;
    let preferred = requirements.preferred_api_path.as_deref();
    if let Some(path) = preferred {
        if !SUPPORTED_PATHS.contains(&path) {
            return Err(ModelError::new(
                "requirement preferred_api_path is not a supported API path",
            ));
        }
    }
    let runtime_required =
        !requirements.governance.is_empty() || requirements.multi_model_converse == Some(true);
    let native_required = requirements.requires_native_payload == Some(true);
    let messages_required = requirements.preserve_messages_api == Some(true)
        || requirements.newest_anthropic_betas == Some(true);
    let source_messages = source.provider == "anthropic"
        && source.api_surface == "messages"
        && requirements.preserve_messages_api != Some(false);

    let runtime_path = if native_required {
        "runtime_invoke"
    } else {
        "runtime_converse"
    };
    let mut conflicts = BTreeSet::new();
    if messages_required && (runtime_required || native_required) {
        conflicts.insert("messages_vs_runtime");
    }
    if let Some(path) = preferred {
        if path.starts_with("mantle_") && (runtime_required || native_required) {
            conflicts.insert("preferred_path_vs_runtime");
        }
        if path.starts_with("runtime_") && messages_required {
            conflicts.insert("preferred_path_vs_messages");
        }
        if native_required && path != "runtime_invoke" {
            conflicts.insert("preferred_path_vs_native_payload");
        }
    }
    if !conflicts.is_empty() {
        return Ok(Constraints {
            paths: Vec::new(),
            conflicts: conflicts.into_iter().collect(),
            option_paths: vec!["mantle_messages".to_string(), runtime_path.to_string()],
            rationale: vec![
                "Messages continuity or beta requirements conflict with required Bedrock runtime capabilities."
                    .to_string(),
            ],
        });
    }
    if let Some(path) = preferred {
        return Ok(fixed(
            &[path],
            &format!("User selected the {path} API path."),
        ));
    }
    if native_required {
        return Ok(fixed(
            &["runtime_invoke"],
            "InvokeModel is required for the provider-native Bedrock body.",
        ));
    }
    if runtime_required {
        return Ok(fixed(
            &["runtime_converse"],
            "Converse is required by governance, logging, or multi-model requirements.",
        ));
    }
    if messages_required {
        return Ok(fixed(
            &["mantle_messages"],
            "Mantle is required to preserve Messages semantics or newest beta features.",
        ));
    }
    if source_messages {
        return Ok(fixed(
            &ANTHROPIC_PATHS,
            "Mantle is preferred for an existing first-party Messages API call.",
        ));
    }
    Ok(fixed(
        &["runtime_converse", "runtime_invoke", "mantle_messages"],
        "Converse is the default for a new governance-ready Bedrock workload.",
    ))
}

/// A model/path candidate that passed the hard filters.
pub(super) struct Candidate<'a> {
    pub(super) model_key: &'a str,
    pub(super) model: &'a Model,
    pub(super) path: String,
    pub(super) path_config: &'a PathConfig,
    pub(super) driver: String,
}

pub(super) fn build_candidates<'a>(
    catalog: &'a Catalog,
    paths: &[String],
    requirements: &Requirements,
    region: &str,
) -> Vec<Candidate<'a>> {
    let (model_order, driver) = candidate_order(requirements);
    let required = required_capabilities(requirements);
    let min_context = requirements.min_context_tokens.unwrap_or(0);
    let expected_output = requirements.expected_output_tokens.unwrap_or(0);
    let mut candidates = Vec::new();
    for path in paths {
        for model_key in &model_order {
            let Some((key, model)) = catalog
                .models
                .iter()
                .find(|(name, _)| name == model_key)
                .map(|(name, model)| (name.as_str(), model))
            else {
                continue;
            };
            let Some(path_config) = model.path(path) else {
                continue;
            };
            if !path_config.available {
                continue;
            }
            if let Some(regions) = &path_config.regions {
                if !regions.iter().any(|known| known == region) {
                    continue;
                }
            }
            if path_config.requires_cris
                && !model.inference_profiles.is_empty()
                && !model
                    .inference_profiles
                    .iter()
                    .any(|(_, regions)| regions.iter().any(|known| known == region))
            {
                continue;
            }
            if !required.iter().all(|name| model.has_capability(name)) {
                continue;
            }
            if model.context_window.below(min_context)
                || model.output_token_ceiling.below(expected_output)
            {
                continue;
            }
            candidates.push(Candidate {
                model_key: key,
                model,
                path: path.clone(),
                path_config,
                driver: driver.clone(),
            });
        }
    }
    candidates
}

fn candidate_summary(
    candidate: &Candidate<'_>,
    requirements: &Requirements,
    reason: &str,
    region: &str,
) -> Json {
    let path_config = candidate.path_config;
    let invocation = resolve_invocation_model_id(
        &path_config.model_id,
        path_config.requires_cris,
        requirements,
        Some(candidate.model),
        region,
    );
    candidate_json(candidate, invocation.as_deref(), reason)
}

fn candidate_json(item: &Candidate<'_>, invocation: Option<&str>, reason: &str) -> Json {
    let path_config = item.path_config;
    candidate(
        item.model_key,
        &path_config.model_id,
        &item.path,
        invocation,
        invocation_requires_cris(path_config, invocation),
        reason,
    )
}

/// `_invocation_requires_cris`.
pub(super) fn invocation_requires_cris(path_config: &PathConfig, invocation: Option<&str>) -> bool {
    path_config.requires_cris && invocation != Some(path_config.model_id.as_str())
}

/// `_allowed_inference_profiles`.
pub(super) fn allowed_inference_profiles(
    model_id: &str,
    model: Option<&Model>,
    region: &str,
    requirements: &Requirements,
) -> Vec<String> {
    let geography = requirements.cris_geography.as_deref();
    let geo_required = requirements.data_residency.as_deref() == Some("geo_required");
    let Some(model) = model else {
        return Vec::new();
    };
    model
        .inference_profiles
        .iter()
        .filter(|(prefix, regions)| {
            regions.iter().any(|known| known == region)
                && !(geo_required
                    && (prefix == "global" || geography.is_some_and(|geo| prefix != geo)))
        })
        .map(|(prefix, _)| format!("{prefix}.{model_id}"))
        .collect()
}

/// `_runtime_in_region_allowed`.
pub(super) fn runtime_in_region_allowed(
    model_id: &str,
    model: Option<&Model>,
    region: &str,
    requirements: &Requirements,
) -> bool {
    let listed = model.is_some_and(|model| {
        model
            .runtime_in_region_regions
            .iter()
            .any(|known| known == region)
    });
    if !listed {
        return false;
    }
    let allowed = allowed_inference_profiles(model_id, model, region, requirements);
    requirements.data_residency.as_deref() != Some("geo_required")
        || allowed
            .iter()
            .any(|profile| !profile.starts_with("global."))
}

/// `_profile_verification`: the `allowed_*` members of a verification block.
pub(super) fn profile_verification(
    model_id: &str,
    requires_cris: bool,
    model: Option<&Model>,
    region: &str,
    requirements: &Requirements,
) -> Vec<(&'static str, Json)> {
    let Some(model) = model.filter(|model| requires_cris && !model.inference_profiles.is_empty())
    else {
        return Vec::new();
    };
    let mut members = vec![(
        "allowed_inference_profiles",
        strings(allowed_inference_profiles(
            model_id,
            Some(model),
            region,
            requirements,
        )),
    )];
    if runtime_in_region_allowed(model_id, Some(model), region, requirements) {
        members.push((
            "allowed_in_region_model_arns",
            strings([format!(
                "arn:aws:bedrock:{region}::foundation-model/{model_id}"
            )]),
        ));
    }
    members
}

/// `_resolve_invocation_model_id`: the callable id of a path, `None` when the
/// requirements leave it unresolved.
pub(super) fn resolve_invocation_model_id(
    model_id: &str,
    requires_cris: bool,
    requirements: &Requirements,
    model: Option<&Model>,
    region: &str,
) -> Option<String> {
    if !requires_cris {
        return Some(model_id.to_string());
    }
    let explicit = requirements.inference_profile_id.as_deref();
    if runtime_in_region_allowed(model_id, model, region, requirements)
        && explicit.is_none_or(|profile| profile == model_id)
    {
        return Some(model_id.to_string());
    }
    let mut candidate = explicit.map(str::to_string);
    if candidate.is_none() {
        match requirements.data_residency.as_deref().unwrap_or("unknown") {
            "global_allowed" => candidate = Some(format!("global.{model_id}")),
            "geo_required" => {
                candidate = requirements
                    .cris_geography
                    .as_ref()
                    .map(|geography| format!("{geography}.{model_id}"));
            }
            _ => {}
        }
    }
    let candidate = candidate?;
    let Some(model) = model.filter(|model| !model.inference_profiles.is_empty()) else {
        return Some(candidate);
    };
    let allowed = allowed_inference_profiles(model_id, Some(model), region, requirements);
    let mut profile_id = candidate.clone();
    if candidate.starts_with("arn:") {
        let found = INFERENCE_PROFILE_ARN
            .captures(candidate.as_str())
            .ok()
            .flatten()?;
        let partition = if region.starts_with("us-gov-") {
            "aws-us-gov"
        } else if region.starts_with("cn-") {
            "aws-cn"
        } else {
            "aws"
        };
        if group(&found, 1) != partition || group(&found, 2) != region {
            return None;
        }
        if group(&found, 3) == "application-inference-profile" {
            // Preserve the callable ARN. The verifier checks its model
            // destinations against allowed_inference_profiles before making an
            // inference call.
            return (!allowed.is_empty()).then_some(candidate);
        }
        profile_id = group(&found, 4).to_string();
    }
    allowed.contains(&profile_id).then_some(candidate)
}

fn decision_options(
    catalog: &Catalog,
    workload: &Workload,
    option_paths: &[String],
    region: &str,
) -> Vec<Json> {
    let mut options = Vec::new();
    let mut seen: Vec<&String> = Vec::new();
    for path in option_paths {
        if seen.contains(&path) {
            continue;
        }
        seen.push(path);
        let candidates = build_candidates(
            catalog,
            std::slice::from_ref(path),
            &workload.requirements,
            region,
        );
        let Some(first) = candidates.first() else {
            continue;
        };
        let tradeoff = if path == "mantle_messages" {
            "Preserves Messages code and beta access but gives up runtime-only governance."
        } else {
            "Provides runtime governance but requires rewriting the Messages integration."
        };
        options.push(candidate_summary(
            first,
            &workload.requirements,
            tradeoff,
            region,
        ));
    }
    options
}

/// `_feature_assessment`: feature statuses, detected features forced to
/// `detected`, version-hop surfaces defaulting to `unknown`.
pub(super) fn feature_assessment(
    workload: &Workload,
    source_version: Option<&str>,
    target_version: Option<&str>,
) -> BTreeMap<String, String> {
    let mut statuses: BTreeMap<String, String> = workload.feature_status.iter().cloned().collect();
    for feature in &workload.detected_features {
        statuses.insert(feature.clone(), "detected".to_string());
    }
    let source_tuple = source_version.and_then(version_tuple);
    let target_tuple = target_version.and_then(version_tuple);
    if let (Some(source), Some(target)) = (source_tuple, target_tuple) {
        if source < target && target >= (4, 7) {
            for feature in VERSION_SCAN_FEATURES {
                statuses
                    .entry(feature.to_string())
                    .or_insert_with(|| "unknown".to_string());
            }
        }
    }
    statuses
}

fn assessment_json(statuses: &BTreeMap<String, String>) -> Json {
    Json::Object(
        statuses
            .iter()
            .map(|(feature, status)| (feature.clone(), text(status)))
            .collect(),
    )
}

fn detected_set(statuses: &BTreeMap<String, String>) -> BTreeSet<String> {
    statuses
        .iter()
        .filter(|(_, status)| status.as_str() == "detected")
        .map(|(feature, _)| feature.clone())
        .collect()
}

struct SourceAnalysis {
    detected_version: Option<String>,
    target_version: Option<String>,
    version_changed: Option<bool>,
}

impl SourceAnalysis {
    fn json(&self) -> Json {
        findings::source_analysis(
            self.detected_version.as_deref(),
            self.target_version.as_deref(),
            self.version_changed,
        )
    }
}

fn source_analysis(source: &Source, target_version: Option<&str>) -> SourceAnalysis {
    let detected = source_version(source);
    let source_tuple = detected.as_deref().and_then(version_tuple);
    let target_tuple = target_version.and_then(version_tuple);
    let changed = match (source_tuple, target_tuple) {
        (Some(source), Some(target)) => Some(source != target),
        _ => None,
    };
    SourceAnalysis {
        detected_version: detected,
        target_version: target_version.map(str::to_string),
        version_changed: changed,
    }
}

fn structured_output_remediation(model: Option<&Model>) -> &'static str {
    if model.is_some_and(|model| model.forced_tool_choice_supported == Some(false)) {
        "Opus 5.5 has no native schema guarantee and rejects forced any/tool choice, \
assistant prefill, and disabled thinking. Move plain-text prefill intent into prompt \
instructions without promising an exact prefix. For structured results, use automatic tool choice or prompted JSON \
with application schema validation and bounded retries; fail closed on invalid output. \
If model-enforced schema output is required, explicitly choose another verified target."
    } else {
        "Use a forced tool without strict only after verifying that the selected model and \
thinking mode permit it. Otherwise use automatic tool choice with application schema \
validation and bounded retries; do not claim a native schema guarantee."
    }
}

fn migration_deltas(
    source: &Source,
    analysis: &SourceAnalysis,
    path: &str,
    statuses: &BTreeMap<String, String>,
    requirements: &Requirements,
    model: Option<&Model>,
) -> Vec<Delta> {
    if source.provider != "anthropic" {
        return Vec::new();
    }
    let mut deltas = vec![
        delta(
            "sdk_and_auth",
            "path",
            "Authentication and client construction change for the selected Bedrock path.",
        ),
        delta(
            "model_id_shape",
            "path",
            "Use the selected region's verified runtime model or CRIS profile ID; \
Mantle uses its clean path ID.",
        ),
        delta(
            "iam_action",
            "platform",
            "Mantle and runtime use different IAM actions and resource conditions.",
        ),
        delta(
            "version_header",
            "path",
            "Mantle uses the HTTP version header; runtime uses the Bedrock body field.",
        ),
        delta(
            "error_and_quota_surface",
            "platform",
            "Messages 400s, runtime ValidationException, and quota pools differ by path.",
        ),
    ];
    if analysis.version_changed == Some(true) {
        deltas.push(delta(
            "claude_version_hop",
            "version",
            format!(
                "Claude {} to {} requires version migration checks.",
                analysis.detected_version.as_deref().unwrap_or("None"),
                analysis.target_version.as_deref().unwrap_or("None"),
            ),
        ));
    }
    let mut detected = detected_set(statuses);
    if requirements.has_critical("structured_output") {
        detected.insert("structured_output".to_string());
    }
    if detected.contains("structured_output") || detected.contains("assistant_prefill") {
        deltas.push(delta(
            "structured_output",
            "feature",
            structured_output_remediation(model),
        ));
    }
    if path == "runtime_converse"
        && (requirements.has_critical("extended_thinking") || detected.contains("budget_tokens"))
    {
        deltas.push(delta(
            "additional_model_request_fields",
            "path",
            "Converse carries Anthropic thinking and effort fields through \
additionalModelRequestFields.",
        ));
    }
    if detected.contains("prompt_caching") {
        deltas.push(delta(
            "prompt_cache_shape",
            "path",
            "Mantle cache_control and Converse cachePoint use different request shapes.",
        ));
    }
    deltas
}

fn compatibility_block(
    statuses: &BTreeMap<String, String>,
    requirements: &Requirements,
    path: &str,
) -> Json {
    let detected = detected_set(statuses);
    let mut native: BTreeSet<String> = detected
        .iter()
        .filter(|feature| {
            matches!(
                feature.as_str(),
                "citations" | "streaming" | "tool_use" | "vision"
            )
        })
        .cloned()
        .collect();
    if detected.contains("prompt_caching") && path == "mantle_messages" {
        native.insert("prompt_caching".to_string());
    }
    let mut portable: BTreeSet<String> = detected
        .iter()
        .filter(|feature| VERSION_SCAN_FEATURES.contains(&feature.as_str()))
        .cloned()
        .collect();
    portable.extend(
        detected
            .iter()
            .filter(|feature| matches!(feature.as_str(), "models_api" | "structured_output"))
            .cloned(),
    );
    if detected.contains("prompt_caching") && path != "mantle_messages" {
        portable.insert("prompt_caching".to_string());
    }
    let rearchitecture: BTreeSet<String> = detected
        .iter()
        .filter(|feature| REARCHITECTURE_FEATURES.contains(&feature.as_str()))
        .cloned()
        .collect();
    native.extend(
        requirements
            .critical_features
            .iter()
            .filter(|feature| matches!(feature.as_str(), "tool_use" | "vision" | "multimodal"))
            .cloned(),
    );
    let portable: BTreeSet<String> = portable.difference(&rearchitecture).cloned().collect();
    compatibility(
        &to_vec(&native),
        &to_vec(&portable),
        &to_vec(&rearchitecture),
        &[],
    )
}

fn architecture_impacts(statuses: &BTreeMap<String, String>, blocks: &[Finding]) -> Vec<Json> {
    let detected = detected_set(statuses);
    detected
        .iter()
        .filter(|feature| REARCHITECTURE_FEATURES.contains(&feature.as_str()))
        .filter_map(|feature| {
            let (_, code, message, remediation) = block_entry(feature)?;
            let remediation = blocks
                .iter()
                .find(|block| block.code == *code)
                .map_or(*remediation, |block| block.remediation.as_str());
            Some(impact(feature, message, remediation))
        })
        .collect()
}

fn evaluation_requirements(workload: &Workload, statuses: &BTreeMap<String, String>) -> Json {
    let detected = detected_set(statuses);
    let requirements = &workload.requirements;
    let trajectory = requirements.has_critical("agentic")
        || requirements.has_critical("tool_use")
        || detected.contains("agent_infra")
        || detected.contains("server_tools");
    let mut gates = vec![
        "Compare representative source and target outputs against a versioned golden set.",
        "Fail on refusal mishandling, truncation, or invalid structured output.",
    ];
    if trajectory {
        gates.extend([
            "Verify the correct tool is called with valid arguments.",
            "Verify the agent loop terminates and guardrails remain effective.",
        ]);
    }
    findings::evaluation(trajectory, &gates)
}

fn base_findings(
    statuses: &BTreeMap<String, String>,
    analysis: &SourceAnalysis,
    model: Option<&Model>,
    requirements: &Requirements,
) -> (Vec<Finding>, Vec<Finding>) {
    let mut detected = detected_set(statuses);
    if requirements.has_critical("structured_output") {
        detected.insert("structured_output".to_string());
    }
    let mut blocks = Vec::new();
    let mut tuning = Vec::new();
    for feature in &detected {
        if let Some((_, code, message, remediation)) = block_entry(feature) {
            let remediation = if feature == "assistant_prefill" {
                structured_output_remediation(model)
            } else {
                remediation
            };
            blocks.push(finding(code, BLOCKS, *message, remediation));
        }
        if let Some((_, code, message, remediation)) =
            TUNE_FINDINGS.iter().find(|entry| entry.0 == feature)
        {
            tuning.push(finding(code, TUNE, *message, *remediation));
        }
    }
    let mut unknown: Vec<&str> = VERSION_SCAN_FEATURES
        .iter()
        .copied()
        .filter(|feature| statuses.get(*feature).map(String::as_str) == Some("unknown"))
        .collect();
    unknown.sort_unstable();
    if analysis.version_changed == Some(true) && !unknown.is_empty() {
        blocks.push(finding(
            "version_scan_incomplete",
            BLOCKS,
            format!(
                "The source-to-target Claude version hop has unverified breaking-change surfaces: {}",
                unknown.join(", ")
            ),
            "Scan the recorded source paths and mark every feature detected or absent.",
        ));
    }
    if detected.contains("structured_output") {
        blocks.push(finding(
            "structured_output_portable_pattern",
            BLOCKS,
            "Native structured-output controls vary by model, path, and region.",
            structured_output_remediation(model),
        ));
    }
    if detected.contains("structured_output") && detected.contains("citations") {
        blocks.push(finding(
            "structured_output_citations_conflict",
            BLOCKS,
            "Structured output and citations are incompatible in the reference snapshot.",
            "Choose one output contract per call site and verify it live.",
        ));
    }
    (blocks, tuning)
}

fn verification(
    candidate: &Candidate<'_>,
    region: &str,
    catalog: &Catalog,
    invocation: Option<&str>,
    requirements: &Requirements,
) -> Json {
    let requires_cris = invocation_requires_cris(candidate.path_config, invocation);
    let mut checks = vec![
        "Probe the selected model through the selected API path in the target account and region.",
        "Verify path-specific IAM before code rewrite or POC generation.",
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
        &candidate.path_config.model_id,
        requires_cris,
        Some(candidate.model),
        region,
        requirements,
    ));
    object(members)
}

fn decision_required(
    workload: &Workload,
    region: &str,
    catalog: &Catalog,
    constraints: &Constraints,
) -> Res<Json> {
    let version = source_version(&workload.source);
    let statuses = feature_assessment(workload, version.as_deref(), None);
    let options = decision_options(catalog, workload, &constraints.option_paths, region);
    let mut distinct: Vec<&String> = Vec::new();
    for path in &constraints.option_paths {
        if !distinct.contains(&path) {
            distinct.push(path);
        }
    }
    if options.len() != distinct.len() {
        return Err(ModelError::new(format!(
            "catalog has no candidate for every conflicting model/path option for workload {}",
            safe_text(&workload.id)
        )));
    }
    let blocks = [finding(
        "model_path_decision_required",
        BLOCKS,
        "Messages continuity conflicts with required Bedrock runtime capabilities.",
        "Choose the Mantle continuity option or the runtime governance option, \
update requirements, and rerun Model Recommend.",
    )];
    Ok(object(vec![
        ("workload_id", text(&workload.id)),
        ("provider_module", text("anthropic")),
        ("decision_status", text("decision_required")),
        ("source", workload.source.json.clone()),
        (
            "source_analysis",
            findings::source_analysis(version.as_deref(), None, None),
        ),
        ("feature_assessment", assessment_json(&statuses)),
        ("primary_model", Json::Null),
        ("model_identity", Json::Null),
        ("api_path", Json::Null),
        ("invocation_model_id", Json::Null),
        ("decision_options", Json::Array(options)),
        ("alternatives", Json::Array(Vec::new())),
        ("rationale", strings(&constraints.rationale)),
        ("blocks", findings_json(&blocks)),
        ("tuning", Json::Array(Vec::new())),
        (
            "compatibility",
            compatibility_block(&statuses, &workload.requirements, ""),
        ),
        (
            "architecture_impacts",
            Json::Array(architecture_impacts(&statuses, &[])),
        ),
        ("migration_deltas", Json::Array(Vec::new())),
        ("evaluation", evaluation_requirements(workload, &statuses)),
        (
            "rollout",
            rollout(
                "decision_required",
                "Resolve the model/path conflict before implementation.",
            ),
        ),
        (
            "verification",
            object(vec![
                ("region", text(region)),
                ("catalog_verified_at", text(&catalog.verified_at)),
                ("verified_at", Json::Null),
                ("probe_status", text("not_applicable")),
                ("availability_claim", text("not_selected")),
                ("invocation_model_id", Json::Null),
                (
                    "required_checks",
                    strings([
                        "Resolve the model/path decision before running an availability probe.",
                    ]),
                ),
            ]),
        ),
    ]))
}

/// `recommend_anthropic_workload`.
///
/// # Errors
///
/// A catalog with no candidate for the workload's requirements.
#[allow(clippy::too_many_lines)]
pub(super) fn recommend_workload(
    workload: &Workload,
    region: &str,
    catalog: &Catalog,
) -> Res<Json> {
    let constraints = path_constraints(workload)?;
    if !constraints.conflicts.is_empty() {
        return decision_required(workload, region, catalog, &constraints);
    }
    let requirements = &workload.requirements;
    let candidates = build_candidates(catalog, &constraints.paths, requirements, region);
    let Some(chosen) = candidates.first() else {
        return Err(ModelError::new(format!(
            "catalog has no model/path candidate satisfying workload {} requirements",
            safe_text(&workload.id)
        )));
    };
    let model = chosen.model;
    let path_config = chosen.path_config;
    let analysis = source_analysis(&workload.source, Some(&model.version));
    let statuses = feature_assessment(
        workload,
        analysis.detected_version.as_deref(),
        analysis.target_version.as_deref(),
    );
    let (mut blocks, mut tuning) = base_findings(&statuses, &analysis, Some(model), requirements);
    let provider = workload.source.provider.as_str();
    let provider_module = if ANTHROPIC_POOL.contains(&provider) {
        "anthropic"
    } else {
        "generic"
    };
    if provider_module == "generic" {
        blocks.push(finding(
            "provider_module_pending",
            BLOCKS,
            format!("The {provider} to Bedrock compatibility module is not implemented yet."),
            "Keep this recommendation provisional until its provider module runs.",
        ));
    }
    let invocation = resolve_invocation_model_id(
        &path_config.model_id,
        path_config.requires_cris,
        requirements,
        Some(model),
        region,
    );
    if !model.inference_profiles.is_empty() && path_config.requires_cris && invocation.is_none() {
        blocks.push(finding(
            "inference_profile_unresolved",
            BLOCKS,
            "The selected model has no verified inference profile matching the region and residency requirements.",
            "Choose a supported Geo or Global profile and probe it in the target account; \
resolve application-profile ARNs through Bedrock before implementation.",
        ));
    }
    if model.adaptive_thinking_only {
        let adaptive = finding(
            "adaptive_thinking_required",
            if requirements.thinking_enabled == Some(false) {
                BLOCKS
            } else {
                TUNE
            },
            "Opus 5.5 always uses adaptive thinking; disabled thinking, manual budgets, and forced any/tool choice are unsupported.",
            "Use adaptive thinking with output_config.effort (default medium). \
Budget max_tokens for thinking plus response text and remeasure token usage. \
Use auto/none tool choice; if forced tool selection is a hard requirement, \
select a verified compatible target rather than silently weakening that requirement.",
        );
        if adaptive.tag == BLOCKS {
            blocks.push(adaptive);
        } else {
            tuning.push(adaptive);
        }
    }
    if model.batch_supported == Some(false) {
        for block in &mut blocks {
            if block.code == "message_batches_not_portable" {
                block.remediation =
                    "Opus 5.5 does not support Bedrock Batch. Use on-demand inference, \
or select a verified batch-capable alternative such as Opus 4.6."
                        .to_string();
            }
        }
    }
    let mut rationale = constraints.rationale.clone();
    rationale.push(format!(
        "{} is the highest-ranked {} model that satisfies the {} constraints.",
        model.display_name, chosen.driver, chosen.path
    ));
    let alternatives: Vec<Json> = candidates
        .iter()
        .skip(1)
        .take(3)
        .map(|candidate| {
            candidate_summary(
                candidate,
                requirements,
                "Next compatible model/path candidate after hard-constraint filtering.",
                region,
            )
        })
        .collect();
    let identity = object(vec![
        ("model_key", text(chosen.model_key)),
        ("display_name", text(&model.display_name)),
        ("family", text(&model.family)),
        ("version", text(&model.version)),
        ("context_window", model.context_window.json()),
        ("output_token_ceiling", model.output_token_ceiling.json()),
        ("path_model_id", text(&path_config.model_id)),
        (
            "requires_cris",
            Json::Bool(invocation_requires_cris(path_config, invocation.as_deref())),
        ),
    ]);
    let deltas = migration_deltas(
        &workload.source,
        &analysis,
        &chosen.path,
        &statuses,
        requirements,
        Some(model),
    );
    Ok(object(vec![
        ("workload_id", text(&workload.id)),
        ("provider_module", text(provider_module)),
        ("decision_status", text("recommended")),
        ("source", workload.source.json.clone()),
        ("source_analysis", analysis.json()),
        ("feature_assessment", assessment_json(&statuses)),
        ("primary_model", text(&path_config.model_id)),
        ("model_identity", identity),
        ("api_path", text(&chosen.path)),
        ("invocation_model_id", optional_text(invocation.as_deref())),
        ("decision_options", Json::Array(Vec::new())),
        ("alternatives", Json::Array(alternatives)),
        ("rationale", strings(&rationale)),
        ("blocks", findings_json(&blocks)),
        ("tuning", findings_json(&tuning)),
        (
            "compatibility",
            compatibility_block(&statuses, requirements, &chosen.path),
        ),
        (
            "architecture_impacts",
            Json::Array(architecture_impacts(&statuses, &blocks)),
        ),
        ("migration_deltas", deltas_json(&deltas)),
        ("evaluation", evaluation_requirements(workload, &statuses)),
        (
            "rollout",
            rollout(
                "canary",
                "Compare source and target on the golden set before percentage rollout.",
            ),
        ),
        (
            "verification",
            verification(chosen, region, catalog, invocation.as_deref(), requirements),
        ),
    ]))
}
