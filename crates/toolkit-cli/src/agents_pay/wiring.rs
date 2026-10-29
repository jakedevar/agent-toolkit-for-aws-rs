//! Transport wiring for the payment process (`aws-sdk` builds; R-TLS-PAY).
//!
//! The merchant plane and the `AgentCore` data plane both carry the proof,
//! so both trust exactly the environment-independent [`PaymentRoots`]
//! (one allocation, T-PROOTS-USE). Credential-provider HTTP keeps the S1
//! provider roots. These helpers build values only: no I/O, no credentials.

use std::sync::Arc;

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::sdk::roots::ValidatedRoots;
use toolkit_aws::sdk::roots_payment::PaymentRoots;
use toolkit_aws::sdk::source::ProviderHttp;
use toolkit_aws::sdk::{SdkRuntime, SdkTransport};
use toolkit_aws::AwsError;

/// The three planes of one payment process.
pub struct PaymentPlanes {
    /// Merchant probe and paid replay: payment roots, guarded `resolver`.
    pub merchant: SdkTransport,
    /// `ProcessPayment` and `GetPaymentSession`: payment roots.
    pub agentcore: SdkTransport,
    /// Credential-provider HTTP (STS, SSO, IMDS, ECS): S1 provider roots.
    pub provider: ProviderHttp,
}

impl PaymentPlanes {
    /// Build every plane from its roots.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if a TLS context cannot be built.
    pub fn build(
        runtime: &SdkRuntime,
        payment: &PaymentRoots,
        provider: &ValidatedRoots,
        resolver: Arc<dyn HostResolver + Send + Sync>,
    ) -> Result<Self, AwsError> {
        Ok(Self {
            merchant: merchant_transport(runtime, payment, resolver)?,
            agentcore: agentcore_transport(runtime, payment)?,
            provider: ProviderHttp::validated(provider)?,
        })
    }
}

/// The merchant transport: payment roots behind the public-address guard.
///
/// # Errors
/// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
pub fn merchant_transport(
    runtime: &SdkRuntime,
    roots: &PaymentRoots,
    resolver: Arc<dyn HostResolver + Send + Sync>,
) -> Result<SdkTransport, AwsError> {
    SdkTransport::with_resolver(runtime, roots.roots(), resolver)
}

/// The `AgentCore` data-plane transport: payment roots, system DNS behind
/// the public-address guard.
///
/// # Errors
/// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
pub fn agentcore_transport(
    runtime: &SdkRuntime,
    roots: &PaymentRoots,
) -> Result<SdkTransport, AwsError> {
    SdkTransport::new(runtime, roots.roots())
}
