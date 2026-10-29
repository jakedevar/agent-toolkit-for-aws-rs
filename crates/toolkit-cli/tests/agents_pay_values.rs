//! x402 S1 (#5118): the pure value layers of the payment policy.
//!
//! Test IDs from `docs/migration/x402-payments-design.md`:
//! - T-URLP: Python `urlparse` compatibility (R-URLPARSE): params, IPv6,
//!   port, case.
//! - T-AMT: the challenge amount grammar and the cap grammar (R-AMT, D5, D6),
//!   and the one cap representation (R-AMT-REP).
//! - T-AMT-USD: the `amount_usd` rendering (R-AMT, Q10).
//! - canonical amounts: the alias rules of `validated_amount_units`.
//! - T-CH: challenge extraction (R-CH, D1).
//! - pyval: Python `str`, truthiness and `int()` over JSON (used by R-POL,
//!   R-AMT and R-VER).
//!
//! The cap boundary table (T-AMT-CAP-1..6) runs through the whole gate and
//! lives in `agents_pay_policy.rs`.

use toolkit_cli::agents_pay::amount::{
    base_units, exceeds_cap, usd_text, validated_amount_units, AmountError, CapDecimal,
};
use toolkit_cli::agents_pay::challenge::{extract, MAX_HEADER_JSON_BYTES};
use toolkit_cli::agents_pay::policy::PolicyError;
use toolkit_cli::agents_pay::pyval::{float_repr, py_int, py_str, truthy};
use toolkit_cli::agents_pay::urlparse::{canonical_origin, urlparse};
use toolkit_core::encoding::base64_standard as encode;
use toolkit_core::{parse_json, Json};

fn json(text: &str) -> Json {
    parse_json(text).unwrap_or_else(|error| panic!("fixture JSON {text}: {error}"))
}

// ------------------------------------------------------------------ pyval

#[test]
fn pyval_truthiness_follows_python() {
    for falsy in ["null", "false", "0", "0.0", "-0.0", "\"\"", "[]", "{}"] {
        assert!(!truthy(&json(falsy)), "{falsy} must be falsy");
    }
    for value in [
        "true",
        "1",
        "-1",
        "0.5",
        "\"0\"",
        "\" \"",
        "[0]",
        "{\"a\":null}",
    ] {
        assert!(truthy(&json(value)), "{value} must be truthy");
    }
}

#[test]
fn pyval_str_renders_scalars_like_python_and_refuses_containers() {
    let cases = [
        ("\"abc\"", "abc"),
        ("\"\"", ""),
        ("-5", "-5"),
        ("100000", "100000"),
        ("true", "True"),
        ("false", "False"),
        ("null", "None"),
        ("0.1", "0.1"),
        ("100000.0", "100000.0"),
        ("1e2", "100.0"),
        ("1e15", "1000000000000000.0"),
        ("1e16", "1e+16"),
        ("1.5e16", "1.5e+16"),
        ("0.0001", "0.0001"),
        ("0.00001", "1e-05"),
        ("1.5e-5", "1.5e-05"),
        ("-0.0", "-0.0"),
        ("0.0", "0.0"),
        ("123456789012345680.0", "1.2345678901234568e+17"),
        ("1e22", "1e+22"),
        ("5e-324", "5e-324"),
        ("1.7976931348623157e308", "1.7976931348623157e+308"),
        // An integer outside i64 is a float in toolkit-core (R-AMT).
        ("100000000000000000000", "1e+20"),
    ];
    for (text, expected) in cases {
        assert_eq!(py_str(&json(text)).as_deref(), Some(expected), "{text}");
    }
    // D2: an array or object has no comparable rendering.
    assert_eq!(py_str(&json("[1]")), None);
    assert_eq!(py_str(&json("{\"a\":1}")), None);
}

#[test]
fn pyval_float_repr_switches_to_exponent_form_like_python() {
    assert_eq!(float_repr(1e16), "1e+16");
    assert_eq!(float_repr(9_999_999_999_999_998.0), "9999999999999998.0");
    assert_eq!(float_repr(0.000_1), "0.0001");
    assert_eq!(float_repr(0.000_099), "9.9e-05");
    assert_eq!(float_repr(2.5), "2.5");
    assert_eq!(float_repr(-1e-7), "-1e-07");
    assert_eq!(float_repr(1e100), "1e+100");
}

