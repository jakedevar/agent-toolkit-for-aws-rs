//! The public service-plane transport of the docgraph live review (#5489,
//! #5522; R-DG-07): the public endpoint guard of [`SdkTransport::new`] with a
//! DNS lookup the operation deadline can abandon.
//!
//! [`SdkTransport::new`] resolves through [`crate::endpoint::HostResolver`],
//! a synchronous call made while the connector's future is polled, so a stuck
//! system lookup holds the caller past any deadline. This construction keeps
//! every request and answer guard of that transport and changes only how the
//! name is resolved:
//!
//! 1. the URL guards run first and unchanged (https only; a literal
//!    non-public host and any framing or authority header refused before a
//!    lookup);
//! 2. the lookup runs behind the process-wide [`LookupGate`] of
//!    [`super::lookup`], the one the database and bounded provider planes
//!    share: on a detached thread, awaited asynchronously, so the operation
//!    deadline drops the wait and returns control;
//! 3. the answers pass the same public-address admission
//!    ([`guarded_addresses`]): an empty answer, a failed lookup or **any**
//!    non-public answer refuses the request before a dial (never filtered);
//! 4. the socket's actual peer is checked again when the response arrives
//!    (the unchanged peer check).
//!
//! Residual (accepted, the same as the database plane's): the blocked
//! `getaddrinfo` thread itself runs until the system resolver returns. While
//! it does, every new lookup on the process gate fails closed at once with
//! [`super::lookup::ABANDONED_LOOKUP_REFUSAL`], so at most one abandoned
//! lookup is outstanding per process.
//!
//! The ordinary public ([`SdkTransport::new`]) and payment transports are not
//! changed by this module.

use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aws_smithy_runtime_api::client::dns::{DnsFuture, ResolveDns, ResolveDnsError};

use super::lookup::{self, BlockingLookup, LookupGate};
use super::roots::ValidatedRoots;
use super::{build_connector, SdkRuntime, SdkTransport};
use crate::binding::guarded_addresses;
use crate::endpoint::HostResolver;
use crate::error::AwsError;
use crate::transport::Timeouts;

/// Which lookup gate a service-plane transport queues on.
#[derive(Clone)]
pub(crate) enum Gate {
    /// The process-wide gate ([`lookup::system_gate`]): production.
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

/// The gated lookup plus the public-address admission, as an SDK resolver.
#[derive(Clone)]
struct GatedPublicResolver {
    gate: Gate,
    lookup: BlockingLookup,
    deadline: Option<Instant>,
}

impl fmt::Debug for GatedPublicResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GatedPublicResolver")
    }
}

/// The answers of a finished lookup, replayed through the public-address
/// policy (the resolver's own failure text is never kept).
struct Answered(Result<Vec<IpAddr>, &'static str>);

impl HostResolver for Answered {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        self.0.clone().map_err(str::to_string)
    }
}

fn deadline_elapsed() -> ResolveDnsError {
    ResolveDnsError::new(std::io::Error::other("DNS deadline elapsed"))
}

impl ResolveDns for GatedPublicResolver {
    fn resolve_dns<'a>(&'a self, name: &'a str) -> DnsFuture<'a> {
        DnsFuture::new(async move {
            let expired = || self.deadline.is_some_and(|d| Instant::now() >= d);
            if expired() {
                return Err(deadline_elapsed());
            }
            let host = name.to_ascii_lowercase();
            let lookup = self.gate.get().lookup(&host, Arc::clone(&self.lookup));
            let answers = match self.deadline {
                Some(deadline) => tokio::time::timeout_at(deadline.into(), lookup)
                    .await
                    .map_err(|_| deadline_elapsed())?,
                None => lookup.await,
            };
            if expired() {
                return Err(deadline_elapsed());
            }
            let answered = Answered(answers.map_err(|error| error.message()));
            guarded_addresses(&host, &answered).map_err(ResolveDnsError::new)
        })
    }
}

impl SdkTransport {
    /// The public service-plane transport (#5522): the public-address guard
    /// of [`SdkTransport::new`] over the operating system resolver behind the
    /// process-wide lookup gate, trusting exactly `roots`. Opened with a
    /// [`crate::stream::StreamLimits`] deadline, DNS, connect, TLS and the
    /// response head all stop at it; see the module note for the residual.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if the TLS context cannot be built.
    pub fn for_public_service(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
    ) -> Result<Self, AwsError> {
        Self::public_service(runtime, roots, &Gate::Process, lookup::system_lookup())
    }

    /// [`Self::for_public_service`] over a given gate and blocking lookup.
    pub(crate) fn public_service(
        runtime: &SdkRuntime,
        roots: &ValidatedRoots,
        gate: &Gate,
        lookup: BlockingLookup,
    ) -> Result<Self, AwsError> {
        let resolver = GatedPublicResolver {
            gate: gate.clone(),
            lookup,
            deadline: None,
        };
        let ctx = roots.tls_context()?;
        let factory_runtime = runtime.clone();
        let factory = move |timeouts: &Timeouts, deadline: Option<Instant>| {
            let mut resolver = resolver.clone();
            resolver.deadline = deadline;
            build_connector(&factory_runtime, timeouts, &ctx, resolver)
        };
        Ok(Self {
            runtime: runtime.clone(),
            factory: Box::new(factory),
            connectors: Mutex::new(Vec::new()),
            roots: Some(roots.clone()),
            peer_check: true,
            release_ended_streams: false,
        })
    }
}

#[cfg(test)]
mod tests;
