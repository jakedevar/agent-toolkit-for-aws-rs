//! `toolkit cloudwatch instrumentation OP` and `toolkit cloudwatch snapshots
//! OP`: the host scripts `di_instrumentation.py` and `di_snapshots.py`.
//!
//! Both read the op's arguments as one JSON object (`--json TEXT`,
//! `--json-file FILE`, either one `-` for stdin), run the op over an injected
//! [`AwsCalls`] client and print the result text. The clients are the
//! recorded replay (`--recorded FILE`) or [`NoLiveClient`]; this build has no
//! live AWS client and sends nothing. `create`, `delete` and the two batch
//! deletes change live resources and are refused without `--i-have-confirmed`
//! before any client call.

use std::io::Write as _;
use std::path::Path;

use toolkit_core::{Json, ToolkitError};

use super::aws_client::{AwsCalls, NoLiveClient};
use super::cli::{
    invalid, now, op_name, read_arguments, spec_of, ArgSpec, Args,
    Origin::{Caller, JsonEncoded},
};
use super::crud_tools;
use super::gateway::Gateway;
use super::logs_query::VirtualPacer;
use super::pytext::{dumps_indent2, safe_block, safe_text};
use super::recorded::RecordedClient;
use super::result::OpResult;
use super::snapshot_tools;
use super::status_tools;
use super::timefmt::{parse_iso_timestamp, UtcTime};
use crate::safe_text::safe_path;
use crate::startup::read_json_document;
use crate::{Invocation, Output};

/// Value flags of `cloudwatch instrumentation`.
pub const INSTRUMENTATION_VALUE_FLAGS: &[&str] = &[
    "--json",
    "--json-file",
    "--region",
    "--profile",
    "--recorded",
    "--now",
];

/// Boolean flags of `cloudwatch instrumentation`.
pub const INSTRUMENTATION_FLAGS: &[&str] = &["--print-contract", "--i-have-confirmed"];

/// Value flags of `cloudwatch snapshots`.
pub const SNAPSHOT_VALUE_FLAGS: &[&str] = &[
    "--json",
    "--json-file",
    "--out",
    "--region",
    "--profile",
    "--recorded",
];

/// Boolean flags of `cloudwatch snapshots`.
pub const SNAPSHOT_FLAGS: &[&str] = &["--print-contract"];

/// The instrumentation ops, in contract order.
pub const INSTRUMENTATION_OPS: &[&str] = &[
    "create",
    "list",
    "get",
    "delete",
    "batch-delete-by-scope",
    "batch-delete-by-arns",
    "get-status",
    "check-status",
];

/// The snapshot ops, in contract order.
pub const SNAPSHOT_OPS: &[&str] = &["sample", "search"];

/// The ops that change live resources.
const MUTATING_OPS: &[&str] = &[
    "create",
    "delete",
    "batch-delete-by-scope",
    "batch-delete-by-arns",
];

/// The `application-signals` API version the operations were authored against.
const APPLICATION_SIGNALS_API_VERSION: &str = "2024-04-15";

const LOCATION_ARGUMENTS: [(&str, super::cli::Origin); 10] = [
    ("service", Caller),
    ("environment", Caller),
    ("instrumentation_type", Caller),
    ("location_hash", Caller),
    ("language", Caller),
    ("file_path", Caller),
    ("code_unit", Caller),
    ("class_name", Caller),
    ("method_name", Caller),
    ("line_number", Caller),
];

const CREATE_ARGUMENTS: ArgSpec = &[
    ("instrumentation_type", Caller),
    ("service", Caller),
    ("environment", Caller),
    ("language", Caller),
    ("file_path", Caller),
    ("code_unit", Caller),
    ("class_name", Caller),
    ("method_name", Caller),
    ("line_number", Caller),
    ("capture_arguments", Caller),
    ("capture_return", Caller),
    ("capture_stack_trace", Caller),
    ("capture_locals", Caller),
    ("max_hits", Caller),
    ("max_string_length", Caller),
    ("max_collection_width", Caller),
    ("max_collection_depth", Caller),
    ("max_stack_frames", Caller),
    ("max_stack_trace_size", Caller),
    ("max_object_depth", Caller),
    ("max_fields_per_object", Caller),
    ("attribute_filters", Caller),
    ("description", Caller),
    ("ttl_hours", Caller),
];

