//! Relational database helpers (#4394, slice `relational`).
//!
//! | Command | Python source |
//! |---|---|
//! | `toolkit dsql selftest`, `tenant-sql`, `tenant-query` | `aurora-dsql/scripts/safe_query.py`, `tenant_query.py` |
//! | `toolkit rds commitment [offline]` | `rds-oss/scripts/rds_commitment_pricing_analyzer.py` |
//! | `toolkit rds-oracle test-connection` | `rds-oracle/scripts/test_oracle_connection.py` |
//! | `toolkit timestream-influxdb validate-input`, `instance-types` | `timestream-influxdb/scripts/input_validator.py`, `instance_types.py` |
//!
//! No command opens a socket or reads a credential in this build. Every AWS or
//! database interaction goes through a seam (`rds::client::AwsRead`,
//! `oracle::OracleDriver`, `tenant::SqlCursor`) with a recorded-input
//! implementation (`--recorded FILE`) and a "no live client" implementation.
//! Live bindings are a follow-up. The parity table and the intentional
//! behaviour changes are in `docs/migration/relational.md`.

use std::path::Path;

use toolkit_aws::is_valid_region;
use toolkit_core::{neutralize_line, quote_untrusted, Json, ToolkitError};

use crate::aurora::args::{int_value, parse_py_int, reject_unrecognized, required, usage};
use crate::aurora::pyfmt::dumps;
use crate::safe_text::safe_path;
use crate::startup::read_json_document;
use crate::{Invocation, Output};

mod members;
mod oracle;
mod rds;
mod sql;
mod tenant;
mod timestream;

#[cfg(test)]
mod tests;

use oracle::{ConnectParams, NoLiveDriver, OracleDriver, RecordedDriver};
use rds::client::{AwsRead, NoLiveClient, RecordedClient};
use rds::{analyze_instance_live, build_comparison, Workload, ENGINES};
use tenant::{select_by_tenant, NoLiveCursor, RecordedCursor, SqlCursor, TenantRead};

/// Value flags of `dsql tenant-sql`.
pub const TENANT_SQL_VALUE_FLAGS: &[&str] = &["--tenant"];
/// Value flags of `dsql tenant-query`.
pub const TENANT_QUERY_VALUE_FLAGS: &[&str] = &["--tenant", "--recorded"];
/// Value flags of `rds commitment`.
pub const RDS_VALUE_FLAGS: &[&str] = &[
    "--region",
    "--format",
    "--instance",
    "--instance-type",
    "--engine",
    "--num-instances",
    "--recorded",
];
/// Boolean flags of `rds commitment`.
pub const RDS_FLAGS: &[&str] = &["--multi-az"];
/// Value flags of `rds-oracle test-connection`.
pub const ORACLE_VALUE_FLAGS: &[&str] = &["--recorded"];
/// Boolean flags of `rds-oracle test-connection`.
pub const ORACLE_FLAGS: &[&str] = &["--verbose"];

/// Flags that only exist for `rds commitment offline`.
const RDS_OFFLINE_ONLY: &[&str] = &[
    "--instance-type",
    "--engine",
    "--num-instances",
    "--multi-az",
];

const RDS_USAGE: &str = "\
usage: toolkit rds commitment [--region R] [--format json] [--instance ID] [--recorded FILE]
       toolkit rds commitment offline --instance-type TYPE --engine mysql|mariadb|postgres
           [--num-instances N] [--multi-az] [--region R] [--format json] [--recorded FILE]";

const ORACLE_USAGE: [&str; 3] = [
    "Usage: toolkit rds-oracle test-connection <endpoint> <port> <service_name> <username> [--verbose] [--recorded FILE]",
    "  This build opens no connection and reads no password: --recorded FILE replays a recorded session.",
    "Example: toolkit rds-oracle test-connection mydb.xxx.us-east-1.rds.amazonaws.com 1521 ORCL admin --recorded session.json",
];

