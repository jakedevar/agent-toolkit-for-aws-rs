//! Offline, deterministic tests of `startup verify-model-path` (#4402): the
//! ported `test_verify_model_path.py` (probe construction, application
//! inference profile checks, structured failures, schema conformance) with the
//! injected [`ProbeClient`] in place of the Python fake clients, plus the
//! redaction, selection, linked-file and hostile-input cases the Python never
//! covered. No test reaches a network, a credential or a model.

#![allow(clippy::too_many_lines)]

use std::fs;

use toolkit_core::{parse_json, Json};

use super::probe::{Operation, ProbeClient, ProbeError, PROMPT};
use super::verify::{format_utc, verify_recommendation, verify_workload};
use super::*;
use crate::startup::scoring::fixtures::{exec, j, path_text, write, TempDir};

const NOW: &str = "2026-07-20T12:00:00+00:00";
const OPUS55: &str = "anthropic.claude-opus-5-5";
const PROFILE_ARN: &str =
    "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-opus-5-5";
const APPLICATION_ARN: &str =
    "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/test-profile";

/// ESC, BEL, CR, LF, U+202E and a secret-looking marker in one value.
const HOSTILE: &str = "x\u{1b}[31m\u{7}\r\nb\u{202e}y token=FAKESECRET4402";

fn json_string(text: &str) -> String {
    Json::string(text).to_compact_string()
}

fn assert_clean(text: &str) {
    for raw in ['\u{1b}', '\u{7}', '\r', '\u{202e}'] {
        assert!(!text.contains(raw), "raw {raw:?} in {text:?}");
    }
    assert!(!text.contains("FAKESECRET4402"), "marker in {text:?}");
}

// ---- recommendation fixtures -------------------------------------------------

struct Spec<'a> {
    id: &'a str,
    status: &'a str,
    path: &'a str,
    invocation: Option<&'a str>,
    path_model: &'a str,
    region: &'a str,
    verification_extra: &'a str,
}

impl<'a> Spec<'a> {
    fn recommended(id: &'a str, path: &'a str, invocation: Option<&'a str>) -> Self {
        Self {
            id,
            status: "recommended",
            path,
            invocation,
            path_model: "anthropic.claude-sonnet-5",
            region: "us-east-1",
            verification_extra: "",
        }
    }

    fn json(&self) -> String {
        let invocation = self
            .invocation
            .map_or_else(|| "null".to_string(), json_string);
        let recommended = self.status == "recommended";
        let (primary, identity, path, options, strategy) = if recommended {
            (
                "\"claude_sonnet_5\"".to_string(),
                format!(
                    r#"{{"model_key":"claude_sonnet_5","display_name":"Claude Sonnet 5","family":"claude",
                       "version":"5","context_window":200000,"output_token_ceiling":"unknown",
                       "path_model_id":{},"requires_cris":false}}"#,
                    json_string(self.path_model)
                ),
                json_string(self.path),
                "[]".to_string(),
                "canary",
            )
        } else {
            let candidate = r#"{"model_key":"k","model":"m","api_path":"runtime_converse",
                "invocation_model_id":null,"requires_cris":false,"reason":"r"}"#;
            (
                "null".to_string(),
                "null".to_string(),
                "null".to_string(),
                format!("[{candidate},{candidate}]"),
                "decision_required",
            )
        };
        let invocation_field = if recommended {
            invocation
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"workload_id":{id},"provider_module":"anthropic","decision_status":"{status}",
              "source":{{"provider":"anthropic","model_ids":["claude-3-7-sonnet-latest"],"sdk":"anthropic",
                         "api_surface":"messages","source_paths":["src/agent.py"]}},
              "source_analysis":{{"detected_version":null,"target_version":null,"version_changed":null}},
              "feature_assessment":{{}},"primary_model":{primary},"model_identity":{identity},
              "api_path":{path},"invocation_model_id":{invocation_field},"decision_options":{options},
              "alternatives":[],"rationale":["because"],"blocks":[],"tuning":[],
              "compatibility":{{"native":[],"portable":[],"rearchitecture":[],"unsupported":[]}},
              "architecture_impacts":[],"migration_deltas":[],
              "evaluation":{{"mode":"prompt","gates":["quality"]}},
              "rollout":{{"strategy":"{strategy}","gate":"g"}},
              "verification":{{"region":{region},"catalog_verified_at":"2026-07-01","verified_at":null,
                "probe_status":"not_run","availability_claim":"provisional",
                "invocation_model_id":{invocation_field},"required_checks":["probe"]{extra}}}}}"#,
            id = json_string(self.id),
            status = self.status,
            region = json_string(self.region),
            extra = self.verification_extra,
        )
    }
}

/// A schema-valid recommendation document holding `workloads`.
fn recommendation_text(workloads: &[Spec<'_>]) -> String {
    let members: Vec<String> = workloads
        .iter()
        .map(|spec| format!("{}:{}", json_string(spec.id), spec.json()))
        .collect();
    let provenance = r#"{"provider":"anthropic","verified_at":"2026-07-01","verified_region":"us-east-1","source":null}"#;
    format!(
        r#"{{"schema_version":2,"catalog":{provenance},"catalog_provenance":{{"anthropic":{provenance}}},
           "primary_unit":{},"workloads":{{{}}}}}"#,
        json_string(workloads[0].id),
        members.join(",")
    )
}

