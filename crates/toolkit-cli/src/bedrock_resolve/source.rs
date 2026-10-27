//! Source model resolution (`resolve_source_model.py`).
//!
//! Hard rule, model line is sacred: auto-resolution may only pin the SAME
//! model line to a date (`YYYY-MM-DD`) or a pure version suffix. Any alphabetic
//! suffix (mini, pro, turbo, latest, codex, ...) is a different line or a
//! moving alias and is surfaced instead of picked.
//!
//! The Python script listed the provider's live catalog with the API key from
//! the env file. This build never reads a key for a network call: the catalog
//! is a recorded listing (`--catalog FILE`), the key values are only used to
//! redact output. Anything unusable about the catalog is the script's
//! `{"status": "error"}` answer (exit 3), never a guess.

use toolkit_core::Json;

use super::identity::strip;
use crate::safe_text::{safe_path, safe_text};

/// The keys of the supported providers, in the script's fallback order.
pub(crate) const PROVIDER_KEYS: [&str; 3] =
    ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"];

/// Most model ids a catalog may hold.
pub(crate) const MAX_CATALOG_IDS: usize = 100_000;

/// `PROVIDER_TO_KEY`.
fn key_for(stated: &str) -> Option<&'static str> {
    match stated {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "google" | "gemini" => Some("GEMINI_API_KEY"),
        _ => None,
    }
}

/// The `KEY=VALUE` pairs of an env file: lines are stripped, a line without
/// `=` is skipped, the first `=` splits and a later duplicate wins.
pub(crate) fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = strip(line);
        if let Some((key, value)) = line.split_once('=').filter(|_| !line.is_empty()) {
            match pairs.iter_mut().find(|(name, _)| name == key) {
                Some(slot) => slot.1 = value.to_string(),
                None => pairs.push((key.to_string(), value.to_string())),
            }
        }
    }
    pairs
}

/// The provider key name. `stated` (`SOURCE_PROVIDER`, lower-cased) is
/// authoritative when set: a file carrying several provider keys must not fall
/// back to order guessing. Without it the first recognised key IN THE FILE
/// wins, never an ambient environment variable.
pub(crate) fn pick_provider(pairs: &[(String, String)], stated: &str) -> Option<&'static str> {
    let in_file = |key: &str| pairs.iter().any(|(name, _)| name == key);
    if !stated.is_empty() {
        return key_for(&stated.to_lowercase()).filter(|key| in_file(key));
    }
    PROVIDER_KEYS.into_iter().find(|key| in_file(key))
}

/// Every textual form a secret can take in text: itself, its
/// `unicode_escape` spelling (a raw CR/LF is rendered escaped in exception
/// messages) and the fragments of at least six characters between control
/// characters. Longest first.
fn secret_variants(secrets: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        out.push(secret.clone());
        let escaped = unicode_escape(secret);
        if escaped != *secret {
            out.push(escaped);
        }
        for fragment in secret.split(|c: char| c <= '\u{1f}') {
            if fragment.chars().count() >= 6 && !out.iter().any(|known| known == fragment) {
                out.push(fragment.to_string());
            }
        }
    }
    out.sort_by_key(|variant| std::cmp::Reverse(variant.chars().count()));
    out
}

/// Python `s.encode("unicode_escape").decode("ascii")`.
fn unicode_escape(text: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let code = u32::from(c);
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ' '..='~' => out.push(c),
            _ if code <= 0xff => {
                let _ = write!(out, "\\x{code:02x}");
            }
            _ if code <= 0xffff => {
                let _ = write!(out, "\\u{code:04x}");
            }
            _ => {
                let _ = write!(out, "\\U{code:08x}");
            }
        }
    }
    out
}

/// `redact`: secret values replaced by `***`.
pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for variant in secret_variants(secrets) {
        out = out.replace(&variant, "***");
    }
    out
}

/// [`shown`] without the length bound, for a diagnostic whose reason must stay
/// visible after a long path.
pub(crate) fn shown_unbounded(text: &str, secrets: &[String]) -> String {
    redact(&safe_path(&redact(text, secrets)), secrets)
}

