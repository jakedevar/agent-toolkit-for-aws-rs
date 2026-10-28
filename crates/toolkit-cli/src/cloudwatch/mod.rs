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

/// Registration of the `cloudwatch-domain` slice: its routes and help text (see `crate::registry`).
pub(crate) const DOMAIN_SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "cloudwatch-domain",
    commands: &[
        crate::CommandSpec {
            path: &["cloudwatch", "render"],
            flags: &[],
            value_flags: ARGUMENT_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: render_command,
        },
        crate::CommandSpec {
            path: &["cloudwatch", "validate"],
            flags: &[],
            value_flags: ARGUMENT_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: validate_command,
        },
        crate::CommandSpec {
            path: &["cloudwatch", "region"],
            flags: &[],
            value_flags: REGION_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: region_command,
        },
    ],
    synopsis: r"  toolkit cloudwatch render RENDERER (--json TEXT | --json-file FILE)
  toolkit cloudwatch validate CHECK (--json TEXT | --json-file FILE)
  toolkit cloudwatch region [--region REGION]
",
    description: r"  cloudwatch render | validate | region
                Offline CloudWatch dynamic-instrumentation logic (#4380): render
                a response or status report, check an input, or resolve the
                region. render RENDERER is one of location, create-success,
                list, get, batch-delete, status-history, status-assessment,
                snapshot-search, snapshot-sample, snapshot-fields, failure;
                validate CHECK is one of instrumentation-type, location-inputs,
                capture-names, probe, location-hash, snapshot-signal. The
                arguments are one JSON object (legacy parameter names) given
                inline with --json or from a file with --json-file. A failed
                check prints the legacy ERROR text on stdout and exits 1; an
                unknown or mistyped argument exits 2. Nothing contacts AWS.
                See docs/migration/cw-domain.md.
",
    exit_notes: "",
    doc: "crates/toolkit-cli/README.md",
    synopsis_rank: 290,
    description_rank: 290,
};

/// Registration of the `cloudwatch-api` slice: its routes and help text (see `crate::registry`).
pub(crate) const API_SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "cloudwatch-api",
    commands: &[
        crate::CommandSpec {
            path: &["cloudwatch", "instrumentation"],
            flags: INSTRUMENTATION_FLAGS,
            value_flags: INSTRUMENTATION_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: instrumentation_command,
        },
        crate::CommandSpec {
            path: &["cloudwatch", "snapshots"],
            flags: SNAPSHOT_FLAGS,
            value_flags: SNAPSHOT_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: snapshots_command,
        },
        crate::CommandSpec {
            path: &["cloudwatch", "evaluate-traces"],
            flags: EVALUATE_FLAGS,
            value_flags: EVALUATE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: evaluate_command,
        },
        crate::CommandSpec {
            path: &["cloudwatch", "capture-dataset"],
            flags: CAPTURE_FLAGS,
            value_flags: CAPTURE_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: capture_command,
        },
    ],
    synopsis: r"  toolkit cloudwatch instrumentation OP (--json TEXT | --json-file FILE) [--recorded FILE]
      [--now ISO] [--i-have-confirmed] [--region REGION] [--profile PROFILE]
  toolkit cloudwatch instrumentation --print-contract
  toolkit cloudwatch snapshots OP (--json TEXT | --json-file FILE) [--out FILE]
      [--recorded FILE] [--region REGION] [--profile PROFILE]
  toolkit cloudwatch snapshots --print-contract
  toolkit cloudwatch evaluate-traces (--trace-id ID | --trace-ids IDS)
      [--evaluator-id ID | --evaluator-ids IDS] [--level TRACE|TOOL_CALL|SESSION]
      [--session-id ID] [--window-days N] [--include-explanations] [--no-writeback]
      [--retention-days N] [--kms-key-id KEY] [--recorded FILE] [--now ISO]
  toolkit cloudwatch capture-dataset --trace-ids IDS [--mode create|add]
      [--dataset-name NAME] [--dataset-id ID] [--description TEXT] [--window-days N]
      [--i-have-confirmed] [--recorded FILE]
",
    description: r"  cloudwatch instrumentation | snapshots | evaluate-traces | capture-dataset
                CloudWatch operations over an injected AWS client (#4381). This
                build has no live AWS client: they send nothing and read no
                credentials. Pass --recorded FILE to replay recorded responses
                (docs/migration/cw-api.md); without it the first AWS call fails
                with 'this build includes no live AWS client'. instrumentation
                OP is create, list, get, delete, batch-delete-by-scope,
                batch-delete-by-arns, get-status or check-status; create, delete
                and the batch deletes change live resources and are refused
                without --i-have-confirmed. snapshots OP is sample or search
                (Logs Insights; --out writes the result owner-only, never
                through a symbolic link). The op arguments are one JSON object
                (--json TEXT or --json-file FILE; stdin is not accepted);
                --print-contract shows the argument schema.
                evaluate-traces scores Omni traces with AgentCore evaluators and
                capture-dataset turns traces into an evaluation dataset
                (--i-have-confirmed required to write); both print one JSON
                receipt. See docs/migration/cw-api.md.
",
    exit_notes: "",
    doc: "crates/toolkit-cli/README.md",
    synopsis_rank: 300,
    description_rank: 300,
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
