//! Slice S5 (#5296): AP-INV-*, the `python-inventory.json` rows of the `ai-payloads`
//! slice (Issue #4386) carry their design section 3 dispositions.
//!
//! The tests read `docs/migration/python-inventory.json` as data. No Python is run
//! and no template is executed.
//!
//! Requirement map (design section 5, R16, section 3 and OD-7):
//! - AP-INV-01: every row with `issue == 4386` carries the section 3 disposition
//!   and none is `converted`:
//!   `ap_inv_01_every_ai_payloads_row_carries_the_retained_disposition`,
//!   `ap_inv_01_language_contract_follows_the_section_3_role`,
//!   `ap_inv_01_no_row_is_marked_converted_or_removed`,
//!   `ap_inv_01_rows_pair_canonical_and_mirror_and_files_exist`
//! - AP-INV-02: T08 and T09 are reclassified `external-service-payload` (OD-7):
//!   `ap_inv_02_reward_templates_t08_and_t09_are_external_service_payload`
//! - AP-INV-03: totals are 0 removed and 56 retained (46 TPL, 8 LAMBDA, 2 REF), the
//!   AC3 statement is recorded and other slices are untouched:
//!   `ap_inv_03_totals_are_zero_removed_and_fifty_six_retained`,
//!   `ap_inv_03_ac3_statement_is_recorded_in_the_inventory`,
//!   `ap_inv_03_other_slices_are_untouched`
//! - Status lines: the design document and the migration README record S1 to S5 as
//!   done: `ap_inv_docs_record_s1_to_s5_done`
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const SLICE_ISSUE: u64 = 4386;
const CANON: &str = "skills/core-skills/aws-ai-ml/references/";
const MIRROR: &str = "plugins/aws-core/skills/aws-ai-ml/references/";
const DISPOSITION: &str = "retained-customer-language";

