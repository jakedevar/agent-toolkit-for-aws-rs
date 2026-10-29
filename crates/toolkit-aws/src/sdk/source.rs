//! Credential-provider construction (#4413 S1): R-PROV's one provider-HTTP
//! seam, R-SRC's single-source builders and the guarded resolver.
//!
//! **The only module that builds SDK credential providers.** The source gate
//! (`gates`) fails if the default-chain, profile, web-identity, ECS or
//! environment provider constructor appears anywhere else (and the IMDS
//! provider is never built directly: only the default chain reaches it), and
//! every builder here takes the one shared [`ProviderConfig`] from
//! [`provider_config`] (the resolved region and the provider-plane HTTP
//! client).
//!
//! - [`ProviderHttp`]: the client every provider-plane request (STS, SSO,
//!   SSO-OIDC, Sign-in, ECS/EKS, IMDS) goes through: an explicit-roots client
//!   built from [`ValidatedRoots`] for the proxy, the SDK default for the
//!   existing `default_chain`, or an injected recorder in tests.
//! - [`SdkCredentialsProvider::default_chain_with_http`]: the SDK default
//!   chain over that client (Absent class only).
//! - [`GuardedCredentials`]: per attempt, the R-ABS preflight, the R-SRC
//!   selector and **only** the selected provider; R-PREC across the load and
//!   D1's single rebuild; R-STICKY for the session. Terminality comes from
//!   which provider was built; no error text is ever read.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::ecs::EcsCredentialsProvider;
use aws_config::environment::EnvironmentVariableCredentialsProvider;
use aws_config::profile::ProfileFileCredentialsProvider;
use aws_config::provider_config::ProviderConfig;
use aws_config::web_identity_token::WebIdentityTokenCredentialsProvider;
use aws_config::Region;
use aws_smithy_http_client::{tls, Builder as HttpClientBuilder};
use aws_smithy_runtime_api::client::http::{HttpClient, SharedHttpClient};

use super::profile::profile_view;
use super::roots::ValidatedRoots;
use super::{SdkCredentialsProvider, SdkRuntime};
use crate::config::ClientConfig;
use crate::credentials::{Credentials, CredentialsProvider};
use crate::env::{Env, ProcessEnv};
use crate::error::AwsError;
use crate::preflight::{
    classify, select_source, AttemptOutcome, AuthDecision, CredentialRefusal, Preflight,
    RequestAuth, SelectedProfile, Selection, SessionAuth, SourceKind,
};

/// The HTTP client of the credential provider plane.
#[derive(Clone)]
pub enum ProviderHttp {
    /// aws-config's own default HTTPS client (the existing `default_chain`).
    SdkDefault,
    /// A rustls (aws-lc) client that trusts exactly the validated roots, with
    /// native roots disabled (R-ROOTS, both planes).
    Validated {
        /// The client.
        client: SharedHttpClient,
        /// The roots it trusts.
        roots: ValidatedRoots,
    },
    /// An injected client (the T-PROV recorder, or a custom stack).
    Injected(SharedHttpClient),
}

impl ProviderHttp {
    /// The proxy's provider-plane client over `roots`.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn validated(roots: &ValidatedRoots) -> Result<Self, AwsError> {
        let client = HttpClientBuilder::new()
            .tls_provider(tls::Provider::Rustls(
                tls::rustls_provider::CryptoMode::AwsLc,
            ))
            .tls_context(roots.tls_context()?)
            .build_https();
        Ok(Self::Validated {
            client,
            roots: roots.clone(),
        })
    }

    /// An injected client.
    #[must_use]
    pub fn injected(client: impl HttpClient + 'static) -> Self {
        Self::Injected(SharedHttpClient::new(client))
    }

    /// The roots of a validated client.
    #[must_use]
    pub fn roots(&self) -> Option<&ValidatedRoots> {
        match self {
            Self::Validated { roots, .. } => Some(roots),
            Self::SdkDefault | Self::Injected(_) => None,
        }
    }
}

impl fmt::Debug for ProviderHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SdkDefault => "ProviderHttp::SdkDefault",
            Self::Validated { .. } => "ProviderHttp::Validated",
            Self::Injected(_) => "ProviderHttp::Injected",
        })
    }
}

