//! `test_heroku_estimate_deferred_cost_comparison.py`: a deferred database's
//! Heroku cost cannot read as savings.

use crate::rules::{check_rules, Rule, Scope};

const DESIGN_MAPPING: &str = "skills/heroku-to-aws/references/phases/design/design-mapping.md";
const COST_ENGINE: &str = "skills/heroku-to-aws/references/phases/estimate/estimate-cost-engine.md";
const ESTIMATE_ASSEMBLE: &str =
    "skills/heroku-to-aws/references/phases/estimate/estimate-assemble.md";
const GENERATE_REPORT: &str = "skills/heroku-to-aws/references/phases/generate/generate-report.md";

#[test]
fn test_design_mapping_deferred_entry_carries_resource_id() {
    check_rules(&[
        Rule::new(DESIGN_MAPPING)
            .scope(Scope::From("### 2F: Specialist Gate"))
            .all(&["\"resource_id\"", "`resource_id`"]),
        Rule::new(DESIGN_MAPPING)
            .scope(Scope::WindowIn(
                "### 2F: Specialist Gate",
                "**Required fields**",
                1200,
            ))
            .all(&["resource_id"]),
    ]);
}

#[test]
fn test_cost_engine_part1b_computes_deferred_resource_cost() {
    check_rules(&[
        Rule::new(COST_ENGINE).all(&["## Part 1b: Flag Deferred-Resource Cost"]),
        Rule::new(COST_ENGINE)
            .scope(Scope::Span(
                "## Part 1b: Flag Deferred-Resource Cost",
                "## Part 2: Calculate Projected AWS Costs",
            ))
            .all(&[
                "deferred_resource_cost",
                "deferred_resource_services",
                "deferred[]",
            ]),
    ]);
}

#[test]
fn test_cost_engine_part3_defines_comparison_status_and_keeps_full_baseline() {
    check_rules(&[Rule::new(COST_ENGINE)
        .scope(Scope::Span(
            "## Part 3: Cost Comparison",
            "## Part 4: Migration Cost Considerations",
        ))
        .all(&[
            "comparison_status",
            "incomplete_unmatched_services",
            "unmatched_deferred_cost",
            "unmatched_deferred_services",
        ])
        .any(&["do not subtract or hide", "do **not** subtract or hide"])]);
}

#[test]
fn test_cost_engine_part8_soft_trigger_table_has_comparison_status_row() {
    check_rules(&[Rule::new(COST_ENGINE)
        .scope(Scope::Within(
            "## Part 8: Recommendation",
            "Soft triggers",
            "**Outcome derivation:**",
        ))
        .all(&["comparison_status =="])]);
}

#[test]
fn test_estimate_assemble_financial_summary_propagates_comparison_status() {
    check_rules(&[Rule::new(ESTIMATE_ASSEMBLE)
        .scope(Scope::Window("\"financial_summary\"", 1500))
        .all(&["comparison_status"])]);
}

#[test]
fn test_generate_report_renders_unmatched_deferred_caveat_in_exec_costs() {
    check_rules(&[
        Rule::new(GENERATE_REPORT)
            .scope(Scope::Span("| `exec-costs`", "\n"))
            .all(&["comparison_status", "unmatched_deferred_services"]),
        Rule::new(GENERATE_REPORT)
            .scope(Scope::Span("## Step 1: Gather figures", "## Step 2:"))
            .all(&["comparison_status"]),
    ]);
}
