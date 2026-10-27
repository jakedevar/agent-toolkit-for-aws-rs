//! `toolkit startup build-diagram` (#4402, slice `startup-diagram`).
//!
//! Ports `plugins/aws-startup-advisor/skills/agent-advisor/scripts/build_diagram.py`,
//! the deterministic architecture-diagram composer: `(scoring-result, confirm,
//! design)` documents in, a Mermaid flowchart plus an ASCII fallback out, the
//! same input giving byte-identical text. The composer is pure ([`build_diagram`]);
//! the command only reads the three documents, writes `diagram.md` beside the
//! scoring result and prints `RESULT=ok RUNTIME=<runtime>`.
//!
//! Python reasoned about whatever JSON it was handed and died with a traceback
//! on a shape it did not expect. Here a document of the wrong shape is a
//! one-line error (exit status 1) naming the field and its JSON type, never the
//! value. Text that comes from the documents (unit ids, model names, runtime
//! ids, task queues) has its control, bidirectional and invisible characters
//! escaped before it reaches `diagram.md` or stdout. Every such string that reaches
//! the Mermaid text goes through one encoder (`mermaid_text` for labels,
//! `NodeIds` for node ids), so a hostile name can neither add lines nor graph
//! statements (#4679, `docs/migration/startup-diagram.md`).
//!
//! The composer writes text only. It does not run Mermaid, a renderer or any
//! other program: rendering `diagram.md` stays with the reader's own tooling.

use std::fmt;
use std::fs;
use std::path::Path;

use toolkit_aws::redact_text;
use toolkit_core::{write_regular_file, Json, ToolkitError};
use toolkit_msk::pyfmt::py_str;

use super::{is_missing, read_json_document};
use crate::manifests::read_failure;
use crate::safe_text::{safe_path, safe_text};
use crate::{Invocation, Output};

#[cfg(test)]
mod tests;

/// Display names of the runtimes (`RUNTIME_LABELS`).
const RUNTIME_LABELS: [(&str, &str); 9] = [
    ("agentcore", "AgentCore Runtime"),
    ("lambda_microvms", "Lambda MicroVMs"),
    ("ecs", "Amazon ECS (Fargate)"),
    ("eks", "Amazon EKS"),
    ("lambda", "AWS Lambda"),
    ("batch", "AWS Batch"),
    ("fargate", "AWS Fargate"),
    (
        "serverless_workers",
        "Temporal Serverless Workers (Public Preview)",
    ),
    ("none", "No viable runtime"),
];

/// Display names of the `AgentCore` services (`SERVICE_LABELS`).
const SERVICE_LABELS: [(&str, &str); 14] = [
    ("identity", "Identity"),
    ("observability", "Observability"),
    ("evaluations", "Evaluations"),
    ("optimization", "Optimization"),
    ("memory", "Memory"),
    ("gateway", "Gateway"),
    ("policy", "Policy"),
    ("managed_kb", "Managed KB"),
    ("code_interpreter", "Code Interpreter"),
    ("browser", "Browser"),
    ("web_search", "Web Search"),
    ("sandbox", "Sandbox"),
    ("payments", "Payments"),
    ("registry", "Registry"),
];

/// Runtimes whose compute layer is handed to the migration skill. `AgentCore`,
/// standard Lambda and Lambda `MicroVMs` are self-contained deliverables.
/// Keep in sync with `design.md`'s `handoff_required` definition.
const HANDOFF_RUNTIMES: [&str; 4] = ["ecs", "eks", "fargate", "batch"];

/// Name of the file `startup build-diagram` writes beside the scoring result.
const DIAGRAM_FILENAME: &str = "diagram.md";

const NO_VIABLE_MESSAGE: &str = "No viable runtime \u{2014} see blocking constraints";
const HANDOFF_NODE: &str = "    handoff[\"Compute configured by the migration skill\"]";

/// A document of the wrong shape. The text names fields and types, never a
/// value of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagramError(String);

impl DiagramError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for DiagramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<DiagramError> for ToolkitError {
    fn from(error: DiagramError) -> Self {
        Self::runtime("startup_diagram_invalid", error.0)
    }
}

type Res<T> = Result<T, DiagramError>;

/// The two renderings of one architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Diagram {
    mermaid: String,
    ascii: String,
}

/// Stand-in for a missing optional object (`.get(key, {})`).
static EMPTY_OBJECT: Json = Json::Object(Vec::new());

/// Stand-in for a missing value (`.get(key)` is `None`).
static NULL: Json = Json::Null;

/// Python truthiness of a JSON value.
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Int(number) => *number != 0,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(members) => !members.is_empty(),
    }
}

