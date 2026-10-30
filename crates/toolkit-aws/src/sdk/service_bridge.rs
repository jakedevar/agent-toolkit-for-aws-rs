//! Generated service SDKs over the toolkit's config, signer and transport.
//!
//! The SDK only serializes/deserializes. Its config must prefer `noAuth`,
//! disable retries/timeouts and install [`ServiceBridge::http_client`]. The
//! bridge signs each attempt with the guarded chain and uses the shared
//! retry policy. Provider/transport errors are kept typed, never formatted
//! through SDK source chains that may contain credentials.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpClient, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{
    HttpRequest as SdkRequest, HttpResponse as SdkResponse,
};
use aws_smithy_runtime_api::client::result::{ConnectorError, SdkError};
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::{body::SdkBody, error::metadata::ProvideErrorMetadata};

use super::{
    roots::ValidatedRoots,
    source::{GuardedCredentials, ProviderHttp},
    SdkRuntime, SdkTransport, SigV4Signer,
};
use crate::error::TimeoutPhase;
use crate::{
    AwsError, ClientConfig, ClientSettings, CredentialsProvider, EndpointPolicy, FullJitter,
    HttpRequest, HttpResponse, Method, ProcessEnv, ServiceError, Signer, Sleeper, StreamError,
    StreamLimits, StreamingTransport, SystemResolver, FRAMING_HEADERS,
};

/// One logical AWS operation's budget, including credentials and retries.
pub const OPERATION_BUDGET: Duration = Duration::from_secs(60);
/// A bounded, buffered catalog or diagnostic page (including error bodies).
const BODY_CAP: u64 = 16 * 1024 * 1024;

type Sender = dyn Fn(HttpRequest, Instant) -> Result<HttpResponse, AwsError> + Send + Sync;

struct State {
    sender: Box<Sender>,
    endpoint_host: String,
    deadline: Mutex<Instant>,
    error: Mutex<Option<AwsError>>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceBridge([REDACTED])")
    }
}

#[derive(Clone, Debug)]
struct Connector(Arc<State>);

impl HttpConnector for Connector {
    fn call(&self, request: SdkRequest) -> HttpConnectorFuture {
        let state = self.0.clone();
        HttpConnectorFuture::new(async move {
            // Toolkit adapters are synchronous. They run on the blocking pool
            // and a separate SDK runtime, never recursively inside this one.
            tokio::task::spawn_blocking(move || {
                let result = exchange(&state, &request);
                result.map_err(|error| {
                    *state
                        .error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                    ConnectorError::user(
                        std::io::Error::other("toolkit service request failed").into(),
                    )
                })
            })
            .await
            .map_err(|_| {
                ConnectorError::user(std::io::Error::other("toolkit service task failed").into())
            })?
        })
    }
}

fn exchange(state: &State, request: &SdkRequest) -> Result<SdkResponse, AwsError> {
    let deadline = *state
        .deadline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if Instant::now() >= deadline {
        return Err(deadline_error());
    }
    let uri: http::Uri = request
        .uri()
        .parse()
        .map_err(|_| AwsError::invalid_config("endpoint", "invalid service request URI"))?;
    if uri.host() != Some(state.endpoint_host.as_str()) {
        return Err(AwsError::invalid_config(
            "endpoint",
            "service request host differs from the resolved endpoint",
        ));
    }
    let body = request
        .body()
        .bytes()
        .ok_or_else(|| {
            AwsError::invalid_config("request", "streaming service requests are unsupported")
        })?
        .to_vec();
    if request.method() != "POST" {
        return Err(AwsError::invalid_config(
            "request",
            "only service POST operations are supported",
        ));
    }
    let mut headers = crate::Headers::new();
    for (name, value) in request.headers() {
        if !FRAMING_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            headers.insert(name, value)?;
        }
    }
    // These must never be supplied by the generated client's signer.
    if ["authorization", "x-amz-security-token", "x-amz-date"]
        .iter()
        .any(|name| headers.get(name).is_some())
    {
        return Err(AwsError::invalid_config(
            "sdk",
            "service SDK authentication must be disabled",
        ));
    }
    let response = (state.sender)(
        HttpRequest {
            method: Method::Post,
            url: request.uri().to_string(),
            headers,
            body,
            timeouts: crate::Timeouts::default(),
        },
        deadline,
    )?;
    let mut sdk = SdkResponse::new(
        StatusCode::try_from(response.status)
            .map_err(|_| AwsError::invalid_response("invalid HTTP status"))?,
        SdkBody::from(response.body),
    );
    for (name, value) in response.headers.entries() {
        sdk.headers_mut()
            .insert(name.to_string(), value.to_string());
    }
    Ok(sdk)
}

