#![allow(
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]
//! Offline fixtures for the Lambda and Fargate processors: the valid
//! EventBridge event, malformed events, error paths and output schema.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use s3_upload_processor::{
    basename, event_fields, handle_lambda_event, run_fargate_task, ObjectStore, Output,
    ProcessorError, StoreError, MAX_KEY_LENGTH,
};
use serde_json::{json, Value};

const OBJECT_CREATED: &str = include_str!("fixtures/object-created.json");

fn object_created() -> Value {
    serde_json::from_str(OBJECT_CREATED).unwrap()
}

/// An event with the given `detail.bucket.name`, `detail.object.key` and size.
fn event(bucket: Value, key: Value, size: Option<Value>) -> Value {
    let mut object = json!({ "key": key });
    if let Some(size) = size {
        object["size"] = size;
    }
    json!({ "detail": { "bucket": { "name": bucket }, "object": object } })
}

/// A scratch directory removed on drop.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "s3-upload-processor-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl Output for Lines {
    fn line(&self, text: &str) {
        self.0.lock().unwrap().push(text.to_string());
    }
}

impl Lines {
    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

enum Behaviour {
    /// Write these bytes to the destination.
    Bytes(Vec<u8>),
    /// Succeed without creating the file.
    NoFile,
    /// Write a partial file, then fail (the store must clean up itself, the
    /// processors must not leave it either).
    Fail(String),
}

struct FakeStore {
    behaviour: Behaviour,
    calls: Mutex<Vec<(String, String, PathBuf)>>,
}

impl FakeStore {
    fn serving(bytes: &[u8]) -> Self {
        Self::with(Behaviour::Bytes(bytes.to_vec()))
    }

