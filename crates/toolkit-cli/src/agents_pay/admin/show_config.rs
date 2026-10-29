//! `agents-pay admin show-config` (legacy `cmd_show_config`).

use std::path::Path;

use toolkit_core::ToolkitError;

use super::{admin_config_path, load_raw_config, refuse, resource, section, AdminDeps};
use crate::agents_pay::pyval::truthy;
use crate::safe_text::shown_path;
use crate::{Invocation, Output};

/// The missing-config hint (D23: the Rust command name).
pub const CREATE_HINT: &str = "Create it with: toolkit agents-pay admin init-config --max-per-payment-usd 0.05 --recipient <payTo> (or explicitly use --allow-any-recipient)";

pub(super) fn run(
    invocation: &Invocation,
    deps: &AdminDeps<'_>,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let path = admin_config_path(invocation.value("--path"), deps);
    if !deps.fs.exists(&path) {
        out.line(&format!(
            "No config at {}. Payments are refused until one exists.",
            shown_path(&path)
        ))?;
        out.line(CREATE_HINT)?;
        out.set_exit_status(1);
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (Some(file), Some(dir)) = (deps.fs.lstat(&path), deps.fs.lstat(parent)) else {
        refuse(
            out,
            &format!("Cannot stat the payment config {}.", shown_path(&path)),
        );
        return Ok(());
    };
    out.line(&format!("Config    : {}", shown_path(&path)))?;
    out.line(&format!(
        "File mode : {} {}",
        crate::agents_pay::config::filemode(file.mode),
        if file.mode & 0o077 == 0 {
            "OK"
        } else {
            "*** TOO OPEN \u{2014} chmod 600 ***"
        }
    ))?;
    out.line(&format!(
        "Dir mode  : {} {}",
        crate::agents_pay::config::filemode(dir.mode),
        if dir.mode & 0o022 == 0 {
            "OK"
        } else {
            "*** WRITABLE BY OTHERS \u{2014} chmod 700 ***"
        }
    ))?;
    out.line(&format!(
        "Owner     : uid {} {}",
        file.uid,
        if file.uid == deps.fs.uid() {
            "(you)"
        } else {
            "*** NOT YOU ***"
        }
    ))?;
    let config = load_raw_config(&path, deps);
    out.line("\n--- resources (what to pay WITH; no secrets) ---")?;
    out.line(&crate::aurora::pyfmt::dumps(&section(&config, "resources")))?;
    out.line("\n--- policy (what MAY be paid) ---")?;
    out.line(&crate::aurora::pyfmt::dumps(&section(&config, "policy")))?;
    let missing: Vec<&str> = [
        "payment_manager_arn",
        "payment_instrument_id",
        "payment_session_id",
        "user_id",
    ]
    .into_iter()
    .filter(|key| !resource(&config, key).is_some_and(truthy))
    .collect();
    if !missing.is_empty() {
        out.line(&format!("\nNot yet set: {}", missing.join(", ")))?;
        out.line("These may also come from the environment, but the config file wins when present.")?;
    }
    Ok(())
}
