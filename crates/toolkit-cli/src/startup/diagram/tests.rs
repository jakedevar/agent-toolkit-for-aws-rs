//! Offline, deterministic tests of `startup build-diagram` (#4402): the ported
//! `test_build_diagram.py`, the `build_diagram` assertions of
//! `test_unit_grouping.py` and `test_collapse_invariant.py` (#4613), and the
//! negative and hostile-input cases the Python never covered.

#![allow(clippy::too_many_lines)]

use std::fs;

use toolkit_core::Json;

use super::*;
use crate::startup::scoring::fixtures::{exec, j, path_text, write, TempDir};

/// ESC, BEL, CR, LF, U+202E and a secret-looking marker in one value.
const HOSTILE: &str = "x\u{1b}[31m\u{7}\r\nb\u{202e}y token=FAKESECRET4402";

fn svc(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn diagram(result: &str, confirm: &str, design: Option<&str>) -> Diagram {
    let design = design.map(j);
    build_diagram(&j(result), &j(confirm), design.as_ref()).expect("the documents compose")
}

fn failure(result: &str, confirm: &str, design: Option<&str>) -> String {
    let design = design.map(j);
    build_diagram(&j(result), &j(confirm), design.as_ref())
        .expect_err("the documents are malformed")
        .to_string()
}

fn mermaid(result: &str, design: &str) -> String {
    diagram(result, "{}", Some(design)).mermaid
}

fn json_string(text: &str) -> String {
    Json::string(text).to_compact_string()
}

fn assert_clean(text: &str) {
    for raw in ['\u{1b}', '\u{7}', '\r', '\u{202e}'] {
        assert!(!text.contains(raw), "raw {raw:?} in {text:?}");
    }
}

// ---- resolve_runtime / resolve_services ------------------------------------

#[test]
fn resolve_runtime_single_winner() {
    assert_eq!(
        resolve_runtime(&j(r#"{"verdict":"agentcore"}"#), &j("{}")),
        Ok(Json::string("agentcore"))
    );
}

#[test]
fn resolve_runtime_co_recommend_uses_the_confirmed_choice_then_the_first() {
    let result = j(r#"{"verdict":"co_recommend","co_recommend":["ecs","eks"]}"#);
    assert_eq!(
        resolve_runtime(&result, &j(r#"{"chosen_runtime":"eks"}"#)),
        Ok(Json::string("eks"))
    );
    assert_eq!(resolve_runtime(&result, &j("{}")), Ok(Json::string("ecs")));
    assert_eq!(
        resolve_runtime(&result, &j(r#"{"chosen_runtime":""}"#)),
        Ok(Json::string("ecs")),
        "an empty choice is no choice"
    );
}

#[test]
fn resolve_runtime_without_a_choice_list_is_none_and_a_missing_verdict_is_null() {
    assert_eq!(
        resolve_runtime(&j(r#"{"verdict":"co_recommend"}"#), &j("{}")),
        Ok(Json::string("none"))
    );
    assert_eq!(resolve_runtime(&j("{}"), &j("{}")), Ok(Json::Null));
}

#[test]
fn resolve_runtime_no_viable() {
    assert_eq!(
        resolve_runtime(&j(r#"{"verdict":"no_viable_runtime"}"#), &j("{}")),
        Ok(Json::string("none"))
    );
}

#[test]
fn resolve_runtime_reports_a_bad_choice_list_without_its_value() {
    for list in ["[]", r#""""#, "5", "null", "{}"] {
        let result = j(&format!(
            r#"{{"verdict":"co_recommend","co_recommend":{list}}}"#
        ));
        let error = resolve_runtime(&result, &j("{}")).expect_err(list);
        assert!(
            error.to_string().contains("co_recommend"),
            "{list}: {error}"
        );
    }
}

#[test]
fn labels_cover_all_runtimes_and_services() {
    for id in ["agentcore", "lambda_microvms", "ecs", "eks", "lambda"] {
        assert!(RUNTIME_LABELS.iter().any(|(known, _)| *known == id), "{id}");
    }
    assert_eq!(service_label("payments"), Some("Payments"));
    assert_eq!(service_label("registry"), Some("Registry"));
    assert_eq!(service_label("bogus"), None);
    assert_eq!(runtime_label("serverless_workers").len(), 44);
    assert_eq!(runtime_label("unlisted"), "unlisted");
}

#[test]
fn handoff_runtimes_are_exactly_ecs_eks_fargate_and_batch() {
    // test_handoff_required_scans_all_units_effective_runtime (#4613)
    let mut set = HANDOFF_RUNTIMES.to_vec();
    set.sort_unstable();
    assert_eq!(set, ["batch", "ecs", "eks", "fargate"]);
}

#[test]
fn resolve_services_prefers_confirm_then_result() {
    let result = j(r#"{"agentcore_services":["identity","observability"]}"#);
    let confirm = j(r#"{"agentcore_services":["identity","memory","gateway"]}"#);
    assert_eq!(
        resolve_services(&result, &confirm),
        Ok(svc(&["identity", "memory", "gateway"]))
    );
    assert_eq!(
        resolve_services(&result, &j("{}")),
        Ok(svc(&["identity", "observability"]))
    );
}

#[test]
fn resolve_services_filters_unknown_and_dedupes() {
    let confirm = j(r#"{"agentcore_services":["identity","identity","bogus","memory",5,null]}"#);
    assert_eq!(
        resolve_services(&j("{}"), &confirm),
        Ok(svc(&["identity", "memory"]))
    );
}

#[test]
fn resolve_services_rejects_a_non_list_without_its_value() {
    let error = resolve_services(&j(r#"{"agentcore_services":"identity"}"#), &j("{}"))
        .expect_err("a string is not a service list");
    assert!(error.to_string().contains("agentcore_services"));
    assert!(!error.to_string().contains("identity"), "{error}");
}

// ---- single unit renderers -------------------------------------------------

#[test]
fn payments_and_registry_render() {
    let out = render_mermaid(
        "agentcore",
        &svc(&["identity", "payments", "registry"]),
        Some("claude_sonnet_5"),
        Some("harness"),
    );
    assert!(out.contains("Payments") && out.contains("Registry"));
}

#[test]
fn mermaid_has_runtime_model_and_services() {
    let out = render_mermaid(
        "agentcore",
        &svc(&["identity", "memory"]),
        Some("claude_sonnet_5"),
        Some("harness"),
    );
    assert!(out.starts_with("flowchart TD"));
    assert!(out.contains("AgentCore Runtime"));
    assert!(out.to_lowercase().contains("harness"));
    assert!(out.contains("claude_sonnet_5"));
    assert!(out.contains("Identity") && out.contains("Memory"));
    assert!(!out.contains("migration skill"), "no handoff for agentcore");
}

#[test]
fn mermaid_model_is_a_solid_edge_and_services_are_dotted() {
    let out = render_mermaid(
        "agentcore",
        &svc(&["identity", "memory"]),
        Some("claude_sonnet_5"),
        Some("harness"),
    );
    assert!(out.contains("rt -->|invoke| model"));
    assert!(out.contains("subgraph svcs[\"AgentCore services\"]"));
    assert!(out.contains("rt -.-> svcs"));
    assert!(!out.contains("rt --> svc_identity"));
}

#[test]
fn mermaid_without_services_has_no_subgraph() {
    let out = render_mermaid("agentcore", &[], Some("claude_sonnet_5"), Some("harness"));
    assert!(!out.contains("subgraph"));
    assert!(out.contains("rt -->|invoke| model"));
}

#[test]
fn mermaid_exact_text_of_a_full_agentcore_diagram() {
    let out = render_mermaid(
        "agentcore",
        &svc(&["identity", "memory"]),
        Some("claude_sonnet_5"),
        Some("harness"),
    );
    assert_eq!(
        out,
        "flowchart TD\n    user[\"User / Client\"]\n    rt[\"AgentCore Runtime<br/>(harness)\"]\n    \
         user -->|request| rt\n    model[\"Bedrock model:<br/>claude_sonnet_5\"]\n    \
         rt -->|invoke| model\n    subgraph svcs[\"AgentCore services\"]\n        direction LR\n        \
         svc_identity[\"Identity\"]\n        svc_memory[\"Memory\"]\n    end\n    rt -.-> svcs"
    );
    let ascii = render_ascii(
        "agentcore",
        &svc(&["identity", "memory"]),
        Some("claude_sonnet_5"),
        Some("harness"),
    );
    assert_eq!(
        ascii,
        "User / Client\n    |  request\n    v\n[ AgentCore Runtime (harness) ]\n    |  invoke\n    v\n\
         Bedrock model: claude_sonnet_5\n\n[ AgentCore Runtime (harness) ] .. attached AgentCore services:\n  \
         - Identity\n  - Memory"
    );
}

#[test]
fn mermaid_adds_the_handoff_for_ecs() {
    let out = render_mermaid("ecs", &[], Some("claude_sonnet_5"), None);
    assert!(out.contains("Amazon ECS (Fargate)") && out.contains("migration skill"));
    assert!(out.ends_with("    rt -.-> handoff"));
}

#[test]
fn the_deployment_model_labels_only_agentcore() {
    let out = render_mermaid("ecs", &[], None, Some("harness"));
    assert!(!out.contains("harness"));
    assert!(!out.contains("model["), "no model node without a model");
}

#[test]
fn ascii_contains_runtime_model_and_services() {
    let out = render_ascii(
        "agentcore",
        &svc(&["identity", "memory"]),
        Some("claude_sonnet_5"),
        Some("harness"),
    );
    assert!(out.contains("AgentCore Runtime") && out.to_lowercase().contains("harness"));
    assert!(out.contains("claude_sonnet_5"));
    assert!(out.contains("- Identity") && out.contains("- Memory"));
    assert!(!out.contains("migration skill"));
    assert!(out.contains("invoke") && out.contains("attached AgentCore services"));
}

#[test]
fn ascii_handoff_for_ecs_but_not_for_standard_lambda() {
    let ecs = render_ascii("ecs", &[], Some("claude_sonnet_5"), None);
    assert!(ecs.contains("Amazon ECS") && ecs.contains("migration skill"));
    let lambda = render_ascii("lambda", &[], Some("claude_sonnet_5"), None);
    assert!(lambda.contains("AWS Lambda") && !lambda.contains("migration skill"));
}

#[test]
fn renderers_are_deterministic() {
    let render = || {
        (
            render_mermaid("agentcore", &svc(&["identity"]), Some("m"), Some("harness")),
            render_ascii("eks", &svc(&["identity"]), Some("m"), None),
        )
    };
    assert_eq!(render(), render());
}

// ---- build_diagram, single unit and legacy ----------------------------------

#[test]
fn end_to_end_legacy_diagram() {
    let out = diagram(
        r#"{"verdict":"agentcore","deployment_model":"harness","agentcore_services":["identity"],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
        "{}",
        None,
    );
    assert!(out.mermaid.contains("flowchart TD"));
    assert!(out.ascii.contains("AgentCore Runtime"));
}

#[test]
fn no_viable_runtime_diagram() {
    let out = diagram(r#"{"verdict":"no_viable_runtime"}"#, "{}", None);
    assert_eq!(
        out.mermaid,
        "flowchart TD\n    n[\"No viable runtime \u{2014} see blocking constraints\"]"
    );
    assert_eq!(
        out.ascii,
        "[ No viable runtime ]\nNo viable runtime \u{2014} see blocking constraints"
    );
}

#[test]
fn golden_agentcore_full() {
    let out = diagram(
        r#"{"verdict":"agentcore","deployment_model":"framework_on_runtime",
            "agentcore_services":["identity","observability","memory","gateway"],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
        "{}",
        None,
    );
    assert!(out.mermaid.contains("framework_on_runtime"));
    for service in ["Identity", "Observability", "Memory", "Gateway"] {
        assert!(out.mermaid.contains(service) && out.ascii.contains(service));
    }
    assert!(!out.mermaid.contains("migration skill"));
}

#[test]
fn golden_lambda_microvms_has_no_handoff() {
    let out = diagram(
        r#"{"verdict":"lambda_microvms","deployment_model":null,"agentcore_services":[],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
        "{}",
        None,
    );
    assert!(out.mermaid.contains("Lambda MicroVMs"));
    assert!(!out.mermaid.contains("migration skill"));
}

#[test]
fn golden_ecs_has_a_handoff() {
    let out = diagram(
        r#"{"verdict":"ecs","deployment_model":null,"agentcore_services":["identity"],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
        "{}",
        None,
    );
    assert!(out.mermaid.contains("migration skill") && out.ascii.contains("migration skill"));
}

#[test]
fn golden_co_recommend_renders_the_chosen_runtime() {
    let out = diagram(
        r#"{"verdict":"co_recommend","co_recommend":["ecs","eks"],"deployment_model":null,
            "agentcore_services":[],"model_recommendation":{"model":"claude_sonnet_5"}}"#,
        r#"{"chosen_runtime":"eks"}"#,
        None,
    );
    assert!(out.mermaid.contains("Amazon EKS") && out.mermaid.contains("migration skill"));
}

#[test]
fn a_missing_verdict_renders_none_like_the_script() {
    let out = diagram("{}", "{}", None);
    assert!(out.mermaid.contains("rt[\"None\"]"), "{}", out.mermaid);
    assert!(!out.mermaid.contains("model["), "no model node for unknown");
}

#[test]
fn a_null_model_recommendation_is_an_unknown_model() {
    let out = diagram(
        r#"{"verdict":"ecs","model_recommendation":null}"#,
        "{}",
        None,
    );
    assert!(!out.mermaid.contains("model["));
}

const SOLO_AGENT: &str = r#"{"id":"only","workload_class":"agent_session","verdict":"agentcore",
    "model_recommendation":{"model":"claude_sonnet_5"}}"#;

#[test]
fn a_single_unit_design_collapses_to_the_legacy_diagram() {
    // test_single_unit_design_falls_back_to_legacy_render and
    // test_diagram_identical_for_single_unit_design (#4613)
    let design = format!(
        r#"{{"units":[{SOLO_AGENT}],"platform":{{"mode":"split","runtime":null,
            "interconnect":"in_process","shared_services":[]}}}}"#
    );
    for result in [
        r#"{"verdict":"agentcore","agentcore_services":[],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
        r#"{"verdict":"agentcore","agentcore_services":["memory"],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
    ] {
        assert_eq!(
            diagram(result, "{}", Some(&design)),
            diagram(result, "{}", None),
            "{result}"
        );
    }
}

#[test]
fn a_single_unit_design_renders_from_the_unit() {
    // test_build_diagram_single_unit_design_renders_from_unit (#4613): the wrapped
    // scoring-result has no top-level verdict or model.
    let design = r#"{"units":[{"id":"solo","workload_class":"agent_session",
        "verdict":"agentcore","effective_runtime":"agentcore","deployment_model":"harness",
        "model_recommendation":{"model":"claude_sonnet_5"},"agentcore_services":["memory"]}],
        "platform":{"mode":"split","runtime":null,"interconnect":"in_process"}}"#;
    let out = diagram(r#"{"units":{"solo":{}}}"#, "{}", Some(design));
    assert!(out.mermaid.contains("AgentCore Runtime"));
    assert!(out.mermaid.contains("claude_sonnet_5"));
    assert!(out.mermaid.contains("(harness)") && out.mermaid.contains("Memory"));
}

#[test]
fn an_empty_unit_service_list_is_authoritative() {
    // test_build_diagram_preserves_authoritative_empty_services (#4613)
    let design = r#"{"units":[{"id":"solo","workload_class":"agent_session","verdict":"ecs",
        "effective_runtime":"ecs","model_recommendation":{"model":"claude_sonnet_5"},
        "agentcore_services":[]}],"platform":{"mode":"split","runtime":null,
        "interconnect":"in_process"}}"#;
    let result = r#"{"verdict":"ecs","agentcore_services":["identity","observability"],
        "model_recommendation":{"model":"claude_sonnet_5"}}"#;
    let out = diagram(result, "{}", Some(design));
    assert!(!out.mermaid.contains("Identity") && !out.mermaid.contains("Observability"));
    assert!(!out.ascii.contains("Identity"));
    // A unit that omits the key falls back to the confirmed services, deduplicated.
    let omitted = r#"{"units":[{"id":"solo","verdict":"ecs","effective_runtime":"ecs",
        "model_recommendation":{"model":"claude_sonnet_5"}}]}"#;
    let out = diagram(
        result,
        r#"{"agentcore_services":["memory","memory"]}"#,
        Some(omitted),
    );
    assert_eq!(out.mermaid.matches("svc_memory").count(), 1);
    assert!(!out.mermaid.contains("Identity"));
}

#[test]
fn a_single_unit_keeps_duplicate_services_like_the_script() {
    let design = r#"{"units":[{"id":"u","verdict":"agentcore",
        "agentcore_services":["memory","memory","bogus"]}]}"#;
    let out = diagram(r#"{"verdict":"agentcore"}"#, "{}", Some(design));
    assert_eq!(out.mermaid.matches("svc_memory").count(), 2);
    assert!(!out.mermaid.contains("bogus"));
}

#[test]
fn a_single_unit_without_a_runtime_asks_the_scoring_result() {
    let design = r#"{"units":[{"id":"u","verdict":"unknown"}]}"#;
    let out = diagram(r#"{"verdict":"ecs"}"#, "{}", Some(design));
    assert!(out.mermaid.contains("Amazon ECS (Fargate)"));
    let out = diagram(r#"{"verdict":"no_viable_runtime"}"#, "{}", Some(design));
    assert!(out.mermaid.contains("No viable runtime"));
    let out = diagram(
        r#"{"verdict":"co_recommend","co_recommend":["eks"]}"#,
        "{}",
        Some(r#"{"units":[{"id":"u"}]}"#),
    );
    assert!(out.mermaid.contains("Amazon EKS"));
}

#[test]
fn model_less_units_render_no_bedrock_wiring() {
    // The `if model and model != "unknown"` source check of
    // test_mixed_system_model_less_and_temporal_downstream (#4613), as behavior.
    for model in ["null", r#""unknown""#, r#""""#] {
        let design = format!(
            r#"{{"units":[{{"id":"batchy","verdict":"batch","model_recommendation":{{"model":{model}}}}}]}}"#
        );
        let out = diagram(r#"{"verdict":"batch"}"#, "{}", Some(&design));
        assert!(!out.mermaid.contains("Bedrock model"), "{model}");
        assert!(!out.ascii.contains("Bedrock model"), "{model}");
        assert!(!out.mermaid.contains("unknown"), "{model}");
    }
    let multi = mermaid(
        "{}",
        r#"{"units":[{"id":"a","verdict":"lambda","model_recommendation":null},
            {"id":"b","verdict":"lambda","model_recommendation":{"model":""}}]}"#,
    );
    assert!(!multi.contains("<br/>claude") && !multi.contains("Bedrock"));
}

// ---- multi-unit ----------------------------------------------------------------

const TWO_UNIT: &str = r#"{"units":[
    {"id":"chat-agent","workload_class":"agent_session","verdict":"agentcore",
     "model_recommendation":{"model":"claude_sonnet_5"}},
    {"id":"summarizer","workload_class":"batch","verdict":"batch","model_recommendation":null}],
  "platform":{"mode":"split","runtime":null,"interconnect":"queue","shared_services":[]}}"#;

#[test]
fn multi_unit_renders_a_subgraph_per_unit_and_the_queue() {
    let out = diagram("{}", "{}", Some(TWO_UNIT));
    assert!(out.mermaid.contains("chat-agent") && out.mermaid.contains("summarizer"));
    assert!(out.mermaid.matches("subgraph").count() >= 2);
    assert!(out.mermaid.to_lowercase().contains("queue"));
    assert!(out.mermaid.contains("    user -->|request| chat_agent"));
    assert!(out.mermaid.contains("    chat_agent -.-> queue"));
    assert!(out.mermaid.contains("subgraph chat_agent[\"chat-agent\"]"));
    assert!(out
        .mermaid
        .contains("        chat_agent_node[\"AgentCore Runtime<br/>claude_sonnet_5\"]"));
    assert!(out
        .ascii
        .starts_with("Multi-unit Architecture:\n\nUser / Client\n    |\n    v\n"));
    assert!(out
        .ascii
        .contains("  [ chat-agent: AgentCore Runtime (claude_sonnet_5) ]"));
    assert!(out
        .ascii
        .contains("Interconnect: Queue \u{2014} chat-agent, summarizer"));
}

const CONSOLIDATED: &str = r#"{"units":[
    {"id":"chat-agent","workload_class":"agent_session","verdict":"agentcore",
     "effective_runtime":"ecs","model_recommendation":{"model":"claude_sonnet_5"}},
    {"id":"summarizer","workload_class":"batch","verdict":"batch","effective_runtime":"ecs",
     "model_recommendation":null}],
  "platform":{"mode":"consolidated","runtime":"ecs","interconnect":"none","shared_services":[]}}"#;

