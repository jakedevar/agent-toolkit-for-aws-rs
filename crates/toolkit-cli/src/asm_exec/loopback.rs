//! Plain-HTTP [`Transport`] for the local Secrets Manager Agent.
//!
//! The agent listens on `http://localhost:2773`; this is the only plain-HTTP
//! destination `asm exec` ever uses, and it refuses everything but a loopback
//! address: the literal `localhost`, an address in `127.0.0.0/8` or `[::1]`.
//! Every resolved address is checked again before connecting. Redirects are not
//! followed. A response is read under one deadline and a size cap.
//!
//! The request carries the agent's SSRF token header, so the caller must have
//! validated the endpoint with [`LoopbackEndpoint::parse`] first.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use toolkit_aws::error::TimeoutPhase;
use toolkit_aws::{AwsError, HttpRequest, HttpResponse, Method, Transport};

/// Largest response (headers and body) read from the agent.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Largest header block accepted.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// A validated `http://<loopback>[:port]` base URL.
#[derive(Clone, PartialEq, Eq)]
pub struct LoopbackEndpoint {
    host: String,
    port: u16,
}

impl LoopbackEndpoint {
    /// Parse `http://host[:port][/]` where `host` is `localhost`, an `IPv4`
    /// loopback literal or `[::1]`. The default port is 80.
    ///
    /// # Errors
    /// A short reason (never the raw text) when the URL is anything else.
    pub fn parse(url: &str) -> Result<Self, &'static str> {
        let Some(rest) = url.strip_prefix("http://") else {
            return Err("the endpoint must be an http:// URL");
        };
        let authority = rest.strip_suffix('/').unwrap_or(rest);
        if authority.is_empty()
            || authority
                .bytes()
                .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'/' | b'?' | b'#' | b'@'))
        {
            return Err("the endpoint must be http://HOST[:PORT] without a path or userinfo");
        }
        let (host, port_text) = if let Some(bracketed) = authority.strip_prefix('[') {
            let Some((inside, after)) = bracketed.split_once(']') else {
                return Err("malformed IPv6 address");
            };
            (format!("[{inside}]"), after.strip_prefix(':'))
        } else {
            match authority.split_once(':') {
                Some((host, port)) => (host.to_string(), Some(port)),
                None => (authority.to_string(), None),
            }
        };
        let port = match port_text {
            None => 80,
            Some(text) => match text.parse::<u16>() {
                Ok(port) if port != 0 => port,
                _ => return Err("the port must be 1-65535"),
            },
        };
        if !is_loopback_host(&host) {
            return Err(
                "the endpoint must be a loopback address (localhost, 127.0.0.0/8 or [::1])",
            );
        }
        Ok(Self { host, port })
    }

    /// `http://host:port`, the base the caller appends request paths to.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return v4.is_loopback();
    }
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .and_then(|inner| inner.parse::<Ipv6Addr>().ok())
        .is_some_and(|v6| v6.is_loopback())
}

/// Split `http://host:port/path?query` produced by [`LoopbackEndpoint`].
fn split_url(url: &str) -> Result<(String, u16, String), AwsError> {
    let invalid = || AwsError::connect("the request URL is not an http://HOST:PORT URL");
    let rest = url.strip_prefix("http://").ok_or_else(invalid)?;
    let (authority, target) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let endpoint =
        LoopbackEndpoint::parse(&format!("http://{authority}")).map_err(|_| invalid())?;
    Ok((endpoint.host, endpoint.port, target.to_string()))
}

/// [`Transport`] over `std::net::TcpStream` that only talks to loopback.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopbackHttp;

impl LoopbackHttp {
    fn addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, AwsError> {
        let target = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host);
        let resolved = (target, port)
            .to_socket_addrs()
            .map_err(|error| AwsError::connect(format!("cannot resolve the endpoint: {error}")))?;
        let loopback: Vec<SocketAddr> = resolved
            .filter(|address| address.ip().is_loopback())
            .collect();
        if loopback.is_empty() {
            return Err(AwsError::connect(
                "the endpoint does not resolve to a loopback address",
            ));
        }
        Ok(loopback)
    }
}

impl Transport for LoopbackHttp {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, AwsError> {
        let (host, port, target) = split_url(&request.url)?;
        let started = Instant::now();
        let mut stream = connect(&Self::addresses(&host, port)?, request.timeouts.connect())?;
        stream
            .set_write_timeout(Some(request.timeouts.read()))
            .map_err(|error| AwsError::connect(format!("cannot set a timeout: {error}")))?;

