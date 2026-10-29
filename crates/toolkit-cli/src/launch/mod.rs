//! Launch with AWS (`skills/core-skills/launch-with-aws/scripts`), ported to
//! Rust per the accepted design `docs/migration/launch-with-aws-design.md`
//! (#4382, revision 3 at `e03640d1`, pre-review #5038).
//!
//! This module holds slices L0-L2 (#5085): configuration and local state
//! ([`config`], [`state`]), the bounded byte collector of #5082 ([`buffer`])
//! and the archive boundary ([`archive`]: rooted race-free source walk, Git
//! ignore rules, the strict ZIP writer and reader, version-control name
//! policy and the materialisation planner). The routes, the OIDC flow, the
//! callback helper and the API client arrive in L3-L5; until L5 registers the
//! slice in `registry.rs` nothing outside the unit tests calls this code, so
//! dead-code analysis is silenced for the module as a whole.
#![allow(dead_code)]

pub mod archive;
pub mod buffer;
pub mod callback;
pub mod clock;
pub mod config;
pub mod http;
pub mod oidc;
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
    /// A command that needs the Unix file-system boundary on another
    /// platform ([`PLATFORM_MESSAGE`]).
    Platform,
}

impl LaunchError {
    /// The message shown to the user.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Config(message) | Self::Archive(message) => message,
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

/// Internal callback route; the remaining launch commands arrive in L5.
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "launch",
    commands: &[crate::CommandSpec {
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
    }],
    synopsis: "  toolkit launch auth-callback\n  toolkit launch auth-start [--region REGION] [--aws-mcp-url URL]\n  toolkit launch auth-wait PID [--region REGION] [--aws-mcp-url URL]\n",
    description:
        "  launch auth-callback  Internal sign-in helper; reads configuration from stdin.\n  launch auth-start     Start sign-in.\n  launch auth-wait      Wait for a sign-in helper to finish.\n",
    exit_notes: "",
    doc: "docs/migration/launch-with-aws-design.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};
