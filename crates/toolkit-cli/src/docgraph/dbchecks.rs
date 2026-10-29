//! Database-level checks from the pre-collected analysis
//! (`run_db_checks` of `wa_review.py`): same ids, thresholds, texts and order.

use toolkit_core::Json;

use super::checks::{Findings, Status};
use super::snapshot::Collection;
use crate::safe_text::safe_path;

const PERF: &str = "Performance Efficiency";
const COST: &str = "Cost Optimization";

/// `", ".join(items[:5])`, each item cleaned.
fn first_five(items: &[String]) -> String {
    items
        .iter()
        .take(5)
        .map(|item| safe_path(item))
        .collect::<Vec<_>>()
        .join(", ")
}

/// How many indexes have another index whose fields start with theirs and are
/// longer (a prefix subset).
fn redundant_indexes(collection: &Collection) -> usize {
    let ordered: Vec<&[Json]> = collection
        .indexes
        .iter()
        .map(|index| index.ordered_fields.as_slice())
        .collect();
    let mut redundant = 0;
    for (i, a) in ordered.iter().enumerate() {
        for (j, b) in ordered.iter().enumerate() {
            if i != j && !a.is_empty() && a.len() < b.len() && b[..a.len()] == **a {
                redundant += 1;
                break;
            }
        }
    }
    redundant
}

/// Accumulators of the first pass over the collections.
#[derive(Default)]
struct Totals {
    indexes: usize,
    unused_indexes: usize,
    redundant: usize,
    low_cardinality: usize,
    low_card_names: Vec<String>,
    large_docs: Vec<String>,
    ttl_collections: Vec<String>,
    data: f64,
    index: f64,
    bloated: Vec<String>,
    over_indexed: Vec<String>,
    compression_disabled: Vec<String>,
    collscan: Vec<String>,
    write_amplification: Vec<String>,
}

fn totals(collections: &[Collection]) -> Totals {
    let mut t = Totals::default();
    for collection in collections {
        let qualified = format!("{}.{}", collection.database, collection.name);
        t.indexes += collection.indexes.len();
        for index in &collection.indexes {
            if index.potential_unused {
                t.unused_indexes += 1;
            }
            if index.low_cardinality {
                t.low_cardinality += 1;
                t.low_card_names.push(format!("{qualified}.{}", index.name));
            }
            if index.has_ttl && !t.ttl_collections.contains(&qualified) {
                t.ttl_collections.push(qualified.clone());
            }
        }
        if collection.avg_obj_size.value() > 8192.0 {
            t.large_docs.push(format!(
                "{qualified} ({} bytes)",
                collection.avg_obj_size.commas()
            ));
        }
        t.redundant += redundant_indexes(collection);

        t.data += collection.size;
        let index_size: f64 = collection.indexes.iter().map(|index| index.size).sum();
        t.index += index_size;

        if collection.unused_percent > 30.0 {
            t.bloated
                .push(format!("{qualified} ({:.0}%)", collection.unused_percent));
        }
        if collection.indexes.len() > 10 {
            t.over_indexed.push(format!(
                "{qualified} ({} indexes)",
                collection.indexes.len()
            ));
        }
        if !collection.compression_enabled {
            t.compression_disabled.push(qualified.clone());
        }
        let secondary = collection
            .indexes
            .iter()
            .filter(|index| index.name != "_id_")
            .count();
        if collection.count.value() > 100_000.0 && secondary == 0 {
            t.collscan
                .push(format!("{qualified} ({} docs)", collection.count.commas()));
        }
        if collection.size > 0.0 && index_size > 2.0 * collection.size {
            t.write_amplification.push(format!(
                "{qualified} (index {:.1}x data)",
                index_size / collection.size
            ));
        }
    }
    t
}

