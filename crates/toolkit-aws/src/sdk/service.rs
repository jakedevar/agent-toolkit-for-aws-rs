//! The public service-plane transport of the docgraph live review (#5489,
//! #5522; R-DG-07).
//!
//! RED-EVIDENCE SKELETON: delegates to the ordinary public composition
//! (`SdkTransport::with_resolver`), whose synchronous DNS lookup the
//! operation deadline cannot interrupt. The tests below must fail on it.

use std::net::IpAddr;
use std::sync::Arc;

use super::lookup::{self, BlockingLookup, LookupGate};
use super::roots::ValidatedRoots;
use super::{SdkRuntime, SdkTransport};
use crate::endpoint::HostResolver;
use crate::error::AwsError;

/// Which lookup gate a service-plane transport queues on.
#[derive(Clone)]
pub(crate) enum Gate {
    /// The process-wide gate ([`lookup::system_gate`]).
    Process,
    /// An injected gate (tests).
    #[cfg(test)]
    Owned(Arc<LookupGate>),
}

impl Gate {
    fn get(&self) -> &LookupGate {
        match self {
            Self::Process => lookup::system_gate(),
            #[cfg(test)]
            Self::Owned(gate) => gate,
        }
    }
}

struct Blocking(BlockingLookup);

impl HostResolver for Blocking {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        (self.0)(host).map_err(|_| "the host could not be resolved".to_string())
    }
}

impl SdkTransport {
    /// The public service-plane transport.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn for_public_service(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
    ) -> Result<Self, AwsError> {
        Self::public_service(runtime, roots, &Gate::Process, lookup::system_lookup())
    }

    pub(crate) fn public_service(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        gate: &Gate,
        lookup: BlockingLookup,
    ) -> Result<Self, AwsError> {
        let _ = gate.get();
        Self::with_resolver(runtime, roots, Arc::new(Blocking(lookup)))
    }
}

#[cfg(test)]
mod tests;
