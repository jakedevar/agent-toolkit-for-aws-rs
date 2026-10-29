//! Package destinations (#4411 slice B3, docs/migration/openclaw-bridge-design.md
//! section 6.2): a declaration entry with `package_root` instead of `plugin`
//! stages into `<package_root>/bin/` with the plugin layout, manifest,
//! dispatcher and modes; `verify`, `verify --complete` and `check` cover it;
//! `verify-package` is the npm pack guard that delegates to the same verifier;
//! the launcher contract is unchanged; and no test target stages the real
//! `agents-pay-live` entry.
//!
//! Test ids: T-PKG-DECL, T-PKG-STAGE, T-PKG-VERIFY, T-PKG-PACK, T-PKG-INV,
//! T-PKG-NOLIVE (design section 10.3). No test here builds anything: stand-in
//! "binaries" are installed through the real `install_at` path.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{repo_root, temp_dir, write, TempDir};
use plugin_stage::declaration::check_package_root;
use plugin_stage::launcher::{check_launchers, dispatcher_script, launcher_snippet};
use plugin_stage::manifest::{tracked_package_files, verify_package, verify_staged};
use plugin_stage::stage::{install_at, BuildStep, Builder, Built, Provenance};
use plugin_stage::{Declaration, Destination, Platform, Role, DECLARATION_PATH};

const ROOT: &str = "pkgs/demo";

const PLATFORMS: &str = r#""platforms": [
    {"platform": "linux-x86_64", "triple": "x86_64-unknown-linux-gnu", "glibc": "2.17"},
    {"platform": "linux-aarch64", "triple": "aarch64-unknown-linux-gnu", "glibc": "2.17"},
    {"platform": "macos-x86_64", "triple": "x86_64-apple-darwin"},
    {"platform": "macos-aarch64", "triple": "aarch64-apple-darwin"},
    {"platform": "windows-x86_64", "triple": "x86_64-pc-windows-gnu"}
  ]"#;

const ENTRY: &str = r##"{"package_root": "pkgs/demo", "name": "toolkit", "package": "toolkit-cli", "bin": "toolkit", "features": ["agents-pay-live"], "role": "subprocess", "consumer": "#4411"}"##;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A fixture mutation for a negative case.
type Edit = Box<dyn Fn(&Fixture)>;

fn declaration_text(entries: &[&str]) -> String {
    format!(
        "{{\n  \"schema\": 1,\n  {PLATFORMS},\n  \"binaries\": [\n    {}\n  ]\n}}\n",
        entries.join(",\n    ")
    )
}

fn parse(entries: &[&str]) -> Result<Declaration, String> {
    Declaration::parse(&declaration_text(entries))
}

fn provenance(dirty: bool) -> Provenance {
    Provenance {
        source_commit: COMMIT.to_string(),
        source_dirty: dirty,
        rustc: "rustc 1.94.1".to_string(),
    }
}

fn assert_flags(problems: &[String], needle: &str) {
    assert!(
        problems.iter().any(|problem| problem.contains(needle)),
        "expected {needle:?} in {problems:#?}"
    );
}

