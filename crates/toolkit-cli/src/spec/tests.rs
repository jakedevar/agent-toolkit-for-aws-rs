//! Offline, deterministic tests for `toolkit spec validate` (#4376).
//!
//! Every rule of `tools/validate_spec.py` has a positive and a negative
//! fixture here, plus a run over this repository's real tree. Fixtures live in
//! unique temporary directories removed on drop; the vendored schemas are
//! copied from `tools/schemas/1.0.0`; nothing touches the network.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    engine, find_placeholders, looks_like_path, nfkc, parse_frontmatter, split_url, validate_tree,
    Frontmatter, UrlParts,
};
use crate::manifests::Report;
use crate::{run, COMMANDS};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-spec-{label}-{}-{}",
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

const PLUGIN_ID: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const MCP_ID: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
const MANIFEST: &str = "plugins/demo/plugin.json";
const MCP: &str = "plugins/demo/mcp.json";
const DESCRIPTION: &str = "A description that is long enough.";

fn plugin_json(name: &str) -> String {
    r#"{"$schema":"@ID@","name":"@NAME@","version":"1.0.0","description":"Demo plugin.","author":{"name":"Dev","email":"dev@example.test","url":"https://example.test"},"keywords":["demo"],"extensions":{"com.example.demo":{"hook":"./hooks/run.sh","nested":{"items":["plain text","./hooks/run.sh"]}}}}"#
        .replace("@ID@", PLUGIN_ID)
        .replace("@NAME@", name)
}

fn valid_mcp() -> String {
    r#"{"$schema":"@ID@","mcpServers":{"local":{"type":"stdio","command":"./bin/run","args":["--root","${PLUGIN_ROOT}/data","plain"],"env":{"MODE":"x"},"cwd":"${PLUGIN_DATA}"},"bare":{"type":"stdio","command":"node"},"remote":{"type":"streamable-http","url":"https://example.test/mcp","headers":{"X-Api":"v"}},"legacy":{"type":"sse","url":"http://localhost:8080/sse"}}}"#
        .replace("@ID@", MCP_ID)
}

fn skill_text(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {DESCRIPTION}\nlicense: MIT\nallowed-tools: Read Grep\ncompatibility: Works anywhere.\nmetadata:\n  version: \"1\"\n---\nBody\n"
    )
}

fn copy_schemas(root: &Path) {
    for name in ["plugin.schema.json", "mcp.schema.json"] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/schemas/1.0.0")
            .join(name);
        let text = fs::read_to_string(source).expect("read vendored schema");
        write(root, &format!("tools/schemas/1.0.0/{name}"), &text);
    }
}

/// A fully valid repository: vendored schemas plus one plugin `demo`.
fn write_valid_repo(root: &Path) {
    copy_schemas(root);
    write(root, MANIFEST, &plugin_json("demo"));
    write(root, MCP, &valid_mcp());
    write(root, "plugins/demo/hooks/run.sh", "#!/bin/sh\n");
    write(
        root,
        "plugins/demo/skills/demo-skill/SKILL.md",
        &skill_text("demo-skill"),
    );
}

fn errors_of(report: &Report) -> Vec<String> {
    report.errors().into_iter().map(str::to_owned).collect()
}

fn repo_errors(change: impl FnOnce(&Path)) -> Vec<String> {
    let root = TempDir::new("repo");
    write_valid_repo(&root);
    change(&root);
    errors_of(&validate_tree(&root).expect("validation runs"))
}

fn manifest_errors(json: &str) -> Vec<String> {
    repo_errors(|root| write(root, MANIFEST, json))
}

fn mcp_errors(json: &str) -> Vec<String> {
    repo_errors(|root| write(root, MCP, json))
}

fn server_errors(name: &str, server: &str) -> Vec<String> {
    mcp_errors(
        &r#"{"$schema":"@ID@","mcpServers":{"@N@":@S@}}"#
            .replace("@ID@", MCP_ID)
            .replace("@N@", name)
            .replace("@S@", server),
    )
}

fn skill_rel(dir: &str) -> String {
    format!("plugins/demo/skills/{dir}/SKILL.md")
}

fn skill_errors(dir: &str, content: &[u8]) -> Vec<String> {
    repo_errors(|root| {
        fs::remove_dir_all(root.join("plugins/demo/skills")).expect("remove default skill");
        let path = root.join("plugins/demo/skills").join(dir).join("SKILL.md");
        fs::create_dir_all(path.parent().expect("skill has a parent")).expect("create skill dir");
        fs::write(path, content).expect("write skill");
    })
}

fn frontmatter_errors(dir: &str, body: &str) -> Vec<String> {
    skill_errors(dir, format!("---\n{body}---\nBody\n").as_bytes())
}

fn fields(name: &str, description: &str) -> String {
    format!("name: {name}\ndescription: {description}\n")
}

// ---- baseline and rule 1: symlinks -------------------------------------------

#[test]
fn valid_repository_passes_with_progress_lines() {
    let root = TempDir::new("valid");
    write_valid_repo(&root);
    let report = validate_tree(&root).expect("validation runs");
    assert_eq!(report.errors(), Vec::<&str>::new());
    assert_eq!(
        report.progress(),
        vec!["Validating plugin: plugins/demo", "Validating skills"]
    );
}

