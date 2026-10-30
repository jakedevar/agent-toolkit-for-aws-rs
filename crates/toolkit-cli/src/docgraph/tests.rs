#![allow(
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::float_cmp,
    clippy::needless_raw_string_hashes,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

//! Offline, deterministic tests of the DocumentDB and Neptune utilities
//! (#4395). Recordings are literals built here; temporary directories are
//! unique and removed on drop. Nothing opens a socket, reads a credential,
//! contacts AWS, connects to a database or creates an environment.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

use super::checks::{Findings, Status};
use super::client::RecordedClient;
use super::infra::run_infra_checks;

mod capture;
#[cfg(feature = "docgraph-live")]
mod live;
mod memory;
mod neptune;
mod redaction;
mod review;
mod template;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fake credential-shaped value that must never reach any output.
pub(super) const SECRET: &str = "FAKE-RECORDED-SECRET-4395-AKIAZZZZZZZZZZZZZZZZ";

pub(super) struct TempDir(PathBuf);

impl TempDir {
    pub(super) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-docgraph-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    /// Writes `contents` to `name` and returns its path as text.
    pub(super) fn write(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, contents).expect("write fixture");
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

pub(super) fn json(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

/// What one in-process `toolkit` run produced.
pub(super) struct Ran {
    pub(super) status: u8,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) fn run(words: &[&str]) -> Ran {
    let args: Vec<OsString> = words.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = crate::run(&args, &mut out, &mut err);
    Ran {
        status,
        stdout: String::from_utf8(out).expect("UTF-8 stdout"),
        stderr: String::from_utf8(err).expect("UTF-8 stderr"),
    }
}

/// The fixed instant of the review tests and the window it implies.
pub(super) const NOW: &str = "2026-01-31T12:00:00Z";
pub(super) const NOW_SECONDS: i64 = 1_769_860_800;

/// `id:status` of every finding, in order.
pub(super) fn summary(findings: &Findings) -> Vec<String> {
    findings
        .checks
        .iter()
        .map(|check| format!("{}:{}", check.id, check.status.as_str()))
        .collect()
}

pub(super) fn find<'f>(findings: &'f Findings, id: &str) -> Vec<&'f super::checks::Check> {
    findings.checks.iter().filter(|c| c.id == id).collect()
}

pub(super) fn review(recording: &str) -> Findings {
    let mut client = RecordedClient::from_document(&json(recording)).expect("recording");
    run_infra_checks(&mut client, "prod-docdb", NOW_SECONDS)
}

#[test]
fn the_commands_are_registered_strict_and_not_passthrough() {
    let mut seen = 0;
    for spec in crate::COMMANDS {
        if !["documentdb", "neptune"].contains(&spec.path[0]) {
            continue;
        }
        seen += 1;
        assert!(!spec.passthrough, "{:?}", spec.path);
        assert!(spec.repeatable_flags.is_empty(), "{:?}", spec.path);
    }
    assert_eq!(seen, 5);
    assert!(Status::Info.as_str() == "info");
}

/// The fake credential of review #4904's reproductions (#4907, #4908).
pub(super) const FAKE: &str = "FAKE_REVIEW_VALUE";

/// `https://review-user:FAKE_REVIEW_VALUE` + `pad` x characters +
/// `@example.invalid/` + `tail`: URL userinfo whose `@host` delimiter lies
/// `pad` characters past the password (200 puts it beyond every display
/// bound of this slice).
pub(super) fn userinfo_url(pad: usize, tail: &str) -> String {
    format!(
        "https://review-user:{FAKE}{}@example.invalid/{tail}",
        "x".repeat(pad)
    )
}

/// Neither the fake password nor the user name reached `text`.
pub(super) fn assert_no_userinfo(text: &str) {
    assert!(!text.contains(FAKE), "password leaked: {text:?}");
    assert!(!text.contains("review-user"), "user name leaked: {text:?}");
}