#[test]
fn consolidated_units_render_the_effective_runtime() {
    let out = mermaid("{}", CONSOLIDATED);
    assert!(out.matches("Amazon ECS (Fargate)").count() >= 2);
    assert!(!out.contains("AgentCore Runtime") && !out.contains("AWS Batch"));
}

#[test]
fn multi_unit_shows_the_handoff_for_handoff_runtimes() {
    let out = diagram("{}", "{}", Some(CONSOLIDATED));
    assert!(out.mermaid.contains("migration skill"));
    assert!(out.mermaid.contains("    chat_agent -.-> handoff"));
    assert!(out.ascii.contains(
        "Note: compute configured by the migration skill \u{2014} chat-agent, summarizer"
    ));
}

#[test]
fn self_contained_multi_unit_systems_have_no_handoff() {
    let design = r#"{"units":[
        {"id":"a","workload_class":"agent_session","verdict":"agentcore",
         "effective_runtime":"agentcore","model_recommendation":null},
        {"id":"b","workload_class":"light_io","verdict":"lambda",
         "effective_runtime":"lambda","model_recommendation":null}],
        "platform":{"mode":"split","runtime":null,"interconnect":"none","shared_services":[]}}"#;
    let out = diagram("{}", "{}", Some(design));
    assert!(!out.mermaid.contains("migration skill") && !out.ascii.contains("migration skill"));
}

