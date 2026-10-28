//! `test_estimate_clarify_volume_resolution.py` and
//! `test_generate_mantle_dispatch.py`.

use crate::common::{check, ensure, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const AZURE_ESTIMATE_AI: &str = "skills/azure-to-aws/references/phases/estimate/estimate-ai.md";
const GCP_ESTIMATE_AI: &str = "skills/gcp-to-aws/references/phases/estimate/estimate-ai.md";
const AZURE_CLARIFY_AI_ONLY: &str =
    "skills/azure-to-aws/references/phases/clarify/clarify-ai-only.md";
const GCP_DISCOVER_ANTHROPIC: &str =
    "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md";
const AZURE_DISCOVER_ANTHROPIC: &str =
    "skills/azure-to-aws/references/phases/discover/discover-anthropic-api.md";
const AZURE_DISCOVER_OPENAI: &str =
    "skills/azure-to-aws/references/phases/discover/discover-openai-api.md";

// ---- test_estimate_clarify_volume_resolution.py --------------------------

#[test]
fn test_azure_estimate_ai_has_ported_unresolved_scenario_rows_language() {
    check_rules(&[Rule::new(AZURE_ESTIMATE_AI).norm().all(&[
        "Do NOT fall back to the",
        "tier table for the partial profile's portion",
        "treat the partial profile's portion as **unresolved**",
        "scenario rows",
        "not derived from the other provider's volume",
    ])]);
}

#[test]
fn test_azure_estimate_ai_no_longer_unconditionally_directs_to_tier_table() {
    check_rules(&[Rule::new(AZURE_ESTIMATE_AI).norm().all(&[
        "when set by Clarify's auto-resolve",
        "was derived by summing ONLY the full-window",
    ])]);
}

const SHARED_PHRASES: [&str; 6] = [
    "Do NOT fall back to the",
    "tier table for the partial profile's portion",
    "was derived by summing ONLY the full-window",
    "treat the partial profile's portion as **unresolved**",
    "plus explicit low/medium/high scenario rows",
    "scenario for the partial provider's portion, not derived from the other provider's volume",
];

#[test]
fn test_azure_and_gcp_estimate_ai_partial_provider_exception_match_in_substance() {
    check_rules(&[
        Rule::new(AZURE_ESTIMATE_AI).norm().all(&SHARED_PHRASES),
        Rule::new(GCP_ESTIMATE_AI).norm().all(&SHARED_PHRASES),
    ]);
}

const Q3: Scope = Scope::Between(
    "Q3 — Monthly AI spend on Azure OpenAI / OpenAI?",
    "Q4 — Cross-cloud API call concerns",
);

#[test]
fn test_clarify_q3_first_branch_checks_provenance_before_auto_resolving() {
    check_rules(&[Rule::new(AZURE_CLARIFY_AI_ONLY)
        .scope(Q3)
        .all(&[
            "current_costs.source",
            "breakdown[]",
            "partial_window",
            "cost_status",
        ])
        .any(&["do NOT auto-resolve", "do NOT trust it blindly"])]);
}

#[test]
fn test_clarify_q3_falls_through_to_asking_normally_when_all_contributors_partial() {
    check_rules(&[Rule::new(AZURE_CLARIFY_AI_ONLY)
        .scope(Q3)
        .any(&["fall through to asking normally", "ask normally"])]);
}

#[test]
fn test_discover_merge_stamps_partial_window_and_cost_status_onto_current_costs() {
    let merge = Scope::AfterWindow(
        "## Step 4: Merge into the AI Workload Profile",
        "current_costs",
        2500,
    );
    check_rules(&[
        Rule::new(GCP_DISCOVER_ANTHROPIC)
            .scope(merge)
            .all(&["\"partial_window\":", "\"cost_status\":"]),
        Rule::new(AZURE_DISCOVER_ANTHROPIC)
            .scope(merge)
            .all(&["\"partial_window\":", "\"cost_status\":"]),
        Rule::new(AZURE_DISCOVER_OPENAI)
            .scope(merge)
            .all(&["\"partial_window\":", "\"cost_status\":"]),
    ]);
}

fn anthropic_copies_mirrored(p: &Plugin) -> Verdict {
    let gcp = p.read(GCP_DISCOVER_ANTHROPIC)?;
    let azure = p.read(AZURE_DISCOVER_ANTHROPIC)?;
    for marker in ["\"partial_window\":", "\"cost_status\":"] {
        ensure!(
            gcp.matches(marker).count() == azure.matches(marker).count(),
            "{marker:?} appears a different number of times in the GCP vs Azure Anthropic discover copies"
        );
    }
    Ok(())
}

#[test]
fn test_anthropic_discover_copies_remain_mirrored_after_this_fix() {
    check(
        anthropic_copies_mirrored,
        &[
            (
                edit_replace!(GCP_DISCOVER_ANTHROPIC, "\"cost_status\":", "\"status\":"),
                "different number",
            ),
            (
                edit_replace!(
                    AZURE_DISCOVER_ANTHROPIC,
                    "\"partial_window\":",
                    "\"window\":"
                ),
                "different number",
            ),
        ],
    );
}

// ---- test_generate_mantle_dispatch.py ------------------------------------

const GCP_GENERATE: &str = "skills/gcp-to-aws/references/phases/generate/generate-artifacts-ai.md";
const AZURE_GENERATE: &str =
    "skills/azure-to-aws/references/phases/generate/generate-artifacts-ai.md";

#[test]
fn test_step1m_base_url_uses_openai_v1_with_caveat() {
    check_rules(&[Rule::new(GCP_GENERATE)
        .all(&["bedrock-mantle.{region}.api.aws/openai/v1", "404"])
        .none(&["bedrock-mantle.{region}.api.aws/v1`"])]);
}

#[test]
fn test_step2_harness_import_conditional_on_mantle_path() {
    check_rules(&[Rule::new(GCP_GENERATE)
        .scope(Scope::Between(
            "## Step 2: Generate Test Comparison Harness",
            "## Step 3: Generate Bedrock Setup Script",
        ))
        .all(&[
            "from provider_adapter import get_provider",
            "Mantle / runtime CRIS",
            "do not import `provider_adapter`",
            "client.responses.create",
        ])]);
}

#[test]
fn test_step3_mantle_iam_and_test_api_are_conditional() {
    check_rules(&[Rule::new(GCP_GENERATE)
        .scope(Scope::Between(
            "## Step 3: Generate Bedrock Setup Script",
            "## Step 3B: Generate Gateway Configuration",
        ))
        .all(&[
            "bedrock-mantle:CreateInference",
            "bedrock-mantle:CallWithBearerToken",
            "project/*",
            "bedrock:InvokeModel",
            "Match inference permissions to the selected endpoint",
            "test_comparison.py --target-only --quick",
            "same selected API, model ID and region",
            "A Mantle selection must never run a Converse probe",
            "runtime selection must preserve its validated profile ID",
        ])]);
}

#[test]
fn test_azure_generate_mantle_branch_still_correct() {
    check_rules(&[Rule::new(AZURE_GENERATE).all(&[
        "bedrock-mantle.{region}.api.aws/openai/v1",
        "bedrock-mantle:",
    ])]);
}
