//! The azure-to-aws Discover asserter over the Terraform corpus
//! (`check_expected_iac_terraform.py`), the external oracle for Terraform
//! discovery.
//!
//! The skill DSL's `_assert` postconditions cannot verify correctness: a run
//! that improvises the ARM type vocabulary instead of consulting
//! `arm-type-canonicalization.md` still produces well-formed output and would
//! be labelled `confidence: deterministic`. So every check pins a fact where a
//! plausible improvisation and the correct answer diverge: the ARM types that
//! are not derivable from the Terraform name, the spelling that keeps
//! `azure_id` joins exact, the App Service Plan fan-in edges, the
//! cross-resource-group edge, the routing attributes and the secret boundary.

use std::collections::{BTreeMap, BTreeSet};

use fancy_regex::Regex;
use toolkit_core::Json;

use super::super::ledger::{
    dumps, eq, get, items, list, members, objs, path, ps, rl, rp, rs, text, truthy, Spec, NULL,
};
use super::super::{Context, Flow};

type Index<'a> = BTreeMap<String, &'a Json>;

/// The resource's identity: its full Terraform ADDRESS (`azurerm_subnet.data`).
///
/// It is the ADDRESS and not `tf_resource_name`, because Terraform namespaces
/// local names PER TYPE, so a bare local name is not unique (this corpus
/// collides on five of them). Read from provenance rather than from `name`,
/// because a resolved Azure name may be an unevaluated expression.
fn tf_name(resource: &Json) -> String {
    let address = path(resource, &["config", "tf_address"]);
    if truthy(address) {
        ps(address)
    } else {
        String::new()
    }
}

/// Index by Terraform address, failing loudly on a missing or duplicate one.
fn by_tf_name<'a>(context: &Context<'_>, resources: &'a [Json]) -> Index<'a> {
    let l = context.ledger;
    let mut out = BTreeMap::new();
    for resource in resources {
        let name = tf_name(resource);
        if name.is_empty() {
            let local = path(resource, &["config", "tf_resource_name"]);
            let local = if truthy(local) {
                local
            } else {
                get(resource, "azure_id")
            };
            let local = if truthy(local) {
                ps(local)
            } else {
                "?".to_string()
            };
            l.fail(format!(
                "{}: no config.tf_address. It is the identity field — see extract-terraform.md step 2.6; tf_resource_name alone is not unique.",
                rs(&local)
            ));
            continue;
        }
        if out.contains_key(&name) {
            l.fail(format!(
                "duplicate config.tf_address {} — addresses are unique in Terraform",
                rs(&name)
            ));
        }
        out.insert(name, resource);
    }
    out
}

fn edges_of(resource: &Json) -> impl Iterator<Item = &Json> {
    objs(get(resource, "edges"))
}

fn label(resource: &Json) -> String {
    let name = tf_name(resource);
    if name.is_empty() {
        "?".to_string()
    } else {
        name
    }
}

/// `re.match(pattern, text)`: anchored at the start.
fn matches_start(context: &Context<'_>, pattern: &str, subject: &str) -> bool {
    if let Ok(regex) = Regex::new(&format!("\\A(?:{pattern})")) {
        regex.is_match(subject).unwrap_or(false)
    } else {
        context.ledger.fail(format!(
            "expectation document is malformed: unusable pattern {}",
            rs(pattern)
        ));
        false
    }
}

fn check_types(context: &Context<'_>, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    for (local, expected) in exp.at("canonical_types").real_entries() {
        let Some(res) = index.get(local) else {
            l.fail(format!(
                "no inventory entry for Terraform address {}",
                rs(local)
            ));
            continue;
        };
        l.check(
            eq(get(res, "azure_type"), expected.json()),
            format!(
                "{}: azure_type is {}, expected {} (exact string; matching folds case but emission must not drift, because azure_id joins are exact — see arm-type-canonicalization.md)",
                rs(local),
                rp(get(res, "azure_type")),
                rp(expected.json())
            ),
        );
    }
}

fn check_forbidden_types(context: &Context<'_>, resources: &[Json], exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("forbidden_types");
    for bad in spec.at("values").arr() {
        l.check(
            !resources.iter().any(|r| eq(get(r, "azure_type"), bad.json())),
            format!(
                "forbidden azure_type present: {} — not a real ARM type string, or not the spelling arm-type-canonicalization.md emits. A wrong PROVIDER or type segment is a guessed translation; a case-only difference is a convention violation that breaks azure_id joins, not a wrong answer.",
                rp(bad.json())
            ),
        );
    }
    for r in resources {
        let kind = get(r, "azure_type");
        l.check(
            !text(kind).unwrap_or("").starts_with("azurerm_"),
            format!(
                "{}: azure_type {} is a Terraform type — it was never translated",
                label(r),
                rp(kind)
            ),
        );
    }
}

