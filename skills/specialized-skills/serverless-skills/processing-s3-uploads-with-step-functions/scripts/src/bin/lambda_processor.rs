//! Lambda entry point for files up to 6 MB. Build it with cargo-lambda:
//! `cargo lambda build --release --bin lambda-processor` and deploy the
//! `bootstrap` it produces to a `provided.al2023` function.

use std::path::Path;
use std::sync::Arc;

use lambda_runtime::{service_fn, Error, LambdaEvent};
use s3_upload_processor::{handle_lambda_event, S3Store, StdoutOutput, DEFAULT_WORK_DIR};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let store = Arc::new(S3Store::from_env().await);
    lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
        let store = Arc::clone(&store);
        async move {
            handle_lambda_event(
                &event.payload,
                store.as_ref(),
                Path::new(DEFAULT_WORK_DIR),
                &StdoutOutput,
            )
            .await
            .map_err(Error::from)
        }
    }))
    .await
}
