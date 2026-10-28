//! Bedrock migration identity, image and pricing resolution (#4403, slice
//! `bedrock-resolve`): the Rust replacement for `model_identity.py`,
//! `image_input.py`, `bedrock_pricing.py` and `resolve_source_model.py` of the
//! `llm-to-bedrock` skill of the `aws-startup-advisor` plugin.
//!
//! - `toolkit bedrock-resolve identity --provider P (--source S --target T |
//!   --mappings FILE)`
//! - `toolkit bedrock-resolve image --emit KIND [--image PATH] [--prompt TEXT |
//!   --prompt-file FILE] [--detail LEVEL]`
//! - `toolkit bedrock-resolve pricing --region R --models ID[,ID...]
//!   [--price-list FILE]`
//! - `toolkit bedrock-resolve source-model ENV_FILE --catalog FILE
//!   [--plan-model-id ID] [--source-provider NAME]`
//!
//! Nothing here talks to a network: the Pricing API answer and the provider
//! catalog are recorded files, and live clients are follow-up #4675. The
//! canonical same-model rule lives in [`identity`] and is shared with
//! `bedrock-report`. See `docs/migration/bedrock-resolve.md` for the parity
//! table and every intentional change.

use std::io::Read as _;
use std::path::Path;

use toolkit_core::{
    open_regular_file, parse_json, read_regular_file, Json, PathError, ToolkitError,
    MAX_TEXT_FILE_BYTES,
};

use crate::ai_data::parse_failure_text;
use crate::manifests::{read_failure, universal_newlines};
use crate::safe_text::{safe_path, safe_text};
use crate::startup::artifacts::pyjson::dumps;
use crate::{Invocation, Output};

pub(crate) mod identity;
mod image;
mod pricing;
mod pricing_table;
mod source;

/// Registration of the `bedrock-resolve` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "bedrock-resolve",
    commands: &[
        crate::CommandSpec {
            path: &["bedrock-resolve", "identity"],
            flags: &[],
            value_flags: IDENTITY_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: identity_command,
        },
        crate::CommandSpec {
            path: &["bedrock-resolve", "image"],
            flags: &[],
            value_flags: IMAGE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: image_command,
        },
        crate::CommandSpec {
            path: &["bedrock-resolve", "pricing"],
            flags: &[],
            value_flags: PRICING_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: pricing_command,
        },
        crate::CommandSpec {
            path: &["bedrock-resolve", "source-model"],
            flags: &[],
            value_flags: SOURCE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: source_command,
        },
    ],
    synopsis: r"  toolkit bedrock-resolve identity --provider anthropic|openai
      (--source MODEL --target BEDROCK_ID | --mappings FILE)
  toolkit bedrock-resolve image --emit format|mime|data-url|converse|responses|chat
      [--image PATH] [--prompt TEXT | --prompt-file FILE] [--detail low|high|auto|original]
  toolkit bedrock-resolve pricing --region REGION --models ID[,ID...] [--price-list FILE]
  toolkit bedrock-resolve source-model ENV_FILE --catalog FILE
      [--plan-model-id ID] [--source-provider NAME]
",
    description: r#"  bedrock-resolve
                Model identity, image, pricing and source-model resolution of
                the llm-to-bedrock skill (#4403), offline. identity prints
                {"same_model": bool} for a source model and a Bedrock id (or
                {"same_model_family": bool} for a mappings file). image builds
                a Converse, Responses or Chat Completions user message, a data
                URL or the Converse format of an image file. pricing prints
                the per-1K-token USD rates of the listed model ids from the
                built-in table (available: false for anything unpriced; a
                recorded Pricing API answer may be given with --price-list).
                source-model resolves PLAN_MODEL_ID (or --plan-model-id)
                against a recorded provider catalog (--catalog), picking the
                same line only for a date or pure version suffix: exit 2
                {"status": "no_key"}, exit 3 {"status": "error"}. Recorded
                inputs replace the live provider and Pricing API calls. See
                docs/migration/bedrock-resolve.md.
"#,
    exit_notes: "",
    doc: "docs/migration/bedrock-resolve.md",
    synopsis_rank: 320,
    description_rank: 320,
};

