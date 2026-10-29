//! Test-only helpers shared by the L6 tests (T-STAGE, T-PLAN-6..8, T-EXT,
//! T-APPLY): a valid byte-built snapshot, a one-call extraction and small
//! file-system observations.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::extract::{extract, prepare_output, ExtractHook, Extracted, LedgerStats};
use super::plan::{plan, PlanStats, MAX_SNAPSHOT_NODES};
use super::tests_zipfx::{Fx, FxEntry};
use super::zip_read::{read_archive, Profile, ReadStats};
use super::ArchiveLimits;
use crate::launch::clock::SeededRandom;
use crate::launch::LaunchError;

/// An executable stored file (`0o100755`).
pub fn executable(name: &str, content: &[u8]) -> FxEntry {
    FxEntry {
        external: 0o100_755 << 16,
        ..FxEntry::deflated(name, content)
    }
}

/// The sample snapshot: two top-level files, a deflated file in a nested
/// directory, an executable, an explicit empty directory.
pub fn sample_entries() -> Vec<FxEntry> {
    vec![
        FxEntry::stored("README.md", b"# sample\n"),
        FxEntry::deflated("src/main.rs", &b"fn main() {}\n".repeat(40)),
        FxEntry::stored("src/lib/util.rs", b"pub fn util() {}\n"),
        executable("bin/run.sh", b"#!/bin/sh\necho run\n"),
        FxEntry::dir("empty/"),
    ]
}

/// The sample snapshot's bytes.
pub fn sample_bytes() -> Vec<u8> {
    Fx::of(sample_entries()).build().bytes
}

/// Validate, plan and extract `bytes` to `output`.
pub fn extract_to(
    bytes: &[u8],
    output: &Path,
    hook: &mut dyn ExtractHook,
) -> (Result<Extracted, LaunchError>, LedgerStats) {
    let mut stats = LedgerStats::default();
    let result = (|| {
        let target = prepare_output(output)?;
        let archive = read_archive(
            bytes,
            Profile::Snapshot,
            &ArchiveLimits::PRODUCTION,
            &mut ReadStats::default(),
        )?;
        let plan = plan(&archive, MAX_SNAPSHOT_NODES, &mut PlanStats::default())?;
        extract(
            &target,
            &archive,
            &plan,
            &SeededRandom::new(7),
            hook,
            &mut stats,
        )
    })();
    (result, stats)
}

/// The names in `dir`, sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .expect("list")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The staging names (`.launch-snapshot-*.partial`) in `dir`.
pub fn partials(dir: &Path) -> Vec<String> {
    names(dir)
        .into_iter()
        .filter(|name| name.starts_with(".launch-snapshot-") && name.ends_with(".partial"))
        .collect()
}

/// The permission bits of `path` (not following a final symlink).
pub fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).expect("stat").permissions().mode() & 0o7777
}

/// Every path below `dir` (relative, `/`-separated, sorted), not following
/// symlinks.
pub fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("list").map(|e| e.expect("entry")) {
            let path = entry.path();
            let relative = path
                .strip_prefix(base)
                .expect("below")
                .to_string_lossy()
                .into_owned();
            let is_dir = fs::symlink_metadata(&path).expect("stat").is_dir();
            out.push(if is_dir {
                format!("{relative}/")
            } else {
                relative
            });
            if is_dir {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}
