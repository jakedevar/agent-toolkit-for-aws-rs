//! Command-seam tests of the network commands (T-CMD-1..13, T-API at the
//! command seam): exact stdout/stderr/exit per command and branch of section
//! 1.3, rendered through the same writers the binary uses, over
//! `FakeStreamingTransport`, a seeded session directory and a fixed resolver.
//! No network, credentials, AWS or Python.

use std::cell::{Cell, RefCell};

use super::*;
use crate::launch::clock::SeededRandom;
use crate::launch::http::SystemClock;
use crate::launch::state::StoredSession;
use crate::launch::tests_support::Scratch;
use toolkit_aws::retry::{NoJitter, RetryPolicy};
use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, FixedResolver, RecordingSleeper};
use toolkit_aws::transport::Method;
use toolkit_aws::{AwsError, MapEnv, Secret};

pub(super) const NOW: i64 = 1_800_000_000;
pub(super) const ACCESS: &str = "FAKE-ACCESS-TOKEN-0001";
pub(super) const REFRESH: &str = "FAKE-REFRESH-TOKEN-0001";
pub(super) const CLIENT_SECRET: &str = "FAKE-CLIENT-SECRET-0001";
pub(super) const US: &str = "https://launch-with-aws.us-east-1.api.aws";
pub(super) const EU: &str = "https://launch-with-aws.eu-central-1.api.aws";
pub(super) const OVERRIDE: &str = "https://launch.test.api.aws";

pub(super) const LAUNCH: &str = r#"{"launch":{"updatedAt":"2026-06-15T12:34:56.5Z","createdAt":"2026-06-15T12:34:56Z","status":"completed","name":"café","id":"l-1","source":{"s3Upload":{"uploadId":"u-1"}},"contextInputs":[],"costEstimate":{"services":[{"monthlyCost":1.5,"service":"Lambda"}],"region":"us-east-1","estimatedMonthlyCost":12},"downloadUrl":"https://dl.example/a?X-Amz-Signature=abc","unknown":1}}"#;

pub(super) const LAUNCH_GOLDEN: &str = r#"{
  "id": "l-1",
  "name": "caf\u00e9",
  "status": "completed",
  "source": {
    "s3Upload": {
      "uploadId": "u-1"
    }
  },
  "costEstimate": {
    "estimatedMonthlyCost": 12.0,
    "region": "us-east-1",
    "services": [
      {
        "service": "Lambda",
        "monthlyCost": 1.5
      }
    ]
  },
  "downloadUrl": "https://dl.example/a?X-Amz-Signature=abc",
  "createdAt": "2026-06-15 12:34:56+00:00",
  "updatedAt": "2026-06-15 12:34:56.500000+00:00",
  "region": "us-east-1",
  "regionName": "US East (N. Virginia)"
}
"#;

pub(super) const SMALL_LAUNCH: &str = r#"{"launch":{"id":"l-1","name":"n","status":"created","source":{"s3Upload":{"uploadId":"u-1"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z"}}"#;

pub(super) const SMALL_GOLDEN: &str = r#"{
  "id": "l-1",
  "name": "n",
  "status": "created",
  "source": {
    "s3Upload": {
      "uploadId": "u-1"
    }
  },
  "createdAt": "2026-06-15 12:34:56+00:00",
  "updatedAt": "2026-06-15 12:34:56+00:00",
  "region": "us-east-1",
  "regionName": "US East (N. Virginia)"
}
"#;

pub(super) const UPLOAD: &str = r#"{"uploadUrl":"https://bucket.s3.us-east-1.amazonaws.com/k?X-Amz-Credential=FAKE-UPLOAD-CREDENTIAL&X-Amz-Signature=FAKE%2DUPLOAD%2DSIGNATURE","uploadId":"u-1"}"#;