    fn with(behaviour: Behaviour) -> Self {
        Self {
            behaviour,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, String, PathBuf)> {
        self.calls.lock().unwrap().clone()
    }
}

impl ObjectStore for FakeStore {
    async fn download(
        &self,
        bucket: &str,
        key: &str,
        destination: &Path,
    ) -> Result<(), StoreError> {
        self.calls.lock().unwrap().push((
            bucket.to_string(),
            key.to_string(),
            destination.to_path_buf(),
        ));
        match &self.behaviour {
            Behaviour::Bytes(bytes) => {
                std::fs::write(destination, bytes).map_err(|e| StoreError(e.to_string()))
            }
            Behaviour::NoFile => Ok(()),
            Behaviour::Fail(message) => Err(StoreError(message.clone())),
        }
    }
}

// ---- Lambda ----

#[tokio::test]
async fn lambda_downloads_the_object_and_returns_its_metadata() {
    let dir = WorkDir::new();
    let store = FakeStore::serving(b"a,b\n1,2\n");
    let out = Lines::default();
    let event = object_created();

    let response = handle_lambda_event(&event, &store, dir.path(), &out)
        .await
        .unwrap();

    let local = dir.path().join("report.csv");
    assert_eq!(
        response,
        json!({
            "statusCode": 200,
            "body": {
                "bucket": "example-uploads",
                "key": "uploads/2024/report.csv",
                "size_bytes": 5368,
                "local_path": local.to_string_lossy(),
                "file_size_on_disk": 8,
            }
        })
    );
    // `body` is an object, not JSON text.
    assert!(response["body"].is_object());
    assert_eq!(
        store.calls(),
        vec![(
            "example-uploads".to_string(),
            "uploads/2024/report.csv".to_string(),
            local.clone()
        )]
    );
    // The temporary file is gone.
    assert!(dir.files().is_empty(), "left behind: {:?}", dir.files());

    let lines = out.all();
    assert_eq!(lines.len(), 3);
    let echoed = lines[0]
        .strip_prefix("Received event: ")
        .expect("event line");
    assert_eq!(serde_json::from_str::<Value>(echoed).unwrap(), event);
    assert_eq!(
        lines[1],
        format!(
            "Downloaded s3://example-uploads/uploads/2024/report.csv to {}",
            local.display()
        )
    );
    let metadata = lines[2]
        .strip_prefix("File metadata: ")
        .expect("metadata line");
    assert_eq!(
        serde_json::from_str::<Value>(metadata).unwrap(),
        response["body"]
    );
}

#[tokio::test]
async fn lambda_defaults_a_missing_size_to_zero_and_echoes_the_event_size() {
    for (size, expected) in [
        (None, json!(0)),
        (Some(json!(12.5)), json!(12.5)),
        (Some(json!(true)), json!(true)),
    ] {
        let dir = WorkDir::new();
        let store = FakeStore::serving(b"x");
        let response = handle_lambda_event(
            &event(json!("b"), json!("k.txt"), size),
            &store,
            dir.path(),
            &Lines::default(),
        )
        .await
        .unwrap();
        assert_eq!(response["body"]["size_bytes"], expected);
        assert_eq!(response["body"]["file_size_on_disk"], 1);
    }
}

#[tokio::test]
async fn lambda_refuses_objects_above_512_mib_before_downloading() {
    let limit = 512u64 * 1024 * 1024;
    let dir = WorkDir::new();
    let store = FakeStore::serving(b"x");
    let at_limit = handle_lambda_event(
        &event(json!("b"), json!("k"), Some(json!(limit))),
        &store,
        dir.path(),
        &Lines::default(),
    )
    .await;
    assert!(at_limit.is_ok(), "exactly 512 MiB is allowed");

    let store = FakeStore::serving(b"x");
    let error = handle_lambda_event(
        &event(json!("b"), json!("k"), Some(json!(limit + 1))),
        &store,
        dir.path(),
        &Lines::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ProcessorError::FileTooLarge));
    assert_eq!(error.to_string(), "File too large for Lambda processing");
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn lambda_rejects_invalid_s3_parameters_with_the_python_message() {
    let cases = [
        ("no detail", json!({})),
        ("empty detail", json!({"detail": {}})),
        ("no bucket", json!({"detail": {"object": {"key": "k"}}})),
        ("empty bucket", event(json!(""), json!("k"), None)),
        ("null bucket", event(Value::Null, json!("k"), None)),
        ("numeric bucket", event(json!(7), json!("k"), None)),
        ("no key", json!({"detail": {"bucket": {"name": "b"}}})),
        ("empty key", event(json!("b"), json!(""), None)),
        ("null key", event(json!("b"), Value::Null, None)),
        ("numeric key", event(json!("b"), json!(5), None)),
        ("list key", event(json!("b"), json!(["k"]), None)),
        ("parent directory", event(json!("b"), json!("a/../b"), None)),
        (
            "leading parent directory",
            event(json!("b"), json!("/../etc/passwd"), None),
        ),
        ("directory key", event(json!("b"), json!("folder/"), None)),
        ("dot-dot object", event(json!("b"), json!(".."), None)),
        ("dot object", event(json!("b"), json!("a/."), None)),
    ];
    for (name, case) in cases {
        let dir = WorkDir::new();
        let store = FakeStore::serving(b"x");
        let error = handle_lambda_event(&case, &store, dir.path(), &Lines::default())
            .await
            .unwrap_err();
        assert!(
            matches!(error, ProcessorError::InvalidPath),
            "{name}: {error:?}"
        );
        assert_eq!(error.to_string(), "Invalid S3 path", "{name}");
        assert!(store.calls().is_empty(), "{name} downloaded");
        assert!(dir.files().is_empty(), "{name} left a file");
    }
}

#[tokio::test]
async fn lambda_accepts_keys_the_slash_dot_dot_check_does_not_catch() {
    // Same rule as the Python script: only the substring `/..` is refused.
    let dir = WorkDir::new();
    let store = FakeStore::serving(b"x");
    let response = handle_lambda_event(
        &event(json!("b"), json!("..hidden"), None),
        &store,
        dir.path(),
        &Lines::default(),
    )
    .await
    .unwrap();
    assert_eq!(response["body"]["key"], "..hidden");
}

#[tokio::test]
async fn lambda_limits_the_key_length_in_characters() {
    let dir = WorkDir::new();
    // A 1024-character key whose last segment is short (a single path segment of
    // that length is not a valid file name on Linux either).
    let ok = format!("{}/f.txt", "a".repeat(MAX_KEY_LENGTH - 6));
    let store = FakeStore::serving(b"x");
    assert!(handle_lambda_event(
        &event(json!("b"), json!(ok), None),
        &store,
        dir.path(),
        &Lines::default()
    )
    .await
    .is_ok());

    // 1024 two-byte characters: 2048 bytes, still 1024 characters.
    let accented = format!("{}/f.txt", "é".repeat(MAX_KEY_LENGTH - 6));
    let store = FakeStore::serving(b"x");
    assert!(handle_lambda_event(
        &event(json!("b"), json!(accented), None),
        &store,
        dir.path(),
        &Lines::default()
    )
    .await
    .is_ok());

    let too_long = "a".repeat(MAX_KEY_LENGTH + 1);
    let store = FakeStore::serving(b"x");
    let error = handle_lambda_event(
        &event(json!("b"), json!(too_long), None),
        &store,
        dir.path(),
        &Lines::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ProcessorError::KeyTooLong));
    assert_eq!(error.to_string(), "S3 key exceeds maximum length");
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn lambda_rejects_events_that_do_not_have_the_eventbridge_shape() {
    let cases = [
        ("string event", json!("text")),
        ("array event", json!([])),
        ("null event", Value::Null),
        ("detail is a string", json!({"detail": "x"})),
        ("detail is null", json!({"detail": null})),
        (
            "bucket is a string",
            json!({"detail": {"bucket": "b", "object": {"key": "k"}}}),
        ),
        (
            "object is a number",
            json!({"detail": {"bucket": {"name": "b"}, "object": 3}}),
        ),
        (
            "size is text",
            event(json!("b"), json!("k"), Some(json!("big"))),
        ),
        (
            "size is null",
            event(json!("b"), json!("k"), Some(Value::Null)),
        ),
        (
            "size is a list",
            event(json!("b"), json!("k"), Some(json!([1]))),
        ),
    ];
    for (name, case) in cases {
        let dir = WorkDir::new();
        let store = FakeStore::serving(b"x");
        let error = handle_lambda_event(&case, &store, dir.path(), &Lines::default())
            .await
            .unwrap_err();
        assert!(
            matches!(error, ProcessorError::Malformed(_)),
            "{name}: {error:?}"
        );
        assert!(error.to_string().starts_with("Malformed event: "), "{name}");
        assert!(store.calls().is_empty(), "{name} downloaded");
    }
}

#[tokio::test]
async fn lambda_checks_the_path_before_the_size() {
    let dir = WorkDir::new();
    let store = FakeStore::serving(b"x");
    // Python: the `ValueError` comes before the size comparison would raise.
    let error = handle_lambda_event(
        &event(json!(""), json!("k"), Some(json!("big"))),
        &store,
        dir.path(),
        &Lines::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ProcessorError::InvalidPath));
}

#[tokio::test]
async fn lambda_reports_download_failures_and_leaves_no_file() {
    let dir = WorkDir::new();
    let store = FakeStore::with(Behaviour::Fail("NoSuchKey: not found".to_string()));
    let out = Lines::default();
    let error = handle_lambda_event(&object_created(), &store, dir.path(), &out)
        .await
        .unwrap_err();
    assert!(matches!(error, ProcessorError::Download(_)));
    assert_eq!(error.to_string(), "NoSuchKey: not found");
    assert!(dir.files().is_empty());
    // Only the received-event line was logged.
    assert_eq!(out.all().len(), 1);
}

#[tokio::test]
async fn lambda_fails_when_the_download_produced_no_file() {
    let dir = WorkDir::new();
    let store = FakeStore::with(Behaviour::NoFile);
    let error = handle_lambda_event(&object_created(), &store, dir.path(), &Lines::default())
        .await
        .unwrap_err();
    assert!(matches!(error, ProcessorError::Io(_)), "{error:?}");
}

#[test]
fn event_fields_read_absent_objects_as_empty() {
    let fields = event_fields(&json!({})).unwrap();
    assert_eq!(fields.bucket, Value::Null);
    assert_eq!(fields.key, Value::Null);
    assert_eq!(fields.size, json!(0));
    assert_eq!(basename("a/b/c.txt"), "c.txt");
    assert_eq!(basename("c.txt"), "c.txt");
    assert_eq!(basename("dir/"), "");
}

// ---- Fargate ----

fn task_event(bucket: Value, key: Value) -> String {
    event(bucket, key, None).to_string()
}

struct Run {
    exit_code: i32,
    stdout: Vec<String>,
    stderr: Vec<String>,
    calls: Vec<(String, String, PathBuf)>,
    files: Vec<String>,
}

async fn run(task_event: Option<&str>, store: FakeStore) -> Run {
    let dir = WorkDir::new();
    let (out, errors) = (Lines::default(), Lines::default());
    let outcome = run_fargate_task(task_event, &store, dir.path(), &out, &errors).await;
    Run {
        exit_code: outcome.exit_code,
        stdout: out.all(),
        stderr: errors.all(),
        calls: store.calls(),
        files: dir.files(),
    }
}

#[tokio::test]
async fn fargate_downloads_the_object_and_reports_its_size() {
    let dir = WorkDir::new();
    let store = FakeStore::serving(&[7; 4096]);
    let (out, errors) = (Lines::default(), Lines::default());
    let text = OBJECT_CREATED;
    let outcome = run_fargate_task(Some(text), &store, dir.path(), &out, &errors).await;

    assert_eq!(outcome.exit_code, 0);
    assert!(errors.all().is_empty());
    let local = dir.path().join("report.csv");
    let lines = out.all();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0],
        format!(
            "Downloaded s3://example-uploads/uploads/2024/report.csv to {}",
            local.display()
        )
    );
    let metadata: Value =
        serde_json::from_str(lines[1].strip_prefix("File metadata: ").unwrap()).unwrap();
    assert_eq!(
        metadata,
        json!({
            "bucket": "example-uploads",
            "key": "uploads/2024/report.csv",
            "local_path": local.to_string_lossy(),
            "file_size_bytes": 4096,
        })
    );
    assert_eq!(lines[2], "Done. File size: 4096 bytes");
    // The container keeps the file; it is discarded with the task.
    assert_eq!(dir.files(), vec!["report.csv".to_string()]);
}

