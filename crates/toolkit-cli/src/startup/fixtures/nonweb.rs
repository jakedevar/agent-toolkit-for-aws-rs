//! The Heroku Horizontal Non-Web Capacity Guard asserter
//! (`heroku-nonweb-scaling/check_expected_nonweb_scaling.py`).
//!
//! A persistent non-web formation with `quantity > 1` is routed off Elastic
//! Beanstalk onto Fargate, so the Fargate sizing table must carry every dyno
//! tier the Elastic Beanstalk table carries. The program also checks the three
//! dyno sizing tables directly (independent of the run directory): tier-set
//! agreement across EB, Fargate and EKS, and every Fargate row against the
//! documented Fargate CPU and memory matrix.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::Json;

use super::ledger::{eq, get, members, number, objs, path, ps, rl, rp, rs, Spec};
use super::{Context, Flow};

/// `memory_allowed(spec, memory)`: a listed value, or a stepped range.
fn memory_allowed(spec: &Spec<'_>, memory: &Json) -> bool {
    if spec.has("values") {
        return spec
            .at("values")
            .arr()
            .iter()
            .any(|allowed| eq(memory, allowed.json()));
    }
    let (lo, hi, step) = (
        spec.at("min").f64(),
        spec.at("max").f64(),
        spec.at("step").f64(),
    );
    let Some(memory) = number(memory) else {
        return false;
    };
    step > 0.0 && lo <= memory && memory <= hi && ((memory - lo) % step).abs() < f64::EPSILON
}

fn check_sizing_tables(context: &Context<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("sizing_tables");
    let mut tables: BTreeMap<&str, Vec<(String, Json)>> = BTreeMap::new();
    for name in ["eb", "fargate", "eks"] {
        let relative = spec.at(name).str();
        let Some(data) = load_table(context, relative) else {
            return;
        };
        tables.insert(name, members(get(&data, "rows")).to_vec());
    }
    let has_tier = |table: &str, tier: &str| tables[table].iter().any(|(key, _)| key == tier);
    let tiers = |table: &str| -> BTreeSet<String> {
        tables[table].iter().map(|(key, _)| key.clone()).collect()
    };

    for tier in spec.at("required_tiers_in_every_table").strs() {
        for name in ["eb", "fargate", "eks"] {
            l.check(
                has_tier(name, tier),
                format!("{name} sizing table has no '{tier}' row"),
            );
        }
    }
    if spec.get("fargate_and_eks_tiers_must_be_identical").truthy() {
        let only_fargate: Vec<String> = tiers("fargate")
            .difference(&tiers("eks"))
            .cloned()
            .collect();
        let only_eks: Vec<String> = tiers("eks")
            .difference(&tiers("fargate"))
            .cloned()
            .collect();
        l.check(
            only_fargate.is_empty(),
            format!(
                "tiers in the Fargate table but not EKS: {}",
                rl(&only_fargate)
            ),
        );
        l.check(
            only_eks.is_empty(),
            format!("tiers in the EKS table but not Fargate: {}", rl(&only_eks)),
        );
    }
    if spec.get("eb_tiers_must_cover_fargate_tiers").truthy() {
        let missing: Vec<String> = tiers("fargate").difference(&tiers("eb")).cloned().collect();
        l.check(
            missing.is_empty(),
            format!("tiers in the Fargate table but not EB: {}", rl(&missing)),
        );
    }

    let matrix = exp.at("fargate_cpu_memory_matrix");
    let exempt: Vec<&str> = if exp.get("fargate_matrix_baseline").truthy() {
        exp.get("fargate_matrix_baseline").at("exempt_rows").strs()
    } else {
        Vec::new()
    };
    for (tier, row) in &tables["fargate"] {
        if exempt.contains(&tier.as_str()) {
            l.note(format!(
                "fargate matrix check skipped for baselined row '{tier}'"
            ));
            continue;
        }
        let cpu = get(row, "fargate_cpu");
        let memory = get(row, "fargate_memory");
        let cpu_key = super::ledger::ps(cpu);
        if !matrix.has(&cpu_key) {
            l.fail(format!(
                "fargate row '{tier}': fargate_cpu={} is not a valid Fargate task CPU value",
                ps(cpu)
            ));
            continue;
        }
        l.check(
            memory_allowed(&matrix.at(&cpu_key), memory),
            format!(
                "fargate row '{tier}': cpu={} with memory={} MiB is not a valid Fargate pairing",
                ps(cpu),
                ps(memory)
            ),
        );
    }
}

/// `load(plugin_root / relative)`: `missing NAME in DIR` or `NAME is not valid
/// JSON`.
fn load_table(context: &Context<'_>, relative: &str) -> Option<Json> {
    let full = context.plugin_root.join(relative);
    let name = full.file_name()?.to_string_lossy().into_owned();
    let dir = full.parent()?.to_path_buf();
    context.json_in_dir(&dir, &name)
}

