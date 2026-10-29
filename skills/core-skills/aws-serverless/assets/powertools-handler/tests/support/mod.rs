#![allow(clippy::doc_markdown, clippy::similar_names)]
//! Test doubles: in-memory sinks, a settable clock and a DynamoDB-like store.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use powertools_handler::idempotency::{PutError, Record, STATUS_COMPLETED, STATUS_IN_PROGRESS};
use powertools_handler::{
    BoxError, Clock, Handler, HandlerSettings, Idempotency, IdempotencyConfig, InvocationContext,
    LineSink, Logger, PersistenceLayer, SegmentSink, Tracer,
};
use serde_json::Value;

/// 2023-11-14T22:13:20.123Z
pub const NOW_MS: u64 = 1_700_000_000_123;

pub const TRACE_HEADER: &str =
    "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";

#[derive(Default)]
pub struct MemorySink(Mutex<Vec<String>>);

impl LineSink for MemorySink {
    fn emit(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_string());
    }
}

impl MemorySink {
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    pub fn json(&self) -> Vec<Value> {
        self.lines()
            .iter()
            .map(|l| serde_json::from_str(l).expect("every emitted line is JSON"))
            .collect()
    }

    /// Parsed lines whose `level` is set (the logger's), skipping EMF blobs.
    pub fn logs(&self) -> Vec<Value> {
        self.json()
            .into_iter()
            .filter(|v| v.get("level").is_some())
            .collect()
    }

    /// Parsed EMF blobs.
    pub fn metrics(&self) -> Vec<Value> {
        self.json()
            .into_iter()
            .filter(|v| v.get("_aws").is_some())
            .collect()
    }
}

pub struct FixedClock(AtomicU64);

impl FixedClock {
    pub fn at(ms: u64) -> Self {
        Self(AtomicU64::new(ms))
    }

    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
pub struct MemorySegments(Mutex<Vec<Value>>);

impl SegmentSink for MemorySegments {
    fn send(&self, document: &Value) {
        self.0.lock().unwrap().push(document.clone());
    }
}

impl MemorySegments {
    pub fn documents(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }
}

/// What the store was asked to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Put(String),
    Get(String),
    Complete(String),
    Delete(String),
}

#[derive(Default)]
struct StoreState {
    records: HashMap<String, Record>,
    calls: Vec<Call>,
    fail_put: Option<String>,
    fail_complete: Option<String>,
    fail_delete: Option<String>,
    /// `get_record` finds nothing, as if the item vanished after the conflict.
    hide_records: bool,
}

/// Same acquire rule as the DynamoDB condition expression.
#[derive(Clone, Default)]
pub struct MemoryStore(Arc<Mutex<StoreState>>);

impl MemoryStore {
    pub fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().calls.clone()
    }

    pub fn record(&self, key: &str) -> Option<Record> {
        self.0.lock().unwrap().records.get(key).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        self.0.lock().unwrap().records.keys().cloned().collect()
    }

    pub fn insert(&self, record: Record) {
        self.0
            .lock()
            .unwrap()
            .records
            .insert(record.key.clone(), record);
    }

    pub fn fail_put(&self, message: &str) {
        self.0.lock().unwrap().fail_put = Some(message.to_string());
    }

    pub fn fail_complete(&self, message: &str) {
        self.0.lock().unwrap().fail_complete = Some(message.to_string());
    }

    pub fn fail_delete(&self, message: &str) {
        self.0.lock().unwrap().fail_delete = Some(message.to_string());
    }

    pub fn hide_records(&self) {
        self.0.lock().unwrap().hide_records = true;
    }
}

impl PersistenceLayer for MemoryStore {
    async fn put_in_progress(
        &self,
        key: &str,
        expiry_seconds: u64,
        in_progress_expiry_ms: Option<u64>,
        now_seconds: u64,
        now_ms: u64,
    ) -> Result<(), PutError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(Call::Put(key.to_string()));
        if let Some(message) = &state.fail_put {
            return Err(PutError::Store(message.clone().into()));
        }
        if let Some(existing) = state.records.get(key) {
            let acquirable = existing.expiry_seconds < now_seconds
                || (existing.status == STATUS_IN_PROGRESS
                    && existing.in_progress_expiry_ms.is_some_and(|t| t < now_ms));
            if !acquirable {
                return Err(PutError::AlreadyExists);
            }
        }
        state.records.insert(
            key.to_string(),
            Record {
                key: key.to_string(),
                status: STATUS_IN_PROGRESS.to_string(),
                expiry_seconds,
                in_progress_expiry_ms,
                response: None,
            },
        );
        Ok(())
    }

    async fn get_record(&self, key: &str) -> Result<Option<Record>, BoxError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(Call::Get(key.to_string()));
        if state.hide_records {
            return Ok(None);
        }
        Ok(state.records.get(key).cloned())
    }

    async fn update_completed(
        &self,
        key: &str,
        response: &str,
        expiry_seconds: u64,
    ) -> Result<(), BoxError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(Call::Complete(key.to_string()));
        if let Some(message) = &state.fail_complete {
            return Err(message.clone().into());
        }
        let record = state.records.get_mut(key).ok_or("no such record")?;
        record.status = STATUS_COMPLETED.to_string();
        record.response = Some(response.to_string());
        record.expiry_seconds = expiry_seconds;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), BoxError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(Call::Delete(key.to_string()));
        if let Some(message) = &state.fail_delete {
            return Err(message.clone().into());
        }
        state.records.remove(key);
        Ok(())
    }
}

pub fn context() -> InvocationContext {
    InvocationContext {
        function_name: "orders-fn".to_string(),
        memory_size_mb: 256,
        function_arn: "arn:aws:lambda:us-east-1:123456789012:function:orders-fn".to_string(),
        request_id: "req-0001".to_string(),
        xray_trace_id: Some(TRACE_HEADER.to_string()),
        deadline_ms: NOW_MS + 30_000,
    }
}

/// A handler with all collaborators in memory.
pub struct Rig {
    pub handler: Handler<MemoryStore>,
    pub sink: Arc<MemorySink>,
    pub segments: Arc<MemorySegments>,
    pub clock: Arc<FixedClock>,
    pub store: MemoryStore,
}

pub fn rig() -> Rig {
    rig_with(IdempotencyConfig::for_body(), true)
}

pub fn rig_with(config: IdempotencyConfig, traced: bool) -> Rig {
    let settings = HandlerSettings {
        metrics_service: Some("orders".to_string()),
        ..HandlerSettings::default()
    };
    rig_with_settings(config, traced, settings)
}

pub fn rig_with_settings(
    config: IdempotencyConfig,
    traced: bool,
    settings: HandlerSettings,
) -> Rig {
    let sink = Arc::new(MemorySink::default());
    let segments = Arc::new(MemorySegments::default());
    let clock = Arc::new(FixedClock::at(NOW_MS));
    let store = MemoryStore::default();
    let logger = Logger::new(
        "orders",
        powertools_handler::Level::Info,
        sink.clone(),
        clock.clone(),
    );
    let tracer = Tracer::new(
        traced.then(|| segments.clone() as Arc<dyn SegmentSink>),
        clock.clone(),
    );
    let handler = Handler::new(
        logger,
        tracer,
        Idempotency::new(store.clone(), config, clock.clone()),
        sink.clone(),
        clock.clone(),
        "orders",
        settings,
    );
    Rig {
        handler,
        sink,
        segments,
        clock,
        store,
    }
}
