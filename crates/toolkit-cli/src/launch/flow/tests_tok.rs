//! T-TOK and T-CODE at the command seam (R-TOK-RED, R-API-CODE, #5197): every
//! seeded secret echoed by the service through every channel a response has
//! (error header, error body keys, request id, modelled success members,
//! the `list-launches` per-region errors) never reaches stdout, stderr or a
//! `Debug` rendering. Assertion messages never print the inspected text, so a
//! failing run cannot itself leak a seeded value.

use super::tests_flow::{
    ok, req, session, Harness, ACCESS, CLIENT_SECRET, NOW, REFRESH, SMALL_LAUNCH, UPLOAD,
};
use super::*;
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport};
use toolkit_aws::Secret;

/// Every value a command may know that must never be shown.
const SEEDED: [&str; 3] = [ACCESS, REFRESH, CLIENT_SECRET];

fn assert_clean(label: &str, texts: &[&str], secrets: &[&str]) {
    for (index, secret) in secrets.iter().enumerate() {
        for text in texts {
            assert!(
                !text.contains(secret),
                "{label}: seeded secret #{index} leaked"
            );
        }
    }
}

/// Run `command` against `fake` and check stdout, stderr and the `Debug` of
/// the result for every seeded secret; returns (stdout, stderr, exit).
fn check(
    label: &str,
    command: &str,
    fake: FakeStreamingTransport,
    extra: &[&str],
) -> (String, String, u8) {
    let harness = Harness {
        fake,
        ..Harness::new(&[])
    };
    let first = if command == "create-launch" {
        "/src/app"
    } else {
        "l-1"
    };
    let request = Request {
        region: Some("us-east-1"),
        ..req(command, Some(first))
    };
    let (stdout, stderr, status, debug) = harness.golden_debug(&request);
    let mut secrets: Vec<&str> = SEEDED.to_vec();
    secrets.extend_from_slice(extra);
    assert_clean(label, &[&stdout, &stderr, &debug], &secrets);
    (stdout, stderr, status)
}

fn expect_generic(result: &(String, String, u8)) {
    let generic = format!("{{\"error\": \"{GENERIC_FAILURE}\"}}\n");
    assert!(
        result.1 == generic && result.2 == 1,
        "expected the generic failure"
    );
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
                (
                    "x-amzn-ErrorType".into(),
                    error_reply(400, &[("x-amzn-ErrorType", secret)], "{}"),
                ),
                (
                    "x-amzn-RequestId".into(),
                    error_reply(404, &[("x-amzn-RequestId", secret)], "{}"),
                ),
                (
                    "long prefix".into(),
                    error_reply(400, &[("x-amzn-ErrorType", long.as_str())], "{}"),
                ),
                (
                    "message".into(),
                    error_reply(
                        400,
                        &[],
                        &format!(r#"{{"message":"{secret}","__type":"ValidationException"}}"#),
                    ),
                ),
            ]
            .into_iter()
            .chain(
                ["__type", "code", "Code", "errorCode"]
                    .into_iter()
                    .map(|key| {
                        (
                            key.to_owned(),
                            error_reply(500, &[], &format!(r#"{{"{key}":"{secret}"}}"#)),
                        )
                    }),
            )
            .collect();
            for (channel, fake) in cases {
                let (stdout, stderr, status) =
                    check(&format!("{command} via {channel}"), command, fake, &[]);
                assert!(
                    stdout.is_empty() && status == 1,
                    "{command} via {channel}: expected a failure"
                );
                assert!(stderr.starts_with("{\"error\": \""));
            }
        }
    }
}

