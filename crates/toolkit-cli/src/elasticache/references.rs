//! `toolkit elasticache validate-references` (port of `validate_references.py`):
//! checks that the skill's reference files carry the required metadata and
//! follow the project conventions, and that its legacy scripts keep a module
//! docstring and a command-line entrypoint.
//!
//! The walk is deterministic (names are sorted), never descends into or
//! follows a symbolic link, and reads files through the shared bounded,
//! symlink-refusing reader.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use toolkit_core::{read_regular_file, PathError, MAX_TEXT_FILE_BYTES};

use super::text::show;

/// Fewest non-whitespace characters, after the title line, of a non-empty
/// reference file.
pub const MIN_CONTENT_CHARS: usize = 50;

/// Severity of one result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The check passed.
    Pass,
    /// Worth a look, not a failure.
    Warn,
    /// The check failed.
    Fail,
}

impl Level {
    /// `PASS`, `WARN` or `FAIL`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

/// One validation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Severity.
    pub level: Level,
    /// Path relative to the skill directory.
    pub path: String,
    /// What was checked.
    pub message: String,
}

impl CheckResult {
    fn new(level: Level, path: &str, message: impl Into<String>) -> Self {
        Self {
            level,
            path: path.to_string(),
            message: message.into(),
        }
    }

    /// `[LEVEL] path: message`, with the untrusted parts escaped.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "[{}] {}: {}",
            self.level.label(),
            show(&self.path),
            self.message
        )
    }
}

/// Whether `name` is `lowercase-kebab.md`.
#[must_use]
pub fn is_kebab_markdown(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    !stem.is_empty()
        && stem.split('-').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn is_ref_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-'
}

/// Every `` `references/<path>.md` `` mentioned in backticks in `content`.
#[must_use]
pub fn skill_md_references(content: &str) -> BTreeSet<String> {
    let chars: Vec<char> = content.chars().collect();
    let prefix: Vec<char> = "references/".chars().collect();
    let suffix: Vec<char> = ".md`".chars().collect();
    let mut found = BTreeSet::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '`' {
            if let Some((path, next)) = match_reference(&chars, index + 1, &prefix, &suffix) {
                // The match consumes the closing backtick too.
                found.insert(path);
                index = next;
                continue;
            }
        }
        index += 1;
    }
    found
}

/// The `references/...md` at `start` when a backtick closes it; also returns
/// the index after the closing backtick.
fn match_reference(
    chars: &[char],
    start: usize,
    prefix: &[char],
    suffix: &[char],
) -> Option<(String, usize)> {
    if !chars[start..].starts_with(prefix) {
        return None;
    }
    let body_start = start + prefix.len();
    let mut end = body_start;
    while end < chars.len() && is_ref_char(chars[end]) {
        end += 1;
    }
    if end == body_start || !chars[end..].starts_with(suffix) {
        return None;
    }
    let mut path: String = chars[start..end].iter().collect();
    path.push_str(".md");
    Some((path, end + suffix.len()))
}

fn is_python_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

fn read_text(path: &Path) -> Result<String, PathError> {
    read_regular_file(path, MAX_TEXT_FILE_BYTES)
}