/// Reads and validates a recording; the reason names the file (escaped) and
/// the wrong member, never a value.
fn load_recording<T>(
    path: &str,
    code: &'static str,
    parse: impl FnOnce(&Json) -> Result<T, String>,
) -> Result<T, ToolkitError> {
    let document = read_json_document(Path::new(path))
        .map_err(|reason| ToolkitError::runtime(code, reason))?;
    parse(&document)
        .map_err(|reason| ToolkitError::runtime(code, format!("{}: {reason}", safe_path(path))))
}

fn required_value<'a>(invocation: &'a Invocation, flag: &str) -> Result<&'a str, ToolkitError> {
    invocation
        .value(flag)
        .ok_or_else(|| usage(format!("the following arguments are required: {flag}")))
}

// ---------------------------------------------------------------- dsql

/// `toolkit dsql selftest`: the smoke test of `python safe_query.py`.
///
/// # Errors
///
/// A runtime error (exit 1) naming the failed check.
pub fn dsql_selftest_command(
    _invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    sql::selftest().map_err(|reason| ToolkitError::runtime("selftest_failed", reason))?;
    output.line("safe_query self-test passed")
}

/// `toolkit dsql tenant-sql --tenant SLUG`: prints the validated tenant
/// `SELECT`, for shell pipelines that cannot bind parameters.
///
/// # Errors
///
/// A usage error without `--tenant`; a runtime error (exit 1) when the tenant
/// is not a valid slug.
pub fn dsql_tenant_sql_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let tenant = required_value(invocation, "--tenant")?;
    let statement = TenantRead::for_tenant(tenant)
        .map_err(|error| ToolkitError::runtime("unsafe_sql", error.to_string()))?;
    output.line(statement.sql())
}

/// `toolkit dsql tenant-query --tenant SLUG [--recorded FILE]`: runs the
/// tenant read (the only statement a cursor accepts, `tenant::TenantRead`) and
/// prints the rows as JSON.
///
/// Without `--recorded` this build has no live connection and fails.
///
/// # Errors
///
/// A usage error without `--tenant`; a runtime error (exit 1) for an invalid
/// tenant, an unreadable or malformed recording or a cursor failure.
pub fn dsql_tenant_query_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let tenant = required_value(invocation, "--tenant")?;
    let mut recorded;
    let mut none;
    let cursor: &mut dyn SqlCursor = if let Some(path) = invocation.value("--recorded") {
        recorded = load_recording(path, "dsql_recorded", RecordedCursor::from_document)?;
        &mut recorded
    } else {
        none = NoLiveCursor;
        &mut none
    };
    let rows = select_by_tenant(cursor, &Json::string(tenant)).map_err(|error| {
        let code = match error {
            tenant::TenantQueryError::Database(_) => "dsql_query",
            _ => "unsafe_sql",
        };
        ToolkitError::runtime(
            code,
            toolkit_aws::redact_text(&neutralize_line(&error.to_string())),
        )
    })?;
    let document = Json::Array(rows.into_iter().map(Json::Array).collect());
    output.line(&dumps(&document))
}

// ----------------------------------------------------------------- rds

fn rds_region(invocation: &Invocation) -> Result<String, ToolkitError> {
    let region = invocation.value("--region").unwrap_or("us-east-1");
    if is_valid_region(region) {
        Ok(region.to_string())
    } else {
        Err(usage(format!(
            "argument --region: invalid region: {}",
            quote_untrusted(region)
        )))
    }
}

fn print_warnings(output: &mut Output<'_>, warnings: &[String]) {
    for warning in warnings {
        output.diagnostic(&format!("warning: {}", neutralize_line(warning)));
    }
}

