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
use std::time::{Duration, Instant, SystemTime};

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
    classify, guard_credential_process, platform_isolates_stdin, select_source, AttemptOutcome,
    AuthDecision, CredentialRefusal, Preflight, ProfileView, RequestAuth, SelectedProfile,
    Selection, SessionAuth, SourceKind,
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

    /// [`Self::validated`] whose DNS goes through the process-wide gated
    /// system resolver ([`super::lookup`]), for bounded (deadline) credential
    /// resolution: a lookup the deadline abandons is capped at one per
    /// process and runs off Tokio's blocking pool. The proxy's
    /// [`Self::validated`] client is unchanged.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn validated_bounded(roots: &ValidatedRoots) -> Result<Self, AwsError> {
        let client = HttpClientBuilder::new()
            .tls_provider(tls::Provider::Rustls(
                tls::rustls_provider::CryptoMode::AwsLc,
            ))
            .tls_context(roots.tls_context()?)
            .build_with_resolver(super::lookup::GatedSystemDns);
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

/// The most provider builds [`GuardedCredentials::build_log`] keeps when
/// recording is enabled (#5112).
pub const BUILD_LOG_CAP: usize = 64;

/// The provider build history (#5112): a test instrument. Off by default,
/// so production storage is constant; when enabled
/// ([`GuardedCredentials::with_build_recording`]) it keeps at most
/// [`BUILD_LOG_CAP`] entries.
#[derive(Debug, Default)]
pub(crate) struct BuildLog {
    enabled: bool,
    entries: Vec<SourceKind>,
}

impl BuildLog {
    /// A log that records.
    pub(crate) fn enabled() -> Self {
        Self {
            enabled: true,
            entries: Vec::new(),
        }
    }

    pub(crate) fn record(&mut self, kind: SourceKind) {
        if self.enabled && self.entries.len() < BUILD_LOG_CAP {
            self.entries.push(kind);
        }
    }

    pub(crate) fn entries(&self) -> &[SourceKind] {
        &self.entries
    }
}

