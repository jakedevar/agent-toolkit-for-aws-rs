//! `test_gpt6_round4_findings.py`: CRIS carve-out propagation, Execute-phase
//! GPT-6 recognition and the report-generator source-pricing rows.

use crate::common::{check, ensure, has, name, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const AZURE_DESIGN: &str = "skills/azure-to-aws/references/phases/design/design.md";
const AZURE_GENERATE: &str = "skills/azure-to-aws/references/phases/generate/generate.md";
const GCP_GENERATE_AI: &str = "skills/gcp-to-aws/references/phases/generate/generate-ai.md";
const GCP_GENERATE_ARTIFACTS_AI: &str =
    "skills/gcp-to-aws/references/phases/generate/generate-artifacts-ai.md";
const GUARDRAILS: &str = "skills/shared/ai/ai-migration-guardrails.md";
const GUARDRAILS_VENDORED: [&str; 2] = [
    "skills/gcp-to-aws/references/vendored/ai/ai-migration-guardrails.md",
    "skills/azure-to-aws/references/vendored/ai/ai-migration-guardrails.md",
];
const OPENAI_GUIDE: &str = "skills/shared/ai/ai-openai-to-bedrock.md";
const OPENAI_GUIDE_VENDORED: [&str; 2] = [
    "skills/gcp-to-aws/references/vendored/ai/ai-openai-to-bedrock.md",
    "skills/azure-to-aws/references/vendored/ai/ai-openai-to-bedrock.md",
];
const ESTIMATE_AI: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/estimate/estimate-ai.md",
    "skills/azure-to-aws/references/phases/estimate/estimate-ai.md",
];
const RESOLVER: &str =
    "skills/llm-to-bedrock/references/helpers/resolve-bedrock-model-id/resolve-bedrock-model-id.md";
const CODE_ANALYZER: &str = "agents/llm2bedrock-code-analyzer.md";
const PROMPT_EVALUATOR: &str = "agents/llm2bedrock-prompt-evaluator.md";
const CODE_REWRITER: &str = "agents/llm2bedrock-code-rewriter.md";
const BEHAVIOR_DELTA: &str =
    "skills/llm-to-bedrock/references/helpers/behavior-delta-detection/references/openai-to-bedrock.md";
const REPORT_GENERATOR: &str = "agents/llm2bedrock-report-generator.md";

fn assert_scopes_gpt_ban(file: &'static str) -> Rule {
    Rule::new(file)
        .norm()
        .all(&[
            "no proprietary GPT-5.5 or GPT-5.4 model ID is paired",
            "runtime_openai_cris on bedrock-runtime — this is not a violation",
        ])
        .none(&["no proprietary openai.gpt-* model ID is paired"])
}

#[test]
fn test_azure_design_assert_scopes_gpt_ban_to_55_54_and_carves_out_cris() {
    check_rules(&[assert_scopes_gpt_ban(AZURE_DESIGN)]);
}

#[test]
fn test_azure_generate_assert_scopes_gpt_ban_to_55_54_and_carves_out_cris() {
    check_rules(&[assert_scopes_gpt_ban(AZURE_GENERATE)]);
}

#[test]
fn test_gcp_generate_ai_routes_cris_through_bedrock_runtime_not_mantle() {
    check_rules(&[
        Rule::new(GCP_GENERATE_AI).all(&["bedrock-runtime.{region}.amazonaws.com/openai/v1"]),
        Rule::new(GCP_GENERATE_AI)
            .norm()
            .ci()
            .any(&["not the mantle url", "not** the mantle url"])
            .any(&["not** `bedrock-mantle:*`", "not `bedrock-mantle:*`"]),
    ]);
}

#[test]
fn test_gcp_step1m_and_harness_agree_on_adapter_free_paths() {
    check_rules(&[
        Rule::new(GCP_GENERATE_ARTIFACTS_AI)
            .scope(Scope::Span("## Step 1M:", "## Step 1:"))
            .all(&["do not emit or import a provider-adapter stub"]),
        Rule::new(GCP_GENERATE_ARTIFACTS_AI)
            .scope(Scope::Span("## Step 2:", "## Step 3:"))
            .all(&[
                "Mantle / runtime CRIS",
                "do not import `provider_adapter`",
                "from provider_adapter import get_provider",
                "client.responses.create",
                "client.chat.completions.create",
            ]),
    ]);
}

/// The canonical file and every vendored copy must be identical.
fn identical(canonical: &str, vendored: &[&str], p: &Plugin) -> Verdict {
    let text = p.read(canonical)?;
    for v in vendored {
        ensure!(
            p.read(v)? == text,
            "{v} is not byte-identical to the canonical {}",
            name(canonical)
        );
    }
    Ok(())
}