fn is_text(value: &Json, expected: &str) -> bool {
    value.as_str() == Some(expected)
}

/// `value` as an object.
fn object<'a>(value: &'a Json, what: &str) -> Res<&'a Json> {
    match value {
        Json::Object(_) => Ok(value),
        other => Err(DiagramError::new(format!(
            "{what} must be a JSON object, found {}",
            other.type_name()
        ))),
    }
}

/// `owner.get(key, {})` as an object; an explicit `null` counts as absent.
fn object_or_empty<'a>(owner: &'a Json, key: &str, what: &str) -> Res<&'a Json> {
    match owner.get(key) {
        None | Some(Json::Null) => Ok(&EMPTY_OBJECT),
        Some(value) => object(value, what),
    }
}

/// The text of a runtime-like scalar as Python formats it (`None`, `True`,
/// `5`); a list or object cannot name a runtime.
fn runtime_text(value: &Json, what: &str) -> Res<String> {
    match value {
        Json::Array(_) | Json::Object(_) => Err(DiagramError::new(format!(
            "{what} must be text, found {}",
            value.type_name()
        ))),
        other => Ok(py_str(other)),
    }
}

fn runtime_label(runtime: &str) -> &str {
    RUNTIME_LABELS
        .iter()
        .find(|(id, _)| *id == runtime)
        .map_or(runtime, |(_, label)| label)
}

fn service_label(id: &str) -> Option<&'static str> {
    SERVICE_LABELS
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, label)| *label)
}

/// Document text made safe for the diagram: unsafe characters escaped.
fn esc(text: &str) -> String {
    safe_path(text)
}

/// The one encoder for caller-derived text that reaches Mermaid (labels, edge
/// labels, subgraph titles, queue names): control and invisible characters
/// become visible `\x..` text first ([`esc`]), then every ASCII character that
/// is not an ASCII letter, digit, space or one of `_ . , : / - + \` becomes a
/// Mermaid entity (`"` is `#quot;`, anything else `#<decimal>;`). Quotes,
/// brackets, braces, parentheses, pipes, semicolons, angle brackets, `#`, `%`,
/// `&`, backticks and the rest of the syntax characters therefore never reach
/// the output raw, whether the text sits in a `"quoted"` label or an unquoted
/// `|edge label|`. Non-ASCII characters that are not control or invisible stay.
fn mermaid_text(text: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(text.len());
    for c in esc(text).chars() {
        match c {
            '"' => out.push_str("#quot;"),
            c if c.is_ascii_alphanumeric()
                || matches!(c, ' ' | '_' | '.' | ',' | ':' | '/' | '-' | '+' | '\\')
                || !c.is_ascii() =>
            {
                out.push(c);
            }
            c => {
                let _ = write!(out, "#{};", u32::from(c));
            }
        }
    }
    out
}

/// Words a Mermaid flowchart treats as syntax, plus the node ids this module
/// writes itself. A unit id equal to one of them (in any case) is not used as
/// a node id.
const RESERVED_NODE_IDS: [&str; 24] = [
    "end",
    "subgraph",
    "graph",
    "flowchart",
    "style",
    "classdef",
    "class",
    "click",
    "linkstyle",
    "direction",
    "default",
    "interpolate",
    "call",
    "href",
    "callback",
    "user",
    "rt",
    "model",
    "svcs",
    "handoff",
    "queue",
    "gateway",
    "temporal_cloud",
    "n",
];

