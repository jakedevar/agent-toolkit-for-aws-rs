//! Operation entrypoints for create/list/get/delete instrumentation
//! (`di_crud_tools.py`).
//!
//! Every function takes the escaped [`Args`] of the op, validates exactly as
//! the Python did (the validators and parsers are the #4380 ones), builds the
//! API request with the Python member order, sends it through the
//! [`Gateway`] and renders the response. Failures that the Python returned
//! as `OpResult(False, text)` are `Ok(OpResult::failure(text))`; only an
//! unusable argument object is an `Err` (exit status 2).
//!
//! `create`, `delete` and the two batch deletes change live resources; the
//! command layer refuses them without `--i-have-confirmed` before this module
//! is reached (see `ops_cli`).

use toolkit_core::{Json, ToolkitError};

use super::capture::{CaptureLimits, CodeCapture};
use super::cli::Args;
use super::consts::SNAPSHOT_SIGNAL_TYPE;
use super::crud_rendering::{
    format_batch_delete_response, render_create_success_message, render_get_instrumentation_output,
    render_list_instrumentations_output, CreateSummary,
};
use super::errors::Prose;
use super::gateway::{render_error, AppSignalsOp, Gateway};
use super::location::{
    is_missing_identifier_error, parse_create_inputs, parse_lookup_inputs, LocationInputs,
    ResolvedLocation,
};
use super::pytext::{get_truthy, strip};
use super::result::OpResult;
use super::timefmt::UtcTime;
use super::validation::{
    format_code_location_troubleshooting, normalize_instrumentation_type, validate_capture_names,
    validate_probe_constraints,
};

const MICROS_PER_HOUR: i64 = 3_600_000_000;

/// The `Must provide one of` help of `delete` and `get`.
fn location_identifier_help(action: &str) -> String {
    format!(
        "ERROR: Must provide one of:\n\
         - location_hash\n\
         - language + file_path (for code locations)\n\
         \n\
         Usage:\n\
         1. {action} by hash:\n\
         \x20  {action}_instrumentation(location_hash=\"abc123...\")\n\
         \n\
         2. {action} by code location:\n\
         \x20  {action}_instrumentation(language=\"Python\", file_path=\"/app/file.py\", ...)"
    )
}

fn str_json(text: &str) -> Json {
    Json::string(text)
}

/// The lookup inputs shared by `get`, `delete` and `get-status`.
pub(super) fn lookup_inputs(args: &Args) -> Result<LocationInputs<'_>, ToolkitError> {
    Ok(LocationInputs {
        location_hash: args.opt_str("location_hash")?,
        language: args.opt_str("language")?,
        file_path: args.opt_str("file_path")?,
        code_unit: args.opt_str("code_unit")?,
        class_name: args.opt_str("class_name")?,
        method_name: args.opt_str("method_name")?,
        line_number: args.opt_i64("line_number")?,
    })
}

/// Resolves a lookup target, or the failure text to return.
pub(super) fn resolve_lookup(
    inputs: &LocationInputs<'_>,
    identifier_help: impl FnOnce() -> String,
) -> Result<ResolvedLocation, OpResult> {
    parse_lookup_inputs(inputs, true).map_err(|error| {
        if is_missing_identifier_error(&error) {
            OpResult::failure(identifier_help())
        } else {
            OpResult::failure(format!("ERROR: {error}"))
        }
    })
}

