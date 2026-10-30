//! The trusted admin commands (x402 S6, #5462; design sections 1.4, 1.5,
//! 7 and R-RES-ADM, R-ADM-WRITE).
//!
//! A human runs these, never the agent: `toolkit agents-pay admin
//! {init-config|show-config|create-instrument|new-session|preflight|setup-openclaw}`
//! and `toolkit agents-build setup-payment-user`. Every route obtains its
//! world through one injected [`AdminDeps`] (G1b): the environment, the file
//! system, the terminal, the `AgentCore` admin client and the random source.
//! Only a live build (`agents-pay-live`) has a process world; a build
//! without it routes every admin command to `no_payment`, which prints the
//! D32 text on stderr and exits 1 before any file, variable, credential,
//! deployed state or terminal is read. Tests call [`run_with`] with the
//! [`fixtures`].
//!
//! The creating commands (`create-instrument`, `new-session`,
//! `setup-openclaw`, `setup-payment-user`) check the terminal first, before
//! any argument, dependency or credential check, and need the literal
//! `approve` typed at it (D28, D33). No `--yes` exists.

pub mod deployed;
pub mod fixtures;
pub mod preflight;
pub mod setup_openclaw;
pub mod terminal;
pub mod write;

mod create_instrument;
mod init_config;
mod new_session;
mod setup_payment_user;
mod show_config;

#[cfg(not(feature = "agents-pay-live"))]
mod no_payment;

use std::path::{Path, PathBuf};

use toolkit_core::{Json, ToolkitError};

use super::agentcore::PaymentError;
use super::pyval::{py_str, truthy};
use crate::safe_text::shown_value;
use crate::{Invocation, Output};

pub use terminal::Terminal;

/// The D32 text of an admin route in a build without `agents-pay-live`
/// (stderr, exit 1).
pub const D32_ADMIN_REFUSAL: &str =
    "This toolkit build has no payment support; install a release build of toolkit.";

/// The `AgentCore` admin agent name (R-AWS; the legacy `AGENT_NAME`).
pub const AGENT_NAME: &str = "aws-agents-pay";

