use super::{parse_request, Outcome};

fn request(query: &str) -> Vec<u8> {
    format!("GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n").into_bytes()
}

#[test]
fn callback_validates_state_and_never_reflects_error_description() {
    assert_eq!(parse_request(&request("state=wrong&code=abc"), 1234, "secret"), Outcome::Ignore(400));
    assert_eq!(parse_request(&request("state=secret&code=abc"), 1234, "secret"), Outcome::Code("abc".into()));
    assert_eq!(parse_request(&request("state=secret&error=access_denied&error_description=private"), 1234, "secret"), Outcome::Denied("access_denied".into()));
}

#[test]
fn callback_rejects_duplicate_parameters_and_invalid_code() {
    assert_eq!(parse_request(&request("state=secret&code=a&code=b"), 1234, "secret"), Outcome::Ignore(400));
    assert_eq!(parse_request(&request("state=secret&code=%0A"), 1234, "secret"), Outcome::InvalidCode);
}
