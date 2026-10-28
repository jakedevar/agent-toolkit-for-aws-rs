//! `toolkit asm exec` — the Rust port of the `aws-secrets-manager` skill's
//! `asm-exec` helper (#4451).
//!
//! `toolkit asm exec [--] COMMAND [ARG...]` replaces every
//! `{{resolve:secretsmanager:ID[:SecretString[:KEY[:STAGE]]]}}` reference in
//! `COMMAND`, its arguments and the exported environment with the secret's
//! value, then runs `COMMAND` with the result and exits with its status. The
//! full contract, the parity table with the Python and the intentional
//! differences are in `docs/migration/asm-exec.md`.
//!
//! ## Secret handling
//!
//! - A resolved value is held in `toolkit_aws::Secret` (fixed `Debug`) and
//!   copied only into the child's argument vector and environment, which is
//!   what the Python did. It is never written to stdout, stderr or any file,
//!   never passed on stdin, and never part of an error.
//! - Every line `asm exec` itself prints to stderr goes through
//!   [`toolkit_aws::redact_text`] in one place ([`emit`]).
//! - Resolution is single-pass: a replacement is never scanned for references.
//! - The Secrets Manager Agent token is only ever sent to a loopback address.
//!
//! ## Collaborators
//!
//! Everything with a side effect is injected through [`Deps`]: the environment,
//! the loopback transport, the MCP signer and transport, DNS and the program
//! launcher. Tests use the doubles from `toolkit_aws::testing`; no test reaches
//! AWS. In this build the MCP signer and the HTTPS transport are the
//! fail-closed [`credentials::UnavailableSigner`]/
//! [`credentials::UnavailableTransport`] pair (see the migration doc).

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::rc::Rc;
use std::time::Duration;

use toolkit_aws::env::non_empty;
use toolkit_aws::{
    redact_text, Env, HostResolver, ProcessEnv, Secret, Signer, SystemResolver, Timeouts, Transport,
};
use toolkit_core::{parse_json, Json, ToolkitError};

use crate::{Invocation, Output};

pub mod credentials;
pub mod loopback;
pub mod mcp;
pub mod process;
pub mod reference;
pub mod sma;

use credentials::{
    credential_chain, CommandRunner, SystemRunner, UnavailableSigner, UnavailableTransport,
};
use loopback::{LoopbackEndpoint, LoopbackHttp};
use mcp::{McpBackend, DEFAULT_ENDPOINT};
use process::{Launcher, SystemLauncher};
use reference::{
    contains_reference, python_json_dumps, substitute, Reference, SubstituteError,
    DEFAULT_FIELD_TYPE,
};
use sma::SmaBackend;

/// Registration of the `asm-exec` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "asm-exec",
    commands: &[crate::CommandSpec {
        path: &["asm", "exec"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: usize::MAX,
        passthrough: true,
        handler: exec_command,
    }],
    synopsis: r"  toolkit asm exec [--] COMMAND [ARG...]
",
    description: r"  asm exec      Replace every {{resolve:secretsmanager:ID[:SecretString[:KEY
                [:STAGE]]]}} reference in COMMAND, its arguments and the
                environment with the secret value, then run COMMAND with the
                result. One optional leading `--` is dropped; every other word,
                dash-prefixed ones included, belongs to COMMAND. Exits with
                COMMAND's status; 1 when a reference cannot be resolved or
                COMMAND cannot be started (nothing is run then). See
                docs/migration/asm-exec.md.
",
    exit_notes: r"  asm exec exits with the status of COMMAND (a command ended by signal N
  exits 256-N); its own failures exit 1, including a missing COMMAND.
",
    doc: "docs/migration/asm-exec.md",
    synopsis_rank: 340,
    description_rank: 330,
};

/// Printed when no command is given.
pub const USAGE_LINE: &str = "Usage: asm-exec <command> [args...]";

/// Default Secrets Manager Agent endpoint.
pub const DEFAULT_SMA_ENDPOINT: &str = "http://localhost:2773";

