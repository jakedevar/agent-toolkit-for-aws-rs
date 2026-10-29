//! `toolkit msk ...` commands (slice #4387): argument parsing, file access and
//! exit-status mapping around the pure `toolkit-msk` library.
//!
//! The published behaviour of the three legacy scripts is kept: report text,
//! JSON documents, stderr formats and exit statuses are documented per command
//! in `crates/toolkit-cli/README.md` and `docs/migration/msk-contracts.md`.

#![allow(clippy::too_many_lines)]

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_core::{parse_json, read_regular_file, Json, PathError, ToolkitError};
use toolkit_msk::compat::{assess, format_utc_date, format_utc_timestamp, output_file_name};
use toolkit_msk::pricing::{UnlinkedPricingApi, PRICING_SOURCES};
use toolkit_msk::pyfmt::{error_json, json_dumps, py_float_parse, py_int_parse, py_str_repr};

use crate::safe_text::{redacted_value, shown_value};
use toolkit_msk::simulation::{
    compute, render_result_json, render_template, validate, DEFAULT_BROKER_COUNT,
    DEFAULT_INSTANCE_TYPE, SOURCE_TEMPLATE,
};
use toolkit_msk::size_report::{run_size, SizeOptions};
use toolkit_msk::sizing::{
    parse_broker_classes, parse_discount_pct, SizingInputs, DEFAULT_BROKER_QUOTA,
};

use crate::{Invocation, Output};

/// Registration of the `msk` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "msk",
    commands: &[
        crate::CommandSpec {
            path: &["msk", "size"],
            flags: SIZE_FLAGS,
            value_flags: SIZE_VALUE_FLAGS,
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: size,
        },
        crate::CommandSpec {
            path: &["msk", "compat"],
            flags: &[],
            value_flags: COMPAT_VALUE_FLAGS,
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 1,
            passthrough: false,
            handler: compat,
        },
        crate::CommandSpec {
            path: &["msk", "sim", "compute"],
            flags: &[],
            value_flags: SIM_COMPUTE_VALUE_FLAGS,
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: sim_compute,
        },
        crate::CommandSpec {
            path: &["msk", "sim", "render"],
            flags: &[],
            value_flags: SIM_RENDER_VALUE_FLAGS,
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: sim_render,
        },
        crate::CommandSpec {
            path: &["msk", "sim", "validate"],
            flags: &[],
            value_flags: SIM_VALIDATE_VALUE_FLAGS,
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: sim_validate,
        },
    ],
    synopsis: r"  toolkit msk size [--emit-static-pricing] [--use-max-partitions]
                   [--no-rack-affined-consumers] [--region REGION]
                   [--pricing-source auto|api|static]
                   [--avg-data-in-mbs N] [--peak-data-in-mbs N]
                   [--avg-data-out-mbs N] [--peak-data-out-mbs N]
                   [--num-partitions N] [--retention-hours N]
                   [--primary-retention-hours N] [--replication-factor N]
                   [--utilization-standard F] [--utilization-express F]
                   [--pst-per-broker-mbs N] [--discount-pct PCT]
                   [--broker-classes standard|express|standard,express]
                   [--broker-quota N]
  toolkit msk compat CLUSTER_CONFIG [--out-dir DIR]
  toolkit msk sim compute [--instance-type T] [--broker-count N]
                          [--kafka-version V]
  toolkit msk sim render --instance-type T --broker-count N
                         --kafka-version V --output PATH
  toolkit msk sim validate --test e2e_latency|broker_restart --config JSON
                           [--instance-type T] [--broker-count N]
",
    description: r"  msk size      MSK broker sizing and monthly cost report (MiB/s, GiB, USD).
                The workload options are required unless
                --emit-static-pricing is given. Only --pricing-source static
                (us-east-1) prices offline; `auto` falls back to it, `api`
                needs a build with an AWS Pricing API client.
  msk compat    Five-pillar MSK Express compatibility assessment of a
                discovery cluster-config.json. Writes
                compatibility.<cluster_name>.json into --out-dir (default:
                current directory) and reports the path and the overall
                verdict on stderr.
  msk sim       MSK Express load-test simulation: `compute` derives capacity
                and fleet sizes, `render` writes a CloudFormation template
                with the sizing baked in, `validate` returns PASS/REJECT with
                warnings for a proposed test config.
",
    exit_notes: "",
    doc: "docs/migration/msk-contracts.md",
    synopsis_rank: 210,
    description_rank: 210,
};

/// Boolean flags of `msk size`.
pub(crate) const SIZE_FLAGS: &[&str] = &[
    "--emit-static-pricing",
    "--use-max-partitions",
    "--no-rack-affined-consumers",
];
/// Value options of `msk size`.
pub(crate) const SIZE_VALUE_FLAGS: &[&str] = &[
    "--region",
    "--pricing-source",
    "--avg-data-in-mbs",
    "--peak-data-in-mbs",
    "--avg-data-out-mbs",
    "--peak-data-out-mbs",
    "--num-partitions",
    "--replication-factor",
    "--retention-hours",
    "--primary-retention-hours",
    "--utilization-standard",
    "--utilization-express",
    "--pst-per-broker-mbs",
    "--discount-pct",
    "--broker-classes",
    "--broker-quota",
];
/// Value options of `msk compat`.
pub(crate) const COMPAT_VALUE_FLAGS: &[&str] = &["--out-dir"];
/// Value options of `msk sim compute`.
pub(crate) const SIM_COMPUTE_VALUE_FLAGS: &[&str] =
    &["--instance-type", "--broker-count", "--kafka-version"];
/// Value options of `msk sim render`.
pub(crate) const SIM_RENDER_VALUE_FLAGS: &[&str] = &[
    "--instance-type",
    "--broker-count",
    "--kafka-version",
    "--output",
];
/// Value options of `msk sim validate`.
pub(crate) const SIM_VALIDATE_VALUE_FLAGS: &[&str] =
    &["--test", "--config", "--instance-type", "--broker-count"];

/// Upper bound for a discovery `cluster-config.json` (full config dumps of
/// large clusters exceed the 1 MiB default of the shared reader).
const MAX_CLUSTER_CONFIG_BYTES: u64 = 64 * 1024 * 1024;

/// Required workload options of `msk size`, in the legacy order.
const REQUIRED_WORKLOAD_OPTIONS: &[&str] = &[
    "--avg-data-in-mbs",
    "--peak-data-in-mbs",
    "--avg-data-out-mbs",
    "--peak-data-out-mbs",
    "--num-partitions",
    "--retention-hours",
    "--primary-retention-hours",
];