fn recommendation(workloads: &[Spec<'_>]) -> Json {
    let document = j(&recommendation_text(workloads));
    check_valid(RECOMMENDATION_SCHEMA, &document);
    document
}

fn check_valid(schema: &str, document: &Json) {
    let compiled = engine::compile(j(schema)).expect("the shipped schema compiles");
    let findings = compiled.validate_redacted(document).expect("evaluates");
    let rendered: Vec<String> = findings
        .iter()
        .map(|f| finding_path(&f.path) + &f.message)
        .collect();
    assert!(findings.is_empty(), "{rendered:?}");
}

fn application_spec(profile: &str) -> Spec<'_> {
    Spec {
        path_model: OPUS55,
        verification_extra: r#","allowed_inference_profiles":["us.anthropic.claude-opus-5-5"]"#,
        ..Spec::recommended("w", "runtime_converse", Some(profile))
    }
}

// ---- injected client ---------------------------------------------------------------

type Handler = Box<dyn FnMut(Operation, &Json) -> Result<Json, ProbeError>>;

struct Scripted {
    calls: Vec<(Operation, String, Json)>,
    handler: Handler,
}

impl Scripted {
    fn new(handler: impl FnMut(Operation, &Json) -> Result<Json, ProbeError> + 'static) -> Self {
        Self {
            calls: Vec::new(),
            handler: Box::new(handler),
        }
    }