/// T06 to T09: AWS Lambda reward functions (C9, role LAMBDA).
const LAMBDA: [&str; 4] = [
    "finetuning/templates/rlvr_reward_function_source_template.py",
    "finetuning/templates/nova_rlvr_reward_function_source_template.py",
    "model-evaluation/scripts/reward_function_source_template.py",
    "model-evaluation/scripts/nova_reward_function_source_template.py",
];
/// T10: the `MLflow` reference pattern (C13, role REF).
const REF: [&str; 1] = ["finetuning/scripts/mlflow_reference.py"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn inventory() -> Value {
    let raw = fs::read_to_string(root().join("docs/migration/python-inventory.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn files(inv: &Value) -> &Vec<Value> {
    inv["files"].as_array().expect("files array")
}

fn slice_rows(inv: &Value) -> Vec<&Value> {
    files(inv)
        .iter()
        .filter(|r| r["issue"].as_u64() == Some(SLICE_ISSUE))
        .collect()
}

fn str_of<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or("")
}

/// The path relative to the references root, for canonical and mirror rows alike.
fn relative(row: &Value) -> String {
    let path = str_of(row, "path");
    path.strip_prefix(CANON)
        .or_else(|| path.strip_prefix(MIRROR))
        .unwrap_or_else(|| panic!("row outside the references roots: {path}"))
        .to_string()
}

fn expected_contract(rel: &str) -> &'static str {
    if LAMBDA.contains(&rel) {
        "C9"
    } else if REF.contains(&rel) {
        "C13"
    } else {
        "C1"
    }
}

// ---- AP-INV-01 ------------------------------------------------------------

#[test]
fn ap_inv_01_every_ai_payloads_row_carries_the_retained_disposition() {
    let inv = inventory();
    let rows = slice_rows(&inv);
    assert_eq!(rows.len(), 56, "28 canonical files and 28 mirrors");
    for row in rows {
        let path = str_of(row, "path");
        assert_eq!(str_of(row, "disposition"), DISPOSITION, "{path}");
        assert_eq!(str_of(row, "status"), "retained", "{path}");
        let note = str_of(row, "disposition_note");
        assert!(note.len() >= 40, "{path}: disposition_note is missing");
        assert!(
            note.contains("not counted as removed") || note.contains("Not counted as removed"),
            "{path}: the note must say the row is not counted as removed: {note}"
        );
    }
}

#[test]
fn ap_inv_01_language_contract_follows_the_section_3_role() {
    let inv = inventory();
    let mut by_contract: BTreeMap<String, usize> = BTreeMap::new();
    for row in slice_rows(&inv) {
        let rel = relative(row);
        let contract = str_of(row, "language_contract");
        assert_eq!(contract, expected_contract(&rel), "{rel}");
        *by_contract.entry(contract.to_string()).or_default() += 1;
    }
    // 23 TPL, 4 LAMBDA and 1 REF canonical files, each with a mirror.
    assert_eq!(by_contract["C1"], 46);
    assert_eq!(by_contract["C9"], 8);
    assert_eq!(by_contract["C13"], 2);
    assert_eq!(by_contract.len(), 3);
}

#[test]
fn ap_inv_01_no_row_is_marked_converted_or_removed() {
    let inv = inventory();
    for row in slice_rows(&inv) {
        let path = str_of(row, "path");
        for key in ["status", "disposition", "classification"] {
            let value = str_of(row, key).to_ascii_lowercase();
            for banned in ["converted", "removed", "deleted", "rust"] {
                assert!(
                    !value.contains(banned),
                    "{path}: {key} = {value:?} must not claim Python removal"
                );
            }
        }
    }
    let statuses: BTreeSet<&str> = files(&inv).iter().map(|r| str_of(r, "status")).collect();
    assert!(
        !statuses.contains("converted"),
        "no inventory row may be converted by this slice: {statuses:?}"
    );
}

#[test]
fn ap_inv_01_rows_pair_canonical_and_mirror_and_files_exist() {
    let inv = inventory();
    let rows = slice_rows(&inv);
    let mut canonical = BTreeSet::new();
    let mut mirrors = BTreeSet::new();
    for row in &rows {
        let path = str_of(row, "path");
        assert!(root().join(path).is_file(), "{path} must stay in place");
        if path.starts_with(CANON) {
            assert_eq!(str_of(row, "canonical_identical_path"), path);
            canonical.insert(relative(row));
        } else {
            assert_eq!(
                str_of(row, "canonical_identical_path"),
                format!("{CANON}{}", relative(row)),
                "{path}"
            );
            mirrors.insert(relative(row));
        }
    }
    assert_eq!(canonical.len(), 28);
    assert_eq!(canonical, mirrors, "each canonical file has one mirror row");
    for rel in LAMBDA.iter().chain(REF.iter()) {
        assert!(canonical.contains(*rel), "{rel}");
    }
}

// ---- AP-INV-02 ------------------------------------------------------------

#[test]
fn ap_inv_02_reward_templates_t08_and_t09_are_external_service_payload() {
    let inv = inventory();
    let wanted = [
        "model-evaluation/scripts/reward_function_source_template.py",
        "model-evaluation/scripts/nova_reward_function_source_template.py",
    ];
    for rel in wanted {
        for prefix in [CANON, MIRROR] {
            let path = format!("{prefix}{rel}");
            let row = files(&inv)
                .iter()
                .find(|r| str_of(r, "path") == path)
                .unwrap_or_else(|| panic!("{path} is missing from the inventory"));
            assert_eq!(
                str_of(row, "classification"),
                "external-service-payload",
                "{path} is a Lambda reward function (OD-7), not a toolkit helper"
            );
            assert_eq!(str_of(row, "language_contract"), "C9", "{path}");
        }
    }
    // T10 stays a helper-class reference pattern (K-PY with no Rust replacement).
    for prefix in [CANON, MIRROR] {
        let path = format!("{prefix}{}", REF[0]);
        let row = files(&inv)
            .iter()
            .find(|r| str_of(r, "path") == path)
            .unwrap();
        assert_eq!(str_of(row, "classification"), "toolkit-helper", "{path}");
        assert_eq!(str_of(row, "language_contract"), "C13", "{path}");
    }
}

// ---- AP-INV-03 ------------------------------------------------------------

#[test]
fn ap_inv_03_totals_are_zero_removed_and_fifty_six_retained() {
    let inv = inventory();
    let rows = slice_rows(&inv);
    let retained = rows
        .iter()
        .filter(|r| str_of(r, "status") == "retained")
        .count();
    let removed = rows
        .iter()
        .filter(|r| matches!(str_of(r, "status"), "removed" | "converted" | "deleted"))
        .count();
    assert_eq!((removed, retained), (0, 56));

    let external = rows
        .iter()
        .filter(|r| str_of(r, "classification") == "external-service-payload")
        .count();
    let helper = rows
        .iter()
        .filter(|r| str_of(r, "classification") == "toolkit-helper")
        .count();
    assert_eq!((external, helper), (54, 2), "46 TPL + 8 LAMBDA, 2 REF");

    let summary = &inv["slice_dispositions"]["ai-payloads"];
    assert_eq!(summary["issue"].as_u64(), Some(SLICE_ISSUE));
    assert_eq!(summary["rows"].as_u64(), Some(56));
    assert_eq!(summary["removed"].as_u64(), Some(0));
    assert_eq!(summary["retained"].as_u64(), Some(56));
    assert_eq!(summary["template_rows"].as_u64(), Some(46));
    assert_eq!(summary["lambda_rows"].as_u64(), Some(8));
    assert_eq!(summary["reference_rows"].as_u64(), Some(2));
}

#[test]
fn ap_inv_03_ac3_statement_is_recorded_in_the_inventory() {
    let inv = inventory();
    let statement = inv["slice_dispositions"]["ai-payloads"]["ac3_statement"]
        .as_str()
        .unwrap_or("");
    for needle in [
        "56 rows: 0 removed and 56 retained with documented disposition",
        "(46 TPL, 8 LAMBDA, 2 REF, mirrors included)",
        "No row may be marked as converted to Rust.",
        "The R-CTRL commands replace agent-side manual steps, not the Python text.",
    ] {
        assert!(
            statement.contains(needle),
            "missing {needle:?} in {statement:?}"
        );
    }
}

#[test]
fn ap_inv_03_other_slices_are_untouched() {
    let inv = inventory();
    let others: Vec<&Value> = files(&inv)
        .iter()
        .filter(|r| r["issue"].as_u64() != Some(SLICE_ISSUE))
        .collect();
    assert_eq!(
        others.len(),
        391 - 56,
        "the inventory baseline has 391 rows"
    );
    for row in others {
        let path = str_of(row, "path");
        // Payment policy S7 (#4410) and OpenClaw B5 (#4411) are ported;
        // every other slice stays pending.
        let ported = matches!(row["issue"].as_u64(), Some(4410 | 4411));
        let expected = if ported {
            if root().join(path).exists() {
                "ported"
            } else {
                "retired"
            }
        } else {
            "pending"
        };
        assert_eq!(str_of(row, "status"), expected, "{path}");
        for key in ["disposition", "disposition_note", "language_contract"] {
            if ported && key == "disposition_note" {
                continue;
            }
            assert!(
                row.get(key).is_none(),
                "{path}: {key} belongs to ai-payloads"
            );
        }
    }
}

// ---- status lines ---------------------------------------------------------

#[test]
fn ap_inv_docs_record_s1_to_s5_done() {
    let design = fs::read_to_string(root().join("docs/migration/ai-payloads-design.md")).unwrap();
    let status = design
        .lines()
        .find(|l| l.starts_with("Status:"))
        .expect("design status line");
    let status_block: String = design
        .lines()
        .skip_while(|l| !l.starts_with("Status:"))
        .take_while(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        status_block.contains("S1") && status_block.contains("S5"),
        "status line must record S1 to S5: {status_block}"
    );
    assert!(
        status_block.contains("implemented") && status_block.contains("0 of 56"),
        "status line must say the slices are implemented with 0 of 56 rows removed: {status}"
    );

    let readme = fs::read_to_string(root().join("docs/migration/README.md")).unwrap();
    let note = readme
        .lines()
        .find(|l| l.contains("#4386") && l.contains("S1-S5"))
        .expect("README note for #4386 recording S1-S5");
    assert!(
        note.contains("0 of 56") || note.contains("0 removed"),
        "README note must record that no row was removed: {note}"
    );

    let parity = fs::read_to_string(root().join("docs/migration/ai-payloads.md")).unwrap();
    assert!(
        parity.contains("S5 `ai-payloads-inventory`"),
        "parity document must list slice S5"
    );
}

/// #5316: the status summaries distinguish implemented from landed, carry no
/// stale "pending review" text, and cite a landing commit for every slice.
#[test]
fn ap_inv_status_lines_distinguish_implemented_from_landed_with_evidence() {
    let parity = fs::read_to_string(root().join("docs/migration/ai-payloads.md")).unwrap();
    assert!(
        !parity.to_ascii_lowercase().contains("pending review"),
        "the parity document keeps stale pending-review text"
    );
    let rows: Vec<&str> = parity
        .lines()
        .filter(|l| l.starts_with("| S") && l.contains(" `ai-payloads-"))
        .collect();
    assert_eq!(rows.len(), 7, "S1, S2a, S2b, S3, S3r, S4 and S5: {rows:?}");
    for row in rows {
        let status = row.trim_end_matches('|').rsplit('|').next().unwrap().trim();
        let landed = status
            .strip_prefix("implemented, landed in `")
            .and_then(|rest| rest.strip_suffix('`'));
        assert!(
            landed.is_some_and(|sha| sha.len() >= 8
                && sha.len() <= 40
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
            "slice row must say `implemented, landed in `<commit>``: {row}"
        );
    }

    let design = fs::read_to_string(root().join("docs/migration/ai-payloads-design.md")).unwrap();
    let status_block: String = design
        .lines()
        .skip_while(|l| !l.starts_with("Status:"))
        .take_while(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        status_block.contains("landed on `origin/rolling`")
            && status_block.contains("ai-payloads.md"),
        "design status must point at the landing evidence table: {status_block}"
    );
}
