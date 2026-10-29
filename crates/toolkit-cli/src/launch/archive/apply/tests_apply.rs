//! T-PLAN-8 (R-AR-PLAN's limits in the apply walk) and T-APPLY-1..10
//! (R-AR-APPLY) on temporary repositories built with the `git` binary.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::launch::archive::tests_snapshot::{repo, run_git, sha_tree, tree, Repo};
use crate::launch::tests_support::Scratch;

/// Apply `snapshot` into `repo` with production settings and `limits`.
pub fn apply_with(
    snapshot: &Path,
    into: &Path,
    limits: ApplyLimits,
    hook: &mut dyn ApplyHook,
) -> Result<Applied, LaunchError> {
    let git = crate::launch::tests_support::test_git();
    let env = toolkit_aws::MapEnv::new();
    let random = crate::launch::clock::SeededRandom::new(5);
    let mut options = ApplyOptions {
        git: &git,
        env: &env,
        limits,
        rules: MetaRules::PRODUCTION,
        hook,
        random: &random,
    };
    apply_snapshot(snapshot, into, &mut options)
}

#[test]
fn t_plan_8_apply_walk_refuses_too_many_files_and_nodes_before_any_write() {
    assert_eq!(ApplyLimits::PRODUCTION.max_files, 100_000);
    assert_eq!(ApplyLimits::PRODUCTION.max_nodes, 200_000);
    let Repo { dir: target, .. } = repo("plan8");
    let before = sha_tree(target.path());

    // 100 001 empty files at the production constants.
    let many = Scratch::new("plan8-files");
    for index in 0..=100_000 {
        fs::write(many.join(&format!("f{index}")), b"").unwrap();
    }
    let error = apply_with(
        many.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("too many files");
    assert_eq!(
        error.message(),
        "The snapshot has more than 100000 files and was not applied."
    );
    assert_eq!(sha_tree(target.path()), before);

    // 201 nodes against an injected node budget of 200: 100 directories of
    // one file each, plus one more file.
    let nodes = Scratch::new("plan8-nodes");
    for index in 0..100 {
        nodes.write(&format!("d{index}/f"), b"x");
    }
    nodes.write("last", b"x");
    let limits = ApplyLimits {
        max_files: 1_000,
        max_nodes: 200,
        max_bytes: ApplyLimits::PRODUCTION.max_bytes,
    };
    let error = apply_with(nodes.path(), target.path(), limits, &mut NoApplyHook)
        .expect_err("too many nodes");
    assert_eq!(
        error.message(),
        "The snapshot would create more than 200 files and directories and was not applied."
    );
    assert_eq!(sha_tree(target.path()), before);

    // The walk alone gives the same answers and the counts at the limit.
    let limits = ApplyLimits {
        max_nodes: 201,
        ..limits
    };
    let walked = walk_snapshot(nodes.path(), &limits).expect("at the limit");
    assert_eq!(
        (walked.files, walked.directories, walked.bytes),
        (101, 100, 101)
    );
}

/// A snapshot directory holding `files` (`path`, bytes, mode).
pub fn snapshot(label: &str, files: &[(&str, &[u8], u32)]) -> Scratch {
    let dir = Scratch::new(label);
    for (path, bytes, mode) in files {
        let written = dir.write(path, bytes);
        fs::set_permissions(&written, fs::Permissions::from_mode(*mode)).unwrap();
    }
    dir
}

/// A `core.fsmonitor` helper that creates `marker` when Git runs it.
pub fn marker_helper(marker: &Path) -> Vec<u8> {
    format!("#!/bin/sh\ntouch '{}'\n", marker.display()).into_bytes()
}

/// `git status` with the repository's own configuration; the marker must
/// not appear.
#[track_caller]
pub fn assert_no_marker(repo: &Path, marker: &Path) {
    let _ = run_git(repo, &["status", "--porcelain"]);
    assert!(!marker.exists(), "the fsmonitor helper ran");
}

/// Every temporary file apply could have left below `dir`.
pub fn leftovers(dir: &Path) -> Vec<String> {
    tree(dir)
        .into_iter()
        .filter(|path| path.contains(".launch-") && path.rsplit('.').next() == Some("tmp"))
        .collect()
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn t_apply_1_normal_snapshot_adds_and_replaces_byte_exact_and_deletes_nothing() {
    let Repo { dir: target } = repo("apply1");
    target.write("src/old.rs", b"old\n");
    target.write("keep.txt", b"keep\n");
    let source = snapshot(
        "apply1-snap",
        &[
            ("README.md", b"new readme\n", 0o600),
            ("src/new.rs", b"fn new() {}\n", 0o644),
            ("bin/tool", b"#!/bin/sh\n", 0o700),
            ("docs/guide.md", b"guide\n", 0o640),
        ],
    );
    let applied = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect("applies");
    assert_eq!(
        applied,
        Applied {
            files: 4,
            directories: 2,
            into: target.path().to_path_buf(),
        }
    );
    assert_eq!(fs::read(target.join("README.md")).unwrap(), b"new readme\n");
    assert_eq!(
        fs::read(target.join("src/new.rs")).unwrap(),
        b"fn new() {}\n"
    );
    assert_eq!(fs::read(target.join("bin/tool")).unwrap(), b"#!/bin/sh\n");
    assert_eq!(fs::read(target.join("docs/guide.md")).unwrap(), b"guide\n");
    assert_eq!(mode_of(&target.join("README.md")), 0o644);
    assert_eq!(mode_of(&target.join("docs/guide.md")), 0o644);
    assert_eq!(mode_of(&target.join("bin/tool")), 0o755);
    assert_eq!(mode_of(&target.join("bin")), 0o755);
    assert_eq!(fs::read(target.join("src/old.rs")).unwrap(), b"old\n");
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep\n");
    assert!(leftovers(target.path()).is_empty());
}

#[test]
fn t_apply_2_planted_dot_git_config_is_refused_and_metadata_unchanged() {
    let Repo { dir: target } = repo("apply2");
    let marker = target.join("../apply2-marker");
    let mut config = fs::read(target.join(".git/config")).unwrap();
    config.extend_from_slice(b"[core]\n\tfsmonitor = ./helper\n");
    let helper = marker_helper(&marker);
    let source = snapshot(
        "apply2-snap",
        &[(".git/config", &config, 0o644), ("helper", &helper, 0o755)],
    );
    let before = sha_tree(target.path());
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("refused");
    assert_eq!(
        error.message(),
        "The snapshot contains version-control metadata and was not applied: .git."
    );
    assert_eq!(sha_tree(target.path()), before);
    assert_no_marker(target.path(), &marker);
}

#[test]
fn t_apply_3_case_alias_of_dot_git_is_refused() {
    let Repo { dir: target } = repo("apply3");
    let source = snapshot(
        "apply3-snap",
        &[(".GIT/hooks/pre-commit", b"#!/bin/sh\n", 0o755)],
    );
    let before = sha_tree(target.path());
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("refused");
    assert!(
        error.message().contains("version-control metadata"),
        "{error:?}"
    );
    assert_eq!(sha_tree(target.path()), before);
}

#[test]
fn t_apply_4_symlink_in_the_snapshot_is_refused() {
    let Repo { dir: target } = repo("apply4");
    let source = snapshot("apply4-snap", &[("a.txt", b"a", 0o644)]);
    std::os::unix::fs::symlink("/etc/passwd", source.join("link")).unwrap();
    let before = sha_tree(target.path());
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("refused");
    assert_eq!(
        error.message(),
        "The snapshot contains a symlink or special file and was not applied: link."
    );
    assert_eq!(sha_tree(target.path()), before);
}

#[test]
fn t_apply_5_fifo_in_the_snapshot_is_refused_without_blocking() {
    let Repo { dir: target } = repo("apply5");
    let source = snapshot("apply5-snap", &[("a.txt", b"a", 0o644)]);
    let status = std::process::Command::new("mkfifo")
        .arg(source.join("pipe"))
        .status()
        .expect("mkfifo");
    assert!(status.success());
    let before = sha_tree(target.path());
    let started = std::time::Instant::now();
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("refused");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(error.message().contains("special file"), "{error:?}");
    assert_eq!(sha_tree(target.path()), before);
}

#[test]
fn t_apply_6_target_directory_symlink_is_refused_and_outside_untouched() {
    let Repo { dir: target } = repo("apply6");
    let outside = Scratch::new("apply6-outside");
    std::os::unix::fs::symlink(outside.path(), target.join("docs")).unwrap();
    let source = snapshot("apply6-snap", &[("docs/x", b"x", 0o644)]);
    let before = sha_tree(target.path());
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("refused");
    assert!(
        error.message().starts_with("The target has a symlink at "),
        "{error:?}"
    );
    assert_eq!(sha_tree(target.path()), before);
    assert!(tree(outside.path()).is_empty());
}

#[test]
fn t_apply_7_target_symlink_at_a_file_position_is_refused() {
    let Repo { dir: target } = repo("apply7");
    let outside = Scratch::new("apply7-outside");
    let victim = outside.write("victim", b"original");
    fs::remove_file(target.join("README.md")).unwrap();
    std::os::unix::fs::symlink(&victim, target.join("README.md")).unwrap();
    let source = snapshot("apply7-snap", &[("README.md", b"replaced", 0o644)]);
    let before = sha_tree(target.path());
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut NoApplyHook,
    )
    .expect_err("refused");
    assert!(
        error.message().starts_with("The target has a symlink at "),
        "{error:?}"
    );
    assert_eq!(sha_tree(target.path()), before);
    assert_eq!(fs::read(&victim).unwrap(), b"original");
}

#[test]
fn t_apply_8_file_directory_conflicts_are_refused_before_writes() {
    let Repo { dir: target } = repo("apply8");
    target.write("src", b"a file named src");
    fs::create_dir(target.join("notes")).unwrap();
    for (label, files) in [
        (
            "apply8-a",
            vec![("a.txt", &b"a"[..], 0o644), ("src/x.rs", b"x", 0o644)],
        ),
        (
            "apply8-b",
            vec![("a.txt", &b"a"[..], 0o644), ("notes", b"n", 0o644)],
        ),
    ] {
        let source = snapshot(label, &files);
        let before = sha_tree(target.path());
        let error = apply_with(
            source.path(),
            target.path(),
            ApplyLimits::PRODUCTION,
            &mut NoApplyHook,
        )
        .expect_err("refused");
        assert!(
            error.message().starts_with("The target has a "),
            "{error:?}"
        );
        assert_eq!(sha_tree(target.path()), before, "{label}");
    }
}

/// Runs `action` between the phases.
struct Between<F: FnMut()>(F);

impl<F: FnMut()> ApplyHook for Between<F> {
    fn between_phases(&mut self) {
        (self.0)();
    }
}

#[test]
fn t_apply_9_directory_swapped_for_a_symlink_after_planning_stops_that_write() {
    let Repo { dir: target } = repo("apply9");
    target.write("docs/old.md", b"old");
    let outside = Scratch::new("apply9-outside");
    let source = snapshot(
        "apply9-snap",
        &[("a.txt", b"a", 0o644), ("docs/x.md", b"x", 0o644)],
    );
    let docs = target.join("docs");
    let away = target.join("docs-away");
    let outside_path = outside.path().to_path_buf();
    let mut hook = Between(|| {
        fs::rename(&docs, &away).unwrap();
        std::os::unix::fs::symlink(&outside_path, &docs).unwrap();
    });
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut hook,
    )
    .expect_err("stopped");
    assert!(
        error
            .message()
            .starts_with("Applying the snapshot stopped after 1 files: "),
        "{error:?}"
    );
    assert!(error
        .message()
        .ends_with(" Review the working tree with git status."));
    assert!(tree(outside.path()).is_empty(), "nothing outside");
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"a");
    assert!(leftovers(target.path()).is_empty());
}

