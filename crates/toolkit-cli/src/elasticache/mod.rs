//! `toolkit elasticache ...` commands (slice `cache-analysis`, Issue #4392):
//! the Rust port of the `amazon-elasticache` skill's pricing, validation,
//! command-analysis, audit and dashboard scripts.
//!
//! | Command | Replaces |
//! | --- | --- |
//! | `elasticache price` | `price_calculator.py` (with `pricing.py`) |
//! | `elasticache estimate-serverless` | `serverless_estimator.py` (with `command_classifier.py`) |
//! | `elasticache validate-input` | `input_validator.py` |
//! | `elasticache validate-references` | `validate_references.py` |
//! | `elasticache preflight` | `migration_preflight.py` |
//! | `elasticache audit` | `security_audit.py` |
//! | `elasticache dashboard` | `generate_dashboards.py` |
//!
//! Prices come from one shared loader ([`pricing`]). This build has no network
//! or AWS client: prices are read from local files, and the preflight and
//! audit analyze recorded snapshots. Parity evidence, the intentional changes
//! and the follow-ups are in `docs/migration/elasticache.md`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::type_complexity
)]

use std::path::Path;

use toolkit_core::{parse_json, read_regular_file, write_regular_file, Json, ToolkitError};

use crate::aurora::args::{float_value, int_value, usage};
use crate::aurora::pyfmt::{dumps, Num};
use crate::{Invocation, Output};

pub mod audit;
pub mod calculator;
pub mod classifier;
pub mod dashboards;
pub mod preflight;
pub mod pricing;
pub mod references;
pub mod serverless;
pub mod text;
pub mod validator;

use dashboards::{CacheType, TemplateOptions};
use pricing::{PricingKind, PricingLoader, CSV_NOTE};
use text::{kv, quoted, set_member, shell_quote, show, splitext};

/// Upper bound for input CSV and JSON files (16 MiB).
pub const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

/// Boolean flags of `elasticache price`.
pub const PRICE_FLAGS: &[&str] = &["--interactive", "--show-ri-options", "--extended-support"];

/// Value flags of `elasticache price`.
pub const PRICE_VALUE_FLAGS: &[&str] = &[
    "--engine",
    "--mode",
    "--region",
    "--data-gb",
    "--ecpu-millions",
    "--ops-per-sec",
    "--node-type",
    "--nodes",
    "--ri-term",
    "--avg-payload-kb",
    "--pricing-csv",
    "--pricing-json",
];

/// Boolean flags of `elasticache estimate-serverless`.
pub const ESTIMATE_FLAGS: &[&str] = &["--json"];

/// Value flags of `elasticache estimate-serverless`.
pub const ESTIMATE_VALUE_FLAGS: &[&str] = &[
    "--input",
    "--endpoint",
    "--commandstats",
    "--pricing",
    "--pricing-json",
    "--output",
];

/// Boolean flags of `elasticache validate-references`.
pub const REFERENCES_FLAGS: &[&str] = &["--verbose", "--quiet"];

/// Value flags of `elasticache validate-references`.
pub const REFERENCES_VALUE_FLAGS: &[&str] = &["--skill-dir"];

/// Boolean flags of `elasticache preflight`.
pub const PREFLIGHT_FLAGS: &[&str] = &["--json"];

/// Value flags of `elasticache preflight`.
pub const PREFLIGHT_VALUE_FLAGS: &[&str] = &["--host", "--port", "--snapshot"];

/// Boolean flags of `elasticache audit`.
pub const AUDIT_FLAGS: &[&str] = &["--json"];

/// Value flags of `elasticache audit`.
pub const AUDIT_VALUE_FLAGS: &[&str] = &["--serverless", "--replication-group", "--describe"];

/// Boolean flags of `elasticache dashboard`.
pub const DASHBOARD_FLAGS: &[&str] = &["--no-dashboard", "--no-alarms"];