#[test]
fn units_needing_handoff_cover_exactly_the_four_runtimes() {
    let units = j(
        r#"[{"id":"e","effective_runtime":"ecs"},{"id":"k","effective_runtime":"eks"},
        {"id":"f","effective_runtime":"fargate"},{"id":"b","effective_runtime":"batch"},
        {"id":"l","effective_runtime":"lambda"},{"id":"a","effective_runtime":"agentcore"},
        {"id":"m","effective_runtime":"lambda_microvms"}]"#,
    );
    let Json::Array(items) = &units else {
        panic!("an array")
    };
    let refs: Vec<&Json> = items.iter().collect();
    let ids: Vec<&str> = units_needing_handoff(&refs)
        .expect("handoff units")
        .into_iter()
        .map(|unit| unit_id(unit).expect("id"))
        .collect();
    assert_eq!(ids, ["e", "k", "f", "b"]);
}

#[test]
fn queue_interconnect_follows_one_way_couplings() {
    let design = r#"{"units":[
        {"id":"producer","workload_class":"service","verdict":"fargate",
         "coupling":{"mode":"queue","interacts_with":["consumer"]},"model_recommendation":null},
        {"id":"consumer","workload_class":"light_io","verdict":"lambda",
         "coupling":{"mode":"none","interacts_with":[]},"model_recommendation":null},
        {"id":"loner","workload_class":"batch","verdict":"batch",
         "coupling":{"mode":"none","interacts_with":[]},"model_recommendation":null}],
        "platform":{"mode":"split","runtime":null,"interconnect":"queue","shared_services":[]}}"#;
    let out = mermaid("{}", design);
    assert!(out.contains("producer -.-> queue") && out.contains("consumer -.-> queue"));
    assert!(out.contains("loner") && !out.contains("loner -.-> queue"));
}

