//! `OpenClaw` B4 (#4411, design section 10.1): text gates over the package's
//! TypeScript runtime. They live beside the plugin-stage tests because the
//! platform map they compare with is plugin-stage's.
//!
//! - **T-PKG-LAYOUT**: the fixed binary table in `src/bridge.ts` equals the
//!   staged layout (`Platform::staged_path`) and the generated dispatcher's
//!   platform mapping, so the runtime and the dispatcher cannot drift.
//! - **T-PKG-TS**: no Python, venv or legacy bridge name in `src/*.ts`,
//!   `package.json` or `scripts/stage-skill.mjs`; one `spawn`, with
//!   `shell: false` and the fixed `["agents-pay", "bridge"]` argv; package
//!   and lockfile identity 1.1.0 with `bin` shipped.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use plugin_stage::launcher::dispatcher_script;
use plugin_stage::{Declaration, Platform, Role, DECLARATION_PATH};
use toolkit_core::Json;

const PACKAGE: &str = "plugins/aws-agents/skills/agents-pay/packages/openclaw";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn package_file(relative: &str) -> String {
    let path = repo_root().join(PACKAGE).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every `src/*.ts` file, by file name.
fn sources() -> BTreeMap<String, String> {
    let dir = repo_root().join(PACKAGE).join("src");
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(&dir).expect("src") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("ts") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            files.insert(name, fs::read_to_string(&path).expect("source"));
        }
    }
    assert!(files.contains_key("bridge.ts"), "{files:?}");
    files
}

fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or_else(|| panic!("unquoted {text:?}"))
        .to_owned()
}

/// The `"<platform>/<arch>": "<path>"` entries of `BRIDGE_BINARIES`.
fn bridge_table(source: &str) -> BTreeMap<String, String> {
    let mut lines = source
        .lines()
        .skip_while(|line| !line.contains("const BRIDGE_BINARIES"));
    let header = lines
        .next()
        .expect("BRIDGE_BINARIES table in src/bridge.ts");
    assert!(header.trim_end().ends_with('{'), "{header}");
    let mut table = BTreeMap::new();
    for line in lines {
        let line = line.trim();
        if line.starts_with('}') {
            return table;
        }
        let entry = line.strip_suffix(',').unwrap_or(line);
        let (key, value) = entry
            .split_once(": ")
            .unwrap_or_else(|| panic!("table line {line:?}"));
        let previous = table.insert(unquote(key), unquote(value));
        assert!(previous.is_none(), "duplicate {key}");
    }
    panic!("BRIDGE_BINARIES table is not closed");
}

fn node_platform(platform: Platform) -> &'static str {
    match platform.os() {
        "linux" => "linux",
        "macos" => "darwin",
        _ => "win32",
    }
}

fn node_arch(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => panic!("unmapped arch {other}"),
    }
}

fn version(value: &Json) -> Option<&str> {
    value.get("version").and_then(Json::as_str)
}

