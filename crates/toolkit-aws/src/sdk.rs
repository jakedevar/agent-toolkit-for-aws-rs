//! AWS SDK for Rust binding of the boundary seams (cargo feature `sdk`, #4463).
//!
//! Compiled only with `--features sdk`; the default workspace build has no AWS
//! SDK crate in it. Three adapters implement the existing traits:
//!
//! - [`SdkCredentialsProvider`] implements [`CredentialsProvider`] over the
//!   SDK's default credential provider chain (environment, shared config and
//!   credentials files including SSO, web identity and assume-role, ECS/EKS
//!   container role, EC2 instance metadata). `AWS_PROFILE`, already validated by
//!   [`ClientConfig`], selects the profile for **credentials only**; the chain
//!   is told the region the boundary resolved, so a profile's configured region
//!   never selects the target region.
//! - [`SigV4Signer`] implements [`Signer`] with `aws-sigv4`, taking credentials
//!   from any [`CredentialsProvider`] on every attempt.
//! - [`SdkTransport`] implements [`Transport`] with the smithy hyper client
//!   (rustls with the aws-lc provider): connect and read timeouts from
//!   [`Timeouts`] (the read timeout also bounds reading the body), no
//!   redirects (hyper never follows them), no proxy, https only, and a
//!   connect-time DNS guard that applies the public-address policy to the very
//!   addresses the socket connects to ([`crate::binding::guarded_addresses`]).
//!   The hyper connector never asks the resolver about a literal IP host, so
//!   the same policy is applied to a literal host, in every numeric form a
//!   resolver accepts, before the connector is called
//!   ([`crate::binding::require_public_host`]), and again to the peer address
//!   of the socket the response came over (#4593).
//!
//! Retries stay with [`crate::ApiClient`] and its [`crate::RetryPolicy`]; the
//! transport sends exactly one request per call.
//!
//! Redaction: no SDK value is ever formatted with `Debug`. Credential and
//! signing failures keep only their kind (and the provider name): a provider's
//! or the signer's own text is never read, because a `credential_process`
//! stderr or a custom provider message can hold an unlabelled secret that
//! pattern redaction cannot recognise (#4594). Connector and body-read
//! failures are rendered with their `Display` chain and pass through the
//! redacting [`AwsError`] constructors; the adapters print fixed, value-free
//! `Debug`.
//!
//! Runtime: the boundary is synchronous, so each adapter drives SDK futures on
//! an [`SdkRuntime`] (a current-thread tokio runtime). Do not call the adapters
//! from inside another tokio runtime; `block_on` would panic there.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_credential_types::Credentials as SdkCredentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use aws_smithy_async::rt::sleep::TokioSleep;
use aws_smithy_http_client::tls::TlsContext;
use aws_smithy_http_client::{tls, Connector};
use aws_smithy_runtime_api::client::connection::CaptureSmithyConnection;
use aws_smithy_runtime_api::client::dns::{DnsFuture, ResolveDns, ResolveDnsError};
use aws_smithy_runtime_api::client::http::{
    HttpConnector, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::identity::Identity;
use aws_smithy_runtime_api::client::orchestrator::HttpRequest as SdkHttpRequest;
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::byte_stream::ByteStream;
use aws_smithy_types::error::display::DisplayErrorContext;

use crate::binding::{
    adopt_credentials, connector_error, credential_error, find_guard_rejection, guarded_addresses,
    require_https, require_public_host, require_public_peer, response_headers, ConnectorFailure,
    CredentialFailure, SDK_DEFAULT_CHAIN,
};
use crate::config::ClientConfig;
use crate::credentials::CredentialsProvider;
use crate::endpoint::{HostResolver, SystemResolver};
use crate::error::{AwsError, TimeoutPhase};
use crate::stream::{ChunkLimiter, ResponseStream, StreamError, StreamLimits, StreamingTransport};
use crate::transport::{
    refuse_framing_headers, Headers, HttpRequest, HttpResponse, Signer, SigningScope, Timeouts,
    Transport,
};
use roots::ValidatedRoots;

pub mod profile;
pub mod recorder;
pub mod roots;
#[cfg(test)]
mod s3_tests;
pub mod source;
pub mod stdin;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tls_tests;

/// Provider name the signer gives the SDK credentials it builds.
const SIGNER_PROVIDER_NAME: &str = "toolkit-aws";

/// The tokio runtime the adapters drive SDK futures on. Cheap to clone.
#[derive(Clone)]
pub struct SdkRuntime(Arc<tokio::runtime::Runtime>);

impl SdkRuntime {
    /// A current-thread runtime with the I/O and time drivers enabled.
    ///
    /// # Errors
    /// `InvalidConfig` (field `runtime`) when the runtime cannot be created.
    pub fn new() -> Result<Self, AwsError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|runtime| Self(Arc::new(runtime)))
            .map_err(|error| {
                AwsError::invalid_config(
                    "runtime",
                    format!("could not start the async runtime: {error}"),
                )
            })
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.0.block_on(future)
    }
}

/// Whether any `tracing` dispatcher was ever installed in this process
/// (T-LEAK-GATE, R-LOG rule 1: the proxy must leave this `false`).
#[must_use]
pub fn tracing_dispatcher_set() -> bool {
    tracing::dispatcher::has_been_set()
}

impl fmt::Debug for SdkRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SdkRuntime")
    }
}

/// Classify an SDK credential failure by its variant only. Neither the
/// error's `Display` nor its `Debug` nor its source chain is read: a
/// `credential_process` failure carries the process's stderr and a custom
/// provider's message is arbitrary, so either can hold an unlabelled secret
/// that redaction cannot recognise (#4594).
fn map_credentials_error(error: &CredentialsError) -> AwsError {
    let kind = match error {
        CredentialsError::CredentialsNotLoaded(_) => CredentialFailure::NotLoaded,
        CredentialsError::ProviderTimedOut(_) => CredentialFailure::TimedOut,
        CredentialsError::InvalidConfiguration(_) => CredentialFailure::InvalidConfiguration,
        CredentialsError::ProviderError(_) => CredentialFailure::Provider,
        _ => CredentialFailure::Unhandled,
    };
    credential_error(SDK_DEFAULT_CHAIN, kind)
}

/// [`CredentialsProvider`] over an SDK credentials provider, by default the
/// SDK's default chain.
pub struct SdkCredentialsProvider {
    runtime: SdkRuntime,
    provider: SharedCredentialsProvider,
    clock: fn() -> SystemTime,
}

impl SdkCredentialsProvider {
    /// The SDK default credential chain for `config`: the profile from
    /// `AWS_PROFILE` (validated by [`ClientConfig`]) for credentials only, and
    /// the boundary's resolved region for any STS or SSO call the chain makes.
    #[must_use]
    pub fn default_chain(runtime: &SdkRuntime, config: &ClientConfig) -> Self {
        Self::default_chain_with_http(runtime, config, &source::ProviderHttp::SdkDefault)
    }

    /// Wrap any SDK credentials provider (a custom source, or a test double).
    #[must_use]
    pub fn from_sdk(runtime: &SdkRuntime, provider: impl ProvideCredentials + 'static) -> Self {
        Self {
            runtime: runtime.clone(),
            provider: SharedCredentialsProvider::new(provider),
            clock: SystemTime::now,
        }
    }

