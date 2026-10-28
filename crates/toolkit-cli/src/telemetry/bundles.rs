//! Generation and check of the self-contained telemetry bundles
//! (`sync_bundles.py`).
//!
//! Only the listed runtime files are distributed into
//! `skills/<skill>/references/vendored/telemetry/`; tests, caches and
//! unrelated scripts never enter a skill bundle. The existing DSL interpreter
//! copies, whose telemetry boundary instructions are shared by Heroku and
//! Azure, are checked too. The writes use the shared, link-refusing tree
//! primitives of `toolkit_core::treesync`; `toolkit sync vendored` keeps
//! skipping the `telemetry/` directory, so exactly one gate owns the bundle.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use toolkit_core::treesync::{copy_file_atomic, ensure_dir_chain, list_tree, lstat_kind, Kind};
use toolkit_core::{neutralize_text, read_regular_file, Json, ToolkitError, MAX_TEXT_FILE_BYTES};

use crate::manifests::py_eq;
use crate::startup::artifacts::pyjson;
use crate::Output;

/// Default plugin root, relative to the current directory (the repository
/// root), as for `sync vendored`.
const DEFAULT_PLUGIN_ROOT: &str = "plugins/aws-startup-advisor";

/// The skills that bundle the telemetry runtime.
pub(super) const SKILLS: [&str; 4] = [
    "azure-to-aws",
    "gcp-to-aws",
    "heroku-to-aws",
    "llm-to-bedrock",
];

/// Runtime files of `scripts/telemetry` distributed into every bundle.
pub(super) const RUNTIME_FILES: [&str; 10] = [
    "cli.py",
    "PROTOCOL.md",
    "consent/record.py",
    "consent/notice.py",
    "consent/cli.py",
    "consent/accept.py",
    "consent/opt_out.py",
    "metric_emission/client.py",
    "metric_emission/migration.py",
    "metric_emission/migration_attributes.py",
];

const VERSION_FILE: &str = "version.json";
const BUNDLE_DIR: &str = "references/vendored/telemetry";
const HOST_MANIFESTS: [&str; 4] = [
    ".claude-plugin/plugin.json",
    ".cursor-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    ".devin-plugin/plugin.json",
];
/// Skills whose vendored DSL interpreter copy is checked with the bundles.
const DSL_SKILLS: [&str; 3] = ["agent-advisor", "azure-to-aws", "heroku-to-aws"];
const INTERPRETER_CANONICAL: &str = "skills/shared/dsl/INTERPRETER.md";
const INTERPRETER_COPY: &str = "references/vendored/dsl/INTERPRETER.md";

fn failure(action: &str, path: &Path) -> ToolkitError {
    ToolkitError::runtime(
        "telemetry_bundles_io",
        format!("cannot {action}: {}", path.display()),
    )
}

