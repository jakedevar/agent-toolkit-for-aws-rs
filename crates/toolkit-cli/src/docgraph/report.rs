//! `wa_review_results.json` and `wa_review_report.md` (`generate_report` of
//! `wa_review.py`).

use std::path::Path;

use toolkit_core::{write_regular_file, Json};

use super::checks::{icon, Check};
use crate::safe_text::safe_path;

/// File names written under the output directory.
pub(super) const RESULTS_FILE: &str = "wa_review_results.json";
pub(super) const REPORT_FILE: &str = "wa_review_report.md";

const PILLARS: [&str; 6] = [
    "Reliability",
    "Security",
    "Operational Excellence",
    "Cost Optimization",
    "Performance Efficiency",
    "Sustainability",
];

/// `{"pass": n, "warn": n, "fail": n, "info": n}` counts of `checks`.
pub(super) fn counts(checks: &[Check]) -> [usize; 4] {
    let mut out = [0; 4];
    for check in checks {
        match check.status.as_str() {
            "pass" => out[0] += 1,
            "warn" => out[1] += 1,
            "fail" => out[2] += 1,
            _ => out[3] += 1,
        }
    }
    out
}

/// The JSON document, two-space indented like `json.dump(indent=2)`.
pub(super) fn results_json(checks: &[Check], cluster_id: &str, timestamp: &str) -> String {
    Json::object(vec![
        ("cluster", Json::string(cluster_id)),
        ("timestamp", Json::string(timestamp)),
        (
            "checks",
            Json::Array(checks.iter().map(Check::to_json).collect()),
        ),
    ])
    .to_pretty_string()
}

/// The Markdown report. `date` is `%Y-%m-%d %H:%M` of the run (UTC).
pub(super) fn markdown(checks: &[Check], cluster_id: &str, date: &str) -> String {
    let [pass, warn, fail, info] = counts(checks);
    let mut lines = vec![
        format!("# Well-Architected Review: {}\n", safe_path(cluster_id)),
        format!("**Date:** {date} UTC\n"),
        format!("**Summary:** {pass} pass, {warn} warnings, {fail} failures, {info} info\n"),
    ];
    for pillar in PILLARS {
        let in_pillar: Vec<&Check> = checks.iter().filter(|c| c.pillar == pillar).collect();
        if in_pillar.is_empty() {
            continue;
        }
        lines.push(format!("\n## {pillar}\n"));
        lines.push("| Status | Check | Detail |".to_string());
        lines.push("|--------|-------|--------|".to_string());
        for check in in_pillar {
            lines.push(format!(
                "| {} | {} | {} |",
                icon(check.status.as_str()),
                check.label.replace('|', "/"),
                check.detail.replace('|', "/")
            ));
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Writes both files under `dir` (created when missing). Returns the two
/// paths as printed by the script (`Path(dir) / name`).
///
/// # Errors
///
/// A reason naming the failing step, never file contents.
pub(super) fn write_report(
    dir: &Path,
    checks: &[Check],
    cluster_id: &str,
    timestamp: &str,
    date: &str,
) -> Result<(String, String), String> {
    std::fs::create_dir_all(dir).map_err(|_| "cannot create the output directory".to_string())?;
    let json_path = dir.join(RESULTS_FILE);
    let md_path = dir.join(REPORT_FILE);
    write_regular_file(&json_path, &results_json(checks, cluster_id, timestamp))
        .map_err(|_| format!("cannot write {RESULTS_FILE}"))?;
    write_regular_file(&md_path, &markdown(checks, cluster_id, date))
        .map_err(|_| format!("cannot write {REPORT_FILE}"))?;
    Ok((shown(dir, RESULTS_FILE), shown(dir, REPORT_FILE)))
}

/// `Path(dir) / name` as `str()` prints it: `.` disappears.
fn shown(dir: &Path, name: &str) -> String {
    let joined = dir.join(name);
    let text = joined.display().to_string();
    text.strip_prefix("./").map_or(text.clone(), str::to_string)
}