/// Default MCP request timeout in seconds.
pub const DEFAULT_MCP_TIMEOUT_SECS: f64 = 30.0;

/// Longest MCP timeout accepted (matches `toolkit_aws::transport::MAX_TIMEOUT`).
const MAX_MCP_TIMEOUT_SECS: f64 = 3600.0;

const ENV_SMA_ENDPOINT: &str = "AWS_SECRETS_MANAGER_AGENT_ENDPOINT";
const ENV_MCP_TIMEOUT: &str = "ASM_EXEC_MCP_TIMEOUT";

/// Everything `asm exec` touches outside its own memory.
pub struct Deps<'a> {
    /// Settings: endpoint overrides, session token, region.
    pub env: &'a dyn Env,
    /// The environment the child receives (references in values are resolved).
    pub process_env: Vec<(OsString, OsString)>,
    /// Starts the resolved command.
    pub launcher: &'a dyn Launcher,
    /// Transport for the Secrets Manager Agent (loopback only).
    pub sma_transport: &'a dyn Transport,
    /// Transport for the MCP endpoint.
    pub mcp_transport: &'a dyn Transport,
    /// Signer for the MCP endpoint.
    pub signer: &'a dyn Signer,
    /// DNS for the MCP endpoint check.
    pub resolver: &'a dyn HostResolver,
}

/// Router entry point: wires the real collaborators.
///
/// # Errors
/// Never; failures are reported on stderr and through the exit status
/// ([`Output::set_exit_status`]) exactly as the Python helper did.
pub fn exec_command(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let env = ProcessEnv;
    let shared_env: Rc<dyn Env> = Rc::new(ProcessEnv);
    let runner: Rc<dyn CommandRunner> = Rc::new(SystemRunner);
    let chain = credential_chain(&shared_env, &runner);
    let signer = UnavailableSigner::new(&chain);
    let deps = Deps {
        env: &env,
        process_env: std::env::vars_os().collect(),
        launcher: &SystemLauncher,
        sma_transport: &LoopbackHttp,
        mcp_transport: &UnavailableTransport,
        signer: &signer,
        resolver: &SystemResolver,
    };
    run_with(&deps, &invocation.positionals, output);
    Ok(())
}

/// Write one stderr line, scrubbed. The only way `asm exec` prints.
fn emit(output: &mut Output<'_>, line: &str) {
    // Scrub first, with the original separators in view; then flatten control
    // characters so remote text cannot forge extra lines or terminal escapes.
    let scrubbed: String = redact_text(line)
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    output.diagnostic(&scrubbed);
}

/// Report `line` and ask for exit status 1.
fn fail(output: &mut Output<'_>, line: &str) {
    emit(output, line);
    output.set_exit_status(1);
}

/// Settings read once from the environment.
struct Config {
    sma: Result<LoopbackEndpoint, &'static str>,
    sma_token: String,
    mcp_endpoint: String,
    mcp_timeout_secs: f64,
    mcp_timeouts: Timeouts,
}

impl Config {
    fn load(env: &dyn Env) -> Result<Self, String> {
        let sma_url = env
            .var(ENV_SMA_ENDPOINT)
            .unwrap_or_else(|| DEFAULT_SMA_ENDPOINT.to_string());
        let mcp_endpoint = env
            .var(mcp::ENDPOINT_SETTING)
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        let (mcp_timeout_secs, mcp_timeouts) = parse_timeout(env.var(ENV_MCP_TIMEOUT).as_deref())?;
        Ok(Self {
            sma: LoopbackEndpoint::parse(&sma_url),
            // Python: `AWS_SESSION_TOKEN`, else `AWS_TOKEN`, else none; an
            // empty `AWS_SESSION_TOKEN` is used as is (no fall back).
            sma_token: env
                .var("AWS_SESSION_TOKEN")
                .or_else(|| env.var("AWS_TOKEN"))
                .unwrap_or_default(),
            mcp_endpoint,
            mcp_timeout_secs,
            mcp_timeouts,
        })
    }
}

