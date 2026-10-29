//! x402 S1 (#5118): the payment policy gate, end to end and network-free.
//!
//! Test IDs from `docs/migration/x402-payments-design.md` (section 8.2, S1):
//! - Python classes of `test_x402_policy.py`, ported as behaviour:
//!   `RecipientAndValue` (`recipient_and_value_*`), `RecipientValidation`
//!   (`recipient_validation_*`), `OptionalOrigin`, pure part
//!   (`optional_origin_*`), `Authorize` (`authorize_*`), `Idempotency`
//!   (`idempotency_*`), `SignerInput`, pure part (`signer_input_*`).
//! - R-TOK golden vectors (`r_tok_golden_*`).
//! - T-POL-UNI (`t_pol_uni_*`), T-VER (`t_ver_*`), the R-ORIGIN texts
//!   (`r_origin_*`), R-RESBIND (`authorize_resource_*`), D4 and D11
//!   (`d4_*`, `d11_*`).
//! - T-AMT-CAP-1..6 (`t_amt_cap_*`): every spelling and boundary row runs
//!   through `authorize` over the spending fake.
//! - T-ORDER-PRE, policy part (`t_order_pre_*`): every locally decidable
//!   refusal leaves zero `ProcessPayment` calls on the spending fake.
//!
//! The spending fake stands in for the S4/S5 `process_payment`: like it, it
//! takes the `Approved` by value, and it is the only thing that can spend.

use std::net::IpAddr;

use toolkit_cli::agents_pay::policy::{
    assert_public_https_url, authorize, derive_client_token, select_accept_entry,
    validated_resource, x402_version, Approved, FixedResolver, PaymentRequest, PolicyError,
    Resources, UNIFORM_REFUSAL,
};
use toolkit_core::{parse_json, Json};

const USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const USDC_LOWER: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
const MERCHANT: &str = "0x1111111111111111111111111111111111111111";
const ATTACKER: &str = "0x2222222222222222222222222222222222222222";
const ORIGIN: &str = "https://sandbox.node4all.com";
const URL: &str = "https://sandbox.node4all.com/v1/x402-test";
const SESSION: &str = "ps-1";

// --------------------------------------------------------------- fixtures

fn json(text: &str) -> Json {
    parse_json(text).unwrap_or_else(|error| panic!("fixture JSON {text}: {error}"))
}

fn s(text: &str) -> Json {
    Json::string(text)
}

/// `value` with member `key` replaced (or appended), or removed for `None`.
fn with(value: &Json, key: &str, member: Option<Json>) -> Json {
    let Json::Object(pairs) = value else {
        panic!("fixture is not an object")
    };
    let mut pairs = pairs.clone();
    match member {
        Some(member) => match pairs.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = member,
            None => pairs.push((key.to_owned(), member)),
        },
        None => pairs.retain(|(name, _)| name != key),
    }
    Json::Object(pairs)
}

fn base_policy() -> Json {
    json(&format!(
        r#"{{"max_per_payment_usd":"0.50","allowed_networks":["eip155:84532"],
            "allowed_assets":{{"eip155:84532":["{USDC}"]}},
            "allowed_recipients":["{MERCHANT}"],"allowed_origins":["{ORIGIN}"],
            "allowed_schemes":["exact"]}}"#
    ))
}

fn policy_with(key: &str, member: Option<Json>) -> Json {
    with(&base_policy(), key, member)
}

/// The well-formed $0.10 entry for the approved merchant.
fn entry() -> Json {
    json(&format!(
        r#"{{"scheme":"exact","network":"eip155:84532","asset":"{USDC}","payTo":"{MERCHANT}",
            "amount":"100000","extra":{{"nonce":"abc123"}}}}"#
    ))
}

fn entry_with(key: &str, member: Option<Json>) -> Json {
    with(&entry(), key, member)
}

fn challenge_of(entries: Vec<Json>) -> Json {
    Json::object(vec![
        ("x402Version", Json::Int(1)),
        ("resource", Json::object(vec![("url", s(URL))])),
        ("accepts", Json::Array(entries)),
    ])
}

/// A well-formed x402 v1 challenge for $0.10 to the approved merchant.
fn challenge() -> Json {
    challenge_of(vec![entry()])
}

fn challenge_entry(key: &str, member: Option<Json>) -> Json {
    challenge_of(vec![entry_with(key, member)])
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect("fixture address")
}

fn resolver() -> FixedResolver {
    FixedResolver::new()
        .with_host("sandbox.node4all.com", vec![ip("93.184.216.34")])
        .with_host("example.com", vec![ip("93.184.216.35")])
        .with_host("merchant.test", vec![ip("93.184.216.36")])
}

fn resources() -> Resources {
    Resources {
        payment_manager_arn: Some(
            "arn:aws:bedrock-agentcore:us-west-2:111122223333:payment-manager/pm-1".to_owned(),
        ),
        payment_instrument_id: Some("pi-1".to_owned()),
        payment_session_id: Some(SESSION.to_owned()),
        user_id: Some("agents-pay-0123456789ab".to_owned()),
    }
}

fn request<'a>(
    url: &'a str,
    challenge: &'a Json,
    policy: &'a Json,
    resources: &'a Resources,
) -> PaymentRequest<'a> {
    PaymentRequest {
        url,
        challenge,
        policy,
        resources,
        purchase_id: None,
    }
}

fn run(url: &str, challenge: &Json, policy: &Json) -> Result<Approved, PolicyError> {
    authorize(&request(url, challenge, policy, &resources()), &resolver())
}

fn refused(text: &str) -> PolicyError {
    PolicyError::Refused(text.to_owned())
}

fn reason<T>(result: Result<T, PolicyError>) -> String {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => error.reason(),
    }
}

/// The spending fake: the stand-in for `agentcore::process_payment`. It
/// consumes the `Approved` by value and records the token it would send.
#[derive(Default)]
struct SpendLog {
    tokens: Vec<String>,
}

impl SpendLog {
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the stand-in keeps the by-value signature of the real spending call"
    )]
    fn process_payment(&mut self, approved: Approved) {
        self.tokens.push(approved.token().to_owned());
    }
}

/// Gate then spend, the order every caller must keep.
fn gate_then_spend(
    request: &PaymentRequest<'_>,
    resolver: &FixedResolver,
    log: &mut SpendLog,
) -> Result<(), PolicyError> {
    let approved = authorize(request, resolver)?;
    log.process_payment(approved);
    Ok(())
}

// ------------------------------------------------- RecipientAndValue

#[test]
fn recipient_and_value_amount_above_ceiling_is_refused() {
    let ch = challenge_entry("amount", Some(s("5000000")));
    let got = select_accept_entry(&ch, &base_policy());
    assert_eq!(got.err(), Some(refused(UNIFORM_REFUSAL)));
}

#[test]
fn recipient_and_value_amount_at_ceiling_is_allowed() {
    let ch = challenge_entry("amount", Some(s("500000")));
    let entry = select_accept_entry(&ch, &base_policy()).unwrap();
    assert_eq!(entry.get("payTo"), Some(&s(MERCHANT)));
}

#[test]
fn recipient_and_value_zero_and_negative_amounts_are_refused() {
    for bad in ["0", "-100000"] {
        let ch = challenge_entry("amount", Some(s(bad)));
        assert!(select_accept_entry(&ch, &base_policy()).is_err(), "{bad}");
    }
}

#[test]
fn recipient_and_value_unapproved_network_asset_or_scheme_is_refused() {
    for (key, value) in [
        ("network", "eip155:1"),
        ("asset", ATTACKER),
        ("scheme", "upto"),
    ] {
        let ch = challenge_entry(key, Some(s(value)));
        assert!(select_accept_entry(&ch, &base_policy()).is_err(), "{key}");
    }
}

