//! `toolkit ai-models ...`: deployable-model discovery, filtering and endpoint
//! diagnostics of the AWS AI/ML skill (#4384, slice `ai-models`).
//!
//! Ports six scripts under `skills/core-skills/aws-ai-ml/references/` (and the
//! byte-identical copies in `plugins/aws-core`):
//!
//! | Python | Command |
//! | --- | --- |
//! | `model-selection/scripts/get_deployable_models.py` | `ai-models list-models` |
//! | `model-selection/scripts/filter_deployable_models.py` | `ai-models filter` |
//! | `model-selection/scripts/search_deployable_models.py` | `ai-models search` |
//! | `model-selection/scripts/get_model_names.py` | `ai-models model-names` |
//! | `finetuning-technique/scripts/get_recipes.py` | `ai-models recipes` |
//! | `endpoint-diagnostics/scripts/collect_diagnostics.py` | `ai-models endpoint-diagnostics` |
//!
//! - [`client`]: the AWS seam. This build binds no live client: the default is
//!   [`client::NoLiveClient`], which sends nothing; `--recorded FILE` replays
//!   recorded responses deterministically ([`recorded::RecordedClient`]).
//! - [`normalize`], [`filter`], [`snapshot`], [`search`], [`hub`],
//!   [`diagnostics`]: the ported logic, one module per concern.
//!
//! Output JSON is `json.dumps` of the Python value (default separators, ASCII
//! only), so every control and bidirectional character of a Hub or log value
//! leaves the program as a `\uXXXX` escape. Text printed on the diagnostics
//! stream is built from fixed wording plus escaped, bounded quotations, and a
//! failure never echoes a caller-supplied identifier. See
//! `docs/migration/ai-models.md`.

use std::path::Path;

use toolkit_aws::{is_valid_region, AwsError};
use toolkit_core::{quote_untrusted, Json, ToolkitError};

use crate::dynamodb::pyfmt;
use crate::safe_text::safe_path;
use crate::startup::read_json_document;
use crate::{Invocation, Output};

mod client;
mod diagnostics;
mod filter;
mod hub;
mod normalize;
mod recorded;
mod search;
mod snapshot;
mod timeutil;

#[cfg(test)]
mod tests;

use client::{NoLiveClient, SageMakerClient};
use diagnostics::TimeRange;
use filter::{add_constraint, constraints_json, filter_models, Constraints, VALID_KEYS};
use hub::{shown, validate_content_name, validate_hub_name, ModelsError};
use recorded::RecordedClient;
use search::{SearchAdapterError, PUBLIC_HUB_NAME};
use snapshot::SnapshotError;

/// Flags of `ai-models list-models`.
pub const LIST_MODELS_FLAGS: &[&str] = &["--list-values", "--create-snapshot"];
/// Value flags of `ai-models list-models`.
pub const LIST_MODELS_VALUE_FLAGS: &[&str] = &["--snapshot-file", "--recorded"];
/// Value flags of `ai-models search`.
pub const SEARCH_VALUE_FLAGS: &[&str] = &["--region", "--snapshot-file", "--recorded"];
/// Value flags of the commands that only read recorded responses.
pub const RECORDED_VALUE_FLAGS: &[&str] = &["--recorded"];
/// Value flags of `ai-models endpoint-diagnostics`.
pub const DIAGNOSTICS_VALUE_FLAGS: &[&str] =
    &["--minutes", "--start", "--end", "--now", "--recorded"];
/// Positional arguments `ai-models filter` and `search` take: the models file
/// or Hub plus constraints.
pub const MAX_CONSTRAINT_POSITIONALS: usize = 256;

/// Largest `--minutes` (one year).
const MAX_MINUTES: i64 = 525_600;

/// Default `--minutes`.
const DEFAULT_MINUTES: i64 = 5;

/// Largest models file read, in bytes.
const MAX_MODELS_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn usage(message: impl Into<String>) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", message)
}

fn runtime(code: &'static str, message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime(code, message)
}

