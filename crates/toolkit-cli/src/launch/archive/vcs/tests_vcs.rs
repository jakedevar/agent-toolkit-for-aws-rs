//! T-AR-VCS-1..8: version-control names in snapshots, through the Snapshot
//! profile of the reader (before anything could be created) and the
//! normaliser table.

use super::*;
use crate::launch::archive::tests_zipfx::{Fx, FxEntry};
use crate::launch::archive::zip_read::{read_archive, Profile, ReadStats};
use crate::launch::archive::ArchiveLimits;

fn snapshot(entries: Vec<FxEntry>) -> Result<usize, LaunchError> {
    let bytes = Fx::of(entries).build().bytes;
    read_archive(
        &bytes,
        Profile::Snapshot,
        &ArchiveLimits::PRODUCTION,
        &mut ReadStats::default(),
    )
    .map(|archive| archive.entries().len())
}

#[track_caller]
fn assert_refused(entries: Vec<FxEntry>, shown: &str) {
    assert_eq!(
        snapshot(entries),
        Err(LaunchError::Archive(format!(
            "The snapshot contains version-control metadata and was not extracted: {shown}."
        )))
    );
}

#[test]
fn t_ar_vcs_1_git_config_hooks_and_helper_are_refused() {
    assert_refused(
        vec![
            FxEntry::stored("src/main.rs", b"fn main() {}"),
            FxEntry::stored(".git/config", b"[core]\n\tfsmonitor = ./helper\n"),
            FxEntry::stored("helper", b"#!/bin/sh\ntouch /tmp/pwned\n"),
            FxEntry::stored(".git/hooks/post-checkout", b"#!/bin/sh\n"),
        ],
        ".git/config",
    );
    assert_refused(vec![FxEntry::dir(".git/")], ".git/");
}

#[test]
fn t_ar_vcs_2_nested_git_as_file_and_directory() {
    assert_refused(
        vec![FxEntry::stored("sub/.git", b"gitdir: ../x\n")],
        "sub/.git",
    );
    assert_refused(
        vec![FxEntry::stored("sub/.git/HEAD", b"ref: x\n")],
        "sub/.git/HEAD",
    );
}

#[test]
fn t_ar_vcs_3_case_aliases() {
    assert_refused(vec![FxEntry::stored(".GIT/config", b"x")], ".GIT/config");
    assert_refused(vec![FxEntry::stored(".Git/hooks/x", b"x")], ".Git/hooks/x");
}

#[test]
fn t_ar_vcs_4_hfs_ignorable_code_points() {
    assert_refused(
        vec![FxEntry::stored(".g\u{200c}it/config", b"x")],
        ".g\\u200cit/config",
    );
    assert!(is_vcs_component("\u{feff}.git"));
    assert!(is_vcs_component(".gi\u{206f}t"));
}

#[test]
fn t_ar_vcs_5_ntfs_aliases() {
    for (name, shown) in [
        (".git./x", ".git./x"),
        (".git /x", ".git /x"),
        ("git~1/config", "git~1/config"),
        (".git::$DATA/x", ".git::$DATA/x"),
        (".git::$INDEX_ALLOCATION/x", ".git::$INDEX_ALLOCATION/x"),
        ("GIT~12/x", "GIT~12/x"),
    ] {
        assert_refused(vec![FxEntry::stored(name, b"x")], shown);
    }
}

#[test]
fn t_ar_vcs_6_other_version_control_systems() {
    for name in [
        ".hg/hgrc",
        ".svn/x",
        ".bzr/x",
        "_darcs/x",
        "CVS/Entries",
        ".jj/x",
        ".pijul/x",
        ".sl/x",
        ".fslckout",
        "_FOSSIL_",
    ] {
        assert_refused(vec![FxEntry::stored(name, b"x")], name);
    }
}

#[test]
fn t_ar_vcs_7_ordinary_names_are_not_denied() {
    assert_eq!(
        snapshot(vec![
            FxEntry::stored(".gitignore", b"target\n"),
            FxEntry::stored(".gitattributes", b"* text=auto\n"),
            FxEntry::stored(".gitmodules", b""),
            FxEntry::stored(".github/workflows/ci.yml", b"on: push\n"),
            FxEntry::stored("git/README", b"x"),
            FxEntry::stored("my.git/x", b"x"),
            FxEntry::stored("x.git", b"x"),
            FxEntry::stored(".gitkeep", b""),
            FxEntry::stored("cvs-export/x", b"x"),
            FxEntry::stored("git~/x", b"x"),
        ]),
        Ok(10)
    );
    check_snapshot_name("src/.github/x").expect("allowed");
}

#[test]
fn t_ar_vcs_8_normaliser_table() {
    type Alias = fn(&str) -> String;
    let aliases: [(&str, Alias); 9] = [
        ("identity", |name| name.to_string()),
        ("upper", |name| name.to_uppercase()),
        ("title", |name| {
            let mut chars = name.chars();
            chars.next().map_or_else(String::new, |first| {
                first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect()
            })
        }),
        ("trailing dot", |name| format!("{name}.")),
        ("trailing dots and spaces", |name| format!("{name}. . ")),
        ("stream", |name| format!("{name}::$DATA")),
        ("named stream", |name| format!("{name}:zone.identifier")),
        ("hfs ignorable", |name| {
            let mut out = String::new();
            for (index, c) in name.chars().enumerate() {
                if index == 1 {
                    out.push('\u{200d}');
                }
                out.push(c);
            }
            out
        }),
        ("trailing ignorable", |name| format!("{name}\u{200e}")),
    ];
    for name in VCS_NAMES {
        for (label, alias) in aliases {
            let variant = alias(name);
            assert!(is_vcs_component(&variant), "{name} / {label}: {variant:?}");
        }
    }
    for short in [
        "git~1", "GIT~1", "hg~2", "svn~10", "bzr~9", "git~1.", "git~1 ",
    ] {
        assert!(is_vcs_component(short), "{short}");
    }
    for allowed in [
        "git",
        "git~",
        "git~x",
        "gitx~1",
        "x~1",
        ".gitx",
        "x.git",
        ".gi",
        ".gitignore",
        ".github",
        "hg",
        "svn",
        "_darcs_",
        "cvsroot",
        ".",
        "",
    ] {
        assert!(!is_vcs_component(allowed), "{allowed:?}");
    }
    assert_eq!(normalise_component(".Git. "), ".git");
    assert_eq!(normalise_component("CVS"), "cvs");
    assert_eq!(normalise_component(".g\u{200c}it::$DATA"), ".git");
    // NFC: a decomposed name folds to the composed one.
    assert_eq!(
        normalise_component("e\u{301}"),
        normalise_component("\u{e9}")
    );
}
