//! Request construction and retry restrictions (R-API-HTTP).
use super::{ApiError, ApiOperation, Client};
use crate::launch::http::SystemClock;
use toolkit_aws::retry::{NoJitter, RetryPolicy};
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};
use toolkit_aws::Secret;
use serde_json::json;

fn call(fake: &FakeStreamingTransport, op: ApiOperation, id: &str, include: &[&str], body: Option<serde_json::Value>) -> Result<serde_json::Value, ApiError> {
    let sleeper = RecordingSleeper::new();
    Client { transport: fake, base: "https://launch-with-aws.us-east-1.api.aws", region: "us-east-1", token: &Secret::new("FAKE-ACCESS-TOKEN"), clock: &SystemClock, sleeper: &sleeper, jitter: &NoJitter, policy: RetryPolicy::default() }.call(op, id, include, body)
}
fn reply(status: u16, body: &str) -> FakeStreamingTransport {
    FakeStreamingTransport::new().respond(status, &[], vec![FakeChunk::data(body.as_bytes())])
}
#[test]
fn t_api_path_query_and_bearer_header() {
    let fake = reply(200, "{}");
    call(&fake, ApiOperation::GetLaunch, "a/b x", &["execution", "cost estimate"], None).unwrap();
    let requests = fake.requests();
    assert_eq!(requests[0].url, "https://launch-with-aws.us-east-1.api.aws/api/launches/a%2Fb%20x?include=execution&include=cost%20estimate");
    assert_eq!(requests[0].headers.get("authorization"), Some("Bearer FAKE-ACCESS-TOKEN"));
}
#[test]
fn t_api_local_id_validation_precedes_request() {
    for id in [String::new(), "a".repeat(2049)] {
        for op in [ApiOperation::GetLaunch, ApiOperation::Delete, ApiOperation::StartExecution] {
            let fake = reply(200, "{}");
            assert_eq!(call(&fake, op, &id, &[], None), Err(ApiError::InvalidLaunchId));
            assert!(fake.requests().is_empty());
        }
    }
}
#[test]
fn t_api_only_idempotent_client_token_retries_and_is_stable() {
    for op in [ApiOperation::CreateUploadUrl, ApiOperation::CreateLaunch, ApiOperation::StartExecution] {
        let fake = reply(503, "{}").respond(201, &[], vec![FakeChunk::data(b"{}")]);
        call(&fake, op, "l-1", &[], Some(json!({"clientToken":"12345678"}))).unwrap();
        let requests = fake.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body, requests[1].body);
    }
    for op in [ApiOperation::GetLaunch, ApiOperation::ListLaunches, ApiOperation::Delete, ApiOperation::Put] {
        let fake = reply(503, "{}").respond(200, &[], vec![FakeChunk::data(b"{}")]);
        assert!(call(&fake, op, "l-1", &[], None).is_err());
        assert_eq!(fake.requests().len(), 1);
    }
}
#[test]
fn t_api_missing_client_token_never_retries() {
    let fake = reply(503, "{}").respond(201, &[], vec![FakeChunk::data(b"{}")]);
    assert!(call(&fake, ApiOperation::CreateLaunch, "l-1", &[], Some(json!({}))).is_err());
    assert_eq!(fake.requests().len(), 1);
}

#[cfg(unix)]
#[test]
fn t_api_put_validates_url_and_never_echoes_signature() {
    use toolkit_aws::testing::FixedResolver;
    let resolver = FixedResolver::new().answer_any(&["8.8.8.8"]);
    for url in ["http://bucket.amazonaws.com/x?signature=FAKE-SIGNATURE", "https://user:FAKE-SECRET@bucket.amazonaws.com/x", "https://evil.example/x?signature=FAKE-SIGNATURE", "https://service.api.aws/x?signature=FAKE-SIGNATURE"] {
        let fake = reply(200, "{}");
        let sleeper = RecordingSleeper::new();
        let client = Client { transport: &fake, base: "https://launch-with-aws.us-east-1.api.aws", region: "us-east-1", token: &Secret::new("FAKE-ACCESS-TOKEN"), clock: &SystemClock, sleeper: &sleeper, jitter: &NoJitter, policy: RetryPolicy::default() };
        let error = client.put_archive(url, b"archive", &resolver).unwrap_err();
        assert!(!format!("{error} {error:?}").contains("FAKE-"));
        assert!(fake.requests().is_empty());
    }
    let fake = reply(503, "{}").respond(200, &[], vec![]);
    let sleeper = RecordingSleeper::new();
    let client = Client { transport: &fake, base: "https://launch-with-aws.us-east-1.api.aws", region: "us-east-1", token: &Secret::new("FAKE-ACCESS-TOKEN"), clock: &SystemClock, sleeper: &sleeper, jitter: &NoJitter, policy: RetryPolicy::default() };
    let error = client.put_archive("https://bucket.amazonaws.com/x?signature=FAKE-SIGNATURE", b"archive", &resolver).unwrap_err();
    assert!(!format!("{error} {error:?}").contains("FAKE-SIGNATURE"));
    assert_eq!(fake.requests().len(), 1);
    assert_eq!(fake.frames_pulled(), 0);
    assert_eq!(fake.requests()[0].headers.get("content-type"), Some("application/zip"));
    assert_eq!(fake.requests()[0].headers.get("authorization"), None);
    assert_eq!(fake.requests()[0].body, b"archive");
}

#[test]
fn t_api_snapshot_lookup_uses_launch_envelope_and_error_hints() {
    let sleeper = RecordingSleeper::new();
    let token = Secret::new("FAKE-ACCESS-TOKEN");
    let fake = reply(200, r#"{"launch":{"downloadUrl":"https://b.amazonaws.com/x?signature=FAKE-SIGNATURE"}}"#);
    let client = Client { transport: &fake, base: "https://launch-with-aws.us-east-1.api.aws", region: "us-east-1", token: &token, clock: &SystemClock, sleeper: &sleeper, jitter: &NoJitter, policy: RetryPolicy::default() };
    assert_eq!(client.download_url("l/1", false).unwrap().as_deref(), Some("https://b.amazonaws.com/x?signature=FAKE-SIGNATURE"));
    assert_eq!(fake.requests()[0].url, "https://launch-with-aws.us-east-1.api.aws/api/launches/l%2F1?include=download_url");
    for (status, hint) in [(401, "backend rejected the Bearer token"), (404, "no launch with this ID exists in us-east-1")] {
        let fake = reply(status, r#"{"code":"ResourceNotFoundException","message":"FAKE-ACCESS-TOKEN"}"#);
        let client = Client { transport: &fake, base: "https://launch-with-aws.us-east-1.api.aws", region: "us-east-1", token: &token, clock: &SystemClock, sleeper: &sleeper, jitter: &NoJitter, policy: RetryPolicy::default() };
        let error = client.download_url("l-1", false).unwrap_err();
        let text = format!("{error} {error:?}");
        assert!(text.contains(hint));
        assert!(text.contains("GET /api/launches/<id> failed"));
        assert!(!text.contains("FAKE-ACCESS-TOKEN"));
        if status == 404 { assert!(!client.download_url("l-1", true).unwrap_err().to_string().contains(hint)); }
    }
}
