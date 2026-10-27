//! `toolkit startup consent-audit`: consent-disclosure drift lint for the
//! usage-API discovery sub-files (port of `consent-disclosure-audit.py`).
//!
//! A `discover-*-api.md` sub-file promises which HTTP methods its flow calls.
//! The lint fails when the Capture Endpoint Table of the `## Step 2` section
//! calls a non-`GET` method but the file also carries a blanket "GET only"
//! style claim that does not name that method in an exception sentence. It is
//! a textual contradiction check; it calls no API.
//!
//! The Python script built the check from regular expressions. They are
//! replaced by exact hand-written matchers (no regex engine): the phrase
//! claims run on the shared [`Pattern`] matcher, the table, heading, list-item
//! and sentence rules are small functions on character slices.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use toolkit_core::{read_regular_file, ToolkitError};

use super::text::{fold, is_space, is_word, Pattern, Tok};
use super::MAX_ARTIFACT_BYTES;
use crate::manifests::{
    py_repr_str, py_splitlines, py_strip, read_failure, resolve_lenient, universal_newlines,
};
use crate::{Invocation, Output};

/// Directory part of the glob that selects the discovery sub-files.
const TARGET_DIR: &str = "references/phases/discover";
/// The glob as the messages print it.
const TARGET_GLOB: &str = "references/phases/discover/discover-*-api.md";

/// The HTTP methods the table recognises, in the Python alternation order.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Blanket "GET only" phrasings, checked block by block (case-insensitive).
/// `(?:s|ed)?` is `Tok::Either` with an empty alternative; the optional
/// backticks are the inline-code wrapper around a bare method word.
static BLANKET_CLAIMS: [Pattern; 8] = [
    Pattern::new(
        r"`?get`? requests? only",
        &[
            Tok::Opt('`'),
            Tok::Lit("get"),
            Tok::Opt('`'),
            Tok::Lit(" request"),
            Tok::Opt('s'),
            Tok::Lit(" only"),
        ],
    ),
    Pattern::new(
        r"\bget-only\b",
        &[Tok::Boundary, Tok::Lit("get-only"), Tok::Boundary],
    ),
    Pattern::new(
        r"\b`?get`? only\b",
        &[
            Tok::Boundary,
            Tok::Opt('`'),
            Tok::Lit("get"),
            Tok::Opt('`'),
            Tok::Lit(" only"),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"only call(?:s|ed)? `?get`?\b",
        &[
            Tok::Lit("only call"),
            Tok::Either(&["s", "ed", ""]),
            Tok::Lit(" "),
            Tok::Opt('`'),
            Tok::Lit("get"),
            Tok::Opt('`'),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"i only call `?get`?\b",
        &[
            Tok::Lit("i only call "),
            Tok::Opt('`'),
            Tok::Lit("get"),
            Tok::Opt('`'),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"non-get (?:method|request)",
        &[Tok::Lit("non-get "), Tok::Either(&["method", "request"])],
    ),
    Pattern::new(
        r"every row is `?get`?\b",
        &[
            Tok::Lit("every row is "),
            Tok::Opt('`'),
            Tok::Lit("get"),
            Tok::Opt('`'),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"any other http method\b",
        &[Tok::Lit("any other http method"), Tok::Boundary],
    ),
];

/// Words that open an exception: `\bexcept\b`, `\bplus one\b`, `\bplus a\b`.
const EXCEPTION_MARKERS: [&str; 3] = ["except", "plus one", "plus a"];

/// Result of auditing a plugin tree.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) setup_errors: Vec<String>,
    pub(crate) failures: Vec<String>,
    pub(crate) checked: usize,
}

fn at_boundary(chars: &[char], pos: usize) -> bool {
    let before = pos > 0 && is_word(chars[pos - 1]);
    let after = chars.get(pos).copied().is_some_and(is_word);
    before != after
}

fn skip_spaces(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).copied().is_some_and(is_space) {
        at += 1;
    }
    at
}

/// Exact (case-sensitive) `word` at `at`.
fn starts_with(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

/// Case-insensitive `word` at `at` (`re.IGNORECASE`).
fn starts_with_folded(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset).is_some_and(|d| fold(*d) == fold(c)))
}

/// `(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b` at `at`: the method and the
/// position after it.
fn method_at(chars: &[char], at: usize) -> Option<(&'static str, usize)> {
    METHODS.iter().find_map(|method| {
        let end = at + method.len();
        (starts_with(chars, at, method) && !chars.get(end).copied().is_some_and(is_word))
            .then_some((*method, end))
    })
}

/// One call-site match at `at`: a method word, optional spaces, an optional
/// `to that same base` or `against`, optional spaces and then a backtick or a
/// slash. Returns the method and the end of the match.
fn call_at(chars: &[char], at: usize) -> Option<(&'static str, usize)> {
    if !at_boundary(chars, at) {
        return None;
    }
    let (method, end) = method_at(chars, at)?;
    let after_spaces = skip_spaces(chars, end);
    let is_opener = |pos: usize| matches!(chars.get(pos), Some('`' | '/'));
    for group in ["to that same base", "against"] {
        if starts_with(chars, after_spaces, group) {
            let next = skip_spaces(chars, after_spaces + group.len());
            if is_opener(next) {
                return Some((method, next + 1));
            }
        }
    }
    is_opener(after_spaces).then_some((method, after_spaces + 1))
}

/// Every real call site in an Endpoint cell (`finditer`: non-overlapping).
fn method_calls(cell: &str) -> Vec<&'static str> {
    let chars: Vec<char> = cell.chars().collect();
    let mut found = Vec::new();
    let mut pos = 0;
    while pos < chars.len() {
        if let Some((method, end)) = call_at(&chars, pos) {
            found.push(method);
            pos = end;
        } else {
            pos += 1;
        }
    }
    found
}

