//! Shared library for the Agent Toolkit Rust CLI.
//!
//! Contracts every domain slice builds on (#4374), with no Python, network or
//! cloud-service dependency (service SDKs belong in optional domain crates or
//! features, never here):
//!
//! - [`error`]: one error type with stable codes and exit-status mapping.
//! - [`json`]: typed [`Json`] values, a strict parser and a deterministic writer.
//! - [`yaml`]: a strict, documented YAML subset parsed into [`Json`].
//! - [`schema`]: a small structural schema and path-addressed validation.
//! - [`diag`]: [`quote_untrusted`], the one escaping helper for untrusted text in
//!   diagnostics (#4639).
//! - [`digest`] / [`encoding`]: SHA-256 (raw and hex) and base64url.
//! - [`paths`]: relative-path normalization, symlink-refusing resolution and
//!   bounded regular-file reads.
//! - [`skills`] / [`frontmatter`]: `SKILL.md` discovery for `toolkit skills list`.
//! - [`treesync`]: byte-exact, symlink-refusing tree comparison, atomic copy
//!   and replacement shared by the synchronization commands (#4377), with
//!   [`plugin_skills`] (canonical skills to plugin bundles) and [`vendored`]
//!   (`skills/shared` to per-skill vendored copies) on top.
#![allow(clippy::module_name_repetitions)]

pub mod diag;
pub mod digest;
pub mod encoding;
pub mod error;
pub mod frontmatter;
pub mod json;
pub mod paths;
pub mod plugin_skills;
pub mod schema;
pub mod skills;
pub mod treesync;
pub mod vendored;
pub mod yaml;

#[cfg(test)]
mod testutil;

pub use diag::{
    escape_untrusted, is_unsafe_char, neutralize_line, neutralize_text, neutralize_text_knowing,
    quote_untrusted, quote_untrusted_bounded,
};
pub use digest::{sha256, sha256_hex};
pub use encoding::base64url_nopad;
pub use error::{ErrorKind, ParseError, ToolkitError};
pub use frontmatter::parse_frontmatter;
pub use json::{
    escape_json_string, parse_json, parse_json_with_limits, skills_to_json, Json, JsonLimits,
};
pub use paths::{
    normalize_relative, open_regular_file, read_regular_file, resolve_within, set_path_display,
    write_regular_file, PathError, MAX_TEXT_FILE_BYTES,
};
pub use plugin_skills::{
    build_canonical_index, sync_plugin_skills, PluginSkillsOptions, PluginSkillsReport,
};
pub use schema::{validate, ObjectSchema, Schema, Violation};
pub use skills::{discover_skills, skills_output_schema, SkillEntry, SkillsError};
pub use vendored::{sync_vendored, VendoredReport, TELEMETRY_BUNDLE_DIR};
pub use yaml::parse_yaml;