/// Validates one reference file (`rel` is its display path).
#[must_use]
pub fn validate_reference_file(
    path: &Path,
    rel: &str,
    file_name: &str,
    skill_refs: &BTreeSet<String>,
) -> Vec<CheckResult> {
    let mut results = Vec::new();
    if is_kebab_markdown(file_name) {
        results.push(CheckResult::new(
            Level::Pass,
            rel,
            "Filename follows kebab-case convention",
        ));
    } else {
        results.push(CheckResult::new(
            Level::Fail,
            rel,
            format!(
                "Filename '{}' does not follow kebab-case convention (expected lowercase letters, digits, and hyphens only)",
                show(file_name)
            ),
        ));
    }

    let content = match read_text(path) {
        Ok(content) => content,
        Err(error) => {
            results.push(CheckResult::new(
                Level::Fail,
                rel,
                format!("Could not read file: {}", show(&error.to_string())),
            ));
            return results;
        }
    };

    let lines: Vec<&str> = content.split('\n').collect();
    let Some(first_line) = lines.iter().find(|line| !line.trim().is_empty()) else {
        results.push(CheckResult::new(
            Level::Fail,
            rel,
            "File is completely empty",
        ));
        return results;
    };
    let first_line = first_line.trim();
    if first_line.starts_with('#') {
        let title: String = first_line.chars().take(60).collect();
        results.push(CheckResult::new(
            Level::Pass,
            rel,
            format!("Has title: {}", show(&title)),
        ));
    } else {
        results.push(CheckResult::new(
            Level::Fail,
            rel,
            "Missing title -- first non-blank line should start with '#'",
        ));
    }

    let body = if lines.len() > 1 {
        lines[1..].join("\n")
    } else {
        String::new()
    };
    let body_chars = body.chars().filter(|c| !is_python_space(*c)).count();
    if body_chars < MIN_CONTENT_CHARS {
        results.push(CheckResult::new(
            Level::Fail,
            rel,
            format!(
                "File body has only {body_chars} non-whitespace chars (minimum {MIN_CONTENT_CHARS}) -- appears empty or stub"
            ),
        ));
    } else {
        results.push(CheckResult::new(
            Level::Pass,
            rel,
            format!("File has meaningful content ({body_chars} body chars)"),
        ));
    }

    if skill_refs.contains(rel) {
        results.push(CheckResult::new(Level::Pass, rel, "Referenced in SKILL.md"));
    } else {
        results.push(CheckResult::new(
            Level::Warn,
            rel,
            "Not referenced in SKILL.md",
        ));
    }
    results
}

fn sorted_entries(dir: &Path) -> std::io::Result<Vec<(String, bool)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            // A link to a directory is never followed; any other link is
            // listed as a file so reading it reports the refusal.
            if fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            entries.push((name, false));
        } else {
            entries.push((name, file_type.is_dir()));
        }
    }
    entries.sort();
    Ok(entries)
}

fn walk_references(
    dir: &Path,
    rel_dir: &str,
    skill_refs: &BTreeSet<String>,
    results: &mut Vec<CheckResult>,
    count: &mut usize,
) -> std::io::Result<()> {
    let entries = sorted_entries(dir)?;
    for (name, is_dir) in &entries {
        if *is_dir || !name.ends_with(".md") {
            continue;
        }
        *count += 1;
        let rel = format!("{rel_dir}/{name}");
        results.extend(validate_reference_file(
            &dir.join(name),
            &rel,
            name,
            skill_refs,
        ));
    }
    for (name, is_dir) in &entries {
        if *is_dir {
            walk_references(
                &dir.join(name),
                &format!("{rel_dir}/{name}"),
                skill_refs,
                results,
                count,
            )?;
        }
    }
    Ok(())
}

/// Walks `references/` and validates every `.md` file. Returns the results and
/// the number of files scanned.
#[must_use]
pub fn validate_all_references(skill_dir: &Path) -> (Vec<CheckResult>, usize) {
    let references_dir = skill_dir.join("references");
    let mut results = Vec::new();
    let skill_refs = match read_text(&skill_dir.join("SKILL.md")) {
        Ok(content) => skill_md_references(&content),
        Err(PathError::NotFound(_)) => BTreeSet::new(),
        Err(error) => {
            results.push(CheckResult::new(
                Level::Fail,
                "SKILL.md",
                format!("Could not read file: {}", show(&error.to_string())),
            ));
            BTreeSet::new()
        }
    };

    let is_dir = fs::symlink_metadata(&references_dir).is_ok_and(|meta| meta.is_dir());
    if !is_dir {
        results.push(CheckResult::new(
            Level::Fail,
            "references/",
            "references/ directory not found",
        ));
        return (results, 0);
    }

    let mut count = 0;
    if let Err(error) = walk_references(
        &references_dir,
        "references",
        &skill_refs,
        &mut results,
        &mut count,
    ) {
        results.push(CheckResult::new(
            Level::Fail,
            "references/",
            format!("Could not list references: {}", show(&error.to_string())),
        ));
    }
    if count == 0 {
        results.push(CheckResult::new(
            Level::Fail,
            "references/",
            "No .md files found under references/",
        ));
    }
    (results, count)
}

