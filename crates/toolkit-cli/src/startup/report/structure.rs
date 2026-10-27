//! Section structure checks of the report validator: required and optional
//! section ids, the table of contents, appendix depth, the security and
//! verdict content of the decision summary, and the small content panels
//! (configuration, glossary, share card).

use toolkit_core::Json;

use super::html::unescape;
use super::rx::{group, rx};
use super::values::{chain, py_float, py_int, truthy};
use super::walk::section_html;
use crate::manifests::is_falsy;

pub(super) const REQUIRED_SECTION_IDS: [&str; 10] = [
    "decision-summary",
    "exec-assumptions",
    "exec-services",
    "exec-costs",
    "exec-timeline",
    "exec-risks",
    "appendix-services",
    "appendix-costs",
    "appendix-steps",
    "appendix-artifacts",
];

/// Decision mode: executive sections and the call to action only.
pub(super) const DECISION_REQUIRED_SECTION_IDS: [&str; 7] = [
    "decision-summary",
    "exec-assumptions",
    "exec-services",
    "exec-costs",
    "exec-timeline",
    "exec-risks",
    "decision-cta",
];

/// AI-only mode: no infrastructure track to populate.
pub(super) const AI_ONLY_REQUIRED_SECTION_IDS: [&str; 7] = [
    "decision-summary",
    "exec-assumptions",
    "exec-risks",
    "appendix-ai",
    "appendix-artifacts",
    "appendix-config",
    "appendix-glossary",
];

pub(super) const OPTIONAL_SECTION_IDS: [&str; 13] = [
    "exec-share",
    "exec-tco",
    "exec-architecture",
    "exec-security-teaser",
    "exec-optimization",
    "what-if-scenarios",
    "appendix-ai",
    "appendix-config",
    "appendix-security",
    "appendix-security-gap",
    "appendix-assumptions",
    "appendix-optimization",
    "appendix-glossary",
];

/// Minimum number of rows (or blocks) of the appendices that must not be stubs.
pub(super) const MIN_CONTENT_DEPTH: [(&str, i64); 3] = [
    ("appendix-costs", 3),
    ("appendix-services", 2),
    ("appendix-steps", 2),
];