/// A check that warns with the first five `items` when there are any and
/// passes with `pass_label` otherwise.
fn list_check(
    findings: &mut Findings,
    (pillar, id): (&'static str, &'static str),
    items: &[String],
    pass_label: &str,
    warn_label: impl FnOnce(usize) -> String,
) {
    if items.is_empty() {
        findings.add(pillar, id, pass_label, Status::Pass, "");
    } else {
        findings.add(
            pillar,
            id,
            warn_label(items.len()),
            Status::Warn,
            first_five(items),
        );
    }
}

/// The checks over the collections of an analysis, in the order of the script.
pub(super) fn run_db_checks(collections: &[Collection]) -> Findings {
    let mut findings = Findings::default();
    let t = totals(collections);

    list_check(
        &mut findings,
        (PERF, "PERF1"),
        &t.large_docs,
        "All collections have avg doc size < 8 KB",
        |n| format!("{n} collection(s) with avg doc size > 8 KB"),
    );

    if t.redundant > 0 {
        findings.add(
            PERF,
            "PERF1b",
            format!("{} redundant index(es) (prefix subsets)", t.redundant),
            Status::Warn,
            "",
        );
    } else {
        findings.add(
            PERF,
            "PERF1b",
            "No redundant indexes detected",
            Status::Pass,
            "",
        );
    }

    if t.low_cardinality > 0 {
        findings.add(
            PERF,
            "PERF1c",
            format!("{} low cardinality index(es)", t.low_cardinality),
            Status::Warn,
            first_five(&t.low_card_names),
        );
    } else {
        findings.add(
            PERF,
            "PERF1c",
            "No low cardinality indexes detected",
            Status::Pass,
            "",
        );
    }

    if t.unused_indexes > 0 {
        findings.add(
            COST,
            "COST3",
            format!(
                "{} unused index(es) of {} total",
                t.unused_indexes, t.indexes
            ),
            Status::Warn,
            "Remove unused indexes to reduce write overhead and storage",
        );
    } else {
        findings.add(
            COST,
            "COST3",
            format!("No unused indexes ({} total)", t.indexes),
            Status::Pass,
            "",
        );
    }

    if t.ttl_collections.is_empty() {
        findings.add(
            COST,
            "COST4",
            "No TTL indexes found",
            Status::Warn,
            "Consider TTL indexes for automatic data expiration",
        );
    } else {
        findings.add(
            COST,
            "COST4",
            format!("TTL indexes on {} collection(s)", t.ttl_collections.len()),
            Status::Pass,
            first_five(&t.ttl_collections),
        );
    }

    if t.data > 0.0 {
        let ratio = t.index / t.data * 100.0;
        let high = ratio > 50.0;
        findings.add(
            PERF,
            "PERF8",
            format!("Index-to-data ratio: {ratio:.0}%"),
            if high { Status::Warn } else { Status::Pass },
            if high {
                "Indexes exceed 50% of data size"
            } else {
                ""
            },
        );
    }

    if t.bloated.is_empty() {
        findings.add(
            PERF,
            "PERF9",
            "No significant storage bloat",
            Status::Pass,
            "",
        );
    } else {
        findings.add(
            PERF,
            "PERF9",
            format!("{} collection(s) with >30% storage bloat", t.bloated.len()),
            Status::Warn,
            format!("Run compact command. {}", first_five(&t.bloated)),
        );
    }

    list_check(
        &mut findings,
        (PERF, "PERF10"),
        &t.over_indexed,
        "No over-indexed collections",
        |n| format!("{n} collection(s) with >10 indexes"),
    );
    list_check(
        &mut findings,
        ("Sustainability", "SUST2"),
        &t.compression_disabled,
        "Compression enabled on all collections",
        |n| format!("Compression disabled on {n} collection(s)"),
    );
    list_check(
        &mut findings,
        (PERF, "PERF15"),
        &t.collscan,
        "All large collections have secondary indexes",
        |n| format!("{n} large collection(s) with no secondary indexes"),
    );
    list_check(
        &mut findings,
        (PERF, "PERF16"),
        &t.write_amplification,
        "No excessive index-to-data ratio",
        |n| format!("{n} collection(s) with index size > 2x data"),
    );

    findings
}