/// FNV-1a over the bytes of `text`: a stable, dependency-free digest for the
/// fallback node ids (collisions are resolved by [`NodeIds`]).
fn fnv1a(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// The Mermaid node id of every unit of a design. An id written
/// `[A-Za-z][A-Za-z0-9_-]*` keeps its readable form (hyphens become
/// underscores) unless it is a reserved word, would equal another unit's id or
/// inner `<id>_node` id, or an id this module writes itself; every other id
/// (spaces, brackets, quotes, pipes, semicolons, a leading digit, non-ASCII)
/// maps to `u_<8 hex digits of its FNV-1a digest>`, with `_2`, `_3`, ... on a
/// collision. The mapping depends only on the ordered unit ids, so the same
/// input always renders the same text; the real id stays visible in the label.
struct NodeIds {
    by_raw: std::collections::HashMap<String, String>,
    used: std::collections::HashSet<String>,
}

impl NodeIds {
    fn for_units(units: &[&Json]) -> Res<Self> {
        let mut table = Self {
            by_raw: std::collections::HashMap::new(),
            used: std::collections::HashSet::new(),
        };
        for unit in units {
            let raw = unit_id(unit)?;
            if table.by_raw.contains_key(raw) {
                continue;
            }
            let id = table.assign(raw);
            table.used.insert(format!("{id}_node"));
            table.used.insert(id.clone());
            table.by_raw.insert(raw.to_string(), id);
        }
        Ok(table)
    }

    fn free(&self, id: &str) -> bool {
        !self.used.contains(id) && !self.used.contains(&format!("{id}_node"))
    }

    fn assign(&self, raw: &str) -> String {
        let mut chars = raw.chars();
        let readable = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if readable {
            let plain = raw.replace('-', "_");
            let lower = plain.to_ascii_lowercase();
            if !RESERVED_NODE_IDS.contains(&lower.as_str()) && self.free(&plain) {
                return plain;
            }
        }
        let base = format!("u_{:08x}", fnv1a(raw));
        let mut candidate = base.clone();
        let mut counter = 2_u32;
        while !self.free(&candidate) {
            candidate = format!("{base}_{counter}");
            counter += 1;
        }
        candidate
    }

    /// The node id of a unit of the design the table was built from.
    fn get(&self, raw: &str) -> &str {
        self.by_raw.get(raw).map_or("u_missing", String::as_str)
    }
}

/// The Mermaid label of a runtime: a known runtime keeps its fixed display
/// name, anything else is caller text and is encoded.
fn runtime_label_text(runtime: &str) -> String {
    RUNTIME_LABELS
        .iter()
        .find(|(id, _)| *id == runtime)
        .map_or_else(|| mermaid_text(runtime), |(_, label)| (*label).to_string())
}

// ---- runtime, services, model ---------------------------------------------

/// `resolve_runtime`: the runtime of a scoring result and the user's choice.
fn resolve_runtime(result: &Json, confirm: &Json) -> Res<Json> {
    let verdict = result.get("verdict");
    if verdict.is_some_and(|value| is_text(value, "co_recommend")) {
        if let Some(chosen) = confirm.get("chosen_runtime").filter(|value| truthy(value)) {
            return Ok(chosen.clone());
        }
        return match result.get("co_recommend") {
            None => Ok(Json::string("none")),
            Some(Json::Array(items)) => items.first().cloned().ok_or_else(|| {
                DiagramError::new("scoring result \"co_recommend\" must not be empty")
            }),
            Some(Json::String(text)) => text
                .chars()
                .next()
                .map(|first| Json::string(first.to_string()))
                .ok_or_else(|| {
                    DiagramError::new("scoring result \"co_recommend\" must not be empty")
                }),
            Some(other) => Err(DiagramError::new(format!(
                "scoring result \"co_recommend\" must be an array, found {}",
                other.type_name()
            ))),
        };
    }
    if verdict.is_some_and(|value| is_text(value, "no_viable_runtime")) {
        return Ok(Json::string("none"));
    }
    Ok(verdict.cloned().unwrap_or(Json::Null))
}

/// The service ids of an `agentcore_services` value: absent or falsy is empty,
/// anything but a list is reported; entries that are not text name no service.
fn service_ids<'a>(value: Option<&'a Json>, owner: &str) -> Res<Vec<&'a str>> {
    match value {
        None => Ok(Vec::new()),
        Some(value) if !truthy(value) => Ok(Vec::new()),
        Some(Json::Array(items)) => Ok(items.iter().filter_map(Json::as_str).collect()),
        Some(other) => Err(DiagramError::new(format!(
            "{owner} \"agentcore_services\" must be an array, found {}",
            other.type_name()
        ))),
    }
}

