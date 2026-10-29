//! Slice S3r (#5120): AP-RWT-01..05, the minimal response-format fix of the two
//! shipped reward-Lambda templates T06 and T09.
//!
//! Everything here reads template *text* or runs the Rust `toolkit ai-payloads
//! reward check`; no Python is run and no Python tooling is involved.
//!
//! Case map:
//! - AP-RWT-01: `ap_rwt_01_t09_body_is_json_string_and_passes_nova_check`
//! - AP-RWT-02: `ap_rwt_02_t06_empty_batch_returns_documented_envelope`,
//!   `ap_rwt_02_t06_failing_sample_yields_one_result_per_input`
//! - AP-RWT-03: `ap_rwt_03_original_d1_d2_shapes_stay_flagged`
//! - AP-RWT-04: `ap_rwt_04_mirrors_are_byte_identical`,
//!   `ap_rwt_04_sync_plugin_skills_check_is_clean`
//! - AP-RWT-05: `ap_rwt_05_only_the_four_template_files_change_and_no_py_is_added`,
//!   `ap_rwt_05_these_tests_never_launch_python`
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

const T06: &str =
    "skills/core-skills/aws-ai-ml/references/finetuning/templates/rlvr_reward_function_source_template.py";
const T09: &str =
    "skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/nova_reward_function_source_template.py";
const MIRROR_PREFIX: &str = "plugins/aws-core/";
const MIRROR_STRIP: &str = "skills/core-skills/";

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn mirror(canonical: &str) -> String {
    format!(
        "{MIRROR_PREFIX}skills/{}",
        canonical.strip_prefix(MIRROR_STRIP).unwrap()
    )
}
fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap()
}
/// The text of `lambda_handler` through the end of the file (it is the last
/// function of both templates).
fn handler(rel: &str) -> String {
    let text = read(rel);
    let at = text.find("def lambda_handler").unwrap();
    text[at..].to_string()
}
fn check(format: &str, event: &str, response: &str) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "reward-templates-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    let (e, r) = (dir.join("event"), dir.join("response"));
    fs::write(&e, event).unwrap();
    fs::write(&r, response).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args([
            "ai-payloads",
            "reward",
            "check",
            "--format",
            format,
            "--event",
        ])
        .arg(&e)
        .arg("--response")
        .arg(&r)
        .output()
        .unwrap();
    fs::remove_dir_all(dir).unwrap();
    out
}
fn is_py(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("py"))
}
fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}
fn assert_ok(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "{}", text(o));
}
fn assert_flagged(o: &Output, needle: &str) {
    assert_eq!(o.status.code(), Some(1), "{}", text(o));
    assert!(text(o).contains(needle), "{needle:?} not in {}", text(o));
}

const OPENAI_EVENT: &str = r#"[{"id":"123","messages":[{"role":"user","content":"What is 2+2?"},{"role":"assistant","content":"4"}],"reference_answer":{"text":"4"}}]"#;
const VERL_EVENT: &str = r#"[{"id":"v1","data_source":"openai/gsm8k","prompt":[{"role":"user","content":"What is the capital of France?"},{"role":"assistant","content":"The capital of France is Paris."}],"response":"The capital of France is Paris.","reward_model":{"style":"rule","ground_truth":"Paris"},"extra_info":{"reference_answer":{"text":"Paris"},"processor_config":{"aggregation":"mean"}}}]"#;
const NOVA_EVENT: &str = r#"{"id":"123","messages":[{"role":"user","content":"Q"},{"role":"assistant","content":"A"}],"reference_answer":{"compliant":"No","explanation":"e"}}"#;

// ---- AP-RWT-01 ------------------------------------------------------------

