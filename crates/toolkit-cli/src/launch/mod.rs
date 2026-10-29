//! Launch with AWS (`skills/core-skills/launch-with-aws/scripts`), ported to
//! Rust per the accepted design `docs/migration/launch-with-aws-design.md`
//! (#4382, revision 3 at `e03640d1`, pre-review #5038).
//!
//! This module holds slices L0-L2 (#5085), L3-L4 and L6 (#5317: the snapshot
//! download, extraction and apply of [`download`], [`snapshot`] and
//! `archive::{extract, apply, meta}`). L0-L2 are: configuration and local state
//! ([`config`], [`state`]), the bounded byte collector of #5082 ([`buffer`])
//! and the archive boundary ([`archive`]: rooted race-free source walk, Git
//! ignore rules, the strict ZIP writer and reader, version-control name
//! policy and the materialisation planner). L5 (#5345) adds the API client
//! ([`api`]), model-driven shaping ([`shape`]) and the API commands
//! ([`flow`], routed by [`commands`]). The network commands are compiled into
//! the default build only for their offline tests, so dead-code analysis is
//! silenced for the module as a whole.
#![allow(dead_code)]

pub mod api;
pub mod archive;
pub mod buffer;
pub mod callback;
pub mod clock;
pub mod commands;
pub mod config;
pub mod flow;
#[cfg(unix)]
pub mod download;
pub mod http;
pub mod oidc;
pub mod shape;
pub mod snapshot;
pub mod state;
pub mod text;

use std::fmt;

/// The fixed refusal of the archive commands on a platform without the
/// descriptor-relative file-system API (R-AR-OPEN, D16).
pub const PLATFORM_MESSAGE: &str = "This command is not supported on this platform.";

/// A failure of a launch command, printed as `{"error": <message>}` (exit 1)
/// by the command layer (L5). Each variant carries the final, already
/// redacted message: callers never format raw caller values into it.
#[derive(Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The Python `ConfigError`: invalid or unsafe configuration.
    Config(String),
    /// The Python `ArchiveError`: the source tree or an archive was refused.
    Archive(String),
    /// A launch lookup or the snapshot download failed (never naming the
    /// URL).
    Download(String),
    /// A command that needs the Unix file-system boundary on another
    /// platform ([`PLATFORM_MESSAGE`]).
    Platform,
}

impl LaunchError {
    /// The message shown to the user.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Config(message) | Self::Archive(message) | Self::Download(message) => message,
            Self::Platform => PLATFORM_MESSAGE,
        }
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl fmt::Debug for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Config(_) => "Config",
            Self::Archive(_) => "Archive",
            Self::Download(_) => "Download",
            Self::Platform => "Platform",
        };
        write!(f, "LaunchError::{kind}({:?})", self.message())
    }
}

/// The platform switch of the archive commands (R-AR-OPEN, D16): `unix` is
/// `cfg!(unix)` in production and both values in the unit test.
///
/// # Errors
/// [`LaunchError::Platform`] when `unix` is false.
pub fn platform_gate(unix: bool) -> Result<(), LaunchError> {
    if unix {
        Ok(())
    } else {
        Err(LaunchError::Platform)
    }
}

/// [`platform_gate`] for the platform this binary was built for.
///
/// # Errors
/// [`LaunchError::Platform`] off Unix.
pub fn require_archive_platform() -> Result<(), LaunchError> {
    platform_gate(cfg!(unix))
}

#[cfg(test)]
mod tests_mod;

#[cfg(test)]
pub mod tests_support;

