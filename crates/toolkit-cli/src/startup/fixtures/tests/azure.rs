//! The azure-to-aws programs (Discover, Clarify x3, Design, Estimate) against
//! the committed golden trees of `azure-iac-terraform` and against
//! deliberately invalid copies of them.

use super::{assert_fails, assert_fails_with, assert_passes, golden, remove, set, text, Copy};
use crate::startup::scoring::fixtures::j;
use toolkit_core::Json;

const SET: &str = "azure-iac-terraform";

fn tree(name: &str) -> std::path::PathBuf {
    golden(SET, name)
}

#[test]
fn every_committed_azure_golden_tree_passes_its_program() {
    assert_passes("azure-discover", &tree("after-discover"));
    assert_passes("azure-clarify", &tree("after-clarify"));
    assert_passes("azure-clarify-complete", &tree("after-clarify-complete"));
    assert_passes("azure-clarify-fast-path", &tree("after-clarify-fast-path"));
    assert_passes("azure-design", &tree("after-design"));
    assert_passes("azure-estimate", &tree("after-estimate"));
}

#[test]
fn the_clarify_expectation_sets_are_not_interchangeable() {
    // The BLOCKED golden must not satisfy the COMPLETING set and vice versa:
    // the sets are what pin the gate branch against the completing branch.
    assert_fails("azure-clarify-complete", &tree("after-clarify"));
    assert_fails("azure-clarify", &tree("after-clarify-complete"));
    assert_fails("azure-clarify-fast-path", &tree("after-clarify"));
}

// ------------------------------------------------------------------ Discover

fn discover_fails(change: impl FnOnce(&mut Json), needle: &str) {
    let run = Copy::of(SET, "after-discover");
    run.edit("azure-resource-inventory.json", change);
    assert_fails_with("azure-discover", run.path(), needle);
}

#[test]
fn discover_fails_on_a_wrong_fast_path_verdict() {
    discover_fails(
        |doc| {
            set(
                doc,
                &["metadata", "clarify_fast_path", "eligible"],
                Json::Bool(true),
            )
        },
        "metadata.clarify_fast_path.eligible is True, expected False",
    );
    discover_fails(
        |doc| remove(doc, &["metadata", "clarify_fast_path"]),
        "metadata.clarify_fast_path is missing",
    );
    discover_fails(
        |doc| {
            set(
                doc,
                &["metadata", "clarify_fast_path", "reasons_ineligible"],
                Json::Array(Vec::new()),
            )
        },
        "reasons_ineligible must be non-empty iff eligible is false",
    );
    discover_fails(
        |doc| {
            set(
                doc,
                &["metadata", "clarify_fast_path", "reasons_ineligible"],
                Json::Array(vec![text("vibes")]),
            );
        },
        "outside the closed vocabulary",
    );
}

#[test]
fn discover_fails_on_metadata_and_counts() {
    discover_fails(
        |doc| {
            set(
                doc,
                &["metadata", "discovery_sources"],
                Json::Array(vec![text("live")]),
            )
        },
        "metadata.discovery_sources is ['live']",
    );
    discover_fails(
        |doc| {
            set(
                doc,
                &["iac_metadata", "subscription_id_source"],
                text("guid"),
            )
        },
        "iac_metadata.subscription_id_source is 'guid'",
    );
    discover_fails(
        |doc| set(doc, &["resources"], Json::Array(Vec::new())),
        "inventory resources[] is missing or empty",
    );
}

#[test]
fn discover_fails_on_type_and_identity_drift() {
    discover_fails(
        |doc| {
            set(
                doc,
                &["resources", "0", "azure_type"],
                text("azurerm_resource_group"),
            )
        },
        "is a Terraform type — it was never translated",
    );
    discover_fails(
        |doc| set(doc, &["resources", "0", "azure_id"], text("not-an-arm-id")),
        "a resource group's own azure_id must be",
    );
    discover_fails(
        |doc| set(doc, &["resources", "1", "config", "tf_address"], j("null")),
        "no config.tf_address",
    );
}

#[test]
fn discover_fails_when_a_secret_leaks_into_the_inventory() {
    discover_fails(
        |doc| {
            set(
                doc,
                &["resources", "0", "config", "connection_string"],
                text("Server=x;Password=hunter2"),
            )
        },
        "config carries 'connection_string'",
    );
}

