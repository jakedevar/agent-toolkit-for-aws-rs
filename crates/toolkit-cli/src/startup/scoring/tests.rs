//! Offline, deterministic tests of the scoring engine and its commands
//! (#4400): the ported `test_scoring.py` and the scoring half of
//! `test_collapse_invariant.py` / `test_unit_grouping.py`, plus the negative
//! cases the Python never covered (malformed registry, linked files, hostile
//! names and identifiers).

#![allow(clippy::too_many_lines)]

use std::fs;

use toolkit_core::Json;

use super::dumps::dumps_indent2;
use super::engine::{
    collect_assumptions, collect_warnings, compute_scores, defaults, determine_verdict,
    evaluate_hard_constraints, select_agentcore_services, select_deployment_model,
};
use super::evidence::{
    is_run_materialized_evidence, RUN_EVIDENCE_ARTIFACT_TYPE, RUN_EVIDENCE_SCHEMA_VERSION,
};
use super::fixtures::{
    exec, j, minimal, path_text, profile_text, real_runtimes, real_skill, with, write, TempDir,
};
use super::profiles::{profile_id, GA_ONLY};
use super::units::{load_run_evidence, score_units, RUN_EVIDENCE_FILENAME};
use super::value::OrderedMap;
use super::{legal_values, load_profiles, score, DIMENSIONS, NEUTRAL_SCORE};
use crate::spec::engine as schema_engine;

const AWS_DOCS: &str = "https://docs.aws.amazon.com/lambda/latest/dg/configuration-timeout.html";
const AGENTCORE_SESSION: &str =
    "https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/agents-tools-runtime.html";
const AGENTCORE_GPU: &str = "https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime-instances-how-it-works.html";
const AGENTCORE_COMPUTE: &str =
    "https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime-how-it-works.html";
const MICROVMS: &str = "https://docs.aws.amazon.com/lambda/latest/dg/";

fn evidence(records: &str, run_id: &str) -> Json {
    j(&format!(
        r#"{{"artifact_type":"{RUN_EVIDENCE_ARTIFACT_TYPE}","schema_version":{RUN_EVIDENCE_SCHEMA_VERSION},"run_id":"{run_id}","verifications":{records}}}"#
    ))
}

fn run_score(answers: &str, profiles: &[Json], evidence: Option<&Json>) -> Json {
    let input = j(&format!(
        r#"{{"entry_point":"build_scratch","answers":{answers}}}"#
    ));
    score(&input, profiles, evidence).expect("the answers score")
}

fn field<'a>(result: &'a Json, key: &str) -> &'a Json {
    result
        .get(key)
        .unwrap_or_else(|| panic!("the result lacks {key}"))
}

fn real_profiles() -> Vec<Json> {
    load_profiles(&real_runtimes(), &GA_ONLY).expect("the real registry loads")
}

fn answers_of(text: &str) -> super::engine::Answers {
    let mut merged = defaults();
    let Json::Object(members) = j(text) else {
        panic!("answers are an object");
    };
    for (name, value) in members {
        merged.insert(name, value);
    }
    merged
}

fn gated(
    id: &str,
    field_name: &str,
    value: &str,
    reason: &str,
    key: &str,
    expected: &str,
    source: &str,
) -> Json {
    with(
        minimal(id, "ga"),
        "hard_constraints",
        &format!(
            r#"[{{"field":"{field_name}","value":"{value}","reason":"{reason}","verification_required":true,"verification_key":"{key}","verification_expected_value":"{expected}","verification_sources":["{source}"]}}]"#
        ),
    )
}

fn session_cap() -> Json {
    gated(
        "agentcore",
        "session_duration",
        "over_8hr",
        "8hr cap",
        "agentcore.session_cap",
        "8h",
        AWS_DOCS,
    )
}

fn deferred_session_cap() -> Json {
    j(&format!(
        r#"[{{"runtime":"agentcore","field":"session_duration","value":"over_8hr","reason":"8hr cap","verification_key":"agentcore.session_cap","verification_expected_value":"8h","verification_sources":["{AWS_DOCS}"]}}]"#
    ))
}

// ---- registry loading ----------------------------------------------------------------------

fn write_profile(dir: &std::path::Path, profile: &Json) {
    write(
        dir,
        &format!("{}.json", profile_id(profile)),
        &profile_text(profile),
    );
}

#[test]
fn load_profiles_filters_by_status_and_sorts() {
    let dir = TempDir::new("load");
    write_profile(&dir, &minimal("ecs", "ga"));
    write_profile(&dir, &minimal("agentcore", "ga"));
    write_profile(&dir, &minimal("preview_rt", "preview"));
    write(&dir, "notes.txt", "not a profile");

    let profiles = load_profiles(&dir, &GA_ONLY).expect("loads");

    let ids: Vec<&str> = profiles.iter().map(profile_id).collect();
    assert_eq!(ids, ["agentcore", "ecs"]);
    let all = load_profiles(&dir, &["ga", "preview"]).expect("loads");
    assert_eq!(all.len(), 3);
}

#[test]
fn load_profiles_rejects_bad_json_naming_the_file() {
    let dir = TempDir::new("badjson");
    write(&dir, "broken.json", "{not json");
    let error = load_profiles(&dir, &GA_ONLY).expect_err("invalid JSON");
    let text = error.to_string();
    assert!(text.contains("broken.json"), "{text}");
    assert!(text.contains("invalid JSON"), "{text}");
}

#[test]
fn load_profiles_rejects_missing_keys_naming_them() {
    let dir = TempDir::new("missing");
    write(&dir, "x.json", r#"{"id": "x", "status": "ga"}"#);
    let text = load_profiles(&dir, &GA_ONLY)
        .expect_err("missing keys")
        .to_string();
    assert!(text.contains("x.json"), "{text}");
    assert!(
        text.contains("missing required keys ['affinities', 'hard_constraints']"),
        "{text}"
    );
}

#[test]
fn load_profiles_rejects_shapes_python_would_crash_on() {
    let dir = TempDir::new("shapes");
    write(&dir, "list.json", "[1, 2]");
    let text = load_profiles(&dir, &GA_ONLY)
        .expect_err("array")
        .to_string();
    assert!(text.contains("must be a JSON object"), "{text}");

    let dir = TempDir::new("numeric-id");
    write(
        &dir,
        "n.json",
        r#"{"id": 5, "status": "ga", "affinities": {}, "hard_constraints": []}"#,
    );
    let text = load_profiles(&dir, &GA_ONLY)
        .expect_err("numeric id")
        .to_string();
    assert!(text.contains("\"id\" must be a string"), "{text}");
}

#[test]
fn load_profiles_reports_a_missing_directory_and_a_directory_named_like_a_profile() {
    let dir = TempDir::new("nodir");
    let text = load_profiles(&dir.join("absent"), &GA_ONLY)
        .expect_err("missing directory")
        .to_string();
    assert!(text.contains("cannot read runtime profiles"), "{text}");

    fs::create_dir_all(dir.join("folder.json")).expect("fixture dir");
    assert!(load_profiles(&dir, &GA_ONLY).is_err());
}

#[cfg(unix)]
#[test]
fn load_profiles_refuses_a_linked_profile() {
    let dir = TempDir::new("link");
    let outside = TempDir::new("link-target");
    write_profile(&outside, &minimal("evil", "ga"));
    std::os::unix::fs::symlink(outside.join("evil.json"), dir.join("evil.json"))
        .expect("create link");
    let text = load_profiles(&dir, &GA_ONLY)
        .expect_err("link refused")
        .to_string();
    assert!(text.contains("symbolic links are not followed"), "{text}");
}

#[test]
fn hostile_profile_file_names_are_escaped_in_errors() {
    let dir = TempDir::new("hostile");
    write(&dir, "a\u{1b}[2J\nFORGED.json", "{not json");
    let text = load_profiles(&dir, &GA_ONLY)
        .expect_err("invalid JSON")
        .to_string();
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains('\n'), "{text:?}");
    assert!(text.contains("\\u{001b}"), "{text}");
}

// ---- hard constraints and run evidence -------------------------------------------------------

fn eliminations(answers: &str, profiles: &[Json]) -> Json {
    let outcome =
        evaluate_hard_constraints(&answers_of(answers), profiles, &Json::Object(Vec::new()))
            .expect("constraints evaluate");
    Json::Object(
        outcome
            .eliminated
            .iter()
            .map(|(id, reason)| (id.to_string(), reason.clone()))
            .collect(),
    )
}

