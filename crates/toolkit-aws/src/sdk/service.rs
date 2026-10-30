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
    AwsError, ClientConfig, ClientSettings, EndpointPolicy, FullJitter, HttpRequest, HttpResponse,
    Method, ProcessEnv, ServiceError, Signer, Sleeper, StreamError, StreamLimits,
    StreamingTransport, SystemResolver, FRAMING_HEADERS,
};

/// One logical AWS operation's budget, including credentials and retries.
pub const OPERATION_BUDGET: Duration = Duration::from_secs(60);
/// A bounded, buffered catalog or diagnostic page (including error bodies).
const BODY_CAP: u64 = 16 * 1024 * 1024;

type Sender = dyn Fn(HttpRequest, Instant) -> Result<HttpResponse, AwsError> + Send + Sync;

struct State {
    sender: Box<Sender>,
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
        let transport = SdkTransport::new(&transport_runtime, &roots)?;
        let provider = ProviderHttp::validated_bounded(&roots)?;
        let credentials = GuardedCredentials::new(
            &transport_runtime,
            config.region().name(),
            config.profile(),
            provider,
        )
        .with_null_stdin(true);
        let scope = config.signing_scope();
        let timeouts = *config.timeouts();
        Self::with_sender(config, move |mut request, deadline| {
            request.timeouts = timeouts;
            SigV4Signer::new(credentials.until(deadline)).sign(&mut request, &scope)?;
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

    /// Inject the transport for deterministic, credential-free wire tests.
    ///
    /// # Errors
    /// An async runtime could not be created.
    pub fn with_sender(
        config: ClientConfig,
        sender: impl Fn(HttpRequest, Instant) -> Result<HttpResponse, AwsError> + Send + Sync + 'static,
    ) -> Result<Self, AwsError> {
        Ok(Self {
            runtime: SdkRuntime::new()?,
            config,
            state: Arc::new(State {
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
