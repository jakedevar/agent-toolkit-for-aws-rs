//! `toolkit startup heroku-report`: validates the thin stakeholder report of
//! the heroku-to-aws skill (port of `validate-heroku-migration-report.py` and
//! its three test files).
//!
//! Required sections, the footer disclaimer, whole-dollar currency formatting,
//! the `data-cost-key` figure cross-check, the cost-optimization content, the
//! typography-first verdict, accessibility and the decision-mode state rules.
//! It shares the tokenizer, the Python word-class adapter, the cost-figure
//! cross-check, the currency patterns and the phrase matching with the full
//! validator of [`super`]; only the structure scanners that differ live in
//! [`parsers`]. See `docs/migration/heroku.md` for the deliberate differences.

use std::path::Path;

use toolkit_core::{Json, ToolkitError};

use super::decision::{rendered_list_after_heading, scenario_len, unmatched_phrases_by};
use super::figures::{check_cost_figures, CostAnchor};
use super::rx::{group, rx};
use super::style::{
    cents_pattern, chars_after, rate_suffix_pattern, readability_scope,
    whole_dollars_at_least_threshold,
};
use super::values::{chain, escaped, truthy, untrusted};
use super::walk::{normalize_phrase, rendered_phrases};
use crate::manifests::{is_py_space, py_strip};
use crate::startup::artifacts::pyjson::loads;
use crate::startup::{is_regular_file, is_regular_file_under, read_text, read_text_under};
use crate::{Invocation, Output};

mod parsers;

#[cfg(test)]
mod tests;

use parsers::{
    accessibility_findings, class_tokens, decoded_text, has_populated_opportunity_row,
    html_lang_declared, parse_sections, Sections,
};

/// Sections required in both report modes. `cost-optimization` is full mode
/// only: the decision pack is pre-execution and carries no optimization table.
const COMMON_REQUIRED_SECTION_IDS: [&str; 2] = ["decision-summary", "exec-costs"];

/// The sentence a report renders when no commitment product applies.
const NO_ELIGIBLE_COMMITMENT_SENTENCE: &str =
    "no 1-year/3-year commitment product applies to this architecture";

/// The one anchor the Heroku report asserts: the recommended AWS monthly
/// (Balanced) figure.
const COST_ANCHORS: [CostAnchor; 1] = [(
    "aws_monthly_balanced",
    ["projected_costs", "aws_monthly_balanced"],
)];

/// Anchors required inside `exec-costs` when the estimate carries the figure.
const REQUIRED_COST_KEYS: [&str; 1] = ["aws_monthly_balanced"];

/// Which report is being validated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// `migration-report.html`: ends on a next-steps list and carries the
    /// cost-optimization table.
    Full,
    /// `decision-report.html`: ends on a decision call to action pointing at
    /// Generate.
    Decision,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "decision" => Some(Self::Decision),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Decision => "decision",
        }
    }

    /// The structural difference between the modes.
    fn terminal_section(self) -> &'static str {
        match self {
            Self::Full => "next-steps",
            Self::Decision => "decision-cta",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Full => Self::Decision,
            Self::Decision => Self::Full,
        }
    }

    /// `_required_sections`.
    fn required_sections(self) -> Vec<&'static str> {
        let mut required = COMMON_REQUIRED_SECTION_IDS.to_vec();
        required.push(self.terminal_section());
        if self == Self::Full {
            required.push("cost-optimization");
        }
        required
    }
}

// ---------------------------------------------------------------------------
// Artifact reads
// ---------------------------------------------------------------------------

/// What reading one JSON artifact of the migration directory produced.
enum Artifact {
    /// No such regular file (or no migration directory).
    Absent,
    /// Present but unreadable: a symbolic link, an I/O error, invalid UTF-8
    /// or an oversized file. Python would follow the link or crash; the
    /// validator refuses to read it and says so.
    Unreadable(String),
    /// Present but not valid JSON.
    Corrupt(String),
    /// The parsed document.
    Json(Json),
}

impl Artifact {
    fn json(&self) -> Option<&Json> {
        match self {
            Self::Json(value) => Some(value),
            _ => None,
        }
    }
}

/// Reads `name` below the migration directory, never following a link.
fn read_artifact(dir: Option<&Path>, name: &str) -> Artifact {
    let Some(dir) = dir else {
        return Artifact::Absent;
    };
    let path = dir.join(name);
    match is_regular_file_under(dir, &path) {
        Ok(false) => return Artifact::Absent,
        Err(message) => return Artifact::Unreadable(message),
        Ok(true) => {}
    }
    let text = match read_text_under(dir, &path, name) {
        Ok(text) => text,
        Err(message) => return Artifact::Unreadable(message),
    };
    match loads(&text) {
        Ok(value) => Artifact::Json(value),
        Err(error) => Artifact::Corrupt(error.to_string()),
    }
}

