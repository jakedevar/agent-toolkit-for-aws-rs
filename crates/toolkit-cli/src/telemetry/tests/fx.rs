//! Offline fixtures for the telemetry tests: an isolated home, project and
//! bundle directory, an injected environment, clock and stdin, and a
//! recording sink. Nothing touches the network or the real home directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::telemetry::client::PostRequest;
use crate::telemetry::client::PostStatus;
use crate::telemetry::{run_action, Ctx, EventSink};
use crate::Output;

pub(super) const RUN_ID: &str = "3f9c2a7e-5b1d-4e8a-9c6f-2d7b8e1a4c53";
pub(super) const SESSION_ID: &str = "9b2c4d6e-8f10-4a12-b345-6789abcdef01";
pub(super) const OTHER_SESSION: &str = "0badf00d-1111-4222-8333-444444444444";
pub(super) const INSTALL_ID: &str = "11111111-2222-3333-4444-555555555555";
pub(super) const VERSION: &str = "2.0.3";

pub(super) struct TempDir(PathBuf);

impl TempDir {
    pub(super) fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-telemetry-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One request the sink received.
pub(super) struct Sent {
    pub url: String,
    pub body: Value,
    pub timeout: Duration,
    pub content_type: String,
    pub user_agent: String,
    pub raw: String,
}

type Responder = Box<dyn Fn(&Value) -> PostStatus + Send>;

/// Records every request and answers with a scripted status.
pub(super) struct Recorder {
    sent: Mutex<Vec<Sent>>,
    responder: Mutex<Responder>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            responder: Mutex::new(Box::new(|_| PostStatus::Status(200))),
        }
    }

    pub(super) fn respond_with(&self, status: PostStatus) {
        *self.responder.lock().expect("lock") = Box::new(move |_| status);
    }

    pub(super) fn respond_by(&self, rule: impl Fn(&Value) -> PostStatus + Send + 'static) {
        *self.responder.lock().expect("lock") = Box::new(rule);
    }

    pub(super) fn count(&self) -> usize {
        self.sent.lock().expect("lock").len()
    }

    pub(super) fn bodies_from(&self, start: usize) -> Vec<Value> {
        self.sent.lock().expect("lock")[start..]
            .iter()
            .map(|sent| sent.body.clone())
            .collect()
    }

    pub(super) fn headers(&self) -> Vec<(String, String)> {
        self.sent
            .lock()
            .expect("lock")
            .iter()
            .map(|sent| (sent.content_type.clone(), sent.user_agent.clone()))
            .collect()
    }

    pub(super) fn all_raw(&self) -> Vec<String> {
        self.sent
            .lock()
            .expect("lock")
            .iter()
            .map(|sent| sent.raw.clone())
            .collect()
    }

    pub(super) fn urls(&self) -> Vec<String> {
        self.sent
            .lock()
            .expect("lock")
            .iter()
            .map(|sent| sent.url.clone())
            .collect()
    }

    pub(super) fn timeouts(&self) -> Vec<Duration> {
        self.sent
            .lock()
            .expect("lock")
            .iter()
            .map(|sent| sent.timeout)
            .collect()
    }
}

impl EventSink for Recorder {
    fn post(&self, request: &PostRequest<'_>) -> PostStatus {
        let value: Value = serde_json::from_str(request.body).expect("the body is JSON");
        let status = (self.responder.lock().expect("lock"))(&value);
        self.sent.lock().expect("lock").push(Sent {
            url: request.url.to_string(),
            body: value,
            timeout: request.timeout,
            content_type: request.content_type.to_string(),
            user_agent: request.user_agent.to_string(),
            raw: request.body.to_string(),
        });
        status
    }
}

/// An isolated environment for one test.
pub(super) struct Fx {
    pub dir: TempDir,
    pub home: PathBuf,
    pub home_known: bool,
    pub bundle: PathBuf,
    pub env: Mutex<BTreeMap<String, String>>,
    pub clock: Mutex<f64>,
    pub stdin: Mutex<String>,
    pub sink: Recorder,
}

impl Fx {
    pub(super) fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        let home = dir.path().join("home");
        let bundle = dir.path().join("bundle");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&bundle).expect("bundle");
        fs::write(
            bundle.join("version.json"),
            json!({"version": VERSION}).to_string(),
        )
        .expect("version.json");
        let mut env = BTreeMap::new();
        env.insert("CLAUDECODE".to_string(), "1".to_string());
        Self {
            dir,
            home,
            home_known: true,
            bundle,
            env: Mutex::new(env),
            clock: Mutex::new(crate::telemetry::system_now()),
            stdin: Mutex::new(String::new()),
            sink: Recorder::new(),
        }
    }

    pub(super) fn set_env(&self, name: &str, value: &str) {
        self.env
            .lock()
            .expect("lock")
            .insert(name.to_string(), value.to_string());
    }

    pub(super) fn clear_env(&self) {
        self.env.lock().expect("lock").clear();
    }

    pub(super) fn set_clock(&self, seconds: f64) {
        *self.clock.lock().expect("lock") = seconds;
    }

    pub(super) fn set_stdin(&self, text: &str) {
        *self.stdin.lock().expect("lock") = text.to_string();
    }

    /// Runs `f` with a context over this environment.
    pub(super) fn with<R>(&self, f: impl FnOnce(&Ctx<'_>) -> R) -> R {
        self.with_cwd(self.dir.path(), f)
    }

    /// [`Fx::with`] with an explicit working directory.
    pub(super) fn with_cwd<R>(&self, cwd: &Path, f: impl FnOnce(&Ctx<'_>) -> R) -> R {
        let env = |name: &str| self.env.lock().expect("lock").get(name).cloned();
        let now = || *self.clock.lock().expect("lock");
        let stdin = |_timeout: Duration| self.stdin.lock().expect("lock").clone();
        let ctx = Ctx {
            home: self.home_known.then_some(self.home.as_path()),
            env: &env,
            cwd,
            telemetry_dir: Some(self.bundle.as_path()),
            now: &now,
            sink: &self.sink,
            stdin: &stdin,
        };
        f(&ctx)
    }

    /// Runs `toolkit telemetry WORDS`; returns (status, stdout, stderr).
    pub(super) fn run(&self, words: &[&str]) -> (u8, String, String) {
        self.run_in(self.dir.path(), words)
    }

    /// [`Fx::run`] from the working directory `cwd`.
    pub(super) fn run_in(&self, cwd: &Path, words: &[&str]) -> (u8, String, String) {
        let words: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = {
            let mut output = Output::with_diagnostics(&mut out, &mut err);
            match self.with_cwd(cwd, |ctx| run_action(&words, ctx, &mut output)) {
                Ok(()) => output.exit_status(),
                Err(error) => error.exit_code(),
            }
        };
        (
            status,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    pub(super) fn consent_path(&self) -> PathBuf {
        self.home
            .join(".aws-startup-advisor")
            .join("plugin-telemetry.json")
    }

    /// Writes a consent record directly, including ones no command writes.
    pub(super) fn write_record(&self, status: &str, install_id: &str) {
        self.write_record_text(
            &json!({"installId": install_id, "consentStatus": status}).to_string(),
        );
    }

    pub(super) fn write_record_text(&self, text: &str) {
        let path = self.consent_path();
        fs::create_dir_all(path.parent().expect("parent")).expect("state dir");
        fs::write(path, text).expect("record");
    }

    pub(super) fn accept(&self) {
        self.write_record("ACCEPTED", INSTALL_ID);
    }

    pub(super) fn record_text(&self) -> Option<String> {
        fs::read_to_string(self.consent_path()).ok()
    }
}
