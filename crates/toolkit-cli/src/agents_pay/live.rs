//! The only live constructors of the payment runtime (design section 7, G1).
//!
//! Compiled only with the `agents-pay-live` feature, which no verification
//! build enables (G1, G5; the G1a guards refuse it in every test target).
//! It builds the real pieces: the config opener on the OS-account home, the
//! process environment, `SystemResolver`, `PaymentRoots::from_process()`
//! for the proof-bearing planes, S1 provider roots for credential HTTP, the
//! guarded credential chain, `SigV4` and the system clock. G3 refuses test
//! identities before any service is contacted.
#![cfg(feature = "agents-pay-live")]

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::sdk::roots::ValidatedRoots;
use toolkit_aws::sdk::roots_payment::PaymentRoots;
use toolkit_aws::sdk::source::{GuardedCredentials, ProviderHttp, RequestCredentials};
use toolkit_aws::sdk::{SdkRuntime, SdkTransport, SigV4Signer};
use toolkit_aws::{
    AwsError, Credentials, CredentialsProvider, ProcessEnv, SelectedProfile, SystemResolver,
};
use toolkit_core::Json;

use super::agentcore::{
    PaymentClient, PaymentError, PaymentErrorClass, PaymentProof, PaymentsApi,
    ProcessPaymentRequest,
};
use super::deps::{
    is_test_identity, ConfigStore, Deps, PayClock, PaymentsConnector, Unavailable,
    TEST_FIXTURE_REFUSAL,
};
use super::net::{
    Clock, MerchantHttp, MerchantRequest, MerchantResponse, NetError, StreamingMerchant,
};
use super::wiring::PaymentPlanes;

/// The system clocks.
struct LiveClock;

impl Clock for LiveClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

impl PayClock for LiveClock {
    fn unix_micros(&self) -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_micros()).unwrap_or(i64::MAX)
            })
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The runtime config on the OS-account home (R-CFG) and the process
/// environment (fallbacks and tunables only).
struct LiveConfig;

impl ConfigStore for LiveConfig {
    fn runtime_config(&self) -> Result<Json, String> {
        super::config::load_runtime_config()
    }
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// System DNS that never looks up a test-fixture host (G3).
struct LiveResolver;

impl HostResolver for LiveResolver {
    fn resolve(&self, host: &str) -> Result<Vec<std::net::IpAddr>, String> {
        if is_test_identity(None, Some(&format!("https://{host}/"))) {
            return Err(TEST_FIXTURE_REFUSAL.to_owned());
        }
        SystemResolver.resolve(host)
    }
}

/// The guarded credential chain as a signing provider.
struct Guarded(GuardedCredentials);

impl CredentialsProvider for Guarded {
    fn name(&self) -> &'static str {
        "guarded"
    }
    fn load(&self) -> Result<Credentials, AwsError> {
        match self.0.resolve(false, false) {
            RequestCredentials::Sign(credentials) => Ok(credentials),
            RequestCredentials::Unsigned | RequestCredentials::Refused(_) => {
                Err(AwsError::NoCredentials)
            }
        }
    }
}

/// One opened `AgentCore` client: the data-plane transport, a signer over
/// the guarded chain, the region and the operation deadline.
struct LiveApi<'w> {
    transport: &'w SdkTransport,
    signer: SigV4Signer<Guarded>,
    region: String,
    deadline: Instant,
}

impl LiveApi<'_> {
    fn client(&self) -> Result<PaymentClient<'_>, PaymentError> {
        PaymentClient::new(&self.region, self.transport, &self.signer)
            .map(|client| client.with_operation_deadline(self.deadline))
            .map_err(|_| PaymentError::Transport(PaymentErrorClass::RuntimeError))
    }
}

impl PaymentsApi for LiveApi<'_> {
    fn get_session(&self, manager: &str, session: &str, user: &str) -> Result<Json, PaymentError> {
        self.client()?.get_session(manager, session, user)
    }
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError> {
        self.client()?
            .create_session(manager, user, budget, expiry, token)
    }
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError> {
        self.client()?
            .create_instrument(manager, user, connector, network, email, token)
    }
    fn process_payment(
        &self,
        request: &ProcessPaymentRequest,
    ) -> Result<PaymentProof, PaymentError> {
        PaymentsApi::process_payment(&self.client()?, request)
    }
}