#[test]
fn recipient_and_value_explicitly_empty_scheme_list_refuses_every_payment() {
    let policy = policy_with("allowed_schemes", Some(json("[]")));
    let ch = challenge();
    let got = select_accept_entry(&ch, &policy);
    assert_eq!(
        got.err(),
        Some(refused("Payment policy allows no schemes."))
    );
    // A null list is present, so it is not the default either.
    let policy = policy_with("allowed_schemes", Some(Json::Null));
    let ch = challenge();
    let got = select_accept_entry(&ch, &policy);
    assert_eq!(
        got.err(),
        Some(refused("Payment policy allows no schemes."))
    );
}

#[test]
fn recipient_and_value_absent_scheme_list_uses_the_exact_scheme_default() {
    let policy = policy_with("allowed_schemes", None);
    let ch = challenge();
    let entry = select_accept_entry(&ch, &policy).unwrap();
    assert_eq!(entry.get("scheme"), Some(&s("exact")));
    let ch = challenge_entry("scheme", Some(s("upto")));
    assert_eq!(
        select_accept_entry(&ch, &policy).err(),
        Some(refused(UNIFORM_REFUSAL))
    );
}

#[test]
fn recipient_and_value_null_amount_uses_max_amount_required() {
    let ch = challenge_of(vec![with(
        &entry_with("amount", Some(Json::Null)),
        "maxAmountRequired",
        Some(s("100000")),
    )]);
    let entry = select_accept_entry(&ch, &base_policy()).unwrap();
    assert_eq!(entry.get("maxAmountRequired"), Some(&s("100000")));
}

#[test]
fn recipient_and_value_conflicting_amount_fields_are_refused() {
    for (amount, maximum) in [("1", "50000000"), ("50000000", "1")] {
        let ch = challenge_of(vec![with(
            &entry_with("amount", Some(s(amount))),
            "maxAmountRequired",
            Some(s(maximum)),
        )]);
        assert!(select_accept_entry(&ch, &base_policy()).is_err());
    }
}

#[test]
fn recipient_and_value_equal_amount_fields_are_allowed() {
    let ch = challenge_of(vec![with(&entry(), "maxAmountRequired", Some(s("100000")))]);
    let entry = select_accept_entry(&ch, &base_policy()).unwrap();
    assert_eq!(entry.get("amount"), entry.get("maxAmountRequired"));
}

#[test]
fn recipient_and_value_checksum_capitalization_still_matches() {
    let ch = challenge_entry("asset", Some(s(USDC_LOWER)));
    assert!(select_accept_entry(&ch, &base_policy()).is_ok());
    let ch = challenge_entry("asset", Some(s(&USDC.to_uppercase())));
    assert!(select_accept_entry(&ch, &base_policy()).is_ok());
}

#[test]
fn recipient_and_value_does_not_blindly_take_first_accepts_entry() {
    let hostile = entry_with("amount", Some(s("5000000")));
    let ch = challenge_of(vec![hostile, entry()]);
    let entry = select_accept_entry(&ch, &base_policy()).unwrap();
    assert_eq!(entry.get("amount"), Some(&s("100000")));
}

#[test]
fn recipient_and_value_missing_required_fields_are_refused() {
    for field in ["scheme", "network", "asset", "payTo"] {
        for member in [None, Some(Json::Null), Some(s(""))] {
            let ch = challenge_entry(field, member.clone());
            assert_eq!(
                select_accept_entry(&ch, &base_policy()).err(),
                Some(refused(UNIFORM_REFUSAL)),
                "{field} {member:?}"
            );
        }
    }
}

#[test]
fn recipient_and_value_refusal_message_does_not_echo_challenge_values() {
    let ch = challenge_of(vec![with(
        &entry_with("asset", Some(s(ATTACKER))),
        "amount",
        Some(s("5000000")),
    )]);
    let message = reason(select_accept_entry(&ch, &base_policy()));
    assert!(!message.contains(ATTACKER));
    assert!(!message.contains("5000000"));
}

// ----------------------------------------------- RecipientValidation

#[test]
fn recipient_validation_unknown_recipient_is_refused() {
    let ch = challenge_entry("payTo", Some(s(ATTACKER)));
    assert_eq!(
        select_accept_entry(&ch, &base_policy()).err(),
        Some(refused(UNIFORM_REFUSAL))
    );
}

#[test]
fn recipient_validation_known_recipient_is_accepted() {
    let ch = challenge_entry("payTo", Some(s(MERCHANT)));
    let entry = select_accept_entry(&ch, &base_policy()).unwrap();
    assert_eq!(entry.get("payTo"), Some(&s(MERCHANT)));
}

#[test]
fn recipient_validation_missing_recipient_allowlist_denies_by_default() {
    let policy = policy_with("allowed_recipients", None);
    assert_eq!(
        select_accept_entry(&challenge(), &policy).err(),
        Some(refused("Payment policy allows no recipients."))
    );
    let policy = policy_with("allowed_recipients", Some(json("[]")));
    assert_eq!(
        select_accept_entry(&challenge(), &policy).err(),
        Some(refused("Payment policy allows no recipients."))
    );
}

#[test]
fn recipient_validation_recipient_match_is_case_insensitive() {
    let upper = MERCHANT.to_uppercase();
    let policy = policy_with("allowed_recipients", Some(Json::Array(vec![s(&upper)])));
    let ch = challenge_entry(
        "payTo",
        Some(s("0X1111111111111111111111111111111111111111")),
    );
    assert!(select_accept_entry(&ch, &policy).is_ok());
    let ch = challenge_entry("payTo", Some(s(MERCHANT)));
    let entry = select_accept_entry(&ch, &policy).unwrap();
    assert_eq!(entry.get("payTo"), Some(&s(MERCHANT)));
}

#[test]
fn recipient_validation_allow_any_recipient_accepts_an_unlisted_payee() {
    let policy = with(
        &policy_with("allowed_recipients", None),
        "allow_any_recipient",
        Some(Json::Bool(true)),
    );
    let ch = challenge_entry("payTo", Some(s(ATTACKER)));
    let entry = select_accept_entry(&ch, &policy).unwrap();
    assert_eq!(entry.get("payTo"), Some(&s(ATTACKER)));
}

#[test]
fn recipient_validation_recipient_modes_are_mutually_exclusive() {
    for value in [true, false] {
        let policy = policy_with("allow_any_recipient", Some(Json::Bool(value)));
        assert_eq!(
            select_accept_entry(&challenge(), &policy).err(),
            Some(refused(
                "Payment policy 'allow_any_recipient' and 'allowed_recipients' are mutually exclusive."
            )),
            "{value}"
        );
    }
}

#[test]
fn recipient_validation_allow_any_recipient_requires_a_boolean() {
    for value in [s("true"), Json::Int(1), Json::Null] {
        let policy = with(
            &policy_with("allowed_recipients", None),
            "allow_any_recipient",
            Some(value.clone()),
        );
        assert_eq!(
            select_accept_entry(&challenge(), &policy).err(),
            Some(refused(
                "Payment policy 'allow_any_recipient' must be a boolean."
            )),
            "{value:?}"
        );
    }
}

#[test]
fn recipient_validation_allow_any_recipient_keeps_other_policy_checks() {
    let policy = with(
        &policy_with("allowed_recipients", None),
        "allow_any_recipient",
        Some(Json::Bool(true)),
    );
    for (key, value) in [
        ("network", "eip155:1"),
        ("asset", ATTACKER),
        ("scheme", "upto"),
        ("amount", "500001"),
    ] {
        let ch = challenge_of(vec![with(
            &entry_with("payTo", Some(s(ATTACKER))),
            key,
            Some(s(value)),
        )]);
        assert_eq!(
            select_accept_entry(&ch, &policy).err(),
            Some(refused(UNIFORM_REFUSAL)),
            "{key}"
        );
    }
}

#[test]
fn recipient_validation_network_asset_and_scheme_are_still_validated_for_known_recipient() {
    for (key, value) in [
        ("network", "eip155:1"),
        ("asset", ATTACKER),
        ("scheme", "upto"),
    ] {
        let ch = challenge_entry(key, Some(s(value)));
        assert_eq!(
            select_accept_entry(&ch, &base_policy()).err(),
            Some(refused(UNIFORM_REFUSAL)),
            "{key}"
        );
    }
}

