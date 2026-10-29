//! The final output sink of every launch route (R-TOK-RED, R-API-CODE
//! second layer, #5406).
//!
//! [`guard`] arms the route's [`Output`] with the stored session's access
//! token, refresh token and client secret, read locally (no refresh, no
//! network) before the route can render anything. From then on no stdout
//! line or diagnostic that carries one of them is written: the caller gets
//! `{"error": GENERIC_FAILURE}` once instead, with exit 1
//! ([`Output::refuse_known`]). A missing or unreadable session record is an
//! empty set, never a failure, so the sink changes no exit behaviour of a
//! clean route.
//!
//! The check is structural: it does not depend on which early or late branch
//! produced the text (an invalid `--region`, `LAUNCH_WITH_AWS_REGION` or
//! `LAUNCH_WITH_AWS_BASE_URL`, a repository URL, a service error), so pattern
//! redaction of caller text (`safe_text`) is no longer the only defence
//! against an opaque live secret a caller passed back in. The values a
//! command learns later (a refreshed token, clientTokens, the presigned
//! upload URL) are scrubbed by [`super::flow::run`] with the same [`Secrets`].

use std::path::Path;

use toolkit_aws::{ProcessEnv, Secret};
use toolkit_core::Json;

use super::state;
use crate::Output;

/// The Python catch-all message (`main`, R-API-ERR last row).
pub const GENERIC_FAILURE: &str = "The operation failed. Re-run with logging enabled for details.";

/// Arm the known-secret sink of one launch route on `output` with the session
/// record of the process state directory (`$HOME/.launch-with-aws`). Every
/// launch route calls this first, before it can render anything.
pub fn guard(output: &mut Output<'_>) {
    let dir = state::state_dir(&ProcessEnv).ok();
    guard_in(dir.as_deref(), output);
}

/// [`guard`] with an explicit state directory (`None`: no session, so an
/// empty set and no change).
pub fn guard_in(dir: Option<&Path>, output: &mut Output<'_>) {
    let mut secrets = Secrets::default();
    secrets.add_stored(dir);
    output.refuse_known(secrets.0, super::text::error_text(GENERIC_FAILURE));
}

/// The live secret values of one command (R-API-CODE second layer): the
/// access token, the session's refresh token and client secret (before and
/// after a refresh), each clientToken and the presigned upload URL with each
/// of its query values (raw and percent-decoded). Values shorter than 8
/// bytes are ignored.
#[derive(Default)]
pub struct Secrets(Vec<String>);

impl Secrets {
    /// Register one value.
    pub fn add(&mut self, value: &str) {
        if value.len() >= 8 && !self.0.iter().any(|v| v == value) {
            self.0.push(value.to_owned());
        }
    }

    fn add_secret(&mut self, value: &Secret) {
        self.add(value.expose());
    }

    /// Register a stored session's secrets.
    pub fn add_session(&mut self, session: Option<&state::StoredSession>) {
        if let Some(session) = session {
            self.add_secret(&session.access_token);
            self.add_secret(&session.refresh_token);
            self.add_secret(&session.client_secret);
        }
    }

    /// Register the secrets of the session stored in `dir`: a local read
    /// only; a missing or unreadable record adds nothing.
    pub fn add_stored(&mut self, dir: Option<&Path>) {
        if let Some(dir) = dir {
            self.add_session(state::load_session(dir).as_ref());
        }
    }

    /// Register a presigned URL, its query string and each query value, raw
    /// and percent-decoded.
    pub fn add_url(&mut self, url: &str) {
        self.add(url);
        if let Some((_, query)) = url.split_once('?') {
            let query = query.split('#').next().unwrap_or_default();
            self.add(query);
            for part in query.split('&') {
                if let Some((_, value)) = part.split_once('=') {
                    self.add(value);
                    if let Some(decoded) = percent_decode(value) {
                        self.add(&decoded);
                    }
                }
            }
        }
    }

    /// Whether `text` contains any registered secret.
    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        self.0.iter().any(|secret| text.contains(secret.as_str()))
    }

    /// `message`, or the generic message when it carries a secret.
    #[must_use]
    pub fn scrub(&self, message: String) -> String {
        if self.contains(&message) {
            GENERIC_FAILURE.to_owned()
        } else {
            message
        }
    }

    /// Whether any key or string anywhere in `value` contains a secret.
    #[must_use]
    // recorded-shape: a recursive scan of every key and string the command built for output; it interprets no response member and accepts every shape.
    pub fn found_in(&self, value: &Json) -> bool {
        match value {
            Json::String(text) => self.contains(text),
            Json::Array(items) => items.iter().any(|v| self.found_in(v)),
            Json::Object(members) => members
                .iter()
                .any(|(k, v)| self.contains(k) || self.found_in(v)),
            _ => false,
        }
    }
}

/// `%XX` decoding (and `+` as space) of a query value; `None` when nothing
/// changes or the result is not UTF-8.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    (decoded != value).then_some(decoded)
}

#[cfg(test)]
#[path = "sink/tests_sink.rs"]
mod tests_sink;