fn parse_timeout(text: Option<&str>) -> Result<(f64, Timeouts), String> {
    let seconds = match text {
        None => DEFAULT_MCP_TIMEOUT_SECS,
        Some(text) => text.trim().parse::<f64>().map_err(|_| bad_timeout())?,
    };
    if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_MCP_TIMEOUT_SECS {
        return Err(bad_timeout());
    }
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| bad_timeout())?;
    let timeouts = Timeouts::new(duration, duration).map_err(|_| bad_timeout())?;
    Ok((seconds, timeouts))
}

fn bad_timeout() -> String {
    format!(
        "asm-exec: ERROR: {ENV_MCP_TIMEOUT} must be a number of seconds greater than 0 and at \
         most 3600"
    )
}

/// Runs `args` (the words after `asm exec`) with the given collaborators and
/// reports through `output`; the exit status is `output.exit_status()`.
pub fn run_with(deps: &Deps<'_>, args: &[String], output: &mut Output<'_>) {
    // One optional leading `--` (`asm-exec -- command`).
    let command = match args.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => args,
    };
    if command.is_empty() {
        fail(output, USAGE_LINE);
        return;
    }
    let config = match Config::load(deps.env) {
        Ok(config) => config,
        Err(message) => {
            fail(output, &message);
            return;
        }
    };
    let resolver = Resolver::new(deps, &config);
    let prepared = prepare(deps, &resolver, command);
    for line in resolver.take_messages() {
        emit(output, &line);
    }
    let (program, program_args, env) = match prepared {
        Ok(parts) => parts,
        Err(message) => {
            fail(output, &message);
            return;
        }
    };
    match deps.launcher.run(&program, &program_args, &env) {
        Ok(code) => output.set_exit_status(code.0),
        Err(error) => {
            // The unresolved first word: it can hold references, never values.
            let shown = command
                .first()
                .map(String::as_str)
                .map_or_else(String::new, describe);
            fail(
                output,
                &format!("asm-exec: ERROR: Failed to run {shown}: {error}"),
            );
        }
    }
}

/// A command word for a message: each reference replaced by `<reference>` so
/// the text names no secret and the scrubber does not mistake the prefix for
/// a labelled value.
fn describe(word: &str) -> String {
    let replaced = substitute::<()>(word.as_bytes(), &mut |_| Ok(b"<reference>".to_vec()));
    replaced
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| "<command>".to_string())
}

type Prepared = (OsString, Vec<OsString>, Vec<(OsString, OsString)>);

