//! `toolkit startup model-staleness`: surface a stale Bedrock model-lifecycle
//! registry (port of `plugins/aws-startup-advisor/tools/model-staleness.py`,
//! #4407).
//!
//! The registry `skills/shared/ai/ai-model-lifecycle.md` declares its own
//! freshness contract: a `**Last updated:** YYYY-MM-DD` line and a
//! `**Staleness window:** N days` line. The check reads both, compares the
//! date with an injected "today" and verifies that every vendored copy
//! (`skills/*/references/vendored/ai/ai-model-lifecycle.md`) is byte-identical
//! to the canonical file.
//!
//! Two failure classes, gated differently, as in the Python tool:
//!
//! - **hard**: a vendored copy that differs, a required copy that is missing,
//!   a missing or malformed contract, a future date, or a registry that lost
//!   its canonical file. These fail always.
//! - **stale**: the snapshot aged past its window. This is time based, so it
//!   fails only under `--strict` and is a warning otherwise.
//!
//! The current date is a parameter ([`Date`]); only the command handler reads
//! the clock, so every test pins the date. The Python `re` patterns are
//! replaced by exact hand-written matchers whose behaviour is pinned by the
//! tests.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_core::{neutralize_line, open_regular_file, PathError, ToolkitError};

use super::{is_regular_file_under, MAX_ARTIFACT_BYTES};
use crate::manifests::{is_py_space, read_failure, resolve_lenient, universal_newlines};
use crate::{Invocation, Output};

/// Registration of the `startup-model-staleness` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-model-staleness",
    commands: &[crate::CommandSpec {
        path: &["startup", "model-staleness"],
        flags: &["--strict"],
        value_flags: &["--plugin-root"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: crate::startup::model_staleness_command,
    }],
    synopsis: r"  toolkit startup model-staleness [--plugin-root DIR] [--strict]
",
    description: r"  startup model-staleness
                Check the Bedrock model-lifecycle registry
                skills/shared/ai/ai-model-lifecycle.md of --plugin-root: its
                `**Last updated:**` and `**Staleness window:**` contract, the
                required vendored copies and byte-identical vendored copies
                (model-staleness.py). A drifted or missing copy, or a missing,
                malformed or future-dated contract, fails always; a snapshot
                older than its window warns, and fails (exit 1) with --strict.
",
    exit_notes: "",
    doc: "docs/migration/startup-tools.md",
    synopsis_rank: 180,
    description_rank: 180,
};

/// The canonical registry, relative to the plugin root.
const CANONICAL: &str = "skills/shared/ai/ai-model-lifecycle.md";
/// File name of the registry and of every copy of it.
const REGISTRY_NAME: &str = "ai-model-lifecycle.md";
/// Directory of a vendored copy below `skills/<skill>/`.
const VENDORED_TAIL: &str = "references/vendored/ai/ai-model-lifecycle.md";

/// Vendored copies that must exist: `estimate-ai.md` and `design.md` read
/// these paths, so dropping one is a failure and not a silent pass.
const REQUIRED_VENDORED: [&str; 2] = [
    "skills/gcp-to-aws/references/vendored/ai/ai-model-lifecycle.md",
    "skills/azure-to-aws/references/vendored/ai/ai-model-lifecycle.md",
];

/// A calendar date as a day count since 1970-01-01 (proleptic Gregorian).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Date(i64);

impl Date {
    /// The date `year-month-day`, or `None` when it is not a real calendar
    /// date in years 1 to 9999 (what `datetime.date` accepts).
    pub(crate) fn new(year: i64, month: i64, day: i64) -> Option<Self> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
            return None;
        }
        if day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self(days_from_civil(year, month, day)))
    }

    /// Days from `then` to `self` (negative when `then` is later).
    fn days_since(self, then: Self) -> i64 {
        self.0 - then.0
    }

    /// The current date in UTC. Python used the local date; the offset can
    /// move the boundary by at most one day (docs/migration/startup-tools.md).
    fn today_utc() -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Self(i64::try_from(seconds / 86_400).unwrap_or(0))
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a valid proleptic Gregorian date (Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The problems found, by class.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    /// Fail always.
    pub(crate) hard: Vec<String>,
    /// Fail only under `--strict`.
    pub(crate) stale: Vec<String>,
    /// Passing observations (`  ok: ...` on stdout).
    pub(crate) notes: Vec<String>,
}

/// `text` as one safe line: caller-derived names (skill directories) must not
/// forge lines or drive the terminal.
fn shown(text: &str) -> String {
    neutralize_line(text).into_owned()
}

