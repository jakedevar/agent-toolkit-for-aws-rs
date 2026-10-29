//! `toolkit cloudwatch ...`: the offline command surface of the pure
//! CloudWatch logic.
//!
//! - `cloudwatch render RENDERER (--json TEXT | --json-file FILE)` runs one
//!   renderer over a JSON object of its arguments and prints the text.
//! - `cloudwatch validate CHECK (--json TEXT | --json-file FILE)` runs one
//!   input check; a failed check prints the legacy `ERROR: ...` text on
//!   stdout and exits 1, like an operation's `OpResult(ok=False, ...)`.
//! - `cloudwatch region [--region R]` resolves the region with the
//!   documented precedence.
//!
//! Argument objects use the legacy parameter names. An unknown or mistyped
//! argument is the legacy `TypeError` case: `error: invalid arguments for op
//! '<op>': ...` and exit status 2. Nothing here contacts AWS.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_core::{parse_json, read_regular_file, Json, ToolkitError};

use super::capture::CaptureLimits;
use super::crud_rendering::{
    format_batch_delete_response, render_create_success_message, render_get_instrumentation_output,
    render_list_instrumentations_output, CreateSummary,
};
use super::errors::{render_failure, AwsFailure, Prose};
use super::location::{
    location_from_response, parse_create_inputs, parse_lookup_inputs, render_location_block,
    LocationInputs,
};
use super::pytext::{dumps_indent2, safe_block, safe_text};
use super::region::resolve_region_from_env;
use super::result::OpResult;
use super::snapshot_parsing::parse_snapshot_fields;
use super::snapshot_rendering::{
    render_get_sample_snapshot_for_breakpoint_output,
    render_search_snapshots_for_status_event_output, QueryResult, SampleContext, SearchContext,
};
use super::status_assessment::{assess, StatusCheckResult};
use super::status_rendering::{
    render_get_instrumentation_configuration_status_output, render_status_assessment, StatusSubject,
};
use super::timefmt::{parse_iso_timestamp, UtcTime};
use super::validation::{
    is_valid_location_hash, normalize_instrumentation_type, validate_capture_names,
    validate_probe_constraints, validate_snapshot_signal,
};
use crate::safe_text::redacted_value;
use crate::{Invocation, Output};

/// Upper bound for an arguments file (16 MiB).
const MAX_ARGUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// The renderers `cloudwatch render` accepts.
pub const RENDERERS: &[&str] = &[
    "location",
    "create-success",
    "list",
    "get",
    "batch-delete",
    "status-history",
    "status-assessment",
    "snapshot-search",
    "snapshot-sample",
    "snapshot-fields",
    "failure",
];

/// The checks `cloudwatch validate` accepts.
pub const CHECKS: &[&str] = &[
    "instrumentation-type",
    "location-inputs",
    "capture-names",
    "probe",
    "location-hash",
    "snapshot-signal",
];

pub(super) fn invalid(op: &str, detail: impl AsRef<str>) -> ToolkitError {
    ToolkitError::runtime(
        "invalid_arguments",
        format!(
            "invalid arguments for op '{}': {}",
            safe_text(op),
            safe_text(detail.as_ref())
        ),
    )
    .with_exit_code(2)
}

/// Where one op argument comes from. It decides the one escaping step
/// [`Args::new`] applies when the arguments are parsed, so the rule lives in
/// one table instead of at each renderer call (#4652, #4653, #4662).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Caller text: every string inside it, and every object key, is made
    /// single-line with [`safe_text`] at parse time. No renderer ever sees a
    /// raw caller newline, carriage return, ESC or bidi control, whatever
    /// the field's type or nesting.
    Caller,
    /// Caller text the renderer prints only inside `json.dumps` output
    /// (`ensure_ascii`), where every newline, control and bidi character is
    /// already a JSON escape. JSON encoding is its one escaping step; the
    /// value is kept verbatim so a multi-line Logs Insights query keeps its
    /// JSON value.
    JsonEncoded,
    /// An AWS response object. Manager ruling on #4380: its free text
    /// (Description, NextToken, ...) may keep newlines; terminal controls are
    /// escaped by the final `safe_block` pass of `render_command`.
    AwsResponse,
    /// An object whose every member is an argument object of this spec (the
    /// recorded status checks: caller errors around AWS events).
    EachMember(ArgSpec),
}

