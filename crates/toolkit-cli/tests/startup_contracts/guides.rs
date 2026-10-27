//! Single-file guide contracts: `test_azure_discovery_source_guidance.py`,
//! `test_gcp_design_guide_selection.py`, `test_estimate_assemble_stale_artifacts.py`,
//! `test_estimate_option_a_single_close.py` and `test_gemini_guide_body_parity.py`.

use crate::common::{between, check, ensure, Plugin, Verdict};
use crate::{edit_empty, edit_replace};

const AZURE_SKILL: &str = "skills/azure-to-aws/SKILL.md";
const README: &str = "README.md";
const GCP_DESIGN_AI: &str = "skills/gcp-to-aws/references/phases/design/design-ai.md";
const ESTIMATE_ASSEMBLE: &str =
    "skills/azure-to-aws/references/phases/estimate/estimate-assemble.md";
const ESTIMATE_MD: &str = "skills/azure-to-aws/references/phases/estimate/estimate.md";
const SHARED_GUIDE: &str = "skills/shared/ai/ai-gemini-to-bedrock.md";
const ADAPTER_GUIDE: &str = "skills/gcp-to-aws/references/design-refs/ai-gemini-to-bedrock.md";
const CLOSE_SENTENCE: &str = "Your decision is saved at";

// ---- test_azure_discovery_source_guidance.py -----------------------------

fn skill_summary(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_SKILL)?;
    // text.split("---", 2)[1]
    let description = text.splitn(3, "---").nth(1).ok_or("no front matter")?;
    let defaults = between(&text, "## Defaults", "## Scope Notes")?;
    ensure!(description.contains("RDfA"), "description never names RDfA");
    ensure!(
        !description.contains("app code, and billing exports"),
        "description still lists billing exports as a source"
    );
    ensure!(
        description.contains("billing-export discovery is not implemented"),
        "description lost the not-implemented statement"
    );
    ensure!(
        defaults.contains("RDfA archive parsing"),
        "defaults lost RDfA archive parsing"
    );
    ensure!(
        !defaults.contains("billing exports as a fallback"),
        "defaults still offer billing exports as a fallback"
    );
    ensure!(
        defaults.contains("billing exports are not implemented"),
        "defaults lost the billing-exports-not-implemented line"
    );
    Ok(())
}

#[test]
fn test_skill_summary_names_rdfa_and_not_billing_exports() {
    check(
        skill_summary,
        &[
            (
                edit_replace!(
                    AZURE_SKILL,
                    "billing-export discovery is not implemented",
                    "x"
                ),
                "not-implemented",
            ),
            (
                edit_replace!(AZURE_SKILL, "RDfA archive parsing", "archive parsing"),
                "RDfA archive parsing",
            ),
            (
                edit_replace!(
                    AZURE_SKILL,
                    "billing exports are not implemented",
                    "billing exports as a fallback"
                ),
                "fallback",
            ),
        ],
    );
}

fn readme_names_rdfa(p: &Plugin) -> Verdict {
    let text = p.read(README)?;
    let azure_row = text
        .lines()
        .find(|l| l.starts_with("| 4 | `azure-to-aws` |"))
        .ok_or("no azure-to-aws table row")?;
    let discover = between(&text, "- **Discover** —", "- **Clarify** —")?;
    let azure_clause = discover.split("`gcp-to-aws`").next().unwrap_or("");
    ensure!(azure_row.contains("RDfA"), "azure row never names RDfA");
    ensure!(
        !azure_row.contains("and billing exports"),
        "azure row still lists billing exports"
    );
    ensure!(
        azure_row.contains("not Bicep, ARM templates, or Azure billing exports"),
        "azure row lost the not-Bicep clause"
    );
    ensure!(
        azure_clause.contains("RDfA"),
        "Discover clause never names RDfA"
    );
    ensure!(
        !azure_clause
            .replace("Azure billing exports", "")
            .contains("billing exports"),
        "Discover clause still lists billing exports for Azure"
    );
    ensure!(
        discover.contains("`gcp-to-aws` reads Terraform, application code, and billing exports"),
        "GCP lost its billing-export source"
    );
    Ok(())
}