#[cfg(test)]
mod tests;

/// Value flags of `bedrock-resolve identity`.
pub(crate) const IDENTITY_VALUE_FLAGS: &[&str] =
    &["--provider", "--source", "--target", "--mappings"];
/// Value flags of `bedrock-resolve image`.
pub(crate) const IMAGE_VALUE_FLAGS: &[&str] =
    &["--emit", "--image", "--prompt", "--prompt-file", "--detail"];
/// Value flags of `bedrock-resolve pricing`.
pub(crate) const PRICING_VALUE_FLAGS: &[&str] = &["--region", "--models", "--price-list"];
/// Value flags of `bedrock-resolve source-model`.
pub(crate) const SOURCE_VALUE_FLAGS: &[&str] =
    &["--catalog", "--plan-model-id", "--source-provider"];

/// Largest image file a payload is built from.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Largest recorded catalog or price list.
const MAX_RECORDED_BYTES: u64 = 8 * 1024 * 1024;
/// Most model ids one `pricing` call looks up.
const MAX_MODELS: usize = 256;

/// An `argparse`-style usage error (exit status 2).
fn usage(message: &str) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", format!("invalid arguments: {message}"))
}

/// A runtime error (exit status 1) with a message that is already safe.
fn failure(code: &'static str, message: String) -> ToolkitError {
    ToolkitError::runtime(code, message)
}

fn path_failure(path: &str, error: &PathError) -> String {
    format!("cannot read {}: {}", safe_path(path), read_failure(error))
}