/// USDC contract addresses per network, pinned so an operator cannot be
/// tricked into allowlisting a look-alike contract (legacy `KNOWN_USDC`).
pub const KNOWN_USDC: &[(&str, &str)] = &[
    ("eip155:84532", "0x036CbD53842c5426634e7929541eC2318f3dCF7e"),
    ("eip155:8453", "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
];

/// The secret-shaped environment name fragments `preflight` refuses
/// (matched as substrings of the upper-cased name).
pub const SECRET_NAME_FRAGMENTS: &[&str] = &[
    "CDP_API_KEY_SECRET",
    "WALLET_SECRET",
    "APP_SECRET",
    "AUTHORIZATION_PRIVATE_KEY",
];

/// Registration of the `agents-pay admin` commands (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "agents-pay-admin",
    commands: &[
        crate::CommandSpec {
            path: &["agents-pay", "admin", "init-config"],
            flags: &["--allow-any-recipient", "--force"],
            value_flags: &[
                "--max-per-payment-usd",
                "--region",
                "--user-id",
                "--network",
                "--path",
            ],
            strict_values: true,
            repeatable_flags: &["--recipient", "--origin"],
            max_positionals: 0,
            passthrough: false,
            #[cfg(feature = "agents-pay-live")]
            handler: init_config_command,
            #[cfg(not(feature = "agents-pay-live"))]
            handler: self::no_payment::admin_command,
        },
        crate::CommandSpec {
            path: &["agents-pay", "admin", "show-config"],
            flags: &[],
            value_flags: &["--path"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            #[cfg(feature = "agents-pay-live")]
            handler: show_config_command,
            #[cfg(not(feature = "agents-pay-live"))]
            handler: self::no_payment::admin_command,
        },
        crate::CommandSpec {
            path: &["agents-pay", "admin", "create-instrument"],
            flags: &[],
            value_flags: &[
                "--user-id",
                "--email",
                "--network-family",
                "--manager-arn",
                "--connector-id",
                "--region",
                "--path",
            ],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            #[cfg(feature = "agents-pay-live")]
            handler: create_instrument_command,
            #[cfg(not(feature = "agents-pay-live"))]
            handler: self::no_payment::admin_command,
        },
        crate::CommandSpec {
            path: &["agents-pay", "admin", "new-session"],
            flags: &[],
            value_flags: &[
                "--user-id",
                "--budget",
                "--expiry-minutes",
                "--manager-arn",
                "--region",
                "--path",
            ],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            #[cfg(feature = "agents-pay-live")]
            handler: new_session_command,
            #[cfg(not(feature = "agents-pay-live"))]
            handler: self::no_payment::admin_command,
        },
        crate::CommandSpec {
            path: &["agents-pay", "admin", "preflight"],
            flags: &[],
            value_flags: &["--path"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            #[cfg(feature = "agents-pay-live")]
            handler: preflight_command,
            #[cfg(not(feature = "agents-pay-live"))]
            handler: self::no_payment::admin_command,
        },
        crate::CommandSpec {
            path: &["agents-pay", "admin", "setup-openclaw"],
            flags: &[],
            value_flags: &["--path", "--project-dir"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            #[cfg(feature = "agents-pay-live")]
            handler: setup_openclaw_command,
            #[cfg(not(feature = "agents-pay-live"))]
            handler: self::no_payment::admin_command,
        },
    ],
    synopsis: r"  toolkit agents-pay admin init-config (--recipient ADDR ... | --allow-any-recipient)
                           [--max-per-payment-usd USD] [--network CAIP2]
                           [--origin URL ...] [--region R] [--user-id ID]
                           [--path FILE] [--force]
  toolkit agents-pay admin show-config [--path FILE]
  toolkit agents-pay admin create-instrument --email EMAIL [--user-id ID]
                           [--network-family ETHEREUM|SOLANA] [--manager-arn ARN]
                           [--connector-id ID] [--region R] [--path FILE]
  toolkit agents-pay admin new-session --budget USD [--expiry-minutes N]
                           [--user-id ID] [--manager-arn ARN] [--region R]
                           [--path FILE]
  toolkit agents-pay admin preflight [--path FILE]
  toolkit agents-pay admin setup-openclaw [--path FILE] [--project-dir DIR]
",
    description: r"  agents-pay admin init-config|show-config|create-instrument|new-session|preflight|setup-openclaw
                The trusted payment administration commands, run by a human
                and never by an agent: write and show the payment policy,
                create the wallet instrument and the budget-bounded session
                (each only after the literal `approve` is typed at a terminal),
                check the runtime wiring, and the guided OpenClaw setup. A
                build without the agents-pay-live feature has no payment
                support. See docs/migration/agents-pay.md.
",
    exit_notes: r"  agents-pay admin exits 1 on a refusal or a failed AgentCore call.
",
    doc: "docs/migration/agents-pay.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// Registration of `agents-build setup-payment-user` (see `crate::registry`).
pub(crate) const BUILD_SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "agents-build",
    commands: &[crate::CommandSpec {
        path: &["agents-build", "setup-payment-user"],
        flags: &[],
        value_flags: &[
            "--user-id",
            "--email",
            "--budget",
            "--expiry-minutes",
            "--network",
            "--manager-arn",
            "--connector-id",
            "--region",
        ],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        #[cfg(feature = "agents-pay-live")]
        handler: setup_payment_user_command,
        #[cfg(not(feature = "agents-pay-live"))]
        handler: self::no_payment::admin_command,
    }],
    synopsis: r"  toolkit agents-build setup-payment-user --user-id ID --email EMAIL
                           [--budget USD] [--expiry-minutes N] [--network ETHEREUM|SOLANA]
                           [--manager-arn ARN] [--connector-id ID] [--region R]
",
    description: r"  agents-build setup-payment-user
                Provision one end user's AgentCore payment instrument and, with
                --budget, a budget-bounded session, after the literal `approve`
                is typed at a terminal. A build without the agents-pay-live
                feature has no payment support. See docs/migration/agents-pay.md.
",
    exit_notes: "",
    doc: "docs/migration/agents-pay.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// One admin route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminCommand {
    /// `agents-pay admin init-config`.
    InitConfig,
    /// `agents-pay admin show-config`.
    ShowConfig,
    /// `agents-pay admin create-instrument`.
    CreateInstrument,
    /// `agents-pay admin new-session`.
    NewSession,
    /// `agents-pay admin preflight`.
    Preflight,
    /// `agents-pay admin setup-openclaw`.
    SetupOpenclaw,
    /// `agents-build setup-payment-user`.
    SetupPaymentUser,
}

impl AdminCommand {
    /// The route of a command path, for example
    /// `["agents-pay", "admin", "init-config"]`.
    #[must_use]
    pub fn from_path(path: &[&str]) -> Option<Self> {
        Some(match path {
            ["agents-pay", "admin", "init-config"] => Self::InitConfig,
            ["agents-pay", "admin", "show-config"] => Self::ShowConfig,
            ["agents-pay", "admin", "create-instrument"] => Self::CreateInstrument,
            ["agents-pay", "admin", "new-session"] => Self::NewSession,
            ["agents-pay", "admin", "preflight"] => Self::Preflight,
            ["agents-pay", "admin", "setup-openclaw"] => Self::SetupOpenclaw,
            ["agents-build", "setup-payment-user"] => Self::SetupPaymentUser,
            _ => return None,
        })
    }

    /// The headless refusal of a creating route (D33, T-TTY): `None` for
    /// the routes that create nothing. [`run_process`] prints it before it
    /// builds any world, and each creating handler checks it again first.
    #[must_use]
    pub fn headless_refusal(self) -> Option<&'static str> {
        match self {
            Self::CreateInstrument => Some(create_instrument::HEADLESS),
            Self::NewSession => Some(new_session::HEADLESS),
            Self::SetupOpenclaw => Some(setup_openclaw::HEADLESS),
            Self::SetupPaymentUser => Some(setup_payment_user::HEADLESS),
            Self::InitConfig | Self::ShowConfig | Self::Preflight => None,
        }
    }
}

/// The process environment (`AGENTS_PAY_CONFIG`, `HOME`, `AWS_REGION`, the
/// `PAYMENT_*` fallbacks; `preflight`'s secret-name scan).
pub trait AdminEnv {
    /// One variable.
    fn var(&self, name: &str) -> Option<String>;
    /// Every variable name, for the `preflight` scan.
    fn names(&self) -> Vec<String>;
}

/// Ownership and permission bits of one path (`lstat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    /// `st_mode`.
    pub mode: u32,
    /// `st_uid`.
    pub uid: u32,
}

/// The file system of the admin routes: the admin config (lenient read,
/// atomic write), deployed state, and the runtime-checked policy read of
/// `preflight`. A relative path is relative to [`AdminFs::cwd`].
pub trait AdminFs {
    /// The working directory.
    fn cwd(&self) -> PathBuf;
    /// The OS-account home, used when `HOME` is unset (`Path.home()`).
    fn account_home(&self) -> Option<PathBuf>;
    /// Whether the path exists (following symlinks).
    fn exists(&self, path: &Path) -> bool;
    /// Whether the path is a directory (following symlinks).
    fn is_dir(&self, path: &Path) -> bool;
    /// The whole file as text, at most 1 MiB; `None` when unreadable.
    fn read(&self, path: &Path) -> Option<String>;
    /// `lstat` of the path.
    fn lstat(&self, path: &Path) -> Option<FileMeta>;
    /// The real user id.
    fn uid(&self) -> u32;
    /// Write the admin config atomically at 0600 in a 0700 directory
    /// (R-ADM-WRITE).
    ///
    /// # Errors
    /// A fixed text naming the path.
    fn write_config(&self, path: &Path, content: &str) -> Result<(), String>;
    /// The runtime's checked policy load (R-CFG) of `preflight`.
    ///
    /// # Errors
    /// The runtime refusal text.
    fn load_runtime_policy(&self, path: &Path) -> Result<Json, String>;
}

/// The `AgentCore` admin client for one region.
pub trait AdminApi {
    /// The read-only STS `GetCallerIdentity` check.
    ///
    /// # Errors
    /// A sanitized failure.
    fn sts_check(&self) -> Result<(), PaymentError>;
    /// `CreatePaymentInstrument`: creates a wallet user at the connector.
    ///
    /// # Errors
    /// A sanitized failure.
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError>;
    /// `CreatePaymentSession`: a budget-bounded session.
    ///
    /// # Errors
    /// A sanitized failure.
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError>;
}

/// Opens the admin client.
pub trait AdminConnector {
    /// A client for `region` (`None`: the SDK chain).
    ///
    /// # Errors
    /// A fixed text (for example the R-ROOTS one).
    fn open<'s>(&'s self, region: Option<&str>) -> Result<Box<dyn AdminApi + 's>, String>;
    /// Refuse a manager before anything is created (G3 in a live build).
    ///
    /// # Errors
    /// A fixed text.
    fn check_manager(&self, manager: &str) -> Result<(), String> {
        let _ = manager;
        Ok(())
    }
}