/// Shared SDK orchestration adapter. Operations run serially through `&mut self`.
pub struct ServiceBridge {
    runtime: SdkRuntime,
    config: ClientConfig,
    state: Arc<State>,
}

impl ServiceBridge {
    /// Production config and guarded SDK credential/HTTP foundations.
    ///
    /// # Errors
    /// Invalid region/profile/endpoint, trust roots or runtime configuration.
    pub fn new(service: &str, region: Option<&str>) -> Result<Self, AwsError> {
        let config = ClientConfig::resolve(
            service,
            &ClientSettings {
                region,
                ..ClientSettings::default()
            },
            &EndpointPolicy::aws_public(),
            &ProcessEnv,
            &SystemResolver,
        )?;
        let roots = ValidatedRoots::from_process().map_err(|_| {
            AwsError::invalid_config("tls_roots", "the TLS trust roots could not be loaded")
        })?;
        let transport_runtime = SdkRuntime::new()?;
        let transport = SdkTransport::for_public_service(&transport_runtime, &roots)?;
        let provider = ProviderHttp::validated_bounded(&roots)?;
        let credentials = GuardedCredentials::new(
            &transport_runtime,
            config.region().name(),
            config.profile(),
            provider,
        )
        .with_null_stdin(true);
        Self::with_foundations(
            config,
            move |deadline| credentials.until(deadline).load(),
            transport,
        )
    }

    // Both production and offline tests use this exact signing/buffering path.
    // Injection stays private: production callers can only construct `new`.
    fn with_foundations(
        config: ClientConfig,
        credentials: impl Fn(Instant) -> Result<crate::Credentials, AwsError> + Send + Sync + 'static,
        transport: impl StreamingTransport + Send + Sync + 'static,
    ) -> Result<Self, AwsError> {
        let scope = config.signing_scope();
        let timeouts = *config.timeouts();
        Self::from_sender(config, move |mut request, deadline| {
            request.timeouts = timeouts;
            SigV4Signer::new(AttemptCredentials(&credentials, deadline))
                .with_payload_checksum()
                .sign(&mut request, &scope)?;
            let limits = StreamLimits::new(BODY_CAP, 64 * 1024)?.with_deadline(deadline);
            let mut stream = transport.open(&request, &limits)?;
            let mut response = HttpResponse {
                status: stream.status(),
                headers: stream.headers().clone(),
                body: Vec::new(),
            };
            while let Some(chunk) = stream.next_chunk().map_err(stream_error)? {
                response.body.extend_from_slice(&chunk);
            }
            Ok(response)
        })
    }

    /// Inject an unsigned sender for offline wire tests only.
    ///
    /// # Errors
    /// An async runtime could not be created.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn with_sender(
        config: ClientConfig,
        sender: impl Fn(HttpRequest, Instant) -> Result<HttpResponse, AwsError> + Send + Sync + 'static,
    ) -> Result<Self, AwsError> {
        Self::from_sender(config, sender)
    }

    fn from_sender(
        config: ClientConfig,
        sender: impl Fn(HttpRequest, Instant) -> Result<HttpResponse, AwsError> + Send + Sync + 'static,
    ) -> Result<Self, AwsError> {
        let endpoint_host = config.endpoint().host().to_string();
        Ok(Self {
            runtime: SdkRuntime::new()?,
            config,
            state: Arc::new(State {
                endpoint_host,
                sender: Box::new(sender),
                deadline: Mutex::new(Instant::now()),
                error: Mutex::new(None),
            }),
        })
    }

    /// Resolved toolkit configuration; use its region and endpoint in the SDK.
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// The only HTTP client the generated service client may use.
    #[must_use]
    pub fn http_client(&self) -> SharedHttpClient {
        let connector = Connector(self.state.clone());
        http_client_fn(move |_, _| SharedHttpConnector::new(connector.clone()))
    }

    /// Run one logical operation with the toolkit retry owner and deadline.
    /// Never formats SDK errors or their sources.
    ///
    /// # Errors
    /// Shared typed credential/transport failures, redacted `ServiceError`,
    /// or a fixed invalid-request/response error for SDK codec failures.
    pub fn run<T, E, F, Fut>(&mut self, mut operation: F) -> Result<T, AwsError>
    where
        E: ProvideErrorMetadata,
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, SdkError<E, SdkResponse>>>,
    {
        let deadline = Instant::now() + OPERATION_BUDGET;
        *self
            .state
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = deadline;
        crate::run_with_retry(
            self.config.retry(),
            &Until(deadline),
            &FullJitter::new(),
            |_| {
                if Instant::now() >= deadline {
                    return Err(deadline_error());
                }
                *self
                    .state
                    .error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                self.runtime.block_on(operation()).map_err(|error| {
                    if let Some(error) = self
                        .state
                        .error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        return error;
                    }
                    match error {
                        SdkError::ServiceError(context) => AwsError::service(ServiceError::new(
                            "POST",
                            "/",
                            context.raw().status().as_u16(),
                            context.err().code(),
                            context.err().message().unwrap_or(""),
                            context.raw().headers().get("x-amzn-requestid"),
                        )),
                        SdkError::ConstructionFailure(_) => AwsError::invalid_config(
                            "request",
                            "the SDK could not construct the service request",
                        ),
                        SdkError::TimeoutError(_) => deadline_error(),
                        SdkError::ResponseError(_) => AwsError::invalid_response(
                            "the SDK could not decode the service response",
                        ),
                        _ => AwsError::connect("the SDK service request failed"),
                    }
                })
            },
        )
        .map(|result| result.value)
        .map_err(|failure| failure.error)
    }
}

