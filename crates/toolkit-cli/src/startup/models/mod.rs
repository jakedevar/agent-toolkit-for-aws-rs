//! Bedrock model/path recommendation of the agent-advisor skill (#4401, slice
//! `startup-models`).
//!
//! Ports the three engines of `plugins/aws-startup-advisor/skills/agent-advisor/scripts`:
//!
//! | Rust | Python original |
//! | --- | --- |
//! | `startup model-recommend` (`recommend.rs`, `command.rs`) | `model_recommendation.py` |
//! | `anthropic.rs` | `anthropic_model_recommendation.py` |
//! | `openai.rs` | `openai_model_recommendation.py` |
//!
//! The orchestrator validates the shared input contract, sends each workload
//! to its provider engine (`OpenAI` has its own, everything else goes to the
//! Anthropic one, which labels providers it cannot reason about
//! `provider_module_pending`), keeps per-workload catalog provenance and checks
//! the shared output contract. The dated catalogs under
//! `references/models/*.json` stay data: nothing about a model is compiled in
//! beyond the selection tables the Python engines also hard-code. Both
//! contracts are the shipped JSON Schemas, evaluated by the shared schema
//! engine; the findings it prints name the field and the rule, never the
//! offending value. Every deliberate difference is in
//! `docs/migration/startup-models.md`.

use std::fmt;

use toolkit_core::ToolkitError;

mod anthropic;
mod catalog;
mod command;
mod findings;
mod openai;
mod recommend;
mod request;

#[cfg(test)]
mod tests;

pub use command::model_recommend_command;

#[cfg(test)]
pub(crate) use catalog::{load_anthropic_catalog, load_openai_catalog};
pub(crate) use recommend::recommend;

/// File name of the Anthropic catalog the engine reads by default
/// (`DEFAULT_CATALOG`).
pub(crate) const DEFAULT_CATALOG: &str = "anthropic-bedrock-2026-09-24.json";

/// File name of the `OpenAI` catalog the engine reads by default
/// (`OPENAI_CATALOG`).
pub(crate) const OPENAI_CATALOG: &str = "openai-bedrock-2026-09-24.json";

/// A recommendation, catalog or input problem. The text names fields, rules and
/// escaped file names, never a caller-supplied value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelError(String);

impl ModelError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ModelError> for ToolkitError {
    fn from(error: ModelError) -> Self {
        Self::runtime("startup_models_invalid", error.0)
    }
}

/// Result of a recommendation step.
pub(crate) type Res<T> = Result<T, ModelError>;

/// Priority-ordered model keys per `priority` (`_PRIORITY_ORDER`), shared with
/// the pool-versus-lifecycle drift tests of the scoring locks (#4612).
#[cfg(test)]
pub(crate) use anthropic::PRIORITY_ORDER;
