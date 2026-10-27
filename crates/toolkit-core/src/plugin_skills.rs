//! Canonical-skill to plugin-bundle synchronization (`toolkit sync
//! plugin-skills`, port of `tools/sync-plugin-skills.py`, #4377).
//!
//! For every `plugins/<plugin>/skills/<name>/` directory whose `<name>` is also
//! a canonical skill (a directory under `skills/`, at any depth, that directly
//! contains a regular `SKILL.md`), the canonical directory is the source of
//! truth and the bundled copy must hold exactly the same files with exactly
//! the same bytes.
//!
//! - **Check** (`check: true`) never writes: it reports every out-of-sync
//!   bundled skill.
//! - **Write** replaces each out-of-sync bundled skill with a staged copy of
//!   the canonical directory (extra files disappear, missing files appear).
//!   The only thing ever deleted is such a bundled skill directory, a real
//!   directory that is a direct child of `plugins/<plugin>/skills/` and has
//!   a canonical counterpart. Nothing under `skills/` is ever written.
//! - Symbolic links are never followed: a link as a plugin, `skills`
//!   directory or bundled skill is an error; a link inside the canonical
//!   source is an error (nothing is copied); a link inside a bundled copy is
//!   drift and is replaced as a link, never written through.
//!
//! Core change note for #4398: the telemetry bundle gate shares
//! [`crate::treesync`] but is deliberately not part of this module.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::ToolkitError;
use crate::paths::normalize_relative;
use crate::treesync::{
    first_unsafe_entry, list_children, list_tree, path_state, replace_tree, trees_match, Kind,
};

/// Options for [`sync_plugin_skills`].
#[derive(Debug, Clone, Copy)]
pub struct PluginSkillsOptions<'a> {
    /// Repository root holding `skills/` and `plugins/`.
    pub root: &'a Path,
    /// Restrict to one plugin directory name.
    pub plugin: Option<&'a str>,
    /// Report drift without writing.
    pub check: bool,
}

/// What a run printed and what went wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSkillsReport {
    /// Standard output lines in order (headers, `synced ...`, the final
    /// "in sync" message when there were no errors).
    pub lines: Vec<String>,
    /// Problems found (drift in check mode, refusals in either mode).
    pub errors: Vec<String>,
}

impl PluginSkillsReport {
    /// The failure text for standard error, or `None` when there were no
    /// errors. Check mode appends the fix hint.
    #[must_use]
    pub fn failure_message(&self, check: bool) -> Option<String> {
        if self.errors.is_empty() {
            return None;
        }
        let mut message = format!("{} error(s):", self.errors.len());
        for error in &self.errors {
            message.push_str("\n  ");
            message.push_str(error);
        }
        if check {
            message.push_str("\n\nRun 'toolkit sync plugin-skills' to fix.");
        }
        Some(message)
    }
}

/// Maps skill name to the canonical directories (relative to `skills/`)
/// carrying that name, in sorted order. More than one means the name is
/// ambiguous.
pub type CanonicalIndex = BTreeMap<String, Vec<String>>;

/// Indexes the canonical skills below `root/skills`.
///
/// A missing `skills/` directory gives an empty index. Symbolic links are not
/// followed, so a linked directory or a linked `SKILL.md` is not a canonical
/// skill. `skills/SKILL.md` itself is ignored.
///
/// # Errors
///
/// Returns a runtime error when `skills` is a link or the tree cannot be
/// listed.
pub fn build_canonical_index(root: &Path) -> Result<CanonicalIndex, ToolkitError> {
    let mut index = CanonicalIndex::new();
    match path_state(root, "skills")? {
        Some(Kind::Dir) => {}
        Some(Kind::Symlink) => {
            return Err(ToolkitError::runtime(
                "path_symlink",
                format!(
                    "refusing to follow symbolic link: {}",
                    root.join("skills").display()
                ),
            ));
        }
        _ => return Ok(index),
    }
    for entry in list_tree(&root.join("skills"))? {
        if entry.kind != Kind::File {
            continue;
        }
        let Some(dir) = entry.rel.strip_suffix("/SKILL.md") else {
            continue;
        };
        let name = dir.rsplit('/').next().unwrap_or(dir);
        index
            .entry(name.to_string())
            .or_default()
            .push(dir.to_string());
    }
    Ok(index)
}