    /// Use `clock` to decide expiry (deterministic tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> SystemTime) -> Self {
        self.clock = clock;
        self
    }
}

impl fmt::Debug for SdkCredentialsProvider {
    /// A fixed marker: never the SDK provider's own `Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SdkCredentialsProvider([REDACTED])")
    }
}

impl CredentialsProvider for SdkCredentialsProvider {
    fn name(&self) -> &'static str {
        SDK_DEFAULT_CHAIN
    }

    fn load(&self) -> Result<crate::credentials::Credentials, AwsError> {
        let provider = &self.provider;
        let loaded = self.runtime.block_on(provider.provide_credentials());
        match loaded {
            Ok(credentials) => adopt_credentials(
                credentials.access_key_id(),
                credentials.secret_access_key(),
                credentials.session_token(),
                credentials.expiry(),
                (self.clock)(),
            ),
            Err(error) => Err(map_credentials_error(&error)),
        }
    }
}

/// AWS Signature Version 4 [`Signer`] (headers, signed payload hash,
/// `x-amz-security-token` for temporary credentials).
pub struct SigV4Signer<P> {
    credentials: P,
    clock: fn() -> SystemTime,
}

impl<P: CredentialsProvider> SigV4Signer<P> {
    /// Sign with credentials from `credentials`, loaded on every attempt.
    #[must_use]
    pub fn new(credentials: P) -> Self {
        Self {
            credentials,
            clock: SystemTime::now,
        }
    }

    /// Use `clock` for the signature time (deterministic tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> SystemTime) -> Self {
        self.clock = clock;
        self
    }
}

impl<P> fmt::Debug for SigV4Signer<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigV4Signer([REDACTED])")
    }
}

/// A signing failure, by stage only: the signer's error text is built from the
/// request and the credentials being signed with, so it is never kept.
fn signing_error(stage: &'static str) -> AwsError {
    AwsError::invalid_config(
        "signing",
        format!("the request could not be signed ({stage})"),
    )
}

impl<P: CredentialsProvider> Signer for SigV4Signer<P> {
    fn sign(&self, request: &mut HttpRequest, scope: &SigningScope) -> Result<(), AwsError> {
        refuse_framing_headers(&request.headers)?;
        let now = (self.clock)();
        let credentials = self.credentials.load()?;
        if credentials.is_expired_at(now) {
            return Err(AwsError::CredentialsExpired);
        }
        let identity: Identity = SdkCredentials::new(
            credentials.access_key_id().expose(),
            credentials.secret_access_key().expose(),
            credentials
                .session_token()
                .map(|token| token.expose().to_string()),
            credentials.expires_at(),
            SIGNER_PROVIDER_NAME,
        )
        .into();
        let headers = {
            let params: aws_sigv4::http_request::SigningParams<'_> = v4::SigningParams::builder()
                .identity(&identity)
                .region(&scope.region)
                .name(&scope.service)
                .time(now)
                .settings(SigningSettings::default())
                .build()
                .map_err(|_| signing_error("signing parameters"))?
                .into();
            let signable = SignableRequest::new(
                request.method.as_str(),
                request.url.as_str(),
                request.headers.entries(),
                SignableBody::Bytes(&request.body),
            )
            .map_err(|_| signing_error("request"))?;
            let (instructions, _signature) = sign(signable, &params)
                .map_err(|_| signing_error("signature"))?
                .into_parts();
            instructions.into_parts().0
        };
        for header in headers {
            request.headers.insert(header.name(), header.value())?;
        }
        Ok(())
    }
}

/// The connect-time DNS guard as an SDK resolver.
#[derive(Clone)]
struct GuardedResolver {
    inner: Arc<dyn HostResolver + Send + Sync>,
}

impl fmt::Debug for GuardedResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuardedResolver")
    }
}

impl ResolveDns for GuardedResolver {
    fn resolve_dns<'a>(&'a self, name: &'a str) -> DnsFuture<'a> {
        DnsFuture::ready(guarded_addresses(name, self.inner.as_ref()).map_err(ResolveDnsError::new))
    }
}

/// A resolver **without** the public-address guard, for the T-TLS loopback
/// harness only (test builds).
#[cfg(test)]
#[derive(Clone)]
struct UnguardedResolver {
    inner: Arc<dyn HostResolver + Send + Sync>,
}

#[cfg(test)]
impl fmt::Debug for UnguardedResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnguardedResolver")
    }
}

#[cfg(test)]
impl ResolveDns for UnguardedResolver {
    fn resolve_dns<'a>(&'a self, name: &'a str) -> DnsFuture<'a> {
        DnsFuture::ready(
            self.inner
                .resolve(name)
                .map_err(|reason| ResolveDnsError::new(std::io::Error::other(reason))),
        )
    }
}

type ConnectorFactory = dyn Fn(&Timeouts) -> SharedHttpConnector + Send + Sync;

/// [`Transport`] and [`StreamingTransport`] over the smithy hyper HTTP client.
pub struct SdkTransport {
    runtime: SdkRuntime,
    factory: Box<ConnectorFactory>,
    connectors: Mutex<Vec<(Timeouts, SharedHttpConnector)>>,
    /// The trust roots the connector was built with (`None` for an injected
    /// connector).
    roots: Option<ValidatedRoots>,
    /// Refuse responses from a non-public socket peer (off only in the
    /// loopback test harness).
    peer_check: bool,
}

/// One rustls (aws-lc) connector over `resolver` that trusts exactly `ctx`:
/// the explicit R-ROOTS context, never Smithy's native roots.
fn build_connector<R: ResolveDns + Clone + 'static>(
    runtime: &SdkRuntime,
    timeouts: &Timeouts,
    ctx: &TlsContext,
    resolver: R,
) -> SharedHttpConnector {
    // The hyper client registers timers and its executor with the current
    // runtime, so build it inside the adapter's runtime.
    let _entered = runtime.0.enter();
    let settings = HttpConnectorSettings::builder()
        .connect_timeout(timeouts.connect())
        .read_timeout(timeouts.read())
        .build();
    let connector = Connector::builder()
        .connector_settings(settings)
        .sleep_impl(TokioSleep::new())
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::AwsLc,
        ))
        .tls_context(ctx.clone())
        .build_with_resolver(resolver);
    SharedHttpConnector::new(connector)
}

