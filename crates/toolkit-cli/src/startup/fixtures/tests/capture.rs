//! The live-capture programs. The repository ships the captured INPUTS of
//! these scenarios (`live-capture/`, `workspace-terraform/`, `seed-*`) but not
//! the replay OUTPUT, which is produced by a model-driven run. The passing
//! runs below are therefore built here, deterministically and offline, to
//! satisfy the committed `expected-*.json`; each invalid variant then breaks
//! exactly one expectation and must fail with its message.

use super::{
    assert_fails, assert_fails_with, assert_passes, golden, remove, set, text, Case, Copy,
};
use crate::startup::scoring::fixtures::j;
use toolkit_core::Json;

// ------------------------------------------------------------ Heroku drift

const HEROKU_INVENTORY: &str = r#"{
  "metadata": {
    "total_apps_discovered": 3,
    "discovery_sources": ["terraform", "live"],
    "confidence": "reduced"
  },
  "apps": [
    {"app_name": "acme-web", "discovery_status": "success", "heroku_generation": "cedar",
     "app_id": "a1b2c3d4-0001-4a5b-8c6d-0123456789ab"},
    {"app_name": "acme-staging", "discovery_status": "success", "heroku_generation": "cedar",
     "unmanaged_by_terraform": true},
    {"app_name": "acme-data-team", "discovery_status": "discovery_failed",
     "failure_reason": "captures failed (403)"}
  ],
  "resources": [
    {"resource_id": "formation:acme-web:web", "source": "live+terraform",
     "config": {"quantity": 2, "dyno_type": "standard-2x", "command": "npm start"}},
    {"resource_id": "formation:acme-web:release", "source": "terraform", "config": {"quantity": 0}},
    {"resource_id": "addon:acme-web:heroku-postgresql:standard-2", "source": "live+terraform",
     "config": {"data_size_gb": 42.3, "pg_version": "16.4", "connection_pooling": true,
                "monthly_price_usd": 200.0}},
    {"resource_id": "addon:acme-web:scheduler:standard", "source": "terraform", "not_found_live": true},
    {"resource_id": "addon:acme-web:heroku-redis:premium-0", "source": "live",
     "unmanaged_by_terraform": true,
     "config": {"ha_enabled": true, "encryption_in_transit": true, "redis_version": "7.2.5",
                "maxmemory_policy": "noeviction", "monthly_price_usd": 15.0}},
    {"resource_id": "addon:acme-web:papertrail:choklad", "source": "live",
     "unmanaged_by_terraform": true},
    {"resource_id": "addon:acme-staging:heroku-postgresql:essential-0", "source": "live",
     "unmanaged_by_terraform": true, "config": {"data_size_gb": 0.4}},
    {"resource_id": "formation:acme-staging:web", "source": "live", "unmanaged_by_terraform": true,
     "config": {"dyno_type": "basic", "quantity": 1}},
    {"resource_id": "config:acme-web",
     "config": {"config_var_keys": ["A", "B", "C", "D", "E", "F", "G", "H", "I"]}},
    {"resource_id": "domain:acme-web:www.acme-demo.com", "source": "live"},
    {"resource_id": "pipeline:acme", "source": "live",
     "config": {"stages": [{"stage": "staging", "app": "acme-staging"},
                           {"stage": "production", "app": "acme-web"}]}}
  ],
  "live_metadata": {
    "apps_captured": 2,
    "apps_failed": 1,
    "limitations": ["formations scaled to zero are not visible to live discovery"],
    "drift": {
      "resources_live_only": 5,
      "resources_terraform_only": 1,
      "config_conflicts": [
        {"resource_id": "formation:acme-web:web", "field": "quantity"},
        {"resource_id": "formation:acme-web:web", "field": "dyno_type"},
        {"resource_id": "addon:acme-web:heroku-postgresql:standard-2", "field": "plan"}
      ]
    }
  }
}"#;

fn heroku_drift_run() -> Copy {
    let run = Copy::empty();
    run.write("heroku-resource-inventory.json", HEROKU_INVENTORY);
    run
}

#[test]
fn heroku_drift_passes_for_a_run_that_meets_the_expectations() {
    assert_passes("heroku-drift", heroku_drift_run().path());
}