/// Runs the synchronization.
///
/// # Errors
///
/// Returns a runtime error for conditions that stop the whole run (invalid or
/// missing `--plugin`, missing `plugins/`, unreadable canonical index).
/// Problems with individual bundled skills are collected in
/// [`PluginSkillsReport::errors`] instead.
pub fn sync_plugin_skills(
    options: &PluginSkillsOptions<'_>,
) -> Result<PluginSkillsReport, ToolkitError> {
    let mut report = PluginSkillsReport::default();
    let verb = if options.check { "Checking" } else { "Syncing" };
    let canonical = build_canonical_index(options.root)?;

    if let Some(plugin) = options.plugin {
        if !normalize_relative(plugin).is_ok_and(|n| n == plugin && !plugin.contains('/')) {
            return Err(ToolkitError::runtime(
                "invalid_plugin_name",
                format!("Invalid plugin name: {plugin}"),
            ));
        }
        if path_state(options.root, &format!("plugins/{plugin}"))? != Some(Kind::Dir) {
            return Err(ToolkitError::runtime(
                "plugin_not_found",
                format!("Plugin not found: {plugin}"),
            ));
        }
        report.lines.push(format!("{verb} plugin: {plugin}"));
        sync_one(options, plugin, &canonical, &mut report)?;
    } else {
        if path_state(options.root, "plugins")? != Some(Kind::Dir) {
            return Err(ToolkitError::runtime(
                "plugins_not_found",
                format!(
                    "plugins directory not found: {}",
                    options.root.join("plugins").display()
                ),
            ));
        }
        for entry in list_children(&options.root.join("plugins"))? {
            match entry.kind {
                Kind::Dir => {}
                Kind::Symlink => {
                    report.errors.push(format!(
                        "plugins/{}: is a symbolic link; refusing to follow it",
                        entry.rel
                    ));
                    continue;
                }
                _ => continue,
            }
            if path_state(options.root, &format!("plugins/{}/skills", entry.rel))?
                .is_some_and(|kind| kind != Kind::File && kind != Kind::Other)
            {
                report.lines.push(format!("{verb} plugin: {}", entry.rel));
                sync_one(options, &entry.rel, &canonical, &mut report)?;
            }
        }
    }

    if report.errors.is_empty() {
        report.lines.push(String::new());
        report
            .lines
            .push("All plugin skills are in sync.".to_string());
    }
    Ok(report)
}