const LIST_ARGUMENTS: ArgSpec = &[
    ("service", Caller),
    ("environment", Caller),
    ("instrumentation_type", Caller),
    ("synced_at", Caller),
    ("max_results", Caller),
    ("next_token", Caller),
];

const LOOKUP_ARGUMENTS: ArgSpec = &LOCATION_ARGUMENTS;

const BATCH_SCOPE_ARGUMENTS: ArgSpec = &[
    ("service", Caller),
    ("environment", Caller),
    ("instrumentation_type", Caller),
];

const BATCH_ARNS_ARGUMENTS: ArgSpec =
    &[("resource_arns", Caller), ("instrumentation_type", Caller)];

const GET_STATUS_ARGUMENTS: ArgSpec = &[
    ("service", Caller),
    ("environment", Caller),
    ("instrumentation_type", Caller),
    ("location_hash", Caller),
    ("language", Caller),
    ("file_path", Caller),
    ("code_unit", Caller),
    ("class_name", Caller),
    ("method_name", Caller),
    ("line_number", Caller),
    ("status", Caller),
    ("start_time", Caller),
    ("end_time", Caller),
    ("max_results", Caller),
    ("next_token", Caller),
    ("signal_type", Caller),
];

const CHECK_STATUS_ARGUMENTS: ArgSpec = &[
    ("service", Caller),
    ("environment", Caller),
    ("instrumentation_type", Caller),
    ("location_hash", Caller),
    ("start_time", Caller),
    ("end_time", Caller),
    ("signal_type", Caller),
];

const SAMPLE_ARGUMENTS: ArgSpec = &[
    ("service", Caller),
    ("environment", Caller),
    ("location_hash", Caller),
    ("status_timestamp", Caller),
    ("max_timeout", Caller),
    ("include_raw", Caller),
];

/// `custom_filters` are raw Logs Insights fragments the caller appends on
/// purpose; they are only ever printed inside JSON output, where encoding is
/// their escaping step.
const SEARCH_ARGUMENTS: ArgSpec = &[
    ("service", Caller),
    ("environment", Caller),
    ("location_hash", Caller),
    ("status_timestamp", Caller),
    ("limit", Caller),
    ("max_timeout", Caller),
    ("custom_filters", JsonEncoded),
    ("start_time", Caller),
    ("end_time", Caller),
];

/// Every instrumentation op with every argument it accepts and that
/// argument's origin (the origin table of #4380).
pub const INSTRUMENTATION_ARGUMENTS: &[(&str, ArgSpec)] = &[
    ("create", CREATE_ARGUMENTS),
    ("list", LIST_ARGUMENTS),
    ("get", LOOKUP_ARGUMENTS),
    ("delete", LOOKUP_ARGUMENTS),
    ("batch-delete-by-scope", BATCH_SCOPE_ARGUMENTS),
    ("batch-delete-by-arns", BATCH_ARNS_ARGUMENTS),
    ("get-status", GET_STATUS_ARGUMENTS),
    ("check-status", CHECK_STATUS_ARGUMENTS),
];

/// Every snapshot op with every argument it accepts and that argument's origin.
pub const SNAPSHOT_ARGUMENTS: &[(&str, ArgSpec)] =
    &[("sample", SAMPLE_ARGUMENTS), ("search", SEARCH_ARGUMENTS)];

/// How the contract shows one parameter of an op signature.
#[derive(Clone, Copy)]
enum Param {
    Required,
    NoneValue,
    Int(i64),
    Str(&'static str),
    Bool(bool),
}

use Param::{Bool, Int, NoneValue, Required, Str};

type Signature = &'static [(&'static str, Param)];