#[test]
fn t_tok_1_2_success_members_echoing_a_secret_fail_closed() {
    for secret in SEEDED {
        let launch = SMALL_LAUNCH.replace(
            "\"name\":\"n\"",
            &format!("\"name\":\"prefix-{secret}-suffix\""),
        );
        for command in ["get-launch", "start-launch-execution"] {
            let (_, stderr, status) = check(command, command, error_reply(200, &[], &launch), &[]);
            expect_generic(&(String::new(), stderr, status));
        }
        let download = SMALL_LAUNCH.replace(
            "\"status\"",
            &format!("\"downloadUrl\":\"https://dl.example/x?t={secret}\",\"status\""),
        );
        expect_generic(&check(
            "download url",
            "get-launch-download-url",
            error_reply(200, &[], &download),
            &[],
        ));
        let failure = SMALL_LAUNCH.replace("\"status\":\"created\"", &format!("\"status\":\"failed\",\"failureReason\":{{\"phase\":\"plan\",\"code\":\"X\",\"message\":\"{secret}\"}}"));
        expect_generic(&check(
            "status",
            "get-launch-status",
            error_reply(200, &[], &failure),
            &[],
        ));
        let list = format!(r#"{{"items":[],"nextToken":"{secret}"}}"#);
        expect_generic(&check(
            "nextToken",
            "list-launches",
            error_reply(200, &[], &list),
            &[],
        ));
        // A secret nested in a projected object.
        let keyed = SMALL_LAUNCH.replace("\"status\":\"created\"", &format!("\"status\":\"failed\",\"costEstimate\":{{\"estimatedMonthlyCost\":1,\"region\":\"{secret}\",\"services\":[]}}"));
        expect_generic(&check(
            "nested",
            "get-launch-status",
            error_reply(200, &[], &keyed),
            &[],
        ));
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
        let (_, _, status) = check(
            "create echo",
            "create-launch",
            fake,
            &[signature, credential, "bucket.s3"],
        );
        assert_eq!(status, 1);
    }
    // A failing PUT (status, connect, timeout) never names the URL.
    for put in [
        FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(UPLOAD.as_bytes())])
            .respond(500, &[], vec![FakeChunk::data(signature.as_bytes())]),
        FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(UPLOAD.as_bytes())])
            .open_error(toolkit_aws::AwsError::connect(format!(
                "connect to {UPLOAD} failed"
            ))),
    ] {
        check(
            "put failure",
            "create-launch",
            put,
            &[signature, credential, "bucket.s3", "amazonaws"],
        );
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
        ok(
            200,
            &format!(r#"{{"accessToken":"{fresh}","expiresIn":3600}}"#),
        ),
        ok(200, &launch),
    ]);
    harness.seed(&session(NOW + 10));
    let (stdout, stderr, status) = harness.golden(&req("get-launch", Some("l-1")));
    assert_eq!(status, 1);
    assert_clean(
        "refreshed",
        &[&stdout, &stderr],
        &[fresh, ACCESS, REFRESH, CLIENT_SECRET],
    );
    // T-CODE-7: an internal error whose text holds a live secret.
    for secret in SEEDED {
        let harness = Harness::new(&[]);
        *harness.archive_result.borrow_mut() = Err(format!("archive failed near {secret}"));
        let (_, stderr, status) = harness.golden(&req("create-launch", Some("/src/app")));
        expect_generic(&(String::new(), stderr, status));
    }
    // A refresh rejected with a secret as its OIDC error code.
    let harness = Harness::without_session(&[ok(
        400,
        &format!(r#"{{"error":"{REFRESH}","error_description":"{CLIENT_SECRET}"}}"#),
    )]);
    harness.seed(&session(NOW + 10));
    let (stdout, stderr, status) = harness.golden(&req("get-launch", Some("l-1")));
    assert_eq!(status, 1);
    assert_clean("refresh error", &[&stdout, &stderr], &SEEDED);
}

