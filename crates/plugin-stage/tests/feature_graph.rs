//! Shipped-build feature gate (#5592), offline: `cargo tree --offline
//! --locked -e no-dev` over every binary in `packaging/plugin-binaries.json`
//! must not enable `toolkit-aws/testing` (the unsigned `ServiceBridge`
//! injection constructor). No network, no build, no fixture crate: the
//! negative cases feed the real workspace graph an explicit opt-in.

mod common;

use common::repo_root;
use plugin_stage::feature_graph::{check_binary, check_declaration, check_graph};
use plugin_stage::{Declaration, DECLARATION_PATH};

fn declaration() -> Declaration {
    Declaration::load(&repo_root().join(DECLARATION_PATH)).expect("committed declaration")
}

#[test]
fn no_shipped_binary_enables_toolkit_aws_testing() {
    let report = check_declaration(&repo_root(), &declaration()).expect("offline cargo tree");
    assert!(report.violations.is_empty(), "{:#?}", report.violations);
    // Not vacuous: the shipped toolkit binary really links toolkit-aws.
    assert!(
        report.guarded_present.contains("toolkit-aws"),
        "toolkit-aws never appeared in a shipped graph"
    );
}

#[test]
fn an_explicit_opt_in_is_detected_in_the_real_normal_graph() {
    // The Issue's example: cargo accepts the feature in the normal graph.
    let report = check_graph(
        &repo_root(),
        "toolkit-aws",
        "toolkit-aws",
        &["sdk".to_string(), "testing".to_string()],
        false,
    )
    .expect("offline cargo tree");
    assert_eq!(report.violations.len(), 1, "{:#?}", report.violations);
    assert!(report.violations[0].contains("toolkit-aws/testing"));
}

#[test]
fn a_declaration_that_adds_the_feature_through_a_shipped_package_fails() {
    // `plugin-binaries.json` could carry `toolkit-aws/testing` in `features`;
    // `plugin-stage` would pass it to `cargo build --features`.
    let mut declaration = declaration();
    let binary = declaration
        .binaries
        .iter_mut()
        .find(|binary| binary.package == "toolkit-cli")
        .expect("toolkit-cli is shipped");
    binary.features.push("toolkit-aws/testing".to_string());
    let report = check_binary(&repo_root(), binary).expect("offline cargo tree");
    assert_eq!(report.violations.len(), 1, "{:#?}", report.violations);
    assert!(report.violations[0].contains("toolkit-aws/testing"));
}

#[test]
fn the_dev_dependency_is_outside_the_normal_graph() {
    // toolkit-cli's dev-dependency enables `testing` for its own tests; the
    // normal graph must not see it, or the guard above would always fail.
    let report = check_graph(&repo_root(), "toolkit-cli", "toolkit-cli", &[], false)
        .expect("offline cargo tree");
    assert!(report.violations.is_empty(), "{:#?}", report.violations);
    assert!(report.guarded_present.contains("toolkit-aws"));
}

#[test]
fn staging_refuses_to_compile_a_forbidden_feature_graph() {
    use plugin_stage::stage::{build, BuildStep, Builder};
    let declaration = declaration();
    let mut binary = declaration
        .binaries
        .iter()
        .find(|binary| binary.package == "toolkit-cli")
        .expect("toolkit-cli is shipped")
        .clone();
    binary.features.push("toolkit-aws/testing".to_string());
    let step = BuildStep {
        binary,
        platform: declaration.platforms[0].clone(),
        builder: Builder::Cargo,
    };
    let target = common::temp_dir("feature-graph-target");
    // The refusal precedes any compilation: the empty target dir stays empty.
    let error = build(&repo_root(), &step, &target).unwrap_err();
    assert!(error.contains("toolkit-aws/testing"), "{error}");
    assert_eq!(std::fs::read_dir(&*target).unwrap().count(), 0);
}
