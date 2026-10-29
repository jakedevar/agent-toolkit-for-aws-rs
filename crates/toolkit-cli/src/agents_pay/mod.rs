//! x402 payments (#4410), ported from `plugins/aws-agents/skills/agents-pay`.
//!
//! Design: `docs/migration/x402-payments-design.md`. The slices so far:
//!
//! - S1 (#5118), the pure gate: [`urlparse`], [`amount`], [`pyval`],
//!   [`challenge`] and [`policy`] with the [`policy::Approved`] value every
//!   spending call consumes (R-SPEND-ORDER);
//! - S2 [`config`], S3 [`net`] (the merchant plane) and S4 [`agentcore`]
//!   (the `AgentCore` client);
//! - S5 (#5352): [`proof`] (R-PP-VERIFY, R-HDR), [`flow`] (`fetch`,
//!   `--status`, `--browser-handle`), [`vault`], [`deps`] (the injected world
//!   and D32), `wiring` (`aws-sdk`), `live` (`agents-pay-live` only, G1) and
//!   the `toolkit agents-pay fetch` route.

pub mod amount;
pub mod challenge;
pub mod config;
pub mod policy;
pub mod pyval;
pub mod urlparse;

pub mod agentcore;
pub mod deps;
pub mod flow;
#[cfg(feature = "agents-pay-live")]
pub mod live;
pub mod net;
pub mod proof;
pub mod vault;
#[cfg(feature = "aws-sdk")]
pub mod wiring;

use toolkit_core::{Json, ToolkitError};

use crate::{Invocation, Output};
use deps::Deps;
#[cfg(feature = "agents-pay-live")]
use deps::Unavailable;

