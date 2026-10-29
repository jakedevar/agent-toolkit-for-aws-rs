//! `toolkit startup migration-report`: validates `migration-report.html`
//! completeness after the Generate phase (port of
//! `validate-migration-report.py`).
//!
//! It checks the required section ids, the table-of-contents anchors, the
//! minimum appendix content, the artifact-derived cost figures and the
//! customer-facing readability, accessibility and decision-core rules. The
//! Python `re` patterns run on `fancy-regex`; `HTMLParser` and
//! `html.unescape` are ported in [`html`]. See
//! `docs/migration/startup-validators.md` for the deliberate differences.

use std::path::Path;

use toolkit_core::{Json, ToolkitError};

use super::artifacts::pyjson::loads;
use super::{is_regular_file, read_text};
use crate::{Invocation, Output};

mod rx;

mod decision;
mod entities;
mod figures;
mod heroku;
mod html;
mod optimize;
mod structure;
mod style;
mod values;
mod walk;

/// Registration of the `startup-report` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-report",
    commands: &[
        crate::CommandSpec {
            path: &["startup", "migration-report"],
            flags: &["--no-require-toc", "--no-readability"],
            value_flags: &[
                "--estimation-infra",
                "--estimation-ai",
                "--aws-design",
                "--migration-dir",
                "--mode",
            ],
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: crate::startup::migration_report_command,
        },
        crate::CommandSpec {
            path: &["startup", "heroku-report"],
            flags: &[],
            value_flags: &["--migration-dir", "--mode"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: crate::startup::heroku_report_command,
        },
    ],
    synopsis: r"  toolkit startup migration-report REPORT [--estimation-infra FILE] [--estimation-ai FILE]
                            [--aws-design FILE] [--migration-dir DIR] [--no-require-toc]
                            [--no-readability] [--mode full|decision|ai_only]
  toolkit startup heroku-report REPORT [--migration-dir DIR] [--mode full|decision]
",
    description: r"  startup migration-report
                Validate a generated migration-report.html (or decision-report.html
                with --mode decision, or an AI-only report with --mode ai_only):
                required section ids, table-of-contents anchors, appendix depth,
                readability, accessibility, the visual shell, the data-cost-key
                figures against --estimation-infra, and the decision-core content.
                --estimation-ai and --aws-design add the checks that need those
                artifacts; --migration-dir enables fixture-bleed and what-if
                scenario checks. Problems are `  - ...` lines on stderr after
                REPORT_FAIL and exit status 1; a pass prints REPORT_OK.
  startup heroku-report
                Validate a heroku-to-aws migration-report.html (or decision-report.html
                with --mode decision): required sections, the draft-for-review
                footer, whole-dollar currency, the data-cost-key figure against
                estimation-infra.json in --migration-dir, cost-optimization content,
                the verdict headline and flip conditions, accessibility and the
                decision-mode phase state. Problems are `  - ...` lines on stderr
                after REPORT_FAIL and exit status 1; a pass prints REPORT_OK.
",
    exit_notes: "",
    doc: "crates/toolkit-cli/README.md",
    synopsis_rank: 120,
    description_rank: 110,
};

#[cfg(test)]
mod tests;

pub use heroku::heroku_report_command;

use rx::rx;
use structure::{
    AI_ONLY_REQUIRED_SECTION_IDS, DECISION_REQUIRED_SECTION_IDS, MIN_CONTENT_DEPTH,
    OPTIONAL_SECTION_IDS, REQUIRED_SECTION_IDS,
};
use walk::section_html;

/// Which report is being validated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// `migration-report.html`.
    Full,
    /// `decision-report.html` of the post-Estimate Decision gate: executive
    /// sections and the call to action, no appendices.
    Decision,
    /// A report for an app-code-only workload with no infrastructure track.
    AiOnly,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "decision" => Some(Self::Decision),
            "ai_only" => Some(Self::AiOnly),
            _ => None,
        }
    }

    fn required_ids(self) -> &'static [&'static str] {
        match self {
            Self::Full => &REQUIRED_SECTION_IDS,
            Self::Decision => &DECISION_REQUIRED_SECTION_IDS,
            Self::AiOnly => &AI_ONLY_REQUIRED_SECTION_IDS,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Decision => "decision",
            Self::AiOnly => "ai_only",
        }
    }
}

/// The parsed artifacts a report is checked against.
#[derive(Clone, Copy, Default)]
pub(super) struct Artifacts<'a> {
    pub(super) infra: Option<&'a Json>,
    pub(super) ai: Option<&'a Json>,
    pub(super) design: Option<&'a Json>,
}

/// The switches of one validation.
#[derive(Clone, Copy)]
pub(super) struct Options<'a> {
    pub(super) require_toc: bool,
    pub(super) check_readability: bool,
    pub(super) migration_dir: Option<&'a Path>,
    pub(super) mode: Mode,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            require_toc: true,
            check_readability: true,
            migration_dir: None,
            mode: Mode::Full,
        }
    }
}

