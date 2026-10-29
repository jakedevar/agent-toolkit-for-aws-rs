//! Offline reward-contract CLI tests (AP-RW-01..27).
use std::{fs, process::Command};

#[test]
fn ap_rw_sample_and_round_trip() {
    let dir = std::env::temp_dir().join(format!("toolkit-reward-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for format in [
        "openai",
        "verl",
        "hf-prompt-completion",
        "hf-preference",
        "sagemaker-eval",
        "nova",
    ] {
        let sample = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["ai-payloads", "reward", "sample", "--format", format])
            .output()
            .unwrap();
        assert!(
            sample.status.success(),
            "sample {format}: {}",
            String::from_utf8_lossy(&sample.stderr)
        );
        let event: serde_json::Value = serde_json::from_slice(&sample.stdout).unwrap();
        let first = event.as_array().map_or(&event, |a| &a[0]);
        let id = first
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("sample-id");
        let response = serde_json::json!([{"id": id, "aggregate_reward_score": 1.0}]);
        let event_path = dir.join("event.json");
        let response_path = dir.join("response.json");
        fs::write(&event_path, &sample.stdout).unwrap();
        fs::write(&response_path, response.to_string()).unwrap();
        let check = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args([
                "ai-payloads",
                "reward",
                "check",
                "--format",
                format,
                "--event",
            ])
            .arg(&event_path)
            .arg("--response")
            .arg(&response_path)
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "check {format}: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ap_rw_envelopes_shapes_and_diagnostics() {
    let dir = std::env::temp_dir().join(format!("toolkit-reward-negative-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let event = dir.join("event");
    let response = dir.join("response");
    let run = |format: &str, e: &str, r: &str, batch: bool| {
        fs::write(&event, e).unwrap();
        fs::write(&response, r).unwrap();
        let mut c = Command::new(env!("CARGO_BIN_EXE_toolkit"));
        c.args([
            "ai-payloads",
            "reward",
            "check",
            "--format",
            format,
            "--event",
        ])
        .arg(&event)
        .arg("--response")
        .arg(&response);
        if batch {
            c.arg("--batch");
        }
        c.output().unwrap()
    };
    let valid = r#"[{"id":"x","messages":[{"role":"assistant","content":"secret-sentinel"}],"reference_answer":{"text":"a"}}]"#;
    let result = r#"[{"id":"x","aggregate_reward_score":1}]"#;
    let wrapped = serde_json::json!({"statusCode":200,"body":result}).to_string();
    assert!(run("openai", valid, &wrapped, false).status.success());
    for r in [
        r#"{"statusCode":200,"body":[]}"#,
        r#"{"error":"secret-sentinel"}"#,
        r#"{"statusCode":500,"body":"[]"}"#,
        "[]",
        r#"[{"id":"other","aggregate_reward_score":1}]"#,
        r#"[{"id":"x","aggregate_reward_score":1,"metrics_list":[{"name":"m","value":1,"type":"bad"}]}]"#,
        r#"{"id":"x","aggregate_reward_score":1}"#,
        r#"[{"id":"x","id":"x"}]"#,
        "\u{feff}[]",
    ] {
        let o = run("openai", valid, r, false);
        assert_eq!(o.status.code(), Some(1), "{r}");
        assert!(!String::from_utf8_lossy(&o.stderr).contains("secret-sentinel"));
    }
    for e in ["[]", "{}", "1", "[1]", "[[]]", "\"x\"", "[{}]"] {
        assert_eq!(
            run("openai", e, result, false).status.code(),
            Some(1),
            "{e}"
        );
    }
    let pair = format!(
        "[{},{}]",
        &valid[1..valid.len() - 1],
        &valid[1..valid.len() - 1]
    );
    let failure = run("openai", &pair, result, false);
    assert!(String::from_utf8_lossy(&failure.stderr).contains("event_sample_count"));
    let other = r#"[{"id":"x","messages":[{"role":"user","content":"a"}],"reference_answer":{"text":"a"}}]"#;
    assert_eq!(run("openai", other, result, false).status.code(), Some(1));
    assert!(run("nova", other, result, false).status.success());
    let verl = r#"[{"data_source":"a","prompt":[{"role":"user","content":"a"}],"response":"a","extra_info":{"reference_answer":{"text":"a"}}}]"#;
    let o = run("verl", verl, result, false);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("id not compared"));
    let usage = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["ai-payloads", "reward", "check"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    fs::remove_dir_all(dir).unwrap();
}
