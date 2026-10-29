//! R-LOG gate (#4996, #4413 S1): no logging backend, ever.
//!
//! aws-config records provider errors through `tracing` before the boundary
//! discards their text, and a `credential_process` error carries the child's
//! stderr verbatim. With no subscriber and no logger installed, `tracing`
//! reports every callsite disabled and those field values are never even
//! formatted. This test keeps it that way:
//!
//! 1. `Cargo.lock` holds no `tracing`/`log` backend crate;
//! 2. `tracing` is built without its `log` (or `log-always`) feature, which
//!    would show as a `log` dependency of `tracing` in the lock file;
//! 3. no workspace source installs a global or scoped dispatcher, subscriber
//!    or logger.
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

/// `(name, dependencies)` for every package in the lock file.
fn lock_packages() -> Vec<(String, Vec<String>)> {
    let lock = std::fs::read_to_string(workspace().join("Cargo.lock")).expect("Cargo.lock");
    let mut packages = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let mut name = String::new();
        let mut dependencies = Vec::new();
        let mut in_dependencies = false;
        for line in block.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("name = ") {
                name = value.trim_matches('"').to_string();
            } else if line == "dependencies = [" {
                in_dependencies = true;
            } else if in_dependencies && line == "]" {
                in_dependencies = false;
            } else if in_dependencies {
                let dependency = line.trim_matches(|c| c == '"' || c == ',');
                let dependency = dependency.split(' ').next().unwrap_or_default();
                dependencies.push(dependency.to_string());
            }
        }
        packages.push((name, dependencies));
    }
    assert!(packages.len() > 100, "the lock file was parsed");
    packages
}

#[test]
fn no_logging_backend_is_in_the_dependency_tree() {
    for (name, _) in lock_packages() {
        assert!(
            !BACKENDS.contains(&name.as_str()),
            "{name} would format dependency events (R-LOG)"
        );
    }
}

#[test]
fn tracing_is_built_without_its_log_feature() {
    let packages = lock_packages();
    let tracing: Vec<_> = packages
        .iter()
        .filter(|(name, _)| name == "tracing")
        .collect();
    assert!(!tracing.is_empty(), "the SDK tree uses tracing");
    for (_, dependencies) in tracing {
        assert!(
            !dependencies.iter().any(|d| d == "log"),
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