#[test]
fn discover_fails_without_its_inventory_or_clusters() {
    let run = Copy::of(SET, "after-discover");
    run.remove("azure-resource-clusters.json");
    assert_fails_with(
        "azure-discover",
        run.path(),
        "missing azure-resource-clusters.json",
    );
    run.remove("azure-resource-inventory.json");
    assert_fails_with(
        "azure-discover",
        run.path(),
        "missing azure-resource-inventory.json",
    );
}

#[test]
fn discover_fails_when_clustering_drops_a_resource() {
    let run = Copy::of(SET, "after-discover");
    run.edit("azure-resource-clusters.json", |doc| {
        set(doc, &["clusters"], Json::Array(Vec::new()));
        set(doc, &["unclustered"], Json::Array(Vec::new()));
    });
    let report = assert_fails("azure-discover", run.path());
    assert!(report.contains("cluster(s), expected"), "{report}");
    assert!(
        report.contains("are in no cluster and not in unclustered[]"),
        "{report}"
    );
}

// ------------------------------------------------------------------- Clarify

#[test]
fn clarify_fails_when_the_essential_gate_does_not_gate() {
    let run = Copy::of(SET, "after-clarify");
    run.edit("preferences.json", |doc| {
        set(doc, &["clarify_status"], text("COMPLETE"))
    });
    let report = assert_fails("azure-clarify", run.path());
    assert!(
        report.contains("clarify_status is 'COMPLETE', expected 'BLOCKED_ON_ESSENTIAL'"),
        "{report}"
    );
}

#[test]
fn clarify_fails_when_a_default_is_promoted_to_detected() {
    let run = Copy::of(SET, "after-clarify");
    let prefs = run.read("preferences.json");
    // Any PROPOSED row that carries a default: make it DETECTED without a source.
    let mut doc = j(&prefs);
    let promoted = promote_first_proposed(&mut doc);
    assert!(promoted, "the golden has a PROPOSED row with a default");
    run.write("preferences.json", &doc.to_compact_string());
    let report = assert_fails("azure-clarify", run.path());
    assert!(
        report.contains("are DETECTED with value == default and carry no `source`"),
        "{report}"
    );
}

/// Turns the first `PROPOSED` row that has a non-null `default` into a
/// `DETECTED` one with `value == default` and no `source`.
fn promote_first_proposed(node: &mut Json) -> bool {
    match node {
        Json::Object(members) => {
            let proposed = members.iter().any(|(key, value)| {
                key == "disposition" && matches!(value, Json::String(s) if s == "PROPOSED")
            });
            let default = members
                .iter()
                .find(|(key, _)| key == "default")
                .map(|(_, value)| value.clone());
            if proposed && default.as_ref().is_some_and(|d| !matches!(d, Json::Null)) {
                for (key, value) in members.iter_mut() {
                    match key.as_str() {
                        "disposition" => *value = text("DETECTED"),
                        "value" => *value = default.clone().expect("default"),
                        _ => {}
                    }
                }
                members.retain(|(key, _)| key != "source" && key != "forced_by");
                return true;
            }
            members
                .iter_mut()
                .any(|(_, value)| promote_first_proposed(value))
        }
        Json::Array(items) => items.iter_mut().any(promote_first_proposed),
        _ => false,
    }
}

#[test]
fn clarify_fails_on_a_deleted_row_a_wrong_mode_and_a_secret() {
    let run = Copy::of(SET, "after-clarify");
    run.edit("preferences.json", |doc| {
        remove(doc, &["data", "availability"]);
        set(doc, &["metadata", "clarify_mode"], text("fast_path"));
        set(
            doc,
            &["metadata", "questions_defaulted"],
            text("not a list"),
        );
        set(doc, &["leak"], text("password=hunter2"));
    });
    let report = assert_fails("azure-clarify", run.path());
    assert!(
        report.contains("'data.availability': no row at all."),
        "{report}"
    );
    assert!(
        report.contains("metadata.clarify_mode is 'fast_path', expected"),
        "{report}"
    );
    assert!(
        report.contains("metadata.questions_defaulted is 'not a list', expected a list"),
        "{report}"
    );
}

#[test]
fn clarify_complete_fails_when_the_conflicting_answer_is_substituted() {
    let run = Copy::of(SET, "after-clarify-complete");
    run.edit("preferences.json", |doc| {
        set(doc, &["clarify_status"], text("BLOCKED_ON_ESSENTIAL"));
    });
    let report = assert_fails("azure-clarify-complete", run.path());
    assert!(
        report.contains("clarify_status is 'BLOCKED_ON_ESSENTIAL'"),
        "{report}"
    );
}

