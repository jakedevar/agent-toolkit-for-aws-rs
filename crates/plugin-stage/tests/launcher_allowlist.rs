//! Launcher allowlist (#4487, PM ruling after review #5018 round 3): a
//! shell-evaluated launcher string is accepted only when it is byte for byte
//! one of a closed set of templates derived from the shipped inventory
//! (`tests/fixtures/shipped-launchers.json`), with only a declared binary
//! name or a plain plugin-relative path as its variable part. These tests
//! pin the inventory, reject every round-3 reproduction (#5069 POSIX bracket
//! classes, #5070 brace ranges, #5071 positional path construction) for
//! hooks and MCP servers, prove those reproductions are real bypasses at
//! runtime, and insert every shell metacharacter at every position of every
//! template to show no mutant is accepted. After review #5018 round 4
//! (#5088, `env -S` splitting an argument into a shell) an MCP server is
//! checked as a whole: its command is a canonical binary or an inventory
//! program (`uvx`) and its args match that program's argv template.

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

/// Templates whose instances name a binary that the repository declaration
/// declares for one plugin. Such a string is valid only in that plugin, so
/// the fixture-plugin tests below skip the shipped instances. They exercise
/// the fixture's own declared `guard` instead, and
/// `shipped_strings_match_their_inventory_templates` checks the shipped
/// instances against the real declaration (#4412 activation, #5078).
const DECLARED_BINARY_TEMPLATES: [&str; 4] =
    ["guard-deny", "bin-hook", "bin-hook-or-true", "mcp-bin"];