/// `int(value)` for the formation quantity.
fn to_int(value: &Json) -> Option<i64> {
    match value {
        Json::Int(n) => Some(*n),
        Json::Bool(flag) => Some(i64::from(*flag)),
        #[allow(clippy::cast_possible_truncation)]
        Json::Float(n) if n.is_finite() => Some(n.trunc() as i64),
        Json::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

pub(super) fn run(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let exp_doc = context.expectation("expected-nonweb-scaling.json")?;
    let exp = context.spec(&exp_doc, "expected-nonweb-scaling.json");

    check_sizing_tables(context, &exp);

    let inventory = context.json_in_dir(&context.run, "heroku-resource-inventory.json");
    let design = context.json_in_dir(&context.run, "aws-design.json");
    let (inventory, design) = (inventory?, design?);

    let mut by_source: BTreeMap<String, Vec<&Json>> = BTreeMap::new();
    for service in objs(get(&design, "services")) {
        by_source
            .entry(ps(get(service, "source_resource_id")))
            .or_default()
            .push(service);
    }

    let formations: Vec<&Json> = objs(get(&inventory, "resources"))
        .filter(|resource| {
            super::ledger::eq_str(get(resource, "resource_type"), "formation")
                && !super::ledger::eq_str(path(resource, &["config", "process_type"]), "release")
        })
        .collect();
    l.check(
        !formations.is_empty(),
        "inventory has no non-release formation resources to check",
    );

    for formation in formations {
        let rid = ps(get(formation, "resource_id"));
        let cfg = get(formation, "config");
        let mapped = by_source.get(&rid).cloned().unwrap_or_default();
        if exp.get("every_formation_must_map").truthy() {
            l.check(
                !mapped.is_empty(),
                format!(
                    "formation {rid} ({}) has no service in the design — silently dropped",
                    ps(get(cfg, "dyno_type"))
                ),
            );
        }
        if mapped.is_empty() {
            continue;
        }
        // Horizontal Non-Web Capacity Guard.
        let quantity = match get(cfg, "quantity") {
            Json::Null if cfg.get("quantity").is_none() => Some(0),
            other => to_int(other),
        };
        let Some(quantity) = quantity else {
            l.fail(format!(
                "formation {rid}: config.quantity is {}, not an integer",
                rp(get(cfg, "quantity"))
            ));
            continue;
        };
        if !super::ledger::eq_str(get(cfg, "process_type"), "web") && quantity > 1 {
            let services: Vec<String> = mapped
                .iter()
                .map(|service| rp(get(service, "aws_service")))
                .collect();
            l.check(
                mapped
                    .iter()
                    .any(|service| super::ledger::eq_str(get(service, "aws_service"), "Fargate")),
                format!(
                    "formation {rid} is non-web with quantity={} but was not routed to Fargate (got [{}])",
                    ps(get(cfg, "quantity")),
                    services.join(", ")
                ),
            );
        }
    }

    for want in exp.get("expected_services").arr_or_empty() {
        let rid = want.at("source_resource_id").str();
        let aws_service = want.at("aws_service").str();
        let mapped: Vec<&&Json> = by_source
            .get(rid)
            .map(|services| {
                services
                    .iter()
                    .filter(|service| {
                        super::ledger::eq_str(get(service, "aws_service"), aws_service)
                    })
                    .collect()
            })
            .unwrap_or_default();
        l.check(
            !mapped.is_empty(),
            format!("no {aws_service} service for {rid}"),
        );
        let Some(first) = mapped.first() else {
            continue;
        };
        let cfg = get(first, "aws_config");
        if want.get("aws_config").truthy() {
            for (key, value) in want.at("aws_config").entries() {
                l.check(
                    eq(get(cfg, key), value.json()),
                    format!(
                        "{rid}: aws_config.{key}={} want {}",
                        rp(get(cfg, key)),
                        rp(value.json())
                    ),
                );
            }
        }
    }

    let warnings = super::ledger::items(get(&design, "warnings"))
        .iter()
        .map(ps)
        .collect::<Vec<_>>()
        .join(" | ");
    for sub in exp.get("required_warning_substrings").arr_or_empty() {
        let sub = sub.str();
        l.check(
            warnings.contains(sub),
            format!("missing expected warning substring: {}", rs(sub)),
        );
    }
    for sub in exp.get("forbidden_warning_substrings").arr_or_empty() {
        let sub = sub.str();
        l.check(
            !warnings.contains(sub),
            format!("design carries a forbidden warning substring: {}", rs(sub)),
        );
    }
    Some(())
}
