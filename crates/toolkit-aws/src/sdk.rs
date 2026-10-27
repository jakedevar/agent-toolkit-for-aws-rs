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
//!
//! Retries stay with [`crate::ApiClient`] and its [`crate::RetryPolicy`]; the
//! transport sends exactly one request per call.
//!
//! Redaction: no SDK value is ever formatted with `Debug`. SDK errors are
//! rendered with their `Display` chain and pass through the redacting
//! [`AwsError`] constructors; the adapters print fixed, value-free `Debug`.
//!
//! Runtime: the boundary is synchronous, so each adapter drives SDK futures on
//! an [`SdkRuntime`] (a current-thread tokio runtime). Do not call the adapters
//! from inside another tokio runtime; `block_on` would panic there.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::Region;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_credential_types::Credentials as SdkCredentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use aws_smithy_async::rt::sleep::TokioSleep;
use aws_smithy_http_client::{tls, Connector};
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
    require_https, response_headers, ConnectorFailure, CredentialFailure, SDK_DEFAULT_CHAIN,
};
use crate::config::ClientConfig;
use crate::credentials::CredentialsProvider;
use crate::endpoint::{HostResolver, SystemResolver};
use crate::error::{AwsError, TimeoutPhase};
use crate::transport::{HttpRequest, HttpResponse, Signer, SigningScope, Timeouts, Transport};

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

impl fmt::Debug for SdkRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SdkRuntime")
    }
}

/// Classify an SDK credential failure without formatting it with `Debug`.
fn map_credentials_error(error: &CredentialsError) -> AwsError {
    let kind = match error {
        CredentialsError::CredentialsNotLoaded(_) => CredentialFailure::NotLoaded,
        CredentialsError::ProviderTimedOut(_) => CredentialFailure::TimedOut,
        CredentialsError::InvalidConfiguration(_) => CredentialFailure::InvalidConfiguration,
        CredentialsError::ProviderError(_) => CredentialFailure::Provider,
        _ => CredentialFailure::Unhandled,
    };
    credential_error(
        SDK_DEFAULT_CHAIN,
        kind,
        &DisplayErrorContext(error).to_string(),
    )
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
        let mut builder = DefaultCredentialsChain::builder()
            .region(Region::new(config.region().name().to_string()));
        if let Some(profile) = config.profile() {
            builder = builder.profile_name(profile);
        }
        let chain = runtime.block_on(builder.build());
        Self::from_sdk(runtime, chain)
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

fn signing_error(error: &(dyn std::error::Error + 'static)) -> AwsError {
    AwsError::invalid_config(
        "signing",
        format!(
            "the request could not be signed: {}",
            DisplayErrorContext(error)
        ),
    )
}

impl<P: CredentialsProvider> Signer for SigV4Signer<P> {
    fn sign(&self, request: &mut HttpRequest, scope: &SigningScope) -> Result<(), AwsError> {
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
                .map_err(|error| signing_error(&error))?
                .into();
            let signable = SignableRequest::new(
                request.method.as_str(),
                request.url.as_str(),
                request.headers.entries(),
                SignableBody::Bytes(&request.body),
            )
            .map_err(|error| signing_error(&error))?;
            let (instructions, _signature) = sign(signable, &params)
                .map_err(|error| signing_error(&error))?
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

type ConnectorFactory = dyn Fn(&Timeouts) -> SharedHttpConnector + Send + Sync;

/// [`Transport`] over the smithy hyper HTTP client.
pub struct SdkTransport {
    runtime: SdkRuntime,
    factory: Box<ConnectorFactory>,
    connectors: Mutex<Vec<(Timeouts, SharedHttpConnector)>>,
}

impl SdkTransport {
    /// The production transport: system DNS behind the public-address guard.
    #[must_use]
    pub fn new(runtime: &SdkRuntime) -> Self {
        Self::with_resolver(runtime, Arc::new(SystemResolver))
    }

    /// The production transport with an injected resolver (still guarded).
    #[must_use]
    pub fn with_resolver(
        runtime: &SdkRuntime,
        resolver: Arc<dyn HostResolver + Send + Sync>,
    ) -> Self {
        let guard = GuardedResolver { inner: resolver };
        let factory_runtime = runtime.clone();
        let factory = move |timeouts: &Timeouts| {
            // The hyper client registers timers and its executor with the
            // current runtime, so build it inside the adapter's runtime.
            let _entered = factory_runtime.0.enter();
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
                .build_with_resolver(guard.clone());
            SharedHttpConnector::new(connector)
        };
        Self {
            runtime: runtime.clone(),
            factory: Box::new(factory),
            connectors: Mutex::new(Vec::new()),
        }
    }

    /// A transport over a given SDK HTTP connector, whatever the timeouts
    /// (tests and custom stacks; the DNS guard is the connector's business).
    #[must_use]
    pub fn with_connector(runtime: &SdkRuntime, connector: impl HttpConnector + 'static) -> Self {
        let shared = SharedHttpConnector::new(connector);
        Self {
            runtime: runtime.clone(),
            factory: Box::new(move |_: &Timeouts| shared.clone()),
            connectors: Mutex::new(Vec::new()),
        }
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

/// Send one request and read the whole body within the read timeout.
async fn exchange(
    connector: SharedHttpConnector,
    request: SdkHttpRequest,
    read_timeout: Duration,
) -> Result<HttpResponse, AwsError> {
    let response = connector
        .call(request)
        .await
        .map_err(|error| map_connector_error(&error))?;
    let status = response.status().as_u16();
    let headers = response_headers(response.headers().iter());
    let collected = tokio::time::timeout(
        read_timeout,
        ByteStream::new(response.into_body()).collect(),
    )
    .await
    .map_err(|_| AwsError::timeout(TimeoutPhase::Read, "the response body was not read in time"))?;
    let body = collected
        .map_err(|error| {
            AwsError::connect(format!(
                "the response body could not be read: {}",
                DisplayErrorContext(&error)
            ))
        })?
        .into_bytes()
        .to_vec();
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

impl Transport for SdkTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, AwsError> {
        require_https(&request.url)?;
        let sdk_request = to_sdk_request(request)?;
        let connector = self.connector_for(&request.timeouts);
        self.runtime
            .block_on(exchange(connector, sdk_request, request.timeouts.read()))
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
    use aws_smithy_runtime_api::client::http::HttpConnectorFuture;
    use aws_smithy_runtime_api::client::orchestrator::HttpResponse as SdkHttpResponse;
    use aws_smithy_runtime_api::http::StatusCode;
    use std::collections::VecDeque;
    use std::net::IpAddr;
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

    /// An SDK credentials provider with a fixed outcome.
    struct FakeSdkProvider(fn() -> Result<SdkCredentials, CredentialsError>);

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
            }
        }
    }

    impl HttpConnector for FakeConnector {
        fn call(&self, request: SdkHttpRequest) -> HttpConnectorFuture {
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
        let transport =
            SdkTransport::with_resolver(&runtime(), Arc::new(StaticResolver("52.94.0.10")));
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