/// The arguments of one op, each with its [`Origin`].
pub type ArgSpec = &'static [(&'static str, Origin)];

use Origin::{AwsResponse, Caller, EachMember, JsonEncoded};

/// One recorded status check of `status-assessment`.
pub const CHECK_RESULT_ARGUMENTS: ArgSpec = &[
    ("has_events", Caller),
    ("events", AwsResponse),
    ("error", Caller),
];

/// The `failure` object of the `failure` renderer.
const FAILURE_ARGUMENTS: ArgSpec = &[
    ("kind", Caller),
    ("code", Caller),
    ("message", Caller),
    ("display", Caller),
    ("type_name", Caller),
];

/// Every renderer with every argument it accepts and that argument's
/// [`Origin`]. [`Args::new`] rejects any other key, and reading a key not
/// listed here is a bug that panics in debug builds, so this table is the
/// complete list of renderer inputs; the caller-text test walks it.
pub const RENDERER_ARGUMENTS: &[(&str, ArgSpec)] = &[
    (
        "location",
        &[("location", Caller), ("location_hash", Caller)],
    ),
    (
        "create-success",
        &[
            ("response", AwsResponse),
            ("normalized_type", Caller),
            ("service", Caller),
            ("environment", Caller),
            ("location", Caller),
            ("ttl_hours", Caller),
            ("capture_arguments", Caller),
            ("capture_locals", Caller),
            ("is_line_level", Caller),
            ("capture_return", Caller),
            ("capture_stack_trace", Caller),
            ("limits", Caller),
            ("attribute_filters", Caller),
            ("now", Caller),
        ],
    ),
    (
        "list",
        &[
            ("data", AwsResponse),
            ("normalized_type", Caller),
            ("service", Caller),
            ("environment", Caller),
        ],
    ),
    (
        "get",
        &[
            ("config", AwsResponse),
            ("service", Caller),
            ("environment", Caller),
        ],
    ),
    (
        "batch-delete",
        &[
            ("mode", Caller),
            ("data", AwsResponse),
            ("instrumentation_type", Caller),
            ("service", Caller),
            ("environment", Caller),
        ],
    ),
    (
        "status-history",
        &[
            ("data", AwsResponse),
            ("normalized_type", Caller),
            ("service", Caller),
            ("environment", Caller),
            ("requested_status", Caller),
        ],
    ),
    (
        "status-assessment",
        &[
            ("created_at", Caller),
            ("requested_start", Caller),
            ("query_end", Caller),
            ("checks", EachMember(CHECK_RESULT_ARGUMENTS)),
            ("location_hash", Caller),
            ("service", Caller),
            ("environment", Caller),
            ("normalized_type", Caller),
        ],
    ),
    (
        "snapshot-search",
        &[
            ("query_result", AwsResponse),
            ("service_name", JsonEncoded),
            ("environment", JsonEncoded),
            ("location_hash", JsonEncoded),
            ("custom_filters", JsonEncoded),
            ("start_time_utc", JsonEncoded),
            ("end_time_utc", JsonEncoded),
            ("start_epoch", JsonEncoded),
            ("end_epoch", JsonEncoded),
            ("query_string", JsonEncoded),
        ],
    ),
    (
        "snapshot-sample",
        &[
            ("query_result", AwsResponse),
            ("service_name", JsonEncoded),
            ("environment", JsonEncoded),
            ("location_hash", JsonEncoded),
            ("start_time_utc", JsonEncoded),
            ("end_time_utc", JsonEncoded),
            ("max_timeout", JsonEncoded),
            ("query_string", JsonEncoded),
            ("include_raw", JsonEncoded),
        ],
    ),
    ("snapshot-fields", &[("result", AwsResponse)]),
    (
        "failure",
        &[
            ("failure", Caller),
            ("action", Caller),
            ("attempted", Caller),
            ("attempted_label", Caller),
            ("possible_causes", Caller),
            ("troubleshooting", Caller),
            ("trailer", Caller),
        ],
    ),
];