/// The inventory entries that are valid in any plugin (no declared binary).
fn plugin_agnostic_inventory() -> Vec<Entry> {
    inventory()
        .into_iter()
        .filter(|entry| !DECLARED_BINARY_TEMPLATES.contains(&entry.template.as_str()))
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

/// Every plugin-agnostic inventory hook string is accepted in a fixture
/// plugin, under every event, and every inventory MCP server (command plus its
/// args) too. The declared-binary instances are the two aws-core secret-safety
/// guard registrations.
#[test]
fn every_inventory_string_is_accepted() {
    let declared: Vec<(String, String)> = inventory()
        .into_iter()
        .filter(|entry| DECLARED_BINARY_TEMPLATES.contains(&entry.template.as_str()))
        .map(|entry| (entry.file, entry.template))
        .collect();
    let guard = (
        "plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json".to_string(),
        "guard-deny".to_string(),
    );
    assert_eq!(declared, [guard.clone(), guard]);
    let entries = plugin_agnostic_inventory();
    let hook_strings: Vec<String> = entries
        .iter()
        .filter(|entry| entry.file.ends_with("hooks.json"))
        .map(|entry| entry.value.clone())
        .collect();
    assert_eq!(hook_strings.len(), 10);
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

/// The #5088 reproduction (review #5018 round 4): `env -S` splits one
/// literal-looking argument into a shell invocation that reaches `libexec/`.
/// `{plugin}` is a literal plugin directory, `{name}` the binary.
const R4_ENV_SPLIT_FORMS: [&[&str]; 8] = [
    &[
        "-C",
        "${CLAUDE_PLUGIN_ROOT}/work",
        "-S",
        "sh -c '../b[[:lower:]]n/{name} || true'",
    ],
    &[
        "-C",
        "{plugin}/work",
        "-S",
        "sh -c '../b[[:lower:]]n/{name} || true'",
    ],
    &[
        "-C",
        "${CLAUDE_PLUGIN_ROOT}/work",
        "-Ssh -c '../b[[:lower:]]n/{name} || true'",
    ],
    &[
        "-C",
        "${CLAUDE_PLUGIN_ROOT}/work",
        "--split-string",
        "sh -c '../b[[:lower:]]n/{name} || true'",
    ],
    &[
        "-C",
        "${CLAUDE_PLUGIN_ROOT}/work",
        "--split-string=sh -c '../b[[:lower:]]n/{name} || true'",
    ],
    &[
        "-S",
        "sh -c 'cd \"$0\"; ./b[[:lower:]]n/{name}'",
        "{plugin}",
    ],
    &["-S", "sh -c ./{b..b}in/{name}"],
    &["-S", "bash -c ./b{i..i}n/{name}"],
];

fn r4_env_forms(plugin: &str, name: &str) -> Vec<Vec<String>> {
    R4_ENV_SPLIT_FORMS
        .iter()
        .map(|form| {
            form.iter()
                .map(|arg| arg.replace("{plugin}", plugin).replace("{name}", name))
                .collect()
        })
        .collect()
}

/// #5088: every `env` split-string wrapper is refused for the fixture guard
/// and MCP server, with a fixed message that does not echo the argv.
#[test]
fn review_r4_env_split_string_mcp_wrappers_are_rejected() {
    for name in ["guard", "tool", "note", "undeclared"] {
        for args in r4_env_forms("/tmp/review/plugins/demo", name) {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            for file in [".mcp.json", "mcp.json"] {
                let (problems, references) = problems_for(&[(file, mcp("env", &args))]);
                assert!(
                    !problems.is_empty(),
                    "{file} env {args:?}: accepted with {references} references"
                );
                assert_eq!(references, 0, "{args:?}: {problems:#?}");
                for problem in &problems {
                    assert!(
                        problem.contains(&format!("plugins/demo/{file}: mcpServers.s.")),
                        "{problem}"
                    );
                    assert!(
                        !problem.contains("lower:") && !problem.contains("-c "),
                        "the message echoes the argv: {problem}"
                    );
                }
            }
        }
    }
}

/// #5088 with the committed declaration: the exact reviewer JSON on
/// aws-core, and every split-string variant aimed at secret-safety.
#[test]
fn review_r4_secret_safety_env_wrappers_fail_the_contract() {
    let declaration = Declaration::load(&repo_root().join(DECLARATION_PATH)).expect("declaration");
    let dir = temp_dir("r4secret");
    let plugins = dir.join("plugins");
    std::fs::create_dir_all(plugins.join("aws-core/work")).expect("work dir");
    let exact = r#"{"mcpServers":{"s":{"command":"env","args":["-C","${CLAUDE_PLUGIN_ROOT}/work","-S","sh -c '../b[[:lower:]]n/secret-safety || true'"]}}}"#;
    let plugin = plugins.join("aws-core").to_string_lossy().into_owned();
    let mut files = vec![exact.to_string()];
    for args in r4_env_forms(&plugin, "secret-safety") {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        files.push(mcp("env", &args));
    }
    for file in files {
        write(&plugins.join("aws-core/.mcp.json"), &file);
        let report = check_launchers(&dir, &plugins, &declaration);
        assert!(
            !report.problems.is_empty(),
            "{file}: accepted with {} references",
            report.references.len()
        );
        assert!(report.references.is_empty(), "{file}");
    }
}

/// The ruling's program launchers, shells and interpreters are refused as an
/// MCP `command` whatever their args, as is any program not in the shipped
/// inventory (only `uvx`), in any spelling.
#[test]
fn mcp_launchers_shells_and_interpreters_are_refused() {
    let launchers = [
        "env",
        "xargs",
        "nice",
        "nohup",
        "timeout",
        "stdbuf",
        "setsid",
        "sudo",
        "doas",
        "busybox",
        "sh",
        "bash",
        "dash",
        "zsh",
        "fish",
        "cmd",
        "cmd.exe",
        "powershell",
        "powershell.exe",
        "pwsh",
        "python",
        "python3",
        "py",
        "node",
        "ruby",
        "perl",
        "deno",
        "npx",
        "uv",
        "pipx",
        "chrt",
        "ionice",
        "taskset",
        "unshare",
        "flock",
        "script",
        "watch",
        "find",
        "awk",
        "/usr/bin/env",
        "/bin/sh",
        "C:\\Windows\\System32\\cmd.exe",
        "UVX",
        "Uvx",
        "uvx.exe",
        "uvx.",
        "./uvx",
        "uvx ",
        " uvx",
    ];
    let argvs: [&[&str]; 5] = [
        &[],
        &["--version"],
        &["-c", "./b[[:lower:]]n/tool"],
        &["-e", "require('child_process')"],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            "https://aws-mcp.us-east-1.api.aws/mcp",
            "--skip-auth",
        ],
    ];
    for command in launchers {
        for args in argvs {
            let (problems, references) = problems_for(&[(".mcp.json", mcp(command, args))]);
            assert!(
                !problems.is_empty(),
                "{command:?} {args:?}: accepted with {references} references"
            );
            assert_eq!(references, 0, "{command:?} {args:?}");
        }
    }
}

const SHIPPED_UVX_ARGS: [&str; 5] = [
    "mcp-proxy-for-aws-cli==1.7.0",
    "https://aws-mcp.us-east-1.api.aws/mcp",
    "--skip-auth",
    "--metadata",
    "INSTALL_SOURCE=agent-toolkit-core",
];

/// `uvx` args must match its inventory template: a pinned package of the
/// inventory, an https URL, then only `--skip-auth` and `--metadata
/// KEY=value`. The server object may hold only `type` (`stdio`), `command`
/// and `args`, and args without a command are refused.
#[test]
fn uvx_argv_must_match_its_inventory_template() {
    let accepted: [&[&str]; 4] = [
        &SHIPPED_UVX_ARGS,
        &[
            "mcp-proxy-for-aws-cli==1.8.12",
            "https://aws-mcp.eu-west-1.api.aws/mcp",
            "--metadata",
            "INSTALL_SOURCE=agent-toolkit-startup-advisor",
        ],
        &[
            "mcp-proxy-for-aws-cli==2.0",
            "https://knowledge-mcp.global.api.aws",
        ],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            "https://aws-mcp.us-east-1.api.aws/mcp",
            "--skip-auth",
        ],
    ];
    for args in accepted {
        let (problems, _) = problems_for(&[(".mcp.json", mcp("uvx", args))]);
        assert!(problems.is_empty(), "{args:?}: {problems:#?}");
    }
    let url = "https://aws-mcp.us-east-1.api.aws/mcp";
    let refused: [&[&str]; 26] = [
        &[],
        &["mcp-proxy-for-aws-cli==1.7.0"],
        &[url],
        &["mcp-proxy-for-aws-cli", url],
        &["mcp-proxy-for-aws-cli>=1.7.0", url],
        &["mcp-proxy-for-aws-cli==1.7.0rc1", url],
        &["mcp-proxy-for-aws-cli==1..7", url],
        &["other-package==1.0", url],
        &["--from", "./x", "mcp-proxy-for-aws-cli==1.7.0", url],
        &[
            "--from=${CLAUDE_PLUGIN_ROOT}/x",
            "mcp-proxy-for-aws-cli==1.7.0",
            url,
        ],
        &["--with", "evil", "mcp-proxy-for-aws-cli==1.7.0", url],
        &["git+https://example.com/x.git", url],
        &["./b[[:lower:]]n/tool", url],
        &["mcp-proxy-for-aws-cli==1.7.0", "http://aws-mcp.example/mcp"],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            "https://user@aws-mcp.example/mcp",
        ],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            "https://aws-mcp.example:8443/mcp",
        ],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            "https://aws-mcp.example/mcp?x=1",
        ],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            "https://aws-mcp.example/libexec/x",
        ],
        &["mcp-proxy-for-aws-cli==1.7.0", "https://localhost/mcp"],
        &["mcp-proxy-for-aws-cli==1.7.0", url, "--metadata"],
        &["mcp-proxy-for-aws-cli==1.7.0", url, "--metadata", "lower=x"],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            url,
            "--metadata",
            "K=${CLAUDE_PLUGIN_ROOT}/x",
        ],
        &["mcp-proxy-for-aws-cli==1.7.0", url, "--metadata", "K=bin"],
        &["mcp-proxy-for-aws-cli==1.7.0", url, "--log-file", "/tmp/x"],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            url,
            "sh -c ./b[[:lower:]]n/tool",
        ],
        &[
            "mcp-proxy-for-aws-cli==1.7.0",
            url,
            "${CLAUDE_PLUGIN_ROOT}/x",
        ],
    ];
    for args in refused {
        let (problems, references) = problems_for(&[(".mcp.json", mcp("uvx", args))]);
        assert!(!problems.is_empty(), "uvx {args:?}: accepted");
        assert_eq!(references, 0, "{args:?}");
        for problem in &problems {
            assert!(
                problem.contains("plugins/demo/.mcp.json: mcpServers.s"),
                "{problem}"
            );
        }
    }
}