fn sync_one(
    options: &PluginSkillsOptions<'_>,
    plugin: &str,
    canonical: &CanonicalIndex,
    report: &mut PluginSkillsReport,
) -> Result<(), ToolkitError> {
    let skills_rel = format!("plugins/{plugin}/skills");
    match path_state(options.root, &skills_rel)? {
        Some(Kind::Dir) => {}
        Some(Kind::Symlink) => {
            report.errors.push(format!(
                "{skills_rel}: is a symbolic link; refusing to follow it"
            ));
            return Ok(());
        }
        _ => return Ok(()),
    }
    for child in list_children(&options.root.join(&skills_rel))? {
        let name = child.rel.as_str();
        let Some(sources) = canonical.get(name) else {
            continue;
        };
        match child.kind {
            Kind::Dir => {}
            Kind::Symlink => {
                report.errors.push(format!(
                    "{plugin}: {name} is a symbolic link; refusing to sync through it"
                ));
                continue;
            }
            _ => continue,
        }
        if sources.len() > 1 {
            report.errors.push(format!(
                "{plugin}: {name} has more than one canonical source ({}); \
                 rename or remove the duplicates",
                sources
                    .iter()
                    .map(|source| format!("skills/{source}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        }
        let source_rel = format!("skills/{}", sources[0]);
        if let Err(error) = sync_skill(options, plugin, name, &source_rel, report) {
            report.errors.push(format!("{plugin}: {name}: {error}"));
        }
    }
    Ok(())
}

fn sync_skill(
    options: &PluginSkillsOptions<'_>,
    plugin: &str,
    name: &str,
    source_rel: &str,
    report: &mut PluginSkillsReport,
) -> Result<(), ToolkitError> {
    let source = options.root.join(source_rel);
    let bundled = options.root.join(format!("plugins/{plugin}/skills/{name}"));
    let source_entries = list_tree(&source)?;
    if let Some(entry) = first_unsafe_entry(&source_entries) {
        return Err(ToolkitError::runtime(
            "path_symlink",
            format!(
                "canonical source {source_rel} contains a {} at {}; refusing to copy it",
                entry.kind.describe(),
                entry.rel
            ),
        ));
    }
    let bundled_entries = list_tree(&bundled)?;
    if trees_match(&source, &source_entries, &bundled, &bundled_entries)? {
        return Ok(());
    }
    if options.check {
        report
            .errors
            .push(format!("{plugin}: {name} is out of sync with {source_rel}"));
    } else {
        replace_tree(&source, &source_entries, &bundled)?;
        report
            .lines
            .push(format!("  synced {name} <- {source_rel}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    /// Canonical `skills/group/alpha` (+ `references/x.md`), `skills/beta`;
    /// plugin `p1` bundles alpha (in sync), beta (drifted) and `local-only`;
    /// plugin `p2` has no `skills/`.
    fn fixture() -> TempDir {
        let root = TempDir::new("ps");
        write(&root, "skills/group/alpha/SKILL.md", "alpha\n");
        write(&root, "skills/group/alpha/references/x.md", "x\n");
        write(&root, "skills/beta/SKILL.md", "beta v2\n");
        write(&root, "plugins/p1/skills/alpha/SKILL.md", "alpha\n");
        write(&root, "plugins/p1/skills/alpha/references/x.md", "x\n");
        write(&root, "plugins/p1/skills/beta/SKILL.md", "beta v1\n");
        write(&root, "plugins/p1/skills/local-only/SKILL.md", "mine\n");
        write(&root, "plugins/p2/README.md", "no skills here\n");
        write(&root, "plugins/FILE", "not a plugin\n");
        root
    }

    fn run(
        root: &Path,
        plugin: Option<&str>,
        check: bool,
    ) -> Result<PluginSkillsReport, ToolkitError> {
        sync_plugin_skills(&PluginSkillsOptions {
            root,
            plugin,
            check,
        })
    }

    #[test]
    fn index_finds_nested_skills_by_directory_name() {
        let root = fixture();
        write(
            &root,
            "skills/SKILL.md",
            "ignored: the skills root itself\n",
        );
        let index = build_canonical_index(&root).expect("index");
        assert_eq!(
            index.keys().map(String::as_str).collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(index["alpha"], ["group/alpha"]);
    }

    #[test]
    fn check_reports_drift_without_writing_anything() {
        let root = fixture();
        let report = run(&root, None, true).expect("run");
        assert_eq!(report.lines, ["Checking plugin: p1"]);
        assert_eq!(report.errors, ["p1: beta is out of sync with skills/beta"]);
        let message = report.failure_message(true).expect("failure");
        assert_eq!(
            message,
            "1 error(s):\n  p1: beta is out of sync with skills/beta\n\nRun 'toolkit sync plugin-skills' to fix."
        );
        assert_eq!(
            fs::read_to_string(root.join("plugins/p1/skills/beta/SKILL.md")).expect("read"),
            "beta v1\n"
        );
    }

    #[test]
    fn write_mode_copies_canonical_over_drift_and_then_check_passes() {
        let root = fixture();
        let report = run(&root, None, false).expect("run");
        assert_eq!(
            report.lines,
            [
                "Syncing plugin: p1",
                "  synced beta <- skills/beta",
                "",
                "All plugin skills are in sync."
            ]
        );
        assert!(report.errors.is_empty());
        assert_eq!(report.failure_message(false), None);
        assert_eq!(
            fs::read_to_string(root.join("plugins/p1/skills/beta/SKILL.md")).expect("read"),
            "beta v2\n"
        );
        // Unrelated bundled skill untouched; a second write is a no-op.
        assert!(root.join("plugins/p1/skills/local-only/SKILL.md").is_file());
        let again = run(&root, None, false).expect("again");
        assert_eq!(
            again.lines,
            ["Syncing plugin: p1", "", "All plugin skills are in sync."]
        );
        let check = run(&root, None, true).expect("check");
        assert!(check.errors.is_empty());
        assert_eq!(
            check.lines,
            ["Checking plugin: p1", "", "All plugin skills are in sync."]
        );
    }

    #[test]
    fn extra_and_missing_files_are_drift_and_sync_removes_extras() {
        let root = fixture();
        write(&root, "plugins/p1/skills/alpha/stale.md", "stale\n");
        fs::remove_file(root.join("plugins/p1/skills/alpha/references/x.md")).expect("rm");
        let check = run(&root, None, true).expect("check");
        assert_eq!(check.errors.len(), 2);
        assert!(check.errors[0].contains("alpha is out of sync"));
        run(&root, None, false).expect("sync");
        assert!(!root.join("plugins/p1/skills/alpha/stale.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("plugins/p1/skills/alpha/references/x.md")).expect("read"),
            "x\n"
        );
        assert!(run(&root, None, true).expect("check").errors.is_empty());
    }

    #[test]
    fn byte_difference_without_length_change_is_drift() {
        let root = fixture();
        write(&root, "plugins/p1/skills/alpha/SKILL.md", "alphX\n");
        let report = run(&root, None, true).expect("run");
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("alpha is out of sync")));
    }

    #[test]
    fn plugin_option_selects_one_plugin_and_reports_a_missing_one() {
        let root = fixture();
        write(&root, "plugins/p3/skills/beta/SKILL.md", "beta old\n");
        let report = run(&root, Some("p3"), true).expect("run");
        assert_eq!(report.lines, ["Checking plugin: p3"]);
        assert_eq!(report.errors, ["p3: beta is out of sync with skills/beta"]);
        // p2 exists but has no skills/: header, then nothing, then success.
        let report = run(&root, Some("p2"), true).expect("run");
        assert_eq!(
            report.lines,
            ["Checking plugin: p2", "", "All plugin skills are in sync."]
        );
        let error = run(&root, Some("nope"), true).expect_err("missing");
        assert_eq!(error.code(), "plugin_not_found");
        assert_eq!(error.message(), "Plugin not found: nope");
        assert_eq!(error.exit_code(), 1);
    }

    #[test]
    fn plugin_names_that_could_escape_the_plugins_directory_are_rejected() {
        let root = fixture();
        for bad in ["../skills", "a/b", ".", "", "/etc", "p1/skills", "a\\b"] {
            let error = run(&root, Some(bad), true).expect_err(bad);
            assert_eq!(error.code(), "invalid_plugin_name", "{bad:?}");
        }
        // Nothing was touched.
        assert!(run(&root, None, true).is_ok());
    }

    #[test]
    fn missing_plugins_directory_is_an_error_and_missing_skills_is_empty() {
        let root = TempDir::new("ps-empty");
        assert_eq!(
            run(&root, None, true).expect_err("no plugins").code(),
            "plugins_not_found"
        );
        fs::create_dir(root.join("plugins")).expect("mkdir");
        let report = run(&root, None, true).expect("empty plugins");
        assert_eq!(report.lines, ["", "All plugin skills are in sync."]);
        assert!(build_canonical_index(&root).expect("index").is_empty());
    }

    #[test]
    fn duplicate_canonical_names_are_an_error_not_a_coin_flip() {
        let root = fixture();
        write(&root, "skills/other/beta/SKILL.md", "second beta\n");
        let report = run(&root, None, false).expect("run");
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("p1: beta has more than one canonical source"));
        assert!(report.errors[0].contains("skills/beta, skills/other/beta"));
        assert_eq!(
            fs::read_to_string(root.join("plugins/p1/skills/beta/SKILL.md")).expect("read"),
            "beta v1\n",
            "an ambiguous skill is never written"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_links_are_refused_and_nothing_outside_is_read_or_copied() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ps-out");
        write(&outside, "secret.txt", "TOP SECRET\n");
        let root = fixture();
        symlink(outside.join("secret.txt"), root.join("skills/beta/leak.md")).expect("link");
        for check in [true, false] {
            let report = run(&root, None, check).expect("run");
            assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
            assert!(
                report.errors[0]
                    .contains("canonical source skills/beta contains a symbolic link at leak.md"),
                "{}",
                report.errors[0]
            );
        }
        assert!(!root.join("plugins/p1/skills/beta/leak.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("plugins/p1/skills/beta/SKILL.md")).expect("read"),
            "beta v1\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_canonical_skill_directory_is_not_followed_or_indexed() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ps-out-skill");
        write(&outside, "evil/SKILL.md", "evil\n");
        let root = fixture();
        symlink(outside.join("evil"), root.join("skills/evil")).expect("link");
        write(&root, "plugins/p1/skills/evil/SKILL.md", "bundled evil\n");
        let index = build_canonical_index(&root).expect("index");
        assert!(!index.contains_key("evil"));
        let report = run(&root, None, false).expect("run");
        assert_eq!(
            fs::read_to_string(root.join("plugins/p1/skills/evil/SKILL.md")).expect("read"),
            "bundled evil\n"
        );
        assert!(report.errors.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn linked_bundles_plugins_and_skills_directories_are_refused() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ps-out-bundle");
        write(&outside, "victim/SKILL.md", "victim\n");
        let root = fixture();
        fs::remove_dir_all(root.join("plugins/p1/skills/beta")).expect("rm");
        symlink(outside.join("victim"), root.join("plugins/p1/skills/beta")).expect("link");
        symlink(&*outside, root.join("plugins/linked")).expect("plugin link");
        let report = run(&root, None, false).expect("run");
        assert!(report
            .errors
            .contains(&"p1: beta is a symbolic link; refusing to sync through it".to_string()));
        assert!(report
            .errors
            .contains(&"plugins/linked: is a symbolic link; refusing to follow it".to_string()));
        assert_eq!(
            fs::read_to_string(outside.join("victim/SKILL.md")).expect("read"),
            "victim\n",
            "the link target was never written or removed"
        );
        let direct = run(&root, Some("linked"), true).expect_err("linked plugin");
        assert_eq!(direct.code(), "plugin_not_found");

        // A linked `skills/` directory inside a plugin is refused as well.
        fs::create_dir_all(root.join("plugins/p4")).expect("mkdir");
        symlink(&*outside, root.join("plugins/p4/skills")).expect("skills link");
        let report = run(&root, Some("p4"), false).expect("run");
        assert_eq!(
            report.errors,
            ["plugins/p4/skills: is a symbolic link; refusing to follow it"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_inside_a_bundle_are_drift_and_are_replaced_without_following() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ps-out-inner");
        write(&outside, "precious/data", "precious\n");
        let root = fixture();
        symlink(
            outside.join("precious"),
            root.join("plugins/p1/skills/alpha/dirlink"),
        )
        .expect("link");
        let check = run(&root, None, true).expect("check");
        assert!(check
            .errors
            .iter()
            .any(|e| e.contains("alpha is out of sync")));
        assert!(
            root.join("plugins/p1/skills/alpha/dirlink").exists(),
            "check does not write"
        );
        run(&root, None, false).expect("sync");
        assert!(!root.join("plugins/p1/skills/alpha/dirlink").exists());
        assert_eq!(
            fs::read_to_string(outside.join("precious/data")).expect("read"),
            "precious\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_skills_root_is_refused() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new("ps-out-skills-root");
        write(&outside, "gamma/SKILL.md", "gamma\n");
        let root = TempDir::new("ps-linked-skills");
        fs::create_dir(root.join("plugins")).expect("mkdir");
        symlink(&*outside, root.join("skills")).expect("link");
        assert_eq!(
            run(&root, None, true).expect_err("link").code(),
            "path_symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn permission_bits_survive_a_sync() {
        use std::os::unix::fs::PermissionsExt;
        let root = fixture();
        write(&root, "skills/beta/scripts/run.sh", "#!/bin/sh\n");
        fs::set_permissions(
            root.join("skills/beta/scripts/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
        run(&root, None, false).expect("sync");
        let mode = fs::metadata(root.join("plugins/p1/skills/beta/scripts/run.sh"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}
