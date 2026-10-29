//! The protocol loop: threads, the pending-request table and the per-request
//! workers.
//!
//! Threads (all named, so a test can tell where parsing happened):
//!
//! - `mcp-proxy-reader` reads stdin lines, parses and classifies client
//!   messages, applies cancellations, injects `_meta`, enforces the
//!   concurrency cap and starts one worker per message.
//! - `mcp-proxy-writer` is the only stdout writer ([`Output`]).
//! - `mcp-proxy-supervisor` fires request deadlines (tool and request
//!   timeouts) on the fake or real clock.
//! - `mcp-proxy-request-N` sends one client message upstream (retries, D1
//!   resend), reads its response stream, parses and re-serialises each
//!   upstream message and queues it.
//! - `mcp-proxy-get` holds the optional server-initiated `GET` stream.
//!
//! The thread that calls [`Proxy::run`] waits for the first of stdin EOF
//! (after the session `DELETE`) and a stdout failure, then aborts every
//! task: reservations are released as each one notices.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use toolkit_aws::diag::{Diag, DiagEvent, ProxyEvent};
use toolkit_aws::{
    AwsError, FullJitter, Headers, HttpRequest, Method, ResponseStream, RetryMode, RetryPolicy,
    StreamError, StreamLimits, StreamingTransport, Timeouts,
};
use toolkit_core::{parse_json_with_limits, Json};

use super::auth::Auth;
use super::budget::{Budget, BudgetError, Reservation, Wait};
use super::clock::Clock;
use super::frame::{
    classify, compact_len, parse_upstream, recover_id, to_line, ClientMessage, Dropped, Line,
    LineReader,
};
use super::limits::Limits;
use super::meta::inject;
use super::output::{OutLine, Output, PushError, WriterEnd};
use super::session::{valid_protocol_version, valid_session_id, Session};
use super::sse::SseParser;
use super::templates::{self, Failure, INVALID_REQUEST, PARSE_ERROR};

/// The budget charge of one retained read-only approval beyond its name's
/// bytes: the set entry and the string header (#5111).
pub(crate) const APPROVED_NAME_OVERHEAD: usize = 64;

/// What the proxy is configured with (S3 builds it from the command line).
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// The validated endpoint URL (S3's `McpTarget`).
    pub url: String,
    /// Extra `--header` values, signed and sent on every request (their
    /// sensitive marks are kept).
    pub headers: Headers,
    /// `--metadata` pairs injected into `params._meta`.
    pub metadata: Vec<(String, String)>,
    /// Connect and idle-read timeouts.
    pub timeouts: Timeouts,
    /// `--retries` (at most 10): resends after a connect failure, a timeout,
    /// a 5xx or a 429, each freshly signed.
    pub retries: u32,
    /// The bounds.
    pub limits: Limits,
    /// The `User-Agent` (S3's Q3 policy); none by default.
    pub user_agent: Option<String>,
    /// `--read-only`: `tools/list` keeps only tools whose
    /// `annotations.readOnlyHint` is `true`, and a `tools/call` for any other
    /// tool is answered locally and never sent.
    pub read_only: bool,
}

impl ProxyConfig {
    /// Defaults for `url`.
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            headers: Headers::new(),
            metadata: Vec::new(),
            timeouts: Timeouts::default(),
            retries: 0,
            limits: Limits::default(),
            user_agent: None,
            read_only: false,
        }
    }
}

/// Why [`Proxy::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// stdin reached end of input (the session `DELETE` was sent).
    StdinClosed,
    /// stdout could not be written.
    StdoutClosed,
}

/// The proxy: build it, then [`Proxy::run`] it once.
pub struct Proxy {
    config: ProxyConfig,
    transport: Arc<dyn StreamingTransport + Send + Sync>,
    auth: Arc<dyn Auth>,
    clock: Arc<dyn Clock>,
    budget: Budget,
    control: Budget,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy")
            .field("budget", &self.budget.limit())
            .finish_non_exhaustive()
    }
}

impl Proxy {
    /// A proxy over `transport`, signing through `auth`.
    #[must_use]
    pub fn new(
        config: ProxyConfig,
        transport: Arc<dyn StreamingTransport + Send + Sync>,
        auth: Arc<dyn Auth>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let budget = Budget::new(config.limits.budget());
        let control = Budget::new(config.limits.control_reserve());
        Self {
            config,
            transport,
            auth,
            clock,
            budget,
            control,
        }
    }

    /// The shared byte budget (R-MEM).
    #[must_use]
    pub fn budget(&self) -> Budget {
        self.budget.clone()
    }

    /// The control reserve.
    #[must_use]
    pub fn control_budget(&self) -> Budget {
        self.control.clone()
    }

