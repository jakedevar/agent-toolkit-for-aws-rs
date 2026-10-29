//! Staging (#4487) without a compiler: stand-in "binaries" are installed
//! into a temporary plugins tree through the real `install` path, then
//! verified, tampered with and, on Unix, executed through the generated
//! dispatcher and the canonical fail-closed guard launcher under a faked
//! `uname` for every platform.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{temp_dir, write, TempDir};
use plugin_stage::launcher::{dispatcher_script, guard_hook_command};
use plugin_stage::manifest::verify_staged;
use plugin_stage::stage::{check_merge, install, BuildStep, Builder, Built, Provenance};
use plugin_stage::{Declaration, Platform};

const DECLARATION: &str = r##"{
  "schema": 1,
  "platforms": [
    {"platform": "linux-x86_64", "triple": "x86_64-unknown-linux-gnu", "glibc": "2.17"},
    {"platform": "linux-aarch64", "triple": "aarch64-unknown-linux-gnu", "glibc": "2.17"},
    {"platform": "macos-x86_64", "triple": "x86_64-apple-darwin"},
    {"platform": "macos-aarch64", "triple": "aarch64-apple-darwin"},
    {"platform": "windows-x86_64", "triple": "x86_64-pc-windows-gnu"}
  ],
  "binaries": [
    {"plugin": "demo", "name": "guard", "package": "g", "bin": "guard",
     "role": "guard-hook", "deny_reason": "Guard unavailable. Reinstall demo.", "consumer": "#1"}
  ]
}"##;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn declaration() -> Declaration {
    Declaration::parse(DECLARATION).expect("declaration")
}

fn provenance(commit: &str, dirty: bool) -> Provenance {
    Provenance {
        source_commit: commit.to_string(),
        source_dirty: dirty,
        rustc: "rustc 1.94.1".to_string(),
    }
}

struct Fixture {
    dir: TempDir,
    plugins: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = temp_dir("staging");
        let plugins = dir.join("plugins");
        write(&plugins.join("demo/README.md"), "demo\n");
        Self { dir, plugins }
    }

    fn bin(&self) -> PathBuf {
        self.plugins.join("demo/bin")
    }

    /// Builds a stand-in output for `platform`: a shell script that names
    /// its platform, echoes stdin and exits with `$GUARD_EXIT` (default 0).
    fn built(&self, platform: Platform) -> Built {
        let declaration = declaration();
        let file = self.dir.join(format!("out/{}/guard", platform.key()));
        write(
            &file,
            &format!(
                "#!/bin/sh\nprintf '%s|' {key}\ncat\nprintf '|%s' \"$*\"\nexit \"${{GUARD_EXIT:-0}}\"\n",
                key = platform.key()
            ),
        );
        Built {
            step: BuildStep {
                binary: declaration.binary("demo", "guard").expect("guard").clone(),
                platform: declaration.platform(platform).expect("platform").clone(),
                builder: Builder::Zigbuild,
            },
            file,
        }
    }

    fn stage(
        &self,
        platforms: &[Platform],
        provenance: &Provenance,
        clean: bool,
    ) -> Result<(), String> {
        let built: Vec<Built> = platforms
            .iter()
            .map(|platform| self.built(*platform))
            .collect();
        install(&self.plugins, "demo", &built, provenance, clean).map(|_| ())
    }

    fn verify(&self, complete: bool) -> Vec<String> {
        verify_staged(&self.dir, &self.plugins, &declaration(), complete)
    }
}

#[test]
fn installed_binaries_verify_and_complete_needs_every_platform() {
    let fixture = Fixture::new();
    fixture
        .stage(&[Platform::LinuxX86_64], &provenance(COMMIT, false), false)
        .expect("stage linux");
    assert!(
        fixture.verify(false).is_empty(),
        "{:#?}",
        fixture.verify(false)
    );
    let missing = fixture.verify(true);
    assert_eq!(missing.len(), 4, "{missing:#?}");
    assert!(missing
        .iter()
        .all(|p| p.contains("guard is not staged for")));

    // A second stage of the same clean commit merges into the manifest.
    fixture
        .stage(&Platform::ALL[1..], &provenance(COMMIT, false), false)
        .expect("stage the rest");
    assert!(
        fixture.verify(true).is_empty(),
        "{:#?}",
        fixture.verify(true)
    );
    let bin = fixture.bin();
    assert!(bin.join("guard.exe").is_file());
    assert!(bin.join("macos-aarch64/guard").is_file());
    assert_eq!(
        fs::read_to_string(bin.join("guard")).expect("dispatcher"),
        dispatcher_script("guard")
    );
    let manifest = fs::read_to_string(bin.join("manifest.json")).expect("manifest");
    assert!(manifest.contains(COMMIT));
    assert!(manifest.contains("\"builder\": \"cargo-zigbuild\""));
}