/// `toolkit rds commitment [offline] ...`.
///
/// Prints the comparison JSON. `offline` prices `--instance-type` and
/// `--engine`; otherwise `--instance ID` is looked up through the client.
/// Without `--recorded` this build has no live client: the offline analysis
/// uses the bundled on-demand prices and finds no reserved or savings plan
/// rates, and a live lookup reports the failure in the JSON `error` member.
///
/// # Errors
///
/// A usage error (exit 2) for an unknown mode, an invalid option or a missing
/// required option; a runtime error (exit 1) for an unreadable or malformed
/// recording.
pub fn rds_commitment_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let offline = match invocation.positional(0) {
        None => false,
        Some("offline") => true,
        Some(other) => {
            return Err(usage(format!(
                "argument mode: invalid choice: {} (choose from 'offline')",
                quote_untrusted(other)
            )))
        }
    };
    let region = rds_region(invocation)?;
    match invocation.value("--format") {
        None | Some("json") => {}
        Some(other) => {
            return Err(usage(format!(
                "argument --format: invalid choice: {} (choose from 'json')",
                quote_untrusted(other)
            )))
        }
    }
    if !offline {
        reject_unrecognized(invocation, RDS_OFFLINE_ONLY)?;
    }
    let workload = if offline {
        let mut missing = Vec::new();
        for flag in ["--instance-type", "--engine"] {
            if invocation.value(flag).is_none() {
                missing.push(flag);
            }
        }
        required(&missing)?;
        let engine = required_value(invocation, "--engine")?;
        if !ENGINES.contains(&engine) {
            return Err(usage(format!(
                "argument --engine: invalid choice: {} (choose from 'mysql', 'mariadb', 'postgres')",
                quote_untrusted(engine)
            )));
        }
        Some((
            required_value(invocation, "--instance-type")?,
            engine,
            int_value(invocation, "--num-instances")?.unwrap_or(1),
            invocation.has_flag("--multi-az"),
        ))
    } else {
        None
    };
    if !offline && invocation.value("--instance").is_none() {
        return output.line(RDS_USAGE);
    }

    let mut recorded;
    let mut none;
    let client: &mut dyn AwsRead = if let Some(path) = invocation.value("--recorded") {
        recorded = load_recording(path, "rds_recorded", |document| {
            RecordedClient::from_document(document)
        })?;
        &mut recorded
    } else {
        none = NoLiveClient;
        &mut none
    };
    let comparison = if let Some((instance_type, engine, num_instances, multi_az)) = workload {
        build_comparison(
            client,
            &Workload {
                instance_type,
                engine,
                num_instances,
                region: &region,
                multi_az,
            },
            None,
        )
    } else {
        let instance = required_value(invocation, "--instance")?;
        analyze_instance_live(client, instance, &region)
    };
    print_warnings(output, &comparison.warnings);
    output.line(&dumps(&comparison.document))
}

// -------------------------------------------------------------- oracle

fn valid_text(value: &str, max: usize, accept: impl Fn(char) -> bool) -> bool {
    !value.is_empty() && value.len() <= max && value.chars().all(accept)
}

fn connect_params(invocation: &Invocation) -> Result<ConnectParams, ToolkitError> {
    let text = |index: usize| invocation.positional(index).unwrap_or_default();
    let endpoint = text(0);
    if !valid_text(endpoint, 255, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']')
    }) {
        return Err(usage(
            "invalid <endpoint>: use host name or IP characters only",
        ));
    }
    let port = text(1)
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0 && text(1).bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| usage("invalid <port>: expected a number from 1 to 65535"))?;
    let service_name = text(2);
    if !valid_text(service_name, 128, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '$' | '#')
    }) {
        return Err(usage(
            "invalid <service_name>: use letters, digits and . - _ $ # only",
        ));
    }
    let username = text(3);
    if !valid_text(username, 128, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '$' | '#')
    }) {
        return Err(usage(
            "invalid <username>: use letters, digits and . - _ $ # only",
        ));
    }
    Ok(ConnectParams {
        endpoint: endpoint.to_string(),
        port,
        service_name: service_name.to_string(),
        username: username.to_string(),
    })
}