fn read_json_file(path: &str, limit: u64) -> Result<Json, String> {
    let text =
        read_regular_file(Path::new(path), limit).map_err(|error| path_failure(path, &error))?;
    parse_json(&text).map_err(|error| {
        format!(
            "not valid JSON: {}: {}",
            safe_path(path),
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

/// `toolkit bedrock-resolve identity`.
pub(crate) fn identity_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if !invocation.positionals.is_empty() {
        return Err(usage("identity takes no positional argument"));
    }
    let provider = required(invocation, "--provider")?;
    let pair = (invocation.value("--source"), invocation.value("--target"));
    let result = match (invocation.value("--mappings"), pair) {
        (Some(_), (Some(_), _) | (_, Some(_))) => {
            return Err(usage(
                "--mappings cannot be combined with --source or --target",
            ));
        }
        (Some(path), (None, None)) => {
            let document = read_json_file(path, MAX_TEXT_FILE_BYTES)
                .map_err(|message| failure("mappings_unreadable", message))?;
            let Json::Array(rows) = document else {
                return Err(failure(
                    "mappings_not_a_list",
                    "the mappings file must hold a JSON array".to_string(),
                ));
            };
            Json::object(vec![(
                "same_model_family",
                Json::Bool(identity::same_model_family(provider, &rows)),
            )])
        }
        (None, (Some(source), Some(target))) => Json::object(vec![(
            "same_model",
            Json::Bool(identity::same_model(provider, source, target)),
        )]),
        (None, _) => return Err(usage("provide --source and --target, or --mappings")),
    };
    print_json(output, &result, None)
}

/// Bytes of an image file; links and special files are refused.
fn read_image(path: &str) -> Result<Vec<u8>, String> {
    let (file, metadata) =
        open_regular_file(Path::new(path)).map_err(|error| path_failure(path, &error))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "cannot read {}: larger than {MAX_IMAGE_BYTES} bytes",
            safe_path(path)
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", safe_path(path)))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IMAGE_BYTES {
        return Err(format!(
            "cannot read {}: larger than {MAX_IMAGE_BYTES} bytes",
            safe_path(path)
        ));
    }
    Ok(bytes)
}

fn prompt_text(invocation: &Invocation) -> Result<String, ToolkitError> {
    match (
        invocation.value("--prompt"),
        invocation.value("--prompt-file"),
    ) {
        (Some(_), Some(_)) => Err(usage("--prompt and --prompt-file are exclusive")),
        (Some(text), None) => Ok(text.to_string()),
        (None, Some(path)) => read_regular_file(Path::new(path), MAX_TEXT_FILE_BYTES)
            .map_err(|error| failure("prompt_unreadable", path_failure(path, &error))),
        (None, None) => Err(usage("--prompt or --prompt-file is required for a message")),
    }
}

const EMIT_KINDS: [&str; 6] = [
    "format",
    "mime",
    "data-url",
    "converse",
    "responses",
    "chat",
];

/// `toolkit bedrock-resolve image`.
pub(crate) fn image_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if !invocation.positionals.is_empty() {
        return Err(usage("image takes no positional argument"));
    }
    let emit = required(invocation, "--emit")?;
    if !EMIT_KINDS.contains(&emit) {
        return Err(usage(&format!(
            "--emit must be one of: {}",
            EMIT_KINDS.join(", ")
        )));
    }
    let message = matches!(emit, "converse" | "responses" | "chat");
    if invocation.value("--detail").is_some() && emit != "responses" {
        return Err(usage("--detail applies to --emit responses only"));
    }
    if !message
        && (invocation.value("--prompt").is_some() || invocation.value("--prompt-file").is_some())
    {
        return Err(usage(
            "--prompt and --prompt-file apply to a message --emit only",
        ));
    }
    let image_path = invocation.value("--image");
    let bad = |text: String| failure("image_input", text);
    let result = match emit {
        "format" | "mime" | "data-url" => {
            let path = image_path.ok_or_else(|| usage("--image is required"))?;
            match emit {
                "format" => Json::object(vec![(
                    "format",
                    Json::string(image::converse_format(path).map_err(bad)?),
                )]),
                "mime" => Json::object(vec![(
                    "mime_type",
                    Json::string(image::mime_type(path).map_err(bad)?),
                )]),
                _ => {
                    // The extension is checked before the file is read.
                    image::converse_format(path).map_err(bad)?;
                    let raw = read_image(path).map_err(|text| failure("image_unreadable", text))?;
                    Json::object(vec![(
                        "data_url",
                        Json::string(image::data_url(path, &raw).map_err(bad)?),
                    )])
                }
            }
        }
        _ => {
            let prompt = prompt_text(invocation)?;
            let raw = match image_path {
                None => None,
                Some(path) => {
                    image::converse_format(path).map_err(bad)?;
                    Some(read_image(path).map_err(|text| failure("image_unreadable", text))?)
                }
            };
            let raw = raw.as_deref();
            match emit {
                "converse" => image::converse_message(&prompt, image_path, raw),
                "responses" => image::responses_message(
                    &prompt,
                    image_path,
                    raw,
                    invocation
                        .value("--detail")
                        .unwrap_or(image::DEFAULT_DETAIL),
                ),
                _ => image::chat_message(&prompt, image_path, raw),
            }
            .map_err(bad)?
        }
    };
    print_json(output, &result, Some(2))
}

/// `toolkit bedrock-resolve pricing`.
pub(crate) fn pricing_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if !invocation.positionals.is_empty() {
        return Err(usage("pricing takes no positional argument"));
    }
    let region = required(invocation, "--region")?;
    let models = required(invocation, "--models")?;
    let price_list = match invocation.value("--price-list") {
        None => None,
        Some(path) => {
            let document = read_json_file(path, MAX_RECORDED_BYTES)
                .map_err(|message| failure("price_list_unreadable", message))?;
            Some(
                pricing::PriceList::from_json(&document)
                    .map_err(|message| failure("price_list_invalid", message))?,
            )
        }
    };
    // `{m.strip(): lookup(...) for m in models.split(",") if m.strip()}`: a
    // repeated id keeps its first position. Ids are shown escaped, so two
    // different ids must not collapse into one shown name.
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut entries: Vec<(String, Json)> = Vec::new();
    for raw in models.split(',') {
        let id = identity::strip(raw);
        if id.is_empty() || seen.iter().any(|(known, _)| known == id) {
            continue;
        }
        if seen.len() >= MAX_MODELS {
            return Err(usage(&format!(
                "--models takes at most {MAX_MODELS} model ids"
            )));
        }
        let name = safe_text(id);
        if seen.iter().any(|(_, shown)| *shown == name) {
            return Err(usage(
                "two model ids are indistinguishable once escaped for display",
            ));
        }
        entries.push((
            name.clone(),
            pricing::lookup(region, id, price_list.as_ref()).to_json(),
        ));
        seen.push((id.to_string(), name));
    }
    print_json(output, &Json::Object(entries), Some(2))
}