/// Registration of the `agents-pay` slice (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "agents-pay",
    commands: &[crate::CommandSpec {
        path: &["agents-pay", "fetch"],
        flags: &["--status"],
        value_flags: &["--browser-handle", "--method", "--purchase-id"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: fetch_command,
    }],
    synopsis: r"  toolkit agents-pay fetch [URL] [--status] [--browser-handle URL]
                           [--method GET|HEAD] [--purchase-id ID]
",
    description: r"  agents-pay fetch
                Fetch an x402-protected https URL, paying only when the
                operator's policy file allows it; --status reports whether the
                payment session can spend; --browser-handle pays and returns an
                opaque single-use handle. Prints one JSON object. A build
                without the agents-pay-live feature has no payment support.
                See docs/migration/agents-pay.md.
",
    exit_notes: r"  agents-pay fetch exits 0 when paid or no payment was needed, 2 on a
  refusal or an unusable session, and 1 when no URL or --status is given.
",
    doc: "docs/migration/agents-pay.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// The legacy usage line (`argparse` `print_usage`), on stderr.
pub const FETCH_USAGE: &str = "usage: toolkit agents-pay fetch [URL] [--status] [--browser-handle URL] [--method GET|HEAD] [--purchase-id ID]";

/// What the `fetch` route was asked to do.
enum Request {
    Status,
    Browser {
        url: String,
        purchase_id: Option<String>,
    },
    Fetch {
        url: String,
        purchase_id: Option<String>,
        method: String,
    },
}

/// Argv to a request: `--method` is checked first (argparse choices),
/// `--status` wins, then `--browser-handle` over the positional URL. `None`
/// after printing the legacy usage error (exit 1).
fn parse(invocation: &Invocation, out: &mut Output<'_>) -> Result<Option<Request>, ToolkitError> {
    let mut words: Vec<String> = invocation.positionals.clone();
    words.extend(invocation.flags.iter().cloned());
    words.extend(invocation.values.iter().map(|(flag, _)| flag.clone()));
    toolkit_aws::reject_credential_arguments(&words).map_err(|_| {
        ToolkitError::usage(
            "credential_argument",
            "agents-pay fetch takes no credential arguments",
        )
    })?;
    let method = invocation.value("--method").unwrap_or("GET");
    if !matches!(method, "GET" | "HEAD") {
        return Err(ToolkitError::usage(
            "invalid_arguments",
            "argument --method: invalid choice (choose from 'GET', 'HEAD')",
        ));
    }
    if invocation.has_flag("--status") {
        return Ok(Some(Request::Status));
    }
    let purchase_id = invocation.value("--purchase-id").map(str::to_owned);
    if let Some(url) = invocation.value("--browser-handle") {
        return Ok(Some(Request::Browser {
            url: url.to_owned(),
            purchase_id,
        }));
    }
    let Some(url) = invocation.positionals.first().filter(|url| !url.is_empty()) else {
        out.diagnostic(FETCH_USAGE);
        out.diagnostic("");
        out.diagnostic("Give a URL, or --status.");
        out.set_exit_status(1);
        return Ok(None);
    };
    Ok(Some(Request::Fetch {
        url: url.clone(),
        purchase_id,
        method: method.to_owned(),
    }))
}

fn emit(out: &mut Output<'_>, document: &Json, status: u8) -> Result<(), ToolkitError> {
    out.line(&flow::render(document))?;
    out.set_exit_status(status);
    Ok(())
}

fn run_request(
    request: &Request,
    deps: &Deps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    match request {
        Request::Status => {
            let document = flow::status(deps);
            emit(out, &document, flow::status_exit_code(&document))
        }
        Request::Browser { url, purchase_id } => {
            let vault = vault::Vault::new();
            let document = flow::prepare_browser_payment(deps, &vault, url, purchase_id.as_deref());
            emit(out, &document, flow::exit_code(&document))
        }
        Request::Fetch {
            url,
            purchase_id,
            method,
        } => {
            let document = flow::fetch(deps, url, purchase_id.as_deref(), method);
            emit(out, &document, flow::exit_code(&document))
        }
    }
}

/// The `fetch` route over an injected world (G1b): what the live registry
/// handler runs with the process world, and what tests run with the
/// [`deps::fixtures`].
///
/// # Errors
/// A usage error for a bad `--method` or a credential argument; a stdout
/// write failure.
pub fn run_with(
    invocation: &Invocation,
    deps: &Deps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    match parse(invocation, out)? {
        Some(request) => run_request(&request, deps, out),
        None => Ok(()),
    }
}

/// The registry handler of a live build: argv first, then the process
/// world, which can still be unavailable (for example R-PROOTS).
#[cfg(feature = "agents-pay-live")]
fn fetch_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let Some(request) = parse(invocation, out)? else {
        return Ok(());
    };
    match Deps::for_process() {
        Ok(world) => run_request(&request, &world.deps(), out),
        Err(Unavailable(reason)) => {
            let document = if matches!(request, Request::Status) {
                Json::object(vec![
                    ("usable", Json::Bool(false)),
                    ("reason", Json::string(reason)),
                ])
            } else {
                Json::object(vec![
                    ("paid", Json::Bool(false)),
                    ("refused", Json::Bool(true)),
                    ("reason", Json::string(reason)),
                ])
            };
            emit(out, &document, 2)
        }
    }
}

/// The whole stdout of `toolkit agents-pay fetch` in a build without
/// `agents-pay-live` (D32), for every argv, `--status` included: the
/// refusal document with [`deps::NO_PAYMENT_SUPPORT`], as [`flow::render`]
/// prints it.
pub const D32_REFUSAL: &str = "{\"paid\": false, \"refused\": true, \"reason\": \"This toolkit build has no payment support; install a release build of toolkit.\"}";

/// The registry handler of a build without `agents-pay-live` (D32): one
/// call to the fixed refusal. Argv is not parsed, no world is built, and no
/// file, variable, credential or terminal is read. `tests/agents_pay_gate.rs`
/// pins this body token for token.
#[cfg(not(feature = "agents-pay-live"))]
fn fetch_command(_invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    refuse_without_payment_support(out)
}

/// D32: print the refusal literal and exit 2. Nothing else runs.
#[cfg(not(feature = "agents-pay-live"))]
fn refuse_without_payment_support(out: &mut Output<'_>) -> Result<(), ToolkitError> {
    out.line(D32_REFUSAL)?;
    out.set_exit_status(2);
    Ok(())
}