/// The Python `main()` wording for an AWS failure, with every AWS-derived part
/// escaped onto one line.
fn aws_failure(error: &AwsError) -> ToolkitError {
    let message = match error {
        AwsError::Service(service) => format!(
            "AWS API call failed ({}): {}",
            shown(service.code().unwrap_or("Unknown")),
            shown(service.message())
        ),
        AwsError::NoCredentials => "No AWS credentials found. Configure credentials via 'aws \
                                    configure' or environment variables."
            .to_string(),
        AwsError::Connect { .. } => format!(
            "Could not connect to SageMaker endpoint: {}",
            shown(&error.to_string())
        ),
        other => shown(&other.to_string()),
    };
    runtime("ai_models_aws", message)
}

fn models_failure(error: ModelsError) -> ToolkitError {
    match error {
        ModelsError::Aws(error) => aws_failure(&error),
        ModelsError::Data(message) => runtime("ai_models_data", shown(&message)),
    }
}

fn snapshot_failure(error: SnapshotError) -> ToolkitError {
    runtime("ai_models_snapshot", error.0)
}

fn search_failure(error: &SearchAdapterError) -> ToolkitError {
    runtime("ai_models_search", error.message().to_string())
}

/// The client of the run: recorded responses with `--recorded FILE`, else the
/// build's [`NoLiveClient`].
fn open_client(invocation: &Invocation) -> Result<Box<dyn SageMakerClient>, ToolkitError> {
    let Some(path) = invocation.value("--recorded") else {
        return Ok(Box::new(NoLiveClient));
    };
    let document = read_json_document(Path::new(path))
        .map_err(|reason| runtime("ai_models_recorded", reason))?;
    RecordedClient::from_json(&document)
        .map(|client| Box::new(client) as Box<dyn SageMakerClient>)
        .map_err(|reason| {
            runtime(
                "ai_models_recorded",
                format!("the recorded input is invalid: {reason}"),
            )
        })
}

fn print_json(output: &mut Output<'_>, value: &Json) -> Result<(), ToolkitError> {
    output.line(&pyfmt::dumps(value, false))
}

fn check_hub(hub: &str) -> Result<(), ToolkitError> {
    validate_hub_name(hub).map_err(usage)
}

/// A region argument: a valid AWS region name, never echoed.
fn check_region(region: &str) -> Result<(), ToolkitError> {
    if is_valid_region(region) {
        Ok(())
    } else {
        Err(usage("invalid AWS region: use a name such as us-east-1"))
    }
}

/// The region of a snapshot: the argument, else `AWS_REGION` /
/// `AWS_DEFAULT_REGION`, else none.
fn snapshot_region(argument: Option<&str>) -> Result<Option<String>, ToolkitError> {
    if let Some(region) = argument.filter(|region| !region.is_empty()) {
        check_region(region)?;
        return Ok(Some(region.to_string()));
    }
    for name in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
        if let Some(region) = std::env::var(name).ok().filter(|region| !region.is_empty()) {
            check_region(&region)?;
            return Ok(Some(region));
        }
    }
    Ok(None)
}

fn clock(invocation: &Invocation) -> Result<crate::cloudwatch::timefmt::UtcTime, ToolkitError> {
    match invocation.value("--now") {
        Some(text) => parse_instant("--now", text),
        None => timeutil::now()
            .ok_or_else(|| runtime("ai_models_clock", "the system clock is out of range")),
    }
}

fn parse_instant(
    flag: &str,
    text: &str,
) -> Result<crate::cloudwatch::timefmt::UtcTime, ToolkitError> {
    crate::cloudwatch::timefmt::parse_iso_timestamp(text)
        .map_err(|_| usage(format!("{flag} is not a valid ISO 8601 timestamp")))
}