#[test]
fn ap_rwt_01_t09_body_is_json_string_and_passes_nova_check() {
    let h = handler(T09);
    // The corrected return statement, pinned from the template text.
    assert!(
        h.contains("\"body\": json.dumps(results),"),
        "T09 return must serialise body with json.dumps:\n{h}"
    );
    assert!(
        !h.contains("\"body\": results"),
        "T09 must not return a parsed list as body:\n{h}"
    );
    assert!(
        !h.contains("parsed list, NOT json.dumps"),
        "T09 comment still demands the rejected shape:\n{h}"
    );
    // The same return statement, transcribed: statusCode 200, body is the JSON
    // string of the results list (what `json.dumps(results)` yields).
    let results = json!([{"id":"123","aggregate_reward_score":0.5,"metrics_list":[]}]);
    let response = json!({"statusCode":200,"headers":{"Content-Type":"application/json"},"body":results.to_string()});
    assert!(response["body"].is_string());
    assert_ok(&check("nova", NOVA_EVENT, &response.to_string()));
    // And the list-valued body this fix removes is still rejected.
    let old = json!({"statusCode":200,"body":results});
    assert_flagged(
        &check("nova", NOVA_EVENT, &old.to_string()),
        "body: expected JSON string",
    );
}

// ---- AP-RWT-02 ------------------------------------------------------------

#[test]
fn ap_rwt_02_t06_empty_batch_returns_documented_envelope() {
    let h = handler(T06);
    assert!(
        !h.contains("Missing or empty batch"),
        "T06 empty batch still returns the undocumented error shape:\n{h}"
    );
    let at = h.find("if not batch:").expect("empty-batch guard kept");
    let guard: String = h[at..].lines().take(8).collect::<Vec<_>>().join("\n");
    assert!(guard.contains("\"statusCode\": 200"), "{guard}");
    assert!(guard.contains("\"body\": json.dumps([])"), "{guard}");
    assert!(!guard.contains("\"error\""), "{guard}");
    // Zero inputs, zero results: the envelope the template returns is the
    // documented Option B with an empty JSON-string list.
    let empty = json!({"statusCode":200,"body":json!([]).to_string()});
    let one = check("openai", OPENAI_EVENT, &empty.to_string());
    assert_flagged(&one, "result_count");
    let out = check("openai", "[]", &empty.to_string());
    assert!(
        !text(&out).contains("matches neither documented envelope"),
        "{}",
        text(&out)
    );
}

#[test]
fn ap_rwt_02_t06_failing_sample_yields_one_result_per_input() {
    let h = handler(T06);
    assert!(
        !h.contains("return {\"error\": str(e)}"),
        "T06 failing sample still aborts the batch with an undocumented error:\n{h}"
    );
    let at = h
        .find("except Exception:\n                # One result per input")
        .expect("failing-sample branch");
    let branch: String = h[at..].lines().take(14).collect::<Vec<_>>().join("\n");
    assert!(branch.contains("results.append("), "{branch}");
    assert!(
        branch.contains("\"aggregate_reward_score\": 0.0"),
        "{branch}"
    );
    assert!(branch.contains("\"metrics_list\": []"), "{branch}");
    assert!(branch.contains("sample.get(\"id\""), "{branch}");
    // The transcribed failure results (score 0.0, id carried from the input,
    // sample-NNN fallback) pass under both RLVR formats.
    let body = json!([{"id":"123","aggregate_reward_score":0.0,"metrics_list":[]}]).to_string();
    let envelope =
        json!({"statusCode":200,"headers":{"Content-Type":"application/json"},"body":body});
    assert_ok(&check("openai", OPENAI_EVENT, &envelope.to_string()));
    let fallback =
        json!([{"id":"sample-000","aggregate_reward_score":0.0,"metrics_list":[]}]).to_string();
    let verl = json!({"statusCode":200,"body":fallback});
    assert_ok(&check(
        "verl",
        VERL_EVENT.replace("\"id\":\"v1\",", "").as_str(),
        &verl.to_string(),
    ));
    // Batch of two inputs with one failure: still two results.
    let two_event = json!([serde_json::from_str::<Value>(OPENAI_EVENT).unwrap()[0].clone(),
                           {"id":"124","messages":[{"role":"user","content":"Q"},{"role":"assistant","content":"A"}],"reference_answer":{"text":"A"}}]);
    let two = json!([
        {"id":"123","aggregate_reward_score":1.0,"metrics_list":[]},
        {"id":"124","aggregate_reward_score":0.0,"metrics_list":[]}
    ]);
    let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args([
            "ai-payloads",
            "reward",
            "check",
            "--format",
            "openai",
            "--batch",
            "--event",
        ])
        .arg(write_tmp(&two_event.to_string()))
        .arg("--response")
        .arg(write_tmp(
            &json!({"statusCode":200,"body":two.to_string()}).to_string(),
        ))
        .output()
        .unwrap();
    assert_ok(&out);
}
fn write_tmp(contents: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "reward-templates-file-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&p, contents).unwrap();
    p
}

