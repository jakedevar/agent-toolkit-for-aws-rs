//! Pure OpenClaw policy mapping.

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    // T-OCP-MAP and T-OCP-ORIG: cap is atomic and origins discard paths.
    #[test]
    fn t_ocp_map_and_orig() {
        let input = parse_json(r#"{"maxPaymentAmountAtomic":"100000","allowAnyRecipient":true,"allowedOrigins":["https://MERCHANT.test:443/path"]}"#).unwrap();
        let policy = from_openclaw(&input).unwrap();
        assert_eq!(policy.get("max_per_payment_usd").and_then(Json::as_str), Some("100000e-6"));
        assert_eq!(policy.get("allowed_origins"), Some(&Json::Array(vec![Json::string("https://merchant.test")])));
        assert_eq!(policy.get("allowed_networks"), Some(&Json::Array(vec![Json::string("eip155:84532")])));
        for raw in [r#"{"maxPaymentAmountAtomic":"01","allowAnyRecipient":true}"#, r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"allowedRecipients":["a"]}"#, r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"allowedOrigins":["http://merchant.test"]}"#, r#"{"maxPaymentAmountAtomic":"1","allowAnyRecipient":true,"unknown":1}"#] {
            assert!(from_openclaw(&parse_json(raw).unwrap()).is_err());
        }
    }
}
