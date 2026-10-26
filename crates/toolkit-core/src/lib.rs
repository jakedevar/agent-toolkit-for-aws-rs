//! Shared library for the Agent Toolkit Rust CLI.
//!
//! Bootstrap slice (#4373): skill-directory discovery, frontmatter reading and
//! JSON rendering used by `toolkit skills list`. No Python, network or service
//! runtime lives here.

pub mod frontmatter;
pub mod json;
pub mod skills;

pub use frontmatter::parse_frontmatter;
pub use json::{escape_json_string, skills_to_json};
pub use skills::{discover_skills, SkillEntry, SkillsError};
