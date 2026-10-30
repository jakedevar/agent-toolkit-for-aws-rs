//! The Gremlin WebSocket plane of the docgraph live adapter (R-DG-05, R-DG-07;
//! slice S3, #5515). Feature `gremlin`, enabled only by `docgraph-live`.
//!
//! [`GremlinConnection`] opens `wss://HOST:8182/gremlin` for one
//! [`DatabaseEndpoint`]:
//!
//! 1. the upgrade `GET https://HOST:8182/gremlin` (authority with its port,
//!    empty body) is signed with the toolkit [`Signer`], service `neptune-db`
//!    and the selected region, **before** any DNS or socket work, so missing
//!    credentials send nothing and an unsigned upgrade is never attempted;
//! 2. the connection comes from the database capability's pre-send guarded
//!    opener (S1, #5387): fresh resolution, every answer and the actual peer
//!    checked before any byte, rustls trusting exactly the [`ValidatedRoots`]
//!    and verifying the original host name;
//! 3. the WebSocket handshake carries the signed headers unchanged
//!    (`host`, `x-amz-date`, `x-amz-security-token`, `authorization`); the
//!    handshake layer adds `Connection`, `Upgrade` and `Sec-WebSocket-*`
//!    after signing, so they are not signed fields. It requires `101` and a
//!    valid `Sec-WebSocket-Accept`; any other answer (a 403 for a bad
//!    signature) fails before a statement is sent.
//!
//! [`GremlinConnection::submit`] sends one `eval` request (`GraphSON` v2,
//! traversal source `g`, bindings as data) with a fresh request id, and
//! collects every 206 batch through the final 200/204 for that id only. One
//! request is in flight; requests are sent in call order.
//!
//! Bounds (R-DG-07, owned by S3 per PM ruling 3 on #5366): frames above
//! [`MAX_FRAME_BYTES`] are refused from the frame header, before the payload
//! is buffered; messages and the bytes of one operation are capped at the
//! database response limit (16 MiB); `GraphSON` depth and bulk expansion are
//! bounded by [`MAX_DEPTH`] and [`MAX_RESULTS`] ([`graphson`]). The upgrade
//! and every statement run inside one absolute deadline (the operation
//! budget, capped by the caller's invocation deadline) with the idle bound
//! between frames.
//!
//! Failure: any failed statement spends the connection: its socket is dropped
//! at once and every later `submit` fails without sending, so a statement is
//! never retried or resubmitted and a plan stops at its first failure. A
//! failure after the request was handed to the socket says the outcome is
//! unknown. There is no reconnect; a new connection is signed afresh.
//!
//! Close: [`GremlinConnection::close`] sends a close frame within
//! [`CLOSE_BUDGET`] (capped by the invocation deadline) and drops the socket;
//! dropping the connection drops the socket synchronously, on every exit.
//! Diagnostics are fixed texts: no server message, frame, URL, header or
//! WebSocket/TLS error text is forwarded (R-DG-02).

mod graphson;
#[cfg(test)]
mod tests;

use std::fmt;
use std::fmt::Write as _;

use futures_util::{SinkExt, StreamExt};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{
    self, handshake::client::generate_key, http, protocol::WebSocketConfig, Message,
};
use tokio_tungstenite::WebSocketStream;
use toolkit_core::Json;

pub use graphson::{MAX_DEPTH, MAX_EXPANDED_BYTES, MAX_RESULTS, MIME};

use super::database::{DatabaseBounds, DatabaseEndpoint, SecureOpener, Stream, CLOSE_BUDGET};
use super::roots::ValidatedRoots;
use super::SdkRuntime;
use crate::error::{AwsError, TimeoutPhase};
use crate::transport::{Headers, HttpRequest, Method, Signer, SigningScope, Timeouts};

/// The Gremlin port of Neptune.
pub const PORT: u16 = 8182;
/// The Gremlin WebSocket path.
pub const PATH: &str = "/gremlin";
/// The signing name of the Neptune database plane.
pub const SERVICE: &str = "neptune-db";
/// The largest WebSocket frame accepted (R-DG-07).
pub const MAX_FRAME_BYTES: usize = 1 << 20;

type Socket = WebSocketStream<tokio_rustls::client::TlsStream<Box<dyn Stream>>>;

/// Why `submit` refuses on a spent connection.
const SPENT: &str =
    "the Gremlin connection ended after an earlier failure; no later statement is sent";

/// One signed, admitted Gremlin WebSocket connection.
pub struct GremlinConnection {
    runtime: SdkRuntime,
    socket: Option<Socket>,
    bounds: DatabaseBounds,
    invocation: Instant,
}