const CREATE_SIGNATURE: Signature = &[
    ("instrumentation_type", Required),
    ("service", Required),
    ("environment", Required),
    ("language", NoneValue),
    ("file_path", NoneValue),
    ("code_unit", NoneValue),
    ("class_name", NoneValue),
    ("method_name", NoneValue),
    ("line_number", NoneValue),
    ("capture_arguments", NoneValue),
    ("capture_return", NoneValue),
    ("capture_stack_trace", NoneValue),
    ("capture_locals", NoneValue),
    ("max_hits", NoneValue),
    ("max_string_length", NoneValue),
    ("max_collection_width", NoneValue),
    ("max_collection_depth", NoneValue),
    ("max_stack_frames", NoneValue),
    ("max_stack_trace_size", NoneValue),
    ("max_object_depth", NoneValue),
    ("max_fields_per_object", NoneValue),
    ("attribute_filters", NoneValue),
    ("description", Str("dynamic instrumentation")),
    ("ttl_hours", NoneValue),
];

const LIST_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("instrumentation_type", Required),
    ("synced_at", NoneValue),
    ("max_results", Int(100)),
    ("next_token", NoneValue),
];

const LOOKUP_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("instrumentation_type", Required),
    ("location_hash", NoneValue),
    ("language", NoneValue),
    ("file_path", NoneValue),
    ("code_unit", NoneValue),
    ("class_name", NoneValue),
    ("method_name", NoneValue),
    ("line_number", NoneValue),
];

const BATCH_SCOPE_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("instrumentation_type", Required),
];

const BATCH_ARNS_SIGNATURE: Signature = &[
    ("resource_arns", Required),
    ("instrumentation_type", Required),
];

const GET_STATUS_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("instrumentation_type", Required),
    ("location_hash", NoneValue),
    ("language", NoneValue),
    ("file_path", NoneValue),
    ("code_unit", NoneValue),
    ("class_name", NoneValue),
    ("method_name", NoneValue),
    ("line_number", NoneValue),
    ("status", NoneValue),
    ("start_time", NoneValue),
    ("end_time", NoneValue),
    ("max_results", Int(100)),
    ("next_token", NoneValue),
    ("signal_type", Str("SNAPSHOT")),
];

const CHECK_STATUS_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("instrumentation_type", Required),
    ("location_hash", Required),
    ("start_time", Required),
    ("end_time", Required),
    ("signal_type", Str("SNAPSHOT")),
];

const SAMPLE_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("location_hash", Required),
    ("status_timestamp", Required),
    ("max_timeout", Int(30)),
    ("include_raw", Bool(false)),
];

const SEARCH_SIGNATURE: Signature = &[
    ("service", Required),
    ("environment", Required),
    ("location_hash", Required),
    ("status_timestamp", Required),
    ("limit", Int(10)),
    ("max_timeout", Int(30)),
    ("custom_filters", NoneValue),
    ("start_time", NoneValue),
    ("end_time", NoneValue),
];

const INSTRUMENTATION_SIGNATURES: &[(&str, Signature)] = &[
    ("create", CREATE_SIGNATURE),
    ("list", LIST_SIGNATURE),
    ("get", LOOKUP_SIGNATURE),
    ("delete", LOOKUP_SIGNATURE),
    ("batch-delete-by-scope", BATCH_SCOPE_SIGNATURE),
    ("batch-delete-by-arns", BATCH_ARNS_SIGNATURE),
    ("get-status", GET_STATUS_SIGNATURE),
    ("check-status", CHECK_STATUS_SIGNATURE),
];

const SNAPSHOT_SIGNATURES: &[(&str, Signature)] =
    &[("sample", SAMPLE_SIGNATURE), ("search", SEARCH_SIGNATURE)];

