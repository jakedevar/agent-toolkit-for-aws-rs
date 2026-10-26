//! Integration tests that exercise the built `toolkit` binary end to end:
//! real process spawns, real exit codes and separated stdout/stderr.
//!
//! Fixtures are deterministic offline directories created under
//! `std::env::temp_dir()`; no network, cloud or live service is involved.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

fn temp_root(label: &str) -> PathBuf {
    let unique = format!(
        "toolkit-cli-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    fs::create_dir_all(&path).expect("create temp root");
    path
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

fn cleanup(root: &Path) {
    fs::remove_dir_all(root).expect("remove temp root");
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
    cleanup(&root);
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
    cleanup(&root);
}

#[test]
fn list_defaults_to_skills_root_in_current_directory() {
    let cwd = temp_root("default-root");
    skill(&cwd, "skills/inside", Some("inside"), None);

    let output = run(&["skills", "list"], &cwd);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "inside\t\n");
    assert_eq!(stderr(&output), "");
    cleanup(&cwd);
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
    cleanup(&root);
}

#[test]
fn missing_root_fails_on_stderr_with_exit_one() {
    let cwd = temp_root("missing-root");

    let output = run(&["skills", "list", "does-not-exist"], &cwd);
    assert_eq!(code(&output), 1, "missing root is a runtime error");
    assert_eq!(stdout(&output), "", "no data on stdout for errors");
    assert!(stderr(&output).starts_with("error: "));
    assert!(stderr(&output).contains("root does not exist"));
    cleanup(&cwd);
}

#[test]
fn file_root_fails_with_exit_one() {
    let cwd = temp_root("file-root");
    fs::write(cwd.join("not-a-dir"), "x").expect("write file");

    let output = run(&["skills", "list", "not-a-dir"], &cwd);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("root is not a directory"));
    assert_eq!(stdout(&output), "");
    cleanup(&cwd);
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
    cleanup(&cwd);
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
    cleanup(&cwd);
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
    cleanup(&cwd);
}