#[test]
fn gateway_interconnect_wires_only_gateway_coupled_units() {
    let design = r#"{"units":[
        {"id":"orchestrator","workload_class":"agent_session","verdict":"agentcore",
         "coupling":{"mode":"a2a","interacts_with":["tool-agent"]},
         "model_recommendation":{"model":"claude_sonnet_5"}},
        {"id":"tool-agent","workload_class":"agent_session","verdict":"agentcore",
         "coupling":{"mode":"none","interacts_with":[]},
         "model_recommendation":{"model":"claude_sonnet_5"}},
        {"id":"loner","workload_class":"batch","verdict":"batch",
         "coupling":{"mode":"none","interacts_with":[]},"model_recommendation":null}],
        "platform":{"mode":"split","runtime":null,"interconnect":"gateway","shared_services":[]}}"#;
    let out = mermaid("{}", design);
    assert!(out.contains("orchestrator -.-> gateway") && out.contains("tool_agent -.-> gateway"));
    assert!(out.contains("loner") && !out.contains("loner -.-> gateway"));
}

#[test]
fn queue_and_gateway_couplings_are_both_drawn() {
    let design = r#"{"units":[
        {"id":"api-a","workload_class":"agent_session","verdict":"agentcore",
         "coupling":{"mode":"api","interacts_with":["api-b"]},
         "model_recommendation":{"model":"claude_sonnet_5"}},
        {"id":"api-b","workload_class":"service","verdict":"fargate",
         "coupling":{"mode":"none","interacts_with":[]},"model_recommendation":null},
        {"id":"q-producer","workload_class":"service","verdict":"fargate",
         "coupling":{"mode":"queue","interacts_with":["q-consumer"]},"model_recommendation":null},
        {"id":"q-consumer","workload_class":"batch","verdict":"batch",
         "coupling":{"mode":"none","interacts_with":[]},"model_recommendation":null}],
        "platform":{"mode":"split","runtime":null,"interconnect":"gateway","shared_services":[]}}"#;
    let out = diagram("{}", "{}", Some(design));
    let m = &out.mermaid;
    assert!(m.contains("api_a -.-> gateway") && m.contains("api_b -.-> gateway"));
    assert!(m.contains("queue[\"Queue\"]"));
    assert!(m.contains("q_producer -.-> queue") && m.contains("q_consumer -.-> queue"));
    assert!(!m.contains("q_producer -.-> gateway") && !m.contains("api_a -.-> queue"));
    assert!(out
        .ascii
        .contains("Interconnect: Queue \u{2014} q-producer, q-consumer"));
    assert!(out
        .ascii
        .contains("Interconnect: Gateway \u{2014} api-a, api-b"));
}