/// `Endpoint\s*\(\s*(METHOD)\b` searched in a header cell.
fn header_cell_method(cell: &str) -> Option<&'static str> {
    let chars: Vec<char> = cell.chars().collect();
    (0..chars.len()).find_map(|start| {
        if !starts_with(&chars, start, "Endpoint") {
            return None;
        }
        let open = skip_spaces(&chars, start + "Endpoint".len());
        if chars.get(open) != Some(&'(') {
            return None;
        }
        method_at(&chars, skip_spaces(&chars, open + 1)).map(|(method, _)| method)
    })
}

/// `(?i)^endpoint\b` matched against a whole cell.
fn is_endpoint_header(cell: &str) -> bool {
    let chars: Vec<char> = cell.chars().collect();
    starts_with_folded(&chars, 0, "endpoint") && at_boundary(&chars, "endpoint".len())
}

/// `^[\s:-]+$` matched against a stripped cell.
fn is_separator_cell(cell: &str) -> bool {
    !cell.is_empty() && cell.chars().all(|c| is_space(c) || c == ':' || c == '-')
}

/// A markdown table row split into its stripped cells, or `None` for a line
/// that does not start with a pipe.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let stripped = py_strip(line);
    if !stripped.starts_with('|') {
        return None;
    }
    let mut cells: Vec<&str> = stripped.split('|').collect();
    // `"| a | b |"` splits to ['', ' a ', ' b ', '']: drop the empty bookends.
    if cells.first() == Some(&"") {
        cells.remove(0);
    }
    if cells.last() == Some(&"") {
        cells.pop();
    }
    Some(
        cells
            .into_iter()
            .map(|cell| py_strip(cell).to_string())
            .collect(),
    )
}

/// `^\s*\|[\s:-]+\|[\s:-]+\|` (multiline) searched in the section: a header
/// separator row such as `| - | --- |`.
fn has_table(section: &[char]) -> bool {
    let in_class = |c: char| is_space(c) || c == ':' || c == '-';
    let run = |mut pos: usize| -> Option<usize> {
        let start = pos;
        while section.get(pos).copied().is_some_and(in_class) {
            pos += 1;
        }
        (pos > start).then_some(pos)
    };
    (0..section.len())
        .filter(|&start| start == 0 || section[start - 1] == '\n')
        .any(|start| {
            let open = skip_spaces(section, start);
            if section.get(open) != Some(&'|') {
                return false;
            }
            let Some(first) = run(open + 1) else {
                return false;
            };
            if section.get(first) != Some(&'|') {
                return false;
            }
            run(first + 1).is_some_and(|second| section.get(second) == Some(&'|'))
        })
}