#[test]
fn heroku_drift_fails_on_each_broken_expectation() {
    let cases: [Case; 8] = [
        (
            "discovery",
            |doc| {
                set(doc, &["metadata", "total_apps_discovered"], Json::Int(2));
                set(
                    doc,
                    &["metadata", "discovery_sources"],
                    Json::Array(vec![text("live")]),
                );
                set(doc, &["metadata", "confidence"], text("high"));
            },
            "discovery_sources missing terraform",
        ),
        (
            "apps",
            |doc| {
                set(doc, &["apps", "2", "failure_reason"], text("timeout"));
                set(doc, &["apps", "0", "heroku_generation"], text("fir"));
            },
            "app acme-web heroku_generation=fir want cedar",
        ),
        (
            "app lost",
            |doc| set(doc, &["apps"], Json::Array(Vec::new())),
            "missing app acme-data-team",
        ),
        (
            "flags",
            |doc| {
                set(
                    doc,
                    &["resources", "3", "not_found_live"],
                    Json::Bool(false),
                );
                set(
                    doc,
                    &["resources", "4", "unmanaged_by_terraform"],
                    Json::Bool(false),
                );
            },
            "addon:acme-web:scheduler:standard not_found_live flag",
        ),
        (
            "config",
            |doc| {
                set(doc, &["resources", "0", "config", "quantity"], Json::Int(3));
                set(
                    doc,
                    &["resources", "8", "config", "config_var_keys"],
                    Json::Array(Vec::new()),
                );
            },
            "formation:acme-web:web config.quantity=3 want 2",
        ),
        (
            "conflicts",
            |doc| {
                set(
                    doc,
                    &["live_metadata", "drift", "config_conflicts"],
                    Json::Array(Vec::new()),
                );
                set(
                    doc,
                    &["live_metadata", "drift", "resources_terraform_only"],
                    Json::Int(0),
                );
            },
            "formation:acme-web:web missing config conflict on quantity",
        ),
        (
            "limitations",
            |doc| {
                set(
                    doc,
                    &["live_metadata", "limitations"],
                    Json::Array(Vec::new()),
                )
            },
            "limitations missing scaled-to-zero note",
        ),
        (
            "clustering fields",
            |doc| set(doc, &["resources", "9", "cluster_id"], text("c1")),
            "forbidden clustering field cluster_id",
        ),
    ];
    for (label, change, needle) in cases {
        let run = heroku_drift_run();
        run.edit("heroku-resource-inventory.json", change);
        let report = assert_fails("heroku-drift", run.path());
        assert!(report.contains(needle), "{label}: {needle}\n{report}");
    }
}

#[test]
fn heroku_drift_fails_on_config_values_and_secrets() {
    let run = heroku_drift_run();
    run.edit("heroku-resource-inventory.json", |doc| {
        set(
            doc,
            &["resources", "8", "config", "config_vars"],
            j(r#"[{"name": "STRIPE_SECRET_KEY", "value": "sk_live_abc"}]"#),
        );
        set(doc, &["resources", "10", "token"], text("Bearer abc"));
        set(doc, &["resources", "10", "STRIPE_SECRET_KEY"], text("x"));
    });
    let report = assert_fails("heroku-drift", run.path());
    for needle in [
        "env-like object with a value payload at $.resources[8].config.config_vars[0]",
        "raw config_vars at $.resources[8].config — only config_var_keys (names) allowed",
        "possible secret value: sk_live",
        "possible secret value: Bearer ",
        "config var name STRIPE_SECRET_KEY appears as a KEY (value paired)",
    ] {
        assert!(report.contains(needle), "{needle}\n{report}");
    }
}

#[test]
fn heroku_drift_fails_without_the_inventory() {
    assert_fails_with(
        "heroku-drift",
        Copy::empty().path(),
        "missing heroku-resource-inventory.json in run dir",
    );
}

// ---------------------------------------------------------------- GCP drift

const GCP_INVENTORY: &str = r#"{
  "metadata": {"discovery_sources": ["terraform", "live"], "clustering_mode": "simplified_live"},
  "resources": [
    {"address": "google_sql_database_instance.db", "source": "live+terraform",
     "config": {"tier_or_settings": {"tier": "db-custom-2-8192"}}},
    {"address": "google_cloud_run_service.orders_api", "source": "live+terraform",
     "classification": "PRIMARY", "config": {"live_type": "google_cloud_run_v2_service"}},
    {"address": "google_storage_bucket.assets", "not_found_live": true},
    {"address": "google_pubsub_topic.events"},
    {"address": "google_redis_instance.cache", "source": "live", "unmanaged_by_terraform": true,
     "classification": "PRIMARY"},
    {"address": "google_cloud_run_v2_service.web_frontend", "source": "live",
     "unmanaged_by_terraform": true, "classification": "PRIMARY"},
    {"address": "google_storage_bucket.acme_prod_uploads", "source": "live",
     "unmanaged_by_terraform": true},
    {"address": "google_compute_network.main"}
  ],
  "ai_detection": {"has_ai_workload": false, "confidence": 0},
  "live_metadata": {
    "method": "per_service",
    "project": "acme-prod",
    "capture_warnings": ["pubsub capture failed", "asset inventory unavailable"],
    "drift": {
      "resources_terraform_only": 1,
      "resources_live_only": 4,
      "config_conflicts": [{"field": "settings.tier"}]
    }
  }
}"#;

