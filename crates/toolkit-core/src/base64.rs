//! Standard base64 (RFC 4648 section 4, `+/` alphabet, `=` padding).
//!
//! The workspace has no base64 dependency. [`encode`] writes the padded
//! form. [`decode_validate`] reads it exactly as Python's
//! `base64.b64decode(s, validate=True)` does since Python 3.11, which is
//! `binascii.a2b_base64(s, strict_mode=True)`:
//!
//! - only the 64 alphabet characters and `=`; anything else (whitespace and
//!   newlines included) is refused;
//! - no leading padding, no padding after a complete group of four, and no
//!   data or padding after the `=` that completes the last group;
//! - the data length is a multiple of four once padded: two data characters
//!   take `==` and three take `=`; one character past a multiple of four is
//!   never valid;
//! - unused low bits in the last character are ignored, as binascii
//!   ignores them.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
#[must_use]
pub fn encode(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
    for chunk in raw.chunks(3) {
        let block = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = (u32::from(block[0]) << 16) | (u32::from(block[1]) << 8) | u32::from(block[2]);
        for (index, shift) in [18_u32, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[((value >> shift) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The six-bit value of one alphabet character.
fn sextet(byte: u8) -> Option<u32> {
    let value = match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

/// Decode `text` under the rules in the module documentation; `None` when
/// Python would raise `binascii.Error`.
#[must_use]
pub fn decode_validate(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let data_len = bytes.iter().position(|&b| b == b'=').unwrap_or(bytes.len());
    let (data, padding) = bytes.split_at(data_len);
    if padding.iter().any(|&b| b != b'=') {
        return None;
    }
    let expected_padding = match data.len() % 4 {
        0 => 0,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    if padding.len() != expected_padding {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    for group in data.chunks(4) {
        let mut value = 0_u32;
        for &byte in group {
            value = (value << 6) | sextet(byte)?;
        }
        // Left-align a short group to 24 bits; its unused low bits drop out.
        value <<= 6 * (4 - group.len());
        let [_, first, second, third] = value.to_be_bytes();
        out.push(first);
        if group.len() > 2 {
            out.push(second);
        }
        if group.len() > 3 {
            out.push(third);
        }
    }
    Some(out)
}
