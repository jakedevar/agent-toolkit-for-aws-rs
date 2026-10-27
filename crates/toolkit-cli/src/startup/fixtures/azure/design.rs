//! The azure-to-aws Design asserter (pass 1) over the Terraform corpus
//! (`check_expected_design.py`), the external oracle for the mapping
//! algorithm and the companion of the Discover asserter.
//!
//! A design that routes a Mongo-API Cosmos account to `DynamoDB`, an SMB share to
//! EFS and a Kafka-enabled Event Hubs namespace to Kinesis is well-formed,
//! satisfies every postcondition, and is wrong in three places that each cost a
//! rewrite. Every check pins a fact where a plausible improvisation and the
//! correct answer diverge: the protocol-conditioned Direct Mappings rows, the
//! App Service Plan fan-in, the untranslated cost-bearing type, the
//! `deterministic` label checked in BOTH directions against the table, and the
//! table's own row set.
//!
//! Identity is the Terraform ADDRESS, resolved to an `azure_id` through the
//! run's own inventory, so these expectations are not coupled to the synthetic
//! `tf:<local>` id reconstruction.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::Json;

use super::super::ledger::{
    dumps, eq, get, items, list, members, objs, path, ps, rl, rp, rs, text, truthy, Spec, NULL,
};
use super::super::{Context, Flow};
use crate::manifests::{py_splitlines, py_strip};

const SERVERFARMS: &str = "Microsoft.Web/serverfarms";
const SITES: &str = "Microsoft.Web/sites";

type IdOf = BTreeMap<String, String>;

/// Table keys, minus the leading-underscore documentation keys.
fn real_keys(value: &Json) -> Vec<&str> {
    members(value)
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| !key.starts_with('_'))
        .collect()
}

fn id_for<'a>(id_of: &'a IdOf, address: &str) -> Option<&'a str> {
    id_of.get(address).map(String::as_str)
}

/// Whether the design item's `azure_id` is `id`.
fn is_id(value: &Json, id: Option<&str>) -> bool {
    id.is_some_and(|id| text(get(value, "azure_id")) == Some(id))
}

/// `{e.get("azure_id"): e for e in services if dict}`; the last duplicate wins.
fn services_by_id(design: &Json) -> BTreeMap<String, &Json> {
    objs(get(design, "services"))
        .map(|entry| (ps(get(entry, "azure_id")), entry))
        .collect()
}

/// Every per-resource entry, whatever section it landed in.
fn entries(design: &Json) -> Vec<(&Json, &'static str)> {
    let mut out = Vec::new();
    for section in ["services", "pending_rubric", "deferred"] {
        for entry in objs(get(design, section)) {
            out.push((entry, section));
        }
    }
    out
}

fn warnings_of(design: &Json) -> &[Json] {
    list(design, "warnings")
}

fn warns_with<'a>(design: &'a Json, code: &Json, id: Option<&str>) -> Vec<&'a Json> {
    warnings_of(design)
        .iter()
        .filter(|w| eq(get(w, "code"), code) && is_id(w, id))
        .collect()
}

// --------------------------------------------------------------------- table

/// Validates the disposition table itself before trusting a design that cites
/// it: a design row saying `confidence: deterministic, fast_path_row: X` is only
/// meaningful if X exists, and a type must resolve to at most one disposition.
fn check_table(context: &Context<'_>, table: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("table_self_checks");
    let direct = get(table, "direct_mappings");
    let skips = get(table, "skip_mappings");
    let gates = get(table, "specialist_gates");

    for t in spec.at("required_direct_rows").strs() {
        l.check(
            direct.get(t).is_some(),
            format!(
                "direct_mappings has no row for {}. {}",
                rs(t),
                spec.note("_required_direct_rows_why")
            ),
        );
    }
    for t in spec.at("must_not_be_direct_rows").strs() {
        l.check(
            direct.get(t).is_none(),
            format!(
                "{} is a direct_mappings row and must not be. {}",
                rs(t),
                spec.note("_must_not_be_direct_rows_why")
            ),
        );
    }
    for t in spec.at("required_skip_rows").strs() {
        l.check(
            skips.get(t).is_some(),
            format!("skip_mappings has no row for {}", rs(t)),
        );
    }
    for t in spec.at("required_gate_rows").strs() {
        l.check(
            gates.get(t).is_some(),
            format!("specialist_gates has no row for {}", rs(t)),
        );
    }

    // Precedence invariant: at most one disposition per canonical type. A
    // conditional gate key carries a '#suffix' and is exempt: it fires on
    // config, not on the type.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, tbl) in [
        ("direct_mappings", direct),
        ("skip_mappings", skips),
        ("specialist_gates", gates),
    ] {
        for t in real_keys(tbl) {
            if t.contains('#') {
                continue;
            }
            if let Some(first) = seen.get(t) {
                l.fail(format!(
                    "{} appears in both {first} and {name}. A canonical type must resolve to AT MOST ONE disposition — two makes the precedence order depend on iteration order rather than on the documented sequence.",
                    rs(t)
                ));
            }
            seen.insert(t, name);
        }
    }

    let banned = spec.at("banned_substring").str();
    l.check(
        !dumps(table).contains(banned),
        format!(
            "{} appears in the disposition table. {}",
            rs(banned),
            spec.note("_banned_substring_why")
        ),
    );
    l.note(format!(
        "table: {} direct, {} skip, {} gate rows",
        real_keys(direct).len(),
        real_keys(skips).len(),
        real_keys(gates).len()
    ));
}

// ------------------------------------------------------------- design body