const GCP_CLUSTERS: &str = r#"{
  "clusters": [
    {"cluster_id": "networking-main", "creation_order_depth": 0,
     "primary_resources": ["google_compute_network.main"],
     "secondary_resources": ["google_sql_database_instance.db"],
     "edges": [
       {"from": "google_cloud_run_service.orders_api", "to": "google_sql_database_instance.db",
        "relationship_type": "data_dependency",
        "evidence": {"annotation": "run.googleapis.com/cloudsql-instances"}},
       {"from": "google_sql_database_instance.db", "to": "google_compute_network.main",
        "relationship_type": "network_membership"}
     ]},
    {"cluster_id": "compute-main", "creation_order_depth": 1,
     "primary_resources": ["google_cloud_run_service.orders_api",
                           "google_cloud_run_v2_service.web_frontend", "google_redis_instance.cache"],
     "secondary_resources": ["google_storage_bucket.assets", "google_pubsub_topic.events",
                             "google_storage_bucket.acme_prod_uploads"]}
  ]
}"#;

fn gcp_drift_run() -> Copy {
    let run = Copy::empty();
    run.write("gcp-resource-inventory.json", GCP_INVENTORY);
    run.write("gcp-resource-clusters.json", GCP_CLUSTERS);
    run
}

#[test]
fn gcp_drift_passes_for_a_run_that_meets_the_expectations() {
    assert_passes("gcp-drift", gcp_drift_run().path());
}