/// A bounded regular-file read that refuses links, as raw bytes.
fn read_bytes(path: &Path) -> Result<Vec<u8>, PathError> {
    let (file, opened) = open_regular_file(path)?;
    let too_large = || PathError::TooLarge {
        path: path.to_path_buf(),
        limit: MAX_ARTIFACT_BYTES,
    };
    if opened.len() > MAX_ARTIFACT_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| PathError::Io(error, path.to_path_buf()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ARTIFACT_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

fn read_problem(relative: &str, error: &PathError) -> String {
    format!("{relative}: cannot read: {}", read_failure(error))
}

/// Files named [`REGISTRY_NAME`] anywhere below `skills/` of `plugin` (the
/// `skills/**/ai-model-lifecycle.md` glob), sorted. Linked directories are not
/// descended into; an entry of any type with the name is listed.
fn registry_files(plugin: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name() == REGISTRY_NAME {
                out.push(path.clone());
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(&path, out);
            }
        }
    }
    let mut found = Vec::new();
    walk(&plugin.join("skills"), &mut found);
    found.sort();
    found
}

/// Names of the directories directly below `skills/`, sorted.
fn skill_names(plugin: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(plugin.join("skills")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `path` below `plugin` as the relative `/`-separated text the messages use.
fn relative_text(plugin: &Path, path: &Path) -> String {
    path.strip_prefix(plugin).map_or_else(
        |_| path.display().to_string(),
        |tail| {
            tail.components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        },
    )
}

/// `^<marker>` at the start of a line, then the value matcher.
///
/// The Python patterns are `re.MULTILINE` searches whose gaps are `\s*`
/// (which crosses newlines) and which end in `\s*$`. The first line start
/// whose remainder matches wins.
fn find_contract_line<T>(
    text: &str,
    marker: &str,
    value: impl Fn(&[char], usize) -> Option<(T, usize)>,
) -> Option<T> {
    let chars: Vec<char> = text.chars().collect();
    let marker: Vec<char> = marker.chars().collect();
    for start in 0..chars.len() {
        if start != 0 && chars[start - 1] != '\n' {
            continue;
        }
        if chars[start..].starts_with(&marker) {
            let at = skip_space(&chars, start + marker.len());
            if let Some((found, end)) = value(&chars, at) {
                if ends_line(&chars, end) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn skip_space(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).copied().is_some_and(is_py_space) {
        at += 1;
    }
    at
}

/// `\s*$` (multi-line `$`) from `at`: the whitespace run that starts there
/// contains a newline or reaches the end of the text.
fn ends_line(chars: &[char], at: usize) -> bool {
    let mut position = at;
    loop {
        match chars.get(position) {
            None | Some('\n') => return true,
            Some(c) if is_py_space(*c) => position += 1,
            Some(_) => return false,
        }
    }
}

fn digits_at(chars: &[char], at: usize, count: usize) -> Option<i64> {
    let slice = chars.get(at..at + count)?;
    if !slice.iter().all(char::is_ascii_digit) {
        return None;
    }
    slice
        .iter()
        .try_fold(0_i64, |acc, c| Some(acc * 10 + i64::from(c.to_digit(10)?)))
}

/// `(\d{4}-\d{2}-\d{2})` as the raw text (validity is checked afterwards, so
/// "not a real calendar date" stays distinct from "no contract line").
fn match_date(chars: &[char], at: usize) -> Option<(String, usize)> {
    let shape_ok = digits_at(chars, at, 4).is_some()
        && chars.get(at + 4) == Some(&'-')
        && digits_at(chars, at + 5, 2).is_some()
        && chars.get(at + 7) == Some(&'-')
        && digits_at(chars, at + 8, 2).is_some();
    shape_ok.then(|| (chars[at..at + 10].iter().collect(), at + 10))
}

/// `(\d+)\s*days?` as the digit text.
fn match_window(chars: &[char], at: usize) -> Option<(String, usize)> {
    let mut end = at;
    while chars.get(end).is_some_and(char::is_ascii_digit) {
        end += 1;
    }
    if end == at {
        return None;
    }
    let digits: String = chars[at..end].iter().collect();
    let word = skip_space(chars, end);
    let day: Vec<char> = "day".chars().collect();
    if !chars[word.min(chars.len())..].starts_with(&day) {
        return None;
    }
    let mut after = word + day.len();
    if chars.get(after) == Some(&'s') {
        // `days?` is greedy but yields the `s` when the line does not end.
        if ends_line(chars, after + 1) {
            after += 1;
        }
    }
    Some((digits, after))
}

/// Parses `YYYY-MM-DD` as `datetime.strptime` does, or `None`.
fn parse_iso_date(text: &str) -> Option<Date> {
    let chars: Vec<char> = text.chars().collect();
    Date::new(
        digits_at(&chars, 0, 4)?,
        digits_at(&chars, 5, 2)?,
        digits_at(&chars, 8, 2)?,
    )
}

/// The window as an unsigned number and as Python prints it (no leading
/// zeros). More than 38 digits is not representable.
fn parse_window(digits: &str) -> Option<(u128, String)> {
    let trimmed = digits.trim_start_matches('0');
    let shown_text = if trimmed.is_empty() { "0" } else { trimmed };
    shown_text
        .parse::<u128>()
        .ok()
        .map(|number| (number, shown_text.to_string()))
}

/// Checks the registry of `plugin_root`, appending to `findings`.
///
/// `today` is injected so tests are deterministic.
pub(crate) fn check_plugin(plugin_root: &Path, today: Date, findings: &mut Findings) {
    let canonical = plugin_root.join(CANONICAL);
    match is_regular_file_under(plugin_root, &canonical) {
        Err(message) => {
            findings.hard.push(shown(&message));
            return;
        }
        Ok(false) => {
            report_missing_canonical(plugin_root, findings);
            return;
        }
        Ok(true) => {}
    }

    let bytes = match read_bytes(&canonical) {
        Ok(bytes) => bytes,
        Err(error) => {
            findings.hard.push(shown(&read_problem(CANONICAL, &error)));
            return;
        }
    };
    let Ok(text) = String::from_utf8(bytes.clone()) else {
        findings
            .hard
            .push(format!("{CANONICAL}: cannot read: not valid UTF-8"));
        return;
    };
    check_contract(&universal_newlines(&text), today, findings);

    check_required_copies(plugin_root, findings);
    check_copies_match(plugin_root, &bytes, findings);
}

/// The canonical file is absent. It is exempt only when nothing else survives:
/// a surviving copy means a consumer reads a registry that lost its gate.
fn report_missing_canonical(plugin_root: &Path, findings: &mut Findings) {
    let mut surviving: Vec<PathBuf> = Vec::new();
    for required in REQUIRED_VENDORED {
        let path = plugin_root.join(required);
        match is_regular_file_under(plugin_root, &path) {
            Ok(true) => surviving.push(path),
            Ok(false) => {}
            Err(message) => findings.hard.push(shown(&message)),
        }
    }
    for path in registry_files(plugin_root) {
        if !surviving.contains(&path) {
            surviving.push(path);
        }
    }
    if surviving.is_empty() {
        // The plugin ships no model-lifecycle registry at all: nothing to gate.
        return;
    }
    let listed = surviving
        .iter()
        .map(|path| relative_text(plugin_root, path))
        .collect::<Vec<_>>()
        .join(", ");
    findings.hard.push(shown(&format!(
        "canonical {CANONICAL} is MISSING, but {listed} still exist and are read by \
         estimate-ai.md/design.md — this plugin still has an active model registry with NO \
         freshness gate. Restore the canonical file (or delete every surviving copy if the \
         registry is being retired intentionally)."
    )));
}

/// The freshness contract must be present and well formed (hard), then the
/// age is compared with the window (stale).
fn check_contract(text: &str, today: Date, findings: &mut Findings) {
    let rel = CANONICAL;
    let Some(last_updated) = find_contract_line(text, "**Last updated:**", match_date) else {
        findings.hard.push(format!(
            "{rel}: no '**Last updated:** YYYY-MM-DD' line — the freshness contract is missing."
        ));
        return;
    };
    let Some(window_digits) = find_contract_line(text, "**Staleness window:**", match_window)
    else {
        findings.hard.push(format!(
            "{rel}: no '**Staleness window:** N days' line — the freshness contract is incomplete."
        ));
        return;
    };
    let Some((window, window_text)) = parse_window(&window_digits) else {
        findings.hard.push(format!(
            "{rel}: '**Staleness window:** {}' is too large to be a day count.",
            window_digits.trim_start_matches('0')
        ));
        return;
    };
    let Some(then) = parse_iso_date(&last_updated) else {
        findings.hard.push(format!(
            "{rel}: '**Last updated:** {last_updated}' is not a real calendar date."
        ));
        return;
    };
    let age = today.days_since(then);
    if age < 0 {
        // A future date is a malformed contract, not "extra fresh": treated as
        // staleness (negative headroom) it would report fresh forever.
        findings.hard.push(format!(
            "{rel}: '**Last updated:** {last_updated}' is in the future ({}d from now) — a bad \
             bump would disable the freshness gate. Fix the date.",
            -age
        ));
        return;
    }
    let age_wide = u128::try_from(age).unwrap_or(0);
    if age_wide > window {
        findings.stale.push(format!(
            "{rel}: STALE — last updated {last_updated} ({age}d ago), window {window_text}d, over \
             by {}d. Refresh the registry against the Bedrock model lifecycle page and bump 'Last \
             updated'.",
            age_wide - window
        ));
    } else {
        findings.notes.push(format!(
            "{rel}: fresh — {age}d old (window {window_text}d, {}d of headroom).",
            window - age_wide
        ));
    }
}

/// Required vendored copies must exist (hard): the glob of the copies only
/// sees what is present, so a deleted copy is invisible to a byte compare.
fn check_required_copies(plugin_root: &Path, findings: &mut Findings) {
    for required in REQUIRED_VENDORED {
        let path = plugin_root.join(required);
        match is_regular_file_under(plugin_root, &path) {
            Ok(true) => {}
            Ok(false) => findings.hard.push(format!(
                "{required}: MISSING — a required vendored copy of the model-lifecycle registry. \
                 AI estimate/design load this path; do not delete it. Copy the canonical \
                 {CANONICAL} into place."
            )),
            Err(message) => findings.hard.push(shown(&message)),
        }
    }
}

/// Every vendored copy that exists must be byte-identical to the canonical
/// file (hard).
fn check_copies_match(plugin_root: &Path, canonical: &[u8], findings: &mut Findings) {
    for skill in skill_names(plugin_root) {
        let relative = format!("skills/{skill}/{VENDORED_TAIL}");
        let path = plugin_root.join(&relative);
        match is_regular_file_under(plugin_root, &path) {
            Ok(false) => continue,
            Err(message) => {
                findings.hard.push(shown(&message));
                continue;
            }
            Ok(true) => {}
        }
        match read_bytes(&path) {
            Ok(copy) if copy == canonical => {}
            Ok(_) => findings.hard.push(shown(&format!(
                "{relative}: DRIFTED from canonical {CANONICAL} — copy the canonical file over it \
                 (they must stay byte-identical)."
            ))),
            Err(error) => findings.hard.push(shown(&read_problem(&relative, &error))),
        }
    }
}

/// The handler with an injected date. [`model_staleness_command`] passes the
/// clock; tests pass a fixed [`Date`].
pub(crate) fn run_model_staleness(
    invocation: &Invocation,
    output: &mut Output<'_>,
    today: Date,
) -> Result<(), ToolkitError> {
    let plugin = resolve_lenient(Path::new(invocation.value("--plugin-root").unwrap_or(".")));
    if !plugin.is_dir() {
        return Err(ToolkitError::usage(
            "plugin_not_found",
            format!(
                "Plugin directory not found: {}",
                shown(&plugin.display().to_string())
            ),
        ));
    }
    let strict = invocation.has_flag("--strict");
    let mut findings = Findings::default();
    check_plugin(&plugin, today, &mut findings);

    for note in &findings.notes {
        output.line(&format!("  ok: {note}"))?;
    }
    for failure in &findings.hard {
        output.diagnostic(&format!("  ERROR: {failure}"));
    }
    for failure in &findings.stale {
        let label = if strict { "ERROR" } else { "WARN" };
        output.diagnostic(&format!("  {label}: {failure}"));
    }

    let problems = findings.hard.len() + if strict { findings.stale.len() } else { 0 };
    if problems > 0 {
        return Err(ToolkitError::runtime(
            "model_staleness_failed",
            format!("model-staleness: {problems} problem(s) — failing."),
        ));
    }
    if findings.stale.is_empty() {
        output.line("\nmodel-staleness: model registry fresh and in sync.")
    } else {
        output.line(&format!(
            "\nmodel-staleness: {} stale registry(ies) — warn-only (--strict fails on these).",
            findings.stale.len()
        ))
    }
}

/// Handler for `startup model-staleness`.
///
/// # Errors
///
/// A usage error (exit 2) for a missing plugin directory; a runtime error
/// (exit 1) when the registry is inconsistent, or stale under `--strict`.
pub fn model_staleness_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    run_model_staleness(invocation, output, Date::today_utc())
}

#[cfg(test)]
mod tests;