/// No randomness is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoRandomness;

/// The random source: generated user ids and client tokens.
pub trait AdminRandom {
    /// Fill `buf`.
    ///
    /// # Errors
    /// When no randomness is available.
    fn fill(&self, buf: &mut [u8]) -> Result<(), NoRandomness>;
}

/// Everything an admin route touches outside its own memory (G1b).
#[derive(Clone, Copy)]
pub struct AdminDeps<'a> {
    /// The environment.
    pub env: &'a dyn AdminEnv,
    /// The file system.
    pub fs: &'a dyn AdminFs,
    /// The terminal.
    pub terminal: &'a dyn Terminal,
    /// The `AgentCore` admin client.
    pub api: &'a dyn AdminConnector,
    /// Randomness.
    pub random: &'a dyn AdminRandom,
}

/// Run one admin route over an injected world (G1b): what the live
/// registry handlers run with the process world, and what tests run with
/// the [`fixtures`].
///
/// # Errors
/// A usage error (exit 2) for a missing required or malformed argument; a
/// stdout write failure.
pub fn run_with(
    command: AdminCommand,
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    match command {
        AdminCommand::InitConfig => init_config::run(invocation, deps, out),
        AdminCommand::ShowConfig => show_config::run(invocation, deps, out),
        AdminCommand::CreateInstrument => create_instrument::run(invocation, deps, out),
        AdminCommand::NewSession => new_session::run(invocation, deps, out),
        AdminCommand::Preflight => preflight::run(invocation, deps, out),
        AdminCommand::SetupOpenclaw => setup_openclaw::run(invocation, deps, out),
        AdminCommand::SetupPaymentUser => setup_payment_user::run(invocation, deps, out),
    }
}

