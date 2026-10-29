//! The L6 routes' own pieces: the `GetLaunch include=download_url` lookup
//! over `FakeStreamingTransport`, the launch id check and the apply output.

use std::path::Path;
use std::time::Duration;

use toolkit_aws::retry::{NoJitter, RetryPolicy};
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};
use toolkit_aws::transport::Method;
use toolkit_aws::Secret;

use super::*;
use crate::launch::http::SystemClock;

const BASE: &str = "https://launch-with-aws.us-east-1.api.aws";

fn lookup(transport: &FakeStreamingTransport, id: &str) -> Result<Option<String>, LaunchError> {
    let sleeper = RecordingSleeper::new();
    get_download_url(
        transport,
        BASE,
        id,
        &Secret::new("FAKE-ACCESS".to_string()),
        &Runner {
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::default(),
        },
    )
}

fn json(status: u16, body: &str) -> FakeStreamingTransport {
    FakeStreamingTransport::new().respond(
        status,
        &[("content-type", "application/json")],
        vec![FakeChunk::data(body.as_bytes())],
    )
}

#[test]
fn get_launch_sends_the_encoded_id_the_include_member_and_the_bearer_token() {
    let transport = json(
        200,
        r#"{"id":"l/1","downloadUrl":"https://b.s3.us-east-1.amazonaws.com/s.zip?X-Amz-Signature=x"}"#,
    );
    let url = lookup(&transport, "l/1 x").expect("found");
    assert_eq!(
        url.as_deref(),
        Some("https://b.s3.us-east-1.amazonaws.com/s.zip?X-Amz-Signature=x")
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::Get);
    assert_eq!(
        request.url,
        format!("{BASE}/api/launches/l%2F1%20x?include=download_url")
    );
    assert_eq!(
        request.headers.get("authorization"),
        Some("Bearer FAKE-ACCESS")
    );
    assert!(request.body.is_empty());
    assert_eq!(
        transport.open_limits()[0].max_body_bytes(),
        crate::launch::http::API_CAP as u64
    );
}

#[test]
fn get_launch_without_a_url_is_none() {
    for body in [
        r#"{"id":"l-1"}"#,
        r#"{"downloadUrl":null}"#,
        r#"{"downloadUrl":""}"#,
    ] {
        assert_eq!(lookup(&json(200, body), "l-1"), Ok(None), "{body}");
    }
    for body in ["[]", "not json", r#"{"downloadUrl":7}"#] {
        assert_eq!(
            lookup(&json(200, body), "l-1"),
            Err(LaunchError::Download(
                "The launch lookup failed: the response is invalid.".to_string()
            )),
            "{body}"
        );
    }
}

#[test]
fn get_launch_failure_names_only_the_status() {
    let transport = json(
        404,
        r#"{"message":"no launch FAKE-SECRET https://evil.example/x"}"#,
    );
    let error = lookup(&transport, "l-1").expect_err("404");
    assert_eq!(
        error.message(),
        "The launch lookup failed with HTTP status 404."
    );
}

#[test]
fn get_launch_does_not_echo_the_token_on_transport_failure() {
    let transport = FakeStreamingTransport::new()
        .open_error(toolkit_aws::AwsError::connect("FAKE-ACCESS refused"));
    let error = lookup(&transport, "l-1").expect_err("unreachable");
    assert_eq!(
        error.message(),
        "The launch lookup failed: the service could not be reached."
    );
    let _ = Duration::ZERO;
}

#[test]
fn launch_ids_are_checked_locally() {
    assert!(validate_launch_id("l-1").is_ok());
    assert!(validate_launch_id(&"x".repeat(MAX_LAUNCH_ID)).is_ok());
    for bad in [String::new(), "x".repeat(MAX_LAUNCH_ID + 1)] {
        assert_eq!(
            validate_launch_id(&bad),
            Err(LaunchError::Config(
                "Invalid launch id: it must be 1 to 2048 characters.".to_string()
            ))
        );
    }
}

#[test]
fn apply_output_is_ordered_json() {
    assert_eq!(
        render_applied(3, 1, Path::new("/repo \"x\"")),
        r#"{"applied":true,"files":3,"directories":1,"into":"/repo \"x\""}"#
    );
}
