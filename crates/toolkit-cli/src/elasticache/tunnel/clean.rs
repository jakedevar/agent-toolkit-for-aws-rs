//! Caller text in diagnostics: redact, then bound, then escape (in this order).
//!
//! Every value that reaches stdout or stderr and was not typed as a literal in
//! this source (an instance name from a response, the SSM process output, an
//! error message, a command-line value) goes through [`clean`]. The whole text
//! is redacted first, so a credential that crosses the bound is still
//! recognized; it is then cut to [`MAX_CHARS`] characters (with `...`), and
//! control, bidirectional and invisible characters are escaped last, so an
//! escape never grows past the bound it was cut for.

use toolkit_aws::redact_text;

use crate::safe_text::safe_path;

/// Longest cleaned text, in characters, before `...`.
pub(super) const MAX_CHARS: usize = 200;

/// `text` redacted, bounded and escaped.
pub(super) fn clean(text: &str) -> String {
    clean_to(text, MAX_CHARS)
}

/// [`clean`] with a bound of `limit` characters.
pub(super) fn clean_to(text: &str, limit: usize) -> String {
    let redacted = redact_text(text);
    let mut bounded: String = redacted.chars().take(limit).collect();
    if redacted.chars().nth(limit).is_some() {
        bounded.push_str("...");
    }
    safe_path(&bounded)
}
