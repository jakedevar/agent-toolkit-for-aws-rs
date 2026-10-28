//! Attributes derived from the run's own artifacts.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use toolkit_core::Json;

use super::fx::{Fx, RUN_ID, SESSION_ID};
use super::migration::{
    activity, all_completed, gcp_inventory, phase_status, with_phases, Project,
};
use crate::telemetry::attributes as attrs;

fn j(value: &Value) -> Json {
    toolkit_core::parse_json(&value.to_string()).expect("json")
}

fn fixture() -> Fx {
    let fx = Fx::new("attributes");
    fx.write_record("ACCEPTED", RUN_ID);
    fx
}

/// The activity of the event named `phase` (or the run-level event name).
fn by_name(bodies: &[Value]) -> std::collections::BTreeMap<String, Value> {
    bodies
        .iter()
        .map(|body| {
            let a = activity(body);
            let key = a["phase"]
                .as_str()
                .unwrap_or_else(|| a["eventName"].as_str().expect("name"));
            (key.to_string(), a.clone())
        })
        .collect()
}

fn repo_fixture(relative: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/aws-startup-advisor/fixtures")
        .join(relative);
    serde_json::from_str(&fs::read_to_string(&path).expect("committed fixture"))
        .expect("fixture JSON")
}

// -------------------------------------------------------------- converters

#[test]
fn spend_bands_split_at_100_1k_and_10k_and_refuse_negative_nan_and_non_numbers() {
    let band = |value: Value| attrs::to_spend_band(Some(&j(&value)));
    assert_eq!(band(json!(0)), Some("UNDER_100"));
    assert_eq!(band(json!(99.99)), Some("UNDER_100"));
    assert_eq!(band(json!(100)), Some("FROM_100_TO_1K"));
    assert_eq!(band(json!(999)), Some("FROM_100_TO_1K"));
    assert_eq!(band(json!(1000)), Some("FROM_1K_TO_10K"));
    assert_eq!(band(json!(9999.5)), Some("FROM_1K_TO_10K"));
    assert_eq!(band(json!(10000)), Some("OVER_10K"));
    assert_eq!(band(json!("2500")), Some("FROM_1K_TO_10K"));
    assert_eq!(
        band(json!(" 2_500 ")),
        Some("FROM_1K_TO_10K"),
        "Python float() accepts underscores"
    );
    for refused in [
        json!(-1),
        json!("-5"),
        json!("nan"),
        json!("$2500"),
        json!(true),
        json!(null),
        json!([1]),
        json!({}),
    ] {
        assert_eq!(band(refused.clone()), None, "{refused}");
    }
    assert_eq!(attrs::to_spend_band(None), None);
}

#[test]
fn projected_costs_round_half_to_even_inside_the_model_range_and_are_never_clamped() {
    let usd = |value: Value| attrs::to_projected_usd(Some(&j(&value)));
    assert_eq!(usd(json!(62.4)), Some(62));
    assert_eq!(usd(json!(62.5)), Some(62), "Python round() is half to even");
    assert_eq!(usd(json!(63.5)), Some(64));
    assert_eq!(usd(json!("110")), Some(110));
    assert_eq!(usd(json!(0)), Some(0));
    assert_eq!(usd(json!(10_000_000)), Some(10_000_000));
    for refused in [
        json!(10_000_001),
        json!(-0.5),
        json!("x"),
        json!(true),
        json!(null),
        json!("nan"),
    ] {
        assert_eq!(usd(refused.clone()), None, "{refused}");
    }
}

#[test]
fn the_accuracy_band_follows_the_upper_bound_of_the_percentage() {
    let band = |text: &str| attrs::to_accuracy_band(Some(&Json::string(text)));
    assert_eq!(band("\u{b1}5-10%"), Some("HIGH"));
    assert_eq!(band("\u{b1}5% (billing)"), Some("HIGH"));
    assert_eq!(band("\u{b1}10-20%"), Some("MEDIUM"));
    assert_eq!(band("\u{b1}20-30%+"), Some("LOW"));
    assert_eq!(band("about 15 %"), Some("MEDIUM"));
    assert_eq!(band("99999999999999999999999999%"), Some("LOW"));
    assert_eq!(band("as stated by the customer"), None);
    assert_eq!(attrs::to_accuracy_band(Some(&Json::Int(5))), None);
    assert_eq!(attrs::to_accuracy_band(None), None);
}

