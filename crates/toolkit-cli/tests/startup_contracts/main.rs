//! Startup cross-file and workflow contracts (#4409).
//!
//! Rust replacements for the markdown, fixture and cross-file tests in
//! `plugins/aws-startup-advisor/tests/`. Each contract asserts the same
//! property of the committed plugin files and is shown to fail on an edited
//! copy (see `common::check`). The one-to-one mapping from every Python test
//! to its Rust test is in `docs/migration/startup-contracts.md`.

#![allow(
    // Rules and models mirror short Python names (`a`, `b`, `n`, `p`).
    clippy::many_single_char_names,
    clippy::similar_names,
    // Doc comments cite Python identifiers and product names verbatim.
    clippy::doc_markdown
)]

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

mod ai_phases;
mod azure_clarify;
mod common;
mod cost_compare;
mod cost_completeness;
mod credentials;
mod discover;
mod gpt6;
mod gpt6_round4;
mod guides;
mod heroku_addon;
mod heroku_deferred;
mod heroku_pricing;
mod openrouter_byok;
mod option_c;
mod rdfa;
mod rules;
mod run_state;
mod usage_api;
