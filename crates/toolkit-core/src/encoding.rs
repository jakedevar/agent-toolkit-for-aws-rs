//! Binary-to-text encodings shared by the slices (#4382 L0).
//!
//! [`base64url_nopad`] is RFC 4648 section 5 (the URL- and filename-safe
//! alphabet) without `=` padding: the encoding OAuth 2.0 PKCE (RFC 7636) uses
//! for the code verifier and the `S256` challenge.

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
