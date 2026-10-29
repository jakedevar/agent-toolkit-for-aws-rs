//! Pure OpenClaw policy mapping.

use toolkit_core::Json;

use crate::agents_pay::bridge::protocol::BridgeError;
use crate::agents_pay::urlparse::urlparse;

/// Convert the trusted glue's profile into the existing policy gate's JSON shape.
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
    if cap.len() > 1000
        || !matches!(cap.as_bytes().first(), Some(b'1'..=b'9'))
        || !cap.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(BridgeError::Input);
    }
    fn strings(value: &Json) -> Result<Vec<Json>, BridgeError> {
        let Json::Array(items) = value else {
            return Err(BridgeError::Input);
        };
        if items.iter().any(|item| !matches!(item, Json::String(_))) {
            return Err(BridgeError::Input);
        }
        Ok(items.clone())
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
}
