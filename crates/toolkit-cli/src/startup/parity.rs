//! `toolkit startup pricing-parity`: proves the Bedrock pricing split kept the
//! pre-split data (port of `check-bedrock-pricing-parity.py`).
//!
//! The immutable comparison point is the pre-split commit [`BASE_SHA`]; the
//! pre-split files are read with `git show`. The checks compare the canonical
//! Bedrock pricing cache and the remaining GCP/Azure caches against that
//! commit (sections, metadata, embedding rows, lookup keys, representative
//! estimates), require the vendored copies to be byte-identical to the
//! canonical cache and require the neutral consumers to stop naming the GCP
//! cache.
//!
//! One step of the Python script is NOT ported: it executed the old and the
//! new `bedrock_pricing.py` through `exec` and compared the behaviour of their
//! parser and lookup functions. That needs a Python runtime and is covered
//! when the `llm-to-bedrock` slice replaces the module; the report says so
//! (`pricing_behavior`). See `docs/migration/startup-validators.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use toolkit_core::{read_regular_file, Json, ToolkitError};

use super::artifacts::pyjson::dumps;
use super::MAX_ARTIFACT_BYTES;
use crate::manifests::{
    is_py_space, py_splitlines, py_strip, read_failure, resolve_lenient, universal_newlines,
};
use crate::{Invocation, Output};

/// The pre-split comparison commit (upstream's 2026-10-05 pricing-cache
/// refresh).
pub(crate) const BASE_SHA: &str = "883270642209b06e1e76dcc4f944a1174abf0675";

const OLD_CACHE: &str =
    "plugins/aws-startup-advisor/skills/gcp-to-aws/references/shared/pricing-cache.md";
const OLD_SKILL: &str = "plugins/aws-startup-advisor/skills/gcp-to-aws/SKILL.md";
const OLD_AZURE_CACHE: &str =
    "plugins/aws-startup-advisor/skills/azure-to-aws/references/shared/pricing-cache.md";

const CANONICAL: &str = "skills/shared/ai/bedrock-pricing-cache.md";
const GCP_CACHE: &str = "skills/gcp-to-aws/references/shared/pricing-cache.md";
const GCP_SKILL: &str = "skills/gcp-to-aws/SKILL.md";
const GCP_ESTIMATE: &str = "skills/gcp-to-aws/references/phases/estimate/estimate.md";
const GCP_ESTIMATE_AI: &str = "skills/gcp-to-aws/references/phases/estimate/estimate-ai.md";
const AZURE_CACHE: &str = "skills/azure-to-aws/references/shared/pricing-cache.md";
const AZURE_ESTIMATE: &str = "skills/azure-to-aws/references/phases/estimate/estimate.md";
const AZURE_ESTIMATE_AI: &str = "skills/azure-to-aws/references/phases/estimate/estimate-ai.md";
const AZURE_OPENAI: &str = "skills/azure-to-aws/references/shared/openai-on-bedrock.md";
const VENDORED: [&str; 3] = [
    "skills/gcp-to-aws/references/vendored/ai/bedrock-pricing-cache.md",
    "skills/azure-to-aws/references/vendored/ai/bedrock-pricing-cache.md",
    "skills/agent-advisor/references/vendored/ai/bedrock-pricing-cache.md",
];
const NEUTRAL_CONSUMERS: [&str; 4] = [
    "skills/llm-to-bedrock/scripts/bedrock_pricing.py",
    "skills/llm-to-bedrock/scripts/test_bedrock_pricing.py",
    "skills/agent-advisor/references/phases/estimate/estimate.md",
    "skills/agent-advisor/references/decision-refs/freshness.md",
];

type Check<T> = Result<T, String>;

/// A failed assertion with the Python message.
fn ensure(condition: bool, message: &str) -> Check<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_string())
    }
}

/// `text.rstrip()` with Python's whitespace.
fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_py_space)
}

/// The end of the line that starts at `start`.
fn line_end(text: &str, start: usize) -> usize {
    text[start..].find('\n').map_or(text.len(), |n| start + n)
}

