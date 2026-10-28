//! `test_openrouter_byok_detection_fixes.py`: PR #436 review-round fixes for
//! OpenRouter BYOK detection.
//!
//! The Python file mixed prose contracts with tests of small functions that
//! the test itself defines (the classification, rollup, window and routing
//! rules the prose describes). Those are ported as local Rust functions with
//! the same fixtures; like the Python they document the intended rule and do
//! not read the plugin.

use crate::common::{check, ensure, mirrored, name, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const OPENROUTER: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/discover/discover-openrouter-api.md",
    "skills/azure-to-aws/references/phases/discover/discover-openrouter-api.md",
];
const GCP_DISCOVER: &str = "skills/gcp-to-aws/references/phases/discover/discover.md";
const AZURE_DISCOVER: &str = "skills/azure-to-aws/references/phases/discover/discover.md";
const SKILL: &str = "skills/llm-to-bedrock/SKILL.md";

const PROVENANCE: &str = "**`cost_provenance` — now derived from `/analytics/query`";
const WRITE_PROFILE: &str = "Write `$MIGRATION_DIR/openrouter-usage-profile.json`";
const STEP_1_5: &str = "## Step 1.5";

fn each(make: impl Fn(Rule) -> Rule) -> Vec<Rule> {
    OPENROUTER.iter().map(|f| make(Rule::new(f))).collect()
}

// ---- Finding 1: join key ---------------------------------------------------------

#[test]
fn test_cost_provenance_join_key_is_model_permaslug_not_model() {
    let scope = Scope::Between(PROVENANCE, WRITE_PROFILE);
    let mut rules = each(|r| {
        r.scope(scope)
            .all(&[
                "join key is `/activity`'s `model_permaslug` field",
                "model_permaslug",
                "usage_by_model[].model` stays in whatever short form it already uses",
                "never leaks into the output",
            ])
            .any(&["not its `model`\n  field", "not its `model` field"])
    });
    rules.extend(each(|r| r.all(&[PROVENANCE])));
    check_rules(&rules);
}

#[test]
fn test_cost_provenance_handles_multiple_permaslugs_per_short_model() {
    let scope = Scope::Between(PROVENANCE, WRITE_PROFILE);
    let mut rules = each(|r| r.scope(scope).all(&["multiple `model_permaslug` values"]));
    rules.extend(each(|r| {
        r.scope(scope)
            .norm()
            .ci()
            .all(&["do not just take the last-seen permaslug"])
    }));
    check_rules(&rules);
}

#[test]
fn test_openrouter_discover_copies_remain_mirrored_for_join_fix() {
    check_mirrored(PROVENANCE, 3200);
}

/// Mirror contract: the two copies agree on `n` characters from `marker`;
/// a drifted copy and a copy without the marker both fail.
fn check_mirrored(marker: &str, n: usize) {
    let real = Plugin::real();
    mirrored(&real, OPENROUTER[0], OPENROUTER[1], marker, n)
        .expect("the committed copies are mirrored");
    let drift = real.with_edit(OPENROUTER[0], |t| {
        t.replacen(marker, &format!("{marker} (drift)"), 1)
    });
    let reason = mirrored(&drift, OPENROUTER[0], OPENROUTER[1], marker, n)
        .expect_err("a drifted copy must fail");
    assert!(reason.contains("drifted"), "{reason}");
    let gone = real.with_edit(OPENROUTER[1], |t| t.replace(marker, "x"));
    let reason = mirrored(&gone, OPENROUTER[0], OPENROUTER[1], marker, n)
        .expect_err("a missing marker must fail");
    assert!(reason.contains("missing"), "{reason}");
}

/// The classification rule of Step 3, as the Python fixture models it.
fn classify(byok_usage: f64, openrouter_usage: f64) -> &'static str {
    if byok_usage > 0.005 && openrouter_usage <= 0.005 {
        "byok_passthrough"
    } else if byok_usage > 0.005 && openrouter_usage > 0.005 {
        "mixed"
    } else if byok_usage <= 0.005 && openrouter_usage > 0.005 {
        "no_byok"
    } else {
        "unknown"
    }
}

#[test]
fn test_cost_provenance_join_fixture_openai_gpt41_classifies_mixed() {
    // /activity's `model` and `model_permaslug` differ; Analytics keys on the permaslug.
    let (model, permaslug, usage) = ("openai/gpt-4.1", "openai/gpt-4.1-2025-04-14", 150.0);
    let analytics = [("openai/gpt-4.1-2025-04-14", 10.0, 140.0)];
    let lookup = |key: &str| analytics.iter().find(|a| a.0 == key);
    let old = lookup(model).map_or("unknown", |a| classify(a.1, a.2));
    assert_eq!(
        old, "unknown",
        "the old model-keyed join must miss this fixture"
    );
    let new = lookup(permaslug).expect("the fixed join finds the permaslug row");
    assert_eq!(classify(new.1, new.2), "mixed");
    assert!((usage - 150.0_f64).abs() < f64::EPSILON);
}

