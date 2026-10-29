//! T-STAGE-1, 3..9 (R-AR-STAGE) and T-PLAN-6 (the pre-sized ledger) on the
//! real file system. T-STAGE-2's "no request" half runs end to end in
//! `download/tests_ext.rs`; its refusal half is here.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use super::*;
use crate::launch::archive::tests_snapshot::{
    extract_to, mode, names, partials, sample_bytes, tree,
};
use crate::launch::archive::tests_zipfx::{Fx, FxEntry};
use crate::launch::tests_support::Scratch;
use crate::launch::text::shown;

/// Runs `action` once at `at`.
struct At<F: FnMut()> {
    point: HookPoint,
    action: F,
    fired: bool,
}

impl<F: FnMut()> ExtractHook for At<F> {
    fn before(&mut self, point: HookPoint) {
        if point == self.point && !self.fired {
            self.fired = true;
            (self.action)();
        }
    }
}

fn at<F: FnMut()>(point: HookPoint, action: F) -> At<F> {
    At {
        point,
        action,
        fired: false,
    }
}

/// The one staging directory in `dir`.
fn staging_in(dir: &Path) -> PathBuf {
    let found = partials(dir);
    assert_eq!(found.len(), 1, "{found:?}");
    dir.join(&found[0])
}

/// The sample snapshot's tree, as `tree` lists it.
pub(crate) fn sample_tree() -> Vec<String> {
    [
        "README.md",
        "bin/",
        "bin/run.sh",
        "empty/",
        "src/",
        "src/lib/",
        "src/lib/util.rs",
        "src/main.rs",
    ]
    .iter()
    .map(|name| (*name).to_string())
    .collect()
}

#[test]
fn t_stage_1_private_parent_extracts_with_exact_modes_and_no_partial() {
    let scratch = Scratch::new("stage1");
    let out = scratch.join("out");
    let (result, stats) = extract_to(&sample_bytes(), &out, &mut NoExtractHook);
    let extracted = result.expect("extracts");
    assert_eq!(
        extracted,
        Extracted {
            files: 4,
            directories: 4,
            bytes: 9 + 13 * 40 + 17 + 19,
        }
    );
    assert_eq!(tree(&out), sample_tree());
    assert_eq!(mode(&out), 0o700);
    for dir in ["bin", "empty", "src", "src/lib"] {
        assert_eq!(mode(&out.join(dir)), 0o755, "{dir}");
    }
    for file in ["README.md", "src/main.rs", "src/lib/util.rs"] {
        assert_eq!(mode(&out.join(file)), 0o644, "{file}");
    }
    assert_eq!(mode(&out.join("bin/run.sh")), 0o755);
    assert_eq!(
        fs::read(out.join("src/main.rs")).unwrap(),
        b"fn main() {}\n".repeat(40)
    );
    assert_eq!(fs::read(out.join("README.md")).unwrap(), b"# sample\n");
    assert!(partials(scratch.path()).is_empty());
    assert_eq!(names(scratch.path()), vec!["out".to_string()]);
    assert_eq!(stats.len_at_stop, 8);
    assert_eq!(stats.removed, 0);
}

#[test]
fn t_stage_2_writable_parent_without_sticky_is_refused_before_anything() {
    for bits in [0o770, 0o707, 0o777] {
        let scratch = Scratch::new("stage2");
        let parent = scratch.join("shared");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(bits)).unwrap();
        let out = parent.join("out");
        let error = prepare_output(&out).expect_err("refused");
        assert_eq!(
            error,
            LaunchError::Archive(parent_policy_message(&shown(&out.to_string_lossy())))
        );
        assert!(names(&parent).is_empty(), "{bits:o}");
    }
}

#[test]
fn t_stage_3_sticky_world_writable_parent_owned_by_the_user_is_accepted() {
    let scratch = Scratch::new("stage3");
    let parent = scratch.join("sticky");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o1777)).unwrap();
    let out = parent.join("out");
    let (result, _) = extract_to(&sample_bytes(), &out, &mut NoExtractHook);
    result.expect("sticky parent accepted");
    assert_eq!(tree(&out), sample_tree());
}

#[test]
fn t_stage_4_policy_over_injected_stat_values() {
    let me = 1000;
    let cases = [
        (me, 0o40_700, true),
        (me, 0o40_755, true),
        (me, 0o40_775, false),
        (me, 0o40_757, false),
        (me, 0o41_777, true),
        (me, 0o41_770, true),
        (0, 0o41_777, true),
        (0, 0o40_755, true),
        (0, 0o40_777, false),
        (2000, 0o40_700, false),
        (2000, 0o41_777, false),
        (2000, 0o40_755, false),
    ];
    for (uid, mode, allowed) in cases {
        assert_eq!(
            parent_policy_allows(ParentFacts { uid, mode }, me),
            allowed,
            "uid {uid} mode {mode:o}"
        );
    }
}

#[test]
fn t_stage_5_staging_swapped_for_a_symlink_before_the_first_creation() {
    swap_staging_at(HookPoint::BeforeCreate { index: 0 });
}

#[test]
fn t_stage_6_staging_swapped_in_the_middle_of_extraction() {
    swap_staging_at(HookPoint::BeforeCreate { index: 5 });
}