#[test]
fn stages_from_different_or_dirty_sources_do_not_merge() {
    let fixture = Fixture::new();
    fixture
        .stage(&[Platform::LinuxX86_64], &provenance(COMMIT, false), false)
        .expect("first stage");
    let other = provenance(&"f".repeat(40), false);
    let error = check_merge(&fixture.plugins, "demo", &other, false).expect_err("other commit");
    assert!(error.contains("--clean"), "{error}");
    let error = fixture
        .stage(&[Platform::LinuxAarch64], &provenance(COMMIT, true), false)
        .expect_err("dirty");
    assert!(error.contains("(dirty)"), "{error}");
    fixture
        .stage(&[Platform::LinuxAarch64], &other, true)
        .expect("clean replaces");
    let manifest = fs::read_to_string(fixture.bin().join("manifest.json")).expect("manifest");
    assert!(!manifest.contains(COMMIT));
    assert!(!fixture.bin().join("linux-x86_64").exists());
    assert!(fixture.verify(false).is_empty());
}

#[test]
fn a_dirty_stage_is_refused_by_the_release_gate() {
    let fixture = Fixture::new();
    fixture
        .stage(&Platform::ALL, &provenance(COMMIT, true), false)
        .expect("dirty stage");
    assert!(fixture.verify(false).is_empty());
    let problems = fixture.verify(true);
    assert!(
        problems.iter().any(|p| p.contains("dirty tree")),
        "{problems:#?}"
    );
}

fn tampered(edit: impl Fn(&Path)) -> Vec<String> {
    let fixture = Fixture::new();
    fixture
        .stage(
            &[Platform::LinuxX86_64, Platform::WindowsX86_64],
            &provenance(COMMIT, false),
            false,
        )
        .expect("stage");
    edit(&fixture.bin());
    fixture.verify(false)
}

fn assert_flags(problems: &[String], needle: &str) {
    assert!(
        problems.iter().any(|problem| problem.contains(needle)),
        "expected {needle:?} in {problems:#?}"
    );
}

#[test]
fn tampering_is_detected() {
    assert_flags(
        &tampered(|bin| {
            fs::write(bin.join("linux-x86_64/guard"), "#!/bin/sh\nexit 0\n").expect("write");
        }),
        "sha256 differs",
    );
    assert_flags(
        &tampered(|bin| fs::remove_file(bin.join("guard.exe")).expect("remove")),
        "guard.exe: is missing",
    );
    assert_flags(
        &tampered(|bin| fs::write(bin.join("extra"), "x").expect("write")),
        "extra: not listed",
    );
    assert_flags(
        &tampered(|bin| fs::create_dir_all(bin.join("scratch")).expect("mkdir")),
        "scratch: unexpected directory",
    );
    assert_flags(
        &tampered(|bin| fs::write(bin.join("guard"), "#!/bin/sh\nexit 0\n").expect("write")),
        "differs from the generated dispatcher",
    );
    assert_flags(
        &tampered(|bin| fs::remove_file(bin.join("manifest.json")).expect("remove")),
        "has no manifest.json",
    );
    assert_flags(
        &tampered(|bin| {
            let path = bin.join("manifest.json");
            let text = fs::read_to_string(&path).expect("read");
            fs::write(
                &path,
                text.replace("\"package\": \"g\"", "\"package\": \"other\""),
            )
            .expect("write");
        }),
        "but the declaration says",
    );
}

