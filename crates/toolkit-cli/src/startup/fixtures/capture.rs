//! The live-capture asserters: Heroku and GCP Discover drift (scenario B),
//! the Heroku derived-baseline estimate (scenario C) and the GCP derived
//! baseline (`heroku-live-capture` and `gcp-live-capture`
//! `check_expected_*.py`).
//!
//! Each compares a replay run with the `expected-*.json` of its fixture and
//! enforces the secret boundary: environment and config-variable data may
//! appear only as name lists, never as objects or keys paired with a value.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::Json;

use super::ledger::{
    dumps, eq, eq_str, get, isclose, items, list, number, number_or_bool, objs, path, ps, r2, rl,
    rp, strip, truthy, type_name, NULL,
};
use super::{Context, Flow};

/// `any(find_number_near(v, ...) for v in node.values())` and friends: whether
/// a number within `tol` of `target` appears anywhere under `node`.
fn find_number_near(node: &Json, target: f64, tol: f64) -> bool {
    match node {
        Json::Object(members) => members
            .iter()
            .any(|(_, value)| find_number_near(value, target, tol)),
        Json::Array(values) => values
            .iter()
            .any(|value| find_number_near(value, target, tol)),
        other => number(other).is_some_and(|n| isclose(n, target, tol)),
    }
}

/// The secret-hygiene walk: no object may pair a `name` with a `value` or
/// `valueFrom` payload, and `forbidden_key` (raw `env` or `config_vars`) may
/// not appear at all.
fn walk(
    context: &Context<'_>,
    node: &Json,
    at: &str,
    forbidden_key: &str,
    shown_as: &str,
    why: &str,
) {
    let l = context.ledger;
    match node {
        Json::Object(members) => {
            if node.get("name").is_some()
                && (node.get("value").is_some() || node.get("valueFrom").is_some())
            {
                l.fail(format!("env-like object with a value payload at {at}"));
            }
            for (key, value) in members {
                let here = format!("{at}.{key}");
                if key == forbidden_key {
                    l.fail(format!("raw {shown_as} at {at} — {why}"));
                }
                walk(context, value, &here, forbidden_key, shown_as, why);
            }
        }
        Json::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                walk(
                    context,
                    value,
                    &format!("{at}[{index}]"),
                    forbidden_key,
                    shown_as,
                    why,
                );
            }
        }
        _ => {}
    }
}

/// `a.get(flag) is True`.
fn is_true(value: &Json) -> bool {
    matches!(value, Json::Bool(true))
}

/// `value >= min` for a possibly absent figure (default `default`).
fn at_least(value: &Json, default: f64, min: f64) -> bool {
    number_or_bool(value).unwrap_or(default) >= min
}

fn map_by<'a>(values: &'a Json, key: &str) -> BTreeMap<String, &'a Json> {
    objs(values)
        .filter_map(|item| get(item, key).as_str().map(|name| (name.to_string(), item)))
        .collect()
}

fn conflict_fields(inv: &Json) -> Vec<&Json> {
    items(path(inv, &["live_metadata", "drift", "config_conflicts"]))
        .iter()
        .map(|conflict| get(conflict, "field"))
        .collect()
}

fn in_list(haystack: &Json, needle: &Json) -> bool {
    items(haystack).iter().any(|item| eq(item, needle))
}

// ------------------------------------------------------------ Heroku drift