/// Appendix sections must not be thin stubs.
fn appendix_depth(html: &str, infra: Option<&Json>) -> Vec<String> {
    let mut errors = Vec::new();
    for (section_id, min_depth) in MIN_CONTENT_DEPTH {
        let Some(section) = section_html(html, section_id) else {
            continue;
        };
        let mut floor = min_depth;
        if section_id == "appendix-services" {
            // A one-resource estate has one truthful row; a larger estate
            // keeps the stub floor.
            if let Some(accounted) = structure::accounted_service_count(infra) {
                if accounted < min_depth {
                    floor = accounted;
                }
            }
        }
        let depth = structure::content_depth(section_id, &section);
        let depth = i64::try_from(depth).unwrap_or(i64::MAX);
        if floor > 0 && depth < floor {
            errors.push(format!(
                "appendix section id={section_id} has insufficient content ({depth}), need >= \
                 {floor}"
            ));
        }
    }
    errors
}

/// The checks that apply to the readability of the report prose and layout.
fn readability_checks(html: &str, infra: Option<&Json>, options: &Options<'_>) -> Vec<String> {
    let mut errors = Vec::new();
    errors.extend(style::readability(html));
    errors.extend(style::currency_formatting(html));
    errors.extend(style::exec_vocabulary(html));
    errors.extend(structure::decision_language(html, infra));
    // The visual shell is enforced with the table-of-contents signal, which
    // keeps --no-require-toc as the escape hatch for minimal fixtures.
    if options.require_toc {
        errors.extend(style::visual_contract(html));
    }
    errors
}

/// The estimate-driven checks.
fn estimate_checks(html: &str, artifacts: &Artifacts<'_>, options: &Options<'_>) -> Vec<String> {
    let infra = artifacts.infra;
    let mut errors = Vec::new();
    if structure::security_baseline(infra).is_some() {
        if let Err(message) = structure::guardduty_or_baseline(html, infra) {
            errors.push(message);
        }
    }
    // The combined run-rate section is required only when BOTH estimates exist.
    if infra.is_some()
        && artifacts.ai.is_some()
        && structure::count_of(&structure::section_id_counts(html), "exec-tco") != 1
    {
        errors.push(
            "when both estimation-infra.json and estimation-ai.json exist, include exactly one \
             <section id=\"exec-tco\"> with the combined infra+AI estimated AWS monthly run rate"
                .to_string(),
        );
    }
    errors.extend(structure::security_teaser(html, infra));
    errors.extend(structure::verdict(html, infra));
    errors.extend(figures::cost_figures(html, infra, options.require_toc));
    errors.extend(structure::activate_link(html));
    if options.require_toc {
        errors.extend(structure::share_section(html, infra, artifacts.ai));
    }
    errors
}

/// Validates a report; the returned problems are empty when it passes.
#[allow(clippy::too_many_lines)]
pub(super) fn validate_report(
    html: &str,
    artifacts: &Artifacts<'_>,
    options: &Options<'_>,
) -> Vec<String> {
    let infra = artifacts.infra;
    let mode = options.mode;
    let required = mode.required_ids();
    let mut errors = structure::required_sections(html, required);

    if mode == Mode::Decision {
        // No Generate artifacts exist at decision time: appendices are forbidden.
        for (id, _) in structure::section_id_counts(html) {
            if id.starts_with("appendix-") {
                errors.push(format!(
                    "decision mode forbids <section id=\"{id}\"> — the decision report has no \
                     appendices; the full migration report (Generate) carries them"
                ));
            }
        }
    }
    if options.require_toc {
        if structure::toc_hrefs(html).is_empty() {
            errors.push("missing <nav class=\"toc\"> with href=\"#section-id\" links".to_string());
        }
        errors.extend(structure::toc_links(html, required));
        errors.extend(structure::decision_first(html));
    }
    if rx!(r"(?i)\[placeholder\]").is_match(html) {
        errors.push("forbidden content: placeholder text".to_string());
    }
    if rx!(r"(?i)\bTODO\b").is_match(html) {
        errors.push("forbidden content: TODO marker".to_string());
    }
    if options.check_readability {
        errors.extend(readability_checks(html, infra, options));
    }
    // Accessibility is independent of the prose checks: --no-readability
    // never disables it.
    if options.require_toc {
        errors.extend(style::accessibility(html));
    }
    errors.extend(appendix_depth(html, infra));
    for stub in [
        structure::appendix_stub(html),
        structure::services_stub(html),
    ] {
        if stub {
            errors.push(
                "appendix appears to be a stub (links to JSON only) — expand per \
                 generate-artifacts-report.md"
                    .to_string(),
            );
        }
    }
    if !html.to_lowercase().contains("draft for review") {
        errors.push("footer must contain \"draft for review\" disclaimer".to_string());
    }
    errors.extend(estimate_checks(html, artifacts, options));
    if matches!(mode, Mode::Full | Mode::AiOnly) && options.require_toc {
        errors.extend(structure::glossary_table(html));
    }
    errors.extend(structure::action_lists(html));
    errors.extend(structure::appendix_config(html));
    errors.extend(optimize::optimization_sections(
        html,
        infra,
        artifacts.ai,
        artifacts.design,
        mode,
        options.require_toc,
    ));
    if options.require_toc {
        errors.extend(decision::decision_core_render(
            html,
            infra,
            artifacts.design,
            mode,
            options.migration_dir,
        ));
    }
    // Catch verbatim copies of the reference fixture into a real run.
    errors.extend(style::fixture_bleed(html, options.migration_dir));
    // The what-if workshop table is required when >=2 scenarios were snapshotted.
    let counts = structure::section_id_counts(html);
    if decision::scenario_count(options.migration_dir) >= 2
        && structure::count_of(&counts, "what-if-scenarios") < 1
    {
        errors.push(
            "scenarios/index.json has ≥2 scenarios but no <section id=\"what-if-scenarios\"> \
             (workshop compare table is required in the migration report when variants exist)"
                .to_string(),
        );
    }
    errors
}