#[test]
fn t_apply_9_directory_swapped_for_another_directory_after_planning_stops_that_write() {
    // A real directory, not a symlink, replaces the planned `docs`:
    // O_NOFOLLOW lets the walk in, so only the recorded identity stops it.
    let Repo { dir: target } = repo("apply9d");
    target.write("docs/old.md", b"old");
    let outside = Scratch::new("apply9d-outside");
    outside.write("impostor/kept.md", b"kept");
    let source = snapshot(
        "apply9d-snap",
        &[("a.txt", b"a", 0o644), ("docs/x.md", b"x", 0o644)],
    );
    let docs = target.join("docs");
    let away = target.join("docs-away");
    let impostor = outside.join("impostor");
    let mut hook = Between(|| {
        fs::rename(&docs, &away).unwrap();
        fs::rename(&impostor, &docs).unwrap();
    });
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut hook,
    )
    .expect_err("stopped");
    let expected = stopped(
        1,
        &format!(
            "a target directory changed after planning: {}",
            shown(&target.join("docs").to_string_lossy())
        ),
    );
    assert_eq!(error.message(), expected.message());
    assert_eq!(tree(&target.join("docs")), vec!["kept.md".to_string()]);
    assert_eq!(tree(&away), vec!["old.md".to_string()]);
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"a");
    assert!(leftovers(target.path()).is_empty());
}

/// Fails before the rename of file `at`.
struct FailAt(usize);

impl ApplyHook for FailAt {
    fn before_rename(&mut self, index: usize) -> io::Result<()> {
        if index == self.0 {
            Err(io::Error::other("injected write failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn t_apply_10_injected_write_failure_halfway_stops_and_removes_the_temporary() {
    let Repo { dir: target } = repo("apply10");
    let source = snapshot(
        "apply10-snap",
        &[
            ("a.txt", b"a", 0o644),
            ("b.txt", b"b", 0o644),
            ("c.txt", b"c", 0o644),
        ],
    );
    let error = apply_with(
        source.path(),
        target.path(),
        ApplyLimits::PRODUCTION,
        &mut FailAt(1),
    )
    .expect_err("stopped");
    assert_eq!(
        error.message(),
        "Applying the snapshot stopped after 1 files: injected write failure. Review the \
         working tree with git status."
    );
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"a");
    assert!(!target.join("b.txt").exists());
    assert!(!target.join("c.txt").exists());
    assert!(leftovers(target.path()).is_empty());
}
