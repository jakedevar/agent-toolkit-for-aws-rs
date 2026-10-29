//! Idempotent execution with the Powertools DynamoDB record layout.
//!
//! One item per idempotency key: `id` (partition key), `expiration` (epoch
//! seconds, the table's TTL attribute), `status` (`INPROGRESS`/`COMPLETED`),
//! `data` (the stored response as JSON text) and `in_progress_expiration`
//! (epoch milliseconds when the invocation that holds the lock times out).

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::future::Future;
use std::sync::Arc;

use aws_sdk_dynamodb::error::DisplayErrorContext;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{BoxError, Clock, InvocationContext, Logger};

/// Same retry budget as Powertools for an inconsistent record.
const MAX_RETRIES: u32 = 2;

pub const STATUS_IN_PROGRESS: &str = "INPROGRESS";
pub const STATUS_COMPLETED: &str = "COMPLETED";

/// Settings (Powertools `IdempotencyConfig`).
#[derive(Debug, Clone)]
pub struct IdempotencyConfig {
    /// Dot-separated path to the part of the event that identifies a request
    /// (`body`, `requestContext.requestId`). This is the subset of JMESPath the
    /// template supports; `None` hashes the whole event.
    pub event_key: Option<String>,
    /// Seconds a completed record keeps deduplicating (default one hour).
    pub expires_after_seconds: u64,
    /// Fail instead of skipping idempotency when the key is missing.
    pub raise_on_no_idempotency_key: bool,
}

impl IdempotencyConfig {
    /// The template's setting: `event_key_jmespath="body"`.
    pub fn for_body() -> Self {
        Self {
            event_key: Some("body".to_string()),
            expires_after_seconds: 3600,
            raise_on_no_idempotency_key: false,
        }
    }
}

/// A stored record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub key: String,
    pub status: String,
    /// Epoch seconds after which the record no longer deduplicates.
    pub expiry_seconds: u64,
    /// Epoch milliseconds when the lock holder times out.
    pub in_progress_expiry_ms: Option<u64>,
    pub response: Option<String>,
}

/// What [`PersistenceLayer::put_in_progress`] can answer besides success.
#[derive(Debug)]
pub enum PutError {
    /// A live record with this key exists.
    AlreadyExists,
    Store(BoxError),
}

/// Why an idempotent call failed.
#[derive(Debug)]
pub enum IdempotencyError {
    /// `raise_on_no_idempotency_key` and the event has no key.
    MissingKey,
    /// Another invocation with the same key is still running.
    AlreadyInProgress(String),
    /// The record kept changing under the retries.
    InconsistentState(String),
    /// Reading or writing the store failed.
    Persistence(BoxError),
    /// The stored response is not JSON.
    CorruptRecord(String),
    /// The wrapped function failed (its record was removed so a retry runs).
    Function(BoxError),
}

