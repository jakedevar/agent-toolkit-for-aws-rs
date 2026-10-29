//! `toolkit mcp-proxy` (#4413): a stdio ↔ MCP Streamable HTTP bridge. S2 is
//! the protocol core below; S3 adds the command ([`command`]: arguments,
//! [`toolkit_aws::McpTarget`], the start-up order, the S1 wiring and the
//! read-only filter).
//!
//! The accepted design is `docs/migration/mcp-proxy-design.md` (sections 2,
//! 4.5 and 4.6). This module owns:
//!
//! - **Framing.** The client side is newline-delimited JSON-RPC. Every
//!   message from upstream, whether an `application/json` body or one SSE
//!   event ([`sse`]), is parsed with `parse_json_with_limits` and
//!   re-serialised compactly before it reaches stdout, so stdout always
//!   carries exactly one valid JSON-RPC object or batch per line. Raw
//!   U+2028/U+2029 are written as escapes. Non-JSON data, and anything that
//!   is not an object or a non-empty batch of objects, is dropped with a
//!   diagnostic. Upstream messages are parsed on the request's own worker
//!   thread, never on the stdin reader or the stdout writer.
//! - **R-MEM.** One shared byte [`Budget`] covers stdin line buffers,
//!   upstream bodies and SSE buffers, the parse charge, output lines and
//!   queued stdout lines, through RAII [`Reservation`]s ([`budget`]). A
//!   separate control reserve holds proxy-generated control lines.
//! - **The writer.** One thread writes stdout: whole lines, the control
//!   queue first at every line boundary, then the data queue. Both queues
//!   are bounded ([`Limits`]).
//! - **Sessions.** `Mcp-Session-Id` is captured from the `initialize`
//!   response (validated: 1-256 visible ASCII bytes) together with the
//!   negotiated `protocolVersion` (validated `YYYY-MM-DD`), and both are sent
//!   on later requests. After `notifications/initialized` the optional `GET`
//!   stream is opened (a 405 is tolerated silently; reopened at most once per
//!   30 s). stdin EOF sends exactly one `DELETE`.
//! - **`_meta` injection** of the configured metadata into `params._meta`,
//!   upstream keys winning ([`meta`]).
//! - **Concurrency.** At most 16 requests in flight; the 17th is refused.
//! - **Cancellation.** A client `notifications/cancelled` for an in-flight
//!   id aborts the local task (its reservations are released, its response
//!   is dropped) and is forwarded upstream.
//! - **Errors.** Fixed templates only ([`templates`]): no upstream body, no
//!   credential and no error `Display` ever reaches stdout or stderr.
//!
//! Credentials stay behind the [`Auth`] seam (S3 wires the guarded SDK
//! sources into it); the transport is any
//! [`toolkit_aws::StreamingTransport`]. Diagnostics go only through the
//! allowlisted [`toolkit_aws::diag::Diag`] (R-LOG).

pub mod budget;
pub mod cli;
pub mod clock;
pub mod command;
pub mod scope;
pub mod sse;

mod auth;
mod frame;
mod limits;
mod meta;
mod output;
mod proxy;
mod session;
mod templates;

pub use auth::{Auth, CredentialSource, Resolved, SignWith, SignerAuth, SourceAuth, Unsigned};
pub use budget::{Budget, Reservation};
pub use clock::{Clock, FakeClock, SystemClock};
pub use limits::{Limits, LimitsError, LimitsSpec, MAX_CONTROL_LINE, MAX_ID_BYTES, NODE_CHARGE};
pub use proxy::{Exit, Proxy, ProxyConfig};

#[cfg(test)]
pub(crate) use frame::parse_threads;

#[cfg(test)]
mod tests;