fn check_deterministic(
    context: &Context<'_>,
    design: &Json,
    table: &Json,
    id_of: &IdOf,
    exp: &Spec<'_>,
) {
    let l = context.ledger;
    let direct = get(table, "direct_mappings");
    let services = services_by_id(design);
    let want_label = exp.at("confidence").at("expected_value");

    for (tf_addr, spec) in exp.at("deterministic_mappings").real_entries() {
        let Some(aid) = id_for(id_of, tf_addr) else {
            l.fail(format!(
                "no inventory resource with Terraform address {} — cannot check its mapping",
                rs(tf_addr)
            ));
            continue;
        };
        let Some(e) = services.get(aid) else {
            l.fail(format!(
                "{}: no services[] entry. It matches a Direct Mappings row, so it must be mapped rather than deferred, skipped, or left pending.",
                rs(tf_addr)
            ));
            continue;
        };
        let got = get(e, "aws_service");
        let why = spec.get("why");
        l.check(
            eq(got, spec.at("aws_service").json()),
            format!(
                "{}: aws_service is {}, expected {}.{}",
                rs(tf_addr),
                rp(got),
                rp(spec.at("aws_service").json()),
                if why.truthy() {
                    format!(" {}", why.str())
                } else {
                    String::new()
                }
            ),
        );
        for bad in spec.get("must_not_be").arr_or_empty() {
            l.check(
                !eq(got, bad.json()),
                format!(
                    "{}: aws_service is {}, which is the plausible-but-wrong answer. {}",
                    rs(tf_addr),
                    rp(bad.json()),
                    if why.truthy() { why.str() } else { "" }
                ),
            );
        }
        l.check(
            eq(get(e, "confidence"), want_label.json()),
            format!(
                "{}: confidence is {}, expected {} — it came from a table row, so no rubric ran.",
                rs(tf_addr),
                rp(get(e, "confidence")),
                rp(want_label.json())
            ),
        );
    }

    // The direction that catches a fabricated label: anything claiming
    // `deterministic` must name a type that really is in the table, with a
    // target the row allows.
    for e in list(design, "services") {
        if !eq(get(e, "confidence"), want_label.json()) {
            continue;
        }
        let t = get(e, "azure_type");
        let row = text(t).and_then(|name| direct.get(name));
        let Some(row) = row else {
            l.fail(format!(
                "{} is labelled {} but has NO direct_mappings row. This is the signature of an improvised label: the target may even be reasonable, but it came from a prior rather than a table, so it is unreproducible and the user-facing 'Standard pairing' claim is false.",
                rp(t),
                rp(want_label.json())
            ));
            continue;
        };
        let mut allowed: Vec<&Json> = vec![get(row, "aws_service")];
        allowed.extend(
            list(row, "alternatives")
                .iter()
                .map(|a| get(a, "aws_service")),
        );
        let shown: BTreeSet<String> = allowed
            .iter()
            .filter(|a| truthy(a))
            .map(|a| ps(a))
            .collect();
        l.check(
            allowed.iter().any(|a| eq(get(e, "aws_service"), a)),
            format!(
                "{} is labelled {} with aws_service {}, which is not the row's target or any of its alternatives ({}).",
                rp(t),
                rp(want_label.json()),
                rp(get(e, "aws_service")),
                rl(&shown.into_iter().collect::<Vec<_>>())
            ),
        );
        let allowed_values = exp.at("confidence").at("allowed_values");
        l.check(
            allowed_values
                .arr()
                .iter()
                .any(|v| eq(get(e, "confidence"), v.json())),
            format!(
                "{}: confidence {} is not one of {}",
                rp(t),
                rp(get(e, "confidence")),
                rp(allowed_values.json())
            ),
        );
    }
}

fn check_fan_in(context: &Context<'_>, design: &Json, id_of: &IdOf, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("app_service_plan_fan_in");
    let all = entries(design);
    let why = spec.note("_why");

    let plans = all
        .iter()
        .filter(|(e, _)| text(get(e, "azure_type")) == Some(SERVERFARMS))
        .count();
    let units = spec.at("expected_compute_unit_count").i64();
    l.check(
        i64::try_from(plans).unwrap_or(i64::MAX) == units,
        format!(
            "{plans} {SERVERFARMS} entr(ies), expected {units} — one compute unit per PLAN. {why}"
        ),
    );
    let sites = all
        .iter()
        .filter(|(e, _)| text(get(e, "azure_type")) == Some(SITES))
        .count();
    let site_count = spec.at("expected_site_entry_count").i64();
    l.check(
        i64::try_from(sites).unwrap_or(i64::MAX) == site_count,
        format!(
            "{sites} {SITES} entr(ies) in services[]/pending_rubric[]/deferred[], expected {site_count}. {} {why}",
            spec.note("_expected_site_entry_count_why")
        ),
    );

    let by_id: BTreeMap<String, &Json> = all
        .iter()
        .map(|(e, _)| (ps(get(e, "azure_id")), *e))
        .collect();
    let known: BTreeSet<&str> = id_of.values().map(String::as_str).collect();
    for (key, want_count) in [
        (
            "web_plan_tf_address",
            spec.at("web_plan_expected_hosted_app_count"),
        ),
        (
            "idle_plan_tf_address",
            spec.at("idle_plan_expected_hosted_app_count"),
        ),
    ] {
        let address = spec.at(key).str();
        let aid = id_for(id_of, address);
        let Some(plan) = aid.and_then(|id| by_id.get(id)) else {
            l.fail(format!("no design entry for plan {}", rs(address)));
            continue;
        };
        let hosted = get(plan, "hosted_app_azure_ids");
        if matches!(hosted, Json::Null) {
            l.fail(format!(
                "plan {} has no hosted_app_azure_ids. It is the record of WHICH apps fanned in; without it the one-compute-unit-per-plan rule cannot be audited, and an app silently dropped from the plan's aws_config is undetectable.",
                rs(address)
            ));
            continue;
        }
        let hosted_items = items(hosted);
        l.check(
            eq(
                &Json::Int(i64::try_from(hosted_items.len()).unwrap_or(i64::MAX)),
                want_count.json(),
            ),
            format!(
                "plan {} lists {} hosted app(s), expected {}. {why}",
                rs(address),
                hosted_items.len(),
                ps(want_count.json())
            ),
        );
        for h in hosted_items {
            l.check(
                text(h).is_some_and(|id| known.contains(id)),
                format!(
                    "plan {} lists hosted app {}, which is not in the inventory",
                    rs(address),
                    rp(h)
                ),
            );
        }
    }

    let web_address = spec.at("web_plan_tf_address").str();
    if let Some(web) = id_for(id_of, web_address).and_then(|id| by_id.get(id)) {
        let sizing = if truthy(get(web, "sizing_source")) {
            get(web, "sizing_source")
        } else if truthy(get(web, "aws_config")) {
            get(web, "aws_config")
        } else {
            &NULL
        };
        l.check(
            eq(get(sizing, "sku_name"), spec.at("web_plan_expected_sku").json()),
            format!(
                "web plan sku is {}, expected {} — sizing comes from the plan's SKU, never from the app count",
                rp(get(sizing, "sku_name")),
                rp(spec.at("web_plan_expected_sku").json())
            ),
        );
        l.check(
            eq(
                get(sizing, "worker_count"),
                spec.at("web_plan_expected_worker_count").json(),
            ),
            format!(
                "web plan worker_count is {}, expected {}",
                rp(get(sizing, "worker_count")),
                rp(spec.at("web_plan_expected_worker_count").json())
            ),
        );
    }

    let code = spec.at("consumed_app_warning_code");
    let consumed = warnings_of(design)
        .iter()
        .filter(|w| eq(get(w, "code"), code.json()))
        .count();
    let expected_consumed = spec.at("expected_consumed_app_count").i64();
    l.check(
        i64::try_from(consumed).unwrap_or(i64::MAX) == expected_consumed,
        format!(
            "{consumed} {} warning(s), expected {expected_consumed} — one per app consumed by a plan. Every site must be accounted for; an app with no entry and no warning is indistinguishable from one the design forgot.",
            rp(code.json())
        ),
    );

    let idle_address = spec.at("idle_plan_tf_address").str();
    let idle_code = spec.at("idle_plan_warning_code");
    l.check(
        !warns_with(design, idle_code.json(), id_for(id_of, idle_address)).is_empty(),
        format!(
            "no {} warning for {}. A plan with zero apps is purchased capacity with no workload — money already being spent, and a cost-optimization finding the customer cannot get anywhere else.",
            rp(idle_code.json()),
            rs(idle_address)
        ),
    );
}

