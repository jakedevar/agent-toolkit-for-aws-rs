//! Test-only helpers shared by the DynamoDB unit tests.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique temporary directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let unique = format!(
            "toolkit-dynamodb-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
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

/// Parses a JSON fixture.
pub fn json(text: &str) -> Json {
    parse_json(text).expect("valid fixture json")
}

/// Asserts two floats agree to `tolerance`.
pub fn assert_close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected} (±{tolerance}), got {actual}"
    );
}