/// `(?m)^<prefix>.*$` searched in `text`: the first line that starts with
/// `prefix`, as `(start, end)` byte offsets of the line.
fn find_line(text: &str, prefix: &str) -> Option<(usize, usize)> {
    let mut start = 0;
    loop {
        if text[start..].starts_with(prefix) {
            return Some((start, line_end(text, start)));
        }
        start = line_end(text, start) + 1;
        if start > text.len() {
            return None;
        }
    }
}

/// `(?m)^(?:<marker>)` searched in `text`: the offset of the first line that
/// starts with any of `markers` (the pattern may also match at offset 0).
fn find_marker(text: &str, markers: &[&str]) -> Option<usize> {
    let mut start = 0;
    loop {
        if markers
            .iter()
            .any(|marker| text[start..].starts_with(marker))
        {
            return Some(start);
        }
        start = line_end(text, start) + 1;
        if start > text.len() {
            return None;
        }
    }
}

/// The text from the line that starts with `prefix` up to the next line that
/// starts with one of `stops`, or the end of the text.
fn block<'a>(text: &'a str, prefix: &str, stops: &[&str]) -> Option<&'a str> {
    let (start, end) = find_line(text, prefix)?;
    let stop = find_marker(&text[end..], stops).map_or(text.len(), |offset| end + offset);
    Some(&text[start..stop])
}

/// A `## ` section up to the next `## ` heading.
pub(crate) fn section<'a>(text: &'a str, heading: &str) -> Check<&'a str> {
    block(text, &format!("## {heading}"), &["## "])
        .ok_or_else(|| format!("missing section: {heading}"))
}

/// A `### ` subsection up to the next `##` or `###` heading.
pub(crate) fn subsection<'a>(text: &'a str, heading: &str) -> Check<&'a str> {
    block(text, &format!("### {heading}"), &["## ", "### "])
        .ok_or_else(|| format!("missing subsection: {heading}"))
}

/// `(?m)^\*\*<key>:\*\*\s*(.+)$` searched in `text`: the first line that
/// carries the field. The value is what the regular expression captures,
/// including its backtracking over whitespace.
fn metadata_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("**{key}:**");
    let mut start = 0;
    loop {
        if let Some(rest) = text[start..].strip_prefix(prefix.as_str()) {
            let from = start + prefix.len();
            let spaces = rest.len() - rest.trim_start_matches(char::is_whitespace).len();
            let after_spaces = from + spaces;
            // `\s*` is greedy and may be given back one character at a time.
            let candidates: Vec<usize> = text[from..after_spaces]
                .char_indices()
                .map(|(offset, _)| from + offset)
                .chain([after_spaces])
                .collect();
            for value_start in candidates.into_iter().rev() {
                let end = line_end(text, value_start);
                if end > value_start && !text[value_start..].starts_with('\n') {
                    return Some(text[value_start..end].to_string());
                }
            }
        }
        start = line_end(text, start) + 1;
        if start > text.len() {
            return None;
        }
    }
}

fn metadata(text: &str) -> Check<BTreeMap<&'static str, String>> {
    let mut fields = BTreeMap::new();
    for key in ["Last updated", "Region", "Currency", "Accuracy"] {
        let value = metadata_field(text, key).ok_or_else(|| format!("missing metadata: {key}"))?;
        fields.insert(key, value);
    }
    Ok(fields)
}

/// `\d+(?:\.\d+)?` matching the whole of `text`.
fn is_decimal(text: &str) -> bool {
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text, None), |(a, b)| (a, Some(b)));
    let digits = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    digits(whole) && fraction.is_none_or(digits)
}

/// Model lookup keys with their input and output prices, read from the
/// Bedrock models section without its embeddings subsection (an input-only
/// table of another shape).
pub(crate) fn model_rows(text: &str) -> Check<BTreeMap<String, (String, String)>> {
    let mut bedrock = section(text, "Bedrock Models (On-Demand)")?.to_string();
    if let Some((start, end)) = find_line(&bedrock, "### Embeddings — Bedrock") {
        let stop = find_marker(&bedrock[end..], &["### "]).map_or(bedrock.len(), |n| end + n);
        bedrock = format!("{}{}", &bedrock[..start], &bedrock[stop..]);
    }
    let mut rows = BTreeMap::new();
    for line in py_splitlines(&bedrock) {
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = py_strip(line)
            .trim_matches('|')
            .split('|')
            .map(py_strip)
            .collect();
        if cells.len() < 5 || !cells[1].contains('.') {
            continue;
        }
        if is_decimal(cells[3]) && is_decimal(cells[4]) {
            rows.insert(
                cells[1].to_string(),
                (cells[3].to_string(), cells[4].to_string()),
            );
        }
    }
    Ok(rows)
}