fn arg_error(message: impl Into<String>) -> ToolkitError {
    ToolkitError::usage("invalid_arguments", message)
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

fn float_option(invocation: &Invocation, name: &str) -> Result<Option<f64>, ToolkitError> {
    invocation
        .value(name)
        .map(|text| {
            py_float_parse(text).ok_or_else(|| {
                arg_error(format!(
                    "argument {name}: invalid float value: {}",
                    py_str_repr(&shown_value(text))
                ))
            })
        })
        .transpose()
}

fn int_option(invocation: &Invocation, name: &str) -> Result<Option<i64>, ToolkitError> {
    invocation
        .value(name)
        .map(|text| {
            py_int_parse(text)
                .and_then(|number| i64::try_from(number).ok())
                .ok_or_else(|| {
                    arg_error(format!(
                        "argument {name}: invalid int value: {}",
                        py_str_repr(&shown_value(text))
                    ))
                })
        })
        .transpose()
}

fn require_options(invocation: &Invocation, names: &[&str]) -> Result<(), ToolkitError> {
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| invocation.value(name).is_none())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(arg_error(format!(
            "the following arguments are required: {}",
            missing.join(", ")
        )))
    }
}

// ---------------------------------------------------------------------------
// msk size
// ---------------------------------------------------------------------------

fn size_options(invocation: &Invocation) -> Result<SizeOptions, ToolkitError> {
    let emit = invocation.has_flag("--emit-static-pricing");
    let pricing_source = invocation.value("--pricing-source").unwrap_or("auto");
    if !PRICING_SOURCES.contains(&pricing_source) {
        let choices: Vec<String> = PRICING_SOURCES
            .iter()
            .map(|choice| py_str_repr(choice))
            .collect();
        return Err(arg_error(format!(
            "argument --pricing-source: invalid choice: {} (choose from {})",
            py_str_repr(&shown_value(pricing_source)),
            choices.join(", ")
        )));
    }

    let avg_in = float_option(invocation, "--avg-data-in-mbs")?;
    let peak_in = float_option(invocation, "--peak-data-in-mbs")?;
    let avg_out = float_option(invocation, "--avg-data-out-mbs")?;
    let peak_out = float_option(invocation, "--peak-data-out-mbs")?;
    let partitions = int_option(invocation, "--num-partitions")?;
    let replication_factor = int_option(invocation, "--replication-factor")?.unwrap_or(3);
    let retention = int_option(invocation, "--retention-hours")?;
    let primary_retention = int_option(invocation, "--primary-retention-hours")?;
    let utilization_standard = float_option(invocation, "--utilization-standard")?.unwrap_or(0.50);
    let utilization_express = float_option(invocation, "--utilization-express")?.unwrap_or(0.75);
    let pst = float_option(invocation, "--pst-per-broker-mbs")?;
    let discount_pct = invocation
        .value("--discount-pct")
        .map(|text| {
            // A credential-shaped value is never valid here: the parser is
            // given the redacted value, so its message cannot quote it (#4941).
            parse_discount_pct(&redacted_value(text))
                .map_err(|message| arg_error(format!("argument --discount-pct: {message}")))
        })
        .transpose()?
        .unwrap_or(0.0);
    let broker_classes = parse_broker_classes(&redacted_value(
        invocation
            .value("--broker-classes")
            .unwrap_or("standard,express"),
    ))
    .map_err(|message| arg_error(format!("argument --broker-classes: {message}")))?;
    let broker_quota = int_option(invocation, "--broker-quota")?.unwrap_or(DEFAULT_BROKER_QUOTA);

    let inputs = if emit {
        // The workload only matters for sizing; --emit-static-pricing refreshes
        // price tables and ignores it.
        None
    } else {
        require_options(invocation, REQUIRED_WORKLOAD_OPTIONS)?;
        match (
            avg_in,
            peak_in,
            avg_out,
            peak_out,
            partitions,
            retention,
            primary_retention,
        ) {
            (
                Some(avg_data_in_mbs),
                Some(peak_data_in_mbs),
                Some(avg_data_out_mbs),
                Some(peak_data_out_mbs),
                Some(num_partitions),
                Some(retention_hours),
                Some(primary_retention_hours),
            ) => Some(SizingInputs {
                avg_data_in_mbs,
                peak_data_in_mbs,
                avg_data_out_mbs,
                peak_data_out_mbs,
                num_partitions,
                replication_factor,
                retention_hours,
                primary_retention_hours,
                utilization_standard,
                utilization_express,
                pst_per_broker_mbs: pst,
                use_max_partitions: invocation.has_flag("--use-max-partitions"),
                rack_affined_consumers: !invocation.has_flag("--no-rack-affined-consumers"),
            }),
            _ => None,
        }
    };

    Ok(SizeOptions {
        region: invocation
            .value("--region")
            .unwrap_or("us-east-1")
            .to_string(),
        pricing_source: pricing_source.to_string(),
        emit_static_pricing: emit,
        inputs,
        discount_pct,
        broker_classes,
        broker_quota,
    })
}

/// `toolkit msk size`: broker sizing and monthly cost calculator.
pub(crate) fn size(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let options = size_options(invocation)?;
    let today = format_utc_date(unix_seconds());
    let run = run_size(&options, &UnlinkedPricingApi, &today);
    for warning in &run.warnings {
        output.diagnostic(&format!("WARNING: {warning}"));
    }
    if let Some(message) = &run.error {
        output.diagnostic(&format!("ERROR: {message}"));
        output.set_exit_status(1);
        return Ok(());
    }
    for line in &run.stdout {
        output.line(line)?;
        if output.is_closed() {
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// msk compat
// ---------------------------------------------------------------------------

/// Writes `text` to `path`, refusing a symbolic link or a non-regular file at
/// the target (the legacy script followed links).
fn write_text_file(path: &Path, text: &str) -> Result<(), ToolkitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(PathError::Symlink(path.to_path_buf()).into());
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(PathError::NotRegularFile(path.to_path_buf()).into());
        }
        _ => {}
    }
    fs::write(path, text).map_err(|error| {
        ToolkitError::runtime(
            "io_error",
            format!("cannot write {}: {error}", path.display()),
        )
    })
}

