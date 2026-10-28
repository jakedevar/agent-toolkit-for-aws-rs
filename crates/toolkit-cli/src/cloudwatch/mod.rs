//! CloudWatch dynamic-instrumentation domain logic (slice `cw-domain`,
//! Issue #4380).
//!
//! Port of the pure modules of
//! `skills/core-skills/aws-observability/scripts/cloudwatch/`:
//!
//! | Python | Rust |
//! |--------|------|
//! | `di_constants.py` | [`consts`] |
//! | `di_formatting.py` | [`timefmt`] (`format_timestamp`, [`timefmt::UtcTime`]) |
//! | `di_result.py` | [`result`] |
//! | `di_region.py` | [`region`] |
//! | `di_validation.py` | [`validation`] |
//! | `di_location.py` | [`location`] |
//! | `di_capture.py` | [`capture`] |
//! | `di_error_translation.py` | [`errors`] |
//! | `di_snapshot_parsing.py` | [`snapshot_parsing`] |
//! | `di_snapshot_rendering.py` | [`snapshot_rendering`] |
//! | `di_status_assessment.py` | [`status_assessment`] |
//! | `di_status_rendering.py` | [`status_rendering`] |
//! | `di_crud_rendering.py` | [`crud_rendering`] |
//!
//! Nothing here contacts AWS. The live seams - the Application Signals and
//! Logs clients, the gateway, the session and the operation entrypoints
//! (`di_crud_tools`, `di_status_tools`, `di_snapshot_tools`, the two host
//! scripts) - belong to slice `cw-api` (#4381), which calls these modules
//! with injected clients. [`cli`] exposes the pure logic as offline
//! `toolkit cloudwatch ...` commands. The parity table and every intentional
//! change are in `docs/migration/cw-domain.md`.
#![allow(
    clippy::case_sensitive_file_extension_comparisons,
    clippy::format_push_string,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::large_enum_variant,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::unnecessary_wraps
)]

pub mod aws_client;
pub mod capture;
pub mod cli;
pub mod consts;
pub mod crud_rendering;
pub mod dataset;
pub mod errors;
pub mod evaluate;
pub mod gateway;
pub mod location;
pub mod logs_query;
pub mod omni_sql;
pub mod pytext;
pub mod recorded;
pub mod region;
pub mod response_reads;
pub mod response_shape;
pub mod result;
pub mod snapshot_parsing;
pub mod snapshot_rendering;
pub mod status_assessment;
pub mod status_rendering;
pub mod timefmt;
pub mod validation;

mod crud_tools;
mod omni_cli;
mod ops_cli;
mod snapshot_tools;
mod status_tools;

pub use cli::{region_command, render_command, validate_command};
pub use omni_cli::{
    capture_command, evaluate_command, CAPTURE_FLAGS, CAPTURE_VALUE_FLAGS, EVALUATE_FLAGS,
    EVALUATE_VALUE_FLAGS,
};
pub use ops_cli::{
    instrumentation_command, snapshots_command, INSTRUMENTATION_FLAGS, INSTRUMENTATION_VALUE_FLAGS,
    SNAPSHOT_FLAGS, SNAPSHOT_VALUE_FLAGS,
};

/// Value flags shared by `cloudwatch render` and `cloudwatch validate`.
pub const ARGUMENT_VALUE_FLAGS: &[&str] = &["--json", "--json-file"];

/// Value flags of `cloudwatch region`.
pub const REGION_VALUE_FLAGS: &[&str] = &["--region"];

#[cfg(test)]
mod tests_api;
#[cfg(test)]
mod tests_caller_text;
#[cfg(test)]
mod tests_core;
#[cfg(test)]
mod tests_omni;
#[cfg(test)]
mod tests_render;
#[cfg(test)]
mod tests_safety;
