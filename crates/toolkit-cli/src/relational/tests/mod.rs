#![allow(
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::float_cmp,
    clippy::needless_raw_string_hashes,
    clippy::unreadable_literal
)]

//! Offline, deterministic tests of the relational helpers (#4394).
//!
//! Recorded sessions are literals; temporary directories are unique and
//! removed on drop. Nothing opens a socket, reads a credential or contacts
//! AWS, and no test needs a database.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

mod commands;
mod oracle;
mod rds;
mod sql;
mod tenant;
mod timestream;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(super) struct TempDir(PathBuf);

impl TempDir {
    pub(super) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-relational-{label}-{}-{}",
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

/// No control character other than the line separator reaches the text.
pub(super) fn assert_single_line_safe(text: &str) {
    for c in text.chars() {
        assert!(
            c == '\n' || !toolkit_core::is_unsafe_char(c),
            "unsafe U+{:04X} in {text:?}",
            u32::from(c)
        );
    }
}
