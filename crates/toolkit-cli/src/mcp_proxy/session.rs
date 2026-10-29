//! Session values from the server, validated before they are stored or sent
//! as headers (design section 4.6: header injection and exfiltration through
//! the session id are refused).

/// An `Mcp-Session-Id` the proxy accepts: 1-256 bytes of visible ASCII
/// (0x21-0x7E), as the MCP specification requires.
pub(crate) fn valid_session_id(value: &str) -> bool {
    (1..=256).contains(&value.len()) && value.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

/// A negotiated protocol version the proxy sends as `Mcp-Protocol-Version`:
/// exactly `YYYY-MM-DD` in ASCII digits.
pub(crate) fn valid_protocol_version(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// The session the proxy holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    /// The validated `Mcp-Session-Id`, if the server issued one.
    pub id: Option<String>,
    /// The validated negotiated protocol version.
    pub protocol_version: Option<String>,
    /// The `GET` stream has been started for this session.
    pub get_started: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids() {
        assert!(valid_session_id("a"));
        assert!(valid_session_id(&"~".repeat(256)));
        assert!(valid_session_id("!#$%&'()*+,-./09:;<=>?@AZ[\\]^_`az{|}~"));
        for bad in ["", " ", "a b", "a\tb", "é", "\u{7f}"] {
            assert!(!valid_session_id(bad), "{bad:?}");
        }
        assert!(!valid_session_id(&"a".repeat(257)));
    }

    #[test]
    fn protocol_versions() {
        assert!(valid_protocol_version("2025-06-18"));
        for bad in ["2025-6-18", "2025-06-18 ", "2025/06/18", "", "abcd-ef-gh", "2025-06-1８"] {
            assert!(!valid_protocol_version(bad), "{bad:?}");
        }
    }
}