/// Handler for `ai-models list-models HUB [REGION]`: the Hub's models as a JSON
/// array, the distinct metadata values (`--list-values`), or a catalog
/// snapshot (`--create-snapshot`, `--snapshot-file`).
///
/// # Errors
///
/// A usage error (exit status 2) for a bad argument combination, Hub name or
/// region; a runtime error (exit status 1) for a failed call, a malformed
/// response or an unwritable snapshot.
pub fn list_models_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(hub) = invocation.positional(0) else {
        return Err(usage(
            "usage: toolkit ai-models list-models HUB [REGION] [--list-values] \
             [--create-snapshot] [--snapshot-file FILE] [--recorded FILE]",
        ));
    };
    let create = invocation.has_flag("--create-snapshot");
    let list_values = invocation.has_flag("--list-values");
    let snapshot_path = invocation.value("--snapshot-file");
    if create && snapshot_path.is_some() {
        return Err(usage(
            "Use either --create-snapshot or --snapshot-file, not both",
        ));
    }
    if create && !list_values {
        return Err(usage("--create-snapshot requires --list-values"));
    }
    check_hub(hub)?;
    let region_argument = invocation.positional(1);
    if let Some(region) = region_argument {
        check_region(region)?;
    }
    let mut client = open_client(invocation)?;
    let models = hub::get_deployable_models(client.as_mut(), hub).map_err(models_failure)?;

    let mut snapshot: Option<Json> = None;
    let mut written: Option<std::path::PathBuf> = None;
    if create || snapshot_path.is_some() {
        let region = snapshot_region(region_argument)?;
        let fetched_at = timeutil::iso_utc(clock(invocation)?);
        let built = snapshot::build_catalog_snapshot(hub, region.as_deref(), &models, &fetched_at)
            .map_err(snapshot_failure)?;
        if create {
            written =
                Some(snapshot::create_catalog_snapshot_file(&built).map_err(snapshot_failure)?);
        } else if let Some(path) = snapshot_path {
            snapshot::write_catalog_snapshot(Path::new(path), &built).map_err(snapshot_failure)?;
        }
        snapshot = Some(built);
    }

    let available = |snapshot: &Option<Json>| -> Result<Json, ToolkitError> {
        match snapshot.as_ref().and_then(|s| s.get("available_values")) {
            Some(values) => Ok(values.clone()),
            None => normalize::list_available_values(&models)
                .map_err(|message| runtime("ai_models_data", shown(&message))),
        }
    };
    if let Some(path) = written {
        print_json(
            output,
            &Json::object(vec![
                ("snapshot_file", Json::String(path.display().to_string())),
                ("available_values", available(&snapshot)?),
            ]),
        )
    } else if list_values {
        print_json(output, &available(&snapshot)?)
    } else {
        print_json(output, &Json::Array(models))
    }
}

/// Reads a JSON array of model objects.
fn read_models_file(path: &str) -> Result<Vec<Json>, ToolkitError> {
    use toolkit_core::{parse_json, read_regular_file, PathError};

    let shown_path = safe_path(path);
    let text = read_regular_file(Path::new(path), MAX_MODELS_FILE_BYTES).map_err(|error| {
        let reason = match error {
            PathError::NotFound(_) => "File not found".to_string(),
            other => format!("cannot read: {}", crate::manifests::read_failure(&other)),
        };
        runtime("ai_models_read", format!("{reason}: {shown_path}"))
    })?;
    let document = parse_json(&text).map_err(|error| {
        runtime(
            "ai_models_read",
            format!(
                "Invalid JSON in {shown_path}: {}",
                crate::ai_data::parse_failure_text(&error)
            ),
        )
    })?;
    match document {
        // recorded-shape: a snapshot file (`--snapshot-file`) the user supplied, not a response;
        // every shape but a list of objects is rejected.
        Json::Array(models) if models.iter().all(|m| matches!(m, Json::Object(_))) => Ok(models),
        _ => Err(runtime(
            "ai_models_read",
            format!("{shown_path} must hold a JSON list of model objects"),
        )),
    }
}