    /// Run until stdin ends or stdout fails. Diagnostics go to `diag` only.
    ///
    /// # Panics
    /// When the operating system refuses to start a thread.
    #[must_use]
    pub fn run(
        self,
        stdin: Box<dyn Read + Send>,
        stdout: Box<dyn Write + Send>,
        diag: Diag,
    ) -> Exit {
        let limits = self.config.limits;
        let retry = RetryPolicy::new(
            self.config.retries.min(toolkit_aws::retry::MAX_RETRIES_CAP),
            RetryMode::Standard,
            Duration::from_secs(1),
            Duration::from_secs(20),
        )
        .unwrap_or_else(|_| RetryPolicy::none());
        let shared = Arc::new(Shared {
            output: Output::new(limits.control_queue_lines, limits.data_queue_lines),
            stream_limits: StreamLimits::new(u64::MAX, limits.max_chunk)
                .expect("the chunk bound is positive"),
            config: self.config,
            transport: self.transport,
            auth: self.auth,
            clock: self.clock,
            budget: self.budget,
            control: self.control,
            diag: Mutex::new(diag),
            pending: Mutex::new(Pending::default()),
            pending_changed: Condvar::new(),
            session: Mutex::new(Session::default()),
            session_changed: Condvar::new(),
            generation: AtomicU64::new(0),
            get_worker: AtomicBool::new(false),
            cancels: Mutex::new(CancelQueue::default()),
            cancels_changed: Condvar::new(),
            shutdown: AtomicBool::new(false),
            exit: Mutex::new(None),
            exit_changed: Condvar::new(),
            jitter: FullJitter::new(),
            retry,
            workers: AtomicUsize::new(0),
            list_seq: AtomicU64::new(0),
            approvals: Mutex::new(Approvals::default()),
            boundary: Mutex::new(0),
        });

        let writer = {
            let shared = Arc::clone(&shared);
            spawn("mcp-proxy-writer", move || {
                let mut stdout = stdout;
                if shared.output.run_writer(&mut *stdout) == WriterEnd::Failed {
                    shared.emit(ProxyEvent::StdoutClosed);
                    shared.finish(Exit::StdoutClosed);
                }
            })
        };
        let supervisor = {
            let shared = Arc::clone(&shared);
            spawn("mcp-proxy-supervisor", move || shared.supervise())
        };
        {
            let shared = Arc::clone(&shared);
            spawn("mcp-proxy-reader", move || shared.read_stdin(stdin));
        }

        let exit = shared.wait_exit();
        shared.shutdown();
        let _ = supervisor.join();
        let _ = writer;
        exit
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    try_spawn(name, body).expect("the proxy can start a thread")
}

/// Start a thread; an error (the operating system refused) is the caller's
/// to degrade on.
fn try_spawn(
    name: &str,
    body: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One message's work: its cancellation and its hard deadline.
#[derive(Debug)]
struct Task {
    aborted: AtomicBool,
    /// After this (clock time) the worker stops reading silently; the
    /// supervisor has already answered its requests.
    deadline: Duration,
}

impl Task {
    fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

/// One in-flight client request.
#[derive(Debug)]
struct Entry {
    id: Json,
    tool: Option<String>,
    deadline: Duration,
    timeout_seconds: u64,
    task: Arc<Task>,
    finished: bool,
    /// The session generation the request was dispatched in.
    generation: u64,
    /// This request is a `tools/list` (its result is filtered in read-only
    /// mode).
    listing: Option<Listing>,
}

/// A `tools/list` request, for the read-only approvals (#5110).
#[derive(Debug, Clone, Copy)]
struct Listing {
    /// Its dispatch order among listings: an older listing never replaces a
    /// newer snapshot.
    seq: u64,
    /// No `params.cursor`: a complete fresh listing starts a new snapshot;
    /// a continuation page extends the current one.
    fresh: bool,
}

#[derive(Debug, Default)]
struct Pending {
    entries: HashMap<String, Entry>,
    /// Concurrency slots held: one per request in a message, or one for a
    /// message without requests (#5107).
    slots: usize,
}

/// `--read-only` approvals of the current session (#5110, #5111): the names
/// a listing marked `readOnlyHint: true`, charged to the budget.
#[derive(Debug, Default)]
struct Approvals {
    /// The session generation they belong to.
    generation: u64,
    /// The listing that started the current snapshot.
    snapshot_seq: u64,
    names: HashSet<String>,
    /// `Σ (name bytes + APPROVED_NAME_OVERHEAD)`, released on replacement.
    hold: Option<Reservation>,
}

/// The proxy's own `notifications/cancelled`, queued for the one control
/// worker (#5107).
struct Cancel {
    body: Vec<u8>,
    _hold: Reservation,
    deadline: Duration,
    generation: u64,
}

#[derive(Default)]
struct CancelQueue {
    items: VecDeque<Cancel>,
    /// The control worker is running.
    worker: bool,
}

/// What a `tools/list` result would approve, applied only after its
/// response is claimed (#5110).
struct ListUpdate {
    key: String,
    listing: Listing,
    generation: u64,
    names: Vec<String>,
}

struct Shared {
    config: ProxyConfig,
    transport: Arc<dyn StreamingTransport + Send + Sync>,
    auth: Arc<dyn Auth>,
    clock: Arc<dyn Clock>,
    budget: Budget,
    control: Budget,
    output: Output,
    stream_limits: StreamLimits,
    diag: Mutex<Diag>,
    pending: Mutex<Pending>,
    pending_changed: Condvar,
    session: Mutex<Session>,
    session_changed: Condvar,
    /// `session.generation`, readable without the lock.
    generation: AtomicU64,
    /// The one `GET` worker is running (#5108).
    get_worker: AtomicBool,
    cancels: Mutex<CancelQueue>,
    cancels_changed: Condvar,
    shutdown: AtomicBool,
    exit: Mutex<Option<Exit>>,
    exit_changed: Condvar,
    jitter: FullJitter,
    retry: RetryPolicy,
    workers: AtomicUsize,
    /// Dispatch order of `tools/list` requests.
    list_seq: AtomicU64,
    approvals: Mutex<Approvals>,
    /// The last generation whose R-STICKY boundary was applied. Held only
    /// by an initialize worker around [`Auth::initialize_started`], never
    /// together with another lock of the proxy (#5136).
    boundary: Mutex<u64>,
}

/// How a worker step ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Answer the task's open requests with this failure.
    Fail(Failure),
    /// Stop silently: cancelled, timed out (already answered) or shutdown.
    Abort,
}

impl From<BudgetError> for Stop {
    fn from(error: BudgetError) -> Self {
        match error {
            BudgetError::Exhausted => Self::Fail(Failure::Budget),
            BudgetError::Cancelled | BudgetError::Closed => Self::Abort,
        }
    }
}

/// What a worker sends.
struct Job {
    body: Vec<u8>,
    body_hold: Reservation,
    /// Pending-table keys of the requests in this message.
    keys: Vec<String>,
    initialize: bool,
    initialized: bool,
    /// The session generation (set at dispatch).
    generation: u64,
    /// Concurrency slots held (set at dispatch).
    slots: usize,
}

/// Per-response context while delivering upstream messages.
struct Delivery<'a> {
    task: &'a Task,
    keys: &'a [String],
    /// The session generation of the request (or stream).
    generation: u64,
    /// A `GET` stream: its messages are dropped once its generation is
    /// retired (#5108).
    stream_owned: bool,
    /// This answers `initialize`: its session is committed with the result.
    initialize: bool,
    /// For `initialize`: the validated session id from the response head.
    session_id: Option<String>,
    /// Deliver only a JSON-RPC error answering one of `keys` (the body of
    /// an HTTP error response).
    errors_only: bool,
}

fn key_of(id: &Json) -> String {
    id.to_compact_string()
}

impl Shared {
    // ------------------------------------------------------------ basics

    fn emit(&self, event: ProxyEvent) {
        lock(&self.diag).emit(DiagEvent::Proxy(event));
    }

