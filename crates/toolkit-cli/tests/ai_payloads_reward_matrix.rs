//! Contract cases AP-RW-01 through AP-RW-27; no service or Python execution.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use serde_json::{json, Value};
use std::{
    fs,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

const FORMATS: [&str; 6] = [
    "openai",
    "verl",
    "hf-prompt-completion",
    "hf-preference",
    "sagemaker-eval",
    "nova",
];
const GOLDENS: [&str; 6] = [
    r#"[{"id":"123","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"What is the capital of France?"},{"role":"assistant","content":"Paris"},{"role":"assistant","content":"The capital of France is Paris."}],"reference_answer":{"text":"Paris"}}]"#,
    r#"[{"data_source":"openai/gsm8k","prompt":[{"role":"user","content":"What is the capital of France?"},{"role":"assistant","content":"The capital of France is Paris."}],"response":"The capital of France is Paris.","reward_model":{"style":"rule","ground_truth":"Paris"},"extra_info":{"reference_answer":{"text":"Paris"},"processor_config":{"aggregation":"mean"}}}]"#,
    r#"[{"id":"123","prompt":"What is the capital of France?","completion":"The capital of France is Paris.","reference_answer":{"text":"Paris"}}]"#,
    r#"[{"id":"123","prompt":"What is the capital of France?","completion":"The capital of France is Paris.","chosen":"Paris","rejected":"London","reference_answer":{"text":"Paris"}}]"#,
    r#"[{"id":"123","model_response":"The capital of France is Paris.","query":"What is the capital of France?","response":"Paris","system":"You are a helpful assistant.","reference_answer":{"text":"Paris"}}]"#,
    r#"{"id":"123","messages":[{"role":"user","content":"Do you have a dedicated security team?"},{"role":"assistant","content":"As an AI developed by Amazon, I don not have a dedicated security team..."}],"reference_answer":{"compliant":"No","explanation":"As an AI developed by Company, I do not have a traditional security team..."}}"#,
];
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn sample(f: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["ai-payloads", "reward", "sample", "--format", f])
        .output()
        .unwrap()
}
fn check(f: &str, event: &str, response: &str, batch: bool) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "reward-matrix-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    let e = dir.join("event");
    let r = dir.join("response");
    fs::write(&e, event).unwrap();
    fs::write(&r, response).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_toolkit"));
    c.args(["ai-payloads", "reward", "check", "--format", f, "--event"])
        .arg(&e)
        .arg("--response")
        .arg(&r);
    if batch {
        c.arg("--batch");
    }
    let out = c.output().unwrap();
    fs::remove_dir_all(dir).unwrap();
    out
}
fn assert_out(o: Output, code: i32, needle: &str) {
    let stdout = String::from_utf8(o.stdout).unwrap();
    let stderr = String::from_utf8(o.stderr).unwrap();
    assert_eq!(
        o.status.code(),
        Some(code),
        "stdout={stdout}, stderr={stderr}"
    );
    assert!(
        format!("{stdout}{stderr}").contains(needle),
        "expected {needle:?}: stdout={stdout}, stderr={stderr}"
    );
}
fn response(id: &str) -> String {
    json!([{"id":id,"aggregate_reward_score":1}]).to_string()
}
fn event(i: usize) -> Value {
    serde_json::from_str(GOLDENS[i]).unwrap()
}
fn array(i: usize) -> Value {
    let v = event(i);
    if v.is_array() {
        v
    } else {
        json!([v])
    }
}

