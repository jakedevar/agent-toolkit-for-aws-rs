//! The injected world of every payment route (design section 7, G1b).
//!
//! A route obtains every side effect through one [`Deps`] value: the runtime
//! config and environment ([`ConfigStore`]), the `AgentCore` client
//! ([`PaymentsConnector`]), the merchant ([`MerchantHttp`]), DNS
//! ([`HostResolver`]) and time ([`PayClock`]). Only a live build has a
//! process world (`Deps::for_process`, `agents-pay-live`). A build without
//! the feature has none: its registry handler prints the D32 refusal literal
//! (`agents_pay::D32_REFUSAL`) before it parses argv or reads any file,
//! variable, credential or terminal. Tests call `run_with` with the
//! [`fixtures`] instead.

use std::time::{Duration, Instant};

use toolkit_aws::endpoint::HostResolver;
use toolkit_core::Json;

use super::agentcore::{FakePaymentsApi, PaymentsApi};
use super::net::{Clock, MerchantHttp};

/// The D32 text: a build without `agents-pay-live` has no payment support.
pub const NO_PAYMENT_SUPPORT: &str =
    "This toolkit build has no payment support; install a release build of toolkit.";

/// The G3 text: a test identity reached a live build.
pub const TEST_FIXTURE_REFUSAL: &str =
    "Payment resources are test fixtures; refusing to contact live services.";

/// Monotonic time for deadlines, wall time for proof validity and session
/// expiry, and the only way the flow waits.
pub trait PayClock: Clock {
    /// Microseconds since the Unix epoch.
    fn unix_micros(&self) -> i64;
    /// Wait (the D13 replay pause, the `validAfter` wait).
    fn sleep(&self, duration: Duration);
}

/// The runtime configuration file and the environment fallback.
pub trait ConfigStore {
    /// The checked runtime config (R-CFG), normalized by `config::load_config`.
    ///
    /// # Errors
    /// The model-facing R-CFG refusal text.
    fn runtime_config(&self) -> Result<Json, String>;
    /// One environment variable (resource fallbacks R-RES-RT, tunables R-TUNE).
    fn env(&self, name: &str) -> Option<String>;
}

/// Opens the `AgentCore` client once the region is known.
pub trait PaymentsConnector {
    /// The profile region, the last step of the runtime region chain.
    fn profile_region(&self) -> Option<String>;
    /// A client for `region`, bounded by the operation deadline (R-AWS-RETRY).
    ///
    /// # Errors
    /// A fixed model-facing text (for example G3's).
    fn open<'s>(
        &'s self,
        region: &str,
        manager_arn: &str,
        operation_deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String>;
}

/// Everything a payment route touches outside its own memory.
#[derive(Clone, Copy)]
pub struct Deps<'a> {
    /// Runtime config and environment.
    pub config: &'a dyn ConfigStore,
    /// The `AgentCore` client factory.
    pub payments: &'a dyn PaymentsConnector,
    /// The merchant plane.
    pub merchant: &'a dyn MerchantHttp,
    /// The R-ORIGIN pre-check resolver.
    pub resolver: &'a dyn HostResolver,
    /// Time.
    pub clock: &'a dyn PayClock,
}

/// No live payment world: a fixed failure text (for example the R-PROOTS
/// one) in a live build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable(pub String);

/// The process world of a live build (G1).
#[cfg(feature = "agents-pay-live")]
pub enum ProcessWorld {
    /// The real constructors (G1).
    Live(Box<super::live::World>),
}

#[cfg(feature = "agents-pay-live")]
impl ProcessWorld {
    /// The world's injected dependencies.
    #[must_use]
    pub fn deps(&self) -> Deps<'_> {
        match *self {
            Self::Live(ref world) => world.deps(),
        }
    }
}

#[cfg(feature = "agents-pay-live")]
impl Deps<'static> {
    /// The live process world. A build without `agents-pay-live` has no
    /// such function: its route refuses with the D32 literal instead.
    ///
    /// # Errors
    /// [`Unavailable`] when the live world cannot be built.
    pub fn for_process() -> Result<ProcessWorld, Unavailable> {
        super::live::World::for_process().map(|world| ProcessWorld::Live(Box::new(world)))
    }
}

/// G3: whether a manager ARN or URL is a test identity (account
/// `111111111111` or `123456789012`; a `.test`, `.example`, `.invalid` or
/// `.localhost` host). A live build refuses to contact services for them.
#[must_use]
pub fn is_test_identity(manager_arn: Option<&str>, url: Option<&str>) -> bool {
    let account = manager_arn
        .filter(|arn| arn.starts_with("arn:"))
        .and_then(|arn| arn.split(':').nth(4));
    if matches!(account, Some("111111111111" | "123456789012")) {
        return true;
    }
    let host = url
        .and_then(|url| super::urlparse::urlparse(url).ok())
        .and_then(|parsed| parsed.hostname());
    host.is_some_and(|host| {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        ["test", "example", "invalid", "localhost"]
            .iter()
            .any(|tld| host == *tld || host.ends_with(&format!(".{tld}")))
    })
}