/// Validates one legacy `.py` script: a module docstring in the first 30 lines
/// and an `if __name__ == "__main__"` entrypoint.
#[must_use]
pub fn validate_script_file(path: &Path, rel: &str) -> Vec<CheckResult> {
    let content = match read_text(path) {
        Ok(content) => content,
        Err(error) => {
            return vec![CheckResult::new(
                Level::Fail,
                rel,
                format!("Could not read file: {}", show(&error.to_string())),
            )];
        }
    };
    let mut results = Vec::new();
    let top_chunk: String = content.split('\n').take(30).collect::<Vec<_>>().join("\n");
    if top_chunk.contains("\"\"\"") || top_chunk.contains("'''") {
        results.push(CheckResult::new(Level::Pass, rel, "Has module docstring"));
    } else {
        results.push(CheckResult::new(
            Level::Fail,
            rel,
            "Missing module docstring at top of file",
        ));
    }
    let has_main = content.contains("if __name__")
        && (content.contains("\"__main__\"") || content.contains("'__main__'"));
    if has_main {
        results.push(CheckResult::new(Level::Pass, rel, "Has CLI entrypoint"));
    } else {
        results.push(CheckResult::new(
            Level::Warn,
            rel,
            "No 'if __name__ == \"__main__\"' block -- OK if this is a library, not a CLI tool",
        ));
    }
    results
}

/// Validates every `.py` file directly under `scripts/`. Returns the results
/// and the number of files scanned.
#[must_use]
pub fn validate_all_scripts(skill_dir: &Path) -> (Vec<CheckResult>, usize) {
    let scripts_dir = skill_dir.join("scripts");
    let is_dir = fs::symlink_metadata(&scripts_dir).is_ok_and(|meta| meta.is_dir());
    if !is_dir {
        return (
            vec![CheckResult::new(
                Level::Fail,
                "scripts/",
                "scripts/ directory not found",
            )],
            0,
        );
    }
    let mut names: Vec<String> = match fs::read_dir(&scripts_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".py"))
            .collect(),
        Err(error) => {
            return (
                vec![CheckResult::new(
                    Level::Fail,
                    "scripts/",
                    format!("Could not list scripts: {}", show(&error.to_string())),
                )],
                0,
            );
        }
    };
    names.sort();
    let mut results = Vec::new();
    for name in &names {
        results.extend(validate_script_file(
            &scripts_dir.join(name),
            &format!("scripts/{name}"),
        ));
    }
    if names.is_empty() {
        results.push(CheckResult::new(
            Level::Warn,
            "scripts/",
            "No .py files found under scripts/",
        ));
    }
    (results, names.len())
}

/// Counts and printable lines of a validation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Lines for stdout, in order.
    pub lines: Vec<String>,
    /// Number of FAIL results.
    pub failures: usize,
}

/// Renders the results the way the script printed them. `verbose` adds the
/// PASS results; `quiet` leaves only the summary line.
#[must_use]
pub fn render_results(results: &[CheckResult], verbose: bool, quiet: bool) -> Report {
    let count = |level: Level| results.iter().filter(|r| r.level == level).count();
    let (fails, warns, passes) = (count(Level::Fail), count(Level::Warn), count(Level::Pass));
    let mut lines = Vec::new();
    let mut section = |title: &str, level: Level| {
        lines.push(String::new());
        lines.push(format!("--- {title} ---"));
        for result in results.iter().filter(|r| r.level == level) {
            lines.push(format!("  {}", result.render()));
        }
    };
    if !quiet {
        if fails > 0 {
            section("FAILURES", Level::Fail);
        }
        if warns > 0 {
            section("WARNINGS", Level::Warn);
        }
        if verbose && passes > 0 {
            section("PASSED", Level::Pass);
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "Summary: {passes} passed, {warns} warnings, {fails} failures"
    ));
    Report {
        lines,
        failures: fails,
    }
}
