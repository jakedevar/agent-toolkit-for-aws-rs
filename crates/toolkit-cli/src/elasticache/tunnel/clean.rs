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

/// Removes known credentials from text a cache client hands back (#4986).
///
/// The generic [`redact_text`] cannot recognize an arbitrary, unlabelled
/// password, so the active password and IAM token are removed by value first,
/// in their common renderings: raw, quoted, escaped for JSON or a shell,
/// percent-encoded, hex and base64 (plain, URL-safe, with or without padding,
/// and as the `user:secret` of Basic auth). Only then is the text redacted,
/// bounded and escaped, so a bound can never cut a credential in two and an
/// escape can never hide one.
pub(super) struct Scrubber {
    /// Needles, longest first.
    needles: Vec<String>,
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8], url_safe: bool, pad: bool) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..=chunk.len() {
            let value = ((n >> (18 - 6 * index)) & 63) as usize;
            let symbol = char::from(BASE64[value]);
            out.push(match (url_safe, symbol) {
                (true, '+') => '-',
                (true, '/') => '_',
                _ => symbol,
            });
        }
        if pad {
            for _ in chunk.len()..3 {
                out.push('=');
            }
        }
    }
    out
}

fn percent(text: &str, upper: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else if upper {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push_str(&format!("%{byte:02x}"));
        }
    }
    out
}

fn renderings(secret: &str, out: &mut Vec<String>) {
    out.push(secret.to_string());
    let quoted = format!("{secret:?}");
    out.push(quoted[1..quoted.len() - 1].to_string());
    out.push(secret.escape_default().to_string());
    out.push(toolkit_core::escape_json_string(secret));
    out.push(secret.replace('\'', "'\\''"));
    out.push(percent(secret, true));
    out.push(percent(secret, false));
    out.push(secret.replace(' ', "+"));
    let hex: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();
    out.push(hex.to_uppercase());
    out.push(hex);
    for url_safe in [false, true] {
        for pad in [true, false] {
            out.push(base64(secret.as_bytes(), url_safe, pad));
        }
    }
}

impl Scrubber {
    /// A scrubber for `secrets` (empty ones are ignored) and, for the Basic
    /// auth rendering, the user name they go with.
    pub(super) fn new(secrets: &[&str], user: Option<&str>) -> Self {
        let mut needles = Vec::new();
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            renderings(secret, &mut needles);
            if let Some(user) = user {
                let basic = format!("{user}:{secret}");
                for url_safe in [false, true] {
                    for pad in [true, false] {
                        needles.push(base64(basic.as_bytes(), url_safe, pad));
                    }
                }
            }
            // The values of a pre-signed IAM token that identify the caller.
            for (_, value) in secret
                .split(['?', '&'])
                .filter_map(|part| part.split_once('='))
                .filter(|(key, value)| {
                    let key = key.to_ascii_lowercase();
                    !value.is_empty()
                        && [
                            "x-amz-signature",
                            "x-amz-credential",
                            "x-amz-security-token",
                        ]
                        .contains(&key.as_str())
                })
            {
                renderings(value, &mut needles);
            }
        }
        needles.retain(|needle| !needle.is_empty());
        needles.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        needles.dedup();
        Self { needles }
    }

    /// `text` with every known credential replaced by `[REDACTED]`.
    pub(super) fn remove(&self, text: &str) -> String {
        if self.needles.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        'scan: while let Some(first) = rest.chars().next() {
            for needle in &self.needles {
                if rest.starts_with(needle.as_str()) {
                    out.push_str(toolkit_aws::REDACTED);
                    rest = &rest[needle.len()..];
                    continue 'scan;
                }
            }
            out.push(first);
            rest = &rest[first.len_utf8()..];
        }
        out
    }

    /// Credentials removed, then [`clean`].
    pub(super) fn clean(&self, text: &str) -> String {
        clean(&self.remove(text))
    }
}
