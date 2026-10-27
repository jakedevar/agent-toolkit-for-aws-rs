//! The what-if workshop asserters (`heroku-workshop` and `gcp-workshop`
//! `check_expected_workshop.py`): a workshop run is compared with
//! `expected-workshop.json` and with the frozen discovery inventory of its
//! seed directory.

use toolkit_core::Json;

use super::ledger::{eq, eq_str, get, list, objs, path, ps, truthy, Spec};
use super::{Anchored, Context, Flow};

/// The scenario index and the count window both programs check.
fn load_index(context: &Context<'_>, run: &Anchored) -> Option<Json> {
    let l = context.ledger;
    let index_present = context.exists(run, "scenarios/index.json");
    l.check(
        index_present,
        if exp_is_heroku(context) {
            "missing scenarios/index.json"
        } else {
            "missing index"
        },
    );
    if !index_present {
        return None;
    }
    context.json_in_dir(&run.join("scenarios"), "index.json")
}

fn exp_is_heroku(context: &Context<'_>) -> bool {
    context.fixture.ends_with("heroku-workshop")
}

/// `inventory_must_match_seed_bytes`: the workshop must freeze discovery.
fn inventory_frozen(
    context: &Context<'_>,
    exp: &Spec<'_>,
    name: &str,
    seed_dir: &Anchored,
    wording: (&str, &str, &str),
) {
    let l = context.ledger;
    let run = &context.run;
    let present = context.exists(run, name);
    let seeded = context.exists(seed_dir, name);
    l.check(present, wording.0);
    if wording.1 != "-" {
        l.check(seeded, wording.1);
    }
    if present && seeded && exp.get("inventory_must_match_seed_bytes").truthy() {
        let run_bytes = context.raw_text(run, name);
        let seed_bytes = context.raw_text(seed_dir, name);
        l.check(run_bytes.is_some() && run_bytes == seed_bytes, wording.2);
    }
}

fn seed_dir(context: &Context<'_>) -> Anchored {
    context
        .seed
        .clone()
        .unwrap_or_else(|| context.fixture.join("seed"))
}

fn first_service<'a>(design: &'a Json, aws_service: &str) -> Option<&'a Json> {
    objs(get(design, "services")).find(|service| eq_str(get(service, "aws_service"), aws_service))
}

