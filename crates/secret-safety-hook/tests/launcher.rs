//! The `hooks.json` launcher must fail closed. These tests execute the
//! registered command with `sh`, exactly as a host would, against stub and
//! real binaries placed under a throwaway plugin root.
//!
//! Integration round 20 (#5019) lands this crate with the plugin registration
//! switch held: the shipped `plugins/aws-core/.../hooks/hooks.json` keeps the
//! Python launcher until the binary packaging slice (#4487) stages the binary.
//! The Rust launcher registration is kept byte-for-byte as reviewed (#4488) in
//! `launcher/hooks.json`; the fail-closed tests below run against it. #4487
//! activates it by copying that file over the plugin `hooks.json`, pointing
//! `HOOKS_JSON` back at the plugin file and deleting the held-registration test.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::thread;

use toolkit_core::{parse_json, Json};

const BINARY: &str = env!("CARGO_BIN_EXE_secret-safety");
/// The staged Rust launcher registration (activated by #4487).
const HOOKS_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/launcher/hooks.json");
/// The registration the plugin ships today (held on the Python launcher).
const LIVE_HOOKS_JSON: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json"
);
const LIVE_PYTHON_HOOK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../plugins/aws-core/com.anthropic.claude-code/hooks/secret-safety.py"
);
const HOOK_DIR: &str = "com.anthropic.claude-code/hooks";
const SECRET_INPUT: &str =
    "{\"tool_name\": \"Bash\", \"tool_input\": {\"command\": \"aws secretsmanager get-secret-value --secret-id demo\"}}";
const SAFE_INPUT: &str = "{\"tool_name\": \"Bash\", \"tool_input\": {\"command\": \"ls\"}}";

/// Tests write and execute files; run them one at a time so a freshly written
/// executable is never held open by a concurrently forked child.
static SERIAL: Mutex<()> = Mutex::new(());

struct Registration {
    matcher: String,
    command: String,
    timeout: i64,
}

fn registrations() -> Vec<Registration> {
    registrations_in(HOOKS_JSON)
}

fn registrations_in(path: &str) -> Vec<Registration> {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let root =
        parse_json(&text).unwrap_or_else(|error| panic!("{path} is not valid JSON: {error}"));
    let Some(Json::Array(entries)) = root.get("hooks").and_then(|hooks| hooks.get("PreToolUse"))
    else {
        panic!("hooks.PreToolUse must be an array");
    };
    let mut found = Vec::new();
    for entry in entries {
        let matcher = entry
            .get("matcher")
            .and_then(Json::as_str)
            .expect("matcher")
            .to_string();
        let Some(Json::Array(hooks)) = entry.get("hooks") else {
            panic!("entry hooks must be an array");
        };
        for hook in hooks {
            assert_eq!(hook.get("type").and_then(Json::as_str), Some("command"));
            let Some(Json::Int(timeout)) = hook.get("timeout") else {
                panic!("timeout must be an integer");
            };
            found.push(Registration {
                matcher: matcher.clone(),
                command: hook
                    .get("command")
                    .and_then(Json::as_str)
                    .expect("command")
                    .to_string(),
                timeout: *timeout,
            });
        }
    }
    found
}

struct PluginRoot {
    root: PathBuf,
}

impl PluginRoot {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("secret-safety-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(HOOK_DIR)).expect("create plugin root");
        Self { root }
    }

    fn binary(&self) -> PathBuf {
        self.root.join(HOOK_DIR).join("secret-safety")
    }

    fn install_script(&self, body: &str, mode: u32) {
        let path = self.binary();
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod stub");
    }

    fn install_real_binary(&self) {
        let path = self.binary();
        fs::copy(BINARY, &path).expect("copy real binary");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod binary");
    }
}

impl Drop for PluginRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_command(command: &str, plugin_root: Option<&Path>, input: &str) -> (Option<i32>, String) {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match plugin_root {
        Some(root) => {
            process.env("CLAUDE_PLUGIN_ROOT", root);
        }
        None => {
            process.env_remove("CLAUDE_PLUGIN_ROOT");
        }
    }
    let mut child = process.spawn().expect("spawn sh");
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let owned = input.as_bytes().to_vec();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });
    let output = child.wait_with_output().expect("wait for sh");
    writer.join().expect("writer thread");
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
    )
}

fn assert_launcher_deny(code: Option<i32>, stdout: &str, scenario: &str) {
    assert_eq!(code, Some(0), "{scenario}: the launcher itself exits 0");
    let parsed =
        parse_json(stdout).unwrap_or_else(|error| panic!("{scenario}: {error}: {stdout:?}"));
    let output = parsed.get("hookSpecificOutput").expect("wrapper object");
    assert_eq!(
        output.get("hookEventName").and_then(Json::as_str),
        Some("PreToolUse"),
        "{scenario}"
    );
    assert_eq!(
        output.get("permissionDecision").and_then(Json::as_str),
        Some("deny"),
        "{scenario}"
    );
    assert!(
        output
            .get("permissionDecisionReason")
            .and_then(Json::as_str)
            .is_some_and(|reason| !reason.is_empty()),
        "{scenario}"
    );
}

