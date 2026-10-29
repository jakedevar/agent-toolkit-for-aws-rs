//! `PreToolUse` hook that blocks direct Secrets Manager value fetching.
//!
//! Rust port of `plugins/aws-core/com.anthropic.claude-code/hooks/secret-safety.py`
//! (slice #4412). Protocol: the host writes one JSON document to stdin; the hook
//! prints nothing to allow, or a single `hookSpecificOutput` deny object, and
//! exits 0 either way. The hook **fails closed**: input it cannot validate, a
//! panic, a read error or exceeding the time budget all deny.
//!
//! - [`evaluate`]: pure decision over raw input bytes (rules R1-R9).
//! - [`decide_from_reader`] / [`decide_with_deadline`]: the same, bounded by a
//!   size cap and a wall-clock deadline.
//! - [`Decision::render`]: the stdout bytes.
//!
//! The rule inventory, the list of deliberate (stricter) differences from the
//! Python hook and the residual risks are in
//! `docs/migration/security-hooks-review.md`.
#![allow(clippy::module_name_repetitions)]

mod decision;
mod matchers;
mod policy;
mod pyjson;
mod runtime;

pub use decision::{Decision, DenyReason, SECRET_FETCH_MESSAGE};
pub use policy::{evaluate, MAX_INPUT_BYTES};
pub use runtime::{decide_from_reader, decide_with_deadline, DEFAULT_DEADLINE};