/// Every launch route: the API commands and offline commands (L5), sign-in
/// (L4) and the snapshot routes (L6).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "launch",
    commands: &[crate::CommandSpec {
        path: &["launch", "resolve-region"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 0, passthrough: false, handler: commands::resolve_region,
    }, crate::CommandSpec {
        path: &["launch", "create-launch"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 2, passthrough: false, handler: commands::create_launch,
    }, crate::CommandSpec {
        path: &["launch", "get-launch"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 2, passthrough: false, handler: commands::get_launch,
    }, crate::CommandSpec {
        path: &["launch", "list-launches"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 0, passthrough: false, handler: commands::list_launches,
    }, crate::CommandSpec {
        path: &["launch", "delete-launch"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 1, passthrough: false, handler: commands::delete_launch,
    }, crate::CommandSpec {
        path: &["launch", "start-launch-execution"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 1, passthrough: false, handler: commands::start_execution,
    }, crate::CommandSpec {
        path: &["launch", "get-launch-status"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 1, passthrough: false, handler: commands::get_status,
    }, crate::CommandSpec {
        path: &["launch", "get-launch-download-url"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 1, passthrough: false, handler: commands::get_download_url,
    }, crate::CommandSpec {
        path: &["launch", "session-status"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 0, passthrough: false, handler: commands::session_status,
    }, crate::CommandSpec {
        path: &["launch", "sign-out"], flags: &[], value_flags: &["--region", "--aws-mcp-url"], strict_values: true, repeatable_flags: &[], max_positionals: 0, passthrough: false, handler: commands::sign_out,
    }, crate::CommandSpec {
        path: &["launch", "auth-callback"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: callback::command::callback,
    }, crate::CommandSpec {
        path: &["launch", "auth-start"],
        flags: &[],
        value_flags: &["--region", "--aws-mcp-url"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: callback::start::command,
    }, crate::CommandSpec {
        path: &["launch", "auth-wait"],
        flags: &[],
        value_flags: &["--region", "--aws-mcp-url"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: callback::wait::command,
    }, crate::CommandSpec {
        path: &["launch", "download-snapshot"],
        flags: &[],
        value_flags: &["--region", "--output"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: snapshot::download_command,
    }, crate::CommandSpec {
        path: &["launch", "apply-snapshot"],
        flags: &[],
        value_flags: &["--into"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: snapshot::apply_command,
    }],
    synopsis: "  toolkit launch resolve-region [--region REGION] [--aws-mcp-url URL]\n  toolkit launch session-status\n  toolkit launch sign-out\n  toolkit launch create-launch DIR [NAME] [--region REGION]\n  toolkit launch get-launch ID [INCLUDE] [--region REGION]\n  toolkit launch list-launches [--region REGION]\n  toolkit launch delete-launch ID [--region REGION]\n  toolkit launch start-launch-execution ID [--region REGION]\n  toolkit launch get-launch-status ID [--region REGION]\n  toolkit launch get-launch-download-url ID [--region REGION]\n  toolkit launch auth-callback\n  toolkit launch auth-start [--region REGION] [--aws-mcp-url URL]\n  toolkit launch auth-wait PID [--region REGION] [--aws-mcp-url URL]\n  toolkit launch download-snapshot LAUNCH_ID [--output DIR] [--region REGION]\n  toolkit launch apply-snapshot SNAPSHOT_DIR [--into DIR]\n",
    description:
        "  launch resolve-region  Resolve the migration region.\n  launch session-status  Show local session status.\n  launch sign-out  Remove the local session.\n  launch create-launch  Upload a local directory and create a launch.\n  launch get-launch  Show launch details.\n  launch list-launches  List launches.\n  launch delete-launch  Delete a launch.\n  launch start-launch-execution  Start a launch.\n  launch get-launch-status  Show launch progress.\n  launch get-launch-download-url  Show the download URL.\n  launch auth-callback  Internal sign-in helper; reads configuration from stdin.\n  launch auth-start     Start sign-in.\n  launch auth-wait      Wait for a sign-in helper to finish.\n  launch download-snapshot  Download and safely extract a launch's migrated snapshot.\n  launch apply-snapshot     Apply an extracted snapshot to the Git work tree.\n",
    exit_notes: "",
    doc: "docs/migration/launch-with-aws.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};