pub(super) fn session(token_expires_at: i64) -> StoredSession {
    StoredSession {
        client_id: "client-id".into(),
        client_secret: Secret::new(CLIENT_SECRET),
        client_expires_at: NOW + 86_400,
        authorize_endpoint: "https://oidc.us-east-1.amazonaws.com/authorize".into(),
        token_endpoint: "https://oidc.us-east-1.amazonaws.com/token".into(),
        scopes: vec!["launch:all".into()],
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH),
        token_expires_at,
    }
}

pub(super) fn ok(status: u16, body: &str) -> (u16, String) {
    (status, body.to_owned())
}

/// One command run with its fakes.
pub(super) struct Harness {
    pub scratch: Scratch,
    pub env: MapEnv,
    pub fake: FakeStreamingTransport,
    pub resolver: FixedResolver,
    pub random: SeededRandom,
    pub sleeper: RecordingSleeper,
    pub policy: RetryPolicy,
    pub archive_calls: Cell<usize>,
    pub archive_result: RefCell<Result<Vec<u8>, String>>,
}

impl Harness {
    /// A valid, unexpired session and the given scripted responses.
    pub fn new(responses: &[(u16, String)]) -> Self {
        let harness = Self::without_session(responses);
        harness.seed(&session(NOW + 3600));
        harness
    }

    pub fn without_session(responses: &[(u16, String)]) -> Self {
        let mut fake = FakeStreamingTransport::new();
        for (status, body) in responses {
            fake = fake.respond(*status, &[], vec![FakeChunk::data(body.as_bytes())]);
        }
        Self {
            scratch: Scratch::new("flow"),
            env: MapEnv::new(),
            fake,
            resolver: FixedResolver::new().answer_any(&["52.94.236.248"]),
            random: SeededRandom::new(7),
            sleeper: RecordingSleeper::new(),
            policy: RetryPolicy::none(),
            archive_calls: Cell::new(0),
            archive_result: RefCell::new(Ok(b"PK-fake-archive".to_vec())),
        }
    }

    pub fn seed(&self, session: &StoredSession) {
        state::save_session(self.scratch.path(), session, &SeededRandom::new(1)).unwrap();
    }

    pub fn run(&self, request: &Request<'_>) -> Result<Json, String> {
        let archive = |_: &str, _: &str| {
            self.archive_calls.set(self.archive_calls.get() + 1);
            self.archive_result.borrow().clone()
        };
        run(
            request,
            &Deps {
                env: &self.env,
                state_dir: self.scratch.path(),
                home: None,
                transport: &self.fake,
                resolver: &self.resolver,
                random: &self.random,
                clock: &SystemClock,
                sleeper: &self.sleeper,
                jitter: &NoJitter,
                policy: self.policy,
                now: NOW,
                archive: &archive,
            },
        )
    }

    /// Exact (stdout, stderr, exit) as the binary writes them.
    pub fn golden(&self, request: &Request<'_>) -> (String, String, u8) {
        let (stdout, stderr, status, _) = self.golden_debug(request);
        (stdout, stderr, status)
    }

    /// [`Self::golden`] plus the `Debug` rendering of the command result.
    pub fn golden_debug(&self, request: &Request<'_>) -> (String, String, u8, String) {
        let result = self.run(request);
        let debug = format!("{result:?}");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = {
            let mut output = crate::Output::with_diagnostics(&mut out, &mut err);
            crate::launch::commands::report(&mut output, result).unwrap();
            output.exit_status()
        };
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            status,
            debug,
        )
    }

    pub fn urls(&self) -> Vec<String> {
        self.fake.requests().into_iter().map(|r| r.url).collect()
    }
}

pub(super) fn req<'a>(command: &'a str, first: Option<&'a str>) -> Request<'a> {
    Request {
        command,
        first,
        second: None,
        region: None,
        aws_mcp_url: None,
    }
}

fn error_line(message: &str) -> String {
    format!("{}\n", crate::launch::text::error_text(message))
}

