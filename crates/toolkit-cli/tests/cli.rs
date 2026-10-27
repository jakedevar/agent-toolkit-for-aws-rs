//! Integration tests that exercise the built `toolkit` binary end to end:
//! real process spawns, real exit codes and separated stdout/stderr.
//!
//! Fixtures are deterministic offline directories created under
//! `std::env::temp_dir()`; no network, cloud or live service is involved.

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
