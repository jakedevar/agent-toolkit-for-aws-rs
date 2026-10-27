//! Offline tests for the MCP backend: scripted transport, fake signer, fixed
//! DNS. The secret values below are synthetic.

use super::*;
use std::time::Duration;
use toolkit_aws::testing::{FakeSigner, FixedResolver, MockTransport};
use toolkit_aws::{AwsError, MapEnv};

const URL: &str = "https://aws-mcp.us-east-1.api.aws/mcp";
const VALUE: &str = "synthetic-secret-value";

struct Rig {
    env: MapEnv,
    resolver: FixedResolver,
    transport: MockTransport,
    signer: FakeSigner,
}

impl Rig {
    fn new(transport: MockTransport) -> Self {
        Self {
            env: MapEnv::new(),
            resolver: FixedResolver::new().answer_any(&["3.5.140.2"]),
            transport,
            signer: FakeSigner::new(),
        }
    }

    fn backend(&self, url: &str) -> McpBackend<'_> {
        let timeout = Duration::from_secs(30);
        McpBackend::new(
            url,
            Timeouts::new(timeout, timeout).unwrap(),
            30.0,
            &self.env,
            &self.resolver,
            &self.transport,
            &self.signer,
        )
    }
}

/// A `tools/call` reply whose single text content is `envelope`.
fn call_reply(envelope: &str) -> String {
    let text = Json::string(envelope).to_compact_string();
    format!(
        r#"{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":{text}}}]}}}}"#
    )
}

fn handshake() -> MockTransport {
    MockTransport::new()
        .respond_with_header(
            200,
            r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#,
            "Mcp-Session-Id",
            "session-1",
        )
        .respond(202, "")
}

fn resolve_with(reply: Option<impl AsRef<str>>, secret: &str) -> Result<Secret, String> {
    let transport = match reply {
        Some(body) => handshake().respond(200, body.as_ref()),
        None => handshake(),
    };
    let rig = Rig::new(transport);
    let backend = rig.backend(URL);
    backend.resolve(secret, "AWSCURRENT", None)
}

#[test]
fn a_secret_is_fetched_through_initialize_initialized_and_run_script() {
    let reply = call_reply(&format!(
        r#"{{"status":"success","return_value":{{"SecretString":"{VALUE}","Name":"prod/db"}}}}"#
    ));
    let rig = Rig::new(handshake().respond(200, &reply));
    let secret = rig
        .backend(URL)
        .resolve("prod/db", "AWSCURRENT", Some("eu-west-1"))
        .unwrap();
    assert_eq!(secret.expose(), VALUE);

    let requests = rig.transport.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.url, URL);
        assert_eq!(request.method, Method::Post);
        assert_eq!(
            request.headers.get("content-type"),
            Some("application/json")
        );
        assert_eq!(
            request.headers.get("accept"),
            Some("application/json, text/event-stream")
        );
        assert_eq!(
            request.headers.get("user-agent"),
            Some("ASMExecWrapper/1.0.0")
        );
        // Signed for the endpoint's own region and service.
        assert_eq!(
            request.headers.get("authorization"),
            Some("FAKE-SIGNED us-east-1/aws-mcp")
        );
        assert_eq!(request.timeouts.read(), Duration::from_secs(30));
    }
    assert_eq!(rig.signer.signed(), 3);
    assert_eq!(requests[0].headers.get("mcp-session-id"), None);
    assert_eq!(requests[1].headers.get("mcp-session-id"), Some("session-1"));
    assert_eq!(requests[2].headers.get("mcp-session-id"), Some("session-1"));

    let body =
        |index: usize| parse_json(std::str::from_utf8(&requests[index].body).unwrap()).unwrap();
    assert_eq!(
        body(0).get("method").and_then(Json::as_str),
        Some("initialize")
    );
    assert_eq!(
        body(0)
            .get("params")
            .and_then(|p| p.get("protocolVersion"))
            .and_then(Json::as_str),
        Some("2024-11-05")
    );
    assert_eq!(
        body(1).get("method").and_then(Json::as_str),
        Some("notifications/initialized")
    );
    let call = body(2);
    assert_eq!(
        call.get("method").and_then(Json::as_str),
        Some("tools/call")
    );
    let params = call.get("params").unwrap();
    assert_eq!(
        params.get("name").and_then(Json::as_str),
        Some("aws___run_script")
    );
    let code = params
        .get("arguments")
        .and_then(|a| a.get("code"))
        .and_then(Json::as_str)
        .unwrap();
    assert_eq!(
        code,
        "result = await call_boto3(service_name='secretsmanager', \
         operation_name='GetSecretValue', params={'SecretId': 'prod/db', \
         'VersionStage': 'AWSCURRENT'}, region_name='eu-west-1')\nresult"
    );
    // The value is never part of anything we send.
    for request in &requests {
        assert!(!String::from_utf8_lossy(&request.body).contains(VALUE));
    }
}