#[test]
fn t_pkg_layout_bridge_table_equals_the_staged_layout_and_dispatcher() {
    let root = repo_root();
    let declaration = Declaration::load(&root.join(DECLARATION_PATH)).expect("declaration");
    let entries: Vec<_> = declaration.package_binaries(PACKAGE).collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, Role::Subprocess);
    let name = entries[0].name.as_str();

    let mut expected = BTreeMap::new();
    for platform in Platform::ALL {
        let key = format!("{}/{}", node_platform(platform), node_arch(platform.arch()));
        expected.insert(key, format!("bin/{}", platform.staged_path(name)));
    }
    // Windows on ARM64 runs the x86_64 binary through emulation: the
    // dispatcher maps every Windows architecture to bin/<name>.exe.
    expected.insert(
        "win32/arm64".to_owned(),
        format!("bin/{}", Platform::WindowsX86_64.staged_path(name)),
    );
    assert_eq!(expected.len(), 6);

    let sources = sources();
    assert_eq!(bridge_table(&sources["bridge.ts"]), expected);

    let dispatcher = dispatcher_script(name);
    let select = format!(
        "if [ \"$o\" = windows ]; then b=\"$d/{name}.exe\"; else b=\"$d/$o-$a/{name}\"; fi"
    );
    for line in [
        "Linux) o=linux ;;",
        "Darwin) o=macos ;;",
        "MINGW*|MSYS*|CYGWIN*) o=windows ;;",
        "x86_64|amd64|x64) a=x86_64 ;;",
        "aarch64|arm64) a=aarch64 ;;",
        select.as_str(),
    ] {
        assert!(dispatcher.contains(line), "dispatcher lacks {line:?}");
    }
    for platform in Platform::ALL {
        let staged = platform.staged_path(name);
        if platform.is_windows() {
            assert_eq!(staged, format!("{name}.exe"));
        } else {
            assert_eq!(
                staged,
                format!("{}-{}/{name}", platform.os(), platform.arch())
            );
        }
    }
}

#[test]
fn t_pkg_ts_runtime_names_no_python_and_spawns_one_fixed_argv() {
    let sources = sources();
    let mut gated: Vec<(String, String)> = sources
        .iter()
        .map(|(name, text)| (format!("src/{name}"), text.clone()))
        .collect();
    gated.push(("package.json".to_owned(), package_file("package.json")));
    gated.push((
        "scripts/stage-skill.mjs".to_owned(),
        package_file("scripts/stage-skill.mjs"),
    ));
    for (name, text) in &gated {
        let lower = text.to_ascii_lowercase();
        for forbidden in [
            "python",
            ".venv",
            "agentcore_bridge",
            "requirements.txt",
            ".py\"",
        ] {
            assert!(!lower.contains(forbidden), "{name} names {forbidden:?}");
        }
    }

    let count =
        |needle: &str| -> usize { sources.values().map(|t| t.matches(needle).count()).sum() };
    let bridge = &sources["bridge.ts"];
    assert_eq!(count("spawn("), 1, "exactly one spawn call");
    assert!(bridge.contains("spawn("), "the spawn call is in bridge.ts");
    assert_eq!(count("shell: false"), 1);
    assert_eq!(count("shell: true"), 0);
    assert_eq!(count("[\"agents-pay\", \"bridge\"]"), 1);
    assert!(bridge.contains("[\"agents-pay\", \"bridge\"]"));
    for forbidden in [
        "exec(",
        "execFile(",
        "execSync(",
        "spawnSync(",
        "fork(",
        "uname",
    ] {
        assert_eq!(count(forbidden), 0, "{forbidden}");
    }
    for (name, text) in &sources {
        if name != "bridge.ts" {
            assert!(!text.contains("node:child_process"), "{name}");
        }
    }
    // The child environment is the inherited one; nothing in it selects
    // the binary.
    assert_eq!(count("process.env"), 1);
    assert!(bridge.contains("env: { ...process.env }"));
}

#[test]
fn t_pkg_ts_package_identity_is_1_1_0_and_ships_bin() {
    let package = toolkit_core::parse_json(&package_file("package.json")).expect("package");
    let lock = toolkit_core::parse_json(&package_file("package-lock.json")).expect("lock");
    assert_eq!(version(&package), Some("1.1.0"));
    assert_eq!(version(&lock), Some("1.1.0"));
    let root = lock
        .get("packages")
        .and_then(|p| p.get(""))
        .expect("lock root");
    assert_eq!(version(root), Some("1.1.0"));
    let Some(Json::Array(files)) = package.get("files") else {
        panic!("files")
    };
    let files: Vec<&str> = files.iter().filter_map(Json::as_str).collect();
    assert!(files.contains(&"bin"), "{files:?}");
    assert!(!files.iter().any(|f| f.contains("runtime")), "{files:?}");
}
