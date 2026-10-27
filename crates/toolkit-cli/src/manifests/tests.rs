//! Offline, deterministic tests for `toolkit manifests validate` (#4375).
//!
//! Every rule of the Python validator has a positive and a negative fixture
//! here; the Python unit tests (`TestDevinManifestParity`, `TestRepoPlugins`)
//! are migrated as `matching_devin_metadata_passes` and friends and
//! `real_repository_tree_passes`. Fixtures live in unique temporary
//! directories removed on drop; nothing touches the network.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    check_inside, is_kebab, parse_simple_frontmatter, py_repr, py_repr_str, py_splitlines,
    universal_newlines, validate_tree, PathCheck, Report, Validator,
};
use crate::{route, run, Routed, COMMANDS};
use toolkit_core::{ErrorKind, Json, ToolkitError};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-manifests-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture dir");
    fs::write(path, content).expect("write fixture");
}

const DESCRIPTION: &str = "A description that is long enough to pass.";

fn claude_json(name: &str) -> String {
    format!(r#"{{"name":"{name}","version":"1.2.0","description":"Demo plugin."}}"#)
}

fn codex_json(name: &str) -> String {
    format!(
        r#"{{"name":"{name}","version":"1.2.0","description":"Demo plugin.","author":{{"name":"x"}},"interface":{{}}}}"#
    )
}

fn devin_json(name: &str) -> String {
    format!(
        r#"{{"name":"{name}","version":"1.2.0","description":"Demo plugin.","skills":"./skills/","mcpServers":"./.mcp.json"}}"#
    )
}

const MCP_JSON: &str = r#"{"mcpServers":{"local":{"command":"tool"},"remote":{"type":"http","url":"https://example.test/mcp"}}}"#;

fn skill_text(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\nBody.\n")
}

/// A fully valid plugin directory.
fn write_plugin(root: &Path, name: &str) {
    let dir = format!("plugins/{name}");
    write(
        root,
        &format!("{dir}/.claude-plugin/plugin.json"),
        &claude_json(name),
    );
    write(
        root,
        &format!("{dir}/.codex-plugin/plugin.json"),
        &codex_json(name),
    );
    write(
        root,
        &format!("{dir}/.devin-plugin/plugin.json"),
        &devin_json(name),
    );
    write(root, &format!("{dir}/.mcp.json"), MCP_JSON);
    write(
        root,
        &format!("{dir}/skills/{name}-skill/SKILL.md"),
        &skill_text(&format!("{name}-skill"), DESCRIPTION),
    );
}

fn marketplace(source: &str) -> String {
    format!(r#"{{"name":"market","plugins":[{{"name":"demo","source":{source}}}]}}"#)
}

/// A fully valid repository: three marketplaces, one plugin, one top-level skill.
fn write_repo(root: &Path) {
    write_plugin(root, "demo");
    write(
        root,
        ".claude-plugin/marketplace.json",
        &marketplace(r#""./plugins/demo""#),
    );
    write(
        root,
        ".agents/plugins/marketplace.json",
        &marketplace(r#"{"source":"local","path":"./plugins/demo"}"#),
    );
    write(
        root,
        ".cursor-plugin/marketplace.json",
        &marketplace(r#""./plugins/demo""#),
    );
    write(
        root,
        "skills/core/top-skill/SKILL.md",
        &skill_text("top-skill", DESCRIPTION),
    );
}

fn errors_of(report: &Report) -> Vec<String> {
    report.errors().into_iter().map(str::to_owned).collect()
}

/// Validates the whole tree and returns the error messages.
fn tree_errors(root: &Path) -> Vec<String> {
    errors_of(&validate_tree(root, None).expect("validation runs"))
}

/// Builds a valid repository, applies `change`, validates only plugin `demo`.
fn plugin_errors(change: impl FnOnce(&Path)) -> Vec<String> {
    let root = TempDir::new("plugin");
    write_repo(&root);
    change(&root);
    errors_of(&validate_tree(&root, Some("demo")).expect("plugin exists"))
}

/// Builds a valid repository, applies `change`, validates the whole tree.
fn repo_errors(change: impl FnOnce(&Path)) -> Vec<String> {
    let root = TempDir::new("repo");
    write_repo(&root);
    change(&root);
    tree_errors(&root)
}

/// Validates one `SKILL.md` content placed in a directory called `dir`.
fn skill_errors(dir: &str, content: &str) -> Vec<String> {
    let root = TempDir::new("skill");
    let rel = format!("skills/{dir}/SKILL.md");
    write(&root, &rel, content);
    let mut validator = Validator::new(&root);
    validator.validate_skill_frontmatter(&rel, dir);
    errors_of(&validator.report)
}

fn skill_rel(dir: &str) -> String {
    format!("skills/{dir}/SKILL.md")
}

const DEVIN_REL: &str = "plugins/demo/.devin-plugin/plugin.json";
const CLAUDE_REL: &str = "plugins/demo/.claude-plugin/plugin.json";

// ---- Python unit tests migrated (TestDevinManifestParity) -------------------

#[test]
fn matching_devin_metadata_passes() {
    assert_eq!(plugin_errors(|_| {}), Vec::<String>::new());
    // The Python fixture declared `skills: "./"`: the plugin root itself.
    let errors = plugin_errors(|root| {
        write(
            root,
            DEVIN_REL,
            r#"{"name":"demo","version":"1.2.0","description":"Demo plugin.","skills":"./"}"#,
        );
    });
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn devin_version_mismatch_fails() {
    let errors = plugin_errors(|root| {
        write(
            root,
            DEVIN_REL,
            r#"{"name":"demo","version":"1.1.0","description":"Demo plugin."}"#,
        );
    });
    assert_eq!(
        errors,
        vec![format!(
            "'version' in {DEVIN_REL} does not match {CLAUDE_REL}: '1.1.0' != '1.2.0'"
        )]
    );
}

#[test]
fn devin_description_mismatch_fails_without_detail() {
    let errors = plugin_errors(|root| {
        write(
            root,
            DEVIN_REL,
            r#"{"name":"demo","version":"1.2.0","description":"Stale."}"#,
        );
    });
    assert_eq!(
        errors,
        vec![format!(
            "'description' in {DEVIN_REL} does not match {CLAUDE_REL}"
        )]
    );
}

#[test]
fn devin_missing_version_fails_with_none_detail() {
    let errors = plugin_errors(|root| {
        write(
            root,
            DEVIN_REL,
            r#"{"name":"demo","description":"Demo plugin."}"#,
        );
    });
    assert_eq!(
        errors,
        vec![format!(
            "'version' in {DEVIN_REL} does not match {CLAUDE_REL}: None != '1.2.0'"
        )]
    );
}

#[test]
fn missing_claude_manifest_skips_devin_parity() {
    let errors = plugin_errors(|root| {
        fs::remove_file(root.join(CLAUDE_REL)).expect("remove claude manifest");
        write(
            root,
            DEVIN_REL,
            r#"{"name":"demo","version":"9.9.9","description":"Other."}"#,
        );
    });
    assert_eq!(errors, vec![format!("Missing file: {CLAUDE_REL}")]);
}

#[test]
fn real_repository_tree_passes() {
    // Migrated from TestRepoPlugins.test_every_devin_plugin_passes, widened to
    // the whole tree: every marketplace, plugin and skill of this repository.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = validate_tree(&root, None).expect("repository root is valid");
    assert_eq!(errors_of(&report), Vec::<String>::new());
    let progress = report.progress();
    assert_eq!(
        progress[..3],
        [
            "Validating Claude Code marketplace: .claude-plugin/marketplace.json",
            "Validating Codex marketplace: .agents/plugins/marketplace.json",
            "Validating Cursor marketplace: .cursor-plugin/marketplace.json",
        ]
    );
    assert!(progress.contains(&"Validating plugin: aws-core"));
    assert!(progress
        .iter()
        .any(|line| line.starts_with("Validating skill: skills/")));
}

#[test]
fn every_real_devin_plugin_passes_individually() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut checked = 0;
    let mut names: Vec<String> = fs::read_dir(root.join("plugins"))
        .expect("plugins directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        if !root
            .join("plugins")
            .join(&name)
            .join(".devin-plugin/plugin.json")
            .exists()
        {
            continue;
        }
        let report = validate_tree(&root, Some(&name)).expect("plugin exists");
        assert_eq!(errors_of(&report), Vec::<String>::new(), "plugin {name}");
        checked += 1;
    }
    assert!(checked > 0, "at least one Devin plugin exists");
}

// ---- marketplace rules ------------------------------------------------------

#[test]
fn valid_repository_passes_with_ordered_progress() {
    let root = TempDir::new("valid");
    write_repo(&root);
    let report = validate_tree(&root, None).expect("validation runs");
    assert_eq!(errors_of(&report), Vec::<String>::new());
    assert_eq!(
        report.progress(),
        vec![
            "Validating Claude Code marketplace: .claude-plugin/marketplace.json",
            "Validating Codex marketplace: .agents/plugins/marketplace.json",
            "Validating Cursor marketplace: .cursor-plugin/marketplace.json",
            "Validating plugin: demo",
            "Validating skill: skills/core/top-skill",
        ]
    );
}

#[test]
fn missing_marketplaces_are_reported_in_order() {
    let root = TempDir::new("empty");
    assert_eq!(
        tree_errors(&root),
        vec![
            "Missing file: .claude-plugin/marketplace.json",
            "Missing file: .agents/plugins/marketplace.json",
            "Missing file: .cursor-plugin/marketplace.json",
        ]
    );
}

#[test]
fn marketplace_missing_keys_are_reported() {
    let errors = repo_errors(|root| write(root, ".claude-plugin/marketplace.json", "{}"));
    assert_eq!(
        errors,
        vec![
            "Missing key 'name' in .claude-plugin/marketplace.json",
            "Missing key 'plugins' in .claude-plugin/marketplace.json",
        ]
    );
}

#[test]
fn marketplace_invalid_json_is_reported() {
    for broken in [
        "{",
        "",
        "{\"name\":1,}",
        "\u{feff}{}",
        "{\"a\":1,\"a\":2}",
        "nope",
    ] {
        let errors = repo_errors(|root| write(root, ".cursor-plugin/marketplace.json", broken));
        assert_eq!(errors.len(), 1, "{broken:?}: {errors:?}");
        assert!(
            errors[0]
                .starts_with("Invalid JSON in .cursor-plugin/marketplace.json: JSON parse error"),
            "{broken:?}: {errors:?}"
        );
    }
}

#[test]
fn marketplace_top_level_must_be_an_object() {
    let errors = repo_errors(|root| write(root, ".claude-plugin/marketplace.json", "[]"));
    assert_eq!(
        errors,
        vec![
            "Missing key 'name' in .claude-plugin/marketplace.json",
            "Missing key 'plugins' in .claude-plugin/marketplace.json",
            "Top-level value must be a JSON object in .claude-plugin/marketplace.json",
        ]
    );
}

#[test]
fn marketplace_plugin_without_name_is_reported() {
    let errors = repo_errors(|root| {
        write(
            root,
            ".claude-plugin/marketplace.json",
            r#"{"name":"m","plugins":[{"source":"./plugins/demo"},"text",7]}"#,
        );
    });
    assert_eq!(
        errors,
        vec!["Plugin missing 'name' in .claude-plugin/marketplace.json"; 3]
    );
}

#[test]
fn marketplace_source_must_exist_as_a_directory() {
    // String form, object form and a path that is a file.
    let errors = repo_errors(|root| {
        write(
            root,
            ".claude-plugin/marketplace.json",
            &marketplace(r#""./plugins/ghost""#),
        );
        write(
            root,
            ".agents/plugins/marketplace.json",
            &marketplace(r#"{"source":"local","path":"./plugins/ghost-two"}"#),
        );
        write(
            root,
            ".cursor-plugin/marketplace.json",
            &marketplace(r#""./plugins/demo/.mcp.json""#),
        );
    });
    assert_eq!(
        errors,
        vec![
            "Plugin source './plugins/ghost' does not exist for 'demo'",
            "Plugin source './plugins/ghost-two' does not exist for 'demo'",
            "Plugin source './plugins/demo/.mcp.json' does not exist for 'demo'",
        ]
    );
}

#[test]
fn marketplace_empty_or_absent_source_is_not_checked() {
    let errors = repo_errors(|root| {
        write(
            root,
            ".claude-plugin/marketplace.json",
            r#"{"name":"m","plugins":[{"name":"a"},{"name":"b","source":""},{"name":"c","source":null},{"name":"d","source":{}},{"name":"e","source":{"source":"local"}}]}"#,
        );
    });
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn marketplace_source_resolves_from_the_repository_root() {
    // Relative to the repository root, not to the manifest's directory.
    let errors = repo_errors(|root| {
        write(
            root,
            ".claude-plugin/marketplace.json",
            &marketplace(r#""plugins/demo""#),
        );
    });
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn marketplace_non_string_source_and_non_array_plugins_are_reported() {
    let errors = repo_errors(|root| {
        write(root, ".claude-plugin/marketplace.json", &marketplace("5"));
        write(
            root,
            ".agents/plugins/marketplace.json",
            r#"{"name":"m","plugins":{}}"#,
        );
    });
    assert_eq!(
        errors,
        vec![
            "Plugin source for 'demo' must be a string in .claude-plugin/marketplace.json",
            "'plugins' must be an array in .agents/plugins/marketplace.json",
        ]
    );
}

#[test]
fn oversized_manifest_is_reported_not_read() {
    let errors = repo_errors(|root| {
        let big = format!("{{\"name\":\"{}\"}}", "a".repeat(1024 * 1024));
        write(root, ".claude-plugin/marketplace.json", &big);
    });
    assert_eq!(
        errors,
        vec!["Cannot read .claude-plugin/marketplace.json: larger than 1048576 bytes"]
    );
}

#[test]
fn non_utf8_manifest_is_reported() {
    let root = TempDir::new("nonutf8");
    write_repo(&root);
    fs::write(
        root.join(".claude-plugin/marketplace.json"),
        b"{\"name\":\"\xff\"}",
    )
    .expect("write fixture");
    assert_eq!(
        tree_errors(&root),
        vec!["Cannot read .claude-plugin/marketplace.json: not valid UTF-8"]
    );
}

// ---- plugin manifests -------------------------------------------------------

#[test]
fn claude_manifest_missing_name_is_reported_with_parity_detail() {
    let errors = plugin_errors(|root| {
        write(
            root,
            CLAUDE_REL,
            r#"{"version":"1.2.0","description":"Demo plugin."}"#,
        );
    });
    assert_eq!(
        errors,
        vec![
            format!("Missing key 'name' in {CLAUDE_REL}"),
            format!("'name' in {DEVIN_REL} does not match {CLAUDE_REL}: 'demo' != None"),
        ]
    );
}

#[test]
fn codex_manifest_requires_all_keys() {
    let errors = plugin_errors(|root| {
        write(
            root,
            "plugins/demo/.codex-plugin/plugin.json",
            r#"{"name":"demo"}"#,
        );
    });
    let rel = "plugins/demo/.codex-plugin/plugin.json";
    assert_eq!(
        errors,
        vec![
            format!("Missing key 'version' in {rel}"),
            format!("Missing key 'description' in {rel}"),
            format!("Missing key 'author' in {rel}"),
            format!("Missing key 'interface' in {rel}"),
        ]
    );
    let missing = plugin_errors(|root| {
        fs::remove_file(root.join(rel)).expect("remove codex manifest");
    });
    assert_eq!(missing, vec![format!("Missing file: {rel}")]);
}

#[test]
fn cursor_manifest_is_optional_and_name_must_match() {
    let rel = "plugins/demo/.cursor-plugin/plugin.json";
    assert_eq!(
        plugin_errors(|root| write(root, rel, r#"{"name":"demo"}"#)),
        Vec::<String>::new()
    );
    assert_eq!(
        plugin_errors(|root| write(root, rel, r#"{"name":"other"}"#)),
        vec![format!(
            "Name 'other' does not match directory 'demo' in {rel}"
        )]
    );
    assert_eq!(
        plugin_errors(|root| write(root, rel, r#"{"name":5}"#)),
        vec![format!("Name '5' does not match directory 'demo' in {rel}")]
    );
    assert_eq!(
        plugin_errors(|root| write(root, rel, r#"{"version":"1"}"#)),
        vec![format!("Missing key 'name' in {rel}")]
    );
    let broken = plugin_errors(|root| write(root, rel, "{"));
    assert_eq!(broken.len(), 1);
    assert!(broken[0].starts_with(&format!("Invalid JSON in {rel}: JSON parse error")));
}

#[test]
fn devin_manifest_is_optional() {
    let errors = plugin_errors(|root| {
        fs::remove_dir_all(root.join("plugins/demo/.devin-plugin")).expect("remove devin dir");
    });
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn devin_name_must_be_a_non_empty_matching_string() {
    let meta =
        |name: &str| format!(r#"{{"name":{name},"version":"1.2.0","description":"Demo plugin."}}"#);
    let empty = plugin_errors(|root| write(root, DEVIN_REL, &meta("\"\"")));
    assert_eq!(
        empty,
        vec![
            format!("'name' must be a non-empty string in {DEVIN_REL}"),
            format!("'name' in {DEVIN_REL} does not match {CLAUDE_REL}: '' != 'demo'"),
        ]
    );
    let number = plugin_errors(|root| write(root, DEVIN_REL, &meta("5")));
    assert_eq!(
        number,
        vec![
            format!("'name' must be a non-empty string in {DEVIN_REL}"),
            format!("'name' in {DEVIN_REL} does not match {CLAUDE_REL}: 5 != 'demo'"),
        ]
    );
    let other = plugin_errors(|root| write(root, DEVIN_REL, &meta("\"other\"")));
    assert_eq!(
        other,
        vec![
            format!("Name 'other' does not match directory 'demo' in {DEVIN_REL}"),
            format!("'name' in {DEVIN_REL} does not match {CLAUDE_REL}: 'other' != 'demo'"),
        ]
    );
}

#[test]
fn devin_missing_name_key_is_reported() {
    let errors = plugin_errors(|root| {
        write(
            root,
            DEVIN_REL,
            r#"{"version":"1.2.0","description":"Demo plugin."}"#,
        );
    });
    assert_eq!(
        errors,
        vec![
            format!("Missing key 'name' in {DEVIN_REL}"),
            format!("'name' in {DEVIN_REL} does not match {CLAUDE_REL}: None != 'demo'"),
        ]
    );
}

fn devin_with_paths(paths: &str) -> String {
    format!(r#"{{"name":"demo","version":"1.2.0","description":"Demo plugin.",{paths}}}"#)
}

#[test]
fn devin_declared_paths_must_stay_inside_and_exist() {
    let cases = [
        (r#""skills":"../outside""#, "skills", "../outside", true),
        (r#""skills":["./skills/","/etc"]"#, "skills", "/etc", true),
        (
            r#""skills":"skills/../../x""#,
            "skills",
            "skills/../../x",
            true,
        ),
        (
            r#""mcpServers":"../x.json""#,
            "mcpServers",
            "../x.json",
            true,
        ),
        (r#""skills":"./ghost""#, "skills", "./ghost", false),
        (
            r#""mcpServers":["./ghost.json"]"#,
            "mcpServers",
            "./ghost.json",
            false,
        ),
    ];
    for (paths, key, declared, escapes) in cases {
        let errors = plugin_errors(|root| write(root, DEVIN_REL, &devin_with_paths(paths)));
        let expected = if escapes {
            format!(
                "'{key}' path '{declared}' must stay inside the plugin directory in {DEVIN_REL}"
            )
        } else {
            format!("'{key}' path '{declared}' does not exist in {DEVIN_REL}")
        };
        // "/etc" exists but is absolute, so it still escapes.
        assert_eq!(errors, vec![expected], "{paths}");
    }
}

#[test]
fn devin_valid_declared_paths_pass() {
    for paths in [
        r#""skills":"./""#,
        r#""skills":"skills""#,
        r#""skills":["./skills/","./skills/demo-skill"]"#,
        r#""mcpServers":"./.mcp.json""#,
        r#""skills":null,"mcpServers":5"#,
        r#""skills":[]"#,
        r#""skills":"""#,
    ] {
        let errors = plugin_errors(|root| write(root, DEVIN_REL, &devin_with_paths(paths)));
        assert_eq!(errors, Vec::<String>::new(), "{paths}");
    }
}

#[test]
fn devin_non_string_path_entries_are_reported() {
    let errors = plugin_errors(|root| {
        write(
            root,
            DEVIN_REL,
            &devin_with_paths(r#""skills":[5,null,true,"it's",{"a":1}],"mcpServers":[["x"]]"#),
        );
    });
    assert_eq!(
        errors,
        vec![
            format!("'skills' entry 5 is not a path string in {DEVIN_REL}"),
            format!("'skills' entry None is not a path string in {DEVIN_REL}"),
            format!("'skills' entry True is not a path string in {DEVIN_REL}"),
            format!("'skills' path 'it's' does not exist in {DEVIN_REL}"),
            format!("'skills' entry {{'a': 1}} is not a path string in {DEVIN_REL}"),
            format!("'mcpServers' entry ['x'] is not a path string in {DEVIN_REL}"),
        ]
    );
}

#[cfg(unix)]
#[test]
fn devin_symlink_leaving_the_plugin_escapes() {
    use std::os::unix::fs::symlink;
    let outside = TempDir::new("outside");
    write(&outside, "keep.txt", "x");
    let errors = plugin_errors(|root| {
        symlink(&*outside, root.join("plugins/demo/linked")).expect("create symlink");
        symlink(
            root.join("plugins/demo/skills"),
            root.join("plugins/demo/inner"),
        )
        .expect("create inner symlink");
        symlink(outside.join("nowhere"), root.join("plugins/demo/dangling"))
            .expect("create dangling symlink");
        write(
            root,
            DEVIN_REL,
            &devin_with_paths(
                r#""skills":["./linked","./linked/keep.txt","./inner","./dangling"]"#,
            ),
        );
    });
    assert_eq!(
        errors,
        vec![
            format!("'skills' path './linked' must stay inside the plugin directory in {DEVIN_REL}"),
            format!(
                "'skills' path './linked/keep.txt' must stay inside the plugin directory in {DEVIN_REL}"
            ),
            format!("'skills' path './dangling' does not exist in {DEVIN_REL}"),
        ]
    );
}

#[test]
fn check_inside_classifies_paths() {
    let root = TempDir::new("inside");
    fs::create_dir_all(root.join("a/b")).expect("create dirs");
    let canonical = fs::canonicalize(&*root).expect("canonical root");
    assert!(matches!(check_inside(&canonical, "a/b"), PathCheck::Inside));
    assert!(matches!(check_inside(&canonical, ""), PathCheck::Inside));
    assert!(matches!(
        check_inside(&canonical, "a/missing/deeper"),
        PathCheck::Missing
    ));
    assert!(matches!(
        check_inside(&canonical, "a/../a"),
        PathCheck::Escapes
    ));
    assert!(matches!(
        check_inside(&canonical, "/tmp"),
        PathCheck::Escapes
    ));
}

// ---- MCP configuration ------------------------------------------------------

const MCP_REL: &str = "plugins/demo/.mcp.json";

fn mcp_errors(content: &str) -> Vec<String> {
    plugin_errors(|root| write(root, MCP_REL, content))
}

#[test]
fn mcp_valid_servers_pass() {
    assert_eq!(mcp_errors(MCP_JSON), Vec::<String>::new());
    assert_eq!(
        mcp_errors(
            r#"{"mcpServers":{"a":{"type":"stdio","command":""},"b":{"type":"sse"},"c":{"type":5},"d":{"type":null}}}"#
        ),
        Vec::<String>::new(),
        "only stdio and http are checked and `command` needs to be present, not truthy"
    );
    assert_eq!(mcp_errors(r#"{"mcpServers":{}}"#), Vec::<String>::new());
}

#[test]
fn mcp_stdio_server_needs_a_command() {
    assert_eq!(
        mcp_errors(
            r#"{"mcpServers":{"local":{},"typed":{"type":"stdio","url":"https://x.test"}}}"#
        ),
        vec![
            "MCP server 'local' is stdio but missing 'command'",
            "MCP server 'typed' is stdio but missing 'command'",
        ]
    );
}

#[test]
fn mcp_http_server_needs_a_url() {
    assert_eq!(
        mcp_errors(r#"{"mcpServers":{"remote":{"type":"http","command":"x"}}}"#),
        vec!["MCP server 'remote' is http but missing 'url'"]
    );
}

#[test]
fn mcp_url_interpolation_is_rejected() {
    assert_eq!(
        mcp_errors(
            r#"{"mcpServers":{"remote":{"type":"http","url":"https://${HOST:-a.test}/mcp"},"other":{"command":"x","url":"${URL}"}}}"#
        ),
        vec![
            "MCP server 'remote' uses unsupported environment interpolation in URL: https://${HOST:-a.test}/mcp",
            "MCP server 'other' uses unsupported environment interpolation in URL: ${URL}",
        ]
    );
    // `$HOST` without braces and non-string URLs are not interpolation.
    assert_eq!(
        mcp_errors(
            r#"{"mcpServers":{"a":{"type":"http","url":"https://$HOST/mcp"},"b":{"type":"sse","url":5}}}"#
        ),
        Vec::<String>::new()
    );
}

#[test]
fn mcp_structure_errors_are_reported() {
    assert_eq!(
        mcp_errors("{}"),
        vec![format!("Missing key 'mcpServers' in {MCP_REL}")]
    );
    assert_eq!(
        mcp_errors(r#"{"mcpServers":[]}"#),
        vec![format!("'mcpServers' must be an object in {MCP_REL}")]
    );
    assert_eq!(
        mcp_errors(r#"{"mcpServers":{"bad":"text","worse":null}}"#),
        vec![
            format!("MCP server 'bad' must be an object in {MCP_REL}"),
            format!("MCP server 'worse' must be an object in {MCP_REL}"),
        ]
    );
    assert_eq!(mcp_errors("[1]").len(), 2);
    assert!(mcp_errors("not json")[0].starts_with("Invalid JSON in plugins/demo/.mcp.json"));
}

#[test]
fn mcp_file_is_optional() {
    let errors = plugin_errors(|root| {
        fs::remove_file(root.join(MCP_REL)).expect("remove mcp file");
        write(
            root,
            DEVIN_REL,
            r#"{"name":"demo","version":"1.2.0","description":"Demo plugin."}"#,
        );
    });
    assert_eq!(errors, Vec::<String>::new());
}

// ---- plugin skills ----------------------------------------------------------

#[test]
fn plugin_skill_errors_name_the_plugin_path() {
    let errors = plugin_errors(|root| {
        write(
            root,
            "plugins/demo/skills/demo-skill/SKILL.md",
            &skill_text("wrong-name", DESCRIPTION),
        );
    });
    assert_eq!(
        errors,
        vec!["Name 'wrong-name' does not match directory 'demo-skill' in plugins/demo/skills/demo-skill/SKILL.md"]
    );
}

#[test]
fn plugin_skills_only_check_direct_children_with_a_skill_file() {
    let errors = plugin_errors(|root| {
        write(root, "plugins/demo/skills/README.md", "not a skill");
        fs::create_dir_all(root.join("plugins/demo/skills/empty-dir")).expect("create dir");
        write(
            root,
            "plugins/demo/skills/group/nested-skill/SKILL.md",
            "no frontmatter at all",
        );
        write(
            root,
            "plugins/demo/skills/zz-bad/SKILL.md",
            "no frontmatter at all",
        );
    });
    assert_eq!(
        errors,
        vec!["Missing YAML frontmatter in plugins/demo/skills/zz-bad/SKILL.md"]
    );
}

#[test]
fn plugins_are_visited_in_sorted_order_and_files_are_skipped() {
    let root = TempDir::new("order");
    write_repo(&root);
    write_plugin(&root, "alpha");
    write_plugin(&root, "zeta");
    write(&root, "plugins/zz-readme.txt", "ignored");
    let report = validate_tree(&root, None).expect("validation runs");
    assert_eq!(errors_of(&report), Vec::<String>::new());
    let plugins: Vec<&str> = report
        .progress()
        .into_iter()
        .filter(|line| line.starts_with("Validating plugin: "))
        .collect();
    assert_eq!(
        plugins,
        vec![
            "Validating plugin: alpha",
            "Validating plugin: demo",
            "Validating plugin: zeta",
        ]
    );
}

// ---- SKILL.md frontmatter ---------------------------------------------------

#[test]
fn valid_frontmatter_passes() {
    assert_eq!(
        skill_errors("demo-skill", &skill_text("demo-skill", DESCRIPTION)),
        Vec::<String>::new()
    );
}

#[test]
fn frontmatter_must_exist_and_be_terminated() {
    let rel = skill_rel("demo-skill");
    for content in [
        "",
        "body only\n",
        "\u{feff}---\nname: x\n---\n",
        " ---\nname: x\n---\n",
    ] {
        assert_eq!(
            skill_errors("demo-skill", content),
            vec![format!("Missing YAML frontmatter in {rel}")],
            "{content:?}"
        );
    }
    for content in [
        "---\nname: demo-skill\ndescription: x\n",
        "---\nname: demo-skill\n---",
        "---\n---\nbody\n",
        "---\n",
    ] {
        assert_eq!(
            skill_errors("demo-skill", content),
            vec![format!("Unterminated frontmatter in {rel}")],
            "{content:?}"
        );
    }
}

#[test]
fn crlf_and_lone_cr_line_endings_are_accepted() {
    let crlf = skill_text("demo-skill", DESCRIPTION).replace('\n', "\r\n");
    assert_eq!(skill_errors("demo-skill", &crlf), Vec::<String>::new());
    let cr = skill_text("demo-skill", DESCRIPTION).replace('\n', "\r");
    assert_eq!(skill_errors("demo-skill", &cr), Vec::<String>::new());
}

#[test]
fn name_is_required_kebab_case_bounded_and_matches_the_directory() {
    let rel = skill_rel("demo-skill");
    let missing = format!("Missing 'name' in frontmatter: {rel}");
    assert_eq!(
        skill_errors(
            "demo-skill",
            &format!("---\ndescription: {DESCRIPTION}\n---\n")
        ),
        vec![missing.clone()]
    );
    assert_eq!(
        skill_errors("demo-skill", &skill_text("", DESCRIPTION)),
        vec![missing.clone()]
    );
    assert_eq!(
        skill_errors("demo-skill", &skill_text("\"\"", DESCRIPTION)),
        vec![missing]
    );
    for bad in [
        "Demo-Skill",
        "a",
        "1abc",
        "demo_skill",
        "demo-",
        "-demo",
        "de--mo",
        "demo skill",
        "dé-mo",
        "a-b",
    ] {
        assert_eq!(
            skill_errors("demo-skill", &skill_text(bad, DESCRIPTION)),
            vec![format!("Name '{bad}' is not kebab-case in {rel}")],
            "{bad:?}"
        );
    }
    let too_long = format!("a{}", "b".repeat(64));
    assert_eq!(
        skill_errors(&too_long, &skill_text(&too_long, DESCRIPTION)),
        vec![format!("Name exceeds 64 chars in {}", skill_rel(&too_long))]
    );
    let longest = format!("a{}", "b".repeat(63));
    assert_eq!(
        skill_errors(&longest, &skill_text(&longest, DESCRIPTION)),
        Vec::<String>::new()
    );
    assert_eq!(
        skill_errors("demo-skill", &skill_text("other-skill", DESCRIPTION)),
        vec![format!(
            "Name 'other-skill' does not match directory 'demo-skill' in {rel}"
        )]
    );
}

#[test]
fn description_is_required_and_at_least_twenty_characters() {
    let rel = skill_rel("demo-skill");
    assert_eq!(
        skill_errors("demo-skill", "---\nname: demo-skill\n---\n"),
        vec![format!("Missing 'description' in frontmatter: {rel}")]
    );
    assert_eq!(
        skill_errors("demo-skill", &skill_text("demo-skill", "")),
        vec![format!("Missing 'description' in frontmatter: {rel}")]
    );
    let short = "x".repeat(19);
    assert_eq!(
        skill_errors("demo-skill", &skill_text("demo-skill", &short)),
        vec![format!("Description too short (<20 chars) in {rel}")]
    );
    let exact = "x".repeat(20);
    assert_eq!(
        skill_errors("demo-skill", &skill_text("demo-skill", &exact)),
        Vec::<String>::new()
    );
    // Characters, not bytes: 19 two-byte characters are still too short.
    let accents = "é".repeat(19);
    assert_eq!(
        skill_errors("demo-skill", &skill_text("demo-skill", &accents)),
        vec![format!("Description too short (<20 chars) in {rel}")]
    );
    let accents_ok = "é".repeat(20);
    assert_eq!(
        skill_errors("demo-skill", &skill_text("demo-skill", &accents_ok)),
        Vec::<String>::new()
    );
}

#[test]
fn both_name_and_description_problems_are_reported_together() {
    let rel = skill_rel("demo-skill");
    assert_eq!(
        skill_errors("demo-skill", "---\ntitle: x\n---\n"),
        vec![
            format!("Missing 'name' in frontmatter: {rel}"),
            format!("Missing 'description' in frontmatter: {rel}"),
        ]
    );
}

#[test]
fn frontmatter_values_follow_the_simple_reader() {
    let rel = skill_rel("demo-skill");
    // Quotes stripped; block-scalar indicators dropped; continuation lines
    // joined with single spaces; the first colon splits key from value.
    for content in [
        "---\nname: \"demo-skill\"\ndescription: 'A description that is long enough'\n---\n",
        "---\nname: demo-skill\ndescription: >\n  A description that is\n  long enough.\n---\n",
        "---\nname: demo-skill\ndescription: |-\n  A description that is\n  long enough.\n---\n",
        "---\nname: demo-skill\ndescription: Use when: something happens, as needed\n---\n",
        "---\nname: demo-skill\nmetadata:\n  name: other\n  description: x\ndescription: A description that is long enough\n---\n",
        "---\nname: demo-skill\ndescription: A description\n  that continues here\n---\n",
        "---\nname: demo-skill\nunrelated line\ndescription: A description that is long enough\n---\n",
        "---\n  : ignored\nname: demo-skill\n description: x\ndescription: A description that is long enough\n---\n",
    ] {
        assert_eq!(skill_errors("demo-skill", content), Vec::<String>::new(), "{content:?}");
    }
    // A key indented by one space is not a key; nested content never supplies one.
    assert_eq!(
        skill_errors(
            "demo-skill",
            "---\n name: demo-skill\ndescription: A description that is long enough\n---\n"
        ),
        vec![format!("Missing 'name' in frontmatter: {rel}")]
    );
    assert_eq!(
        skill_errors("demo-skill", "---\nmetadata:\n  name: demo-skill\ndescription: A description that is long enough\n---\n"),
        vec![format!("Missing 'name' in frontmatter: {rel}")]
    );
    // Folded text that is still short after joining.
    assert_eq!(
        skill_errors(
            "demo-skill",
            "---\nname: demo-skill\ndescription: >-\n  short\n  words\n---\n"
        ),
        vec![format!("Description too short (<20 chars) in {rel}")]
    );
}

#[test]
fn unreadable_skill_files_are_reported() {
    let root = TempDir::new("unreadable");
    let rel = skill_rel("demo-skill");
    fs::create_dir_all(root.join("skills/demo-skill")).expect("create dir");
    fs::write(root.join(&rel), b"---\nname: \xff\n---\n").expect("write fixture");
    let mut validator = Validator::new(&root);
    validator.validate_skill_frontmatter(&rel, "demo-skill");
    assert_eq!(
        errors_of(&validator.report),
        vec![format!("Cannot read {rel}: not valid UTF-8")]
    );
}

// ---- top-level skills -------------------------------------------------------

fn top_level_report(change: impl FnOnce(&Path)) -> Report {
    let root = TempDir::new("top");
    change(&root);
    let mut validator = Validator::new(&root);
    validator.validate_top_level_skills();
    validator.report
}

#[test]
fn top_level_skills_are_found_recursively_in_component_order() {
    let report = top_level_report(|root| {
        write(
            root,
            "skills/aa/bb/SKILL.md",
            &skill_text("bb", DESCRIPTION),
        );
        write(
            root,
            "skills/aa-bb/SKILL.md",
            &skill_text("aa-bb", DESCRIPTION),
        );
        write(
            root,
            "skills/aa/cc/deep/er/SKILL.md",
            &skill_text("er", DESCRIPTION),
        );
        write(
            root,
            "skills/.hidden/hh-skill/SKILL.md",
            &skill_text("hh-skill", DESCRIPTION),
        );
        write(root, "skills/SKILL.md", &skill_text("skills", DESCRIPTION));
        write(root, "skills/aa/notes.md", "ignored");
    });
    assert_eq!(errors_of(&report), Vec::<String>::new());
    // Path components sort one by one, not as strings: `aa/bb` comes before
    // `aa-bb` although '-' is smaller than '/', and a `SKILL.md` directly in
    // `skills/` sorts by its own name (`SKILL.md` is after `.hidden`, before `aa`).
    assert_eq!(
        report.progress(),
        vec![
            "Validating skill: skills/.hidden/hh-skill",
            "Validating skill: skills",
            "Validating skill: skills/aa/bb",
            "Validating skill: skills/aa/cc/deep/er",
            "Validating skill: skills/aa-bb",
        ]
    );
}

#[test]
fn top_level_skill_errors_use_repository_relative_paths() {
    let report = top_level_report(|root| {
        write(root, "skills/g/bad-one/SKILL.md", "plain text");
        write(
            root,
            "skills/g/short-one/SKILL.md",
            &skill_text("short-one", "tiny"),
        );
    });
    assert_eq!(
        errors_of(&report),
        vec![
            "Missing YAML frontmatter in skills/g/bad-one/SKILL.md",
            "Description too short (<20 chars) in skills/g/short-one/SKILL.md",
        ]
    );
}

#[test]
fn missing_skills_directory_is_fine() {
    let report = top_level_report(|_| {});
    assert!(report.events().is_empty());
}

#[cfg(unix)]
#[test]
fn top_level_walk_never_follows_symlinks() {
    use std::os::unix::fs::symlink;
    let outside = TempDir::new("walk-outside");
    write(
        &outside,
        "trapped/SKILL.md",
        "outside the tree, not a valid skill",
    );
    write(&outside, "real.md", &skill_text("real", DESCRIPTION));
    let report = top_level_report(|root| {
        write(
            root,
            "skills/ok-skill/SKILL.md",
            &skill_text("ok-skill", DESCRIPTION),
        );
        symlink(&*outside, root.join("skills/linked-dir")).expect("create dir symlink");
        fs::create_dir_all(root.join("skills/with-link-file")).expect("create dir");
        symlink(
            outside.join("real.md"),
            root.join("skills/with-link-file/SKILL.md"),
        )
        .expect("create file symlink");
    });
    assert_eq!(
        errors_of(&report),
        vec!["Cannot read skills/with-link-file/SKILL.md: symbolic links are not followed"]
    );
    assert_eq!(
        report.progress(),
        vec![
            "Validating skill: skills/ok-skill",
            "Validating skill: skills/with-link-file",
        ]
    );
}

// ---- plugin selection and roots --------------------------------------------

#[test]
fn single_plugin_skips_marketplaces_and_top_level_skills() {
    let root = TempDir::new("select");
    write_plugin(&root, "demo");
    write(&root, ".claude-plugin/marketplace.json", "not json");
    write(&root, "skills/bad/SKILL.md", "not a skill");
    let report = validate_tree(&root, Some("demo")).expect("plugin exists");
    assert_eq!(report.progress(), vec!["Validating plugin: demo"]);
    assert_eq!(errors_of(&report), Vec::<String>::new());
    // The whole tree does see the broken files.
    assert_eq!(tree_errors(&root).len(), 4);
}

#[test]
fn unknown_or_unsafe_plugin_names_are_runtime_errors() {
    let root = TempDir::new("notfound");
    write_repo(&root);
    write(&root, "plugins/a-file", "x");
    fs::create_dir_all(root.join("outside")).expect("create dir");
    for name in [
        "ghost",
        "a-file",
        "",
        ".",
        "..",
        "../outside",
        "demo/..",
        "demo/",
        "a/b",
        "demo\\x",
        "de\0mo",
    ] {
        let error = validate_tree(&root, Some(name)).expect_err(name);
        assert_eq!(error.kind(), ErrorKind::Runtime, "{name:?}");
        assert_eq!(error.code(), "plugin_not_found", "{name:?}");
        assert_eq!(error.message(), format!("Plugin not found: {name}"));
    }
}

#[test]
fn unusable_roots_are_runtime_errors() {
    let root = TempDir::new("roots");
    write(&root, "file.txt", "x");
    let missing = validate_tree(&root.join("absent"), None).expect_err("missing root");
    assert_eq!(missing.code(), "root_not_found");
    assert!(missing.message().starts_with("root does not exist: "));
    let file = validate_tree(&root.join("file.txt"), None).expect_err("file root");
    assert_eq!(file.code(), "root_not_directory");
    assert!(file.message().starts_with("root is not a directory: "));
}

// ---- command handler, routing and exit behaviour ---------------------------

fn os_args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn exec(values: &[&str]) -> (u8, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = run(&os_args(values), &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

struct FailingWriter(io::ErrorKind);

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from(self.0))
    }
}

fn root_arg(root: &TempDir) -> &str {
    root.to_str().expect("utf-8 temp path")
}

#[test]
fn command_success_prints_progress_then_the_pass_line() {
    let root = TempDir::new("cmd-ok");
    write_repo(&root);
    let (status, stdout, stderr) = exec(&["manifests", "validate", root_arg(&root)]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "Validating Claude Code marketplace: .claude-plugin/marketplace.json\n\
         Validating Codex marketplace: .agents/plugins/marketplace.json\n\
         Validating Cursor marketplace: .cursor-plugin/marketplace.json\n\
         Validating plugin: demo\n\
         Validating skill: skills/core/top-skill\n\
         \n\
         All validations passed.\n"
    );
}

#[test]
fn command_failure_reports_errors_on_stderr_and_exits_one() {
    let root = TempDir::new("cmd-fail");
    write_repo(&root);
    fs::remove_file(root.join(CLAUDE_REL)).expect("remove claude manifest");
    write(
        &root,
        "skills/core/top-skill/SKILL.md",
        &skill_text("top-skill", "tiny"),
    );
    let (status, stdout, stderr) = exec(&["manifests", "validate", root_arg(&root)]);
    assert_eq!(status, 1);
    assert_eq!(
        stdout,
        "Validating Claude Code marketplace: .claude-plugin/marketplace.json\n\
         Validating Codex marketplace: .agents/plugins/marketplace.json\n\
         Validating Cursor marketplace: .cursor-plugin/marketplace.json\n\
         Validating plugin: demo\n\
         Validating skill: skills/core/top-skill\n"
    );
    assert_eq!(
        stderr,
        format!(
            "  ERROR: Missing file: {CLAUDE_REL}\n  ERROR: Description too short (<20 chars) in skills/core/top-skill/SKILL.md\nerror: Validation failed with 2 error(s).\n"
        )
    );
}

#[test]
fn command_plugin_option_selects_one_plugin() {
    let root = TempDir::new("cmd-plugin");
    write_repo(&root);
    write(&root, ".claude-plugin/marketplace.json", "broken");
    for args in [
        vec!["manifests", "validate", root_arg(&root), "--plugin", "demo"],
        vec!["manifests", "validate", "--plugin", "demo", root_arg(&root)],
    ] {
        let (status, stdout, stderr) = exec(&args);
        assert_eq!((status, stderr.as_str()), (0, ""), "{args:?}");
        assert_eq!(
            stdout,
            "Validating plugin: demo\n\nAll validations passed.\n"
        );
    }
    let (status, stdout, _) = exec(&["manifests", "validate", root_arg(&root), "--plugin=demo"]);
    assert_eq!(status, 0);
    assert_eq!(
        stdout,
        "Validating plugin: demo\n\nAll validations passed.\n"
    );
}

#[test]
fn command_unknown_plugin_exits_one_with_a_message_only() {
    let root = TempDir::new("cmd-ghost");
    write_repo(&root);
    let (status, stdout, stderr) = exec(&[
        "manifests",
        "validate",
        root_arg(&root),
        "--plugin",
        "ghost",
    ]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(stderr, "error: Plugin not found: ghost\n");
}

#[test]
fn command_missing_root_exits_one() {
    let root = TempDir::new("cmd-root");
    let missing = root.join("absent");
    let (status, stdout, stderr) =
        exec(&["manifests", "validate", missing.to_str().expect("utf-8")]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(
        stderr.starts_with("error: root does not exist: "),
        "{stderr}"
    );
    assert!(!stderr.contains("Usage:"));
}

#[test]
fn command_usage_errors_exit_two() {
    for args in [
        vec!["manifests"],
        vec!["manifests", "check"],
        vec!["manifests", "validate", "one", "two"],
        vec!["manifests", "validate", "--plugin"],
        vec!["manifests", "validate", "--json"],
        vec!["manifests", "validate", "--wat"],
        vec!["manifests", "validate", "--plugins=x"],
    ] {
        let (status, stdout, stderr) = exec(&args);
        assert_eq!(status, 2, "{args:?}");
        assert_eq!(stdout, "");
        assert!(
            stderr.starts_with("error: invalid arguments\n\nUsage:"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn help_documents_the_command() {
    let (status, stdout, stderr) = exec(&["manifests", "validate", "--help"]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.contains("toolkit manifests validate [ROOT] [--plugin NAME]"));
    assert!(stdout.contains("--plugin NAME"));
}

#[test]
fn closed_stdout_still_validates_and_keeps_the_exit_status() {
    let root = TempDir::new("cmd-pipe");
    write_repo(&root);
    let args = os_args(&["manifests", "validate", root_arg(&root)]);
    let mut sink = FailingWriter(io::ErrorKind::BrokenPipe);
    let mut err = Vec::new();
    assert_eq!(run(&args, &mut sink, &mut err), 0);
    assert!(err.is_empty());

    fs::remove_file(root.join(CLAUDE_REL)).expect("remove claude manifest");
    let mut sink = FailingWriter(io::ErrorKind::BrokenPipe);
    let mut err = Vec::new();
    assert_eq!(run(&args, &mut sink, &mut err), 1);
    let stderr = String::from_utf8(err).expect("utf-8 stderr");
    assert!(stderr.contains("  ERROR: Missing file: "));
    assert!(stderr.ends_with("error: Validation failed with 1 error(s).\n"));
}

#[test]
fn other_stdout_failures_and_closed_stderr_do_not_panic() {
    let root = TempDir::new("cmd-write");
    write_repo(&root);
    let args = os_args(&["manifests", "validate", root_arg(&root)]);
    let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
    let mut err = Vec::new();
    assert_eq!(run(&args, &mut sink, &mut err), 1);
    assert!(String::from_utf8(err)
        .expect("utf-8 stderr")
        .starts_with("error: cannot write to stdout: "));

    fs::remove_file(root.join(CLAUDE_REL)).expect("remove claude manifest");
    let mut out = Vec::new();
    let mut closed_err = FailingWriter(io::ErrorKind::BrokenPipe);
    assert_eq!(run(&args, &mut out, &mut closed_err), 1);
}

fn route_words(values: &[&str]) -> Result<Routed<'static>, ToolkitError> {
    let words: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
    route(COMMANDS, &words)
}

#[test]
fn routing_understands_value_flags() {
    for values in [
        vec!["manifests", "validate", "--plugin", "x", "root"],
        vec!["manifests", "validate", "root", "--plugin=x"],
        vec![
            "manifests",
            "validate",
            "--plugin",
            "y",
            "--plugin",
            "x",
            "root",
        ],
    ] {
        let Ok(Routed::Run(spec, invocation)) = route_words(&values) else {
            panic!("{values:?} must route");
        };
        assert_eq!(spec.path, &["manifests", "validate"]);
        assert_eq!(invocation.value("--plugin"), Some("x"), "{values:?}");
        assert_eq!(invocation.positional(0), Some("root"), "{values:?}");
        assert!(invocation.flags.is_empty());
    }
    // A value flag takes the next word whatever it looks like; absent means None.
    let Ok(Routed::Run(_, invocation)) = route_words(&["manifests", "validate", "--plugin", "-x"])
    else {
        panic!("must route");
    };
    assert_eq!(invocation.value("--plugin"), Some("-x"));
    let Ok(Routed::Run(_, invocation)) = route_words(&["manifests", "validate"]) else {
        panic!("must route");
    };
    assert_eq!(invocation.value("--plugin"), None);
    assert!(matches!(
        route_words(&["manifests", "validate", "-h"]),
        Ok(Routed::Help)
    ));
    assert!(route_words(&["manifests", "validate", "--plugin"]).is_err());
}

// ---- helper unit tests ------------------------------------------------------

#[test]
fn kebab_matches_the_original_regular_expression() {
    for good in [
        "ab",
        "abc",
        "a1",
        "ab-cd",
        "a1-b2-c3",
        "aws-lambda-durable-functions",
        "ab-1",
    ] {
        assert!(is_kebab(good), "{good}");
    }
    for bad in [
        "", "a", "-", "ab-", "-ab", "a-b", "ab--cd", "Ab", "aB", "1a", "a_b", "ab cd", "ab\n",
        "ab-c d", "é1",
    ] {
        assert!(!is_kebab(bad), "{bad:?}");
    }
}

#[test]
fn splitlines_follows_python() {
    assert_eq!(py_splitlines(""), Vec::<&str>::new());
    assert_eq!(py_splitlines("a"), vec!["a"]);
    assert_eq!(py_splitlines("a\n"), vec!["a"]);
    assert_eq!(py_splitlines("a\n\nb"), vec!["a", "", "b"]);
    assert_eq!(py_splitlines("\na"), vec!["", "a"]);
    assert_eq!(py_splitlines("a\r\nb\rc"), vec!["a", "b", "c"]);
    assert_eq!(
        py_splitlines("a\u{b}b\u{c}c\u{1c}d\u{1d}e\u{1e}f\u{85}g\u{2028}h\u{2029}i"),
        vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]
    );
    assert_eq!(py_splitlines("a\u{1f}b"), vec!["a\u{1f}b"]);
}

#[test]
fn universal_newlines_translate_like_text_mode() {
    assert_eq!(universal_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
}

#[test]
fn simple_frontmatter_reader_edge_cases() {
    let fields = parse_simple_frontmatter(": value\n  continuation ignored\nname: a\n  b\nname: c");
    assert_eq!(fields.get("name").map(String::as_str), Some("c"));
    assert_eq!(fields.get("").map(String::as_str), Some("value"));
    assert_eq!(fields.len(), 2);

    let fields = parse_simple_frontmatter("k:  \"'x'\"  \nj: >\nl: ''\nm: \u{1f} spaced \u{1f}");
    assert_eq!(fields.get("k").map(String::as_str), Some("x"));
    assert_eq!(fields.get("j").map(String::as_str), Some(""));
    assert_eq!(fields.get("l").map(String::as_str), Some(""));
    assert_eq!(fields.get("m").map(String::as_str), Some("spaced"));

    // Quote characters are stripped as a set from both ends, in order.
    let fields = parse_simple_frontmatter("a: \"\"x\"'\nb: it's");
    assert_eq!(fields.get("a").map(String::as_str), Some("x\""));
    assert_eq!(fields.get("b").map(String::as_str), Some("it's"));
}

#[test]
fn python_style_repr() {
    assert_eq!(py_repr_str("plain"), "'plain'");
    assert_eq!(py_repr_str("it's"), "\"it's\"");
    assert_eq!(py_repr_str("both ' and \""), "'both \\' and \"'");
    assert_eq!(py_repr_str("a\\b\nc\td\u{1}"), "'a\\\\b\\nc\\td\\x01'");
    assert_eq!(py_repr(&Json::Null), "None");
    assert_eq!(py_repr(&Json::Bool(true)), "True");
    assert_eq!(py_repr(&Json::Bool(false)), "False");
    assert_eq!(py_repr(&Json::Int(-3)), "-3");
    assert_eq!(py_repr(&Json::Float(1.5)), "1.5");
    assert_eq!(
        py_repr(&Json::Array(vec![Json::string("a"), Json::Int(1)])),
        "['a', 1]"
    );
    assert_eq!(
        py_repr(&Json::object(vec![("k", Json::Null)])),
        "{'k': None}"
    );
}
