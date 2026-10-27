//! The scoring engine proper: hard constraints, affinity scores, the verdict,
//! deployment model, `AgentCore` compute type and services, assumptions and
//! warnings (`score` and its helpers of `scoring.py`).

use std::collections::BTreeMap;

use toolkit_core::Json;

use super::evidence::{
    has_current_run_evidence, is_current_run_verified, is_run_materialized_evidence,
};
use super::profiles::profile_id;
use super::value::{
    array_or_empty, members, py_contains, py_eq, required, Num, OrderedMap, Res, NULL,
};
use super::{ScoreError, DIMENSIONS, NEUTRAL_SCORE};
use crate::dynamodb::pyfmt;
use crate::manifests::is_falsy;

/// A candidate within this many points of the best score is co-recommended.
const TIE_THRESHOLD: i64 = 2;

const AGENTCORE_ALWAYS_SERVICES: [&str; 4] =
    ["identity", "observability", "evaluations", "optimization"];

/// Merged answers: the defaults overlaid with the non-null answers given.
pub(super) type Answers = BTreeMap<String, Json>;

/// `DEFAULTS`: every dimension `"unknown"`, compliance `["none"]`, region
/// `"unknown"`.
pub(super) fn defaults() -> Answers {
    let mut answers = Answers::new();
    for dimension in DIMENSIONS {
        answers.insert(dimension.to_string(), Json::string("unknown"));
    }
    answers.insert(
        "compliance".to_string(),
        Json::Array(vec![Json::string("none")]),
    );
    answers.insert("region".to_string(), Json::string("unknown"));
    answers
}

/// The string value of an answer, if it is a string.
fn text_answer<'a>(answers: &'a Answers, key: &str) -> Option<&'a str> {
    answers.get(key).and_then(Json::as_str)
}

/// `_constraint_matches`: whether the answers trigger a constraint.
fn constraint_matches(answers: &Answers, constraint: &Json) -> Res<bool> {
    let field = required(constraint, "field", "constraint")?;
    let trigger = required(constraint, "value", "constraint")?;
    if field.as_str() == Some("compliance") {
        let none_only = Json::Array(vec![Json::string("none")]);
        let compliance = answers.get("compliance").unwrap_or(&none_only);
        return py_contains(compliance, trigger);
    }
    let current = match field {
        Json::String(name) => answers.get(name.as_str()),
        Json::Array(_) | Json::Object(_) => {
            return Err(ScoreError::new("a constraint field must be a string"));
        }
        _ => None,
    };
    Ok(py_eq(current.unwrap_or(&NULL), trigger))
}

/// A hard-constraint outcome: final eliminations and matched constraints that
/// still await valid evidence.
pub(super) struct Eliminations {
    pub(super) eliminated: OrderedMap<Json>,
    pub(super) deferred: Vec<Json>,
}

/// The first matching hard constraint of each profile either eliminates it or,
/// when it needs verification the run does not provide, is deferred.
pub(super) fn evaluate_hard_constraints(
    answers: &Answers,
    profiles: &[Json],
    verifications: &Json,
) -> Res<Eliminations> {
    let mut result = Eliminations {
        eliminated: OrderedMap::default(),
        deferred: Vec::new(),
    };
    for profile in profiles {
        let id = profile_id(profile);
        for constraint in array_or_empty(profile, "hard_constraints", "runtime profile")? {
            if !constraint_matches(answers, constraint)? {
                continue;
            }
            let verification_required = constraint
                .get("verification_required")
                .is_some_and(|flag| !is_falsy(flag));
            if verification_required {
                let key = required(constraint, "verification_key", "hard constraint")?;
                let verified = is_current_run_verified(
                    verifications,
                    key,
                    constraint.get("verification_expected_value"),
                    constraint.get("verification_sources"),
                )?;
                if !verified {
                    result.deferred.push(Json::object(vec![
                        ("runtime", Json::string(id)),
                        (
                            "field",
                            required(constraint, "field", "constraint")?.clone(),
                        ),
                        (
                            "value",
                            required(constraint, "value", "constraint")?.clone(),
                        ),
                        (
                            "reason",
                            required(constraint, "reason", "hard constraint")?.clone(),
                        ),
                        ("verification_key", key.clone()),
                        (
                            "verification_expected_value",
                            constraint
                                .get("verification_expected_value")
                                .cloned()
                                .unwrap_or(Json::Null),
                        ),
                        (
                            "verification_sources",
                            constraint
                                .get("verification_sources")
                                .cloned()
                                .unwrap_or(Json::Null),
                        ),
                    ]));
                    break;
                }
            }
            let reason = required(constraint, "reason", "hard constraint")?;
            result.eliminated.set(id, reason.clone());
            break;
        }
    }
    Ok(result)
}

