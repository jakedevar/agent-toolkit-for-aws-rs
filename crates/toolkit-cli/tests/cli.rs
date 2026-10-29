//! Integration tests that exercise the built `toolkit` binary end to end:
//! real process spawns, real exit codes and separated stdout/stderr.
//!
//! Fixtures are deterministic offline directories created under
//! `std::env::temp_dir()`; no network, cloud or live service is involved.

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fmt::Write as _;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique temporary directory removed on drop, so fixtures are cleaned up
/// even when an assertion fails mid-test.
struct TempDir(PathBuf);

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

fn temp_root(label: &str) -> TempDir {
    let unique = format!(
        "toolkit-cli-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn skill(root: &Path, relative: &str, name: Option<&str>, description: Option<&str>) {
    let full = root.join(relative);
    fs::create_dir_all(&full).expect("create skill dir");
    let mut content = String::from("---\n");
    if let Some(name) = name {
        let _ = writeln!(content, "name: {name}");
    }
    if let Some(description) = description {
        let _ = writeln!(content, "description: >\n  {description}");
    }
    content.push_str("---\n");
    fs::write(full.join("SKILL.md"), content).expect("write SKILL.md");
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn toolkit binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is utf-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is utf-8")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exits with a code")
}

#[test]
fn list_reports_sorted_entries_on_stdout() {
    let root = temp_root("success");
    skill(&root, "core-skills/zulu", None, Some("last"));
    skill(
        &root,
        "specialized-skills/storage/alpha",
        Some("alpha"),
        Some("first"),
    );
    skill(&root, "core-skills/mid", None, None);

    let output = run(
        &["skills", "list", root.to_str().expect("utf-8 root")],
        &root,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        stdout(&output),
        "alpha\tfirst\nmid\t\nzulu\tlast\n",
        "entries are sorted by name, absent description renders empty"
    );
}

#[test]
fn list_json_emits_name_description_path() {
    let root = temp_root("json");
    skill(&root, "b/skill", Some("beta"), Some("two lines"));
    skill(&root, "a/quoted", Some("alpha"), Some("say \"hi\""));

    let output = run(
        &[
            "skills",
            "list",
            root.to_str().expect("utf-8 root"),
            "--json",
        ],
        &root,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let json = stdout(&output);
    // The document is an array of objects with exactly the three documented
    // fields, in sorted order.
    assert!(json.starts_with("[\n  {\n"));
    assert!(json.contains("\"name\": \"alpha\""));
    assert!(json.contains("\"description\": \"say \\\"hi\\\"\""));
    assert!(json.contains("\"path\": \"a/quoted\""));
    let alpha = json.find("\"name\": \"alpha\"").expect("alpha present");
    let beta = json.find("\"name\": \"beta\"").expect("beta present");
    assert!(alpha < beta, "JSON entries follow the sorted order");
    assert!(
        json.contains("\"description\": \"say \\\"hi\\\"\""),
        "quotes are escaped"
    );
}

#[test]
fn list_defaults_to_skills_root_in_current_directory() {
    let cwd = temp_root("default-root");
    skill(&cwd, "skills/inside", Some("inside"), None);

    let output = run(&["skills", "list"], &cwd);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "inside\t\n");
    assert_eq!(stderr(&output), "");
}

#[test]
fn list_on_empty_root_succeeds_silently() {
    let root = temp_root("empty");

    let plain = run(
        &["skills", "list", root.to_str().expect("utf-8 root")],
        &root,
    );
    assert_eq!(code(&plain), 0);
    assert_eq!(stdout(&plain), "");
    assert_eq!(stderr(&plain), "");

    let json = run(
        &[
            "skills",
            "list",
            root.to_str().expect("utf-8 root"),
            "--json",
        ],
        &root,
    );
    assert_eq!(code(&json), 0);
    assert_eq!(stdout(&json), "[]\n");
}

#[test]
fn missing_root_fails_on_stderr_with_exit_one() {
    let cwd = temp_root("missing-root");

    let output = run(&["skills", "list", "does-not-exist"], &cwd);
    assert_eq!(code(&output), 1, "missing root is a runtime error");
    assert_eq!(stdout(&output), "", "no data on stdout for errors");
    assert!(stderr(&output).starts_with("error: "));
    assert!(stderr(&output).contains("root does not exist"));
}