#[tokio::test]
async fn fargate_ignores_the_event_size() {
    let event = event(
        json!("b"),
        json!("big.bin"),
        Some(json!(10_000_000_000_u64)),
    );
    let run = run(Some(&event.to_string()), FakeStore::serving(b"x")).await;
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stdout.last().unwrap(), "Done. File size: 1 bytes");
}

#[tokio::test]
async fn fargate_without_a_bucket_or_key_exits_1_on_stdout() {
    let cases: Vec<(&str, Option<String>)> = vec![
        ("unset", None),
        ("empty object", Some("{}".to_string())),
        (
            "no bucket",
            Some(json!({"detail": {"object": {"key": "k"}}}).to_string()),
        ),
        ("empty bucket", Some(task_event(json!(""), json!("k")))),
        ("null bucket", Some(task_event(Value::Null, json!("k")))),
        (
            "no key",
            Some(json!({"detail": {"bucket": {"name": "b"}}}).to_string()),
        ),
        ("empty key", Some(task_event(json!("b"), json!("")))),
        ("zero key", Some(task_event(json!("b"), json!(0)))),
    ];
    for (name, case) in cases {
        let run = run(case.as_deref(), FakeStore::serving(b"x")).await;
        assert_eq!(run.exit_code, 1, "{name}");
        assert_eq!(
            run.stdout,
            vec!["Error: No bucket or key in event"],
            "{name}"
        );
        assert!(run.stderr.is_empty(), "{name}");
        assert!(run.calls.is_empty(), "{name}");
    }
}

