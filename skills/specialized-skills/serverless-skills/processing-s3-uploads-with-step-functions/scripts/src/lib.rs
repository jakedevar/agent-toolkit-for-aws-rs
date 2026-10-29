//! File processors for the S3 upload workflow: a Lambda function for small
//! files and a Fargate task for large ones.
//!
//! This is the Rust counterpart of the former `lambda_function.py` and
//! `fargate_processor.py`. Both receive the EventBridge "Object Created" event
//! of S3 (`detail.bucket.name`, `detail.object.key`, `detail.object.size`),
//! validate it, download the object to a local directory and report the file
//! metadata. The S3 access sits behind [`ObjectStore`] so the tests run
//! without AWS; [`S3Store`] is the real one.

#![allow(
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::must_use_candidate
)]

use std::fmt;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_sdk_s3::error::DisplayErrorContext;
use aws_sdk_s3::Client;
use serde_json::{Map, Value};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;

/// S3 object keys are at most 1024 characters long here (the Python check
/// counted characters, and so does this one).
pub const MAX_KEY_LENGTH: usize = 1024;

/// The Lambda function refuses objects above 512 MiB (its `/tmp` limit).
pub const MAX_LAMBDA_FILE_BYTES: f64 = 512.0 * 1024.0 * 1024.0;

/// Where downloads go in Lambda and in the container.
pub const DEFAULT_WORK_DIR: &str = "/tmp";

/// Output of the processors: one line per call (stdout in both runtimes).
pub trait Output: Send + Sync {
    fn line(&self, text: &str);
}

/// Writes lines to standard output, which CloudWatch Logs captures.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdoutOutput;

impl Output for StdoutOutput {
    fn line(&self, text: &str) {
        println!("{text}");
    }
}

/// Source of S3 objects.
pub trait ObjectStore: Send + Sync {
    /// Download `s3://bucket/key` to `destination`. The destination is replaced
    /// only by a complete download: on failure it is left as it was (nothing
    /// is deleted) and no partial file remains.
    fn download(
        &self,
        bucket: &str,
        key: &str,
        destination: &Path,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// A failed download.
#[derive(Debug)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// Why the Lambda processor refused or failed an event. The messages of the
/// first three are those of the Python `ValueError`s.
#[derive(Debug)]
pub enum ProcessorError {
    /// Missing bucket or key, a non-string key, or a `/..` in the key.
    InvalidPath,
    KeyTooLong,
    FileTooLarge,
    /// The event does not have the EventBridge shape (a field that must be an
    /// object or a number is something else).
    Malformed(String),
    Download(StoreError),
    /// Local file handling failed.
    Io(std::io::Error),
}

impl fmt::Display for ProcessorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => write!(f, "Invalid S3 path"),
            Self::KeyTooLong => write!(f, "S3 key exceeds maximum length"),
            Self::FileTooLarge => write!(f, "File too large for Lambda processing"),
            Self::Malformed(what) => write!(f, "Malformed event: {what}"),
            Self::Download(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProcessorError {}

/// The values taken from the event, before validation.
#[derive(Debug, Clone, PartialEq)]
pub struct EventFields {
    pub bucket: Value,
    pub key: Value,
    /// `detail.object.size`; `0` when absent.
    pub size: Value,
}

/// A bucket and key that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub bucket: String,
    pub key: String,
}

fn object_field<'a>(
    parent: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<Option<&'a Map<String, Value>>, ProcessorError> {
    match parent.get(name) {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map)),
        Some(_) => Err(ProcessorError::Malformed(format!(
            "{path} is not an object"
        ))),
    }
}

/// Read `detail.bucket.name`, `detail.object.key` and `detail.object.size`.
/// Absent objects read as empty (Python `.get(name, {})`); a value that is
/// present but not an object is a malformed event.
pub fn event_fields(event: &Value) -> Result<EventFields, ProcessorError> {
    let root = event
        .as_object()
        .ok_or_else(|| ProcessorError::Malformed("the event is not an object".to_string()))?;
    let detail = object_field(root, "detail", "detail")?;
    let bucket = detail
        .map(|d| object_field(d, "bucket", "detail.bucket"))
        .transpose()?
        .flatten();
    let object = detail
        .map(|d| object_field(d, "object", "detail.object"))
        .transpose()?
        .flatten();
    let field = |map: Option<&Map<String, Value>>, name: &str| {
        map.and_then(|m| m.get(name))
            .cloned()
            .unwrap_or(Value::Null)
    };
    Ok(EventFields {
        bucket: field(bucket, "name"),
        key: field(object, "key"),
        size: object
            .and_then(|o| o.get("size"))
            .cloned()
            .unwrap_or_else(|| Value::from(0)),
    })
}

