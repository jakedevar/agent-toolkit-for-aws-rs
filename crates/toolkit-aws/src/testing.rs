//! Deterministic test doubles for the AWS boundary.
//!
//! Public so domain slices can test their AWS code offline with the same
//! doubles: no sockets, no DNS, no sleeping, no credential variables, no
//! clock. Every double records what it was asked so tests can assert on it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::IpAddr;
use std::rc::Rc;
use std::time::Duration;

use crate::credentials::{Credentials, CredentialsProvider};
use crate::endpoint::HostResolver;
use crate::error::AwsError;
use crate::redact::{write_scrubbed_debug, Scrubbed};
use crate::retry::Sleeper;
use crate::transport::{HttpRequest, HttpResponse, Signer, SigningScope, Transport};

/// A resolver with fixed answers that records every lookup.
#[derive(Debug, Default)]
pub struct FixedResolver {
    answers: BTreeMap<String, Vec<IpAddr>>,
    fallback: Option<Vec<IpAddr>>,
    lookups: RefCell<Vec<String>>,
}

fn parse_addresses(addresses: &[&str]) -> Vec<IpAddr> {
    addresses
        .iter()
        .map(|text| {
            text.parse::<IpAddr>()
                .unwrap_or_else(|_| panic!("test fixture {text:?} is not an IP address"))
        })
        .collect()
}

impl FixedResolver {
    /// A resolver that fails every lookup until answers are added.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer lookups of `host` with `addresses` (may be empty).
    ///
    /// # Panics
    /// When an address is not a valid IP literal (a broken test fixture).
    #[must_use]
    pub fn answer(mut self, host: &str, addresses: &[&str]) -> Self {
        self.answers
            .insert(host.to_ascii_lowercase(), parse_addresses(addresses));
        self
    }

    /// Answer every host without a specific answer.
    ///
    /// # Panics
    /// When an address is not a valid IP literal (a broken test fixture).
    #[must_use]
    pub fn answer_any(mut self, addresses: &[&str]) -> Self {
        self.fallback = Some(parse_addresses(addresses));
        self
    }

    /// Hosts looked up so far, in order.
    #[must_use]
    pub fn lookups(&self) -> Vec<String> {
        self.lookups.borrow().clone()
    }
}

impl HostResolver for FixedResolver {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        self.lookups.borrow_mut().push(host.to_string());
        self.answers
            .get(host)
            .or(self.fallback.as_ref())
            .cloned()
            .ok_or_else(|| "no such host (test resolver)".to_string())
    }
}

/// A sleeper that records requested sleeps and returns immediately.
#[derive(Debug, Default)]
pub struct RecordingSleeper {
    sleeps: RefCell<Vec<Duration>>,
}

impl RecordingSleeper {
    /// A sleeper with no recorded sleeps.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sleeps requested so far, in order.
    #[must_use]
    pub fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.borrow().clone()
    }
}

impl Sleeper for RecordingSleeper {
    fn sleep(&self, duration: Duration) {
        self.sleeps.borrow_mut().push(duration);
    }
}

/// A credentials provider with a fixed outcome that counts its loads.
///
/// `Debug` never shows the fixture key material.
pub struct FakeProvider {
    name: &'static str,
    outcome: Result<(String, String), AwsError>,
    calls: Rc<Cell<u32>>,
}

impl fmt::Debug for FakeProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            let outcome = match &self.outcome {
                Ok(_) => "Ok([REDACTED])".to_string(),
                Err(error) => format!("Err({error:?})"),
            };
            f.debug_struct("FakeProvider")
                .field("name", &Scrubbed(self.name))
                .field("outcome", &format_args!("{outcome}"))
                .field("calls", &self.calls.get())
                .finish()
        })
    }
}

impl FakeProvider {
    /// Fixture credentials (never real keys).
    #[must_use]
    pub fn with_keys(name: &'static str, access_key_id: &str, secret_access_key: &str) -> Self {
        Self {
            name,
            outcome: Ok((access_key_id.to_string(), secret_access_key.to_string())),
            calls: Rc::new(Cell::new(0)),
        }
    }