/// The live process world.
pub struct World {
    runtime: SdkRuntime,
    agentcore: SdkTransport,
    provider: ProviderHttp,
    merchant: StreamingMerchant<SdkTransport, LiveResolver, LiveClock>,
}

impl World {
    /// Build the live world: payment roots for the proof-bearing planes,
    /// provider roots for credential HTTP.
    ///
    /// # Errors
    /// [`Unavailable`] with the fixed R-PROOTS or R-ROOTS text, or the
    /// generic text when the runtime cannot start.
    pub fn for_process() -> Result<Self, Unavailable> {
        let payment = PaymentRoots::from_process()
            .map_err(|failure| Unavailable(failure.message().to_owned()))?;
        let provider =
            ValidatedRoots::from_process().map_err(|failure| Unavailable(failure.message()))?;
        let generic = |_| Unavailable("RuntimeError during payment flow.".to_owned());
        let runtime = SdkRuntime::new().map_err(generic)?;
        let resolver: Arc<dyn HostResolver + Send + Sync> = Arc::new(SystemResolver);
        let planes =
            PaymentPlanes::build(&runtime, &payment, &provider, resolver).map_err(generic)?;
        Ok(Self {
            runtime,
            agentcore: planes.agentcore,
            provider: planes.provider,
            merchant: StreamingMerchant {
                transport: planes.merchant,
                resolver: LiveResolver,
                clock: LiveClock,
            },
        })
    }

    /// The injected world of the routes.
    #[must_use]
    pub fn deps(&self) -> Deps<'_> {
        Deps {
            config: &LiveConfig,
            payments: self,
            merchant: self,
            resolver: &LiveResolver,
            clock: &LiveClock,
        }
    }
}

impl MerchantHttp for World {
    fn request(&self, request: &MerchantRequest<'_>) -> Result<MerchantResponse, NetError> {
        if is_test_identity(None, Some(request.url)) {
            return Err(NetError(TEST_FIXTURE_REFUSAL.to_owned()));
        }
        self.merchant.request(request)
    }
}

impl PaymentsConnector for World {
    fn profile_region(&self) -> Option<String> {
        let selected = SelectedProfile::resolve(None, &ProcessEnv);
        toolkit_aws::sdk::profile::profile_region(&ProcessEnv, &selected)
    }
    fn open<'s>(
        &'s self,
        region: &str,
        manager_arn: &str,
        operation_deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String> {
        if is_test_identity(Some(manager_arn), None) {
            return Err(TEST_FIXTURE_REFUSAL.to_owned());
        }
        let credentials =
            GuardedCredentials::new(&self.runtime, region, None, self.provider.clone());
        Ok(Box::new(LiveApi {
            transport: &self.agentcore,
            signer: SigV4Signer::new(Guarded(credentials)),
            region: region.to_owned(),
            deadline: operation_deadline,
        }))
    }
}

// ------------------------------------------------------- admin (x402 S6)

/// The process environment of the admin routes.
struct LiveAdminEnv;

impl super::admin::AdminEnv for LiveAdminEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn names(&self) -> Vec<String> {
        std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .collect()
    }
}

/// The real file system: relative paths are relative to the process
/// working directory, the account home comes from the OS account database.
struct LiveAdminFs;

impl super::admin::AdminFs for LiveAdminFs {
    fn cwd(&self) -> std::path::PathBuf {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    }
    fn account_home(&self) -> Option<std::path::PathBuf> {
        nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .ok()
            .flatten()
            .map(|user| user.dir)
            .filter(|dir| dir.is_absolute())
    }
    fn exists(&self, path: &std::path::Path) -> bool {
        path.exists()
    }
    fn is_dir(&self, path: &std::path::Path) -> bool {
        path.is_dir()
    }
    fn read(&self, path: &std::path::Path) -> Option<String> {
        super::admin::deployed::read_bounded(path)
    }
    fn lstat(&self, path: &std::path::Path) -> Option<super::admin::FileMeta> {
        super::admin::fixtures::lstat(path)
    }
    fn uid(&self) -> u32 {
        super::admin::fixtures::current_uid()
    }
    fn write_config(&self, path: &std::path::Path, content: &str) -> Result<(), String> {
        super::admin::write::atomic_write_0600(path, content)
            .map_err(|_| super::admin::write_failure(path))
    }
    fn load_runtime_policy(&self, path: &std::path::Path) -> Result<Json, String> {
        super::config::load_config(path)
    }
}

