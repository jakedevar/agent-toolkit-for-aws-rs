//! MSK sizing, Express compatibility and simulation configuration tools
//! (slice `msk`, #4387).
//!
//! Ports of three legacy Python scripts that ship with the `managing-amazon-msk`
//! and `migrate-to-msk` skills:
//!
//! - [`sizing`], [`pricing`] and [`size_report`]: the broker sizing and monthly
//!   cost calculator (`msk_sizing.py`).
//! - [`compat`]: the five-pillar MSK Express compatibility assessor
//!   (`compatibility.py`).
//! - [`simulation`]: the load-test simulation compute, render and validate
//!   engine (`simulation_load_test_config.py`).
//! - [`pyfmt`]: Python-compatible number, string and JSON rendering so the
//!   customer-facing text matches the legacy output byte for byte.
//!
//! The crate is pure: it does no I/O and has no AWS dependency. The Pricing API
//! sits behind the [`pricing::PricingApi`] trait so a service SDK can be
//! supplied by an optional feature or crate; the `toolkit` binary links none.
//! File access, argument parsing and exit-status mapping live in
//! `toolkit-cli`.
//!
//! The numeric port keeps the legacy operation order, so floating point results
//! are bit-identical. The casts between integer and float types are therefore
//! deliberate and allowed crate-wide.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::too_many_lines
)]

pub mod compat;
pub mod pricing;
pub mod pyfmt;
pub mod simulation;
pub mod size_report;
pub mod sizing;