    /// Every operation succeeds with `{"model": <model>}`.
    fn echoing(model: &'static str) -> Self {
        Self::new(move |_, _| Ok(j(&format!(r#"{{"model":"{model}"}}"#))))
    }

    fn operations(&self) -> Vec<Operation> {
        self.calls
            .iter()
            .map(|(operation, _, _)| *operation)
            .collect()
    }

    fn inference_calls(&self) -> usize {
        self.calls
            .iter()
            .filter(|(operation, _, _)| *operation != Operation::GetInferenceProfile)
            .count()
    }
}

impl ProbeClient for Scripted {
    fn call(
        &mut self,
        operation: Operation,
        region: &str,
        request: &Json,
    ) -> Result<Json, ProbeError> {
        self.calls
            .push((operation, region.to_string(), request.clone()));
        (self.handler)(operation, request)
    }
}

fn profile_metadata(regions: &[&str], model: &str, status: &str) -> Json {
    let models: Vec<String> = regions
        .iter()
        .map(|region| {
            format!(r#"{{"modelArn":"arn:aws:bedrock:{region}::foundation-model/{model}"}}"#)
        })
        .collect();
    j(&format!(
        r#"{{"status":"{status}","models":[{}]}}"#,
        models.join(",")
    ))
}

fn default_metadata() -> Json {
    profile_metadata(&["us-east-1", "us-west-2"], OPUS55, "ACTIVE")
}

fn verify(specs: &[Spec<'_>], client: &mut Scripted) -> Json {
    verify_recommendation(&recommendation(specs), &[], client, Some(NOW)).expect("verifies")
}

fn first_workload(result: &Json) -> &Json {
    match result.get("workloads") {
        Some(Json::Object(members)) => &members[0].1,
        other => panic!("no workloads: {other:?}"),
    }
}

fn status(result: &Json) -> &str {
    first_workload(result)
        .get("status")
        .and_then(Json::as_str)
        .expect("status")
}

fn error_of(result: &Json) -> (String, String) {
    let error = first_workload(result).get("error").expect("error");
    let text = |key: &str| {
        error
            .get(key)
            .and_then(Json::as_str)
            .expect(key)
            .to_string()
    };
    (text("type"), text("message"))
}

// ---- probes --------------------------------------------------------------------------

#[test]
fn the_mantle_probe_uses_the_recommended_clean_id() {
    let mut client = Scripted::echoing("anthropic.claude-sonnet-5");
    let result = verify(
        &[Spec::recommended(
            "support-agent",
            "mantle_messages",
            Some("anthropic.claude-sonnet-5"),
        )],
        &mut client,
    );
    let workload = first_workload(&result);
    assert_eq!(status(&result), "passed");
    assert_eq!(
        workload.get("response_model_id"),
        Some(&Json::string("anthropic.claude-sonnet-5"))
    );
    assert_eq!(workload.get("checked_at"), Some(&Json::string(NOW)));
    let (operation, region, request) = &client.calls[0];
    assert_eq!(
        (*operation, region.as_str()),
        (Operation::MantleMessages, "us-east-1")
    );
    assert_eq!(
        *request,
        j(&format!(
            r#"{{"model":"anthropic.claude-sonnet-5","max_tokens":8,
                "messages":[{{"role":"user","content":"{PROMPT}"}}]}}"#
        ))
    );
    assert_eq!(client.calls.len(), 1);
}

#[test]
fn the_converse_probe_uses_the_resolved_cris_id_without_substitution() {
    let mut client = Scripted::new(|_, request| {
        Ok(j(&format!(
            r#"{{"modelId":{}}}"#,
            request.get("modelId").expect("modelId").to_compact_string()
        )))
    });
    let id = "global.anthropic.claude-sonnet-5";
    let result = verify(
        &[Spec::recommended("w", "runtime_converse", Some(id))],
        &mut client,
    );
    assert_eq!(status(&result), "passed");
    assert_eq!(
        first_workload(&result).get("response_model_id"),
        Some(&Json::string(id))
    );
    assert_eq!(client.operations(), [Operation::Converse]);
    assert_eq!(
        client.calls[0].2,
        j(&format!(
            r#"{{"modelId":"{id}","messages":[{{"role":"user","content":[{{"text":"{PROMPT}"}}]}}],
                "inferenceConfig":{{"maxTokens":8}}}}"#
        ))
    );
}

#[test]
fn the_invoke_probe_uses_the_native_bedrock_body() {
    let mut client = Scripted::new(|_, _| Ok(j("{}")));
    let id = "global.anthropic.claude-sonnet-5";
    let result = verify(
        &[Spec::recommended("w", "runtime_invoke", Some(id))],
        &mut client,
    );
    assert_eq!(status(&result), "passed");
    assert_eq!(
        first_workload(&result).get("response_model_id"),
        Some(&Json::Null)
    );
    let request = &client.calls[0].2;
    assert_eq!(request.get("modelId"), Some(&Json::string(id)));
    assert_eq!(
        request.get("contentType"),
        Some(&Json::string("application/json"))
    );
    assert_eq!(
        request.get("accept"),
        Some(&Json::string("application/json"))
    );
    let body =
        parse_json(request.get("body").and_then(Json::as_str).expect("body")).expect("body JSON");
    assert_eq!(
        body.get("anthropic_version"),
        Some(&Json::string("bedrock-2023-05-31"))
    );
    assert_eq!(body.get("max_tokens"), Some(&Json::Int(8)));
    assert_eq!(
        body.get("messages"),
        Some(&j(&format!(r#"[{{"role":"user","content":"{PROMPT}"}}]"#)))
    );
}

#[test]
fn an_unresolved_runtime_id_creates_no_client_call() {
    let mut client = Scripted::new(|_, _| panic!("no call is expected"));
    let result = verify(
        &[Spec::recommended("w", "runtime_converse", None)],
        &mut client,
    );
    assert_eq!(status(&result), "needs_resolution");
    let workload = first_workload(&result);
    assert_eq!(workload.get("invocation_model_id"), Some(&Json::Null));
    assert_eq!(workload.get("checked_at"), Some(&Json::string(NOW)));
    let (kind, message) = error_of(&result);
    assert_eq!(kind, "UnresolvedInvocationModelId");
    assert!(
        message.contains("will not substitute a model ID"),
        "{message}"
    );
    assert!(client.calls.is_empty());
}

#[test]
fn a_probe_failure_is_structured() {
    let mut client = Scripted::new(|_, _| Err(ProbeError::new("PermissionError", "denied")));
    let result = verify(
        &[Spec::recommended(
            "w",
            "mantle_messages",
            Some("anthropic.claude-sonnet-5"),
        )],
        &mut client,
    );
    assert_eq!(status(&result), "failed");
    assert_eq!(
        error_of(&result),
        ("PermissionError".to_string(), "denied".to_string())
    );
    assert_eq!(
        first_workload(&result).get("response_model_id"),
        Some(&Json::Null)
    );
}

#[test]
fn a_decision_required_workload_is_not_probed() {
    let mut client = Scripted::new(|_, _| panic!("no call is expected"));
    let spec = Spec {
        status: "decision_required",
        ..Spec::recommended("w", "runtime_converse", None)
    };
    let result = verify(&[spec], &mut client);
    assert_eq!(status(&result), "not_applicable");
    assert_eq!(first_workload(&result).get("checked_at"), Some(&Json::Null));
    assert_eq!(first_workload(&result).get("api_path"), Some(&Json::Null));
    assert!(client.calls.is_empty());
}

#[test]
fn the_verification_output_matches_its_schema() {
    let mut client = Scripted::echoing("anthropic.claude-sonnet-5");
    let result = verify(
        &[
            Spec::recommended(
                "passed",
                "mantle_messages",
                Some("anthropic.claude-sonnet-5"),
            ),
            Spec::recommended("open", "runtime_converse", None),
            Spec {
                status: "decision_required",
                ..Spec::recommended("choice", "runtime_converse", None)
            },
        ],
        &mut client,
    );
    check_valid(VERIFICATION_SCHEMA, &result);
    assert_eq!(result.get("schema_version"), Some(&Json::Int(1)));
    assert_eq!(
        result.get("recommendation_schema_version"),
        Some(&Json::Int(2))
    );
    assert_eq!(result.get("generated_at"), Some(&Json::string(NOW)));
    let Some(Json::Object(workloads)) = result.get("workloads") else {
        panic!("workloads")
    };
    let order: Vec<&str> = workloads.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        order,
        ["passed", "open", "choice"],
        "recommendation order is kept"
    );
}

// ---- OpenAI paths -----------------------------------------------------------------------

#[test]
fn the_openai_responses_probe_calls_the_exact_model() {
    let mut client = Scripted::echoing("openai.gpt-5.6-sol");
    let result = verify(
        &[Spec::recommended(
            "openai-svc",
            "mantle_openai_responses",
            Some("openai.gpt-5.6-sol"),
        )],
        &mut client,
    );
    assert_eq!(status(&result), "passed");
    assert_eq!(
        first_workload(&result).get("response_model_id"),
        Some(&Json::string("openai.gpt-5.6-sol"))
    );
    assert_eq!(client.operations(), [Operation::OpenAiResponses]);
    assert_eq!(
        client.calls[0].2,
        j(&format!(
            r#"{{"model":"openai.gpt-5.6-sol","input":"{PROMPT}"}}"#
        ))
    );
}

#[test]
fn the_openai_chat_probe_calls_the_exact_model() {
    let mut client = Scripted::echoing("openai.gpt-6-sol");
    let result = verify(
        &[Spec::recommended(
            "openai-svc",
            "mantle_openai_chat",
            Some("openai.gpt-6-sol"),
        )],
        &mut client,
    );
    check_valid(VERIFICATION_SCHEMA, &result);
    assert_eq!(status(&result), "passed");
    assert_eq!(
        first_workload(&result).get("response_model_id"),
        Some(&Json::string("openai.gpt-6-sol"))
    );
    assert_eq!(
        client.calls[0].2,
        j(&format!(
            r#"{{"model":"openai.gpt-6-sol","max_completion_tokens":8,
                "messages":[{{"role":"user","content":"{PROMPT}"}}]}}"#
        ))
    );
}

#[test]
fn openai_failures_never_substitute_a_model() {
    for (path, id, operation) in [
        (
            "mantle_openai_responses",
            "openai.gpt-5.6-sol",
            Operation::OpenAiResponses,
        ),
        (
            "mantle_openai_chat",
            "openai.gpt-6-sol",
            Operation::OpenAiChat,
        ),
    ] {
        let mut client =
            Scripted::new(|_, _| Err(ProbeError::new("RuntimeError", "access denied")));
        let result = verify(&[Spec::recommended("w", path, Some(id))], &mut client);
        assert_eq!(status(&result), "failed", "{path}");
        assert_eq!(
            first_workload(&result).get("invocation_model_id"),
            Some(&Json::string(id))
        );
        assert_eq!(
            client.operations(),
            [operation],
            "exactly the selected model is probed"
        );
        assert_eq!(client.calls[0].2.get("model"), Some(&Json::string(id)));
    }
}

#[test]
fn system_profile_ids_and_arns_reach_the_probe_unchanged() {
    for path in ["runtime_converse", "runtime_invoke"] {
        for profile in [format!("us.{OPUS55}"), PROFILE_ARN.to_string()] {
            let mut client = Scripted::new(|_, _| Ok(j("{}")));
            let spec = Spec {
                path_model: OPUS55,
                ..Spec::recommended("w", path, Some(&profile))
            };
            let result = verify(&[spec], &mut client);
            assert_eq!(status(&result), "passed", "{path} {profile}");
            assert_eq!(
                client.operations().len(),
                1,
                "no profile lookup for a system profile"
            );
            assert_eq!(
                client.calls[0].2.get("modelId"),
                Some(&Json::string(&profile))
            );
        }
    }
}

// ---- application inference profiles ---------------------------------------------------

/// Control plane answers `app` for the application ARN and a fixed map for the
/// reference profiles; runtime calls succeed.
fn profile_client(app: Json, reference: Result<Json, ProbeError>) -> Scripted {
    Scripted::new(move |operation, request| {
        if operation != Operation::GetInferenceProfile {
            return Ok(j("{}"));
        }
        let id = request
            .get("inferenceProfileIdentifier")
            .and_then(Json::as_str)
            .expect("identifier");
        if id.contains(":application-inference-profile/") {
            Ok(app.clone())
        } else {
            reference.clone()
        }
    })
}

#[test]
fn application_profile_metadata_is_verified_before_the_arn_is_invoked() {
    for path in ["runtime_converse", "runtime_invoke"] {
        let mut client = profile_client(default_metadata(), Ok(default_metadata()));
        let spec = Spec {
            path,
            ..application_spec(APPLICATION_ARN)
        };
        let result = verify(&[spec], &mut client);
        assert_eq!(status(&result), "passed", "{path}");
        let identifiers: Vec<&str> = client
            .calls
            .iter()
            .filter(|(operation, _, _)| *operation == Operation::GetInferenceProfile)
            .map(|(_, _, request)| {
                request
                    .get("inferenceProfileIdentifier")
                    .and_then(Json::as_str)
                    .expect("id")
            })
            .collect();
        assert_eq!(
            identifiers,
            [APPLICATION_ARN, "us.anthropic.claude-opus-5-5"]
        );
        assert_eq!(
            client.operations(),
            [
                Operation::GetInferenceProfile,
                Operation::GetInferenceProfile,
                if path == "runtime_invoke" {
                    Operation::InvokeModel
                } else {
                    Operation::Converse
                }
            ],
            "the lookups come first"
        );
        assert_eq!(
            client.calls[2].2.get("modelId"),
            Some(&Json::string(APPLICATION_ARN))
        );
    }
}

#[test]
fn a_wrong_model_or_wrong_destinations_fail_before_inference() {
    for (label, metadata, message) in [
        (
            "wrong model",
            profile_metadata(
                &["us-east-1", "us-west-2"],
                "anthropic.claude-sonnet-5",
                "ACTIVE",
            ),
            "Inference profile does not match the selected model",
        ),
        (
            "wrong destinations",
            profile_metadata(&["us-east-1", "eu-west-1"], OPUS55, "ACTIVE"),
            "Application profile destinations do not match a verified allowed inference profile",
        ),
        (
            "no destinations",
            profile_metadata(&[], OPUS55, "ACTIVE"),
            "Inference profile has no active model destinations",
        ),
        (
            "inactive",
            profile_metadata(&["us-east-1", "us-west-2"], OPUS55, "INACTIVE"),
            "Inference profile has no active model destinations",
        ),
    ] {
        let mut client = profile_client(metadata, Ok(default_metadata()));
        let result = verify(&[application_spec(APPLICATION_ARN)], &mut client);
        assert_eq!(status(&result), "failed", "{label}");
        assert_eq!(
            error_of(&result),
            ("ValueError".to_string(), message.to_string()),
            "{label}"
        );
        assert_eq!(client.inference_calls(), 0, "{label}");
    }
}

#[test]
fn a_denied_profile_lookup_fails_without_inference() {
    let mut client = Scripted::new(|_, _| {
        Err(ProbeError::new(
            "PermissionError",
            "GetInferenceProfile denied",
        ))
    });
    let result = verify(&[application_spec(APPLICATION_ARN)], &mut client);
    assert_eq!(status(&result), "failed");
    assert_eq!(error_of(&result).0, "PermissionError");
    assert_eq!(client.operations(), [Operation::GetInferenceProfile]);
}

#[test]
fn an_application_profile_without_constraints_is_not_invoked() {
    let mut client = Scripted::new(|_, _| panic!("no call is expected"));
    let spec = Spec {
        verification_extra: "",
        ..application_spec(APPLICATION_ARN)
    };
    let result = verify(&[spec], &mut client);
    assert_eq!(status(&result), "failed");
    assert_eq!(
        error_of(&result),
        (
            "ValueError".to_string(),
            "Application profile requires verified model and residency constraints".to_string()
        )
    );
    assert!(client.calls.is_empty());
}

#[test]
fn an_unreadable_reference_does_not_block_a_later_allowed_one() {
    let mut client = Scripted::new(|operation, request| {
        if operation != Operation::GetInferenceProfile {
            return Ok(j("{}"));
        }
        match request
            .get("inferenceProfileIdentifier")
            .and_then(Json::as_str)
        {
            Some("us.anthropic.claude-opus-5-5") => Err(ProbeError::new(
                "PermissionError",
                "US reference not readable",
            )),
            _ => Ok(profile_metadata(
                &["us-east-1", "eu-west-1"],
                OPUS55,
                "ACTIVE",
            )),
        }
    });
    let spec = Spec {
        verification_extra: r#","allowed_inference_profiles":["us.anthropic.claude-opus-5-5","global.anthropic.claude-opus-5-5"]"#,
        ..application_spec(APPLICATION_ARN)
    };
    let result = verify(&[spec], &mut client);
    assert_eq!(status(&result), "passed");
    assert_eq!(client.inference_calls(), 1);
}

#[test]
fn the_london_in_region_allowance_accepts_one_matching_destination() {
    let arn = APPLICATION_ARN.replace("us-east-1", "eu-west-2");
    for regions in [&["eu-west-2"][..], &["eu-west-2", "eu-west-1"][..]] {
        let regions: Vec<&str> = regions.to_vec();
        let app = profile_metadata(&regions, OPUS55, "ACTIVE");
        let mut client = profile_client(
            app,
            Ok(profile_metadata(
                &["eu-west-2", "eu-west-1"],
                OPUS55,
                "ACTIVE",
            )),
        );
        let spec = Spec {
            region: "eu-west-2",
            verification_extra: r#","allowed_inference_profiles":["eu.anthropic.claude-opus-5-5"],
                "allowed_in_region_model_arns":["arn:aws:bedrock:eu-west-2::foundation-model/anthropic.claude-opus-5-5"]"#,
            ..application_spec(&arn)
        };
        let result = verify(&[spec], &mut client);
        assert_eq!(status(&result), "passed", "{regions:?}");
        assert!(client
            .calls
            .iter()
            .all(|(_, region, _)| region == "eu-west-2"));
    }
}

#[test]
fn the_london_in_region_allowance_rejects_wrong_or_additional_destinations() {
    let arn = APPLICATION_ARN.replace("us-east-1", "eu-west-2");
    for metadata in [
        profile_metadata(&["eu-west-1"], OPUS55, "ACTIVE"),
        profile_metadata(&["eu-west-2"], "anthropic.claude-sonnet-5", "ACTIVE"),
        profile_metadata(&["eu-west-2"], OPUS55, "INACTIVE"),
        profile_metadata(&["eu-west-2", "us-east-1"], OPUS55, "ACTIVE"),
    ] {
        let mut client = profile_client(
            metadata,
            Ok(profile_metadata(
                &["eu-west-2", "eu-west-1"],
                OPUS55,
                "ACTIVE",
            )),
        );
        let spec = Spec {
            region: "eu-west-2",
            verification_extra: r#","allowed_inference_profiles":["eu.anthropic.claude-opus-5-5"],
                "allowed_in_region_model_arns":["arn:aws:bedrock:eu-west-2::foundation-model/anthropic.claude-opus-5-5"]"#,
            ..application_spec(&arn)
        };
        let result = verify(&[spec], &mut client);
        assert_eq!(status(&result), "failed");
        assert_eq!(client.inference_calls(), 0);
    }
}

#[test]
fn malformed_profile_metadata_is_a_structured_failure() {
    for (metadata, kind) in [
        (j("[]"), "TypeError"),
        (j(r#"{"status":"ACTIVE","models":[{}]}"#), "KeyError"),
        (j(r#"{"status":"ACTIVE","models":"x"}"#), "TypeError"),
    ] {
        let mut client = profile_client(metadata, Ok(default_metadata()));
        let result = verify(&[application_spec(APPLICATION_ARN)], &mut client);
        assert_eq!(status(&result), "failed");
        assert_eq!(error_of(&result).0, kind);
        assert_eq!(client.inference_calls(), 0);
    }
}

#[test]
fn only_the_opus_application_profile_is_looked_up() {
    let mut client = Scripted::new(|_, _| Ok(j("{}")));
    let spec = Spec {
        path_model: "anthropic.claude-sonnet-5",
        ..application_spec(APPLICATION_ARN)
    };
    let result = verify(&[spec], &mut client);
    assert_eq!(status(&result), "passed");
    assert_eq!(client.operations(), [Operation::Converse]);
}

#[test]
fn an_unknown_api_path_is_a_failure_that_names_no_input_value() {
    let recommendation = recommendation(&[Spec::recommended("w", "runtime_converse", Some("m"))]);
    let Some(Json::Object(workloads)) = recommendation.get("workloads") else {
        panic!("workloads")
    };
    let mut broken = workloads[0].1.clone();
    let Json::Object(members) = &mut broken else {
        panic!("object")
    };
    for (key, value) in members.iter_mut() {
        if key == "api_path" {
            *value = Json::string(HOSTILE);
        }
    }
    let mut client = Scripted::new(|_, _| panic!("no call is expected"));
    let result = verify_workload("w", &broken, &mut client, NOW);
    let error = result.get("error").expect("error");
    assert_eq!(result.get("status"), Some(&Json::string("failed")));
    let message = error
        .get("message")
        .and_then(Json::as_str)
        .expect("message");
    assert!(
        message.starts_with("verification is not implemented for API path: x\\x1b[31m"),
        "{message}"
    );
    assert_clean(message);
}

// ---- selection, clock -------------------------------------------------------------------------

#[test]
fn selected_workloads_are_probed_in_recommendation_order() {
    let specs = [
        Spec::recommended("a", "mantle_messages", Some("anthropic.claude-sonnet-5")),
        Spec::recommended("b", "mantle_messages", Some("anthropic.claude-sonnet-5")),
        Spec::recommended("c", "mantle_messages", Some("anthropic.claude-sonnet-5")),
    ];
    let document = recommendation(&specs);
    let mut client = Scripted::echoing("anthropic.claude-sonnet-5");
    let selected = ["c".to_string(), "a".to_string()];
    let result =
        verify_recommendation(&document, &selected, &mut client, Some(NOW)).expect("verifies");
    let Some(Json::Object(workloads)) = result.get("workloads") else {
        panic!("workloads")
    };
    assert_eq!(
        workloads
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["a", "c"]
    );
    assert_eq!(client.calls.len(), 2);
}

#[test]
fn an_unknown_workload_is_reported_by_count_not_by_value() {
    let specs = [Spec::recommended(
        HOSTILE,
        "mantle_messages",
        Some("anthropic.claude-sonnet-5"),
    )];
    let document = recommendation(&specs);
    let mut client = Scripted::new(|_, _| panic!("no call is expected"));
    let error = verify_recommendation(
        &document,
        &["nope-FAKESECRET4402".to_string()],
        &mut client,
        Some(NOW),
    )
    .expect_err("unknown workload");
    let message = error.to_string();
    assert!(
        message.starts_with(
            "workload not found in recommendation: 1 selected workload id(s) are unknown;"
        ),
        "{message}"
    );
    assert!(!message.contains("nope"), "{message}");
    assert_clean(&message);
    // Known ids are listed escaped, and since #4941 the shared helper redacts
    // the labelled credential before it bounds and escapes the id.
    assert!(
        message.contains("x\\x1b[31m\\x07\\x0d\\x0ab\\u202ey token=[REDACTED]"),
        "known ids are listed redacted and escaped: {message}"
    );
    assert!(!message.contains("FAKESECRET4402"), "{message}");
    assert!(!message.contains('\n'));
}

#[test]
fn the_clock_text_is_python_isoformat() {
    assert_eq!(format_utc(0, 0), "1970-01-01T00:00:00+00:00");
    assert_eq!(format_utc(951_782_400, 0), "2000-02-29T00:00:00+00:00");
    assert_eq!(format_utc(1_000_000_000, 0), "2001-09-09T01:46:40+00:00");
    assert_eq!(
        format_utc(1_784_548_800, 123_456),
        "2026-07-20T12:00:00.123456+00:00"
    );
    assert_eq!(
        format_utc(1_709_251_199, 5),
        "2024-02-29T23:59:59.000005+00:00"
    );
    assert_eq!(format_utc(-1, 0), "1969-12-31T23:59:59+00:00");
    assert!(verify::utc_now().ends_with("+00:00"));
}

// ---- redaction (AC3) --------------------------------------------------------------------------

#[test]
fn probe_errors_are_redacted_escaped_and_bounded_before_they_are_stored() {
    let long = "z".repeat(2_000);
    let message = format!(
        "POST https://bedrock-mantle.us-east-1.api.aws/openai/v1?api_key=FAKESECRET4402 failed: \
         Authorization: Bearer FAKESECRET4402\ttoken=FAKESECRET4402 {HOSTILE} {long}"
    );
    let kind = format!("Err\u{1b}[0m{HOSTILE}");
    let mut client = Scripted::new(move |_, _| Err(ProbeError::new(kind.clone(), message.clone())));
    let result = verify(
        &[Spec::recommended(
            "w",
            "mantle_openai_chat",
            Some("openai.gpt-6-sol"),
        )],
        &mut client,
    );
    check_valid(VERIFICATION_SCHEMA, &result);
    let (kind, message) = error_of(&result);
    for text in [&kind, &message] {
        assert_clean(text);
        assert!(!text.contains('\n') && !text.contains('\t'));
    }
    assert!(message.contains("[REDACTED]"), "{message}");
    assert!(
        message.contains("\\x0d\\x0a") && message.contains("\\u202e"),
        "{message}"
    );
    assert!(message.ends_with("..."), "bounded: {}", message.len());
    assert!(message.chars().count() <= 520);
    let rendered = json_dumps(&result);
    assert_clean(&rendered);
    assert!(rendered.is_ascii());
}

#[test]
fn the_default_client_sends_nothing_and_says_so() {
    let mut client = NoLiveClient;
    for operation in [
        Operation::MantleMessages,
        Operation::OpenAiResponses,
        Operation::OpenAiChat,
        Operation::Converse,
        Operation::InvokeModel,
        Operation::GetInferenceProfile,
    ] {
        let error = client
            .call(operation, "us-east-1", &j("{}"))
            .expect_err("no live client");
        assert_eq!(error.kind, "RuntimeError");
        assert!(
            error.message.contains("requires a live Bedrock client"),
            "{error}"
        );
    }
}

// ---- the command ---------------------------------------------------------------------------------

fn recommended_text() -> String {
    recommendation_text(&[Spec::recommended(
        "support-agent",
        "mantle_messages",
        Some("anthropic.claude-sonnet-5"),
    )])
}

fn run(values: &[&str]) -> (u8, String, String) {
    exec(values)
}

#[test]
fn the_command_stores_a_no_live_client_failure_and_exits_two() {
    let dir = TempDir::new("verify-nolive");
    write(&dir, "model-recommendation.json", &recommended_text());
    let source = path_text(&dir.join("model-recommendation.json")).to_string();
    let (status, stdout, stderr) = run(&["startup", "verify-model-path", &source]);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (2, "RESULT=failed WORKLOADS=1\n", "")
    );
    let written =
        fs::read_to_string(dir.join("model-verification.json")).expect("written beside the source");
    assert!(written.ends_with("}\n"));
    let result = j(&written);
    check_valid(VERIFICATION_SCHEMA, &result);
    assert_eq!(status_of(&result), "failed");
    let (kind, message) = error_of(&result);
    assert_eq!(kind, "RuntimeError");
    assert!(
        message.contains("requires a live Bedrock client"),
        "{message}"
    );
    assert!(
        written.contains("\n  \"workloads\": {\n    \"support-agent\": {\n      \"workload_id\""),
        "indent 2: {written}"
    );
}

fn status_of(result: &Json) -> &str {
    status(result)
}

#[test]
fn the_command_exits_zero_when_nothing_failed_and_honours_output_and_workload() {
    let dir = TempDir::new("verify-output");
    let text = recommendation_text(&[
        Spec {
            status: "decision_required",
            ..Spec::recommended("choice", "runtime_converse", None)
        },
        Spec::recommended("live", "mantle_messages", Some("anthropic.claude-sonnet-5")),
    ]);
    write(&dir, "r.json", &text);
    let source = path_text(&dir.join("r.json")).to_string();
    let out = path_text(&dir.join("custom.json")).to_string();
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        &source,
        "--workload",
        "choice",
        "--output",
        &out,
    ]);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (0, "RESULT=ok WORKLOADS=1\n", "")
    );
    assert!(!dir.join("model-verification.json").exists());
    let result = j(&fs::read_to_string(dir.join("custom.json")).expect("custom output"));
    assert_eq!(status_of(&result), "not_applicable");
    // comma-separated selection, and the router's last-flag-wins rule
    let (status, stdout, _) = run(&[
        "startup",
        "verify-model-path",
        &source,
        "--workload",
        "nope",
        "--workload=choice,live",
        "--output",
        &out,
    ]);
    assert_eq!(
        (status, stdout.as_str()),
        (2, "RESULT=failed WORKLOADS=2\n")
    );
    // the whole value names a workload even if it holds a comma
    let comma = recommendation_text(&[Spec {
        status: "decision_required",
        ..Spec::recommended("a,b", "runtime_converse", None)
    }]);
    write(&dir, "comma.json", &comma);
    let (status, stdout, _) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("comma.json")),
        "--workload",
        "a,b",
        "--output",
        &out,
    ]);
    assert_eq!((status, stdout.as_str()), (0, "RESULT=ok WORKLOADS=1\n"));
}

