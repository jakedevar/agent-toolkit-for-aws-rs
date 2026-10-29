//! Lambda handler with structured logging, X-Ray tracing, EMF metrics and
//! DynamoDB idempotency wired, as one template to copy.
//!
//! This is the Rust counterpart of the former `powertools-handler.py`. AWS
//! Lambda Powertools has no Rust edition, so the four utilities are small
//! modules here that keep the Powertools output formats:
//!
//! - [`logger`]: one JSON object per line (`level`, `location`, `message`,
//!   `timestamp`, `service`, `cold_start`, `function_*`, `xray_trace_id`);
//! - [`metrics`]: CloudWatch Embedded Metric Format lines, with the cold start
//!   metric;
//! - [`tracer`]: X-Ray subsegments sent to the daemon over UDP;
//! - [`idempotency`]: the Powertools DynamoDB record layout (`id`,
//!   `expiration`, `status`, `data`, `in_progress_expiration`).
//!
//! [`Handler`] wires them. Replace [`process`] with your business logic. All
//! I/O goes through traits ([`LineSink`], [`Clock`], [`idempotency::PersistenceLayer`],
//! [`tracer::SegmentSink`]) so the tests in `tests/` run without AWS.

#![allow(
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::must_use_candidate
)]

pub mod idempotency;
pub mod logger;
pub mod metrics;
pub mod tracer;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub use idempotency::{
    DynamoDbPersistence, Idempotency, IdempotencyConfig, IdempotencyError, PersistenceLayer,
};
pub use logger::{Level, Logger};
pub use metrics::{MetricUnit, Metrics, MetricsError};
pub use tracer::{SegmentSink, Tracer, UdpSegmentSink};

/// The error type every fallible template function uses.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Source of the current time, in milliseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
}

/// Destination of the log and metric lines (stdout in Lambda).
pub trait LineSink: Send + Sync {
    fn emit(&self, line: &str);
}

/// Writes each line to standard output, which CloudWatch Logs captures.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdoutSink;

impl LineSink for StdoutSink {
    fn emit(&self, line: &str) {
        println!("{line}");
    }
}

/// What the handler needs to know about the current invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvocationContext {
    pub function_name: String,
    pub memory_size_mb: i32,
    pub function_arn: String,
    pub request_id: String,
    /// The `X-Amzn-Trace-Id` header value of the invocation, if any.
    pub xray_trace_id: Option<String>,
    /// Invocation deadline in milliseconds since the Unix epoch (0 = unknown).
    pub deadline_ms: u64,
}

impl InvocationContext {
    /// Milliseconds left before the deadline, or `None` when it is unknown.
    pub fn remaining_time_ms(&self, now_ms: u64) -> Option<u64> {
        (self.deadline_ms != 0).then(|| self.deadline_ms.saturating_sub(now_ms))
    }
}

/// Render `fields` as one JSON object, keeping their order.
///
/// `serde_json::Map` sorts its keys, but Powertools log and EMF lines have a
/// fixed field order (`level` first, `_aws` first), so the line is assembled
/// by hand from individually serialized values.
pub(crate) fn ordered_object(fields: &[(String, Value)]) -> String {
    let mut out = String::from("{");
    for (index, (name, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&Value::String(name.clone()).to_string());
        out.push(':');
        out.push_str(&value.to_string());
    }
    out.push('}');
    out
}

/// Replace with your business logic.
#[allow(clippy::unused_async)] // real business logic awaits
pub async fn process(_event: &Value) -> Result<Value, BoxError> {
    Ok(json!({ "message": "success" }))
}

/// Settings of the template, with the values the Python template hard-coded.
#[derive(Debug, Clone)]
pub struct HandlerSettings {
    /// Metrics namespace. As in the Python template an explicit namespace wins
    /// over `POWERTOOLS_METRICS_NAMESPACE` (see [`metrics::namespace_from_env`]
    /// to use the variable instead). Replace `MyApp`.
    pub metrics_namespace: String,
    /// CORS origin of the response; replace with your domain.
    pub allow_origin: String,
    /// The `service` metric dimension. `None` (the default) leaves the
    /// dimension out, as Python does without `POWERTOOLS_SERVICE_NAME`; use
    /// [`metrics::service_from_env`] to read the variable. This is independent
    /// of the Logger's service name, which falls back to `service_undefined`.
    pub metrics_service: Option<String>,
}

