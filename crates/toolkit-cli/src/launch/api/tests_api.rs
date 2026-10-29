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