impl fmt::Display for IdempotencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey => write!(f, "No data found to create a hashed idempotency key"),
            Self::AlreadyInProgress(key) => write!(
                f,
                "Execution already in progress with idempotency key: {key}"
            ),
            Self::InconsistentState(key) => {
                write!(f, "Inconsistent state of the idempotency record: {key}")
            }
            Self::Persistence(e) => write!(f, "Failed to access the idempotency store: {e}"),
            Self::CorruptRecord(key) => {
                write!(f, "Stored idempotent response is not valid JSON: {key}")
            }
            Self::Function(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IdempotencyError {}

/// Storage of idempotency records. [`DynamoDbPersistence`] is the real one.
pub trait PersistenceLayer: Send + Sync {
    /// Create the `INPROGRESS` record, or replace an expired or timed-out one.
    fn put_in_progress(
        &self,
        key: &str,
        expiry_seconds: u64,
        in_progress_expiry_ms: Option<u64>,
        now_seconds: u64,
        now_ms: u64,
    ) -> impl Future<Output = Result<(), PutError>> + Send;

    /// Strongly consistent read.
    fn get_record(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<Record>, BoxError>> + Send;

    /// Mark the record `COMPLETED` and keep the response.
    fn update_completed(
        &self,
        key: &str,
        response: &str,
        expiry_seconds: u64,
    ) -> impl Future<Output = Result<(), BoxError>> + Send;

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BoxError>> + Send;
}

/// Idempotent execution on top of a [`PersistenceLayer`].
pub struct Idempotency<P: PersistenceLayer> {
    store: P,
    config: IdempotencyConfig,
    clock: Arc<dyn Clock>,
}

impl<P: PersistenceLayer> Idempotency<P> {
    pub fn new(store: P, config: IdempotencyConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            config,
            clock,
        }
    }

    /// Run `call` once per idempotency key. A repeat of a completed request
    /// returns the stored response without calling `call`; a repeat of a
    /// running one fails with [`IdempotencyError::AlreadyInProgress`].
    pub async fn run<F, Fut>(
        &self,
        event: &Value,
        context: &InvocationContext,
        logger: &Logger,
        call: F,
    ) -> Result<Value, IdempotencyError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, BoxError>>,
    {
        let Some(key) = self.record_key(event, context, logger)? else {
            return call().await.map_err(IdempotencyError::Function);
        };

        let mut attempts = 0;
        loop {
            let now_ms = self.clock.now_ms();
            let now_seconds = now_ms / 1000;
            let expiry = now_seconds + self.config.expires_after_seconds;
            let in_progress_expiry = context
                .remaining_time_ms(now_ms)
                .map(|remaining| now_ms + remaining);
            match self
                .store
                .put_in_progress(&key, expiry, in_progress_expiry, now_seconds, now_ms)
                .await
            {
                Ok(()) => break,
                Err(PutError::Store(e)) => return Err(IdempotencyError::Persistence(e)),
                Err(PutError::AlreadyExists) => {
                    let existing = self
                        .store
                        .get_record(&key)
                        .await
                        .map_err(IdempotencyError::Persistence)?;
                    if let Some(record) = existing {
                        if let Some(response) = Self::settled(&record, now_seconds, now_ms)? {
                            return Ok(response);
                        }
                    }
                    attempts += 1;
                    if attempts >= MAX_RETRIES {
                        return Err(IdempotencyError::InconsistentState(key));
                    }
                }
            }
        }

        match call().await {
            Ok(response) => {
                let text = serde_json::to_string(&response)
                    .map_err(|e| IdempotencyError::Persistence(Box::new(e)))?;
                let expiry = self.clock.now_ms() / 1000 + self.config.expires_after_seconds;
                self.store
                    .update_completed(&key, &text, expiry)
                    .await
                    .map_err(IdempotencyError::Persistence)?;
                Ok(response)
            }
            Err(error) => {
                // Free the key so the caller's retry runs the function again.
                self.store
                    .delete(&key)
                    .await
                    .map_err(IdempotencyError::Persistence)?;
                Err(IdempotencyError::Function(error))
            }
        }
    }

    /// `Ok(Some(response))` for a completed duplicate, an error for a running
    /// one, `Ok(None)` when the record is expired or its holder timed out (the
    /// caller retries the save).
    fn settled(
        record: &Record,
        now_seconds: u64,
        now_ms: u64,
    ) -> Result<Option<Value>, IdempotencyError> {
        if record.expiry_seconds < now_seconds {
            return Ok(None);
        }
        match record.status.as_str() {
            STATUS_COMPLETED => {
                let text = record.response.as_deref().unwrap_or("null");
                serde_json::from_str(text)
                    .map(Some)
                    .map_err(|_| IdempotencyError::CorruptRecord(record.key.clone()))
            }
            STATUS_IN_PROGRESS => {
                if record.in_progress_expiry_ms.is_some_and(|t| t < now_ms) {
                    Ok(None)
                } else {
                    Err(IdempotencyError::AlreadyInProgress(record.key.clone()))
                }
            }
            _ => Ok(None),
        }
    }

    /// The record id, or `None` when the event has no key and idempotency is
    /// skipped (with a warning).
    fn record_key(
        &self,
        event: &Value,
        context: &InvocationContext,
        logger: &Logger,
    ) -> Result<Option<String>, IdempotencyError> {
        let data = match &self.config.event_key {
            Some(path) => select(event, path),
            None => Some(event),
        };
        let Some(data) = data.filter(|d| !is_missing(d)) else {
            if self.config.raise_on_no_idempotency_key {
                return Err(IdempotencyError::MissingKey);
            }
            logger.warning(
                "No idempotency key value found. Skipping persistence layer and validation operations.",
            );
            return Ok(None);
        };
        Ok(Some(format!(
            "{}.handler#{}",
            context.function_name,
            hash(data)
        )))
    }
}

/// Follow a dot-separated path of object fields; `None` when it does not exist.
pub fn select<'a>(event: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(event, |value, field| value.as_object()?.get(field))
}

