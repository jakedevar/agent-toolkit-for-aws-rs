//! Shared fixtures of the scoring tests: temporary directories, JSON
//! builders, the in-process CLI runner and the real skill tree.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

use crate::run;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique temporary directory removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-scoring-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    /// A named child directory (created) of this one.
    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).expect("create fixture dir");
        path
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

pub(crate) fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture dir");
    fs::write(path, content).expect("write fixture");
}

pub(crate) fn path_text(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}

/// Parses fixture JSON.
pub(crate) fn j(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

/// Runs the CLI in-process; returns (status, stdout, stderr).
pub(crate) fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = run(&args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

/// The agent-advisor skill of the real plugin tree.
pub(crate) fn real_skill() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/aws-startup-advisor/skills/agent-advisor")
}

/// The runtime registry of the real skill.
pub(crate) fn real_runtimes() -> PathBuf {
    real_skill().join("references").join("runtimes")
}

/// A minimal valid runtime profile (`_minimal` of `test_scoring.py`).
pub(crate) fn minimal(id: &str, status: &str) -> Json {
    Json::object(vec![
        ("id", Json::string(id)),
        ("display_name", Json::string(id)),
        ("status", Json::string(status)),
        ("service_card", Json::string(format!("{id}.md"))),
        ("hard_constraints", Json::Array(Vec::new())),
        ("affinities", Json::Object(Vec::new())),
        ("deployment_models", Json::Array(Vec::new())),
        ("volatile_facts", Json::Array(Vec::new())),
    ])
}

/// `profile` with `key` replaced (or added) by the JSON `value`.
pub(crate) fn with(profile: Json, key: &str, value: &str) -> Json {
    let Json::Object(mut members) = profile else {
        panic!("a profile is an object");
    };
    let value = j(value);
    match members.iter_mut().find(|(name, _)| name == key) {
        Some(slot) => slot.1 = value,
        None => members.push((key.to_string(), value)),
    }
    Json::Object(members)
}

/// The text of a profile, for writing into a registry directory.
pub(crate) fn profile_text(profile: &Json) -> String {
    profile.to_compact_string()
}
