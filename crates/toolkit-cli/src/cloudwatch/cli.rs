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

fn invalid(op: &str, detail: impl AsRef<str>) -> ToolkitError {
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

/// A typed view of the JSON arguments object of one op.
struct Args<'a> {
    op: String,
    value: &'a Json,
}

impl<'a> Args<'a> {
    fn new(op: &str, value: &'a Json, allowed: &[&str]) -> Result<Self, ToolkitError> {
        let Json::Object(pairs) = value else {
            return Err(invalid(op, "arguments must be a JSON object"));
        };
        if let Some((key, _)) = pairs
            .iter()
            .find(|(key, _)| !allowed.contains(&key.as_str()))
        {
            return Err(invalid(op, format!("got an unexpected argument '{key}'")));
        }
        Ok(Self {
            op: op.to_string(),
            value,
        })
    }

    fn present(&self, key: &str) -> Option<&'a Json> {
        self.value.get(key).filter(|value| **value != Json::Null)
    }

    fn mistyped(&self, key: &str, expected: &str, got: &Json) -> ToolkitError {
        invalid(
            &self.op,
            format!(
                "argument '{key}' must be {expected}, got {}",
                got.type_name()
            ),
        )
    }

    fn required(&self, key: &str) -> Result<&'a Json, ToolkitError> {
        self.present(key)
            .ok_or_else(|| invalid(&self.op, format!("missing required argument '{key}'")))
    }

    fn opt_str(&self, key: &str) -> Result<Option<&'a str>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::String(text)) => Ok(Some(text.as_str())),
            Some(other) => Err(self.mistyped(key, "a string", other)),
        }
    }

    fn req_str(&self, key: &str) -> Result<&'a str, ToolkitError> {
        match self.required(key)? {
            Json::String(text) => Ok(text.as_str()),
            other => Err(self.mistyped(key, "a string", other)),
        }
    }

    fn opt_i64(&self, key: &str) -> Result<Option<i64>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Int(number)) => Ok(Some(*number)),
            Some(other) => Err(self.mistyped(key, "an integer", other)),
        }
    }

    fn req_i64(&self, key: &str) -> Result<i64, ToolkitError> {
        self.opt_i64(key)?
            .ok_or_else(|| invalid(&self.op, format!("missing required argument '{key}'")))
    }

    fn opt_bool(&self, key: &str) -> Result<Option<bool>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Bool(flag)) => Ok(Some(*flag)),
            Some(other) => Err(self.mistyped(key, "a boolean", other)),
        }
    }

    fn opt_strings(&self, key: &str) -> Result<Option<Vec<String>>, ToolkitError> {
        match self.present(key) {
            None => Ok(None),
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

    fn opt_array(&self, key: &str) -> Result<&'a [Json], ToolkitError> {
        match self.present(key) {
            None => Ok(&[]),
            Some(Json::Array(items)) => Ok(items),
            Some(other) => Err(self.mistyped(key, "an array", other)),
        }
    }

    fn opt_time(&self, key: &str) -> Result<Option<UtcTime>, ToolkitError> {
        self.opt_str(key)?
            .map(|text| {
                parse_iso_timestamp(text)
                    .map_err(|message| invalid(&self.op, format!("argument '{key}': {message}")))
            })
            .transpose()
    }

    fn req_time(&self, key: &str) -> Result<UtcTime, ToolkitError> {
        self.opt_time(key)?
            .ok_or_else(|| invalid(&self.op, format!("missing required argument '{key}'")))
    }
}

fn read_arguments(invocation: &Invocation) -> Result<Json, ToolkitError> {
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
    if !matches!(payload, Json::Object(_)) {
        return Err(ToolkitError::usage(
            "arguments_not_object",
            "arguments must be a JSON object of the op's parameters",
        ));
    }
    Ok(payload)
}

fn op_name<'a>(
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
                safe_text(name),
                known.join(", ")
            ),
        ))
    }
}