#[test]
fn the_command_needs_a_recommendation() {
    let (status, stdout, stderr) = run(&["startup", "verify-model-path"]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(
        stderr.contains("usage: toolkit startup verify-model-path RECOMMENDATION.json"),
        "{stderr}"
    );
}

#[test]
fn an_unknown_workload_is_exit_one_and_writes_nothing() {
    let dir = TempDir::new("verify-unknown");
    write(&dir, "r.json", &recommended_text());
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("r.json")),
        "--workload",
        "ghost",
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(
        stderr.starts_with(
            "error: workload not found in recommendation: 1 selected workload id(s) are unknown;"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("ghost"), "{stderr}");
    assert!(!dir.join("model-verification.json").exists());
}

#[test]
fn unreadable_and_malformed_recommendations_are_one_line_errors() {
    let dir = TempDir::new("verify-errors");
    write(&dir, "bad.json", "{\"workloads\": ");
    for (name, expected) in [
        ("missing.json", "file does not exist"),
        ("bad.json", "invalid JSON"),
    ] {
        let (status, stdout, stderr) =
            run(&["startup", "verify-model-path", path_text(&dir.join(name))]);
        assert_eq!((status, stdout.as_str()), (1, ""), "{name}");
        assert!(
            stderr.starts_with("error: ") && stderr.contains(expected),
            "{stderr}"
        );
        assert_eq!(stderr.trim_end().lines().count(), 1);
    }
}

#[test]
fn a_recommendation_that_breaks_the_schema_lists_findings_without_values() {
    let dir = TempDir::new("verify-schema");
    let broken = recommended_text()
        .replace(
            r#""decision_status":"recommended""#,
            r#""decision_status":"FAKESECRET4402""#,
        )
        .replacen(
            r#""schema_version":2"#,
            r#""schema_version":2,"extra":true"#,
            1,
        );
    write(&dir, "r.json", &broken);
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("r.json")),
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(
        stderr.contains("  $: Additional properties are not allowed ('extra' was unexpected)"),
        "{stderr}"
    );
    assert!(
        stderr
            .contains("$.workloads.support-agent.decision_status: a string value is not one of ["),
        "{stderr}"
    );
    assert!(
        stderr.contains("does not match the model-recommendation schema ("),
        "{stderr}"
    );
    assert!(
        stderr
            .trim_end()
            .lines()
            .last()
            .expect("summary")
            .starts_with("error: the recommendation does not match"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("FAKESECRET4402"),
        "no value is repeated: {stderr}"
    );
    assert!(!dir.join("model-verification.json").exists());
}

#[test]
fn many_schema_findings_are_counted_after_twenty() {
    let dir = TempDir::new("verify-many");
    let specs: Vec<Spec<'static>> = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y",
    ]
    .iter()
    .map(|id| Spec::recommended(id, "mantle_messages", Some("anthropic.claude-sonnet-5")))
    .collect();
    let broken =
        recommendation_text(&specs).replace(r#""alternatives":[]"#, r#""alternatives":[],"bad":1"#);
    write(&dir, "r.json", &broken);
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("r.json")),
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_eq!(
        stderr
            .matches("Additional properties are not allowed")
            .count(),
        20,
        "{stderr}"
    );
    assert!(stderr.contains("  ... and 5 more\n"), "{stderr}");
    assert!(stderr.contains("(25 finding(s))"), "{stderr}");
}

