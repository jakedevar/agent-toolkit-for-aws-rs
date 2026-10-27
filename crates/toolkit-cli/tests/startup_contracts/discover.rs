//! Discover and handoff wiring contracts: `test_decision_gate_wiring.py`,
//! `test_resume_and_sentinel_consistency.py` and `test_azure_discover_ordering.py`.

use crate::common::{between, check, ensure, has, idx, idx_from, name, Plugin, Verdict};
use crate::edit_replace;

// ---- test_decision_gate_wiring.py ----------------------------------------

/// Every skill exit that must emit plan.json and present the web CTA.
const HANDOFF_FILES: [&str; 6] = [
    "skills/gcp-to-aws/references/phases/estimate/estimate.md",
    "skills/azure-to-aws/references/phases/estimate/estimate.md",
    "skills/heroku-to-aws/references/phases/estimate/estimate-assemble.md",
    "skills/gcp-to-aws/references/phases/generate/generate.md",
    "skills/azure-to-aws/references/phases/generate/generate.md",
    "skills/heroku-to-aws/references/phases/generate/generate-assemble.md",
];
const AZURE_ESTIMATE: &str = HANDOFF_FILES[1];
const AZURE_ASSEMBLE: &str = "skills/azure-to-aws/references/phases/estimate/estimate-assemble.md";