/// The points `profile` awards `value` on `dimension` (neutral when unrated).
fn affinity_points(affinities: Option<&Json>, dimension: &str, value: &Json) -> Res<Num> {
    let Some(affinities) = affinities else {
        return Ok(Num::Int(NEUTRAL_SCORE));
    };
    members(affinities, "profile \"affinities\"")?;
    let Some(table) = affinities.get(dimension) else {
        return Ok(Num::Int(NEUTRAL_SCORE));
    };
    members(table, "affinity table")?;
    let points = match value {
        Json::String(name) => table.get(name),
        Json::Array(_) | Json::Object(_) => {
            return Err(ScoreError::new("an answer must be a string or a number"));
        }
        _ => None,
    };
    match points {
        None => Ok(Num::Int(NEUTRAL_SCORE)),
        Some(points) => Num::from_json(points)
            .ok_or_else(|| ScoreError::new("an affinity score must be a number")),
    }
}

/// `_compute_scores`: the summed affinity of every runtime not eliminated.
pub(super) fn compute_scores(
    answers: &Answers,
    profiles: &[Json],
    eliminated: &OrderedMap<Json>,
) -> Res<OrderedMap<Num>> {
    let unknown = Json::string("unknown");
    let mut scores = OrderedMap::default();
    for profile in profiles {
        let id = profile_id(profile);
        if eliminated.contains(id) {
            continue;
        }
        let affinities = profile.get("affinities");
        let mut total = Num::Int(0);
        for dimension in DIMENSIONS {
            let value = answers.get(dimension).unwrap_or(&unknown);
            total = total.add(affinity_points(affinities, dimension, value)?)?;
        }
        scores.set(id, total);
    }
    Ok(scores)
}

/// `_determine_verdict`: the single winner, a tie within the threshold, or no
/// viable runtime.
pub(super) fn determine_verdict(
    scores: &OrderedMap<Num>,
    eliminated: &OrderedMap<Json>,
) -> Res<(String, Vec<String>)> {
    let active: Vec<(&str, Num)> = scores
        .iter()
        .filter(|(id, _)| !eliminated.contains(id))
        .map(|(id, score)| (id, *score))
        .collect();
    let Some(&(_, first)) = active.first() else {
        return Ok(("no_viable_runtime".to_string(), Vec::new()));
    };
    let best = active.iter().fold(
        first,
        |best, &(_, score)| if score > best { score } else { best },
    );
    let threshold = best.minus(TIE_THRESHOLD);
    let mut top: Vec<String> = active
        .iter()
        .filter(|&&(_, score)| score >= threshold)
        .map(|&(id, _)| id.to_string())
        .collect();
    top.sort();
    match top.len() {
        0 => Err(ScoreError::new("no runtime reaches the top score")),
        1 => Ok((top.remove(0), Vec::new())),
        _ => Ok(("co_recommend".to_string(), top)),
    }
}

/// `_select_deployment_model`: harness versus framework on the runtime, only
/// for a runtime that offers both.
pub(super) fn select_deployment_model(
    answers: &Answers,
    verdict: &str,
    profiles: &[Json],
) -> Res<Option<&'static str>> {
    let Some(profile) = profiles
        .iter()
        .find(|profile| profile_id(profile) == verdict)
    else {
        return Ok(None);
    };
    let none = Json::Array(Vec::new());
    let models = profile.get("deployment_models").unwrap_or(&none);
    if !py_contains(models, &Json::string("harness"))?
        || !py_contains(models, &Json::string("framework_on_runtime"))?
    {
        return Ok(None);
    }
    // An explicit user preference (Pass 2) overrides the inference below.
    match text_answer(answers, "deployment_preference") {
        Some("harness") => return Ok(Some("harness")),
        Some("framework") => return Ok(Some("framework_on_runtime")),
        _ => {}
    }
    // Inference: multi-agent or a code framework means framework on runtime.
    if text_answer(answers, "multi_agent") == Some("yes")
        || matches!(
            text_answer(answers, "framework"),
            Some("langgraph" | "crewai" | "custom")
        )
    {
        return Ok(Some("framework_on_runtime"));
    }
    Ok(Some("harness"))
}