/// The derived type must be DEFERRED: not halted on, and not silently skipped.
fn check_unknown_stop(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("unknown_type_stop");
    let halt = get(design, "halt");
    let blocking: &[Json] = if matches!(halt, Json::Object(_)) {
        list(halt, "blocking")
    } else {
        &[]
    };

    if spec.get("halt_required").truthy() {
        if !matches!(halt, Json::Object(_)) {
            l.fail(format!(
                "no `halt` object, and one is required. {}",
                spec.note("_why")
            ));
            return;
        }
        let kind = spec.at("expected_halt_kind");
        let identifier = spec.at("expected_identifier_substring");
        let hit = blocking
            .iter()
            .any(|b| eq(get(b, "kind"), kind.json()) && dumps(b).contains(identifier.str()));
        l.check(
            hit,
            format!(
                "halt.blocking has no {} entry naming {}. {}",
                rp(kind.json()),
                rs(identifier.str()),
                spec.note("_why")
            ),
        );
    } else {
        let forbidden = spec.get("forbidden_halt_kind");
        let bad: Vec<&Json> = blocking
            .iter()
            .filter(|b| eq(get(b, "kind"), forbidden.json()))
            .collect();
        l.check(
            bad.is_empty(),
            format!(
                "halt.blocking carries a {} entry {}, but that halt is UNREACHABLE on this corpus now. {}",
                rp(forbidden.json()),
                rp(&Json::Array(bad.iter().map(|b| (*b).clone()).collect())),
                spec.note("_forbidden_halt_why")
            ),
        );
    }

    let disp = spec.get("derived_type_disposition");
    if disp.truthy() {
        let want_type = disp.at("azure_type").str().to_lowercase();
        let where_ = disp.at("expected_in").str();
        let hit: Vec<&Json> = list(design, where_)
            .iter()
            .filter(|e| text(get(e, "azure_type")).unwrap_or("").to_lowercase() == want_type)
            .collect();
        l.check(
            !hit.is_empty(),
            format!(
                "{} is not in {where_}[]. A derived type must reach a real disposition — mapped, or deferred — never vanish. {}",
                disp.at("azure_type").str(),
                disp.note("_why")
            ),
        );
        for e in hit {
            l.check(
                e.get("confidence").is_none(),
                format!(
                    "{} is deferred but carries a confidence field. A deferral did not come from a rubric (13.1e).",
                    disp.at("azure_type").str()
                ),
            );
        }
    }

    let ident = {
        let primary = spec.get("identifier_substring");
        if primary.truthy() {
            primary.str()
        } else {
            spec.at("expected_identifier_substring").str()
        }
    };
    let benign = spec.at("must_not_appear_as_benign_skip_codes");
    for w in warnings_of(design) {
        if benign
            .arr()
            .iter()
            .any(|code| eq(get(w, "code"), code.json()))
        {
            l.check(
                !dumps(w).to_lowercase().contains(ident),
                format!(
                    "{} is recorded as a benign skip ({}). A cost-bearing derived type may be MAPPED or DEFERRED, never quietly skipped. {}",
                    rs(ident),
                    rp(get(w, "code")),
                    spec.note("_must_not_appear_why")
                ),
            );
        }
    }
}

/// Pass 2 exists now, so `pending_rubric[]` must be EMPTY: a resource still
/// PARKED as pending is a run that did not load rubric files that are on disk.
fn check_pending_rubric(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("missing_rubric_halt");
    let pending = list(design, "pending_rubric");
    let expected = spec.at("expected_pending_rubric_count").i64();
    let kinds: Vec<String> = pending.iter().map(|e| rp(get(e, "azure_type"))).collect();
    l.check(
        i64::try_from(pending.len()).unwrap_or(i64::MAX) == expected,
        format!(
            "pending_rubric[] has {} entr(ies), expected {expected}: [{}]. {}",
            pending.len(),
            kinds.join(", "),
            spec.note("_why")
        ),
    );
    for b in list(get(design, "halt"), "blocking") {
        l.check(
            !eq(get(b, "kind"), spec.at("forbidden_halt_kind").json()),
            format!(
                "halt.blocking still carries a {} entry ({}), but that file is on disk. {}",
                rp(spec.at("forbidden_halt_kind").json()),
                rp(get(b, "identifier")),
                spec.note("_why")
            ),
        );
    }
}

fn check_rubric_mappings(context: &Context<'_>, design: &Json, id_of: &IdOf, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("rubric_mappings");
    if !spec.truthy() {
        return;
    }
    let services = services_by_id(design);
    let want_conf = spec.at("expected_confidence");
    for (tf_addr, row) in spec.real_entries() {
        if !matches!(row.json(), Json::Object(_)) || !row.has("aws_service") {
            continue;
        }
        let why = row.note("why");
        let Some(aid) = id_for(id_of, tf_addr) else {
            l.fail(format!(
                "no inventory resource with Terraform address {}",
                rs(tf_addr)
            ));
            continue;
        };
        let Some(e) = services.get(aid) else {
            l.fail(format!(
                "{}: no services[] entry. Its rubric file is on disk, so it must be MAPPED — not pending, not deferred. {why}",
                rs(tf_addr)
            ));
            continue;
        };
        let got = get(e, "aws_service");
        l.check(
            eq(got, row.at("aws_service").json()),
            format!(
                "{}: aws_service is {}, expected {}. {why}",
                rs(tf_addr),
                rp(got),
                rp(row.at("aws_service").json())
            ),
        );
        for bad in row.get("must_not_be").arr_or_empty() {
            l.check(
                !eq(got, bad.json()),
                format!(
                    "{}: aws_service is {}, the plausible-but-wrong answer. {why}",
                    rs(tf_addr),
                    rp(bad.json())
                ),
            );
        }
        l.check(
            eq(get(e, "confidence"), want_conf.json()),
            format!(
                "{}: confidence is {}, expected {} — a rubric ran, so it is not deterministic; that tier is only for fast-path rows.",
                rs(tf_addr),
                rp(get(e, "confidence")),
                rp(want_conf.json())
            ),
        );
        l.check(
            truthy(get(e, "rubric_applied")),
            format!(
                "{}: no rubric_applied field, so which rubric produced this target is unauditable.",
                rs(tf_addr)
            ),
        );
    }
}

fn check_architecture_default(context: &Context<'_>, design: &Json, id_of: &IdOf, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("architecture_default");
    if !spec.truthy() {
        return;
    }
    let services = services_by_id(design);
    let why = spec.note("_why");
    for tf_addr in spec.at("tf_addresses").strs() {
        let Some(e) = id_for(id_of, tf_addr).and_then(|id| services.get(id)) else {
            l.fail(format!(
                "no services[] entry for {} — cannot check its architecture",
                rs(tf_addr)
            ));
            continue;
        };
        let cfg = get(e, "aws_config");
        // Scan only the architecture-bearing KEYS, not the whole config blob:
        // a rationale that explains why Graviton was NOT chosen legitimately
        // contains the word, and that explanation is what stops the x86 default
        // reading as a bug.
        let arch: Vec<(&str, &Json)> = members(cfg)
            .iter()
            .filter(|(key, _)| {
                let lowered = key.to_lowercase();
                lowered.contains("architecture") || lowered == "arch" || lowered == "cpu"
            })
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        let values = arch
            .iter()
            .map(|(_, v)| ps(v))
            .collect::<Vec<_>>()
            .join(" ");
        let keys: Vec<&str> = arch.iter().map(|(k, _)| *k).collect();
        l.check(
            !keys.is_empty(),
            format!(
                "{}: aws_config has no architecture field at all, so the x86_64 default is unrecorded and unverifiable. {why}",
                rs(tf_addr)
            ),
        );
        let expected = spec.at("expected_value").str();
        l.check(
            values.contains(expected),
            format!(
                "{}: architecture field(s) {} say {}, expected {}. {why}",
                rs(tf_addr),
                rl(&keys),
                rs(&values),
                rs(expected)
            ),
        );
        for bad in spec.at("forbidden_values").strs() {
            l.check(
                !values.contains(bad),
                format!(
                    "{}: an architecture field is set to {}. {why}",
                    rs(tf_addr),
                    rs(bad)
                ),
            );
        }
    }
}