#[test]
fn pyval_int_follows_python_int_on_ascii() {
    let ok = [
        ("2", 2),
        ("-7", -7),
        ("2.9", 2),
        ("-2.9", -2),
        ("0.5", 0),
        ("true", 1),
        ("false", 0),
        ("\"2\"", 2),
        ("\" 2 \"", 2),
        ("\"+2\"", 2),
        ("\"-2\"", -2),
        ("\"1_0\"", 10),
        ("\"0_1\"", 1),
        ("\"007\"", 7),
        ("\"\\t\\n 3 \\f\\r\\u000b\"", 3),
        ("\"9223372036854775807\"", i64::MAX),
        ("\"-9223372036854775808\"", i64::MIN),
    ];
    for (text, expected) in ok {
        assert_eq!(py_int(&json(text)), Ok(expected), "{text}");
    }
    let refused = [
        "null",
        "[1]",
        "{}",
        "\"\"",
        "\" \"",
        "\"1__0\"",
        "\"_1\"",
        "\"1_\"",
        "\"2.0\"",
        "\"0x10\"",
        "\"- 1\"",
        "\"+-1\"",
        "\"\u{663}\"",
        "\"1e3\"",
        // D7: outside i64.
        "\"9223372036854775808\"",
        "1e300",
        "-1e300",
    ];
    for text in refused {
        assert!(py_int(&json(text)).is_err(), "{text} must raise ValueError");
    }
    for c in '\u{1c}'..='\u{1f}' {
        for text in [format!("{c}2"), format!("2{c}")] {
            assert!(py_int(&Json::string(&text)).is_err(), "{text:?}");
        }
    }
}

// ---------------------------------------------------------------- T-URLP

#[test]
fn t_urlp_splits_scheme_netloc_path_query_and_fragment() {
    let parsed = urlparse("HTTPS://Merchant.Test:8443/v1/x;v=2?x=1#frag").unwrap();
    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.netloc(), "Merchant.Test:8443");
    assert_eq!(parsed.path(), "/v1/x");
    assert_eq!(parsed.query(), "x=1");
    assert_eq!(parsed.fragment(), "frag");
    assert_eq!(parsed.hostname().as_deref(), Some("merchant.test"));
    assert_eq!(parsed.port(), Ok(Some(8443)));

    let bare = urlparse("https://h?x").unwrap();
    assert_eq!(bare.netloc(), "h");
    assert_eq!(bare.path(), "");
    assert_eq!(bare.query(), "x");
    let hash = urlparse("https://h#/p").unwrap();
    assert_eq!(hash.netloc(), "h");
    assert_eq!(hash.path(), "");
}

#[test]
fn t_urlp_params_are_cut_from_the_last_path_segment_only() {
    assert_eq!(urlparse("https://h/a;p").unwrap().path(), "/a");
    assert_eq!(urlparse("https://h/a;p/b").unwrap().path(), "/a;p/b");
    assert_eq!(urlparse("https://h/a/b;p;q").unwrap().path(), "/a/b");
    assert_eq!(urlparse("https://h/;p").unwrap().path(), "/");
    assert_eq!(urlparse("https://h/a;p?q;r").unwrap().path(), "/a");
}

#[test]
fn t_urlp_ipv6_hosts_lose_their_brackets() {
    let parsed = urlparse("https://[2606:4700::1]:8443/x").unwrap();
    assert_eq!(parsed.netloc(), "[2606:4700::1]:8443");
    assert_eq!(parsed.hostname().as_deref(), Some("2606:4700::1"));
    assert_eq!(parsed.port(), Ok(Some(8443)));
    assert_eq!(
        canonical_origin("https://[2606:4700::1]:8443/x").unwrap(),
        "https://2606:4700::1:8443"
    );
    assert_eq!(
        canonical_origin("https://[2606:4700::1]/x").unwrap(),
        "https://2606:4700::1"
    );
    let upper = urlparse("https://[2606:4700::ABCD]/").unwrap();
    assert_eq!(upper.hostname().as_deref(), Some("2606:4700::abcd"));
    // The zone keeps its case, as Python's `hostname` keeps it.
    let zoned = urlparse("https://[FE80::1%25Eth0]/").unwrap();
    assert_eq!(zoned.hostname().as_deref(), Some("fe80::1%25Eth0"));
    assert!(urlparse("https://[vF.Future]/").is_ok());
}