#[test]
fn without_any_coupling_the_platform_interconnect_covers_every_unit() {
    let design = TWO_UNIT.replace("queue", "gateway");
    let out = mermaid("{}", &design);
    assert!(out.contains("chat_agent -.-> gateway") && out.contains("summarizer -.-> gateway"));
    let none = TWO_UNIT.replace(
        "\"interconnect\":\"queue\"",
        "\"interconnect\":\"in_process\"",
    );
    let out = mermaid("{}", &none);
    assert!(!out.contains("-.-> queue") && !out.contains("-.-> gateway"));
}

const TEMPORAL: &str = r#"{"units":[
    {"id":"worker-fleet","workload_class":"temporal_worker_poll","verdict":"ecs",
     "queues":["agent-tasks","batch-tasks"],"model_recommendation":null},
    {"id":"doc-chat","workload_class":"agent_session","verdict":"agentcore",
     "model_recommendation":{"model":"claude_sonnet_5"},"task_queue":"agent-tasks"},
    {"id":"ocr","workload_class":"batch","verdict":"batch","model_recommendation":null,
     "task_queue":"batch-tasks"}],
  "platform":{"mode":"split","runtime":null,"interconnect":"none","shared_services":[]},
  "temporal":{"way":"cloud","server":"Temporal Cloud"}}"#;