impl SdkTransport {
    /// The production transport: system DNS behind the public-address guard,
    /// trusting exactly `roots`.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn new(runtime: &SdkRuntime, roots: &ValidatedRoots) -> Result<Self, AwsError> {
        Self::with_resolver(runtime, roots, Arc::new(SystemResolver))
    }

    /// The production transport with an injected resolver (still guarded).
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn with_resolver(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        resolver: Arc<dyn HostResolver + Send + Sync>,
    ) -> Result<Self, AwsError> {
        let guard = GuardedResolver { inner: resolver };
        let ctx = roots.tls_context()?;
        let factory_runtime = runtime.clone();
        let factory = move |timeouts: &Timeouts| {
            build_connector(&factory_runtime, timeouts, &ctx, guard.clone())
        };
        Ok(Self {
            runtime: runtime.clone(),
            factory: Box::new(factory),
            connectors: Mutex::new(Vec::new()),
            roots: Some(roots.clone()),
            peer_check: true,
        })
    }

    /// The T-TLS harness transport: the same connector builder, TLS provider
    /// and `ValidatedRoots` as production, with only the DNS resolver
    /// replaced (unguarded, so a synthetic name can reach a loopback server)
    /// and the peer check off. Test builds only.
    #[cfg(test)]
    pub(crate) fn loopback_for_tests(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        resolver: Arc<dyn HostResolver + Send + Sync>,
    ) -> Result<Self, AwsError> {
        let mut transport = Self::with_resolver(runtime, roots, Arc::clone(&resolver))?;
        let unguarded = UnguardedResolver { inner: resolver };
        let ctx = roots.tls_context()?;
        let factory_runtime = runtime.clone();
        transport.factory = Box::new(move |timeouts: &Timeouts| {
            build_connector(&factory_runtime, timeouts, &ctx, unguarded.clone())
        });
        transport.peer_check = false;
        Ok(transport)
    }

    /// A transport over a given SDK HTTP connector, whatever the timeouts
    /// (tests and custom stacks; the DNS guard and trust roots are the
    /// connector's business).
    #[must_use]
    pub fn with_connector(runtime: &SdkRuntime, connector: impl HttpConnector + 'static) -> Self {
        let shared = SharedHttpConnector::new(connector);
        Self {
            runtime: runtime.clone(),
            factory: Box::new(move |_: &Timeouts| shared.clone()),
            connectors: Mutex::new(Vec::new()),
            roots: None,
            peer_check: true,
        }
    }

    /// The trust roots this transport's connectors use (`None` for an
    /// injected connector).
    #[must_use]
    pub fn roots(&self) -> Option<&ValidatedRoots> {
        self.roots.as_ref()
    }

    fn connector_for(&self, timeouts: &Timeouts) -> SharedHttpConnector {
        let mut cache = self
            .connectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((_, connector)) = cache.iter().find(|(key, _)| key == timeouts) {
            return connector.clone();
        }
        let connector = (self.factory)(timeouts);
        cache.push((*timeouts, connector.clone()));
        connector
    }

    /// The guards every request passes before a connector is called: https,
    /// a public literal host, and no authority or framing header (R-HDR).
    fn admit(request: &HttpRequest) -> Result<SdkHttpRequest, AwsError> {
        require_https(&request.url)?;
        require_public_host(&request.url)?;
        refuse_framing_headers(&request.headers)?;
        to_sdk_request(request)
    }

    /// Send and wait for the response head (bounded by the connector's read
    /// timeout); the peer check runs before anything is returned.
    async fn call(
        &self,
        connector: SharedHttpConnector,
        mut request: SdkHttpRequest,
    ) -> Result<aws_smithy_runtime_api::client::orchestrator::HttpResponse, AwsError> {
        let capture = CaptureSmithyConnection::new();
        request.add_extension(capture.clone());
        let response = connector
            .call(request)
            .await
            .map_err(|error| map_connector_error(&error))?;
        if self.peer_check {
            check_peer(&capture)?;
        }
        Ok(response)
    }
}

impl fmt::Debug for SdkTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SdkTransport")
    }
}

/// Build the SDK request without formatting any part of it.
fn to_sdk_request(request: &HttpRequest) -> Result<SdkHttpRequest, AwsError> {
    let mut sdk = SdkHttpRequest::new(SdkBody::from(request.body.clone()));
    sdk.set_method(request.method.as_str())
        .map_err(|_| AwsError::invalid_config("method", "the HTTP method was refused"))?;
    sdk.set_uri(request.url.clone())
        .map_err(|_| AwsError::invalid_config("url", "the request URL could not be parsed"))?;
    for (name, value) in request.headers.entries() {
        sdk.headers_mut()
            .try_insert(name.to_string(), value.to_string())
            .map_err(|_| AwsError::invalid_config("header", "a request header was refused"))?;
    }
    Ok(sdk)
}

/// Map a connector failure: a guard rejection is a non-retryable endpoint
/// refusal; everything else follows [`connector_error`].
fn map_connector_error(error: &ConnectorError) -> AwsError {
    if let Some(rejection) = find_guard_rejection(error) {
        return rejection.error().clone();
    }
    let kind = if error.is_timeout() {
        ConnectorFailure::Timeout
    } else if error.is_io() {
        ConnectorFailure::Io
    } else if error.is_user() {
        ConnectorFailure::User
    } else {
        ConnectorFailure::Other
    };
    connector_error(kind, &DisplayErrorContext(error).to_string())
}

/// Refuse a response that came over a socket whose peer is not public, and
/// poison that connection so it is never reused (#4593).
///
/// A backstop: the connector dials only a literal host, already checked by
/// [`require_public_host`], or the answers of the guarded resolver. It runs once
/// the response headers are in, because the smithy client exposes the
/// connection only then; it keeps any response from a non-public peer from
/// reaching the caller. Connectors that report no connection (test doubles)
/// skip it.
fn check_peer(capture: &CaptureSmithyConnection) -> Result<(), AwsError> {
    let Some(connection) = capture.get() else {
        return Ok(());
    };
    let Some(peer) = connection.remote_addr() else {
        return Ok(());
    };
    require_public_peer(peer.ip()).inspect_err(|_| connection.poison())
}

/// A body-read failure as a redacted boundary error.
fn body_error(error: &aws_smithy_types::byte_stream::error::Error) -> AwsError {
    AwsError::connect(format!(
        "the response body could not be read: {}",
        DisplayErrorContext(error)
    ))
}