/// A built admin world that lends its [`AdminDeps`]: the live process world
/// (`live::AdminWorld`), or a test fixture.
pub trait AdminWorldSource {
    /// The injected world of the admin routes.
    fn deps(&self) -> AdminDeps<'_>;
}

/// Run one admin route the way the live registry handlers do (#5475): a
/// creating route checks `terminal` first and refuses headless before
/// `build` runs, so no roots, runtime, transport, file, variable or
/// argument is touched (design section 7, T-TTY, D33). Only then is the
/// world built; a build failure prints its reason and exits 1.
///
/// # Errors
/// What [`run_with`] returns.
pub fn run_process<W: AdminWorldSource>(
    command: AdminCommand,
    invocation: &Invocation,
    terminal: &dyn Terminal,
    build: impl FnOnce() -> Result<W, super::deps::Unavailable>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if let Some(headless) = command.headless_refusal() {
        if !terminal.is_tty() {
            refuse(out, headless);
            return Ok(());
        }
    }
    match build() {
        Ok(world) => run_with(command, invocation, &world.deps(), out),
        Err(super::deps::Unavailable(reason)) => {
            refuse(out, &reason);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- live routes

#[cfg(feature = "agents-pay-live")]
fn live_command(
    command: AdminCommand,
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    run_process(
        command,
        invocation,
        &super::live::LiveTerminal,
        super::live::AdminWorld::for_process,
        out,
    )
}

#[cfg(feature = "agents-pay-live")]
fn init_config_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    live_command(AdminCommand::InitConfig, invocation, out)
}

#[cfg(feature = "agents-pay-live")]
fn show_config_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    live_command(AdminCommand::ShowConfig, invocation, out)
}

#[cfg(feature = "agents-pay-live")]
fn create_instrument_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    live_command(AdminCommand::CreateInstrument, invocation, out)
}