/// The semantic hints layered onto a parameter in the emitted contract: the
/// meaning the agent cannot infer from a bare name.
fn instrumentation_hint(name: &str) -> Vec<(&'static str, Json)> {
    match name {
        "instrumentation_type" => vec![
            (
                "enum",
                Json::Array(vec![Json::string("BREAKPOINT"), Json::string("PROBE")]),
            ),
            (
                "note",
                Json::string("required on every op; must match how the breakpoint was created"),
            ),
        ],
        "service" => vec![(
            "note",
            Json::string(
                "service identifier; `toolkit cloudwatch snapshots` uses the same key `service`",
            ),
        )],
        _ => Vec::new(),
    }
}

fn snapshot_hint(name: &str) -> Vec<(&'static str, Json)> {
    match name {
        "service" => vec![(
            "note",
            Json::string(
                "service identifier; `toolkit cloudwatch instrumentation` uses the same key \
                 `service`",
            ),
        )],
        "custom_filters" => vec![
            ("type", Json::string("array of strings")),
            (
                "note",
                Json::string(
                    "JSON array of raw Logs Insights filter fragments, appended with `and`, \
                     e.g. [\"@message like /ORD-123/\"]. A single bare string is also accepted \
                     and treated as a one-element list.",
                ),
            ),
        ],
        "start_time" => vec![(
            "note",
            Json::string(
                "optional ISO 8601 lower bound; pass with end_time to override the \
                 status_timestamp-anchored window and sweep a wider span (both or neither)",
            ),
        )],
        "end_time" => vec![(
            "note",
            Json::string("optional ISO 8601 upper bound; see start_time (both or neither)"),
        )],
        _ => Vec::new(),
    }
}