#[test]
fn test_plugin_readme_names_rdfa_for_azure_only() {
    check(
        readme_names_rdfa,
        &[
            (
                edit_replace!(
                    README,
                    "not Bicep, ARM templates, or Azure billing exports",
                    "x"
                ),
                "not-Bicep",
            ),
            (
                edit_replace!(
                    README,
                    "`gcp-to-aws` reads Terraform, application code, and billing exports",
                    "`gcp-to-aws` reads Terraform and application code"
                ),
                "GCP lost",
            ),
        ],
    );
}

// ---- test_gcp_design_guide_selection.py ----------------------------------

fn both_not_hardcoded(p: &Plugin) -> Verdict {
    let text = p.read(GCP_DESIGN_AI)?;
    ensure!(
        !text.contains("load both `ai-gemini-to-bedrock.md` and `ai-openai-to-bedrock.md`"),
        "GCP design-ai.md's 'both' branch still hardcodes exactly Gemini+OpenAI (finding 9 regression)"
    );
    Ok(())
}

#[test]
fn test_both_branch_no_longer_hardcodes_gemini_and_openai_only() {
    check(
        both_not_hardcoded,
        &[(
            edit_replace!(
                GCP_DESIGN_AI,
                "ACTUALLY",
                "load both `ai-gemini-to-bedrock.md` and `ai-openai-to-bedrock.md`"
            ),
            "hardcodes",
        )],
    );
}

fn both_selects_by_presence(p: &Plugin) -> Verdict {
    let text = p.read(GCP_DESIGN_AI)?;
    ensure!(
        text.contains("ai-anthropic-to-bedrock.md"),
        "GCP design-ai.md's 'both' branch never loads the Anthropic guide"
    );
    let start = text.find("- `\"both\"`").ok_or("no 'both' branch")?;
    let section: String = text[start..].chars().take(1200).collect();
    for guide in [
        "ai-gemini-to-bedrock.md",
        "ai-openai-to-bedrock.md",
        "ai-anthropic-to-bedrock.md",
    ] {
        ensure!(
            section.contains(guide),
            "GCP design-ai.md's 'both' branch no longer references {guide}"
        );
    }
    ensure!(
        section.contains("ACTUALLY"),
        "GCP design-ai.md's 'both' branch no longer explains it selects guides by actual provider presence"
    );
    Ok(())
}

#[test]
fn test_both_branch_selects_guides_from_actual_provider_presence() {
    check(
        both_selects_by_presence,
        &[
            (
                edit_replace!(GCP_DESIGN_AI, "ACTUALLY", "always"),
                "ACTUALLY",
            ),
            (
                edit_replace!(GCP_DESIGN_AI, "ai-anthropic-to-bedrock.md", "ai-x.md"),
                "never loads the Anthropic guide",
            ),
        ],
    );
}

// ---- test_estimate_assemble_stale_artifacts.py ---------------------------

fn assemble_invalidates_stale(p: &Plugin) -> Verdict {
    let text = p.read(ESTIMATE_ASSEMBLE)?;
    ensure!(
        text.contains("estimation-ai.json") && text.to_lowercase().contains("stale"),
        "estimate-assemble.md no longer mentions invalidating a stale estimation-ai.json"
    );
    ensure!(
        text.contains("estimation-infra.json"),
        "estimate-assemble.md no longer covers the infra-side stale-artifact case"
    );
    let lower = text.to_lowercase();
    ensure!(
        lower.contains("delete") || lower.contains("invalidat"),
        "estimate-assemble.md no longer describes deleting/invalidating the stale artifact"
    );
    Ok(())
}

#[test]
fn test_azure_estimate_assemble_invalidates_stale_sibling_route_artifact() {
    check(
        assemble_invalidates_stale,
        &[
            (
                edit_replace!(
                    ESTIMATE_ASSEMBLE,
                    "estimation-infra.json",
                    "estimation-x.json"
                ),
                "infra-side",
            ),
            (
                edit_replace!(ESTIMATE_ASSEMBLE, "estimation-ai.json", "x.json"),
                "stale",
            ),
            (edit_empty!(ESTIMATE_ASSEMBLE), "stale"),
        ],
    );
}

// ---- test_estimate_option_a_single_close.py ------------------------------

fn step_3a_section(text: &str) -> Result<&str, String> {
    let start = text
        .find("### Step 3a")
        .ok_or("estimate-assemble.md: Step 3a section not found")?;
    let end = text[start..]
        .find("\n---")
        .or_else(|| text[start..].find("\n## Step 4"))
        .ok_or("estimate-assemble.md: could not bound Step 3a section")?;
    Ok(&text[start..start + end])
}

