//! Strict, pure bridge request validation.

use std::io::Read;
use toolkit_core::{parse_json, Json};

/// Minimum contiguous secret bytes used as an anchored partial-match pattern.
pub const MIN_SECRET_FRAGMENT_BYTES: usize = 32;

/// Fixed error classes; details from untrusted input never cross the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    /// Invalid input, exit 2.
    Input,
    /// Service parameter or internal failure, exit 1.
    AgentCore,
    /// Service support absent, exit 1.
    Unavailable,
}

/// Read and validate a bounded request without consulting the environment.
pub fn read_request(reader: &mut impl Read) -> Result<Json, BridgeError> {
    let mut bytes = Vec::new();
    reader
        .take(131_073)
        .read_to_end(&mut bytes)
        .map_err(|_| BridgeError::Input)?;
    if bytes.is_empty() || bytes.len() > 131_072 {
        return Err(BridgeError::Input);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| BridgeError::Input)?;
    let request = parse_json(text).map_err(|_| BridgeError::Input)?;
    let common = [
        "operation",
        "region",
        "payment_manager_arn",
        "payment_session_id",
        "user_id",
        "agent_name",
    ];
    let operation = request
        .get("operation")
        .and_then(Json::as_str)
        .ok_or(BridgeError::Input)?;
    if operation == "sanitize_output" {
        validate_candidate(&request)?;
        return Ok(request);
    }
    let extra = [
        "payment_instrument_id",
        "version",
        "request_url",
        "accepted",
        "resource",
        "policy",
    ];
    let keys: Vec<&str> = match operation {
        "get_payment_session" => common.to_vec(),
        "process_payment" => common.iter().chain(extra.iter()).copied().collect(),
        _ => return Err(BridgeError::Input),
    };
    if !exact_keys(&request, &keys) {
        return Err(BridgeError::Input);
    }
    let field = |name: &str, max: usize| -> Result<&str, BridgeError> {
        let value = request
            .get(name)
            .and_then(Json::as_str)
            .ok_or(BridgeError::Input)?;
        if value.chars().count() > max
            || value.chars().all(super::super::pyval::is_python_whitespace)
        {
            return Err(BridgeError::Input);
        }
        Ok(value)
    };
    if !toolkit_aws::endpoint::is_valid_region(field("region", 128)?) {
        return Err(BridgeError::Input);
    }
    let manager = field("payment_manager_arn", 2048)?;
    let session = field("payment_session_id", 2048)?;
    let user = field("user_id", 512)?;
    if field("agent_name", 256)? != "openclaw-aws-agents-pay" {
        return Err(BridgeError::Input);
    }
    let instrument = if operation == "process_payment" {
        let instrument = field("payment_instrument_id", 2048)?;
        if field("version", 8)? != "2" {
            return Err(BridgeError::Input);
        }
        field("request_url", 2048)?;
        if !matches!(request.get("accepted"), Some(Json::Object(_))) {
            return Err(BridgeError::Input);
        }
        let resource = request.get("resource").ok_or(BridgeError::Input)?;
        let url = resource
            .get("url")
            .and_then(Json::as_str)
            .ok_or(BridgeError::Input)?;
        if url.chars().count() > 2048 || url.chars().all(super::super::pyval::is_python_whitespace)
        {
            return Err(BridgeError::Input);
        }
        super::super::policy::openclaw::from_openclaw(
            request.get("policy").ok_or(BridgeError::Input)?,
        )?;
        Some(instrument)
    } else {
        None
    };
    if !(66..=2048).contains(&manager.chars().count())
        || session.chars().count() != 31
        || user.chars().count() > 120
        || instrument.is_some_and(|value| value.chars().count() != 34)
    {
        return Err(BridgeError::AgentCore);
    }
    Ok(request)
}

fn exact_keys(value: &Json, keys: &[&str]) -> bool {
    matches!(value, Json::Object(members) if members.len() == keys.len() && members.iter().all(|(key, _)| keys.contains(&key.as_str())))
}

fn same_value(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Object(left), Json::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| b.get(key).is_some_and(|v| same_value(value, v)))
        }
        (Json::Array(left), Json::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| same_value(a, b))
        }
        _ => a == b,
    }
}