#[test]
fn file_root_fails_with_exit_one() {
    let cwd = temp_root("file-root");
    fs::write(cwd.join("not-a-dir"), "x").expect("write file");

    let output = run(&["skills", "list", "not-a-dir"], &cwd);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("root is not a directory"));
    assert_eq!(stdout(&output), "");
}

#[test]
fn usage_errors_go_to_stderr_with_exit_two() {
    let cwd = temp_root("usage");
    for bad in [
        vec!["unknown"],
        vec![],
        vec!["skills"],
        vec!["skills", "export"],
        vec!["skills", "list", "--wat"],
        vec!["skills", "list", "a", "b"],
    ] {
        let output = run(&bad, &cwd);
        assert_eq!(code(&output), 2, "args {bad:?} must be a usage error");
        assert_eq!(stdout(&output), "", "args {bad:?} must not print data");
        assert!(
            stderr(&output).starts_with("error: invalid arguments"),
            "args {bad:?}"
        );
        assert!(
            stderr(&output).contains("Usage:"),
            "args {bad:?} prints usage"
        );
    }
}

#[test]
fn help_prints_usage_on_stdout_with_success() {
    let cwd = temp_root("help");
    for flag in [["--help"], ["-h"], ["help"]] {
        let output = run(&flag, &cwd);
        assert_eq!(code(&output), 0);
        assert_eq!(stderr(&output), "");
        assert!(stdout(&output).contains("Usage:"));
        assert!(stdout(&output).contains("skills list"));
        assert!(stdout(&output).contains("Exit codes:"));
        // The heading is exactly `Options:` (#4764), not `Options:)`.
        assert!(stdout(&output).lines().any(|line| line == "Options:"));
        assert!(!stdout(&output).contains("Options:)"));
    }
}

#[test]
#[cfg(unix)]
fn closed_stdout_pipe_is_success_not_panic() {
    // `toolkit skills list | head` closes the read end once the consumer is
    // done. Rust ignores SIGPIPE, so the write surfaces as BrokenPipe; the CLI
    // must exit 0 (or at worst a clean signal-free death), never panic with
    // exit 101. The listing is far larger than the 64 KiB pipe buffer so the
    // child really does hit the closed pipe.
    let root = temp_root("broken-pipe");
    for index in 0..2000u32 {
        let name = format!("skill-{index:04}");
        skill(&root, &name, Some(&name), Some(&"x".repeat(80)));
    }

    let mut child = Command::new(BIN)
        .args(["skills", "list", root.to_str().expect("utf-8 root")])
        .current_dir(&*root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn toolkit binary");
    // Take and drop the stdout handle: the child's next write past the pipe
    // buffer fails with EPIPE.
    drop(child.stdout.take());
    let status = child.wait().expect("wait for toolkit");

    assert_eq!(
        status.code(),
        Some(0),
        "closed pipe is documented success; exit 101 would mean a stdout panic: {status:?}"
    );
}

#[test]
#[cfg(unix)]
fn non_utf8_arguments_are_usage_errors_not_panics() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let cwd = temp_root("non-utf8-arg");
    let bad = OsStr::from_bytes(b"skills\xff");
    let output = Command::new(BIN)
        .arg(bad)
        .current_dir(&*cwd)
        .output()
        .expect("spawn toolkit binary");
    assert_eq!(code(&output), 2, "non-UTF-8 argument is a usage error");
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).starts_with("error: invalid arguments"));
    assert!(stderr(&output).contains("valid UTF-8"));
    assert!(stderr(&output).contains("Usage:"), "usage text is printed");
}

