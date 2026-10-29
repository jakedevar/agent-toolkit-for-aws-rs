//! Pure `OpenClaw` policy mapping.

use toolkit_core::Json;

use crate::agents_pay::bridge::protocol::BridgeError;
use crate::agents_pay::urlparse::urlparse;

// recorded-shape: OpenClaw policy is input, not a recorded AWS response.
fn strings(value: &Json) -> Result<Vec<Json>, BridgeError> {
    let Json::Array(items) = value else {
        return Err(BridgeError::Input);
    };
    if items.iter().any(|item| !matches!(item, Json::String(_))) {
        return Err(BridgeError::Input);
    }
    Ok(items.clone())
}

fn valid_cap(cap: &str) -> bool {
    cap.len() <= 1000
        && matches!(cap.as_bytes().first(), Some(b'1'..=b'9'))
        && cap.bytes().all(|c| c.is_ascii_digit())
}

fn origins(input: &Json) -> Result<Vec<Json>, BridgeError> {
    let mut origins = Vec::new();
    if let Some(value) = input.get("allowedOrigins") {
        for origin in strings(value)? {
            let parsed = urlparse(origin.as_str().ok_or(BridgeError::Input)?)
                .map_err(|_| BridgeError::Input)?;
            if parsed.scheme() != "https"
                || parsed.netloc().contains('@')
                || parsed.hostname().is_none_or(|host| host.is_empty())
            {
                return Err(BridgeError::Input);
            }
            origins.push(Json::string(
                parsed
                    .canonical_origin()
                    .map_err(|_| BridgeError::Input)?
                    .to_lowercase(),
            ));
        }
    }
    Ok(origins)
}

