//! T-TOK and T-CODE at the command seam (R-TOK-RED, R-API-CODE, #5197): every
//! seeded secret echoed by the service through every channel a response has
//! (error header, error body keys, request id, modelled success members,
//! the `list-launches` per-region errors) never reaches stdout, stderr or a
//! `Debug` rendering. Assertion messages never print the inspected text, so a
//! failing run cannot itself leak a seeded value.

use super::tests_flow::{ok, req, session, Harness, ACCESS, CLIENT_SECRET, NOW, REFRESH, SMALL_LAUNCH, UPLOAD};
use super::*;
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport};

/// Every value a command may know that must never be shown.
const SEEDED: [&str; 3] = [ACCESS, REFRESH, CLIENT_SECRET];

fn assert_clean(label: &str, texts: &[&str], secrets: &[&str]) {
    for (index, secret) in secrets.iter().enumerate() {
        for text in texts {
            assert!(!text.contains(secret), "{label}: seeded secret #{index} leaked");
        }
    }
}

/// Run `command` against `fake` and check stdout, stderr and the `Debug` of
/// the result for every seeded secret; returns (stdout, stderr, exit).
fn check(label: &str, command: &str, fake: FakeStreamingTransport, extra: &[&str]) -> (String, String, u8) {
    let harness = Harness { fake, ..Harness::new(&[]) };
    let first = if command == "create-launch" { "/src/app" } else { "l-1" };
    let request = Request { region: Some("us-east-1"), ..req(command, Some(first)) };
    let (stdout, stderr, status, debug) = harness.golden_debug(&request);
    let mut secrets: Vec<&str> = SEEDED.to_vec();
    secrets.extend_from_slice(extra);
    assert_clean(label, &[&stdout, &stderr, &debug], &secrets);
    (stdout, stderr, status)
}

fn expect_generic(result: (String, String, u8)) {
    assert_eq!((result.1.as_str(), result.2), (&*format!("{{\"error\": \"{GENERIC_FAILURE}\"}}\n"), 1));
}

fn error_reply(status: u16, headers: &[(&str, &str)], body: &str) -> FakeStreamingTransport {
    FakeStreamingTransport::new().respond(status, headers, vec![FakeChunk::data(body.as_bytes())])
}