/// A `uvx` server object may hold only `type` (`stdio`), `command` and
/// `args`; args must be an array of strings; args without a string command
/// are refused. A canonical binary server keeps plain literal args.
#[test]
fn mcp_server_objects_are_checked_as_a_whole() {
    let quoted: Vec<String> = SHIPPED_UVX_ARGS
        .iter()
        .map(|arg| format!("\"{arg}\""))
        .collect();
    let args = quoted.join(", ");
    for server in [
        format!(
            r#"{{"command": "uvx", "args": [{args}], "env": {{"PATH": "${{CLAUDE_PLUGIN_ROOT}}/x"}}}}"#
        ),
        format!(r#"{{"command": "uvx", "args": [{args}], "cwd": "${{CLAUDE_PLUGIN_ROOT}}/work"}}"#),
        format!(r#"{{"type": "http", "command": "uvx", "args": [{args}]}}"#),
        format!(
            r#"{{"command": "uvx", "args": "{}"}}"#,
            SHIPPED_UVX_ARGS.join(" ")
        ),
        format!(r#"{{"command": "uvx", "args": [{args}, 7]}}"#),
        r#"{"command": "uvx"}"#.to_string(),
        r#"{"command": ["env", "-S", "sh -c x"], "args": []}"#.to_string(),
        r#"{"args": ["-S", "sh -c ./b[[:lower:]]n/tool"]}"#.to_string(),
    ] {
        let file = format!(r#"{{"mcpServers": {{"s": {server}}}}}"#);
        let (problems, references) = problems_for(&[(".mcp.json", file)]);
        assert!(!problems.is_empty(), "{server}: accepted");
        assert_eq!(references, 0, "{server}");
    }
    // A canonical binary server may still carry plain literal args.
    let (problems, references) = problems_for(&[(
        ".mcp.json",
        mcp(
            &mcp_command("CLAUDE_PLUGIN_ROOT", "tool"),
            &["serve", "--port", "0"],
        ),
    )]);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(references, 1);
}

/// Runtime proof for #5088 (when this host's `env` supports `-S` and `-C`):
/// with no binary staged the wrapper exits without a deny, and with an
/// executable marker at `libexec/guard` it runs the marker. The checker refuses
/// every variant.
#[cfg(unix)]
#[test]
fn review_r4_env_split_string_reaches_the_binary_at_runtime_and_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let supported = Command::new("env")
        .args(["-C", "/", "-S", "true"])
        .status()
        .is_ok_and(|status| status.success());
    let dir = temp_dir("r4runtime");
    let plugin = dir.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("work")).expect("work dir");
    let plugin_text = plugin.to_string_lossy().into_owned();
    let forms = r4_env_forms(&plugin_text, "guard");
    for args in &forms {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (problems, references) = problems_for(&[(".mcp.json", mcp("env", &args))]);
        assert!(!problems.is_empty(), "env {args:?} passes the checker");
        assert_eq!(references, 0);
    }
    if !supported {
        eprintln!("env -C/-S unsupported here; runtime half skipped");
        return;
    }
    let run = |args: &[String]| {
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.replace("${CLAUDE_PLUGIN_ROOT}", &plugin_text))
            .collect();
        Command::new("env")
            .args(&args)
            .current_dir(&*dir)
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    };
    for args in &forms[..5] {
        let out = run(args);
        assert!(
            !out.contains(r#""permissionDecision":"deny""#),
            "{args:?} denies without its binary"
        );
    }
    let marker = plugin.join("libexec/guard");
    write(&marker, "#!/bin/sh\necho REACHED_PLUGIN_BINARY\n");
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let reached = forms[..5]
        .iter()
        .filter(|args| run(args).contains("REACHED_PLUGIN_BINARY"))
        .count();
    assert_eq!(
        reached, 5,
        "the split, attached and long -S forms reach libexec/guard"
    );
}

/// Arguments of a canonical binary MCP server are literal data: one naming
/// the plugin root must be a plain plugin-relative path, and a `bin` segment
/// in any spelling is refused.
#[test]
fn mcp_argv_naming_the_root_or_bin_must_be_canonical() {
    let tool = mcp_command("CLAUDE_PLUGIN_ROOT", "tool");
    for args in [
        &["${CLAUDE_PLUGIN_ROOT}/libexec/note"][..],
        &["${CLAUDE_PLUGIN_ROOT}"],
        &["--root=${CLAUDE_PLUGIN_ROOT}/x"],
        &["${CLAUDE_PLUGIN_ROOT}/../demo/x"],
        &["${CLAUDE_PLUGIN_ROOT}/BIN/x"],
        &["${claude_plugin_root}/x"],
        &["$CLAUDE_PLUGIN_ROOT/x"],
        &["./libexec/tool"],
        &["bin\\tool.exe"],
        &["x/Bin./tool"],
        &["sh", "-c", "x"],
    ] {
        let (problems, _) = problems_for(&[(".mcp.json", mcp(&tool, args))]);
        assert!(!problems.is_empty(), "{args:?}: accepted");
    }
    let (problems, _) = problems_for(&[(
        ".mcp.json",
        mcp(
            &tool,
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
        "sh -c '\"$0\" --flush || true' \"${CLAUDE_PLUGIN_ROOT}/libexec/note\"",
        "sh -c '\"$0\" --flush || true' \"${CURSOR_PLUGIN_ROOT}/libexec/note\"",
        "sh -c 'python3 \"$0\" \"$@\" 2>/dev/null || true' \"${CURSOR_PLUGIN_ROOT}/scripts/x/migration.py\" --reconcile",
        "sh -c 'python3 \"$0\" 2>/dev/null || python \"$0\"' \"${CLAUDE_PLUGIN_ROOT}/hooks/guard.py\"",
        "cat \"${CLAUDE_PLUGIN_ROOT}/x.txt\"",
        "\"${CLAUDE_PLUGIN_ROOT}/scripts/../libexec/note\"",
        "\"${CLAUDE_PLUGIN_ROOT}/scripts/libexec/note\"",
        "\"${CLAUDE_PLUGIN_ROOT}/Bin./note\"",
        "\"${CLAUDE_PLUGIN_ROOT}/.hidden/x\"",
        "\"${CLAUDE_PLUGIN_ROOT}/x\" || true",
        "\"${CURSOR_PLUGIN_ROOT}/libexec/note\"",
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
/// `libexec/`; it is refused even though its spelling is a plain relpath.
#[cfg(unix)]
#[test]
fn a_symlinked_plugin_path_is_not_a_plain_relpath() {
    let dir = temp_dir("relsymlink");
    let plugins = dir.join("plugins");
    write(&plugins.join("demo/libexec/note"), "#!/bin/sh\n");
    std::os::unix::fs::symlink("../libexec", plugins.join("demo/hooks")).expect("symlink");
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
    let mut templates: Vec<String> = plugin_agnostic_inventory()
        .into_iter()
        .filter(|entry| entry.file.ends_with("hooks.json"))
        .map(|entry| entry.value)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    templates.extend([
        guard_hook_command("CLAUDE_PLUGIN_ROOT", "guard", DENY_REASON),
        r#""${CLAUDE_PLUGIN_ROOT}/libexec/note""#.to_string(),
        r#""${CLAUDE_PLUGIN_ROOT}/libexec/note" || true"#.to_string(),
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
}

/// Property-style over MCP commands (#5088 ruling): each metacharacter
/// inserted anywhere in a program or binary command is refused, even with
/// the args its unmutated form accepts.
#[test]
fn metacharacter_insertions_into_mcp_commands_are_rejected() {
    // MCP commands: each mutant of a program or binary command is refused
    // even with the args its unmutated form accepts.
    let shipped_args: String = SHIPPED_UVX_ARGS
        .iter()
        .map(|arg| format!("\"{arg}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut commands: Vec<(String, String)> = vec![
        ("uvx".to_string(), shipped_args),
        ("./libexec/tool".to_string(), String::new()),
    ];
    commands.extend(
        ["CLAUDE_PLUGIN_ROOT", "CURSOR_PLUGIN_ROOT"]
            .map(|r| (mcp_command(r, "tool"), String::new())),
    );
    for (command, args) in commands {
        let (problems, _) = problems_for(&[(
            ".mcp.json",
            format!(r#"{{"mcpServers": {{"s": {{"command": "{command}", "args": [{args}]}}}}}}"#),
        )]);
        assert!(problems.is_empty(), "control {command}: {problems:#?}");
        let mutants = mutants(&command);
        let servers: Vec<String> = mutants
            .iter()
            .enumerate()
            .map(|(index, mutant)| {
                format!(
                    r#""s{index}": {{"command": "{}", "args": [{args}]}}"#,
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

/// Property-style over MCP argv (#5088 ruling): every distinct shipped `uvx`
/// argv with each metacharacter inserted at each position of each element
/// is refused; each unmutated argv is accepted.
#[test]
fn metacharacter_insertions_into_mcp_argv_are_rejected() {
    // MCP argv (#5088 ruling): every distinct shipped uvx argv, with each
    // metacharacter inserted at each position of each element, is refused.
    let entries = inventory();
    let mut argvs: BTreeSet<Vec<String>> = BTreeSet::new();
    for entry in entries.iter().filter(|e| e.value == "uvx") {
        let server = entry.key.trim_end_matches(".command");
        argvs.insert(
            entries
                .iter()
                .filter(|e| e.file == entry.file && e.key.starts_with(&format!("{server}.args[")))
                .map(|e| e.value.clone())
                .collect(),
        );
    }
    assert_eq!(argvs.len(), 3, "distinct shipped uvx argvs");
    let mut argv_mutants = 0;
    for argv in &argvs {
        let render = |args: &[String]| {
            args.iter()
                .map(|arg| format!("\"{}\"", escape_json_string(arg)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (problems, _) = problems_for(&[(
            ".mcp.json",
            format!(
                r#"{{"mcpServers": {{"s": {{"type": "stdio", "command": "uvx", "args": [{}]}}}}}}"#,
                render(argv)
            ),
        )]);
        assert!(problems.is_empty(), "control {argv:?}: {problems:#?}");
        let mut variants: Vec<Vec<String>> = Vec::new();
        for (at, arg) in argv.iter().enumerate() {
            for mutant in mutants(arg) {
                let mut variant = argv.clone();
                variant[at] = mutant;
                variants.push(variant);
            }
        }
        argv_mutants += variants.len();
        let servers: Vec<String> = variants
            .iter()
            .enumerate()
            .map(|(index, variant)| {
                format!(
                    r#""s{index}": {{"type": "stdio", "command": "uvx", "args": [{}]}}"#,
                    render(variant)
                )
            })
            .collect();
        let file = format!(r#"{{"mcpServers": {{{}}}}}"#, servers.join(", "));
        let (problems, references) = problems_for(&[(".mcp.json", file)]);
        assert_eq!(references, 0);
        let rejected = rejected_indices(&problems, "mcpServers.s");
        let accepted: Vec<&Vec<String>> = (0..variants.len())
            .filter(|index| !rejected.contains(index))
            .map(|index| &variants[index])
            .collect();
        assert!(
            accepted.is_empty(),
            "{} uvx argv mutants accepted, e.g. {:?}",
            accepted.len(),
            &accepted[..accepted.len().min(3)]
        );
    }
    assert!(argv_mutants > 10_000, "only {argv_mutants} argv mutants");
}

/// Runtime proof that the round-3 forms are real bypasses and that the
/// guard's missing-binary deny contract holds only for the canonical
/// launcher: with no binary staged, the canonical guard prints the deny and
/// no reproduction does; with an executable marker at `libexec/guard`, the
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

    let marker = plugin.join("libexec/guard");
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

/// The real repository check matches each inventory string to exactly the
/// template the inventory names (the matcher accepts a string only when one
/// template matches), and accepts nothing else of the scanned `command` /
/// `args` strings.
#[test]
fn shipped_strings_match_their_inventory_templates() {
    let root = repo_root();
    let declaration = Declaration::load(&root.join(DECLARATION_PATH)).expect("declaration");
    let report = check_launchers(&root, &root.join("plugins"), &declaration);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    let matched: BTreeSet<Entry> = report
        .accepted
        .iter()
        .filter_map(|accepted| {
            let (file, key) = accepted.location.split_once(": ")?;
            let launcher = key.ends_with(".command") || key.contains(".args[");
            launcher.then(|| Entry {
                file: file.to_string(),
                key: key.to_string(),
                value: accepted.value.clone(),
                template: accepted.template.to_string(),
            })
        })
        .collect();
    let listed: BTreeSet<Entry> = inventory().into_iter().collect();
    assert_eq!(matched, listed);
}
