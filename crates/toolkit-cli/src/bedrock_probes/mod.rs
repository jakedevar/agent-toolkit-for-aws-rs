//! Bedrock baseline, preflight and IAM policy tools (#4404, slice
//! `bedrock-probes`): the Rust replacement for `iam_policy.py`,
//! `preflight_bedrock.py` and `source_baseline.py` of the `llm-to-bedrock`
//! skill of the `aws-startup-advisor` plugin.
//!
//! - `toolkit bedrock-probes iam-policy --models ID[,ID...] --region R
//!   --account-id A [--output FILE]`
//! - `toolkit bedrock-probes preflight --region R --models ID[,ID...]
//!   [--dataset-size N] [--runtime-api API] [--recorded FILE]`
//! - `toolkit bedrock-probes api-path [--models ID[,ID...]] [--plan-path P]
//!   [--runtime-api API]`
//! - `toolkit bedrock-probes classify --models ID[,ID...] [--region R]`
//! - `toolkit bedrock-probes source-baseline ENV_FILE [--source-model-id ID]
//!   [--dataset FILE] [--output FILE] [--source-provider NAME] [--recorded FILE]`
//!
//! Nothing here talks to a network, reads credentials or signs a request: the
//! AWS and provider answers are recorded files (`--recorded`), and a run
//! without one fails closed. Live clients and the Bedrock token signer are a
//! follow-up. See `docs/migration/bedrock-probes.md` for the parity table and
//! every intentional change.

use std::path::Path;

use toolkit_core::{
    parse_json, read_regular_file, write_regular_file, Json, PathError, ToolkitError,
    MAX_TEXT_FILE_BYTES,
};

use crate::ai_data::parse_failure_text;
use crate::bedrock_resolve::source;
use crate::manifests::{read_failure, universal_newlines};
use crate::startup::artifacts::pyjson::dumps;
use crate::{Invocation, Output};

mod baseline;
mod client;
mod diag;
mod http;
mod iam;
mod preflight;
mod recorded;

/// Registration of the `bedrock-probes` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "bedrock-probes",
    commands: &[
        crate::CommandSpec {
            path: &["bedrock-probes", "iam-policy"],
            flags: &[],
            value_flags: IAM_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: iam_policy_command,
        },
        crate::CommandSpec {
            path: &["bedrock-probes", "preflight"],
            flags: &[],
            value_flags: BEDROCK_PREFLIGHT_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: preflight_command,
        },
        crate::CommandSpec {
            path: &["bedrock-probes", "api-path"],
            flags: &[],
            value_flags: API_PATH_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: api_path_command,
        },
        crate::CommandSpec {
            path: &["bedrock-probes", "classify"],
            flags: &[],
            value_flags: CLASSIFY_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: classify_command,
        },
        crate::CommandSpec {
            path: &["bedrock-probes", "source-baseline"],
            flags: &[],
            value_flags: BEDROCK_BASELINE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: source_baseline_command,
        },
    ],
    synopsis: r"  toolkit bedrock-probes iam-policy --models ID[,ID...] --region REGION
      --account-id ACCOUNT_ID [--output FILE]
  toolkit bedrock-probes preflight --region REGION --models ID[,ID...]
      [--dataset-size N] [--runtime-api converse|responses|chat_completions]
      [--recorded FILE]
  toolkit bedrock-probes api-path [--models ID[,ID...]] [--plan-path PATH]
      [--runtime-api API]
  toolkit bedrock-probes classify --models ID[,ID...] [--region REGION]
  toolkit bedrock-probes source-baseline ENV_FILE [--source-model-id ID]
      [--dataset FILE] [--output FILE] [--source-provider NAME] [--recorded FILE]