const TEMPORAL_FLEETS: &str = r#"{"units":[
    {"id":"fleet-a","workload_class":"temporal_worker_poll","verdict":"ecs",
     "queues":["agent-tasks"],"model_recommendation":null},
    {"id":"fleet-b","workload_class":"temporal_worker_poll","verdict":"ecs",
     "queues":["batch-tasks"],"model_recommendation":null},
    {"id":"chat","workload_class":"agent_session","verdict":"agentcore",
     "model_recommendation":{"model":"claude_sonnet_5"},"task_queue":"agent-tasks"},
    {"id":"ocr","workload_class":"batch","verdict":"batch","model_recommendation":null,
     "task_queue":"batch-tasks"}],
  "platform":{"mode":"split","runtime":null,"interconnect":"none","shared_services":[]},
  "temporal":{"way":"self_hosted","server":"self-hosted"}}"#;

#[test]
fn a_self_hosted_temporal_way_is_not_labelled_cloud() {
    let out = diagram("{}", "{}", Some(TEMPORAL_FLEETS));
    assert!(!out.mermaid.contains("Temporal Cloud") && !out.ascii.contains("Temporal Cloud"));
    assert!(out
        .mermaid
        .contains("Temporal Server<br/>(self-hosted, orchestrator)"));
    assert!(out
        .ascii
        .contains("Temporal Server (self-hosted, orchestrator)"));
}

#[test]
fn each_fleet_connects_only_to_the_activities_on_its_queues() {
    let m = mermaid("{}", TEMPORAL_FLEETS);
    assert!(m.contains("fleet_a -->|agent-tasks| chat"));
    assert!(m.contains("fleet_b -->|batch-tasks| ocr"));
    for cross in [
        "fleet_a -->|batch-tasks| ocr",
        "fleet_a --> ocr",
        "fleet_b --> chat",
        "fleet_b -->|agent-tasks| chat",
    ] {
        assert!(!m.contains(cross), "{cross}");
    }
}

#[test]
fn a_cloud_temporal_topology_connects_the_worker_fleet() {
    let out = diagram("{}", "{}", Some(TEMPORAL));
    assert!(out.mermaid.contains("Temporal Cloud<br/>(orchestrator)"));
    assert!(out.mermaid.contains("temporal_cloud --> worker_fleet"));
    assert!(out
        .mermaid
        .contains("worker_fleet -->|agent-tasks| doc_chat"));
    assert!(out.mermaid.contains("worker_fleet -->|batch-tasks| ocr"));
    assert!(out
        .ascii
        .contains("Temporal Cloud (orchestrator)\n    |\n    v\n"));
    assert!(out
        .ascii
        .contains("  [ worker-fleet: Amazon ECS (Fargate) ] <-- long-polls task queues"));
    assert!(out.ascii.contains(
        "    --> [ doc-chat: AgentCore Runtime (claude_sonnet_5) ] (task queue: agent-tasks)"
    ));
    assert!(!out.mermaid.contains("user[\"User") && !out.mermaid.contains("queue[\"Queue\"]"));
    assert!(!out.ascii.contains("User / Client") && !out.ascii.contains("Interconnect: Queue"));
}

#[test]
fn a_temporal_block_alone_makes_a_temporal_topology() {
    let design = r#"{"units":[{"id":"a","verdict":"lambda"},{"id":"b","verdict":"lambda"}],
        "temporal":{"way":"surprise"}}"#;
    let out = diagram("{}", "{}", Some(design));
    assert!(out
        .mermaid
        .contains("temporal_cloud[\"Temporal Server<br/>(orchestrator)\"]"));
    assert!(
        !out.mermaid.contains("-->|"),
        "no queue data, several fleets would be a guess"
    );
}

#[test]
fn a_single_fleet_without_queue_data_runs_every_activity() {
    let design = r#"{"units":[{"id":"w","workload_class":"temporal_worker_poll","verdict":"ecs"},
        {"id":"act","verdict":"lambda"},{"id":"act2","verdict":"lambda","task_queue":"q"}]}"#;
    let m = mermaid("{}", design);
    assert!(m.contains("    w --> act"));
    assert!(m.contains("w -->|q| act2"));
}

// ---- malformed documents ---------------------------------------------------------

#[test]
fn malformed_documents_are_reported_by_field_and_type() {
    let cases: [(&str, &str, Option<&str>, &str); 12] = [
        (
            "{}",
            "{}",
            Some(r#"{"units":"x"}"#),
            "design \"units\" must be an array, found string",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[1,2]}"#),
            "a unit must be a JSON object, found integer",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{},{}]}"#),
            "a unit lacks \"id\"",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{"id":5},{"id":6}]}"#),
            "unit \"id\" must be text, found integer",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{"id":"a","verdict":["x"]},{"id":"b"}]}"#),
            "a unit runtime must be text, found array",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{"id":"a","model_recommendation":5},{"id":"b"}]}"#),
            "unit \"model_recommendation\" must be a JSON object, found integer",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{"id":"a","model_recommendation":{"model":5}},{"id":"b"}]}"#),
            "unit model must be text, found integer",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{"id":"a"},{"id":"b"}],"platform":3}"#),
            "design \"platform\" must be a JSON object, found integer",
        ),
        (
            "{}",
            "{}",
            Some(r#"{"units":[{"id":"a","coupling":3},{"id":"b"}]}"#),
            "unit \"coupling\" must be a JSON object, found integer",
        ),
        (
            "{}",
            "{}",
            Some(
                r#"{"units":[{"id":"a","coupling":{"mode":"queue","interacts_with":"b"}},{"id":"b"}]}"#,
            ),
            "unit coupling \"interacts_with\" must be an array, found string",
        ),
        (
            "{}",
            "{}",
            Some(
                r#"{"units":[{"id":"w","workload_class":"temporal_worker_poll","queues":"q"},{"id":"b"}]}"#,
            ),
            "unit \"queues\" must be an array, found string",
        ),
        (
            r#"{"verdict":["x"]}"#,
            "{}",
            None,
            "the runtime must be text, found array",
        ),
    ];
    for (result, confirm, design, expected) in cases {
        assert_eq!(failure(result, confirm, design), expected, "{design:?}");
    }
}

