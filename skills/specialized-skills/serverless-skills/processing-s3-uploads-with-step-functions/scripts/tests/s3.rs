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

// ---- atomic publication of the download (#5073) ----
//
// The former boto3 `download_file` wrote a temporary sibling and renamed it:
// a failed download kept a previous file, and a symlink at the destination was
// replaced, never written through.

use std::fs;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame};

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "s3-upload-processor-atomic-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Names of everything in `dir`, sorted.
fn entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

const NO_SUCH_KEY: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchKey</Code>\
                           <Message>The specified key does not exist.</Message><Key>k</Key></Error>";

/// A body that yields `chunk` and then fails, like a dropped connection.
struct DropsAfterOneChunk {
    chunk: Option<Bytes>,
}

impl Body for DropsAfterOneChunk {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        Poll::Ready(Some(match self.chunk.take() {
            Some(chunk) => Ok(Frame::data(chunk)),
            None => Err(std::io::Error::other("connection reset")),
        }))
    }
}

fn dropped_stream(partial: &'static str) -> ReplayEvent {
    ReplayEvent::new(
        http::Request::builder()
            .uri("https://example-uploads.s3.us-east-1.amazonaws.com/")
            .body(SdkBody::empty())
            .unwrap(),
        http::Response::builder()
            .status(200)
            .body(SdkBody::from_body_1_x(DropsAfterOneChunk {
                chunk: Some(Bytes::from_static(partial.as_bytes())),
            }))
            .unwrap(),
    )
}

#[tokio::test]
async fn a_failed_get_keeps_an_existing_destination() {
    let dir = work_dir("get-404");
    let destination = dir.join("report.csv");
    fs::write(&destination, "previous download").unwrap();
    let (store, _) = store(vec![event(404, NO_SUCH_KEY)]);

    let error = store
        .download("example-uploads", "report.csv", &destination)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("NoSuchKey"), "{error}");
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "previous download"
    );
    assert_eq!(entries(&dir), ["report.csv"]);
}

#[tokio::test]
async fn a_stream_error_after_partial_content_keeps_the_previous_file_and_leaves_nothing() {
    let dir = work_dir("partial-existing");
    let destination = dir.join("report.csv");
    fs::write(&destination, "previous download").unwrap();
    let (store, _) = store(vec![dropped_stream("half of the new con")]);

    let error = store
        .download("example-uploads", "report.csv", &destination)
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Failed to read s3://example-uploads/report.csv"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "previous download"
    );
    assert_eq!(
        entries(&dir),
        ["report.csv"],
        "no temporary file may remain"
    );
}

#[tokio::test]
async fn a_stream_error_without_a_previous_file_publishes_nothing() {
    let dir = work_dir("partial-fresh");
    let destination = dir.join("report.csv");
    let (store, _) = store(vec![dropped_stream("half")]);

    store
        .download("example-uploads", "report.csv", &destination)
        .await
        .unwrap_err();

    assert!(
        !destination.exists(),
        "a partial object must not be published"
    );
    assert!(entries(&dir).is_empty(), "{:?}", entries(&dir));
}

#[tokio::test]
async fn a_successful_download_replaces_an_existing_file() {
    let dir = work_dir("replace");
    let destination = dir.join("report.csv");
    fs::write(&destination, "previous download that is longer").unwrap();
    let (store, _) = store(vec![event(200, "new")]);

    store
        .download("example-uploads", "report.csv", &destination)
        .await
        .unwrap();

    assert_eq!(fs::read_to_string(&destination).unwrap(), "new");
    assert_eq!(
        entries(&dir),
        ["report.csv"],
        "no temporary file may remain"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_at_the_destination_is_replaced_not_written_through() {
    let dir = work_dir("symlink-ok");
    let precious = dir.join("precious.txt");
    fs::write(&precious, "do not touch").unwrap();
    let destination = dir.join("report.csv");
    std::os::unix::fs::symlink(&precious, &destination).unwrap();
    let (store, _) = store(vec![event(200, "downloaded")]);

    store
        .download("example-uploads", "report.csv", &destination)
        .await
        .unwrap();

    assert_eq!(fs::read_to_string(&precious).unwrap(), "do not touch");
    let meta = fs::symlink_metadata(&destination).unwrap();
    assert!(meta.file_type().is_file(), "the link itself is replaced");
    assert_eq!(fs::read_to_string(&destination).unwrap(), "downloaded");
    assert_eq!(entries(&dir), ["precious.txt", "report.csv"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_download_leaves_a_destination_symlink_and_its_target_alone() {
    for failure in ["get", "stream"] {
        let dir = work_dir(&format!("symlink-fail-{failure}"));
        let precious = dir.join("precious.txt");
        fs::write(&precious, "do not touch").unwrap();
        let destination = dir.join("report.csv");
        std::os::unix::fs::symlink(&precious, &destination).unwrap();
        let events = if failure == "get" {
            vec![event(404, NO_SUCH_KEY)]
        } else {
            vec![dropped_stream("partial")]
        };
        let (store, _) = store(events);

        store
            .download("example-uploads", "report.csv", &destination)
            .await
            .unwrap_err();

        assert_eq!(
            fs::read_to_string(&precious).unwrap(),
            "do not touch",
            "{failure}"
        );
        assert!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{failure}: the link must stay"
        );
        assert_eq!(entries(&dir), ["precious.txt", "report.csv"], "{failure}");
    }
}

#[tokio::test]
async fn a_destination_that_cannot_be_replaced_is_an_error_and_leaves_no_temporary_file() {
    let dir = work_dir("directory");
    let destination = dir.join("report.csv");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("inner"), "x").unwrap();
    let (store, _) = store(vec![event(200, "data")]);

    let error = store
        .download("example-uploads", "report.csv", &destination)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("Failed to write"), "{error}");
    assert!(destination.join("inner").exists());
    assert_eq!(entries(&dir), ["report.csv"]);
}

#[tokio::test]
async fn concurrent_downloads_to_different_files_do_not_collide() {
    let dir = work_dir("concurrent");
    let (store, _) = store(vec![event(200, "one"), event(200, "two")]);
    let first = dir.join("a.csv");
    let second = dir.join("b.csv");
    let (a, b) = tokio::join!(
        store.download("example-uploads", "a.csv", &first),
        store.download("example-uploads", "b.csv", &second),
    );
    a.unwrap();
    b.unwrap();
    let mut contents = [
        fs::read_to_string(&first).unwrap(),
        fs::read_to_string(&second).unwrap(),
    ];
    contents.sort();
    assert_eq!(contents, ["one", "two"]);
    assert_eq!(entries(&dir), ["a.csv", "b.csv"]);
}