#[test]
fn registrations_keep_matchers_and_timeouts_and_drop_python() {
    let found = registrations();
    let matchers: Vec<&str> = found.iter().map(|r| r.matcher.as_str()).collect();
    assert_eq!(
        matchers,
        ["Bash", "use_aws|mcp__aws.*|mcp__plugin_.*aws-mcp.*"]
    );
    for registration in &found {
        assert_eq!(registration.timeout, 5);
        let command = registration.command.to_lowercase();
        assert!(!command.contains("python"), "{command}");
        assert!(!command.contains("py -3"), "{command}");
        assert!(!command.contains(".py"), "{command}");
        assert!(
            registration
                .command
                .contains("${CLAUDE_PLUGIN_ROOT}/com.anthropic.claude-code/hooks/secret-safety"),
            "{command}"
        );
        assert!(
            registration.command.contains("permissionDecision"),
            "{command}"
        );
    }
}

/// While the switch is held (#5019, until #4487), the shipped plugin must keep
/// guarding the same tools with the same timeout through the Python launcher,
/// and the Python hook it launches must still be present: holding the switch
/// never removes the protection.
#[test]
fn the_live_registration_is_held_on_the_python_launcher_until_activation() {
    let staged = registrations();
    let live = registrations_in(LIVE_HOOKS_JSON);
    let matchers = |found: &[Registration]| -> Vec<String> {
        found.iter().map(|r| r.matcher.clone()).collect()
    };
    assert_eq!(matchers(&live), matchers(&staged));
    for registration in &live {
        assert_eq!(registration.timeout, 5);
        assert!(
            registration.command.contains(
                "\"${CLAUDE_PLUGIN_ROOT}/com.anthropic.claude-code/hooks/secret-safety.py\""
            ),
            "{}",
            registration.command
        );
    }
    let python = fs::metadata(LIVE_PYTHON_HOOK).expect("the held Python hook is shipped");
    assert!(python.is_file() && python.len() > 0);
}

#[test]
fn missing_binary_denies() {
    let _guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let plugin = PluginRoot::new("missing");
    for registration in registrations() {
        for input in [SAFE_INPUT, SECRET_INPUT, ""] {
            let (code, stdout) =
                run_command(&registration.command, Some(plugin.root.as_path()), input);
            assert_launcher_deny(code, &stdout, "binary missing");
        }
    }
}

#[test]
fn unset_plugin_root_denies() {
    let _guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    for registration in registrations() {
        let (code, stdout) = run_command(&registration.command, None, SAFE_INPUT);
        assert_launcher_deny(code, &stdout, "CLAUDE_PLUGIN_ROOT unset");
    }
}

#[test]
fn failing_binary_denies() {
    let _guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let plugin = PluginRoot::new("failing");
    let scenarios: [(&str, &str, u32); 5] = [
        ("exit status 3", "cat > /dev/null; exit 3", 0o755),
        (
            "exit status 1 with allow-looking empty output",
            "exit 1",
            0o755,
        ),
        ("killed by signal", "kill -9 $$", 0o755),
        ("not executable", "exit 0", 0o644),
        ("output then failure", "printf '{}'; exit 2", 0o755),
    ];
    for (scenario, body, mode) in scenarios {
        plugin.install_script(body, mode);
        for registration in registrations() {
            let (code, stdout) = run_command(
                &registration.command,
                Some(plugin.root.as_path()),
                SAFE_INPUT,
            );
            assert_launcher_deny(code, &stdout, scenario);
        }
    }
}

#[test]
fn a_directory_at_the_binary_path_denies() {
    let _guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let plugin = PluginRoot::new("directory");
    fs::create_dir_all(plugin.binary()).expect("directory at binary path");
    for registration in registrations() {
        let (code, stdout) = run_command(
            &registration.command,
            Some(plugin.root.as_path()),
            SAFE_INPUT,
        );
        assert_launcher_deny(code, &stdout, "directory instead of binary");
    }
}

#[test]
fn successful_binary_output_is_passed_through() {
    let _guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let plugin = PluginRoot::new("passthrough");
    plugin.install_script("cat > /dev/null", 0o755);
    for registration in registrations() {
        let (code, stdout) = run_command(
            &registration.command,
            Some(plugin.root.as_path()),
            SAFE_INPUT,
        );
        assert_eq!((code, stdout.as_str()), (Some(0), ""), "allow stays silent");
    }
    plugin.install_script("cat > /dev/null; printf '%s' 'sentinel-from-binary'", 0o755);
    for registration in registrations() {
        let (code, stdout) = run_command(
            &registration.command,
            Some(plugin.root.as_path()),
            SAFE_INPUT,
        );
        assert_eq!(
            (code, stdout.as_str()),
            (Some(0), "sentinel-from-binary"),
            "stdout is forwarded unchanged"
        );
    }
}

#[test]
fn real_binary_through_the_launcher_allows_and_denies() {
    let _guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let plugin = PluginRoot::new("real");
    plugin.install_real_binary();
    for registration in registrations() {
        let (code, stdout) = run_command(
            &registration.command,
            Some(plugin.root.as_path()),
            SAFE_INPUT,
        );
        assert_eq!(
            (code, stdout.as_str()),
            (Some(0), ""),
            "safe call is allowed"
        );
        let (code, stdout) = run_command(
            &registration.command,
            Some(plugin.root.as_path()),
            SECRET_INPUT,
        );
        assert_eq!(code, Some(0));
        let parsed = parse_json(&stdout).expect("deny JSON");
        let reason = parsed
            .get("hookSpecificOutput")
            .and_then(|output| output.get("permissionDecisionReason"))
            .and_then(Json::as_str)
            .expect("reason");
        assert!(
            reason.starts_with("Direct secret fetching is blocked."),
            "{reason}"
        );
        let (code, stdout) = run_command(
            &registration.command,
            Some(plugin.root.as_path()),
            "garbage",
        );
        assert_launcher_deny(code, &stdout, "real binary, unvalidatable input");
    }
}