#[test]
fn malformed_input_errors_never_echo_values() {
    let hostile = json_string(HOSTILE);
    let designs = [
        format!(r#"{{"units":[{{"id":{hostile},"verdict":[{hostile}]}},{{"id":"b"}}]}}"#),
        format!(
            r#"{{"units":[{{"id":"a","task_queue":[{hostile}]}},{{"id":"w","workload_class":"temporal_worker_poll"}}]}}"#
        ),
        format!(
            r#"{{"units":[{{"id":"a","model_recommendation":{{"model":{{{hostile}:1}}}}}},{{"id":"b"}}]}}"#
        ),
    ];
    for design in &designs {
        let message = failure("{}", "{}", Some(design));
        assert!(!message.contains("FAKESECRET4402"), "{message}");
        assert_clean(&message);
        assert!(!message.contains('\n'), "{message}");
    }
}

// ---- hostile text in a diagram ---------------------------------------------------

#[test]
fn hostile_names_cannot_add_lines_or_leave_a_label() {
    let hostile = json_string(HOSTILE);
    let design = format!(
        r#"{{"units":[{{"id":{hostile},"workload_class":"agent_session","verdict":{hostile},
            "task_queue":{hostile},"model_recommendation":{{"model":{hostile}}}}},
            {{"id":"w","workload_class":"temporal_worker_poll","verdict":"ecs"}}],
            "temporal":{{"way":"cloud"}}}}"#
    );
    let benign = design.replace(&json_string(HOSTILE), "\"plain\"");
    let out = diagram("{}", "{}", Some(&design));
    let plain = diagram("{}", "{}", Some(&benign));
    for text in [&out.mermaid, &out.ascii] {
        assert_clean(text);
        assert!(text.contains("\\x1b[31m\\x07\\x0d\\x0ab\\u202ey"), "{text}");
    }
    assert_eq!(out.mermaid.lines().count(), plain.mermaid.lines().count());
    assert_eq!(out.ascii.lines().count(), plain.ascii.lines().count());
    let quoted_label = diagram(
        r#"{"verdict":"lambda","model_recommendation":{"model":"a\"]\n    evil[\"x"}}"#,
        "{}",
        None,
    );
    assert!(quoted_label
        .mermaid
        .contains("a#quot;]\\x0a    evil[#quot;x"));
    assert_eq!(quoted_label.mermaid.lines().count(), 6);
}

// ---- the command -------------------------------------------------------------------

fn run_dir(label: &str) -> TempDir {
    TempDir::new(&format!("diagram-{label}"))
}