fn check_function_app(context: &Context<'_>, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("function_app");
    let local = spec.at("local_name").str();
    let Some(res) = index.get(local) else {
        l.fail(format!(
            "no inventory entry for the function app {}",
            rs(local)
        ));
        return;
    };
    l.check(
        eq(get(res, "azure_type"), spec.at("azure_type").json()),
        format!(
            "function app: azure_type is {}, expected {} — Microsoft.Web/functionApps does not exist",
            rp(get(res, "azure_type")),
            rp(spec.at("azure_type").json())
        ),
    );
    let kind = path(res, &["config", "kind"]);
    let kind = if truthy(kind) {
        text(kind).unwrap_or("").to_lowercase()
    } else {
        String::new()
    };
    l.check(
        kind.contains(spec.at("kind_contains").str()),
        format!(
            "function app: config.kind is {} and does not contain {} — kind is the ONLY thing separating a function app from a web app, so dropping it makes the two indistinguishable",
            rs(&kind),
            rs(spec.at("kind_contains").str())
        ),
    );
}

fn check_azure_ids(context: &Context<'_>, resources: &[Json], exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("azure_id");
    let placeholder = spec.at("subscription_placeholder").str();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut saw_placeholder = false;
    for r in resources {
        let aid = text(get(r, "azure_id")).unwrap_or("");
        let local = label(r);
        if aid.contains(placeholder) {
            saw_placeholder = true;
        }
        if let Some(first) = seen.get(aid) {
            l.fail(format!(
                "azure_id collision between {} and {}: {aid}",
                rs(first),
                rs(&local)
            ));
        }
        seen.insert(aid.to_string(), local.clone());
        if eq(
            get(r, "azure_type"),
            &Json::string("Microsoft.Resources/resourceGroups"),
        ) {
            l.check(
                matches_start(context, spec.at("resource_group_form_regex").str(), aid),
                format!(
                    "{}: a resource group's own azure_id must be /subscriptions/<s>/resourceGroups/<rg> with NO /providers/ segment, got {}",
                    rs(&local),
                    rs(aid)
                ),
            );
            l.check(
                !aid.contains("/providers/"),
                format!(
                    "{}: resource-group azure_id wrongly carries a /providers/ segment: {}",
                    rs(&local),
                    rs(aid)
                ),
            );
        } else {
            l.check(
                matches_start(context, spec.at("standard_form_regex").str(), aid),
                format!(
                    "{}: azure_id does not match the standard ARM form: {}",
                    rs(&local),
                    rs(aid)
                ),
            );
        }
    }
    l.check(
        saw_placeholder,
        format!(
            "no azure_id carries the {} placeholder — the corpus's provider block declares no subscription_id, so a real GUID here means one was invented, which makes azure_id unstable across runs and breaks drift comparison against a live capture",
            rs(placeholder)
        ),
    );
}

fn check_metadata(context: &Context<'_>, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let meta = get(inv, "metadata");
    let sources = get(meta, "discovery_sources");
    let want_sources = exp.at("discovery_sources");
    l.check(
        eq(sources, want_sources.json()),
        format!(
            "metadata.discovery_sources is {}, expected {} — only sources that actually contributed may be listed",
            rp(sources),
            rp(want_sources.json())
        ),
    );
    let iac = get(inv, "iac_metadata");
    let want = exp.at("azure_id").at("subscription_id_source");
    l.check(
        eq(get(iac, "subscription_id_source"), want.json()),
        format!(
            "iac_metadata.subscription_id_source is {}, expected {}",
            rp(get(iac, "subscription_id_source")),
            rp(want.json())
        ),
    );
    let counts = exp.at("resource_counts");
    let actual = list(inv, "resources").len();
    let expected = counts.at("expected_in_inventory").i64();
    l.check(
        i64::try_from(actual).unwrap_or(i64::MAX) == expected,
        format!(
            "inventory has {actual} resources, expected {expected} ({} declared, minus the untranslated type)",
            super::super::ledger::ps(counts.at("declared_in_corpus").json())
        ),
    );
}