#[cfg(unix)]
#[test]
fn symlinks_under_plugins_are_rejected_in_component_order() {
    use std::os::unix::fs::symlink;
    let errors = repo_errors(|root| {
        symlink("hooks", root.join("plugins/demo/hooks-link")).expect("dir link");
        symlink("plugin.json", root.join("plugins/demo/a-link")).expect("file link");
    });
    assert_eq!(
        errors,
        [
            "plugins/demo/a-link: symlinks are not allowed under plugins/",
            "plugins/demo/hooks-link: symlinks are not allowed under plugins/",
        ]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_skill_files_are_rejected_and_never_read() {
    use std::os::unix::fs::symlink;
    let errors = repo_errors(|root| {
        let skill = root.join("plugins/demo/skills/demo-skill/SKILL.md");
        fs::remove_file(&skill).expect("remove skill");
        symlink(root.join("plugins/demo/hooks/run.sh"), &skill).expect("link skill");
    });
    assert_eq!(
        errors,
        [
            "plugins/demo/skills/demo-skill/SKILL.md: symlinks are not allowed under plugins/"
                .to_string(),
            format!(
                "{}: cannot read as UTF-8: symbolic links are not followed",
                skill_rel("demo-skill")
            ),
        ]
    );
}

#[cfg(unix)]
#[test]
fn symlink_directories_are_reported_but_not_descended() {
    use std::os::unix::fs::symlink;
    let outside = TempDir::new("outside");
    write(&outside, "inner/deep-link-target", "x");
    let errors = repo_errors(|root| {
        symlink(&*outside, root.join("plugins/demo/out")).expect("dir link");
    });
    assert_eq!(
        errors,
        ["plugins/demo/out: symlinks are not allowed under plugins/"]
    );
}

// ---- plugin.json: presence, JSON, schema, explicit checks ----------------------

#[test]
fn missing_plugin_json_is_reported() {
    let errors = repo_errors(|root| {
        fs::remove_file(root.join(MANIFEST)).expect("remove manifest");
    });
    assert_eq!(
        errors,
        ["plugins/demo: missing required root plugin.json (spec §5.1)"]
    );
}

#[test]
fn invalid_plugin_json_is_reported() {
    let errors = manifest_errors("{");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("plugins/demo/plugin.json: invalid JSON: JSON parse error"),
        "{errors:?}"
    );
}

#[test]
fn unreadable_plugin_json_is_a_finding_not_a_crash() {
    let errors = repo_errors(|root| {
        fs::remove_file(root.join(MANIFEST)).expect("remove manifest");
        fs::create_dir_all(root.join(MANIFEST)).expect("directory in place of file");
    });
    assert_eq!(
        errors,
        ["plugins/demo/plugin.json: cannot read: not a regular file"]
    );
}

#[test]
fn plugin_json_must_be_an_object() {
    assert_eq!(
        manifest_errors("[]"),
        ["plugins/demo/plugin.json: [] is not of type 'object' (at <root>)"]
    );
}

#[test]
fn empty_object_reports_required_members_and_explicit_checks() {
    assert_eq!(
        manifest_errors("{}"),
        [
            "plugins/demo/plugin.json: '$schema' is a required property (at <root>)".to_string(),
            "plugins/demo/plugin.json: 'name' is a required property (at <root>)".to_string(),
            format!("plugins/demo/plugin.json: $schema must be {PLUGIN_ID}"),
            "plugins/demo/plugin.json: name None != directory 'demo'".to_string(),
        ]
    );
}

#[test]
fn schema_violations_are_reported_sorted_by_path() {
    let errors = manifest_errors(
        r#"{"$schema":"https://example.test/x.json","name":"Bad_Name","version":1,"unknown":true,"author":{"phone":"1"},"keywords":["a",2]}"#,
    );
    assert_eq!(errors.len(), 8, "{errors:?}");
    assert_eq!(
        errors[0],
        "plugins/demo/plugin.json: Additional properties are not allowed ('unknown' was unexpected) (at <root>)"
    );
    assert_eq!(
        errors[1],
        format!("plugins/demo/plugin.json: '{PLUGIN_ID}' was expected (at $schema)")
    );
    assert_eq!(
        errors[2],
        "plugins/demo/plugin.json: Additional properties are not allowed ('phone' was unexpected) (at author)"
    );
    assert_eq!(
        errors[3],
        "plugins/demo/plugin.json: 2 is not of type 'string' (at keywords/1)"
    );
    assert!(
        errors[4].starts_with("plugins/demo/plugin.json: 'Bad_Name' does not match '^")
            && errors[4].ends_with(" (at name)"),
        "{}",
        errors[4]
    );
    assert_eq!(
        errors[5],
        "plugins/demo/plugin.json: 1 is not of type 'string' (at version)"
    );
    assert_eq!(
        errors[6],
        format!("plugins/demo/plugin.json: $schema must be {PLUGIN_ID}")
    );
    assert_eq!(
        errors[7],
        "plugins/demo/plugin.json: name 'Bad_Name' != directory 'demo'"
    );
}

#[test]
fn name_pattern_and_length_come_from_the_vendored_schema() {
    for bad in ["-a", "a-", "a--b", "a..b", "A", "a_b", ".a", "a."] {
        let errors = manifest_errors(&plugin_json(bad));
        assert!(
            errors
                .iter()
                .any(|e| e.contains(&format!("'{bad}' does not match '^"))
                    && e.ends_with("(at name)")),
            "{bad}: {errors:?}"
        );
    }
    // A name the pattern accepts differs from the directory only.
    assert_eq!(
        manifest_errors(&plugin_json("a.b-c")),
        ["plugins/demo/plugin.json: name 'a.b-c' != directory 'demo'"]
    );
    let errors = manifest_errors(&plugin_json(&"a".repeat(65)));
    assert!(
        errors.iter().any(|e| e.ends_with(" is too long (at name)")),
        "{errors:?}"
    );
    let errors = manifest_errors(&plugin_json(""));
    assert_eq!(
        errors[0],
        "plugins/demo/plugin.json: '' should be non-empty (at name)"
    );
    assert!(errors[1].contains("'' does not match '^"), "{errors:?}");
    assert_eq!(
        errors[2],
        "plugins/demo/plugin.json: name '' != directory 'demo'"
    );
}

#[test]
fn name_must_equal_the_directory() {
    assert_eq!(
        manifest_errors(&plugin_json("other")),
        ["plugins/demo/plugin.json: name 'other' != directory 'demo'"]
    );
    assert_eq!(manifest_errors(&plugin_json("demo")), Vec::<String>::new());
}

// ---- plugin.json: extensions ---------------------------------------------------

const EXTENSION_LABEL: &str = "plugins/demo/plugin.json extensions.com.x.p";

fn extension_errors(value: &str) -> Vec<String> {
    repo_errors(|root| {
        write(
            root,
            MANIFEST,
            &r#"{"$schema":"@ID@","name":"demo","extensions":{"com.x":{"p":@V@}}}"#
                .replace("@ID@", PLUGIN_ID)
                .replace("@V@", value),
        );
    })
}

#[test]
fn extension_values_that_are_not_paths_or_that_exist_pass() {
    assert_eq!(extension_errors("\"./hooks/run.sh\""), Vec::<String>::new());
    assert_eq!(extension_errors("\"plain text\""), Vec::<String>::new());
    assert_eq!(extension_errors("5"), Vec::<String>::new());
}

#[test]
fn extension_paths_must_exist_and_stay_inside() {
    assert_eq!(
        extension_errors("\"./missing.txt\""),
        [format!(
            "{EXTENSION_LABEL}: points at missing ./missing.txt"
        )]
    );
    assert_eq!(
        extension_errors("\"../outside\""),
        [format!(
            "{EXTENSION_LABEL}: plugin-relative path must begin with './' (got '../outside')"
        )]
    );
    assert_eq!(
        extension_errors("\"/abs/path\""),
        [format!(
            "{EXTENSION_LABEL}: plugin-relative path must begin with './' (got '/abs/path')"
        )]
    );
    assert_eq!(
        extension_errors("\"${PLUGIN_ROOT}/x\""),
        [format!(
            "{EXTENSION_LABEL}: plugin-relative path must begin with './' (got '${{PLUGIN_ROOT}}/x')"
        )]
    );
    assert_eq!(
        extension_errors("\"./hooks/../../x\""),
        [
            format!("{EXTENSION_LABEL}: path escapes the plugin root ('./hooks/../../x')"),
            format!("{EXTENSION_LABEL}: points at missing ./hooks/../../x"),
        ]
    );
    assert_eq!(
        extension_errors("\"a\\\\b\""),
        [format!(
            "{EXTENSION_LABEL}: path must use '/' separators (got 'a\\\\b')"
        )]
    );
}

#[test]
fn extension_leaves_are_checked_at_any_depth_with_indexed_labels() {
    let errors = repo_errors(|root| {
        write(
            root,
            MANIFEST,
            &r#"{"$schema":"@ID@","name":"demo","extensions":{"com.x":{"deep":{"list":["ok",{"p":"./nope"}]}}}}"#
                .replace("@ID@", PLUGIN_ID),
        );
    });
    assert_eq!(
        errors,
        ["plugins/demo/plugin.json extensions.com.x.deep.list[1].p: points at missing ./nope"]
    );
}

#[test]
fn extensions_must_be_an_object_of_reverse_domain_namespaces() {
    let with = |extensions: &str| {
        manifest_errors(
            &r#"{"$schema":"@ID@","name":"demo","extensions":@E@}"#
                .replace("@ID@", PLUGIN_ID)
                .replace("@E@", extensions),
        )
    };
    assert_eq!(
        with("[]"),
        [
            "plugins/demo/plugin.json: [] is not of type 'object' (at extensions)",
            "plugins/demo/plugin.json: extensions must be an object (spec §8.1)",
        ]
    );
    assert_eq!(
        with(r#"{"nodots":{"k":"v"}}"#),
        ["plugins/demo/plugin.json: extension namespace 'nodots' is not reverse-domain (spec §8)"]
    );
    assert_eq!(
        with(r#"{"com.x":"./hooks/run.sh"}"#),
        ["plugins/demo/plugin.json: './hooks/run.sh' is not of type 'object' (at extensions/com.x)"]
    );
    assert_eq!(with("{}"), Vec::<String>::new());
    // A JSON null counts as absent for the explicit check; the schema still objects.
    assert_eq!(
        with("null"),
        ["plugins/demo/plugin.json: None is not of type 'object' (at extensions)"]
    );
}

#[cfg(unix)]
#[test]
fn extension_symlink_escapes_are_detected_by_resolution() {
    use std::os::unix::fs::symlink;
    let outside = TempDir::new("escape-target");
    let errors = repo_errors(|root| {
        write(
            root,
            MANIFEST,
            &r#"{"$schema":"@ID@","name":"demo","extensions":{"com.x":{"p":"./escape/x"}}}"#
                .replace("@ID@", PLUGIN_ID),
        );
        symlink(&*outside, root.join("plugins/demo/escape")).expect("link outside");
    });
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert_eq!(
        errors[0],
        "plugins/demo/escape: symlinks are not allowed under plugins/"
    );
    assert!(
        errors[1].starts_with(&format!(
            "{EXTENSION_LABEL}: path resolves outside the plugin root ('./escape/x' -> "
        )),
        "{errors:?}"
    );
    assert_eq!(
        errors[2],
        format!("{EXTENSION_LABEL}: points at missing ./escape/x")
    );
}

// ---- mcp.json ------------------------------------------------------------------

#[test]
fn valid_mcp_configuration_passes_and_is_optional() {
    assert_eq!(mcp_errors(&valid_mcp()), Vec::<String>::new());
    let errors = repo_errors(|root| fs::remove_file(root.join(MCP)).expect("remove mcp.json"));
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn invalid_mcp_json_is_reported() {
    let errors = mcp_errors("{nope");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("plugins/demo/mcp.json: invalid JSON: JSON parse error"),
        "{errors:?}"
    );
}

#[test]
fn mcp_requires_schema_and_servers() {
    assert_eq!(
        mcp_errors("{}"),
        [
            "plugins/demo/mcp.json: '$schema' is a required property (at <root>)".to_string(),
            "plugins/demo/mcp.json: 'mcpServers' is a required property (at <root>)".to_string(),
            format!("plugins/demo/mcp.json: $schema must be {MCP_ID}"),
        ]
    );
    assert_eq!(
        mcp_errors("[]"),
        ["plugins/demo/mcp.json: [] is not of type 'object' (at <root>)"]
    );
    assert_eq!(
        mcp_errors(&r#"{"$schema":"@ID@","mcpServers":[]}"#.replace("@ID@", MCP_ID)),
        ["plugins/demo/mcp.json: [] is not of type 'object' (at mcpServers)"]
    );
}

#[test]
fn mcp_schema_const_and_explicit_schema_check_both_report() {
    assert_eq!(
        mcp_errors(r#"{"$schema":"https://example.test/x","mcpServers":{}}"#),
        [
            format!("plugins/demo/mcp.json: '{MCP_ID}' was expected (at $schema)"),
            format!("plugins/demo/mcp.json: $schema must be {MCP_ID}"),
        ]
    );
}

#[test]
fn mcp_server_shape_violations_report_one_of_and_the_explicit_rule() {
    assert_eq!(
        server_errors("c", r#"{"type":"stdio"}"#),
        [
            format!(
                "{MCP}: {} (at mcpServers/c)",
                "{'type': 'stdio'} is not valid under any of the given schemas"
            ),
            format!("{MCP}: server 'c' command must be a string"),
        ]
    );
    assert_eq!(
        server_errors("s", "5"),
        [
            format!("{MCP}: 5 is not valid under any of the given schemas (at mcpServers/s)"),
            format!("{MCP}: server 's' must be an object"),
        ]
    );
}

#[test]
fn stdio_env_must_not_set_reserved_names() {
    assert_eq!(
        server_errors(
            "s",
            r#"{"type":"stdio","command":"node","env":{"PLUGIN_ROOT":"x"}}"#
        ),
        [
            format!(
                "{MCP}: {} (at mcpServers/s)",
                "{'type': 'stdio', 'command': 'node', 'env': {'PLUGIN_ROOT': 'x'}} is not valid under any of the given schemas"
            ),
            format!("{MCP}: server 's' env must not set PLUGIN_ROOT (spec §9.2)"),
        ]
    );
    assert_eq!(
        server_errors(
            "s",
            r#"{"type":"stdio","command":"node","env":{"PLUGIN_DATA":"x"}}"#
        )
        .last()
        .map(String::as_str),
        Some("plugins/demo/mcp.json: server 's' env must not set PLUGIN_DATA (spec §9.2)")
    );
    assert_eq!(
        server_errors(
            "s",
            r#"{"type":"stdio","command":"node","env":{"MODE":"x"}}"#
        ),
        Vec::<String>::new()
    );
    let not_an_object = server_errors("c", r#"{"type":"stdio","command":"node","env":[]}"#);
    assert_eq!(
        not_an_object.last().map(String::as_str),
        Some("plugins/demo/mcp.json: server 'c' env must be an object")
    );
    assert_eq!(not_an_object.len(), 2, "{not_an_object:?}");
}

#[test]
fn stdio_command_rules() {
    assert_eq!(
        server_errors("c", r#"{"type":"stdio","command":"${PLUGIN_ROOT}/bin"}"#),
        [
            format!("{MCP}: server 'c' command must not use placeholders (spec §7.2.1)"),
            format!("{MCP}: server 'c' command must be a bare name or './' path"),
        ]
    );
    assert_eq!(
        server_errors("c", r#"{"type":"stdio","command":"/usr/bin/tool"}"#),
        [format!(
            "{MCP}: server 'c' command must be a bare name or './' path"
        )]
    );
    assert_eq!(
        server_errors("c", r#"{"type":"stdio","command":"./x/../../y"}"#),
        [format!(
            "{MCP} server 'c' command: path escapes the plugin root ('./x/../../y')"
        )]
    );
    for good in ["node", "./bin/run", "./a/b"] {
        assert_eq!(
            server_errors("c", &format!(r#"{{"type":"stdio","command":"{good}"}}"#)),
            Vec::<String>::new(),
            "{good}"
        );
    }
}

#[test]
fn stdio_args_and_cwd_rules() {
    assert_eq!(
        server_errors(
            "c",
            r#"{"type":"stdio","command":"node","args":["${HOME}/x","${PLUGIN_ROOT}/ok"]}"#
        ),
        [format!(
            "{MCP}: server 'c' args uses unsupported placeholder ${{HOME}} (spec §9.2)"
        )]
    );
    assert_eq!(
        server_errors(
            "c",
            r#"{"type":"stdio","command":"node","args":["--flag","${PLUGIN_DATA}/ok","./sub/file","plain"]}"#
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        server_errors("c", r#"{"type":"stdio","command":"node","args":["../up"]}"#),
        [format!(
            "{MCP} server 'c' args: plugin-relative path must begin with './' (got '../up')"
        )]
    );
    assert_eq!(
        server_errors("c", r#"{"type":"stdio","command":"node","args":["/abs"]}"#),
        [format!(
            "{MCP} server 'c' args: plugin-relative path must begin with './' (got '/abs')"
        )]
    );
    assert_eq!(
        server_errors(
            "c",
            r#"{"type":"stdio","command":"node","args":["${PLUGIN_ROOT}/../up"]}"#
        ),
        [format!(
            "{MCP} server 'c' args: path escapes the plugin root ('./../up')"
        )]
    );
    assert_eq!(
        server_errors("c", r#"{"type":"stdio","command":"node","cwd":"./.."}"#),
        [format!(
            "{MCP} server 'c' cwd: path escapes the plugin root ('./..')"
        )]
    );
    assert_eq!(
        server_errors(
            "c",
            r#"{"type":"stdio","command":"node","cwd":"${PLUGIN_ROOT}"}"#
        ),
        Vec::<String>::new()
    );
}

#[test]
fn stdio_args_and_cwd_entries_must_be_strings() {
    let args = server_errors("c", r#"{"type":"stdio","command":"node","args":[1]}"#);
    assert_eq!(
        args.last().map(String::as_str),
        Some("plugins/demo/mcp.json: server 'c' args entries must be strings")
    );
    assert_eq!(args.len(), 2, "{args:?}");
    let cwd = server_errors("c", r#"{"type":"stdio","command":"node","cwd":5}"#);
    assert_eq!(
        cwd,
        [
            format!(
                "{MCP}: {} (at mcpServers/c)",
                "{'type': 'stdio', 'command': 'node', 'cwd': 5} is not valid under any of the given schemas"
            ),
            format!("{MCP}: server 'c' cwd entries must be strings"),
        ]
    );
}

#[test]
fn http_url_rules() {
    let url_errors = |url: &str| {
        server_errors(
            "h",
            &format!(r#"{{"type":"streamable-http","url":"{url}"}}"#),
        )
    };
    assert_eq!(
        url_errors("https://${HOST}/mcp"),
        [format!(
            "{MCP}: server 'h' url must not use expansion (spec §7.2.1)"
        )]
    );
    let userinfo = [
        "https://user:pw@example.test/mcp",
        "https://user@example.test/mcp",
        "https://:pw@example.test/mcp",
        "https://example.test/mcp#frag",
    ];
    for url in userinfo {
        assert_eq!(
            url_errors(url),
            [format!(
                "{MCP}: server 'h' url must not contain user info or a fragment (spec §7.2.1)"
            )],
            "{url}"
        );
    }
    assert_eq!(
        url_errors("http://example.test/mcp"),
        [format!(
            "{MCP}: server 'h' non-loopback url must use HTTPS (spec §7.2.1)"
        )]
    );
    assert_eq!(
        url_errors("https://[::1/x"),
        [format!(
            "{MCP}: server 'h' url is not a valid URL (Invalid IPv6 URL)"
        )]
    );
    let accepted = [
        "https://example.test/mcp",
        "https://example.test/mcp#",
        "https://@example.test/mcp",
        "http://127.0.0.1:9/m",
        "http://localhost/",
        "HTTP://LOCALHOST/x",
        "http://[::1]:8080/x",
        "ftp://localhost/x",
    ];
    for url in accepted {
        assert_eq!(url_errors(url), Vec::<String>::new(), "{url}");
    }
}

#[test]
fn http_url_must_be_a_string() {
    let errors = server_errors("s", r#"{"type":"sse","url":5}"#);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert_eq!(errors[1], format!("{MCP}: server 's' url must be a string"));
}

#[test]
fn http_header_rules() {
    let header_errors = |headers: &str| {
        server_errors(
            "h",
            &format!(r#"{{"type":"sse","url":"https://example.test/s","headers":{headers}}}"#),
        )
    };
    assert_eq!(
        header_errors(r#"{"X-A":"v","x-a":"w"}"#),
        [format!(
            "{MCP}: server 'h' has duplicate header names under different casing"
        )]
    );
    assert_eq!(
        header_errors(r#"{"Authorization":"Bearer x"}"#),
        [format!(
            "{MCP}: server 'h' must not embed credentials in headers (spec §7.2.1)"
        )]
    );
    assert_eq!(
        header_errors(r#"{"authorization":"${TOKEN}"}"#),
        [
            format!(
                "{MCP}: server 'h' header 'authorization' must not use expansion (spec §7.2.1)"
            ),
            format!("{MCP}: server 'h' must not embed credentials in headers (spec §7.2.1)"),
        ]
    );
    assert_eq!(
        header_errors(r#"{"X-Token":"${TOKEN}"}"#),
        [format!(
            "{MCP}: server 'h' header 'X-Token' must not use expansion (spec §7.2.1)"
        )]
    );
    let not_object = header_errors("[]");
    assert_eq!(not_object.len(), 2, "{not_object:?}");
    assert_eq!(
        not_object[1],
        format!("{MCP}: server 'h' headers must be an object")
    );
    assert_eq!(
        header_errors(r#"{"X-A":"v","X-B":"w"}"#),
        Vec::<String>::new()
    );
}

#[test]
fn skills_that_is_not_a_directory_is_reported() {
    let errors = repo_errors(|root| {
        fs::remove_dir_all(root.join("plugins/demo/skills")).expect("remove skills");
        write(root, "plugins/demo/skills", "not a directory");
    });
    assert_eq!(
        errors,
        ["plugins/demo: skills exists but is not a directory (spec §6.2)"]
    );
}

// ---- skills --------------------------------------------------------------------

#[test]
fn unreadable_skill_files_are_findings() {
    assert_eq!(
        skill_errors("demo-skill", &[0xff, 0xfe, 0x00]),
        [format!(
            "{}: cannot read as UTF-8: not valid UTF-8",
            skill_rel("demo-skill")
        )]
    );
}

#[test]
fn frontmatter_must_exist_and_be_terminated() {
    for content in [
        "Body only\n",
        "---\nname: x\n",
        "---\nname: x\n---",
        "---\n---\n",
        "--- \nname: x\n---\n",
    ] {
        assert_eq!(
            skill_errors("demo-skill", content.as_bytes()),
            [format!(
                "{}: missing or unterminated YAML frontmatter",
                skill_rel("demo-skill")
            )],
            "{content:?}"
        );
    }
}

#[test]
fn invalid_yaml_and_non_mappings_are_reported() {
    let errors = frontmatter_errors("demo-skill", "\tname: demo-skill\n");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with(&format!(
            "{}: invalid YAML frontmatter: ",
            skill_rel("demo-skill")
        )) && errors[0].ends_with("(line 2 of the file)"),
        "{errors:?}"
    );
    for body in ["- a\n- b\n", "\n"] {
        assert_eq!(
            frontmatter_errors("demo-skill", body),
            [format!(
                "{}: invalid YAML frontmatter: frontmatter is not a mapping",
                skill_rel("demo-skill")
            )],
            "{body:?}"
        );
    }
}

#[test]
fn only_spec_fields_are_allowed() {
    let body = format!(
        "{}version: 2\nauthor: x\n",
        fields("demo-skill", DESCRIPTION)
    );
    assert_eq!(
        frontmatter_errors("demo-skill", &body),
        [format!(
            "{}: non-spec frontmatter field(s): author, version",
            skill_rel("demo-skill")
        )]
    );
}

#[test]
fn name_rules() {
    let cases: [(&str, &str, &[&str]); 12] = [
        (
            "demo-skill",
            "Demo-Skill",
            &[
                "name must be lowercase",
                "name 'Demo-Skill' != directory 'demo-skill'",
            ],
        ),
        (
            "-demo",
            "-demo",
            &["name must not start or end with a hyphen"],
        ),
        (
            "demo-",
            "demo-",
            &["name must not start or end with a hyphen"],
        ),
        (
            "de--mo",
            "de--mo",
            &["name must not contain consecutive hyphens"],
        ),
        ("de_mo", "de_mo", &["name has invalid characters"]),
        ("demo skill", "demo skill", &["name has invalid characters"]),
        (
            "demo-skill",
            "other",
            &["name 'other' != directory 'demo-skill'"],
        ),
        ("demo-skill", "demo-skill", &[]),
        // Fullwidth forms fold to ASCII before comparing, as under NFKC.
        ("demo", "ｄｅｍｏ", &[]),
        ("123", "123", &[]),
        ("a", "a", &[]),
        ("demo", "ａ", &["name 'ａ' != directory 'demo'"]),
    ];
    for (dir, name, expected) in cases {
        let body = fields(&format!("\"{name}\""), DESCRIPTION);
        let want: Vec<String> = expected
            .iter()
            .map(|suffix| format!("{}: {suffix}", skill_rel(dir)))
            .collect();
        assert_eq!(frontmatter_errors(dir, &body), want, "{dir} / {name}");
    }
}

#[test]
fn name_length_is_bounded_in_characters() {
    let long = "a".repeat(65);
    assert_eq!(
        frontmatter_errors(&long, &fields(&long, DESCRIPTION)),
        [format!("{}: name exceeds 64 chars", skill_rel(&long))]
    );
    let limit = "a".repeat(64);
    assert_eq!(
        frontmatter_errors(&limit, &fields(&limit, DESCRIPTION)),
        Vec::<String>::new()
    );
}

#[test]
fn name_and_description_are_required() {
    let rel = skill_rel("demo-skill");
    assert_eq!(
        frontmatter_errors("demo-skill", &format!("description: {DESCRIPTION}\n")),
        [format!("{rel}: missing required 'name'")]
    );
    for empty in ["name:\n", "name: \"\"\n", "name: 0\n"] {
        assert_eq!(
            frontmatter_errors(
                "demo-skill",
                &format!("{empty}description: {DESCRIPTION}\n")
            ),
            [format!("{rel}: missing required 'name'")],
            "{empty:?}"
        );
    }
    assert_eq!(
        frontmatter_errors("demo-skill", "name: demo-skill\n"),
        [format!("{rel}: missing required 'description'")]
    );
    assert_eq!(
        frontmatter_errors("demo-skill", "license: MIT\n"),
        [
            format!("{rel}: missing required 'name'"),
            format!("{rel}: missing required 'description'"),
        ]
    );
}

#[test]
fn description_length_rules() {
    let rel = skill_rel("demo-skill");
    let quoted = |text: &str| format!("name: demo-skill\ndescription: \"{text}\"\n");
    assert_eq!(
        frontmatter_errors("demo-skill", &quoted(&"x".repeat(1025))),
        [format!(
            "{rel}: description is 1025 chars, over the 1024 limit"
        )]
    );
    assert_eq!(
        frontmatter_errors("demo-skill", &quoted(&"x".repeat(1024))),
        Vec::<String>::new()
    );
    assert_eq!(
        frontmatter_errors("demo-skill", &quoted("   ")),
        [format!("{rel}: missing required 'description'")]
    );
    assert_eq!(
        frontmatter_errors(
            "demo-skill",
            "name: demo-skill\ndescription: >\n  A folded description\n  across two lines.\n"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn compatibility_and_allowed_tools_rules() {
    let rel = skill_rel("demo-skill");
    let with = |extra: &str| {
        frontmatter_errors(
            "demo-skill",
            &format!("{}{extra}", fields("demo-skill", DESCRIPTION)),
        )
    };
    assert_eq!(
        with(&format!("compatibility: \"{}\"\n", "c".repeat(501))),
        [format!("{rel}: compatibility exceeds 500 chars")]
    );
    assert_eq!(
        with(&format!("compatibility: \"{}\"\n", "c".repeat(500))),
        Vec::<String>::new()
    );
    assert_eq!(
        with("allowed-tools:\n  - Read\n  - Grep\n"),
        [format!(
            "{rel}: allowed-tools must be a space-separated string, not a sequence"
        )]
    );
    assert_eq!(with("allowed-tools: Read Grep\n"), Vec::<String>::new());
    assert_eq!(with("allowed-tools:\n"), Vec::<String>::new());
}

#[test]
fn crlf_skill_files_are_accepted() {
    let content = skill_text("demo-skill").replace('\n', "\r\n");
    assert_eq!(
        skill_errors("demo-skill", content.as_bytes()),
        Vec::<String>::new()
    );
}

#[test]
fn only_direct_children_with_a_skill_file_are_validated_in_sorted_order() {
    let errors = repo_errors(|root| {
        write(root, "plugins/demo/skills/zz-skill/SKILL.md", "bad\n");
        write(root, "plugins/demo/skills/aa-skill/SKILL.md", "bad\n");
        write(
            root,
            "plugins/demo/skills/demo-skill/nested/SKILL.md",
            "bad\n",
        );
        fs::create_dir_all(root.join("plugins/demo/skills/empty-dir")).expect("empty dir");
        write(root, "plugins/demo/skills/loose-file.md", "bad\n");
    });
    assert_eq!(
        errors,
        [
            format!(
                "{}: missing or unterminated YAML frontmatter",
                skill_rel("aa-skill")
            ),
            format!(
                "{}: missing or unterminated YAML frontmatter",
                skill_rel("zz-skill")
            ),
        ]
    );
}

// ---- tree walk, ordering, runtime failures ---------------------------------------

#[test]
fn plugins_are_visited_in_sorted_order_and_files_are_skipped() {
    let root = TempDir::new("order");
    write_valid_repo(&root);
    fs::create_dir_all(root.join("plugins/b-plugin")).expect("b-plugin");
    fs::create_dir_all(root.join("plugins/a-plugin")).expect("a-plugin");
    write(&root, "plugins/notes.txt", "not a plugin");
    let report = validate_tree(&root).expect("validation runs");
    assert_eq!(
        report.progress(),
        vec![
            "Validating plugin: plugins/a-plugin",
            "Validating plugin: plugins/b-plugin",
            "Validating plugin: plugins/demo",
            "Validating skills",
        ]
    );
    assert_eq!(
        report.errors(),
        vec![
            "plugins/a-plugin: missing required root plugin.json (spec §5.1)",
            "plugins/b-plugin: missing required root plugin.json (spec §5.1)",
        ]
    );
}

#[test]
fn one_broken_plugin_does_not_stop_the_others() {
    let errors = repo_errors(|root| {
        write(root, "plugins/aaa/plugin.json", "{");
        write(root, "plugins/zzz/plugin.json", "[]");
    });
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].starts_with("plugins/aaa/plugin.json: invalid JSON"));
    assert_eq!(
        errors[1],
        "plugins/zzz/plugin.json: [] is not of type 'object' (at <root>)"
    );
}

#[test]
fn unusable_roots_and_schemas_are_runtime_errors() {
    let code_of = |root: &Path| {
        let error = validate_tree(root).expect_err("runtime failure");
        assert_eq!(error.exit_code(), 1);
        (error.code(), error.message().to_string())
    };

    assert_eq!(
        code_of(Path::new("/definitely/not/here/spec-root")).0,
        "root_not_found"
    );

    let file_root = TempDir::new("file-root");
    write(&file_root, "file", "x");
    assert_eq!(code_of(&file_root.join("file")).0, "root_not_directory");

    let no_schema = TempDir::new("no-schema");
    fs::create_dir_all(no_schema.join("plugins")).expect("plugins");
    let (code, message) = code_of(&no_schema);
    assert_eq!(code, "schema_unreadable");
    assert!(
        message.contains("tools/schemas/1.0.0/plugin.schema.json"),
        "{message}"
    );

    let no_plugins = TempDir::new("no-plugins");
    copy_schemas(&no_plugins);
    assert_eq!(code_of(&no_plugins).0, "plugins_not_found");

    let bad_json = TempDir::new("bad-schema-json");
    copy_schemas(&bad_json);
    write(&bad_json, "tools/schemas/1.0.0/mcp.schema.json", "{");
    assert_eq!(code_of(&bad_json).0, "schema_invalid");

    let unsupported = TempDir::new("unsupported-schema");
    copy_schemas(&unsupported);
    write(
        &unsupported,
        "tools/schemas/1.0.0/plugin.schema.json",
        r#"{"$ref":"https://example.test/x"}"#,
    );
    let (code, message) = code_of(&unsupported);
    assert_eq!(code, "schema_unsupported");
    assert!(message.contains("not local"), "{message}");
}

#[test]
fn a_repository_without_plugin_directories_passes_the_walk() {
    let root = TempDir::new("empty-plugins");
    copy_schemas(&root);
    fs::create_dir_all(root.join("plugins")).expect("plugins");
    let report = validate_tree(&root).expect("validation runs");
    assert_eq!(report.errors(), Vec::<&str>::new());
    assert_eq!(report.progress(), vec!["Validating skills"]);
}

// ---- command output and exit codes -------------------------------------------------

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

#[test]
fn command_success_prints_progress_then_the_pass_line() {
    let root = TempDir::new("cmd-ok");
    write_valid_repo(&root);
    let (status, stdout, stderr) = exec(&["spec", "validate", root.to_str().expect("utf-8")]);
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        "Validating plugin: plugins/demo\nValidating skills\n\nAll Agent Plugins v1.0.0 checks passed.\n"
    );
}

#[test]
fn command_failure_reports_errors_on_stderr_and_exits_one() {
    let root = TempDir::new("cmd-fail");
    write_valid_repo(&root);
    write(
        &root,
        "plugins/demo/skills/demo-skill/SKILL.md",
        "no frontmatter\n",
    );
    write(&root, MANIFEST, "[]");
    let (status, stdout, stderr) = exec(&["spec", "validate", root.to_str().expect("utf-8")]);
    assert_eq!(status, 1);
    assert_eq!(
        stdout,
        "Validating plugin: plugins/demo\nValidating skills\n"
    );
    assert_eq!(
        stderr,
        "  ERROR: plugins/demo/plugin.json: [] is not of type 'object' (at <root>)\n\
         \x20 ERROR: plugins/demo/skills/demo-skill/SKILL.md: missing or unterminated YAML frontmatter\n\
         error: Spec validation failed with 2 error(s).\n"
    );
}

#[test]
fn command_runtime_errors_exit_one_with_the_shared_report() {
    let (status, stdout, stderr) = exec(&["spec", "validate", "/definitely/not/here/spec-root"]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "error: root does not exist: /definitely/not/here/spec-root\n"
    );
}

#[test]
fn command_usage_errors_exit_two() {
    for args in [
        vec!["spec"],
        vec!["spec", "check"],
        vec!["spec", "validate", "one", "two"],
        vec!["spec", "validate", "--plugin", "x"],
        vec!["spec", "validate", "--json"],
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
fn help_documents_the_command_and_it_is_registered() {
    let (status, stdout, stderr) = exec(&["spec", "validate", "--help"]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.contains("toolkit spec validate [ROOT]"));
    assert!(COMMANDS
        .iter()
        .any(|spec| spec.path == ["spec", "validate"] && spec.max_positionals == 1));
}

// ---- real repository ---------------------------------------------------------------

#[test]
fn real_repository_tree_passes() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = validate_tree(&repo).expect("real tree validates");
    assert_eq!(
        report.errors(),
        Vec::<&str>::new(),
        "genuine findings in the repository"
    );
    let progress = report.progress();
    assert!(progress.contains(&"Validating plugin: plugins/aws-core"));
    assert!(progress.contains(&"Validating skills"));
    assert!(progress.len() >= 6, "{progress:?}");
}

// ---- helpers -----------------------------------------------------------------------

#[test]
fn placeholders_are_found_like_the_original_regular_expression() {
    assert_eq!(find_placeholders("${A}-${B}"), ["A", "B"]);
    assert_eq!(find_placeholders("${}"), [""]);
    assert_eq!(find_placeholders("a${x${y}"), ["x${y"]);
    assert!(find_placeholders("${A").is_empty());
    assert!(find_placeholders("$ {A}").is_empty());
    assert!(find_placeholders("plain").is_empty());
}

#[test]
fn path_like_values_follow_the_original_prefixes() {
    for yes in [
        "./x",
        "../x",
        "/x",
        "\\x",
        "${PLUGIN_ROOT}",
        "${PLUGIN_DATA}/x",
        "a\\b",
    ] {
        assert!(looks_like_path(yes), "{yes}");
    }
    for no in ["plain", "a/b", "--flag", "${HOME}/x", ".hidden", ""] {
        assert!(!looks_like_path(no), "{no}");
    }
}

#[test]
fn frontmatter_block_is_found_like_the_original() {
    assert!(matches!(parse_frontmatter("x"), Frontmatter::Missing));
    assert!(matches!(
        parse_frontmatter("---\n---\n"),
        Frontmatter::Missing
    ));
    assert!(matches!(
        parse_frontmatter("---\nname: a\n---\nbody"),
        Frontmatter::Mapping(ref members) if members.len() == 1
    ));
    // The first closing fence wins; later fences belong to the body.
    assert!(matches!(
        parse_frontmatter("---\nname: a\n---\nbody\n---\nmore\n"),
        Frontmatter::Mapping(ref members) if members.len() == 1
    ));
}

#[test]
fn urls_split_like_urlsplit() {
    let parts = |scheme: &str, fragment: &str, credentials: bool, host: Option<&str>| UrlParts {
        scheme: scheme.to_string(),
        fragment: fragment.to_string(),
        has_credentials: credentials,
        hostname: host.map(str::to_string),
    };
    let cases = [
        (
            "https://example.test/mcp",
            parts("https", "", false, Some("example.test")),
        ),
        (
            "HTTP://LocalHost:80/x#f",
            parts("http", "f", false, Some("localhost")),
        ),
        ("https://u:p@host/x", parts("https", "", true, Some("host"))),
        ("https://@host/x", parts("https", "", false, Some("host"))),
        ("http://[::1]:8080/x", parts("http", "", false, Some("::1"))),
        ("//host/x", parts("", "", false, Some("host"))),
        ("/path", parts("", "", false, None)),
        ("mailto:a@b", parts("mailto", "", false, None)),
        ("  https://x/\n", parts("https", "", false, Some("x"))),
        (
            "https://host?q=1#frag",
            parts("https", "frag", false, Some("host")),
        ),
    ];
    for (url, expected) in cases {
        assert_eq!(split_url(url), Ok(expected), "{url:?}");
    }
    assert!(split_url("https://[::1/x").is_err());
    assert!(split_url("https://::1]/x").is_err());
}

// ---- exact NFKC (B4) -----------------------------------------------------------------

#[test]
fn nfkc_is_exact_unicode_normalization() {
    // Fullwidth and ideographic space fold as before.
    assert_eq!(nfkc("ＡＢＣ１２３－"), "ABC123-");
    assert_eq!(nfkc("a\u{3000}b"), "a b");
    // Canonical composition: e + COMBINING ACUTE ACCENT becomes U+00E9.
    assert_eq!(nfkc("e\u{301}"), "\u{e9}");
    // Compatibility expansion: LATIN SMALL LIGATURE FF becomes `ff`.
    assert_eq!(nfkc("\u{fb00}"), "ff");
    assert_eq!(nfkc("\u{fb03}"), "ffi");
    assert_eq!(nfkc("\u{2460}"), "1");
    assert_eq!(nfkc("plain-é"), "plain-é");
}

#[test]
fn skill_names_compare_after_composition_and_expansion() {
    let accepted = [
        // decomposed name, precomposed directory
        ("\u{e9}", "e\u{301}"),
        // precomposed name, decomposed directory
        ("e\u{301}", "\u{e9}"),
        // ligature in the name, expanded directory
        ("ff", "\u{fb00}"),
        ("\u{fb00}", "ff"),
        ("\u{fb03}-x", "ffi-x"),
    ];
    for (dir, name) in accepted {
        let body = fields(&format!("\"{name}\""), DESCRIPTION);
        assert_eq!(
            frontmatter_errors(dir, &body),
            Vec::<String>::new(),
            "{dir:?} / {name:?}"
        );
    }
    // Still different after normalization.
    let body = fields("\"e\u{301}\"", DESCRIPTION);
    assert_eq!(
        frontmatter_errors("e", &body),
        [format!(
            "{}: name 'e\u{301}' != directory 'e'",
            skill_rel("e")
        )]
    );
    // An expansion that is uppercase is reported on the normalized text.
    let body = fields("\"\u{ff21}\u{ff22}\"", DESCRIPTION);
    assert_eq!(
        frontmatter_errors("ab", &body),
        [
            format!("{}: name must be lowercase", skill_rel("ab")),
            format!(
                "{}: name '\u{ff21}\u{ff22}' != directory 'ab'",
                skill_rel("ab")
            ),
        ]
    );
}

#[test]
fn skill_name_length_counts_normalized_characters() {
    // 64 decomposed letters are 128 code points but 64 after composition.
    let decomposed = "e\u{301}".repeat(64);
    let composed = "\u{e9}".repeat(64);
    assert_eq!(
        frontmatter_errors(
            &composed,
            &fields(&format!("\"{decomposed}\""), DESCRIPTION)
        ),
        Vec::<String>::new()
    );
    // 65 of them exceed the limit once normalized.
    let long = "e\u{301}".repeat(65);
    let long_dir = "\u{e9}".repeat(65);
    assert_eq!(
        frontmatter_errors(&long_dir, &fields(&format!("\"{long}\""), DESCRIPTION)),
        [format!("{}: name exceeds 64 chars", skill_rel(&long_dir))]
    );
    // 32 ligatures expand to exactly 64 letters; 33 expand to 66.
    let ligatures = "\u{fb00}".repeat(32);
    let expanded = "f".repeat(64);
    assert_eq!(
        frontmatter_errors(&expanded, &fields(&format!("\"{ligatures}\""), DESCRIPTION)),
        Vec::<String>::new()
    );
    let too_many = "\u{fb00}".repeat(33);
    let too_many_dir = "f".repeat(66);
    assert_eq!(
        frontmatter_errors(
            &too_many_dir,
            &fields(&format!("\"{too_many}\""), DESCRIPTION)
        ),
        [format!(
            "{}: name exceeds 64 chars",
            skill_rel(&too_many_dir)
        )]
    );
}

// ---- float zero is falsy (B3) --------------------------------------------------------

#[test]
fn float_zero_is_a_missing_required_field() {
    let rel = skill_rel("demo-skill");
    for zero in ["0.0", "-0.0", "0"] {
        assert_eq!(
            frontmatter_errors(
                "demo-skill",
                &format!("name: demo-skill\ndescription: {zero}\n")
            ),
            [format!("{rel}: missing required 'description'")],
            "description: {zero}"
        );
        assert_eq!(
            frontmatter_errors("0", &format!("name: {zero}\ndescription: {DESCRIPTION}\n")),
            [format!("{}: missing required 'name'", skill_rel("0"))],
            "name: {zero}"
        );
    }
}

#[test]
fn quoted_and_nonzero_floats_are_text() {
    let rel = skill_rel("demo-skill");
    // A quoted "0.0" is a non-empty string, not zero.
    assert_eq!(
        frontmatter_errors("demo-skill", "name: demo-skill\ndescription: \"0.0\"\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        frontmatter_errors("demo-skill", "name: demo-skill\ndescription: 0.5\n"),
        Vec::<String>::new()
    );
    // A float name is its Python text (`1.0`), not `1`.
    assert_eq!(
        frontmatter_errors("1", "name: 1.0\ndescription: Fine description here.\n"),
        [
            format!("{}: name has invalid characters", skill_rel("1")),
            format!("{}: name '1.0' != directory '1'", skill_rel("1")),
        ]
    );
    // The same name written as a string reports the same way.
    assert_eq!(
        frontmatter_errors(
            "demo-skill",
            &format!("name: \"0.0\"\ndescription: {DESCRIPTION}\n")
        ),
        [
            format!("{rel}: name has invalid characters"),
            format!("{rel}: name '0.0' != directory 'demo-skill'"),
        ]
    );
}

// ---- URL splitting (B5) --------------------------------------------------------------

#[test]
fn bracketed_hosts_must_be_ipv6_literals() {
    let valid = [
        "http://[::1]/x",
        "http://[::1]:80/x",
        "https://[2001:db8::1]/x",
        "http://[fe80::1%eth0]/x",
        "http://[::ffff:192.0.2.1]/x",
        "http://[v1.fe80]/x",
    ];
    for url in valid {
        assert!(split_url(url).is_ok(), "{url}: {:?}", split_url(url));
    }
    let invalid = [
        (
            "http://[localhost]/mcp",
            "'localhost' does not appear to be an IPv4 or IPv6 address",
        ),
        (
            "http://[127.0.0.1]/mcp",
            "An IPv4 address cannot be in brackets",
        ),
        (
            "http://[]/mcp",
            "'' does not appear to be an IPv4 or IPv6 address",
        ),
        ("http://[::1/mcp", "Invalid IPv6 URL"),
        ("http://::1]/mcp", "Invalid IPv6 URL"),
        (
            "http://[::1%]/mcp",
            "'::1%' does not appear to be an IPv4 or IPv6 address",
        ),
        (
            "http://[gggg::1]/mcp",
            "'gggg::1' does not appear to be an IPv4 or IPv6 address",
        ),
        (
            "http://[1:2:3:4:5:6:7:8:9]/mcp",
            "'1:2:3:4:5:6:7:8:9' does not appear to be an IPv4 or IPv6 address",
        ),
        ("http://[v]/mcp", "IPvFuture address is invalid"),
        ("http://[vx.y]/mcp", "IPvFuture address is invalid"),
        ("http://[v1.]/mcp", "IPvFuture address is invalid"),
    ];
    for (url, reason) in invalid {
        assert_eq!(split_url(url), Err(reason.to_string()), "{url}");
    }
}

#[test]
fn bracketed_non_ip_hosts_never_get_the_loopback_exception() {
    let url_errors = |url: &str| server_errors("h", &format!(r#"{{"type":"sse","url":"{url}"}}"#));
    assert_eq!(
        url_errors("http://[localhost]/mcp"),
        [format!(
            "{MCP}: server 'h' url is not a valid URL ('localhost' does not appear to be an IPv4 or IPv6 address)"
        )]
    );
    assert_eq!(
        url_errors("http://[127.0.0.1]/mcp"),
        [format!(
            "{MCP}: server 'h' url is not a valid URL (An IPv4 address cannot be in brackets)"
        )]
    );
    assert_eq!(url_errors("http://[::1]:8080/mcp"), Vec::<String>::new());
    assert_eq!(url_errors("http://127.0.0.1:9/mcp"), Vec::<String>::new());
}

#[test]
fn netlocs_are_checked_under_nfkc() {
    // Characters that normalize to a URL delimiter hide a different authority
    // from the splitter: U+FF0F becomes `/`, U+FF1F `?`, U+FF03 `#`, U+FE55 `:`.
    for netloc in [
        "example.test\u{ff0f}evil",
        "example.test\u{ff1f}q",
        "example.test\u{ff03}f",
        "host\u{fe55}80",
    ] {
        assert_eq!(
            split_url(&format!("https://{netloc}")).map(|_| ()),
            Err(format!(
                "netloc '{netloc}' contains invalid characters under NFKC normalization"
            )),
            "{netloc:?}"
        );
    }
    // Non-ASCII netlocs that do not gain a delimiter are fine.
    assert!(split_url("https://b\u{fc}cher.example/").is_ok());
    assert!(split_url("https://\u{ff45}xample.test/").is_ok());
    // ASCII netlocs are never normalized.
    assert!(split_url("https://example.test:8080/x").is_ok());
}

#[test]
fn nfkc_delimiter_netlocs_are_findings_in_the_command_not_passes() {
    let errors = server_errors(
        "h",
        "{\"type\":\"streamable-http\",\"url\":\"https://example.test\u{ff0f}evil\"}",
    );
    assert_eq!(
        errors,
        [format!(
            "{MCP}: server 'h' url is not a valid URL (netloc 'example.test\u{ff0f}evil' contains invalid characters under NFKC normalization)"
        )]
    );
}

// ---- schema engine: fail-closed compilation and bounded evaluation (B1, B2) --------------

fn json(text: &str) -> toolkit_core::Json {
    toolkit_core::parse_json(text).expect("fixture JSON parses")
}

fn compile_error(schema: &str) -> String {
    match engine::compile(json(schema)) {
        Ok(_) => panic!("schema was accepted: {schema}"),
        Err(reason) => reason,
    }
}

fn evaluate(schema: &str, instance: &str) -> Result<Vec<String>, String> {
    let compiled = engine::compile(json(schema)).expect("schema compiles");
    compiled.validate(&json(instance)).map(|issues| {
        issues
            .into_iter()
            .map(|issue| format!("{} (at {})", issue.message, issue.path_text()))
            .collect()
    })
}

#[test]
fn external_references_are_rejected_even_under_combinators() {
    for schema in [
        r#"{"$ref":"https://example.test/schema"}"#,
        r#"{"not":{"$ref":"https://example.test/schema"}}"#,
        r#"{"oneOf":[{"$ref":"other.json"},{"type":"string"}]}"#,
        r#"{"properties":{"a":{"$ref":"http://localhost:1/x#/definitions/y"}}}"#,
        r#"{"$ref":"file:///etc/passwd"}"#,
        r#"{"$ref":"https://json-schema.org/draft/2020-12/schema"}"#,
    ] {
        let reason = compile_error(schema);
        assert!(reason.contains("not local"), "{schema}: {reason}");
    }
}

#[test]
fn missing_non_schema_and_unsupported_references_are_rejected() {
    let reason = compile_error(r##"{"not":{"$ref":"#/$defs/missing"}}"##);
    assert!(reason.contains("does not resolve"), "{reason}");
    let reason = compile_error(r##"{"$ref":"#/description","description":"not a schema"}"##);
    assert!(reason.contains("not a schema"), "{reason}");
    let reason = compile_error(r##"{"$ref":"#anchor"}"##);
    assert!(reason.contains("does not resolve"), "{reason}");
    let reason = compile_error(r##"{"$defs":{"a b":{}},"$ref":"#/$defs/a%20b"}"##);
    assert!(reason.contains("percent-encoding"), "{reason}");
    let reason = compile_error(r##"{"$dynamicRef":"#x"}"##);
    assert!(reason.contains("dynamic references"), "{reason}");
    let reason = compile_error(r#"{"$defs":{"a":{"$id":"https://example.test/a"}}}"#);
    assert!(reason.contains("nested '$id'"), "{reason}");
}

#[test]
fn recursive_references_are_rejected() {
    for schema in [
        r##"{"$ref":"#"}"##,
        r##"{"properties":{"child":{"$ref":"#"}}}"##,
        r##"{"$defs":{"a":{"$ref":"#/$defs/a"}},"$ref":"#/$defs/a"}"##,
        r##"{"$defs":{"a":{"$ref":"#/$defs/b"},"b":{"items":{"$ref":"#/$defs/a"}}},"$ref":"#/$defs/a"}"##,
    ] {
        let reason = compile_error(schema);
        assert!(reason.contains("recursive"), "{schema}: {reason}");
    }
    // A shared (non-recursive) definition reached twice is fine.
    let schema = r##"{"$defs":{"s":{"type":"string"}},"properties":{"a":{"$ref":"#/$defs/s"},"b":{"$ref":"#/$defs/s"}}}"##;
    assert_eq!(
        evaluate(schema, r#"{"a":"x","b":1}"#),
        Ok(vec!["1 is not of type 'string' (at b)".to_string()])
    );
}

#[test]
fn invalid_regular_expressions_are_rejected() {
    // Python refuses "nothing to repeat"; fancy-regex alone would accept it.
    for pattern in ["^*", "$+", "\\\\b?", "a|^{2}", "^{1,3}", "(^)*x$*"] {
        let schema = format!(r#"{{"type":"string","pattern":"{pattern}"}}"#);
        let outcome = engine::compile(json(&schema));
        if pattern == "(^)*x$*" {
            // A group before the quantifier is repeatable, an anchor is not.
            assert!(outcome.is_err(), "{pattern}");
        } else {
            let reason = outcome
                .err()
                .unwrap_or_else(|| panic!("{pattern} accepted"));
            assert!(reason.contains("nothing to repeat"), "{pattern}: {reason}");
        }
    }
    for pattern in ["(", "[a", "*a", "(?P<n"] {
        let schema = format!(r#"{{"type":"string","pattern":"{pattern}"}}"#);
        assert!(engine::compile(json(&schema)).is_err(), "{pattern}");
    }
    // Quantifiers elsewhere, lazy forms and literal braces stay valid.
    for pattern in [
        "^a*$",
        "^a+?$",
        "^(?:ab)*$",
        "^a{2,3}$",
        "^[*+?]$",
        "^\\\\^*$",
    ] {
        let schema = format!(r#"{{"type":"string","pattern":"{pattern}"}}"#);
        assert!(engine::compile(json(&schema)).is_ok(), "{pattern}");
    }
    // The same applies to patternProperties keys.
    let reason = compile_error(r#"{"patternProperties":{"^*":{}}}"#);
    assert!(reason.contains("nothing to repeat"), "{reason}");
}

#[test]
fn catastrophic_patterns_stay_bounded() {
    // Without lookaround or backreferences the pattern runs on the linear
    // engine, so nested quantifiers cannot blow up.
    let started = std::time::Instant::now();
    let input = format!("{}!", "a".repeat(40));
    let schema = r#"{"type":"string","pattern":"^(a+)+$"}"#;
    let outcome = evaluate(schema, &format!("\"{input}\""));
    assert_eq!(
        outcome,
        Ok(vec![format!(
            "'{input}' does not match '^(a+)+$' (at <root>)"
        )])
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn backtracking_exhaustion_is_an_evaluation_failure_not_a_match() {
    let hostile = format!("\"{}!\"", "a".repeat(60));
    let pattern = r"^(a|a)*\\1b";
    let started = std::time::Instant::now();
    // The pattern alone.
    let schema = format!(r#"{{"type":"string","pattern":"{pattern}"}}"#);
    let reason = evaluate(&schema, &hostile).expect_err("exhaustion is an error");
    assert!(reason.contains("regular expression"), "{reason}");
    // Under `not`: exhaustion must not read as a non-match and pass.
    let schema = format!(r#"{{"not":{{"pattern":"{pattern}"}}}}"#);
    assert!(evaluate(&schema, &hostile).is_err());
    // Under `oneOf`, where one alternative's outcome decides the result.
    let schema = format!(r#"{{"oneOf":[{{"pattern":"{pattern}"}},{{"type":"string"}}]}}"#);
    assert!(evaluate(&schema, &hostile).is_err());
    // Inside a negative lookahead: exhaustion must not satisfy it.
    let schema = format!(
        r#"{{"type":"string","pattern":"^(?!{})"}}"#,
        pattern.trim_start_matches('^')
    );
    assert!(evaluate(&schema, &hostile).is_err());
    // A property name checked by `propertyNames` is covered too.
    let schema = format!(r#"{{"propertyNames":{{"not":{{"pattern":"{pattern}"}}}}}}"#);
    let object = format!("{{{}:1}}", &hostile);
    assert!(evaluate(&schema, &object).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

#[test]
fn instance_violations_stay_distinct_from_evaluation_failures() {
    // A shared deterministic violation is reported even though the pattern
    // itself cannot be decided.
    let hostile = format!("\"{}!\"", "a".repeat(60));
    let schema = r#"{"type":"string","maxLength":5,"pattern":"^(a|a)*\\1b"}"#;
    let outcome = evaluate(schema, &hostile).expect("the length violation is decided");
    assert_eq!(outcome.len(), 1, "{outcome:?}");
    assert!(
        outcome[0].ends_with("is too long (at <root>)"),
        "{outcome:?}"
    );
    // And a pattern that decides normally is an ordinary finding or a pass.
    let schema = r#"{"type":"string","pattern":"^(?!.*--)[a-z]+$"}"#;
    assert_eq!(evaluate(schema, "\"abc\""), Ok(vec![]));
    assert_eq!(
        evaluate(schema, "\"a--b\""),
        Ok(vec![
            "'a--b' does not match '^(?!.*--)[a-z]+$' (at <root>)".to_string()
        ])
    );
}

#[test]
fn a_replacement_schema_cannot_turn_failures_into_a_pass() {
    // Command level: the vendored plugin schema is replaced by one that would
    // have let any plugin through if the unresolvable reference were ignored.
    let cases = [
        (
            r#"{"not":{"$ref":"https://example.test/schema"}}"#,
            "not local",
        ),
        (
            r##"{"not":{"$ref":"#/$defs/missing"}}"##,
            "does not resolve",
        ),
        (
            r##"{"$ref":"#/description","description":"x"}"##,
            "not a schema",
        ),
        (
            r#"{"type":"object","properties":{"name":{"pattern":"^*"}}}"#,
            "nothing to repeat",
        ),
    ];
    for (schema, expected) in cases {
        let root = TempDir::new("bad-replacement-schema");
        write_valid_repo(&root);
        write(&root, "tools/schemas/1.0.0/plugin.schema.json", schema);
        let error = validate_tree(&root).expect_err("the schema is refused");
        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.code(), "schema_unsupported");
        assert!(error.message().contains(expected), "{}", error.message());
    }
}

#[test]
fn exhaustion_while_validating_a_manifest_is_a_runtime_error() {
    let root = TempDir::new("exhausted-schema");
    write_valid_repo(&root);
    write(
        &root,
        "tools/schemas/1.0.0/plugin.schema.json",
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"description":{"not":{"pattern":"^(a|a)*\\1b"}}}}"#,
    );
    let hostile = format!("{}!", "a".repeat(60));
    write(
        &root,
        MANIFEST,
        &plugin_json("demo").replace("Demo plugin.", &hostile),
    );
    let error = validate_tree(&root).expect_err("evaluation fails");
    assert_eq!(error.exit_code(), 1);
    assert_eq!(error.code(), "schema_evaluation_failed");
    assert!(
        error
            .message()
            .contains("schema evaluation failed for plugins/demo/plugin.json"),
        "{}",
        error.message()
    );
}

#[test]
fn schema_messages_keep_the_published_wording() {
    let schema = r#"{
        "type":"object",
        "properties":{
            "n":{"type":["string","null"]},
            "c":{"const":{"b":1,"a":[true]}},
            "e":{"enum":["x","y"]},
            "s":{"type":"string","minLength":2,"maxLength":3},
            "t":{"minLength":1},
            "p":{"propertyNames":{"not":{"enum":["bad"]}}},
            "o":{"oneOf":[{"type":"integer"},{"minimum":0}]},
            "f":false
        },
        "required":["req"],
        "additionalProperties":false
    }"#;
    let outcome = evaluate(
        schema,
        r#"{"n":5,"c":{"a":[true],"b":2},"e":"z","s":"abcd","t":"","p":{"bad":1,"ok":2},"o":3,"f":1,"z2":1,"z1":2}"#,
    )
    .expect("violations only");
    assert_eq!(
        outcome,
        vec![
            "'req' is a required property (at <root>)",
            "Additional properties are not allowed ('z1', 'z2' were unexpected) (at <root>)",
            "{'b': 1, 'a': [True]} was expected (at c)",
            "'z' is not one of ['x', 'y'] (at e)",
            "False schema does not allow 1 (at f)",
            "5 is not of type 'string', 'null' (at n)",
            "3 is valid under each of {'minimum': 0}, {'type': 'integer'} (at o)",
            "'bad' should not be valid under {'enum': ['bad']} (at p)",
            "'abcd' is too long (at s)",
            "'' should be non-empty (at t)",
        ]
    );
}

#[test]
fn findings_at_one_path_follow_the_schema_keyword_order() {
    let schema = r#"{"type":"string","maxLength":1,"minLength":3,"pattern":"^z"}"#;
    assert_eq!(
        evaluate(schema, "\"ab\""),
        Ok(vec![
            "'ab' is too long (at <root>)".to_string(),
            "'ab' is too short (at <root>)".to_string(),
            "'ab' does not match '^z' (at <root>)".to_string(),
        ])
    );
    // Array indexes sort numerically before names; keys that look numeric in
    // an object stay keys.
    let schema = r#"{"properties":{"l":{"items":{"type":"string"}},"o":{"additionalProperties":{"type":"string"}}}}"#;
    assert_eq!(
        evaluate(schema, r#"{"l":[1,"a",2],"o":{"10":1,"2":2}}"#),
        Ok(vec![
            "1 is not of type 'string' (at l/0)".to_string(),
            "2 is not of type 'string' (at l/2)".to_string(),
            "1 is not of type 'string' (at o/10)".to_string(),
            "2 is not of type 'string' (at o/2)".to_string(),
        ])
    );
}

// ---- key order never depends on the serde_json map implementation (#5394) ----------
//
// `serde_json::Map` sorts its keys in a per-package build and keeps insertion
// order when another workspace member turns on `preserve_order`, so any text
// built from a `Value` object differs by build scope. These tests pin the
// scope-independent result: the first asserts the document's own order, the
// second that the `Value` handed to the validator iterates the same way in
// every build.

#[test]
fn unworded_keywords_render_the_instance_in_document_order() {
    assert_eq!(
        evaluate(r#"{"minProperties":3}"#, r#"{"b":1,"a":[{"d":1,"c":2}]}"#),
        Ok(vec![
            r#"{"b":1,"a":[{"d":1,"c":2}]} has less than 3 properties (at <root>)"#.to_string()
        ])
    );
    // Scalars have no order to keep; the engine's own sentence is unchanged.
    assert_eq!(
        evaluate(r#"{"properties":{"n":{"minimum":5}}}"#, r#"{"n":2}"#),
        Ok(vec!["2 is less than the minimum of 5 (at n)".to_string()])
    );
}

#[test]
fn values_given_to_the_validator_have_one_key_order_in_every_build() {
    let value = engine::to_value(&json(r#"{"b":1,"a":{"d":1,"c":2}}"#)).expect("converts");
    let keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["a", "b"]);
    let inner: Vec<&str> = value["a"]
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(inner, ["c", "d"]);
}