/// Table data rows without separators and headers.
pub(crate) fn markdown_data_rows(text: &str) -> Vec<String> {
    let is_separator = |line: &str| {
        line.strip_prefix('|').is_some_and(|rest| {
            let run = rest
                .chars()
                .take_while(|c| is_py_space(*c) || *c == ':' || *c == '-')
                .count();
            run > 0 && rest.chars().nth(run) == Some('|')
        })
    };
    py_splitlines(text)
        .into_iter()
        .filter(|line| {
            line.starts_with('|') && !is_separator(line) && !line.starts_with("| Model ")
        })
        .map(str::to_string)
        .collect()
}

/// `re.findall(r"±\d+-\d+%", line)`.
fn accuracy_bands_in(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let digits = |from: usize| {
        let mut end = from;
        while chars.get(end).is_some_and(char::is_ascii_digit) {
            end += 1;
        }
        end
    };
    let mut bands = Vec::new();
    let mut pos = 0;
    while pos < chars.len() {
        if chars[pos] == '±' {
            let first = digits(pos + 1);
            if first > pos + 1 && chars.get(first) == Some(&'-') {
                let second = digits(first + 1);
                if second > first + 1 && chars.get(second) == Some(&'%') {
                    bands.push(chars[pos..=second].iter().collect());
                    pos = second + 1;
                    continue;
                }
            }
        }
        pos += 1;
    }
    bands
}

/// The infrastructure and AI pricing accuracy bands of the skill contract.
pub(crate) fn pricing_accuracy_bands(text: &str) -> Check<(String, String)> {
    let line = py_splitlines(text)
        .into_iter()
        .find(|line| line.starts_with("- Primary pricing source"))
        .ok_or("missing primary pricing source contract")?;
    let mut bands = accuracy_bands_in(line);
    ensure(
        bands.len() == 2,
        "expected infrastructure and AI pricing accuracy bands",
    )?;
    let second = bands.pop().unwrap_or_default();
    let first = bands.pop().unwrap_or_default();
    Ok((first, second))
}

/// Removes the reinstated embeddings subsection, which is the one reviewed
/// addition to the canonical Bedrock section:
/// `(?s)### Embeddings — Bedrock \(per 1M input tokens, US East\).*?(?=### Stability AI)`.
pub(crate) fn strip_embeddings(text: &str) -> String {
    const START: &str = "### Embeddings — Bedrock (per 1M input tokens, US East)";
    const STOP: &str = "### Stability AI";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(START) {
        let after = start + START.len();
        let Some(stop) = rest[after..].find(STOP) else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[after + stop..];
    }
    out.push_str(rest);
    out
}

fn read_text(path: &Path) -> Check<String> {
    read_regular_file(path, MAX_ARTIFACT_BYTES)
        .map(|text| universal_newlines(&text))
        .map_err(|error| format!("cannot read {}: {}", path.display(), read_failure(&error)))
}

/// `git show <BASE_SHA>:<path>` in `repo`.
fn git_show(repo: &Path, path: &str) -> Check<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{BASE_SHA}:{path}"))
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git show {BASE_SHA}:{path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|text| universal_newlines(&text))
        .map_err(|_| format!("git show {BASE_SHA}:{path}: not valid UTF-8"))
}

/// The pre-split and current texts the checks compare.
struct Texts {
    old: String,
    canonical: String,
    remaining: String,
    azure_old: String,
    azure_cache: String,
}