#[test]
fn version_prints_the_package_version() {
    let cwd = temp_root("version");
    for flag in [["--version"], ["-V"], ["version"]] {
        let output = run(&flag, &cwd);
        assert_eq!(code(&output), 0);
        assert_eq!(stderr(&output), "");
        assert_eq!(
            stdout(&output),
            format!("toolkit {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn json_output_is_valid_json_matching_the_published_schema() {
    let root = temp_root("json-schema");
    skill(&root, "one", Some("one"), Some("say \"hi\"\tnow"));
    skill(&root, "two/nested", None, None);

    let output = run(
        &[
            "skills",
            "list",
            root.to_str().expect("utf-8 root"),
            "--json",
        ],
        &root,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    let parsed = toolkit_core::parse_json(&stdout(&output)).expect("stdout is one JSON document");
    toolkit_core::validate(&toolkit_core::skills_output_schema(), &parsed)
        .expect("output matches the published schema");
    let toolkit_core::Json::Array(items) = &parsed else {
        panic!("expected a JSON array");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].get("path").and_then(toolkit_core::Json::as_str),
        Some("two/nested")
    );
}

#[test]
fn json_output_is_byte_identical_across_runs() {
    let root = temp_root("deterministic");
    for index in 0..12u32 {
        skill(
            &root,
            &format!("g{}/s{index:02}", index % 4),
            None,
            Some("d"),
        );
    }
    let args = [
        "skills",
        "list",
        root.to_str().expect("utf-8 root"),
        "--json",
    ];
    let first = run(&args, &root);
    let second = run(&args, &root);
    assert_eq!(code(&first), 0);
    assert_eq!(stdout(&first), stdout(&second));
}

#[test]
fn root_itself_is_not_listed_but_children_are() {
    let root = temp_root("root-self");
    fs::write(root.join("SKILL.md"), "---\nname: root-skill\n---\n").expect("write");
    skill(&root, "child", Some("child"), None);

    let output = run(
        &["skills", "list", root.to_str().expect("utf-8 root")],
        &root,
    );
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output), "child\t\n");

    let only_root = temp_root("root-only");
    fs::write(only_root.join("SKILL.md"), "---\nname: root-skill\n---\n").expect("write");
    let output = run(
        &["skills", "list", only_root.to_str().expect("utf-8 root")],
        &only_root,
    );
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output), "");
}

#[test]
fn empty_name_falls_back_and_bom_is_ignored() {
    let root = temp_root("fallbacks");
    let write = |dir: &str, content: &[u8]| {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
        fs::write(root.join(dir).join("SKILL.md"), content).expect("write");
    };
    write("empty-name", b"---\nname:\ndescription: kept\n---\n");
    write("with-bom", b"\xef\xbb\xbf---\nname: bom-name\n---\n");

    let output = run(
        &["skills", "list", root.to_str().expect("utf-8 root")],
        &root,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "bom-name\t\nempty-name\tkept\n");
}

#[test]
fn multiline_names_stay_on_one_plain_line_and_exact_in_json() {
    let root = temp_root("multiline-name");
    fs::create_dir_all(root.join("multi")).expect("mkdir");
    fs::write(
        root.join("multi").join("SKILL.md"),
        "---\nname: |\n  first\n  second\ndescription: a\tb\n---\n",
    )
    .expect("write");
    let root_arg = root.to_str().expect("utf-8 root");

    let plain = run(&["skills", "list", root_arg], &root);
    assert_eq!(code(&plain), 0);
    assert_eq!(stdout(&plain), "first second\ta b\n");

    let json = run(&["skills", "list", root_arg, "--json"], &root);
    assert_eq!(code(&json), 0);
    assert!(stdout(&json).contains("\"name\": \"first\\nsecond\""));
}

#[test]
#[cfg(unix)]
fn symlinks_never_leak_content_or_listings_through_the_binary() {
    let root = temp_root("symlinks");
    let outside = temp_root("symlinks-outside");
    skill(&root, "real", Some("real"), None);
    // A linked skill directory outside ROOT.
    skill(&outside, "linked-skill", Some("linked-dir-leak"), None);
    std::os::unix::fs::symlink(outside.join("linked-skill"), root.join("linked")).expect("link");
    // A real directory whose SKILL.md is a link to a file outside ROOT.
    fs::create_dir_all(root.join("leaky")).expect("mkdir");
    fs::write(
        outside.join("secret.md"),
        "---\nname: leaked-file\ndescription: secret\n---\n",
    )
    .expect("write");
    std::os::unix::fs::symlink(
        outside.join("secret.md"),
        root.join("leaky").join("SKILL.md"),
    )
    .expect("link");

    for extra in [&[][..], &["--json"][..]] {
        let mut args = vec!["skills", "list", root.to_str().expect("utf-8 root")];
        args.extend_from_slice(extra);
        let output = run(&args, &root);
        assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
        let text = stdout(&output);
        assert!(text.contains("real"));
        assert!(!text.contains("leak"), "no content through links: {text}");
        assert!(!text.contains("secret"), "no content through links: {text}");
        assert!(
            !text.contains("linked"),
            "linked directory is not listed: {text}"
        );
        assert_eq!(stderr(&output), "");
    }
}

