//! Vendored-copy parity gate (`toolkit sync vendored`, port of
//! `plugins/aws-startup-advisor/tools/sync-vendored.py`, #4377).
//!
//! Each skill folder of a plugin must be installable on its own, so shared
//! material is *vendored* (copied) into
//! `skills/<skill>/references/vendored/<path>` from the canonical
//! `skills/shared/<path>`. Two rules, both enforced in check and write mode:
//!
//! 1. Every managed file under a skill's `references/vendored/` is
//!    byte-identical to `skills/shared/<same path>`. A file is managed unless
//!    it is named `README.md` (at any depth) or lives below a top-level
//!    directory in [`SKIP_TOP_LEVEL_DIRS`].
//! 2. The vendored directory's `README.md` carries a
//!    ``| `Vendored path` | `Canonical source` |`` table: every row exists on
//!    disk, every managed file has a row, and the canonical source of a row
//!    is exactly `skills/shared/<same path>`.
//!
//! Check mode never writes. Write mode (a) restores copies the README lists
//! that are missing on disk and (b) overwrites drifted copies with the
//! canonical bytes, each through [`copy_file_atomic`]. It never deletes
//! anything: an unlisted extra file or a copy without a canonical source stays
//! and is reported as an error. A new `skills/shared/` file nobody vendors yet
//! is not drift (a skill vendors only what it loads).
//!
//! Safety additions over the Python original (recorded in
//! `docs/migration/sync-contracts.md`): README row paths are untrusted and must
//! already be normalized relative paths, so a row can never name a location
//! outside the vendored directory; nothing is read or written through a
//! symbolic link at any level below the plugin root; a vendored link or
//! special file is drift that write mode replaces atomically.
//!
//! # Telemetry seam (#4398)
//!
//! The `telemetry/` subtree of each vendored directory is generated from
//! `scripts/telemetry/` by a separate gate and is excluded here through
//! [`SKIP_TOP_LEVEL_DIRS`] / [`TELEMETRY_BUNDLE_DIR`]. The telemetry port
//! adds its own bundle sync on [`crate::treesync`] and updates that single
//! constant in the same change.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::ToolkitError;
use crate::paths::{normalize_relative, read_regular_file, MAX_TEXT_FILE_BYTES};
use crate::treesync::{
    copy_file_atomic, ensure_dir_chain, files_equal, list_children, list_tree, path_state, Kind,
};

/// Name of the shared canonical directory under a plugin's `skills/`.
pub const SHARED_DIRNAME: &str = "shared";
/// Vendored directory relative to a skill folder.
pub const VENDORED_SUBDIR: &str = "references/vendored";
/// File names (at any depth) this gate does not manage.
pub const SKIP_NAMES: &[&str] = &["README.md"];
/// The independently generated telemetry bundle (owned by #4398's gate).
pub const TELEMETRY_BUNDLE_DIR: &str = "telemetry";
/// Top-level vendored directories this gate does not manage.
pub const SKIP_TOP_LEVEL_DIRS: &[&str] = &[TELEMETRY_BUNDLE_DIR];

/// Result of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendoredReport {
    /// Managed vendored files found before any change was made.
    pub total_files: usize,
    /// Files restored or overwritten (write mode).
    pub copied: usize,
    /// Progress lines (`  synced ...`, `  restored ...`), write mode only.
    pub progress: Vec<String>,
    /// Contract violations, in report order.
    pub errors: Vec<String>,
}

impl VendoredReport {
    /// Whether the plugin satisfies the contract.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Final stdout lines after the progress lines: the `PASS`/`OK` line, or
    /// `FAIL` plus one `  - error` line each.
    #[must_use]
    pub fn summary(&self, check: bool) -> Vec<String> {
        if !self.errors.is_empty() {
            let mut lines = vec![format!(
                "FAIL — {} vendored file(s) out of contract:",
                self.errors.len()
            )];
            lines.extend(self.errors.iter().map(|error| format!("  - {error}")));
            lines
        } else if check {
            vec![format!(
                "PASS — {} vendored file(s) byte-identical to skills/shared/ and matching their README tables",
                self.total_files
            )]
        } else {
            vec![format!(
                "OK — {} vendored file(s) checked, {} synced",
                self.total_files, self.copied
            )]
        }
    }
}

