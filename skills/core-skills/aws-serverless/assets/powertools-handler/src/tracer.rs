//! X-Ray subsegments for the handler and its methods.
//!
//! Lambda active tracing already records the function segment. This tracer
//! adds subsegments under it, the way Powertools `Tracer` does: one `## handler`
//! subsegment per invocation annotated with `ColdStart` and `Service`, and one
//! `### <method>` subsegment per [`Tracer::capture_method`] call. Documents go
//! to the X-Ray daemon over UDP (`AWS_XRAY_DAEMON_ADDRESS`, set by Lambda).
//!
//! Tracing is a no-op without a sink, when the trace header says
//! `Sampled=0`, and when `POWERTOOLS_TRACE_DISABLED=true`.

use std::collections::hash_map::RandomState;
use std::fmt::Display;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::Clock;

/// Receives finished subsegment documents.
pub trait SegmentSink: Send + Sync {
    fn send(&self, document: &Value);
}

/// Sends documents to the X-Ray daemon: `{"format":"json","version":1}`, a
/// newline, then the document.
pub struct UdpSegmentSink {
    socket: UdpSocket,
    address: String,
}

impl UdpSegmentSink {
    /// The sink for `AWS_XRAY_DAEMON_ADDRESS`, or `None` outside Lambda or
    /// when tracing is disabled (`POWERTOOLS_TRACE_DISABLED=true`).
    pub fn from_env() -> Option<Self> {
        if std::env::var("POWERTOOLS_TRACE_DISABLED").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return None;
        }
        let address = std::env::var("AWS_XRAY_DAEMON_ADDRESS").ok()?;
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        Some(Self { socket, address })
    }
}

impl SegmentSink for UdpSegmentSink {
    fn send(&self, document: &Value) {
        let packet = format!("{{\"format\":\"json\",\"version\":1}}\n{document}");
        // Tracing must never fail an invocation.
        let _ = self.socket.send_to(packet.as_bytes(), &self.address);
    }
}

/// The parts of `X-Amzn-Trace-Id: Root=...;Parent=...;Sampled=1` used here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceHeader {
    pub root: String,
    pub parent: Option<String>,
    pub sampled: bool,
}

impl TraceHeader {
    pub fn parse(header: &str) -> Option<Self> {
        let mut root = None;
        let mut parent = None;
        let mut sampled = false;
        for part in header.split(';') {
            let part = part.trim();
            if let Some(v) = part.strip_prefix("Root=") {
                root = Some(v.to_string());
            } else if let Some(v) = part.strip_prefix("Parent=") {
                parent = Some(v.to_string());
            } else if part == "Sampled=1" {
                sampled = true;
            }
        }
        Some(Self {
            root: root?,
            parent,
            sampled,
        })
    }
}

struct Open {
    id: String,
    name: String,
    parent_id: String,
    start: f64,
    annotations: Value,
}

struct State {
    header: Option<TraceHeader>,
    stack: Vec<Open>,
}

pub struct Tracer {
    sink: Option<Arc<dyn SegmentSink>>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl Tracer {
    pub fn new(sink: Option<Arc<dyn SegmentSink>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            sink,
            clock,
            state: Mutex::new(State {
                header: None,
                stack: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn seconds(&self) -> f64 {
        // Millisecond epoch values stay exact in an f64.
        #[allow(clippy::cast_precision_loss)]
        let ms = self.clock.now_ms() as f64;
        ms / 1000.0
    }

    /// Open the `## handler` subsegment under the Lambda function segment.
    pub fn begin_invocation(&self, trace_header: Option<&str>, cold_start: bool, service: &str) {
        let header = trace_header.and_then(TraceHeader::parse);
        let mut state = self.lock();
        state.stack.clear();
        state.header = header;
        let Some(header) = state.header.clone() else {
            return;
        };
        if self.sink.is_none() || !header.sampled {
            return;
        }
        let open = Open {
            id: new_segment_id(),
            name: "## handler".to_string(),
            parent_id: header.parent.unwrap_or_default(),
            start: self.seconds(),
            annotations: json!({ "ColdStart": cold_start, "Service": service }),
        };
        state.stack.push(open);
    }

    /// Close the handler subsegment, recording `error` as a fault.
    pub fn end_invocation(&self, error: Option<&str>) {
        let mut state = self.lock();
        // Close anything a cancelled method left open, innermost first.
        while let Some(open) = state.stack.pop() {
            let failed = error.filter(|_| state.stack.is_empty()).map(str::to_string);
            self.finish(&state, open, failed);
        }
        state.header = None;
    }

    /// Run `future` inside a `### <name>` subsegment (Powertools
    /// `capture_method`). A returned error marks the subsegment as a fault.
    pub async fn capture_method<T, E, F>(&self, name: &str, future: F) -> Result<T, E>
    where
        E: Display,
        F: Future<Output = Result<T, E>>,
    {
        self.push(&format!("### {name}"));
        let result = future.await;
        self.pop(result.as_ref().err().map(ToString::to_string));
        result
    }

    fn push(&self, name: &str) {
        let mut state = self.lock();
        let Some(parent) = state.stack.last().map(|o| o.id.clone()) else {
            return;
        };
        let open = Open {
            id: new_segment_id(),
            name: name.to_string(),
            parent_id: parent,
            start: self.seconds(),
            annotations: Value::Null,
        };
        state.stack.push(open);
    }

    fn pop(&self, error: Option<String>) {
        let mut state = self.lock();
        // Never pop the handler subsegment: it belongs to `end_invocation`.
        if state.stack.len() < 2 {
            return;
        }
        if let Some(open) = state.stack.pop() {
            self.finish(&state, open, error);
        }
    }

    fn finish(&self, state: &State, open: Open, error: Option<String>) {
        let (Some(sink), Some(header)) = (self.sink.as_ref(), state.header.as_ref()) else {
            return;
        };
        let mut document = json!({
            "id": open.id,
            "name": open.name,
            "trace_id": header.root,
            "parent_id": open.parent_id,
            "type": "subsegment",
            "start_time": open.start,
            "end_time": self.seconds(),
        });
        if !open.annotations.is_null() {
            document["annotations"] = open.annotations;
        }
        if let Some(message) = error {
            document["fault"] = Value::Bool(true);
            document["cause"] = json!({
                "exceptions": [{ "message": message, "type": "Error" }],
            });
        }
        sink.send(&document);
    }
}

/// 16 hex digits from the randomly seeded standard hasher (X-Ray segment ids
/// need to be unique, not unguessable).
fn new_segment_id() -> String {
    let value = RandomState::new().build_hasher().finish();
    format!("{value:016x}")
}