/// `resolve_services`: the confirmed services, else the scoring result's;
/// unknown ids dropped, duplicates removed, first occurrence kept.
fn resolve_services(result: &Json, confirm: &Json) -> Res<Vec<String>> {
    let confirmed = confirm.get("agentcore_services").filter(|v| truthy(v));
    let (chosen, owner) = match confirmed {
        Some(value) => (Some(value), "confirm"),
        None => (result.get("agentcore_services"), "scoring result"),
    };
    let mut out: Vec<String> = Vec::new();
    for id in service_ids(chosen, owner)? {
        if service_label(id).is_some() && !out.iter().any(|seen| seen == id) {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

/// The model of the Bedrock node, if the unit calls one: a truthy model that is
/// not the placeholder `unknown`.
fn model_line(model: &Json) -> Option<String> {
    (truthy(model) && !is_text(model, "unknown")).then(|| py_str(model))
}

// ---- single unit ------------------------------------------------------------

fn render_mermaid(
    runtime: &str,
    services: &[String],
    model: Option<&str>,
    deployment_model: Option<&str>,
) -> String {
    let mut label = runtime_label_text(runtime);
    if runtime == "agentcore" {
        if let Some(deployment) = deployment_model {
            label = format!("{label}<br/>({})", mermaid_text(deployment));
        }
    }
    let mut lines = vec![
        "flowchart TD".to_string(),
        "    user[\"User / Client\"]".to_string(),
        format!("    rt[\"{label}\"]"),
        "    user -->|request| rt".to_string(),
    ];
    if let Some(model) = model {
        lines.push(format!(
            "    model[\"Bedrock model:<br/>{}\"]",
            mermaid_text(model)
        ));
        lines.push("    rt -->|invoke| model".to_string());
    }
    if !services.is_empty() {
        lines.push("    subgraph svcs[\"AgentCore services\"]".to_string());
        lines.push("        direction LR".to_string());
        for id in services {
            let name = service_label(id).unwrap_or(id);
            lines.push(format!("        svc_{id}[\"{name}\"]"));
        }
        lines.push("    end".to_string());
        lines.push("    rt -.-> svcs".to_string());
    }
    if HANDOFF_RUNTIMES.contains(&runtime) {
        lines.push(HANDOFF_NODE.to_string());
        lines.push("    rt -.-> handoff".to_string());
    }
    lines.join("\n")
}

fn render_ascii(
    runtime: &str,
    services: &[String],
    model: Option<&str>,
    deployment_model: Option<&str>,
) -> String {
    let mut label = esc(runtime_label(runtime));
    if runtime == "agentcore" {
        if let Some(deployment) = deployment_model {
            label = format!("{label} ({})", esc(deployment));
        }
    }
    let mut lines = vec![
        "User / Client".to_string(),
        "    |  request".to_string(),
        "    v".to_string(),
        format!("[ {label} ]"),
    ];
    if let Some(model) = model {
        lines.push("    |  invoke".to_string());
        lines.push("    v".to_string());
        lines.push(format!("Bedrock model: {}", esc(model)));
    }
    if !services.is_empty() {
        lines.push(String::new());
        lines.push(format!("[ {label} ] .. attached AgentCore services:"));
        for id in services {
            lines.push(format!("  - {}", service_label(id).unwrap_or(id)));
        }
    }
    if HANDOFF_RUNTIMES.contains(&runtime) {
        lines.push(String::new());
        lines.push("Note: compute configured by the migration skill".to_string());
    }
    lines.join("\n")
}

fn no_viable_runtime() -> Diagram {
    Diagram {
        mermaid: format!("flowchart TD\n    n[\"{NO_VIABLE_MESSAGE}\"]"),
        ascii: format!("[ {} ]\n{NO_VIABLE_MESSAGE}", runtime_label("none")),
    }
}

/// The first truthy of `unit.get(key)` and `result.get(key)`.
fn first_truthy<'a>(unit: &'a Json, result: &'a Json, key: &str) -> Option<&'a Json> {
    unit.get(key)
        .filter(|value| truthy(value))
        .or_else(|| result.get(key).filter(|value| truthy(value)))
}

/// The model a single-unit diagram shows: the unit's recommendation, else the
/// scoring result's, else `unknown`.
fn single_unit_model(unit: &Json, result: &Json) -> Res<Json> {
    if let Some(recommendation) = unit.get("model_recommendation").filter(|v| truthy(v)) {
        let recommendation = object(recommendation, "unit \"model_recommendation\"")?;
        if let Some(model) = recommendation.get("model").filter(|v| truthy(v)) {
            return Ok(model.clone());
        }
    }
    let recommendation = object_or_empty(
        result,
        "model_recommendation",
        "scoring result \"model_recommendation\"",
    )?;
    Ok(recommendation
        .get("model")
        .cloned()
        .unwrap_or_else(|| Json::string("unknown")))
}

fn render_single(
    runtime: &Json,
    services: &[String],
    model: &Json,
    deployment_model: Option<&Json>,
) -> Res<Diagram> {
    let runtime = runtime_text(runtime, "the runtime")?;
    let model = model_line(model);
    let deployment = deployment_model.map(py_str);
    Ok(Diagram {
        mermaid: render_mermaid(&runtime, services, model.as_deref(), deployment.as_deref()),
        ascii: render_ascii(&runtime, services, model.as_deref(), deployment.as_deref()),
    })
}

/// The path with no usable design: runtime, services and model of the scoring
/// result and the user's confirmation alone.
fn legacy_diagram(result: &Json, confirm: &Json) -> Res<Diagram> {
    let runtime = resolve_runtime(result, confirm)?;
    if is_text(&runtime, "none") {
        return Ok(no_viable_runtime());
    }
    let services = resolve_services(result, confirm)?;
    let recommendation = object_or_empty(
        result,
        "model_recommendation",
        "scoring result \"model_recommendation\"",
    )?;
    let model = recommendation
        .get("model")
        .cloned()
        .unwrap_or_else(|| Json::string("unknown"));
    render_single(
        &runtime,
        &services,
        &model,
        result.get("deployment_model").filter(|v| truthy(v)),
    )
}

/// A design with exactly one unit renders from the unit, falling back to the
/// scoring result for what the unit omits. An explicit empty
/// `agentcore_services` list is authoritative: the user declined every add-on.
fn single_unit_diagram(unit: &Json, result: &Json, confirm: &Json) -> Res<Diagram> {
    let mut runtime = unit_runtime(unit);
    if ["none", "no_viable_runtime", "unknown"]
        .iter()
        .any(|word| is_text(&runtime, word))
    {
        runtime = resolve_runtime(result, confirm)?;
    }
    if is_text(&runtime, "none") {
        return Ok(no_viable_runtime());
    }
    let services: Vec<String> = match unit.get("agentcore_services") {
        Some(own) => service_ids(Some(own), "unit")?
            .into_iter()
            .filter(|id| service_label(id).is_some())
            .map(str::to_string)
            .collect(),
        None => resolve_services(result, confirm)?,
    };
    let model = single_unit_model(unit, result)?;
    let deployment = first_truthy(unit, result, "deployment_model");
    render_single(&runtime, &services, &model, deployment)
}

// ---- units ---------------------------------------------------------------

/// The runtime a unit actually deploys on: `effective_runtime` under a
/// consolidated platform, else the best-fit `verdict` (`unknown` when absent).
fn unit_runtime(unit: &Json) -> Json {
    match unit.get("effective_runtime") {
        Some(value) if truthy(value) => value.clone(),
        _ => unit
            .get("verdict")
            .cloned()
            .unwrap_or_else(|| Json::string("unknown")),
    }
}

fn unit_runtime_text(unit: &Json) -> Res<String> {
    runtime_text(&unit_runtime(unit), "a unit runtime")
}

fn unit_id(unit: &Json) -> Res<&str> {
    match unit.get("id") {
        Some(Json::String(id)) => Ok(id),
        Some(other) => Err(DiagramError::new(format!(
            "unit \"id\" must be text, found {}",
            other.type_name()
        ))),
        None => Err(DiagramError::new("a unit lacks \"id\"")),
    }
}

/// The model of a unit's `model_recommendation`, if it names one.
fn unit_model(unit: &Json) -> Res<Option<&str>> {
    let Some(recommendation) = unit.get("model_recommendation").filter(|v| truthy(v)) else {
        return Ok(None);
    };
    let recommendation = object(recommendation, "unit \"model_recommendation\"")?;
    match recommendation.get("model") {
        Some(Json::String(model)) if !model.is_empty() => Ok(Some(model)),
        Some(value) if truthy(value) => Err(DiagramError::new(format!(
            "unit model must be text, found {}",
            value.type_name()
        ))),
        _ => Ok(None),
    }
}

/// The Temporal task queue of a unit; absent, null and other falsy values
/// mean none.
fn task_queue(unit: &Json) -> Res<Option<&str>> {
    match unit.get("task_queue") {
        Some(Json::String(queue)) if !queue.is_empty() => Ok(Some(queue)),
        Some(value) if truthy(value) => Err(DiagramError::new(format!(
            "unit \"task_queue\" must be text, found {}",
            value.type_name()
        ))),
        _ => Ok(None),
    }
}

/// The task queues a worker-poll unit polls.
fn polled_queues(unit: &Json) -> Res<Vec<&str>> {
    match unit.get("queues") {
        None => Ok(Vec::new()),
        Some(Json::Array(items)) => Ok(items.iter().filter_map(Json::as_str).collect()),
        Some(other) => Err(DiagramError::new(format!(
            "unit \"queues\" must be an array, found {}",
            other.type_name()
        ))),
    }
}

/// The units of a design: an array of objects with text ids.
fn design_units(design: &Json) -> Res<Vec<&Json>> {
    match design.get("units") {
        None => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|unit| object(unit, "a unit"))
            .collect::<Res<Vec<_>>>(),
        Some(other) => Err(DiagramError::new(format!(
            "design \"units\" must be an array, found {}",
            other.type_name()
        ))),
    }
}

fn unit_is(unit: &Json, class: &str) -> bool {
    unit.get("workload_class")
        .is_some_and(|value| is_text(value, class))
}

/// Units coupled through any of `modes`, plus the targets of their one-way
/// `interacts_with` couplings (a producer on a queue puts its consumer on it
/// too). Each interconnect is computed independently from the per-unit
/// `coupling.mode`.
fn units_on_interconnect<'a>(units: &[&'a Json], modes: &[&str]) -> Res<Vec<&'a Json>> {
    let mut on: Vec<&str> = Vec::new();
    for unit in units {
        let coupling = match unit.get("coupling") {
            None => &EMPTY_OBJECT,
            Some(value) => object(value, "unit \"coupling\"")?,
        };
        let coupled = coupling
            .get("mode")
            .and_then(Json::as_str)
            .is_some_and(|mode| modes.contains(&mode));
        if !coupled {
            continue;
        }
        on.push(unit_id(unit)?);
        let targets = match coupling.get("interacts_with") {
            None => &[][..],
            Some(Json::Array(items)) => items,
            Some(other) => {
                return Err(DiagramError::new(format!(
                    "unit coupling \"interacts_with\" must be an array, found {}",
                    other.type_name()
                )))
            }
        };
        for target in targets.iter().filter_map(Json::as_str) {
            if units.iter().any(|other| unit_id(other) == Ok(target)) {
                on.push(target);
            }
        }
    }
    let mut selected = Vec::new();
    for unit in units {
        if on.contains(&unit_id(unit)?) {
            selected.push(*unit);
        }
    }
    Ok(selected)
}