fn contract_ops(
    ops: &[&str],
    signatures: &[(&str, Signature)],
    hint: fn(&str) -> Vec<(&'static str, Json)>,
) -> Json {
    let mut out: Vec<(String, Json)> = Vec::new();
    for op in ops {
        let Some((_, signature)) = signatures.iter().find(|(name, _)| name == op) else {
            continue;
        };
        let mut args: Vec<(String, Json)> = Vec::new();
        for (name, default) in *signature {
            let mut entry: Vec<(String, Json)> = vec![(
                "required".to_string(),
                Json::Bool(matches!(default, Required)),
            )];
            match default {
                Int(value) => entry.push(("default".to_string(), Json::Int(*value))),
                Str(value) => entry.push(("default".to_string(), Json::string(*value))),
                Bool(value) => entry.push(("default".to_string(), Json::Bool(*value))),
                Required | NoneValue => {}
            }
            for (key, value) in hint(name) {
                entry.push((key.to_string(), value));
            }
            args.push(((*name).to_string(), Json::Object(entry)));
        }
        out.push((
            (*op).to_string(),
            Json::Object(vec![("args".to_string(), Json::Object(args))]),
        ));
    }
    Json::Object(out)
}

/// The canonical op and argument schema of `cloudwatch instrumentation`.
#[must_use]
pub fn instrumentation_contract() -> Json {
    Json::object(vec![
        ("api_version", Json::string(APPLICATION_SIGNALS_API_VERSION)),
        (
            "encoding",
            Json::string("toolkit cloudwatch instrumentation <op> --json-file args.json"),
        ),
        (
            "region",
            Json::string(
                "pass --region, or set AWS_REGION/AWS_DEFAULT_REGION (default us-east-1); \
                 use the region your instrumented service runs in",
            ),
        ),
        (
            "ops",
            contract_ops(
                INSTRUMENTATION_OPS,
                INSTRUMENTATION_SIGNATURES,
                instrumentation_hint,
            ),
        ),
    ])
}

/// The canonical op and argument schema of `cloudwatch snapshots`.
#[must_use]
pub fn snapshot_contract() -> Json {
    Json::object(vec![
        (
            "surface",
            Json::string("public CloudWatch Logs Insights (/aws/service-events/{service})"),
        ),
        (
            "encoding",
            Json::string("toolkit cloudwatch snapshots <op> --json '{<args>}' [--out FILE]"),
        ),
        (
            "region",
            Json::string(
                "pass --region, or set AWS_REGION/AWS_DEFAULT_REGION (default us-east-1); \
                 use the same region the breakpoint was created in",
            ),
        ),
        (
            "ops",
            contract_ops(SNAPSHOT_OPS, SNAPSHOT_SIGNATURES, snapshot_hint),
        ),
    ])
}

/// The client of a run: the recorded replay, or one that sends nothing.
pub(super) fn client_of(invocation: &Invocation) -> Result<Box<dyn AwsCalls>, ToolkitError> {
    let Some(path) = invocation.value("--recorded") else {
        return Ok(Box::new(NoLiveClient));
    };
    let document = read_json_document(Path::new(path))
        .map_err(|reason| ToolkitError::runtime("cloudwatch_recorded", reason))?;
    let client = RecordedClient::from_document(&document).map_err(|reason| {
        ToolkitError::runtime(
            "cloudwatch_recorded",
            format!("{}: {reason}", safe_path(path)),
        )
    })?;
    Ok(Box::new(client))
}

/// The clock of a run: `--now`, else the system clock.
pub(super) fn clock_of(invocation: &Invocation) -> Result<UtcTime, ToolkitError> {
    match invocation.value("--now") {
        None => Ok(now()),
        Some(text) => parse_iso_timestamp(text).map_err(|message| {
            ToolkitError::usage(
                "invalid_now",
                format!(
                    "--now is not an ISO 8601 timestamp: {}",
                    safe_text(&message)
                ),
            )
        }),
    }
}

/// `toolkit cloudwatch instrumentation OP (--json TEXT | --json-file FILE)
/// [--recorded FILE] [--now ISO] [--i-have-confirmed]`, or `--print-contract`.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing or unknown op or unusable
/// arguments, a runtime error for an unreadable or malformed recording.
pub fn instrumentation_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if invocation.has_flag("--print-contract") {
        return out.line(&dumps_indent2(&instrumentation_contract()));
    }
    let op = op_name(invocation, "op", INSTRUMENTATION_OPS)?;
    if MUTATING_OPS.contains(&op) && !invocation.has_flag("--i-have-confirmed") {
        out.set_exit_status(1);
        return out.line(&format!(
            "ERROR: refusing to run '{op}': it changes live resources. Confirm with the user \
             first, then re-run with --i-have-confirmed."
        ));
    }
    let payload = read_arguments(invocation, true)?;
    let now = clock_of(invocation)?;
    let args = Args::new(op, &payload, spec_of(INSTRUMENTATION_ARGUMENTS, op)?)?;
    let mut client = client_of(invocation)?;
    let mut gateway = Gateway::new(client.as_mut());
    let result = match op {
        "create" => crud_tools::create_instrumentation(&mut gateway, &args, now)?,
        "list" => crud_tools::list_instrumentations(&mut gateway, &args)?,
        "get" => crud_tools::get_instrumentation(&mut gateway, &args)?,
        "delete" => crud_tools::delete_instrumentation(&mut gateway, &args)?,
        "batch-delete-by-scope" => crud_tools::batch_delete_by_scope(&mut gateway, &args)?,
        "batch-delete-by-arns" => crud_tools::batch_delete_by_arns(&mut gateway, &args)?,
        "get-status" => status_tools::get_status(&mut gateway, &args)?,
        "check-status" => status_tools::check_status(&mut gateway, &args)?,
        other => return Err(invalid(other, "unknown op")),
    };
    emit(out, &result)
}

fn emit(out: &mut Output<'_>, result: &OpResult) -> Result<(), ToolkitError> {
    if !result.ok {
        out.set_exit_status(result.exit_status());
    }
    out.line(&safe_block(&result.text))
}

/// A point of [`write_owner_only_with`] where a test may change the file
/// system, to prove the write survives a swap (#4757).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteStage {
    /// The final path was checked and no file has been opened yet.
    Checked,
    /// The private temporary file is written, before the rename.
    TempWritten,
}

