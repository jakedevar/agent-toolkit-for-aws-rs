//! Launcher allowlist (#4487, PM ruling after review #5018 round 3): a
//! shell-evaluated launcher string is accepted only when it is byte for byte
//! one of a closed set of templates derived from the shipped inventory
//! (`tests/fixtures/shipped-launchers.json`), with only a declared binary
//! name or a plain plugin-relative path as its variable part. These tests
//! pin the inventory, reject every round-3 reproduction (#5069 POSIX bracket
//! classes, #5070 brace ranges, #5071 positional path construction) for
//! hooks and MCP servers, prove those reproductions are real bypasses at
//! runtime, and insert every shell metacharacter at every position of every
//! template to show no mutant is accepted.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{repo_root, temp_dir, write};
use plugin_stage::launcher::{check_launchers, guard_hook_command, mcp_command};
use plugin_stage::{Declaration, DECLARATION_PATH};
use toolkit_core::{escape_json_string, parse_json, Json};

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

const DENY_REASON: &str = "Guard unavailable. Reinstall demo.";

const INVENTORY: &str = "tests/fixtures/shipped-launchers.json";

/// Directories the packaging contract never scans for launcher files.
const SKIP_DIRS: [&str; 7] = [
    "skills",
    "fixtures",
    "tests",
    "node_modules",
    "bin",
    "target",
    ".git",
];

/// One inventory entry: repository-relative file, JSON key, string value and
/// the template it matches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    file: String,
    key: String,
    value: String,
    template: String,
}

fn inventory() -> Vec<Entry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(INVENTORY);
    let text = std::fs::read_to_string(&path).expect("inventory fixture");
    let root = parse_json(&text).expect("inventory JSON");
    let Some(Json::Array(items)) = root.get("strings") else {
        panic!("inventory has no strings list");
    };
    items
        .iter()
        .map(|item| {
            let field = |name: &str| {
                item.get(name)
                    .and_then(Json::as_str)
                    .unwrap_or_else(|| panic!("inventory entry without {name}: {item:?}"))
                    .to_string()
            };
            Entry {
                file: field("file"),
                key: field("key"),
                value: field("value"),
                template: field("template"),
            }
        })
        .collect()
}

fn hooks_entries(event: &str, commands: &[String]) -> String {
    let entries: Vec<String> = commands
        .iter()
        .map(|command| {
            format!(
                r#"{{"hooks": [{{"type": "command", "command": "{}"}}]}}"#,
                escape_json_string(command)
            )
        })
        .collect();
    format!(r#"{{"hooks": {{"{event}": [{}]}}}}"#, entries.join(", "))
}

fn hooks(event: &str, command: &str) -> String {
    hooks_entries(event, &[command.to_string()])
}

fn mcp(command: &str, args: &[&str]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|arg| format!("\"{}\"", escape_json_string(arg)))
        .collect();
    format!(
        r#"{{"mcpServers": {{"s": {{"type": "stdio", "command": "{}", "args": [{}]}}}}}}"#,
        escape_json_string(command),
        args.join(", ")
    )
}

/// Checks one fixture plugin `demo` holding `files`, against the fixture
/// declaration; returns the problems and the number of references.
fn problems_for(files: &[(&str, String)]) -> (Vec<String>, usize) {
    let dir = temp_dir("allowlist");
    let plugins = dir.join("plugins");
    write(&plugins.join("demo/README.md"), "demo\n");
    for (path, text) in files {
        write(&plugins.join("demo").join(path), text);
    }
    let declaration = Declaration::parse(FIXTURE_DECLARATION).expect("fixture declaration");
    let report = check_launchers(&dir, &plugins, &declaration);
    (report.problems, report.references.len())
}

fn json_key(path: &[String]) -> String {
    let mut out = String::new();
    for part in path {
        if !part.starts_with('[') && !out.is_empty() {
            out.push('.');
        }
        out.push_str(part);
    }
    out
}

