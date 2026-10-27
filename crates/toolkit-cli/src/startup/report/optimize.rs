//! The dedicated Cost Optimization section required when Estimate emitted
//! optimization opportunities.

use toolkit_core::Json;

use super::rx::{group, rx};
use super::structure::{count_of, section_id_counts, tag_stripped_text, toc_hrefs};
use super::values::{py_str, py_str_or_empty};
use super::walk::section_html;
use super::Mode;
use crate::manifests::{is_falsy, py_strip};
use crate::startup::artifacts::pyjson::dumps;

const EXEC_ID: &str = "exec-optimization";
const APPENDIX_ID: &str = "appendix-optimization";

/// The opportunity objects of the infrastructure and AI estimates.
fn opportunities<'a>(infra: Option<&'a Json>, ai: Option<&'a Json>) -> Vec<&'a Json> {
    let mut rows = Vec::new();
    for artifact in [infra, ai].into_iter().flatten() {
        if is_falsy(artifact) {
            continue;
        }
        if let Some(Json::Array(items)) = artifact.get("optimization_opportunities") {
            rows.extend(items.iter().filter(|item| matches!(item, Json::Object(_))));
        }
    }
    rows
}

/// Text of an opportunity used to look for the commitment-eligible services.
fn opportunity_blob(item: &Json) -> String {
    let target_text = match item.get("target_services") {
        Some(Json::Array(targets)) => targets.iter().map(py_str).collect::<Vec<_>>().join(" "),
        other => py_str_or_empty(other),
    };
    [
        py_str_or_empty(item.get("opportunity")),
        py_str_or_empty(item.get("type")),
        py_str_or_empty(item.get("description")),
        target_text,
    ]
    .join(" ")
}

/// Text used only to classify an opportunity as a Savings Plan or RI product:
/// the structured name and type, never the explanatory description.
fn classification_blob(item: &Json) -> String {
    let type_text = py_str_or_empty(item.get("type")).replace('_', " ");
    format!("{} {type_text}", py_str_or_empty(item.get("opportunity")))
}

/// The header of an opportunity table names all five columns.
fn header_has_columns(header_html: &str) -> bool {
    let text = tag_stripped_text(header_html).to_lowercase();
    ["optimization", "target", "commitment", "effort", "saving"]
        .iter()
        .all(|word| text.contains(word))
}

/// True if ANY `<thead>` of the section has the required columns.
fn has_optimization_columns(section: &str) -> bool {
    rx!(r"(?is)<thead\b[^>]*>(.*?)</thead>")
        .captures_all(section)
        .iter()
        .any(|found| header_has_columns(group(section, found, 1)))
}

/// First-column text of each data row of the opportunity tables.
fn opportunity_cells(section: &str) -> Vec<String> {
    let mut cells = Vec::new();
    for table_groups in rx!(r"(?is)<table\b[^>]*>(.*?)</table>").captures_all(section) {
        let table = group(section, &table_groups, 1);
        let Some(head) = rx!(r"(?is)<thead\b[^>]*>(.*?)</thead>").captures(table) else {
            continue;
        };
        if !header_has_columns(group(table, &head, 1)) {
            continue;
        }
        for body_groups in rx!(r"(?is)<tbody\b[^>]*>(.*?)</tbody>").captures_all(table) {
            let body = group(table, &body_groups, 1);
            for row_groups in rx!(r"(?is)<tr\b[^>]*>(.*?)</tr>").captures_all(body) {
                let row = group(body, &row_groups, 1);
                if let Some(cell) = rx!(r"(?is)<t[dh]\b[^>]*>(.*?)</t[dh]>").captures(row) {
                    cells.push(py_strip(&tag_stripped_text(group(row, &cell, 1))).to_string());
                }
            }
        }
    }
    cells
}

fn design_has_commitment_eligible(design: Option<&Json>) -> bool {
    let Some(design) = design.filter(|value| !is_falsy(value)) else {
        return false;
    };
    rx!(r"(?i)\b(?:rds|aurora|fargate|lambda|elasticache|dynamodb|ec2)\b")
        .is_match(&dumps(design, None, false))
}

