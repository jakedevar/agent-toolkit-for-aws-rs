//! Content locks of the unit-inventory grouping rules and the producer-to-consumer
//! seams between the agent-advisor phases (the ported `test_unit_grouping.py`).
//!
//! The grouping judgment (in-process agents merge, cross-process units split)
//! and the phase contracts are prose, so these tests pin their load-bearing
//! sentences the same way the temporal locks do. The scoring command checks of
//! the Python file (`score_units.py` run in a subprocess) are the CLI tests of
//! `tests.rs`; the `build_diagram` assertions belong to the `startup-diagram`
//! slice.

use super::{count, found, frontmatter, group, has, norm, raw};

fn phase(relative: &str) -> String {
    norm(&format!("references/phases/{relative}"))
}

fn decision(relative: &str) -> String {
    norm(&format!("references/decision-refs/{relative}"))
}

fn discover() -> String {
    phase("discover/discover.md")
}

fn clarify() -> String {
    phase("clarify/clarify.md")
}

fn confirm() -> String {
    phase("confirm/confirm.md")
}

fn design() -> String {
    phase("design/design.md")
}

fn estimate() -> String {
    phase("estimate/estimate.md")
}

fn generate_report() -> String {
    phase("generate/generate-report.md")
}

fn migration_plan() -> String {
    phase("migration-plan/migration-plan.md")
}

fn poc() -> String {
    phase("poc/poc.md")
}

fn poc_report() -> String {
    phase("poc/poc-report.md")
}

fn shapes() -> String {
    decision("poc-shapes.md")
}

// ---- Discover: grouping vocabulary -------------------------------------------------------------

#[test]
fn in_process_agents_merge() {
    let text = discover();
    assert!(text.to_lowercase().contains("in-process"));
    assert!(
        has(&text, r"(?i)in-process.{0,200}ONE unit"),
        "in-process coupled agents must be stated to merge into one unit"
    );
}

#[test]
fn coupling_mode_vocabulary() {
    let text = discover();
    for mode in ["queue", "api", "a2a", "none"] {
        assert!(text.contains(&format!("`{mode}`")), "{mode}");
    }
    assert!(
        has(&text, r"in_process.{0,160}never"),
        "in_process must be stated to never survive grouping"
    );
}

#[test]
fn workload_class_vocabulary() {
    let text = discover();
    for class in ["agent_session", "batch", "light_io", "service"] {
        assert!(text.contains(&format!("`{class}`")), "{class}");
    }
}

#[test]
fn confirmation_only_when_multiple() {
    // Collapse invariant: a single-unit draft is recorded silently.
    let text = phase("discover/discover-assemble.md");
    assert!(
        has(&text, r">\s*1 unit|ONLY when.{0,80}more than one unit"),
        "grouping confirmation must be gated on >1 unit"
    );
}

#[test]
fn unit_trigger_vocabulary() {
    let text = discover();
    assert!(text.contains("\"trigger\"") || text.contains("`trigger`"));
    for value in ["request", "event", "schedule", "temporal"] {
        assert!(
            text.contains(&format!("`{value}`")),
            "trigger vocab missing {value}"
        );
    }
}

#[test]
fn temporal_detection_creates_units() {
    let text = discover();
    assert!(
        text.contains("`temporal_worker_poll`"),
        "temporal poll tier must be a unit class"
    );
    assert!(has(
        &text,
        r"(?i)one\s+`temporal_worker_poll`\s+unit per (worker fleet|task-queue group)"
    ));
    assert!(
        has(
            &text,
            r"(?s)Activity execution class.{0,200}(agent_session|`agent_session`)"
        ),
        "activity classes must map onto unit classes"
    );
    assert!(
        text.contains("decision-refs/temporal.md"),
        "classification vocabulary must defer to temporal.md"
    );
}

#[test]
fn poll_units_list_queues() {
    let text = discover();
    assert!(text.contains("\"queues\"") || text.contains("`queues`"));
    assert!(has(&text, r"(?i)temporal_worker_poll.{0,200}queues"));
}

#[test]
fn activity_units_carry_task_queue_join_key() {
    // The diagram connected each fleet to EVERY activity (cartesian product)
    // because activity units had no queue membership.
    let text = discover();
    assert!(
        has(&text, "\"task_queue\"") && has(&text, "Activity-class unit"),
        "discover must give each Activity-class unit a task_queue field"
    );
    assert!(
        has(
            &text,
            r"(?i)join key|never a cartesian product|never a cartesian"
        ),
        "discover must frame task_queue as the fleet<->activity join key"
    );
}

// ---- Clarify: two levels and the scoring loop -------------------------------------------------