/// Every check of `cloudwatch validate` with its arguments: all caller text.
const CHECK_ARGUMENTS: &[(&str, ArgSpec)] = &[
    ("instrumentation-type", &[("instrumentation_type", Caller)]),
    (
        "location-inputs",
        &[
            ("mode", Caller),
            ("location_hash", Caller),
            ("language", Caller),
            ("file_path", Caller),
            ("code_unit", Caller),
            ("class_name", Caller),
            ("method_name", Caller),
            ("line_number", Caller),
            ("allow_code_location_lookup", Caller),
        ],
    ),
    (
        "capture-names",
        &[("field_name", Caller), ("names", Caller)],
    ),
    (
        "probe",
        &[
            ("instrumentation_type", Caller),
            ("language", Caller),
            ("line_number", Caller),
        ],
    ),
    ("location-hash", &[("location_hash", Caller)]),
    ("snapshot-signal", &[("signal_type", Caller)]),
];

pub(super) fn spec_of(table: &[(&str, ArgSpec)], op: &str) -> Result<ArgSpec, ToolkitError> {
    table
        .iter()
        .find(|(name, _)| *name == op)
        .map(|(_, spec)| *spec)
        .ok_or_else(|| invalid(op, "unknown op"))
}

fn origin_of(spec: ArgSpec, key: &str) -> Option<Origin> {
    spec.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, origin)| *origin)
}