/// The Discover-written Clarify fast-path verdict (discover-assemble.md rule
/// 9). It is where an improvising run and a correct run diverge most cheaply:
/// a run that skips the rule writes no key at all, and one that eyeballs
/// "small startup estate" marks this corpus eligible although it has VMs, a
/// zone-redundant Postgres and five clusters.
fn check_clarify_fast_path(context: &Context<'_>, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("clarify_fast_path");
    let verdict = path(inv, &["metadata", "clarify_fast_path"]);
    l.check(
        matches!(verdict, Json::Object(_)),
        "metadata.clarify_fast_path is missing — discover-assemble.md § Assembly rule 9 requires Discover to write the Clarify fast-path verdict; without it Clarify falls back to the wizard and the short path is never offered",
    );
    if !matches!(verdict, Json::Object(_)) {
        return;
    }
    let eligible = get(verdict, "eligible");
    let reasons = get(verdict, "reasons_ineligible");
    let want_eligible = spec.at("eligible");
    l.check(
        eligible == want_eligible.json(),
        format!(
            "metadata.clarify_fast_path.eligible is {}, expected {} — this corpus has VMs, a ZoneRedundant Postgres, and {} clusters, each of which independently disqualifies the short path",
            rp(eligible),
            rp(want_eligible.json()),
            ps(spec.get("_cluster_count").json())
        ),
    );
    l.check(
        matches!(reasons, Json::Array(_)),
        format!(
            "metadata.clarify_fast_path.reasons_ineligible is {}, expected a list",
            rp(reasons)
        ),
    );
    let Json::Array(reason_list) = reasons else {
        return;
    };
    let allowed: BTreeSet<&str> = spec.at("allowed_reasons").strs().into_iter().collect();
    let got: BTreeSet<String> = reason_list.iter().map(ps).collect();
    let unknown: Vec<String> = got
        .iter()
        .filter(|reason| !allowed.contains(reason.as_str()))
        .cloned()
        .collect();
    l.check(
        unknown.is_empty(),
        format!(
            "reasons_ineligible contains {}, outside the closed vocabulary {} (the rule-9 input names)",
            rl(&unknown),
            rl(&allowed.iter().copied().collect::<Vec<_>>())
        ),
    );
    let missing: Vec<&str> = spec
        .at("reasons_must_include")
        .strs()
        .into_iter()
        .filter(|reason| !got.contains(*reason))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    l.check(
        missing.is_empty(),
        format!(
            "reasons_ineligible is {}, missing {} — rule 9 says record EVERY failing input, not just the first, so Clarify can explain the full flow",
            rp(reasons),
            rl(&missing)
        ),
    );
    l.check(
        matches!(eligible, Json::Bool(false)) != reason_list.is_empty(),
        "reasons_ineligible must be non-empty iff eligible is false",
    );
}

fn check_fan_in(context: &Context<'_>, index: &Index<'_>, resources: &[Json], exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("app_service_plan_fan_in");
    let plan_name = spec.at("plan_local_name").str();
    let Some(plan) = index.get(plan_name) else {
        l.fail(format!(
            "no inventory entry for the App Service Plan {}",
            rs(plan_name)
        ));
        return;
    };
    let cfg = get(plan, "config");
    l.check(
        eq(get(cfg, "sku_name"), spec.at("plan_sku_name").json()),
        format!(
            "plan config.sku_name is {}, expected {} — the plan's SKU is what is being paid for",
            rp(get(cfg, "sku_name")),
            rp(spec.at("plan_sku_name").json())
        ),
    );
    l.check(
        eq(get(cfg, "worker_count"), spec.at("plan_worker_count").json()),
        format!(
            "plan config.worker_count is {}, expected {} — sizing comes from the plan's instance count, never from the number of apps",
            rp(get(cfg, "worker_count")),
            rp(spec.at("plan_worker_count").json())
        ),
    );
    let plan_id = get(plan, "azure_id");
    for (key, want) in [
        ("plan_local_name", spec.at("expected_hosted_on_edge_count")),
        (
            "idle_plan_local_name",
            spec.at("idle_plan_expected_hosted_on_edge_count"),
        ),
    ] {
        let name = spec.at(key).str();
        let Some(target) = index.get(name) else {
            l.fail(format!("no inventory entry for plan {}", rs(name)));
            continue;
        };
        let tid = get(target, "azure_id");
        let n = resources
            .iter()
            .flat_map(edges_of)
            .filter(|edge| {
                matches!(get(edge, "type"), Json::String(t) if t == "hosted_on")
                    && eq(get(edge, "to"), tid)
            })
            .count();
        l.check(
            eq(&Json::Int(i64::try_from(n).unwrap_or(i64::MAX)), want.json()),
            format!(
                "plan {} has {n} inbound hosted_on edge(s), expected {}. These edges are what stop five apps on one plan from becoming five compute line items — a 5x estimate error that is unrecoverable by Design.",
                rs(name),
                ps(want.json())
            ),
        );
    }
    if truthy(plan_id) {
        l.note(format!("plan {} azure_id = {}", rs(plan_name), ps(plan_id)));
    }
}

