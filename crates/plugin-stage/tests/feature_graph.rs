//! Shipped-build feature gate (#5592/#5603/#5604).
//!
//! Two layers, no network and no compile anywhere:
//!
//! * the broad drift lint runs the real `cargo tree` with an explicit
//!   `Network::Offline` over every binary in `packaging/plugin-binaries.json`
//!   and the real workspace graph (negatives feed it an explicit opt-in);
//! * the production path (`Resolver::production` semantics) is exercised with
//!   an injected fake cargo that records its arguments, so the contract is
//!   what staging passes to cargo and that any resolution failure refuses
//!   staging.

mod common;

use common::repo_root;
use plugin_stage::feature_graph::{
    check_binary, check_declaration, check_graph, Network, Query, Resolver,
};
use plugin_stage::stage::{build_with, refuse_forbidden_features, BuildStep, Builder};
use plugin_stage::{BinarySpec, Declaration, DECLARATION_PATH};

fn declaration() -> Declaration {
    Declaration::load(&repo_root().join(DECLARATION_PATH)).expect("committed declaration")
}

fn toolkit_cli(declaration: &Declaration) -> BinarySpec {
    declaration
        .binaries
        .iter()
        .find(|binary| binary.package == "toolkit-cli")
        .expect("toolkit-cli is shipped")
        .clone()
}

fn step_for(declaration: &Declaration, binary: BinarySpec) -> BuildStep {
    BuildStep {
        binary,
        platform: declaration.platforms[0].clone(),
        builder: Builder::Cargo,
    }
}

fn query(package: &str, features: &[&str]) -> Query {
    Query {
        package: package.to_string(),
        features: features.iter().map(|f| (*f).to_string()).collect(),
        all_features: false,
        target: None,
    }
}

#[test]
fn no_shipped_binary_enables_toolkit_aws_testing() {
    let report = check_declaration(&repo_root(), &Resolver::offline(), &declaration())
        .expect("offline cargo tree");
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
        &Resolver::offline(),
        "toolkit-aws",
        &query("toolkit-aws", &["sdk", "testing"]),
    )
    .expect("offline cargo tree");
    assert_eq!(report.violations.len(), 1, "{:#?}", report.violations);
    assert!(report.violations[0].contains("toolkit-aws/testing"));
}

#[test]
fn a_declaration_that_adds_the_feature_through_a_shipped_package_fails() {
    // `plugin-binaries.json` could carry `toolkit-aws/testing` in `features`;
    // `plugin-stage` would pass it to `cargo build --features`.
    let declaration = declaration();
    let mut binary = toolkit_cli(&declaration);
    binary.features.push("toolkit-aws/testing".to_string());
    let report = check_binary(&repo_root(), &Resolver::offline(), &binary).expect("cargo tree");
    assert_eq!(report.violations.len(), 1, "{:#?}", report.violations);
    assert!(report
        .violations
        .iter()
        .all(|v| v.contains("toolkit-aws/testing")));
}

#[test]
fn the_dev_dependency_is_outside_the_normal_graph() {
    // toolkit-cli's dev-dependency enables `testing` for its own tests; the
    // normal graph must not see it, or the guard above would always fail.
    let report = check_graph(
        &repo_root(),
        &Resolver::offline(),
        "toolkit-cli",
        &query("toolkit-cli", &[]),
    )
    .expect("offline cargo tree");
    assert!(report.violations.is_empty(), "{:#?}", report.violations);
    assert!(report.guarded_present.contains("toolkit-aws"));
}

#[test]
fn the_real_production_preflight_refuses_a_forbidden_graph_before_compiling() {
    // Real cargo under an explicit offline policy (deterministic here). The
    // refusal precedes any compilation: the empty target dir stays empty.
    let declaration = declaration();
    let mut binary = toolkit_cli(&declaration);
    binary.features.push("toolkit-aws/testing".to_string());
    let step = step_for(&declaration, binary);
    let target = common::temp_dir("feature-graph-target");
    let error = build_with(&repo_root(), &step, &target, &Resolver::offline()).unwrap_err();
    assert!(error.contains("toolkit-aws/testing"), "{error}");
    assert_eq!(std::fs::read_dir(&*target).unwrap().count(), 0);
}

