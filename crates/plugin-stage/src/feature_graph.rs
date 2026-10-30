//! Shipped-build feature gate (#5592): no declared binary's normal (non-dev)
//! dependency graph may enable an offline test-injection feature, today
//! `toolkit-aws/testing`, which gates the unsigned `ServiceBridge::with_sender`.
//!
//! The check asks cargo for the resolved graph without building. `-e no-dev`
//! is the graph a release build sees, so `toolkit-cli`'s own dev-dependency on
//! `toolkit-aws/testing` is not part of it. Cargo features are additive: this
//! is a guard on what the shipped declaration and the workspace manifests
//! enable, not a proof that no other build can opt in (`cargo build
//! --features toolkit-aws/testing` by hand stays possible and is the
//! deliberate escape hatch).
//!
//! Two scopes, deliberately different (#5603):
//!
//! * [`preflight`] is the production gate `plugin-stage stage` runs before a
//!   compile. It resolves exactly the graph that compile will build: the same
//!   package, the declared features and the build's own target triple. It
//!   follows the staging build's network policy ([`Network::Inherit`]: no
//!   `--offline` of its own, so ambient `CARGO_NET_OFFLINE` / `net.offline`
//!   decide for both commands) and keeps `--locked`. Resolving more than the
//!   build needs (`--target all`, `--all-features`) could demand packages a
//!   cache populated for the selected build lacks, so a build that works would
//!   be refused by its own guard. The declared-feature graph for the build's
//!   target is the whole shipped graph for that build, so nothing is lost.
//! * [`check_binary`] / [`check_declaration`] are the broad drift lint: every
//!   target and `--all-features`, so a later feature edit that pulls the
//!   forbidden feature in is caught in review before it is declared. It runs
//!   in the tests with an explicit [`Network::Offline`] and is never part of
//!   staging.
//!
//! Neither scope skips on error: an unresolvable graph, empty output, a
//! missing root row or a malformed row for a guarded package is a failure,
//! never a pass.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{BinarySpec, Declaration};

/// `(package, feature)` pairs a shipped graph must not enable.
pub const FORBIDDEN: &[(&str, &str)] = &[("toolkit-aws", "testing")];

/// Whether graph resolution may reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Add no network flag: the ambient cargo configuration decides, exactly
    /// as it does for the staging build this guards.
    Inherit,
    /// `--offline`: deterministic tests that must never touch a registry.
    Offline,
}

/// How to run cargo for graph resolution.
#[derive(Debug, Clone)]
pub struct Resolver {
    /// The cargo program.
    pub cargo: OsString,
    /// The network policy.
    pub network: Network,
}

impl Resolver {
    /// The production resolver: the same `cargo` the staging build runs and
    /// the same (inherited) network policy.
    #[must_use]
    pub fn production() -> Self {
        Self {
            cargo: "cargo".into(),
            network: Network::Inherit,
        }
    }

    /// An explicit offline resolver for deterministic tests (cargo from
    /// `$CARGO` when set).
    #[must_use]
    pub fn offline() -> Self {
        Self {
            cargo: std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            network: Network::Offline,
        }
    }

    /// The same policy with a different cargo program.
    #[must_use]
    pub fn with_cargo(mut self, cargo: impl Into<OsString>) -> Self {
        self.cargo = cargo.into();
        self
    }
}

/// One `cargo tree` question about a package's normal graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The root package (`--package`).
    pub package: String,
    /// Declared features (`--features`), ignored with `all_features`.
    pub features: Vec<String>,
    /// `--all-features`.
    pub all_features: bool,
    /// `--target`: a triple, or `None` for `all`.
    pub target: Option<String>,
}