fn check_source_ha_finding(context: &Context<'_>, design: &Json, id_of: &IdOf, exp: &Spec<'_>) {
    let spec = exp.get("source_ha_finding");
    if !spec.truthy() {
        return;
    }
    let address = spec.at("tf_address").str();
    let code = spec.at("expected_warning_code");
    context.ledger.check(
        !warns_with(design, code.json(), id_for(id_of, address)).is_empty(),
        format!(
            "no {} warning for {}. {}",
            rp(code.json()),
            rs(address),
            spec.note("_why")
        ),
    );
}

// ------------------------------------------------- skill-file relationships

/// The cells of a markdown table row.
fn cells(line: &str) -> Vec<&str> {
    py_strip(line)
        .trim_matches('|')
        .split('|')
        .map(py_strip)
        .collect()
}

/// `re.findall(r"`<prefix>[<class>]+`", text)`: the inner text (prefix
/// included) of every backticked token that has the prefix followed by one or
/// more characters of the class.
fn ticked<'a>(text: &'a str, prefix: &str, class: impl Fn(char) -> bool) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find('`') {
        let start = at + offset + 1;
        let rest = &text[start..];
        if let Some(body) = rest.strip_prefix(prefix) {
            let width: usize = body
                .chars()
                .take_while(|c| class(*c))
                .map(char::len_utf8)
                .sum();
            if width > 0 && body[width..].starts_with('`') {
                let inner = &rest[..prefix.len() + width];
                found.push(inner);
                at = start + inner.len() + 1;
                continue;
            }
        }
        at = start;
    }
    found
}

fn tf_class(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
}

fn arm_class(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '/'
}

/// The `(azurerm type, ARM type)` rows of arm-type-canonicalization.md: only
/// TABLE ROWS count. A type named in prose (an example, a trap explanation) is
/// not a canonicalization row.
fn canonical_rows(markdown: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in py_splitlines(markdown) {
        if !line.starts_with('|') {
            continue;
        }
        let cells = cells(line);
        if cells.len() < 2 {
            continue;
        }
        let arms = ticked(cells[1], "Microsoft.", arm_class);
        let tfs = ticked(cells[0], "azurerm_", tf_class);
        if let Some(first) = arms.first() {
            for tf in tfs {
                rows.push((tf.to_string(), (*first).to_string()));
            }
        }
    }
    rows
}

fn check_canonical_type_coverage(context: &Context<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("canonical_type_coverage");
    if !spec.truthy() {
        return;
    }
    let canon = context.read_relative(spec.at("canon_relpath").str());
    let index = context.read_relative(spec.at("index_relpath").str());
    let table_rel = exp.at("fast_path_table_relpath").str();
    let table_text = context.read_relative(table_rel);
    let (Some(canon), Some(index), Some(table_text)) = (canon, index, table_text) else {
        l.fail("cannot check canonical-type coverage: a source file is missing");
        return;
    };
    let exempt: BTreeSet<&str> = spec.at("exempt").strs().into_iter().collect();
    let mut types = BTreeSet::new();
    for line in py_splitlines(&canon) {
        if !line.starts_with('|') {
            continue;
        }
        let cells = cells(line);
        if cells.len() < 2 || ticked(cells[0], "azurerm_", tf_class).is_empty() {
            continue;
        }
        for arm in ticked(cells[1], "Microsoft.", arm_class) {
            if arm.contains('/') && !exempt.contains(arm) {
                types.insert(arm.to_string());
            }
        }
    }
    let Some(table) = context.parse_text(&table_text, "fast-path-services.json") else {
        return;
    };
    let mut disposed = BTreeSet::new();
    for section in [
        "direct_mappings",
        "skip_mappings",
        "specialist_gates",
        "hard_blockers",
    ] {
        for t in real_keys(get(&table, section)) {
            disposed.insert(t.split('#').next().unwrap_or("").to_string());
        }
    }
    let routed: BTreeSet<&String> = types
        .iter()
        .filter(|t| index.contains(&format!("`{t}`")))
        .collect();
    let orphans: Vec<String> = types
        .iter()
        .filter(|t| !disposed.contains(*t) && !routed.contains(t))
        .cloned()
        .collect();
    l.check(
        i64::try_from(orphans.len()).unwrap_or(i64::MAX) <= spec.at("max_orphans").i64(),
        format!(
            "{} canonical type(s) have NO disposition — not a fast-path row, not an index.md Reference row: {}. {}",
            orphans.len(),
            rl(&orphans),
            spec.note("_why")
        ),
    );
    l.note(format!(
        "canonical-type coverage: {} types, {} orphan(s)",
        types.len(),
        orphans.len()
    ));
}

/// Every rubric file index.md ROUTES to must be on disk, or declared pending.
/// A resource routed to a Reference file that is not on disk stops the design;
/// that is correct, but a rubric deliberately not written yet and a filename
/// typo looked identical at runtime. No run input is needed: this is a
/// relationship between two files.
fn check_index_reference_files_exist(context: &Context<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("index_reference_files");
    if !spec.truthy() {
        return;
    }
    let index = context.read_relative(spec.at("index_relpath").str());
    let on_disk = context.list_relative(spec.at("design_refs_relpath").str(), ".md");
    let (Some(index), Some(on_disk)) = (index, on_disk) else {
        l.fail("cannot check index.md Reference files: index.md or design-refs/ missing");
        return;
    };
    let pending: BTreeSet<&str> = spec.at("known_pending").strs().into_iter().collect();

    let mut routed: BTreeSet<String> = BTreeSet::new();
    for line in py_splitlines(&index) {
        if !line.starts_with('|') {
            continue;
        }
        for name in md_files(line) {
            routed.insert(name.to_string());
        }
    }
    // vendored/ai/*.md live outside design-refs/ and are not this check's business
    routed.retain(|name| !index.contains(&format!("`vendored/ai/{name}`")));

    let missing: Vec<&String> = routed
        .iter()
        .filter(|name| !on_disk.contains(*name) && !pending.contains(name.as_str()))
        .collect();
    l.check(
        missing.is_empty(),
        format!(
            "index.md routes to {} which are NOT on disk and NOT declared in known_pending. Either the rubric is missing (write it, or add it to known_pending with a reason) or the filename is wrong. {}",
            rl(&missing.iter().map(|m| m.as_str()).collect::<Vec<_>>()),
            spec.note("_why")
        ),
    );

    // Anti-rot: a pending entry that has since been written must be removed,
    // or the next reader trusts a stale list.
    let landed: Vec<&str> = pending
        .iter()
        .copied()
        .filter(|name| on_disk.contains(*name))
        .collect();
    l.check(
        landed.is_empty(),
        format!(
            "known_pending still lists {}, but those files now EXIST. Remove them from expected-design.json -- a pending list that is never pruned stops meaning anything.",
            rl(&landed)
        ),
    );
    let declared: Vec<&str> = pending
        .iter()
        .copied()
        .filter(|name| routed.contains(*name))
        .collect();
    l.note(format!(
        "index.md Reference files: {} routed, {} on disk, {} declared pending",
        routed.len(),
        routed.len() - declared.len(),
        rl(&declared)
    ));
}

