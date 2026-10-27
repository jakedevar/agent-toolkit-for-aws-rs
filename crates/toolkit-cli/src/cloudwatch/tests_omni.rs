//! Tests of the trace evaluation and dataset capture helpers (#4381): the
//! scenarios of `test_evaluate_traces.py` plus the writeback, confirmation
//! and recorded-input rules. Every client is scripted; nothing reaches AWS.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::aws_client::{NoLiveClient, Service, NO_LIVE_CLIENT_MESSAGE};
use super::dataset::{self, invalid, to_example, Mode, MAX_EXAMPLE_BYTES};
use super::evaluate::{
    self, is_session_id, is_trace_id, normalize_span_kind, row_to_span, Level, Limits, Options,
};
use super::logs_query::VirtualPacer;
use super::tests_api::{json, service_error, Fake};
use super::timefmt::parse_iso_timestamp;
use crate::dynamodb::pyfmt;

fn now() -> super::timefmt::UtcTime {
    parse_iso_timestamp("2025-02-03T18:42:00Z").expect("fixed instant")
}

// ---- the fake Omni / AgentCore world ------------------------------------

#[derive(Default)]
struct Counters {
    fetch: usize,
    eval: usize,
    sql: Vec<String>,
    logs: Vec<(String, Json)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Eval {
    Ok,
    Error,
}

struct World {
    tool_spans: usize,
    session_id: Option<&'static str>,
    eval: Eval,
    omni_error: bool,
    logs: HashMap<&'static str, Vec<AwsError>>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            tool_spans: 1,
            session_id: None,
            eval: Eval::Ok,
            omni_error: false,
            logs: HashMap::new(),
        }
    }
}

fn span_row(trace: &str, span_id: &str, tool: bool, session: Option<&str>) -> Json {
    let mut pairs: Vec<(&str, Json)> = vec![
        ("traceId", Json::string(trace)),
        ("spanId", Json::string(span_id)),
        ("parentSpanId", Json::Null),
        ("name", Json::string("span")),
        ("kind", Json::string("INTERNAL")),
        ("startTimeUnixNano", Json::string("1000000000")),
        ("endTimeUnixNano", Json::string("2000000000")),
        ("svc_name", Json::string("svc")),
        ("scope_name", Json::string("")),
        ("status_code", Json::string("")),
    ];
    if tool {
        pairs.push(("gen_ai_op_name", Json::string("execute_tool")));
    }
    if let Some(session) = session {
        pairs.push(("session_id_attr", Json::string(session)));
    }
    Json::object(pairs)
}