/// The `cargo tree` arguments (after `cargo`) for a query.
#[must_use]
pub fn tree_args(query: &Query, network: Network) -> Vec<String> {
    let mut args: Vec<String> = ["tree", "--locked"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    if network == Network::Offline {
        args.push("--offline".to_string());
    }
    for arg in ["--package", query.package.as_str()] {
        args.push(arg.to_string());
    }
    args.push("--target".to_string());
    args.push(query.target.clone().unwrap_or_else(|| "all".to_string()));
    for arg in [
        "--edges",
        "no-dev",
        "--prefix",
        "none",
        "--format",
        "{p} [{f}]",
    ] {
        args.push(arg.to_string());
    }
    if query.all_features {
        args.push("--all-features".to_string());
    } else if !query.features.is_empty() {
        args.push("--features".to_string());
        args.push(query.features.join(","));
    }
    args
}

/// The first whitespace-delimited token of a trimmed row: the package name.
fn row_name(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// The feature set in one row (`name vX.Y.Z (path) [a,b] (*)`), or an error
/// when the row has no bracketed feature list.
fn row_features(line: &str) -> Result<BTreeSet<String>, String> {
    let open = line
        .rfind('[')
        .ok_or_else(|| format!("row has no feature list: `{line}`"))?;
    let close = line[open..]
        .find(']')
        .ok_or_else(|| format!("row has an unterminated feature list: `{line}`"))?
        + open;
    Ok(line[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .map(str::to_string)
        .collect())
}

/// Every feature set `package` appears with in `cargo tree --format "{p} [{f}]"`
/// output. A row naming the package without a parseable feature list is an
/// error, never silently dropped.
///
/// # Errors
///
/// When a row for `package` is malformed.
pub fn feature_sets(tree: &str, package: &str) -> Result<Vec<BTreeSet<String>>, String> {
    let mut sets = Vec::new();
    for line in tree.lines().map(str::trim) {
        if row_name(line) == package {
            sets.push(row_features(line).map_err(|err| format!("{package}: {err}"))?);
        }
    }
    Ok(sets)
}

/// The result of checking one graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphReport {
    /// Human-readable violations, empty when the graph is clean.
    pub violations: Vec<String>,
    /// Packages from [`FORBIDDEN`] that appear in the graph at all, so a
    /// caller can prove the parse is not vacuous.
    pub guarded_present: BTreeSet<String>,
}

/// Judge `cargo tree` output of `package`'s normal graph. The output must be
/// non-empty and its first row (the root, with `--prefix none`) must be
/// `package`; a guarded package may legitimately be absent.
///
/// # Errors
///
/// When the output is empty, has the wrong root, or has a malformed row for a
/// guarded package.
pub fn judge(subject: &str, package: &str, tree: &str) -> Result<GraphReport, String> {
    let root = tree
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| format!("{subject}: `cargo tree` printed no packages"))?;
    if row_name(root) != package {
        return Err(format!(
            "{subject}: `cargo tree` root row is `{root}`, expected package {package}"
        ));
    }
    let mut violations = Vec::new();
    let mut guarded_present = BTreeSet::new();
    for (guarded, feature) in FORBIDDEN {
        let sets = feature_sets(tree, guarded).map_err(|err| format!("{subject}: {err}"))?;
        if !sets.is_empty() {
            guarded_present.insert((*guarded).to_string());
        }
        if sets.iter().any(|set| set.contains(*feature)) {
            violations.push(format!(
                "{subject}: the normal (non-dev) feature graph enables {guarded}/{feature}, which exposes unsigned test injection; remove it from the shipped feature set"
            ));
        }
    }
    Ok(GraphReport {
        violations,
        guarded_present,
    })
}

/// Resolve and judge one query.
///
/// # Errors
///
/// When cargo cannot run or fails (the graph cannot be resolved under the
/// resolver's network policy and `--locked`), or its output is invalid.
pub fn check_graph(
    root: &Path,
    resolver: &Resolver,
    subject: &str,
    query: &Query,
) -> Result<GraphReport, String> {
    let output = Command::new(&resolver.cargo)
        .args(tree_args(query, resolver.network))
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("{subject}: cannot run cargo tree: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{subject}: `cargo tree --locked` could not resolve the feature graph ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    judge(
        subject,
        &query.package,
        &String::from_utf8_lossy(&output.stdout),
    )
}

fn binary_subject(binary: &BinarySpec) -> String {
    format!("{}/{}", binary.destination.key(), binary.name)
}

/// The production gate: the graph one staging build will compile (declared
/// features, the build's `triple`), resolved under the resolver's network
/// policy. Returns the report; the caller refuses staging on violations.
///
/// # Errors
///
/// When the graph cannot be resolved or validated: staging must fail, never
/// skip the check.
pub fn preflight(
    root: &Path,
    resolver: &Resolver,
    binary: &BinarySpec,
    triple: &str,
) -> Result<GraphReport, String> {
    check_graph(
        root,
        resolver,
        &format!("{} [{triple}]", binary_subject(binary)),
        &Query {
            package: binary.package.clone(),
            features: binary.features.clone(),
            all_features: false,
            target: Some(triple.to_string()),
        },
    )
}

/// The broad drift lint for one shipped binary: the declared feature set over
/// every target, then every feature of its package. `--all-features` is a
/// superset, so a guarded package present with the declared features must
/// also be present with all of them; if not, a parse or resolution is wrong
/// and the lint fails instead of passing.
///
/// # Errors
///
/// When cargo cannot resolve a graph, an output is invalid or the two graphs
/// are inconsistent.
pub fn check_binary(
    root: &Path,
    resolver: &Resolver,
    binary: &BinarySpec,
) -> Result<GraphReport, String> {
    let subject = binary_subject(binary);
    let declared = check_graph(
        root,
        resolver,
        &subject,
        &Query {
            package: binary.package.clone(),
            features: binary.features.clone(),
            all_features: false,
            target: None,
        },
    )?;
    let all_subject = format!("{subject} (--all-features)");
    let all = check_graph(
        root,
        resolver,
        &all_subject,
        &Query {
            package: binary.package.clone(),
            features: Vec::new(),
            all_features: true,
            target: None,
        },
    )?;
    if let Some(missing) = declared
        .guarded_present
        .difference(&all.guarded_present)
        .next()
    {
        return Err(format!(
            "{all_subject}: {missing} is in the declared graph but missing from the --all-features superset; the graph output cannot be trusted"
        ));
    }
    let mut violations = declared.violations;
    violations.extend(all.violations);
    let mut guarded_present = declared.guarded_present;
    guarded_present.extend(all.guarded_present);
    Ok(GraphReport {
        violations,
        guarded_present,
    })
}

/// Lint every binary of a declaration.
///
/// # Errors
///
/// When any binary's lint cannot resolve or validate a graph.
pub fn check_declaration(
    root: &Path,
    resolver: &Resolver,
    declaration: &Declaration,
) -> Result<GraphReport, String> {
    let mut total = GraphReport {
        violations: Vec::new(),
        guarded_present: BTreeSet::new(),
    };
    let mut seen = BTreeSet::new();
    for binary in &declaration.binaries {
        if !seen.insert((binary.package.clone(), binary.features.clone())) {
            continue;
        }
        let report = check_binary(root, resolver, binary)?;
        total.violations.extend(report.violations);
        total.guarded_present.extend(report.guarded_present);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(package: &str, features: &[&str], all: bool, target: Option<&str>) -> Query {
        Query {
            package: package.to_string(),
            features: features.iter().map(|f| (*f).to_string()).collect(),
            all_features: all,
            target: target.map(str::to_string),
        }
    }

    #[test]
    fn parses_every_cargo_tree_spelling() {
        let tree = "\
toolkit-cli v0.1.0 (/w/crates/toolkit-cli) []
toolkit-aws v0.1.0 (/w/crates/toolkit-aws) [default,sdk]
toolkit-aws v0.1.0 (/w/crates/toolkit-aws) [default,sdk,testing] (*)
toolkit-aws-extra v0.1.0 [testing]
serde_derive v1.0.0 (proc-macro) []
";
        let sets = feature_sets(tree, "toolkit-aws").unwrap();
        assert_eq!(sets.len(), 2);
        assert!(!sets[0].contains("testing"));
        assert!(sets[1].contains("testing") && sets[1].contains("sdk"));
        assert!(feature_sets(tree, "absent").unwrap().is_empty());
        let report = judge("x", "toolkit-cli", tree).unwrap();
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].contains("toolkit-aws/testing"));
    }

    #[test]
    fn clean_and_absent_graphs_pass() {
        let clean = "toolkit-cli v0.1.0 (/w) []\ntoolkit-aws v0.1.0 (/w) [default,sdk]\n";
        let report = judge("x", "toolkit-cli", clean).unwrap();
        assert!(report.violations.is_empty());
        assert!(report.guarded_present.contains("toolkit-aws"));
        // A binary that legitimately lacks toolkit-aws passes.
        let absent = "secret-safety-hook v0.1.0 (/w) []\nserde v1.0.0 []\n";
        let report = judge("x", "secret-safety-hook", absent).unwrap();
        assert!(report.violations.is_empty() && report.guarded_present.is_empty());
    }

    #[test]
    fn empty_wrong_root_and_malformed_guarded_rows_are_errors() {
        for empty in ["", "\n  \n"] {
            let err = judge("x", "toolkit-cli", empty).unwrap_err();
            assert!(err.contains("no packages"), "{err}");
        }
        let err = judge("x", "toolkit-cli", "other v0.1.0 (/w) []\n").unwrap_err();
        assert!(err.contains("expected package toolkit-cli"), "{err}");
        let root = "toolkit-cli v0.1.0 (/w) []\n";
        for bad in [
            "toolkit-aws v0.1.0 (/w)",
            "toolkit-aws v0.1.0 (/w) [default,testing",
            "toolkit-aws",
            "toolkit-aws v0.1.0 (/w) testing]",
        ] {
            let err = judge("x", "toolkit-cli", &format!("{root}{bad}\n")).unwrap_err();
            assert!(err.contains("toolkit-aws"), "{bad}: {err}");
        }
    }

    #[test]
    fn production_arguments_follow_the_build_and_add_no_network_flag() {
        let args = tree_args(
            &query(
                "toolkit-cli",
                &["a", "b"],
                false,
                Some("x86_64-unknown-linux-gnu"),
            ),
            Network::Inherit,
        );
        assert!(!args.contains(&"--offline".to_string()));
        assert!(!args.contains(&"--all-features".to_string()));
        assert!(args.contains(&"--locked".to_string()));
        assert!(args
            .windows(2)
            .any(|w| w == ["--target", "x86_64-unknown-linux-gnu"]));
        assert!(args.windows(2).any(|w| w == ["--edges", "no-dev"]));
        assert!(args.windows(2).any(|w| w == ["--features", "a,b"]));
    }

    #[test]
    fn lint_arguments_are_explicitly_offline_and_broad() {
        let args = tree_args(&query("p", &[], true, None), Network::Offline);
        assert!(args.contains(&"--offline".to_string()));
        assert!(args.contains(&"--locked".to_string()));
        assert!(args.contains(&"--all-features".to_string()));
        assert!(args.windows(2).any(|w| w == ["--target", "all"]));
    }
}