/// `re.findall(r"`([a-z0-9-]+\.md)`", line)`.
fn md_files(line: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = line[at..].find('`') {
        let start = at + offset + 1;
        let rest = &line[start..];
        let width: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .count();
        if width > 0 && rest[width..].starts_with(".md`") {
            found.push(&rest[..width + 3]);
            at = start + width + 4;
        } else {
            at = start;
        }
    }
    found
}

/// A size with no declared origin is indistinguishable from a looked-up one:
/// this pins the corrected values and requires every sized entry to say where
/// its number came from.
fn check_sizing_provenance(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("sizing_provenance");
    if !spec.truthy() {
        return;
    }
    let size_keys: BTreeSet<&str> = spec.at("size_keys").strs().into_iter().collect();
    let allowed = spec.at("allowed_provenance");
    for entry in list(design, "services") {
        let cfg = get(entry, "aws_config");
        let sized: Vec<&str> = {
            let mut keys: Vec<&str> = members(cfg)
                .iter()
                .filter(|(key, value)| {
                    size_keys.contains(key.as_str()) && !matches!(value, Json::Null)
                })
                .map(|(key, _)| key.as_str())
                .collect();
            keys.sort_unstable();
            keys
        };
        if sized.is_empty() {
            continue;
        }
        let prov = get(entry, "sizing_provenance");
        l.check(
            allowed.arr().iter().any(|a| eq(prov, a.json())),
            format!(
                "{}: aws_config carries {} but sizing_provenance is {}, not one of {}. {}",
                ps(get(entry, "service_id")),
                rl(&sized),
                rp(prov),
                rp(allowed.json()),
                spec.note("_why")
            ),
        );
    }
    let pin_key = {
        let pinned_by = spec.get("pinned_by");
        if pinned_by.truthy() {
            pinned_by.str()
        } else {
            "azure_id"
        }
    };
    let pinned = spec.at("pinned");
    for (ident, want) in pinned.entries() {
        let entry = list(design, "services")
            .iter()
            .find(|e| text(get(e, pin_key)) == Some(ident));
        let Some(entry) = entry else {
            l.fail(format!(
                "no services[] entry with {pin_key}={} to check its pinned size. NOTE: pins are keyed on {pin_key} deliberately -- service_id is per-run and two valid runs name the same resource differently.",
                rs(ident)
            ));
            continue;
        };
        let sid = ps(get(entry, "service_id"));
        let cfg = get(entry, "aws_config");
        for (key, expected) in want.real_entries() {
            l.check(
                eq(get(cfg, key), expected.json()),
                format!(
                    "{sid}.{key} is {}, expected {}. {}",
                    rp(get(cfg, key)),
                    rp(expected.json()),
                    want.note("_why")
                ),
            );
        }
    }
    l.note(format!(
        "sizing_provenance: {} size(s) pinned against the knowledge/design tables",
        pinned.entries().len()
    ));
}

/// Every disposition must say how it was reached, and a DERIVED route is not
/// deterministic: a derived mapping would otherwise look identical to a curated
/// one in the artifact.
fn check_routing_provenance(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("routing_provenance");
    if !spec.truthy() {
        return;
    }
    let allowed = spec.at("allowed");
    let derived = spec.at("derived");
    let mut counts: Vec<(String, usize)> = Vec::new();
    for entry in list(design, "services") {
        let sid = ps(get(entry, "service_id"));
        let prov = get(entry, "routing_provenance");
        l.check(
            allowed.arr().iter().any(|a| eq(prov, a.json())),
            format!(
                "{sid}: routing_provenance is {}, not one of {}. {}",
                rp(prov),
                rp(allowed.json()),
                spec.note("_why")
            ),
        );
        let key = rp(prov);
        match counts.iter_mut().find(|(name, _)| *name == key) {
            Some((_, n)) => *n += 1,
            None => counts.push((key, 1)),
        }
        if derived.arr().iter().any(|d| eq(prov, d.json())) {
            l.check(
                !matches!(get(entry, "confidence"), Json::String(s) if s == "deterministic"),
                format!(
                    "{sid}: routing_provenance is {} (DERIVED) but confidence is 'deterministic'. That tier requires an authored direct_mappings row and a fast_path_row naming it; a derived route cannot earn it.",
                    rp(prov)
                ),
            );
        }
    }
    let shown: Vec<String> = counts
        .iter()
        .map(|(name, n)| format!("{name}: {n}"))
        .collect();
    l.note(format!("routing_provenance: {{{}}}", shown.join(", ")));
}

/// Every rubric file `namespace_routing` names must exist, or be declared
/// pending: adding a namespace rule that points at an unwritten file turns a
/// STOP into a HALT and moves the failure rather than fixing it.
fn check_namespace_routing_targets(context: &Context<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("routing_provenance");
    let pending_spec = exp.get("index_reference_files");
    if !spec.truthy() || !pending_spec.truthy() {
        return;
    }
    let table_text = context.read_relative(spec.at("namespace_routing_relpath").str());
    let on_disk = context.list_relative(spec.at("design_refs_relpath").str(), ".md");
    let (Some(table_text), Some(on_disk)) = (table_text, on_disk) else {
        l.fail("cannot check namespace_routing targets: table or design-refs/ missing");
        return;
    };
    let Some(table) = context.parse_text(&table_text, "namespace_routing table") else {
        return;
    };
    let rules = path(&table, &["namespace_routing", "rules"]);
    let pending: BTreeSet<&str> = pending_spec
        .at("known_pending")
        .strs()
        .into_iter()
        .collect();
    let missing: BTreeSet<&str> = members(rules)
        .iter()
        .filter_map(|(_, rule)| text(get(rule, "route")))
        .filter(|route| !route.is_empty() && !on_disk.contains(*route) && !pending.contains(route))
        .collect();
    l.check(
        missing.is_empty(),
        format!(
            "namespace_routing routes to {}, which are NOT on disk and NOT in known_pending. A namespace rule pointing at an unwritten rubric converts the unknown-type STOP into a missing-rubric HALT -- it moves the failure, it does not fix it.",
            rl(&missing.iter().copied().collect::<Vec<_>>())
        ),
    );
    l.note(format!(
        "namespace_routing: {} namespaces, all routes resolvable",
        members(rules).len()
    ));
}

/// `sn.split("_")` camel-cased: first word as is, the rest capitalised.
fn camel(snake: &str) -> String {
    let mut parts = snake.split('_');
    let mut out = parts.next().unwrap_or("").to_string();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(&chars.as_str().to_lowercase());
        }
    }
    out
}

