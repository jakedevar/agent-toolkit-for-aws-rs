//! The one schema-driven case of `tests/test_azure_rdfa_discover_contracts.py`
//! (#4806): the committed after-discover `RDfA` run directory passes the real
//! artifact validator. It needs the crate-private CLI entry point, so it lives
//! here; the prose and simulation cases of that file are in
//! `tests/startup_contracts/rdfa.rs`.

use std::fs;

use super::documents::put;
use super::{artifacts, copy_dir, error_pairs, json, plugin_arg, real_plugin, text, TempDir};
use crate::startup::artifacts::pyjson::dumps;
use toolkit_core::Json;

const RUN: &str = "fixtures/azure-rdfa-final/after-discover";
const ARTIFACT: &str = "azure-resource-inventory.json";

/// The CLI over `run` as `azure-to-aws` without the baseline: status and the
/// `(code, path)` pairs of the JSON report.
fn validate(run: &std::path::Path) -> (u8, Vec<(String, String)>) {
    let plugin = plugin_arg();
    let (status, stdout, stderr) = artifacts(&[
        "--run-dir",
        text(run),
        "--skill",
        "azure-to-aws",
        "--no-baseline",
        "--json",
        "--plugin-root",
        plugin.as_str(),
    ]);
    assert!(!stdout.is_empty(), "{stderr}");
    (status, error_pairs(&stdout))
}

#[test]
fn test_final_rdfa_inventory_passes_real_artifact_validator() {
    let committed = real_plugin().join(RUN);
    assert!(committed.join(ARTIFACT).is_file(), "{RUN}/{ARTIFACT}");
    let (status, errors) = validate(&committed);
    assert_eq!(status, 0, "{errors:?}");
    assert!(errors.is_empty(), "{errors:?}");

    // Non-vacuous: the same directory with `resources` replaced by a string is rejected.
    let broken = TempDir::new("rdfa-final");
    copy_dir(&committed, &broken);
    let path = broken.join(ARTIFACT);
    let mut inventory = json(&fs::read_to_string(&path).expect("read the copied inventory"));
    put(
        &mut inventory,
        "",
        "resources",
        Json::string("not an array"),
    );
    fs::write(&path, dumps(&inventory, None, false)).expect("write the broken inventory");
    let (status, errors) = validate(&broken);
    assert_eq!(status, 1, "{errors:?}");
    assert!(!errors.is_empty());
}
