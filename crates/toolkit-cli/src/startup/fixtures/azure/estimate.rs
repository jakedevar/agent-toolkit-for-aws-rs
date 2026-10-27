//! The azure-to-aws ESTIMATE asserter over the Terraform corpus
//! (`check_expected_estimate.py`), the sibling of the Design asserter and the
//! independent oracle for a phase whose `_assert` prose is bound by CI but
//! never evaluated.
//!
//! Estimate cannot use exact assertions (AWS rates change on AWS's cadence), so
//! tolerance is applied in three tiers, and the tiers are the point:
//!
//! 1. **exact**: everything that is not a dollar figure (which lines exist,
//!    which are excluded and why, labels, flags, `run_mode`);
//! 2. **relational**: arithmetic identities exact to a relative tolerance,
//!    properties of the ARTIFACT that survive rate refreshes;
//! 3. **banded**: per-line dollar figures against `[min, max]` RECORDED in
//!    `expected-estimate.json`, never recomputed here (recomputing from the
//!    same rate card the run read would catch arithmetic slips and nothing
//!    else).

use toolkit_core::Json;
use toolkit_msk::pyfmt::{py_float_parse, round_to};

use super::super::ledger::{
    dotted, dumps_unicode, eq, get, items, list, members, number, path, ps, r2, rl, rp, rs, strip,
    truthy, type_name, Ledger, NULL,
};
use super::super::{Context, Flow};
use crate::manifests::py_float_repr;

/// A breakdown line keyed by its `service_id`.
type Lines = Vec<(String, Json)>;

/// `num(v)`: a cost value as a float, tolerating `"123.45"` strings.
fn num(value: &Json) -> Option<f64> {
    match value {
        Json::String(text) => py_float_parse(text.replace(['$', ','], "").trim()),
        other => number(other),
    }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * 1.0_f64.max(a.abs()).max(b.abs())
}

fn fl(value: f64) -> String {
    py_float_repr(value)
}

/// TIER 3: the band is recorded, not computed.
fn banded(l: &Ledger, label: &str, got: &Json, lo: &Json, hi: &Json, basis: &str) {
    let (Some(low), Some(high)) = (number(lo), number(hi)) else {
        l.fail(format!(
            "expectation document is malformed: the band of {label} is not numeric"
        ));
        return;
    };
    let Some(g) = num(got) else {
        l.fail(format!(
            "{label}: expected a numeric cost in [{}, {}], got {}",
            ps(lo),
            ps(hi),
            rp(got)
        ));
        return;
    };
    if !(low <= g && g <= high) {
        l.fail(format!(
            "{label}: {} is outside the recorded band [{}, {}] (basis {basis}). If a rate genuinely changed, edit expected-estimate.json so the new band is a reviewable diff -- do not widen it to pass.",
            fl(g),
            ps(lo),
            ps(hi)
        ));
    }
}

/// `v or default`: the value if truthy, else the empty object.
fn or_empty(value: &Json) -> &Json {
    static EMPTY: Json = Json::Object(Vec::new());
    if truthy(value) {
        value
    } else {
        &EMPTY
    }
}

fn line<'a>(lines: &'a Lines, sid: &str) -> Option<&'a Json> {
    lines
        .iter()
        .rev()
        .find(|(key, _)| key == sid)
        .map(|(_, v)| v)
}

/// `str.isupper()`.
fn is_upper(text: &str) -> bool {
    let mut cased = false;
    for c in text.chars() {
        if c.is_lowercase() {
            return false;
        }
        if c.is_uppercase() {
            cased = true;
        }
    }
    cased
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// `re.search(rf"\b{re.escape(term)}\b", text)` with Python's word class.
fn has_whole_word(text: &str, term: &str) -> bool {
    if term.is_empty() {
        return false;
    }
    text.match_indices(term).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + term.len()..].chars().next();
        is_word(before) != is_word(term.chars().next())
            && is_word(term.chars().next_back()) != is_word(after)
    })
}

/// Drops underscore-prefixed keys and the explicit disclaimers: an artifact
/// that correctly states "no engineer-weeks appear here" necessarily contains
/// the forbidden words, and an oracle that fails it for saying so rejects a
/// correct answer as fraud.
fn scrub(value: &Json) -> Json {
    const DISCLAIMER_KEYS: [&str; 3] = [
        "no_labor_costs",
        "no_dollar_values_assigned",
        "no_labour_costs",
    ];
    match value {
        Json::Object(entries) => Json::Object(
            entries
                .iter()
                .filter(|(key, _)| {
                    !key.starts_with('_') && !DISCLAIMER_KEYS.contains(&key.as_str())
                })
                .map(|(key, member)| (key.clone(), scrub(member)))
                .collect(),
        ),
        Json::Array(values) => Json::Array(values.iter().map(scrub).collect()),
        other => other.clone(),
    }
}

/// Builds `by_id` from the breakdown. It is an OBJECT keyed by `service_id`
/// per the shared schema; an array is a schema violation that parseability
/// checks cannot catch.
fn breakdown_lines(l: &Ledger, raw: &Json) -> Lines {
    let mut lines: Lines = Vec::new();
    match raw {
        Json::Object(entries) => {
            for (sid, entry) in entries {
                if sid == "total" || sid.starts_with('_') {
                    continue;
                }
                let mut merged: Vec<(String, Json)> = members(entry).to_vec();
                match merged.iter_mut().find(|(key, _)| key == "service_id") {
                    Some((_, value)) => *value = Json::string(sid.as_str()),
                    None => merged.push(("service_id".to_string(), Json::string(sid.as_str()))),
                }
                lines.push((sid.clone(), Json::Object(merged)));
            }
        }
        // Tolerated, so the rest of the checks still report.
        Json::Array(entries) => {
            for entry in entries {
                let sid = ps(get(entry, "service_id"));
                if lines.iter().any(|(key, _)| *key == sid) {
                    l.fail(format!(
                        "breakdown has duplicate service_id {}",
                        rp(get(entry, "service_id"))
                    ));
                }
                lines.retain(|(key, _)| *key != sid);
                lines.push((sid, entry.clone()));
            }
        }
        _ => {}
    }
    lines
}

/// The upstream phase INPUT from the run dir, falling back to its committed
/// golden, so the goldens stay de-duplicated.
fn upstream(context: &Context<'_>, file: &str, sibling: &str) -> Json {
    let l = context.ledger;
    let (root, shown) = if context.exists(&context.run, file) {
        (context.run.clone(), file)
    } else {
        (context.fixture.join(sibling), file)
    };
    if context.exists(&root, shown) {
        context
            .json_if_exists(&root, shown)
            .unwrap_or(Json::Object(Vec::new()))
    } else {
        l.fail(format!("missing {file}"));
        Json::Object(Vec::new())
    }
}