#[test]
#[cfg(unix)]
fn closed_stdout_pipe_with_json_is_success_not_panic() {
    // Far larger than the 64 KiB pipe buffer so the child really hits EPIPE.
    let root = temp_root("broken-pipe-json");
    for index in 0..2000u32 {
        let name = format!("skill-{index:04}");
        skill(&root, &name, Some(&name), Some(&"x".repeat(80)));
    }
    let mut child = Command::new(BIN)
        .args([
            "skills",
            "list",
            root.to_str().expect("utf-8 root"),
            "--json",
        ])
        .current_dir(&*root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn toolkit binary");
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("wait for toolkit");
    assert_eq!(
        output.status.code(),
        Some(0),
        "closed pipe is documented success: {:?}",
        output.status
    );
    assert_eq!(
        stderr(&output),
        "",
        "a closed pipe prints nothing on stderr"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn stdout_write_failure_is_reported_with_exit_one() {
    // /dev/full accepts opens but fails every write with ENOSPC, a stdout
    // error that is not a closed pipe.
    let root = temp_root("dev-full");
    skill(&root, "one", Some("one"), Some("d"));
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let output = Command::new(BIN)
        .args(["skills", "list", root.to_str().expect("utf-8 root")])
        .current_dir(&*root)
        .stdout(Stdio::from(full))
        .output()
        .expect("spawn toolkit binary");
    assert_eq!(code(&output), 1, "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).starts_with("error: cannot write to stdout: "),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!stderr(&output).contains("Usage:"));
}

#[test]
fn unreadable_skill_md_is_a_runtime_error_naming_the_file() {
    let root = temp_root("binary-skill");
    fs::create_dir_all(root.join("bad")).expect("mkdir");
    fs::write(root.join("bad").join("SKILL.md"), b"\xff\xfe not utf-8").expect("write");
    let output = run(
        &["skills", "list", root.to_str().expect("utf-8 root")],
        &root,
    );
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).starts_with("error: "));
    assert!(stderr(&output).contains("SKILL.md"));
    assert!(stderr(&output).contains("not valid UTF-8"));
}

// ---- manifests validate (#4375) ------------------------------------------

fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture dir");
    fs::write(path, content).expect("write fixture");
}

/// A minimal valid repository: one plugin named `demo`, three marketplaces,
/// one top-level skill.
fn write_manifest_repo(root: &Path) {
    let market = r#"{"name":"m","plugins":[{"name":"demo","source":"./plugins/demo"}]}"#;
    write_file(root, ".claude-plugin/marketplace.json", market);
    write_file(root, ".agents/plugins/marketplace.json", market);
    write_file(root, ".cursor-plugin/marketplace.json", market);
    let meta = r#"{"name":"demo","version":"1.0.0","description":"Demo."}"#;
    write_file(root, "plugins/demo/.claude-plugin/plugin.json", meta);
    write_file(
        root,
        "plugins/demo/.codex-plugin/plugin.json",
        r#"{"name":"demo","version":"1.0.0","description":"Demo.","author":{},"interface":{}}"#,
    );
    write_file(
        root,
        "plugins/demo/.mcp.json",
        r#"{"mcpServers":{"a":{"command":"tool"}}}"#,
    );
    let skill = "---\nname: demo-skill\ndescription: A description that is long enough.\n---\n";
    write_file(root, "plugins/demo/skills/demo-skill/SKILL.md", skill);
    write_file(
        root,
        "skills/group/top-skill/SKILL.md",
        &skill.replace("demo-skill", "top-skill"),
    );
}

#[test]
fn manifests_validate_passes_on_a_valid_repository() {
    let root = temp_root("manifests-ok");
    write_manifest_repo(&root);
    let output = run(&["manifests", "validate"], &root);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        stdout(&output),
        "Validating Claude Code marketplace: .claude-plugin/marketplace.json\n\
         Validating Codex marketplace: .agents/plugins/marketplace.json\n\
         Validating Cursor marketplace: .cursor-plugin/marketplace.json\n\
         Validating plugin: demo\n\
         Validating skill: skills/group/top-skill\n\
         \n\
         All validations passed.\n",
        "ROOT defaults to the current directory"
    );
}

