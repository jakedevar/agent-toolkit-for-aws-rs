//! Repair 2 of the S0-S3 stack after S4 round 2 (#5105): one regression per
//! finding, in-process over the scripted upstream.
//!
//! - #5136 no proxy lock is held across credential I/O: a session boundary
//!   whose `end_session` waits on a held provider load never stops stdin,
//!   other requests, cancellations, deadlines or shutdown.
//! - #5137 a retired `GET` delivery waiting on stdout backpressure is dropped,
//!   never queued into the next session, and frees the one `GET` worker.
//! - #5138 the cancel worker re-checks its deadline, shutdown and session
//!   after signing, and bounds the transport by the time left.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use toolkit_aws::diag::Diag;
use toolkit_aws::{AwsError, Credentials, HttpRequest, Method};
use toolkit_core::Json;

use super::harness::{
    config, error_message, eventually, id_of, ok_result, request, result, settle, small_limits,
    spec, start_with, tool_call, Gate, Harness, Reply, Seen, Sink, Step, Upstream,
};
use crate::mcp_proxy::{Auth, CredentialSource, Exit, Limits, LimitsSpec, Resolved, SourceAuth};

const INIT: &str =
    r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const PARSE_ERROR: i64 = -32700;

fn init_ok(seen: &Seen, session: &str) -> Reply {
    Reply::json(&result(
        seen.id().expect("an initialize id"),
        r#"{"protocolVersion":"2025-06-18","capabilities":{}}"#,
    ))
    .header("mcp-session-id", session)
}

fn counting_inits(seen: &Seen, inits: &AtomicUsize) -> Reply {
    let n = inits.fetch_add(1, Ordering::SeqCst) + 1;
    init_ok(seen, &format!("s-{n}"))
}

fn timed_out(line: &Json) -> bool {
    line.get("error").is_some() || line.get("result").is_some()
}

// ------------------------------------------------------------ #5136

/// A credential source shaped like S1's `GuardedCredentials`: one state
/// mutex, held across the provider load (which waits on `gate` while
/// `hold` is set) and taken by `end_session`.
#[derive(Clone, Default)]
struct HeldProvider {
    state: Arc<Mutex<()>>,
    hold: Arc<AtomicBool>,
    gate: Gate,
    /// Loads that reached the held provider.
    loading: Arc<AtomicUsize>,
    /// `end_session` calls started.
    ending: Arc<AtomicUsize>,
}

impl HeldProvider {
    fn held(&self) {
        if self.hold.load(Ordering::SeqCst) {
            self.gate.wait();
        }
    }
}

impl CredentialSource for HeldProvider {
    fn resolve(&self, _skip_auth: bool, _force_rebuild: bool) -> Resolved {
        let _state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.hold.load(Ordering::SeqCst) {
            self.loading.fetch_add(1, Ordering::SeqCst);
        }
        self.held();
        Resolved::Sign(Credentials::new("AKIDHELD", "SECRETCANARY-held", None, None).unwrap())
    }

    fn end_session(&self) {
        self.ending.fetch_add(1, Ordering::SeqCst);
        let _state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.held();
    }
}

fn source_auth(source: &HeldProvider) -> Arc<dyn Auth> {
    Arc::new(SourceAuth::new(
        Box::new(source.clone()),
        Box::new(|_, request, _| request.headers.insert("authorization", "TEST signed")),
        toolkit_aws::SigningScope {
            region: "us-east-1".to_string(),
            service: "aws-mcp".to_string(),
        },
        false,
        Diag::new("mcp-proxy", Box::new(Sink::default())),
    ))
}

