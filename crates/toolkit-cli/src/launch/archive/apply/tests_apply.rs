//! T-PLAN-8 (R-AR-PLAN's limits in the apply walk) and T-APPLY-1..10
//! (R-AR-APPLY) on temporary repositories built with the `git` binary.

use std::fs;

use super::*;
use crate::launch::archive::tests_snapshot::{repo, sha_tree, Repo};
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
    assert_eq!((walked.files, walked.directories, walked.bytes), (101, 100, 101));
}