#[test]
fn clarify_fast_path_fails_on_provenance_list_defects() {
    // A deferred row listed in BOTH lists, and a defaulted key that resolves to
    // no row.
    let run = Copy::of(SET, "after-clarify-fast-path");
    run.edit("preferences.json", |doc| {
        set(
            doc,
            &["metadata", "questions_defaulted"],
            j(r#"["data.db_cutover", "data.no_such_row", "app_service_plans[7].isolation_split"]"#),
        );
    });
    let report = assert_fails("azure-clarify-fast-path", run.path());
    assert!(
        report.contains(
            "appear in BOTH metadata.questions_defaulted and metadata.deferred_to_generate"
        ),
        "{report}"
    );
    assert!(
        report.contains("is in metadata.questions_defaulted but resolves to no row"),
        "{report}"
    );
}

#[test]
fn clarify_fast_path_fails_on_the_db_cutover_size_rule() {
    // A measured 150 GiB database must default to DMS, not dump_restore.
    let run = Copy::of(SET, "after-clarify-fast-path");
    run.edit("azure-resource-inventory.json", |doc| {
        let Json::Array(resources) = super::super::ledger::get(doc, "resources").clone() else {
            panic!("resources");
        };
        let at = resources
            .iter()
            .position(|r| {
                super::super::ledger::eq_str(
                    super::super::ledger::get(r, "azure_type"),
                    "Microsoft.DBforPostgreSQL/flexibleServers",
                )
            })
            .expect("a Postgres server")
            .to_string();
        set(
            doc,
            &["resources", &at, "config", "storage_mb"],
            Json::Int(153_600),
        );
    });
    let report = assert_fails("azure-clarify-fast-path", run.path());
    assert!(
        report.contains("data.db_cutover.default is 'dump_restore', expected 'dms'"),
        "{report}"
    );
    assert!(
        report.contains("data.db_cutover.size_coverage is 'unknown', expected 'complete'"),
        "{report}"
    );
}

#[test]
fn clarify_prefers_the_run_inventory_over_the_golden_one() {
    // With an inventory in the run directory the golden fallback note is not
    // printed (the program notes the fallback only when it is used).
    let run = Copy::of(SET, "after-clarify-fast-path");
    let (status, stdout, _) = super::check("azure-clarify-fast-path", run.path(), &[]);
    assert_eq!(status, 0, "{stdout}");
    assert!(!stdout.contains("using the fixture's golden"), "{stdout}");
    let blocked = Copy::of(SET, "after-clarify");
    let (status, stdout, _) = super::check("azure-clarify", blocked.path(), &[]);
    assert_eq!(status, 0, "{stdout}");
    assert!(
        stdout.contains("note: no inventory in the run dir; using the fixture's golden at"),
        "{stdout}"
    );
    assert!(stdout.contains("note: essential gate: "), "{stdout}");
}

// -------------------------------------------------------------------- Design

fn design_fails(change: impl FnOnce(&mut Json), needle: &str) {
    let run = Copy::of(SET, "after-design");
    run.edit("aws-design.json", change);
    assert_fails_with("azure-design", run.path(), needle);
}

#[test]
fn design_fails_when_services_are_dropped_or_mislabelled() {
    design_fails(
        |doc| set(doc, &["services"], Json::Array(Vec::new())),
        ": no services[] entry. It matches a Direct Mappings row",
    );
    design_fails(
        |doc| {
            set(
                doc,
                &["services", "0", "azure_type"],
                text("Microsoft.Fake/things"),
            )
        },
        "has NO direct_mappings row",
    );
    design_fails(
        |doc| set(doc, &["services", "0", "routing_provenance"], text("vibes")),
        "routing_provenance is 'vibes'",
    );
}

#[test]
fn design_fails_on_parked_rubrics_and_stale_halts() {
    design_fails(
        |doc| {
            set(
                doc,
                &["pending_rubric"],
                j(
                    r#"[{"azure_type": "Microsoft.Compute/virtualMachines", "ref_file": "compute.md"}]"#,
                ),
            );
        },
        "pending_rubric[] has 1 entr(ies), expected 0",
    );
    design_fails(
        |doc| {
            set(
                doc,
                &["halt"],
                j(
                    r#"{"blocking": [{"kind": "untranslated_terraform_type", "identifier": "azurerm_iothub.hub"}]}"#,
                ),
            );
        },
        "halt.blocking carries a 'untranslated_terraform_type' entry",
    );
}

#[test]
fn design_fails_on_missing_warnings_and_forbidden_content() {
    design_fails(
        |doc| set(doc, &["warnings"], Json::Array(Vec::new())),
        "is a Skip Mapping but has no warnings[] entry",
    );
    design_fails(
        |doc| set(doc, &["leak"], text("a note about App Runner")),
        "appears in aws-design.json",
    );
}

#[test]
fn design_fails_without_its_design_and_stays_offline_for_the_inventory() {
    let run = Copy::of(SET, "after-design");
    run.remove("aws-design.json");
    assert_fails_with("azure-design", run.path(), "missing aws-design.json at");
}

// ------------------------------------------------------------------ Estimate

fn estimate_fails(change: impl FnOnce(&mut Json), needle: &str) {
    let run = Copy::of(SET, "after-estimate");
    run.edit("estimation-infra.json", change);
    let report = assert_fails("azure-estimate", run.path());
    assert!(report.starts_with("FAIL — "), "{report}");
    assert!(report.contains(needle), "{needle}\n{report}");
}

#[test]
fn estimate_fails_on_totals_that_do_not_reconcile() {
    estimate_fails(
        |doc| {
            set(
                doc,
                &["projected_costs", "right_sized", "monthly"],
                j("1.0"),
            )
        },
        "its non-excluded lines sum to",
    );
    estimate_fails(
        |doc| set(doc, &["projected_costs", "aws_monthly_premium"], j("1.0")),
        "is below Balanced",
    );
    estimate_fails(
        |doc| {
            set(
                doc,
                &["projected_costs", "aws_monthly_optimized"],
                j("99999.0"),
            )
        },
        "is above Balanced",
    );
}

#[test]
fn estimate_fails_on_the_contract_not_the_dollars() {
    estimate_fails(
        |doc| set(doc, &["recommendation", "outcome"], text("go")),
        "recommendation.outcome is 'go'",
    );
    estimate_fails(
        |doc| {
            set(
                doc,
                &["pricing_source", "region_mismatch"],
                Json::Bool(false),
            )
        },
        "pricing_source.region_mismatch must be true",
    );
    estimate_fails(
        |doc| set(doc, &["complexity_tier"], text("trivial")),
        "complexity_tier is 'trivial'",
    );
    estimate_fails(
        |doc| {
            set(
                doc,
                &["projected_costs", "breakdown"],
                Json::Array(Vec::new()),
            )
        },
        "projected_costs.breakdown is list",
    );
    estimate_fails(
        |doc| set(doc, &["roi_analysis"], j(r#"{"engineer-weeks": "3 FTE"}"#)),
        "appears in a cost section",
    );
}

#[test]
fn estimate_fails_on_a_wrong_phase_status_and_a_missing_artifact() {
    let run = Copy::of(SET, "after-estimate");
    run.edit(".phase-status.json", |doc| {
        set(doc, &["run_mode"], text("decide_and_execute"));
        set(doc, &["phases", "generate"], text("completed"));
    });
    let report = assert_fails("azure-estimate", run.path());
    assert!(
        report.contains("run_mode is 'decide_and_execute'"),
        "{report}"
    );
    run.remove("estimation-infra.json");
    let report = assert_fails("azure-estimate", run.path());
    assert!(report.contains("missing estimation-infra.json"), "{report}");
}

/// The tiered tolerance is data: a rate refresh that moves a line out of its
/// recorded band is a failure naming the band, not a silent pass.
#[test]
fn estimate_reports_a_line_outside_its_recorded_band() {
    let run = Copy::of(SET, "after-estimate");
    let first_priced = {
        let estimate = j(&run.read("estimation-infra.json"));
        let breakdown =
            super::super::ledger::path(&estimate, &["projected_costs", "breakdown"]).clone();
        super::super::ledger::members(&breakdown)
            .iter()
            .find(|(key, value)| {
                key != "total"
                    && !key.starts_with('_')
                    && super::super::ledger::number(super::super::ledger::get(
                        value,
                        "right_sized_monthly",
                    ))
                    .is_some_and(|n| n > 100.0)
            })
            .map(|(key, _)| key.clone())
            .expect("a line above 100 USD")
    };
    run.edit("estimation-infra.json", |doc| {
        set(
            doc,
            &[
                "projected_costs",
                "breakdown",
                &first_priced,
                "right_sized_monthly",
            ],
            j("1.0"),
        );
    });
    let report = assert_fails("azure-estimate", run.path());
    assert!(report.contains("is outside the recorded band"), "{report}");
    assert!(report.contains("do not widen it to pass"), "{report}");
}
