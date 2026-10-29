//! Strict, pure bridge request validation.

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const VALID: &str = r#"{"operation":"get_payment_session","region":"us-east-1","payment_manager_arn":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","payment_session_id":"sssssssssssssssssssssssssssssss","user_id":"user","agent_name":"openclaw-aws-agents-pay"}"#;

    // T-BR-REQ-SIZE, JSON, KEYS, STR, AGENT, REGION and PARAM.
    #[test]
    fn t_br_req_and_param() {
        assert!(read_request(&mut Cursor::new(VALID)).is_ok());
        for bad in ["", "[]", "NaN", "\u{feff}{}", r#"{"operation":"x","operation":"x"}"#] {
            assert_eq!(read_request(&mut Cursor::new(bad)), Err(BridgeError::Input));
        }
        for bad in [VALID.replace("\"user_id\":\"user\",", ""), VALID.replace("\"user\"", "\"\\u001c\""), VALID.replace("us-east-1", "us-east-1x"), VALID.replace("openclaw-aws-agents-pay", "other"), VALID.replace("\"user\"", "0"), VALID.replace("\"operation\":", "\"extra\":1,\"operation\":")] {
            assert_eq!(read_request(&mut Cursor::new(bad)), Err(BridgeError::Input));
        }
        let long_user = VALID.replace("\"user\"", &format!("\"{}\"", "x".repeat(121)));
        assert_eq!(read_request(&mut Cursor::new(long_user)), Err(BridgeError::AgentCore));
        let mut large = Cursor::new(vec![b' '; 1_048_576]);
        assert_eq!(read_request(&mut large), Err(BridgeError::Input));
        assert_eq!(large.position(), 131073);
    }
}
