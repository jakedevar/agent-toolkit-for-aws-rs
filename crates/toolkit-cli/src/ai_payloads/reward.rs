//! Offline validation of the documented reward Lambda input and output contracts.
use crate::{Invocation, Output};
use std::{collections::HashSet, path::Path};
use toolkit_core::{parse_json, read_regular_file, Json, ToolkitError, MAX_TEXT_FILE_BYTES};

// AWS SageMaker custom scorer sample input, per-format sections:
// https://docs.aws.amazon.com/sagemaker/latest/dg/model-customize-evaluation-preset-custom-scorers.html#model-customize-evaluation-custom-scorers-lambda-input
// Nova sample payload: https://docs.aws.amazon.com/nova/latest/nova2-userguide/nova-implementing-reward-functions.html#nova-reward-interface
fn sample(format: &str) -> Option<&'static str> {
    match format {
        "openai" => Some(
            r#"[{"id":"123","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"What is the capital of France?"},{"role":"assistant","content":"Paris"},{"role":"assistant","content":"The capital of France is Paris."}],"reference_answer":{"text":"Paris"}}]"#,
        ),
        "verl" => Some(
            r#"[{"data_source":"openai/gsm8k","prompt":[{"role":"user","content":"What is the capital of France?"},{"role":"assistant","content":"The capital of France is Paris."}],"response":"The capital of France is Paris.","reward_model":{"style":"rule","ground_truth":"Paris"},"extra_info":{"reference_answer":{"text":"Paris"},"processor_config":{"aggregation":"mean"}}}]"#,
        ),
        "hf-prompt-completion" => Some(
            r#"[{"id":"123","prompt":"What is the capital of France?","completion":"The capital of France is Paris.","reference_answer":{"text":"Paris"}}]"#,
        ),
        "hf-preference" => Some(
            r#"[{"id":"123","prompt":"What is the capital of France?","completion":"The capital of France is Paris.","chosen":"Paris","rejected":"London","reference_answer":{"text":"Paris"}}]"#,
        ),
        "sagemaker-eval" => Some(
            r#"[{"id":"123","model_response":"The capital of France is Paris.","query":"What is the capital of France?","response":"Paris","system":"You are a helpful assistant.","reference_answer":{"text":"Paris"}}]"#,
        ),
        "nova" => Some(
            r#"{"id":"123","messages":[{"role":"user","content":"Do you have a dedicated security team?"},{"role":"assistant","content":"As an AI developed by Amazon, I don not have a dedicated security team..."}],"reference_answer":{"compliant":"No","explanation":"As an AI developed by Company, I do not have a traditional security team..."}}"#,
        ),
        _ => None,
    }
}
fn format_arg(inv: &Invocation) -> Result<&str, ToolkitError> {
    inv.value("--format")
        .filter(|v| sample(v).is_some())
        .ok_or_else(|| {
            ToolkitError::usage(
                "reward_format",
                "--format must name a supported reward format",
            )
        })
}
pub(super) fn sample_command(inv: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    out.line(sample(format_arg(inv)?).expect("validated format"))
}
fn read(inv: &Invocation, flag: &str) -> Result<Json, ToolkitError> {
    let path = inv.value(flag).ok_or_else(|| {
        ToolkitError::usage("reward_arguments", "--event and --response are required")
    })?;
    let text = read_regular_file(Path::new(path), MAX_TEXT_FILE_BYTES).map_err(|_| {
        ToolkitError::runtime(
            "reward_read",
            format!("cannot read {flag} as a bounded regular file"),
        )
    })?;
    parse_json(&text)
        .map_err(|_| ToolkitError::runtime("reward_json", format!("invalid JSON in {flag}")))
}
fn string_field(v: &Json, name: &str, pos: &str, errors: &mut Vec<String>) {
    if v.get(name).and_then(Json::as_str).is_none() {
        errors.push(format!("{pos}.{name}: expected string"));
    }
}
fn reference(v: &Json, pos: &str, errors: &mut Vec<String>) {
    match v.get("reference_answer") {
        Some(r @ Json::Object(_)) => {
            string_field(r, "text", &format!("{pos}.reference_answer"), errors);
        }
        _ => errors.push(format!("{pos}.reference_answer.text: expected string")),
    }
}
fn messages(v: &Json, field: &str, pos: &str, assistant: bool, errors: &mut Vec<String>) {
    let Some(Json::Array(items)) = v.get(field) else {
        errors.push(format!("{pos}.{field}: expected non-empty array"));
        return;
    };
    if items.is_empty() {
        errors.push(format!("{pos}.{field}: expected non-empty array"));
    }
    for (i, item) in items.iter().enumerate() {
        let p = format!("{pos}.{field}[{i}]");
        string_field(item, "role", &p, errors);
        string_field(item, "content", &p, errors);
    }
    if assistant
        && items
            .last()
            .and_then(|m| m.get("role"))
            .and_then(Json::as_str)
            != Some("assistant")
    {
        errors.push(format!("{pos}.{field}[last].role: expected assistant"));
    }
}
fn validate_event(event: &Json, format: &str, batch: bool, errors: &mut Vec<String>) -> Vec<Json> {
    let items = match event {
        Json::Array(a)
            if !a.is_empty()
                && a.len() <= 1000
                && a.iter().all(|x| matches!(x, Json::Object(_))) =>
        {
            a.clone()
        }
        Json::Object(_) if format == "nova" => vec![event.clone()],
        _ => {
            errors.push("event_shape: expected non-empty sample object array (or nova object), at most 1000".into());
            return vec![];
        }
    };
    if format != "nova" && !batch && items.len() != 1 {
        errors.push(
            "event_sample_count: expected one sample; --batch enables local extension".into(),
        );
    }
    for (i, v) in items.iter().enumerate() {
        let p = format!("event[{i}]");
        if format != "verl" || v.get("id").is_some() {
            string_field(v, "id", &p, errors);
        }
        match format {
            "openai" | "nova" => {
                messages(v, "messages", &p, format == "openai", errors);
                if format == "openai" {
                    reference(v, &p, errors);
                }
            }
            "verl" => {
                string_field(v, "data_source", &p, errors);
                string_field(v, "response", &p, errors);
                messages(v, "prompt", &p, false, errors);
                match v.get("extra_info") {
                    Some(x @ Json::Object(_)) => reference(x, &format!("{p}.extra_info"), errors),
                    _ => errors.push(format!(
                        "{p}.extra_info.reference_answer.text: expected string"
                    )),
                }
                if v.get("reward_model")
                    .is_some_and(|x| !matches!(x, Json::Object(_)))
                {
                    errors.push(format!("{p}.reward_model: expected object"));
                }
            }
            "hf-prompt-completion" | "hf-preference" => {
                for f in ["prompt", "completion"] {
                    string_field(v, f, &p, errors);
                }
                if format == "hf-preference" {
                    for f in ["chosen", "rejected"] {
                        string_field(v, f, &p, errors);
                    }
                }
                reference(v, &p, errors);
            }
            "sagemaker-eval" => {
                for f in ["model_response", "query", "response"] {
                    string_field(v, f, &p, errors);
                }
                if v.get("system").is_some() {
                    string_field(v, "system", &p, errors);
                }
                reference(v, &p, errors);
            }
            _ => unreachable!(),
        }
    }
    items
}
fn number(v: Option<&Json>) -> bool {
    matches!(v, Some(Json::Int(_))) || matches!(v, Some(Json::Float(n)) if n.is_finite())
}
fn validate_response(
    response: Json,
    samples: &[Json],
    errors: &mut Vec<String>,
) -> Option<(&'static str, bool)> {
    let (body, envelope) = match response {
        Json::Array(a) => (a, "A"),
        Json::Object(ref obj) if obj.iter().any(|(k, _)| k == "statusCode") => {
            let status = response.get("statusCode");
            if status != Some(&Json::Int(200)) {
                errors.push("statusCode: expected integer 200; non-200 counts in byoc_failure_count and drops custom metrics".into());
            }
            let Some(s) = response.get("body").and_then(Json::as_str) else {
                errors.push("body: expected JSON string, not a nested value".into());
                return None;
            };
            if let Ok(Json::Array(a)) = parse_json(s) {
                (a, "B")
            } else {
                errors.push("body: expected JSON string containing a result list".into());
                return None;
            }
        }
        _ => {
            errors.push("response: matches neither documented envelope (C10 Option A/B)".into());
            return None;
        }
    };
    if body.len() != samples.len() {
        errors.push("body: result count differs from sample count".into());
    }
    let ids: HashSet<&str> = samples
        .iter()
        .filter_map(|s| s.get("id").and_then(Json::as_str))
        .collect();
    let mut seen = HashSet::new();
    let mut unverified = false;
    for (i, item) in body.iter().enumerate() {
        let p = format!("body[{i}]");
        match item.get("id").and_then(Json::as_str) {
            Some(id) => {
                if !seen.insert(id) {
                    errors.push(format!("{p}.id: duplicate result id"));
                }
                if samples.get(i).is_some_and(|s| s.get("id").is_none()) {
                    unverified = true;
                    if ids.contains(id) {
                        errors.push(format!("{p}.id: belongs to another sample"));
                    }
                } else if !ids.contains(id) {
                    errors.push(format!("{p}.id: no matching sample"));
                }
            }
            None => errors.push(format!("{p}.id: expected string")),
        }
        if !number(item.get("aggregate_reward_score")) {
            errors.push(format!(
                "{p}.aggregate_reward_score: expected finite number"
            ));
        }
        if let Some(metrics) = item.get("metrics_list") {
            if let Json::Array(metrics) = metrics {
                for (j, metric) in metrics.iter().enumerate() {
                    let m = format!("{p}.metrics_list[{j}]");
                    string_field(metric, "name", &m, errors);
                    if !number(metric.get("value")) {
                        errors.push(format!("{m}.value: expected finite number"));
                    }
                    if !matches!(
                        metric.get("type").and_then(Json::as_str),
                        Some("Reward" | "Metric")
                    ) {
                        errors.push(format!("{m}.type: expected Reward or Metric"));
                    }
                }
            } else {
                errors.push(format!("{p}.metrics_list: expected array"));
            }
        }
    }
    for (i, s) in samples.iter().enumerate() {
        if let Some(id) = s.get("id").and_then(Json::as_str) {
            if !seen.contains(id) {
                errors.push(format!("event[{i}].id: no matching result"));
            }
        }
    }
    Some((envelope, unverified))
}
pub(super) fn check_command(inv: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let format = format_arg(inv)?;
    if inv.value("--event").is_none() || inv.value("--response").is_none() {
        return Err(ToolkitError::usage(
            "reward_arguments",
            "--event and --response are required",
        ));
    }
    let event = read(inv, "--event")?;
    let mut errors = Vec::new();
    let samples = validate_event(&event, format, inv.has_flag("--batch"), &mut errors);
    if !errors.is_empty() {
        for e in errors {
            out.diagnostic(&e);
        }
        out.set_exit_status(1);
        return Ok(());
    }
    let response = read(inv, "--response")?;
    let result = validate_response(response, &samples, &mut errors);
    if !errors.is_empty() {
        for e in errors {
            out.diagnostic(&e);
        }
        out.set_exit_status(1);
        return Ok(());
    }
    let (envelope, unverified) = result.expect("valid response");
    let batch = if format != "nova" && inv.has_flag("--batch") {
        " (batch: local extension, the service sends 1)"
    } else {
        ""
    };
    let note = if unverified {
        "; id not compared (verl sample without id)"
    } else {
        ""
    };
    out.line(&format!(
        "ok: {format}, {} sample(s), {} result(s), envelope {envelope}{batch}{note}",
        samples.len(),
        samples.len()
    ))
}
