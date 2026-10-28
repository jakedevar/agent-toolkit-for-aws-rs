//! The azure-to-aws Clarify asserter (`check_expected_clarify.py` and its two
//! thin wrappers).
//!
//! Clarify is the one phase the capability-test method cannot reach: it is
//! interactive, so `clarify-answers.json` substitutes for the user and this
//! asserter checks what a scripted user can check, the assumption sheet's
//! BRANCHING: which rows fire, which are ESSENTIAL versus PROPOSED, which are
//! N/A, and the provenance lists. It does not test wording, batching or tone.
//!
//! One program serves three expectation sets: `expected-clarify.json` (the
//! scripted user declines to state spend, so an ESSENTIAL row is null and the
//! phase must gate), `expected-clarify-complete.json` (spend answered, the
//! phase completes, and a conflicting answer is recorded as given) and
//! `expected-clarify-fast-path.json` (an eligible estate on the fast path).

use std::collections::BTreeSet;

use toolkit_core::Json;

use super::super::ledger::{
    dotted, dumps, eq, get, items, number, objs, path, ps, rl, rp, rs, truthy, Spec,
};
use super::super::{Context, Flow};

const RELATIONAL_TYPES: [&str; 3] = [
    "Microsoft.DBforPostgreSQL/flexibleServers",
    "Microsoft.DBforMySQL/flexibleServers",
    "Microsoft.Sql/servers/databases",
];
const GIB_MB: f64 = 1024.0;
const DUMP_RESTORE_MAX_GIB: f64 = 100.0;

pub(in crate::startup::fixtures) fn blocked(context: &Context<'_>) -> Flow {
    run(context, "expected-clarify.json")
}

pub(in crate::startup::fixtures) fn complete(context: &Context<'_>) -> Flow {
    run(context, "expected-clarify-complete.json")
}

pub(in crate::startup::fixtures) fn fast_path(context: &Context<'_>) -> Flow {
    run(context, "expected-clarify-fast-path.json")
}

/// A `^([^\[\]]+)\[(\d+)\]$` path segment: the array name and the index.
fn index_segment(part: &str) -> Option<(&str, usize)> {
    let body = part.strip_suffix(']')?;
    let (name, digits) = body.split_once('[')?;
    if name.is_empty()
        || name.contains(['[', ']'])
        || digits.is_empty()
        || !digits.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((name, digits.parse().ok()?))
}

/// `data.availability` -> that row, or `None`.
///
/// Array rows use the index notation `questions_defaulted[]` records them
/// under (`app_service_plans[0].isolation_split`); one lookup resolves either
/// shape.
fn row<'a>(prefs: &'a Json, dotted_key: &str) -> Option<&'a Json> {
    let mut node = prefs;
    for part in dotted_key.split('.') {
        if let Some((name, index)) = index_segment(part) {
            if !matches!(node, Json::Object(_)) {
                return None;
            }
            node = items(get(node, name)).get(index)?;
            continue;
        }
        if !matches!(node, Json::Object(_)) {
            return None;
        }
        node = get(node, part);
    }
    matches!(node, Json::Object(_)).then_some(node)
}

/// `(default, size_coverage)` of Q-D2's three-branch rule, as a pure
/// function. The boundary is 100 GiB INCLUSIVE on the `dump_restore` side; with
/// no measured size the default is `dump_restore` with coverage `unknown`; a mix
/// of measured and unmeasured servers applies the rule to the measured maximum
/// with coverage `partial`.
fn expected_db_cutover_default(sizes: &[Option<f64>]) -> (&'static str, &'static str) {
    let measured: Vec<f64> = sizes.iter().flatten().copied().collect();
    if measured.is_empty() {
        return ("dump_restore", "unknown");
    }
    let coverage = if measured.len() == sizes.len() {
        "complete"
    } else {
        "partial"
    };
    let largest_gib = measured.iter().copied().fold(f64::MIN, f64::max) / GIB_MB;
    let default = if largest_gib <= DUMP_RESTORE_MAX_GIB {
        "dump_restore"
    } else {
        "dms"
    };
    (default, coverage)
}