pub(super) fn heroku_drift(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    if !context.exists(&context.run, "heroku-resource-inventory.json") {
        l.fail("missing heroku-resource-inventory.json in run dir");
        return None;
    }
    let (_, inv) = context.json_in_dir_with_text(&context.run, "heroku-resource-inventory.json")?;
    let exp_doc = context.expectation("expected-drift.json")?;
    let exp = context.spec(&exp_doc, "expected-drift.json");

    // Metadata
    let meta = get(&inv, "metadata");
    let exp_meta = exp.at("metadata");
    l.check(
        eq(
            get(meta, "total_apps_discovered"),
            exp_meta.at("total_apps_discovered").json(),
        ),
        "total_apps_discovered",
    );
    for source in exp_meta.at("discovery_sources_must_include").arr() {
        l.check(
            in_list(get(meta, "discovery_sources"), source.json()),
            format!("discovery_sources missing {}", ps(source.json())),
        );
    }
    l.check(
        eq(get(meta, "confidence"), exp_meta.at("confidence").json()),
        format!("confidence={}", ps(get(meta, "confidence"))),
    );

    // Apps
    let apps = map_by(get(&inv, "apps"), "app_name");
    for (name, e) in exp.at("apps").entries() {
        let Some(app) = apps.get(name) else {
            l.fail(format!("missing app {name}"));
            continue;
        };
        for key in ["discovery_status", "heroku_generation", "app_id"] {
            if e.has(key) {
                l.check(
                    eq(get(app, key), e.at(key).json()),
                    format!(
                        "app {name} {key}={} want {}",
                        ps(get(app, key)),
                        ps(e.at(key).json())
                    ),
                );
            }
        }
        if e.has("failure_reason_contains") {
            let reason = get(app, "failure_reason");
            let reason = if truthy(reason) {
                reason.as_str().unwrap_or("")
            } else {
                ""
            };
            l.check(
                reason.contains(e.at("failure_reason_contains").str()),
                format!("app {name} failure_reason"),
            );
        }
    }

    // Resources
    let resources = map_by(get(&inv, "resources"), "resource_id");
    for (rid, e) in exp.at("merged_resources").entries() {
        let Some(r) = resources.get(rid) else {
            l.fail(format!("missing resource {rid}"));
            continue;
        };
        if e.has("source") {
            l.check(
                eq(get(r, "source"), e.at("source").json()),
                format!(
                    "{rid} source={} want {}",
                    ps(get(r, "source")),
                    ps(e.at("source").json())
                ),
            );
        }
        if e.get("unmanaged_by_terraform").truthy() {
            l.check(
                is_true(get(r, "unmanaged_by_terraform")),
                format!("{rid} unmanaged flag"),
            );
        }
        if e.get("not_found_live").truthy() {
            l.check(
                is_true(get(r, "not_found_live")),
                format!("{rid} not_found_live flag"),
            );
        }
        for flag in e.get("must_not_have").arr_or_empty() {
            l.check(
                !is_true(get(r, flag.str())),
                format!("{rid} must not have {}", flag.str()),
            );
        }
        for (key, want) in e.get("config").entries_or_empty() {
            let config = get(r, "config");
            match key {
                "config_var_keys_count" => {
                    let count = list(config, "config_var_keys").len();
                    l.check(
                        eq(
                            &Json::Int(i64::try_from(count).unwrap_or(i64::MAX)),
                            want.json(),
                        ),
                        format!("{rid} key count"),
                    );
                }
                "stages" => l.check(
                    eq(get(config, "stages"), want.json()),
                    format!("{rid} stages"),
                ),
                _ => l.check(
                    eq(get(config, key), want.json()),
                    format!(
                        "{rid} config.{key}={} want {}",
                        ps(get(config, key)),
                        ps(want.json())
                    ),
                ),
            }
        }
        if e.has("expected_config_conflicts") {
            let fields: Vec<&Json> =
                items(path(&inv, &["live_metadata", "drift", "config_conflicts"]))
                    .iter()
                    .filter(|conflict| eq(get(conflict, "resource_id"), &Json::string(rid)))
                    .map(|conflict| get(conflict, "field"))
                    .collect();
            for field in e.at("expected_config_conflicts").arr() {
                l.check(
                    fields.iter().any(|candidate| eq(candidate, field.json())),
                    format!("{rid} missing config conflict on {}", ps(field.json())),
                );
            }
        }
    }

    // live_metadata + drift
    let lm = get(&inv, "live_metadata");
    let exp_lm = exp.at("live_metadata");
    l.check(
        eq(get(lm, "apps_captured"), exp_lm.at("apps_captured").json()),
        "apps_captured",
    );
    l.check(
        eq(get(lm, "apps_failed"), exp_lm.at("apps_failed").json()),
        "apps_failed",
    );
    let limitations = dumps(&Json::Array(list(lm, "limitations").to_vec()));
    l.check(
        limitations.contains(exp_lm.at("limitations_must_include").str()),
        "limitations missing scaled-to-zero note",
    );
    let drift = get(lm, "drift");
    let exp_drift = exp_lm.at("drift");
    l.check(
        at_least(
            get(drift, "resources_live_only"),
            0.0,
            exp_drift.at("resources_live_only_min").f64(),
        ),
        format!(
            "resources_live_only={}",
            ps(get(drift, "resources_live_only"))
        ),
    );
    l.check(
        eq(
            get(drift, "resources_terraform_only"),
            exp_drift.at("resources_terraform_only").json(),
        ),
        format!(
            "resources_terraform_only={}",
            ps(get(drift, "resources_terraform_only"))
        ),
    );
    let mut got: Vec<String> = Vec::new();
    for field in conflict_fields(&inv) {
        if let Some(name) = field.as_str() {
            got.push(name.to_string());
        } else {
            l.fail(format!(
                "config_conflicts entry has a non-string field: {}",
                rp(field)
            ));
        }
    }
    got.sort();
    let mut want: Vec<String> = exp_drift
        .at("config_conflicts_expected_fields")
        .strs()
        .into_iter()
        .map(str::to_string)
        .collect();
    want.sort();
    l.check(got == want, format!("conflict fields {}", rl(&got)));

    // Must-not-exist / secret hygiene
    let doc = dumps(&inv);
    l.check(
        !doc.contains("domain:acme-web:acme-web-1a2b3c4d5e6f"),
        "default herokuapp domain leaked as resource",
    );
    for bad in [
        "cluster_id",
        "creation_order_depth",
        "must_migrate_together",
        "\"edges\"",
        "\"dependencies\"",
    ] {
        l.check(
            !doc.contains(bad),
            format!("forbidden clustering field {bad}"),
        );
    }
    for bad in ["sk_live", "postgres://", "rediss://", "AKIA", "Bearer "] {
        l.check(!doc.contains(bad), format!("possible secret value: {bad}"));
    }
    walk(
        context,
        &inv,
        "$",
        "config_vars",
        "config_vars",
        "only config_var_keys (names) allowed",
    );
    for key_name in ["STRIPE_SECRET_KEY", "DATABASE_URL", "SESSION_SECRET"] {
        l.check(
            !doc.contains(&format!("\"{key_name}\": ")),
            format!("config var name {key_name} appears as a KEY (value paired)"),
        );
    }
    Some(())
}

