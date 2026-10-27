//! `toolkit startup model-id-lint`: fail when a plugin file names a Bedrock
//! model that the lifecycle registry has retired, outside the catalog files
//! whose job is to record it (port of `model-id-lint.py`).
//!
//! The banned set is read from `skills/shared/ai/ai-model-lifecycle.md`: the
//! IDs in the Model ID column of table rows whose status is **excluded**, and
//! every ID in the **Removed** list (before any `replacement:` clause). A
//! heuristic adds fabricated dated hybrids of the undated Sonnet 4.6 and
//! Opus 4.8 IDs. The Python regular expressions are replaced by exact
//! hand-written matchers whose behaviour is pinned by the tests.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{read_regular_file, PathError, ToolkitError};

use super::text::is_space;
use super::MAX_ARTIFACT_BYTES;
use crate::manifests::{
    is_py_space, py_splitlines, py_strip, read_failure, resolve_lenient, universal_newlines,
};
use crate::{Invocation, Output};

const REGISTRY_REL: &str = "skills/shared/ai/ai-model-lifecycle.md";
const EXTS: [&str; 8] = [
    ".md",
    ".py",
    ".json",
    ".ts",
    ".tf",
    ".sh",
    ".template",
    ".html",
];

/// Catalog files whose job is to record retired models. Vendored copies
/// collapse onto the canonical shared path first, so only canonical paths
/// are listed.
const CATALOG_ALLOWLIST: [&str; 3] = [
    "skills/shared/ai/ai-model-lifecycle.md",
    "skills/shared/ai/bedrock-pricing-cache.md",
    "skills/azure-to-aws/references/shared/pricing-cache.md",
];

/// The broken-ID-repair helper's example is intentionally invalid.
const FABRICATED_ALLOWLIST: [&str; 1] = [
    "skills/llm-to-bedrock/references/helpers/resolve-bedrock-model-id/resolve-bedrock-model-id.md",
];

/// The legacy Python lint excluded itself; its docstring names IDs.
const LEGACY_SCRIPT: &str = "scripts/model-id-lint.py";

/// Result of one lint run over a plugin tree.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LintOutcome {
    /// Problems that make the run meaningless (missing or unparsable registry).
    pub(crate) setup_errors: Vec<String>,
    /// One line per offending occurrence.
    pub(crate) failures: Vec<String>,
    /// Number of distinct retired IDs enforced.
    pub(crate) banned: usize,
}

/// Handler for `startup model-id-lint [--plugin-root DIR]` (default `.`).
///
/// # Errors
///
/// A usage error (exit status 2) when the plugin directory does not exist;
/// a runtime error (exit status 1) for setup problems, findings, or a tree
/// that cannot be walked.
pub fn model_id_lint_command(
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
    let outcome =
        lint(&plugin).map_err(|message| ToolkitError::runtime("model_id_lint_input", message))?;
    if !outcome.setup_errors.is_empty() {
        output.diagnostic(&format!(
            "model-id lint: {} setup problem(s)",
            outcome.setup_errors.len()
        ));
        for error in &outcome.setup_errors {
            output.diagnostic(&format!("  - {error}"));
        }
        return Err(ToolkitError::runtime(
            "model_id_lint_setup",
            format!(
                "model-id lint stopped on {} setup problem(s)",
                outcome.setup_errors.len()
            ),
        ));
    }
    if !outcome.failures.is_empty() {
        output.diagnostic(&format!(
            "model-id lint: {} problem(s)",
            outcome.failures.len()
        ));
        for failure in &outcome.failures {
            output.diagnostic(&format!("  - {failure}"));
        }
        return Err(ToolkitError::runtime(
            "model_id_lint_failed",
            format!("model-id lint found {} problem(s)", outcome.failures.len()),
        ));
    }
    output.line(&format!(
        "model-id lint: OK ({} retired IDs enforced from the registry)",
        outcome.banned
    ))
}

