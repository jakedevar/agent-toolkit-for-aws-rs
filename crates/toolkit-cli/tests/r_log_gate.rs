//! R-LOG gate (#4996, #4413 S1): no logging backend, ever.
//!
//! aws-config records provider errors through `tracing` before the boundary
//! discards their text, and a `credential_process` error carries the child's
//! stderr verbatim. With no subscriber and no logger installed, `tracing`
//! reports every callsite disabled and those field values are never even
//! formatted. This test keeps it that way:
//!
//! 1. the resolved dependency graph of `toolkit-cli` and `toolkit-aws` holds
//!    no `tracing`/`log` backend crate;
//! 2. in that graph `tracing` is built without its `log` (or `log-always`)
//!    feature;
//! 3. no workspace source installs a global or scoped dispatcher, subscriber
//!    or logger.
//!
//! Checks 1 and 2 read `cargo tree` for those two packages with every feature
//! and every edge kind, not the whole-workspace `Cargo.lock`: the lock is
//! feature-unified across all members, and the Lambda example members
//! (#4397: `lambda_runtime` brings `tracing-subscriber` and enables
//! `tracing/log`) are outside the `toolkit` binary, which is built per
//! package (`cargo build --package toolkit-cli --bin toolkit`, #4487).
//!
//! A later slice that needs dependency tracing must follow the design's rule
//! 5 (an `Interest::never` filter for every foreign target) and update this
//! gate and T-LEAK-SDK in the same change.

use std::path::{Path, PathBuf};

/// Crates that install or implement a `tracing` subscriber or a `log` logger.
const BACKENDS: &[&str] = &[
    "tracing-subscriber",
    "tracing-log",
    "tracing-appender",
    "tracing-journald",
    "tracing-bunyan-formatter",
    "env_logger",
    "pretty_env_logger",
    "simple_logger",
    "simplelog",
    "fern",
    "log4rs",
    "flexi_logger",
    "slog-stdlog",
    "slog-envlogger",
    "stderrlog",
    "syslog",
    "systemd-journal-logger",
    "android_logger",
    "oslog",
];

/// Calls that would install one.
const INSTALLERS: &[&str] = &[
    "subscriber::set_global_default",
    "dispatcher::set_global_default",
    "subscriber::set_default",
    "dispatcher::set_default",
    "dispatcher::with_default",
    "subscriber::with_default",
    "log::set_logger",
    "log::set_boxed_logger",
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

/// Packages whose resolved graph ships in the `toolkit` binary.
const GATED: &[&str] = &["toolkit-cli", "toolkit-aws"];

/// `(name, enabled features)` for every package in the resolved dependency
/// graph of each [`GATED`] package, with all its features, every edge kind
/// and every target, as `cargo tree` resolves it from the locked lock file.
fn resolved_packages() -> Vec<(String, Vec<String>)> {
    let mut packages = Vec::new();
    for package in GATED {
        let output = std::process::Command::new(env!("CARGO"))
            .current_dir(workspace())
            .args([
                "tree",
                "--locked",
                "--offline",
                "--package",
                package,
                "--all-features",
                "--target",
                "all",
                "--edges",
                "normal,build,dev",
                "--prefix",
                "none",
                "--format",
                "{p}|{f}",
            ])
            .output()
            .expect("run cargo tree");
        assert!(
            output.status.success(),
            "cargo tree -p {package}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for line in String::from_utf8(output.stdout).expect("utf-8").lines() {
            let (id, features) = line.split_once('|').unwrap_or((line, ""));
            let name = id.split(' ').next().unwrap_or_default().to_string();
            let features = features
                .trim_end_matches(" (*)")
                .split(',')
                .filter(|f| !f.is_empty())
                .map(str::to_string)
                .collect();
            packages.push((name, features));
        }
    }
    assert!(packages.len() > 100, "cargo tree listed the graph");
    packages
}

#[test]
fn no_logging_backend_is_in_the_dependency_tree() {
    for (name, _) in resolved_packages() {
        assert!(
            !BACKENDS.contains(&name.as_str()),
            "{name} would format dependency events (R-LOG)"
        );
    }
}

#[test]
fn tracing_is_built_without_its_log_feature() {
    let packages = resolved_packages();
    let tracing: Vec<_> = packages
        .iter()
        .filter(|(name, _)| name == "tracing")
        .collect();
    assert!(!tracing.is_empty(), "the SDK tree uses tracing");
    for (_, features) in tracing {
        assert!(
            !features.iter().any(|f| f == "log" || f == "log-always"),
            "tracing's `log`/`log-always` feature is enabled"
        );
    }
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_workspace_source_installs_a_subscriber_or_logger() {
    let this = Path::new(file!())
        .file_name()
        .expect("this file")
        .to_os_string();
    let mut files = Vec::new();
    rust_sources(&workspace().join("crates"), &mut files);
    assert!(files.len() > 50, "the scan found the workspace sources");
    for file in files {
        if file.file_name() == Some(this.as_os_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("source");
        for installer in INSTALLERS {
            assert!(
                !text.contains(installer),
                "{}: {installer} (R-LOG)",
                file.display()
            );
        }
    }
}