#[cfg(unix)]
#[test]
fn a_binary_without_the_executable_bit_is_flagged() {
    use std::os::unix::fs::PermissionsExt;
    let problems = tampered(|bin| {
        fs::set_permissions(
            bin.join("linux-x86_64/guard"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("chmod");
    });
    assert_flags(&problems, "is not executable");
    let problems = tampered(|bin| {
        fs::set_permissions(bin.join("guard"), fs::Permissions::from_mode(0o644)).expect("chmod");
    });
    assert_flags(&problems, "guard: is not executable");
}

#[test]
fn an_undeclared_plugin_may_not_have_a_bin_directory() {
    let fixture = Fixture::new();
    write(&fixture.plugins.join("other/bin/tool"), "x");
    let problems = fixture.verify(false);
    assert_flags(&problems, "plugin other declares no binaries");
}

#[cfg(unix)]
mod sh {
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};

    use super::*;

    const REASON: &str = "Guard unavailable. Reinstall demo.";
    const DENY: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Guard unavailable. Reinstall demo."}}"#;

    /// A `uname` that reports `$FAKE_S` / `$FAKE_M`, first on PATH.
    fn fake_uname(dir: &Path) -> String {
        let fakebin = dir.join("fakebin");
        write(
            &fakebin.join("uname"),
            "#!/bin/sh\ncase \"$1\" in -s) echo \"$FAKE_S\" ;; -m) echo \"$FAKE_M\" ;; esac\n",
        );
        fs::set_permissions(fakebin.join("uname"), fs::Permissions::from_mode(0o755))
            .expect("chmod");
        format!(
            "{}:{}",
            fakebin.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn run(program: &str, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("wait")
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn the_dispatcher_runs_the_binary_for_each_platform() {
        let fixture = Fixture::new();
        fixture
            .stage(&Platform::ALL, &provenance(COMMIT, false), false)
            .expect("stage");
        let path = fake_uname(&fixture.dir);
        let dispatcher = fixture.bin().join("guard");
        let dispatcher = dispatcher.to_str().expect("utf-8 path");
        let cases = [
            ("Linux", "x86_64", "linux-x86_64"),
            ("Linux", "aarch64", "linux-aarch64"),
            ("Darwin", "x86_64", "macos-x86_64"),
            ("Darwin", "arm64", "macos-aarch64"),
            ("MINGW64_NT-10.0-26100", "x86_64", "windows-x86_64"),
            ("MSYS_NT-10.0", "aarch64", "windows-x86_64"),
            ("CYGWIN_NT-10.0", "x86_64", "windows-x86_64"),
        ];
        for (system, machine, expected) in cases {
            let output = run(
                dispatcher,
                &["one", "two"],
                &[("PATH", &path), ("FAKE_S", system), ("FAKE_M", machine)],
                "{\"tool_name\":\"Bash\"}",
            );
            assert!(
                output.status.success(),
                "{system} {machine}: {}",
                text(&output.stderr)
            );
            assert_eq!(
                text(&output.stdout),
                format!("{expected}|{{\"tool_name\":\"Bash\"}}|one two"),
                "{system} {machine}"
            );
        }
        for (system, machine) in [
            ("Linux", "armv7l"),
            ("FreeBSD", "amd64"),
            ("SunOS", "i86pc"),
        ] {
            let output = run(
                dispatcher,
                &[],
                &[("PATH", &path), ("FAKE_S", system), ("FAKE_M", machine)],
                "",
            );
            assert_eq!(output.status.code(), Some(127), "{system} {machine}");
            assert!(
                text(&output.stderr).contains("no staged binary for"),
                "{}",
                text(&output.stderr)
            );
        }
    }

    /// Runs the canonical guard launcher the way a host runs a hook command.
    fn guard(plugin_root: &Path, path: &str, extra: &[(&str, &str)]) -> Output {
        let command = guard_hook_command("CLAUDE_PLUGIN_ROOT", "guard", REASON);
        let root = plugin_root.to_str().expect("utf-8 path");
        let mut env = vec![
            ("PATH", path),
            ("CLAUDE_PLUGIN_ROOT", root),
            ("FAKE_S", "Linux"),
            ("FAKE_M", "x86_64"),
        ];
        env.extend_from_slice(extra);
        run("sh", &["-c", &command], &env, "{}")
    }

    #[test]
    fn the_guard_launcher_forwards_the_binary_and_fails_closed() {
        let fixture = Fixture::new();
        let plugin_root = fixture.plugins.join("demo");
        let path = fake_uname(&fixture.dir);

        // Nothing staged: deny.
        let output = guard(&plugin_root, &path, &[]);
        assert!(output.status.success());
        assert_eq!(text(&output.stdout), DENY);

        // Staged for this platform: the binary's output is forwarded.
        fixture
            .stage(&[Platform::LinuxX86_64], &provenance(COMMIT, false), false)
            .expect("stage");
        let output = guard(&plugin_root, &path, &[]);
        assert_eq!(text(&output.stdout), "linux-x86_64|{}|");

        // The binary fails: deny.
        let output = guard(&plugin_root, &path, &[("GUARD_EXIT", "3")]);
        assert_eq!(text(&output.stdout), DENY);

        // Staged, but not for the running platform: deny.
        let output = guard(&plugin_root, &path, &[("FAKE_M", "aarch64")]);
        assert_eq!(text(&output.stdout), DENY);

        // Binary present but not executable: deny.
        fs::set_permissions(
            fixture.bin().join("linux-x86_64/guard"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("chmod");
        let output = guard(&plugin_root, &path, &[]);
        assert_eq!(text(&output.stdout), DENY);

        // Plugin root unset: deny.
        let command = guard_hook_command("CLAUDE_PLUGIN_ROOT", "guard", REASON);
        let output = Command::new("sh")
            .args(["-c", &command])
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .stdin(Stdio::null())
            .output()
            .expect("run");
        assert_eq!(text(&output.stdout), DENY);
    }
}
