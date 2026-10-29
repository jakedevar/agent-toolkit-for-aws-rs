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

#[test]
fn t_scrub_finds_whole_cut_and_partial_secrets() {
    let secret = "eyj4ndayvmvyc2lvbii6miwiywnjzxb0zwqionsic2nozw1lijoizxhhy3qi";
    assert!(leaks(&format!("text/{secret}"), secret));
    // A cut keeps a prefix; a partial echo keeps a middle piece.
    assert!(leaks(&format!("text/{}", &secret[..40]), secret));
    assert!(leaks(&format!("x{}y", &secret[13..50]), secret));
    // Shorter than a window and not the whole secret: not a match.
    assert!(!leaks(&secret[..20], secret));
    // Secrets under 8 bytes are never searched (they match ordinary words).
    assert!(!leaks("ps-1", "ps-1"));
    assert!(leaks("session payment-s", "payment-s"));
    let document = Json::object(vec![("content_type", Json::string(&secret[..40]))]);
    assert_eq!(
        render(&scrubbed(document, &[secret.to_owned()])),
        r#"{"paid": false, "refused": true, "reason": "RuntimeError during payment flow."}"#
    );
}