/// A retired model ID as a matcher: `*` is a token-bounded wildcard (any run
/// of characters that are neither whitespace nor a backtick); everything else
/// is literal and case-sensitive.
#[derive(Debug)]
pub(crate) struct IdMatcher {
    parts: Vec<String>,
    chars: Vec<Vec<char>>,
}

impl IdMatcher {
    pub(crate) fn new(model_id: &str) -> Self {
        let parts: Vec<String> = model_id.split('*').map(str::to_string).collect();
        let chars: Vec<Vec<char>> = parts.iter().map(|part| part.chars().collect()).collect();
        Self { parts, chars }
    }

    /// `re.search`: whether the ID occurs anywhere in `line`.
    pub(crate) fn is_match(&self, line: &str, chars: &[char]) -> bool {
        if !self.parts[0].is_empty() && !line.contains(self.parts[0].as_str()) {
            return false;
        }
        (0..=chars.len()).any(|start| self.matches_from(0, chars, start))
    }

    fn matches_from(&self, index: usize, chars: &[char], position: usize) -> bool {
        let literal = &self.chars[index];
        let end = position + literal.len();
        if chars.len() < end || chars[position..end] != literal[..] {
            return false;
        }
        if index + 1 == self.chars.len() {
            return true;
        }
        let mut limit = end;
        while limit < chars.len() && !is_space(chars[limit]) && chars[limit] != '`' {
            limit += 1;
        }
        (end..=limit).any(|next| self.matches_from(index + 1, chars, next))
    }
}

/// `skills/<skill>/references/vendored/<rel>` becomes `skills/shared/<rel>`.
pub(crate) fn canonicalize(rel: &str) -> String {
    let rest = rel.strip_prefix("skills/").and_then(|after| {
        let (skill, tail) = after.split_once('/')?;
        if skill.is_empty() {
            return None;
        }
        tail.strip_prefix("references/vendored/")
    });
    match rest {
        Some(tail) => format!("skills/shared/{tail}"),
        None => rel.to_string(),
    }
}

/// Whether `body` (the text between two backticks) is a Bedrock model ID:
/// `provider.model-name[:rev]` with optional `*` wildcards, or the bare
/// `vN:M` continuation of a dual-ID entry.
pub(crate) fn is_model_id(body: &str) -> bool {
    is_dotted_id(body) || is_bare_revision(body)
}

fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_bare_revision(body: &str) -> bool {
    body.strip_prefix('v')
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(version, revision)| all_digits(version) && all_digits(revision))
}

/// `[a-z0-9][a-z0-9.\-*]*[.\-][a-z0-9]+(:[0-9]+)?`, where the final
/// separator must precede the last run of `[a-z0-9]` and cannot be the very
/// first character.
fn is_dotted_id(body: &str) -> bool {
    let main = match body.split_once(':') {
        Some((main, revision)) => {
            if !all_digits(revision) {
                return false;
            }
            main
        }
        None => body,
    };
    let bytes = main.as_bytes();
    let alnum = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if !bytes.first().is_some_and(|first| alnum(*first)) {
        return false;
    }
    if !bytes
        .iter()
        .all(|byte| alnum(*byte) || matches!(byte, b'.' | b'-' | b'*'))
    {
        return false;
    }
    let mut run_start = bytes.len();
    while run_start > 0 && alnum(bytes[run_start - 1]) {
        run_start -= 1;
    }
    run_start < bytes.len() && run_start >= 2 && matches!(bytes[run_start - 1], b'.' | b'-')
}

/// `re.findall` of the backtick-enclosed model IDs in `text`: each match is
/// a whole span between two backticks; a span that is not an ID is skipped
/// and its closing backtick may open the next candidate.
pub(crate) fn ids_in_backticks(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find('`') {
        let start = from + open + 1;
        let Some(length) = text[start..].find('`') else {
            break;
        };
        let body = &text[start..start + length];
        if is_model_id(body) {
            ids.push(body.to_string());
            from = start + length + 1;
        } else {
            from = start;
        }
    }
    ids
}