impl PaymentsConnector for FakePaymentsApi {
    fn profile_region(&self) -> Option<String> {
        None
    }
    fn open<'s>(
        &'s self,
        region: &str,
        _manager_arn: &str,
        _operation_deadline: Instant,
    ) -> Result<Box<dyn PaymentsApi + 's>, String> {
        self.record_open(region);
        Ok(Box::new(self))
    }
}

/// Public, harmless fixtures (G1b, G2): in memory, or inside their own
/// temporary directory. None of them opens a socket.
pub mod fixtures {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use toolkit_aws::transport::Headers;
    use toolkit_core::Json;

    use super::{ConfigStore, PayClock};
    use crate::agents_pay::net::{
        Clock, MerchantHttp, MerchantRequest, MerchantResponse, NetError,
    };

    /// A shared fake clock: `sleep` and `advance` move both its monotonic and
    /// wall time. Clones share the same time.
    #[derive(Clone)]
    pub struct FakeClock(Rc<ClockState>);

    struct ClockState {
        base: Instant,
        unix: i64,
        offset: Cell<Duration>,
        sleeps: RefCell<Vec<Duration>>,
    }

    impl FakeClock {
        /// A clock whose wall time starts at `unix_seconds`.
        #[must_use]
        pub fn at_unix(unix_seconds: i64) -> Self {
            Self(Rc::new(ClockState {
                base: Instant::now(),
                unix: unix_seconds.saturating_mul(1_000_000),
                offset: Cell::new(Duration::ZERO),
                sleeps: RefCell::new(Vec::new()),
            }))
        }
        /// Move time forward without recording a sleep.
        pub fn advance(&self, duration: Duration) {
            self.0.offset.set(self.0.offset.get() + duration);
        }
        /// Every `sleep` so far.
        #[must_use]
        pub fn sleeps(&self) -> Vec<Duration> {
            self.0.sleeps.borrow().clone()
        }
        /// The current monotonic time.
        #[must_use]
        pub fn now_instant(&self) -> Instant {
            self.0.base + self.0.offset.get()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.now_instant()
        }
    }

    impl PayClock for FakeClock {
        fn unix_micros(&self) -> i64 {
            let offset = i64::try_from(self.0.offset.get().as_micros()).unwrap_or(i64::MAX);
            self.0.unix.saturating_add(offset)
        }
        fn sleep(&self, duration: Duration) {
            self.0.sleeps.borrow_mut().push(duration);
            self.advance(duration);
        }
    }

    /// A runtime config in its own temporary directory, read through the
    /// real checked opener (`config::load_config`), plus an injected
    /// environment. The directory is removed on drop.
    pub struct TempConfigStore {
        root: PathBuf,
        env: Vec<(String, String)>,
    }

