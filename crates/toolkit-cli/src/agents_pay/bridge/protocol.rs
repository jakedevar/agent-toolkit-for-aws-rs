//! Strict, pure bridge request validation.

use std::io::Read;
use toolkit_core::{parse_json, Json};

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
    let keys = [
        "operation",
        "region",
        "payment_manager_arn",
        "payment_session_id",
        "user_id",
        "agent_name",
    ];
    let Json::Object(members) = &request else {
        return Err(BridgeError::Input);
    };
    if members.len() != keys.len()
        || members.iter().any(|(key, _)| !keys.contains(&key.as_str()))
        || request.get("operation").and_then(Json::as_str) != Some("get_payment_session")
    {
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
    if !(66..=2048).contains(&manager.chars().count())
        || session.chars().count() != 31
        || user.chars().count() > 120
    {
        return Err(BridgeError::AgentCore);
    }
    Ok(request)
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
}