/// Writes `text` to `path` with owner-only permissions, refusing a symbolic
/// link or a non-regular file at the path. Snapshots may hold PII or secrets
/// from live request arguments.
fn write_owner_only(path: &Path, text: &str) -> Result<(), String> {
    write_owner_only_with(path, text, &mut |_| {})
}

/// [`write_owner_only`] with a hook at each [`WriteStage`].
///
/// The final path is never opened (#4757). The text goes to a new private
/// file in the same directory, created with `create_new` (`O_CREAT|O_EXCL`,
/// which never follows a link and never reuses an existing entry) and mode
/// 0600, and is then renamed over the final path. `rename` replaces the
/// directory entry itself, so a symbolic link swapped in after the check is
/// replaced, never followed: its target is not opened, truncated, chmodded
/// or written. A pre-existing symbolic link or non-regular file at the final
/// path is still refused up front, as the Python `O_NOFOLLOW` open refused
/// it. The parent directories are the caller's; none is created.
pub(super) fn write_owner_only_with(
    path: &Path,
    text: &str,
    hook: &mut dyn FnMut(WriteStage),
) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(found) if found.file_type().is_symlink() => {
            return Err("the path is a symbolic link".to_string());
        }
        Ok(found) if !found.is_file() => return Err("the path is not a regular file".to_string()),
        _ => {}
    }
    let name = path
        .file_name()
        .ok_or_else(|| "the path has no file name".to_string())?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    hook(WriteStage::Checked);

    let (temp, mut file) = create_private_temp(directory, name)?;
    let written = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // `create_new` applied 0600 through the umask; set it exactly.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        file.write_all(text.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        hook(WriteStage::TempWritten);
        std::fs::rename(&temp, path).map_err(|error| error.to_string())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// A new owner-only file `.<name>.<pid>.<n>.tmp` in `directory`, created
/// with `create_new` so it can be neither an existing file nor a link.
fn create_private_temp(
    directory: &Path,
    name: &std::ffi::OsStr,
) -> Result<(std::path::PathBuf, std::fs::File), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    for attempt in 0..16_u32 {
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            stamp.wrapping_add(attempt)
        ));
        let temp = directory.join(temp_name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("cannot create a private temporary file next to the path".to_string())
}

/// `toolkit cloudwatch snapshots OP (--json TEXT | --json-file FILE)
/// [--out FILE] [--recorded FILE]`, or `--print-contract`.
///
/// Exits 1 for a bare `ERROR` result and for a result whose inner `status` is
/// an AWS-side failure (`ERROR`, `TIMEOUT`, `FAILED`, `CANCELLED`).
///
/// # Errors
///
/// A usage error (exit status 2) for a missing or unknown op or unusable
/// arguments, a runtime error for an unreadable recording or an unwritable
/// `--out`.
pub fn snapshots_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if invocation.has_flag("--print-contract") {
        return out.line(&dumps_indent2(&snapshot_contract()));
    }
    let op = op_name(invocation, "op", SNAPSHOT_OPS)?;
    let payload = read_arguments(invocation, true)?;
    let args = Args::new(op, &payload, spec_of(SNAPSHOT_ARGUMENTS, op)?)?;
    let mut client = client_of(invocation)?;
    let mut pacer = VirtualPacer::default();
    let result = match op {
        "sample" => snapshot_tools::sample_snapshot(client.as_mut(), &mut pacer, &args)?,
        "search" => snapshot_tools::search_snapshots(client.as_mut(), &mut pacer, &args)?,
        other => return Err(invalid(other, "unknown op")),
    };
    if snapshot_tools::is_failure(&result) {
        out.set_exit_status(1);
    }
    if let Some(path) = invocation.value("--out") {
        write_owner_only(Path::new(path), &result).map_err(|reason| {
            ToolkitError::runtime(
                "cloudwatch_snapshot_write",
                format!("cannot write {}: {}", safe_path(path), safe_text(&reason)),
            )
        })?;
        out.line(&format!("wrote result to {} (0600)", safe_text(path)))
    } else {
        out.line(&safe_block(&result))
    }
}