// ---- Finding 2: scope boundary ---------------------------------------------------

#[test]
fn test_scope_boundary_exempts_post_analytics_query() {
    check_rules(&each(|r| {
        r.scope(Scope::From("## Scope Boundary"))
            .all(&[
                "any HTTP method other than `GET`",
                "EXCEPT the one read-only `POST /analytics/query`",
            ])
            .none(&["non-GET method"])
    }));
}

fn no_blanket_get_claim(p: &Plugin) -> Verdict {
    for f in OPENROUTER {
        let text = p.read(f)?;
        for (n, line) in text.lines().enumerate() {
            let lowered = line.to_lowercase();
            if lowered.contains("non-get")
                || (lowered.contains("get only") && !lowered.contains("except"))
            {
                ensure!(
                    line.contains("/analytics/query") || lowered.contains("analytics"),
                    "{}: line {} makes a blanket GET-only/non-GET claim without qualifying the /analytics/query POST exception: {line:?}",
                    name(f),
                    n + 1
                );
            }
        }
    }
    Ok(())
}

#[test]
fn test_no_blanket_get_only_claim_without_analytics_exemption_anywhere() {
    check(
        no_blanket_get_claim,
        &[(
            edit_replace!(
                OPENROUTER[0],
                "## Scope Boundary",
                "A blanket non-GET ban applies.\n\n## Scope Boundary"
            ),
            "blanket",
        )],
    );
}

// ---- Finding 3: cross-file orchestration -----------------------------------------

const GCP_1F: Scope = Scope::Span("**1f. OpenRouter usage discovery", "**1g.");
const AZURE_OR: Scope = Scope::Span(
    "### OpenRouter usage-API consent + capture",
    "### Anthropic usage-API consent + capture",
);

#[test]
fn test_gcp_discover_checks_analytics_entry_before_routing_activity_ok_to_step3() {
    check_rules(&[Rule::new(GCP_DISCOVER).scope(GCP_1F).all(&[
        "/activity: ok` does NOT mean route straight to Step 3",
        "/analytics/query` entry",
        "\"failed\"",
        "Step 2 retry path for the Analytics row only",
    ])]);
}

#[test]
fn test_azure_discover_reentry_checks_manifest_not_just_profile_existence() {
    check_rules(&[
        Rule::new(AZURE_DISCOVER).scope(AZURE_OR).all(&[
            "openrouter-capture/manifest.json` FIRST",
            "No manifest",
            "`/analytics/query` entry `\"failed\"`",
            "do NOT re-offer consent",
        ]),
        Rule::new(AZURE_DISCOVER).scope(AZURE_OR).norm().all(&[
            "resume `discover-openrouter-api.md` from its Step 2 Analytics-only retry path",
        ]),
    ]);
}

#[test]
fn test_azure_discover_reentry_still_reoffers_consent_for_fully_completed_run() {
    check_rules(&[Rule::new(AZURE_DISCOVER).scope(AZURE_OR).any(&[
        "a prior attempt fully\n     completed",
        "a prior attempt fully completed",
    ])]);
}

const ROW4: &str = "Row 4 `/analytics/query` sends this exact JSON body";

#[test]
fn test_both_openrouter_discover_copies_reuse_original_capture_window_on_retry() {
    let row4 = Scope::Span(ROW4, "```json");
    let mut rules = each(|r| r.scope(row4).all(&["On the FIRST attempt (no retry"]));
    rules.extend(each(|r| {
        r.scope(row4).norm().all(&[
            "On a retry of this row alone",
            "manifest's recorded `window_end_utc`",
        ])
    }));
    rules.extend(each(|r| {
        r.scope(Scope::Window(
            "`/analytics/query` entry's `status` is `failed` → retry ONLY that row",
            300,
        ))
        .all(&["reusing the manifest's recorded `window_start_utc`/`window_end_utc`"])
    }));
    check_rules(&rules);
}

#[test]
fn test_openrouter_discover_copies_remain_mirrored_for_retry_window_fix() {
    check_mirrored(ROW4, 2200);
}

// ---- Finding 4: undercount disclosure --------------------------------------------

