//! T-APPLY-11..17 (R-AR-META): the repository's actual Git metadata, on
//! temporary repositories built with the `git` binary. Each layout is first
//! asserted with `git rev-parse`; "unchanged" is the SHA-256 of every file
//! below the target (metadata included); the marker check runs `git status`
//! with the repository's own configuration.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::tests_apply::{assert_no_marker, marker_helper, snapshot};
use super::*;
use crate::launch::archive::meta::{discovery_message, metadata_message, reason};
use crate::launch::archive::tests_snapshot::{run_git, sha_tree, tree};
use crate::launch::tests_support::{test_git, Scratch};
use toolkit_aws::MapEnv;

/// Apply with every knob.
fn apply_opts(
    source: &Path,
    into: &Path,
    git: &GitRunner,
    env: &MapEnv,
    rules: MetaRules,
    hook: &mut dyn ApplyHook,
) -> Result<Applied, LaunchError> {
    let random = crate::launch::clock::SeededRandom::new(9);
    let mut options = ApplyOptions {
        git,
        env,
        limits: ApplyLimits::PRODUCTION,
        rules,
        hook,
        random: &random,
    };
    apply_snapshot(source, into, &mut options)
}

fn apply_rules(source: &Path, into: &Path, rules: MetaRules) -> Result<Applied, LaunchError> {
    apply_opts(
        source,
        into,
        &test_git(),
        &MapEnv::new(),
        rules,
        &mut NoApplyHook,
    )
}

fn apply_default(source: &Path, into: &Path) -> Result<Applied, LaunchError> {
    apply_rules(source, into, MetaRules::PRODUCTION)
}

fn rev_parse(dir: &Path) -> (PathBuf, PathBuf) {
    let out = run_git(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    );
    let mut lines = out.lines().map(PathBuf::from);
    (lines.next().unwrap(), lines.next().unwrap())
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

/// `<T>` with `git init --separate-git-dir=<admin> <T>` and one commit.
fn separate(work: &Path, admin: &Path) {
    run_git(
        work,
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", admin.display()),
            ".",
        ],
    );
    fs::write(work.join("README.md"), b"base\n").unwrap();
    run_git(work, &["add", "README.md"]);
    run_git(work, &["commit", "-q", "-m", "base"]);
}

fn fsmonitor_config(real: &Path) -> Vec<u8> {
    let mut config = fs::read(real).unwrap();
    config.extend_from_slice(b"[core]\n\tfsmonitor = ./helper\n");
    config
}

#[test]
fn t_apply_11_in_tree_separate_git_dir_is_refused_by_each_rule_alone() {
    let target = Scratch::new("apply11");
    let admin = target.join("repo-admin");
    separate(target.path(), &admin);
    assert_eq!(
        rev_parse(target.path()),
        (canonical(&admin), canonical(&admin))
    );
    let marker = target.join("../apply11-marker");
    let config = fsmonitor_config(&admin.join("config"));
    let helper = marker_helper(&marker);
    let source = snapshot(
        "apply11-snap",
        &[
            ("repo-admin/config", &config, 0o644),
            ("helper", &helper, 0o755),
            ("repo-admin/hooks/pre-commit", b"#!/bin/sh\n", 0o755),
        ],
    );
    let expected = metadata_message(&crate::launch::text::shown(
        &target.join("repo-admin").to_string_lossy(),
    ));
    for rules in [
        MetaRules::PRODUCTION,
        MetaRules {
            identity: false,
            shaped: true,
        },
        MetaRules {
            identity: true,
            shaped: false,
        },
    ] {
        let before = sha_tree(target.path());
        let error = apply_rules(source.path(), target.path(), rules).expect_err("refused");
        assert_eq!(error.message(), expected, "{rules:?}");
        assert_eq!(sha_tree(target.path()), before, "{rules:?}");
        assert_no_marker(target.path(), &marker);
    }
}

