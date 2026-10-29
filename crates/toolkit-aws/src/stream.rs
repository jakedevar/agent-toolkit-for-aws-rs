//! Streaming response seam (#4413 S1).
//!
//! [`crate::Transport`] reads whole bodies. An MCP Streamable HTTP response can
//! be a long Server-Sent Events stream, so [`StreamingTransport::open`] returns
//! the status and headers at once and the body as a [`ResponseStream`] of
//! incremental chunks:
//!
//! - **Idle-read timeout.** Each wait for the next bytes is bounded by the
//!   request's read timeout ([`crate::Timeouts::read`]);
//!   [`StreamError::IdleTimeout`] when it passes.
//! - **Body cap.** At most [`StreamLimits::max_body_bytes`] are handed over in
//!   total; one byte more is [`StreamError::TooLarge`] and the stream stays
//!   refused. Nothing past the cap is returned.
//! - **R-MEM reservation hook.** The stream never reads ahead: it pulls the
//!   next frame from the connection only inside [`ResponseStream::next_chunk`],
//!   and no chunk is longer than [`StreamLimits::max_chunk_bytes`]. A caller
//!   reserves `max_chunk_bytes` from its byte budget *before* asking for a
//!   chunk (and releases the unused part after), and while it waits for budget
//!   it does not ask, so the socket is not read and TCP flow control pushes
//!   back on the server.
//!
//! The SDK binding implements it on `SdkTransport` with the same https,
//! public-address and framing-header guards as `Transport::send`; tests use
//! [`crate::testing::FakeStreamingTransport`].

use std::fmt;

use crate::error::AwsError;
use crate::transport::{Headers, HttpRequest};

/// Bounds for one streamed response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    max_body_bytes: u64,
    max_chunk_bytes: usize,
}

impl StreamLimits {
    /// Validated limits.
    ///
    /// # Errors
    /// `InvalidConfig` (field `stream_limits`) when either bound is zero.
    pub fn new(max_body_bytes: u64, max_chunk_bytes: usize) -> Result<Self, AwsError> {
        if max_body_bytes == 0 || max_chunk_bytes == 0 {
            return Err(AwsError::invalid_config(
                "stream_limits",
                "stream limits must be positive",
            ));
        }
        Ok(Self {
            max_body_bytes,
            max_chunk_bytes,
        })
    }

    /// Most body bytes handed over in total.
    #[must_use]
    pub fn max_body_bytes(&self) -> u64 {
        self.max_body_bytes
    }

    /// Longest single chunk handed over.
    #[must_use]
    pub fn max_chunk_bytes(&self) -> usize {
        self.max_chunk_bytes
    }
}

/// Why a stream stopped early. Typed, so callers never classify by text.
#[derive(Clone, PartialEq, Eq)]
pub enum StreamError {
    /// The body passed [`StreamLimits::max_body_bytes`].
    TooLarge,
    /// No bytes arrived within the read timeout.
    IdleTimeout,
    /// The connection failed while reading (a redacted boundary error).
    Failed(AwsError),
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("the response body exceeded its size limit"),
            Self::IdleTimeout => f.write_str("no response bytes arrived within the read timeout"),
            Self::Failed(error) => write!(f, "{error}"),
        }
    }
}

impl fmt::Debug for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("TooLarge"),
            Self::IdleTimeout => f.write_str("IdleTimeout"),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

impl std::error::Error for StreamError {}

/// An open response: status and headers now, the body chunk by chunk.
pub trait ResponseStream: Send {
    /// HTTP status code.
    fn status(&self) -> u16;

    /// Response headers.
    fn headers(&self) -> &Headers;

    /// The next body chunk (at most [`StreamLimits::max_chunk_bytes`]), or
    /// `None` at the end of the body. Reads from the connection only when the
    /// limiter has no bytes left over from an earlier frame.
    ///
    /// # Errors
    /// [`StreamError`]; after [`StreamError::TooLarge`] every call returns it.
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError>;
}

/// Sends one request and returns its response as a stream.
pub trait StreamingTransport {
    /// Send `request` and return once the response headers are in.
    ///
    /// # Errors
    /// The same boundary errors as [`crate::Transport::send`] for connecting,
    /// the guards and the response head.
    fn open<'a>(
        &'a self,
        request: &HttpRequest,
        limits: &StreamLimits,
    ) -> Result<Box<dyn ResponseStream + 'a>, AwsError>;
}

/// Applies [`StreamLimits`] to a source of raw frames: splits frames to the
/// chunk bound and enforces the body cap. Shared by every implementation.
#[derive(Debug)]
pub struct ChunkLimiter {
    limits: StreamLimits,
    delivered: u64,
    pending: Vec<u8>,
    offset: usize,
    refused: bool,
}

impl ChunkLimiter {
    /// A limiter that has handed nothing over.
    #[must_use]
    pub fn new(limits: StreamLimits) -> Self {
        Self {
            limits,
            delivered: 0,
            pending: Vec::new(),
            offset: 0,
            refused: false,
        }
    }

    /// The next chunk: from the remainder of the last frame, else from one
    /// call of `pull` (which yields the next raw frame or `None` at the end).
    ///
    /// # Errors
    /// [`StreamError::TooLarge`] once the cap is passed (and on every later
    /// call), or whatever `pull` returns.
    pub fn next(
        &mut self,
        mut pull: impl FnMut() -> Result<Option<Vec<u8>>, StreamError>,
    ) -> Result<Option<Vec<u8>>, StreamError> {
        if self.refused {
            return Err(StreamError::TooLarge);
        }
        while self.offset >= self.pending.len() {
            match pull()? {
                None => return Ok(None),
                Some(frame) => {
                    let total = self
                        .delivered
                        .saturating_add(u64::try_from(frame.len()).unwrap_or(u64::MAX));
                    if total > self.limits.max_body_bytes {
                        self.refused = true;
                        self.pending = Vec::new();
                        self.offset = 0;
                        return Err(StreamError::TooLarge);
                    }
                    self.delivered = total;
                    self.pending = frame;
                    self.offset = 0;
                }
            }
        }
        let end = self
            .pending
            .len()
            .min(self.offset + self.limits.max_chunk_bytes);
        let chunk = self.pending[self.offset..end].to_vec();
        self.offset = end;
        Ok(Some(chunk))
    }
}

#[cfg(test)]
mod tests;
