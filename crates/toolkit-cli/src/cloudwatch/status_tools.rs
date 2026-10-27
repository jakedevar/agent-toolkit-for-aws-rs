//! Operation entrypoints for status queries and reporting
//! (`di_status_tools.py`).

use toolkit_core::{Json, ToolkitError};

use super::aws_client::failure_text;
use super::cli::Args;
use super::consts::SNAPSHOT_SIGNAL_TYPE;
use super::crud_tools::{lookup_inputs, resolve_lookup};
use super::errors::Prose;
use super::gateway::{render_error, AppSignalsOp, Gateway};
use super::pytext::{safe_text, strip};
use super::result::OpResult;
use super::status_assessment::{assess, StatusCheckResult};
use super::status_rendering::{
    render_get_instrumentation_configuration_status_output, render_status_assessment, StatusSubject,
};
use super::timefmt::{parse_iso_timestamp, time_from_json, UtcTime};
use super::validation::{
    is_valid_location_hash, normalize_instrumentation_type, validate_snapshot_signal,
};
use super::aws_client::optional_list;

const STATUS_IDENTIFIER_HELP: &str = "ERROR: Must provide one of:\n\
- location_hash\n\
- language + file_path (for code location identifier)\n\
\n\
Usage:\n\
1. Get by hash (preferred):\n\
\x20  get_instrumentation_configuration_status(location_hash=\"abc123...\")\n\
\n\
2. Get by code location:\n\
\x20  get_instrumentation_configuration_status(language=\"Python\", file_path=\"/app/file.py\", ...)";

const STATUS_REQUIRED: &str = "ERROR: status is required\n\
\n\
This API cannot return all statuses in one call.\n\
If status is omitted, AWS defaults to ACTIVE, which is ambiguous.\n\
\n\
Use explicit status checks in this order:\n\
1. status=\"READY\"\n\
2. status=\"ACTIVE\" (only after READY is confirmed by events)\n\
3. status=\"ERROR\" (if READY not confirmed)\n\
4. status=\"DISABLED\" (when checking max-hits scenarios)";

fn str_json(text: &str) -> Json {
    Json::string(text)
}

fn invalid_time(field: &str, message: &str) -> OpResult {
    OpResult::failure(format!(
        "ERROR: Invalid {field} format. Expected ISO 8601. Error: {}",
        safe_text(message)
    ))
}

/// `_check_status_with_time_range`: whether status events exist for the
/// configuration in a time range.
fn check_status_with_time_range(
    gateway: &mut Gateway<'_>,
    request: Vec<(String, Json)>,
) -> StatusCheckResult {
    let response = match gateway.call(AppSignalsOp::GetStatus, &Json::Object(request)) {
        Ok(response) => response,
        Err(error) => {
            return StatusCheckResult {
                has_events: false,
                events: Vec::new(),
                error: Some(format!("API error: {}", failure_text(&error.failure))),
            };
        }
    };
    match optional_list(&response, "Events") {
        Ok(events) => StatusCheckResult {
            has_events: !events.is_empty(),
            events,
            error: None,
        },
        Err(error) => StatusCheckResult {
            has_events: false,
            events: Vec::new(),
            error: Some(format!("API error: {error}")),
        },
    }
}

/// `get_instrumentation_configuration_status`.
pub(super) fn get_status(gateway: &mut Gateway<'_>, args: &Args) -> Result<OpResult, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let inputs = lookup_inputs(args)?;
    let status = args.opt_str("status")?;
    let start_time = args.opt_str("start_time")?;
    let end_time = args.opt_str("end_time")?;
    let max_results = args.opt_i64("max_results")?.unwrap_or(100);
    let next_token = args.opt_str("next_token")?;
    let signal_type = args.opt_str("signal_type")?.unwrap_or(SNAPSHOT_SIGNAL_TYPE);

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }
    if let Some(error) = validate_snapshot_signal(Some(signal_type)) {
        return Ok(OpResult::failure(error));
    }
    let location = match resolve_lookup(&inputs, || STATUS_IDENTIFIER_HELP.to_string()) {
        Ok(location) => location,
        Err(failure) => return Ok(failure),
    };
    let target_desc = location.describe();

    let requested_status = strip(status.unwrap_or("")).to_uppercase();
    if requested_status.is_empty() {
        return Ok(OpResult::failure(STATUS_REQUIRED));
    }
    if !matches!(
        requested_status.as_str(),
        "READY" | "ACTIVE" | "ERROR" | "DISABLED"
    ) {
        return Ok(OpResult::failure(format!(
            "ERROR: invalid status. Must be one of: READY, ACTIVE, ERROR, DISABLED \
             (received: {})",
            status.unwrap_or("")
        )));
    }

    let mut request: Vec<(String, Json)> = vec![
        (
            "InstrumentationType".to_string(),
            str_json(&normalized_type),
        ),
        ("Service".to_string(), str_json(service)),
        ("Environment".to_string(), str_json(environment)),
        ("SignalType".to_string(), str_json(SNAPSHOT_SIGNAL_TYPE)),
        ("Status".to_string(), str_json(&requested_status)),
        ("LocationIdentifier".to_string(), location.to_identifier()),
    ];
    if let Some(start_time) = start_time.filter(|text| !text.is_empty()) {
        match parse_iso_timestamp(start_time) {
            Ok(parsed) => request.push(("StartTime".to_string(), str_json(&parsed.isoformat_z()))),
            Err(message) => return Ok(invalid_time("start_time", &message)),
        }
    }
    if let Some(end_time) = end_time.filter(|text| !text.is_empty()) {
        match parse_iso_timestamp(end_time) {
            Ok(parsed) => request.push(("EndTime".to_string(), str_json(&parsed.isoformat_z()))),
            Err(message) => return Ok(invalid_time("end_time", &message)),
        }
    }
    if max_results != 100 {
        request.push(("MaxResults".to_string(), Json::Int(max_results)));
    }
    if let Some(token) = next_token.filter(|text| !text.is_empty()) {
        request.push(("NextToken".to_string(), str_json(token)));
    }

    match gateway.call(AppSignalsOp::GetStatus, &Json::Object(request)) {
        Ok(data) => Ok(OpResult::success(
            render_get_instrumentation_configuration_status_output(
                &data,
                &normalized_type,
                service,
                environment,
                &requested_status,
            ),
        )),
        Err(error) => {
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
                troubleshooting: &["Use get_instrumentation to verify the configuration exists"],
                trailer: None,
            };
            Ok(OpResult::failure(render_error(
                &error,
                "get instrumentation status",
                &attempted,
                &prose,
            )))
        }
    }
}

