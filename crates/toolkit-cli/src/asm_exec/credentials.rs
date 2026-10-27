//! Credentials for signing the MCP request, in the Python order:
//!
//! 1. the environment (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
//!    `AWS_SESSION_TOKEN`);
//! 2. `aws configure export-credentials --format env` (AWS CLI v2: resolved,
//!    possibly assumed-role, credentials);
//! 3. `aws configure get` of `aws_access_key_id`, `aws_secret_access_key` and
//!    `aws_session_token` (AWS CLI v1 static keys).
//!
//! Each step is a [`CredentialsProvider`], composed with `ProviderChain`
//! (first success wins, "no credentials" moves on, anything else stops). The
//! AWS CLI is reached only through [`CommandRunner`] so tests never start a
//! process. Credential values live in [`Credentials`]/`Secret`, whose `Debug`
//! prints nothing; process output is never echoed.
//!
//! The `SigV4` signature and the HTTPS connection themselves are not available
//! in this build; see [`UnavailableSigner`] and [`UnavailableTransport`].

use std::io::Read;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use toolkit_aws::env::non_empty;
use toolkit_aws::{
    AwsError, Credentials, CredentialsProvider, Env, HttpRequest, HttpResponse, ProviderChain,
    Signer, SigningScope, Transport,
};

/// How long one `aws` invocation may run (the Python `timeout=5`).
pub const CLI_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes of `aws` output kept; credentials are a few hundred bytes.
const MAX_CLI_OUTPUT: u64 = 64 * 1024;

/// What running a program produced. `Debug` hides the output (it may be
/// credentials).
#[derive(Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Whether the program exited with status 0.
    pub success: bool,
    /// Standard output as lossy UTF-8.
    pub stdout: String,
}

impl std::fmt::Debug for CommandOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandOutput")
            .field("success", &self.success)
            .field("stdout_len", &self.stdout.len())
            .finish()
    }
}

/// Why a program could not be run to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailure {
    /// The program does not exist or cannot be started.
    NotStarted,
    /// It did not finish within the timeout and was killed.
    TimedOut,
}

/// Runs a program with arguments and captures its standard output.
pub trait CommandRunner {
    /// Run `program` and wait up to `timeout`.
    ///
    /// # Errors
    /// [`RunFailure`] when it could not be started or timed out.
    fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, RunFailure>;
}

