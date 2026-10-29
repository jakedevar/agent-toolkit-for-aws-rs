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
        for origin in [
            "https://merchant.test/some/path",
            "https://MERCHANT.test",
            "https://merchant.test:443",
        ] {
            let input = Json::object(vec![
                ("maxPaymentAmountAtomic", Json::string("1")),
                ("allowAnyRecipient", Json::Bool(true)),
                ("networkPreferences", Json::Array(vec![])),
                ("allowedOrigins", Json::Array(vec![Json::string(origin)])),
            ]);
            let mapped = from_openclaw(&input).unwrap();
            assert_eq!(
                mapped.get("allowed_origins"),
                Some(&Json::Array(vec![Json::string("https://merchant.test")]))
            );
            assert_eq!(
                mapped.get("allowed_networks"),
                Some(&Json::Array(vec![Json::string("eip155:84532")]))
            );
            assert_eq!(
                mapped.get("allowed_assets").unwrap().get("eip155:84532"),
                Some(&Json::Array(vec![Json::string(
                    "0x036cbd53842c5426634e7929541ec2318f3dcf7e"
                )]))
            );
        }
        let input = parse_json(r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"allowedOrigins":["https://u:p@merchant.test"]}"#).unwrap();
        assert!(from_openclaw(&input).is_err());
    }

    // #5409, T-OCP-MAP / T-BR-REQ-PAY: the recipient control is `allowedRecipients`
    // (non-empty) xor the boolean `true`; `false` alone never opens the control.
    #[test]
    fn t_ocp_map_allow_any_recipient_only_true() {
        let cap = r#""maxPaymentAmountAtomic":"1""#;
        for control in [
            r#""allowAnyRecipient":false"#,
            r#""allowAnyRecipient":"true""#,
            r#""allowAnyRecipient":1"#,
            r#""allowAnyRecipient":null"#,
            r#""allowAnyRecipient":[]"#,
            r#""allowAnyRecipient":false,"allowedRecipients":["a"]"#,
            r#""allowAnyRecipient":true,"allowedRecipients":["a"]"#,
            r#""allowedRecipients":[]"#,
            r#""allowedRecipients":"a""#,
            r#""allowedRecipients":[1]"#,
            "",
        ] {
            let body = format!("{{{cap},{control}}}").replace(",}", "}");
            let input = parse_json(&body).unwrap();
            assert_eq!(from_openclaw(&input), Err(BridgeError::Input), "{control}");
        }
        let any =
            from_openclaw(&parse_json(&format!("{{{cap},\"allowAnyRecipient\":true}}")).unwrap())
                .unwrap();
        assert_eq!(any.get("allow_any_recipient"), Some(&Json::Bool(true)));
        assert!(any.get("allowed_recipients").is_none());
        let listed = from_openclaw(
            &parse_json(&format!("{{{cap},\"allowedRecipients\":[\"a\"]}}")).unwrap(),
        )
        .unwrap();
        assert!(listed.get("allow_any_recipient").is_none());
        assert_eq!(
            listed.get("allowed_recipients"),
            Some(&Json::Array(vec![Json::string("a")]))
        );
    }

    // #5410, T-OCP-MAP (§4.3 allowedAssetsByNetwork): the configured list wins
    // when non-empty; the default USDC entry only fills an empty Base Sepolia list.
    #[test]
    fn t_ocp_map_configured_assets_enforced() {
        use crate::agents_pay::policy::select_accept_entry;
        const USDC: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
        let other = format!("0x{}", "ab".repeat(20));
        let entry = |asset: &str| {
            Json::object(vec![
                ("scheme", Json::string("exact")),
                ("network", Json::string("eip155:84532")),
                ("asset", Json::string(asset)),
                ("payTo", Json::string("0xa")),
                ("amount", Json::string("1000")),
                ("maxTimeoutSeconds", Json::Int(60)),
            ])
        };
        let challenge =
            |asset: &str| Json::object(vec![("accepts", Json::Array(vec![entry(asset)]))]);
        let map = |assets: &str| {
            from_openclaw(
                &parse_json(&format!(
                    r#"{{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true,"allowedAssetsByNetwork":{assets}}}"#
                ))
                .unwrap(),
            )
            .unwrap()
        };
        let sepolia = |policy: &Json| {
            policy
                .get("allowed_assets")
                .unwrap()
                .get("eip155:84532")
                .cloned()
                .unwrap()
        };
        // Configured non-default list: mapped verbatim, default USDC is refused.
        let configured = map(&format!(r#"{{"eip155:84532":["{other}"]}}"#));
        assert_eq!(
            sepolia(&configured),
            Json::Array(vec![Json::string(&other)])
        );
        assert!(select_accept_entry(&challenge(USDC), &configured).is_err());
        assert_eq!(
            select_accept_entry(&challenge(&other), &configured)
                .unwrap()
                .get("asset")
                .and_then(Json::as_str),
            Some(other.as_str())
        );
        // Configured empty list, or no entry for the network: the default applies.
        for defaulted in [map(r#"{"eip155:84532":[]}"#), map("{}")] {
            assert_eq!(sepolia(&defaulted), Json::Array(vec![Json::string(USDC)]));
            assert!(select_accept_entry(&challenge(USDC), &defaulted).is_ok());
            assert!(select_accept_entry(&challenge(&other), &defaulted).is_err());
        }
        // Only Base Sepolia has a default; another network stays empty.
        let input = parse_json(r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"networkPreferences":["eip155:1"]}"#).unwrap();
        let mapped = from_openclaw(&input).unwrap();
        assert_eq!(
            mapped.get("allowed_assets").unwrap().get("eip155:1"),
            Some(&Json::Array(vec![]))
        );
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
            ("amount", "-1"),
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
        // D-OC3(b): the full gate refuses a configured non-EVM network.
        let other_policy = from_openclaw(&parse_json(r#"{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true,"networkPreferences":["solana:mainnet"],"allowedAssetsByNetwork":{"solana:mainnet":["asset"]}}"#).unwrap()).unwrap();
        let non_evm = parse_json(r#"{"accepts":[{"scheme":"exact","network":"solana:mainnet","asset":"asset","payTo":"recipient","amount":"1000","maxTimeoutSeconds":60}]}"#).unwrap();
        assert!(select_accept_entry(&non_evm, &other_policy).is_ok());
        let resolver = super::super::FixedResolver::new()
            .with_host("merchant.example", vec!["8.8.8.8".parse().unwrap()]);
        let resources = super::super::Resources::default();
        let error = super::super::authorize(
            &super::super::PaymentRequest {
                url: "https://merchant.example/pay",
                challenge: &non_evm,
                policy: &other_policy,
                resources: &resources,
                purchase_id: None,
            },
            &resolver,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Payment network family is not supported by this runtime."
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "parity cases share a single fixture and resolver"
    )]
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
        // Under allowAnyRecipient the other five controls remain in force.
        let any_profile = Json::object(vec![
            (
                "maxPaymentAmountAtomic",
                config.get("maxPaymentAmountAtomic").unwrap().clone(),
            ),
            ("allowAnyRecipient", Json::Bool(true)),
            (
                "networkPreferences",
                config.get("networkPreferences").unwrap().clone(),
            ),
            (
                "allowedAssetsByNetwork",
                config.get("allowedAssetsByNetwork").unwrap().clone(),
            ),
            (
                "allowedOrigins",
                config.get("allowedOrigins").unwrap().clone(),
            ),
        ]);
        let any_policy = from_openclaw(&any_profile).unwrap();
        let Json::Array(entries) = challenge.get("accepts").unwrap() else {
            unreachable!()
        };
        let single = |entry: Json| {
            let Json::Object(mut fields) = challenge.clone() else {
                unreachable!()
            };
            fields
                .iter_mut()
                .find(|(key, _)| key == "accepts")
                .unwrap()
                .1 = Json::Array(vec![entry]);
            Json::Object(fields)
        };
        let first = &entries[0];
        assert!(gate(&single(first.clone()), &any_policy, &resolver).is_ok());
        for (key, value) in [
            ("amount", "100001"),
            ("asset", "other"),
            ("scheme", "upto"),
            ("network", "eip155:1"),
        ] {
            let Json::Object(mut fields) = first.clone() else {
                unreachable!()
            };
            fields.iter_mut().find(|(k, _)| k == key).unwrap().1 = Json::string(value);
            assert!(
                gate(&single(Json::Object(fields)), &any_policy, &resolver).is_err(),
                "{key}"
            );
        }
        let mut altered = single(first.clone());
        if let Json::Object(fields) = &mut altered {
            fields.iter_mut().find(|(k, _)| k == "resource").unwrap().1 = Json::object(vec![(
                "url",
                Json::string("https://evil.example/pay/weather"),
            )]);
        }
        assert!(gate(&altered, &any_policy, &resolver).is_err());
        assert!(gate(&single(first.clone()), &wrong_origin, &resolver).is_err());
    }
}