/// `(est or {}).get("recommendation") or {}` as an object, else nothing.
fn recommendation(estimate: Option<&Json>) -> Option<&Json> {
    chain(estimate?, &["recommendation"]).filter(|rec| matches!(rec, Json::Object(_)))
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

/// Cost figures against `estimation-infra.json` (P1-C).
fn cost_figures(html: &str, sections: &Sections, estimate: Option<&Json>) -> Vec<String> {
    check_cost_figures(
        html,
        estimate,
        &COST_ANCHORS,
        &REQUIRED_COST_KEYS,
        sections.html("exec-costs"),
    )
}

/// Monthly cost figures must render as whole dollars: flags any `$X.YY` whose
/// whole-dollar part is at least two and that is not followed by a per-unit
/// rate suffix (`/hr`, `per policy`, ...).
fn currency_formatting(html: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let decoded = decoded_text(&readability_scope(html));
    let text = decoded.text();
    let mut seen: Vec<String> = Vec::new();
    for groups in cents_pattern().captures_all(&text) {
        let Some((start, end)) = groups.first().copied().flatten() else {
            continue;
        };
        if !whole_dollars_at_least_threshold(group(&text, &groups, 1)) {
            continue;
        }
        // The suffix window stops at the next block-level boundary.
        let mut cutoff = chars_after(&text, end, 25);
        if let Some(boundary) = decoded
            .boundaries()
            .iter()
            .find(|boundary| **boundary >= end)
        {
            cutoff = cutoff.min(*boundary);
        }
        if rate_suffix_pattern().is_match(&text[end..cutoff]) {
            continue;
        }
        let token = text[start..end].to_string();
        if seen.contains(&token) {
            continue;
        }
        errors.push(format!(
            "currency formatting: \"{token}\" renders cents on a monthly-scale figure — round to \
             a whole dollar (cents only for genuinely sub-dollar precision, e.g. \"$1.50\", \
             \"$0.40\", or a per-unit rate like \"$0.018/hr\")"
        ));
        seen.push(token);
    }
    errors
}

/// `re.sub(r"\s+", " ", text).strip().lower()`.
fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_py_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    py_strip(&out).to_lowercase()
}

/// The cost-optimization section must render either a populated opportunity
/// row or the explicit no-eligible-commitment sentence, nothing else.
fn optimization_content(body: &str) -> Vec<String> {
    if has_populated_opportunity_row(body) {
        return Vec::new();
    }
    let text = collapse_spaces(&decoded_text(body).text().replace('\u{a0}', " "));
    if text.contains(NO_ELIGIBLE_COMMITMENT_SENTENCE) {
        return Vec::new();
    }
    vec![format!(
        "<section id=\"cost-optimization\"> has no substantive content — render a populated \
         opportunity row (not just column headers, an empty <tbody>, or other prose like the \
         credits disclaimer) or the exact \"{NO_ELIGIBLE_COMMITMENT_SENTENCE}\" sentence, never \
         a blank, heading-only, or table-header-only section"
    )]
}

/// Typography-first verdict rules: the verdict is the section thesis and is
/// never a coloured-pill row.
fn verdict(sections: &Sections, estimate: Option<&Json>) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(summary) = sections.html("decision-summary") else {
        // The missing section is already reported by the required-id check.
        return errors;
    };
    let classes = class_tokens(summary);
    if classes
        .iter()
        .any(|token| token.starts_with("badge-verdict-"))
    {
        errors.push(
            "decision-summary must use a typography-first verdict-headline, not badge-verdict-* \
             pills (meaning must not depend on color alone)"
                .to_string(),
        );
    }
    let rec = recommendation(estimate);
    let outcome = truthy(rec.and_then(|rec| rec.get("outcome")));
    let flips: &[Json] = match rec.and_then(|rec| rec.get("would_flip_if")) {
        Some(Json::Array(items)) => items,
        _ => &[],
    };
    if outcome && !classes.iter().any(|token| token == "verdict-headline") {
        errors.push(
            "estimation-infra.json declares recommendation.outcome but decision-summary has no \
             verdict-headline element (render outcome_label as <p \
             class=\"verdict-headline\">…</p>)"
                .to_string(),
        );
    }
    if flips.is_empty() {
        return errors;
    }
    // The heading is matched on decoded, whitespace-normalized visible text,
    // then every artifact flip condition must appear as an item of the list
    // that follows it.
    let entries = rendered_phrases(summary).entries;
    match rendered_list_after_heading(&entries, "what would flip") {
        None => errors.push(
            "estimation-infra.json declares recommendation.would_flip_if but decision-summary \
             has no \"What would flip this\" list"
                .to_string(),
        ),
        Some(items) => {
            let missing = unmatched_phrases_by(normalize_phrase, flips, &items);
            if !missing.is_empty() {
                let shown: Vec<String> = missing
                    .iter()
                    .map(|flip| format!("\"{}\"", untrusted(flip)))
                    .collect();
                errors.push(format!(
                    "\"What would flip this\" list renders {} of {} recommendation.would_flip_if \
                     entries — missing: {}",
                    flips.len() - missing.len(),
                    flips.len(),
                    shown.join("; ")
                ));
            }
        }
    }
    errors
}