/// `toolkit msk compat`: five-pillar MSK Express compatibility assessment.
pub(crate) fn compat(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let Some(config_path) = invocation.positional(0) else {
        return Err(arg_error(
            "the following arguments are required: cluster_config",
        ));
    };
    let text = read_regular_file(Path::new(config_path), MAX_CLUSTER_CONFIG_BYTES)?;
    let cfg = parse_json(&text)?;
    let assessed_at = format_utc_timestamp(unix_seconds());
    let doc = assess(&cfg, &assessed_at)
        .map_err(|message| ToolkitError::runtime("invalid_discovery_contract", message))?;
    let cluster_name = doc.get("cluster_name").cloned().unwrap_or(Json::Null);
    let file_name = output_file_name(&cluster_name)
        .map_err(|message| ToolkitError::runtime("invalid_cluster_name", message))?;
    let out_dir = match invocation.value("--out-dir") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().map_err(|error| {
            ToolkitError::runtime(
                "io_error",
                format!("cannot determine the current directory: {error}"),
            )
        })?,
    };
    let out_path = out_dir.join(file_name);
    write_text_file(&out_path, &format!("{}\n", json_dumps(&doc)))?;
    output.diagnostic(&out_path.display().to_string());
    output.diagnostic(&format!(
        "overall: {}",
        doc.get("overall").and_then(Json::as_str).unwrap_or("")
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// msk sim
// ---------------------------------------------------------------------------

/// Reports a simulation failure the legacy way: a JSON `{"error": ...}`
/// object on stderr and exit status 2.
// Always `Ok`: the handler signature lets call sites tail-return it.
#[allow(clippy::unnecessary_wraps)]
fn sim_error(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    output.diagnostic(&error_json(message));
    output.set_exit_status(2);
    Ok(())
}

fn sim_instance_and_count(invocation: &Invocation) -> Result<(String, i64), ToolkitError> {
    Ok((
        // Caller text that reaches messages and stdout: redacted at the value
        // (a credential-shaped value is never an instance type, #4941).
        redacted_value(
            invocation
                .value("--instance-type")
                .unwrap_or(DEFAULT_INSTANCE_TYPE),
        )
        .into_owned(),
        int_option(invocation, "--broker-count")?.unwrap_or(DEFAULT_BROKER_COUNT),
    ))
}

/// `toolkit msk sim compute`: derived capacity, fleet and test bounds.
pub(crate) fn sim_compute(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let (instance_type, broker_count) = sim_instance_and_count(invocation)?;
    match compute(
        &instance_type,
        broker_count,
        invocation
            .value("--kafka-version")
            .map(redacted_value)
            .as_deref(),
    ) {
        Ok(caps) => output.line(&json_dumps(&caps.to_json())),
        Err(message) => sim_error(output, &message),
    }
}

/// Makes `raw` absolute (home expansion, current directory) and resolves it in
/// filesystem order like Python's `Path(raw).expanduser().resolve()`: every
/// symbolic link below the final component is followed *before* a following
/// `..` is applied, so `link/../x` names the parent of the link's target, not
/// the parent of the link. Components that do not exist yet are kept as written
/// (a `..` after one removes it). The one difference from Python is the final
/// component: a symbolic link there is kept unresolved so that
/// [`write_text_file`] refuses it instead of writing through it.
///
/// No path is ever normalised lexically: `..` is applied only to a path whose
/// every component has already been resolved on disk (#4542). The check-then-use
/// window between this resolution and the write is the one the core path
/// contracts document (no handle-anchored directory walker exists in
/// `toolkit-core`).
fn resolve_output_path(raw: &str) -> Result<PathBuf, String> {
    let expanded = if raw == "~" || raw.starts_with("~/") {
        match std::env::var_os("HOME") {
            Some(home) => {
                PathBuf::from(home).join(raw.trim_start_matches('~').trim_start_matches('/'))
            }
            None => PathBuf::from(raw),
        }
    } else {
        PathBuf::from(raw)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot determine the current directory: {error}"))?
            .join(expanded)
    };
    let components: Vec<Component<'_>> = absolute.components().collect();
    // `resolved` is always a fully resolved (canonical) existing directory
    // path, so popping it for `..` is the physical parent; `tail` holds the
    // components that do not exist yet.
    let mut resolved = PathBuf::new();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if tail.pop().is_none() {
                    resolved.pop();
                }
            }
            Component::Normal(name) => {
                if !tail.is_empty() || index + 1 == components.len() {
                    // Either below a missing directory, or the final component,
                    // which is deliberately not resolved: a link there is
                    // refused when the file is written.
                    tail.push(name.to_os_string());
                    continue;
                }
                let candidate = resolved.join(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(_) => {
                        resolved = fs::canonicalize(&candidate).map_err(|error| {
                            format!("cannot resolve {}: {error}", candidate.display())
                        })?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        tail.push(name.to_os_string());
                    }
                    Err(error) => {
                        return Err(format!("cannot resolve {}: {error}", candidate.display()));
                    }
                }
            }
        }
    }
    resolved.extend(tail);
    if resolved.file_name().is_none() {
        return Err(format!("{} is not a file path", resolved.display()));
    }
    Ok(resolved)
}

/// The skill package directory (a directory with `SKILL.md` and
/// `assets/simulation-stack.yaml`) that contains `path`, if any.
fn enclosing_skill_package(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| {
            dir.join("SKILL.md").is_file()
                && dir.join("assets").join("simulation-stack.yaml").is_file()
        })
        .map(Path::to_path_buf)
}

/// `toolkit msk sim render`: write a deployable template with sizing baked in.
pub(crate) fn sim_render(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let broker_count = int_option(invocation, "--broker-count")?;
    require_options(
        invocation,
        &[
            "--instance-type",
            "--broker-count",
            "--kafka-version",
            "--output",
        ],
    )?;
    let (Some(instance_type), Some(broker_count), Some(kafka_version), Some(raw_output)) = (
        invocation.value("--instance-type"),
        broker_count,
        invocation.value("--kafka-version"),
        invocation.value("--output"),
    ) else {
        return Err(arg_error("missing required arguments"));
    };

    let rendered =
        match render_template(SOURCE_TEMPLATE, instance_type, broker_count, kafka_version) {
            Ok(rendered) => rendered,
            Err(message) => return sim_error(output, &message),
        };
    let out = match resolve_output_path(raw_output) {
        Ok(path) => path,
        Err(message) => return sim_error(output, &message),
    };
    // Safety rail: the rendered artifact must never be written into the skill
    // package, so the skill source stays clean and the customer-facing file
    // never points back at the install directory.
    if let Some(package) = enclosing_skill_package(&out) {
        return sim_error(
            output,
            &format!(
                "refusing to write the rendered template inside the skill package \
                 ({}); choose an output path under the customer's working \
                 directory instead, e.g. \
                 $(pwd)/migrate-to-msk-skill-artifacts/simulation/simulation-stack.yaml",
                package.display()
            ),
        );
    }
    if let Some(parent) = out.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return sim_error(
                output,
                &format!("cannot create {}: {error}", parent.display()),
            );
        }
    }
    if let Err(error) = write_text_file(&out, &rendered.template) {
        return sim_error(output, error.message());
    }
    let result = render_result_json(&rendered, &out.display().to_string());
    output.line(&json_dumps(&result))
}

