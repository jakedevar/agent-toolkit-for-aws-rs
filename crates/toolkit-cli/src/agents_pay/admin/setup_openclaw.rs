//! `agents-pay admin setup-openclaw`, the guided wizard (legacy
//! `cmd_setup_openclaw`), and the decimal helpers the creating commands
//! share (`parse_positive_decimal`, `usd_to_atomic`, `format_duration`,
//! `build_openclaw_config`; the `test_setup_openclaw.py` contracts, Q12).
//!
//! Amounts go through [`CapDecimal`] (R-AMT-REP): no float, exact
//! digit-string arithmetic. D29: the expiry must be in the model's 15..=480;
//! D30: the expiry prompt names that range; D28: EOF at any prompt aborts.

use std::cmp::Ordering;

use toolkit_core::{Json, ToolkitError};

use super::{
    admin_config_path, api_failure, ask, ask_approval, client_token, credentials_ok, deployed,
    generate_user_id, open_api, or_none, refuse, response_text, save_config, update_resources,
    wallet_of, AdminDeps, Approval, EXPIRY_RANGE, KNOWN_USDC, NO_RANDOM,
};
use crate::agents_pay::agentcore::PaymentError;
use crate::agents_pay::amount::CapDecimal;
use crate::safe_text::{shown_path, shown_value};
use crate::{Invocation, Output};

/// USDC decimals (legacy `USDC_DECIMALS`).
pub const USDC_DECIMALS: i32 = 6;

/// The D28 abort text of the wizard and `new-session`.
pub const ABORTED: &str = "Aborted. No session created.";

/// A human-entered positive decimal (legacy `parse_positive_decimal`).
///
/// # Errors
/// `"<label> must be a decimal number."` or `"<label> must be greater than
/// zero."`.
pub fn parse_positive_decimal(text: &str, label: &str) -> Result<CapDecimal, String> {
    let value = CapDecimal::parse(text).map_err(|_| format!("{label} must be a decimal number."))?;
    if !value.is_positive() {
        return Err(format!("{label} must be greater than zero."));
    }
    Ok(value)
}

fn zeros(count: i32) -> String {
    "0".repeat(usize::try_from(count).unwrap_or(0))
}

/// Python `format(Decimal, 'f')`: fixed-point, every written digit kept.
#[must_use]
pub fn fixed(value: &CapDecimal) -> String {
    let digits = value.digits();
    let exponent = value.exponent();
    let body = if exponent >= 0 {
        if digits == "0" {
            "0".to_owned()
        } else {
            format!("{digits}{}", zeros(exponent))
        }
    } else {
        let places = usize::try_from(-i64::from(exponent)).unwrap_or(usize::MAX);
        if digits.len() > places {
            let (whole, fraction) = digits.split_at(digits.len() - places);
            format!("{whole}.{fraction}")
        } else {
            format!("0.{}{digits}", "0".repeat(places - digits.len()))
        }
    };
    if value.is_negative() {
        format!("-{body}")
    } else {
        body
    }
}

/// Exact atomic units (legacy `usd_to_atomic`).
///
/// # Errors
/// `"Max per-payment USD supports at most 6 decimal places."`.
pub fn usd_to_atomic(value: &CapDecimal) -> Result<String, String> {
    let digits = value.digits();
    let shift = value.exponent() + USDC_DECIMALS;
    if digits == "0" {
        return Ok("0".to_owned());
    }
    if shift >= 0 {
        return Ok(format!("{digits}{}", zeros(shift)));
    }
    let cut = usize::try_from(-i64::from(shift)).unwrap_or(usize::MAX);
    match digits.len().checked_sub(cut) {
        Some(keep) if keep > 0 && digits[keep..].bytes().all(|b| b == b'0') => {
            Ok(digits[..keep].to_owned())
        }
        _ => Err(format!(
            "Max per-payment USD supports at most {USDC_DECIMALS} decimal places."
        )),
    }
}