/// Text made safe for output: secrets redacted first (on the raw text, so no
/// escape can hide one), then escaped and bounded by `safe_text`.
pub(crate) fn shown(text: &str, secrets: &[String]) -> String {
    redact(&safe_text(&redact(text, secrets)), secrets)
}

/// Whether `catalog_id` is the plan id itself or the same line pinned by a
/// date (`YYYY-MM-DD`) or a pure version suffix (`3`, `3.1`).
///
/// Stricter than the script in two ways (recorded in
/// `docs/migration/bedrock-resolve.md`): the suffix must end exactly (the
/// Python `$` also accepted a trailing newline) and digits are ASCII.
pub(crate) fn safe_variant(catalog_id: &str, plan_id: &str) -> bool {
    if catalog_id == plan_id {
        return true;
    }
    // `catalog_id[len(plan_id) + 1:]`, counted in characters.
    let skip = plan_id.chars().count() + 1;
    let suffix: String = catalog_id.chars().skip(skip).collect();
    is_date(&suffix) || is_version(&suffix)
}

fn is_date(suffix: &str) -> bool {
    let parts: Vec<&str> = suffix.split('-').collect();
    matches!(parts.as_slice(), [year, month, day]
        if [(year, 4), (month, 2), (day, 2)]
            .iter()
            .all(|(part, width)| part.len() == *width && part.bytes().all(|b| b.is_ascii_digit())))
}

fn is_version(suffix: &str) -> bool {
    let mut bytes = suffix.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_digit() || b == b'.')
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// The decision, as the script's JSON answer. Pure: no network.
pub(crate) fn resolve(catalog: &[String], plan_id: &str) -> Json {
    let text_list = |items: &[&String]| {
        Json::Array(
            items
                .iter()
                .map(|item| Json::string((*item).clone()))
                .collect(),
        )
    };
    // A catalog that lists an id twice names one model, not two candidates
    // (#4685); the first occurrence wins, so the order is otherwise kept.
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<String> = catalog
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect();
    let catalog = unique.as_slice();
    if catalog.iter().any(|id| id == plan_id) {
        return Json::object(vec![
            ("status", Json::string("exact")),
            ("resolved_id", Json::string(plan_id)),
        ]);
    }
    let dashed = format!("{plan_id}-");
    let prefix_hits: Vec<&String> = catalog
        .iter()
        .filter(|id| id.as_str() == plan_id || id.starts_with(&dashed))
        .collect();
    // A bare prefix match is NOT enough: `gpt-4o-mini` and
    // `claude-3-5-sonnet-latest` start with a plausible plan id but are
    // different lines or moving aliases.
    let mut safe_hits: Vec<&String> = prefix_hits
        .iter()
        .copied()
        .filter(|id| safe_variant(id, plan_id))
        .collect();
    // Shortest first; equal lengths in id order, so the answer does not
    // depend on the order the catalog listed them in.
    safe_hits.sort_by(|a, b| {
        a.chars()
            .count()
            .cmp(&b.chars().count())
            .then_with(|| a.cmp(b))
    });
    // Distinct candidates tied under the supported rule (two dated snapshots
    // of the same length): no deterministic pick exists, so none is made
    // (#4685). `all_hits` keeps the tie visible.
    if let [first, second, ..] = safe_hits.as_slice() {
        if first.chars().count() == second.chars().count() {
            return Json::object(vec![
                ("status", Json::string("not_found")),
                (
                    "candidates",
                    text_list(&safe_hits[..safe_hits.len().min(5)]),
                ),
                ("ambiguous_prefix", Json::Bool(true)),
                ("all_hits", text_list(&safe_hits)),
            ]);
        }
    }
    if let Some(first) = safe_hits.first() {
        return Json::object(vec![
            ("status", Json::string("prefix")),
            ("resolved_id", Json::string((*first).clone())),
            ("all_hits", text_list(&safe_hits)),
        ]);
    }
    if !prefix_hits.is_empty() {
        return Json::object(vec![
            ("status", Json::string("not_found")),
            (
                "candidates",
                text_list(&prefix_hits[..prefix_hits.len().min(5)]),
            ),
            ("ambiguous_prefix", Json::Bool(true)),
        ]);
    }
    let mut ranked: Vec<&String> = catalog.iter().collect();
    ranked.sort_by_key(|id| std::cmp::Reverse(common_prefix(id, plan_id)));
    Json::object(vec![
        ("status", Json::string("not_found")),
        ("candidates", text_list(&ranked[..ranked.len().min(5)])),
    ])
}