#[tokio::test]
async fn fargate_rejects_invalid_keys_on_stdout() {
    let too_long = "a".repeat(MAX_KEY_LENGTH + 1);
    let cases = [
        ("parent directory", json!("a/../b")),
        ("leading parent directory", json!("/../x")),
        ("too long", json!(too_long)),
        ("number", json!(5)),
        ("list", json!(["k"])),
    ];
    for (name, key) in cases {
        let run = run(Some(&task_event(json!("b"), key)), FakeStore::serving(b"x")).await;
        assert_eq!(run.exit_code, 1, "{name}");
        assert_eq!(run.stdout, vec!["Error: Invalid S3 key"], "{name}");
        assert!(run.calls.is_empty(), "{name}");
    }
}

#[tokio::test]
async fn fargate_accepts_a_key_of_exactly_the_maximum_length() {
    let key = format!("{}/f.txt", "a".repeat(MAX_KEY_LENGTH - 6));
    let run = run(
        Some(&task_event(json!("b"), json!(key))),
        FakeStore::serving(b"x"),
    )
    .await;
    assert_eq!(run.exit_code, 0);
}

#[tokio::test]
async fn fargate_reports_unusable_task_events_on_stderr_with_status_1() {
    let cases = [
        ("not JSON", "not json"),
        ("empty", ""),
        ("array", "[]"),
        ("null", "null"),
        ("detail is text", r#"{"detail": "x"}"#),
        (
            "bucket is an object list",
            r#"{"detail": {"bucket": [], "object": {"key": "k"}}}"#,
        ),
    ];
    for (name, text) in cases {
        let run = run(Some(text), FakeStore::serving(b"x")).await;
        assert_eq!(run.exit_code, 1, "{name}");
        assert!(run.stdout.is_empty(), "{name}: {:?}", run.stdout);
        assert_eq!(run.stderr.len(), 1, "{name}");
        assert!(
            run.stderr[0].starts_with("Error: "),
            "{name}: {}",
            run.stderr[0]
        );
        assert!(run.calls.is_empty(), "{name}");
    }
}

#[tokio::test]
async fn fargate_rejects_a_bucket_that_is_not_text() {
    let run = run(
        Some(&task_event(json!(7), json!("k"))),
        FakeStore::serving(b"x"),
    )
    .await;
    assert_eq!(run.exit_code, 1);
    assert!(run.calls.is_empty());
    assert_eq!(run.stderr, vec!["Error: the bucket name must be a string"]);
}

#[tokio::test]
async fn fargate_rejects_keys_that_do_not_name_a_file() {
    for key in ["folder/", "..", "a/."] {
        let run = run(
            Some(&task_event(json!("b"), json!(key))),
            FakeStore::serving(b"x"),
        )
        .await;
        assert_eq!(run.exit_code, 1, "{key}");
        assert!(run.calls.is_empty(), "{key}");
        assert_eq!(run.stderr.len(), 1, "{key}");
    }
}

#[tokio::test]
async fn fargate_download_failure_exits_1_and_leaves_no_file() {
    let run = run(
        Some(OBJECT_CREATED),
        FakeStore::with(Behaviour::Fail("NoSuchKey: not found".to_string())),
    )
    .await;
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, vec!["Error: NoSuchKey: not found"]);
    assert!(run.stdout.is_empty());
    assert!(run.files.is_empty());
}
