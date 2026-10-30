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

/// #5023: a symlinked plugin directory, `bin/` root, platform directory or
/// manifest is refused by verify (partial and complete) and by stage, before
/// anything is read through it or written into it.
#[cfg(unix)]
mod symlinks {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    fn staged() -> Fixture {
        let fixture = Fixture::new();
        fixture
            .stage(&Platform::ALL, &provenance(COMMIT, false), false)
            .expect("stage");
        assert!(
            fixture.verify(true).is_empty(),
            "{:#?}",
            fixture.verify(true)
        );
        fixture
    }

    /// Moves `path` to `outside` and leaves an absolute symlink behind.
    fn relink(path: &Path, outside: &Path) {
        fs::rename(path, outside).expect("move out");
        symlink(outside, path).expect("symlink");
    }

    fn assert_refused(fixture: &Fixture, needle: &str) {
        for complete in [false, true] {
            let problems = fixture.verify(complete);
            assert_flags(&problems, needle);
            assert_flags(&problems, "symlink");
        }
    }

    #[test]
    fn a_symlinked_bin_root_is_refused() {
        let fixture = staged();
        relink(&fixture.bin(), &fixture.dir.join("outside-bin"));
        assert_refused(&fixture, "demo/bin");
    }

    #[test]
    fn a_symlinked_manifest_is_refused() {
        let fixture = staged();
        relink(
            &fixture.bin().join("manifest.json"),
            &fixture.dir.join("outside-manifest.json"),
        );
        assert_refused(&fixture, "manifest.json");
    }

    #[test]
    fn a_symlinked_platform_directory_is_refused() {
        let fixture = staged();
        relink(
            &fixture.bin().join("linux-x86_64"),
            &fixture.dir.join("outside-linux"),
        );
        assert_refused(&fixture, "linux-x86_64");
    }

    #[test]
    fn a_symlinked_plugin_directory_is_refused() {
        let fixture = staged();
        relink(
            &fixture.plugins.join("demo"),
            &fixture.dir.join("outside-demo"),
        );
        assert_refused(&fixture, "plugins/demo");
    }

    #[test]
    fn stage_refuses_to_write_through_symlinks() {
        for clean in [false, true] {
            let fixture = staged();
            let outside = fixture.dir.join("outside-bin");
            relink(&fixture.bin(), &outside);
            let before = fs::read(outside.join("manifest.json")).expect("manifest");
            let error = fixture
                .stage(&[Platform::LinuxX86_64], &provenance(COMMIT, false), clean)
                .expect_err("symlinked bin");
            assert!(error.contains("symlink"), "{error}");
            let error = check_merge(&fixture.plugins, "demo", &provenance(COMMIT, false), clean)
                .expect_err("check_merge");
            assert!(error.contains("symlink"), "{error}");
            assert_eq!(
                fs::read(outside.join("manifest.json")).expect("manifest"),
                before
            );
            assert!(
                outside.join("linux-x86_64/guard").is_file(),
                "clean={clean}"
            );
            assert!(fs::symlink_metadata(fixture.bin())
                .expect("link")
                .file_type()
                .is_symlink());
        }
        let fixture = staged();
        let outside = fixture.dir.join("outside-linux");
        relink(&fixture.bin().join("linux-x86_64"), &outside);
        for clean in [false, true] {
            let error = fixture
                .stage(&[Platform::LinuxX86_64], &provenance(COMMIT, false), clean)
                .expect_err("symlinked platform directory");
            assert!(error.contains("symlink"), "{error}");
            assert!(error.contains("linux-x86_64"), "{error}");
        }
        let leftovers: Vec<_> = fs::read_dir(&outside)
            .expect("outside")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["guard"], "nothing written through the link");
    }
}

/// #5027: clean provenance includes ignored inputs; only explicitly named,
/// ignored generated output directories stay permitted.
mod provenance_of_untracked_inputs {
    use std::process::Command;

    use plugin_stage::stage::source_provenance as read_provenance;

    use super::*;

