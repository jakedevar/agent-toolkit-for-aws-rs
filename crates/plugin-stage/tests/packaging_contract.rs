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
fn skill_plugins_ship_toolkit_with_the_required_live_features() {
    let declaration = repo_declaration();
    for plugin in ["aws-core", "aws-agents", "aws-startup-advisor"] {
        let binary = declaration
            .binary(plugin, "toolkit")
            .unwrap_or_else(|| panic!("{plugin} must stage toolkit"));
        assert_eq!(binary.package, "toolkit-cli");
        assert_eq!(binary.bin, "toolkit");
        let expected = if plugin == "aws-agents" {
            vec!["agents-pay-live"]
        } else {
            vec!["ai-models-live", "docgraph-live"]
        };
        assert_eq!(binary.features, expected);
        assert_eq!(binary.role, plugin_stage::Role::Subprocess);
    }
    // The separately distributed payment package retains its live payment build.
    let payment = declaration
        .binaries
        .iter()
        .find(|binary| {
            matches!(&binary.destination, plugin_stage::Destination::PackageRoot(root)
            if root == "plugins/aws-agents/skills/agents-pay/packages/openclaw")
        })
        .expect("OpenClaw payment package");
    assert_eq!(payment.features, ["agents-pay-live"]);
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
            hooks("stop", r#""${CLAUDE_PLUGIN_ROOT}/bin/note" || true"#),
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
            "not an allowed launcher template",
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

/// #5022: every spelling of a plugin `bin/` path the checker cannot resolve
/// to exactly one canonical launcher reference is rejected (fail closed),
/// including the fail-open guard `"${CLAUDE_PLUGIN_ROOT}"/bin/guard || true`.
#[test]
fn unresolvable_plugin_bin_spellings_are_rejected() {
    let hook_forms = [
        r#""${CLAUDE_PLUGIN_ROOT}"/bin/guard || true"#,
        r#""$CLAUDE_PLUGIN_ROOT"/bin/guard"#,
        r#""./bin/undeclared""#,
        "exec ./bin/undeclared",
        r#"sh -c "./bin/undeclared""#,
        "${CLAUDE_PLUGIN_ROOT}/./bin/undeclared",
        "${CLAUDE_PLUGIN_ROOT}//bin/guard",
        "${CLAUDE_PLUGIN_ROOT}/BIN/guard",
        r#"cd "${CLAUDE_PLUGIN_ROOT}" && bin/guard || true"#,
        r#"cd "${CLAUDE_PLUGIN_ROOT}" && ./bin/guard"#,
        r#""${CLAUDE_PLUGIN_ROOT}/b""in/guard""#,
        r"${CLAUDE_PLUGIN_ROOT}/b\in/guard",
        "${CLAUDE_PLUGIN_ROOT}/../demo/bin/guard",
        r#"sh -c '"$0"/bin/guard || true' "${CLAUDE_PLUGIN_ROOT}""#,
        r#"sh -c '"$0"/guard || true' "${CLAUDE_PLUGIN_ROOT}/bin""#,
        "${CLAUDE_PLUGIN_ROOT}/${SUB}/guard",
        "${CLAUDE_PLUGIN_ROOT}/b?n/guard",
        "${CLAUDE_PLUGIN_ROOT}/b*/guard",
        "${CLAUDE_PLUGIN_ROOT}/{bin,x}/guard",
        "${CLAUDE_PLUGIN_ROOT:-.}/bin/guard",
        r#"D="${CLAUDE_PLUGIN_ROOT}"; "$D"/bin/guard || true"#,
        "PATH=${CLAUDE_PLUGIN_ROOT}/bin:$PATH guard",
        r#"x=$(echo bin); "${CLAUDE_PLUGIN_ROOT}/$x/guard""#,
        r#""${CLAUDE_PLUGIN_ROOT}/bin/note" "${CLAUDE_PLUGIN_ROOT}/bin/note""#,
    ];
    for command in hook_forms {
        for event in ["PreToolUse", "Stop"] {
            let (problems, references) =
                problems_for(&[("hooks/hooks.json", hooks(event, command))]);
            assert!(
                !problems.is_empty(),
                "{event} {command}: accepted with {references} references"
            );
            assert_eq!(references, 0, "{event} {command}: {problems:#?}");
        }
    }
    // A canonical guard with anything appended is no longer canonical.
    for suffix in [" || true", "; true", " &"] {
        let command = format!("{}{suffix}", guard());
        let (problems, _) = problems_for(&[("hooks/hooks.json", hooks("PreToolUse", &command))]);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("canonical fail-closed launcher")),
            "{command}: {problems:#?}"
        );
    }
    for (command, args) in [
        (r#""./bin/tool""#, "[]"),
        ("exec ./bin/tool", "[]"),
        ("${CLAUDE_PLUGIN_ROOT}/./bin/tool", "[]"),
        ("sh", r#"["-c", "./bin/tool"]"#),
        (
            "sh",
            r#"["-c", "exec \"$0\"", "${CLAUDE_PLUGIN_ROOT}/./bin/tool"]"#,
        ),
    ] {
        let (problems, references) = problems_for(&[(".mcp.json", mcp(command, args))]);
        assert!(
            !problems.is_empty(),
            "{command} {args}: accepted with {references} references"
        );
    }
    // A shipped template naming a plugin path outside bin/ is not a plugin
    // binary (the python guard launcher, until #4412 and #4415 retire it).
    let (problems, references) = problems_for(&[(
        "hooks/hooks.json",
        hooks(
            "PreToolUse",
            r#"sh -c 'python3 "$0" 2>/dev/null || python "$0" 2>/dev/null || py -3 "$0"' "${CLAUDE_PLUGIN_ROOT}/hooks/guard.py""#,
        ),
    )]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 0);
}

/// #5022, the reviewer's exact case against the committed declaration: the
/// fail-open secret-safety registration fails the contract.
#[test]
fn the_fail_open_secret_safety_registration_fails_the_contract() {
    let declaration = repo_declaration();
    let dir = temp_dir("failopen");
    let plugins = dir.join("plugins");
    for plugin in declaration.plugins() {
        std::fs::create_dir_all(plugins.join(plugin)).unwrap();
    }
    let deny_reason = match &declaration
        .binary("aws-core", "secret-safety")
        .expect("declared")
        .role
    {
        plugin_stage::Role::GuardHook { deny_reason } => deny_reason.clone(),
        other => panic!("{other:?}"),
    };
    for (command, ok) in [
        (
            r#""${CLAUDE_PLUGIN_ROOT}"/bin/secret-safety || true"#.to_string(),
            false,
        ),
        (
            guard_hook_command("CLAUDE_PLUGIN_ROOT", "secret-safety", &deny_reason),
            true,
        ),
    ] {
        write(
            &plugins.join("aws-core/hooks/hooks.json"),
            &hooks("PreToolUse", &command),
        );
        let report = check_launchers(&dir, &plugins, &declaration);
        assert_eq!(
            report.problems.is_empty(),
            ok,
            "{command}: {:#?}",
            report.problems
        );
        assert_eq!(report.references.len(), usize::from(ok), "{command}");
    }
}

/// #5024: `hooks` / `mcpServers` path lists keep their kind; every listed
/// file is resolved and scanned, and malformed members are refused.
#[test]
fn manifest_path_lists_are_resolved_and_scanned() {
    let undeclared =
        r#"{"mcpServers": {"s": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/undeclared"}}}"#;
    let (problems, _) = problems_for(&[
        (
            ".devin-plugin/plugin.json",
            r#"{"name": "demo", "mcpServers": ["./custom/servers.json"]}"#.to_string(),
        ),
        ("custom/servers.json", undeclared.to_string()),
    ]);
    assert!(
        problems.iter().any(|p| p.contains("bin/undeclared")),
        "{problems:#?}"
    );
    let (problems, _) = problems_for(&[
        (
            ".devin-plugin/plugin.json",
            r#"{"name": "demo", "mcpServers": ["./custom/servers.json"]}"#.to_string(),
        ),
        ("custom/servers.json", mcp(r#""./bin/tool""#, "[]")),
    ]);
    assert!(!problems.is_empty(), "unsafe launcher in a listed file");

    // A hooks list: each member is scanned as hooks, so the canonical guard
    // resolves and a second, fail-open file is rejected.
    let (problems, references) = problems_for(&[
        (
            ".claude-plugin/plugin.json",
            r#"{"name": "demo", "hooks": ["./custom/a.json", "custom/b.json"]}"#.to_string(),
        ),
        ("custom/a.json", hooks("PreToolUse", &guard())),
        (
            "custom/b.json",
            hooks("PreToolUse", r#""${CLAUDE_PLUGIN_ROOT}"/bin/guard || true"#),
        ),
    ]);
    assert_eq!(references, 1, "{problems:#?}");
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("custom/b.json"), "{problems:#?}");

    // Missing and escaping members, and inline objects inside a list.
    let (problems, references) = problems_for(&[(
        ".devin-plugin/plugin.json",
        r#"{"name": "demo", "mcpServers": ["./absent.json", "../escape.json",
            {"inline": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/tool"}}]}"#
            .to_string(),
    )]);
    assert_eq!(references, 1, "{problems:#?}");
    assert!(
        problems.iter().any(|p| p.contains("does not exist")),
        "{problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("points outside the plugin")),
        "{problems:#?}"
    );

    // Malformed member and value types are refused, not skipped.
    for manifest in [
        r#"{"name": "demo", "mcpServers": [1]}"#,
        r#"{"name": "demo", "mcpServers": [["./x.json"]]}"#,
        r#"{"name": "demo", "hooks": [null]}"#,
        r#"{"name": "demo", "hooks": true}"#,
        r#"{"name": "demo", "mcpServers": 7}"#,
    ] {
        let (problems, _) = problems_for(&[(".devin-plugin/plugin.json", manifest.to_string())]);
        assert!(
            problems.iter().any(|p| p.contains("must be a file path")),
            "{manifest}: {problems:#?}"
        );
    }
}

/// #5053 reproductions (review #5018 round 2) plus the round-1 #5022 forms
/// they extend: repeated and current-directory separators, shell
/// backslash-newline continuations (quoted and unquoted, LF and CRLF),
/// mixed quotes, an unbraced `$CLAUDE_PLUGIN_ROOT`, a second path to the
/// plugin `bin/` beside one canonical reference, and a system `/bin/` path.
/// Under the PM ruling on #4487 a string that touches `bin/` after
/// normalisation is refused unless it is a canonical form (or, for a plain
/// hook, holds exactly one canonical reference and nothing else touching
/// `bin/`), so every one of these is rejected for a guard and a plain hook.
const BIN_TOUCHING_FORMS: [&str; 24] = [
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; .//bin/guard || true",
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; .///bin/guard || true",
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; ././/bin/guard",
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; .//.//bin//guard",
    "cd \"${CLAUDE_PLUGIN_ROOT}\" && .//./bin/note",
    "${CLAUDE_PLUGIN_ROOT}/.//./bin/guard",
    "${CLAUDE_PLUGIN_ROOT}//.///bin/note || true",
    "\"${CLAUDE_PLUGIN_ROOT}/b\\\nin/guard\" || true",
    "${CLAUDE_PLUGIN_ROOT}/b\\\nin/guard || true",
    "\"${CLAUDE_PLUGIN_ROOT}/b\\\r\nin/guard\" || true",
    "\"${CLAUDE_PLUGIN_ROOT}\"/\\\nbin/note",
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; ./b\\\nin/guard",
    "\"${CLAUDE_PLUGIN_ROOT}\"'/bin'/guard || true",
    "'\"${CLAUDE_PLUGIN_ROOT}\"'/b\"i\"n/guard",
    "\"${CLAUDE_PLUGIN_ROOT}\"/'b'in/note || true",
    "$CLAUDE_PLUGIN_ROOT/bin/guard || true",
    "\"$CLAUDE_PLUGIN_ROOT/bin/note\"",
    "\"$CLAUDE_PLUGIN_ROOT\"/bin/note || true",
    "\"${CLAUDE_PLUGIN_ROOT}/bin/note\" ./bin/guard",
    "\"${CLAUDE_PLUGIN_ROOT}/bin/note\" \\\n.//bin/guard",
    "sh -c 'cd \"$0\"/.. && ./guard' \"${CLAUDE_PLUGIN_ROOT}/bin/note\"",
    "sh -c '\"$0\" || \"${0%/*}\"/guard' \"${CLAUDE_PLUGIN_ROOT}/bin/note\"",
    "sh -c 'cd \"$(dirname \"$0\")\" && ./guard' \"${CLAUDE_PLUGIN_ROOT}/bin/note\"",
    "/usr/bin/env sh \"${CLAUDE_PLUGIN_ROOT}/hooks/guard.sh\"",
];

#[test]
fn review_5053_spellings_are_rejected_for_every_hook() {
    for command in BIN_TOUCHING_FORMS {
        for event in ["PreToolUse", "Stop"] {
            let (problems, references) =
                problems_for(&[("hooks/hooks.json", hooks(event, command))]);
            assert!(
                !problems.is_empty(),
                "{event} {command:?}: accepted with {references} references"
            );
            assert_eq!(references, 0, "{event} {command:?}: {problems:#?}");
            // The refusal names the file and key and never echoes the
            // command (#5053, PM ruling point 5).
            for problem in &problems {
                assert!(
                    problem.contains("plugins/demo/hooks/hooks.json")
                        && problem.contains(&format!("hooks.{event}[0].hooks[0].command")),
                    "{command:?}: {problem}"
                );
                assert!(
                    !problem.contains("guard ||")
                        && !problem.contains("//")
                        && !problem.contains('\n'),
                    "{command:?}: the message echoes the command: {problem}"
                );
            }
        }
    }
}

/// #5053 against the committed declaration: the reviewer's exact
/// secret-safety registrations fail the contract.
#[test]
fn review_5053_secret_safety_registrations_fail_the_contract() {
    let declaration = repo_declaration();
    let dir = temp_dir("r5053");
    let plugins = dir.join("plugins");
    for command in [
        "cd \"${CLAUDE_PLUGIN_ROOT}\"; .//bin/secret-safety || true",
        "cd \"${CLAUDE_PLUGIN_ROOT}\"; .///bin/secret-safety || true",
        "\"${CLAUDE_PLUGIN_ROOT}/b\\\nin/secret-safety\" || true",
        "${CLAUDE_PLUGIN_ROOT}/b\\\nin/secret-safety || true",
    ] {
        write(
            &plugins.join("aws-core/hooks/hooks.json"),
            &hooks("PreToolUse", command),
        );
        let report = check_launchers(&dir, &plugins, &declaration);
        assert!(
            !report.problems.is_empty(),
            "{command:?}: accepted with {} references",
            report.references.len()
        );
        assert!(report.references.is_empty(), "{command:?}");
    }
}

/// #5053 on the MCP side: command and args strings, and an inline
/// mcpServers object inside a Devin path list, all fail closed.
#[test]
fn review_5053_mcp_spellings_are_rejected() {
    for (command, args) in [
        (r#"".//bin/undeclared""#, "[]"),
        (".//bin/tool", "[]"),
        (".///bin/tool", "[]"),
        ("././/bin/tool", "[]"),
        ("${CLAUDE_PLUGIN_ROOT}/.//bin/tool", "[]"),
        ("${CLAUDE_PLUGIN_ROOT}/b\\\nin/tool", "[]"),
        ("$CLAUDE_PLUGIN_ROOT/bin/tool", "[]"),
        ("sh", r#"["-c", ".//bin/tool"]"#),
        ("sh", r#"["-c", "./b\\\nin/tool"]"#),
        (
            "sh",
            r#"["-c", "exec \"$0\"", "${CLAUDE_PLUGIN_ROOT}/.//bin/tool"]"#,
        ),
        ("sh", r#"["-c", "./b?n/tool"]"#),
        ("sh", r#"["-c", "./{b,x}in/tool"]"#),
    ] {
        let (problems, references) = problems_for(&[(".mcp.json", mcp(command, args))]);
        assert!(
            !problems.is_empty(),
            "{command:?} {args}: accepted with {references} references"
        );
        assert_eq!(references, 0, "{command:?} {args}: {problems:#?}");
    }
    let (problems, references) = problems_for(&[(
        ".devin-plugin/plugin.json",
        r#"{"name": "demo", "mcpServers": [{"inline": {"command": "\".//bin/undeclared\""}}]}"#
            .to_string(),
    )]);
    assert!(!problems.is_empty(), "inline list object accepted");
    assert_eq!(references, 0, "{problems:#?}");
}

/// Controls for the #5053 ruling: ordinary commands that do not touch the
/// plugin `bin/` directory (every shipped launcher shape among them), the
/// canonical forms and a wrapped plain-hook reference still pass.
#[test]
fn ordinary_commands_and_canonical_forms_still_pass() {
    let (problems, references) = problems_for(&[
        (
            "hooks/hooks.json",
            hooks(
                "PreToolUse",
                r#"sh -c 'python3 "$0" 2>/dev/null || python "$0" 2>/dev/null || py -3 "$0"' "${CLAUDE_PLUGIN_ROOT}/hooks/guard.py""#,
            ),
        ),
        (
            "a/hooks.json",
            hooks(
                "SessionStart",
                r#"cat "${CLAUDE_PLUGIN_ROOT}/hooks/offer-context.txt" 2>/dev/null || true"#,
            ),
        ),
        (
            ".cursor-plugin/hooks.json",
            hooks(
                "stop",
                r#"sh -c 'python3 "$0" "$@" 2>/dev/null || python "$0" "$@" 2>/dev/null || true' "${CURSOR_PLUGIN_ROOT}/scripts/x/migration.py" --reconcile"#,
            ),
        ),
        (
            "b/hooks.json",
            hooks(
                "Stop",
                r#""${CLAUDE_PLUGIN_ROOT}/scripts/cabin/robin.bin-cache""#,
            ),
        ),
        (
            ".mcp.json",
            mcp(
                "uvx",
                r#"["mcp-proxy-for-aws-cli==1.7.0", "https://aws-mcp.us-east-1.api.aws/mcp", "--skip-auth", "--metadata", "INSTALL_SOURCE=agent-toolkit-core"]"#,
            ),
        ),
        (
            "c/.mcp.json",
            r#"{"mcpServers": {"h": {"type": "http", "url": "https://x.example/mcp?a=1",
                "headers": {"Authorization": "Bearer ${DEVOPS_AGENT_TOKEN}"}}}}"#
                .to_string(),
        ),
    ]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 0);

    let (problems, references) = problems_for(&[
        ("hooks/hooks.json", hooks("PreToolUse", &guard())),
        (
            ".mcp.json",
            mcp(&mcp_command("CLAUDE_PLUGIN_ROOT", "tool"), r#"["serve"]"#),
        ),
        ("mcp.json", mcp("./bin/tool", "[]")),
        (
            "e/hooks.json",
            hooks("Stop", r#""${CLAUDE_PLUGIN_ROOT}/bin/note" || true"#),
        ),
        (
            "f/hooks.json",
            hooks("Stop", r#""${CLAUDE_PLUGIN_ROOT}/bin/note""#),
        ),
    ]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 5);
}

/// The missing-binary deny contract behind #5053: with no binary staged, the
/// canonical guard launcher prints the deny and passes the checker, while
/// every reviewed bypass exits without a deny (fail open at runtime) and is
/// rejected by the checker. A guard registration passes the checker only if
/// it denies without its binary.
#[cfg(unix)]
#[test]
fn a_guard_registration_passes_only_if_it_denies_without_its_binary() {
    use std::process::Command;
    let dir = temp_dir("denycontract");
    let root = dir.join("plugins/demo");
    write(&root.join("README.md"), "demo\n");
    let run = |command: &str| {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("CLAUDE_PLUGIN_ROOT", &root)
            .current_dir(&*dir)
            .output()
            .expect("run sh");
        String::from_utf8_lossy(&output.stdout).contains(r#""permissionDecision":"deny""#)
    };
    let canonical = guard();
    assert!(run(&canonical), "the canonical guard must deny");
    let (problems, references) =
        problems_for(&[("hooks/hooks.json", hooks("PreToolUse", &canonical))]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 1);
    let mut fail_open = 0;
    for command in BIN_TOUCHING_FORMS
        .iter()
        .filter(|form| form.contains("in/guard") || form.contains("/guard"))
    {
        let denies = run(command);
        fail_open += usize::from(!denies);
        let (problems, references) =
            problems_for(&[("hooks/hooks.json", hooks("PreToolUse", command))]);
        assert!(
            !problems.is_empty() || denies,
            "{command:?} passes the checker ({references} references) but does not deny without its binary"
        );
    }
    assert!(
        fail_open >= 10,
        "only {fail_open} forms fail open at runtime"
    );
}