        let mut head = format!(
            "{} {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept-Encoding: identity\r\n",
            request.method.as_str()
        );
        for (name, value) in request.headers.entries() {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        if !request.body.is_empty() || request.method == Method::Post {
            let _ = write!(head, "Content-Length: {}\r\n", request.body.len());
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(&request.body))
            .and_then(|()| stream.flush())
            .map_err(|error| io_error(&error, "send the request"))?;

        let deadline = started.checked_add(request.timeouts.read().max(request.timeouts.connect()));
        let raw = read_all(&mut stream, deadline)?;
        parse_response(&raw, request.method)
    }
}

fn connect(addresses: &[SocketAddr], timeout: Duration) -> Result<TcpStream, AwsError> {
    let mut last = None;
    for address in addresses {
        match TcpStream::connect_timeout(address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(last.map_or_else(
        || AwsError::connect("no address to connect to"),
        |error| io_error(&error, "connect"),
    ))
}

fn io_error(error: &std::io::Error, doing: &str) -> AwsError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => AwsError::timeout(
            TimeoutPhase::Read,
            format!("timed out while trying to {doing}"),
        ),
        _ => AwsError::connect(format!("cannot {doing}: {error}")),
    }
}

/// Read until EOF, the byte cap or the deadline.
fn read_all(stream: &mut TcpStream, deadline: Option<Instant>) -> Result<Vec<u8>, AwsError> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = deadline
            .and_then(|limit| limit.checked_duration_since(Instant::now()))
            .filter(|left| !left.is_zero())
            .ok_or_else(|| AwsError::timeout(TimeoutPhase::Read, "the response was too slow"))?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|error| AwsError::connect(format!("cannot set a timeout: {error}")))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(data),
            Ok(count) => {
                let Some(next) = data.len().checked_add(count) else {
                    return Err(AwsError::invalid_response("the response is too large"));
                };
                if next > MAX_RESPONSE_BYTES {
                    return Err(AwsError::invalid_response("the response is too large"));
                }
                data.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(&error, "read the response")),
        }
    }
}

fn bad(reason: &str) -> AwsError {
    AwsError::invalid_response(reason)
}

fn parse_response(raw: &[u8], method: Method) -> Result<HttpResponse, AwsError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| bad("the response has no header terminator"))?;
    if split > MAX_HEADER_BYTES {
        return Err(bad("the response headers are too large"));
    }
    let head = raw
        .get(..split)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .ok_or_else(|| bad("the response headers are not UTF-8"))?;
    let body = split
        .checked_add(4)
        .and_then(|start| raw.get(start..))
        .ok_or_else(|| bad("the response has no header terminator"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut words = status_line.split_whitespace();
    let version = words.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(bad("the response is not HTTP/1.x"));
    }
    let status: u16 = words
        .next()
        .and_then(|code| code.parse().ok())
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| bad("the response status is malformed"))?;
    let mut response = HttpResponse::new(status, b"");
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(bad("a response header is malformed"));
        };
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
            if !chunked {
                return Err(bad("an unsupported transfer encoding"));
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            length = Some(value.parse().map_err(|_| bad("a bad content length"))?);
        }
        // A header the boundary rejects (control characters) is dropped; the
        // agent's payload is the body.
        let _ = response.headers.insert(name, value);
    }
    response.body = if method == Method::Head || status == 204 || status == 304 {
        Vec::new()
    } else if chunked {
        decode_chunked(body)?
    } else if let Some(length) = length {
        if body.len() < length {
            return Err(bad("the response body is shorter than its content length"));
        }
        body.get(..length).unwrap_or_default().to_vec()
    } else {
        body.to_vec()
    };
    Ok(response)
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, AwsError> {
    let bad = || AwsError::invalid_response("the chunked response body is malformed");
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(bad)?;
        let size_text = data
            .get(..line_end)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .ok_or_else(bad)?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| bad())?;
        data = line_end
            .checked_add(2)
            .and_then(|start| data.get(start..))
            .ok_or_else(bad)?;
        if size == 0 {
            return Ok(out);
        }
        let end = size.checked_add(2).ok_or_else(bad)?;
        let chunk = data.get(..size).ok_or_else(bad)?;
        if data.get(size..end) != Some(b"\r\n".as_slice()) {
            return Err(bad());
        }
        out.extend_from_slice(chunk);
        data = data.get(end..).ok_or_else(bad)?;
    }
}

#[cfg(test)]
mod tests;
