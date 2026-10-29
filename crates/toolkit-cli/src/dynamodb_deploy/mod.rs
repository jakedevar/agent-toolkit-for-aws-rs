//! `toolkit dynamodb deploy-model` and `toolkit dynamodb generate-teardown`
//! (slice `dynamo-deploy`, Issue #4391).
//!
//! Ports `amazon-dynamodb/scripts/deploy_model.py` and `generate_teardown.py`
//! as **offline plan generators**. Nothing here creates or deletes an AWS
//! resource, and no command runs a generated script:
//!
//! - `deploy-model` validates the design and the benchmark config, reads the
//!   caller identity from a recorded document (`--recorded FILE`, the
//!   [`identity::IdentityClient`] seam), refuses a production identity and
//!   writes the deployment plan: the exact `CreateTable`, IAM and Lambda
//!   requests a live deploy would send ([`plan`], [`policy`], [`design`]).
//!   A live client is a follow-up.
//! - `generate-teardown` renders the `teardown.sh` of a run manifest
//!   ([`teardown`]) and writes it only with `--yes-generate-teardown`.
//!
//! Outputs are private (owner-only), refuse a symbolic link and are written to
//! a temporary file and renamed ([`write`]). The behavior contract, the
//! security surface and every intentional change are recorded in
//! `docs/migration/dynamodb-deploy.md`.

use std::path::Path;

use toolkit_core::{
    neutralize_line, parse_json, quote_untrusted, read_regular_file, Json, ToolkitError,
};
use toolkit_msk::pyfmt::ascii_escape;

use crate::manifests::read_failure;
use crate::safe_text::safe_path;
use crate::{Invocation, Output};

mod design;
mod identity;
mod model;
mod names;
mod plan;
mod policy;
mod recorded;
mod teardown;
pub(crate) mod write;

/// Registration of the `dynamodb-deploy` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "dynamodb-deploy",
    commands: &[
        crate::CommandSpec {
            path: &["dynamodb", "deploy-model"],
            flags: DEPLOY_MODEL_FLAGS,
            value_flags: DEPLOY_MODEL_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: deploy_model_command,
        },
        crate::CommandSpec {
            path: &["dynamodb", "generate-teardown"],
            flags: GENERATE_TEARDOWN_FLAGS,
            value_flags: GENERATE_TEARDOWN_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: generate_teardown_command,
        },
    ],
    synopsis: r"  toolkit dynamodb deploy-model --model FILE --config FILE --manifest-out FILE --yes-deploy
      [--recorded FILE --plan-out FILE] [--created-at TS] [--run-id ID]
  toolkit dynamodb generate-teardown --manifest FILE --out FILE --yes-generate-teardown
",
    description: r"  dynamodb deploy-model | generate-teardown
                Offline plans of the DynamoDB benchmark deployment (#4391). deploy-model
                writes the CreateTable, IAM and Lambda requests a live deploy would
                send, from a recorded caller identity (--recorded); it creates nothing.
                generate-teardown writes teardown.sh from a run manifest only with
                --yes-generate-teardown; neither command runs a script or calls AWS.
",
    exit_notes: "",
    doc: "docs/migration/dynamodb-deploy.md",
    synopsis_rank: 230,
    description_rank: 230,
};

#[cfg(test)]
mod tests;

use identity::{refuse_production, resolve_identity, Identity, IdentityClient, NoLiveClient};
use model::die;
use plan::{build_plan, settings, RunClock};
use recorded::RecordedClient;
use write::{write_private, PRIVATE_EXECUTABLE, PRIVATE_FILE};

/// Value flags of `dynamodb deploy-model`.
pub const DEPLOY_MODEL_VALUE_FLAGS: &[&str] = &[
    "--model",
    "--config",
    "--manifest-out",
    "--plan-out",
    "--recorded",
    "--created-at",
    "--run-id",
];

/// Boolean flags of `dynamodb deploy-model`.
pub const DEPLOY_MODEL_FLAGS: &[&str] = &["--yes-deploy"];

/// Value flags of `dynamodb generate-teardown`.
pub const GENERATE_TEARDOWN_VALUE_FLAGS: &[&str] = &["--manifest", "--out"];

/// Boolean flags of `dynamodb generate-teardown`.
pub const GENERATE_TEARDOWN_FLAGS: &[&str] = &["--yes-generate-teardown"];

/// Upper bound for the design, config, manifest and recorded JSON (16 MiB).
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

fn usage_missing(name: &str) -> ToolkitError {
    ToolkitError::usage(
        "missing_option",
        format!("the following arguments are required: {name}"),
    )
}

fn required<'a>(invocation: &'a Invocation, name: &str) -> Result<&'a str, ToolkitError> {
    invocation.value(name).ok_or_else(|| usage_missing(name))
}