#[test]
fn clarify_two_levels() {
    let text = clarify();
    for dimension in [
        "ops_preference",
        "existing_cluster",
        "multi_cloud",
        "platform_fit",
    ] {
        assert!(
            text.contains(&format!("`{dimension}`")),
            "system-level dim {dimension} must be named"
        );
    }
    assert!(has(&text, r"(?i)system-level.{0,200}asked once"));
    assert!(has(&text, r"(?i)primary unit.{0,200}full"));
    assert!(
        has(&text, "ONE batched delta question"),
        "non-primary units get exactly one batched delta question"
    );
    assert!(has(&text, r"(?i)inherit.{0,120}primary"));
}

#[test]
fn scoring_loop_per_agent_unit() {
    let text = clarify();
    assert!(
        has(&text, r"once per `agent_session` unit"),
        "scoring must be stated to run once per agent-class unit"
    );
    assert!(text.contains("scoring.py is NOT modified") || text.contains("scoring.py unchanged"));
}

#[test]
fn scoring_command_is_the_committed_wrapper() {
    // The Clarify scoring command must invoke the committed score_units.py with
    // answers.json (it wraps results under {"units": {...}}), never inline code.
    let text = clarify();
    assert!(
        has(&text, r#"score_units\.py"\s+"\$RUN_DIR/answers\.json""#),
        "clarify must invoke the committed score_units.py with answers.json as an argument"
    );
    assert!(
        !text.contains("python -c"),
        "clarify must not inline scoring code: committed scripts only"
    );
}

#[test]
fn scoring_reads_only_answers_json() {
    // Scoring must NOT open context-signals.json (absent on skipped-Discover
    // runs). workload_class is persisted into answers.json.units[id]; entry_point
    // is read from .phase-status.json.
    let text = clarify();
    assert!(
        has(&text, r#""workload_class":\s*"<class>""#),
        "answers.json units[] must persist workload_class (Step 4 schema)"
    );
    assert!(
        has(&text, r"(?s)entry_point.{0,80}\.phase-status\.json"),
        "entry_point must be read from .phase-status.json"
    );
}

#[test]
fn clarify_scope_gate_requires_agent_unit() {
    // agent-advisor is scoped to agentic systems: Clarify halts a purely
    // non-agent system BEFORE primary selection.
    let raw_text = raw("references/phases/clarify/clarify.md");
    let text = clarify();
    assert!(has(&text, "[Ss]cope gate") && has(&text, "BEFORE primary selection"));
    assert!(
        has(&text, r"(?i)at least one .?agent_session.? unit"),
        "the gate must require >=1 agent_session unit"
    );
    assert!(has(&text, "_halt_and_inform"), "the gate must halt");
    let gate = raw_text.find("Scope gate");
    let primary = raw_text.find("**Primary unit:**");
    let (Some(gate), Some(primary)) = (gate, primary) else {
        panic!("the scope gate and the primary unit bullet must both exist");
    };
    assert!(
        0 < gate && gate < primary,
        "the scope gate must appear before the Primary unit selection"
    );
    assert!(
        !text.contains("no_agent_units"),
        "the zero-agent path must be removed"
    );
    assert!(
        !text.contains("'non_agent'") && !text.contains("\"non_agent\""),
        "the non_agent sentinel must be gone"
    );
}

#[test]
fn clarify_materializes_single_unit_when_no_inventory() {
    let text = clarify();
    assert!(has(
        &text,
        r"No inventory at all|MATERIALIZES exactly one .{0,20}unit"
    ));
    assert!(
        has(
            &text,
            r"(?s)workload_class.{0,40}defaults to .?agent_session"
        ),
        "the materialized single unit defaults to agent_session"
    );
    for field in ["coupling", "trigger", "description", "evidence"] {
        assert!(
            text.contains(field),
            "the materialized unit record must carry {field}"
        );
    }
}

#[test]
fn scope_gate_after_temporal_activity_classification() {
    // The gate must not halt a no-code Temporal workload before its Activities
    // are classified.
    let text = clarify();
    assert!(has(
        &text,
        r"[Dd]o NOT halt a Temporal system on the strength of a .?temporal_worker_poll"
    ));
    assert!(has(
        &text,
        r"(?s)Activity classification is part of building\s*the inventory.{0,80}BEFORE the scope gate"
    ));
}

#[test]
fn round26_temporal_single_workload_and_multiunit_poc_report() {
    // A single-workload Temporal signal still records a draft; the one-unit
    // collapse does not skip Activity classification; the POC report renders
    // per-unit runtime and model.
    let intake = phase("intake/intake.md");
    assert!(has(
        &intake,
        "Temporal signal ALWAYS records a draft, even for a single workload"
    ));
    assert!(has(
        &clarify(),
        "Temporal Activity interview/classification below and the scope gate are NOT skippable"
    ));
    let report = poc_report();
    assert!(has(
        &report,
        r"(?s)one node per\s*unit in .?UNITS.?, each on ITS OWN .?effective_runtime"
    ));
    assert!(has(&report, "resolved Bedrock model PER UNIT"));
}

// ---- Cross-phase field seams ---------------------------------------------------------------------

#[test]
fn cross_phase_field_seams() {
    // Whole-branch review found field-name forks at phase seams: pin the reader
    // side to the producer's vocabulary.
    let report = generate_report();
    let plan = migration_plan();
    let clarify = clarify();
    assert!(!report.contains("PLATFORM.decision") && !report.contains("PLATFORM.rationale"));
    assert!(!plan.contains("platform.architecture"));
    assert!(report.contains("consolidated") && !report.contains("consolidate |"));
    assert!(
        !design().contains("consolidate |"),
        "stale consolidate token in design.md"
    );
    // clarify writes the legacy mirror.
    assert!(
        has(
            &clarify,
            r"(?i)entry_point.{0,300}answers.{0,200}primary unit"
        ) || has(&clarify, r"(?i)legacy.{0,120}mirror")
    );
    // plan injection uses real unit field names.
    assert!(plan.contains("model_recommendation") && plan.contains("agentcore_services"));
}

#[test]
fn intake_routes_temporal_into_main_flow() {
    let intake = phase("intake/intake.md");
    assert!(
        !intake.contains("entry_point = temporal_worker"),
        "the temporal branch entry point is retired"
    );
    assert!(has(&intake, "[Tt]emporal signal"));
}

#[test]
fn design_writes_effective_runtime_and_coupling_per_unit() {
    // design.json carries per-unit effective_runtime (single source for every
    // downstream deploy/cost/render target) and per-unit coupling.
    let design = design();
    assert!(design.contains("effective_runtime"));
    assert!(
        has(&design, r"(?i)consolidated.{0,80}platform\.runtime")
            && has(&design, r"(?i)split.{0,160}(verdict|resolved runtime)")
    );
    assert!(has(&design, "\"coupling\"") || has(&design, r"coupling.{0,40}context-signals"));
}

#[test]
fn platform_runtime_enum_allows_lambda_consolidation() {
    // The consolidation superset is "the runtime satisfying every unit's hard
    // constraints with the highest summed score": any qualifying runtime is
    // legal, including Lambda / Lambda MicroVMs.
    let enumeration = group(
        &design(),
        r#""runtime":\s*"([^"]*consolidation superset[^"]*)""#,
        1,
    )
    .expect("design.md must document the platform.runtime superset enum");
    assert!(enumeration.contains("lambda"));
}

#[test]
fn downstream_phases_read_effective_runtime() {
    assert!(
        estimate().contains("effective_runtime"),
        "estimate must cost by effective_runtime"
    );
    assert!(
        generate_report().contains("effective_runtime"),
        "report must render the effective_runtime as the target"
    );
}

#[test]
fn estimate_eks_not_hardcoded_fargate() {
    let est = estimate();
    assert!(has(&est, r"(?i)do NOT assume Fargate|not assume Fargate"));
    assert!(has(&est, r"(?i)node capacity type") && has(&est, r"EC2|Karpenter|Spot"));
    // Reusing an existing cluster is near-zero ONLY with spare capacity.
    assert!(
        has(
            &est,
            r"(?is)near-zero.{0,60}(only|ONLY).{0,40}spare capacity"
        ) || has(&est, r"(?is)spare capacity.{0,80}near-zero")
    );
    assert!(has(&est, r"(?i)Karpenter/ASG adds nodes|full incremental"));
    // The node-aware EKS rule applies to ANY unit whose effective_runtime is eks.
    assert!(has(
        &est,
        r"(?i)ANY unit whose .?effective_runtime == .?eks"
    ));
    assert!(has(&est, r"(?i)regardless of\s+class"));
    // The temporal_worker_poll polling tier must not be hardcoded "tens of $/mo".
    let tw = found(&est, r"(?s)temporal_worker_poll units.{0,2600}")
        .expect("estimate must have a temporal_worker_poll cost bullet");
    assert!(has(
        &tw,
        r"(?is)effective_runtime == .?eks.{0,200}(EKS pricing rule|node capacity)"
    ));
    assert!(has(
        &tw,
        r"(?is)serverless_workers.{0,300}(qualitative|Public Preview|unverified)"
    ));
    // Do NOT assert execution-tier dominance unconditionally.
    assert!(
        !has(&tw, r"(?i)execution tier\s+dominates(?![^.]*compar)"),
        "must not state 'execution tier dominates' unconditionally"
    );
    assert!(has(
        &tw,
        r"(?i)compare the .{0,20}bands|can equal or exceed|material or even dominant"
    ));
    // Temporal Cloud orchestration is a system-level cost, never a unit line.
    assert!(has(&est, r"(?i)do NOT fold them into any unit"));
    assert!(!has(
        &est,
        r"(?i)separate line in the per-unit .{0,20}breakdown"
    ));
    assert!(has(
        &est,
        r"(?is)total_monthly_magnitude_usd.{0,120}(system-level|orchestration|PLUS)"
    ));
}

#[test]
fn agentcore_endpoint_note_is_per_unit_not_primary() {
    // The AgentCore /invocations+/ping note attaches to EVERY unit whose
    // effective_runtime is agentcore, not just the primary/winning unit.
    let plan = migration_plan();
    assert!(
        has(&plan, "PER UNIT")
            && has(
                &plan,
                r"(?i)EVERY unit whose .?effective_runtime == .?agentcore"
            )
    );
    assert!(has(&plan, r"(?i)NOT just the primary"));
    // The injection JSON schema must ALSO carry a per-unit endpoint field.
    assert!(has(&plan, "\"endpoint_contract\""));
}

#[test]
fn poc_goal_line_enumerates_lambda_microvms() {
    let goal = found(&poc(), r"(?s)\*\*Goal\*\*.{0,600}").expect("poc Goal line");
    assert!(
        goal.contains("Lambda MicroVMs"),
        "poc Goal-line enumeration must include a Lambda MicroVMs phrase"
    );
}

#[test]
fn migration_plan_consolidated_overrides_top_level_approach() {
    let plan = migration_plan();
    assert!(
        has(&plan, "[Cc]onsolidated platform overrides")
            || has(
                &plan,
                r#"(?s)platform\.mode\s*==\s*"?consolidated"?.{0,200}platform\.runtime"#
            )
    );
    assert!(
        plan.contains("lambda_microvms")
            && has(&plan, r"(?i)harness/strands ONLY when|only inject .?.?harness"),
        "the retarget override must cover lambda_microvms; harness only when the superset is agentcore"
    );
}

#[test]
fn handoff_required_scans_all_units_effective_runtime() {
    // handoff_required is true when ANY unit's effective_runtime needs a compute
    // handoff: ecs, eks, fargate or batch.
    let design = design();
    assert!(has(
        &design,
        r"(?i)ANY unit'?s? .?effective_runtime.? needs a"
    ));
    assert!(has(&design, r"(?i)not just the primary"));
    assert!(has(&design, r"\{ecs, eks, fargate, batch\}"));
}

#[test]
fn agentcore_consolidation_keeps_endpoint_note() {
    let plan = migration_plan();
    assert!(has(
        &plan,
        r"(?i)AgentCore .*note follows .*platform\.runtime"
    ));
    assert!(has(&plan, r"ONLY when .?platform\.runtime == .?agentcore"));
}

#[test]
fn contributing_does_not_mislabel_llm_to_bedrock_as_dsl() {
    // CONTRIBUTING.md lives at the plugin root; the Python test returned early
    // when it was absent, as it is in this tree.
    let path = super::real_skill().join("../../CONTRIBUTING.md");
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let text = super::squash(&text);
    let row = group(&text, r"\*\*llm-to-bedrock\*\*\s*\|([^|]*)\|", 1)
        .expect("CONTRIBUTING must have an llm-to-bedrock architecture row");
    assert!(!row.contains("phase DSL"));
    assert!(has(&text, r"[Tt]wo are built on the .{0,20}phase DSL") || has(&text, "prose SKILL"));
}

#[test]
fn co_recommend_resolved_to_runtime_before_gate_and_effective_runtime() {
    // co_recommend is NOT a runtime: the platform gate compares the RESOLVED
    // runtime, and design's effective_runtime is never the literal co_recommend.
    let confirm = confirm();
    let design = design();
    assert!(
        has(&confirm, r"(?i)resolved runtime")
            && has(&confirm, r"[Nn]ever compare the literal|not a runtime")
    );
    assert!(has(&confirm, "chosen_runtime"));
    assert!(has(
        &design,
        r"[Nn]ever the literal .?co_recommend|ALWAYS a concrete runtime"
    ));
}

#[test]
fn migration_plan_deployment_model_consistent_with_target_runtime() {
    let plan = migration_plan();
    assert!(has(
        &plan,
        r"(?i)deployment_model.{0,80}consistent with.{0,20}target_runtime"
    ));
    assert!(
        has(
            &plan,
            r#"(?i)only when.{0,40}target_runtime\s*==\s*"?agentcore"#
        ) && has(&plan, "framework_on_runtime")
    );
}

#[test]
fn confirm_step3_covers_all_non_agentcore_runtimes() {
    let confirm = confirm();
    assert!(
        has(&confirm, r"Step 3.{0,120}lambda_microvms")
            || has(
                &confirm,
                r"(?i)non-AgentCore runtime.{0,120}lambda_microvms"
            )
    );
    assert!(has(
        &confirm,
        r"[Ee]very non-agentcore runtime|no runtime pick skips"
    ));
}

#[test]
fn confirm_resolves_each_agent_unit_independently() {
    let confirm = confirm();
    assert!(has(
        &confirm,
        r"(?i)once per .?agent_session.? unit|per-unit confirm"
    ));
    assert!(confirm.contains("units[<unit_id>]") || has(&confirm, r"confirm\.json\.units\["));
    assert!(has(
        &confirm,
        r"[Nn]ever (apply|copy).{0,90}(another|primary)"
    ));
}

#[test]
fn migration_plan_injects_effective_runtime() {
    let plan = migration_plan();
    assert!(plan.to_lowercase().contains("effective runtime"));
    assert!(
        has(&plan, r#"platform\.mode\s*==\s*"?consolidated"?"#)
            && plan.contains("platform.runtime")
    );
    assert!(plan.contains("raw_verdict"));
}

#[test]
fn migration_plan_unit_correlation_overlay() {
    let plan = migration_plan();
    assert!(plan.contains("\"evidence\""));
    assert!(plan.contains("advisor_unit") && plan.contains("advisor_target_runtime"));
    assert!(
        has(&plan, r"(?is)additive.{0,200}never (modify|remove)"),
        "annotation must be declared additive-only"
    );
    assert!(
        has(&plan, r"[Ss]ingle-unit.{0,120}(skip|SKIP)"),
        "single-unit runs skip the overlay"
    );
}

#[test]
fn migration_plan_validates_advisor_model_contract_without_reselection() {
    let plan = migration_plan();
    assert!(has(&plan, "Validate the advisor model/path contract"));
    assert!(plan.to_lowercase().contains("advisor wins"));
    assert!(plan.contains("plan_model_mismatch") && plan.contains("_halt_and_inform"));
    assert!(has(&plan, r"(?i)do not rewrite `design\.json`"));
    assert!(
        plan.contains("model-recommendation-input.json")
            && plan.contains("model_recommendation.py")
    );
    assert!(has(&frontmatter(&plan), r"(?i)model contract validation"));
}

#[test]
fn migration_plan_gcp_report_firewall() {
    // The inline gcp engine must render migration-report.html in gcp's OWN
    // format: the advisor v3 shell must not bleed into it.
    let plan = migration_plan();
    assert!(has(&plan, "[Cc]ontext firewall"));
    assert!(
        plan.contains("report-shell.md") && has(&plan, r"(?s)[Dd]o NOT apply.{0,120}report-shell")
    );
    assert!(has(&plan, "ADDITIVE-ONLY|additive-only"));
}

// ---- Reports and estimate ----------------------------------------------------------------------------

#[test]
fn report_v3_data_seams() {
    let report = generate_report();
    for token in [
        "trigger",
        "provenance",
        "platform_decision.offer",
        "breakdown",
        "drivers",
        "key_change",
        "volatile_facts",
    ] {
        assert!(report.contains(token), "v3 report must consume {token}");
    }
    assert!(!report.contains("hero-panel"), "v3 dropped the hero panel");
}

#[test]
fn migrate_runs_estimate() {
    assert!(!norm("SKILL.md").contains("skip Estimate"));
    let est = estimate();
    assert!(has(&est, r"(?i)target-state run cost"));
    assert!(has(
        &est,
        r"(?is)TCO.{0,120}(Migration Plan|migration plugins|gcp)"
    ));
}

#[test]
fn help_banner_suppressed_pre_launch() {
    // The support page is not launched: the help banner is SUPPRESSED at its
    // single source and every consumer gates on that status.
    let banner = norm("references/report-help-banner.md");
    assert!(has(&banner, r"banner_status:\s*SUPPRESSED"));
    for relative in [
        "references/phases/generate/generate-report.md",
        "references/phases/poc/poc-report.md",
        "references/phases/migration-plan/migration-plan.md",
    ] {
        let text = norm(relative);
        assert!(
            text.contains("SUPPRESSED") && text.contains("banner_status"),
            "{relative} must gate the help banner on banner_status"
        );
    }
    let generate = generate_report();
    assert!(has(
        &generate,
        r"(?i)ONLY when.{0,40}banner_status.{0,20}LIVE|render NO help CTA"
    ));
    assert!(has(
        &banner,
        r"(?i)applies ONLY while .?banner_status.? is .?LIVE"
    ));
}

#[test]
fn serverless_workers_unverified_breakdown_schema() {
    // SW polling may be qualitatively unverifiable: an unverifiable component
    // uses "unverified" and the unit total reflects it rather than a number.
    let est = estimate();
    assert!(has(&est, "\"unverified\""));
    assert!(has(
        &est,
        r"(?is)EXCEPT when.{0,80}unverifiable|unverified.{0,120}monthly_magnitude_usd"
    ));
}

#[test]
fn report_cost_dominance_not_hardcoded_and_temporal_vars_defined() {
    let report = generate_report();
    assert!(!report.contains("model tokens dominate every line"));
    assert!(has(&report, "COST_DOMINANT_NOTE"));
    for variable in [
        "COST_DOMINANT_NOTE",
        "TEMPORAL_COST",
        "TEMPORAL_COST_SUMMARY",
    ] {
        assert!(
            count(&report, &format!("`?{variable}`?")) >= 2,
            "{variable} must be BOTH defined in the data-collection table AND used"
        );
    }
}

#[test]
fn report_reads_fields_producers_actually_write() {
    let report = generate_report();
    let est = estimate();
    assert!(!report.contains("ESTIMATE.total or") && !report.contains("ESTIMATE.total }}"));
    for field in ["total_compute", "total_model", "total_other"] {
        assert!(est.contains(field), "estimate.md must emit {field}");
    }
    assert!(
        !report.contains("driver.dimension"),
        "report must read driver.driver"
    );
    assert!(
        !report.contains("model_display"),
        "report must read model_recommendation.model"
    );
    assert!(!report.contains("compute_note") && !report.contains("model_note"));
    assert!(!report.contains("unit.scores["));
    assert!(!report.contains("PLATFORM_DECISION.rationale"));
    assert!(has(
        &poc_report(),
        r"(?i)GATED on .{0,40}banner_status|render NO help strip"
    ));
}

#[test]
fn report_contract_round13_fixes() {
    let report = generate_report();
    let est = estimate();
    assert!(has(
        &report,
        r"(?s)TEMPORAL_UNITS_PRESENT.{0,160}workload_class == .?temporal_worker_poll"
    ));
    assert!(has(&est, "uniform across all three column totals AND"));
    for ghost in ["verdict_detail", "rule_text", "considered_and_rejected"] {
        assert!(
            !report.contains(ghost),
            "rule-based form must not read {ghost}"
        );
    }
    assert!(!report.contains(".eliminated.length") && !report.contains("elim.runtime"));
    assert!(has(
        &report,
        r"FOR EACH \(runtime, reason\) IN SCORING_RESULT\.units\[unit\.id\]\.eliminated"
    ));
}

#[test]
fn report_all_template_vars_have_a_producer() {
    let report = generate_report();
    assert!(!report.contains("unit.rule_cite"));
    for variable in [
        "PRIMARY_UNIT",
        "unit.runner_up_runtime",
        "unit.runner_up_score",
        "SYSTEM_NAME",
    ] {
        assert!(
            report.contains(variable),
            "{variable} must be defined in the R0 table"
        );
    }
    assert!(has(&report, "IF unit.runner_up_runtime"));
}

#[test]
fn report_contract_round14_fixes() {
    let report = generate_report();
    assert!(!report.contains("RUNNER_UP_RUNTIME") && !report.contains("RUNNER_UP_SCORE"));
    assert!(has(
        &report,
        r"(?is)unit\.runner_up_runtime.{0,220}per-unit loop|NOT a global scalar"
    ));
    assert!(!report.contains("(runtime, score, pct) IN"));
    assert!(has(&report, r"pct\s*=\s*ROUND\(100"));
    assert!(!report.contains("answers.json.provenance"));
    assert!(has(&report, r"system\.provenance") && has(&report, r"units\[unit_id\]\.provenance"));
}

#[test]
fn report_assessment_inputs_enumerates_layered_answers() {
    let report = generate_report();
    assert!(!report.contains("(dim, value, scope, unit_id) IN ANSWERS + UNITS"));
    assert!(
        has(
            &report,
            r"FOR EACH \(dim, value\) IN ANSWER_LAYERS\.system EXCEPT"
        ) && has(
            &report,
            r"FOR EACH \(dim, value\) IN ANSWER_LAYERS\.units\[unit_id\] EXCEPT"
        )
    );
}

#[test]
fn runner_up_excludes_the_winner() {
    // The runner-up must EXCLUDE unit.verdict, not just be "2nd sorted".
    assert!(has(
        &generate_report(),
        r"(?s)runner_up_runtime.{0,200}EXCLUDING .?unit\.verdict"
    ));
}

#[test]
fn assessment_inputs_excludes_workload_class_metadata() {
    assert!(has(
        &generate_report(),
        r#"EXCEPT "provenance", "workload_class""#
    ));
}

#[test]
fn report_answer_layers_variable_defined() {
    let report = generate_report();
    assert!(count(&report, "`?ANSWER_LAYERS`?") >= 2);
    assert!(has(
        &report,
        r"(?s)ANSWER_LAYERS.{0,120}\.system.{0,40}\.units"
    ));
}

// ---- POC shapes and dispatch ---------------------------------------------------------------------------

#[test]
fn temporal_poc_dispatches_on_effective_runtime() {
    let shapes = shapes();
    let poc = poc();
    assert!(has(
        &shapes,
        r"[Bb]ase shape follows.{0,40}effective_runtime"
    ));
    assert!(has(
        &shapes,
        r"(?s)effective_runtime == .?eks.{0,120}(eks|kubectl|Kubernetes)"
    ));
    assert!(has(
        &poc,
        r"(?is)effective.?runtime.{0,80}eks.{0,60}kubectl|eks.{0,40}NO Terraform"
    ));
}

#[test]
fn eks_temporal_cert_paths_mounted_as_volume() {
    // TEMPORAL_TLS_*_PATH are file paths: the cert Secret keys are mounted as a
    // volume, not injected as secretKeyRef env values.
    assert!(has(
        &shapes(),
        r"(?s)TEMPORAL_TLS_CA_PATH.{0,200}(volume|volumeMount|mounted)"
    ));
}

#[test]
fn eks_temporal_poc_no_http_smoke_no_terraform_teardown() {
    let shapes = shapes();
    assert!(has(&shapes, r"(?i)NO `?service\.yaml`?|exposes NO HTTP"));
    assert!(has(&shapes, "kubectl logs"));
    assert!(
        has(&shapes, r"(?i)on \*\*EKS\*\*,?\s*`?kubectl delete")
            || has(
                &shapes,
                r"(?i)never `?terraform\s+destroy`? on the EKS base"
            )
    );
}

#[test]
fn eks_temporal_artifacts_complete_and_not_ecs_runtask() {
    let shapes = shapes();
    assert!(has(&shapes, r"smoke_worker\.py") && has(&shapes, r"smoke-job\.yaml"));
    assert!(has(
        &shapes,
        r"(?s)ECS.{0,30}run-task.{0,60}EKS.{0,30}Job|EKS base a one-shot .?Job"
    ));
    assert!(has(
        &shapes,
        r"(?i)do NOT commit connection material into\s*a generated .?secret\.yaml"
    ));
}

#[test]
fn poc_shapes_model_less_variants_per_shape() {
    let shapes = shapes();
    assert!(shapes.matches("Model-less variant").count() >= 3);
    assert!(has(
        &shapes,
        r"Bedrock wiring is CONDITIONAL on the unit'?s `?model_recommendation != null"
    ));
    assert!(has(
        &poc(),
        r"(?s)Bedrock model access for the\s*resolved model id ONLY when the unit is model-bearing"
    ));
}

#[test]
fn round26_model_less_prose_conditional() {
    assert!(has(
        &shapes(),
        r"(?s)do not claim Bedrock calls for a\s*model-less unit"
    ));
    assert!(has(
        &poc(),
        r"Never claim .?Bedrock connectivity.? for a unit whose .?model_recommendation.? is null"
    ));
    assert!(has(
        &decision("batch.md"),
        r"(?s)`?bedrock:InvokeModel`? ONLY when the\s*job actually calls a model"
    ));
}

#[test]
fn round27_service_cards_model_less_conditional() {
    for card in ["ecs.md", "eks.md", "lambda.md", "lambda-microvms.md"] {
        assert!(
            has(
                &decision(card),
                r"bedrock:InvokeModel`? \(model-bearing units only"
            ),
            "{card} IAM must condition InvokeModel on model-bearing units"
        );
    }
    let batch = decision("batch.md");
    assert!(has(&batch, "model-bearing jobs only"));
    assert!(has(
        &batch,
        r"(?s)model-less.{0,80}omits `?bedrock:InvokeModel"
    ));
    assert!(has(
        &design(),
        r"(?s)[Mm]odel-less consumption rule.{0,200}model_recommendation.? is null"
    ));
}

#[test]
fn mixed_system_model_less_and_temporal_downstream() {
    // Model-less non-agent units still occur in MIXED systems, so the consumer
    // handling for model-less units and the temporal fixes survive the scope-out.
    let generate = phase("generate/generate.md");
    let poc = poc();
    let report = poc_report();
    let est = estimate();
    assert!(has(&poc, r"whose `?model_recommendation`? is non-null"));
    assert!(has(
        &poc,
        r"(?s)[Mm]odel-less units.{0,200}omits all Bedrock wiring"
    ));
    assert!(has(
        &generate,
        r"(?s)workload_class == .?temporal_worker_poll.{0,80}temporal.{0,20}block has no .?units"
    ));
    assert!(has(
        &generate,
        r"(?s)serverless_workers.{0,60}NO .{0,20}card"
    ));
    assert!(has(
        &report,
        r"(?s)each on ITS OWN `?effective_runtime`?, NOT hardcoded to AgentCore"
    ));
    assert!(has(
        &est,
        r"(?i)DOUBLE-COUNT|do NOT also add execution cost into the worker unit"
    ));
    assert!(!generate.contains("ZERO-AGENT path") && !generate.contains("no_agent_units"));
}

// ---- Confirm and Design: resolved runtimes ----------------------------------------------------------------

#[test]
fn confirm_persists_resolved_runtimes_including_temporal() {
    let confirm = confirm();
    let design = design();
    assert!(confirm.contains("resolved_runtimes"));
    assert!(has(
        &confirm,
        r"(?is)Tier-1 rule.{0,80}(user choice|AskUserQuestion).{0,120}resolved_runtimes"
    ));
    assert!(has(
        &design,
        r"(?is)resolved_runtimes.{0,120}verbatim|Consume Confirm.{0,80}do NOT re-evaluate"
    ));
}

#[test]
fn poc_gate_keys_on_effective_runtime_not_agent_pool() {
    for (document, name) in [
        (phase("generate/generate.md"), "generate"),
        (poc(), "poc"),
        (migration_plan(), "migration-plan"),
    ] {
        assert!(
            document.contains("effective_runtime"),
            "{name} POC gate must key on effective_runtime"
        );
        assert!(
            has(
                &document,
                r"batch.{0,20}fargate.{0,20}serverless_workers|serverless_workers"
            ),
            "{name} POC gate must include batch/fargate/serverless_workers"
        );
    }
}

#[test]
fn confirm_resolves_temporal_via_temporal_md() {
    assert!(has(
        &confirm(),
        r"(?s)temporal_worker_poll.{0,120}temporal\.md"
    ));
}

#[test]
fn confirm_rescore_reuses_wrapper_command() {
    // No-viable-runtime rescoring must reuse clarify's wrapper-producing
    // command, not invoke scoring.py directly (a flat result clobbers units{}).
    let confirm = confirm();
    assert!(has(&confirm, r"(?i)Do NOT invoke .?scoring\.py.? directly"));
    assert!(has(&confirm, r"clarify\.md Step 5"));
    assert!(!confirm.contains("scripts/scoring.py $RUN_DIR/answers.json"));
}

#[test]
fn design_effective_runtime_enum_includes_serverless_workers() {
    assert!(has(
        &design(),
        r"(?s)effective_runtime.{0,200}serverless_workers"
    ));
}

#[test]
fn serverless_workers_has_no_card_derivation() {
    let design = design();
    assert!(has(
        &design,
        r"(?is)serverless_workers.{0,60}NO runtime card|NO runtime card.{0,60}serverless_workers"
    ));
    assert!(has(
        &design,
        r"[Dd]o NOT try to load .?serverless_workers\.md|skip this load for it"
    ));
    assert!(has(&design, r"(?s)serverless_workers.{0,200}temporal\.md"));
}

#[test]
fn design_consumes_resolved_runtimes_for_all_units() {
    let design = design();
    assert!(has(
        &design,
        r"(?is)non-agent unit'?s? runtime comes from .?confirm\.json\.resolved_runtimes.{0,40}VERBATIM"
    ));
    assert!(has(
        &design,
        r"[Ff]allback (only|when).{0,80}resolved_runtimes is absent|[Ff]allback only:"
    ));
}

#[test]
fn design_top_level_chosen_runtime_defined() {
    assert!(has(
        &design(),
        r"(?s)top-level `?chosen_runtime`?.{0,120}resolved_runtimes\[primary_unit\]"
    ));
}
