//! T-AR-GIT-1..6: Git ignore rules on temporary repositories built with the
//! `git` binary (no network).

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::*;
use crate::launch::archive::fsops::open_root;
use crate::launch::clock::SeededRandom;
use crate::launch::tests_support::Scratch;

/// Run the test's own `git`, isolated from the user's configuration.
pub fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

fn select(
    runner: &GitRunner,
    root: &Path,
    temp: &Path,
) -> Result<Option<HashSet<String>>, LaunchError> {
    let handle = open_root(root).expect("root");
    git_included_files(runner, root, &handle, temp, &SeededRandom::new(1))
}

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(ToString::to_string).collect()
}

fn script(dir: &Scratch, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.write(name, body.as_bytes());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn t_ar_git_1_tracked_and_untracked_files_minus_ignored() {
    let repo = Scratch::new("git1");
    let temp = Scratch::new("git1-tmp");
    git(repo.path(), &["init", "-q"]);
    repo.write(".gitignore", b"*.log\nbuild-out/\n");
    repo.write("tracked.txt", b"t");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    repo.write("new.txt", b"n");
    repo.write("debug.log", b"d");
    repo.write("build-out/x", b"x");
    repo.write("dir with space/\u{e9}.txt", b"u");
    let selected = select(&GitRunner::system(), repo.path(), temp.path()).expect("git");
    assert_eq!(
        selected,
        Some(set(&[
            ".gitignore",
            "tracked.txt",
            "new.txt",
            "dir with space/\u{e9}.txt"
        ]))
    );
}

#[test]
fn t_ar_git_2_ignored_but_tracked_files_are_excluded() {
    let repo = Scratch::new("git2");
    let temp = Scratch::new("git2-tmp");
    git(repo.path(), &["init", "-q"]);
    repo.write("keep.log", b"k");
    repo.write("a.txt", b"a");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    repo.write(".gitignore", b"*.log\n");
    let selected = select(&GitRunner::system(), repo.path(), temp.path()).expect("git");
    assert_eq!(selected, Some(set(&[".gitignore", "a.txt"])));
}

#[test]
fn t_ar_git_3_sub_directory_root() {
    let repo = Scratch::new("git3");
    let temp = Scratch::new("git3-tmp");
    git(repo.path(), &["init", "-q"]);
    repo.write("outside.txt", b"o");
    repo.write("sub/a.txt", b"a");
    repo.write("sub/deeper/b.txt", b"b");
    repo.write("sub/.gitignore", b"ignored.txt\n");
    repo.write("sub/ignored.txt", b"i");
    git(repo.path(), &["add", "outside.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    let selected = select(&GitRunner::system(), &repo.join("sub"), temp.path()).expect("git");
    assert_eq!(
        selected,
        Some(set(&["a.txt", "deeper/b.txt", ".gitignore"]))
    );
}

#[test]
fn t_ar_git_4_non_repository_with_a_gitignore() {
    let tree = Scratch::new("git4");
    let temp = Scratch::new("git4-tmp");
    tree.write(".gitignore", b"*.tmp\n");
    tree.write("a.txt", b"a");
    tree.write("b.tmp", b"b");
    tree.write("nested/c.txt", b"c");
    tree.write("nested/d.tmp", b"d");
    let selected = select(&GitRunner::system(), tree.path(), temp.path()).expect("git");
    assert_eq!(
        selected,
        Some(set(&[".gitignore", "a.txt", "nested/c.txt"]))
    );
    assert_eq!(
        fs::read_dir(temp.path()).expect("list").count(),
        0,
        "temporary metadata removed"
    );
    // A `.gitignore` only under a skipped directory does not count.
    let plain = Scratch::new("git4-plain");
    plain.write("a.txt", b"a");
    plain.write("node_modules/.gitignore", b"*\n");
    assert_eq!(
        select(&GitRunner::system(), plain.path(), temp.path()),
        Ok(None)
    );
    let handle = open_root(plain.path()).expect("root");
    assert!(!has_gitignore(&handle));
    plain.write("deep/er/.gitignore", b"x\n");
    assert!(has_gitignore(&handle));
}

#[test]
fn t_ar_git_5_repository_fsmonitor_hook_never_runs() {
    let repo = Scratch::new("git5");
    let temp = Scratch::new("git5-tmp");
    let tools = Scratch::new("git5-tools");
    let marker = tools.join("fsmonitor-ran");
    let hook = script(
        &tools,
        "fsmonitor.sh",
        &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    );
    git(repo.path(), &["init", "-q"]);
    repo.write("a.txt", b"a");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    git(
        repo.path(),
        &["config", "core.fsmonitor", hook.to_str().expect("utf-8")],
    );
    repo.write("b.txt", b"b");
    let selected = select(&GitRunner::system(), repo.path(), temp.path()).expect("git");
    assert_eq!(selected, Some(set(&["a.txt", "b.txt"])));
    assert!(
        !marker.exists(),
        "core.fsmonitor=false kept the hook from running"
    );
}

#[test]
fn t_ar_git_6_missing_git_timeouts_and_refused_output() {
    let missing = GitRunner {
        program: "/nonexistent/toolkit-test/git".into(),
        ..GitRunner::system()
    };
    let temp = Scratch::new("git6-tmp");
    // No rules apply: every file.
    let plain = Scratch::new("git6-plain");
    plain.write("a.txt", b"a");
    assert_eq!(select(&missing, plain.path(), temp.path()), Ok(None));
    // Rules apply (a .gitignore, or Git metadata): Git is required.
    let ignored = Scratch::new("git6-ignored");
    ignored.write(".gitignore", b"x\n");
    assert_eq!(
        select(&missing, ignored.path(), temp.path()),
        Err(LaunchError::Archive(message::REQUIRED.to_string()))
    );
    let repo = Scratch::new("git6-repo");
    git(repo.path(), &["init", "-q"]);
    assert_eq!(
        select(&missing, repo.path(), temp.path()),
        Err(LaunchError::Archive(message::REQUIRED.to_string()))
    );
    assert!(has_git_metadata(&repo.join("x/y")));
    assert!(!has_git_metadata(plain.path()));
    // A Git that never answers is killed at the (test-shortened) limit.
    let tools = Scratch::new("git6-tools");
    let sleeper = GitRunner {
        program: script(&tools, "git-sleeps", "#!/bin/sh\nsleep 30\n"),
        timeout: Duration::from_millis(300),
        ..GitRunner::system()
    };
    let started = Instant::now();
    assert_eq!(
        select(&sleeper, plain.path(), temp.path()),
        Err(LaunchError::Archive(message::TIMEOUT.to_string()))
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // Output over the cap is refused, never truncated into a selection.
    let chatty = GitRunner {
        program: script(
            &tools,
            "git-chatty",
            "#!/bin/sh\nprintf 'aaaaaaaaaaaaaaaa\\0bbbbbbbb\\0'\n",
        ),
        stdout_cap: 8,
        ..GitRunner::system()
    };
    assert_eq!(
        select(&chatty, plain.path(), temp.path()),
        Err(LaunchError::Archive(message::SELECTION.to_string()))
    );
    // A non-UTF-8 path from Git is the R-AR-SEL refusal.
    let binary = GitRunner {
        program: script(
            &tools,
            "git-binary",
            "#!/bin/sh\nprintf 'ok\\0bad\\377\\0'\n",
        ),
        ..GitRunner::system()
    };
    assert_eq!(
        select(&binary, plain.path(), temp.path()),
        Err(LaunchError::Archive(
            "Archive cannot include a file whose name is not valid UTF-8: bad\u{fffd}".to_string()
        ))
    );
    // A Git that fails outside a repository with no rules: every file.
    let failing = GitRunner {
        program: script(&tools, "git-fails", "#!/bin/sh\nexit 128\n"),
        ..GitRunner::system()
    };
    assert_eq!(select(&failing, plain.path(), temp.path()), Ok(None));
    assert_eq!(
        select(&failing, ignored.path(), temp.path()),
        Err(LaunchError::Archive(message::INIT.to_string()))
    );
    assert_eq!(
        select(&failing, repo.path(), temp.path()),
        Err(LaunchError::Archive(message::SELECTION.to_string()))
    );
}