// --------------------------------------------------------------- GCP drift

pub(super) fn gcp_drift(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let run = &context.run;
    if !context.exists(run, "gcp-resource-inventory.json")
        || !context.exists(run, "gcp-resource-clusters.json")
    {
        l.fail("missing gcp-resource-inventory.json or gcp-resource-clusters.json in run dir");
        return None;
    }
    let inv = context.json_in_dir(run, "gcp-resource-inventory.json")?;
    let clusters = context.json_in_dir(run, "gcp-resource-clusters.json")?;
    let exp_doc = context.expectation("expected-drift.json")?;
    let exp = context.spec(&exp_doc, "expected-drift.json");

    // Metadata
    let meta = get(&inv, "metadata");
    let exp_meta = exp.at("metadata");
    for source in exp_meta.at("discovery_sources_must_include").arr() {
        l.check(
            in_list(get(meta, "discovery_sources"), source.json()),
            format!("discovery_sources missing {}", ps(source.json())),
        );
    }
    let modes = exp_meta.at("clustering_mode_one_of");
    l.check(
        in_list(modes.json(), get(meta, "clustering_mode")),
        format!(
            "clustering_mode {} not in {}",
            ps(get(meta, "clustering_mode")),
            rp(modes.json())
        ),
    );

    // Resources
    let res = map_by(get(&inv, "resources"), "address");
    for (addr, e) in exp.at("resources").entries() {
        let found = res.get(addr).copied().or_else(|| {
            // Address synthesis can differ for live-only names; try a
            // name-suffix match.
            let mut parts = addr.split('.');
            let (kind, name) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let candidates: Vec<&Json> = res
                .iter()
                .filter(|(key, _)| key.split('.').next() == Some(kind) && key.contains(name))
                .map(|(_, value)| *value)
                .collect();
            (candidates.len() == 1).then(|| candidates[0])
        });
        let Some(r) = found else {
            l.fail(format!("missing resource {addr}"));
            continue;
        };
        if e.has("source") {
            l.check(
                eq(get(r, "source"), e.at("source").json()),
                format!(
                    "{addr} source={} want {}",
                    ps(get(r, "source")),
                    ps(e.at("source").json())
                ),
            );
        }
        if e.has("classification") {
            l.check(
                eq(get(r, "classification"), e.at("classification").json()),
                format!("{addr} classification"),
            );
        }
        if e.get("not_found_live").truthy() {
            l.check(
                is_true(get(r, "not_found_live")),
                format!("{addr} not_found_live missing"),
            );
        }
        if e.get("unmanaged_by_terraform").truthy() {
            l.check(
                is_true(get(r, "unmanaged_by_terraform")),
                format!("{addr} unmanaged_by_terraform missing"),
            );
        }
        for flag in e.get("must_not_have").arr_or_empty() {
            l.check(
                !is_true(get(r, flag.str())),
                format!("{addr} must not have {}", flag.str()),
            );
        }
        if e.has("config_must_include") {
            let config_text = dumps(get(r, "config"));
            for (_, value) in e.at("config_must_include").entries() {
                let value_text = value.str();
                l.check(
                    config_text.contains(value_text),
                    format!("{addr} config missing value {value_text}"),
                );
            }
        }
    }

    // Edges
    let all_edges: Vec<&Json> = items(get(&clusters, "clusters"))
        .iter()
        .flat_map(|cluster| items(get(cluster, "edges")))
        .collect();
    for ee in exp.at("edges_must_include").arr() {
        let from = ee.at("from_contains").str();
        let to = ee.at("to_contains").str();
        let found = all_edges.iter().any(|edge| {
            get(edge, "from").as_str().unwrap_or("").contains(from)
                && get(edge, "to").as_str().unwrap_or("").contains(to)
                && eq(
                    get(edge, "relationship_type"),
                    ee.at("relationship_type").json(),
                )
                && (!ee.has("evidence_contains")
                    || dumps(get(edge, "evidence")).contains(ee.at("evidence_contains").str()))
        });
        l.check(
            found,
            format!(
                "edge missing: {from} -> {to} ({})",
                ps(ee.at("relationship_type").json())
            ),
        );
    }

    // Cluster coverage
    let clustered: BTreeSet<String> = items(get(&clusters, "clusters"))
        .iter()
        .flat_map(|cluster| {
            list(cluster, "primary_resources")
                .iter()
                .chain(list(cluster, "secondary_resources"))
        })
        .map(ps)
        .collect();
    let known: BTreeSet<String> = res.keys().cloned().collect();
    let difference: Vec<String> = known.symmetric_difference(&clustered).cloned().collect();
    l.check(
        known == clustered,
        format!("cluster coverage mismatch: {}", rl(&difference)),
    );
    l.check(
        items(get(&clusters, "clusters")).iter().any(|cluster| {
            eq(get(cluster, "creation_order_depth"), &Json::Int(0))
                && get(cluster, "cluster_id")
                    .as_str()
                    .is_some_and(|id| id.contains("networking"))
        }),
        "no networking cluster at depth 0",
    );

    // AI detection
    l.check(
        get(path(&inv, &["ai_detection"]), "has_ai_workload")
            == exp.at("ai_detection").at("has_ai_workload").json(),
        "ai_detection mismatch",
    );

    // live_metadata + drift
    let lm = get(&inv, "live_metadata");
    let exp_lm = exp.at("live_metadata");
    l.check(
        eq(get(lm, "method"), exp_lm.at("method").json()),
        "live method",
    );
    l.check(
        eq(get(lm, "project"), exp_lm.at("project").json()),
        "live project",
    );
    let warns = dumps(&or_default(lm, "capture_warnings", Json::Array(Vec::new()))).to_lowercase();
    for mention in exp_lm.at("capture_warnings_must_mention").strs() {
        l.check(
            warns.contains(mention),
            format!("capture_warnings missing mention of {mention}"),
        );
    }
    let drift = get(lm, "drift");
    let exp_drift = exp_lm.at("drift");
    let terraform_only = number_or_bool(get(drift, "resources_terraform_only")).unwrap_or(99.0);
    l.check(
        terraform_only <= exp_drift.at("resources_terraform_only_max").f64(),
        format!(
            "resources_terraform_only={}",
            ps(get(drift, "resources_terraform_only"))
        ),
    );
    l.check(
        at_least(
            get(drift, "resources_live_only"),
            0.0,
            exp_drift.at("resources_live_only_min").f64(),
        ),
        format!(
            "resources_live_only={}",
            ps(get(drift, "resources_live_only"))
        ),
    );
    let fields = conflict_fields(&inv);
    for field in exp_drift.at("config_conflict_fields_must_include").arr() {
        l.check(
            fields.iter().any(|candidate| eq(candidate, field.json())),
            format!("config_conflicts missing field {}", ps(field.json())),
        );
    }

    // Safety: no fixture env names paired with values, no AWS names
    let doc = dumps(&inv);
    l.check(
        !doc.to_lowercase()
            .replace("aws-design", "")
            .contains("aws_"),
        "possible AWS naming in discover artifact",
    );

    // Secret hygiene: no env entry anywhere may carry a value payload, and env
    // data must appear only as name lists (env_var_names), never env objects.
    walk(
        context,
        &inv,
        "$",
        "env",
        "'env' key",
        "spec requires env_var_names (names only)",
    );
    for env_name in ["STRIPE_SECRET_KEY", "DATABASE_URL", "REDIS_URL"] {
        l.check(
            !doc.contains(&format!("\"{env_name}\": ")),
            format!("fixture env name {env_name} appears as a KEY (value paired) — names must be list items only"),
        );
    }
    Some(())
}