pub(super) fn heroku(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let run = &context.run;
    let exp_doc = context.expectation("expected-workshop.json")?;
    let exp = context.spec(&exp_doc, "expected-workshop.json");
    let seed = seed_dir(context);

    inventory_frozen(
        context,
        &exp,
        "heroku-resource-inventory.json",
        &seed,
        (
            "missing heroku-resource-inventory.json in run dir",
            "missing seed inventory",
            "inventory bytes changed — workshop must freeze discovery",
        ),
    );

    let index = load_index(context, run)?;
    let scenarios = list(&index, "scenarios").len();
    let min = exp.at("min_scenarios").i64();
    let max = exp.at("max_scenarios").i64();
    let count = i64::try_from(scenarios).unwrap_or(i64::MAX);
    l.check(count >= min, format!("scenario count {scenarios} < {min}"));
    l.check(
        count <= max,
        format!("scenario count {scenarios} > max {max}"),
    );
    l.check(
        eq(
            get(&index, "baseline_scenario_id"),
            exp.at("baseline_scenario_id").json(),
        ),
        "baseline_scenario_id mismatch",
    );
    l.check(
        eq(
            get(&index, "active_scenario_id"),
            exp.at("active_scenario_id").json(),
        ),
        "active_scenario_id mismatch",
    );

    let (prefs_text, prefs) = context.json_in_dir_with_text(run, "preferences.json")?;
    let workshop = get(&prefs, "workshop");
    l.check(
        eq(
            get(workshop, "cpu_architecture"),
            exp.at("active_cpu_architecture").json(),
        ),
        "active cpu_architecture mismatch",
    );
    l.check(
        eq(
            get(workshop, "active_scenario_id"),
            exp.at("active_scenario_id").json(),
        ),
        "prefs workshop.active_scenario_id mismatch",
    );

    let (design_text, design) = context.json_in_dir_with_text(run, "aws-design.json")?;
    l.design_shape(&design, "aws-design.json");
    let eb = first_service(&design, "Elastic Beanstalk");
    l.check(
        eb.is_some(),
        "no Elastic Beanstalk service in active design",
    );
    if let Some(eb) = eb {
        let want = exp.at("active_eb_instance_type");
        l.check(
            eq(path(eb, &["aws_config", "instance_type"]), want.json()),
            format!(
                "EB instance_type={} want {}",
                ps(path(eb, &["aws_config", "instance_type"])),
                ps(want.json())
            ),
        );
        l.check(
            eq(
                path(eb, &["aws_config", "cpu_architecture"]),
                exp.at("active_cpu_architecture").json(),
            ),
            "EB cpu_architecture mismatch",
        );
    }

    let baseline_id = exp.at("baseline_scenario_id").str().to_string();
    let base_design_name = format!("{baseline_id}.aws-design.json");
    let scenarios_dir = run.join("scenarios");
    if context.exists(&scenarios_dir, &base_design_name)
        && exp.get("design_must_differ_from_baseline").truthy()
    {
        if let Some(base_design) = context.json_in_dir(&scenarios_dir, &base_design_name) {
            l.design_shape(&base_design, &base_design_name);
            let base_eb = first_service(&base_design, "Elastic Beanstalk");
            l.check(
                base_eb.is_some(),
                "no Elastic Beanstalk in baseline design snapshot",
            );
            if let (Some(base_eb), Some(eb)) = (base_eb, eb) {
                let base_type = path(base_eb, &["aws_config", "instance_type"]);
                let act_type = path(eb, &["aws_config", "instance_type"]);
                l.check(
                    !eq(act_type, base_type),
                    format!(
                        "design instance_type unchanged ({}) vs baseline ({})",
                        ps(act_type),
                        ps(base_type)
                    ),
                );
                if exp.get("baseline_eb_instance_type").truthy() {
                    let want = exp.at("baseline_eb_instance_type");
                    l.check(
                        eq(base_type, want.json()),
                        format!(
                            "baseline EB instance_type={} want {}",
                            ps(base_type),
                            ps(want.json())
                        ),
                    );
                }
            }
        }
    }

    let est = context.json_in_dir(run, "estimation-infra.json")?;
    let base_manifest_name = format!("{baseline_id}.json");
    let manifest_present = context.exists(&scenarios_dir, &base_manifest_name);
    l.check(manifest_present, "missing baseline scenario manifest");
    if manifest_present && exp.get("balanced_must_differ_from_baseline").truthy() {
        balanced_differs(context, &scenarios_dir, &base_manifest_name, &est, true);
    }

    phase_status(context, &exp, true);

    // Secret hygiene: no token-looking material.
    let blob = format!("{prefs_text}{design_text}");
    for bad in ["HEROKU_API_KEY", "Bearer ", "sk_live"] {
        l.check(
            !blob.contains(bad),
            format!("possible secret material: {bad}"),
        );
    }
    Some(())
}

/// `balanced_must_differ_from_baseline`: the active total moved.
fn balanced_differs(
    context: &Context<'_>,
    scenarios_dir: &Anchored,
    manifest: &str,
    est: &Json,
    heroku: bool,
) {
    let l = context.ledger;
    let Some(base) = context.json_in_dir(scenarios_dir, manifest) else {
        return;
    };
    let base_bal = path(&base, &["estimation_summary", "aws_monthly_balanced"]);
    let act_bal = path(est, &["projected_costs", "aws_monthly_balanced"]);
    if matches!(base_bal, Json::Null) || matches!(act_bal, Json::Null) {
        l.fail("balanced totals are missing: estimation_summary.aws_monthly_balanced of the baseline manifest or projected_costs.aws_monthly_balanced of the estimate");
        return;
    }
    l.check(
        !eq(act_bal, base_bal),
        if heroku {
            format!(
                "balanced total unchanged ({}) vs baseline ({})",
                ps(act_bal),
                ps(base_bal)
            )
        } else {
            format!("balanced unchanged {}", ps(act_bal))
        },
    );
}

