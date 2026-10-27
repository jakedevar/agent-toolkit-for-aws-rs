//! The decision-core content checks: artifact fields the decision core
//! requires must be rendered, not only the section ids.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use toolkit_core::Json;

use super::rx::rx;
use super::values::{py_str, truthy};
use super::walk::{plain_text, rendered_fragment, section_html, Kind};
use super::Mode;
use crate::manifests::{is_falsy, py_strip};
use crate::startup::artifacts::pyjson::loads;
use crate::startup::{is_regular_file, read_text};

/// `re.findall(r"[a-z0-9]+", text)`.
fn ascii_words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// Word phrases used to recognize a rendered condition without a verbatim copy.
fn word_shingles(text: &str, size: usize) -> HashSet<String> {
    let words = ascii_words(&text.to_lowercase());
    if words.is_empty() {
        return HashSet::new();
    }
    if words.len() <= size {
        return HashSet::from([words.join(" ")]);
    }
    words.windows(size).map(|window| window.join(" ")).collect()
}

/// `rendered_text` shares a word phrase with the artifact `phrase`; the
/// shingle width is the phrase's own token count, capped at four.
pub(super) fn phrase_rendered(phrase: &str, rendered_text: &str) -> bool {
    let words = ascii_words(&plain_text(phrase));
    if words.is_empty() {
        return true;
    }
    let width = words.len().min(4);
    let wanted = word_shingles(&words.join(" "), width);
    let rendered = word_shingles(rendered_text, width);
    wanted.intersection(&rendered).next().is_some()
}

/// Augmenting-path step of the phrase-to-item matching.
fn assign(
    phrase_index: usize,
    seen_items: &mut HashSet<usize>,
    matched_items: &mut HashMap<usize, usize>,
    phrases: &[String],
    items: &[String],
) -> bool {
    for (item_index, item) in items.iter().enumerate() {
        if seen_items.contains(&item_index) || !phrase_rendered(&phrases[phrase_index], item) {
            continue;
        }
        seen_items.insert(item_index);
        let previous = matched_items.get(&item_index).copied();
        let free = match previous {
            None => true,
            Some(other) => assign(other, seen_items, matched_items, phrases, items),
        };
        if free {
            matched_items.insert(item_index, phrase_index);
            return true;
        }
    }
    false
}

/// The artifact phrases that cannot be assigned distinct rendered items.
fn unmatched_phrases(phrases: &[Json], rendered_items: &[String]) -> Vec<String> {
    let texts: Vec<String> = phrases.iter().map(py_str).collect();
    let mut matched_items: HashMap<usize, usize> = HashMap::new();
    for phrase_index in 0..texts.len() {
        assign(
            phrase_index,
            &mut HashSet::new(),
            &mut matched_items,
            &texts,
            rendered_items,
        );
    }
    let matched: HashSet<usize> = matched_items.values().copied().collect();
    texts
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !matched.contains(index))
        .map(|(_, text)| text)
        .collect()
}

/// The consecutive list items from `start`.
fn items_from(entries: &[(Kind, String)], start: usize) -> Vec<String> {
    entries[start..]
        .iter()
        .take_while(|(kind, _)| *kind == Kind::Item)
        .map(|(_, text)| text.clone())
        .collect()
}

/// Items of the rendered list that `phrase` introduces: a heading or prose
/// run carrying the phrase followed by the list, or a list item carrying the
/// phrase as an inline label. `None` when no run carries the phrase and an
/// empty list when nothing renders under it.
pub(super) fn rendered_list_after_heading(
    entries: &[(Kind, String)],
    phrase: &str,
) -> Option<Vec<String>> {
    for wanted in [Kind::Heading, Kind::Text, Kind::Item] {
        for (index, (kind, text)) in entries.iter().enumerate() {
            if *kind != wanted || !text.contains(phrase) {
                continue;
            }
            if *kind == Kind::Item {
                let remainder = text
                    .split_once(phrase)
                    .map_or("", |(_, rest)| rest)
                    .trim_start_matches([' ', ':', ';', ',', '.', '-']);
                let mut items = if remainder.is_empty() {
                    Vec::new()
                } else {
                    vec![remainder.to_string()]
                };
                items.extend(items_from(entries, index + 1));
                return Some(items);
            }
            for (offset, (next_kind, _)) in entries.iter().enumerate().skip(index + 1) {
                match next_kind {
                    Kind::Item => return Some(items_from(entries, offset)),
                    Kind::Heading => break,
                    Kind::Text => {}
                }
            }
            return Some(Vec::new());
        }
    }
    None
}

