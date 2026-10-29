//! Offline reward-contract CLI tests (AP-RW-01..27).
use std::{fs, process::Command};

#[test]
fn ap_rw_sample_and_round_trip() {
    let dir = std::env::temp_dir().join(format!("toolkit-reward-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for format in ["openai", "verl", "hf-prompt-completion", "hf-preference", "sagemaker-eval", "nova"] {
        let sample = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["ai-payloads", "reward", "sample", "--format", format]).output().unwrap();
        assert!(sample.status.success(), "sample {format}: {}", String::from_utf8_lossy(&sample.stderr));
        let event: serde_json::Value = serde_json::from_slice(&sample.stdout).unwrap();
        let first = event.as_array().map_or(&event, |a| &a[0]);
        let id = first.get("id").and_then(|v| v.as_str()).unwrap_or("sample-id");
        let response = serde_json::json!([{"id": id, "aggregate_reward_score": 1.0}]);
        let event_path = dir.join("event.json");
        let response_path = dir.join("response.json");
        fs::write(&event_path, &sample.stdout).unwrap();
        fs::write(&response_path, response.to_string()).unwrap();
        let check = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["ai-payloads", "reward", "check", "--format", format, "--event"])
            .arg(&event_path).arg("--response").arg(&response_path).output().unwrap();
        assert!(check.status.success(), "check {format}: {}", String::from_utf8_lossy(&check.stderr));
    }
    fs::remove_dir_all(dir).unwrap();
}