/// Asserts the SPECIFIC app-to-database edge, not merely that some edge
/// crosses: the corpus also has crossing subnet edges, and accepting any
/// crossing edge would let an extractor that drops the app-to-data reference
/// pass, which is the one edge the horizontal-resource-group merge depends on.
fn check_cross_rg_edge(context: &Context<'_>, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("cross_resource_group_edge");
    let from = spec.at("from_tf_address").str();
    let to = spec.at("to_tf_address").str();
    let (Some(src), Some(dst)) = (index.get(from), index.get(to)) else {
        l.fail(format!(
            "cannot check the cross-resource-group edge: missing {} or {}",
            rs(from),
            rs(to)
        ));
        return;
    };
    l.check(
        eq(
            get(src, "resource_group"),
            spec.at("from_resource_group").json(),
        ),
        format!(
            "{} resource_group is {}, expected {}",
            rs(from),
            rp(get(src, "resource_group")),
            rp(spec.at("from_resource_group").json())
        ),
    );
    l.check(
        eq(
            get(dst, "resource_group"),
            spec.at("to_resource_group").json(),
        ),
        format!(
            "{} resource_group is {}, expected {}",
            rs(to),
            rp(get(dst, "resource_group")),
            rp(spec.at("to_resource_group").json())
        ),
    );
    let target = get(dst, "azure_id");
    let acceptable = spec.at("acceptable_edge_types");
    let accepted = acceptable.strs();
    let found = edges_of(src).find(|edge| {
        eq(get(edge, "to"), target)
            && text(get(edge, "type")).is_some_and(|kind| accepted.contains(&kind))
    });
    let types = accepted.join("/");
    l.check(
        found.is_some(),
        format!(
            "no {types} edge from {} (in {}) to {} (in {}). The app's DATABASE_HOST setting interpolates the server's fqdn, and that edge is the ONLY thing that later merges these two resource groups into one cluster — resource-group seeding alone would leave the app and its database apart.",
            rs(from),
            ps(spec.at("from_resource_group").json()),
            rs(to),
            ps(spec.at("to_resource_group").json())
        ),
    );
    if let Some(edge) = found {
        l.note(format!(
            "app-to-data edge preserved across rg boundary: {}",
            ps(get(edge, "type"))
        ));
    }
}

fn check_private_endpoint(
    context: &Context<'_>,
    index: &Index<'_>,
    resources: &[Json],
    inv: &Json,
    exp: &Spec<'_>,
) {
    let l = context.ledger;
    let spec = exp.at("private_endpoint");
    let local = spec.at("local_name").str();
    if let Some(pe) = index.get(local) {
        l.check(
            eq(get(pe, "azure_type"), spec.at("azure_type").json()),
            format!(
                "private endpoint azure_type is {}, expected {}",
                rp(get(pe, "azure_type")),
                rp(spec.at("azure_type").json())
            ),
        );
    } else {
        l.fail(format!(
            "no inventory entry for private endpoint {} — it must be inventoried even though it is not a mapping target, because a resource absent from the inventory cannot be reported as skipped",
            rs(local)
        ));
    }
    let edge_type = spec.at("must_produce_edge_type");
    l.check(
        resources
            .iter()
            .flat_map(edges_of)
            .any(|edge| eq(get(edge, "type"), edge_type.json())),
        format!(
            "no {} edge anywhere — the private endpoint's private_connection_resource_id is the explicit app-to-data edge and is the whole reason the endpoint is read at all",
            rp(edge_type.json())
        ),
    );
    if spec.at("must_appear_in_warnings").truthy() {
        let warnings = dumps(&Json::Array(list(inv, "warnings").to_vec()));
        l.check(
            warnings.contains(local)
                || warnings.contains("privateEndpoint")
                || warnings.to_lowercase().contains("private endpoint"),
            "no warnings[] entry names the consumed private endpoint — each one must be recorded with the edge it produced",
        );
    }
}