/// `create_instrumentation`.
pub(super) fn create_instrumentation(
    gateway: &mut Gateway<'_>,
    args: &Args,
    now: UtcTime,
) -> Result<OpResult, ToolkitError> {
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let language = args.opt_str("language")?;
    let file_path = args.opt_str("file_path")?;
    let code_unit = args.opt_str("code_unit")?;
    let class_name = args.opt_str("class_name")?;
    let method_name = args.opt_str("method_name")?;
    let line_number = args.opt_i64("line_number")?;
    let capture_arguments = args.opt_strings("capture_arguments")?;
    let capture_return = args.opt_bool("capture_return")?;
    let capture_stack_trace = args.opt_bool("capture_stack_trace")?;
    let capture_locals = args.opt_strings("capture_locals")?;
    let limits = CaptureLimits::from_values([
        args.opt_i64("max_hits")?,
        args.opt_i64("max_string_length")?,
        args.opt_i64("max_collection_width")?,
        args.opt_i64("max_collection_depth")?,
        args.opt_i64("max_stack_frames")?,
        args.opt_i64("max_stack_trace_size")?,
        args.opt_i64("max_object_depth")?,
        args.opt_i64("max_fields_per_object")?,
    ]);
    let attribute_filters = args.opt_array("attribute_filters")?;
    let description = args
        .opt_str("description")?
        .unwrap_or("dynamic instrumentation");
    let ttl_hours = args.opt_i64("ttl_hours")?;

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }
    if let Some(error) = validate_probe_constraints(&normalized_type, language, line_number) {
        return Ok(OpResult::failure(error));
    }

    let location = match parse_create_inputs(&LocationInputs {
        location_hash: None,
        language,
        file_path,
        code_unit,
        class_name,
        method_name,
        line_number,
    }) {
        Ok(location) => location,
        Err(error) => return Ok(OpResult::failure(error)),
    };
    let location_troubleshooting = format_code_location_troubleshooting(
        language,
        file_path,
        code_unit,
        class_name,
        method_name,
        line_number,
    );

    if let Some(error) = validate_capture_names("capture_arguments", capture_arguments.as_deref()) {
        return Ok(OpResult::failure(error));
    }
    if let Some(error) = validate_capture_names("capture_locals", capture_locals.as_deref()) {
        return Ok(OpResult::failure(error));
    }

    // Line-level instrumentation fires mid-function, where only locals carry
    // data; without capture_locals it would capture nothing useful.
    let is_line_level = line_number.is_some();
    if is_line_level && capture_locals.is_none() {
        return Ok(OpResult::failure(
            "ERROR: line-level instrumentation (line_number set) requires capture_locals.\n\
             At a specific line, only local variables carry data \u{2014} arguments and return \
             values apply to method/function-level targets (no line_number).\n\
             Provide capture_locals=[...] with the local variable names to capture.",
        ));
    }

    let code_capture_return = capture_return.unwrap_or(!is_line_level);
    let code_capture_stack_trace = capture_stack_trace.unwrap_or(true);
    let capture = CodeCapture {
        capture_return: code_capture_return,
        capture_stack_trace: code_capture_stack_trace,
        capture_arguments: capture_arguments.clone(),
        capture_locals: capture_locals.clone(),
        limits: limits.clone(),
    };

    let target_desc = location.describe();
    let Some(location_payload) = location.to_api_payload() else {
        return Ok(OpResult::failure(
            "ERROR: Internal error resolving location. Please report this issue.",
        ));
    };

    let mut request: Vec<(String, Json)> = vec![
        (
            "InstrumentationType".to_string(),
            str_json(&normalized_type),
        ),
        ("Service".to_string(), str_json(service)),
        ("Environment".to_string(), str_json(environment)),
        ("SignalType".to_string(), str_json(SNAPSHOT_SIGNAL_TYPE)),
        ("Location".to_string(), location_payload),
        ("CaptureConfiguration".to_string(), capture.to_api_payload()),
        ("Description".to_string(), str_json(description)),
    ];
    if let Some(hours) = ttl_hours {
        let expires_at = hours
            .checked_mul(MICROS_PER_HOUR)
            .and_then(|delta| now.checked_add_micros(delta));
        let Some(expires_at) = expires_at else {
            return Ok(OpResult::failure(
                "ERROR: ttl_hours puts the expiry outside the supported date range",
            ));
        };
        request.push(("ExpiresAt".to_string(), str_json(&expires_at.isoformat_z())));
    }
    if !attribute_filters.is_empty() {
        request.push((
            "AttributeFilters".to_string(),
            Json::Array(attribute_filters.to_vec()),
        ));
    }

    let response = match gateway.call(AppSignalsOp::Create, &Json::Object(request)) {
        Ok(response) => response,
        Err(error) => {
            let action = format!("create {normalized_type} instrumentation");
            let attempted = [
                ("Type", str_json(&normalized_type)),
                ("Target", str_json(&target_desc)),
                ("Service", str_json(service)),
                ("Environment", str_json(environment)),
            ];
            let prose = Prose {
                attempted_label: Some("ATTEMPTED CONFIGURATION:"),
                possible_causes: &[
                    "AWS credentials missing or scoped to a different account",
                    "Invalid service or environment identifier",
                    "Instrumentation already exists at this location",
                    "Invalid location/capture payload",
                    "AWS API endpoint not accessible",
                ],
                troubleshooting: &[
                    "Verify AWS credentials: aws configure list",
                    "Check service name and environment match your deployment",
                    "Try listing existing instrumentations with list_instrumentations",
                ],
                trailer: Some(&location_troubleshooting),
            };
            return Ok(OpResult::failure(render_error(
                &error, &action, &attempted, &prose,
            )));
        }
    };

    let location = location.into_location();
    let summary = CreateSummary {
        normalized_type: &normalized_type,
        service,
        environment,
        location: &location,
        ttl_hours,
        capture_arguments: capture_arguments.as_deref(),
        capture_locals: capture_locals.as_deref(),
        is_line_level,
        capture_return: Some(code_capture_return),
        capture_stack_trace: Some(code_capture_stack_trace),
        limits: &limits,
        attribute_filters,
        now,
    };
    Ok(OpResult::success(render_create_success_message(
        &response, &summary,
    )))
}