/// Convert the trusted glue's profile into the existing policy gate's JSON shape.
///
/// # Errors
/// Returns an input error for malformed or unsupported profile fields.
// recorded-shape: this maps policy input, not a recorded AWS response.
pub fn from_openclaw(input: &Json) -> Result<Json, BridgeError> {
    let Json::Object(fields) = input else {
        return Err(BridgeError::Input);
    };
    let allowed = [
        "networkPreferences",
        "allowedOrigins",
        "allowedRecipients",
        "allowAnyRecipient",
        "allowedAssetsByNetwork",
        "maxPaymentAmountAtomic",
    ];
    if fields
        .iter()
        .any(|(key, _)| !allowed.contains(&key.as_str()))
    {
        return Err(BridgeError::Input);
    }
    let cap = input
        .get("maxPaymentAmountAtomic")
        .and_then(Json::as_str)
        .ok_or(BridgeError::Input)?;
    if !valid_cap(cap) {
        return Err(BridgeError::Input);
    }
    let mut networks = input
        .get("networkPreferences")
        .map(strings)
        .transpose()?
        .unwrap_or_default();
    if networks.is_empty() {
        networks.push(Json::string("eip155:84532"));
    }
    let recipient = input.get("allowedRecipients");
    let any = input.get("allowAnyRecipient");
    if recipient.is_some() && any.is_some() {
        return Err(BridgeError::Input);
    }
    let recipient_field = match (recipient, any) {
        (Some(v), None) => {
            let list = strings(v)?;
            if list.is_empty() {
                return Err(BridgeError::Input);
            }
            ("allowed_recipients", Json::Array(list))
        }
        (None, Some(Json::Bool(true))) => ("allow_any_recipient", Json::Bool(true)),
        _ => return Err(BridgeError::Input),
    };
    let configured = match input.get("allowedAssetsByNetwork") {
        Some(Json::Object(entries)) => {
            for (_, value) in entries {
                strings(value)?;
            }
            Some(entries)
        }
        Some(_) => return Err(BridgeError::Input),
        None => None,
    };
    let mut assets = Vec::new();
    for network in &networks {
        let name = network.as_str().ok_or(BridgeError::Input)?;
        let values = configured
            .and_then(|entries| entries.iter().find(|(key, _)| key == name))
            .map(|(_, value)| strings(value))
            .transpose()?
            .unwrap_or_default();
        let values = if values.is_empty() && name == "eip155:84532" {
            vec![Json::string("0x036cbd53842c5426634e7929541ec2318f3dcf7e")]
        } else {
            values
        };
        if !assets.iter().any(|(key, _)| key == name) {
            assets.push((name.to_owned(), Json::Array(values)));
        }
    }
    let origins = origins(input)?;
    Ok(Json::object(vec![
        ("max_per_payment_usd", Json::string(format!("{cap}e-6"))),
        ("allowed_networks", Json::Array(networks)),
        ("allowed_schemes", Json::Array(vec![Json::string("exact")])),
        ("allowed_assets", Json::Object(assets)),
        ("allowed_origins", Json::Array(origins)),
        recipient_field,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    // T-OCP-MAP and T-OCP-ORIG: cap is atomic and origins discard paths.
    #[test]
    fn t_ocp_map_and_orig() {
        let input = parse_json(r#"{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true,"allowedOrigins":["https://MERCHANT.test:443/path"]}"#).unwrap();
        let policy = from_openclaw(&input).unwrap();
        assert_eq!(
            policy.get("max_per_payment_usd").and_then(Json::as_str),
            Some("100000e-6")
        );
        assert_eq!(
            policy.get("allowed_origins"),
            Some(&Json::Array(vec![Json::string("https://merchant.test")]))
        );
        assert_eq!(
            policy.get("allowed_networks"),
            Some(&Json::Array(vec![Json::string("eip155:84532")]))
        );
        for raw in [
            r#"{"maxPaymentAmountAtomic":"01","allowAnyRecipient":true}"#,
            r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"allowedRecipients":["a"]}"#,
            r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"allowedOrigins":["http://merchant.test"]}"#,
            r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"unknown":1}"#,
        ] {
            assert!(from_openclaw(&parse_json(raw).unwrap()).is_err());
        }
    }

    // T-OCP-PAR, T-OCP-MAP, T-BR-TOK: fixture, controls and atomic cap.
    #[test]
    fn t_ocp_par_and_token() {
        use crate::agents_pay::policy::{derive_client_token, select_accept_entry};
        let fixture = parse_json(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/aws-agents/skills/agents-pay/packages/openclaw/test/fixtures/v2-security-contract.json"))).unwrap();
        let config = fixture.get("config").unwrap();
        let profile = Json::object(
            [
                "networkPreferences",
                "allowedOrigins",
                "allowedRecipients",
                "allowedAssetsByNetwork",
                "maxPaymentAmountAtomic",
            ]
            .iter()
            .map(|key| (*key, config.get(key).unwrap().clone()))
            .collect(),
        );
        let policy = from_openclaw(&profile).unwrap();
        let challenge = fixture.get("challenge").unwrap();
        let selected = select_accept_entry(challenge, &policy).unwrap();
        assert_eq!(selected.get("amount").and_then(Json::as_str), Some("1000"));
        let change = |value: &Json, key: &str, replacement: Json| {
            let Json::Object(fields) = value else {
                panic!("object fixture")
            };
            Json::Object(
                fields
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            if k == key {
                                replacement.clone()
                            } else {
                                v.clone()
                            },
                        )
                    })
                    .collect(),
            )
        };
        for (key, replacement) in [
            ("payTo", "other"),
            ("amount", "100001"),
            ("asset", "other"),
            ("scheme", "upto"),
            ("network", "eip155:1"),
            ("amount", "0"),
            ("amount", "1.0"),
            ("amount", "1e3"),
            ("amount", " 1000 "),
        ] {
            let altered = change(selected, key, Json::string(replacement));
            assert!(
                select_accept_entry(
                    &Json::object(vec![("accepts", Json::Array(vec![altered]))]),
                    &policy
                )
                .is_err(),
                "{key}: {replacement}"
            );
        }
        let url = fixture.get("requestUrl").unwrap().as_str().unwrap();
        let token = derive_client_token("session", url, selected, None).unwrap();
        let changed_nonce = change(
            selected,
            "extra",
            Json::object(vec![("nonce", Json::string("different"))]),
        );
        assert_eq!(
            token,
            derive_client_token("session", url, &changed_nonce, None).unwrap()
        );
        assert_ne!(
            token,
            derive_client_token("other", url, selected, None).unwrap()
        );
        assert_ne!(
            token,
            derive_client_token(
                "session",
                url,
                &change(selected, "amount", Json::string("1001")),
                None
            )
            .unwrap()
        );
    }

    #[test]
    fn t_br_tok_golden_vectors() {
        use crate::agents_pay::policy::derive_client_token;
        let entry = |amount: &str, timeout: i64| {
            Json::object(vec![
                ("network", Json::string("eip155:84532")),
                (
                    "asset",
                    Json::string("0x036cbd53842c5426634e7929541ec2318f3dcf7e"),
                ),
                (
                    "payTo",
                    Json::string("0x1111111111111111111111111111111111111111"),
                ),
                ("amount", Json::string(amount)),
                ("maxTimeoutSeconds", Json::Int(timeout)),
            ])
        };
        let token = |amount, timeout| {
            derive_client_token(
                "payment-session-TEST00000000001",
                "https://merchant.test/pay/weather?city=sydney",
                &entry(amount, timeout),
                None,
            )
            .unwrap()
        };
        assert_eq!(
            token("1000", 60),
            "e36c6d851f2fc37ad61d084f213d568e5c6920da19a3aa2675babb3bcff5c81d"
        );
        assert_eq!(
            token("1001", 60),
            "617369d56a4f010acec1db48579ab07eaf78887e3464e044a68c04c5b6162b3d"
        );
        assert_eq!(token("1000", 60), token("1000", 30));
    }

    #[test]
    fn t_ocp_map_cap_boundaries() {
        use crate::agents_pay::amount::{exceeds_cap, CapDecimal};
        for (maximum, lower, upper) in [
            ("1", "0", "2"),
            ("100000", "99999", "100001"),
            (
                "1000000000000000000000000000000000000000",
                "999999999999999999999999999999999999999",
                "1000000000000000000000000000000000000001",
            ),
        ] {
            let input = Json::object(vec![
                ("maxPaymentAmountAtomic", Json::string(maximum)),
                ("allowAnyRecipient", Json::Bool(true)),
            ]);
            let policy = from_openclaw(&input).unwrap();
            let cap = CapDecimal::from_json(policy.get("max_per_payment_usd").unwrap()).unwrap();
            assert!(!exceeds_cap(lower, &cap));
            assert!(!exceeds_cap(maximum, &cap));
            assert!(exceeds_cap(upper, &cap));
        }
    }

    #[test]
    fn t_ocp_div_strict_entries() {
        use crate::agents_pay::policy::select_accept_entry;
        let policy = from_openclaw(
            &parse_json(r#"{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true}"#).unwrap(),
        )
        .unwrap();
        let original = parse_json(r#"{"scheme":"exact","network":"eip155:84532","asset":"0x036cbd53842c5426634e7929541ec2318f3dcf7e","payTo":"0xa","amount":"1000","maxTimeoutSeconds":60,"extra":{}}"#).unwrap();
        let Json::Object(fields) = original else {
            unreachable!()
        };
        let decide = |extra: (&str, Json)| {
            let mut entry = fields.clone();
            entry.retain(|(key, _)| key != extra.0);
            entry.push((extra.0.into(), extra.1));
            select_accept_entry(
                &Json::object(vec![("accepts", Json::Array(vec![Json::Object(entry)]))]),
                &policy,
            )
            .is_err()
        };
        assert!(decide(("maxTimeoutSeconds", Json::Int(3601))));
        assert!(decide(("extra", Json::string("invalid"))));
        assert!(decide((
            "extra",
            Json::object(vec![("x", Json::string("a".repeat(4096)))])
        )));
        assert!(decide(("maxAmountRequired", Json::string("1001"))));
        assert!(decide(("payTo", Json::Array(vec![]))));
        assert!(parse_json(r#"{"a":1,"a":2}"#).is_err());
    }

    #[test]
    fn t_ocp_par_origins_and_resource() {
        use crate::agents_pay::policy::{authorize, FixedResolver, PaymentRequest, Resources};
        let fixture = parse_json(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/aws-agents/skills/agents-pay/packages/openclaw/test/fixtures/v2-security-contract.json"))).unwrap();
        let config = fixture.get("config").unwrap();
        let profile = Json::object(
            [
                "networkPreferences",
                "allowedOrigins",
                "allowedRecipients",
                "allowedAssetsByNetwork",
                "maxPaymentAmountAtomic",
            ]
            .iter()
            .map(|key| (*key, config.get(key).unwrap().clone()))
            .collect(),
        );
        let policy = from_openclaw(&profile).unwrap();
        let challenge = fixture.get("challenge").unwrap();
        let resources = Resources {
            payment_manager_arn: Some("m".into()),
            payment_instrument_id: Some("i".into()),
            payment_session_id: Some("s".into()),
            user_id: Some("u".into()),
        };
        let resolver =
            FixedResolver::new().with_host("merchant.example", vec!["8.8.8.8".parse().unwrap()]);
        let url = fixture.get("requestUrl").unwrap().as_str().unwrap();
        let gate = |challenge: &Json, policy: &Json, resolver: &FixedResolver| {
            authorize(
                &PaymentRequest {
                    url,
                    challenge,
                    policy,
                    resources: &resources,
                    purchase_id: None,
                },
                resolver,
            )
        };
        assert_eq!(
            gate(challenge, &policy, &resolver).unwrap().amount(),
            "1000"
        );
        let wrong_origin = from_openclaw(&parse_json(r#"{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true,"allowedOrigins":["https://other.example"]}"#).unwrap()).unwrap();
        assert!(gate(challenge, &wrong_origin, &resolver).is_err());
        let private =
            FixedResolver::new().with_host("merchant.example", vec!["127.0.0.1".parse().unwrap()]);
        assert!(gate(challenge, &policy, &private).is_err());
        let Json::Object(members) = challenge else {
            unreachable!()
        };
        for bad_url in [
            "https://evil.example/pay/weather",
            "https://MERCHANT.example/pay/weather?city=sydney",
            "https://merchant.example:443/pay/weather?city=sydney",
        ] {
            let modified = Json::Object(
                members
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            if key == "resource" {
                                Json::object(vec![("url", Json::string(bad_url))])
                            } else {
                                value.clone()
                            },
                        )
                    })
                    .collect(),
            );
            assert!(gate(&modified, &policy, &resolver).is_err());
        }
        let any_policy = from_openclaw(
            &parse_json(r#"{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(
            gate(challenge, &any_policy, &resolver).unwrap().amount(),
            "1"
        );
    }
}
