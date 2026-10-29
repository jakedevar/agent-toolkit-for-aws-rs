//! One table of every reproduction from the toolkit-aws credential-redaction
//! cluster (#4468, #4469, #4494, #4515, #4516, #4536), run through every
//! diagnostic surface that can carry AWS error text or response input.
//!
//! Each row leaked on the commit its Issue reviewed and must stay redacted;
//! the non-secret rows must come out byte-for-byte unchanged. All values are
//! synthetic (`FAKE-...`); no real credential is used.

use std::time::{Duration, Instant};

use toolkit_aws::error::render_failure;
use toolkit_aws::redact::redact_text;
use toolkit_aws::retry::RetryFailure;
use toolkit_aws::transport::{ApiRequest, Headers, HttpRequest, HttpResponse, Method, Timeouts};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::ToolkitError;

/// `(issue, raw diagnostic text, synthetic secrets that must not survive)`.
const LEAKS: &[(&str, &str, &[&str])] = &[
    // #4468: URL userinfo inside free error text.
    (
        "#4468",
        "dial https://review-user:FAKE-URL-PASSWORD@logs.amazonaws.com failed",
        &["review-user", "FAKE-URL-PASSWORD"],
    ),
    // #4468: an escaped quote inside a JSON string value.
    (
        "#4468",
        r#"{"password":"aa\"FAKE-TAIL-LEAK"}"#,
        &["FAKE-TAIL-LEAK"],
    ),
    // #4468: a newline between the colon and the value.
    (
        "#4468",
        "{\"password\":\n\"FAKE-NEWLINE-PASSWORD\"}",
        &["FAKE-NEWLINE-PASSWORD"],
    ),
    // #4468: a malformed (unterminated) JSON body.
    (
        "#4468",
        r#"{"secret":"FAKE-UNTERMINATED"#,
        &["FAKE-UNTERMINATED"],
    ),
    // #4469: a labelled value in a hand-built variant / benign header value.
    ("#4469", "token=FAKE-DEBUG-TOKEN", &["FAKE-DEBUG-TOKEN"]),
    (
        "#4469",
        "Bearer FAKE-HEADER-BEARER",
        &["FAKE-HEADER-BEARER"],
    ),
    // #4494: several separator spaces (and tabs) after Bearer/Basic.
    (
        "#4494",
        "Bearer  FAKE-TWO-SPACE-BEARER",
        &["FAKE-TWO-SPACE-BEARER"],
    ),
    (
        "#4494",
        "Basic  RkFLRTpNVUxUSVNQQUNF",
        &["RkFLRTpNVUxUSVNQQUNF"],
    ),
    (
        "#4494",
        "authorization failed: bearer \t  FAKE-MIXED-RUN",
        &["FAKE-MIXED-RUN"],
    ),
    // #4515: a label whose value starts with a scheme word.
    (
        "#4515",
        "token=Bearer FAKE-OVERLAP-VALUE",
        &["FAKE-OVERLAP-VALUE"],
    ),
    (
        "#4515",
        "password=Basic RkFLRTpPVkVSTEFQ",
        &["RkFLRTpPVkVSTEFQ"],
    ),
    // #4516: a secret label at the start, middle or end of a longer key.
    (
        "#4516",
        r#"{"token_value":"FAKE-JSON-PREFIX"}"#,
        &["FAKE-JSON-PREFIX"],
    ),
    (
        "#4516",
        "password_backup=FAKE-SUFFIX-TEXT",
        &["FAKE-SUFFIX-TEXT"],
    ),
    (
        "#4516",
        "my_Password_Old: FAKE-INFIX-TEXT",
        &["FAKE-INFIX-TEXT"],
    ),
    (
        "#4516",
        r#"{\"token_value\":\"FAKE-ESCAPED-JSON\"}"#,
        &["FAKE-ESCAPED-JSON"],
    ),
    // #4536: repeated labels inside one key must still hide the value.
    (
        "#4536",
        "tokentokentokentoken=FAKE-REPEATED-KEY",
        &["FAKE-REPEATED-KEY"],
    ),
];

/// Ordinary diagnostic text that must pass through unchanged.
const CLEAN: &[&str] = &[
    "connection refused: logs.us-east-1.amazonaws.com:443",
    "HTTP 503 Service Unavailable",
    "https://logs.us-east-1.amazonaws.com/path?limit=10",
    r#"{"message":"Rate exceeded","code":"ThrottlingException"}"#,
    "region us-west-2 is not enabled for this account",
];

