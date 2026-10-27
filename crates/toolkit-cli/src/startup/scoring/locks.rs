//! Content locks of the agent-advisor prose (#4400): the ported
//! `test_cost_levers.py`, `test_poc_shapes.py`, `test_temporal_decision_refs.py`,
//! `test_workload_classes.py` and the lifecycle self-proofs of `test_scoring.py`.
//!
//! The decision references and phase files are markdown contracts the model
//! follows. These tests pin their security- and cost-load-bearing sentences so
//! a wording "simplification" cannot silently weaken them. Patterns are the
//! Python ones (`fancy-regex` accepts them); `norm` collapses whitespace like
//! the Python `re.sub(r"\s+", " ", ...)` because markdown wraps lines.

#![allow(clippy::too_many_lines)]

use std::fs;

use fancy_regex::Regex;

use super::fixtures::real_skill;
use super::{legal_values, DIMENSIONS};
use crate::startup::text::is_space;

mod grouping;

/// The text of a file below the agent-advisor skill.
pub(super) fn raw(relative: &str) -> String {
    let path = real_skill().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("cannot read {relative}: {error}"))
}

/// `re.sub(r"\s+", " ", text)`.
pub(super) fn squash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// A skill file with its whitespace collapsed.
pub(super) fn norm(relative: &str) -> String {
    squash(&raw(relative))
}

fn pattern(source: &str) -> Regex {
    Regex::new(source).unwrap_or_else(|error| panic!("invalid pattern {source}: {error}"))
}

/// `re.search(...)` is not `None`; flags are written inline (`(?is)`).
pub(super) fn has(text: &str, source: &str) -> bool {
    pattern(source).is_match(text).expect("the match runs")
}

/// The text of the whole match of the first `re.search` hit.
pub(super) fn found(text: &str, source: &str) -> Option<String> {
    pattern(source)
        .find(text)
        .expect("the match runs")
        .map(|hit| hit.as_str().to_string())
}

/// Capture group `index` of the first `re.search` hit.
pub(super) fn group(text: &str, source: &str, index: usize) -> Option<String> {
    pattern(source)
        .captures(text)
        .expect("the match runs")
        .and_then(|captures| captures.get(index))
        .map(|hit| hit.as_str().to_string())
}

/// `len(re.findall(...))`.
pub(super) fn count(text: &str, source: &str) -> usize {
    pattern(source).find_iter(text).flatten().count()
}

/// The text between the first two `---` fences (`text.split("---", 2)[1]`),
/// or empty when there are fewer than two.
pub(super) fn frontmatter(text: &str) -> String {
    if text.matches("---").count() >= 2 {
        text.split("---").nth(1).unwrap_or("").to_string()
    } else {
        String::new()
    }
}

const COST_LEVERS: &str = "references/decision-refs/cost-levers.md";
const ESTIMATE: &str = "references/phases/estimate/estimate.md";
const SHAPES: &str = "references/decision-refs/poc-shapes.md";
const POC: &str = "references/phases/poc/poc.md";
const POC_REPORT: &str = "references/phases/poc/poc-report.md";
const TEMPORAL: &str = "references/decision-refs/temporal.md";
const GENERATE: &str = "references/phases/generate/generate.md";
const WORKLOAD_CLASSES: &str = "references/decision-refs/workload-classes.md";
const DESIGN: &str = "references/phases/design/design.md";

// ---- test_cost_levers.py -----------------------------------------------------------------------

#[test]
fn cost_levers_file_exists_and_is_the_only_legal_source() {
    let text = raw(COST_LEVERS);
    assert!(text.to_lowercase().contains("only legal source"));
}

#[test]
fn five_levers_present() {
    let text = raw(COST_LEVERS);
    for lever in [
        "Model tier routing",
        "Prompt caching",
        "Batch inference",
        "Scale-to-zero runtimes",
        "Quota scheduling",
    ] {
        assert!(
            text.contains(lever),
            "Lever '{lever}' not found in cost-levers.md"
        );
    }
}

