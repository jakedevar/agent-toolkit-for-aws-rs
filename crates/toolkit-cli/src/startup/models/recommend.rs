//! The orchestrator of `model_recommendation.py`: dispatches each workload to
//! its provider engine, keeps per-workload catalog provenance and assembles the
//! shared output document.

use toolkit_core::Json;

use super::anthropic;
use super::catalog::Catalog;
use super::findings::{object, optional_text, text};
use super::openai;
use super::request::Input;
use super::{ModelError, Res};
use crate::safe_text::safe_text;

/// Which provider engine owns a source (`_provider_module`). This is a
/// two-way decision, not a per-provider table: `OpenAI` has its own engine and
/// everything else, `none`/`unknown` and the providers with no engine yet
/// (`azure_openai`, `google_genai`, `bedrock`) included, goes to the Anthropic
/// one. That engine classifies the source against its own pool and attaches a
/// `provider_module_pending` finding to anything outside it, so the
/// recommendation is produced but stays provisional. Enumerating the
/// fall-through providers here would duplicate that classification.
pub(super) fn provider_module(provider: &str) -> &'static str {
    if provider == "openai" {
        "openai"
    } else {
        "anthropic"
    }
}

fn provenance(catalog: &Catalog) -> Json {
    object(vec![
        ("provider", text(&catalog.provider)),
        ("verified_at", text(&catalog.verified_at)),
        ("verified_region", text(&catalog.verified_region)),
        ("source", optional_text(catalog.source.as_deref())),
    ])
}

/// Recommends a model and API path for every workload of `input`.
///
/// `openai_catalog` is only called for a workload whose source provider is
/// `OpenAI`, and at most once. Every workload records the provenance of the
/// catalog that produced it, so a mixed-provider run never mislabels a source.
///
/// # Errors
///
/// A duplicate `workload_id`, a `primary_unit` that names no workload, a
/// catalog without a candidate for a workload, or a catalog that cannot be
/// loaded.
pub(crate) fn recommend(
    input: &Input,
    catalog: &Catalog,
    openai_catalog: &mut dyn FnMut() -> Res<Catalog>,
) -> Res<Json> {
    let mut loaded_openai: Option<Catalog> = None;
    let mut workloads: Vec<(String, Json)> = Vec::new();
    let mut provenances: Vec<(String, Json)> = Vec::new();
    for workload in &input.workloads {
        if workloads.iter().any(|(id, _)| *id == workload.id) {
            return Err(ModelError::new(format!(
                "duplicate workload_id: {}",
                safe_text(&workload.id)
            )));
        }
        let (recommendation, used) = if provider_module(&workload.source.provider) == "openai" {
            if loaded_openai.is_none() {
                loaded_openai = Some(openai_catalog()?);
            }
            let used = loaded_openai.as_ref().unwrap_or(catalog);
            (
                openai::recommend_workload(workload, &input.region, used)?,
                used,
            )
        } else {
            (
                anthropic::recommend_workload(workload, &input.region, catalog)?,
                catalog,
            )
        };
        workloads.push((workload.id.clone(), recommendation));
        provenances.push((workload.id.clone(), provenance(used)));
    }
    let Some(primary) = provenances
        .iter()
        .find(|(id, _)| *id == input.primary_unit)
        .map(|(_, value)| value.clone())
    else {
        return Err(ModelError::new(
            "primary_unit does not name a workload_id of the input",
        ));
    };
    // Backward-compatible top-level `catalog`: the primary unit's catalog, plus
    // explicit per-workload provenance so a mixed run keeps correct attribution.
    Ok(object(vec![
        ("schema_version", Json::Int(2)),
        ("catalog", primary),
        ("catalog_provenance", Json::Object(provenances)),
        ("primary_unit", text(&input.primary_unit)),
        ("workloads", Json::Object(workloads)),
    ]))
}
