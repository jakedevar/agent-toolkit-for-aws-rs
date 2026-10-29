//! Caller text in diagnostics: redact, then bound, then escape (in this order).
//!
//! Every value that reaches stdout or stderr and was not typed as a literal in
//! this source (an instance name from a response, the SSM process output, an
//! error message, a command-line value) goes through [`clean`], which is the
//! shared `safe_text::safe_path` pipeline (#4941; integration round 20, #5019).
//! The whole text is redacted first, so a credential that crosses the bound is
//! still recognized; it is then cut to `safe_text::MAX_PATH_CHARS` (200)
//! characters (with `...`), and control, bidirectional and invisible
//! characters are escaped last, so an escape never grows past the bound it was
//! cut for.

use crate::safe_text::safe_path;

/// `text` redacted, bounded and escaped (the shared [`safe_path`]).
pub(super) fn clean(text: &str) -> String {
    safe_path(text)
}

/// Longest server value (or secret) the containment check looks at, in bytes.
/// A longer value is refused before any scan (fail closed).
pub(super) const GATE_MAX_BYTES: usize = 1024;

/// Printed instead of a server value that fails the allowlist or the gate.
pub(super) const UNKNOWN: &str = "unknown";

/// The engine modes of `INFO server` that are printed (`redis_mode`).
const MODES: [&str; 3] = ["standalone", "cluster", "sentinel"];

/// `^[0-9A-Za-z][0-9A-Za-z.+-]{0,31}$`: a version such as `7.1.0`.
fn is_version(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

/// The gate for text a cache server reports (PM ruling for #5004, #5005 and
/// #5007).
///
/// No client or server string is echoed. A server value is printed only when
/// it matches a strict allowlist *and* holds none of the active secrets
/// (password, IAM token) as a substring; otherwise `unknown` is printed. The
/// containment check is one exact-value pass, bounded: a value above
/// [`GATE_MAX_BYTES`] is refused unread, and a secret longer than the value
/// cannot occur in it, so no scan runs for it.
pub(super) struct SecretGate {
    secrets: Vec<String>,
}

impl SecretGate {
    /// A gate for `secrets` (empty ones are ignored).
    pub(super) fn new(secrets: &[&str]) -> Self {
        Self {
            secrets: secrets
                .iter()
                .filter(|secret| !secret.is_empty())
                .map(|secret| (*secret).to_string())
                .collect(),
        }
    }

    /// Whether `value` holds a secret, or is too long to be checked (fail
    /// closed).
    fn holds_secret(&self, value: &str) -> bool {
        if value.len() > GATE_MAX_BYTES {
            return true;
        }
        self.secrets
            .iter()
            .any(|secret| secret.len() <= value.len() && value.contains(secret.as_str()))
    }

    /// The engine version, or `unknown`.
    pub(super) fn version<'a>(&self, version: &'a str) -> &'a str {
        if is_version(version) && !self.holds_secret(version) {
            version
        } else {
            UNKNOWN
        }
    }

    /// The engine mode, or `unknown`.
    pub(super) fn mode<'a>(&self, mode: &'a str) -> &'a str {
        if MODES.contains(&mode) && !self.holds_secret(mode) {
            mode
        } else {
            UNKNOWN
        }
    }
}
