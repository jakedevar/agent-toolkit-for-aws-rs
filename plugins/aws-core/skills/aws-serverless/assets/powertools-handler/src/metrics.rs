//! CloudWatch Embedded Metric Format (EMF) metrics in the Powertools layout.

use std::fmt;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::{ordered_object, Clock, LineSink};

/// CloudWatch allows at most 100 metrics in one EMF blob; Powertools flushes
/// automatically when the limit is reached.
pub const MAX_METRICS_PER_BLOB: usize = 100;

/// CloudWatch metric units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    Seconds,
    Microseconds,
    Milliseconds,
    Bytes,
    Kilobytes,
    Megabytes,
    Gigabytes,
    Terabytes,
    Bits,
    Kilobits,
    Megabits,
    Gigabits,
    Terabits,
    Percent,
    Count,
    BytesPerSecond,
    KilobytesPerSecond,
    MegabytesPerSecond,
    GigabytesPerSecond,
    TerabytesPerSecond,
    BitsPerSecond,
    KilobitsPerSecond,
    MegabitsPerSecond,
    GigabitsPerSecond,
    TerabitsPerSecond,
    CountPerSecond,
    NoUnit,
}

impl MetricUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seconds => "Seconds",
            Self::Microseconds => "Microseconds",
            Self::Milliseconds => "Milliseconds",
            Self::Bytes => "Bytes",
            Self::Kilobytes => "Kilobytes",
            Self::Megabytes => "Megabytes",
            Self::Gigabytes => "Gigabytes",
            Self::Terabytes => "Terabytes",
            Self::Bits => "Bits",
            Self::Kilobits => "Kilobits",
            Self::Megabits => "Megabits",
            Self::Gigabits => "Gigabits",
            Self::Terabits => "Terabits",
            Self::Percent => "Percent",
            Self::Count => "Count",
            Self::BytesPerSecond => "Bytes/Second",
            Self::KilobytesPerSecond => "Kilobytes/Second",
            Self::MegabytesPerSecond => "Megabytes/Second",
            Self::GigabytesPerSecond => "Gigabytes/Second",
            Self::TerabytesPerSecond => "Terabytes/Second",
            Self::BitsPerSecond => "Bits/Second",
            Self::KilobitsPerSecond => "Kilobits/Second",
            Self::MegabitsPerSecond => "Megabits/Second",
            Self::GigabitsPerSecond => "Gigabits/Second",
            Self::TerabitsPerSecond => "Terabits/Second",
            Self::CountPerSecond => "Count/Second",
            Self::NoUnit => "None",
        }
    }
}

/// Flushing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsError {
    /// No namespace was given and `POWERTOOLS_METRICS_NAMESPACE` is unset
    /// (Powertools raises `SchemaValidationError`).
    MissingNamespace,
}

impl fmt::Display for MetricsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNamespace => write!(
                f,
                "Must contain a metric namespace: set it in code or with \
                 POWERTOOLS_METRICS_NAMESPACE"
            ),
        }
    }
}

impl std::error::Error for MetricsError {}

struct Metric {
    name: String,
    unit: MetricUnit,
    values: Vec<f64>,
}

/// Metrics for one invocation. [`Metrics::flush`] writes one EMF line.
pub struct Metrics {
    namespace: Option<String>,
    /// The `service` dimension; Powertools adds it only when a service is
    /// configured (`POWERTOOLS_SERVICE_NAME` or the constructor argument).
    service: Option<String>,
    entries: Vec<Metric>,
    sink: Arc<dyn LineSink>,
    clock: Arc<dyn Clock>,
}

impl Metrics {
    /// A `namespace` of `None` makes [`Metrics::flush`] fail; pass
    /// [`namespace_from_env`] to honour `POWERTOOLS_METRICS_NAMESPACE`.
    pub fn new(
        namespace: Option<&str>,
        service: Option<&str>,
        sink: Arc<dyn LineSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            namespace: namespace.map(str::to_string),
            service: service.map(str::to_string),
            entries: Vec::new(),
            sink,
            clock,
        }
    }

    /// Record a value. Adding the same name again keeps both values (an EMF
    /// array). The 101st distinct metric flushes the first 100 first.
    pub fn add_metric(&mut self, name: &str, unit: MetricUnit, value: f64) {
        if let Some(existing) = self.entries.iter_mut().find(|m| m.name == name) {
            existing.values.push(value);
            return;
        }
        if self.entries.len() >= MAX_METRICS_PER_BLOB {
            // Over the limit with no namespace the later flush reports it.
            let _ = self.flush();
        }
        self.entries.push(Metric {
            name: name.to_string(),
            unit,
            values: vec![value],
        });
    }

    /// The `ColdStart` metric: its own EMF line with a `function_name`
    /// dimension, written before the handler runs.
    pub fn emit_cold_start(&self, function_name: &str) {
        let Some(namespace) = self.namespace.as_deref() else {
            return;
        };
        let metric = Metric {
            name: "ColdStart".to_string(),
            unit: MetricUnit::Count,
            values: vec![1.0],
        };
        let mut dimensions = vec![("function_name".to_string(), function_name.to_string())];
        dimensions.extend(self.service_dimension());
        self.sink.emit(&render_blob(
            namespace,
            &dimensions,
            &[metric],
            self.clock.now_ms(),
        ));
    }

    /// Write the collected metrics as one EMF line. Nothing is written when no
    /// metric was added (Powertools warns and publishes nothing).
    pub fn flush(&mut self) -> Result<(), MetricsError> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let namespace = self
            .namespace
            .as_deref()
            .ok_or(MetricsError::MissingNamespace)?;
        let dimensions: Vec<(String, String)> = self.service_dimension().into_iter().collect();
        let line = render_blob(namespace, &dimensions, &self.entries, self.clock.now_ms());
        self.sink.emit(&line);
        self.entries.clear();
        Ok(())
    }

    fn service_dimension(&self) -> Option<(String, String)> {
        self.service
            .as_ref()
            .map(|service| ("service".to_string(), service.clone()))
    }
}

fn render_blob(
    namespace: &str,
    dimensions: &[(String, String)],
    metrics: &[Metric],
    timestamp_ms: u64,
) -> String {
    let dimension_names: Vec<&str> = dimensions.iter().map(|(n, _)| n.as_str()).collect();
    let definitions: Vec<Value> = metrics
        .iter()
        .map(|m| json!({ "Name": m.name, "Unit": m.unit.as_str() }))
        .collect();
    let mut fields = vec![(
        "_aws".to_string(),
        json!({
            "Timestamp": timestamp_ms,
            "CloudWatchMetrics": [{
                "Namespace": namespace,
                "Dimensions": [dimension_names],
                "Metrics": definitions,
            }],
        }),
    )];
    for (name, value) in dimensions {
        fields.push((name.clone(), Value::String(value.clone())));
    }
    for metric in metrics {
        // Always a list, as Powertools writes it: `[1.0]` for one value.
        fields.push((metric.name.clone(), json!(metric.values)));
    }
    ordered_object(&fields)
}

/// `POWERTOOLS_SERVICE_NAME`, when set and not empty. Metrics have no default
/// service (the Logger's `service_undefined` is only its display default):
/// without it the `service` dimension is left out.
pub fn service_from_env() -> Option<String> {
    std::env::var("POWERTOOLS_SERVICE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
}

/// `POWERTOOLS_METRICS_NAMESPACE`, when set and not empty.
pub fn namespace_from_env() -> Option<String> {
    std::env::var("POWERTOOLS_METRICS_NAMESPACE")
        .ok()
        .filter(|s| !s.is_empty())
}