#[test]
fn classification_coverage_is_full_near_complete_or_partial() {
    let coverage = |value: Value| attrs::to_coverage(Some(&j(&value)));
    assert_eq!(coverage(json!("100%")), Some("FULL"));
    assert_eq!(coverage(json!(100)), Some("FULL"));
    assert_eq!(coverage(json!("97%")), Some("NEAR_COMPLETE"));
    assert_eq!(coverage(json!(90.0)), Some("NEAR_COMPLETE"));
    assert_eq!(coverage(json!("89.9 %")), Some("PARTIAL"));
    for refused in [
        json!(""),
        json!(null),
        json!("lots"),
        json!(true),
        json!([]),
    ] {
        assert_eq!(coverage(refused.clone()), None, "{refused}");
    }
}

#[test]
fn enum_values_are_normalised_to_upper_snake_and_admitted_only_if_modelled() {
    let members = ["MULTI_AZ_HA", "US_EAST_1", "SMALL"];
    let member = |value: Value| attrs::to_enum(&members, Some(&j(&value)));
    assert_eq!(member(json!("multi-az-ha")).as_deref(), Some("MULTI_AZ_HA"));
    assert_eq!(member(json!("us-east-1")).as_deref(), Some("US_EAST_1"));
    assert_eq!(member(json!("  Small  ")).as_deref(), Some("SMALL"));
    assert_eq!(member(json!("__small__")).as_deref(), Some("SMALL"));
    assert_eq!(member(json!("multi az ha")).as_deref(), Some("MULTI_AZ_HA"));
    for refused in [
        json!("eu-moon-1"),
        json!(""),
        json!(null),
        json!(7),
        json!("__proto__"),
        json!("small!!x"),
    ] {
        assert_eq!(member(refused.clone()), None, "{refused}");
    }
}

#[test]
fn compliance_aliases_dedupe_and_an_explicit_empty_list_is_none() {
    let list = |value: Value| attrs::to_compliance_list(Some(&j(&value)));
    assert_eq!(list(json!([])), Some(vec!["NONE".to_string()]));
    assert_eq!(
        list(json!(["pci-dss", "PCI", "soc 2", "hipaa", "fed_ramp"])),
        Some(vec![
            "PCI".into(),
            "SOC2".into(),
            "HIPAA".into(),
            "FEDRAMP".into()
        ])
    );
    assert_eq!(list(json!("gdpr")), Some(vec!["GDPR".to_string()]));
    assert_eq!(list(json!(["iso-27001"])), None);
    assert_eq!(list(json!(null)), None);
    assert_eq!(attrs::to_compliance_list(None), None);
}

#[test]
fn pricing_source_marks_a_stale_cache_and_raises_on_a_malformed_staleness() {
    let source = |value: Value| attrs::to_pricing_source(Some(&j(&value)));
    assert_eq!(source(json!("live")), Ok(Some("LIVE")));
    assert_eq!(
        source(json!({"status": "cached", "fallback_staleness": {"is_stale": false}})),
        Ok(Some("CACHED"))
    );
    assert_eq!(
        source(json!({"status": "cached", "fallback_staleness": {"is_stale": true}})),
        Ok(Some("CACHED_STALE"))
    );
    assert_eq!(source(json!({"status": "mystery"})), Ok(None));
    assert_eq!(
        source(json!({"status": "cached", "fallback_staleness": ["x"]})),
        Err(attrs::Abort)
    );
    assert_eq!(attrs::to_pricing_source(None), Ok(None));
}

#[test]
fn python_float_text_rules() {
    for (text, expected) in [
        ("1", Some(1.0)),
        (" 2.5 ", Some(2.5)),
        ("1_0", Some(10.0)),
        ("1e3", Some(1000.0)),
        (".5", Some(0.5)),
    ] {
        assert_eq!(attrs::py_float(text), expected, "{text}");
    }
    for text in ["", "_1", "1_", "1__0", "1 0", "0x10", "$1"] {
        assert_eq!(attrs::py_float(text), None, "{text}");
    }
    assert!(attrs::py_float("inf").is_some_and(f64::is_infinite));
    assert!(attrs::py_float("nan").is_some_and(f64::is_nan));
}