    /// A provider with no credentials (`NoCredentials`).
    #[must_use]
    pub fn missing(name: &'static str) -> Self {
        Self::failing(name, AwsError::NoCredentials)
    }

    /// A provider that always fails with `error`.
    #[must_use]
    pub fn failing(name: &'static str, error: AwsError) -> Self {
        Self {
            name,
            outcome: Err(error),
            calls: Rc::new(Cell::new(0)),
        }
    }

    /// Shared counter of `load` calls (clone it before boxing the provider).
    #[must_use]
    pub fn calls(&self) -> Rc<Cell<u32>> {
        Rc::clone(&self.calls)
    }
}

impl CredentialsProvider for FakeProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        self.calls.set(self.calls.get() + 1);
        match &self.outcome {
            Ok((key, secret)) => Credentials::new(key, secret, None, None),
            Err(error) => Err(error.clone()),
        }
    }
}

/// A transport that replays scripted outcomes and records every request.
#[derive(Debug, Default)]
pub struct MockTransport {
    script: RefCell<VecDeque<Result<HttpResponse, AwsError>>>,
    requests: RefCell<Vec<HttpRequest>>,
}

impl MockTransport {
    /// A transport with an empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a response with `status` and a text body.
    #[must_use]
    pub fn respond(self, status: u16, body: &str) -> Self {
        self.push(Ok(HttpResponse::new(status, body.as_bytes())))
    }

    /// Queue a response with one extra header.
    ///
    /// # Panics
    /// When the header is invalid (a broken test fixture).
    #[must_use]
    pub fn respond_with_header(self, status: u16, body: &str, name: &str, value: &str) -> Self {
        let mut response = HttpResponse::new(status, body.as_bytes());
        response
            .headers
            .insert(name, value)
            .expect("test fixture header is valid");
        self.push(Ok(response))
    }

    /// Queue a transport failure.
    #[must_use]
    pub fn fail(self, error: AwsError) -> Self {
        self.push(Err(error))
    }

    /// Queue an outcome.
    #[must_use]
    pub fn push(self, outcome: Result<HttpResponse, AwsError>) -> Self {
        self.script.borrow_mut().push_back(outcome);
        self
    }

    /// Requests received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.borrow().clone()
    }

    /// Scripted outcomes not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.script.borrow().len()
    }
}

impl Transport for MockTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, AwsError> {
        self.requests.borrow_mut().push(request.clone());
        self.script.borrow_mut().pop_front().unwrap_or_else(|| {
            Err(AwsError::invalid_config(
                "mock transport",
                "the scripted responses are exhausted",
            ))
        })
    }
}

/// A signer that stamps `authorization: FAKE-SIGNED <region>/<service>` and
/// counts calls (or fails with a fixed error).
#[derive(Debug, Default)]
pub struct FakeSigner {
    failure: Option<AwsError>,
    signed: Cell<u32>,
}

impl FakeSigner {
    /// A signer that succeeds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A signer that always fails with `error`.
    #[must_use]
    pub fn failing(error: AwsError) -> Self {
        Self {
            failure: Some(error),
            signed: Cell::new(0),
        }
    }

    /// Requests signed so far.
    #[must_use]
    pub fn signed(&self) -> u32 {
        self.signed.get()
    }
}

impl Signer for FakeSigner {
    fn sign(&self, request: &mut HttpRequest, scope: &SigningScope) -> Result<(), AwsError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        self.signed.set(self.signed.get() + 1);
        request.headers.insert(
            "authorization",
            &format!("FAKE-SIGNED {}/{}", scope.region, scope.service),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Method, Timeouts};

    #[test]
    fn fake_provider_debug_never_shows_fixture_keys() {
        let provider = FakeProvider::with_keys("env", "AKIAFAKEFAKEFAKEFAKE", "FAKE-SECRET-VALUE");
        let shown = format!("{provider:?}");
        assert!(!shown.contains("FAKE"), "{shown}");
        assert!(shown.contains("Ok([REDACTED])"));
        let failing = FakeProvider::failing(
            "env",
            AwsError::Connect {
                message: "token=FAKE-TOKEN-VALUE".to_string(),
            },
        );
        assert!(!format!("{failing:?}").contains("FAKE"));
    }