fn plural(word: &str) -> String {
    if word.ends_with('y') && !["ay", "ey", "oy", "uy"].iter().any(|s| word.ends_with(s)) {
        return format!("{}ies", &word[..word.len() - 1]);
    }
    if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|s| word.ends_with(s))
    {
        return format!("{word}es");
    }
    format!("{word}s")
}

/// Would the pattern rule produce this ARM type's last segment from the
/// azurerm name?
fn segment_derivable(tf_type: &str, arm_type: &str) -> bool {
    let suffix = tf_type.strip_prefix("azurerm_").unwrap_or(tf_type);
    let last = arm_type.rsplit('/').next().unwrap_or("").to_lowercase();
    let mut candidates = vec![plural(&camel(suffix)), camel(suffix)];
    let words: Vec<&str> = suffix.split('_').collect();
    for i in 1..=suffix.matches('_').count() {
        let tail = words[i..].join("_");
        if !tail.is_empty() {
            candidates.push(plural(&camel(&tail)));
            candidates.push(camel(&tail));
        }
    }
    candidates.iter().any(|c| c.to_lowercase() == last)
}

/// Caps the DERIVABLE rows so the exception list cannot become a coverage list
/// again: derivation is the default path, so a new row is only justified where
/// derivation would be WRONG.
fn check_canonicalization_governance(context: &Context<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("canonicalization_governance");
    if !spec.truthy() {
        return;
    }
    let canon = context.read_relative(spec.at("canon_relpath").str());
    let namespaces = context.read_relative(spec.at("namespace_table_relpath").str());
    let (Some(canon), Some(namespaces)) = (canon, namespaces) else {
        l.fail("cannot check canonicalization governance: a source file is missing");
        return;
    };
    let rows = canonical_rows(&canon);
    let (derivable, divergent): (Vec<_>, Vec<_>) = rows
        .iter()
        .partition(|(tf, arm)| segment_derivable(tf, arm));

    let ceiling = spec.at("max_derivable_rows").i64();
    l.check(
        i64::try_from(derivable.len()).unwrap_or(i64::MAX) <= ceiling,
        format!(
            "{} DERIVABLE rows in arm-type-canonicalization.md, ceiling is {ceiling}. A derivable row restates the pattern rule instead of recording an exception. {}",
            derivable.len(),
            spec.note("_max_derivable_why")
        ),
    );
    let floor = spec.at("min_divergent_rows").i64();
    l.check(
        i64::try_from(divergent.len()).unwrap_or(i64::MAX) >= floor,
        format!(
            "only {} DIVERGENT rows, floor is {floor}. Pruning has removed rows that carry information. {}",
            divergent.len(),
            spec.note("_min_divergent_why")
        ),
    );

    if spec.get("require_all_namespaces_routable").truthy() {
        let Some(table) = context.parse_text(&namespaces, "namespace table") else {
            return;
        };
        let routable: BTreeSet<&str> = members(path(&table, &["namespace_routing", "rules"]))
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let orphan: BTreeSet<&str> = rows
            .iter()
            .map(|(_, arm)| arm.split('/').next().unwrap_or(""))
            .filter(|ns| !routable.contains(ns))
            .collect();
        l.check(
            orphan.is_empty(),
            format!(
                "namespaces {} appear in arm-type-canonicalization.md but NOT in namespace_routing. Derivation cross-checks the namespace against that table, so the two artefacts contradict each other. {}",
                rl(&orphan.iter().copied().collect::<Vec<_>>()),
                spec.note("_require_all_namespaces_routable_why")
            ),
        );
    }
    l.note(format!(
        "canonicalization: {} rows = {} divergent (kept) + {} derivable (capped at {ceiling})",
        rows.len(),
        divergent.len(),
        derivable.len()
    ));
}

/// Would this azurerm type plausibly derive into this namespace? Conservative.
fn plausible_namespace(tf_type: &str, namespace: &str) -> bool {
    let hints: &[&str] = match namespace {
        "Microsoft.Devices" => &["iothub", "iot_"],
        "Microsoft.Maps" => &["maps_"],
        "Microsoft.NotificationHubs" => &["notification_hub"],
        "Microsoft.Kusto" => &["kusto"],
        "Microsoft.HDInsight" => &["hdinsight"],
        "Microsoft.Purview" => &["purview"],
        "Microsoft.Relay" => &["relay"],
        "Microsoft.NetApp" => &["netapp"],
        _ => &[],
    };
    let suffix = tf_type.strip_prefix("azurerm_").unwrap_or(tf_type);
    hints.iter().any(|hint| suffix.contains(hint))
}

/// An untranslated-type halt is stale if that type's namespace is now routable:
/// derivation resolves an unlisted type and only STOPs when the derived
/// NAMESPACE is unrecognised, so a golden halting on a type whose namespace
/// `namespace_routing` now carries describes behaviour the skill no longer
/// produces, and every shape assertion still passes.
fn check_halt_not_stale(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("canonicalization_governance");
    if !spec.truthy() || !spec.get("assert_halt_not_stale").truthy() {
        return;
    }
    let Some(namespaces) = context.read_relative(spec.at("namespace_table_relpath").str()) else {
        return;
    };
    let Some(table) = context.parse_text(&namespaces, "namespace table") else {
        return;
    };
    let rules: Vec<&str> = members(path(&table, &["namespace_routing", "rules"]))
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let Some(canon) = context.read_relative(spec.at("canon_relpath").str()) else {
        l.fail("cannot check canonicalization governance: a source file is missing");
        return;
    };
    for blocker in list(get(design, "halt"), "blocking") {
        if !matches!(get(blocker, "kind"), Json::String(s) if s == "untranslated_terraform_type") {
            continue;
        }
        let identifier = text(get(blocker, "identifier")).unwrap_or("");
        let Some(tf) = identifier.strip_prefix("azurerm_").and_then(|rest| {
            let width: usize = rest
                .chars()
                .take_while(|c| tf_class(*c))
                .map(char::len_utf8)
                .sum();
            (width > 0).then(|| format!("azurerm_{}", &rest[..width]))
        }) else {
            continue;
        };
        if canon.contains(&format!("`{tf}`")) {
            l.fail(format!(
                "halt names {tf} as untranslated, but it IS in arm-type-canonicalization.md"
            ));
            continue;
        }
        let mut derived: Vec<&str> = rules
            .iter()
            .copied()
            .filter(|ns| plausible_namespace(&tf, ns))
            .collect();
        derived.sort_unstable();
        l.check(
            derived.is_empty(),
            format!(
                "STALE HALT: {tf} halts as untranslated, but derivation would place it in {} which namespace_routing RECOGNISES, so it now resolves instead of stopping. This golden needs regenerating by a capability run. {}",
                rl(&derived),
                spec.note("_assert_halt_not_stale_why")
            ),
        );
    }
}

// ------------------------------------------------------------ entry details