// --------------------------------------------------------------- end to end

#[test]
fn attributes_follow_producer_shaped_artifacts_including_the_ai_only_route() {
    let fx = fixture();
    let done = phase_status(
        json!({"current_phase": "complete", "run_mode": "decide", "phases": all_completed()}),
    );
    let gcp = Project::new(
        &fx,
        &done,
        &[
            ("gcp-resource-inventory.json", gcp_inventory()),
            (
                "preferences.json",
                json!({"metadata": {"clarify_mode": "wizard", "migration_type": "full"},
                       "design_constraints": {"gcp_monthly_spend": {"value": "$1K-$5K", "chosen_by": "user"}}}),
            ),
            (
                "estimation-infra.json",
                json!({"recommendation": {"outcome": "defer_for_evidence"},
                       "pricing_source": {"status": "cached", "fallback_staleness": {"is_stale": false}},
                       "current_costs": {"source": "preferences", "gcp_monthly_spend": 2500}}),
            ),
        ],
    );
    let mut ai_status = done.clone();
    ai_status["initiated_by"] = json!("LLM_TO_BEDROCK");
    let ai_only = Project::named(
        &fx,
        &ai_status,
        &[
            (
                "ai-workload-profile.json",
                json!({"summary": {"ai_source": "openai", "total_models_detected": 2}}),
            ),
            (
                "preferences.json",
                json!({"metadata": {"clarify_mode": "fast_path", "migration_type": "ai-only"}}),
            ),
        ],
        "0226-1500",
    );
    let g = by_name(&gcp.reconcile(SESSION_ID));
    let a = by_name(&ai_only.reconcile(SESSION_ID));
    assert_eq!(
        g["DISCOVER"]["attributes"],
        json!({"sourceProvider": "GCP", "resourceCount": 2, "hasDatabase": true, "hasAi": false})
    );
    assert_eq!(
        g["CLARIFY"]["attributes"],
        json!({"sourceProvider": "GCP", "clarifyMode": "WIZARD"})
    );
    assert_eq!(
        g["ESTIMATE"]["attributes"],
        json!({"sourceProvider": "GCP", "recommendationOutcome": "DEFER", "pricingSource": "CACHED",
               "spendBand": "FROM_1K_TO_10K", "spendBasis": "USER_PROVIDED"})
    );
    assert_eq!(
        g["RUN_COMPLETED"]["attributes"],
        json!({"sourceProvider": "GCP", "runMode": "DECIDE"})
    );
    assert_eq!(
        a["DISCOVER"]["attributes"],
        json!({"sourceProvider": "OPENAI", "hasAi": true})
    );
    assert_eq!(
        a["CLARIFY"]["attributes"],
        json!({"sourceProvider": "OPENAI", "clarifyMode": "AI_ONLY"})
    );
    assert_eq!(a["RUN_STARTED"]["initiatingSkill"], "LLM_TO_BEDROCK");
    assert!(
        g["RUN_STARTED"].get("initiatingSkill").is_none(),
        "an event carries only a known initiator"
    );
}

#[test]
fn an_unknown_initiator_is_dropped_rather_than_rejecting_the_event() {
    let fx = fixture();
    let mut status = phase_status(json!({}));
    status["initiated_by"] = json!("SOMETHING_ELSE");
    let p = Project::gcp(&fx, &status);
    assert!(p
        .reconcile(SESSION_ID)
        .iter()
        .all(|b| activity(b).get("initiatingSkill").is_none()));
}

#[test]
fn azure_runs_report_with_their_own_inventory_vocabulary() {
    let fx = fixture();
    let p = Project::new(
        &fx,
        &phase_status(json!({"owning_skill": "AZURE_TO_AWS"})),
        &[(
            "azure-resource-inventory.json",
            json!({"resources": [
                {"azure_type": "Microsoft.DocumentDB/databaseAccounts", "name": "db"},
                {"azure_type": "Microsoft.CognitiveServices/accounts", "name": "ai"},
                {"azure_type": "Microsoft.App/containerApps", "name": "api"},
            ]}),
        )],
    );
    let events = by_name(&p.reconcile(SESSION_ID));
    assert_eq!(events["RUN_STARTED"]["skill"], "AZURE_TO_AWS");
    assert_eq!(
        events["DISCOVER"]["attributes"],
        json!({"sourceProvider": "AZURE", "resourceCount": 3, "hasDatabase": true, "hasAi": true})
    );
}