#[cfg(feature = "agents-pay-live")]
fn new_session_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    live_command(AdminCommand::NewSession, invocation, out)
}

#[cfg(feature = "agents-pay-live")]
fn preflight_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    live_command(AdminCommand::Preflight, invocation, out)
}

#[cfg(feature = "agents-pay-live")]
fn setup_openclaw_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    live_command(AdminCommand::SetupOpenclaw, invocation, out)
}

#[cfg(feature = "agents-pay-live")]
fn setup_payment_user_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    live_command(AdminCommand::SetupPaymentUser, invocation, out)
}

// ------------------------------------------------------------ shared helpers

/// Refuse with `text` on stderr and exit 1.
pub(super) fn refuse(out: &mut Output<'_>, text: &str) {
    out.diagnostic(text);
    out.set_exit_status(1);
}

/// D31: an `AgentCore` or STS failure as `"<Operation> failed: <code>"`.
pub(super) fn api_failure(out: &mut Output<'_>, operation: &str, error: PaymentError) {
    refuse(out, &format!("{operation} failed: {}", error_code(error)));
}

/// The allowlisted service code, or the fixed class name (R-AWS-CODE, R-ERR).
#[must_use]
pub fn error_code(error: PaymentError) -> &'static str {
    match error {
        PaymentError::Service(code) => code,
        other => other.class_name(),
    }
}

/// A required value flag, as argparse reports a missing one (exit 2).
pub(super) fn required<'i>(
    invocation: &'i Invocation,
    flag: &str,
) -> Result<&'i str, ToolkitError> {
    invocation.value(flag).ok_or_else(|| {
        ToolkitError::usage(
            "invalid_arguments",
            format!("the following arguments are required: {flag}"),
        )
    })
}

/// An argparse `type=int` value flag with its default (exit 2 when malformed).
pub(super) fn int_flag(
    invocation: &Invocation,
    flag: &str,
    default: i64,
) -> Result<i64, ToolkitError> {
    match invocation.value(flag) {
        None => Ok(default),
        Some(text) => super::pyval::py_int(&Json::string(text)).map_err(|_| {
            ToolkitError::usage(
                "invalid_arguments",
                format!(
                    "argument {flag}: invalid int value: '{}'",
                    shown_value(text)
                ),
            )
        }),
    }
}

/// D29: the model's `expiryTimeInMinutes` range.
pub const EXPIRY_RANGE: std::ops::RangeInclusive<i64> = 15..=480;

