//! `test_ai_only_option_c_framework_route.py`: the AI-only "Option C" route
//! label consults the framework when `migration_path` is absent.

use crate::common::{check, ensure, from, idx, name, Plugin, Verdict};
use crate::edit_replace;

const GCP_ESTIMATE: &str = "skills/gcp-to-aws/references/phases/estimate/estimate.md";
const AZURE_ASSEMBLE: &str = "skills/azure-to-aws/references/phases/estimate/estimate-assemble.md";
const FILES: [&str; 2] = [GCP_ESTIMATE, AZURE_ASSEMBLE];
const GATEWAY_TOKENS: [&str; 4] = ["llm_router", "api_gateway", "voice_platform", "framework"];

/// The route-resolution block: everything from the first "OpenRouter subroute".
fn section(text: &str) -> Result<String, String> {
    from(text, "OpenRouter subroute")
}

fn absent_path_consults_framework(p: &Plugin) -> Verdict {
    for f in FILES {
        let s = section(&p.read(f)?)?;
        ensure!(
            s.contains("ai_constraints.ai_framework.value"),
            "{}: option-C resolution no longer consults ai_constraints.ai_framework.value",
            name(f)
        );
        ensure!(
            s.contains("absent/missing `migration_path`"),
            "{}: the framework-precedence check is not tied to an absent/missing migration_path",
            name(f)
        );
        for token in GATEWAY_TOKENS {
            ensure!(
                s.contains(token),
                "{}: gateway-framework token '{token}' missing from the precedence check",
                name(f)
            );
        }
    }
    Ok(())
}

#[test]
fn test_absent_migration_path_consults_framework_before_adapter() {
    check(
        absent_path_consults_framework,
        &[
            (
                edit_replace!(
                    GCP_ESTIMATE,
                    "ai_constraints.ai_framework.value",
                    "ai_constraints.x"
                ),
                "no longer consults",
            ),
            (
                edit_replace!(
                    AZURE_ASSEMBLE,
                    "absent/missing `migration_path`",
                    "missing path"
                ),
                "not tied",
            ),
            (
                edit_replace!(AZURE_ASSEMBLE, "voice_platform", "voice"),
                "voice_platform",
            ),
        ],
    );
}

fn mirrors_generate_dispatch(p: &Plugin) -> Verdict {
    for f in FILES {
        ensure!(
            section(&p.read(f)?)?.contains("generate-artifacts-ai.md"),
            "{}: the framework-precedence rule no longer cites generate-artifacts-ai.md",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_framework_consult_mirrors_generate_dispatch() {
    check(
        mirrors_generate_dispatch,
        &[(
            edit_replace!(GCP_ESTIMATE, "generate-artifacts-ai.md", "generate-x.md"),
            "no longer cites",
        )],
    );
}

fn spellings_and_priority(p: &Plugin) -> Verdict {
    for f in FILES {
        let text = p.read(f)?;
        ensure!(
            text.contains("mantle_openai_responses"),
            "{}: the Mantle spelling 'mantle_openai_responses' was lost",
            name(f)
        );
        ensure!(
            text.to_lowercase().contains("converse"),
            "{}: the Converse client-swap mapping was lost",
            name(f)
        );
        let s = section(&text)?;
        ensure!(
            idx(&s, "OpenRouter subroute")? < idx(&s, "migration_path")?,
            "{}: the OpenRouter subroute no longer takes priority over migration_path",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_dd38bc2_spellings_and_openrouter_priority_preserved() {
    check(
        spellings_and_priority,
        &[
            (
                edit_replace!(GCP_ESTIMATE, "mantle_openai_responses", "mantle_x"),
                "Mantle spelling",
            ),
            (
                edit_replace!(AZURE_ASSEMBLE, "OpenRouter subroute", "OpenRouter route"),
                "not found",
            ),
        ],
    );
}
