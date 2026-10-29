//! Packaging contract (#4487), offline and without building any target:
//! every launcher path referenced by a plugin's `hooks.json`, `.mcp.json` or
//! `mcp.json` resolves to a binary declared in
//! `packaging/plugin-binaries.json`, with the launcher form its role
//! requires; and any staged `bin/` directory matches its manifest (presence,
//! executable bit, size, sha256, no unlisted files). Fixture plugins prove
//! each violation is caught.

mod common;

use common::{repo_root, temp_dir, write};
use plugin_stage::launcher::{check_launchers, guard_hook_command, mcp_command};
use plugin_stage::manifest::verify_staged;
use plugin_stage::{Declaration, DECLARATION_PATH};

fn repo_declaration() -> Declaration {
    Declaration::load(&repo_root().join(DECLARATION_PATH)).expect("committed declaration")
}

#[test]
fn every_repository_launcher_resolves_to_a_declared_binary() {
    let root = repo_root();
    let report = check_launchers(&root, &root.join("plugins"), &repo_declaration());
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    // The scan is not vacuous: every tracked hooks.json, .mcp.json and
    // mcp.json of the shipped plugins is read.
    assert!(
        report.files_scanned >= 12,
        "scanned {}",
        report.files_scanned
    );
}

#[test]
fn staged_repository_binaries_match_their_manifests() {
    let root = repo_root();
    let problems = verify_staged(&root, &root.join("plugins"), &repo_declaration(), false);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_secret_safety_guard_is_declared_fail_closed() {
    let declaration = repo_declaration();
    let guard = declaration
        .binary("aws-core", "secret-safety")
        .expect("aws-core/secret-safety is declared");
    assert_eq!(guard.package, "secret-safety-hook");
    assert_eq!(guard.bin, "secret-safety");
    let plugin_stage::Role::GuardHook { deny_reason } = &guard.role else {
        panic!("secret-safety must be a guard-hook, got {:?}", guard.role);
    };
    let command = guard_hook_command("CLAUDE_PLUGIN_ROOT", "secret-safety", deny_reason);
    assert!(command.contains(r#"\"permissionDecision\":\"deny\""#));
    assert!(command.ends_with(r#""${CLAUDE_PLUGIN_ROOT}/bin/secret-safety""#));
    let keys: Vec<&str> = declaration
        .platforms
        .iter()
        .map(|spec| spec.platform.key())
        .collect();
    assert_eq!(
        keys,
        [
            "linux-x86_64",
            "linux-aarch64",
            "macos-x86_64",
            "macos-aarch64",
            "windows-x86_64"
        ]
    );
}

const FIXTURE_DECLARATION: &str = r##"{
  "schema": 1,
  "platforms": [{"platform": "linux-x86_64", "triple": "x86_64-unknown-linux-gnu"}],
  "binaries": [
    {"plugin": "demo", "name": "guard", "package": "g", "bin": "guard",
     "role": "guard-hook", "deny_reason": "Guard unavailable. Reinstall demo.", "consumer": "#1"},
    {"plugin": "demo", "name": "tool", "package": "t", "bin": "tool", "role": "mcp-server", "consumer": "#2"},
    {"plugin": "demo", "name": "note", "package": "n", "bin": "note", "role": "hook", "consumer": "#3"}
  ]
}"##;

fn hooks(event: &str, command: &str) -> String {
    let command = toolkit_core::escape_json_string(command);
    format!(
        r#"{{"hooks": {{"{event}": [{{"matcher": "Bash", "hooks": [{{"type": "command", "command": "{command}", "timeout": 5}}]}}]}}}}"#
    )
}

fn mcp(command: &str, args: &str) -> String {
    let command = toolkit_core::escape_json_string(command);
    format!(
        r#"{{"mcpServers": {{"s": {{"type": "stdio", "command": "{command}", "args": {args}}}}}}}"#
    )
}

/// Checks one fixture plugin `demo` holding `files` (relative path, text).
fn problems_for(files: &[(&str, String)]) -> (Vec<String>, usize) {
    let dir = temp_dir("contract");
    let plugins = dir.join("plugins");
    write(&plugins.join("demo/README.md"), "demo\n");
    for (path, text) in files {
        write(&plugins.join("demo").join(path), text);
    }
    let declaration = Declaration::parse(FIXTURE_DECLARATION).expect("fixture declaration");
    let report = check_launchers(&dir, &plugins, &declaration);
    (report.problems, report.references.len())
}

fn guard() -> String {
    guard_hook_command(
        "CLAUDE_PLUGIN_ROOT",
        "guard",
        "Guard unavailable. Reinstall demo.",
    )
}

#[test]
fn canonical_launchers_pass() {
    let (problems, references) = problems_for(&[
        ("hooks/hooks.json", hooks("PreToolUse", &guard())),
        (
            ".mcp.json",
            mcp(&mcp_command("CLAUDE_PLUGIN_ROOT", "tool"), r#"["serve"]"#),
        ),
        ("mcp.json", mcp("./bin/tool", "[]")),
        (
            ".cursor-plugin/hooks.json",
            hooks(
                "stop",
                r#"sh -c '"$0" --flush || true' "${CURSOR_PLUGIN_ROOT}/bin/note""#,
            ),
        ),
    ]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 4);
}

#[test]
fn launcher_violations_are_reported() {
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "hooks/hooks.json",
            hooks("PreToolUse", r#""${CLAUDE_PLUGIN_ROOT}/bin/missing""#),
            "does not declare for plugin demo",
        ),
        (
            "hooks/hooks.json",
            hooks(
                "PreToolUse",
                r#""${CLAUDE_PLUGIN_ROOT}/bin/linux-x86_64/note""#,
            ),
            "must call the dispatcher",
        ),
        (
            "hooks/hooks.json",
            hooks("PreToolUse", r#""${CLAUDE_PLUGIN_ROOT}/bin/note.exe""#),
            "runs the .exe on Windows",
        ),
        (
            "hooks/hooks.json",
            hooks(
                "PreToolUse",
                r#"sh -c '"$0"/note' "${CLAUDE_PLUGIN_ROOT}/bin""#,
            ),
            "bin/ directory itself",
        ),
        (
            "hooks/hooks.json",
            hooks("PreToolUse", r#""${CLAUDE_PLUGIN_ROOT}/bin/guard""#),
            "canonical fail-closed launcher",
        ),
        (
            "hooks/hooks.json",
            hooks("PreToolUse", &guard().replace("deny", "allow")),
            "canonical fail-closed launcher",
        ),
        (
            "hooks/hooks.json",
            hooks("PostToolUse", &guard()),
            "must be registered under PreToolUse",
        ),
        (
            "hooks/hooks.json",
            hooks("PreToolUse", &mcp_command("CLAUDE_PLUGIN_ROOT", "tool")),
            "cannot be launched from a hook",
        ),
        (
            ".mcp.json",
            mcp(
                "sh",
                r#"["-c", "exec \"$0\"", "${CLAUDE_PLUGIN_ROOT}/bin/tool"]"#,
            ),
            "only appear in a launcher `command` field",
        ),
        (
            ".mcp.json",
            mcp("${CLAUDE_PLUGIN_ROOT}/bin/tool --stdio", "[]"),
            "must be exactly",
        ),
        (
            ".mcp.json",
            mcp(&mcp_command("CLAUDE_PLUGIN_ROOT", "guard"), "[]"),
            "cannot be launched as an MCP server",
        ),
        (
            "nested/deeper/mcp.json",
            mcp("bin/missing", "[]"),
            "does not declare",
        ),
        (".mcp.json", "{not json".to_string(), "invalid JSON"),
    ];
    for (path, text, needle) in cases {
        let (problems, _) = problems_for(&[(path, text.clone())]);
        assert!(
            problems.iter().any(|problem| problem.contains(needle)),
            "{path} {text}: expected {needle:?} in {problems:#?}"
        );
    }
}

#[test]
fn manifests_route_launcher_files_and_inline_servers_into_the_scan() {
    // A hooks file with a non-standard name is still scanned as hooks when a
    // manifest points at it, and inline mcpServers are scanned too.
    let (problems, references) = problems_for(&[
        (
            ".claude-plugin/plugin.json",
            r#"{"name": "demo", "hooks": "./custom/guards.json",
                "mcpServers": {"inline": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/undeclared"}}}"#
                .to_string(),
        ),
        ("custom/guards.json", hooks("PreToolUse", &guard())),
    ]);
    assert_eq!(references, 1, "{problems:#?}");
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("bin/undeclared"), "{problems:#?}");

    let (problems, _) = problems_for(&[(
        ".claude-plugin/plugin.json",
        r#"{"name": "demo", "hooks": "../escape/hooks.json", "mcpServers": "./absent.json"}"#
            .to_string(),
    )]);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("points outside the plugin")),
        "{problems:#?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("does not exist")),
        "{problems:#?}"
    );
}

#[test]
fn skills_and_fixtures_are_not_launchers() {
    let (problems, references) = problems_for(&[
        ("skills/x/mcp.json", mcp("bin/missing", "[]")),
        ("fixtures/hooks.json", hooks("PreToolUse", "bin/missing")),
    ]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 0);
}

#[test]
fn a_declared_plugin_must_exist() {
    let dir = temp_dir("noplugin");
    std::fs::create_dir_all(dir.join("plugins")).expect("plugins");
    let declaration = Declaration::parse(FIXTURE_DECLARATION).expect("fixture declaration");
    let report = check_launchers(&dir, &dir.join("plugins"), &declaration);
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("plugin demo has no directory")),
        "{:#?}",
        report.problems
    );
}
