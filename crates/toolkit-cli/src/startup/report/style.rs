//! Customer-facing readability, currency formatting, visual contract,
//! executive vocabulary, accessibility and fixture-bleed checks.

use std::collections::HashSet;
use std::path::Path;

use super::rx::{group, rx, Rx};
use super::walk::{decoded_text, section_html};
use crate::manifests::py_repr_str;

/// Sections of the executive flow, which speak the reader's language.
const EXEC_SECTION_IDS: [&str; 11] = [
    "decision-summary",
    "exec-share",
    "exec-tco",
    "exec-services",
    "exec-costs",
    "exec-architecture",
    "exec-security-teaser",
    "exec-optimization",
    "what-if-scenarios",
    "exec-timeline",
    "exec-risks",
];

/// Migration ID baked into the reference fixture; seeing it in a real run
/// means the golden file was copied verbatim.
const FIXTURE_CANARY_ID: &str = "0611-0606";

/// `re.sub(pattern, "", text)` for a pattern matched by spans.
fn remove_spans(text: &str, spans: &[(usize, usize)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in spans {
        out.push_str(&text[cursor..*start]);
        cursor = *end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// `_readability_scope`: the body without `<style>` blocks.
pub(super) fn readability_scope(html: &str) -> String {
    let no_style = remove_spans(html, &rx!(r"(?is)<style\b.*?</style>").find_all(html));
    let body = rx!(r"(?is)<body\b[^>]*>(.*?)</body>");
    match body.captures(&no_style) {
        Some(groups) => group(&no_style, &groups, 1).to_string(),
        None => no_style,
    }
}

fn readability_patterns() -> [(&'static Rx, &'static str); 8] {
    [
        (
            rx!(r"(?i)(?<![-_])\bTCO\b|Total\s+Cost\s+of\s+Ownership"),
            "misleading ownership-cost label (\"TCO\") — use \"estimated AWS monthly run rate\"; \
             the report does not price staffing or operating labor",
        ),
        (
            rx!(r"(?i)Rubric:"),
            "internal scoring trace (\"Rubric:\") — drop it or gate behind a <details> \"Why \
             this mapping?\" block",
        ),
        (
            rx!(r"(?i)\d+\s*(?:–|-|to)\s*\d+\s*(?:engineering\s+|effort\s+)?hours\b"),
            "effort-hours range — the plugin has no calibrated effort data and hour figures get \
             pasted into budgets; communicate time as stage sequence + duration drivers \
             (migration-complexity.md § Provenance)",
        ),
        (
            rx!(
                r"(?i)(?<!uncalibrated\):\s)(?<!uncalibrated\): )\b\d+\s*(?:–|-|to)\s*\d+\+?\s*weeks\b"
            ),
            "bare week-range estimate — durations are uncalibrated; either drop it or label it \
             \"Legacy planning heuristic (uncalibrated): N–M weeks\"",
        ),
        (
            rx!(r"(?i)Section\s+0\b"),
            "literal \"Section 0\" heading — drop numeric \"Section N\" prefixes from \
             customer-facing headings; let the table of contents carry structure",
        ),
        (
            rx!(r"(?i)<h[1-6][^>]*>\s*Section\s+\d+[a-z]?\s*[—-]"),
            "numbered \"Section N —\" heading — drop numeric prefixes from headings; let the \
             table of contents carry structure",
        ),
        (
            rx!(r"(?i)\b(?:very|significantly|extremely|vastly)\b"),
            "vague intensifier (very/significantly/extremely/vastly) — quantify the claim from \
             artifact data instead (\"-32% ($497/mo lower)\"), or drop it",
        ),
        (
            rx!(r"(?i)\b\d{1,2}/\d{1,2}/\d{4}\b"),
            "slash-format date (N/N/YYYY reads differently across locales) — use ISO YYYY-MM-DD \
             for all dates",
        ),
    ]
}

/// Customer-facing readability rules.
pub(super) fn readability(html: &str) -> Vec<String> {
    let scope = readability_scope(html);
    readability_patterns()
        .iter()
        .filter(|(pattern, _)| pattern.is_match(&scope))
        .map(|(_, label)| format!("readability: {label}"))
        .collect()
}

/// Whole-dollar part below this keeps a cents digit meaningful.
const CENTS_MEANINGFUL_BELOW: u32 = 2;

/// The position `count` characters after `from` (clamped to the text).
fn chars_after(text: &str, from: usize, count: usize) -> usize {
    text[from..]
        .char_indices()
        .nth(count)
        .map_or(text.len(), |(offset, _)| from + offset)
}

/// The position `count` characters before `from` (clamped to the start).
fn chars_before(text: &str, from: usize, count: usize) -> usize {
    text[..from]
        .char_indices()
        .rev()
        .nth(count.saturating_sub(1))
        .map_or(0, |(offset, _)| offset)
}

/// Whether the whole-dollar digits (with separators) are at least the
/// threshold that makes cents suspicious.
fn whole_dollars_at_least_threshold(digits: &str) -> bool {
    let cleaned: String = digits.chars().filter(|c| *c != ',').collect();
    let significant = cleaned.trim_start_matches('0');
    match significant.len() {
        0 => false,
        1 => significant
            .parse::<u32>()
            .is_ok_and(|value| value >= CENTS_MEANINGFUL_BELOW),
        _ => true,
    }
}

/// Monthly cost figures must render as whole dollars (rule 2).
pub(super) fn currency_formatting(html: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let scope = readability_scope(html);
    let decoded = decoded_text(&scope);
    let text = decoded.text.as_str();
    let cents = rx!(r"\$([0-9][0-9,]*)\.([0-9]{2})\b");
    let rate_suffix = rx!(
        r"(?i)^\s*(?:/|\(|\bper\b)?\s*(?:mo\b\s*(?:per\b\s*)?)?(?:hr|hour|hourly|vcpu|gb|gib|tb|image|unit|policy|1m|10k|[0-9]+-mo)\b"
    );
    let operand_trailing = rx!(r"(?i)^\s*(?:×|times\b|x(?=\s|$|[$0-9]))");
    let operand_leading = rx!(r"(?i)(?:×|\btimes|(?<=[\s0-9])x)\s*$");
    let mut seen: HashSet<String> = HashSet::new();
    for groups in cents.captures_all(text) {
        let Some((start, end)) = groups.first().copied().flatten() else {
            continue;
        };
        if !whole_dollars_at_least_threshold(group(text, &groups, 1)) {
            continue;
        }
        // The suffix window stops at the next block-level boundary.
        let mut cutoff = chars_after(text, end, 25);
        if let Some(boundary) = decoded.boundaries.iter().find(|boundary| **boundary >= end) {
            cutoff = cutoff.min(*boundary);
        }
        let trailing = &text[end..cutoff];
        if rate_suffix.is_match(trailing) {
            continue;
        }
        // In a Calculation/Notes cell only a rate operand next to a
        // multiplication marker is exempt.
        if decoded.calc_mask[start..end].iter().any(|flag| *flag) {
            if operand_trailing.is_match(trailing) {
                continue;
            }
            let lead_start = decoded
                .boundaries
                .iter()
                .rev()
                .find(|boundary| **boundary <= start)
                .copied()
                .unwrap_or(0);
            let leading = &text[lead_start.max(chars_before(text, start, 25))..start];
            if operand_leading.is_match(leading) {
                continue;
            }
        }
        let token = &text[start..end];
        if !seen.insert(token.to_string()) {
            continue;
        }
        errors.push(format!(
            "currency formatting: \"{token}\" renders cents on a monthly-scale figure — round to \
             a whole dollar (generate-artifacts-report.md rule 2: cents only for genuinely \
             sub-dollar precision, e.g. \"$1.50\", \"$0.40\", or a per-unit rate like \
             \"$0.018/hr\")"
        ));
    }
    errors
}

fn visual_contract_checks() -> [(&'static Rx, &'static str); 11] {
    [
        (
            rx!(r"(?is)body\s*\{[^}]*background\s*:\s*(?:var\([^)]*\)|#f6f8fa)"),
            "body must use a contrasting page background",
        ),
        (
            rx!(r"(?is)\.(?:report|container)\s*\{[^}]*max-width\s*:"),
            "report shell must constrain line length with max-width",
        ),
        (
            rx!(
                r"(?is)section\s*\{[^}]*background\s*:[^;}]+;?[^}]*border\s*:[^;}]+;?[^}]*border-radius\s*:"
            ),
            "sections must render as bordered surface cards",
        ),
        (
            rx!(r"(?is)\.metrics\s*\{[^}]*display\s*:\s*grid[^}]*grid-template-columns\s*:"),
            "executive metrics must use a responsive CSS grid",
        ),
        (
            rx!(r"(?is)\.metric\s*\{[^}]*border\s*:[^;}]+;?[^}]*border-radius\s*:"),
            "metric cards must have a bordered card treatment",
        ),
        (
            rx!(r"(?is)\.verdict\s*\{[^}]*background\s*:[^;}]+;?[^}]*border\s*:"),
            "recommendation must have a visually distinct callout treatment",
        ),
        (
            rx!(r"(?is)\.savings\s*\{[^}]*color\s*:"),
            "supported savings values must have a reusable visual treatment",
        ),
        (
            rx!(r"(?is)\.appendix-header\s*\{"),
            "full/decision shared shell must define an appendix divider",
        ),
        (
            rx!(r"(?is):focus-visible\s*\{"),
            "keyboard focus must be visibly styled",
        ),
        (
            rx!(r"(?is)@media\s*\([^)]*max-width\s*:\s*700px[^)]*\)"),
            "mobile readability breakpoint (700px) is required",
        ),
        (
            rx!(r"(?is)@media\s+print\s*\{"),
            "print-specific styling is required",
        ),
    ]
}

/// The shared report shell's minimum visual readability contract.
pub(super) fn visual_contract(html: &str) -> Vec<String> {
    let styles = rx!(r"(?is)<style\b[^>]*>(.*?)</style>").captures_all(html);
    if styles.is_empty() {
        return vec!["visual readability: missing inline <style> report shell".to_string()];
    }
    let css = styles
        .iter()
        .map(|found| group(html, found, 1))
        .collect::<Vec<_>>()
        .join("\n");
    visual_contract_checks()
        .iter()
        .filter(|(pattern, _)| !pattern.is_match(&css))
        .map(|(_, label)| format!("visual readability: {label}"))
        .collect()
}

/// A Python list of strings as `repr` shows it.
fn py_list_repr(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|item| py_repr_str(item)).collect();
    format!("[{}]", inner.join(", "))
}

/// Executive sections name what the reader controls: artifact filenames and
/// Terraform resource IDs belong in the appendices.
pub(super) fn exec_vocabulary(html: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for id in EXEC_SECTION_IDS {
        let Some(section) = section_html(html, id).filter(|text| !text.is_empty()) else {
            continue;
        };
        let mut filenames: Vec<String> = rx!(r"(?i)\b[a-z0-9][a-z0-9_-]*\.json\b")
            .find_all(&section)
            .iter()
            .map(|(start, end)| section[*start..*end].to_lowercase())
            .collect();
        filenames.sort();
        filenames.dedup();
        let mut resources: Vec<String> = rx!(r"\baws_[a-z0-9_]+\.[a-z0-9_]+\b")
            .find_all(&section)
            .iter()
            .map(|(start, end)| section[*start..*end].to_string())
            .collect();
        resources.sort();
        resources.dedup();
        if !filenames.is_empty() {
            errors.push(format!(
                "exec vocabulary: <section id=\"{id}\"> exposes artifact filename(s) {} — name \
                 what the reader controls in the executive flow; keep artifact filenames in the \
                 technical appendices",
                py_list_repr(&filenames)
            ));
        }
        if !resources.is_empty() {
            errors.push(format!(
                "exec vocabulary: <section id=\"{id}\"> exposes Terraform resource ID(s) {} — \
                 move resource names to the appendix; the executive flow names what the reader \
                 controls",
                py_list_repr(&resources)
            ));
        }
    }
    errors
}

/// Dependency-free checks for high-value WCAG-oriented HTML semantics.
pub(super) fn accessibility(html: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if !rx!(r#"(?i)<html\b[^>]*\blang=["'][a-z]{2}(?:-[A-Za-z0-9]+)?["']"#).is_match(html) {
        errors.push("accessibility: <html> must declare a valid lang attribute".to_string());
    }
    let body = remove_spans(html, &rx!(r"(?is)<style\b.*?</style>").find_all(html));
    if rx!(r"(?i)<h1\b").find_all(&body).len() != 1 {
        errors.push("accessibility: report must contain exactly one <h1>".to_string());
    }
    for (offset, (start, end)) in rx!(r"(?is)<table\b[^>]*>.*?</table>")
        .find_all(&body)
        .into_iter()
        .enumerate()
    {
        let index = offset + 1;
        let table = &body[start..end];
        if !rx!(r"(?i)<caption\b").is_match(table) {
            errors.push(format!(
                "accessibility: table {index} must include a <caption>"
            ));
        }
        let scoped = rx!(r#"(?i)\bscope=["'](?:col|row)["']"#);
        let missing_scope = rx!(r"(?i)<th\b[^>]*>")
            .find_all(table)
            .iter()
            .any(|(th_start, th_end)| !scoped.is_match(&table[*th_start..*th_end]));
        if missing_scope {
            errors.push(format!(
                "accessibility: table {index} header cells must declare scope=\"col\" or \
                 scope=\"row\""
            ));
        }
    }
    for (offset, (start, end)) in rx!(r"(?is)<figure\b[^>]*>.*?</figure>")
        .find_all(&body)
        .into_iter()
        .enumerate()
    {
        let index = offset + 1;
        let figure = &body[start..end];
        let opening = rx!(r"(?i)^<figure\b[^>]*>")
            .find(figure)
            .map_or("", |(open_start, open_end)| &figure[open_start..open_end]);
        if !rx!(r#"(?i)\brole=["']img["']"#).is_match(opening) {
            errors.push(format!(
                "accessibility: figure {index} must declare role=\"img\""
            ));
        }
        if !rx!(r#"(?i)\baria-label=["'][^"']+["']"#).is_match(opening) {
            errors.push(format!(
                "accessibility: figure {index} must have an aria-label"
            ));
        }
        if !rx!(r"(?i)<figcaption\b").is_match(figure) {
            errors.push(format!(
                "accessibility: figure {index} must include a <figcaption>"
            ));
        }
    }
    errors
}

/// Catch agents that copied the reference fixture verbatim into a real run.
pub(super) fn fixture_bleed(html: &str, migration_dir: Option<&Path>) -> Vec<String> {
    let Some(migration_dir) = migration_dir else {
        return Vec::new();
    };
    let dir_name = migration_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let body = readability_scope(html);
    let mut errors = Vec::new();
    if body.contains(FIXTURE_CANARY_ID) && dir_name != FIXTURE_CANARY_ID {
        errors.push(format!(
            "fixture bleed: reference canary migration ID \"{FIXTURE_CANARY_ID}\" appears in a \
             real run (--migration-dir={dir_name}) — the report was copied from the fixture"
        ));
    }
    let mut ids: Vec<String> = rx!(r"\b(\d{4}-\d{4})\b")
        .captures_all(&body)
        .iter()
        .map(|found| group(&body, found, 1).to_string())
        .collect();
    ids.sort();
    ids.dedup();
    if rx!(r"^\d{4}-\d{4}$").is_match(&dir_name) && !ids.is_empty() && !ids.contains(&dir_name) {
        errors.push(format!(
            "migration ID mismatch: report references {} but --migration-dir is {dir_name} — \
             verify the report belongs to this run",
            py_list_repr(&ids)
        ));
    }
    errors
}