/// `a` compared with `b` (both non-negative), exactly.
#[must_use]
pub fn compare(a: &CapDecimal, b: &CapDecimal) -> Ordering {
    let low = a.exponent().min(b.exponent());
    let scaled = |v: &CapDecimal| {
        let text = format!("{}{}", v.digits(), zeros(v.exponent() - low));
        let trimmed = text.trim_start_matches('0');
        if trimmed.is_empty() {
            "0".to_owned()
        } else {
            trimmed.to_owned()
        }
    };
    let (left, right) = (scaled(a), scaled(b));
    left.len().cmp(&right.len()).then_with(|| left.cmp(&right))
}

/// Minutes with an hours hint for whole hours (legacy `format_duration`).
#[must_use]
pub fn format_duration(minutes: i64) -> String {
    if minutes % 60 == 0 {
        let hours = minutes / 60;
        format!(
            "{minutes} minutes ({hours} hour{})",
            if hours == 1 { "" } else { "s" }
        )
    } else {
        format!("{minutes} minutes")
    }
}

/// The validated wizard inputs of the `OpenClaw` plugin configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenClawInputs {
    /// AWS region.
    pub region: String,
    /// Payment manager ARN.
    pub manager_arn: String,
    /// Payment instrument id.
    pub instrument_id: String,
    /// Payment session id.
    pub session_id: String,
    /// Payer id.
    pub user_id: String,
    /// CAIP-2 network.
    pub network: String,
    /// USDC contract on that network.
    pub asset: String,
    /// The per-payment cap in atomic units.
    pub max_payment_atomic: String,
    /// The allowlisted recipients.
    pub recipients: Vec<String>,
    /// Allow any recipient instead.
    pub allow_any: bool,
    /// The pinned origins (none: any public HTTPS site).
    pub origins: Vec<String>,
    /// Return the paid body to the agent.
    pub return_body: bool,
}

fn strings(values: &[String]) -> Json {
    Json::Array(values.iter().map(|v| Json::string(v.as_str())).collect())
}

/// The `OpenClaw` configuration (legacy `build_openclaw_config`).
#[must_use]
pub fn build_openclaw_config(inputs: &OpenClawInputs) -> Json {
    let mut plugin = vec![
        ("region", Json::string(inputs.region.as_str())),
        ("paymentManagerArn", Json::string(inputs.manager_arn.as_str())),
        ("paymentInstrumentId", Json::string(inputs.instrument_id.as_str())),
        ("payment_session_id", Json::string(inputs.session_id.as_str())),
        ("userId", Json::string(inputs.user_id.as_str())),
        (
            "networkPreferences",
            strings(std::slice::from_ref(&inputs.network)),
        ),
        (
            "allowedAssetsByNetwork",
            Json::object(vec![(
                inputs.network.as_str(),
                strings(std::slice::from_ref(&inputs.asset)),
            )]),
        ),
        (
            "maxPaymentAmountAtomic",
            Json::string(inputs.max_payment_atomic.as_str()),
        ),
        ("returnBody", Json::Bool(inputs.return_body)),
    ];
    if inputs.allow_any {
        plugin.push(("allowAnyRecipient", Json::Bool(true)));
    } else {
        plugin.push(("allowedRecipients", strings(&inputs.recipients)));
    }
    if !inputs.origins.is_empty() {
        plugin.push(("allowedOrigins", strings(&inputs.origins)));
    }
    Json::object(vec![(
        "plugins",
        Json::object(vec![
            ("allow", Json::Array(vec![Json::string("aws-agents-pay")])),
            (
                "entries",
                Json::object(vec![(
                    "aws-agents-pay",
                    Json::object(vec![
                        ("enabled", Json::Bool(true)),
                        ("config", Json::object(plugin)),
                    ]),
                )]),
            ),
        ]),
    )])
}

// ----------------------------------------------------------------- the wizard

/// `_prompt(question, default)`; `None` after printing the D28 abort.
fn prompt(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
    question: &str,
    default: &str,
) -> Result<Option<String>, ToolkitError> {
    let suffix = if default.is_empty() {
        ": ".to_owned()
    } else {
        format!(" [{default}]: ")
    };
    match ask(deps, out, &format!("{question}{suffix}"))? {
        Some(answer) if answer.is_empty() => Ok(Some(default.to_owned())),
        Some(answer) => Ok(Some(answer)),
        None => {
            abort(out)?;
            Ok(None)
        }
    }
}