/// Units whose actual runtime hands the compute layer to the migration skill.
fn units_needing_handoff<'a>(units: &[&'a Json]) -> Res<Vec<&'a Json>> {
    let mut selected = Vec::new();
    for unit in units {
        if HANDOFF_RUNTIMES.contains(&unit_runtime_text(unit)?.as_str()) {
            selected.push(*unit);
        }
    }
    Ok(selected)
}

/// The orchestrator name of a Temporal design: the chosen Way is kept, a
/// self-hosted server is never drawn as Temporal Cloud.
fn temporal_name(temporal: &Json) -> &'static str {
    match temporal.get("way") {
        Some(way) if is_text(way, "self_hosted") => "Temporal Server (self-hosted, orchestrator)",
        Some(way) if is_text(way, "cloud") => "Temporal Cloud (orchestrator)",
        _ => "Temporal Server (orchestrator)",
    }
}

/// The per-design context both multi-unit renderers share.
struct MultiUnit<'a> {
    units: Vec<&'a Json>,
    workers: Vec<&'a Json>,
    is_temporal: bool,
    temporal: &'a Json,
    interconnect: &'a Json,
}

impl<'a> MultiUnit<'a> {
    fn new(design: &'a Json) -> Res<Self> {
        let units = design_units(design)?;
        let platform = object_or_empty(design, "platform", "design \"platform\"")?;
        let temporal = object_or_empty(design, "temporal", "design \"temporal\"")?;
        let workers: Vec<&Json> = units
            .iter()
            .copied()
            .filter(|unit| unit_is(unit, "temporal_worker_poll"))
            .collect();
        let is_temporal = !workers.is_empty() || truthy(temporal);
        Ok(Self {
            units,
            workers,
            is_temporal,
            temporal,
            interconnect: platform.get("interconnect").unwrap_or(&NULL),
        })
    }