/// An optional artifact: `None` when the flag is absent or the path is not a
/// file; a parse failure is an error.
fn load_artifact(path: Option<&str>, shown: &str) -> Result<Option<Json>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let path = Path::new(path);
    if !is_regular_file(path)? {
        return Ok(None);
    }
    let text = read_text(path, shown)?;
    let value = loads(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    match value {
        Json::Null => Ok(None),
        Json::Object(_) => Ok(Some(value)),
        other => Err(format!(
            "{}: expected a JSON object, found {}",
            path.display(),
            other.type_name()
        )),
    }
}

/// Handler for `startup migration-report REPORT [options]`.
///
/// # Errors
///
/// A usage error (exit status 2) without the report path or with an unknown
/// `--mode`; a runtime error (exit status 1) when the report is missing, an
/// input cannot be read or parsed, or the report fails validation.
pub fn migration_report_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(report) = invocation.positional(0) else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "the following arguments are required: report_path",
        ));
    };
    let mode = match invocation.value("--mode") {
        None => Mode::Full,
        Some(value) => Mode::parse(value).ok_or_else(|| {
            ToolkitError::usage(
                "invalid_argument",
                format!(
                    "argument --mode: invalid choice: '{}' (choose from 'full', \
                     'decision', 'ai_only')",
                    crate::safe_text::shown_value(value)
                ),
            )
        })?,
    };
    let report_path = Path::new(report);
    let is_file = is_regular_file(report_path)
        .map_err(|message| ToolkitError::runtime("report_unreadable", message))?;
    if !is_file {
        output.diagnostic(&format!("REPORT_FAIL | file={report} | reason=not_found"));
        return Err(ToolkitError::runtime(
            "report_not_found",
            format!("report not found: {report}"),
        ));
    }
    let html = read_text(report_path, report)
        .map_err(|message| ToolkitError::runtime("report_unreadable", message))?;
    let load = |flag: &str| {
        load_artifact(invocation.value(flag), flag)
            .map_err(|message| ToolkitError::runtime("artifact_unreadable", message))
    };
    let infra = load("--estimation-infra")?;
    let ai = load("--estimation-ai")?;
    let design = load("--aws-design")?;
    let artifacts = Artifacts {
        infra: infra.as_ref(),
        ai: ai.as_ref(),
        design: design.as_ref(),
    };
    let options = Options {
        require_toc: !invocation.has_flag("--no-require-toc"),
        check_readability: !invocation.has_flag("--no-readability"),
        migration_dir: invocation.value("--migration-dir").map(Path::new),
        mode,
    };
    let errors = validate_report(&html, &artifacts, &options);
    if !errors.is_empty() {
        output.diagnostic("REPORT_FAIL | migration-report.html");
        for error in &errors {
            output.diagnostic(&format!("  - {error}"));
        }
        return Err(ToolkitError::runtime(
            "report_failed",
            format!("{} report problem(s)", errors.len()),
        ));
    }
    output.line(&success_line(&html, mode))
}

/// The `REPORT_OK` line.
fn success_line(html: &str, mode: Mode) -> String {
    let counts = structure::section_id_counts(html);
    let optional: Vec<&str> = OPTIONAL_SECTION_IDS
        .iter()
        .copied()
        .filter(|id| structure::count_of(&counts, id) >= 1)
        .collect();
    let required = mode.required_ids().len();
    let mode_tag = if mode == Mode::Full {
        String::new()
    } else {
        format!("mode={} | ", mode.name())
    };
    let optional_part = if optional.is_empty() {
        String::new()
    } else {
        format!(" | optional={}", optional.join(","))
    };
    format!(
        "REPORT_OK | {mode_tag}structure=complete | sections={required}/{required}{optional_part} \
         | note=verify dollar figures against estimation JSON before sign-off"
    )
}