/// Python truthiness of a JSON value (`data.get("has_more")`).
fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null | Json::Bool(false)) => false,
        Some(Json::Int(number)) => *number != 0,
        Some(Json::Float(number)) => *number != 0.0,
        Some(Json::String(text)) => !text.is_empty(),
        Some(Json::Array(items)) => !items.is_empty(),
        Some(Json::Object(members)) => !members.is_empty(),
        Some(Json::Bool(true)) => true,
    }
}

fn page_list<'a>(page: &'a Json, key: &str, required: bool) -> Result<&'a [Json], String> {
    match page.get(key) {
        Some(Json::Array(items)) => Ok(items),
        None if !required => Ok(&[]),
        _ => Err(format!("a catalog page has no `{key}` list")),
    }
}

fn text_member(item: &Json, key: &str) -> Result<String, String> {
    item.get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("a catalog entry has no text `{key}`"))
}

/// The model ids of a recorded listing for the provider `key`.
///
/// The document is either a list of ids (a flattened catalog) or the
/// provider's own response pages in listing order, as the live client would
/// have read them: `OpenAI` one `{"data": [{"id"}]}` page; Anthropic pages that
/// continue while `has_more`; Gemini pages that continue while
/// `nextPageToken` is set (a `models/` prefix is stripped from each name).
///
/// # Errors
///
/// A document that does not match, a listing that stops before its last page
/// or continues past it, or more than [`MAX_CATALOG_IDS`] ids. A catalog that
/// might be incomplete is never used: a model past the missing page would be
/// reported `not_found`.
pub(crate) fn catalog_ids(provider_key: &str, document: &Json) -> Result<Vec<String>, String> {
    let pages: Vec<&Json> = match document {
        Json::Array(items) if items.iter().all(|item| matches!(item, Json::String(_))) => {
            let ids: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect();
            return if ids.len() > MAX_CATALOG_IDS {
                Err("the catalog holds too many model ids".to_string())
            } else {
                Ok(ids)
            };
        }
        Json::Array(items) => items.iter().collect(),
        object @ Json::Object(_) => vec![object],
        _ => return Err("the catalog must be a list of ids or of response pages".to_string()),
    };
    let mut ids: Vec<String> = Vec::new();
    let mut pages = pages.into_iter();
    loop {
        let Some(page) = pages.next() else {
            return Err("the catalog ends before the last page of the listing".to_string());
        };
        let more = match provider_key {
            "OPENAI_API_KEY" => {
                for item in page_list(page, "data", true)? {
                    ids.push(text_member(item, "id")?);
                }
                false
            }
            "ANTHROPIC_API_KEY" => {
                for item in page_list(page, "data", true)? {
                    ids.push(text_member(item, "id")?);
                }
                truthy(page.get("has_more"))
            }
            _ => {
                for item in page_list(page, "models", false)? {
                    let name = text_member(item, "name")?;
                    ids.push(
                        name.split_once('/')
                            .map_or(name.as_str(), |(_, rest)| rest)
                            .to_string(),
                    );
                }
                truthy(page.get("nextPageToken"))
            }
        };
        if ids.len() > MAX_CATALOG_IDS {
            return Err("the catalog holds too many model ids".to_string());
        }
        if !more {
            break;
        }
    }
    if pages.next().is_some() {
        return Err("the catalog continues past the last page of the listing".to_string());
    }
    Ok(ids)
}