/// The Powertools rule for "no idempotency key"
/// (`BasePersistenceLayer.is_missing_idempotency_key`): `None` and the empty
/// string; a list that is empty or holds only `None`; a dict that is empty.
/// The Python check iterates a dict's KEYS (never `None` in JSON), so a
/// non-empty dict identifies a request whatever its values are. Numbers and
/// booleans are never missing: `0` and `false` identify a request.
fn is_missing(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(items) => items.iter().all(Value::is_null),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Compact JSON with object keys sorted at every level (independent of the
/// `serde_json` map implementation another crate may select).
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// SHA-256 (lower-case hex) of the canonical JSON: object keys sorted, no
/// whitespace.
pub fn hash(data: &Value) -> String {
    let mut canonical = String::new();
    canonical_json(data, &mut canonical);
    Sha256::digest(canonical.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// The condition that lets a new invocation take the key: no record, an
/// expired record, or an `INPROGRESS` record whose holder has timed out.
pub const ACQUIRE_CONDITION: &str = "attribute_not_exists(#id) OR #expiry < :now OR \
     (#status = :inprogress AND attribute_exists(#in_progress_expiry) \
     AND #in_progress_expiry < :now_in_millis)";

/// DynamoDB implementation of [`PersistenceLayer`].
#[derive(Clone)]
pub struct DynamoDbPersistence {
    client: Client,
    table_name: String,
}

impl DynamoDbPersistence {
    pub fn new(client: Client, table_name: &str) -> Self {
        Self {
            client,
            table_name: table_name.to_string(),
        }
    }
}

fn number(value: u64) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

fn key_attribute(key: &str) -> AttributeValue {
    AttributeValue::S(key.to_string())
}

fn read_record(item: &HashMap<String, AttributeValue>) -> Option<Record> {
    let text = |name: &str| item.get(name)?.as_s().ok().cloned();
    let seconds = |name: &str| item.get(name)?.as_n().ok()?.parse::<u64>().ok();
    Some(Record {
        key: text("id")?,
        status: text("status")?,
        expiry_seconds: seconds("expiration")?,
        in_progress_expiry_ms: seconds("in_progress_expiration"),
        response: text("data"),
    })
}

impl PersistenceLayer for DynamoDbPersistence {
    async fn put_in_progress(
        &self,
        key: &str,
        expiry_seconds: u64,
        in_progress_expiry_ms: Option<u64>,
        now_seconds: u64,
        now_ms: u64,
    ) -> Result<(), PutError> {
        let mut request = self
            .client
            .put_item()
            .table_name(&self.table_name)
            .item("id", key_attribute(key))
            .item("expiration", number(expiry_seconds))
            .item("status", AttributeValue::S(STATUS_IN_PROGRESS.to_string()))
            .condition_expression(ACQUIRE_CONDITION)
            .expression_attribute_names("#id", "id")
            .expression_attribute_names("#expiry", "expiration")
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#in_progress_expiry", "in_progress_expiration")
            .expression_attribute_values(":now", number(now_seconds))
            .expression_attribute_values(":now_in_millis", number(now_ms))
            .expression_attribute_values(
                ":inprogress",
                AttributeValue::S(STATUS_IN_PROGRESS.to_string()),
            );
        if let Some(timeout) = in_progress_expiry_ms {
            request = request.item("in_progress_expiration", number(timeout));
        }
        match request.send().await {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(aws_sdk_dynamodb::operation::put_item::PutItemError::is_conditional_check_failed_exception) =>
            {
                Err(PutError::AlreadyExists)
            }
            Err(error) => Err(PutError::Store(describe(&error))),
        }
    }

    async fn get_record(&self, key: &str) -> Result<Option<Record>, BoxError> {
        let output = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("id", key_attribute(key))
            .consistent_read(true)
            .send()
            .await
            .map_err(|e| describe(&e))?;
        Ok(output.item().and_then(read_record))
    }

    async fn update_completed(
        &self,
        key: &str,
        response: &str,
        expiry_seconds: u64,
    ) -> Result<(), BoxError> {
        self.client
            .update_item()
            .table_name(&self.table_name)
            .key("id", key_attribute(key))
            .update_expression(
                "SET #response_data = :response_data, #expiry = :expiry, #status = :status",
            )
            .expression_attribute_names("#response_data", "data")
            .expression_attribute_names("#expiry", "expiration")
            .expression_attribute_names("#status", "status")
            .expression_attribute_values(":response_data", AttributeValue::S(response.to_string()))
            .expression_attribute_values(":expiry", number(expiry_seconds))
            .expression_attribute_values(":status", AttributeValue::S(STATUS_COMPLETED.to_string()))
            .send()
            .await
            .map_err(|e| describe(&e))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), BoxError> {
        self.client
            .delete_item()
            .table_name(&self.table_name)
            .key("id", key_attribute(key))
            .send()
            .await
            .map_err(|e| describe(&e))?;
        Ok(())
    }
}

/// An SDK error with its full cause chain (its own `Display` is just
/// "service error").
fn describe<E: std::error::Error + 'static>(error: &E) -> BoxError {
    DisplayErrorContext(error).to_string().into()
}