#[test]
fn estimate_references_cost_levers_and_drivers() {
    let estimate = raw(ESTIMATE);
    assert!(estimate.contains("cost-levers.md"));
    assert!(estimate.contains("drivers[]") || estimate.contains("drivers["));
}

// ---- test_poc_shapes.py ------------------------------------------------------------------------

#[test]
fn poc_ecs_create_allowlist_and_never_list() {
    let text = norm(SHAPES);
    assert!(text.contains("Terraform may create ONLY"));
    assert!(text.contains("Terraform must NEVER create"));
    for never in ["VPC", "subnets", "NAT gateway", "internet gateway", "ALB"] {
        assert!(
            text.contains(never),
            "ECS never-list item missing: {never:?}"
        );
    }
    // No-ALB smoke path: a service that deploys but can't be invoked isn't a POC.
    assert!(text.contains("No ALB"));
    assert!(text.contains("run-task") || text.contains("execute-command"));
}

#[test]
fn poc_ecs_networking_never_creates_vpc() {
    let text = norm(SHAPES);
    assert!(text.contains("default VPC"));
    assert!(
        text.contains("never create one") || text.to_lowercase().contains("never create a vpc")
    );
}

#[test]
fn poc_eks_unique_namespace_and_no_iam() {
    let text = norm(SHAPES);
    assert!(
        text.contains("agent-advisor-poc-<run_id>"),
        "EKS namespace must be unique per run: a fixed name risks deleting a user's namespace"
    );
    assert!(text.contains("Never a fixed name"));
    assert!(text.contains("ClusterIP"));
    assert!(text.contains("No LoadBalancer"));
    assert!(text.contains("creates no IAM resources"));
}

#[test]
fn poc_eks_never_creates_cluster() {
    assert!(norm(SHAPES).contains("Never creates an EKS cluster"));
}

#[test]
fn poc_lambda_url_auth_is_iam_never_none() {
    let text = norm(SHAPES);
    assert!(text.contains("AWS_IAM"));
    assert!(
        text.contains("never `NONE`") || text.contains("never NONE"),
        "a public Bedrock-invoking Function URL is an open cost hole"
    );
}

#[test]
fn poc_microvms_disabled_fallback() {
    let text = norm(SHAPES);
    assert!(text.contains("microvms.tf.disabled"));
    assert!(text.contains("MicroVMs config pending verification"));
    assert!(text.contains("No deployable claim"));
}

#[test]
fn poc_temporal_tls_contract_complete() {
    // All connection vars, explicitly: the API-key-implies-TLS binary broke
    // mTLS self-hosted servers.
    let text = norm(SHAPES);
    for var in [
        "TEMPORAL_ADDRESS",
        "TEMPORAL_NAMESPACE",
        "TEMPORAL_TLS",
        "TEMPORAL_API_KEY",
        "TEMPORAL_TLS_SERVER_NAME",
        "TEMPORAL_TLS_CA_PATH",
        "TEMPORAL_TLS_CERT_PATH",
        "TEMPORAL_TLS_KEY_PATH",
    ] {
        assert!(text.contains(var), "TLS contract var missing: {var}");
    }
    assert!(text.contains("never inferred"));
}

#[test]
fn poc_temporal_smoke_queue_isolated() {
    let text = norm(SHAPES);
    assert!(text.contains("poc-smoke-<run_id>"));
    assert!(
        text.to_lowercase().contains("never the user's real queues")
            || text.contains("never touched production queues")
    );
}

#[test]
fn poc_temporal_apply_proves_nothing() {
    // terraform apply succeeding is not pickup proof: the starter's result is.
    let text = norm(SHAPES);
    assert!(
        text.contains("apply alone proves nothing") || text.contains("proves nothing by itself")
    );
}

#[test]
fn poc_temporal_teardown_wording() {
    let text = norm(SHAPES);
    assert!(text.contains("not deletable resources"));
    assert!(text.contains("ages out"));
}