#[test]
fn t_tok_1_2_6_error_channels_never_echo_seeded_secrets() {
    for command in COMMANDS {
        for secret in SEEDED {
            let long = format!("{}{secret}", "ResourceNotFoundException".repeat(5));
            let cases: Vec<(String, FakeStreamingTransport)> = vec![
                ("x-amzn-ErrorType".into(), error_reply(400, &[("x-amzn-ErrorType", secret)], "{}")),
                ("x-amzn-RequestId".into(), error_reply(404, &[("x-amzn-RequestId", secret)], "{}")),
                ("long prefix".into(), error_reply(400, &[("x-amzn-ErrorType", long.as_str())], "{}")),
                ("message".into(), error_reply(400, &[], &format!(r#"{{"message":"{secret}","__type":"ValidationException"}}"#))),
            ]
            .into_iter()
            .chain(["__type", "code", "Code", "errorCode"].into_iter().map(|key| {
                (key.to_owned(), error_reply(500, &[], &format!(r#"{{"{key}":"{secret}"}}"#)))
            }))
            .collect();
            for (channel, fake) in cases {
                let (stdout, stderr, status) = check(&format!("{command} via {channel}"), command, fake, &[]);
                assert_eq!((stdout.as_str(), status), ("", 1));
                assert!(stderr.starts_with("{\"error\": \""));
            }
        }
    }
}

#[test]
fn t_tok_1_2_success_members_echoing_a_secret_fail_closed() {
    for secret in SEEDED {
        let launch = SMALL_LAUNCH.replace("\"name\":\"n\"", &format!("\"name\":\"prefix-{secret}-suffix\""));
        for command in ["get-launch", "start-launch-execution"] {
            let (_, stderr, status) = check(command, command, error_reply(200, &[], &launch), &[]);
            assert_eq!((stderr.as_str(), status), (&*format!("{{\"error\": \"{GENERIC_FAILURE}\"}}\n"), 1));
        }
        let download = SMALL_LAUNCH.replace("\"status\"", &format!("\"downloadUrl\":\"https://dl.example/x?t={secret}\",\"status\""));
        expect_generic(check("download url", "get-launch-download-url", error_reply(200, &[], &download), &[]));
        let failure = SMALL_LAUNCH.replace("\"status\":\"created\"", &format!("\"status\":\"failed\",\"failureReason\":{{\"phase\":\"plan\",\"code\":\"X\",\"message\":\"{secret}\"}}"));
        expect_generic(check("status", "get-launch-status", error_reply(200, &[], &failure), &[]));
        let list = format!(r#"{{"items":[],"nextToken":"{secret}"}}"#);
        expect_generic(check("nextToken", "list-launches", error_reply(200, &[], &list), &[]));
        // A secret nested in a projected object.
        let keyed = SMALL_LAUNCH.replace("\"status\":\"created\"", &format!("\"status\":\"failed\",\"costEstimate\":{{\"estimatedMonthlyCost\":1,\"region\":\"{secret}\",\"services\":[]}}"));
        expect_generic(check("nested", "get-launch-status", error_reply(200, &[], &keyed), &[]));
    }
}

#[test]
fn t_tok_upload_url_signature_and_client_tokens_never_shown() {
    let signature = "FAKE-UPLOAD-SIGNATURE";
    let credential = "FAKE-UPLOAD-CREDENTIAL";
    // CreateLaunch echoes the presigned signature (percent-decoded form) and
    // the credential in modelled members.
    for echoed in [signature, credential, "FAKE%2DUPLOAD%2DSIGNATURE"] {
        let launch = SMALL_LAUNCH.replace("\"name\":\"n\"", &format!("\"name\":\"{echoed}\""));
        let fake = FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(UPLOAD.as_bytes())])
            .respond(200, &[], vec![])
            .respond(201, &[], vec![FakeChunk::data(launch.as_bytes())]);
        let (_, _, status) = check("create echo", "create-launch", fake, &[signature, credential, "bucket.s3"]);
        assert_eq!(status, 1);
    }
    // A failing PUT (status, connect, timeout) never names the URL.
    for put in [
        FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(UPLOAD.as_bytes())])
            .respond(500, &[], vec![FakeChunk::data(signature.as_bytes())]),
        FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(UPLOAD.as_bytes())])
            .open_error(toolkit_aws::AwsError::connect(format!("connect to {UPLOAD} failed"))),
    ] {
        check("put failure", "create-launch", put, &[signature, credential, "bucket.s3", "amazonaws"]);
    }
    // The clientToken of StartLaunchExecution echoed back.
    let harness = Harness::new(&[]);
    let token = client_token(&crate::launch::clock::SeededRandom::new(7)).unwrap();
    let echoed = SMALL_LAUNCH.replace("\"name\":\"n\"", &format!("\"name\":\"{token}\""));
    let fake = error_reply(202, &[], &echoed);
    let harness = Harness { fake, ..harness };
    let (stdout, stderr, status) = harness.golden(&req("start-launch-execution", Some("l-1")));
    assert_eq!(status, 1);
    assert_clean("client token", &[&stdout, &stderr], &[&token]);
}

#[test]
fn t_tok_refreshed_token_and_internal_errors_are_scrubbed() {
    // The new access token from a refresh, echoed in a launch name.
    let fresh = "FAKE-ACCESS-NEW-0002";
    let launch = SMALL_LAUNCH.replace("\"name\":\"n\"", &format!("\"name\":\"{fresh}\""));
    let harness = Harness::without_session(&[
        ok(200, &format!(r#"{{"accessToken":"{fresh}","expiresIn":3600}}"#)),
        ok(200, &launch),
    ]);
    harness.seed(&session(NOW + 10));
    let (stdout, stderr, status) = harness.golden(&req("get-launch", Some("l-1")));
    assert_eq!(status, 1);
    assert_clean("refreshed", &[&stdout, &stderr], &[fresh, ACCESS, REFRESH, CLIENT_SECRET]);
    // T-CODE-7: an internal error whose text holds a live secret.
    for secret in SEEDED {
        let harness = Harness::new(&[]);
        *harness.archive_result.borrow_mut() = Err(format!("archive failed near {secret}"));
        assert_eq!(
            harness.golden(&req("create-launch", Some("/src/app"))).1,
            format!("{{\"error\": \"{GENERIC_FAILURE}\"}}\n")
        );
    }
    // A refresh rejected with a secret as its OIDC error code.
    let harness = Harness::without_session(&[ok(400, &format!(r#"{{"error":"{REFRESH}","error_description":"{CLIENT_SECRET}"}}"#))]);
    harness.seed(&session(NOW + 10));
    let (stdout, stderr, status) = harness.golden(&req("get-launch", Some("l-1")));
    assert_eq!(status, 1);
    assert_clean("refresh error", &[&stdout, &stderr], &SEEDED);
}

#[test]
fn t_tok_6_debug_of_every_launch_error_and_secret_holder() {
    let shown = [
        format!("{:?}", ApiError::Status { status: 400, code: "UnrecognizedError", operation: "GET /api/launches/<id>" }),
        format!("{:?}", ApiError::Lookup { status: 404, code: "UnrecognizedError", hint: "" }),
        format!("{:?}", session(NOW)),
        format!("{:?}", crate::launch::LaunchError::Config(GENERIC_FAILURE.into())),
        format!("{:?}", Secret::new(ACCESS)),
        format!("{:?}", crate::launch::oidc::OidcError::SessionExpired),
    ];
    let joined = shown.join(" ");
    assert_clean("debug", &[&joined], &SEEDED);
    // The scrub list itself never formats its contents.
    let mut secrets = Secrets::default();
    secrets.add(ACCESS);
    secrets.add("short");
    assert!(secrets.contains(&format!("x{ACCESS}y")));
    assert!(!secrets.contains("short"));
    assert_eq!(secrets.scrub(format!("a {ACCESS}")), GENERIC_FAILURE);
    assert_eq!(secrets.scrub("plain".into()), "plain");
    assert!(secrets.found_in(&Json::object(vec![(ACCESS, Json::Null)])));
    assert!(secrets.found_in(&Json::Array(vec![Json::string(format!("[{ACCESS}]"))])));
    assert!(!secrets.found_in(&Json::object(vec![("k", Json::Int(1))])));
    secrets.add_url("https://h.example/p?a=FAKE%2DSIG%2D0003&b=c+d+e+f+g+h#frag");
    for text in ["FAKE%2DSIG%2D0003", "FAKE-SIG-0003", "c d e f g h", "a=FAKE%2DSIG%2D0003&b=c+d+e+f+g+h"] {
        assert!(secrets.contains(text));
    }
}

#[test]
fn t_code_4_list_region_error_with_secret_code_is_unrecognized() {
    for secret in SEEDED {
        let fake = FakeStreamingTransport::new()
            .respond(500, &[("x-amzn-ErrorType", secret)], vec![FakeChunk::data(b"{}")])
            .respond(200, &[], vec![FakeChunk::data(br#"{"items":[]}"#)]);
        let harness = Harness { fake, ..Harness::new(&[]) };
        let (stdout, stderr, status) = harness.golden(&req("list-launches", None));
        assert_eq!(status, 0);
        assert_eq!(
            stdout,
            "{\n  \"items\": [],\n  \"errors\": {\n    \"us-east-1\": \"GET /api/launches failed (500): UnrecognizedError\"\n  }\n}\n"
        );
        assert_clean("list errors", &[&stdout, &stderr], &SEEDED);
    }
}