/// A dedicated section is required when Estimate emitted opportunities.
#[allow(clippy::too_many_lines)]
pub(super) fn optimization_sections(
    html: &str,
    infra: Option<&Json>,
    ai: Option<&Json>,
    design: Option<&Json>,
    mode: Mode,
    require_toc: bool,
) -> Vec<String> {
    let rows = opportunities(infra, ai);
    if rows.is_empty() {
        return Vec::new();
    }
    let mut errors = Vec::new();
    let counts = section_id_counts(html);
    let exec_n = count_of(&counts, EXEC_ID);
    let appendix_n = count_of(&counts, APPENDIX_ID);
    let full = mode == Mode::Full;

    if exec_n == 0 {
        errors.push(format!(
            "optimization_opportunities exist but no <section id=\"{EXEC_ID}\"> — render a \
             standalone Cost Optimization section (Balanced on-demand vs Savings Plans / \
             Reserved Instances). A table buried only in appendix-costs does not satisfy this \
             gate"
        ));
    } else if exec_n > 1 {
        errors.push(format!(
            "duplicate <section id=\"{EXEC_ID}\"> ({exec_n} occurrences)"
        ));
    }
    if full {
        if appendix_n == 0 {
            errors.push(format!(
                "optimization_opportunities exist but no <section id=\"{APPENDIX_ID}\"> — the \
                 opportunity table must be its own appendix section, not only a subsection of \
                 appendix-costs"
            ));
        } else if appendix_n > 1 {
            errors.push(format!(
                "duplicate <section id=\"{APPENDIX_ID}\"> ({appendix_n} occurrences)"
            ));
        }
    }
    if require_toc {
        let hrefs = toc_hrefs(html);
        if !hrefs.is_empty() {
            if exec_n > 0 && !hrefs.iter().any(|href| href == EXEC_ID) {
                errors.push(format!(
                    "TOC missing link to required section id=\"{EXEC_ID}\" (add <a \
                     href=\"#{EXEC_ID}\">)"
                ));
            }
            if full && appendix_n > 0 && !hrefs.iter().any(|href| href == APPENDIX_ID) {
                errors.push(format!(
                    "TOC missing link to required section id=\"{APPENDIX_ID}\" (add <a \
                     href=\"#{APPENDIX_ID}\">)"
                ));
            }
        }
    }

    let exec_html = section_html(html, EXEC_ID).unwrap_or_default();
    let appendix_html = section_html(html, APPENDIX_ID).unwrap_or_default();
    let table_scope = if full { &appendix_html } else { &exec_html };
    if !table_scope.is_empty() && !has_optimization_columns(table_scope) {
        let location = if full { APPENDIX_ID } else { EXEC_ID };
        errors.push(format!(
            "{location} table must include Optimization, Target, Monthly savings (or Est. \
             savings), Commitment, and Effort columns"
        ));
    }
    if !exec_html.is_empty() {
        if !rx!(r"(?i)balanced").is_match(&exec_html) {
            errors.push(
                "exec-optimization must compare commitment savings to the Balanced on-demand \
                 baseline"
                    .to_string(),
            );
        }
        let non_stack = rx!(
            r"(?i)(?:not\s+additive|do\s+not\s+add|already\s+embed|already\s+includes|not\s+a\s+sum|already\s+assume)"
        );
        if rx!(r"(?i)optimized").is_match(&exec_html) && !non_stack.is_match(&exec_html) {
            errors.push(
                "exec-optimization mentions Optimized but does not warn that Savings Plans / RI \
                 savings are incremental to Balanced and must not be added on top of the \
                 Optimized tier"
                    .to_string(),
            );
        }
    }

    // Classified from the structured name and type only.
    let savings_plan = rx!(r"(?i)savings\s*plans?");
    let reserved = rx!(r"(?i)reserved\s+(?:instances?|capacity|nodes?)");
    let has_sp_opp = rows
        .iter()
        .any(|item| savings_plan.is_match(&classification_blob(item)));
    let has_ri_opp = rows
        .iter()
        .any(|item| reserved.is_match(&classification_blob(item)));

    // Presence must be a rendered opportunity row of the opportunity table.
    let mut cells = opportunity_cells(&appendix_html);
    cells.extend(opportunity_cells(&exec_html));
    let row_text = cells.join("\n");

    if has_sp_opp && !savings_plan.is_match(&row_text) {
        errors.push(
            "optimization_opportunities include a Savings Plan but the opportunity table \
             (appendix-optimization / exec-optimization) has no Savings Plans data row — a \
             heading, caveat, or posture-comparison row does not satisfy this gate"
                .to_string(),
        );
    }
    if has_ri_opp && !reserved.is_match(&row_text) {
        errors.push(
            "optimization_opportunities include a Reserved Instance (or reserved \
             capacity/nodes) but the opportunity table (appendix-optimization / \
             exec-optimization) has no Reserved Instances data row — a heading, caveat, or \
             posture-comparison row does not satisfy this gate"
                .to_string(),
        );
    }
    let commitment_service = rx!(r"(?i)\b(?:rds|aurora|fargate|lambda|elasticache|dynamodb|ec2)\b");
    let targets_eligible = rows
        .iter()
        .any(|item| commitment_service.is_match(&opportunity_blob(item)));
    if !has_sp_opp
        && !has_ri_opp
        && (design_has_commitment_eligible(design) || targets_eligible)
        && !(savings_plan.is_match(&row_text) || reserved.is_match(&row_text))
    {
        errors.push(
            "RDS, Aurora, Fargate, or Lambda is in the design (or opportunity targets) but the \
             opportunity table has no Savings Plans or Reserved Instances row"
                .to_string(),
        );
    }
    errors
}
