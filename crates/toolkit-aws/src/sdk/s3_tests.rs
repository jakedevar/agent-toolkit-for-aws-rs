//! #4413 S3 cases that need the S1 loopback harness: T-TLS-21 (the provider
//! plane trusts exactly the validated roots), T-HDR on the wire (one `Host`
//! from [`McpTarget`], `Content-Length` from the final body, no
//! `Transfer-Encoding`, a signature that re-verifies against the received
//! request) and the `SigV4` canonical-URI known answer for a percent-encoded
//! path, derived independently with a hand-written HMAC-SHA256.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::roots::ValidatedRoots;
use super::source::{GuardedCredentials, ProviderHttp, RequestCredentials};
use super::test_support::{
    child_case, child_command, http_ok, report, run, Part, Pki, Responder, TempDir, TlsServer,
};
use super::{SdkRuntime, SdkTransport, SigV4Signer};
use crate::endpoint::HostResolver;
use crate::mcp_target::McpTarget;
use crate::stream::{StreamLimits, StreamingTransport};
use crate::testing::FakeProvider;
use crate::transport::{Headers, HttpRequest, Method, Signer, SigningScope, Timeouts};

const ENTRY: &str = "sdk::s3_tests::child_entry";
const HOST: &str = "loopback.mcp-test.api.aws";
const STS_KEY: &str = "ASIASTSFIXTURE00001";

/// 2015-08-30T12:36:00Z, the `SigV4` test-suite time.
fn fixed_clock() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_440_938_160)
}

struct Loopback;

impl HostResolver for Loopback {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        Ok(vec![IpAddr::from([127, 0, 0, 1])])
    }
}

#[test]
fn malformed_merchant_response_heads_are_protocol_errors() {
    for raw in [
        &b"HTTP/1.1 XYZ secret-status\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\ninvalid header secret-value\r\n\r\n"[..],
    ] {
        let pki = Pki::new("merchant protocol test CA");
        let (chain, key) = pki.server(HOST);
        let bytes = raw.to_vec();
        let respond: Responder = Arc::new(move |_, _| vec![Part::Bytes(bytes.clone())]);
        let server = TlsServer::start(chain, key, respond);
        let roots = ValidatedRoots::for_tests(vec![pki.ca_der.clone()]);
        let transport = SdkTransport::loopback_for_tests(
            &SdkRuntime::new().unwrap(),
            &roots,
            Arc::new(Loopback),
        )
        .unwrap();
        let request = HttpRequest {
            method: Method::Get,
            url: format!("https://{HOST}:{}/paid", server.port()),
            headers: Headers::new(),
            body: Vec::new(),
            timeouts: Timeouts::new(Duration::from_secs(2), Duration::from_secs(2)).unwrap(),
        };
        let limits = StreamLimits::new(1024, 1024)
            .unwrap()
            .with_deadline(std::time::Instant::now() + Duration::from_secs(10));
        assert!(matches!(
            transport.open(&request, &limits),
            Err(crate::error::AwsError::Protocol)
        ));
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0]
            .0
            .to_ascii_lowercase()
            .contains("payment-signature:"));
        assert!(!requests[0].0.to_ascii_lowercase().contains("x-payment:"));
        assert!(requests[0].1.is_empty());
    }
}

// ------------------------------------------------------------- T-TLS-21

fn assume_role_xml() -> String {
    format!(
        "<AssumeRoleResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><AssumeRoleResult>\
         <AssumedRoleUser><AssumedRoleId>AROAFIXTURE:s</AssumedRoleId>\
         <Arn>arn:aws:sts::123456789012:assumed-role/r/s</Arn></AssumedRoleUser>\
         <Credentials><AccessKeyId>{STS_KEY}</AccessKeyId><SecretAccessKey>sts-secret</SecretAccessKey>\
         <SessionToken>sts-token</SessionToken><Expiration>2099-01-01T00:00:00Z</Expiration></Credentials>\
         </AssumeRoleResult><ResponseMetadata><RequestId>r</RequestId></ResponseMetadata></AssumeRoleResponse>"
    )
}

