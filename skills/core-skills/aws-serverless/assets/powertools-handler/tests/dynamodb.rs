#![allow(
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]
//! The DynamoDB persistence layer against recorded responses: the requests it
//! sends (record layout and condition expression) and how it reads answers.

use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::{Client, Config};
use aws_smithy_http_client::test_util::{ReplayEvent, StaticReplayClient};
use aws_smithy_types::body::SdkBody;
use powertools_handler::idempotency::{
    PutError, Record, ACQUIRE_CONDITION, STATUS_COMPLETED, STATUS_IN_PROGRESS,
};
use powertools_handler::{DynamoDbPersistence, PersistenceLayer};
use serde_json::{json, Value};

const KEY: &str = "orders-fn.handler#abc";

fn event(status: u16, body: &Value) -> ReplayEvent {
    ReplayEvent::new(
        http::Request::builder()
            .uri("https://dynamodb.us-east-1.amazonaws.com/")
            .body(SdkBody::empty())
            .unwrap(),
        http::Response::builder()
            .status(status)
            .header("content-type", "application/x-amz-json-1.0")
            .body(SdkBody::from(body.to_string()))
            .unwrap(),
    )
}

fn store(events: Vec<ReplayEvent>) -> (DynamoDbPersistence, StaticReplayClient) {
    let replay = StaticReplayClient::new(events);
    let config = Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "fixture", "fixture", None, None, "fixture",
        ))
        .http_client(replay.clone())
        .build();
    (
        DynamoDbPersistence::new(Client::from_conf(config), "IdempotencyTable"),
        replay,
    )
}

fn sent(replay: &StaticReplayClient, index: usize) -> (String, Value) {
    let request = replay
        .actual_requests()
        .nth(index)
        .expect("a request was sent");
    let target = request
        .headers()
        .get("x-amz-target")
        .expect("DynamoDB JSON protocol target header")
        .to_string();
    let body = serde_json::from_slice(request.body().bytes().expect("in-memory body")).unwrap();
    (target, body)
}

#[tokio::test]
async fn the_lock_is_a_conditional_put_in_the_powertools_layout() {
    let (store, replay) = store(vec![event(200, &json!({}))]);
    store
        .put_in_progress(
            KEY,
            1_700_003_600,
            Some(1_700_000_030_123),
            1_700_000_000,
            1_700_000_000_123,
        )
        .await
        .unwrap();
    let (target, body) = sent(&replay, 0);
    assert_eq!(target, "DynamoDB_20120810.PutItem");
    assert_eq!(body["TableName"], "IdempotencyTable");
    assert_eq!(
        body["Item"],
        json!({
            "id": {"S": KEY},
            "expiration": {"N": "1700003600"},
            "status": {"S": "INPROGRESS"},
            "in_progress_expiration": {"N": "1700000030123"},
        })
    );
    assert_eq!(body["ConditionExpression"], ACQUIRE_CONDITION);
    assert_eq!(
        body["ExpressionAttributeNames"],
        json!({
            "#id": "id",
            "#expiry": "expiration",
            "#status": "status",
            "#in_progress_expiry": "in_progress_expiration",
        })
    );
    assert_eq!(
        body["ExpressionAttributeValues"],
        json!({
            ":now": {"N": "1700000000"},
            ":now_in_millis": {"N": "1700000000123"},
            ":inprogress": {"S": "INPROGRESS"},
        })
    );
}

#[tokio::test]
async fn the_lock_omits_the_timeout_when_the_deadline_is_unknown() {
    let (store, replay) = store(vec![event(200, &json!({}))]);
    store.put_in_progress(KEY, 10, None, 1, 1000).await.unwrap();
    let (_, body) = sent(&replay, 0);
    assert!(body["Item"].get("in_progress_expiration").is_none());
}

