//! Binary-to-text encodings shared by the slices (#4382 L0, #5118 x402 S1).
//!
//! [`base64url_nopad`] is RFC 4648 section 5 (the URL- and filename-safe
//! alphabet) without `=` padding: the encoding OAuth 2.0 PKCE (RFC 7636) uses
//! for the code verifier and the `S256` challenge.
//!
//! [`base64_standard`] is RFC 4648 section 4 (`+/`, `=` padding), moved here
//! from `bedrock_resolve`. [`base64_standard_decode`] reads it exactly as
//! Python's `base64.b64decode(s, validate=True)` does since Python 3.11,
//! which is `binascii.a2b_base64(s, strict_mode=True)`:
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

/// `data` in base64url (RFC 4648 section 5) without padding.
#[must_use]
pub fn base64url_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let group = (b0 << 16) | (b1 << 8) | b2;
        let symbols = chunk.len() + 1;
        for index in 0..symbols {
            let shift = 18 - 6 * index;
            out.push(char::from(ALPHABET[((group >> shift) & 0x3f) as usize]));
        }
    }
    out
}

const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
#[must_use]
pub fn base64_standard(raw: &[u8]) -> String {
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
                out.push(char::from(
                    STANDARD_ALPHABET[((value >> shift) & 0x3f) as usize],
                ));
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
pub fn base64_standard_decode(text: &str) -> Option<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;

    #[test]
    fn t_b64_1_rfc_4648_section_10_vectors_without_padding() {
        let vectors: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in vectors {
            assert_eq!(base64url_nopad(input), expected);
        }
        // The two URL-safe symbols replace `+` and `/`.
        assert_eq!(base64url_nopad(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url_nopad(&[0xfb, 0xef, 0xff]), "--__");
        let all: Vec<u8> = (0..=255).collect();
        let encoded = base64url_nopad(&all);
        assert_eq!(encoded.len(), 342);
        assert!(encoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn t_pkce_1_rfc_7636_appendix_b_challenge() {
        let octets: [u8; 32] = [
            116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212,
            37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
        ];
        let verifier = base64url_nopad(&octets);
        assert_eq!(verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            base64url_nopad(&sha256(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