const BYOK: &str = "row's `cost_provenance` is `\"byok_passthrough\"` →";
const MIXED: &str = "row's `cost_provenance` is `\"mixed\"` →";
const NO_BYOK: &str = "row's `cost_provenance` is `\"no_byok\"` →";
const UNKNOWN: &str = "row's `cost_provenance` is `\"unknown\"` →";
const UNCERTAIN: &str = "row's `cost_provenance` is `\"uncertain\"` →";
const KEEP: &str = "Keep every OpenRouter-only model";

fn step15() -> Scope {
    Scope::Span(STEP_1_5, "## Phase A")
}

fn bullet(a: &'static str, b: &'static str) -> Scope {
    Scope::Within(STEP_1_5, a, b)
}

#[test]
fn test_skill_md_mixed_exclusion_carries_undercount_disclosure() {
    let mixed = bullet(MIXED, NO_BYOK);
    check_rules(&[
        Rule::new(SKILL)
            .scope(step15())
            .before(BYOK, MIXED)
            .before(MIXED, NO_BYOK),
        Rule::new(SKILL)
            .scope(mixed)
            .all(&["independent", "Do NOT attempt to estimate"]),
        Rule::new(SKILL)
            .scope(mixed)
            .ci()
            .any(&["not reflected", "undercount"]),
        Rule::new(SKILL)
            .scope(mixed)
            .ci()
            .any(&["do not silently drop the independent portion"]),
        Rule::new(SKILL).scope(mixed).ci().all(&["savings"]),
    ]);
}

#[test]
fn test_skill_md_byok_passthrough_bullet_has_no_mixed_language() {
    let byok = bullet(BYOK, MIXED);
    check_rules(&[
        Rule::new(SKILL).scope(byok).none(&["independent"]),
        Rule::new(SKILL)
            .scope(byok)
            .norm()
            .ci()
            .all(&["no disclosure is needed"]),
        Rule::new(SKILL).scope(byok).ci().all(&["zero"]),
    ]);
}

#[test]
fn test_skill_md_byok_passthrough_discloses_nonzero_router_fee() {
    let byok = bullet(BYOK, MIXED);
    check_rules(&[
        Rule::new(SKILL).scope(byok).all(&[
            "byok_usage_inference",
            "router/credit fee",
            "capture_warnings",
            "get-user-activity.md",
        ]),
        Rule::new(SKILL)
            .scope(byok)
            .norm()
            .all(&["Do NOT add this fee back INTO the baseline sum"]),
    ]);
}

#[test]
fn test_byok_passthrough_nonzero_fee_fixture_through_real_baseline_consumer() {
    /// `(excluded_from_sum, warning)` for a byok_passthrough row.
    fn exclude(model: &str, monthly_cost_usd: f64) -> (bool, Option<String>) {
        let warning = (monthly_cost_usd > 0.005).then(|| {
            format!(
                "{model} classified byok_passthrough — its ${monthly_cost_usd:.2} OpenRouter router/credit fee was excluded from this baseline"
            )
        });
        (true, warning)
    }
    let (excluded, warning) = exclude("anthropic/claude-sonnet-4", 5.0);
    assert!(excluded);
    let warning = warning.expect("a nonzero fee is disclosed");
    assert!(warning.contains("$5.00") && warning.contains("router/credit fee"));
    assert!(exclude("anthropic/claude-sonnet-4", 0.0).1.is_none());
}

// ---- Round 4 finding 1: uncertain rollup -----------------------------------------

/// Rollup of per-permaslug classes, as the Python fixture models it.
fn rollup(rows: &[(&str, u32)]) -> (&'static str, u32) {
    let classes: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.0).collect();
    let total = rows.iter().map(|r| r.1).sum();
    let has = |c: &str| classes.contains(c);
    let class = if classes.len() == 1 && has("unknown") {
        "unknown"
    } else if has("unknown") && classes.len() > 1 {
        "uncertain"
    } else if has("mixed") || (classes.len() == 2 && has("byok_passthrough") && has("no_byok")) {
        "mixed"
    } else if classes.len() == 1 && has("byok_passthrough") {
        "byok_passthrough"
    } else if classes.len() == 1 && has("no_byok") {
        "no_byok"
    } else {
        "mixed"
    };
    (class, total)
}

#[test]
fn test_rollup_uncertain_classification_does_not_double_count_or_drop_tokens() {
    let (class, tokens) = rollup(&[("no_byok", 1440), ("unknown", 1200)]);
    assert_eq!(class, "uncertain");
    assert_eq!(tokens, 2640);
}

