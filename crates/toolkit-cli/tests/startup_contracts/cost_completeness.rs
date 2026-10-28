//! `test_cost_completeness.py`: cost completeness is tracked separately from
//! window completeness, and OpenRouter cost provenance is documented.

use crate::common::{check, mirrored, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const ANTHROPIC: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md",
    "skills/azure-to-aws/references/phases/discover/discover-anthropic-api.md",
];
const OPENROUTER: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/discover/discover-openrouter-api.md",
    "skills/azure-to-aws/references/phases/discover/discover-openrouter-api.md",
];
const ESTIMATE_AI: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/estimate/estimate-ai.md",
    "skills/azure-to-aws/references/phases/estimate/estimate-ai.md",
];
const SKILL: &str = "skills/llm-to-bedrock/SKILL.md";
const REPORT_GENERATOR: &str = "agents/llm2bedrock-report-generator.md";
const STEP_1_5: Scope = Scope::Span("## Step 1.5", "## Phase A");

fn each(files: &[&'static str], make: impl Fn(Rule) -> Rule) -> Vec<Rule> {
    files.iter().map(|f| make(Rule::new(f))).collect()
}

#[test]
fn test_both_anthropic_discover_copies_document_cost_status_enum() {
    check_rules(&each(&ANTHROPIC, |r| {
        r.all(&[
            "metadata.cost_status",
            "\"complete\"",
            "\"partial_window\"",
            "\"cost_unavailable\"",
        ])
    }));
}

#[test]
fn test_both_anthropic_discover_copies_document_cost_unavailable_null_semantics() {
    check_rules(&each(&ANTHROPIC, |r| {
        r.all(&["summary.monthly_cost_usd: null"])
            .any(&["never `0`", "never 0"])
    }));
}

#[test]
fn test_both_anthropic_discover_copies_error_handling_table_sets_cost_unavailable() {
    check_rules(&each(&ANTHROPIC, |r| {
        r.scope(Scope::From("## Error Handling"))
            .all(&["cost_unavailable", "cost_report"])
    }));
}

const COST_STATUS_MARKER: &str =
    "**`metadata.cost_status` — cost completeness, independent of `partial_window`.**";

fn cost_status_mirrored(p: &Plugin) -> Verdict {
    mirrored(p, ANTHROPIC[0], ANTHROPIC[1], COST_STATUS_MARKER, 1200)
}

#[test]
fn test_anthropic_discover_copies_remain_mirrored_for_cost_status_text() {
    check(
        cost_status_mirrored,
        &[
            (edit_replace!(ANTHROPIC[0], COST_STATUS_MARKER, "**`metadata.cost_status` — x.**"), "missing"),
            (
                edit_replace!(
                    ANTHROPIC[1],
                    "**`metadata.cost_status` — cost completeness, independent of `partial_window`.**",
                    "**`metadata.cost_status` — cost completeness, independent of `partial_window`.** (drift)"
                ),
                "drifted",
            ),
        ],
    );
}

#[test]
fn test_both_estimate_ai_files_exclude_cost_unavailable_dollars_from_part_1() {
    check_rules(&each(&ESTIMATE_AI, |r| {
        r.scope(Scope::Span("## Part 1", "## Part 2"))
            .all(&["cost_status", "cost_unavailable"])
            .any(&[
                "exclude it from this SUM",
                "do NOT rank it",
                "fall through",
                "fall straight through",
                "cover that provider",
            ])
    }));
}

#[test]
fn test_both_estimate_ai_files_still_count_cost_unavailable_volume_in_part_2() {
    check_rules(&each(&ESTIMATE_AI, |r| {
        r.scope(Scope::Span("## Part 2", "## Part 3"))
            .all(&["cost_status", "cost_unavailable"])
            .any(&["does NOT disqualify", "normal"])
    }));
}

#[test]
fn test_skill_md_usage_baseline_schema_has_cost_status_per_window() {
    check_rules(&[Rule::new(SKILL).all(&["\"cost_status\"", "windows.<provider>.cost_status"])]);
}

#[test]
fn test_skill_md_sum_step_excludes_cost_unavailable_dollars() {
    check_rules(&[Rule::new(SKILL).scope(STEP_1_5).all(&[
        "cost_status",
        "cost_unavailable",
        "EXCLUDE it from this dollar SUM",
    ])]);
}

#[test]
fn test_report_generator_documents_cost_unavailable_presentation_rule() {
    check_rules(&[
        Rule::new(REPORT_GENERATOR)
            .scope(Scope::Span("## 6.3", "# 6.4"))
            .all(&["cost_status", "cost_unavailable"]),
        Rule::new(REPORT_GENERATOR)
            .scope(Scope::Span("## 6.3", "# 6.4"))
            .ci()
            .all(&["unavailable", "savings"]),
    ]);
}

#[test]
fn test_both_openrouter_discover_copies_document_real_cost_provenance_detection() {
    check_rules(&each(&OPENROUTER, |r| {
        r.all(&[
            "metadata.cost_provenance",
            "usage_by_model[].cost_provenance",
            "\"byok_passthrough\" | \"mixed\" | \"no_byok\" |",
            "POST /analytics/query",
            "byok_usage",
            "openrouter_usage",
        ])
    }));
}

const PROVENANCE_MARKER: &str = "**`cost_provenance` — now derived from `/analytics/query`";

fn provenance_mirrored(p: &Plugin) -> Verdict {
    mirrored(p, OPENROUTER[0], OPENROUTER[1], PROVENANCE_MARKER, 1800)
}

#[test]
fn test_openrouter_discover_copies_remain_mirrored_for_cost_provenance_text() {
    check(
        provenance_mirrored,
        &[
            (
                edit_replace!(OPENROUTER[1], PROVENANCE_MARKER, "**`cost_provenance` — x"),
                "missing",
            ),
            (
                edit_replace!(
                    OPENROUTER[0],
                    "now derived from `/analytics/query`",
                    "now derived from `/analytics/query` (drift)"
                ),
                "drifted",
            ),
        ],
    );
}

#[test]
fn test_both_openrouter_discover_copies_have_fourth_analytics_table_row() {
    check_rules(&each(&OPENROUTER, |r| {
        r.all(&["| 4 | `POST /analytics/query`", "analytics.json"])
    }));
}

#[test]
fn test_both_anthropic_discover_copies_detect_priority_tier_rows() {
    check_rules(&each(&ANTHROPIC, |r| {
        r.all(&["service_tier", "\"priority\"", "\"priority_on_demand\""])
    }));
}

#[test]
fn test_both_anthropic_discover_copies_do_not_mark_priority_tier_as_complete() {
    check_rules(&each(&ANTHROPIC, |r| {
        r.all(&[
            "Priority Tier dollars are not in",
            "no Priority Tier usage was detected",
        ])
    }));
}

#[test]
fn test_skill_md_sum_step_has_byok_overlap_exception() {
    check_rules(&[Rule::new(SKILL).scope(STEP_1_5).all(&[
        "byok_passthrough",
        "cost_provenance",
        "openrouter-usage-profile.json",
    ])]);
}

#[test]
fn test_skill_md_step15_consumes_per_model_cost_provenance() {
    check_rules(&[
        Rule::new(SKILL)
            .scope(STEP_1_5)
            .all(&["no_byok", "usage_by_model[].cost_provenance"]),
        Rule::new(SKILL)
            .scope(STEP_1_5)
            .ci()
            .any(&["old profile", "backward compat"]),
    ]);
}
