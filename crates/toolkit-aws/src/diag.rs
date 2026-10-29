//! Allowlisted diagnostics (R-LOG, #4996; #4413 S1).
//!
//! Nothing in this workspace installs a `tracing` subscriber or a `log`
//! logger (the R-LOG gate test enforces it), so dependency events, which can
//! carry a `credential_process` stderr or a provider's text, are never even
//! formatted. Proxy diagnostics therefore do not use `tracing`: they go
//! through [`Diag`], a sink for a **closed** set of [`DiagEvent`]s whose
//! fields are counts, statuses and fixed kinds only. No event carries a
//! provider string, a header value, a body, a URL or any error's `Display`.
//! Each rendered line is length-bounded and passed through
//! [`crate::redact_text`] as a second line of defence (not the control).
//! Lines go only to the writer given (stderr in production), never stdout.
//!
//! Later slices add variants here; each variant must keep to the allowlisted
//! field kinds. S2 added [`ProxyEvent`]: the proxy's protocol events, as
//! fixed kinds plus counts and booleans.

use std::fmt::Write as _;
use std::io::Write;

use crate::preflight::{CredentialRefusal, InvalidReason};
use crate::redact::redact_text;

/// Longest rendered diagnostic line, in characters.
pub const MAX_DIAG_CHARS: usize = 512;

/// A diagnostic event: allowlisted structured fields only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagEvent {
    /// Platform trust-store entries that failed to load while others loaded
    /// (R-ROOTS): a count, never a path or error text.
    TlsRootErrorsTolerated {
        /// How many.
        count: usize,
    },
    /// A request failed closed for credentials (R-PREC, R-STICKY).
    CredentialFailure {
        /// The typed refusal.
        refusal: CredentialRefusal,
    },
    /// A request failed in the transport, by boundary error code.
    TransportFailure {
        /// [`crate::AwsError::code`].
        code: &'static str,
    },
    /// An HTTP response status.
    HttpStatus {
        /// The status code.
        status: u16,
    },
    /// An MCP proxy protocol event (#4413 S2).
    Proxy(ProxyEvent),
}

/// MCP proxy protocol events (#4413 S2): fixed kinds and counts only. None
/// carries a message body, a key or value from a message, a header value or
/// any error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyEvent {
    /// A stdin line passed the line bound and was discarded.
    StdinLineTooLarge {
        /// Whether its id was recovered (and answered).
        id_recovered: bool,
    },
    /// A stdin line was not UTF-8, not JSON or not a JSON-RPC message.
    StdinLineInvalid,
    /// An upstream message was not UTF-8 or not JSON, or passed the depth or
    /// node bound; it was dropped.
    UpstreamMessageMalformed,
    /// An upstream message was JSON but not an object or a batch; dropped.
    UpstreamMessageNotRpc,
    /// An upstream body, SSE line or SSE event passed its size bound.
    UpstreamResponseTooLarge,
    /// A request failed because the byte budget could not be reserved.
    BudgetExhausted,
    /// The server's `Mcp-Session-Id` was refused.
    SessionIdRefused,
    /// The server's negotiated protocol version was refused.
    ProtocolVersionRefused,
    /// Configured `_meta` keys the client's own values kept.
    MetaKeysKept {
        /// How many.
        count: usize,
    },
    /// A client `params._meta` was not an object; nothing was injected.
    MetaNotObject,
    /// A client notification or response could not be delivered upstream.
    NotificationFailed,
    /// stdout could not be written.
    StdoutClosed,
    /// `--read-only`: a `tools/call` for a tool not listed with a truthy
    /// `readOnlyHint` was answered locally and never sent (#4413 S3).
    ReadOnlyToolRefused,
    /// The proxy's own deadline fired for a request and a
    /// `notifications/cancelled` was sent upstream for it (#4413 S3).
    TimeoutCancelForwarded,
}