/// Reads and parses a JSON input through the shared path contract (symbolic
/// links and non-regular files refused, size bounded).
fn load(path: &Path, what: &str) -> Result<Json, ToolkitError> {
    let shown = safe_path(&path.display().to_string());
    let text = read_regular_file(path, MAX_INPUT_BYTES).map_err(|error| {
        die(format!(
            "cannot read {what} {shown}: {}",
            read_failure(&error)
        ))
    })?;
    parse_json(&text).map_err(|error| {
        die(format!(
            "{what} {shown} is not valid JSON: {}",
            quote_untrusted(&error.to_string())
        ))
    })
}

fn random_hex(digits: usize) -> String {
    use std::collections::hash_map::RandomState;
    use std::fmt::Write as _;
    use std::hash::{BuildHasher, Hasher};

    let mut out = String::new();
    let mut round = 0_u32;
    while out.len() < digits {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.write_u32(round);
        if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            hasher.write_u128(now.as_nanos());
        }
        let _ = write!(out, "{:016x}", hasher.finish());
        round += 1;
    }
    out.truncate(digits);
    out
}

fn is_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

fn run_clock(invocation: &Invocation) -> Result<RunClock, ToolkitError> {
    let created_at = match invocation.value("--created-at") {
        Some(given) if is_timestamp(given) => given.to_string(),
        Some(_) => {
            return Err(die(
                "--created-at must look like 2026-01-31T12:00:00Z (UTC, whole seconds)",
            ))
        }
        None => {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
            crate::dynamodb::iterate::utc_iso(seconds)
        }
    };
    let run_id = match invocation.value("--run-id") {
        Some(given)
            if given.len() == 32
                && given
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            given.to_string()
        }
        Some(_) => return Err(die("--run-id must be 32 lowercase hex digits")),
        None => random_hex(32),
    };
    Ok(RunClock {
        created_at,
        run_id,
        prefix_token: random_hex(8),
    })
}

/// What `deploy-model` was asked to do.
struct DeployArgs<'a> {
    model: &'a Path,
    config: &'a Path,
    manifest_out: &'a Path,
    plan_out: &'a Path,
    yes_deploy: bool,
}

fn banner(
    out: &mut Output<'_>,
    bar: &str,
    region: &str,
    identity: &Identity,
) -> Result<(), ToolkitError> {
    let alias = if identity.alias.is_empty() {
        "<none>".to_string()
    } else {
        quote_untrusted(&identity.alias)
    };
    out.line(bar)?;
    out.line("# PLAN ONLY — this command creates and changes no AWS resource.")?;
    out.line("# A live deploy of this plan would create REAL AWS resources in the account shown")?;
    out.line("# below, with live capacity and traffic. A benchmark run typically costs")?;
    out.line("# single-digit cents but involves:")?;
    out.line("#   - Multiple DynamoDB tables and GSIs with live capacity.")?;
    out.line("#   - A Lambda function and IAM role.")?;
    out.line("#   - Seed + warmup + measurement traffic.")?;
    out.line("#")?;
    out.line("# USE AN AWS ACCOUNT DEDICATED TO TESTING.")?;
    out.line("# Do NOT use production or any account holding real user data. The teardown")?;
    out.line("# script is generated separately and must be run by you.")?;
    out.line(bar)?;
    out.line("")?;
    out.line(&"=".repeat(72))?;
    out.line("Caller identity confirmation:")?;
    out.line(&format!("  Account: {}", identity.account))?;
    out.line(&format!("  Alias:   {alias}"))?;
    out.line(&format!("  ARN:     {}", quote_untrusted(&identity.arn)))?;
    out.line(&format!("  Region:  {region}"))?;
    out.line(&"=".repeat(72))
}

/// `main()` of `deploy_model.py`, minus every AWS write.
fn run_deploy_model(
    args: &DeployArgs<'_>,
    client: &mut dyn IdentityClient,
    clock: &RunClock,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if !args.yes_deploy {
        return Err(die("refusing to deploy without --yes-deploy"));
    }
    let design_json = load(args.model, "the design JSON")?;
    let config_json = load(args.config, "the benchmark config")?;
    design::validate_design(&design_json)?;
    let settings = settings(&config_json, clock)?;
    if settings.prefix_generated {
        out.line(&format!(
            "No resource_prefix supplied — generated {} for this run (override by setting \
             resource_prefix in the config).",
            quote_untrusted(settings.prefix.trim_end_matches('-'))
        ))?;
    }

    let identity = resolve_identity(client, &settings.profile)?;
    refuse_production(&identity)?;
    banner(out, &"#".repeat(72), &settings.region, &identity)?;

    if let Some((given, _)) = &settings.global_provisioned {
        out.line(&format!(
            "Provisioned-capacity mode: {} (hard ceiling; bench-only — production designs \
             default to on-demand).",
            neutralize_line(&crate::dynamodb::pyfmt::repr(given))
        ))?;
    }
    let (plan, summary) = build_plan(&design_json, &settings, &identity, clock)?;
    out.line("")?;
    if settings.dry_run {
        out.line("DRY RUN — intended resources:")?;
    } else {
        out.line("PLAN ONLY — resources a live deploy would create (nothing was created):")?;
    }
    for (name, original) in &summary.tables {
        out.line(&format!("  table: {name} (from {original})"))?;
    }
    out.line(&format!(
        "  lambda: {}  (not built: this build only plans)",
        summary.function_name
    ))?;
    out.line(&format!("  role:   {}", summary.role_name))?;

    let text = ascii_escape(&plan.to_pretty_string()) + "\n";
    write_private(args.plan_out, &text, PRIVATE_FILE)?;
    out.line(&format!(
        "Plan written to {} (owner-only; it holds the account id and the IAM policy text).",
        safe_path(&args.plan_out.display().to_string())
    ))?;
    out.line(&format!(
        "No manifest was written to {}: this build creates nothing, so there is no \
         created-resources manifest. Review the plan; applying it is a follow-up.",
        safe_path(&args.manifest_out.display().to_string())
    ))
}

