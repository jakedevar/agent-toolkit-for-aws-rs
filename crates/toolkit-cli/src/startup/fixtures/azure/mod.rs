//! The azure-to-aws asserters over the `azure-iac-terraform` corpus: Discover,
//! Clarify (three expectation sets), Design and Estimate.
//!
//! These are external oracles. The skill DSL's `_assert` postconditions verify
//! the SHAPE of an artifact, but the model both produces the artifact and
//! evaluates the assertion, so a well-formed artifact can be wrong. Every check
//! pins a fact where a plausible improvisation and the correct answer diverge.

pub(super) mod clarify;
pub(super) mod design;
pub(super) mod discover;
pub(super) mod estimate;
