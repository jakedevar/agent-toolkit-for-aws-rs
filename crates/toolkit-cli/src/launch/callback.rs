//! Loopback OAuth callback request boundary.

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Ignore(u16),
    Code(String),
    Denied(String),
    InvalidCode,
}

pub fn parse_request(_head: &[u8], _port: u16, _state: &str) -> Outcome {
    Outcome::Ignore(400)
}

#[cfg(test)]
mod tests;