fn hard(id: &str, field_name: &str, value: &str, reason: &str) -> Json {
    with(
        minimal(id, "ga"),
        "hard_constraints",
        &format!(r#"[{{"field":"{field_name}","value":"{value}","reason":"{reason}"}}]"#),
    )
}

#[test]
fn hard_constraint_scalar_match() {
    let profiles = [
        hard("agentcore", "session_duration", "over_8hr", "8hr cap"),
        minimal("ecs", "ga"),
    ];
    assert_eq!(
        eliminations(r#"{"session_duration":"over_8hr"}"#, &profiles),
        j(r#"{"agentcore":"8hr cap"}"#)
    );
}

#[test]
fn hard_constraint_compliance_list_match() {
    let profiles = [hard("agentcore", "compliance", "fedramp", "not FedRAMP")];
    assert_eq!(
        eliminations(r#"{"compliance":["soc2","fedramp"]}"#, &profiles),
        j(r#"{"agentcore":"not FedRAMP"}"#)
    );
    // The default compliance answer is ["none"].
    let none = [hard("agentcore", "compliance", "none", "needs a framework")];
    assert_eq!(
        eliminations("{}", &none),
        j(r#"{"agentcore":"needs a framework"}"#)
    );
}

#[test]
fn hard_constraint_no_match() {
    let profiles = [hard("agentcore", "session_duration", "over_8hr", "8hr cap")];
    assert_eq!(
        eliminations(
            r#"{"session_duration":"15min_to_8hr","compliance":["none"]}"#,
            &profiles
        ),
        j("{}")
    );
}

#[test]
fn only_the_first_matching_constraint_of_a_profile_counts() {
    let profile = with(
        minimal("agentcore", "ga"),
        "hard_constraints",
        r#"[{"field":"session_duration","value":"over_8hr","reason":"first"},
            {"field":"traffic_pattern","value":"idle","reason":"second"}]"#,
    );
    assert_eq!(
        eliminations(
            r#"{"session_duration":"over_8hr","traffic_pattern":"idle"}"#,
            &[profile]
        ),
        j(r#"{"agentcore":"first"}"#)
    );
}

#[test]
fn malformed_constraints_are_reported_without_echoing_values() {
    let no_reason = with(
        minimal("agentcore", "ga"),
        "hard_constraints",
        r#"[{"field":"session_duration","value":"SECRET_SENTINEL"}]"#,
    );
    let input = j(r#"{"answers":{"session_duration":"SECRET_SENTINEL"}}"#);
    let text = score(&input, &[no_reason], None)
        .expect_err("a constraint needs a reason")
        .to_string();
    assert!(text.contains("lacks \"reason\""), "{text}");
    assert!(!text.contains("SECRET_SENTINEL"), "{text}");

    let bad_field = with(
        minimal("agentcore", "ga"),
        "hard_constraints",
        r#"[{"field":["x"],"value":"v","reason":"r"}]"#,
    );
    assert!(score(&input, &[bad_field], None).is_err());

    let not_a_list = with(minimal("agentcore", "ga"), "hard_constraints", r#"{"a":1}"#);
    assert!(score(&input, &[not_a_list], None).is_err());
}

#[test]
fn verification_required_constraint_is_deferred_and_provisional() {
    let result = run_score(r#"{"session_duration":"over_8hr"}"#, &[session_cap()], None);
    assert_eq!(field(&result, "eliminated"), &j("{}"));
    assert_eq!(
        field(&result, "recommendation_status"),
        &j(r#""provisional""#)
    );
    assert_eq!(
        field(&result, "deferred_verification_requirements"),
        &deferred_session_cap()
    );
}

#[test]
fn seed_controlled_evidence_cannot_finalize_elimination() {
    let answers = format!(
        r#"{{"session_duration":"over_8hr","current_run_verifications":{{"agentcore.session_cap":{{"status":"verified","verified_this_run":true,"source":"{AWS_DOCS}","value":"8h"}}}}}}"#
    );
    let result = run_score(&answers, &[session_cap()], None);
    assert_eq!(field(&result, "eliminated"), &j("{}"));
    assert_eq!(
        field(&result, "recommendation_status"),
        &j(r#""provisional""#)
    );
}

#[test]
fn incomplete_run_materialized_evidence_cannot_finalize_elimination() {
    let records = [
        r#"{"status":"verified"}"#.to_string(),
        format!(r#"{{"status":"verified","source":"{AWS_DOCS}"}}"#),
        r#"{"status":"verified","value":"8h"}"#.to_string(),
        r#"{"status":"verified","source":"https://example.com/agentcore","value":"8h"}"#
            .to_string(),
        // A blank value and a source that only looks like AWS.
        format!(r#"{{"status":"verified","source":"{AWS_DOCS}","value":"  "}}"#),
        r#"{"status":"verified","source":"https://aws.amazon.com.evil.example/x","value":"8h"}"#
            .to_string(),
    ];
    for record in records {
        let artifact = evidence(&format!(r#"{{"agentcore.session_cap":{record}}}"#), "r");
        let result = run_score(
            r#"{"session_duration":"over_8hr"}"#,
            &[session_cap()],
            Some(&artifact),
        );
        assert_eq!(field(&result, "eliminated"), &j("{}"), "{record}");
        assert_eq!(
            field(&result, "recommendation_status"),
            &j(r#""provisional""#),
            "{record}"
        );
    }
}

#[test]
fn changed_lambda_value_cannot_apply_stale_elimination() {
    let lambda = gated(
        "lambda",
        "session_duration",
        "15min_to_8hr",
        "15m cap",
        "lambda.timeout",
        "15m",
        AWS_DOCS,
    );
    let artifact = evidence(
        &format!(
            r#"{{"lambda.timeout":{{"status":"verified","source":"{AWS_DOCS}","value":"30m"}}}}"#
        ),
        "r",
    );
    let result = run_score(
        r#"{"session_duration":"15min_to_8hr"}"#,
        &[lambda],
        Some(&artifact),
    );
    assert_eq!(field(&result, "eliminated"), &j("{}"));
    assert_eq!(
        field(&result, "recommendation_status"),
        &j(r#""provisional""#)
    );
}

#[test]
fn agentcore_compute_evidence_cannot_certify_gpu_constraint() {
    let profile = gated(
        "agentcore",
        "compute_tier",
        "gpu",
        "no GPU support",
        "agentcore.gpu_support",
        "unsupported",
        AGENTCORE_GPU,
    );
    let artifact = evidence(
        &format!(
            r#"{{"agentcore.gpu_support":{{"status":"verified","source":"{AGENTCORE_COMPUTE}","value":"unsupported"}}}}"#
        ),
        "r",
    );
    let result = run_score(r#"{"compute_tier":"gpu"}"#, &[profile], Some(&artifact));
    assert_eq!(field(&result, "eliminated"), &j("{}"));
    assert_eq!(
        field(&result, "recommendation_status"),
        &j(r#""provisional""#)
    );
}

#[test]
fn source_backed_matching_run_materialized_evidence_finalizes_constraint() {
    let artifact = evidence(
        &format!(
            r#"{{"agentcore.session_cap":{{"status":"verified","source":"{AWS_DOCS}","value":"8h"}}}}"#
        ),
        "r",
    );
    let result = run_score(
        r#"{"session_duration":"over_8hr"}"#,
        &[session_cap()],
        Some(&artifact),
    );
    assert_eq!(
        field(&result, "eliminated"),
        &j(r#"{"agentcore":"8hr cap"}"#)
    );
    assert_eq!(
        field(&result, "deferred_verification_requirements"),
        &j("[]")
    );
    assert_eq!(field(&result, "recommendation_status"), &j(r#""final""#));
}

#[test]
fn an_invalid_evidence_artifact_is_ignored_by_scoring() {
    // Wrong artifact type: the engine treats it as absent evidence.
    let artifact = j(&format!(
        r#"{{"artifact_type":"other","schema_version":1,"run_id":"r","verifications":{{"agentcore.session_cap":{{"status":"verified","source":"{AWS_DOCS}","value":"8h"}}}}}}"#
    ));
    let result = run_score(
        r#"{"session_duration":"over_8hr"}"#,
        &[session_cap()],
        Some(&artifact),
    );
    assert_eq!(field(&result, "eliminated"), &j("{}"));
}

#[test]
fn the_evidence_artifact_contract_is_exact() {
    let ok = evidence(r#"{"a":{"status":"not_verified"}}"#, "run");
    assert!(is_run_materialized_evidence(&ok));
    for bad in [
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"run"}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":2,"run_id":"run","verifications":{}}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"  ","verifications":{}}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"run","verifications":[]}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"run","verifications":{"a":{"status":"maybe"}}}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"run","verifications":{"a":{"status":"failed","extra":1}}}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"run","verifications":{"a":{"status":["verified"]}}}"#,
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":1,"run_id":"run","verifications":{"a":{"status":"verified","source":"s","value":" "}}}"#,
        r"[]",
    ] {
        assert!(!is_run_materialized_evidence(&j(bad)), "{bad}");
    }
    // Python: `True == 1`, so a boolean schema_version passes.
    let truthy = j(
        r#"{"artifact_type":"agent-advisor.current-run-verifications","schema_version":true,"run_id":"run","verifications":{}}"#,
    );
    assert!(is_run_materialized_evidence(&truthy));
}

// ---- scores, verdict, deployment model, services ----------------------------------------------

#[test]
fn compute_scores_uses_affinity_and_neutral_default() {
    let agentcore = with(
        minimal("agentcore", "ga"),
        "affinities",
        r#"{"session_duration":{"15min_to_8hr":5},"traffic_pattern":{"bursty":5}}"#,
    );
    let ecs = with(
        minimal("ecs", "ga"),
        "affinities",
        r#"{"session_duration":{"15min_to_8hr":3}}"#,
    );
    let answers = answers_of(r#"{"session_duration":"15min_to_8hr","traffic_pattern":"bursty"}"#);
    let scores =
        compute_scores(&answers, &[agentcore, ecs], &OrderedMap::default()).expect("scores");
    let total = |id: &str| -> i64 {
        let (_, score) = scores.iter().find(|(name, _)| *name == id).expect("scored");
        let Json::Int(points) = score.to_json() else {
            panic!("integer score");
        };
        points
    };
    let rest = i64::try_from(DIMENSIONS.len()).expect("small") - 2;
    assert_eq!(total("agentcore"), 5 + 5 + NEUTRAL_SCORE * rest);
    assert_eq!(total("ecs"), 3 + NEUTRAL_SCORE * (rest + 1));
}

#[test]
fn compute_scores_omits_eliminated_and_rejects_bad_affinities() {
    let mut eliminated = OrderedMap::default();
    eliminated.set("agentcore", j(r#""x""#));
    let scores =
        compute_scores(&defaults(), &[minimal("agentcore", "ga")], &eliminated).expect("scores");
    assert_eq!(scores.iter().count(), 0);

    for bad in [
        r#"{"session_duration":{"unknown":"five"}}"#,
        r#"{"session_duration":[1]}"#,
    ] {
        let profile = with(minimal("agentcore", "ga"), "affinities", bad);
        assert!(
            compute_scores(&defaults(), &[profile], &OrderedMap::default()).is_err(),
            "{bad}"
        );
    }
    let not_object = with(minimal("agentcore", "ga"), "affinities", "[]");
    assert!(compute_scores(&defaults(), &[not_object], &OrderedMap::default()).is_err());
}

#[test]
fn defaults_cover_all_dimensions() {
    let answers = defaults();
    for dimension in DIMENSIONS {
        assert!(answers.contains_key(dimension), "{dimension}");
    }
    assert_eq!(answers.get("compliance"), Some(&j(r#"["none"]"#)));
    assert_eq!(answers.get("region"), Some(&j(r#""unknown""#)));
}

fn scores_of(pairs: &[(&str, i64)]) -> OrderedMap<super::value::Num> {
    let mut scores = OrderedMap::default();
    for (id, points) in pairs {
        scores.set(id, super::value::Num::Int(*points));
    }
    scores
}

#[test]
fn verdict_single_winner() {
    let (verdict, co) = determine_verdict(
        &scores_of(&[("agentcore", 30), ("ecs", 20)]),
        &OrderedMap::default(),
    )
    .expect("verdict");
    assert_eq!(verdict, "agentcore");
    assert!(co.is_empty());
}

#[test]
fn verdict_co_recommend_within_threshold() {
    let (verdict, co) = determine_verdict(
        &scores_of(&[("ecs", 30), ("eks", 29), ("lambda", 10)]),
        &OrderedMap::default(),
    )
    .expect("verdict");
    assert_eq!(verdict, "co_recommend");
    assert_eq!(co, ["ecs", "eks"]);

    // Exactly the threshold apart is still a tie; one more point is not.
    let (verdict, co) =
        determine_verdict(&scores_of(&[("b", 28), ("a", 30)]), &OrderedMap::default())
            .expect("verdict");
    assert_eq!(
        (verdict.as_str(), co),
        ("co_recommend", vec!["a".to_string(), "b".to_string()])
    );
    let (verdict, _) =
        determine_verdict(&scores_of(&[("b", 27), ("a", 30)]), &OrderedMap::default())
            .expect("verdict");
    assert_eq!(verdict, "a");
}

#[test]
fn verdict_no_viable_runtime() {
    let mut eliminated = OrderedMap::default();
    eliminated.set("agentcore", j(r#""x""#));
    eliminated.set("lambda", j(r#""y""#));
    let (verdict, co) = determine_verdict(&OrderedMap::default(), &eliminated).expect("verdict");
    assert_eq!(verdict, "no_viable_runtime");
    assert!(co.is_empty());
}

fn with_models() -> Json {
    with(
        minimal("agentcore", "ga"),
        "deployment_models",
        r#"["harness","framework_on_runtime"]"#,
    )
}

fn model(answers: &str, verdict: &str, profiles: &[Json]) -> Option<&'static str> {
    select_deployment_model(&answers_of(answers), verdict, profiles).expect("selects")
}

#[test]
fn deployment_model_follows_the_answers() {
    let ecs = [minimal("ecs", "ga")];
    assert_eq!(model("{}", "ecs", &ecs), None);
    assert_eq!(model("{}", "absent", &ecs), None);

    let profiles = [with_models()];
    assert_eq!(
        model(
            r#"{"multi_agent":"yes","framework":"none"}"#,
            "agentcore",
            &profiles
        ),
        Some("framework_on_runtime")
    );
    assert_eq!(
        model(
            r#"{"multi_agent":"no","framework":"none"}"#,
            "agentcore",
            &profiles
        ),
        Some("harness")
    );
    for framework in ["langgraph", "crewai", "custom"] {
        assert_eq!(
            model(
                &format!(r#"{{"multi_agent":"no","framework":"{framework}"}}"#),
                "agentcore",
                &profiles
            ),
            Some("framework_on_runtime"),
            "{framework}"
        );
    }
}

#[test]
fn explicit_deployment_preference_overrides_the_inference() {
    let profiles = [with_models()];
    assert_eq!(
        model(
            r#"{"multi_agent":"yes","framework":"langgraph","deployment_preference":"harness"}"#,
            "agentcore",
            &profiles
        ),
        Some("harness")
    );
    assert_eq!(
        model(
            r#"{"multi_agent":"no","framework":"none","deployment_preference":"framework"}"#,
            "agentcore",
            &profiles
        ),
        Some("framework_on_runtime")
    );
    assert_eq!(
        model(
            r#"{"multi_agent":"yes","deployment_preference":"either"}"#,
            "agentcore",
            &profiles
        ),
        Some("framework_on_runtime")
    );
}

#[test]
fn services_always_on_baseline_and_add_ons() {
    assert_eq!(
        select_agentcore_services(&answers_of("{}")),
        ["identity", "observability", "evaluations", "optimization"]
    );
    let services = select_agentcore_services(&answers_of(
        r#"{"memory_needs":"cross_session","isolation":"required","multi_agent":"yes"}"#,
    ));
    assert_eq!(
        services[..4],
        ["identity", "observability", "evaluations", "optimization"]
    );
    assert_eq!(services[4..], ["memory", "policy", "gateway"]);
    let services = select_agentcore_services(&answers_of(
        r#"{"session_state":"hitl","memory_needs":"cross_session"}"#,
    ));
    assert_eq!(services.iter().filter(|name| *name == "memory").count(), 1);
}

#[test]
fn model_selection_never_changes_the_verdict() {
    // Independence invariant: model_* answers must not affect verdict or scores.
    let base = r#""session_duration":"15min_to_8hr","traffic_pattern":"bursty","session_state":"hitl","ops_preference":"minimal""#;
    let profiles = real_profiles();
    let reference = run_score(&format!("{{{base}}}"), &profiles, None);
    for (priority, features) in [
        ("cost", "speech"),
        ("quality", "extended_thinking"),
        ("speed", "image_generation"),
        ("balanced", "long_context"),
    ] {
        let varied = run_score(
            &format!(r#"{{{base},"model_priority":"{priority}","model_features":"{features}"}}"#),
            &profiles,
            None,
        );
        assert_eq!(field(&varied, "verdict"), field(&reference, "verdict"));
        assert_eq!(field(&varied, "scores"), field(&reference, "scores"));
    }
}

#[test]
fn assumptions_list_unknown_dimensions() {
    let assumptions = collect_assumptions(&j(r#"{"session_duration":"under_15min"}"#));
    assert!(!assumptions.contains(&"session_duration defaulted to unknown".to_string()));
    assert!(assumptions.contains(&"traffic_pattern defaulted to unknown".to_string()));
    assert_eq!(assumptions.len(), DIMENSIONS.len() - 1);
    // An explicit null is not an assumption (Python: `None != "unknown"`).
    let nulls = collect_assumptions(&j(
        r#"{"session_duration":null,"traffic_pattern":"unknown"}"#,
    ));
    assert!(!nulls.contains(&"session_duration defaulted to unknown".to_string()));
    assert!(nulls.contains(&"traffic_pattern defaulted to unknown".to_string()));
}

fn warnings(answers: &str, verdict: &str, co: &[&str]) -> Vec<String> {
    let co: Vec<String> = co.iter().map(|id| (*id).to_string()).collect();
    collect_warnings(
        &answers_of(answers),
        &Json::Object(Vec::new()),
        verdict,
        &co,
        &[],
        None,
    )
    .expect("warnings")
}

#[test]
fn warning_fires_for_microvms_high_launch() {
    let high = r#"{"launch_concurrency":"high"}"#;
    let direct = warnings(high, "lambda_microvms", &[]);
    assert_eq!(direct.len(), 1);
    assert!(direct[0].contains("current-run verification"), "{direct:?}");
    let tied = warnings(high, "co_recommend", &["agentcore", "lambda_microvms"]);
    assert_eq!(tied.len(), 1);
    assert!(tied[0].contains("current-run verification"), "{tied:?}");
}

#[test]
fn no_warning_when_microvms_is_not_selected() {
    let high = r#"{"launch_concurrency":"high"}"#;
    assert!(warnings(high, "co_recommend", &["ecs", "eks"]).is_empty());
    assert!(warnings(high, "agentcore", &[]).is_empty());
    assert!(warnings("{}", "lambda_microvms", &[]).is_empty());
}

// ---- end to end over fixture and real registries --------------------------------------------------

#[test]
fn score_end_to_end_with_fixture_profiles() {
    let dir = TempDir::new("e2e");
    let agentcore = with(
        with_models(),
        "affinities",
        r#"{"session_duration":{"15min_to_8hr":5},"traffic_pattern":{"bursty":5}}"#,
    );
    let lambda = hard(
        "lambda",
        "session_duration",
        "15min_to_8hr",
        "Lambda has 15-minute timeout",
    );
    write_profile(&dir, &agentcore);
    write_profile(&dir, &lambda);
    let profiles = load_profiles(&dir, &GA_ONLY).expect("loads");

    let result = run_score(
        r#"{"session_duration":"15min_to_8hr","traffic_pattern":"bursty","multi_agent":"no","framework":"none"}"#,
        &profiles,
        None,
    );

    assert_eq!(field(&result, "verdict"), &j(r#""agentcore""#));
    assert_eq!(
        field(&result, "eliminated"),
        &j(r#"{"lambda":"Lambda has 15-minute timeout"}"#)
    );
    assert_eq!(field(&result, "deployment_model"), &j(r#""harness""#));
    assert!(result.get("co_recommend").is_none());
    assert!(result.get("blocking_constraints").is_none());
    let Json::Object(members) = &result else {
        panic!("object");
    };
    let order: Vec<&str> = members.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        order,
        [
            "verdict",
            "scores",
            "eliminated",
            "deferred_verification_requirements",
            "recommendation_status",
            "deployment_model",
            "agentcore_compute_type",
            "agentcore_services",
            "assumptions_used",
            "warnings"
        ]
    );
}

#[test]
fn score_no_viable_lists_blocking_constraints_sorted() {
    let profiles = [
        hard("lambda", "session_duration", "over_8hr", "15m cap"),
        hard("agentcore", "session_duration", "over_8hr", "8hr cap"),
    ];
    let result = run_score(r#"{"session_duration":"over_8hr"}"#, &profiles, None);
    assert_eq!(field(&result, "verdict"), &j(r#""no_viable_runtime""#));
    assert_eq!(
        field(&result, "blocking_constraints"),
        &j(r#"["agentcore: 8hr cap","lambda: 15m cap"]"#)
    );
    assert_eq!(field(&result, "scores"), &j("{}"));
}

#[test]
fn score_input_shape_errors_do_not_echo_values() {
    let profiles = [minimal("ecs", "ga")];
    for bad in [
        r"[]",
        r#"{"answers":[]}"#,
        r#"{"answers":"SECRET_SENTINEL"}"#,
    ] {
        let text = score(&j(bad), &profiles, None)
            .expect_err("shape")
            .to_string();
        assert!(!text.contains("SECRET_SENTINEL"), "{text}");
    }
}

#[test]
fn score_output_matches_the_published_schema() {
    let profiles = [with_models()];
    let result = run_score("{}", &profiles, None);
    let Json::Object(mut members) = result.clone() else {
        panic!("object");
    };
    members.push((
        "units".to_string(),
        Json::object(vec![("primary-agent", result)]),
    ));
    let text = fs::read_to_string(real_skill().join("scripts/schemas/scoring-result.json"))
        .expect("schema");
    let schema = schema_engine::compile(j(&text)).expect("schema compiles");
    let issues = schema
        .validate(&Json::Object(members))
        .expect("validation runs");
    assert!(issues.is_empty(), "{} issue(s)", issues.len());
}

#[test]
fn seed_schema_rejects_current_run_verifications() {
    let text = fs::read_to_string(real_skill().join("scripts/schemas/seed.json")).expect("schema");
    let schema = schema_engine::compile(j(&text)).expect("schema compiles");
    let issues = schema
        .validate(&j(r#"{"system":{"current_run_verifications":{}}}"#))
        .expect("validation runs");
    assert!(!issues.is_empty());
}

#[test]
fn scoring_result_schema_is_one_scored_shape() {
    let text = fs::read_to_string(real_skill().join("scripts/schemas/scoring-result.json"))
        .expect("schema");
    let root = j(&text);
    assert!(root.get("oneOf").is_none(), "no zero-agent union");
    let Some(Json::Array(required)) = root.get("required") else {
        panic!("required list");
    };
    assert!(required.contains(&j(r#""units""#)));
    let defs = root.get("$defs").expect("$defs");
    assert!(defs.get("scoredUnit").is_some());

    let schema = schema_engine::compile(root).expect("schema compiles");
    let unit = r#"{"verdict":"ecs","scores":{"ecs":10},"eliminated":{},"deployment_model":null,"agentcore_services":[],"model_recommendation":{"model":"m","reasoning":"r"},"deferred_verification_requirements":[],"recommendation_status":"final","assumptions_used":[],"warnings":[]}"#;
    let valid = |units: &str, verdict: &str| -> bool {
        let document = format!(
            r#"{{"verdict":"{verdict}","scores":{{"ecs":10}},"eliminated":{{}},"deployment_model":null,"agentcore_services":[],"deferred_verification_requirements":[],"recommendation_status":"final","assumptions_used":[],"warnings":[],"units":{units}}}"#
        );
        schema
            .validate(&j(&document))
            .expect("validation runs")
            .is_empty()
    };
    assert!(valid(&format!(r#"{{"solo":{unit}}}"#), "ecs"));
    assert!(!valid("{}", "ecs"), "empty units are rejected");
    assert!(!valid(r#"{"broken":{}}"#, "ecs"), "each unit is validated");
    assert!(!valid(&format!(r#"{{"solo":{unit}}}"#), "non_agent"));
}

// ---- golden results over the real registry ------------------------------------------------------

#[test]
fn golden_loads_five_ga_runtimes() {
    let ids: Vec<String> = real_profiles()
        .iter()
        .map(|profile| profile_id(profile).to_string())
        .collect();
    assert_eq!(
        ids,
        ["agentcore", "ecs", "eks", "lambda", "lambda_microvms"]
    );
}

fn eliminated_ids(result: &Json) -> Vec<String> {
    let Json::Object(members) = field(result, "eliminated") else {
        panic!("eliminated is an object");
    };
    members.iter().map(|(id, _)| id.clone()).collect()
}

fn deferred_runtimes(result: &Json) -> Vec<String> {
    let Json::Array(items) = field(result, "deferred_verification_requirements") else {
        panic!("deferred is an array");
    };
    items
        .iter()
        .filter_map(|item| item.get("runtime").and_then(Json::as_str))
        .map(str::to_string)
        .collect()
}

#[test]
fn golden_over_8hr_routes_agentcore_to_instances_not_elimination() {
    let result = run_score(r#"{"session_duration":"over_8hr"}"#, &real_profiles(), None);
    assert!(!eliminated_ids(&result).contains(&"agentcore".to_string()));
    let deferred = deferred_runtimes(&result);
    assert!(!deferred.contains(&"agentcore".to_string()), "{deferred:?}");
    assert!(deferred.contains(&"lambda".to_string()), "{deferred:?}");
    assert!(
        deferred.contains(&"lambda_microvms".to_string()),
        "{deferred:?}"
    );
    assert_eq!(
        field(&result, "agentcore_compute_type"),
        &j(r#""instances""#)
    );
    let Json::Array(warnings) = field(&result, "warnings") else {
        panic!("warnings");
    };
    assert!(warnings.iter().any(|warning| warning
        .as_str()
        .is_some_and(|text| text.contains("14 days"))));
}

#[test]
fn golden_over_8hr_verified_evidence_finalizes_lambda_family_only() {
    let artifact = evidence(
        &format!(
            r#"{{"agentcore.session_cap":{{"status":"verified","source":"{AGENTCORE_SESSION}","value":"8h"}},
                "lambda_microvms.session_cap":{{"status":"verified","source":"{MICROVMS}","value":"8h"}},
                "lambda.timeout":{{"status":"verified","source":"{AWS_DOCS}","value":"15m"}}}}"#
        ),
        "r",
    );
    let result = run_score(
        r#"{"session_duration":"over_8hr"}"#,
        &real_profiles(),
        Some(&artifact),
    );
    let eliminated = eliminated_ids(&result);
    assert!(!eliminated.contains(&"agentcore".to_string()));
    assert!(eliminated.contains(&"lambda_microvms".to_string()));
    assert!(eliminated.contains(&"lambda".to_string()));
    assert_eq!(field(&result, "recommendation_status"), &j(r#""final""#));
    assert_eq!(
        field(&result, "agentcore_compute_type"),
        &j(r#""instances""#)
    );
}

#[test]
fn golden_verdicts() {
    let profiles = real_profiles();
    let process_level = run_score(
        r#"{"session_duration":"15min_to_8hr","idle_resume":"process_level","session_state":"hitl","ops_preference":"moderate"}"#,
        &profiles,
        None,
    );
    assert_eq!(field(&process_level, "verdict"), &j(r#""lambda_microvms""#));

    let heavy = run_score(
        r#"{"compute_tier":"heavy_non_gpu","session_duration":"15min_to_8hr"}"#,
        &profiles,
        None,
    );
    assert!(!eliminated_ids(&heavy).contains(&"agentcore".to_string()));
    assert_eq!(field(&heavy, "verdict"), &j(r#""lambda_microvms""#));

    let agentic = run_score(
        r#"{"session_duration":"15min_to_8hr","traffic_pattern":"bursty","session_state":"hitl","ops_preference":"minimal","multi_agent":"no","framework":"none"}"#,
        &profiles,
        None,
    );
    assert_eq!(field(&agentic, "verdict"), &j(r#""agentcore""#));
    assert_eq!(field(&agentic, "deployment_model"), &j(r#""harness""#));
}

fn microvms_capacity_requirement() -> Json {
    j(&format!(
        r#"{{"runtime":"lambda_microvms","field":"launch_concurrency","value":"high","reason":"Lambda MicroVMs launch capacity must be verified before selection for high launch concurrency","verification_key":"lambda_microvms.launch_tps","verification_expected_value":"5 (not adjustable)","verification_sources":["{MICROVMS}"]}}"#
    ))
}

fn high_launch(launch_record: &str) -> Json {
    let artifact = evidence(
        &format!(
            r#"{{"agentcore.max_compute":{{"status":"verified","source":"{AGENTCORE_COMPUTE}","value":"2vCPU/8GB"}},
                "lambda.timeout":{{"status":"verified","source":"{AWS_DOCS}","value":"15m"}}{launch_record}}}"#
        ),
        "r",
    );
    run_score(
        r#"{"compute_tier":"heavy_non_gpu","session_duration":"15min_to_8hr","launch_concurrency":"high"}"#,
        &real_profiles(),
        Some(&artifact),
    )
}

fn assert_tied_agentcore_and_microvms(result: &Json) {
    assert_eq!(field(result, "verdict"), &j(r#""co_recommend""#));
    assert_eq!(
        field(result, "co_recommend"),
        &j(r#"["agentcore","lambda_microvms"]"#)
    );
    assert_eq!(
        field(result, "agentcore_compute_type"),
        &j(r#""instances""#)
    );
}

#[test]
fn golden_microvms_high_launch_requires_capacity_verification() {
    let result = high_launch("");
    assert_tied_agentcore_and_microvms(&result);
    assert_eq!(
        field(&result, "recommendation_status"),
        &j(r#""provisional""#)
    );
    assert_eq!(
        field(&result, "deferred_verification_requirements"),
        &Json::Array(vec![microvms_capacity_requirement()])
    );
    let Json::Array(warnings) = field(&result, "warnings") else {
        panic!("warnings");
    };
    assert!(warnings.iter().any(|warning| warning
        .as_str()
        .is_some_and(|text| text.contains("before selection"))));
}

#[test]
fn golden_microvms_high_launch_rejects_wrong_capacity_evidence() {
    for record in [
        format!(
            r#","lambda_microvms.launch_tps":{{"status":"verified","source":"{AWS_DOCS}","value":"5 (not adjustable)"}}"#
        ),
        format!(
            r#","lambda_microvms.launch_tps":{{"status":"verified","source":"{MICROVMS}","value":"4"}}"#
        ),
    ] {
        let result = high_launch(&record);
        assert_tied_agentcore_and_microvms(&result);
        assert_eq!(
            field(&result, "recommendation_status"),
            &j(r#""provisional""#)
        );
        assert_eq!(
            field(&result, "deferred_verification_requirements"),
            &Json::Array(vec![microvms_capacity_requirement()])
        );
    }
}

#[test]
fn golden_microvms_high_launch_emits_verified_warning() {
    let result = high_launch(&format!(
        r#","lambda_microvms.launch_tps":{{"status":"verified","source":"{MICROVMS}","value":"5 (not adjustable)"}}"#
    ));
    assert_tied_agentcore_and_microvms(&result);
    assert_eq!(field(&result, "recommendation_status"), &j(r#""final""#));
    assert_eq!(
        field(&result, "deferred_verification_requirements"),
        &j("[]")
    );
    let Json::Array(warnings) = field(&result, "warnings") else {
        panic!("warnings");
    };
    assert!(warnings.iter().any(|warning| warning
        .as_str()
        .is_some_and(|text| text.contains("verified in this run"))));
}

#[test]
fn every_real_profile_is_well_formed() {
    let statuses = ["ga", "preview", "coming_soon"];
    let profiles = load_profiles(&real_runtimes(), &statuses).expect("registry loads");
    assert!(!profiles.is_empty());
    for profile in &profiles {
        let id = profile_id(profile);
        let status = profile
            .get("status")
            .and_then(Json::as_str)
            .expect("status");
        assert!(statuses.contains(&status), "{id}");
        let Some(Json::Object(affinities)) = profile.get("affinities") else {
            panic!("{id}: affinities is an object");
        };
        for (dimension, table) in affinities {
            assert!(
                DIMENSIONS.contains(&dimension.as_str()),
                "{id}: unknown dimension {dimension}"
            );
            let Json::Object(points) = table else {
                panic!("{id}.{dimension} is an object");
            };
            let legal = legal_values(dimension).expect("legal values");
            for (value, score) in points {
                assert!(
                    matches!(score, Json::Int(_)),
                    "{id}.{dimension}.{value} not an int"
                );
                assert!(
                    legal.contains(&value.as_str()),
                    "{id}: illegal value {dimension}.{value}"
                );
            }
            // Explicit-unknown authoring rule: a declared dimension declares ALL legal values.
            let mut declared: Vec<&str> = points.iter().map(|(value, _)| value.as_str()).collect();
            declared.sort_unstable();
            let mut expected = legal.to_vec();
            expected.sort_unstable();
            assert_eq!(declared, expected, "{id}.{dimension}");
        }
        let mut answerable: Vec<&str> = DIMENSIONS.to_vec();
        answerable.push("compliance");
        let mut keys: Vec<&str> = Vec::new();
        if let Some(Json::Array(facts)) = profile.get("volatile_facts") {
            keys.extend(
                facts
                    .iter()
                    .filter_map(|fact| fact.get("verification_key").and_then(Json::as_str)),
            );
        }
        let Some(Json::Array(constraints)) = profile.get("hard_constraints") else {
            panic!("{id}: hard_constraints is an array");
        };
        for constraint in constraints {
            let field_name = constraint
                .get("field")
                .and_then(Json::as_str)
                .expect("field");
            assert!(answerable.contains(&field_name), "{id}: {field_name}");
            assert!(
                constraint
                    .get("reason")
                    .and_then(Json::as_str)
                    .is_some_and(|reason| !reason.is_empty()),
                "{id}: reason"
            );
            if constraint
                .get("verification_required")
                .is_some_and(|flag| *flag == Json::Bool(true))
            {
                let key = constraint.get("verification_key").and_then(Json::as_str);
                assert!(
                    key.is_some_and(|key| keys.contains(&key)),
                    "{id}: verification key"
                );
            }
        }
        if let Some(Json::Array(requirements)) = profile.get("selection_verification_requirements")
        {
            for requirement in requirements {
                let field_name = requirement
                    .get("field")
                    .and_then(Json::as_str)
                    .expect("field");
                assert!(answerable.contains(&field_name), "{id}: {field_name}");
                assert!(requirement
                    .get("reason")
                    .and_then(Json::as_str)
                    .is_some_and(|r| !r.is_empty()));
                let key = requirement.get("verification_key").and_then(Json::as_str);
                assert!(
                    key.is_some_and(|key| keys.contains(&key)),
                    "{id}: selection key"
                );
                assert!(requirement
                    .get("verification_expected_value")
                    .and_then(Json::as_str)
                    .is_some_and(|value| !value.is_empty()));
                assert!(matches!(
                    requirement.get("verification_sources"),
                    Some(Json::Array(sources)) if !sources.is_empty()
                ));
            }
        }
    }
}

#[test]
fn the_default_registry_is_the_skill_references_runtimes() {
    let runtimes = real_runtimes();
    let parts: Vec<_> = runtimes.components().rev().take(3).collect();
    let names: Vec<String> = parts
        .iter()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["runtimes", "references", "agent-advisor"]);
}

// ---- JSON rendering ------------------------------------------------------------------------------

#[test]
fn dumps_matches_python_indent_two_with_ensure_ascii() {
    let value = Json::object(vec![
        ("a", Json::Array(Vec::new())),
        ("b", Json::Object(Vec::new())),
        ("c", Json::string("\u{e9}\u{1f600}\"\\\n\u{7f}")),
        ("d", Json::Float(1.5)),
        (
            "e",
            Json::Array(vec![Json::Int(1), Json::Null, Json::Bool(true)]),
        ),
    ]);
    assert_eq!(
        dumps_indent2(&value),
        "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": \"\\u00e9\\ud83d\\ude00\\\"\\\\\\n\\u007f\",\n  \"d\": 1.5,\n  \"e\": [\n    1,\n    null,\n    true\n  ]\n}"
    );
    assert_eq!(dumps_indent2(&Json::Float(1e16)), "1e+16");
    assert_eq!(dumps_indent2(&Json::Float(f64::INFINITY)), "Infinity");
}

// ---- score-units and the commands ---------------------------------------------------------------------

const UNITS_ANSWERS: &str = r#"{
  "entry_point": "build_scratch",
  "primary_unit": "solo",
  "system": {"ops_preference": "minimal", "existing_cluster": "none",
             "multi_cloud": "no", "platform_fit": "none"},
  "units": {
    "solo": {"workload_class": "agent_session", "session_duration": "15min_to_8hr",
             "traffic_pattern": "steady", "session_state": "stateful",
             "isolation": "nice_to_have", "memory_needs": "session_only",
             "multi_agent": "no", "framework": "langgraph", "idle_resume": "none",
             "compute_tier": "light", "launch_concurrency": "moderate",
             "deployment_preference": "framework", "compliance": [],
             "model_priority": "balanced", "region": "single"},
    "cron": {"workload_class": "batch"}
  }
}"#;

fn units_result(answers: &str) -> Json {
    score_units(&j(answers), &real_profiles(), None).expect("units score")
}

#[test]
fn score_units_wraps_agent_units_and_mirrors_the_primary() {
    let out = units_result(UNITS_ANSWERS);
    let Some(Json::Object(units)) = out.get("units") else {
        panic!("units wrapper");
    };
    let ids: Vec<&str> = units.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["solo"], "agent_session units only");
    let Json::Object(solo) = &units[0].1 else {
        panic!("unit result");
    };
    for (key, value) in solo {
        assert_eq!(
            out.get(key),
            Some(value),
            "{key} is mirrored to the top level"
        );
    }
    let Json::Object(top) = &out else {
        panic!("object");
    };
    assert_eq!(top[0].0, "units", "the wrapper comes first");
}

#[test]
fn score_units_mirrors_the_named_primary_and_falls_back_to_the_first_unit() {
    let two = r#"{
      "primary_unit": "second",
      "system": {},
      "units": {
        "first": {"workload_class": "agent_session", "session_duration": "under_15min"},
        "second": {"workload_class": "agent_session", "session_duration": "15min_to_8hr", "idle_resume": "process_level"}
      }
    }"#;
    let out = units_result(two);
    let second = out
        .get("units")
        .and_then(|units| units.get("second"))
        .expect("second");
    assert_eq!(out.get("scores"), second.get("scores"));
    let fallback = units_result(&two.replace("\"second\",\n", "\"missing\",\n"));
    let first = fallback
        .get("units")
        .and_then(|units| units.get("first"))
        .expect("first");
    assert_eq!(fallback.get("scores"), first.get("scores"));
}

#[test]
fn single_unit_collapse_is_identical_to_the_direct_score() {
    // The unit loop merges system and unit answers; for one unit that merge is
    // the legacy flat answers dict, so the scoring call is literally identical.
    let flat = r#"{"session_duration":"over_8hr","traffic_pattern":"bursty","platform_fit":"none","session_state":"stateful","ops_preference":"low","isolation":"not_needed","memory_needs":"cross_session","multi_agent":"no","framework":"langgraph","existing_cluster":"none","multi_cloud":"no","idle_resume":"unknown","compute_tier":"light","launch_concurrency":"unknown"}"#;
    let profiles = real_profiles();
    let legacy = run_score(flat, &profiles, None);
    let document = r#"{"system":{"ops_preference":"low","existing_cluster":"none","multi_cloud":"no","platform_fit":"none"},"primary_unit":"only","units":{"only":{"workload_class":"agent_session","session_duration":"over_8hr","traffic_pattern":"bursty","session_state":"stateful","isolation":"not_needed","memory_needs":"cross_session","multi_agent":"no","framework":"langgraph","idle_resume":"unknown","compute_tier":"light","launch_concurrency":"unknown"}}}"#;
    let unitized = score_units(&j(document), &profiles, None).expect("units score");
    assert_eq!(
        unitized.get("units").and_then(|units| units.get("only")),
        Some(&legacy)
    );
    for (key, value) in match &legacy {
        Json::Object(members) => members.clone(),
        _ => panic!("object"),
    } {
        assert_eq!(unitized.get(&key), Some(&value), "{key}");
    }
}

#[test]
fn score_units_ignores_the_overridden_system_value_for_a_null_unit_answer() {
    let document = r#"{"system":{"ops_preference":"minimal"},"units":{"u":{"workload_class":"agent_session","ops_preference":null}}}"#;
    let out = units_result(document);
    let unit = out
        .get("units")
        .and_then(|units| units.get("u"))
        .expect("unit");
    let Some(Json::Array(assumptions)) = unit.get("assumptions_used") else {
        panic!("assumptions");
    };
    // The null is kept in the raw merge: not listed as an assumption, and the
    // value is the default "unknown" for scoring.
    assert!(!assumptions.contains(&j(r#""ops_preference defaulted to unknown""#)));
}

#[test]
fn score_units_reports_malformed_documents_and_no_agent_unit() {
    let profiles = real_profiles();
    for bad in [
        r"[]",
        r#"{"units":{}}"#,
        r#"{"system":[],"units":{}}"#,
        r#"{"system":{},"units":{"u":5}}"#,
        r#"{"system":{},"units":{}}"#,
        r#"{"system":{},"units":{"u":{"workload_class":"batch"}}}"#,
    ] {
        assert!(score_units(&j(bad), &profiles, None).is_err(), "{bad}");
    }
}

fn run_dir(label: &str) -> (TempDir, std::path::PathBuf) {
    let root = TempDir::new(label);
    let run = root.dir("run-1");
    (root, run)
}

#[test]
fn score_units_command_ignores_seed_evidence_and_uses_the_run_artifact() {
    let (_root, run) = run_dir("units-cmd");
    write(
        &run,
        "answers.json",
        &format!(
            r#"{{"entry_point":"build_scratch","system":{{"current_run_verifications":{{"lambda.timeout":{{"status":"verified","verified_this_run":true,"source":"{AWS_DOCS}","value":"15m"}}}}}},"primary_unit":"primary-agent","units":{{"primary-agent":{{"workload_class":"agent_session","session_duration":"15min_to_8hr"}}}}}}"#
        ),
    );
    let answers = run.join("answers.json");
    let plugin = real_skill().join("../..");
    let args = [
        "startup",
        "score-units",
        path_text(&answers),
        "--plugin-root",
        path_text(&plugin),
    ];

    let (status, stdout, stderr) = exec(&args);
    assert_eq!(status, 0, "{stderr}");
    let seed_only = j(&stdout);
    assert!(!eliminated_ids(&seed_only).contains(&"lambda".to_string()));
    assert_eq!(
        field(&seed_only, "recommendation_status"),
        &j(r#""provisional""#)
    );

    write(
        &run,
        RUN_EVIDENCE_FILENAME,
        &evidence(
            &format!(r#"{{"lambda.timeout":{{"status":"verified","source":"{AWS_DOCS}","value":"15m"}}}}"#),
            "run-1",
        )
        .to_compact_string(),
    );
    let (status, stdout, stderr) = exec(&args);
    assert_eq!(status, 0, "{stderr}");
    let materialized = j(&stdout);
    assert!(eliminated_ids(&materialized).contains(&"lambda".to_string()));
    assert_eq!(
        field(&materialized, "recommendation_status"),
        &j(r#""final""#)
    );
    // The output is Python's `json.dumps(indent=2)` plus the print newline.
    assert!(stdout.starts_with("{\n  \"units\": {\n"), "{stdout}");
    assert!(stdout.ends_with("}\n"));
    // Deterministic: the same input prints the same bytes.
    assert_eq!(exec(&args).1, stdout);
}

#[test]
fn score_units_command_rejects_evidence_from_another_run() {
    let (_root, run) = run_dir("units-other");
    write(&run, "answers.json", r#"{"system":{},"units":{}}"#);
    write(
        &run,
        RUN_EVIDENCE_FILENAME,
        &evidence("{}", "another-run").to_compact_string(),
    );
    let answers = run.join("answers.json");
    let plugin = real_skill().join("../..");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "score-units",
        path_text(&answers),
        "--plugin-root",
        path_text(&plugin),
    ]);
    assert_eq!(status, 1);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("run_id must match"), "{stderr}");
    assert!(load_run_evidence(&answers).is_err());
}

#[test]
fn invalid_or_linked_run_evidence_is_refused() {
    let (_root, run) = run_dir("units-bad-evidence");
    write(&run, "answers.json", r#"{"system":{},"units":{}}"#);
    let answers = run.join("answers.json");
    assert_eq!(load_run_evidence(&answers).expect("absent is fine"), None);

    write(&run, RUN_EVIDENCE_FILENAME, r#"{"artifact_type":"x"}"#);
    let text = load_run_evidence(&answers)
        .expect_err("invalid")
        .to_string();
    assert!(text.contains("invalid run-materialized evidence"), "{text}");

    write(&run, RUN_EVIDENCE_FILENAME, "{not json");
    assert!(load_run_evidence(&answers).is_err());

    #[cfg(unix)]
    {
        let other = TempDir::new("units-evidence-target");
        write(
            &other,
            "e.json",
            &evidence("{}", "run-1").to_compact_string(),
        );
        fs::remove_file(run.join(RUN_EVIDENCE_FILENAME)).expect("remove");
        std::os::unix::fs::symlink(other.join("e.json"), run.join(RUN_EVIDENCE_FILENAME))
            .expect("create link");
        let text = load_run_evidence(&answers).expect_err("link").to_string();
        assert!(text.contains("symbolic links are not followed"), "{text}");
    }
}

#[test]
fn score_command_writes_the_result_and_prints_the_verdict() {
    let (_root, run) = run_dir("score-cmd");
    write(
        &run,
        "answers.json",
        r#"{"entry_point":"build_scratch","answers":{"session_duration":"15min_to_8hr","traffic_pattern":"bursty","session_state":"hitl","ops_preference":"minimal","multi_agent":"no","framework":"none"}}"#,
    );
    let answers = run.join("answers.json");
    let plugin = real_skill().join("../..");
    let args = [
        "startup",
        "score",
        path_text(&answers),
        "--plugin-root",
        path_text(&plugin),
    ];
    let (status, stdout, stderr) = exec(&args);
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(stdout, "RESULT=ok VERDICT=agentcore\n");
    let written = fs::read_to_string(run.join("scoring-result.json")).expect("result written");
    assert!(
        written.starts_with("{\n  \"verdict\": \"agentcore\",\n"),
        "{written}"
    );
    assert!(!written.ends_with('\n'), "write_text adds no newline");
    assert_eq!(field(&j(&written), "deployment_model"), &j(r#""harness""#));

    // Writing again replaces the file with the same bytes.
    assert_eq!(exec(&args).0, 0);
    assert_eq!(
        fs::read_to_string(run.join("scoring-result.json")).expect("result"),
        written
    );
}

#[cfg(unix)]
#[test]
fn score_command_refuses_to_write_through_a_link() {
    let (_root, run) = run_dir("score-link");
    write(&run, "answers.json", r#"{"answers":{}}"#);
    let target = TempDir::new("score-link-target");
    write(&target, "victim.json", "keep");
    std::os::unix::fs::symlink(target.join("victim.json"), run.join("scoring-result.json"))
        .expect("create link");
    let answers = run.join("answers.json");
    let plugin = real_skill().join("../..");
    let (status, _, stderr) = exec(&[
        "startup",
        "score",
        path_text(&answers),
        "--plugin-root",
        path_text(&plugin),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("cannot write"), "{stderr}");
    assert_eq!(
        fs::read_to_string(target.join("victim.json")).expect("read"),
        "keep"
    );
}

#[test]
fn commands_report_usage_and_input_errors() {
    for command in ["score", "score-units"] {
        let (status, _, stderr) = exec(&["startup", command]);
        assert_eq!(status, 2, "{command}");
        assert!(stderr.contains("usage: toolkit startup"), "{stderr}");

        let (status, _, _) = exec(&["startup", command, "a.json", "b.json"]);
        assert_eq!(status, 2, "{command} takes one answers file");

        let dir = TempDir::new("cmd-errors");
        let missing = dir.join("absent.json");
        let (status, _, stderr) = exec(&["startup", command, path_text(&missing)]);
        assert_eq!(status, 1, "{command}");
        assert!(stderr.contains("cannot read"), "{stderr}");

        write(&dir, "bad.json", "{not json");
        let bad = dir.join("bad.json");
        let (status, _, stderr) = exec(&["startup", command, path_text(&bad)]);
        assert_eq!(status, 1, "{command}");
        assert!(stderr.contains("invalid JSON"), "{stderr}");

        write(&dir, "ok.json", r#"{"answers":{},"system":{},"units":{}}"#);
        let ok = dir.join("ok.json");
        let no_registry = dir.join("no-registry");
        let (status, _, stderr) = exec(&[
            "startup",
            command,
            path_text(&ok),
            "--runtimes-dir",
            path_text(&no_registry),
        ]);
        assert_eq!(status, 1, "{command}");
        assert!(stderr.contains("cannot read runtime profiles"), "{stderr}");
    }
}

#[test]
fn the_verdict_line_escapes_a_hostile_runtime_id() {
    let dir = TempDir::new("hostile-id");
    let registry = dir.dir("registry");
    write(
        &registry,
        "x.json",
        r#"{"id":"x\u001b[2J\nFORGED","status":"ga","affinities":{},"hard_constraints":[]}"#,
    );
    write(&dir, "answers.json", r#"{"answers":{}}"#);
    let answers = dir.join("answers.json");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "score",
        path_text(&answers),
        "--runtimes-dir",
        path_text(&registry),
    ]);
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
    assert!(stdout.contains("\\u{001b}"), "{stdout}");
    // The data document keeps the id, JSON-escaped.
    let written = fs::read_to_string(dir.join("scoring-result.json")).expect("result");
    assert!(written.contains("x\\u001b[2J\\nFORGED"), "{written}");
}

// ---- trusted entry point and linked-registry diagnostics (#4624, #4625) ----------------------

/// A registry whose profile `gated` is eliminated by `_entry_point == value`
/// and whose profile `open` is unconstrained.
fn entry_point_registry(value: &str) -> Vec<Json> {
    vec![
        hard("gated", "_entry_point", value, "entry gate"),
        minimal("open", "ga"),
    ]
}

fn entry_point_result(input: &str, profiles: &[Json]) -> Json {
    score(&j(input), profiles, None).expect("the input scores")
}

#[test]
fn an_explicit_entry_point_gates_a_constraint_like_scoring_py() {
    let profiles = entry_point_registry("migrate");
    let result = entry_point_result(r#"{"entry_point":"migrate","answers":{}}"#, &profiles);
    assert_eq!(eliminated_ids(&result), ["gated"]);
    assert_eq!(field(&result, "verdict"), &j(r#""open""#));
}

#[test]
fn an_omitted_entry_point_defaults_to_build_scratch() {
    let by_default = entry_point_registry("build_scratch");
    let result = entry_point_result(r#"{"answers":{}}"#, &by_default);
    assert_eq!(eliminated_ids(&result), ["gated"]);

    let migrate = entry_point_registry("migrate");
    let result = entry_point_result(r#"{"answers":{}}"#, &migrate);
    assert!(eliminated_ids(&result).is_empty());
}

#[test]
fn a_raw_entry_point_answer_cannot_steer_a_constraint() {
    let profiles = entry_point_registry("migrate");
    // The trusted default overwrites the spoofed answer.
    let spoofed = entry_point_result(r#"{"answers":{"_entry_point":"migrate"}}"#, &profiles);
    assert!(eliminated_ids(&spoofed).is_empty());
    // An explicit entry point wins over a conflicting raw answer both ways.
    let hidden = entry_point_result(
        r#"{"entry_point":"migrate","answers":{"_entry_point":"build_scratch"}}"#,
        &profiles,
    );
    assert_eq!(eliminated_ids(&hidden), ["gated"]);
}

#[test]
fn a_null_entry_point_stays_null_like_python_none() {
    let migrate = entry_point_registry("migrate");
    let result = entry_point_result(r#"{"entry_point":null,"answers":{}}"#, &migrate);
    assert!(eliminated_ids(&result).is_empty());
    let scratch = entry_point_registry("build_scratch");
    let result = entry_point_result(r#"{"entry_point":null,"answers":{}}"#, &scratch);
    assert!(eliminated_ids(&result).is_empty());
}

#[test]
fn score_units_passes_the_trusted_entry_point_to_every_unit() {
    let profiles = entry_point_registry("migrate");
    let unit = r#""units":{"u":{"workload_class":"agent_session"}}"#;
    let unit_eliminated = |document: &str| {
        let out = score_units(&j(document), &profiles, None).expect("units score");
        let unit = out
            .get("units")
            .and_then(|units| units.get("u"))
            .expect("unit");
        (eliminated_ids(unit), eliminated_ids(&out))
    };

    let (unit_ids, mirrored) = unit_eliminated(&format!(
        r#"{{"entry_point":"migrate","system":{{}},"primary_unit":"u",{unit}}}"#
    ));
    assert_eq!(unit_ids, ["gated"]);
    assert_eq!(mirrored, ["gated"]);

    let (unit_ids, _) = unit_eliminated(&format!(r#"{{"system":{{}},{unit}}}"#));
    assert!(
        unit_ids.is_empty(),
        "the default entry point is build_scratch"
    );

    // A raw `_entry_point` in the system or unit answers is overwritten.
    let (unit_ids, _) = unit_eliminated(
        r#"{"system":{"_entry_point":"migrate"},"units":{"u":{"workload_class":"agent_session","_entry_point":"migrate"}}}"#,
    );
    assert!(unit_ids.is_empty());
}

#[cfg(unix)]
fn hostile_link_name() -> &'static str {
    "a\u{1b}[2J\nFORGED\u{202e}.json"
}

#[cfg(unix)]
fn assert_one_escaped_line(text: &str) {
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains('\u{202e}'), "{text:?}");
    assert!(!text.contains('\n'), "{text:?}");
    assert!(!text.contains('\r'), "{text:?}");
    assert!(text.contains("\\u{001b}"), "{text}");
    assert!(text.contains("symbolic links are not followed"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_hostile_named_linked_profile_is_refused_with_an_escaped_diagnostic() {
    for dangling in [true, false] {
        let dir = TempDir::new("hostile-link");
        let outside = TempDir::new("hostile-link-target");
        write_profile(&outside, &minimal("evil", "ga"));
        let target = if dangling {
            outside.join("absent.json")
        } else {
            outside.join("evil.json")
        };
        std::os::unix::fs::symlink(target, dir.join(hostile_link_name())).expect("create link");
        let text = load_profiles(&dir, &GA_ONLY)
            .expect_err("link refused")
            .to_string();
        assert_one_escaped_line(&text);
    }
}

#[cfg(unix)]
#[test]
fn commands_refuse_a_hostile_named_linked_profile_without_raw_controls() {
    let plugin = TempDir::new("hostile-plugin");
    let runtimes = plugin.dir("skills/agent-advisor/references/runtimes");
    write(&runtimes, "ok.json", &profile_text(&minimal("ok", "ga")));
    let outside = TempDir::new("hostile-plugin-target");
    std::os::unix::fs::symlink(
        outside.join("absent.json"),
        runtimes.join(hostile_link_name()),
    )
    .expect("create dangling link");
    let (_root, run) = run_dir("hostile-link-cli");
    write(&run, "answers.json", r#"{"answers":{}}"#);
    write(
        &run,
        "units.json",
        r#"{"system":{},"units":{"u":{"workload_class":"agent_session"}}}"#,
    );
    for (command, file) in [("score", "answers.json"), ("score-units", "units.json")] {
        let answers = run.join(file);
        let (status, _, stderr) = exec(&[
            "startup",
            command,
            path_text(&answers),
            "--runtimes-dir",
            path_text(&runtimes),
        ]);
        assert_eq!(status, 1, "{command}");
        assert_one_escaped_line(stderr.trim_end_matches('\n'));
    }
}