/// The one shared provider configuration: the boundary's resolved region and
/// the provider-plane client. Built only from startup settings.
fn provider_config(region: &str, http: &ProviderHttp) -> ProviderConfig {
    let config = ProviderConfig::default().with_region(Some(Region::new(region.to_string())));
    match http {
        ProviderHttp::SdkDefault => config,
        ProviderHttp::Validated { client, .. } | ProviderHttp::Injected(client) => {
            config.with_http_client(client.clone())
        }
    }
}

impl SdkCredentialsProvider {
    /// The SDK default credential chain for `config` over the provider-plane
    /// client `http` (R-PROV): the profile from `AWS_PROFILE` (validated by
    /// [`ClientConfig`]) for credentials only, and the boundary's resolved
    /// region for any STS, SSO or Sign-in call. Every IMDS, ECS, STS, SSO,
    /// SSO-OIDC and Sign-in request of the chain uses `http`.
    #[must_use]
    pub fn default_chain_with_http(
        runtime: &SdkRuntime,
        config: &ClientConfig,
        http: &ProviderHttp,
    ) -> Self {
        let region = config.region().name();
        Self::from_sdk(
            runtime,
            build_default_chain(runtime, region, config.profile(), http),
        )
    }
}

fn build_default_chain(
    runtime: &SdkRuntime,
    region: &str,
    profile: Option<&str>,
    http: &ProviderHttp,
) -> DefaultCredentialsChain {
    let mut builder = DefaultCredentialsChain::builder()
        .configure(provider_config(region, http))
        .region(Region::new(region.to_string()));
    if let Some(profile) = profile {
        builder = builder.profile_name(profile);
    }
    runtime.block_on(builder.build())
}

/// Build exactly the provider R-SRC selected, over the shared configuration.
fn build_source(
    runtime: &SdkRuntime,
    kind: SourceKind,
    region: &str,
    profile: &SelectedProfile,
    http: &ProviderHttp,
) -> SdkCredentialsProvider {
    let pc = provider_config(region, http);
    match kind {
        SourceKind::Env => {
            SdkCredentialsProvider::from_sdk(runtime, EnvironmentVariableCredentialsProvider::new())
        }
        SourceKind::Profile => SdkCredentialsProvider::from_sdk(
            runtime,
            ProfileFileCredentialsProvider::builder()
                .configure(&pc)
                .profile_name(profile.name())
                .build(),
        ),
        SourceKind::WebIdentity => SdkCredentialsProvider::from_sdk(
            runtime,
            WebIdentityTokenCredentialsProvider::builder()
                .configure(&pc)
                .build(),
        ),
        SourceKind::Ecs => SdkCredentialsProvider::from_sdk(
            runtime,
            EcsCredentialsProvider::builder().configure(&pc).build(),
        ),
        SourceKind::DefaultChain => SdkCredentialsProvider::from_sdk(
            runtime,
            build_default_chain(
                runtime,
                region,
                profile.is_explicit().then(|| profile.name()),
                http,
            ),
        ),
    }
}

/// What one request does about credentials.
#[derive(Debug)]
pub enum RequestCredentials {
    /// Sign with these.
    Sign(Credentials),
    /// Send unsigned (`--skip-auth`, genuine absence, unsigned session).
    Unsigned,
    /// Fail closed with the fixed credentials message.
    Refused(CredentialRefusal),
}

/// The R-SRC selector type (a pure function; replaceable in tests only).
type Selector = fn(&Preflight) -> Selection;

/// A hook run between the load and the D1 rebuild (tests only).
#[cfg(test)]
type BetweenAttempts = Box<dyn Fn() + Send + Sync>;

struct State {
    built: Option<(SourceKind, SdkCredentialsProvider)>,
    log: Vec<SourceKind>,
    session: SessionAuth,
}

/// Guarded credential resolution for one process (R-ABS, R-SRC, R-PREC, D1,
/// R-STICKY). Built from startup settings only: the region, the `--profile`
/// value and the provider-plane client; the environment and profile files are
/// re-read on every attempt, never mutated.
pub struct GuardedCredentials {
    runtime: SdkRuntime,
    region: String,
    cli_profile: Option<String>,
    http: ProviderHttp,
    env: Box<dyn Env + Send + Sync>,
    selector: Selector,
    #[cfg(test)]
    between_attempts: Option<BetweenAttempts>,
    state: Mutex<State>,
}