fn check_secrets(context: &Context<'_>, inv: &Json, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.at("secrets");
    let blob = dumps(inv);
    let sentinel = spec.at("sentinel").str();
    l.check(
        !blob.contains(sentinel),
        format!(
            "the secret sentinel {} appears in the artifact. App-setting values, connection strings and passwords must be DISCARDED, not recorded and not redacted in place — a redaction placeholder still discloses that the field existed.",
            rs(sentinel)
        ),
    );
    for resource in objs(get(inv, "resources")) {
        let cfg = get(resource, "config");
        for bad in spec.get("forbidden_config_keys").arr_or_empty() {
            l.check(
                cfg.get(bad.str()).is_none(),
                format!(
                    "{}: config carries {}. Values are DISCARDED, so the container must not exist — a redaction placeholder still discloses that the field was there and roughly how long it was, which is why asserting only on the secret's text is not enough.",
                    label(resource),
                    rs(bad.str())
                ),
            );
        }
    }
    let owner_name = spec.at("app_setting_names_owner_local_name").str();
    let Some(owner) = index.get(owner_name) else {
        l.fail(format!("no inventory entry for {}", rs(owner_name)));
        return;
    };
    let names = path(owner, &["config", "app_setting_names"]);
    let mut got: Vec<String> = items(names).iter().map(ps).collect();
    got.sort();
    let mut want: Vec<String> = spec
        .at("app_setting_names_expected")
        .strs()
        .into_iter()
        .map(str::to_string)
        .collect();
    want.sort();
    l.check(
        matches!(names, Json::Array(_)) && got == want,
        format!(
            "config.app_setting_names is {}, expected {} — names are kept, values never are",
            rp(names),
            rl(&want)
        ),
    );
}

fn check_child_rg_inheritance(context: &Context<'_>, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("child_resource_group_inheritance");
    if !spec.truthy() {
        return;
    }
    let address = spec.at("tf_address").str();
    let parent_address = spec.at("parent_tf_address").str();
    let (Some(child), Some(parent)) = (index.get(address), index.get(parent_address)) else {
        l.fail(format!(
            "cannot check child resource-group inheritance: missing {} or {}",
            rs(address),
            rs(parent_address)
        ));
        return;
    };
    l.check(
        eq(get(child, "resource_group"), spec.at("expected_resource_group").json()),
        format!(
            "{} resource_group is {}, expected {}. It declares no resource_group_name and must inherit its parent's via the storage_account_id reference — a null group cannot be clustered, and most child resources in a real estate look like this.",
            rs(address),
            rp(get(child, "resource_group")),
            rp(spec.at("expected_resource_group").json())
        ),
    );
    l.check(
        eq(get(child, "resource_group"), get(parent, "resource_group")),
        format!(
            "{} resource_group {} does not match its parent {} ({})",
            rs(address),
            rp(get(child, "resource_group")),
            rs(parent_address),
            rp(get(parent, "resource_group"))
        ),
    );
}

fn check_warnings(context: &Context<'_>, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let warnings = format!(
        "{}{}",
        dumps(&Json::Array(list(inv, "warnings").to_vec())),
        dumps(&members_or_empty(get(inv, "iac_metadata")))
    );
    let meta = get(inv, "iac_metadata");
    let untranslated = exp.at("untranslated_types");
    let why = untranslated.note("_why");
    for t in untranslated.at("expected").strs() {
        l.check(
            warnings.contains(t),
            format!("{} must be reported as untranslated. {why}", rs(t)),
        );
    }
    if untranslated.get("must_be_empty_in_artifact").truthy() {
        let actual = get(meta, "untranslated_types");
        l.check(
            matches!(actual, Json::Array(values) if values.is_empty()),
            format!(
                "iac_metadata.untranslated_types is {}, expected []. That field now means 'the skill cannot NAME the service'. A type with no table row is DERIVED and kept; a type whose namespace is unrecognised is also kept. {why}",
                rp(actual)
            ),
        );
    }
    let derived = exp.get("derived_types");
    if derived.truthy() {
        let actual = get(meta, "derived_types");
        for t in derived.at("expected").arr() {
            l.check(
                items(actual).iter().any(|candidate| eq(candidate, t.json())),
                format!(
                    "{} is absent from arm-type-canonicalization.md, so it must appear in iac_metadata.derived_types — derived and RETAINED, not dropped. Got {}. {}",
                    rp(t.json()),
                    rp(actual),
                    derived.note("_why")
                ),
            );
        }
    }
    let sub = exp.at("unresolved_modules").at("expected_substring").str();
    l.check(
        warnings.contains(sub),
        format!(
            "no warning names the unresolvable module {}. A registry module whose content is not in the workspace is the commonest reason a Terraform-only inventory is confidently incomplete.",
            rs(sub)
        ),
    );
}

/// `inv.get("iac_metadata") or {}`.
fn members_or_empty(value: &Json) -> Json {
    if truthy(value) {
        value.clone()
    } else {
        Json::Object(Vec::new())
    }
}

