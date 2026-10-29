//! `toolkit startup program-artifacts --migration-dir DIR`: when
//! `preferences.json` records `startup_program_status` as `"unknown"`, the
//! startup-program artifacts must not imply a confirmed AWS Activate tier and
//! must link to the Activate application page (port of
//! `validate-startup-program-artifacts.py`).

use std::path::{Path, PathBuf};

use toolkit_core::{neutralize_line, parse_json, Json, ToolkitError};

use super::text::{Pattern, Tok};
use super::{is_regular_file_under, read_text_under};
use crate::manifests::is_falsy;
use crate::{Invocation, Output};

/// Registration of the `startup-program` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-program",
    commands: &[crate::CommandSpec {
        path: &["startup", "program-artifacts"],
        flags: &[],
        value_flags: &["--migration-dir"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: crate::startup::program_artifacts_command,
    }],
    synopsis: r"  toolkit startup program-artifacts --migration-dir DIR
",
    description: r"  startup program-artifacts
                Check that STARTUP_PROGRAMS.md, ai-migration/STARTUP_PROGRAMS.md
                and migration-report.html in the migration directory do not
                imply a confirmed AWS Activate tier when preferences.json
                records startup_program_status as unknown, and that they link
                to the Activate application page when they mention it.
",
    exit_notes: "",
    doc: "docs/migration/startup-validators.md",
    synopsis_rank: 50,
    description_rank: 50,
};

/// Phrases that imply a confirmed tier: forbidden when the status is unknown.
static FORBIDDEN_WHEN_UNKNOWN: [Pattern; 6] = [
    Pattern::new(
        r"your\s+status:\s*eligible",
        &[
            Tok::Lit("your"),
            Tok::Space1,
            Tok::Lit("status:"),
            Tok::Space0,
            Tok::Lit("eligible"),
        ],
    ),
    Pattern::new(
        r"\beligible\s+founders\s+tier\b",
        &[
            Tok::Boundary,
            Tok::Lit("eligible"),
            Tok::Space1,
            Tok::Lit("founders"),
            Tok::Space1,
            Tok::Lit("tier"),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"\beligible\s+for\s+up\s+to\s+\$5k\s+aws\s+activate\b",
        &[
            Tok::Boundary,
            Tok::Lit("eligible"),
            Tok::Space1,
            Tok::Lit("for"),
            Tok::Space1,
            Tok::Lit("up"),
            Tok::Space1,
            Tok::Lit("to"),
            Tok::Space1,
            Tok::Lit("$5k"),
            Tok::Space1,
            Tok::Lit("aws"),
            Tok::Space1,
            Tok::Lit("activate"),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"\bactivate\s+portfolio\s+credits\s+eligibility\b",
        &[
            Tok::Boundary,
            Tok::Lit("activate"),
            Tok::Space1,
            Tok::Lit("portfolio"),
            Tok::Space1,
            Tok::Lit("credits"),
            Tok::Space1,
            Tok::Lit("eligibility"),
            Tok::Boundary,
        ],
    ),
    Pattern::new(
        r"\*\*your\s+status:\s*eligible_founders\*\*",
        &[
            Tok::Lit("**your"),
            Tok::Space1,
            Tok::Lit("status:"),
            Tok::Space0,
            Tok::Lit("eligible_founders**"),
        ],
    ),
    Pattern::new(
        r"\*\*your\s+status:\s*eligible_portfolio\*\*",
        &[
            Tok::Lit("**your"),
            Tok::Space1,
            Tok::Lit("status:"),
            Tok::Space0,
            Tok::Lit("eligible_portfolio**"),
        ],
    ),
];

const ACTIVATE_APPLY_URL: &str = "https://aws.amazon.com/startups/credits/";

/// `href=["']https?://aws\.amazon\.com/startups/credits/?["']`
const ACTIVATE_LINK: Pattern = Pattern::new(
    r#"href=["']https?://aws\.amazon\.com/startups/credits/?["']"#,
    &[
        Tok::Lit("href="),
        Tok::OneOf("\"'"),
        Tok::Lit("http"),
        Tok::Opt('s'),
        Tok::Lit("://aws.amazon.com/startups/credits"),
        Tok::Opt('/'),
        Tok::OneOf("\"'"),
    ],
);

/// `\baws\s+activate\b`
const ACTIVATE_MENTION: Pattern = Pattern::new(
    r"\baws\s+activate\b",
    &[
        Tok::Boundary,
        Tok::Lit("aws"),
        Tok::Space1,
        Tok::Lit("activate"),
        Tok::Boundary,
    ],
);

/// `preferences.json` blocks that may carry `startup_program_status`. All are
/// searched, in this order; the first block whose status is an object wins.
const STATUS_BLOCKS: [&str; 3] = [
    "startup_constraints",
    "ai_constraints",
    "design_constraints",
];

/// Handler for `startup program-artifacts --migration-dir DIR`.
///
/// # Errors
///
/// A usage error (exit status 2) without `--migration-dir`; a runtime error
/// (exit status 1) when the directory is missing, an input cannot be read or
/// parsed, or an artifact contradicts the recorded status.
pub fn program_artifacts_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(dir) = invocation.value("--migration-dir") else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "the following arguments are required: --migration-dir",
        ));
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        output.diagnostic(&format!(
            "STARTUP_FAIL | reason=not_a_directory | path={}",
            neutralize_line(&crate::safe_text::shown_path(&dir))
        ));
        return Err(ToolkitError::runtime(
            "startup_not_a_directory",
            format!(
                "migration directory is not a directory: {}",
                crate::safe_text::shown_path(&dir)
            ),
        ));
    }
    let errors = validate_startup_artifacts(&dir)
        .map_err(|message| ToolkitError::runtime("startup_input_invalid", message))?;
    if errors.is_empty() {
        return output.line("STARTUP_OK | startup_program_status aligned with artifacts");
    }
    output.diagnostic("STARTUP_FAIL | startup-program-artifacts");
    for error in &errors {
        output.diagnostic(&format!("  - {error}"));
    }
    Err(ToolkitError::runtime(
        "startup_artifacts_failed",
        format!("{} startup program artifact problem(s)", errors.len()),
    ))
}