fn check_dispositions(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    for (dotted_key, spec) in exp.at("row_dispositions").real_entries() {
        let why = spec.note("why");
        let Some(r) = row(prefs, dotted_key) else {
            l.fail(format!("{}: no row at all. {why}", rs(dotted_key)));
            continue;
        };
        let want = spec.at("disposition");
        l.check(
            eq(get(r, "disposition"), want.json()),
            format!(
                "{}: disposition is {}, expected {}. {why}",
                rs(dotted_key),
                rp(get(r, "disposition")),
                rp(want.json())
            ),
        );
        if spec.has("value") {
            l.check(
                eq(get(r, "value"), spec.at("value").json()),
                format!(
                    "{}: value is {}, expected {}. {why}",
                    rs(dotted_key),
                    rp(get(r, "value")),
                    rp(spec.at("value").json())
                ),
            );
        }
        for bad in spec.get("must_not_be_disposition").arr_or_empty() {
            l.check(
                !eq(get(r, "disposition"), bad.json()),
                format!(
                    "{}: disposition is {}, the plausible-but-wrong answer. {why}",
                    rs(dotted_key),
                    rp(bad.json())
                ),
            );
        }
        for key in spec.get("requires_keys").arr_or_empty() {
            l.check(
                truthy(get(r, key.str())),
                format!(
                    "{}: has no {}. An ESSENTIAL question without its context is unanswerable, and an N/A without a reason is indistinguishable from an omission.",
                    rs(dotted_key),
                    rs(key.str())
                ),
            );
        }
    }
}

/// A user answer a `hard_blocker` suppresses is recorded AS GIVEN, and does not
/// gate.
fn check_conflicting_answer(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("conflicting_answer");
    if !spec.truthy() {
        return;
    }
    let why = spec.note("_why");
    let row_key = spec.at("row").str();
    let Some(r) = row(prefs, row_key) else {
        l.fail(format!(
            "no row at {} to check the conflicting answer. {why}",
            rs(row_key)
        ));
        return;
    };
    let expected = spec.at("expected_value");
    l.check(
        eq(get(r, "value"), expected.json()),
        format!(
            "{row_key}: value is {}, expected {}. The answer is recorded AS GIVEN — silently substituting a different one hides a decision the customer has to make. {why}",
            rp(get(r, "value")),
            rp(expected.json())
        ),
    );
    for key in spec.get("requires_keys").arr_or_empty() {
        l.check(
            truthy(get(r, key.str())),
            format!(
                "{row_key}: has no {}. Recording the answer without recording what suppresses it leaves the conflict invisible. {why}",
                rs(key.str())
            ),
        );
    }
    let blockers_at = spec.at("blocker_code_in").str();
    let blockers = items(dotted(prefs, blockers_at));
    let codes: BTreeSet<String> = blockers
        .iter()
        .filter(|blocker| matches!(blocker, Json::Object(_)))
        .map(|blocker| ps(get(blocker, "code")))
        .collect();
    let want_code = spec.at("expected_blocker_code");
    l.check(
        codes.contains(&ps(want_code.json())),
        format!(
            "{blockers_at} has no {} entry (got {}). The blocker is a PREREQUISITE and must be recorded even though it does not change the answer.",
            rp(want_code.json()),
            rl(&codes.iter().cloned().collect::<Vec<_>>())
        ),
    );
    let remain = spec.at("status_must_remain");
    l.check(
        eq(get(prefs, "clarify_status"), remain.json()),
        format!(
            "clarify_status is {}, expected {}. A conflicting answer does NOT gate the phase.",
            rp(get(prefs, "clarify_status")),
            rp(remain.json())
        ),
    );
}