/// Every diagnostic rendering of `raw` that can reach a user or a log.
fn renderings(raw: &str) -> Vec<(&'static str, String)> {
    let mut out = vec![("redact_text", redact_text(raw))];
    let built = AwsError::connect(raw);
    out.push(("AwsError Display", built.to_string()));
    out.push(("AwsError Debug", format!("{built:?}")));
    let hand_built = AwsError::Connect {
        message: raw.to_string(),
    };
    out.push(("hand-built AwsError Display", hand_built.to_string()));
    out.push(("hand-built AwsError Debug", format!("{hand_built:?}")));
    let retry = RetryFailure {
        error: hand_built.clone(),
        attempts: 3,
    };
    out.push(("RetryFailure Debug", format!("{retry:?}")));
    out.push(("RetryFailure Display", retry.to_string()));
    out.push((
        "ToolkitError",
        ToolkitError::from(hand_built.clone()).to_string(),
    ));
    out.push((
        "render_failure",
        render_failure("call the service", &hand_built, &[("endpoint", raw)]),
    ));
    let service = ServiceError::new("GET", "/v1/items", 400, Some("Bad"), raw, None);
    out.push(("ServiceError Display", service.to_string()));
    out.push(("ServiceError Debug", format!("{service:?}")));
    out.push(("ServiceError message", service.message().to_string()));
    let service_error = AwsError::service(service);
    out.push((
        "render_failure(service)",
        render_failure("list items", &service_error, &[]),
    ));
    // Headers reject control characters, so newline rows skip this surface.
    let mut headers = Headers::new();
    if headers.insert("x-extra", raw).is_ok() {
        headers
            .insert("x-access-token", "FAKE-ACCESS-HEADER")
            .expect("valid header");
        out.push(("Headers Debug", format!("{headers:?}")));
        let mut api = ApiRequest::new(Method::Get, "/v1/items");
        api.headers = headers.clone();
        out.push(("ApiRequest Debug", format!("{api:?}")));
        let http = HttpRequest {
            method: Method::Get,
            url: "https://logs.amazonaws.com/".to_string(),
            headers: headers.clone(),
            body: Vec::new(),
            timeouts: Timeouts::default(),
        };
        out.push(("HttpRequest Debug", format!("{http:?}")));
        let mut response = HttpResponse::new(400, raw.as_bytes());
        response.headers = headers;
        out.push(("HttpResponse Debug", format!("{response:?}")));
    }
    out
}

#[test]
fn every_cluster_reproduction_is_redacted_on_every_surface() {
    let mut leaks = Vec::new();
    for (issue, raw, secrets) in LEAKS {
        for (surface, text) in renderings(raw) {
            for secret in secrets.iter().copied().chain(["FAKE-ACCESS-HEADER"]) {
                if text.contains(secret) {
                    leaks.push(format!("{issue} {surface}: {secret} in {text:?}"));
                }
            }
        }
        let once = redact_text(raw);
        assert_eq!(redact_text(&once), once, "{issue}: not idempotent: {raw:?}");
    }
    assert!(leaks.is_empty(), "leaks:\n{}", leaks.join("\n"));
}

#[test]
fn non_secret_diagnostic_text_is_unchanged() {
    for raw in CLEAN {
        assert_eq!(redact_text(raw), *raw);
        assert_eq!(
            AwsError::connect(raw).to_string(),
            format!("could not connect to the endpoint: {raw}")
        );
        let service = ServiceError::new("GET", "/v1/items", 400, None, raw, None);
        assert_eq!(service.message(), *raw);
    }
}

/// #4536: one key with a label repeated `k` times cost `5k(k-1)/2` key-byte
/// checks per pass (about 10^9 at k = 20,000). The linear scrubber finishes
/// these 100 KB inputs in milliseconds; the bound is far above that and far
/// below the quadratic cost, so it tolerates a loaded CI host.
#[test]
fn repeated_label_keys_finish_within_a_time_bound() {
    const BOUND: Duration = Duration::from_secs(3);
    let k = 20_000;
    let inputs = [
        format!("{}=x", "token".repeat(k)),
        format!("{}=FAKE-LONG-VALUE", "token".repeat(k)),
        format!("{}x", "token".repeat(k)),
        format!("{{\"{}\":\"FAKE-JSON\"}}", "token".repeat(k)),
        format!("{}=x", "token_".repeat(k)),
        format!("{}=x", "password".repeat(k * 5 / 8)),
    ];
    for input in &inputs {
        let started = Instant::now();
        let out = redact_text(input);
        let elapsed = started.elapsed();
        assert!(
            elapsed < BOUND,
            "{} bytes took {elapsed:?} (bound {BOUND:?})",
            input.len()
        );
        assert!(!out.contains("FAKE-"), "value survived: {}", &out[..64]);
    }
}