/// The section ids of the document with the number of times each occurs, in
/// order of first appearance.
pub(super) fn section_id_counts(html: &str) -> Vec<(String, usize)> {
    let pattern = rx!(r#"(?i)<section\b[^>]*\bid=(['"])([^'"]+)\1"#);
    let mut counts: Vec<(String, usize)> = Vec::new();
    for groups in pattern.captures_all(html) {
        let id = group(html, &groups, 2);
        match counts.iter_mut().find(|entry| entry.0.as_str() == id) {
            Some((_, count)) => *count += 1,
            None => counts.push((id.to_string(), 1)),
        }
    }
    counts
}

/// `counts.get(id, 0)`.
pub(super) fn count_of(counts: &[(String, usize)], id: &str) -> usize {
    counts
        .iter()
        .find(|(name, _)| name.as_str() == id)
        .map_or(0, |(_, count)| *count)
}

pub(super) fn required_sections(html: &str, required: &[&str]) -> Vec<String> {
    let counts = section_id_counts(html);
    let mut errors = Vec::new();
    for id in required {
        match count_of(&counts, id) {
            0 => errors.push(format!("missing required <section id=\"{id}\">")),
            1 => {}
            n => errors.push(format!("duplicate <section id=\"{id}\"> ({n} occurrences)")),
        }
    }
    errors
}

/// The `href="#..."` targets of the `<nav class="toc">`.
pub(super) fn toc_hrefs(html: &str) -> Vec<String> {
    let nav = rx!(r#"(?is)<nav\b[^>]*\bclass=["'][^"']*toc[^"']*["'][^>]*>(.*?)</nav>"#);
    let Some(groups) = nav.captures(html) else {
        return Vec::new();
    };
    let inner = group(html, &groups, 1);
    let link = rx!(r##"(?i)href="#([^"]+)""##);
    link.captures_all(inner)
        .iter()
        .map(|found| group(inner, found, 1).to_string())
        .collect()
}

pub(super) fn toc_links(html: &str, required: &[&str]) -> Vec<String> {
    let hrefs = toc_hrefs(html);
    let mut errors = Vec::new();
    if hrefs.is_empty() {
        return errors;
    }
    let counts = section_id_counts(html);
    for href in &hrefs {
        if count_of(&counts, href) == 0 {
            errors.push(format!(
                "TOC broken link href=\"#{href}\" — no matching <section id=\"{href}\">"
            ));
        }
    }
    for id in required {
        if count_of(&counts, id) > 0 && !hrefs.iter().any(|href| href == id) {
            errors.push(format!(
                "TOC missing link to required section id=\"{id}\" (add <a href=\"#{id}\">)"
            ));
        }
    }
    errors
}

/// The verdict is the report thesis and must precede navigation.
pub(super) fn decision_first(html: &str) -> Vec<String> {
    let decision = rx!(r#"(?i)<section\b[^>]*\bid=["']decision-summary["']"#).find(html);
    let toc = rx!(r#"(?i)<nav\b[^>]*\bclass=["'][^"']*\btoc\b[^"']*["']"#).find(html);
    if let (Some((decision_start, _)), Some((toc_start, _))) = (decision, toc) {
        if decision_start > toc_start {
            return vec![
                "decision-summary must appear before the table of contents (show the report \
                 thesis before navigation)"
                    .to_string(),
            ];
        }
    }
    Vec::new()
}

/// `_count_table_rows`: the `<tr` count of the first `<tbody>`.
pub(super) fn count_table_rows(section: &str) -> usize {
    let Some(groups) = rx!(r"(?is)<tbody>(.*?)</tbody>").captures(section) else {
        return 0;
    };
    let body = group(section, &groups, 1);
    rx!(r"(?i)<tr\b").find_all(body).len()
}

/// `_accounted_service_count`: priced services plus specialist deferrals.
pub(super) fn accounted_service_count(infra: Option<&Json>) -> Option<i64> {
    let inputs = infra?.get("complexity_inputs")?;
    if !matches!(inputs, Json::Object(_)) {
        return None;
    }
    let services = py_int(inputs.get("service_count")?)?;
    let deferred = py_int(inputs.get("deferred_count")?)?;
    if services < 0 || deferred < 0 {
        return None;
    }
    services.checked_add(deferred)
}

/// The content depth of an appendix section.
pub(super) fn content_depth(section_id: &str, section: &str) -> usize {
    let rows = count_table_rows(section);
    match section_id {
        "appendix-services" => rows.max(rx!(r#"class="cluster-block""#).find_all(section).len()),
        "appendix-steps" => rows.max(rx!(r"(?i)<h3>Phase\s+\d").find_all(section).len()),
        _ => rows,
    }
}

/// Appendix sections must not be stubs that link to the JSON only.
pub(super) fn appendix_stub(html: &str) -> bool {
    rx!(r#"(?is)<section[^>]*id="appendix-costs"[^>]*>.*?Full artifacts:\s*<code>estimation-infra\.json</code>"#)
        .is_match(html)
}

/// The second stub pattern: `appendix-services` that only points at the design.
pub(super) fn services_stub(html: &str) -> bool {
    rx!(r#"(?is)<section[^>]*id="appendix-services"[^>]*>\s*<p>\s*See\s*<code>aws-design\.json</code>"#)
        .is_match(html)
}

fn security_scoped_html(html: &str) -> String {
    let chunks: Vec<String> = [
        "appendix-security",
        "appendix-costs",
        "exec-security-teaser",
    ]
    .iter()
    .filter_map(|id| section_html(html, id))
    .filter(|part| !part.is_empty())
    .collect();
    chunks.join("\n")
}

/// `_dollar_amount_present`: a dollar-formatted value appears in `text`.
#[allow(clippy::float_cmp)]
fn dollar_amount_present(amount: f64, text: &str) -> bool {
    let normalized = text.replace(',', "");
    let mut candidates: Vec<String> = Vec::new();
    if amount.is_finite() && amount.fract() == 0.0 {
        let whole = format!("{amount:.0}");
        candidates.push(format!("${whole}"));
        candidates.push(format!("${whole}.00"));
        candidates.push(format!("${whole}.0"));
    }
    candidates.push(format!("${amount:.2}"));
    candidates
        .iter()
        .any(|candidate| normalized.contains(candidate.as_str()))
}

/// The `security_baseline` block of the estimate, when it carries one.
pub(super) fn security_baseline(infra: Option<&Json>) -> Option<&Json> {
    let baseline = chain(
        infra?,
        &["projected_costs", "breakdown", "security_baseline"],
    )?;
    truthy(Some(baseline)).then_some(baseline)
}

/// `_has_guardduty_or_baseline`: `Ok` when the security content is there,
/// otherwise the message.
pub(super) fn guardduty_or_baseline(html: &str, infra: Option<&Json>) -> Result<(), String> {
    let scope = security_scoped_html(html);
    if scope.is_empty() {
        return Err(
            "missing security content in appendix-security or appendix-costs sections".to_string(),
        );
    }
    if rx!(r"(?i)GuardDuty").is_match(&scope) {
        return Ok(());
    }
    let Some(infra) = infra.filter(|value| !is_falsy(value)) else {
        return Err("missing GuardDuty mention in security/cost appendix sections".to_string());
    };
    let Some(baseline) = security_baseline(Some(infra)) else {
        return Ok(());
    };
    if let Some(Json::Object(components)) = baseline
        .get("components")
        .filter(|member| !is_falsy(member))
    {
        for (_, value) in components {
            if matches!(value, Json::Null) {
                continue;
            }
            if let Some(amount) = py_float(value) {
                if amount > 0.0 && dollar_amount_present(amount, &scope) {
                    return Ok(());
                }
            }
        }
    }
    Err(
        "appendix-security/appendix-costs must mention GuardDuty or include dollar-formatted \
         security_baseline component costs from estimation-infra.json \
         (e.g. GuardDuty $13.00, CloudTrail $1.50)"
            .to_string(),
    )
}

/// A baseline needs a compact teaser in the executive flow.
pub(super) fn security_teaser(html: &str, infra: Option<&Json>) -> Vec<String> {
    if security_baseline(infra).is_none() {
        return Vec::new();
    }
    if count_of(&section_id_counts(html), "exec-security-teaser") >= 1 {
        return Vec::new();
    }
    vec![
        "security_baseline exists but no <section id=\"exec-security-teaser\"> — keep a compact \
         teaser in the executive flow and the full control table in appendix-security"
            .to_string(),
    ]
}

/// A recommendation needs a one-sentence verdict callout in the summary.
pub(super) fn verdict(html: &str, infra: Option<&Json>) -> Vec<String> {
    let has_recommendation = infra.is_some_and(|value| truthy(value.get("recommendation")));
    if !has_recommendation {
        return Vec::new();
    }
    let summary = section_html(html, "decision-summary").unwrap_or_default();
    if rx!(r#"(?i)class="[^"]*\bverdict\b[^"]*""#).is_match(&summary) {
        return Vec::new();
    }
    vec![
        "recommendation block exists but decision-summary has no verdict banner (wrap the \
         one-sentence recommendation in class=\"verdict\")"
            .to_string(),
    ]
}

/// The Activate action stays attached to its executive-summary benefit.
pub(super) fn activate_link(html: &str) -> Vec<String> {
    let summary = section_html(html, "decision-summary").unwrap_or_default();
    let mentions =
        rx!(r"(?i)AWS\s+Activate|Activate\s+(?:Founders|Portfolio|credits)").is_match(&summary);
    if !mentions {
        return Vec::new();
    }
    if rx!(r#"(?i)href=["']https://aws\.amazon\.com/startups/credits/?["']"#).is_match(&summary) {
        return Vec::new();
    }
    vec![
        "decision-summary mentions AWS Activate but has no clickable official apply link \
         (https://aws.amazon.com/startups/credits/)"
            .to_string(),
    ]
}

/// Full reports present terms and meanings in a scannable two-column table.
pub(super) fn glossary_table(html: &str) -> Vec<String> {
    let Some(section) = section_html(html, "appendix-glossary") else {
        return vec![
            "full report must include a dedicated <section id=\"appendix-glossary\">".to_string(),
        ];
    };
    let table =
        rx!(r#"(?is)<table\b[^>]*class=["'][^"']*\bglossary-table\b[^"']*["'][^>]*>(.*?)</table>"#);
    let Some(groups) = table.captures(&section) else {
        return vec![
            "full report glossary must use <table class=\"glossary-table\"> with distinct Term \
             and Meaning cells"
                .to_string(),
        ];
    };
    let table = group(&section, &groups, 1);
    let header = rx!(r"(?is)<thead\b[^>]*>(.*?)</thead>")
        .captures(table)
        .map(|found| group(table, &found, 1).to_string());
    let has_columns = header.as_deref().is_some_and(|text| {
        rx!(r"(?i)>\s*Term\s*<").is_match(text) && rx!(r"(?i)>\s*Meaning\s*<").is_match(text)
    });
    if !has_columns {
        return vec![
            "glossary-table must have \"Term\" and \"Meaning\" column headers".to_string(),
        ];
    }
    if count_table_rows(&format!("<table>{table}</table>")) < 3 {
        return vec!["glossary-table must contain at least 3 term-definition rows".to_string()];
    }
    Vec::new()
}

/// Key decisions and next steps must be ordered lists.
pub(super) fn action_lists(html: &str) -> Vec<String> {
    let summary = section_html(html, "decision-summary").unwrap_or_default();
    let decisions = rx!(r"(?is)<h3[^>]*>\s*Key decisions ahead\s*</h3>\s*<(ul|ol)\b");
    let steps = rx!(r"(?is)<h3[^>]*>\s*Next steps\s*</h3>\s*<(ul|ol)\b");
    let mut errors = Vec::new();
    for (heading, pattern) in [("Key decisions ahead", decisions), ("Next steps", steps)] {
        if let Some(groups) = pattern.captures(&summary) {
            if group(&summary, &groups, 1).to_lowercase() == "ul" {
                errors.push(format!(
                    "decision-summary: \"{heading}\" must use <ol class=\"compact\"> (ordered \
                     action items), not a bullet list"
                ));
            }
        }
    }
    errors
}

/// Whole-stack decisions stay distinct from track-scoped holds.
pub(super) fn decision_language(html: &str, infra: Option<&Json>) -> Vec<String> {
    let summary = section_html(html, "decision-summary").unwrap_or_default();
    let mut errors = Vec::new();
    let has_stay_reasons = infra
        .and_then(|value| chain(value, &["recommendation", "stay_if"]))
        .is_some_and(|value| truthy(Some(value)));
    let entirely = rx!(r"(?i)<h[1-6][^>]*>\s*Stay\s+entirely\s+if\s*</h[1-6]>")
        .find_all(&summary)
        .len();
    if has_stay_reasons && entirely != 1 {
        errors.push(format!(
            "decision-summary has recommendation.stay_if reasons and must contain exactly one \
             \"Stay entirely if\" heading (found {entirely})"
        ));
    } else if rx!(r"(?i)<h[1-6][^>]*>\s*Stay\s+if\s*</h[1-6]>").is_match(&summary) {
        errors.push(
            "decision-summary heading \"Stay if\" is ambiguous — use \"Stay entirely if\" for \
             whole-stack reasons"
                .to_string(),
        );
    }
    if rx!(r#"(?i)class=["'][^"']*\bbadge-verdict-"#).is_match(&summary) {
        errors.push(
            "decision-summary must use a typography-first verdict headline and plain-text \
             metadata, not badge-verdict-* pills"
                .to_string(),
        );
    }
    errors
}

/// When `appendix-config` is present it must carry the provenance columns.
pub(super) fn appendix_config(html: &str) -> Vec<String> {
    let Some(section) = section_html(html, "appendix-config").filter(|text| !text.is_empty())
    else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    if !rx!(r"(?i)<table\b").is_match(&section) {
        errors.push(
            "appendix-config must contain an HTML table of configuration choices".to_string(),
        );
        return errors;
    }
    let Some((start, end)) = rx!(r"(?is)<thead\b.*?</thead>").find(&section) else {
        errors.push("appendix-config table must have <thead> with column headers".to_string());
        return errors;
    };
    let header = section[start..end].to_lowercase();
    if !header.contains("question") && !header.contains("assumption") {
        errors.push(
            "appendix-config table must include a \"Question\" or \"Assumption\" column (from \
             preferences.prompt)"
                .to_string(),
        );
    }
    if !header.contains("choice") {
        errors.push(
            "appendix-config table must include a \"Your choice\" column (the value the reader \
             confirmed)"
                .to_string(),
        );
    }
    if !header.contains("source") {
        errors.push(
            "appendix-config table must include a \"Source\" column (user answer, extracted, or \
             default)"
                .to_string(),
        );
    }
    if !header.contains("consequence") {
        errors.push(
            "appendix-config table must include a Design consequence column (from \
             preferences.design_consequence)"
                .to_string(),
        );
    }
    let rows = count_table_rows(&section);
    if rows < 2 {
        errors.push(format!(
            "appendix-config must have >=2 configuration rows (found {rows})"
        ));
    }
    errors
}

/// `unescape(re.sub(r"<[^>]+>", " ", fragment))`: the text of a fragment.
pub(super) fn tag_stripped_text(fragment: &str) -> String {
    let stripped = rx!(r"<[^>]+>").find_all(fragment);
    let mut out = String::with_capacity(fragment.len());
    let mut cursor = 0;
    for (start, end) in stripped {
        out.push_str(&fragment[cursor..start]);
        out.push(' ');
        cursor = end;
    }
    out.push_str(&fragment[cursor..]);
    unescape(&out)
}

/// A copy-ready leadership brief is required when estimates are available.
pub(super) fn share_section(html: &str, infra: Option<&Json>, ai: Option<&Json>) -> Vec<String> {
    if infra.is_none() && ai.is_none() {
        return Vec::new();
    }
    let Some(section) = section_html(html, "exec-share") else {
        return vec![
            "monthly estimation artifacts exist but no <section id=\"exec-share\"> copy-ready \
             leadership brief was rendered"
                .to_string(),
        ];
    };
    let card =
        rx!(r#"(?is)<(div|aside)\b[^>]*class=["'][^"']*\bshare-card\b[^"']*["'][^>]*>(.*?)</\1>"#);
    let Some(groups) = card.captures(&section) else {
        return vec!["exec-share must contain a class=\"share-card\" decision brief".to_string()];
    };
    let body = group(&section, &groups, 2);
    let paragraphs: Vec<String> = rx!(r"(?is)<p\b[^>]*>(.*?)</p>")
        .captures_all(body)
        .iter()
        .map(|found| group(body, found, 1).to_string())
        .collect();
    let mut errors = Vec::new();
    if let [paragraph] = paragraphs.as_slice() {
        let text = tag_stripped_text(paragraph);
        let words = rx!(r"\b[\w'-]+\b").find_all(&text).len();
        if words < 20 {
            errors.push(format!(
                "exec-share share-card paragraph must contain a substantive copy-ready brief of \
                 at least 20 words (found {words})"
            ));
        }
    } else {
        errors.push(format!(
            "exec-share share-card must contain exactly one standalone <p> (found {})",
            paragraphs.len()
        ));
    }
    if rx!(r"(?i)<(?:button|script|input|textarea)\b").is_match(&section) {
        errors.push(
            "exec-share must be a static copy-ready paragraph without buttons, scripts, or \
             editable controls"
                .to_string(),
        );
    }
    errors
}