#[test]
fn manifests_validate_reports_errors_on_stderr_and_exits_one() {
    let root = temp_root("manifests-fail");
    write_manifest_repo(&root);
    write_file(
        &root,
        "plugins/demo/.mcp.json",
        r#"{"mcpServers":{"a":{}}}"#,
    );
    write_file(&root, "skills/group/top-skill/SKILL.md", "no frontmatter\n");

    let output = run(
        &["manifests", "validate", root.to_str().expect("utf-8 root")],
        Path::new("/"),
    );
    assert_eq!(code(&output), 1);
    assert!(stdout(&output).starts_with("Validating Claude Code marketplace: "));
    assert!(!stdout(&output).contains("All validations passed."));
    assert_eq!(
        stderr(&output),
        "  ERROR: MCP server 'a' is stdio but missing 'command'\n\
         \x20 ERROR: Missing YAML frontmatter in skills/group/top-skill/SKILL.md\n\
         error: Validation failed with 2 error(s).\n"
    );
}

#[test]
fn manifests_validate_plugin_selection_and_usage_errors() {
    let root = temp_root("manifests-plugin");
    write_manifest_repo(&root);
    write_file(&root, ".claude-plugin/marketplace.json", "broken");
    let root_arg = root.to_str().expect("utf-8 root");

    let output = run(
        &["manifests", "validate", root_arg, "--plugin", "demo"],
        &root,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "Validating plugin: demo\n\nAll validations passed.\n"
    );

    let output = run(
        &["manifests", "validate", root_arg, "--plugin", "ghost"],
        &root,
    );
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "error: Plugin not found: ghost\n");

    for args in [
        vec!["manifests", "validate", "--plugin"],
        vec!["manifests", "validate", "--json"],
        vec!["manifests", "validate", "a", "b"],
        vec!["manifests"],
    ] {
        let output = run(&args, &root);
        assert_eq!(code(&output), 2, "{args:?}");
        assert_eq!(stdout(&output), "");
        assert!(
            stderr(&output).starts_with("error: invalid arguments\n\nUsage:"),
            "{args:?}"
        );
    }
}

#[test]
fn manifests_validate_passes_on_the_real_repository_tree() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = run(
        &[
            "manifests",
            "validate",
            repo.to_str().expect("utf-8 repo path"),
        ],
        &repo,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let text = stdout(&output);
    assert!(text.ends_with("\nAll validations passed.\n"), "{text}");
    assert!(text.contains("Validating plugin: aws-core\n"));
}

#[cfg(unix)]
#[test]
fn manifests_validate_never_follows_symlinked_skill_files() {
    use std::os::unix::fs::symlink;
    let root = temp_root("manifests-link");
    write_manifest_repo(&root);
    let outside = temp_root("manifests-link-outside");
    write_file(
        &outside,
        "SKILL.md",
        "---\nname: linked-skill\ndescription: A description that is long enough.\n---\n",
    );
    fs::create_dir_all(root.join("skills/linked-skill")).expect("create dir");
    symlink(
        outside.join("SKILL.md"),
        root.join("skills/linked-skill/SKILL.md"),
    )
    .expect("create symlink");
    let output = run(&["manifests", "validate"], &root);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains(
        "  ERROR: Cannot read skills/linked-skill/SKILL.md: symbolic links are not followed\n"
    ));
}

// ---- spec validate (#4376) -------------------------------------------------

const SPEC_PLUGIN_ID: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// A minimal valid repository for `spec validate`: the vendored schemas plus
/// one plugin `demo` with one skill.
fn write_spec_repo(root: &Path) {
    for name in ["plugin.schema.json", "mcp.schema.json"] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/schemas/1.0.0")
            .join(name);
        let text = fs::read_to_string(source).expect("read vendored schema");
        write_file(root, &format!("tools/schemas/1.0.0/{name}"), &text);
    }
    write_file(
        root,
        "plugins/demo/plugin.json",
        &format!(r#"{{"$schema":"{SPEC_PLUGIN_ID}","name":"demo","version":"1.0.0"}}"#),
    );
    write_file(
        root,
        "plugins/demo/skills/demo-skill/SKILL.md",
        "---\nname: demo-skill\ndescription: A description that is long enough.\n---\nBody\n",
    );
}

