//! `toolkit launch download-snapshot` and `toolkit launch apply-snapshot` at
//! binary level (#5317): missing arguments, the output check before any
//! request, the default build's network refusal and one real apply into a
//! temporary repository built with the `git` binary. No network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("l6-{label}-{}-{unique}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
}

fn toolkit(args: &[&str], home: &Path, cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("LAUNCH_WITH_AWS_REGION")
        .env_remove("LAUNCH_WITH_AWS_BASE_URL")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn missing_positionals_keep_the_python_error_json() {
    let home = scratch("missing");
    for (command, name) in [
        ("download-snapshot", "download-snapshot"),
        ("apply-snapshot", "apply-snapshot"),
    ] {
        let output = toolkit(&["launch", command], &home, &home);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            text(&output.stderr),
            format!("{{\"error\":\"Missing required argument for {name}\"}}\n")
        );
    }
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(unix)]
#[test]
fn an_existing_output_is_refused_before_any_request() {
    let home = scratch("exists");
    let out = home.join("out");
    std::fs::create_dir(&out).unwrap();
    let output = toolkit(
        &[
            "launch",
            "download-snapshot",
            "l-1",
            "--output",
            &out.to_string_lossy(),
            "--region",
            "us-east-1",
        ],
        &home,
        &home,
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "{{\"error\":\"Output directory already exists: {}.\"}}\n",
            out.display()
        )
    );
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(all(unix, not(feature = "aws-sdk")))]
#[test]
fn the_default_build_refuses_the_network_and_creates_nothing() {
    let home = scratch("nonet");
    let out = home.join("out");
    let output = toolkit(
        &[
            "launch",
            "download-snapshot",
            "l-1",
            "--output",
            &out.to_string_lossy(),
        ],
        &home,
        &home,
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "{\"error\":\"This toolkit build has no network support; install a build with the \
         aws-sdk feature.\"}\n"
    );
    assert!(!out.exists());
    let names: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy() != ".launch-with-aws")
        .collect();
    assert!(names.is_empty(), "{names:?}");
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "init.defaultBranch=main"])
        // No detached auto maintenance after `commit` (#5370).
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[cfg(unix)]
#[test]
fn apply_snapshot_overlays_the_work_tree_and_prints_the_summary() {
    let home = scratch("apply");
    let repo = home.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "."]);
    std::fs::write(repo.join("README.md"), b"base\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let snapshot = home.join("snapshot");
    std::fs::create_dir_all(snapshot.join("src")).unwrap();
    std::fs::write(snapshot.join("README.md"), b"migrated\n").unwrap();
    std::fs::write(snapshot.join("src/main.rs"), b"fn main() {}\n").unwrap();
    let output = toolkit(
        &["launch", "apply-snapshot", &snapshot.to_string_lossy()],
        &home,
        &repo,
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "{\"applied\":true,\"files\":2,\"directories\":1,\"into\":\".\"}\n"
    );
    assert_eq!(
        std::fs::read(repo.join("README.md")).unwrap(),
        b"migrated\n"
    );
    assert_eq!(
        std::fs::read(repo.join("src/main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    // A snapshot carrying Git metadata is refused with nothing written.
    std::fs::create_dir_all(snapshot.join(".git")).unwrap();
    std::fs::write(snapshot.join(".git/config"), b"[core]\n").unwrap();
    std::fs::write(snapshot.join("README.md"), b"again\n").unwrap();
    let output = toolkit(
        &[
            "launch",
            "apply-snapshot",
            &snapshot.to_string_lossy(),
            "--into",
            &repo.to_string_lossy(),
        ],
        &home,
        &home,
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "{\"error\":\"The snapshot contains version-control metadata and was not applied: \
         .git.\"}\n"
    );
    assert_eq!(
        std::fs::read(repo.join("README.md")).unwrap(),
        b"migrated\n"
    );
    std::fs::remove_dir_all(&home).unwrap();
}
