//! `test_llm_to_bedrock_run_state.py`: the llm-to-bedrock run-state and
//! resume machine.

use crate::common::{between, check, ensure, idx, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const SKILL: &str = "skills/llm-to-bedrock/SKILL.md";
const REPORT_GENERATOR: &str = "agents/llm2bedrock-report-generator.md";
const STEP_1_5: Scope = Scope::Span("## Step 1.5", "## Phase A");
const A3: Scope = Scope::Span(
    "### A3 — Locate Assess output, then establish this skill's own run state",
    "## Phase B — Execute Prep",
);
const C0: Scope = Scope::Span("### C0 — Run-context gate", "### C1 — Analyzer");

#[test]
fn test_step_1_5_defers_the_write_instead_of_writing_immediately() {
    check_rules(&[Rule::new(SKILL)
        .scope(STEP_1_5)
        .all(&["do not write them yet", "`$BEDROCK_RUN_DIR` does not exist"])]);
}

fn a3_heading_references(p: &Plugin) -> Verdict {
    let text = p.read(SKILL)?;
    ensure!(
        text.matches("A3 — Locate Assess output").count() >= 3,
        "cross-references to the A3 block should consistently quote its heading"
    );
    Ok(())
}

#[test]
fn test_a3_heading_is_named_consistently_and_performs_the_write() {
    check_rules(&[
        Rule::new(SKILL)
            .all(&["### A3 — Locate Assess output, then establish this skill's own run state"]),
        Rule::new(SKILL)
            .scope(A3)
            .all(&["Write `usage-baseline.json`, now that `$BEDROCK_RUN_DIR` exists"]),
    ]);
    check(
        a3_heading_references,
        &[(
            edit_replace!(SKILL, "A3 — Locate Assess output", "A3 block"),
            "cross-references",
        )],
    );
}

fn write_after_run_dir_is_set(p: &Plugin) -> Verdict {
    let text = p.read(SKILL)?;
    let a3 = between(
        &text,
        "### A3 — Locate Assess output, then establish this skill's own run state",
        "## Phase B — Execute Prep",
    )?;
    let set_dir = idx(a3, "Set `$BEDROCK_RUN_DIR`")?;
    let write = idx(
        a3,
        "Write `usage-baseline.json`, now that `$BEDROCK_RUN_DIR` exists",
    )?;
    ensure!(
        write > set_dir,
        "the usage-baseline.json write instruction appears before $BEDROCK_RUN_DIR is set within A3"
    );
    Ok(())
}

#[test]
fn test_usage_baseline_write_step_in_a3_comes_after_bedrock_run_dir_is_set() {
    check(
        write_after_run_dir_is_set,
        &[
            (
                edit_replace!(
                    SKILL,
                    "Write `usage-baseline.json`, now that `$BEDROCK_RUN_DIR` exists",
                    "Skip it"
                ),
                "not found",
            ),
            (
                edit_replace!(
                    SKILL,
                    "### A3 — Locate Assess output, then establish this skill's own run state",
                    "### A3 — Locate Assess output, then establish this skill's own run state\n\nWrite `usage-baseline.json`, now that `$BEDROCK_RUN_DIR` exists\n"
                ),
                "appears before",
            ),
        ],
    );
}

#[test]
fn test_context_block_includes_usage_baseline_path_line() {
    check_rules(&[
        Rule::new(SKILL)
            .scope(Scope::Span(
                "### The context block",
                "### C0 — Run-context gate",
            ))
            .all(&["Usage baseline path:"]),
        // `split("Usage baseline path:")[1][:120]`: the 120 characters after the marker.
        Rule::new(SKILL)
            .scope(Scope::WindowIn(
                "### The context block",
                "Usage baseline path:",
                140,
            ))
            .all(&["C5/C6"]),
    ]);
}

#[test]
fn test_report_generator_reads_passed_baseline_path_not_bare_bedrock_run_dir() {
    let section = Scope::Span("## 6.3", "The migration plan may map multiple");
    check_rules(&[
        Rule::new(REPORT_GENERATOR)
            .scope(section)
            .all(&["Usage baseline path:"]),
        Rule::new(REPORT_GENERATOR)
            .scope(section)
            .ci()
            .any(&["do not derive or assume"]),
    ]);
}

#[test]
fn test_current_context_schema_includes_usage_baseline_sha256() {
    check_rules(&[Rule::new(SKILL)
        .scope(C0)
        .all(&["\"usage_baseline_sha256\""])]);
}

#[test]
fn test_mismatch_table_has_usage_baseline_sha256_row() {
    check_rules(&[
        Rule::new(SKILL)
            .scope(Scope::Within(
                "### C0 — Run-context gate",
                "| Mismatched field(s)",
                "### C1 — Analyzer",
            ))
            .all(&["usage_baseline_sha256"]),
        Rule::new(SKILL)
            .scope(Scope::LineAfter(
                "### C0 — Run-context gate",
                "| Mismatched field(s)",
                "usage_baseline_sha256",
            ))
            .all(&["REPORT"])
            .any(&["ANALYSIS", "INGESTION"]),
    ]);
}

#[test]
fn test_tiebreak_no_longer_claims_unconditional_lexicographic_chronology() {
    check_rules(&[Rule::new(SKILL).scope(STEP_1_5).none(&["IS chronological"])]);
}

#[test]
fn test_tiebreak_uses_last_updated_as_primary_sort_key() {
    check_rules(&[Rule::new(SKILL)
        .scope(STEP_1_5)
        .all(&["last_updated", "primary"])
        .any(&["same-year", "same calendar year"])]);
}

#[test]
fn test_tiebreak_surfaces_genuine_ties_to_the_user_instead_of_guessing() {
    check_rules(&[Rule::new(SKILL)
        .scope(STEP_1_5)
        .ci()
        .any(&["do not silently guess"])]);
}
