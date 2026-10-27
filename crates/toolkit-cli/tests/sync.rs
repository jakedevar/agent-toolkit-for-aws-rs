//! End-to-end tests for `toolkit sync plugin-skills` and `toolkit sync
//! vendored` (#4377) through the built binary: real exit codes, separated
//! stdout/stderr, deterministic offline temp fixtures. The last tests run
//! check mode (read-only) on the real repository tree.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

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
        "toolkit-sync-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write");
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

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn plugin_skills_fixture() -> TempDir {
    let root = temp_root("plugin-skills");
    write(&root, "skills/group/alpha/SKILL.md", "alpha v2\n");
    write(&root, "skills/group/alpha/references/x.md", "x\n");
    write(&root, "plugins/p1/skills/alpha/SKILL.md", "alpha v1\n");
    write(&root, "plugins/p1/skills/alpha/stale.md", "stale\n");
    write(&root, "plugins/p1/skills/unrelated/SKILL.md", "mine\n");
    root
}

const VEND: &str = "skills/alpha/references/vendored";

fn vendored_fixture() -> TempDir {
    let plugin = temp_root("vendored");
    write(
        &plugin,
        "skills/shared/dsl/INTERPRETER.md",
        "canonical v2\n",
    );
    write(&plugin, "skills/shared/pricing.json", "{\"a\": 1}\n");
    write(
        &plugin,
        &format!("{VEND}/dsl/INTERPRETER.md"),
        "canonical v2\n",
    );
    write(&plugin, &format!("{VEND}/pricing.json"), "{\"a\": 0}\n");
    write(&plugin, &format!("{VEND}/orphan.md"), "no source\n");
    write(
        &plugin,
        &format!("{VEND}/README.md"),
        "| Vendored path | Canonical source |\n| --- | --- |\n\
         | `dsl/INTERPRETER.md` | `skills/shared/dsl/INTERPRETER.md` |\n\
         | `pricing.json` | `skills/shared/pricing.json` |\n\
         | `orphan.md` | `skills/shared/orphan.md` |\n",
    );
    plugin
}

#[test]
fn plugin_skills_check_reports_drift_writes_nothing_and_exits_one() {
    let root = plugin_skills_fixture();
    let output = run(&["sync", "plugin-skills", "--check"], &root);
    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "Checking plugin: p1\n");
    assert_eq!(
        stderr(&output),
        "error: 1 error(s):\n  p1: alpha is out of sync with skills/group/alpha\n\n\
         Run 'toolkit sync plugin-skills' to fix.\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("plugins/p1/skills/alpha/SKILL.md")).expect("read"),
        "alpha v1\n"
    );
    assert!(root.join("plugins/p1/skills/alpha/stale.md").exists());
}

#[test]
fn plugin_skills_write_syncs_then_check_passes() {
    let root = plugin_skills_fixture();
    let output = run(&["sync", "plugin-skills"], &root);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        stdout(&output),
        "Syncing plugin: p1\n  synced alpha <- skills/group/alpha\n\nAll plugin skills are in sync.\n"
    );
    assert!(!root.join("plugins/p1/skills/alpha/stale.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("plugins/p1/skills/alpha/references/x.md")).expect("read"),
        "x\n"
    );
    let output = run(&["sync", "plugin-skills", "--check"], &root);
    assert_eq!(code(&output), 0);
    assert_eq!(
        stdout(&output),
        "Checking plugin: p1\n\nAll plugin skills are in sync.\n"
    );
}

