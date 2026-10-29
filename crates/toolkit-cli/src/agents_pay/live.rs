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