/// The fetched creation time of a configuration, or the failure to return.
fn fetch_created_at(
    gateway: &mut Gateway<'_>,
    normalized_type: &str,
    service: &str,
    environment: &str,
    location_hash: &str,
) -> Result<UtcTime, OpResult> {
    let request = Json::object(vec![
        ("InstrumentationType", str_json(normalized_type)),
        ("Service", str_json(service)),
        ("Environment", str_json(environment)),
        ("SignalType", str_json(SNAPSHOT_SIGNAL_TYPE)),
        (
            "LocationIdentifier",
            Json::object(vec![("LocationHash", str_json(location_hash))]),
        ),
    ]);
    let response = gateway.call(AppSignalsOp::Get, &request).map_err(|error| {
        OpResult::failure(format!(
            "ERROR: Failed to fetch created_at: Exception: {}",
            safe_text(failure_text(&error.failure))
        ))
    })?;
    let config = match response.get("Configuration") {
        Some(config @ Json::Object(members)) if !members.is_empty() => config,
        Some(Json::Object(_) | Json::Null) | None => {
            return Err(OpResult::failure(format!(
                "ERROR: Failed to fetch created_at: No instrumentation found for LocationHash \
                 {location_hash}"
            )));
        }
        Some(other) => {
            return Err(OpResult::failure(format!(
                "ERROR: Failed to fetch created_at: the Configuration of the response is not an \
                 object (it is {})",
                other.type_name()
            )));
        }
    };
    match config.get("CreatedAt") {
        None | Some(Json::Null) => Err(OpResult::failure(
            "ERROR: Failed to fetch created_at: CreatedAt not found in instrumentation \
             configuration",
        )),
        Some(created) => time_from_json(created).map_err(|message| {
            OpResult::failure(format!(
                "ERROR: Failed to fetch created_at: CreatedAt is not a timestamp: {}",
                safe_text(&message)
            ))
        }),
    }
}

/// `check_instrumentation_status`.
pub(super) fn check_status(
    gateway: &mut Gateway<'_>,
    args: &Args,
) -> Result<OpResult, ToolkitError> {
    let service = args.req_str("service")?;
    let environment = args.req_str("environment")?;
    let instrumentation_type = args.req_str("instrumentation_type")?;
    let location_hash = args.req_str("location_hash")?;
    let start_time = args.req_str("start_time")?;
    let end_time = args.req_str("end_time")?;
    let signal_type = args.opt_str("signal_type")?.unwrap_or(SNAPSHOT_SIGNAL_TYPE);

    let (normalized_type, type_error) = normalize_instrumentation_type(Some(instrumentation_type));
    if let Some(error) = type_error {
        return Ok(OpResult::failure(error));
    }
    if let Some(error) = validate_snapshot_signal(Some(signal_type)) {
        return Ok(OpResult::failure(error));
    }
    if !is_valid_location_hash(Some(location_hash)) {
        return Ok(OpResult::failure(
            "ERROR: location_hash must be a 16-character hex string",
        ));
    }

    let created_at = match fetch_created_at(
        gateway,
        &normalized_type,
        service,
        environment,
        location_hash,
    ) {
        Ok(created_at) => created_at,
        Err(failure) => return Ok(failure),
    };
    let start = match parse_iso_timestamp(start_time) {
        Ok(parsed) => parsed,
        Err(message) => return Ok(invalid_time("start_time", &message)),
    };
    let query_end = match parse_iso_timestamp(end_time) {
        Ok(parsed) => parsed,
        Err(message) => return Ok(invalid_time("end_time", &message)),
    };
    if query_end <= start {
        return Ok(OpResult::failure(
            "ERROR: end_time must be later than start_time",
        ));
    }

    let (verdict, window) = assess(
        created_at,
        start,
        query_end,
        &mut |status, status_start, status_end| {
            let request = vec![
                (
                    "InstrumentationType".to_string(),
                    str_json(&normalized_type),
                ),
                ("Service".to_string(), str_json(service)),
                ("Environment".to_string(), str_json(environment)),
                ("SignalType".to_string(), str_json(SNAPSHOT_SIGNAL_TYPE)),
                ("Status".to_string(), str_json(status)),
                (
                    "LocationIdentifier".to_string(),
                    Json::object(vec![("LocationHash", str_json(location_hash))]),
                ),
                (
                    "StartTime".to_string(),
                    str_json(&status_start.isoformat_z()),
                ),
                ("EndTime".to_string(), str_json(&status_end.isoformat_z())),
            ];
            check_status_with_time_range(gateway, request)
        },
    );
    let subject = StatusSubject {
        location_hash,
        service,
        environment,
        normalized_type: &normalized_type,
    };
    Ok(OpResult::success(render_status_assessment(
        &verdict, &subject, &window,
    )))
}