fn validate_candidate(request: &Json) -> Result<(), BridgeError> {
    if !exact_keys(request, &["operation", "candidate", "secrets"]) {
        return Err(BridgeError::Input);
    }
    let Some(Json::Array(secrets)) = request.get("secrets") else {
        return Err(BridgeError::Input);
    };
    if secrets
        .iter()
        .any(|value| !matches!(value, Json::String(_)))
    {
        return Err(BridgeError::Input);
    }
    let candidate = request.get("candidate").ok_or(BridgeError::Input)?;
    if !exact_keys(candidate, &["content", "details"]) {
        return Err(BridgeError::Input);
    }
    let details = candidate
        .get("details")
        .filter(|v| matches!(v, Json::Object(_)))
        .ok_or(BridgeError::Input)?;
    let Some(Json::Array(content)) = candidate.get("content") else {
        return Err(BridgeError::Input);
    };
    if content.len() != 1
        || !exact_keys(&content[0], &["type", "text"])
        || content[0].get("type").and_then(Json::as_str) != Some("text")
    {
        return Err(BridgeError::Input);
    }
    let text = content[0]
        .get("text")
        .and_then(Json::as_str)
        .ok_or(BridgeError::Input)?;
    let parsed = parse_json(text).map_err(|_| BridgeError::Input)?;
    if !same_value(&parsed, details) {
        return Err(BridgeError::Input);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const VALID: &str = r#"{"operation":"get_payment_session","region":"us-east-1","payment_manager_arn":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","payment_session_id":"sssssssssssssssssssssssssssssss","user_id":"user","agent_name":"openclaw-aws-agents-pay"}"#;

    // T-BR-REQ-SIZE, JSON, KEYS, STR, AGENT, REGION and PARAM.
    #[test]
    fn t_br_req_and_param() {
        assert!(read_request(&mut Cursor::new(VALID)).is_ok());
        for bad in [
            "",
            "[]",
            "NaN",
            "\u{feff}{}",
            r#"{"operation":"x","operation":"x"}"#,
        ] {
            assert_eq!(read_request(&mut Cursor::new(bad)), Err(BridgeError::Input));
        }
        for bad in [
            VALID.replace("\"user_id\":\"user\",", ""),
            VALID.replace("\"user\"", "\"\\u001c\""),
            VALID.replace("us-east-1", "us-east-1x"),
            VALID.replace("openclaw-aws-agents-pay", "other"),
            VALID.replace("\"user\"", "0"),
            VALID.replace("\"operation\":", "\"extra\":1,\"operation\":"),
        ] {
            assert_eq!(read_request(&mut Cursor::new(bad)), Err(BridgeError::Input));
        }
        let long_user = VALID.replace("\"user\"", &format!("\"{}\"", "x".repeat(121)));
        assert_eq!(
            read_request(&mut Cursor::new(long_user)),
            Err(BridgeError::AgentCore)
        );
        let mut large = Cursor::new(vec![b' '; 1_048_576]);
        assert_eq!(read_request(&mut large), Err(BridgeError::Input));
        assert_eq!(large.position(), 131073);
    }

    #[test]
    fn t_br_req_size_and_unicode_boundaries() {
        let exactly = format!("{}{}", VALID, " ".repeat(131072 - VALID.len()));
        assert!(read_request(&mut Cursor::new(&exactly)).is_ok());
        assert_eq!(
            read_request(&mut Cursor::new(format!("{exactly} "))),
            Err(BridgeError::Input)
        );
        let manager = "a".repeat(66);
        for (length, expected) in [(2048, true), (2049, false)] {
            let request = VALID.replace(&manager, &"é".repeat(length));
            assert_eq!(read_request(&mut Cursor::new(request)).is_ok(), expected);
        }
        let accepted = VALID.replace("\"user\"", "\"\u{200b}\"");
        let parsed = read_request(&mut Cursor::new(accepted)).unwrap();
        assert_eq!(
            parsed.get("user_id").and_then(Json::as_str),
            Some("\u{200b}")
        );
        for whitespace in [" ", "\u{3000}", "\u{1c}"] {
            let request = VALID.replace("\"user\"", &format!("\"{whitespace}\""));
            assert_eq!(
                read_request(&mut Cursor::new(request)),
                Err(BridgeError::Input)
            );
        }
    }

    // T-BR-REQ-PAY, T-BR-REQ-LEG, T-OC-FINAL-BOUND (pure shapes).
    #[test]
    fn t_br_req_pay_and_final_bound() {
        let payment = VALID.replace("get_payment_session", "process_payment").replace(
            "\"user_id\":", "\"payment_instrument_id\":\"iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii\",\"version\":\"2\",\"request_url\":\"https://merchant.test\",\"accepted\":{},\"resource\":{\"url\":\"https://merchant.test\"},\"policy\":{\"maxPaymentAmountAtomic\":\"1\",\"allowAnyRecipient\":true},\"user_id\":");
        assert!(read_request(&mut Cursor::new(&payment)).is_ok());
        for bad in [
            payment.replace("\"version\":\"2\"", "\"version\":2"),
            payment.replace("\"accepted\":{}", "\"accepted\":[]"),
            payment.replace(
                "\"resource\":{\"url\":\"https://merchant.test\"}",
                "\"resource\":{}",
            ),
            payment.replace("\"version\":\"2\"", "\"payload\":{},\"client_token\":\"x\""),
        ] {
            assert_eq!(read_request(&mut Cursor::new(bad)), Err(BridgeError::Input));
        }
        let clean = r#"{"operation":"sanitize_output","candidate":{"content":[{"type":"text","text":"{\"paid\":false}"}],"details":{"paid":false}},"secrets":["abcdefgh"]}"#;
        assert!(read_request(&mut Cursor::new(clean)).is_ok());
        for bad in [
            clean.replace("\"paid\":false}", "\"paid\":true}"),
            clean.replace("\"secrets\":[\"abcdefgh\"]", "\"secrets\":[1]"),
            clean.replace("\"type\":\"text\"", "\"type\":\"image\""),
        ] {
            assert_eq!(read_request(&mut Cursor::new(bad)), Err(BridgeError::Input));
        }
    }

    // T-OC-FINAL-BOUND: budget shapes and public prefix controls (#5180).
    #[test]
    fn t_oc_final_bound_budget_and_prefix() {
        assert_eq!(MIN_SECRET_FRAGMENT_BYTES, 32);
        assert!("eyJ4NDAyVmVyc2lvbiI6Mi".len() < MIN_SECRET_FRAGMENT_BYTES);
        assert!("0x".len() < MIN_SECRET_FRAGMENT_BYTES);
        let make = |body: &str| {
            let details = Json::object(vec![("body", Json::string(body))]);
            let candidate = Json::object(vec![
                (
                    "content",
                    Json::Array(vec![Json::object(vec![
                        ("type", Json::string("text")),
                        ("text", Json::string(details.to_pretty_string())),
                    ])]),
                ),
                ("details", details),
            ]);
            Json::object(vec![
                ("operation", Json::string("sanitize_output")),
                ("candidate", candidate),
                (
                    "secrets",
                    Json::Array(vec![
                        Json::string("eyJ4NDAyVmVyc2lvbiI6Mi-private-proof-material-12345"),
                        Json::string("0x1234567890abcdef1234567890abcdef"),
                    ]),
                ),
            ])
            .to_compact_string()
        };
        let dense = make(&"\u{1}".repeat(10240));
        assert!(dense.len() > 131072);
        assert_eq!(
            read_request(&mut Cursor::new(dense)),
            Err(BridgeError::Input)
        );
        for body in [
            "é".repeat(10240),
            "漢".repeat(10240),
            "😀".repeat(5120),
            "eyJ4NDAyVmVyc2lvbiI6Mi 0x".to_owned(),
        ] {
            let clean = make(&body);
            assert!(clean.len() <= 131072);
            assert!(read_request(&mut Cursor::new(clean)).is_ok());
        }
        let lone_surrogate = r#"{"operation":"sanitize_output","candidate":{"content":[{"type":"text","text":"{}"}],"details":{}},"secrets":["\ud800"]}"#;
        assert_eq!(
            read_request(&mut Cursor::new(lone_surrogate)),
            Err(BridgeError::Input)
        );
    }
}