#[test]
fn schema_findings_escape_hostile_keys_and_bound_every_line() {
    let dir = TempDir::new("verify-hostile");
    // thousands of unexpected hostile keys make one finding: the line is cut
    let extras: Vec<String> = (0..300)
        .map(|n| format!("{}:{n}", json_string(&format!("k{n}{HOSTILE}"))))
        .collect();
    let broken = recommended_text().replacen(
        r#""schema_version":2"#,
        &format!(r#""schema_version":2,{}"#, extras.join(",")),
        1,
    );
    write(&dir, "r.json", &broken);
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("r.json")),
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_clean(&stderr);
    assert!(
        stderr.contains("\\x1b[31m\\x07\\x0d\\x0ab\\u202ey token=[REDACTED]"),
        "{stderr}"
    );
    assert!(
        stderr.lines().all(|line| line.chars().count() <= 410),
        "{stderr}"
    );
    // a hostile workload id is a path segment of a finding
    let keyed = recommendation_text(&[Spec::recommended(
        HOSTILE,
        "mantle_messages",
        Some("anthropic.claude-sonnet-5"),
    )])
    .replacen(
        r#""source_paths":["src/agent.py"]"#,
        r#""source_paths":[5]"#,
        1,
    );
    write(&dir, "k.json", &keyed);
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("k.json")),
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_clean(&stderr);
    assert!(
        stderr.contains("$.workloads.x\\x1b[31m\\x07\\x0d\\x0ab\\u202ey token=[REDACTED]"),
        "{stderr}"
    );
}