fn handoff_invokes_writer(p: &Plugin) -> Verdict {
    for f in HANDOFF_FILES {
        let text = p.read(f)?;
        ensure!(
            text.contains("emit-plan-json.py"),
            "{}: handoff exit no longer invokes the plan.json writer",
            name(f)
        );
        ensure!(
            text.contains("--migration-dir"),
            "{}: writer invocation is missing its --migration-dir argument",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_handoff_exits_invoke_plan_writer() {
    check(
        handoff_invokes_writer,
        &[
            (
                edit_replace!(HANDOFF_FILES[0], "emit-plan-json.py", "x.py"),
                "no longer invokes",
            ),
            (
                edit_replace!(HANDOFF_FILES[5], "--migration-dir", "--dir"),
                "--migration-dir",
            ),
        ],
    );
}

fn handoff_presents_cta(p: &Plugin) -> Verdict {
    for f in HANDOFF_FILES {
        ensure!(
            p.read(f)?.contains("migrate/credits"),
            "{}: handoff exit no longer presents the web-import CTA",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_handoff_exits_present_import_cta() {
    check(
        handoff_presents_cta,
        &[(
            edit_replace!(HANDOFF_FILES[3], "migrate/credits", "migrate/x"),
            "CTA",
        )],
    );
}

fn exports_after_postconditions(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_ESTIMATE)?;
    let post = text.find("Evaluate `_postconditions`");
    let writer = text.find("emit-plan-json.py");
    let (Some(post), Some(writer)) = (post, writer) else {
        return Err("postconditions or writer marker missing".into());
    };
    ensure!(
        writer > post,
        "emit-plan-json.py is invoked before _postconditions is evaluated in estimate.md's Run the phase step"
    );
    ensure!(
        has(r"only after[^\n]{0,40}HANDOFF_OK", &text),
        "estimate.md's writer invocation is not explicitly gated on HANDOFF_OK, matching generate.md"
    );
    ensure!(
        !p.read(AZURE_ASSEMBLE)?.contains("emit-plan-json.py"),
        "estimate-assemble.md still invokes the writer directly; it must move to estimate.md, after _postconditions"
    );
    Ok(())
}

#[test]
fn test_estimate_exports_only_after_postconditions() {
    check(
        exports_after_postconditions,
        &[
            (
                edit_replace!(
                    AZURE_ESTIMATE,
                    "Evaluate `_postconditions`",
                    "Evaluate `_post`"
                ),
                "marker missing",
            ),
            (
                edit_replace!(AZURE_ESTIMATE, "HANDOFF_OK", "HANDOFF"),
                "gated on HANDOFF_OK",
            ),
            (
                edit_replace!(
                    AZURE_ASSEMBLE,
                    "### Step 3a",
                    "run emit-plan-json.py\n### Step 3a"
                ),
                "still invokes the writer",
            ),
        ],
    );
}

fn handoff_fail_open(p: &Plugin) -> Verdict {
    for f in HANDOFF_FILES {
        let text = p.read(f)?;
        ensure!(
            text.contains("PLAN_OK") && text.contains("PLAN_SKIP"),
            "{}: handoff exit no longer gates the CTA on the writer's PLAN_OK/PLAN_SKIP status (fail-open contract)",
            name(f)
        );
        ensure!(
            has(r"only when[^\n]{0,40}PLAN_OK", &text),
            "{}: web-handoff block is no longer gated on PLAN_OK",
            name(f)
        );
        ensure!(
            has(r"PLAN_SKIP[^\n]{0,40}omit", &text),
            "{}: web-handoff block is no longer omitted on PLAN_SKIP",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_handoff_exits_are_fail_open_gated() {
    check(
        handoff_fail_open,
        &[
            (
                edit_replace!(HANDOFF_FILES[2], "PLAN_SKIP", "PLAN_X"),
                "no longer gates",
            ),
            (
                edit_replace!(HANDOFF_FILES[4], "only when", "always when"),
                "gated on PLAN_OK",
            ),
            (
                edit_replace!(HANDOFF_FILES[0], "omit", "show"),
                "omitted on PLAN_SKIP",
            ),
        ],
    );
}

// ---- test_resume_and_sentinel_consistency.py -----------------------------

const GCP_DISCOVER: &str = "skills/gcp-to-aws/references/phases/discover/discover.md";
const AZURE_DISCOVER: &str = "skills/azure-to-aws/references/phases/discover/discover.md";
const GCP_ANTHROPIC: &str =
    "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md";
const AZURE_ANTHROPIC: &str =
    "skills/azure-to-aws/references/phases/discover/discover-anthropic-api.md";
const AZURE_OPENAI: &str = "skills/azure-to-aws/references/phases/discover/discover-openai-api.md";

fn gcp_row_status(p: &Plugin) -> Verdict {
    let text = p.read(GCP_DISCOVER)?;
    ensure!(
        text.contains("status: \"ok\"") || text.contains("status == \"ok\""),
        "gcp-to-aws discover.md no longer checks captures[] row status for the Anthropic resume/route-output-gate"
    );
    ensure!(
        text.contains("retry_pending"),
        "gcp-to-aws discover.md lost the retry_pending carve-out reference"
    );
    Ok(())
}

#[test]
fn test_gcp_discover_checks_anthropic_row_status_not_bare_existence() {
    check(
        gcp_row_status,
        &[
            (
                edit_replace!(GCP_DISCOVER, "retry_pending", "retry_x"),
                "retry_pending",
            ),
            (
                edit_replace!(GCP_DISCOVER, "status: \"ok\"", "status: ok"),
                "no longer checks captures[] row status",
            ),
        ],
    );
}

fn azure_row_status(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_DISCOVER)?;
    ensure!(
        text.contains("status: \"ok\""),
        "azure-to-aws discover.md no longer checks captures[] row status for the Anthropic resume gate"
    );
    ensure!(
        text.contains("retry_pending"),
        "azure-to-aws discover.md lost the retry_pending carve-out reference"
    );
    ensure!(
        text.contains("captures[] rows are ALL"),
        "azure-to-aws discover.md's Anthropic postcondition no longer distinguishes an all-failed manifest"
    );
    Ok(())
}

#[test]
fn test_azure_discover_checks_anthropic_row_status_not_bare_existence() {
    check(
        azure_row_status,
        &[
            (
                edit_replace!(AZURE_DISCOVER, "status: \"ok\"", "status: ok"),
                "row status",
            ),
            (
                edit_replace!(AZURE_DISCOVER, "retry_pending", "retry_x"),
                "retry_pending",
            ),
            (
                edit_replace!(AZURE_DISCOVER, "captures[] rows are ALL", "rows are"),
                "all-failed",
            ),
        ],
    );
}

fn azure_openai_workload_row(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_OPENAI)?;
    ensure!(
        text.contains("workloads[]"),
        "azure-to-aws discover-openai-api.md Step 4 still has no workloads[] append for usage-only models"
    );
    ensure!(
        text.contains("Workload row"),
        "azure-to-aws discover-openai-api.md Step 4 is missing the 'Workload row' sub-step"
    );
    Ok(())
}

#[test]
fn test_azure_openai_discover_now_appends_workload_row() {
    check(
        azure_openai_workload_row,
        &[
            (
                edit_replace!(AZURE_OPENAI, "workloads[]", "workloads"),
                "workloads[]",
            ),
            (
                edit_replace!(AZURE_OPENAI, "Workload row", "Row"),
                "Workload row",
            ),
        ],
    );
}

fn sentinel_has_no_sdk_method_qualifier(p: &Plugin) -> Verdict {
    for f in [GCP_ANTHROPIC, AZURE_ANTHROPIC] {
        let text = p.read(f)?;
        ensure!(
            !text.contains("AND `sdk_method: \"usage_api\"`"),
            "{}: Step 4.4b absence check still carries the sdk_method qualifier (finding 8 regression)",
            name(f)
        );
        ensure!(
            text.contains("any `sdk_method`"),
            "{}: Step 4.4b absence check no longer documents that it matches model_id under any sdk_method",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_anthropic_sentinel_condition_has_no_sdk_method_qualifier() {
    check(
        sentinel_has_no_sdk_method_qualifier,
        &[
            (
                edit_replace!(
                    GCP_ANTHROPIC,
                    "any `sdk_method`",
                    "AND `sdk_method: \"usage_api\"`"
                ),
                "still carries",
            ),
            (
                edit_replace!(AZURE_ANTHROPIC, "any `sdk_method`", "any method"),
                "no longer documents",
            ),
        ],
    );
}

// ---- test_azure_discover_ordering.py -------------------------------------

const USAGE_HEADINGS: [&str; 3] = [
    "OpenAI usage-API consent + capture",
    "OpenRouter usage-API consent + capture",
    "Anthropic usage-API consent + capture",
];
const PRE_DISPATCH_LIVE: &str = "## Pre-dispatch main-window action (live `az` consent + capture)";
const SHARED_INIT: &str = "### Shared Step 0 — initialize the run";
const RDFA_HEADING: &str = "### RDfA detection, guidance, and preparation";

fn usage_sections_after_phase_step(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_DISCOVER)?;
    let run_phase = idx(&text, "## Step: Run the phase")?;
    let assemble_ref = idx_from(&text, "discover-assemble.md", run_phase)?;
    for heading in USAGE_HEADINGS {
        idx_from(&text, heading, assemble_ref).map_err(|_| {
            format!("'{heading}' appears before the discover-assemble.md execution step inside '## Step: Run the phase'")
        })?;
    }
    Ok(())
}

#[test]
fn test_usage_api_sections_run_after_phase_execution_step() {
    check(
        usage_sections_after_phase_step,
        &[
            // The OpenAI heading renamed away leaves no heading after assembly.
            (
                edit_replace!(
                    AZURE_DISCOVER,
                    "OpenAI usage-API consent + capture",
                    "OpenAI consent"
                ),
                "appears before",
            ),
        ],
    );
}

fn usage_sections_not_pre_dispatch(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_DISCOVER)?;
    let section = between(&text, PRE_DISPATCH_LIVE, "## Orientation")?;
    for heading in USAGE_HEADINGS {
        ensure!(
            !section.contains(heading),
            "'{heading}' is still nested under the pre-dispatch main-window action section"
        );
    }
    Ok(())
}

#[test]
fn test_usage_api_sections_no_longer_under_pre_dispatch_heading() {
    check(
        usage_sections_not_pre_dispatch,
        &[(
            edit_replace!(
                AZURE_DISCOVER,
                "## Orientation",
                "### Anthropic usage-API consent + capture\n\n## Orientation"
            ),
            "still nested",
        )],
    );
}

fn shared_init_precedes_pre_actions(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_DISCOVER)?;
    let init = idx(&text, SHARED_INIT)?;
    let rdfa = idx(&text, RDFA_HEADING)?;
    let live = idx(&text, PRE_DISPATCH_LIVE)?;
    ensure!(
        init < rdfa && rdfa < live,
        "init < rdfa < live ordering broken"
    );
    let pre = idx(&text, "## Pre-dispatch main-window actions")?;
    let before_init = if init < pre { "" } else { &text[pre..init] };
    ensure!(
        !before_init.contains("$MIGRATION_DIR/rdfa-cache"),
        "RDfA cache access is documented before the shared run initialization"
    );
    Ok(())
}

#[test]
fn test_shared_init_precedes_both_pre_dispatch_source_actions() {
    check(
        shared_init_precedes_pre_actions,
        &[
            (
                edit_replace!(AZURE_DISCOVER, SHARED_INIT, "### Shared Step 0 — setup"),
                "not found",
            ),
            (
                edit_replace!(
                    AZURE_DISCOVER,
                    SHARED_INIT,
                    "Use $MIGRATION_DIR/rdfa-cache here.\n\n### Shared Step 0 — initialize the run"
                ),
                "before the shared run initialization",
            ),
        ],
    );
}

fn shared_init_selects_or_terminates(p: &Plugin) -> Verdict {
    let text = p.read(AZURE_DISCOVER)?;
    let section = between(&text, SHARED_INIT, RDFA_HEADING)?;
    for needle in [
        "Resolve resume-vs-fresh",
        "on a fresh run",
        "cancellation",
        "without running either action",
        "idempotent\nconfirmation",
    ] {
        ensure!(
            section.contains(needle),
            "shared init section lost {needle:?}"
        );
    }
    Ok(())
}

#[test]
fn test_shared_init_selects_or_terminates_run_before_pre_actions() {
    check(
        shared_init_selects_or_terminates,
        &[
            (
                edit_replace!(AZURE_DISCOVER, "Resolve resume-vs-fresh", "Resolve"),
                "resume-vs-fresh",
            ),
            (
                edit_replace!(AZURE_DISCOVER, "cancellation", "stop"),
                "cancellation",
            ),
            (
                edit_replace!(
                    AZURE_DISCOVER,
                    "idempotent\nconfirmation",
                    "idempotent confirmation"
                ),
                "idempotent",
            ),
        ],
    );
}
