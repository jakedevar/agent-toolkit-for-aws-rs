//! The conservative same-model rule `validate_result.py` applies to an
//! analysis result (`model_identity.same_model`).
//!
//! `model_identity.py` itself is slice #4403; until it lands this private copy
//! is the only user. Once #4403 ports the module, `same_model` should move
//! there and this file go away.

use std::sync::LazyLock;

use crate::startup::text::is_space;

static DECIMAL: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r"\A\p{Nd}\z").expect("a valid category class"));

/// Python `\d` for `str`: a character of the Unicode category `Nd`.
pub(super) fn is_decimal(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    let mut buffer = [0_u8; 4];
    DECIMAL
        .is_match(c.encode_utf8(&mut buffer))
        .unwrap_or(false)
}

/// Python `str.strip()`: removes leading and trailing whitespace.
pub(super) fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

const REGION_PREFIXES: [&str; 7] = ["us", "eu", "au", "jp", "apac", "in", "global"];

fn without_region(value: &str) -> &str {
    for region in REGION_PREFIXES {
        if let Some(rest) = value
            .strip_prefix(region)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            return rest;
        }
    }
    value
}

/// `-v\d+(?::\d+)?$` removed from the end of `value`.
fn without_revision(value: &str) -> &str {
    for (index, _) in value.match_indices("-v") {
        if is_revision(&value[index + 2..]) {
            return &value[..index];
        }
    }
    value
}

fn is_revision(rest: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.chars().all(is_decimal);
    match rest.split_once(':') {
        Some((major, minor)) => digits(major) && digits(minor),
        None => digits(rest),
    }
}

/// The comparable identity of a model ID, or `None` when it is a moving alias,
/// an opaque profile or not a first-party model of `provider`.
fn identity(model_id: &str, provider: &str, target: bool) -> Option<String> {
    let lowered = strip(model_id).to_lowercase();
    if lowered.is_empty()
        || lowered.starts_with("arn:")
        || lowered.contains("latest")
        || lowered.contains('*')
    {
        return None;
    }
    let mut value = without_region(&lowered);
    let prefix = format!("{provider}.");
    if target && !value.starts_with(&prefix) {
        return None;
    }
    value = value.strip_prefix(prefix.as_str()).unwrap_or(value);
    if target {
        // A Bedrock transport revision is not a different checkpoint; release
        // dates and tier suffixes are kept because they may be.
        value = without_revision(value);
    }
    let allowed = match provider {
        "anthropic" => value.starts_with("claude-"),
        "openai" => value.starts_with("gpt-") && !value.starts_with("gpt-oss"),
        _ => true,
    };
    allowed.then(|| value.to_string())
}

/// Whether `source_model` and `target_model` name the same model of `provider`
/// (only `anthropic` and `openai` are known; anything else is never the same).
pub(super) fn same_model(provider: &str, source_model: &str, target_model: &str) -> bool {
    if !matches!(provider, "anthropic" | "openai") {
        return false;
    }
    matches!(
        (
            identity(source_model, provider, false),
            identity(target_model, provider, true),
        ),
        (Some(source), Some(target)) if source == target
    )
}