// ---- AP-RWT-03 ------------------------------------------------------------

#[test]
fn ap_rwt_03_original_d1_d2_shapes_stay_flagged() {
    // Frozen D-1 and D-2 fixtures (see also AP-RW-26 in ai_payloads_reward_matrix.rs).
    assert_flagged(
        &check("nova", NOVA_EVENT, r#"{"statusCode":200,"body":[]}"#),
        "body: expected JSON string",
    );
    for (f, e) in [("openai", OPENAI_EVENT), ("verl", VERL_EVENT)] {
        assert_flagged(
            &check(f, e, r#"{"error":"Missing or empty batch"}"#),
            "matches neither documented envelope (C10 Option A/B)",
        );
    }
}

// ---- AP-RWT-04 ------------------------------------------------------------

#[test]
fn ap_rwt_04_mirrors_are_byte_identical() {
    for canonical in [T06, T09] {
        let a = fs::read(root().join(canonical)).unwrap();
        let b = fs::read(root().join(mirror(canonical))).unwrap();
        assert_eq!(a, b, "{canonical} differs from its plugins/ mirror");
    }
}

#[test]
fn ap_rwt_04_sync_plugin_skills_check_is_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["sync", "plugin-skills", "--check"])
        .current_dir(root())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
}

// ---- AP-RWT-05 ------------------------------------------------------------

#[test]
fn ap_rwt_05_only_the_four_template_files_change_and_no_py_is_added() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root())
            .output()
            .unwrap()
    };
    let base = git(&["merge-base", "HEAD", "origin/rolling"]);
    if !base.status.success() {
        eprintln!("no origin/rolling ref; diff-scope check skipped");
        return;
    }
    let base = String::from_utf8(base.stdout).unwrap().trim().to_string();
    let diff = git(&["diff", "--name-status", "--no-renames", &base]);
    assert!(diff.status.success());
    let allowed = [T06.to_string(), T09.to_string(), mirror(T06), mirror(T09)];
    for line in String::from_utf8(diff.stdout).unwrap().lines() {
        let (status, path) = line.split_once('\t').unwrap();
        if is_py(path) {
            assert_eq!(status, "M", "a .py file was added or deleted: {line}");
            assert!(
                allowed.iter().any(|a| a == path),
                "a .py file outside T06/T09 and their mirrors changed: {line}"
            );
        }
    }
    let untracked = git(&["ls-files", "--others", "--exclude-standard"]);
    for path in String::from_utf8(untracked.stdout).unwrap().lines() {
        assert!(!is_py(path), "untracked .py file: {path}");
    }
}

#[test]
fn ap_rwt_05_these_tests_never_launch_python() {
    let src = read("crates/toolkit-cli/tests/ai_payloads_reward_templates.rs");
    // The needles are escaped in this source, so the test does not match itself.
    for needle in ["Command::new(\"py", "Command::new(\"python"] {
        assert_eq!(src.matches(needle).count(), 0, "{needle}");
    }
}
