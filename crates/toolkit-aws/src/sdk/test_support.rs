//! Offline harness for the S1 SDK tests (#4413): temp directories, child
//! processes, a synthetic PKI and a loopback TLS server.
//!
//! **Children.** The SDK and the root loaders read the process environment,
//! and roots are validated once per process, so each case runs in its own
//! process: this test binary re-executed with `--exact <entry test>`, a
//! cleared environment set with `Command::env` (never `set_var`) and
//! `TOOLKIT_AWS_CASE` naming the case. The entry test does nothing when that
//! variable is absent, so it is a no-op in the normal run. A child reports
//! facts as `RESULT key=value` lines on stdout; the parent asserts on them.
//!
//! **Network.** Only loopback: the TLS server listens on `127.0.0.1:0`, and
//! every other request in these tests goes to a recorder, never a socket.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Environment variable naming the case a child runs.
pub const CASE_VAR: &str = "TOOLKIT_AWS_CASE";

/// The case this process runs as a child, if any.
pub fn child_case() -> Option<String> {
    std::env::var(CASE_VAR).ok()
}

/// Print one fact for the parent.
pub fn report(key: &str, value: impl std::fmt::Display) {
    println!("\nRESULT {key}={value}");
}

/// A directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    /// A fresh, empty directory.
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "toolkit-aws-{label}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Write `contents` to `name` inside it and return the path.
    pub fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture parent dir");
        }
        std::fs::write(&path, contents).expect("fixture file");
        path
    }

    /// Create (and return) a sub-directory.
    pub fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).expect("fixture dir");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child's outcome.
pub struct Child {
    /// Exit status code (`None` when killed by a signal).
    pub code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// `RESULT` facts in order of appearance (a later key replaces).
    pub results: BTreeMap<String, String>,
}

impl Child {
    /// The fact `key`.
    ///
    /// # Panics
    /// When the child did not report it (with the child's output).
    pub fn result(&self, key: &str) -> &str {
        self.results.get(key).map_or_else(
            || {
                panic!(
                    "child reported no `{key}`\nstdout:\n{}\nstderr:\n{}",
                    self.stdout, self.stderr
                )
            },
            String::as_str,
        )
    }