    impl TempConfigStore {
        fn create_root(label: &str) -> PathBuf {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let root = std::env::temp_dir().join(format!(
                "agents-pay-{label}-{}-{nanos}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(root.join(".agents-pay")).expect("temporary config root");
            set_mode(&root.join(".agents-pay"), 0o700);
            root
        }

        /// A store holding `config` (mode 0600 in a 0700 directory).
        ///
        /// # Panics
        /// If the temporary directory cannot be written.
        #[must_use]
        pub fn new(label: &str, config: &Json) -> Self {
            let root = Self::create_root(label);
            let path = root.join(".agents-pay/config.json");
            std::fs::write(&path, config.to_compact_string()).expect("temporary config");
            set_mode(&path, 0o600);
            Self {
                root,
                env: Vec::new(),
            }
        }

        /// A store with no config file.
        #[must_use]
        pub fn empty(label: &str) -> Self {
            Self {
                root: Self::create_root(label),
                env: Vec::new(),
            }
        }

        /// The same store with one environment variable set.
        #[must_use]
        pub fn with_env(mut self, name: &str, value: &str) -> Self {
            self.env.push((name.to_owned(), value.to_owned()));
            self
        }

        /// The config path inside the temporary root.
        #[must_use]
        pub fn path(&self) -> PathBuf {
            self.root.join(".agents-pay/config.json")
        }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }
    #[cfg(not(unix))]
    fn set_mode(_path: &Path, _mode: u32) {}

    impl Drop for TempConfigStore {
        fn drop(&mut self) {
            if self.root.starts_with(std::env::temp_dir()) {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }

    impl ConfigStore for TempConfigStore {
        fn runtime_config(&self) -> Result<Json, String> {
            let path = self.path();
            if !path.starts_with(&self.root) {
                return Err("TempConfigStore refuses a path outside its root.".to_owned());
            }
            crate::agents_pay::config::load_config(&path)
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env
                .iter()
                .rev()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// One request the fake merchant received.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MerchantCall {
        /// The URL.
        pub url: String,
        /// The method as given.
        pub method: String,
        /// The proof header `(name, value)`, when one was sent.
        pub proof: Option<(String, String)>,
    }

    enum Scripted {
        Respond(u16, Vec<(String, String)>, Vec<u8>),
        Fail(String),
        SettleThenFail(String),
        EchoInBody(u16),
        EchoInContentType(u16),
    }

    /// A scripted merchant. It records every request and keeps a settlement
    /// ledger: a proof header counts as settled once, the first time a 2xx is
    /// answered for it (or a `settle_then_fail` consumes it).
    #[derive(Default)]
    pub struct FakeMerchant {
        script: RefCell<VecDeque<Scripted>>,
        calls: RefCell<Vec<MerchantCall>>,
        settled: RefCell<Vec<String>>,
    }

    impl FakeMerchant {
        /// A merchant with an empty script.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }
        /// Queue a response.
        pub fn respond(&self, status: u16, headers: &[(&str, &str)], body: &[u8]) -> &Self {
            self.script.borrow_mut().push_back(Scripted::Respond(
                status,
                headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                body.to_vec(),
            ));
            self
        }
        /// Queue a transport failure with this refusal text.
        pub fn fail(&self, reason: &str) -> &Self {
            self.script
                .borrow_mut()
                .push_back(Scripted::Fail(reason.to_owned()));
            self
        }
        /// Queue a settlement whose response is lost.
        pub fn settle_then_fail(&self, reason: &str) -> &Self {
            self.script
                .borrow_mut()
                .push_back(Scripted::SettleThenFail(reason.to_owned()));
            self
        }
        /// Queue a response whose body echoes the received proof header.
        pub fn echo_proof_in_body(&self, status: u16) -> &Self {
            self.script
                .borrow_mut()
                .push_back(Scripted::EchoInBody(status));
            self
        }
        /// Queue a response whose content type echoes the proof header.
        pub fn echo_proof_in_content_type(&self, status: u16) -> &Self {
            self.script
                .borrow_mut()
                .push_back(Scripted::EchoInContentType(status));
            self
        }
        /// Every request so far.
        #[must_use]
        pub fn requests(&self) -> Vec<MerchantCall> {
            self.calls.borrow().clone()
        }
        /// Requests that carried a proof header.
        #[must_use]
        pub fn proof_sends(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|call| call.proof.is_some())
                .count()
        }
        /// Distinct proofs settled.
        #[must_use]
        pub fn settlements(&self) -> usize {
            self.settled.borrow().len()
        }
        fn settle(&self, value: Option<&str>) {
            if let Some(value) = value {
                let mut settled = self.settled.borrow_mut();
                if !settled.iter().any(|known| known == value) {
                    settled.push(value.to_owned());
                }
            }
        }
    }

    fn response(
        status: u16,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<MerchantResponse, NetError> {
        let mut out = Headers::new();
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("location") {
                out.insert(name, value)
                    .map_err(|_| NetError("RuntimeError during payment flow.".into()))?;
            }
        }
        Ok(MerchantResponse {
            status,
            headers: out,
            body,
        })
    }

    impl MerchantHttp for FakeMerchant {
        fn request(&self, request: &MerchantRequest<'_>) -> Result<MerchantResponse, NetError> {
            let proof = request
                .proof
                .map(|header| (header.name().to_owned(), header.value().to_owned()));
            let value = proof.as_ref().map(|(_, value)| value.clone());
            self.calls.borrow_mut().push(MerchantCall {
                url: request.url.to_owned(),
                method: request.method.to_owned(),
                proof,
            });
            let next = self.script.borrow_mut().pop_front();
            let (status, headers, body) = match next {
                None => return Err(NetError("RuntimeError during payment flow.".into())),
                Some(Scripted::Fail(reason)) => return Err(NetError(reason)),
                Some(Scripted::SettleThenFail(reason)) => {
                    self.settle(value.as_deref());
                    return Err(NetError(reason));
                }
                Some(Scripted::Respond(status, headers, body)) => (status, headers, body),
                Some(Scripted::EchoInBody(status)) => {
                    let echoed = format!("you sent: {}", value.clone().unwrap_or_default());
                    (status, Vec::new(), echoed.into_bytes())
                }
                Some(Scripted::EchoInContentType(status)) => (
                    status,
                    vec![(
                        "content-type".to_owned(),
                        format!("text/{}", value.clone().unwrap_or_default()),
                    )],
                    b"ok".to_vec(),
                ),
            };
            if (200..300).contains(&status) {
                self.settle(value.as_deref());
            }
            response(status, &headers, body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g3_test_identities_are_recognized() {
        let fixture = "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/x";
        let docs = "arn:aws:bedrock-agentcore:us-east-1:123456789012:payment-manager/x";
        let real = "arn:aws:bedrock-agentcore:us-east-1:444455556666:payment-manager/x";
        assert!(is_test_identity(Some(fixture), None));
        assert!(is_test_identity(Some(docs), None));
        assert!(!is_test_identity(Some(real), None));
        for url in [
            "https://merchant.test/x",
            "https://a.example/x",
            "https://B.INVALID./x",
            "https://localhost/x",
            "https://x.localhost/x",
        ] {
            assert!(is_test_identity(Some(real), Some(url)), "{url}");
        }
        for url in ["https://merchant.example.com/x", "https://testnet.com/x"] {
            assert!(!is_test_identity(Some(real), Some(url)), "{url}");
        }
    }
}
