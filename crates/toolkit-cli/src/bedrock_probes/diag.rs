//! Caller-derived text in diagnostics: redacted, then truncated, then escaped
//! on one line, with the shared helpers (`toolkit_aws::redact_text` and
//! `toolkit_core::escape_untrusted`).

use toolkit_aws::redact_text;
use toolkit_core::escape_untrusted;

/// Longest text, in characters, a verdict detail keeps before the
/// `...[N more chars]` marker.
pub(crate) const MAX_CHARS: usize = 300;

/// `text` redacted, cut to `max_chars` characters and escaped on one line.
pub(crate) fn bounded(text: &str, max_chars: usize) -> String {
    escape_untrusted(&redact_text(text), max_chars)
}

/// [`bounded`] with the default bound of a verdict detail.
pub(crate) fn text(text: &str) -> String {
    bounded(text, MAX_CHARS)
}