#[test]
fn plugin_skills_value_flags_accept_both_spellings_and_reject_bad_use() {
    let root = plugin_skills_fixture();
    let elsewhere = temp_root("elsewhere");
    let root_arg = root.to_str().expect("utf-8 root");
    let spaced = run(
        &[
            "sync",
            "plugin-skills",
            "--root",
            root_arg,
            "--plugin",
            "p1",
            "--check",
        ],
        &elsewhere,
    );
    assert_eq!(code(&spaced), 1);
    assert_eq!(stdout(&spaced), "Checking plugin: p1\n");
    let joined_root = format!("--root={root_arg}");
    let joined = run(
        &[
            "sync",
            "plugin-skills",
            &joined_root,
            "--plugin=p1",
            "--check",
        ],
        &elsewhere,
    );
    assert_eq!(code(&joined), 1);

    let missing = run(
        &[
            "sync",
            "plugin-skills",
            "--plugin",
            "nope",
            "--root",
            root_arg,
        ],
        &elsewhere,
    );
    assert_eq!(code(&missing), 1);
    assert_eq!(stdout(&missing), "");
    assert_eq!(stderr(&missing), "error: Plugin not found: nope\n");

    for bad in [
        vec!["sync", "plugin-skills", "--plugin"],
        vec!["sync", "plugin-skills", "--plugin", "--check"],
        vec!["sync", "plugin-skills", "--plugin="],
        vec!["sync", "plugin-skills", "stray"],
        vec!["sync", "plugin-skills", "--wat"],
        vec!["sync", "vendored", "--plugin"],
        vec!["sync", "plugin-skills", "--plugin-root", "x"],
    ] {
        let output = run(&bad, &elsewhere);
        assert_eq!(code(&output), 2, "{bad:?}");
        assert!(stderr(&output).contains("Usage:"));
    }
    let traversal = run(
        &[
            "sync",
            "plugin-skills",
            "--root",
            root_arg,
            "--plugin",
            "../skills",
        ],
        &elsewhere,
    );
    assert_eq!(code(&traversal), 1);
    assert!(stderr(&traversal).starts_with("error: Invalid plugin name"));
}

#[test]
fn plugin_skills_without_a_plugins_directory_is_a_runtime_error() {
    let root = temp_root("no-plugins");
    let output = run(&["sync", "plugin-skills", "--check"], &root);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).starts_with("error: plugins directory not found: "));
}

#[cfg(unix)]
#[test]
fn plugin_skills_never_follows_or_copies_links_out_of_the_tree() {
    use std::os::unix::fs::symlink;
    let outside = temp_root("plugin-skills-outside");
    write(&outside, "secret.txt", "TOP SECRET\n");
    let root = plugin_skills_fixture();
    symlink(
        outside.join("secret.txt"),
        root.join("skills/group/alpha/leak.md"),
    )
    .expect("link");
    for args in [
        ["sync", "plugin-skills", "--check"],
        ["sync", "plugin-skills", "--root=."],
    ] {
        let output = run(&args, &root);
        assert_eq!(code(&output), 1, "{args:?}");
        assert!(
            stderr(&output).contains(
                "canonical source skills/group/alpha contains a symbolic link at leak.md"
            ),
            "{}",
            stderr(&output)
        );
        assert!(!stdout(&output).contains("synced"));
    }
    assert!(!root.join("plugins/p1/skills/alpha/leak.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("plugins/p1/skills/alpha/SKILL.md")).expect("read"),
        "alpha v1\n",
        "a refused skill is left exactly as it was"
    );
}

#[test]
fn vendored_check_fails_with_stdout_report_and_a_stderr_summary() {
    let plugin = vendored_fixture();
    let plugin_arg = plugin.to_str().expect("utf-8 path");
    let output = run(
        &["sync", "vendored", "--check", "--plugin-root", plugin_arg],
        &plugin,
    );
    assert_eq!(code(&output), 1);
    let out = stdout(&output);
    assert!(
        out.starts_with("FAIL — 2 vendored file(s) out of contract:\n"),
        "{out}"
    );
    assert!(out.contains(&format!(
        "  - {VEND}/pricing.json: differs from skills/shared/pricing.json — run `toolkit sync vendored`\n"
    )));
    assert!(out.contains(&format!(
        "  - {VEND}/orphan.md: no canonical source at skills/shared/orphan.md"
    )));
    assert_eq!(
        stderr(&output),
        "error: 2 vendored file(s) out of contract\n"
    );
    assert_eq!(
        fs::read_to_string(plugin.join(format!("{VEND}/pricing.json"))).expect("read"),
        "{\"a\": 0}\n",
        "check mode writes nothing"
    );
}

#[test]
fn vendored_sync_then_check_pass_after_the_orphan_is_resolved() {
    let plugin = vendored_fixture();
    let plugin_arg = format!("--plugin-root={}", plugin.to_str().expect("utf-8 path"));
    fs::remove_file(plugin.join(format!("{VEND}/orphan.md"))).expect("rm");
    let readme = plugin.join(format!("{VEND}/README.md"));
    let kept: Vec<String> = fs::read_to_string(&readme)
        .expect("read")
        .lines()
        .filter(|line| !line.contains("orphan"))
        .map(str::to_string)
        .collect();
    fs::write(&readme, kept.join("\n") + "\n").expect("write");

    let output = run(&["sync", "vendored", &plugin_arg], &plugin);
    assert_eq!(code(&output), 0, "stdout: {}", stdout(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "  synced {VEND}/pricing.json <- skills/shared/pricing.json\nOK — 2 vendored file(s) checked, 1 synced\n"
        )
    );
    assert_eq!(stderr(&output), "");
    let output = run(&["sync", "vendored", "--check", &plugin_arg], &plugin);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).starts_with("PASS"), "{}", stdout(&output));
}