/// Caller JSON with every string and every object key made single-line.
// recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
fn escape_caller_json(value: &Json) -> Json {
    match value {
        Json::String(text) => Json::String(safe_text(text)),
        Json::Array(items) => Json::Array(items.iter().map(escape_caller_json).collect()),
        Json::Object(pairs) => Json::Object(
            pairs
                .iter()
                .map(|(key, item)| (safe_text(key), escape_caller_json(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The parse-time escaping of one argument value by its [`Origin`].
fn escape_by_origin(value: &Json, origin: Origin) -> Json {
    match origin {
        Caller => escape_caller_json(value),
        JsonEncoded | AwsResponse => value.clone(),
        EachMember(spec) => match value {
            // recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
            Json::Object(members) => Json::Object(
                members
                    .iter()
                    .map(|(key, member)| (safe_text(key), escape_arguments(member, spec)))
                    .collect(),
            ),
            other => escape_caller_json(other),
        },
    }
}

/// An arguments object with each listed key escaped by its [`Origin`]. An
/// unlisted key (which [`Args::new`] refuses) or a non-object is treated as
/// caller text.
// recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
fn escape_arguments(value: &Json, spec: ArgSpec) -> Json {
    match value {
        Json::Object(pairs) => Json::Object(
            pairs
                .iter()
                .map(|(key, item)| match origin_of(spec, key) {
                    Some(origin) => (key.clone(), escape_by_origin(item, origin)),
                    None => (safe_text(key), escape_caller_json(item)),
                })
                .collect(),
        ),
        other => escape_caller_json(other),
    }
}

/// A typed view of the JSON arguments object of one op. It holds only the
/// arguments as escaped by [`escape_arguments`]: the raw caller object is not
/// kept, so no accessor can hand a renderer a raw caller string (#4662).
pub(super) struct Args {
    op: String,
    spec: ArgSpec,
    value: Json,
}

impl Args {
    pub(super) fn new(op: &str, value: &Json, spec: ArgSpec) -> Result<Self, ToolkitError> {
        // recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
        let Json::Object(pairs) = value else {
            return Err(invalid(op, "arguments must be a JSON object"));
        };
        if let Some((key, _)) = pairs.iter().find(|(key, _)| origin_of(spec, key).is_none()) {
            return Err(invalid(op, format!("got an unexpected argument '{key}'")));
        }
        Ok(Self {
            op: op.to_string(),
            spec,
            value: escape_arguments(value, spec),
        })
    }

    pub(super) fn present(&self, key: &str) -> Option<&Json> {
        debug_assert!(
            origin_of(self.spec, key).is_some(),
            "op '{}' reads argument '{key}' that its ArgSpec does not list",
            self.op
        );
        self.value.get(key).filter(|value| **value != Json::Null)
    }

    /// The argument as given, an explicit `null` included (`present` treats
    /// `null` as absent, like Python's `None` default).
    pub(super) fn explicit(&self, key: &str) -> Option<&Json> {
        debug_assert!(
            origin_of(self.spec, key).is_some(),
            "op '{}' reads argument '{key}' that its ArgSpec does not list",
            self.op
        );
        self.value.get(key)
    }

    pub(super) fn mistyped(&self, key: &str, expected: &str, got: &Json) -> ToolkitError {
        invalid(
            &self.op,
            format!(
                "argument '{key}' must be {expected}, got {}",
                got.type_name()
            ),
        )
    }

    pub(super) fn required(&self, key: &str) -> Result<&Json, ToolkitError> {
        self.present(key)
            .ok_or_else(|| invalid(&self.op, format!("missing required argument '{key}'")))
    }

    pub(super) fn opt_str(&self, key: &str) -> Result<Option<&str>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::String(text)) => Ok(Some(text.as_str())),
            Some(other) => Err(self.mistyped(key, "a string", other)),
        }
    }

    pub(super) fn req_str(&self, key: &str) -> Result<&str, ToolkitError> {
        match self.required(key)? {
            Json::String(text) => Ok(text.as_str()),
            other => Err(self.mistyped(key, "a string", other)),
        }
    }

    pub(super) fn opt_i64(&self, key: &str) -> Result<Option<i64>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Int(number)) => Ok(Some(*number)),
            Some(other) => Err(self.mistyped(key, "an integer", other)),
        }
    }

    pub(super) fn req_i64(&self, key: &str) -> Result<i64, ToolkitError> {
        self.opt_i64(key)?
            .ok_or_else(|| invalid(&self.op, format!("missing required argument '{key}'")))
    }

    pub(super) fn opt_bool(&self, key: &str) -> Result<Option<bool>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Bool(flag)) => Ok(Some(*flag)),
            Some(other) => Err(self.mistyped(key, "a boolean", other)),
        }
    }

    pub(super) fn opt_strings(&self, key: &str) -> Result<Option<Vec<String>>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            // recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
            Some(Json::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Json::String(text) => Ok(text.clone()),
                    other => Err(self.mistyped(key, "an array of strings", other)),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(other) => Err(self.mistyped(key, "an array of strings", other)),
        }
    }

    pub(super) fn opt_array(&self, key: &str) -> Result<&[Json], ToolkitError> {
        match self.present(key) {
            None => Ok(&[]),
            // recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
            Some(Json::Array(items)) => Ok(items),
            Some(other) => Err(self.mistyped(key, "an array", other)),
        }
    }

    pub(super) fn opt_time(&self, key: &str) -> Result<Option<UtcTime>, ToolkitError> {
        self.opt_str(key)?
            .map(|text| {
                parse_iso_timestamp(text)
                    .map_err(|message| invalid(&self.op, format!("argument '{key}': {message}")))
            })
            .transpose()
    }

    pub(super) fn req_time(&self, key: &str) -> Result<UtcTime, ToolkitError> {
        self.opt_time(key)?
            .ok_or_else(|| invalid(&self.op, format!("missing required argument '{key}'")))
    }
}