#[test]
fn poc_temporal_secrets_never_inline() {
    let text = norm(SHAPES);
    assert!(text.contains("SSM Parameter Store") || text.contains("SSM SecureString"));
    assert!(text.contains("never inline"));
}

#[test]
fn poc_md_dispatches_to_shapes() {
    let text = norm(POC);
    assert!(text.contains("poc-shapes.md"));
    assert!(text.contains("Per-unit runtime dispatch"));
    // The old skip message must be gone.
    assert!(!text.contains("supports AgentCore only"));
}

#[test]
fn poc_plan_eks_uses_kubectl_not_terraform_runtask() {
    // The EKS shape is kubectl apply / port-forward / kubectl delete, not
    // Terraform + run-task.
    let text = norm(POC);
    assert!(
        has(&text, r"(?i)eks.{0,120}kubectl apply"),
        "EKS plan must deploy with kubectl apply, not terraform"
    );
    assert!(
        has(&text, r"(?i)eks.{0,80}kubectl delete"),
        "EKS rollback must be kubectl delete, not terraform destroy / agentcore destroy"
    );
}

#[test]
fn poc_plan_covers_lambda_microvms() {
    let text = norm(POC);
    assert!(
        text.matches("lambda_microvms").count() >= 2,
        "poc.md plan must cover lambda_microvms in both staged steps and rollback"
    );
}

#[test]
fn poc_md_dispatches_temporal_worker_poll() {
    let text = norm(POC);
    assert!(text.contains("temporal_worker_poll"));
    assert!(text.contains("Temporal worker POC"));
    assert!(text.contains("shape in") && text.contains("poc-shapes.md"));
}

#[test]
fn poc_skill_md_rows() {
    // SKILL.md's execution moved to the interpreter-loop delegation; the two
    // invariants this test pins are unchanged.
    let text = norm("SKILL.md");
    assert!(
        !text.contains("load `references/phases/temporal-worker"),
        "the temporal branch must not be routable"
    );
    let gate2 = group(&text, r"\*\*Gate 2 → `poc`\*\*(.*?)- Persisting", 1)
        .expect("SKILL.md must carry the Gate 2 → poc semantics");
    assert!(gate2.contains("Any winning runtime"));
    assert!(!gate2.contains("== agentcore"));
}

#[test]
fn poc_report_uses_v3_shell() {
    let text = norm(POC_REPORT);
    assert!(text.contains("report-shell.md"));
    assert!(text.contains("SHARED_SHELL_CSS") || text.to_lowercase().contains("shared shell"));
    assert!(
        text.contains("SHARED_SHELL_MERMAID_TAG")
            || text.to_lowercase().contains("sri-pinned mermaid")
    );
    assert!(text.contains(".doc-head"));
    assert!(text.contains(".help-strip") || text.contains("HELP_URL"));
}

#[test]
fn poc_model_resolved_per_unit() {
    let text = norm(POC);
    let lower = text.to_lowercase();
    assert!(
        lower.contains("per unit")
            || lower.contains("for each unit")
            || text.contains("units[<id>]")
            || text.contains("units[<unit-id>]"),
        "Step 2 must resolve model per-unit"
    );
    assert!(
        lower.contains("single source of truth") || lower.contains("authoritative"),
        "Step 2 must state design.json is the authoritative model source"
    );
    assert!(
        text.contains("THAT unit's model") || lower.contains("unit's model id"),
        "Step 3 dispatch must use each unit's model id"
    );
}

#[test]
fn poc_model_not_taken_from_first_block_for_all_units() {
    let text = norm(POC);
    assert!(
        text.contains("NOT `design_blocks[0]`")
            || text.to_lowercase().contains("not `design_blocks[0]`"),
        "the plan-backed cross-check must warn against using design_blocks[0] for all units"
    );
    assert!(text.contains("source_paths") && text.contains("evidence"));
}