/// A dual-ID entry lists a full ID followed by a bare `vN:M` continuation
/// sharing its stem (`amazon.nova-reel-v1:0` / `v1:1`); rebuild the
/// continuation into its own complete ID.
pub(crate) fn rebuild_dual_ids(tokens: &[String]) -> Vec<String> {
    let mut rebuilt = Vec::with_capacity(tokens.len());
    let mut last_full: Option<&String> = None;
    for token in tokens {
        match last_full {
            Some(full) if token.starts_with('v') && full.contains(':') => {
                let stem = full
                    .rsplit_once("-v")
                    .map_or(full.as_str(), |(stem, _)| stem);
                rebuilt.push(format!("{stem}-{token}"));
            }
            _ => {
                rebuilt.push(token.clone());
                last_full = Some(token);
            }
        }
    }
    rebuilt
}

/// Parses the registry. Returns the banned IDs (deduplicated in first-seen
/// order) and setup errors; a missing registry or a section that parses to
/// nothing refuses to run a no-op gate.
pub(crate) fn load_banned_ids(plugin: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let registry = plugin.join(REGISTRY_REL);
    if !super::is_regular_file_under(plugin, &registry)? {
        return Ok((
            Vec::new(),
            vec![format!("registry not found: {REGISTRY_REL}")],
        ));
    }
    let text = super::read_text_under(plugin, &registry, REGISTRY_REL)?;
    let lines = py_splitlines(&text);
    let mut ids: Vec<String> = Vec::new();
    let mut excluded_count = 0;
    let mut removed_count = 0;

    // 1) Table rows whose status cell is **excluded**: only the Model ID
    // column (the second cell), never the Active Replacement column.
    for line in &lines {
        if !line.trim_start_matches(is_py_space).starts_with('|') {
            continue;
        }
        if !line.to_lowercase().contains("excluded") {
            continue;
        }
        let cells: Vec<&str> = py_strip(line)
            .trim_matches('|')
            .split('|')
            .map(py_strip)
            .collect();
        if cells.len() < 2 {
            continue;
        }
        let found = ids_in_backticks(cells[1]);
        excluded_count += found.len();
        ids.extend(rebuild_dual_ids(&found));
    }

    // 2) The Removed (past-EOL) bullet list, up to any `replacement:` clause.
    let mut in_removed = false;
    for line in &lines {
        if line.starts_with("**Removed") {
            in_removed = true;
            continue;
        }
        if in_removed {
            if line.starts_with(['>', '#']) || line.starts_with("**") {
                in_removed = false;
                continue;
            }
            if line.trim_start_matches(is_py_space).starts_with('-') {
                let head = line
                    .split_once("replacement:")
                    .map_or(*line, |(head, _)| head);
                let found = ids_in_backticks(head);
                removed_count += found.len();
                ids.extend(rebuild_dual_ids(&found));
            }
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let unique: Vec<String> = ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();

    let mut errors = Vec::new();
    if excluded_count == 0 {
        errors.push(format!(
            "{REGISTRY_REL}: parsed ZERO **excluded** table rows — the Legacy/EOL table \
             format may have changed. Refusing to run a no-op gate."
        ));
    }
    if removed_count == 0 {
        errors.push(format!(
            "{REGISTRY_REL}: parsed ZERO IDs from the **Removed** (past-EOL) list — its \
             header/format may have changed. Refusing to run a no-op gate."
        ));
    }
    Ok((unique, errors))
}

/// Python `Path.suffix`: the text from the last dot, unless the dot starts
/// the name or ends it.
pub(crate) fn suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => &name[index..],
        _ => "",
    }
}