/// Runner that starts the real program with null stdin and stderr.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, RunFailure> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| RunFailure::NotStarted)?;
        let reader = child.stdout.take().map(|stdout| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = stdout.take(MAX_CLI_OUTPUT).read_to_end(&mut bytes);
                bytes
            })
        });
        let deadline = Instant::now().checked_add(timeout);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(_) => break None,
            }
            if deadline.is_some_and(|limit| Instant::now() >= limit) {
                let _ = child.kill();
                let _ = child.wait();
                // The reader is detached: a grandchild may still hold the pipe.
                return Err(RunFailure::TimedOut);
            }
            thread::sleep(Duration::from_millis(10));
        };
        let bytes = reader
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        let status = status.ok_or(RunFailure::NotStarted)?;
        Ok(CommandOutput {
            success: status.success(),
            stdout: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

/// Provider 1: credentials from the environment.
///
/// The access key is the trigger (as in the Python: `if AWS_ACCESS_KEY_ID`);
/// a missing secret key then makes the credentials partial, which stops the
/// chain.
pub struct EnvProvider {
    env: Rc<dyn Env>,
}

impl EnvProvider {
    /// Provider over an environment view.
    #[must_use]
    pub fn new(env: Rc<dyn Env>) -> Self {
        Self { env }
    }
}

impl CredentialsProvider for EnvProvider {
    fn name(&self) -> &'static str {
        "environment"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        let Some(access_key) = non_empty(self.env.as_ref(), "AWS_ACCESS_KEY_ID") else {
            return Err(AwsError::NoCredentials);
        };
        let secret_key = self.env.var("AWS_SECRET_ACCESS_KEY").unwrap_or_default();
        let token = self.env.var("AWS_SESSION_TOKEN");
        Credentials::new(&access_key, &secret_key, token.as_deref(), None)
    }
}

/// Provider 2: `aws configure export-credentials --format env` (CLI v2).
pub struct CliExportProvider {
    runner: Rc<dyn CommandRunner>,
}

impl CliExportProvider {
    /// Provider over a command runner.
    #[must_use]
    pub fn new(runner: Rc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CredentialsProvider for CliExportProvider {
    fn name(&self) -> &'static str {
        "aws-cli-export-credentials"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        let output = self
            .runner
            .run(
                "aws",
                &["configure", "export-credentials", "--format", "env"],
                CLI_TIMEOUT,
            )
            .ok()
            .filter(|output| output.success)
            .ok_or(AwsError::NoCredentials)?;
        let (mut access_key, mut secret_key, mut token) = (None, None, None);
        for line in output.stdout.lines() {
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "AWS_ACCESS_KEY_ID" => access_key = Some(value),
                "AWS_SECRET_ACCESS_KEY" => secret_key = Some(value),
                "AWS_SESSION_TOKEN" => token = Some(value),
                _ => {}
            }
        }
        match access_key.filter(|key| !key.is_empty()) {
            Some(key) => Credentials::new(key, secret_key.unwrap_or_default(), token, None),
            None => Err(AwsError::NoCredentials),
        }
    }
}

/// Provider 3: `aws configure get ...` (CLI v1 static keys).
pub struct CliGetProvider {
    runner: Rc<dyn CommandRunner>,
}

impl CliGetProvider {
    /// Provider over a command runner.
    #[must_use]
    pub fn new(runner: Rc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// `aws configure get <key>`: trimmed stdout on status 0, else empty. A
    /// runner failure is `None` (the Python abandons the provider then).
    fn get(&self, key: &str) -> Option<String> {
        let output = self
            .runner
            .run("aws", &["configure", "get", key], CLI_TIMEOUT)
            .ok()?;
        Some(if output.success {
            output.stdout.trim().to_string()
        } else {
            String::new()
        })
    }
}

impl CredentialsProvider for CliGetProvider {
    fn name(&self) -> &'static str {
        "aws-cli-configure-get"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        let abandon = || AwsError::NoCredentials;
        let access_key = self.get("aws_access_key_id").ok_or_else(abandon)?;
        if access_key.is_empty() {
            return Err(AwsError::NoCredentials);
        }
        let secret_key = self.get("aws_secret_access_key").ok_or_else(abandon)?;
        let token = self.get("aws_session_token").ok_or_else(abandon)?;
        Credentials::new(&access_key, &secret_key, Some(&token), None)
    }
}

/// The standard order for `asm exec`.
#[must_use]
pub fn credential_chain(env: &Rc<dyn Env>, runner: &Rc<dyn CommandRunner>) -> ProviderChain {
    ProviderChain::new()
        .with(Box::new(EnvProvider::new(Rc::clone(env))))
        .with(Box::new(CliExportProvider::new(Rc::clone(runner))))
        .with(Box::new(CliGetProvider::new(Rc::clone(runner))))
}

/// Message when the `SigV4` signer and HTTPS transport are missing.
pub const UNAVAILABLE_MESSAGE: &str = "AWS MCP request signing and the HTTPS transport are not \
available in this build; start the AWS Secrets Manager Agent on localhost, or use the Python \
asm-exec until the SDK adapter lands";

/// Signer for builds without a `SigV4` implementation (this build). It loads
/// credentials first, so a machine without any reports the Python's "no AWS
/// credentials" cause, and then fails closed: **nothing is ever sent unsigned**.
pub struct UnavailableSigner<'a> {
    credentials: &'a dyn CredentialsProvider,
}

impl<'a> UnavailableSigner<'a> {
    /// Signer over a credential provider.
    #[must_use]
    pub fn new(credentials: &'a dyn CredentialsProvider) -> Self {
        Self { credentials }
    }
}

impl Signer for UnavailableSigner<'_> {
    fn sign(&self, _request: &mut HttpRequest, _scope: &SigningScope) -> Result<(), AwsError> {
        self.credentials.load()?;
        Err(AwsError::provider("sigv4", UNAVAILABLE_MESSAGE))
    }
}

/// Transport for builds without TLS (this build): every send fails closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableTransport;

impl Transport for UnavailableTransport {
    fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, AwsError> {
        Err(AwsError::connect(UNAVAILABLE_MESSAGE))
    }
}

#[cfg(test)]
mod tests;