#[test]
fn the_committed_azure_fixtures_map_their_producer_fields() {
    // 30 ARM resources (PostgreSQL, Redis, DocumentDB among them) and an
    // estimate whose baseline is the customer's stated USD 4200.
    let fx = fixture();
    let inventory =
        repo_fixture("azure-iac-terraform/after-discover/azure-resource-inventory.json");
    let estimate = repo_fixture("azure-iac-terraform/after-estimate/estimation-infra.json");
    let p = Project::new(
        &fx,
        &phase_status(json!({
            "owning_skill": "AZURE_TO_AWS", "current_phase": "complete", "run_mode": "decide",
            "phases": with_phases(&[("clarify", "completed"), ("design", "completed"), ("estimate", "completed")]),
        })),
        &[
            ("azure-resource-inventory.json", inventory),
            ("estimation-infra.json", estimate),
        ],
    );
    let events = by_name(&p.reconcile(SESSION_ID));
    let discover = &events["DISCOVER"]["attributes"];
    assert_eq!(discover["sourceProvider"], "AZURE");
    assert_eq!(discover["resourceCount"], 30);
    assert_eq!(discover["hasDatabase"], true);
    let estimate = &events["ESTIMATE"]["attributes"];
    assert_eq!(estimate["spendBand"], "FROM_1K_TO_10K");
    assert_eq!(estimate["spendBasis"], "USER_PROVIDED");
    assert_eq!(estimate["recommendationOutcome"], "CONDITIONAL_GO");
    assert!(
        estimate.get("estimateAccuracyBand").is_none(),
        "'as stated by the customer' is not a percentage band"
    );
}

#[test]
fn a_heroku_database_is_detected_from_the_add_on_service() {
    let fx = fixture();
    let formation = json!({"resource_type": "formation", "name": "web"});
    let status = phase_status(json!({"owning_skill": "HEROKU_TO_AWS"}));
    let with_db = Project::new(
        &fx,
        &status,
        &[(
            "heroku-resource-inventory.json",
            json!({"resources": [formation, {"resource_type": "addon", "config": {"addon_service": "heroku-postgresql"}}]}),
        )],
    );
    let without = Project::named(
        &fx,
        &status,
        &[(
            "heroku-resource-inventory.json",
            json!({"resources": [formation, {"resource_type": "addon", "config": {"addon_service": "papertrail"}}]}),
        )],
        "0226-1500",
    );
    let d = by_name(&with_db.reconcile(SESSION_ID));
    let n = by_name(&without.reconcile(SESSION_ID));
    assert_eq!(
        d["DISCOVER"]["attributes"],
        json!({"sourceProvider": "HEROKU", "resourceCount": 2, "hasDatabase": true, "hasAi": false})
    );
    assert_eq!(n["DISCOVER"]["attributes"]["hasDatabase"], false);
}