impl Transport for SdkTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, AwsError> {
        let sdk_request = Self::admit(request)?;
        let connector = self.connector_for(&request.timeouts);
        let read_timeout = request.timeouts.read();
        self.runtime.block_on(async {
            let response = self.call(connector, sdk_request).await?;
            let status = response.status().as_u16();
            let headers = response_headers(response.headers().iter());
            let collected = tokio::time::timeout(
                read_timeout,
                ByteStream::new(response.into_body()).collect(),
            )
            .await
            .map_err(|_| {
                AwsError::timeout(TimeoutPhase::Read, "the response body was not read in time")
            })?;
            let body = collected
                .map_err(|error| body_error(&error))?
                .into_bytes()
                .to_vec();
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

/// A streamed SDK response body.
struct SdkStream {
    runtime: SdkRuntime,
    status: u16,
    headers: Headers,
    body: ByteStream,
    idle: Duration,
    limiter: ChunkLimiter,
    deadline: Option<std::time::Instant>,
    expired: bool,
}

impl ResponseStream for SdkStream {
    fn status(&self) -> u16 {
        self.status
    }

    fn headers(&self) -> &Headers {
        &self.headers
    }

    fn narrow_body_limit(&mut self, max_body_bytes: u64) {
        self.limiter.narrow(max_body_bytes);
    }

    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        if self.expired
            || self
                .deadline
                .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.expired = true;
            return Err(StreamError::DeadlineExceeded);
        }
        let (runtime, body, idle) = (&self.runtime, &mut self.body, self.idle);
        let deadline = self.deadline;
        let remaining = self.limiter.remaining();
        let mut oversized = false;
        let result = self.limiter.next(|| {
            // The timer must be created inside the runtime.
            let now = std::time::Instant::now();
            let (wait, deadline_binding) = match deadline {
                Some(d) if d.saturating_duration_since(now) <= idle => {
                    (d.saturating_duration_since(now), true)
                }
                _ => (idle, false),
            };
            match runtime.block_on(async { tokio::time::timeout(wait, body.next()).await }) {
                Err(_) if deadline_binding => Err(StreamError::DeadlineExceeded),
                Err(_) => Err(StreamError::IdleTimeout),
                Ok(None) => Ok(None),
                Ok(Some(Err(error))) => Err(StreamError::Failed(body_error(&error))),
                Ok(Some(Ok(bytes)))
                    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > remaining =>
                {
                    oversized = true;
                    Err(StreamError::TooLarge)
                }
                Ok(Some(Ok(bytes))) => Ok(Some(bytes.to_vec())),
            }
        });
        if oversized {
            self.limiter.refuse();
        }
        if result == Err(StreamError::DeadlineExceeded) {
            self.expired = true;
        }
        result
    }
}

impl StreamingTransport for SdkTransport {
    fn open<'a>(
        &'a self,
        request: &HttpRequest,
        limits: &StreamLimits,
    ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
        let deadline_error =
            || AwsError::timeout(TimeoutPhase::Deadline, "the response deadline elapsed");
        if limits
            .deadline()
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            return Err(deadline_error());
        }
        let sdk_request = Self::admit(request)?;
        let connector = self.connector_for(&request.timeouts);
        let response = self.runtime.block_on(async {
            if let Some(deadline) = limits.deadline() {
                tokio::time::timeout_at(deadline.into(), self.call(connector, sdk_request))
                    .await
                    .map_err(|_| deadline_error())?
            } else {
                self.call(connector, sdk_request).await
            }
        })?;
        Ok(Box::new(SdkStream {
            runtime: self.runtime.clone(),
            status: response.status().as_u16(),
            headers: response_headers(response.headers().iter()),
            body: ByteStream::new(response.into_body()),
            idle: request.timeouts.read(),
            limiter: ChunkLimiter::new(*limits),
            deadline: limits.deadline(),
            expired: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ApiClient;
    use crate::config::ClientSettings;
    use crate::endpoint::EndpointPolicy;
    use crate::env::MapEnv;
    use crate::retry::{NoJitter, RetryMode, RetryPolicy};
    use crate::testing::{FakeProvider, FixedResolver, RecordingSleeper};
    use crate::transport::{ApiRequest, Headers, Method};
    use aws_credential_types::provider::future;
    use aws_smithy_runtime_api::client::connection::ConnectionMetadata;
    use aws_smithy_runtime_api::client::http::HttpConnectorFuture;
    use aws_smithy_runtime_api::client::orchestrator::HttpResponse as SdkHttpResponse;
    use aws_smithy_runtime_api::http::StatusCode;
    use std::collections::VecDeque;
    use std::net::{IpAddr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::UNIX_EPOCH;

    /// 2015-08-30T12:36:00Z, the AWS `SigV4` test-suite time.
    fn suite_time() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_440_938_160)
    }

    /// An hour after [`suite_time`].
    fn much_later() -> SystemTime {
        suite_time() + Duration::from_secs(3600)
    }

    fn runtime() -> SdkRuntime {
        SdkRuntime::new().expect("runtime")
    }

    fn test_roots() -> ValidatedRoots {
        ValidatedRoots::for_tests(vec![super::test_support::Pki::new("sdk test CA").ca_der])
    }

    /// A fixed SDK credentials outcome.
    type SdkOutcome = fn() -> Result<SdkCredentials, CredentialsError>;

    /// An SDK credentials provider with a fixed outcome.
    struct FakeSdkProvider(SdkOutcome);

    impl fmt::Debug for FakeSdkProvider {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("FakeSdkProvider")
        }
    }

    impl ProvideCredentials for FakeSdkProvider {
        fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            future::ProvideCredentials::ready((self.0)())
        }
    }

    fn sdk_keys() -> SdkCredentials {
        SdkCredentials::new(
            "FAKEKEYID",
            "fake-secret-value",
            Some("fake-session".to_string()),
            Some(suite_time() + Duration::from_secs(3600)),
            "fake",
        )
    }

    fn sdk_expired() -> SdkCredentials {
        SdkCredentials::new(
            "FAKEKEYID",
            "fake-secret-value",
            None,
            Some(suite_time() - Duration::from_secs(1)),
            "fake",
        )
    }

