//! `test_azure_clarify_fast_path_contracts.py`: the Azure Clarify fast path
//! keeps its gate and provenance contracts.
//!
//! The two golden-driven tests of the Python file ran Python fixture
//! asserters; their Rust replacement is `toolkit startup check-expected`
//! (#4408), whose unit tests cover the Q-D2 boundary and the three clarify
//! goldens (see `docs/migration/startup-contracts.md`).

use crate::common::{check, ensure, has, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const CLARIFY: &str = "skills/azure-to-aws/references/phases/clarify/clarify.md";
const CLARIFY_ASSEMBLE: &str = "skills/azure-to-aws/references/phases/clarify/clarify-assemble.md";
const CLARIFY_DATABASE: &str = "skills/azure-to-aws/references/phases/clarify/clarify-database.md";
const DISCOVER_ASSEMBLE: &str =
    "skills/azure-to-aws/references/phases/discover/discover-assemble.md";
const ESTIMATE_ASSEMBLE: &str =
    "skills/azure-to-aws/references/phases/estimate/estimate-assemble.md";
const ESTIMATE_INFRA: &str = "skills/azure-to-aws/references/phases/estimate/estimate-infra.md";
const GENERATE: &str = "skills/azure-to-aws/references/phases/generate/generate.md";
const WORKSHOP_REFRESH: &str = "skills/azure-to-aws/references/phases/workshop/workshop-refresh.md";
const WORKSHOP_SHEET: &str = "skills/azure-to-aws/references/phases/workshop/workshop-sheet.md";
const WORKSHOP_COMPARE: &str = "skills/azure-to-aws/references/phases/workshop/workshop-compare.md";
const SCHEMA_PREFS: &str = "skills/azure-to-aws/references/shared/schema-preferences.md";
const SCHEMA_SCENARIOS: &str = "skills/azure-to-aws/references/shared/schema-workshop-scenarios.md";
const REPORT_CORE: &str = "skills/azure-to-aws/references/shared/report-decision-core.md";

/// The front-matter body of `text`, as the Python `_frontmatter` helper.
fn frontmatter(text: &str) -> Result<&str, String> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or("file has no YAML frontmatter")?;
    let end = rest.find("\n---\n").ok_or("file has no YAML frontmatter")?;
    Ok(&rest[..end])
}

/// First front-matter line that holds `_assert` and every one of `needles`.
fn assert_line<'a>(fm: &'a str, needles: &[&str]) -> Result<&'a str, String> {
    fm.lines()
        .find(|l| l.contains("_assert") && needles.iter().all(|n| l.contains(n)))
        .ok_or_else(|| format!("no _assert line naming {needles:?}"))
}

fn gates_are_mode_aware(p: &Plugin) -> Verdict {
    let fm_text = p.read(CLARIFY)?;
    let fm = frontmatter(&fm_text)?;
    ensure!(
        !fm.contains("isolation question was asked and its answer recorded"),
        "clarify.md: the isolation postcondition still asserts the question WAS ASKED"
    );
    for anchor in ["isolation_split", "pattern_id"] {
        let line = assert_line(fm, &[anchor])?;
        ensure!(
            line.contains("fast_path") && line.contains("questions_defaulted"),
            "clarify.md: the {anchor} postcondition has no fast-path branch keyed on metadata.clarify_mode / metadata.questions_defaulted"
        );
    }
    let every_row = assert_line(fm, &["every PROPOSED row"])?;
    ensure!(
        every_row.contains("deferred_to_generate"),
        "clarify.md: the 'every PROPOSED row ... questions_defaulted[]' postcondition does not exempt rows carrying deferred_to_generate: true"
    );
    let assemble = p.read(CLARIFY_ASSEMBLE)?;
    let checklist = crate::common::between(&assemble, "## Validation Checklist", "## Status")?;
    ensure!(
        checklist.contains("size_coverage"),
        "clarify-assemble.md checklist no longer requires size_coverage on deferred rows"
    );
    ensure!(
        has(
            r"questions_defaulted\[\]`? and `?deferred_to_generate\[\]`? share no key",
            checklist
        ),
        "clarify-assemble.md checklist dropped the disjointness line"
    );
    ensure!(
        checklist.contains("app_service_plans[n].isolation_split"),
        "clarify-assemble.md checklist no longer names the fast-path isolation list entry"
    );
    Ok(())
}

