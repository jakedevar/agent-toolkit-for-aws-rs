//! T-ERR (R-ERR): the class-name table is exhaustive over the flow's error
//! enum, and every source of a failure maps into it.

use super::*;
use crate::agents_pay::agentcore::PaymentErrorClass;
use crate::agents_pay::net::{MerchantHttp, StreamingMerchant, SystemClock};
use toolkit_aws::error::{AwsError, TimeoutPhase};
use toolkit_aws::testing::{FakeStreamingTransport, FixedResolver};

#[test]
fn t_err_the_class_name_table_is_exhaustive() {
    let all = [
        (FlowError::Connect, "ConnectError"),
        (FlowError::ConnectTimeout, "ConnectTimeout"),
        (FlowError::Timeout, "ReadTimeout"),
        (FlowError::Protocol, "RemoteProtocolError"),
        (FlowError::Service, "ClientError"),
        (FlowError::NoCredentials, "NoCredentialsError"),
        (FlowError::Value, "ValueError"),
        (FlowError::Json, "JSONDecodeError"),
        (FlowError::Runtime, "RuntimeError"),
    ];
    for (error, name) in all {
        // The match below fails to compile when a variant is added.
        match error {
            FlowError::Connect
            | FlowError::ConnectTimeout
            | FlowError::Timeout
            | FlowError::Protocol
            | FlowError::Service
            | FlowError::NoCredentials
            | FlowError::Value
            | FlowError::Json
            | FlowError::Runtime => {}
        }
        assert_eq!(error.class_name(), name);
        assert_eq!(error.during_flow(), format!("{name} during payment flow."));
        assert_eq!(
            error.during_status(),
            format!("{name} while reading session status.")
        );
    }
}

#[test]
fn t_err_agentcore_failures_map_into_the_table() {
    for (error, name) in [
        (PaymentError::Service("ThrottlingException"), "ClientError"),
        (PaymentError::InvalidResponse, "RuntimeError"),
        (
            PaymentError::Transport(PaymentErrorClass::ConnectError),
            "ConnectError",
        ),
        (
            PaymentError::Transport(PaymentErrorClass::ConnectTimeout),
            "ConnectTimeout",
        ),
        (
            PaymentError::Transport(PaymentErrorClass::ReadTimeout),
            "ReadTimeout",
        ),
        (
            PaymentError::Transport(PaymentErrorClass::RemoteProtocolError),
            "RemoteProtocolError",
        ),
        (
            PaymentError::Transport(PaymentErrorClass::NoCredentialsError),
            "NoCredentialsError",
        ),
        (
            PaymentError::Transport(PaymentErrorClass::ClientError),
            "ClientError",
        ),
        (
            PaymentError::Transport(PaymentErrorClass::RuntimeError),
            "RuntimeError",
        ),
    ] {
        assert_eq!(FlowError::from_payment(&error).class_name(), name);
        assert_eq!(error.class_name(), name);
    }
}

#[test]
fn t_err_merchant_transport_failures_use_the_same_table() {
    for (error, name) in [
        (AwsError::Protocol, "RemoteProtocolError"),
        (
            AwsError::Timeout {
                phase: TimeoutPhase::Connect,
                message: String::new(),
            },
            "ConnectTimeout",
        ),
        (
            AwsError::Timeout {
                phase: TimeoutPhase::Read,
                message: String::new(),
            },
            "ReadTimeout",
        ),
        (AwsError::NoCredentials, "NoCredentialsError"),
    ] {
        let merchant = StreamingMerchant {
            transport: FakeStreamingTransport::new().open_error(error),
            resolver: FixedResolver::new().answer_any(&["93.184.216.34"]),
            clock: SystemClock,
        };
        let failure = merchant
            .request(&MerchantRequest {
                url: "https://merchant.example/x",
                method: "GET",
                proof: None,
                body_cap: 1024,
                timeout: Duration::from_secs(2),
                operation_deadline: Instant::now() + Duration::from_secs(30),
            })
            .unwrap_err();
        assert_eq!(failure.0, format!("{name} during payment flow."));
    }
}

fn learned(secrets: &[&str]) -> KnownSecrets {
    let mut known = KnownSecrets::default();
    for secret in secrets {
        learn_secret(&mut known, secret);
    }
    known
}

#[test]
fn t_scrub_finds_whole_cut_and_partial_secrets() {
    let secret = "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0ZWQiOnsic2NoZW1lIjoiZXhhY3Qi";
    let known = learned(&[secret]);
    let lower = secret.to_ascii_lowercase();
    assert!(known.found_in(&format!("text/{lower}")));
    // A cut keeps a prefix; a partial echo keeps a piece at any offset.
    assert!(known.found_in(&format!("text/{}", &lower[..8])));
    assert!(known.found_in(&format!("x{}y", &secret[13..50])));
    assert!(known.found_in(&format!("x{}y", &secret[1..33])));
    assert!(known.found_in(&secret[47..55]));
    // Seven bytes are ordinary text.
    assert!(!known.found_in(&secret[47..54]));
    // Secrets under 8 bytes are never searched (they match ordinary words).
    assert!(learned(&["ps-1"]).is_empty());
    let document = Json::object(vec![("content_type", Json::string(&lower[3..11]))]);
    assert_eq!(
        render(&scrubbed(document, &known)),
        r#"{"paid": false, "refused": true, "reason": "RuntimeError during payment flow."}"#
    );
}

/// The fixed head of a payment session id is public: only the part after it
/// is a secret, so the fixed texts that say `payment-` or `-session` pass.
#[test]
fn t_scrub_a_session_id_is_secret_after_its_public_head() {
    assert_eq!(
        session_secret("payment-session-TEST12345678901"),
        "TEST12345678901"
    );
    assert_eq!(session_secret("PAYMENT-SESSION-abcdefgh"), "abcdefgh");
    assert_eq!(
        session_secret("ps-custom-identifier"),
        "ps-custom-identifier"
    );
    let known = learned(&[session_secret("payment-session-TEST12345678901")]);
    for fixed in [
        WITHHELD,
        MINT_BUDGET,
        NEW_SESSION,
        CHECK_SESSION,
        BROWSER_NEXT,
    ] {
        assert!(!known.found_in(fixed), "{fixed}");
    }
    assert!(known.found_in("id payment-session-test1234"));
    assert!(known.found_in("12345678"));
}

/// The JSON escape of a secret is learned too: a secret holding a quote or
/// a non-ASCII character is still found in the rendered document.
#[test]
fn t_scrub_finds_the_rendered_json_spelling() {
    let secret = "s\u{e9}cret\"value-0123";
    let known = learned(&[secret]);
    let document = Json::object(vec![("content_type", Json::string(secret))]);
    assert!(render(&document).contains("\\u00e9"));
    assert_eq!(
        render(&scrubbed(document, &known)),
        r#"{"paid": false, "refused": true, "reason": "RuntimeError during payment flow."}"#
    );
}