",
    description: r"  bedrock-probes
                Bedrock baseline, preflight and IAM policy tools of the
                llm-to-bedrock skill (#4404), offline. iam-policy prints the
                scoped policy for the model ids (the dual-ARN pattern for
                inference profiles, bedrock-mantle statements for bare GPT
                ids; an id, region or account id with a wildcard is refused).
                preflight probes each model and prints one JSON verdict (exit
                1 unless every model passes) from recorded provider answers
                (--recorded FILE); without a recording every probe fails
                closed. api-path resolves the API path of a migration plan and
                classify names the endpoint family of each id. source-baseline
                re-runs the golden prompts of --dataset against the source
                model from recorded replies (--recorded FILE) into --output,
                keeping live rows. Recorded inputs replace the live AWS and
                provider calls. See docs/migration/bedrock-probes.md.
",
    exit_notes: "",
    doc: "docs/migration/bedrock-probes.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

#[cfg(test)]
mod tests;

/// Value flags of `bedrock-probes iam-policy`.
pub(crate) const IAM_VALUE_FLAGS: &[&str] = &["--models", "--region", "--account-id", "--output"];
/// Value flags of `bedrock-probes preflight`.
///
/// The two constants that hold `--recorded` carry a `BEDROCK_` prefix:
/// `tests/recorded_contract.rs` matches such constants by bare name across
/// `src`, so a shared name (elasticache has its own `PREFLIGHT_VALUE_FLAGS`)
/// would put another slice's rows in recorded scope.
pub(crate) const BEDROCK_PREFLIGHT_VALUE_FLAGS: &[&str] = &[
    "--region",
    "--models",
    "--dataset-size",
    "--runtime-api",
    "--recorded",
];
/// Value flags of `bedrock-probes api-path`.
pub(crate) const API_PATH_VALUE_FLAGS: &[&str] = &["--models", "--plan-path", "--runtime-api"];
/// Value flags of `bedrock-probes classify`.
pub(crate) const CLASSIFY_VALUE_FLAGS: &[&str] = &["--models", "--region"];
/// Value flags of `bedrock-probes source-baseline`.
pub(crate) const BEDROCK_BASELINE_VALUE_FLAGS: &[&str] = &[
    "--source-model-id",
    "--dataset",
    "--output",
    "--source-provider",
    "--recorded",
];

/// Largest recorded response file.
const MAX_RECORDED_BYTES: u64 = 8 * 1024 * 1024;
/// Most model ids one call takes.
const MAX_MODELS: usize = 256;

/// An `argparse`-style usage error (exit status 2).
fn usage(message: &str) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", format!("invalid arguments: {message}"))
}

/// A runtime error (exit status 1) with a message that is already safe.
fn failure(code: &'static str, message: String) -> ToolkitError {
    ToolkitError::runtime(code, message)
}

fn path_failure(path: &str, error: &PathError, secrets: &[String]) -> String {
    format!(
        "cannot read {}: {}",
        diag::path(path, secrets),
        read_failure(error)
    )
}

fn read_json_file(path: &str, limit: u64, secrets: &[String]) -> Result<Json, String> {
    let text = read_regular_file(Path::new(path), limit)
        .map_err(|error| path_failure(path, &error, secrets))?;
    parse_json(&text).map_err(|error| {
        format!(
            "not valid JSON: {}: {}",
            diag::path(path, secrets),
            parse_failure_text(&error)
        )
    })
}

fn print_json(
    output: &mut Output<'_>,
    value: &Json,
    indent: Option<usize>,
) -> Result<(), ToolkitError> {
    output.line(&dumps(value, indent, false))
}

fn required<'a>(invocation: &'a Invocation, flag: &str) -> Result<&'a str, ToolkitError> {
    invocation
        .value(flag)
        .ok_or_else(|| usage(&format!("{flag} is required")))
}

fn no_positionals(invocation: &Invocation, command: &str) -> Result<(), ToolkitError> {
    if invocation.positionals.is_empty() {
        Ok(())
    } else {
        Err(usage(&format!("{command} takes no positional argument")))
    }
}

/// The model ids of `--models`, at most [`MAX_MODELS`].
fn model_list(models: &str) -> Result<Vec<String>, ToolkitError> {
    let ids = preflight::split_models(models);
    if ids.len() > MAX_MODELS {
        return Err(usage(&format!(
            "--models takes at most {MAX_MODELS} model ids"
        )));
    }
    Ok(ids)
}