/// Every string `aws_service` value of the design, depth first.
fn aws_services<'a>(node: &'a Json, out: &mut Vec<&'a str>) {
    match node {
        Json::Object(members) => {
            if let Some(Json::String(service)) = node.get("aws_service") {
                out.push(service);
            }
            for (_, value) in members {
                aws_services(value, out);
            }
        }
        Json::Array(items) => {
            for item in items {
                aws_services(item, out);
            }
        }
        _ => {}
    }
}

fn design_has_clusters(design: Option<&Json>) -> bool {
    matches!(
        design.and_then(|value| value.get("clusters")),
        Some(Json::Array(clusters)) if !clusters.is_empty()
    )
}

fn design_has_deferred_service(design: Option<&Json>) -> bool {
    let Some(design) = design.filter(|value| !is_falsy(value)) else {
        return false;
    };
    let mut services = Vec::new();
    aws_services(design, &mut services);
    services
        .iter()
        .any(|service| py_strip(service).to_lowercase().starts_with("deferred"))
}

/// Workshop compare tables must carry the decision-core column set.
fn what_if_column_errors(section: &str) -> Vec<String> {
    let Some((start, end)) = rx!(r"(?is)<thead\b.*?</thead>").find(section) else {
        return vec![
            "what-if-scenarios must contain a table whose header includes Scenario, Region, HA, \
             Compute, Arch, the three monthly tiers, and Complexity"
                .to_string(),
        ];
    };
    let text = plain_text(&section[start..end]);
    let mut missing: Vec<&str> = Vec::new();
    for (label, present) in [
        ("Scenario", text.contains("scenario")),
        ("Region", text.contains("region")),
        ("HA", text.contains("ha") || text.contains("availability")),
        ("Compute", text.contains("compute")),
        ("Arch", text.contains("arch")),
        ("Complexity", text.contains("complexity")),
    ] {
        if !present {
            missing.push(label);
        }
    }
    if !(text.contains("premium") && text.contains("balanced") && text.contains("optimized")) {
        missing.push("Premium/Balanced/Optimized");
    }
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "what-if-scenarios table is missing decision-core column(s): {} \
         (report-decision-core.md Section 3b)",
        missing.join(", ")
    )]
}

/// The number of scenarios `scenarios/index.json` of the migration directory
/// lists; zero when the file is absent or unreadable.
pub(super) fn scenario_count(migration_dir: Option<&Path>) -> usize {
    let Some(dir) = migration_dir else {
        return 0;
    };
    let path = dir.join("scenarios").join("index.json");
    if !matches!(is_regular_file(&path), Ok(true)) {
        return 0;
    }
    let Ok(text) = read_text(&path, "scenarios/index.json") else {
        return 0;
    };
    let Ok(index) = loads(&text) else {
        return 0;
    };
    match index.get("scenarios") {
        Some(Json::Array(items)) => items.len(),
        Some(Json::Object(members)) => members.len(),
        Some(Json::String(text)) => text.chars().count(),
        _ => 0,
    }
}