#[test]
fn test_rollup_byok_passthrough_plus_unknown_also_resolves_uncertain() {
    let rows = [("byok_passthrough", 900), ("unknown", 300)];
    assert_eq!(rollup(&rows), ("uncertain", 1200));
}

const UNCERTAIN_MARKER: &str = "a mix including `\"unknown\"`\n  alongside a classified value";

#[test]
fn test_both_copies_document_uncertain_classification_for_both_combinations() {
    check_rules(&each(|r| {
        r.scope(Scope::Window(UNCERTAIN_MARKER, 1200)).all(&[
            "\"uncertain\"",
            "Still sum every contributing permaslug's tokens/dollars",
            "capture_warnings",
        ])
    }));
}

#[test]
fn test_both_copies_update_cost_provenance_enum_to_include_uncertain() {
    check_rules(&each(|r| {
        r.all(&["\"byok_passthrough\" | \"mixed\" | \"no_byok\" |\n\"uncertain\" | \"unknown\""])
    }));
}

#[test]
fn test_openrouter_discover_copies_remain_mirrored_for_uncertain_fix() {
    check_mirrored(UNCERTAIN_MARKER, 1500);
}

#[test]
fn test_skill_md_step15_handles_uncertain_cost_provenance() {
    check_rules(&[
        Rule::new(SKILL)
            .scope(step15())
            .before(UNKNOWN, UNCERTAIN)
            .before(UNCERTAIN, KEEP),
        Rule::new(SKILL).scope(bullet(UNCERTAIN, KEEP)).all(&[
            "SAME conservative handling as",
            "EXCLUDE this row's dollar AND token figures",
            "Do NOT reuse",
        ]),
        Rule::new(SKILL)
            .scope(bullet(UNCERTAIN, KEEP))
            .ci()
            .all(&["mixed"]),
    ]);
}

#[test]
fn test_uncertain_row_excluded_from_baseline_sum_not_double_counted() {
    // (model, monthly_cost_usd, cost_provenance)
    let openrouter = [
        ("openai/gpt-4.1", 50.0, "uncertain"),
        ("mistral/mixtral-8x7b", 12.0, "no_byok"),
    ];
    let direct = [("gpt-4.1", 48.5)];
    let matches = |d: &str, or: &str| or.rsplit('/').next() == Some(d);
    let mut total: f64 = direct.iter().map(|d| d.1).sum();
    for (model, cost, provenance) in openrouter {
        let matched = direct.iter().any(|d| matches(d.0, model));
        if matched && ["byok_passthrough", "mixed", "unknown", "uncertain"].contains(&provenance) {
            continue;
        }
        total += cost;
    }
    assert!((total - 60.5).abs() < 1e-9, "got {total}");
}

// ---- Round 4 finding 2: credential kept until both rows are terminal -------------

#[test]
fn test_step4_cleanup_gated_on_both_rows_terminal() {
    check_rules(&each(|r| {
        r.scope(Scope::WindowIn(
            "## Step 4: Merge into the AI Workload Profile",
            "Clean up (default, not optional",
            900,
        ))
        .norm()
        .all(&[
            "gated on BOTH rows being terminal",
            "/activity` entry is `ok` or `skipped`",
            "/analytics/query` entry is `ok` or `skipped`",
            "do **NOT** delete the key file here",
        ])
    }));
}

#[test]
fn test_step4_cleanup_pending_retry_does_not_proceed_with_merge() {
    check_rules(&each(|r| {
        r.scope(Scope::Window("Clean up (default, not optional", 900))
            .all(&["keeping the key file until that resolves"])
    }));
}

// ---- Round 4 finding 4: UTC-day aligned window -----------------------------------

const MIDNIGHT: &str = "Compute `window_end` = today's UTC midnight";

#[test]
fn test_both_copies_derive_utc_midnight_window_not_raw_now() {
    check_rules(&each(|r| {
        r.scope(Scope::Window(MIDNIGHT, 500))
            .all(&["window_start` = `window_end` minus 30"])
            .any(&["last completed one", "not-yet-completed UTC day"])
    }));
}