    /// The units on the queue and on the gateway. A system can mix couplings, so
    /// both are computed from the per-unit data; only a design without any
    /// `coupling` falls back to the single `platform.interconnect`.
    fn interconnects(&self) -> Res<(Vec<&'a Json>, Vec<&'a Json>)> {
        let mut queue = units_on_interconnect(&self.units, &["queue"])?;
        let mut gateway = units_on_interconnect(&self.units, &["api", "a2a"])?;
        if !self.units.iter().any(|unit| unit.get("coupling").is_some()) {
            if is_text(self.interconnect, "queue") {
                queue.clone_from(&self.units);
            } else if is_text(self.interconnect, "gateway") {
                gateway.clone_from(&self.units);
            }
        }
        Ok((queue, gateway))
    }

    fn non_workers(&self) -> Vec<&'a Json> {
        self.units
            .iter()
            .copied()
            .filter(|unit| !unit_is(unit, "temporal_worker_poll"))
            .collect()
    }

    /// The activity units one worker fleet executes, with the queue to label
    /// the edge. Matched by queue membership, never a cartesian product; a
    /// single fleet with no queue data runs every activity; several fleets
    /// without queue data cannot be attributed and stay unconnected.
    fn fleet_edges(&self, worker: &Json) -> Res<Vec<(&'a Json, Option<&'a str>)>> {
        let fleet_queues = polled_queues(worker)?;
        let single_fleet = self.workers.len() == 1;
        let mut edges = Vec::new();
        for activity in self.non_workers() {
            let queue = task_queue(activity)?;
            match queue {
                Some(queue) if !fleet_queues.is_empty() => {
                    if fleet_queues.contains(&queue) {
                        edges.push((activity, Some(queue)));
                    }
                }
                _ if single_fleet => edges.push((activity, queue)),
                _ => {}
            }
        }
        Ok(edges)
    }
}