/// Every hook or MCP `command` string and MCP `args` element of `value`.
fn launcher_strings(value: &Json, path: &mut Vec<String>, out: &mut Vec<(String, String)>) {
    match value {
        Json::String(text) => {
            let last = path.last().map(String::as_str);
            let parent = path.len().checked_sub(2).map(|at| path[at].as_str());
            if last == Some("command") || parent == Some("args") {
                out.push((json_key(path), text.clone()));
            }
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(format!("[{index}]"));
                launcher_strings(item, path, out);
                path.pop();
            }
        }
        Json::Object(pairs) => {
            for (key, item) in pairs {
                path.push(key.clone());
                launcher_strings(item, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn launcher_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read plugin directory")
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() && !SKIP_DIRS.contains(&name.as_str()) {
            launcher_files(&entry.path(), out);
        } else if kind.is_file() && ["hooks.json", ".mcp.json", "mcp.json"].contains(&name.as_str())
        {
            out.push(entry.path());
        }
    }
}

/// The inventory is complete and current: it lists exactly the `command` and
/// MCP `args` strings of every launcher file shipped under `plugins/`, and no
/// plugin manifest inlines launcher configuration (each `hooks` /
/// `mcpServers` member is a file path the scan reads).
#[test]
fn the_inventory_lists_every_shipped_launcher_string() {
    let root = repo_root();
    let mut files = Vec::new();
    launcher_files(&root.join("plugins"), &mut files);
    let mut shipped = BTreeSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("launcher file");
        let json = parse_json(&text).expect("launcher JSON");
        let mut found = Vec::new();
        launcher_strings(&json, &mut Vec::new(), &mut found);
        let rel = file
            .strip_prefix(&root)
            .expect("inside the repository")
            .to_string_lossy()
            .into_owned();
        for (key, value) in found {
            shipped.insert((rel.clone(), key, value));
        }
    }
    let listed: BTreeSet<(String, String, String)> = inventory()
        .into_iter()
        .map(|entry| (entry.file, entry.key, entry.value))
        .collect();
    assert_eq!(
        shipped.difference(&listed).collect::<Vec<_>>(),
        Vec::<&(String, String, String)>::new(),
        "shipped launcher strings missing from {INVENTORY}"
    );
    assert_eq!(
        listed.difference(&shipped).collect::<Vec<_>>(),
        Vec::<&(String, String, String)>::new(),
        "{INVENTORY} lists strings that are not shipped"
    );
    assert_eq!(listed.len(), 48, "inventory size");

    for plugin in std::fs::read_dir(root.join("plugins")).expect("plugins") {
        let plugin = plugin.expect("plugin").path();
        for manifest in [
            "plugin.json",
            ".claude-plugin/plugin.json",
            ".codex-plugin/plugin.json",
            ".cursor-plugin/plugin.json",
            ".devin-plugin/plugin.json",
        ] {
            let path = plugin.join(manifest);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let json = parse_json(&text).expect("manifest JSON");
            assert_no_inline_launchers(&json, &path);
        }
    }
}

fn assert_no_inline_launchers(value: &Json, path: &Path) {
    match value {
        Json::Object(pairs) => {
            for (key, item) in pairs {
                if key == "hooks" || key == "mcpServers" {
                    let paths_only = match item {
                        Json::String(_) => true,
                        Json::Array(items) => items.iter().all(|i| matches!(i, Json::String(_))),
                        _ => false,
                    };
                    assert!(
                        paths_only,
                        "{}: {key} inlines launcher configuration; add it to {INVENTORY}",
                        path.display()
                    );
                } else {
                    assert_no_inline_launchers(item, path);
                }
            }
        }
        Json::Array(items) => {
            for item in items {
                assert_no_inline_launchers(item, path);
            }
        }
        _ => {}
    }
}

/// Every inventory hook string is accepted in a fixture plugin, under every
/// event, and every inventory MCP server (command plus its args) too.
#[test]
fn every_inventory_string_is_accepted() {
    let entries = inventory();
    let hook_strings: Vec<String> = entries
        .iter()
        .filter(|entry| entry.file.ends_with("hooks.json"))
        .map(|entry| entry.value.clone())
        .collect();
    assert_eq!(hook_strings.len(), 12);
    for event in ["PreToolUse", "PostToolUse", "Stop", "SessionStart", "stop"] {
        let (problems, references) =
            problems_for(&[("hooks/hooks.json", hooks_entries(event, &hook_strings))]);
        assert!(problems.is_empty(), "{event}: {problems:#?}");
        assert_eq!(references, 0);
    }
    let mut servers = BTreeSet::new();
    for entry in entries.iter().filter(|e| e.key.ends_with(".command")) {
        if entry.file.ends_with("hooks.json") {
            continue;
        }
        let server = entry.key.trim_end_matches(".command");
        let args: Vec<&str> = entries
            .iter()
            .filter(|e| e.file == entry.file && e.key.starts_with(&format!("{server}.args[")))
            .map(|e| e.value.as_str())
            .collect();
        servers.insert((entry.value.clone(), args.join("\u{1}")));
    }
    assert!(!servers.is_empty());
    for (command, args) in servers {
        let args: Vec<&str> = args.split('\u{1}').collect();
        let (problems, references) = problems_for(&[(".mcp.json", mcp(&command, &args))]);
        assert!(problems.is_empty(), "{command} {args:?}: {problems:#?}");
        assert_eq!(references, 0);
    }
}

/// Review #5018 round 3 reproductions as hook commands. `{plugin}` is a
/// literal plugin directory (the reviewer's `cd "<plugin>"` cwd form).
const R3_HOOK_FORMS: [&str; 12] = [
    // #5069: POSIX bracket classes.
    "cd \"{plugin}\"; ./b[[:lower:]]n/guard || true",
    "cd \"{plugin}\"; ./b[[:alpha:]]n/guard || true",
    "env -C \"{plugin}\" sh -c './b[[:lower:]]n/guard || true'",
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; ./b[[:lower:]]n/guard || true",
    "\"${CLAUDE_PLUGIN_ROOT}\"/b[[:lower:]]n/guard || true",
    // #5070: brace sequence expansion.
    "cd \"{plugin}\"; ./{b..b}in/guard || true",
    "cd \"{plugin}\"; ./b{i..i}n/guard || true",
    "cd \"${CLAUDE_PLUGIN_ROOT}\"; ./{b..b}in/guard",
    // #5071: positional parameters building the path from the root.
    "sh -c '\"$0/$1$2/guard\" || true' \"${CLAUDE_PLUGIN_ROOT}\" b in",
    "sh -c '\"$0/$1/guard\" || true' \"${CLAUDE_PLUGIN_ROOT}\" bin",
    "sh -c 'cd \"$0\" && ./$1$2/guard' \"${CLAUDE_PLUGIN_ROOT}\" b in",
    "sh -c '\"$0\"/b$1/guard || true' \"${CLAUDE_PLUGIN_ROOT}\" in",
];

fn r3_hook_forms(plugin: &Path) -> Vec<String> {
    let plugin = plugin.to_string_lossy();
    R3_HOOK_FORMS
        .iter()
        .map(|form| form.replace("{plugin}", &plugin))
        .collect()
}

#[test]
fn review_r3_hook_reproductions_are_rejected() {
    for command in r3_hook_forms(Path::new("/tmp/review/plugins/demo")) {
        for event in ["PreToolUse", "Stop"] {
            let (problems, references) =
                problems_for(&[("hooks/hooks.json", hooks(event, &command))]);
            assert!(
                !problems.is_empty(),
                "{event} {command:?}: accepted with {references} references"
            );
            assert_eq!(references, 0, "{event} {command:?}: {problems:#?}");
            for problem in &problems {
                assert!(
                    problem.contains("plugins/demo/hooks/hooks.json")
                        && problem.contains(&format!("hooks.{event}[0].hooks[0].command")),
                    "{command:?}: {problem}"
                );
                assert!(
                    !problem.contains("lower:")
                        && !problem.contains("..b")
                        && !problem.contains("$1"),
                    "{command:?}: the message echoes the command: {problem}"
                );
            }
        }
    }
}

/// The same forms with the committed declaration and the real guard name.
#[test]
fn review_r3_secret_safety_registrations_fail_the_contract() {
    let declaration = Declaration::load(&repo_root().join(DECLARATION_PATH)).expect("declaration");
    let dir = temp_dir("r3secret");
    let plugins = dir.join("plugins");
    for command in r3_hook_forms(&plugins.join("aws-core")) {
        let command = command.replace("/guard", "/secret-safety");
        write(
            &plugins.join("aws-core/hooks/hooks.json"),
            &hooks("PreToolUse", &command),
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

/// Round 3 on the MCP side: shell interpreters with the script and the root
/// in separate args elements, and wrappers that start a shell.
#[test]
fn review_r3_mcp_reproductions_are_rejected() {
    let cases: [(&str, &[&str]); 9] = [
        (
            "sh",
            &[
                "-c",
                "cd \"$0\"; ./b[[:lower:]]n/tool || true",
                "${CLAUDE_PLUGIN_ROOT}",
            ],
        ),
        (
            "sh",
            &[
                "-c",
                "\"$0/$1$2/tool\" || true",
                "${CLAUDE_PLUGIN_ROOT}",
                "b",
                "in",
            ],
        ),
        (
            "sh",
            &["-c", "cd /tmp/review/plugins/demo; ./{b..b}in/tool"],
        ),
        (
            "bash",
            &[
                "-c",
                "cd \"$0\" && ./b{i..i}n/tool",
                "${CLAUDE_PLUGIN_ROOT}",
            ],
        ),
        ("bash", &["-c", "./b[[:alpha:]]n/tool"]),
        (
            "env",
            &[
                "-C",
                "/tmp/review/plugins/demo",
                "sh",
                "-c",
                "./b[[:lower:]]n/tool || true",
            ],
        ),
        (
            "env",
            &["-C", "${CLAUDE_PLUGIN_ROOT}", "./b[[:lower:]]n/tool"],
        ),
        ("/bin/sh", &["-c", "./b[[:lower:]]n/tool"]),
        ("cmd.exe", &["/c", "b^in\\tool.exe"]),
    ];
    for (command, args) in cases {
        let (problems, references) = problems_for(&[(".mcp.json", mcp(command, args))]);
        assert!(
            !problems.is_empty(),
            "{command} {args:?}: accepted with {references} references"
        );
        assert_eq!(references, 0, "{command} {args:?}: {problems:#?}");
    }
}

/// Plugin-root arguments of a non-shell MCP command must be a plain
/// plugin-relative path; a `bin` segment in any spelling is refused.
#[test]
fn mcp_argv_naming_the_root_or_bin_must_be_canonical() {
    for args in [
        &["${CLAUDE_PLUGIN_ROOT}/bin/note"][..],
        &["${CLAUDE_PLUGIN_ROOT}"],
        &["--root=${CLAUDE_PLUGIN_ROOT}/x"],
        &["${CLAUDE_PLUGIN_ROOT}/../demo/x"],
        &["${CLAUDE_PLUGIN_ROOT}/BIN/x"],
        &["${claude_plugin_root}/x"],
        &["$CLAUDE_PLUGIN_ROOT/x"],
        &["./bin/tool"],
        &["bin\\tool.exe"],
        &["x/Bin./tool"],
    ] {
        let (problems, references) = problems_for(&[(".mcp.json", mcp("node", args))]);
        assert!(!problems.is_empty(), "{args:?}: accepted");
        assert_eq!(references, 0, "{args:?}");
    }
    let (problems, _) = problems_for(&[(
        ".mcp.json",
        mcp(
            "node",
            &["${CLAUDE_PLUGIN_ROOT}/server/main.js", "--port", "0"],
        ),
    )]);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Hook shapes that are not in the shipped inventory are refused even when
/// they look harmless: a new shape needs a reviewed template change.
#[test]
fn non_template_hook_shapes_are_rejected() {
    for command in [
        "echo cabin binary robin ${CLAUDE_PLUGIN_ROOT}/node_modules/.bin-cache",
        "sh -c '\"$0\" --flush || true' \"${CLAUDE_PLUGIN_ROOT}/bin/note\"",
        "sh -c '\"$0\" --flush || true' \"${CURSOR_PLUGIN_ROOT}/bin/note\"",
        "sh -c 'python3 \"$0\" \"$@\" 2>/dev/null || true' \"${CURSOR_PLUGIN_ROOT}/scripts/x/migration.py\" --reconcile",
        "sh -c 'python3 \"$0\" 2>/dev/null || python \"$0\"' \"${CLAUDE_PLUGIN_ROOT}/hooks/guard.py\"",
        "cat \"${CLAUDE_PLUGIN_ROOT}/x.txt\"",
        "\"${CLAUDE_PLUGIN_ROOT}/scripts/../bin/note\"",
        "\"${CLAUDE_PLUGIN_ROOT}/scripts/bin/note\"",
        "\"${CLAUDE_PLUGIN_ROOT}/Bin./note\"",
        "\"${CLAUDE_PLUGIN_ROOT}/.hidden/x\"",
        "\"${CLAUDE_PLUGIN_ROOT}/x\" || true",
        "\"${CURSOR_PLUGIN_ROOT}/bin/note\"",
        "python3 x.py",
        "",
    ] {
        let (problems, references) = problems_for(&[("hooks/hooks.json", hooks("Stop", command))]);
        assert!(
            !problems.is_empty(),
            "{command:?}: accepted with {references} references"
        );
        assert_eq!(references, 0, "{command:?}");
    }
    // The ruling's twins are only templates when shipped: the guard and
    // plain-hook templates exist for CLAUDE_PLUGIN_ROOT only.
    let cursor_guard = guard_hook_command("CURSOR_PLUGIN_ROOT", "guard", DENY_REASON);
    let (problems, references) =
        problems_for(&[("hooks/hooks.json", hooks("PreToolUse", &cursor_guard))]);
    assert!(!problems.is_empty(), "CURSOR guard twin accepted");
    assert_eq!(references, 0);
}

/// A plugin-relative path whose components are symlinks could point into
/// `bin/`; it is refused even though its spelling is a plain relpath.
#[cfg(unix)]
#[test]
fn a_symlinked_plugin_path_is_not_a_plain_relpath() {
    let dir = temp_dir("relsymlink");
    let plugins = dir.join("plugins");
    write(&plugins.join("demo/bin/note"), "#!/bin/sh\n");
    std::os::unix::fs::symlink("../bin", plugins.join("demo/hooks")).expect("symlink");
    write(
        &plugins.join("demo/cfg/hooks.json"),
        &hooks("Stop", "\"${CLAUDE_PLUGIN_ROOT}/hooks/note\""),
    );
    let declaration = Declaration::parse(FIXTURE_DECLARATION).expect("fixture declaration");
    let report = check_launchers(&dir, &plugins, &declaration);
    assert!(!report.problems.is_empty(), "symlinked relpath accepted");
}

/// Shell metacharacters inserted into templates. Every byte that the
/// variable parts (`<name>`, `<relpath>`) cannot contain and every byte the
/// shell gives meaning to.
const METACHARACTERS: [char; 31] = [
    '$', '`', '\\', '\'', '"', '*', '?', '[', ']', '{', '}', '~', ';', '&', '|', '<', '>', '(',
    ')', '!', '#', '=', '%', '^', ',', ':', '@', ' ', '\t', '\n', '\r',
];

/// Every single-character insertion of each metacharacter at each position.
fn mutants(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    for at in 0..=template.len() {
        if !template.is_char_boundary(at) {
            continue;
        }
        for meta in METACHARACTERS {
            let mut mutant = template.to_string();
            mutant.insert(at, meta);
            out.push(mutant);
        }
    }
    out
}

/// The entry indices named by `problems` after `prefix` (`hooks.Stop[` or
/// `mcpServers.s`), read from each problem's `file: key: message` prefix.
fn rejected_indices(problems: &[String], prefix: &str) -> BTreeSet<usize> {
    problems
        .iter()
        .filter_map(|problem| {
            let at = problem.find(prefix)? + prefix.len();
            let digits: String = problem[at..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
        .collect()
}

/// Property-style (deterministic, exhaustive over single insertions): no
/// template instance accepts any metacharacter inserted anywhere, under a
/// guard event or any other, and no MCP command does either.
#[test]
fn metacharacter_insertions_into_templates_are_rejected() {
    let mut templates: Vec<String> = inventory()
        .into_iter()
        .filter(|entry| entry.file.ends_with("hooks.json"))
        .map(|entry| entry.value)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    templates.extend([
        guard_hook_command("CLAUDE_PLUGIN_ROOT", "guard", DENY_REASON),
        r#""${CLAUDE_PLUGIN_ROOT}/bin/note""#.to_string(),
        r#""${CLAUDE_PLUGIN_ROOT}/bin/note" || true"#.to_string(),
        r#""${CLAUDE_PLUGIN_ROOT}/scripts/run.sh""#.to_string(),
    ]);
    // Controls: every unmutated template is accepted.
    let (problems, _) =
        problems_for(&[("hooks/hooks.json", hooks_entries("PreToolUse", &templates))]);
    assert!(problems.is_empty(), "{problems:#?}");

    let mut total = 0;
    for template in &templates {
        let mutants = mutants(template);
        total += mutants.len();
        for event in ["PreToolUse", "Stop"] {
            let (problems, references) =
                problems_for(&[("hooks/hooks.json", hooks_entries(event, &mutants))]);
            assert_eq!(references, 0, "{template}: a mutant resolved a reference");
            let rejected = rejected_indices(&problems, &format!("hooks.{event}["));
            let accepted: Vec<&String> = (0..mutants.len())
                .filter(|index| !rejected.contains(index))
                .map(|index| &mutants[index])
                .collect();
            assert!(
                accepted.is_empty(),
                "{event}: {} mutants of {template:?} accepted, e.g. {:?}",
                accepted.len(),
                &accepted[..accepted.len().min(5)]
            );
        }
    }
    assert!(total > 50_000, "only {total} mutants");

    let mut commands: Vec<String> = vec!["uvx".to_string(), "./bin/tool".to_string()];
    commands.extend(["CLAUDE_PLUGIN_ROOT", "CURSOR_PLUGIN_ROOT"].map(|r| mcp_command(r, "tool")));
    for command in commands {
        let mutants = mutants(&command);
        let servers: Vec<String> = mutants
            .iter()
            .enumerate()
            .map(|(index, mutant)| {
                format!(
                    r#""s{index}": {{"command": "{}", "args": []}}"#,
                    escape_json_string(mutant)
                )
            })
            .collect();
        let file = format!(r#"{{"mcpServers": {{{}}}}}"#, servers.join(", "));
        let (problems, references) = problems_for(&[(".mcp.json", file)]);
        assert_eq!(references, 0, "{command}: a mutant resolved a reference");
        let rejected = rejected_indices(&problems, "mcpServers.s");
        for (index, mutant) in mutants.iter().enumerate() {
            assert!(
                rejected.contains(&index),
                "MCP command mutant {mutant:?} accepted"
            );
        }
    }
}

/// Runtime proof that the round-3 forms are real bypasses and that the
/// guard's missing-binary deny contract holds only for the canonical
/// launcher: with no binary staged, the canonical guard prints the deny and
/// no reproduction does; with an executable marker at `bin/guard`, the
/// reproductions run it. The checker accepts the canonical guard and
/// rejects every reproduction.
#[cfg(unix)]
#[test]
fn review_r3_forms_bypass_the_guard_at_runtime_and_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let dir = temp_dir("r3runtime");
    let plugin = dir.join("plugins/demo");
    write(&plugin.join("README.md"), "demo\n");
    let forms = r3_hook_forms(&plugin);
    let run = |shell: &str, command: &str| {
        Command::new(shell)
            .arg("-c")
            .arg(command)
            .env("CLAUDE_PLUGIN_ROOT", &plugin)
            .current_dir(&*dir)
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    };
    let canonical = guard_hook_command("CLAUDE_PLUGIN_ROOT", "guard", DENY_REASON);
    let denies = |out: &str| out.contains(r#""permissionDecision":"deny""#);
    assert!(
        denies(&run("sh", &canonical)),
        "the canonical guard must deny"
    );
    let (problems, references) =
        problems_for(&[("hooks/hooks.json", hooks("PreToolUse", &canonical))]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 1);

    let has_bash = Command::new("bash").arg("-c").arg("true").status().is_ok();
    for form in &forms {
        assert!(
            !denies(&run("sh", form)),
            "{form:?} denies without its binary"
        );
        let (problems, _) = problems_for(&[("hooks/hooks.json", hooks("PreToolUse", form))]);
        assert!(!problems.is_empty(), "{form:?} passes the checker");
    }

    let marker = plugin.join("bin/guard");
    write(&marker, "#!/bin/sh\necho REACHED_PLUGIN_BINARY\n");
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let reached = |shell: &str| {
        forms
            .iter()
            .filter(|form| run(shell, form).contains("REACHED_PLUGIN_BINARY"))
            .count()
    };
    // POSIX classes (cd and root forms) and the four positional forms reach
    // the binary in any POSIX sh.
    let reached_sh = reached("sh");
    assert!(
        reached_sh >= 7,
        "only {reached_sh} forms reach the binary under sh"
    );
    if has_bash {
        // bash also performs the brace sequence expansions (#5070).
        let reached_bash = reached("bash");
        assert!(
            reached_bash >= 10,
            "only {reached_bash} forms reach the binary under bash"
        );
    }
}
