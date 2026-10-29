//! System DNS lookups a deadline can abandon, with a process-wide cap on
//! abandoned lookups (R-DG-07; #5366 PM ruling 2).
//!
//! `getaddrinfo` cannot be cancelled. A bounded caller therefore runs it on a
//! dedicated, detached thread and waits for the answer asynchronously, so the
//! caller's deadline drops the wait. The thread itself runs until the system
//! resolver returns: that is the **accepted residual**. It is capped:
//!
//! - one [`LookupGate`] admits one blocking lookup at a time (waiters queue
//!   asynchronously, so their own deadlines still apply);
//! - while an abandoned lookup is still running, a new lookup fails closed at
//!   once with [`ABANDONED_LOOKUP_REFUSAL`] and starts no thread.
//!
//! So at most one abandoned lookup is ever outstanding per gate, and the
//! production gate ([`system_gate`]) is one per process. The thread is a plain
//! `std` thread, not Tokio's blocking pool, so dropping the runtime never
//! waits for it.

use std::net::{IpAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use aws_smithy_runtime_api::client::dns::{DnsFuture, ResolveDns, ResolveDnsError};
use tokio::sync::{oneshot, Semaphore};

/// The fixed refusal while an abandoned lookup is still running.
pub const ABANDONED_LOOKUP_REFUSAL: &str =
    "an earlier DNS lookup is still running after its deadline; refusing to start another";

/// A blocking host lookup (production: the operating system resolver).
pub(crate) type BlockingLookup = Arc<dyn Fn(&str) -> std::io::Result<Vec<IpAddr>> + Send + Sync>;

/// A lookup the gate refused or that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LookupError {
    /// An abandoned lookup is still outstanding (fixed text).
    AbandonedOutstanding,
    /// The lookup thread could not be started.
    NoThread,
    /// The resolver failed (its message is never shown).
    Failed,
}

impl LookupError {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::AbandonedOutstanding => ABANDONED_LOOKUP_REFUSAL,
            Self::NoThread => "the DNS lookup could not be started",
            Self::Failed => "the host could not be resolved",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    Abandoned,
    Done,
}

/// One gate: a single lookup slot and the abandoned-lookup flag.
pub(crate) struct LookupGate {
    slot: Arc<Semaphore>,
    abandoned: Arc<AtomicBool>,
}

/// The process-wide gate of every production system lookup on the bounded
/// (database and bounded provider) paths.
pub(crate) fn system_gate() -> &'static LookupGate {
    static GATE: OnceLock<LookupGate> = OnceLock::new();
    GATE.get_or_init(LookupGate::new)
}

/// The operating system resolver (`getaddrinfo`), as a [`BlockingLookup`].
pub(crate) fn system_lookup() -> BlockingLookup {
    Arc::new(|host: &str| {
        (host, 0_u16)
            .to_socket_addrs()
            .map(|answers| answers.map(|answer| answer.ip()).collect())
    })
}

/// Marks the lookup abandoned when its waiter is dropped first.
struct AbandonOnDrop {
    phase: Arc<Mutex<Phase>>,
    abandoned: Arc<AtomicBool>,
}

impl Drop for AbandonOnDrop {
    fn drop(&mut self) {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if *phase == Phase::Running {
            *phase = Phase::Abandoned;
            self.abandoned.store(true, Ordering::SeqCst);
        }
    }
}

impl LookupGate {
    pub(crate) fn new() -> Self {
        Self {
            slot: Arc::new(Semaphore::new(1)),
            abandoned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether an abandoned lookup is still running.
    pub(crate) fn has_abandoned(&self) -> bool {
        self.abandoned.load(Ordering::SeqCst)
    }

    /// Resolve `host` with `lookup` on a detached thread. Dropping the
    /// returned future (the caller's deadline) abandons the wait; the thread
    /// clears the abandoned flag and frees the slot when the resolver returns.
    pub(crate) async fn lookup(
        &self,
        host: &str,
        lookup: BlockingLookup,
    ) -> Result<Vec<IpAddr>, LookupError> {
        if self.has_abandoned() {
            return Err(LookupError::AbandonedOutstanding);
        }
        let permit = Arc::clone(&self.slot)
            .acquire_owned()
            .await
            .map_err(|_| LookupError::NoThread)?;
        // The slot is held by any abandoned lookup, so this cannot change
        // while the permit is ours; checked again for clarity.
        if self.has_abandoned() {
            return Err(LookupError::AbandonedOutstanding);
        }
        let phase = Arc::new(Mutex::new(Phase::Running));
        let (send, receive) = oneshot::channel();
        let thread_phase = Arc::clone(&phase);
        let thread_abandoned = Arc::clone(&self.abandoned);
        let name = host.to_string();
        let spawned = std::thread::Builder::new()
            .name("toolkit-dns".to_string())
            .spawn(move || {
                let answer = lookup(&name).map_err(|_| LookupError::Failed);
                let mut phase = thread_phase.lock().unwrap_or_else(PoisonError::into_inner);
                if *phase == Phase::Abandoned {
                    thread_abandoned.store(false, Ordering::SeqCst);
                }
                *phase = Phase::Done;
                drop(phase);
                let _ = send.send(answer);
                drop(permit);
            });
        if spawned.is_err() {
            return Err(LookupError::NoThread);
        }
        let _abandon = AbandonOnDrop {
            phase,
            abandoned: Arc::clone(&self.abandoned),
        };
        receive.await.unwrap_or(Err(LookupError::Failed))
    }
}

/// The gated system resolver as an SDK resolver, for the bounded provider
/// plane ([`super::source::ProviderHttp::validated_bounded`]).
#[derive(Clone, Copy, Default)]
pub(crate) struct GatedSystemDns;

impl std::fmt::Debug for GatedSystemDns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GatedSystemDns")
    }
}