/// Python truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The part of the key after the last `/` (Python `os.path.basename`).
pub fn basename(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

/// A key whose basename cannot name a file (`""`, `.`, `..`): the Python
/// script failed inside `download_file` after fetching the object.
fn names_a_file(key: &str) -> bool {
    !matches!(basename(key), "" | "." | "..")
}

/// Validation shared by both processors: bucket and key present, key a
/// string without `/..` and with at most [`MAX_KEY_LENGTH`] characters.
fn validate_target(fields: &EventFields) -> Result<Target, ProcessorError> {
    let bucket = match &fields.bucket {
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => return Err(ProcessorError::InvalidPath),
    };
    // Python: `not key or not isinstance(key, str) or '/..' in key`.
    let key = match &fields.key {
        Value::String(s) if !s.is_empty() && !s.contains("/..") => s.clone(),
        _ => return Err(ProcessorError::InvalidPath),
    };
    if key.chars().count() > MAX_KEY_LENGTH {
        return Err(ProcessorError::KeyTooLong);
    }
    Ok(Target { bucket, key })
}

/// `size > 512 MiB` as Python compares it: numbers (and booleans, which are
/// integers there) compare, anything else is a `TypeError`.
fn exceeds_limit(size: &Value) -> Result<bool, ProcessorError> {
    match size {
        Value::Number(n) => Ok(n.as_f64().is_some_and(|f| f > MAX_LAMBDA_FILE_BYTES)),
        Value::Bool(_) => Ok(false),
        _ => Err(ProcessorError::Malformed(
            "detail.object.size is not a number".to_string(),
        )),
    }
}

/// What the Lambda function returns: `{"statusCode": 200, "body": metadata}`.
/// `body` is an object, not JSON text.
pub fn lambda_response(
    target: &Target,
    size: &Value,
    local_path: &Path,
    file_size_on_disk: u64,
) -> Value {
    let mut metadata = Map::new();
    metadata.insert("bucket".to_string(), Value::from(target.bucket.as_str()));
    metadata.insert("key".to_string(), Value::from(target.key.as_str()));
    metadata.insert("size_bytes".to_string(), size.clone());
    metadata.insert(
        "local_path".to_string(),
        Value::from(local_path.to_string_lossy().as_ref()),
    );
    metadata.insert(
        "file_size_on_disk".to_string(),
        Value::from(file_size_on_disk),
    );
    let mut response = Map::new();
    response.insert("statusCode".to_string(), Value::from(200));
    response.insert("body".to_string(), Value::Object(metadata));
    Value::Object(response)
}

fn log_event_line(prefix: &str, value: &Value) -> String {
    format!("{prefix}: {value}")
}

/// Handle one Lambda invocation: validate, download to `work_dir`, log the
/// metadata and delete the file again (also on failure).
pub async fn handle_lambda_event<S: ObjectStore>(
    event: &Value,
    store: &S,
    work_dir: &Path,
    out: &dyn Output,
) -> Result<Value, ProcessorError> {
    out.line(&log_event_line("Received event", event));

    let fields = event_fields(event)?;
    let target = validate_target(&fields)?;
    if exceeds_limit(&fields.size)? {
        return Err(ProcessorError::FileTooLarge);
    }
    if !names_a_file(&target.key) {
        return Err(ProcessorError::InvalidPath);
    }

    let local_path = work_dir.join(basename(&target.key));
    let result = download_and_describe(&target, &fields.size, &local_path, store, out).await;
    // Best effort, as the Python `finally: os.remove` that ignores OSError.
    let _ = tokio::fs::remove_file(&local_path).await;
    result
}

async fn download_and_describe<S: ObjectStore>(
    target: &Target,
    size: &Value,
    local_path: &Path,
    store: &S,
    out: &dyn Output,
) -> Result<Value, ProcessorError> {
    store
        .download(&target.bucket, &target.key, local_path)
        .await
        .map_err(ProcessorError::Download)?;
    out.line(&format!(
        "Downloaded s3://{}/{} to {}",
        target.bucket,
        target.key,
        local_path.display()
    ));
    let on_disk = tokio::fs::metadata(local_path)
        .await
        .map_err(ProcessorError::Io)?
        .len();
    let response = lambda_response(target, size, local_path, on_disk);
    out.line(&log_event_line("File metadata", &response["body"]));
    Ok(response)
}

/// How the Fargate task ended. `exit_code` is the process exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    pub exit_code: i32,
}