/// Dependency-free WCAG-oriented semantics: the safe subset the one-pager
/// emits (no single-`<h1>` or per-table-`<caption>` requirement).
fn accessibility(html: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if !html_lang_declared(html) {
        errors.push("accessibility: <html> must declare a valid lang attribute".to_string());
    }
    let body = rx!(r"(?is)<style\b.*?</style>").replace_all_with(html, "");
    let (tables, figures) = accessibility_findings(&body);
    for index in tables {
        errors.push(format!(
            "accessibility: table {index} header cells must declare scope=\"col\" or \
             scope=\"row\""
        ));
    }
    for (index, has_aria_label, has_figcaption) in figures {
        if !has_aria_label {
            errors.push(format!(
                "accessibility: figure {index} must have an aria-label"
            ));
        }
        if !has_figcaption {
            errors.push(format!(
                "accessibility: figure {index} must include a <figcaption>"
            ));
        }
    }
    errors
}

/// Workshop compare tables use the column set of generate-report.md.
fn what_if_column_errors(section: &str) -> Vec<String> {
    let Some((start, end)) = rx!(r"(?is)<thead\b.*?</thead>").find(section) else {
        return vec![
            "what-if-scenarios must contain a table headed Scenario, Region, HA, Compute, Arch, \
             and Complexity"
                .to_string(),
        ];
    };
    let text = rx!(r"<[^>]+>")
        .replace_all_with(&section[start..end], " ")
        .to_lowercase();
    let missing: Vec<&str> = [
        ("Region", text.contains("region")),
        ("HA", text.contains("ha") || text.contains("availability")),
        ("Compute", text.contains("compute")),
        ("Arch", rx!(r"\barch\b").is_match(&text)),
        ("Complexity", text.contains("complexity")),
    ]
    .into_iter()
    .filter(|(_, present)| !present)
    .map(|(label, _)| label)
    .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "what-if-scenarios table is missing column(s): {} (generate-report.md what-if table)",
        missing.join(", ")
    )]
}

/// Decision mode's invariant: this decide-complete cycle has not itself gone
/// through Generate yet (`phases.generate` is pending or absent). The signal
/// is `.phase-status.json`'s own bookkeeping, not the filesystem; a missing
/// status file fails open, a corrupt one does not.
fn decision_mode_state(dir: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let phase = match read_artifact(Some(dir), ".phase-status.json") {
        Artifact::Absent => return errors,
        Artifact::Unreadable(message) => {
            errors.push(format!(
                "decision mode: .phase-status.json could not be read ({}) — an unreadable state \
                 file is not evidence of a pre-execution decision",
                escaped(&message)
            ));
            return errors;
        }
        Artifact::Corrupt(message) => {
            errors.push(format!(
                "decision mode: .phase-status.json exists but could not be read/parsed ({}) — \
                 state corrupted (invalid JSON). Delete the file and restart the current phase \
                 (INTERPRETER.md § State-file validation); an unreadable state file is not \
                 evidence of a pre-execution decision",
                untrusted(&message)
            ));
            return errors;
        }
        Artifact::Json(value) => value,
    };
    // An empty file is invalid JSON; a document that is not an object holds no
    // phase bookkeeping.
    let status = chain(&phase, &["phases", "generate"]).and_then(Json::as_str);
    if let Some(status @ ("completed" | "in_progress")) = status {
        errors.push(format!(
            "decision mode: .phase-status.json phases.generate is '{status}' — this \
             decide-complete cycle already went through Generate; decision mode is pre-execution \
             only for the CURRENT cycle (a prior cycle's execution pack may legitimately remain \
             on disk after a workshop reprice)"
        ));
    }
    errors
}