#[test]
fn t_apply_12_common_dir_indirection_in_a_linked_worktree_is_refused() {
    let main = Scratch::new("apply12-main");
    run_git(main.path(), &["init", "-q", "."]);
    main.write("README.md", b"base\n");
    run_git(main.path(), &["add", "README.md"]);
    run_git(main.path(), &["commit", "-q", "-m", "base"]);
    let holder = Scratch::new("apply12-wt");
    let work = holder.join("wt");
    run_git(
        main.path(),
        &["worktree", "add", "-q", "-b", "wt", &work.to_string_lossy()],
    );
    // Move the common directory and the per-worktree directory into the
    // work tree under ordinary names and re-link them (what `git worktree
    // repair` records).
    let common = work.join("common-admin");
    let per = work.join("wt-admin");
    fs::rename(main.join(".git"), &common).unwrap();
    fs::rename(common.join("worktrees/wt"), &per).unwrap();
    fs::write(work.join(".git"), format!("gitdir: {}\n", per.display())).unwrap();
    fs::write(per.join("commondir"), format!("{}\n", common.display())).unwrap();
    fs::write(
        per.join("gitdir"),
        format!("{}\n", work.join(".git").display()),
    )
    .unwrap();
    assert_eq!(rev_parse(&work), (canonical(&per), canonical(&common)));
    let marker = holder.join("apply12-marker");
    let helper = marker_helper(&marker);
    let common_config = fsmonitor_config(&common.join("config"));
    for files in [
        vec![
            ("common-admin/config", &common_config[..], 0o644),
            ("helper", &helper[..], 0o755),
        ],
        vec![
            (
                "wt-admin/config.worktree",
                &b"[core]\n\tfsmonitor = ./helper\n"[..],
                0o644,
            ),
            ("helper", &helper[..], 0o755),
        ],
    ] {
        let source = snapshot("apply12-snap", &files);
        let before = sha_tree(&work);
        let error = apply_default(source.path(), &work).expect_err("refused");
        assert!(
            error
                .message()
                .starts_with("The snapshot would write into Git metadata at "),
            "{error:?}"
        );
        assert_eq!(sha_tree(&work), before);
        assert_no_marker(&work, &marker);
    }
}

#[test]
fn t_apply_13_ordinary_overlay_with_an_external_git_dir_applies() {
    let target = Scratch::new("apply13");
    let external = Scratch::new("apply13-admin");
    let admin = external.join("admin");
    separate(target.path(), &admin);
    assert_eq!(rev_parse(target.path()).0, canonical(&admin));
    let source = snapshot("apply13-snap", &[("src/a.rs", b"a\n", 0o644)]);
    let before = sha_tree(external.path());
    let applied = apply_default(source.path(), target.path()).expect("applies");
    assert_eq!((applied.files, applied.directories), (1, 1));
    assert_eq!(fs::read(target.join("src/a.rs")).unwrap(), b"a\n");
    assert_eq!(sha_tree(external.path()), before);
}

#[test]
fn t_apply_14_target_root_inside_metadata_is_refused() {
    let target = Scratch::new("apply14");
    let admin = target.join("repo-admin");
    separate(target.path(), &admin);
    target.write("sub/keep.txt", b"keep\n");
    let source = snapshot("apply14-snap", &[("x.txt", b"x", 0o644)]);
    for into in [admin.clone(), admin.join("hooks")] {
        let before = sha_tree(target.path());
        let error = apply_default(source.path(), &into).expect_err("refused");
        assert!(
            error
                .message()
                .starts_with("apply-snapshot could not locate")
                || error
                    .message()
                    .starts_with("The snapshot would write into Git metadata"),
            "{into:?}: {error:?}"
        );
        assert_eq!(sha_tree(target.path()), before);
    }
    let ordinary = crate::launch::archive::tests_snapshot::repo("apply14-ordinary");
    let before = sha_tree(ordinary.dir.path());
    let error = apply_default(source.path(), &ordinary.dir.join(".git")).expect_err("refused");
    // Git refuses `--show-toplevel` inside the Git directory (`git_failed`);
    // either way the discovery fails closed.
    assert!(
        error
            .message()
            .starts_with("apply-snapshot could not locate"),
        "{error:?}"
    );
    assert_eq!(sha_tree(ordinary.dir.path()), before);
    let applied = apply_default(source.path(), &target.join("sub")).expect("applies in sub");
    assert_eq!(applied.files, 1);
    assert_eq!(fs::read(target.join("sub/x.txt")).unwrap(), b"x");
}

#[test]
fn t_apply_15_nested_admin_dirs_bare_repos_and_shaped_snapshot_dirs() {
    let ordinary = crate::launch::archive::tests_snapshot::repo("apply15");
    let target = ordinary.dir.path();
    fs::create_dir_all(target.join("vendor/x")).unwrap();
    separate(&target.join("vendor/x"), &target.join("vendor/x-admin"));
    fs::create_dir_all(target.join("tools")).unwrap();
    run_git(&target.join("tools"), &["init", "-q", "--bare", "lib.git"]);
    let refusals: Vec<Vec<(&str, &[u8], u32)>> = vec![
        vec![("vendor/x-admin/config", b"[core]\n", 0o644)],
        vec![("tools/lib.git/hooks/x", b"#!/bin/sh\n", 0o755)],
        vec![
            ("newmeta/HEAD", b"ref: refs/heads/main\n", 0o644),
            ("newmeta/objects/.keep", b"", 0o644),
            ("newmeta/refs/.keep", b"", 0o644),
        ],
        vec![("newmeta/commondir", b"..\n", 0o644)],
    ];
    for (index, files) in refusals.iter().enumerate() {
        let source = snapshot("apply15-snap", files);
        let before = sha_tree(target);
        let error = apply_default(source.path(), target).expect_err("refused");
        let text = error.message();
        if index < 2 {
            assert!(
                text.starts_with("The snapshot would write into Git metadata at "),
                "{text}"
            );
        } else {
            assert_eq!(
                text,
                "The snapshot contains a directory shaped like Git metadata: newmeta."
            );
        }
        assert_eq!(sha_tree(target), before, "{index}");
    }
    let source = snapshot("apply15-ok", &[("plain/HEAD", b"not git\n", 0o644)]);
    apply_default(source.path(), target).expect("a lone HEAD file applies");
    assert_eq!(fs::read(target.join("plain/HEAD")).unwrap(), b"not git\n");
}