// ------------------------------------------------- policy shape (R-POL)

#[test]
fn r_pol_policy_shape_errors_keep_the_python_texts_and_order() {
    let cases = [
        (
            policy_with("allowed_networks", Some(s("eip155:84532"))),
            "Payment policy 'allowed_networks' must be a list.",
        ),
        (
            policy_with("allowed_schemes", Some(s("exact"))),
            "Payment policy 'allowed_schemes' must be a list.",
        ),
        (
            policy_with("allowed_recipients", Some(s(MERCHANT))),
            "Payment policy 'allowed_recipients' must be a list.",
        ),
        (
            policy_with("allowed_assets", Some(json("[1]"))),
            "Payment policy 'allowed_assets' must be an object.",
        ),
        (
            policy_with("allowed_networks", None),
            "Payment policy allows no networks.",
        ),
        (
            policy_with("allowed_networks", Some(json("[]"))),
            "Payment policy allows no networks.",
        ),
        (
            policy_with("max_per_payment_usd", None),
            "Payment policy has no per-payment ceiling ('max_per_payment_usd'). Refusing to pay: \
             the session budget is cumulative and does not bound a single transaction.",
        ),
        (
            policy_with("max_per_payment_usd", Some(s("abc"))),
            "Payment policy 'max_per_payment_usd' is not a decimal number.",
        ),
        (
            policy_with("max_per_payment_usd", Some(s("Infinity"))),
            "Payment policy 'max_per_payment_usd' is not a decimal number.",
        ),
        (
            policy_with("max_per_payment_usd", Some(Json::Null)),
            "Payment policy 'max_per_payment_usd' is not a decimal number.",
        ),
        (
            policy_with("max_per_payment_usd", Some(s("0"))),
            "Payment policy 'max_per_payment_usd' must be greater than zero.",
        ),
        (
            with(
                &policy_with("max_per_payment_usd", None),
                "max_amount_usd",
                Some(s("-1")),
            ),
            "Payment policy 'max_amount_usd' must be greater than zero.",
        ),
    ];
    for (policy, text) in cases {
        assert_eq!(
            select_accept_entry(&challenge(), &policy).err(),
            Some(refused(text)),
            "{text}"
        );
    }
    // A list type error is reported before an empty list (Python order).
    let policy = with(
        &policy_with("allowed_networks", Some(json("[]"))),
        "allowed_schemes",
        Some(s("exact")),
    );
    assert_eq!(
        select_accept_entry(&challenge(), &policy).err(),
        Some(refused("Payment policy 'allowed_schemes' must be a list."))
    );
}

#[test]
fn r_pol_the_earlier_cap_key_is_honoured_and_the_current_key_wins() {
    let policy = with(
        &policy_with("max_per_payment_usd", None),
        "max_amount_usd",
        Some(s("0.05")),
    );
    assert_eq!(
        select_accept_entry(&challenge(), &policy).err(),
        Some(refused(UNIFORM_REFUSAL))
    );
    let ch = challenge_entry("amount", Some(s("50000")));
    assert!(select_accept_entry(&ch, &policy).is_ok());
    let both = with(&base_policy(), "max_amount_usd", Some(s("0.01")));
    assert!(select_accept_entry(&challenge(), &both).is_ok());
}

#[test]
fn r_pol_challenge_without_accepts_entries_is_refused() {
    for accepts in [
        None,
        Some(json("[]")),
        Some(s("x")),
        Some(json("{\"a\":1}")),
    ] {
        let ch = with(&challenge(), "accepts", accepts.clone());
        assert_eq!(
            select_accept_entry(&ch, &base_policy()).err(),
            Some(refused("Payment challenge has no accepts entries.")),
            "{accepts:?}"
        );
    }
}

#[test]
fn r_pol_d3_non_string_assets_give_the_fixed_policy_error() {
    let text = "Payment policy 'allowed_assets' must map networks to lists of strings.";
    for assets in [
        r#"{"eip155:84532":[1]}"#,
        r#"{"eip155:84532":[null]}"#,
        r#"{"eip155:84532":"0x036cbd53842c5426634e7929541ec2318f3dcf7e"}"#,
        r#"{"eip155:84532":null}"#,
    ] {
        let policy = policy_with("allowed_assets", Some(json(assets)));
        assert_eq!(
            select_accept_entry(&challenge(), &policy).err(),
            Some(refused(text)),
            "{assets}"
        );
    }
    // Falsy `allowed_assets` is an empty mapping, so nothing matches.
    for assets in ["{}", "null", "[]", "0"] {
        let policy = policy_with("allowed_assets", Some(json(assets)));
        assert_eq!(
            select_accept_entry(&challenge(), &policy).err(),
            Some(refused(UNIFORM_REFUSAL)),
            "{assets}"
        );
    }
}