/// `list_instrumentations`.
pub(super) fn list_instrumentations(
    gateway: &mut Gateway<'_>,
    args: &Args,
) -> Result<OpResult, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let synced_at = args.opt_str("synced_at")?;
    let max_results = args.opt_i64("max_results")?.unwrap_or(100);
    let next_token = args.opt_str("next_token")?;

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }

    let mut request: Vec<(String, Json)> = vec![
        ("Service".to_string(), str_json(service)),
        ("Environment".to_string(), str_json(environment)),
        (
            "InstrumentationType".to_string(),
            str_json(&normalized_type),
        ),
    ];
    if let Some(synced_at) = synced_at.filter(|text| !text.is_empty()) {
        request.push(("SyncedAt".to_string(), str_json(synced_at)));
    }
    if max_results != 100 {
        request.push(("MaxResults".to_string(), Json::Int(max_results)));
    }
    if let Some(token) = next_token.filter(|text| !text.is_empty()) {
        request.push(("NextToken".to_string(), str_json(token)));
    }

    match gateway.call(AppSignalsOp::List, &Json::Object(request)) {
        Ok(data) => Ok(OpResult::success(render_list_instrumentations_output(
            &data,
            &normalized_type,
            service,
            environment,
        ))),
        Err(error) => {
            let attempted = [
                ("Service", str_json(service)),
                ("Environment", str_json(environment)),
                ("InstrumentationType", str_json(&normalized_type)),
            ];
            Ok(OpResult::failure(render_error(
                &error,
                "list instrumentations",
                &attempted,
                &Prose::default(),
            )))
        }
    }
}

/// `batch_delete_instrumentations_by_scope`.
pub(super) fn batch_delete_by_scope(
    gateway: &mut Gateway<'_>,
    args: &Args,
) -> Result<OpResult, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let instrumentation_type = args.req_str("instrumentation_type")?;

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }

    let request = Json::object(vec![(
        "DeletionTarget",
        Json::object(vec![(
            "Scope",
            Json::object(vec![
                ("Service", str_json(service)),
                ("Environment", str_json(environment)),
                ("InstrumentationType", str_json(&normalized_type)),
            ]),
        )]),
    )]);
    match gateway.call(AppSignalsOp::BatchDelete, &request) {
        // `ok` reflects whether the backend reported per-item errors (read
        // from the response, not the rendered text).
        Ok(data) => Ok(OpResult {
            ok: !get_truthy(&data, "Errors"),
            text: format_batch_delete_response(
                "Scope",
                &data,
                &normalized_type,
                Some(service),
                Some(environment),
            ),
        }),
        Err(error) => {
            let attempted = [
                ("Service", str_json(service)),
                ("Environment", str_json(environment)),
                ("InstrumentationType", str_json(&normalized_type)),
            ];
            Ok(OpResult::failure(render_error(
                &error,
                "batch delete instrumentation configurations (scope mode)",
                &attempted,
                &Prose::default(),
            )))
        }
    }
}

/// `batch_delete_instrumentations_by_arns`.
pub(super) fn batch_delete_by_arns(
    gateway: &mut Gateway<'_>,
    args: &Args,
) -> Result<OpResult, ToolkitError> {
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let resource_arns = args.required("resource_arns")?;
    // recorded-shape: the caller's `resource_arns` argument; not a response.
    let Json::Array(arns) = resource_arns else {
        return Err(args.mistyped("resource_arns", "an array", resource_arns));
    };

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }
    if arns.is_empty() {
        return Ok(OpResult::failure(
            "ERROR: resource_arns must contain at least one ARN.",
        ));
    }
    if arns.len() > 50 {
        return Ok(OpResult::failure(
            "ERROR: resource_arns can include at most 50 ARNs per request.",
        ));
    }
    let all_valid = arns
        .iter()
        .all(|arn| matches!(arn, Json::String(text) if !strip(text).is_empty()));
    if !all_valid {
        return Ok(OpResult::failure(
            "ERROR: resource_arns must contain non-empty ARN strings only.",
        ));
    }

    let request = Json::object(vec![(
        "DeletionTarget",
        Json::object(vec![(
            "ResourceArns",
            Json::object(vec![
                ("ResourceArns", Json::Array(arns.clone())),
                ("InstrumentationType", str_json(&normalized_type)),
            ]),
        )]),
    )]);
    match gateway.call(AppSignalsOp::BatchDelete, &request) {
        Ok(data) => Ok(OpResult {
            ok: !get_truthy(&data, "Errors"),
            text: format_batch_delete_response("ResourceArns", &data, &normalized_type, None, None),
        }),
        Err(error) => {
            let count = i64::try_from(arns.len()).unwrap_or(i64::MAX);
            let attempted = [
                ("InstrumentationType", str_json(&normalized_type)),
                ("ResourceArnCount", Json::Int(count)),
            ];
            Ok(OpResult::failure(render_error(
                &error,
                "batch delete instrumentation configurations (resource ARN mode)",
                &attempted,
                &Prose::default(),
            )))
        }
    }
}