pub(super) fn read_arguments(invocation: &Invocation) -> Result<Json, ToolkitError> {
    let text = match (invocation.value("--json"), invocation.value("--json-file")) {
        (Some(_), Some(_)) => {
            return Err(ToolkitError::usage(
                "conflicting_arguments",
                "pass the arguments via exactly one of --json or --json-file",
            ))
        }
        (Some(text), None) => text.to_string(),
        (None, Some(path)) => {
            read_regular_file(Path::new(path), MAX_ARGUMENT_BYTES).map_err(|error| {
                ToolkitError::usage(
                    "arguments_unreadable",
                    format!(
                        "--json-file could not be read: {}",
                        safe_text(&error.to_string())
                    ),
                )
            })?
        }
        (None, None) => {
            return Err(ToolkitError::usage(
                "missing_arguments",
                "the op's arguments are required (use --json-file PATH or --json '{...}')",
            ))
        }
    };
    let payload = parse_json(&text).map_err(|error| {
        ToolkitError::usage(
            "invalid_json",
            format!(
                "arguments are not valid JSON: {}",
                safe_text(&error.to_string())
            ),
        )
    })?;
    // recorded-shape: the caller's --json arguments payload; not a response.
    if !matches!(payload, Json::Object(_)) {
        return Err(ToolkitError::usage(
            "arguments_not_object",
            "arguments must be a JSON object of the op's parameters",
        ));
    }
    Ok(payload)
}

pub(super) fn op_name<'a>(
    invocation: &'a Invocation,
    kind: &str,
    known: &[&str],
) -> Result<&'a str, ToolkitError> {
    let name = invocation.positional(0).ok_or_else(|| {
        ToolkitError::usage(
            "missing_argument",
            format!("an op is required: {}", known.join(", ")),
        )
    })?;
    if known.contains(&name) {
        Ok(name)
    } else {
        Err(ToolkitError::usage(
            "unknown_op",
            format!(
                "argument {kind}: invalid choice: '{}' (choose from {})",
                safe_text(&crate::safe_text::shown_value(name)),
                known.join(", ")
            ),
        ))
    }
}

pub(super) fn now() -> UtcTime {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    UtcTime::from_epoch_seconds(seconds).unwrap_or_else(|| {
        UtcTime::from_epoch_micros(0).expect("the Unix epoch is inside the supported range")
    })
}

fn json_or_empty(value: Option<&Json>) -> &Json {
    value.unwrap_or(&Json::Null)
}

fn limits_from(args: &Args) -> Result<CaptureLimits, ToolkitError> {
    let Some(value) = args.present("limits") else {
        return Ok(CaptureLimits::unset());
    };
    // recorded-shape: the caller's `limits` argument; not a response.
    let Json::Object(_) = value else {
        return Err(args.mistyped("limits", "an object", value));
    };
    let get = |key: &str| value.get(key).cloned().unwrap_or(Json::Null);
    Ok(CaptureLimits {
        max_hits: get("MaxHits"),
        max_string_length: get("MaxStringLength"),
        max_collection_width: get("MaxCollectionWidth"),
        max_collection_depth: get("MaxCollectionDepth"),
        max_stack_frames: get("MaxStackFrames"),
        max_stack_trace_size: get("MaxStackTraceSize"),
        max_object_depth: get("MaxObjectDepth"),
        max_fields_per_object: get("MaxFieldsPerObject"),
    })
}

fn status_check_from(
    op: &str,
    status: &str,
    checks: Option<&Json>,
) -> Result<StatusCheckResult, ToolkitError> {
    let Some(check) = checks.and_then(|checks| checks.get(status)) else {
        return Err(invalid(
            op,
            format!("the recorded checks have no '{status}' entry that this assessment needs"),
        ));
    };
    let args = Args::new(op, check, CHECK_RESULT_ARGUMENTS)?;
    let events = args.opt_array("events")?.to_vec();
    let has_events = args.opt_bool("has_events")?.unwrap_or(!events.is_empty());
    Ok(StatusCheckResult {
        has_events,
        events,
        error: args.opt_str("error")?.map(str::to_string),
    })
}