fn read_json(path: &Path) -> Result<Json, ToolkitError> {
    let text = read_regular_file(path, MAX_TEXT_FILE_BYTES).map_err(|_| failure("read", path))?;
    pyjson::loads(&text).map_err(|_| failure("parse", path))
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, ToolkitError> {
    match lstat_kind(path)? {
        Some(Kind::File) => fs::read(path).map_err(|_| failure("read", path)),
        _ => Err(failure("read (not a regular file)", path)),
    }
}

/// Writes `bytes` to `dest` through a sibling temporary file and a rename, so
/// an existing destination link is replaced as a link, never written through.
fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), ToolkitError> {
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary =
        dest.with_file_name(format!(".{name}.toolkit-telemetry-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&temporary, dest));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(failure("write", dest));
    }
    Ok(())
}

/// Where a bundle file's bytes come from.
enum Source<'a> {
    /// Copy this regular file.
    File(PathBuf),
    /// Write these bytes.
    Bytes(&'a [u8]),
}

fn matches_source(dest: &Path, source: &Source<'_>) -> Result<bool, ToolkitError> {
    if lstat_kind(dest)? != Some(Kind::File) {
        return Ok(false);
    }
    let expected = match source {
        Source::File(path) => read_bytes(path)?,
        Source::Bytes(bytes) => bytes.to_vec(),
    };
    Ok(fs::read(dest).is_ok_and(|actual| actual == expected))
}

fn install(plugin: &Path, relative: &str, source: &Source<'_>) -> Result<(), ToolkitError> {
    let (directory, _) = relative.rsplit_once('/').unwrap_or(("", relative));
    ensure_dir_chain(plugin, directory)?;
    let dest = plugin.join(relative);
    match source {
        Source::File(path) => copy_file_atomic(path, &dest),
        Source::Bytes(bytes) => write_atomic(&dest, bytes),
    }
}

/// Everything out of sync, as printable lines. Refuses (returns the version
/// conflicts) to generate bundles from conflicting version metadata.
///
/// # Errors
///
/// A runtime error when an input cannot be read or a destination cannot be
/// written.
pub(super) fn sync(plugin: &Path, write: bool) -> Result<Vec<String>, ToolkitError> {
    let telemetry = plugin.join("scripts/telemetry");
    let version = read_json(&plugin.join("plugin.json"))?
        .get("version")
        .cloned()
        .ok_or_else(|| failure("find a version in", &plugin.join("plugin.json")))?;
    let mut problems = Vec::new();
    for name in HOST_MANIFESTS {
        let manifest = plugin.join(name);
        if lstat_kind(&manifest)? == Some(Kind::File) {
            let host_version = read_json(&manifest)?
                .get("version")
                .cloned()
                .unwrap_or(Json::Null);
            if !py_eq(&host_version, &version) {
                problems.push(format!("{name}: version does not match plugin.json"));
            }
        }
    }
    if !problems.is_empty() {
        return Ok(problems); // Refuse to generate bundles from conflicting version metadata.
    }

    let mut version_json = pyjson::dumps(&Json::object(vec![("version", version)]), Some(2), false);
    version_json.push('\n');
    let mut files: Vec<(&str, Source<'_>)> = RUNTIME_FILES
        .iter()
        .map(|name| (*name, Source::File(telemetry.join(name))))
        .collect();
    files.push((VERSION_FILE, Source::Bytes(version_json.as_bytes())));

    let mut expected: Vec<(String, &Source<'_>)> = Vec::new();
    for skill in SKILLS {
        for (name, source) in &files {
            expected.push((format!("skills/{skill}/{BUNDLE_DIR}/{name}"), source));
        }
    }
    let interpreter = Source::File(plugin.join(INTERPRETER_CANONICAL));
    // The canonical interpreter must be readable even when no copy is missing.
    read_bytes(&plugin.join(INTERPRETER_CANONICAL))?;
    for skill in DSL_SKILLS {
        expected.push((format!("skills/{skill}/{INTERPRETER_COPY}"), &interpreter));
    }
    for (_, source) in &files {
        if let Source::File(path) = source {
            read_bytes(path)?;
        }
    }

    for (relative, source) in &expected {
        if matches_source(&plugin.join(relative), source)? {
            continue;
        }
        if write {
            install(plugin, relative, source)?;
        } else {
            problems.push(relative.clone());
        }
    }

    for skill in SKILLS {
        let target = plugin.join("skills").join(skill).join(BUNDLE_DIR);
        if lstat_kind(&target)? != Some(Kind::Dir) {
            continue;
        }
        for entry in list_tree(&target)? {
            if entry.kind == Kind::Dir {
                continue;
            }
            if entry.rel.split('/').any(|part| part == "__pycache__")
                && Path::new(&entry.rel)
                    .extension()
                    .is_some_and(|ext| ext == "pyc")
            {
                continue;
            }
            if !files.iter().any(|(name, _)| *name == entry.rel) {
                problems.push(format!(
                    "Unexpected bundle file: skills/{skill}/{BUNDLE_DIR}/{}",
                    entry.rel
                ));
            }
        }
    }
    Ok(problems)
}

/// `toolkit telemetry bundles [--write | --check] [--plugin-root DIR]`.
pub(super) fn run(args: &[&str], output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let mut write = false;
    let mut check = false;
    let mut root = DEFAULT_PLUGIN_ROOT.to_string();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match *arg {
            "--write" => write = true,
            "--check" => check = true,
            "--plugin-root" => match iter.next() {
                Some(value) if !value.is_empty() && !value.starts_with('-') => {
                    root = (*value).to_string();
                }
                _ => {
                    return Err(ToolkitError::usage(
                        "invalid_arguments",
                        "invalid arguments",
                    ))
                }
            },
            other => match other.strip_prefix("--plugin-root=") {
                Some(value) if !value.is_empty() => root = value.to_string(),
                _ => {
                    return Err(ToolkitError::usage(
                        "invalid_arguments",
                        "invalid arguments",
                    ))
                }
            },
        }
    }
    if write && check {
        return Err(ToolkitError::usage(
            "invalid_arguments",
            "invalid arguments",
        ));
    }
    let problems = sync(Path::new(&root), write)?;
    if problems.is_empty() {
        return output.line("Telemetry bundles and DSL copies: OK");
    }
    output.line("Telemetry manifests or bundles are out of sync:")?;
    for problem in &problems {
        output.line(&neutralize_text(problem))?;
    }
    output.line("Ensure manifest versions agree, then run `toolkit telemetry bundles --write`.")?;
    output.line(
        "Review and remove unexpected bundle files manually; --write does not delete them.",
    )?;
    Err(ToolkitError::runtime(
        "telemetry_bundles_drift",
        format!("{} telemetry bundle problem(s)", problems.len()),
    ))
}