#[test]
fn spec_validate_passes_on_a_valid_repository() {
    let root = temp_root("spec-ok");
    write_spec_repo(&root);
    let output = run(&["spec", "validate"], &root);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        stdout(&output),
        "Validating plugin: plugins/demo\n\
         Validating skills\n\
         \n\
         All Agent Plugins v1.0.0 checks passed.\n",
        "ROOT defaults to the current directory"
    );
}

#[test]
fn spec_validate_reports_findings_on_stderr_and_exits_one() {
    let root = temp_root("spec-fail");
    write_spec_repo(&root);
    write_file(
        &root,
        "plugins/demo/plugin.json",
        &format!(r#"{{"$schema":"{SPEC_PLUGIN_ID}","name":"demo","version":1}}"#),
    );
    write_file(&root, "plugins/demo/skills/demo-skill/SKILL.md", "plain\n");
    let output = run(
        &["spec", "validate", root.to_str().expect("utf-8 root")],
        Path::new("/"),
    );
    assert_eq!(code(&output), 1);
    assert_eq!(
        stdout(&output),
        "Validating plugin: plugins/demo\nValidating skills\n"
    );
    assert_eq!(
        stderr(&output),
        "  ERROR: plugins/demo/plugin.json: 1 is not of type 'string' (at version)\n\
         \x20 ERROR: plugins/demo/skills/demo-skill/SKILL.md: missing or unterminated YAML frontmatter\n\
         error: Spec validation failed with 2 error(s).\n"
    );
}

#[test]
fn spec_validate_missing_schemas_and_bad_arguments() {
    let root = temp_root("spec-noschema");
    fs::create_dir_all(root.join("plugins")).expect("create plugins");
    let output = run(&["spec", "validate"], &root);
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output)
            .starts_with("error: cannot read schema tools/schemas/1.0.0/plugin.schema.json: "),
        "{}",
        stderr(&output)
    );

    for args in [
        vec!["spec", "validate", "a", "b"],
        vec!["spec", "validate", "--plugin", "demo"],
        vec!["spec"],
    ] {
        let output = run(&args, &root);
        assert_eq!(code(&output), 2, "{args:?}");
        assert_eq!(stdout(&output), "");
        assert!(stderr(&output).starts_with("error: invalid arguments\n\nUsage:"));
    }
}

#[test]
fn spec_validate_passes_on_the_real_repository_tree() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = run(
        &["spec", "validate", repo.to_str().expect("utf-8 repo path")],
        &repo,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let text = stdout(&output);
    assert!(
        text.ends_with("\nAll Agent Plugins v1.0.0 checks passed.\n"),
        "{text}"
    );
    assert!(text.contains("Validating plugin: plugins/aws-core\n"));
}

#[cfg(unix)]
#[test]
fn spec_validate_rejects_symlinks_under_plugins() {
    use std::os::unix::fs::symlink;
    let root = temp_root("spec-link");
    write_spec_repo(&root);
    symlink("plugin.json", root.join("plugins/demo/link.json")).expect("create symlink");
    let output = run(&["spec", "validate"], &root);
    assert_eq!(code(&output), 1);
    assert_eq!(
        stderr(&output),
        "  ERROR: plugins/demo/link.json: symlinks are not allowed under plugins/\n\
         error: Spec validation failed with 1 error(s).\n"
    );
}

// ---- startup validators (#4399) ---------------------------------------------

fn real_startup_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

#[test]
fn startup_model_pricing_and_lint_pass_on_the_real_plugin_through_the_binary() {
    let plugin = real_startup_plugin();
    let root = plugin.to_str().expect("utf-8 plugin path");

    let output = run(
        &["startup", "model-pricing", "--plugin-root", root],
        &plugin,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        stdout(&output),
        "Model pricing coverage: all Anthropic candidates have numeric Active cache rows.\n"
    );

    // The plugin directory is also the default root.
    let output = run(&["startup", "model-id-lint"], &plugin);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert!(
        stdout(&output).starts_with("model-id lint: OK ("),
        "{}",
        stdout(&output)
    );
}