fn failure_from(args: &Args) -> Result<AwsFailure, ToolkitError> {
    let value = args.required("failure")?;
    let inner = Args::new(&args.op, value, FAILURE_ARGUMENTS)?;
    let display = inner.opt_str("display")?.unwrap_or("").to_string();
    Ok(match inner.req_str("kind")? {
        "client_error" => AwsFailure::Client {
            code: inner.opt_str("code")?.map(str::to_string),
            message: inner.opt_str("message")?.map(str::to_string),
            display,
        },
        "endpoint_connection" => AwsFailure::EndpointConnection(display),
        "timeout" => AwsFailure::Timeout(display),
        "credentials" => AwsFailure::Credentials {
            type_name: inner
                .opt_str("type_name")?
                .unwrap_or("NoCredentialsError")
                .to_string(),
            message: display,
        },
        "other" => AwsFailure::Other(display),
        unknown => {
            return Err(invalid(
                &args.op,
                format!(
                    "argument 'failure.kind' must be one of client_error, endpoint_connection, \
                     timeout, credentials, other (got '{unknown}')"
                ),
            ))
        }
    })
}

fn render_text(op: &str, value: &Json) -> Result<String, ToolkitError> {
    let args = Args::new(op, value, spec_of(RENDERER_ARGUMENTS, op)?)?;
    match op {
        "location" => {
            let location = location_from_response(json_or_empty(args.present("location")));
            Ok(render_location_block(
                &location,
                args.opt_str("location_hash")?,
            ))
        }
        "create-success" => render_create_success(&args),
        "list" => Ok(render_list_instrumentations_output(
            json_or_empty(args.present("data")),
            args.req_str("normalized_type")?,
            args.req_str("service")?,
            args.req_str("environment")?,
        )),
        "get" => Ok(render_get_instrumentation_output(
            json_or_empty(args.present("config")),
            args.req_str("service")?,
            args.req_str("environment")?,
        )),
        "batch-delete" => {
            let mode = args.req_str("mode")?;
            let instrumentation_type = args.req_str("instrumentation_type")?;
            let service = args.opt_str("service")?;
            let environment = args.opt_str("environment")?;
            Ok(format_batch_delete_response(
                mode,
                json_or_empty(args.present("data")),
                instrumentation_type,
                service,
                environment,
            ))
        }
        "status-history" => Ok(render_get_instrumentation_configuration_status_output(
            json_or_empty(args.present("data")),
            args.req_str("normalized_type")?,
            args.req_str("service")?,
            args.req_str("environment")?,
            args.req_str("requested_status")?,
        )),
        "snapshot-search" => render_snapshot_search(&args),
        "snapshot-sample" => render_snapshot_sample(&args),
        "snapshot-fields" => Ok(dumps_indent2(&parse_snapshot_fields(json_or_empty(
            args.present("result"),
        )))),
        "failure" => render_failure_text(&args),
        other => Err(invalid(other, "unknown renderer")),
    }
}

fn render_create_success(args: &Args) -> Result<String, ToolkitError> {
    let location = location_from_response(json_or_empty(args.present("location")));
    let capture_arguments = args.opt_strings("capture_arguments")?;
    let capture_locals = args.opt_strings("capture_locals")?;
    let limits = limits_from(args)?;
    let summary = CreateSummary {
        normalized_type: args.req_str("normalized_type")?,
        service: args.req_str("service")?,
        environment: args.req_str("environment")?,
        location: &location,
        ttl_hours: args.opt_i64("ttl_hours")?,
        capture_arguments: capture_arguments.as_deref(),
        capture_locals: capture_locals.as_deref(),
        is_line_level: args.opt_bool("is_line_level")?.unwrap_or(false),
        capture_return: args.opt_bool("capture_return")?,
        capture_stack_trace: args.opt_bool("capture_stack_trace")?,
        limits: &limits,
        attribute_filters: args.opt_array("attribute_filters")?,
        now: args.opt_time("now")?.unwrap_or_else(now),
    };
    Ok(render_create_success_message(
        json_or_empty(args.present("response")),
        &summary,
    ))
}