/// Bedrock section, metadata and the non-Bedrock sections of the GCP cache.
fn check_gcp_cache(texts: &Texts) -> Check<()> {
    let old_bedrock = section(&texts.old, "Bedrock Models (On-Demand)")?;
    let new_bedrock = section(&texts.canonical, "Bedrock Models (On-Demand)")?;
    // The reinstated embeddings subsection is the one reviewed addition.
    let without_embeddings = strip_embeddings(new_bedrock);
    ensure(
        rstrip(&without_embeddings) == rstrip(old_bedrock),
        "Bedrock section changed during extraction",
    )?;
    let old_metadata = metadata(&texts.old)?;
    let canonical_metadata = metadata(&texts.canonical)?;
    for field in ["Last updated", "Region", "Currency"] {
        ensure(
            canonical_metadata[field] == old_metadata[field],
            &format!("Bedrock {field} metadata changed"),
        )?;
    }
    let accuracy = &canonical_metadata["Accuracy"];
    ensure(
        accuracy.starts_with("±15-25% for AI/Bedrock"),
        "assertion failed: the canonical Accuracy starts with ±15-25% for AI/Bedrock",
    )?;
    ensure(
        !accuracy.contains("±5-10%"),
        "assertion failed: the canonical Accuracy has no ±5-10%",
    )?;
    ensure(
        !texts.canonical.contains("other services in this file"),
        "assertion failed: the canonical cache mentions other services in this file",
    )?;
    ensure(
        !accuracy.contains("infrastructure services"),
        "assertion failed: the canonical Accuracy names infrastructure services",
    )?;
    check_non_bedrock_sections(texts)
}

/// The sections that stay in the GCP cache are unchanged; the Bedrock section
/// moved out and the cache points at the vendored copy.
fn check_non_bedrock_sections(texts: &Texts) -> Check<()> {
    for heading in [
        "Compute",
        "Database",
        "Storage",
        "Networking",
        "Supporting Services",
        "Analytics",
        "Source Provider Pricing (for Migration Comparison)",
        "Security Baseline",
    ] {
        ensure(
            section(&texts.remaining, heading)? == section(&texts.old, heading)?,
            &format!("non-Bedrock section changed: {heading}"),
        )?;
    }
    let remaining = &texts.remaining;
    ensure(
        !remaining.contains("## Bedrock Models (On-Demand)"),
        "assertion failed: the GCP cache still has the Bedrock section",
    )?;
    ensure(
        remaining.contains("references/vendored/ai/bedrock-pricing-cache.md"),
        "assertion failed: the GCP cache does not point at the vendored Bedrock cache",
    )?;
    let remaining_accuracy = metadata(remaining)?["Accuracy"].clone();
    ensure(
        remaining_accuracy.starts_with("±5-10%"),
        "assertion failed: the GCP cache Accuracy starts with ±5-10%",
    )?;
    ensure(
        !remaining_accuracy.contains("±15-25%"),
        "assertion failed: the GCP cache Accuracy has no ±15-25%",
    )?;
    ensure(
        !remaining.contains("Bedrock subsection"),
        "assertion failed: the GCP cache mentions a Bedrock subsection",
    )?;
    let compute = remaining
        .find("## Compute")
        .ok_or("substring not found: ## Compute")?;
    ensure(
        !remaining[..compute].contains("Amazon Nova"),
        "assertion failed: Amazon Nova precedes the Compute section",
    )
}