/// Replaces every string of `value` (object keys are fixed names) by its
/// display form.
fn shown_json(value: &Json, secrets: &[String]) -> Json {
    match value {
        Json::String(text) => Json::string(source::shown(text, secrets)),
        Json::Array(items) => {
            Json::Array(items.iter().map(|item| shown_json(item, secrets)).collect())
        }
        Json::Object(members) => Json::Object(
            members
                .iter()
                .map(|(key, member)| (key.clone(), shown_json(member, secrets)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `{"status": "error", "detail": ...}` and exit status 3.
fn source_error(
    output: &mut Output<'_>,
    detail: &str,
    secrets: &[String],
) -> Result<(), ToolkitError> {
    let value = Json::object(vec![
        ("status", Json::string("error")),
        (
            "detail",
            Json::string(source::shown_unbounded(detail, secrets)),
        ),
    ]);
    print_json(output, &value, None)?;
    output.set_exit_status(3);
    Ok(())
}

/// `toolkit bedrock-resolve source-model`.
pub(crate) fn source_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let env_path = match invocation.positionals.as_slice() {
        [path] => path.as_str(),
        _ => return Err(usage("source-model takes exactly one ENV_FILE argument")),
    };
    let text = read_regular_file(Path::new(env_path), MAX_TEXT_FILE_BYTES)
        .map_err(|error| failure("env_file_unreadable", path_failure(env_path, &error)))?;
    let pairs = source::parse_env_file(&universal_newlines(&text));
    let plan_id = match invocation.value("--plan-model-id") {
        Some(id) => id.to_string(),
        None => std::env::var("PLAN_MODEL_ID")
            .map_err(|_| usage("PLAN_MODEL_ID is not set (or pass --plan-model-id)"))?,
    };
    let stated = invocation
        .value("--source-provider")
        .map(str::to_string)
        .or_else(|| std::env::var("SOURCE_PROVIDER").ok())
        .unwrap_or_default();
    let Some(provider) = source::pick_provider(&pairs, &stated) else {
        print_json(
            output,
            &Json::object(vec![("status", Json::string("no_key"))]),
            None,
        )?;
        output.set_exit_status(2);
        return Ok(());
    };
    let secrets: Vec<String> = pairs
        .iter()
        .filter(|(key, _)| source::PROVIDER_KEYS.contains(&key.as_str()))
        .map(|(_, value)| value.clone())
        .collect();
    let Some(catalog_path) = invocation.value("--catalog") else {
        return source_error(
            output,
            "no recorded catalog was given (pass --catalog FILE); this build has no live provider client",
            &secrets,
        );
    };
    let catalog = read_json_file(catalog_path, MAX_RECORDED_BYTES)
        .and_then(|document| source::catalog_ids(provider, &document));
    let catalog = match catalog {
        Ok(ids) => ids,
        Err(detail) => return source_error(output, &detail, &secrets),
    };
    let decision = source::resolve(&catalog, &plan_id);
    // A resolved id that cannot be shown as it is (control characters, a
    // secret, over-long) must not be offered as the pinned model.
    if let Some(Json::String(id)) = decision.get("resolved_id") {
        if source::shown(id, &secrets) != *id {
            return source_error(
                output,
                "the resolved model id cannot be shown safely",
                &secrets,
            );
        }
    }
    print_json(output, &shown_json(&decision, &secrets), None)
}
