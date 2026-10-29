//! `agents-pay admin init-config`: write the resources and policy (legacy
//! `cmd_init_config`). No terminal gate: it creates nothing at a service.

use toolkit_core::{Json, ToolkitError};

use super::{
    admin_config_path, deployed, generate_user_id, load_raw_config, refuse, replace_section,
    resource, save_config, section, set_default_in, set_in, AdminDeps, KNOWN_USDC, NO_RANDOM,
};
use crate::agents_pay::pyval::{py_str, truthy};
use crate::safe_text::{shown_path, shown_value};
use crate::{Invocation, Output};

fn strings(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|v| Json::string(*v)).collect())
}

/// The argparse mutually exclusive, required recipient group (exit 2).
fn recipient_mode(invocation: &Invocation) -> Result<Option<Vec<&str>>, ToolkitError> {
    let recipients: Vec<&str> = invocation.values_of("--recipient").collect();
    let any = invocation.has_flag("--allow-any-recipient");
    match (recipients.is_empty(), any) {
        (false, true) => Err(ToolkitError::usage(
            "invalid_arguments",
            "argument --allow-any-recipient: not allowed with argument --recipient",
        )),
        (true, false) => Err(ToolkitError::usage(
            "invalid_arguments",
            "one of the arguments --recipient --allow-any-recipient is required",
        )),
        (true, true) => Ok(None),
        (false, false) => Ok(Some(recipients)),
    }
}

#[allow(clippy::too_many_lines)] // The legacy command, one step after another.
pub(super) fn run(
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let recipients = recipient_mode(invocation)?;
    let path = admin_config_path(invocation.value("--path"), deps);
    let force = invocation.has_flag("--force");
    if deps.fs.exists(&path) && !force {
        refuse(
            out,
            &format!(
                "Refusing to overwrite existing policy at {} (pass --force).",
                shown_path(&path)
            ),
        );
        return Ok(());
    }
    let network = invocation.value("--network").unwrap_or("eip155:84532");
    let Some((_, asset)) = KNOWN_USDC.iter().find(|(n, _)| *n == network) else {
        let known: Vec<&str> = KNOWN_USDC.iter().map(|(n, _)| *n).collect();
        refuse(
            out,
            &format!(
                "Unknown network {}. Known: {}. Add the exact USDC contract to KNOWN_USDC rather than passing one in.",
                shown_value(network),
                known.join(", ")
            ),
        );
        return Ok(());
    };
    let mut config = if force {
        load_raw_config(&path, deps)
    } else {
        Json::object(vec![
            ("resources", Json::Object(vec![])),
            ("policy", Json::Object(vec![])),
        ])
    };
    let cap = invocation.value("--max-per-payment-usd").unwrap_or("0.10");
    let mut policy = vec![
        ("max_per_payment_usd".to_owned(), Json::string(cap)),
        ("allowed_networks".to_owned(), strings(&[network])),
        (
            "allowed_assets".to_owned(),
            Json::object(vec![(network, strings(&[asset]))]),
        ),
        ("allowed_schemes".to_owned(), strings(&["exact"])),
    ];
    match &recipients {
        None => policy.push(("allow_any_recipient".to_owned(), Json::Bool(true))),
        Some(list) => policy.push(("allowed_recipients".to_owned(), strings(list))),
    }
    let origins: Vec<&str> = invocation.values_of("--origin").collect();
    if !origins.is_empty() {
        policy.push(("allowed_origins".to_owned(), strings(&origins)));
    }
    replace_section(&mut config, "policy", policy);

    let explicit_user = invocation.value("--user-id").filter(|u| !u.is_empty());
    let existing = resource(&config, "user_id").filter(|v| truthy(v)).cloned();
    let user = match (explicit_user, &existing) {
        (Some(user), _) => Json::string(user),
        (None, Some(existing)) => existing.clone(),
        (None, None) => {
            let Ok(user) = generate_user_id(deps) else {
                refuse(out, NO_RANDOM);
                return Ok(());
            };
            Json::string(user)
        }
    };
    let user_text = py_str(&user).unwrap_or_default();
    set_in(&mut config, "resources", "user_id", user);

    if let Some(manager) = deployed::discover(deps, None)
        .manager_arn
        .filter(|m| !m.is_empty())
    {
        set_default_in(
            &mut config,
            "resources",
            "payment_manager_arn",
            Json::string(manager),
        );
    }
    if let Some(region) = invocation.value("--region").filter(|r| !r.is_empty()) {
        set_default_in(&mut config, "resources", "region", Json::string(region));
    }

    if let Err(reason) = save_config(&path, &config, deps) {
        refuse(out, &reason);
        return Ok(());
    }
    out.line(&format!(
        "Wrote config to {} (mode 0600, in a 0700 directory)",
        shown_path(&path)
    ))?;
    let generated = explicit_user.is_none() && existing.is_none();
    out.line(&format!(
        "Payer identity: {}{}",
        shown_value(&user_text),
        if generated {
            "  (generated \u{2014} single tenant, no need to pass it again)"
        } else {
            ""
        }
    ))?;
    let shown = Json::object(vec![
        ("resources", section(&config, "resources")),
        ("policy", section(&config, "policy")),
    ]);
    out.line(&crate::aurora::pyfmt::dumps(&shown))?;
    out.line("\nThe policy section authorizes payments made through the sanctioned runtime. That runtime ignores config-path environment overrides. Keep the config and ProcessPayment credentials outside any unrestricted same-user shell.")?;
    out.line("\nTwo ceilings, both needed: max_per_payment_usd bounds ONE transaction; the\nsession budget (set later by new-session) bounds CUMULATIVE spend. Without a\nper-payment cap, one hostile challenge for the whole remaining balance would\ndrain the session in a single payment.")?;
    if recipients.is_none() {
        out.line("\nWARNING: allow_any_recipient is enabled. The publisher may choose the\npayment beneficiary. Network, asset, scheme, origin, per-payment, and\ncumulative session limits remain enforced.")?;
    } else {
        out.line("\nApproved recipients are enforced in trusted code. A challenge whose payTo is\nnot in allowed_recipients is refused before ProcessPayment is called.")?;
    }
    if origins.is_empty() {
        out.line("\nNo allowed_origins set: the agent may fetch ANY public HTTPS site. The\nSSRF protections still apply (HTTPS only, internal ranges refused, pinned\naddress, no redirects, bounded body). Pass --origin to pin a merchant set.")?;
    }
    Ok(())
}