/// A temporary release checkout: the declaration, a package destination
/// with `package.json` and a `.gitignore` that ignores `bin/`, and an empty
/// plugins directory.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = temp_dir("package");
        write(&dir.join(DECLARATION_PATH), &declaration_text(&[ENTRY]));
        write(
            &dir.join(ROOT).join("package.json"),
            "{\"name\": \"demo\"}\n",
        );
        write(&dir.join(ROOT).join(".gitignore"), "node_modules/\nbin/\n");
        fs::create_dir_all(dir.join("plugins")).expect("plugins dir");
        Self { dir }
    }

    fn staged() -> Self {
        let fixture = Self::new();
        fixture
            .stage(&Platform::ALL, &provenance(false))
            .expect("stage");
        fixture
    }

    fn package(&self) -> PathBuf {
        self.dir.join(ROOT)
    }

    fn bin(&self) -> PathBuf {
        self.package().join("bin")
    }

    fn declaration(&self) -> Declaration {
        Declaration::load(&self.dir.join(DECLARATION_PATH)).expect("fixture declaration")
    }

    fn built(&self, platform: Platform) -> Built {
        let declaration = self.declaration();
        let binary = declaration
            .package_binaries(ROOT)
            .next()
            .expect("package binary")
            .clone();
        let file = self.dir.join(format!("out/{}/toolkit", platform.key()));
        write(
            &file,
            &format!("#!/bin/sh\nprintf '%s' {}\n", platform.key()),
        );
        Built {
            step: BuildStep {
                binary,
                platform: declaration.platform(platform).expect("platform").clone(),
                builder: Builder::Zigbuild,
            },
            file,
        }
    }

    fn stage(&self, platforms: &[Platform], provenance: &Provenance) -> Result<(), String> {
        let built: Vec<Built> = platforms.iter().map(|p| self.built(*p)).collect();
        install_at(
            &self.package(),
            &Destination::PackageRoot(ROOT.to_string()),
            &built,
            provenance,
            false,
        )
        .map(|_| ())
    }

    fn verify(&self, complete: bool) -> Vec<String> {
        verify_staged(
            &self.dir,
            &self.dir.join("plugins"),
            &self.declaration(),
            complete,
        )
    }

    fn verify_package(&self) -> Vec<String> {
        verify_package(&self.dir, &self.declaration(), &self.package(), true)
    }

    fn edit_manifest(&self, from: &str, to: &str) {
        let path = self.bin().join("manifest.json");
        let text = fs::read_to_string(&path).expect("manifest");
        assert!(text.contains(from), "manifest lacks {from}");
        fs::write(&path, text.replacen(from, to, 1)).expect("write manifest");
    }
}