#[allow(clippy::too_many_lines)]
pub(in crate::startup::fixtures) fn run(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let run = &context.run;
    let exp_doc = context.expectation_or_empty("expected-estimate.json");
    if !truthy(&exp_doc) {
        return None;
    }
    let exp = context.spec(&exp_doc, "expected-estimate.json");

    let estimate_file = exp.at("estimate_file").str();
    let est = if context.exists(run, estimate_file) {
        context
            .json_if_exists(run, estimate_file)
            .unwrap_or_else(|| Json::Object(Vec::new()))
    } else {
        l.fail(format!("missing {estimate_file}"));
        Json::Object(Vec::new())
    };
    let phase_file = exp.at("phase_status_file").str();
    let ph = if context.exists(run, phase_file) {
        context
            .json_if_exists(run, phase_file)
            .unwrap_or_else(|| Json::Object(Vec::new()))
    } else {
        l.fail(format!("missing {phase_file}"));
        Json::Object(Vec::new())
    };
    let design = upstream(context, exp.at("design_file").str(), "after-design");
    let prefs = upstream(
        context,
        exp.at("preferences_file").str(),
        "after-clarify-complete",
    );
    if !truthy(&est) {
        return None;
    }

    let pc = get(&est, "projected_costs");
    let pc = or_empty(pc);
    let lift = or_empty(get(pc, "lift"));
    let rs_obj = or_empty(get(pc, "right_sized"));
    let cc = or_empty(get(&est, "cost_comparison"));

    let raw_bd = get(pc, "breakdown");
    l.check(
        matches!(raw_bd, Json::Object(_)),
        format!(
            "projected_costs.breakdown is {}, and the shared estimation-infra.schema.json declares it type 'object' keyed by service_id with a 'total'. An array passes _validate_json and still violates the contract.",
            type_name(raw_bd)
        ),
    );
    let by_id = breakdown_lines(l, raw_bd);
    let breakdown: Vec<&Json> = by_id.iter().map(|(_, v)| v).collect();

    // ---------------- TIER 1: dual output exists at all ----------------
    let dual = exp.at("dual_output");
    for p in dual.at("required_paths").strs() {
        let mut cur = &est;
        let mut ok = true;
        for seg in p.split('.') {
            match cur {
                Json::Object(_) if cur.get(seg).is_some() => cur = get(cur, seg),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        l.check(
            ok,
            format!(
                "dual_output: required path {} is absent. {} is not the specified shape.",
                rs(p),
                dual.note("must_not_be")
            ),
        );
    }

    // Tolerance for every TIER 2 identity.
    let rel = exp.at("relational").at("rel_tolerance").f64();

    // ------------ TIER 1 + TIER 2: the three shared scenario keys ------------
    // An INTENSIONAL check against the vendored schema: read the schema's own
    // `required` list rather than restating it, so the two cannot drift apart.
    let sc = exp.at("scenarios");
    let schema_rel = exp.at("schema_relpath").str();
    if let Some(schema_text) = context.read_relative(schema_rel) {
        if let Some(schema) = context.parse_text(&schema_text, schema_rel) {
            for key in items(path(
                &schema,
                &["properties", "projected_costs", "required"],
            )) {
                let key_text = key.as_str().unwrap_or("");
                l.check(
                    pc.get(key_text).is_some(),
                    format!(
                        "projected_costs is missing {}, which the shared estimation-infra.schema.json lists as REQUIRED. No additionalProperties:false permits ADDING the azure lift/right_sized keys; it does not permit omitting a required one.",
                        rp(key)
                    ),
                );
            }
            let ac_type = path(&schema, &["properties", "accuracy_confidence", "type"]);
            if matches!(ac_type, Json::String(s) if s == "string")
                && est.get("accuracy_confidence").is_some()
            {
                l.check(
                    matches!(get(&est, "accuracy_confidence"), Json::String(_)),
                    format!(
                        "accuracy_confidence is {}; the shared schema declares it a string.",
                        type_name(get(&est, "accuracy_confidence"))
                    ),
                );
            }
        }
    } else {
        l.fail(format!(
            "cannot reach the shared schema at {schema_rel} to check conformance"
        ));
    }

    let bal = num(get(pc, "aws_monthly_balanced"));
    let prem = num(get(pc, "aws_monthly_premium"));
    let opt = num(get(pc, "aws_monthly_optimized"));
    let rsm = num(get(rs_obj, "monthly"));
    if let (Some(bal), Some(rsm)) = (bal, rsm) {
        l.check(
            close(bal, rsm, rel),
            format!(
                "aws_monthly_balanced is {} but right_sized.monthly is {}. {}",
                fl(bal),
                fl(rsm),
                sc.note("_anchor_why")
            ),
        );
    }
    if let (Some(prem), Some(bal)) = (prem, bal) {
        l.check(
            prem >= bal,
            format!(
                "aws_monthly_premium {} is below Balanced {}; higher resilience costs more, not less",
                fl(prem),
                fl(bal)
            ),
        );
    }
    if let (Some(opt), Some(bal)) = (opt, bal) {
        l.check(
            opt <= bal,
            format!(
                "aws_monthly_optimized {} is above Balanced {}; a commitment discount cannot raise the bill",
                fl(opt),
                fl(bal)
            ),
        );
    }
    banded(
        l,
        "aws_monthly_premium",
        get(pc, "aws_monthly_premium"),
        sc.at("premium_min").json(),
        sc.at("premium_max").json(),
        "total",
    );
    banded(
        l,
        "aws_monthly_optimized",
        get(pc, "aws_monthly_optimized"),
        sc.at("optimized_min").json(),
        sc.at("optimized_max").json(),
        "total",
    );
    if let (Some(opt), Some(bal)) = (opt, bal) {
        let discount = sc.at("blanket_discount").f64();
        if close(opt, round_to(bal * (1.0 - discount), 2), 0.01) {
            l.fail(format!(
                "aws_monthly_optimized {} equals Balanced x (1 - {}), i.e. a BLANKET discount across the whole total. {}",
                fl(opt),
                ps(sc.at("blanket_discount").json()),
                sc.note("_blanket_why")
            ));
        }
    }
    if let (Some(ann), Some(opt)) = (num(get(pc, "aws_annual_optimized")), opt) {
        l.check(
            close(ann, opt * 12.0, rel),
            format!(
                "aws_annual_optimized is {}, expected {}",
                fl(ann),
                r2(opt * 12.0)
            ),
        );
    }
    l.check(
        truthy(get(pc, "_premium_basis")),
        format!(
            "projected_costs._premium_basis must show the uplift arithmetic. {}",
            sc.note("_show_work_why")
        ),
    );
    l.check(
        truthy(get(pc, "_optimized_basis")),
        format!(
            "projected_costs._optimized_basis must show the two subtotals and the rate. {}",
            sc.note("_show_work_why")
        ),
    );

    // ------------ TIER 1: the line shape every line owes ------------
    // estimate-infra.md requires `basis` on EVERY line, including the $0 ones,
    // and requires `components` to sum to its own line.
    for entry in &breakdown {
        let sid = rp(get(entry, "service_id"));
        let basis = get(entry, "basis");
        l.check(
            truthy(basis) && !strip(basis.as_str().unwrap_or("x")).is_empty(),
            format!(
                "{sid}: basis is required on every line, including the $0 ones -- 'a VPC is not billed' is the answer to a question a reader otherwise has to assume."
            ),
        );
        let comps = get(entry, "components");
        if matches!(comps, Json::Object(_)) {
            let cs: f64 = members(comps)
                .iter()
                .map(|(_, v)| num(v).unwrap_or(0.0))
                .sum();
            let lv = num(get(entry, "right_sized_monthly"));
            if let Some(lv) = lv {
                if members(comps).iter().any(|(_, v)| num(v).is_some()) {
                    l.check(
                        close(cs, lv, 1e-2),
                        format!(
                            "{sid}: components sum to {} but the line is {}. A breakdown that contradicts itself is worse than one without components.",
                            r2(cs),
                            fl(lv)
                        ),
                    );
                }
            }
        }
    }

    // ------------ TIER 3: priced line bands ------------
    let lb = exp.at("line_bands");
    for spec in lb.at("priced").arr() {
        let sid = spec.at("service_id").str();
        let Some(entry) = line(&by_id, sid) else {
            l.fail(format!("breakdown is missing priced line {}", rs(sid)));
            continue;
        };
        banded(
            l,
            &format!("{sid}.right_sized_monthly"),
            get(entry, "right_sized_monthly"),
            spec.at("min").json(),
            spec.at("max").json(),
            spec.at("basis").str(),
        );
        if spec.has("must_not_be") {
            if let Some(g) = num(get(entry, "right_sized_monthly")) {
                if close(g, spec.at("must_not_be").f64(), 0.02) {
                    l.fail(format!(
                        "{sid}: {} matches the known-wrong answer {}. {}",
                        fl(g),
                        ps(spec.at("must_not_be").json()),
                        spec.note("must_not_be_why")
                    ));
                }
            }
        }
    }

    // zero-cost lines must be PRESENT at 0, not absent
    for sid in lb.at("zero_cost").at("service_ids").strs() {
        let Some(entry) = line(&by_id, sid) else {
            l.fail(format!("{sid}: absent from breakdown. It must appear at 0.00 -- a missing line is indistinguishable from a forgotten one."));
            continue;
        };
        l.check(
            num(get(entry, "right_sized_monthly")) == Some(0.0),
            format!(
                "{sid}: expected 0.0, got {}",
                rp(get(entry, "right_sized_monthly"))
            ),
        );
    }

    // ------------ TIER 1: exclusions, reasons, and the Windows rule ------------
    let mut excluded_ids: Vec<&str> = Vec::new();
    for spec in lb.at("excluded").arr() {
        let sid = spec.at("service_id").str();
        excluded_ids.push(sid);
        let Some(entry) = line(&by_id, sid) else {
            l.fail(format!("{sid}: absent from breakdown. An excluded line must still appear, carrying its exclusion_reason -- dropping it makes the estate look smaller than it is."));
            continue;
        };
        let why = spec.note("_why");
        let reason = get(entry, "exclusion_reason");
        l.check(
            eq(reason, spec.at("exclusion_reason").json()),
            format!(
                "{sid}: exclusion_reason is {}, expected {}. {why}",
                rp(reason),
                rp(spec.at("exclusion_reason").json())
            ),
        );
        l.check(
            eq(
                get(entry, "pricing_source"),
                spec.at("pricing_source").json(),
            ),
            format!(
                "{sid}: pricing_source is {}, expected {}",
                rp(get(entry, "pricing_source")),
                rp(spec.at("pricing_source").json())
            ),
        );
        l.check(
            matches!(get(entry, "excluded_from_total"), Json::Bool(true)),
            format!("{sid}: excluded_from_total must be true"),
        );
        if spec.get("missing_component_substring").truthy() {
            let missing = get(entry, "missing_component");
            let shown = if truthy(missing) {
                missing.as_str().unwrap_or("").to_lowercase()
            } else {
                String::new()
            };
            l.check(
                shown.contains(&spec.at("missing_component_substring").str().to_lowercase()),
                format!(
                    "{sid}: missing_component is {} and must name the missing piece (expected to contain {}). {why}",
                    rp(missing),
                    rs(spec.at("missing_component_substring").str())
                ),
            );
        }
        if !spec.get("floor").is_null() {
            banded(
                l,
                &format!("{sid} floor"),
                get(entry, "right_sized_monthly"),
                spec.at("floor_min").json(),
                spec.at("floor_max").json(),
                "rate_times_count",
            );
        }
        if spec.get("must_not_be_priced_from").truthy() {
            // The detectable signal is a line that ended up with a NUMBER while
            // claiming the wrong table. Forbidding the substring outright fails
            // a correct artifact that names the table in order to say it was
            // NOT used.
            let priced = num(get(entry, "right_sized_monthly"))
                .filter(|v| *v != 0.0)
                .or_else(|| num(get(entry, "lift_monthly")));
            if let Some(priced) = priced.filter(|v| *v != 0.0) {
                let table = spec.at("must_not_be_priced_from").str();
                l.check(
                    !dumps_unicode(entry).contains(table),
                    format!(
                        "{sid}: carries a cost of {} and cites {}. {why}",
                        fl(priced),
                        rs(table)
                    ),
                );
            }
        }
    }

    // ------------ TIER 2: totals reconcile against their own lines ------------
    for (key, obj, field) in [
        ("lift", lift, "lift_monthly"),
        ("right_sized", rs_obj, "right_sized_monthly"),
    ] {
        let Some(total) = num(get(obj, "monthly")) else {
            l.fail(format!(
                "projected_costs.{key}.monthly is not numeric: {}",
                rp(get(obj, "monthly"))
            ));
            continue;
        };
        let mut sum = 0.0;
        for entry in &breakdown {
            if truthy(get(entry, "exclusion_reason")) {
                continue;
            }
            match num(get(entry, field)) {
                Some(v) => sum += v,
                None => l.fail(format!(
                    "{}: {field} is not numeric ({}) and carries no exclusion_reason",
                    rp(get(entry, "service_id")),
                    rp(get(entry, field))
                )),
            }
        }
        l.check(
            close(total, sum, rel),
            format!(
                "projected_costs.{key}.monthly is {} but its non-excluded lines sum to {}. A total that does not reconcile is a gate failure, not a rounding note.",
                fl(total),
                r2(sum)
            ),
        );
        // excluded lines must NOT be in the sum
        for sid in &excluded_ids {
            let entry = line(&by_id, sid).unwrap_or(&NULL);
            if let Some(v) = num(get(entry, field)).filter(|v| *v != 0.0) {
                l.check(
                    !close(total, sum + v, rel) || close(total, sum, rel),
                    format!(
                        "{key} total appears to include the excluded line {}",
                        rs(sid)
                    ),
                );
            }
        }
    }

    let tt = exp.at("totals");
    banded(
        l,
        "projected_costs.lift.monthly",
        get(lift, "monthly"),
        tt.at("min").json(),
        tt.at("max").json(),
        tt.at("basis").str(),
    );
    banded(
        l,
        "projected_costs.right_sized.monthly",
        get(rs_obj, "monthly"),
        tt.at("min").json(),
        tt.at("max").json(),
        tt.at("basis").str(),
    );
    for (key, obj) in [("lift", lift), ("right_sized", rs_obj)] {
        if let Some(g) = num(get(obj, "monthly")) {
            if close(g, tt.at("must_not_be").f64(), 0.01) {
                l.fail(format!(
                    "projected_costs.{key}.monthly is {}, the known-wrong total. {}",
                    fl(g),
                    tt.note("must_not_be_why")
                ));
            }
        }
    }

    // ------------ TIER 1: a rate that describes the wrong configuration ------------
    let rcm = exp.at("rate_configuration_mismatch");
    let rcm_sid = rcm.at("service_id").str();
    let rl_line = line(&by_id, rcm_sid).unwrap_or(&NULL);
    let got_rcm = get(rl_line, "rate_configuration_mismatch");
    l.check(
        matches!(got_rcm, Json::Object(_)),
        format!(
            "{} has no rate_configuration_mismatch. {}",
            rs(rcm_sid),
            rcm.note("_why")
        ),
    );
    if matches!(got_rcm, Json::Object(_)) {
        for k in rcm.at("required_keys").strs() {
            l.check(
                got_rcm.get(k).is_some(),
                format!("{rcm_sid}.rate_configuration_mismatch is missing {}", rs(k)),
            );
        }
        l.check(
            eq(get(got_rcm, "direction"), rcm.at("expected_direction").json()),
            format!(
                "{rcm_sid}: direction is {}, expected {} — a Multi-AZ rate on a single-AZ design overstates",
                rp(get(got_rcm, "direction")),
                rp(rcm.at("expected_direction").json())
            ),
        );
    }
    l.check(
        get(rl_line, "is_ceiling") == rcm.at("expected_is_ceiling").json(),
        format!(
            "{rcm_sid}.is_ceiling must be {}: the figure is an upper bound for that line even while the TOTAL is a lower bound. {}",
            ps(rcm.at("expected_is_ceiling").json()),
            rcm.note("_mixed_direction_why")
        ),
    );
    if let Some(v) = num(get(rl_line, "right_sized_monthly")) {
        for spec in lb.at("priced").arr() {
            if spec.at("service_id").str() == rcm_sid {
                if close(v, round_to(spec.at("expected").f64() / 2.0, 2), 0.02) {
                    l.fail(format!(
                        "{rcm_sid}: {} is the Multi-AZ rate halved. {}",
                        fl(v),
                        rcm.note("must_not_be_halved")
                    ));
                }
                break;
            }
        }
    }
    l.check(
        matches!(get(rl_line, "exclusion_reason"), Json::Null),
        format!(
            "{rcm_sid} was excluded. {}",
            rcm.note("must_not_be_excluded")
        ),
    );
    // Must be ONE condition naming both the service and the mismatch. Matching
    // "single-az" anywhere in conditions[] was too weak: the availability
    // condition already says "answered single-az against a ZoneRedundant
    // source", so dropping the repricing condition entirely still passed.
    let reco = or_empty(get(&est, "recommendation"));
    let conds_l: Vec<String> = list(reco, "conditions")
        .iter()
        .map(|c| ps(c).to_lowercase())
        .collect();
    let mention = rcm.at("condition_must_mention").str();
    l.check(
        conds_l
            .iter()
            .any(|c| c.contains(&rcm_sid.to_lowercase()) && c.contains(&mention.to_lowercase())),
        format!(
            "recommendation.conditions must carry ONE condition naming both {} and {} — repricing it is the highest-value correction available to this estimate and is invisible from the number. A mention of {} in some other condition does not satisfy this.",
            rs(rcm_sid),
            rs(mention),
            rs(mention)
        ),
    );

    // ------------ TIER 1: floor propagation ------------
    let fp = exp.at("floor_propagation");
    let any_excluded = breakdown.iter().any(|e| truthy(get(e, "exclusion_reason")));
    l.check(
        matches!(fp.at("expected_is_floor").json(), Json::Bool(want) if *want == any_excluded),
        format!(
            "expected {} line to carry an exclusion_reason",
            if fp.at("expected_is_floor").truthy() {
                "some"
            } else {
                "no"
            }
        ),
    );
    for (key, obj) in [("lift", lift), ("right_sized", rs_obj)] {
        l.check(
            matches!(get(obj, "is_floor"), Json::Bool(flag) if *flag == any_excluded),
            format!(
                "projected_costs.{key}.is_floor is {} but {} line is excluded. {}",
                rp(get(obj, "is_floor")),
                if any_excluded { "some" } else { "no" },
                fp.note("_why")
            ),
        );
    }

    // ------------ TIER 2 + TIER 1: the rightsizing delta ------------
    let rd = or_empty(get(cc, "rightsizing_delta"));
    let spec = exp.at("rightsizing_delta");
    if let (Some(lm), Some(rm), Some(dm)) = (
        num(get(lift, "monthly")),
        num(get(rs_obj, "monthly")),
        num(get(rd, "monthly")),
    ) {
        l.check(
            close(dm, lm - rm, rel),
            format!(
                "rightsizing_delta.monthly is {} but lift - right_sized is {}",
                fl(dm),
                r2(lm - rm)
            ),
        );
    }
    let explanation_value = get(rd, "explanation");
    let explanation = if truthy(explanation_value) {
        strip(explanation_value.as_str().unwrap_or("")).to_string()
    } else {
        String::new()
    };
    let min_length = spec.at("explanation_min_length").i64();
    l.check(
        i64::try_from(explanation.chars().count()).unwrap_or(i64::MAX) >= min_length,
        format!(
            "rightsizing_delta.explanation is {} chars, needs >= {min_length}. {}",
            explanation.chars().count(),
            spec.note("must_not_be_bare_zero")
        ),
    );
    let mention_any = spec.at("explanation_must_mention_any").strs();
    l.check(
        mention_any.iter().any(|t| explanation.to_lowercase().contains(&t.to_lowercase())),
        format!(
            "rightsizing_delta.explanation must say WHY the delta is what it is -- expected it to mention one of {}. {}",
            rl(&mention_any),
            spec.note("must_not_be_bare_zero")
        ),
    );
    let allowed_basis = spec.at("allowed_basis");
    l.check(
        allowed_basis
            .arr()
            .iter()
            .any(|b| eq(get(rd, "basis"), b.json())),
        format!(
            "rightsizing_delta.basis is {}, expected one of {}",
            rp(get(rd, "basis")),
            rp(allowed_basis.json())
        ),
    );
    if spec.at("declared_waste_must_be_recorded").truthy() {
        l.check(
            truthy(get(rd, "declared_waste_found")),
            format!(
                "rightsizing_delta.declared_waste_found is empty. The idle Windows plan with zero apps is the estate's clearest cost finding. {}",
                spec.note("declared_waste_why")
            ),
        );
    }
    // the idle plan must not ALSO be an optimization opportunity
    let opps = dumps_unicode(&Json::Array(
        list(&est, "optimization_opportunities").to_vec(),
    ));
    l.check(
        !opps.to_lowercase().contains("idle"),
        format!(
            "the idle plan appears in optimization_opportunities. {}",
            spec.note("declared_waste_must_not_be_double_counted")
        ),
    );

    // ------------ TIER 1: the licensing delta, gated on Clarify ------------
    let ls = exp.at("licensing_delta");
    let gate_path = ls.at("preferences_gate_path").str();
    let mut fired: Option<&Json> = None;
    if truthy(&prefs) {
        let value = dotted(&prefs, gate_path);
        fired = Some(value);
        l.check(
            eq(value, ls.at("expected_fired").json()),
            format!(
                "preferences {gate_path} is {}, expected {} (the fixture's own premise -- if this fails the golden inputs drifted)",
                rp(value),
                rp(ls.at("expected_fired").json())
            ),
        );
    }
    let ld = get(&est, "licensing_delta");
    let gate = match fired {
        Some(value) if !matches!(value, Json::Null) => truthy(value),
        _ => truthy(ls.at("expected_fired").json()),
    };
    if gate {
        l.check(
            !matches!(ld, Json::Null),
            format!(
                "licensing_delta is absent but preferences {gate_path} is true. {}",
                ls.note("_why")
            ),
        );
    } else {
        l.check(
            matches!(ld, Json::Null),
            format!(
                "licensing_delta is present but preferences {gate_path} is not true. {}",
                ls.note("_why")
            ),
        );
    }
    if matches!(ld, Json::Object(_)) {
        l.check(
            eq(
                get(ld, "windows_model"),
                ls.at("expected_windows_model").json(),
            ),
            format!(
                "licensing_delta.windows_model is {}, expected {}",
                rp(get(ld, "windows_model")),
                rp(ls.at("expected_windows_model").json())
            ),
        );
        l.check(
            matches!(get(ld, "monthly_delta"), Json::Null),
            format!(
                "licensing_delta.monthly_delta is {} and must be null. {}",
                rp(get(ld, "monthly_delta")),
                ls.note("monthly_delta_must_be_null_why")
            ),
        );
        l.check(
            truthy(get(ld, "delta_basis")),
            "licensing_delta.delta_basis must state why monthly_delta is null",
        );
        l.check(
            eq(get(ld, "ahub_in_use"), ls.at("expected_ahub_in_use").json()),
            format!(
                "licensing_delta.ahub_in_use is {}, expected {}",
                rp(get(ld, "ahub_in_use")),
                rp(ls.at("expected_ahub_in_use").json())
            ),
        );
        let blk = ls.at("blockers_must_not_be_priced");
        let code = blk.at("code").str();
        let recorded = list(ld, "blockers_are_not_costs")
            .iter()
            .any(|entry| entry.as_str().is_some_and(|text| text.contains(code)));
        if !recorded {
            l.fail(format!(
                "licensing_delta must record the {code} blocker as a non-cost. {}",
                blk.note("_why")
            ));
        }
        // a blocker must never carry a dollar figure
        for entry in list(ld, "blockers_are_not_costs") {
            l.check(
                !ps(entry).contains('$'),
                format!(
                    "a blocker entry carries a dollar figure: {}. {}",
                    rp(entry),
                    blk.note("_why")
                ),
            );
        }
    }

    // ------------ TIER 1: the baseline rung ------------
    let bs = exp.at("baseline");
    let cur_costs = or_empty(get(&est, "current_costs"));
    let allowed_sources = bs.at("allowed_sources");
    l.check(
        allowed_sources
            .arr()
            .iter()
            .any(|s| eq(get(cur_costs, "source"), s.json())),
        format!(
            "current_costs.source is {}, expected one of {}",
            rp(get(cur_costs, "source")),
            rp(allowed_sources.json())
        ),
    );
    l.check(
        eq(get(cur_costs, "source"), bs.at("expected_source").json()),
        format!(
            "current_costs.source is {}, expected {}",
            rp(get(cur_costs, "source")),
            rp(bs.at("expected_source").json())
        ),
    );
    if bs.at("baseline_note_required").truthy()
        && !matches!(get(cur_costs, "source"), Json::String(s) if s == "cost_management_export")
    {
        l.check(
            truthy(get(cur_costs, "baseline_note")),
            "current_costs.baseline_note is required for a non-invoice baseline",
        );
    }
    let source_path = bs.at("preferences_source_path").str();
    if truthy(&prefs) {
        let want = num(dotted(&prefs, source_path));
        let got = num(get(cur_costs, "azure_monthly"));
        if let (Some(want), Some(got)) = (want, got) {
            l.check(
                close(got, want, 1e-6),
                format!(
                    "current_costs.azure_monthly is {} but preferences {source_path} is {}",
                    fl(got),
                    fl(want)
                ),
            );
        }
    }

    // ------------ TIER 1: the reservation rule is present and vacuous ------------
    let rr = exp.at("reservation_rule");
    let required = rr.at("required_path").str();
    let subs = get(&est, required);
    l.check(
        matches!(subs, Json::Array(_)),
        format!(
            "{required} must be present as an array. {}",
            rr.note("_why")
        ),
    );
    if let Json::Array(values) = subs {
        l.check(
            i64::try_from(values.len()).unwrap_or(i64::MAX) == rr.at("expected_length").i64(),
            format!(
                "{required} has {} entr(ies), expected {}. {}",
                values.len(),
                ps(rr.at("expected_length").json()),
                rr.note("_why")
            ),
        );
    }

    // ------------ TIER 1: region and staleness ------------
    let rg = exp.at("region");
    let ps_obj = or_empty(get(&est, "pricing_source"));
    let dtr = if truthy(&design) {
        dotted(&design, rg.at("design_target_region_path").str())
    } else {
        &NULL
    };
    if truthy(dtr) {
        l.check(
            eq(dtr, rg.at("expected_target_region").json()),
            format!(
                "design {} is {}, expected {} (fixture premise -- the golden inputs drifted if this fails)",
                rg.at("design_target_region_path").str(),
                rp(dtr),
                rp(rg.at("expected_target_region").json())
            ),
        );
    }
    l.check(
        matches!(get(ps_obj, "region_mismatch"), Json::Bool(true)),
        format!(
            "pricing_source.region_mismatch must be true. {}",
            rg.note("_why")
        ),
    );
    let blob = dumps_unicode(&est);
    l.check(
        blob.contains(rg.at("expected_cache_region").str()),
        format!(
            "the artifact must state the cache region {}",
            rp(rg.at("expected_cache_region").json())
        ),
    );
    l.check(
        blob.contains(rg.at("expected_target_region").str()),
        format!(
            "the artifact must state the target region {}",
            rp(rg.at("expected_target_region").json())
        ),
    );

    let st = exp.at("staleness");
    let allowed_status = st.at("allowed_status");
    l.check(
        allowed_status
            .arr()
            .iter()
            .any(|s| eq(get(ps_obj, "status"), s.json())),
        format!(
            "pricing_source.status is {}, expected one of {}",
            rp(get(ps_obj, "status")),
            rp(allowed_status.json())
        ),
    );
    l.check(
        eq(get(ps_obj, "status"), st.at("expected_status").json()),
        format!(
            "pricing_source.status is {}, expected {}. {}",
            rp(get(ps_obj, "status")),
            rp(st.at("expected_status").json()),
            st.note("_why")
        ),
    );

    // ------------ TIER 1: recommendation ------------
    let rc = exp.at("recommendation");
    let allowed_outcomes = rc.at("allowed_outcomes");
    l.check(
        allowed_outcomes
            .arr()
            .iter()
            .any(|o| eq(get(reco, "outcome"), o.json())),
        format!(
            "recommendation.outcome is {}, expected one of {}",
            rp(get(reco, "outcome")),
            rp(allowed_outcomes.json())
        ),
    );
    l.check(
        eq(get(reco, "outcome"), rc.at("expected_outcome").json()),
        format!(
            "recommendation.outcome is {}, expected {}. {}",
            rp(get(reco, "outcome")),
            rp(rc.at("expected_outcome").json()),
            rc.note("must_not_be_why")
        ),
    );
    let conds = list(reco, "conditions");
    if matches!(get(reco, "outcome"), Json::String(s) if s == "conditional_go") {
        let min = rc.at("conditions_min_length").i64();
        l.check(
            i64::try_from(conds.len()).unwrap_or(i64::MAX) >= min,
            format!(
                "recommendation.conditions has {} entr(ies), expected >= {min} for a conditional_go",
                conds.len()
            ),
        );
        let cond_blob = dumps_unicode(&Json::Array(conds.to_vec())).to_lowercase();
        let any = rc.at("conditions_must_mention_any").strs();
        l.check(
            any.iter().any(|t| cond_blob.contains(&t.to_lowercase())),
            format!(
                "conditions must name the unpriced/floor problem -- expected one of {}",
                rl(&any)
            ),
        );
        let region_mention = rg.at("condition_must_mention").str();
        l.check(
            cond_blob.contains(&region_mention.to_lowercase()),
            format!(
                "conditions must carry the region mismatch (expected {}). {}",
                rs(region_mention),
                rg.note("_why")
            ),
        );
    }
    if rc.at("stay_requires_path_stay").truthy()
        && matches!(get(reco, "outcome"), Json::String(s) if s == "stay")
    {
        l.check(
            matches!(get(reco, "path"), Json::String(s) if s == "stay"),
            "outcome 'stay' only ever accompanies path 'stay'",
        );
    }
    l.check(
        i64::try_from(list(reco, "would_flip_if").len()).unwrap_or(i64::MAX)
            >= rc.at("would_flip_if_min_length").i64(),
        "recommendation.would_flip_if must name at least one change that would alter the outcome",
    );
    let basis = or_empty(get(reco, "decision_basis"));
    for k in rc.at("decision_basis_required_keys").strs() {
        l.check(
            basis.get(k).is_some(),
            format!("recommendation.decision_basis is missing {}", rs(k)),
        );
    }

    // ------------ TIER 1: complexity ------------
    let cx = exp.at("complexity");
    let allowed_tiers = cx.at("allowed_tiers");
    l.check(
        allowed_tiers
            .arr()
            .iter()
            .any(|t| eq(get(&est, "complexity_tier"), t.json())),
        format!(
            "complexity_tier is {}, expected one of {}",
            rp(get(&est, "complexity_tier")),
            rp(allowed_tiers.json())
        ),
    );
    l.check(
        eq(get(&est, "complexity_tier"), cx.at("expected_tier").json()),
        format!(
            "complexity_tier is {}, expected {}. {}",
            rp(get(&est, "complexity_tier")),
            rp(cx.at("expected_tier").json()),
            cx.note("_why")
        ),
    );
    if cx.at("inputs_required").truthy() {
        l.check(
            truthy(get(&est, "complexity_inputs")),
            "complexity_inputs must record what the tier was derived from",
        );
    }

    // ------------ TIER 1: accounting -- every designed service, exactly once ------------
    let ac = exp.at("accounting");
    let dsvc: Vec<&Json> = list(&design, "services")
        .iter()
        .map(|s| get(s, "service_id"))
        .collect();
    if truthy(&design) {
        let expected = ac.at("expected_design_service_count").i64();
        l.check(
            i64::try_from(dsvc.len()).unwrap_or(i64::MAX) == expected,
            format!(
                "the design has {} services[], expected {expected} (fixture premise -- the golden inputs drifted if this fails)",
                dsvc.len()
            ),
        );
        for sid in &dsvc {
            l.check(
                by_id.iter().any(|(key, _)| *key == ps(sid)),
                format!(
                    "designed service {} has no line in the cost breakdown. {}",
                    rp(sid),
                    ac.note("_why")
                ),
            );
        }
    }
    let line_count = ac.at("expected_breakdown_line_count").i64();
    l.check(
        i64::try_from(breakdown.len()).unwrap_or(i64::MAX) == line_count,
        format!(
            "breakdown has {} lines, expected {line_count}. {}",
            breakdown.len(),
            ac.note("_line_count_why")
        ),
    );
    let dm_spec = ac.at("deferred_must_have_no_cost_line");
    for d in list(&design, "deferred") {
        if eq(get(d, "azure_type"), dm_spec.at("azure_type").json()) {
            l.check(
                d.get("confidence").is_none(),
                "a deferred entry must not carry confidence",
            );
            break;
        }
    }

    // ------------ TIER 1: run_mode, the consent gate ------------
    let rmspec = exp.at("run_mode");
    let rm = get(&ph, "run_mode");
    l.check(
        !matches!(rm, Json::Null),
        format!(
            "run_mode is absent from .phase-status.json. {}",
            rmspec.note("_why")
        ),
    );
    let allowed_modes = rmspec.at("allowed");
    l.check(
        allowed_modes.arr().iter().any(|m| eq(rm, m.json())),
        format!(
            "run_mode is {}, expected one of {}",
            rp(rm),
            rp(allowed_modes.json())
        ),
    );
    l.check(
        eq(rm, rmspec.at("expected").json()),
        format!(
            "run_mode is {}, expected {} for {}. {}",
            rp(rm),
            rp(rmspec.at("expected").json()),
            ps(rmspec.at("golden_branch").json()),
            rmspec.note("must_not_be_decide_and_execute_why")
        ),
    );
    l.check(
        eq(
            get(&ph, "current_phase"),
            rmspec.at("expected_current_phase").json(),
        ),
        format!(
            "current_phase is {}, expected {}",
            rp(get(&ph, "current_phase")),
            rp(rmspec.at("expected_current_phase").json())
        ),
    );
    let generate = path(&ph, &["phases", "generate"]);
    l.check(
        eq(generate, rmspec.at("expected_generate_phase").json()),
        format!(
            "phases.generate is {}, expected {}. {}",
            rp(generate),
            rp(rmspec.at("expected_generate_phase").json()),
            rmspec.note("_why_generate_pending")
        ),
    );

    // ------------ TIER 1: the closed warning vocabulary ------------
    let wv = exp.at("warning_vocabulary");
    let source_rel = wv.at("source_file_relpath").str();
    let must_not_be = wv.note("must_not_be");
    let mut declared: Vec<String> = Vec::new();
    if let Some(source_text) = context.read_relative(source_rel) {
        // read the codes out of the skill file's table, rather than restating them
        declared = warning_codes(&source_text);
        l.check(
            !declared.is_empty(),
            format!("could not read any warning codes from {source_rel}"),
        );
    } else {
        l.fail(format!(
            "cannot reach {source_rel} to read the closed vocabulary"
        ));
    }
    let warnings = list(&est, "warnings");
    for w in warnings {
        if !matches!(w, Json::Object(_)) {
            let shown: String = ps(w).chars().take(60).collect();
            l.fail(format!(
                "warnings[] entry is a bare string: {}. {must_not_be}",
                rs(&shown)
            ));
            continue;
        }
        let code = get(w, "code");
        l.check(
            truthy(code),
            format!("a warnings[] entry has no code. {must_not_be}"),
        );
        if truthy(code) && !declared.is_empty() {
            l.check(
                declared.iter().any(|d| *d == ps(code)),
                format!(
                    "warning code {} is not a row in estimate-infra.md's closed vocabulary. {must_not_be}",
                    rp(code)
                ),
            );
        }
    }
    for code in wv.at("expected_codes_present").strs() {
        l.check(
            warnings.iter().any(|w| {
                matches!(w, Json::Object(_))
                    && matches!(get(w, "code"), Json::String(c) if c == code)
            }),
            format!(
                "expected a {} warning on this estate and found none",
                rs(code)
            ),
        );
    }

    // ------------ TIER 1: compliance was never asked ------------
    let cna = exp.at("compliance_never_asked");
    let inputs = or_empty(get(&est, "complexity_inputs"));
    let absent = Json::string("__absent__");
    let got_comp = inputs.get("compliance").unwrap_or(&absent);
    l.check(
        matches!(got_comp, Json::Null),
        format!(
            "complexity_inputs.compliance is {} and must be null. {}",
            rp(got_comp),
            cna.note("must_not_be_why")
        ),
    );

    // ------------ TIER 1: forbidden content ------------
    let fb = exp.at("forbidden");
    for s in fb.at("substrings").strs() {
        l.check(
            !blob.contains(s),
            format!(
                "forbidden substring {} appears in the artifact. {}",
                rs(s),
                fb.note("_app_runner_why")
            ),
        );
    }
    let ng = fb.at("no_graviton_opportunity");
    for o in list(&est, "optimization_opportunities") {
        l.check(
            !ng.at("forbidden_opportunity_types")
                .arr()
                .iter()
                .any(|t| eq(get(o, "type"), t.json())),
            format!(
                "optimization_opportunities carries a {} entry. {}",
                rp(get(o, "type")),
                ng.note("_why")
            ),
        );
    }
    for s in ng.at("forbidden_substrings_in_opportunities").strs() {
        l.check(
            !opps.contains(s),
            format!(
                "{} appears in optimization_opportunities. {}",
                rs(s),
                ng.note("_why")
            ),
        );
    }
    let nl = fb.at("no_labor_costs");
    let cost_doc = Json::Object(vec![
        (
            "migration_cost_considerations".to_string(),
            get(&est, "migration_cost_considerations").clone(),
        ),
        ("projected_costs".to_string(), pc.clone()),
        ("cost_comparison".to_string(), cc.clone()),
        (
            "roi_analysis".to_string(),
            get(&est, "roi_analysis").clone(),
        ),
    ]);
    let cost_raw = dumps_unicode(&scrub(&cost_doc));
    for s in nl.at("forbidden_substrings_in_costs").strs() {
        // An ALL-CAPS term is an acronym: match it whole-word and
        // case-SENSITIVELY. A case-insensitive substring match on "FTE" hits
        // "aFTEr", "oFTEn" and "liFTEd", which fails correct artifacts for
        // containing ordinary English.
        let hit = if is_upper(s) {
            has_whole_word(&cost_raw, s)
        } else {
            cost_raw.to_lowercase().contains(&s.to_lowercase())
        };
        l.check(
            !hit,
            format!("{} appears in a cost section. {}", rs(s), nl.note("_why")),
        );
    }

    // ------------ TIER 2: the remaining relational identities ------------
    let az = num(get(cur_costs, "azure_monthly"));
    for (key, obj, total) in [
        ("lift", or_empty(get(cc, "lift")), lift),
        ("right_sized", or_empty(get(cc, "right_sized")), rs_obj),
    ] {
        let aws = num(get(obj, "aws_monthly"));
        let md = num(get(obj, "monthly_difference"));
        let ad = num(get(obj, "annual_difference"));
        let src = num(get(total, "monthly"));
        if let (Some(aws), Some(src)) = (aws, src) {
            l.check(
                close(aws, src, rel),
                format!(
                    "cost_comparison.{key}.aws_monthly {} != projected_costs.{key}.monthly {}",
                    fl(aws),
                    fl(src)
                ),
            );
        }
        if let (Some(aws), Some(az), Some(md)) = (aws, az, md) {
            l.check(
                close(md, aws - az, rel),
                format!(
                    "cost_comparison.{key}.monthly_difference is {}, expected aws - azure = {} (sign convention: difference = AWS minus Azure, so negative means AWS cheaper)",
                    fl(md),
                    r2(aws - az)
                ),
            );
        }
        if let (Some(md), Some(ad)) = (md, ad) {
            l.check(
                close(ad, md * 12.0, rel),
                format!(
                    "cost_comparison.{key}.annual_difference is {}, expected {}",
                    fl(ad),
                    r2(md * 12.0)
                ),
            );
        }
    }

    let fin = or_empty(get(&est, "financial_summary"));
    let rsd = num(path(cc, &["right_sized", "monthly_difference"]));
    let sav = num(get(fin, "monthly_savings_right_sized"));
    if let (Some(rsd), Some(sav)) = (rsd, sav) {
        l.check(
            close(sav, -rsd, rel),
            format!(
                "financial_summary.monthly_savings_right_sized is {} but cost_comparison right_sized monthly_difference is {}. financial_summary uses savings = Azure - AWS, the OPPOSITE sign; they must be negations of each other.",
                fl(sav),
                fl(rsd)
            ),
        );
    }

    // per-cluster sums to the right-sized total, and every cluster_id is real
    let per_cluster = list(cc, "per_cluster");
    if !per_cluster.is_empty() {
        let mut sum = 0.0;
        let design_cluster_ids: Vec<String> = list(&design, "clusters")
            .iter()
            .map(|c| ps(get(c, "cluster_id")))
            .collect();
        for entry in per_cluster {
            let Some(v) = num(get(entry, "right_sized_monthly")) else {
                l.fail(format!(
                    "per_cluster entry {} has a non-numeric right_sized_monthly",
                    rp(get(entry, "cluster_id"))
                ));
                continue;
            };
            sum += v;
            // estimate-infra.md: a $0.00 cluster figure needs its note, because
            // a cluster whose every line was excluded reads as free and is not.
            if v == 0.0 {
                let note = get(entry, "note");
                l.check(
                    truthy(note) && !strip(note.as_str().unwrap_or("x")).is_empty(),
                    format!(
                        "per_cluster {} is $0.00 with no note. A cluster whose every line was excluded is UNPRICED, not free, and the number alone cannot say which.",
                        rp(get(entry, "cluster_id"))
                    ),
                );
            }
            let cid = get(entry, "cluster_id");
            if !matches!(cid, Json::Null) && !design_cluster_ids.is_empty() {
                l.check(
                    design_cluster_ids.contains(&ps(cid)),
                    format!(
                        "per_cluster cluster_id {} does not appear in the design's clusters[]. Checked by MEMBERSHIP, not against literal strings, because cluster_id has no stability specification yet.",
                        rp(cid)
                    ),
                );
            }
        }
        if let Some(total_rs) = num(get(rs_obj, "monthly")) {
            l.check(
                close(sum, total_rs, rel),
                format!(
                    "per_cluster right_sized_monthly sums to {} but projected_costs.right_sized.monthly is {}. Every priced line belongs to exactly one cluster, or to the unattributed estate-wide entry.",
                    r2(sum),
                    fl(total_rs)
                ),
            );
        }
    }

    // observability components must sum to its own figure
    if let Some(obs) = line(&by_id, "observability-cloudwatch") {
        let comps = get(obs, "components");
        if matches!(comps, Json::Object(_)) {
            let cs: f64 = members(comps)
                .iter()
                .map(|(_, v)| num(v).unwrap_or(0.0))
                .sum();
            if let Some(ov) = num(get(obs, "right_sized_monthly")) {
                l.check(
                    close(cs, ov, 1e-2),
                    format!(
                        "observability components sum to {} but the line is {}",
                        r2(cs),
                        fl(ov)
                    ),
                );
            }
        }
    }
    Some(())
}

/// `re.findall(r"^\| `([a-z0-9_]+)` +\|", text, re.M)`.
fn warning_codes(text: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for line in text.split('\n') {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let width: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .count();
        if width == 0 {
            continue;
        }
        let Some(after) = rest[width..].strip_prefix('`') else {
            continue;
        };
        let spaces = after.len() - after.trim_start_matches(' ').len();
        if spaces > 0 && after[spaces..].starts_with('|') {
            codes.push(rest[..width].to_string());
        }
    }
    codes
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn num_tolerates_formatted_strings_and_rejects_bools() {
        assert_eq!(num(&Json::string("$1,234.50 ")), Some(1234.5));
        assert_eq!(num(&Json::string("n/a")), None);
        assert_eq!(num(&Json::Bool(true)), None);
        assert_eq!(num(&Json::Null), None);
        assert_eq!(num(&Json::Int(3)), Some(3.0));
    }

    #[test]
    fn acronyms_match_whole_words_case_sensitively() {
        assert!(is_upper("FTE"));
        assert!(!is_upper("engineer-weeks"));
        assert!(!is_upper("123"));
        assert!(has_whole_word("3 FTE for a month", "FTE"));
        assert!(!has_whole_word("often liFTEd after", "FTE"));
        assert!(!has_whole_word("aFTEr", "FTE"));
    }

    #[test]
    fn relational_closeness_scales_with_magnitude() {
        assert!(close(1000.0, 1000.0005, 1e-6));
        assert!(!close(1000.0, 1000.01, 1e-6));
        assert!(close(0.0, 5e-7, 1e-6));
    }

    #[test]
    fn warning_codes_are_read_from_the_table_rows() {
        let table =
            "| code | meaning |\n| `a_code`  | x |\n| `Bad` | y |\n|`none` | z |\n| `b1` | w |\n";
        assert_eq!(warning_codes(table), vec!["a_code", "b1"]);
    }
}