/// Fails when artifact fields that the decision core requires are not
/// rendered. The checks fire only when the corresponding artifact data was
/// passed in.
#[allow(clippy::too_many_lines)]
pub(super) fn decision_core_render(
    html: &str,
    infra: Option<&Json>,
    design: Option<&Json>,
    mode: Mode,
    migration_dir: Option<&Path>,
) -> Vec<String> {
    if mode == Mode::AiOnly {
        return Vec::new();
    }
    let mut errors = Vec::new();
    let summary = section_html(html, "decision-summary").unwrap_or_default();
    let summary_text = plain_text(&summary);
    // The rendered view: class tokens and list items come from elements the
    // browser shows, so markup in a <template> or comment cannot satisfy a check.
    let rendered = rendered_fragment(&summary);
    let recommendation = infra.and_then(|value| value.get("recommendation"));
    let field = |name: &str| recommendation.and_then(|value| value.get(name));

    if truthy(recommendation) {
        if !rendered.class_tokens.contains("metric-hero") {
            errors.push(
                "recommendation block exists but decision-summary has no hero metric (render \
                 the AWS run rate and migration shape with class=\"metric-hero\")"
                    .to_string(),
            );
        }
        let outcome = truthy(field("outcome")) || truthy(field("outcome_label"));
        if outcome && !rendered.class_tokens.contains("verdict-headline") {
            errors.push(
                "recommendation.outcome exists but decision-summary has no verdict-headline \
                 (render outcome_label as <p class=\"verdict-headline\">)"
                    .to_string(),
            );
        }
    }

    if let Some(Json::Array(flips)) = field("would_flip_if").filter(|value| !is_falsy(value)) {
        match rendered_list_after_heading(&rendered.entries, "what would flip") {
            None => errors.push(
                "recommendation.would_flip_if is non-empty but decision-summary has no \"What \
                 would flip this\" list"
                    .to_string(),
            ),
            Some(items) => {
                // Every artifact flip condition must be a rendered item.
                let missing = unmatched_phrases(flips, &items);
                if !missing.is_empty() {
                    let quoted: Vec<String> =
                        missing.iter().map(|flip| format!("\"{flip}\"")).collect();
                    errors.push(format!(
                        "\"What would flip this\" list renders {} of {} \
                         recommendation.would_flip_if entries — missing: {}",
                        flips.len() - missing.len(),
                        flips.len(),
                        quoted.join("; ")
                    ));
                }
            }
        }
    }

    if matches!(field("track_outcomes"), Some(Json::Array(tracks)) if !tracks.is_empty())
        && !summary_text.contains("by track")
    {
        errors.push(
            "recommendation.track_outcomes exists but decision-summary has no \"By track\" \
             disposition line"
                .to_string(),
        );
    }

    let conditional = field("outcome").and_then(Json::as_str) == Some("conditional_go");
    if let (true, Some(Json::Array(conditions))) = (
        conditional,
        field("conditions").filter(|value| !is_falsy(value)),
    ) {
        // Conditions render as a checklist: match against list items only.
        let checklist: Vec<&String> = rendered
            .entries
            .iter()
            .filter(|(kind, _)| *kind == Kind::Item)
            .map(|(_, text)| text)
            .collect();
        for condition in conditions {
            let text = py_str(condition);
            if !checklist.iter().any(|item| phrase_rendered(&text, item)) {
                errors.push(
                    "recommendation.conditions is non-empty but decision-summary does not \
                     render that condition as a checklist (a shared phrase from the artifact \
                     condition is missing)"
                        .to_string(),
                );
                break;
            }
        }
    }

    if design_has_deferred_service(design) && !summary_text.contains("specialist engagement") {
        errors.push(
            "aws-design.json maps a service to \"Deferred — specialist engagement\" but \
             decision-summary has no specialist-engagement callout"
                .to_string(),
        );
    }

    if mode == Mode::Full
        && design_has_clusters(design)
        && section_html(html, "exec-architecture").is_none()
    {
        errors.push(
            "aws-design.json has clusters but no <section id=\"exec-architecture\"> (the \
             architecture overview is required in full mode)"
                .to_string(),
        );
    }

    let risks = rx!(r#"(?i)<section\b[^>]*\bid=["']exec-risks["']"#).find(html);
    let assumptions = rx!(r#"(?i)<section\b[^>]*\bid=["']exec-assumptions["']"#).find(html);
    if let (Some((risks_start, _)), Some((assumptions_start, _))) = (risks, assumptions) {
        if assumptions_start < risks_start {
            errors.push(
                "exec-assumptions must follow exec-risks — the assumptions panel is the last \
                 executive section (report-decision-core.md Section 8)"
                    .to_string(),
            );
        }
    }

    if let Some(risks_html) = section_html(html, "exec-risks") {
        if !rx!(r"(?i)<table\b").is_match(&risks_html) {
            errors.push(
                "exec-risks must render risks in a table (impact, likelihood, mitigation), not a \
                 bullet list"
                    .to_string(),
            );
        }
    }

    if scenario_count(migration_dir) >= 2 {
        if let Some(section) = section_html(html, "what-if-scenarios") {
            errors.extend(what_if_column_errors(&section));
        }
    }
    errors
}