/// Resolve the arguments, then the environment (the Python order). Nothing is
/// started unless both succeed.
fn prepare(
    deps: &Deps<'_>,
    resolver: &Resolver<'_>,
    command: &[String],
) -> Result<Prepared, String> {
    let mut words: Vec<OsString> = Vec::with_capacity(command.len());
    for word in command {
        words.push(resolver.resolve_text(word.as_bytes())?);
    }
    let mut env = Vec::with_capacity(deps.process_env.len());
    for (name, value) in &deps.process_env {
        env.push((name.clone(), resolver.resolve_text(&os_bytes(value))?));
    }
    let mut words = words.into_iter();
    let program = words.next().ok_or_else(|| USAGE_LINE.to_string())?;
    Ok((program, words.collect(), env))
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn os_from_bytes(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// Resolves references; owns the backends and the diagnostics they produce.
struct Resolver<'a> {
    env: &'a dyn Env,
    sma: Option<SmaBackend<'a>>,
    sma_skipped: Cell<Option<&'static str>>,
    mcp: McpBackend<'a>,
    messages: RefCell<Vec<String>>,
}

impl<'a> Resolver<'a> {
    fn new(deps: &Deps<'a>, config: &Config) -> Self {
        let (sma, skipped) = match &config.sma {
            Ok(endpoint) => (
                Some(SmaBackend::new(
                    endpoint.base_url(),
                    &config.sma_token,
                    deps.sma_transport,
                )),
                None,
            ),
            Err(reason) => (None, Some(*reason)),
        };
        Self {
            env: deps.env,
            sma,
            sma_skipped: Cell::new(skipped),
            mcp: McpBackend::new(
                &config.mcp_endpoint,
                config.mcp_timeouts,
                config.mcp_timeout_secs,
                deps.env,
                deps.resolver,
                deps.mcp_transport,
                deps.signer,
            ),
            messages: RefCell::new(Vec::new()),
        }
    }

    fn note(&self, line: String) {
        self.messages.borrow_mut().push(line);
    }

    fn take_messages(&self) -> Vec<String> {
        std::mem::take(&mut *self.messages.borrow_mut())
    }

    /// Substitute the references in one argument or environment value.
    fn resolve_text(&self, input: &[u8]) -> Result<OsString, String> {
        let bytes = if contains_reference(input) {
            substitute(input, &mut |text| {
                self.resolve_reference(text)
                    .map(|secret| secret.expose().as_bytes().to_vec())
            })
            .map_err(|error| match error {
                SubstituteError::Resolver(message) => message,
                SubstituteError::NotUtf8 => {
                    "asm-exec: ERROR: A reference is not valid UTF-8".to_string()
                }
                SubstituteError::TooLarge => {
                    "asm-exec: ERROR: The resolved text is too large".to_string()
                }
            })?
        } else {
            input.to_vec()
        };
        Ok(os_from_bytes(bytes))
    }

    /// `resolve_one` of the Python. `Err` is the line to print before exit 1.
    fn resolve_reference(&self, text: &str) -> Result<Secret, String> {
        let reference = Reference::parse(text);
        if reference.field_type != DEFAULT_FIELD_TYPE {
            return Err(format!(
                "asm-exec: ERROR: Only SecretString is supported, got: {}",
                reference.field_type
            ));
        }
        let region = reference
            .arn_region()
            .map(str::to_string)
            .or_else(|| non_empty(self.env, "AWS_REGION"))
            .or_else(|| non_empty(self.env, "AWS_DEFAULT_REGION"));

        let mut value = self.agent_value(&reference);
        if value.is_none() {
            value = match self.mcp.resolve(
                &reference.secret_id,
                &reference.version_stage,
                region.as_deref(),
            ) {
                Ok(secret) => Some(secret),
                Err(reason) => {
                    self.note(format!("asm-exec: MCP resolution failed: {reason}"));
                    None
                }
            };
        }
        let Some(value) = value.filter(|secret| !secret.is_empty()) else {
            return Err(format!("asm-exec: ERROR: Failed to resolve: {text}"));
        };
        match reference.json_key.as_deref() {
            None => Ok(value),
            Some(key) => pick_json_key(&value, key).ok_or_else(|| {
                format!(
                    "asm-exec: ERROR: JSON key '{key}' not found in: {}",
                    reference.secret_id
                )
            }),
        }
    }

    fn agent_value(&self, reference: &Reference) -> Option<Secret> {
        if let Some(reason) = self.sma_skipped.take() {
            self.note(format!(
                "asm-exec: Secrets Manager Agent not used: {reason} \
                 ({ENV_SMA_ENDPOINT})"
            ));
        }
        let sma = self.sma.as_ref()?;
        if !sma.is_available() {
            return None;
        }
        sma.get(&reference.secret_id, &reference.version_stage)
    }
}

/// `json.loads(value)[key]`, a string kept as is and anything else rendered by
/// `json.dumps`. `None` for invalid JSON, a non-object or a missing key.
fn pick_json_key(value: &Secret, key: &str) -> Option<Secret> {
    let parsed = parse_json(value.expose()).ok()?;
    match parsed.get(key)? {
        Json::String(text) => Some(Secret::new(text.as_str())),
        other => Some(Secret::new(python_json_dumps(other))),
    }
}

#[cfg(test)]
mod tests;
