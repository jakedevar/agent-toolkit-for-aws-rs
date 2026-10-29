//! #5135: the skipped-secret ledger is bounded by count, bytes and
//! allocation, and reports what it did not keep.

use super::*;

fn ledger(max_paths: usize, max_bytes: usize) -> SkippedSecrets {
    SkippedSecrets::new(SkippedBudget {
        max_paths,
        max_bytes,
    })
}

fn paths(ledger: &SkippedSecrets) -> Vec<&str> {
    ledger.paths().collect()
}

#[test]
fn production_budget_is_pinned() {
    assert_eq!(
        SkippedBudget::PRODUCTION,
        SkippedBudget {
            max_paths: 1000,
            max_bytes: 65_536
        }
    );
}

#[test]
fn an_empty_ledger_allocates_nothing_and_reports_nothing() {
    let empty = ledger(10, 1024);
    assert_eq!(empty.total(), 0);
    assert_eq!(empty.retained(), 0);
    assert_eq!(empty.omitted(), 0);
    assert_eq!(empty.capacity(), 0);
    assert!(paths(&empty).is_empty());
    assert_eq!(empty.summary(), None);
}

#[test]
fn every_path_within_budget_is_kept_and_listed() {
    let mut kept = ledger(10, 1024);
    kept.record(".env");
    kept.record("a/server.pem");
    assert_eq!(paths(&kept), vec![".env", "a/server.pem"]);
    assert_eq!((kept.total(), kept.retained(), kept.omitted()), (2, 2, 0));
    assert_eq!(
        kept.summary().as_deref(),
        Some("Excluded 2 file(s) matching secret patterns: .env, a/server.pem")
    );
}

#[test]
fn the_count_cap_refuses_retention_and_counts_the_rest() {
    let mut small = ledger(2, 1024);
    for index in 0..7 {
        small.record(&format!("d{index}/.env.x"));
    }
    assert_eq!(paths(&small), vec!["d0/.env.x", "d1/.env.x"]);
    assert_eq!(
        (small.total(), small.retained(), small.omitted()),
        (7, 2, 5)
    );
    assert_eq!(
        small.summary().as_deref(),
        Some(
            "Excluded 7 file(s) matching secret patterns: d0/.env.x, d1/.env.x, \
             and 5 more not listed"
        )
    );
}

#[test]
fn the_byte_cap_closes_the_ledger_at_the_first_path_that_does_not_fit() {
    // "abc" + separator = 4 bytes; "defgh" + separator would reach 10 > 8.
    let mut small = ledger(100, 8);
    small.record("abc");
    small.record("defgh");
    // Short enough to fit the remaining 4 bytes, but the ledger is closed:
    // the retained list stays a prefix of the walk order.
    small.record("i");
    assert_eq!(paths(&small), vec!["abc"]);
    assert_eq!(
        (small.total(), small.retained(), small.omitted()),
        (3, 1, 2)
    );
    assert!(small.capacity() <= 8, "{}", small.capacity());
    assert_eq!(
        small.summary().as_deref(),
        Some("Excluded 3 file(s) matching secret patterns: abc, and 2 more not listed")
    );
}

#[test]
fn a_zero_budget_keeps_nothing_and_allocates_nothing() {
    for (max_paths, max_bytes) in [(0, 1024), (10, 0), (10, 3)] {
        let mut none = ledger(max_paths, max_bytes);
        none.record("abc");
        none.record("d");
        assert_eq!(none.retained(), 0, "{max_paths} {max_bytes}");
        assert_eq!(none.omitted(), 2);
        assert_eq!(none.capacity(), 0);
        assert_eq!(
            none.summary().as_deref(),
            Some("Excluded 2 file(s) matching secret patterns: 2 not listed")
        );
    }
}

#[test]
fn allocation_stays_within_the_byte_cap_for_any_number_of_paths() {
    let budget = SkippedBudget {
        max_paths: 1_000_000,
        max_bytes: 5_000,
    };
    let mut ledger = SkippedSecrets::new(budget);
    for index in 0..20_000 {
        ledger.record(&format!("dir{index}/.env.local.{index}"));
        assert!(
            ledger.capacity() <= budget.max_bytes,
            "{}",
            ledger.capacity()
        );
    }
    assert_eq!(ledger.total(), 20_000);
    assert!(ledger.retained() > 0);
    assert_eq!(ledger.omitted(), 20_000 - ledger.retained() as u64);
    let text: usize = ledger.paths().map(|path| path.len() + 1).sum();
    assert!(text <= budget.max_bytes, "{text}");
    assert!(ledger.peak() < 2 * budget.max_bytes, "{}", ledger.peak());
    // The summary is bounded by the retained text, not by the walk.
    let summary = ledger.summary().expect("summary");
    assert!(summary.len() < 2 * budget.max_bytes, "{}", summary.len());
    assert!(summary.ends_with(&format!(", and {} more not listed", ledger.omitted())));
}

#[test]
fn shown_paths_are_escaped() {
    let mut odd = ledger(10, 1024);
    odd.record("a\nInjected line/.env");
    let summary = odd.summary().expect("summary");
    assert!(!summary.contains('\n'), "{summary:?}");
    assert!(summary.contains("a\\nInjected line/.env"), "{summary:?}");
}