struct AttemptCredentials<'a, F>(&'a F, Instant);
impl<F: Fn(Instant) -> Result<crate::Credentials, AwsError>> CredentialsProvider
    for AttemptCredentials<'_, F>
{
    fn name(&self) -> &'static str {
        "service-attempt"
    }

    fn load(&self) -> Result<crate::Credentials, AwsError> {
        (self.0)(self.1)
    }
}

struct Until(Instant);
impl Sleeper for Until {
    fn sleep(&self, delay: Duration) {
        crate::ThreadSleeper.sleep(delay.min(self.0.saturating_duration_since(Instant::now())));
    }
}

fn deadline_error() -> AwsError {
    AwsError::timeout(
        TimeoutPhase::Deadline,
        "the service operation deadline elapsed",
    )
}
fn stream_error(error: StreamError) -> AwsError {
    match error {
        StreamError::Failed(error) => error,
        StreamError::DeadlineExceeded => deadline_error(),
        StreamError::IdleTimeout => {
            AwsError::timeout(TimeoutPhase::Read, "the service response read timed out")
        }
        StreamError::TooLarge => {
            AwsError::invalid_response("the service response exceeded its size limit")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeChunk, FakeStreamingTransport, FixedResolver};
    use crate::{Headers, MapEnv, ResponseStream, RetryMode, RetryPolicy};
    use aws_smithy_types::error::ErrorMetadata;
    use sha2::{Digest, Sha256};

    struct Transport(Arc<FakeStreamingTransport>);
    impl StreamingTransport for Transport {
        fn open<'a>(
            &'a self,
            request: &HttpRequest,
            limits: &StreamLimits,
        ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
            self.0.open(request, limits)
        }
    }

    fn config(service: &str) -> ClientConfig {
        ClientConfig::resolve(
            service,
            &ClientSettings {
                region: Some("us-west-2"),
                retry: RetryPolicy::new(
                    2,
                    RetryMode::Standard,
                    Duration::from_millis(1),
                    Duration::from_millis(1),
                )
                .unwrap(),
                ..ClientSettings::default()
            },
            &EndpointPolicy::aws_public(),
            &MapEnv::new(),
            &FixedResolver::new(),
        )
        .unwrap()
    }

    fn request(bridge: &ServiceBridge) -> SdkRequest {
        let mut request = SdkRequest::new(SdkBody::from("{}"));
        request.set_method("POST").unwrap();
        request.set_uri(bridge.config().endpoint().url()).unwrap();
        request
    }

    // Exercise the real bridge retry/connector boundary with a minimal codec.
    fn run(bridge: &mut ServiceBridge, request: &SdkRequest) -> Result<(), AwsError> {
        let state = bridge.state.clone();
        bridge.run(|| async {
            let response = exchange(&state, request).map_err(|error| {
                *state.error.lock().unwrap() = Some(error);
                SdkError::<ErrorMetadata, SdkResponse>::construction_failure(std::io::Error::other(
                    "offline fixture",
                ))
            })?;
            if response.status().as_u16() == 503 {
                Err(SdkError::service_error(
                    ErrorMetadata::builder().code("ServiceUnavailable").build(),
                    response,
                ))
            } else {
                Ok(())
            }
        })
    }

    #[test]
    fn production_signing_path_signs_every_attempt_for_each_service() {
        for service in ["sagemaker", "monitoring", "logs"] {
            let transport = Arc::new(
                FakeStreamingTransport::new()
                    .respond(503, &[], vec![])
                    .respond(503, &[], vec![])
                    .respond(200, &[], vec![]),
            );
            let deadlines = Arc::new(Mutex::new(Vec::new()));
            let captured = deadlines.clone();
            let mut bridge = ServiceBridge::with_foundations(
                config(service),
                move |deadline| {
                    captured.lock().unwrap().push(deadline);
                    crate::Credentials::new(
                        "OFFLINEKEY",
                        "offline-secret",
                        Some("offline-token"),
                        None,
                    )
                },
                Transport(transport.clone()),
            )
            .unwrap();
            let request = request(&bridge);
            run(&mut bridge, &request).unwrap();
            let requests = transport.requests();
            assert_eq!(requests.len(), 3);
            let deadlines = deadlines.lock().unwrap();
            assert_eq!(deadlines.len(), 3, "load credentials on every attempt");
            for (index, request) in requests.iter().enumerate() {
                let date = request.headers.get("x-amz-date").expect("signed date");
                assert_eq!(date.len(), 16);
                let auth = request
                    .headers
                    .get("authorization")
                    .expect("signed authorization");
                assert!(auth.starts_with("AWS4-HMAC-SHA256 "));
                assert!(auth.contains(&format!(
                    "Credential=OFFLINEKEY/{}/us-west-2/{service}/aws4_request",
                    &date[..8]
                )));
                assert!(auth.contains("Signature="));
                assert_eq!(
                    request.headers.get("x-amz-security-token"),
                    Some("offline-token")
                );
                assert_eq!(
                    request.headers.get("x-amz-content-sha256"),
                    Some(format!("{:x}", Sha256::digest(b"{}")).as_str())
                );
                let limits = transport.open_limits()[index];
                assert_eq!(limits.max_body_bytes(), BODY_CAP);
                assert_eq!(limits.max_chunk_bytes(), 64 * 1024);
                assert_eq!(limits.deadline(), Some(deadlines[index]));
                assert_eq!(deadlines[index], deadlines[0], "one operation budget");
                assert_eq!(request.timeouts, *bridge.config().timeouts());
            }
        }
    }

    #[test]
    fn production_signing_path_refuses_missing_credentials_before_transport() {
        let transport = Arc::new(FakeStreamingTransport::new());
        let mut bridge = ServiceBridge::with_foundations(
            config("sagemaker"),
            |_| Err(AwsError::NoCredentials),
            Transport(transport.clone()),
        )
        .unwrap();
        let request = request(&bridge);
        assert_eq!(run(&mut bridge, &request), Err(AwsError::NoCredentials));
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn production_signing_path_caps_response_without_echoing_body() {
        let frame = FakeChunk::data(&vec![b'x'; 64 * 1024]);
        let mut frames = vec![frame; 256];
        frames.push(FakeChunk::data(b"oversized-body-secret"));
        let transport = Arc::new(FakeStreamingTransport::new().respond(200, &[], frames));
        let mut bridge = ServiceBridge::with_foundations(
            config("logs"),
            |_| crate::Credentials::new("OFFLINEKEY", "offline-secret", None, None),
            Transport(transport.clone()),
        )
        .unwrap();
        let request = request(&bridge);
        let error = run(&mut bridge, &request).unwrap_err();
        assert_eq!(
            error,
            AwsError::invalid_response("the service response exceeded its size limit")
        );
        assert!(!format!("{error:?} {error}").contains("oversized-body-secret"));
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn exchange_refuses_changed_uri_host_before_sender() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let mut bridge = ServiceBridge::with_sender(config("sagemaker"), move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse {
                status: 200,
                headers: Headers::new(),
                body: vec![],
            })
        })
        .unwrap();
        let mut request = request(&bridge);
        run(&mut bridge, &request).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for host in [
            "prefix.api.sagemaker.us-west-2.amazonaws.com",
            "logs.us-west-2.amazonaws.com",
            "unexpected.example",
        ] {
            request.set_uri(format!("https://{host}/")).unwrap();
            let error = run(&mut bridge, &request).unwrap_err();
            assert_eq!(
                error,
                AwsError::invalid_config(
                    "endpoint",
                    "service request host differs from the resolved endpoint"
                )
            );
            assert!(!error.to_string().contains(host));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "changed host must not reach sender"
            );
        }
    }

    #[test]
    fn exchange_refuses_each_sdk_auth_header_before_sender() {
        let mut bridge = ServiceBridge::with_sender(config("sagemaker"), |_, _| {
            panic!("unsigned sender must not run")
        })
        .unwrap();
        for header in ["authorization", "x-amz-security-token", "x-amz-date"] {
            let mut request = request(&bridge);
            request.headers_mut().insert(header, "offline-sensitive");
            assert_eq!(
                run(&mut bridge, &request),
                Err(AwsError::invalid_config(
                    "sdk",
                    "service SDK authentication must be disabled"
                ))
            );
        }
    }
}
