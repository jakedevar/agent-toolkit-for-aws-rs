//! `agents-build setup-payment-user` (legacy `setup_payment_user.py`, with
//! the D33 gate): the terminal first, then the input checks (D29 for
//! `--budget`), the resolution and the STS check, then one summary and one
//! literal `approve` covering the instrument and the optional session.
//! D27: its single deployed-state candidate is read by the shared parser,
//! so the 0.26.x layout is accepted.

use std::path::PathBuf;

use toolkit_core::ToolkitError;

use super::create_instrument::{ABORTED, NETWORK_FAMILIES};
use super::new_session::check_budget_and_expiry;
use super::{
    api_failure, ask_approval, client_token, credentials_ok, deployed, int_flag, open_api, or_none,
    refuse, required, response_text, wallet_of, AdminDeps, Approval, NO_RANDOM,
};
use crate::agents_pay::agentcore::PaymentError;
use crate::safe_text::shown_value;
use crate::{Invocation, Output};

/// The D33 headless refusal.
pub const HEADLESS: &str =
    "setup-payment-user must be run by a human at a terminal; it creates a wallet user.";

/// The legacy resolution failure.
pub const UNRESOLVED: &str = "Could not resolve manager ARN / connector ID. Pass --manager-arn and --connector-id, or run from the project dir with agentcore/.cli/deployed-state.json present.";

fn flag_or_env(
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    flag: &str,
    env: &str,
) -> Option<String> {
    invocation
        .value(flag)
        .map(str::to_owned)
        .or_else(|| deps.env.var(env))
        .filter(|v| !v.is_empty())
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
    let user = required(invocation, "--user-id")?;
    let email = required(invocation, "--email")?;
    let expiry = int_flag(invocation, "--expiry-minutes", 60)?;
    let network = invocation.value("--network").unwrap_or("ETHEREUM");
    if !NETWORK_FAMILIES.contains(&network) {
        refuse(out, "--network must be ETHEREUM or SOLANA.");
        return Ok(());
    }
    let budget = match invocation.value("--budget").filter(|b| !b.is_empty()) {
        None => None,
        Some(text) => match check_budget_and_expiry(text, expiry) {
            Ok(budget) => Some(budget),
            Err(reason) => {
                refuse(out, &reason);
                return Ok(());
            }
        },
    };
    let mut manager = flag_or_env(invocation, deps, "--manager-arn", "PAYMENT_MANAGER_ARN");
    let mut connector = flag_or_env(invocation, deps, "--connector-id", "PAYMENT_CONNECTOR_ID");
    let region = flag_or_env(invocation, deps, "--region", "AWS_REGION")
        .unwrap_or_else(|| "us-east-1".to_owned());
    if manager.is_none() || connector.is_none() {
        let found = deployed::discover_in(
            &[PathBuf::from(deployed::SETUP_PAYMENT_USER_CANDIDATE)],
            deps,
        );
        manager = manager.or(found.manager_arn.filter(|m| !m.is_empty()));
        connector = connector.or(found.connector_id.filter(|c| !c.is_empty()));
    }
    let (Some(manager), Some(connector)) = (manager, connector) else {
        refuse(out, UNRESOLVED);
        return Ok(());
    };
    if let Err(reason) = deps.api.check_manager(&manager) {
        refuse(out, &reason);
        return Ok(());
    }
    let Some(api) = open_api(deps, Some(region.as_str()), out) else {
        return Ok(());
    };
    if !credentials_ok(api.as_ref(), out) {
        return Ok(());
    }
    out.line("About to create a payment instrument (a wallet user at the connector):")?;
    out.line(&format!("  email     : {}", shown_value(email)))?;
    out.line(&format!("  network   : {network}"))?;
    out.line(&format!("  manager   : {}", shown_value(&manager)))?;
    out.line(&format!("  connector : {}", shown_value(&connector)))?;
    out.line(&format!("  user      : {}", shown_value(user)))?;
    if let Some(budget) = &budget {
        out.line(&format!(
            "and a payment session of {budget} USD expiring in {expiry} minutes."
        ))?;
    }
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
    let response = match api.create_instrument(&manager, user, &connector, network, email, &token) {
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
            PaymentError::InvalidResponse,
        );
        return Ok(());
    };
    let instrument = shown_value(&instrument).into_owned();
    let (wallet, redirect) = wallet_of(&response);
    let wallet = or_none(wallet.as_deref());
    let mut session_id = None;
    if let Some(budget) = &budget {
        let Ok(token) = client_token(deps) else {
            refuse(out, NO_RANDOM);
            return Ok(());
        };
        match api.create_session(&manager, user, budget, expiry, &token) {
            Ok(session) => {
                let Some(id) = response_text(&session, "paymentSession", "paymentSessionId") else {
                    out.line(&format!("Instrument ID : {instrument}"))?;
                    api_failure(out, "CreatePaymentSession", PaymentError::InvalidResponse);
                    return Ok(());
                };
                session_id = Some(shown_value(&id).into_owned());
            }
            Err(error) => {
                out.line(&format!("Instrument ID : {instrument}"))?;
                api_failure(out, "CreatePaymentSession", error);
                return Ok(());
            }
        }
    }
    out.line(&format!("Instrument ID : {instrument}"))?;
    out.line(&format!("Wallet address: {wallet}"))?;
    out.line(&format!(
        "Session ID    : {}",
        session_id
            .as_deref()
            .unwrap_or("(none - use `agentcore invoke --auto-session`)")
    ))?;
    out.line("\nExport these for the x402 tool (Step 8):")?;
    out.line(&format!(
        "  export PAYMENT_MANAGER_ARN={}",
        shown_value(&manager)
    ))?;
    out.line(&format!("  export PAYMENT_INSTRUMENT_ID={instrument}"))?;
    if let Some(session) = &session_id {
        out.line(&format!("  export PAYMENT_SESSION_ID={session}"))?;
    }
    out.line(&format!("  export PAYMENT_USER_ID={}", shown_value(user)))?;
    out.line(&format!("  export AWS_REGION={}", shown_value(&region)))?;
    out.line("\nOne-time per wallet:")?;
    if let Some(url) = redirect {
        out.line(&format!(
            "  1. Delegation (Coinbase): visit {url}, log in, grant access to {wallet}"
        ))?;
    } else {
        out.line("  1. Delegation (Privy): approve delegation via the Privy frontend SDK")?;
    }
    out.line(&format!(
        "  2. Funding: send testnet USDC to {wallet} via https://faucet.circle.com/ (Base Sepolia)"
    ))?;
    Ok(())
}