/// The admin config path: `--path` > `AGENTS_PAY_CONFIG` >
/// `$HOME/.agents-pay/config.json` (R-RES-ADM; `HOME`, then the OS account).
#[must_use]
pub fn admin_config_path(explicit: Option<&str>, deps: &AdminDeps<'_>) -> PathBuf {
    if let Some(path) = explicit.filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = deps.env.var("AGENTS_PAY_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    home(deps).join(".agents-pay/config.json")
}

/// `Path.home()`: `HOME`, then the OS account, then the working directory.
#[must_use]
pub fn home(deps: &AdminDeps<'_>) -> PathBuf {
    deps.env
        .var("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| deps.fs.account_home())
        .unwrap_or_else(|| deps.fs.cwd())
}

fn skeleton() -> Json {
    Json::object(vec![
        ("resources", Json::Object(vec![])),
        ("policy", Json::Object(vec![])),
    ])
}

/// The admin config for editing (legacy `load_raw_config`): missing,
/// unreadable or invalid gives the empty skeleton; a flat legacy policy is
/// lifted into `policy`; `resources` and `policy` are always present.
#[must_use]
// recorded-shape: the operator's own admin config file, never an AWS response.
pub fn load_raw_config(path: &Path, deps: &AdminDeps<'_>) -> Json {
    let Some(text) = deps.fs.read(path) else {
        return skeleton();
    };
    let Ok(Json::Object(mut fields)) = toolkit_core::parse_json(&text) else {
        return skeleton();
    };
    let has = |fields: &[(String, Json)], key: &str| fields.iter().any(|(k, _)| k == key);
    if !has(&fields, "policy") && !has(&fields, "resources") {
        return Json::object(vec![
            ("resources", Json::Object(vec![])),
            ("policy", Json::Object(fields)),
        ]);
    }
    for key in ["resources", "policy"] {
        if !has(&fields, key) {
            fields.push((key.to_owned(), Json::Object(vec![])));
        }
    }
    Json::Object(fields)
}

/// One `resources` member, when the section is an object.
#[must_use]
// recorded-shape: the operator's own admin config file, never an AWS response.
pub fn resource<'c>(config: &'c Json, key: &str) -> Option<&'c Json> {
    match config.get("resources") {
        Some(Json::Object(fields)) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// A truthy `resources` member as Python `str`.
#[must_use]
pub fn resource_text(config: &Json, key: &str) -> Option<String> {
    resource(config, key).filter(|v| truthy(v)).and_then(py_str)
}

/// A mutable section of the config, replaced by `{}` when it is not an
/// object (Python would have raised there).
// recorded-shape: the operator's own admin config file, never an AWS response.
fn section_mut<'c>(config: &'c mut Json, key: &str) -> &'c mut Vec<(String, Json)> {
    if !matches!(config, Json::Object(_)) {
        *config = skeleton();
    }
    let Json::Object(fields) = config else {
        unreachable!("the config was made an object above")
    };
    let at = if let Some(at) = fields.iter().position(|(k, _)| k == key) {
        at
    } else {
        fields.push((key.to_owned(), Json::Object(vec![])));
        fields.len() - 1
    };
    if !matches!(fields[at].1, Json::Object(_)) {
        fields[at].1 = Json::Object(vec![]);
    }
    let Json::Object(section) = &mut fields[at].1 else {
        unreachable!("the section was made an object above")
    };
    section
}

/// `dict[key] = value` on a section (insertion order kept).
pub(super) fn set_in(config: &mut Json, section: &str, key: &str, value: Json) {
    let fields = section_mut(config, section);
    if let Some(slot) = fields.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        fields.push((key.to_owned(), value));
    }
}

/// `dict.setdefault(key, value)` on a section.
pub(super) fn set_default_in(config: &mut Json, section: &str, key: &str, value: Json) {
    let fields = section_mut(config, section);
    if !fields.iter().any(|(k, _)| k == key) {
        fields.push((key.to_owned(), value));
    }
}

/// Replace a whole section.
pub(super) fn replace_section(config: &mut Json, section: &str, value: Vec<(String, Json)>) {
    *section_mut(config, section) = value;
}

/// A section as written (an absent one is `{}`).
#[must_use]
pub fn section(config: &Json, key: &str) -> Json {
    config
        .get(key)
        .cloned()
        .unwrap_or_else(|| Json::Object(vec![]))
}