/// The VM's root volume must be in `aws_config`, and must not be its own service
/// entry.
fn check_inline_os_disk(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("inline_os_disk");
    if !spec.truthy() {
        return;
    }
    let aws_service = spec.at("aws_service");
    let hits: Vec<&Json> = list(design, "services")
        .iter()
        .filter(|e| eq(get(e, "aws_service"), aws_service.json()))
        .collect();
    l.check(
        !hits.is_empty(),
        format!(
            "no {} entry to check its root volume",
            ps(aws_service.json())
        ),
    );
    let key = spec.at("required_aws_config_key").str();
    for e in hits {
        let cfg = get(e, "aws_config");
        let sid = ps(get(e, "service_id"));
        let rv = get(cfg, key);
        l.check(
            matches!(rv, Json::Object(_)),
            format!(
                "{sid}: aws_config has no {}. {}",
                rs(key),
                spec.note("_why")
            ),
        );
        for bad in spec.at("forbidden_keys").strs() {
            l.check(
                cfg.get(bad).is_none(),
                format!(
                    "{sid}: aws_config carries {}. {}",
                    rs(bad),
                    spec.note("_forbidden_why")
                ),
            );
        }
        if matches!(rv, Json::Object(_)) {
            l.check(
                eq(get(rv, "volume_type"), spec.at("expected_volume_type").json()),
                format!(
                    "root_volume.volume_type is {}, expected {} — a tier_mapping lookup, not a judgement.",
                    rp(get(rv, "volume_type")),
                    rp(spec.at("expected_volume_type").json())
                ),
            );
            if spec.get("size_gib_must_be_null").truthy() {
                l.check(
                    matches!(get(rv, "size_gib"), Json::Null),
                    format!(
                        "root_volume.size_gib is {}, expected null. The source sets no disk_size_gb, so the size is the image default and is not discoverable — a remembered number would be priced as fact.",
                        rp(get(rv, "size_gib"))
                    ),
                );
            }
        }
    }
    let forbidden_type = spec.at("must_not_be_own_service_entry");
    let own: Vec<String> = list(design, "services")
        .iter()
        .filter(|e| eq(get(e, "azure_type"), forbidden_type.json()))
        .map(|e| rp(get(e, "service_id")))
        .collect();
    l.check(
        own.is_empty(),
        format!(
            "{} appears as its own services[] entry [{}]. {}",
            ps(forbidden_type.json()),
            own.join(", "),
            spec.note("_must_not_why")
        ),
    );
}

/// The honest 'no pattern catalog' state, not a plausible architecture string:
/// design.md's cluster postcondition demands `target_architecture` while
/// patterns.md does not exist, and an agent optimising for a green gate writes
/// a convincing string there that nothing downstream can catch.
fn check_cluster_pattern_status(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("cluster_pattern_status");
    if !spec.truthy() {
        return;
    }
    let clusters: BTreeMap<String, &Json> = objs(get(design, "clusters"))
        .map(|c| (ps(get(c, "cluster_id")), c))
        .collect();
    for cid in spec.at("expected_cluster_ids").strs() {
        let Some(c) = clusters.get(cid) else {
            l.fail(format!(
                "no clusters[] entry for {}. {}",
                rs(cid),
                spec.note("_why")
            ));
            continue;
        };
        l.check(
            eq(get(c, "pattern_status"), spec.at("expected_status").json()),
            format!(
                "cluster {}: pattern_status is {}, expected {} — design-refs/patterns.md is not on disk, so no recognition was ATTEMPTED, which is a different fact from 'nothing matched'.",
                rs(cid),
                rp(get(c, "pattern_status")),
                rp(spec.at("expected_status").json())
            ),
        );
        if spec.get("target_architecture_must_be_null").truthy() {
            l.check(
                matches!(get(c, "target_architecture"), Json::Null),
                format!(
                    "cluster {}: target_architecture is {}, expected null. {}",
                    rs(cid),
                    rp(get(c, "target_architecture")),
                    spec.note("_why")
                ),
            );
        }
    }
}

fn check_compute_unit_fields(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let spec = exp.get("compute_unit_required_fields");
    if !spec.truthy() {
        return;
    }
    for (e, section) in entries(design) {
        if !eq(get(e, "azure_type"), spec.at("azure_type").json()) {
            continue;
        }
        for field in spec.at("required").strs() {
            context.ledger.check(
                e.get(field).is_some(),
                format!(
                    "{} entry {} is missing {} (section {section}). {}",
                    ps(spec.at("azure_type").json()),
                    rp(get(e, "azure_id")),
                    rs(field),
                    spec.note("_why")
                ),
            );
        }
    }
}

fn check_halt_covers_pending(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let spec = exp.at("missing_rubric_halt");
    if !spec.get("require_halt_entry_per_ref_file").truthy() {
        return;
    }
    let refs: BTreeSet<String> = list(design, "pending_rubric")
        .iter()
        .filter(|e| truthy(get(e, "ref_file")))
        .map(|e| ps(get(e, "ref_file")))
        .collect();
    let named = dumps(&Json::Array(list(get(design, "halt"), "blocking").to_vec()));
    for r in refs {
        context.ledger.check(
            named.contains(&r),
            format!(
                "{} appears in pending_rubric[] but no halt.blocking entry names it. {}",
                rs(&r),
                spec.note("_require_halt_entry_why")
            ),
        );
    }
}

fn check_skips(context: &Context<'_>, design: &Json, id_of: &IdOf, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("skips");
    let mapped: BTreeSet<String> = entries(design)
        .iter()
        .map(|(e, _)| ps(get(e, "azure_id")))
        .collect();
    let mut blob_by_id: BTreeMap<String, String> = BTreeMap::new();
    for w in warnings_of(design) {
        let aid = get(w, "azure_id");
        if truthy(aid) {
            blob_by_id.entry(ps(aid)).or_default().push_str(&dumps(w));
        }
    }
    for tf_addr in spec.at("expected_skipped_tf_addresses").strs() {
        let Some(aid) = id_for(id_of, tf_addr) else {
            l.fail(format!(
                "no inventory resource with Terraform address {}",
                rs(tf_addr)
            ));
            continue;
        };
        l.check(
            blob_by_id.contains_key(aid),
            format!(
                "{} is a Skip Mapping but has no warnings[] entry. A resource with no entry and no warning is indistinguishable from one the design forgot.",
                rs(tf_addr)
            ),
        );
        l.check(
            !mapped.contains(aid),
            format!(
                "{} is a Skip Mapping but was given a target. Observability in particular has no import path, so a CloudWatch TARGET would claim a migration that cannot happen — the CloudWatch fallback is a report note, not a mapping.",
                rs(tf_addr)
            ),
        );
    }
    for tf_addr in spec.at("config_source_tf_addresses").strs() {
        let blob = id_for(id_of, tf_addr)
            .and_then(|aid| blob_by_id.get(aid))
            .map_or("", String::as_str);
        l.check(
            blob.contains("config_source") || blob.contains("consumed") || blob.contains("edge"),
            format!(
                "{} is a CONFIG SOURCE skip, but its warning does not say what it contributed. {}",
                rs(tf_addr),
                spec.note("_config_source_why")
            ),
        );
    }
}