#[test]
fn test_clarify_gates_are_mode_aware() {
    check(
        gates_are_mode_aware,
        &[
            (
                edit_replace!(CLARIFY, "metadata.questions_defaulted", "metadata.defaults"),
                "has no fast-path branch",
            ),
            (
                edit_replace!(CLARIFY_ASSEMBLE, "size_coverage", "coverage"),
                "size_coverage",
            ),
            (
                edit_replace!(CLARIFY_ASSEMBLE, "share no key", "share keys"),
                "disjointness",
            ),
            (
                edit_replace!(
                    CLARIFY_ASSEMBLE,
                    "app_service_plans[n].isolation_split",
                    "app_service_plans[n].x"
                ),
                "isolation list entry",
            ),
        ],
    );
}

#[test]
fn test_db_cutover_unknown_size_branch() {
    check_rules(&[
        Rule::new(CLARIFY_DATABASE).all(&[
            "size_coverage",
            "largest_relational_db_gib",
            "\"unknown\"",
        ]),
        Rule::new(SCHEMA_PREFS).all(&["size_coverage", "largest_relational_db_gib", "\"unknown\""]),
        Rule::new(CLARIFY_DATABASE)
            .scope(Scope::Span("### Q-D2", "### Q-D3"))
            .all(&["100 GiB", "inclusive"]),
        Rule::new(DISCOVER_ASSEMBLE)
            .none(&["database cutover when a relational database is present"]),
    ]);
}

#[test]
fn test_workshop_patch_updates_default_index() {
    check_rules(&[
        Rule::new(WORKSHOP_REFRESH)
            .scope(Scope::Span("### 3. Patch preferences", "### 4"))
            .all(&["questions_defaulted", "user_corrected"]),
        Rule::new(WORKSHOP_SHEET).all(&["questions_defaulted", "clarify_mode", "user_corrected"]),
        Rule::new(ESTIMATE_ASSEMBLE)
            .scope(Scope::Span("## Step 2", "## Step 3"))
            .all(&[
                "workshop-refresh.md",
                "user_corrected",
                "user_confirmed_at_generate",
                "deferred_to_generate: false",
            ]),
    ]);
}

#[test]
fn test_step3b_clears_all_deferral_state() {
    check_rules(&[
        Rule::new(ESTIMATE_ASSEMBLE)
            .scope(Scope::Span("### Step 3b", "### Scenario reconciliation"))
            .all(&[
                "deferred_to_generate: false",
                "questions_defaulted",
                "Part 4",
                "user_stated_size_gib",
                "\"unknown\"",
            ])
            .none(&["Part 7 one-off"]),
        Rule::new(ESTIMATE_INFRA)
            .scope(Scope::Span("## Part 4", "## Part 5"))
            .all(&["user_confirmed_at_generate", "deferred_to_generate: false"]),
        Rule::new(GENERATE)
            .scope(Scope::Front)
            .all(&["deferred_to_generate: true"]),
    ]);
}

#[test]
fn test_gate_mutations_reconcile_scenarios() {
    check_rules(&[
        Rule::new(ESTIMATE_ASSEMBLE).all(&["### Scenario reconciliation"]),
        Rule::new(ESTIMATE_ASSEMBLE)
            .scope(Scope::Span("### Scenario reconciliation", "### Step 3a"))
            .all(&[
                "active_scenario_id",
                "stale: true",
                "stale_reason",
                "corrected_at_gate",
            ]),
        Rule::new(ESTIMATE_ASSEMBLE)
            .scope(Scope::Span("## Step 2", "## Step 3"))
            .all(&["Scenario reconciliation"]),
        Rule::new(ESTIMATE_ASSEMBLE)
            .scope(Scope::Span("### Step 3b", "### Scenario reconciliation"))
            .all(&["Scenario reconciliation"])
            .none(&["answer changed the estimate"]),
        Rule::new(WORKSHOP_COMPARE).all(&["stale"]),
        Rule::new(SCHEMA_SCENARIOS).all(&["stale"]),
        Rule::new(REPORT_CORE).all(&["stale"]),
        Rule::new(WORKSHOP_REFRESH).all(&["stale"]),
    ]);
}