/// Estimate documents, the Azure cache and the Azure embeddings table.
fn check_references(plugin: &Path, texts: &Texts) -> Check<()> {
    let gcp_estimate = read_text(&plugin.join(GCP_ESTIMATE))?;
    let gcp_estimate_ai = read_text(&plugin.join(GCP_ESTIMATE_AI))?;
    let azure_estimate = read_text(&plugin.join(AZURE_ESTIMATE))?;
    let azure_estimate_ai = read_text(&plugin.join(AZURE_ESTIMATE_AI))?;
    let azure_openai = read_text(&plugin.join(AZURE_OPENAI))?;
    let azure_cache = &texts.azure_cache;

    ensure(
        !gcp_estimate.contains("±5-25%"),
        "assertion failed: the GCP estimate still names ±5-25%",
    )?;
    ensure(
        gcp_estimate.contains("infrastructure/source-provider cache (updated [date], ±5-10%)"),
        "assertion failed: the GCP estimate does not name the infrastructure cache band",
    )?;
    ensure(
        gcp_estimate.contains("Bedrock cache (updated [date], ±15-25%)"),
        "assertion failed: the GCP estimate does not name the Bedrock cache band",
    )?;
    ensure(
        gcp_estimate_ai.contains(
            "a `references/vendored/ai/bedrock-pricing-cache.md` cell marked `_unverified_`",
        ),
        "assertion failed: the GCP AI estimate does not name the vendored cache",
    )?;
    ensure(
        azure_estimate.contains("references/vendored/ai/bedrock-pricing-cache.md"),
        "assertion failed: the Azure estimate does not name the vendored cache",
    )?;
    ensure(
        azure_estimate_ai
            .contains("references/vendored/ai/bedrock-pricing-cache.md` (primary for Bedrock)"),
        "assertion failed: the Azure AI estimate does not name the vendored cache as primary",
    )?;
    ensure(
        azure_estimate_ai.contains("`shared/pricing-cache.md` (primary for source providers)"),
        "assertion failed: the Azure AI estimate does not name the shared cache as primary",
    )?;
    ensure(
        !azure_cache.contains("## Bedrock Models (On-Demand)"),
        "assertion failed: the Azure cache still has the Bedrock section",
    )?;
    // Scoped to a reinstated Bedrock model row or recommendation sentence: the
    // refresh note legitimately names a model while listing re-verified rates.
    ensure(
        !azure_cache.contains("| Claude Fable"),
        "assertion failed: the Azure cache has a Claude Fable row",
    )?;
    ensure(
        !azure_cache.contains("default to Claude Fable"),
        "assertion failed: the Azure cache recommends Claude Fable",
    )?;
    ensure(
        metadata(azure_cache)?["Last updated"] == metadata(&texts.azure_old)?["Last updated"],
        "assertion failed: the Azure cache Last updated changed",
    )?;
    let source_heading = "Source Provider Pricing (for Migration Comparison)";
    ensure(
        markdown_data_rows(section(azure_cache, source_heading)?)
            == markdown_data_rows(section(&texts.azure_old, source_heading)?),
        "Azure source-provider rate rows changed",
    )?;
    ensure(
        azure_openai
            .contains("any Bedrock rate change into\n   `references/vendored/ai/bedrock-pricing-cache.md`"),
        "assertion failed: the Azure OpenAI guide does not route rate changes to the vendored cache",
    )
}

/// The embeddings target table and the vendored copies of the canonical cache.
fn check_embeddings_and_copies(plugin: &Path, texts: &Texts) -> Check<()> {
    // The pre-split Azure cache's own embeddings table is not part of the GCP
    // Bedrock section covered above, so a split could have dropped it.
    let heading = "Embeddings — Bedrock";
    let old_rows = markdown_data_rows(subsection(&texts.azure_old, heading)?);
    ensure(
        !old_rows.is_empty(),
        "pre-split Azure cache's Embeddings — Bedrock table is missing; nothing to compare against",
    )?;
    ensure(
        markdown_data_rows(subsection(&texts.canonical, heading)?) == old_rows,
        "Bedrock embedding target rows were not preserved during the split",
    )?;
    for copy in VENDORED {
        let text = read_text(&plugin.join(copy))?;
        ensure(
            markdown_data_rows(subsection(&text, heading)?) == old_rows,
            &format!(
                "Bedrock embedding target rows drifted in vendored copy: {}",
                plugin.join(copy).display()
            ),
        )?;
    }
    let canonical_bytes = fs::read(plugin.join(CANONICAL))
        .map_err(|error| format!("cannot read {CANONICAL}: {error}"))?;
    for copy in VENDORED {
        let bytes =
            fs::read(plugin.join(copy)).map_err(|error| format!("cannot read {copy}: {error}"))?;
        ensure(
            bytes == canonical_bytes,
            &format!("vendored drift: {}", plugin.join(copy).display()),
        )?;
    }
    Ok(())
}