struct State {
    built: Option<(SourceKind, SdkCredentialsProvider)>,
    log: BuildLog,
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
    /// Whether `credential_process` children get a null stdin (D2); when
    /// not, a profile that could run one is refused.
    null_stdin: bool,
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
            null_stdin: platform_isolates_stdin(),
            #[cfg(test)]
            between_attempts: None,
            state: Mutex::new(State {
                built: None,
                log: BuildLog::default(),
                session: SessionAuth::new(),
            }),
        }
    }

    /// Declare whether this process gave `credential_process` children a
    /// null stdin (D2, `isolate_stdin` succeeded). The default is this
    /// target's capability; the proxy passes the real outcome. Without it, a
    /// selected profile that could run a `credential_process` fails closed
    /// ([`crate::preflight::guard_credential_process`]).
    #[must_use]
    pub fn with_null_stdin(mut self, null_stdin: bool) -> Self {
        self.null_stdin = null_stdin;
        self
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

    /// Record provider builds for [`Self::build_log`] (a test instrument,
    /// bounded by [`BUILD_LOG_CAP`]; production never enables it).
    #[must_use]
    pub fn with_build_recording(self) -> Self {
        self.lock().log = BuildLog::enabled();
        self
    }

    /// The providers built so far, in order (never any value); empty unless
    /// [`Self::with_build_recording`] was set.
    #[must_use]
    pub fn build_log(&self) -> Vec<SourceKind> {
        self.lock().log.entries().to_vec()
    }

    /// The MCP session ended: a new session may start unsigned (R-STICKY).
    pub fn end_session(&self) {
        self.lock().session.reset();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One attempt: preflight, select, build (or reuse) only the selected
    /// provider, load, classify by variant. With a `deadline` (the bounded
    /// path), the load stops at it and a source that could run a
    /// `credential_process` is refused before it is built; either is an
    /// `Err` that ends resolution at once (no rebuild, no fallback).
    fn attempt(
        &self,
        rebuild: bool,
        deadline: Option<Instant>,
    ) -> Result<(AttemptOutcome, Option<Credentials>), AwsError> {
        let profile = SelectedProfile::resolve(self.cli_profile.as_deref(), self.env.as_ref());
        let view = profile_view(self.env.as_ref(), &profile);
        let preflight = classify(self.env.as_ref(), &profile, &view);
        let selection =
            guard_credential_process((self.selector)(&preflight), &view, self.null_stdin);
        let kind = match selection {
            Selection::Refuse(reason) => return Ok((AttemptOutcome::Invalid(reason), None)),
            Selection::Build(kind) => kind,
        };
        if deadline.is_some() && runs_unbounded_process(kind, &view) {
            return Err(AwsError::invalid_config(
                "credentials",
                UNBOUNDED_PROCESS_REFUSAL,
            ));
        }
        let mut state = self.lock();
        let reuse = !rebuild
            && state
                .built
                .as_ref()
                .is_some_and(|(built, _)| *built == kind);
        if !reuse {
            let provider = build_source(&self.runtime, kind, &self.region, &profile, &self.http);
            state.log.record(kind);
            state.built = Some((kind, provider));
        }
        let Some((_, provider)) = state.built.as_ref() else {
            return Ok((AttemptOutcome::ProviderFailed, None));
        };
        let loaded = match deadline {
            None => provider.load(),
            Some(deadline) => match provider.load_until(deadline) {
                Err(error @ AwsError::Timeout { .. }) => return Err(error),
                other => other,
            },
        };
        Ok(match loaded {
            Ok(credentials) => (AttemptOutcome::Loaded, Some(credentials)),
            Err(AwsError::NoCredentials) if kind == SourceKind::DefaultChain => {
                (AttemptOutcome::AbsentNotLoaded, None)
            }
            Err(AwsError::NoCredentials) => (AttemptOutcome::ConfiguredNotLoaded, None),
            Err(_) => (AttemptOutcome::ProviderFailed, None),
        })
    }

    /// Resolve credentials for one request: the load, then at most one D1
    /// rebuild (after a level-2/3 outcome, or at once when `force_rebuild` is
    /// set after a 401/403), R-PREC across both, then R-STICKY.
    pub fn resolve(&self, skip_auth: bool, force_rebuild: bool) -> RequestCredentials {
        // Without a deadline no attempt returns `Err`.
        self.resolve_by(skip_auth, force_rebuild, None)
            .unwrap_or(RequestCredentials::Refused(
                CredentialRefusal::ProviderFailed,
            ))
    }

    fn resolve_by(
        &self,
        skip_auth: bool,
        force_rebuild: bool,
        deadline: Option<Instant>,
    ) -> Result<RequestCredentials, AwsError> {
        let mut auth = RequestAuth::new();
        let mut rebuild = force_rebuild;
        let mut loaded = None;
        loop {
            let (outcome, credentials) = self.attempt(rebuild, deadline)?;
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
        Ok(match (decision, loaded) {
            (AuthDecision::Sign, Some(credentials)) => RequestCredentials::Sign(credentials),
            (AuthDecision::Unsigned, _) => RequestCredentials::Unsigned,
            (AuthDecision::Refuse(refusal), _) => RequestCredentials::Refused(refusal),
            (AuthDecision::Sign, None) => {
                RequestCredentials::Refused(CredentialRefusal::ProviderFailed)
            }
        })
    }

    /// [`Self::resolve_required`] under an absolute `deadline` (R-DG-07): the
    /// operation's one monotonic deadline, further capped at
    /// [`CREDENTIAL_BUDGET`] from now. Every provider load (and the D1
    /// rebuild) runs as an async future that is dropped when the bound fires,
    /// so no provider HTTP work outlives it. A source that could run a
    /// `credential_process` is refused before it is built, because aws-config
    /// does not kill that child when its future is dropped. Live signing must
    /// use this (or [`Self::until`]), never the unbounded
    /// [`Self::resolve_required`].
    ///
    /// # Errors
    /// `Timeout` (phase `deadline`) when the bound fires;
    /// `InvalidConfig` (field `credentials`) for a `credential_process`
    /// source; otherwise the fixed classes of [`Self::resolve_required`].
    pub fn resolve_required_until(&self, deadline: Instant) -> Result<Credentials, AwsError> {
        let bound = deadline.min(Instant::now() + CREDENTIAL_BUDGET);
        let mut stopped: Option<AwsError> = None;
        let result = required_credentials(SystemTime::now, |rebuild| {
            if stopped.is_some() {
                return RequestCredentials::Refused(CredentialRefusal::ProviderFailed);
            }
            self.resolve_by(false, rebuild, Some(bound))
                .unwrap_or_else(|error| {
                    stopped = Some(error);
                    RequestCredentials::Refused(CredentialRefusal::ProviderFailed)
                })
        });
        match stopped {
            Some(error) => Err(error),
            None => result,
        }
    }

    /// A [`CredentialsProvider`] view whose every load is
    /// [`Self::resolve_required_until`] `deadline`: hand it to the signer of
    /// one operation together with the same deadline for its transport.
    #[must_use]
    pub fn until(&self, deadline: Instant) -> CredentialsUntil<'_> {
        CredentialsUntil {
            guarded: self,
            deadline,
        }
    }

    /// Resolve credentials for a request that must be signed. Expired
    /// credentials trigger one rebuild before the request is admitted; an
    /// absent or failing source never becomes an unsigned request.
    ///
    /// This method does not impose a deadline on provider work. Callers must
    /// supply a cancellable outer boundary before using it for live traffic.
    ///
    /// # Errors
    /// A fixed credential class when resolution or refresh fails.
    pub fn resolve_required(&self) -> Result<Credentials, AwsError> {
        required_credentials(SystemTime::now, |rebuild| self.resolve(false, rebuild))
    }
}

fn required_credentials(
    mut now: impl FnMut() -> SystemTime,
    mut resolve: impl FnMut(bool) -> RequestCredentials,
) -> Result<Credentials, AwsError> {
    let mut result = resolve(false);
    if matches!(&result, RequestCredentials::Sign(credentials) if credentials.is_expired_at(now()))
    {
        result = resolve(true);
    }
    match result {
        RequestCredentials::Sign(credentials) if !credentials.is_expired_at(now()) => {
            Ok(credentials)
        }
        RequestCredentials::Sign(_) => Err(AwsError::CredentialsExpired),
        RequestCredentials::Unsigned
        | RequestCredentials::Refused(
            CredentialRefusal::NoCredentials | CredentialRefusal::SignedSessionLostCredentials,
        ) => Err(AwsError::NoCredentials),
        RequestCredentials::Refused(CredentialRefusal::InvalidConfiguration(_)) => Err(
            AwsError::invalid_config("credentials", "invalid credential configuration"),
        ),
        RequestCredentials::Refused(
            CredentialRefusal::ProviderFailed | CredentialRefusal::ConfiguredSourceNotLoaded,
        ) => Err(AwsError::CredentialProvider {
            provider: "guarded",
            message: "credential source failed".to_string(),
        }),
    }
}

impl CredentialsProvider for GuardedCredentials {
    fn name(&self) -> &'static str {
        "guarded"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        self.resolve_required()
    }
}