impl ProxyEvent {
    fn render(self, line: &mut String) -> std::fmt::Result {
        match self {
            Self::StdinLineTooLarge { id_recovered } => {
                write!(line, "stdin-line-too-large id={id_recovered}")
            }
            Self::StdinLineInvalid => line.write_str("stdin-line-invalid"),
            Self::UpstreamMessageMalformed => line.write_str("upstream-message-malformed"),
            Self::UpstreamMessageNotRpc => line.write_str("upstream-message-not-rpc"),
            Self::UpstreamResponseTooLarge => line.write_str("upstream-response-too-large"),
            Self::BudgetExhausted => line.write_str("budget-exhausted"),
            Self::SessionIdRefused => line.write_str("session-id-refused"),
            Self::ProtocolVersionRefused => line.write_str("protocol-version-refused"),
            Self::MetaKeysKept { count } => write!(line, "meta-keys-kept count={count}"),
            Self::MetaNotObject => line.write_str("meta-not-object"),
            Self::NotificationFailed => line.write_str("notification-failed"),
            Self::StdoutClosed => line.write_str("stdout-closed"),
            Self::ReadOnlyToolRefused => line.write_str("read-only-tool-refused"),
            Self::TimeoutCancelForwarded => line.write_str("timeout-cancel-forwarded"),
        }
    }
}

impl DiagEvent {
    /// The line body (without the component prefix).
    fn render(self) -> String {
        let mut line = String::new();
        let _ = match self {
            Self::TlsRootErrorsTolerated { count } => {
                write!(line, "tls-root-errors-tolerated count={count}")
            }
            Self::CredentialFailure { refusal } => {
                write!(line, "credential-failure kind={}", refusal_kind(refusal))
            }
            Self::TransportFailure { code } => write!(line, "transport-failure code={code}"),
            Self::HttpStatus { status } => write!(line, "http-status status={status}"),
            Self::Proxy(event) => event.render(&mut line),
        };
        line
    }
}

fn refusal_kind(refusal: CredentialRefusal) -> &'static str {
    match refusal {
        CredentialRefusal::InvalidConfiguration(InvalidReason::CredentialProcessNeedsNullStdin) => {
            "credential-process-needs-null-stdin"
        }
        CredentialRefusal::InvalidConfiguration(_) => "invalid-configuration",
        CredentialRefusal::ProviderFailed => "provider-failed",
        CredentialRefusal::ConfiguredSourceNotLoaded => "configured-source-not-loaded",
        CredentialRefusal::NoCredentials => "no-credentials",
        CredentialRefusal::SignedSessionLostCredentials => "signed-session-lost-credentials",
    }
}

/// Diagnostic severity, ordered from the most verbose (`--log-level`, #4413
/// S3: upstream's `DEBUG`, `INFO`, `WARNING`, `ERROR` and `CRITICAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Everything.
    Debug,
    /// Informational events.
    Info,
    /// Recoverable problems.
    Warning,
    /// Failed requests.
    Error,
    /// Nothing the proxy emits is this severe; `CRITICAL` silences the sink.
    Critical,
}

impl Level {
    /// Parse an upstream `--log-level` name (ASCII case-insensitive).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARNING" => Some(Self::Warning),
            "ERROR" => Some(Self::Error),
            "CRITICAL" => Some(Self::Critical),
            _ => None,
        }
    }
}

impl DiagEvent {
    /// The severity of this event.
    #[must_use]
    pub fn level(self) -> Level {
        match self {
            Self::TlsRootErrorsTolerated { .. } => Level::Info,
            Self::CredentialFailure { .. } => Level::Error,
            Self::TransportFailure { .. } | Self::HttpStatus { .. } => Level::Warning,
            Self::Proxy(event) => match event {
                ProxyEvent::BudgetExhausted
                | ProxyEvent::SessionIdRefused
                | ProxyEvent::ProtocolVersionRefused
                | ProxyEvent::StdoutClosed => Level::Error,
                ProxyEvent::ReadOnlyToolRefused | ProxyEvent::TimeoutCancelForwarded => Level::Info,
                _ => Level::Warning,
            },
        }
    }
}

/// The diagnostics sink.
pub struct Diag {
    component: &'static str,
    sink: Box<dyn Write + Send>,
    threshold: Level,
}

impl Diag {
    /// A sink writing `toolkit <component>: <event>` lines to `sink`.
    #[must_use]
    pub fn new(component: &'static str, sink: Box<dyn Write + Send>) -> Self {
        Self {
            component,
            sink,
            threshold: Level::Debug,
        }
    }

    /// Emit only events at `threshold` or above (a new sink emits all).
    #[must_use]
    pub fn with_threshold(mut self, threshold: Level) -> Self {
        self.threshold = threshold;
        self
    }