/// The recorded `startup_program_status` value, or `None` when there is no
/// `preferences.json` (or it records no usable status).
fn load_status(migration_dir: &Path) -> Result<Option<Json>, String> {
    let path = migration_dir.join("preferences.json");
    if !is_regular_file_under(migration_dir, &path)? {
        return Ok(None);
    }
    let prefs = parse_json(&read_text_under(migration_dir, &path, "preferences.json")?)
        .map_err(|error| format!("preferences.json: {error}"))?;
    if !matches!(prefs, Json::Object(_)) {
        return Err(format!(
            "preferences.json: expected a JSON object, found {}",
            prefs.type_name()
        ));
    }
    for block_name in STATUS_BLOCKS {
        // `(prefs.get(name) or {}).get("startup_program_status")`
        let block = match prefs.get(block_name) {
            Some(value) if !is_falsy(value) => value,
            _ => continue,
        };
        if !matches!(block, Json::Object(_)) {
            return Err(format!(
                "preferences.json: {block_name} must be an object, found {}",
                block.type_name()
            ));
        }
        if let Some(status) = block
            .get("startup_program_status")
            .filter(|status| matches!(status, Json::Object(_)))
        {
            let value = match status.get("value") {
                Some(Json::Array(items)) => items.first(),
                other => other,
            };
            return Ok(value.filter(|value| **value != Json::Null).cloned());
        }
    }
    Ok(None)
}

/// Findings for one migration directory (empty when the status is absent or
/// anything but the string `"unknown"`), or the message of a failed read.
pub(crate) fn validate_startup_artifacts(migration_dir: &Path) -> Result<Vec<String>, String> {
    let Some(status) = load_status(migration_dir)? else {
        return Ok(Vec::new());
    };
    if status.as_str() != Some("unknown") {
        return Ok(Vec::new());
    }

    let mut texts: Vec<(String, String)> = Vec::new();
    for name in ["STARTUP_PROGRAMS.md", "migration-report.html"] {
        let path = migration_dir.join(name);
        if is_regular_file_under(migration_dir, &path)? {
            texts.push((
                name.to_string(),
                read_text_under(migration_dir, &path, name)?,
            ));
        }
    }
    let ai_label = "ai-migration/STARTUP_PROGRAMS.md";
    let ai_path = migration_dir
        .join("ai-migration")
        .join("STARTUP_PROGRAMS.md");
    if is_regular_file_under(migration_dir, &ai_path)? {
        texts.push((
            ai_label.to_string(),
            read_text_under(migration_dir, &ai_path, ai_label)?,
        ));
    }

    let mut errors = Vec::new();
    for (label, content) in &texts {
        if let Some(pattern) = FORBIDDEN_WHEN_UNKNOWN
            .iter()
            .find(|pattern| pattern.is_match(content))
        {
            errors.push(format!(
                "{label}: startup_program_status is \"unknown\" but content implies a \
                 confirmed Activate tier ({})",
                pattern.source()
            ));
        }
        if ACTIVATE_MENTION.is_match(content)
            && !ACTIVATE_LINK.is_match(content)
            && !content.contains(ACTIVATE_APPLY_URL)
        {
            errors.push(format!(
                "{label}: mentions AWS Activate but has no clickable link to \
                 {ACTIVATE_APPLY_URL} — add <a href=\"...\"> in HTML or [text](url) in Markdown"
            ));
        }
    }
    Ok(errors)
}