fn world_client(world: World, counters: &Rc<RefCell<Counters>>) -> Fake {
    let counters = Rc::clone(counters);
    let queries: Rc<RefCell<HashMap<String, String>>> = Rc::default();
    let logs = RefCell::new(world.logs);
    Fake::new(
        move |service, operation, request| match (service, operation) {
            (Service::CloudWatchOmni, "StartTelemetryQuerySession") => {
                Ok(json(r#"{"sessionId": "sess-1"}"#))
            }
            (Service::CloudWatchOmni, "StartTelemetryQuery") => {
                let sql = request
                    .get("queryString")
                    .and_then(Json::as_str)
                    .unwrap()
                    .to_string();
                counters.borrow_mut().sql.push(sql.clone());
                let id = format!("q-{}", queries.borrow().len());
                queries.borrow_mut().insert(id.clone(), sql);
                Ok(Json::object(vec![("queryId", Json::string(id))]))
            }
            (Service::CloudWatchOmni, "GetTelemetryQueryResults") => {
                if world.omni_error {
                    return Err(service_error(
                        "AccessDeniedException",
                        "no cloudwatchomni access",
                    ));
                }
                if request.get("maxResults") == Some(&Json::Int(1)) {
                    return Ok(json(r#"{"status": "Complete", "rows": []}"#));
                }
                let id = request
                    .get("queryId")
                    .and_then(Json::as_str)
                    .unwrap()
                    .to_string();
                let sql = queries.borrow().get(&id).cloned().unwrap_or_default();
                if sql.contains("IS NOT NULL") {
                    let rows = world.session_id.map_or_else(Vec::new, |session| {
                        vec![Json::object(vec![("session_id", Json::string(session))])]
                    });
                    return Ok(Json::object(vec![
                        ("status", Json::string("Complete")),
                        ("rows", Json::Array(rows)),
                    ]));
                }
                counters.borrow_mut().fetch += 1;
                let mut rows = vec![span_row("t", &"0".repeat(16), false, world.session_id)];
                for index in 0..world.tool_spans {
                    rows.push(span_row(
                        "t",
                        &format!("{:016x}", index + 1),
                        true,
                        world.session_id,
                    ));
                }
                Ok(Json::object(vec![
                    ("status", Json::string("Complete")),
                    ("rows", Json::Array(rows)),
                ]))
            }
            (Service::CloudWatchOmni, "StopTelemetryQuerySession") => Ok(json("{}")),
            (Service::BedrockAgentCore, "Evaluate") => {
                counters.borrow_mut().eval += 1;
                let evaluator = request
                    .get("evaluatorId")
                    .and_then(Json::as_str)
                    .unwrap()
                    .to_string();
                let result = if world.eval == Eval::Error {
                    Json::object(vec![
                        ("evaluatorId", Json::string(evaluator)),
                        ("value", Json::Null),
                        ("errorCode", Json::string("AgentSpanMappingException")),
                        ("errorMessage", Json::string("boom")),
                    ])
                } else {
                    Json::object(vec![
                        ("evaluatorId", Json::string(evaluator)),
                        ("value", Json::Float(0.5)),
                        ("label", Json::string("ok")),
                        ("explanation", Json::string("why")),
                    ])
                };
                Ok(Json::object(vec![(
                    "evaluationResults",
                    Json::Array(vec![result]),
                )]))
            }
            (Service::Logs, operation) => {
                counters
                    .borrow_mut()
                    .logs
                    .push((operation.to_string(), request.clone()));
                if let Some(queue) = logs.borrow_mut().get_mut(operation) {
                    if !queue.is_empty() {
                        return Err(queue.remove(0));
                    }
                }
                Ok(json("{}"))
            }
            other => panic!("unexpected call {other:?}"),
        },
    )
}

fn options(trace_ids: &str) -> Options {
    Options {
        trace_ids: Some(trace_ids.to_string()),
        no_writeback: true,
        ..Options::default()
    }
}

fn run_with(
    world: World,
    options: &Options,
    limits: &Limits,
) -> (Result<Json, Json>, Rc<RefCell<Counters>>) {
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = world_client(world, &counters);
    let outcome = evaluate::run(
        &mut client,
        &mut VirtualPacer::default(),
        options,
        limits,
        now(),
    );
    (outcome, counters)
}

fn scored(world: World, options: &Options) -> (Json, Rc<RefCell<Counters>>) {
    let (outcome, counters) = run_with(world, options, &Limits::default());
    match outcome {
        Ok(receipt) => (receipt, counters),
        Err(payload) => panic!("expected a receipt, got {}", pyfmt::dumps(&payload, false)),
    }
}

fn int(receipt: &Json, key: &str) -> i64 {
    match receipt.get(key) {
        Some(Json::Int(value)) => *value,
        other => panic!("{key} is not an integer: {other:?}"),
    }
}

fn notes(receipt: &Json) -> Vec<String> {
    match receipt.get("notes") {
        // recorded-shape: test assertion over a receipt, request or fixture the test holds.
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn failure_text(payload: &Json) -> String {
    pyfmt::dumps(payload, false)
}

// ---- the scenarios of test_evaluate_traces.py ---------------------------

#[test]
fn single_trace_scores_one_pair_with_one_fetch() {
    let mut opts = options("");
    opts.trace_ids = None;
    opts.trace_id = Some("aa".to_string());
    let (receipt, counters) = scored(World::default(), &opts);
    assert_eq!(int(&receipt, "pairsScored"), 1);
    assert_eq!(counters.borrow().fetch, 1);
    // recorded-shape: test assertion over a receipt, request or fixture the test holds.
    let Some(Json::Array(results)) = receipt.get("results") else {
        panic!("results")
    };
    let keys: Vec<&str> = match &results[0] {
        Json::Object(pairs) => pairs.iter().map(|(key, _)| key.as_str()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        keys,
        ["evaluatorId", "value", "label", "hasExplanation", "traceId"]
    );
}

#[test]
fn matrix_fetches_each_trace_once() {
    let mut opts = options("aa,bb");
    opts.evaluator_ids = Some("E1,E2".to_string());
    let (receipt, counters) = scored(World::default(), &opts);
    assert_eq!(int(&receipt, "pairsScored"), 4);
    assert_eq!(counters.borrow().fetch, 2, "N fetches, not N x M");
    assert_eq!(counters.borrow().eval, 4);
    assert_eq!(int(&receipt, "tracesFetched"), 2);
    assert_eq!(int(&receipt, "tracesScored"), 2);
}

#[test]
fn ground_truth_evaluators_are_dropped_or_refuse_the_run() {
    let mut opts = options("aa");
    opts.evaluator_ids = Some("Builtin.Helpfulness,Builtin.TrajectoryExactOrderMatch".to_string());
    let (receipt, _) = scored(World::default(), &opts);
    assert_eq!(
        pyfmt::dumps(receipt.get("evaluators").unwrap(), false),
        r#"["Builtin.Helpfulness"]"#
    );
    assert!(notes(&receipt)[0].starts_with(
        "skipped evaluator(s) Builtin.TrajectoryExactOrderMatch require ground truth"
    ));

    let mut only = options("aa");
    only.evaluator_ids = Some("Builtin.TrajectoryExactOrderMatch".to_string());
    let (outcome, counters) = run_with(World::default(), &only, &Limits::default());
    let payload = outcome.expect_err("all ground truth refuses the run");
    assert_eq!(
        pyfmt::dumps(payload.get("evaluatorIds").unwrap(), false),
        r#"["Builtin.TrajectoryExactOrderMatch"]"#
    );
    assert_eq!(counters.borrow().fetch, 0, "refused before any fetch");
}

#[test]
fn caps_truncate_with_a_note_and_never_silently() {
    let ids = |count: usize| {
        (0..count)
            .map(|i| format!("{i:02x}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let (receipt60, _) = scored(World::default(), &options(&ids(60)));
    assert_eq!(int(&receipt60, "tracesScored"), 50);
    assert!(notes(&receipt60)
        .iter()
        .any(|n| n.starts_with("capped to 50 traces")));

    let mut many_evaluators = options("aa,bb,cc");
    many_evaluators.evaluator_ids = Some(
        (0..12)
            .map(|i| format!("E{i}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    let (receipt, _) = scored(World::default(), &many_evaluators);
    // recorded-shape: test assertion over a receipt, request or fixture the test holds.
    let Some(Json::Array(evaluators)) = receipt.get("evaluators") else {
        panic!()
    };
    assert_eq!(evaluators.len(), 10);
    assert!(notes(&receipt)
        .iter()
        .any(|n| n.starts_with("capped to 10 evaluators") && n.ends_with("NOT run: E10, E11")));

    let mut pair_bound = options(&ids(40));
    pair_bound.evaluator_ids = Some("E1,E2,E3".to_string());
    let (receipt, _) = scored(World::default(), &pair_bound);
    assert_eq!(int(&receipt, "tracesScored"), 33);
    assert_eq!(int(&receipt, "pairsScored"), 99);
    assert!(notes(&receipt).iter().any(|n| n.contains("100-pair")));
}

#[test]
fn the_evaluate_call_budget_bounds_tool_call_fan_out() {
    let mut opts = options("aa,bb,cc");
    opts.evaluator_ids = Some("E1,E2".to_string());
    opts.level = Level::ToolCall;
    let limits = Limits {
        max_evaluate_calls: 5,
        ..Limits::default()
    };
    let (outcome, counters) = run_with(
        World {
            tool_spans: 25,
            ..World::default()
        },
        &opts,
        &limits,
    );
    let receipt = outcome.expect("some pairs score");
    assert!(counters.borrow().eval <= 5, "{}", counters.borrow().eval);
    assert!(
        notes(&receipt).iter().any(|n| n.contains("budget")),
        "{:?}",
        notes(&receipt)
    );
}

#[test]
fn tool_spans_are_chunked_ten_ids_per_call() {
    let mut opts = options("aa");
    opts.level = Level::ToolCall;
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = world_client(
        World {
            tool_spans: 25,
            ..World::default()
        },
        &counters,
    );
    let outcome = evaluate::run(
        &mut client,
        &mut VirtualPacer::default(),
        &opts,
        &Limits::default(),
        now(),
    );
    assert!(outcome.is_ok());
    let evaluates: Vec<usize> = client
        .calls
        .iter()
        .filter(|(_, operation, _)| operation == "Evaluate")
        .map(|(_, _, request)| {
            match request
                .get("evaluationTarget")
                .and_then(|t| t.get("spanIds"))
            {
                // recorded-shape: test assertion over a receipt, request or fixture the test holds.
                Some(Json::Array(ids)) => ids.len(),
                other => panic!("{other:?}"),
            }
        })
        .collect();
    assert_eq!(evaluates, [10, 10, 5]);
}

#[test]
fn tool_call_level_without_tool_spans_is_reported() {
    let mut opts = options("aa");
    opts.level = Level::ToolCall;
    let (outcome, counters) = run_with(
        World {
            tool_spans: 0,
            ..World::default()
        },
        &opts,
        &Limits::default(),
    );
    let payload = outcome.expect_err("nothing to score");
    assert!(failure_text(&payload).contains("TOOL_CALL requested but the trace has no tool spans"));
    assert_eq!(counters.borrow().eval, 0);
}

#[test]
fn error_only_pairs_are_not_counted_but_stay_visible() {
    let (receipt, _) = scored(
        World {
            eval: Eval::Error,
            ..World::default()
        },
        &options("aa"),
    );
    assert_eq!(int(&receipt, "pairsScored"), 0);
    assert_eq!(int(&receipt, "tracesScored"), 0);
    let text = pyfmt::dumps(&receipt, false);
    assert!(
        text.contains(r#""errorCode": "AgentSpanMappingException", "errorMessage": "boom""#),
        "{text}"
    );
}

#[test]
fn traces_scored_is_honest_when_the_budget_truncates() {
    let mut opts = options("aa,bb,cc");
    opts.level = Level::ToolCall;
    let limits = Limits {
        max_evaluate_calls: 3,
        ..Limits::default()
    };
    let (outcome, _) = run_with(
        World {
            tool_spans: 25,
            ..World::default()
        },
        &opts,
        &limits,
    );
    let receipt = outcome.expect("the first trace fits");
    assert!(int(&receipt, "tracesScored") < int(&receipt, "tracesFetched"));
}

#[test]
fn a_shared_session_is_fetched_and_scored_once() {
    let mut opts = options("aa,bb");
    opts.evaluator_ids = Some("E1".to_string());
    opts.level = Level::Session;
    let (receipt, counters) = scored(
        World {
            session_id: Some("S1"),
            ..World::default()
        },
        &opts,
    );
    assert_eq!(counters.borrow().fetch, 1);
    // recorded-shape: test assertion over a receipt, request or fixture the test holds.
    let Some(Json::Array(results)) = receipt.get("results") else {
        panic!()
    };
    assert_eq!(results.len(), 1, "SESSION scored once, not per trace");
    assert!(counters
        .borrow()
        .sql
        .iter()
        .any(|sql| sql.contains("attributes['session.id'] = 'S1'")));
}

#[test]
fn a_real_omni_error_reaches_the_failure_receipt() {
    let (outcome, _) = run_with(
        World {
            omni_error: true,
            ..World::default()
        },
        &options("aa"),
        &Limits::default(),
    );
    let text = failure_text(&outcome.expect_err("no span could be fetched"));
    assert!(text.contains("no cloudwatchomni access"), "{text}");
    assert!(
        text.contains("no (trace, evaluator) pair produced a score"),
        "{text}"
    );
}

#[test]
fn no_live_client_fails_closed_with_a_receipt() {
    let outcome = evaluate::run(
        &mut NoLiveClient,
        &mut VirtualPacer::default(),
        &options("aa"),
        &Limits::default(),
        now(),
    );
    let text = failure_text(&outcome.expect_err("nothing can be fetched"));
    assert!(text.contains(NO_LIVE_CLIENT_MESSAGE), "{text}");
}

// ---- receipt privacy and writeback --------------------------------------

#[test]
fn explanations_stay_off_the_receipt_unless_requested() {
    let (plain, _) = scored(World::default(), &options("aa"));
    let text = pyfmt::dumps(&plain, false);
    assert!(text.contains(r#""hasExplanation": true"#), "{text}");
    assert!(
        !text.contains("why") && !text.contains("_explanation"),
        "{text}"
    );

    let mut asked = options("aa");
    asked.include_explanations = true;
    let (full, _) = scored(World::default(), &asked);
    let text = pyfmt::dumps(&full, false);
    assert!(text.contains(r#""explanation": "why""#), "{text}");
    assert!(
        !text.contains("hasExplanation") && !text.contains("_explanation"),
        "{text}"
    );
}

#[test]
fn long_explanations_are_clipped_at_600_characters() {
    let long = "x".repeat(700);
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = Fake::new({
        let mut inner = world_client(World::default(), &counters);
        let long = long.clone();
        move |service, operation, request| {
            use super::aws_client::AwsCalls;
            let response = inner.call(service, operation, request)?;
            if operation == "Evaluate" {
                return Ok(Json::object(vec![(
                    "evaluationResults",
                    Json::Array(vec![Json::object(vec![
                        ("evaluatorId", Json::string("E")),
                        ("value", Json::Int(1)),
                        ("explanation", Json::string(long.clone())),
                    ])]),
                )]));
            }
            Ok(response)
        }
    });
    let mut opts = options("aa");
    opts.include_explanations = true;
    let receipt = evaluate::run(
        &mut client,
        &mut VirtualPacer::default(),
        &opts,
        &Limits::default(),
        now(),
    )
    .expect("scored");
    let text = pyfmt::dumps(&receipt, false);
    assert!(
        text.contains(&format!("{}\\u2026", "x".repeat(600))),
        "clipped with an ellipsis"
    );
    assert!(!text.contains(&"x".repeat(601)));
}

fn writeback_options() -> Options {
    Options {
        no_writeback: false,
        ..options("aa")
    }
}

#[test]
fn writeback_persists_the_result_record_including_the_explanation() {
    let (receipt, counters) = scored(
        World {
            session_id: Some("S1"),
            ..World::default()
        },
        &writeback_options(),
    );
    assert_eq!(int(&receipt, "persistedTraces"), 1);
    assert_eq!(
        receipt.get("resultsLogGroupPrefix").and_then(Json::as_str),
        Some("/aws/cloudwatch/evaluations/results/")
    );
    assert_eq!(int(&receipt, "resultsRetentionDaysOnCreate"), 30);
    assert_eq!(
        receipt.get("resultsKmsKeyOnCreate").and_then(Json::as_str),
        Some("aws-owned")
    );
    let counters = counters.borrow();
    assert_eq!(counters.logs.len(), 1);
    let (operation, request) = &counters.logs[0];
    assert_eq!(operation, "PutLogEvents");
    assert_eq!(
        request.get("logGroupName").and_then(Json::as_str),
        Some("/aws/cloudwatch/evaluations/results/svc")
    );
    assert_eq!(
        request.get("logStreamName").and_then(Json::as_str),
        Some("on-demand")
    );
    // recorded-shape: test assertion over a receipt, request or fixture the test holds.
    let Some(Json::Array(events)) = request.get("logEvents") else {
        panic!()
    };
    let message = events[0].get("message").and_then(Json::as_str).unwrap();
    assert_eq!(
        message,
        concat!(
            r#"{"name": "gen_ai.evaluation.result", "traceId": "aa", "attributes": {"#,
            r#""gen_ai.evaluation.name": "Builtin.Helpfulness", "#,
            r#""gen_ai.evaluation.score.value": "0.5", "#,
            r#""aws.bedrock_agentcore.evaluation_level": "TRACE", "session.id": "S1", "#,
            r#""gen_ai.evaluation.score.label": "ok", "gen_ai.evaluation.explanation": "why"}, "#,
            r#""resource": {"attributes": {"service.name": "svc"}}, "@timestamp": 1738608120000}"#
        )
    );
    assert_eq!(
        events[0].get("timestamp"),
        Some(&Json::Int(1_738_608_120_000))
    );
}

#[test]
fn writeback_provisions_a_missing_group_bounded_and_encrypted() {
    let mut logs = HashMap::new();
    logs.insert(
        "PutLogEvents",
        vec![service_error("ResourceNotFoundException", "no group")],
    );
    let mut opts = writeback_options();
    opts.kms_key_id = Some("arn:aws:kms:us-east-1:111122223333:key/k".to_string());
    opts.retention_days = 90;
    let (receipt, counters) = scored(
        World {
            logs,
            ..World::default()
        },
        &opts,
    );
    let operations: Vec<String> = counters
        .borrow()
        .logs
        .iter()
        .map(|(op, _)| op.clone())
        .collect();
    assert_eq!(
        operations,
        [
            "PutLogEvents",
            "CreateLogGroup",
            "PutRetentionPolicy",
            "CreateLogStream",
            "PutLogEvents"
        ]
    );
    let counters = counters.borrow();
    assert_eq!(
        pyfmt::dumps(&counters.logs[1].1, false),
        concat!(
            r#"{"logGroupName": "/aws/cloudwatch/evaluations/results/svc", "#,
            r#""kmsKeyId": "arn:aws:kms:us-east-1:111122223333:key/k"}"#
        )
    );
    assert_eq!(
        pyfmt::dumps(&counters.logs[2].1, false),
        r#"{"logGroupName": "/aws/cloudwatch/evaluations/results/svc", "retentionInDays": 90}"#
    );
    assert_eq!(int(&receipt, "persistedTraces"), 1);
    assert_eq!(int(&receipt, "resultsRetentionDaysOnCreate"), 90);
    assert_eq!(
        receipt.get("resultsKmsKeyOnCreate").and_then(Json::as_str),
        Some("arn:aws:kms:us-east-1:111122223333:key/k")
    );
}

#[test]
fn writeback_without_permission_is_skipped_silently() {
    let mut logs = HashMap::new();
    logs.insert(
        "PutLogEvents",
        vec![service_error("AccessDeniedException", "no logs")],
    );
    let (receipt, _) = scored(
        World {
            logs,
            ..World::default()
        },
        &writeback_options(),
    );
    assert_eq!(int(&receipt, "persistedTraces"), 0);
    assert!(notes(&receipt).is_empty(), "{:?}", notes(&receipt));
    assert!(
        receipt.get("resultsLogGroupPrefix").is_none(),
        "nothing was persisted"
    );
}

#[test]
fn writeback_failures_are_notes_and_never_fail_the_run() {
    let mut logs = HashMap::new();
    logs.insert(
        "PutLogEvents",
        vec![service_error("ThrottlingException", "slow")],
    );
    let (receipt, _) = scored(
        World {
            logs,
            ..World::default()
        },
        &writeback_options(),
    );
    assert_eq!(int(&receipt, "persistedTraces"), 0);
    assert_eq!(int(&receipt, "pairsScored"), 1);
    assert!(notes(&receipt)[0].starts_with(
        "trace aa: could not persist the score for later querying: An error occurred (ThrottlingException)"
    ));

    let mut logs = HashMap::new();
    logs.insert(
        "PutLogEvents",
        vec![service_error("ResourceNotFoundException", "gone")],
    );
    logs.insert(
        "CreateLogGroup",
        vec![service_error("InvalidParameterException", "bad key")],
    );
    let mut opts = writeback_options();
    opts.kms_key_id = Some("k".to_string());
    let (receipt, _) = scored(
        World {
            logs,
            ..World::default()
        },
        &opts,
    );
    assert!(
        notes(&receipt)[0].contains("could not create /aws/cloudwatch/evaluations/results/svc with kmsKeyId k (does the key policy allow"),
        "{:?}",
        notes(&receipt)
    );

    let mut logs = HashMap::new();
    logs.insert(
        "PutLogEvents",
        vec![service_error("ResourceNotFoundException", "gone")],
    );
    logs.insert(
        "PutRetentionPolicy",
        vec![service_error("AccessDeniedException", "no")],
    );
    let (receipt, _) = scored(
        World {
            logs,
            ..World::default()
        },
        &writeback_options(),
    );
    assert_eq!(
        int(&receipt, "persistedTraces"),
        1,
        "the group exists, only retention failed"
    );
    assert!(notes(&receipt)[0].starts_with(
        "created /aws/cloudwatch/evaluations/results/svc but could not set 30-day retention"
    ));
}

#[test]
fn an_existing_group_keeps_its_retention_and_key() {
    let mut logs = HashMap::new();
    logs.insert(
        "PutLogEvents",
        vec![service_error("ResourceNotFoundException", "gone")],
    );
    logs.insert(
        "CreateLogGroup",
        vec![service_error("ResourceAlreadyExistsException", "there")],
    );
    let (receipt, counters) = scored(
        World {
            logs,
            ..World::default()
        },
        &writeback_options(),
    );
    let operations: Vec<String> = counters
        .borrow()
        .logs
        .iter()
        .map(|(op, _)| op.clone())
        .collect();
    assert_eq!(
        operations,
        [
            "PutLogEvents",
            "CreateLogGroup",
            "CreateLogStream",
            "PutLogEvents"
        ]
    );
    assert_eq!(int(&receipt, "persistedTraces"), 1);
}

#[test]
fn service_names_become_safe_single_log_group_segments() {
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = Fake::new({
        let mut inner = world_client(World::default(), &counters);
        move |service, operation, request| {
            use super::aws_client::AwsCalls;
            let mut response = inner.call(service, operation, request)?;
            if operation == "GetTelemetryQueryResults" {
                // recorded-shape: test assertion over a receipt, request or fixture the test holds.
                if let Some(Json::Array(rows)) = response.get("rows").cloned() {
                    let renamed = rows
                        .into_iter()
                        .map(|row| match row {
                            Json::Object(mut pairs) => {
                                for (key, value) in &mut pairs {
                                    if key == "svc_name" {
                                        *value = Json::string("my svc/\u{e9}\n");
                                    }
                                }
                                Json::Object(pairs)
                            }
                            other => other,
                        })
                        .collect();
                    response = Json::object(vec![
                        ("status", Json::string("Complete")),
                        ("rows", Json::Array(renamed)),
                    ]);
                }
            }
            Ok(response)
        }
    });
    let outcome = evaluate::run(
        &mut client,
        &mut VirtualPacer::default(),
        &writeback_options(),
        &Limits::default(),
        now(),
    );
    assert!(outcome.is_ok());
    let group = counters.borrow().logs[0]
        .1
        .get("logGroupName")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert_eq!(group, "/aws/cloudwatch/evaluations/results/my_svc/_");
}

// ---- argument validation ------------------------------------------------

#[test]
fn invalid_arguments_fail_before_any_call() {
    let cases: Vec<(Options, &str)> =
        vec![
        (Options::default(), "no trace ids given (use --trace-id or --trace-ids)"),
        (options("aa,zz"), "invalid trace id (expected hex): 'zz'"),
        (options(&"a".repeat(65)), "invalid trace id (expected hex):"),
        (
            Options { session_id: Some("S".to_string()), ..options("aa,bb") },
            "--session-id applies to a single trace; omit it when scoring multiple traces",
        ),
        (
            Options { session_id: Some("bad id".to_string()), ..options("aa") },
            "invalid session id: 'bad id'",
        ),
        (
            Options { session_id: Some("S\n".to_string()), ..options("aa") },
            "invalid session id: 'S\\\\n'",
        ),
        (
            Options { retention_days: 31, ..options("aa") },
            "invalid --retention-days 31; CloudWatch Logs accepts one of [1, 3, 5, 7, 14, 30, 60,",
        ),
    ];
    for (opts, expected) in cases {
        let (outcome, counters) = run_with(World::default(), &opts, &Limits::default());
        let text = failure_text(&outcome.expect_err("rejected"));
        assert!(text.contains(expected), "{expected}: {text}");
        assert_eq!(counters.borrow().fetch + counters.borrow().eval, 0);
        assert!(counters.borrow().sql.is_empty());
    }
    let (ok, _) = run_with(
        World::default(),
        &Options {
            retention_days: 0,
            ..options("aa")
        },
        &Limits::default(),
    );
    assert!(ok.is_ok(), "0 leaves the account default");
}

#[test]
fn ids_are_split_lowercased_and_de_duplicated() {
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = world_client(World::default(), &counters);
    let mut opts = options(" AA, bb ,aa,,");
    opts.trace_id = Some("BB".to_string());
    opts.evaluator_ids = Some("Builtin.B, Builtin.A,Builtin.B".to_string());
    let receipt = evaluate::run(
        &mut client,
        &mut VirtualPacer::default(),
        &opts,
        &Limits::default(),
        now(),
    )
    .unwrap();
    assert_eq!(counters.borrow().fetch, 2, "aa and bb, once each");
    assert_eq!(
        pyfmt::dumps(receipt.get("evaluators").unwrap(), false),
        r#"["Builtin.B", "Builtin.A"]"#
    );
}

#[test]
fn id_validators_accept_only_the_documented_shapes() {
    assert!(is_trace_id("6a82FF") && is_trace_id(&"f".repeat(64)));
    assert!(
        !is_trace_id("")
            && !is_trace_id("g")
            && !is_trace_id(&"f".repeat(65))
            && !is_trace_id("ab\n")
    );
    assert!(is_session_id("a.b_c:d-1") && is_session_id(&"s".repeat(128)));
    assert!(
        !is_session_id("")
            && !is_session_id("a b")
            && !is_session_id("a'b")
            && !is_session_id("a\n")
    );
}

// ---- span reshaping -----------------------------------------------------

#[test]
fn span_kinds_are_coerced_to_bare_otel_strings() {
    let cases: Vec<(Option<Json>, &str)> = vec![
        (None, "UNSPECIFIED"),
        (Some(Json::Null), "UNSPECIFIED"),
        (Some(Json::Bool(true)), "UNSPECIFIED"),
        (Some(Json::Int(2)), "SERVER"),
        (Some(Json::Int(99)), "UNSPECIFIED"),
        (Some(Json::Int(-1)), "UNSPECIFIED"),
        (Some(Json::Float(3.0)), "CLIENT"),
        (Some(Json::Float(3.5)), "UNSPECIFIED"),
        (Some(Json::string("5")), "CONSUMER"),
        (Some(Json::string(" 4 ")), "PRODUCER"),
        (Some(Json::string("")), "UNSPECIFIED"),
        (Some(Json::string("server")), "SERVER"),
        (Some(Json::string("SPAN_KIND_CLIENT")), "CLIENT"),
        (Some(Json::string("SpanKind.INTERNAL")), "INTERNAL"),
        (Some(Json::string("SPAN_KIND_BOGUS")), "UNSPECIFIED"),
        (Some(Json::string("\u{b2}")), "UNSPECIFIED"),
        // recorded-shape: test assertion over a receipt, request or fixture the test holds.
        (Some(Json::Array(Vec::new())), "UNSPECIFIED"),
    ];
    for (value, expected) in cases {
        assert_eq!(normalize_span_kind(value.as_ref()), expected, "{value:?}");
    }
}

#[test]
fn rows_become_spans_with_only_non_empty_attributes() {
    let row = json(
        r#"{"traceId": "t1", "spanId": "s1", "parentSpanId": "p1", "name": "",
            "kind": "SPAN_KIND_SERVER", "startTimeUnixNano": "10", "endTimeUnixNano": "",
            "svc_name": "svc", "scope_name": "lib", "status_code": "OK",
            "input_value": "hi", "output_value": "", "tool_name": null, "prompt_tokens": 7}"#,
    );
    assert_eq!(
        pyfmt::dumps(&row_to_span(&row), false),
        concat!(
            r#"{"traceId": "t1", "spanId": "s1", "name": "", "kind": "SERVER", "#,
            r#""attributes": {"input.value": "hi", "llm.token_count.prompt": 7}, "#,
            r#""resource": {"attributes": {"service.name": "svc"}}, "parentSpanId": "p1", "#,
            r#""startTimeUnixNano": "10", "scope": {"name": "lib"}, "status": {"code": "OK"}}"#
        )
    );
}

#[test]
fn unknown_evaluator_errors_get_the_discovery_mismatch_hint() {
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = Fake::new({
        let mut inner = world_client(World::default(), &counters);
        move |service, operation, request| {
            use super::aws_client::AwsCalls;
            if operation == "Evaluate" {
                return Err(service_error(
                    "ValidationException",
                    "Unknown evaluator provider",
                ));
            }
            inner.call(service, operation, request)
        }
    });
    let outcome = evaluate::run(
        &mut client,
        &mut VirtualPacer::default(),
        &options("aa"),
        &Limits::default(),
        now(),
    );
    let text = failure_text(&outcome.expect_err("nothing scored"));
    assert!(
        text.contains(
            "Unknown evaluator provider \\u2014 this evaluator is listed by list-evaluators"
        ),
        "{text}"
    );
}

// ---- dataset capture ----------------------------------------------------

fn capture_options(trace_ids: &str) -> dataset::Options {
    dataset::Options {
        trace_ids: trace_ids.to_string(),
        mode: Mode::Create,
        dataset_name: Some("regressions".to_string()),
        dataset_id: None,
        description: None,
        window_days: 30,
        confirmed: true,
    }
}

fn attr_row(
    trace: &str,
    span: &str,
    parent: Option<&str>,
    ts: &str,
    attrs: &[(&str, &str)],
) -> Json {
    let mut pairs: Vec<(&str, Json)> = vec![
        ("traceId", Json::string(trace)),
        ("spanId", Json::string(span)),
        ("parentSpanId", parent.map_or(Json::Null, Json::string)),
        ("startTimeUnixNano", Json::string(ts)),
    ];
    for (alias, value) in attrs {
        pairs.push((alias, Json::string(*value)));
    }
    Json::object(pairs)
}

/// A client answering the Omni query with `rows` and recording the control
/// plane calls.
fn capture_client(rows: Vec<Json>, datasets: Vec<Json>, example_count: Option<i64>) -> Fake {
    Fake::new(
        move |service, operation, request| match (service, operation) {
            (Service::CloudWatchOmni, "StartTelemetryQuerySession") => {
                Ok(json(r#"{"sessionId": "s"}"#))
            }
            (Service::CloudWatchOmni, "StartTelemetryQuery") => Ok(json(r#"{"queryId": "q"}"#)),
            (Service::CloudWatchOmni, "GetTelemetryQueryResults") => {
                if request.get("maxResults") == Some(&Json::Int(1)) {
                    Ok(json(r#"{"status": "Complete"}"#))
                } else {
                    Ok(Json::object(vec![("rows", Json::Array(rows.clone()))]))
                }
            }
            (Service::CloudWatchOmni, "StopTelemetryQuerySession") => Ok(json("{}")),
            (Service::BedrockAgentCoreControl, "ListDatasets") => {
                let (page, next) = if request.get("nextToken").is_some() {
                    (datasets.get(1..).unwrap_or_default().to_vec(), Json::Null)
                } else {
                    (
                        datasets.get(..1).unwrap_or_default().to_vec(),
                        Json::string("page-2"),
                    )
                };
                Ok(Json::object(vec![
                    ("datasets", Json::Array(page)),
                    ("nextToken", next),
                ]))
            }
            (Service::BedrockAgentCoreControl, "GetDataset") => Ok(example_count.map_or_else(
                || json("{}"),
                |count| Json::object(vec![("exampleCount", Json::Int(count))]),
            )),
            (Service::BedrockAgentCoreControl, "CreateDataset") => {
                Ok(json(r#"{"datasetId": "ds-1", "status": "CREATING"}"#))
            }
            (Service::BedrockAgentCoreControl, "AddDatasetExamples") => {
                Ok(json(r#"{"exampleIds": ["e1", "e2"]}"#))
            }
            other => panic!("unexpected call {other:?}"),
        },
    )
}

fn control_calls(client: &Fake) -> Vec<&str> {
    client
        .calls
        .iter()
        .filter(|(service, _, _)| *service == Service::BedrockAgentCoreControl)
        .map(|(_, operation, _)| operation.as_str())
        .collect()
}

#[test]
fn an_unconfirmed_write_is_refused_before_any_call() {
    let mut client = capture_client(Vec::new(), Vec::new(), None);
    let mut opts = capture_options("aa");
    opts.confirmed = false;
    let payload =
        dataset::run(&mut client, &mut VirtualPacer::default(), &opts).expect_err("refused");
    assert_eq!(payload.get("confirmationRequired"), Some(&Json::Bool(true)));
    assert!(payload
        .get("error")
        .and_then(Json::as_str)
        .unwrap()
        .starts_with("refusing to write:"));
    assert!(
        client.calls.is_empty(),
        "no AWS call before the confirmation"
    );

    let mut never = NoLiveClient;
    assert!(dataset::run(&mut never, &mut VirtualPacer::default(), &opts).is_err());
}

#[test]
fn capture_arguments_are_validated_before_any_call() {
    let cases: Vec<(dataset::Options, &str)> = vec![
        (
            dataset::Options {
                dataset_name: None,
                ..capture_options("aa")
            },
            "--dataset-name is required for --mode create",
        ),
        (
            dataset::Options {
                mode: Mode::Add,
                dataset_name: None,
                ..capture_options("aa")
            },
            "--dataset-id or --dataset-name is required for --mode add",
        ),
        (
            capture_options("aa,xyz,qq"),
            "invalid trace id(s) (expected hex): xyz, qq",
        ),
        (capture_options(" , ,"), "no valid trace ids provided"),
    ];
    for (opts, expected) in cases {
        let mut client = capture_client(Vec::new(), Vec::new(), None);
        let payload =
            dataset::run(&mut client, &mut VirtualPacer::default(), &opts).expect_err("rejected");
        assert_eq!(payload.get("error").and_then(Json::as_str), Some(expected));
        assert!(client.calls.is_empty());
    }
}

#[test]
fn examples_use_the_root_span_with_ordered_collapsed_tool_trajectory() {
    let spans = vec![
        json(
            r#"{"spanId": "c", "parentSpanId": "r", "ts": "30", "attributes": {"openinference.span.kind": "TOOL", "tool.name": "search", "output.value": "tool out"}}"#,
        ),
        json(
            r#"{"spanId": "r", "parentSpanId": null, "ts": "10", "attributes": {"input.value": "  hello  ", "session.id": "S9"}}"#,
        ),
        json(
            r#"{"spanId": "d", "parentSpanId": "r", "ts": "40", "attributes": {"aws.genai.span_kind": "TOOL", "tool.name": "search"}}"#,
        ),
        json(
            r#"{"spanId": "e", "parentSpanId": "r", "ts": "50", "attributes": {"openinference.span.kind": "TOOL", "tool.name": "fetch"}}"#,
        ),
        json(
            r#"{"spanId": "f", "parentSpanId": "r", "ts": "60", "attributes": {"output.value": "final answer"}}"#,
        ),
    ];
    let example = to_example("aa", &spans, false);
    assert_eq!(
        pyfmt::dumps(&example, false),
        concat!(
            r#"{"scenario_id": "aa", "turns": [{"input": "hello", "expected_response": "final answer"}], "#,
            r#""metadata": {"sourceTraceId": "aa", "sessionId": "S9"}, "#,
            r#""expected_trajectory": ["search", "fetch"]}"#
        )
    );
    let partial = to_example("aa", &spans[..2], true);
    assert!(pyfmt::dumps(&partial, false).contains(r#""partial": true"#));
}

#[test]
fn examples_without_a_root_or_input_are_reported() {
    let no_root = vec![
        json(
            r#"{"spanId": "a", "parentSpanId": "x", "ts": "2", "attributes": {"gen_ai.input_value": "second"}}"#,
        ),
        json(
            r#"{"spanId": "b", "parentSpanId": "x", "ts": "1", "attributes": {"gen_ai.input_value": "first"}}"#,
        ),
    ];
    let example = to_example("aa", &no_root, false);
    assert!(
        pyfmt::dumps(&example, false).contains(r#""input": "first""#),
        "first span by time becomes the root"
    );

    let empty = to_example(
        "aa",
        &[json(r#"{"spanId": "a", "ts": "1", "attributes": {}}"#)],
        false,
    );
    assert_eq!(
        invalid(&empty).as_deref(),
        Some("turn 1 missing a non-empty input")
    );
    assert_eq!(
        invalid(&json(r#"{"turns": []}"#)).as_deref(),
        Some("no turns")
    );
    assert_eq!(invalid(&json(r#"{"turns": [{"input": {"a": 1}}]}"#)), None);
    let huge = Json::object(vec![(
        "turns",
        Json::Array(vec![Json::object(vec![(
            "input",
            Json::string("x".repeat(MAX_EXAMPLE_BYTES + 1)),
        )])]),
    )]);
    assert!(
        invalid(&huge)
            .unwrap()
            .starts_with("example serializes to 1000"),
        "oversize examples are refused"
    );
}

#[test]
fn create_writes_one_dataset_with_the_built_examples() {
    let rows = vec![
        attr_row(
            "aa",
            "r",
            None,
            "1",
            &[("input_value", "q1"), ("output_value", "a1")],
        ),
        attr_row("bb", "r", None, "1", &[("input_value", "q2")]),
    ];
    let mut client = capture_client(rows, Vec::new(), None);
    let mut opts = capture_options("AA, bb");
    opts.description = Some("golden".to_string());
    let receipt = dataset::run(&mut client, &mut VirtualPacer::default(), &opts).expect("created");
    assert_eq!(
        pyfmt::dumps(&receipt, false),
        r#"{"mode": "create", "datasetId": "ds-1", "status": "CREATING", "examplesWritten": 2}"#
    );
    assert_eq!(control_calls(&client), ["CreateDataset"]);
    let create = client
        .calls
        .iter()
        .find(|c| c.1 == "CreateDataset")
        .unwrap();
    assert_eq!(
        create.2.get("datasetName").and_then(Json::as_str),
        Some("regressions")
    );
    assert_eq!(
        create.2.get("schemaType").and_then(Json::as_str),
        Some("AGENTCORE_EVALUATION_PREDEFINED_V1")
    );
    assert_eq!(
        create.2.get("description").and_then(Json::as_str),
        Some("golden")
    );
    let token = create.2.get("clientToken").and_then(Json::as_str).unwrap();
    assert_eq!(token.len(), 36);
    assert_eq!(token.as_bytes()[14], b'4', "version 4");
    let sql = client
        .calls
        .iter()
        .find(|c| c.1 == "StartTelemetryQuery")
        .unwrap()
        .2
        .get("queryString")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert!(sql.contains("WHERE traceId IN ('aa', 'bb')"), "{sql}");
    assert!(
        sql.ends_with("ORDER BY traceId ASC, startTimeUnixNano ASC LIMIT 1001"),
        "{sql}"
    );
}

#[test]
fn missing_traces_are_conversion_errors_not_failures() {
    let rows = vec![attr_row("aa", "r", None, "1", &[("input_value", "q1")])];
    let mut client = capture_client(rows, Vec::new(), None);
    let receipt = dataset::run(
        &mut client,
        &mut VirtualPacer::default(),
        &capture_options("aa,bb"),
    )
    .unwrap();
    assert_eq!(
        pyfmt::dumps(receipt.get("conversionErrors").unwrap(), false),
        r#"[{"traceId": "bb", "error": "no spans found in window"}]"#
    );
    assert_eq!(int(&receipt, "examplesWritten"), 1);

    let mut nothing = capture_client(Vec::new(), Vec::new(), None);
    let payload = dataset::run(
        &mut nothing,
        &mut VirtualPacer::default(),
        &capture_options("aa"),
    )
    .expect_err("no example");
    assert_eq!(
        payload.get("error").and_then(Json::as_str),
        Some("no examples could be built")
    );
    assert!(
        control_calls(&nothing).is_empty(),
        "nothing is written when no example was built"
    );
}

#[test]
fn more_than_25_traces_are_capped_with_a_note() {
    let ids: Vec<String> = (0..30).map(|i| format!("{i:02x}")).collect();
    let rows = vec![attr_row("00", "r", None, "1", &[("input_value", "q")])];
    let mut client = capture_client(rows, Vec::new(), None);
    let receipt = dataset::run(
        &mut client,
        &mut VirtualPacer::default(),
        &capture_options(&ids.join(",")),
    )
    .unwrap();
    assert_eq!(
        pyfmt::dumps(receipt.get("notes").unwrap(), false),
        r#"["captured the first 25 of 30 traces; run again for the rest"]"#
    );
    let sql = client
        .calls
        .iter()
        .find(|c| c.1 == "StartTelemetryQuery")
        .unwrap()
        .2
        .get("queryString")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert!(sql.contains("'18'") && !sql.contains("'19'"), "{sql}");
}

#[test]
fn a_row_cap_hit_flags_the_boundary_trace_and_the_unreached_ones() {
    let mut rows = vec![attr_row("aa", "r", None, "1", &[("input_value", "q1")])];
    for index in 0..1000 {
        rows.push(attr_row(
            "bb",
            &format!("s{index}"),
            if index == 0 { None } else { Some("s0") },
            "1",
            &[("input_value", "q2")],
        ));
    }
    assert_eq!(rows.len(), 1001);
    let mut client = capture_client(rows, Vec::new(), None);
    let receipt = dataset::run(
        &mut client,
        &mut VirtualPacer::default(),
        &capture_options("aa,bb,cc"),
    )
    .unwrap();
    let text = pyfmt::dumps(&receipt, false);
    assert!(
        text.contains(
            r#"{"traceId": "cc", "error": "row cap (1000) reached before this trace; not fetched"}"#
        ),
        "{text}"
    );
    assert!(text.contains("hit the 1000-row cap"), "{text}");
    let create = client
        .calls
        .iter()
        .find(|c| c.1 == "CreateDataset")
        .unwrap();
    let examples = pyfmt::dumps(&create.2, false);
    assert!(
        examples.contains(r#""metadata": {"sourceTraceId": "bb", "partial": true}"#),
        "{examples}"
    );
    assert!(!examples.contains(r#""sourceTraceId": "aa", "partial""#));
}

#[test]
fn add_resolves_the_dataset_by_name_across_pages_and_enforces_the_cap() {
    let datasets = vec![
        json(r#"{"datasetId": "ds-a", "datasetName": "other"}"#),
        json(r#"{"datasetId": "ds-b", "datasetName": "target"}"#),
    ];
    let rows = vec![attr_row("aa", "r", None, "1", &[("input_value", "q1")])];
    let mut opts = capture_options("aa");
    opts.mode = Mode::Add;
    opts.dataset_name = None;
    opts.dataset_id = Some("target".to_string());

    let mut client = capture_client(rows.clone(), datasets.clone(), Some(10));
    let receipt = dataset::run(&mut client, &mut VirtualPacer::default(), &opts).unwrap();
    assert_eq!(
        pyfmt::dumps(&receipt, false),
        r#"{"mode": "add", "datasetId": "ds-b", "examplesWritten": 2, "exampleIds": ["e1", "e2"]}"#
    );
    assert_eq!(
        control_calls(&client),
        [
            "ListDatasets",
            "ListDatasets",
            "GetDataset",
            "AddDatasetExamples"
        ]
    );

    let mut full = capture_client(rows.clone(), datasets.clone(), Some(1000));
    let payload =
        dataset::run(&mut full, &mut VirtualPacer::default(), &opts).expect_err("over the cap");
    assert!(payload
        .get("error")
        .and_then(Json::as_str)
        .unwrap()
        .starts_with("dataset ds-b already holds 1000 examples; adding 1 would exceed"));
    assert!(!control_calls(&full).contains(&"AddDatasetExamples"));

    let mut unknown_count = capture_client(rows.clone(), datasets.clone(), None);
    assert!(
        dataset::run(&mut unknown_count, &mut VirtualPacer::default(), &opts).is_ok(),
        "an unreadable count must not block the write"
    );

    let mut missing = capture_client(rows, datasets, Some(0));
    opts.dataset_id = Some("nope".to_string());
    let payload =
        dataset::run(&mut missing, &mut VirtualPacer::default(), &opts).expect_err("not found");
    assert_eq!(
        payload.get("error").and_then(Json::as_str),
        Some("dataset 'nope' not found")
    );
}

#[test]
fn write_failures_keep_the_conversion_errors_in_the_failure_receipt() {
    let rows = vec![attr_row("aa", "r", None, "1", &[("input_value", "q1")])];
    let mut client = Fake::new({
        let mut inner = capture_client(rows, Vec::new(), None);
        move |service, operation, request| {
            use super::aws_client::AwsCalls;
            if operation == "CreateDataset" {
                return Err(service_error("ConflictException", "exists"));
            }
            inner.call(service, operation, request)
        }
    });
    let payload = dataset::run(
        &mut client,
        &mut VirtualPacer::default(),
        &capture_options("aa,bb"),
    )
    .expect_err("failed");
    let text = pyfmt::dumps(&payload, false);
    assert!(text.starts_with(r#"{"error": "create_dataset failed: An error occurred (ConflictException) when calling the CreateDataset operation: exists", "conversionErrors": [{"traceId": "bb""#), "{text}");
}

#[test]
fn an_incomplete_or_failing_omni_query_ends_the_capture() {
    let mut client = Fake::new(|service, operation, _| match (service, operation) {
        (Service::CloudWatchOmni, "StartTelemetryQuerySession") => {
            Ok(json(r#"{"sessionId": "s"}"#))
        }
        (Service::CloudWatchOmni, "StartTelemetryQuery") => Ok(json(r#"{"queryId": "q"}"#)),
        (Service::CloudWatchOmni, "GetTelemetryQueryResults") => {
            Ok(json(r#"{"status": "Failed"}"#))
        }
        _ => Ok(json("{}")),
    });
    let payload = dataset::run(
        &mut client,
        &mut VirtualPacer::default(),
        &capture_options("aa"),
    )
    .expect_err("failed query");
    assert_eq!(
        payload.get("error").and_then(Json::as_str),
        Some("Omni SQL query did not complete (status=Failed); reduce --trace-ids or narrow --window-days")
    );
    assert!(
        client
            .calls
            .iter()
            .any(|c| c.1 == "StopTelemetryQuerySession"),
        "the session is stopped on every path"
    );

    // An unrecognised status is a response value: the diagnostic never
    // echoes it (#4784).
    let mut client = Fake::new(|service, operation, _| match (service, operation) {
        (Service::CloudWatchOmni, "StartTelemetryQuerySession") => {
            Ok(json(r#"{"sessionId": "s"}"#))
        }
        (Service::CloudWatchOmni, "StartTelemetryQuery") => Ok(json(r#"{"queryId": "q"}"#)),
        (Service::CloudWatchOmni, "GetTelemetryQueryResults") => {
            Ok(json(r#"{"status": "SYNTHETIC_STATUS_SENTINEL"}"#))
        }
        _ => Ok(json("{}")),
    });
    let payload = dataset::run(
        &mut client,
        &mut VirtualPacer::default(),
        &capture_options("aa"),
    )
    .expect_err("unrecognised status");
    assert_eq!(
        payload.get("error").and_then(Json::as_str),
        Some(
            "Omni SQL query did not complete (status=<unrecognised>); reduce --trace-ids or \
             narrow --window-days"
        )
    );
}

// ---- malformed Evaluate results are never a score (#4758) ---------------

/// A world whose `Evaluate` answers `evaluationResults` per evaluator, with
/// writeback on, so a persisted record would show in the Logs calls.
fn evaluate_world(
    results: impl Fn(&str) -> Json + 'static,
    counters: &Rc<RefCell<Counters>>,
) -> Fake {
    let mut inner = world_client(World::default(), counters);
    Fake::new(move |service, operation, request| {
        use super::aws_client::AwsCalls;
        if operation == "Evaluate" {
            let evaluator = request
                .get("evaluatorId")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string();
            return Ok(results(&evaluator));
        }
        inner.call(service, operation, request)
    })
}

fn evaluate_with(client: &mut Fake, evaluators: &str) -> Result<Json, Json> {
    let mut opts = options("aa");
    opts.no_writeback = false;
    opts.evaluator_ids = Some(evaluators.to_string());
    evaluate::run(
        client,
        &mut VirtualPacer::default(),
        &opts,
        &Limits::default(),
        now(),
    )
}

#[test]
fn a_wrong_typed_score_is_a_score_error_never_a_scored_pair_or_a_record() {
    for value in ["{}", "[]", r#""0.5""#, "true", r#"{"score": 1}"#] {
        let counters = Rc::new(RefCell::new(Counters::default()));
        let mut client = evaluate_world(
            move |evaluator| {
                json(&format!(
                    r#"{{"evaluationResults": [{{"evaluatorId": "{evaluator}", "value": {value}, "explanation": ""}}]}}"#
                ))
            },
            &counters,
        );
        let payload = evaluate_with(&mut client, "Builtin.Helpfulness").expect_err(value);
        let text = failure_text(&payload);
        assert!(
            text.contains("no (trace, evaluator) pair produced a score"),
            "{value}: {text}"
        );
        assert!(
            text.contains(
                "the member `evaluationResults[0].value` of the response is not a number or null"
            ),
            "{value}: {text}"
        );
        assert!(!text.contains("pairsScored"), "{value}: {text}");
        assert!(
            counters.borrow().logs.is_empty(),
            "{value}: nothing is written back"
        );
    }
}

#[test]
fn a_malformed_evaluate_envelope_or_member_is_a_score_error() {
    for response in [
        r#"{"evaluationResults": null}"#,
        r#"{"evaluationResults": false}"#,
        r#"{"evaluationResults": {}}"#,
        r#"{"evaluationResults": "x"}"#,
        r#"{"evaluationResults": [1]}"#,
        r#"{"evaluationResults": [{"value": 1, "label": null}]}"#,
        r#"{"evaluationResults": [{"value": 1, "evaluatorId": 5}]}"#,
        r#"{"evaluationResults": [{"value": 1, "explanation": {}}]}"#,
        r#"{"evaluationResults": [{"value": 1, "errorCode": false}]}"#,
    ] {
        let counters = Rc::new(RefCell::new(Counters::default()));
        let mut client = evaluate_world(move |_| json(response), &counters);
        let payload = evaluate_with(&mut client, "Builtin.Helpfulness").expect_err(response);
        let text = failure_text(&payload);
        assert!(
            text.contains("evaluate failed: the Evaluate response is invalid"),
            "{response}: {text}"
        );
        assert!(counters.borrow().logs.is_empty(), "{response}");
    }
}

#[test]
fn mixed_validity_runs_count_and_persist_only_the_valid_scores() {
    let counters = Rc::new(RefCell::new(Counters::default()));
    let mut client = evaluate_world(
        |evaluator| {
            if evaluator == "Builtin.Correctness" {
                json(
                    r#"{"evaluationResults": [{"evaluatorId": "Builtin.Correctness", "value": {}}]}"#,
                )
            } else {
                // Numeric zero is a real score.
                json(
                    r#"{"evaluationResults": [{"evaluatorId": "Builtin.Helpfulness", "value": 0, "label": "Not helpful"}]}"#,
                )
            }
        },
        &counters,
    );
    let receipt = evaluate_with(&mut client, "Builtin.Helpfulness,Builtin.Correctness")
        .expect("one pair scored");
    assert_eq!(int(&receipt, "pairsScored"), 1);
    assert_eq!(int(&receipt, "tracesScored"), 1);
    let text = pyfmt::dumps(&receipt, false);
    assert!(
        text.contains(r#""evaluatorId": "Builtin.Correctness", "error": "evaluate failed: the Evaluate response is invalid"#),
        "{text}"
    );
    assert!(text.contains(r#""value": 0"#), "{text}");
    assert!(!text.contains(r#""value": {}"#), "{text}");
    let logs = counters.borrow();
    let put: Vec<String> = logs
        .logs
        .iter()
        .filter(|(operation, _)| operation == "PutLogEvents")
        .map(|(_, request)| pyfmt::dumps(request, false))
        .collect();
    assert!(!put.is_empty(), "the valid score is written back");
    assert!(
        put.iter()
            .all(|request| !request.contains("Builtin.Correctness")),
        "{put:?}"
    );
}

#[test]
fn an_error_row_without_a_score_stays_a_score_error_not_a_scored_pair() {
    for value in ["null", "absent"] {
        let counters = Rc::new(RefCell::new(Counters::default()));
        let member = if value == "null" {
            r#""value": null, "#
        } else {
            ""
        };
        let response = format!(
            r#"{{"evaluationResults": [{{"evaluatorId": "E", {member}"errorCode": "AgentSpanMappingException", "errorMessage": "boom"}}]}}"#
        );
        let mut client = evaluate_world(move |_| json(&response), &counters);
        let receipt = evaluate_with(&mut client, "E").expect("error row kept");
        assert_eq!(int(&receipt, "pairsScored"), 0, "{value}");
        assert_eq!(int(&receipt, "tracesScored"), 0, "{value}");
    }
}

// ---- malformed dataset responses never become a write (#4755) -----------

/// `capture_client` with the control-plane `operation` answered by `answer`.
fn capture_override(
    rows: Vec<Json>,
    datasets: Vec<Json>,
    example_count: Option<i64>,
    operation: &'static str,
    answer: &'static str,
) -> Fake {
    let mut inner = capture_client(rows, datasets, example_count);
    Fake::new(move |service, called, request| {
        use super::aws_client::AwsCalls;
        if called == operation {
            return Ok(json(answer));
        }
        inner.call(service, called, request)
    })
}

fn one_row() -> Vec<Json> {
    vec![attr_row(
        "aa",
        "r",
        None,
        "1",
        &[("input_value", "q1"), ("output_value", "a1")],
    )]
}

fn add_options() -> dataset::Options {
    let mut opts = capture_options("aa,bb");
    opts.mode = Mode::Add;
    opts.dataset_name = None;
    opts.dataset_id = Some("target".to_string());
    opts
}

fn target_datasets() -> Vec<Json> {
    vec![json(r#"{"datasetId": "ds-b", "datasetName": "target"}"#)]
}

fn assert_no_write_receipt(payload: &Json, what: &str) {
    let text = failure_text(payload);
    assert!(!text.contains("examplesWritten"), "{what}: {text}");
    assert!(
        text.contains(r#""conversionErrors": [{"traceId": "bb""#),
        "{what}: the conversion errors are kept: {text}"
    );
}

#[test]
fn a_malformed_create_response_is_never_a_write_receipt() {
    for answer in [
        r#"{"datasetId": null, "status": false}"#,
        r#"{"datasetId": null, "status": "CREATING"}"#,
        r#"{"datasetId": 5, "status": "CREATING"}"#,
        r#"{"datasetId": {}, "status": "CREATING"}"#,
        r#"{"datasetId": "ds-1", "status": false}"#,
        r#"{"datasetId": "ds-1", "status": null}"#,
        r#"{"datasetId": "ds-1", "status": ["CREATING"]}"#,
        r#"{"status": "CREATING"}"#,
        r#"{"datasetId": "", "status": "CREATING"}"#,
    ] {
        let mut client = capture_override(one_row(), Vec::new(), None, "CreateDataset", answer);
        let payload = dataset::run(
            &mut client,
            &mut VirtualPacer::default(),
            &capture_options("aa,bb"),
        )
        .expect_err(answer);
        assert!(
            payload
                .get("error")
                .and_then(Json::as_str)
                .is_some_and(|e| e
                    .starts_with("create_dataset failed: the CreateDataset response is invalid")),
            "{answer}: {}",
            failure_text(&payload)
        );
        assert_no_write_receipt(&payload, answer);
    }
}

#[test]
fn malformed_example_ids_are_never_a_write_count() {
    for answer in [
        r#"{"exampleIds": null}"#,
        r#"{"exampleIds": false}"#,
        r#"{"exampleIds": {}}"#,
        r#"{"exampleIds": "e1"}"#,
        r#"{"exampleIds": [1]}"#,
        r#"{"exampleIds": ["e1", null]}"#,
    ] {
        let mut client = capture_override(
            one_row(),
            target_datasets(),
            Some(1),
            "AddDatasetExamples",
            answer,
        );
        let payload = dataset::run(&mut client, &mut VirtualPacer::default(), &add_options())
            .expect_err(answer);
        assert!(
            payload
                .get("error")
                .and_then(Json::as_str)
                .is_some_and(|e| e.starts_with(
                    "add_dataset_examples failed: the AddDatasetExamples response is invalid"
                )),
            "{answer}: {}",
            failure_text(&payload)
        );
        assert_no_write_receipt(&payload, answer);
    }
    // Absent ids keep the script's fallback to the number of examples sent.
    let mut client = capture_override(
        one_row(),
        target_datasets(),
        Some(1),
        "AddDatasetExamples",
        "{}",
    );
    let receipt =
        dataset::run(&mut client, &mut VirtualPacer::default(), &add_options()).expect("written");
    assert_eq!(int(&receipt, "examplesWritten"), 1);
}

#[test]
fn malformed_listings_and_counts_stop_the_add_before_any_write() {
    for (operation, answer) in [
        ("ListDatasets", r#"{"datasets": null}"#),
        ("ListDatasets", r#"{"datasets": false}"#),
        (
            "ListDatasets",
            r#"{"datasets": [{"datasetId": null, "datasetName": "target"}]}"#,
        ),
        (
            "ListDatasets",
            r#"{"datasets": [{"datasetId": "ds-b", "datasetName": 3}]}"#,
        ),
        ("ListDatasets", r#"{"datasets": [], "nextToken": 4}"#),
        ("GetDataset", r#"{"exampleCount": null}"#),
        ("GetDataset", r#"{"exampleCount": "10"}"#),
        ("GetDataset", r#"{"exampleCount": 1.5}"#),
        ("GetDataset", "[]"),
    ] {
        let mut client = capture_override(one_row(), target_datasets(), None, operation, answer);
        let payload = dataset::run(&mut client, &mut VirtualPacer::default(), &add_options())
            .expect_err(answer);
        assert!(
            failure_text(&payload).contains("response is"),
            "{operation} {answer}: {}",
            failure_text(&payload)
        );
        assert!(
            !control_calls(&client).contains(&"AddDatasetExamples"),
            "{operation} {answer}: no write"
        );
        // #4785: the resolver's failure keeps bb's conversion error too.
        assert_no_write_receipt(&payload, &format!("{operation} {answer}"));
    }
}

/// #4785: every way the dataset resolver can fail (a service error, a
/// malformed first or later page, or no such dataset) is a failure receipt
/// that names the listing failure and keeps the conversion errors already
/// collected, never a partial success.
#[test]
fn dataset_resolver_failures_keep_the_conversion_errors() {
    let pages = |first: &'static str, second: &'static str| {
        let mut inner = capture_client(one_row(), target_datasets(), None);
        Fake::new(move |service, called, request| {
            use super::aws_client::AwsCalls;
            if called == "ListDatasets" {
                return Ok(json(if request.get("nextToken").is_some() {
                    second
                } else {
                    first
                }));
            }
            inner.call(service, called, request)
        })
    };
    let cases: Vec<(&str, Fake, &str)> = vec![
        (
            "service error",
            {
                let mut inner = capture_client(one_row(), target_datasets(), None);
                Fake::new(move |service, called, request| {
                    use super::aws_client::AwsCalls;
                    if called == "ListDatasets" {
                        return Err(service_error("AccessDeniedException", "denied"));
                    }
                    inner.call(service, called, request)
                })
            },
            "list_datasets failed: An error occurred (AccessDeniedException)",
        ),
        (
            "malformed first page",
            pages(r#"{"datasets": null}"#, "{}"),
            "list_datasets failed: the ListDatasets response is invalid",
        ),
        (
            "malformed later page",
            pages(
                r#"{"datasets": [], "nextToken": "p2"}"#,
                r#"{"datasets": [{"datasetId": 7, "datasetName": "target"}]}"#,
            ),
            "list_datasets failed: the ListDatasets response is invalid",
        ),
        (
            "bad later token",
            pages(
                r#"{"datasets": [], "nextToken": "p2"}"#,
                r#"{"datasets": [], "nextToken": []}"#,
            ),
            "list_datasets failed:",
        ),
        (
            "not an object",
            pages("[]", "{}"),
            "list_datasets failed: the ListDatasets response is not an object",
        ),
        (
            "no such dataset",
            pages(
                r#"{"datasets": [{"datasetId": "x", "datasetName": "y"}]}"#,
                "{}",
            ),
            "dataset 'target' not found",
        ),
    ];
    for (what, mut client, expected) in cases {
        let payload = dataset::run(&mut client, &mut VirtualPacer::default(), &add_options())
            .expect_err(what);
        let error = payload.get("error").and_then(Json::as_str).unwrap_or("");
        assert!(
            error.starts_with(expected),
            "{what}: {}",
            failure_text(&payload)
        );
        assert_no_write_receipt(&payload, what);
        assert_eq!(
            pyfmt::dumps(payload.get("conversionErrors").unwrap(), false),
            r#"[{"traceId": "bb", "error": "no spans found in window"}]"#,
            "{what}"
        );
        assert!(
            !control_calls(&client).contains(&"AddDatasetExamples"),
            "{what}: no write"
        );
    }
}