#[test]
fn t_tok_6_debug_of_every_launch_error_and_secret_holder() {
    let shown = [
        format!(
            "{:?}",
            ApiError::Status {
                status: 400,
                code: "UnrecognizedError",
                operation: "GET /api/launches/<id>"
            }
        ),
        format!("{:?}", session(NOW)),
        format!(
            "{:?}",
            crate::launch::LaunchError::Config(GENERIC_FAILURE.into())
        ),
        format!("{:?}", Secret::new(ACCESS)),
        format!("{:?}", crate::launch::oidc::OidcError::SessionExpired),
        // The callback's authorization-code holder (#5504).
        format!(
            "{:?}",
            crate::launch::callback::Outcome::Code(REFRESH.into())
        ),
    ];
    let joined = shown.join(" ");
    assert_clean("debug", &[&joined], &SEEDED);
    // The scrub list itself never formats its contents.
    let mut secrets = Secrets::default();
    secrets.add(ACCESS);
    secrets.add("short");
    assert!(secrets.contains(&format!("x{ACCESS}y")));
    assert!(!secrets.contains("short"));
    assert!(
        secrets.scrub(format!("a {ACCESS}")) == GENERIC_FAILURE,
        "scrub kept a secret"
    );
    assert_eq!(secrets.scrub("plain".into()), "plain");
    assert!(secrets.found_in(&Json::object(vec![(ACCESS, Json::Null)])));
    assert!(secrets.found_in(&Json::Array(vec![Json::string(format!("[{ACCESS}]"))])));
    assert!(!secrets.found_in(&Json::object(vec![("k", Json::Int(1))])));
    secrets.add_url("https://h.example/p?a=FAKE%2DSIG%2D0003&b=c+d+e+f+g+h#frag");
    for text in [
        "FAKE%2DSIG%2D0003",
        "FAKE-SIG-0003",
        "c d e f g h",
        "a=FAKE%2DSIG%2D0003&b=c+d+e+f+g+h",
    ] {
        assert!(secrets.contains(text));
    }
}

#[test]
fn t_code_4_list_region_error_with_secret_code_is_unrecognized() {
    for secret in SEEDED {
        let fake = FakeStreamingTransport::new()
            .respond(
                500,
                &[("x-amzn-ErrorType", secret)],
                vec![FakeChunk::data(b"{}")],
            )
            .respond(200, &[], vec![FakeChunk::data(br#"{"items":[]}"#)]);
        let harness = Harness {
            fake,
            ..Harness::new(&[])
        };
        let (stdout, stderr, status) = harness.golden(&req("list-launches", None));
        assert_eq!(status, 0);
        assert!(
            stdout == "{\n  \"items\": [],\n  \"errors\": {\n    \"us-east-1\": \"GET /api/launches failed (500): UnrecognizedError\"\n  }\n}\n",
            "unexpected list output"
        );
        assert_clean("list errors", &[&stdout, &stderr], &SEEDED);
    }
}

/// One early refusal (#5406): the request and the environment it runs in.
struct Early<'a> {
    label: String,
    request: Request<'a>,
    env: toolkit_aws::MapEnv,
    /// Whether the refusal message names the offending value (so the
    /// stored-secret scrub must replace it with the generic failure).
    echoes: bool,
}

/// Every refusal `run` makes before the access token, with a stored session
/// secret as the offending value: an explicit `--region`, the
/// `LAUNCH_WITH_AWS_REGION` chain, the `LAUNCH_WITH_AWS_BASE_URL` override
/// (both before and after its host check), the repository URL and the
/// fixed-message argument checks (launch id, display name).
fn early_cases<'a>(command: &'a str, secret: &'a str, owned: &'a [String; 4]) -> Vec<Early<'a>> {
    let first = if command == "create-launch" {
        "/src/app"
    } else {
        "l-1"
    };
    let none = toolkit_aws::MapEnv::new;
    let mut cases = vec![
        Early {
            label: format!("{command}: explicit region"),
            request: Request {
                region: Some(secret),
                ..req(command, Some(first))
            },
            env: none(),
            echoes: true,
        },
        Early {
            label: format!("{command}: region environment"),
            request: req(command, Some(first)),
            env: none().with(config::ENV_REGION, secret),
            echoes: true,
        },
        Early {
            label: format!("{command}: base URL without host"),
            request: req(command, Some(first)),
            env: none().with(config::ENV_BASE_URL, &owned[0]),
            echoes: true,
        },
        Early {
            label: format!("{command}: base URL host"),
            request: req(command, Some(first)),
            env: none().with(config::ENV_BASE_URL, &owned[1]),
            echoes: false,
        },
    ];
    if command == "create-launch" {
        cases.push(Early {
            label: "create-launch: repository URL".into(),
            request: req(command, Some(&owned[2])),
            env: none(),
            echoes: true,
        });
        cases.push(Early {
            label: "create-launch: display name".into(),
            request: Request {
                second: Some(&owned[3]),
                ..req(command, Some(first))
            },
            env: none(),
            echoes: false,
        });
    } else if command != "list-launches" {
        cases.push(Early {
            label: format!("{command}: launch id"),
            request: req(command, Some(&owned[3])),
            env: none(),
            echoes: false,
        });
    }
    cases
}