    fn emit_event(&self, event: DiagEvent) {
        lock(&self.diag).emit(event);
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    fn finish(&self, exit: Exit) {
        let mut slot = lock(&self.exit);
        if slot.is_none() {
            *slot = Some(exit);
        }
        drop(slot);
        self.exit_changed.notify_all();
    }

    fn wait_exit(&self) -> Exit {
        let mut slot = lock(&self.exit);
        loop {
            if let Some(exit) = *slot {
                return exit;
            }
            slot = self
                .exit_changed
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Abort everything and release what can be released.
    fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let mut pending = lock(&self.pending);
            for entry in pending.entries.values_mut() {
                entry.finished = true;
                entry.task.aborted.store(true, Ordering::SeqCst);
            }
        }
        self.budget.close();
        self.control.close();
        self.output.close();
        lock(&self.cancels).items.clear();
        self.pending_changed.notify_all();
        self.session_changed.notify_all();
        self.cancels_changed.notify_all();
    }

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// `initialize` is dispatched: retire the current session and start the
    /// next generation, atomically (#5106). The new generation has no
    /// session id, protocol version or read-only approvals; work of the old
    /// one is never (re)sent, its `GET` stream stops and its late listings
    /// are ignored. Only bookkeeping happens under the locks: the credential
    /// side of the boundary (R-STICKY) runs later in the initialize worker,
    /// with no proxy lock held ([`Self::session_boundary`], #5136).
    fn retire_session(&self) -> u64 {
        let (generation, old_approvals) = {
            let mut session = lock(&self.session);
            let generation = session.generation + 1;
            *session = Session {
                generation,
                ..Session::default()
            };
            self.generation.store(generation, Ordering::SeqCst);
            let old = std::mem::replace(
                &mut *lock(&self.approvals),
                Approvals {
                    generation,
                    ..Approvals::default()
                },
            );
            (generation, old)
        };
        drop(old_approvals); // releases their budget charge, outside the locks
        self.session_changed.notify_all();
        self.wake_waiters();
        generation
    }

    /// R-STICKY's credential boundary for the session `generation` opens:
    /// [`Auth::initialize_started`], which may wait on credential I/O (the
    /// guarded source's state mutex is held across a provider load). It runs
    /// in the initialize worker before its request is signed, with no proxy
    /// lock held, so stdin, deadlines, cancels and shutdown never wait on
    /// it (#5136). The boundary lock only orders these calls: a boundary for
    /// a generation already retired, or older than the last one applied, is
    /// skipped, so a slow earlier boundary can never reset the signing state
    /// of a later session.
    fn session_boundary(&self, generation: u64) {
        let mut applied = lock(&self.boundary);
        if *applied >= generation || self.current_generation() != generation {
            return;
        }
        self.auth.initialize_started();
        *applied = generation;
    }

    fn wake_waiters(&self) {
        self.budget.wake();
        self.output.wake();
        self.pending_changed.notify_all();
    }

    fn wait_for<'a>(
        &'a self,
        cancelled: &'a dyn Fn() -> bool,
        deadline: Option<Duration>,
    ) -> Wait<'a> {
        Wait {
            clock: &*self.clock,
            deadline,
            cancelled,
        }
    }

    /// Queue a proxy-generated line on the control lane.
    fn send_control(&self, text: &str) {
        let shutdown = || self.is_shutdown();
        let wait = self.wait_for(&shutdown, None);
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\n');
        let Ok(hold) = self.control.reserve(bytes.len(), &wait) else {
            return;
        };
        let _ = self.output.push_control(OutLine { bytes, hold });
    }

    // ------------------------------------------------------- pending table

    /// Answer request `key` with `failure`, unless it is already answered.
    fn fail_entry(&self, key: &str, failure: Failure) {
        let answer = {
            let mut pending = lock(&self.pending);
            match pending.entries.get_mut(key) {
                Some(entry) if !entry.finished => {
                    entry.finished = true;
                    let failure = match failure {
                        Failure::TimedOut { .. } => Failure::TimedOut {
                            seconds: entry.timeout_seconds,
                        },
                        other => other,
                    };
                    Some(templates::failure(
                        &entry.id,
                        entry.tool.as_deref(),
                        failure,
                    ))
                }
                _ => None,
            }
        };
        if let Some(line) = answer {
            self.send_control(&line);
        }
    }

    /// Claim the right to deliver the response to `key`.
    fn claim(&self, key: &str) -> bool {
        let mut pending = lock(&self.pending);
        match pending.entries.get_mut(key) {
            Some(entry) if !entry.finished => {
                entry.finished = true;
                true
            }
            _ => false,
        }
    }

    /// The client cancelled `key`: answer nothing, abort its task when no
    /// other request of the task is open.
    fn cancel_local(&self, key: &str) {
        let mut pending = lock(&self.pending);
        let Some(entry) = pending.entries.get_mut(key) else {
            return;
        };
        if entry.finished {
            return;
        }
        entry.finished = true;
        let task = Arc::clone(&entry.task);
        let open = pending
            .entries
            .values()
            .any(|e| Arc::ptr_eq(&e.task, &task) && !e.finished);
        drop(pending);
        if !open {
            task.aborted.store(true, Ordering::SeqCst);
            self.wake_waiters();
        }
    }

    fn release_slot(&self, keys: &[String], slots: usize) {
        let mut pending = lock(&self.pending);
        for key in keys {
            pending.entries.remove(key);
        }
        pending.slots = pending.slots.saturating_sub(slots);
        drop(pending);
        self.pending_changed.notify_all();
    }

    // -------------------------------------------------------- supervisor

