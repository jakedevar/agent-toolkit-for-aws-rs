//! `FixedResolver`: a harmless, network-free `HostResolver` for tests and
//! fixtures (x402 S1 #5118; design section 7, G1b).

use std::cell::RefCell;
use std::net::IpAddr;

use toolkit_aws::endpoint::HostResolver;

/// Answers lookups from a fixed table and records every host it was asked
/// for. An unknown host does not resolve. It performs no I/O.
#[derive(Debug, Default)]
pub struct FixedResolver {
    answers: Vec<(String, Vec<IpAddr>)>,
    lookups: RefCell<Vec<String>>,
}

impl FixedResolver {
    /// A resolver that knows no host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same resolver answering `host` with `addresses` (replacing an
    /// earlier answer for it). An empty list is an empty answer.
    #[must_use]
    pub fn with_host(mut self, host: &str, addresses: Vec<IpAddr>) -> Self {
        self.answers.retain(|(known, _)| known != host);
        self.answers.push((host.to_owned(), addresses));
        self
    }

    /// Every host looked up so far, in order.
    #[must_use]
    pub fn lookups(&self) -> Vec<String> {
        self.lookups.borrow().clone()
    }
}

impl HostResolver for FixedResolver {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        self.lookups.borrow_mut().push(host.to_owned());
        self.answers
            .iter()
            .find(|(known, _)| known == host)
            .map(|(_, addresses)| addresses.clone())
            .ok_or_else(|| "no fixture answer for this host".to_owned())
    }
}