fn check_discriminators(context: &Context<'_>, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    for (label, spec) in exp.at("routing_discriminators").real_entries() {
        let local = spec.at("local_name").str();
        let Some(res) = index.get(local) else {
            l.fail(format!("no inventory entry for {} ({label})", rs(local)));
            continue;
        };
        let key = spec.at("config_key").str();
        let actual = path(res, &["config", key]);
        l.check(
            eq(actual, spec.at("value").json()),
            format!(
                "{label}: config.{key} is {}, expected {}. This attribute is the SOLE input to a later rubric decision — dropping it fails no shape assertion, it just makes the rubric guess.",
                rp(actual),
                rp(spec.at("value").json())
            ),
        );
    }
}

fn check_downloaded_module(context: &Context<'_>, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("downloaded_module");
    if !spec.truthy() {
        return;
    }
    for addr in spec.at("tf_addresses").strs() {
        let Some(res) = index.get(addr) else {
            l.fail(format!(
                "{} is absent from the inventory. It lives in a DOWNLOADED registry module under .terraform/modules/, which is readable. {}",
                rs(addr),
                spec.note("_why")
            ));
            continue;
        };
        let module = path(res, &["config", "tf_module"]);
        l.check(
            eq(module, spec.at("expected_tf_module").json()),
            format!(
                "{}: config.tf_module is {}, expected {} — without it the report cannot say the resources came from a module rather than the root configuration",
                rs(addr),
                rp(module),
                rp(spec.at("expected_tf_module").json())
            ),
        );
    }
}

fn check_module_not_over_warned(context: &Context<'_>, inv: &Json, exp: &Spec<'_>) {
    let spec = exp.get("downloaded_module");
    if !spec.truthy() {
        return;
    }
    for w in list(inv, "warnings") {
        if eq(get(w, "code"), spec.at("must_not_be_warned_as").json()) {
            let blob = dumps(w);
            context.ledger.check(
                !blob.contains(spec.at("expected_tf_module").str()) && !blob.contains("naming"),
                format!(
                    "module.naming is warned as {}, but its source IS on disk under .terraform/modules/. {}",
                    rp(spec.at("must_not_be_warned_as").json()),
                    spec.note("_must_not_be_warned_why")
                ),
            );
        }
    }
}

fn check_association_only(context: &Context<'_>, index: &Index<'_>, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("association_only_resource");
    if !spec.truthy() {
        return;
    }
    let addr = spec.at("tf_address").str();
    let why = spec.note("_why");
    if spec.at("must_not_be_in_inventory").truthy() {
        l.check(
            !index.contains_key(addr),
            format!(
                "{} has an inventory entry. It is an ASSOCIATION and has no ARM type at all, so an entry invents a resource that does not exist. {why}",
                rs(addr)
            ),
        );
    }
    if spec.at("must_not_be_untranslated").truthy() {
        let blob = format!(
            "{}{}",
            dumps(&Json::Array(list(inv, "warnings").to_vec())),
            dumps(&members_or_empty(get(inv, "iac_metadata")))
        );
        l.check(
            !blob.contains("subnet_route_table_association"),
            format!("{} is reported as an untranslated type. {why}", rs(addr)),
        );
    }
    let e = spec.at("expected_edge");
    let (from, to) = (e.at("from_tf_address").str(), e.at("to_tf_address").str());
    let (Some(src), Some(dst)) = (index.get(from), index.get(to)) else {
        l.fail(format!(
            "cannot check the association edge: missing {} or {}",
            rs(from),
            rs(to)
        ));
        return;
    };
    l.check(
        edges_of(src).any(|x| eq(get(x, "type"), e.at("type").json()) && eq(get(x, "to"), get(dst, "azure_id"))),
        format!(
            "no {} edge from {} to {}. The association's whole contribution is that edge — drop it and the association was read for nothing.",
            rp(e.at("type").json()),
            rs(from),
            rs(to)
        ),
    );
}