/// Collects candidate files (components relative to the plugin root) in
/// Python `Path` order. Symbolic links are never followed or reported.
fn collect_files(
    directory: &Path,
    prefix: &mut Vec<String>,
    files: &mut Vec<Vec<String>>,
) -> Result<(), String> {
    let shown = prefix.join("/");
    let entries =
        fs::read_dir(directory).map_err(|error| format!("cannot list {shown}: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot list {shown}: {error}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {shown}/{name}: {error}"))?;
        if kind.is_symlink() {
            continue;
        }
        prefix.push(name);
        let rel = prefix.join("/");
        if !rel.contains("node_modules") {
            if kind.is_dir() {
                collect_files(&entry.path(), prefix, files)?;
            } else if kind.is_file() {
                let name = prefix.last().map_or("", String::as_str);
                if EXTS.contains(&suffix(name)) {
                    files.push(prefix.clone());
                }
            }
        }
        prefix.pop();
    }
    Ok(())
}

/// The fabricated dated form `claude-(sonnet-4-6|opus-4-8)-<8 digits>`.
pub(crate) fn has_fabricated_id(line: &str) -> bool {
    let mut from = 0;
    while let Some(found) = line[from..].find("claude-") {
        let after = &line[from + found + "claude-".len()..];
        for stem in ["sonnet-4-6", "opus-4-8"] {
            if let Some(date) = after
                .strip_prefix(stem)
                .and_then(|rest| rest.strip_prefix('-'))
            {
                let bytes = date.as_bytes();
                if bytes.len() >= 8 && bytes[..8].iter().all(u8::is_ascii_digit) {
                    return true;
                }
            }
        }
        from += found + 1;
    }
    false
}

/// Lints one plugin tree.
///
/// # Errors
///
/// A message when the registry cannot be read or the tree cannot be walked.
pub(crate) fn lint(plugin: &Path) -> Result<LintOutcome, String> {
    let (banned_ids, setup_errors) = load_banned_ids(plugin)?;
    if !setup_errors.is_empty() {
        return Ok(LintOutcome {
            setup_errors,
            ..LintOutcome::default()
        });
    }
    let banned: Vec<(String, IdMatcher)> = banned_ids
        .iter()
        .map(|id| (id.clone(), IdMatcher::new(id)))
        .collect();

    let mut files = Vec::new();
    collect_files(plugin, &mut Vec::new(), &mut files)?;
    files.sort();

    let mut failures = Vec::new();
    for components in &files {
        let rel = components.join("/");
        if rel == LEGACY_SCRIPT {
            continue;
        }
        let path: PathBuf = components
            .iter()
            .fold(plugin.to_path_buf(), |mut acc, part| {
                acc.push(part);
                acc
            });
        let text = match read_regular_file(&path, MAX_ARTIFACT_BYTES) {
            Ok(text) => universal_newlines(&text),
            Err(error @ PathError::TooLarge { .. }) => {
                return Err(format!("cannot read {rel}: {}", read_failure(&error)));
            }
            // Unreadable or non-UTF-8 files were skipped by the Python lint.
            Err(_) => continue,
        };
        let canonical = canonicalize(&rel);
        let in_catalog = CATALOG_ALLOWLIST.contains(&canonical.as_str())
            || CATALOG_ALLOWLIST.contains(&rel.as_str());
        let in_fabricated_allowlist = FABRICATED_ALLOWLIST.contains(&canonical.as_str())
            || FABRICATED_ALLOWLIST.contains(&rel.as_str());

        for (index, line) in py_splitlines(&text).into_iter().enumerate() {
            let number = index + 1;
            if !in_catalog {
                let chars: Vec<char> = line.chars().collect();
                for (id, matcher) in &banned {
                    if matcher.is_match(line, &chars) {
                        failures.push(format!(
                            "{rel}:{number}: retired Bedrock model `{id}` \
                             (excluded/removed per ai-model-lifecycle.md) used outside the model catalog"
                        ));
                    }
                }
            }
            if !in_fabricated_allowlist && has_fabricated_id(line) {
                failures.push(format!(
                    "{rel}:{number}: fabricated dated ID — Sonnet 4.6 / Opus 4.8 Bedrock IDs are undated; \
                     this form never existed"
                ));
            }
        }
    }
    Ok(LintOutcome {
        setup_errors: Vec::new(),
        failures,
        banned: banned.len(),
    })
}
