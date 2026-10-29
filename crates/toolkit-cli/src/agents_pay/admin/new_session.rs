//! `agents-pay admin new-session` (legacy `cmd_new_session`): the terminal
//! first, then the argument checks (D29), the STS check and the
//! resolution, then a summary and the literal `approve` (D28). No `--yes`.

use toolkit_core::ToolkitError;

use super::setup_openclaw::{fixed, parse_positive_decimal, ABORTED};
use super::{
    admin_config_path, api_failure, ask_approval, client_token, credentials_ok, deployed, int_flag,
    open_api, refuse, required, resolve_manager_arn, resolve_region, resolve_user_id,
    response_text, update_resources, AdminDeps, Approval, EXPIRY_RANGE, NO_PAYER, NO_RANDOM,
};
use crate::agents_pay::agentcore::PaymentError;
use crate::safe_text::{shown_path, shown_value};
use crate::{Invocation, Output};

/// The legacy headless refusal.
pub const HEADLESS: &str = "Refusing to create a payment session without an interactive terminal.\nSession creation requires a human typing 'approve' at a TTY. There is no\nnon-interactive mode: that would let an automated caller mint spending budget.";

/// D29: the budget is a positive decimal and the expiry is in 15..=480.
///
/// # Errors
/// The fixed D29 text.
pub fn check_budget_and_expiry(budget: &str, expiry: i64) -> Result<String, String> {
    let value = parse_positive_decimal(budget, "--budget")?;
    if !EXPIRY_RANGE.contains(&expiry) {
        return Err("--expiry-minutes must be between 15 and 480.".to_owned());
    }
    Ok(fixed(&value))
}

fn manager_missing(checked: &str) -> String {
    format!(
        "Could not determine the payment manager ARN.\n\nSearched for deployed-state.json at: {checked}\n\nThis file is created by `agentcore deploy`. To resolve:\n  \u{2022} Run this command from the directory that CONTAINS the agentcore/ folder, OR\n  \u{2022} Run from inside the agentcore/ directory itself, OR\n  \u{2022} Pass --manager-arn explicitly, OR\n  \u{2022} Set PAYMENT_MANAGER_ARN in your environment."
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
    let budget_text = required(invocation, "--budget")?;
    let expiry = int_flag(invocation, "--expiry-minutes", 60)?;
    let budget = match check_budget_and_expiry(budget_text, expiry) {
        Ok(budget) => budget,
        Err(reason) => {
            refuse(out, &reason);
            return Ok(());
        }
    };
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
    let Some(manager) = resolve_manager_arn(invocation.value("--manager-arn"), &path, deps) else {
        refuse(
            out,
            &manager_missing(&deployed::checked(&deployed::candidates(None, deps))),
        );
        return Ok(());
    };
    if let Err(reason) = deps.api.check_manager(&manager) {
        refuse(out, &reason);
        return Ok(());
    }
    out.line("About to create a payment session:")?;
    out.line(&format!("  manager   : {}", shown_value(&manager)))?;
    out.line(&format!("  payer     : {}", shown_value(&user)))?;
    out.line(&format!(
        "  budget    : {budget} USD (hard cap for the whole session)"
    ))?;
    out.line(&format!("  expires in: {expiry} minutes"))?;
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
    let session = match api.create_session(&manager, &user, &budget, expiry, &token) {
        Ok(session) => session,
        Err(error) => {
            api_failure(out, "CreatePaymentSession", error);
            return Ok(());
        }
    };
    let Some(session_id) = response_text(&session, "paymentSession", "paymentSessionId") else {
        api_failure(out, "CreatePaymentSession", PaymentError::InvalidResponse);
        return Ok(());
    };
    let shown_session = shown_value(&session_id).into_owned();
    if let Err(reason) = update_resources(
        &path,
        &[
            ("payment_session_id", Some(session_id.as_str())),
            ("user_id", Some(user.as_str())),
        ],
        deps,
    ) {
        refuse(
            out,
            &format!("Session {shown_session} was created, but {reason}"),
        );
        return Ok(());
    }
    out.line(&format!("\nPayment session created: {shown_session}"))?;
    out.line(&format!(
        "Recorded in            : {}  (nothing to copy by hand)",
        shown_path(&path)
    ))?;
    out.line(&format!("\nThis session allows {budget} USD of CUMULATIVE spend. Each individual\npayment is additionally capped by max_per_payment_usd in the policy section \u{2014}\nrun show-config to see it. Both bounds apply."))?;
    out.line("\nWhen this budget is spent, the agent CANNOT mint another session \u{2014} by design.\nRe-run this command yourself to authorize more spending.")?;
    Ok(())
}