fn guardrails_identical(p: &Plugin) -> Verdict {
    identical(GUARDRAILS, &GUARDRAILS_VENDORED, p)
}

fn openai_guide_identical(p: &Plugin) -> Verdict {
    identical(OPENAI_GUIDE, &OPENAI_GUIDE_VENDORED, p)
}

#[test]
fn test_guardrails_canonical_and_vendored_name_gpt6_family() {
    check_rules(&[Rule::new(GUARDRAILS).all(&["GPT-6"]).any(&[
        "fallback exists only for GPT-5.6 and GPT-6",
        "GPT-5.6 and GPT-6",
    ])]);
    check(
        guardrails_identical,
        &[
            (
                edit_replace!(GUARDRAILS_VENDORED[0], "GPT-6", "GPT-7"),
                "not byte-identical",
            ),
            (
                edit_replace!(GUARDRAILS, "GPT-6", "GPT-7"),
                "not byte-identical",
            ),
        ],
    );
}

#[test]
fn test_openai_guide_canonical_and_vendored_region_gate_names_gpt6() {
    check_rules(&[Rule::new(OPENAI_GUIDE).all(&["GPT-6 Astra / Sol / Luna / 6.1 Sol:"])]);
    check(
        openai_guide_identical,
        &[
            (
                edit_replace!(OPENAI_GUIDE_VENDORED[1], "GPT-6", "GPT-7"),
                "not byte-identical",
            ),
            (
                edit_replace!(OPENAI_GUIDE, "GPT-6", "GPT-7"),
                "not byte-identical",
            ),
        ],
    );
}

#[test]
fn test_both_estimators_carve_out_global_cris_parity() {
    let mut rules: Vec<Rule> = ESTIMATE_AI
        .iter()
        .map(|f| {
            Rule::new(*f).all(&[
                "model_change: false",
                "Global CRIS",
                "computed Part 5 result, not a fixed premium",
                "source comparison is unavailable",
            ])
        })
        .collect();
    rules.extend(
        ESTIMATE_AI
            .iter()
            .map(|f| Rule::new(*f).norm().ci().all(&["parity"])),
    );
    check_rules(&rules);
}

#[test]
fn test_resolver_covers_bare_gpt6_ids() {
    check_rules(&[Rule::new(RESOLVER).any(&["openai.gpt-6", "GPT-6"])]);
}

#[test]
fn test_code_analyzer_covers_gpt6() {
    check_rules(&[Rule::new(CODE_ANALYZER).ci().all(&["gpt-6"])]);
}

#[test]
fn test_prompt_evaluator_covers_gpt6() {
    check_rules(&[Rule::new(PROMPT_EVALUATOR).ci().all(&["gpt-6"])]);
}

#[test]
fn test_code_rewriter_covers_gpt6() {
    check_rules(&[Rule::new(CODE_REWRITER).ci().all(&["gpt-6"])]);
}

#[test]
fn test_behavior_delta_reference_covers_gpt6() {
    check_rules(&[Rule::new(BEHAVIOR_DELTA).ci().all(&["gpt-6"])]);
}

fn gpt6_source_rows(p: &Plugin) -> Verdict {
    let text = p.read(REPORT_GENERATOR)?;
    for (model, input, output) in [
        ("gpt-6-astra", "10.00", "50.00"),
        ("gpt-6.1-sol", "2.00", "10.00"),
        ("gpt-6-sol", "2.00", "10.00"),
        ("gpt-6-luna", "0.10", "0.50"),
    ] {
        let pattern = format!(
            r"\|\s*{}\s*\|\s*{}\s*\|\s*{}\s*\|",
            fancy_regex::escape(model),
            fancy_regex::escape(input),
            fancy_regex::escape(output)
        );
        ensure!(
            has(&pattern, &text),
            "§6.2 source-pricing table missing/incorrect row for {model} (expected {input}/{output})"
        );
    }
    Ok(())
}

#[test]
fn test_report_generator_section_6_2_has_gpt6_source_rows() {
    check(
        gpt6_source_rows,
        &[
            (
                edit_replace!(REPORT_GENERATOR, "gpt-6-astra", "gpt-6-x"),
                "gpt-6-astra",
            ),
            (
                edit_replace!(REPORT_GENERATOR, "gpt-6.1-sol", "gpt-6.1-x"),
                "gpt-6.1-sol",
            ),
            (
                edit_replace!(REPORT_GENERATOR, "gpt-6-luna", "gpt-6-x"),
                "gpt-6-luna",
            ),
        ],
    );
}