/// `delete_instrumentation`.
pub(super) fn delete_instrumentation(
    gateway: &mut Gateway<'_>,
    args: &Args,
) -> Result<OpResult, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let inputs = lookup_inputs(args)?;

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }
    let location = match resolve_lookup(&inputs, || location_identifier_help("delete")) {
        Ok(location) => location,
        Err(failure) => return Ok(failure),
    };
    let target_desc = location.describe();

    let request = Json::object(vec![
        ("InstrumentationType", str_json(&normalized_type)),
        ("Service", str_json(service)),
        ("Environment", str_json(environment)),
        ("SignalType", str_json(SNAPSHOT_SIGNAL_TYPE)),
        ("LocationIdentifier", location.to_identifier()),
    ]);
    match gateway.call(AppSignalsOp::Delete, &request) {
        Ok(_) => Ok(OpResult::success(format!(
            "Successfully deleted {normalized_type} instrumentation\n\
             \n\
             Target: {target_desc}\n\
             Service: {service}\n\
             Environment: {environment}\n\
             \n\
             TIP: Use list_instrumentations to verify removal."
        ))),
        Err(error) => {
            let action = format!("delete {normalized_type} instrumentation");
            let attempted = [
                ("Target", str_json(&target_desc)),
                ("Service", str_json(service)),
                ("Environment", str_json(environment)),
            ];
            let prose = Prose {
                attempted_label: Some("ATTEMPTED TO DELETE:"),
                possible_causes: &[
                    "Instrumentation doesn't exist at this location",
                    "Location parameters don't match exactly",
                    "Wrong service or environment identifier",
                    "Already deleted",
                ],
                troubleshooting: &["Use list_instrumentations to see exact configuration details"],
                trailer: None,
            };
            Ok(OpResult::failure(render_error(
                &error, &action, &attempted, &prose,
            )))
        }
    }
}

/// `get_instrumentation`.
pub(super) fn get_instrumentation(
    gateway: &mut Gateway<'_>,
    args: &Args,
) -> Result<OpResult, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let inputs = lookup_inputs(args)?;

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }
    let location = match resolve_lookup(&inputs, || location_identifier_help("get")) {
        Ok(location) => location,
        Err(failure) => return Ok(failure),
    };
    let target_desc = location.describe();

    let request = Json::object(vec![
        ("InstrumentationType", str_json(&normalized_type)),
        ("Service", str_json(service)),
        ("Environment", str_json(environment)),
        ("SignalType", str_json(SNAPSHOT_SIGNAL_TYPE)),
        ("LocationIdentifier", location.to_identifier()),
    ]);
    let data = match gateway.call(AppSignalsOp::Get, &request) {
        Ok(data) => data,
        Err(error) => {
            let action = "get instrumentation";
            let attempted = [
                ("Target", str_json(&target_desc)),
                ("Service", str_json(service)),
                ("Environment", str_json(environment)),
            ];
            let prose = Prose {
                attempted_label: Some("ATTEMPTED TO RETRIEVE:"),
                possible_causes: &[
                    "Instrumentation doesn't exist at this location",
                    "Location parameters don't match exactly",
                    "Wrong service or environment identifier",
                ],
                troubleshooting: &["Use list_instrumentations to see all active instrumentations"],
                trailer: None,
            };
            return Ok(OpResult::failure(render_error(
                &error, action, &attempted, &prose,
            )));
        }
    };

    if !get_truthy(&data, "Configuration") {
        return Ok(OpResult::failure(format!(
            "No instrumentation found for {target_desc}"
        )));
    }
    let config = data.get("Configuration").unwrap_or(&Json::Null);
    Ok(OpResult::success(render_get_instrumentation_output(
        config,
        service,
        environment,
    )))
}