fn render_multi_unit_mermaid(design: &Json) -> Res<String> {
    let multi = MultiUnit::new(design)?;
    let mut lines = vec!["flowchart TD".to_string()];
    let ids = NodeIds::for_units(&multi.units)?;
    let subgraph = |lines: &mut Vec<String>, unit: &Json| -> Res<String> {
        let id = unit_id(unit)?;
        let sanitized = ids.get(id).to_string();
        let mut parts = vec![runtime_label_text(&unit_runtime_text(unit)?)];
        if let Some(model) = unit_model(unit)? {
            parts.push(mermaid_text(model));
        }
        lines.push(format!(
            "    subgraph {sanitized}[\"{}\"]",
            mermaid_text(id)
        ));
        lines.push(format!(
            "        {sanitized}_node[\"{}\"]",
            parts.join("<br/>")
        ));
        lines.push("    end".to_string());
        Ok(sanitized)
    };
    if multi.is_temporal {
        let name = temporal_name(multi.temporal).replace(" (", "<br/>(");
        lines.push(format!("    temporal_cloud[\"{name}\"]"));
        for unit in &multi.units {
            subgraph(&mut lines, unit)?;
        }
        for worker in &multi.workers {
            lines.push(format!(
                "    temporal_cloud --> {}",
                ids.get(unit_id(worker)?)
            ));
        }
        for worker in &multi.workers {
            let worker_id = ids.get(unit_id(worker)?);
            for (activity, queue) in multi.fleet_edges(worker)? {
                let activity_id = ids.get(unit_id(activity)?);
                match queue {
                    Some(queue) => {
                        lines.push(format!(
                            "    {worker_id} -->|{}| {activity_id}",
                            mermaid_text(queue)
                        ));
                    }
                    None => lines.push(format!("    {worker_id} --> {activity_id}")),
                }
            }
        }
    } else {
        lines.push("    user[\"User / Client\"]".to_string());
        let mut entries = Vec::new();
        for unit in &multi.units {
            let sanitized = subgraph(&mut lines, unit)?;
            if unit_is(unit, "agent_session") {
                entries.push(sanitized);
            }
        }
        for entry in entries {
            lines.push(format!("    user -->|request| {entry}"));
        }
        if multi.units.len() > 1 {
            let (queue, gateway) = multi.interconnects()?;
            if queue.len() > 1 {
                lines.push("    queue[\"Queue\"]".to_string());
                for unit in queue {
                    lines.push(format!("    {} -.-> queue", ids.get(unit_id(unit)?)));
                }
            }
            if gateway.len() > 1 {
                lines.push("    gateway[\"Gateway\"]".to_string());
                for unit in gateway {
                    lines.push(format!("    {} -.-> gateway", ids.get(unit_id(unit)?)));
                }
            }
        }
    }
    let handoff = units_needing_handoff(&multi.units)?;
    if !handoff.is_empty() {
        lines.push(HANDOFF_NODE.to_string());
        for unit in handoff {
            lines.push(format!("    {} -.-> handoff", ids.get(unit_id(unit)?)));
        }
    }
    Ok(lines.join("\n"))
}

fn unit_ascii_label(unit: &Json) -> Res<String> {
    let mut label = esc(runtime_label(&unit_runtime_text(unit)?));
    if let Some(model) = unit_model(unit)? {
        label = format!("{label} ({})", esc(model));
    }
    Ok(label)
}

fn render_multi_unit_ascii(design: &Json) -> Res<String> {
    let multi = MultiUnit::new(design)?;
    let mut lines = vec!["Multi-unit Architecture:".to_string(), String::new()];
    if multi.is_temporal {
        lines.push(temporal_name(multi.temporal).to_string());
        lines.push("    |".to_string());
        lines.push("    v".to_string());
        for unit in &multi.units {
            let id = esc(unit_id(unit)?);
            let label = unit_ascii_label(unit)?;
            if unit_is(unit, "temporal_worker_poll") {
                lines.push(format!("  [ {id}: {label} ] <-- long-polls task queues"));
            } else {
                let queue = task_queue(unit)?
                    .map(|queue| format!(" (task queue: {})", esc(queue)))
                    .unwrap_or_default();
                lines.push(format!("    --> [ {id}: {label} ]{queue}"));
            }
        }
    } else {
        lines.push("User / Client".to_string());
        lines.push("    |".to_string());
        lines.push("    v".to_string());
        for unit in &multi.units {
            lines.push(format!(
                "  [ {}: {} ]",
                esc(unit_id(unit)?),
                unit_ascii_label(unit)?
            ));
        }
        let (queue, gateway) = multi.interconnects()?;
        for (name, members) in [("Queue", queue), ("Gateway", gateway)] {
            if members.len() > 1 {
                let ids = members
                    .iter()
                    .map(|unit| unit_id(unit).map(esc))
                    .collect::<Res<Vec<_>>>()?;
                lines.push(String::new());
                lines.push(format!("Interconnect: {name} \u{2014} {}", ids.join(", ")));
            }
        }
    }
    let handoff = units_needing_handoff(&multi.units)?;
    if !handoff.is_empty() {
        let ids = handoff
            .iter()
            .map(|unit| unit_id(unit).map(esc))
            .collect::<Res<Vec<_>>>()?;
        lines.push(String::new());
        lines.push(format!(
            "Note: compute configured by the migration skill \u{2014} {}",
            ids.join(", ")
        ));
    }
    Ok(lines.join("\n"))
}

