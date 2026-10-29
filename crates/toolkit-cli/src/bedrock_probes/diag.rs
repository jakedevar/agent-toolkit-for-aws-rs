//! Caller-derived text in diagnostics: redacted, then truncated, then escaped
//! on one line, with the shared helpers (`toolkit_aws::redact_text` and
//! `toolkit_core::escape_untrusted`; caller paths through the shared
//! `safe_text::safe_path`, #4941).

use toolkit_aws::redact_text;
use toolkit_core::escape_untrusted;

use crate::bedrock_resolve::source::redact;
use crate::safe_text::safe_path;

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

/// [`bounded`] for text that may hold a known key value: the values are
/// replaced on the full raw text first (so no cut or escape can split one),
/// then the shared redactor, the cut and the escape run, then the values are
/// replaced again (an escape may have spelled one differently).
pub(crate) fn scrubbed(text: &str, secrets: &[String], max_chars: usize) -> String {
    redact(&bounded(&redact(text, secrets), max_chars), secrets)
}

/// A caller path as a diagnostic shows it, on the shared #4941 pipeline
/// (integration round 20, #5019): the known key values are replaced on the
/// full raw path first (#4939: no cut or escape runs before this scrub, so
/// none can split a value), then [`safe_path`] redacts it with the shared
/// redactor, bounds it to its 200 characters (then `...`) and escapes it, then
/// the values are replaced again (an escape may have spelled one
/// differently). Only the rendering changes; the file operation itself uses
/// the path as given.
pub(crate) fn path(path: &str, secrets: &[String]) -> String {
    redact(&safe_path(&redact(path, secrets)), secrets)
}