#[tokio::test]
async fn a_failed_condition_means_the_key_is_taken() {
    let (store, _) = store(vec![event(
        400,
        &json!({
            "__type": "com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException",
            "message": "The conditional request failed",
        }),
    )]);
    let outcome = store.put_in_progress(KEY, 10, None, 1, 1000).await;
    assert!(
        matches!(outcome, Err(PutError::AlreadyExists)),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn any_other_service_error_is_a_store_error() {
    let (store, _) = store(vec![event(
        400,
        &json!({
            "__type": "com.amazon.coral.validate#ValidationException",
            "message": "One or more parameter values were invalid",
        }),
    )]);
    let outcome = store.put_in_progress(KEY, 10, None, 1, 1000).await;
    assert!(matches!(outcome, Err(PutError::Store(_))), "{outcome:?}");
}

#[tokio::test]
async fn a_record_is_read_back_with_a_consistent_read() {
    let (store, replay) = store(vec![event(
        200,
        &json!({"Item": {
            "id": {"S": KEY},
            "expiration": {"N": "1700003600"},
            "status": {"S": "COMPLETED"},
            "data": {"S": "{\"statusCode\":200}"},
        }}),
    )]);
    let record = store.get_record(KEY).await.unwrap().unwrap();
    assert_eq!(
        record,
        Record {
            key: KEY.to_string(),
            status: STATUS_COMPLETED.to_string(),
            expiry_seconds: 1_700_003_600,
            in_progress_expiry_ms: None,
            response: Some("{\"statusCode\":200}".to_string()),
        }
    );
    let (target, body) = sent(&replay, 0);
    assert_eq!(target, "DynamoDB_20120810.GetItem");
    assert_eq!(body["Key"], json!({"id": {"S": KEY}}));
    assert_eq!(body["ConsistentRead"], true);
}

#[tokio::test]
async fn an_in_progress_record_carries_its_timeout() {
    let (store, _) = store(vec![event(
        200,
        &json!({"Item": {
            "id": {"S": KEY},
            "expiration": {"N": "1700003600"},
            "status": {"S": "INPROGRESS"},
            "in_progress_expiration": {"N": "1700000030123"},
        }}),
    )]);
    let record = store.get_record(KEY).await.unwrap().unwrap();
    assert_eq!(record.status, STATUS_IN_PROGRESS);
    assert_eq!(record.in_progress_expiry_ms, Some(1_700_000_030_123));
    assert_eq!(record.response, None);
}

#[tokio::test]
async fn a_missing_or_malformed_item_is_no_record() {
    let (store, _) = store(vec![
        event(200, &json!({})),
        event(200, &json!({"Item": {"id": {"S": KEY}}})),
    ]);
    assert_eq!(store.get_record(KEY).await.unwrap(), None);
    // No expiration or status: not a record this layer wrote.
    assert_eq!(store.get_record(KEY).await.unwrap(), None);
}

#[tokio::test]
async fn completing_stores_the_response_status_and_new_expiry() {
    let (store, replay) = store(vec![event(200, &json!({}))]);
    store
        .update_completed(KEY, "{\"statusCode\":200}", 1_700_003_700)
        .await
        .unwrap();
    let (target, body) = sent(&replay, 0);
    assert_eq!(target, "DynamoDB_20120810.UpdateItem");
    assert_eq!(body["Key"], json!({"id": {"S": KEY}}));
    assert_eq!(
        body["UpdateExpression"],
        "SET #response_data = :response_data, #expiry = :expiry, #status = :status"
    );
    assert_eq!(
        body["ExpressionAttributeNames"],
        json!({"#response_data": "data", "#expiry": "expiration", "#status": "status"})
    );
    assert_eq!(
        body["ExpressionAttributeValues"],
        json!({
            ":response_data": {"S": "{\"statusCode\":200}"},
            ":expiry": {"N": "1700003700"},
            ":status": {"S": "COMPLETED"},
        })
    );
}

#[tokio::test]
async fn deleting_removes_the_item_by_key() {
    let (store, replay) = store(vec![event(200, &json!({}))]);
    store.delete(KEY).await.unwrap();
    let (target, body) = sent(&replay, 0);
    assert_eq!(target, "DynamoDB_20120810.DeleteItem");
    assert_eq!(body["TableName"], "IdempotencyTable");
    assert_eq!(body["Key"], json!({"id": {"S": KEY}}));
}

#[tokio::test]
async fn service_errors_from_the_other_calls_are_returned() {
    let throttled = json!({
        "__type": "com.amazon.coral.validate#ValidationException",
        "message": "bad",
    });
    let (store, _) = store(vec![
        event(400, &throttled),
        event(400, &throttled),
        event(400, &throttled),
    ]);
    assert!(store.get_record(KEY).await.is_err());
    assert!(store.update_completed(KEY, "{}", 1).await.is_err());
    assert!(store.delete(KEY).await.is_err());
}