/// `build_diagram`: the Mermaid and ASCII renderings of `result` and
/// `confirm`, from `design` when it has units.
///
/// # Errors
///
/// A [`DiagramError`] naming the field when a document has the wrong shape.
fn build_diagram(result: &Json, confirm: &Json, design: Option<&Json>) -> Res<Diagram> {
    if let Some(design) = design {
        let units = design_units(design)?;
        match units.as_slice() {
            [] => {}
            [unit] => return single_unit_diagram(unit, result, confirm),
            _ => {
                return Ok(Diagram {
                    mermaid: render_multi_unit_mermaid(design)?,
                    ascii: render_multi_unit_ascii(design)?,
                })
            }
        }
    }
    legacy_diagram(result, confirm)
}

// ---- command -----------------------------------------------------------------

/// Reads and parses a JSON document, naming the file safely on failure.
fn read_document(path: &Path) -> Res<Json> {
    read_json_document(path).map_err(DiagramError::new)
}

/// An optional document: a missing file is absent, a link or directory is an
/// error.
fn optional_document(path: &Path) -> Res<Option<Json>> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_document(path).map(Some),
        Err(error) if is_missing(&error) => Ok(None),
        Err(error) => Err(DiagramError::new(format!(
            "cannot read {}: {error}",
            safe_path(&path.display().to_string())
        ))),
    }
}

/// Handler for `startup build-diagram RESULT CONFIRM [DESIGN]`: composes the
/// diagram of a scoring result, writes it to `diagram.md` beside RESULT and
/// prints `RESULT=ok RUNTIME=<runtime>` (`multi-unit` with a design). A CONFIRM
/// or DESIGN file that does not exist is treated as absent.
///
/// # Errors
///
/// A usage error (exit status 2) without RESULT and CONFIRM; a runtime error
/// (exit status 1) when RESULT is missing, a document is unreadable, linked,
/// malformed or of the wrong shape, or `diagram.md` cannot be written.
pub fn build_diagram_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let (Some(result_arg), Some(confirm_arg)) =
        (invocation.positional(0), invocation.positional(1))
    else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "usage: toolkit startup build-diagram RESULT.json CONFIRM.json [DESIGN.json]",
        ));
    };
    let result_path = Path::new(result_arg);
    let result = read_document(result_path)?;
    object(&result, "the scoring result")?;
    let confirm =
        optional_document(Path::new(confirm_arg))?.unwrap_or_else(|| EMPTY_OBJECT.clone());
    object(&confirm, "the confirm document")?;
    let design = match invocation.positional(2) {
        Some(path) => optional_document(Path::new(path))?,
        None => None,
    };
    if let Some(design) = &design {
        object(design, "the design")?;
    }
    let diagram = build_diagram(&result, &confirm, design.as_ref())?;
    let document = format!(
        "```mermaid\n{}\n```\n\n<details><summary>ASCII (plain-text fallback)</summary>\n\n```\n{}\n```\n\n</details>\n",
        diagram.mermaid, diagram.ascii
    );
    let diagram_path = result_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(DIAGRAM_FILENAME);
    write_regular_file(&diagram_path, &document).map_err(|error| {
        ToolkitError::runtime(
            "startup_diagram_write",
            format!(
                "cannot write {}: {}",
                safe_path(&diagram_path.display().to_string()),
                read_failure(&error)
            ),
        )
    })?;
    let runtime = match design.as_ref().filter(|design| truthy(design)) {
        Some(_) => "multi-unit".to_string(),
        None => runtime_text(&resolve_runtime(&result, &confirm)?, "the runtime")?,
    };
    output.line(&format!(
        "RESULT=ok RUNTIME={}",
        safe_text(&redact_text(&runtime))
    ))
}
