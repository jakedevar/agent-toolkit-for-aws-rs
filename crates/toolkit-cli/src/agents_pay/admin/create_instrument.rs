//! `agents-pay admin create-instrument` (legacy `cmd_create_instrument`,
//! with the D33 gate): the terminal first, then the input checks, the STS
//! check and the resolution, then a summary and the literal `approve`.

use toolkit_core::ToolkitError;

use super::{
    admin_config_path, api_failure, ask_approval, client_token, credentials_ok, deployed,
    open_api, or_none, refuse, required, resolve_manager_arn, resolve_region, resolve_user_id,
    response_text, update_resources, wallet_of, AdminDeps, Approval, NO_PAYER, NO_RANDOM,
};
use crate::safe_text::{shown_path, shown_value};
use crate::{Invocation, Output};

/// The D33 headless refusal.
pub const HEADLESS: &str =
    "create-instrument must be run by a human at a terminal; it creates a wallet user.";

/// The D33 abort text (also at EOF, the D28 rule).
pub const ABORTED: &str = "Aborted. No instrument created.";

/// The network families the model admits.
pub const NETWORK_FAMILIES: [&str; 2] = ["ETHEREUM", "SOLANA"];

/// The missing manager or connector text.
pub(super) fn not_found(checked: &str) -> String {
    format!(
        "Could not find deployed-state.json (checked: {checked}).\n\nThis file is created by `agentcore deploy`. To resolve:\n  \u{2022} Run this command from the directory that CONTAINS the agentcore/ folder, OR\n  \u{2022} Run from inside the agentcore/ directory itself, OR\n  \u{2022} Pass --manager-arn and --connector-id explicitly."
    )
}

#[allow(clippy::too_many_lines)] // The legacy command, one step after another.
pub(super) fn run(
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if !deps.terminal.is_tty() {
        refuse(out, HEADLESS);
        return Ok(());
    }
    let email = required(invocation, "--email")?;
    let family = invocation.value("--network-family").unwrap_or("ETHEREUM");
    if !NETWORK_FAMILIES.contains(&family) {
        refuse(out, "--network-family must be ETHEREUM or SOLANA.");
        return Ok(());
    }
    let path = admin_config_path(invocation.value("--path"), deps);
    let region = resolve_region(invocation.value("--region"), &path, deps);
    let Some(api) = open_api(deps, region.as_deref(), out) else {
        return Ok(());
    };
    if !credentials_ok(api.as_ref(), out) {
        return Ok(());
    }
    let Some(user) = resolve_user_id(invocation.value("--user-id"), &path, deps) else {
        refuse(out, NO_PAYER);
        return Ok(());
    };
    let discovered = deployed::discover(deps, None);
    let manager = resolve_manager_arn(invocation.value("--manager-arn"), &path, deps);
    let connector = invocation
        .value("--connector-id")
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            deps.env
                .var("PAYMENT_CONNECTOR_ID")
                .filter(|c| !c.is_empty())
        })
        .or_else(|| discovered.connector_id.filter(|c| !c.is_empty()));
    let (Some(manager), Some(connector)) = (manager, connector) else {
        refuse(
            out,
            &not_found(&deployed::checked(&deployed::candidates(None, deps))),
        );
        return Ok(());
    };
    if let Err(reason) = deps.api.check_manager(&manager) {
        refuse(out, &reason);
        return Ok(());
    }
    out.line("About to create a payment instrument (a wallet user at the connector):")?;
    out.line(&format!("  email     : {}", shown_value(email)))?;
    out.line(&format!("  network   : {family}"))?;
    out.line(&format!("  manager   : {}", shown_value(&manager)))?;
    out.line(&format!("  connector : {}", shown_value(&connector)))?;
    out.line(&format!("  payer     : {}", shown_value(&user)))?;
    if matches!(
        ask_approval(deps, out, "Type 'approve' to continue: ")?,
        Approval::Refused
    ) {
        out.line(ABORTED)?;
        out.set_exit_status(1);
        return Ok(());
    }
    let Ok(token) = client_token(deps) else {
        refuse(out, NO_RANDOM);
        return Ok(());
    };
    let response = match api.create_instrument(&manager, &user, &connector, family, email, &token)
    {
        Ok(response) => response,
        Err(error) => {
            api_failure(out, "CreatePaymentInstrument", error);
            return Ok(());
        }
    };
    let Some(instrument) = response_text(&response, "paymentInstrument", "paymentInstrumentId")
    else {
        api_failure(
            out,
            "CreatePaymentInstrument",
            crate::agents_pay::agentcore::PaymentError::InvalidResponse,
        );
        return Ok(());
    };
    let (wallet, redirect) = wallet_of(&response);
    let shown_instrument = shown_value(&instrument).into_owned();
    if let Err(reason) = update_resources(
        &path,
        &[
            ("payment_manager_arn", Some(manager.as_str())),
            ("payment_instrument_id", Some(instrument.as_str())),
            ("user_id", Some(user.as_str())),
            ("region", region.as_deref()),
        ],
        deps,
    ) {
        refuse(
            out,
            &format!("Instrument {shown_instrument} was created, but {reason}"),
        );
        return Ok(());
    }
    let wallet = or_none(wallet.as_deref());
    out.line(&format!("Instrument created : {shown_instrument}"))?;
    out.line(&format!("Wallet address     : {wallet}"))?;
    out.line(&format!(
        "Recorded in        : {}  (nothing to copy by hand)",
        shown_path(&path)
    ))?;
    out.line("\nTwo one-time steps remain, both done by the END USER, not the agent:")?;
    match redirect {
        Some(url) => out.line(&format!(
            "  1. Delegation: visit {url}, sign in, grant access to {wallet}"
        ))?,
        None => {
            out.line("  1. Delegation: approve via the Privy frontend SDK")?;
            out.line("     https://github.com/privy-io/aws-agentcore-sdk")?;
        }
    }
    out.line(&format!("  2. Funding   : send testnet USDC to {wallet}"))?;
    out.line("                 https://faucet.circle.com/ (Base Sepolia)")?;
    out.line("\nThen authorize spending:\n  toolkit agents-pay admin new-session --budget 1.00 --expiry-minutes 120")?;
    Ok(())
}
