#![allow(
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]
//! The S3 adapter against recorded responses: the request it sends and what it
//! does with the object, a missing object and a broken stream.

use std::path::PathBuf;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::{Client, Config};
use aws_smithy_http_client::test_util::{ReplayEvent, StaticReplayClient};
use aws_smithy_types::body::SdkBody;
use s3_upload_processor::{ObjectStore, S3Store};

fn event(status: u16, body: &str) -> ReplayEvent {
    ReplayEvent::new(
        http::Request::builder()
            .uri("https://example-uploads.s3.us-east-1.amazonaws.com/")
            .body(SdkBody::empty())
            .unwrap(),
        http::Response::builder()
            .status(status)
            .header("content-length", body.len().to_string())
            .body(SdkBody::from(body.to_string()))
            .unwrap(),
    )
}

fn store(events: Vec<ReplayEvent>) -> (S3Store, StaticReplayClient) {
    let replay = StaticReplayClient::new(events);
    let config = Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "fixture", "fixture", None, None, "fixture",
        ))
        .http_client(replay.clone())
        .build();
    (S3Store::new(Client::from_conf(config)), replay)
}

fn target(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("s3-upload-processor-s3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[tokio::test]
async fn the_object_is_streamed_to_the_destination() {
    let (store, replay) = store(vec![event(200, "col\n1\n")]);
    let destination = target("streamed.csv");
    store
        .download(
            "example-uploads",
            "uploads/2024/my report.csv",
            &destination,
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "col\n1\n");

    let request = replay.actual_requests().next().unwrap();
    assert_eq!(request.method(), "GET");
    let uri = request.uri();
    assert!(
        uri.starts_with("https://example-uploads.s3.us-east-1.amazonaws.com/"),
        "{uri}"
    );
    // The key is sent percent-encoded, with its slashes kept.
    assert!(uri.contains("/uploads/2024/my%20report.csv"), "{uri}");
    assert!(
        request.headers().get("authorization").is_some(),
        "request must be signed"
    );
    let _ = std::fs::remove_file(&destination);
}

#[tokio::test]
async fn a_missing_object_is_an_error_and_creates_no_file() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchKey</Code>\
               <Message>The specified key does not exist.</Message><Key>gone.csv</Key></Error>";
    let (store, _) = store(vec![event(404, xml)]);
    let destination = target("gone.csv");
    let error = store
        .download("example-uploads", "gone.csv", &destination)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("s3://example-uploads/gone.csv"),
        "{message}"
    );
    assert!(message.contains("NoSuchKey"), "{message}");
    assert!(!destination.exists());
}

#[tokio::test]
async fn an_unwritable_destination_is_an_error() {
    let (store, _) = store(vec![event(200, "data")]);
    let destination = target("missing-dir").join("nested").join("file.csv");
    let error = store
        .download("example-uploads", "file.csv", &destination)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Failed to write"), "{error}");
}