#[test]
fn credential_io_a_held_session_boundary_never_blocks_stdin_deadlines_or_shutdown() {
    let inits = Arc::new(AtomicUsize::new(0));
    let upstream = {
        let inits = Arc::clone(&inits);
        Upstream::new(move |seen| match seen.rpc_method() {
            Some("initialize") => counting_inits(seen, &inits),
            Some(_) if seen.id().is_some() => Reply::json(&ok_result(seen)),
            _ => Reply::accepted(),
        })
    };
    let source = HeldProvider::default();
    let mut h = start_with(config(), upstream.clone(), source_auth(&source));
    h.send(&request(1, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(1));

    // Request A's provider load is held, inside the credential mutex.
    source.hold.store(true, Ordering::SeqCst);
    h.send(&tool_call(2, "echo"));
    eventually("request A to reach the held provider", || {
        source.loading.load(Ordering::SeqCst) == 1
    });
    // Initialize B: its session boundary waits on that same mutex.
    h.send(&request(3, "initialize", INIT));
    eventually(
        "the session boundary to wait on the credential mutex",
        || source.ending.load(Ordering::SeqCst) == 1,
    );

    // stdin is still handled: a malformed line is answered at once.
    h.send("not json");
    let line = h.next_as("the reader to answer stdin while credential I/O is held");
    assert_eq!(error_message(&line).0, PARSE_ERROR);

    // Other requests are admitted, and a client cancellation applies.
    h.send(&request(5, "ping", "{}"));
    h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":5}}"#);
    settle();

    // Deadlines still fire: A (tool timeout) and B (request timeout) are
    // answered while the provider is held; the cancelled request is not.
    h.clock.advance(Duration::from_secs(301));
    let mut ids = Vec::new();
    for _ in 0..2 {
        let line = h.next_as("a timeout answered while credential I/O is held");
        assert!(timed_out(&line), "{line:?}");
        ids.push(id_of(&line));
    }
    ids.sort_by_key(Json::to_compact_string);
    assert_eq!(ids, vec![Json::Int(2), Json::Int(3)]);
    h.no_more_output();

    // stdout EPIPE still finishes shutdown with the provider held.
    h.stdout.break_pipe();
    h.send("not json");
    assert_eq!(h.join(), Exit::StdoutClosed);
    source.gate.open();
}

// ------------------------------------------------------------ #5137

fn notification(session: &str, n: u32) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"notifications/message","params":{{"from":"{session}","n":{n}}}}}"#
    )
}

fn gets_for(upstream: &Upstream, session: &str) -> usize {
    upstream
        .seen()
        .iter()
        .filter(|s| s.method == Method::Get && s.header("mcp-session-id") == Some(session))
        .count()
}

