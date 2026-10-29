//! T-PLAN-1..5: the planner over byte-built archives; a pure function, no
//! file-system access.

use super::*;
use crate::launch::archive::tests_zipfx::{Fx, FxEntry};
use crate::launch::archive::zip_read::{read_archive, Profile, ReadStats};
use crate::launch::archive::ArchiveLimits;

fn archive_bytes(entries: Vec<FxEntry>) -> Vec<u8> {
    Fx::of(entries).build().bytes
}

fn run(bytes: &[u8], budget: usize) -> (Result<Plan, LaunchError>, PlanStats) {
    let archive = read_archive(
        bytes,
        Profile::Snapshot,
        &ArchiveLimits::PRODUCTION,
        &mut ReadStats::default(),
    )
    .expect("valid snapshot");
    let mut stats = PlanStats::default();
    (plan(&archive, budget, &mut stats), stats)
}

fn chain(top: &str, depth: usize, leaf: &str) -> String {
    let mut name = top.to_string();
    for _ in 0..depth {
        name.push_str("/a");
    }
    name.push('/');
    name.push_str(leaf);
    name
}

#[track_caller]
fn assert_bounded(stats: &PlanStats, budget: usize) {
    assert!(stats.index_requested <= budget, "{stats:?}");
    assert_eq!(
        stats.index_capacity_reserved, stats.index_capacity_final,
        "index never grew"
    );
    assert!(stats.nodes_capacity_reserved <= budget, "{stats:?}");
    assert_eq!(
        stats.nodes_capacity_reserved, stats.nodes_capacity_final,
        "nodes never grew"
    );
}

#[test]
fn t_plan_1_deep_disjoint_chains_are_refused_at_the_injected_budget() {
    let entries = (0..3)
        .map(|index| FxEntry::stored(&chain(&format!("t{index}"), 39, "f"), b"x"))
        .collect();
    let bytes = archive_bytes(entries);
    let (result, stats) = run(&bytes, 100);
    assert_eq!(
        result.map(|_| ()),
        Err(LaunchError::Archive(budget_message(100)))
    );
    assert_eq!(
        budget_message(100),
        "The snapshot would create more than 100 files and directories and was not extracted."
    );
    assert!(stats.peak_nodes <= 101, "{stats:?}");
    assert_eq!(stats.peak_nodes, 101);
    assert_bounded(&stats, 100);
}

#[test]
fn t_plan_2_production_budget_against_the_5066_shape() {
    assert_eq!(MAX_SNAPSHOT_NODES, 200_000);
    let entries: Vec<FxEntry> = (0..101)
        .map(|index| FxEntry::stored(&chain(&format!("t{index:03}"), 1990, "f"), b"d"))
        .collect();
    assert!(entries.iter().all(|entry| entry.name.len() < 4096));
    let bytes = archive_bytes(entries);
    assert!(bytes.len() < 1024 * 1024, "archive {} bytes", bytes.len());
    let (result, stats) = run(&bytes, MAX_SNAPSHOT_NODES);
    assert_eq!(
        result.map(|_| ()),
        Err(LaunchError::Archive(
            "The snapshot would create more than 200000 files and directories and was not \
             extracted."
                .to_string()
        ))
    );
    assert_eq!(stats.peak_nodes, 200_001);
    assert_bounded(&stats, MAX_SNAPSHOT_NODES);
}

#[test]
fn t_plan_3_exactly_the_budget_passes_one_more_is_refused() {
    let bytes = archive_bytes(vec![
        FxEntry::stored("a/b/c", b"x"),
        FxEntry::stored("a/d", b"y"),
    ]);
    let (result, stats) = run(&bytes, 4);
    let planned = result.expect("exactly the budget");
    assert_eq!(planned.nodes().len(), 4);
    assert_bounded(&stats, 4);
    let (result, stats) = run(&bytes, 3);
    assert_eq!(
        result.map(|_| ()),
        Err(LaunchError::Archive(budget_message(3)))
    );
    assert_eq!(stats.peak_nodes, 4);
    assert_bounded(&stats, 3);
}

#[test]
fn t_plan_4_shared_prefixes_are_counted_once() {
    let entries = (0..1000)
        .map(|index| FxEntry::stored(&format!("src/a/b/{index}.txt"), b"x"))
        .collect();
    let bytes = archive_bytes(entries);
    let (result, stats) = run(&bytes, MAX_SNAPSHOT_NODES);
    let planned = result.expect("plans");
    assert_eq!(planned.nodes().len(), 1003);
    assert_eq!((planned.directories(), planned.files()), (3, 1000));
    assert_bounded(&stats, MAX_SNAPSHOT_NODES);
    assert!(
        stats.index_requested <= 1003 * 4,
        "reserved from the segments, not the budget"
    );
    // Parents come first and nodes reference the archive's own bytes.
    let nodes = planned.nodes();
    for (id, node) in nodes.iter().enumerate() {
        if node.parent != ROOT {
            assert!((node.parent as usize) < id);
            assert_eq!(nodes[node.parent as usize].kind, NodeKind::Directory);
        }
    }
    let first = nodes[0];
    let start = first.name_offset as usize;
    assert_eq!(&bytes[start..start + usize::from(first.name_len)], b"src");
    assert_eq!(planned.entry_nodes().len(), 1000);
}

#[test]
fn t_plan_5_explicit_and_implicit_directories_merge_in_either_order() {
    for entries in [
        vec![
            FxEntry::dir("a/"),
            FxEntry::dir("a/b/"),
            FxEntry::stored("a/b/c", b"x"),
        ],
        vec![
            FxEntry::stored("a/b/c", b"x"),
            FxEntry::dir("a/b/"),
            FxEntry::dir("a/"),
        ],
        vec![FxEntry::dir("a/b/"), FxEntry::stored("a/b/c", b"x")],
    ] {
        let bytes = archive_bytes(entries);
        let (result, _) = run(&bytes, MAX_SNAPSHOT_NODES);
        let planned = result.expect("plans");
        assert_eq!(planned.nodes().len(), 3);
        assert_eq!((planned.directories(), planned.files()), (2, 1));
    }
}