#[test]
fn hostile_workload_ids_and_file_names_stay_escaped_in_every_report() {
    let dir = TempDir::new("verify-report");
    let name = "r\u{1b}[31m\u{7}\r\nx\u{202e}.json";
    write(
        &dir,
        name,
        &recommendation_text(&[Spec::recommended(
            HOSTILE,
            "mantle_messages",
            Some("anthropic.claude-sonnet-5"),
        )]),
    );
    let source = path_text(&dir.join(name)).to_string();
    let out = dir.join("out.json");
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        &source,
        "--output",
        path_text(&out),
    ]);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (2, "RESULT=failed WORKLOADS=1\n", "")
    );
    // The stored workload id is the recommendation's own key: JSON-escaped, ASCII, one line per field.
    let written = fs::read_to_string(&out).expect("output");
    assert!(
        written
            .chars()
            .all(|c| c.is_ascii() && (c == '\n' || !c.is_control())),
        "{written:?}"
    );
    let reread = j(&written);
    let Some(Json::Object(workloads)) = reread.get("workloads") else {
        panic!("workloads")
    };
    assert_eq!(
        workloads[0].0, HOSTILE,
        "the id round-trips through the JSON escapes"
    );
    // an unreadable hostile source name
    let missing = dir.join("gone\u{1b}\r\n.json");
    let (status, stdout, stderr) = run(&["startup", "verify-model-path", path_text(&missing)]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_clean(&stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr:?}");
    // an unwritable hostile output name
    let nowhere = dir.join("no\u{1b}\r\ndir").join("out.json");
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        &source,
        "--output",
        path_text(&nowhere),
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_clean(&stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr:?}");
    assert!(stderr.contains("cannot write"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn linked_inputs_and_outputs_are_refused() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("verify-links");
    write(&dir, "real.json", &recommended_text());
    symlink(dir.join("real.json"), dir.join("linked.json")).expect("link");
    let (status, stdout, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("linked.json")),
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
    // the default output is a link to another file: nothing is written through it
    let target = dir.join("elsewhere.json");
    fs::write(&target, "keep").expect("target");
    symlink(&target, dir.join("model-verification.json")).expect("link");
    let (status, _, stderr) = run(&[
        "startup",
        "verify-model-path",
        path_text(&dir.join("real.json")),
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("cannot write") && stderr.contains("symbolic link"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&target).expect("target"), "keep");
}

#[test]
fn the_shipped_schemas_compile_and_describe_the_documents_the_tests_build() {
    for schema in [RECOMMENDATION_SCHEMA, VERIFICATION_SCHEMA] {
        engine::compile(j(schema)).expect("compiles offline");
    }
    let verification = j(r#"{"schema_version":1,"recommendation_schema_version":2,
        "generated_at":"2026-07-20T12:00:00+00:00","workloads":{}}"#);
    let compiled = engine::compile(j(VERIFICATION_SCHEMA)).expect("compiles");
    let findings = compiled
        .validate_redacted(&verification)
        .expect("evaluates");
    assert_eq!(
        findings.len(),
        1,
        "an empty workload map is rejected (minProperties)"
    );
}