/// The file text `save_config` writes: `{"resources", "policy"}` as
/// `json.dumps(indent=2)` plus a newline.
#[must_use]
pub fn config_text(config: &Json) -> String {
    let ordered = Json::object(vec![
        ("resources", section(config, "resources")),
        ("policy", section(config, "policy")),
    ]);
    format!("{}\n", crate::aurora::pyfmt::dumps(&ordered))
}

/// Save the config atomically at 0600 (legacy `save_config`).
///
/// # Errors
/// The fixed write failure text.
pub fn save_config(path: &Path, config: &Json, deps: &AdminDeps<'_>) -> Result<(), String> {
    deps.fs.write_config(path, &config_text(config))
}

/// Merge truthy resource identifiers without touching the policy (legacy
/// `update_resources`).
///
/// # Errors
/// The fixed write failure text.
pub fn update_resources(
    path: &Path,
    values: &[(&str, Option<&str>)],
    deps: &AdminDeps<'_>,
) -> Result<(), String> {
    let mut config = load_raw_config(path, deps);
    for (key, value) in values {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            set_in(&mut config, "resources", key, Json::string(value));
        }
    }
    save_config(path, &config, deps)
}

fn env_text(deps: &AdminDeps<'_>, name: &str) -> Option<String> {
    deps.env.var(name).filter(|v| !v.is_empty())
}

/// Region: flag > `AWS_REGION` > `AWS_DEFAULT_REGION` > config
/// `resources.region` > none (the SDK chain).
#[must_use]
pub fn resolve_region(explicit: Option<&str>, path: &Path, deps: &AdminDeps<'_>) -> Option<String> {
    if let Some(region) = explicit.filter(|r| !r.is_empty()) {
        return Some(region.to_owned());
    }
    env_text(deps, "AWS_REGION")
        .or_else(|| env_text(deps, "AWS_DEFAULT_REGION"))
        .or_else(|| resource_text(&load_raw_config(path, deps), "region"))
}

/// Manager ARN: flag > `PAYMENT_MANAGER_ARN` > config
/// `resources.payment_manager_arn` > deployed state.
#[must_use]
pub fn resolve_manager_arn(
    explicit: Option<&str>,
    path: &Path,
    deps: &AdminDeps<'_>,
) -> Option<String> {
    if let Some(arn) = explicit.filter(|a| !a.is_empty()) {
        return Some(arn.to_owned());
    }
    env_text(deps, "PAYMENT_MANAGER_ARN")
        .or_else(|| resource_text(&load_raw_config(path, deps), "payment_manager_arn"))
        .or_else(|| {
            deployed::discover(deps, None)
                .manager_arn
                .filter(|a| !a.is_empty())
        })
}