#[test]
fn the_command_writes_diagram_md_beside_the_result() {
    let dir = run_dir("write");
    write(
        &dir,
        "scoring-result.json",
        r#"{"verdict":"agentcore","deployment_model":"harness","agentcore_services":["identity"],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
    );
    let (status, stdout, stderr) = exec(&[
        "startup",
        "build-diagram",
        path_text(&dir.join("scoring-result.json")),
        path_text(&dir.join("confirm.json")),
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(stdout, "RESULT=ok RUNTIME=agentcore\n");
    let expected = diagram(
        r#"{"verdict":"agentcore","deployment_model":"harness","agentcore_services":["identity"],
            "model_recommendation":{"model":"claude_sonnet_5"}}"#,
        "{}",
        None,
    );
    assert_eq!(
        fs::read_to_string(dir.join("diagram.md")).expect("diagram.md"),
        format!(
            "```mermaid\n{}\n```\n\n<details><summary>ASCII (plain-text fallback)</summary>\n\n```\n{}\n```\n\n</details>\n",
            expected.mermaid, expected.ascii
        )
    );
}

#[test]
fn the_command_reads_confirm_and_design_when_they_exist() {
    let dir = run_dir("full");
    write(
        &dir,
        "scoring-result.json",
        r#"{"verdict":"co_recommend","co_recommend":["ecs","eks"]}"#,
    );
    write(&dir, "confirm.json", r#"{"chosen_runtime":"eks"}"#);
    let args = |extra: &[&'static str]| {
        let mut values = vec![
            "startup".to_string(),
            "build-diagram".to_string(),
            path_text(&dir.join("scoring-result.json")).to_string(),
            path_text(&dir.join("confirm.json")).to_string(),
        ];
        values.extend(extra.iter().map(|value| (*value).to_string()));
        values
    };
    let run = |values: Vec<String>| {
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        exec(&refs)
    };
    let (status, stdout, _) = run(args(&[]));
    assert_eq!((status, stdout.as_str()), (0, "RESULT=ok RUNTIME=eks\n"));
    assert!(fs::read_to_string(dir.join("diagram.md"))
        .expect("diagram")
        .contains("Amazon EKS"));

    write(&dir, "design.json", TWO_UNIT);
    let mut with_design = args(&[]);
    with_design.push(path_text(&dir.join("design.json")).to_string());
    let (status, stdout, _) = run(with_design);
    assert_eq!(
        (status, stdout.as_str()),
        (0, "RESULT=ok RUNTIME=multi-unit\n")
    );
    assert!(fs::read_to_string(dir.join("diagram.md"))
        .expect("diagram")
        .contains("chat_agent"));

    let mut absent_design = args(&[]);
    absent_design.push(path_text(&dir.join("no-design.json")).to_string());
    let (status, stdout, _) = run(absent_design);
    assert_eq!((status, stdout.as_str()), (0, "RESULT=ok RUNTIME=eks\n"));
}

#[test]
fn an_empty_design_object_prints_the_resolved_runtime() {
    let dir = run_dir("empty-design");
    write(&dir, "r.json", r#"{"verdict":"lambda"}"#);
    write(&dir, "d.json", "{}");
    let (status, stdout, _) = exec(&[
        "startup",
        "build-diagram",
        path_text(&dir.join("r.json")),
        path_text(&dir.join("c.json")),
        path_text(&dir.join("d.json")),
    ]);
    assert_eq!((status, stdout.as_str()), (0, "RESULT=ok RUNTIME=lambda\n"));
}

#[test]
fn the_command_needs_a_result_and_a_confirm() {
    for values in [
        vec!["startup", "build-diagram"],
        vec!["startup", "build-diagram", "only-one.json"],
    ] {
        let (status, stdout, stderr) = exec(&values);
        assert_eq!(status, 2, "{values:?}");
        assert_eq!(stdout, "");
        assert!(
            stderr.contains("usage: toolkit startup build-diagram"),
            "{stderr}"
        );
    }
    let (status, _, _) = exec(&["startup", "build-diagram", "a", "b", "c", "d"]);
    assert_eq!(status, 2, "a fourth positional is a usage error");
}

#[test]
fn unreadable_and_malformed_files_are_one_line_errors() {
    let dir = run_dir("errors");
    write(&dir, "bad.json", "{\"verdict\": ");
    write(&dir, "list.json", "[1]");
    write(&dir, "ok.json", r#"{"verdict":"ecs"}"#);
    let at = |name: &str| path_text(&dir.join(name)).to_string();
    for (result, confirm, design, expected) in [
        ("missing.json", "ok.json", None, "does not exist"),
        (
            "bad.json",
            "ok.json",
            None,
            "invalid JSON (unexpected end of input",
        ),
        (
            "list.json",
            "ok.json",
            None,
            "the scoring result must be a JSON object, found array",
        ),
        ("ok.json", "bad.json", None, "invalid JSON"),
        (
            "ok.json",
            "list.json",
            None,
            "the confirm document must be a JSON object, found array",
        ),
        (
            "ok.json",
            "ok.json",
            Some("list.json"),
            "the design must be a JSON object, found array",
        ),
    ] {
        let mut values = vec![
            "startup".to_string(),
            "build-diagram".to_string(),
            at(result),
            at(confirm),
        ];
        values.extend(design.map(at));
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let (status, stdout, stderr) = exec(&refs);
        assert_eq!(status, 1, "{result}");
        assert_eq!(stdout, "", "{result}");
        assert!(stderr.starts_with("error: "), "{stderr}");
        assert!(stderr.contains(expected), "{expected}: {stderr}");
        assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr}");
    }
    assert!(
        !dir.join("diagram.md").exists(),
        "nothing is written on error"
    );
}

#[cfg(unix)]
#[test]
fn linked_inputs_and_outputs_are_refused() {
    use std::os::unix::fs::symlink;

    let dir = run_dir("links");
    write(&dir, "real.json", r#"{"verdict":"ecs"}"#);
    write(&dir, "design-real.json", TWO_UNIT);
    symlink(dir.join("real.json"), dir.join("linked.json")).expect("link");
    symlink(dir.join("design-real.json"), dir.join("linked-design.json")).expect("link");
    let at = |name: &str| path_text(&dir.join(name)).to_string();
    for (result, confirm, design) in [
        ("linked.json", "real.json", None),
        ("real.json", "linked.json", None),
        ("real.json", "real.json", Some("linked-design.json")),
    ] {
        let mut values = vec![
            "startup".to_string(),
            "build-diagram".to_string(),
            at(result),
            at(confirm),
        ];
        values.extend(design.map(at));
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let (status, stdout, stderr) = exec(&refs);
        assert_eq!(status, 1, "{result} {confirm} {design:?}");
        assert_eq!(stdout, "");
        assert!(
            stderr.contains("symbolic links are not followed"),
            "{stderr}"
        );
    }
    // diagram.md as a link to another file is never written through.
    let target = dir.join("elsewhere.md");
    fs::write(&target, "keep").expect("target");
    symlink(&target, dir.join("diagram.md")).expect("link");
    let (status, _, stderr) = exec(&[
        "startup",
        "build-diagram",
        &at("real.json"),
        &at("real.json"),
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("cannot write") && stderr.contains("symbolic link"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&target).expect("target"), "keep");
}

#[test]
fn hostile_values_and_file_names_are_escaped_in_every_report() {
    let dir = run_dir("hostile");
    let hostile = json_string(HOSTILE);
    // stdout: the runtime is a document value.
    write(
        &dir,
        "scoring-result.json",
        &format!(r#"{{"verdict":{hostile}}}"#),
    );
    let (status, stdout, stderr) = exec(&[
        "startup",
        "build-diagram",
        path_text(&dir.join("scoring-result.json")),
        path_text(&dir.join("confirm.json")),
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert_clean(&stdout);
    assert!(
        stdout.starts_with("RESULT=ok RUNTIME=x\\x1b[31m\\x07\\x0d\\x0ab\\u202ey"),
        "{stdout}"
    );
    assert!(!stdout.contains("FAKESECRET4402"), "{stdout}");
    // diagram.md
    let document = fs::read_to_string(dir.join("diagram.md")).expect("diagram.md");
    assert_clean(&document);
    // stderr: a hostile file name in an error.
    let name = "bad\u{1b}[31m\u{7}\r\nname\u{202e}.json";
    write(&dir, name, "{");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "build-diagram",
        path_text(&dir.join(name)),
        "confirm.json",
    ]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_clean(&stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr:?}");
    assert!(
        stderr.contains("\\x1b[31m\\x07\\x0d\\x0aname\\u202e.json"),
        "{stderr}"
    );
    // stderr: a missing file with a hostile name.
    let missing = dir.join("gone\u{1b}\r\n.json");
    let (status, _, stderr) = exec(&["startup", "build-diagram", path_text(&missing), "c"]);
    assert_eq!(status, 1);
    assert_clean(&stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr:?}");
}
