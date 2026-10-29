//! The protocol loop (stub for the red run; implemented next).

use std::io::{Read, Write};
use std::sync::Arc;

use toolkit_aws::diag::Diag;
use toolkit_aws::{Headers, StreamingTransport, Timeouts};

use super::auth::Auth;
use super::budget::Budget;
use super::clock::Clock;
use super::limits::Limits;

/// What the proxy is configured with (S3 builds it from the command line).
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// The validated endpoint URL (S3's `McpTarget`).
    pub url: String,
    /// Extra `--header` values, signed and sent on every request.
    pub headers: Headers,
    /// `--metadata` pairs injected into `params._meta`.
    pub metadata: Vec<(String, String)>,
    /// Connect and idle-read timeouts.
    pub timeouts: Timeouts,
    /// `--retries`: resends after a connect failure, a 5xx or a 429.
    pub retries: u32,
    /// The bounds.
    pub limits: Limits,
    /// The `User-Agent` (S3's Q3 policy); none by default.
    pub user_agent: Option<String>,
}

impl ProxyConfig {
    /// Defaults for `url`.
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            headers: Headers::new(),
            metadata: Vec::new(),
            timeouts: Timeouts::default(),
            retries: 0,
            limits: Limits::default(),
            user_agent: None,
        }
    }
}

/// Why [`Proxy::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// stdin reached end of input.
    StdinClosed,
    /// stdout could not be written.
    StdoutClosed,
}

/// The proxy.
pub struct Proxy {
    budget: Budget,
    control: Budget,
}

impl Proxy {
    /// A proxy over `transport`, signing through `auth`.
    #[must_use]
    pub fn new(
        config: ProxyConfig,
        transport: Arc<dyn StreamingTransport + Send + Sync>,
        auth: Arc<dyn Auth>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let _ = (transport, auth, clock);
        Self {
            budget: Budget::new(config.limits.budget()),
            control: Budget::new(config.limits.control_reserve()),
        }
    }

    /// The shared byte budget.
    #[must_use]
    pub fn budget(&self) -> Budget {
        self.budget.clone()
    }

    /// The control reserve.
    #[must_use]
    pub fn control_budget(&self) -> Budget {
        self.control.clone()
    }

    /// Run until stdin ends or stdout fails.
    pub fn run(self, stdin: Box<dyn Read + Send>, stdout: Box<dyn Write + Send>, diag: Diag) -> Exit {
        let _ = (stdin, stdout, diag);
        todo!("S2 protocol loop")
    }
}