fn render_assessment(value: &Json) -> Result<OpResult, ToolkitError> {
    let op = "status-assessment";
    let args = Args::new(op, value, spec_of(RENDERER_ARGUMENTS, op)?)?;
    let created_at = args.req_time("created_at")?;
    let requested_start = args.req_time("requested_start")?;
    let query_end = args.req_time("query_end")?;
    let checks = args.present("checks");
    if query_end <= requested_start {
        return Ok(OpResult::failure(
            "ERROR: end_time must be later than start_time",
        ));
    }

    let mut missing: Option<ToolkitError> = None;
    let (verdict, window) = assess(
        created_at,
        requested_start,
        query_end,
        &mut |status, _, _| match status_check_from(op, status, checks) {
            Ok(result) => result,
            Err(error) => {
                missing.get_or_insert(error);
                StatusCheckResult::default()
            }
        },
    );
    if let Some(error) = missing {
        return Err(error);
    }
    let subject = StatusSubject {
        location_hash: args.req_str("location_hash")?,
        service: args.req_str("service")?,
        environment: args.req_str("environment")?,
        normalized_type: args.req_str("normalized_type")?,
    };
    Ok(OpResult::success(render_status_assessment(
        &verdict, &subject, &window,
    )))
}

fn render_snapshot_search(args: &Args) -> Result<String, ToolkitError> {
    let query_result = QueryResult::from_json(json_or_empty(args.present("query_result")))
        .map_err(|message| invalid(&args.op, message))?;
    let custom_filters = args.opt_strings("custom_filters")?.unwrap_or_default();
    let context = SearchContext {
        service_name: args.req_str("service_name")?,
        environment: args.req_str("environment")?,
        location_hash: args.req_str("location_hash")?,
        custom_filters: &custom_filters,
        start_time_utc: args.req_str("start_time_utc")?,
        end_time_utc: args.req_str("end_time_utc")?,
        start_epoch: args.req_i64("start_epoch")?,
        end_epoch: args.req_i64("end_epoch")?,
        query_string: args.req_str("query_string")?,
    };
    Ok(render_search_snapshots_for_status_event_output(
        &context,
        &query_result,
    ))
}

fn render_snapshot_sample(args: &Args) -> Result<String, ToolkitError> {
    let query_result = QueryResult::from_json(json_or_empty(args.present("query_result")))
        .map_err(|message| invalid(&args.op, message))?;
    let context = SampleContext {
        service_name: args.req_str("service_name")?,
        environment: args.req_str("environment")?,
        location_hash: args.req_str("location_hash")?,
        start_time_utc: args.req_str("start_time_utc")?,
        end_time_utc: args.req_str("end_time_utc")?,
        max_timeout: args.opt_i64("max_timeout")?.unwrap_or(30),
        query_string: args.req_str("query_string")?,
        include_raw: args.opt_bool("include_raw")?.unwrap_or(false),
    };
    Ok(render_get_sample_snapshot_for_breakpoint_output(
        &context,
        &query_result,
    ))
}

fn render_failure_text(args: &Args) -> Result<String, ToolkitError> {
    let failure = failure_from(args)?;
    let attempted: Vec<(String, Json)> = match args.present("attempted") {
        None => Vec::new(),
        // recorded-shape: caller arguments, typed by the #4380 origin table; not a response.
        Some(Json::Object(pairs)) => pairs.clone(),
        Some(other) => return Err(args.mistyped("attempted", "an object", other)),
    };
    let attempted_refs: Vec<(&str, Json)> = attempted
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();
    // `Prose` may span lines when the library builds it; every field here
    // came from caller JSON, which `Args` already made single-line (#4653).
    let causes = args.opt_strings("possible_causes")?.unwrap_or_default();
    let cause_refs: Vec<&str> = causes.iter().map(String::as_str).collect();
    let steps = args.opt_strings("troubleshooting")?.unwrap_or_default();
    let step_refs: Vec<&str> = steps.iter().map(String::as_str).collect();
    let prose = Prose {
        attempted_label: args.opt_str("attempted_label")?,
        possible_causes: &cause_refs,
        troubleshooting: &step_refs,
        trailer: args.opt_str("trailer")?,
    };
    Ok(render_failure(
        &failure,
        args.req_str("action")?,
        &attempted_refs,
        &prose,
    ))
}

