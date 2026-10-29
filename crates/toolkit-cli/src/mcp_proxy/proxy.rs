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

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
            shutdown: AtomicBool::new(false),
            exit: Mutex::new(None),
            exit_changed: Condvar::new(),
            jitter: FullJitter::new(),
            retry,
            workers: AtomicUsize::new(0),
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
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .expect("the proxy can start a thread")
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
}

#[derive(Debug, Default)]
struct Pending {
    entries: HashMap<String, Entry>,
    /// Workers holding a concurrency slot.
    slots: usize,
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
    shutdown: AtomicBool,
    exit: Mutex<Option<Exit>>,
    exit_changed: Condvar,
    jitter: FullJitter,
    retry: RetryPolicy,
    workers: AtomicUsize,
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
}

/// Per-response context while delivering upstream messages.
struct Delivery<'a> {
    task: &'a Task,
    keys: &'a [String],
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
        self.pending_changed.notify_all();
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

    fn release_slot(&self, keys: &[String]) {
        let mut pending = lock(&self.pending);
        for key in keys {
            pending.entries.remove(key);
        }
        pending.slots = pending.slots.saturating_sub(1);
        drop(pending);
        self.pending_changed.notify_all();
    }

    // -------------------------------------------------------- supervisor

    fn supervise(&self) {
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
                drop(pending);
                for key in &expired {
                    self.fail_entry(key, Failure::TimedOut { seconds: 0 });
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
        };
        self.dispatch(&requests, job);
    }

    /// The concurrency cap, then registration and the worker: a message
    /// with requests beyond the cap is refused; other messages wait for a
    /// slot.
    fn dispatch(self: &Arc<Self>, requests: &[(Json, Option<String>, String)], job: Job) {
        let limits = &self.config.limits;
        let mut pending = lock(&self.pending);
        loop {
            if self.is_shutdown() {
                return;
            }
            if pending.slots < limits.max_in_flight {
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
        pending.slots += 1;
        let now = self.clock.now();
        let mut task_deadline = now + limits.request_timeout;
        let mut entries = Vec::new();
        for (id, tool, _) in requests {
            let timeout = if tool.is_some() {
                limits.tool_timeout
            } else {
                limits.request_timeout
            };
            task_deadline = task_deadline.max(now + timeout);
            entries.push((id.clone(), tool.clone(), now + timeout, timeout.as_secs()));
        }
        let task = Arc::new(Task {
            aborted: AtomicBool::new(false),
            deadline: task_deadline,
        });
        for (id, tool, deadline, timeout_seconds) in entries {
            pending.entries.insert(
                key_of(&id),
                Entry {
                    id,
                    tool,
                    deadline,
                    timeout_seconds,
                    task: Arc::clone(&task),
                    finished: false,
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

    fn build_request(&self, method: Method, body: Vec<u8>) -> Result<HttpRequest, AwsError> {
        let mut headers = Headers::new();
        for (name, value) in self.config.headers.entries() {
            if self.config.headers.is_sensitive(name) {
                headers.insert_sensitive(name, value)?;
            } else {
                headers.insert(name, value)?;
            }
        }
        match method {
            Method::Get => headers.insert("accept", "text/event-stream")?,
            _ => headers.insert("accept", "application/json, text/event-stream")?,
        }
        if method == Method::Post {
            headers.insert("content-type", "application/json")?;
        }
        {
            let session = lock(&self.session);
            if let Some(id) = &session.id {
                headers.insert("mcp-session-id", id)?;
            }
            if let Some(version) = &session.protocol_version {
                headers.insert("mcp-protocol-version", version)?;
            }
        }
        if let Some(agent) = &self.config.user_agent {
            headers.insert("user-agent", agent)?;
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

    /// Send `body` with retries and the one D1 resend; the accepted stream.
    fn open_post<'a>(
        &'a self,
        task: &Task,
        body: &[u8],
    ) -> Result<Box<dyn ResponseStream + 'a>, Stop> {
        let mut retries = 0;
        let mut refreshed = false;
        loop {
            if task.aborted() || self.is_shutdown() {
                return Err(Stop::Abort);
            }
            let mut request = self
                .build_request(Method::Post, body.to_vec())
                .map_err(|_| Stop::Fail(Failure::Connect))?;
            if let Err(error) = self.auth.sign(&mut request) {
                self.emit_event(DiagEvent::TransportFailure { code: error.code() });
                return Err(Stop::Fail(Failure::Credentials));
            }
            match self.transport.open(&request, &self.stream_limits) {
                Err(error) => {
                    if error.is_retryable() && retries < self.retry.max_retries() {
                        retries += 1;
                        self.backoff(retries);
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
                    if (status == 429 || (500..600).contains(&status))
                        && retries < self.retry.max_retries()
                    {
                        drop(stream);
                        retries += 1;
                        self.backoff(retries);
                        continue;
                    }
                    return Ok(stream);
                }
            }
        }
    }

    fn work(self: &Arc<Self>, task: &Arc<Task>, job: Job) {
        let Job {
            body,
            body_hold,
            keys,
            initialize,
            initialized,
        } = job;
        let outcome = self.exchange(task, body, body_hold, &keys, initialize, initialized);
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
        self.release_slot(&keys);
    }

    fn exchange(
        self: &Arc<Self>,
        task: &Task,
        body: Vec<u8>,
        body_hold: Reservation,
        keys: &[String],
        initialize: bool,
        initialized: bool,
    ) -> Result<(), Stop> {
        let mut stream = self.open_post(task, &body)?;
        drop(body);
        drop(body_hold);
        let status = stream.status();
        let has_session = lock(&self.session).id.is_some();
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
            self.start_get_stream();
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
        let cancelled = || task.aborted() || self.is_shutdown();
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

    /// Parse, check, re-serialise and queue one upstream message; `true` when
    /// it was queued.
    fn deliver(
        &self,
        delivery: &Delivery<'_>,
        bytes: Vec<u8>,
        raw_hold: Reservation,
    ) -> Result<bool, Stop> {
        let task = delivery.task;
        let cancelled = || task.aborted() || self.is_shutdown();
        let limits = &self.config.limits;
        let charge = self.budget.reserve(
            limits.parse_charge(bytes.len()),
            &self.budget_wait(&cancelled),
        )?;
        let parsed = parse_upstream(&bytes, limits);
        drop(bytes);
        drop(raw_hold);
        let value = match parsed {
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
            if !self.commit_session(delivery.session_id.clone(), &value) {
                self.fail_entry(key, Failure::SessionRefused);
                return Ok(false);
            }
        }
        drop(value);
        drop(charge);
        if !answered.is_empty() {
            let claimed: Vec<bool> = answered.iter().map(|key| self.claim(key)).collect();
            if !claimed.iter().any(|c| *c) {
                return Ok(false); // already answered: timed out or cancelled
            }
        }
        if task.aborted() {
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

    /// Store the session from an `initialize` response; `false` refuses it.
    fn commit_session(&self, session_id: Option<String>, response: &Json) -> bool {
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
        *lock(&self.session) = Session {
            id: session_id,
            protocol_version: version,
            get_started: false,
        };
        if started {
            self.auth.session_started();
        }
        true
    }

    fn start_get_stream(self: &Arc<Self>) {
        {
            let mut session = lock(&self.session);
            if session.id.is_none() || session.get_started {
                return;
            }
            session.get_started = true;
        }
        let shared = Arc::clone(self);
        spawn("mcp-proxy-get", move || shared.get_loop());
    }

    /// Sleep on the clock until `at`; `false` on shutdown.
    fn sleep_until(&self, at: Duration) -> bool {
        let mut pending = lock(&self.pending);
        loop {
            if self.is_shutdown() {
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

    /// The server-initiated stream: reopened at most once per interval; a
    /// 405 (no stream offered) ends it silently.
    fn get_loop(self: &Arc<Self>) {
        let task = Task {
            aborted: AtomicBool::new(false),
            deadline: Duration::MAX,
        };
        let mut last_open: Option<Duration> = None;
        loop {
            if let Some(at) = last_open {
                if !self.sleep_until(at + self.config.limits.get_reopen_interval) {
                    return;
                }
            }
            if self.is_shutdown() {
                return;
            }
            last_open = Some(self.clock.now());
            let Ok(mut request) = self.build_request(Method::Get, Vec::new()) else {
                return;
            };
            if let Err(error) = self.auth.sign(&mut request) {
                self.emit_event(DiagEvent::TransportFailure { code: error.code() });
                continue;
            }
            let mut stream = match self.transport.open(&request, &self.stream_limits) {
                Ok(stream) => stream,
                Err(error) => {
                    self.emit_event(DiagEvent::TransportFailure { code: error.code() });
                    continue;
                }
            };
            let status = stream.status();
            if status == 405 {
                return;
            }
            if !(200..300).contains(&status) {
                self.emit_event(DiagEvent::HttpStatus { status });
                continue;
            }
            if is_event_stream(&*stream) {
                let delivery = Delivery {
                    task: &task,
                    keys: &[],
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

    /// stdin EOF: end the session with exactly one `DELETE`.
    fn end_session(&self) {
        if lock(&self.session).id.is_none() {
            return;
        }
        let Ok(mut request) = self.build_request(Method::Delete, Vec::new()) else {
            return;
        };
        if let Err(error) = self.auth.sign(&mut request) {
            self.emit_event(DiagEvent::TransportFailure { code: error.code() });
            return;
        }
        match self.transport.open(&request, &self.stream_limits) {
            Ok(stream) => {
                let status = stream.status();
                if !(200..300).contains(&status) && status != 405 {
                    self.emit_event(DiagEvent::HttpStatus { status });
                }
            }
            Err(error) => self.emit_event(DiagEvent::TransportFailure { code: error.code() }),
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
        StreamError::IdleTimeout => Stop::Fail(Failure::Unresponsive),
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
