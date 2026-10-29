//! The shared base64 helpers (x402 S1, #5118; design section 9).
//!
//! `base64_standard` (`encode` here) is the standard padded encoder moved
//! from `bedrock_resolve`. `base64_standard_decode` (`decode_validate` here)
//! reproduces Python's `base64.b64decode(s, validate=True)`, which the x402
//! challenge reader (R-CH step 2) relies on. Since Python 3.11 that is
//! `binascii.a2b_base64(s, strict_mode=True)`: the standard alphabet only,
//! correct padding, nothing after it and no leading padding.

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
fn decode_follows_the_python_padding_rules() {
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
    // Strict mode: no data or padding after the pad that completes the
    // quad, no padding after a complete quad, no leading padding.
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
