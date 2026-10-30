//! `agents-pay admin preflight` (legacy `cmd_preflight`; T-PREFLIGHT).

use toolkit_core::ToolkitError;

use super::{admin_config_path, deployed, AdminDeps, SECRET_NAME_FRAGMENTS};
use crate::agents_pay::pyval::py_str;
use crate::safe_text::{shown_path, shown_value};
use crate::{Invocation, Output};

/// The runtime variables `preflight` reports.
pub const RUNTIME_VARIABLES: [&str; 4] = [
    "PAYMENT_MANAGER_ARN",
    "PAYMENT_INSTRUMENT_ID",
    "PAYMENT_SESSION_ID",
    "PAYMENT_USER_ID",
];

/// The secret-shaped variable names in `names`, sorted: any name whose
/// upper-cased form contains one of [`SECRET_NAME_FRAGMENTS`].
#[must_use]
pub fn leaked_names(names: &[String]) -> Vec<String> {
    let mut leaked: Vec<String> = names
        .iter()
        .filter(|name| {
            let upper = name.to_uppercase();
            SECRET_NAME_FRAGMENTS.iter().any(|f| upper.contains(f))
        })
        .cloned()
        .collect();
    leaked.sort();
    leaked
}

pub(super) fn run(
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let mut ok = true;
    let path = admin_config_path(invocation.value("--path"), deps);
    match deps.fs.load_runtime_policy(&path) {
        Ok(policy) => {
            let cap = policy
                .get("max_per_payment_usd")
                .and_then(py_str)
                .unwrap_or_default();
            out.line(&format!(
                "[ok]   policy loaded, max_per_payment_usd={}",
                shown_value(&cap)
            ))?;
        }
        Err(reason) => {
            out.line(&format!("[FAIL] policy: {reason}"))?;
            ok = false;
        }
    }
    let set = |name: &str| deps.env.var(name).is_some_and(|v| !v.is_empty());
    for name in RUNTIME_VARIABLES {
        if set(name) {
            out.line(&format!("[ok]   {name}"))?;
        } else {
            out.line(&format!("[--]   {name} not set"))?;
        }
    }
    if !set("PAYMENT_MANAGER_ARN") {
        let candidates = deployed::candidates(None, deps);
        let found = deployed::find(&candidates, deps);
        let discovered = deployed::discover_in(&candidates, deps);
        if let (Some(state), Some(manager)) =
            (found, discovered.manager_arn.filter(|m| !m.is_empty()))
        {
            out.line(&format!(
                "\n       Found in {}: run this to fix the above ->",
                shown_path(&state)
            ))?;
            out.line(&format!(
                "         export PAYMENT_MANAGER_ARN={}",
                shown_value(&manager)
            ))?;
        } else {
            out.line(&format!(
                "\n       deployed-state.json not found (checked: {}).",
                deployed::checked(&candidates)
            ))?;
            out.line("       Run from the directory that CONTAINS the agentcore/ folder,")?;
            out.line("       from inside it, or set the variables by hand.")?;
        }
    }
    let leaked = leaked_names(&deps.env.names());
    if leaked.is_empty() {
        out.line("[ok]   no provider secrets in the runtime environment")?;
    } else {
        let shown: Vec<String> = leaked.iter().map(|n| shown_value(n).into_owned()).collect();
        out.line(&format!(
            "[FAIL] provider secrets present in this environment: {}",
            shown.join(", ")
        ))?;
        out.line("       The runtime must never hold provider credentials; signing happens in AgentCore.")?;
        ok = false;
    }
    out.line("[ok]   AgentCore payments client built in")?;
    out.line(&format!(
        "\nPreflight {}",
        if ok { "PASSED" } else { "FAILED" }
    ))?;
    out.set_exit_status(u8::from(!ok));
    Ok(())
}
