//! The request pipeline: address, sign, send, classify, retry.
//!
//! [`ApiClient`] ties a [`ClientConfig`] to injected [`Transport`], [`Signer`],
//! [`Sleeper`] and [`Jitter`]. For each attempt it signs a fresh copy of the
//! request (so every retry carries a fresh signature), sends it, and turns an
//! HTTP status of 400 or above into a redacted [`ServiceError`] whose code and
//! message are read from `x-amzn-ErrorType` or the JSON body. Retryable errors
//! are retried by [`run_with_retry`] with the configured policy.

use toolkit_core::{parse_json, Json};

use crate::config::ClientConfig;
use crate::error::{AwsError, ServiceError};
use crate::retry::{run_with_retry, Attempted, Jitter, RetryFailure, Sleeper};
use crate::transport::{
    build_url, ApiRequest, HttpRequest, HttpResponse, Signer, SigningScope, Transport,
};

/// JSON members that may hold the error code, in priority order.
const CODE_KEYS: &[&str] = &["__type", "code", "Code", "errorCode"];

/// JSON members that may hold the error message, in priority order.
const MESSAGE_KEYS: &[&str] = &["message", "Message", "errorMessage"];

/// A configured client over injected collaborators.
#[derive(Clone, Copy)]
pub struct ApiClient<'a> {
    config: &'a ClientConfig,
    transport: &'a dyn Transport,
    signer: &'a dyn Signer,
    sleeper: &'a dyn Sleeper,
    jitter: &'a dyn Jitter,
}

impl<'a> ApiClient<'a> {
    /// Assemble a client.
    #[must_use]
    pub fn new(
        config: &'a ClientConfig,
        transport: &'a dyn Transport,
        signer: &'a dyn Signer,
        sleeper: &'a dyn Sleeper,
        jitter: &'a dyn Jitter,
    ) -> Self {
        Self {
            config,
            transport,
            signer,
            sleeper,
            jitter,
        }
    }

    /// The configuration in use.
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        self.config
    }

    /// Send `request` with retries and return the (status < 400) response.
    ///
    /// # Errors
    /// A [`RetryFailure`] with the last error and attempt count. A request that
    /// cannot be addressed (bad path) fails with zero attempts before anything
    /// is signed or sent.
    pub fn execute(&self, request: &ApiRequest) -> Result<Attempted<HttpResponse>, RetryFailure> {
        let url = build_url(self.config.endpoint(), &request.path, &request.query)
            .map_err(|error| RetryFailure { error, attempts: 0 })?;
        let template = HttpRequest {
            method: request.method,
            url,
            headers: request.headers.clone(),
            body: request.body.clone(),
            timeouts: *self.config.timeouts(),
        };
        let scope = self.config.signing_scope();
        run_with_retry(self.config.retry(), self.sleeper, self.jitter, |_| {
            self.attempt(request, &template, &scope)
        })
    }

    /// Like [`ApiClient::execute`] but parses the body as JSON (an empty body
    /// is `null`).
    ///
    /// # Errors
    /// As [`ApiClient::execute`], plus `InvalidResponse` (not retried) when the
    /// body is not UTF-8 JSON.
    pub fn execute_json(&self, request: &ApiRequest) -> Result<Attempted<Json>, RetryFailure> {
        let Attempted { value, attempts } = self.execute(request)?;
        let json = parse_body(&value).map_err(|error| RetryFailure { error, attempts })?;
        Ok(Attempted {
            value: json,
            attempts,
        })
    }

    fn attempt(
        &self,
        request: &ApiRequest,
        template: &HttpRequest,
        scope: &SigningScope,
    ) -> Result<HttpResponse, AwsError> {
        let mut http = template.clone();
        self.signer.sign(&mut http, scope)?;
        let response = self.transport.send(&http)?;
        if response.status >= 400 {
            return Err(service_error(request, &response));
        }
        Ok(response)
    }
}

fn parse_body(response: &HttpResponse) -> Result<Json, AwsError> {
    if response.body.is_empty() {
        return Ok(Json::Null);
    }
    let text = response
        .body_text()
        .ok_or_else(|| AwsError::invalid_response("the response body is not valid UTF-8"))?;
    parse_json(text).map_err(|error| {
        AwsError::invalid_response(format!("the response body is not valid JSON: {error}"))
    })
}