#[test]
fn the_real_production_preflight_accepts_every_shipped_binary_for_its_own_target() {
    let declaration = declaration();
    let resolver = Resolver::offline();
    for binary in &declaration.binaries {
        for platform in &declaration.platforms {
            let step = BuildStep {
                binary: binary.clone(),
                platform: platform.clone(),
                builder: Builder::Cargo,
            };
            refuse_forbidden_features(&repo_root(), &step, &resolver)
                .unwrap_or_else(|err| panic!("{}: {err}", step.display()));
        }
    }
}

#[cfg(unix)]
mod fake_cargo {
    //! A fake `cargo` that records its arguments and prints canned output, so
    //! the production contract is tested without a registry or a compile.

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    pub struct Fake {
        pub dir: common::TempDir,
        pub program: PathBuf,
    }

    impl Fake {
        pub fn new(stdout: &str, stderr: &str, exit: i32) -> Self {
            let dir = common::temp_dir("fake-cargo");
            let out = dir.join("stdout");
            let err = dir.join("stderr");
            common::write(&out, stdout);
            common::write(&err, stderr);
            let program = dir.join("cargo");
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{args}'\ncat '{out}'\ncat '{err}' >&2\nexit {exit}\n",
                args = dir.join("args").display(),
                out = out.display(),
                err = err.display(),
            );
            common::write(&program, &script);
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir, program }
        }

        pub fn resolver(&self, network: Network) -> Resolver {
            let mut resolver = Resolver::production().with_cargo(&self.program);
            resolver.network = network;
            resolver
        }

        /// The production resolver (network policy included) with only the
        /// cargo program swapped.
        pub fn production(&self) -> Resolver {
            Resolver::production().with_cargo(&self.program)
        }

        pub fn args(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("args"))
                .expect("fake cargo was never run")
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    /// Spawning a just-written executable can hit ETXTBSY when another test
    /// forks concurrently; retry the whole call.
    pub fn retry<T>(mut run: impl FnMut() -> Result<T, String>) -> Result<T, String> {
        for _ in 0..20 {
            match run() {
                Err(err) if err.contains("Text file busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                other => return other,
            }
        }
        run()
    }
}

#[cfg(unix)]
mod production_contract {
    use super::fake_cargo::{retry, Fake};
    use super::*;

    const CLEAN: &str = "toolkit-cli v0.1.0 (/w/crates/toolkit-cli) []\ntoolkit-aws v0.1.0 (/w/crates/toolkit-aws) [default,sdk]\n";

