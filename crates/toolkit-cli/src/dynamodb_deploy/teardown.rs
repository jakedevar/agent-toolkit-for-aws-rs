//! `generate_teardown.py`: the standalone `teardown.sh` for one benchmark run.
//!
//! The command never deletes anything and never runs the script. It reads the
//! manifest of a run (untrusted: it may be hand-edited), refuses anything that
//! does not belong to the run's `ddb-skill-bench-` prefix, and renders a bash
//! script with its own safety layer (identity check against the manifest's
//! account, a prefix guard on every delete, `--confirm`, `--dry-run`).
//!
//! Two rules differ from the script, both recorded in
//! `docs/migration/dynamodb-deploy.md`: every value written into the script is
//! shell-quoted after validation (the script interpolated raw manifest text
//! between single quotes), and the file is written only when the caller passes
//! `--yes-generate-teardown`.

use std::path::Path;

use toolkit_core::{quote_untrusted, Json, ToolkitError};

use super::model::{die, first_text, list, object, require_objects, text};
use super::names::{
    check_account, check_plain, check_policy_name, check_region, check_resource_name,
    check_table_name, REQUIRED_PREFIX,
};
use crate::dynamodb::pyfmt;

const TEMPLATE: &str = include_str!("teardown_template.txt");

/// What the teardown script deletes, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Teardown {
    pub(crate) account: String,
    pub(crate) region: String,
    pub(crate) prefix: String,
    pub(crate) run_id: String,
    pub(crate) profile_hint: String,
    pub(crate) tables: Vec<String>,
    pub(crate) lambda_fn: String,
    pub(crate) lambda_role: String,
    pub(crate) lambda_policy: String,
}

/// `'text'` for bash, with every embedded `'` closed, escaped and reopened.
pub(crate) fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn refuse_prefix(prefix: Option<&Json>) -> ToolkitError {
    let shown = prefix.map_or_else(|| "None".to_string(), |p| quote_untrusted(&pyfmt::text(p)));
    die(format!(
        "manifest prefix {shown} is missing or does not start with '{REQUIRED_PREFIX}'. Refusing \
         to generate teardown."
    ))
}

/// Parses and validates a run manifest.
///
/// # Errors
///
/// Exit status 2 naming the offending member, never echoing control text.
pub(crate) fn from_manifest(manifest: &Json) -> Result<Teardown, ToolkitError> {
    if manifest.type_name() != "object" {
        return Err(die("the manifest must be a JSON object"));
    }
    let prefix = match manifest.get("prefix") {
        Some(Json::String(prefix)) if prefix.starts_with(REQUIRED_PREFIX) => prefix.clone(),
        other => return Err(refuse_prefix(other)),
    };
    super::names::check_prefix(&prefix)?;

    let entries = list(manifest, "tables")?;
    require_objects(entries, "tables")?;
    let mut tables = Vec::new();
    for entry in entries {
        if let Some(name) = text(entry, "name")? {
            tables.push(name.to_string());
        }
    }
    let bad: Vec<String> = tables
        .iter()
        .filter(|name| !name.starts_with(&prefix))
        .map(|name| quote_untrusted(name))
        .collect();
    if !bad.is_empty() {
        return Err(die(format!(
            "manifest contains tables whose names don't match prefix {}: [{}]. Refusing to \
             generate teardown.",
            quote_untrusted(&prefix),
            bad.join(", ")
        )));
    }
    for name in &tables {
        check_table_name(name)?;
    }

    let lambda = object(manifest, "lambda")?;
    let member = |key: &str| -> Result<String, ToolkitError> {
        match lambda {
            Some(lambda) => Ok(text(lambda, key)?.unwrap_or_default().to_string()),
            None => Ok(String::new()),
        }
    };
    let lambda_fn = member("function_name")?;
    let lambda_role = member("role_name")?;
    let lambda_policy = member("policy_name")?;
    if !lambda_fn.is_empty() && !lambda_fn.starts_with(&prefix) {
        return Err(die(format!(
            "manifest lambda function_name {} does not start with prefix {}. Refusing.",
            quote_untrusted(&lambda_fn),
            quote_untrusted(&prefix)
        )));
    }
    if !lambda_role.is_empty() && !lambda_role.starts_with(&prefix) {
        return Err(die(format!(
            "manifest lambda role_name {} does not start with prefix {}. Refusing.",
            quote_untrusted(&lambda_role),
            quote_untrusted(&prefix)
        )));
    }
    if !lambda_fn.is_empty() {
        check_resource_name("lambda function_name", &lambda_fn)?;
    }
    if !lambda_role.is_empty() {
        check_resource_name("lambda role_name", &lambda_role)?;
    }
    if !lambda_policy.is_empty() {
        check_policy_name(&lambda_policy)?;
    }

    // The identity guard of the script compares the live account with this one:
    // an empty or malformed value would make the guard meaningless.
    let account = text(manifest, "account")?.unwrap_or_default();
    check_account(account)?;
    let region = text(manifest, "region")?.unwrap_or_default();
    check_region(region)?;
    let run_id = text(manifest, "run_id")?.unwrap_or_default();
    check_plain("run_id", run_id, 64)?;
    let profile_hint = first_text(manifest, &["aws_profile", "alias"])?.unwrap_or_default();
    check_plain("aws_profile", profile_hint, 128)?;
    Ok(Teardown {
        account: account.to_string(),
        region: region.to_string(),
        prefix,
        run_id: run_id.to_string(),
        profile_hint: profile_hint.to_string(),
        tables,
        lambda_fn,
        lambda_role,
        lambda_policy,
    })
}

/// Fills `%%name%%` markers in one pass: inserted values are never rescanned,
/// so a value that looks like a marker cannot be expanded.
pub(crate) fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len() + 512);
    let mut rest = template;
    while let Some(start) = rest.find("%%") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let known = after.find("%%").and_then(|end| {
            values
                .iter()
                .find(|(name, _)| *name == &after[..end])
                .map(|(_, value)| (end, value))
        });
        if let Some((end, value)) = known {
            out.push_str(value);
            rest = &after[end + 2..];
        } else {
            out.push_str("%%");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Renders the `teardown.sh` of a run.
pub(crate) fn render(teardown: &Teardown) -> String {
    let log_group = if teardown.lambda_fn.is_empty() {
        String::new()
    } else {
        format!("/aws/lambda/{}", teardown.lambda_fn)
    };
    let tables = teardown
        .tables
        .iter()
        .map(|name| sh_quote(name))
        .collect::<Vec<_>>()
        .join(" ");
    fill(
        TEMPLATE,
        &[
            ("account", sh_quote(&teardown.account)),
            ("region", sh_quote(&teardown.region)),
            ("prefix", sh_quote(&teardown.prefix)),
            ("run_id", sh_quote(&teardown.run_id)),
            ("profile_hint", sh_quote(&teardown.profile_hint)),
            ("tables", tables),
            ("lambda_fn", sh_quote(&teardown.lambda_fn)),
            ("lambda_role", sh_quote(&teardown.lambda_role)),
            ("lambda_policy", sh_quote(&teardown.lambda_policy)),
            ("lambda_log_group", sh_quote(&log_group)),
        ],
    )
}

/// Whether two paths name the same file (writing the script over the manifest
/// would destroy the manifest).
pub(crate) fn same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