/// `toolkit bedrock-probes iam-policy`.
pub(crate) fn iam_policy_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    no_positionals(invocation, "iam-policy")?;
    let models = model_list(required(invocation, "--models")?)?;
    let region = required(invocation, "--region")?;
    let account_id = required(invocation, "--account-id")?;
    // The script pasted whatever it was given into the resource ARNs, so a
    // region or id with a wildcard widened the policy. Refuse those inputs.
    if !toolkit_aws::is_valid_region(region) {
        return Err(usage("--region is not an AWS region name"));
    }
    if !iam::is_account_id(account_id) {
        return Err(usage("--account-id must be 12 digits"));
    }
    if !models.iter().all(|id| iam::is_literal_policy_text(id)) {
        return Err(usage(
            "a model id contains an IAM wildcard, a policy variable or a control character",
        ));
    }
    let policy = iam::generate_policy(&models, region, account_id);
    let text = dumps(&policy, Some(2), false) + "\n";
    match invocation.value("--output") {
        Some(path) => {
            write_regular_file(Path::new(path), &text).map_err(|error| {
                failure(
                    "output_unwritable",
                    format!(
                        "cannot write {}: {}",
                        diag::path(path, &[]),
                        read_failure(&error)
                    ),
                )
            })?;
            output.diagnostic(&format!("Policy written to {}", diag::path(path, &[])));
            Ok(())
        }
        // `print(output)`: the text already ends in a newline.
        None => output.line(&text),
    }
}

/// Python `int(text)` for an option value: surrounding whitespace, an optional
/// sign, ASCII digits with single underscores between them. A value beyond the
/// integer range saturates.
fn parse_int(text: &str) -> Option<i64> {
    let trimmed = crate::bedrock_resolve::identity::strip(text);
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let mut value: i64 = 0;
    for digit in digits.bytes().filter(u8::is_ascii_digit) {
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'));
    }
    Some(if negative { -value } else { value })
}

/// The recorded client of `--recorded FILE`, or the fail-closed no-live one.
fn probe_client(invocation: &Invocation) -> Result<Box<dyn client::ProbeClient>, ToolkitError> {
    let Some(path) = invocation.value("--recorded") else {
        return Ok(Box::new(client::NoLiveClient));
    };
    let document = read_json_file(path, MAX_RECORDED_BYTES, &[])
        .map_err(|message| failure("recorded_unreadable", message))?;
    recorded::RecordedClient::from_document(&document)
        .map(|client| Box::new(client) as Box<dyn client::ProbeClient>)
        .map_err(|message| failure("recorded_invalid", format!("invalid recording: {message}")))
}

/// `toolkit bedrock-probes preflight`.
pub(crate) fn preflight_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    no_positionals(invocation, "preflight")?;
    let region = required(invocation, "--region")?;
    let models = required(invocation, "--models")?;
    model_list(models)?;
    let dataset_size = match invocation.value("--dataset-size") {
        None => 0,
        Some(text) => parse_int(text).ok_or_else(|| usage("--dataset-size must be an integer"))?,
    };
    let runtime_api = invocation.value("--runtime-api").unwrap_or("converse");
    if !matches!(runtime_api, "converse" | "responses" | "chat_completions") {
        return Err(usage(
            "--runtime-api must be one of: converse, responses, chat_completions",
        ));
    }
    let mut client = probe_client(invocation)?;
    let options = preflight::Options {
        region,
        models,
        dataset_size,
        runtime_api,
    };
    let (report, all_ok) = preflight::run(&options, client.as_mut())
        .map_err(|message| failure("recorded_invalid_response", message))?;
    print_json(output, &report, Some(2))?;
    if !all_ok {
        output.set_exit_status(1);
    }
    Ok(())
}

/// `toolkit bedrock-probes api-path`.
pub(crate) fn api_path_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    no_positionals(invocation, "api-path")?;
    let models = match invocation.value("--models") {
        Some(models) => model_list(models)?,
        None => Vec::new(),
    };
    let path = preflight::normalize_api_path(
        invocation.value("--plan-path"),
        &models,
        invocation.value("--runtime-api"),
    )
    .map_err(|message| failure("api_path_unresolved", message))?;
    print_json(
        output,
        &Json::object(vec![("api_path", Json::string(path))]),
        None,
    )
}

/// `toolkit bedrock-probes classify`.
pub(crate) fn classify_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    no_positionals(invocation, "classify")?;
    let models = model_list(required(invocation, "--models")?)?;
    let region = invocation.value("--region").unwrap_or("");
    let rows = models
        .iter()
        .map(|id| {
            Json::object(vec![
                ("model_id", Json::string(id.clone())),
                ("mantle", Json::Bool(preflight::is_mantle_model(id))),
                ("iam_mantle", Json::Bool(iam::is_mantle_model(id, region))),
                ("embedding", Json::Bool(preflight::is_embedding_model(id))),
                (
                    "inference_profile",
                    Json::Bool(iam::is_inference_profile(id)),
                ),
                ("gpt6_cris", Json::Bool(iam::is_gpt6_cris_model(id))),
            ])
        })
        .collect();
    print_json(
        output,
        &Json::object(vec![("models", Json::Array(rows))]),
        Some(2),
    )
}