#[test]
fn get_stream_a_retired_delivery_waiting_on_stdout_is_dropped_and_frees_the_worker() {
    let s1_hold = Gate::new();
    let s2_hold = Gate::new();
    let upstream = {
        let (s1_hold, s2_hold) = (s1_hold.clone(), s2_hold.clone());
        let inits = Arc::new(AtomicUsize::new(0));
        Upstream::new(move |seen| match (seen.method, seen.rpc_method()) {
            (_, Some("initialize")) => counting_inits(seen, &inits),
            (Method::Get, _) if seen.header("mcp-session-id") == Some("s-1") => {
                let events: Vec<String> = (1..=3).map(|n| notification("s-1", n)).collect();
                let events: Vec<&str> = events.iter().map(String::as_str).collect();
                Reply::sse(&events).step(Step::Gate(s1_hold.clone()))
            }
            (Method::Get, _) => Reply::sse(&[])
                .step(Step::Gate(s2_hold.clone()))
                .data(format!("data: {}\n\n", notification("s-2", 1)).as_bytes()),
            (_, Some(_)) if seen.id().is_some() => Reply::json(&ok_result(seen)),
            _ => Reply::accepted(),
        })
    };
    let mut cfg = config();
    cfg.limits = Limits::new(LimitsSpec {
        data_queue_lines: 1,
        ..spec(small_limits())
    })
    .expect("valid limits");
    let mut h = start_with(cfg, upstream.clone(), Arc::new(crate::mcp_proxy::Unsigned));
    h.send(&request(1, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(1));

    // stdout stops: the writer holds event 1, the data queue event 2, and
    // event 3 waits for queue room.
    h.stdout.pause();
    let pulled = upstream.pulled();
    h.send(INITIALIZED);
    eventually("the three s-1 events", || upstream.pulled() >= pulled + 3);
    settle();

    // A new initialize retires s-1 while event 3 waits on stdout.
    h.send(&request(4, "initialize", INIT));
    eventually("the re-initialize", || {
        upstream.seen_rpc("initialize").len() == 2
    });
    // s-2 is committed once a request of it carries its id.
    let mut id = 10;
    loop {
        h.send(&request(id, "ping", "{}"));
        eventually("the ping", || {
            upstream
                .seen_rpc("ping")
                .iter()
                .any(|s| s.id() == Some(&Json::Int(id)))
        });
        let carried = upstream
            .seen_rpc("ping")
            .iter()
            .any(|s| s.id() == Some(&Json::Int(id)) && s.header("mcp-session-id") == Some("s-2"));
        if carried {
            break;
        }
        assert!(id < 14, "s-2 was never committed");
        settle();
        id += 1;
    }
    h.send(INITIALIZED);
    // The pending s-1 delivery is cancelled, so the one GET worker is free
    // for s-2 while stdout is still paused.
    eventually("the s-2 GET while stdout is still paused", || {
        gets_for(&upstream, "s-2") == 1
    });

    h.stdout.resume();
    eventually("the initialize answer", || {
        h.stdout
            .lines()
            .iter()
            .any(|l| l.contains(r#""id":4"#) && l.contains("result"))
    });
    settle();
    let text = h.stdout.text();
    assert!(text.contains(r#""from":"s-1","n":1"#), "{text}");
    assert!(text.contains(r#""from":"s-1","n":2"#), "{text}");
    assert!(
        !text.contains(r#""from":"s-1","n":3"#),
        "a retired GET event was queued into the next session: {text}"
    );
    assert_eq!(gets_for(&upstream, "s-1"), 1, "s-1 is never reopened");
    s1_hold.open();
    s2_hold.open();
    h.finish();
}

// ------------------------------------------------------------ #5138

/// Signs everything at once except the proxy's own `notifications/cancelled`,
/// which waits on `gate`.
#[derive(Default)]
struct DelayedSigner {
    gate: Gate,
    entered: AtomicUsize,
}

impl Auth for DelayedSigner {
    fn sign(&self, request: &mut HttpRequest) -> Result<(), AwsError> {
        if std::str::from_utf8(&request.body).is_ok_and(|b| b.contains("request timed out")) {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.gate.wait();
        }
        request.headers.insert("authorization", "TEST signed")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum During {
    /// The cancel's deadline passes while it is signed.
    Deadline,
    /// A new `initialize` retires its session while it is signed.
    Retired,
    /// The proxy shuts down while it is signed.
    Shutdown,
    /// Only part of its deadline passes while it is signed.
    Partial,
}

struct Cancelling {
    h: Harness,
    upstream: Arc<Upstream>,
    signer: Arc<DelayedSigner>,
    hold: Gate,
}

/// A session, one request that times out, and its cancel held in signing.
fn cancel_held_in_signing() -> Cancelling {
    let hold = Gate::new();
    let upstream = {
        let hold = hold.clone();
        let inits = Arc::new(AtomicUsize::new(0));
        Upstream::new(move |seen| match seen.rpc_method() {
            Some("initialize") => counting_inits(seen, &inits),
            Some("ping") => Reply::sse(&[]).step(Step::Gate(hold.clone())),
            _ => Reply::accepted(),
        })
    };
    let signer = Arc::new(DelayedSigner::default());
    let mut h = start_with(config(), upstream.clone(), signer.clone());
    h.send(&request(1, "initialize", INIT));
    assert_eq!(id_of(&h.next()), Json::Int(1));
    h.send(&request(2, "ping", "{}"));
    eventually("the ping", || upstream.seen_rpc("ping").len() == 1);
    h.clock.advance(Duration::from_secs(180));
    assert_eq!(id_of(&h.next()), Json::Int(2));
    eventually("the cancel to be signing", || {
        signer.entered.load(Ordering::SeqCst) == 1
    });
    Cancelling {
        h,
        upstream,
        signer,
        hold,
    }
}

fn dropped(diag: &Sink) -> usize {
    diag.text().matches("timeout-cancel-dropped").count()
}

#[test]
fn bounded_control_a_cancel_is_checked_again_after_signing() {
    for during in [
        During::Deadline,
        During::Retired,
        During::Shutdown,
        During::Partial,
    ] {
        let Cancelling {
            mut h,
            upstream,
            signer,
            hold,
        } = cancel_held_in_signing();
        let diag = h.diag.clone();
        match during {
            During::Deadline => h.clock.advance(Duration::from_secs(181)),
            During::Partial => h.clock.advance(Duration::from_secs(170)),
            During::Retired => {
                h.send(&request(3, "initialize", INIT));
                assert_eq!(id_of(&h.next()), Json::Int(3));
            }
            During::Shutdown => {
                h.stdout.break_pipe();
                h.send("not json");
            }
        }
        let running = if during == During::Shutdown {
            assert_eq!(h.join(), Exit::StdoutClosed);
            None
        } else {
            Some(h)
        };
        signer.gate.open();
        settle();
        let cancels = upstream.seen_rpc("notifications/cancelled");
        if during == During::Partial {
            eventually("the cancel within its deadline", || {
                upstream.seen_rpc("notifications/cancelled").len() == 1
            });
            let cancel = &upstream.seen_rpc("notifications/cancelled")[0];
            let left = Duration::from_secs(10);
            assert!(
                cancel.timeouts.connect() <= left && cancel.timeouts.read() <= left,
                "the transport outlives the cancel's deadline: {:?}",
                cancel.timeouts
            );
            assert_eq!(dropped(&diag), 0);
        } else {
            assert!(
                cancels.is_empty(),
                "{during:?}: a cancel was sent after signing outlived it"
            );
            assert_eq!(
                dropped(&diag),
                1,
                "{during:?}: the late cancel is dropped with timeout-cancel-dropped: {}",
                diag.text()
            );
        }
        hold.open();
        if let Some(h) = running {
            h.wait_budget_zero();
            h.finish();
        }
    }
}