fn failed(message: &str) -> (String, String, u8) {
    (String::new(), error_line(message), 1)
}

const HINT_401: &str =
    " Hint: the backend rejected the Bearer token. Ensure you signed in successfully.";
fn hint_404(region: &str) -> String {
    format!(" Hint: no launch with this ID exists in {region}. A launch exists only in the region it was created in; retry with --region set to that region (one of: us-east-1, eu-central-1).")
}

// ---- base-URL override and local validation before any token work ----

#[test]
fn t_api_override_url_is_validated_before_token_refresh() {
    // An expired access token with a valid client: the old order refreshed
    // first (one OIDC request), then refused the override.
    for url in [
        "http://launch.test.api.aws",
        "https://evil.example.com",
        "https://user:pw@launch.test.api.aws",
    ] {
        let mut harness = Harness::without_session(&[ok(200, r#"{"accessToken":"FAKE-ACCESS-NEW-0002","expiresIn":3600}"#)]);
        harness.seed(&session(NOW - 10));
        harness.env = MapEnv::new().with(config::ENV_BASE_URL, url);
        let expected = config::validate_base_url(url, &harness.resolver).unwrap_err().to_string();
        assert_eq!(harness.run(&req("get-launch", Some("l-1"))).unwrap_err(), expected);
        assert!(harness.fake.requests().is_empty(), "override {url} sent a request");
        assert_eq!(state::load_session(harness.scratch.path()).unwrap().token_expires_at, NOW - 10);
    }
    // A private address is refused after DNS, still before any request.
    let mut harness = Harness::new(&[]);
    harness.env = MapEnv::new().with(config::ENV_BASE_URL, OVERRIDE);
    harness.resolver = FixedResolver::new().answer_any(&["10.0.0.1"]);
    assert!(harness.run(&req("list-launches", None)).unwrap_err().contains("non-public"));
    assert!(harness.fake.requests().is_empty());
}

#[test]
fn t_api_local_checks_precede_configuration_and_token() {
    let harness = Harness::without_session(&[]);
    let long = "x".repeat(129);
    for (request, message) in [
        (req("get-launch", None), "Missing required argument for get-launch".to_owned()),
        (req("create-launch", None), "Missing required argument for create-launch".to_owned()),
        (req("delete-launch", Some("")), ApiError::InvalidLaunchId.to_string()),
        (
            Request { second: Some(&long), ..req("create-launch", Some("dir")) },
            ApiError::InvalidLaunchName.to_string(),
        ),
    ] {
        let mut bad_region = request;
        bad_region.region = Some("mars-1");
        assert_eq!(harness.run(&bad_region).unwrap_err(), message);
    }
    assert!(harness.fake.requests().is_empty());
    assert_eq!(harness.archive_calls.get(), 0);
}

#[test]
fn t_api_token_is_checked_before_the_archive_is_built() {
    let harness = Harness::without_session(&[]);
    assert_eq!(
        harness.golden(&req("create-launch", Some("/src/app"))),
        failed(crate::launch::oidc::SESSION_EXPIRED)
    );
    assert_eq!(harness.archive_calls.get(), 0);
    assert!(harness.fake.requests().is_empty());
    // An archive failure after a valid token: the archive text, no request,
    // no clientToken drawn.
    let harness = Harness::new(&[]);
    *harness.archive_result.borrow_mut() = Err("No files found in '/src/app'.".into());
    assert_eq!(
        harness.golden(&req("create-launch", Some("/src/app"))),
        failed("No files found in '/src/app'.")
    );
    assert_eq!(harness.archive_calls.get(), 1);
    assert!(harness.fake.requests().is_empty());
}

#[test]
fn t_api_client_tokens_are_uuid_v4_distinct_and_stable_across_retry() {
    let mut harness = Harness::new(&[
        ok(503, "{}"),
        ok(201, UPLOAD),
        ok(200, ""),
        ok(500, "{}"),
        ok(201, SMALL_LAUNCH),
    ]);
    harness.policy = RetryPolicy::default();
    let (stdout, stderr, status) = harness.golden(&req("create-launch", Some("/src/app")));
    assert_eq!((stdout.as_str(), stderr.as_str(), status), (SMALL_GOLDEN, "", 0));
    let requests = harness.fake.requests();
    assert_eq!(requests.len(), 5);
    let token = |i: usize| {
        let body = toolkit_core::parse_json(std::str::from_utf8(&requests[i].body).unwrap()).unwrap();
        body.get("clientToken").and_then(Json::as_str).unwrap().to_owned()
    };
    let (upload, create) = (token(0), token(3));
    assert_eq!(upload, token(1));
    assert_eq!(create, token(4));
    assert_ne!(upload, create);
    for t in [&upload, &create] {
        let parts: Vec<&str> = t.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(t.chars().all(|c| c == '-' || (c.is_ascii_hexdigit() && !c.is_ascii_uppercase())));
        assert!(parts[2].starts_with('4'));
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
    }
    // StartLaunchExecution draws its own token, reused across a 503.
    let mut harness = Harness::new(&[ok(503, "{}"), ok(202, SMALL_LAUNCH)]);
    harness.policy = RetryPolicy::default();
    assert_eq!(harness.golden(&req("start-launch-execution", Some("l-1"))).0, SMALL_GOLDEN);
    let requests = harness.fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    assert!(requests[0].url.ends_with("/api/launches/l-1/start"));
}

// ---- T-CMD goldens ----

#[test]
fn t_cmd_create_launch_success_saves_region_and_hides_upload_url() {
    let harness = Harness::new(&[ok(201, UPLOAD), ok(200, "<xml/>"), ok(201, SMALL_LAUNCH)]);
    let request = Request { second: Some("  my app  "), ..req("create-launch", Some("/src/app/")) };
    assert_eq!(harness.golden(&request), (SMALL_GOLDEN.to_owned(), String::new(), 0));
    let requests = harness.fake.requests();
    assert_eq!(
        harness.urls(),
        [
            format!("{US}/api/uploads"),
            "https://bucket.s3.us-east-1.amazonaws.com/k?X-Amz-Credential=FAKE-UPLOAD-CREDENTIAL&X-Amz-Signature=FAKE%2DUPLOAD%2DSIGNATURE".to_owned(),
            format!("{US}/api/launches"),
        ]
    );
    assert_eq!(requests[1].method, Method::Put);
    assert!(requests[1].headers.get("authorization").is_none());
    assert_eq!(requests[1].body, b"PK-fake-archive");
    let create = std::str::from_utf8(&requests[2].body).unwrap();
    assert!(create.starts_with(r#"{"name":"my app","source":{"s3Upload":{"uploadId":"u-1"}},"clientToken":""#));
    assert_eq!(state::load_saved_region(harness.scratch.path()), Some("us-east-1"));
    // The display name falls back to the basename, then to uploaded-app.
    assert_eq!(launch_name("/src/app/", None), "app");
    assert_eq!(launch_name("/", Some(" ")), "uploaded-app");
}

#[test]
fn t_cmd_create_launch_save_region_failure_is_best_effort() {
    let harness = Harness::new(&[ok(201, UPLOAD), ok(200, ""), ok(201, SMALL_LAUNCH)]);
    std::fs::create_dir(harness.scratch.join(state::CONFIG_FILE_NAME)).unwrap();
    assert_eq!(
        harness.golden(&Request { region: Some("us-east-1"), ..req("create-launch", Some("/src/app")) }),
        (SMALL_GOLDEN.to_owned(), String::new(), 0)
    );
}

#[test]
fn t_cmd_create_launch_repository_url_refusal_redacts_userinfo() {
    let harness = Harness::new(&[]);
    for source in [
        "https://user:FAKE-URL-SECRET-0001@github.com/o/r",
        "HTTP://user:FAKE-URL-SECRET-0001@github.com/o/r",
    ] {
        let (stdout, stderr, status) = harness.golden(&req("create-launch", Some(source)));
        assert_eq!((stdout.as_str(), status), ("", 1));
        assert!(stderr.starts_with("{\"error\": \"create-launch takes a local directory, not a repository URL. Clone "));
        assert!(stderr.ends_with(" locally and pass the clone's directory.\"}\n"));
        assert!(!stderr.contains("FAKE-URL-SECRET-0001"));
    }
    // Not a URL scheme: a local path that merely contains "http".
    assert!(validate_local(&req("create-launch", Some("./http:/x"))).is_ok());
    assert!(harness.fake.requests().is_empty());
}

#[test]
fn t_cmd_create_launch_put_and_api_failures() {
    let harness = Harness::new(&[ok(201, UPLOAD), ok(403, "<Error>FAKE-UPLOAD-SIGNATURE</Error>")]);
    assert_eq!(
        harness.golden(&req("create-launch", Some("/src/app"))),
        failed("PUT (presigned S3 upload) failed (403)")
    );
    let harness = Harness::new(&[ok(201, r#"{"uploadUrl":"http://bucket.s3.amazonaws.com/k?X-Amz-Signature=FAKE-UPLOAD-SIGNATURE","uploadId":"u"}"#)]);
    let (_, stderr, status) = harness.golden(&req("create-launch", Some("/src/app")));
    assert_eq!(status, 1);
    assert!(!stderr.contains("FAKE-UPLOAD-SIGNATURE") && !stderr.contains("bucket"));
    assert_eq!(harness.fake.requests().len(), 1);
    let harness = Harness::new(&[ok(400, r#"{"__type":"ValidationException"}"#)]);
    assert_eq!(
        harness.golden(&req("create-launch", Some("/src/app"))),
        failed("POST /api/uploads failed (400): ValidationException")
    );
    let harness = Harness::new(&[ok(201, r#"{"uploadId":"u"}"#)]);
    assert_eq!(harness.golden(&req("create-launch", Some("/src/app"))), failed(INVALID_RESPONSE));
}

#[test]
fn t_cmd_get_launch_golden_include_and_hints() {
    let harness = Harness::new(&[ok(200, LAUNCH)]);
    let request = Request { second: Some(" plan, download_url ,execution"), ..req("get-launch", Some("a/b c")) };
    assert_eq!(harness.golden(&request), (LAUNCH_GOLDEN.to_owned(), String::new(), 0));
    assert_eq!(
        harness.urls(),
        [format!("{US}/api/launches/a%2Fb%20c?include=plan&include=download_url&include=execution")]
    );
    let auth = harness.fake.requests()[0].headers.get("authorization").map(str::to_owned);
    assert_eq!(auth.as_deref(), Some(&*format!("Bearer {ACCESS}")));
    // An empty include sends no query (Python `if include:`).
    let harness = Harness::new(&[ok(200, LAUNCH)]);
    harness.golden(&Request { second: Some(""), ..req("get-launch", Some("l-1")) });
    assert_eq!(harness.urls(), [format!("{US}/api/launches/l-1")]);
    for (region, base) in [("us-east-1", US), ("eu-central-1", EU)] {
        let harness = Harness::new(&[ok(404, r#"{"__type":"ResourceNotFoundException"}"#)]);
        let request = Request { region: Some(region), ..req("get-launch", Some("l-1")) };
        assert_eq!(
            harness.golden(&request),
            failed(&format!("GET /api/launches/<id> failed (404): ResourceNotFoundException{}", hint_404(region)))
        );
        assert_eq!(harness.urls(), [format!("{base}/api/launches/l-1")]);
    }
    let mut harness = Harness::new(&[ok(404, r#"{"__type":"ResourceNotFoundException"}"#)]);
    harness.env = MapEnv::new().with(config::ENV_BASE_URL, OVERRIDE);
    assert_eq!(
        harness.golden(&req("get-launch", Some("l-1"))),
        failed("GET /api/launches/<id> failed (404): ResourceNotFoundException")
    );
    assert_eq!(harness.urls(), [format!("{OVERRIDE}/api/launches/l-1")]);
    for override_url in [None, Some(OVERRIDE)] {
        let mut harness = Harness::new(&[ok(401, "{}")]);
        if let Some(url) = override_url {
            harness.env = MapEnv::new().with(config::ENV_BASE_URL, url);
        }
        assert_eq!(
            harness.golden(&req("get-launch", Some("l-1"))),
            failed(&format!("GET /api/launches/<id> failed (401): UnrecognizedError{HINT_401}"))
        );
    }
}

#[test]
fn t_cmd_list_launches_both_regions_tokens_and_order() {
    let us = r#"{"nextToken":"next-us","items":[{"updatedAt":"2026-06-15T12:34:56Z","createdAt":"2026-06-15T12:34:56Z","status":"created","name":"a","id":"l-a","extra":1}]}"#;
    let eu = r#"{"items":[],"nextToken":""}"#;
    let harness = Harness::new(&[ok(200, us), ok(200, eu)]);
    let golden = r#"{
  "items": [
    {
      "id": "l-a",
      "name": "a",
      "status": "created",
      "createdAt": "2026-06-15 12:34:56+00:00",
      "updatedAt": "2026-06-15 12:34:56+00:00",
      "region": "us-east-1"
    }
  ],
  "nextTokens": {
    "us-east-1": "next-us"
  }
}
"#;
    assert_eq!(harness.golden(&req("list-launches", None)), (golden.to_owned(), String::new(), 0));
    assert_eq!(harness.urls(), [format!("{US}/api/launches"), format!("{EU}/api/launches")]);
    // --region queries only that region.
    let harness = Harness::new(&[ok(200, eu)]);
    assert_eq!(
        harness.golden(&Request { region: Some("eu-central-1"), ..req("list-launches", None) }),
        ("{\n  \"items\": []\n}\n".to_owned(), String::new(), 0)
    );
    assert_eq!(harness.urls(), [format!("{EU}/api/launches")]);
}

#[test]
fn t_cmd_list_launches_partial_and_total_failures() {
    // One region fails with a service error: Python `str(err)`, no hint.
    let harness = Harness::new(&[ok(401, "{}"), ok(200, r#"{"items":[]}"#)]);
    assert_eq!(
        harness.golden(&req("list-launches", None)).0,
        "{\n  \"items\": [],\n  \"errors\": {\n    \"us-east-1\": \"GET /api/launches failed (401): UnrecognizedError\"\n  }\n}\n"
    );
    // One region unreachable.
    let fake = FakeStreamingTransport::new()
        .respond(200, &[], vec![FakeChunk::data(br#"{"items":[]}"#)])
        .open_error(AwsError::connect("FAKE connect refused to https://launch-with-aws"));
    let harness = Harness { fake, ..Harness::new(&[]) };
    assert_eq!(
        harness.golden(&req("list-launches", None)).0,
        "{\n  \"items\": [],\n  \"errors\": {\n    \"eu-central-1\": \"Could not reach Launch with AWS in eu-central-1.\"\n  }\n}\n"
    );
    // Every region unreachable.
    let fake = FakeStreamingTransport::new()
        .open_error(AwsError::connect("refused"))
        .open_error(AwsError::connect("refused"));
    let harness = Harness { fake, ..Harness::new(&[]) };
    assert_eq!(
        harness.golden(&req("list-launches", None)),
        failed("Could not reach Launch with AWS in us-east-1, eu-central-1.")
    );
    // Every region failed, the last with a service error: that error and its hint.
    let fake = FakeStreamingTransport::new()
        .open_error(AwsError::connect("refused"))
        .respond(401, &[], vec![FakeChunk::data(b"{}")]);
    let harness = Harness { fake, ..Harness::new(&[]) };
    assert_eq!(
        harness.golden(&req("list-launches", None)),
        failed(&format!("GET /api/launches failed (401): UnrecognizedError{HINT_401}"))
    );
    // --region with a service error.
    let harness = Harness::new(&[ok(503, "{}")]);
    assert_eq!(
        harness.golden(&Request { region: Some("us-east-1"), ..req("list-launches", None) }),
        failed("GET /api/launches failed (503): UnrecognizedError")
    );
    // An invalid response ends the command (not a per-region error).
    let harness = Harness::new(&[ok(200, "[]"), ok(200, r#"{"items":[]}"#)]);
    assert_eq!(harness.golden(&req("list-launches", None)), failed(INVALID_RESPONSE));
    assert_eq!(harness.fake.requests().len(), 1);
    // The 404 hint never applies to list-launches.
    let harness = Harness::new(&[ok(200, r#"{"items":[]}"#), ok(404, "{}")]);
    let (stdout, _, _) = harness.golden(&req("list-launches", None));
    assert!(stdout.contains("\"eu-central-1\": \"GET /api/launches failed (404): UnrecognizedError\""));
}

#[test]
fn t_cmd_delete_launch_golden_and_redacted_id() {
    let harness = Harness::new(&[ok(204, "")]);
    assert_eq!(
        harness.golden(&req("delete-launch", Some("l-1"))),
        ("{\n  \"deleted\": true,\n  \"id\": \"l-1\",\n  \"region\": \"us-east-1\"\n}\n".to_owned(), String::new(), 0)
    );
    let requests = harness.fake.requests();
    assert_eq!(requests[0].method, Method::Delete);
    assert_eq!(harness.fake.frames_pulled(), 0);
    let harness = Harness::new(&[ok(404, "{}")]);
    assert_eq!(
        harness.golden(&req("delete-launch", Some("l-1"))),
        failed(&format!("DELETE /api/launches/<id> failed (404): UnrecognizedError{}", hint_404("us-east-1")))
    );
}

#[test]
fn t_cmd_start_and_status_goldens() {
    let harness = Harness::new(&[ok(202, SMALL_LAUNCH)]);
    assert_eq!(harness.golden(&req("start-launch-execution", Some("l-1"))), (SMALL_GOLDEN.to_owned(), String::new(), 0));
    assert_eq!(harness.fake.requests()[0].method, Method::Post);
    let running = r#"{"launch":{"id":"l-1","name":"n","status":"running","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z","execution":{"totalTasks":3,"completedTasks":1},"costEstimate":{"estimatedMonthlyCost":0.1,"region":"us-east-1","services":[]}}}"#;
    let harness = Harness::new(&[ok(200, running)]);
    let golden = r#"{
  "id": "l-1",
  "region": "us-east-1",
  "regionName": "US East (N. Virginia)",
  "status": "running",
  "isComplete": false,
  "isFailed": false,
  "completedTasks": 1,
  "totalTasks": 3,
  "currentPhase": null,
  "costEstimate": {
    "estimatedMonthlyCost": 0.1,
    "region": "us-east-1",
    "services": []
  }
}
"#;
    assert_eq!(harness.golden(&req("get-launch-status", Some("l-1"))), (golden.to_owned(), String::new(), 0));
    assert_eq!(harness.urls(), [format!("{US}/api/launches/l-1?include=execution&include=cost_estimate")]);
    let failed_launch = r#"{"launch":{"id":"l-1","name":"n","status":"failed","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z","failureReason":{"message":"Plan failed: quota","code":"QUOTA","phase":"plan"}}}"#;
    let harness = Harness::new(&[ok(200, failed_launch)]);
    let golden = "{\n  \"id\": \"l-1\",\n  \"region\": \"eu-central-1\",\n  \"regionName\": \"Europe (Frankfurt)\",\n  \"status\": \"failed\",\n  \"isComplete\": false,\n  \"isFailed\": true,\n  \"failureReason\": {\n    \"phase\": \"plan\",\n    \"code\": \"QUOTA\",\n    \"message\": \"Plan failed: quota\"\n  }\n}\n";
    assert_eq!(
        harness.golden(&Request { region: Some("eu-central-1"), ..req("get-launch-status", Some("l-1")) }),
        (golden.to_owned(), String::new(), 0)
    );
}

#[test]
fn t_cmd_download_url_printed_or_refused() {
    let harness = Harness::new(&[ok(200, LAUNCH)]);
    assert_eq!(
        harness.golden(&req("get-launch-download-url", Some("l-1"))),
        ("{\n  \"downloadUrl\": \"https://dl.example/a?X-Amz-Signature=abc\"\n}\n".to_owned(), String::new(), 0)
    );
    assert_eq!(harness.urls(), [format!("{US}/api/launches/l-1?include=download_url")]);
    for body in [SMALL_LAUNCH, &SMALL_LAUNCH.replace("\"status\"", "\"downloadUrl\":\"\",\"status\"")] {
        let harness = Harness::new(&[ok(200, body)]);
        assert_eq!(harness.golden(&req("get-launch-download-url", Some("l-1"))), failed(NO_DOWNLOAD_URL));
    }
    let harness = Harness::new(&[ok(404, "{}")]);
    assert_eq!(
        harness.golden(&req("get-launch-download-url", Some("l-1"))),
        failed(&format!("GET /api/launches/<id> failed (404): UnrecognizedError{}", hint_404("us-east-1")))
    );
}

#[test]
fn t_cmd_session_expired_and_unreachable_for_every_command() {
    for command in COMMANDS {
        let first = if command == "create-launch" { Some("/src/app") } else { Some("l-1") };
        let harness = Harness::without_session(&[]);
        assert_eq!(harness.golden(&req(command, first)), failed(crate::launch::oidc::SESSION_EXPIRED));
        assert!(harness.fake.requests().is_empty());
        if command == "list-launches" {
            continue;
        }
        let fake = FakeStreamingTransport::new().open_error(AwsError::connect("FAKE refused https://launch-with-aws.us-east-1.api.aws"));
        let harness = Harness { fake, ..Harness::new(&[]) };
        assert_eq!(harness.golden(&req(command, first)), failed("Could not reach Launch with AWS in us-east-1."));
    }
}

#[test]
fn t_cmd_invalid_responses_for_every_json_command() {
    for command in ["get-launch", "start-launch-execution", "get-launch-status", "get-launch-download-url", "list-launches"] {
        for body in ["", "not json", "[]", r#"{"launch":null}"#, r#"{"launch":{"id":"l"}}"#, r#"{"items":null}"#] {
            let harness = Harness::new(&[ok(200, body)]);
            let request = Request { region: Some("us-east-1"), ..req(command, Some("l-1")) };
            assert_eq!(harness.golden(&request), failed(INVALID_RESPONSE), "{command} {body}");
        }
    }
}

#[test]
fn t_cmd_refresh_happens_once_and_is_persisted() {
    let harness = Harness::without_session(&[
        ok(200, r#"{"accessToken":"FAKE-ACCESS-NEW-0002","expiresIn":3600}"#),
        ok(200, r#"{"items":[]}"#),
        ok(200, r#"{"items":[]}"#),
    ]);
    harness.seed(&session(NOW + 100));
    assert_eq!(harness.golden(&req("list-launches", None)).2, 0);
    let requests = harness.fake.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].url.ends_with("/token"));
    for request in &requests[1..] {
        assert_eq!(request.headers.get("authorization"), Some("Bearer FAKE-ACCESS-NEW-0002"));
    }
}