/// The `## Step 2: ...` section: from the end of its heading line up to the
/// next `## ` heading (`^##\s`, multiline), as characters.
fn capture_section(text: &str) -> Option<Vec<char>> {
    let chars: Vec<char> = text.chars().collect();
    let line_start = |pos: usize| pos == 0 || chars[pos - 1] == '\n';
    let heading_end = (0..chars.len()).find_map(|start| {
        if !line_start(start) || !starts_with(&chars, start, "##") {
            return None;
        }
        let after_hashes = start + 2;
        let word = skip_spaces(&chars, after_hashes);
        if word == after_hashes || !starts_with(&chars, word, "Step 2:") {
            return None;
        }
        let rest = word + "Step 2:".len();
        Some(
            chars[rest..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |n| rest + n),
        )
    })?;
    let rest = &chars[heading_end..];
    let next_heading = (1..rest.len()).find(|&pos| {
        rest[pos - 1] == '\n'
            && starts_with(rest, pos, "##")
            && rest.get(pos + 2).copied().is_some_and(is_space)
    });
    Some(rest[..next_heading.unwrap_or(rest.len())].to_vec())
}

/// The real HTTP methods the Capture Endpoint Table calls and whether Step 2
/// holds a table whose shape no method could be read from.
///
/// The check is scoped to the Step 2 section and to the table's own Endpoint
/// column, so a method named in a Purpose cell, in prose, or in another
/// section's policy text is never read as a call.
pub(crate) fn methods_in_capture_section(text: &str) -> (BTreeSet<&'static str>, bool) {
    let Some(section_chars) = capture_section(text) else {
        return (BTreeSet::new(), false);
    };
    let section: String = section_chars.iter().collect();
    let mut methods: BTreeSet<&'static str> = BTreeSet::new();
    let mut endpoint_col: Option<usize> = None;
    let mut header_method = None;
    for line in py_splitlines(&section) {
        let Some(cells) = table_cells(line) else {
            continue;
        };
        let Some(col) = endpoint_col else {
            if let Some((idx, cell)) = cells
                .iter()
                .enumerate()
                .find(|(_, cell)| is_endpoint_header(cell))
            {
                endpoint_col = Some(idx);
                header_method = header_cell_method(cell);
            }
            continue;
        };
        let Some(cell) = cells.get(col) else {
            continue;
        };
        // The header-separator row itself is not a data row.
        if is_separator_cell(cell) && cells.iter().all(|c| is_separator_cell(c)) {
            continue;
        }
        methods.extend(method_calls(cell));
    }
    methods.extend(header_method);
    if !methods.is_empty() {
        return (methods, false);
    }
    // A table with nothing read is an unrecognised shape, never "GET only".
    (BTreeSet::new(), has_table(&section_chars))
}

/// `^(\s*)(?:[-*+]|\d+\.)\s+`: the indent of a Markdown list item line.
fn list_item_indent(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let indent = skip_spaces(&chars, 0);
    let marker_end = match chars.get(indent)? {
        '-' | '*' | '+' => indent + 1,
        c if c.is_ascii_digit() => {
            let mut end = indent;
            while chars.get(end).is_some_and(char::is_ascii_digit) {
                end += 1;
            }
            if chars.get(end) != Some(&'.') {
                return None;
            }
            end + 1
        }
        _ => return None,
    };
    chars
        .get(marker_end)
        .copied()
        .is_some_and(is_space)
        .then_some(indent)
}

