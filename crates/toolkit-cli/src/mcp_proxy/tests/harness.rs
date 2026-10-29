//! In-process harness: injected stdin, a pausable and breakable stdout, a
//! diagnostics sink, a fake clock and scripted upstreams. No sockets, no DNS,
//! no credentials and no real waiting on protocol timeouts.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use toolkit_aws::diag::Diag;
use toolkit_aws::stream::ChunkLimiter;
use toolkit_aws::{
    AwsError, Headers, HttpRequest, Method, ResponseStream, StreamError, StreamLimits,
    StreamingTransport,
};
use toolkit_core::{parse_json, Json};

use crate::mcp_proxy::{
    Auth, Budget, Exit, FakeClock, Limits, LimitsSpec, Proxy, ProxyConfig, Unsigned,
};

/// How long a test waits for something that should happen at once.
pub const PATIENCE: Duration = Duration::from_secs(20);

pub const URL: &str = "https://aws-mcp.us-east-1.api.aws/mcp";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Poll `condition` until it holds; panic with `what` after [`PATIENCE`].
pub fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Give background threads a moment to do something they must not do.
pub fn settle() {
    std::thread::sleep(Duration::from_millis(60));
}

// ---------------------------------------------------------------- stdin

struct ChannelReader {
    rx: Receiver<Vec<u8>>,
    current: Vec<u8>,
    offset: usize,
    reads: Arc<AtomicUsize>,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.offset >= self.current.len() {
            match self.rx.recv() {
                Ok(next) => {
                    self.reads.fetch_add(1, Ordering::SeqCst);
                    self.current = next;
                    self.offset = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len() - self.offset);
        buf[..n].copy_from_slice(&self.current[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

// --------------------------------------------------------------- stdout

#[derive(Default)]
struct SinkState {
    bytes: Vec<u8>,
    paused: bool,
    broken: bool,
}

/// A shared writer. `pause` blocks writes; `break_pipe` fails them (EPIPE).
#[derive(Clone, Default)]
pub struct Sink(Arc<(Mutex<SinkState>, Condvar)>);

impl Sink {
    pub fn pause(&self) {
        lock(&self.0 .0).paused = true;
    }

    pub fn resume(&self) {
        lock(&self.0 .0).paused = false;
        self.0 .1.notify_all();
    }

    pub fn break_pipe(&self) {
        lock(&self.0 .0).broken = true;
        self.0 .1.notify_all();
    }

    pub fn text(&self) -> String {
        String::from_utf8(lock(&self.0 .0).bytes.clone()).expect("output is UTF-8")
    }

    /// Complete lines written so far.
    pub fn lines(&self) -> Vec<String> {
        let text = self.text();
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        lines.pop(); // the part after the last newline (empty when complete)
        lines
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (state, changed) = &*self.0;
        let mut state = lock(state);
        while state.paused && !state.broken {
            state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        if state.broken {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        state.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if lock(&self.0 .0).broken {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        Ok(())
    }
}

// ------------------------------------------------------------- upstream

/// A gate a scripted stream blocks on until the test opens it.
#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&self) {
        *lock(&self.0 .0) = true;
        self.0 .1.notify_all();
    }

    fn wait(&self) {
        let (open, changed) = &*self.0;
        let mut open = lock(open);
        while !*open {
            open = changed.wait(open).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// One body step of a scripted response.
#[derive(Clone)]
pub enum Step {
    Data(Vec<u8>),
    Gate(Gate),
    IdleTimeout,
    Fail(AwsError),
}

/// A scripted response.
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub steps: Vec<Step>,
    /// Gate `open` itself on this (the response head is delayed).
    pub head_gate: Option<Gate>,
    /// Fail `open` with this error.
    pub open_error: Option<AwsError>,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            steps: Vec::new(),
            head_gate: None,
            open_error: None,
        }
    }

    pub fn accepted() -> Self {
        Self::status(202)
    }

    pub fn json(body: &str) -> Self {
        Self::status(200)
            .header("content-type", "application/json")
            .data(body.as_bytes())
    }

    /// An SSE response with one event per entry.
    pub fn sse(events: &[&str]) -> Self {
        let mut reply = Self::status(200).header("content-type", "text/event-stream");
        for event in events {
            reply = reply.data(format!("data: {event}\n\n").as_bytes());
        }
        reply
    }

    pub fn sse_raw(chunks: &[&[u8]]) -> Self {
        let mut reply = Self::status(200).header("content-type", "text/event-stream");
        for chunk in chunks {
            reply = reply.data(chunk);
        }
        reply
    }

    pub fn fail_open(error: AwsError) -> Self {
        let mut reply = Self::status(0);
        reply.open_error = Some(error);
        reply
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn data(mut self, bytes: &[u8]) -> Self {
        self.steps.push(Step::Data(bytes.to_vec()));
        self
    }

    pub fn step(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }
}

/// What the upstream received.
#[derive(Clone)]
pub struct Seen {
    pub method: Method,
    pub url: String,
    pub headers: Headers,
    pub body: Vec<u8>,
    pub json: Option<Json>,
}

impl Seen {
    pub fn rpc_method(&self) -> Option<&str> {
        self.json.as_ref()?.get("method")?.as_str()
    }

    pub fn id(&self) -> Option<&Json> {
        self.json.as_ref()?.get("id")
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    pub fn body_text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("body is UTF-8")
    }
}

type Handler = Box<dyn Fn(&Seen) -> Reply + Send + Sync>;

/// A routing upstream: each request is answered by the handler, so
/// concurrent requests get their own scripted replies whatever their order.
pub struct Upstream {
    handler: Handler,
    seen: Mutex<Vec<Seen>>,
    pulled: Arc<AtomicUsize>,
}

impl Upstream {
    pub fn new(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            handler: Box::new(handler),
            seen: Mutex::new(Vec::new()),
            pulled: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.seen).clone()
    }

    pub fn seen_rpc(&self, method: &str) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.rpc_method() == Some(method))
            .collect()
    }

    pub fn count(&self, method: Method) -> usize {
        self.seen().iter().filter(|s| s.method == method).count()
    }

    /// Body frames pulled from every stream so far.
    pub fn pulled(&self) -> usize {
        self.pulled.load(Ordering::SeqCst)
    }
}

struct ScriptedStream {
    status: u16,
    headers: Headers,
    steps: VecDeque<Step>,
    limiter: ChunkLimiter,
    pulled: Arc<AtomicUsize>,
}

impl ResponseStream for ScriptedStream {
    fn status(&self) -> u16 {
        self.status
    }

    fn headers(&self) -> &Headers {
        &self.headers
    }

    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        let steps = &mut self.steps;
        let pulled = &self.pulled;
        self.limiter.next(|| loop {
            let Some(step) = steps.pop_front() else {
                return Ok(None);
            };
            match step {
                Step::Gate(gate) => gate.wait(),
                Step::Data(bytes) => {
                    pulled.fetch_add(1, Ordering::SeqCst);
                    return Ok(Some(bytes));
                }
                Step::IdleTimeout => return Err(StreamError::IdleTimeout),
                Step::Fail(error) => return Err(StreamError::Failed(error)),
            }
        })
    }
}

impl StreamingTransport for Upstream {
    fn open<'a>(
        &'a self,
        request: &HttpRequest,
        limits: &StreamLimits,
    ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
        toolkit_aws::refuse_framing_headers(&request.headers)?;
        let json = std::str::from_utf8(&request.body)
            .ok()
            .and_then(|text| parse_json(text).ok());
        let seen = Seen {
            method: request.method,
            url: request.url.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            json,
        };
        lock(&self.seen).push(seen.clone());
        let reply = (self.handler)(&seen);
        if let Some(gate) = &reply.head_gate {
            gate.wait();
        }
        if let Some(error) = reply.open_error {
            return Err(error);
        }
        let mut headers = Headers::new();
        for (name, value) in &reply.headers {
            headers.insert(name, value).expect("fixture header");
        }
        Ok(Box::new(ScriptedStream {
            status: reply.status,
            headers,
            steps: reply.steps.into(),
            limiter: ChunkLimiter::new(*limits),
            pulled: Arc::clone(&self.pulled),
        }))
    }
}

// -------------------------------------------------------------- harness

pub struct Harness {
    stdin: Option<Sender<Vec<u8>>>,
    pub stdin_reads: Arc<AtomicUsize>,
    pub stdout: Sink,
    pub diag: Sink,
    pub clock: Arc<FakeClock>,
    pub budget: Budget,
    pub control: Budget,
    cursor: usize,
    handle: Option<JoinHandle<Exit>>,
}

pub fn config() -> ProxyConfig {
    ProxyConfig::new(URL)
}

/// The T-BUDGET limits (see the module docs of `budget.rs` in tests).
pub fn small_limits() -> Limits {
    Limits::new(LimitsSpec {
        max_line: 1024,
        max_message: 1024,
        max_sse_line: 1024,
        max_depth: 16,
        max_nodes: 32,
        budget: 8192,
        control_reserve: 1025,
        max_in_flight: 16,
        data_queue_lines: 64,
        control_queue_lines: 64,
        max_chunk: 4096,
        request_timeout: Duration::from_secs(180),
        tool_timeout: Duration::from_secs(300),
        get_reopen_interval: Duration::from_secs(30),
    })
    .expect("valid test limits")
}

pub fn spec(limits: Limits) -> LimitsSpec {
    LimitsSpec::from(limits)
}

pub fn start(config: ProxyConfig, transport: Arc<dyn StreamingTransport + Send + Sync>) -> Harness {
    start_with(config, transport, Arc::new(Unsigned))
}

pub fn start_with(
    config: ProxyConfig,
    transport: Arc<dyn StreamingTransport + Send + Sync>,
    auth: Arc<dyn Auth>,
) -> Harness {
    let clock = Arc::new(FakeClock::new());
    let proxy = Proxy::new(config, transport, auth, clock.clone());
    let budget = proxy.budget();
    let control = proxy.control_budget();
    let (tx, rx) = channel();
    let reads = Arc::new(AtomicUsize::new(0));
    let stdin = ChannelReader {
        rx,
        current: Vec::new(),
        offset: 0,
        reads: Arc::clone(&reads),
    };
    let stdout = Sink::default();
    let diag = Sink::default();
    let handle = {
        let stdout = stdout.clone();
        let diag = Diag::new("mcp-proxy", Box::new(diag.clone()));
        std::thread::spawn(move || proxy.run(Box::new(stdin), Box::new(stdout), diag))
    };
    Harness {
        stdin: Some(tx),
        stdin_reads: reads,
        stdout,
        diag,
        clock,
        budget,
        control,
        cursor: 0,
        handle: Some(handle),
    }
}

impl Harness {
    /// Send one client line (a newline is appended).
    pub fn send(&self, line: &str) {
        self.send_raw(format!("{line}\n").as_bytes());
    }

    pub fn send_raw(&self, bytes: &[u8]) {
        self.stdin
            .as_ref()
            .expect("stdin is open")
            .send(bytes.to_vec())
            .expect("the proxy reads stdin");
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// The next stdout line, parsed (waits for it).
    pub fn next(&mut self) -> Json {
        let text = self.next_text();
        parse_json(&text).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {text}"))
    }

    pub fn next_text(&mut self) -> String {
        let want = self.cursor + 1;
        eventually("a stdout line", || self.stdout.lines().len() >= want);
        let line = self.stdout.lines()[self.cursor].clone();
        self.cursor += 1;
        line
    }

    /// Assert no further stdout line appears for a moment.
    pub fn no_more_output(&self) {
        settle();
        assert_eq!(
            self.stdout.lines().len(),
            self.cursor,
            "unexpected output: {:?}",
            &self.stdout.lines()[self.cursor..]
        );
    }

    pub fn diag_text(&self) -> String {
        self.diag.text()
    }

    pub fn wait_for_in_use(&self, bytes: usize) {
        eventually(&format!("in_use == {bytes}"), || {
            self.budget.in_use() == bytes
        });
    }

    pub fn wait_budget_zero(&self) {
        eventually("the budget to return to zero", || {
            self.budget.in_use() == 0 && self.control.in_use() == 0
        });
    }

    /// Close stdin and wait for the proxy to return.
    pub fn finish(mut self) -> Exit {
        self.close_stdin();
        self.join()
    }

    pub fn join(mut self) -> Exit {
        let handle = self.handle.take().expect("running");
        let start = Instant::now();
        while !handle.is_finished() {
            assert!(start.elapsed() < PATIENCE, "the proxy did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        handle.join().expect("the proxy did not panic")
    }
}

// --------------------------------------------------------------- helpers

pub fn request(id: i64, method: &str, params: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
}

pub fn tool_call(id: i64, name: &str) -> String {
    request(
        id,
        "tools/call",
        &format!(r#"{{"name":"{name}","arguments":{{}}}}"#),
    )
}

pub fn result(id: &Json, body: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"result":{body}}}"#,
        id.to_compact_string()
    )
}

pub fn ok_result(seen: &Seen) -> String {
    result(seen.id().expect("a request"), r#"{"ok":true}"#)
}

pub fn error_message(line: &Json) -> (i64, String) {
    let error = line.get("error").expect("an error response");
    let code = match error.get("code") {
        Some(Json::Int(code)) => *code,
        other => panic!("error code {other:?}"),
    };
    let message = error
        .get("message")
        .and_then(Json::as_str)
        .expect("message");
    (code, message.to_string())
}

pub fn tool_error_text(line: &Json) -> String {
    let result = line.get("result").expect("a result");
    assert_eq!(result.get("isError"), Some(&Json::Bool(true)), "{line:?}");
    let Some(Json::Array(content)) = result.get("content") else {
        panic!("content array: {line:?}");
    };
    assert_eq!(content.len(), 1);
    assert_eq!(content[0].get("type").and_then(Json::as_str), Some("text"));
    content[0]
        .get("text")
        .and_then(Json::as_str)
        .expect("text")
        .to_string()
}

pub fn id_of(line: &Json) -> Json {
    line.get("id").cloned().expect("an id")
}