/// Lookup keys, rates and the representative estimates (60 input and 40
/// output units): the keys and rates are unchanged, so the estimates are too.
fn check_lookups(texts: &Texts) -> Check<(usize, Json)> {
    let old_rows = model_rows(&texts.old)?;
    let new_rows = model_rows(&texts.canonical)?;
    ensure(
        new_rows == old_rows && !new_rows.is_empty(),
        "model lookup keys or quick-reference rates changed",
    )?;
    let estimate = |rows: &BTreeMap<String, (String, String)>, model: &str| -> Check<f64> {
        let (input, output) = rows
            .get(model)
            .ok_or_else(|| format!("representative model has no lookup row: {model}"))?;
        let parse = |text: &str| {
            text.parse::<f64>()
                .map_err(|_| format!("not a number: {text}"))
        };
        Ok(60.0 * parse(input)? + 40.0 * parse(output)?)
    };
    let mut estimates = Vec::new();
    for model in [
        "anthropic.claude-sonnet-5",
        "amazon.nova-pro-v1:0",
        "openai.gpt-5.6-luna",
    ] {
        let old = estimate(&old_rows, model)?;
        let new = estimate(&new_rows, model)?;
        ensure(
            old.to_bits() == new.to_bits(),
            "representative estimate outputs changed",
        )?;
        estimates.push((model.to_string(), Json::Float(new)));
    }
    Ok((new_rows.len(), Json::Object(estimates)))
}

/// Runs every check against `plugin`, reading pre-split files with `show`.
pub(crate) fn run_checks(plugin: &Path, show: &dyn Fn(&str) -> Check<String>) -> Check<Json> {
    let texts = Texts {
        old: show(OLD_CACHE)?,
        canonical: read_text(&plugin.join(CANONICAL))?,
        remaining: read_text(&plugin.join(GCP_CACHE))?,
        azure_old: show(OLD_AZURE_CACHE)?,
        azure_cache: read_text(&plugin.join(AZURE_CACHE))?,
    };
    let old_accuracy = pricing_accuracy_bands(&show(OLD_SKILL)?)?;
    let new_accuracy = pricing_accuracy_bands(&read_text(&plugin.join(GCP_SKILL))?)?;
    ensure(
        new_accuracy == old_accuracy,
        "estimate accuracy metadata changed",
    )?;

    check_gcp_cache(&texts)?;
    check_references(plugin, &texts)?;
    check_embeddings_and_copies(plugin, &texts)?;
    let (lookup_keys, estimates) = check_lookups(&texts)?;

    let forbidden = "skills/gcp-to-aws/references/shared/pricing-cache.md";
    for consumer in NEUTRAL_CONSUMERS {
        let path = plugin.join(consumer);
        ensure(
            !read_text(&path)?.contains(forbidden),
            &format!("neutral GCP pricing dependency remains: {}", path.display()),
        )?;
    }

    Ok(Json::object(vec![
        ("status", Json::string("PASS")),
        (
            "bedrock_model_lookup_keys",
            Json::Int(lookup_keys.try_into().unwrap_or(i64::MAX)),
        ),
        ("representative_monthly_estimates", estimates),
        (
            "pricing_accuracy_bands",
            Json::object(vec![
                ("infrastructure", Json::string(new_accuracy.0)),
                ("ai_models", Json::string(new_accuracy.1)),
            ]),
        ),
        ("vendored_copies", Json::Int(3)),
        ("baseline", Json::string(BASE_SHA)),
        (
            "pricing_behavior",
            Json::string("not checked: needs the Python bedrock_pricing.py modules"),
        ),
    ]))
}

/// Handler for `startup pricing-parity [--plugin-root DIR]`.
///
/// # Errors
///
/// A usage error (exit status 2) when the plugin directory does not exist; a
/// runtime error (exit status 1) with the failed assertion otherwise.
pub fn pricing_parity_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let plugin = resolve_lenient(Path::new(invocation.value("--plugin-root").unwrap_or(".")));
    if !plugin.is_dir() {
        return Err(ToolkitError::usage(
            "plugin_not_found",
            format!("Plugin directory not found: {}", plugin.display()),
        ));
    }
    // The repository root is two levels above `plugins/<name>`.
    let repo = plugin.parent().and_then(Path::parent).unwrap_or(&plugin);
    let report = run_checks(&plugin, &|path| git_show(repo, path))
        .map_err(|message| ToolkitError::runtime("pricing_parity_failed", message))?;
    output.line(&dumps(&report, Some(2), true))
}

#[cfg(test)]
mod tests;