/// Validates a report; the returned problems are empty when it passes.
pub(super) fn validate(html: &str, migration_dir: Option<&Path>, mode: Mode) -> Vec<String> {
    let mut errors = Vec::new();
    let sections = parse_sections(html);
    for id in mode.required_sections() {
        match sections.count_of(id) {
            0 => errors.push(format!("missing required <section id=\"{id}\">")),
            1 => {}
            n => errors.push(format!("duplicate <section id=\"{id}\"> ({n} occurrences)")),
        }
    }

    // The other mode's terminal section must not appear: the decision report
    // carries no pointer into an execution pack that does not exist yet, and
    // the migration report no pre-execution call to action.
    let other = mode.other();
    let other_terminal = other.terminal_section();
    if sections.count_of(other_terminal) >= 1 {
        errors.push(format!(
            "--mode {} report must not contain <section id=\"{other_terminal}\"> (that is the \
             {}-mode terminal section)",
            mode.name(),
            other.name()
        ));
    }

    if !html.to_lowercase().contains("draft for review") {
        errors.push("footer must contain \"draft for review\" disclaimer".to_string());
    }
    errors.extend(currency_formatting(html));

    // A corrupt estimate fails open (it does not gate the report); one that
    // cannot be read at all is a problem of its own.
    let estimate = read_artifact(migration_dir, "estimation-infra.json");
    if let Artifact::Unreadable(message) = &estimate {
        errors.push(escaped(message));
    }
    let estimate = estimate.json();
    errors.extend(cost_figures(html, &sections, estimate));

    // cost-optimization must carry substantive content, full mode only.
    if mode == Mode::Full && sections.count_of("cost-optimization") >= 1 {
        errors.extend(optimization_content(
            sections.html("cost-optimization").unwrap_or(""),
        ));
    }

    errors.extend(verdict(&sections, estimate));
    errors.extend(accessibility(html));

    if let Some(dir) = migration_dir {
        match read_artifact(Some(dir), "scenarios/index.json") {
            Artifact::Unreadable(message) => errors.push(escaped(&message)),
            Artifact::Json(index) if scenario_len(&index) >= 2 => {
                if sections.count_of("what-if-scenarios") < 1 {
                    errors.push(
                        "scenarios/index.json has ≥2 scenarios but no <section \
                         id=\"what-if-scenarios\">"
                            .to_string(),
                    );
                } else {
                    errors.extend(what_if_column_errors(
                        sections.html("what-if-scenarios").unwrap_or(""),
                    ));
                }
            }
            _ => {}
        }

        // When Estimate declared decision_basis the report must render it, in
        // both modes, so a report that silently drops it cannot say REPORT_OK.
        let decision_basis = recommendation(estimate).and_then(|rec| rec.get("decision_basis"));
        if truthy(decision_basis) && sections.count_of("decision-basis") < 1 {
            errors.push(
                "estimation-infra.json declares recommendation.decision_basis but the report has \
                 no <section id=\"decision-basis\"> (\"What This Assessment Rests On\")"
                    .to_string(),
            );
        }
    }

    if mode == Mode::Decision {
        if let Some(dir) = migration_dir {
            errors.extend(decision_mode_state(dir));
        }
    }
    errors
}

/// The `REPORT_OK` line.
fn success_line(html: &str, mode: Mode) -> String {
    let sections = parse_sections(html);
    let required = mode.required_sections().len();
    let mut line = format!("REPORT_OK | structure=complete | sections={required}/{required}");
    if sections.count_of("what-if-scenarios") >= 1 {
        line.push_str(" | optional=what-if-scenarios");
    }
    line
}

/// Handler for `startup heroku-report REPORT [--migration-dir DIR]
/// [--mode full|decision]`.
///
/// # Errors
///
/// A usage error (exit status 2) without the report path or with an unknown
/// `--mode`; a runtime error (exit status 1) when the report is missing or
/// cannot be read, or fails validation.
pub fn heroku_report_command(
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
                    "argument --mode: invalid choice: '{}' (choose from 'full', 'decision')",
                    untrusted(value)
                ),
            )
        })?,
    };
    let report_path = Path::new(report);
    let shown = escaped(report);
    let is_file = is_regular_file(report_path)
        .map_err(|message| ToolkitError::runtime("report_unreadable", escaped(&message)))?;
    if !is_file {
        output.diagnostic(&format!("REPORT_FAIL | file={shown} | reason=not_found"));
        return Err(ToolkitError::runtime(
            "report_not_found",
            format!("report not found: {shown}"),
        ));
    }
    let html = read_text(report_path, report)
        .map_err(|message| ToolkitError::runtime("report_unreadable", escaped(&message)))?;
    let migration_dir = invocation.value("--migration-dir").map(Path::new);
    let errors = validate(&html, migration_dir, mode);
    if !errors.is_empty() {
        output.diagnostic(&format!(
            "REPORT_FAIL | file={shown} | errors={}",
            errors.len()
        ));
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