fn now() -> UtcTime {
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

fn limits_from(args: &Args<'_>) -> Result<CaptureLimits, ToolkitError> {
    let Some(value) = args.present("limits") else {
        return Ok(CaptureLimits::unset());
    };
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
    let args = Args::new(op, check, &["has_events", "events", "error"])?;
    let events = args.opt_array("events")?.to_vec();
    let has_events = args.opt_bool("has_events")?.unwrap_or(!events.is_empty());
    Ok(StatusCheckResult {
        has_events,
        events,
        error: args.opt_str("error")?.map(str::to_string),
    })
}

fn failure_from(args: &Args<'_>) -> Result<AwsFailure, ToolkitError> {
    let value = args.required("failure")?;
    let inner = Args::new(
        &args.op,
        value,
        &["kind", "code", "message", "display", "type_name"],
    )?;
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
    match op {
        "location" => {
            let args = Args::new(op, value, &["location", "location_hash"])?;
            let location = location_from_response(json_or_empty(args.present("location")));
            Ok(render_location_block(
                &location,
                args.opt_str("location_hash")?,
            ))
        }
        "create-success" => render_create_success(value),
        "list" => {
            let args = Args::new(
                op,
                value,
                &["data", "normalized_type", "service", "environment"],
            )?;
            Ok(render_list_instrumentations_output(
                json_or_empty(args.present("data")),
                args.req_str("normalized_type")?,
                args.req_str("service")?,
                args.req_str("environment")?,
            ))
        }
        "get" => {
            let args = Args::new(op, value, &["config", "service", "environment"])?;
            Ok(render_get_instrumentation_output(
                json_or_empty(args.present("config")),
                args.req_str("service")?,
                args.req_str("environment")?,
            ))
        }
        "batch-delete" => {
            let args = Args::new(
                op,
                value,
                &[
                    "mode",
                    "data",
                    "instrumentation_type",
                    "service",
                    "environment",
                ],
            )?;
            Ok(format_batch_delete_response(
                args.req_str("mode")?,
                json_or_empty(args.present("data")),
                args.req_str("instrumentation_type")?,
                args.opt_str("service")?,
                args.opt_str("environment")?,
            ))
        }
        "status-history" => {
            let args = Args::new(
                op,
                value,
                &[
                    "data",
                    "normalized_type",
                    "service",
                    "environment",
                    "requested_status",
                ],
            )?;
            Ok(render_get_instrumentation_configuration_status_output(
                json_or_empty(args.present("data")),
                args.req_str("normalized_type")?,
                args.req_str("service")?,
                args.req_str("environment")?,
                args.req_str("requested_status")?,
            ))
        }
        "snapshot-search" => render_snapshot_search(value),
        "snapshot-sample" => render_snapshot_sample(value),
        "snapshot-fields" => {
            let args = Args::new(op, value, &["result"])?;
            Ok(dumps_indent2(&parse_snapshot_fields(json_or_empty(
                args.present("result"),
            ))))
        }
        "failure" => render_failure_text(value),
        other => Err(invalid(other, "unknown renderer")),
    }
}

fn render_create_success(value: &Json) -> Result<String, ToolkitError> {
    let op = "create-success";
    let args = Args::new(
        op,
        value,
        &[
            "response",
            "normalized_type",
            "service",
            "environment",
            "location",
            "ttl_hours",
            "capture_arguments",
            "capture_locals",
            "is_line_level",
            "capture_return",
            "capture_stack_trace",
            "limits",
            "attribute_filters",
            "now",
        ],
    )?;
    let location = location_from_response(json_or_empty(args.present("location")));
    let capture_arguments = args.opt_strings("capture_arguments")?;
    let capture_locals = args.opt_strings("capture_locals")?;
    let limits = limits_from(&args)?;
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
    let args = Args::new(
        op,
        value,
        &[
            "created_at",
            "requested_start",
            "query_end",
            "checks",
            "location_hash",
            "service",
            "environment",
            "normalized_type",
        ],
    )?;
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

fn render_snapshot_search(value: &Json) -> Result<String, ToolkitError> {
    let op = "snapshot-search";
    let args = Args::new(
        op,
        value,
        &[
            "query_result",
            "service_name",
            "environment",
            "location_hash",
            "custom_filters",
            "start_time_utc",
            "end_time_utc",
            "start_epoch",
            "end_epoch",
            "query_string",
        ],
    )?;
    let query_result = QueryResult::from_json(json_or_empty(args.present("query_result")))
        .map_err(|message| invalid(op, message))?;
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

fn render_snapshot_sample(value: &Json) -> Result<String, ToolkitError> {
    let op = "snapshot-sample";
    let args = Args::new(
        op,
        value,
        &[
            "query_result",
            "service_name",
            "environment",
            "location_hash",
            "start_time_utc",
            "end_time_utc",
            "max_timeout",
            "query_string",
            "include_raw",
        ],
    )?;
    let query_result = QueryResult::from_json(json_or_empty(args.present("query_result")))
        .map_err(|message| invalid(op, message))?;
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

fn render_failure_text(value: &Json) -> Result<String, ToolkitError> {
    let op = "failure";
    let args = Args::new(
        op,
        value,
        &[
            "failure",
            "action",
            "attempted",
            "attempted_label",
            "possible_causes",
            "troubleshooting",
            "trailer",
        ],
    )?;
    let failure = failure_from(&args)?;
    let attempted: Vec<(String, Json)> = match args.present("attempted") {
        None => Vec::new(),
        Some(Json::Object(pairs)) => pairs.clone(),
        Some(other) => return Err(args.mistyped("attempted", "an object", other)),
    };
    let attempted_refs: Vec<(&str, Json)> = attempted
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();
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

/// `toolkit cloudwatch render RENDERER (--json TEXT | --json-file FILE)`.
pub fn render_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let op = op_name(invocation, "renderer", RENDERERS)?;
    let arguments = read_arguments(invocation)?;
    let result = if op == "status-assessment" {
        render_assessment(&arguments)?
    } else {
        OpResult::success(render_text(op, &arguments)?)
    };
    if !result.ok {
        out.set_exit_status(result.exit_status());
    }
    out.line(&safe_block(&result.text))
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
    match op {
        "instrumentation-type" => {
            let args = Args::new(op, value, &["instrumentation_type"])?;
            let (normalized, error) =
                normalize_instrumentation_type(args.opt_str("instrumentation_type")?);
            validation_outcome(out, error, &normalized)
        }
        "location-inputs" => {
            let args = Args::new(
                op,
                value,
                &[
                    "mode",
                    "location_hash",
                    "language",
                    "file_path",
                    "code_unit",
                    "class_name",
                    "method_name",
                    "line_number",
                    "allow_code_location_lookup",
                ],
            )?;
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
            let args = Args::new(op, value, &["field_name", "names"])?;
            let names = args.opt_strings("names")?;
            let error = validate_capture_names(args.req_str("field_name")?, names.as_deref());
            validation_outcome(out, error, "OK")
        }
        "probe" => {
            let args = Args::new(
                op,
                value,
                &["instrumentation_type", "language", "line_number"],
            )?;
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
            let args = Args::new(op, value, &["location_hash"])?;
            let error = (!is_valid_location_hash(args.opt_str("location_hash")?))
                .then(|| "ERROR: location_hash must be a 16-character hex string".to_string());
            validation_outcome(out, error, "OK")
        }
        "snapshot-signal" => {
            let args = Args::new(op, value, &["signal_type"])?;
            validation_outcome(
                out,
                validate_snapshot_signal(args.opt_str("signal_type")?),
                "OK",
            )
        }
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
    out.line(&safe_text(&resolve_region_from_env(
        invocation.value("--region"),
    )))
}