    #[test]
    fn production_passes_the_build_graph_and_no_offline_to_cargo() {
        let declaration = declaration();
        let binary = toolkit_cli(&declaration);
        let step = step_for(&declaration, binary.clone());
        let fake = Fake::new(CLEAN, "", 0);
        retry(|| refuse_forbidden_features(&repo_root(), &step, &fake.production()))
            .expect("clean graph stages");
        let args = fake.args();
        // The network policy is the build's, not ours: no --offline flag.
        assert!(!args.contains(&"--offline".to_string()), "{args:?}");
        assert!(args.contains(&"--locked".to_string()), "{args:?}");
        // Exactly the graph this build compiles, not a wider one.
        assert!(!args.contains(&"--all-features".to_string()), "{args:?}");
        assert!(
            !args.windows(2).any(|w| w == ["--target", "all"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--target", step.platform.triple.as_str()]),
            "{args:?}"
        );
        assert!(args.windows(2).any(|w| w == ["--package", "toolkit-cli"]));
        assert!(args.windows(2).any(|w| w == ["--edges", "no-dev"]));
        if !binary.features.is_empty() {
            let joined = binary.features.join(",");
            assert!(args
                .windows(2)
                .any(|w| w == ["--features", joined.as_str()]));
        }
    }

    #[test]
    fn the_shipped_resolver_is_inherit_and_the_lint_is_explicitly_offline() {
        assert_eq!(Resolver::production().network, Network::Inherit);
        assert_eq!(Resolver::offline().network, Network::Offline);
        // The explicit offline mode still adds the flag for deterministic tests.
        let declaration = declaration();
        let step = step_for(&declaration, toolkit_cli(&declaration));
        let fake = Fake::new(CLEAN, "", 0);
        retry(|| refuse_forbidden_features(&repo_root(), &step, &fake.resolver(Network::Offline)))
            .unwrap();
        assert!(fake.args().contains(&"--offline".to_string()));
    }

    #[test]
    fn a_resolution_failure_refuses_staging_before_any_compile() {
        let declaration = declaration();
        let step = step_for(&declaration, toolkit_cli(&declaration));
        let fake = Fake::new(
            "",
            "error: no matching package named `serde` found\nlocation searched: registry\n",
            101,
        );
        let target = common::temp_dir("feature-graph-failure-target");
        let error = retry(|| {
            build_with(
                &repo_root(),
                &step,
                &target,
                &fake.resolver(Network::Inherit),
            )
            .map(|_| ())
        })
        .unwrap_err();
        assert!(
            error.contains("could not resolve the feature graph"),
            "{error}"
        );
        assert!(error.contains("no matching package"), "{error}");
        assert_eq!(std::fs::read_dir(&*target).unwrap().count(), 0);
    }

    #[test]
    fn a_missing_cargo_refuses_staging() {
        let declaration = declaration();
        let step = step_for(&declaration, toolkit_cli(&declaration));
        let resolver = Resolver::production().with_cargo("/nonexistent/cargo-for-plugin-stage");
        let error = refuse_forbidden_features(&repo_root(), &step, &resolver).unwrap_err();
        assert!(error.contains("cannot run cargo tree"), "{error}");
    }

    #[test]
    fn empty_or_rootless_output_never_passes() {
        let declaration = declaration();
        let step = step_for(&declaration, toolkit_cli(&declaration));
        for (stdout, expect) in [
            ("", "printed no packages"),
            ("\n\n", "printed no packages"),
            ("serde v1.0.0 []\n", "expected package toolkit-cli"),
        ] {
            let fake = Fake::new(stdout, "", 0);
            let error = retry(|| {
                refuse_forbidden_features(&repo_root(), &step, &fake.resolver(Network::Inherit))
            })
            .unwrap_err();
            assert!(error.contains(expect), "{stdout:?}: {error}");
        }
    }

    #[test]
    fn a_malformed_guarded_row_never_passes() {
        let declaration = declaration();
        let step = step_for(&declaration, toolkit_cli(&declaration));
        for row in [
            "toolkit-aws v0.1.0 (/w)",
            "toolkit-aws v0.1.0 (/w) [default,testing",
            "toolkit-aws",
        ] {
            let fake = Fake::new(&format!("toolkit-cli v0.1.0 (/w) []\n{row}\n"), "", 0);
            let error = retry(|| {
                refuse_forbidden_features(&repo_root(), &step, &fake.resolver(Network::Inherit))
            })
            .unwrap_err();
            assert!(error.contains("toolkit-aws"), "{row}: {error}");
        }
    }

    #[test]
    fn a_forbidden_feature_in_the_output_refuses_and_absence_passes() {
        let declaration = declaration();
        let step = step_for(&declaration, toolkit_cli(&declaration));
        let bad = Fake::new(
            "toolkit-cli v0.1.0 (/w) []\ntoolkit-aws v0.1.0 (/w) [sdk,testing]\n",
            "",
            0,
        );
        let error = retry(|| {
            refuse_forbidden_features(&repo_root(), &step, &bad.resolver(Network::Inherit))
        })
        .unwrap_err();
        assert!(error.contains("toolkit-aws/testing"), "{error}");
        // A binary that legitimately lacks toolkit-aws still passes.
        let absent = Fake::new("toolkit-cli v0.1.0 (/w) []\nserde v1.0.0 []\n", "", 0);
        retry(|| {
            refuse_forbidden_features(&repo_root(), &step, &absent.resolver(Network::Inherit))
        })
        .expect("absence of toolkit-aws is not a violation");
    }

    #[test]
    fn the_lint_rejects_a_superset_that_loses_a_guarded_package() {
        // --all-features is a superset of the declared features: a guarded
        // package seen in the first graph and missing from the second means a
        // bad parse or resolution, not a clean graph.
        let declaration = declaration();
        let binary = toolkit_cli(&declaration);
        let dir = common::temp_dir("fake-cargo-lint");
        let program = dir.join("cargo");
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--all-features*) printf 'toolkit-cli v0.1.0 (/w) []\\n' ;;\n  *) printf 'toolkit-cli v0.1.0 (/w) []\\ntoolkit-aws v0.1.0 (/w) [sdk]\\n' ;;\nesac\n"
        );
        common::write(&program, &script);
        std::fs::set_permissions(
            &program,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let resolver = Resolver::offline().with_cargo(&program);
        let error =
            retry(|| check_binary(&repo_root(), &resolver, &binary).map(|_| ())).unwrap_err();
        assert!(
            error.contains("missing from the --all-features superset"),
            "{error}"
        );
    }
}