impl fmt::Debug for GuardedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuardedCredentials([REDACTED])")
    }
}

/// The R-DG-07 credential resolution budget: at most this long from the start
/// of a bounded resolution, and never past the operation deadline.
pub const CREDENTIAL_BUDGET: Duration = Duration::from_secs(10);

/// The fixed refusal of a `credential_process` source on the bounded path.
pub const UNBOUNDED_PROCESS_REFUSAL: &str =
    "a credential_process source cannot be stopped at the operation deadline, \
     so it is not used for this command; choose another credential source";

/// The fixed error when bounded credential resolution runs out of time.
pub(crate) fn credential_deadline_error() -> AwsError {
    AwsError::timeout(
        crate::error::TimeoutPhase::Deadline,
        "credential resolution did not finish within its time budget; nothing was sent",
    )
}

/// Whether building `kind` over `view` could start a `credential_process`
/// child. aws-config 1.13.0 runs it with `tokio::process::Command::output`
/// without `kill_on_drop`, so a dropped (timed-out) load would leave the
/// child running: the bounded path refuses such a source instead.
fn runs_unbounded_process(kind: SourceKind, view: &ProfileView) -> bool {
    matches!(kind, SourceKind::Profile | SourceKind::DefaultChain)
        && matches!(view, ProfileView::Loaded(facts) if facts.credential_process_reachable)
}

/// [`GuardedCredentials`] bounded by one operation deadline
/// ([`GuardedCredentials::until`]).
pub struct CredentialsUntil<'a> {
    guarded: &'a GuardedCredentials,
    deadline: Instant,
}

impl CredentialsProvider for CredentialsUntil<'_> {
    fn name(&self) -> &'static str {
        "guarded"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        self.guarded.resolve_required_until(self.deadline)
    }
}

impl fmt::Debug for CredentialsUntil<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialsUntil([REDACTED])")
    }
}

#[cfg(test)]
mod tests;