/// The controlling terminal on stdin: the admin routes' TTY gate (#5475)
/// and their prompts.
pub struct LiveTerminal;

impl super::admin::Terminal for LiveTerminal {
    fn is_tty(&self) -> bool {
        std::io::IsTerminal::is_terminal(&std::io::stdin())
    }
    fn read_line(&self) -> Option<String> {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) if bytes.is_empty() => return None,
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => {
                    if bytes.len() < 4096 {
                        bytes.push(byte[0]);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// The OS random source.
struct LiveRandom;

impl super::admin::AdminRandom for LiveRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), super::admin::NoRandomness> {
        getrandom::fill(buf).map_err(|_| super::admin::NoRandomness)
    }
}

/// One opened admin client: S1 roots (design: admin keeps R-ROOTS), a
/// signer over the guarded chain and the region.
struct LiveAdminApi<'w> {
    transport: &'w SdkTransport,
    signer: SigV4Signer<Guarded>,
    region: String,
}

impl LiveAdminApi<'_> {
    fn client(&self) -> Result<PaymentClient<'_>, PaymentError> {
        PaymentClient::new(&self.region, self.transport, &self.signer)
            .map_err(|_| PaymentError::Transport(PaymentErrorClass::RuntimeError))
    }
}

impl super::admin::AdminApi for LiveAdminApi<'_> {
    fn sts_check(&self) -> Result<(), PaymentError> {
        self.client()?.sts_check()
    }
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError> {
        self.client()?
            .create_instrument(manager, user, connector, network, email, token)
    }
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError> {
        self.client()?
            .create_session(manager, user, budget, expiry, token)
    }
}

/// The live admin world: the S1 provider roots for the `AgentCore` admin
/// plane, STS and credential HTTP (R-TLS-PAY: admin keeps R-ROOTS).
pub struct AdminWorld {
    runtime: SdkRuntime,
    transport: SdkTransport,
    provider: ProviderHttp,
}

impl AdminWorld {
    /// Build the live admin world. `admin::run_process` calls it only after
    /// a creating route's TTY gate (#5475).
    ///
    /// # Errors
    /// [`Unavailable`] with the fixed R-ROOTS text, or a generic text when
    /// the runtime cannot start.
    pub fn for_process() -> Result<Self, Unavailable> {
        let roots =
            ValidatedRoots::from_process().map_err(|failure| Unavailable(failure.message()))?;
        let generic = |_| Unavailable("RuntimeError during payment administration.".to_owned());
        let runtime = SdkRuntime::new().map_err(generic)?;
        let transport = SdkTransport::new(&runtime, &roots).map_err(generic)?;
        let provider = ProviderHttp::validated(&roots).map_err(generic)?;
        Ok(Self {
            runtime,
            transport,
            provider,
        })
    }
}

impl super::admin::AdminWorldSource for AdminWorld {
    fn deps(&self) -> super::admin::AdminDeps<'_> {
        super::admin::AdminDeps {
            env: &LiveAdminEnv,
            fs: &LiveAdminFs,
            terminal: &LiveTerminal,
            api: self,
            random: &LiveRandom,
        }
    }
}

impl super::admin::AdminConnector for AdminWorld {
    fn open<'s>(
        &'s self,
        region: Option<&str>,
    ) -> Result<Box<dyn super::admin::AdminApi + 's>, String> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let region = region
            .map(str::to_owned)
            .or_else(|| env("AWS_REGION"))
            .or_else(|| env("AWS_DEFAULT_REGION"))
            .or_else(|| {
                let selected = SelectedProfile::resolve(None, &ProcessEnv);
                toolkit_aws::sdk::profile::profile_region(&ProcessEnv, &selected)
            })
            .unwrap_or_else(|| "us-east-1".to_owned());
        let credentials =
            GuardedCredentials::new(&self.runtime, &region, None, self.provider.clone());
        Ok(Box::new(LiveAdminApi {
            transport: &self.transport,
            signer: SigV4Signer::new(Guarded(credentials)),
            region,
        }))
    }
    fn check_manager(&self, manager: &str) -> Result<(), String> {
        if is_test_identity(Some(manager), None) {
            return Err(TEST_FIXTURE_REFUSAL.to_owned());
        }
        Ok(())
    }
}