fn swap_staging_at(point: HookPoint) {
    let scratch = Scratch::new("stage-swap");
    let parent = scratch.join("p");
    let outside = scratch.join("outside");
    fs::create_dir(&parent).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::create_dir(outside.join("tree")).unwrap();
    let out = parent.join("out");
    let moved = scratch.join("p/moved-staging");
    let mut hook = at(point, || {
        let staging = staging_in(&parent);
        fs::rename(&staging, &moved).unwrap();
        symlink(&outside, &staging).unwrap();
    });
    let (result, stats) = extract_to(&sample_bytes(), &out, &mut hook);
    assert!(hook.fired);
    assert!(result.is_err(), "nothing is published");
    assert!(!out.exists());
    assert!(fs::symlink_metadata(&out).is_err());
    assert_eq!(names(&outside), vec!["tree".to_string()]);
    assert!(names(&outside.join("tree")).is_empty(), "nothing outside");
    // Cleanup removed every created entry through the held descriptors (the
    // moved staging is empty) and left the symlink it did not create.
    assert!(names(&moved).is_empty(), "{:?}", tree(&moved));
    assert_eq!(stats.left, 0);
    assert_eq!(stats.removed, stats.len_at_stop);
    let link = staging_in(&parent);
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn t_stage_7_ancestor_of_the_parent_replaced_after_open() {
    let scratch = Scratch::new("stage7");
    let ancestor = scratch.join("a");
    fs::create_dir_all(ancestor.join("p")).unwrap();
    let decoy = scratch.join("decoy");
    fs::create_dir_all(decoy.join("p")).unwrap();
    let out = ancestor.join("p/out");
    let renamed = scratch.join("a-moved");
    let mut hook = at(HookPoint::BeforeCreate { index: 0 }, || {
        fs::rename(&ancestor, &renamed).unwrap();
        symlink(&decoy, &ancestor).unwrap();
    });
    let (result, _) = extract_to(&sample_bytes(), &out, &mut hook);
    result.expect("writes stay in the held parent");
    assert_eq!(tree(&renamed.join("p/out")), sample_tree());
    assert!(names(&decoy.join("p")).is_empty(), "nothing at the target");
}

#[test]
fn t_stage_8_directory_created_at_the_destination_before_publication() {
    let scratch = Scratch::new("stage8");
    let out = scratch.join("out");
    let out_for_hook = out.clone();
    let mut hook = at(HookPoint::BeforePublish, move || {
        fs::create_dir(&out_for_hook).unwrap();
        fs::write(out_for_hook.join("intruder"), b"keep").unwrap();
    });
    let (result, stats) = extract_to(&sample_bytes(), &out, &mut hook);
    assert_eq!(
        result,
        Err(LaunchError::Archive(exists_message(&shown(
            &out.to_string_lossy()
        ))))
    );
    assert_eq!(names(&out), vec!["intruder".to_string()]);
    assert_eq!(fs::read(out.join("intruder")).unwrap(), b"keep");
    assert!(partials(scratch.path()).is_empty());
    assert_eq!(stats.removed, stats.len_at_stop);
}

#[test]
fn t_stage_9_symlinked_parent_is_followed_once_and_bad_final_components_refused() {
    let scratch = Scratch::new("stage9");
    let real = scratch.join("real");
    fs::create_dir(&real).unwrap();
    symlink(&real, scratch.join("link")).unwrap();
    let (result, _) = extract_to(
        &sample_bytes(),
        &scratch.join("link/out"),
        &mut NoExtractHook,
    );
    result.expect("symlinked parent followed");
    assert_eq!(tree(&real.join("out")), sample_tree());

    for bad in [
        format!("{}/.", real.display()),
        format!("{}/..", real.display()),
        format!("{}/", real.display()),
        String::new(),
        ".".to_string(),
    ] {
        let before = names(&real);
        let error = prepare_output(Path::new(&bad)).expect_err("refused");
        assert!(
            error
                .message()
                .starts_with("The output directory must end in a name"),
            "{bad:?}: {error:?}"
        );
        assert_eq!(names(&real), before);
    }
    let error = prepare_output(&real.join("out")).expect_err("exists");
    assert_eq!(
        error,
        LaunchError::Archive(exists_message(&shown(&real.join("out").to_string_lossy())))
    );
    let error = prepare_output(&scratch.join("missing/out")).expect_err("no parent");
    assert!(error.message().contains("does not exist"), "{error:?}");
}

#[test]
fn t_plan_6_crc_failure_in_the_last_file_cleans_exactly_the_ledger() {
    let scratch = Scratch::new("plan6");
    let mut bad = FxEntry::stored("b/z", b"last");
    bad.crc ^= 1;
    let bytes = Fx::of(vec![
        FxEntry::stored("a/x", b"x"),
        FxEntry::deflated("a/y", &b"y".repeat(500)),
        bad,
    ])
    .build()
    .bytes;
    let out = scratch.join("out");
    let (result, stats) = extract_to(&bytes, &out, &mut NoExtractHook);
    assert_eq!(
        result,
        Err(LaunchError::Archive(
            "The archive is not a valid ZIP file (crc_mismatch).".to_string()
        ))
    );
    // a, b, x, y, z: every node was created before the last file's data
    // failed, so the ledger holds the whole plan.
    assert_eq!(stats.plan_len, 5);
    assert_eq!(stats.capacity_reserved, 5);
    assert_eq!(stats.capacity_max, 5, "the ledger never grew");
    assert_eq!(stats.len_at_stop, 5);
    assert_eq!(stats.removed, 5);
    assert_eq!(stats.left, 0);
    assert!(fs::symlink_metadata(&out).is_err());
    assert!(names(scratch.path()).is_empty());
}
