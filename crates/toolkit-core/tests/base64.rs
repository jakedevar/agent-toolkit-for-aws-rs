//! The shared base64 helpers (x402 S1, #5118; design section 9).
//!
//! `base64_standard` (`encode` here) is the standard padded encoder moved
//! from `bedrock_resolve`. `base64_standard_decode` (`decode_validate` here)
//! implements the strict x402 challenge reader rules (R-CH step 2), including
//! the deliberate D35 tightening over Python's `b64decode(validate=True)`.

use toolkit_core::encoding::{
    base64_standard as encode, base64_standard_decode as decode_validate,
};

#[test]
fn encode_matches_the_standard_vectors() {
    for (raw, expected) in [
        (&b""[..], ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
        (b"\xff\xfe\xfd", "//79"),
    ] {
        assert_eq!(encode(raw), expected);
    }
}

#[test]
fn decode_round_trips_the_standard_vectors() {
    for raw in [
        &b""[..],
        b"f",
        b"fo",
        b"foo",
        b"foob",
        b"fooba",
        b"foobar",
        b"\xff\xfe\xfd\x00",
    ] {
        assert_eq!(decode_validate(&encode(raw)).as_deref(), Some(raw));
    }
}

#[test]
fn decode_refuses_characters_outside_the_standard_alphabet() {
    for text in [
        "Zm9v\n",
        " Zm9v",
        "Zm9v ",
        "Zm-v",
        "Zm_v",
        "Zm9v\r\n",
        "Zm9v!",
        "Zm9vYg==\n",
        "Zm=9v",
        "=Zm9v",
        "Zm9vY===",
        "Zm9vYg=a",
        "Zm9\u{e9}",
    ] {
        assert_eq!(decode_validate(text), None, "{text:?}");
    }
}

#[test]
fn decode_enforces_padding_rules_and_d35_tightening() {
    // One data character past a multiple of four is never valid.
    assert_eq!(decode_validate("Z"), None);
    assert_eq!(decode_validate("Zm9vY"), None);
    assert_eq!(decode_validate("Zm9vY="), None);
    assert_eq!(decode_validate("Zm9vY=="), None);
    // Two or three data characters need their padding.
    assert_eq!(decode_validate("Zg"), None);
    assert_eq!(decode_validate("Zg="), None);
    assert_eq!(decode_validate("Zm8"), None);
    assert_eq!(decode_validate("Zg==").as_deref(), Some(&b"f"[..]));
    assert_eq!(decode_validate("Zm8=").as_deref(), Some(&b"fo"[..]));
    // Python strict mode rejects excess padding after a partial quartet;
    // D35 additionally rejects padding after a complete quartet.
    assert_eq!(decode_validate("Zm8=="), None);
    assert_eq!(decode_validate("Zg==="), None);
    assert_eq!(decode_validate("Zg==Zg=="), None);
    assert_eq!(decode_validate("Zm9v=="), None);
    assert_eq!(decode_validate("Zm9v="), None);
    assert_eq!(decode_validate("=="), None);
    assert_eq!(decode_validate("="), None);
    // Non-zero trailing bits are ignored, as binascii ignores them.
    assert_eq!(decode_validate("Zh==").as_deref(), Some(&b"f"[..]));
    assert_eq!(decode_validate("Zm9=").as_deref(), Some(&b"fo"[..]));
    assert_eq!(decode_validate("").as_deref(), Some(&b""[..]));
}