    fn request() -> HttpRequest {
        HttpRequest {
            method: Method::Get,
            url: "https://x.amazonaws.com/".to_string(),
            headers: crate::transport::Headers::new(),
            body: Vec::new(),
            timeouts: Timeouts::default(),
        }
    }

    #[test]
    fn resolver_prefers_specific_answers_then_fallback_then_fails() {
        let resolver = FixedResolver::new()
            .answer("A.example", &["1.2.3.4"])
            .answer_any(&["5.6.7.8"]);
        assert_eq!(
            resolver.resolve("a.example").unwrap(),
            vec!["1.2.3.4".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(
            resolver.resolve("b.example").unwrap(),
            vec!["5.6.7.8".parse::<IpAddr>().unwrap()]
        );
        assert!(FixedResolver::new().resolve("x").is_err());
        assert_eq!(
            resolver.lookups(),
            vec!["a.example".to_string(), "b.example".to_string()]
        );
    }

    #[test]
    fn sleeper_records_without_waiting() {
        let sleeper = RecordingSleeper::new();
        sleeper.sleep(Duration::from_secs(3600));
        sleeper.sleep(Duration::from_millis(5));
        assert_eq!(
            sleeper.sleeps(),
            vec![Duration::from_secs(3600), Duration::from_millis(5)]
        );
    }

    #[test]
    fn transport_replays_the_script_and_records_requests() {
        let transport = MockTransport::new()
            .respond(200, "ok")
            .respond_with_header(500, "", "x-amzn-requestid", "r-1")
            .fail(AwsError::connect("down"));
        assert_eq!(transport.remaining(), 3);
        assert_eq!(transport.send(&request()).unwrap().body_text(), Some("ok"));
        let second = transport.send(&request()).unwrap();
        assert_eq!(
            (second.status, second.headers.get("X-Amzn-RequestId")),
            (500, Some("r-1"))
        );
        assert_eq!(
            transport.send(&request()).unwrap_err().code(),
            "aws_connect"
        );
        let exhausted = transport.send(&request()).unwrap_err();
        assert_eq!(exhausted.code(), "aws_invalid_config");
        assert_eq!(transport.requests().len(), 4);
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn signer_stamps_scope_and_counts_or_fails() {
        let signer = FakeSigner::new();
        let scope = SigningScope {
            region: "eu-west-1".to_string(),
            service: "logs".to_string(),
        };
        let mut stamped = request();
        signer.sign(&mut stamped, &scope).unwrap();
        assert_eq!(
            stamped.headers.get("authorization"),
            Some("FAKE-SIGNED eu-west-1/logs")
        );
        assert_eq!(signer.signed(), 1);
        let failing = FakeSigner::failing(AwsError::NoCredentials);
        let mut unsigned = request();
        assert_eq!(
            failing.sign(&mut unsigned, &scope).unwrap_err(),
            AwsError::NoCredentials
        );
        assert!(unsigned.headers.is_empty());
        assert_eq!(failing.signed(), 0);
    }

    #[test]
    fn fake_provider_counts_loads() {
        let provider = FakeProvider::with_keys("p", "KEY", "secret");
        let calls = provider.calls();
        assert_eq!(provider.name(), "p");
        assert!(provider.load().is_ok());
        assert!(provider.load().is_ok());
        assert_eq!(calls.get(), 2);
        assert_eq!(
            FakeProvider::missing("m").load().unwrap_err(),
            AwsError::NoCredentials
        );
    }

    #[test]
    fn fake_provider_debug_scrubs_its_name() {
        let provider = FakeProvider::missing("token=FAKE-PROVIDER-NAME");
        for shown in [format!("{provider:?}"), format!("{provider:#?}")] {
            assert!(!shown.contains("FAKE-PROVIDER-NAME"), "{shown}");
            assert!(shown.contains("FakeProvider"), "{shown}");
        }
    }
}