/// Keep the part of an error code that names the error: drop a `:url` suffix
/// and a `namespace#` prefix.
fn clean_error_code(raw: &str) -> String {
    let before_colon = raw.split(':').next().unwrap_or(raw);
    let after_hash = before_colon.rsplit('#').next().unwrap_or(before_colon);
    after_hash.trim().to_string()
}

fn first_string<'j>(json: &'j Json, keys: &[&str]) -> Option<&'j str> {
    keys.iter()
        .find_map(|key| json.get(key).and_then(Json::as_str))
}

fn service_error(request: &ApiRequest, response: &HttpResponse) -> AwsError {
    let parsed = response.body_text().and_then(|text| parse_json(text).ok());
    let header_code = response
        .headers
        .get("x-amzn-errortype")
        .map(clean_error_code);
    let body_code = parsed
        .as_ref()
        .and_then(|json| first_string(json, CODE_KEYS))
        .map(clean_error_code);
    let code = header_code.or(body_code);
    // A JSON body without a message member is not dumped; only non-JSON text is.
    let message = match &parsed {
        Some(json) => first_string(json, MESSAGE_KEYS).unwrap_or("").to_string(),
        None => response.body_text().unwrap_or("").to_string(),
    };
    let request_id = response
        .headers
        .get("x-amzn-requestid")
        .or_else(|| response.headers.get("x-amz-request-id"));
    AwsError::service(ServiceError::new(
        request.method.as_str(),
        &request.path,
        response.status,
        code.as_deref(),
        &message,
        request_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::EndpointPolicy;
    use crate::env::MapEnv;
    use crate::paginate::{collect_pages, next_token_from, Page, PageLimits};
    use crate::retry::NoJitter;
    use crate::testing::{FakeSigner, FixedResolver, MockTransport, RecordingSleeper};
    use crate::transport::Method;
    use std::time::Duration;

    fn config() -> ClientConfig {
        ClientConfig::resolve(
            "logs",
            &crate::config::ClientSettings::default(),
            &EndpointPolicy::aws_public(),
            &MapEnv::new(),
            &FixedResolver::new(),
        )
        .unwrap()
    }

    fn secs(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|v| Duration::from_secs(*v)).collect()
    }

    struct Rig {
        config: ClientConfig,
        transport: MockTransport,
        signer: FakeSigner,
        sleeper: RecordingSleeper,
    }

    impl Rig {
        fn new(transport: MockTransport) -> Self {
            Self {
                config: config(),
                transport,
                signer: FakeSigner::new(),
                sleeper: RecordingSleeper::new(),
            }
        }

        fn client(&self) -> ApiClient<'_> {
            ApiClient::new(
                &self.config,
                &self.transport,
                &self.signer,
                &self.sleeper,
                &NoJitter,
            )
        }
    }

    fn get(path: &str) -> ApiRequest {
        ApiRequest::new(Method::Get, path)
    }

    #[test]
    fn a_successful_call_is_addressed_signed_and_sent_once() {
        let rig = Rig::new(MockTransport::new().respond(200, "{}"));
        let request = ApiRequest::new(Method::Post, "/api/launches")
            .with_query("include", "a b")
            .with_body(b"{}");
        let done = rig.client().execute(&request).unwrap();
        assert_eq!((done.attempts, done.value.status), (1, 200));
        let sent = rig.transport.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, Method::Post);
        assert_eq!(
            sent[0].url,
            "https://logs.us-east-1.amazonaws.com/api/launches?include=a%20b"
        );
        assert_eq!(sent[0].body, b"{}".to_vec());
        assert_eq!(sent[0].timeouts, *rig.config.timeouts());
        assert_eq!(
            sent[0].headers.get("authorization"),
            Some("FAKE-SIGNED us-east-1/logs")
        );
        assert_eq!(rig.signer.signed(), 1);
        assert!(rig.sleeper.sleeps().is_empty());
    }

    #[test]
    fn server_errors_are_retried_with_fresh_signatures_and_backoff() {
        let rig = Rig::new(MockTransport::new().respond(503, "").respond(200, "ok"));
        let done = rig.client().execute(&get("/x")).unwrap();
        assert_eq!(done.attempts, 2);
        assert_eq!(rig.transport.requests().len(), 2);
        assert_eq!(rig.signer.signed(), 2);
        assert_eq!(rig.sleeper.sleeps(), secs(&[2]));
    }

    #[test]
    fn connection_failures_and_timeouts_are_retried() {
        let transport = MockTransport::new()
            .fail(AwsError::connect("refused"))
            .fail(AwsError::timeout(crate::error::TimeoutPhase::Read, "slow"))
            .respond(200, "");
        let rig = Rig::new(transport);
        assert_eq!(rig.client().execute(&get("/x")).unwrap().attempts, 3);
        assert_eq!(rig.sleeper.sleeps(), secs(&[2, 4]));
    }

    #[test]
    fn throttling_exhausts_the_retries_with_the_last_error() {
        let body = r#"{"__type":"com.amazon#ThrottlingException","message":"Rate exceeded"}"#;
        let mut transport = MockTransport::new();
        for _ in 0..4 {
            transport = transport.respond(400, body);
        }
        let rig = Rig::new(transport);
        let failure = rig
            .client()
            .execute(&get("/api/things/abcdefghijklmnopqrstuv"))
            .unwrap_err();
        assert_eq!(failure.attempts, 4);
        assert!(failure.error.is_throttling());
        assert_eq!(
            failure.error.to_string(),
            "GET /api/things/<id> failed (400): ThrottlingException"
        );
        assert_eq!(rig.sleeper.sleeps(), secs(&[2, 4, 8]));
        assert_eq!(rig.transport.remaining(), 0);
    }

    #[test]
    fn client_errors_are_not_retried_and_carry_code_message_and_request_id() {
        let transport = MockTransport::new().respond_with_header(
            403,
            r#"{"message":"User is not authorized","extra":1}"#,
            "x-amzn-RequestId",
            "req-42",
        );
        let rig = Rig::new(transport);
        let failure = rig.client().execute(&get("/x")).unwrap_err();
        assert_eq!(failure.attempts, 1);
        assert!(rig.sleeper.sleeps().is_empty());
        let AwsError::Service(service) = &failure.error else {
            panic!("expected a service error, got {:?}", failure.error);
        };
        assert_eq!(service.status(), 403);
        assert_eq!(service.code(), None);
        assert_eq!(service.message(), "User is not authorized");
        assert_eq!(service.request_id(), Some("req-42"));
    }

    #[test]
    fn the_error_type_header_wins_and_is_cleaned() {
        let transport = MockTransport::new().respond_with_header(
            400,
            r#"{"__type":"Other","Message":"bad"}"#,
            "x-amzn-ErrorType",
            "ValidationException:http://internal.amazon.com/coral/",
        );
        let failure = Rig::new(transport)
            .client()
            .execute(&get("/x"))
            .unwrap_err();
        let AwsError::Service(service) = &failure.error else {
            panic!("expected a service error");
        };
        assert_eq!(service.code(), Some("ValidationException"));
        assert_eq!(service.message(), "bad");
    }

    #[test]
    fn non_json_error_bodies_become_redacted_truncated_messages() {
        let transport = MockTransport::new().respond(404, "<html>token=abc123 not found</html>");
        let failure = Rig::new(transport)
            .client()
            .execute(&get("/x"))
            .unwrap_err();
        let AwsError::Service(service) = &failure.error else {
            panic!("expected a service error");
        };
        assert_eq!(service.message(), "<html>token=[REDACTED] not found</html>");
        assert!(!failure.error.to_string().contains("abc123"));
    }

    #[test]
    fn json_error_bodies_without_a_message_are_not_dumped() {
        let transport =
            MockTransport::new().respond(400, r#"{"code":"Bad","detail":"password: hunter2"}"#);
        let failure = Rig::new(transport)
            .client()
            .execute(&get("/x"))
            .unwrap_err();
        let AwsError::Service(service) = &failure.error else {
            panic!("expected a service error");
        };
        assert_eq!(service.code(), Some("Bad"));
        assert_eq!(service.message(), "");
    }

    #[test]
    fn secrets_in_service_messages_never_reach_the_rendered_error() {
        let body = r#"{"message":"bad key AKIAIOSFODNN7EXAMPLE and secret_access_key=abcd1234"}"#;
        let failure = Rig::new(MockTransport::new().respond(400, body))
            .client()
            .execute(&get("/x"))
            .unwrap_err();
        let rendered = crate::error::render_failure("do thing", &failure.error, &[]);
        assert!(rendered.contains("bad key [REDACTED] and secret_access_key=[REDACTED]"));
        assert!(!rendered.contains("AKIA") && !rendered.contains("abcd1234"));
    }

    #[test]
    fn signer_failures_stop_before_anything_is_sent() {
        let mut rig = Rig::new(MockTransport::new().respond(200, ""));
        rig.signer = FakeSigner::failing(AwsError::NoCredentials);
        let failure = rig.client().execute(&get("/x")).unwrap_err();
        assert_eq!(
            (failure.error, failure.attempts),
            (AwsError::NoCredentials, 1)
        );
        assert!(rig.transport.requests().is_empty());
        assert_eq!(rig.transport.remaining(), 1);
    }

    #[test]
    fn unaddressable_requests_fail_with_zero_attempts() {
        let rig = Rig::new(MockTransport::new().respond(200, ""));
        for path in ["x", "/a/../b", "/a b", "/a?x=1"] {
            let failure = rig.client().execute(&get(path)).unwrap_err();
            assert_eq!(failure.attempts, 0, "{path}");
            assert_eq!(failure.error.code(), "aws_invalid_config");
        }
        assert!(rig.transport.requests().is_empty());
        assert_eq!(rig.signer.signed(), 0);
    }

    #[test]
    fn redirect_statuses_below_400_are_returned_not_retried() {
        let rig = Rig::new(MockTransport::new().respond(304, ""));
        assert_eq!(rig.client().execute(&get("/x")).unwrap().value.status, 304);
    }

    #[test]
    fn execute_json_parses_bodies_and_treats_empty_as_null() {
        let rig = Rig::new(
            MockTransport::new()
                .respond(200, r#"{"a":[1,2]}"#)
                .respond(204, ""),
        );
        let first = rig.client().execute_json(&get("/x")).unwrap();
        assert_eq!(
            first.value.get("a"),
            Some(&Json::Array(vec![Json::Int(1), Json::Int(2)]))
        );
        assert_eq!(
            rig.client().execute_json(&get("/x")).unwrap().value,
            Json::Null
        );
    }

    #[test]
    fn invalid_json_success_bodies_are_not_retried() {
        let rig = Rig::new(
            MockTransport::new()
                .respond(200, "{oops")
                .respond(200, "{}"),
        );
        let failure = rig.client().execute_json(&get("/x")).unwrap_err();
        assert_eq!(failure.error.code(), "aws_invalid_response");
        assert_eq!(failure.attempts, 1);
        assert_eq!(rig.transport.remaining(), 1);
    }

    #[test]
    fn pagination_composes_with_retries_and_passes_tokens_in_the_query() {
        let transport = MockTransport::new()
            .respond(200, r#"{"items":[1,2],"nextToken":"p2"}"#)
            .respond(429, r#"{"message":"slow down"}"#)
            .respond(200, r#"{"items":[3],"nextToken":""}"#);
        let rig = Rig::new(transport);
        let client = rig.client();
        let items = collect_pages(&PageLimits::default(), |token| {
            let mut request = get("/items");
            if let Some(token) = token {
                request = request.with_query("nextToken", token);
            }
            let json = client.execute_json(&request).map_err(AwsError::from)?.value;
            let items: Vec<i64> = match json.get("items") {
                Some(Json::Array(values)) => values
                    .iter()
                    .filter_map(|v| if let Json::Int(n) = v { Some(*n) } else { None })
                    .collect(),
                _ => Vec::new(),
            };
            Ok(Page {
                items,
                next_token: next_token_from(&json, "nextToken")?,
            })
        })
        .unwrap();
        assert_eq!(items, vec![1, 2, 3]);
        let urls: Vec<String> = rig
            .transport
            .requests()
            .into_iter()
            .map(|r| r.url)
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://logs.us-east-1.amazonaws.com/items".to_string(),
                "https://logs.us-east-1.amazonaws.com/items?nextToken=p2".to_string(),
                "https://logs.us-east-1.amazonaws.com/items?nextToken=p2".to_string(),
            ]
        );
        assert_eq!(rig.sleeper.sleeps(), secs(&[2]));
    }

    #[test]
    fn debug_of_the_request_pipeline_never_shows_the_signature() {
        let rig = Rig::new(MockTransport::new().respond(200, ""));
        rig.client().execute(&get("/x")).unwrap();
        let shown = format!("{:?}", rig.transport.requests());
        assert!(shown.contains("\"authorization\": \"[REDACTED]\""));
        assert!(!shown.contains("FAKE-SIGNED"));
    }
}