/// Run the Fargate task. `task_event` is the `TASK_EVENT` environment
/// variable (`None` reads as `{}`). Messages that the Python script printed
/// stay on standard output; failures it reported as a traceback go to
/// `errors`. Exit status 0 on success, 1 on any failure.
pub async fn run_fargate_task<S: ObjectStore>(
    task_event: Option<&str>,
    store: &S,
    work_dir: &Path,
    out: &dyn Output,
    errors: &dyn Output,
) -> TaskOutcome {
    let failure = TaskOutcome { exit_code: 1 };
    let event: Value = match serde_json::from_str(task_event.unwrap_or("{}")) {
        Ok(event) => event,
        Err(e) => {
            errors.line(&format!("Error: TASK_EVENT is not valid JSON: {e}"));
            return failure;
        }
    };
    let fields = match event_fields(&event) {
        Ok(fields) => fields,
        Err(e) => {
            errors.line(&format!("Error: {e}"));
            return failure;
        }
    };

    if !truthy(&fields.bucket) || !truthy(&fields.key) {
        out.line("Error: No bucket or key in event");
        return failure;
    }
    let Value::String(bucket) = &fields.bucket else {
        errors.line("Error: the bucket name must be a string");
        return failure;
    };
    let bucket = bucket.clone();
    let key = match &fields.key {
        Value::String(s) if !s.contains("/..") && s.chars().count() <= MAX_KEY_LENGTH => s.clone(),
        _ => {
            out.line("Error: Invalid S3 key");
            return failure;
        }
    };
    if !names_a_file(&key) {
        errors.line("Error: the S3 key does not name a file");
        return failure;
    }

    let local_path = work_dir.join(basename(&key));
    match process_file(&bucket, &key, &local_path, store, out).await {
        Ok(size) => {
            out.line(&format!("Done. File size: {size} bytes"));
            TaskOutcome { exit_code: 0 }
        }
        Err(e) => {
            errors.line(&format!("Error: {e}"));
            failure
        }
    }
}

async fn process_file<S: ObjectStore>(
    bucket: &str,
    key: &str,
    local_path: &Path,
    store: &S,
    out: &dyn Output,
) -> Result<u64, ProcessorError> {
    store
        .download(bucket, key, local_path)
        .await
        .map_err(ProcessorError::Download)?;
    out.line(&format!(
        "Downloaded s3://{bucket}/{key} to {}",
        local_path.display()
    ));
    let size = tokio::fs::metadata(local_path)
        .await
        .map_err(ProcessorError::Io)?
        .len();
    let mut metadata = Map::new();
    metadata.insert("bucket".to_string(), Value::from(bucket));
    metadata.insert("key".to_string(), Value::from(key));
    metadata.insert(
        "local_path".to_string(),
        Value::from(local_path.to_string_lossy().as_ref()),
    );
    metadata.insert("file_size_bytes".to_string(), Value::from(size));
    out.line(&log_event_line("File metadata", &Value::Object(metadata)));
    Ok(size)
}

/// [`ObjectStore`] over the AWS SDK for Rust.
#[derive(Clone)]
pub struct S3Store {
    client: Client,
}

impl S3Store {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// A store from the default credential and region chain.
    pub async fn from_env() -> Self {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Self::new(Client::new(&config))
    }
}

impl ObjectStore for S3Store {
    async fn download(
        &self,
        bucket: &str,
        key: &str,
        destination: &Path,
    ) -> Result<(), StoreError> {
        let mut object = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                StoreError(format!(
                    "Failed to download s3://{bucket}/{key}: {}",
                    DisplayErrorContext(&e)
                ))
            })?;
        let io = |e: std::io::Error| {
            StoreError(format!("Failed to write {}: {e}", destination.display()))
        };

        // Publish like boto3's `download_file` (s3transfer): stream into a new
        // temporary sibling and rename it over the destination once the whole
        // object arrived. A failure at any point leaves the destination as it
        // was (a previous download, a symlink and its target) and removes only
        // the temporary file.
        let (temporary, mut file) = create_temporary_sibling(destination).await.map_err(io)?;
        let streamed = async {
            while let Some(chunk) = object
                .body
                .try_next()
                .await
                .map_err(|e| StoreError(format!("Failed to read s3://{bucket}/{key}: {e}")))?
            {
                file.write_all(&chunk).await.map_err(io)?;
            }
            file.flush().await.map_err(io)
        }
        .await;
        drop(file);
        let published = match streamed {
            // `rename` replaces a symlink at the destination; it never
            // follows it.
            Ok(()) => tokio::fs::rename(&temporary, destination).await.map_err(io),
            Err(e) => Err(e),
        };
        if published.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        published
    }
}

static TEMPORARY_FILES: AtomicU64 = AtomicU64::new(0);

/// Create a new, empty file next to `destination`. `create_new` is
/// `O_CREAT | O_EXCL`: it fails when anything exists at the path, a symlink
/// included (it is never followed), so the file is always ours.
async fn create_temporary_sibling(destination: &Path) -> std::io::Result<(PathBuf, File)> {
    let directory = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    for _ in 0..16 {
        let unique = TEMPORARY_FILES.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        // A fixed-length name: the object's own name may already be at the
        // file system's limit.
        let path = directory.join(format!(
            ".s3-download-{}-{unique}-{nanos}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "no unused temporary file name",
    ))
}
