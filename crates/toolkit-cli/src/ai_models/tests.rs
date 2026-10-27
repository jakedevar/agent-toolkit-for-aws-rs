//! Tests of the model tools. Every fixture is offline and deterministic; the
//! only client is a recorded or scripted one, so nothing reaches AWS.

use toolkit_aws::AwsError;
use toolkit_core::{parse_json, Json};

use super::client::{Operation, SageMakerClient};
use super::recorded::RecordedClient;
use crate::dynamodb::pyfmt;

mod cli;
mod diagnostics;
mod filter;
mod hub;
mod normalize;
mod recorded;
mod search;
mod shape;
mod snapshot;

/// Parses fixture JSON.
pub(super) fn j(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

/// `json.dumps(value)` of a value.
pub(super) fn dumps(value: &Json) -> String {
    pyfmt::dumps(value, false)
}

/// A `ListHubContents` summary with the given keywords.
pub(super) fn summary(name: &str, keywords: &[&str], created: Option<&str>) -> Json {
    let mut members = vec![
        ("HubContentName", Json::string(name)),
        (
            "HubContentSearchKeywords",
            Json::Array(keywords.iter().map(|k| Json::string(*k)).collect()),
        ),
    ];
    if let Some(created) = created {
        members.push(("OriginalCreationTime", Json::string(created)));
    }
    Json::object(members)
}

/// A recorded client from a `calls` document.
pub(super) fn recorded(text: &str) -> RecordedClient {
    RecordedClient::from_json(&j(text)).expect("valid recorded fixture")
}

/// A client that answers from a script and keeps every request it saw.
pub(super) struct Spy {
    pub(super) seen: Vec<(Operation, Json)>,
    answers: std::collections::VecDeque<Result<Json, AwsError>>,
    pub(super) search_support: Result<(), String>,
}

impl Spy {
    pub(super) fn new(answers: Vec<Result<Json, AwsError>>) -> Self {
        Self {
            seen: Vec::new(),
            answers: answers.into(),
            search_support: Ok(()),
        }
    }
}

impl SageMakerClient for Spy {
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, AwsError> {
        self.seen.push((operation, request.clone()));
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(AwsError::invalid_response("spy ran out of answers")))
    }

    fn check_search_support(&self) -> Result<(), String> {
        self.search_support.clone()
    }
}

/// The catalog the search tests resolve constraints against (normalized).
pub(super) fn catalog() -> Vec<Json> {
    let records = [
        summary(
            "llama-3-8b",
            &[
                "@task:text generation",
                "@task:generation-text",
                "@model-size:8B",
                "@license:Apache 2.0",
                "@language:en",
                "@language:de",
                "@provider:meta",
                "@model-type:LLM",
                "@context-window:8k",
                "@data-type:text",
                "@input-modality:text",
                "@output-modality:text",
                "@capability:bedrock_console",
                "@capability:customization",
            ],
            Some("2024-05-01T00:00:00+00:00"),
        ),
        summary(
            "Mistral-7B",
            &[
                "@task:text generation",
                "@model-size:7b",
                "@license:Apache-2.0",
                "@language:en",
                "@provider:mistral",
                "@model-type:LLM",
                "@context-window:32k",
            ],
            Some("2024-06-01T00:00:00+00:00"),
        ),
        summary(
            "big-model-70b",
            &[
                "@task:text generation",
                "@model-size:10b-70b",
                "@license:MIT",
                "@provider:acme",
                "@model-type:LLM",
            ],
            None,
        ),
        summary(
            "tiny-embedder",
            &[
                "@task:embedding",
                "@model-size:<1b",
                "@license:MIT",
                "@provider:acme",
                "@model-type:EMBEDDING",
                "@input-modality:text",
            ],
            Some("2023-01-01T00:00:00+00:00"),
        ),
    ];
    records
        .iter()
        .map(|record| super::normalize::normalize_hub_content(record).expect("normalizes"))
        .collect()
}

/// A unique temporary directory, removed when dropped.
pub(super) struct TempDir(std::path::PathBuf);

impl TempDir {
    pub(super) fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a fixture file below `root`.
pub(super) fn write(root: &std::path::Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    std::fs::write(path, content).expect("write fixture");
}

/// A temp path as text.
pub(super) fn path_text(path: &std::path::Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}

/// Runs the router in process: exit status, stdout, stderr.
pub(super) fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<std::ffi::OsString> = values.iter().map(std::ffi::OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = crate::run(&args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}