    fn supervise(self: &Arc<Self>) {
        let mut pending = lock(&self.pending);
        loop {
            if self.is_shutdown() {
                return;
            }
            let now = self.clock.now();
            let expired: Vec<String> = pending
                .entries
                .iter()
                .filter(|(_, e)| !e.finished && e.deadline <= now)
                .map(|(key, _)| key.clone())
                .collect();
            if !expired.is_empty() {
                let ids: Vec<(Json, u64)> = expired
                    .iter()
                    .filter_map(|key| {
                        pending
                            .entries
                            .get(key)
                            .map(|e| (e.id.clone(), e.generation))
                    })
                    .collect();
                drop(pending);
                for key in &expired {
                    self.fail_entry(key, Failure::TimedOut { seconds: 0 });
                }
                for (id, generation) in ids {
                    self.forward_cancel(id, generation);
                }
                pending = lock(&self.pending);
                for key in &expired {
                    if let Some(task) = pending.entries.get(key).map(|e| Arc::clone(&e.task)) {
                        let open = pending
                            .entries
                            .values()
                            .any(|e| Arc::ptr_eq(&e.task, &task) && !e.finished);
                        if !open {
                            task.aborted.store(true, Ordering::SeqCst);
                        }
                    }
                }
                drop(pending);
                self.wake_waiters();
                pending = lock(&self.pending);
                continue;
            }
            let next = pending
                .entries
                .values()
                .filter(|e| !e.finished)
                .map(|e| e.deadline)
                .min();
            let remaining = next.map_or(Duration::from_secs(3600), |d| d.saturating_sub(now));
            let slice = self
                .clock
                .wait_slice(remaining.max(Duration::from_millis(1)));
            pending = self
                .pending_changed
                .wait_timeout(pending, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    // ------------------------------------------------------------ reader

    fn read_stdin(self: &Arc<Self>, stdin: Box<dyn Read + Send>) {
        let mut lines = LineReader::new(stdin, self.config.limits.max_line);
        loop {
            if self.is_shutdown() {
                return;
            }
            let shutdown = || self.is_shutdown();
            let wait = self.wait_for(&shutdown, None);
            let next = match lines.next(self.budget.empty(), &wait) {
                Ok(Ok(line)) => line,
                Ok(Err(_)) => return,
                Err(_) => Line::Eof,
            };
            match next {
                Line::Eof => break,
                Line::Oversize(prefix) => match recover_id(&prefix) {
                    Some(id) => {
                        self.emit(ProxyEvent::StdinLineTooLarge { id_recovered: true });
                        self.send_control(&templates::rpc_error(
                            &id,
                            INVALID_REQUEST,
                            "Invalid Request: message too large",
                        ));
                    }
                    None => self.emit(ProxyEvent::StdinLineTooLarge {
                        id_recovered: false,
                    }),
                },
                Line::Complete(bytes, hold) => self.handle_line(bytes, hold),
            }
        }
        if !self.is_shutdown() {
            self.end_session();
        }
        self.finish(Exit::StdinClosed);
    }

    fn refuse_line(&self, id: &Json, code: i64, message: &str) {
        self.emit(ProxyEvent::StdinLineInvalid);
        self.send_control(&templates::rpc_error(id, code, message));
    }

    fn handle_line(self: &Arc<Self>, bytes: Vec<u8>, hold: Reservation) {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            self.refuse_line(&Json::Null, PARSE_ERROR, "Parse error");
            return;
        };
        let limits = &self.config.limits;
        let shutdown = || self.is_shutdown();
        let wait = self.wait_for(&shutdown, None);
        let Ok(charge) = self.budget.reserve(limits.parse_charge(bytes.len()), &wait) else {
            return;
        };
        let Ok(mut value) = parse_json_with_limits(text, &limits.json()) else {
            self.refuse_line(&Json::Null, PARSE_ERROR, "Parse error");
            return;
        };
        let messages = match classify(&value) {
            Ok(messages) => messages,
            Err(id) => {
                self.refuse_line(
                    &id.unwrap_or(Json::Null),
                    INVALID_REQUEST,
                    "Invalid Request",
                );
                return;
            }
        };
        let batch = matches!(value, Json::Array(_));

        // Local effects of client cancellations, before forwarding them.
        for cancelled in cancelled_ids(&value) {
            self.cancel_local(&key_of(&cancelled));
        }

        let requests: Vec<(Json, Option<String>, String)> = messages
            .iter()
            .filter_map(|m| match m {
                ClientMessage::Request { id, tool, method } => Some((
                    id.clone(),
                    if batch { None } else { tool.clone() },
                    method.clone(),
                )),
                _ => None,
            })
            .collect();
        let mut keys: Vec<String> = requests.iter().map(|(id, _, _)| key_of(id)).collect();
        {
            let mut sorted = keys.clone();
            sorted.sort();
            sorted.dedup();
            let pending = lock(&self.pending);
            let duplicate =
                sorted.len() != keys.len() || keys.iter().any(|k| pending.entries.contains_key(k));
            drop(pending);
            if duplicate {
                self.refuse_line(
                    &Json::Null,
                    INVALID_REQUEST,
                    "Invalid Request: duplicate request id",
                );
                return;
            }
        }

        if self.config.read_only && self.refuse_read_only(&messages) {
            return;
        }
        let paged = paged_listings(&value);

        // `_meta` injection: the body is the client's bytes unless it changed.
        let injected = inject(&mut value, &self.config.metadata);
        if injected.kept > 0 {
            self.emit(ProxyEvent::MetaKeysKept {
                count: injected.kept,
            });
        }
        if injected.not_object {
            self.emit(ProxyEvent::MetaNotObject);
        }
        let (body, body_hold) = if injected.changed {
            drop(hold);
            let Ok(out_hold) = self.budget.reserve(compact_len(&value), &wait) else {
                return;
            };
            (to_line(&value).into_bytes(), out_hold)
        } else {
            (bytes, hold)
        };
        drop(value);
        drop(charge);

        let job = Job {
            body,
            body_hold,
            keys: std::mem::take(&mut keys),
            initialize: !batch && requests.first().is_some_and(|(_, _, m)| m == "initialize"),
            initialized: !batch
                && matches!(
                    messages.first(),
                    Some(ClientMessage::Notification { method }) if method == "notifications/initialized"
                ),
            generation: 0,
            slots: 0,
        };
        self.dispatch(&requests, &paged, job);
    }

    /// The concurrency cap, then registration and the worker. Each request
    /// of a message takes a slot (a message without requests takes one): a
    /// message whose requests do not fit is refused whole, so a batch can
    /// never hold more than the cap (#5107); other messages wait for a slot.
    fn dispatch(
        self: &Arc<Self>,
        requests: &[(Json, Option<String>, String)],
        paged: &[String],
        mut job: Job,
    ) {
        let limits = &self.config.limits;
        let needed = requests.len().max(1);
        let mut pending = lock(&self.pending);
        loop {
            if self.is_shutdown() {
                return;
            }
            if pending.slots + needed <= limits.max_in_flight {
                break;
            }
            if !requests.is_empty() {
                drop(pending);
                for (id, tool, _) in requests {
                    self.send_control(&templates::failure(
                        id,
                        tool.as_deref(),
                        Failure::TooManyRequests,
                    ));
                }
                return;
            }
            let slice = self.clock.wait_slice(Duration::from_millis(50));
            pending = self
                .pending_changed
                .wait_timeout(pending, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        pending.slots += needed;
        job.slots = needed;
        job.generation = if job.initialize {
            self.retire_session()
        } else {
            self.current_generation()
        };
        let now = self.clock.now();
        let mut task_deadline = now + limits.request_timeout;
        let mut entries = Vec::new();
        for (id, tool, method) in requests {
            let timeout = if tool.is_some() {
                limits.tool_timeout
            } else {
                limits.request_timeout
            };
            task_deadline = task_deadline.max(now + timeout);
            entries.push((
                id.clone(),
                tool.clone(),
                now + timeout,
                timeout.as_secs(),
                (method == "tools/list").then(|| Listing {
                    seq: self.list_seq.fetch_add(1, Ordering::SeqCst) + 1,
                    fresh: !paged.contains(&key_of(id)),
                }),
            ));
        }
        let task = Arc::new(Task {
            aborted: AtomicBool::new(false),
            deadline: task_deadline,
        });
        let generation = job.generation;
        for (id, tool, deadline, timeout_seconds, listing) in entries {
            pending.entries.insert(
                key_of(&id),
                Entry {
                    id,
                    tool,
                    deadline,
                    timeout_seconds,
                    task: Arc::clone(&task),
                    finished: false,
                    generation,
                    listing,
                },
            );
        }
        drop(pending);
        self.pending_changed.notify_all();
        let shared = Arc::clone(self);
        let n = self.workers.fetch_add(1, Ordering::SeqCst);
        spawn(&format!("mcp-proxy-request-{n}"), move || {
            shared.work(&task, job);
        });
    }

    // ----------------------------------------------------------- requests

    /// The request for `generation`, with its session headers; refused
    /// (the session expired) when that generation was retired (#5106).
    fn build_request(
        &self,
        method: Method,
        body: Vec<u8>,
        generation: u64,
    ) -> Result<HttpRequest, Stop> {
        let header = |_| Stop::Fail(Failure::Connect);
        let mut headers = Headers::new();
        for (name, value) in self.config.headers.entries() {
            if self.config.headers.is_sensitive(name) {
                headers.insert_sensitive(name, value).map_err(header)?;
            } else {
                headers.insert(name, value).map_err(header)?;
            }
        }
        match method {
            Method::Get => headers.insert("accept", "text/event-stream"),
            _ => headers.insert("accept", "application/json, text/event-stream"),
        }
        .map_err(header)?;
        if method == Method::Post {
            headers
                .insert("content-type", "application/json")
                .map_err(header)?;
        }
        {
            let session = lock(&self.session);
            if session.generation != generation {
                return Err(Stop::Fail(Failure::SessionExpired));
            }
            if let Some(id) = &session.id {
                headers.insert("mcp-session-id", id).map_err(header)?;
            }
            if let Some(version) = &session.protocol_version {
                headers
                    .insert("mcp-protocol-version", version)
                    .map_err(header)?;
            }
        }
        if let Some(agent) = &self.config.user_agent {
            headers.insert("user-agent", agent).map_err(header)?;
        }
        Ok(HttpRequest {
            method,
            url: self.config.url.clone(),
            headers,
            body,
            timeouts: self.config.timeouts,
        })
    }

    fn backoff(&self, retry: u32) {
        self.clock.sleep(self.retry.delay(retry, &self.jitter));
    }

    /// Send one MCP request of `generation`: each attempt is built and
    /// signed afresh; after signing, the generation is checked again, so a
    /// request of a retired session is never sent (R-STICKY across sessions,
    /// #5106). Connect failures, timeouts, 5xx and 429 are retried up to
    /// `retries`; a 401/403 gets D1's one rebuild and resend (every method,
    /// #5113), a second 401/403 fails with the credentials text.
    fn open_signed<'a>(
        &'a self,
        method: Method,
        body: &[u8],
        generation: u64,
        task: &Task,
        retries: u32,
    ) -> Result<Box<dyn ResponseStream + 'a>, Stop> {
        let mut attempt = 0;
        let mut refreshed = false;
        loop {
            if task.aborted() || self.is_shutdown() {
                return Err(Stop::Abort);
            }
            let mut request = self.build_request(method, body.to_vec(), generation)?;
            if let Err(error) = self.auth.sign(&mut request) {
                self.emit_event(DiagEvent::TransportFailure { code: error.code() });
                return Err(Stop::Fail(Failure::Credentials));
            }
            if self.current_generation() != generation {
                return Err(Stop::Fail(Failure::SessionExpired));
            }
            match self.transport.open(&request, &self.stream_limits) {
                Err(error) => {
                    if error.is_retryable() && attempt < retries {
                        attempt += 1;
                        self.backoff(attempt);
                        continue;
                    }
                    self.emit_event(DiagEvent::TransportFailure { code: error.code() });
                    return Err(Stop::Fail(failure_of(&error)));
                }
                Ok(stream) => {
                    let status = stream.status();
                    if matches!(status, 401 | 403) {
                        drop(stream);
                        if !refreshed && self.auth.refresh() {
                            refreshed = true;
                            continue;
                        }
                        self.emit_event(DiagEvent::HttpStatus { status });
                        return Err(Stop::Fail(Failure::Credentials));
                    }
                    if (status == 429 || (500..600).contains(&status)) && attempt < retries {
                        drop(stream);
                        attempt += 1;
                        self.backoff(attempt);
                        continue;
                    }
                    return Ok(stream);
                }
            }
        }
    }

    fn work(self: &Arc<Self>, task: &Arc<Task>, job: Job) {
        let keys = job.keys.clone();
        let slots = job.slots;
        let outcome = self.exchange(task, job);
        match outcome {
            Ok(()) => {
                for key in &keys {
                    self.fail_entry(key, Failure::NoResponse);
                }
            }
            Err(Stop::Fail(failure)) => {
                if failure == Failure::Budget {
                    self.emit(ProxyEvent::BudgetExhausted);
                }
                if keys.is_empty() {
                    self.emit_event(DiagEvent::Proxy(ProxyEvent::NotificationFailed));
                }
                for key in &keys {
                    self.fail_entry(key, failure);
                }
            }
            Err(Stop::Abort) => {}
        }
        self.release_slot(&keys, slots);
    }

    fn exchange(self: &Arc<Self>, task: &Task, job: Job) -> Result<(), Stop> {
        let Job {
            body,
            body_hold,
            keys,
            initialize,
            initialized,
            generation,
            slots: _,
        } = job;
        let keys = keys.as_slice();
        if initialize {
            self.session_boundary(generation);
        }
        let mut stream = self.open_signed(
            Method::Post,
            &body,
            generation,
            task,
            self.retry.max_retries(),
        )?;
        drop(body);
        drop(body_hold);
        let status = stream.status();
        let has_session = {
            let session = lock(&self.session);
            session.generation == generation && session.id.is_some()
        };
        if status == 404 && has_session {
            self.emit_event(DiagEvent::HttpStatus { status });
            return Err(Stop::Fail(Failure::SessionExpired));
        }
        if !(200..300).contains(&status) {
            self.emit_event(DiagEvent::HttpStatus { status });
            // A JSON-RPC error body for one of these requests is forwarded.
            let delivery = Delivery {
                task,
                keys,
                generation,
                stream_owned: false,
                initialize: false,
                session_id: None,
                errors_only: true,
            };
            if is_json(&*stream) {
                if let Ok((bytes, hold)) = self.read_body(&mut *stream, task) {
                    if self.deliver(&delivery, bytes, hold)? {
                        return Ok(());
                    }
                }
            }
            return Err(Stop::Fail(Failure::Http(status)));
        }
        let mut delivery = Delivery {
            task,
            keys,
            generation,
            stream_owned: false,
            initialize: false,
            session_id: None,
            errors_only: false,
        };
        if initialize {
            let session_id = stream.headers().get("mcp-session-id").map(str::to_string);
            if let Some(id) = &session_id {
                if !valid_session_id(id) {
                    self.emit(ProxyEvent::SessionIdRefused);
                    return Err(Stop::Fail(Failure::SessionRefused));
                }
            }
            delivery.initialize = true;
            delivery.session_id = session_id;
        }
        if is_json(&*stream) {
            let (bytes, hold) = self.read_body(&mut *stream, task)?;
            self.deliver(&delivery, bytes, hold)?;
        } else if is_event_stream(&*stream) {
            self.read_events(&mut *stream, &delivery)?;
        }
        if initialized {
            self.start_get_stream(generation);
        }
        Ok(())
    }

    fn budget_wait<'a>(&'a self, cancelled: &'a dyn Fn() -> bool) -> Wait<'a> {
        let deadline = self.clock.now() + self.config.timeouts.read();
        self.wait_for(cancelled, Some(deadline))
    }

    /// Read a whole `application/json` body (at most one message).
    fn read_body(
        &self,
        stream: &mut dyn ResponseStream,
        task: &Task,
    ) -> Result<(Vec<u8>, Reservation), Stop> {
        let max = self.config.limits.max_message;
        let cancelled = || task.aborted() || self.is_shutdown();
        let mut hold = self.budget.empty();
        let mut body = Vec::new();
        loop {
            if cancelled() || self.clock.now() >= task.deadline {
                return Err(Stop::Abort);
            }
            match stream.next_chunk() {
                Ok(None) => return Ok((body, hold)),
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > max {
                        self.emit(ProxyEvent::UpstreamResponseTooLarge);
                        return Err(Stop::Fail(Failure::TooLarge));
                    }
                    hold.grow(chunk.len(), &self.budget_wait(&cancelled))?;
                    body.extend_from_slice(&chunk);
                }
                Err(error) => return Err(stream_stop(&error)),
            }
        }
    }

    /// Read an SSE body, delivering each event.
    fn read_events(
        &self,
        stream: &mut dyn ResponseStream,
        delivery: &Delivery<'_>,
    ) -> Result<(), Stop> {
        let limits = &self.config.limits;
        let task = delivery.task;
        let cancelled = || task.aborted() || self.is_shutdown() || self.retired(delivery);
        let mut parser = SseParser::new(limits.max_sse_line, limits.max_message);
        let mut hold = self.budget.empty();
        loop {
            if cancelled() || self.clock.now() >= task.deadline {
                return Err(Stop::Abort);
            }
            let chunk = match stream.next_chunk() {
                Ok(None) => return Ok(()),
                Ok(Some(chunk)) => chunk,
                Err(error) => return Err(stream_stop(&error)),
            };
            hold.grow(chunk.len(), &self.budget_wait(&cancelled))?;
            let mut events = Vec::new();
            if parser.feed(&chunk, &mut events).is_err() {
                self.emit(ProxyEvent::UpstreamResponseTooLarge);
                return Err(Stop::Fail(Failure::TooLarge));
            }
            drop(chunk);
            for event in events {
                let event_hold = hold.split(event.len());
                self.deliver(delivery, event, event_hold)?;
            }
            hold.shrink_to(parser.buffered());
        }
    }

    /// A `GET` stream's generation was retired: its messages are dropped
    /// and it stops (#5108).
    fn retired(&self, delivery: &Delivery<'_>) -> bool {
        delivery.stream_owned && self.current_generation() != delivery.generation
    }

    /// Parse, check, re-serialise and queue one upstream message; `true` when
    /// it was queued.
    fn deliver(
        &self,
        delivery: &Delivery<'_>,
        bytes: Vec<u8>,
        raw_hold: Reservation,
    ) -> Result<bool, Stop> {
        let task = delivery.task;
        // A retired `GET` delivery is cancelled wherever it waits, budget or
        // stdout room included, so it is never queued into the next session
        // and frees the `GET` worker (#5137). POST responses are not
        // stream-owned: an old request's answer is still delivered.
        let cancelled = || task.aborted() || self.is_shutdown() || self.retired(delivery);
        let limits = &self.config.limits;
        let charge = self.budget.reserve(
            limits.parse_charge(bytes.len()),
            &self.budget_wait(&cancelled),
        )?;
        let parsed = parse_upstream(&bytes, limits);
        drop(bytes);
        drop(raw_hold);
        let mut value = match parsed {
            Ok(value) => value,
            Err(Dropped::Malformed) => {
                self.emit(ProxyEvent::UpstreamMessageMalformed);
                return Ok(false);
            }
            Err(Dropped::NotRpc) => {
                self.emit(ProxyEvent::UpstreamMessageNotRpc);
                return Ok(false);
            }
        };
        let updates = if self.config.read_only {
            self.filter_tool_lists(&mut value, delivery.keys)
        } else {
            Vec::new()
        };
        let line_hold = self
            .budget
            .reserve(compact_len(&value) + 1, &self.budget_wait(&cancelled))?;
        let mut text = to_line(&value);
        text.push('\n');

        let answered: Vec<String> = response_ids(&value)
            .iter()
            .map(key_of)
            .filter(|key| delivery.keys.contains(key))
            .collect();
        if delivery.errors_only && (answered.is_empty() || value.get("error").is_none()) {
            return Ok(false);
        }
        if let (true, [key]) = (delivery.initialize, answered.as_slice()) {
            if !self.commit_session(delivery.generation, delivery.session_id.clone(), &value) {
                self.fail_entry(key, Failure::SessionRefused);
                return Ok(false);
            }
        }
        drop(value);
        drop(charge);
        if !answered.is_empty() {
            let claimed: Vec<&String> = answered.iter().filter(|key| self.claim(key)).collect();
            if claimed.is_empty() {
                return Ok(false); // already answered: timed out or cancelled
            }
            // Approvals change only for a listing this delivery answers.
            for update in updates {
                if claimed.contains(&&update.key) {
                    self.apply_listing(update);
                }
            }
        }
        if task.aborted() || self.retired(delivery) {
            return Err(Stop::Abort);
        }
        match self.output.push_data(
            OutLine {
                bytes: text.into_bytes(),
                hold: line_hold,
            },
            &cancelled,
        ) {
            Ok(()) => Ok(true),
            Err(PushError::Cancelled | PushError::Closed) => Err(Stop::Abort),
        }
    }

    // ------------------------------------------------------------ session

    /// Store the session from an `initialize` response of `generation`;
    /// `false` refuses it (an invalid version, or a retired generation).
    fn commit_session(&self, generation: u64, session_id: Option<String>, response: &Json) -> bool {
        let Some(result) = response.get("result") else {
            return true; // an error response starts no session
        };
        let version = match result.get("protocolVersion") {
            None => None,
            Some(Json::String(version)) if valid_protocol_version(version) => Some(version.clone()),
            Some(_) => {
                self.emit(ProxyEvent::ProtocolVersionRefused);
                return false;
            }
        };
        let started = session_id.is_some();
        {
            let mut session = lock(&self.session);
            if session.generation != generation {
                return false; // a later initialize retired this one
            }
            *session = Session {
                generation,
                id: session_id,
                protocol_version: version,
                get_started: false,
                get_ended: false,
            };
        }
        if started {
            self.auth.session_started();
        }
        true
    }

    /// `notifications/initialized` of `generation` was sent: the `GET`
    /// stream is wanted for it. One worker serves every generation in turn,
    /// so at most one `GET` stream is ever open (#5108).
    fn start_get_stream(self: &Arc<Self>, generation: u64) {
        {
            let mut session = lock(&self.session);
            if session.generation != generation || session.id.is_none() || session.get_started {
                return;
            }
            session.get_started = true;
        }
        self.session_changed.notify_all();
        if !self.get_worker.swap(true, Ordering::SeqCst) {
            let shared = Arc::clone(self);
            if try_spawn("mcp-proxy-get", move || shared.get_loop()).is_err() {
                self.get_worker.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Sleep on the clock until `at`; `false` on shutdown or when
    /// `generation` is retired.
    fn sleep_until(&self, at: Duration, generation: u64) -> bool {
        let mut pending = lock(&self.pending);
        loop {
            if self.is_shutdown() || self.current_generation() != generation {
                return false;
            }
            let now = self.clock.now();
            if now >= at {
                return true;
            }
            let slice = self.clock.wait_slice(at.saturating_sub(now));
            pending = self
                .pending_changed
                .wait_timeout(pending, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The one `GET` worker: waits for a generation that wants a stream and
    /// serves it until it is retired, then serves the next.
    fn get_loop(self: &Arc<Self>) {
        loop {
            let generation = {
                let mut session = lock(&self.session);
                loop {
                    if self.is_shutdown() {
                        return;
                    }
                    if session.get_started && !session.get_ended && session.id.is_some() {
                        break session.generation;
                    }
                    let slice = self.clock.wait_slice(Duration::from_secs(1));
                    session = self
                        .session_changed
                        .wait_timeout(session, slice)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            };
            if !self.serve_get(generation) {
                let mut session = lock(&self.session);
                if session.generation == generation {
                    session.get_ended = true;
                }
            }
        }
    }

    /// The server-initiated stream of `generation`: reopened at most once
    /// per interval while the generation is current. `false` when it ended
    /// for good (405: no stream offered, or the request cannot be built).
    fn serve_get(&self, generation: u64) -> bool {
        let task = Task {
            aborted: AtomicBool::new(false),
            deadline: Duration::MAX,
        };
        let mut last_open: Option<Duration> = None;
        loop {
            if let Some(at) = last_open {
                if !self.sleep_until(at + self.config.limits.get_reopen_interval, generation) {
                    return true;
                }
            }
            if self.is_shutdown() || self.current_generation() != generation {
                return true;
            }
            last_open = Some(self.clock.now());
            let mut stream = match self.open_signed(Method::Get, &[], generation, &task, 0) {
                Ok(stream) => stream,
                Err(Stop::Fail(Failure::SessionExpired) | Stop::Abort) => return true,
                Err(Stop::Fail(_)) => continue, // reported; reopen later
            };
            let status = stream.status();
            if status == 405 {
                return false;
            }
            if !(200..300).contains(&status) {
                self.emit_event(DiagEvent::HttpStatus { status });
                continue;
            }
            if is_event_stream(&*stream) {
                let delivery = Delivery {
                    task: &task,
                    keys: &[],
                    generation,
                    stream_owned: true,
                    initialize: false,
                    session_id: None,
                    errors_only: false,
                };
                if let Err(Stop::Fail(failure)) = self.read_events(&mut *stream, &delivery) {
                    if failure == Failure::Budget {
                        self.emit(ProxyEvent::BudgetExhausted);
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------- read-only

    /// `--read-only`: answer locally (never forward) a line holding a
    /// `tools/call` for a tool the current session's approvals do not hold
    /// (before any listing, every tool), and any `tools/call` without an id
    /// (it cannot be answered, so it is dropped, #5109). In a batch, the
    /// other requests are refused too, so nothing of it is sent. `true` when
    /// the line was refused.
    fn refuse_read_only(&self, messages: &[ClientMessage]) -> bool {
        let lines: Vec<String> = {
            let approvals = lock(&self.approvals);
            let refused = |message: &ClientMessage| match message {
                ClientMessage::Request { method, tool, .. } => {
                    method == "tools/call"
                        && tool
                            .as_ref()
                            .is_none_or(|name| !approvals.names.contains(name))
                }
                ClientMessage::Notification { method } => method == "tools/call",
                ClientMessage::Response => false,
            };
            if !messages.iter().any(refused) {
                return false;
            }
            messages
                .iter()
                .filter_map(|message| match message {
                    ClientMessage::Request { id, tool, .. } if refused(message) => Some(
                        templates::read_only_refusal(id, tool.as_deref().unwrap_or("")),
                    ),
                    ClientMessage::Request { id, .. } => Some(templates::rpc_error(
                        id,
                        INVALID_REQUEST,
                        "Invalid Request: batch refused in read-only mode",
                    )),
                    _ => None,
                })
                .collect()
        };
        self.emit(ProxyEvent::ReadOnlyToolRefused);
        for line in lines {
            self.send_control(&line);
        }
        true
    }

    /// `--read-only`: keep only `readOnlyHint: true` tools in the results of
    /// this message's `tools/list` requests. The approvals they would make
    /// are returned, to be applied once the response is claimed.
    fn filter_tool_lists(&self, value: &mut Json, keys: &[String]) -> Vec<ListUpdate> {
        let lists: Vec<(String, Listing, u64)> = {
            let pending = lock(&self.pending);
            keys.iter()
                .filter_map(|key| {
                    let entry = pending.entries.get(key)?;
                    entry.listing.map(|l| (key.clone(), l, entry.generation))
                })
                .collect()
        };
        if lists.is_empty() {
            return Vec::new();
        }
        let mut updates = Vec::new();
        let mut filter = |item: &mut Json| {
            let Some((key, listing, generation)) = item
                .get("id")
                .map(key_of)
                .and_then(|key| lists.iter().find(|(k, _, _)| *k == key))
                .cloned()
            else {
                return;
            };
            let Json::Object(members) = item else {
                return;
            };
            let Some((_, Json::Object(result))) = members.iter_mut().find(|(k, _)| k == "result")
            else {
                return;
            };
            let Some((_, Json::Array(tools))) = result.iter_mut().find(|(k, _)| k == "tools")
            else {
                return;
            };
            let mut names = Vec::new();
            tools.retain(|tool| {
                let read_only = tool
                    .get("annotations")
                    .and_then(|annotations| annotations.get("readOnlyHint"))
                    == Some(&Json::Bool(true));
                if read_only {
                    if let Some(name) = tool.get("name").and_then(Json::as_str) {
                        names.push(name.to_string());
                    }
                }
                read_only
            });
            updates.push(ListUpdate {
                key,
                listing,
                generation,
                names,
            });
        };
        match value {
            Json::Array(items) => items.iter_mut().for_each(&mut filter),
            other => filter(other),
        }
        updates
    }

    /// Apply a claimed listing's approvals (#5110, #5111): only for the
    /// current session and only when no newer fresh listing replaced the
    /// snapshot; a fresh listing replaces the set, a page extends it. The
    /// retained names are charged to the budget and capped at one message;
    /// past the cap, or when the budget cannot hold them, every approval is
    /// revoked (fail closed).
    fn apply_listing(&self, update: ListUpdate) {
        let mut approvals = lock(&self.approvals);
        if approvals.generation != update.generation || update.listing.seq <= approvals.snapshot_seq
        {
            return;
        }
        let mut names = if update.listing.fresh {
            approvals.snapshot_seq = update.listing.seq;
            HashSet::new()
        } else {
            std::mem::take(&mut approvals.names)
        };
        names.extend(update.names);
        approvals.names = HashSet::new();
        approvals.hold = None;
        let bytes: usize = names
            .iter()
            .map(|name| name.len() + APPROVED_NAME_OVERHEAD)
            .sum();
        let hold = (bytes <= self.config.limits.max_message)
            .then(|| self.budget.try_reserve(bytes))
            .flatten();
        let Some(hold) = hold else {
            approvals.snapshot_seq = update.listing.seq;
            drop(approvals);
            self.emit(ProxyEvent::ReadOnlyCatalogueRefused);
            return;
        };
        approvals.names = names;
        approvals.hold = Some(hold);
    }

    // ------------------------------------------------------- cancellation

    /// The proxy's own deadline fired for request `id`: queue one
    /// `notifications/cancelled` for the control worker (#5107). The queue
    /// holds at most the request cap, each body is charged to the budget,
    /// and the supervisor never blocks or panics here: a full queue, an
    /// exhausted budget or a worker that cannot start drops the cancel with
    /// `timeout-cancel-dropped`.
    fn forward_cancel(self: &Arc<Self>, id: Json, generation: u64) {
        let body = to_line(&Json::object(vec![
            ("jsonrpc", Json::string("2.0")),
            ("method", Json::string("notifications/cancelled")),
            (
                "params",
                Json::object(vec![
                    ("requestId", id),
                    ("reason", Json::string("request timed out")),
                ]),
            ),
        ]))
        .into_bytes();
        let Some(hold) = self.budget.try_reserve(body.len()) else {
            self.emit(ProxyEvent::TimeoutCancelDropped);
            return;
        };
        let mut queue = lock(&self.cancels);
        if self.is_shutdown() || queue.items.len() >= self.config.limits.max_in_flight {
            drop(queue);
            self.emit(ProxyEvent::TimeoutCancelDropped);
            return;
        }
        queue.items.push_back(Cancel {
            body,
            _hold: hold,
            deadline: self.clock.now() + self.config.limits.request_timeout,
            generation,
        });
        if !queue.worker {
            let shared = Arc::clone(self);
            if try_spawn("mcp-proxy-cancel", move || shared.cancel_loop()).is_ok() {
                queue.worker = true;
            } else {
                queue.items.pop_back();
                drop(queue);
                self.emit(ProxyEvent::TimeoutCancelDropped);
                return;
            }
        }
        drop(queue);
        self.cancels_changed.notify_all();
    }

    /// The control worker: sends queued cancels one at a time, best effort
    /// (signed once, no D1 refresh, response unread). The deadline is
    /// absolute (#5138): a cancel past it, of a retired session, refused by
    /// the credentials or met by shutdown, is dropped with
    /// `timeout-cancel-dropped`, checked when it is dequeued and again
    /// after signing (which may wait on credential I/O); the transport's
    /// connect and read timeouts are bounded by the time left.
    fn cancel_loop(&self) {
        loop {
            let cancel = {
                let mut queue = lock(&self.cancels);
                loop {
                    if self.is_shutdown() {
                        queue.items.clear();
                        queue.worker = false;
                        return;
                    }
                    if let Some(cancel) = queue.items.pop_front() {
                        break cancel;
                    }
                    let slice = self.clock.wait_slice(Duration::from_secs(1));
                    queue = self
                        .cancels_changed
                        .wait_timeout(queue, slice)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            };
            if self.clock.now() >= cancel.deadline {
                self.emit(ProxyEvent::TimeoutCancelDropped);
                continue;
            }
            let Ok(mut request) = self.build_request(Method::Post, cancel.body, cancel.generation)
            else {
                self.emit(ProxyEvent::TimeoutCancelDropped);
                continue;
            };
            if self.auth.sign(&mut request).is_err() {
                self.emit(ProxyEvent::TimeoutCancelDropped);
                continue;
            }
            let Some(timeouts) = self.cancel_timeouts(cancel.deadline, cancel.generation) else {
                self.emit(ProxyEvent::TimeoutCancelDropped);
                continue;
            };
            request.timeouts = timeouts;
            match self.transport.open(&request, &self.stream_limits) {
                Ok(_stream) => self.emit(ProxyEvent::TimeoutCancelForwarded),
                Err(error) => self.emit_event(DiagEvent::TransportFailure { code: error.code() }),
            }
        }
    }

    /// A signed cancel may still be sent: the proxy is running, its session
    /// is current and its deadline has not passed. The transport timeouts
    /// for it, each at most the time left; `None` drops it.
    fn cancel_timeouts(&self, deadline: Duration, generation: u64) -> Option<Timeouts> {
        if self.is_shutdown() || self.current_generation() != generation {
            return None;
        }
        let left = deadline.checked_sub(self.clock.now())?;
        if left.is_zero() {
            return None;
        }
        let configured = self.config.timeouts;
        Timeouts::new(configured.connect().min(left), configured.read().min(left)).ok()
    }

    /// stdin EOF: end the session with one `DELETE` (D1: one rebuild and
    /// resend after a 401/403, #5113).
    fn end_session(&self) {
        let generation = {
            let session = lock(&self.session);
            if session.id.is_none() {
                return;
            }
            session.generation
        };
        let task = Task {
            aborted: AtomicBool::new(false),
            deadline: Duration::MAX,
        };
        // Failures are reported by `open_signed`.
        if let Ok(stream) = self.open_signed(Method::Delete, &[], generation, &task, 0) {
            let status = stream.status();
            if !(200..300).contains(&status) && status != 405 {
                self.emit_event(DiagEvent::HttpStatus { status });
            }
        }
    }
}

fn failure_of(error: &AwsError) -> Failure {
    match error {
        AwsError::Timeout { .. } => Failure::Unresponsive,
        _ => Failure::Connect,
    }
}

fn stream_stop(error: &StreamError) -> Stop {
    match error {
        StreamError::TooLarge => Stop::Fail(Failure::TooLarge),
        StreamError::IdleTimeout | StreamError::DeadlineExceeded => {
            Stop::Fail(Failure::Unresponsive)
        }
        StreamError::Failed(error) => Stop::Fail(failure_of(error)),
    }
}

fn content_type(stream: &dyn ResponseStream) -> String {
    stream
        .headers()
        .get("content-type")
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_json(stream: &dyn ResponseStream) -> bool {
    content_type(stream) == "application/json"
}

fn is_event_stream(stream: &dyn ResponseStream) -> bool {
    content_type(stream) == "text/event-stream"
}

/// The ids a message (or batch) answers: objects with an `id` and a
/// `result` or `error`, and no `method`.
fn response_ids(value: &Json) -> Vec<Json> {
    let one = |item: &Json| -> Option<Json> {
        let answers = item.get("result").is_some() || item.get("error").is_some();
        (answers && item.get("method").is_none())
            .then(|| item.get("id").cloned())
            .flatten()
    };
    match value {
        Json::Array(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

/// `params.requestId` of every `notifications/cancelled` in a client line.
fn cancelled_ids(value: &Json) -> Vec<Json> {
    let one = |item: &Json| -> Option<Json> {
        (item.get("method").and_then(Json::as_str) == Some("notifications/cancelled"))
            .then(|| item.get("params").and_then(|p| p.get("requestId")).cloned())
            .flatten()
    };
    match value {
        Json::Array(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

/// Keys of the `tools/list` requests in a client line that continue a
/// listing (`params.cursor` present): pages extend the current approvals.
fn paged_listings(value: &Json) -> Vec<String> {
    let one = |item: &Json| -> Option<String> {
        (item.get("method").and_then(Json::as_str) == Some("tools/list")
            && item.get("params").and_then(|p| p.get("cursor")).is_some())
        .then(|| item.get("id").map(key_of))
        .flatten()
    };
    match value {
        Json::Array(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_ids_ignore_requests_and_notifications() {
        let value = toolkit_core::parse_json(
            r#"[{"id":1,"result":{}},{"id":2,"method":"x"},{"method":"n"},{"id":"a","error":{}}]"#,
        )
        .unwrap();
        assert_eq!(response_ids(&value), vec![Json::Int(1), Json::string("a")]);
    }
}