/// Logical blocks of `text`, each with its first line number. A block ends at a
/// blank line or at the next list item at the same or a shallower indent;
/// wrapped continuation lines stay in the block.
fn paragraphs(text: &str) -> Vec<(usize, Vec<&str>)> {
    let mut blocks: Vec<(usize, Vec<&str>)> = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut start = 1;
    let mut item_indent: Option<usize> = None;
    for (index, line) in py_splitlines(text).into_iter().enumerate() {
        let number = index + 1;
        if py_strip(line).is_empty() {
            if !buf.is_empty() {
                blocks.push((start, std::mem::take(&mut buf)));
            }
            item_indent = None;
            continue;
        }
        if let Some(indent) = list_item_indent(line) {
            if !buf.is_empty() && item_indent.is_none_or(|current| indent <= current) {
                blocks.push((start, std::mem::take(&mut buf)));
            }
            item_indent = Some(indent);
        }
        if buf.is_empty() {
            start = number;
        }
        buf.push(line);
    }
    if !buf.is_empty() {
        blocks.push((start, buf));
    }
    blocks
}

/// Which physical line (0-indexed) a character offset of the soft-wrap-joined
/// block falls on.
fn line_for_offset(lines: &[&str], offset: usize) -> usize {
    let mut pos = 0;
    for (index, line) in lines.iter().enumerate() {
        let length = py_strip(line).chars().count();
        if offset <= pos + length {
            return index;
        }
        pos += length + 1;
    }
    lines.len().saturating_sub(1)
}

/// `(?<=[.?!])\s+` split: a whitespace run after sentence punctuation ends a
/// sentence.
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut current = String::new();
    let mut pos = 0;
    while pos < chars.len() {
        if is_space(chars[pos]) && pos > 0 && matches!(chars[pos - 1], '.' | '?' | '!') {
            found.push(std::mem::take(&mut current));
            pos = skip_spaces(&chars, pos);
        } else {
            current.push(chars[pos]);
            pos += 1;
        }
    }
    found.push(current);
    found
}

/// `\bword\b` (case-insensitive) occurring anywhere in `text`.
fn contains_word(text: &str, word: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let width = word.chars().count();
    (0..chars.len()).any(|start| {
        starts_with_folded(&chars, start, word)
            && (start == 0 || !is_word(chars[start - 1]))
            && !chars.get(start + width).copied().is_some_and(is_word)
    })
}

/// Every sentence that itself carries an exception marker: the only text an
/// exception is allowed to grant coverage from.
fn exception_clauses(normalized: &str) -> Vec<String> {
    sentences(normalized)
        .into_iter()
        .filter(|sentence| {
            EXCEPTION_MARKERS
                .iter()
                .any(|marker| contains_word(sentence, marker))
        })
        .collect()
}

/// Every `(line number, offending line)` carrying a blanket GET-only claim
/// with no exception that names every non-GET method the file calls, each as
/// a complete token inside one exception sentence.
pub(crate) fn unqualified_claims(
    text: &str,
    non_get_methods: &BTreeSet<&'static str>,
) -> Vec<(usize, String)> {
    let mut failures = Vec::new();
    if non_get_methods.is_empty() {
        return failures;
    }
    for (para_start, lines) in paragraphs(text) {
        let normalized = lines
            .iter()
            .map(|line| py_strip(line))
            .collect::<Vec<_>>()
            .join(" ");
        let covered = exception_clauses(&normalized).iter().any(|clause| {
            non_get_methods
                .iter()
                .all(|method| contains_word(clause, method))
        });
        if covered {
            continue;
        }
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for pattern in &BLANKET_CLAIMS {
            for offset in pattern.find_starts(&normalized) {
                let index = line_for_offset(&lines, offset);
                if seen.insert(index) {
                    failures.push((para_start + index, py_strip(lines[index]).to_string()));
                }
            }
        }
    }
    failures
}

/// The discovery sub-files of `plugin`, sorted by skill then file name, as
/// `(skill, file name)`.
fn target_files(plugin: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let Ok(skills) = fs::read_dir(plugin.join("skills")) else {
        return files;
    };
    for skill in skills.flatten() {
        let skill_name = skill.file_name().to_string_lossy().into_owned();
        let Ok(entries) = fs::read_dir(skill.path().join(TARGET_DIR)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `discover-*-api.md`: the star may be empty, the pieces may not overlap.
            if name.len() >= "discover--api.md".len()
                && name.starts_with("discover-")
                && name.ends_with("-api.md")
            {
                files.push((skill_name.clone(), name));
            }
        }
    }
    files.sort();
    files
}