fn step_3a_no_close(p: &Plugin) -> Verdict {
    let assemble = p.read(ESTIMATE_ASSEMBLE)?;
    let section = step_3a_section(&assemble)?;
    ensure!(
        !section.contains(CLOSE_SENTENCE),
        "estimate-assemble.md Step 3a emits its own close message again ('{CLOSE_SENTENCE} ...')"
    );
    Ok(())
}

#[test]
fn test_step_3a_does_not_emit_its_own_close() {
    check(
        step_3a_no_close,
        &[(
            edit_replace!(
                ESTIMATE_ASSEMBLE,
                "### Step 3a",
                "### Step 3a\n\nYour decision is saved at X.\n"
            ),
            "emits its own close",
        )],
    );
}

fn estimate_is_only_close_owner(p: &Plugin) -> Verdict {
    let estimate = p.read(ESTIMATE_MD)?;
    ensure!(
        estimate.contains(CLOSE_SENTENCE),
        "estimate.md no longer closes Option A with the decision-saved sentence"
    );
    let total = estimate.matches(CLOSE_SENTENCE).count()
        + p.read(ESTIMATE_ASSEMBLE)?.matches(CLOSE_SENTENCE).count();
    ensure!(
        total == 1,
        "expected exactly one close-message owner across estimate.md + estimate-assemble.md, found {total}"
    );
    Ok(())
}

#[test]
fn test_estimate_md_step_4_is_the_only_close_owner() {
    check(
        estimate_is_only_close_owner,
        &[
            (
                edit_replace!(ESTIMATE_MD, CLOSE_SENTENCE, "Saved at"),
                "no longer closes",
            ),
            (
                edit_replace!(
                    ESTIMATE_ASSEMBLE,
                    "### Step 3a",
                    "Your decision is saved at X\n### Step 3a"
                ),
                "found 2",
            ),
        ],
    );
}

// ---- test_gemini_guide_body_parity.py ------------------------------------

const BODY_ANCHOR: &str = "## Competitive Reality";

fn guide_body(p: &Plugin, rel: &str) -> Result<String, String> {
    let text = p.read(rel)?;
    let idx = text
        .find(BODY_ANCHOR)
        .ok_or_else(|| format!("{rel} is missing the body anchor {BODY_ANCHOR:?}"))?;
    Ok(text[idx..].to_owned())
}

fn both_guides_exist(p: &Plugin) -> Verdict {
    ensure!(p.is_file(SHARED_GUIDE), "missing shared Gemini guide");
    ensure!(
        p.is_file(ADAPTER_GUIDE),
        "missing adapter-local Gemini guide"
    );
    Ok(())
}

#[test]
fn test_both_gemini_guides_exist() {
    // A missing file cannot be produced by an in-place edit of a copy, so the
    // negative case points the view at a root that lacks the file.
    check_missing_file(both_guides_exist);
}

/// Prove `contract` fails when the guide is absent from the view: edit a
/// different file, then ask a view whose real root has no such file.
fn check_missing_file(contract: fn(&Plugin) -> Verdict) {
    let real = Plugin::real();
    contract(&real).expect("the committed plugin keeps both guides");
    let absent = Plugin::at(std::env::temp_dir().join("startup-contracts-absent-root"));
    assert!(
        contract(&absent).is_err(),
        "a root without the guides passed"
    );
}

fn bodies_identical(p: &Plugin) -> Verdict {
    ensure!(
        guide_body(p, SHARED_GUIDE)? == guide_body(p, ADAPTER_GUIDE)?,
        "Gemini mapping-guide bodies have drifted between {ADAPTER_GUIDE} and {SHARED_GUIDE}"
    );
    Ok(())
}

#[test]
fn test_gemini_guide_bodies_are_byte_identical() {
    check(
        bodies_identical,
        &[
            (
                edit_replace!(ADAPTER_GUIDE, BODY_ANCHOR, "## Competitive Reality\n\nx"),
                "drifted",
            ),
            (
                edit_replace!(SHARED_GUIDE, BODY_ANCHOR, "## Other"),
                "missing the body anchor",
            ),
        ],
    );
}