#[test]
fn r_pol_scalars_compare_through_python_str() {
    // An integer network matches a policy string written the Python way.
    let policy = with(
        &policy_with("allowed_networks", Some(json("[\"7\"]"))),
        "allowed_assets",
        Some(json(&format!(r#"{{"7":["{USDC}"]}}"#))),
    );
    let ch = challenge_entry("network", Some(Json::Int(7)));
    // D4 refuses the family after selection, which proves selection passed.
    assert_eq!(
        reason(run(URL, &ch, &policy)),
        "Payment network family is not supported by this runtime."
    );
    // D2: an array field is never eligible.
    let ch = challenge_entry("scheme", Some(json("[\"exact\"]")));
    assert_eq!(
        select_accept_entry(&ch, &base_policy()).err(),
        Some(refused(UNIFORM_REFUSAL))
    );
}

// ------------------------------------------------------------- T-POL-UNI

/// Every skip class, each as a one-entry challenge.
fn skip_classes() -> Vec<(&'static str, Json)> {
    let mut cases = vec![
        ("not an object", challenge_of(vec![s("exact")])),
        ("array entry", challenge_of(vec![json("[1]")])),
        ("scheme", challenge_entry("scheme", Some(s("upto")))),
        ("network", challenge_entry("network", Some(s("eip155:1")))),
        ("asset", challenge_entry("asset", Some(s(ATTACKER)))),
        ("payTo", challenge_entry("payTo", Some(s(ATTACKER)))),
        ("over cap", challenge_entry("amount", Some(s("5000000")))),
        ("zero", challenge_entry("amount", Some(s("0")))),
        ("exotic", challenge_entry("amount", Some(s("1E+7")))),
        ("float", challenge_entry("amount", Some(json("100000.0")))),
        ("no amount", challenge_entry("amount", None)),
        (
            "conflict",
            challenge_of(vec![with(
                &entry(),
                "maxAmountRequired",
                Some(s("5000001")),
            )]),
        ),
        (
            "D2 object",
            challenge_entry("payTo", Some(json("{\"a\":1}"))),
        ),
        (
            "D11 timeout",
            challenge_entry("maxTimeoutSeconds", Some(Json::Int(0))),
        ),
        ("D11 extra", challenge_entry("extra", Some(s("x")))),
    ];
    for field in ["scheme", "network", "asset", "payTo"] {
        cases.push(("missing", challenge_entry(field, None)));
        cases.push(("null", challenge_entry(field, Some(Json::Null))));
        cases.push(("empty", challenge_entry(field, Some(s("")))));
    }
    cases
}

#[test]
fn t_pol_uni_every_skip_class_gives_the_byte_identical_uniform_text() {
    assert_eq!(
        UNIFORM_REFUSAL,
        "No payment option in this challenge satisfies the configured policy (scheme, network, \
         asset, recipient, and amount ceiling are all enforced). Refusing to pay."
    );
    for (class, ch) in skip_classes() {
        assert_eq!(
            select_accept_entry(&ch, &base_policy()).err(),
            Some(refused(UNIFORM_REFUSAL)),
            "{class}"
        );
        assert_eq!(
            reason(run(URL, &ch, &base_policy())),
            UNIFORM_REFUSAL,
            "{class}"
        );
    }
}

#[test]
fn t_pol_uni_the_text_carries_no_challenge_value() {
    for needle in [
        ATTACKER, "upto", "eip155:1", "5000000", "1E+7", USDC, MERCHANT, "0x",
    ] {
        assert!(!UNIFORM_REFUSAL.contains(needle), "{needle}");
    }
}

// --------------------------------------------------------- OptionalOrigin

#[test]
fn optional_origin_no_origin_list_allows_any_public_https_site() {
    let policy = policy_with("allowed_origins", None);
    let url = "https://example.com/paid";
    let ch = with(
        &challenge(),
        "resource",
        Some(Json::object(vec![("url", s(url))])),
    );
    assert!(run(url, &ch, &policy).is_ok());
    assert_eq!(
        assert_public_https_url(url, &resolver()),
        Ok("https://example.com".to_owned())
    );
    let empty = policy_with("allowed_origins", Some(json("[]")));
    assert!(run(url, &ch, &empty).is_ok());
}

#[test]
fn optional_origin_an_origin_list_still_pins_when_provided() {
    let url = "https://example.com/paid";
    let ch = with(
        &challenge(),
        "resource",
        Some(Json::object(vec![("url", s(url))])),
    );
    assert_eq!(
        reason(run(url, &ch, &base_policy())),
        "Origin https://example.com is not in the configured allowed_origins."
    );
}

#[test]
fn optional_origin_entries_are_lowercased_and_trailing_slashes_stripped() {
    let policy = policy_with(
        "allowed_origins",
        Some(json("[\"HTTPS://SANDBOX.node4all.com/\"]")),
    );
    assert!(run(URL, &challenge(), &policy).is_ok());
    let upper = "https://SANDBOX.node4all.com:443/v1/x402-test";
    let ch = with(
        &challenge(),
        "resource",
        Some(Json::object(vec![("url", s(upper))])),
    );
    assert!(run(upper, &ch, &policy).is_ok());
}

#[test]
fn optional_origin_a_bracketed_ipv6_entry_never_matches() {
    let url = "https://[2606:4700::1]/x";
    let policy = policy_with(
        "allowed_origins",
        Some(json("[\"https://[2606:4700::1]\"]")),
    );
    let ch = with(
        &challenge(),
        "resource",
        Some(Json::object(vec![("url", s(url))])),
    );
    assert_eq!(
        reason(run(url, &ch, &policy)),
        "Origin https://2606:4700::1 is not in the configured allowed_origins."
    );
    let bare = policy_with("allowed_origins", Some(json("[\"https://2606:4700::1\"]")));
    assert!(run(url, &ch, &bare).is_ok());
}

#[test]
fn optional_origin_allowed_origins_must_be_a_list() {
    let policy = policy_with("allowed_origins", Some(s(ORIGIN)));
    assert_eq!(
        reason(run(URL, &challenge(), &policy)),
        "Payment policy 'allowed_origins' must be a list."
    );
}

#[test]
fn optional_origin_the_refused_origin_is_a_shown_value() {
    // The origin is a caller value: redacted, then bounded to 200
    // characters (`shown_value`, #4941); the JSON writer escapes it.
    let host = format!("{}.example", "a".repeat(250));
    let url = format!("https://{host}/x");
    let resolver = resolver().with_host(&host, vec![ip("93.184.216.37")]);
    let ch = challenge();
    let got = authorize(&request(&url, &ch, &base_policy(), &resources()), &resolver);
    let origin = format!("https://{host}");
    let shown: String = origin.chars().take(200).collect();
    assert_eq!(
        reason(got),
        format!("Origin {shown}... is not in the configured allowed_origins.")
    );
}

#[test]
fn optional_origin_mandatory_ssrf_controls_apply_even_with_no_origin_list() {
    let policy = policy_with("allowed_origins", None);
    for url in [
        "http://example.com/x",
        "https://user:pw@example.com/x",
        "https://169.254.169.254/x",
        "https://127.0.0.1/x",
        "https://100.64.0.1/x",
        "https://[::1]/x",
        "https://[::ffff:127.0.0.1]/x",
    ] {
        assert!(run(url, &challenge(), &policy).is_err(), "{url}");
    }
}

// --------------------------------------------------------------- R-ORIGIN

#[test]
fn r_origin_failures_keep_the_python_texts() {
    let resolver = resolver()
        .with_host("private.example", vec![ip("93.184.216.40"), ip("10.0.0.7")])
        .with_host("empty.example", vec![])
        .with_host("doc.example", vec![ip("192.0.2.1")]);
    let cases = [
        (
            "http://example.com/x",
            "Only https:// payment URLs are allowed.",
        ),
        (
            "ftp://example.com/x",
            "Only https:// payment URLs are allowed.",
        ),
        ("example.com/x", "Only https:// payment URLs are allowed."),
        ("https:///x", "Payment URL has no host."),
        (
            "https://u:p@example.com/x",
            "Payment URL must not contain embedded credentials.",
        ),
        (
            "https://u@example.com/x",
            "Payment URL must not contain embedded credentials.",
        ),
        (
            "https://unknown.example/x",
            "Payment URL hostname does not resolve.",
        ),
        (
            "https://empty.example/x",
            "Payment URL hostname does not resolve.",
        ),
        (
            "https://private.example/x",
            "Payment URL resolves to a non-public address.",
        ),
        (
            "https://doc.example/x",
            "Payment URL resolves to a non-public address.",
        ),
        (
            "https://10.1.2.3/x",
            "Payment URL resolves to a non-public address.",
        ),
    ];
    for (url, text) in cases {
        assert_eq!(
            assert_public_https_url(url, &resolver),
            Err(refused(text)),
            "{url}"
        );
    }
    // Python raises ValueError from urlparse or from `port`.
    for url in [
        "https://example.com:99999/x",
        "https://[::1/x",
        "https://example.com:x/",
    ] {
        assert_eq!(
            assert_public_https_url(url, &resolver),
            Err(PolicyError::ValueError),
            "{url}"
        );
    }
    assert_eq!(
        PolicyError::ValueError.reason(),
        "ValueError during payment flow."
    );
    // An address literal is judged without a lookup.
    let quiet = FixedResolver::new();
    assert_eq!(
        assert_public_https_url("https://93.184.216.34/x", &quiet),
        Ok("https://93.184.216.34".to_owned())
    );
    assert!(quiet.lookups().is_empty());
}

#[test]
fn r_origin_every_answer_is_checked_and_the_origin_is_canonical() {
    let resolver = resolver().with_host(
        "merchant.test",
        vec![ip("93.184.216.36"), ip("2606:4700::1")],
    );
    assert_eq!(
        assert_public_https_url("https://MERCHANT.test:8443/a", &resolver),
        Ok("https://merchant.test:8443".to_owned())
    );
    assert_eq!(resolver.lookups(), vec!["merchant.test".to_owned()]);
}

// -------------------------------------------------------------- Authorize

#[test]
fn authorize_approved_payment_returns_full_decision() {
    let approved = run(URL, &challenge(), &base_policy()).unwrap();
    assert_eq!(approved.amount_usd(), "0.1");
    assert_eq!(approved.amount(), "100000");
    assert_eq!(approved.entry().get("payTo"), Some(&s(MERCHANT)));
    assert_eq!(approved.version(), 1);
    assert_eq!(approved.token().len(), 64);
    assert!(approved
        .token()
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(
        approved.resource(),
        Some(&Json::object(vec![("url", s(URL))]))
    );
    assert_eq!(approved.session_id(), SESSION);
    assert_eq!(approved.instrument_id(), "pi-1");
    assert_eq!(approved.user_id(), "agents-pay-0123456789ab");
    assert_eq!(
        approved.manager_arn(),
        "arn:aws:bedrock-agentcore:us-west-2:111122223333:payment-manager/pm-1"
    );
}

#[test]
fn authorize_resource_none_when_challenge_omits_it() {
    let ch = with(&challenge(), "resource", None);
    assert_eq!(run(URL, &ch, &base_policy()).unwrap().resource(), None);
}

#[test]
fn authorize_resource_url_mismatch_is_refused() {
    let text = "Challenge resource.url does not match the requested URL.";
    for other in [
        "https://attacker.example/evil",
        "https://sandbox.node4all.com/v1/other",
        "http://sandbox.node4all.com/v1/x402-test",
        "https://SANDBOX.node4all.com/v1/x402-test",
        "https://sandbox.node4all.com:443/v1/x402-test",
    ] {
        let ch = with(
            &challenge(),
            "resource",
            Some(Json::object(vec![("url", s(other))])),
        );
        assert_eq!(reason(run(URL, &ch, &base_policy())), text, "{other}");
    }
}

#[test]
fn authorize_resource_binding_ignores_query_params_and_a_trailing_slash() {
    for same in [
        "https://sandbox.node4all.com/v1/x402-test?utm=1",
        "https://sandbox.node4all.com/v1/x402-test/",
        "https://sandbox.node4all.com/v1/x402-test;v=2",
        "https://sandbox.node4all.com/v1/x402-test#frag",
    ] {
        let ch = with(
            &challenge(),
            "resource",
            Some(Json::object(vec![("url", s(same))])),
        );
        let approved = run(URL, &ch, &base_policy()).unwrap();
        // The sanitized resource keeps the publisher's URL text.
        assert_eq!(
            approved.resource(),
            Some(&Json::object(vec![("url", s(same))]))
        );
    }
    assert_eq!(
        validated_resource(&challenge(), URL),
        Ok(Some(Json::object(vec![("url", s(URL))])))
    );
}

#[test]
fn authorize_resource_extra_fields_are_stripped() {
    let resource = json(&format!(
        r#"{{"url":"{URL}","injected":"payload","nested":{{"x":1}}}}"#
    ));
    let ch = with(&challenge(), "resource", Some(resource));
    let approved = run(URL, &ch, &base_policy()).unwrap();
    assert_eq!(
        approved.resource(),
        Some(&Json::object(vec![("url", s(URL))]))
    );
}

#[test]
fn authorize_resource_non_dict_or_non_string_url_is_ignored() {
    for resource in [
        s("not-a-dict"),
        json("[1]"),
        json("{\"url\":12345}"),
        json("{}"),
    ] {
        let ch = with(&challenge(), "resource", Some(resource.clone()));
        assert_eq!(
            run(URL, &ch, &base_policy()).unwrap().resource(),
            None,
            "{resource:?}"
        );
    }
}

#[test]
fn authorize_non_dict_challenge_is_refused() {
    for bad in ["\"[]\"", "null", "5", "[]"] {
        assert_eq!(
            reason(run(URL, &json(bad), &base_policy())),
            "Payment challenge is not a JSON object.",
            "{bad}"
        );
    }
}

#[test]
fn authorize_exotic_amount_in_a_challenge_is_refused_not_paid() {
    for bad in ["1E+7", "Infinity", "500000.0"] {
        let ch = challenge_entry("amount", Some(s(bad)));
        assert_eq!(
            reason(run(URL, &ch, &base_policy())),
            UNIFORM_REFUSAL,
            "{bad}"
        );
    }
}

#[test]
fn authorize_missing_resources_are_refused_after_the_policy_in_python_order() {
    let all = resources();
    let cases = [
        (
            Resources {
                payment_manager_arn: None,
                ..all.clone()
            },
            "payment_manager_arn",
            "PAYMENT_MANAGER_ARN",
        ),
        (
            Resources {
                payment_instrument_id: Some(String::new()),
                ..all.clone()
            },
            "payment_instrument_id",
            "PAYMENT_INSTRUMENT_ID",
        ),
        (
            Resources {
                payment_session_id: None,
                ..all.clone()
            },
            "payment_session_id",
            "PAYMENT_SESSION_ID",
        ),
        (
            Resources {
                user_id: None,
                ..all.clone()
            },
            "user_id",
            "PAYMENT_USER_ID",
        ),
        (
            Resources::default(),
            "payment_manager_arn",
            "PAYMENT_MANAGER_ARN",
        ),
    ];
    let ch = challenge();
    let policy = base_policy();
    for (resources, key, env) in cases {
        let got = authorize(&request(URL, &ch, &policy, &resources), &resolver());
        assert_eq!(
            reason(got),
            format!(
                "Missing payment resource '{key}'. Add it to the config (agents_pay_admin.py \
                 init-config) or set {env}. Provision resources with the trusted setup path first."
            )
        );
    }
    // A policy refusal is reported before a missing resource.
    let over = challenge_entry("amount", Some(s("5000000")));
    let got = authorize(
        &request(URL, &over, &policy, &Resources::default()),
        &resolver(),
    );
    assert_eq!(reason(got), UNIFORM_REFUSAL);
}

#[test]
fn authorize_debug_shows_only_the_token_prefix_and_the_amount() {
    let approved = run(URL, &challenge(), &base_policy()).unwrap();
    let shown = format!("{approved:?}");
    assert!(shown.contains("100000"), "{shown}");
    assert!(shown.contains(&approved.token()[..8]), "{shown}");
    assert!(!shown.contains(approved.token()), "{shown}");
    for secret in [
        SESSION,
        "pi-1",
        "agents-pay-0123456789ab",
        "pm-1",
        MERCHANT,
        "abc123",
    ] {
        assert!(!shown.contains(secret), "{secret} in {shown}");
    }
}

// ------------------------------------------------- D4 and D11 (selection)

#[test]
fn d4_a_non_evm_network_is_refused_after_selection() {
    let network = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
    let policy = with(
        &policy_with("allowed_networks", Some(Json::Array(vec![s(network)]))),
        "allowed_assets",
        Some(json(&format!(r#"{{"{network}":["{USDC}"]}}"#))),
    );
    let ch = challenge_entry("network", Some(s(network)));
    assert!(select_accept_entry(&ch, &policy).is_ok());
    assert_eq!(
        reason(run(URL, &ch, &policy)),
        "Payment network family is not supported by this runtime."
    );
}

#[test]
fn d11_timeout_and_extra_bounds_make_the_entry_ineligible() {
    let big_extra = Json::object(vec![("pad", s(&"a".repeat(4096)))]);
    for (key, value) in [
        ("maxTimeoutSeconds", Json::Int(0)),
        ("maxTimeoutSeconds", Json::Int(3601)),
        ("maxTimeoutSeconds", Json::Int(-1)),
        ("maxTimeoutSeconds", s("60")),
        ("maxTimeoutSeconds", json("60.0")),
        ("maxTimeoutSeconds", Json::Null),
        ("extra", s("x")),
        ("extra", json("[]")),
        ("extra", Json::Null),
        ("extra", big_extra),
    ] {
        let ch = challenge_entry(key, Some(value.clone()));
        assert_eq!(
            select_accept_entry(&ch, &base_policy()).err(),
            Some(refused(UNIFORM_REFUSAL)),
            "{key} {value:?}"
        );
    }
    let at_bound = {
        let fill = 4096 - r#"{"pad":""}"#.len();
        Json::object(vec![("pad", s(&"a".repeat(fill)))])
    };
    assert_eq!(at_bound.to_compact_string().len(), 4096);
    for (key, value) in [
        ("maxTimeoutSeconds", Json::Int(1)),
        ("maxTimeoutSeconds", Json::Int(3600)),
        ("extra", json("{}")),
        ("extra", at_bound),
    ] {
        let ch = challenge_entry(key, Some(value));
        assert!(select_accept_entry(&ch, &base_policy()).is_ok(), "{key}");
    }
    // Absent members are fine.
    let ch = challenge_entry("extra", None);
    assert!(select_accept_entry(&ch, &base_policy()).is_ok());
}

// ------------------------------------------------------------------ T-VER

#[test]
fn t_ver_follows_python_int_with_truthiness_fallbacks() {
    let ok = [
        (r#"{"x402Version":2}"#, 2),
        (r#"{"x402Version":1}"#, 1),
        ("{}", 1),
        (r#"{"version":2}"#, 2),
        (r#"{"x402Version":0,"version":2}"#, 2),
        (r#"{"x402Version":false,"version":2}"#, 2),
        (r#"{"x402Version":null,"version":"3"}"#, 3),
        (r#"{"x402Version":"","version":0}"#, 1),
        (r#"{"x402Version":[]}"#, 1),
        (r#"{"x402Version":"0"}"#, 0),
        (r#"{"x402Version":" 2 "}"#, 2),
        (r#"{"x402Version":2.9}"#, 2),
        (r#"{"x402Version":true}"#, 1),
        (r#"{"x402Version":"-1"}"#, -1),
    ];
    for (text, expected) in ok {
        assert_eq!(x402_version(&json(text)), Ok(expected), "{text}");
    }
    for text in [
        r#"{"x402Version":"abc"}"#,
        r#"{"x402Version":[1]}"#,
        r#"{"x402Version":{"a":1}}"#,
        r#"{"x402Version":"2.0"}"#,
        r#"{"x402Version":"99999999999999999999"}"#,
        r#"{"x402Version":1e300}"#,
    ] {
        assert_eq!(
            x402_version(&json(text)),
            Err(PolicyError::ValueError),
            "{text}"
        );
    }
}

#[test]
fn t_ver_version_errors_surface_through_authorize_as_value_error() {
    let ch = with(&challenge(), "x402Version", Some(s("two")));
    assert_eq!(
        run(URL, &ch, &base_policy()).err(),
        Some(PolicyError::ValueError)
    );
}

#[test]
fn t_ver_ascii_separators_are_refused_without_spending() {
    let policy = base_policy();
    let res = resources();
    for c in '\u{1c}'..='\u{1f}' {
        for version in [format!("{c}2"), format!("2{c}")] {
            let ch = with(&challenge(), "x402Version", Some(s(&version)));
            let mut log = SpendLog::default();
            assert_eq!(
                gate_then_spend(&request(URL, &ch, &policy, &res), &resolver(), &mut log),
                Err(PolicyError::ValueError),
                "{version:?}"
            );
            assert!(log.tokens.is_empty(), "{version:?}");
        }
    }
}

// ------------------------------------------------- SignerInput (pure part)

#[test]
fn signer_input_receives_only_the_vetted_entry() {
    let hostile = json(&format!(
        r#"{{"scheme":"exact","network":"eip155:1","asset":"{ATTACKER}","payTo":"{ATTACKER}","amount":"50000000"}}"#
    ));
    let compliant = json(&format!(
        r#"{{"scheme":"exact","network":"eip155:84532","asset":"{USDC}","payTo":"{MERCHANT}","amount":"100000"}}"#
    ));
    let ch = Json::object(vec![
        ("x402Version", Json::Int(2)),
        ("accepts", Json::Array(vec![hostile, compliant.clone()])),
    ]);
    let approved = run(URL, &ch, &base_policy()).unwrap();
    assert_eq!(approved.entry(), &compliant);
    let blob = approved.entry().to_compact_string();
    assert!(!blob.contains(ATTACKER));
    assert!(!blob.contains("eip155:1\""));
    assert!(!blob.contains("50000000"));
}

#[test]
fn signer_input_receives_only_the_canonical_validated_amount() {
    for (version, expected_key, removed_key) in [
        (1, "maxAmountRequired", "amount"),
        (2, "amount", "maxAmountRequired"),
    ] {
        let ch = with(
            &challenge_of(vec![with(&entry(), "maxAmountRequired", Some(s("100000")))]),
            "x402Version",
            Some(Json::Int(version)),
        );
        let approved = run(URL, &ch, &base_policy()).unwrap();
        assert_eq!(approved.entry().get(expected_key), Some(&s("100000")));
        assert_eq!(approved.entry().get(removed_key), None);
        assert_eq!(approved.amount(), "100000");
        assert_eq!(approved.version(), version);
    }
}

#[test]
fn signer_input_the_canonical_amount_moves_to_the_end_as_a_string() {
    let reordered = json(&format!(
        r#"{{"amount":100000,"scheme":"exact","network":"eip155:84532","asset":"{USDC}","payTo":"{MERCHANT}","extra":{{"n":1}}}}"#
    ));
    let ch = with(
        &challenge_of(vec![reordered]),
        "x402Version",
        Some(Json::Int(2)),
    );
    let approved = run(URL, &ch, &base_policy()).unwrap();
    let Json::Object(pairs) = approved.entry() else {
        panic!("entry is an object")
    };
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        ["scheme", "network", "asset", "payTo", "extra", "amount"]
    );
    assert_eq!(approved.entry().get("amount"), Some(&s("100000")));
    // Leading zeros are kept verbatim in the signed amount, as in Python.
    let ch = challenge_entry("amount", Some(s("0100000")));
    let approved = run(URL, &ch, &base_policy()).unwrap();
    assert_eq!(approved.amount(), "0100000");
    assert_eq!(
        approved.entry().get("maxAmountRequired"),
        Some(&s("0100000"))
    );
    assert_eq!(approved.amount_usd(), "0.1");
}

// ------------------------------------------------------------ Idempotency

fn token_of(url: &str, entry: &Json) -> String {
    derive_client_token(SESSION, url, entry, None).unwrap()
}

#[test]
fn idempotency_same_purchase_yields_same_token() {
    assert_eq!(token_of(URL, &entry()), token_of(URL, &entry()));
    for _ in 0..5 {
        assert_eq!(token_of(URL, &entry()), token_of(URL, &entry()));
    }
}

#[test]
fn idempotency_token_survives_a_rotating_publisher_nonce() {
    let first = entry_with("extra", Some(json("{\"nonce\":\"server-nonce-1\"}")));
    let second = entry_with("extra", Some(json("{\"nonce\":\"server-nonce-2\"}")));
    assert_eq!(token_of(URL, &first), token_of(URL, &second));
    let timeout = entry_with("maxTimeoutSeconds", Some(Json::Int(60)));
    assert_eq!(token_of(URL, &timeout), token_of(URL, &entry()));
}

#[test]
fn idempotency_token_ignores_an_absent_nonce_and_the_query() {
    assert_eq!(
        token_of(URL, &entry_with("extra", None)),
        token_of(URL, &entry())
    );
    assert_eq!(
        token_of(&format!("{URL}?x=1"), &entry()),
        token_of(URL, &entry())
    );
}

#[test]
fn idempotency_explicit_purchase_id_distinguishes_deliberate_repeat_buys() {
    let base = derive_client_token(SESSION, URL, &entry(), None).unwrap();
    let empty = derive_client_token(SESSION, URL, &entry(), Some("")).unwrap();
    let first = derive_client_token(SESSION, URL, &entry(), Some("order-1")).unwrap();
    let second = derive_client_token(SESSION, URL, &entry(), Some("order-2")).unwrap();
    assert_ne!(first, second);
    assert_ne!(base, first);
    assert_eq!(base, empty);
}

#[test]
fn idempotency_terms_and_resource_and_session_change_the_token() {
    let base = token_of(URL, &entry());
    assert_ne!(
        base,
        token_of(URL, &entry_with("amount", Some(s("200000"))))
    );
    assert_ne!(base, token_of(URL, &entry_with("payTo", Some(s(ATTACKER)))));
    assert_ne!(base, token_of(&format!("{ORIGIN}/v1/other"), &entry()));
    assert_ne!(
        base,
        derive_client_token("sess-2", URL, &entry(), None).unwrap()
    );
}

#[test]
fn idempotency_conflicting_amount_aliases_cannot_change_the_signed_value_or_token() {
    let conflicting = with(
        &entry_with("amount", Some(s("1"))),
        "maxAmountRequired",
        Some(s("50000000")),
    );
    assert!(derive_client_token(SESSION, URL, &conflicting, None).is_err());
}

#[test]
fn idempotency_authorization_token_uses_the_resolved_session() {
    let ch = challenge();
    let approved = run(URL, &ch, &base_policy()).unwrap();
    let vetted = with(
        &entry_with("amount", None),
        "maxAmountRequired",
        Some(s("100000")),
    );
    assert_eq!(approved.token(), token_of(URL, &vetted));
    assert_eq!(approved.token(), token_of(URL, &entry()));
    assert_ne!(
        approved.token(),
        derive_client_token("sess-attacker-huge", URL, &entry(), None).unwrap()
    );
    let policy = base_policy();
    let res = resources();
    let purchase = PaymentRequest {
        purchase_id: Some("order-7"),
        ..request(URL, &ch, &policy, &res)
    };
    let approved = authorize(&purchase, &resolver()).unwrap();
    assert_eq!(
        approved.token(),
        derive_client_token(SESSION, URL, &entry(), Some("order-7")).unwrap()
    );
}

// --------------------------------------------------- R-TOK golden vectors

const GOLDEN_PLAIN: &str = "c4293dfd364640b953d5ab169ea31f99eed39e706ace87581a511c65b06c2a87";
const GOLDEN_ORDER_7: &str = "78b5376d8941566a4f28e9e74c1b738a42c651464fd56a4d853c42ddb9ef98cf";
const GOLDEN_PORT: &str = "4cea55bdb8d2335d8bf4a1a5d40a0f1b92e61d8670a47113ad92ebac812110a9";

fn golden_entry() -> Json {
    json(&format!(
        r#"{{"scheme":"exact","network":"eip155:84532","asset":"{USDC_LOWER}","payTo":"{MERCHANT}","amount":"100000"}}"#
    ))
}

#[test]
fn r_tok_golden_vectors_match_byte_for_byte() {
    assert_eq!(
        derive_client_token(
            "ps-1",
            "https://[2606:4700::1%25Eth0]/item",
            &golden_entry(),
            None
        )
        .unwrap(),
        "4f2feeb31c468e7deea344d59da2242eef64144c64616bf727934771c0343598"
    );
    for url in [
        "https://merchant.test/v1/x402-test",
        "https://MERCHANT.test:443/v1/x402-test?x=1",
        "https://merchant.test/v1/x402-test;v=2",
    ] {
        assert_eq!(
            derive_client_token("ps-1", url, &golden_entry(), None).unwrap(),
            GOLDEN_PLAIN,
            "{url}"
        );
    }
    assert_eq!(
        derive_client_token(
            "ps-1",
            "https://merchant.test/v1/x402-test",
            &golden_entry(),
            Some("order-7")
        )
        .unwrap(),
        GOLDEN_ORDER_7
    );
    assert_eq!(
        derive_client_token("ps-1", "https://merchant.test:8443", &golden_entry(), None).unwrap(),
        GOLDEN_PORT
    );
    // The checksum-case asset and the v1 alias derive the same bytes.
    let v1 = with(
        &with(&golden_entry(), "asset", Some(s(USDC))),
        "amount",
        None,
    );
    let v1 = with(&v1, "maxAmountRequired", Some(s("100000")));
    assert_eq!(
        derive_client_token("ps-1", "https://merchant.test/v1/x402-test", &v1, None).unwrap(),
        GOLDEN_PLAIN
    );
}

#[test]
fn r_tok_golden_vectors_hold_through_authorize() {
    let policy = with(
        &policy_with("allowed_origins", None),
        "allowed_recipients",
        Some(Json::Array(vec![s(MERCHANT)])),
    );
    let ch = challenge_of(vec![with(&entry(), "extra", None)]);
    let ch = with(&ch, "resource", None);
    let res = resources();
    for (url, purchase, golden) in [
        ("https://merchant.test/v1/x402-test", None, GOLDEN_PLAIN),
        (
            "https://MERCHANT.test:443/v1/x402-test?x=1",
            None,
            GOLDEN_PLAIN,
        ),
        (
            "https://merchant.test/v1/x402-test",
            Some("order-7"),
            GOLDEN_ORDER_7,
        ),
        ("https://merchant.test:8443", None, GOLDEN_PORT),
    ] {
        let request = PaymentRequest {
            purchase_id: purchase,
            ..request(url, &ch, &policy, &res)
        };
        assert_eq!(
            authorize(&request, &resolver()).unwrap().token(),
            golden,
            "{url}"
        );
    }
}

#[test]
fn r_tok_the_token_fits_the_model_client_token_pattern() {
    let token = derive_client_token("", URL, &entry(), None).unwrap();
    assert!((33..=256).contains(&token.len()));
    assert!(token.bytes().all(|b| b.is_ascii_alphanumeric()));
}

// --------------------------------------------------------------- T-AMT-CAP

const CAP_KEY_TEXT: &str = "Payment policy 'max_per_payment_usd'";

/// A cap spelling: a JSON string, or a JSON number literal.
fn cap_value(spelling: &str) -> Json {
    match spelling.strip_prefix('#') {
        Some(literal) => json(literal),
        None => s(spelling),
    }
}

/// Whether `units` passes the gate under `cap` (through `authorize` over the
/// spending fake). An over-cap row must leave the fake untouched.
fn passes(cap: &str, units: &str) -> bool {
    let policy = policy_with("max_per_payment_usd", Some(cap_value(cap)));
    let ch = challenge_entry("amount", Some(s(units)));
    let mut log = SpendLog::default();
    let res = resources();
    let outcome = gate_then_spend(&request(URL, &ch, &policy, &res), &resolver(), &mut log);
    match outcome {
        Ok(()) => {
            assert_eq!(log.tokens.len(), 1);
            true
        }
        Err(error) => {
            assert_eq!(error, refused(UNIFORM_REFUSAL), "cap {cap} units {units}");
            assert!(log.tokens.is_empty(), "cap {cap} units {units}");
            false
        }
    }
}

/// One T-AMT-CAP row: equivalent spellings (a `#` prefix marks a JSON number
/// literal), the amounts each must allow and the amounts each must refuse.
struct CapRow {
    row: &'static str,
    spellings: Vec<&'static str>,
    allowed: Vec<&'static str>,
    over: Vec<&'static str>,
}

fn cap_row(
    row: &'static str,
    spellings: &[&'static str],
    allowed: &[&'static str],
    over: &[&'static str],
) -> CapRow {
    CapRow {
        row,
        spellings: spellings.to_vec(),
        allowed: allowed.to_vec(),
        over: over.to_vec(),
    }
}

fn cap_rows() -> Vec<CapRow> {
    vec![
        cap_row(
            "T-AMT-CAP-1 $0.10",
            &[
                "0.10",
                "0.1",
                "1e-1",
                "1E-1",
                "+1e-1",
                "10e-2",
                "0.01e1",
                "100000e-6",
                ".1",
                " 0.1 ",
                "1_0e-2",
                "#0.1",
                "#1e-1",
                "#10E-2",
            ],
            &["99999", "100000"],
            &["100001", "1000000"],
        ),
        cap_row(
            "T-AMT-CAP-2 $1",
            &[
                "1", "1.", "1.0", "1e0", "1E+00", "0.1e1", "10e-1", "#1", "#1.0",
            ],
            &["999999", "1000000"],
            &["1000001"],
        ),
        cap_row(
            "T-AMT-CAP-3 $100",
            &["1e2", "0.001e5"],
            &["100000000"],
            &["100000001"],
        ),
        cap_row(
            "T-AMT-CAP-3 $150",
            &["1.5E+2"],
            &["150000000"],
            &["150000001"],
        ),
        cap_row("T-AMT-CAP-4 1e-6", &["1e-6"], &["1"], &["2"]),
        cap_row("T-AMT-CAP-4 5e-7", &["5e-7"], &[], &["1"]),
        cap_row("T-AMT-CAP-4 15e-7", &["15e-7"], &["1"], &["2"]),
    ]
}

#[test]
fn t_amt_cap_1_to_4_every_spelling_is_exact_at_the_unit_boundary() {
    for CapRow {
        row,
        spellings,
        allowed,
        over,
    } in cap_rows()
    {
        for spelling in spellings {
            for units in &allowed {
                assert!(
                    passes(spelling, units),
                    "{row}: {spelling} must allow {units}"
                );
            }
            for units in &over {
                assert!(
                    !passes(spelling, units),
                    "{row}: {spelling} must refuse {units}"
                );
            }
        }
    }
}

#[test]
fn t_amt_cap_5_refusals_give_the_d6_or_cap_le_zero_text() {
    let mantissa = "1".repeat(1001);
    let not_decimal = format!("{CAP_KEY_TEXT} is not a decimal number.");
    let not_positive = format!("{CAP_KEY_TEXT} must be greater than zero.");
    let cases = [
        ("1e-1001", &not_decimal),
        ("1e1001", &not_decimal),
        (mantissa.as_str(), &not_decimal),
        ("-1e-1", &not_positive),
        ("0e5", &not_positive),
        ("-0", &not_positive),
        ("1__0", &not_decimal),
        ("_1", &not_decimal),
        ("1e", &not_decimal),
        ("e1", &not_decimal),
        (".", &not_decimal),
        ("Infinity", &not_decimal),
        ("NaN", &not_decimal),
        ("sNaN", &not_decimal),
        ("#0", &not_positive),
        ("#-0.5", &not_positive),
        ("#true", &not_decimal),
    ];
    for (spelling, text) in cases {
        let policy = policy_with("max_per_payment_usd", Some(cap_value(spelling)));
        let mut log = SpendLog::default();
        let ch = challenge();
        let res = resources();
        let got = gate_then_spend(&request(URL, &ch, &policy, &res), &resolver(), &mut log);
        assert_eq!(got, Err(refused(text)), "{spelling}");
        assert!(log.tokens.is_empty());
    }
}

#[test]
fn t_amt_cap_6_equivalent_spellings_decide_every_amount_identically() {
    let amounts = [
        "1",
        "2",
        "99999",
        "100000",
        "100001",
        "999999",
        "1000000",
        "1000001",
        "100000000",
        "100000001",
        "150000000",
        "150000001",
    ];
    for CapRow { row, spellings, .. } in cap_rows() {
        let decisions: Vec<Vec<bool>> = spellings
            .iter()
            .map(|spelling| {
                amounts
                    .iter()
                    .map(|units| passes(spelling, units))
                    .collect()
            })
            .collect();
        for (spelling, decision) in spellings.iter().zip(&decisions) {
            assert_eq!(
                decision, &decisions[0],
                "{row}: {spelling} differs from {}",
                spellings[0]
            );
        }
    }
}

// ------------------------------------------------------------ T-ORDER-PRE

/// One locally decidable refusal of the policy part.
struct PreCase {
    case: String,
    url: String,
    challenge: Json,
    policy: Json,
    resources: Resources,
    /// The refusal precedes any host lookup.
    early: bool,
}

impl PreCase {
    /// A case over the base policy and the complete resources.
    fn new(case: &str, url: &str, challenge: Json) -> Self {
        Self {
            case: case.to_owned(),
            url: url.to_owned(),
            challenge,
            policy: base_policy(),
            resources: resources(),
            early: false,
        }
    }

    #[must_use]
    fn early(self) -> Self {
        Self {
            early: true,
            ..self
        }
    }

    #[must_use]
    fn policy(self, policy: Json) -> Self {
        Self { policy, ..self }
    }

    #[must_use]
    fn resources(self, resources: Resources) -> Self {
        Self { resources, ..self }
    }
}

fn pre_authorization_refusals() -> Vec<PreCase> {
    let mut cases = vec![
        // R-ORIGIN.
        PreCase::new("http", "http://sandbox.node4all.com/x", challenge()).early(),
        PreCase::new("no host", "https:///x", challenge()).early(),
        PreCase::new(
            "userinfo",
            "https://u:p@sandbox.node4all.com/x",
            challenge(),
        )
        .early(),
        PreCase::new(
            "bad port",
            "https://sandbox.node4all.com:99999/x",
            challenge(),
        )
        .early(),
        PreCase::new("brackets", "https://[::1/x", challenge()).early(),
        PreCase::new("literal", "https://127.0.0.1/x", challenge()).early(),
        PreCase::new("unresolved", "https://unknown.example/x", challenge()),
        PreCase::new("non-public", "https://private.example/x", challenge()),
        PreCase::new("origin", "https://example.com/x", challenge()),
        PreCase::new("origin type", URL, challenge())
            .policy(policy_with("allowed_origins", Some(s(ORIGIN)))),
        // The challenge shape at the gate.
        PreCase::new("not object", URL, json("[]")),
        PreCase::new("no accepts", URL, with(&challenge(), "accepts", None)),
        // R-VER and R-RESBIND.
        PreCase::new(
            "version",
            URL,
            with(&challenge(), "x402Version", Some(s("v2"))),
        ),
        PreCase::new(
            "resource",
            URL,
            with(
                &challenge(),
                "resource",
                Some(Json::object(vec![("url", s("https://attacker.example/x"))])),
            ),
        ),
        // Missing resources.
        PreCase::new("resources", URL, challenge()).resources(Resources {
            payment_session_id: None,
            ..resources()
        }),
        PreCase::new("no resources", URL, challenge()).resources(Resources::default()),
    ];
    // R-POL policy shape.
    for (key, value) in [
        ("allowed_networks", Some(json("[]"))),
        ("allowed_schemes", Some(json("[]"))),
        ("allowed_recipients", None),
        ("allowed_assets", Some(s("x"))),
        ("allow_any_recipient", Some(Json::Bool(true))),
        ("max_per_payment_usd", None),
        ("max_per_payment_usd", Some(s("0"))),
        ("max_per_payment_usd", Some(s("Infinity"))),
    ] {
        cases.push(
            PreCase::new(&format!("policy {key}"), URL, challenge())
                .policy(policy_with(key, value)),
        );
    }
    // Every R-POL skip class (uniform text).
    for (class, ch) in skip_classes() {
        cases.push(PreCase::new(&format!("skip {class}"), URL, ch));
    }
    // D4.
    let solana = "solana:mainnet";
    cases.push(
        PreCase::new("D4", URL, challenge_entry("network", Some(s(solana)))).policy(with(
            &policy_with("allowed_networks", Some(Json::Array(vec![s(solana)]))),
            "allowed_assets",
            Some(json(&format!(r#"{{"{solana}":["{USDC}"]}}"#))),
        )),
    );
    cases
}

#[test]
fn t_order_pre_every_policy_refusal_records_zero_process_payment_calls() {
    let cases = pre_authorization_refusals();
    assert!(
        cases.len() > 40,
        "the refusal table shrank to {}",
        cases.len()
    );
    for case in cases {
        let resolver = resolver().with_host("private.example", vec![ip("10.0.0.1")]);
        let mut log = SpendLog::default();
        let request = request(&case.url, &case.challenge, &case.policy, &case.resources);
        let got = gate_then_spend(&request, &resolver, &mut log);
        let name = &case.case;
        assert!(got.is_err(), "{name} was not refused");
        assert!(log.tokens.is_empty(), "{name} reached ProcessPayment");
        if case.early {
            assert!(resolver.lookups().is_empty(), "{name} looked up a host");
        }
    }
}

#[test]
fn t_order_pre_the_control_case_spends_exactly_once() {
    let mut log = SpendLog::default();
    let ch = challenge();
    let policy = base_policy();
    let res = resources();
    gate_then_spend(&request(URL, &ch, &policy, &res), &resolver(), &mut log).unwrap();
    assert_eq!(log.tokens.len(), 1);
    assert_eq!(
        log.tokens[0],
        derive_client_token(SESSION, URL, &entry(), None).unwrap()
    );
}