impl Default for HandlerSettings {
    fn default() -> Self {
        Self {
            metrics_namespace: "MyApp".to_string(),
            allow_origin: "https://your-domain.example".to_string(),
            metrics_service: None,
        }
    }
}

/// Why an invocation failed.
#[derive(Debug)]
pub enum HandlerError {
    /// The idempotency layer refused or failed.
    Idempotency(IdempotencyError),
    /// The business logic failed.
    Process(BoxError),
    /// Metrics could not be flushed (for example no namespace).
    Metrics(MetricsError),
}

impl fmt::Display for HandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idempotency(e) => write!(f, "{e}"),
            Self::Process(e) => write!(f, "{e}"),
            Self::Metrics(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HandlerError {}

/// The handler with its collaborators.
pub struct Handler<P: PersistenceLayer> {
    pub logger: Logger,
    pub tracer: Tracer,
    pub idempotency: Idempotency<P>,
    pub sink: Arc<dyn LineSink>,
    pub clock: Arc<dyn Clock>,
    pub service: String,
    pub settings: HandlerSettings,
    /// True until the first invocation of this execution environment.
    cold_start: AtomicBool,
}

impl<P: PersistenceLayer> Handler<P> {
    pub fn new(
        logger: Logger,
        tracer: Tracer,
        idempotency: Idempotency<P>,
        sink: Arc<dyn LineSink>,
        clock: Arc<dyn Clock>,
        service: &str,
        settings: HandlerSettings,
    ) -> Self {
        Self {
            logger,
            tracer,
            idempotency,
            sink,
            clock,
            service: service.to_string(),
            settings,
            cold_start: AtomicBool::new(true),
        }
    }

    /// One invocation: log context, trace, metrics and idempotency around
    /// [`process`]. Mirrors the decorator stack of the Python template
    /// (logger, tracer, metrics, idempotent, innermost last). The event is
    /// never logged: events may contain auth tokens, cookies or PII.
    pub async fn handle(
        &self,
        event: Value,
        context: &InvocationContext,
    ) -> Result<Value, HandlerError> {
        let cold_start = self.cold_start.swap(false, Ordering::SeqCst);
        self.logger.set_invocation(context, cold_start);
        self.tracer
            .begin_invocation(context.xray_trace_id.as_deref(), cold_start, &self.service);

        let mut metrics = Metrics::new(
            Some(&self.settings.metrics_namespace),
            self.settings.metrics_service.as_deref(),
            self.sink.clone(),
            self.clock.clone(),
        );
        if cold_start {
            metrics.emit_cold_start(&context.function_name);
        }

        let result = self.run(&event, context, &mut metrics).await;
        let failure = result.as_ref().err().map(ToString::to_string);
        self.tracer.end_invocation(failure.as_deref());
        // The metrics decorator flushes in a finally clause: also on failure.
        let flushed = metrics.flush().map_err(HandlerError::Metrics);
        let response = result?;
        flushed?;
        Ok(response)
    }

    async fn run(
        &self,
        event: &Value,
        context: &InvocationContext,
        metrics: &mut Metrics,
    ) -> Result<Value, HandlerError> {
        let allow_origin = self.settings.allow_origin.clone();
        let logger = &self.logger;
        let tracer = &self.tracer;
        let call = move || async move {
            logger.info("Processing request");
            let result = tracer.capture_method("process", process(event)).await?;
            metrics.add_metric("RequestsProcessed", MetricUnit::Count, 1.0);
            let body = serde_json::to_string(&result)?;
            Ok::<Value, BoxError>(json!({
                "statusCode": 200,
                "headers": {
                    "Content-Type": "application/json",
                    "Access-Control-Allow-Origin": allow_origin,
                },
                "body": body,
            }))
        };
        self.idempotency
            .run(event, context, &self.logger, call)
            .await
            .map_err(|e| match e {
                IdempotencyError::Function(inner) => HandlerError::Process(inner),
                other => HandlerError::Idempotency(other),
            })
    }
}
