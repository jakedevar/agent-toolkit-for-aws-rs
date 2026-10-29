//! Payload templates of the `aws-ai-ml` skill (#4386, slice S1 of Issue #5143).
//!
//! `toolkit ai-payloads templates [--root DIR] [--json] [--check]` lists the 23
//! `references/*/code_templates/*.py` files with their cells, placeholders,
//! assignable literal fields and manual edit points, and lints them against the
//! template grammar of `docs/migration/ai-payloads-design.md` section 4.3.
//!
//! This slice reads and never writes: there is no render, no file publication
//! and no value substitution yet (slices S2a and S2b). The template text stays
//! Python because the customer's own kernel runs it (design section 3); the
//! files are read as text and never executed. See `docs/migration/ai-payloads.md`
//! for the command contract and the JSON catalog.

use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{
    neutralize_line, neutralize_text, read_regular_file, Json, ToolkitError, MAX_TEXT_FILE_BYTES,
};

use crate::safe_text::safe_path;
use crate::{Invocation, Output};

mod place;
mod template;
mod types;

use template::Template;

/// Registration of the `ai-payloads` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "ai-payloads",
    commands: &[crate::CommandSpec {
        path: &["ai-payloads", "templates"],
        flags: TEMPLATES_FLAGS,
        value_flags: TEMPLATES_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: templates_command,
    }],
    synopsis: "  toolkit ai-payloads templates [--root DIR] [--json] [--check]\n",
    description: r"  ai-payloads   Payload templates of the AWS AI/ML skill (#4386).
                templates lists the code_templates/*.py files under
                ROOT/references (default: skills/core-skills/aws-ai-ml) with
                their cells, placeholders and literal fields, and --check
                lints the template grammar (exit 1 on a violation). It reads
                templates and writes nothing. See
                docs/migration/ai-payloads.md.
",
    exit_notes: "",
    doc: "docs/migration/ai-payloads.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// Flags of `ai-payloads templates`.
pub(crate) const TEMPLATES_FLAGS: &[&str] = &["--json", "--check"];
/// Value flags of `ai-payloads templates`.
pub(crate) const TEMPLATES_VALUE_FLAGS: &[&str] = &["--root"];

/// The skill directory searched when `--root` is absent, relative to the
/// working directory (the repository root).
const DEFAULT_ROOT: &str = "skills/core-skills/aws-ai-ml";

/// Most templates one run reads; the real tree holds 23.
const MAX_TEMPLATES: usize = 512;

/// One template of the catalog.
struct Entry {
    /// Path relative to `references/`, `/`-separated.
    path: String,
    template: Template,
}

fn failure(code: &'static str, message: String) -> ToolkitError {
    ToolkitError::runtime(code, message)
}

fn root_failure(root: &str, why: &str) -> ToolkitError {
    failure(
        "ai_payloads_root",
        format!(
            "{why}: {}; pass --root DIR, the skill directory that holds references/",
            safe_path(root)
        ),
    )
}

/// The entries of `directory` sorted by name, as `(name, path)`.
fn sorted_entries(directory: &Path) -> Result<Vec<(String, PathBuf)>, ToolkitError> {
    let unreadable = |error: std::io::Error| {
        failure(
            "ai_payloads_read",
            format!(
                "cannot read {}: {error}",
                safe_path(&directory.display().to_string())
            ),
        )
    };
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let name = entry.file_name().into_string().map_err(|name| {
            failure(
                "ai_payloads_read",
                format!(
                    "a name under {} is not valid UTF-8: {}",
                    safe_path(&directory.display().to_string()),
                    safe_path(&name.to_string_lossy())
                ),
            )
        })?;
        entries.push((name, entry.path()));
    }
    entries.sort();
    Ok(entries)
}

fn refuse_symlink(path: &Path) -> ToolkitError {
    failure(
        "ai_payloads_symlink",
        format!(
            "refusing to follow symbolic link: {}",
            safe_path(&path.display().to_string())
        ),
    )
}

/// Finds `ROOT/references/*/code_templates/*.py`, refusing symbolic links on
/// the directories it descends through. Returns `(relative path, path)` sorted
/// by relative path.
fn discover(root: &str) -> Result<Vec<(String, PathBuf)>, ToolkitError> {
    if !fs::metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(root_failure(root, "template root is not a directory"));
    }
    let references = Path::new(root).join("references");
    if !fs::symlink_metadata(&references).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(root_failure(
            root,
            "no references/ directory under the template root",
        ));
    }
    let mut found = Vec::new();
    for (name, directory) in sorted_entries(&references)? {
        let Ok(metadata) = fs::symlink_metadata(&directory) else {
            continue;
        };
        if metadata.is_symlink() {
            if fs::metadata(&directory).is_ok_and(|target| target.is_dir()) {
                return Err(refuse_symlink(&directory));
            }
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let templates = directory.join("code_templates");
        match fs::symlink_metadata(&templates) {
            Ok(metadata) if metadata.is_symlink() => return Err(refuse_symlink(&templates)),
            Ok(metadata) if metadata.is_dir() => {}
            _ => continue,
        }
        for (file, path) in sorted_entries(&templates)? {
            if Path::new(&file)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
            {
                found.push((format!("{name}/code_templates/{file}"), path));
            }
        }
    }
    found.sort();
    if found.len() > MAX_TEMPLATES {
        return Err(failure(
            "ai_payloads_read",
            format!("more than {MAX_TEMPLATES} templates under the root"),
        ));
    }
    Ok(found)
}

/// Text of the catalog that is safe on a terminal and in a log: unsafe
/// characters of template-controlled text (labels, titles, file names) become
/// `\uXXXX` escapes.
fn safe(text: &str) -> String {
    neutralize_line(text).into_owned()
}

fn count(value: usize) -> Json {
    Json::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

fn cell_json(cell: &template::Cell) -> Json {
    let mut pairs = vec![
        ("number", count(cell.number)),
        ("label", Json::string(safe(&cell.label))),
        ("markdown", Json::Bool(cell.markdown)),
        (
            "notebook_only_section",
            Json::Bool(cell.notebook_only_section),
        ),
        ("line", count(cell.line)),
        ("lines", count(cell.lines)),
        ("notebook_only_lines", count(cell.notebook_only_lines)),
    ];
    if let Some(text) = &cell.text {
        pairs.push(("text", Json::string(neutralize_text(text).into_owned())));
    }
    Json::object(pairs)
}

fn slot_json(slot: &template::Slot) -> Json {
    Json::object(vec![
        ("name", Json::string(slot.name.as_str())),
        ("kind", Json::string(slot.kind.name())),
        ("code", count(slot.code)),
        ("comment_only", Json::Bool(slot.comment_only())),
        ("sentinel", count(slot.sentinel)),
    ])
}

fn occurrence_json(occurrence: &template::Occurrence) -> Json {
    let mut pairs = vec![
        ("name", Json::string(occurrence.name.as_str())),
        ("context", Json::string(occurrence.context.name())),
        ("line", count(occurrence.line)),
        ("column", count(occurrence.column)),
    ];
    if let Some(quote) = occurrence.quote {
        pairs.push(("quote", Json::string(quote.to_string())));
    }
    Json::object(pairs)
}

fn target_json(target: &template::Target) -> Json {
    Json::object(vec![
        ("name", Json::string(target.name.as_str())),
        ("cell", count(target.cell)),
        ("line", count(target.line)),
        ("kind", Json::string(target.kind.name())),
        ("literal", Json::string(safe(&target.literal))),
    ])
}

fn manual_json(edit: &template::ManualEdit) -> Json {
    Json::object(vec![
        ("cell", edit.cell.map_or(Json::Null, count)),
        ("line", count(edit.line)),
        ("text", Json::string(safe(&edit.text))),
    ])
}

fn violation_json(violation: &template::Violation) -> Json {
    Json::object(vec![
        ("code", Json::string(violation.code)),
        ("line", count(violation.line)),
        ("column", count(violation.column)),
        ("message", Json::string(safe(&violation.message))),
    ])
}

fn entry_json(entry: &Entry) -> Json {
    let template = &entry.template;
    Json::object(vec![
        ("path", Json::string(safe(&entry.path))),
        ("title", Json::string(safe(&template.title))),
        (
            "cells",
            Json::Array(template.cells.iter().map(cell_json).collect()),
        ),
        (
            "placeholders",
            Json::Array(template.slots.iter().map(slot_json).collect()),
        ),
        (
            "occurrences",
            Json::Array(template.occurrences.iter().map(occurrence_json).collect()),
        ),
        (
            "targets",
            Json::Array(template.targets.iter().map(target_json).collect()),
        ),
        (
            "manual_edits",
            Json::Array(template.manual_edits.iter().map(manual_json).collect()),
        ),
        (
            "violations",
            Json::Array(template.violations.iter().map(violation_json).collect()),
        ),
    ])
}

fn write_listing(entries: &[Entry], output: &mut Output<'_>) -> Result<(), ToolkitError> {
    for entry in entries {
        let template = &entry.template;
        output.line(&safe(&entry.path))?;
        output.line(&format!("  title: {}", safe(&template.title)))?;
        for cell in &template.cells {
            output.line(&format!(
                "  cell {}{}: {}{}",
                cell.number,
                if cell.markdown { " markdown" } else { "" },
                safe(&cell.label),
                if cell.notebook_only_section {
                    " [NOTEBOOK_ONLY_SECTION]"
                } else {
                    ""
                }
            ))?;
        }
        for slot in &template.slots {
            output.line(&format!(
                "  slot {} kind={} code={} sentinel={}{}",
                slot.name,
                slot.kind.name(),
                slot.code,
                slot.sentinel,
                if slot.comment_only() {
                    " comment-only"
                } else {
                    ""
                }
            ))?;
        }
        for target in &template.targets {
            output.line(&format!(
                "  target {} cell={} line={} kind={} literal={}",
                target.name,
                target.cell,
                target.line,
                target.kind.name(),
                safe(&target.literal)
            ))?;
        }
        for edit in &template.manual_edits {
            let cell = edit
                .cell
                .map_or_else(String::new, |cell| format!(" cell={cell}"));
            output.line(&format!(
                "  manual{cell} line={}: {}",
                edit.line,
                safe(&edit.text)
            ))?;
        }
        for violation in &template.violations {
            output.line(&format!(
                "  violation {} line={} column={}: {}",
                violation.code,
                violation.line,
                violation.column,
                safe(&violation.message)
            ))?;
        }
        output.line("")?;
    }
    output.line(&format!("templates: {}", entries.len()))
}

fn write_check(entries: &[Entry], output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let mut total = 0;
    let mut failing = 0;
    for entry in entries {
        let violations = &entry.template.violations;
        total += violations.len();
        failing += usize::from(!violations.is_empty());
        for violation in violations {
            output.line(&format!(
                "{}:{}:{}: {}: {}",
                safe(&entry.path),
                violation.line,
                violation.column,
                violation.code,
                safe(&violation.message)
            ))?;
        }
    }
    if entries.is_empty() {
        output.line("check failed: no templates found under references/*/code_templates")
    } else if total == 0 {
        output.line(&format!(
            "ok: {} templates satisfy the template grammar",
            entries.len()
        ))
    } else {
        output.line(&format!(
            "check failed: {total} violations in {failing} of {} templates",
            entries.len()
        ))
    }
}

/// `toolkit ai-payloads templates`.
pub(crate) fn templates_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let root = invocation.value("--root").unwrap_or(DEFAULT_ROOT);
    let mut entries = Vec::new();
    for (relative, path) in discover(root)? {
        let text = read_regular_file(&path, MAX_TEXT_FILE_BYTES)
            .map_err(|error| failure("ai_payloads_read", error.to_string()))?;
        entries.push(Entry {
            path: relative,
            template: template::parse(&text),
        });
    }
    let clean = !entries.is_empty()
        && entries
            .iter()
            .all(|entry| entry.template.violations.is_empty());
    let check = invocation.has_flag("--check");
    if invocation.has_flag("--json") {
        let catalog = Json::object(vec![
            ("ok", Json::Bool(clean)),
            (
                "templates",
                Json::Array(entries.iter().map(entry_json).collect()),
            ),
        ]);
        output.line(&catalog.to_pretty_string())?;
    } else if check {
        write_check(&entries, output)?;
    } else {
        write_listing(&entries, output)?;
    }
    if check && !clean {
        output.set_exit_status(1);
    }
    Ok(())
}