#[test]
fn gcp_drift_fails_on_each_broken_expectation() {
    let inventory_cases: [Case; 7] = [
        (
            "type alias lock",
            |doc| {
                set(doc, &["resources", "1", "not_found_live"], Json::Bool(true));
                set(
                    doc,
                    &["resources", "1", "unmanaged_by_terraform"],
                    Json::Bool(true),
                );
                set(doc, &["resources", "1", "source"], text("terraform"));
            },
            "google_cloud_run_service.orders_api must not have not_found_live",
        ),
        (
            "absence of evidence",
            |doc| set(doc, &["resources", "3", "not_found_live"], Json::Bool(true)),
            "google_pubsub_topic.events must not have not_found_live",
        ),
        (
            "missing resource",
            |doc| set(doc, &["resources", "7", "address"], text("other_kind.main")),
            "missing resource google_compute_network.main",
        ),
        (
            "metadata",
            |doc| {
                set(doc, &["metadata", "clustering_mode"], text("full"));
                set(
                    doc,
                    &["metadata", "discovery_sources"],
                    Json::Array(Vec::new()),
                );
            },
            "clustering_mode full not in ['simplified', 'simplified_live']",
        ),
        (
            "drift counts",
            |doc| {
                set(
                    doc,
                    &["live_metadata", "drift", "resources_live_only"],
                    Json::Int(1),
                );
                set(
                    doc,
                    &["live_metadata", "drift", "resources_terraform_only"],
                    Json::Int(5),
                );
                set(
                    doc,
                    &["live_metadata", "drift", "config_conflicts"],
                    Json::Array(Vec::new()),
                );
            },
            "config_conflicts missing field settings.tier",
        ),
        (
            "aws naming",
            |doc| {
                set(
                    doc,
                    &["resources", "3", "note"],
                    text("maps to aws_sqs_queue"),
                )
            },
            "possible AWS naming in discover artifact",
        ),
        (
            "env payload",
            |doc| {
                set(
                    doc,
                    &["resources", "0", "config", "env"],
                    j(r#"[{"name": "DATABASE_URL", "value": "postgres://x"}]"#),
                );
            },
            "raw 'env' key at $.resources[0].config — spec requires env_var_names (names only)",
        ),
    ];
    for (label, change, needle) in inventory_cases {
        let run = gcp_drift_run();
        run.edit("gcp-resource-inventory.json", change);
        let report = assert_fails("gcp-drift", run.path());
        assert!(report.contains(needle), "{label}: {needle}\n{report}");
    }
}

#[test]
fn gcp_drift_fails_on_cluster_coverage_edges_and_depth() {
    let run = gcp_drift_run();
    run.edit("gcp-resource-clusters.json", |doc| {
        set(doc, &["clusters", "0", "edges"], Json::Array(Vec::new()));
        set(doc, &["clusters", "0", "cluster_id"], text("storage-main"));
        set(
            doc,
            &["clusters", "1", "secondary_resources"],
            Json::Array(vec![text("google_storage_bucket.assets")]),
        );
    });
    let report = assert_fails("gcp-drift", run.path());
    assert!(
        report
            .contains("edge missing: orders_api -> google_sql_database_instance (data_dependency)"),
        "{report}"
    );
    assert!(
        report.contains("cluster coverage mismatch: ['google_pubsub_topic.events'"),
        "{report}"
    );
    assert!(
        report.contains("no networking cluster at depth 0"),
        "{report}"
    );
}

#[test]
fn gcp_drift_matches_a_live_only_name_by_suffix() {
    // Address synthesis can differ for live-only names: a single resource of
    // the same kind whose address contains the expected name is accepted.
    let run = gcp_drift_run();
    run.edit("gcp-resource-inventory.json", |doc| {
        set(
            doc,
            &["resources", "7", "address"],
            text("google_compute_network.main_vpc"),
        );
    });
    run.edit("gcp-resource-clusters.json", |doc| {
        set(
            doc,
            &["clusters", "0", "primary_resources"],
            j(r#"["google_compute_network.main_vpc"]"#),
        );
    });
    assert_passes("gcp-drift", run.path());
}

#[test]
fn gcp_drift_fails_without_its_inputs() {
    let run = gcp_drift_run();
    run.remove("gcp-resource-clusters.json");
    assert_fails_with(
        "gcp-drift",
        run.path(),
        "missing gcp-resource-inventory.json or gcp-resource-clusters.json in run dir",
    );
}

// ------------------------------------------------------------- GCP baseline

pub(super) const GCP_ESTIMATE: &str = r#"{
  "current_costs": {
    "source": "inventory_estimate",
    "accuracy": "+/-20-30% (derived from rate cards)",
    "monthly_total": 149.45,
    "breakdown": {"google_sql_database_instance.db": {"monthly": 113.68},
                  "google_redis_instance.cache": {"monthly": 35.77}},
    "baseline_note": "Derived from the inventory and rate cards: this is not a bill, and usage-based services are excluded.",
    "excluded": ["orders_api", "web_frontend", "acme_prod_uploads"]
  },
  "warnings": []
}"#;

pub(super) fn gcp_baseline_run() -> Copy {
    let run = Copy::empty();
    run.write("estimation-infra.json", GCP_ESTIMATE);
    run
}

#[test]
fn gcp_baseline_passes_for_both_artifact_shapes() {
    assert_passes("gcp-baseline", gcp_baseline_run().path());
    // The Part 1 preview file may BE the current_costs object.
    let preview = Copy::empty();
    let estimate = j(GCP_ESTIMATE);
    let costs = super::super::ledger::get(&estimate, "current_costs").clone();
    preview.write("current-costs-preview.json", &costs.to_compact_string());
    assert_passes("gcp-baseline", preview.path());
}

#[test]
fn gcp_baseline_fails_on_each_broken_expectation() {
    let cases: [Case; 6] = [
        (
            "billing claim",
            |doc| set(doc, &["current_costs", "source"], text("billing_data")),
            "claims billing_data with no billing-profile.json",
        ),
        (
            "accuracy",
            |doc| set(doc, &["current_costs", "accuracy"], text("exact")),
            "accuracy=exact",
        ),
        (
            "math",
            |doc| {
                set(doc, &["current_costs", "monthly_total"], j("500.0"));
                set(
                    doc,
                    &["current_costs", "breakdown"],
                    j(r#"{"db": {"monthly": 1.0}}"#),
                );
            },
            "no numeric total ≈ 149.45 (±0.75) in current_costs — rate-card math broken",
        ),
        (
            "caveat",
            |doc| set(doc, &["current_costs", "baseline_note"], text("   ")),
            "baseline_note missing — the derived-baseline caveat is mandatory",
        ),
        (
            "caveat words",
            |doc| {
                set(
                    doc,
                    &["current_costs", "baseline_note"],
                    text("an estimate"),
                )
            },
            "baseline_note does not mention 'not a bill'",
        ),
        (
            "excluded",
            |doc| set(doc, &["current_costs", "excluded"], Json::Array(Vec::new())),
            "excluded resource 'orders_api' not surfaced in warnings",
        ),
    ];
    for (label, change, needle) in cases {
        let run = gcp_baseline_run();
        run.edit("estimation-infra.json", change);
        let report = assert_fails("gcp-baseline", run.path());
        assert!(report.contains(needle), "{label}: {needle}\n{report}");
    }
    let run = gcp_baseline_run();
    run.edit("estimation-infra.json", |doc| {
        remove(doc, &["current_costs"])
    });
    assert_fails("gcp-baseline", run.path());
}

// -------------------------------------------------------- Heroku estimate

/// A passing estimate for the seed design of the scenario: one line per
/// designed service, priced so every identity holds.
pub(super) fn heroku_estimate_run() -> Copy {
    let run = Copy::empty();
    let design_text = std::fs::read_to_string(
        golden("heroku-live-capture", "seed-estimate").join("aws-design.json"),
    )
    .expect("the seed design");
    run.write("aws-design.json", &design_text);
    let design = j(&design_text);
    let services = super::super::ledger::list(&design, "services");
    assert!(!services.is_empty(), "the seed design has services");
    let mut breakdown = Vec::new();
    for service in services {
        let sid = super::super::ledger::get(service, "service_id")
            .as_str()
            .expect("service_id")
            .to_string();
        breakdown.push((sid, Json::object(vec![("balanced", j("10.0"))])));
    }
    let balanced = 10.0 * f64::from(u32::try_from(services.len()).expect("few services"));
    let option = |aws: f64| {
        let diff = aws - 352.0;
        Json::object(vec![
            ("aws_monthly", Json::Float(aws)),
            ("monthly_difference", Json::Float(diff)),
            ("annual_difference", Json::Float(diff * 12.0)),
            ("percent_change", Json::Float(diff / 352.0 * 100.0)),
        ])
    };
    let estimate = Json::object(vec![
        (
            "current_costs",
            j(r#"{"source": "live_prices_plus_cache", "monthly": 352.0,
                  "baseline_note": "Derived from live add-on prices and dyno rates, not an invoice; usage not metered."}"#),
        ),
        (
            "cost_comparison",
            Json::object(vec![
                ("heroku_monthly_baseline", Json::Float(352.0)),
                ("baseline_source", text("live_prices_plus_cache")),
                ("option_a_premium", option(balanced + 20.0)),
                ("option_b_balanced", option(balanced)),
                ("option_c_optimized", option(balanced - 20.0)),
            ]),
        ),
        (
            "migration_cost_considerations",
            j(
                r#"{"baseline_available": true, "baseline_source": "live_prices_plus_cache",
                  "categories": [{"name": "dual-run"}]}"#,
            ),
        ),
        (
            "projected_costs",
            Json::Object(vec![
                ("aws_monthly_balanced".to_string(), Json::Float(balanced)),
                ("breakdown".to_string(), Json::Object(breakdown)),
            ]),
        ),
        ("warnings", Json::Array(Vec::new())),
    ]);
    run.write("estimation-infra.json", &estimate.to_compact_string());
    run
}

#[test]
fn heroku_estimate_passes_for_a_run_that_meets_the_expectations() {
    assert_passes("heroku-estimate", heroku_estimate_run().path());
}

#[test]
fn heroku_estimate_fails_on_each_broken_expectation() {
    let cases: [Case; 10] = [
        (
            "fabricated billing",
            |doc| {
                set(doc, &["current_costs", "source"], text("billing_data"));
                set(doc, &["billing_profile"], j(r#"{"plan": "x"}"#));
            },
            "estimation-infra.json fabricated a billing_profile",
        ),
        (
            "baseline total",
            |doc| set(doc, &["current_costs", "monthly"], j("300.0")),
            "current_costs has no numeric total == 352 (exact add-on + cache dyno math)",
        ),
        (
            "caveat",
            |doc| set(doc, &["current_costs", "baseline_note"], text("")),
            "baseline_note missing/empty — the derived-baseline caveat is mandatory",
        ),
        (
            "no comparison",
            |doc| remove(doc, &["cost_comparison"]),
            "cost_comparison absent — comparison must run for ANY baseline source",
        ),
        (
            "option arithmetic",
            |doc| {
                set(
                    doc,
                    &["cost_comparison", "option_b_balanced", "monthly_difference"],
                    j("1.0"),
                );
                set(
                    doc,
                    &["cost_comparison", "option_a_premium", "annual_difference"],
                    j("1.0"),
                );
            },
            "cost_comparison.option_b_balanced monthly_difference 1 != ",
        ),
        (
            "missing option field",
            |doc| {
                remove(
                    doc,
                    &["cost_comparison", "option_c_optimized", "percent_change"],
                )
            },
            "cost_comparison.option_c_optimized.percent_change missing",
        ),
        (
            "migration considerations",
            |doc| {
                set(
                    doc,
                    &["migration_cost_considerations", "baseline_available"],
                    Json::Bool(false),
                );
                set(
                    doc,
                    &["migration_cost_considerations", "categories"],
                    Json::Array(Vec::new()),
                );
            },
            "migration_cost_considerations.baseline_available=False",
        ),
        (
            "property 16",
            |doc| {
                set(
                    doc,
                    &["projected_costs", "aws_monthly_balanced"],
                    j("9999.0"),
                )
            },
            "(Property-16)",
        ),
        (
            "unpriced warning",
            |doc| {
                set(
                    doc,
                    &["warnings"],
                    j(r#"[{"code": "unpriced_heroku_addon"}]"#),
                )
            },
            "warnings contain 'unpriced_heroku' — everything in this scenario is priceable",
        ),
        (
            "invoice leak",
            |doc| set(doc, &["billing_period"], text("2026-07")),
            "billing_period present — no invoice data exists in this scenario",
        ),
    ];
    for (label, change, needle) in cases {
        let run = heroku_estimate_run();
        run.edit("estimation-infra.json", change);
        let report = assert_fails("heroku-estimate", run.path());
        assert!(report.contains(needle), "{label}: {needle}\n{report}");
    }
}

#[test]
fn heroku_estimate_fails_when_a_designed_service_is_neither_priced_nor_warned() {
    let run = heroku_estimate_run();
    run.edit("aws-design.json", |doc| {
        let service =
            j(r#"{"service_id": "unpriced-widget-7", "aws_service": "Widget Service 7"}"#);
        let Json::Array(mut services) = super::super::ledger::get(doc, "services").clone() else {
            panic!("services");
        };
        services.push(service);
        set(doc, &["services"], Json::Array(services));
    });
    assert_fails_with(
        "heroku-estimate",
        run.path(),
        "design service unpriced-widget-7 neither in the cost breakdown nor warned as unpriced",
    );
    let run = heroku_estimate_run();
    run.remove("estimation-infra.json");
    assert_fails_with(
        "heroku-estimate",
        run.path(),
        "missing estimation-infra.json in run dir",
    );
}

#[test]
fn heroku_estimate_fails_on_a_breakdown_without_cost_fields() {
    let run = heroku_estimate_run();
    run.edit("estimation-infra.json", |doc| {
        set(
            doc,
            &["projected_costs", "breakdown"],
            j(r#"{"x": {"note": "no figure"}}"#),
        );
    });
    assert_fails_with(
        "heroku-estimate",
        run.path(),
        "breakdown entries lack a recognizable balanced/monthly cost field",
    );
}
