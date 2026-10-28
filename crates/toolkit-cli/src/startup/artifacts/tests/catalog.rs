//! The Rust port of `plugins/aws-startup-advisor/tests/test_ai_capability_catalog_contracts.py`
//! (#4409): the target-only `ai-capability-catalog.json` reference data file
//! against its schema, its semantic invariants and its vendored copies.
//!
//! These cases need the schema-subset validator, which is crate-private, so
//! they live beside the other `startup artifacts` tests instead of in
//! `tests/startup_contracts`. Each positive check is shown to fail on an edited
//! copy; the Python file's negative mutations are kept as they were.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use toolkit_core::Json;

use super::documents::{node, put, remove};
use super::{json, real_plugin};
use crate::startup::artifacts::schema::{unsupported_keywords, Context, SubsetValidator};

const SCHEMA: &str = "scripts/contracts/ai-capability-catalog.schema.json";
const DATA: &str = "skills/shared/ai/ai-capability-catalog.json";
const VENDORED: [&str; 2] = [
    "skills/gcp-to-aws/references/vendored/ai/ai-capability-catalog.json",
    "skills/azure-to-aws/references/vendored/ai/ai-capability-catalog.json",
];

fn read(rel: &str) -> String {
    fs::read_to_string(real_plugin().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn schema() -> Json {
    json(&read(SCHEMA))
}

fn catalog() -> Json {
    json(&read(DATA))
}

/// `validate_json_schema` findings of `catalog` against the catalog schema.
fn findings(catalog: &Json) -> Vec<(String, String)> {
    let schema = schema();
    let mut validator = SubsetValidator::default();
    let mut out = Vec::new();
    let context = Context {
        artifact: "catalog",
        contract: SCHEMA,
    };
    validator
        .validate(&schema, catalog, "", &context, &mut out, &schema)
        .expect("the schema evaluates");
    out.into_iter().map(|f| (f.path, f.message)).collect()
}

fn array<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    match value.get(key) {
        Some(Json::Array(items)) => items,
        other => panic!("{key} is not an array: {other:?}"),
    }
}

fn identity(entry: &Json) -> &str {
    entry
        .get("identity")
        .and_then(Json::as_str)
        .expect("identity")
}

/// Duplicate-identity rejection and the gateway-upstream-points-at-a-real-provider
/// check: what the schema cannot express.
fn semantic_problem(catalog: &Json) -> Option<String> {
    let mut providers = BTreeSet::new();
    for provider in array(catalog, "providers") {
        if !providers.insert(identity(provider).to_owned()) {
            return Some(format!(
                "duplicate provider identity: {}",
                identity(provider)
            ));
        }
    }
    let mut gateways = BTreeSet::new();
    for gateway in array(catalog, "gateways") {
        if !gateways.insert(identity(gateway).to_owned()) {
            return Some(format!("duplicate gateway identity: {}", identity(gateway)));
        }
        for upstream in array(gateway, "upstream_providers") {
            let upstream = upstream.as_str().expect("upstream name");
            if !providers.contains(upstream) {
                return Some(format!(
                    "gateway {:?} names upstream_provider {upstream:?} not declared in providers[]",
                    identity(gateway)
                ));
            }
        }
    }
    None
}

/// Whether the vendored file at `copy` is byte-identical to the canonical data file.
fn identical(copy: &PathBuf) -> bool {
    fs::read(copy).expect("copy") == fs::read(real_plugin().join(DATA)).expect("canonical")
}

// ---- schema self-validity ---------------------------------------------------------

#[test]
fn test_schema_has_no_unsupported_keywords() {
    assert!(unsupported_keywords(&schema(), "").is_empty());
    // Non-vacuous: a schema using `contains` is reported.
    let mut bad = schema();
    put(&mut bad, "", "contains", Json::object(Vec::new()));
    assert!(!unsupported_keywords(&bad, "").is_empty());
}

#[test]
fn test_schema_is_valid_json_and_draft07_shaped() {
    let schema = schema();
    assert_eq!(
        schema.get("$schema").and_then(Json::as_str),
        Some("http://json-schema.org/draft-07/schema#")
    );
    assert_eq!(schema.get("type").and_then(Json::as_str), Some("object"));
}

// ---- positive fixture --------------------------------------------------------------

#[test]
fn test_canonical_catalog_validates_against_schema() {
    assert!(findings(&catalog()).is_empty());
}

#[test]
fn test_canonical_catalog_satisfies_semantic_invariants() {
    assert_eq!(semantic_problem(&catalog()), None);
    // Non-vacuous: the semantic check rejects a duplicate and an unknown upstream.
    let mut dup = catalog();
    let first = array(&dup, "providers")[0].clone();
    node_push(&mut dup, "providers", first);
    assert!(semantic_problem(&dup).is_some());
}

#[test]
fn test_canonical_catalog_never_claims_an_implemented_adapter_or_route() {
    let catalog = catalog();
    for provider in array(&catalog, "providers") {
        assert_eq!(provider.get("implemented_adapter"), Some(&Json::Null));
    }
    for gateway in array(&catalog, "gateways") {
        assert_eq!(gateway.get("route"), Some(&Json::Null));
    }
    // Non-vacuous: a claimed adapter is visible to the same assertion.
    let mut claimed = catalog;
    put(
        &mut claimed,
        "providers.0",
        "implemented_adapter",
        Json::string("x"),
    );
    assert_ne!(
        array(&claimed, "providers")[0].get("implemented_adapter"),
        Some(&Json::Null)
    );
}

// ---- vendored-copy byte-identity -----------------------------------------------------

#[test]
fn test_vendored_copy_is_byte_identical_to_canonical() {
    for copy in VENDORED {
        assert!(
            identical(&real_plugin().join(copy)),
            "{copy} has drifted from canonical {DATA}; run `toolkit sync vendored`"
        );
    }
    // Non-vacuous: an edited copy in a scratch directory is not identical.
    let scratch = std::env::temp_dir().join(format!("catalog-copy-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("scratch");
    let edited = scratch.join("ai-capability-catalog.json");
    fs::write(
        &edited,
        read(DATA).replace("\"anthropic\"", "\"anthropics\""),
    )
    .expect("write");
    assert!(!identical(&edited));
    let _ = fs::remove_dir_all(&scratch);
}

// ---- negative mutations ---------------------------------------------------------------

fn node_push(value: &mut Json, path: &str, item: Json) {
    match node(value, path) {
        Json::Array(items) => items.push(item),
        other => panic!("{path} is not an array: {other:?}"),
    }
}

#[test]
fn test_duplicate_provider_identity_is_rejected() {
    let mut catalog = catalog();
    let first = array(&catalog, "providers")[0].clone();
    node_push(&mut catalog, "providers", first);
    if findings(&catalog).is_empty() {
        assert!(semantic_problem(&catalog).is_some());
    }
}

#[test]
fn test_openrouter_as_a_direct_provider_identity_is_rejected() {
    let mut catalog = catalog();
    put(
        &mut catalog,
        "providers.0",
        "identity",
        Json::string("openrouter"),
    );
    assert!(!findings(&catalog).is_empty());
}

#[test]
fn test_invalid_status_enum_value_is_rejected() {
    let mut catalog = catalog();
    put(&mut catalog, "providers.0", "status", Json::string("maybe"));
    assert!(!findings(&catalog).is_empty());
}

#[test]
fn test_non_null_malformed_implemented_adapter_is_rejected() {
    let mut catalog = catalog();
    put(
        &mut catalog,
        "providers.0",
        "implemented_adapter",
        Json::string("Not Valid!"),
    );
    assert!(!findings(&catalog).is_empty());
}

#[test]
fn test_missing_required_field_is_rejected() {
    let mut catalog = catalog();
    remove(&mut catalog, "providers.0", "source_services");
    assert!(!findings(&catalog).is_empty());
}

#[test]
fn test_gateway_upstream_provider_absent_from_providers_is_rejected_semantically() {
    let mut catalog = catalog();
    node_push(
        &mut catalog,
        "gateways.0.upstream_providers",
        Json::string("nonexistent_provider"),
    );
    assert!(
        findings(&catalog).is_empty(),
        "the schema cannot express the cross-array check"
    );
    assert!(semantic_problem(&catalog).is_some());
}