impl fmt::Debug for GremlinConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GremlinConnection")
    }
}

fn unknown_after_send(reason: &str) -> String {
    format!("{reason} after the statement was sent; whether the graph applied it is unknown")
}

/// A fresh random (version 4) request id from the TLS stack's generator.
fn request_id() -> Result<String, AwsError> {
    let mut bytes = [0_u8; 16];
    rustls::crypto::aws_lc_rs::default_provider()
        .secure_random
        .fill(&mut bytes)
        .map_err(|_| {
            AwsError::invalid_config("request_id", "no random request id could be generated")
        })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// The signed upgrade request: the signer's headers verbatim, then the
/// handshake headers, which the signer never sees.
fn upgrade_request(
    endpoint: &DatabaseEndpoint,
    signer: &dyn Signer,
    region: &str,
) -> Result<http::Request<()>, AwsError> {
    if endpoint.port() != PORT {
        return Err(AwsError::invalid_config(
            "endpoint",
            "the Gremlin endpoint port must be 8182",
        ));
    }
    if region != endpoint.region() {
        return Err(AwsError::invalid_config(
            "region",
            "the signing region differs from the network policy's region; nothing was signed or sent",
        ));
    }
    let authority = format!("{}:{}", endpoint.host(), endpoint.port());
    let mut upgrade = HttpRequest {
        method: Method::Get,
        url: format!("https://{authority}{PATH}"),
        headers: Headers::new(),
        body: Vec::new(),
        timeouts: Timeouts::default(),
    };
    signer.sign(
        &mut upgrade,
        &SigningScope {
            region: region.to_string(),
            service: SERVICE.to_string(),
        },
    )?;
    if upgrade.headers.get("authorization").is_none() {
        return Err(AwsError::invalid_config(
            "signing",
            "the Gremlin upgrade was not signed; nothing was sent",
        ));
    }
    let mut builder = http::Request::builder()
        .method("GET")
        .uri(format!("wss://{authority}{PATH}"))
        .header("Host", authority.as_str());
    for (name, value) in upgrade.headers.entries() {
        builder = builder.header(name, value);
    }
    builder
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .map_err(|_| {
            AwsError::invalid_config("request", "the Gremlin upgrade request could not be built")
        })
}

fn upgrade_error(error: &tungstenite::Error) -> AwsError {
    match error {
        tungstenite::Error::Http(response) => AwsError::connect(format!(
            "the Gremlin WebSocket upgrade was refused with HTTP {}; no statement was sent",
            response.status().as_u16()
        )),
        _ => AwsError::connect("the Gremlin WebSocket upgrade failed; no statement was sent"),
    }
}

fn read_error(error: &tungstenite::Error) -> AwsError {
    match error {
        tungstenite::Error::Capacity(_) => AwsError::invalid_response(
            "a Gremlin frame or message exceeds its size limit (1 MiB per frame)",
        ),
        tungstenite::Error::Protocol(_) | tungstenite::Error::Utf8(_) => {
            AwsError::invalid_response("the Gremlin connection broke the WebSocket protocol")
        }
        _ => AwsError::connect(unknown_after_send("the Gremlin connection failed")),
    }
}

/// One statement on `socket`: send, then collect its batches.
async fn exchange(
    socket: &mut Socket,
    payload: Vec<u8>,
    id: &str,
    deadline: Instant,
    bounds: DatabaseBounds,
) -> Result<Vec<Json>, AwsError> {
    let late = || {
        AwsError::timeout(
            TimeoutPhase::Deadline,
            unknown_after_send("the operation deadline elapsed"),
        )
    };
    match tokio::time::timeout_at(deadline, socket.send(Message::Binary(payload.into()))).await {
        Err(_) => return Err(late()),
        Ok(Err(error)) => return Err(read_error(&error)),
        Ok(Ok(())) => {}
    }
    let mut results = graphson::Results::default();
    let mut received: u64 = 0;
    loop {
        let bound = deadline.min(Instant::now() + bounds.idle);
        let message = match tokio::time::timeout_at(bound, socket.next()).await {
            Err(_) if Instant::now() >= deadline => return Err(late()),
            Err(_) => {
                return Err(AwsError::timeout(
                    TimeoutPhase::Read,
                    unknown_after_send("no Gremlin response arrived in time"),
                ))
            }
            Ok(None | Some(Ok(Message::Close(_)))) => {
                return Err(AwsError::connect(unknown_after_send(
                    "the Gremlin connection closed before the response completed",
                )))
            }
            Ok(Some(Err(error))) => return Err(read_error(&error)),
            Ok(Some(Ok(message))) => message,
        };
        let bytes: &[u8] = match &message {
            Message::Text(text) => text.as_bytes(),
            Message::Binary(bytes) => bytes,
            _ => continue,
        };
        received = received.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if received > bounds.max_response_bytes {
            return Err(AwsError::invalid_response(
                "the Gremlin response exceeds the 16 MiB limit of one operation",
            ));
        }
        if results.add_message(bytes, id)? == graphson::Status::Complete {
            return Ok(results.items);
        }
    }
}

impl GremlinConnection {
    /// Sign the upgrade, then open the connection through the system
    /// resolver and a TCP dial admitted by `endpoint` (default bounds).
    ///
    /// # Errors
    /// Signing and credential errors (nothing sent), the capability's
    /// refusals and timeouts, or a refused upgrade.
    pub fn connect_live(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        endpoint: DatabaseEndpoint,
        signer: &dyn Signer,
        region: &str,
        invocation_deadline: std::time::Instant,
    ) -> Result<Self, AwsError> {
        let opener = SecureOpener::system(roots, endpoint, DatabaseBounds::default())?;
        Self::open(
            runtime,
            &opener,
            MAX_FRAME_BYTES,
            signer,
            region,
            invocation_deadline,
        )
    }

    pub(crate) fn open(
        runtime: &SdkRuntime,
        opener: &SecureOpener,
        max_frame_bytes: usize,
        signer: &dyn Signer,
        region: &str,
        invocation_deadline: std::time::Instant,
    ) -> Result<Self, AwsError> {
        let bounds = opener.bounds();
        let invocation = Instant::from_std(invocation_deadline);
        let request = upgrade_request(opener.endpoint(), signer, region)?;
        let config = WebSocketConfig::default()
            .max_frame_size(Some(max_frame_bytes))
            .max_message_size(Some(
                usize::try_from(bounds.max_response_bytes).unwrap_or(usize::MAX),
            ));
        let socket = runtime.block_on(async {
            let deadline = (Instant::now() + bounds.operation).min(invocation);
            let upgrade = async {
                let tls = opener.open(deadline).await?;
                let bound = deadline.min(Instant::now() + bounds.connect);
                match tokio::time::timeout_at(
                    bound,
                    tokio_tungstenite::client_async_with_config(request, tls, Some(config)),
                )
                .await
                {
                    Err(_) => Err(AwsError::timeout(
                        TimeoutPhase::Connect,
                        "the Gremlin WebSocket upgrade did not finish in time; no statement was sent",
                    )),
                    Ok(Err(error)) => Err(upgrade_error(&error)),
                    Ok(Ok((socket, _))) => Ok(socket),
                }
            };
            tokio::time::timeout_at(deadline, upgrade)
                .await
                .unwrap_or_else(|_| {
                    Err(AwsError::timeout(
                        TimeoutPhase::Deadline,
                        "the operation deadline elapsed before any statement was sent",
                    ))
                })
        })?;
        Ok(Self {
            runtime: runtime.clone(),
            socket: Some(socket),
            bounds,
            invocation,
        })
    }

    /// Whether the connection can still send (no statement has failed).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.socket.is_some()
    }

    /// Send one statement and return its decoded results.
    ///
    /// # Errors
    /// The statement's failure; afterwards the connection is spent and every
    /// later call fails without sending.
    pub fn submit(
        &mut self,
        gremlin: &str,
        bindings: Option<&Json>,
    ) -> Result<Vec<Json>, AwsError> {
        let Some(socket) = self.socket.as_mut() else {
            return Err(AwsError::connect(SPENT));
        };
        let outcome = match request_id() {
            Ok(id) => {
                let payload = graphson::request_payload(&id, gremlin, bindings);
                let bounds = self.bounds;
                let invocation = self.invocation;
                self.runtime.block_on(async {
                    let deadline = (Instant::now() + bounds.operation).min(invocation);
                    exchange(socket, payload, &id, deadline, bounds).await
                })
            }
            Err(error) => Err(error),
        };
        if outcome.is_err() {
            // Spent: drop the socket now, so nothing more is sent on it.
            self.socket = None;
        }
        outcome
    }

    /// Send a close frame within the close budget, then drop the socket.
    pub fn close(mut self) {
        if let Some(mut socket) = self.socket.take() {
            let invocation = self.invocation;
            self.runtime.block_on(async {
                let bound = (Instant::now() + CLOSE_BUDGET).min(invocation);
                let _ = tokio::time::timeout_at(bound, socket.close(None)).await;
            });
        }
    }
}