#[test]
fn startup_model_id_lint_reports_findings_on_stderr_and_exits_one() {
    let root = temp_root("startup-lint");
    write_file(
        &root,
        "skills/shared/ai/ai-model-lifecycle.md",
        "| Model | Model ID | Status |\n| - | - | - |\n| A | `acme.alpha-v1:0` | **excluded** |\n\n**Removed:**\n- Gone (`acme.gone-v1:0`)\n",
    );
    write_file(&root, "skills/x/SKILL.md", "use `acme.gone-v1:0`\n");
    let output = run(&["startup", "model-id-lint"], &root);
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "model-id lint: 1 problem(s)\n  - skills/x/SKILL.md:1: retired Bedrock model `acme.gone-v1:0` (excluded/removed per ai-model-lifecycle.md) used outside the model catalog\nerror: model-id lint found 1 problem(s)\n"
    );

    let missing = root.join("absent");
    let output = run(
        &[
            "startup",
            "model-id-lint",
            "--plugin-root",
            missing.to_str().expect("utf-8 path"),
        ],
        &root,
    );
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).starts_with("error: Plugin directory not found: "));
}

#[test]
fn startup_program_artifacts_exit_codes_and_streams() {
    let dir = temp_root("startup-program");
    let dir_arg = dir.to_str().expect("utf-8 dir");
    write_file(
        &dir,
        "preferences.json",
        r#"{"startup_constraints":{"startup_program_status":{"value":"unknown"}}}"#,
    );
    write_file(&dir, "STARTUP_PROGRAMS.md", "Your status: eligible\n");
    let output = run(
        &["startup", "program-artifacts", "--migration-dir", dir_arg],
        &dir,
    );
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "STARTUP_FAIL | startup-program-artifacts\n  - STARTUP_PROGRAMS.md: startup_program_status is \"unknown\" but content implies a confirmed Activate tier (your\\s+status:\\s*eligible)\nerror: 1 startup program artifact problem(s)\n"
    );

    write_file(&dir, "STARTUP_PROGRAMS.md", "No tier claimed.\n");
    let output = run(
        &["startup", "program-artifacts", "--migration-dir", dir_arg],
        &dir,
    );
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "STARTUP_OK | startup_program_status aligned with artifacts\n"
    );
    assert_eq!(stderr(&output), "");

    let output = run(&["startup", "program-artifacts"], &dir);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output)
        .starts_with("error: the following arguments are required: --migration-dir\n"));
}

