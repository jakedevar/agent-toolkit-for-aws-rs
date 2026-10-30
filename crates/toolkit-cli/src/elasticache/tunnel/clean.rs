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
/// With any supplied or active credential, server values are withheld. An
/// allowlist cannot distinguish a version from an encoded credential: even
/// form-urlencoded spaces (`a+b`), base64 and hex can pass it (#5004).
/// Without credentials, only short allowlisted values are printed.
///
/// No credential is copied, encoded or scanned. Authenticated runs do not
/// inspect the server value at all; unauthenticated runs inspect at most 32
/// version bytes or compare against three fixed mode names (#5005).
pub(super) struct SecretGate {
    has_credentials: bool,
}

impl SecretGate {
    /// A gate for `secrets` (empty ones are ignored).
    pub(super) fn new(secrets: &[&str]) -> Self {
        Self {
            has_credentials: secrets.iter().any(|secret| !secret.is_empty()),
        }
    }

    /// The engine version, or `unknown`.
    pub(super) fn version<'a>(&self, version: &'a str) -> &'a str {
        if !self.has_credentials && is_version(version) {
            version
        } else {
            UNKNOWN
        }
    }

    /// The engine mode, or `unknown`.
    pub(super) fn mode<'a>(&self, mode: &'a str) -> &'a str {
        if !self.has_credentials && MODES.contains(&mode) {
            mode
        } else {
            UNKNOWN
        }
    }
}
