//! Loopback OAuth callback request boundary.

/// Result of parsing one complete request head. Ignored requests do not end sign-in.
#[derive(PartialEq, Eq)]
pub enum Outcome {
    Ignore(u16),
    /// The authorization code: a single-use secret, never shown by `Debug`
    /// (R-TOK-MEM, T-TOK-6, #5504).
    Code(String),
    Denied(String),
    InvalidCode,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ignore(status) => f.debug_tuple("Ignore").field(status).finish(),
            Self::Code(_) => f.debug_tuple("Code").field(&"<redacted>").finish(),
            Self::Denied(error) => f.debug_tuple("Denied").field(error).finish(),
            Self::InvalidCode => f.write_str("InvalidCode"),
        }
    }
}

pub mod command;
pub mod gate;
pub mod lifecycle;
pub mod listener;
pub mod pending;
pub mod spawn;
pub mod start;
pub mod wait;

const MAX_HEAD: usize = 8192;

fn decode(input: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut iter = input.bytes();
    while let Some(b) = iter.next() {
        match b {
            b'+' => bytes.push(b' '),
            b'%' => {
                let digit = |b: u8| {
                    char::from(b)
                        .to_digit(16)
                        .and_then(|n| u8::try_from(n).ok())
                };
                bytes.push(digit(iter.next()?)? * 16 + digit(iter.next()?)?);
            }
            _ => bytes.push(b),
        }
    }
    String::from_utf8(bytes).ok()
}

/// Compare equal-length state bytes without an early exit on differing content.
#[must_use]
pub fn state_matches(actual: &str, expected: &str) -> bool {
    state_matches_counted(actual, expected, || {})
}

fn state_matches_counted(actual: &str, expected: &str, mut visit: impl FnMut()) -> bool {
    actual.len() == expected.len()
        && actual.bytes().zip(expected.bytes()).fold(0, |acc, (a, b)| {
            visit();
            acc | (a ^ b)
        }) == 0
}

fn valid_oauth_error(error: &str) -> bool {
    !error.is_empty()
        && error.len() <= 64
        && error
            .bytes()
            .all(|b| matches!(b, 0x20..=0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}

/// Parse a complete bounded HTTP request head; no request data is reflected.
#[must_use]
pub fn parse_request(head: &[u8], port: u16, state: &str) -> Outcome {
    if head.len() > MAX_HEAD || !head.ends_with(b"\r\n\r\n") {
        return Outcome::Ignore(400);
    }
    let Ok(head) = std::str::from_utf8(head) else {
        return Outcome::Ignore(400);
    };
    let mut lines = head[..head.len() - 4].split("\r\n");
    let Some(line) = lines.next() else {
        return Outcome::Ignore(400);
    };
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Outcome::Ignore(400);
    };
    if version != "HTTP/1.0" && version != "HTTP/1.1" {
        return Outcome::Ignore(400);
    }
    if method != "GET" {
        return Outcome::Ignore(405);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return Outcome::Ignore(404);
    }
    let expected_host = format!("127.0.0.1:{port}");
    let mut host = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Outcome::Ignore(400);
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Outcome::Ignore(400);
        }
        if name.eq_ignore_ascii_case("host") && host.replace(value.trim()).is_some() {
            return Outcome::Ignore(400);
        }
    }
    if host != Some(expected_host.as_str()) {
        return Outcome::Ignore(400);
    }
    let (mut code, mut received_state, mut error) = (None, None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(key), Some(value)) = (decode(key), decode(value)) else {
            return Outcome::Ignore(400);
        };
        let slot = match key.as_str() {
            "code" => &mut code,
            "state" => &mut received_state,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Outcome::Ignore(400);
        }
    }
    if !received_state
        .as_deref()
        .is_some_and(|s| state_matches(s, state))
    {
        return Outcome::Ignore(400);
    }
    if let Some(error) = error {
        let valid = valid_oauth_error(&error);
        return Outcome::Denied(if valid {
            error
        } else {
            "invalid_error_code".to_string()
        });
    }
    if let Some(code) = code {
        if !code.is_empty()
            && code.len() <= 4096
            && code.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Outcome::Code(code);
        }
        return Outcome::InvalidCode;
    }
    Outcome::Ignore(400)
}

#[cfg(test)]
mod tests;

/// Pending record states; connection traffic never creates a terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingStatus {
    Listening,
    Exchanging,
    Succeeded,
    Failed,
    TimedOut,
}

/// Result of one auth-wait evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitDecision {
    Authenticated,
    Poll,
    HelperBlocked,
    Failed,
    TimedOut,
    Ended,
}

/// The lock is the custody boundary: only a free lock permits a final failure.
#[must_use]
pub fn wait_decision(
    status: PendingStatus,
    lock_held: bool,
    valid_session: bool,
    now: i64,
    cutoff: i64,
) -> WaitDecision {
    if valid_session {
        return WaitDecision::Authenticated;
    }
    if lock_held {
        return if now >= cutoff.saturating_add(5) {
            WaitDecision::HelperBlocked
        } else {
            WaitDecision::Poll
        };
    }
    match status {
        PendingStatus::Failed => WaitDecision::Failed,
        PendingStatus::TimedOut => WaitDecision::TimedOut,
        _ if now >= cutoff => WaitDecision::TimedOut,
        _ => WaitDecision::Ended,
    }
}

/// Construct a fixed response; input values never enter the page.
#[must_use]
pub fn response(status: u16) -> Vec<u8> {
    let (status, body) = match status {
        200 => (
            "200 OK",
            "<!doctype html><title>Sign-in complete</title><p>You may close this window.</p>",
        ),
        404 => ("404 Not Found", "<!doctype html><title>Not found</title>"),
        405 => (
            "405 Method Not Allowed",
            "<!doctype html><title>Invalid request</title>",
        ),
        _ => (
            "400 Bad Request",
            "<!doctype html><title>Invalid request</title>",
        ),
    };
    response_body(status, body)
}

fn authorization_failed_response() -> Vec<u8> {
    response_body(
        "400 Bad Request",
        "<!doctype html><title>Authorization failed</title>",
    )
}

fn response_body(status: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}