fn plugin_stage(dir: &Path, args: &[&str], path: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plugin-stage"));
    command.args(args).current_dir(dir);
    if let Some(path) = path {
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", dir)
            .env("LANG", "C");
    }
    command.output().expect("run plugin-stage")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---------------------------------------------------------------- T-PKG-DECL

#[test]
fn t_pkg_decl_an_entry_names_exactly_one_destination() {
    let both = ENTRY.replace(
        "\"package_root\": \"pkgs/demo\"",
        "\"plugin\": \"demo\", \"package_root\": \"pkgs/demo\"",
    );
    let neither = ENTRY.replace("\"package_root\": \"pkgs/demo\", ", "");
    for entry in [both, neither] {
        let error = parse(&[&entry]).expect_err(&entry);
        assert!(
            error.contains("exactly one of `plugin` or `package_root`"),
            "{entry}: {error}"
        );
    }
    let declaration = parse(&[ENTRY]).expect("valid package entry");
    let binary = &declaration.binaries[0];
    assert_eq!(
        binary.destination,
        Destination::PackageRoot(ROOT.to_string())
    );
    assert_eq!(binary.plugin(), None);
    assert_eq!(binary.role, Role::Subprocess);
    assert!(declaration.plugins().is_empty());
    assert_eq!(
        declaration.package_roots().into_iter().collect::<Vec<_>>(),
        [ROOT]
    );
}

#[test]
fn t_pkg_decl_package_root_paths_are_refused() {
    for root in [
        "/abs/pkg",
        "pkgs/../demo",
        "../demo",
        "./pkgs/demo",
        "pkgs/./demo",
        "pkgs//demo",
        "pkgs/demo/",
        "",
        ".",
        "pkgs\\demo",
        "C:/pkgs/demo",
        "pkgs/de mo",
        "pkgs/\u{1b}[31mdemo\nforged",
        "plugins/aws-agents/bin",
        "plugins/aws-agents/bin/pkg",
        "plugins/aws-agents/bin/linux-x86_64/pkg",
    ] {
        let entry = ENTRY.replace(
            "pkgs/demo",
            &root
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\u{1b}', "\\u001b"),
        );
        let error = parse(&[&entry]).expect_err(root);
        assert!(error.contains("package_root"), "{root:?}: {error}");
        assert!(
            !error.contains('\u{1b}') && !error.contains('\n'),
            "diagnostic must be single-line and escaped: {error:?}"
        );
    }
    // Nested inside another package destination's bin/.
    let nested = ENTRY
        .replace("pkgs/demo", "pkgs/demo/bin/inner")
        .replace("\"name\": \"toolkit\"", "\"name\": \"inner\"");
    let error = parse(&[ENTRY, &nested]).expect_err("nested in another bin/");
    assert!(error.contains("bin/"), "{error}");
    // Two entries may share one package destination.
    let second = ENTRY.replace("\"name\": \"toolkit\"", "\"name\": \"helper\"");
    let declaration = parse(&[ENTRY, &second]).expect("two binaries, one destination");
    assert_eq!(declaration.package_binaries(ROOT).count(), 2);
}

#[test]
fn t_pkg_decl_subprocess_is_the_role_of_package_destinations() {
    let plugin_subprocess =
        ENTRY.replace("\"package_root\": \"pkgs/demo\"", "\"plugin\": \"demo\"");
    let error = parse(&[&plugin_subprocess]).expect_err("plugin subprocess");
    assert!(error.contains("subprocess"), "{error}");
    for role in ["mcp-server", "hook"] {
        let entry = ENTRY.replace("\"subprocess\"", &format!("\"{role}\""));
        let error = parse(&[&entry]).expect_err(role);
        assert!(error.contains("subprocess"), "{role}: {error}");
    }
}

#[test]
fn t_pkg_decl_the_destination_must_be_a_real_npm_package_ignoring_bin() {
    let ok = Fixture::new();
    assert!(
        check_package_root(&ok.dir, ROOT).is_empty(),
        "{:#?}",
        check_package_root(&ok.dir, ROOT)
    );

    let cases: Vec<(&str, Edit, &str)> = vec![
        (
            "no package.json",
            Box::new(|f: &Fixture| fs::remove_file(f.package().join("package.json")).expect("rm")),
            "package.json",
        ),
        (
            "no .gitignore",
            Box::new(|f: &Fixture| fs::remove_file(f.package().join(".gitignore")).expect("rm")),
            ".gitignore",
        ),
        (
            ".gitignore without bin/",
            Box::new(|f: &Fixture| {
                fs::write(f.package().join(".gitignore"), "node_modules/\nbinaries/\n").expect("w");
            }),
            "bin/",
        ),
        (
            ".gitignore re-includes bin/",
            Box::new(|f: &Fixture| {
                fs::write(f.package().join(".gitignore"), "bin/\n!bin/\n").expect("w");
            }),
            "bin/",
        ),
        (
            "missing directory",
            Box::new(|f: &Fixture| fs::remove_dir_all(f.package()).expect("rm")),
            "not a directory",
        ),
    ];
    for (label, edit, needle) in cases {
        let fixture = Fixture::new();
        edit(&fixture);
        let problems = check_package_root(&fixture.dir, ROOT);
        assert_flags(&problems, needle);
        // Every verifier entry point reports it.
        for complete in [false, true] {
            let all = fixture.verify(complete);
            for problem in &problems {
                assert!(all.contains(problem), "{label}: {problem} not in {all:#?}");
            }
        }
        assert!(!fixture.verify_package().is_empty(), "{label}");
    }
}

#[cfg(unix)]
#[test]
fn t_pkg_decl_a_symlinked_component_is_refused() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = fixture.dir.join("elsewhere");
    fs::rename(fixture.dir.join("pkgs"), &outside).expect("move");
    symlink(&outside, fixture.dir.join("pkgs")).expect("symlink");
    let problems = check_package_root(&fixture.dir, ROOT);
    assert_flags(&problems, "symlink");
    assert_flags(&fixture.verify(false), "symlink");
    assert_flags(&fixture.verify_package(), "symlink");
}

