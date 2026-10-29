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
//!
//! Each value is registered in every form launch code itself renders a value
//! into output text (#5426): raw; with the escaping of `text::repr` (either
//! quote) and `text::shown`; percent-encoded and form-encoded with upper- and
//! lower-case hex; and each of those inside a JSON string, as `pyfmt::dumps`
//! (`ensure_ascii`) and `serde_json` write it. So an error whose JSON escaping
//! changed a secret's quote or backslash, or a URL that carries it
//! percent-encoded, is refused like the raw secret. Arbitrary third-party
//! transforms are out of scope.

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

/// One way launch code escapes or encodes a value into output text (#5426).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Escape {
    /// The value as it is.
    Raw,
    /// `text::shown`: `text::repr` escaping without a quote.
    Shown,
    /// `text::repr` inside `'` quotes.
    ReprSingle,
    /// `text::repr` inside `"` quotes.
    ReprDouble,
    /// RFC 3986 percent-encoding (`toolkit_aws::transport::percent_encode`),
    /// upper-case hex.
    PercentUpper,
    /// Percent-encoding with lower-case hex.
    PercentLower,
    /// Form encoding (`+` for space, as the sign-in token request), upper-case
    /// hex.
    FormUpper,
    /// Form encoding with lower-case hex.
    FormLower,
}

/// Every [`Escape`].
pub(crate) const ESCAPES: [Escape; 8] = [
    Escape::Raw,
    Escape::Shown,
    Escape::ReprSingle,
    Escape::ReprDouble,
    Escape::PercentUpper,
    Escape::PercentLower,
    Escape::FormUpper,
    Escape::FormLower,
];

impl Escape {
    pub(crate) fn render(self, value: &str) -> String {
        match self {
            Self::Raw => value.to_owned(),
            Self::Shown => super::text::escaped(value, None),
            Self::ReprSingle => super::text::escaped(value, Some('\'')),
            Self::ReprDouble => super::text::escaped(value, Some('"')),
            Self::PercentUpper => url_encode(value, true, false),
            Self::PercentLower => url_encode(value, false, false),
            Self::FormUpper => url_encode(value, true, true),
            Self::FormLower => url_encode(value, false, true),
        }
    }
}

/// The JSON string an escaped value may be written inside (#5426).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    /// Not inside a JSON string.
    Bare,
    /// A launch output line: Python `json.dumps` (`ensure_ascii`), as
    /// `text::error_text` and `text::ok_text` write it.
    PyJson,
    /// `serde_json` (non-ASCII written as it is).
    SerdeJson,
}

/// Every [`Layer`].
pub(crate) const LAYERS: [Layer; 3] = [Layer::Bare, Layer::PyJson, Layer::SerdeJson];

impl Layer {
    pub(crate) fn render(self, value: &str) -> String {
        let quoted = match self {
            Self::Bare => return value.to_owned(),
            Self::PyJson => crate::aurora::pyfmt::dumps(&Json::String(value.to_owned())),
            Self::SerdeJson => serde_json::to_string(value).unwrap_or_default(),
        };
        quoted
            .strip_prefix('"')
            .and_then(|q| q.strip_suffix('"'))
            .unwrap_or(&quoted)
            .to_owned()
    }
}

/// Percent-encoding of every byte but the RFC 3986 unreserved characters,
/// with upper- or lower-case hex; `form` writes a space as `+`.
fn url_encode(value: &str, upper: bool, form: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else if form && byte == b' ' {
            out.push('+');
        } else if upper {
            let _ = write!(out, "%{byte:02X}");
        } else {
            let _ = write!(out, "%{byte:02x}");
        }
    }
    out
}

/// The live secret values of one command (R-API-CODE second layer): the
/// access token, the session's refresh token and client secret (before and
/// after a refresh), each clientToken and the presigned upload URL with each
/// of its query values (raw and percent-decoded). Each is held in every
/// rendered form (each [`Escape`] inside each [`Layer`], #5426). Values
/// shorter than 8 bytes are ignored.
#[derive(Default)]
pub struct Secrets(Vec<String>);

impl Secrets {
    /// Register one value in every rendered form.
    pub fn add(&mut self, value: &str) {
        self.add_forms(value, &ESCAPES, &LAYERS);
    }

    /// Register `value` in each of `escapes` inside each of `layers` (tests
    /// leave one out to show it is needed).
    pub(crate) fn add_forms(&mut self, value: &str, escapes: &[Escape], layers: &[Layer]) {
        if value.len() < 8 {
            return;
        }
        for escape in escapes {
            let escaped = escape.render(value);
            for layer in layers {
                let form = layer.render(&escaped);
                if !self.0.contains(&form) {
                    self.0.push(form);
                }
            }
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

    /// Whether `text` contains any registered secret, in any rendered form.
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