#[test]
fn poc_dispatches_on_effective_runtime_not_raw_verdict() {
    let text = norm(POC);
    assert!(text.to_lowercase().contains("effective runtime"));
    assert!(
        has(&text, r#"platform\.mode\s*==\s*"?consolidated"?"#),
        "poc.md must key the consolidated case on platform.mode == consolidated"
    );
    assert!(text.contains("platform.runtime"));
}

#[test]
fn poc_postcondition_dispatches_on_effective_runtime() {
    let text = norm(POC);
    let front = frontmatter(&text);
    assert!(front.contains("effective_runtime"));
    assert!(has(
        &front,
        r"(?i)NOT the raw split verdict|not the raw.{0,20}verdict"
    ));
}

#[test]
fn poc_dispatch_includes_fargate() {
    let poc = norm(POC);
    assert!(
        poc.contains("fargate"),
        "poc.md dispatch must include fargate"
    );
    let shapes = norm(SHAPES);
    assert!(
        has(
            &shapes,
            r"(?i)fargate.{0,80}ecs shape|`fargate`.{0,80}ecs|ecs.{0,120}fargate"
        ),
        "the ecs shape must state it also serves fargate verdicts"
    );
    assert!(
        has(
            &shapes,
            r"(?is)W5.{0,200}(does NOT provision an ALB|not the public ALB|without.{0,20}ALB)"
        ),
        "the ecs shape must state the W5 ALB scope limit"
    );
}

#[test]
fn poc_batch_shape_exists_with_allowlist_secrets_and_teardown() {
    let text = norm(SHAPES);
    assert!(
        text.contains("## batch"),
        "poc-shapes.md must have a ## batch section"
    );
    for resource in [
        "aws_batch_compute_environment",
        "aws_batch_job_queue",
        "aws_batch_job_definition",
    ] {
        assert!(text.contains(resource), "{resource}");
    }
    for never in [
        "VPC",
        "subnets",
        "NAT gateway",
        "internet gateway",
        "always-on compute",
    ] {
        assert!(
            text.contains(never),
            "Batch never-list item missing: {never:?}"
        );
    }
    assert!(
        text.contains("SSM Parameter Store")
            || text.contains("SSM SecureString")
            || text.contains("Secrets Manager")
    );
    assert!(text.contains("never inline"));
    assert!(text.contains("not deletable") || text.contains("not deletable resources"));
    assert!(text.contains("drain") || text.contains("ages out"));
}

#[test]
fn poc_md_dispatches_batch() {
    let text = norm(POC);
    assert!(text.contains("batch"));
    assert!(
        text.contains("batch / ecs / eks / lambda / lambda_microvms")
            || text.contains("batch/ecs/eks/lambda/lambda_microvms")
            || text.contains("ecs / eks / lambda / lambda_microvms"),
        "poc.md Step 3 dispatch must include batch"
    );
}

#[test]
fn poc_md_multiunit_dispatch_is_unconditionally_first() {
    let text = norm(POC);
    assert!(
        text.contains("Multi-unit system decision gate")
            || text.contains("equals design.json.units[].length")
    );
    assert!(text.contains("Temporal system") && text.to_lowercase().contains("multi-unit"));
    assert!(text.contains("NOT one app") || text.contains("never one"));
    assert!(text.contains("PER-UNIT shape") || text.contains("per-unit shape"));
    assert!(text.contains("Per-unit runtime dispatch"));
    let step3 = found(&text, r"(?s)## Step 3 —.*?## Step 4").expect("Step 3 section");
    let first = step3.to_lowercase().matches("dispatch first").count();
    assert!(
        first <= 1,
        "Step 3 has {first} 'DISPATCH FIRST' headings; only the multi-unit gate may be first"
    );
}

// ---- test_temporal_decision_refs.py --------------------------------------------------------------

#[test]
fn temporal_tier1_eliminations_present() {
    let text = raw(TEMPORAL);
    for (runtime, reason) in [
        ("Lambda classic", "15-min cap"),
        ("AgentCore Runtime", "8-hour max execution"),
        ("Lambda MicroVMs", "resident-polling cost"),
    ] {
        let source = format!(
            "(?s){}.{{0,200}}{}",
            fancy_regex::escape(runtime),
            fancy_regex::escape(reason)
        );
        assert!(
            has(&text, &source),
            "Tier 1 elimination for '{runtime}' (reason '{reason}') missing or reworded"
        );
    }
}

#[test]
fn temporal_tier1_tension_rule_is_first() {
    // If the plain "K8s → EKS" direct-pick evaluates first, the tension case
    // can never fire.
    let text = raw(TEMPORAL);
    let tension = text.find("the only genuine tension");
    let eks_pick = text.find("team already operates K8s");
    let (Some(tension), Some(eks_pick)) = (tension, eks_pick) else {
        panic!("Tier 1 rules missing");
    };
    assert!(tension < eks_pick, "rules are first-match-wins");
}

#[test]
fn temporal_tier1_rules_are_first_match_wins() {
    let text = raw(TEMPORAL);
    assert!(text.contains("first match wins") || text.contains("IN ORDER"));
}

#[test]
fn temporal_tier1_user_choice_gates_are_marked() {
    // Rules 1 and 4 are offers, not direct picks.
    let text = raw(TEMPORAL);
    assert!(text.contains("OFFER a split"), "rule 4 must be an offer");
    assert!(text.contains("declining it is legitimate"));
    assert!(text.contains("NOT as falling through to rule 5"));
}

#[test]
fn temporal_serverless_workers_labeled_public_preview() {
    let text = raw(TEMPORAL);
    assert!(text.contains("Public Preview"));
    assert!(
        text.to_lowercase().contains("do not trust the docs label")
            || text.contains("Do not trust the docs label")
    );
}

#[test]
fn temporal_runbook1_preconditions_locked() {
    let text = raw(TEMPORAL);
    for fragment in [
        "SAME workflow and activity types",
        "GRACEFULLY",
        "do NOT migrate mid-execution",
        "retry policies + heartbeat timeouts",
    ] {
        assert!(
            text.contains(fragment),
            "runbook 1 precondition missing: {fragment:?}"
        );
    }
}

#[test]
fn temporal_runbook2_warns_no_history_migration_tool() {
    assert!(norm(TEMPORAL).contains("no official cross-cluster history migration tool"));
}

#[test]
fn temporal_runbook3_determinism_broader_than_signatures() {
    let text = raw(TEMPORAL);
    assert!(text.contains("broader than Activity signatures"));
    assert!(text.contains("NOT a") && text.contains("compatibility layer"));
    assert!(text.contains("Worker Versioning"));
}

#[test]
fn temporal_way_table_defaults_to_way1() {
    let text = raw(TEMPORAL);
    assert!(text.contains("Way 1 (default)"));
    // Must be surfaced before Way 2.
    assert!(text.contains("External Payload Storage"));
}

#[test]
fn temporal_commercials_split_by_current_state() {
    // Users already on Temporal Cloud must not be pitched the Marketplace
    // subscribe flow.
    let text = norm(TEMPORAL);
    assert!(text.contains("Already on Temporal Cloud"));
    assert!(text.contains("do NOT pitch the subscribe flow"));
    assert!(text.contains("not a technical"));
    assert!(text.contains("New Temporal Cloud customer"));
}

#[test]
fn temporal_sfn_comparison_is_chat_only() {
    let text = raw(TEMPORAL);
    assert!(text.contains("does NOT include a Step Functions comparison"));
    assert!(text.contains("Do not volunteer this unprompted"));
    for fragment in ["waitForTaskToken", "1-year Standard", "time-skipping"] {
        assert!(
            text.contains(fragment),
            "SFN chat-fallback fact missing: {fragment:?}"
        );
    }
    assert!(
        raw(GENERATE).contains("No Step Functions comparison section"),
        "generate.md must say the plan has no SFN section"
    );
}

#[test]
fn temporal_design_points_at_decision_ref_and_reads_the_way_seam() {
    let design = raw(DESIGN);
    assert!(design.contains("decision-refs/temporal.md"));
    assert!(design.contains("answers.json.system.temporal_way"));
    assert!(design.contains("Way table") || design.contains("Way-table"));
    assert!(design.contains("undecided"));
    assert!(design.contains("context-signals.json.temporal.server"));
}

#[test]
fn temporal_intake_seeds_no_code_temporal() {
    let intake = raw("references/phases/intake/intake.md");
    assert!(intake.to_lowercase().contains("no-code path"));
    assert!(intake.contains("SEED declared temporal units"));
    assert!(intake.contains("temporal_worker_poll"));
    assert!(intake.contains(r#"source: "declared""#));
}

#[test]
fn temporal_branch_adapter_dimensions_are_legal() {
    // The Tier 2 agent-session adapter table must only name real scoring
    // dimensions with legal values; otherwise scoring rejects or silently
    // defaults the input.
    let temporal = raw(TEMPORAL);
    // Single-dimension rows only; ` +` tolerates the alignment padding.
    let rows = Regex::new(r"(?m)^\s*\| `([a-z_]+)` +\|").expect("valid pattern");
    let mut adapter_dims = Vec::new();
    for captures in rows.captures_iter(&temporal) {
        let captures = captures.expect("the match runs");
        let dimension = captures.get(1).expect("group").as_str().to_string();
        if DIMENSIONS.contains(&dimension.as_str()) {
            adapter_dims.push(dimension);
        }
    }
    assert!(
        adapter_dims.len() >= 8,
        "adapter table looks broken: only found {adapter_dims:?}"
    );
    for dimension in &adapter_dims {
        let row = group(&temporal, &format!(r"\| `{dimension}` +\|([^\n]*)"), 1)
            .expect("the adapter row");
        let legal = legal_values(dimension).expect("legal values");
        let value = Regex::new(r"`([a-z0-9_]+)`").expect("valid pattern");
        for captures in value.captures_iter(&row) {
            let captures = captures.expect("the match runs");
            let value = captures.get(1).expect("group").as_str();
            assert!(
                legal.contains(&value),
                "adapter maps {dimension} to illegal value {value:?} (legal: {legal:?})"
            );
        }
    }
}

// ---- test_workload_classes.py ----------------------------------------------------------------------

#[test]
fn workload_classes_first_match_wins_and_rules_are_in_order() {
    let text = raw(WORKLOAD_CLASSES);
    assert!(text.to_lowercase().contains("first match wins"));
    let positions: Vec<Option<usize>> = (1..=6).map(|i| text.find(&format!("**W{i}**"))).collect();
    assert!(
        positions.iter().all(Option::is_some),
        "missing rule ids: {positions:?}"
    );
    let mut sorted = positions.clone();
    sorted.sort();
    assert_eq!(positions, sorted, "W1..W6 must appear in order");
}

#[test]
fn workload_classes_batch_never_agentcore_and_light_io_defaults() {
    // AgentCore frames work as sessions with session-scoped billing, the wrong
    // shape for batch schedulers.
    let text = raw(WORKLOAD_CLASSES);
    assert!(text.contains("batch"));
    assert!(has(&text, r"(?is)batch.{0,400}never AgentCore"));
    assert!(has(&text, r"(?s)light_io.{0,300}Lambda"));
    // W1: an existing EKS/ECS cluster pulls same-class workloads onto it.
    assert!(text.contains("existing_cluster"));
}

#[test]
fn workload_classes_verdict_tokens_explicit() {
    // Each rule states the verdict enum token explicitly alongside the prose
    // label, so design.json.verdict cannot drift from workload_class.
    let text = raw(WORKLOAD_CLASSES);
    for (rule, token, label) in [
        ("W2", "batch", "AWS Batch"),
        ("W3", "lambda", "Lambda"),
        ("W4", "lambda", "Lambda"),
        ("W5", "fargate", "Fargate"),
        ("W6", "fargate", "Fargate"),
    ] {
        assert!(
            has(&text, &format!(r"(?s){rule}.*`{token}`.*{label}")),
            "{rule} must state `{token}` and {label}"
        );
    }
}

#[test]
fn workload_classes_design_asserts_verdict_workload_class_consistency() {
    let design = raw(DESIGN);
    assert!(has(&design, r"(?is)verdict.*workload.*never contradictory"));
}

#[test]
fn workload_classes_consolidated_does_not_rewrite_per_unit_verdict() {
    // Live-test 0715: the LLM overwrote every unit.verdict with the superset,
    // losing the raw verdict the effective-runtime logic needs.
    let design = norm(DESIGN);
    assert!(
        has(&design, r"[Cc]onsolidated does NOT rewrite per-unit")
            || has(
                &design,
                r"(?i)never overwrites?.{0,40}verdict.{0,40}superset"
            ),
        "design.md must state consolidated does not rewrite per-unit verdict"
    );
    assert!(design.contains("effective_runtime"));
    assert!(
        has(&design, r"(?i)consolidated.{0,80}platform\.runtime"),
        "design.md must set per-unit effective_runtime to platform.runtime when consolidated"
    );
}

#[test]
fn workload_classes_every_verdict_has_a_service_card_with_serving_notes() {
    // design.md derives each unit's key_change from the winning runtime's
    // "## Serving & security notes" block; fargate reuses ecs.md.
    for (verdict, card) in [
        ("batch", "batch.md"),
        ("lambda", "lambda.md"),
        ("ecs", "ecs.md"),
        ("eks", "eks.md"),
        ("fargate", "ecs.md"),
    ] {
        let text = raw(&format!("references/decision-refs/{card}"));
        assert!(
            text.contains("## Serving & security notes"),
            "{card} ({verdict}) lacks the '## Serving & security notes' block key_change needs"
        );
    }
    let design = raw(DESIGN);
    assert!(
        has(
            &design,
            r"(?i)`fargate`.{0,40}`?ecs\.md`?|ecs\.md.{0,40}fargate"
        ),
        "design.md must state fargate resolves to ecs.md for card loading"
    );
}

// ---- test_scoring.py: model lifecycle drift self-proofs ----------------------------------------------
//
// The pool-versus-lifecycle checks need the model-recommendation modules and
// move with the `startup-models` slice; the matcher itself and its two
// self-proofs are held here so a changed lifecycle format cannot silently
// blind them.

/// Every line of the lifecycle file that marks a model as unsafe to select: a
/// pipe-table row tagged `legacy`/`excluded`, or a bullet of the **Removed**
/// section (past EOL, a stronger violation than "excluded").
fn legacy_or_excluded_rows() -> Vec<String> {
    let text = raw("../shared/ai/ai-model-lifecycle.md").to_lowercase();
    let mut rows: Vec<String> = text
        .lines()
        .filter(|line| {
            line.trim().starts_with('|') && (line.contains("legacy") || line.contains("excluded"))
        })
        .map(str::to_string)
        .collect();
    let mut in_removed = false;
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.starts_with("**removed") {
            in_removed = true;
            continue;
        }
        if in_removed && stripped.starts_with('>') {
            // The "AWS page lag" blockquote ends the section.
            in_removed = false;
            continue;
        }
        if in_removed && stripped.starts_with("- ") {
            rows.push(line.to_string());
        }
    }
    rows
}

#[test]
fn drift_mechanism_fires_on_a_known_legacy_model() {
    let rows = legacy_or_excluded_rows();
    assert!(
        rows.iter().any(|row| row.contains("claude-opus-4-1")),
        "expected Claude Opus 4.1 in a legacy row: the lifecycle file format changed"
    );
}

#[test]
fn drift_mechanism_fires_on_a_known_removed_model() {
    let rows = legacy_or_excluded_rows();
    assert!(
        rows.iter().any(|row| row.contains("nova-sonic-v1")),
        "expected Nova Sonic v1 in a Removed row: the Removed-section matcher needs updating"
    );
}

// ---- test_scoring.py: model-pool lifecycle drift (#4612) ---------------------------------------

/// Each internal model id of the selection pool and the substring that
/// identifies it in the lifecycle file's Legacy/EOL table (`_POOL_LIFECYCLE_KEYS`).
const POOL_LIFECYCLE_KEYS: [(&str, &str); 15] = [
    ("claude_opus_5_5", "claude-opus-5-5"),
    ("claude_sonnet_5", "claude-sonnet-5"),
    ("claude_haiku_4_5", "claude-haiku-4-5"),
    ("openai_gpt_6_astra", "gpt-6-astra"),
    ("openai_gpt_5_6_sol", "gpt-5.6-sol"),
    ("openai_gpt_5_6_terra", "gpt-5.6-terra"),
    ("openai_gpt_5_6_luna", "gpt-5.6-luna"),
    ("openai_gpt_5_5", "gpt-5.5"),
    ("openai_gpt_5_4", "gpt-5.4"),
    ("openai_gpt_6_sol", "gpt-6-sol"),
    ("openai_gpt_6_luna", "gpt-6-luna"),
    ("openai_gpt_6_1_sol", "gpt-6.1-sol"),
    ("anthropic_claude_sonnet_5", "claude-sonnet-5"),
    ("anthropic_claude_opus_5_5", "claude-opus-5-5"),
    ("anthropic_claude_haiku_4_5", "claude-haiku-4-5"),
];

/// The Model Recommend engine's selectable set: the priority ordering plus
/// every model of the dated per-provider catalogs (`_pool_models`).
fn pool_models() -> std::collections::BTreeSet<String> {
    use crate::startup::models::{
        load_anthropic_catalog, load_openai_catalog, DEFAULT_CATALOG, OPENAI_CATALOG,
        PRIORITY_ORDER,
    };

    let models = real_skill().join("references").join("models");
    let mut pool: std::collections::BTreeSet<String> = PRIORITY_ORDER
        .iter()
        .flat_map(|(_, order)| order.iter().map(ToString::to_string))
        .collect();
    for catalog in [
        load_anthropic_catalog(&models.join(DEFAULT_CATALOG)).expect("the Anthropic catalog loads"),
        load_openai_catalog(&models.join(OPENAI_CATALOG)).expect("the OpenAI catalog loads"),
    ] {
        pool.extend(catalog.models.iter().map(|(key, _)| key.clone()));
    }
    pool
}

#[test]
fn pool_keys_cover_every_selectable_model() {
    // If a new model enters the selection pool it must have a lifecycle key so
    // the drift test below actually checks it.
    let missing: Vec<String> = pool_models()
        .into_iter()
        .filter(|model| !POOL_LIFECYCLE_KEYS.iter().any(|(known, _)| known == model))
        .collect();
    assert!(
        missing.is_empty(),
        "models missing a lifecycle key: {missing:?}"
    );
}

#[test]
fn no_pool_model_is_legacy_or_excluded() {
    let bad_rows = legacy_or_excluded_rows();
    for model in pool_models() {
        let (_, key) = POOL_LIFECYCLE_KEYS
            .iter()
            .find(|(known, _)| *known == model)
            .unwrap_or_else(|| panic!("{model} has no lifecycle key"));
        let offending: Vec<&String> = bad_rows.iter().filter(|row| row.contains(key)).collect();
        assert!(
            offending.is_empty(),
            "model '{model}' (key '{key}') appears Legacy/excluded in the lifecycle file: \
update the selection pool. Row: {}",
            offending.first().map_or("", |row| row.trim())
        );
    }
}