#[test]
fn t_pkg_decl_launcher_refuses_the_subprocess_role() {
    let fixture = Fixture::new();
    let declaration = fixture.declaration();
    let binary = declaration.package_binaries(ROOT).next().expect("binary");
    let error = launcher_snippet(binary).expect_err("subprocess has no launcher");
    assert!(error.contains("subprocess"), "{error}");
    let root = fixture.dir.to_string_lossy().into_owned();
    let output = plugin_stage(
        &fixture.dir,
        &["launcher", "--root", &root, "pkgs/demo/toolkit"],
        None,
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    assert!(
        stderr(&output).contains("subprocess"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn t_pkg_decl_the_committed_openclaw_entry_is_exact() {
    let root = repo_root();
    let declaration = Declaration::load(&root.join(DECLARATION_PATH)).expect("declaration");
    let openclaw = "plugins/aws-agents/skills/agents-pay/packages/openclaw";
    let entries: Vec<_> = declaration.package_binaries(openclaw).collect();
    assert_eq!(entries.len(), 1, "{entries:#?}");
    let entry = entries[0];
    assert_eq!(entry.name, "toolkit");
    assert_eq!(entry.package, "toolkit-cli");
    assert_eq!(entry.bin, "toolkit");
    assert_eq!(entry.features, ["agents-pay-live"]);
    assert_eq!(entry.role, Role::Subprocess);
    assert_eq!(entry.consumer, "#4411");
    assert!(
        check_package_root(&root, openclaw).is_empty(),
        "{:#?}",
        check_package_root(&root, openclaw)
    );
    let gitignore =
        fs::read_to_string(root.join(openclaw).join(".gitignore")).expect("package .gitignore");
    assert!(gitignore.lines().any(|line| line.trim() == "bin/"));
}

// --------------------------------------------------------------- T-PKG-STAGE

#[test]
fn t_pkg_stage_a_package_destination_uses_the_plugin_layout() {
    let fixture = Fixture::staged();
    let bin = fixture.bin();
    let mut files = Vec::new();
    collect(&bin, "", &mut files);
    files.sort();
    assert_eq!(
        files,
        [
            "linux-aarch64/toolkit",
            "linux-x86_64/toolkit",
            "macos-aarch64/toolkit",
            "macos-x86_64/toolkit",
            "manifest.json",
            "toolkit",
            "toolkit.exe",
        ]
    );
    assert_eq!(
        fs::read_to_string(bin.join("toolkit")).expect("dispatcher"),
        dispatcher_script("toolkit")
    );
    let manifest = fs::read_to_string(bin.join("manifest.json")).expect("manifest");
    assert!(
        manifest.contains("\"package_root\": \"pkgs/demo\""),
        "{manifest}"
    );
    assert!(!manifest.contains("\"plugin\""), "{manifest}");
    assert!(manifest.contains("\"agents-pay-live\""), "{manifest}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |rel: &str| {
            fs::metadata(bin.join(rel))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        for rel in ["toolkit", "toolkit.exe", "linux-x86_64/toolkit"] {
            assert_eq!(mode(rel), 0o755, "{rel}");
        }
        assert_eq!(mode("manifest.json"), 0o644);
    }
    assert!(
        fixture.verify(true).is_empty(),
        "{:#?}",
        fixture.verify(true)
    );
    assert!(
        fixture.verify_package().is_empty(),
        "{:#?}",
        fixture.verify_package()
    );
}

#[cfg(unix)]
#[test]
fn t_pkg_stage_refuses_to_write_through_a_symlinked_bin() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::staged();
    let outside = fixture.dir.join("outside-bin");
    fs::rename(fixture.bin(), &outside).expect("move");
    symlink(&outside, fixture.bin()).expect("symlink");
    let error = fixture
        .stage(&[Platform::LinuxX86_64], &provenance(false))
        .expect_err("symlinked bin/");
    assert!(error.contains("symlink"), "{error}");
}

#[test]
fn t_pkg_stage_plugins_and_packages_coexist() {
    let plugin = r##"{"plugin": "demo", "name": "guard", "package": "g", "bin": "guard", "role": "guard-hook", "deny_reason": "Guard unavailable.", "consumer": "#1"}"##;
    let declaration = parse(&[plugin, ENTRY]).expect("both destinations");
    assert_eq!(
        declaration.plugins().into_iter().collect::<Vec<_>>(),
        ["demo"]
    );
    assert!(declaration.binary("demo", "guard").is_some());
    // A package binary is never resolved as a plugin binary.
    assert!(declaration.binary("pkgs", "toolkit").is_none());
    assert!(declaration.binary(ROOT, "toolkit").is_none());
    assert_eq!(
        declaration
            .select("pkgs/demo/toolkit")
            .map(|binary| binary.name.as_str()),
        Some("toolkit")
    );
    assert_eq!(
        declaration
            .select("demo/guard")
            .map(|binary| binary.name.as_str()),
        Some("guard")
    );
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("list").filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type().expect("type").is_dir() {
            collect(&entry.path(), &rel, out);
        } else {
            out.push(rel);
        }
    }
}

// -------------------------------------------------------------- T-PKG-VERIFY

#[test]
fn t_pkg_verify_covers_package_destinations() {
    let fixture = Fixture::staged();
    fs::write(fixture.bin().join("extra"), "x").expect("write");
    assert_flags(&fixture.verify(false), "pkgs/demo/bin/extra: not listed");

    let fixture = Fixture::staged();
    fs::write(
        fixture.bin().join("linux-x86_64/toolkit"),
        "#!/bin/sh\nexit 9\n",
    )
    .expect("write");
    assert_flags(&fixture.verify(false), "sha256 differs");

    let fixture = Fixture::new();
    fixture
        .stage(&[Platform::LinuxX86_64], &provenance(false))
        .expect("stage one platform");
    assert!(
        fixture.verify(false).is_empty(),
        "{:#?}",
        fixture.verify(false)
    );
    let missing = fixture.verify(true);
    assert_eq!(missing.len(), 4, "{missing:#?}");
    assert!(missing
        .iter()
        .all(|p| p.contains("toolkit is not staged for")));

    let fixture = Fixture::new();
    assert_flags(&fixture.verify(true), "pkgs/demo/bin: not staged");
    assert!(
        fixture.verify(false).is_empty(),
        "{:#?}",
        fixture.verify(false)
    );
}

#[cfg(unix)]
#[test]
fn t_pkg_verify_a_symlinked_package_bin_is_refused() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::staged();
    let outside = fixture.dir.join("outside-bin");
    fs::rename(fixture.bin(), &outside).expect("move");
    symlink(&outside, fixture.bin()).expect("symlink");
    for complete in [false, true] {
        let problems = fixture.verify(complete);
        assert_flags(&problems, "pkgs/demo/bin");
        assert_flags(&problems, "symlink");
    }
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

#[test]
fn t_pkg_verify_check_refuses_a_tracked_file_under_a_package_bin() {
    let fixture = Fixture::staged();
    git(&fixture.dir, &["init", "--quiet"]);
    git(&fixture.dir, &["add", "--all"]);
    git(&fixture.dir, &["commit", "--quiet", "-m", "fixture"]);
    let declaration = fixture.declaration();
    assert!(
        tracked_package_files(&fixture.dir, &declaration).is_empty(),
        "{:#?}",
        tracked_package_files(&fixture.dir, &declaration)
    );
    let root = fixture.dir.to_string_lossy().into_owned();
    let output = plugin_stage(&fixture.dir, &["check", "--root", &root], None);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    git(
        &fixture.dir,
        &["add", "--force", "pkgs/demo/bin/manifest.json"],
    );
    let problems = tracked_package_files(&fixture.dir, &declaration);
    assert_flags(&problems, "pkgs/demo/bin/manifest.json");
    assert_flags(&problems, "tracked");
    let output = plugin_stage(&fixture.dir, &["check", "--root", &root], None);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("tracked"), "{}", stderr(&output));
}

// ---------------------------------------------------------------- T-PKG-PACK

fn empty_path(fixture: &Fixture) -> PathBuf {
    let path = fixture.dir.join("empty-path");
    fs::create_dir_all(&path).expect("empty PATH dir");
    path
}

#[test]
fn t_pkg_pack_a_clean_complete_package_passes_offline() {
    let fixture = Fixture::staged();
    assert!(
        fixture.verify_package().is_empty(),
        "{:#?}",
        fixture.verify_package()
    );
    // The prepared publisher tool runs with no cargo, git, network or
    // toolchain on PATH and no inherited environment.
    let path = empty_path(&fixture);
    let output = plugin_stage(
        &fixture.package(),
        &["verify-package", "--package-root", ".", "--complete"],
        Some(&path),
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    // Nothing was written into the package.
    let mut files = Vec::new();
    collect(&fixture.bin(), "", &mut files);
    assert_eq!(files.len(), 7, "{files:#?}");
}

#[test]
#[allow(clippy::too_many_lines)] // One table of refusal cases.
fn t_pkg_pack_negatives_fail_and_agree_with_verify_complete() {
    let mut cases: Vec<(&str, Edit, &str)> = vec![
        (
            "missing manifest",
            Box::new(|f: &Fixture| fs::remove_file(f.bin().join("manifest.json")).expect("rm")),
            "has no manifest.json",
        ),
        (
            "unparseable manifest",
            Box::new(|f: &Fixture| fs::write(f.bin().join("manifest.json"), "{").expect("w")),
            "invalid JSON",
        ),
        (
            "dirty manifest",
            Box::new(|f: &Fixture| {
                f.edit_manifest("\"source_dirty\": false", "\"source_dirty\": true");
            }),
            "dirty tree",
        ),
        (
            "manifest of another destination",
            Box::new(|f: &Fixture| {
                f.edit_manifest(
                    "\"package_root\": \"pkgs/demo\"",
                    "\"package_root\": \"pkgs/other\"",
                );
            }),
            "names package_root",
        ),
        (
            "manifest of a plugin",
            Box::new(|f: &Fixture| {
                f.edit_manifest("\"package_root\": \"pkgs/demo\"", "\"plugin\": \"demo\"");
            }),
            "names plugin",
        ),
        (
            "missing platform",
            Box::new(|f: &Fixture| fs::remove_dir_all(f.bin().join("macos-x86_64")).expect("rm")),
            "macos-x86_64/toolkit: is missing",
        ),
        (
            "missing dispatcher",
            Box::new(|f: &Fixture| fs::remove_file(f.bin().join("toolkit")).expect("rm")),
            "bin/toolkit: is missing",
        ),
        (
            "tampered size",
            Box::new(|f: &Fixture| {
                let path = f.bin().join("linux-aarch64/toolkit");
                let mut bytes = fs::read(&path).expect("read");
                bytes.push(b'\n');
                fs::write(&path, bytes).expect("write");
            }),
            "size differs",
        ),
        (
            "tampered hash",
            Box::new(|f: &Fixture| {
                let path = f.bin().join("linux-aarch64/toolkit");
                let mut bytes = fs::read(&path).expect("read");
                let last = bytes.len() - 1;
                bytes[last] ^= 0x01;
                fs::write(&path, bytes).expect("write");
            }),
            "sha256 differs",
        ),
        (
            "unlisted file",
            Box::new(|f: &Fixture| fs::write(f.bin().join("linux-x86_64/extra"), "x").expect("w")),
            "linux-x86_64/extra: not listed",
        ),
        (
            "unlisted directory",
            Box::new(|f: &Fixture| fs::create_dir_all(f.bin().join("scratch")).expect("mkdir")),
            "scratch: unexpected directory",
        ),
        (
            "non-regular file at a listed path",
            Box::new(|f: &Fixture| {
                let path = f.bin().join("toolkit.exe");
                fs::remove_file(&path).expect("rm");
                fs::create_dir_all(&path).expect("mkdir");
            }),
            "toolkit.exe",
        ),
        (
            "wrong layout",
            Box::new(|f: &Fixture| {
                f.edit_manifest(
                    "\"path\": \"linux-x86_64/toolkit\"",
                    "\"path\": \"linux-x86_64/toolkit2\"",
                );
            }),
            "expected layout path",
        ),
        (
            "wrong target",
            Box::new(|f: &Fixture| {
                f.edit_manifest(
                    "\"triple\": \"x86_64-apple-darwin\"",
                    "\"triple\": \"x86_64-apple-ios\"",
                );
            }),
            "but the declaration says",
        ),
        (
            "wrong platform",
            Box::new(|f: &Fixture| {
                f.edit_manifest(
                    "\"platform\": \"macos-x86_64\"",
                    "\"platform\": \"plan9-x86_64\"",
                );
            }),
            "unknown platform",
        ),
        (
            "features without agents-pay-live",
            Box::new(|f: &Fixture| f.edit_manifest("\"agents-pay-live\"", "\"aws-sdk\"")),
            "but the declaration says",
        ),
        (
            "wrong package",
            Box::new(|f: &Fixture| {
                f.edit_manifest("\"package\": \"toolkit-cli\"", "\"package\": \"other\"");
            }),
            "but the declaration says",
        ),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        cases.push((
            "symlinked file",
            Box::new(|f: &Fixture| {
                let path = f.bin().join("linux-x86_64/toolkit");
                let copy = f.dir.join("copy-toolkit");
                fs::rename(&path, &copy).expect("move");
                symlink(&copy, &path).expect("symlink");
            }),
            "symlink",
        ));
        cases.push((
            "symlinked bin",
            Box::new(|f: &Fixture| {
                let outside = f.dir.join("outside-bin");
                fs::rename(f.bin(), &outside).expect("move");
                symlink(&outside, f.bin()).expect("symlink");
            }),
            "symlink",
        ));
        cases.push((
            "symlinked platform directory",
            Box::new(|f: &Fixture| {
                let outside = f.dir.join("outside-linux");
                fs::rename(f.bin().join("linux-x86_64"), &outside).expect("move");
                symlink(&outside, f.bin().join("linux-x86_64")).expect("symlink");
            }),
            "symlink",
        ));
        cases.push((
            "symlinked path component",
            Box::new(|f: &Fixture| {
                let outside = f.dir.join("outside-pkgs");
                fs::rename(f.dir.join("pkgs"), &outside).expect("move");
                symlink(&outside, f.dir.join("pkgs")).expect("symlink");
            }),
            "symlink",
        ));
        cases.push((
            "wrong mode",
            Box::new(|f: &Fixture| {
                fs::set_permissions(
                    f.bin().join("linux-x86_64/toolkit"),
                    fs::Permissions::from_mode(0o644),
                )
                .expect("chmod");
            }),
            "is not executable",
        ));
        cases.push((
            "dispatcher without the executable bit",
            Box::new(|f: &Fixture| {
                fs::set_permissions(f.bin().join("toolkit"), fs::Permissions::from_mode(0o644))
                    .expect("chmod");
            }),
            "bin/toolkit: is not executable",
        ));
    }
    for (label, edit, needle) in cases {
        let fixture = Fixture::staged();
        assert!(fixture.verify_package().is_empty(), "{label}: clean first");
        edit(&fixture);
        let problems = fixture.verify_package();
        assert!(!problems.is_empty(), "{label}: verify-package passed");
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{label}: expected {needle:?} in {problems:#?}"
        );
        // The pack guard and the release gate are one verifier: every
        // problem the guard reports, verify --complete reports too.
        let complete = fixture.verify(true);
        for problem in &problems {
            assert!(
                complete.contains(problem),
                "{label}: {problem:?} missing from verify --complete {complete:#?}"
            );
        }
    }
}

#[test]
fn t_pkg_pack_a_dirty_stage_is_refused() {
    let fixture = Fixture::new();
    fixture
        .stage(&Platform::ALL, &provenance(true))
        .expect("dirty stage");
    assert_flags(&fixture.verify_package(), "dirty tree");
    let path = empty_path(&fixture);
    let output = plugin_stage(
        &fixture.package(),
        &["verify-package", "--package-root", ".", "--complete"],
        Some(&path),
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("dirty tree"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn t_pkg_pack_undeclared_roots_and_missing_declarations_fail_closed() {
    let fixture = Fixture::staged();
    // An undeclared directory, even a valid-looking npm package.
    let other = fixture.dir.join("pkgs/other");
    write(&other.join("package.json"), "{}\n");
    write(&other.join(".gitignore"), "bin/\n");
    let problems = verify_package(&fixture.dir, &fixture.declaration(), &other, true);
    assert_flags(&problems, "is not a declared package destination");
    let path = empty_path(&fixture);
    let output = plugin_stage(
        &other,
        &["verify-package", "--package-root", ".", "--complete"],
        Some(&path),
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("is not a declared package destination"),
        "{}",
        stderr(&output)
    );
    // The repository root itself is not a package destination.
    let problems = verify_package(&fixture.dir, &fixture.declaration(), &fixture.dir, true);
    assert_flags(&problems, "is not a declared package destination");

    // No release declaration in any ancestor.
    let bare = temp_dir("package-bare");
    write(&bare.join("pkg/package.json"), "{}\n");
    let output = plugin_stage(
        &bare.join("pkg"),
        &["verify-package", "--package-root", ".", "--complete"],
        Some(&path),
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(DECLARATION_PATH),
        "{}",
        stderr(&output)
    );

    // A missing package root fails closed.
    let output = plugin_stage(
        &fixture.package(),
        &["verify-package", "--package-root", "./nope", "--complete"],
        Some(&path),
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn t_pkg_pack_usage_errors_and_hostile_arguments() {
    let fixture = Fixture::staged();
    let path = empty_path(&fixture);
    for args in [
        &["verify-package", "--package-root", "."][..],
        &["verify-package", "--complete"][..],
        &[
            "verify-package",
            "--package-root",
            ".",
            "--complete",
            "extra",
        ][..],
    ] {
        let output = plugin_stage(&fixture.package(), args, Some(&path));
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    let hostile = "x\u{1b}[2J\nplugin-stage: forged OK";
    let output = plugin_stage(
        &fixture.package(),
        &["verify-package", "--package-root", hostile, "--complete"],
        Some(&path),
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(
        !text
            .lines()
            .any(|line| line.starts_with("plugin-stage: forged")),
        "{text:?}"
    );
}

// ----------------------------------------------------------------- T-PKG-INV

#[test]
fn t_pkg_inv_the_launcher_contract_is_identical_with_and_without_packages() {
    let root = repo_root();
    let plugins = root.join("plugins");
    let with = Declaration::load(&root.join(DECLARATION_PATH)).expect("declaration");
    assert!(
        with.binaries
            .iter()
            .any(|binary| matches!(binary.destination, Destination::PackageRoot(_))),
        "the committed declaration declares the OpenClaw package destination"
    );
    let without = Declaration {
        platforms: with.platforms.clone(),
        binaries: with
            .binaries
            .iter()
            .filter(|binary| matches!(binary.destination, Destination::Plugin(_)))
            .cloned()
            .collect(),
    };
    let a = check_launchers(&root, &plugins, &with);
    let b = check_launchers(&root, &plugins, &without);
    assert_eq!(format!("{:?}", a.problems), format!("{:?}", b.problems));
    assert_eq!(format!("{:?}", a.references), format!("{:?}", b.references));
    assert_eq!(format!("{:?}", a.accepted), format!("{:?}", b.accepted));
    assert_eq!(a.files_scanned, b.files_scanned);
    let names = |report: &plugin_stage::launcher::LauncherReport, d: &Declaration| {
        report
            .unreferenced(d)
            .iter()
            .map(|binary| format!("{:?}/{}", binary.destination, binary.name))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&a, &with), names(&b, &without));
    for binary in &without.binaries {
        assert_eq!(
            launcher_snippet(binary).expect("plugin launcher"),
            launcher_snippet(
                with.select(&format!(
                    "{}/{}",
                    binary.plugin().expect("plugin"),
                    binary.name
                ))
                .expect("same binary")
            )
            .expect("plugin launcher")
        );
    }
}

// -------------------------------------------------------------- T-PKG-NOLIVE

/// R-PKG-7: no test target builds or stages the real declaration. The real
/// `agents-pay-live` entry is staged only by the `plugin-binaries` release
/// target; every test uses fixture declarations and stand-in binaries.
#[test]
fn t_pkg_nolive_no_test_target_stages_the_real_declaration() {
    let root = repo_root();
    let declaration = Declaration::load(&root.join(DECLARATION_PATH)).expect("declaration");
    let live: Vec<_> = declaration
        .binaries
        .iter()
        .filter(|binary| binary.features.iter().any(|f| f == "agents-pay-live"))
        .collect();
    assert_eq!(live.len(), 1, "{live:#?}");
    assert!(matches!(live[0].destination, Destination::PackageRoot(_)));

    // Build-capable entry points never appear in a test source.
    let forbidden = [
        concat!("stage::", "build("),
        concat!("Probe::", "detect("),
        concat!(".arg(\"", "stage\")"),
        concat!("[\"", "stage\""),
        concat!("&[\"", "plan\""),
    ];
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for dir in ["tests", "tests/common", "src"] {
        for entry in fs::read_dir(crate_dir.join(dir))
            .expect("list")
            .filter_map(Result::ok)
        {
            if entry.path().extension().is_some_and(|ext| ext == "rs") {
                sources.push(entry.path());
            }
        }
    }
    assert!(sources.len() >= 10, "{sources:#?}");
    for path in sources {
        let text = fs::read_to_string(&path).expect("source");
        // Library sources: only their #[cfg(test)] module is a test target.
        let tests = if path.parent().is_some_and(|p| p.ends_with("src")) {
            text.split_once("#[cfg(test)]").map_or("", |(_, rest)| rest)
        } else {
            text.as_str()
        };
        for token in forbidden {
            assert!(
                !tests.contains(token),
                "{}: a test target must not use {token}",
                path.display()
            );
        }
    }

    // Only the `plugin-binaries` release target stages; no verification
    // target depends on it (X402 Q24).
    let makefile = fs::read_to_string(root.join("Makefile")).expect("Makefile");
    let mut target = String::new();
    for line in makefile.lines() {
        if let Some((name, deps)) = line.split_once(':') {
            if !line.starts_with(['\t', ' ', '#', '.']) && !name.contains('=') {
                target = name.trim().to_string();
                assert!(
                    !deps.split_whitespace().any(|dep| dep == "plugin-binaries"),
                    "Makefile target {target} depends on the release staging target"
                );
            }
        }
        if line.contains("plugin-stage") && line.contains(" stage") {
            assert_eq!(target, "plugin-binaries", "{line}");
        }
    }
}