/// Handler for `ai-models filter MODELS_JSON [KEY:VALUE...]`: applies the
/// constraints to a models file and prints the summary with the matches,
/// newest first.
///
/// # Errors
///
/// A usage error without `MODELS_JSON`; a runtime error (exit status 1) for an
/// unreadable or malformed file, a mistyped model member or a `bedrock` /
/// `customization` value other than `true` or `false`.
pub fn filter_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(file) = invocation.positional(0) else {
        return Err(usage(
            "usage: toolkit ai-models filter MODELS_JSON [KEY:VALUE ...]",
        ));
    };
    let mut constraints: Constraints = Vec::new();
    for word in invocation.positionals.iter().skip(1) {
        let Some((key, value)) = word.split_once(':') else {
            output.diagnostic(&format!(
                "Warning: Ignoring malformed constraint (no ':' separator): {}",
                quote_untrusted(word)
            ));
            continue;
        };
        if !VALID_KEYS.contains(&key) {
            output.diagnostic(&format!(
                "Warning: Unrecognized constraint key {}. Valid keys: {}",
                quote_untrusted(key),
                sorted_keys()
            ));
            continue;
        }
        add_constraint(&mut constraints, key, value.to_lowercase());
    }
    let models = read_models_file(file)?;
    let filtered = filter_models(&models, &constraints)
        .map_err(|message| runtime("ai_models_data", shown(&message)))?;
    let count = |n: usize| Json::Int(i64::try_from(n).unwrap_or(i64::MAX));
    print_json(
        output,
        &Json::object(vec![
            ("total_models", count(models.len())),
            ("filters_applied", constraints_json(&constraints)),
            ("matched", count(filtered.len())),
            ("models", Json::Array(filtered)),
        ]),
    )
}

fn sorted_keys() -> String {
    let mut keys = VALID_KEYS;
    keys.sort_unstable();
    keys.join(", ")
}

/// Handler for `ai-models search HUB [KEY:VALUE...]`: public-Hub candidate
/// selection through `SageMaker` Search with the complete local fallback.
///
/// # Errors
///
/// A runtime error (exit status 1) for an unsupported Hub, an unresolvable
/// constraint, a missing or mismatching snapshot or a failed catalog listing.
pub fn search_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(hub) = invocation.positional(0) else {
        return Err(usage(
            "usage: toolkit ai-models search SageMakerPublicHub [KEY:VALUE ...] [--region R] \
             [--snapshot-file FILE] [--recorded FILE]",
        ));
    };
    let region = invocation.value("--region");
    if let Some(region) = region {
        check_region(region)?;
    }
    let raw: Vec<&str> = invocation
        .positionals
        .iter()
        .skip(1)
        .map(String::as_str)
        .collect();
    let constraints = search::parse_constraints(&raw).map_err(|e| search_failure(&e))?;
    if hub != PUBLIC_HUB_NAME {
        return Err(runtime(
            "ai_models_search",
            "The public-Hub Search adapter supports SageMakerPublicHub only; private Hubs must \
             continue using ListHubContents",
        ));
    }
    let mut client = open_client(invocation)?;
    let (catalog, source): (Vec<Json>, &str) =
        if let Some(path) = invocation.value("--snapshot-file") {
            let loaded = snapshot::load_catalog_snapshot(Path::new(path), Some(hub), region)
                .map_err(snapshot_failure)?;
            let models = match loaded.get("catalog_models") {
                // recorded-shape: a snapshot file already validated by
                // `load_catalog_snapshot`, not a response.
                Some(Json::Array(models)) => models.clone(),
                _ => Vec::new(),
            };
            (models, "snapshot")
        } else {
            let models = hub::get_deployable_models(client.as_mut(), hub).map_err(|error| {
                runtime(
                    "ai_models_search",
                    match &error {
                        ModelsError::Aws(AwsError::Service(service)) => format!(
                            "Public-Hub catalog discovery failed ({}): {}",
                            shown(service.code().unwrap_or("Unknown")),
                            shown(service.message())
                        ),
                        ModelsError::Aws(other) => format!(
                            "Public-Hub catalog discovery failed: {}",
                            shown(&other.to_string())
                        ),
                        ModelsError::Data(message) => shown(message),
                    },
                )
            })?;
            (models, "live_list")
        };
    let selection =
        search::select_public_hub_models(hub, &constraints, &catalog, client.as_mut(), source)
            .map_err(|e| search_failure(&e))?;
    let count = |n: usize| Json::Int(i64::try_from(n).unwrap_or(i64::MAX));
    let mut members = vec![
        ("total_models".to_string(), count(catalog.len())),
        (
            "filters_applied".to_string(),
            constraints_json(&constraints),
        ),
        ("matched".to_string(), count(selection.models.len())),
        ("catalog_source".to_string(), Json::string(source)),
    ];
    members.extend(selection.output_members());
    print_json(output, &Json::Object(members))
}

