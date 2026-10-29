//! Fargate task entry point for files above 6 MB. Step Functions passes the
//! `EventBridge` event in the `TASK_EVENT` environment variable.

use std::path::Path;
use std::process::ExitCode;

use s3_upload_processor::{run_fargate_task, Output, S3Store, StdoutOutput, DEFAULT_WORK_DIR};

struct StderrOutput;

impl Output for StderrOutput {
    fn line(&self, text: &str) {
        eprintln!("{text}");
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let task_event = std::env::var("TASK_EVENT").ok();
    let store = S3Store::from_env().await;
    let outcome = run_fargate_task(
        task_event.as_deref(),
        &store,
        Path::new(DEFAULT_WORK_DIR),
        &StdoutOutput,
        &StderrOutput,
    )
    .await;
    ExitCode::from(u8::try_from(outcome.exit_code).unwrap_or(1))
}
