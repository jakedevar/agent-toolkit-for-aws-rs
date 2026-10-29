//! `toolkit documentdb wa-review --analysis-data` report custody through the
//! real binary (#5344; R-DG-08). The permissive-umask case needs a process
//! umask of 000, which is process-global: it is set in a child shell so no
//! test process in this workspace ever changes its own umask.

#![cfg(unix)]

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");
const RESULTS_NAME: &str = "wa_review_results.json";
const REPORT_NAME: &str = "wa_review_report.md";
const ANALYSIS: &str =
    r#"{"app": {"orders": {"count": 250000, "size": 1000, "avgObjSize": 9000.5}}}"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-wa-review-private-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn analysis_data_reports_are_created_0600_under_a_permissive_umask() {
    let scratch = Scratch::new("umask");
    let recorded = scratch.0.join("recorded.json");
    let analysis = scratch.0.join("analysis.json");
    let out = scratch.0.join("out");
    fs::write(&recorded, r#"{"calls": []}"#).expect("recording");
    fs::write(&analysis, ANALYSIS).expect("analysis");
    let output = Command::new("/bin/sh")
        .args(["-c", "umask 000 && exec \"$0\" \"$@\"", BIN])
        .args([
            "documentdb",
            "wa-review",
            "--cluster-id",
            "prod-docdb",
            "--region",
            "us-east-1",
            "--recorded",
        ])
        .arg(&recorded)
        .arg("--analysis-data")
        .arg(&analysis)
        .arg("--output")
        .arg(&out)
        .args(["--now", "2026-01-01T00:00:00Z"])
        .env_clear()
        .env("HOME", &scratch.0)
        .current_dir(&scratch.0)
        .stdin(Stdio::null())
        .output()
        .expect("spawn sh");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in [RESULTS_NAME, REPORT_NAME] {
        let mode = fs::metadata(out.join(name))
            .expect("report file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o600, "{name} is {:o}", mode & 0o7777);
    }
}