fn fake_git(dir: &Scratch, body: &str) -> GitRunner {
    let path = dir.write("fake-git", format!("#!/bin/sh\n{body}\n").as_bytes());
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    GitRunner {
        program: path,
        ..test_git()
    }
}

#[test]
fn t_apply_16_discovery_fails_closed_before_the_snapshot_is_read() {
    let ordinary = crate::launch::archive::tests_snapshot::repo("apply16");
    let target = ordinary.dir.path();
    let tools = Scratch::new("apply16-tools");
    // A snapshot path that does not exist: reading it first would fail with
    // another message.
    let missing = tools.join("no-such-snapshot");
    let check = |git: &GitRunner, env: &MapEnv, into: &Path, expected: String| {
        let before = sha_tree(target);
        let error = apply_opts(
            &missing,
            into,
            git,
            env,
            MetaRules::PRODUCTION,
            &mut NoApplyHook,
        )
        .expect_err("refused");
        assert_eq!(error.message(), expected);
        assert_eq!(sha_tree(target), before);
    };
    let none = MapEnv::new();
    let missing_git = GitRunner {
        program: tools.join("no-such-git"),
        ..test_git()
    };
    check(
        &missing_git,
        &none,
        target,
        discovery_message(reason::GIT_MISSING),
    );
    let outside = Scratch::new("apply16-outside");
    check(
        &test_git(),
        &none,
        outside.path(),
        discovery_message(reason::GIT_FAILED),
    );
    let five = fake_git(&tools, "printf 'true\\nfalse\\nfalse\\n/a\\n/b\\n'");
    check(&five, &none, target, discovery_message(reason::LINES));
    let relative = fake_git(&tools, "printf 'true\\nfalse\\nfalse\\n/a\\nrel/b\\n/c\\n'");
    check(
        &relative,
        &none,
        target,
        discovery_message(reason::RELATIVE),
    );
    let old = fake_git(
        &tools,
        "printf -- '--path-format=absolute\\ntrue\\nfalse\\nfalse\\n/a\\n/b\\n'",
    );
    check(&old, &none, target, discovery_message(reason::OLD_GIT));
    let slow = GitRunner {
        timeout: Duration::from_millis(200),
        ..fake_git(&tools, "sleep 5")
    };
    check(&slow, &none, target, discovery_message(reason::TIMEOUT));
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_COUNT",
    ] {
        check(
            &test_git(),
            &MapEnv::new().with(name, "x"),
            target,
            format!("apply-snapshot does not run with {name} set; unset it and re-run."),
        );
    }
}

#[test]
fn t_apply_17_git_dir_moved_into_a_planned_directory_after_planning_stops() {
    struct Swap(PathBuf);
    impl ApplyHook for Swap {
        fn between_phases(&mut self) {
            fs::rename(self.0.join("docs"), self.0.join("docs-away")).unwrap();
            fs::rename(self.0.join(".git"), self.0.join("docs")).unwrap();
        }
    }
    let ordinary = crate::launch::archive::tests_snapshot::repo("apply17");
    let target = ordinary.dir.path().to_path_buf();
    ordinary.dir.write("docs/old.md", b"old\n");
    let metadata = sha_tree(&target.join(".git"));
    let source = snapshot(
        "apply17-snap",
        &[("docs/hooks/post-checkout", b"#!/bin/sh\n", 0o755)],
    );
    let error = apply_opts(
        source.path(),
        &target,
        &test_git(),
        &MapEnv::new(),
        MetaRules::PRODUCTION,
        &mut Swap(target.clone()),
    )
    .expect_err("stopped");
    assert!(
        error
            .message()
            .starts_with("Applying the snapshot stopped after 0 files: "),
        "{error:?}"
    );
    assert_eq!(
        sha_tree(&target.join("docs")),
        metadata,
        "metadata unchanged"
    );
    assert!(tree(&target.join("docs-away")).contains(&"old.md".to_string()));
}