// ------------------------------------------------------------- GCP baseline

pub(super) fn gcp_baseline(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let exp_doc = context.expectation("expected-baseline.json")?;
    let exp = context.spec(&exp_doc, "expected-baseline.json");

    // The estimate, or the Part 1 preview of the targeted-replay harness.
    let mut source = None;
    for name in ["estimation-infra.json", "current-costs-preview.json"] {
        if context.exists(&context.run, name) {
            source = Some(context.json_in_dir(&context.run, name)?);
            break;
        }
    }
    let Some(src) = source else {
        l.fail("neither estimation-infra.json nor current-costs-preview.json in run dir");
        return None;
    };
    // The preview file may BE the current_costs object.
    let cur = src.get("current_costs").unwrap_or(&src);

    let e = exp.at("current_costs");
    l.check(
        eq(get(cur, "source"), e.at("source").json()),
        format!(
            "source={} want {}",
            ps(get(cur, "source")),
            ps(e.at("source").json())
        ),
    );
    let accuracy = cur.get("accuracy").map_or_else(String::new, ps);
    l.check(
        accuracy.contains(e.at("accuracy_contains").str()),
        format!("accuracy={}", ps(get(cur, "accuracy"))),
    );
    let total = e.at("monthly_total").f64();
    let tolerance = e.at("tolerance").f64();
    l.check(
        find_number_near(cur, total, tolerance),
        format!(
            "no numeric total ≈ {total} (±{tolerance}) in current_costs — rate-card math broken"
        ),
    );
    let note_value = get(cur, "baseline_note");
    let note = if truthy(note_value) {
        ps(note_value)
    } else {
        String::new()
    };
    l.check(
        !strip(&note).is_empty(),
        "baseline_note missing — the derived-baseline caveat is mandatory",
    );
    for phrase in e.at("baseline_note_must_mention").strs() {
        l.check(
            note.to_lowercase().contains(&phrase.to_lowercase()),
            format!("baseline_note does not mention '{phrase}'"),
        );
    }

    for (rid, spec) in exp.at("per_resource").entries() {
        let monthly = spec.at("monthly").f64();
        l.check(
            find_number_near(cur, monthly, spec.at("tolerance").f64()),
            format!("per-resource figure for {rid} ≈ {monthly} not found in the breakdown"),
        );
    }

    let warn_text = format!(
        "{}{}{}",
        dumps(&or_default(&src, "warnings", Json::Array(Vec::new()))),
        dumps(&or_default(cur, "warnings", Json::Array(Vec::new()))),
        dumps(&or_default(cur, "excluded", Json::Array(Vec::new()))),
    );
    let cur_text = dumps(cur);
    for name in exp.at("excluded_must_be_warned").strs() {
        l.check(
            warn_text.contains(name) || cur_text.contains(name),
            format!("excluded resource '{name}' not surfaced in warnings"),
        );
    }

    l.check(
        !eq_str(get(cur, "source"), "billing_data"),
        "claims billing_data with no billing-profile.json",
    );
    Some(())
}

