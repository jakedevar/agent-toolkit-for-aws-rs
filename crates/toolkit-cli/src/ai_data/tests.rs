//! Offline tests for the `ai-data` commands: every rule has a positive and a
//! negative case, and no test touches AWS, S3 or the network.

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::Json;

use super::format::{
    classify_schema, detect_lines, lines_from_sample, parse_s3_uri, sample_local_file,
    sample_s3_file, Confidence, FormatType, NoObjectStore, ObjectStore, MAX_REPORTED_ERRORS,
};
use super::transform::{
    build_plan, describe, submit, JobDetails, NoSageMaker, ProcessingPlan, ProcessingService,
    TransformRequest,
};
use super::{detect_format_with, metrics, py_type_name};
use crate::{route, Output, Routed, COMMANDS};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-ai-data-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn file(&self, name: &str, content: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, content).expect("write fixture");
        path.to_string_lossy().into_owned()
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exec(args: &[&str]) -> (u8, String, String) {
    let os: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = crate::run(&os, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(ToString::to_string).collect()
}

fn lines(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn detect(values: &[&str]) -> super::format::FormatDetectionResult {
    detect_lines(&lines(values))
}

fn messages(result: &super::format::FormatDetectionResult) -> Vec<String> {
    result
        .errors
        .iter()
        .map(|error| {
            format!(
                "{}:{}:{}",
                error.line_number, error.error_type, error.message
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Format classification: one valid record per format
// ---------------------------------------------------------------------------

const NOVA_SFT: &str = r#"{"messages":[{"role":"user","content":[{"text":"hi"}]},{"role":"assistant","content":[{"text":"yo"}]}]}"#;
const NOVA_DPO: &str = r#"{"messages":[{"role":"user","content":[{"text":"hi"}]},{"role":"assistant","candidates":[{"content":[{"text":"a"}],"preferenceLabel":"preferred"},{"content":[{"text":"b"}],"preferenceLabel":"non-preferred"}]}]}"#;
const NOVA_RLVR: &str =
    r#"{"messages":[{"role":"user","content":"q"}],"reference_answer":{"answer":"1"}}"#;
const GPT_OSS_SFT: &str =
    r#"{"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"yo"}]}"#;
const GPT_OSS_DPO: &str = r#"{"prompt":[{"role":"user","content":"q"}],"chosen":[{"role":"assistant","content":"a"}],"rejected":[{"role":"assistant","content":"b"}]}"#;
const OPEN_SFT: &str = r#"{"prompt":"q","completion":"a"}"#;
const OPEN_SFT_CONV: &str = r#"{"prompt":[{"role":"user","content":"q"}],"completion":[{"role":"assistant","content":"a"}]}"#;
const OPEN_DPO: &str = r#"{"prompt":"q","chosen":"a","rejected":"b"}"#;
const VERL: &str =
    r#"{"prompt":[{"role":"user","content":"q"}],"reward_model":{"ground_truth":"1"}}"#;
const VERL_LEGACY: &str = r#"{"prompt":"q","extra_info":{"index":1}}"#;
const SAGEMAKER_EVAL: &str = r#"{"query":"q","response":"r"}"#;

#[test]
fn every_format_is_detected_and_validates_cleanly() {
    let cases = [
        (NOVA_SFT, "nova_sft"),
        (NOVA_DPO, "nova_dpo"),
        (NOVA_RLVR, "nova_rlvr"),
        (GPT_OSS_SFT, "gpt_oss_sft"),
        (GPT_OSS_DPO, "gpt_oss_dpo"),
        (OPEN_SFT, "open_weights_sft"),
        (OPEN_SFT_CONV, "open_weights_sft_conv"),
        (OPEN_DPO, "open_weights_dpo"),
        (VERL, "verl"),
        (VERL_LEGACY, "verl_legacy"),
        (SAGEMAKER_EVAL, "sagemaker_eval"),
    ];
    for (line, expected) in cases {
        let result = detect(&[line, line]);
        assert_eq!(result.format_type.value(), expected, "{line}");
        assert!(result.is_valid, "{line}: {:?}", result.errors);
        assert_eq!(result.confidence, Confidence::High, "{line}");
        assert_eq!(result.lines_sampled, 2);
        assert!(result.errors.is_empty());
    }
}

#[test]
fn classification_precedence_follows_the_script() {
    let classify = |text: &str| classify_schema(&toolkit_core::parse_json(text).expect("json"));
    // query+response wins over messages.
    assert_eq!(
        classify(r#"{"query":"q","response":"r","messages":[]}"#),
        FormatType::SagemakerEval
    );
    // prompt+reward_model without completion is verl; with completion it falls through.
    assert_eq!(
        classify(r#"{"prompt":"q","reward_model":{},"completion":"a"}"#),
        FormatType::OpenWeightsSft
    );
    // reference_answer makes any messages record RLVR, even with a bad messages value.
    assert_eq!(
        classify(r#"{"messages":"x","reference_answer":{}}"#),
        FormatType::NovaRlvr
    );
    // Empty, wrongly typed or odd messages are unknown.
    for text in [
        r#"{"messages":[]}"#,
        r#"{"messages":"x"}"#,
        r#"{"messages":[{"role":"user"}]}"#,
        r#"{"messages":[{"content":5}]}"#,
        r#"{"messages":["text"]}"#,
        r#"{"messages":[{"content":[]},{"content":[]}]}"#,
        r#"{"messages":[{"content":[]},"text"]}"#,
        r#"{"other":1}"#,
        "[1]",
        "5",
    ] {
        assert_eq!(classify(text), FormatType::Unknown, "{text}");
    }
}

#[test]
fn nova_last_message_decides_between_sft_and_dpo() {
    let classify = |text: &str| classify_schema(&toolkit_core::parse_json(text).expect("json"));
    let head = r#"{"role":"user","content":[{"text":"a"}]}"#;
    assert_eq!(
        classify(&format!(
            r#"{{"messages":[{head},{{"role":"assistant","candidates":[]}}]}}"#
        )),
        FormatType::NovaDpo
    );
    assert_eq!(
        classify(&format!(
            r#"{{"messages":[{head},{{"role":"assistant","content":[{{"text":"b"}}]}}]}}"#
        )),
        FormatType::NovaSft
    );
    // A falsy last content is not SFT.
    for last in [
        r#"{"content":[]}"#,
        r#"{"content":""}"#,
        r#"{"content":0}"#,
        r#"{"content":null}"#,
        r#"{"role":"assistant"}"#,
    ] {
        assert_eq!(
            classify(&format!(r#"{{"messages":[{head},{last}]}}"#)),
            FormatType::Unknown,
            "{last}"
        );
    }
}

// ---------------------------------------------------------------------------
// Validation rules (negative cases)
// ---------------------------------------------------------------------------

#[test]
fn messages_need_a_valid_role() {
    let result = detect(&[r#"{"messages":[{"content":"hi"}]}"#]);
    assert_eq!(
        messages(&result),
        ["1:missing_field:Message 0 missing required field 'role'"]
    );
    assert_eq!(result.confidence, Confidence::Low);
    let result = detect(&[r#"{"messages":[{"role":"bot","content":"hi"}]}"#]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:Invalid role 'bot' in message 0"]
    );
    // A non-string role is shown the way Python's str() shows it.
    let result =
        detect(&[r#"{"messages":[{"role":5,"content":"hi"},{"role":null,"content":"x"}]}"#]);
    assert_eq!(
        messages(&result),
        [
            "1:invalid_structure:Invalid role '5' in message 0",
            "1:invalid_structure:Invalid role 'None' in message 1",
        ]
    );
}

#[test]
fn gpt_oss_content_must_be_a_string() {
    let result = detect(&[
        r#"{"messages":[{"role":"user","content":"a"},{"role":"user","content":5},{"role":"user"}]}"#,
    ]);
    assert_eq!(
        messages(&result),
        [
            "1:invalid_structure:GPT-OSS format content must be string, got int",
            "1:missing_field:Message 2 missing required field 'content'",
        ]
    );
}

#[test]
fn nova_messages_need_list_content_or_candidates() {
    let head = r#"{"role":"user","content":[{"text":"a"}]}"#;
    let tail = r#"{"role":"assistant","content":[{"text":"b"}]}"#;
    let result = detect(&[&format!(
        r#"{{"messages":[{head},{{"role":"user"}},{tail}]}}"#
    )]);
    assert_eq!(
        messages(&result),
        ["1:missing_field:Message 1 missing 'content' or 'candidates'"]
    );
    let result = detect(&[&format!(
        r#"{{"messages":[{head},{{"role":"user","content":"s"}},{tail}]}}"#
    )]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:Nova format content must be list, got str"]
    );
}

#[test]
fn dpo_candidates_need_a_preference_label() {
    let head = r#"{"role":"user","content":[{"text":"a"}]}"#;
    let result = detect(&[&format!(
        r#"{{"messages":[{head},{{"role":"assistant","candidates":[{{"x":1}},{{"preferenceLabel":"maybe"}},{{"preferenceLabel":"preferred"}}]}}]}}"#
    )]);
    assert_eq!(result.format_type, FormatType::NovaDpo);
    assert_eq!(
        messages(&result),
        [
            "1:missing_field:DPO message 1 candidate 0 missing 'preferenceLabel'",
            "1:invalid_structure:Invalid preferenceLabel 'maybe' in message 1 candidate 1",
        ]
    );
    // Shapes Python would crash on become findings.
    let result = detect(&[&format!(
        r#"{{"messages":[{head},{{"role":"assistant","candidates":"x"}}]}}"#
    )]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:DPO message 1 'candidates' must be list, got str"]
    );
    let result = detect(&[&format!(
        r#"{{"messages":[{head},{{"role":"assistant","candidates":[5]}}]}}"#
    )]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:DPO message 1 candidate 0 must be an object, got int"]
    );
}

#[test]
fn candidates_are_only_checked_for_dpo() {
    let tail = r#"{"role":"assistant","content":[{"text":"b"}]}"#;
    let result = detect(&[&format!(
        r#"{{"messages":[{{"role":"user","content":[{{"text":"a"}}],"candidates":"ignored"}},{tail}]}}"#
    )]);
    assert_eq!(result.format_type, FormatType::NovaSft);
    assert!(result.is_valid, "{:?}", result.errors);
}

#[test]
fn rlvr_checks_reference_answer_and_content() {
    let result = detect(&[
        r#"{"messages":[{"role":"user","content":"q"},{"role":"assistant","content":7}],"reference_answer":[]}"#,
    ]);
    assert_eq!(result.format_type, FormatType::NovaRlvr);
    assert_eq!(
        messages(&result),
        [
            "1:invalid_structure:Field 'reference_answer' must be dict, got list",
            "1:invalid_structure:Nova RLVR content must be string, got int",
        ]
    );
    // A messages field that is not a list skips the per-message checks.
    let result = detect(&[r#"{"messages":"x","reference_answer":{}}"#]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:Field 'messages' must be a list"]
    );
}

#[test]
fn flat_formats_check_field_types_with_their_prefix() {
    let result = detect(&[r#"{"prompt":"q","completion":5}"#]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:Open Weights SFT field 'completion' must be str, got int"]
    );
    let result = detect(&[r#"{"prompt":"q","chosen":["a"],"rejected":null}"#]);
    assert_eq!(
        messages(&result),
        [
            "1:invalid_structure:Open Weights DPO field 'chosen' must be str, got list",
            "1:invalid_structure:Open Weights DPO field 'rejected' must be str, got NoneType",
        ]
    );
    let result = detect(&[r#"{"query":"q","response":{"a":1}}"#]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:SageMaker Eval field 'response' must be str, got dict"]
    );
    let result = detect(&[r#"{"prompt":[],"chosen":[],"rejected":1.5}"#]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:GPT-OSS DPO field 'rejected' must be list, got float"]
    );
    let result = detect(&[r#"{"prompt":[],"completion":"x"}"#]);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:Open Weights SFT Conv field 'completion' must be list, got str"]
    );
}

#[test]
fn verl_prompts_are_checked() {
    let result =
        detect(&[r#"{"prompt":[{"role":"user"},"text",{"content":"x"}],"reward_model":{}}"#]);
    assert_eq!(result.format_type, FormatType::Verl);
    assert_eq!(
        messages(&result),
        [
            "1:invalid_structure:Prompt message 0 must have 'role' and 'content'",
            "1:invalid_structure:Prompt message 1 must have 'role' and 'content'",
            "1:invalid_structure:Prompt message 2 must have 'role' and 'content'",
        ]
    );
    let result = detect(&[r#"{"prompt":5,"reward_model":{}}"#]);
    assert_eq!(result.format_type, FormatType::VerlLegacy);
    assert_eq!(
        messages(&result),
        ["1:invalid_structure:Verl Legacy field 'prompt' must be string, got int"]
    );
}

#[test]
fn records_must_match_the_first_records_format() {
    let result = detect(&[OPEN_SFT, SAGEMAKER_EVAL, OPEN_SFT, OPEN_DPO]);
    assert_eq!(
        messages(&result),
        [
            "2:schema_mismatch:Expected open_weights_sft but found sagemaker_eval",
            "4:schema_mismatch:Expected open_weights_sft but found open_weights_dpo",
        ]
    );
    assert_eq!(result.confidence, Confidence::Low);
    assert!(!result.is_valid);
}

#[test]
fn unknown_records_are_valid_unknown_and_mismatch_known_ones() {
    let result = detect(&[r#"{"foo":1}"#, r#"{"bar":2}"#]);
    assert_eq!(result.format_type, FormatType::Unknown);
    assert!(result.is_valid);
    assert_eq!(result.confidence, Confidence::High);
    let result = detect(&[r#"{"foo":1}"#, OPEN_SFT]);
    assert_eq!(
        messages(&result),
        ["2:schema_mismatch:Expected unknown but found open_weights_sft"]
    );
}

#[test]
fn records_that_are_not_objects_are_findings() {
    let result = detect(&["[1,2]", "5", OPEN_SFT]);
    assert_eq!(result.format_type, FormatType::Unknown);
    assert_eq!(
        messages(&result),
        [
            "1:invalid_structure:Record must be a JSON object, got list",
            "2:invalid_structure:Record must be a JSON object, got int",
            "3:schema_mismatch:Expected unknown but found open_weights_sft",
        ]
    );
    let result = detect(&[OPEN_SFT, r#""text""#]);
    assert_eq!(
        messages(&result),
        ["2:invalid_structure:Record must be a JSON object, got str"]
    );
}

#[test]
fn malformed_lines_are_parse_errors_with_no_confidence() {
    let result = detect(&[r#"{"prompt":"#, OPEN_SFT, "not json", r#"{"a":1,"a":2}"#]);
    assert_eq!(result.lines_sampled, 4);
    assert_eq!(result.format_type, FormatType::OpenWeightsSft);
    assert_eq!(result.confidence, Confidence::None);
    assert!(!result.is_valid);
    let numbered: Vec<(usize, &str)> = result
        .errors
        .iter()
        .map(|error| (error.line_number, error.error_type))
        .collect();
    // Duplicate keys are rejected by the strict shared parser (documented change).
    assert_eq!(
        numbered,
        [(1, "parse_error"), (3, "parse_error"), (4, "parse_error")]
    );
    assert!(result
        .errors
        .iter()
        .all(|error| error.message.starts_with("Invalid JSON: ")));
}

#[test]
fn only_unparseable_lines_give_unknown_with_no_confidence() {
    let result = detect(&["{", "}"]);
    assert_eq!(result.format_type, FormatType::Unknown);
    assert!(!result.is_valid);
    assert_eq!(result.confidence, Confidence::None);
    assert_eq!(result.lines_sampled, 2);
    let empty = detect(&[]);
    assert_eq!(empty.format_type, FormatType::Unknown);
    assert!(empty.is_valid);
    assert_eq!(empty.confidence, Confidence::High);
    assert_eq!(empty.lines_sampled, 0);
}

#[test]
fn error_reports_are_capped_but_counted() {
    let many = vec!["{".to_string(); MAX_REPORTED_ERRORS + 50];
    let result = detect_lines(&many);
    assert_eq!(result.errors.len(), MAX_REPORTED_ERRORS);
    assert_eq!(result.errors_omitted, 50);
    assert!(!result.is_valid);
    assert_eq!(result.confidence, Confidence::None);
    assert_eq!(result.lines_sampled, MAX_REPORTED_ERRORS + 50);
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

#[test]
fn samples_keep_only_complete_nonempty_lines() {
    assert!(lines_from_sample(b"").is_empty());
    assert!(lines_from_sample(b"{\"a\":1}").is_empty());
    assert_eq!(
        lines_from_sample(b"one\n\ntwo\npartial"),
        ["one", "two"].map(String::from)
    );
    assert_eq!(
        lines_from_sample(b"a\r\nb\n"),
        ["a\r", "b"].map(String::from)
    );
    assert_eq!(lines_from_sample(b"\n\n"), Vec::<String>::new());
    // Invalid UTF-8 is replaced, never an error.
    assert_eq!(lines_from_sample(b"\xff\n"), ["\u{fffd}"].map(String::from));
}

#[test]
fn line_numbers_count_nonempty_lines() {
    let sampled = lines_from_sample(b"\n{bad\n\n{\"prompt\":\"q\",\"completion\":\"a\"}\n");
    let result = detect_lines(&sampled);
    assert_eq!(result.lines_sampled, 2);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].line_number, 1);
}

#[test]
fn local_sampling_reads_only_the_requested_prefix() {
    let dir = TempDir::new("sample");
    let line = format!("{{\"query\":\"q\",\"response\":\"{}\"}}\n", "x".repeat(72));
    assert_eq!(line.len(), 100);
    let path = dir.file("big.jsonl", &line.repeat(50_000));
    let sampled = sample_local_file(Path::new(&path), 1_048_576).expect("sample");
    // 10485 whole 100-byte lines fit in the first MiB; the 10486th is cut.
    assert_eq!(sampled.len(), 10_485);
    let tiny = sample_local_file(Path::new(&path), 250).expect("sample");
    assert_eq!(tiny.len(), 2);
    assert!(sample_local_file(Path::new(&path), 0)
        .expect("sample")
        .is_empty());
    let result = detect_lines(&sampled);
    assert!(result.is_valid);
    assert_eq!(result.format_type, FormatType::SagemakerEval);
}

#[test]
fn local_sampling_refuses_missing_directories_and_links() {
    let dir = TempDir::new("refuse");
    let missing = dir.join("none.jsonl");
    let error = sample_local_file(&missing, 100).expect_err("missing");
    assert!(error.contains("does not exist"), "{error}");
    let error = sample_local_file(&dir, 100).expect_err("directory");
    assert!(error.contains("not a regular file"), "{error}");
    #[cfg(unix)]
    {
        let target = dir.file("real.jsonl", "{}\n");
        let link = dir.join("link.jsonl");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let error = sample_local_file(&link, 100).expect_err("symlink");
        assert!(error.contains("symbolic link"), "{error}");
    }
}

// ---------------------------------------------------------------------------
// S3 sampling through an injected object store
// ---------------------------------------------------------------------------

struct FakeStore {
    data: Vec<u8>,
    calls: RefCell<Vec<(String, String, String)>>,
}

impl FakeStore {
    fn new(data: &[u8]) -> Self {
        Self {
            data: data.to_vec(),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl ObjectStore for FakeStore {
    fn get_range(&self, bucket: &str, key: &str, range: &str) -> Result<Vec<u8>, String> {
        self.calls
            .borrow_mut()
            .push((bucket.to_string(), key.to_string(), range.to_string()));
        Ok(self.data.clone())
    }
}

#[test]
fn s3_uris_are_validated_with_the_script_messages() {
    assert_eq!(parse_s3_uri("s3://b/k/x.jsonl"), Ok(("b", "k/x.jsonl")));
    for (uri, reason) in [
        ("http://b/k", "must start with 's3://'"),
        ("b/k", "must start with 's3://'"),
        ("s3://bucket", "must contain bucket and key"),
        ("s3://bucket/", "must contain bucket and key"),
        ("s3:///key", "must contain bucket and key"),
        ("s3://", "must contain bucket and key"),
    ] {
        let error = parse_s3_uri(uri).expect_err(uri);
        assert_eq!(error, format!("Invalid S3 URI: {reason} (got: {uri})"));
    }
}

#[test]
fn s3_sampling_sends_a_range_and_drops_the_partial_line() {
    let store = FakeStore::new(b"{\"a\":1}\n{\"b\":2}\n{\"par");
    let sampled = sample_s3_file("s3://bkt/dir/data.jsonl", 1024, &store).expect("sample");
    assert_eq!(sampled, ["{\"a\":1}", "{\"b\":2}"].map(String::from));
    assert_eq!(
        store.calls.borrow().as_slice(),
        [(
            "bkt".to_string(),
            "dir/data.jsonl".to_string(),
            "bytes=0-1023".to_string()
        )]
    );
}

#[test]
fn s3_sampling_never_exceeds_the_sample_size() {
    let store = FakeStore::new(b"aa\nbb\ncc\n");
    let sampled = sample_s3_file("s3://b/k", 6, &store).expect("sample");
    assert_eq!(sampled, ["aa", "bb"].map(String::from));
    let error = sample_s3_file("s3://b/k", 0, &store).expect_err("zero");
    assert!(error.contains("at least 1 byte"), "{error}");
    assert_eq!(store.calls.borrow().len(), 1, "no read for sample size 0");
    let error = sample_s3_file("s3://b", 10, &store).expect_err("bad uri");
    assert!(error.starts_with("Invalid S3 URI"), "{error}");
}

#[test]
fn builds_without_an_s3_client_report_it() {
    let error = sample_s3_file("s3://b/k", 10, &NoObjectStore).expect_err("no s3");
    assert!(error.contains("S3 access is not available"), "{error}");
}

fn run_detect_with(args: &[&str], store: &dyn ObjectStore) -> (u8, String, String) {
    let Ok(Routed::Run(_, invocation)) = route(COMMANDS, &words(args)) else {
        panic!("{args:?} must route");
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = {
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        detect_format_with(&invocation, &mut output, store).expect("handler");
        output.finish().expect("finish");
        output.exit_status()
    };
    (
        status,
        String::from_utf8(out).expect("utf-8"),
        String::from_utf8(err).expect("utf-8"),
    )
}

#[test]
fn detect_format_reads_s3_objects_through_the_store() {
    let store = FakeStore::new(b"{\"query\":\"q\",\"response\":\"r\"}\n");
    let (status, stdout, stderr) = run_detect_with(
        &[
            "ai-data",
            "detect-format",
            "s3://b/k.jsonl",
            "--sample-size",
            "64",
        ],
        &store,
    );
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "Format: sagemaker_eval\nValid: \u{2713}\nConfidence: HIGH\nLines sampled: 1\n"
    );
    assert_eq!(store.calls.borrow()[0].2, "bytes=0-63");
}

// ---------------------------------------------------------------------------
// detect-format command
// ---------------------------------------------------------------------------

#[test]
fn detect_format_text_report_for_a_valid_file() {
    let dir = TempDir::new("text");
    let path = dir.file("ok.jsonl", &format!("{SAGEMAKER_EVAL}\n"));
    let (status, stdout, stderr) = exec(&["ai-data", "detect-format", &path]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "Format: sagemaker_eval\nValid: \u{2713}\nConfidence: HIGH\nLines sampled: 1\n"
    );
}

#[test]
fn detect_format_text_report_lists_errors_and_exits_one() {
    let dir = TempDir::new("invalid");
    let path = dir.file("bad.jsonl", &format!("{OPEN_SFT}\n{SAGEMAKER_EVAL}\n"));
    let (status, stdout, stderr) = exec(&["ai-data", "detect-format", &path]);
    assert_eq!((status, stderr.as_str()), (1, ""));
    assert_eq!(
        stdout,
        "Format: open_weights_sft\nValid: \u{2717}\nConfidence: LOW\nLines sampled: 2\nErrors:\n  Line 2: Expected open_weights_sft but found sagemaker_eval\n"
    );
}

#[test]
fn detect_format_json_report_is_the_scripts_document() {
    let dir = TempDir::new("json");
    let path = dir.file("ok.jsonl", &format!("{SAGEMAKER_EVAL}\n"));
    let (status, stdout, _) = exec(&["ai-data", "detect-format", &path, "--json"]);
    assert_eq!(status, 0);
    assert_eq!(
        stdout,
        "{\n  \"format_type\": \"sagemaker_eval\",\n  \"is_valid\": true,\n  \"confidence\": \"high\",\n  \"lines_sampled\": 1,\n  \"errors\": []\n}\n"
    );
    let path = dir.file("bad.jsonl", &format!("{OPEN_SFT}\n{SAGEMAKER_EVAL}\n"));
    let (status, stdout, _) = exec(&["ai-data", "detect-format", "--json", &path]);
    assert_eq!(status, 1);
    assert!(stdout.contains(
        "  \"errors\": [\n    {\n      \"line_number\": 2,\n      \"error_type\": \"schema_mismatch\",\n      \"message\": \"Expected open_weights_sft but found sagemaker_eval\"\n    }\n  ]\n"
    ));
    assert!(!stdout.contains("errors_omitted"));
}

#[test]
fn detect_format_json_is_ascii_only() {
    let dir = TempDir::new("ascii");
    let path = dir.file(
        "role.jsonl",
        "{\"messages\":[{\"role\":\"\u{e9}\u{1f600}\\u007f\",\"content\":\"x\"}]}\n",
    );
    let (status, stdout, _) = exec(&["ai-data", "detect-format", &path, "--json"]);
    assert_eq!(status, 1);
    assert!(stdout.is_ascii(), "{stdout}");
    assert!(
        stdout.contains("Invalid role '\\u00e9\\ud83d\\ude00\\u007f' in message 0"),
        "{stdout}"
    );
}

#[test]
fn detect_format_reports_unreadable_inputs_on_stderr() {
    let dir = TempDir::new("errors");
    let missing = dir.join("nope.jsonl").to_string_lossy().into_owned();
    let (status, stdout, stderr) = exec(&["ai-data", "detect-format", &missing]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(
        stderr.starts_with("Error: path does not exist: "),
        "{stderr}"
    );
    let (status, stdout, stderr) = exec(&["ai-data", "detect-format", "s3://bucket/key.jsonl"]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_eq!(
        stderr,
        "Error: S3 access is not available in this build; use a local file path.\n"
    );
    let (status, _, stderr) = exec(&["ai-data", "detect-format", "s3://bucket"]);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        "Error: Invalid S3 URI: must contain bucket and key (got: s3://bucket)\n"
    );
}

#[test]
fn detect_format_sample_size_is_bounded_and_validated() {
    let dir = TempDir::new("size");
    let path = dir.file("a.jsonl", &format!("{OPEN_SFT}\n"));
    for args in [
        vec!["ai-data", "detect-format"],
        vec![
            "ai-data",
            "detect-format",
            path.as_str(),
            "--sample-size",
            "abc",
        ],
        vec![
            "ai-data",
            "detect-format",
            path.as_str(),
            "--sample-size",
            "-1",
        ],
        vec![
            "ai-data",
            "detect-format",
            path.as_str(),
            "--sample-size=-1",
        ],
        vec![
            "ai-data",
            "detect-format",
            path.as_str(),
            "--sample-size",
            " -1",
        ],
        vec![
            "ai-data",
            "detect-format",
            &path,
            "--sample-size",
            "16777217",
        ],
        vec![
            "ai-data",
            "detect-format",
            &path,
            "--sample-size",
            "99999999999999999999999999999999999999999",
        ],
        vec!["ai-data", "detect-format", path.as_str(), "--sample-size"],
        vec!["ai-data", "detect-format", path.as_str(), "extra"],
    ] {
        let (status, stdout, stderr) = exec(&args);
        assert_eq!((status, stdout.as_str()), (2, ""), "{args:?}");
        assert!(
            stderr.starts_with("error: invalid arguments"),
            "{args:?}: {stderr}"
        );
    }
    let (status, stdout, _) = exec(&[
        "ai-data",
        "detect-format",
        &path,
        "--sample-size",
        "16777216",
    ]);
    assert_eq!(status, 0, "{stdout}");
    let (status, stdout, _) = exec(&["ai-data", "detect-format", &path, "--sample-size", "0"]);
    assert_eq!(status, 0);
    assert!(stdout.contains("Format: unknown"));
    assert!(stdout.contains("Lines sampled: 0"));
}

#[test]
fn detect_format_samples_a_large_file_without_reading_it_all() {
    let dir = TempDir::new("large");
    let line = format!("{OPEN_SFT}\n");
    let path = dir.file("large.jsonl", &line.repeat(200_000));
    let (status, stdout, _) = exec(&["ai-data", "detect-format", &path, "--json"]);
    assert_eq!(status, 0);
    // 1 MiB of 32-byte lines.
    let per_sample = 1_048_576 / line.len();
    assert!(
        stdout.contains(&format!("\"lines_sampled\": {per_sample}")),
        "{stdout}"
    );
}

// ---------------------------------------------------------------------------
// validate-metrics
// ---------------------------------------------------------------------------

fn metric(definition_fields: &str) -> String {
    format!(r#"{{"customMetricDefinition":{{{definition_fields}}}}}"#)
}

fn valid_definition() -> &'static str {
    r#""name":"helpfulness","instructions":"Rate {{prompt}} against {{prediction}}.""#
}

fn failures(raw: &str) -> Vec<String> {
    metrics::validate(raw).expect_err("must fail")
}

#[test]
fn valid_metrics_are_counted() {
    let one = format!("[{}]", metric(valid_definition()));
    assert_eq!(metrics::validate(&one), Ok(1));
    let scale = metric(&format!(
        r#"{},"ratingScale":[{{"definition":"good","value":{{"floatValue":1}}}},{{"definition":"bad","value":{{"floatValue":0.5}}}}]"#,
        valid_definition()
    ));
    let strings = metric(&format!(
        r#"{},"ratingScale":[{{"definition":"good","value":{{"stringValue":"yes"}}}}]"#,
        valid_definition()
    ));
    assert_eq!(metrics::validate(&format!("[{scale},{strings}]")), Ok(2));
    // Extra keys are ignored and a null or empty scale is allowed.
    let loose = metric(&format!(
        r#"{},"extra":1,"ratingScale":null"#,
        valid_definition()
    ));
    let empty = metric(&format!(r#"{},"ratingScale":[]"#, valid_definition()));
    assert_eq!(metrics::validate(&format!("[{loose},{empty}]")), Ok(2));
}

#[test]
fn the_array_shape_is_checked_first() {
    assert!(failures("{")
        .pop()
        .expect("one")
        .starts_with("Invalid JSON: "));
    assert_eq!(
        failures("{}"),
        ["Must be a JSON array of metric definitions."]
    );
    assert_eq!(
        failures("5"),
        ["Must be a JSON array of metric definitions."]
    );
    assert_eq!(
        failures("[]"),
        ["Array is empty \u{2014} need at least one metric."]
    );
    let eleven = vec![metric(valid_definition()); 11].join(",");
    assert_eq!(
        failures(&format!("[{eleven}]")),
        ["Too many metrics (11). Maximum is 10."]
    );
    let ten = vec![metric(valid_definition()); 10].join(",");
    assert_eq!(metrics::validate(&format!("[{ten}]")), Ok(10));
}

#[test]
fn metrics_must_be_objects_with_a_definition() {
    assert_eq!(
        failures("[5]"),
        ["Metric [0]: 1 validation error for CustomMetric\n  Input should be a valid dictionary or instance of CustomMetric [type=model_type, input_type=int]"]
    );
    assert_eq!(
        failures("[{}]"),
        ["Metric [0]: 1 validation error for CustomMetric\ncustomMetricDefinition\n  Field required [type=missing, input_type=dict]"]
    );
    assert_eq!(
        failures(r#"[{"customMetricDefinition":[]}]"#),
        ["Metric [0]: 1 validation error for CustomMetric\ncustomMetricDefinition\n  Input should be a valid dictionary or instance of CustomMetricDefinition [type=model_type, input_type=list]"]
    );
}

#[test]
fn field_errors_are_reported_together_in_field_order() {
    let raw = format!("[{}]", metric(r#""instructions":5,"ratingScale":"x""#));
    assert_eq!(
        failures(&raw),
        ["Metric [0]: 3 validation errors for CustomMetric\ncustomMetricDefinition.name\n  Field required [type=missing, input_type=dict]\ncustomMetricDefinition.instructions\n  Input should be a valid string [type=string_type, input_type=int]\ncustomMetricDefinition.ratingScale\n  Input should be a valid list [type=list_type, input_type=str]"]
    );
}

#[test]
fn instructions_need_a_placeholder_and_a_bounded_length() {
    let build = |instructions: &str| {
        format!(
            "[{}]",
            metric(&format!(r#""name":"n","instructions":"{instructions}""#))
        )
    };
    assert_eq!(metrics::validate(&build("{{prompt}}")), Ok(1));
    assert_eq!(metrics::validate(&build("{{prediction}}")), Ok(1));
    let errors = failures(&build("no placeholder"));
    assert_eq!(
        errors,
        ["Metric [0]: 1 validation error for CustomMetric\ncustomMetricDefinition\n  Value error, Instructions must contain at least {{prompt}} or {{prediction}}. [type=value_error, input_type=dict]"]
    );
    // Single braces are not the placeholder.
    assert!(metrics::validate(&build("{prompt}")).is_err());

    let at_limit = format!("{{{{prompt}}}}{}", "a".repeat(5000 - 10));
    assert_eq!(at_limit.chars().count(), 5000);
    assert_eq!(metrics::validate(&build(&at_limit)), Ok(1));
    let over = format!("{at_limit}a");
    let errors = failures(&build(&over));
    assert!(
        errors[0].contains("Value error, Instructions exceed 5000 char limit (5001)."),
        "{errors:?}"
    );
}

#[test]
fn lengths_count_characters_not_bytes() {
    let build = |instructions: &str| {
        format!(
            "[{}]",
            metric(&format!(r#""name":"n","instructions":"{instructions}""#))
        )
    };
    // 5000 two-byte characters are within the limit; 5001 are not.
    let at_limit = format!("{{{{prompt}}}}{}", "\u{e9}".repeat(4990));
    assert_eq!(metrics::validate(&build(&at_limit)), Ok(1));
    let over = format!("{at_limit}\u{e9}");
    assert!(metrics::validate(&build(&over)).is_err());
}

fn scale_metric(entries: &str) -> String {
    format!(
        "[{}]",
        metric(&format!(
            r#"{},"ratingScale":[{entries}]"#,
            valid_definition()
        ))
    )
}

#[test]
fn definitions_are_limited_to_100_characters() {
    let entry =
        |definition: &str| format!(r#"{{"definition":"{definition}","value":{{"floatValue":1}}}}"#);
    assert_eq!(
        metrics::validate(&scale_metric(&entry(&"d".repeat(100)))),
        Ok(1)
    );
    assert_eq!(
        metrics::validate(&scale_metric(&entry(&"\u{e9}".repeat(100)))),
        Ok(1)
    );
    assert_eq!(
        failures(&scale_metric(&entry(&"d".repeat(101)))),
        ["Metric [0]: 1 validation error for CustomMetric\ncustomMetricDefinition.ratingScale.0.definition\n  Value error, Definition exceeds 100 chars (101). [type=value_error, input_type=dict]"]
    );
}

#[test]
fn a_rating_value_holds_exactly_one_value() {
    let both = r#"{"definition":"d","value":{"floatValue":1,"stringValue":"a"}}"#;
    let neither = r#"{"definition":"d","value":{}}"#;
    let null_values = r#"{"definition":"d","value":{"floatValue":null,"stringValue":null}}"#;
    for entry in [both, neither, null_values] {
        assert_eq!(
            failures(&scale_metric(entry)),
            ["Metric [0]: 1 validation error for CustomMetric\ncustomMetricDefinition.ratingScale.0.value\n  Value error, Exactly one of 'floatValue' or 'stringValue' must be set. [type=value_error, input_type=dict]"],
            "{entry}"
        );
    }
}

#[test]
fn rating_value_types_follow_pydantic_coercion() {
    let entry = |value: &str| format!(r#"{{"definition":"d","value":{{{value}}}}}"#);
    for ok in [
        r#""floatValue":"1.5""#,
        r#""floatValue":" 2 ""#,
        r#""floatValue":0"#,
        r#""floatValue":true"#,
        r#""stringValue":"""#,
    ] {
        assert_eq!(metrics::validate(&scale_metric(&entry(ok))), Ok(1), "{ok}");
    }
    let errors = failures(&scale_metric(&entry(r#""floatValue":"abc""#)));
    assert!(
        errors[0].contains("customMetricDefinition.ratingScale.0.value.floatValue\n  Input should be a valid number, unable to parse string as a number [type=float_parsing, input_type=str]"),
        "{errors:?}"
    );
    let errors = failures(&scale_metric(&entry(r#""floatValue":[1]"#)));
    assert!(errors[0].contains("Input should be a valid number [type=float_type, input_type=list]"));
    let errors = failures(&scale_metric(&entry(r#""stringValue":5"#)));
    assert!(errors[0].contains("customMetricDefinition.ratingScale.0.value.stringValue\n  Input should be a valid string [type=string_type, input_type=int]"));
}

#[test]
fn a_scale_must_not_mix_value_types() {
    let float = r#"{"definition":"a","value":{"floatValue":1}}"#;
    let string = r#"{"definition":"b","value":{"stringValue":"x"}}"#;
    assert_eq!(
        failures(&scale_metric(&format!("{float},{string}"))),
        ["Metric [0]: 1 validation error for CustomMetric\ncustomMetricDefinition\n  Value error, ratingScale mixes float and string values. Use one type. [type=value_error, input_type=dict]"]
    );
    assert_eq!(
        metrics::validate(&scale_metric(&format!("{float},{float}"))),
        Ok(1)
    );
}

#[test]
fn entry_shapes_are_checked() {
    let errors = failures(&scale_metric(
        r#"5,{"value":{"floatValue":1}},{"definition":"d"}"#,
    ));
    let text = &errors[0];
    assert!(text.starts_with("Metric [0]: 3 validation errors for CustomMetric\n"));
    assert!(text.contains("customMetricDefinition.ratingScale.0\n  Input should be a valid dictionary or instance of RatingScaleEntry [type=model_type, input_type=int]"));
    assert!(text.contains("customMetricDefinition.ratingScale.1.definition\n  Field required"));
    assert!(text.contains("customMetricDefinition.ratingScale.2.value\n  Field required"));
}

#[test]
fn field_errors_suppress_the_model_level_rules() {
    // Missing name AND no placeholder: only the field error is reported.
    let raw = format!("[{}]", metric(r#""instructions":"nothing""#));
    let errors = failures(&raw);
    assert!(errors[0].starts_with("Metric [0]: 1 validation error for CustomMetric\n"));
    assert!(errors[0].contains("customMetricDefinition.name\n  Field required"));
    // A too-long instructions text stops before the scale check.
    let long = "a".repeat(5001);
    let raw = format!(
        "[{}]",
        metric(&format!(
            r#""name":"n","instructions":"{long}","ratingScale":[{{"definition":"a","value":{{"floatValue":1}}}},{{"definition":"b","value":{{"stringValue":"x"}}}}]"#
        ))
    );
    let errors = failures(&raw);
    assert!(errors[0].starts_with("Metric [0]: 1 validation error"));
    assert!(errors[0].contains("exceed 5000"));
}

#[test]
fn every_failing_metric_is_reported_with_its_index() {
    let good = metric(valid_definition());
    let raw = format!("[{good},{{}},{good},3]");
    let errors = failures(&raw);
    assert_eq!(errors.len(), 2);
    assert!(errors[0].starts_with("Metric [1]: "));
    assert!(errors[1].starts_with("Metric [3]: "));
}

#[test]
fn validate_metrics_command_prints_the_verdict() {
    let one = format!("[{}]", metric(valid_definition()));
    let (status, stdout, stderr) = exec(&["ai-data", "validate-metrics", &one]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(stdout, "\u{2705} Valid \u{2014} 1 custom metric defined.\n");
    let long = format!(
        "[{}]",
        metric(&format!(
            r#""name":"n","instructions":"{{{{prompt}}}} {}""#,
            "x".repeat(300)
        ))
    );
    assert!(long.len() > 255);
    let (status, stdout, _) = exec(&["ai-data", "validate-metrics", &long]);
    assert_eq!(status, 0, "inline JSON longer than a file name: {stdout}");
    let two = format!("[{0},{0}]", metric(valid_definition()));
    let (_, stdout, _) = exec(&["ai-data", "validate-metrics", &two]);
    assert_eq!(
        stdout,
        "\u{2705} Valid \u{2014} 2 custom metrics defined.\n"
    );

    let (status, stdout, stderr) = exec(&["ai-data", "validate-metrics", "[]"]);
    assert_eq!((status, stderr.as_str()), (1, ""));
    assert_eq!(
        stdout,
        "\u{274c} Validation failed:\n  - Array is empty \u{2014} need at least one metric.\n"
    );
}

#[test]
fn validate_metrics_without_an_argument_prints_usage_and_exits_one() {
    let (status, stdout, stderr) = exec(&["ai-data", "validate-metrics"]);
    assert_eq!((status, stderr.as_str()), (1, ""));
    assert_eq!(
        stdout,
        "Usage: toolkit ai-data validate-metrics '<json>' | file.json\n"
    );
}

#[test]
fn validate_metrics_reads_a_file_before_treating_the_argument_as_json() {
    let dir = TempDir::new("metrics");
    let good = format!("[{}]", metric(valid_definition()));
    let path = dir.file("metrics.json", &good);
    let (status, stdout, _) = exec(&["ai-data", "validate-metrics", &path]);
    assert_eq!(status, 0, "{stdout}");
    // A path that does not exist, and a directory, are inline JSON (and fail to parse).
    for argument in [
        dir.join("none.json").to_string_lossy().into_owned(),
        dir.to_string_lossy().into_owned(),
    ] {
        let (status, stdout, _) = exec(&["ai-data", "validate-metrics", &argument]);
        assert_eq!(status, 1);
        assert!(stdout.contains("Invalid JSON"), "{stdout}");
    }
}

#[test]
fn validate_metrics_refuses_unsafe_or_unreadable_files() {
    let dir = TempDir::new("metrics-unsafe");
    let big = dir.file("big.json", &" ".repeat(1024 * 1024 + 1));
    let (status, stdout, stderr) = exec(&["ai-data", "validate-metrics", &big]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("larger than"), "{stderr}");
    let bytes = dir.join("latin1.json");
    fs::write(&bytes, [0xff_u8, 0xfe]).expect("write");
    let (status, _, stderr) = exec(&["ai-data", "validate-metrics", &bytes.to_string_lossy()]);
    assert_eq!(status, 1);
    assert!(stderr.contains("not valid UTF-8"), "{stderr}");
    #[cfg(unix)]
    {
        let target = dir.file("real.json", "[]");
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let (status, stdout, stderr) =
            exec(&["ai-data", "validate-metrics", &link.to_string_lossy()]);
        assert_eq!((status, stdout.as_str()), (1, ""));
        assert!(stderr.contains("symbolic link"), "{stderr}");
    }
}

// ---------------------------------------------------------------------------
// transform
// ---------------------------------------------------------------------------

fn request(script: &Path, source: &str, output: &str) -> TransformRequest {
    TransformRequest {
        script: script.to_path_buf(),
        source_s3: source.to_string(),
        output_s3: output.to_string(),
        instance_type: "ml.m5.xlarge".to_string(),
        region: None,
        execution_role: None,
        base_job_name: "dataset-transformation".to_string(),
        image_uri: None,
    }
}

struct FakeSage {
    log: RefCell<Vec<String>>,
    started: RefCell<Option<ProcessingPlan>>,
    details: JobDetails,
}

impl FakeSage {
    fn new() -> Self {
        Self {
            log: RefCell::new(Vec::new()),
            started: RefCell::new(None),
            details: JobDetails {
                status: Some("Completed".to_string()),
                failure_reason: None,
                creation_time: "2026-10-27 10:00:00+00:00".to_string(),
                processing_end_time: String::new(),
                inputs: vec![Json::string("in")],
                outputs: vec![],
            },
        }
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

impl ProcessingService for FakeSage {
    fn default_role(&self) -> Result<String, String> {
        self.log.borrow_mut().push("role".to_string());
        Ok("arn:aws:iam::123456789012:role/Exec".to_string())
    }

    fn default_region(&self) -> Result<String, String> {
        self.log.borrow_mut().push("region".to_string());
        Ok("eu-west-1".to_string())
    }

    fn sklearn_image(&self, region: &str, version: &str, instance: &str) -> Result<String, String> {
        self.log
            .borrow_mut()
            .push(format!("image {region} {version} {instance}"));
        Ok("sklearn-image".to_string())
    }

    fn start(&self, plan: &ProcessingPlan) -> Result<String, String> {
        self.log.borrow_mut().push("start".to_string());
        *self.started.borrow_mut() = Some(plan.clone());
        Ok("dataset-transformation-2026".to_string())
    }

    fn describe(&self, job_name: &str, region: Option<&str>) -> Result<JobDetails, String> {
        self.log
            .borrow_mut()
            .push(format!("describe {job_name} {region:?}"));
        Ok(self.details.clone())
    }
}

#[test]
fn input_and_output_arguments_use_the_source_file_name() {
    let dir = TempDir::new("plan");
    let script = dir.file("transform.py", "print('x')\n");
    for (source, name) in [
        ("s3://bucket/data/train.jsonl", "train.jsonl"),
        ("s3://bucket/data/", "data"),
        ("s3://bucket/data///", "data"),
        ("s3://bucket", "bucket"),
        ("s3://bucket/", "bucket"),
    ] {
        let plan = build_plan(&request(Path::new(&script), source, "s3://out/x/")).expect(source);
        assert_eq!(
            plan.arguments,
            [
                "--input".to_string(),
                format!("/opt/ml/processing/input/{name}"),
                "--output".to_string(),
                format!("/opt/ml/processing/output/{name}"),
            ],
            "{source}"
        );
    }
}

#[test]
fn the_plan_names_the_script_and_its_directory() {
    let dir = TempDir::new("plan-script");
    let script = dir.file("transform.py", "x\n");
    let plan = build_plan(&request(Path::new(&script), "s3://b/k", "s3://o/p")).expect("plan");
    assert_eq!(plan.code, "transform.py");
    assert_eq!(Path::new(&plan.source_dir), &*dir);
    // `..` is kept as written, so the directory is what the OS resolves.
    fs::create_dir_all(dir.join("sub")).expect("mkdir");
    let indirect = dir.join("sub").join("..").join("transform.py");
    let plan = build_plan(&request(&indirect, "s3://b/k", "s3://o/p")).expect("plan");
    assert!(plan.source_dir.ends_with("sub/.."), "{}", plan.source_dir);
    assert_eq!(plan.code, "transform.py");
}

#[test]
fn the_script_must_be_a_regular_file() {
    let dir = TempDir::new("plan-refuse");
    let error =
        build_plan(&request(&dir.join("none.py"), "s3://b/k", "s3://o/p")).expect_err("missing");
    assert!(error.contains("does not exist"), "{error}");
    let error = build_plan(&request(&dir, "s3://b/k", "s3://o/p")).expect_err("directory");
    assert!(error.contains("not a regular file"), "{error}");
    #[cfg(unix)]
    {
        let target = dir.file("real.py", "x\n");
        let link = dir.join("link.py");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let error = build_plan(&request(&link, "s3://b/k", "s3://o/p")).expect_err("symlink");
        assert!(error.contains("symbolic link"), "{error}");
    }
}

#[test]
fn s3_locations_must_name_a_bucket() {
    let dir = TempDir::new("plan-uri");
    let script = dir.file("t.py", "x\n");
    for bad in ["https://b/k", "b/k", "s3://", "s3:///k", ""] {
        let error = build_plan(&request(Path::new(&script), bad, "s3://o/p")).expect_err(bad);
        assert!(error.starts_with("Invalid source dataset"), "{error}");
        let error = build_plan(&request(Path::new(&script), "s3://b/k", bad)).expect_err(bad);
        assert!(error.starts_with("Invalid output location"), "{error}");
    }
    assert!(build_plan(&request(Path::new(&script), "s3://b", "s3://o")).is_ok());
}

#[test]
fn submit_resolves_defaults_in_the_scripts_order() {
    let dir = TempDir::new("submit");
    let script = dir.file("t.py", "x\n");
    let service = FakeSage::new();
    let message = submit(
        &request(Path::new(&script), "s3://b/d/train.jsonl", "s3://o/out/"),
        &service,
    )
    .expect("submit");
    assert_eq!(
        message,
        "Processing job 'dataset-transformation-2026' submitted. Output will be at: s3://o/out/"
    );
    assert_eq!(
        service.calls(),
        [
            "role",
            "region",
            "image eu-west-1 1.2-1 ml.m5.xlarge",
            "start"
        ]
    );
    let started = service.started.borrow().clone().expect("started");
    assert_eq!(
        started.execution_role.as_deref(),
        Some("arn:aws:iam::123456789012:role/Exec")
    );
    assert_eq!(started.region.as_deref(), Some("eu-west-1"));
    assert_eq!(started.image_uri.as_deref(), Some("sklearn-image"));
}

#[test]
fn submit_keeps_explicit_role_region_and_image() {
    let dir = TempDir::new("submit-explicit");
    let script = dir.file("t.py", "x\n");
    let service = FakeSage::new();
    let mut explicit = request(Path::new(&script), "s3://b/k", "s3://o/p");
    explicit.region = Some("us-west-2".to_string());
    explicit.execution_role = Some("arn:role".to_string());
    explicit.image_uri = Some("my-image".to_string());
    submit(&explicit, &service).expect("submit");
    assert_eq!(service.calls(), ["start"]);
    let started = service.started.borrow().clone().expect("started");
    assert_eq!(started.region.as_deref(), Some("us-west-2"));
    assert_eq!(started.image_uri.as_deref(), Some("my-image"));
    // Only the image is looked up when role and region are given.
    let service = FakeSage::new();
    explicit.image_uri = None;
    submit(&explicit, &service).expect("submit");
    assert_eq!(
        service.calls(),
        ["image us-west-2 1.2-1 ml.m5.xlarge", "start"]
    );
}

#[test]
fn a_build_without_sagemaker_cannot_submit_or_describe() {
    let dir = TempDir::new("no-sagemaker");
    let script = dir.file("t.py", "x\n");
    let error = submit(
        &request(Path::new(&script), "s3://b/k", "s3://o/p"),
        &NoSageMaker,
    )
    .expect_err("no sdk");
    assert_eq!(error, "SageMaker SDK access is not available in this build");
    let error = describe("job", None, &NoSageMaker).expect_err("no sdk");
    assert_eq!(error, "SageMaker SDK access is not available in this build");
}

#[test]
fn describe_maps_the_job_like_the_script() {
    let service = FakeSage::new();
    let details = describe("my-job", Some("us-east-1"), &service).expect("describe");
    assert_eq!(
        details.to_pretty_string(),
        "{\n  \"job_name\": \"my-job\",\n  \"status\": \"Completed\",\n  \"failure_reason\": null,\n  \"creation_time\": \"2026-10-27 10:00:00+00:00\",\n  \"processing_end_time\": \"\",\n  \"inputs\": [\n    \"in\"\n  ],\n  \"outputs\": []\n}"
    );
    assert_eq!(service.calls(), ["describe my-job Some(\"us-east-1\")"]);
    let error = describe("", None, &service).expect_err("empty");
    assert!(error.contains("must not be empty"));
}

#[test]
fn transform_commands_route_with_their_options() {
    let dir = TempDir::new("cli");
    let script = dir.file("t.py", "x\n");
    let (status, stdout, stderr) = exec(&[
        "ai-data",
        "transform",
        "plan",
        "--script",
        &script,
        "--source-s3=s3://b/d/train.jsonl",
        "--output-s3",
        "s3://o/out/",
        "--instance-type",
        "ml.m5.2xlarge",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(
        stdout.contains("\"instance_type\": \"ml.m5.2xlarge\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"code\": \"t.py\""));
    assert!(stdout.contains("\"role\": null"));
    assert!(stdout.contains("\"s3_upload_mode\": \"EndOfJob\""));
    assert!(stdout.contains("\"/opt/ml/processing/input/train.jsonl\""));
    assert!(stdout.contains("\"wait\": false"));
}

#[test]
fn transform_submit_and_describe_fail_cleanly_in_this_build() {
    let dir = TempDir::new("cli-submit");
    let script = dir.file("t.py", "x\n");
    let (status, stdout, stderr) = exec(&[
        "ai-data",
        "transform",
        "submit",
        "--script",
        &script,
        "--source-s3",
        "s3://b/k",
        "--output-s3",
        "s3://o/p",
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert_eq!(
        stderr,
        "error: SageMaker SDK access is not available in this build\n"
    );
    let (status, stdout, stderr) = exec(&["ai-data", "transform", "describe", "job-1"]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.contains("not available in this build"));
}

#[test]
fn transform_arguments_are_validated() {
    for args in [
        vec!["ai-data", "transform", "plan"],
        vec!["ai-data", "transform", "plan", "--script", "t.py"],
        vec![
            "ai-data",
            "transform",
            "plan",
            "--script",
            "t.py",
            "--source-s3",
            "s3://b/k",
        ],
        vec![
            "ai-data",
            "transform",
            "plan",
            "--script",
            "--source-s3",
            "s3://b/k",
            "--output-s3",
            "s3://o",
        ],
        vec!["ai-data", "transform", "describe"],
        vec!["ai-data", "transform", "describe", "a", "b"],
        vec!["ai-data", "transform", "describe", "job", "--bogus", "x"],
        vec!["ai-data", "transform"],
    ] {
        let (status, stdout, stderr) = exec(&args);
        assert_eq!((status, stdout.as_str()), (2, ""), "{args:?}");
        assert!(
            stderr.starts_with("error: invalid arguments"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn python_type_names_match_the_scripts_messages() {
    let parse = |text: &str| toolkit_core::parse_json(text).expect("json");
    for (text, name) in [
        ("null", "NoneType"),
        ("true", "bool"),
        ("3", "int"),
        ("3.5", "float"),
        ("\"s\"", "str"),
        ("[]", "list"),
        ("{}", "dict"),
    ] {
        assert_eq!(py_type_name(&parse(text)), name);
    }
}