#[test]
fn vendored_plugin_root_without_skills_passes_and_a_missing_root_fails() {
    let plugin = temp_root("mcp-only");
    let plugin_arg = plugin.to_str().expect("utf-8 path");
    let output = run(
        &["sync", "vendored", "--check", "--plugin-root", plugin_arg],
        &plugin,
    );
    assert_eq!(code(&output), 0);
    assert!(
        stdout(&output).starts_with("PASS — 0 vendored file(s)"),
        "{}",
        stdout(&output)
    );
    let missing = plugin.join("absent");
    let output = run(
        &[
            "sync",
            "vendored",
            "--check",
            "--plugin-root",
            missing.to_str().expect("utf-8 path"),
        ],
        &plugin,
    );
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).starts_with("error: plugin root is not a directory: "));
}

#[cfg(unix)]
#[test]
fn vendored_sync_does_not_write_through_links_or_follow_readme_traversal() {
    use std::os::unix::fs::symlink;
    let outside = temp_root("vendored-outside");
    write(&outside, "victim.md", "untouched\n");
    let plugin = vendored_fixture();
    let plugin_arg = plugin.to_str().expect("utf-8 path");
    // The drifted vendored copy is replaced by a link to an outside file.
    let pricing = plugin.join(format!("{VEND}/pricing.json"));
    fs::remove_file(&pricing).expect("rm");
    symlink(outside.join("victim.md"), &pricing).expect("link");
    // A README row that tries to climb out of the vendored directory.
    let readme = plugin.join(format!("{VEND}/README.md"));
    let mut text = fs::read_to_string(&readme).expect("read");
    text.push_str("| `../../../../../escape-cli-sync.md` | `skills/shared/pricing.json` |\n");
    fs::write(&readme, text).expect("write");

    let output = run(&["sync", "vendored", "--plugin-root", plugin_arg], &plugin);
    assert_eq!(code(&output), 1);
    assert!(
        stdout(&output).contains("is not a safe relative path"),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        fs::read_to_string(outside.join("victim.md")).expect("read"),
        "untouched\n"
    );
    assert_eq!(
        fs::read_to_string(&pricing).expect("read"),
        "{\"a\": 1}\n",
        "the link itself was replaced by the canonical bytes"
    );
    assert!(!std::env::temp_dir().join("escape-cli-sync.md").exists());
}

#[test]
fn real_tree_is_in_sync_for_plugin_skills() {
    let output = run(&["sync", "plugin-skills", "--check"], &repo_root());
    assert_eq!(
        code(&output),
        0,
        "stdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stderr(&output), "");
    assert!(stdout(&output).ends_with("\nAll plugin skills are in sync.\n"));
    assert!(stdout(&output).contains("Checking plugin: aws-core\n"));
}

#[test]
fn real_tree_is_in_sync_for_vendored_copies() {
    let output = run(&["sync", "vendored", "--check"], &repo_root());
    assert_eq!(
        code(&output),
        0,
        "stdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stderr(&output), "");
    let out = stdout(&output);
    assert!(out.starts_with("PASS — "), "{out}");
    assert!(
        out.contains(
            "vendored file(s) byte-identical to skills/shared/ and matching their README tables"
        ),
        "{out}"
    );
    assert!(
        !out.starts_with("PASS — 0 "),
        "the real plugin vendors files: {out}"
    );
}