/// `_select_agentcore_compute_type`: `instances` for sessions up to 14 days,
/// GPU or heavy compute and instance-type choice, else the default `microvms`.
pub(super) fn select_agentcore_compute_type(
    answers: &Answers,
    verdict: &str,
    co_recommend: &[String],
) -> Option<&'static str> {
    let agentcore_wins = verdict == "agentcore"
        || (verdict == "co_recommend" && co_recommend.iter().any(|id| id == "agentcore"));
    if !agentcore_wins {
        return None;
    }
    let needs_instances = text_answer(answers, "session_duration") == Some("over_8hr")
        || matches!(
            text_answer(answers, "compute_tier"),
            Some("gpu" | "heavy_non_gpu")
        )
        || text_answer(answers, "instance_type_requirement") == Some("yes");
    Some(if needs_instances {
        "instances"
    } else {
        "microvms"
    })
}

/// `_select_agentcore_services`: the always-on baseline plus the add-ons the
/// answers call for, in a fixed order without duplicates.
pub(super) fn select_agentcore_services(answers: &Answers) -> Vec<String> {
    let mut services: Vec<String> = AGENTCORE_ALWAYS_SERVICES
        .iter()
        .map(|service| (*service).to_string())
        .collect();
    let mut add = |name: &str| {
        if !services.iter().any(|service| service == name) {
            services.push(name.to_string());
        }
    };
    if matches!(
        text_answer(answers, "session_state"),
        Some("hitl" | "stateful")
    ) {
        add("memory");
    }
    if text_answer(answers, "memory_needs") == Some("cross_session") {
        add("memory");
    }
    if text_answer(answers, "isolation") == Some("required") {
        add("policy");
    }
    if text_answer(answers, "multi_agent") == Some("yes") {
        add("gateway");
    }
    services
}

/// `_collect_assumptions`: a dimension is assumed unknown when the raw answers
/// omit it or say `"unknown"` (an explicit `null` is not an assumption).
pub(super) fn collect_assumptions(raw_answers: &Json) -> Vec<String> {
    DIMENSIONS
        .iter()
        .filter(|dimension| match raw_answers.get(dimension) {
            None => true,
            Some(value) => value.as_str() == Some("unknown"),
        })
        .map(|dimension| format!("{dimension} defaulted to unknown"))
        .collect()
}

/// A selection-verification requirement that applies to a selected runtime.
pub(super) type Requirement<'a> = (String, &'a Json);

/// `_matching_selection_verification_requirements`: gates that apply only to
/// the selected runtime(s).
pub(super) fn matching_selection_requirements<'a>(
    answers: &Answers,
    profiles: &'a [Json],
    verdict: &str,
    co_recommend: &[String],
) -> Res<Vec<Requirement<'a>>> {
    let selected: Vec<&str> = if verdict == "co_recommend" {
        co_recommend.iter().map(String::as_str).collect()
    } else if verdict == "no_viable_runtime" {
        Vec::new()
    } else {
        vec![verdict]
    };
    let mut matching = Vec::new();
    for profile in profiles {
        let id = profile_id(profile);
        if !selected.contains(&id) {
            continue;
        }
        for requirement in array_or_empty(
            profile,
            "selection_verification_requirements",
            "runtime profile",
        )? {
            if constraint_matches(answers, requirement)? {
                matching.push((id.to_string(), requirement));
            }
        }
    }
    Ok(matching)
}