/// Handler for `ai-models model-names HUB [REGION]`: the names of the
/// fine-tunable models of the Hub as a sorted JSON array.
///
/// # Errors
///
/// A usage error without HUB or for a bad name or region; a runtime error
/// (exit status 1) for a failed call or a malformed response.
pub fn model_names_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(hub) = invocation.positional(0) else {
        return Err(usage(
            "usage: toolkit ai-models model-names HUB [REGION] [--recorded FILE]",
        ));
    };
    check_hub(hub)?;
    if let Some(region) = invocation.positional(1) {
        check_region(region)?;
    }
    let mut client = open_client(invocation)?;
    let names = hub::customization_model_names(client.as_mut(), hub).map_err(models_failure)?;
    print_json(output, &names)
}

/// Handler for `ai-models recipes MODEL HUB`: the supported fine-tuning
/// techniques (`sft`, `dpo`, `rlvr`, `rlaif`) of one model as a JSON array.
///
/// # Errors
///
/// A usage error without MODEL and HUB or for a bad name; a runtime error
/// (exit status 1) for a failed call, which includes an unknown model.
pub fn recipes_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let (Some(model), Some(hub)) = (invocation.positional(0), invocation.positional(1)) else {
        return Err(usage(
            "usage: toolkit ai-models recipes MODEL HUB [--recorded FILE]",
        ));
    };
    validate_content_name(model).map_err(usage)?;
    check_hub(hub)?;
    let mut client = open_client(invocation)?;
    let techniques =
        hub::finetuning_techniques(client.as_mut(), hub, model).map_err(models_failure)?;
    print_json(output, &techniques)
}

fn parse_minutes(text: &str) -> Result<i64, ToolkitError> {
    match text.parse::<i64>() {
        Ok(minutes)
            if (1..=MAX_MINUTES).contains(&minutes) && text.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Ok(minutes)
        }
        _ => Err(usage(format!(
            "--minutes must be a whole number from 1 to {MAX_MINUTES}"
        ))),
    }
}

/// Handler for `ai-models endpoint-diagnostics ENDPOINT REGION`: endpoint
/// status, `CloudWatch` metrics and up to 100 recent container logs as JSON.
///
/// # Errors
///
/// A usage error for missing arguments, a bad name, region or window; a
/// runtime error (exit status 1) when a step fails other than with a service
/// error (those are listed in the result's `errors`).
pub fn endpoint_diagnostics_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let (Some(endpoint), Some(region)) = (invocation.positional(0), invocation.positional(1))
    else {
        return Err(usage("endpoint_name and region are required"));
    };
    check_region(region)?;
    let minutes = invocation.value("--minutes");
    let start = invocation.value("--start");
    let end = invocation.value("--end");
    let range = match (start, end, minutes) {
        (Some(_), _, Some(_)) => {
            return Err(usage("--minutes cannot be combined with --start"));
        }
        (None, Some(_), _) => return Err(usage("--end requires --start")),
        (Some(start), end, None) => TimeRange::Between {
            start: parse_instant("--start", start)?,
            end: end.map(|end| parse_instant("--end", end)).transpose()?,
        },
        (None, None, minutes) => TimeRange::Last(match minutes {
            Some(text) => parse_minutes(text)?,
            None => DEFAULT_MINUTES,
        }),
    };
    let now = clock(invocation)?;
    let mut client = open_client(invocation)?;
    let result = diagnostics::collect_endpoint_diagnostics(client.as_mut(), endpoint, range, now)
        .map_err(|error| match error {
        ModelsError::Data(message) if message.starts_with("invalid endpoint name") => {
            usage(message)
        }
        other => models_failure(other),
    })?;
    print_json(output, &result)
}