/// The child: validate the roots from the process environment, build the
/// proxy's provider-plane client from them and resolve the role profile.
#[test]
fn child_entry() {
    let Some(case) = child_case() else {
        return;
    };
    assert_eq!(case, "assume-role");
    let roots = ValidatedRoots::from_process().expect("valid roots");
    let runtime = SdkRuntime::new().expect("runtime");
    let http = ProviderHttp::validated(&roots).expect("provider client");
    let credentials = GuardedCredentials::new(&runtime, "us-east-1", Some("role"), http);
    match credentials.resolve(false, false) {
        RequestCredentials::Sign(loaded) => {
            report("decision", "sign");
            report("key", loaded.access_key_id().expose());
        }
        RequestCredentials::Unsigned => report("decision", "unsigned"),
        RequestCredentials::Refused(refusal) => report("decision", format!("refused:{refusal:?}")),
    }
}

fn run_assume_role(trusted: &Pki, server_ca: &Pki) -> (String, Option<String>, usize) {
    let (chain, key) = server_ca.server("localhost");
    let xml = assume_role_xml();
    let respond: Responder = Arc::new(move |_, _| {
        vec![Part::Bytes(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{xml}",
                xml.len()
            )
            .into_bytes(),
        )]
    });
    let server = TlsServer::start(chain, key, respond);
    let tmp = TempDir::new("t-tls-21");
    let config = tmp.write(
        "config",
        "[profile role]\nrole_arn = arn:aws:iam::123456789012:role/fixture\nsource_profile = base\n\
         [profile base]\naws_access_key_id = AKIDBASEFIXTURE0001\naws_secret_access_key = base-secret\n",
    );
    let credentials = tmp.write("credentials", "");
    let ca = tmp.write("ca.pem", &trusted.ca_pem);
    let child = run(child_command(ENTRY, "assume-role")
        .env("HOME", tmp.path())
        .env("AWS_CONFIG_FILE", &config)
        .env("AWS_SHARED_CREDENTIALS_FILE", &credentials)
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env(
            "AWS_ENDPOINT_URL_STS",
            format!("https://localhost:{}", server.port()),
        )
        .env("SSL_CERT_FILE", &ca));
    child.assert_ok();
    let key = child.results.get("key").cloned();
    (
        child.result("decision").to_string(),
        key,
        server.requests().len(),
    )
}

#[test]
fn t_tls_21_the_provider_plane_trusts_the_validated_roots() {
    let pki = Pki::new("t-tls-21 sts ca");
    let (decision, key, requests) = run_assume_role(&pki, &pki);
    assert_eq!(decision, "sign");
    assert_eq!(key.as_deref(), Some(STS_KEY));
    assert!(requests >= 1, "the STS fixture answered AssumeRole");
}

#[test]
fn t_tls_21_a_different_valid_ca_fails_the_handshake_and_nothing_is_signed() {
    let trusted = Pki::new("t-tls-21 other ca");
    let server = Pki::new("t-tls-21 sts ca");
    let (decision, key, requests) = run_assume_role(&trusted, &server);
    assert!(decision.starts_with("refused:"), "{decision}");
    assert_eq!(key, None);
    assert_eq!(requests, 0, "no request bytes reached the STS fixture");
}

// ---------------------------------------------------------- T-HDR wire

fn signer() -> SigV4Signer<FakeProvider> {
    SigV4Signer::new(FakeProvider::with_keys(
        "fixture",
        "AKIDEXAMPLE",
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
    ))
    .with_clock(fixed_clock)
}

fn scope() -> SigningScope {
    SigningScope {
        region: "us-east-1".to_string(),
        service: "loopback".to_string(),
    }
}

/// The proxy's request shape: no `Host`, no `Content-Length`, the owned MCP
/// headers, a sensitive `--header` and the post-`_meta` body.
fn proxy_request(target: &McpTarget, body: &[u8]) -> HttpRequest {
    let mut headers = Headers::new();
    headers
        .insert_sensitive(
            "x-amzn-agentaccess-streaming-session-url",
            "HEADER-CANARY-7f3",
        )
        .unwrap();
    headers
        .insert("accept", "application/json, text/event-stream")
        .unwrap();
    headers.insert("content-type", "application/json").unwrap();
    headers.insert("mcp-session-id", "session-1").unwrap();
    headers
        .insert("user-agent", "toolkit-mcp-proxy/test")
        .unwrap();
    HttpRequest {
        method: Method::Post,
        url: target.url(),
        headers,
        body: body.to_vec(),
        timeouts: Timeouts::new(Duration::from_secs(5), Duration::from_secs(5)).unwrap(),
    }
}

fn header_values<'a>(head: &'a str, name: &str) -> Vec<&'a str> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .filter(|(header, _)| header.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .collect()
}