/// Handler of `toolkit dynamodb deploy-model`.
///
/// Without `--recorded FILE` this build has no AWS client, and the identity
/// step fails after the design and the config were validated (exit status 2).
///
/// # Errors
///
/// A usage error (exit status 2) for a missing option; a runtime error with
/// exit status 2 for a refused design, config, identity or output, like the
/// script's `ERROR:` exits.
pub fn deploy_model_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let model = Path::new(required(invocation, "--model")?);
    let config = Path::new(required(invocation, "--config")?);
    let manifest_out = Path::new(required(invocation, "--manifest-out")?);
    let clock = run_clock(invocation)?;
    let yes_deploy = invocation.has_flag("--yes-deploy");
    if !yes_deploy {
        return Err(die("refusing to deploy without --yes-deploy"));
    }
    if let Some(recorded_path) = invocation.value("--recorded") {
        let plan_out = Path::new(required(invocation, "--plan-out")?);
        let document = load(Path::new(recorded_path), "the recorded document")?;
        let mut client = RecordedClient::from_document(&document).map_err(|reason| {
            die(format!(
                "the recorded document {}: {reason}",
                safe_path(recorded_path)
            ))
        })?;
        let args = DeployArgs {
            model,
            config,
            manifest_out,
            plan_out,
            yes_deploy,
        };
        return run_deploy_model(&args, &mut client, &clock, out);
    }
    // No AWS client in this build: the design and config are validated, then
    // the identity step fails before anything is written.
    let args = DeployArgs {
        model,
        config,
        manifest_out,
        plan_out: Path::new(invocation.value("--plan-out").unwrap_or("")),
        yes_deploy,
    };
    run_deploy_model(&args, &mut NoLiveClient, &clock, out)
}

/// `main()` of `generate_teardown.py`.
fn run_generate_teardown(
    manifest_path: &Path,
    out_path: &Path,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if teardown::same_file(manifest_path, out_path) {
        return Err(die(
            "--out would overwrite the manifest; choose another path for the teardown script",
        ));
    }
    let shown_manifest = safe_path(&manifest_path.display().to_string());
    let manifest = match std::fs::symlink_metadata(manifest_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(die(format!("manifest not found: {shown_manifest}")));
        }
        _ => load(manifest_path, "the manifest")?,
    };
    let run = teardown::from_manifest(&manifest)?;
    let script = teardown::render(&run);
    write_private(out_path, &script, PRIVATE_EXECUTABLE)?;
    let shown = safe_path(&out_path.display().to_string());
    let command = if shown
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        shown.clone()
    } else {
        teardown::sh_quote(&shown)
    };
    out.line(&format!("Teardown script written to: {shown}"))?;
    out.line("")?;
    out.line("Review the script, then:")?;
    out.line(&format!(
        "  bash {command} --dry-run   # preview — calls no delete APIs"
    ))?;
    out.line(&format!(
        "  bash {command} --confirm   # delete the tables, Lambda, and IAM role"
    ))?;
    out.line(&format!(
        "  bash {command} --confirm --delete-logs   # also delete the Lambda's CloudWatch log \
         group (its run logs are gone for good)"
    ))
}

/// Handler of `toolkit dynamodb generate-teardown`.
///
/// The script is written only with `--yes-generate-teardown`: without it the
/// command writes nothing and exits with status 2. The command never runs the
/// script and never calls AWS.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing option; a runtime error with
/// exit status 2 for a missing consent flag, an unreadable or refused
/// manifest, or an unwritable output.
pub fn generate_teardown_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let manifest = Path::new(required(invocation, "--manifest")?);
    let out_path = Path::new(required(invocation, "--out")?);
    if !invocation.has_flag("--yes-generate-teardown") {
        return Err(die(
            "refusing to generate a teardown script without --yes-generate-teardown. The script \
             deletes DynamoDB tables, a Lambda function and an IAM role when it is later run with \
             --confirm; review the manifest, then re-run with the flag. Nothing was written.",
        ));
    }
    run_generate_teardown(manifest, out_path, out)
}
