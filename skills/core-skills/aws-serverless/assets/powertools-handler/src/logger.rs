//! Structured JSON logging in the Powertools Logger layout.

use std::panic::Location;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::{ordered_object, Clock, InvocationContext, LineSink};

/// Log levels, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Critical => "CRITICAL",
        }
    }

    /// Parse a level name case-insensitively (`WARN` is accepted).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_uppercase().as_str() {
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARN" | "WARNING" => Some(Self::Warning),
            "ERROR" => Some(Self::Error),
            "CRITICAL" | "FATAL" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// JSON logger. Create once; call [`Logger::set_invocation`] per invocation.
pub struct Logger {
    service: String,
    level: Level,
    sink: Arc<dyn LineSink>,
    clock: Arc<dyn Clock>,
    context: Mutex<Vec<(String, Value)>>,
}

impl Logger {
    pub fn new(
        service: &str,
        level: Level,
        sink: Arc<dyn LineSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            service: service.to_string(),
            level,
            sink,
            clock,
            context: Mutex::new(Vec::new()),
        }
    }

    /// Service and level from the Powertools environment variables:
    /// `POWERTOOLS_SERVICE_NAME` (default `service_undefined`) and
    /// `POWERTOOLS_LOG_LEVEL`, then `LOG_LEVEL` (default `INFO`; an
    /// unrecognised level falls back to `INFO`).
    pub fn from_env(sink: Arc<dyn LineSink>, clock: Arc<dyn Clock>) -> Self {
        let service = std::env::var("POWERTOOLS_SERVICE_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "service_undefined".to_string());
        let level = std::env::var("POWERTOOLS_LOG_LEVEL")
            .or_else(|_| std::env::var("LOG_LEVEL"))
            .ok()
            .and_then(|s| Level::parse(&s))
            .unwrap_or(Level::Info);
        Self::new(&service, level, sink, clock)
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    /// Add the Lambda context keys to every following line.
    pub fn set_invocation(&self, context: &InvocationContext, cold_start: bool) {
        let mut fields = vec![
            ("cold_start".to_string(), Value::Bool(cold_start)),
            (
                "function_name".to_string(),
                Value::String(context.function_name.clone()),
            ),
            (
                "function_memory_size".to_string(),
                Value::from(context.memory_size_mb),
            ),
            (
                "function_arn".to_string(),
                Value::String(context.function_arn.clone()),
            ),
            (
                "function_request_id".to_string(),
                Value::String(context.request_id.clone()),
            ),
        ];
        if let Some(trace) = context.xray_trace_id.as_deref() {
            // Powertools logs the `Root` part of the trace header.
            let root = trace
                .split(';')
                .find_map(|p| p.strip_prefix("Root="))
                .unwrap_or(trace);
            fields.push(("xray_trace_id".to_string(), Value::String(root.to_string())));
        }
        *self
            .context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fields;
    }

    #[track_caller]
    pub fn debug(&self, message: &str) {
        self.log(Level::Debug, message, Location::caller());
    }

    #[track_caller]
    pub fn info(&self, message: &str) {
        self.log(Level::Info, message, Location::caller());
    }

    #[track_caller]
    pub fn warning(&self, message: &str) {
        self.log(Level::Warning, message, Location::caller());
    }

    #[track_caller]
    pub fn error(&self, message: &str) {
        self.log(Level::Error, message, Location::caller());
    }

    fn log(&self, level: Level, message: &str, at: &Location<'_>) {
        if level < self.level {
            return;
        }
        let mut fields = vec![
            ("level".to_string(), Value::from(level.as_str())),
            (
                "location".to_string(),
                Value::String(format!("{}:{}", at.file(), at.line())),
            ),
            ("message".to_string(), Value::from(message)),
            (
                "timestamp".to_string(),
                Value::String(format_timestamp(self.clock.now_ms())),
            ),
            ("service".to_string(), Value::String(self.service.clone())),
        ];
        fields.extend(
            self.context
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .cloned(),
        );
        self.sink.emit(&ordered_object(&fields));
    }
}

/// `2023-11-14T22:13:20.123Z` for milliseconds since the Unix epoch.
pub fn format_timestamp(epoch_ms: u64) -> String {
    let ms = i64::try_from(epoch_ms).unwrap_or(i64::MAX);
    let days = ms.div_euclid(86_400_000);
    let in_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second, milli) = (
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