/// Value flags of `elasticache dashboard`.
pub const DASHBOARD_VALUE_FLAGS: &[&str] = &[
    "--serverless",
    "--replication-group",
    "--node-ids",
    "--region",
    "--sns-topic",
    "--max-storage-gb",
    "--storage-alarm-pct",
    "--memory-threshold",
    "--engine-cpu-threshold",
    "--replication-lag-threshold",
    "--hit-rate-threshold",
    "--throttle-threshold",
    "--read-latency-threshold",
    "--write-latency-threshold",
    "--ecpu-threshold",
    "--evictions-threshold",
    "--new-connections-threshold",
    "--output",
];

const ENGINES: &[&str] = &["valkey", "redis", "memcached"];
const RI_TERMS: &[&str] = &[
    "1yr_no_upfront",
    "1yr_partial_upfront",
    "1yr_all_upfront",
    "3yr_no_upfront",
    "3yr_partial_upfront",
    "3yr_all_upfront",
];

fn runtime(code: &'static str, message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime(code, message)
}

/// An `argparse` `choices=[...]` option: the value, or the default.
fn choice(
    invocation: &Invocation,
    flag: &str,
    choices: &[&str],
    default: &str,
) -> Result<String, ToolkitError> {
    match invocation.value(flag) {
        None => Ok(default.to_string()),
        Some(value) if choices.contains(&value) => Ok(value.to_string()),
        Some(value) => {
            let listed: Vec<String> = choices.iter().map(|name| format!("'{name}'")).collect();
            Err(usage(format!(
                "argument {flag}: invalid choice: {} (choose from {})",
                quoted(value),
                listed.join(", ")
            )))
        }
    }
}

fn required_text<'a>(invocation: &'a Invocation, flag: &str) -> Result<&'a str, ToolkitError> {
    invocation
        .value(flag)
        .ok_or_else(|| usage(format!("the following arguments are required: {flag}")))
}

/// Exactly one of two options (an `argparse` required mutually exclusive group).
fn exclusive<'a>(
    invocation: &'a Invocation,
    first: &str,
    second: &str,
) -> Result<(&'a str, &'a str), ToolkitError> {
    match (invocation.value(first), invocation.value(second)) {
        (Some(_), Some(_)) => Err(usage(format!(
            "argument {second}: not allowed with argument {first}"
        ))),
        (Some(value), None) => Ok((first, value)),
        (None, Some(value)) => Ok((second, value)),
        (None, None) => Err(usage(format!(
            "one of the arguments {first} {second} is required"
        ))),
    }
}

fn read_json_file(path: &str) -> Result<Json, ToolkitError> {
    let text = read_regular_file(Path::new(path), MAX_INPUT_BYTES)?;
    Ok(parse_json(&text)?)
}

fn load_pricing(
    invocation: &Invocation,
    csv_flag: &str,
    out: &mut Output<'_>,
) -> Result<PricingLoader, ToolkitError> {
    match (invocation.value(csv_flag), invocation.value("--pricing-json")) {
        (Some(_), Some(_)) => Err(usage(format!(
            "argument --pricing-json: not allowed with argument {csv_flag}"
        ))),
        (Some(path), None) => {
            let loader = PricingLoader::load_file(Path::new(path), PricingKind::Csv)?;
            // The CSV carries no reserved or Extended Support data.
            out.diagnostic(CSV_NOTE);
            Ok(loader)
        }
        (None, Some(path)) => PricingLoader::load_file(Path::new(path), PricingKind::Json),
        (None, None) => Err(usage(format!(
            "a pricing source is required: pass {csv_flag} FILE (a pricing CSV) or --pricing-json FILE (the AWS bulk pricing JSON); live AWS Bulk Pricing access is not available in this build"
        ))),
    }
}

fn pricing_error(message: String) -> ToolkitError {
    runtime("pricing_error", message)
}

fn default_region() -> String {
    for name in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                return value;
            }
        }
    }
    "us-east-1".to_string()
}

