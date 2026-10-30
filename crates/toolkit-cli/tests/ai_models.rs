//! Real-process tests of `toolkit ai-models ...`: exit codes and the separation
//! of stdout and stderr. Fixtures are offline temporary files; the binary has no
//! live AWS client, so nothing can reach the network.

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-ai-models-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch");
        Self(path)
    }

    fn file(&self, name: &str, content: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, content).expect("write fixture");
        path.to_str().expect("utf-8 path").to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .stdin(Stdio::null())
        .output()
        .expect("spawn toolkit")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

#[test]
fn a_recorded_listing_prints_json_on_stdout_and_nothing_on_stderr() {
    let scratch = Scratch::new("list");
    let recorded = scratch.file(
        "recorded.json",
        r#"{"calls": {"ListHubContents": [{"response": {"HubContentSummaries": [
            {"HubContentName": "m1", "HubContentSearchKeywords": ["@task:chat"]}]}}]}}"#,
    );
    let output = run(
        &[
            "ai-models",
            "list-models",
            "my-hub",
            "--recorded",
            &recorded,
        ],
        &scratch.0,
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        text(&output.stdout),
        "[{\"name\": \"m1\", \"tasks\": [\"chat\"], \"bedrock_eligible\": false, \"customization_eligible\": false}]\n"
    );
    assert_eq!(text(&output.stderr), "");
}

#[test]
#[cfg(not(feature = "ai-models-live"))]
fn the_default_build_sends_nothing_and_exits_one() {
    let scratch = Scratch::new("nolive");
    for args in [
        vec!["ai-models", "list-models", "my-hub"],
        vec!["ai-models", "model-names", "my-hub"],
        vec!["ai-models", "recipes", "m1", "my-hub"],
        vec![
            "ai-models",
            "endpoint-diagnostics",
            "my-endpoint",
            "us-east-1",
        ],
    ] {
        let output = run(&args, &scratch.0);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(text(&output.stdout), "");
        let stderr = text(&output.stderr);
        assert!(stderr.contains("built without ai-models-live"), "{stderr}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
    }
}

#[test]
fn usage_mistakes_exit_two() {
    let scratch = Scratch::new("usage");
    for args in [
        vec!["ai-models"],
        vec!["ai-models", "list-models"],
        vec!["ai-models", "list-models", "my-hub", "--create-snapshot"],
        vec!["ai-models", "recipes", "only-a-model"],
        vec!["ai-models", "filter"],
        vec![
            "ai-models",
            "endpoint-diagnostics",
            "e",
            "us-east-1",
            "--minutes",
            "0",
        ],
    ] {
        let output = run(&args, &scratch.0);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&output.stderr)
        );
        assert_eq!(text(&output.stdout), "");
    }
}

#[test]
fn filter_works_offline_and_orders_newest_first() {
    let scratch = Scratch::new("filter");
    let models = scratch.file(
        "models.json",
        r#"[{"name": "old", "tasks": ["chat"], "original_creation_time": "2023-01-01T00:00:00+00:00"},
            {"name": "new", "tasks": ["chat"], "original_creation_time": "2024-01-01T00:00:00+00:00"},
            {"name": "other", "tasks": ["embedding"]}]"#,
    );
    let output = run(&["ai-models", "filter", &models, "task:chat"], &scratch.0);
    assert_eq!(output.status.code(), Some(0));
    let stdout = text(&output.stdout);
    assert!(
        stdout.starts_with(
            "{\"total_models\": 3, \"filters_applied\": {\"task\": [\"chat\"]}, \"matched\": 2, "
        ),
        "{stdout}"
    );
    assert!(stdout.find("\"new\"") < stdout.find("\"old\""), "{stdout}");
}