/// `toolkit rds-oracle test-connection ENDPOINT PORT SERVICE_NAME USERNAME`.
///
/// Prints the four-check report of the script. Connection metadata (endpoint,
/// DSN, user name) is hidden unless `--verbose`. The report is exit status 1
/// when the driver or the connection fails and 0 otherwise, as in the script.
/// Fewer than four arguments print the usage on stdout and exit 1.
///
/// # Errors
///
/// A usage error (exit 2) for an invalid argument; a runtime error (exit 1)
/// for an unreadable or malformed recording.
pub fn oracle_test_connection_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if invocation.positionals.len() < 4 {
        for line in ORACLE_USAGE {
            output.line(line)?;
        }
        output.set_exit_status(1);
        return Ok(());
    }
    let params = connect_params(invocation)?;
    let verbose = invocation.has_flag("--verbose");
    let mut recorded;
    let mut none;
    let driver: &mut dyn OracleDriver = if let Some(path) = invocation.value("--recorded") {
        recorded = load_recording(path, "oracle_recorded", |document| {
            RecordedDriver::from_document(document)
        })?;
        &mut recorded
    } else {
        none = NoLiveDriver;
        &mut none
    };
    let report = oracle::run_checks(driver, &params, verbose);
    for line in &report.lines {
        output.line(line)?;
    }
    if report.status != 0 {
        output.set_exit_status(report.status);
    }
    Ok(())
}

// ---------------------------------------------------------- timestream

/// `toolkit timestream-influxdb validate-input [KEY=VALUE ...]`.
///
/// Checks the inputs of an AWS CLI call: `instance_name`, `instance_type`,
/// `region`, `vpc_id`, `subnet_id`, `sg_id`, `snapshot_id`, `storage_gb`.
/// Other keys have no validator and pass. Prints `All inputs valid.` or
/// `Validation FAILED:` with one line per bad input and exits 1.
///
/// # Errors
///
/// A usage error (exit 2) for an argument without `=`.
pub fn timestream_validate_input_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let mut pairs = Vec::new();
    for (index, argument) in invocation.positionals.iter().enumerate() {
        let Some((key, value)) = argument.split_once('=') else {
            return Err(usage(format!("argument {}: expected KEY=VALUE", index + 1)));
        };
        pairs.push((key.to_string(), value.to_string()));
    }
    let (lines, ok) = timestream::validate_all(&pairs);
    for line in &lines {
        output.line(line)?;
    }
    if !ok {
        output.set_exit_status(1);
    }
    Ok(())
}

/// `toolkit timestream-influxdb instance-types [recommend WRITES QUERIES DAYS]`.
///
/// Lists the instance types, or recommends one for a workload of
/// `WRITES` writes per second, `QUERIES` concurrent queries and `DAYS` days of
/// retention.
///
/// # Errors
///
/// A usage error (exit 2) for an unknown word or a missing or non-integer
/// workload number.
pub fn timestream_instance_types_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    match invocation.positional(0) {
        None => {
            for line in timestream::list_types() {
                output.line(&line)?;
            }
            Ok(())
        }
        Some("recommend") => {
            let mut numbers = [0_i64; 3];
            for (offset, name) in ["WRITES", "QUERIES", "DAYS"].into_iter().enumerate() {
                let text = invocation.positional(offset + 1).ok_or_else(|| {
                    usage("recommend requires WRITES QUERIES DAYS (three integers)")
                })?;
                numbers[offset] = parse_py_int(text).ok_or_else(|| {
                    usage(format!(
                        "invalid int value for {name}: {}",
                        quote_untrusted(text)
                    ))
                })?;
            }
            for line in timestream::recommend(numbers[0], numbers[1], numbers[2]) {
                output.line(&line)?;
            }
            Ok(())
        }
        Some(other) => Err(usage(format!(
            "unknown instance-types mode {}: use 'recommend' or no argument",
            quote_untrusted(other)
        ))),
    }
}