    /// Assert the child's own assertions passed.
    pub fn assert_ok(&self) {
        assert_eq!(
            self.code,
            Some(0),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
    }
}

fn parse(output: &Output) -> Child {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let results = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("RESULT "))
        .filter_map(|fact| fact.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    Child {
        code: output.status.code(),
        stdout,
        stderr,
        results,
    }
}

/// A command that re-runs this test binary as the child `entry` test for
/// `case`, with a cleared environment. Callers add variables and run it.
pub fn child_command(entry: &str, case: &str) -> Command {
    let exe = std::env::current_exe().expect("test binary path");
    let mut command = Command::new(exe);
    command
        .args(["--exact", entry, "--nocapture", "--test-threads", "1"])
        .env_clear()
        .env(CASE_VAR, case)
        .stdin(Stdio::null());
    command
}

/// Run `command` to completion and parse its output.
pub fn run(command: &mut Command) -> Child {
    parse(&command.output().expect("child process"))
}

/// A synthetic CA and certificates it issues.
pub struct Pki {
    key: KeyPair,
    params: CertificateParams,
    /// The CA certificate.
    pub ca_der: CertificateDer<'static>,
    /// The CA certificate as PEM.
    pub ca_pem: String,
}

impl Pki {
    /// A new CA named `name`.
    pub fn new(name: &str) -> Self {
        let ca_key = KeyPair::generate().expect("CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name.push(DnType::CommonName, name);
        let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
        Self {
            ca_der: ca.der().clone(),
            ca_pem: ca.pem(),
            key: ca_key,
            params: ca_params,
        }
    }

    /// A server certificate for `host` and its private key.
    pub fn server(&self, host: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let key = KeyPair::generate().expect("server key");
        let params = CertificateParams::new(vec![host.to_string()]).expect("server params");
        let issuer = Issuer::from_params(&self.params, &self.key);
        let cert = params.signed_by(&key, &issuer).expect("server certificate");
        (
            vec![cert.der().clone(), self.ca_der.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
    }
}

/// One piece of a loopback reply.
#[derive(Debug, Clone)]
pub enum Part {
    /// Bytes written (and flushed) as they are.
    Bytes(Vec<u8>),
    /// A pause before the next part.
    Pause(Duration),
    /// Wait for the client to close the connection and report the result.
    ObserveClose(std::sync::mpsc::Sender<bool>),
}

/// What the loopback server does with each request (head text, body).
pub type Responder = Arc<dyn Fn(&str, &[u8]) -> Vec<Part> + Send + Sync>;

/// Requests a loopback server read: head text and body.
type Recorded = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A loopback TLS server (rustls, aws-lc) on `127.0.0.1:0`. It counts accepted
/// TCP connections, records each complete request head and body it reads,
/// and answers with the responder's raw bytes.
pub struct TlsServer {
    port: u16,
    accepted: Arc<AtomicUsize>,
    requests: Recorded,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TlsServer {
    /// Serve `chain`/`key` with `respond`.
    pub fn start(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        respond: Responder,
    ) -> Self {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("server certificate");
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (accepted, requests, stop) = (
                Arc::clone(&accepted),
                Arc::clone(&requests),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            accepted.fetch_add(1, Ordering::SeqCst);
                            serve(socket, &config, &requests, &respond);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            port,
            accepted,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// The listening port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// TCP connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Complete requests read so far: head text and body bytes.
    pub fn requests(&self) -> Vec<(String, Vec<u8>)> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(
    socket: TcpStream,
    config: &Arc<rustls::ServerConfig>,
    requests: &Mutex<Vec<(String, Vec<u8>)>>,
    respond: &Responder,
) {
    let _ = socket.set_nonblocking(false);
    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(connection) = rustls::ServerConnection::new(Arc::clone(config)) else {
        return;
    };
    let mut tls = rustls::StreamOwned::new(connection, socket);
    let mut buffer = Vec::new();
    let mut byte = [0_u8; 1];
    // Read the head byte by byte (a failed handshake surfaces here).
    while !buffer.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte) {
            Ok(1) => buffer.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&buffer).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if tls.read_exact(&mut body).is_err() {
        return;
    }
    let reply = respond(&head, &body);
    requests.lock().expect("requests").push((head, body));
    for part in reply {
        match part {
            Part::Bytes(bytes) => {
                if tls.write_all(&bytes).and_then(|()| tls.flush()).is_err() {
                    return;
                }
            }
            Part::Pause(duration) => std::thread::sleep(duration),
            Part::ObserveClose(tx) => {
                let _ = tls.sock.set_read_timeout(Some(Duration::from_secs(2)));
                let mut byte = [0_u8; 1];
                let closed = match tls.read(&mut byte) {
                    Ok(0) => true,
                    Err(e) => matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                    ),
                    Ok(_) => false,
                };
                let _ = tx.send(closed);
                return;
            }
        }
    }
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// A plain-TCP listener that only counts connections (the `HTTPS_PROXY`
/// recorder of T-TLS-8).
pub struct CountingListener {
    port: u16,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CountingListener {
    /// Listen on `127.0.0.1:0`.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (accepted, stop) = (Arc::clone(&accepted), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if listener.accept().is_ok() {
                        accepted.fetch_add(1, Ordering::SeqCst);
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            })
        };
        Self {
            port,
            accepted,
            stop,
            handle: Some(handle),
        }
    }

    /// The listening port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for CountingListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A plain `200 OK` HTTP/1.1 response with `body`.
pub fn http_ok(body: &str) -> Vec<Part> {
    vec![Part::Bytes(raw_ok(body))]
}

/// The bytes of [`http_ok`].
pub fn raw_ok(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}