/// The ambient environment variable `var`, as the script's `os.environ` saw it
/// before the env file was loaded.
fn ambient_env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// One input of the script (`SOURCE_MODEL_ID`, `GOLDEN_DATASET_PATH`,
/// `OUTPUT_PATH`, `SOURCE_PROVIDER`), resolved without touching the process
/// environment.
///
/// `source_baseline.py:42-55` (`load_env_file`) wrote every file pair into
/// `os.environ` (`:54`) at `main:247`, before `main` (`:250-252`) and
/// `pick_provider_key` (`:138`) read these inputs, so a
/// file pair shadows the ambient variable, an empty pair included. The port
/// reads the same values from one lookup: the flag (new in the port) first,
/// then the env-file pair, then the ambient environment. An empty value counts
/// as unset (migration divergence 10; for `SOURCE_PROVIDER` the script's `""`
/// meant the same).
fn setting(
    invocation: &Invocation,
    pairs: &[(String, String)],
    ambient: &dyn Fn(&str) -> Option<String>,
    flag: &str,
    var: &str,
) -> Option<String> {
    let value = match invocation.value(flag) {
        Some(value) => Some(value.to_string()),
        None => match pairs.iter().find(|(name, _)| name == var) {
            Some((_, value)) => Some(value.clone()),
            None => ambient(var),
        },
    };
    value.filter(|value| !value.is_empty())
}

/// The text of a file that may be absent (a first run has no output file).
fn read_optional(path: &str, secrets: &[String]) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => read_regular_file(Path::new(path), MAX_TEXT_FILE_BYTES)
            .map(Some)
            .map_err(|error| path_failure(path, &error, secrets)),
    }
}

/// What a baseline run needs once its inputs are checked.
struct BaselineInputs {
    model: String,
    dataset_path: String,
    output_path: String,
    provider: baseline::Provider,
    api_key: String,
    /// Every provider key value of the env file (redacted from every row).
    secrets: Vec<String>,
}

/// Checks the script's inputs before any request. `Err` is the script's
/// `FAIL: ...` line (exit status 2).
fn baseline_inputs(
    invocation: &Invocation,
    pairs: &[(String, String)],
    ambient: &dyn Fn(&str) -> Option<String>,
) -> Result<BaselineInputs, String> {
    let lookup = |flag: &str, var: &str| setting(invocation, pairs, ambient, flag, var);
    let need = |flag: &str, var: &str| {
        lookup(flag, var).ok_or_else(|| format!("FAIL: required env var '{var}' not set"))
    };
    let model = need("--source-model-id", "SOURCE_MODEL_ID")?;
    let dataset_path = need("--dataset", "GOLDEN_DATASET_PATH")?;
    let output_path = need("--output", "OUTPUT_PATH")?;
    let stated = lookup("--source-provider", "SOURCE_PROVIDER").unwrap_or_default();
    let no_key =
        || "FAIL: no usable provider key in the env file (check SOURCE_PROVIDER)".to_string();
    let key_name = source::pick_provider(pairs, &stated).ok_or_else(no_key)?;
    let provider = baseline::Provider::from_key_name(key_name).ok_or_else(no_key)?;
    if provider == baseline::Provider::Gemini && !baseline::is_gemini_model_id(&model) {
        return Err("FAIL: the source model id is not valid for the Gemini endpoint (letters, digits, '.', '_' and '-' only)".to_string());
    }
    let value_of = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    Ok(BaselineInputs {
        model,
        dataset_path,
        output_path,
        provider,
        api_key: value_of(key_name).unwrap_or_default(),
        secrets: source::PROVIDER_KEYS
            .iter()
            .filter_map(|name| value_of(name))
            .collect(),
    })
}

/// The `live` rows of an earlier output, if the file exists.
fn previous_live_rows(output_path: &str, secrets: &[String]) -> Result<Vec<Json>, ToolkitError> {
    match read_optional(output_path, secrets)
        .map_err(|message| failure("output_unreadable", message))?
    {
        None => Ok(Vec::new()),
        Some(text) => {
            baseline::parse_live_rows(&text).map_err(|message| failure("output_invalid", message))
        }
    }
}