#[test]
fn t_urlp_invalid_brackets_raise_value_error() {
    for url in [
        "https://[::1/x",
        "https://::1]/x",
        "https://x[::1]/",
        "https://[::1]x/",
        "https://[1.2.3.4]/",
        "https://[not-an-address]/",
    ] {
        assert!(urlparse(url).is_err(), "{url}");
    }
}

#[test]
fn t_urlp_port_follows_python() {
    assert_eq!(urlparse("https://h/").unwrap().port(), Ok(None));
    assert_eq!(urlparse("https://h:/").unwrap().port(), Ok(None));
    assert_eq!(urlparse("https://h:0/").unwrap().port(), Ok(Some(0)));
    assert_eq!(
        urlparse("https://h:65535/").unwrap().port(),
        Ok(Some(65535))
    );
    assert_eq!(
        urlparse("https://u:p@h:443/").unwrap().port(),
        Ok(Some(443))
    );
    for url in [
        "https://h:65536/",
        "https://h:abc/",
        "https://h:+1/",
        "https://h:-1/",
        "https://h: 1/",
        "https://h:1:2/",
        "https://h:\u{663}/",
    ] {
        assert!(urlparse(url).unwrap().port().is_err(), "{url}");
    }
}

#[test]
fn t_urlp_canonical_origin_drops_443_and_0_and_lowercases() {
    assert_eq!(
        canonical_origin("https://[2606:4700::1%25Eth0]/item").unwrap(),
        "https://2606:4700::1%25eth0"
    );
    assert_eq!(
        canonical_origin("https://MERCHANT.test:443/v1/x402-test?x=1").unwrap(),
        "https://merchant.test"
    );
    assert_eq!(
        canonical_origin("https://merchant.test:8443").unwrap(),
        "https://merchant.test:8443"
    );
    assert_eq!(canonical_origin("https://h:0/x").unwrap(), "https://h");
    assert_eq!(canonical_origin("https://h:/x").unwrap(), "https://h");
    assert_eq!(
        canonical_origin("https://user:pw@Host.Example/x").unwrap(),
        "https://host.example"
    );
    assert!(canonical_origin("https://h:99999/").is_err());
}

#[test]
fn t_urlp_credentials_follow_python_truthiness() {
    assert!(urlparse("https://u:p@h/").unwrap().has_credentials());
    assert!(urlparse("https://u@h/").unwrap().has_credentials());
    assert!(urlparse("https://:p@h/").unwrap().has_credentials());
    // Python's username is "" and its password is "" or None: both falsy.
    assert!(!urlparse("https://@h/").unwrap().has_credentials());
    assert!(!urlparse("https://:@h/").unwrap().has_credentials());
    assert!(!urlparse("https://h/").unwrap().has_credentials());
    // The last `@` ends the userinfo.
    let parsed = urlparse("https://a@b@Host/").unwrap();
    assert!(parsed.has_credentials());
    assert_eq!(parsed.hostname().as_deref(), Some("host"));
}

#[test]
fn t_urlp_strips_leading_controls_and_removes_tabs_and_newlines() {
    let parsed = urlparse(" \u{1}https://h/a\tb\r\nc").unwrap();
    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.path(), "/abc");
    // A scheme needs a leading letter; otherwise everything is the path.
    let no_scheme = urlparse("1https://h/").unwrap();
    assert_eq!(no_scheme.scheme(), "");
    assert_eq!(no_scheme.netloc(), "");
    assert_eq!(urlparse("https:///x").unwrap().hostname(), None);
}

#[test]
fn t_urlp_nfkc_netloc_characters_raise_value_error() {
    // U+FF0F FULLWIDTH SOLIDUS normalises to `/` under NFKC.
    assert!(urlparse("https://h\u{ff0f}x/").is_err());
    // A non-ASCII host that normalises to itself is fine.
    assert!(urlparse("https://b\u{fc}cher.example/").is_ok());
}

// ----------------------------------------------------------------- T-AMT

