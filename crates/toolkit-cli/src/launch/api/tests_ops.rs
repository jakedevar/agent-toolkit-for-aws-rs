//! R-API-OPS status and response contract.
use super::{Client, ApiOperation, ApiError};
use crate::launch::http::SystemClock;
use toolkit_aws::retry::{NoJitter, RetryPolicy};
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper};
use toolkit_aws::Secret;
use serde_json::json;

fn call(fake: &FakeStreamingTransport, operation: ApiOperation) -> Result<serde_json::Value, ApiError> {
    let sleeper = RecordingSleeper::new();
    Client { transport: fake, base: "https://launch-with-aws.us-east-1.api.aws", region: "us-east-1", token: &Secret::new("FAKE-ACCESS-TOKEN"), clock: &SystemClock, sleeper: &sleeper, jitter: &NoJitter, policy: RetryPolicy::none() }.call(operation, "l-1", &[], None)
}
fn response(status: u16, body: &[u8]) -> FakeStreamingTransport {
    FakeStreamingTransport::new().respond(status, &[], vec![FakeChunk::data(body)])
}
#[test]
fn t_ops_1_delete_204_succeeds_without_reading() {
    let fake = response(204, b"");
    assert_eq!(call(&fake, ApiOperation::Delete).unwrap(), json!({"deleted":true,"id":"l-1","region":"us-east-1"}));
    assert_eq!(fake.frames_pulled(), 0);
    assert_eq!(fake.requests().len(), 1);
}
#[test]
fn t_ops_2_delete_200_ignores_body() {
    let fake = response(200, b"not json");
    assert!(call(&fake, ApiOperation::Delete).is_ok());
    assert_eq!(fake.frames_pulled(), 0);
}
#[test]
fn t_ops_3_json_operations_require_objects() {
    for op in [ApiOperation::CreateUploadUrl, ApiOperation::CreateLaunch, ApiOperation::GetLaunch, ApiOperation::ListLaunches, ApiOperation::StartExecution] {
        for body in [b"".as_slice(), b"bad", b"[]"] {
            let fake = response(200, body);
            assert_eq!(call(&fake, op), Err(ApiError::InvalidResponse));
            assert_eq!(fake.requests().len(), 1);
        }
    }
}
#[test]
fn t_ops_4_unmodelled_2xx_is_success() {
    assert_eq!(call(&response(200, b"{}"), ApiOperation::CreateLaunch).unwrap(), json!({}));
}
#[test]
fn t_ops_5_redirect_is_an_error_without_retry_or_body_read() {
    let fake = response(307, b"secret");
    let error = call(&fake, ApiOperation::GetLaunch).unwrap_err().to_string();
    assert!(error.contains("307"));
    assert_eq!(fake.requests().len(), 1);
    assert_eq!(fake.frames_pulled(), 0);
}
#[test]
fn t_ops_6_put_never_reads_body_or_retries() {
    for (status, ok) in [(200, true), (307, false), (503, false)] {
        let fake = response(status, b"<xml>secret</xml>");
        let result = call(&fake, ApiOperation::Put);
        assert_eq!(result.is_ok(), ok);
        if let Err(error) = result { assert!(!error.to_string().contains("secret")); }
        assert_eq!(fake.frames_pulled(), 0);
        assert_eq!(fake.requests().len(), 1);
    }
}
#[test]
fn t_ops_7_service_errors_use_only_allowlisted_codes() {
    for op in [ApiOperation::CreateUploadUrl, ApiOperation::CreateLaunch, ApiOperation::GetLaunch, ApiOperation::ListLaunches, ApiOperation::StartExecution, ApiOperation::Delete] {
        let fake = response(404, br#"{"__type":"ResourceNotFoundException","message":"FAKE-ACCESS-TOKEN"}"#);
        let error = call(&fake, op).unwrap_err();
        assert!(error.to_string().contains("ResourceNotFoundException"));
        assert!(!format!("{error:?} {error}").contains("FAKE-ACCESS-TOKEN"));
    }
}