    fn sdk_not_loaded() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::not_loaded(
            "no AWS_ACCESS_KEY_ID in the environment",
        ))
    }

    fn sdk_leaky_failure() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::provider_error(
            "sso refresh failed: aws_secret_access_key=FAKESECRETVALUE0123456789",
        ))
    }

    /// A fake secret with no label, prefix or scheme: pattern redaction cannot
    /// recognise it (redact.rs documents this), so only never taking provider
    /// text keeps it out of errors (#4594).
    const BARE_SECRET: &str = "Zq7bW2xNcv8RtYp4LmKd9HsF3gJaE6uQ1oXiT5nB";

    /// A second bare fake secret, carried only in an error's source chain.
    const BARE_SOURCE_SECRET: &str = "Hk3pV9sLq2WmZx7RtB5nYc8DfG4jAe6uN1oXiTsQ";

    /// A custom provider error that carries bare secrets in its `Display`, its
    /// derived `Debug` and its source chain.
    #[derive(Debug)]
    struct LeakyProviderError {
        detail: &'static str,
        source: Option<Box<LeakyProviderError>>,
    }

    impl fmt::Display for LeakyProviderError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "custom provider refused: {}", self.detail)
        }
    }

    impl std::error::Error for LeakyProviderError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source
                .as_deref()
                .map(|inner| inner as &(dyn std::error::Error + 'static))
        }
    }

    fn leaky_custom_error() -> LeakyProviderError {
        LeakyProviderError {
            detail: BARE_SECRET,
            source: Some(Box::new(LeakyProviderError {
                detail: BARE_SOURCE_SECRET,
                source: None,
            })),
        }
    }

    fn bare_provider_failure() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::provider_error(leaky_custom_error()))
    }

    fn bare_invalid_configuration() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::invalid_configuration(leaky_custom_error()))
    }

    fn bare_unhandled() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::unhandled(leaky_custom_error()))
    }

    fn bare_not_loaded() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::not_loaded(leaky_custom_error()))
    }

    /// Every rendering of `error` is free of both bare secrets.
    fn assert_no_bare_secret(error: &AwsError) {
        for shown in [
            error.to_string(),
            format!("{error:?}"),
            format!("{error:#?}"),
        ] {
            for secret in [BARE_SECRET, BARE_SOURCE_SECRET] {
                assert!(!shown.contains(&secret[..12]), "{shown}");
            }
        }
        assert!(std::error::Error::source(error).is_none());
    }

    fn sdk_invalid() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::invalid_configuration(
            "profile x has no keys",
        ))
    }

    fn sdk_timed_out() -> Result<SdkCredentials, CredentialsError> {
        Err(CredentialsError::provider_timed_out(Duration::from_secs(5)))
    }

    fn sdk_provider(
        outcome: fn() -> Result<SdkCredentials, CredentialsError>,
    ) -> SdkCredentialsProvider {
        SdkCredentialsProvider::from_sdk(&runtime(), FakeSdkProvider(outcome))
            .with_clock(suite_time)
    }

    #[test]
    fn sdk_credentials_are_adopted_and_never_printed() {
        let provider = sdk_provider(|| Ok(sdk_keys()));
        assert_eq!(provider.name(), SDK_DEFAULT_CHAIN);
        let loaded = provider.load().expect("credentials");
        assert_eq!(loaded.access_key_id().expose(), "FAKEKEYID");
        assert_eq!(loaded.secret_access_key().expose(), "fake-secret-value");
        assert_eq!(
            loaded.session_token().map(crate::redact::Secret::expose),
            Some("fake-session")
        );
        for shown in [format!("{provider:?}"), format!("{loaded:?}")] {
            assert!(!shown.contains("FAKEKEYID"), "{shown}");
            assert!(!shown.contains("fake-secret"), "{shown}");
        }
    }

    #[test]
    fn sdk_credential_failures_map_onto_the_chain_rules() {
        assert_eq!(
            sdk_provider(sdk_leaky_failure).load().unwrap_err(),
            AwsError::provider(SDK_DEFAULT_CHAIN, "the credential provider failed")
        );
        assert_eq!(
            sdk_provider(|| Ok(sdk_expired())).load().unwrap_err(),
            AwsError::CredentialsExpired
        );
        assert_eq!(
            sdk_provider(sdk_not_loaded).load().unwrap_err(),
            AwsError::NoCredentials
        );
        for outcome in [sdk_leaky_failure, sdk_invalid, sdk_timed_out] {
            let error = sdk_provider(outcome).load().unwrap_err();
            assert_eq!(error.code(), "aws_credential_provider");
            for shown in [error.to_string(), format!("{error:?}")] {
                assert!(!shown.contains("FAKESECRETVALUE"), "{shown}");
            }
        }
    }

    #[test]
    fn unlabelled_secrets_in_provider_errors_never_reach_the_boundary_error() {
        // Precondition: redaction alone would let these through.
        for secret in [BARE_SECRET, BARE_SOURCE_SECRET] {
            assert_eq!(crate::redact::redact_text(secret), secret);
        }
        let cases: [(SdkOutcome, &str); 3] = [
            (bare_provider_failure, "the credential provider failed"),
            (
                bare_invalid_configuration,
                "invalid credential configuration",
            ),
            (bare_unhandled, "unexpected credential error"),
        ];
        for (outcome, label) in cases {
            // The SDK error itself does carry the secrets...
            let raw = outcome().unwrap_err();
            let raw_shown = DisplayErrorContext(&raw).to_string();
            assert!(raw_shown.contains(BARE_SECRET), "{raw_shown}");
            assert!(raw_shown.contains(BARE_SOURCE_SECRET), "{raw_shown}");
            // ...the boundary error is the fixed kind plus provider name only.
            let error = sdk_provider(outcome).load().unwrap_err();
            assert_eq!(error, AwsError::provider(SDK_DEFAULT_CHAIN, label));
            assert_no_bare_secret(&error);
            // The same through a provider chain and through the signer.
            let chain = crate::credentials::ProviderChain::new()
                .with(Box::new(sdk_provider(sdk_not_loaded)))
                .with(Box::new(sdk_provider(outcome)));
            assert_no_bare_secret(&chain.load().unwrap_err());
            let mut request = vanilla_request();
            let signed = SigV4Signer::new(sdk_provider(outcome))
                .with_clock(suite_time)
                .sign(&mut request, &suite_scope())
                .unwrap_err();
            assert_eq!(signed, error);
            assert_no_bare_secret(&signed);
            assert!(request.headers.is_empty());
        }
        let not_loaded = sdk_provider(bare_not_loaded).load().unwrap_err();
        assert_eq!(not_loaded, AwsError::NoCredentials);
        assert_no_bare_secret(&not_loaded);
    }

    /// The real aws-config `credential_process` provider over a fake shell
    /// command (`sh -c`): no AWS call, no network.
    fn credential_process(command: String) -> SdkCredentialsProvider {
        SdkCredentialsProvider::from_sdk(
            &runtime(),
            aws_config::credential_process::CredentialProcessProvider::new(command),
        )
        .with_clock(suite_time)
    }

    #[test]
    fn a_failing_credential_process_never_surfaces_its_output() {
        let stderr_command = format!("printf '%s' '{BARE_SECRET}' >&2; exit 3");
        // Precondition: aws-config puts the process's stderr in its error.
        let raw = runtime()
            .block_on(
                aws_config::credential_process::CredentialProcessProvider::new(
                    stderr_command.clone(),
                )
                .provide_credentials(),
            )
            .unwrap_err();
        let raw_shown = DisplayErrorContext(&raw).to_string();
        assert!(raw_shown.contains(BARE_SECRET), "{raw_shown}");

        for command in [
            stderr_command,
            // Output that is not valid credential JSON but holds the secret.
            format!(
                "printf '%s' '{{\"Version\": 1, \"AccessKeyId\": \"{BARE_SOURCE_SECRET}\", \
                 \"SecretAccessKey\": \"{BARE_SECRET}\"'"
            ),
            format!(
                "printf '%s' '{{\"Version\": 2, \"AccessKeyId\": \"FAKEKEYID\", \
                 \"SecretAccessKey\": \"{BARE_SECRET}\"}}'"
            ),
        ] {
            let error = credential_process(command.clone()).load().unwrap_err();
            assert_eq!(
                error,
                AwsError::provider(SDK_DEFAULT_CHAIN, "the credential provider failed"),
                "{command}"
            );
            assert_no_bare_secret(&error);
        }
    }

    #[test]
    fn the_sdk_provider_takes_part_in_a_provider_chain() {
        let chain = crate::credentials::ProviderChain::new()
            .with(Box::new(sdk_provider(sdk_not_loaded)))
            .with(Box::new(FakeProvider::with_keys(
                "profile", "KEY2", "secret2",
            )));
        assert_eq!(chain.load().unwrap().access_key_id().expose(), "KEY2");
        let stopped = crate::credentials::ProviderChain::new()
            .with(Box::new(sdk_provider(sdk_invalid)))
            .with(Box::new(FakeProvider::with_keys(
                "profile", "KEY2", "secret2",
            )));
        assert_eq!(
            stopped.load().unwrap_err().code(),
            "aws_credential_provider"
        );
    }

    fn vanilla_request() -> HttpRequest {
        HttpRequest {
            method: Method::Get,
            url: "https://example.amazonaws.com/".to_string(),
            headers: Headers::new(),
            body: Vec::new(),
            timeouts: Timeouts::default(),
        }
    }

    fn suite_scope() -> SigningScope {
        SigningScope {
            region: "us-east-1".to_string(),
            service: "service".to_string(),
        }
    }

    #[test]
    fn sigv4_matches_the_aws_test_suite_get_vanilla_vector() {
        let signer = SigV4Signer::new(FakeProvider::with_keys(
            "suite",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        ))
        .with_clock(suite_time);
        let mut request = vanilla_request();
        signer.sign(&mut request, &suite_scope()).expect("signed");
        assert_eq!(request.headers.get("x-amz-date"), Some("20150830T123600Z"));
        assert_eq!(
            request.headers.get("authorization"),
            Some(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
                 SignedHeaders=host;x-amz-date, \
                 Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
            )
        );
        assert!(request.headers.get("x-amz-security-token").is_none());
        let shown = format!("{request:?}");
        assert!(!shown.contains("5fa00fa3"), "{shown}");
        assert!(!format!("{signer:?}").contains("AKID"));
    }

    fn suite_signer() -> SigV4Signer<FakeProvider> {
        SigV4Signer::new(FakeProvider::with_keys(
            "suite",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        ))
        .with_clock(suite_time)
    }

    /// Method, URL, headers, body, signed headers, signature.
    type SuiteVector = (
        Method,
        &'static str,
        Vec<(&'static str, &'static str)>,
        &'static [u8],
        &'static str,
        &'static str,
    );

    /// T-SIG-1: further vectors from the AWS `SigV4` test suite (2015-08-30).
    #[test]
    fn sigv4_matches_more_aws_test_suite_vectors() {
        let credential = "Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request";
        let cases: [SuiteVector; 3] = [
            (
                Method::Post,
                "https://example.amazonaws.com/",
                vec![],
                b"",
                "host;x-amz-date",
                "5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b",
            ),
            (
                Method::Get,
                "https://example.amazonaws.com/?Param2=value2&Param1=value1",
                vec![],
                b"",
                "host;x-amz-date",
                "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500",
            ),
            (
                Method::Post,
                "https://example.amazonaws.com/",
                vec![("content-type", "application/x-www-form-urlencoded")],
                b"Param1=value1",
                "content-type;host;x-amz-date",
                "ff11897932ad3f4e8b18135d722051e5ac45fc38421b1da7b9d196a0fe09473a",
            ),
        ];
        for (method, url, headers, body, signed, signature) in cases {
            let mut request = vanilla_request();
            request.method = method;
            request.url = url.to_string();
            request.body = body.to_vec();
            for (name, value) in headers {
                request.headers.insert(name, value).unwrap();
            }
            suite_signer()
                .sign(&mut request, &suite_scope())
                .expect("signed");
            assert_eq!(
                request.headers.get("authorization"),
                Some(
                    format!(
                        "AWS4-HMAC-SHA256 {credential}, SignedHeaders={signed}, Signature={signature}"
                    )
                    .as_str()
                ),
                "{url}"
            );
        }
    }

    /// R-HDR: the signer refuses authority and framing headers (a supplied
    /// `Host` would otherwise be signed instead of the URL authority).
    #[test]
    fn the_signer_refuses_authority_and_framing_headers() {
        for name in ["Host", "content-length", "Transfer-Encoding", "connection"] {
            let mut request = vanilla_request();
            request.headers.insert(name, "evil.example").unwrap();
            let error = suite_signer()
                .sign(&mut request, &suite_scope())
                .unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{name}");
            assert!(request.headers.get("authorization").is_none(), "{name}");
            assert!(!error.to_string().contains("evil.example"));
        }
    }

    /// R-HDR: both transports refuse them before any connector is called.
    #[test]
    fn the_transport_refuses_authority_and_framing_headers_before_sending() {
        let connector = FakeConnector::new(vec![Ok((200, vec![], "{}"))]);
        let seen = Arc::clone(&connector.seen);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let limits = StreamLimits::new(1024, 64).unwrap();
        for name in [
            "host",
            "Content-Length",
            "te",
            "trailer",
            "keep-alive",
            "upgrade",
        ] {
            let mut request = vanilla_request();
            request.headers.insert(name, "x").unwrap();
            assert_eq!(
                transport.send(&request).unwrap_err().code(),
                "aws_invalid_config",
                "{name}"
            );
            assert_eq!(
                transport.open(&request, &limits).err().map(|e| e.code()),
                Some("aws_invalid_config"),
                "{name}"
            );
        }
        assert!(seen.lock().unwrap().methods.is_empty());
    }

    #[test]
    fn the_streaming_transport_applies_the_same_guards_and_returns_the_head() {
        let connector = FakeConnector::new(vec![Ok((
            202,
            vec![("content-type", "text/event-stream")],
            "data: 1\n\n",
        ))]);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let limits = StreamLimits::new(1024, 4).unwrap();
        let mut plain = vanilla_request();
        plain.url = "http://logs.us-east-1.amazonaws.com/".to_string();
        assert_eq!(
            transport.open(&plain, &limits).err().map(|e| e.code()),
            Some("aws_invalid_config")
        );
        let mut literal = vanilla_request();
        literal.url = "https://169.254.169.254/".to_string();
        assert_eq!(
            transport.open(&literal, &limits).err().map(|e| e.code()),
            Some("aws_invalid_config")
        );
        let mut stream = transport.open(&vanilla_request(), &limits).expect("stream");
        assert_eq!(stream.status(), 202);
        assert_eq!(
            stream.headers().get("content-type"),
            Some("text/event-stream")
        );
        let mut body = Vec::new();
        while let Some(chunk) = stream.next_chunk().expect("chunk") {
            assert!(chunk.len() <= 4);
            body.extend(chunk);
        }
        assert_eq!(body, b"data: 1\n\n");
    }

    #[test]
    fn a_streamed_response_from_a_non_public_peer_is_refused() {
        let connector = FakeConnector::new(vec![Ok((200, vec![], "{}"))]).with_peer("10.0.0.1:443");
        let poisoned = Arc::clone(&connector.poisoned);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let limits = StreamLimits::new(1024, 64).unwrap();
        let error = transport
            .open(&vanilla_request(), &limits)
            .err()
            .expect("refused");
        assert_eq!(error.code(), "aws_invalid_config");
        assert!(poisoned.load(Ordering::SeqCst));
    }

    /// A Send + Sync provider with a session token.
    struct TokenProvider;

    impl CredentialsProvider for TokenProvider {
        fn name(&self) -> &'static str {
            "token"
        }

        fn load(&self) -> Result<crate::credentials::Credentials, AwsError> {
            crate::credentials::Credentials::new(
                "FAKEKEYID",
                "fake-secret-value",
                Some("FAKE-SESSION-TOKEN"),
                Some(suite_time() + Duration::from_secs(60)),
            )
        }
    }

    #[test]
    fn temporary_credentials_send_the_session_token_and_expired_ones_are_refused() {
        let mut request = vanilla_request();
        SigV4Signer::new(TokenProvider)
            .with_clock(suite_time)
            .sign(&mut request, &suite_scope())
            .expect("signed");
        assert_eq!(
            request.headers.get("x-amz-security-token"),
            Some("FAKE-SESSION-TOKEN")
        );
        assert!(!format!("{request:?}").contains("FAKE-SESSION-TOKEN"));

        let mut late = vanilla_request();
        assert_eq!(
            SigV4Signer::new(TokenProvider)
                .with_clock(much_later)
                .sign(&mut late, &suite_scope())
                .unwrap_err(),
            AwsError::CredentialsExpired
        );
        assert!(late.headers.is_empty());
    }

    #[test]
    fn signer_credential_errors_propagate_unsigned() {
        let signer = SigV4Signer::new(FakeProvider::missing("environment")).with_clock(suite_time);
        let mut request = vanilla_request();
        assert_eq!(
            signer.sign(&mut request, &suite_scope()).unwrap_err(),
            AwsError::NoCredentials
        );
        assert!(request.headers.get("authorization").is_none());
    }

    /// What the fake connector saw.
    #[derive(Default)]
    struct Seen {
        methods: Vec<String>,
        uris: Vec<String>,
        authorizations: Vec<Option<String>>,
        bodies: Vec<Vec<u8>>,
    }

    /// Scripted connector failures.
    #[derive(Clone, Copy)]
    enum Fail {
        ConnectTimeout,
        ReadTimeout,
        Refused,
        GuardRefused,
    }

    impl Fail {
        fn error(self) -> ConnectorError {
            match self {
                Self::ConnectTimeout => {
                    ConnectorError::timeout("HTTP connect timeout occurred after 1s".into())
                }
                Self::ReadTimeout => {
                    ConnectorError::timeout("HTTP read timeout occurred after 1s".into())
                }
                Self::Refused => {
                    ConnectorError::io("connection refused; password=FAKEPASSWORDVALUE".into())
                }
                Self::GuardRefused => {
                    let resolver = FixedResolver::new().answer_any(&["169.254.169.254"]);
                    let rejection = guarded_addresses("evil.amazonaws.com", &resolver).unwrap_err();
                    ConnectorError::io(Box::new(std::io::Error::other(ResolveDnsError::new(
                        rejection,
                    ))))
                }
            }
        }
    }

    /// Status, headers and body text of a scripted response, or a failure.
    type Outcome = Result<(u16, Vec<(&'static str, &'static str)>, &'static str), Fail>;

    /// An SDK HTTP connector that replays scripted outcomes; no sockets.
    #[derive(Clone)]
    struct FakeConnector {
        script: Arc<Mutex<VecDeque<Outcome>>>,
        seen: Arc<Mutex<Seen>>,
        /// The socket peer the connection reports, as the hyper client does.
        peer: Option<SocketAddr>,
        poisoned: Arc<AtomicBool>,
    }

    impl fmt::Debug for FakeConnector {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("FakeConnector")
        }
    }

    impl FakeConnector {
        fn new(outcomes: Vec<Outcome>) -> Self {
            Self {
                script: Arc::new(Mutex::new(outcomes.into())),
                seen: Arc::new(Mutex::new(Seen::default())),
                peer: None,
                poisoned: Arc::new(AtomicBool::new(false)),
            }
        }

        fn with_peer(mut self, peer: &str) -> Self {
            self.peer = Some(peer.parse().expect("fixture socket address"));
            self
        }
    }

    impl HttpConnector for FakeConnector {
        fn call(&self, request: SdkHttpRequest) -> HttpConnectorFuture {
            if let (Some(peer), Some(capture)) =
                (self.peer, request.extension::<CaptureSmithyConnection>())
            {
                let poisoned = Arc::clone(&self.poisoned);
                capture.set_connection_retriever(move || {
                    let poisoned = Arc::clone(&poisoned);
                    Some(
                        ConnectionMetadata::builder()
                            .proxied(false)
                            .remote_addr(peer)
                            .poison_fn(move || poisoned.store(true, Ordering::SeqCst))
                            .build(),
                    )
                });
            }
            {
                let mut seen = self.seen.lock().unwrap();
                seen.methods.push(request.method().to_string());
                seen.uris.push(request.uri().to_string());
                seen.authorizations
                    .push(request.headers().get("authorization").map(str::to_string));
                seen.bodies
                    .push(request.body().bytes().unwrap_or_default().to_vec());
            }
            let next = self.script.lock().unwrap().pop_front();
            let result = match next {
                Some(Ok((status, headers, body))) => {
                    let mut response = SdkHttpResponse::new(
                        StatusCode::try_from(status).expect("status"),
                        SdkBody::from(body.as_bytes().to_vec()),
                    );
                    for (name, value) in headers {
                        response
                            .headers_mut()
                            .try_insert(name, value.to_string())
                            .expect("header");
                    }
                    Ok(response)
                }
                Some(Err(fail)) => Err(fail.error()),
                None => Err(ConnectorError::other("script exhausted".into(), None)),
            };
            HttpConnectorFuture::ready(result)
        }
    }

    #[test]
    fn the_transport_sends_the_request_as_built_and_returns_the_response() {
        let connector = FakeConnector::new(vec![Ok((
            201,
            vec![
                ("x-amzn-RequestId", "req-123"),
                ("content-type", "application/json"),
            ],
            "{\"ok\":true}",
        ))]);
        let seen = Arc::clone(&connector.seen);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let mut request = vanilla_request();
        request.method = Method::Post;
        request.url = "https://logs.us-east-1.amazonaws.com/x?a=b%20c".to_string();
        request
            .headers
            .insert("authorization", "FAKE-SIGNED")
            .unwrap();
        request.body = b"{\"q\":1}".to_vec();
        let response = transport.send(&request).expect("response");
        assert_eq!(response.status, 201);
        assert_eq!(response.headers.get("x-amzn-requestid"), Some("req-123"));
        assert_eq!(response.body_text(), Some("{\"ok\":true}"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.methods, vec!["POST"]);
        assert_eq!(
            seen.uris,
            vec!["https://logs.us-east-1.amazonaws.com/x?a=b%20c"]
        );
        assert_eq!(seen.authorizations, vec![Some("FAKE-SIGNED".to_string())]);
        assert_eq!(seen.bodies, vec![b"{\"q\":1}".to_vec()]);
        assert!(!format!("{transport:?}").contains("FAKE"));
    }

    #[test]
    fn non_https_urls_are_refused_before_anything_is_sent() {
        let connector = FakeConnector::new(vec![Ok((200, vec![], ""))]);
        let seen = Arc::clone(&connector.seen);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let mut request = vanilla_request();
        request.url = "http://logs.us-east-1.amazonaws.com/".to_string();
        assert_eq!(
            transport.send(&request).unwrap_err().code(),
            "aws_invalid_config"
        );
        assert!(seen.lock().unwrap().methods.is_empty());
    }

    #[test]
    fn non_public_literal_ip_urls_are_refused_before_the_connector_is_called() {
        let connector = FakeConnector::new(vec![Ok((200, vec![], ""))]);
        let seen = Arc::clone(&connector.seen);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        for url in [
            "https://127.0.0.1/",
            "https://127.0.0.1:443/x",
            "https://10.0.0.1/",
            "https://169.254.169.254/latest/meta-data/",
            "https://2130706433/",
            "https://0177.0.0.1/",
            "https://0x7f.0.0.1/",
            "https://0xA9FEA9FE/",
            "https://127.1/",
            "https://127.0.0.1./",
            "https://[::1]/",
            "https://[::]/",
            "https://[fe80::1]/",
            "https://[::ffff:127.0.0.1]/",
            "https://[::ffff:a9fe:a9fe]/",
            "https://[64:ff9b::a9fe:a9fe]/",
            "https://[::127.0.0.1]/",
        ] {
            let mut request = vanilla_request();
            request.url = url.to_string();
            let error = transport.send(&request).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{url}");
            assert!(!error.is_retryable(), "{url}");
            assert!(error.to_string().contains("non-public address"), "{error}");
        }
        for url in [
            "https://[fe80::1%25eth0]/",
            "https://256.0.0.1/",
            "https://127.0.0.%31/",
        ] {
            let mut request = vanilla_request();
            request.url = url.to_string();
            let error = transport.send(&request).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{url}");
        }
        assert!(seen.lock().unwrap().methods.is_empty());
        // A public literal and a name still go out.
        for url in [
            "https://52.94.0.10/",
            "https://logs.us-east-1.amazonaws.com/",
        ] {
            let connector = FakeConnector::new(vec![Ok((200, vec![], ""))]);
            let seen = Arc::clone(&connector.seen);
            let mut request = vanilla_request();
            request.url = url.to_string();
            SdkTransport::with_connector(&runtime(), connector)
                .send(&request)
                .expect(url);
            assert_eq!(seen.lock().unwrap().uris, vec![url]);
        }
    }

    #[test]
    fn the_production_transport_refuses_literal_ips_without_connecting() {
        // The guarded production stack: the refusal comes before the hyper
        // connector, which would skip the resolver for a literal IP.
        let transport = SdkTransport::with_resolver(
            &runtime(),
            &test_roots(),
            Arc::new(StaticResolver("52.94.0.10")),
        )
        .expect("transport");
        for url in [
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://2130706433/",
            "https://[::ffff:169.254.169.254]/",
        ] {
            let mut request = vanilla_request();
            request.url = url.to_string();
            let error = transport.send(&request).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{url}");
            assert!(error.to_string().contains("non-public address"), "{error}");
        }
        assert!(transport.connectors.lock().unwrap().is_empty());
    }

    #[test]
    fn a_response_from_a_non_public_socket_peer_is_refused_and_the_connection_poisoned() {
        for peer in [
            "127.0.0.1:443",
            "10.0.0.1:443",
            "169.254.169.254:443",
            "[::1]:443",
            "[::ffff:127.0.0.1]:443",
            "[fe80::1%2]:443",
        ] {
            let connector = FakeConnector::new(vec![Ok((200, vec![], "{}"))]).with_peer(peer);
            let poisoned = Arc::clone(&connector.poisoned);
            let transport = SdkTransport::with_connector(&runtime(), connector);
            let error = transport.send(&vanilla_request()).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{peer}");
            assert!(!error.is_retryable(), "{peer}");
            assert!(
                error.to_string().contains("the connection reached"),
                "{error}"
            );
            assert!(poisoned.load(Ordering::SeqCst), "{peer}");
        }
        for peer in ["52.94.0.10:443", "[2600:1f18::1]:443"] {
            let connector = FakeConnector::new(vec![Ok((200, vec![], "{}"))]).with_peer(peer);
            let poisoned = Arc::clone(&connector.poisoned);
            let transport = SdkTransport::with_connector(&runtime(), connector);
            let response = transport.send(&vanilla_request()).expect(peer);
            assert_eq!(response.status, 200);
            assert!(!poisoned.load(Ordering::SeqCst), "{peer}");
        }
    }

    #[test]
    fn connector_failures_become_redacted_boundary_errors() {
        let connector = FakeConnector::new(vec![
            Err(Fail::ConnectTimeout),
            Err(Fail::ReadTimeout),
            Err(Fail::Refused),
            Err(Fail::GuardRefused),
        ]);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let request = vanilla_request();
        let first = transport.send(&request).unwrap_err();
        assert!(matches!(
            first,
            AwsError::Timeout {
                phase: TimeoutPhase::Connect,
                ..
            }
        ));
        let second = transport.send(&request).unwrap_err();
        assert!(matches!(
            second,
            AwsError::Timeout {
                phase: TimeoutPhase::Read,
                ..
            }
        ));
        let third = transport.send(&request).unwrap_err();
        assert_eq!(third.code(), "aws_connect");
        assert!(third.is_retryable());
        for shown in [third.to_string(), format!("{third:?}")] {
            assert!(!shown.contains("FAKEPASSWORDVALUE"), "{shown}");
        }
        let fourth = transport.send(&request).unwrap_err();
        assert_eq!(fourth.code(), "aws_invalid_config");
        assert!(!fourth.is_retryable());
        assert!(fourth.to_string().contains("169.254.169.254"), "{fourth}");
    }

    /// A thread-safe resolver with one fixed answer.
    struct StaticResolver(&'static str);

    impl HostResolver for StaticResolver {
        fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            Ok(vec![self.0.parse().expect("fixture address")])
        }
    }

    #[test]
    fn the_sdk_dns_resolver_enforces_the_public_address_policy() {
        let rt = runtime();
        let public = GuardedResolver {
            inner: Arc::new(StaticResolver("52.94.0.10")),
        };
        let addresses = rt
            .block_on(public.resolve_dns("logs.us-east-1.amazonaws.com"))
            .expect("public");
        assert_eq!(addresses, vec!["52.94.0.10".parse::<IpAddr>().unwrap()]);
        let private = GuardedResolver {
            inner: Arc::new(StaticResolver("10.1.2.3")),
        };
        let error = rt
            .block_on(private.resolve_dns("logs.us-east-1.amazonaws.com"))
            .unwrap_err();
        let rejection = find_guard_rejection(&error).expect("guard rejection in the chain");
        assert!(rejection.to_string().contains("10.1.2.3"));
        assert!(!format!("{private:?}").contains("10.1.2.3"));
    }

    #[test]
    fn the_production_transport_builds_without_touching_the_network() {
        // Building a connector loads TLS roots but opens no socket.
        let transport = SdkTransport::with_resolver(
            &runtime(),
            &test_roots(),
            Arc::new(StaticResolver("52.94.0.10")),
        )
        .expect("transport");
        let first = transport.connector_for(&Timeouts::default());
        let again = transport.connector_for(&Timeouts::default());
        let other = transport
            .connector_for(&Timeouts::new(Duration::from_secs(5), Duration::from_secs(7)).unwrap());
        drop((first, again, other));
        assert_eq!(transport.connectors.lock().unwrap().len(), 2);
    }

    #[test]
    fn api_client_retries_through_the_sdk_signer_and_transport_offline() {
        let resolver = FixedResolver::new();
        let settings = ClientSettings {
            retry: RetryPolicy::new(
                2,
                RetryMode::Standard,
                Duration::from_millis(10),
                Duration::from_secs(1),
            )
            .expect("policy"),
            ..ClientSettings::default()
        };
        let config = ClientConfig::resolve(
            "logs",
            &settings,
            &EndpointPolicy::aws_public(),
            &MapEnv::new().with("AWS_REGION", "us-west-2"),
            &resolver,
        )
        .expect("config");
        let connector = FakeConnector::new(vec![
            Ok((503, vec![], "{\"__type\":\"ServiceUnavailableException\"}")),
            Ok((200, vec![], "{\"logGroups\":[]}")),
        ]);
        let seen = Arc::clone(&connector.seen);
        let transport = SdkTransport::with_connector(&runtime(), connector);
        let signer = SigV4Signer::new(FakeProvider::with_keys("p", "FAKEKEYID", "fake-secret"))
            .with_clock(suite_time);
        let sleeper = RecordingSleeper::new();
        let client = ApiClient::new(&config, &transport, &signer, &sleeper, &NoJitter);
        let result = client
            .execute_json(&ApiRequest::new(Method::Post, "/"))
            .expect("second attempt succeeds");
        assert_eq!(result.attempts, 2);
        assert_eq!(sleeper.sleeps().len(), 1);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.uris, vec!["https://logs.us-west-2.amazonaws.com/"; 2]);
        for authorization in &seen.authorizations {
            let value = authorization.as_deref().expect("signed");
            assert!(
                value.starts_with(
                    "AWS4-HMAC-SHA256 Credential=FAKEKEYID/20150830/us-west-2/logs/aws4_request"
                ),
                "{value}"
            );
        }
    }
}