/// A bare `input(text).strip()`; `None` after printing the D28 abort.
fn line(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
    text: &str,
) -> Result<Option<String>, ToolkitError> {
    if let Some(answer) = ask(deps, out, text)? {
        return Ok(Some(answer));
    }
    abort(out)?;
    Ok(None)
}

fn abort(out: &mut Output<'_>) -> Result<(), ToolkitError> {
    out.line("")?;
    out.line(ABORTED)?;
    out.set_exit_status(1);
    Ok(())
}

/// Lines until a blank one.
fn lines_until_blank(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
    text: &str,
) -> Result<Option<Vec<String>>, ToolkitError> {
    let mut all = Vec::new();
    loop {
        match line(deps, out, text)? {
            None => return Ok(None),
            Some(answer) if answer.is_empty() => return Ok(Some(all)),
            Some(answer) => all.push(answer),
        }
    }
}

/// The policy part of the wizard.
struct Answers {
    user: String,
    region: String,
    network: String,
    asset: String,
    recipients: Vec<String>,
    allow_any: bool,
    max: CapDecimal,
    atomic: String,
    budget: CapDecimal,
    expiry: i64,
    origins: Vec<String>,
    return_body: bool,
}

const RULE: &str = "============================================================";

fn banner(out: &mut Output<'_>, project: Option<&str>) -> Result<(), ToolkitError> {
    out.line(&format!("\n{RULE}"))?;
    out.line("  AWS Agents Pay \u{2014} OpenClaw Setup")?;
    out.line(RULE)?;
    out.line("\nThis wizard provisions payment resources and generates your")?;
    out.line("OpenClaw plugin configuration. You'll need:")?;
    out.line("  \u{2022} agentcore CLI installed and deployed (agentcore deploy)")?;
    out.line("  \u{2022} a toolkit release build with payment support")?;
    out.line("  \u{2022} AWS credentials with the ManagementRole")?;
    if let Some(project) = project {
        out.line(&format!("  \u{2022} AgentCore project: {project}"))?;
    }
    out.line("")
}

/// Steps 1 to 3.
struct Identity {
    user: String,
    region: String,
    network: String,
    asset: String,
    recipients: Vec<String>,
    allow_any: bool,
}