/// Handler of `toolkit elasticache price`.
pub fn price_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    if invocation.has_flag("--interactive") {
        return Err(usage(
            "--interactive is not available in this build; pass the options on the command line",
        ));
    }
    let engine = choice(invocation, "--engine", ENGINES, "valkey")?;
    let mode = choice(invocation, "--mode", &["serverless", "node"], "serverless")?;
    let region = invocation
        .value("--region")
        .map_or_else(default_region, str::to_string);
    let data_gb = float_value(invocation, "--data-gb")?.unwrap_or(1.0);
    let ecpu_millions = float_value(invocation, "--ecpu-millions")?;
    let ops_per_sec = float_value(invocation, "--ops-per-sec")?;
    let node_type = invocation
        .value("--node-type")
        .unwrap_or("cache.r7g.large")
        .to_string();
    let nodes = int_value(invocation, "--nodes")?.unwrap_or(2);
    let ri_term = match invocation.value("--ri-term") {
        None => None,
        Some(_) => Some(choice(invocation, "--ri-term", RI_TERMS, "")?),
    };
    let avg_payload_kb = float_value(invocation, "--avg-payload-kb")?.unwrap_or(1.0);

    let loader = load_pricing(invocation, "--pricing-csv", out)?;

    let ecpu_millions = match (ecpu_millions, ops_per_sec) {
        (Some(value), _) => value,
        (None, Some(ops)) if ops != 0.0 => calculator::estimate_ecpu_from_ops(ops, 1.0),
        (None, _) => 100.0,
    };

    if invocation.has_flag("--extended-support") {
        let result =
            calculator::estimate_extended_support(&loader, &engine, &region, &node_type, nodes);
        return out.line(&calculator::format_extended_support_report(&result));
    }
    if invocation.has_flag("--show-ri-options") {
        let report = calculator::format_ri_options(&loader, &engine, &region, &node_type, nodes)
            .map_err(pricing_error)?;
        return out.line(&report);
    }
    let mut fields = if mode == "serverless" {
        calculator::estimate_serverless(
            &loader,
            &engine,
            &region,
            data_gb,
            ecpu_millions,
            avg_payload_kb,
        )
        .map_err(pricing_error)?
    } else {
        calculator::estimate_node_based(
            &loader,
            &engine,
            &region,
            &node_type,
            nodes,
            ri_term.as_deref(),
        )
        .map_err(pricing_error)?
    };
    fields.push(kv("disclaimer", text::js(calculator::JSON_DISCLAIMER)));
    out.line(&dumps(&Json::Object(fields)))
}

