//! Caller-derived text in diagnostics: redacted, then truncated, then escaped
//! on one line, with the shared helpers (`toolkit_aws::redact_text` and
//! `toolkit_core::escape_untrusted`).

use toolkit_aws::redact_text;
use toolkit_core::escape_untrusted;

use crate::bedrock_resolve::source::redact;

/// Longest text, in characters, a verdict detail keeps before the
/// `...[N more chars]` marker.
pub(crate) const MAX_CHARS: usize = 300;

/// Longest caller path, in characters, a diagnostic keeps.
pub(crate) const MAX_PATH_CHARS: usize = 200;

/// `text` redacted, cut to `max_chars` characters and escaped on one line.
pub(crate) fn bounded(text: &str, max_chars: usize) -> String {
    escape_untrusted(&redact_text(text), max_chars)
}

/// [`bounded`] with the default bound of a verdict detail.
pub(crate) fn text(text: &str) -> String {
    bounded(text, MAX_CHARS)
}

/// [`bounded`] for text that may hold a known key value: the values are
/// replaced on the full raw text first (so no cut or escape can split one),
/// then the shared redactor, the cut and the escape run, then the values are
/// replaced again (an escape may have spelled one differently).
pub(crate) fn scrubbed(text: &str, secrets: &[String], max_chars: usize) -> String {
    redact(&bounded(&redact(text, secrets), max_chars), secrets)
}

/// A caller path as a diagnostic shows it: [`scrubbed`] and bounded to
/// [`MAX_PATH_CHARS`]. Only the rendering changes; the file operation itself
/// uses the path as given.
pub(crate) fn path(path: &str, secrets: &[String]) -> String {
    scrubbed(path, secrets, MAX_PATH_CHARS)
}