#[test]
fn startup_artifacts_self_check_and_fixtures_pass_on_the_real_plugin_through_the_binary() {
    let plugin = real_startup_plugin();
    let root = plugin.to_str().expect("utf-8 plugin path");

    let output = run(
        &[
            "startup",
            "artifacts",
            "--self-check",
            "--plugin-root",
            root,
        ],
        &plugin,
    );
    assert_eq!(code(&output), 0, "stdout: {}", stdout(&output));
    assert_eq!(stderr(&output), "");
    assert!(
        stdout(&output)
            .ends_with("PASS — 0 artifact(s) checked, 0 error(s), 0 baselined, 0 info\n"),
        "{}",
        stdout(&output)
    );

    let output = run(&["startup", "artifacts", "--fixtures", "--json"], &plugin);
    assert_eq!(code(&output), 0, "stdout: {}", stdout(&output));
    assert_eq!(stderr(&output), "");
    assert!(
        stdout(&output).contains("\"errors\": []"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("\"stale_baseline_entries\": []"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn startup_artifacts_failures_use_stdout_for_the_report_and_stderr_for_the_summary() {
    let plugin = real_startup_plugin();
    let root = plugin.to_str().expect("utf-8 plugin path");
    let run_dir = temp_root("startup-artifacts");
    write_file(
        &run_dir,
        ".phase-status.json",
        r#"{"migration_id": "x", "last_updated": "2026-01-01T00:00:00Z", "phases": {}}"#,
    );
    let dir_arg = run_dir.to_str().expect("utf-8 dir");
    let output = run(
        &[
            "startup",
            "artifacts",
            "--run-dir",
            dir_arg,
            "--skill",
            "gcp-to-aws",
            "--plugin-root",
            root,
        ],
        &plugin,
    );
    assert_eq!(code(&output), 1);
    assert!(
        stdout(&output).contains("SCHEMA_VIOLATION"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("minProperties"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("\nFAIL — 1 artifact(s) checked, "),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );

    let output = run(&["startup", "artifacts"], &plugin);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output)
        .starts_with("error: exactly one of --self-check, --fixtures or --run-dir is required"));
}

#[test]
fn startup_emit_plan_writes_the_plan_and_fails_open_through_the_binary() {
    let root = temp_root("emit-plan");
    let plugin = root.join("plugin");
    let run_dir = root.join("run");
    write_file(
        &plugin,
        ".claude-plugin/plugin.json",
        r#"{"version": "9.8.7"}"#,
    );
    write_file(
        &run_dir,
        ".phase-status.json",
        r#"{"owning_skill": "HEROKU_TO_AWS", "run_id": "ABCDEF01-0000-4000-8000-000000000001"}"#,
    );
    write_file(
        &run_dir,
        "estimation-infra.json",
        r#"{"projected_costs": {"aws_monthly_balanced": 42.5}}"#,
    );
    let run_arg = run_dir.to_str().expect("utf-8 dir");
    let plugin_arg = plugin.to_str().expect("utf-8 dir");

    let output = run(
        &[
            "startup",
            "emit-plan",
            "--migration-dir",
            run_arg,
            "--plugin-root",
            plugin_arg,
        ],
        &root,
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("PLAN_OK | path={run_arg}/plan.json | platform=HEROKU | scope=INFRA_ONLY\n")
    );
    assert_eq!(stderr(&output), "");
    assert_eq!(
        fs::read_to_string(run_dir.join("plan.json")).expect("plan.json"),
        "{\n  \"schemaVersion\": 1,\n  \"sourcePlatform\": \"HEROKU\",\n  \"scope\": \"INFRA_ONLY\",\n  \"runId\": \"abcdef01-0000-4000-8000-000000000001\",\n  \"cost\": {\n    \"awsMonthly\": 42.5,\n    \"awsMonthlyBasis\": \"BALANCED\"\n  },\n  \"producerVersion\": \"9.8.7\"\n}\n"
    );

    // Fail-open: an unusable estimate is a skip with exit 0 and drops the plan.
    write_file(
        &run_dir,
        "estimation-infra.json",
        r#"{"projected_costs": {}}"#,
    );
    let output = run(&["startup", "emit-plan", "--migration-dir", run_arg], &root);
    assert_eq!(code(&output), 0);
    assert_eq!(
        stdout(&output),
        "PLAN_SKIP | reason=no usable projected_costs.aws_monthly_balanced\n"
    );
    assert!(!run_dir.join("plan.json").exists());

    // Only a usage error is non-zero.
    let output = run(&["startup", "emit-plan"], &root);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("--migration-dir"));
}

#[test]
fn startup_consent_audit_passes_on_the_real_plugin_and_fails_closed_through_the_binary() {
    let plugin = real_startup_plugin();
    let output = run(&["startup", "consent-audit"], &plugin);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        stdout(&output),
        "consent-disclosure audit: OK (6 discovery sub-file(s) checked)\n"
    );

    let root = temp_root("consent-audit");
    write_file(
        &root,
        "skills/demo/references/phases/discover/discover-demo-api.md",
        "This runs GET requests only.\n\n## Step 2: Capture\n\n| # | Endpoint | Purpose |\n| - | - | - |\n| 1 | `POST /x` | y |\n",
    );
    let root_arg = root.to_str().expect("utf-8 dir");
    let output = run(
        &["startup", "consent-audit", "--plugin-root", root_arg],
        &root,
    );
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).starts_with("consent-disclosure audit: 1 problem(s)\n  - skills/demo/references/phases/discover/discover-demo-api.md:1: claims a GET-only restriction"),
        "{}",
        stderr(&output)
    );

    let output = run(
        &[
            "startup",
            "consent-audit",
            "--plugin-root",
            "does-not-exist",
        ],
        &root,
    );
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("Plugin directory not found: "));
}