/// Handler of `toolkit elasticache estimate-serverless`.
pub fn estimate_serverless_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let input = invocation.value("--input");
    if invocation.value("--endpoint").is_some() {
        return Err(usage(
            "--endpoint is not available in this build (no Redis/Valkey client); export INFO commandstats to a CSV and pass --input and --commandstats",
        ));
    }
    let Some(input) = input else {
        return Err(usage("--input (CSV) is required"));
    };

    let pricing = load_pricing(invocation, "--pricing", out)?;

    out.line(&format!("Loading clusters from: {}", show(input)))?;
    let cluster_text = read_regular_file(Path::new(input), MAX_INPUT_BYTES)?;
    let clusters = serverless::load_clusters(&cluster_text)
        .map_err(|message| runtime("input_invalid", message))?;
    out.line(&format!("  {} clusters loaded", clusters.len()))?;

    let mut all_commandstats = None;
    if let Some(path) = invocation.value("--commandstats") {
        out.line(&format!("Loading commandstats from: {}", show(path)))?;
        let stats_text = read_regular_file(Path::new(path), MAX_INPUT_BYTES)?;
        let parsed = serverless::parse_commandstats_file(&stats_text)
            .map_err(|message| runtime("input_invalid", message))?;
        out.line(&format!("  Stats for {} clusters", parsed.len()))?;
        all_commandstats = Some(parsed);
    }

    let mut results = Vec::new();
    for cluster in &clusters {
        let stats = all_commandstats.as_ref().and_then(|all| {
            all.iter()
                .find(|(name, _)| *name == cluster.cluster_name)
                .map(|(_, stats)| stats)
        });
        let mut warnings = Vec::new();
        let estimate = serverless::estimate_cluster(cluster, &pricing, stats, &mut warnings)
            .map_err(pricing_error)?;
        for warning in &warnings {
            out.diagnostic(warning);
        }
        results.push(estimate);
    }

    let output = invocation
        .value("--output")
        .unwrap_or("serverless_estimate.csv");
    if !results.is_empty() {
        write_regular_file(Path::new(output), &serverless::csv_report(&results))?;
        out.line(&format!("Output written to: {}", show(output)))?;
    }
    for line in serverless::summary_lines(&results) {
        out.line(&line)?;
    }

    if invocation.has_flag("--json") {
        let (base, _) = splitext(output);
        let json_path = format!("{base}.json");
        let document = Json::Array(
            results
                .iter()
                .map(serverless::ClusterResult::to_json)
                .collect(),
        );
        write_regular_file(Path::new(&json_path), &dumps(&document))?;
        out.line(&format!("JSON written to: {}", show(&json_path)))?;
    }
    Ok(())
}

/// Handler of `toolkit elasticache validate-input`.
pub fn validate_input_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if invocation.positionals.is_empty() {
        out.line("Usage: toolkit elasticache validate-input key=value [key=value ...]")?;
        out.line(
            "Example: toolkit elasticache validate-input cache_name=my-cache region=us-east-1",
        )?;
        out.line("")?;
        out.line(&format!(
            "Recognized keys: {}",
            validator::RECOGNIZED_KEYS.join(", ")
        ))?;
        return Ok(());
    }
    let mut params: Vec<(String, Json)> = Vec::new();
    for (index, argument) in invocation.positionals.iter().enumerate() {
        let Some((key, value)) = argument.split_once('=') else {
            return Err(usage(format!(
                "argument {} is not in key=value format",
                index + 1
            )));
        };
        set_member(&mut params, key, Json::String(value.to_string()));
    }
    let errors = validator::validate_all(&params);
    if errors.is_empty() {
        return out.line("All inputs valid.");
    }
    out.line("Validation FAILED:")?;
    for error in &errors {
        out.line(&format!("  - {}", show(error)))?;
    }
    out.set_exit_status(1);
    Ok(())
}

/// Handler of `toolkit elasticache validate-references`.
pub fn validate_references_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let skill_dir = Path::new(invocation.value("--skill-dir").unwrap_or("."));
    let mut results = Vec::new();

    out.line("Validating reference files...")?;
    let (reference_results, reference_count) = references::validate_all_references(skill_dir);
    results.extend(reference_results);
    out.line(&format!("  Scanned {reference_count} reference .md files"))?;

    out.line("Validating scripts...")?;
    let (script_results, script_count) = references::validate_all_scripts(skill_dir);
    results.extend(script_results);
    out.line(&format!("  Scanned {script_count} script .py files"))?;

    let report = references::render_results(
        &results,
        invocation.has_flag("--verbose"),
        invocation.has_flag("--quiet"),
    );
    for line in &report.lines {
        out.line(line)?;
    }
    if report.failures > 0 {
        out.set_exit_status(1);
    }
    Ok(())
}

/// Handler of `toolkit elasticache preflight`.
pub fn preflight_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let host = required_text(invocation, "--host")?;
    let port = int_value(invocation, "--port")?.unwrap_or(6379);
    let snapshot = match invocation.value("--snapshot") {
        Some(path) => Some(read_json_file(path)?),
        None => None,
    };
    let result = preflight::run_preflight(host, port, snapshot.as_ref())?;
    if invocation.has_flag("--json") {
        out.line(&dumps(&result.to_json()))?;
    } else {
        out.line(&preflight::format_report(&result))?;
    }
    if result.failed() {
        out.set_exit_status(1);
    }
    Ok(())
}