/// `_defer_unverified_selection_requirements`: unresolved selected-runtime
/// gates, which never eliminate a candidate.
pub(super) fn defer_unverified_selection_requirements(
    verifications: &Json,
    requirements: &[Requirement<'_>],
) -> Res<Vec<Json>> {
    let owner = "selection verification requirement";
    let mut deferred = Vec::new();
    for (runtime, requirement) in requirements {
        let key = required(requirement, "verification_key", owner)?;
        let expected = required(requirement, "verification_expected_value", owner)?;
        let sources = required(requirement, "verification_sources", owner)?;
        if is_current_run_verified(verifications, key, Some(expected), Some(sources))? {
            continue;
        }
        deferred.push(Json::object(vec![
            ("runtime", Json::string(runtime.as_str())),
            ("field", required(requirement, "field", owner)?.clone()),
            ("value", required(requirement, "value", owner)?.clone()),
            ("reason", required(requirement, "reason", owner)?.clone()),
            ("verification_key", key.clone()),
            ("verification_expected_value", expected.clone()),
            ("verification_sources", sources.clone()),
        ]));
    }
    Ok(deferred)
}

const MICROVMS_LAUNCH_KEY: &str = "lambda_microvms.launch_tps";

const INSTANCES_WARNING: &str = "AgentCore Instances compute type: sessions persist up to 14 days \
(not indefinitely \u{2014} an always-on service is still a better fit for \
ECS/EKS); pricing is EC2 in your account (Savings Plans/ODCRs \
apply) plus an AgentCore management fee, NOT consumption-based; \
Linux only at launch; launch-region set is limited and volatile \u{2014} \
verify current availability via MCP (volatile_facts.\
instances_regions).";

/// `_collect_warnings`.
pub(super) fn collect_warnings(
    answers: &Answers,
    verifications: &Json,
    verdict: &str,
    co_recommend: &[String],
    requirements: &[Requirement<'_>],
    compute_type: Option<&str>,
) -> Res<Vec<String>> {
    let mut warnings = Vec::new();
    let microvms_is_winner = verdict == "lambda_microvms"
        || (verdict == "co_recommend" && co_recommend.iter().any(|id| id == "lambda_microvms"));
    if microvms_is_winner && text_answer(answers, "launch_concurrency") == Some("high") {
        let launch_requirements: Vec<&Json> = requirements
            .iter()
            .filter(|(runtime, requirement)| {
                runtime == "lambda_microvms"
                    && requirement.get("verification_key").and_then(Json::as_str)
                        == Some(MICROVMS_LAUNCH_KEY)
            })
            .map(|(_, requirement)| *requirement)
            .collect();
        let launch_verified = if launch_requirements.is_empty() {
            has_current_run_evidence(verifications.get(MICROVMS_LAUNCH_KEY))
        } else {
            let mut all = true;
            for requirement in launch_requirements {
                let owner = "selection verification requirement";
                all &= is_current_run_verified(
                    verifications,
                    required(requirement, "verification_key", owner)?,
                    Some(required(requirement, "verification_expected_value", owner)?),
                    Some(required(requirement, "verification_sources", owner)?),
                )?;
            }
            all
        };
        warnings.push(if launch_verified {
            "High launch concurrency requires capacity planning against the Lambda \
MicroVMs launch-rate value verified in this run."
                .to_string()
        } else {
            "High launch concurrency requires current-run verification of Lambda \
MicroVMs launch capacity before selection."
                .to_string()
        });
    }
    if compute_type == Some("instances") {
        warnings.push(INSTANCES_WARNING.to_string());
    }
    Ok(warnings)
}

/// The deployment model of the verdict, or of the first co-recommended runtime
/// that offers one.
fn deployment_model_for(
    answers: &Answers,
    verdict: &str,
    co_recommend: &[String],
    profiles: &[Json],
) -> Res<Option<&'static str>> {
    if verdict == "no_viable_runtime" {
        return Ok(None);
    }
    if verdict != "co_recommend" {
        return select_deployment_model(answers, verdict, profiles);
    }
    for id in co_recommend {
        if let Some(model) = select_deployment_model(answers, id, profiles)? {
            return Ok(Some(model));
        }
    }
    Ok(None)
}

/// `"<runtime>: <reason>"` for every eliminated runtime, sorted by runtime.
fn blocking_constraints(eliminated: &OrderedMap<Json>) -> Json {
    let mut blocking: Vec<(&str, &Json)> = eliminated.iter().collect();
    blocking.sort_by(|left, right| left.0.cmp(right.0));
    let lines: Vec<String> = blocking
        .iter()
        .map(|(id, reason)| format!("{id}: {}", pyfmt::text(reason)))
        .collect();
    string_array(&lines)
}

fn string_array(items: &[String]) -> Json {
    Json::Array(
        items
            .iter()
            .map(|item| Json::string(item.as_str()))
            .collect(),
    )
}

/// Scores one answers document against the runtime `profiles`.
///
/// `input` is `{"entry_point": ..., "answers": {...}}`; `run_evidence`, when it
/// satisfies the artifact contract, supplies the verifications that finalize
/// constraints. The result is ordered like the Python dict: `verdict`,
/// `scores`, `eliminated`, `deferred_verification_requirements`,
/// `recommendation_status`, `deployment_model`, `agentcore_compute_type`,
/// `agentcore_services`, `assumptions_used`, `warnings`, then `co_recommend`
/// or `blocking_constraints` when they apply.
///
/// # Errors
///
/// Where Python raised: a profile, constraint or answers value of the wrong
/// type or a missing constraint key.
pub(crate) fn score(input: &Json, profiles: &[Json], run_evidence: Option<&Json>) -> Res<Json> {
    members(input, "scoring input")?;
    let no_answers = Json::Object(Vec::new());
    let raw_answers = input.get("answers").unwrap_or(&no_answers);
    let raw_pairs = members(raw_answers, "\"answers\"")?;
    let no_verifications = Json::Object(Vec::new());
    let verifications = match run_evidence {
        Some(evidence) if is_run_materialized_evidence(evidence) => {
            evidence.get("verifications").unwrap_or(&no_verifications)
        }
        _ => &no_verifications,
    };

    let mut answers = defaults();
    for (name, value) in raw_pairs {
        if *value != Json::Null {
            answers.insert(name.clone(), value.clone());
        }
    }
    // scoring.py assigns the trusted entry point after the overlay, so a raw
    // `answers._entry_point` can never steer a constraint (a JSON null stays
    // null, as Python's `None` does).
    let entry_point = input
        .get("entry_point")
        .cloned()
        .unwrap_or_else(|| Json::string("build_scratch"));
    answers.insert("_entry_point".to_string(), entry_point);

    let outcome = evaluate_hard_constraints(&answers, profiles, verifications)?;
    let Eliminations {
        eliminated,
        mut deferred,
    } = outcome;
    let scores = compute_scores(&answers, profiles, &eliminated)?;
    let (verdict, co_recommend) = determine_verdict(&scores, &eliminated)?;
    let requirements =
        matching_selection_requirements(&answers, profiles, &verdict, &co_recommend)?;
    deferred.extend(defer_unverified_selection_requirements(
        verifications,
        &requirements,
    )?);

    let deployment_model = deployment_model_for(&answers, &verdict, &co_recommend, profiles)?;
    let compute_type = select_agentcore_compute_type(&answers, &verdict, &co_recommend);
    let warnings = collect_warnings(
        &answers,
        verifications,
        &verdict,
        &co_recommend,
        &requirements,
        compute_type,
    )?;

    let status = if deferred.is_empty() {
        "final"
    } else {
        "provisional"
    };
    let mut result = vec![
        ("verdict", Json::string(verdict.as_str())),
        (
            "scores",
            Json::Object(
                scores
                    .iter()
                    .map(|(id, score)| (id.to_string(), score.to_json()))
                    .collect(),
            ),
        ),
        (
            "eliminated",
            Json::Object(
                eliminated
                    .iter()
                    .map(|(id, reason)| (id.to_string(), reason.clone()))
                    .collect(),
            ),
        ),
        ("deferred_verification_requirements", Json::Array(deferred)),
        ("recommendation_status", Json::string(status)),
        (
            "deployment_model",
            deployment_model.map_or(Json::Null, Json::string),
        ),
        (
            "agentcore_compute_type",
            compute_type.map_or(Json::Null, Json::string),
        ),
        (
            "agentcore_services",
            string_array(&select_agentcore_services(&answers)),
        ),
        (
            "assumptions_used",
            string_array(&collect_assumptions(raw_answers)),
        ),
        ("warnings", string_array(&warnings)),
    ];
    if verdict == "co_recommend" {
        result.push(("co_recommend", string_array(&co_recommend)));
    }
    if verdict == "no_viable_runtime" {
        result.push(("blocking_constraints", blocking_constraints(&eliminated)));
    }
    Ok(Json::object(result))
}
