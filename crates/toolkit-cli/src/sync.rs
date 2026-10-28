//! Handlers for the `toolkit sync ...` commands (#4377).
//!
//! The behavior lives in `toolkit_core` ([`toolkit_core::plugin_skills`] and
//! [`toolkit_core::vendored`]); these handlers only map options to library
//! calls and the reports to stdout lines and the shared error contract.
//!
//! Exit status: 0 when everything is in sync (or after a successful write), 1
//! for drift in `--check` mode, for refusals (links, ambiguous sources, orphan
//! copies) and for runtime failures. Drift details are part of the error
//! message (stderr) for `plugin-skills`, and part of stdout for `vendored`,
//! matching the Python tools they replace.

use std::path::PathBuf;

use toolkit_core::{sync_plugin_skills, sync_vendored, PluginSkillsOptions, ToolkitError};

use crate::{Invocation, Output};

/// Registration of the `sync` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "sync",
    commands: &[
        crate::CommandSpec {
            path: &["sync", "plugin-skills"],
            flags: &["--check"],
            value_flags: &["--root", "--plugin"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: plugin_skills,
        },
        crate::CommandSpec {
            path: &["sync", "vendored"],
            flags: &["--check"],
            value_flags: &["--plugin-root"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: vendored,
        },
    ],
    synopsis: r"  toolkit sync plugin-skills [--root DIR] [--plugin NAME] [--check]
  toolkit sync vendored [--plugin-root DIR] [--check]
",
    description: r"  sync plugin-skills
                Make plugins/<plugin>/skills/<name> copies byte-identical to the
                canonical skills/**/<name> (a directory holding SKILL.md). --root
                defaults to the current directory. --check reports drift, exits
                1 and writes nothing.
  sync vendored Make each skills/<skill>/references/vendored copy in the plugin
                at --plugin-root (default plugins/aws-startup-advisor)
                byte-identical to skills/shared and its README table. --check
                reports drift, exits 1 and writes nothing.
",
    exit_notes: "",
    doc: "docs/migration/sync-contracts.md",
    synopsis_rank: 200,
    description_rank: 200,
};

/// Default plugin root for `sync vendored`, relative to the current
/// directory (the repository root).
const DEFAULT_VENDORED_PLUGIN_ROOT: &str = "plugins/aws-startup-advisor";

fn emit(output: &mut Output<'_>, lines: &[String]) -> Result<(), ToolkitError> {
    for line in lines {
        output.line(line)?;
        if output.is_closed() {
            break;
        }
    }
    Ok(())
}

/// `toolkit sync plugin-skills [--root DIR] [--plugin NAME] [--check]`.
pub(crate) fn plugin_skills(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let root = PathBuf::from(invocation.value("--root").unwrap_or("."));
    let check = invocation.has_flag("--check");
    let report = sync_plugin_skills(&PluginSkillsOptions {
        root: &root,
        plugin: invocation.value("--plugin"),
        check,
    })?;
    emit(output, &report.lines)?;
    match report.failure_message(check) {
        Some(message) => Err(ToolkitError::runtime("sync_failed", message)),
        None => Ok(()),
    }
}

/// `toolkit sync vendored [--plugin-root DIR] [--check]`.
pub(crate) fn vendored(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let root = PathBuf::from(
        invocation
            .value("--plugin-root")
            .unwrap_or(DEFAULT_VENDORED_PLUGIN_ROOT),
    );
    let check = invocation.has_flag("--check");
    let report = sync_vendored(&root, check)?;
    emit(output, &report.progress)?;
    emit(output, &report.summary(check))?;
    if report.is_ok() {
        Ok(())
    } else {
        Err(ToolkitError::runtime(
            "vendored_drift",
            format!("{} vendored file(s) out of contract", report.errors.len()),
        ))
    }
}