struct Skill {
    /// `skills/<name>/references/vendored`, plugin-relative.
    vend_rel: String,
}

/// Plugin-relative path of a vendored file: `skills/<name>/references/vendored/<rel>`.
fn vend_path(skill: &Skill, rel: &str) -> String {
    format!("{}/{rel}", skill.vend_rel)
}

fn canon_path(rel: &str) -> String {
    format!("skills/{SHARED_DIRNAME}/{rel}")
}

fn is_managed(rel: &str, kind: Kind) -> bool {
    if kind == Kind::Dir {
        return false;
    }
    let mut parts = rel.split('/');
    let first = parts.next().unwrap_or("");
    let last = rel.rsplit('/').next().unwrap_or("");
    !SKIP_NAMES.contains(&last) && !SKIP_TOP_LEVEL_DIRS.contains(&first)
}

fn list_skills(plugin_root: &Path, errors: &mut Vec<String>) -> Result<Vec<Skill>, ToolkitError> {
    let mut out = Vec::new();
    match path_state(plugin_root, "skills")? {
        Some(Kind::Dir) => {}
        Some(Kind::Symlink) => {
            errors.push("skills: is a symbolic link; refusing to follow it".to_string());
            return Ok(out);
        }
        _ => return Ok(out),
    }
    for child in list_children(&plugin_root.join("skills"))? {
        if child.rel == SHARED_DIRNAME {
            continue;
        }
        let vend_rel = format!("skills/{}/{VENDORED_SUBDIR}", child.rel);
        match path_state(plugin_root, &vend_rel)? {
            Some(Kind::Dir) => out.push(Skill { vend_rel }),
            Some(Kind::Symlink) => {
                errors.push(format!(
                    "{vend_rel}: a path component is a symbolic link; refusing to follow it"
                ));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Managed entries of one skill's vendored directory, sorted.
fn managed_files(
    plugin_root: &Path,
    skill: &Skill,
    errors: &mut Vec<String>,
) -> Vec<(String, Kind)> {
    match list_tree(&plugin_root.join(&skill.vend_rel)) {
        Ok(entries) => entries
            .into_iter()
            .filter(|entry| is_managed(&entry.rel, entry.kind))
            .map(|entry| (entry.rel, entry.kind))
            .collect(),
        Err(error) => {
            errors.push(format!("{}: {}", skill.vend_rel, error.message()));
            Vec::new()
        }
    }
}

/// Parses one ``| `a` | `b` |`` table row.
fn parse_row(line: &str) -> Option<(String, String)> {
    fn cell(text: &str) -> Option<(&str, &str)> {
        let text = text.trim_start().strip_prefix('`')?;
        let end = text.find('`')?;
        (end > 0).then(|| (&text[..end], &text[end + 1..]))
    }
    let rest = line.strip_prefix('|')?;
    let (first, rest) = cell(rest)?;
    let rest = rest.trim_start().strip_prefix('|')?;
    let (second, rest) = cell(rest)?;
    let rest = rest.trim_start().strip_prefix('|')?;
    rest.trim()
        .is_empty()
        .then(|| (first.trim().to_string(), second.trim().to_string()))
}

/// Reads the README table of one vendored directory: the vendored path to
/// canonical source mapping (rows with unsafe paths are dropped and
/// reported) plus errors.
fn readme_manifest(plugin_root: &Path, skill: &Skill) -> (BTreeMap<String, String>, Vec<String>) {
    let readme_rel = vend_path(skill, "README.md");
    let missing = || {
        (
            BTreeMap::new(),
            vec![format!(
                "{}: no README.md — the vendored-path table lives there",
                skill.vend_rel
            )],
        )
    };
    let state = match path_state(plugin_root, &readme_rel) {
        Ok(state) => state,
        Err(error) => {
            return (
                BTreeMap::new(),
                vec![format!("{readme_rel}: {}", error.message())],
            )
        }
    };
    match state {
        Some(Kind::File) => {}
        Some(Kind::Symlink) => {
            return (
                BTreeMap::new(),
                vec![format!(
                    "{readme_rel}: is a symbolic link; refusing to read it"
                )],
            );
        }
        _ => return missing(),
    }
    let text = match read_regular_file(&plugin_root.join(&readme_rel), MAX_TEXT_FILE_BYTES) {
        Ok(text) => text,
        Err(error) => return (BTreeMap::new(), vec![format!("{readme_rel}: {error}")]),
    };
    let raw: Vec<(String, String)> = text.lines().filter_map(parse_row).collect();
    if raw.is_empty() {
        return (
            BTreeMap::new(),
            vec![format!(
                "{readme_rel}: no `| Vendored path | Canonical source |` table rows found"
            )],
        );
    }
    let mut rows = BTreeMap::new();
    let mut errors = Vec::new();
    for (rel, canonical) in raw {
        match normalize_relative(&rel) {
            Ok(normal) if normal == rel => {
                rows.insert(rel, canonical);
            }
            Ok(normal) => errors.push(format!(
                "{readme_rel}: row `{rel}` is not a normalized relative path (use `{normal}`)"
            )),
            Err(error) => errors.push(format!(
                "{readme_rel}: row `{rel}` is not a safe relative path ({error})"
            )),
        }
    }
    (rows, errors)
}

/// Rule 2: README table against files on disk, both directions, with the
/// expected canonical path for each row.
fn check_manifest(plugin_root: &Path, skills: &[Skill], errors: &mut Vec<String>) {
    for skill in skills {
        let on_disk: Vec<String> = managed_files(plugin_root, skill, &mut Vec::new())
            .into_iter()
            .map(|(rel, _)| rel)
            .collect();
        let readme_is_file = matches!(
            path_state(plugin_root, &vend_path(skill, "README.md")),
            Ok(Some(Kind::File))
        );
        if !readme_is_file && on_disk.is_empty() {
            // A vendored tree owned entirely by another generator
            // (currently telemetry) has no skills/shared manifest to check.
            continue;
        }
        let (rows, row_errors) = readme_manifest(plugin_root, skill);
        errors.extend(row_errors);
        if rows.is_empty() {
            continue;
        }
        let readme = vend_path(skill, "README.md");
        for rel in rows.keys().filter(|rel| !on_disk.contains(*rel)) {
            errors.push(format!(
                "{}: listed in {readme} but missing on disk — the copy was deleted; \
                 restore it with `toolkit sync vendored` or remove the row",
                vend_path(skill, rel)
            ));
        }
        let mut unlisted: Vec<&String> = on_disk
            .iter()
            .filter(|rel| !rows.contains_key(*rel))
            .collect();
        unlisted.sort();
        for rel in unlisted {
            errors.push(format!(
                "{}: on disk but not listed in {readme} — add a row",
                vend_path(skill, rel)
            ));
        }
        for (rel, canonical) in &rows {
            let expected = canon_path(rel);
            if *canonical != expected {
                errors.push(format!(
                    "{readme}: row for `{rel}` names canonical `{canonical}`, expected `{expected}`"
                ));
            }
        }
    }
}

/// Rule 1 in check mode: one error per drifted or orphaned managed file.
fn check_files(plugin_root: &Path, skills: &[Skill], errors: &mut Vec<String>) {
    for skill in skills {
        for (rel, kind) in managed_files(plugin_root, skill, errors) {
            let vend = vend_path(skill, &rel);
            let canon = canon_path(&rel);
            if let Err(message) = canonical_problem(plugin_root, &canon) {
                errors.push(format!(
                    "{vend}: no canonical source at {canon} — either add the file to \
                     skills/shared/ or stop vendoring it{message}"
                ));
            } else if kind != Kind::File {
                errors.push(format!(
                    "{vend}: is a {}, not a regular file — run `toolkit sync vendored`",
                    kind.describe()
                ));
            } else {
                match files_equal(&plugin_root.join(&vend), &plugin_root.join(&canon)) {
                    Ok(true) => {}
                    Ok(false) => errors.push(format!(
                        "{vend}: differs from {canon} — run `toolkit sync vendored`"
                    )),
                    Err(error) => errors.push(format!("{vend}: {}", error.message())),
                }
            }
        }
    }
}

/// `Ok(())` when `canon` is a regular file reached without any link;
/// otherwise a message suffix explaining a refusal (empty for plain missing).
fn canonical_problem(plugin_root: &Path, canon: &str) -> Result<(), String> {
    match path_state(plugin_root, canon) {
        Ok(Some(Kind::File)) => Ok(()),
        Ok(Some(Kind::Symlink)) => Err(" (a path component is a symbolic link; \
                                      refusing to follow it)"
            .to_string()),
        Ok(Some(other)) => Err(format!(" (it is a {})", other.describe())),
        Ok(None) => Err(String::new()),
        Err(error) => Err(format!(" ({})", error.message())),
    }
}

/// Write mode step 1: restore listed copies that are missing on disk.
fn restore_missing(plugin_root: &Path, skills: &[Skill], report: &mut VendoredReport) {
    for skill in skills {
        let (rows, _) = readme_manifest(plugin_root, skill);
        for rel in rows.keys() {
            if !is_managed(rel, Kind::File) {
                continue;
            }
            let target = vend_path(skill, rel);
            let canon = canon_path(rel);
            if !matches!(path_state(plugin_root, &target), Ok(None)) {
                continue;
            }
            if canonical_problem(plugin_root, &canon).is_err() {
                continue;
            }
            let parent = rel.rsplit_once('/').map_or("", |(parent, _)| parent);
            let result =
                ensure_dir_chain(&plugin_root.join(&skill.vend_rel), parent).and_then(|()| {
                    copy_file_atomic(&plugin_root.join(&canon), &plugin_root.join(&target))
                });
            match result {
                Ok(()) => {
                    report.copied += 1;
                    report
                        .progress
                        .push(format!("  restored {target} <- {canon}"));
                }
                Err(error) => report.errors.push(format!("{target}: {}", error.message())),
            }
        }
    }
}

/// Write mode step 2: overwrite drifted copies.
fn overwrite_drifted(plugin_root: &Path, skills: &[Skill], report: &mut VendoredReport) {
    for skill in skills {
        for (rel, kind) in managed_files(plugin_root, skill, &mut report.errors) {
            let vend = vend_path(skill, &rel);
            let canon = canon_path(&rel);
            if let Err(message) = canonical_problem(plugin_root, &canon) {
                report.errors.push(format!(
                    "{vend}: no canonical source at {canon} — cannot sync; add it to \
                     skills/shared/ or delete the copy{message}"
                ));
                continue;
            }
            let differs = if kind == Kind::File {
                match files_equal(&plugin_root.join(&vend), &plugin_root.join(&canon)) {
                    Ok(equal) => !equal,
                    Err(error) => {
                        report.errors.push(format!("{vend}: {}", error.message()));
                        continue;
                    }
                }
            } else {
                true
            };
            if differs {
                match copy_file_atomic(&plugin_root.join(&canon), &plugin_root.join(&vend)) {
                    Ok(()) => {
                        report.copied += 1;
                        report.progress.push(format!("  synced {vend} <- {canon}"));
                    }
                    Err(error) => report.errors.push(format!("{vend}: {}", error.message())),
                }
            }
        }
    }
}

/// Runs the gate on `plugin_root` (the plugin directory, e.g.
/// `plugins/aws-startup-advisor`).
///
/// # Errors
///
/// Returns a runtime error when `plugin_root` is not a directory or a
/// directory cannot be listed. Contract violations are collected in
/// [`VendoredReport::errors`].
pub fn sync_vendored(plugin_root: &Path, check: bool) -> Result<VendoredReport, ToolkitError> {
    if !plugin_root.is_dir() {
        return Err(ToolkitError::runtime(
            "plugin_root_not_found",
            format!("plugin root is not a directory: {}", plugin_root.display()),
        ));
    }
    let mut report = VendoredReport::default();
    let mut structural = Vec::new();
    let listed = list_skills(plugin_root, &mut structural)?;
    report.total_files = listed
        .iter()
        .map(|skill| managed_files(plugin_root, skill, &mut Vec::new()).len())
        .sum();
    if check {
        report.errors.extend(structural);
        check_files(plugin_root, &listed, &mut report.errors);
    } else {
        restore_missing(plugin_root, &listed, &mut report);
        overwrite_drifted(plugin_root, &listed, &mut report);
        let mut errors = structural;
        errors.append(&mut report.errors);
        report.errors = errors;
    }
    check_manifest(plugin_root, &listed, &mut report.errors);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    const VEND: &str = "skills/alpha/references/vendored";

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    fn read(root: &Path, rel: &str) -> String {
        fs::read_to_string(root.join(rel)).expect("read")
    }

    /// Port of the Python `_tree` fixture: one in-sync copy, one drifted
    /// copy, one orphan with no canonical source, a skill that vendors
    /// nothing and the shared directory itself.
    fn tree() -> TempDir {
        let plugin = TempDir::new("vd");
        write(
            &plugin,
            "skills/shared/dsl/INTERPRETER.md",
            "canonical v2\n",
        );
        write(&plugin, "skills/shared/pricing.json", "{\"a\": 1}\n");
        write(
            &plugin,
            &format!("{VEND}/dsl/INTERPRETER.md"),
            "canonical v2\n",
        );
        write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 0}\n");
        write(&plugin, &format!("{VEND}/orphan.md"), "no source\n");
        write(
            &plugin,
            &format!("{VEND}/README.md"),
            "# Vendored\n\n\
             | Vendored path | Canonical source |\n\
             | --- | --- |\n\
             | `dsl/INTERPRETER.md` | `skills/shared/dsl/INTERPRETER.md` |\n\
             | `pricing.json` | `skills/shared/pricing.json` |\n\
             | `orphan.md` | `skills/shared/orphan.md` |\n",
        );
        fs::create_dir_all(plugin.join("skills/beta")).expect("mkdir");
        plugin
    }

    fn drop_orphan(plugin: &Path) {
        fs::remove_file(plugin.join(format!("{VEND}/orphan.md"))).expect("rm");
        let readme = format!("{VEND}/README.md");
        let kept: Vec<String> = read(plugin, &readme)
            .lines()
            .filter(|line| !line.contains("orphan"))
            .map(str::to_string)
            .collect();
        write(plugin, &readme, &(kept.join("\n") + "\n"));
    }

    fn errors_of(plugin: &Path, check: bool) -> Vec<String> {
        sync_vendored(plugin, check).expect("run").errors
    }

    #[test]
    fn check_reports_drift_and_orphans_but_not_readme() {
        let plugin = tree();
        let errors = errors_of(&plugin, true);
        let joined = errors.join("\n");
        assert!(joined.contains("pricing.json: differs from"), "{joined}");
        assert!(
            joined.contains("orphan.md: no canonical source"),
            "{joined}"
        );
        assert!(!joined.contains("INTERPRETER.md"), "{joined}");
        assert!(!joined.contains("README.md"), "{joined}");
        assert_eq!(errors.len(), 2);
        assert_eq!(
            read(&plugin, &format!("{VEND}/pricing.json")),
            "{\"a\": 0}\n",
            "check never writes"
        );
    }

    #[test]
    fn sync_copies_canonical_over_drifted_and_leaves_orphans_as_errors() {
        let plugin = tree();
        let report = sync_vendored(&plugin, false).expect("run");
        assert_eq!(report.copied, 1);
        assert_eq!(
            read(&plugin, &format!("{VEND}/pricing.json")),
            "{\"a\": 1}\n"
        );
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("orphan.md"));
        assert_eq!(
            report.progress,
            [format!(
                "  synced {VEND}/pricing.json <- skills/shared/pricing.json"
            )]
        );
        assert!(
            read(&plugin, &format!("{VEND}/orphan.md")).contains("no source"),
            "orphan kept"
        );
        let remaining: Vec<String> = errors_of(&plugin, true)
            .into_iter()
            .filter(|error| !error.contains("orphan"))
            .collect();
        assert!(remaining.is_empty(), "{remaining:?}");
    }

    #[test]
    fn deleted_copy_is_caught_via_readme_table_and_restored_by_sync() {
        let plugin = tree();
        fs::remove_file(plugin.join(format!("{VEND}/dsl/INTERPRETER.md"))).expect("rm");
        let errors = errors_of(&plugin, true);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("dsl/INTERPRETER.md") && e.contains("missing on disk")),
            "{errors:?}"
        );
        let report = sync_vendored(&plugin, false).expect("run");
        assert_eq!(
            read(&plugin, &format!("{VEND}/dsl/INTERPRETER.md")),
            "canonical v2\n"
        );
        assert!(report.copied >= 1);
        assert!(report
            .progress
            .iter()
            .any(|line| line.starts_with("  restored ") && line.contains("dsl/INTERPRETER.md")));
        assert!(!report.errors.iter().any(|e| e.contains("missing on disk")));
    }

    #[test]
    fn unlisted_copy_and_wrong_canonical_path_are_caught() {
        let plugin = tree();
        write(&plugin, &format!("{VEND}/extra.md"), "x\n");
        let readme = format!("{VEND}/README.md");
        let text = read(&plugin, &readme).replace(
            "`skills/shared/pricing.json`",
            "`skills/other/pricing.json`",
        );
        write(&plugin, &readme, &text);
        let errors = errors_of(&plugin, true);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("extra.md") && e.contains("not listed")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("pricing.json")
                && e.contains("expected `skills/shared/pricing.json`")),
            "{errors:?}"
        );
    }

    #[test]
    fn new_shared_file_nobody_vendors_is_not_an_error() {
        let plugin = tree();
        write(&plugin, "skills/shared/brand-new.md", "new\n");
        drop_orphan(&plugin);
        write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 1}\n");
        assert_eq!(errors_of(&plugin, true), Vec::<String>::new());
    }

    #[test]
    fn telemetry_bundle_is_owned_by_its_separate_sync_gate() {
        let plugin = tree();
        write(
            &plugin,
            &format!("{VEND}/telemetry/PROTOCOL.md"),
            "generated bundle\n",
        );
        write(
            &plugin,
            &format!("{VEND}/telemetry/version.json"),
            "{\"version\": \"1.0.0\"}\n",
        );
        drop_orphan(&plugin);
        write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 1}\n");
        write(
            &plugin,
            "skills/telemetry-only/references/vendored/telemetry/cli.py",
            "# generated bundle\n",
        );
        let report = sync_vendored(&plugin, true).expect("run");
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(report.total_files, 2, "telemetry files are not counted");
        // Write mode leaves the bundle alone too, even when it differs from shared/.
        write(&plugin, "skills/shared/telemetry/PROTOCOL.md", "other\n");
        sync_vendored(&plugin, false).expect("sync");
        assert_eq!(
            read(&plugin, &format!("{VEND}/telemetry/PROTOCOL.md")),
            "generated bundle\n"
        );
    }

    #[test]
    fn readme_rows_for_unmanaged_paths_are_never_restored() {
        let plugin = tree();
        drop_orphan(&plugin);
        write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 1}\n");
        write(
            &plugin,
            "skills/shared/telemetry/cli.py",
            "canonical telemetry\n",
        );
        write(&plugin, "skills/shared/README.md", "canonical readme\n");
        let readme = format!("{VEND}/README.md");
        let text = read(&plugin, &readme)
            + "| `telemetry/cli.py` | `skills/shared/telemetry/cli.py` |\n"
            + "| `sub/README.md` | `skills/shared/sub/README.md` |\n";
        write(&plugin, &readme, &text);
        let report = sync_vendored(&plugin, false).expect("run");
        assert!(!plugin.join(format!("{VEND}/telemetry/cli.py")).exists());
        assert!(!plugin.join(format!("{VEND}/sub")).exists());
        assert_eq!(report.copied, 0);
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("telemetry/cli.py") && e.contains("missing on disk")));
    }

    #[test]
    fn plugin_without_a_skills_directory_passes_and_a_missing_root_fails() {
        let plugin = TempDir::new("vd-mcp-only");
        let report = sync_vendored(&plugin, true).expect("run");
        assert_eq!(report.total_files, 0);
        assert_eq!(
            report.summary(true),
            ["PASS — 0 vendored file(s) byte-identical to skills/shared/ and matching their README tables"]
        );
        let error = sync_vendored(&plugin.join("absent"), true).expect_err("missing");
        assert_eq!(error.code(), "plugin_root_not_found");
        assert_eq!(error.exit_code(), 1);
    }

    #[test]
    fn summaries_match_the_published_lines() {
        let plugin = tree();
        let fail = sync_vendored(&plugin, true).expect("run");
        let lines = fail.summary(true);
        assert_eq!(lines[0], "FAIL — 2 vendored file(s) out of contract:");
        assert!(lines[1].starts_with("  - "));
        assert!(!fail.is_ok());
        drop_orphan(&plugin);
        let synced = sync_vendored(&plugin, false).expect("sync");
        assert_eq!(
            synced.summary(false),
            ["OK — 2 vendored file(s) checked, 1 synced"]
        );
        let pass = sync_vendored(&plugin, true).expect("check");
        assert!(pass.is_ok());
        assert_eq!(
            pass.summary(true),
            ["PASS — 2 vendored file(s) byte-identical to skills/shared/ and matching their README tables"]
        );
        // total_files is counted before the sync copies anything.
        write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 9}\n");
        let again = sync_vendored(&plugin, false).expect("sync");
        assert_eq!(
            again.summary(false),
            ["OK — 2 vendored file(s) checked, 1 synced"]
        );
    }

    #[test]
    fn missing_readme_empty_table_and_bad_rows_are_reported() {
        let plugin = tree();
        let readme = format!("{VEND}/README.md");
        fs::remove_file(plugin.join(&readme)).expect("rm");
        let errors = errors_of(&plugin, true);
        assert!(errors
            .iter()
            .any(|e| e.contains("no README.md — the vendored-path table lives there")));
        write(&plugin, &readme, "# nothing here\n| not | a row |\n");
        let errors = errors_of(&plugin, true);
        assert!(errors
            .iter()
            .any(|e| e.contains("no `| Vendored path | Canonical source |` table rows found")));

        write(
            &plugin,
            &readme,
            "| `../escape.md` | `skills/shared/../escape.md` |\n\
             | `/abs.md` | `x` |\n\
             | `./pricing.json` | `skills/shared/pricing.json` |\n\
             | `pricing.json` | `skills/shared/pricing.json` |\n",
        );
        let errors = errors_of(&plugin, true);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("row `../escape.md` is not a safe relative path")),
            "{errors:?}"
        );
        assert!(errors
            .iter()
            .any(|e| e.contains("row `/abs.md` is not a safe relative path")));
        assert!(errors.iter().any(|e| e.contains(
            "row `./pricing.json` is not a normalized relative path (use `pricing.json`)"
        )));
    }

    #[test]
    fn traversal_rows_never_write_outside_the_vendored_directory() {
        let plugin = tree();
        write(
            &plugin,
            "skills/shared/escape-vendored-traversal.md",
            "canonical\n",
        );
        let readme = format!("{VEND}/README.md");
        let text = read(&plugin, &readme) + "| `../../../../../escape-vendored-traversal.md` | `skills/shared/escape-vendored-traversal.md` |\n";
        write(&plugin, &readme, &text);
        let report = sync_vendored(&plugin, false).expect("run");
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("is not a safe relative path")));
        let parent = plugin
            .parent()
            .expect("temp parent")
            .join("escape-vendored-traversal.md");
        assert!(!parent.exists());
        assert!(!plugin.join("escape-vendored-traversal.md").exists());
    }

    #[test]
    fn parse_row_accepts_only_the_two_backticked_cell_shape() {
        assert_eq!(
            parse_row("|  `a/b`  |   `c`   |   "),
            Some(("a/b".to_string(), "c".to_string()))
        );
        assert_eq!(parse_row("| `a` | `b` | `extra` |"), None);
        assert_eq!(parse_row("| a | `b` |"), None);
        assert_eq!(parse_row("| `` | `b` |"), None);
        assert_eq!(parse_row("`a` | `b` |"), None);
        assert_eq!(parse_row("| `a` | `b`"), None);
        assert_eq!(parse_row("| Vendored path | Canonical source |"), None);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed_read_or_written() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("vd-out");
        write(&outside, "secret.md", "TOP SECRET\n");
        write(&outside, "dir/data.md", "data\n");
        let plugin = tree();
        drop_orphan(&plugin);
        write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 1}\n");
        // A vendored file replaced by a link to an outside file...
        fs::remove_file(plugin.join(format!("{VEND}/dsl/INTERPRETER.md"))).expect("rm");
        symlink(
            outside.join("secret.md"),
            plugin.join(format!("{VEND}/dsl/INTERPRETER.md")),
        )
        .expect("link");
        // ...and an extra linked directory.
        symlink(
            outside.join("dir"),
            plugin.join(format!("{VEND}/linkeddir")),
        )
        .expect("link");
        let errors = errors_of(&plugin, true);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("dsl/INTERPRETER.md: is a symbolic link, not a regular file")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("linkeddir: no canonical source")),
            "{errors:?}"
        );
        // Write mode replaces the link with the canonical file; the outside file is untouched.
        let report = sync_vendored(&plugin, false).expect("sync");
        assert_eq!(read(&outside, "secret.md"), "TOP SECRET\n");
        assert_eq!(
            read(&plugin, &format!("{VEND}/dsl/INTERPRETER.md")),
            "canonical v2\n"
        );
        assert_eq!(report.copied, 1);
        assert!(read(&outside, "dir/data.md") == "data\n");
        assert!(
            report.errors.iter().any(|e| e.contains("linkeddir")),
            "unlisted link stays an error"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_canonical_source_or_shared_directory_is_refused() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("vd-out-canon");
        write(&outside, "pricing.json", "{\"a\": \"outside\"}\n");
        let plugin = tree();
        drop_orphan(&plugin);
        fs::remove_file(plugin.join("skills/shared/pricing.json")).expect("rm");
        symlink(
            outside.join("pricing.json"),
            plugin.join("skills/shared/pricing.json"),
        )
        .expect("link");
        for check in [true, false] {
            let errors = errors_of(&plugin, check);
            assert!(
                errors
                    .iter()
                    .any(|e| e.contains("pricing.json: no canonical source")
                        && e.contains("symbolic link")),
                "{errors:?}"
            );
        }
        assert_eq!(
            read(&plugin, &format!("{VEND}/pricing.json")),
            "{\"a\": 0}\n",
            "never copied from a link"
        );

        // The whole shared directory as a link is refused for every file.
        let plugin = tree();
        drop_orphan(&plugin);
        fs::remove_dir_all(plugin.join("skills/shared")).expect("rm");
        write(&outside, "dsl/INTERPRETER.md", "canonical v2\n");
        symlink(&*outside, plugin.join("skills/shared")).expect("link");
        let errors = errors_of(&plugin, true);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("INTERPRETER.md: no canonical source")),
            "{errors:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_skill_vendored_dir_readme_and_plugin_skills_are_refused() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("vd-out-structure");
        write(&outside, "references/vendored/x.md", "x\n");
        write(&outside, "README.md", "| `x.md` | `skills/shared/x.md` |\n");
        let plugin = tree();
        symlink(
            outside.join("references"),
            plugin.join("skills/linked-skill-refs"),
        )
        .expect("link");
        // skills/gamma is itself a link to a directory with a vendored tree.
        symlink(&*outside, plugin.join("skills/gamma")).expect("link");
        let errors = errors_of(&plugin, true);
        assert!(
            errors.iter().any(|e| e
                .contains("skills/gamma/references/vendored: a path component is a symbolic link")),
            "{errors:?}"
        );
        assert!(
            !errors.iter().any(|e| e.contains("x.md")),
            "outside content not read: {errors:?}"
        );

        // README itself a link.
        let readme = plugin.join(format!("{VEND}/README.md"));
        fs::remove_file(&readme).expect("rm");
        symlink(outside.join("README.md"), &readme).expect("link");
        let errors = errors_of(&plugin, true);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("README.md: is a symbolic link; refusing to read it")),
            "{errors:?}"
        );

        // skills/ itself a link.
        let linked = TempDir::new("vd-linked-skills");
        symlink(&*outside, linked.join("skills")).expect("link");
        let report = sync_vendored(&linked, true).expect("run");
        assert_eq!(
            report.errors,
            ["skills: is a symbolic link; refusing to follow it"]
        );
    }
}
