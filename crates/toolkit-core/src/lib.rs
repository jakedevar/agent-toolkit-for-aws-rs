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
//! - [`paths`]: relative-path normalization, symlink-refusing resolution and
//!   bounded regular-file reads.
//! - [`skills`] / [`frontmatter`]: `SKILL.md` discovery for `toolkit skills list`.
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod frontmatter;
pub mod json;
pub mod paths;
pub mod schema;
pub mod skills;
pub mod yaml;

#[cfg(test)]
mod testutil;

pub use error::{ErrorKind, ParseError, ToolkitError};
pub use frontmatter::parse_frontmatter;
pub use json::{escape_json_string, parse_json, skills_to_json, Json};
pub use paths::{
    normalize_relative, read_regular_file, resolve_within, PathError, MAX_TEXT_FILE_BYTES,
};
pub use schema::{validate, ObjectSchema, Schema, Violation};
pub use skills::{discover_skills, skills_output_schema, SkillEntry, SkillsError};
pub use yaml::parse_yaml;