// ---------------------------------------------------------- Heroku estimate

/// Names a boolean figure in a message: it would read as 1 or 0 there.
fn bool_note(value: &Json) -> &'static str {
    if matches!(value, Json::Bool(_)) {
        " (a boolean is not a number)"
    } else {
        ""
    }
}

/// `value.get(key, default)`: the supplied value whatever its type, the
/// default only for an absent key.
fn or_default(value: &Json, key: &str, default: Json) -> Json {
    value.get(key).cloned().unwrap_or(default)
}

/// `math.isclose(a, b, abs_tol=0.01)` for a figure that must be a number.
fn close(value: &Json, target: f64) -> bool {
    number(value).is_some_and(|n| isclose(n, target, 0.01))
}

/// Whether `target` appears as a numeric value anywhere under `node`.
fn find_number(node: &Json, target: f64) -> bool {
    find_number_near(node, target, 0.01)
}

/// The balanced/mid/monthly cost of one breakdown entry.
fn entry_cost(value: &Json) -> Option<f64> {
    if let Some(n) = number(value) {
        return Some(n);
    }
    ["balanced", "mid", "monthly", "monthly_cost", "cost"]
        .iter()
        .find_map(|key| number(get(value, key)))
}

pub(super) fn heroku_estimate(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let run = &context.run;
    if !context.exists(run, "estimation-infra.json") {
        l.fail("missing estimation-infra.json in run dir");
        return None;
    }
    let est = context.json_in_dir(run, "estimation-infra.json")?;
    let exp_doc = context.expectation("expected-estimate.json")?;
    let exp = context.spec(&exp_doc, "expected-estimate.json");
    let doc = dumps(&est);

    // --- Baseline (Part 1, rung 2: live prices + dyno cache) ---
    let b = exp.at("baseline");
    let cur = est.get("current_costs").unwrap_or(&NULL);
    l.check(
        eq(get(cur, "source"), b.at("source").json()),
        format!(
            "current_costs.source={} want {}",
            ps(get(cur, "source")),
            ps(b.at("source").json())
        ),
    );
    let monthly_total = b.at("monthly_total").f64();
    l.check(
        find_number(cur, monthly_total),
        format!("current_costs has no numeric total == {monthly_total} (exact add-on + cache dyno math)"),
    );
    let note_value = get(cur, "baseline_note");
    let note = if truthy(note_value) {
        ps(note_value)
    } else {
        String::new()
    };
    l.check(
        !strip(&note).is_empty(),
        "baseline_note missing/empty — the derived-baseline caveat is mandatory",
    );
    for word in b.at("baseline_note_must_mention").strs() {
        l.check(
            note.to_lowercase().contains(&word.to_lowercase()),
            format!("baseline_note does not mention '{word}'"),
        );
    }
    if b.get("must_not_fabricate_billing").truthy() {
        l.check(
            !eq_str(get(cur, "source"), "billing_data"),
            "source claims billing_data with no billing_profile in inventory",
        );
        l.check(
            !truthy(get(&est, "billing_profile")),
            "estimation-infra.json fabricated a billing_profile",
        );
    }

    // --- Cost comparison (Part 3 — must run for the derived source) ---
    let c = exp.at("cost_comparison");
    let comp = get(&est, "cost_comparison");
    l.check(
        matches!(comp, Json::Object(_)) && truthy(comp),
        "cost_comparison absent — comparison must run for ANY baseline source",
    );
    if matches!(comp, Json::Object(_)) {
        let baseline = c.at("heroku_monthly_baseline").f64();
        l.check(
            close(get(comp, "heroku_monthly_baseline"), baseline),
            format!(
                "cost_comparison.heroku_monthly_baseline={} want {baseline}",
                ps(get(comp, "heroku_monthly_baseline"))
            ),
        );
        l.check(
            eq(get(comp, "baseline_source"), c.at("baseline_source").json()),
            format!(
                "cost_comparison.baseline_source={}",
                ps(get(comp, "baseline_source"))
            ),
        );
        for option in c.at("required_options").strs() {
            let o = get(comp, option);
            if !matches!(o, Json::Object(_)) {
                l.fail(format!("cost_comparison.{option} missing"));
                continue;
            }
            for field in c.at("required_option_fields").strs() {
                l.check(
                    o.get(field).is_some(),
                    format!("cost_comparison.{option}.{field} missing"),
                );
            }
            // The outer guards accept a JSON bool (a Python `int`), but the
            // figure compared first goes through `close`, which refuses one
            // (#4753): a boolean difference fails instead of reading as 1/0.
            let aws_json = get(o, "aws_monthly");
            let diff_json = get(o, "monthly_difference");
            let (aws, diff) = (number_or_bool(aws_json), number_or_bool(diff_json));
            if let (Some(aws), Some(diff)) = (aws, diff) {
                l.check(
                    close(diff_json, aws - baseline),
                    format!(
                        "cost_comparison.{option} monthly_difference {diff} != {aws} - {baseline}{}",
                        bool_note(diff_json)
                    ),
                );
            }
            let annual_json = get(o, "annual_difference");
            if let (Some(diff), Some(annual)) = (diff, number_or_bool(annual_json)) {
                l.check(
                    close(annual_json, diff * 12.0),
                    format!(
                        "cost_comparison.{option} annual_difference {annual} != 12 x {diff}{}",
                        bool_note(annual_json)
                    ),
                );
            }
        }
    }

    // --- Migration cost considerations (Part 4 — keyed off baseline presence) ---
    let m = exp.at("migration_cost_considerations");
    let mig = est.get("migration_cost_considerations").unwrap_or(&NULL);
    l.check(
        get(mig, "baseline_available") == m.at("baseline_available").json(),
        format!(
            "migration_cost_considerations.baseline_available={}",
            ps(get(mig, "baseline_available"))
        ),
    );
    l.check(
        eq(get(mig, "baseline_source"), m.at("baseline_source").json()),
        format!(
            "migration_cost_considerations.baseline_source={}",
            ps(get(mig, "baseline_source"))
        ),
    );
    l.check(
        i64::try_from(list(mig, "categories").len()).unwrap_or(i64::MAX) >= m.at("categories_min").i64(),
        "migration_cost_considerations.categories empty — dual-run cost must be priced from the derived source",
    );

    // --- Projected costs (Property-16 invariant + design coverage) ---
    let proj = get(&est, "projected_costs");
    let balanced = get(proj, "aws_monthly_balanced");
    l.check(
        number_or_bool(balanced).is_some_and(|n| n > 0.0),
        format!(
            "projected_costs.aws_monthly_balanced={} not a positive number",
            ps(balanced)
        ),
    );
    let breakdown = get(proj, "breakdown");
    let exp_projected = exp.at("projected_costs");
    if exp_projected.at("balanced_equals_breakdown_sum").truthy()
        && matches!(breakdown, Json::Object(_))
        && truthy(breakdown)
    {
        let costs: Vec<Option<f64>> = super::ledger::members(breakdown)
            .iter()
            .map(|(_, value)| entry_cost(value))
            .collect();
        match (
            costs.iter().copied().collect::<Option<Vec<f64>>>(),
            number_or_bool(balanced),
        ) {
            (Some(costs), Some(balanced_value)) => {
                let total: f64 = costs.iter().sum();
                l.check(
                    isclose(total, balanced_value, (0.02 * balanced_value).max(1.0)),
                    format!(
                        "balanced total {balanced_value} != breakdown sum {} (Property-16)",
                        r2(total)
                    ),
                );
            }
            _ => l.fail("breakdown entries lack a recognizable balanced/monthly cost field"),
        }
    }

    if exp_projected
        .at("every_design_service_priced_or_warned")
        .truthy()
    {
        if let Some(design) = context.json_in_dir(run, "aws-design.json") {
            // `est.get("warnings", [])` and `est.get("pricing_source", {})`:
            // the supplied value is serialised as it is, whatever its type
            // (#4748); only an absent key reads as the default.
            let warnings_txt = format!(
                "{}{}",
                dumps(&or_default(&est, "warnings", Json::Array(Vec::new()))),
                dumps(&or_default(
                    &est,
                    "pricing_source",
                    Json::Object(Vec::new())
                ))
            );
            let services = get(&design, "services");
            if !matches!(services, Json::Array(_)) && !matches!(services, Json::Null) {
                l.fail(format!(
                    "aws-design.json services is {}, not a list",
                    type_name(services)
                ));
            }
            for svc in items(services) {
                let sid = get(svc, "service_id").as_str().unwrap_or("");
                let name = get(svc, "aws_service").as_str().unwrap_or("");
                let covered = doc.contains(sid) || doc.contains(name) || warnings_txt.contains(sid);
                l.check(
                    covered,
                    format!(
                        "design service {sid} neither in the cost breakdown nor warned as unpriced"
                    ),
                );
            }
        }
    }

    // --- Warnings hygiene ---
    let warn_doc = dumps(&or_default(&est, "warnings", Json::Array(Vec::new())));
    for bad in exp.at("warnings_must_not_contain").strs() {
        l.check(
            !warn_doc.contains(bad),
            format!("warnings contain '{bad}' — everything in this scenario is priceable"),
        );
    }

    // --- Must-not-exist / secret hygiene ---
    l.check(
        !doc.contains("\"billing_period\""),
        "billing_period present — no invoice data exists in this scenario",
    );
    for bad in ["sk_live", "postgres://", "rediss://", "AKIA", "Bearer "] {
        l.check(!doc.contains(bad), format!("possible secret value: {bad}"));
    }
    Some(())
}