/// The script's `RESUME: N live baselines kept, M to fetch` line.
fn announce_resume(
    output: &mut Output<'_>,
    kept: &[Json],
    prompts: &[Json],
) -> Result<(), ToolkitError> {
    let mut done: Vec<&Json> = Vec::new();
    for id in kept.iter().filter_map(|row| row.get("id")) {
        if !done.contains(&id) {
            done.push(id);
        }
    }
    if done.is_empty() {
        return Ok(());
    }
    let to_fetch = prompts
        .iter()
        .filter(|prompt| prompt.get("id").is_none_or(|id| !done.contains(&id)))
        .count();
    output.line(&format!(
        "RESUME: {} live baselines kept, {to_fetch} to fetch",
        done.len()
    ))
}

/// The recorded client of `--recorded FILE`, or the fail-closed no-live one.
fn http_client(
    invocation: &Invocation,
    secrets: &[String],
) -> Result<Box<dyn http::HttpClient>, ToolkitError> {
    let Some(path) = invocation.value("--recorded") else {
        return Ok(Box::new(http::NoLiveHttp));
    };
    let document = read_json_file(path, MAX_RECORDED_BYTES, secrets)
        .map_err(|message| failure("recorded_unreadable", message))?;
    let client = http::RecordedHttp::from_document(&document)
        .map_err(|message| failure("recorded_invalid", format!("invalid recording: {message}")))?;
    Ok(Box::new(client))
}

/// Writes the rows. The script's `os.makedirs(os.path.dirname(path))` failed
/// for a bare file name; a missing parent directory is created and an empty one
/// is skipped.
fn write_rows(output_path: &str, rows: &[Json], secrets: &[String]) -> Result<(), ToolkitError> {
    let unwritable = |what: &str, why: String| {
        failure(
            "output_unwritable",
            format!("cannot {what} {}: {why}", diag::path(output_path, secrets)),
        )
    };
    if let Some(parent) = Path::new(output_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| unwritable("create the directory of", error.to_string()))?;
    }
    write_regular_file(Path::new(output_path), &baseline::rows_text(rows))
        .map_err(|error| unwritable("write", read_failure(&error)))
}

/// `toolkit bedrock-probes source-baseline`.
pub(crate) fn source_baseline_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let env_path = match invocation.positionals.as_slice() {
        [path] => path.as_str(),
        _ => return Err(usage("source-baseline takes exactly one ENV_FILE argument")),
    };
    let text = read_regular_file(Path::new(env_path), MAX_TEXT_FILE_BYTES)
        .map_err(|error| failure("env_file_unreadable", path_failure(env_path, &error, &[])))?;
    let pairs = source::parse_env_file(&universal_newlines(&text));
    let inputs = match baseline_inputs(invocation, &pairs, &ambient_env) {
        Ok(inputs) => inputs,
        Err(fail_line) => {
            output.diagnostic(&fail_line);
            output.set_exit_status(2);
            return Ok(());
        }
    };

    let dataset_text = read_regular_file(Path::new(&inputs.dataset_path), MAX_TEXT_FILE_BYTES)
        .map_err(|error| {
            failure(
                "dataset_unreadable",
                path_failure(&inputs.dataset_path, &error, &inputs.secrets),
            )
        })?;
    let prompts = baseline::parse_dataset(&dataset_text)
        .map_err(|message| failure("dataset_invalid", message))?;
    let kept = previous_live_rows(&inputs.output_path, &inputs.secrets)?;
    announce_resume(output, &kept, &prompts)?;

    let mut client = http_client(invocation, &inputs.secrets)?;
    let config = baseline::Run {
        provider: inputs.provider,
        model: &inputs.model,
        api_key: &inputs.api_key,
        secrets: &inputs.secrets,
    };
    let rows = baseline::run(&config, kept, &prompts, client.as_mut());
    write_rows(&inputs.output_path, &rows, &inputs.secrets)?;
    let live = rows
        .iter()
        .filter(|row| row.get("status").and_then(Json::as_str) == Some("live"))
        .count();
    output.line(&format!("live source baselines: {live}/{}", rows.len()))
}