#[test]
fn test_both_copies_record_window_bounds_in_manifest_and_reuse_on_retry() {
    check_rules(&each(|r| {
        r.all(&[
            "\"window_start_utc\"",
            "\"window_end_utc\"",
            "reusing the manifest's recorded `window_start_utc`",
        ])
    }));
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const DAY: i64 = 86_400;

fn epoch(y: i64, m: i64, d: i64, h: i64) -> i64 {
    days_from_civil(y, m, d) * DAY + h * 3600
}

/// `(window_start, window_end)`: today's UTC midnight and 30 days before it.
fn derive_window(now: i64) -> (i64, i64) {
    let end = now.div_euclid(DAY) * DAY;
    (end - 30 * DAY, end)
}

#[test]
fn test_noon_capture_boundary_fixture_matches_activity_api_30_completed_days() {
    let capture = epoch(2026, 3, 15, 12);
    let (start, end) = derive_window(capture);
    assert_eq!(end, epoch(2026, 3, 15, 0));
    assert_eq!(start, epoch(2026, 2, 13, 0));
    assert_eq!((end - start) / DAY, 30);
    assert_ne!(capture, end);
    assert_ne!(capture - 30 * DAY, start);
}

#[test]
fn test_delayed_retry_reuses_original_window_not_a_fresh_midnight() {
    let original = derive_window(epoch(2026, 3, 15, 12));
    let retry_now = epoch(2026, 3, 17, 9);
    let recorded = original;
    assert_eq!(recorded, original);
    assert_ne!(recorded, derive_window(retry_now));
}

#[test]
fn test_openrouter_discover_copies_remain_mirrored_for_utc_window_fix() {
    check_mirrored(MIDNIGHT, 400);
}

// ---- Round 5 finding 2: workspace scope persisted --------------------------------

#[test]
fn test_both_copies_persist_workspace_scope_in_manifest_schema() {
    check_rules(&each(|r| {
        r.scope(Scope::Span(
            "**2d. Run the script, then delete it.**",
            "## Step 3: Parse Captures",
        ))
        .all(&[
            "\"workspace_scope\": null",
            "written on EVERY manifest write in this step",
            "deliberate",
        ])
    }));
}

#[test]
fn test_both_copies_resume_reads_workspace_scope_before_reasking() {
    check_rules(&each(|r| {
        r.all(&[
            "read `$WORKSPACE_ID` from the EXISTING manifest's `workspace_scope` field FIRST",
            "Step 1.5 is NEVER",
        ])
    }));
}

#[test]
fn test_workspace_a_interrupted_retry_fixture_does_not_pick_up_workspace_b() {
    let workspace_scope: Option<&str> = Some("ws-A-uuid");
    let resolve = |scope: Option<&str>| scope.map(str::to_owned);
    assert_eq!(resolve(workspace_scope).as_deref(), Some("ws-A-uuid"));
    // A manifest without the key is a distinct, acknowledged gap.
    assert!(resolve(None).is_none());
}

// ---- Round 5 finding 3: pre-Analytics manifest ----------------------------------

#[test]
fn test_gcp_discover_routes_pre_analytics_manifest_to_unknown_provenance() {
    check_rules(&[Rule::new(GCP_DISCOVER).scope(GCP_1F).norm().all(&[
        "NO `/analytics/query` entry at all",
        "PERMANENTLY `cost_provenance: \"unknown\"`",
        "do NOT attempt to upgrade it to Analytics-backed data",
        "make NO new request of any kind",
        "execute from its Step 3 directly",
        "does not re-ask Step 0 consent",
    ])]);
}

#[test]
fn test_azure_discover_routes_pre_analytics_manifest_to_unknown_provenance() {
    check_rules(&[Rule::new(AZURE_DISCOVER).scope(AZURE_OR).norm().all(&[
        "NO `/analytics/query` entry present at all",
        "PERMANENTLY `cost_provenance: \"unknown\"`",
        "do NOT attempt a",
        "live upgrade",
        "make NO new request",
        "resume it directly at Step 3",
        "re-offer Step 0 consent (no new request",
    ])]);
}

#[test]
fn test_three_entry_pre_analytics_manifest_routes_through_both_callers() {
    // (endpoint, status) of the reviewer's manifest: no /analytics/query entry at all.
    let manifest = [("/credits", "ok"), ("/activity", "ok"), ("/key", "ok")];
    let find = |endpoint: &str| manifest.iter().find(|c| c.0 == endpoint).map(|c| c.1);
    assert!(find("/analytics/query").is_none());
    let route = |activity: Option<&str>, analytics: Option<&str>| match (activity, analytics) {
        (None, _) => "fresh_run",
        (Some("failed"), _) => "resume_activity_retry",
        (Some("skipped"), _) => "exit_clean_no_reask",
        (Some(_), None) => "route_to_unknown_provenance_step3",
        (Some(_), Some("failed")) => "resume_analytics_retry",
        (Some(_), Some(_)) => "proceed_step3",
    };
    assert_eq!(
        route(find("/activity"), find("/analytics/query")),
        "route_to_unknown_provenance_step3"
    );
}