#[test]
fn t_tok_7_early_refusals_never_echo_stored_session_secrets() {
    // Every leaking case is named (label and secret index, never the text),
    // so a red run shows the whole matrix, not only its first case.
    let mut leaked = Vec::new();
    for (number, secret) in SEEDED.into_iter().enumerate() {
        let owned = [
            format!("https:///{secret}"),
            format!("https://{secret}.example.com"),
            format!("https://github.com/o/{secret}"),
            secret.repeat(100),
        ];
        for command in COMMANDS {
            for case in early_cases(command, secret, &owned) {
                let harness = Harness {
                    env: case.env,
                    ..Harness::new(&[])
                };
                let (stdout, stderr, status, debug) = harness.golden_debug(&case.request);
                if [&stdout, &stderr, &debug]
                    .iter()
                    .any(|text| SEEDED.iter().any(|s| text.contains(s)))
                {
                    leaked.push(format!("{} (secret #{number})", case.label));
                    continue;
                }
                assert!(
                    stdout.is_empty() && status == 1 && stderr.starts_with("{\"error\": \""),
                    "{}: expected a refusal",
                    case.label
                );
                if case.echoes {
                    assert!(
                        stderr == format!("{{\"error\": \"{GENERIC_FAILURE}\"}}\n"),
                        "{}: expected the generic failure",
                        case.label
                    );
                }
                assert!(
                    harness.fake.requests().is_empty(),
                    "{}: a request was made",
                    case.label
                );
                assert_eq!(
                    harness.archive_calls.get(),
                    0,
                    "{}: archive built",
                    case.label
                );
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "{} early refusals leaked a seeded secret: {leaked:?}",
        leaked.len()
    );
}

#[test]
fn t_tok_7_missing_or_unreadable_session_keeps_early_refusals() {
    // No session record: the scrub set is empty and the refusal is the
    // ordinary message (nothing to hide, exit behaviour unchanged).
    let harness = Harness::without_session(&[]);
    let request = Request {
        region: Some("mars-1"),
        ..req("get-launch", Some("l-1"))
    };
    let expected = config::resolve_region(&RegionInputs {
        explicit: Some("mars-1"),
        aws_mcp_url: None,
        env: &harness.env,
        state_dir: None,
        home: None,
    })
    .unwrap_err()
    .to_string();
    assert_eq!(harness.golden(&request), failed_line(&expected));
    // An unreadable record (not JSON) is the same empty set.
    std::fs::write(harness.scratch.path().join(state::SESSION_FILE_NAME), b"{").unwrap();
    assert_eq!(harness.golden(&request), failed_line(&expected));
    assert!(harness.fake.requests().is_empty());
}

fn failed_line(message: &str) -> (String, String, u8) {
    (
        String::new(),
        format!("{}\n", crate::launch::text::error_text(message)),
        1,
    )
}