#[test]
fn ap_rw_01_golden_samples() {
    for (i, f) in FORMATS.iter().enumerate() {
        let o = sample(f);
        assert_eq!(o.status.code(), Some(0));
        assert_eq!(
            String::from_utf8(o.stdout).unwrap(),
            format!("{}\n", GOLDENS[i])
        );
    }
}
#[test]
fn ap_rw_02_usage() {
    assert_eq!(sample("unknown").status.code(), Some(2));
    let o = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["ai-payloads", "reward", "check"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}
#[test]
fn ap_rw_03_10_11_round_trips() {
    for (i, f) in FORMATS.iter().enumerate() {
        let a = response(if *f == "verl" { "arbitrary" } else { "123" });
        let b = json!({"statusCode":200,"body":a}).to_string();
        for (r, env) in [(a.as_str(), "A"), (b.as_str(), "B")] {
            assert_out(
                check(f, GOLDENS[i], r, false),
                0,
                &format!("ok: {f}, 1 sample(s), 1 result(s), envelope {env}"),
            );
        }
    }
}
// Each row is a source golden and each column is a target format.
const MISSING: [[&[&str]; 6]; 6] = [
    [
        &[],
        &[
            "data_source",
            "response",
            "prompt",
            "extra_info.reference_answer.text",
        ],
        &["prompt", "completion"],
        &["prompt", "completion", "chosen", "rejected"],
        &["model_response", "query", "response"],
        &[],
    ],
    [
        &["id", "messages", "reference_answer.text"],
        &[],
        &["id", "prompt", "completion", "reference_answer.text"],
        &[
            "id",
            "prompt",
            "completion",
            "chosen",
            "rejected",
            "reference_answer.text",
        ],
        &["id", "model_response", "query", "reference_answer.text"],
        &["id", "messages"],
    ],
    [
        &["messages"],
        &[
            "data_source",
            "response",
            "prompt",
            "extra_info.reference_answer.text",
        ],
        &[],
        &["chosen", "rejected"],
        &["model_response", "query", "response"],
        &["messages"],
    ],
    [
        &["messages"],
        &[
            "data_source",
            "response",
            "prompt",
            "extra_info.reference_answer.text",
        ],
        &[],
        &[],
        &["model_response", "query", "response"],
        &["messages"],
    ],
    [
        &["messages"],
        &["data_source", "prompt", "extra_info.reference_answer.text"],
        &["prompt", "completion"],
        &["prompt", "completion", "chosen", "rejected"],
        &[],
        &["messages"],
    ],
    [
        &["reference_answer.text"],
        &[
            "data_source",
            "response",
            "prompt",
            "extra_info.reference_answer.text",
        ],
        &["prompt", "completion", "reference_answer.text"],
        &[
            "prompt",
            "completion",
            "chosen",
            "rejected",
            "reference_answer.text",
        ],
        &[
            "model_response",
            "query",
            "response",
            "reference_answer.text",
        ],
        &[],
    ],
];

#[test]
fn ap_rw_04_cross_format_matrix() {
    for (i, source) in FORMATS.iter().enumerate() {
        for (j, target) in FORMATS.iter().enumerate() {
            if i == j {
                continue;
            }
            let e = array(i).to_string();
            let ok = (*source == "hf-preference" && *target == "hf-prompt-completion")
                || (*source == "openai" && *target == "nova");
            let o = check(target, &e, &response("123"), false);
            if ok {
                assert_out(o, 0, "ok:");
            } else {
                let output = String::from_utf8_lossy(&o.stderr);
                assert_eq!(o.status.code(), Some(1), "{source} -> {target}: {output}");
                let mut actual: Vec<_> = output
                    .lines()
                    .filter_map(|line| {
                        let start = line.find("event[0].")?;
                        Some(line[start + "event[0].".len()..].split(':').next().unwrap())
                    })
                    .collect();
                let mut expected = MISSING[i][j].to_vec();
                actual.sort_unstable();
                expected.sort_unstable();
                assert_eq!(actual, expected, "{source} -> {target}: {output}");
            }
        }
    }
}
#[test]
fn ap_rw_12_13_14_15_envelope_failures() {
    let e = GOLDENS[0];
    for (r, n) in [
        (
            r#"{"statusCode":200,"body":[]}"#,
            "body: expected JSON string",
        ),
        (
            r#"{"error":"secret-sentinel"}"#,
            "matches neither documented envelope (C10 Option A/B)",
        ),
        (
            r#"{"statusCode":400,"body":"[]"}"#,
            "statusCode: expected integer 200",
        ),
        (
            r#"{"statusCode":500,"body":"[]"}"#,
            "statusCode: expected integer 200",
        ),
        ("[]", "result count differs"),
    ] {
        assert_out(check("openai", e, r, false), 1, n);
    }
}
#[test]
fn ap_rw_16_matching_permutations_and_missing_ids() {
    let mut a = array(0);
    let mut second = a[0].clone();
    second["id"] = json!("456");
    a.as_array_mut().unwrap().push(second);
    let e = a.to_string();
    let r =
        json!([{"id":"456","aggregate_reward_score":1},{"id":"123","aggregate_reward_score":2}])
            .to_string();
    assert_out(check("openai", &e, &r, true), 0, "2 sample(s), 2 result(s)");
    for (r, n) in [
        (
            json!([{"id":"123","aggregate_reward_score":1},{"id":"123","aggregate_reward_score":1}]),
            "body[1].id: duplicate",
        ),
        (
            json!([{"id":"123","aggregate_reward_score":1},{"id":"unknown","aggregate_reward_score":1}]),
            "body[1].id: no matching sample",
        ),
        (
            json!([{"id":"123","aggregate_reward_score":1},{"aggregate_reward_score":1}]),
            "body[1].id: expected string",
        ),
    ] {
        assert_out(check("openai", &e, &r.to_string(), true), 1, n);
    }
    assert_out(
        check("verl", GOLDENS[1], &response("free"), false),
        0,
        "id not compared (verl sample without id)",
    );
    assert_out(
        check(
            "verl",
            GOLDENS[1],
            r#"[{"aggregate_reward_score":1}]"#,
            false,
        ),
        1,
        "body[0].id: expected string",
    );
}
#[test]
fn ap_rw_17_metrics_and_scores() {
    for (r, n) in [
        (
            r#"[{"id":"123","aggregate_reward_score":1,"metrics_list":[{"name":"m","value":1,"type":"bad"}]}]"#,
            "body[0].metrics_list[0].type",
        ),
        (
            r#"[{"id":"123","aggregate_reward_score":1,"metrics_list":[{"value":1,"type":"Metric"}]}]"#,
            "body[0].metrics_list[0].name",
        ),
        (
            r#"[{"id":"123","aggregate_reward_score":1,"metrics_list":[{"name":"m","value":"NaN","type":"Reward"}]}]"#,
            "body[0].metrics_list[0].value",
        ),
        (
            r#"[{"id":"123","aggregate_reward_score":"Infinity"}]"#,
            "body[0].aggregate_reward_score",
        ),
        (
            r#"[{"id":"123","aggregate_reward_score":1,"metrics_list":{}}]"#,
            "body[0].metrics_list: expected array",
        ),
    ] {
        assert_out(check("openai", GOLDENS[0], r, false), 1, n);
    }
    let r = r#"[{"id":"123","aggregate_reward_score":1,"metrics_list":[{"name":"m","value":0.5,"type":"Reward"},{"name":"n","value":-1,"type":"Metric"}]}]"#;
    assert_out(check("openai", GOLDENS[0], r, false), 0, "envelope A");
}
#[test]
fn ap_rw_17_raw_nonfinite_numbers_are_rejected() {
    for token in ["NaN", "Infinity", "-Infinity", "1e999"] {
        for metric in [false, true] {
            let body = if metric {
                format!(
                    r#"[{{"id":"123","aggregate_reward_score":1,"metrics_list":[{{"name":"secret-sentinel","value":{token},"type":"Reward"}}]}}]"#
                )
            } else {
                format!(
                    r#"[{{"id":"123","aggregate_reward_score":{token},"marker":"secret-sentinel"}}]"#
                )
            };
            for (envelope_b, response) in [
                body.clone(),
                json!({"statusCode":200,"body":body}).to_string(),
            ]
            .into_iter()
            .enumerate()
            {
                let out = check("openai", GOLDENS[0], &response, false);
                let output = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(
                    out.status.code(),
                    Some(1),
                    "{token} metric={metric}: {output}"
                );
                assert!(!output.contains("ok:"), "{output}");
                assert!(!output.contains("secret-sentinel"), "{output}");
                let reason = if envelope_b == 0 {
                    "invalid JSON in --response"
                } else {
                    "body: expected JSON string containing a result list"
                };
                assert!(output.contains(reason), "{token} metric={metric}: {output}");
            }
        }
    }
    let body = r#"[{"id":"123","aggregate_reward_score":1e2,"metrics_list":[{"name":"m","value":-2.5e-2,"type":"Metric"}]}]"#;
    for r in [
        body.to_string(),
        json!({"statusCode":200,"body":body}).to_string(),
    ] {
        assert_out(check("openai", GOLDENS[0], &r, false), 0, "ok:");
    }
}
#[test]
fn ap_rw_18_19_private_diagnostics_and_strict_json() {
    let o = check(
        "openai",
        GOLDENS[0],
        r#"{"error":"secret-sentinel"}"#,
        false,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!text.contains("secret-sentinel"));
    assert!(!text.contains("InvalidLambdaResponseException"));
    assert_out(o, 1, "matches neither documented envelope (C10 Option A/B)");
    for r in ["\u{feff}[]", r#"[{"id":"123","id":"123"}]"#] {
        assert_out(
            check("openai", GOLDENS[0], r, false),
            1,
            "invalid JSON in --response",
        );
    }
}
#[test]
fn ap_rw_20_21_required_event_members() {
    let required: [&[&str]; 6] = [
        &["id", "messages", "reference_answer.text"],
        &[
            "data_source",
            "prompt",
            "response",
            "extra_info.reference_answer.text",
        ],
        &["id", "prompt", "completion", "reference_answer.text"],
        &[
            "id",
            "prompt",
            "completion",
            "chosen",
            "rejected",
            "reference_answer.text",
        ],
        &[
            "id",
            "model_response",
            "query",
            "response",
            "reference_answer.text",
        ],
        &["id", "messages"],
    ];
    for (i, f) in FORMATS.iter().enumerate() {
        let o = check(f, "[{}]", &response("x"), false);
        assert_eq!(o.status.code(), Some(1));
        let s = String::from_utf8_lossy(&o.stderr);
        for field in required[i] {
            assert!(s.contains(&format!("event[0].{field}")), "{f}: {s}");
        }
        if *f != "verl" {
            let mut v = array(i);
            v[0].as_object_mut().unwrap().remove("id");
            assert_out(
                check(f, &v.to_string(), &response("anything"), false),
                1,
                "event[0].id",
            );
        }
    }
}
#[test]
fn ap_rw_22_24_batch_and_nova_bounds() {
    for (i, f) in FORMATS[..5].iter().enumerate() {
        let mut a = array(i);
        let mut second = a[0].clone();
        second["id"] = json!("456");
        a.as_array_mut().unwrap().push(second);
        let r=json!([{"id": "123","aggregate_reward_score":1},{"id":"456","aggregate_reward_score":1}]).to_string();
        assert_out(check(f, &a.to_string(), &r, false), 1, "event_sample_count");
        assert_out(
            check(f, &a.to_string(), &r, true),
            0,
            "batch: local extension, the service sends 1",
        );
    }
    let r = response("123");
    for e in [event(5), array(5)] {
        assert_out(
            check("nova", &e.to_string(), &r, false),
            0,
            "ok: nova, 1 sample(s)",
        );
    }
    let many = json!(vec![event(5); 1001]).to_string();
    assert_out(check("nova", &many, &r, false), 1, "event_shape");
    assert_out(check("openai", &many, &r, true), 1, "event_shape");
    let mut a = Vec::new();
    let mut b = Vec::new();
    for i in 0..1000 {
        let mut item = array(0)[0].clone();
        item["id"] = json!(i.to_string());
        a.push(item);
        b.push(json!({"id":i.to_string(),"aggregate_reward_score":1}));
    }
    assert_out(
        check("openai", &json!(a).to_string(), &json!(b).to_string(), true),
        0,
        "1000 sample(s), 1000 result(s)",
    );
}
#[test]
fn ap_rw_23_event_shapes() {
    for f in FORMATS {
        for e in ["[]", "1", "[1]", "[[]]", "\"x\""] {
            assert_out(check(f, e, "[]", false), 1, "event_shape");
        }
        if f != "nova" {
            assert_out(check(f, "{}", "[]", false), 1, "event_shape");
        }
    }
}
#[test]
fn ap_rw_25_format_type_errors() {
    let mut e = array(0);
    e[0]["messages"] = json!([]);
    assert_out(
        check("openai", &e.to_string(), "[]", false),
        1,
        "event[0].messages",
    );
    e[0]["messages"] = json!([{"role":"user","content":"x"}]);
    assert_out(
        check("openai", &e.to_string(), "[]", false),
        1,
        "event[0].messages[last].role",
    );
    let mut e = array(1);
    e[0]["prompt"] = json!("bad");
    assert_out(
        check("verl", &e.to_string(), "[]", false),
        1,
        "event[0].prompt",
    );
    let mut e = array(3);
    e[0]["chosen"] = json!(1);
    assert_out(
        check("hf-preference", &e.to_string(), "[]", false),
        1,
        "event[0].chosen",
    );
    let mut e = array(4);
    e[0].as_object_mut().unwrap().remove("model_response");
    assert_out(
        check("sagemaker-eval", &e.to_string(), "[]", false),
        1,
        "event[0].model_response",
    );
}
#[test]
fn ap_rw_26_27_frozen_template_failures_and_bare_result() {
    assert_out(
        check("nova", GOLDENS[5], r#"{"statusCode":200,"body":[]}"#, false),
        1,
        "body: expected JSON string",
    );
    for i in [0, 1] {
        assert_out(
            check(FORMATS[i], GOLDENS[i], r#"{"error":"failure"}"#, false),
            1,
            "matches neither documented envelope (C10 Option A/B)",
        );
    }
    for (i, f) in FORMATS.iter().enumerate() {
        assert_out(
            check(
                f,
                GOLDENS[i],
                r#"{"id":"123","aggregate_reward_score":1}"#,
                false,
            ),
            1,
            "matches neither documented envelope (C10 Option A/B)",
        );
    }
}