/// Audits the discovery sub-files of `plugin`.
pub(crate) fn audit(plugin: &Path) -> Outcome {
    let mut outcome = Outcome::default();
    let files = target_files(plugin);
    if files.is_empty() {
        outcome.setup_errors.push(format!(
            "consent-disclosure audit: found ZERO files matching skills/*/{TARGET_GLOB} — the \
             discovery sub-file layout may have changed. Refusing to run a no-op gate."
        ));
        return outcome;
    }
    for (skill, name) in files {
        let rel = format!("skills/{skill}/{TARGET_DIR}/{name}");
        let path = plugin.join(&rel);
        let text = match read_regular_file(&path, MAX_ARTIFACT_BYTES) {
            Ok(text) => universal_newlines(&text),
            Err(error) => {
                // An unreadable file was inspected for NOTHING, not confirmed
                // clean: it is a failure, never a silent skip.
                outcome.failures.push(format!(
                    "{rel}: could not read file ({}) — treating as a failure, not a skip",
                    read_failure(&error)
                ));
                continue;
            }
        };
        outcome.checked += 1;
        audit_text(&rel, &text, &mut outcome.failures);
    }
    outcome
}

fn audit_text(rel: &str, text: &str, failures: &mut Vec<String>) {
    let (real_methods, unrecognized_table) = methods_in_capture_section(text);
    if unrecognized_table {
        failures.push(format!(
            "{rel}: Step 2 contains a Capture Endpoint Table but no HTTP method could be read \
             from it (neither a per-row `METHOD /path` cell nor an 'Endpoint (METHOD, ...)' \
             header cell matched) — fix the table shape, or this script's method patterns, \
             rather than letting this file pass unchecked"
        ));
        return;
    }
    let non_get: BTreeSet<&'static str> = real_methods
        .into_iter()
        .filter(|method| *method != "GET")
        .collect();
    // A GET-only flow cannot have a disclosure gap.
    if non_get.is_empty() {
        return;
    }
    let joined = non_get.iter().copied().collect::<Vec<_>>().join("/");
    for (line_no, line) in unqualified_claims(text, &non_get) {
        failures.push(format!(
            "{rel}:{line_no}: claims a GET-only restriction, but the Capture Endpoint Table \
             calls {joined} too — add an inline exception naming the method, or the endpoint, \
             to this line: {}",
            py_repr_str(&line)
        ));
    }
}

/// Handler for `startup consent-audit [--plugin-root DIR]`.
///
/// # Errors
///
/// A usage error (exit status 2) when the plugin directory does not exist; a
/// runtime error (exit status 1) when no discovery sub-file exists or any
/// problem is found.
pub fn consent_audit_command(
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
    let outcome = audit(&plugin);
    if !outcome.setup_errors.is_empty() {
        output.diagnostic(&format!(
            "consent-disclosure audit: {} setup problem(s)",
            outcome.setup_errors.len()
        ));
        for error in &outcome.setup_errors {
            output.diagnostic(&format!("  - {error}"));
        }
        return Err(ToolkitError::runtime(
            "consent_audit_setup",
            format!(
                "consent-disclosure audit stopped on {} setup problem(s)",
                outcome.setup_errors.len()
            ),
        ));
    }
    if !outcome.failures.is_empty() {
        output.diagnostic(&format!(
            "consent-disclosure audit: {} problem(s)",
            outcome.failures.len()
        ));
        for failure in &outcome.failures {
            output.diagnostic(&format!("  - {failure}"));
        }
        return Err(ToolkitError::runtime(
            "consent_audit_failed",
            format!(
                "consent-disclosure audit found {} problem(s)",
                outcome.failures.len()
            ),
        ));
    }
    output.line(&format!(
        "consent-disclosure audit: OK ({} discovery sub-file(s) checked)",
        outcome.checked
    ))
}

#[cfg(test)]
mod tests;