/// The result `cloudwatch render RENDERER` prints for one arguments object:
/// the renderer's text after the final whole-output `safe_block` pass.
pub(super) fn render_report(op: &str, arguments: &Json) -> Result<OpResult, ToolkitError> {
    let mut result = if op == "status-assessment" {
        render_assessment(arguments)?
    } else {
        OpResult::success(render_text(op, arguments)?)
    };
    result.text = safe_block(&result.text);
    Ok(result)
}

/// `toolkit cloudwatch render RENDERER (--json TEXT | --json-file FILE)`.
pub fn render_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let op = op_name(invocation, "renderer", RENDERERS)?;
    let arguments = read_arguments(invocation)?;
    let result = render_report(op, &arguments)?;
    if !result.ok {
        out.set_exit_status(result.exit_status());
    }
    out.line(&result.text)
}

fn validation_outcome(
    out: &mut Output<'_>,
    failure: Option<String>,
    success: &str,
) -> Result<(), ToolkitError> {
    match failure {
        Some(text) => {
            out.set_exit_status(1);
            out.line(&text)
        }
        None => out.line(success),
    }
}

fn run_check(op: &str, value: &Json, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let args = Args::new(op, value, spec_of(CHECK_ARGUMENTS, op)?)?;
    match op {
        "instrumentation-type" => {
            let (normalized, error) =
                normalize_instrumentation_type(args.opt_str("instrumentation_type")?);
            validation_outcome(out, error, &normalized)
        }
        "location-inputs" => {
            let inputs = LocationInputs {
                location_hash: args.opt_str("location_hash")?,
                language: args.opt_str("language")?,
                file_path: args.opt_str("file_path")?,
                code_unit: args.opt_str("code_unit")?,
                class_name: args.opt_str("class_name")?,
                method_name: args.opt_str("method_name")?,
                line_number: args.opt_i64("line_number")?,
            };
            match args.opt_str("mode")?.unwrap_or("create") {
                "create" => match parse_create_inputs(&inputs) {
                    Ok(location) => out.line(&dumps_indent2(
                        &location.to_api_payload().unwrap_or(Json::Null),
                    )),
                    Err(text) => validation_outcome(out, Some(text), ""),
                },
                "lookup" => {
                    let allow = args.opt_bool("allow_code_location_lookup")?.unwrap_or(true);
                    match parse_lookup_inputs(&inputs, allow) {
                        Ok(location) => out.line(&dumps_indent2(&location.to_identifier())),
                        Err(text) => validation_outcome(out, Some(format!("ERROR: {text}")), ""),
                    }
                }
                other => Err(invalid(
                    op,
                    format!("argument 'mode' must be create or lookup (got '{other}')"),
                )),
            }
        }
        "capture-names" => {
            let names = args.opt_strings("names")?;
            let error = validate_capture_names(args.req_str("field_name")?, names.as_deref());
            validation_outcome(out, error, "OK")
        }
        "probe" => {
            let (normalized, error) =
                normalize_instrumentation_type(args.opt_str("instrumentation_type")?);
            if error.is_some() {
                return validation_outcome(out, error, "");
            }
            let error = validate_probe_constraints(
                &normalized,
                args.opt_str("language")?,
                args.opt_i64("line_number")?,
            );
            validation_outcome(out, error, "OK")
        }
        "location-hash" => {
            let error = (!is_valid_location_hash(args.opt_str("location_hash")?))
                .then(|| "ERROR: location_hash must be a 16-character hex string".to_string());
            validation_outcome(out, error, "OK")
        }
        "snapshot-signal" => validation_outcome(
            out,
            validate_snapshot_signal(args.opt_str("signal_type")?),
            "OK",
        ),
        other => Err(invalid(other, "unknown check")),
    }
}

/// `toolkit cloudwatch validate CHECK (--json TEXT | --json-file FILE)`.
pub fn validate_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let op = op_name(invocation, "check", CHECKS)?;
    let arguments = read_arguments(invocation)?;
    run_check(op, &arguments, out)
}

/// `toolkit cloudwatch region [--region R]`.
pub fn region_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    // Caller text on stdout: redacted where it is built (#4941).
    out.line(&safe_text(&redacted_value(&resolve_region_from_env(
        invocation.value("--region"),
    ))))
}
