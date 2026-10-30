//! The snapshot command's output format.
use super::*;
use std::path::Path;

#[test]
fn apply_output_is_ordered_json() {
    assert_eq!(
        render_applied(3, 1, Path::new("/repo \"x\"")),
        r#"{"applied":true,"files":3,"directories":1,"into":"/repo \"x\""}"#
    );
}

#[test]
fn snapshot_lookup_uses_get_launch_error_mapping() {
    use crate::launch::http::SystemClock;
    use toolkit_aws::retry::{NoJitter, RetryPolicy};
    use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};
    use toolkit_aws::Secret;

    let fake = FakeStreamingTransport::new().respond(
        404,
        &[],
        vec![FakeChunk::data(
            br#"{"code":"ResourceNotFoundException","message":"PRIVATE RESPONSE"}"#,
        )],
    );
    let sleeper = RecordingSleeper::new();
    let token = Secret::new("FAKE-TOKEN");
    let client = super::super::api::Client {
        transport: &fake,
        base: "https://launch-with-aws.us-east-1.api.aws",
        region: "us-east-1",
        token: &token,
        clock: &SystemClock,
        sleeper: &sleeper,
        jitter: &NoJitter,
        policy: RetryPolicy::none(),
    };
    let error = lookup_download_url(&client, "a/b", false).unwrap_err();
    assert_eq!(error.message(), "GET /api/launches/<id> failed (404): ResourceNotFoundException Hint: no launch with this ID exists in us-east-1. A launch exists only in the region it was created in; retry with --region set to that region (one of: us-east-1, eu-central-1).");
    let requests = fake.requests();
    assert_eq!(
        requests[0].url,
        "https://launch-with-aws.us-east-1.api.aws/api/launches/a%2Fb?include=download_url"
    );
    assert_eq!(
        requests[0].headers.get("authorization"),
        Some("Bearer FAKE-TOKEN")
    );
}