impl GuardedCredentials {
    /// A resolver over the process environment and profile files.
    #[must_use]
    pub fn new(
        runtime: &SdkRuntime,
        region: &str,
        cli_profile: Option<&str>,
        http: ProviderHttp,
    ) -> Self {
        Self {
            runtime: runtime.clone(),
            region: region.to_string(),
            cli_profile: cli_profile.map(str::to_string),
            http,
            env: Box::new(ProcessEnv),
            selector: select_source,
            #[cfg(test)]
            between_attempts: None,
            state: Mutex::new(State {
                built: None,
                log: Vec::new(),
                session: SessionAuth::new(),
            }),
        }
    }

    /// Replace the R-SRC selector (the mutation check). Test builds only: the
    /// production binary has no such switch.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_selector(mut self, selector: Selector) -> Self {
        self.selector = selector;
        self
    }

    /// Run `hook` between the load and the D1 rebuild. Test builds only.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_between_attempts(mut self, hook: BetweenAttempts) -> Self {
        self.between_attempts = Some(hook);
        self
    }

    /// The providers built so far, in order (never any value).
    #[must_use]
    pub fn build_log(&self) -> Vec<SourceKind> {
        self.lock().log.clone()
    }

    /// The MCP session ended: a new session may start unsigned (R-STICKY).
    pub fn end_session(&self) {
        self.lock().session.reset();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One attempt: preflight, select, build (or reuse) only the selected
    /// provider, load, classify by variant.
    fn attempt(&self, rebuild: bool) -> (AttemptOutcome, Option<Credentials>) {
        let profile = SelectedProfile::resolve(self.cli_profile.as_deref(), self.env.as_ref());
        let view = profile_view(self.env.as_ref(), &profile);
        let preflight = classify(self.env.as_ref(), &profile, &view);
        let kind = match (self.selector)(&preflight) {
            Selection::Refuse(reason) => return (AttemptOutcome::Invalid(reason), None),
            Selection::Build(kind) => kind,
        };
        let mut state = self.lock();
        let reuse = !rebuild
            && state
                .built
                .as_ref()
                .is_some_and(|(built, _)| *built == kind);
        if !reuse {
            let provider = build_source(&self.runtime, kind, &self.region, &profile, &self.http);
            state.log.push(kind);
            state.built = Some((kind, provider));
        }
        let Some((_, provider)) = state.built.as_ref() else {
            return (AttemptOutcome::ProviderFailed, None);
        };
        match provider.load() {
            Ok(credentials) => (AttemptOutcome::Loaded, Some(credentials)),
            Err(AwsError::NoCredentials) if kind == SourceKind::DefaultChain => {
                (AttemptOutcome::AbsentNotLoaded, None)
            }
            Err(AwsError::NoCredentials) => (AttemptOutcome::ConfiguredNotLoaded, None),
            Err(_) => (AttemptOutcome::ProviderFailed, None),
        }
    }

    /// Resolve credentials for one request: the load, then at most one D1
    /// rebuild (after a level-2/3 outcome, or at once when `force_rebuild` is
    /// set after a 401/403), R-PREC across both, then R-STICKY.
    pub fn resolve(&self, skip_auth: bool, force_rebuild: bool) -> RequestCredentials {
        let mut auth = RequestAuth::new();
        let mut rebuild = force_rebuild;
        let mut loaded = None;
        loop {
            let (outcome, credentials) = self.attempt(rebuild);
            auth.record(outcome);
            if credentials.is_some() {
                loaded = credentials;
                break;
            }
            if !auth.should_retry(outcome) {
                break;
            }
            #[cfg(test)]
            if let Some(hook) = &self.between_attempts {
                hook();
            }
            rebuild = true;
        }
        let decision = self.lock().session.apply(auth.decide(skip_auth));
        match (decision, loaded) {
            (AuthDecision::Sign, Some(credentials)) => RequestCredentials::Sign(credentials),
            (AuthDecision::Unsigned, _) => RequestCredentials::Unsigned,
            (AuthDecision::Refuse(refusal), _) => RequestCredentials::Refused(refusal),
            (AuthDecision::Sign, None) => {
                RequestCredentials::Refused(CredentialRefusal::ProviderFailed)
            }
        }
    }
}

impl fmt::Debug for GuardedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuardedCredentials([REDACTED])")
    }
}

#[cfg(test)]
mod tests;
