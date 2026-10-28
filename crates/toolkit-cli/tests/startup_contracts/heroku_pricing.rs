//! `test_heroku_postgres_advanced_pricing.py`: Heroku Postgres Advanced tier
//! pricing is modeled, not silently omitted.

use crate::common::{between, check, ensure, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const PRICING_CACHE: &str = "skills/heroku-to-aws/references/shared/heroku-pricing-cache.md";
const COST_ENGINE: &str = "skills/heroku-to-aws/references/phases/estimate/estimate-cost-engine.md";
const GENERATE_REPORT: &str = "skills/heroku-to-aws/references/phases/generate/generate-report.md";
const CAPTURE_MD: &str = "skills/heroku-to-aws/references/phases/discover/discover-live-capture.md";
const DISCOVER_LIVE: &str = "skills/heroku-to-aws/references/phases/discover/discover-live.md";
const SCHEMA_HEROKU: &str = "skills/heroku-to-aws/references/shared/schema-discover-heroku.md";

const ADVANCED: Scope = Scope::Span("### Advanced Tier", "### Deprecated Plans");
const USAGE_RULES: Scope = Scope::From("## Usage Rules");
const RUNG2: Scope = Scope::Span("2. **Live-captured prices", "3. **Heroku pricing cache**");
const RUNG3: Scope = Scope::Span("3. **Heroku pricing cache**", "4. **User-provided**");
const RUNG2_IDENTITY: Scope = Scope::WindowIn(
    "2. **Live-captured prices",
    "addon_service == \"heroku-postgresql\"",
    2500,
);
const IDENTITY: &str = "addon_service == \"heroku-postgresql\"";

const LEVELS: [&str; 11] = [
    "4G-Performance",
    "8G-Performance",
    "16G-Performance",
    "32G-Performance",
    "64G-Performance",
    "128G-Performance",
    "256G-Performance",
    "384G-Performance",
    "512G-Performance",
    "768G-Performance",
    "1536G-Performance",
];
const UNVERIFIED_LEVELS: usize = 7;
const CONFIRMED: [(&str, &str); 4] = [
    ("4G-Performance", "150"),
    ("8G-Performance", "300"),
    ("16G-Performance", "500"),
    ("32G-Performance", "900"),
];

#[test]
fn test_pricing_cache_has_advanced_tier_heading() {
    check_rules(&[Rule::new(PRICING_CACHE).all(&["### Advanced Tier"])]);
}

#[test]
fn test_pricing_cache_advanced_tier_lists_all_eleven_levels() {
    check_rules(&[Rule::new(PRICING_CACHE).scope(ADVANCED).all(&LEVELS)]);
}

fn unverified_marks(p: &Plugin) -> Verdict {
    let text = p.read(PRICING_CACHE)?;
    let section = between(&text, "### Advanced Tier", "### Deprecated Plans")?;
    ensure!(
        section.matches("unverified").count() >= UNVERIFIED_LEVELS,
        "Expected one 'unverified' marker per unconfirmed Advanced Tier level"
    );
    Ok(())
}

#[test]
fn test_pricing_cache_advanced_tier_marks_unconfirmed_rates_unverified() {
    check(
        unverified_marks,
        &[(
            edit_replace!(PRICING_CACHE, "unverified", "unchecked"),
            "Expected one",
        )],
    );
    let mut rules = Vec::new();
    for (level, rate) in CONFIRMED {
        rules.push(
            Rule::new(PRICING_CACHE)
                .scope(Scope::LineAfter(
                    "### Advanced Tier",
                    "### Advanced Tier",
                    level,
                ))
                .all(&[rate])
                .none(&["unverified"]),
        );
    }
    check_rules(&rules);
}

#[test]
fn test_pricing_cache_advanced_tier_states_storage_overage_rate() {
    check_rules(&[Rule::new(PRICING_CACHE).all(&["100 GB", "$0.20 per GB-month"])]);
}

#[test]
fn test_pricing_cache_usage_rules_document_advanced_formula() {
    check_rules(&[Rule::new(PRICING_CACHE).scope(USAGE_RULES).all(&[
        "compute-month",
        "data_size_gb",
        "unpriced_heroku",
    ])]);
}

#[test]
fn test_pricing_cache_advanced_tier_documents_multi_pool_known_limitation() {
    check_rules(&[
        Rule::new(PRICING_CACHE).scope(ADVANCED).all(&[
            "Known limitation",
            "advanced_pools",
            "Terraform",
            "unpriced_heroku",
        ]),
        Rule::new(PRICING_CACHE)
            .scope(USAGE_RULES)
            .all(&["instance_count", "advanced_pools"])
            .any(&[
                "rate[pool.level] × pool.instance_count",
                "rate[pool.level] x pool.instance_count",
            ]),
    ]);
}

#[test]
fn test_pricing_cache_classic_tier_rows_untouched() {
    check_rules(&[Rule::new(PRICING_CACHE).all(&[
        "| standard-0 | 50      | 4 GB   | 64 GB   | 200         |",
        "| premium-0 | 200     | 4 GB   | 64 GB   | 200         |",
        "| private-0 | 200     | 4 GB   | 64 GB   | 200         |",
        "| shield-0 | 200     | 4 GB   | 64 GB   | 200         |",
    ])]);
}

#[test]
fn test_generate_report_requires_rendering_baseline_note() {
    check_rules(&[
        Rule::new(GENERATE_REPORT)
            .scope(Scope::Span("## Step 1: Gather figures", "## Step 2:"))
            .all(&["baseline_note"]),
        Rule::new(GENERATE_REPORT)
            .scope(Scope::Span("| `exec-costs`", "\n"))
            .all(&["baseline_note"]),
    ]);
}

#[test]
fn test_cost_engine_rung3_references_advanced_tier_formula() {
    check_rules(&[Rule::new(COST_ENGINE).scope(RUNG3).all(&[
        "Heroku Postgres Advanced",
        "data_size_gb",
        "baseline_note",
        "instance_count",
        "advanced_pools",
        "Terraform",
        "unpriced_heroku",
    ])]);
}

#[test]
fn test_discover_live_capture_probes_both_postgres_tiers() {
    check_rules(&[
        Rule::new(CAPTURE_MD).all(&[
            "heroku data:pg:info",
            "heroku pg:info",
            "pg-advanced-<app>-<addon_name>.out",
            "pg-<app>.out",
        ]),
        Rule::new(CAPTURE_MD)
            .scope(Scope::From("3c. Per-command error handling"))
            .all(&["skipped"]),
        Rule::new(CAPTURE_MD)
            .scope(Scope::From("3c. Per-command error handling"))
            .ci()
            .all(&["tier"]),
    ]);
}

#[test]
fn test_discover_live_parses_advanced_pool_blocks() {
    check_rules(&[Rule::new(DISCOVER_LIVE).all(&[
        "pg-advanced-<app>-<addon_name>.out",
        "instances of",
        "(HA)",
        "advanced_pools",
        "tier",
        "pg-<app>.out",
    ])]);
}

#[test]
fn test_discover_live_associates_each_database_by_addon_name_not_just_app() {
    check_rules(&[Rule::new(DISCOVER_LIVE)
        .all(&[
            "pg-advanced-<app>-<addon_name>.out",
            "multiple Postgres add-ons",
        ])
        .any(&["never fan out one add-on's", "never fan out"])]);
}

#[test]
fn test_discover_live_capture_row_7b_passes_explicit_database_identifier() {
    check_rules(&[
        Rule::new(CAPTURE_MD)
            .none(&["heroku data:pg:info -a <app>"])
            .all(&[
                "heroku data:pg:info <addon_name> -a <app>",
                "pg-advanced-<app>-<addon_name>.out",
            ]),
        Rule::new(CAPTURE_MD)
            .scope(Scope::From("**Row 7b note:**"))
            .none(&["not confirmed against a live Advanced account"])
            .all(&["REQUIRED", "AddonResolver"]),
    ]);
}

#[test]
fn test_cost_engine_rung2_prices_advanced_tier_via_pool_sum_not_flat_price() {
    check_rules(&[Rule::new(COST_ENGINE).scope(RUNG2).all(&[
        "tier == \"advanced\"",
        "advanced_pools",
        "rate[pool.level]",
        "instance_count",
    ])]);
}

#[test]
fn test_cost_engine_marks_advanced_tier_unpriced_when_pool_data_missing() {
    check_rules(&[
        Rule::new(COST_ENGINE)
            .scope(RUNG2)
            .all(&["unpriced_heroku"]),
        Rule::new(COST_ENGINE)
            .scope(RUNG3)
            .all(&["unpriced_heroku"]),
    ]);
}

#[test]
fn test_schema_documents_advanced_tier_fields() {
    check_rules(&[Rule::new(SCHEMA_HEROKU).all(&[
        "tier",
        "advanced_pools",
        "instance_count",
        "leader",
        "follower",
    ])]);
}

#[test]
fn test_cost_engine_rung2_detects_advanced_family_by_identity_not_tier() {
    check_rules(&[Rule::new(COST_ENGINE)
        .scope(RUNG2)
        .all(&["plan", "== \"advanced\""])
        .before(IDENTITY, "tier ==")]);
}

#[test]
fn test_cost_engine_rung2_marks_failed_capture_record_unpriced_not_zero() {
    check_rules(&[
        Rule::new(COST_ENGINE)
            .scope(RUNG2)
            .all(&["unpriced_heroku"]),
        Rule::new(COST_ENGINE)
            .scope(RUNG2_IDENTITY)
            .all(&["unpriced_heroku", "do NOT", "$0"])
            .any(&["failed", "skipped", "malformed"]),
    ]);
}

#[test]
fn test_cost_engine_rung2_covers_private_and_shield_advanced_variants() {
    check_rules(&[Rule::new(COST_ENGINE)
        .scope(RUNG2)
        .all(&["Private", "Shield"])]);
}

#[test]
fn test_cost_engine_rung3_detects_advanced_family_by_identity_not_tier() {
    check_rules(&[Rule::new(COST_ENGINE)
        .scope(RUNG3)
        .all(&["plan", "== \"advanced\""])
        .before(IDENTITY, "tier ==")]);
}

#[test]
fn test_cost_engine_rung3_covers_failed_capture_and_private_shield() {
    check_rules(&[Rule::new(COST_ENGINE)
        .scope(RUNG3)
        .all(&["Private", "Shield", "unpriced_heroku"])
        .any(&["failed", "skipped", "malformed"])]);
}

#[test]
fn test_pricing_cache_rule9_gates_on_identity_not_tier() {
    check_rules(&[Rule::new(PRICING_CACHE)
        .scope(Scope::FromIn(
            "## Usage Rules",
            "9. **Heroku Postgres Advanced",
        ))
        .all(&[
            "plan",
            "== \"advanced\"",
            "instance_count",
            "advanced_pools",
            "unpriced_heroku",
        ])
        .before(IDENTITY, "tier ==")]);
}