/// Handler of `toolkit elasticache audit`.
pub fn audit_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let (mode, identifier) = exclusive(invocation, "--serverless", "--replication-group")?;
    let Some(path) = invocation.value("--describe") else {
        return Err(runtime(
            "aws_unavailable",
            "Failed to create AWS session: AWS SDK access is not available in this build; pass --describe FILE with the recorded describe_* responses",
        ));
    };
    let describe = read_json_file(path)?;
    let result = if mode == "--serverless" {
        audit::audit_serverless(&describe, identifier)
    } else {
        audit::audit_replication_group(&describe, identifier)
    };
    if invocation.has_flag("--json") {
        out.line(&dumps(&result.to_json()))?;
    } else {
        out.line(&audit::format_report(&result))?;
    }
    if result.failed() {
        out.set_exit_status(1);
    }
    Ok(())
}

/// A threshold option: absent stays `default`, a given value is a float.
fn threshold(
    invocation: &Invocation,
    flag: &str,
    default: Option<Num>,
) -> Result<Option<Num>, ToolkitError> {
    Ok(float_value(invocation, flag)?.map(Num::Float).or(default))
}

/// Handler of `toolkit elasticache dashboard`.
pub fn dashboard_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let (mode, identifier) = exclusive(invocation, "--serverless", "--replication-group")?;
    let node_ids = invocation.value("--node-ids").map(|text| {
        text.split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let options = TemplateOptions {
        cache_type: if mode == "--serverless" {
            CacheType::Serverless
        } else {
            CacheType::NodeBased
        },
        identifier: identifier.to_string(),
        region: invocation
            .value("--region")
            .unwrap_or("us-east-1")
            .to_string(),
        sns_topic: invocation.value("--sns-topic").map(str::to_string),
        include_dashboard: !invocation.has_flag("--no-dashboard"),
        include_alarms: !invocation.has_flag("--no-alarms"),
        max_storage_gb: float_value(invocation, "--max-storage-gb")?,
        storage_alarm_pct: threshold(invocation, "--storage-alarm-pct", Some(Num::Int(80)))?
            .unwrap_or(Num::Int(80)),
        memory_threshold: threshold(invocation, "--memory-threshold", Some(Num::Int(80)))?,
        engine_cpu_threshold: threshold(invocation, "--engine-cpu-threshold", Some(Num::Int(90)))?,
        replication_lag_threshold: threshold(invocation, "--replication-lag-threshold", None)?,
        hit_rate_threshold: threshold(invocation, "--hit-rate-threshold", Some(Num::Int(80)))?,
        throttle_threshold: threshold(invocation, "--throttle-threshold", None)?,
        read_latency_threshold: threshold(invocation, "--read-latency-threshold", None)?,
        write_latency_threshold: threshold(invocation, "--write-latency-threshold", None)?,
        ecpu_threshold: threshold(invocation, "--ecpu-threshold", None)?,
        evictions_threshold: threshold(invocation, "--evictions-threshold", None)?,
        new_connections_threshold: threshold(invocation, "--new-connections-threshold", None)?,
        node_ids,
    };
    let template = dashboards::generate_template(&options)?;
    let rendered = dumps(&template);
    match invocation.value("--output") {
        Some(path) => {
            write_regular_file(Path::new(path), &rendered)?;
            out.line(&format!("Written to {}", show(path)))?;
            out.line("")?;
            out.line("Deploy with:")?;
            out.line(&format!(
                "  aws cloudformation deploy --template-file {} --stack-name {}",
                shell_quote(path),
                shell_quote(&format!("{identifier}-observability"))
            ))
        }
        None => out.line(&rendered),
    }
}

#[cfg(test)]
mod tests;