fn check_hard_blocker(context: &Context<'_>, design: &Json, id_of: &IdOf, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("hard_blocker");
    let address = spec.at("tf_address").str();
    let code = spec.at("expected_warning_code");
    let hit = warns_with(design, code.json(), id_for(id_of, address));
    l.check(
        !hit.is_empty(),
        format!(
            "no {} warning for {}. {}",
            rp(code.json()),
            rs(address),
            spec.note("_why")
        ),
    );
    if let Some(first) = hit.first() {
        l.check(
            eq(get(first, "severity"), spec.at("expected_severity").json()),
            format!(
                "{} severity is {}, expected {} — MGN refuses the image, so this is not advisory",
                rp(code.json()),
                rp(get(first, "severity")),
                rp(spec.at("expected_severity").json())
            ),
        );
    }
}

fn check_deferred_shape(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("deferred_shape");
    let deferred = list(design, "deferred");
    for d in deferred {
        l.check(
            eq(
                get(d, "aws_service"),
                spec.at("expected_aws_service").json(),
            ),
            format!(
                "deferred entry aws_service is {}, expected {}",
                rp(get(d, "aws_service")),
                rp(spec.at("expected_aws_service").json())
            ),
        );
        l.check(
            d.get(spec.at("forbidden_field").str()).is_none(),
            format!(
                "deferred entry carries a {} field. {}",
                rp(spec.at("forbidden_field").json()),
                spec.note("_why")
            ),
        );
    }
    if deferred.is_empty() {
        l.note("deferred[] is empty — no specialist gate fires on this corpus (expected)");
    }
}

fn check_accounting(
    context: &Context<'_>,
    design: &Json,
    inv: &Json,
    id_of: &IdOf,
    exp: &Spec<'_>,
) {
    let l = context.ledger;
    let spec = exp.at("accounting");
    let n = list(inv, "resources").len();
    let expected = spec.at("expected_inventory_resource_count").i64();
    l.check(
        i64::try_from(n).unwrap_or(i64::MAX) == expected,
        format!("the run's inventory has {n} resources, expected {expected} — the design is being checked against the wrong inventory"),
    );
    let mut accounted: BTreeSet<String> = entries(design)
        .iter()
        .map(|(e, _)| ps(get(e, "azure_id")))
        .collect();
    for w in warnings_of(design) {
        if truthy(get(w, "azure_id")) {
            accounted.insert(ps(get(w, "azure_id")));
        }
    }
    for (tf_addr, aid) in id_of {
        l.check(
            accounted.contains(aid),
            format!(
                "{} appears NOWHERE in the design — not mapped, not pending, not deferred, not skipped. {}",
                rs(tf_addr),
                spec.note("_why")
            ),
        );
    }
}

fn check_forbidden(context: &Context<'_>, design: &Json, exp: &Spec<'_>) {
    let spec = exp.at("forbidden_substrings");
    let blob = dumps(design);
    for bad in spec.at("values").strs() {
        context.ledger.check(
            !blob.contains(bad),
            format!(
                "{} appears in aws-design.json. {}",
                rs(bad),
                spec.note("_why")
            ),
        );
    }
}

pub(in crate::startup::fixtures) fn run(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let exp_doc = context.expectation("expected-design.json")?;
    let exp = context.spec(&exp_doc, "expected-design.json");

    let design_file = exp.at("design_file").str();
    let design = context.json_labeled(&context.run, design_file, design_file)?;

    let inventory_file = exp.at("inventory_file").str();
    let inv_dir = if context.exists(&context.run, inventory_file) {
        context.run.clone()
    } else {
        // The golden design tree does not duplicate the 23k inventory. A REAL
        // run always has both in the same directory; this fallback exists only
        // so the committed golden tree can be checked without a second copy.
        l.note(format!(
            "no inventory in the run dir; using the fixture's own golden at {inventory_file}"
        ));
        context.fixture.join("after-discover")
    };
    let inv = context.json_labeled(&inv_dir, inventory_file, inventory_file)?;

    let table_rel = exp.at("fast_path_table_relpath").str();
    let table = context.json_labeled(&context.fixture, table_rel, "fast-path-services.json")?;

    // tf_address -> azure_id, from the inventory the design was built from.
    let mut id_of = IdOf::new();
    for r in objs(get(&inv, "resources")) {
        let address = path(r, &["config", "tf_address"]);
        let aid = get(r, "azure_id");
        if truthy(address) && truthy(aid) {
            id_of.insert(ps(address), ps(aid));
        }
    }
    if id_of.is_empty() {
        l.fail("no resource in the inventory carries config.tf_address — cannot resolve identity");
        return None;
    }

    check_table(context, &table, &exp);
    check_canonical_type_coverage(context, &exp);
    check_index_reference_files_exist(context, &exp);
    check_sizing_provenance(context, &design, &exp);
    check_routing_provenance(context, &design, &exp);
    check_namespace_routing_targets(context, &exp);
    check_canonicalization_governance(context, &exp);
    check_halt_not_stale(context, &design, &exp);
    check_deterministic(context, &design, &table, &id_of, &exp);
    check_fan_in(context, &design, &id_of, &exp);
    check_unknown_stop(context, &design, &exp);
    check_inline_os_disk(context, &design, &exp);
    check_pending_rubric(context, &design, &exp);
    check_rubric_mappings(context, &design, &id_of, &exp);
    check_architecture_default(context, &design, &id_of, &exp);
    check_source_ha_finding(context, &design, &id_of, &exp);
    check_halt_covers_pending(context, &design, &exp);
    check_cluster_pattern_status(context, &design, &exp);
    check_compute_unit_fields(context, &design, &exp);
    check_skips(context, &design, &id_of, &exp);
    check_hard_blocker(context, &design, &id_of, &exp);
    check_deferred_shape(context, &design, &exp);
    check_accounting(context, &design, &inv, &id_of, &exp);
    check_forbidden(context, &design, &exp);
    Some(())
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn pattern_rule_mirrors_the_python_helpers() {
        assert_eq!(camel("linux_web_app"), "linuxWebApp");
        assert_eq!(plural("policy"), "policies");
        assert_eq!(plural("key"), "keys");
        assert_eq!(plural("box"), "boxes");
        assert_eq!(plural("account"), "accounts");
        assert!(segment_derivable(
            "azurerm_subnet",
            "Microsoft.Network/virtualNetworks/subnets"
        ));
        assert!(!segment_derivable(
            "azurerm_linux_web_app",
            "Microsoft.Web/sites"
        ));
        assert!(plausible_namespace("azurerm_iothub", "Microsoft.Devices"));
        assert!(!plausible_namespace("azurerm_iothub", "Microsoft.Kusto"));
    }

    #[test]
    fn ticked_tokens_follow_the_regex_findall_rules() {
        let cell = "see `azurerm_subnet` and `azurerm_x-y` and `azurerm_a_b`";
        assert_eq!(
            ticked(cell, "azurerm_", tf_class),
            vec!["azurerm_subnet", "azurerm_a_b"]
        );
        assert_eq!(
            ticked(
                "`Microsoft.Web/sites` `Microsoft.`",
                "Microsoft.",
                arm_class
            ),
            vec!["Microsoft.Web/sites"]
        );
        assert_eq!(
            md_files("| `compute.md` | `vendored/ai/x.md` | `Bad.md` |"),
            vec!["compute.md"]
        );
        assert_eq!(cells("| a | b |"), vec!["a", "b"]);
    }
}