/// The clusters artifact. Clustering shipped ungated; this is that gap closed.
fn check_clustering(context: &Context<'_>, inv: &Json, index: &Index<'_>, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("clustering");
    if !spec.truthy() {
        return;
    }
    let file = exp.at("clusters_file").str();
    if !context.exists(&context.run, file) {
        l.fail(format!(
            "missing {file} — clustering is a declared phase output. {}",
            spec.note("_why")
        ));
        return;
    }
    let Some(doc) = context.json_in_dir(&context.run, file) else {
        return;
    };
    let clusters = list(&doc, "clusters");
    let ids: Vec<String> = clusters.iter().map(|c| rp(get(c, "cluster_id"))).collect();
    let expected_count = spec.at("expected_cluster_count").i64();
    l.check(
        i64::try_from(clusters.len()).unwrap_or(i64::MAX) == expected_count,
        format!(
            "{} cluster(s), expected {expected_count}: [{}]. {}",
            clusters.len(),
            ids.join(", "),
            spec.note("_expected_cluster_count_why")
        ),
    );
    let by_id: BTreeMap<String, &Json> = clusters
        .iter()
        .map(|c| (ps(get(c, "cluster_id")), c))
        .collect();
    let id_of =
        |address: &str| -> &Json { index.get(address).map_or(&NULL, |r| get(r, "azure_id")) };

    for (cid, want) in spec.at("expected_clusters").entries() {
        let why = want.note("why");
        let Some(c) = by_id.get(cid) else {
            l.fail(format!("no cluster {}. {why}", rs(cid)));
            continue;
        };
        l.check(
            eq(get(c, "justification"), want.at("justification").json()),
            format!(
                "{cid}: justification is {}, expected {}. {why}",
                rp(get(c, "justification")),
                rp(want.at("justification").json())
            ),
        );
        let member_count = list(c, "members").len();
        l.check(
            i64::try_from(member_count).unwrap_or(i64::MAX) == want.at("member_count").i64(),
            format!(
                "{cid}: {member_count} members, expected {}. {why}",
                ps(want.at("member_count").json())
            ),
        );
        let primary = want.at("primary_tf_address").str();
        l.check(
            eq(get(c, "primary"), id_of(primary)),
            format!(
                "{cid}: primary is not {}. See classification-rules.md ranks.",
                rs(primary)
            ),
        );
        for a in want.get("must_contain_tf_addresses").arr_or_empty() {
            let wanted = id_of(a.str());
            l.check(
                list(c, "members").iter().any(|m| eq(m, wanted)),
                format!("{cid}: does not contain {}. {why}", rs(a.str())),
            );
        }
    }

    for c in clusters {
        let justification = get(c, "justification");
        let j = if truthy(justification) {
            text(justification).unwrap_or("")
        } else {
            ""
        };
        let cid = ps(get(c, "cluster_id"));
        if j.starts_with("merge:") && spec.at("merge_must_have_edges").truthy() {
            l.check(
                truthy(get(c, "edges")),
                format!(
                    "{cid}: justification {} with an EMPTY edges[]. A merge must show the crossing edges that caused it, or the user cannot validate it on the sheet.",
                    rs(j)
                ),
            );
        }
        if j.starts_with("split:") && spec.at("split_may_have_empty_edges").truthy() {
            let why: String = spec.note("_split_edges_why").chars().take(60).collect();
            l.note(format!(
                "{cid}: split with empty edges[] (correct) — {why}…"
            ));
        }
        let primary = get(c, "primary");
        for r in objs(get(inv, "resources")) {
            if eq(get(r, "azure_id"), primary) {
                l.check(
                    !eq(
                        get(r, "azure_type"),
                        spec.at("forbidden_primary_type").json(),
                    ),
                    format!(
                        "{cid}: primary is a {}. {}",
                        ps(spec.at("forbidden_primary_type").json()),
                        spec.note("_forbidden_primary_why")
                    ),
                );
            }
        }
    }

    let members_all: Vec<String> = clusters
        .iter()
        .flat_map(|c| list(c, "members"))
        .map(ps)
        .collect();
    let unique: BTreeSet<&String> = members_all.iter().collect();
    l.check(
        members_all.len() == unique.len(),
        "a resource is a member of two clusters — merging must union members, not duplicate them",
    );
    let unclustered: BTreeSet<String> = list(&doc, "unclustered").iter().map(ps).collect();
    let missing = objs(get(inv, "resources"))
        .map(|r| ps(get(r, "azure_id")))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|id| !unique.contains(id) && !unclustered.contains(id))
        .count();
    l.check(
        missing == 0,
        format!("{missing} inventory resource(s) are in no cluster and not in unclustered[] — clustering must account for every resource"),
    );
}