    /// The production sink: stderr.
    #[must_use]
    pub fn stderr(component: &'static str) -> Self {
        Self::new(component, Box::new(std::io::stderr()))
    }

    /// The rendered, bounded, redacted line for `event`.
    #[must_use]
    pub fn line(&self, event: DiagEvent) -> String {
        let full = format!("toolkit {}: {}", self.component, event.render());
        let bounded: String = full.chars().take(MAX_DIAG_CHARS).collect();
        redact_text(&bounded)
    }

    /// Write one event. A write failure is ignored: diagnostics never change
    /// the outcome.
    pub fn emit(&mut self, event: DiagEvent) {
        if event.level() < self.threshold {
            return;
        }
        let line = self.line(event);
        let _ = writeln!(self.sink, "{line}");
        let _ = self.sink.flush();
    }
}

impl std::fmt::Debug for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Diag")
            .field("component", &self.component)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::preflight::InvalidReason;

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn every_event_renders_allowlisted_fields_only() {
        let shared = Shared::default();
        let mut diag = Diag::new("mcp-proxy", Box::new(shared.clone()));
        for event in [
            DiagEvent::TlsRootErrorsTolerated { count: 3 },
            DiagEvent::CredentialFailure {
                refusal: CredentialRefusal::InvalidConfiguration(InvalidReason::PartialStaticKeys),
            },
            DiagEvent::CredentialFailure {
                refusal: CredentialRefusal::SignedSessionLostCredentials,
            },
            DiagEvent::TransportFailure {
                code: "aws_connect",
            },
            DiagEvent::HttpStatus { status: 503 },
            DiagEvent::Proxy(ProxyEvent::StdinLineTooLarge {
                id_recovered: false,
            }),
            DiagEvent::Proxy(ProxyEvent::MetaKeysKept { count: 2 }),
            DiagEvent::Proxy(ProxyEvent::UpstreamMessageMalformed),
        ] {
            diag.emit(event);
        }
        let written = String::from_utf8(shared.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            written,
            "toolkit mcp-proxy: tls-root-errors-tolerated count=3\n\
             toolkit mcp-proxy: credential-failure kind=invalid-configuration\n\
             toolkit mcp-proxy: credential-failure kind=signed-session-lost-credentials\n\
             toolkit mcp-proxy: transport-failure code=aws_connect\n\
             toolkit mcp-proxy: http-status status=503\n\
             toolkit mcp-proxy: stdin-line-too-large id=false\n\
             toolkit mcp-proxy: meta-keys-kept count=2\n\
             toolkit mcp-proxy: upstream-message-malformed\n"
        );
    }

    #[test]
    fn lines_are_bounded() {
        let diag = Diag::new("mcp-proxy", Box::new(Shared::default()));
        let line = diag.line(DiagEvent::HttpStatus { status: 200 });
        assert!(line.chars().count() <= MAX_DIAG_CHARS);
        assert!(!format!("{diag:?}").contains("sink"));
    }
}

#[cfg(test)]
mod level_tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_threshold_filters_by_severity() {
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("WARNING"), Some(Level::Warning));
        assert_eq!(Level::parse("WARN"), None);
        let shared = Shared::default();
        let mut diag =
            Diag::new("mcp-proxy", Box::new(shared.clone())).with_threshold(Level::Error);
        diag.emit(DiagEvent::HttpStatus { status: 503 });
        diag.emit(DiagEvent::Proxy(ProxyEvent::ReadOnlyToolRefused));
        diag.emit(DiagEvent::CredentialFailure {
            refusal: CredentialRefusal::InvalidConfiguration(
                InvalidReason::CredentialProcessNeedsNullStdin,
            ),
        });
        let text = String::from_utf8(shared.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            text,
            "toolkit mcp-proxy: credential-failure kind=credential-process-needs-null-stdin\n"
        );
        let mut silent =
            Diag::new("mcp-proxy", Box::new(shared.clone())).with_threshold(Level::Critical);
        silent.emit(DiagEvent::Proxy(ProxyEvent::StdoutClosed));
        assert_eq!(
            String::from_utf8(shared.0.lock().unwrap().clone()).unwrap(),
            text
        );
    }
}