/// User id: flag > config `resources.user_id` > `PAYMENT_USER_ID`.
#[must_use]
pub fn resolve_user_id(
    explicit: Option<&str>,
    path: &Path,
    deps: &AdminDeps<'_>,
) -> Option<String> {
    if let Some(user) = explicit.filter(|u| !u.is_empty()) {
        return Some(user.to_owned());
    }
    resource_text(&load_raw_config(path, deps), "user_id")
        .or_else(|| env_text(deps, "PAYMENT_USER_ID"))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A fresh opaque payer id: `agents-pay-` plus 12 hex (legacy
/// `generate_user_id`).
///
/// # Errors
/// When no randomness is available.
pub fn generate_user_id(deps: &AdminDeps<'_>) -> Result<String, NoRandomness> {
    let mut bytes = [0u8; 6];
    deps.random.fill(&mut bytes)?;
    Ok(format!("agents-pay-{}", hex(&bytes)))
}

/// A UUID4 `clientToken`, one per command (section 6.1).
///
/// # Errors
/// When no randomness is available.
pub fn client_token(deps: &AdminDeps<'_>) -> Result<String, NoRandomness> {
    let mut b = [0u8; 16];
    deps.random.fill(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// The randomness failure text.
pub(super) const NO_RANDOM: &str = "No randomness is available; nothing was created.";

/// The STS check before any prompt (legacy `_check_aws_credentials`).
/// `false` after printing the refusal.
pub(super) fn credentials_ok(api: &dyn AdminApi, out: &mut Output<'_>) -> bool {
    match api.sts_check() {
        Ok(()) => true,
        Err(error) => {
            refuse(
                out,
                &format!(
                    "AWS credentials are invalid, expired, or missing.\n  ({})\n\nRun `aws sso login` (or otherwise refresh your credentials), then re-run this command.",
                    error_code(error)
                ),
            );
            false
        }
    }
}

/// Open the client, printing the refusal when it cannot be opened.
pub(super) fn open_api<'d>(
    deps: &AdminDeps<'d>,
    region: Option<&str>,
    out: &mut Output<'_>,
) -> Option<Box<dyn AdminApi + 'd>> {
    match deps.api.open(region) {
        Ok(api) => Some(api),
        Err(reason) => {
            refuse(out, &reason);
            None
        }
    }
}

/// The payer-identity refusal.
pub(super) const NO_PAYER: &str = "No payer identity found. Run init-config first (it generates one), or pass --user-id explicitly.";

/// What a creating command's typed approval gave.
pub(super) enum Approval {
    /// Exactly `approve`.
    Approved,
    /// Anything else, or EOF (D28).
    Refused,
}

/// Ask for the literal `approve` (D28, D33): the line without its
/// terminator must be exactly `approve`; EOF refuses.
///
/// # Errors
/// A stdout write failure.
pub(super) fn ask_approval(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
    prompt: &str,
) -> Result<Approval, ToolkitError> {
    out.prompt(prompt)?;
    Ok(match deps.terminal.read_line() {
        Some(line) if line == "approve" => Approval::Approved,
        _ => Approval::Refused,
    })
}

/// One wizard answer, stripped (`input().strip()`); `None` at EOF.
///
/// # Errors
/// A stdout write failure.
pub(super) fn ask(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
    prompt: &str,
) -> Result<Option<String>, ToolkitError> {
    out.prompt(prompt)?;
    Ok(deps.terminal.read_line().map(|line| {
        line.trim_matches(super::pyval::is_python_whitespace)
            .to_owned()
    }))
}

/// A response member through the shared recorded readers: the
/// `paymentInstrument` or `paymentSession` object, then a text member.
pub(super) fn response_text(response: &Json, object: &str, key: &str) -> Option<String> {
    let inner = toolkit_aws::recorded::optional_object(response, object)
        .ok()
        .flatten()?;
    toolkit_aws::recorded::optional_text(inner, key)
        .ok()
        .flatten()
        .map(str::to_owned)
}

/// The wallet address and delegation URL of an instrument response.
pub(super) fn wallet_of(response: &Json) -> (Option<String>, Option<String>) {
    let wallet = toolkit_aws::recorded::optional_object(response, "paymentInstrument")
        .ok()
        .flatten()
        .and_then(|i| {
            toolkit_aws::recorded::optional_object(i, "paymentInstrumentDetails")
                .ok()
                .flatten()
        })
        .and_then(|d| {
            toolkit_aws::recorded::optional_object(d, "embeddedCryptoWallet")
                .ok()
                .flatten()
        });
    let text = |key: &str| {
        wallet
            .and_then(|w| toolkit_aws::recorded::optional_text(w, key).ok().flatten())
            .map(|v| shown_value(v).into_owned())
    };
    (text("walletAddress"), text("redirectUrl"))
}

/// Python `str(None)` for an optional printed value.
pub(super) fn or_none(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), |v| shown_value(v).into_owned())
}

/// The admin config write failure text.
#[must_use]
pub(in crate::agents_pay) fn write_failure(path: &Path) -> String {
    format!(
        "Cannot write the payment config {}.",
        crate::safe_text::shown_path(path)
    )
}