/// The parts of the contract a correct-by-the-ref run can still get wrong:
/// every check exists because a fresh-context capability run diverged from the
/// golden tree on something no ref stated.
fn check_contract_vocabulary(context: &Context<'_>, inv: &Json, exp: &Spec<'_>) {
    let l = context.ledger;
    let spec = exp.get("contract_vocabulary");
    if !spec.truthy() {
        return;
    }
    let allowed_codes: Vec<&str> = spec.at("warning_codes").strs();
    let warnings = get(inv, "warnings");
    if let Json::Array(entries) = warnings {
        for w in entries {
            let code = get(w, "code");
            l.check(
                text(code).is_some_and(|c| allowed_codes.contains(&c)),
                format!(
                    "warning code {} is not in the closed vocabulary {}. {}",
                    rp(code),
                    rl(&sorted(&allowed_codes)),
                    spec.note("_warning_codes_why")
                ),
            );
            for key in spec.at("warning_required_keys").strs() {
                l.check(
                    truthy(get(w, key)),
                    format!("warning {} has no {}", rp(code), rs(key)),
                );
            }
            let one_of = spec.at("warning_required_one_of").strs();
            l.check(
                one_of.iter().any(|key| truthy(get(w, key))),
                format!(
                    "warning {} has neither {} — a warning nobody can attribute to anything is noise",
                    rp(code),
                    one_of.join(" nor ")
                ),
            );
        }
    } else {
        l.fail("inventory has no top-level warnings[] array. Three files mandate writing to it; it is defined in schema-discover-azure.md § Warnings and is always present, [] when clean.");
    }

    let allowed_edges: Vec<&str> = spec.at("edge_types").strs();
    for r in objs(get(inv, "resources")) {
        for e in edges_of(r) {
            l.check(
                text(get(e, "type")).is_some_and(|t| allowed_edges.contains(&t)),
                format!(
                    "{}: edge type {} is not in the canonical set {}. {}",
                    label(r),
                    rp(get(e, "type")),
                    rl(&sorted(&allowed_edges)),
                    spec.note("_edge_types_why")
                ),
            );
        }
    }

    if spec.get("forbid_null_config_values").truthy() {
        for r in objs(get(inv, "resources")) {
            for (key, value) in members(get(r, "config")) {
                l.check(
                    !matches!(value, Json::Null),
                    format!(
                        "{}: config.{key} is null. {}",
                        label(r),
                        spec.note("_forbid_null_why")
                    ),
                );
            }
        }
    }
}

fn sorted<'a>(values: &[&'a str]) -> Vec<&'a str> {
    let mut out = values.to_vec();
    out.sort_unstable();
    out
}

fn check_required_config_fields(context: &Context<'_>, resources: &[Json], exp: &Spec<'_>) {
    let spec = exp.get("required_config_fields");
    if !spec.truthy() {
        return;
    }
    for r in resources {
        let Some(kind) = text(get(r, "azure_type")) else {
            continue;
        };
        let want = spec.get(kind);
        if !want.truthy() {
            continue;
        }
        for field in want.arr() {
            context.ledger.check(
                get(r, "config").get(field.str()).is_some(),
                format!(
                    "{} ({kind}): config is missing {}. {}",
                    label(r),
                    rs(field.str()),
                    spec.note("_why")
                ),
            );
        }
    }
}

fn check_no_terraform_leakage(context: &Context<'_>, inv: &Json, exp: &Spec<'_>) {
    // tf_file / tf_resource_name are legitimate provenance, so scan azure_type only.
    let bads = exp.at("forbidden_artifact_substrings").at("values").strs();
    for r in objs(get(inv, "resources")) {
        let kind = text(get(r, "azure_type")).unwrap_or("");
        for bad in &bads {
            context.ledger.check(
                !kind.contains(bad),
                format!("{}: azure_type {} contains {}", label(r), rs(kind), rs(bad)),
            );
        }
    }
}

pub(in crate::startup::fixtures) fn run(context: &Context<'_>) -> Flow {
    let l = context.ledger;
    let exp_doc = context.expectation("expected-iac-terraform.json")?;
    let exp = context.spec(&exp_doc, "expected-iac-terraform.json");
    let inv = context.json_in_dir(&context.run, exp.at("inventory_file").str())?;

    let resources = list(&inv, "resources");
    if resources.is_empty() {
        l.fail("inventory resources[] is missing or empty");
        return None;
    }
    let index = by_tf_name(context, resources);

    check_metadata(context, &inv, &exp);
    check_clarify_fast_path(context, &inv, &exp);
    check_types(context, &index, &exp);
    check_forbidden_types(context, resources, &exp);
    check_function_app(context, &index, &exp);
    check_azure_ids(context, resources, &exp);
    check_fan_in(context, &index, resources, &exp);
    check_cross_rg_edge(context, &index, &exp);
    check_private_endpoint(context, &index, resources, &inv, &exp);
    check_secrets(context, &inv, &index, &exp);
    check_child_rg_inheritance(context, &index, &exp);
    check_warnings(context, &inv, &exp);
    check_discriminators(context, &index, &exp);
    check_clustering(context, &inv, &index, &exp);
    check_downloaded_module(context, &index, &exp);
    check_module_not_over_warned(context, &inv, &exp);
    check_association_only(context, &index, &inv, &exp);
    check_contract_vocabulary(context, &inv, &exp);
    check_required_config_fields(context, resources, &exp);
    check_no_terraform_leakage(context, &inv, &exp);
    Some(())
}
