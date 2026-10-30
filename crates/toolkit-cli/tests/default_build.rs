//! #5586: release packaging must not enable SDKs in the ordinary CLI build.

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::path::Path;
use std::process::Command;

#[test]
fn default_cli_features_and_dependency_graph_stay_sdk_free() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    for manifest in [
        "crates/toolkit-cli/Cargo.toml",
        "crates/toolkit-aws/Cargo.toml",
    ] {
        let text = std::fs::read_to_string(root.join(manifest)).unwrap();
        let defaults = text.lines().find(|line| line.starts_with("default ="));
        assert_eq!(defaults, Some("default = []"), "{manifest}");
    }
    // Resolve a fresh default graph even when this test harness was compiled
    // with a live feature. All targets make the snapshot host-independent.
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "tree",
            "-p",
            "toolkit-cli",
            "--target",
            "all",
            "--edges",
            "normal,build,features",
            "--prefix",
            "depth",
            "--format",
            "{p}",
            "--offline",
            "--locked",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let graph = String::from_utf8(output.stdout)
        .unwrap()
        .replace(root.to_str().unwrap(), "<workspace>");
    assert!(
        !graph.contains("aws-sdk-")
            && !graph.contains("aws-smithy-")
            && !graph.contains("aws-config "),
        "SDK entered the default CLI graph"
    );
    assert_eq!(
        graph,
        include_str!("fixtures/default-cli-tree.txt"),
        "default CLI build graph changed; review the default-build invariant"
    );
}