    fn source_provenance(dir: &Path, rustc: &str) -> Result<Provenance, String> {
        read_provenance(
            dir,
            rustc,
            &[PathBuf::from("target"), PathBuf::from("plugins/demo/bin")],
        )
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=plugin-stage test",
                "-c",
                "user.email=plugin-stage@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> TempDir {
        let dir = temp_dir("provenance");
        write(
            &dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/tool\"]\n",
        );
        write(
            &dir.join("crates/tool/Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(&dir.join("crates/tool/src/main.rs"), "fn main() {}\n");
        write(&dir.join(".gitignore"), "/target/\n/plugins/*/bin/\n");
        write(&dir.join("plugins/demo/README.md"), "demo\n");
        git(&dir, &["init", "--quiet"]);
        git(&dir, &["add", "--all"]);
        git(&dir, &["commit", "--quiet", "-m", "fixture"]);
        dir
    }

    fn dirty(dir: &Path) -> bool {
        source_provenance(dir, "rustc 1.94.1")
            .expect("provenance")
            .source_dirty
    }

    #[test]
    fn untracked_build_inputs_make_the_source_dirty() {
        let dir = repo();
        assert!(!dirty(&dir), "fresh commit is clean");
        // Ignored generated outputs keep the source clean.
        write(&dir.join("plugins/demo/bin/manifest.json"), "{}\n");
        write(&dir.join("target/release/tool"), "x");
        assert!(!dirty(&dir), "ignored outputs");
        for input in [
            "crates/tool/build.rs",
            "crates/tool/src/generated.rs",
            ".cargo/config.toml",
            "build.rs",
            "rust-toolchain.toml",
        ] {
            write(&dir.join(input), "// untracked\n");
            assert!(dirty(&dir), "{input} is an untracked build input");
            fs::remove_file(dir.join(input)).expect("remove");
            assert!(!dirty(&dir), "{input} removed");
        }
        // A tracked modification is still dirty.
        write(&dir.join("crates/tool/src/main.rs"), "fn main() { }\n");
        assert!(dirty(&dir));
    }

    #[test]
    fn ignored_build_inputs_make_the_source_dirty() {
        let dir = repo();
        // Local excludes must not be able to hide build inputs either.
        write(
            &dir.join(".git/info/exclude"),
            "build.rs\n.cargo/\n*.rs\nbuild/\n*.inc\n",
        );
        for input in [
            "crates/tool/build.rs",
            "crates/tool/src/generated.rs",
            ".cargo/config.toml",
            "build/payload.inc",
            "crates/tool/src/payload with spaces.inc",
            "crates/tool/src/payload\nsecond line.inc",
            "crates/tool/target/build.rs",
            "plugins/demo/binary/build.rs",
        ] {
            write(&dir.join(input), "// ignored build input\n");
            assert!(dirty(&dir), "{input:?} is ignored but can affect Cargo");
            fs::remove_file(dir.join(input)).expect("remove");
            assert!(!dirty(&dir), "{input:?} removed");
        }
        // Committed ignore rules are equally unable to establish provenance.
        write(
            &dir.join(".gitignore"),
            "/target/\n/plugins/*/bin/\nbuild.rs\n",
        );
        git(&dir, &["add", ".gitignore"]);
        git(&dir, &["commit", "--quiet", "-m", "ignore build scripts"]);
        write(&dir.join("crates/tool/build.rs"), "fn main() {}\n");
        assert!(dirty(&dir), "committed ignore rules cannot hide build.rs");
    }

    #[test]
    fn ignored_build_inputs_cannot_merge_or_pass_complete_verification() {
        let dir = repo();
        let clean = source_provenance(&dir, "rustc 1.94.1").expect("clean");
        let fixture = Fixture::new();
        fixture.stage(&Platform::ALL, &clean, false).expect("stage");
        assert!(fixture.verify(true).is_empty());
        write(&dir.join(".git/info/exclude"), "build.rs\n");
        write(&dir.join("crates/tool/build.rs"), "fn main() {}\n");
        let ignored = source_provenance(&dir, "rustc 1.94.1").expect("ignored");
        assert!(ignored.source_dirty, "ignored build.rs must record dirty");
        assert_eq!(ignored.source_commit, clean.source_commit);
        let error = check_merge(&fixture.plugins, "demo", &ignored, false)
            .expect_err("dirty inputs cannot merge");
        assert!(error.contains("(dirty)"), "{error}");
        fixture
            .stage(&Platform::ALL, &ignored, true)
            .expect("explicit dirty replacement");
        assert_flags(&fixture.verify(true), "dirty");
    }

    #[test]
    fn generated_output_exemptions_are_exact_and_require_ignored_files() {
        let dir = repo();
        let package_bin = PathBuf::from("packages/example/bin");
        write(&dir.join(".git/info/exclude"), "/packages/example/bin/\n");
        write(&dir.join(&package_bin).join("tool.exe"), "output");
        assert!(dirty(&dir), "an undeclared output directory stays dirty");
        let outputs = [PathBuf::from("target"), package_bin];
        assert!(
            !read_provenance(&dir, "rustc", &outputs)
                .expect("provenance")
                .source_dirty,
            "a declared package output is allowed"
        );
        write(&dir.join(".git/info/exclude"), "");
        assert!(
            read_provenance(&dir, "rustc", &outputs)
                .expect("provenance")
                .source_dirty,
            "output exemptions never suppress ordinary untracked files"
        );
        write(&dir.join(".git/info/exclude"), "/packages/\n");
        write(&dir.join("packages/source.inc"), "ignored build input\n");
        assert!(
            read_provenance(&dir, "rustc", &outputs)
                .expect("provenance")
                .source_dirty,
            "an ignored ancestor cannot hide inputs alongside the output"
        );
    }

    #[test]
    fn a_stage_with_untracked_inputs_cannot_merge_into_a_clean_stage() {
        let dir = repo();
        let plugins = dir.join("plugins");
        let declaration = declaration();
        let file = dir.join("target/out/guard");
        write(&file, "#!/bin/sh\nexit 0\n");
        let built = Built {
            step: BuildStep {
                binary: declaration.binary("demo", "guard").expect("guard").clone(),
                platform: declaration
                    .platform(Platform::LinuxX86_64)
                    .expect("platform")
                    .clone(),
                builder: Builder::Zigbuild,
            },
            file,
        };
        let clean = source_provenance(&dir, "rustc 1.94.1").expect("clean");
        assert!(!clean.source_dirty);
        install(&plugins, "demo", &[built], &clean, false).expect("clean stage");
        assert!(!dirty(&dir), "the staged bin/ is ignored output");
        write(&dir.join("crates/tool/build.rs"), "fn main() {}\n");
        let untracked = source_provenance(&dir, "rustc 1.94.1").expect("untracked");
        assert!(untracked.source_dirty);
        assert_eq!(untracked.source_commit, clean.source_commit);
        let error = check_merge(&plugins, "demo", &untracked, false).expect_err("no merge");
        assert!(error.contains("(dirty)"), "{error}");
    }
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
                text(&output.stderr).contains("unsupported platform"),
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

    /// #5025: an unsupported OS or architecture exits non-zero before any
    /// file lookup, so a planted executable at an unsupported layout path
    /// never runs and the canonical guard denies.
    #[test]
    fn unsupported_platforms_deny_even_with_a_planted_binary() {
        let fixture = Fixture::new();
        fixture
            .stage(&Platform::ALL, &provenance(COMMIT, false), false)
            .expect("stage");
        let allow = "#!/bin/sh\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}'\n";
        for planted in [
            "unsupported-x86_64",
            "linux-unsupported",
            "unsupported-unsupported",
            "windows-unsupported",
            "macos-unsupported",
            "unsupported-aarch64",
        ] {
            let file = fixture.bin().join(planted).join("guard");
            write(&file, allow);
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let plugin_root = fixture.plugins.join("demo");
        let path = fake_uname(&fixture.dir);
        let dispatcher = fixture.bin().join("guard");
        let dispatcher = dispatcher.to_str().expect("utf-8 path");
        for (system, machine) in [
            ("FreeBSD", "x86_64"),
            ("FreeBSD", "amd64"),
            ("Linux", "armv7l"),
            ("Linux", "unsupported"),
            ("SunOS", "sparc64"),
            ("unsupported", "unsupported"),
            ("MINGW32_NT-10.0", "i686"),
            ("Darwin", "ppc"),
            ("", ""),
            ("Linux", "../../x"),
        ] {
            let env = [("FAKE_S", system), ("FAKE_M", machine)];
            let output = guard(&plugin_root, &path, &env);
            assert_eq!(text(&output.stdout), DENY, "{system} {machine}");
            let mut env = vec![("PATH", path.as_str())];
            env.extend_from_slice(&[("FAKE_S", system), ("FAKE_M", machine)]);
            let output = run(dispatcher, &[], &env, "");
            assert_eq!(output.status.code(), Some(127), "{system} {machine}");
            assert!(
                !text(&output.stdout).contains("allow"),
                "{system} {machine}"
            );
            assert!(
                text(&output.stderr).contains("unsupported platform"),
                "{system} {machine}: {}",
                text(&output.stderr)
            );
        }
        // Windows ARM64 keeps running the x86_64 build under emulation.
        let output = guard(
            &plugin_root,
            &path,
            &[("FAKE_S", "MINGW64_NT-10.0-26100"), ("FAKE_M", "aarch64")],
        );
        assert_eq!(text(&output.stdout), "windows-x86_64|{}|");
    }
}
