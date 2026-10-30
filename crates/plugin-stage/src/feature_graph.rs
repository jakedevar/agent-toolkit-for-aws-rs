//! Shipped-build feature gate (#5592): no declared binary's normal (non-dev)
//! dependency graph may enable an offline test-injection feature, today
//! `toolkit-aws/testing`, which gates the unsigned `ServiceBridge::with_sender`.
//!
//! The check asks cargo for the resolved graph, offline and without building:
//! `cargo tree --offline --locked -p PACKAGE [--features F | --all-features]
//! --target all -e no-dev`. `-e no-dev` is the graph a release build sees, so
//! `toolkit-cli`'s own dev-dependency on `toolkit-aws/testing` is not part of
//! it. Cargo features are additive: this is a guard on what the shipped
//! declaration and the workspace manifests enable, not a proof that no other
//! build can opt in (`cargo build --features toolkit-aws/testing` by hand
//! stays possible and is the deliberate escape hatch).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{BinarySpec, Declaration};

/// `(package, feature)` pairs a shipped graph must not enable.
pub const FORBIDDEN: &[(&str, &str)] = &[("toolkit-aws", "testing")];

/// The `cargo tree` arguments (after `cargo`) for one package's normal
/// feature graph: the declared features, or every feature of the package.
#[must_use]
pub fn tree_args(package: &str, features: &[String], all_features: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "tree",
        "--offline",
        "--locked",
        "--package",
        package,
        "--target",
        "all",
        "--edges",
        "no-dev",
        "--prefix",
        "none",
        "--format",
        "{p} [{f}]",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    if all_features {
        args.push("--all-features".to_string());
    } else if !features.is_empty() {
        args.push("--features".to_string());
        args.push(features.join(","));
    }
    args
}

/// Every feature set `package` appears with in `cargo tree --format "{p} [{f}]"`
/// output (`name vX.Y.Z (path) [a,b] (*)`).
#[must_use]
pub fn feature_sets(tree: &str, package: &str) -> Vec<BTreeSet<String>> {
    tree.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, rest) = line.split_once(' ')?;
            if name != package {
                return None;
            }
            let open = rest.rfind('[')?;
            let close = rest[open..].find(']')? + open;
            Some(
                rest[open + 1..close]
                    .split(',')
                    .map(str::trim)
                    .filter(|feature| !feature.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        })
        .collect()
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

/// Judge `cargo tree` output of `subject`'s normal graph.
#[must_use]
pub fn judge(subject: &str, tree: &str) -> GraphReport {
    let mut violations = Vec::new();
    let mut guarded_present = BTreeSet::new();
    for (package, feature) in FORBIDDEN {
        let sets = feature_sets(tree, package);
        if !sets.is_empty() {
            guarded_present.insert((*package).to_string());
        }
        if sets.iter().any(|set| set.contains(*feature)) {
            violations.push(format!(
                "{subject}: the normal (non-dev) feature graph enables {package}/{feature}, which exposes unsigned test injection; remove it from the shipped feature set"
            ));
        }
    }
    GraphReport {
        violations,
        guarded_present,
    }
}

/// Resolve and judge one package's normal feature graph.
///
/// # Errors
///
/// When cargo cannot run offline against the lockfile, or fails.
pub fn check_graph(
    root: &Path,
    subject: &str,
    package: &str,
    features: &[String],
    all_features: bool,
) -> Result<GraphReport, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(tree_args(package, features, all_features))
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("{subject}: cannot run cargo tree: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{subject}: `cargo tree --offline --locked` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    Ok(judge(subject, &String::from_utf8_lossy(&output.stdout)))
}

/// Check the declared feature set of one shipped binary, then every feature
/// of its package, since a later default or feature edit that pulls the
/// forbidden feature in is the accident this guards.
///
/// # Errors
///
/// When cargo cannot resolve the graph.
pub fn check_binary(root: &Path, binary: &BinarySpec) -> Result<GraphReport, String> {
    let subject = format!("{}/{}", binary.destination.key(), binary.name);
    let mut report = check_graph(root, &subject, &binary.package, &binary.features, false)?;
    let all = check_graph(
        root,
        &format!("{subject} (--all-features)"),
        &binary.package,
        &[],
        true,
    )?;
    report.violations.extend(all.violations);
    report.guarded_present.extend(all.guarded_present);
    Ok(report)
}

/// Check every binary of a declaration.
///
/// # Errors
///
/// When cargo cannot resolve a graph.
pub fn check_declaration(root: &Path, declaration: &Declaration) -> Result<GraphReport, String> {
    let mut total = GraphReport {
        violations: Vec::new(),
        guarded_present: BTreeSet::new(),
    };
    let mut seen = BTreeSet::new();
    for binary in &declaration.binaries {
        if !seen.insert((binary.package.clone(), binary.features.clone())) {
            continue;
        }
        let report = check_binary(root, binary)?;
        total.violations.extend(report.violations);
        total.guarded_present.extend(report.guarded_present);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_cargo_tree_spelling() {
        let tree = "\
toolkit-cli v0.1.0 (/w/crates/toolkit-cli) []
toolkit-aws v0.1.0 (/w/crates/toolkit-aws) [default,sdk]
toolkit-aws v0.1.0 (/w/crates/toolkit-aws) [default,sdk,testing] (*)
toolkit-aws-extra v0.1.0 [testing]
serde_derive v1.0.0 (proc-macro) []
";
        let sets = feature_sets(tree, "toolkit-aws");
        assert_eq!(sets.len(), 2);
        assert!(!sets[0].contains("testing"));
        assert!(sets[1].contains("testing") && sets[1].contains("sdk"));
        assert!(feature_sets(tree, "absent").is_empty());
        let report = judge("x", tree);
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].contains("toolkit-aws/testing"));
    }

    #[test]
    fn clean_and_absent_graphs_pass() {
        let clean = "toolkit-aws v0.1.0 (/w) [default,sdk]\n";
        assert!(judge("x", clean).violations.is_empty());
        assert!(judge("x", clean).guarded_present.contains("toolkit-aws"));
        let absent = "secret-safety-hook v0.1.0 (/w) []\n";
        let report = judge("x", absent);
        assert!(report.violations.is_empty() && report.guarded_present.is_empty());
    }

    #[test]
    fn arguments_are_offline_locked_normal_edges() {
        let args = tree_args("toolkit-cli", &["a".into(), "b".into()], false);
        assert!(args.contains(&"--offline".to_string()));
        assert!(args.contains(&"--locked".to_string()));
        assert!(args.windows(2).any(|w| w == ["--edges", "no-dev"]));
        assert!(args.windows(2).any(|w| w == ["--features", "a,b"]));
        assert!(tree_args("p", &[], true).contains(&"--all-features".to_string()));
    }
}