/// `toolkit msk sim validate`: PASS/REJECT plus warnings for a test config.
pub(crate) fn sim_validate(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let (instance_type, broker_count) = sim_instance_and_count(invocation)?;
    require_options(invocation, &["--test", "--config"])?;
    let (Some(test), Some(config_text)) =
        (invocation.value("--test"), invocation.value("--config"))
    else {
        return Err(arg_error("missing required arguments"));
    };
    if test != "e2e_latency" && test != "broker_restart" {
        return Err(arg_error(format!(
            "argument --test: invalid choice: {} (choose from 'e2e_latency', 'broker_restart')",
            py_str_repr(&shown_value(test))
        )));
    }
    let caps = match compute(&instance_type, broker_count, None) {
        Ok(caps) => caps,
        Err(message) => return sim_error(output, &message),
    };
    let config = match parse_json(config_text) {
        Ok(config) => config,
        Err(error) => return sim_error(output, &format!("invalid --config JSON: {error}")),
    };
    if !matches!(config, Json::Object(_)) {
        return sim_error(output, "--config must be a JSON object");
    }
    match validate(test, &config, &caps) {
        Ok(result) => {
            output.line(&json_dumps(&result.to_json()))?;
            if !result.errors.is_empty() {
                output.set_exit_status(1);
            }
            Ok(())
        }
        Err(message) => sim_error(output, &message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run;
    use std::ffi::OsString;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "toolkit-msk-cli-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Runs the CLI in-process; returns (status, stdout, stderr).
    fn exec(values: &[&str]) -> (u8, String, String) {
        let args: Vec<OsString> = values.iter().map(OsString::from).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = run(&args, &mut out, &mut err);
        (
            status,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    const WORKLOAD: &[&str] = &[
        "--avg-data-in-mbs",
        "10",
        "--peak-data-in-mbs",
        "20",
        "--avg-data-out-mbs",
        "30",
        "--peak-data-out-mbs",
        "40",
        "--num-partitions",
        "3000",
        "--retention-hours",
        "168",
        "--primary-retention-hours",
        "24",
    ];

    fn size_args(extra: &[&'static str]) -> Vec<&'static str> {
        let mut args = vec!["msk", "size", "--pricing-source", "static"];
        args.extend(WORKLOAD.iter().copied());
        args.extend(extra.iter().copied());
        args
    }

    #[test]
    fn size_static_report_matches_the_worked_example() {
        let (status, stdout, stderr) = exec(&size_args(&[]));
        assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
        // Hand-computed from the legacy formulas: m5.large needs 6 brokers
        // (ingress and egress both ceil(4.25) = 5 -> 6), 6 * 0.21 * 730 =
        // 919.80, plus 506.25 EBS + 303.75 Tiered Storage + 342.1875 cross-AZ.
        for expected in [
            "NOTE: all cost figures below use us-east-1 public on-demand pricing,",
            "=== Sizing inputs ===",
            "=== Standard Sizing (using recommended partition limits per broker) ===",
            "=== Express Sizing (using recommended partition limits per broker) ===",
            "Shared monthly costs (same for every instance in this section):",
            "| broker type | required brokers | bottleneck | broker cost / mo | total cost / mo incl. shared |",
            "| kafka.m5.large | 6 | ingress | $919.80 | $2,071.99 |",
            "| kafka.m7g.large (Express) | 3 | ingress | $893.52 | $2,082.97 |",
            "=== Recommended pick per class (\u{2264} 60 brokers, lowest monthly cost) ===",
            "  Cost breakdown for each pick is in the sizing sections above.",
        ] {
            assert!(stdout.contains(expected), "missing {expected:?} in\n{stdout}");
        }
        for (label, amount) in [
            ("Storage", "506.25"),
            ("Tiered Storage", "303.75"),
            ("Cross-AZ", "342.19"),
            ("Subtotal", "1,152.19"),
        ] {
            let line = format!("  {label:<16} ${amount:>14}");
            assert!(stdout.contains(&line), "missing {line:?} in\n{stdout}");
        }
        assert!(
            stdout.starts_with('\n'),
            "the banner starts with a blank line"
        );
    }

    #[test]
    fn size_express_only_hides_standard_results() {
        let (status, stdout, _) = exec(&size_args(&["--broker-classes", "express"]));
        assert_eq!(status, 0);
        assert!(stdout.contains("Express Sizing"));
        assert!(!stdout.contains("Standard Sizing"));
        assert!(stdout.contains("  Express M7g: "));
        assert!(!stdout.contains("Standard M5"));
        // Without Standard results the Express utilization row has no suffix.
        assert!(stdout.contains("  utilization "));
        assert!(!stdout.contains("utilization (Express)"));
    }

    #[test]
    fn size_discount_and_max_partitions_change_the_report() {
        let (status, stdout, _) = exec(&size_args(&[
            "--discount-pct",
            "15",
            "--use-max-partitions",
            "--no-rack-affined-consumers",
        ]));
        assert_eq!(status, 0);
        assert!(stdout.contains("NOTE: cost figures below apply a 15% negotiated discount"));
        assert!(stdout.contains("(using maximum partition limits per broker)"));
        assert!(stdout.contains("-15%"));
        assert!(stdout.contains("consumers fetch across AZs"));
    }

    #[test]
    fn size_emit_static_pricing_prints_the_refresh_literals() {
        let (status, stdout, stderr) = exec(&[
            "msk",
            "size",
            "--pricing-source",
            "static",
            "--emit-static-pricing",
        ]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert!(stdout
            .starts_with("STATIC_PRICING_REGION = \"us-east-1\"\nSTATIC_PRICING_CAPTURED = \"20"));
        for expected in [
            "STATIC_BROKER_PRICE_PER_HR = {",
            "    \"kafka.m5.large\": 0.21,",
            "    \"kafka.m7g.16xlarge (Express)\": 13.056,",
            "STATIC_SHARED_PRICING = {",
            "    \"cross_az_cost_per_gb\": 0.02,",
            "    \"pst_cost_per_mbs_month\": 0.08,",
        ] {
            assert!(
                stdout.contains(expected),
                "missing {expected:?} in\n{stdout}"
            );
        }
    }

    #[test]
    fn size_usage_errors_exit_two() {
        let (status, stdout, stderr) = exec(&["msk", "size", "--pricing-source", "static"]);
        assert_eq!(status, 2);
        assert_eq!(stdout, "");
        assert!(stderr.starts_with(
            "error: the following arguments are required: --avg-data-in-mbs, --peak-data-in-mbs, \
             --avg-data-out-mbs, --peak-data-out-mbs, --num-partitions, --retention-hours, \
             --primary-retention-hours\n"
        ));
        assert!(stderr.contains("Usage:"));

        for (args, fragment) in [
            (
                vec!["--pricing-source", "bogus"],
                "argument --pricing-source: invalid choice: 'bogus' (choose from 'auto', 'api', 'static')",
            ),
            (
                vec!["--discount-pct", "100"],
                "argument --discount-pct: discount percentage must be >= 0 and < 100; got 100",
            ),
            (
                vec!["--discount-pct", "abc"],
                "argument --discount-pct: invalid discount percentage 'abc'; expected a number",
            ),
            (
                vec!["--broker-classes", "bogus"],
                "argument --broker-classes: invalid broker class 'bogus'; choose from: express, standard",
            ),
            (
                vec!["--broker-quota", "x"],
                "argument --broker-quota: invalid int value: 'x'",
            ),
            (
                vec!["--utilization-standard", "x"],
                "argument --utilization-standard: invalid float value: 'x'",
            ),
        ] {
            let mut full = size_args(&[]);
            full.extend(args);
            let (status, _, stderr) = exec(&full);
            assert_eq!(status, 2, "{full:?}");
            assert!(stderr.contains(fragment), "{fragment:?} not in {stderr}");
        }
    }

    #[test]
    fn size_runtime_errors_exit_one_with_the_legacy_prefix() {
        let mut args = size_args(&[]);
        args.extend(["--region", "eu-west-1"]);
        let (status, stdout, stderr) = exec(&args);
        assert_eq!(status, 1);
        assert_eq!(stdout, "");
        assert!(stderr.starts_with(
            "ERROR: static pricing covers only us-east-1, but --region eu-west-1 was requested."
        ));

        // The api pathway has no client in this build.
        let (status, _, stderr) = exec(&[
            "msk",
            "size",
            "--pricing-source",
            "api",
            "--emit-static-pricing",
        ]);
        assert_eq!(status, 1);
        assert!(stderr.starts_with("ERROR: this toolkit build has no AWS Pricing API client"));

        // auto falls back to static for us-east-1 with warnings...
        let mut auto = vec!["msk", "size"];
        auto.extend(WORKLOAD.iter().copied());
        let (status, stdout, stderr) = exec(&auto);
        assert_eq!(status, 0);
        assert!(stdout.contains("| kafka.m5.large | 6 |"));
        assert!(stderr.starts_with("WARNING: the AWS Pricing API is unavailable ("));
        assert!(stderr.contains(
            "\nWARNING: falling back to embedded us-east-1 pricing captured on 2026-10-05; rates may be stale."
        ));
        // ...but cannot for another region.
        auto.extend(["--region", "eu-west-1"]);
        let (status, _, stderr) = exec(&auto);
        assert_eq!(status, 1);
        assert!(stderr
            .contains("Falling back to static pricing is not possible for --region eu-west-1"));

        // Workload validation keeps the legacy messages.
        let (status, _, stderr) = exec(&[
            "msk",
            "size",
            "--pricing-source",
            "static",
            "--avg-data-in-mbs",
            "10",
            "--peak-data-in-mbs",
            "5",
            "--avg-data-out-mbs",
            "1",
            "--peak-data-out-mbs",
            "1",
            "--num-partitions",
            "1",
            "--retention-hours",
            "1",
            "--primary-retention-hours",
            "1",
        ]);
        assert_eq!(status, 1);
        assert_eq!(
            stderr,
            "ERROR: peak_data_in_mbs (5.0) must be >= avg_data_in_mbs (10.0)\n"
        );
    }

    #[test]
    fn size_with_huge_finite_throughput_is_a_clean_error() {
        // 1e30 MiB/s needs more brokers than an i64 can count (#4541).
        let (status, stdout, stderr) = exec(&[
            "msk",
            "size",
            "--pricing-source",
            "static",
            "--avg-data-in-mbs",
            "1e30",
            "--peak-data-in-mbs",
            "1e30",
            "--avg-data-out-mbs",
            "1e30",
            "--peak-data-out-mbs",
            "1e30",
            "--num-partitions",
            "3000",
            "--retention-hours",
            "168",
            "--primary-retention-hours",
            "24",
        ]);
        assert_eq!((status, stdout.as_str()), (1, ""));
        assert!(stderr.starts_with("ERROR: "), "{stderr}");
        assert!(stderr.contains("is not representable"), "{stderr}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
    }

    #[test]
    fn sim_compute_and_render_refuse_a_fleet_that_overflows() {
        // The largest multiple of three below i64::MAX.
        let huge = "9223372036854775806";
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "compute",
            "--instance-type",
            "express.m7g.16xlarge",
            "--broker-count",
            huge,
        ]);
        assert_eq!((status, stdout.as_str()), (2, ""));
        assert!(
            stderr.starts_with(
                "{\"error\": \"broker_count 9223372036854775806 is too large: the producer fleet"
            ),
            "{stderr}"
        );
        let dir = TempDir::new("render-overflow");
        let target = dir.path().join("stack.yaml");
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "render",
            "--instance-type",
            "express.m7g.16xlarge",
            "--broker-count",
            huge,
            "--kafka-version",
            "3.9.x.kraft",
            "--output",
            target.to_str().expect("utf-8"),
        ]);
        assert_eq!((status, stdout.as_str()), (2, ""));
        assert!(stderr.contains("is too large"), "{stderr}");
        assert!(!target.exists());
        // validate computes the same caps first.
        let (status, _, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--instance-type",
            "express.m7g.16xlarge",
            "--broker-count",
            huge,
            "--test",
            "e2e_latency",
            "--config",
            "{}",
        ]);
        assert_eq!(status, 2);
        assert!(stderr.contains("is too large"), "{stderr}");
    }

    #[test]
    fn sim_validate_rejects_the_signed_integer_minimum_duration() {
        // Used to panic (overflow checks) or wrap (release) on `duration - 2`.
        let config = r#"{"target_throughput_mbps":1,"duration_minutes":-9223372036854775808,"reboot_at_minute":2,"num_producers":1}"#;
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "broker_restart",
            "--config",
            config,
        ]);
        assert_eq!((status, stderr.as_str()), (1, ""));
        let result = parse_json(&stdout).expect("valid JSON");
        assert_eq!(
            result.get("decision").and_then(Json::as_str),
            Some("REJECT")
        );
        assert!(stdout.contains(
            "duration_minutes=-9223372036854775808 is outside the allowed range [10, 120]"
        ));
        assert!(stdout
            .contains("reboot_at_minute=2 is outside the allowed range [2, -9223372036854775810]"));
    }

    const COMPAT_CONFIG: &str = r#"{
      "cluster_name": "orders",
      "kafka": {"version": "3.5.1", "coordination_mechanism": "ZooKeeper"},
      "topology": {"num_brokers": 2, "num_azs": 2},
      "broker_configs": {"log.cleaner.max.compaction.lag.ms": "1000", "num.io.threads": 16},
      "topics": [{"name": "t1", "replication_factor": 2, "num_partitions": 10, "configs": {}}],
      "security": {"encryption_in_transit": "TLS", "authentication": "SASL_SCRAM"}
    }"#;

    #[test]
    fn compat_writes_the_document_and_reports_on_stderr() {
        let dir = TempDir::new("compat");
        let config = dir.path().join("cluster-config.json");
        fs::write(&config, COMPAT_CONFIG).expect("write config");
        let out_dir = dir.path().join("out");
        fs::create_dir_all(&out_dir).expect("out dir");
        let (status, stdout, stderr) = exec(&[
            "msk",
            "compat",
            config.to_str().expect("utf-8"),
            "--out-dir",
            out_dir.to_str().expect("utf-8"),
        ]);
        assert_eq!(status, 0, "{stderr}");
        assert_eq!(stdout, "");
        let written = out_dir.join("compatibility.orders.json");
        assert_eq!(
            stderr,
            format!("{}\noverall: ACTION_REQUIRED\n", written.display())
        );
        let text = fs::read_to_string(&written).expect("document written");
        assert!(text.ends_with("}\n"));
        assert!(text.is_ascii(), "json.dumps escapes non-ASCII characters");
        assert!(text.contains("\\u2014"), "em dash escaped");
        let doc = parse_json(&text).expect("valid JSON");
        let keys: Vec<&str> = match &doc {
            Json::Object(pairs) => pairs.iter().map(|(key, _)| key.as_str()).collect(),
            _ => Vec::new(),
        };
        assert_eq!(
            keys,
            [
                "cluster_name",
                "assessed_at",
                "overall",
                "pillars",
                "summary"
            ]
        );
        let assessed_at = doc
            .get("assessed_at")
            .and_then(Json::as_str)
            .expect("timestamp");
        assert_eq!(assessed_at.len(), 20);
        assert!(assessed_at.ends_with('Z') && assessed_at.contains('T'));
        let pillar = |name: &str| {
            doc.get("pillars")
                .and_then(|pillars| pillars.get(name))
                .and_then(|entry| entry.get("verdict"))
                .and_then(Json::as_str)
                .map(str::to_string)
        };
        assert_eq!(pillar("topology").as_deref(), Some("ADVISORY"));
        assert_eq!(pillar("kafka_version").as_deref(), Some("ADVISORY"));
        assert_eq!(pillar("configs").as_deref(), Some("ACTION_REQUIRED"));
        assert_eq!(pillar("auth").as_deref(), Some("INFO"));
        assert_eq!(pillar("quotas").as_deref(), Some("ADVISORY"));
    }

    #[test]
    fn compat_rejects_bad_input_with_exit_one_or_two() {
        let dir = TempDir::new("compat-bad");
        let missing = dir.path().join("absent.json");
        let (status, _, stderr) = exec(&["msk", "compat", missing.to_str().expect("utf-8")]);
        assert_eq!(status, 1);
        assert!(stderr.starts_with("error: path does not exist: "));

        let broken = dir.path().join("broken.json");
        fs::write(&broken, "{\"cluster_name\": ").expect("write");
        let (status, _, stderr) = exec(&["msk", "compat", broken.to_str().expect("utf-8")]);
        assert_eq!(status, 1);
        assert!(stderr.contains("JSON parse error"), "{stderr}");

        let incomplete = dir.path().join("incomplete.json");
        fs::write(
            &incomplete,
            r#"{"cluster_name": "x", "kafka": {"version": "3.6"}}"#,
        )
        .expect("write");
        let (status, _, stderr) = exec(&["msk", "compat", incomplete.to_str().expect("utf-8")]);
        assert_eq!(status, 1);
        assert_eq!(
            stderr,
            "error: discovery contract missing top-level fields: ['topology', 'topics']\n"
        );

        let (status, stdout, stderr) = exec(&["msk", "compat"]);
        assert_eq!(status, 2);
        assert_eq!(stdout, "");
        assert!(stderr.starts_with("error: the following arguments are required: cluster_config"));
    }

    #[test]
    fn compat_refuses_path_traversal_in_cluster_name() {
        let dir = TempDir::new("compat-name");
        let config = dir.path().join("evil.json");
        fs::write(
            &config,
            r#"{"cluster_name": "../escape", "kafka": {"version": "3.6"},
                "topology": {"num_brokers": 3}, "topics": []}"#,
        )
        .expect("write");
        let out_dir = dir.path().join("out");
        fs::create_dir_all(&out_dir).expect("out dir");
        let (status, _, stderr) = exec(&[
            "msk",
            "compat",
            config.to_str().expect("utf-8"),
            "--out-dir",
            out_dir.to_str().expect("utf-8"),
        ]);
        assert_eq!(status, 1);
        assert!(stderr.contains("cluster_name '../escape' cannot be used in an output file name"));
        assert!(!dir.path().join("compatibility.../escape.json").exists());
        assert_eq!(fs::read_dir(&out_dir).expect("readdir").count(), 0);
        assert!(!dir.path().join("escape.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn compat_refuses_symbolic_links_for_input_and_output() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new("compat-link");
        let config = dir.path().join("cluster-config.json");
        fs::write(&config, COMPAT_CONFIG).expect("write config");

        let link = dir.path().join("link.json");
        symlink(&config, &link).expect("symlink");
        let (status, _, stderr) = exec(&["msk", "compat", link.to_str().expect("utf-8")]);
        assert_eq!(status, 1);
        assert!(
            stderr.contains("refusing to follow symbolic link"),
            "{stderr}"
        );

        let out_dir = dir.path().join("out");
        fs::create_dir_all(&out_dir).expect("out dir");
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "keep").expect("write");
        symlink(&victim, out_dir.join("compatibility.orders.json")).expect("symlink");
        let (status, _, stderr) = exec(&[
            "msk",
            "compat",
            config.to_str().expect("utf-8"),
            "--out-dir",
            out_dir.to_str().expect("utf-8"),
        ]);
        assert_eq!(status, 1);
        assert!(
            stderr.contains("refusing to follow symbolic link"),
            "{stderr}"
        );
        assert_eq!(fs::read_to_string(&victim).expect("read"), "keep");
    }

    #[test]
    fn sim_compute_prints_the_derived_document() {
        let (status, stdout, stderr) = exec(&["msk", "sim", "compute"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        let doc = parse_json(&stdout).expect("valid JSON");
        let int = |key: &str| {
            doc.get(key).and_then(|value| match value {
                Json::Int(number) => Some(*number),
                _ => None,
            })
        };
        // express.m7g.4xlarge x 12: max ingress 187.5 * 12 = 2250.0 MB/s,
        // fleet = ceil(2250.0 * 1.5 / 400) = 9 producers = 3600 MB/s.
        assert_eq!(int("broker_count"), Some(12));
        assert_eq!(int("fleet_producer_count"), Some(9));
        assert_eq!(int("fleet_max_mbps"), Some(3600));
        assert_eq!(int("default_target_throughput_mbps"), Some(1199));
        assert!(stdout.contains("\"cluster_max_ingress_mbps\": 2250.0,"));
        assert!(stdout.starts_with("{\n  \"instance_type\": \"express.m7g.4xlarge\",\n"));
    }

    #[test]
    fn sim_errors_are_json_on_stderr_with_exit_two() {
        for (args, fragment) in [
            (
                vec!["msk", "sim", "compute", "--broker-count", "4"],
                "{\"error\": \"broker_count must be a multiple of 3 and >= 3\"}\n",
            ),
            (
                vec!["msk", "sim", "compute", "--instance-type", "x"],
                "{\"error\": \"unknown instance_type 'x'; valid: ['express.m7g.large'",
            ),
            (
                vec!["msk", "sim", "compute", "--kafka-version", "3.9.x"],
                "unsupported kafka_version '3.9.x'; valid: ['3.6.0', '3.8.x', '3.9.x.kraft'] (3.9 is KRaft-only on Express)",
            ),
        ] {
            let (status, stdout, stderr) = exec(&args);
            assert_eq!(status, 2, "{args:?}");
            assert_eq!(stdout, "");
            assert!(stderr.contains(fragment), "{fragment:?} not in {stderr}");
            assert!(!stderr.contains("Usage:"));
        }
    }

    #[test]
    fn sim_validate_pass_warn_and_reject() {
        let pass =
            r#"{"throughput_mbps":10,"duration_minutes":10,"num_producers":1,"num_consumers":1}"#;
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "e2e_latency",
            "--config",
            pass,
        ]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(
            stdout,
            "{\n  \"decision\": \"PASS\",\n  \"errors\": [],\n  \"warnings\": []\n}\n"
        );

        let warn =
            r#"{"throughput_mbps":2000,"duration_minutes":10,"num_producers":1,"num_consumers":1}"#;
        let (status, stdout, _) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "e2e_latency",
            "--config",
            warn,
        ]);
        assert_eq!(status, 0);
        assert!(stdout.contains("\"decision\": \"PASS\""));
        assert!(stdout.contains(
            "throughput_mbps=2000 MB/s is above the cluster's sustained/recommended ingress (1498.8 MB/s)"
        ));
        assert!(
            stdout.contains("num_producers=1 may not sustain 2000 MB/s; consider >= 5 producers.")
        );
        assert!(stdout.is_ascii());

        let reject = r#"{"target_throughput_mbps":99999,"duration_minutes":10,"reboot_at_minute":9,"num_producers":1}"#;
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "broker_restart",
            "--config",
            reject,
        ]);
        assert_eq!(status, 1, "REJECT exits 1");
        assert_eq!(stderr, "");
        assert!(stdout.contains("\"decision\": \"REJECT\""));
        assert!(
            stdout.contains("target_throughput_mbps=99999 is outside the allowed range [1, 3600]")
        );
        assert!(stdout.contains("reboot_at_minute=9 is outside the allowed range [2, 8]"));
    }

    #[test]
    fn sim_validate_input_errors() {
        let (status, _, stderr) = exec(&["msk", "sim", "validate", "--config", "{}"]);
        assert_eq!(status, 2);
        assert!(stderr.starts_with("error: the following arguments are required: --test\n"));

        let (status, _, stderr) =
            exec(&["msk", "sim", "validate", "--test", "nope", "--config", "{}"]);
        assert_eq!(status, 2);
        assert!(stderr.contains("argument --test: invalid choice: 'nope'"));

        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "e2e_latency",
            "--config",
            "{}",
        ]);
        assert_eq!((status, stdout.as_str()), (2, ""));
        assert_eq!(stderr, "{\"error\": \"'throughput_mbps'\"}\n");

        let (status, _, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "e2e_latency",
            "--config",
            "[1]",
        ]);
        assert_eq!(status, 2);
        assert_eq!(stderr, "{\"error\": \"--config must be a JSON object\"}\n");

        let (status, _, stderr) = exec(&[
            "msk",
            "sim",
            "validate",
            "--test",
            "e2e_latency",
            "--config",
            "{nope",
        ]);
        assert_eq!(status, 2);
        assert!(stderr.starts_with("{\"error\": \"invalid --config JSON: JSON parse error"));
    }

    #[test]
    fn sim_render_writes_the_baked_template_and_refuses_the_skill_package() {
        let dir = TempDir::new("render");
        let output_path = dir.path().join("work").join("simulation-stack.yaml");
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "render",
            "--instance-type",
            "express.m7g.4xlarge",
            "--broker-count=12",
            "--kafka-version",
            "3.9.x.kraft",
            "--output",
            output_path.to_str().expect("utf-8"),
        ]);
        assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
        let written = fs::read_to_string(&output_path).expect("template written");
        assert!(written.ends_with('\n'));
        assert_eq!(written.lines().count(), SOURCE_TEMPLATE.lines().count());
        let result = parse_json(&stdout).expect("valid JSON");
        let canonical = fs::canonicalize(&output_path).expect("canonical output");
        assert_eq!(
            result.get("output_path").and_then(Json::as_str),
            canonical.to_str()
        );
        assert!(stdout.contains("\"ProducerCount\": 9,"));
        assert!(stdout.contains("--template-body file://"));

        // A package directory: SKILL.md plus assets/simulation-stack.yaml.
        let package = dir.path().join("pkg");
        fs::create_dir_all(package.join("assets")).expect("assets dir");
        fs::write(package.join("SKILL.md"), "---\nname: x\n---\n").expect("skill");
        fs::write(package.join("assets").join("simulation-stack.yaml"), "x").expect("asset");
        let inside = package.join("out").join("stack.yaml");
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "render",
            "--instance-type",
            "express.m7g.4xlarge",
            "--broker-count",
            "12",
            "--kafka-version",
            "3.9.x.kraft",
            "--output",
            inside.to_str().expect("utf-8"),
        ]);
        assert_eq!((status, stdout.as_str()), (2, ""));
        assert!(stderr.starts_with(
            "{\"error\": \"refusing to write the rendered template inside the skill package ("
        ));
        assert!(!inside.exists());
        assert!(!package.join("out").exists());
    }

    #[test]
    fn sim_render_rejects_bad_sizing_and_missing_options() {
        let (status, _, stderr) = exec(&["msk", "sim", "render", "--instance-type", "x"]);
        assert_eq!(status, 2);
        assert!(stderr.starts_with(
            "error: the following arguments are required: --broker-count, --kafka-version, --output\n"
        ));

        let dir = TempDir::new("render-bad");
        let target = dir.path().join("stack.yaml");
        let (status, _, stderr) = exec(&[
            "msk",
            "sim",
            "render",
            "--instance-type",
            "express.m7g.4xlarge",
            "--broker-count",
            "12",
            "--kafka-version",
            "3.9.x",
            "--output",
            target.to_str().expect("utf-8"),
        ]);
        assert_eq!(status, 2);
        assert!(stderr.contains("unsupported kafka_version '3.9.x'"));
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sim_render_refuses_to_write_through_a_symbolic_link() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new("render-link");
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "keep").expect("write");
        let link = dir.path().join("stack.yaml");
        symlink(&victim, &link).expect("symlink");
        let (status, _, stderr) = exec(&[
            "msk",
            "sim",
            "render",
            "--instance-type",
            "express.m7g.4xlarge",
            "--broker-count",
            "12",
            "--kafka-version",
            "3.9.x.kraft",
            "--output",
            link.to_str().expect("utf-8"),
        ]);
        assert_eq!(status, 2);
        assert!(
            stderr.contains("refusing to follow symbolic link"),
            "{stderr}"
        );
        assert_eq!(fs::read_to_string(&victim).expect("read"), "keep");
    }

    #[test]
    fn output_path_resolution_is_absolute_and_removes_missing_components() {
        let cwd = std::env::current_dir().expect("cwd");
        let resolved = resolve_output_path("a/./b/../c.yaml").expect("resolves");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("a/c.yaml"));
        assert!(resolved.starts_with(fs::canonicalize(&cwd).expect("canonical cwd")));
        // A `..` above the root stays at the root; the root is not a file.
        assert_eq!(
            resolve_output_path("/../../x.yaml").expect("resolves"),
            PathBuf::from("/x.yaml")
        );
        assert!(resolve_output_path("/").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parent_traversal_is_applied_after_symbolic_links_are_resolved() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new("render-dotdot");
        let base = fs::canonicalize(dir.path()).expect("canonical temp dir");
        fs::create_dir_all(base.join("real").join("sub")).expect("dirs");
        // link -> real/sub, so `link/..` is `real`, not the directory of `link`.
        symlink(base.join("real").join("sub"), base.join("link")).expect("symlink");

        let through = |raw: &str| resolve_output_path(raw).expect("resolves");
        let spelled = |tail: &str| format!("{}/{tail}", base.display());
        assert_eq!(
            through(&spelled("link/../x.yaml")),
            base.join("real/x.yaml")
        );
        assert_eq!(
            through(&spelled("link/missing/../y.yaml")),
            base.join("real/sub/y.yaml")
        );
        assert_eq!(
            through(&spelled("missing/../link/../z.yaml")),
            base.join("real/z.yaml")
        );
        // Two links deep: both are followed before the `..`s are applied.
        symlink(base.join("link"), base.join("link2")).expect("symlink");
        assert_eq!(
            through(&spelled("link2/../../w.yaml")),
            base.join("w.yaml"),
            "link2 -> link -> real/sub, so `..` is real and `../..` is the base"
        );
        // The final component is never followed.
        symlink(base.join("real"), base.join("final")).expect("symlink");
        assert_eq!(through(&spelled("final")), base.join("final"));
    }

    #[cfg(unix)]
    #[test]
    fn sim_render_writes_where_the_resolved_parent_is_not_the_lexical_one() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new("render-link-dotdot");
        let base = fs::canonicalize(dir.path()).expect("canonical temp dir");
        fs::create_dir_all(base.join("customer").join("subdir")).expect("dirs");
        symlink(base.join("customer").join("subdir"), base.join("link")).expect("symlink");
        // Lexically `link/../stack.yaml` is `<base>/stack.yaml`; on disk it is
        // `<base>/customer/stack.yaml`. An unrelated file at the lexical
        // location must stay untouched (#4542).
        let unrelated = base.join("stack.yaml");
        fs::write(&unrelated, "keep").expect("write");
        let requested = format!("{}/link/../stack.yaml", base.display());
        let (status, stdout, stderr) = exec(&[
            "msk",
            "sim",
            "render",
            "--instance-type",
            "express.m7g.4xlarge",
            "--broker-count",
            "12",
            "--kafka-version",
            "3.9.x.kraft",
            "--output",
            &requested,
        ]);
        assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
        let expected = base.join("customer").join("stack.yaml");
        assert_eq!(fs::read_to_string(&unrelated).expect("read"), "keep");
        assert!(fs::read_to_string(&expected)
            .expect("template written at the resolved path")
            .contains("ProducerCount"));
        let result = parse_json(&stdout).expect("valid JSON");
        assert_eq!(
            result.get("output_path").and_then(Json::as_str),
            expected.to_str()
        );
    }
}