/// `.phase-status.json` assertions, when the file exists.
fn phase_status(context: &Context<'_>, exp: &Spec<'_>, heroku: bool) {
    let l = context.ledger;
    let Some(phase) = context.json_if_exists(&context.run, ".phase-status.json") else {
        return;
    };
    let phases = get(&phase, "phases");
    if exp.get("generate_must_not_be_completed").truthy() {
        l.check(
            !eq_str(get(phases, "generate"), "completed"),
            if heroku {
                "generate must not auto-complete during workshop"
            } else {
                "generate completed"
            },
        );
    }
    if exp.get("current_phase_must_be").truthy() {
        let want = exp.at("current_phase_must_be");
        l.check(
            eq(get(&phase, "current_phase"), want.json()),
            if heroku {
                format!(
                    "current_phase={} want {}",
                    ps(get(&phase, "current_phase")),
                    ps(want.json())
                )
            } else {
                "current_phase".to_string()
            },
        );
    }
    if exp.get("workshop_phase_must_be").truthy() {
        let want = exp.at("workshop_phase_must_be");
        l.check(
            eq(get(phases, "workshop"), want.json()),
            if heroku {
                format!(
                    "phases.workshop={} want {}",
                    ps(get(phases, "workshop")),
                    ps(want.json())
                )
            } else {
                "workshop phase".to_string()
            },
        );
    }
}

pub(super) fn gcp(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let run = &context.run;
    let exp_doc = context.expectation("expected-workshop.json")?;
    let exp = context.spec(&exp_doc, "expected-workshop.json");
    let seed = seed_dir(context);

    inventory_frozen(
        context,
        &exp,
        "gcp-resource-inventory.json",
        &seed,
        ("missing inventory", "-", "inventory bytes changed"),
    );

    let index = load_index(context, run)?;
    let count = i64::try_from(list(&index, "scenarios").len()).unwrap_or(i64::MAX);
    l.check(count >= exp.at("min_scenarios").i64(), "too few scenarios");
    l.check(
        eq(
            get(&index, "active_scenario_id"),
            exp.at("active_scenario_id").json(),
        ),
        "active id",
    );

    let prefs = context.json_in_dir(run, "preferences.json")?;
    let arch = path(&prefs, &["design_constraints", "cpu_architecture"]);
    l.check(
        eq(get(arch, "value"), exp.at("active_cpu_architecture").json()),
        format!("arch={}", ps(get(arch, "value"))),
    );
    if exp.get("q11b_prompt_must_contain").truthy() {
        let needle = exp.at("q11b_prompt_must_contain").str();
        let prompt = get(arch, "prompt").as_str().unwrap_or("");
        l.check(
            prompt.contains(needle),
            "Q11b prompt missing real clarify text",
        );
    }
    if exp.get("must_have_graviton_note").truthy() {
        let note = get(get(&prefs, "workshop"), "graviton_note");
        l.check(truthy(note), "missing workshop.graviton_note");
        let lowered = note.as_str().unwrap_or("").to_lowercase();
        l.check(
            lowered.contains("incompatible"),
            "graviton_note should mention incompatible",
        );
    }
    if exp.get("must_keep_incompatible_worker_x86").truthy() {
        let design = context.json_in_dir(run, "aws-design.json")?;
        l.design_shape(&design, "aws-design.json");
        let worker = objs(get(&design, "services"))
            .find(|service| eq_str(get(service, "source_resource_id"), "run:worker"));
        l.check(worker.is_some(), "missing worker service");
        if let Some(worker) = worker {
            l.check(
                eq_str(path(worker, &["aws_config", "cpu_architecture"]), "X86_64"),
                "incompatible worker must stay X86_64",
            );
        }
    }

    let est = context.json_in_dir(run, "estimation-infra.json")?;
    let baseline_id = exp.at("baseline_scenario_id").str().to_string();
    let scenarios_dir = run.join("scenarios");
    let manifest = format!("{baseline_id}.json");
    if context.exists(&scenarios_dir, &manifest)
        && exp.get("balanced_must_differ_from_baseline").truthy()
    {
        balanced_differs(context, &scenarios_dir, &manifest, &est, false);
    }
    phase_status(context, &exp, false);
    Some(())
}