/// Steps 1 to 3: identity, region, network and recipients.
fn identity(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<Option<Identity>, ToolkitError> {
    out.line("\n--- Step 1: User Identity ---")?;
    out.line("A stable userId ties your instrument and session together.")?;
    let Some(mut user) = prompt(deps, out, "User ID (blank to auto-generate)", "")? else {
        return Ok(None);
    };
    if user.is_empty() {
        let Ok(generated) = generate_user_id(deps) else {
            refuse(out, NO_RANDOM);
            return Ok(None);
        };
        user = generated;
        out.line(&format!("  Generated: {user}"))?;
    }
    let Some(region) = prompt(deps, out, "AWS region", "us-east-1")? else {
        return Ok(None);
    };
    out.line("\n--- Step 2: Network ---")?;
    let known: Vec<&str> = KNOWN_USDC.iter().map(|(n, _)| *n).collect();
    out.line(&format!("Known networks: {}", known.join(", ")))?;
    let Some(network) = prompt(deps, out, "CAIP-2 network", "eip155:84532")? else {
        return Ok(None);
    };
    let Some((_, asset)) = KNOWN_USDC.iter().find(|(n, _)| *n == network) else {
        refuse(
            out,
            &format!(
                "Unknown network {}. Known: {}",
                shown_value(&network),
                known.join(", ")
            ),
        );
        return Ok(None);
    };
    out.line("\n--- Step 3: Recipient Mode ---")?;
    out.line("  1. Allowlist specific merchant addresses (recommended)")?;
    out.line("  2. Allow any recipient (high risk \u{2014} publisher chooses beneficiary)")?;
    let Some(mode) = prompt(deps, out, "Choice", "1")? else {
        return Ok(None);
    };
    let mut recipients = Vec::new();
    let allow_any = mode == "2";
    if allow_any {
        out.line("  \u{26a0} allow-any-recipient enabled.")?;
    } else {
        out.line("Enter merchant wallet addresses (one per line, blank to finish):")?;
        let Some(list) = lines_until_blank(deps, out, "  payTo: ")? else {
            return Ok(None);
        };
        recipients = list;
        if recipients.is_empty() {
            refuse(out, "At least one recipient is required.");
            return Ok(None);
        }
    }
    Ok(Some(Identity {
        user,
        region,
        network,
        asset: (*asset).to_owned(),
        recipients,
        allow_any,
    }))
}

/// Step 4: the per-payment cap, the budget and the expiry (D29, D30).
fn limits(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<Option<(CapDecimal, String, CapDecimal, i64)>, ToolkitError> {
    out.line("\n--- Step 4: Spend Limits ---")?;
    let Some(max_text) = prompt(
        deps,
        out,
        "Max per-payment USD (for example 0.10, not atomic units)",
        "0.05",
    )?
    else {
        return Ok(None);
    };
    let checked = parse_positive_decimal(&max_text, "Max per-payment USD")
        .and_then(|max| usd_to_atomic(&max).map(|atomic| (max, atomic)));
    let (max, atomic) = match checked {
        Ok(pair) => pair,
        Err(reason) => {
            refuse(out, &reason);
            return Ok(None);
        }
    };
    out.line(&format!(
        "  ${} USD = {atomic} atomic units (USDC, {USDC_DECIMALS} decimals)",
        fixed(&max)
    ))?;
    let Some(budget_text) = prompt(deps, out, "Cumulative session budget USD", "5.00")? else {
        return Ok(None);
    };
    let Some(expiry_text) = prompt(
        deps,
        out,
        "Session expiry in minutes (15-480; 480 = 8 hours)",
        "120",
    )?
    else {
        return Ok(None);
    };
    let checked = parse_positive_decimal(&budget_text, "Session budget USD").and_then(|budget| {
        let expiry = crate::agents_pay::pyval::py_int(&Json::string(expiry_text.as_str()))
            .map_err(|_| "Expiry minutes must be a whole number.".to_owned())?;
        if !EXPIRY_RANGE.contains(&expiry) {
            return Err("Expiry minutes must be between 15 and 480.".to_owned());
        }
        if compare(&max, &budget) == Ordering::Greater {
            return Err("Max per-payment USD cannot exceed the cumulative session budget. Enter decimal USD values, not atomic units.".to_owned());
        }
        Ok((budget, expiry))
    });
    match checked {
        Ok((budget, expiry)) => Ok(Some((max, atomic, budget, expiry))),
        Err(reason) => {
            refuse(out, &reason);
            Ok(None)
        }
    }
}

/// Steps 1 to 5.
fn answers(deps: &AdminDeps<'_>, out: &mut Output<'_>) -> Result<Option<Answers>, ToolkitError> {
    let Some(Identity {
        user,
        region,
        network,
        asset,
        recipients,
        allow_any,
    }) = identity(deps, out)?
    else {
        return Ok(None);
    };
    let Some((max, atomic, budget, expiry)) = limits(deps, out)? else {
        return Ok(None);
    };
    out.line("\nAllowed origins (blank to allow any public HTTPS site):")?;
    let Some(origins) = lines_until_blank(deps, out, "  origin: ")? else {
        return Ok(None);
    };
    out.line("\n--- Step 5: Paid Content Return ---")?;
    out.line("By default, paid publisher content is withheld from the model's context as a security\ncontrol \u{2014} the response body may contain prompt injection. Returning it lets the agent\nread/summarize what it paid for, at that risk.")?;
    let Some(answer) = prompt(deps, out, "Return paid response body to the agent? (y/n)", "y")?
    else {
        return Ok(None);
    };
    let lowered = answer
        .trim_matches(crate::agents_pay::pyval::is_python_whitespace)
        .to_lowercase();
    let return_body = !matches!(lowered.as_str(), "n" | "no" | "false" | "0");
    Ok(Some(Answers {
        user,
        region,
        network,
        asset,
        recipients,
        allow_any,
        max,
        atomic,
        budget,
        expiry,
        origins,
        return_body,
    }))
}

fn policy_config(a: &Answers) -> Json {
    let mut policy = vec![
        ("max_per_payment_usd", Json::string(fixed(&a.max))),
        (
            "allowed_networks",
            strings(std::slice::from_ref(&a.network)),
        ),
        (
            "allowed_assets",
            Json::object(vec![(
                a.network.as_str(),
                strings(std::slice::from_ref(&a.asset)),
            )]),
        ),
        ("allowed_schemes", Json::Array(vec![Json::string("exact")])),
        ("return_body", Json::Bool(a.return_body)),
    ];
    if a.allow_any {
        policy.push(("allow_any_recipient", Json::Bool(true)));
    } else {
        policy.push(("allowed_recipients", strings(&a.recipients)));
    }
    if !a.origins.is_empty() {
        policy.push(("allowed_origins", strings(&a.origins)));
    }
    Json::object(vec![
        (
            "resources",
            Json::object(vec![
                ("user_id", Json::string(a.user.as_str())),
                ("region", Json::string(a.region.as_str())),
            ]),
        ),
        ("policy", Json::object(policy)),
    ])
}

/// The manager and connector: discovered, else asked.
fn resources(
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
    project: Option<&str>,
) -> Result<Option<(String, String)>, ToolkitError> {
    let discovered = deployed::discover(deps, project);
    let manager = if let Some(manager) = discovered.manager_arn.filter(|m| !m.is_empty()) {
        out.line(&format!(
            "\n  Discovered manager ARN: {}",
            shown_value(&manager)
        ))?;
        manager
    } else {
        out.line(&format!(
            "\nCould not auto-discover payment manager ARN from deployed-state.json.\nChecked: {}",
            deployed::checked(&deployed::candidates(project, deps))
        ))?;
        let Some(manager) = prompt(deps, out, "Payment Manager ARN", "")? else {
            return Ok(None);
        };
        if manager.is_empty() {
            refuse(out, "Manager ARN is required.");
            return Ok(None);
        }
        manager
    };
    let connector = if let Some(connector) = discovered.connector_id.filter(|c| !c.is_empty()) {
        out.line(&format!(
            "  Discovered connector ID: {}",
            shown_value(&connector)
        ))?;
        connector
    } else {
        let Some(connector) = prompt(deps, out, "Payment Connector ID", "")? else {
            return Ok(None);
        };
        if connector.is_empty() {
            refuse(out, "Connector ID is required.");
            return Ok(None);
        }
        connector
    };
    Ok(Some((manager, connector)))
}

#[allow(clippy::too_many_lines)] // The legacy wizard, one step after another.
pub(super) fn run(
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if !deps.terminal.is_tty() {
        refuse(out, "setup-openclaw requires an interactive terminal.");
        return Ok(());
    }
    let project = invocation
        .value("--project-dir")
        .filter(|p| !p.is_empty())
        .map(|p| deployed::resolve_dir(p, deps));
    if let Some(dir) = &project {
        if !deps.fs.is_dir(dir) {
            refuse(
                out,
                &format!(
                    "AgentCore project directory does not exist: {}",
                    shown_path(dir)
                ),
            );
            return Ok(());
        }
    }
    let project_text = project.as_ref().map(|p| shown_path(p));
    let project_arg = project.as_ref().map(|p| p.to_string_lossy().into_owned());
    banner(out, project_text.as_deref())?;
    {
        let Some(sts) = open_api(deps, None, out) else {
            return Ok(());
        };
        if !credentials_ok(sts.as_ref(), out) {
            return Ok(());
        }
    }
    let Some(a) = answers(deps, out)? else {
        return Ok(());
    };
    let path = admin_config_path(invocation.value("--path"), deps);
    let mut config = policy_config(&a);
    let Some((manager, connector)) = resources(deps, out, project_arg.as_deref())? else {
        return Ok(());
    };
    super::set_in(
        &mut config,
        "resources",
        "payment_manager_arn",
        Json::string(manager.as_str()),
    );
    if let Err(reason) = save_config(&path, &config, deps) {
        refuse(out, &reason);
        return Ok(());
    }
    out.line(&format!("\n  \u{2713} Config written to {}", shown_path(&path)))?;
    out.line(&format!(
        "  Paid response body will be {} the agent (policy.return_body).",
        if a.return_body {
            "RETURNED to"
        } else {
            "WITHHELD from"
        }
    ))?;

    out.line("\n--- Step 6: Create Payment Instrument ---")?;
    let Some(email) = prompt(deps, out, "End-user email (for wallet delegation)", "")? else {
        return Ok(());
    };
    if email.is_empty() {
        refuse(out, "Email is required for instrument creation.");
        return Ok(());
    }
    let family = if a.network.contains("eip155") {
        "ETHEREUM"
    } else {
        "SOLANA"
    };
    if let Err(reason) = deps.api.check_manager(&manager) {
        refuse(out, &reason);
        return Ok(());
    }
    let Some(api) = open_api(deps, Some(a.region.as_str()), out) else {
        return Ok(());
    };
    let Ok(token) = client_token(deps) else {
        refuse(out, NO_RANDOM);
        return Ok(());
    };
    let response =
        match api.create_instrument(&manager, &a.user, &connector, family, &email, &token) {
            Ok(response) => response,
            Err(error) => {
                api_failure(out, "CreatePaymentInstrument", error);
                return Ok(());
            }
        };
    let Some(instrument) = response_text(&response, "paymentInstrument", "paymentInstrumentId")
    else {
        api_failure(out, "CreatePaymentInstrument", PaymentError::InvalidResponse);
        return Ok(());
    };
    let (wallet, redirect) = wallet_of(&response);
    let wallet = or_none(wallet.as_deref());
    let shown_instrument = shown_value(&instrument).into_owned();
    if let Err(reason) = update_resources(
        &path,
        &[("payment_instrument_id", Some(instrument.as_str()))],
        deps,
    ) {
        refuse(
            out,
            &format!("Instrument {shown_instrument} was created, but {reason}"),
        );
        return Ok(());
    }
    out.line(&format!("  \u{2713} Instrument created: {shown_instrument}"))?;
    out.line(&format!("    Wallet: {wallet}"))?;

    out.line("\n--- Step 7: Delegate & Fund ---")?;
    match redirect {
        Some(url) => {
            out.line(&format!("  1. Visit: {url}"))?;
            out.line(&format!("     Sign in and grant access to {wallet}"))?;
        }
        None => out.line("  1. Approve via the Privy frontend SDK")?,
    }
    out.line(&format!("  2. Send testnet USDC to {wallet}"))?;
    out.line("     https://faucet.circle.com/ (Base Sepolia)")?;
    out.line("")?;
    if line(deps, out, "Press Enter when delegation and funding are complete...")?.is_none() {
        return Ok(());
    }

    out.line("\n--- Step 8: Create Payment Session ---")?;
    out.line("\n  About to create session:")?;
    out.line(&format!(
        "    Budget:          ${} USD cumulative",
        fixed(&a.budget)
    ))?;
    out.line(&format!(
        "    Per-payment cap: ${} USD ({} atomic units)",
        fixed(&a.max),
        a.atomic
    ))?;
    out.line(&format!("    Expiry:          {}", format_duration(a.expiry)))?;
    out.line(&format!("    User:            {}", shown_value(&a.user)))?;
    if matches!(
        ask_approval(deps, out, "  Type 'approve' to continue: ")?,
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
    let session = match api.create_session(&manager, &a.user, &fixed(&a.budget), a.expiry, &token)
    {
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
        &[("payment_session_id", Some(session_id.as_str()))],
        deps,
    ) {
        refuse(
            out,
            &format!("Session {shown_session} was created, but {reason}"),
        );
        return Ok(());
    }
    out.line(&format!("  \u{2713} Session created: {shown_session}"))?;
    out.line(&format!("\n{RULE}"))?;
    out.line("  \u{2705} Setup complete!")?;
    out.line(RULE)?;
    out.line("\nAdd this to your OpenClaw config (~/.openclaw/openclaw.json):")?;
    out.line("")?;
    let openclaw = build_openclaw_config(&OpenClawInputs {
        region: a.region.clone(),
        manager_arn: manager.clone(),
        instrument_id: instrument,
        session_id,
        user_id: a.user.clone(),
        network: a.network.clone(),
        asset: a.asset.clone(),
        max_payment_atomic: a.atomic.clone(),
        recipients: a.recipients.clone(),
        allow_any: a.allow_any,
        origins: a.origins.clone(),
        return_body: a.return_body,
    });
    out.line(&crate::aurora::pyfmt::dumps(&openclaw))?;
    out.line("\nThen restart OpenClaw to activate the plugin.")?;
    Ok(())
}