impl ResolveDns for GatedSystemDns {
    fn resolve_dns<'a>(&'a self, name: &'a str) -> DnsFuture<'a> {
        DnsFuture::new(async move {
            system_gate()
                .lookup(name, system_lookup())
                .await
                .map_err(|error| ResolveDnsError::new(std::io::Error::other(error.message())))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    /// An injected resolver whose first call blocks until released.
    fn stuck_then_answering(calls: &Arc<AtomicUsize>) -> (BlockingLookup, mpsc::Sender<()>) {
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let calls = Arc::clone(calls);
        let lookup: BlockingLookup = Arc::new(move |_host: &str| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = wait.lock().expect("wait").recv();
            }
            Ok(vec![IpAddr::from([10, 1, 2, 3])])
        });
        (lookup, release)
    }

    fn wait_until_clear(gate: &LookupGate) {
        let start = Instant::now();
        while gate.has_abandoned() {
            assert!(start.elapsed() < Duration::from_secs(5), "never cleared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn an_abandoned_lookup_blocks_new_lookups_until_the_resolver_returns() {
        let rt = runtime();
        let gate = LookupGate::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (lookup, release) = stuck_then_answering(&calls);

        let first = rt.block_on(tokio::time::timeout(
            Duration::from_millis(100),
            gate.lookup("db.example", Arc::clone(&lookup)),
        ));
        assert!(first.is_err(), "the deadline abandons the stuck lookup");
        assert!(gate.has_abandoned());

        let start = Instant::now();
        let second = rt.block_on(tokio::time::timeout(
            Duration::from_secs(2),
            gate.lookup("db.example", Arc::clone(&lookup)),
        ));
        assert_eq!(second, Ok(Err(LookupError::AbandonedOutstanding)));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "fails closed at once"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no second thread started");
        assert_eq!(
            LookupError::AbandonedOutstanding.message(),
            ABANDONED_LOOKUP_REFUSAL
        );

        release.send(()).expect("release");
        wait_until_clear(&gate);
        let third = rt.block_on(gate.lookup("db.example", lookup));
        assert_eq!(third, Ok(vec![IpAddr::from([10, 1, 2, 3])]));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_completed_lookup_leaves_nothing_abandoned_and_failures_hide_the_message() {
        let rt = runtime();
        let gate = LookupGate::new();
        let ok: BlockingLookup = Arc::new(|_: &str| Ok(vec![IpAddr::from([10, 0, 0, 1])]));
        assert_eq!(
            rt.block_on(gate.lookup("a", Arc::clone(&ok))),
            Ok(vec![IpAddr::from([10, 0, 0, 1])])
        );
        assert!(!gate.has_abandoned());
        let failing: BlockingLookup =
            Arc::new(|_: &str| Err(std::io::Error::other("secret-resolver-detail")));
        let error = rt.block_on(gate.lookup("a", failing)).unwrap_err();
        assert_eq!(error, LookupError::Failed);
        assert!(!error.message().contains("secret"));
        assert!(!gate.has_abandoned());
    }

    #[test]
    fn a_live_lookup_makes_others_wait_under_their_own_deadline() {
        let gate = Arc::new(LookupGate::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (lookup, release) = stuck_then_answering(&calls);
        let first = {
            let (gate, lookup) = (Arc::clone(&gate), Arc::clone(&lookup));
            std::thread::spawn(move || runtime().block_on(gate.lookup("db.example", lookup)))
        };
        let start = Instant::now();
        while calls.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(5), "first never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        let waited = runtime().block_on(tokio::time::timeout(
            Duration::from_millis(100),
            gate.lookup("db.example", Arc::clone(&lookup)),
        ));
        assert!(waited.is_err(), "the waiter times out on its own deadline");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one thread at a time");
        assert!(!gate.has_abandoned(), "a waiter is not an abandoned lookup");
        release.send(()).expect("release");
        assert_eq!(
            first.join().expect("first"),
            Ok(vec![IpAddr::from([10, 1, 2, 3])])
        );
        assert!(!gate.has_abandoned());
    }
}
