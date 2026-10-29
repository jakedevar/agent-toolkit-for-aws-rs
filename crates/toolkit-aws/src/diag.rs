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
//! Later slices (S2, S3) add variants here; each variant must keep to the
//! allowlisted field kinds.

use std::fmt::Write as _;
use std::io::Write;

use crate::preflight::CredentialRefusal;
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
        };
        line
    }
}

fn refusal_kind(refusal: CredentialRefusal) -> &'static str {
    match refusal {
        CredentialRefusal::InvalidConfiguration(_) => "invalid-configuration",
        CredentialRefusal::ProviderFailed => "provider-failed",
        CredentialRefusal::ConfiguredSourceNotLoaded => "configured-source-not-loaded",
        CredentialRefusal::NoCredentials => "no-credentials",
        CredentialRefusal::SignedSessionLostCredentials => "signed-session-lost-credentials",
    }
}

/// The diagnostics sink.
pub struct Diag {
    component: &'static str,
    sink: Box<dyn Write + Send>,
}

impl Diag {
    /// A sink writing `toolkit <component>: <event>` lines to `sink`.
    #[must_use]
    pub fn new(component: &'static str, sink: Box<dyn Write + Send>) -> Self {
        Self { component, sink }
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
             toolkit mcp-proxy: http-status status=503\n"
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
