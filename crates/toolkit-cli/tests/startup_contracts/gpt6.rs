//! GPT-6 review contracts: `test_gpt6_review_round2.py` and
//! `test_gpt6_review_round5.py`.

use crate::common::{check, ensure, name, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule};

const PRICING_CACHE: [&str; 2] = [
    "skills/gcp-to-aws/references/shared/pricing-cache.md",
    "skills/azure-to-aws/references/shared/pricing-cache.md",
];
const OPENAI_ON_BEDROCK: [&str; 2] = [
    "skills/gcp-to-aws/references/shared/openai-on-bedrock.md",
    "skills/azure-to-aws/references/shared/openai-on-bedrock.md",
];
const DESIGN_AI: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/design/design-ai.md",
    "skills/azure-to-aws/references/phases/design/design-ai.md",
];
const GENERATE_ARTIFACTS_AI: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/generate/generate-artifacts-ai.md",
    "skills/azure-to-aws/references/phases/generate/generate-artifacts-ai.md",
];
const SCHEMA_DESIGN_AWS_AI: &str = "skills/azure-to-aws/references/shared/schema-design-aws-ai.md";
const GENERATE_AI_GCP: &str = "skills/gcp-to-aws/references/phases/generate/generate-ai.md";
const ESTIMATE_AI: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/estimate/estimate-ai.md",
    "skills/azure-to-aws/references/phases/estimate/estimate-ai.md",
];
const BEHAVIOR_DELTA: &str =
    "skills/llm-to-bedrock/references/helpers/behavior-delta-detection/references/openai-to-bedrock.md";

/// One rule per file of `files`, built by `make`.
fn per_file(files: &[&'static str], make: impl Fn(Rule) -> Rule) -> Vec<Rule> {
    files.iter().map(|f| make(Rule::new(*f))).collect()
}

// ---- round 2 ----------------------------------------------------------------

#[test]
fn test_both_pricing_caches_document_gpt6_breakpoint() {
    check_rules(&per_file(&PRICING_CACHE, |r| {
        r.all(&["GPT-6", "272,000 input tokens"])
    }));
}

#[test]
fn test_both_estimate_ai_files_require_matching_context_tiers() {
    let mut rules = per_file(&ESTIMATE_AI, |r| r.all(&["272,000 input tokens"]));
    rules.extend(per_file(&ESTIMATE_AI, |r| {
        r.ci().all(&["long-context parity", "approximate"])
    }));
    check_rules(&rules);
}

#[test]
fn test_design_ai_scopes_mantle_only_to_gpt55_gpt54() {
    check_rules(&per_file(&DESIGN_AI, |r| {
        r.all(&["runtime_openai_cris", "GPT-5.5", "GPT-5.4"])
    }));
}

#[test]
fn test_generate_artifacts_ai_dispatches_runtime_openai_cris() {
    check_rules(&per_file(&GENERATE_ARTIFACTS_AI, |r| {
        r.all(&["runtime_openai_cris"])
    }));
}

#[test]
fn test_generate_ai_gcp_timeline_handles_runtime_openai_cris() {
    check_rules(&[Rule::new(GENERATE_AI_GCP).all(&["runtime_openai_cris"])]);
}

#[test]
fn test_schema_design_aws_ai_vocabulary_includes_runtime_openai_cris() {
    check_rules(&[Rule::new(SCHEMA_DESIGN_AWS_AI).all(&[
        "runtime_openai_cris",
        "GPT-5.5",
        "GPT-5.4",
    ])]);
}

#[test]
fn test_openai_on_bedrock_scopes_invoke_support_per_model() {
    let mut rules = per_file(&OPENAI_ON_BEDROCK, |r| {
        r.norm().all(&["does NOT support `Invoke`"])
    });
    rules.extend(per_file(&OPENAI_ON_BEDROCK, |r| {
        r.all(&["Astra"])
            .none(&["All four support `Responses`, `Chat Completions`, `Invoke`, and"])
    }));
    check_rules(&rules);
}

fn mantle_never_supports_invoke(p: &Plugin) -> Verdict {
    for f in OPENAI_ON_BEDROCK {
        let text = p.read(f)?;
        ensure!(
            text.contains("Invoke is NOT available on `bedrock-mantle`")
                || (text.contains("Invoke") && text.to_lowercase().contains("mantle")),
            "{}: missing the mantle-never-supports-Invoke correction (Finding 3)",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_openai_on_bedrock_notes_mantle_never_supports_invoke() {
    check(
        mantle_never_supports_invoke,
        &[(
            edit_replace!(OPENAI_ON_BEDROCK[1], "Invoke", "Call"),
            "mantle-never-supports-Invoke",
        )],
    );
}

// ---- round 5 ----------------------------------------------------------------

#[test]
fn test_both_generate_artifacts_ai_document_gpt6_cris_project_default() {
    check_rules(&per_file(&GENERATE_ARTIFACTS_AI, |r| {
        r.norm().all(&["project/default", "GPT-6"])
    }));
}

#[test]
fn test_schema_documents_required_client_api_with_converse_for_guardrails() {
    check_rules(&[
        Rule::new(SCHEMA_DESIGN_AWS_AI).all(&["required_client_api", "Astra"]),
        Rule::new(SCHEMA_DESIGN_AWS_AI).ci().all(&["converse"]),
    ]);
}

#[test]
fn test_both_design_ai_require_converse_client_for_astra_guardrails() {
    let mut rules = per_file(&DESIGN_AI, |r| r.norm().all(&["required_client_api"]));
    rules.extend(per_file(&DESIGN_AI, |r| {
        r.norm()
            .ci()
            .all(&["converse"])
            .any(&["never `responses`", "never responses"])
    }));
    check_rules(&rules);
}

#[test]
fn test_both_generate_artifacts_ai_consume_required_client_api_in_cris_step() {
    check_rules(&per_file(&GENERATE_ARTIFACTS_AI, |r| {
        r.norm().all(&["required_client_api", "responses.create"])
    }));
}

#[test]
fn test_region_only_cris_move_keeps_responses() {
    let files: Vec<&'static str> = DESIGN_AI
        .iter()
        .chain(GENERATE_ARTIFACTS_AI.iter())
        .copied()
        .collect();
    check_rules(&per_file(&files, |r| r.norm().ci().all(&["region-only"])));
}

#[test]
fn test_behavior_delta_no_longer_claims_chat_completions_unverified() {
    check_rules(&[
        Rule::new(BEHAVIOR_DELTA)
            .none(&["no GPT model card lists Chat Completions as supported"])
            .all(&["Chat Completions", "OPTIONAL"]),
        Rule::new(BEHAVIOR_DELTA).ci().all(&["supported"]),
    ]);
}

#[test]
fn test_both_design_ai_no_longer_force_gpt6_chat_completions_reshape() {
    let mut rules = per_file(&DESIGN_AI, |r| r.norm().all(&["GPT-5.5", "GPT-5.4"]));
    rules.extend(per_file(&DESIGN_AI, |r| {
        r.norm().ci().any(&["near-drop-in", "optional"])
    }));
    check_rules(&rules);
}

#[test]
fn test_gcp_generate_ai_no_longer_forces_gpt6_chat_completions_reshape() {
    check_rules(&[
        Rule::new(GENERATE_AI_GCP)
            .norm()
            .all(&["GPT-5.5", "GPT-5.4"]),
        Rule::new(GENERATE_AI_GCP).norm().ci().all(&["optional"]),
    ]);
}
