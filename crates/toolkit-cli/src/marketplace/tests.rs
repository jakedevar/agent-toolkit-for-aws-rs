#![allow(
    clippy::needless_raw_string_hashes,
    clippy::too_many_lines,
    clippy::similar_names
)]

//! Offline, deterministic tests of the marketplace slice (#4396). Recordings
//! are literals built here; nothing opens a socket, reads a credential or
//! contacts AWS.

use std::ffi::OsString;

use toolkit_core::{parse_json, Json};

use super::base::{Config, Ctx, Failure, Logger};
use super::client::RecordedClient;
use super::clock::at;

mod commands;
mod core_logic;
mod events_web;
mod pipeline_flow;
mod query_flow;
mod submitter_flow;
mod subscribers_flow;

/// A credential-shaped value that must never reach any output.
pub(super) const SECRET: &str = "FAKE-RECORDED-SECRET-4396-AKIAZZZZZZZZZZZZZZZZ";

pub(super) const NOW: &str = "2026-01-31T12:30:00Z";
pub(super) const ACCOUNT: &str = "111122223333";
pub(super) const LICENSE: &str = "arn:aws:license-manager:us-east-1:111122223333:license:l-abc123";

pub(super) fn j(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

/// The environment every pipeline handler needs.
pub(super) const CFG: [(&str, &str); 9] = [
    ("SUBSCRIBERS_TABLE", "subs"),
    ("PRODUCT_CODE", "prod123"),
    ("USAGE_TABLE", "usage"),
    ("AGGREGATED_USAGE_TABLE", "agg"),
    ("WORK_QUEUE_URL", "https://sqs.test/work"),
    ("DEPROVISION_WORK_QUEUE_URL", "https://sqs.test/depwork"),
    ("CLEANUP_QUEUE_URL", "https://sqs.test/cleanup"),
    ("CUSTOMER_PROFILE_TABLE", "profiles"),
    ("AWS_REGION", "eu-west-1"),
];

/// A recorded step that answers.
pub(super) fn step(
    service: &str,
    operation: &str,
    request: Option<&str>,
    response: &str,
) -> String {
    let request = request.map_or_else(String::new, |r| format!(r#""request": {r},"#));
    format!(
        r#"{{"service": "{service}", "operation": "{operation}", {request} "response": {response}}}"#
    )
}

/// A recorded step that fails with a service error code.
pub(super) fn fail(service: &str, operation: &str, code: &str) -> String {
    format!(
        r#"{{"service": "{service}", "operation": "{operation}", "error": {{"code": "{code}", "message": "{SECRET}"}}}}"#
    )
}

pub(super) fn query(response: &str) -> String {
    step("dynamodb", "Query", None, response)
}

pub(super) fn update() -> String {
    step("dynamodb", "UpdateItem", None, "{}")
}

pub(super) struct Ran {
    pub(super) result: Result<Json, Failure>,
    pub(super) log: Vec<String>,
    pub(super) sent: Vec<(String, Json)>,
    pub(super) unused: usize,
}

impl Ran {
    pub(super) fn ok(&self) -> &Json {
        self.result.as_ref().expect("the handler succeeds")
    }

    pub(super) fn err(&self) -> String {
        self.result
            .as_ref()
            .expect_err("the handler fails")
            .to_string()
    }

    /// The requests sent to `service:Operation`, in order.
    pub(super) fn requests(&self, key: &str) -> Vec<&Json> {
        self.sent
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, r)| r)
            .collect()
    }

    pub(super) fn logged(&self, needle: &str) -> bool {
        self.log.iter().any(|line| line.contains(needle))
    }
}

pub(super) fn run_with(
    name: &str,
    config: &[(&str, &str)],
    event: &str,
    calls: &[String],
    now: &str,
) -> Ran {
    let document = parse_json(&format!(r#"{{"calls": [{}]}}"#, calls.join(",")))
        .expect("recording JSON");
    let mut client = RecordedClient::from_document(&document).expect("recording");
    let config = Config::from_pairs(config);
    let mut log = Logger::default();
    let result = {
        let mut ctx = Ctx {
            client: &mut client,
            config: &config,
            log: &mut log,
            now: at(now),
        };
        super::dispatch(name, &mut ctx, &j(event), 7)
    };
    Ran {
        result,
        log: log.lines().to_vec(),
        sent: client.sent().to_vec(),
        unused: client.unconsumed(),
    }
}

pub(super) fn run(name: &str, event: &str, calls: &[String]) -> Ran {
    run_with(name, &CFG, event, calls, NOW)
}

/// What one in-process `toolkit` run produced.
pub(super) struct Cli {
    pub(super) status: u8,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) fn toolkit(words: &[&str]) -> Cli {
    let args: Vec<OsString> = words.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = crate::run(&args, &mut out, &mut err);
    Cli {
        status,
        stdout: String::from_utf8(out).expect("UTF-8 stdout"),
        stderr: String::from_utf8(err).expect("UTF-8 stderr"),
    }
}

/// A unique temporary file holding `contents`.
pub(super) struct TempFile(std::path::PathBuf);

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl TempFile {
    pub(super) fn new(label: &str, contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-marketplace-{label}-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, contents).expect("write fixture");
        Self(path)
    }

    pub(super) fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