#[test]
fn t_hdr_wire_host_length_and_a_signature_that_re_verifies() {
    let pki = Pki::new("t-hdr wire ca");
    let (chain, key) = pki.server(HOST);
    let respond: Responder = Arc::new(|_, _| http_ok("{}"));
    let server = TlsServer::start(chain, key, respond);
    let roots = ValidatedRoots::for_tests(vec![pki.ca_der.clone()]);
    let runtime = SdkRuntime::new().unwrap();
    let transport = SdkTransport::loopback_for_tests(&runtime, &roots, Arc::new(Loopback)).unwrap();

    for path in ["/mcp", "/mcp/", "/", "/a%2Fb/mcp"] {
        let target = McpTarget::parse(&format!("https://{HOST}:{}{path}", server.port())).unwrap();
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"INSTALL_SOURCE":"x"}}}"#;
        let mut request = proxy_request(&target, body);
        signer().sign(&mut request, &scope()).unwrap();
        let before = server.requests().len();
        let stream = transport
            .open(&request, &StreamLimits::new(1 << 20, 4096).unwrap())
            .unwrap();
        assert_eq!(stream.status(), 200);
        drop(stream);
        let (head, received) = server.requests()[before].clone();

        let request_line = head.lines().next().unwrap();
        assert_eq!(request_line, format!("POST {path} HTTP/1.1"));
        assert_eq!(header_values(&head, "host"), vec![target.authority()]);
        assert_eq!(received, body);
        assert_eq!(
            header_values(&head, "content-length"),
            vec![body.len().to_string()]
        );
        assert!(header_values(&head, "transfer-encoding").is_empty());
        assert_eq!(
            header_values(&head, "x-amzn-agentaccess-streaming-session-url"),
            vec!["HEADER-CANARY-7f3"]
        );

        // Re-verify: rebuild the request from what the server received and
        // sign it again with the fixture credentials and clock.
        let authorization = header_values(&head, "authorization")[0].to_string();
        let signed = authorization
            .split("SignedHeaders=")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .unwrap();
        let names: Vec<&str> = signed.split(';').collect();
        assert!(names.contains(&"host"), "{signed}");
        assert!(names.contains(&"x-amzn-agentaccess-streaming-session-url"));
        let mut headers = Headers::new();
        for name in &names {
            if matches!(*name, "host" | "x-amz-date") {
                continue;
            }
            headers.insert(name, header_values(&head, name)[0]).unwrap();
        }
        let mut rebuilt = HttpRequest {
            method: Method::Post,
            url: format!("https://{}{path}", header_values(&head, "host")[0]),
            headers,
            body: received,
            timeouts: request.timeouts,
        };
        signer().sign(&mut rebuilt, &scope()).unwrap();
        assert_eq!(
            rebuilt.headers.get("authorization"),
            Some(authorization.as_str())
        );
    }
}

// --------------------------------------------- canonical URI known answer

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut block = [0_u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let mut inner = Sha256::new();
    inner.update(pad(0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(pad(0x5c));
    outer.update(inner.finalize());
    outer.finalize().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[test]
fn the_canonical_uri_of_a_percent_encoded_path_matches_a_known_answer() {
    let secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    let target = McpTarget::parse("https://mcp.us-east-1.api.aws/a%2Fb/mcp").unwrap();
    let body = b"{}";
    let mut request = HttpRequest {
        method: Method::Post,
        url: target.url(),
        headers: Headers::new(),
        body: body.to_vec(),
        timeouts: Timeouts::default(),
    };
    let scope = SigningScope {
        region: "us-east-1".to_string(),
        service: "aws-mcp".to_string(),
    };
    signer().sign(&mut request, &scope).unwrap();

    // Non-S3 services encode each path segment twice: `%` becomes `%25`.
    let canonical = format!(
        "POST\n/a%252Fb/mcp\n\nhost:mcp.us-east-1.api.aws\nx-amz-date:20150830T123600Z\n\n\
         host;x-amz-date\n{}",
        hex(&Sha256::digest(body))
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/aws-mcp/aws4_request\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut key = hmac(format!("AWS4{secret}").as_bytes(), b"20150830");
    for part in ["us-east-1", "aws-mcp", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let expected = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/aws-mcp/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature={}",
        hex(&hmac(&key, string_to_sign.as_bytes()))
    );
    assert_eq!(
        request.headers.get("authorization"),
        Some(expected.as_str())
    );
}

#[test]
fn the_hmac_matches_rfc_4231_case_2() {
    assert_eq!(
        hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}