#[test]
fn t_amt_challenge_amount_accepts_only_ascii_digit_strings() {
    for good in ["1", "100000", "0100000", "500000"] {
        assert_eq!(base_units(good), Ok(good), "{good}");
    }
    for bad in [
        "1E+7",
        "Infinity",
        "NaN",
        "1_000_000",
        "500000.0",
        "  500000  ",
        "+500000",
        "0x7A120",
        "",
        "-500000",
        "\u{663}",
        "100.5",
        "1 0",
    ] {
        assert_eq!(
            base_units(bad),
            Err(AmountError::NotPlainInteger),
            "{bad:?}"
        );
    }
    for zero in ["0", "000"] {
        assert_eq!(base_units(zero), Err(AmountError::NotPositive), "{zero}");
    }
    assert_eq!(
        AmountError::NotPlainInteger.message(),
        "Payment challenge amount must be a plain integer in base units."
    );
    assert_eq!(
        AmountError::NotPositive.message(),
        "Payment challenge amount must be positive."
    );
}

#[test]
fn canonical_amounts_resolve_aliases_like_python() {
    let cases = [
        (r#"{"amount":"100000"}"#, Ok("100000")),
        (r#"{"amount":100000}"#, Ok("100000")),
        (r#"{"amount":"0100000"}"#, Ok("0100000")),
        (
            r#"{"amount":null,"maxAmountRequired":"100000"}"#,
            Ok("100000"),
        ),
        (r#"{"maxAmountRequired":"100000"}"#, Ok("100000")),
        (
            r#"{"amount":"100000","maxAmountRequired":"100000"}"#,
            Ok("100000"),
        ),
        (
            r#"{"amount":100000,"maxAmountRequired":"100000"}"#,
            Ok("100000"),
        ),
        (
            r#"{"amount":"1","maxAmountRequired":"50000000"}"#,
            Err(AmountError::Conflicting),
        ),
        (
            r#"{"amount":"50000000","maxAmountRequired":"1"}"#,
            Err(AmountError::Conflicting),
        ),
        (
            r#"{"amount":"0100000","maxAmountRequired":"100000"}"#,
            Err(AmountError::Conflicting),
        ),
        ("{}", Err(AmountError::Missing)),
        (
            r#"{"amount":null,"maxAmountRequired":null}"#,
            Err(AmountError::Missing),
        ),
        (r#"{"amount":100000.0}"#, Err(AmountError::NotPlainInteger)),
        (r#"{"amount":true}"#, Err(AmountError::NotPlainInteger)),
        (r#"{"amount":-5}"#, Err(AmountError::NotPlainInteger)),
        (r#"{"amount":0}"#, Err(AmountError::NotPositive)),
        (
            r#"{"amount":["100000"]}"#,
            Err(AmountError::NotPlainInteger),
        ),
        (
            r#"{"amount":100000000000000000000}"#,
            Err(AmountError::NotPlainInteger),
        ),
    ];
    for (entry, expected) in cases {
        assert_eq!(
            validated_amount_units(&json(entry)),
            expected.map(str::to_owned),
            "{entry}"
        );
    }
    assert_eq!(
        AmountError::Conflicting.message(),
        "Payment challenge contains conflicting amount fields."
    );
    assert_eq!(
        AmountError::Missing.message(),
        "Payment challenge has no amount."
    );
}

#[test]
fn t_amt_cap_representation_is_digits_times_a_power_of_ten() {
    let cases = [
        ("0.10", "10", -2),
        ("0.1", "1", -1),
        ("1e-1", "1", -1),
        ("10e-2", "10", -2),
        ("0.01e1", "1", -1),
        ("1.5E+2", "15", 1),
        ("1", "1", 0),
        (".1", "1", -1),
        ("1.", "1", 0),
        (" 0.1 ", "1", -1),
        ("\t0.1\n", "1", -1),
        ("1_0e-2", "10", -2),
        ("1e1_0", "1", 10),
        ("+1e-1", "1", -1),
        ("100000e-6", "100000", -6),
        ("0", "0", 0),
        ("000.000", "0", -3),
    ];
    for (text, digits, exponent) in cases {
        let cap = CapDecimal::parse(text).unwrap_or_else(|_| panic!("{text:?} must parse"));
        assert_eq!(
            (cap.digits(), cap.exponent()),
            (digits, exponent),
            "{text:?}"
        );
        assert!(!cap.is_negative(), "{text:?}");
    }
    let negative = CapDecimal::parse("-1e-1").unwrap();
    assert!(negative.is_negative());
    assert!(!negative.is_positive());
    assert!(!CapDecimal::parse("-0").unwrap().is_positive());
    assert!(!CapDecimal::parse("0e5").unwrap().is_positive());
    assert!(CapDecimal::parse("1e-1").unwrap().is_positive());
}

#[test]
fn t_amt_cap_from_json_uses_the_number_value() {
    for (text, digits, exponent) in [
        ("1", "1", 0),
        ("1.0", "1", 0),
        ("0.1", "1", -1),
        ("1e-1", "1", -1),
        ("10E-2", "1", -1),
        ("\"0.10\"", "10", -2),
        ("0.5", "5", -1),
    ] {
        let cap = CapDecimal::from_json(&json(text)).unwrap();
        assert_eq!((cap.digits(), cap.exponent()), (digits, exponent), "{text}");
    }
    for text in ["true", "null", "[1]", "{}", "\"Infinity\"", "\"NaN\""] {
        assert!(CapDecimal::from_json(&json(text)).is_err(), "{text}");
    }
}

#[test]
fn t_amt_cap_d6_refuses_non_finite_and_unbounded_spellings() {
    let mantissa_1000 = "1".repeat(1000);
    let mantissa_1001 = "1".repeat(1001);
    let fraction_1001 = format!("0.{}", "1".repeat(1000));
    for text in [
        "Infinity",
        "-Infinity",
        "+Infinity",
        "inf",
        "Inf",
        "NaN",
        "nan",
        "sNaN",
        "-NaN",
        "1e1001",
        "1e-1001",
        "1e+1001",
        mantissa_1001.as_str(),
        fraction_1001.as_str(),
        "1e99999999999999999999",
    ] {
        assert!(
            CapDecimal::parse(text).is_err(),
            "{text} must be refused (D6)"
        );
    }
    assert!(CapDecimal::parse("1e1000").is_ok());
    assert!(CapDecimal::parse("1e-1000").is_ok());
    assert!(CapDecimal::parse(&mantissa_1000).is_ok());
}

#[test]
fn t_amt_cap_grammar_errors_are_refused() {
    for text in [
        "", " ", "1__0", "_1", "1_", "1_.5", "1._5", "1e", "e1", ".", "1e+", "--1", "+-1", "1.2.3",
        "1e1.5", "0x10", "1 0", "1,5", "\u{663}", "1e_1", "1e1_", "١",
    ] {
        assert!(CapDecimal::parse(text).is_err(), "{text:?} must be refused");
    }
}

#[test]
fn t_amt_comparison_is_exact_at_the_unit_boundary() {
    let cap = |text: &str| CapDecimal::parse(text).unwrap();
    // $0.10 == 100000 units.
    assert!(!exceeds_cap("100000", &cap("0.1")));
    assert!(exceeds_cap("100001", &cap("0.1")));
    assert!(!exceeds_cap("0100000", &cap("0.1")));
    assert!(exceeds_cap("1000000", &cap("1e-1")));
    // k < 0: the cap is below one unit.
    assert!(exceeds_cap("1", &cap("5e-7")));
    assert!(!exceeds_cap("1", &cap("15e-7")));
    assert!(exceeds_cap("2", &cap("15e-7")));
    // A huge amount against a huge cap.
    let units = format!("1{}", "0".repeat(1006));
    assert!(!exceeds_cap(&units, &cap("1e1000")));
    assert!(exceeds_cap(&format!("{units}0"), &cap("1e1000")));
}

// -------------------------------------------------------------- T-AMT-USD

#[test]
fn t_amt_usd_renders_python_decimal_division() {
    for (units, expected) in [
        ("100000", "0.1"),
        ("1000000", "1"),
        ("10000000", "10"),
        ("1", "0.000001"),
        ("500000", "0.5"),
        ("0100000", "0.1"),
        ("123456", "0.123456"),
        ("1230000", "1.23"),
        ("100000000000", "100000"),
        (
            "1234567890123456789012345678",
            "1234567890123456789012.345678",
        ),
        // 29 significant digits: rounded half-even to 28 (Q10).
        (
            "12345678901234567890123456789",
            "12345678901234567890123.45679",
        ),
        (
            "99999999999999999999999999999",
            "100000000000000000000000.0000",
        ),
    ] {
        assert_eq!(usd_text(units), expected, "{units}");
    }
    // Beyond 28 digits with a positive exponent: stable and no panic.
    let huge = format!("1{}", "0".repeat(40));
    assert_eq!(usd_text(&huge), "1.000000000000000000000000000E+34");
    assert_eq!(usd_text(&huge), usd_text(&huge));
}

// ------------------------------------------------------------------ T-CH

fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

const CHALLENGE: &str = r#"{"x402Version":2,"accepts":[{"scheme":"exact"}]}"#;
const OTHER: &str = r#"{"x402Version":1,"accepts":[{"scheme":"other"}]}"#;

fn refused(text: &str) -> Result<Json, PolicyError> {
    Err(PolicyError::Refused(text.to_owned()))
}

const NOT_PARSEABLE: &str = "402 response contains no parseable x402 challenge.";
const NO_ACCEPTS: &str = "402 challenge is missing the required 'accepts' array.";

#[test]
fn t_ch_header_base64_json_wins() {
    let encoded = encode(CHALLENGE.as_bytes());
    let got = extract(
        &headers(&[("payment-required", &encoded)]),
        OTHER.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
}

#[test]
fn t_ch_extra_padding_after_complete_quartet_falls_back_to_body() {
    let document = r#"{"accepts":[1]}"#;
    let encoded = encode(document.as_bytes());
    assert!(!encoded.ends_with('='));
    let padded = format!("{encoded}=");
    assert_eq!(extract(&headers(&[("payment-required", &padded)]), b""), refused(NOT_PARSEABLE));
    assert_eq!(extract(&headers(&[("payment-required", &padded)]), OTHER.as_bytes()), Ok(json(OTHER)));
}

#[test]
fn t_ch_header_raw_json_is_the_second_choice() {
    let got = extract(
        &headers(&[("payment-required", CHALLENGE)]),
        OTHER.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
}

#[test]
fn t_ch_header_names_are_case_insensitive_and_x_payment_required_is_the_fallback() {
    let got = extract(&headers(&[("Payment-Required", CHALLENGE)]), b"");
    assert_eq!(got, Ok(json(CHALLENGE)));
    let got = extract(&headers(&[("X-PAYMENT-REQUIRED", CHALLENGE)]), b"");
    assert_eq!(got, Ok(json(CHALLENGE)));
    // payment-required wins over x-payment-required.
    let got = extract(
        &headers(&[
            ("x-payment-required", OTHER),
            ("payment-required", CHALLENGE),
        ]),
        b"",
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
    // An empty payment-required is falsy, so the fallback header is read.
    let got = extract(
        &headers(&[("payment-required", ""), ("x-payment-required", CHALLENGE)]),
        b"",
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
}

#[test]
fn t_ch_repeated_header_lines_are_joined_with_comma_space() {
    let got = extract(
        &headers(&[
            ("payment-required", r#"{"accepts":[1]"#),
            ("Payment-Required", r#""x":1}"#),
        ]),
        b"",
    );
    assert_eq!(got, Ok(json(r#"{"accepts":[1], "x":1}"#)));
}

#[test]
fn t_ch_a_header_without_truthy_accepts_falls_through_to_the_body() {
    for header in [
        r#"{"accepts":[]}"#,
        r#"{"accepts":null}"#,
        r#"{"accepts":0}"#,
        r#"{"accepts":""}"#,
        r#"{"accepts":{}}"#,
        r#"{"accepts":false}"#,
        r#"[{"accepts":[1]}]"#,
        "not json",
    ] {
        let got = extract(
            &headers(&[("payment-required", header)]),
            CHALLENGE.as_bytes(),
        );
        assert_eq!(got, Ok(json(CHALLENGE)), "{header}");
        let encoded = encode(header.as_bytes());
        let got = extract(
            &headers(&[("payment-required", &encoded)]),
            CHALLENGE.as_bytes(),
        );
        assert_eq!(got, Ok(json(CHALLENGE)), "base64 {header}");
    }
    // A truthy non-list `accepts` is accepted here; selection refuses it later.
    let odd = r#"{"accepts":"x"}"#;
    assert_eq!(
        extract(&headers(&[("payment-required", odd)]), b""),
        Ok(json(odd))
    );
}

#[test]
fn t_ch_base64_that_is_not_utf8_falls_back_to_raw_then_body() {
    let encoded = encode(b"\xff\xfe{}");
    let got = extract(
        &headers(&[("payment-required", &encoded)]),
        CHALLENGE.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
}

#[test]
fn t_ch_d1_duplicate_keys_and_non_finite_literals_are_never_accepted() {
    let duplicate = r#"{"accepts":[{"payTo":"a"}],"accepts":[{"payTo":"b"}]}"#;
    let got = extract(
        &headers(&[("payment-required", duplicate)]),
        CHALLENGE.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
    let encoded = encode(duplicate.as_bytes());
    let got = extract(
        &headers(&[("payment-required", &encoded)]),
        CHALLENGE.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
    assert_eq!(extract(&[], duplicate.as_bytes()), refused(NOT_PARSEABLE));
    let inner = r#"{"accepts":[{"amount":"1","amount":"9"}]}"#;
    assert_eq!(extract(&[], inner.as_bytes()), refused(NOT_PARSEABLE));
    for literal in ["NaN", "Infinity", "-Infinity"] {
        let body = format!(r#"{{"accepts":[{{"amount":{literal}}}]}}"#);
        assert_eq!(
            extract(&[], body.as_bytes()),
            refused(NOT_PARSEABLE),
            "{literal}"
        );
    }
}

#[test]
fn t_ch_body_rules() {
    assert_eq!(extract(&[], CHALLENGE.as_bytes()), Ok(json(CHALLENGE)));
    let wrapped = format!(r#"{{"error":"pay","challenge":{CHALLENGE}}}"#);
    assert_eq!(extract(&[], wrapped.as_bytes()), Ok(json(CHALLENGE)));
    let both = format!(r#"{{"accepts":[],"challenge":{CHALLENGE}}}"#);
    assert_eq!(extract(&[], both.as_bytes()), Ok(json(CHALLENGE)));
    assert_eq!(extract(&[], b"<html>pay</html>"), refused(NOT_PARSEABLE));
    assert_eq!(extract(&[], b""), refused(NOT_PARSEABLE));
    for body in [
        r#"{"error":"pay"}"#,
        r#"{"accepts":[]}"#,
        r#"{"challenge":{"accepts":[]}}"#,
        r#"{"challenge":"x"}"#,
        "[1,2]",
        "5",
        "null",
    ] {
        assert_eq!(extract(&[], body.as_bytes()), refused(NO_ACCEPTS), "{body}");
    }
    // Whitespace around the document is allowed, as json.loads allows it.
    let padded = format!("\n  {CHALLENGE}\n");
    assert_eq!(extract(&[], padded.as_bytes()), Ok(json(CHALLENGE)));
}

#[test]
fn t_ch_body_is_decoded_lossily() {
    let mut body = br#"{"accepts":[{"note":""#.to_vec();
    body.extend_from_slice(b"\xff\xfe");
    body.extend_from_slice(br#""}]}"#);
    let got = extract(&[], &body).unwrap();
    assert_eq!(got, json("{\"accepts\":[{\"note\":\"\u{fffd}\u{fffd}\"}]}"));
}

#[test]
fn t_ch_header_json_over_64_kib_is_ignored() {
    let pad = "a".repeat(MAX_HEADER_JSON_BYTES);
    let big = format!(r#"{{"accepts":[1],"pad":"{pad}"}}"#);
    assert!(big.len() > MAX_HEADER_JSON_BYTES);
    let got = extract(
        &headers(&[("payment-required", &big)]),
        CHALLENGE.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
    let encoded = encode(big.as_bytes());
    let got = extract(
        &headers(&[("payment-required", &encoded)]),
        CHALLENGE.as_bytes(),
    );
    assert_eq!(got, Ok(json(CHALLENGE)));
    // Exactly at the bound is still read.
    let fill = MAX_HEADER_JSON_BYTES - r#"{"accepts":[1],"pad":""}"#.len();
    let edge = format!(r#"{{"accepts":[1],"pad":"{}"}}"#, "a".repeat(fill));
    assert_eq!(edge.len(), MAX_HEADER_JSON_BYTES);
    let got = extract(
        &headers(&[("payment-required", &edge)]),
        CHALLENGE.as_bytes(),
    );
    assert_eq!(got, Ok(json(&edge)));
}