#[test]
fn heroku_clarify_answers_are_read_from_preferences_global() {
    let fx = fixture();
    let preferences = repo_fixture("heroku-workshop/seed/preferences.json");
    let p = Project::new(
        &fx,
        &phase_status(
            json!({"owning_skill": "HEROKU_TO_AWS", "current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
        ),
        &[
            ("heroku-resource-inventory.json", json!({"resources": []})),
            ("preferences.json", preferences),
        ],
    );
    let clarify = &by_name(&p.reconcile(SESSION_ID))["CLARIFY"]["attributes"];
    for (key, expected) in [
        ("compliance", json!(["NONE"])),
        ("availability", json!("SINGLE_AZ")),
        ("targetRegion", json!("US_EAST_1")),
        ("computePosture", json!("ELASTIC_BEANSTALK")),
    ] {
        assert_eq!(clarify[key], expected, "{key}");
    }
}

#[test]
fn the_gcp_clarify_answers_the_producer_writes_are_mapped() {
    let fx = fixture();
    let clarified = |constraints: Value, run: &str| {
        Project::named(
            &fx,
            &phase_status(
                json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
            ),
            &[
                ("gcp-resource-inventory.json", gcp_inventory()),
                (
                    "preferences.json",
                    json!({"design_constraints": constraints}),
                ),
            ],
            run,
        )
    };
    let explicit = clarified(
        json!({"compliance": {"value": []}, "database_traffic": {"value": "write-heavy-global"}, "kubernetes": {"value": "eks-managed"}, "db_size": {"value": "10 - 100 GB"}, "cutover_strategy": {"value": "zero-downtime"}}),
        "0226-1501",
    );
    let absent = clarified(
        json!({"database_traffic": {"value": "rapidly-growing"}}),
        "0226-1502",
    );
    let unsupported = clarified(json!({"compliance": {"value": ["iso-27001"]}}), "0226-1503");
    let attributes =
        |p: &Project<'_>| by_name(&p.reconcile(SESSION_ID))["CLARIFY"]["attributes"].clone();
    let (a, b, c) = (
        attributes(&explicit),
        attributes(&absent),
        attributes(&unsupported),
    );
    assert_eq!(
        (&a["compliance"], &a["databaseTraffic"]),
        (&json!(["NONE"]), &json!("WRITE_HEAVY"))
    );
    assert_eq!(a["computePosture"], "EKS_MANAGED");
    assert_eq!(a["dbSize"], "DB_10_100GB");
    assert_eq!(a["cutoverStrategy"], "ZERO_DOWNTIME");
    assert_eq!(
        (b.get("compliance"), b.get("databaseTraffic")),
        (None, None)
    );
    assert!(c.get("compliance").is_none());
}

#[test]
fn a_generation_tier_is_dropped_once_a_confirmed_re_entry_resets_generate() {
    let fx = fixture();
    let p = Project::new(
        &fx,
        &phase_status(
            json!({"current_phase": "complete", "run_mode": "execute", "phases": all_completed()}),
        ),
        &[
            ("gcp-resource-inventory.json", gcp_inventory()),
            ("generation-infra.json", json!({"complexity_tier": "small"})),
            (
                "migration-preview.json",
                json!({"complexity_signal": "likely_simple"}),
            ),
        ],
    );
    let first = by_name(&p.reconcile(SESSION_ID));
    fs::write(
        p.run_dir.join("migration-preview.json"),
        json!({"complexity_signal": "complex"}).to_string(),
    )
    .expect("preview");
    p.write_status(&phase_status(json!({"current_phase": "estimate", "phases": with_phases(&[("clarify", "completed"), ("design", "pending")])})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(json!({"current_phase": "estimate", "phases": with_phases(&[("clarify", "completed"), ("design", "completed")])})));
    let redesign = by_name(&p.reconcile(SESSION_ID))["DESIGN"].clone();
    assert_eq!(first["DESIGN"]["attributes"]["complexityTier"], "SMALL");
    assert_eq!(redesign["attributes"]["complexityTier"], "LARGE");
}

#[test]
fn the_provenance_of_a_preferences_sourced_spend_figure_is_reported() {
    let fx = fixture();
    let estimated = |spend: Value, run: &str| {
        Project::named(
            &fx,
            &phase_status(
                json!({"current_phase": "workshop", "phases": with_phases(&[("clarify", "completed"), ("design", "completed"), ("estimate", "completed")])}),
            ),
            &[
                ("gcp-resource-inventory.json", gcp_inventory()),
                (
                    "preferences.json",
                    json!({"design_constraints": {"gcp_monthly_spend": spend}}),
                ),
                (
                    "estimation-infra.json",
                    json!({"current_costs": {"source": "preferences", "gcp_monthly": 3000}}),
                ),
            ],
            run,
        )
    };
    let cases = [
        (
            "default",
            json!({"value": "$1K-$5K", "chosen_by": "default", "source": "default:Q3"}),
            Some("DEFAULTED"),
        ),
        (
            "user",
            json!({"value": "$1K-$5K", "chosen_by": "user"}),
            Some("USER_PROVIDED"),
        ),
        (
            "extracted",
            json!({"value": "$1K-$5K", "chosen_by": "extracted", "source": "billing:monthly_total=$3000"}),
            Some("BILLING_DATA"),
        ),
        ("unknown", json!({"value": "$1K-$5K"}), None),
    ];
    for (index, (name, spend, basis)) in cases.into_iter().enumerate() {
        let p = estimated(spend, &format!("0226-151{index}"));
        let attributes = by_name(&p.reconcile(SESSION_ID))["ESTIMATE"]["attributes"].clone();
        match basis {
            Some(basis) => {
                assert_eq!(attributes["spendBand"], "FROM_1K_TO_10K", "{name}");
                assert_eq!(attributes["spendBasis"], basis, "{name}");
            }
            None => assert!(
                attributes.get("spendBand").is_none() && attributes.get("spendBasis").is_none(),
                "{name}"
            ),
        }
    }
}

#[test]
fn the_enriched_estimate_attributes_are_derived() {
    let fx = fixture();
    let p = Project::new(
        &fx,
        &phase_status(
            json!({"current_phase": "workshop", "phases": with_phases(&[("clarify", "completed"), ("design", "completed"), ("estimate", "completed")])}),
        ),
        &[
            (
                "gcp-resource-inventory.json",
                json!({"resources": gcp_inventory()["resources"], "summary": {"classification_coverage": "97%"}}),
            ),
            (
                "estimation-infra.json",
                json!({
                    "recommendation": {"outcome": "go", "confidence": "high"},
                    "current_costs": {"source": "billing_data", "gcp_monthly": 1200, "accuracy": "\u{b1}5-10%"},
                    "migration_cost_considerations": {"billing_data_available": true},
                    "projected_costs": {"aws_monthly_optimized": 62.4, "aws_monthly_balanced": 85, "aws_monthly_premium": "110"},
                    "estimation_summary": {"complexity_tier": "medium"},
                }),
            ),
        ],
    );
    let by_phase = by_name(&p.reconcile(SESSION_ID));
    assert_eq!(
        by_phase["DISCOVER"]["attributes"]["classificationCoverage"],
        "NEAR_COMPLETE"
    );
    assert_eq!(
        by_phase["ESTIMATE"]["attributes"],
        json!({
            "sourceProvider": "GCP", "recommendationOutcome": "GO", "recommendationConfidence": "HIGH",
            "estimateAccuracyBand": "HIGH", "billingDataAvailable": true,
            "awsProjectedCostOptimized": 62, "awsProjectedCostBalanced": 85, "awsProjectedCostPremium": 110,
            "spendBand": "FROM_1K_TO_10K", "spendBasis": "BILLING_DATA", "complexityTier": "MEDIUM",
        })
    );
}

#[test]
fn attributes_keep_python_dict_insertion_order_on_the_wire() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    let body = fx
        .sink
        .all_raw()
        .into_iter()
        .find(|b| b.contains("\"DISCOVER\""))
        .expect("discover body");
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {body}"))
    };
    assert!(at("\"sourceProvider\"") < at("\"resourceCount\""));
    assert!(at("\"resourceCount\"") < at("\"hasDatabase\""));
    assert!(at("\"hasDatabase\"") < at("\"hasAi\""));
    assert!(at("\"eventName\"") < at("\"skill\"") && at("\"skill\"") < at("\"runId\""));
}

// ------------------------------------------------- shapes Python raised on

fn derive(
    dir: &Path,
    skill: &str,
    event_name: &'static str,
    phase: &str,
    status: &Value,
) -> attrs::R<Option<Json>> {
    let mut event = attrs::Event::named(event_name);
    event.phase = Some(phase.to_string());
    attrs::derive_attributes(dir, skill, &event, &j(status))
}

fn temp_run(files: &[(&str, Value)]) -> (super::fx::TempDir, PathBuf) {
    let dir = super::fx::TempDir::new("derive");
    let path = dir.path().to_path_buf();
    for (name, value) in files {
        fs::write(path.join(name), value.to_string()).expect("artifact");
    }
    (dir, path)
}

#[test]
fn artifacts_of_a_shape_the_python_raised_on_abort_the_attributes() {
    let status = phase_status(json!({}));
    for (file, value) in [
        (
            "ai-workload-profile.json",
            json!({"summary": {"total_models_detected": null}}),
        ),
        (
            "ai-workload-profile.json",
            json!({"summary": {"total_models_detected": "2"}}),
        ),
        ("ai-workload-profile.json", json!(["x"])),
        ("ai-workload-profile.json", json!({"summary": [1]})),
    ] {
        let (_guard, dir) = temp_run(&[
            (file, value.clone()),
            ("gcp-resource-inventory.json", gcp_inventory()),
        ]);
        assert_eq!(
            derive(&dir, "GCP_TO_AWS", "PHASE_COMPLETED", "DISCOVER", &status),
            Err(attrs::Abort),
            "{value}"
        );
    }
    let (_guard, dir) = temp_run(&[("preferences.json", json!({"design_constraints": ["x"]}))]);
    assert_eq!(
        derive(&dir, "GCP_TO_AWS", "PHASE_COMPLETED", "CLARIFY", &status),
        Err(attrs::Abort)
    );
    let (_guard, dir) = temp_run(&[("estimation-infra.json", json!({"recommendation": "go"}))]);
    assert_eq!(
        derive(&dir, "GCP_TO_AWS", "PHASE_COMPLETED", "ESTIMATE", &status),
        Err(attrs::Abort)
    );
    let (_guard, dir) = temp_run(&[("validation-report.json", json!([1]))]);
    assert_eq!(
        derive(&dir, "GCP_TO_AWS", "PHASE_COMPLETED", "GENERATE", &status),
        Err(attrs::Abort)
    );
}

#[test]
fn falsy_containers_read_as_empty_and_a_models_list_decides_ai_from_the_profile() {
    let status = phase_status(json!({}));
    let (_guard, dir) = temp_run(&[
        (
            "gcp-resource-inventory.json",
            json!({"resources": [{"type": "vertex_ai_endpoint"}, "not-an-object", {"type": "", "resource_type": "cloud_run"}]}),
        ),
        (
            "ai-workload-profile.json",
            json!({"models": [], "summary": ""}),
        ),
    ]);
    let attributes = derive(&dir, "GCP_TO_AWS", "PHASE_COMPLETED", "DISCOVER", &status)
        .expect("derived")
        .expect("some");
    assert_eq!(
        attributes.get("hasAi"),
        Some(&Json::Bool(true)),
        "vertex is an AI type"
    );
    assert_eq!(attributes.get("resourceCount"), Some(&Json::Int(3)));
    assert_eq!(attributes.get("hasDatabase"), Some(&Json::Bool(false)));
    let (_guard, dir) = temp_run(&[("ai-workload-profile.json", json!({"models": ["m"]}))]);
    let ai_only = derive(
        &dir,
        "LLM_TO_BEDROCK",
        "PHASE_COMPLETED",
        "DISCOVER",
        &status,
    )
    .expect("derived")
    .expect("some");
    assert_eq!(ai_only.get("hasAi"), Some(&Json::Bool(true)));
    let (_guard, dir) = temp_run(&[]);
    assert_eq!(
        derive(&dir, "LLM_TO_BEDROCK", "RUN_STARTED", "", &status),
        Ok(None),
        "nothing to say is no attributes"
    );
}

#[test]
fn unreadable_artifacts_are_absent_artifacts() {
    let status = phase_status(json!({}));
    let dir = super::fx::TempDir::new("unreadable");
    fs::write(
        dir.path().join("gcp-resource-inventory.json"),
        "{\"resources\": [",
    )
    .expect("truncated");
    fs::write(dir.path().join("preferences.json"), "null").expect("null");
    fs::create_dir(dir.path().join("estimation-infra.json")).expect("directory");
    let discover = derive(
        dir.path(),
        "GCP_TO_AWS",
        "PHASE_COMPLETED",
        "DISCOVER",
        &status,
    );
    assert_eq!(
        discover,
        Ok(Some(j(&json!({"sourceProvider": "GCP"})))),
        "the inventory file exists, so the provider is known"
    );
    assert_eq!(
        derive(
            dir.path(),
            "GCP_TO_AWS",
            "PHASE_COMPLETED",
            "CLARIFY",
            &status
        ),
        Ok(Some(j(&json!({"sourceProvider": "GCP"}))))
    );
    assert_eq!(
        derive(
            dir.path(),
            "GCP_TO_AWS",
            "PHASE_COMPLETED",
            "ESTIMATE",
            &status
        ),
        Ok(Some(j(&json!({"sourceProvider": "GCP"}))))
    );
}