fn join_path(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

fn collect_unanswered(node: &Json, at: &str, found: &mut Vec<String>) {
    match node {
        Json::Object(members) => {
            if matches!(get(node, "disposition"), Json::String(s) if s == "ESSENTIAL") {
                if matches!(get(node, "value"), Json::Null) {
                    found.push(at.to_string());
                }
                return;
            }
            for (key, value) in members {
                collect_unanswered(value, &join_path(at, key), found);
            }
        }
        Json::Array(values) => {
            for (n, value) in values.iter().enumerate() {
                collect_unanswered(value, &format!("{at}[{n}]"), found);
            }
        }
        _ => {}
    }
}

fn check_essential_gate(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("essential_gate");
    let mut unanswered = Vec::new();
    collect_unanswered(prefs, "", &mut unanswered);
    unanswered.sort();
    let mut expected: Vec<String> = spec
        .at("expected_unanswered")
        .strs()
        .into_iter()
        .map(str::to_string)
        .collect();
    expected.sort();
    l.check(
        unanswered == expected,
        format!(
            "unanswered ESSENTIAL rows are {}, expected {}. {}",
            rl(&unanswered),
            rl(&expected),
            spec.note("_why")
        ),
    );
    let want = spec.at("expected_status");
    l.check(
        eq(get(prefs, "clarify_status"), want.json()),
        format!(
            "clarify_status is {}, expected {}. {}",
            rp(get(prefs, "clarify_status")),
            rp(want.json()),
            spec.note("_why")
        ),
    );
    l.note(format!(
        "essential gate: {} unanswered — phase must GATE_FAIL",
        unanswered.len()
    ));
}

fn collect_promoted(node: &Json, at: &str, found: &mut Vec<String>) {
    match node {
        Json::Object(members) => {
            if node.get("disposition").is_some() && node.get("default").is_some() {
                if matches!(get(node, "disposition"), Json::String(s) if s == "DETECTED")
                    && eq(get(node, "value"), get(node, "default"))
                    && !matches!(get(node, "default"), Json::Null)
                    && !truthy(get(node, "source"))
                    && !truthy(get(node, "forced_by"))
                {
                    found.push(at.to_string());
                }
                return;
            }
            for (key, value) in members {
                collect_promoted(value, &join_path(at, key), found);
            }
        }
        Json::Array(values) => {
            for (n, value) in values.iter().enumerate() {
                collect_promoted(value, &format!("{at}[{n}]"), found);
            }
        }
        _ => {}
    }
}

/// A value equal to its default must still read PROPOSED.
fn check_defaults_not_promoted(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let spec = exp.at("defaults_not_promoted");
    let mut offenders = Vec::new();
    collect_promoted(prefs, "", &mut offenders);
    context.ledger.check(
        offenders.is_empty(),
        format!(
            "{} are DETECTED with value == default and carry no `source`. The key is `source` for a DETECTED row and `forced_by` when a blocker removed the choice — see schema-preferences.md. NOT `note`, `reason`, `mapped_from` or `detail`: capability run 5 used `note` for nine rows whose content was entirely correct and this failed on the key NAME, because nothing specified it. {}",
            rl(&offenders),
            spec.note("_why")
        ),
    );
}

/// Keyed on `config.tf_address` when the inventory carries one (`IaC` source),
/// else on `azure_id` (live source, where there is no Terraform address).
fn check_isolation_rows(context: &Context<'_>, prefs: &Json, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("isolation_rows");
    let by_id: Vec<(&Json, String)> = objs(get(inv, "resources"))
        .map(|resource| {
            let address = path(resource, &["config", "tf_address"]);
            let key = if truthy(address) {
                ps(address)
            } else {
                ps(get(resource, "azure_id"))
            };
            (get(resource, "azure_id"), key)
        })
        .collect();
    let lookup = |azure_id: &Json| -> String {
        by_id
            .iter()
            .rev()
            .find(|(id, _)| eq(id, azure_id))
            .map_or_else(|| "None".to_string(), |(_, key)| key.clone())
    };
    let got: Vec<(String, &Json, Json)> = objs(get(prefs, "app_service_plans"))
        .map(|plan| {
            let split = get(plan, "isolation_split");
            let split = if truthy(split) {
                split
            } else {
                &super::super::ledger::NULL
            };
            (
                lookup(get(plan, "plan_azure_id")),
                get(plan, "hosted_app_count"),
                get(split, "disposition").clone(),
            )
        })
        .collect();
    for (addr, want) in spec.at("expected").real_entries() {
        let Some((_, count, disposition)) = got.iter().rev().find(|(key, _, _)| key == addr) else {
            l.fail(format!(
                "no app_service_plans[] row for {}. {}",
                rs(addr),
                spec.note("_why")
            ));
            continue;
        };
        l.check(
            eq(count, want.at("hosted_app_count").json()),
            format!(
                "{}: hosted_app_count is {}, expected {}",
                rs(addr),
                rp(count),
                rp(want.at("hosted_app_count").json())
            ),
        );
        l.check(
            eq(disposition, want.at("disposition").json()),
            format!(
                "{} (hosts {}): isolation disposition is {}, expected {}. {}",
                rs(addr),
                ps(count),
                rp(disposition),
                rp(want.at("disposition").json()),
                want.note("why")
            ),
        );
    }
}

fn check_licensing_firing(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("licensing_firing");
    let lic_raw = get(prefs, "licensing");
    let lic = if truthy(lic_raw) {
        lic_raw
    } else {
        &super::super::ledger::NULL
    };
    let expected_fired = spec.at("expected_fired");
    l.check(
        eq(get(lic, "_fired"), expected_fired.json()),
        format!(
            "licensing._fired is {}, expected {}. {}",
            rp(get(lic, "_fired")),
            rp(expected_fired.json()),
            spec.note("_why")
        ),
    );
    if matches!(expected_fired.json(), Json::Bool(false)) {
        // The category did not fire: clarify-assemble.md § Assembly rule 3
        // requires an EXPLICIT N/A stub with a reason, not an absent key.
        l.check(
            matches!(get(lic, "disposition"), Json::String(s) if s == "N/A")
                && truthy(get(lic, "reason")),
            format!(
                "licensing did not fire but is not written as an explicit N/A with a reason (got disposition={}, reason={}). An absent key and a considered N/A are different facts.",
                rp(get(lic, "disposition")),
                rp(get(lic, "reason"))
            ),
        );
        return;
    }
    l.check(
        truthy(get(lic, "_firing_reason")),
        "licensing fired with no _firing_reason — which VM or SQL resource triggered it must be recorded, or the user cannot tell why they are being asked",
    );
    let code = spec.at("expected_blocker_code");
    let hit: Vec<&Json> = items(get(lic, "blockers"))
        .iter()
        .filter(|blocker| eq(get(blocker, "code"), code.json()))
        .collect();
    l.check(
        !hit.is_empty(),
        format!(
            "no {} blocker. {}",
            rp(code.json()),
            spec.note("_blocker_why")
        ),
    );
    if let Some(first) = hit.first() {
        l.check(
            matches!(get(first, "severity"), Json::String(s) if s == "blocker"),
            format!(
                "the Azure Edition entry has severity {}, expected 'blocker' — MGN refuses the image, so it is not advisory",
                rp(get(first, "severity"))
            ),
        );
    }
}

/// `preferences.json` `metadata.clarify_mode` (clarify-assemble.md § Assembly
/// rule 0). This corpus is ineligible for the Step 0.5 fast path, so a run
/// that reports `fast_path` here either ignored Discover's verdict or
/// re-derived eligibility itself.
fn check_clarify_mode(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("clarify_mode");
    let meta = get(prefs, "metadata");
    let expected = spec.at("expected");
    l.check(
        eq(get(meta, "clarify_mode"), expected.json()),
        format!(
            "metadata.clarify_mode is {}, expected {}. {}",
            rp(get(meta, "clarify_mode")),
            rp(expected.json()),
            spec.note("_why")
        ),
    );
    let want_eligible = spec.at("expected_fast_path_eligible");
    l.check(
        get(meta, "fast_path_eligible") == want_eligible.json(),
        format!(
            "metadata.fast_path_eligible is {}, expected {} — it must be copied from the inventory's metadata.clarify_fast_path.eligible, not re-decided by Clarify",
            rp(get(meta, "fast_path_eligible")),
            rp(want_eligible.json())
        ),
    );
    l.check(
        matches!(get(meta, "questions_defaulted"), Json::Array(_)),
        format!(
            "metadata.questions_defaulted is {}, expected a list (possibly empty) of dotted row keys that took their documented default",
            rp(get(meta, "questions_defaulted"))
        ),
    );
    let want_deferred = spec.get("expected_deferred_to_generate");
    if !want_deferred.is_null() {
        let deferred = get(meta, "deferred_to_generate");
        let mut got: Vec<String> = items(deferred).iter().map(ps).collect();
        got.sort();
        let mut want: Vec<String> = want_deferred
            .arr()
            .iter()
            .map(|entry| ps(entry.json()))
            .collect();
        want.sort();
        l.check(
            got == want,
            format!(
                "metadata.deferred_to_generate is {}, expected {}. {}",
                rp(deferred),
                rp(want_deferred.json()),
                spec.note("_deferred_why")
            ),
        );
        for entry in want_deferred.arr() {
            let key = entry.str();
            let r = row(prefs, key).unwrap_or(&super::super::ledger::NULL);
            l.check(
                matches!(get(r, "deferred_to_generate"), Json::Bool(true)),
                format!(
                    "{}: listed in metadata.deferred_to_generate but the row does not carry deferred_to_generate: true — generate.md's precondition and estimate-assemble.md Step 3b both key off the row flag",
                    rs(key)
                ),
            );
            l.check(
                !matches!(get(r, "default"), Json::Null) && eq(get(r, "value"), get(r, "default")),
                format!(
                    "{}: a deferred row must carry a non-null default and (before Step 3b) value == default; got value={} default={}",
                    rs(key),
                    rp(get(r, "value")),
                    rp(get(r, "default"))
                ),
            );
        }
    }
}

/// The two metadata lists are DISJOINT and every `questions_defaulted[]` key
/// resolves (schema-preferences.md § Shape). Runs on EVERY expectation set.
fn check_provenance_lists(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let meta = get(prefs, "metadata");
    let defaulted = items(get(meta, "questions_defaulted"));
    let deferred = items(get(meta, "deferred_to_generate"));
    let overlap: Vec<String> = defaulted
        .iter()
        .filter(|key| deferred.iter().any(|other| eq(key, other)))
        .map(ps)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    l.check(
        overlap.is_empty(),
        format!(
            "{} appear in BOTH metadata.questions_defaulted and metadata.deferred_to_generate. The lists are disjoint (clarify-assemble.md § Assembly rule 0): a deferred row renders once in the Estimate block and Step 3b has one entry to clear.",
            rl(&overlap)
        ),
    );
    for entry in defaulted {
        let Some(key) = entry.as_str() else {
            l.fail(format!(
                "{} is in metadata.questions_defaulted but is not a dotted row key.",
                rp(entry)
            ));
            continue;
        };
        let Some(r) = row(prefs, key) else {
            l.fail(format!(
                "{} is in metadata.questions_defaulted but resolves to no row. Array rows use index notation (app_service_plans[0].isolation_split) so one lookup finds them; a key the Estimate block cannot resolve is a row it cannot show.",
                rs(key)
            ));
            continue;
        };
        l.check(
            matches!(get(r, "disposition"), Json::String(s) if s == "PROPOSED"),
            format!(
                "{} is in questions_defaulted but its disposition is {}; only PROPOSED rows take a default without being asked (DETECTED rows were READ).",
                rs(key),
                rp(get(r, "disposition"))
            ),
        );
        l.check(
            eq(get(r, "value"), get(r, "default")),
            format!(
                "{} is in questions_defaulted but value={} != default={}. A corrected row must leave the list (schema-preferences.md § Correction provenance) or Step 2 re-lists the user's choice as an assumption.",
                rs(key),
                rp(get(r, "value")),
                rp(get(r, "default"))
            ),
        );
        l.check(
            !truthy(get(r, "source")),
            format!(
                "{} is in questions_defaulted but carries source={}. A row with a correction/confirmation source is no longer a silent default.",
                rs(key),
                rp(get(r, "source"))
            ),
        );
    }
    for entry in deferred {
        let key = entry.as_str().unwrap_or("");
        let r = row(prefs, key).unwrap_or(&super::super::ledger::NULL);
        l.check(
            matches!(get(r, "deferred_to_generate"), Json::Bool(true)),
            format!(
                "{} is listed in metadata.deferred_to_generate but the row flag is {}; Part 4's label and Step 3b key off the row flag.",
                rp(entry),
                rp(get(r, "deferred_to_generate"))
            ),
        );
    }
    let spec = exp.get("fast_path_provenance");
    if spec.truthy() {
        let missing: Vec<&str> = spec
            .at("expected_defaulted_keys")
            .strs()
            .into_iter()
            .filter(|key| !defaulted.iter().any(|d| d.as_str() == Some(key)))
            .collect();
        l.check(
            missing.is_empty(),
            format!(
                "{} are missing from metadata.questions_defaulted. {}",
                rl(&missing),
                spec.note("_why")
            ),
        );
        let wrongly: Vec<&str> = spec
            .get("must_not_be_defaulted")
            .arr_or_empty()
            .iter()
            .map(Spec::str)
            .filter(|key| defaulted.iter().any(|d| d.as_str() == Some(key)))
            .collect();
        l.check(
            wrongly.is_empty(),
            format!(
                "{} appear in metadata.questions_defaulted but were asked (ESSENTIAL) or deferred (asked at Step 3b) — neither is a silent default. {}",
                rl(&wrongly),
                spec.note("_why")
            ),
        );
    }
}

/// `data.db_cutover.default` and `.size_coverage` follow Q-D2's rule from the
/// inventory: a fixed expected value would pin one estate, the rule pins the
/// behaviour, including the missing-size case where a plausible improvisation
/// invents a size or escalates to ESSENTIAL.
fn check_db_cutover_default_rule(context: &Context<'_>, prefs: &Json, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("db_cutover_default_rule");
    if !spec.truthy() {
        return;
    }
    let why = spec.note("_why");
    let Some(r) = row(prefs, "data.db_cutover") else {
        l.fail(format!(
            "no data.db_cutover row to check the size rule against. {why}"
        ));
        return;
    };
    let mut sizes: Vec<Option<f64>> = Vec::new();
    let mut size_values: Vec<Json> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for resource in objs(get(inv, "resources")) {
        let relational = get(resource, "azure_type")
            .as_str()
            .is_some_and(|t| RELATIONAL_TYPES.contains(&t));
        if relational {
            let storage = path(resource, &["config", "storage_mb"]);
            sizes.push(number(storage));
            size_values.push(storage.clone());
            let name = get(resource, "name");
            names.push(ps(if truthy(name) {
                name
            } else {
                get(resource, "azure_id")
            }));
        }
    }
    if sizes.is_empty() {
        l.check(
            matches!(get(r, "disposition"), Json::String(s) if s == "N/A"),
            format!(
                "no relational database in the inventory, but data.db_cutover is {} rather than N/A",
                rp(get(r, "disposition"))
            ),
        );
        return;
    }
    let sizes_text = rp(&Json::Array(size_values));
    let (want_default, want_coverage) = expected_db_cutover_default(&sizes);
    l.check(
        matches!(get(r, "default"), Json::String(s) if s == want_default),
        format!(
            "data.db_cutover.default is {}, expected {} from relational storage_mb {sizes_text} on {}. {why}",
            rp(get(r, "default")),
            rs(want_default),
            rl(&names)
        ),
    );
    l.check(
        matches!(get(r, "size_coverage"), Json::String(s) if s == want_coverage),
        format!(
            "data.db_cutover.size_coverage is {}, expected {} from relational storage_mb {sizes_text}. The uncertainty travels on the row; it is never papered over by inventing a size.",
            rp(get(r, "size_coverage")),
            rs(want_coverage)
        ),
    );
    let measured: Vec<f64> = sizes.iter().flatten().copied().collect();
    let want_gib = if measured.is_empty() {
        Json::Null
    } else {
        Json::Float(measured.iter().copied().fold(f64::MIN, f64::max) / GIB_MB)
    };
    l.check(
        eq(get(r, "largest_relational_db_gib"), &want_gib),
        format!(
            "data.db_cutover.largest_relational_db_gib is {}, expected {} (null when nothing was measured).",
            rp(get(r, "largest_relational_db_gib")),
            rp(&want_gib)
        ),
    );
    l.check(
        truthy(get(r, "default_basis")),
        "data.db_cutover has no default_basis — the rule and what it was applied to must be on the row so Step 3b can say why the default was chosen",
    );
    l.note(format!(
        "db_cutover rule: sizes_mb={sizes_text} -> default={want_default} coverage={want_coverage}"
    ));
}

fn check_cluster_rows(
    context: &Context<'_>,
    prefs: &Json,
    clusters_dir: &super::super::Anchored,
    exp: &Spec<'_>,
) {
    let l = context.ledger;
    let spec = exp.at("cluster_rows");
    let file = spec.at("clusters_file").str();
    let Some(clusters) = context.json_labeled(clusters_dir, file, file) else {
        return;
    };
    let ids = |value: &Json| -> BTreeSet<String> {
        items(get(value, "clusters"))
            .iter()
            .map(|c| ps(get(c, "cluster_id")))
            .collect()
    };
    let want = ids(&clusters);
    let got = ids(prefs);
    l.check(
        got == want,
        format!(
            "preferences clusters {} do not match the clusters artifact {}. {}",
            rl(&got.iter().cloned().collect::<Vec<_>>()),
            rl(&want.iter().cloned().collect::<Vec<_>>()),
            spec.note("_why")
        ),
    );
}

fn check_no_secrets(context: &Context<'_>, prefs: &Json, exp: &Spec<'_>) {
    let spec = exp.at("forbidden_substrings");
    let blob = dumps(prefs);
    for bad in spec.at("values").strs() {
        context.ledger.check(
            !blob.contains(bad),
            format!(
                "{} appears in preferences.json. {}",
                rs(bad),
                spec.note("_why")
            ),
        );
    }
}

fn run(context: &Context<'_>, spec_name: &str) -> Flow {
    let exp_doc = context.expectation(spec_name)?;
    let exp = context.spec(&exp_doc, spec_name);
    let preferences_file = exp.at("preferences_file").str();
    let prefs = context.json_labeled(&context.run, preferences_file, preferences_file)?;

    let inventory_file = exp.at("inventory_file").str();
    let golden = context.fixture.join("after-discover");
    let inv_dir = if context.exists(&context.run, inventory_file) {
        context.run.clone()
    } else {
        context.ledger.note(format!(
            "no inventory in the run dir; using the fixture's golden at {inventory_file}"
        ));
        golden.clone()
    };
    let inv = context.json_labeled(&inv_dir, inventory_file, inventory_file)?;
    let clusters_file = exp.at("cluster_rows").at("clusters_file").str();
    let cl_dir = if context.exists(&context.run, clusters_file) {
        context.run.clone()
    } else {
        golden
    };

    check_dispositions(context, &prefs, &exp);
    check_essential_gate(context, &prefs, &exp);
    check_defaults_not_promoted(context, &prefs, &exp);
    check_conflicting_answer(context, &prefs, &exp);
    check_isolation_rows(context, &prefs, &inv, &exp);
    check_licensing_firing(context, &prefs, &exp);
    check_clarify_mode(context, &prefs, &exp);
    check_provenance_lists(context, &prefs, &exp);
    check_db_cutover_default_rule(context, &prefs, &inv, &exp);
    check_cluster_rows(context, &prefs, &cl_dir, &exp);
    check_no_secrets(context, &prefs, &exp);
    Some(())
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn the_q_d2_rule_has_an_inclusive_100_gib_boundary() {
        assert_eq!(
            expected_db_cutover_default(&[Some(102_400.0)]),
            ("dump_restore", "complete")
        );
        assert_eq!(
            expected_db_cutover_default(&[Some(102_401.0)]),
            ("dms", "complete")
        );
        assert_eq!(
            expected_db_cutover_default(&[None]),
            ("dump_restore", "unknown")
        );
        assert_eq!(
            expected_db_cutover_default(&[None, Some(2048.0)]),
            ("dump_restore", "partial")
        );
        assert_eq!(
            expected_db_cutover_default(&[None, Some(204_800.0)]),
            ("dms", "partial")
        );
        // The remaining parametrised cases of the legacy
        // `test_db_cutover_rule_boundary` (#4409): a measured 64 GiB server,
        // no server at all, and a 64 GiB server beside an unmeasured one.
        assert_eq!(
            expected_db_cutover_default(&[Some(65_536.0)]),
            ("dump_restore", "complete")
        );
        assert_eq!(
            expected_db_cutover_default(&[]),
            ("dump_restore", "unknown")
        );
        assert_eq!(
            expected_db_cutover_default(&[Some(65_536.0), None]),
            ("dump_restore", "partial")
        );
    }

    #[test]
    fn row_lookup_resolves_dotted_and_indexed_keys() {
        let prefs = toolkit_core::parse_json(
            r#"{"data": {"availability": {"disposition": "ESSENTIAL"}},
                "app_service_plans": [{"isolation_split": {"disposition": "PROPOSED"}}],
                "scalar": 3}"#,
        )
        .unwrap();
        assert!(row(&prefs, "data.availability").is_some());
        assert!(row(&prefs, "app_service_plans[0].isolation_split").is_some());
        assert!(row(&prefs, "app_service_plans[1].isolation_split").is_none());
        assert!(row(&prefs, "app_service_plans[x].isolation_split").is_none());
        assert!(row(&prefs, "scalar").is_none());
        assert!(row(&prefs, "data.availability.disposition").is_none());
        assert!(row(&prefs, "missing.deeper").is_none());
    }
}