#[test]
fn without_a_region_the_script_has_no_region_name() {
    let reply = call_reply(&format!(r#"{{"SecretString":"{VALUE}"}}"#));
    let rig = Rig::new(handshake().respond(200, &reply));
    rig.backend(URL).resolve("n", "AWSPREVIOUS", None).unwrap();
    let body = String::from_utf8(rig.transport.requests()[2].body.clone()).unwrap();
    assert!(body.contains("'VersionStage': 'AWSPREVIOUS'}"), "{body}");
    assert!(!body.contains("region_name"), "{body}");
}

#[test]
fn hostile_ids_stay_inside_their_python_string_literals() {
    let hostile = "a'b\"c\nd\\e'); import os; os.system('id') #";
    let region = "x'\ny";
    let reply = call_reply(&format!(r#"{{"SecretString":"{VALUE}"}}"#));
    let rig = Rig::new(handshake().respond(200, &reply));
    rig.backend(URL)
        .resolve(hostile, hostile, Some(region))
        .unwrap();
    let requests = rig.transport.requests();
    let request = &requests[2];
    let call = parse_json(std::str::from_utf8(&request.body).unwrap()).unwrap();
    let code = call
        .get("params")
        .and_then(|p| p.get("arguments"))
        .and_then(|a| a.get("code"))
        .and_then(Json::as_str)
        .unwrap();
    assert_eq!(code.matches('\n').count(), 1, "{code:?}");
    let expected = format!(
        "result = await call_boto3(service_name='secretsmanager', operation_name='GetSecretValue', \
         params={{'SecretId': {id}, 'VersionStage': {id}}}, region_name={region})\nresult",
        id = py_repr_str(hostile),
        region = py_repr_str(region)
    );
    assert_eq!(code, expected);
    // Independent of the helper: every quote is escaped, so the injected `)`
    // and `os.system` stay inside the string literal.
    assert!(
        code.contains(r#"'a\'b"c\nd\\e\'); import os; os.system(\'id\') #'"#),
        "{code}"
    );
    assert!(code.contains(r#"region_name="x'\ny")"#), "{code}");
}

#[test]
fn signing_service_and_region_come_from_the_endpoint_host() {
    assert_eq!(
        signing_service_region("aws-mcp.us-east-1.api.aws"),
        ("aws-mcp".to_string(), Some("us-east-1".to_string()))
    );
    assert_eq!(
        signing_service_region("x.bedrock-agentcore.us-west-2.amazonaws.com"),
        (
            "bedrock-agentcore".to_string(),
            Some("us-west-2".to_string())
        )
    );
    assert_eq!(
        signing_service_region("custom.amazonaws.com"),
        ("custom".to_string(), None)
    );
    assert_eq!(
        signing_service_region("a.b.c.api.aws"),
        ("a".to_string(), None)
    );
    assert_eq!(signing_service_region(""), (String::new(), None));
}

#[test]
fn a_host_without_a_region_uses_the_environment_then_the_default() {
    let reply = call_reply(&format!(r#"{{"SecretString":"{VALUE}"}}"#));
    for (env, expected) in [
        (MapEnv::new().with("AWS_REGION", "eu-west-2"), "eu-west-2"),
        (
            MapEnv::new().with("AWS_DEFAULT_REGION", "ap-south-1"),
            "ap-south-1",
        ),
        (MapEnv::new().with("AWS_REGION", ""), "us-east-1"),
    ] {
        let mut rig = Rig::new(handshake().respond(200, &reply));
        rig.env = env;
        rig.backend("https://custom.amazonaws.com/mcp")
            .resolve("n", "AWSCURRENT", None)
            .unwrap();
        let signed = rig.transport.requests()[0]
            .headers
            .get("authorization")
            .map(str::to_string);
        assert_eq!(signed, Some(format!("FAKE-SIGNED {expected}/custom")));
    }
    let mut rig = Rig::new(handshake());
    rig.env = MapEnv::new().with("AWS_REGION", "not a region");
    let reason = rig
        .backend("https://custom.amazonaws.com/mcp")
        .resolve("n", "AWSCURRENT", None)
        .unwrap_err();
    assert!(reason.contains("not a valid AWS region"), "{reason}");
    assert!(rig.transport.requests().is_empty());
}

#[test]
fn the_endpoint_is_validated_once_and_before_anything_is_sent() {
    for url in [
        "http://aws-mcp.us-east-1.api.aws/mcp",
        "https://evil.example.com/mcp",
        "https://user:pw@aws-mcp.us-east-1.api.aws/mcp",
        "https://aws-mcp.us-east-1.api.aws.evil.com/mcp",
        "",
        "not a url",
    ] {
        let rig = Rig::new(handshake());
        let backend = rig.backend(url);
        let first = backend.resolve("n", "AWSCURRENT", None).unwrap_err();
        let second = backend.resolve("n", "AWSCURRENT", None).unwrap_err();
        assert_eq!(first, second, "{url}");
        assert!(first.contains(ENDPOINT_SETTING), "{first}");
        assert!(!first.contains("pw@"), "{first}");
        assert!(rig.transport.requests().is_empty(), "{url}");
        assert_eq!(rig.signer.signed(), 0);
    }
    // A host that resolves to a private address is refused, once.
    let mut rig = Rig::new(handshake());
    rig.resolver = FixedResolver::new().answer_any(&["10.0.0.5"]);
    let backend = rig.backend(URL);
    assert!(backend.resolve("n", "AWSCURRENT", None).is_err());
    assert!(backend.resolve("n", "AWSCURRENT", None).is_err());
    assert_eq!(rig.resolver.lookups().len(), 1);
    assert!(rig.transport.requests().is_empty());
}

#[test]
fn rejections_status_codes_and_transport_faults_have_distinct_causes() {
    let rejected =
        MockTransport::new().respond(200, r#"{"error":{"code":-32600,"message":"nope"}}"#);
    let rig = Rig::new(rejected);
    assert_eq!(
        rig.backend(URL)
            .resolve("n", "AWSCURRENT", None)
            .unwrap_err(),
        "RuntimeError: MCP initialize rejected: nope"
    );
    assert_eq!(rig.transport.requests().len(), 1);

    let call_rejected = handshake().respond(200, r#"{"error":{"message":"boom"}}"#);
    let rig = Rig::new(call_rejected);
    assert_eq!(
        rig.backend(URL)
            .resolve("n", "AWSCURRENT", None)
            .unwrap_err(),
        "RuntimeError: aws___run_script call rejected: boom"
    );

    let rig = Rig::new(MockTransport::new().respond(403, "denied"));
    assert_eq!(
        rig.backend(URL)
            .resolve("n", "AWSCURRENT", None)
            .unwrap_err(),
        "HTTP 403 from MCP endpoint (Forbidden)"
    );

    let rig = Rig::new(MockTransport::new().fail(AwsError::timeout(
        toolkit_aws::error::TimeoutPhase::Read,
        "no answer",
    )));
    let reason = rig
        .backend(URL)
        .resolve("n", "AWSCURRENT", None)
        .unwrap_err();
    assert!(
        reason.starts_with("timed out after 30.0s -- raise ASM_EXEC_MCP_TIMEOUT"),
        "{reason}"
    );

    let rig = Rig::new(MockTransport::new().fail(AwsError::connect("refused")));
    let reason = rig
        .backend(URL)
        .resolve("n", "AWSCURRENT", None)
        .unwrap_err();
    assert_eq!(reason, "cannot reach MCP endpoint: refused");

    let rig = Rig::new(MockTransport::new().respond(200, "<html>"));
    let reason = rig
        .backend(URL)
        .resolve("n", "AWSCURRENT", None)
        .unwrap_err();
    assert!(reason.starts_with("JSONDecodeError"), "{reason}");
}

#[test]
fn a_signing_failure_sends_nothing() {
    let mut rig = Rig::new(handshake());
    rig.signer = FakeSigner::failing(AwsError::NoCredentials);
    assert_eq!(
        rig.backend(URL)
            .resolve("n", "AWSCURRENT", None)
            .unwrap_err(),
        "RuntimeError: no AWS credentials available for MCP signing"
    );
    assert!(rig.transport.requests().is_empty());

    let mut rig = Rig::new(handshake());
    rig.signer = FakeSigner::failing(AwsError::provider("sigv4", "unavailable"));
    let reason = rig
        .backend(URL)
        .resolve("n", "AWSCURRENT", None)
        .unwrap_err();
    assert!(reason.contains("unavailable"), "{reason}");
    assert!(rig.transport.requests().is_empty());
}

#[test]
fn the_secret_string_is_found_where_run_script_nests_it() {
    for envelope in [
        format!(r#"{{"SecretString":"{VALUE}"}}"#),
        format!(r#"{{"status":"success","return_value":{{"SecretString":"{VALUE}"}}}}"#),
        // return_value as JSON text.
        format!(r#"{{"return_value":"{{\"SecretString\":\"{VALUE}\"}}"}}"#),
        format!(r#"{{"result":{{"output":{{"stdout":{{"SecretString":"{VALUE}"}}}}}}}}"#),
        // Unparseable text under a key is skipped; the next key is tried.
        format!(r#"{{"stdout":"not json","results":{{"SecretString":"{VALUE}"}}}}"#),
    ] {
        let secret = resolve_with(Some(&call_reply(&envelope)), "n").unwrap();
        assert_eq!(secret.expose(), VALUE, "{envelope}");
    }
    // The first envelope without one is skipped, the second is used; and a
    // reply whose `result` is the envelope itself works too.
    let two = format!(
        r#"{{"result":{{"content":[{{"type":"image","text":"x"}},{{"type":"text","text":"junk"}},{{"type":"text","text":"{{\"SecretString\":\"{VALUE}\"}}"}}]}}}}"#
    );
    assert_eq!(resolve_with(Some(&two), "n").unwrap().expose(), VALUE);
    let bare = format!(
        r#"{{"result":{{"status":"success","return_value":{{"SecretString":"{VALUE}"}}}}}}"#
    );
    assert_eq!(resolve_with(Some(&bare), "n").unwrap().expose(), VALUE);
}

#[test]
fn run_script_errors_and_missing_secrets_are_reported_by_cause() {
    let reason = resolve_with(
        Some(&call_reply(
            r#"{"status":"error","error":"AccessDeniedException: not allowed"}"#,
        )),
        "n",
    )
    .unwrap_err();
    assert_eq!(
        reason,
        "RuntimeError: run_script reported an error: AccessDeniedException: not allowed"
    );
    let reason = resolve_with(
        Some(&call_reply(
            r#"{"status":"error","error":"","stderr":"Traceback..."}"#,
        )),
        "n",
    )
    .unwrap_err();
    assert!(reason.ends_with("Traceback..."), "{reason}");
    let reason = resolve_with(Some(&call_reply(r#"{"status":"error"}"#)), "n").unwrap_err();
    assert!(reason.ends_with("unknown error"), "{reason}");

    for reply in [
        Some(r#"{"jsonrpc":"2.0","id":2}"#.to_string()),
        Some(r#"{"result":{}}"#.to_string()),
        Some(r#"{"result":"text"}"#.to_string()),
        Some(r#"{"result":{"content":"text"}}"#.to_string()),
        Some(call_reply(r#"{"SecretString":""}"#)),
        Some(call_reply(r#"{"SecretString":null}"#)),
        Some(call_reply(r#"{"SecretString":5}"#)),
        Some(call_reply(r#"{"SecretBinary":"AAAA"}"#)),
        Some(call_reply(r#"["SecretString"]"#)),
        Some(call_reply(
            r#"{"return_value":[{"SecretString":"hidden-in-array"}]}"#,
        )),
    ] {
        let reason = resolve_with(reply.as_deref(), "n").unwrap_err();
        assert!(
            reason.starts_with("RuntimeError: no SecretString in run_script response"),
            "{reply:?}: {reason}"
        );
    }
}

#[test]
fn deep_nesting_is_bounded() {
    let mut envelope = format!(r#"{{"SecretString":"{VALUE}"}}"#);
    for _ in 0..40 {
        envelope = format!(r#"{{"return_value":{envelope}}}"#);
    }
    assert!(resolve_with(Some(&call_reply(&envelope)), "n").is_err());
}

#[test]
fn debug_output_has_no_endpoint_or_secret() {
    let rig = Rig::new(handshake());
    let text = format!(
        "{:?}",
        rig.backend("https://user:pw@aws-mcp.us-east-1.api.aws/mcp")
    );
    assert!(!text.contains("pw"), "{text}");
}
